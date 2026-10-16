//! Port of `upstream/packages/provider-opencode/src/server/adapter.ts`.
//!
//! One directory-scoped v1 client per session. Prompts carry per-turn
//! instructions in `system`. `/name` that matches the command list is
//! `session.command`; a bare `/compact` is `session.summarize`. Permission
//! replies use `once` / `always` / `reject`. A clean event-stream EOF breaks
//! the session. Transport errors resubscribe like the 1.x SDK's SSE client:
//! `Last-Event-ID`, the server's `retry:`, and an uncapped attempt count with
//! the 3s→30s backoff. Message history is not backfilled.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use turin_contracts::{
    NodeId, PlanId, PositiveInt, ProviderSessionId, RuntimeRequestId, ThreadId,
    TrimmedNonEmptyString, UtcDateTime,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2AppThread, OrchestrationV2ConversationMessage,
        OrchestrationV2ConversationMessageRole, OrchestrationV2CreationSource,
        OrchestrationV2ExecutionNode, OrchestrationV2ExecutionNodeKind,
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2ProviderFailureClass,
        OrchestrationV2ProviderSession, OrchestrationV2ProviderSessionStatus,
        OrchestrationV2ProviderThread, OrchestrationV2ProviderThreadDisposition,
        OrchestrationV2ProviderThreadStatus, OrchestrationV2ProviderTurn,
        OrchestrationV2ProviderTurnStatus, OrchestrationV2ResponseCapability,
        OrchestrationV2RuntimeRequest, OrchestrationV2RuntimeRequestKind,
        OrchestrationV2RuntimeRequestStatus, OrchestrationV2TurnItem,
        OrchestrationV2TurnItemStatus,
    },
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
    provider_policy::ProviderRequestKind,
};
use turin_provider_core::id_allocator::{IdAllocator, derive};
use turin_provider_core::subagent_projection::{
    SubagentChildThreadInput, SubagentConversationArtifactsInput, make_subagent_child_thread,
    make_subagent_conversation_artifacts, subagent_thread_title,
};

use crate::adapter::api::*;
use crate::adapter::logic::*;
use crate::adapter::records::{
    Compaction, NodeOf, NodeTiming, RequestBody, SubagentCall, TextBlock, assistant_message,
    compaction_turn_item, epoch_ms_or, execution_node, next_ordinal, open_code_question_id,
    request_node, request_turn_item, root_turn_node, settle_request_item, settle_request_node,
    subagent_records, text_turn_item, todo_native_id, todo_plan, todo_steps, todos_complete,
    user_input_questions,
};
use crate::events::V1Event;
use crate::v1_client::{PermissionReply, QuestionReply, V1Client};

const RECONNECT_BASE: Duration = Duration::from_secs(3);
const RECONNECT_CAP: Duration = Duration::from_secs(30);

/// `createSseClient`'s backoff: the delay doubles per attempt since the
/// stream was first opened, capped at 30s. The attempt count never resets.
fn reconnect_backoff(retry: Duration, attempt: u32) -> Duration {
    let doubled = retry.saturating_mul(1u32 << attempt.saturating_sub(1).min(16));
    doubled.min(RECONNECT_CAP)
}

struct V1Turn {
    provider_turn: OrchestrationV2ProviderTurn,
    thread_id: ThreadId,
    run_ordinal: i64,
    admission: AdmissionFlags,
    interrupted: bool,
    finalized: bool,
    usage: UsageAccum,
    /// The app thread the turn runs for: a child thread's parent.
    app_thread: OrchestrationV2AppThread,
    /// Ordinals are per native item, assigned on first sight, as upstream's `itemOrdinal`.
    item_ordinals: HashMap<String, u64>,
    next_item_ordinal: u64,
    plan_id: Option<PlanId>,
    parts: HashMap<String, PartRecord>,
}

/// A text or reasoning part, kept as upstream's `ActiveOpenCodeTurn.parts` keeps it, so a
/// delta or the completion of its message can re-project it.
#[derive(Clone)]
struct PartRecord {
    message_id: String,
    reasoning: bool,
    text: String,
    ignored: bool,
    start: Option<i64>,
    end: Option<i64>,
}

struct V1Thread {
    provider_thread: OrchestrationV2ProviderThread,
    active: Option<V1Turn>,
    turns: Vec<OrchestrationV2ProviderTurn>,
    requests: Vec<OrchestrationV2RuntimeRequest>,
    /// `messageRoles`: the role each message reported, so a user's echoed text is not an answer.
    message_roles: HashMap<String, String>,
}

struct Pending {
    request_id: RuntimeRequestId,
    native_id: String,
    session_id: String,
    kind: &'static str,
    question: Option<Value>,
    /// The node and item shown while the request waits, settled when it ends.
    node: OrchestrationV2ExecutionNode,
    item: OrchestrationV2TurnItem,
}

struct V1Inner {
    threads: HashMap<String, V1Thread>,
    pending: HashMap<String, Pending>,
    busy: HashSet<String>,
    children: HashMap<String, String>,
    /// Child sessions a `task` call revealed, by child session id.
    subagents: HashMap<String, V1Child>,
    stream_dead: Option<String>,
}

/// A child session a `task` call started (upstream's child `OpenCodeThreadState`): its thread,
/// its provider thread, and the turn its execution runs.
struct V1Child {
    session_id: String,
    thread: OrchestrationV2AppThread,
    provider_thread: OrchestrationV2ProviderThread,
    /// Message roles in the child session, so a user's echoed prompt is not a result.
    roles: HashMap<String, String>,
    turn: Option<V1Turn>,
    next_turn_ordinal: u64,
}

