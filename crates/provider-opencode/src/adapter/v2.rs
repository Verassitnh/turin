//! Port of `upstream/packages/provider-opencode/src/server/v2/adapter.ts`.
//!
//! OpenCode 2 has no per-prompt `system` field. Per-turn instructions are
//! `PUT /api/experimental/session/{id}/instructions/entries/t3-code`. A prompt
//! carries `msg_t3_turn_{session}:{attempt}`. Accept-for-session writes a
//! session grant and replies `once`. A decline posts the declined note, then
//! rejects with [`DECLINED`]. A dropped stream reconnects five times, two
//! seconds apart, then backfills messages, permissions, and forms.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use turin_contracts::orchestration_v2::OrchestrationV2NotificationOutcome;
use turin_contracts::{
    MessageId, NodeId, PositiveInt, ProviderSessionId, ProviderThreadId, ProviderTurnId,
    RuntimeRequestId, ThreadId, TrimmedNonEmptyString, UtcDateTime,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2AppThread, OrchestrationV2ConversationMessage,
        OrchestrationV2ConversationMessageRole, OrchestrationV2CreationSource,
        OrchestrationV2ExecutionNode, OrchestrationV2ExecutionNodeKind,
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2ProviderFailureClass,
        OrchestrationV2ProviderSession, OrchestrationV2ProviderSessionStatus,
        OrchestrationV2ProviderThread, OrchestrationV2ProviderThreadDisposition,
        OrchestrationV2ProviderThreadForkedFrom, OrchestrationV2ProviderThreadStatus,
        OrchestrationV2ProviderTurn, OrchestrationV2ProviderTurnStatus,
        OrchestrationV2ResponseCapability, OrchestrationV2RuntimeRequest,
        OrchestrationV2RuntimeRequestKind, OrchestrationV2RuntimeRequestStatus,
        OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
    },
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
    provider_policy::{
        ProviderApprovalDecision, ProviderInteractionMode, ProviderRequestKind, RuntimeMode,
    },
};
use turin_provider_core::id_allocator::{IdAllocator, derive};
use turin_provider_core::notification::{
    BackgroundWork, BackgroundWorkKind, BackgroundWorkReport, background_work_notification,
};
use turin_provider_core::provider_continuation_requests::{
    DispatchIfCurrent, ProviderContinuationDelivery, ProviderContinuationRequest,
    ProviderContinuationRequests,
};
use turin_provider_core::subagent_projection::{
    SubagentChildThreadInput, SubagentConversationArtifactsInput, make_subagent_child_thread,
    make_subagent_conversation_artifacts, subagent_thread_title,
};

use crate::adapter::api::*;
use crate::adapter::logic::*;
use crate::adapter::records::{
    Compaction, NodeOf, NodeTiming, RequestBody, SubagentCall, TextBlock, assistant_message,
    compaction_turn_item, execution_node, next_ordinal, node_status_of, request_node,
    request_turn_item, root_turn_node, settle_request_item, settle_request_node, subagent_records,
    text_turn_item,
};
use crate::events::V2StreamEvent;
use crate::permission_rules::{SessionPermissionRule, session_permission_rules};
use crate::v2_client::{MessageQuery, V2Client};

const RECONNECT_DELAY: Duration = Duration::from_secs(2);
/// Upstream `INTERRUPT_TIMEOUT`: a Stop does not wait longer than this for one session.
const INTERRUPT_TIMEOUT: Duration = Duration::from_secs(10);
/// Upstream `REQUEST_REPLY_TIMEOUT`: one deadline over an answer and its one retry.
const REQUEST_REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// The answers OpenCode gives for a request it already dropped (upstream `permissionGone`,
/// `formGone`). Such an answer counts as delivered.
const GONE_PERMISSION: [&str; 2] = ["PermissionNotFoundError", "SessionNotFoundError"];
const GONE_FORM: [&str; 3] = [
    "FormNotFoundError",
    "FormAlreadySettledError",
    "SessionNotFoundError",
];
/// The turn failure when a request could not be answered (upstream `abandonRequest`).
const REQUEST_ABANDONED: &str = "OpenCode is waiting on a request T3 Code couldn't answer.";

struct V2Turn {
    provider_turn: OrchestrationV2ProviderTurn,
    thread_id: ThreadId,
    run_ordinal: i64,
    interrupted: bool,
    finalized: bool,
    usage: UsageAccum,
    /// Ordinals are per native item, assigned on first sight, as upstream's `ordinalOf`.
    item_ordinals: HashMap<String, u64>,
    next_item_ordinal: u64,
    /// Text and reasoning blocks still streaming, keyed by their native id.
    text_blocks: HashMap<String, TextBlockState>,
    /// When each tool call was first seen, for its node and item start.
    tool_started: HashMap<String, UtcDateTime>,
    /// The compaction running in this turn: its native id, and when it started.
    compaction: Option<(String, UtcDateTime)>,
    compactions: u64,
    /// The app thread the turn runs for: a child thread's parent.
    app_thread: OrchestrationV2AppThread,
    before: Option<String>,
}

/// A text block being accumulated (upstream `emitText`'s `turn.texts` entry).
struct TextBlockState {
    started_at: UtcDateTime,
    text: String,
}

struct V2Thread {
    provider_thread: OrchestrationV2ProviderThread,
    directory: String,
    agent: String,
    model_key: String,
    runtime_mode: RuntimeMode,
    interaction: String,
    rules: Vec<SessionPermissionRule>,
    grants: Vec<SessionPermissionRule>,
    instructions: Option<String>,
    active: Option<V2Turn>,
    turns: Vec<OrchestrationV2ProviderTurn>,
    requests: Vec<OrchestrationV2RuntimeRequest>,
    unsettled: bool,
}

#[derive(Clone)]
enum Ask {
    Permission {
        action: String,
        resources: Vec<String>,
        save: Vec<String>,
    },
    Form(NativeForm),
}

#[derive(Clone)]
struct Pending {
    request_id: RuntimeRequestId,
    native_id: String,
    session_id: String,
    ask: Ask,
    /// The node and item shown while the request waits, settled when it ends.
    node: OrchestrationV2ExecutionNode,
    item: OrchestrationV2TurnItem,
    /// A reply is in flight. The entry stays until it lands, so a failed reply can be retried.
    answering: bool,
}

/// A child session a `task` call revealed (upstream's child `ThreadState`): its app thread, its
/// provider thread, the task's prompt, and the turns its executions run.
struct V2Child {
    thread: OrchestrationV2AppThread,
    /// The title the task call gave, which the child's reports are labelled with.
    title: Option<String>,
    provider_thread: OrchestrationV2ProviderThread,
    parent_thread: ThreadId,
    prompt: String,
    /// Executions started so far; upstream numbers the child's turns by it.
    turns: u64,
}

struct V2Inner {
    threads: HashMap<String, V2Thread>,
    /// Child sessions a `task` call revealed, by child session id.
    subagents: HashMap<String, V2Child>,
    /// The turn each child session is running. Its own map, so revealing a grandchild can
    /// borrow the subagents while a child's turn is borrowed.
    child_turns: HashMap<String, V2Turn>,
    pending: HashMap<String, Pending>,
    children: HashMap<String, String>,
    windows: HashMap<String, u64>,
    stream_dead: Option<String>,
    /// The wakes each idle session holds, oldest first: executions OpenCode started on its own
    /// (upstream `state.wakes`). Only the newest, while it runs, parks events.
    wakes: HashMap<String, Vec<Wake>>,
    /// Children a Stop ended. Their reports answer that Stop, so they start no turn.
    stopped_children: HashSet<String>,
    /// Subagent reports waiting in each session's inbox for the next wake.
    reports: HashMap<String, Vec<WakeReport>>,
    wake_seq: u64,
    /// The tool name each call was first seen with. Later events of a call (progress, success)
    /// carry no name, so the name comes from here (upstream's `turn.tools` and `state.calls`).
    tool_names: HashMap<String, String>,
    /// Calls whose launch returned while they run in the background (upstream `call.background`).
    background_calls: HashSet<String>,
    /// Background subagents still running, by child session id. Each stays open until its
    /// report settles it, past the end of the turn that launched it.
    background: HashMap<String, OpenSubagent>,
}

/// A background subagent whose launch returned while it still runs (upstream's background
/// `SubagentCall`). Its item stays running until its report settles it.
struct OpenSubagent {
    /// The session that launched the call. A Stop of this session stops the call.
    parent_session: String,
    /// The child session the report names: the key it is settled by.
    child_session: String,
    thread_id: ThreadId,
    root_node_id: NodeId,
    provider_thread_id: ProviderThreadId,
    provider_turn_id: ProviderTurnId,
    native_item_id: String,
    ordinal: u64,
    prompt: String,
    title: Option<String>,
    started_at: UtcDateTime,
    child: Option<(ThreadId, ProviderThreadId)>,
}

/// A subagent's report waiting in the inbox (upstream's `state.reports`).
struct WakeReport {
    /// The inbox item the report arrived as: a delivery or cancel removes it.
    inbox_id: String,
    /// The child session that sent it.
    child_session: String,
    text: String,
    label: Option<String>,
    child_thread: Option<ThreadId>,
    outcome: OrchestrationV2NotificationOutcome,
}

/// A turn OpenCode started on its own while the thread had none (upstream `Wake`). The
/// continuation T3 is offered for it starts from `message_id`, and the events that arrived
/// while it waited are parked here and replayed into that continuation's turn.
struct Wake {
    message_id: MessageId,
    /// Parked in arrival order.
    events: Vec<(String, Value)>,
    /// The execution is still running, so its events park here (upstream `running`). Cleared
    /// when the execution ends.
    running: bool,
    /// Set when a Stop or a later continuation discards the wake. Its offer is then not
    /// dispatched (upstream `dropped`).
    dropped: Arc<AtomicBool>,
}

