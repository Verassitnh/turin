//! Port of `upstream/packages/provider-opencode/src/server/adapter.ts`.
//!
//! One directory-scoped v1 client per session. Prompts carry per-turn
//! instructions in `system`. `/name` that matches the command list is
//! `session.command`; a bare `/compact` is `session.summarize`. Permission
//! replies use `once` / `always` / `reject`. A clean event-stream EOF breaks
//! the session. Transport errors resubscribe with the 3s→30s backoff and do
//! not backfill message history.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Value, json};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use turin_contracts::{
    PositiveInt, ProviderSessionId, RuntimeRequestId, ThreadId,
    UtcDateTime,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2ConversationMessage,
        OrchestrationV2ConversationMessageRole, OrchestrationV2CreationSource,
        OrchestrationV2ProviderFailureClass,
        OrchestrationV2ProviderSession, OrchestrationV2ProviderSessionStatus,
        OrchestrationV2ProviderThread, OrchestrationV2ProviderThreadDisposition,
        OrchestrationV2ProviderThreadStatus,
        OrchestrationV2ProviderTurn, OrchestrationV2ProviderTurnStatus,
        OrchestrationV2ResponseCapability, OrchestrationV2RuntimeRequest,
        OrchestrationV2RuntimeRequestKind, OrchestrationV2RuntimeRequestStatus,
        OrchestrationV2Subagent, OrchestrationV2SubagentOrigin, OrchestrationV2SubagentStatus,
        OrchestrationV2TurnItemStatus,
    },
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
};
use turin_provider_core::id_allocator::{IdAllocator, derive};

use crate::adapter::api::*;
use crate::adapter::logic::*;
use crate::error::EventStreamError;
use crate::events::V1Event;
use crate::v1_client::{PermissionReply, QuestionReply, V1Client};

const RECONNECT_BASE: Duration = Duration::from_secs(3);
const RECONNECT_CAP: Duration = Duration::from_secs(30);

struct V1Turn {
    provider_turn: OrchestrationV2ProviderTurn,
    thread_id: ThreadId,
    run_ordinal: i64,
    admission: AdmissionFlags,
    interrupted: bool,
    finalized: bool,
    usage: UsageAccum,
    next_ordinal: u64,
    texts: HashMap<String, String>,
}

struct V1Thread {
    provider_thread: OrchestrationV2ProviderThread,
    active: Option<V1Turn>,
    turns: Vec<OrchestrationV2ProviderTurn>,
    requests: Vec<OrchestrationV2RuntimeRequest>,
}

struct Pending {
    request_id: RuntimeRequestId,
    native_id: String,
    session_id: String,
    kind: &'static str,
    question: Option<Value>,
}