pub struct OpenCodeV1Session {
    instance_id: ProviderInstanceId,
    driver: ProviderDriverKind,
    provider_session_id: ProviderSessionId,
    /// Open-time session record. Later status changes are
    /// `provider_session.updated` events; this field stays put so
    /// `provider_session()` can return a reference.
    session: OrchestrationV2ProviderSession,
    client: V1Client,
    inner: Mutex<V1Inner>,
    tx: mpsc::UnboundedSender<Result<ProviderAdapterV2Event, ProviderAdapterV2Error>>,
    rx: Mutex<
        Option<mpsc::UnboundedReceiver<Result<ProviderAdapterV2Event, ProviderAdapterV2Error>>>,
    >,
    reader: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl OpenCodeV1Session {
    pub async fn open(
        instance_id: ProviderInstanceId,
        provider_session_id: ProviderSessionId,
        cwd: &str,
        base_url: &str,
        password: Option<&str>,
        model: Option<&str>,
    ) -> Result<Arc<Self>, ProviderAdapterV2Error> {
        Self::open_logged(
            instance_id,
            provider_session_id,
            cwd,
            base_url,
            password,
            model,
            None,
        )
        .await
    }

    /// `open` with the adapter's NDJSON protocol sink attached to every session call.
    pub async fn open_logged(
        instance_id: ProviderInstanceId,
        provider_session_id: ProviderSessionId,
        cwd: &str,
        base_url: &str,
        password: Option<&str>,
        model: Option<&str>,
        protocol: Option<Arc<dyn crate::protocol_log::ProtocolLog>>,
    ) -> Result<Arc<Self>, ProviderAdapterV2Error> {
        let mut client = V1Client::new(base_url, cwd, password).map_err(|err| {
            ProviderAdapterV2Error::protocol(format!("OpenCode v1 client: {}", err.detail))
        })?;
        if let Some(protocol) = protocol {
            client = client.with_protocol_log(protocol);
        }
        let now = UtcDateTime::now();
        let cwd = turin_contracts::TrimmedNonEmptyString::new(cwd)
            .map_err(|_| ProviderAdapterV2Error::protocol("OpenCode session cwd is blank"))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let provider_session = OrchestrationV2ProviderSession {
            id: provider_session_id.clone(),
            driver: opencode_driver(),
            provider_instance_id: instance_id.clone(),
            status: OrchestrationV2ProviderSessionStatus::Ready,
            cwd,
            model: model.and_then(|model| turin_contracts::TrimmedNonEmptyString::new(model).ok()),
            capabilities: v1_capabilities(),
            created_at: now,
            updated_at: now,
            last_error: None,
        };
        let session = Arc::new(Self {
            instance_id,
            driver: opencode_driver(),
            provider_session_id,
            session: provider_session,
            client,
            inner: Mutex::new(V1Inner {
                threads: HashMap::new(),
                subagents: HashMap::new(),
                pending: HashMap::new(),
                busy: HashSet::new(),
                children: HashMap::new(),
                stream_dead: None,
            }),
            tx,
            rx: Mutex::new(Some(rx)),
            reader: Mutex::new(None),
        });
        let this = Arc::clone(&session);
        *session.reader.lock() = Some(tokio::spawn(async move {
            this.follow_events().await;
        }));
        Ok(session)
    }

    pub async fn close(&self) {
        if let Some(handle) = self.reader.lock().take() {
            handle.abort();
        }
    }

    async fn follow_events(self: Arc<Self>) {
        let mut last_event_id: Option<String> = None;
        let mut retry = RECONNECT_BASE;
        let mut attempt = 0u32;
        loop {
            attempt = attempt.saturating_add(1);
            if let Ok(mut stream) = self.client.events_from(last_event_id.as_deref()).await {
                loop {
                    match stream.next().await {
                        Some(Ok(frame)) => {
                            if let Some(id) = frame.id {
                                last_event_id = Some(id);
                            }
                            if let Some(ms) = frame.retry {
                                retry = Duration::from_millis(ms);
                            }
                            if let Some(event) = frame.event {
                                self.on_event(event).await;
                            }
                        }
                        Some(Err(_)) => break,
                        None => {
                            self.fail_stream("OpenCode event stream ended unexpectedly.")
                                .await;
                            return;
                        }
                    }
                }
            }
            tokio::time::sleep(reconnect_backoff(retry, attempt)).await;
        }
    }

    async fn fail_stream(&self, message: &str) {
        let mut guard = self.inner.lock();
        guard.stream_dead = Some(message.to_owned());
        let mut session = self.session.clone();
        session.status = OrchestrationV2ProviderSessionStatus::Error;
        session.last_error = Some(message.to_owned());
        session.updated_at = UtcDateTime::now();
        let mut terminals = Vec::new();
        for thread in guard.threads.values_mut() {
            if let Some(turn) = thread.active.as_mut()
                && !turn.finalized
            {
                turn.finalized = true;
                turn.provider_turn.status = OrchestrationV2ProviderTurnStatus::Failed;
                turn.provider_turn.completed_at = Some(UtcDateTime::now());
                terminals.push((
                    thread.provider_thread.id.clone(),
                    turn.provider_turn.clone(),
                    turn.run_ordinal,
                ));
                thread.active = None;
                thread.provider_thread.status = OrchestrationV2ProviderThreadStatus::Error;
            }
        }
        drop(guard);
        self.emit(ProviderAdapterV2Event::ProviderSessionUpdated {
            driver: self.driver.clone(),
            provider_session: session,
        });
        for (provider_thread_id, turn, run_ordinal) in terminals {
            self.emit(ProviderAdapterV2Event::TurnTerminal {
                driver: self.driver.clone(),
                provider_thread_id,
                provider_turn_id: turn.id.clone(),
                run_ordinal: PositiveInt::new(run_ordinal.max(1) as u64).expect("run ordinal"),
                status: ProviderAdapterV2TurnTerminalStatus::Failed,
                failure: Some(provider_failure(
                    OrchestrationV2ProviderFailureClass::TransportError,
                    message,
                )),
                failure_item_ordinal: None,
                retry: None,
                retry_started_at: None,
                thread_disposition: OrchestrationV2ProviderThreadDisposition::Broken,
            });
        }
    }

    fn emit(&self, event: ProviderAdapterV2Event) {
        let _ = self.tx.send(Ok(event));
    }

    async fn on_event(&self, event: V1Event) {
        let mut guard = self.inner.lock();
        let props = event.properties;
        match event.event_type.as_str() {
            "server.connected" => {
                for thread in guard.threads.values_mut() {
                    if let Some(turn) = thread.active.as_mut() {
                        turn.usage.complete = false;
                    }
                }
            }
            "session.created" | "session.updated" => {
                let info = props.get("info").cloned().unwrap_or(Value::Null);
                let id = info.get("id").and_then(Value::as_str).map(str::to_owned);
                let parent = info
                    .get("parentID")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                // Ownership only: the subagent record comes from the task that names the child
                // (upstream `relatedSessionOwners`), so the link and the task key one record.
                if let (Some(id), Some(parent)) = (id, parent)
                    && guard.threads.contains_key(&parent)
                {
                    guard.children.insert(id, parent);
                }
            }
            "session.status" | "session.idle" => {
                let session_id = str_field(&props, "sessionID");
                let status_type = props
                    .get("status")
                    .and_then(|status| status.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or(if event.event_type == "session.idle" {
                        "idle"
                    } else {
                        ""
                    });
                if let Some(session_id) = session_id {
                    if status_type == "idle"
                        && let Some(child) = guard.subagents.get_mut(&session_id)
                    {
                        self.finish_child_turn(child);
                    }
                    if status_type == "idle" {
                        guard.busy.remove(&session_id);
                    } else if status_type == "busy" || status_type == "retry" {
                        guard.busy.insert(session_id.clone());
                    }
                    if status_type == "idle" {
                        self.settle_idle(&mut guard, &session_id);
                    } else if status_type == "busy"
                        && let Some(thread) = guard.threads.get_mut(&session_id)
                        && let Some(turn) = thread.active.as_mut()
                    {
                        let _ = advance_open_code_prompt_admission(
                            &mut turn.admission,
                            OpenCodeAdmissionSignal::Busy,
                        );
                    }
                }
            }
            "session.error" => {
                let session_id = str_field(&props, "sessionID");
                let message = props
                    .pointer("/error/data/message")
                    .or_else(|| props.pointer("/error/message"))
                    .and_then(Value::as_str)
                    .unwrap_or("OpenCode session error")
                    .to_owned();
                let aborted = message.to_ascii_lowercase().contains("abort");
                if let Some(session_id) = session_id.clone() {
                    self.finish_active(
                        &mut guard,
                        &session_id,
                        if aborted {
                            ProviderAdapterV2TurnTerminalStatus::Interrupted
                        } else {
                            ProviderAdapterV2TurnTerminalStatus::Failed
                        },
                        Some(message),
                        OrchestrationV2ProviderThreadDisposition::Reusable,
                    );
                }
            }
            "message.updated" => {
                let info = props.get("info").cloned().unwrap_or(Value::Null);
                let session_id = str_field(&info, "sessionID");
                let role = info.get("role").and_then(Value::as_str);
                let message_id = str_field(&info, "id");
                if let (Some(session), Some(message)) = (session_id.as_ref(), message_id.as_ref())
                    && let Some(child) = guard.subagents.get_mut(session)
                {
                    self.on_child_message(child, &info, message);
                }
                if let (Some(session), Some(message), Some(role)) =
                    (session_id.as_ref(), message_id.as_ref(), role)
                    && let Some(thread) = guard.threads.get_mut(session)
                {
                    thread
                        .message_roles
                        .insert(message.clone(), role.to_owned());
                }
                if let (Some(session_id), Some("user"), Some(_)) =
                    (session_id, role, message_id.as_ref())
                    && let Some(thread) = guard.threads.get_mut(&session_id)
                    && let Some(turn) = thread.active.as_mut()
                {
                    let _ = advance_open_code_prompt_admission(
                        &mut turn.admission,
                        OpenCodeAdmissionSignal::UserMessage,
                    );
                }
                if role == Some("assistant") {
                    let completed = info
                        .get("time")
                        .and_then(|time| time.get("completed"))
                        .is_some();
                    if completed
                        && let Some(session_id) = str_field(&info, "sessionID")
                        && let Some(thread) = guard.threads.get_mut(&session_id)
                        && let Some(turn) = thread.active.as_mut()
                    {
                        let _ = advance_open_code_prompt_admission(
                            &mut turn.admission,
                            OpenCodeAdmissionSignal::AssistantCompleted,
                        );
                        // The message is done, so its text and reasoning parts settle as completed.
                        if let Some(message) = message_id.as_deref() {
                            let owned: Vec<String> = turn
                                .parts
                                .iter()
                                .filter(|(_, record)| record.message_id == message)
                                .map(|(id, _)| id.clone())
                                .collect();
                            for part_id in owned {
                                self.emit_text_part(turn, &part_id, true);
                            }
                        }
                    }
                }
            }
            "message.part.updated" => {
                self.on_part(&mut guard, &props);
            }
            "message.part.delta" => {
                self.on_part_delta(&mut guard, &props);
            }
            "todo.updated" => {
                self.on_todo(&mut guard, &props);
            }
            "session.compacted" => {
                if let Some(session_id) = str_field(&props, "sessionID") {
                    self.on_compacted(&mut guard, &session_id);
                }
            }
            "permission.asked" | "question.asked" => {
                self.on_request(&mut guard, &event.event_type, &props);
            }
            "permission.replied" | "question.replied" | "question.rejected" => {
                let native = str_field(&props, "requestID");
                if let Some(native) = native {
                    let status = if event.event_type == "question.rejected" {
                        OrchestrationV2RuntimeRequestStatus::Cancelled
                    } else {
                        OrchestrationV2RuntimeRequestStatus::Resolved
                    };
                    if let Some(pending) = guard.pending.remove(&native) {
                        self.resolve_request(&mut guard, pending, status);
                    }
                }
            }
            _ => {}
        }
    }

    fn settle_idle(&self, guard: &mut V1Inner, session_id: &str) {
        let Some(thread) = guard.threads.get_mut(session_id) else {
            return;
        };
        let Some(turn) = thread.active.as_mut() else {
            return;
        };
        let action =
            advance_open_code_prompt_admission(&mut turn.admission, OpenCodeAdmissionSignal::Idle);
        if action == OpenCodeAdmissionAction::Hold {
            return;
        }
        let status = if turn.interrupted {
            ProviderAdapterV2TurnTerminalStatus::Interrupted
        } else {
            ProviderAdapterV2TurnTerminalStatus::Completed
        };
        self.finish_active(
            guard,
            session_id,
            status,
            None,
            OrchestrationV2ProviderThreadDisposition::Reusable,
        );
    }

    fn finish_active(
        &self,
        guard: &mut V1Inner,
        session_id: &str,
        status: ProviderAdapterV2TurnTerminalStatus,
        failure: Option<String>,
        disposition: OrchestrationV2ProviderThreadDisposition,
    ) {
        // `finishTurn`: OpenCode drops a request its session asked when the execution
        // ends, so the request cancels with the turn.
        if guard
            .threads
            .get(session_id)
            .is_some_and(|thread| thread.active.is_some())
        {
            let doomed: Vec<String> = guard
                .pending
                .iter()
                .filter(|(_, pending)| {
                    pending.session_id == session_id
                        || guard
                            .children
                            .get(&pending.session_id)
                            .is_some_and(|owner| owner == session_id)
                })
                .map(|(native, _)| native.clone())
                .collect();
            for native in doomed {
                if let Some(pending) = guard.pending.remove(&native) {
                    self.resolve_request(
                        guard,
                        pending,
                        OrchestrationV2RuntimeRequestStatus::Cancelled,
                    );
                }
            }
        }
        let Some(thread) = guard.threads.get_mut(session_id) else {
            return;
        };
        let Some(mut turn) = thread.active.take() else {
            return;
        };
        if turn.finalized {
            thread.active = Some(turn);
            return;
        }
        turn.finalized = true;
        turn.provider_turn.status = match status {
            ProviderAdapterV2TurnTerminalStatus::Completed => {
                OrchestrationV2ProviderTurnStatus::Completed
            }
            ProviderAdapterV2TurnTerminalStatus::Interrupted => {
                OrchestrationV2ProviderTurnStatus::Interrupted
            }
            ProviderAdapterV2TurnTerminalStatus::Cancelled => {
                OrchestrationV2ProviderTurnStatus::Cancelled
            }
            ProviderAdapterV2TurnTerminalStatus::Failed => {
                OrchestrationV2ProviderTurnStatus::Failed
            }
        };
        turn.provider_turn.completed_at = Some(UtcDateTime::now());
        turn.provider_turn.turn_token_usage = Some(
            turn.usage
                .to_turn_usage(status == ProviderAdapterV2TurnTerminalStatus::Completed),
        );
        let provider_turn = turn.provider_turn.clone();
        let run_ordinal = turn.run_ordinal.max(1) as u64;
        let thread_id = turn.thread_id.clone();
        if let Some(stored) = thread
            .turns
            .iter_mut()
            .find(|item| item.id == provider_turn.id)
        {
            *stored = provider_turn.clone();
        }
        thread.provider_thread.status = OrchestrationV2ProviderThreadStatus::Idle;
        thread.provider_thread.updated_at = UtcDateTime::now();
        let provider_thread = thread.provider_thread.clone();
        let driver = self.driver.clone();
        self.emit(ProviderAdapterV2Event::ProviderTurnUpdated {
            driver: driver.clone(),
            thread_id: Some(thread_id),
            provider_turn: provider_turn.clone(),
        });
        self.emit(ProviderAdapterV2Event::ProviderThreadUpdated {
            driver: driver.clone(),
            provider_thread,
            retained_native_turn_ids: None,
        });
        self.emit(ProviderAdapterV2Event::TurnTerminal {
            driver,
            provider_thread_id: provider_turn.provider_thread_id.clone(),
            provider_turn_id: provider_turn.id,
            run_ordinal: PositiveInt::new(run_ordinal).expect("run ordinal"),
            status,
            failure: failure.map(|message| {
                provider_failure(OrchestrationV2ProviderFailureClass::ProviderError, &message)
            }),
            failure_item_ordinal: None,
            retry: None,
            retry_started_at: None,
            thread_disposition: disposition,
        });
    }

    fn on_part(&self, guard: &mut V1Inner, props: &Value) {
        let part = props.get("part").cloned().unwrap_or(Value::Null);
        let Some(session_id) =
            str_field(&part, "sessionID").or_else(|| str_field(props, "sessionID"))
        else {
            return;
        };
        let part_type = part
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let part_id = str_field(&part, "id")
            .or_else(|| str_field(props, "partID"))
            .unwrap_or_else(|| "part".to_owned());
        let message_id = str_field(&part, "messageID").unwrap_or_default();
        if part_type == "step-finish" || part.get("tokens").is_some() {
            if let Some(turn) = guard
                .threads
                .get_mut(&session_id)
                .and_then(|thread| thread.active.as_mut())
                && !turn.finalized
                && let Some(tokens) = parse_step_tokens(&part)
            {
                turn.usage.add_step(&part_id, tokens);
            }
            return;
        }
        // A child session's parts belong to its child turn. Its user text is the prompt, which
        // projects as a conversation message on that turn.
        if let Some(child) = guard.subagents.get_mut(&session_id) {
            let user = child.roles.get(&message_id).map(String::as_str) == Some("user");
            let Some(turn) = child.turn.as_mut() else {
                return;
            };
            if part_type == "text" && user {
                self.project_child_user_part(turn, &part, &message_id);
                return;
            }
            self.dispatch_part(
                turn,
                &mut HashMap::new(),
                None,
                &session_id,
                &part,
                &part_id,
                &message_id,
                &part_type,
            );
            return;
        }
        let Some(thread) = guard.threads.get_mut(&session_id) else {
            return;
        };
        let user = thread.message_roles.get(&message_id).map(String::as_str) == Some("user");
        let Some(turn) = thread.active.as_mut() else {
            return;
        };
        if turn.finalized || (part_type == "text" && user) {
            return;
        }
        self.dispatch_part(
            turn,
            &mut guard.children,
            Some(&mut guard.subagents),
            &session_id,
            &part,
            &part_id,
            &message_id,
            &part_type,
        );
    }

    /// The text, reasoning and tool handling shared by root and child turns. `subagents` is
    /// `None` inside a child turn: its nested tasks are projected without a child thread.
    #[allow(clippy::too_many_arguments)]
    fn dispatch_part(
        &self,
        turn: &mut V1Turn,
        children: &mut HashMap<String, String>,
        subagents: Option<&mut HashMap<String, V1Child>>,
        session_id: &str,
        part: &Value,
        part_id: &str,
        message_id: &str,
        part_type: &str,
    ) {
        if part_type == "text" || part_type == "reasoning" {
            let record = PartRecord {
                message_id: message_id.to_owned(),
                reasoning: part_type == "reasoning",
                text: part
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                ignored: part.get("ignored").and_then(Value::as_bool) == Some(true)
                    || part.get("synthetic").and_then(Value::as_bool) == Some(true),
                start: part.pointer("/time/start").and_then(Value::as_i64),
                end: part.pointer("/time/end").and_then(Value::as_i64),
            };
            turn.parts.insert(part_id.to_owned(), record);
            self.emit_text_part(turn, part_id, false);
        } else if part_type == "tool" {
            self.on_tool_part(turn, children, subagents, session_id, part, part_id);
        }
    }

    /// `message.part.delta`. Only the `text` field is projected, as upstream's
    /// `handlePartDelta` does, and only for a part whose full text was seen first.
    fn on_part_delta(&self, guard: &mut V1Inner, props: &Value) {
        if str_field(props, "field").as_deref() != Some("text") {
            return;
        }
        let Some(session_id) = str_field(props, "sessionID") else {
            return;
        };
        let Some(part_id) = str_field(props, "partID") else {
            return;
        };
        let delta = props.get("delta").and_then(Value::as_str).unwrap_or("");
        let Some(thread) = guard.threads.get_mut(&session_id) else {
            return;
        };
        let Some(turn) = thread.active.as_mut() else {
            return;
        };
        match turn.parts.get_mut(&part_id) {
            Some(record) => record.text.push_str(delta),
            None => return,
        }
        self.emit_text_part(turn, &part_id, false);
    }

    /// `emitTextPart`: the node, then the assistant message for text, then the item.
    fn emit_text_part(&self, turn: &mut V1Turn, part_id: &str, force_completed: bool) {
        let Some(record) = turn.parts.get(part_id).cloned() else {
            return;
        };
        if record.ignored || record.text.is_empty() {
            return;
        }
        let now = UtcDateTime::now();
        let completed = force_completed || record.end.is_some();
        let started_at = epoch_ms_or(record.start, now);
        let completed_at = completed.then(|| epoch_ms_or(record.end, now));
        let thread_id = turn.thread_id.clone();
        let root_node_id = turn.provider_turn.node_id.clone();
        let provider_thread_id = turn.provider_turn.provider_thread_id.clone();
        let provider_turn_id = turn.provider_turn.id.clone();
        let of = NodeOf {
            thread_id: &thread_id,
            root_node_id: &root_node_id,
            provider_thread_id: &provider_thread_id,
            provider_turn_id: &provider_turn_id,
            native_item_id: part_id,
            weak_ref: false,
        };
        let kind = if record.reasoning {
            OrchestrationV2ExecutionNodeKind::Reasoning
        } else {
            OrchestrationV2ExecutionNodeKind::AssistantMessage
        };
        let node = execution_node(
            &self.driver,
            &of,
            NodeTiming {
                kind,
                status: if completed {
                    OrchestrationV2ExecutionNodeStatus::Completed
                } else {
                    OrchestrationV2ExecutionNodeStatus::Running
                },
                started_at,
                completed_at,
            },
        );
        let node_id = node.id.clone();
        self.emit(ProviderAdapterV2Event::NodeUpdated {
            driver: self.driver.clone(),
            node,
        });
        let ordinal = item_ordinal(turn, part_id);
        let status = if completed {
            OrchestrationV2TurnItemStatus::Completed
        } else {
            OrchestrationV2TurnItemStatus::Running
        };
        if !record.reasoning {
            let message = assistant_message(
                &self.driver,
                &of,
                &node_id,
                &record.text,
                !completed,
                started_at,
                now,
            );
            self.emit(ProviderAdapterV2Event::MessageUpdated {
                driver: self.driver.clone(),
                message,
            });
        }
        let item = text_turn_item(
            &self.driver,
            &of,
            &node_id,
            TextBlock {
                reasoning: record.reasoning,
                text: &record.text,
                streaming: !completed,
                status,
                ordinal,
                started_at,
                completed_at,
                updated_at: now,
            },
        );
        self.emit(ProviderAdapterV2Event::TurnItemUpdated {
            driver: self.driver.clone(),
            turn_item: item,
        });
    }

    /// `emitToolPart` for every tool except `question`, whose item is the respondable
    /// runtime request. `task` keeps its existing child-session bookkeeping here.
    fn on_tool_part(
        &self,
        turn: &mut V1Turn,
        children: &mut HashMap<String, String>,
        subagents: Option<&mut HashMap<String, V1Child>>,
        session_id: &str,
        part: &Value,
        part_id: &str,
    ) {
        let name = part
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_owned();
        if name.eq_ignore_ascii_case("question") {
            return;
        }
        let state = part.get("state").cloned().unwrap_or(Value::Null);
        let status_name = state
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("running");
        let status = match status_name {
            "pending" => OrchestrationV2TurnItemStatus::Pending,
            "completed" => OrchestrationV2TurnItemStatus::Completed,
            "error" => OrchestrationV2TurnItemStatus::Failed,
            _ => OrchestrationV2TurnItemStatus::Running,
        };
        let node_status = match status_name {
            "pending" => OrchestrationV2ExecutionNodeStatus::Pending,
            "completed" => OrchestrationV2ExecutionNodeStatus::Completed,
            "error" => OrchestrationV2ExecutionNodeStatus::Failed,
            _ => OrchestrationV2ExecutionNodeStatus::Running,
        };
        let input = state.get("input").cloned().unwrap_or(json!({}));
        let output = state
            .get("output")
            .or_else(|| state.get("error"))
            .and_then(Value::as_str);
        if name == "task" {
            turn.usage.has_subagents = true;
            let child = state
                .pointer("/metadata/sessionId")
                .or_else(|| state.pointer("/metadata/sessionID"))
                .and_then(Value::as_str);
            if let Some(child) = child {
                children.insert(child.to_owned(), session_id.to_owned());
            }
        }
        let now = UtcDateTime::now();
        let started_at = if status_name == "pending" {
            now
        } else {
            epoch_ms_or(state.pointer("/time/start").and_then(Value::as_i64), now)
        };
        let completed_at = matches!(status_name, "completed" | "error")
            .then(|| epoch_ms_or(state.pointer("/time/end").and_then(Value::as_i64), now));
        let thread_id = turn.thread_id.clone();
        let root_node_id = turn.provider_turn.node_id.clone();
        let provider_thread_id = turn.provider_turn.provider_thread_id.clone();
        let provider_turn_id = turn.provider_turn.id.clone();
        let of = NodeOf {
            thread_id: &thread_id,
            root_node_id: &root_node_id,
            provider_thread_id: &provider_thread_id,
            provider_turn_id: &provider_turn_id,
            native_item_id: part_id,
            weak_ref: false,
        };
        if name == "task" {
            // `emitSubagent`: a task call is a subagent, not a tool call. The child thread exists
            // from the first task update that names the child session; the record names it.
            let prompt = input
                .get("prompt")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let title = state
                .get("title")
                .and_then(Value::as_str)
                .or_else(|| input.get("description").and_then(Value::as_str));
            let child_session = state
                .pointer("/metadata/sessionId")
                .or_else(|| state.pointer("/metadata/sessionID"))
                .and_then(Value::as_str);
            let subagent_node = derive::node_from_provider_item(&self.driver, part_id);
            let mut child = None;
            if let (Some(child_session), Some(subagents)) = (child_session, subagents) {
                self.ensure_child(
                    subagents,
                    turn,
                    child_session,
                    &prompt,
                    title,
                    &subagent_node,
                );
                child = subagents
                    .get(child_session)
                    .map(|child| (child.thread.id.clone(), child.provider_thread.id.clone()));
            }
            let ordinal = item_ordinal(turn, part_id);
            let records = subagent_records(
                &self.driver,
                SubagentCall {
                    instance_id: &self.instance_id,
                    child,
                    of: NodeOf {
                        thread_id: &thread_id,
                        root_node_id: &root_node_id,
                        provider_thread_id: &provider_thread_id,
                        provider_turn_id: &provider_turn_id,
                        native_item_id: part_id,
                        weak_ref: false,
                    },
                    ordinal,
                    prompt: &prompt,
                    title,
                    status,
                    started_at,
                    completed_at,
                    result: output.map(str::to_owned),
                    child_session,
                    now,
                },
            );
            self.emit(ProviderAdapterV2Event::NodeUpdated {
                driver: self.driver.clone(),
                node: records.node,
            });
            self.emit(ProviderAdapterV2Event::SubagentUpdated {
                driver: self.driver.clone(),
                subagent: records.subagent,
            });
            self.emit(ProviderAdapterV2Event::TurnItemUpdated {
                driver: self.driver.clone(),
                turn_item: records.item,
            });
            return;
        }
        let node = execution_node(
            &self.driver,
            &of,
            NodeTiming {
                kind: OrchestrationV2ExecutionNodeKind::ToolCall,
                status: node_status,
                started_at,
                completed_at,
            },
        );
        self.emit(ProviderAdapterV2Event::NodeUpdated {
            driver: self.driver.clone(),
            node,
        });
        let base = tool_base(
            turn,
            &self.driver,
            part_id,
            status,
            started_at,
            completed_at,
        );
        let item = open_code_tool_turn_item(base, &name, &input, output, state.get("metadata"));
        self.emit(ProviderAdapterV2Event::TurnItemUpdated {
            driver: self.driver.clone(),
            turn_item: item,
        });
    }

    /// `emitSubagent`'s child thread: created the first time a task reveals its child session.
    /// The child app thread is made from the parent's, and it runs on its own provider thread.
    fn ensure_child(
        &self,
        subagents: &mut HashMap<String, V1Child>,
        parent: &V1Turn,
        child_session: &str,
        prompt: &str,
        title: Option<&str>,
        subagent_node: &NodeId,
    ) {
        if subagents.contains_key(child_session) {
            return;
        }
        let now = UtcDateTime::now();
        let ordinal = subagents.len() as u64 + 1;
        let thread_id = derive::thread_from_provider_thread(
            &self.driver,
            Some(&self.instance_id),
            child_session,
        );
        let mut provider_thread = make_thread(
            &self.instance_id,
            &self.provider_session_id,
            &thread_id,
            child_session,
            None,
            now,
        );
        provider_thread.owner_node_id = Some(subagent_node.clone());
        let child_title =
            subagent_thread_title(parent.app_thread.title.as_str(), title, prompt, ordinal);
        let thread = make_subagent_child_thread(SubagentChildThreadInput {
            parent_thread: &parent.app_thread,
            child_thread_id: thread_id.clone(),
            parent_node_id: subagent_node.clone(),
            active_provider_thread_id: Some(provider_thread.id.clone()),
            provider_instance_id: self.instance_id.clone(),
            model_selection: ModelSelection {
                instance_id: self.instance_id.clone(),
                model: parent.app_thread.model_selection.model.clone(),
                options: None,
            },
            title: TrimmedNonEmptyString::new(child_title)
                .unwrap_or_else(|_| TrimmedNonEmptyString::new("Subagent").expect("literal")),
            now,
            created_by: OrchestrationV2Actor::Agent,
            creation_source: OrchestrationV2CreationSource::Provider,
        });
        self.emit(ProviderAdapterV2Event::AppThreadCreated {
            driver: self.driver.clone(),
            app_thread: thread.clone(),
        });
        self.emit(ProviderAdapterV2Event::ProviderThreadUpdated {
            driver: self.driver.clone(),
            provider_thread: provider_thread.clone(),
            retained_native_turn_ids: None,
        });
        subagents.insert(
            child_session.to_owned(),
            V1Child {
                session_id: child_session.to_owned(),
                thread,
                provider_thread,
                roles: HashMap::new(),
                turn: None,
                next_turn_ordinal: 1,
            },
        );
    }

    /// `createChildTurn`: a child session's user message starts its turn, under a `root_turn`.
    fn start_child_turn(&self, child: &mut V1Child, message_id: &str, now: UtcDateTime) {
        if child.turn.is_some() {
            return;
        }
        let ordinal = child.next_turn_ordinal;
        child.next_turn_ordinal += 1;
        let root = derive::node_from_provider_item(
            &self.driver,
            &format!("{}:root:{message_id}", child.session_id),
        );
        let provider_turn_id = derive::provider_turn(&self.driver, message_id);
        let provider_turn = OrchestrationV2ProviderTurn {
            id: provider_turn_id.clone(),
            provider_thread_id: child.provider_thread.id.clone(),
            node_id: root.clone(),
            run_attempt_id: None,
            native_turn_ref: Some(native_ref(message_id, true)),
            ordinal: PositiveInt::new(ordinal).expect("child turn ordinals start at one"),
            status: OrchestrationV2ProviderTurnStatus::Running,
            started_at: Some(now),
            completed_at: None,
            token_usage: None,
            turn_token_usage: None,
        };
        let node = root_turn_node(
            &child.thread.id,
            &root,
            &child.provider_thread.id,
            &provider_turn_id,
            provider_turn.native_turn_ref.clone(),
            OrchestrationV2ExecutionNodeStatus::Running,
            now,
            None,
        );
        child.turn = Some(V1Turn {
            provider_turn: provider_turn.clone(),
            thread_id: child.thread.id.clone(),
            run_ordinal: 0,
            admission: AdmissionFlags::default(),
            interrupted: false,
            finalized: false,
            usage: UsageAccum::new(),
            app_thread: child.thread.clone(),
            item_ordinals: HashMap::new(),
            next_item_ordinal: 1,
            plan_id: None,
            parts: HashMap::new(),
        });
        self.emit(ProviderAdapterV2Event::NodeUpdated {
            driver: self.driver.clone(),
            node,
        });
        self.emit(ProviderAdapterV2Event::ProviderTurnUpdated {
            driver: self.driver.clone(),
            thread_id: Some(child.thread.id.clone()),
            provider_turn,
        });
    }

    /// The child's turn settles when its session goes idle: its streamed text is final, its
    /// root node completes, and its provider turn completes.
    fn finish_child_turn(&self, child: &mut V1Child) {
        let Some(mut turn) = child.turn.take() else {
            return;
        };
        let open: Vec<String> = turn.parts.keys().cloned().collect();
        for part_id in open {
            self.emit_text_part(&mut turn, &part_id, true);
        }
        let now = UtcDateTime::now();
        turn.provider_turn.status = OrchestrationV2ProviderTurnStatus::Completed;
        turn.provider_turn.completed_at = Some(now);
        let node = root_turn_node(
            &child.thread.id,
            &turn.provider_turn.node_id,
            &child.provider_thread.id,
            &turn.provider_turn.id,
            turn.provider_turn.native_turn_ref.clone(),
            OrchestrationV2ExecutionNodeStatus::Completed,
            turn.provider_turn.started_at.unwrap_or(now),
            Some(now),
        );
        self.emit(ProviderAdapterV2Event::NodeUpdated {
            driver: self.driver.clone(),
            node,
        });
        self.emit(ProviderAdapterV2Event::ProviderTurnUpdated {
            driver: self.driver.clone(),
            thread_id: Some(child.thread.id.clone()),
            provider_turn: turn.provider_turn,
        });
    }

    /// A message in a child session: its role is kept, a user's message starts the turn, and
    /// a finished assistant message settles the text it carried.
    fn on_child_message(&self, child: &mut V1Child, info: &Value, message_id: &str) {
        let role = info.get("role").and_then(Value::as_str);
        if let Some(role) = role {
            child.roles.insert(message_id.to_owned(), role.to_owned());
        }
        match role {
            Some("user") => self.start_child_turn(child, message_id, UtcDateTime::now()),
            Some("assistant") if info.pointer("/time/completed").is_some() => {
                if let Some(turn) = child.turn.as_mut() {
                    let owned: Vec<String> = turn
                        .parts
                        .iter()
                        .filter(|(_, record)| record.message_id == message_id)
                        .map(|(id, _)| id.clone())
                        .collect();
                    for part_id in owned {
                        self.emit_text_part(turn, &part_id, true);
                    }
                }
            }
            _ => {}
        }
    }

    /// `projectChildUserPart`: the child's prompt, as a user message on its turn's root.
    fn project_child_user_part(&self, turn: &mut V1Turn, part: &Value, message_id: &str) {
        let text = part.get("text").and_then(Value::as_str).unwrap_or("");
        if text.is_empty() {
            return;
        }
        let now = UtcDateTime::now();
        let ordinal = item_ordinal(turn, message_id);
        let artifacts = make_subagent_conversation_artifacts(SubagentConversationArtifactsInput {
            message_id: derive::message_from_provider_item(&self.driver, message_id),
            sender_thread_id: None,
            turn_item_id: derive::turn_item_from_provider_item(&self.driver, message_id),
            thread_id: turn.thread_id.clone(),
            root_node_id: turn.provider_turn.node_id.clone(),
            provider_thread_id: Some(turn.provider_turn.provider_thread_id.clone()),
            provider_turn_id: Some(turn.provider_turn.id.clone()),
            native_item_ref: Some(native_ref(message_id, false)),
            role: OrchestrationV2ConversationMessageRole::User,
            text: text.to_owned(),
            ordinal,
            now,
        });
        self.emit(ProviderAdapterV2Event::MessageUpdated {
            driver: self.driver.clone(),
            message: artifacts.message,
        });
        self.emit(ProviderAdapterV2Event::TurnItemUpdated {
            driver: self.driver.clone(),
            turn_item: artifacts.turn_item,
        });
    }

    /// `emitCompactionItem`: once per turn, the context was compacted.
    fn on_compacted(&self, guard: &mut V1Inner, session_id: &str) {
        let Some(turn) = guard
            .threads
            .get_mut(session_id)
            .and_then(|thread| thread.active.as_mut())
        else {
            return;
        };
        let native = format!("{}:compaction", turn.provider_turn.id.as_str());
        if turn.item_ordinals.contains_key(&native) {
            return;
        }
        let ordinal = item_ordinal(turn, &native);
        let now = UtcDateTime::now();
        let item = compaction_turn_item(
            &self.driver,
            Compaction {
                thread_id: &turn.thread_id,
                root_node_id: &turn.provider_turn.node_id,
                provider_thread_id: &turn.provider_turn.provider_thread_id,
                provider_turn_id: &turn.provider_turn.id,
                native_item_id: &native,
                ordinal,
                title: "Context compacted",
                status: OrchestrationV2TurnItemStatus::Completed,
                started_at: turn.provider_turn.started_at,
                completed_at: Some(now),
                now,
                summary: None,
                weak_ref: true,
            },
        );
        self.emit(ProviderAdapterV2Event::TurnItemUpdated {
            driver: self.driver.clone(),
            turn_item: item,
        });
    }

    /// `emitTodo`: a `todo.updated` list becomes a todo node, its plan and a todo item.
    fn on_todo(&self, guard: &mut V1Inner, props: &Value) {
        let Some(session_id) = str_field(props, "sessionID") else {
            return;
        };
        let Some(thread) = guard.threads.get_mut(&session_id) else {
            return;
        };
        let Some(turn) = thread.active.as_mut() else {
            return;
        };
        let todos: Vec<(String, String)> = props
            .get("todos")
            .and_then(Value::as_array)
            .map(|todos| {
                todos
                    .iter()
                    .map(|todo| {
                        (
                            todo.get("content")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                            todo.get("status")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_owned(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let now = UtcDateTime::now();
        let plan_id = turn
            .plan_id
            .get_or_insert_with(|| IdAllocator.plan(&turn.thread_id, None, &self.driver))
            .clone();
        let native = todo_native_id(&session_id, turn.provider_turn.id.as_str());
        let steps = todo_steps(&native, &todos);
        let completed = todos_complete(&steps);
        let thread_id = turn.thread_id.clone();
        let root_node_id = turn.provider_turn.node_id.clone();
        let provider_thread_id = turn.provider_turn.provider_thread_id.clone();
        let provider_turn_id = turn.provider_turn.id.clone();
        let started_at = turn.provider_turn.started_at.unwrap_or(now);
        let of = NodeOf {
            thread_id: &thread_id,
            root_node_id: &root_node_id,
            provider_thread_id: &provider_thread_id,
            provider_turn_id: &provider_turn_id,
            native_item_id: &native,
            weak_ref: true,
        };
        let node = execution_node(
            &self.driver,
            &of,
            NodeTiming {
                kind: OrchestrationV2ExecutionNodeKind::TodoList,
                status: if completed {
                    OrchestrationV2ExecutionNodeStatus::Completed
                } else {
                    OrchestrationV2ExecutionNodeStatus::Running
                },
                started_at,
                completed_at: completed.then_some(now),
            },
        );
        let node_id = node.id.clone();
        self.emit(ProviderAdapterV2Event::NodeUpdated {
            driver: self.driver.clone(),
            node,
        });
        self.emit(ProviderAdapterV2Event::PlanUpdated {
            driver: self.driver.clone(),
            plan: todo_plan(&plan_id, &thread_id, &node_id, completed, steps.clone()),
        });
        let ordinal = item_ordinal(turn, &native);
        self.emit(ProviderAdapterV2Event::TurnItemUpdated {
            driver: self.driver.clone(),
            turn_item: OrchestrationV2TurnItem::TodoList {
                tool_non_execution_kind: None,
                tool_surface: None,
                tool_icon: None,
                tool_source: None,
                id: derive::turn_item_from_provider_item(&self.driver, &native),
                thread_id,
                run_id: None,
                node_id: Some(node_id),
                provider_thread_id: Some(provider_thread_id),
                provider_turn_id: Some(provider_turn_id),
                native_item_ref: Some(native_ref(&native, true)),
                parent_item_id: None,
                ordinal,
                status: if completed {
                    OrchestrationV2TurnItemStatus::Completed
                } else {
                    OrchestrationV2TurnItemStatus::Running
                },
                title: Some("Todo list".to_owned()),
                started_at: Some(started_at),
                completed_at: completed.then_some(now),
                updated_at: now,
                plan_id,
                steps,
                explanation: None,
            },
        });
    }

    fn on_request(&self, guard: &mut V1Inner, event_type: &str, props: &Value) {
        let native_id = str_field(props, "id").unwrap_or_default();
        let session_id = str_field(props, "sessionID").unwrap_or_default();
        let owner = guard
            .children
            .get(&session_id)
            .cloned()
            .unwrap_or_else(|| session_id.clone());
        if guard.pending.contains_key(&native_id) {
            return;
        }
        let user_input = event_type == "question.asked";
        let kind = if user_input {
            "user_input"
        } else {
            open_code_permission_request_kind(
                props
                    .get("permission")
                    .and_then(Value::as_str)
                    .unwrap_or("bash"),
                props
                    .get("tool")
                    .and_then(|tool| tool.get("name"))
                    .and_then(Value::as_str),
            )
        };
        let now = UtcDateTime::now();
        let Some(thread) = guard.threads.get_mut(&owner) else {
            return;
        };
        let Some(turn) = thread.active.as_mut() else {
            return;
        };
        let provider_turn_id = turn.provider_turn.id.clone();
        let thread_id = turn.thread_id.clone();
        let root_node_id = turn.provider_turn.node_id.clone();
        let provider_thread_id = turn.provider_turn.provider_thread_id.clone();
        let request_id =
            IdAllocator.runtime_request(&self.driver, Some(&provider_turn_id), Some(&native_id));
        let ordinal = item_ordinal(turn, &native_id);
        let of = NodeOf {
            thread_id: &thread_id,
            root_node_id: &root_node_id,
            provider_thread_id: &provider_thread_id,
            provider_turn_id: &provider_turn_id,
            native_item_id: &native_id,
            weak_ref: false,
        };
        let node = request_node(&self.driver, &of, &request_id, user_input, now);
        let body = if user_input {
            let questions: Vec<Value> = props
                .get("questions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            RequestBody::UserInput {
                questions: user_input_questions(&questions),
            }
        } else {
            RequestBody::Approval {
                request_kind: match kind {
                    "file-read" => ProviderRequestKind::FileRead,
                    "file-change" => ProviderRequestKind::FileChange,
                    _ => ProviderRequestKind::Command,
                },
                title: props
                    .get("permission")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                prompt: None,
                options: None,
            }
        };
        let item = request_turn_item(&of, &request_id, ordinal, now, body);
        let request = OrchestrationV2RuntimeRequest {
            id: request_id.clone(),
            node_id: node.id.clone(),
            provider_turn_id: Some(provider_turn_id),
            native_request_ref: Some(native_ref(&native_id, false)),
            kind: match kind {
                "file-read" => OrchestrationV2RuntimeRequestKind::FileRead,
                "file-change" => OrchestrationV2RuntimeRequestKind::FileChange,
                "user_input" => OrchestrationV2RuntimeRequestKind::UserInput,
                _ => OrchestrationV2RuntimeRequestKind::Command,
            },
            status: OrchestrationV2RuntimeRequestStatus::Pending,
            response_capability: OrchestrationV2ResponseCapability::Live {
                provider_session_id: self.provider_session_id.clone(),
            },
            created_at: now,
            resolved_at: None,
            decision: None,
            answers: None,
        };
        thread.requests.push(request.clone());
        guard.pending.insert(
            native_id.clone(),
            Pending {
                request_id,
                native_id,
                session_id,
                kind,
                question: props.get("questions").cloned(),
                node: node.clone(),
                item: item.clone(),
            },
        );
        self.emit(ProviderAdapterV2Event::NodeUpdated {
            driver: self.driver.clone(),
            node,
        });
        self.emit(ProviderAdapterV2Event::TurnItemUpdated {
            driver: self.driver.clone(),
            turn_item: item,
        });
        self.emit(ProviderAdapterV2Event::RuntimeRequestUpdated {
            driver: self.driver.clone(),
            thread_id: Some(thread_id),
            runtime_request: request,
        });
    }

    fn resolve_request(
        &self,
        guard: &mut V1Inner,
        pending: Pending,
        status: OrchestrationV2RuntimeRequestStatus,
    ) {
        let now = UtcDateTime::now();
        let cancelled = status == OrchestrationV2RuntimeRequestStatus::Cancelled;
        let owner = guard
            .children
            .get(&pending.session_id)
            .cloned()
            .unwrap_or_else(|| pending.session_id.clone());
        if let Some(thread) = guard.threads.get_mut(&owner)
            && let Some(request) = thread
                .requests
                .iter_mut()
                .find(|request| request.id == pending.request_id)
        {
            request.status = status;
            request.resolved_at = Some(now);
            let thread_id = thread.active.as_ref().map(|turn| turn.thread_id.clone());
            self.emit(ProviderAdapterV2Event::RuntimeRequestUpdated {
                driver: self.driver.clone(),
                thread_id,
                runtime_request: request.clone(),
            });
        }
        // `settleRequest`: the node and item the request stood for settle with it.
        self.emit(ProviderAdapterV2Event::NodeUpdated {
            driver: self.driver.clone(),
            node: settle_request_node(&pending.node, cancelled, now),
        });
        self.emit(ProviderAdapterV2Event::TurnItemUpdated {
            driver: self.driver.clone(),
            turn_item: settle_request_item(&pending.item, cancelled, now),
        });
    }

    fn require_session(
        guard: &V1Inner,
        thread: &OrchestrationV2ProviderThread,
    ) -> Result<String, ProviderAdapterV2Error> {
        thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.as_ref())
            .map(|id| id.as_str().to_owned())
            .filter(|id| guard.threads.contains_key(id))
            .ok_or_else(|| {
                ProviderAdapterV2Error::protocol(format!(
                    "Provider thread {} has no OpenCode session id",
                    thread.id
                ))
            })
    }
}

#[async_trait::async_trait]
impl ProviderSessionRuntime for OpenCodeV1Session {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.instance_id
    }
    fn driver(&self) -> &ProviderDriverKind {
        &self.driver
    }
    fn provider_session_id(&self) -> &ProviderSessionId {
        &self.provider_session_id
    }
    fn provider_session(&self) -> &OrchestrationV2ProviderSession {
        &self.session
    }
    fn events(&self) -> ProviderEventStream {
        let rx = self.rx.lock().take();
        match rx {
            Some(rx) => Box::pin(UnboundedReceiverStream::new(rx)),
            None => Box::pin(futures::stream::empty()),
        }
    }
    fn has_pending_background_work(
        &self,
    ) -> Option<BoxFuture<'_, Result<bool, ProviderAdapterV2Error>>> {
        let pending = !self.inner.lock().busy.is_empty();
        Some(Box::pin(async move { Ok(pending) }))
    }
    fn get_model_context_window(
        &self,
        _model_selection: &turin_contracts::model_selection::ModelSelection,
        _cwd: Option<&str>,
    ) -> Option<Option<u64>> {
        None
    }

    async fn ensure_thread(
        &self,
        input: ProviderAdapterV2EnsureThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        if input
            .existing_provider_thread
            .as_ref()
            .and_then(|thread| thread.native_thread_ref.as_ref())
            .is_some()
        {
            return self
                .resume_thread(ProviderAdapterV2ResumeThreadInput {
                    provider_thread: input.existing_provider_thread.expect("checked"),
                    thread_id: Some(input.thread_id),
                    model_selection: Some(input.model_selection),
                    runtime_policy: Some(input.runtime_policy),
                })
                .await;
        }
        let rules = open_code_permission_rules(
            input.runtime_policy.runtime_mode,
            input.runtime_policy.approval_policy.as_ref(),
            input.runtime_policy.sandbox_policy.as_ref(),
        );
        let created = self
            .client
            .create_session(&json!({ "permission": rules_json(&rules) }))
            .await
            .map_err(http_err)?;
        let native_id = created
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ProviderAdapterV2Error::protocol("OpenCode session.create returned no id")
            })?
            .to_owned();
        let now = UtcDateTime::now();
        let provider_thread = make_thread(
            &self.instance_id,
            &self.provider_session_id,
            &input.thread_id,
            &native_id,
            input.existing_provider_thread,
            now,
        );
        self.inner.lock().threads.insert(
            native_id,
            V1Thread {
                provider_thread: provider_thread.clone(),
                active: None,
                turns: Vec::new(),
                requests: Vec::new(),
                message_roles: HashMap::new(),
            },
        );
        self.emit(ProviderAdapterV2Event::ProviderThreadUpdated {
            driver: self.driver.clone(),
            provider_thread: provider_thread.clone(),
            retained_native_turn_ids: None,
        });
        Ok(provider_thread)
    }

    async fn resume_thread(
        &self,
        input: ProviderAdapterV2ResumeThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        let native_id = input
            .provider_thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.as_ref())
            .map(|id| id.as_str().to_owned())
            .ok_or_else(|| {
                ProviderAdapterV2Error::protocol(format!(
                    "Provider thread {} has no OpenCode session id",
                    input.provider_thread.id
                ))
            })?;
        self.client
            .get_session(&native_id)
            .await
            .map_err(http_err)?;
        let mut thread = input.provider_thread;
        thread.provider_session_id = Some(self.provider_session_id.clone());
        thread.status = OrchestrationV2ProviderThreadStatus::Idle;
        thread.updated_at = UtcDateTime::now();
        self.inner.lock().threads.insert(
            native_id,
            V1Thread {
                provider_thread: thread.clone(),
                active: None,
                turns: Vec::new(),
                requests: Vec::new(),
                message_roles: HashMap::new(),
            },
        );
        Ok(thread)
    }

    async fn start_turn(
        &self,
        input: ProviderAdapterV2TurnInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        let session_id = {
            let guard = self.inner.lock();
            if guard.stream_dead.is_some() {
                return Err(ProviderAdapterV2Error::protocol(
                    "OpenCode event stream has ended; reconnect the provider session before starting another turn.",
                ));
            }
            let session_id = Self::require_session(&guard, &input.provider_thread)?;
            if guard
                .threads
                .get(&session_id)
                .and_then(|thread| thread.active.as_ref())
                .is_some()
            {
                return Err(ProviderAdapterV2Error::protocol(format!(
                    "OpenCode provider thread {} already has an active turn",
                    input.provider_thread.id
                )));
            }
            session_id
        };
        let parsed =
            parse_open_code_model_slug(input.model_selection.model.as_str()).ok_or_else(|| {
                ProviderAdapterV2Error::protocol(malformed_model(
                    input.model_selection.model.as_str(),
                ))
            })?;
        let text = input.message.text.trim().to_owned();
        let bare = input.message.attachments.is_empty();
        let is_compact = bare && text == "/compact";
        let now = UtcDateTime::now();
        let native_turn = format!("{session_id}:attempt:{}", input.attempt_id);
        let provider_turn_id = derive::provider_turn(&self.driver, &native_turn);
        let ordinal = PositiveInt::new(input.provider_turn_ordinal.max(1) as u64)
            .unwrap_or(PositiveInt::new(1).expect("one"));
        let provider_turn = OrchestrationV2ProviderTurn {
            id: provider_turn_id,
            provider_thread_id: input.provider_thread.id.clone(),
            node_id: input.root_node_id.clone(),
            run_attempt_id: Some(input.attempt_id.clone()),
            native_turn_ref: Some(native_ref(&native_turn, true)),
            ordinal,
            status: OrchestrationV2ProviderTurnStatus::Running,
            started_at: Some(now),
            completed_at: None,
            token_usage: None,
            turn_token_usage: None,
        };
        {
            let mut guard = self.inner.lock();
            let thread = guard.threads.get_mut(&session_id).ok_or_else(|| {
                ProviderAdapterV2Error::protocol(format!(
                    "OpenCode session {session_id} is not registered"
                ))
            })?;
            thread.turns.push(provider_turn.clone());
            thread.provider_thread.status = OrchestrationV2ProviderThreadStatus::Active;
            thread.provider_thread.updated_at = now;
            thread.active = Some(V1Turn {
                provider_turn: provider_turn.clone(),
                thread_id: input.thread_id.clone(),
                run_ordinal: input.run_ordinal,
                admission: AdmissionFlags {
                    admission_pending: !is_compact,
                    admission_accepted: is_compact,
                    ..AdmissionFlags::default()
                },
                interrupted: false,
                finalized: false,
                usage: UsageAccum::new(),
                app_thread: input.app_thread.clone(),
                next_item_ordinal: input.provider_turn_ordinal.max(0) as u64 * 100 + 1,
                item_ordinals: HashMap::new(),
                plan_id: None,
                parts: HashMap::new(),
            });
        }
        self.emit(ProviderAdapterV2Event::ProviderTurnUpdated {
            driver: self.driver.clone(),
            thread_id: Some(input.thread_id.clone()),
            provider_turn,
        });
        let result = if is_compact {
            self.client
                .summarize_session(
                    &session_id,
                    &json!({
                        "providerID": parsed.provider_id,
                        "modelID": parsed.model_id,
                        "auto": false
                    }),
                )
                .await
                .map(|_| ())
                .map_err(http_err)
        } else {
            self.submit_prompt(&session_id, &input, &parsed, &text)
                .await
        };
        if let Err(err) = result {
            let mut guard = self.inner.lock();
            self.finish_active(
                &mut guard,
                &session_id,
                ProviderAdapterV2TurnTerminalStatus::Failed,
                Some(err.to_string()),
                OrchestrationV2ProviderThreadDisposition::Reusable,
            );
            return Err(err);
        }
        if !is_compact {
            let mut guard = self.inner.lock();
            if let Some(turn) = guard
                .threads
                .get_mut(&session_id)
                .and_then(|thread| thread.active.as_mut())
            {
                let action = advance_open_code_prompt_admission(
                    &mut turn.admission,
                    OpenCodeAdmissionSignal::Accepted,
                );
                if action == OpenCodeAdmissionAction::ReconcileIdle {
                    cancel_open_code_prompt_admission(&mut turn.admission);
                }
            }
        } else {
            let mut guard = self.inner.lock();
            self.finish_active(
                &mut guard,
                &session_id,
                ProviderAdapterV2TurnTerminalStatus::Completed,
                None,
                OrchestrationV2ProviderThreadDisposition::Reusable,
            );
        }
        Ok(())
    }

    fn compact_thread(
        &self,
        mut input: ProviderAdapterV2TurnInput,
    ) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        input.message.text = "/compact".to_owned();
        Some(Box::pin(async move { self.start_turn(input).await }))
    }

    async fn steer_turn(
        &self,
        input: ProviderAdapterV2SteerInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        let session_id = {
            let guard = self.inner.lock();
            Self::require_session(&guard, &input.provider_thread)?.to_owned()
        };
        let text = input.message.text.trim().to_owned();
        if text.is_empty() {
            return Err(ProviderAdapterV2Error::protocol(
                "OpenCode steering requires text or an attachment",
            ));
        }
        self.client
            .prompt_async(
                &session_id,
                &json!({ "parts": [{ "type": "text", "text": text }] }),
            )
            .await
            .map_err(http_err)
    }

    async fn interrupt_turn(
        &self,
        input: ProviderAdapterV2InterruptInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        let session_id = {
            let guard = self.inner.lock();
            let session_id = Self::require_session(&guard, &input.provider_thread)?.to_owned();
            if let Some(turn) = guard
                .threads
                .get(&session_id)
                .and_then(|thread| thread.active.as_ref())
                && turn.provider_turn.id != input.provider_turn_id
            {
                return Ok(());
            }
            session_id
        };
        {
            let mut guard = self.inner.lock();
            if let Some(turn) = guard
                .threads
                .get_mut(&session_id)
                .and_then(|thread| thread.active.as_mut())
            {
                turn.interrupted = true;
                cancel_open_code_prompt_admission(&mut turn.admission);
            }
        }
        self.client
            .abort_session(&session_id)
            .await
            .map_err(http_err)?;
        let children: Vec<String> = {
            let guard = self.inner.lock();
            guard
                .children
                .iter()
                .filter(|(_, parent)| parent.as_str() == session_id)
                .map(|(child, _)| child.clone())
                .collect()
        };
        for child in children {
            let _ = self.client.abort_session(&child).await;
        }
        Ok(())
    }

    async fn respond_to_runtime_request(
        &self,
        input: ProviderAdapterV2RuntimeRequestResponseInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        let pending = {
            let guard = self.inner.lock();
            guard
                .pending
                .values()
                .find(|pending| pending.request_id == input.request_id)
                .map(|pending| {
                    (
                        pending.native_id.clone(),
                        pending.kind,
                        pending.question.clone(),
                    )
                })
                .ok_or_else(|| {
                    ProviderAdapterV2Error::protocol(format!(
                        "No pending OpenCode request {}",
                        input.request_id
                    ))
                })?
        };
        if pending.1 == "user_input" {
            let answers = input.answers.ok_or_else(|| {
                ProviderAdapterV2Error::protocol(format!(
                    "OpenCode question request {} requires answers",
                    input.request_id
                ))
            })?;
            let rows = question_rows(pending.2.as_ref(), &answers);
            self.client
                .reply_question(&pending.0, &QuestionReply { answers: rows })
                .await
                .map_err(http_err)?;
        } else {
            let decision = input.decision.ok_or_else(|| {
                ProviderAdapterV2Error::protocol(format!(
                    "OpenCode approval request {} requires a decision",
                    input.request_id
                ))
            })?;
            self.client
                .reply_permission(
                    &pending.0,
                    &PermissionReply {
                        reply: v1_permission_reply(decision).to_owned(),
                        message: None,
                    },
                )
                .await
                .map_err(http_err)?;
        }
        Ok(())
    }

    async fn read_thread_snapshot(
        &self,
        input: ProviderAdapterV2ReadThreadSnapshotInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        let session_id = input
            .provider_thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.as_ref())
            .map(|id| id.as_str().to_owned())
            .ok_or_else(|| ProviderAdapterV2Error::protocol("OpenCode snapshot has no session"))?;
        let native = self
            .client
            .get_session(&session_id)
            .await
            .map_err(http_err)?;
        let messages = self
            .client
            .session_messages(&session_id)
            .await
            .map_err(http_err)?;
        let revert = native.pointer("/revert/messageID").and_then(Value::as_str);
        let guard = self.inner.lock();
        let stored = guard.threads.get(&session_id);
        Ok(snapshot_from(
            input.provider_thread,
            &messages,
            revert,
            stored
                .map(|thread| thread.turns.clone())
                .unwrap_or_default(),
            stored
                .map(|thread| thread.requests.clone())
                .unwrap_or_default(),
        ))
    }