pub struct OpenCodeV2Session {
    instance_id: ProviderInstanceId,
    driver: ProviderDriverKind,
    provider_session_id: ProviderSessionId,
    /// Open-time session record. Later status changes are
    /// `provider_session.updated` events.
    session: OrchestrationV2ProviderSession,
    client: V2Client,
    inner: Mutex<V2Inner>,
    /// Where wake continuations are offered. Unset, offers are dropped.
    continuation: Mutex<Arc<ProviderContinuationRequests>>,
    tx: mpsc::UnboundedSender<Result<ProviderAdapterV2Event, ProviderAdapterV2Error>>,
    rx: Mutex<
        Option<mpsc::UnboundedReceiver<Result<ProviderAdapterV2Event, ProviderAdapterV2Error>>>,
    >,
    reader: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl OpenCodeV2Session {
    pub fn open(
        instance_id: ProviderInstanceId,
        provider_session_id: ProviderSessionId,
        cwd: &str,
        client: V2Client,
        model: Option<&str>,
    ) -> Result<Arc<Self>, ProviderAdapterV2Error> {
        let cwd = turin_contracts::TrimmedNonEmptyString::new(cwd)
            .map_err(|_| ProviderAdapterV2Error::protocol("OpenCode session cwd is blank"))?;
        let now = UtcDateTime::now();
        let (tx, rx) = mpsc::unbounded_channel();
        let provider_session = OrchestrationV2ProviderSession {
            id: provider_session_id.clone(),
            driver: opencode_driver(),
            provider_instance_id: instance_id.clone(),
            status: OrchestrationV2ProviderSessionStatus::Ready,
            cwd,
            model: model.and_then(|model| turin_contracts::TrimmedNonEmptyString::new(model).ok()),
            capabilities: v2_capabilities(),
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
            inner: Mutex::new(V2Inner {
                threads: HashMap::new(),
                pending: HashMap::new(),
                children: HashMap::new(),
                windows: HashMap::new(),
                subagents: HashMap::new(),
                child_turns: HashMap::new(),
                stream_dead: None,
                wakes: HashMap::new(),
                reports: HashMap::new(),
                wake_seq: 0,
                tool_names: HashMap::new(),
                background_calls: HashSet::new(),
                background: HashMap::new(),
                stopped_children: HashSet::new(),
            }),
            continuation: Mutex::new(Arc::new(ProviderContinuationRequests::dropping())),
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
        let mut failures = 0u32;
        loop {
            let stream = match self.client.events().await {
                Ok(stream) => stream,
                Err(err) => {
                    failures += 1;
                    if failures >= RECONNECT_ATTEMPTS {
                        self.fail_stream(&err.detail).await;
                        return;
                    }
                    tokio::time::sleep(RECONNECT_DELAY).await;
                    continue;
                }
            };
            if failures > 0 {
                self.reconcile().await;
            }
            failures = 0;
            let mut stream = stream;
            while let Some(Ok(event)) = stream.next().await {
                self.on_stream(event).await;
            }
            failures += 1;
            if failures >= RECONNECT_ATTEMPTS {
                self.reconcile().await;
                self.fail_stream("The OpenCode event stream was lost and could not reconnect.")
                    .await;
                return;
            }
            tokio::time::sleep(RECONNECT_DELAY).await;
        }
    }

    async fn reconcile(&self) {
        let active = self.client.session_active().await.unwrap_or_default();
        let sessions: Vec<String> = self.inner.lock().threads.keys().cloned().collect();
        for session_id in sessions {
            let prompt = {
                let guard = self.inner.lock();
                guard.threads.get(&session_id).and_then(|thread| {
                    thread.active.as_ref().and_then(|turn| {
                        prompt_of(&turn.provider_turn)
                            .map(str::to_owned)
                            .or(turn.before.clone())
                    })
                })
            };
            if let Some(start) = prompt.as_deref()
                && let Ok(page) = self
                    .client
                    .list_messages(&MessageQuery {
                        session_id: session_id.clone(),
                        limit: Some(50),
                        order: Some("desc".to_owned()),
                        cursor: None,
                        message_type: None,
                    })
                    .await
            {
                self.project_backfill(&session_id, start, &page.data);
            }
            let still = active.contains_key(&session_id);
            if !still {
                let mut guard = self.inner.lock();
                let outcome = guard
                    .threads
                    .get(&session_id)
                    .and_then(|thread| thread.active.as_ref().map(|turn| turn.interrupted));
                if outcome == Some(true) {
                    self.finish(
                        &mut guard,
                        &session_id,
                        ProviderAdapterV2TurnTerminalStatus::Interrupted,
                        None,
                    );
                } else if outcome == Some(false) {
                    self.finish(
                        &mut guard,
                        &session_id,
                        ProviderAdapterV2TurnTerminalStatus::Interrupted,
                        Some(
                            "OpenCode did not report an idle item for the lost execution."
                                .to_owned(),
                        ),
                    );
                }
            }
            if still {
                if let Ok(permissions) = self.client.list_permissions(&session_id).await {
                    for request in permissions {
                        self.show_permission(&request);
                    }
                }
                if let Ok(forms) = self.client.list_forms(&session_id).await {
                    for form in forms {
                        self.show_form(&form);
                    }
                }
            }
        }
    }

    fn project_backfill(&self, session_id: &str, start: &str, messages: &[Value]) {
        let mut recent = Vec::new();
        let mut found = false;
        for message in messages {
            let id = message.get("id").and_then(Value::as_str).unwrap_or("");
            if id == start {
                found = true;
                break;
            }
            recent.push(message);
        }
        if !found && !messages.is_empty() {
            return;
        }
        let mut idle_outcome = None;
        for message in recent.iter().rev() {
            if message.get("type").and_then(Value::as_str) == Some("idle") {
                idle_outcome = message
                    .get("outcome")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            if message.get("type").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            let content = message.get("content").and_then(Value::as_array);
            let Some(content) = content else { continue };
            for part in content {
                let part_type = part.get("type").and_then(Value::as_str).unwrap_or("");
                if part_type == "text" || part_type == "reasoning" {
                    let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                    let id = part.get("id").and_then(Value::as_str).unwrap_or("part");
                    self.emit_settled_text(session_id, id, text, part_type == "reasoning");
                } else if part_type == "tool" {
                    let id = part.get("id").and_then(Value::as_str).unwrap_or("tool");
                    let name = part.get("name").and_then(Value::as_str).unwrap_or("tool");
                    let state = part.get("state").cloned().unwrap_or(Value::Null);
                    self.emit_tool(session_id, "", id, name, &state);
                }
            }
        }
        if let Some(outcome) = idle_outcome {
            let mut guard = self.inner.lock();
            let status = match outcome.as_str() {
                "succeeded" => ProviderAdapterV2TurnTerminalStatus::Completed,
                "failed" => ProviderAdapterV2TurnTerminalStatus::Failed,
                _ => ProviderAdapterV2TurnTerminalStatus::Interrupted,
            };
            self.finish(&mut guard, session_id, status, None);
        }
    }

    async fn fail_stream(&self, message: &str) {
        let mut guard = self.inner.lock();
        guard.stream_dead = Some(message.to_owned());
        let mut session = self.session.clone();
        session.status = OrchestrationV2ProviderSessionStatus::Error;
        session.last_error = Some(message.to_owned());
        session.updated_at = UtcDateTime::now();
        let ids: Vec<String> = guard.threads.keys().cloned().collect();
        let mut terminals = Vec::new();
        for id in ids {
            if guard
                .threads
                .get(&id)
                .and_then(|thread| thread.active.as_ref())
                .is_some()
            {
                terminals.push(id);
            }
        }
        drop(guard);
        self.emit(ProviderAdapterV2Event::ProviderSessionUpdated {
            driver: self.driver.clone(),
            provider_session: session,
        });
        for id in terminals {
            let mut guard = self.inner.lock();
            self.finish(
                &mut guard,
                &id,
                ProviderAdapterV2TurnTerminalStatus::Failed,
                Some(message.to_owned()),
            );
        }
    }

    fn emit(&self, event: ProviderAdapterV2Event) {
        let _ = self.tx.send(Ok(event));
    }

    async fn on_stream(&self, event: V2StreamEvent) {
        match event {
            V2StreamEvent::UnreadableExecutionEnded {
                session_id,
                execution_type,
            } => {
                let mut guard = self.inner.lock();
                let status = if execution_type.contains("interrupt") {
                    ProviderAdapterV2TurnTerminalStatus::Interrupted
                } else if execution_type.contains("fail") {
                    ProviderAdapterV2TurnTerminalStatus::Failed
                } else {
                    ProviderAdapterV2TurnTerminalStatus::Completed
                };
                self.finish(&mut guard, &session_id, status, None);
            }
            V2StreamEvent::UnreadableExecutionStarted { .. } => {}
            V2StreamEvent::Event(event) => self.on_event(&event.event_type, &event.data),
        }
    }

    fn on_event(&self, event_type: &str, data: &Value) {
        let session_id = str_field(data, "sessionID");
        if let Some(session_id) = &session_id
            && self.child_execution(session_id, event_type)
        {
            return;
        }
        if let Some(session_id) = &session_id
            && self.park_or_wake(session_id, event_type, data)
        {
            return;
        }
        match event_type {
            "session.inbox.enqueued" => {
                if let Some(session_id) = &session_id {
                    self.collect_report(session_id, data);
                }
            }
            "session.inbox.delivered" | "session.inbox.cancelled" => {
                // Delivered into a turn already running, or cancelled: no wake reports it.
                if let (Some(session_id), Some(inbox_id)) =
                    (&session_id, str_field(data, "inboxID"))
                {
                    let mut guard = self.inner.lock();
                    if let Some(reports) = guard.reports.get_mut(session_id) {
                        reports.retain(|report| report.inbox_id != inbox_id);
                    }
                }
            }
            "session.created" => {
                let parent = str_field(data, "parentID");
                let id = str_field(data, "id");
                if let (Some(id), Some(parent)) = (id, parent) {
                    self.link_child(&id, &parent);
                }
            }
            "session.execution.succeeded" => {
                if let Some(session_id) = session_id {
                    let mut guard = self.inner.lock();
                    let interrupted = guard
                        .threads
                        .get(&session_id)
                        .and_then(|thread| thread.active.as_ref())
                        .is_some_and(|turn| turn.interrupted);
                    let status = if interrupted {
                        ProviderAdapterV2TurnTerminalStatus::Interrupted
                    } else {
                        ProviderAdapterV2TurnTerminalStatus::Completed
                    };
                    self.finish(&mut guard, &session_id, status, None);
                }
            }
            "session.execution.failed" => {
                if let Some(session_id) = session_id {
                    let mut guard = self.inner.lock();
                    self.finish(
                        &mut guard,
                        &session_id,
                        ProviderAdapterV2TurnTerminalStatus::Failed,
                        data.get("error")
                            .and_then(|error| error.get("message"))
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    );
                }
            }
            "session.execution.interrupted" => {
                if let Some(session_id) = session_id {
                    let mut guard = self.inner.lock();
                    self.finish(
                        &mut guard,
                        &session_id,
                        ProviderAdapterV2TurnTerminalStatus::Interrupted,
                        None,
                    );
                }
            }
            "session.step.ended" => {
                if let Some(session_id) = session_id
                    && let Some(tokens) = parse_step_tokens(data)
                {
                    let mut guard = self.inner.lock();
                    if let Some(turn) = guard
                        .threads
                        .get_mut(&session_id)
                        .and_then(|thread| thread.active.as_mut())
                    {
                        let id = str_field(data, "id").unwrap_or_else(|| "step".to_owned());
                        turn.usage.add_step(&id, tokens);
                    }
                }
            }
            "session.text.started"
            | "session.reasoning.started"
            | "session.text.delta"
            | "session.reasoning.delta"
            | "session.text.ended"
            | "session.reasoning.ended" => {
                if let Some(session_id) = session_id {
                    self.on_text(&session_id, event_type, data);
                }
            }
            "session.compaction.started"
            | "session.compaction.ended"
            | "session.compaction.failed" => {
                if let Some(session_id) = session_id {
                    self.compaction(&session_id, event_type, data);
                }
            }
            "session.tool.input.started"
            | "session.tool.called"
            | "session.tool.progress"
            | "session.tool.success"
            | "session.tool.failed" => {
                if event_type.contains("question") {
                    return;
                }
                if let Some(session_id) = session_id {
                    let id = str_field(data, "id").or_else(|| str_field(data, "callID"));
                    // Only the call's first events name the tool; the rest are looked up.
                    let name = {
                        let mut guard = self.inner.lock();
                        match data
                            .get("name")
                            .or_else(|| data.get("tool"))
                            .and_then(Value::as_str)
                        {
                            Some(name) => {
                                if let Some(id) = &id {
                                    guard.tool_names.insert(id.clone(), name.to_owned());
                                }
                                name.to_owned()
                            }
                            None => id
                                .as_ref()
                                .and_then(|id| guard.tool_names.get(id).cloned())
                                .unwrap_or_else(|| "tool".to_owned()),
                        }
                    };
                    let name = name.as_str();
                    if (name == "subagent" || name == "task")
                        && let Some(child) = data
                            .pointer("/state/metadata/sessionID")
                            .or_else(|| data.get("sessionID"))
                            .and_then(Value::as_str)
                        && child != session_id
                    {
                        self.link_child(child, &session_id);
                    }
                    let id = id.unwrap_or_else(|| name.to_owned());
                    self.emit_tool(&session_id, event_type, &id, name, data);
                }
            }
            "permission.asked" => self.show_permission(data),
            "form.created" => {
                let form = data.get("form").cloned().unwrap_or_else(|| data.clone());
                self.show_form(&form);
            }
            _ => {}
        }
    }

    /// Ownership only (upstream `relatedSessionOwners`): the subagent record comes from the
    /// task call that names the child, so the link and the task key one record.
    fn link_child(&self, child: &str, parent: &str) {
        let mut guard = self.inner.lock();
        if guard.threads.contains_key(parent) {
            guard.children.insert(child.to_owned(), parent.to_owned());
        }
    }

    /// `emitText` for a v2 stream event. The block's native id is
    /// `${assistantMessageID}:${kind}:${ordinal}`. Deltas accumulate; the `text` field
    /// of an ended event is the final text, so `completed` is "text" in data.
    fn on_text(&self, session_id: &str, event_type: &str, data: &Value) {
        let reasoning = event_type.contains("reasoning");
        let kind = if reasoning { "reasoning" } else { "text" };
        let Some(message_id) = str_field(data, "assistantMessageID") else {
            return;
        };
        let ordinal = data.get("ordinal").and_then(Value::as_u64).unwrap_or(0);
        let native = format!("{message_id}:{kind}:{ordinal}");
        let now = UtcDateTime::now();
        let completed = data.get("text").is_some();
        let mut events = Vec::new();
        {
            let mut guard = self.inner.lock();
            let Some(turn) = active_turn_mut(&mut guard, session_id) else {
                return;
            };
            let block = turn
                .text_blocks
                .entry(native.clone())
                .or_insert_with(|| TextBlockState {
                    started_at: now,
                    text: String::new(),
                });
            if let Some(delta) = data.get("delta").and_then(Value::as_str) {
                block.text.push_str(delta);
            } else if let Some(text) = data.get("text").and_then(Value::as_str) {
                block.text = text.to_owned();
            }
            let text = block.text.clone();
            let started_at = block.started_at;
            if completed {
                turn.text_blocks.remove(&native);
            }
            if !text.is_empty() {
                self.block_events(
                    turn,
                    &native,
                    reasoning,
                    &text,
                    completed,
                    started_at,
                    now,
                    &mut events,
                );
            }
        }
        for event in events {
            self.emit(event);
        }
    }

    /// A text block already settled in OpenCode's history: it is emitted once, completed.
    fn emit_settled_text(&self, session_id: &str, part_id: &str, text: &str, reasoning: bool) {
        let now = UtcDateTime::now();
        let mut events = Vec::new();
        {
            let mut guard = self.inner.lock();
            let Some(turn) = active_turn_mut(&mut guard, session_id) else {
                return;
            };
            self.block_events(turn, part_id, reasoning, text, true, now, now, &mut events);
        }
        for event in events {
            self.emit(event);
        }
    }

    /// The node, the assistant message (text only), and the item for one block.
    #[allow(clippy::too_many_arguments)]
    fn block_events(
        &self,
        turn: &mut V2Turn,
        native: &str,
        reasoning: bool,
        text: &str,
        completed: bool,
        started_at: UtcDateTime,
        now: UtcDateTime,
        events: &mut Vec<ProviderAdapterV2Event>,
    ) {
        let thread_id = turn.thread_id.clone();
        let root_node_id = turn.provider_turn.node_id.clone();
        let provider_thread_id = turn.provider_turn.provider_thread_id.clone();
        let provider_turn_id = turn.provider_turn.id.clone();
        let of = NodeOf {
            thread_id: &thread_id,
            root_node_id: &root_node_id,
            provider_thread_id: &provider_thread_id,
            provider_turn_id: &provider_turn_id,
            native_item_id: native,
            weak_ref: false,
        };
        let completed_at = completed.then_some(now);
        let node = execution_node(
            &self.driver,
            &of,
            NodeTiming {
                kind: if reasoning {
                    OrchestrationV2ExecutionNodeKind::Reasoning
                } else {
                    OrchestrationV2ExecutionNodeKind::AssistantMessage
                },
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
        events.push(ProviderAdapterV2Event::NodeUpdated {
            driver: self.driver.clone(),
            node,
        });
        if !reasoning {
            events.push(ProviderAdapterV2Event::MessageUpdated {
                driver: self.driver.clone(),
                message: assistant_message(
                    &self.driver,
                    &of,
                    &node_id,
                    text,
                    !completed,
                    started_at,
                    now,
                ),
            });
        }
        let ordinal = item_ordinal(turn, native);
        events.push(ProviderAdapterV2Event::TurnItemUpdated {
            driver: self.driver.clone(),
            turn_item: text_turn_item(
                &self.driver,
                &of,
                &node_id,
                TextBlock {
                    reasoning,
                    text,
                    streaming: !completed,
                    status: if completed {
                        OrchestrationV2TurnItemStatus::Completed
                    } else {
                        OrchestrationV2TurnItemStatus::Running
                    },
                    ordinal,
                    started_at,
                    completed_at,
                    updated_at: now,
                },
            ),
        });
    }

    /// One tool event. OpenCode 2 puts a call's status in `metadata.status` (a success carries
    /// `completed`, a background launch `running`), so that is read too. Without a status, a
    /// success is completed and a failure failed, as upstream's `session.tool.success` settles.
    fn emit_tool(
        &self,
        session_id: &str,
        event_type: &str,
        call_id: &str,
        name: &str,
        state: &Value,
    ) {
        let mut events = Vec::new();
        {
            let mut guard = self.inner.lock();
            let inner: &mut V2Inner = &mut guard;
            // A thread's own turn, or a child session's turn. A child reveals no grandchild.
            let is_root = inner
                .threads
                .get(session_id)
                .is_some_and(|thread| thread.active.is_some());
            let turn = if is_root {
                inner
                    .threads
                    .get_mut(session_id)
                    .and_then(|thread| thread.active.as_mut())
            } else {
                inner.child_turns.get_mut(session_id)
            };
            let Some(turn) = turn else {
                return;
            };
            let status_name = state
                .get("status")
                .or_else(|| state.pointer("/state/status"))
                .or_else(|| state.pointer("/metadata/status"))
                .or_else(|| state.pointer("/state/metadata/status"))
                .and_then(Value::as_str);
            let status = match status_name {
                Some("completed" | "success") => OrchestrationV2TurnItemStatus::Completed,
                Some("error" | "failed") => OrchestrationV2TurnItemStatus::Failed,
                Some("running") => OrchestrationV2TurnItemStatus::Running,
                _ => match event_type {
                    "session.tool.success" => OrchestrationV2TurnItemStatus::Completed,
                    "session.tool.failed" => OrchestrationV2TurnItemStatus::Failed,
                    _ => OrchestrationV2TurnItemStatus::Running,
                },
            };
            if status != OrchestrationV2TurnItemStatus::Running {
                inner.tool_names.remove(call_id);
            }
            let now = UtcDateTime::now();
            let started_at = *turn.tool_started.entry(call_id.to_owned()).or_insert(now);
            let completed_at = matches!(
                status,
                OrchestrationV2TurnItemStatus::Completed | OrchestrationV2TurnItemStatus::Failed
            )
            .then_some(now);
            let thread_id = turn.thread_id.clone();
            let root_node_id = turn.provider_turn.node_id.clone();
            let provider_thread_id = turn.provider_turn.provider_thread_id.clone();
            let provider_turn_id = turn.provider_turn.id.clone();
            let of = NodeOf {
                thread_id: &thread_id,
                root_node_id: &root_node_id,
                provider_thread_id: &provider_thread_id,
                provider_turn_id: &provider_turn_id,
                native_item_id: call_id,
                weak_ref: false,
            };
            let input = state.get("input").cloned().unwrap_or_else(|| json!({}));
            let output = state
                .get("output")
                .or_else(|| state.pointer("/state/output"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            if name == "task" || name == "subagent" {
                turn.usage.has_subagents = true;
                // `emitSubagent`: a task call is a subagent, not a tool call. The child thread
                // exists from the first update that names the child session.
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
                    .or_else(|| state.pointer("/state/metadata/sessionID"))
                    .and_then(Value::as_str);
                let subagent_node = derive::node_from_provider_item(&self.driver, call_id);
                let mut child = None;
                if let Some(child_session) = child_session.filter(|_| is_root) {
                    inner
                        .children
                        .insert(child_session.to_owned(), session_id.to_owned());
                    self.ensure_child(
                        &mut inner.subagents,
                        turn,
                        child_session,
                        &prompt,
                        title,
                        &subagent_node,
                    );
                    child = inner
                        .subagents
                        .get(child_session)
                        .map(|child| (child.thread.id.clone(), child.provider_thread.id.clone()));
                }
                // A background launch returns while the subagent runs (`input.background`, or a
                // success whose `metadata.status` is `running`). Its item stays open until the
                // report settles it, so only a terminal status ends the call here.
                if input.get("background").and_then(Value::as_bool) == Some(true)
                    || (event_type == "session.tool.success"
                        && state.pointer("/metadata/status").and_then(Value::as_str)
                            == Some("running"))
                {
                    inner.background_calls.insert(call_id.to_owned());
                }
                let background = inner.background_calls.contains(call_id);
                let result = output.as_deref().map(subagent_output);
                let ordinal = item_ordinal(turn, call_id);
                if status == OrchestrationV2TurnItemStatus::Running && background {
                    if let Some(child_session) = child_session.filter(|_| is_root) {
                        inner.background.insert(
                            child_session.to_owned(),
                            OpenSubagent {
                                parent_session: session_id.to_owned(),
                                child_session: child_session.to_owned(),
                                thread_id: thread_id.clone(),
                                root_node_id: root_node_id.clone(),
                                provider_thread_id: provider_thread_id.clone(),
                                provider_turn_id: provider_turn_id.clone(),
                                native_item_id: call_id.to_owned(),
                                ordinal,
                                prompt: prompt.clone(),
                                title: title.map(str::to_owned),
                                started_at,
                                child: child.clone(),
                            },
                        );
                    }
                } else {
                    inner.background_calls.remove(call_id);
                    inner
                        .background
                        .retain(|_, open| open.native_item_id != call_id);
                }
                let records = subagent_records(
                    &self.driver,
                    SubagentCall {
                        instance_id: &self.instance_id,
                        child,
                        of,
                        ordinal,
                        prompt: &prompt,
                        title,
                        status,
                        started_at,
                        completed_at,
                        result,
                        child_session,
                        now,
                    },
                );
                events.push(ProviderAdapterV2Event::NodeUpdated {
                    driver: self.driver.clone(),
                    node: records.node,
                });
                events.push(ProviderAdapterV2Event::SubagentUpdated {
                    driver: self.driver.clone(),
                    subagent: records.subagent,
                });
                events.push(ProviderAdapterV2Event::TurnItemUpdated {
                    driver: self.driver.clone(),
                    turn_item: records.item,
                });
            } else {
                events.push(ProviderAdapterV2Event::NodeUpdated {
                    driver: self.driver.clone(),
                    node: execution_node(
                        &self.driver,
                        &of,
                        NodeTiming {
                            kind: OrchestrationV2ExecutionNodeKind::ToolCall,
                            status: node_status_of(status),
                            started_at,
                            completed_at,
                        },
                    ),
                });
                let shown = output.as_deref().map(str::to_owned);
                let base = tool_base(
                    turn,
                    &self.driver,
                    call_id,
                    status,
                    started_at,
                    completed_at,
                );
                events.push(ProviderAdapterV2Event::TurnItemUpdated {
                    driver: self.driver.clone(),
                    turn_item: open_code_tool_turn_item(
                        base,
                        name,
                        &input,
                        shown.as_deref(),
                        state.get("metadata"),
                    ),
                });
            }
        }
        for event in events {
            self.emit(event);
        }
    }

    fn show_permission(&self, data: &Value) {
        let native_id = str_field(data, "id").unwrap_or_default();
        let session_id = str_field(data, "sessionID").unwrap_or_default();
        let action = str_field(data, "action")
            .or_else(|| str_field(data, "permission"))
            .unwrap_or_else(|| "bash".to_owned());
        let resources = data
            .get("resources")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let save = data
            .get("save")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        self.show_ask(
            &session_id,
            &native_id,
            Ask::Permission {
                action,
                resources,
                save,
            },
            open_code_permission_request_kind(
                data.get("action").and_then(Value::as_str).unwrap_or("bash"),
                None,
            ),
        );
    }

    fn show_form(&self, data: &Value) {
        let Ok(form) = serde_json::from_value::<NativeForm>(data.clone()) else {
            return;
        };
        let session_id = str_field(data, "sessionID").unwrap_or_default();
        let native_id = form.id.clone();
        if form_questions(&form).is_err() {
            let session_id = session_id.clone();
            let form_id = form.id.clone();
            let client = self.client.clone();
            tokio::spawn(async move {
                let _ = client.cancel_form(&session_id, &form_id, None).await;
            });
            return;
        }
        self.show_ask(&session_id, &native_id, Ask::Form(form), "user_input");
    }

    fn show_ask(&self, session_id: &str, native_id: &str, ask: Ask, kind: &str) {
        let mut guard = self.inner.lock();
        if guard.pending.contains_key(native_id) {
            return;
        }
        let owner = guard
            .children
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| session_id.to_owned());
        let now = UtcDateTime::now();
        let (request, item, node, request_id, thread_id) = {
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
                IdAllocator.runtime_request(&self.driver, Some(&provider_turn_id), Some(native_id));
            let ordinal = next_ordinal(
                &mut turn.item_ordinals,
                &mut turn.next_item_ordinal,
                native_id,
            );
            let of = NodeOf {
                thread_id: &thread_id,
                root_node_id: &root_node_id,
                provider_thread_id: &provider_thread_id,
                provider_turn_id: &provider_turn_id,
                native_item_id: native_id,
                weak_ref: false,
            };
            let node = request_node(
                &self.driver,
                &of,
                &request_id,
                matches!(ask, Ask::Form(_)),
                now,
            );
            let body = match &ask {
                Ask::Permission { action, .. } => RequestBody::Approval {
                    request_kind: match kind {
                        "file-read" => ProviderRequestKind::FileRead,
                        "file-change" => ProviderRequestKind::FileChange,
                        _ => ProviderRequestKind::Command,
                    },
                    title: Some(action.clone()),
                },
                Ask::Form(form) => RequestBody::UserInput {
                    questions: form_questions(form).unwrap_or_default(),
                },
            };
            let item = request_turn_item(&of, &request_id, ordinal, now, body);
            let request = OrchestrationV2RuntimeRequest {
                id: request_id.clone(),
                node_id: node.id.clone(),
                provider_turn_id: Some(provider_turn_id),
                native_request_ref: Some(native_ref(native_id, false)),
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
            (request, item, node, request_id, thread_id)
        };
        guard.pending.insert(
            native_id.to_owned(),
            Pending {
                request_id,
                native_id: native_id.to_owned(),
                session_id: session_id.to_owned(),
                ask,
                node: node.clone(),
                item: item.clone(),
                answering: false,
            },
        );
        drop(guard);
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

    /// Where wake continuations are offered. Set it before the first wake; the default drops them.
    pub fn set_continuation_requests(&self, requests: Arc<ProviderContinuationRequests>) {
        *self.continuation.lock() = requests;
    }

    /// The wake (upstream `onWake` and `holderOf`): OpenCode started an execution on its own
    /// while the thread had no turn. The wake is offered once, and the execution's events park
    /// until a turn takes them. Returns true when the event was parked.
    fn park_or_wake(&self, session_id: &str, event_type: &str, data: &Value) -> bool {
        // Inbox items are what a wake delivers, so they never park.
        if event_type.starts_with("session.inbox.") {
            return false;
        }
        let mut guard = self.inner.lock();
        let Some(thread) = guard.threads.get(session_id) else {
            return false;
        };
        if thread.active.is_some() {
            return false;
        }
        let route = thread.provider_thread.app_thread_id.clone();
        let provider_thread_id = thread.provider_thread.id.clone();
        // Only the newest wake holds events, and only while its execution runs.
        if let Some(held) = guard
            .wakes
            .get_mut(session_id)
            .and_then(|wakes| wakes.last_mut())
            && held.running
        {
            held.events.push((event_type.to_owned(), data.clone()));
            if is_execution_end(event_type) {
                held.running = false;
            }
            return true;
        }
        if event_type != "session.execution.started" {
            return false;
        }
        let Some(route) = route else {
            return false;
        };
        // upstream `takeReports`: reports that all come from children a Stop ended answer that
        // Stop alone, so the execution they start is stopped, not woken.
        let reports = guard.reports.remove(session_id).unwrap_or_default();
        let stopped = !reports.is_empty()
            && reports
                .iter()
                .all(|report| guard.stopped_children.contains(&report.child_session));
        for report in &reports {
            guard.stopped_children.remove(&report.child_session);
        }
        if stopped {
            if let Some(thread) = guard.threads.get_mut(session_id) {
                thread.unsettled = true;
            }
            drop(guard);
            let client = self.client.clone();
            let session = session_id.to_owned();
            tokio::spawn(async move {
                let _ =
                    tokio::time::timeout(INTERRUPT_TIMEOUT, client.interrupt(&session, None)).await;
            });
            return false;
        }
        guard.wake_seq += 1;
        let message_id = IdAllocator.message(&route, guard.wake_seq);
        let detail = (!reports.is_empty()).then(|| {
            reports
                .iter()
                .map(|report| report.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
        });
        let background: Vec<BackgroundWorkReport> = reports
            .iter()
            .map(|report| BackgroundWorkReport {
                work: BackgroundWork {
                    label: report.label.clone(),
                    kind: BackgroundWorkKind::Subagent {
                        child_thread_id: report.child_thread.clone(),
                    },
                },
                outcome: report.outcome,
            })
            .collect();
        let dropped = Arc::new(AtomicBool::new(false));
        guard
            .wakes
            .entry(session_id.to_owned())
            .or_default()
            .push(Wake {
                message_id: message_id.clone(),
                events: vec![(event_type.to_owned(), data.clone())],
                running: true,
                dropped: Arc::clone(&dropped),
            });
        drop(guard);
        let requests = Arc::clone(&self.continuation.lock());
        requests.offer(ProviderContinuationRequest {
            thread_id: route,
            provider_thread_id,
            driver: self.driver.clone(),
            detail,
            notification: background_work_notification(&background),
            delegated_completion: None,
            // The adapter parks the CLI's output itself, so the dispatch only ingests it.
            delivery: Some(ProviderContinuationDelivery::AdapterBuffered),
            message_id: Some(message_id),
            dispatch_if_current: Some(wake_dispatch(dropped)),
            clear_if_current: None,
        });
        true
    }

    /// A subagent's report, waiting in the inbox for the wake that delivers it (upstream
    /// `onReport`). Only a report a subagent sent counts.
    fn collect_report(&self, session_id: &str, data: &Value) {
        let Some(item) = data.get("item") else {
            return;
        };
        if item.get("type").and_then(Value::as_str) != Some("synthetic") {
            return;
        }
        let payload = item.get("payload");
        let metadata = |key: &str| {
            payload
                .and_then(|payload| payload.pointer(&format!("/metadata/{key}")))
                .and_then(Value::as_str)
        };
        let Some(child) = metadata("childID") else {
            return;
        };
        if metadata("source") != Some("subagent") {
            return;
        }
        let Some(text) = payload
            .and_then(|payload| payload.get("text"))
            .and_then(Value::as_str)
        else {
            return;
        };
        let outcome = match metadata("state") {
            Some("completed") => OrchestrationV2NotificationOutcome::Completed,
            Some("error") => OrchestrationV2NotificationOutcome::Failed,
            Some("cancelled") => OrchestrationV2NotificationOutcome::Cancelled,
            _ => OrchestrationV2NotificationOutcome::Unknown,
        };
        let mut guard = self.inner.lock();
        // The label is the task's title, else the description OpenCode sent.
        let (label, child_thread) = match guard.subagents.get(child) {
            Some(known) => (
                known
                    .title
                    .clone()
                    .or_else(|| metadata("description").map(str::to_owned)),
                Some(known.thread.id.clone()),
            ),
            None => (metadata("description").map(str::to_owned), None),
        };
        // The report is the end of a background subagent: its item settles with it.
        let settled = guard.background.remove(child);
        if let Some(open) = &settled {
            guard.background_calls.remove(&open.native_item_id);
        }
        guard
            .reports
            .entry(session_id.to_owned())
            .or_default()
            .push(WakeReport {
                inbox_id: str_field(data, "inboxID").unwrap_or_default(),
                child_session: child.to_owned(),
                text: text.to_owned(),
                label,
                child_thread,
                outcome,
            });
        drop(guard);
        if let Some(open) = settled {
            let status = match outcome {
                OrchestrationV2NotificationOutcome::Failed => OrchestrationV2TurnItemStatus::Failed,
                OrchestrationV2NotificationOutcome::Cancelled => {
                    OrchestrationV2TurnItemStatus::Cancelled
                }
                _ => OrchestrationV2TurnItemStatus::Completed,
            };
            for event in self.settle_background(open, status, Some(subagent_output(text))) {
                self.emit(event);
            }
        }
    }

    /// The events that settle a background subagent: its node, record and item, as upstream
    /// `settleCall` does when its report arrives.
    fn settle_background(
        &self,
        open: OpenSubagent,
        status: OrchestrationV2TurnItemStatus,
        result: Option<String>,
    ) -> Vec<ProviderAdapterV2Event> {
        let now = UtcDateTime::now();
        let records = subagent_records(
            &self.driver,
            SubagentCall {
                instance_id: &self.instance_id,
                of: NodeOf {
                    thread_id: &open.thread_id,
                    root_node_id: &open.root_node_id,
                    provider_thread_id: &open.provider_thread_id,
                    provider_turn_id: &open.provider_turn_id,
                    native_item_id: &open.native_item_id,
                    weak_ref: false,
                },
                ordinal: open.ordinal,
                prompt: &open.prompt,
                title: open.title.as_deref(),
                status,
                started_at: open.started_at,
                completed_at: Some(now),
                result,
                child_session: Some(&open.child_session),
                child: open.child,
                now,
            },
        );
        vec![
            ProviderAdapterV2Event::NodeUpdated {
                driver: self.driver.clone(),
                node: records.node,
            },
            ProviderAdapterV2Event::SubagentUpdated {
                driver: self.driver.clone(),
                subagent: records.subagent,
            },
            ProviderAdapterV2Event::TurnItemUpdated {
                driver: self.driver.clone(),
                turn_item: records.item,
            },
        ]
    }

    /// `emitSubagent`'s child thread: created the first time a task reveals its child session.
    fn ensure_child(
        &self,
        subagents: &mut HashMap<String, V2Child>,
        parent: &V2Turn,
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
            V2Child {
                thread,
                title: title.map(str::to_owned),
                provider_thread,
                parent_thread: parent.thread_id.clone(),
                prompt: prompt.to_owned(),
                turns: 0,
            },
        );
    }

    /// A child session's executions are its turns (upstream `startChildTurn` and the end of a
    /// child's execution). Returns true when the event is one of them.
    fn child_execution(&self, session_id: &str, event_type: &str) -> bool {
        let end = match event_type {
            "session.execution.started" => None,
            "session.execution.succeeded" => Some(ProviderAdapterV2TurnTerminalStatus::Completed),
            "session.execution.failed" => Some(ProviderAdapterV2TurnTerminalStatus::Failed),
            "session.execution.interrupted" => {
                Some(ProviderAdapterV2TurnTerminalStatus::Interrupted)
            }
            _ => return false,
        };
        let mut guard = self.inner.lock();
        if !guard.subagents.contains_key(session_id) {
            return false;
        }
        match end {
            None => self.start_child_turn(&mut guard, session_id, UtcDateTime::now()),
            Some(status) => self.finish_child_turn(&mut guard, session_id, status),
        }
        true
    }

    /// `startChildTurn`: an execution of the child opens its turn, under a `root_turn`. The
    /// task's prompt opens the conversation as a user message from the parent thread.
    fn start_child_turn(&self, inner: &mut V2Inner, child_session: &str, now: UtcDateTime) {
        if inner.child_turns.contains_key(child_session) {
            return;
        }
        let Some(child) = inner.subagents.get_mut(child_session) else {
            return;
        };
        child.turns += 1;
        let native_turn = format!("{child_session}:turn:{}", child.turns);
        let root = derive::node_from_provider_item(&self.driver, &format!("{native_turn}:root"));
        let provider_turn_id = derive::provider_turn(&self.driver, &native_turn);
        let provider_turn = OrchestrationV2ProviderTurn {
            id: provider_turn_id.clone(),
            provider_thread_id: child.provider_thread.id.clone(),
            node_id: root.clone(),
            run_attempt_id: None,
            native_turn_ref: Some(native_ref(&native_turn, true)),
            ordinal: PositiveInt::new(child.turns).expect("child turn ordinals start at one"),
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
        let thread_id = child.thread.id.clone();
        let provider_thread_id = child.provider_thread.id.clone();
        let child_app_thread = child.thread.clone();
        let prompt = child.prompt.clone();
        let parent_thread = child.parent_thread.clone();
        inner.child_turns.insert(
            child_session.to_owned(),
            V2Turn {
                provider_turn: provider_turn.clone(),
                thread_id: thread_id.clone(),
                run_ordinal: 0,
                interrupted: false,
                finalized: false,
                usage: UsageAccum::new(),
                item_ordinals: HashMap::new(),
                next_item_ordinal: 1,
                text_blocks: HashMap::new(),
                tool_started: HashMap::new(),
                compaction: None,
                compactions: 0,
                app_thread: child_app_thread,
                before: None,
            },
        );
        self.emit(ProviderAdapterV2Event::NodeUpdated {
            driver: self.driver.clone(),
            node,
        });
        self.emit(ProviderAdapterV2Event::ProviderTurnUpdated {
            driver: self.driver.clone(),
            thread_id: Some(thread_id.clone()),
            provider_turn,
        });
        if prompt.is_empty() {
            return;
        }
        let native_prompt = format!("{native_turn}:prompt");
        let turn = inner
            .child_turns
            .get_mut(child_session)
            .expect("the turn was just installed");
        let ordinal = item_ordinal(turn, &native_prompt);
        let artifacts = make_subagent_conversation_artifacts(SubagentConversationArtifactsInput {
            message_id: derive::message_from_provider_item(&self.driver, &native_prompt),
            sender_thread_id: Some(parent_thread),
            turn_item_id: derive::turn_item_from_provider_item(&self.driver, &native_prompt),
            thread_id,
            root_node_id: root,
            provider_thread_id: Some(provider_thread_id),
            provider_turn_id: Some(provider_turn_id),
            native_item_ref: Some(native_ref(&native_prompt, true)),
            role: OrchestrationV2ConversationMessageRole::User,
            text: prompt,
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

    /// The end of a child's execution: its open text is final, its root node and provider turn
    /// settle with the terminal status (upstream `finishTurn` for a subagent).
    fn finish_child_turn(
        &self,
        inner: &mut V2Inner,
        child_session: &str,
        status: ProviderAdapterV2TurnTerminalStatus,
    ) {
        let Some(mut turn) = inner.child_turns.remove(child_session) else {
            return;
        };
        let Some(child) = inner.subagents.get(child_session) else {
            return;
        };
        let now = UtcDateTime::now();
        let mut events = Vec::new();
        let open: Vec<(String, TextBlockState)> = turn.text_blocks.drain().collect();
        for (native, block) in open {
            let reasoning = native.contains(":reasoning:");
            self.block_events(
                &mut turn,
                &native,
                reasoning,
                &block.text,
                true,
                block.started_at,
                now,
                &mut events,
            );
        }
        let (provider_status, node_status) = match status {
            ProviderAdapterV2TurnTerminalStatus::Completed => (
                OrchestrationV2ProviderTurnStatus::Completed,
                OrchestrationV2ExecutionNodeStatus::Completed,
            ),
            ProviderAdapterV2TurnTerminalStatus::Interrupted => (
                OrchestrationV2ProviderTurnStatus::Interrupted,
                OrchestrationV2ExecutionNodeStatus::Interrupted,
            ),
            ProviderAdapterV2TurnTerminalStatus::Failed => (
                OrchestrationV2ProviderTurnStatus::Failed,
                OrchestrationV2ExecutionNodeStatus::Failed,
            ),
            ProviderAdapterV2TurnTerminalStatus::Cancelled => (
                OrchestrationV2ProviderTurnStatus::Cancelled,
                OrchestrationV2ExecutionNodeStatus::Cancelled,
            ),
        };
        turn.provider_turn.status = provider_status;
        turn.provider_turn.completed_at = Some(now);
        events.push(ProviderAdapterV2Event::NodeUpdated {
            driver: self.driver.clone(),
            node: root_turn_node(
                &child.thread.id,
                &turn.provider_turn.node_id,
                &child.provider_thread.id,
                &turn.provider_turn.id,
                turn.provider_turn.native_turn_ref.clone(),
                node_status,
                turn.provider_turn.started_at.unwrap_or(now),
                Some(now),
            ),
        });
        events.push(ProviderAdapterV2Event::ProviderTurnUpdated {
            driver: self.driver.clone(),
            thread_id: Some(child.thread.id.clone()),
            provider_turn: turn.provider_turn,
        });
        for event in events {
            self.emit(event);
        }
    }

    /// `emitCompaction`: the turn's compaction item. A `/compact`, or OpenCode's own when the
    /// context fills, runs inside the turn's execution. Its summary is the text it carried.
    fn compaction(&self, session_id: &str, event_type: &str, data: &Value) {
        let now = UtcDateTime::now();
        let mut guard = self.inner.lock();
        let Some(turn) = active_turn_mut(&mut guard, session_id) else {
            return;
        };
        let (native, title, status, started_at, completed_at, summary) = match event_type {
            "session.compaction.started" => {
                let native = format!(
                    "{}:compaction:{}",
                    turn.provider_turn.id.as_str(),
                    turn.compactions
                );
                turn.compactions += 1;
                turn.compaction = Some((native.clone(), now));
                (
                    native,
                    "Compacting context",
                    OrchestrationV2TurnItemStatus::Running,
                    Some(now),
                    None,
                    None,
                )
            }
            ended_or_failed => {
                let Some((native, started)) = turn.compaction.take() else {
                    return;
                };
                let (title, status) = if ended_or_failed == "session.compaction.ended" {
                    (
                        "Context compacted",
                        OrchestrationV2TurnItemStatus::Completed,
                    )
                } else {
                    ("Compaction failed", OrchestrationV2TurnItemStatus::Failed)
                };
                let summary = data
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned);
                (native, title, status, Some(started), Some(now), summary)
            }
        };
        let ordinal = item_ordinal(turn, &native);
        let item = compaction_turn_item(
            &self.driver,
            Compaction {
                thread_id: &turn.thread_id,
                root_node_id: &turn.provider_turn.node_id,
                provider_thread_id: &turn.provider_turn.provider_thread_id,
                provider_turn_id: &turn.provider_turn.id,
                native_item_id: &native,
                ordinal,
                title,
                status,
                started_at,
                completed_at,
                now,
                summary,
                weak_ref: false,
            },
        );
        drop(guard);
        self.emit(ProviderAdapterV2Event::TurnItemUpdated {
            driver: self.driver.clone(),
            turn_item: item,
        });
    }

    /// Sends the answer to OpenCode. Returns whether the request ends cancelled rather than
    /// resolved; the caller settles the request once the reply has landed.
    /// Answers a request. Returns whether it was cancelled and whether the answer reached OpenCode.
    async fn answer(
        &self,
        pending: &Pending,
        input: &ProviderAdapterV2RuntimeRequestResponseInput,
    ) -> Result<(bool, bool), ProviderAdapterV2Error> {
        match pending.ask.clone() {
            Ask::Permission {
                action,
                resources,
                save,
            } => {
                let decision = input.decision.ok_or_else(|| {
                    ProviderAdapterV2Error::protocol(format!(
                        "OpenCode approval request {} requires a decision",
                        input.request_id
                    ))
                })?;
                if matches!(
                    decision,
                    ProviderApprovalDecision::AcceptForSession
                        | ProviderApprovalDecision::AcceptAlways
                ) {
                    // Only the patterns OpenCode asked to save are granted. Empty `save` grants
                    // nothing (upstream iterates `native.save`).
                    let rules = {
                        let mut guard = self.inner.lock();
                        guard.threads.get_mut(&pending.session_id).map(|thread| {
                            for resource in &save {
                                let grant = SessionPermissionRule {
                                    action: action.clone(),
                                    resource: resource.clone(),
                                    effect: "allow".to_owned(),
                                };
                                if !thread.grants.iter().any(|existing| {
                                    existing.action == grant.action
                                        && existing.resource == grant.resource
                                }) {
                                    thread.grants.push(grant);
                                }
                            }
                            let rules = session_permission_rules(
                                mode_name(thread.runtime_mode),
                                &thread.interaction,
                                &thread.grants,
                                &[],
                            );
                            thread.rules = rules.clone();
                            rules
                        })
                    };
                    if let Some(rules) = rules {
                        let _ = self
                            .client
                            .update_session(&pending.session_id, &json!({ "permissions": rules }))
                            .await;
                    }
                }
                if decision == ProviderApprovalDecision::Decline {
                    let note = declined_note(&action, &resources);
                    let _ = self
                        .client
                        .synthetic(
                            &pending.session_id,
                            &json!({ "text": note, "delivery": "steer", "resume": false }),
                        )
                        .await;
                }
                let mut body = json!({ "decision": v2_permission_decision(decision) });
                if decision == ProviderApprovalDecision::Decline {
                    body["message"] = json!(DECLINED);
                }
                let cancelled = matches!(
                    decision,
                    ProviderApprovalDecision::Decline | ProviderApprovalDecision::Cancel
                );
                let delivered = deliver(&GONE_PERMISSION, || {
                    let (client, session_id, native_id, body) = (
                        self.client.clone(),
                        pending.session_id.clone(),
                        pending.native_id.clone(),
                        body.clone(),
                    );
                    async move {
                        client
                            .reply_permission(&session_id, &native_id, &body)
                            .await
                    }
                })
                .await;
                Ok((cancelled, delivered))
            }
            Ask::Form(form) => {
                let decline = matches!(
                    input.decision,
                    Some(ProviderApprovalDecision::Decline | ProviderApprovalDecision::Cancel)
                );
                let cancelled = input.answers.is_none() || decline;
                let delivered = if cancelled {
                    deliver(&GONE_FORM, || {
                        let (client, session_id, form_id) = (
                            self.client.clone(),
                            pending.session_id.clone(),
                            form.id.clone(),
                        );
                        async move { client.cancel_form(&session_id, &form_id, None).await }
                    })
                    .await
                } else {
                    let answers = input.answers.clone().unwrap_or_default();
                    let map = answers
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect();
                    let answer = form_answer(&form, &map);
                    deliver(&GONE_FORM, || {
                        let (client, session_id, form_id, answer) = (
                            self.client.clone(),
                            pending.session_id.clone(),
                            form.id.clone(),
                            answer.clone(),
                        );
                        async move { client.reply_form(&session_id, &form_id, &answer).await }
                    })
                    .await
                };
                Ok((cancelled, delivered))
            }
        }
    }

    /// Ends the turn a request OpenCode waits on would block, after the request could not be
    /// answered (upstream `abandonRequest`). The stopped run is interrupted, so it is not the
    /// next turn's.
    fn abandon_request(&self, guard: &mut V2Inner, session_id: &str) {
        if !guard
            .threads
            .get(session_id)
            .is_some_and(|thread| thread.active.is_some())
        {
            return;
        }
        if let Some(thread) = guard.threads.get_mut(session_id) {
            thread.unsettled = true;
        }
        self.finish(
            guard,
            session_id,
            ProviderAdapterV2TurnTerminalStatus::Failed,
            Some(REQUEST_ABANDONED.to_owned()),
        );
        let client = self.client.clone();
        let session = session_id.to_owned();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(2), client.interrupt(&session, None))
                .await;
        });
    }

    /// `settleRequest`: the request ends, resolved or cancelled, with its node and item.
    fn settle_request(
        &self,
        guard: &mut V2Inner,
        request_id: &RuntimeRequestId,
        session_id: &str,
        node: &OrchestrationV2ExecutionNode,
        item: &OrchestrationV2TurnItem,
        cancelled: bool,
    ) {
        let now = UtcDateTime::now();
        let owner = guard
            .children
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| session_id.to_owned());
        if let Some(thread) = guard.threads.get_mut(&owner)
            && let Some(request) = thread.requests.iter_mut().find(|r| r.id == *request_id)
        {
            request.status = if cancelled {
                OrchestrationV2RuntimeRequestStatus::Cancelled
            } else {
                OrchestrationV2RuntimeRequestStatus::Resolved
            };
            request.resolved_at = Some(now);
            let thread_id = thread.active.as_ref().map(|turn| turn.thread_id.clone());
            self.emit(ProviderAdapterV2Event::RuntimeRequestUpdated {
                driver: self.driver.clone(),
                thread_id,
                runtime_request: request.clone(),
            });
        }
        self.emit(ProviderAdapterV2Event::NodeUpdated {
            driver: self.driver.clone(),
            node: settle_request_node(node, cancelled, now),
        });
        self.emit(ProviderAdapterV2Event::TurnItemUpdated {
            driver: self.driver.clone(),
            turn_item: settle_request_item(item, cancelled, now),
        });
    }

    fn finish(
        &self,
        guard: &mut V2Inner,
        session_id: &str,
        status: ProviderAdapterV2TurnTerminalStatus,
        failure: Option<String>,
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
                    self.settle_request(
                        guard,
                        &pending.request_id,
                        &pending.session_id,
                        &pending.node,
                        &pending.item,
                        true,
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
        thread.unsettled = false;
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
        let thread_id = turn.thread_id.clone();
        let run_ordinal = turn.run_ordinal.max(1) as u64;
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
            thread_disposition: OrchestrationV2ProviderThreadDisposition::Reusable,
        });
    }

    fn require_session(
        guard: &V2Inner,
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

    async fn remember_windows(&self, directory: &str) {
        let Ok(located) = self.client.list_models(directory).await else {
            return;
        };
        let mut windows = HashMap::new();
        for model in located.data {
            let slug = model
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| model.get("slug").and_then(Value::as_str));
            if let (Some(slug), Some(window)) = (slug, context_window_of(&model)) {
                windows.insert(slug.to_owned(), window);
            }
        }
        self.inner.lock().windows.extend(windows);
    }
}

#[async_trait::async_trait]
impl ProviderSessionRuntime for OpenCodeV2Session {
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
        match self.rx.lock().take() {
            Some(rx) => Box::pin(UnboundedReceiverStream::new(rx)),
            None => Box::pin(futures::stream::empty()),
        }
    }
    fn has_pending_background_work(
        &self,
    ) -> Option<BoxFuture<'_, Result<bool, ProviderAdapterV2Error>>> {
        let pending = self
            .inner
            .lock()
            .threads
            .values()
            .any(|thread| thread.active.is_some());
        Some(Box::pin(async move { Ok(pending) }))
    }
    fn get_model_context_window(
        &self,
        model_selection: &turin_contracts::model_selection::ModelSelection,
        _cwd: Option<&str>,
    ) -> Option<Option<u64>> {
        Some(
            self.inner
                .lock()
                .windows
                .get(model_selection.model.as_str())
                .copied(),
        )
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
        let directory = input
            .runtime_policy
            .cwd
            .clone()
            .unwrap_or_else(|| self.session.cwd.as_str().to_owned());
        self.remember_windows(&directory).await;
        let parsed =
            parse_open_code_model_slug(input.model_selection.model.as_str()).ok_or_else(|| {
                ProviderAdapterV2Error::protocol(malformed_model(
                    input.model_selection.model.as_str(),
                ))
            })?;
        let rules = rules_for(&input.runtime_policy, &[]);
        let mut body = json!({
            "location": { "directory": directory },
            "model": { "providerID": parsed.provider_id, "modelID": parsed.model_id },
            "permissions": rules,
        });
        if let Some(title) = input
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
        {
            body["title"] = json!(title);
        }
        let created = self.client.create_session(&body).await.map_err(http_err)?;
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
            V2Thread {
                provider_thread: provider_thread.clone(),
                directory,
                agent: "build".to_owned(),
                model_key: format!("{}/{}", parsed.provider_id, parsed.model_id),
                runtime_mode: input.runtime_policy.runtime_mode,
                interaction: interaction_name(&input.runtime_policy).to_owned(),
                rules,
                grants: Vec::new(),
                instructions: None,
                unsettled: false,
                active: None,
                turns: Vec::new(),
                requests: Vec::new(),
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
        let native = self
            .client
            .get_session(&native_id)
            .await
            .map_err(http_err)?;
        let directory = input
            .runtime_policy
            .as_ref()
            .and_then(|policy| policy.cwd.clone())
            .or_else(|| {
                native
                    .pointer("/location/directory")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| self.session.cwd.as_str().to_owned());
        let mut thread = input.provider_thread;
        thread.provider_session_id = Some(self.provider_session_id.clone());
        thread.status = OrchestrationV2ProviderThreadStatus::Idle;
        thread.updated_at = UtcDateTime::now();
        let policy = input
            .runtime_policy
            .clone()
            .unwrap_or(ProviderAdapterV2RuntimePolicy {
                runtime_mode: RuntimeMode::FullAccess,
                interaction_mode: ProviderInteractionMode::Default,
                cwd: Some(directory.clone()),
                approval_policy: None,
                sandbox_policy: None,
                reasoning_effort: None,
            });
        let rules = rules_for(&policy, &[]);
        self.client
            .update_session(&native_id, &json!({ "permissions": rules }))
            .await
            .map_err(http_err)?;
        if native
            .pointer("/location/directory")
            .and_then(Value::as_str)
            != Some(directory.as_str())
        {
            let _ = self
                .client
                .move_session(&native_id, &json!({ "directory": directory }))
                .await;
        }
        self.inner.lock().threads.insert(
            native_id,
            V2Thread {
                provider_thread: thread.clone(),
                directory,
                agent: native
                    .get("agent")
                    .and_then(Value::as_str)
                    .unwrap_or("build")
                    .to_owned(),
                model_key: String::new(),
                runtime_mode: policy.runtime_mode,
                interaction: interaction_name(&policy).to_owned(),
                rules,
                grants: Vec::new(),
                instructions: None,
                unsettled: false,
                active: None,
                turns: Vec::new(),
                requests: Vec::new(),
            },
        );
        Ok(thread)
    }

    async fn start_turn(
        &self,
        input: ProviderAdapterV2TurnInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        if self.inner.lock().stream_dead.is_some() {
            return Err(ProviderAdapterV2Error::protocol(
                "OpenCode event stream has ended; reconnect the provider session before starting another turn.",
            ));
        }
        let session_id = {
            let guard = self.inner.lock();
            let session_id = Self::require_session(&guard, &input.provider_thread)?;
            if guard
                .threads
                .get(&session_id)
                .and_then(|thread| thread.active.as_ref())
                .is_some()
            {
                return Err(ProviderAdapterV2Error::protocol(format!(
                    "OpenCode session {session_id} already has an active turn"
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
        let still_stopping = {
            let unsettled = self
                .inner
                .lock()
                .threads
                .get(&session_id)
                .is_some_and(|thread| thread.unsettled);
            if unsettled {
                let active = self.client.session_active().await.map_err(http_err)?;
                if active.contains_key(&session_id) {
                    let _ = self.client.interrupt(&session_id, None).await;
                    true
                } else {
                    if let Some(thread) = self.inner.lock().threads.get_mut(&session_id) {
                        thread.unsettled = false;
                    }
                    false
                }
            } else {
                false
            }
        };
        let agent = if input.runtime_policy.interaction_mode == ProviderInteractionMode::Plan {
            "plan"
        } else {
            "build"
        };
        let model_key = format!("{}/{}", parsed.provider_id, parsed.model_id);
        let variant = model_option(&input.model_selection, "variant");
        let switch_agent = {
            let mut guard = self.inner.lock();
            let thread = guard.threads.get_mut(&session_id).ok_or_else(|| {
                ProviderAdapterV2Error::protocol(format!(
                    "OpenCode session {session_id} is not registered"
                ))
            })?;
            if thread.agent != agent {
                thread.agent = agent.to_owned();
                true
            } else {
                false
            }
        };
        if switch_agent {
            self.client
                .switch_agent(&session_id, &json!({ "agent": agent }))
                .await
                .map_err(http_err)?;
        }
        let rules = {
            let mut guard = self.inner.lock();
            if let Some(thread) = guard.threads.get_mut(&session_id) {
                thread.runtime_mode = input.runtime_policy.runtime_mode;
                thread.interaction = interaction_name(&input.runtime_policy).to_owned();
                let rules = rules_for(&input.runtime_policy, &thread.grants);
                thread.rules = rules.clone();
                rules
            } else {
                rules_for(&input.runtime_policy, &[])
            }
        };
        self.client
            .update_session(&session_id, &json!({ "permissions": rules }))
            .await
            .map_err(http_err)?;
        let switch_model = {
            let mut guard = self.inner.lock();
            guard.threads.get_mut(&session_id).and_then(|thread| {
                let next_key = match &variant {
                    Some(variant) => format!("{model_key}#{variant}"),
                    None => model_key.clone(),
                };
                if thread.model_key != next_key {
                    thread.model_key = next_key;
                    Some(())
                } else {
                    None
                }
            })
        };
        if switch_model.is_some() {
            let mut body = json!({
                "providerID": parsed.provider_id,
                "modelID": parsed.model_id,
            });
            if let Some(variant) = &variant {
                body["variant"] = json!(variant);
            }
            self.client
                .switch_model(&session_id, &body)
                .await
                .map_err(http_err)?;
        }
        let instructions = per_turn_instructions(input.model_selection.model.as_str());
        let changed = self
            .inner
            .lock()
            .threads
            .get(&session_id)
            .is_none_or(|thread| thread.instructions.as_deref() != Some(instructions.as_str()));
        if changed {
            self.client
                .put_instruction(&session_id, INSTRUCTIONS_KEY, &instructions)
                .await
                .map_err(http_err)?;
            if let Some(thread) = self.inner.lock().threads.get_mut(&session_id) {
                thread.instructions = Some(instructions);
            }
        }
        let prompt_id = turn_prompt_id(&session_id, input.attempt_id.as_str());
        let now = UtcDateTime::now();
        let provider_turn = OrchestrationV2ProviderTurn {
            id: derive::provider_turn(&self.driver, &prompt_id),
            provider_thread_id: input.provider_thread.id.clone(),
            node_id: input.root_node_id.clone(),
            run_attempt_id: Some(input.attempt_id.clone()),
            native_turn_ref: Some(native_ref(&prompt_id, true)),
            ordinal: PositiveInt::new(input.provider_turn_ordinal.max(1) as u64)
                .unwrap_or(PositiveInt::new(1).expect("one")),
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
            thread.active = Some(V2Turn {
                provider_turn: provider_turn.clone(),
                thread_id: input.thread_id.clone(),
                run_ordinal: input.run_ordinal,
                interrupted: false,
                finalized: false,
                usage: UsageAccum::new(),
                next_item_ordinal: input.provider_turn_ordinal.max(0) as u64 * 100 + 1,
                item_ordinals: HashMap::new(),
                text_blocks: HashMap::new(),
                tool_started: HashMap::new(),
                compaction: None,
                compactions: 0,
                app_thread: input.app_thread.clone(),
                before: None,
            });
        }
        self.emit(ProviderAdapterV2Event::ProviderTurnUpdated {
            driver: self.driver.clone(),
            thread_id: Some(input.thread_id.clone()),
            provider_turn,
        });
        // A continuation has no prompt of its own: it drains the wake its message names, and
        // with no wake to drain it finishes without a submit (upstream `runWake`).
        if is_continuation(&input) {
            match self.drain_wake(&session_id, &input.message.message_id) {
                Some(wake) => self.replay(&wake),
                None => {
                    let mut guard = self.inner.lock();
                    self.finish(
                        &mut guard,
                        &session_id,
                        ProviderAdapterV2TurnTerminalStatus::Completed,
                        None,
                    );
                }
            }
            return Ok(());
        }
        if still_stopping {
            let mut guard = self.inner.lock();
            self.finish(
                &mut guard,
                &session_id,
                ProviderAdapterV2TurnTerminalStatus::Failed,
                Some(OPENCODE_2_STILL_STOPPING.to_owned()),
            );
            return Err(ProviderAdapterV2Error::protocol(OPENCODE_2_STILL_STOPPING));
        }
        // A prompt joins the execution OpenCode is running on its own, so the turn takes that
        // wake and replays what it parked (upstream `takeRunningWake`).
        let running = {
            let mut guard = self.inner.lock();
            let held = guard.wakes.get_mut(&session_id);
            match held {
                Some(wakes) if wakes.last().is_some_and(|wake| wake.running) => wakes.pop(),
                _ => None,
            }
        };
        if let Some(wake) = running {
            self.replay(&wake);
        }
        let text =
            turin_provider_core::attachment_prompt::provider_message_text_with_attachment_paths(
                &input.message.text,
                &input.message.attachments,
                |_| None,
            );
        let result = self.submit(&session_id, &input, &text, &prompt_id).await;
        if let Err(err) = result {
            let mut guard = self.inner.lock();
            if let Some(thread) = guard.threads.get_mut(&session_id) {
                thread.unsettled = true;
            }
            self.finish(
                &mut guard,
                &session_id,
                ProviderAdapterV2TurnTerminalStatus::Failed,
                Some(err.to_string()),
            );
            return Err(err);
        }
        Ok(())
    }

    /// Forgets an idle thread and the subagent sessions it owns, as upstream
    /// `unloadThread` does. A thread with a running turn, or a subagent still
    /// working, stays. MCP removal is cut with MCP.
    fn unload_thread(
        &self,
        provider_thread: &OrchestrationV2ProviderThread,
    ) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        let native = provider_thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.as_ref())
            .map(|id| id.as_str().to_owned());
        if let Some(native) = native {
            let mut guard = self.inner.lock();
            let idle = guard
                .threads
                .get(&native)
                .is_some_and(|thread| thread.active.is_none());
            let children: Vec<String> = guard
                .children
                .iter()
                .filter(|(_, parent)| **parent == native)
                .map(|(child, _)| child.clone())
                .collect();
            let child_busy = children.iter().any(|child| {
                guard
                    .threads
                    .get(child)
                    .is_some_and(|thread| thread.active.is_some())
            });
            if idle && !child_busy {
                guard.threads.remove(&native);
                for child in children {
                    guard.children.remove(&child);
                    guard.threads.remove(&child);
                }
            }
        }
        Some(Box::pin(async { Ok(()) }))
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
            Self::require_session(&guard, &input.provider_thread)?
        };
        let text = input.message.text.trim().to_owned();
        if text.is_empty() {
            return Err(ProviderAdapterV2Error::protocol(
                "OpenCode steering requires text or an attachment",
            ));
        }
        let id = steer_prompt_id(&session_id, input.message.message_id.as_str());
        self.client
            .prompt(
                &session_id,
                &json!({ "id": id, "text": text, "delivery": "steer" }),
            )
            .await
            .map(|_| ())
            .map_err(http_err)
    }

    async fn interrupt_turn(
        &self,
        input: ProviderAdapterV2InterruptInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        let (session_id, mismatched) = {
            let guard = self.inner.lock();
            let session_id = Self::require_session(&guard, &input.provider_thread)?;
            let mismatched = guard
                .threads
                .get(&session_id)
                .and_then(|thread| thread.active.as_ref())
                .is_some_and(|turn| turn.provider_turn.id != input.provider_turn_id);
            (session_id, mismatched)
        };
        // A Stop that restarts the runtime also stops the background work (upstream
        // `interruptTurn`).
        if input.request_runtime_restart == Some(true) {
            self.stop_background(&session_id).await;
        }
        if mismatched {
            return Ok(());
        }
        if let Some(turn) = self
            .inner
            .lock()
            .threads
            .get_mut(&session_id)
            .and_then(|thread| thread.active.as_mut())
        {
            turn.interrupted = true;
        }
        let reply = self
            .client
            .interrupt(&session_id, None)
            .await
            .map_err(http_err)?;
        if !reply.interrupted {
            let mut guard = self.inner.lock();
            if let Some(turn) = guard
                .threads
                .get_mut(&session_id)
                .and_then(|thread| thread.active.as_mut())
                && !turn.finalized
            {
                turn.interrupted = false;
                return Err(ProviderAdapterV2Error::protocol(format!(
                    "OpenCode session {session_id} had nothing running to stop"
                )));
            }
        }
        Ok(())
    }

    async fn respond_to_runtime_request(
        &self,
        input: ProviderAdapterV2RuntimeRequestResponseInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        // The entry stays until the reply lands, so a failed reply can be retried. A second
        // answer while one is in flight is refused, as upstream's `answering` flag does.
        let (key, pending) = {
            let mut guard = self.inner.lock();
            let key = guard
                .pending
                .iter()
                .find(|(_, pending)| pending.request_id == input.request_id)
                .map(|(key, _)| key.clone());
            let Some(key) = key else {
                return Err(ProviderAdapterV2Error::protocol(format!(
                    "No pending OpenCode request {}",
                    input.request_id
                )));
            };
            let entry = guard.pending.get_mut(&key).expect("just found");
            if entry.answering {
                return Err(ProviderAdapterV2Error::protocol(format!(
                    "OpenCode request {} is already being answered",
                    input.request_id
                )));
            }
            entry.answering = true;
            (key, entry.clone())
        };
        let outcome = self.answer(&pending, &input).await;
        let mut guard = self.inner.lock();
        match outcome {
            Ok((cancelled, delivered)) => {
                guard.pending.remove(&key);
                self.settle_request(
                    &mut guard,
                    &pending.request_id,
                    &pending.session_id,
                    &pending.node,
                    &pending.item,
                    cancelled || !delivered,
                );
                // OpenCode waits on a request nobody answered, so the turn it blocks ends failed.
                if !delivered {
                    self.abandon_request(&mut guard, &pending.session_id);
                }
                Ok(())
            }
            Err(err) => {
                if let Some(entry) = guard.pending.get_mut(&key) {
                    entry.answering = false;
                }
                Err(err)
            }
        }
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
        let page = self
            .client
            .list_messages(&MessageQuery::new(&session_id))
            .await
            .map_err(http_err)?;
        let guard = self.inner.lock();
        let stored = guard.threads.get(&session_id);
        Ok(ProviderAdapterV2ThreadSnapshot {
            provider_thread: input.provider_thread,
            provider_turns: stored
                .map(|thread| thread.turns.clone())
                .unwrap_or_default(),
            messages: messages_of(&page.data),
            runtime_requests: stored
                .map(|thread| thread.requests.clone())
                .unwrap_or_default(),
            provider_payload: Some(Value::Array(page.data)),
        })
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
        let kept = match &input.target {
            ProviderAdapterV2RollbackTarget::ThreadStart { .. } => None,
            ProviderAdapterV2RollbackTarget::ProviderTurn { provider_turn, .. } => {
                Some(provider_turn.ordinal.get())
            }
        };
        let prompts = user_prompt_ids(&input.provider_thread_turns);
        let boundary = boundary_after(
            &input.provider_thread_turns,
            &input.provider_thread.id,
            kept,
            &prompts,
        )
        .map_err(ProviderAdapterV2Error::protocol)?;
        if let Some(boundary) = boundary {
            if let Err(err) = self
                .client
                .revert_stage(
                    &session_id,
                    &json!({ "messageID": boundary, "files": false }),
                )
                .await
            {
                let _ = self.client.revert_clear(&session_id).await;
                return Err(http_err(err));
            }
            if let Err(err) = self.client.revert_commit(&session_id).await {
                let _ = self.client.revert_clear(&session_id).await;
                return Err(http_err(err));
            }
        }
        self.read_thread_snapshot(ProviderAdapterV2ReadThreadSnapshotInput {
            provider_thread: input.provider_thread,
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
        let turns = input.source_provider_turns.clone().unwrap_or_default();
        let before = if let Some(turn_id) = &input.provider_turn_id {
            let selected = turns
                .iter()
                .find(|turn| &turn.id == turn_id)
                .ok_or_else(|| {
                    ProviderAdapterV2Error::protocol(format!(
                        "OpenCode fork boundary turn {turn_id} was not found"
                    ))
                })?;
            let prompts = user_prompt_ids(&turns);
            boundary_after(
                &turns,
                &input.source_provider_thread.id,
                Some(selected.ordinal.get()),
                &prompts,
            )
            .map_err(ProviderAdapterV2Error::protocol)?
        } else {
            None
        };
        let body = match &before {
            Some(before) => json!({ "before": before }),
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
        let mut thread = make_thread(
            &self.instance_id,
            &self.provider_session_id,
            &input.target_thread_id,
            fork_id,
            None,
            now,
        );
        thread.forked_from = Some(OrchestrationV2ProviderThreadForkedFrom {
            provider_thread_id: input.source_provider_thread.id.clone(),
            provider_turn_id: input.provider_turn_id.clone(),
            checkpoint_id: None,
        });
        let directory = input
            .runtime_policy
            .as_ref()
            .and_then(|policy| policy.cwd.clone())
            .unwrap_or_else(|| self.session.cwd.as_str().to_owned());
        self.inner.lock().threads.insert(
            fork_id.to_owned(),
            V2Thread {
                provider_thread: thread.clone(),
                directory,
                agent: "build".to_owned(),
                model_key: String::new(),
                runtime_mode: input
                    .runtime_policy
                    .as_ref()
                    .map(|policy| policy.runtime_mode)
                    .unwrap_or(RuntimeMode::FullAccess),
                interaction: input
                    .runtime_policy
                    .as_ref()
                    .map(interaction_name)
                    .unwrap_or("default")
                    .to_owned(),
                rules: Vec::new(),
                grants: Vec::new(),
                instructions: None,
                unsettled: false,
                active: None,
                turns: Vec::new(),
                requests: Vec::new(),
            },
        );
        Ok(thread)
    }
}

impl OpenCodeV2Session {
    async fn submit(
        &self,
        session_id: &str,
        input: &ProviderAdapterV2TurnInput,
        text: &str,
        prompt_id: &str,
    ) -> Result<(), ProviderAdapterV2Error> {
        let bare = input.message.attachments.is_empty();
        if bare && text.trim() == "/compact" {
            return self
                .client
                .compact(session_id, &json!({ "id": prompt_id }))
                .await
                .map(|_| ())
                .map_err(http_err);
        }
        if bare && let Some(command) = command_of(text.trim()) {
            let directory = self
                .inner
                .lock()
                .threads
                .get(session_id)
                .map(|thread| thread.directory.clone())
                .unwrap_or_default();
            let commands = self
                .client
                .list_commands(&directory)
                .await
                .map(|located| located.data)
                .unwrap_or_default();
            if commands.iter().any(|entry| {
                entry.get("name").and_then(Value::as_str) == Some(command.name.as_str())
            }) {
                let newest = self
                    .client
                    .list_messages(&MessageQuery {
                        session_id: session_id.to_owned(),
                        limit: Some(1),
                        order: Some("desc".to_owned()),
                        cursor: None,
                        message_type: None,
                    })
                    .await
                    .ok()
                    .and_then(|page| {
                        page.data.first().and_then(|message| {
                            message.get("id").and_then(Value::as_str).map(str::to_owned)
                        })
                    });
                if let Some(turn) = self
                    .inner
                    .lock()
                    .threads
                    .get_mut(session_id)
                    .and_then(|thread| thread.active.as_mut())
                {
                    turn.before = Some(newest.unwrap_or_default());
                }
                return self
                    .client
                    .command(
                        session_id,
                        &json!({ "command": command.name, "arguments": command.text }),
                    )
                    .await
                    .map_err(http_err);
            }
        }
        let mut skills = Vec::new();
        if text.contains('$') {
            let directory = self
                .inner
                .lock()
                .threads
                .get(session_id)
                .map(|thread| thread.directory.clone())
                .unwrap_or_default();
            let known = self
                .client
                .list_skills(&directory)
                .await
                .map(|located| {
                    located
                        .data
                        .iter()
                        .filter_map(|skill| {
                            skill
                                .get("id")
                                .or_else(|| skill.get("name"))
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .collect::<HashSet<_>>()
                })
                .unwrap_or_default();
            skills = skills_named(text, &known);
        }
        let mut body = json!({ "id": prompt_id, "text": text.trim() });
        if !skills.is_empty() {
            body["skills"] = json!(
                skills
                    .into_iter()
                    .map(|id| json!({ "id": id }))
                    .collect::<Vec<_>>()
            );
        }
        self.client
            .prompt(session_id, &body)
            .await
            .map(|_| ())
            .map_err(http_err)
    }
}

impl OpenCodeV2Session {
    /// The parked events of a wake, replayed into the turn that took it.
    fn replay(&self, wake: &Wake) {
        for (event_type, data) in &wake.events {
            self.on_event(event_type, data);
        }
    }

    /// The wake a continuation names (upstream `runWake`). Wakes held before it lost their
    /// continuation, so they are discarded. Wakes after it stay held.
    fn drain_wake(&self, session_id: &str, message_id: &MessageId) -> Option<Wake> {
        let mut guard = self.inner.lock();
        let mut wakes = guard.wakes.remove(session_id)?;
        let Some(index) = wakes.iter().position(|wake| &wake.message_id == message_id) else {
            guard.wakes.insert(session_id.to_owned(), wakes);
            return None;
        };
        let later = wakes.split_off(index + 1);
        let wake = wakes.pop()?;
        for earlier in &wakes {
            earlier.dropped.store(true, Ordering::SeqCst);
        }
        if !later.is_empty() {
            guard.wakes.insert(session_id.to_owned(), later);
        }
        Some(wake)
    }

    /// A Stop that restarts the runtime (upstream `stopBackground`): the background subagents
    /// the session launched are interrupted and settled, the wakes it holds are discarded, and
    /// the reports already queued answer that Stop. A child that does not answer its interrupt
    /// stays running and tracked, so the next Stop tries it again.
    async fn stop_background(&self, session_id: &str) {
        let children: Vec<String> = self
            .inner
            .lock()
            .background
            .iter()
            .filter(|(_, open)| open.parent_session == session_id)
            .map(|(child, _)| child.clone())
            .collect();
        for child in children {
            self.inner.lock().stopped_children.insert(child.clone());
            let reached = matches!(
                tokio::time::timeout(INTERRUPT_TIMEOUT, self.client.interrupt(&child, None)).await,
                Ok(Ok(_))
            );
            if !reached {
                continue;
            }
            let open = {
                let mut guard = self.inner.lock();
                let open = guard.background.remove(&child);
                if let Some(open) = &open {
                    guard.background_calls.remove(&open.native_item_id);
                }
                open
            };
            if let Some(open) = open {
                for event in
                    self.settle_background(open, OrchestrationV2TurnItemStatus::Interrupted, None)
                {
                    self.emit(event);
                }
            }
        }
        let running = {
            let mut guard = self.inner.lock();
            let queued: Vec<String> = guard
                .reports
                .get(session_id)
                .map(|reports| {
                    reports
                        .iter()
                        .map(|report| report.child_session.clone())
                        .collect()
                })
                .unwrap_or_default();
            guard.stopped_children.extend(queued);
            // A held wake no turn will take: its execution is stopped, not replayed.
            let wakes = guard.wakes.remove(session_id).unwrap_or_default();
            for wake in &wakes {
                wake.dropped.store(true, Ordering::SeqCst);
            }
            wakes.iter().any(|wake| wake.running)
        };
        if running {
            if let Some(thread) = self.inner.lock().threads.get_mut(session_id) {
                thread.unsettled = true;
            }
            let _ =
                tokio::time::timeout(INTERRUPT_TIMEOUT, self.client.interrupt(session_id, None))
                    .await;
        }
    }
}

impl Drop for OpenCodeV2Session {
    fn drop(&mut self) {
        if let Some(handle) = self.reader.lock().take() {
            handle.abort();
        }
    }
}

/// The events that end an execution (upstream `executionEnd`).
fn is_execution_end(event_type: &str) -> bool {
    matches!(
        event_type,
        "session.execution.succeeded"
            | "session.execution.failed"
            | "session.execution.interrupted"
    )
}

/// An agent's continuation: the turn a wake asks for (upstream `isContinuation`).
fn is_continuation(input: &ProviderAdapterV2TurnInput) -> bool {
    input.message.created_by == OrchestrationV2Actor::Agent
        && input.message.creation_source == OrchestrationV2CreationSource::Provider
}

/// Whether an answer reached OpenCode (upstream `deliver`): retried once, under one deadline. An
/// answer to a request OpenCode already dropped (`gone`) counts as delivered.
async fn deliver<T, Fut>(gone: &[&str], mut send: impl FnMut() -> Fut) -> bool
where
    Fut: std::future::Future<Output = Result<T, crate::error::OpenCodeRuntimeError>>,
{
    let attempts = async {
        for _ in 0..2 {
            match send().await {
                Ok(_) => return true,
                Err(err) if gone.iter().any(|name| err.detail.contains(name)) => return true,
                Err(_) => {}
            }
        }
        false
    };
    tokio::time::timeout(REQUEST_REPLY_TIMEOUT, attempts)
        .await
        .unwrap_or(false)
}

/// The gate upstream `offerWake` puts on a wake: a wake a Stop or a later continuation dropped
/// is not dispatched (`dispatchIfCurrent`).
fn wake_dispatch(dropped: Arc<AtomicBool>) -> DispatchIfCurrent {
    Box::new(
        move |dispatch: BoxFuture<'static, Result<(), String>>| -> BoxFuture<'static, Result<Option<()>, String>> {
            let dropped = Arc::clone(&dropped);
            Box::pin(async move {
                if dropped.load(Ordering::SeqCst) {
                    return Ok(None);
                }
                dispatch.await.map(Some)
            })
        },
    )
}

fn http_err(err: crate::error::OpenCodeRuntimeError) -> ProviderAdapterV2Error {
    ProviderAdapterV2Error::protocol(err.detail)
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn interaction_name(policy: &ProviderAdapterV2RuntimePolicy) -> &'static str {
    if policy.interaction_mode == ProviderInteractionMode::Plan {
        "plan"
    } else {
        "default"
    }
}

fn mode_name(mode: RuntimeMode) -> &'static str {
    match mode {
        RuntimeMode::ApprovalRequired => "approval-required",
        RuntimeMode::AutoAcceptEdits => "auto-accept-edits",
        RuntimeMode::Auto => "auto",
        RuntimeMode::FullAccess => "full-access",
    }
}

fn rules_for(
    policy: &ProviderAdapterV2RuntimePolicy,
    grants: &[SessionPermissionRule],
) -> Vec<SessionPermissionRule> {
    let interaction = if policy.interaction_mode == ProviderInteractionMode::Plan {
        "plan"
    } else {
        "default"
    };
    session_permission_rules(mode_name(policy.runtime_mode), interaction, grants, &[])
}

fn user_prompt_ids(turns: &[OrchestrationV2ProviderTurn]) -> HashSet<String> {
    turns
        .iter()
        .filter_map(|turn| prompt_of(turn).map(str::to_owned))
        .collect()
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

/// The turn running in `session`: a thread's active turn, or a child session's turn.
fn active_turn_mut<'a>(inner: &'a mut V2Inner, session: &str) -> Option<&'a mut V2Turn> {
    if inner
        .threads
        .get(session)
        .is_some_and(|thread| thread.active.is_some())
    {
        return inner
            .threads
            .get_mut(session)
            .and_then(|thread| thread.active.as_mut());
    }
    inner.child_turns.get_mut(session)
}

fn item_ordinal(turn: &mut V2Turn, native_id: &str) -> u64 {
    next_ordinal(
        &mut turn.item_ordinals,
        &mut turn.next_item_ordinal,
        native_id,
    )
}

fn tool_base(
    turn: &mut V2Turn,
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

fn messages_of(messages: &[Value]) -> Vec<OrchestrationV2ConversationMessage> {
    let now = UtcDateTime::now();
    messages
        .iter()
        .filter_map(|message| {
            let role = message
                .get("role")
                .or_else(|| message.pointer("/info/role"))
                .and_then(Value::as_str)?;
            let id = message
                .get("id")
                .or_else(|| message.pointer("/info/id"))
                .and_then(Value::as_str)?;
            let text = message
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    message
                        .get("content")
                        .or_else(|| message.get("parts"))
                        .and_then(Value::as_array)
                        .map(|parts| {
                            parts
                                .iter()
                                .filter(|part| {
                                    part.get("type").and_then(Value::as_str) == Some("text")
                                })
                                .filter_map(|part| part.get("text").and_then(Value::as_str))
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                })?;
            if text.is_empty() {
                return None;
            }
            Some(OrchestrationV2ConversationMessage {
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
                thread_id: ThreadId::new("thread").expect("thread"),
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
            })
        })
        .collect()
}
