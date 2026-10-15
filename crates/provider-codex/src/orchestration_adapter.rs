//! Port of upstream/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts
//! at the provider-core `ProviderAdapterV2` boundary.
use crate::{
    adapter::{CodexAdapter, CodexEvent, CodexSession},
    approvals::{Decision, PendingRequest},
    items,
    params::RuntimePolicy,
    projection::{self, RuntimeState, TurnState},
    runtime::CodexRuntimeConfig,
    subagent,
};
use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{collections::VecDeque, path::Path, sync::Arc};
use turin_codex_app_server::{client::Client, errors::AppServerError};
use turin_contracts::{
    ProviderSessionId, ThreadId, TrimmedNonEmptyString, UtcDateTime,
    model::ProviderOptionSelectionValue,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2ExecutionNode, OrchestrationV2ExecutionNodeKind,
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2NativeRefStrength,
        OrchestrationV2ProviderCapabilities, OrchestrationV2ProviderRef,
        OrchestrationV2ProviderSession, OrchestrationV2ProviderSessionStatus,
        OrchestrationV2ProviderThread, OrchestrationV2ProviderThreadForkedFrom,
        OrchestrationV2ProviderThreadStatus, OrchestrationV2ProviderTurn,
        OrchestrationV2ProviderTurnStatus, OrchestrationV2ResponseCapability,
        OrchestrationV2RuntimeRequest, OrchestrationV2RuntimeRequestKind,
        OrchestrationV2RuntimeRequestStatus, OrchestrationV2TurnItem,
        OrchestrationV2TurnItemStatus,
    },
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
    provider_policy::{ProviderApprovalDecision, ProviderRequestKind},
};
use turin_provider_core::{
    failure::ProviderFailureCause,
    id_allocator::{IdAllocator, derive},
    provider_adapter::{
        ProviderAdapterErrorKind, ProviderAdapterV2, ProviderAdapterV2EnsureThreadInput,
        ProviderAdapterV2Error, ProviderAdapterV2Event, ProviderAdapterV2ForkThreadInput,
        ProviderAdapterV2InterruptInput, ProviderAdapterV2OpenSessionInput,
        ProviderAdapterV2ReadThreadSnapshotInput, ProviderAdapterV2ResumeThreadInput,
        ProviderAdapterV2RollbackTarget, ProviderAdapterV2RollbackThreadInput,
        ProviderAdapterV2RuntimeRequestResponseInput, ProviderAdapterV2SteerInput,
        ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2TurnInput, ProviderEventStream,
        ProviderSessionRuntime,
    },
    selection_transition::{
        ProviderSelectionTransitionInput, ProviderSelectionTransitionPlan,
        turn_scoped_selection_transition,
    },
};

pub(crate) fn driver() -> ProviderDriverKind {
    ProviderDriverKind::new("codex").expect("driver kind")
}
fn nonempty(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).expect("nonempty provider value")
}
pub(crate) fn native_ref(id: &str) -> OrchestrationV2ProviderRef {
    OrchestrationV2ProviderRef {
        driver: driver(),
        native_id: Some(nonempty(id)),
        strength: OrchestrationV2NativeRefStrength::Strong,
        fingerprint: None,
        ordinal: None,
    }
}
#[allow(clippy::result_large_err)]
fn native_id(thread: &OrchestrationV2ProviderThread) -> Result<&str, ProviderAdapterV2Error> {
    thread
        .native_thread_ref
        .as_ref()
        .and_then(|r| r.native_id.as_ref())
        .map(TrimmedNonEmptyString::as_str)
        .ok_or_else(|| {
            ProviderAdapterV2Error::new(ProviderAdapterErrorKind::Protocol {
                driver: driver(),
                detail: "Provider thread is missing a native Codex thread id".into(),
                payload: None,
            })
        })
}
fn time(seconds: Option<&Value>) -> UtcDateTime {
    seconds
        .and_then(Value::as_f64)
        .and_then(|s| chrono::DateTime::from_timestamp_millis((s * 1000.0) as i64))
        .map(UtcDateTime::from_datetime)
        .unwrap_or_else(UtcDateTime::now)
}
fn error(kind: ProviderAdapterErrorKind, cause: impl std::fmt::Display) -> ProviderAdapterV2Error {
    ProviderAdapterV2Error::new(kind).with_cause(
        ProviderFailureCause::tagged("CodexAppServerError").with_message(cause.to_string()),
    )
}

pub struct CodexProviderAdapter {
    instance_id: ProviderInstanceId,
    driver: ProviderDriverKind,
    config: CodexRuntimeConfig,
    injected_client: Mutex<Option<Client>>,
}

impl CodexProviderAdapter {
    pub fn new(instance_id: ProviderInstanceId, config: CodexRuntimeConfig) -> Self {
        Self {
            instance_id,
            driver: driver(),
            config,
            injected_client: Mutex::new(None),
        }
    }
    /// Test seam: use the pinned app-server replay transport without paid turns.
    pub fn with_client(
        instance_id: ProviderInstanceId,
        config: CodexRuntimeConfig,
        client: Client,
    ) -> Self {
        Self {
            instance_id,
            driver: driver(),
            config,
            injected_client: Mutex::new(Some(client)),
        }
    }
}