    async fn rollback_thread(
        &self,
        input: ProviderAdapterV2RollbackThreadInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        let session_id = input
            .provider_thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.as_ref())
            .map(|id| id.as_str().to_owned())
            .ok_or_else(|| ProviderAdapterV2Error::protocol("OpenCode rollback has no session"))?;
        {
            let guard = self.inner.lock();
            if guard
                .threads
                .get(&session_id)
                .and_then(|thread| thread.active.as_ref())
                .is_some()
            {
                return Err(ProviderAdapterV2Error::protocol(format!(
                    "Cannot roll back OpenCode thread {} while a turn is active",
                    input.provider_thread.id
                )));
            }
        }
        let messages = self
            .client
            .session_messages(&session_id)
            .await
            .map_err(http_err)?;
        let boundary = match &input.target {
            ProviderAdapterV2RollbackTarget::ThreadStart { .. } => {
                messages.iter().find_map(|entry| {
                    (entry.pointer("/info/role").and_then(Value::as_str) == Some("user"))
                        .then(|| {
                            entry
                                .pointer("/info/id")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .flatten()
                })
            }
            ProviderAdapterV2RollbackTarget::ProviderTurn { provider_turn, .. } => {
                open_code_boundary_after_provider_turn(
                    &input.provider_thread_turns,
                    &provider_turn.id,
                )
            }
        };
        let mut retained = input.provider_thread.clone();
        if let Some(boundary) = boundary {
            let forked = self
                .client
                .fork_session(&session_id, &json!({ "messageID": boundary }))
                .await
                .map_err(http_err)?;
            let fork_id = forked
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| ProviderAdapterV2Error::protocol("OpenCode fork returned no id"))?;
            retained.native_thread_ref = Some(native_ref(fork_id, false));
            retained.updated_at = UtcDateTime::now();
            self.inner.lock().threads.insert(
                fork_id.to_owned(),
                V1Thread {
                    provider_thread: retained.clone(),
                    active: None,
                    turns: Vec::new(),
                    requests: Vec::new(),
                    message_roles: HashMap::new(),
                },
            );
        }
        self.read_thread_snapshot(ProviderAdapterV2ReadThreadSnapshotInput {
            provider_thread: retained,
        })
        .await
    }