struct V1Inner {
    threads: HashMap<String, V1Thread>,
    pending: HashMap<String, Pending>,
    busy: HashSet<String>,
    children: HashMap<String, String>,
    stream_dead: Option<String>,
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
    rx: Mutex<Option<mpsc::UnboundedReceiver<Result<ProviderAdapterV2Event, ProviderAdapterV2Error>>>>,
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
        let client = V1Client::new(base_url, cwd, password).map_err(|err| {
            ProviderAdapterV2Error::protocol(format!("OpenCode v1 client: {}", err.detail))
        })?;
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
        let mut delay = RECONNECT_BASE;
        let mut attempts = 0u32;
        loop {
            let stream = match self.client.events().await {
                Ok(stream) => stream,
                Err(err) => {
                    attempts += 1;
                    if attempts >= V1_RECONNECT_ATTEMPTS {
                        self.fail_stream(&err.detail).await;
                        return;
                    }
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(RECONNECT_CAP);
                    continue;
                }
            };
            attempts = 0;
            delay = RECONNECT_BASE;
            let mut stream = stream;
            loop {
                match stream.next().await {
                    Some(Ok(event)) => self.on_event(event).await,
                    Some(Err(EventStreamError::Silent { .. })) | Some(Err(_)) => break,
                    None => {
                        self.fail_stream("OpenCode event stream ended unexpectedly.").await;
                        return;
                    }
                }
            }
            attempts += 1;
            if attempts >= V1_RECONNECT_ATTEMPTS {
                self.fail_stream("The OpenCode event stream was lost and could not reconnect.")
                    .await;
                return;
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(RECONNECT_CAP);
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
            if let Some(turn) = thread.active.as_mut() {
                if !turn.finalized {
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
                let parent = info.get("parentID").and_then(Value::as_str).map(str::to_owned);
                if let (Some(id), Some(parent)) = (id, parent) {
                    if guard.threads.contains_key(&parent) {
                        guard.children.insert(id.clone(), parent.clone());
                        if let Some(thread) = guard.threads.get(&parent) {
                            if let Some(turn) = thread.active.as_ref() {
                                let subagent = subagent_for(
                                    &self.instance_id,
                                    &thread.provider_thread,
                                    &turn.thread_id,
                                    turn.provider_turn.run_attempt_id.as_ref().map(|_| turn.provider_turn.node_id.clone()),
                                    &id,
                                    &turn.provider_turn,
                                );
                                drop(guard);
                                self.emit(ProviderAdapterV2Event::SubagentUpdated {
                                    driver: self.driver.clone(),
                                    subagent,
                                });
                                return;
                            }
                        }
                    }
                }
            }
            "session.status" | "session.idle" => {
                let session_id = str_field(&props, "sessionID");
                let status_type = props
                    .get("status")
                    .and_then(|status| status.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or(if event.event_type == "session.idle" { "idle" } else { "" });
                if let Some(session_id) = session_id {
                    if status_type == "idle" {
                        guard.busy.remove(&session_id);
                    } else if status_type == "busy" || status_type == "retry" {
                        guard.busy.insert(session_id.clone());
                    }
                    if status_type == "idle" {
                        self.settle_idle(&mut guard, &session_id);
                    } else if status_type == "busy" {
                        if let Some(thread) = guard.threads.get_mut(&session_id) {
                            if let Some(turn) = thread.active.as_mut() {
                                let _ = advance_open_code_prompt_admission(
                                    &mut turn.admission,
                                    OpenCodeAdmissionSignal::Busy,
                                );
                            }
                        }
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
                if let (Some(session_id), Some("user"), Some(_)) = (session_id, role, message_id) {
                    if let Some(thread) = guard.threads.get_mut(&session_id) {
                        if let Some(turn) = thread.active.as_mut() {
                            let _ = advance_open_code_prompt_admission(
                                &mut turn.admission,
                                OpenCodeAdmissionSignal::UserMessage,
                            );
                        }
                    }
                }
                if role == Some("assistant") {
                    let completed = info.get("time").and_then(|time| time.get("completed")).is_some();
                    if completed {
                        if let Some(session_id) = str_field(&info, "sessionID") {
                            if let Some(thread) = guard.threads.get_mut(&session_id) {
                                if let Some(turn) = thread.active.as_mut() {
                                    let _ = advance_open_code_prompt_admission(
                                        &mut turn.admission,
                                        OpenCodeAdmissionSignal::AssistantCompleted,
                                    );
                                }
                            }
                        }
                    }
                }
            }
            "message.part.updated" | "message.part.delta" => {
                self.on_part(&mut guard, &event.event_type, &props);
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
        let action = advance_open_code_prompt_admission(&mut turn.admission, OpenCodeAdmissionSignal::Idle);
        if action == OpenCodeAdmissionAction::Hold {
            return;
        }
        let status = if turn.interrupted {
            ProviderAdapterV2TurnTerminalStatus::Interrupted
        } else {
            ProviderAdapterV2TurnTerminalStatus::Completed
        };
        self.finish_active(guard, session_id, status, None, OrchestrationV2ProviderThreadDisposition::Reusable);
    }

    fn finish_active(
        &self,
        guard: &mut V1Inner,
        session_id: &str,
        status: ProviderAdapterV2TurnTerminalStatus,
        failure: Option<String>,
        disposition: OrchestrationV2ProviderThreadDisposition,
    ) {
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
            ProviderAdapterV2TurnTerminalStatus::Completed => OrchestrationV2ProviderTurnStatus::Completed,
            ProviderAdapterV2TurnTerminalStatus::Interrupted => OrchestrationV2ProviderTurnStatus::Interrupted,
            ProviderAdapterV2TurnTerminalStatus::Cancelled => OrchestrationV2ProviderTurnStatus::Cancelled,
            ProviderAdapterV2TurnTerminalStatus::Failed => OrchestrationV2ProviderTurnStatus::Failed,
        };
        turn.provider_turn.completed_at = Some(UtcDateTime::now());
        turn.provider_turn.turn_token_usage = Some(
            turn.usage
                .to_turn_usage(status == ProviderAdapterV2TurnTerminalStatus::Completed),
        );
        let provider_turn = turn.provider_turn.clone();
        let run_ordinal = turn.run_ordinal.max(1) as u64;
        let thread_id = turn.thread_id.clone();
        if let Some(stored) = thread.turns.iter_mut().find(|item| item.id == provider_turn.id) {
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

    fn on_part(&self, guard: &mut V1Inner, event_type: &str, props: &Value) {
        let part = if event_type == "message.part.delta" {
            props.clone()
        } else {
            props.get("part").cloned().unwrap_or(Value::Null)
        };
        let session_id = str_field(&part, "sessionID").or_else(|| str_field(props, "sessionID"));
        let Some(session_id) = session_id else { return };
        let Some(thread) = guard.threads.get_mut(&session_id) else {
            return;
        };
        let Some(turn) = thread.active.as_mut() else { return };
        let part_type = part.get("type").and_then(Value::as_str).unwrap_or("");
        let part_id = str_field(&part, "id")
            .or_else(|| str_field(props, "partID"))
            .unwrap_or_else(|| "part".to_owned());
        if part_type == "step-finish" || part.get("tokens").is_some() {
            if let Some(tokens) = parse_step_tokens(&part) {
                turn.usage.add_step(&part_id, tokens);
            }
            return;
        }
        if event_type == "message.part.delta" {
            let delta = part.get("delta").and_then(Value::as_str).unwrap_or("");
            let field = part.get("field").and_then(Value::as_str).unwrap_or("text");
            if field == "text" || field == "reasoning" {
                let text = {
                    let slot = turn.texts.entry(part_id.clone()).or_default();
                    slot.push_str(delta);
                    slot.clone()
                };
                let item = text_item(turn, &self.driver, &part_id, &text, field == "reasoning", true);
                self.emit(ProviderAdapterV2Event::TurnItemUpdated {
                    driver: self.driver.clone(),
                    turn_item: item,
                });
            }
            return;
        }
        if part_type == "text" || part_type == "reasoning" {
            let text = part.get("text").and_then(Value::as_str).unwrap_or("").to_owned();
            turn.texts.insert(part_id.clone(), text.clone());
            let item = text_item(turn, &self.driver, &part_id, &text, part_type == "reasoning", false);
            self.emit(ProviderAdapterV2Event::TurnItemUpdated {
                driver: self.driver.clone(),
                turn_item: item,
            });
            return;
        }
        if part_type == "tool" {
            let name = part
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_owned();
            let state = part.get("state").cloned().unwrap_or(Value::Null);
            let status_name = state.get("status").and_then(Value::as_str).unwrap_or("running");
            let status = match status_name {
                "pending" => OrchestrationV2TurnItemStatus::Pending,
                "completed" => OrchestrationV2TurnItemStatus::Completed,
                "error" => OrchestrationV2TurnItemStatus::Failed,
                _ => OrchestrationV2TurnItemStatus::Running,
            };
            let input = state.get("input").cloned().unwrap_or(json!({}));
            let output = state
                .get("output")
                .or_else(|| state.get("error"))
                .and_then(Value::as_str);
            let call_id = str_field(&part, "callID").unwrap_or(part_id);
            if name == "task" {
                turn.usage.has_subagents = true;
                let child = state
                    .pointer("/metadata/sessionId")
                    .or_else(|| state.pointer("/metadata/sessionID"))
                    .and_then(Value::as_str);
                if let Some(child) = child {
                    guard.children.insert(child.to_owned(), session_id);
                }
            }
            let base = tool_base(turn, &self.driver, &call_id, status);
            let item = open_code_tool_turn_item(
                base,
                &name,
                &input,
                output,
                state.get("metadata"),
            );
            self.emit(ProviderAdapterV2Event::TurnItemUpdated {
                driver: self.driver.clone(),
                turn_item: item,
            });
        }
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
        let Some((provider_turn_id, thread_id)) = guard.threads.get(&owner).and_then(|thread| {
            thread.active.as_ref().map(|turn| {
                (turn.provider_turn.id.clone(), turn.thread_id.clone())
            })
        }) else {
            return;
        };
        let request_id = IdAllocator.runtime_request(
            &self.driver,
            Some(&provider_turn_id),
            Some(&native_id),
        );
        let kind = if event_type == "question.asked" {
            "user_input"
        } else {
            open_code_permission_request_kind(
                props.get("permission").and_then(Value::as_str).unwrap_or("bash"),
                props.get("tool").and_then(|tool| tool.get("name")).and_then(Value::as_str),
            )
        };
        let now = UtcDateTime::now();
        let request = OrchestrationV2RuntimeRequest {
            id: request_id.clone(),
            node_id: derive::node_from_provider_item(&self.driver, &native_id),
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
        if let Some(thread) = guard.threads.get_mut(&owner) {
            thread.requests.push(request.clone());
        }
        guard.pending.insert(
            native_id.clone(),
            Pending {
                request_id: request_id.clone(),
                native_id,
                session_id,
                kind,
                question: props.get("questions").cloned(),
            },
        );
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
        let owner = guard
            .children
            .get(&pending.session_id)
            .cloned()
            .unwrap_or(pending.session_id);
        let Some(thread) = guard.threads.get_mut(&owner) else {
            return;
        };
        if let Some(request) = thread
            .requests
            .iter_mut()
            .find(|request| request.id == pending.request_id)
        {
            request.status = status;
            request.resolved_at = Some(UtcDateTime::now());
            let thread_id = thread.active.as_ref().map(|turn| turn.thread_id.clone());
            self.emit(ProviderAdapterV2Event::RuntimeRequestUpdated {
                driver: self.driver.clone(),
                thread_id,
                runtime_request: request.clone(),
            });
        }
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
            .ok_or_else(|| ProviderAdapterV2Error::protocol("OpenCode session.create returned no id"))?
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
        self.inner
            .lock()
            .threads
            .insert(native_id, V1Thread {
                provider_thread: provider_thread.clone(),
                active: None,
                turns: Vec::new(),
                requests: Vec::new(),
            });
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
        self.client.get_session(&native_id).await.map_err(http_err)?;
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
            },
        );
        Ok(thread)
    }

    async fn start_turn(&self, input: ProviderAdapterV2TurnInput) -> Result<(), ProviderAdapterV2Error> {
        let session_id = {
            let guard = self.inner.lock();
            if guard.stream_dead.is_some() {
                return Err(ProviderAdapterV2Error::protocol(
                    "OpenCode event stream has ended; reconnect the provider session before starting another turn.",
                ));
            }
            let session_id = Self::require_session(&guard, &input.provider_thread)?;
            if guard.threads.get(&session_id).and_then(|thread| thread.active.as_ref()).is_some() {
                return Err(ProviderAdapterV2Error::protocol(format!(
                    "OpenCode provider thread {} already has an active turn",
                    input.provider_thread.id
                )));
            }
            session_id
        };
        let parsed = parse_open_code_model_slug(input.model_selection.model.as_str())
            .ok_or_else(|| ProviderAdapterV2Error::protocol(malformed_model(input.model_selection.model.as_str())))?;
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
                ProviderAdapterV2Error::protocol(format!("OpenCode session {session_id} is not registered"))
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
                next_ordinal: input.provider_turn_ordinal.max(0) as u64 * 100 + 1,
                texts: HashMap::new(),
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
            self.submit_prompt(&session_id, &input, &parsed, &text).await
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
            if let Some(turn) = guard.threads.get_mut(&session_id).and_then(|thread| thread.active.as_mut()) {
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

    async fn steer_turn(&self, input: ProviderAdapterV2SteerInput) -> Result<(), ProviderAdapterV2Error> {
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

    async fn interrupt_turn(&self, input: ProviderAdapterV2InterruptInput) -> Result<(), ProviderAdapterV2Error> {
        let session_id = {
            let guard = self.inner.lock();
            let session_id = Self::require_session(&guard, &input.provider_thread)?.to_owned();
            if let Some(turn) = guard.threads.get(&session_id).and_then(|thread| thread.active.as_ref()) {
                if turn.provider_turn.id != input.provider_turn_id {
                    return Ok(());
                }
            }
            session_id
        };
        {
            let mut guard = self.inner.lock();
            if let Some(turn) = guard.threads.get_mut(&session_id).and_then(|thread| thread.active.as_mut()) {
                turn.interrupted = true;
                cancel_open_code_prompt_admission(&mut turn.admission);
            }
        }
        self.client.abort_session(&session_id).await.map_err(http_err)?;
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
                .map(|pending| (pending.native_id.clone(), pending.kind, pending.question.clone()))
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
        let native = self.client.get_session(&session_id).await.map_err(http_err)?;
        let messages = self.client.session_messages(&session_id).await.map_err(http_err)?;
        let revert = native.pointer("/revert/messageID").and_then(Value::as_str);
        let guard = self.inner.lock();
        let stored = guard.threads.get(&session_id);
        Ok(snapshot_from(
            input.provider_thread,
            &messages,
            revert,
            stored.map(|thread| thread.turns.clone()).unwrap_or_default(),
            stored.map(|thread| thread.requests.clone()).unwrap_or_default(),
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
            if guard.threads.get(&session_id).and_then(|thread| thread.active.as_ref()).is_some() {
                return Err(ProviderAdapterV2Error::protocol(format!(
                    "Cannot roll back OpenCode thread {} while a turn is active",
                    input.provider_thread.id
                )));
            }
        }
        let messages = self.client.session_messages(&session_id).await.map_err(http_err)?;
        let boundary = match &input.target {
            ProviderAdapterV2RollbackTarget::ThreadStart { .. } => messages.iter().find_map(|entry| {
                (entry.pointer("/info/role").and_then(Value::as_str) == Some("user"))
                    .then(|| entry.pointer("/info/id").and_then(Value::as_str).map(str::to_owned))
                    .flatten()
            }),
            ProviderAdapterV2RollbackTarget::ProviderTurn { provider_turn, .. } => {
                open_code_boundary_after_provider_turn(&input.provider_thread_turns, &provider_turn.id)
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
            open_code_boundary_after_provider_turn(input.source_provider_turns.as_deref().unwrap_or(&[]), turn_id)
        });
        let body = match boundary {
            Some(boundary) => json!({ "messageID": boundary }),
            None => json!({}),
        };
        let forked = self.client.fork_session(&session_id, &body).await.map_err(http_err)?;
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
            if commands.iter().any(|entry| entry.get("name").and_then(Value::as_str) == Some(command.name.as_str()))
            {
                let mut body = json!({
                    "command": command.name,
                    "arguments": command.text,
                    "model": format!("{}/{}", parsed.provider_id, parsed.model_id),
                });
                let agent = agent_for(input.runtime_policy.interaction_mode, &input.model_selection);
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
        let agent = agent_for(input.runtime_policy.interaction_mode, &input.model_selection);
        if input.runtime_policy.interaction_mode
            == turin_contracts::provider_policy::ProviderInteractionMode::Plan
            || model_option(&input.model_selection, "agent").is_some()
        {
            body["agent"] = json!(agent);
        }
        self.client.prompt_async(session_id, &body).await.map_err(http_err)
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

fn tool_base(turn: &mut V1Turn, driver: &ProviderDriverKind, native_id: &str, status: OrchestrationV2TurnItemStatus) -> ToolItemBase {
    let ordinal = turn.next_ordinal;
    turn.next_ordinal += 1;
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
        started_at: Some(UtcDateTime::now()),
        completed_at: matches!(
            status,
            OrchestrationV2TurnItemStatus::Completed | OrchestrationV2TurnItemStatus::Failed
        )
        .then(UtcDateTime::now),
        updated_at: UtcDateTime::now(),
    }
}

fn text_item(
    turn: &mut V1Turn,
    driver: &ProviderDriverKind,
    part_id: &str,
    text: &str,
    reasoning: bool,
    streaming: bool,
) -> turin_contracts::orchestration_v2::OrchestrationV2TurnItem {
    let ordinal = turn.next_ordinal;
    turn.next_ordinal += 1;
    let now = UtcDateTime::now();
    let id = derive::turn_item_from_provider_item(driver, part_id);
    if reasoning {
        turin_contracts::orchestration_v2::OrchestrationV2TurnItem::Reasoning {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id,
            thread_id: turn.thread_id.clone(),
            run_id: None,
            node_id: None,
            provider_thread_id: Some(turn.provider_turn.provider_thread_id.clone()),
            provider_turn_id: Some(turn.provider_turn.id.clone()),
            native_item_ref: Some(native_ref(part_id, false)),
            parent_item_id: None,
            ordinal,
            status: OrchestrationV2TurnItemStatus::Running,
            title: None,
            started_at: Some(now),
            completed_at: None,
            updated_at: now,
            text: text.to_owned(),
            streaming,
        }
    } else {
        turin_contracts::orchestration_v2::OrchestrationV2TurnItem::AssistantMessage {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id,
            thread_id: turn.thread_id.clone(),
            run_id: None,
            node_id: None,
            provider_thread_id: Some(turn.provider_turn.provider_thread_id.clone()),
            provider_turn_id: Some(turn.provider_turn.id.clone()),
            native_item_ref: Some(native_ref(part_id, false)),
            parent_item_id: None,
            ordinal,
            status: OrchestrationV2TurnItemStatus::Running,
            title: None,
            started_at: Some(now),
            completed_at: None,
            updated_at: now,
            message_id: derive::message_from_provider_item(driver, part_id),
            text: text.to_owned(),
            attachments: None,
            streaming,
        }
    }
}

fn subagent_for(
    instance_id: &ProviderInstanceId,
    thread: &OrchestrationV2ProviderThread,
    thread_id: &ThreadId,
    parent_node: Option<turin_contracts::NodeId>,
    child_session: &str,
    turn: &OrchestrationV2ProviderTurn,
) -> OrchestrationV2Subagent {
    let now = UtcDateTime::now();
    OrchestrationV2Subagent {
        id: parent_node.unwrap_or_else(|| derive::node_from_provider_item(&opencode_driver(), child_session)),
        thread_id: thread_id.clone(),
        run_id: None,
        parent_node_id: turn.node_id.clone(),
        origin: OrchestrationV2SubagentOrigin::ProviderNative,
        created_by: OrchestrationV2Actor::Agent,
        driver: opencode_driver(),
        provider_instance_id: instance_id.clone(),
        provider_thread_id: Some(thread.id.clone()),
        child_thread_id: None,
        native_task_ref: Some(native_ref(child_session, false)),
        prompt: String::new(),
        title: None,
        model: None,
        model_selection: None,
        completion_wake: None,
        completion_delivery: None,
        status: OrchestrationV2SubagentStatus::Running,
        progress: None,
        workflow: None,
        result: None,
        started_at: Some(now),
        completed_at: None,
        updated_at: now,
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
                Value::Array(items) => items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect(),
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
            let prompt = question.get("question").and_then(Value::as_str).unwrap_or("");
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

fn open_code_question_id(index: usize, header: &str) -> String {
    let header = header
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
        .collect::<String>();
    if header.is_empty() {
        format!("question-{index}")
    } else {
        format!("question-{index}-{header}")
    }
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
        let id = entry.pointer("/info/id").and_then(Value::as_str).unwrap_or("");
        if Some(id) == revert {
            break;
        }
        let role = entry.pointer("/info/role").and_then(Value::as_str).unwrap_or("assistant");
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