fn capabilities() -> OrchestrationV2ProviderCapabilities {
    serde_json::from_value(json!({
        "sessions":{"supportsMultipleProviderThreadsPerSession":true,"supportsModelSwitchInSession":true,"supportsProviderSwitchingViaHandoff":true,"supportsRuntimeModeSwitchInSession":true,"pendingRequestsSurviveRestart":false},
        "threads":{"canCreateEmptyThread":true,"canReadThreadSnapshot":true,"canRollbackThread":true,"canForkThread":true,"canForkFromTurn":true,"canForkFromSubagentThread":true,"exposesNativeThreadId":true},
        "turns":{"exposesNativeTurnId":true,"emitsTurnStarted":true,"emitsTurnCompleted":true,"supportsInterrupt":true,"supportsActiveSteering":true,"supportsSteeringByInterruptRestart":true,"supportsQueuedMessages":true,"terminalStatusQuality":"strong"},
        "streaming":{"streamsAssistantText":true,"streamsReasoning":true,"streamsToolOutput":true,"streamsPlanText":true,"emitsMessageCompleted":true},
        "tools":{"exposesToolItemIds":true,"emitsToolStarted":true,"emitsToolCompleted":true,"emitsToolOutput":true,"supportsMcpTools":true,"supportsDynamicToolCallbacks":true},
        "approvals":{"supportsCommandApproval":true,"supportsFileReadApproval":true,"supportsFileChangeApproval":true,"supportsApplyPatchApproval":true,"approvalsHaveNativeRequestIds":true,"approvalCallbacksAreLiveOnly":true,"approvalsCanOriginateFromSubagents":true},
        "planning":{"emitsPlanUpdated":true,"emitsTodoList":true,"emitsProposedPlan":true,"supportsStructuredQuestions":true,"planDeltasHaveItemIds":true},
        "subagents":{"supportsSubagents":true,"exposesSubagentThreadIds":true,"emitsSubagentLifecycle":true,"canWaitForSubagents":true,"canCloseSubagents":true,"canForkSubagentThread":true},
        "context":{"acceptsSystemContext":true,"acceptsDeveloperContext":true,"acceptsSyntheticUserContext":true,"canGenerateSummaries":true,"canConsumeHandoffSummaries":true,"supportsDeltaHandoff":true,"supportsFullThreadHandoff":true,"maxRecommendedHandoffChars":null},
        "checkpointing":{"appCanCheckpointFilesystem":true,"supportsNestedCheckpointScopes":true,"providerCanRollbackConversation":true,"providerRollbackReturnsSnapshot":true,"providerCanReadConversationSnapshot":true},
        "identity":{"nativeThreadIds":"strong","nativeTurnIds":"strong","nativeItemIds":"strong","nativeRequestIds":"strong"},
        "runtimePolicy":{"enforcement":"native"}
    })).expect("upstream Codex capabilities")
}

#[async_trait]
impl ProviderAdapterV2 for CodexProviderAdapter {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.instance_id
    }
    fn driver(&self) -> &ProviderDriverKind {
        &self.driver
    }
    async fn get_capabilities(
        &self,
    ) -> Result<OrchestrationV2ProviderCapabilities, ProviderAdapterV2Error> {
        Ok(capabilities())
    }
    async fn plan_selection_transition(
        &self,
        _input: ProviderSelectionTransitionInput,
    ) -> Result<ProviderSelectionTransitionPlan, ProviderAdapterV2Error> {
        Ok(turn_scoped_selection_transition())
    }
    async fn open_session(
        &self,
        input: ProviderAdapterV2OpenSessionInput,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderAdapterV2Error> {
        let client = if let Some(client) = self.injected_client.lock().take() {
            client
        } else {
            let home = std::env::var("HOME").unwrap_or_default();
            self.config.spawn(Path::new(&home)).map_err(|e| {
                error(
                    ProviderAdapterErrorKind::OpenSession {
                        driver: self.driver.clone(),
                        provider_session_id: input.provider_session_id.clone(),
                    },
                    e,
                )
            })?
        };
        let session = CodexAdapter::initialize(client, &self.config.client_version)
            .await
            .map_err(|e| {
                error(
                    ProviderAdapterErrorKind::OpenSession {
                        driver: self.driver.clone(),
                        provider_session_id: input.provider_session_id.clone(),
                    },
                    e,
                )
            })?;
        let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel();
        let state = Arc::new(Mutex::new(RuntimeState::default()));
        let turn_registered = Arc::new(tokio::sync::Notify::new());
        let pump = tokio::spawn(project_pump(
            session.take_events(),
            session.take_requests(),
            state.clone(),
            turn_registered.clone(),
            input.provider_session_id.clone(),
            out_tx,
        ));
        let now = UtcDateTime::now();
        let cwd = input
            .runtime_policy
            .cwd
            .as_deref()
            .unwrap_or_else(|| self.config.cwd.to_str().unwrap_or("."));
        let provider_session = OrchestrationV2ProviderSession {
            id: input.provider_session_id.clone(),
            driver: self.driver.clone(),
            provider_instance_id: self.instance_id.clone(),
            status: OrchestrationV2ProviderSessionStatus::Ready,
            cwd: nonempty(cwd),
            model: Some(input.model_selection.model.clone()),
            capabilities: capabilities(),
            created_at: now,
            updated_at: now,
            last_error: None,
        };
        Ok(Arc::new(CodexProviderRuntime {
            instance_id: self.instance_id.clone(),
            driver: self.driver.clone(),
            provider_session_id: input.provider_session_id,
            provider_session,
            session: Arc::new(session),
            events: Mutex::new(Some(out_rx)),
            pump,
            turn_registered,
            state,
            config: self.config.clone(),
        }))
    }
}