    async fn fork_thread(
        &self,
        input: ProviderAdapterV2ForkThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        let session_id = input
            .source_provider_thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.as_ref())
            .map(|id| id.as_str().to_owned())
            .ok_or_else(|| ProviderAdapterV2Error::protocol("OpenCode fork has no session"))?;
        let boundary = input.provider_turn_id.as_ref().and_then(|turn_id| {
            open_code_boundary_after_provider_turn(
                input.source_provider_turns.as_deref().unwrap_or(&[]),
                turn_id,
            )
        });
        let body = match boundary {
            Some(boundary) => json!({ "messageID": boundary }),
            None => json!({}),
        };
        let forked = self
            .client
            .fork_session(&session_id, &body)
            .await
            .map_err(http_err)?;
        let fork_id = forked
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| ProviderAdapterV2Error::protocol("OpenCode fork returned no id"))?;
        let now = UtcDateTime::now();
        let thread = make_thread(
            &self.instance_id,
            &self.provider_session_id,
            &input.target_thread_id,
            fork_id,
            None,
            now,
        );
        self.inner.lock().threads.insert(
            fork_id.to_owned(),
            V1Thread {
                provider_thread: thread.clone(),
                active: None,
                turns: Vec::new(),
                requests: Vec::new(),
                message_roles: HashMap::new(),
            },
        );
        Ok(thread)
    }
}

impl OpenCodeV1Session {
    async fn submit_prompt(
        &self,
        session_id: &str,
        input: &ProviderAdapterV2TurnInput,
        parsed: &ParsedModelSlug,
        text: &str,
    ) -> Result<(), ProviderAdapterV2Error> {
        let command = if input.message.attachments.is_empty() {
            command_of(text)
        } else {
            None
        };
        if let Some(command) = command {
            let commands = self.client.list_commands().await.unwrap_or_default();
            if commands.iter().any(|entry| {
                entry.get("name").and_then(Value::as_str) == Some(command.name.as_str())
            }) {
                let mut body = json!({
                    "command": command.name,
                    "arguments": command.text,
                    "model": format!("{}/{}", parsed.provider_id, parsed.model_id),
                });
                let agent = agent_for(
                    input.runtime_policy.interaction_mode,
                    &input.model_selection,
                );
                body["agent"] = json!(agent);
                if let Some(variant) = model_option(&input.model_selection, "variant") {
                    body["variant"] = json!(variant);
                }
                return self
                    .client
                    .command(session_id, &body)
                    .await
                    .map(|_| ())
                    .map_err(http_err);
            }
        }
        let system = per_turn_instructions(input.model_selection.model.as_str());
        let mut body = json!({
            "model": { "providerID": parsed.provider_id, "modelID": parsed.model_id },
            "system": system,
            "parts": [{ "type": "text", "text": text }],
        });
        if let Some(variant) = model_option(&input.model_selection, "variant") {
            body["variant"] = json!(variant);
        }
        let agent = agent_for(
            input.runtime_policy.interaction_mode,
            &input.model_selection,
        );
        if input.runtime_policy.interaction_mode
            == turin_contracts::provider_policy::ProviderInteractionMode::Plan
            || model_option(&input.model_selection, "agent").is_some()
        {
            body["agent"] = json!(agent);
        }
        self.client
            .prompt_async(session_id, &body)
            .await
            .map_err(http_err)
    }
}