pub struct CodexProviderRuntime {
    instance_id: ProviderInstanceId,
    driver: ProviderDriverKind,
    provider_session_id: ProviderSessionId,
    provider_session: OrchestrationV2ProviderSession,
    session: Arc<CodexSession>,
    events: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<ProviderAdapterV2Event>>>,
    /// Projects notifications as they arrive, not when the stream is polled, so
    /// interrupt sees the running commands the turn has started.
    pump: tokio::task::JoinHandle<()>,
    /// Wakes the pump when `turn/start` registers a turn that parked events wait on.
    turn_registered: Arc<tokio::sync::Notify>,
    state: Arc<Mutex<RuntimeState>>,
    config: CodexRuntimeConfig,
}

impl Drop for CodexProviderRuntime {
    fn drop(&mut self) {
        self.pump.abort();
    }
}

async fn recv_raw(
    rx: &mut Option<tokio::sync::mpsc::UnboundedReceiver<CodexEvent>>,
) -> Option<CodexEvent> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

async fn recv_request(
    rx: &mut Option<tokio::sync::mpsc::UnboundedReceiver<PendingRequest>>,
) -> PendingRequest {
    match rx {
        Some(rx) => match rx.recv().await {
            Some(request) => request,
            None => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

/// Owns the raw notification and request channels. Each frame is projected in
/// arrival order under the state lock, and the events go to the stream.
/// A notification or request whose turn is not registered yet. `turn/start`
/// returns after its first notifications can arrive, so they wait here.
enum Parked {
    Event(CodexEvent),
    Request(PendingRequest),
}

/// Bounds the parked queue. A turn that never registers is dropped once it is
/// this old, so a stray notification cannot grow memory without bound.
const PARKED_LIMIT: usize = 4096;

impl Parked {
    fn turn(&self) -> Option<&str> {
        match self {
            Parked::Event(event) => projection::turn_scoped(&event.method, &event.params),
            Parked::Request(request) => request.params.get("turnId").and_then(Value::as_str),
        }
    }
}

fn project_parked(
    item: Parked,
    state: &mut RuntimeState,
    session_id: &ProviderSessionId,
) -> Vec<ProviderAdapterV2Event> {
    match item {
        Parked::Event(event) => projection::project_event(event, state),
        Parked::Request(request) => project_request(request, state, session_id),
    }
}

/// Whether an item's turn is registered, or is the first `turn/started` of a
/// registered subagent, which opens a child turn when it projects.
fn admitted(item: &Parked, state: &RuntimeState) -> bool {
    match item.turn() {
        None => true,
        Some(turn) => {
            state.turns.contains_key(turn)
                || state.settled.contains_key(turn)
                || match item {
                    Parked::Event(event) => {
                        let thread = event
                            .params
                            .get("threadId")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        event.method == "turn/started"
                            && subagent::opens_child_turn(state, thread, turn)
                    }
                    Parked::Request(_) => false,
                }
        }
    }
}

/// Projects one arrival in order. It parks the arrival if its turn is not
/// admitted, or if an earlier parked item for the same turn is still waiting.
fn ingest(
    parked: &mut VecDeque<Parked>,
    item: Parked,
    state: &mut RuntimeState,
    session_id: &ProviderSessionId,
) -> Vec<ProviderAdapterV2Event> {
    let behind_parked = item
        .turn()
        .is_some_and(|turn| parked.iter().any(|p| p.turn() == Some(turn)));
    if behind_parked || !admitted(&item, state) {
        if parked.len() >= PARKED_LIMIT {
            parked.pop_front();
        }
        parked.push_back(item);
        return vec![];
    }
    project_parked(item, state, session_id)
}

/// Projects every parked item that is admitted now, keeping arrival order. A
/// projected item can register a subagent that an earlier parked item waits on,
/// so the pass repeats until it makes no progress.
fn drain_parked(
    parked: &mut VecDeque<Parked>,
    state: &mut RuntimeState,
    session_id: &ProviderSessionId,
) -> Vec<ProviderAdapterV2Event> {
    let mut events = vec![];
    loop {
        let mut progressed = false;
        let mut index = 0;
        while index < parked.len() {
            if admitted(&parked[index], state) {
                let item = parked.remove(index).expect("index in range");
                events.extend(project_parked(item, state, session_id));
                progressed = true;
            } else {
                index += 1;
            }
        }
        if !progressed {
            return events;
        }
    }
}

async fn project_pump(
    mut events: Option<tokio::sync::mpsc::UnboundedReceiver<CodexEvent>>,
    mut requests: Option<tokio::sync::mpsc::UnboundedReceiver<PendingRequest>>,
    state: Arc<Mutex<RuntimeState>>,
    registered: Arc<tokio::sync::Notify>,
    session_id: ProviderSessionId,
    out: tokio::sync::mpsc::UnboundedSender<ProviderAdapterV2Event>,
) {
    let mut parked = VecDeque::new();
    loop {
        let flush = projection::next_flush(&state.lock());
        let mut batch = tokio::select! {
            raw = recv_raw(&mut events) => match raw {
                Some(raw) => ingest(&mut parked, Parked::Event(raw), &mut state.lock(), &session_id),
                None => return,
            },
            request = recv_request(&mut requests) => {
                ingest(&mut parked, Parked::Request(request), &mut state.lock(), &session_id)
            }
            _ = registered.notified() => vec![],
            _ = async {
                match flush {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => projection::flush_due(&mut state.lock(), tokio::time::Instant::now()),
        };
        batch.extend(drain_parked(&mut parked, &mut state.lock(), &session_id));
        for event in batch {
            if out.send(event).is_err() {
                return;
            }
        }
    }
}

fn thread_from(
    response: &Value,
    instance_id: &ProviderInstanceId,
    session_id: &ProviderSessionId,
    app_thread_id: ThreadId,
    owner_node_id: Option<turin_contracts::NodeId>,
    forked_from: Option<OrchestrationV2ProviderThreadForkedFrom>,
) -> Result<OrchestrationV2ProviderThread, AppServerError> {
    let thread = response.get("thread").unwrap_or(response);
    let id =
        thread
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| AppServerError::InvalidPayload {
                method: "thread/start".into(),
                detail: "missing thread.id".into(),
            })?;
    Ok(OrchestrationV2ProviderThread {
        id: derive::provider_thread(&driver(), Some(instance_id), id),
        driver: driver(),
        provider_instance_id: instance_id.clone(),
        provider_session_id: Some(session_id.clone()),
        app_thread_id: Some(app_thread_id),
        owner_node_id,
        native_thread_ref: Some(native_ref(id)),
        native_conversation_head_ref: None,
        status: OrchestrationV2ProviderThreadStatus::Idle,
        first_run_ordinal: None,
        last_run_ordinal: None,
        handoff_ids: vec![],
        forked_from,
        pending_background_tasks: vec![],
        context_usage: None,
        native_metadata: None,
        goal: None,
        created_at: time(thread.get("createdAt")),
        updated_at: time(thread.get("updatedAt")),
    })
}

fn runtime_policy(
    policy: &turin_provider_core::provider_adapter::ProviderAdapterV2RuntimePolicy,
    fallback: &Path,
) -> RuntimePolicy {
    RuntimePolicy {
        cwd: policy
            .cwd
            .clone()
            .unwrap_or_else(|| fallback.to_string_lossy().into_owned()),
        runtime_mode: policy.runtime_mode,
        interaction_mode: policy.interaction_mode,
        approval_policy: policy.approval_policy.clone(),
        sandbox_policy: policy.sandbox_policy.clone(),
    }
}
fn option<'a>(selection: &'a ModelSelection, key: &str) -> Option<&'a str> {
    selection
        .options
        .as_ref()?
        .iter()
        .find(|o| o.id.as_str() == key)
        .and_then(|o| match &o.value {
            ProviderOptionSelectionValue::Str(s) => Some(s.as_str()),
            _ => None,
        })
}
fn turn_input(text: &str) -> Vec<Value> {
    if text.is_empty() {
        vec![]
    } else {
        vec![json!({"type":"text","text":text})]
    }
}
/// Upstream `isTerminalProviderTurn`: a turn with a native ref that has stopped.
fn is_terminal_turn(turn: &OrchestrationV2ProviderTurn) -> bool {
    turn.native_turn_ref.is_some()
        && matches!(
            turn.status,
            OrchestrationV2ProviderTurnStatus::Completed
                | OrchestrationV2ProviderTurnStatus::Interrupted
                | OrchestrationV2ProviderTurnStatus::Failed
                | OrchestrationV2ProviderTurnStatus::Cancelled
        )
}

/// Upstream `resolveCodexRollbackTurnCount`: the stopped turns a rollback reverts.
/// A turn boundary reverts the terminal turns after it, and the boundary must be
/// in the thread's own history.
fn rollback_turn_count(input: &ProviderAdapterV2RollbackThreadInput) -> Result<usize, String> {
    let turns = &input.provider_thread_turns;
    match &input.target {
        ProviderAdapterV2RollbackTarget::ThreadStart { .. } => {
            Ok(turns.iter().filter(|turn| is_terminal_turn(turn)).count())
        }
        ProviderAdapterV2RollbackTarget::ProviderTurn { provider_turn, .. } => {
            if provider_turn.provider_thread_id != input.provider_thread.id {
                return Err(format!(
                    "Cannot roll back Codex thread {} to provider turn {}: target turn belongs to provider thread {}.",
                    input.provider_thread.id, provider_turn.id, provider_turn.provider_thread_id,
                ));
            }
            let Some(boundary) = turns.iter().find(|turn| turn.id == provider_turn.id) else {
                return Err(format!(
                    "Cannot roll back Codex thread {} to provider turn {}: target turn was not found in durable provider turn history.",
                    input.provider_thread.id, provider_turn.id,
                ));
            };
            Ok(turns
                .iter()
                .filter(|turn| turn.ordinal > boundary.ordinal && is_terminal_turn(turn))
                .count())
        }
    }
}

/// Upstream `resolveCodexForkBoundary`. A native turn boundary forks at that turn:
/// `lastTurnId` is inclusive on the Codex side, so later turns are omitted. A
/// boundary without a native ref needs fork-then-revert, which is not ported, so
/// it is refused rather than forking the whole thread. Upstream's
/// `resolveCodexForkRollbackTurnCount` errors for a boundary the source thread does
/// not have.
fn fork_last_turn(input: &ProviderAdapterV2ForkThreadInput) -> Result<Option<String>, String> {
    let (Some(target), Some(turns)) = (&input.provider_turn_id, &input.source_provider_turns)
    else {
        return Ok(None);
    };
    let source_turns: Vec<&OrchestrationV2ProviderTurn> = turns
        .iter()
        .filter(|turn| turn.provider_thread_id == input.source_provider_thread.id)
        .collect();
    let Some(boundary) = source_turns.iter().find(|turn| turn.id == *target) else {
        return Err(format!(
            "Cannot fork Codex thread from provider turn {target}: source turn was not found in provider thread {}.",
            input.source_provider_thread.id,
        ));
    };
    if let Some(native) = boundary
        .native_turn_ref
        .as_ref()
        .and_then(|r| r.native_id.as_ref())
    {
        return Ok(Some(native.as_str().to_owned()));
    }
    let later = source_turns
        .iter()
        .filter(|turn| turn.ordinal > boundary.ordinal && is_terminal_turn(turn))
        .count();
    if later == 0 {
        return Ok(None);
    }
    Err(format!(
        "Cannot fork Codex thread from provider turn {target}: the turn has no native reference and {later} turn(s) follow it. Fork-then-rollback is not ported."
    ))
}

fn snapshot(
    thread: OrchestrationV2ProviderThread,
    payload: Option<Value>,
) -> ProviderAdapterV2ThreadSnapshot {
    ProviderAdapterV2ThreadSnapshot {
        provider_thread: thread,
        provider_turns: vec![],
        messages: vec![],
        runtime_requests: vec![],
        provider_payload: payload,
    }
}

#[async_trait]
impl ProviderSessionRuntime for CodexProviderRuntime {
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
        &self.provider_session
    }
    fn events(&self) -> ProviderEventStream {
        let Some(events) = self.events.lock().take() else {
            return Box::pin(stream::empty());
        };
        Box::pin(stream::unfold(events, |mut rx| async move {
            rx.recv().await.map(|event| (Ok(event), rx))
        }))
    }
    fn can_reuse_context_usage(
        &self,
        previous: &ModelSelection,
        next: &ModelSelection,
    ) -> Option<bool> {
        Some(
            previous.instance_id == next.instance_id
                && previous.model == next.model
                && previous
                    .options
                    .as_ref()
                    .into_iter()
                    .flatten()
                    .filter(|o| o.id.as_str() != "reasoningEffort")
                    .eq(next
                        .options
                        .as_ref()
                        .into_iter()
                        .flatten()
                        .filter(|o| o.id.as_str() != "reasoningEffort")),
        )
    }
    async fn ensure_thread(
        &self,
        input: ProviderAdapterV2EnsureThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        let cwd = input
            .runtime_policy
            .cwd
            .as_deref()
            .unwrap_or_else(|| self.config.cwd.to_str().unwrap_or("."));
        let response = self
            .session
            .start_thread(cwd, input.model_selection.model.as_str())
            .await
            .map_err(|e| {
                error(
                    ProviderAdapterErrorKind::EnsureThread {
                        driver: self.driver.clone(),
                        thread_id: input.thread_id.clone(),
                    },
                    e,
                )
            })?;
        thread_from(
            &response,
            &self.instance_id,
            &self.provider_session_id,
            input.thread_id.clone(),
            None,
            None,
        )
        .map_err(|e| {
            error(
                ProviderAdapterErrorKind::EnsureThread {
                    driver: self.driver.clone(),
                    thread_id: input.thread_id,
                },
                e,
            )
        })
    }
    async fn resume_thread(
        &self,
        input: ProviderAdapterV2ResumeThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        let native = native_id(&input.provider_thread)?.to_owned();
        let cwd = input.runtime_policy.as_ref().and_then(|p| p.cwd.as_deref());
        let model = input.model_selection.as_ref().map(|m| m.model.as_str());
        let response = self
            .session
            .resume_thread(&native, cwd, model)
            .await
            .map_err(|e| {
                error(
                    ProviderAdapterErrorKind::ResumeThread {
                        driver: self.driver.clone(),
                        provider_session_id: self.provider_session_id.clone(),
                        provider_thread_id: input.provider_thread.id.clone(),
                    },
                    e,
                )
            })?;
        let mut thread = input.provider_thread;
        thread.provider_session_id = Some(self.provider_session_id.clone());
        thread.provider_instance_id = self.instance_id.clone();
        thread.status = OrchestrationV2ProviderThreadStatus::Idle;
        thread.native_thread_ref = Some(native_ref(&native));
        thread.updated_at = time(response.pointer("/thread/updatedAt"));
        Ok(thread)
    }
    async fn start_turn(
        &self,
        input: ProviderAdapterV2TurnInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        let thread = native_id(&input.provider_thread)?.to_owned();
        let policy = runtime_policy(&input.runtime_policy, &self.config.cwd);
        let model = input.model_selection.model.as_str();
        let native_turn = self
            .session
            .start_turn(
                &thread,
                turn_input(&input.message.text),
                model,
                option(&input.model_selection, "reasoningEffort"),
                option(&input.model_selection, "serviceTier"),
                &policy,
            )
            .await
            .map_err(|e| {
                error(
                    ProviderAdapterErrorKind::TurnStart {
                        driver: self.driver.clone(),
                        thread_id: input.thread_id.clone(),
                        provider_thread_id: input.provider_thread.id.clone(),
                        run_id: input.run_id.clone(),
                    },
                    e,
                )
            })?;
        self.state
            .lock()
            .turns
            .insert(native_turn.clone(), TurnState::new(native_turn, input));
        self.turn_registered.notify_one();
        Ok(())
    }
    async fn steer_turn(
        &self,
        input: ProviderAdapterV2SteerInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        let thread = native_id(&input.provider_thread)?.to_owned();
        let native_turn = self
            .state
            .lock()
            .turns
            .iter()
            .find(|(id, _)| derive::provider_turn(&self.driver, id) == input.provider_turn_id)
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| input.provider_turn_id.as_str().to_owned());
        self.session
            .steer_turn(&thread, &native_turn, turn_input(&input.message.text))
            .await
            .map_err(|e| {
                error(
                    ProviderAdapterErrorKind::SteerRun {
                        driver: self.driver.clone(),
                        provider_thread_id: input.provider_thread.id,
                        provider_turn_id: input.provider_turn_id,
                    },
                    e,
                )
            })
    }
    async fn interrupt_turn(
        &self,
        input: ProviderAdapterV2InterruptInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        let thread = native_id(&input.provider_thread)?.to_owned();
        let native_turn = self
            .state
            .lock()
            .turns
            .iter()
            .find(|(id, _)| derive::provider_turn(&self.driver, id) == input.provider_turn_id)
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| input.provider_turn_id.as_str().to_owned());
        let interrupt_error = |e| {
            error(
                ProviderAdapterErrorKind::Interrupt {
                    driver: self.driver.clone(),
                    provider_thread_id: input.provider_thread.id.clone(),
                    provider_turn_id: input.provider_turn_id.clone(),
                },
                e,
            )
        };
        self.session
            .interrupt_turn(&thread, &native_turn)
            .await
            .map_err(interrupt_error)?;
        // Commands still running under this turn keep their background terminal
        // alive after the turn stops, so Codex needs an explicit terminate.
        let processes: Vec<String> = self
            .state
            .lock()
            .turns
            .get(&native_turn)
            .map(|turn| {
                turn.running_commands
                    .iter()
                    .filter_map(|command| command.process_id.clone())
                    .collect()
            })
            .unwrap_or_default();
        for process_id in processes {
            // Upstream calls this through the raw client: the method is not in the
            // pinned typed surface.
            self.session
                .client()
                .raw
                .request(
                    "thread/backgroundTerminals/terminate",
                    Some(json!({"threadId": thread, "processId": process_id})),
                )
                .await
                .map_err(interrupt_error)?;
        }
        Ok(())
    }
    async fn respond_to_runtime_request(
        &self,
        input: ProviderAdapterV2RuntimeRequestResponseInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        let Some(broker_id) = self.state.lock().requests.remove(&input.request_id) else {
            return Err(ProviderAdapterV2Error::new(
                ProviderAdapterErrorKind::RuntimeRequestResponse {
                    driver: self.driver.clone(),
                    request_id: input.request_id,
                },
            ));
        };
        let decision = if let Some(answers) = input.answers {
            Decision::Answers(serde_json::to_value(answers).expect("answers"))
        } else {
            let choice = match input.decision {
                Some(ProviderApprovalDecision::Accept) => "accept",
                Some(ProviderApprovalDecision::AcceptForSession) => "acceptForSession",
                Some(ProviderApprovalDecision::AcceptAlways) => "acceptAlways",
                Some(ProviderApprovalDecision::Decline) => "decline",
                Some(ProviderApprovalDecision::Cancel) => "cancel",
                None => {
                    return Err(ProviderAdapterV2Error::new(
                        ProviderAdapterErrorKind::RuntimeRequestResponse {
                            driver: self.driver.clone(),
                            request_id: input.request_id,
                        },
                    ));
                }
            };
            Decision::Approval(choice)
        };
        if self.session.broker.reply(broker_id, decision) {
            Ok(())
        } else {
            Err(ProviderAdapterV2Error::new(
                ProviderAdapterErrorKind::RuntimeRequestResponse {
                    driver: self.driver.clone(),
                    request_id: input.request_id,
                },
            ))
        }
    }
    async fn read_thread_snapshot(
        &self,
        input: ProviderAdapterV2ReadThreadSnapshotInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        let native = native_id(&input.provider_thread)?.to_owned();
        let response = self
            .session
            .client()
            .request(
                "thread/read",
                Some(json!({"threadId":native,"includeTurns":true})),
            )
            .await
            .map_err(|e| {
                error(
                    ProviderAdapterErrorKind::ReadThreadSnapshot {
                        driver: self.driver.clone(),
                        provider_thread_id: input.provider_thread.id.clone(),
                    },
                    e,
                )
            })?
            .unwrap_or(Value::Null);
        let mut thread = input.provider_thread;
        thread.updated_at = time(response.pointer("/thread/updatedAt"));
        Ok(snapshot(thread, response.get("thread").cloned()))
    }
    async fn rollback_thread(
        &self,
        input: ProviderAdapterV2RollbackThreadInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        let native = native_id(&input.provider_thread)?.to_owned();
        let count = rollback_turn_count(&input).map_err(|cause| {
            error(
                ProviderAdapterErrorKind::RollbackThread {
                    driver: self.driver.clone(),
                    provider_thread_id: input.provider_thread.id.clone(),
                    checkpoint_id: None,
                },
                cause,
            )
        })?;
        let head_ref = match &input.target {
            ProviderAdapterV2RollbackTarget::ProviderTurn { provider_turn, .. } => {
                provider_turn.native_turn_ref.clone()
            }
            ProviderAdapterV2RollbackTarget::ThreadStart { .. } => None,
        };
        // Upstream returns the idle thread without a request when nothing reverts.
        if count == 0 {
            let mut thread = input.provider_thread;
            thread.status = OrchestrationV2ProviderThreadStatus::Idle;
            thread.native_conversation_head_ref = head_ref;
            return Ok(snapshot(thread, None));
        }
        let native_thread = self
            .session
            .revert_thread(&native, count as u64)
            .await
            .map_err(|e| {
                error(
                    ProviderAdapterErrorKind::RollbackThread {
                        driver: self.driver.clone(),
                        provider_thread_id: input.provider_thread.id.clone(),
                        checkpoint_id: None,
                    },
                    e,
                )
            })?;
        let mut thread = input.provider_thread;
        thread.status = OrchestrationV2ProviderThreadStatus::Idle;
        thread.updated_at = time(native_thread.get("updatedAt"));
        thread.native_conversation_head_ref = head_ref;
        Ok(snapshot(thread, Some(native_thread)))
    }
    async fn fork_thread(
        &self,
        input: ProviderAdapterV2ForkThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        let native = native_id(&input.source_provider_thread)?.to_owned();
        let mut params = crate::params::thread_runtime_params(
            input.runtime_policy.as_ref().and_then(|p| p.cwd.as_deref()),
            input.model_selection.as_ref().map(|m| m.model.as_str()),
        );
        params["threadId"] = json!(native);
        let last_turn = fork_last_turn(&input).map_err(|cause| {
            error(
                ProviderAdapterErrorKind::ForkThread {
                    driver: self.driver.clone(),
                    provider_thread_id: input.source_provider_thread.id.clone(),
                },
                cause,
            )
        })?;
        if let Some(last_turn) = last_turn {
            params["lastTurnId"] = json!(last_turn);
        }
        let response = self
            .session
            .client()
            .request("thread/fork", Some(params))
            .await
            .map_err(|e| {
                error(
                    ProviderAdapterErrorKind::ForkThread {
                        driver: self.driver.clone(),
                        provider_thread_id: input.source_provider_thread.id.clone(),
                    },
                    e,
                )
            })?
            .unwrap_or(Value::Null);
        thread_from(
            &response,
            &self.instance_id,
            &self.provider_session_id,
            input.target_thread_id,
            input.owner_node_id,
            Some(OrchestrationV2ProviderThreadForkedFrom {
                provider_thread_id: input.source_provider_thread.id.clone(),
                provider_turn_id: input.provider_turn_id,
                checkpoint_id: None,
            }),
        )
        .map_err(|e| {
            error(
                ProviderAdapterErrorKind::ForkThread {
                    driver: self.driver.clone(),
                    provider_thread_id: input.source_provider_thread.id,
                },
                e,
            )
        })
    }
}