fn http_err(err: crate::error::OpenCodeRuntimeError) -> ProviderAdapterV2Error {
    ProviderAdapterV2Error::protocol(err.detail)
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn rules_json(rules: &[V1PermissionRule]) -> Value {
    Value::Array(
        rules
            .iter()
            .map(|rule| {
                json!({
                    "permission": rule.permission,
                    "pattern": rule.pattern,
                    "action": rule.action,
                })
            })
            .collect(),
    )
}

fn make_thread(
    instance_id: &ProviderInstanceId,
    session_id: &ProviderSessionId,
    thread_id: &ThreadId,
    native_id: &str,
    existing: Option<OrchestrationV2ProviderThread>,
    now: UtcDateTime,
) -> OrchestrationV2ProviderThread {
    let mut thread = existing.unwrap_or_else(|| OrchestrationV2ProviderThread {
        id: derive::provider_thread(&opencode_driver(), Some(instance_id), native_id),
        driver: opencode_driver(),
        provider_instance_id: instance_id.clone(),
        provider_session_id: Some(session_id.clone()),
        app_thread_id: Some(thread_id.clone()),
        owner_node_id: None,
        native_thread_ref: None,
        native_conversation_head_ref: None,
        status: OrchestrationV2ProviderThreadStatus::Idle,
        first_run_ordinal: None,
        last_run_ordinal: None,
        handoff_ids: Vec::new(),
        forked_from: None,
        pending_background_tasks: Vec::new(),
        context_usage: None,
        native_metadata: None,
        goal: None,
        created_at: now,
        updated_at: now,
    });
    thread.provider_session_id = Some(session_id.clone());
    thread.native_thread_ref = Some(native_ref(native_id, false));
    thread.status = OrchestrationV2ProviderThreadStatus::Idle;
    thread.updated_at = now;
    thread
}

/// `itemOrdinal`: an item keeps the ordinal of its first emission, so updates do not renumber it.
fn item_ordinal(turn: &mut V1Turn, native_id: &str) -> u64 {
    next_ordinal(
        &mut turn.item_ordinals,
        &mut turn.next_item_ordinal,
        native_id,
    )
}

fn tool_base(
    turn: &mut V1Turn,
    driver: &ProviderDriverKind,
    native_id: &str,
    status: OrchestrationV2TurnItemStatus,
    started_at: UtcDateTime,
    completed_at: Option<UtcDateTime>,
) -> ToolItemBase {
    let ordinal = item_ordinal(turn, native_id);
    ToolItemBase {
        id: derive::turn_item_from_provider_item(driver, native_id),
        thread_id: turn.thread_id.clone(),
        run_id: None,
        node_id: Some(derive::node_from_provider_item(driver, native_id)),
        provider_thread_id: Some(turn.provider_turn.provider_thread_id.clone()),
        provider_turn_id: Some(turn.provider_turn.id.clone()),
        native_item_ref: Some(native_ref(native_id, false)),
        ordinal,
        status,
        title: None,
        started_at: Some(started_at),
        completed_at,
        updated_at: UtcDateTime::now(),
    }
}

fn question_rows(
    questions: Option<&Value>,
    answers: &turin_contracts::provider_policy::ProviderUserInputAnswers,
) -> Vec<Vec<String>> {
    let Some(Value::Array(questions)) = questions else {
        return answers
            .values()
            .map(|value| match value {
                Value::Array(items) => items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect(),
                Value::String(text) if !text.trim().is_empty() => vec![text.clone()],
                _ => Vec::new(),
            })
            .collect();
    };
    questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            let header = question.get("header").and_then(Value::as_str).unwrap_or("");
            let prompt = question
                .get("question")
                .and_then(Value::as_str)
                .unwrap_or("");
            let id = open_code_question_id(index, header);
            let raw = answers
                .get(&id)
                .or_else(|| answers.get(header))
                .or_else(|| answers.get(prompt));
            match raw {
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect(),
                Some(Value::String(text)) if !text.trim().is_empty() => vec![text.clone()],
                _ => Vec::new(),
            }
        })
        .collect()
}

fn snapshot_from(
    mut provider_thread: OrchestrationV2ProviderThread,
    messages: &[Value],
    revert: Option<&str>,
    turns: Vec<OrchestrationV2ProviderTurn>,
    requests: Vec<OrchestrationV2RuntimeRequest>,
) -> ProviderAdapterV2ThreadSnapshot {
    let now = UtcDateTime::now();
    let mut out = Vec::new();
    for entry in messages {
        let id = entry
            .pointer("/info/id")
            .and_then(Value::as_str)
            .unwrap_or("");
        if Some(id) == revert {
            break;
        }
        let role = entry
            .pointer("/info/role")
            .and_then(Value::as_str)
            .unwrap_or("assistant");
        let text = entry
            .get("parts")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                    .filter(|part| part.get("ignored").and_then(Value::as_bool) != Some(true))
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if text.is_empty() {
            continue;
        }
        let thread_id = provider_thread
            .app_thread_id
            .clone()
            .unwrap_or_else(|| ThreadId::new("thread").expect("thread"));
        out.push(OrchestrationV2ConversationMessage {
            notification: None,
            created_by: if role == "user" {
                OrchestrationV2Actor::User
            } else {
                OrchestrationV2Actor::Agent
            },
            creation_source: OrchestrationV2CreationSource::Provider,
            scheduled_task_id: None,
            sender_thread_id: None,
            id: derive::message_from_provider_item(&opencode_driver(), id),
            thread_id,
            run_id: None,
            node_id: None,
            role: if role == "user" {
                OrchestrationV2ConversationMessageRole::User
            } else {
                OrchestrationV2ConversationMessageRole::Assistant
            },
            text,
            context: None,
            attachments: Vec::new(),
            streaming: false,
            created_at: now,
            updated_at: now,
            delegated_completion: None,
        });
    }
    provider_thread.status = OrchestrationV2ProviderThreadStatus::Idle;
    provider_thread.updated_at = now;
    ProviderAdapterV2ThreadSnapshot {
        provider_thread,
        provider_turns: turns,
        messages: out,
        runtime_requests: requests,
        provider_payload: Some(Value::Array(messages.to_vec())),
    }
}

impl Drop for OpenCodeV1Session {
    fn drop(&mut self) {
        if let Some(handle) = self.reader.lock().take() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_per_attempt_from_the_retry_delay_and_caps_at_30s() {
        assert_eq!(reconnect_backoff(RECONNECT_BASE, 1), Duration::from_secs(3));
        assert_eq!(reconnect_backoff(RECONNECT_BASE, 2), Duration::from_secs(6));
        assert_eq!(
            reconnect_backoff(RECONNECT_BASE, 4),
            Duration::from_secs(24)
        );
        assert_eq!(reconnect_backoff(RECONNECT_BASE, 5), RECONNECT_CAP);
        assert_eq!(reconnect_backoff(RECONNECT_BASE, 400), RECONNECT_CAP);
        assert_eq!(
            reconnect_backoff(Duration::from_millis(10), 1),
            Duration::from_millis(10)
        );
    }
}