/// The runtime request kind for a Codex server request method.
fn runtime_kind(method: &str) -> OrchestrationV2RuntimeRequestKind {
    match method {
        "item/fileChange/requestApproval" | "applyPatchApproval" => {
            OrchestrationV2RuntimeRequestKind::FileChange
        }
        "item/permissions/requestApproval" => OrchestrationV2RuntimeRequestKind::Permission,
        "mcpServer/elicitation/request" => OrchestrationV2RuntimeRequestKind::McpElicitation,
        "item/tool/requestUserInput" => OrchestrationV2RuntimeRequestKind::UserInput,
        _ => OrchestrationV2RuntimeRequestKind::Command,
    }
}

/// The approval kind an approval node is raised for. Plan questions are not
/// approvals, so they have none.
fn approval_kind(method: &str) -> Option<ProviderRequestKind> {
    match runtime_kind(method) {
        OrchestrationV2RuntimeRequestKind::Command => Some(ProviderRequestKind::Command),
        OrchestrationV2RuntimeRequestKind::FileRead => Some(ProviderRequestKind::FileRead),
        OrchestrationV2RuntimeRequestKind::FileChange => Some(ProviderRequestKind::FileChange),
        OrchestrationV2RuntimeRequestKind::Permission => Some(ProviderRequestKind::Permission),
        OrchestrationV2RuntimeRequestKind::McpElicitation => {
            Some(ProviderRequestKind::McpElicitation)
        }
        OrchestrationV2RuntimeRequestKind::UserInput
        | OrchestrationV2RuntimeRequestKind::DynamicToolCall
        | OrchestrationV2RuntimeRequestKind::AuthRefresh => None,
    }
}

/// Upstream `item/tool/requestUserInput`: a plan question is answered on the turn
/// that asked it, with its node and item under that turn's root.
fn project_user_input(
    request: PendingRequest,
    state: &mut RuntimeState,
    session_id: &ProviderSessionId,
) -> Vec<ProviderAdapterV2Event> {
    let Some(native_turn) = request.params.get("turnId").and_then(Value::as_str) else {
        return vec![];
    };
    let Some(turn) = state.turns.get_mut(native_turn) else {
        return vec![];
    };
    let native_item = request
        .params
        .get("itemId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let questions = items::codex_questions(request.params.get("questions").unwrap_or(&Value::Null));
    let artifacts = items::user_input_request(
        turn,
        native_item,
        native_item,
        questions,
        Some(session_id.clone()),
        UtcDateTime::now(),
    );
    let thread_id = turn.input.thread_id.clone();
    state
        .requests
        .insert(artifacts.request.id.clone(), request.id);
    vec![
        ProviderAdapterV2Event::NodeUpdated {
            driver: driver(),
            node: artifacts.node,
        },
        ProviderAdapterV2Event::TurnItemUpdated {
            driver: driver(),
            turn_item: artifacts.turn_item,
        },
        ProviderAdapterV2Event::RuntimeRequestUpdated {
            driver: driver(),
            thread_id: Some(thread_id),
            runtime_request: artifacts.request,
        },
    ]
}

/// Upstream `buildApprovalRequestArtifacts`. A request from a subagent is asked on
/// the top-level turn that owns it, under the subagent that asked, so the user can
/// answer where they can see it. Native subagent threads are hidden.
fn project_request(
    request: PendingRequest,
    state: &mut RuntimeState,
    session_id: &ProviderSessionId,
) -> Vec<ProviderAdapterV2Event> {
    let Some(native_turn) = request.params.get("turnId").and_then(Value::as_str) else {
        return vec![];
    };
    if runtime_kind(&request.method) == OrchestrationV2RuntimeRequestKind::UserInput {
        return project_user_input(request, state, session_id);
    }
    let Some(context) = state.turns.get(native_turn) else {
        return vec![];
    };
    let owner_native = subagent::owner_native_turn(state, context);
    let native_item = request.params.get("itemId").and_then(Value::as_str);
    let native_req = request
        .params
        .get("approvalId")
        .or_else(|| request.params.get("itemId"))
        .and_then(Value::as_str);
    let parent_node = subagent::subagent_node(state, context).unwrap_or_else(|| {
        derive::node_from_provider_item(&driver(), native_item.or(native_req).unwrap_or_default())
    });
    let Some(owner) = state.turns.get_mut(&owner_native) else {
        return vec![];
    };
    let now = UtcDateTime::now();
    let provider_turn_id = derive::provider_turn(&driver(), &owner_native);
    let id = IdAllocator.runtime_request(&driver(), Some(&provider_turn_id), native_req);
    state.requests.insert(id.clone(), request.id);
    let kind = runtime_kind(&request.method);
    let Some(item_kind) = approval_kind(&request.method) else {
        return vec![];
    };
    let ordinal_key = format!(
        "{}:approval:{}",
        native_item.or(native_req).unwrap_or_default(),
        native_req.unwrap_or_default()
    );
    let (ordinal, _) = items::resolve_position(owner, &ordinal_key, None, now);
    let prompt = request
        .params
        .get("reason")
        .or_else(|| request.params.get("command"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let node_id = derive::approval_node(&id);
    let turn_item_id = derive::approval_turn_item(&id);
    let node = OrchestrationV2ExecutionNode {
        id: node_id.clone(),
        thread_id: owner.input.thread_id.clone(),
        run_id: owner.run_id(),
        parent_node_id: Some(parent_node),
        root_node_id: owner.input.root_node_id.clone(),
        kind: OrchestrationV2ExecutionNodeKind::ApprovalRequest,
        status: OrchestrationV2ExecutionNodeStatus::Waiting,
        counts_for_run: false,
        provider_thread_id: Some(owner.input.provider_thread.id.clone()),
        provider_turn_id: Some(provider_turn_id.clone()),
        native_item_ref: native_item.map(items::native_item_ref),
        runtime_request_id: Some(id.clone()),
        checkpoint_scope_id: None,
        started_at: Some(now),
        completed_at: None,
    };
    let turn_item = OrchestrationV2TurnItem::ApprovalRequest {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: turn_item_id,
        thread_id: owner.input.thread_id.clone(),
        run_id: owner.run_id(),
        node_id: Some(node_id.clone()),
        provider_thread_id: Some(owner.input.provider_thread.id.clone()),
        provider_turn_id: Some(provider_turn_id.clone()),
        native_item_ref: native_item.map(items::native_item_ref),
        parent_item_id: None,
        ordinal,
        status: OrchestrationV2TurnItemStatus::Waiting,
        title: None,
        started_at: Some(now),
        completed_at: None,
        updated_at: now,
        request_id: id.clone(),
        request_kind: item_kind,
        prompt,
        app_name: None,
        options: None,
    };
    let runtime_request = OrchestrationV2RuntimeRequest {
        id,
        node_id,
        provider_turn_id: Some(provider_turn_id),
        native_request_ref: native_req.map(native_ref),
        kind,
        status: OrchestrationV2RuntimeRequestStatus::Pending,
        response_capability: OrchestrationV2ResponseCapability::Live {
            provider_session_id: session_id.clone(),
        },
        created_at: now,
        resolved_at: None,
        decision: None,
        answers: None,
    };
    let thread_id = owner.input.thread_id.clone();
    vec![
        ProviderAdapterV2Event::NodeUpdated {
            driver: driver(),
            node,
        },
        ProviderAdapterV2Event::TurnItemUpdated {
            driver: driver(),
            turn_item,
        },
        ProviderAdapterV2Event::RuntimeRequestUpdated {
            driver: driver(),
            thread_id: Some(thread_id),
            runtime_request,
        },
    ]
}
