//! Port of upstream/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts
//! at the provider-core `ProviderAdapterV2` boundary.
use crate::{
    adapter::{CodexAdapter, CodexEvent, CodexSession},
    approvals::{Decision, PendingRequest, codex_file_change_approval_prompt},
    codex_input::{AttachmentPathResolver, to_codex_input},
    codex_mcp_elicitation::describe_mcp_elicitation,
    codex_usage_limits::{CodexRateLimitSnapshot, codex_rate_limits_to_update},
    goal::{self, GoalCommand},
    items,
    params::{RuntimePolicy, TurnContext},
    projection::{self, RuntimeState, TurnState},
    runtime::{CodexRuntimeConfig, CodexRuntimeResolver},
    subagent,
};
use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{collections::VecDeque, path::Path, sync::Arc, time::Duration};
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
    provider_usage_limits::ProviderUsageLimitsUpdate,
};
use turin_provider_core::{
    failure::ProviderFailureCause,
    handoff_budget::history_response_items,
    id_allocator::{IdAllocator, derive},
    provider_adapter::{
        ProviderAdapterErrorKind, ProviderAdapterV2, ProviderAdapterV2AppContext,
        ProviderAdapterV2EnsureThreadInput, ProviderAdapterV2Error, ProviderAdapterV2Event,
        ProviderAdapterV2ForkThreadInput, ProviderAdapterV2HistoricalContext,
        ProviderAdapterV2InterruptInput, ProviderAdapterV2OpenSessionInput,
        ProviderAdapterV2ReadThreadSnapshotInput, ProviderAdapterV2ResumeThreadInput,
        ProviderAdapterV2RollbackTarget, ProviderAdapterV2RollbackThreadInput,
        ProviderAdapterV2RuntimeRequestResponseInput, ProviderAdapterV2SteerInput,
        ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2TurnInput,
        ProviderAdapterV2UploadFeedbackInput, ProviderEventStream, ProviderSessionRuntime,
    },
    rt::BoxFuture,
    selection_transition::{
        ProviderSelectionTransitionInput, ProviderSelectionTransitionPlan,
        turn_scoped_selection_transition,
    },
};
use turin_shared::model::model_selections_equal;

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

/// Receives a runtime rate-limit push (`onUsageLimits`): the update and when it arrived.
pub type UsageLimitsSink = Arc<dyn Fn(ProviderUsageLimitsUpdate, String) + Send + Sync>;

pub struct CodexProviderAdapter {
    instance_id: ProviderInstanceId,
    driver: ProviderDriverKind,
    config: CodexRuntimeConfig,
    injected_client: Mutex<Option<Client>>,
    /// Resolves the launch per session (managed Codex: the signed-in token).
    resolver: Option<Arc<dyn CodexRuntimeResolver>>,
    usage_sink: Option<UsageLimitsSink>,
    attachment_path: Option<AttachmentPathResolver>,
}

impl CodexProviderAdapter {
    pub fn new(instance_id: ProviderInstanceId, config: CodexRuntimeConfig) -> Self {
        Self {
            instance_id,
            driver: driver(),
            config,
            injected_client: Mutex::new(None),
            resolver: None,
            usage_sink: None,
            attachment_path: None,
        }
    }
    /// `resolveAttachmentPath` for turn input: attachment paths in the text and image data URLs.
    pub fn with_attachment_resolver(mut self, resolver: AttachmentPathResolver) -> Self {
        self.attachment_path = Some(resolver);
        self
    }
    /// Upstream's `onUsageLimits` hook: where `account/rateLimits/updated` pushes go.
    pub fn with_usage_sink(mut self, sink: UsageLimitsSink) -> Self {
        self.usage_sink = Some(sink);
        self
    }
    /// Upstream's `resolveRuntime` option: the launch settings come from `resolver`
    /// for every session, with `config` supplying the cwd and client version.
    pub fn with_runtime_resolver(
        instance_id: ProviderInstanceId,
        config: CodexRuntimeConfig,
        resolver: Arc<dyn CodexRuntimeResolver>,
    ) -> Self {
        Self {
            resolver: Some(resolver),
            ..Self::new(instance_id, config)
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
            resolver: None,
            usage_sink: None,
            attachment_path: None,
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
        let open_error = |cause: &dyn std::fmt::Display| {
            error(
                ProviderAdapterErrorKind::OpenSession {
                    driver: self.driver.clone(),
                    provider_session_id: input.provider_session_id.clone(),
                },
                cause,
            )
        };
        let (config, keepalive) = match &self.resolver {
            Some(resolver) => {
                let launch = resolver
                    .resolve(&self.config)
                    .await
                    .map_err(|e| open_error(&e))?;
                (launch.config, Some(launch.keepalive))
            }
            None => (self.config.clone(), None),
        };
        let client = if let Some(client) = self.injected_client.lock().take() {
            client
        } else {
            let home = std::env::var("HOME").unwrap_or_default();
            config.spawn(Path::new(&home)).map_err(|e| {
                error(
                    ProviderAdapterErrorKind::OpenSession {
                        driver: self.driver.clone(),
                        provider_session_id: input.provider_session_id.clone(),
                    },
                    e,
                )
            })?
        };
        let session = CodexAdapter::initialize(client, &config.client_version)
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
        let session = Arc::new(session);
        let state = Arc::new(Mutex::new(RuntimeState::default()));
        let turn_registered = Arc::new(tokio::sync::Notify::new());
        let out = out_tx.clone();
        let pump = tokio::spawn(project_pump(
            session.take_events(),
            session.take_requests(),
            state.clone(),
            session.clone(),
            turn_registered.clone(),
            input.provider_session_id.clone(),
            out_tx,
            self.usage_sink.clone(),
        ));
        let now = UtcDateTime::now();
        let cwd = input
            .runtime_policy
            .cwd
            .as_deref()
            .unwrap_or_else(|| config.cwd.to_str().unwrap_or("."));
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
            out,
            instance_id: self.instance_id.clone(),
            driver: self.driver.clone(),
            provider_session_id: input.provider_session_id,
            provider_session,
            session,
            events: Mutex::new(Some(out_rx)),
            pump,
            turn_registered,
            state,
            config,
            attachment_path: self.attachment_path.clone(),
            // ChatGPT token sharing does not accept service tiers.
            omit_service_tier: self.resolver.is_some(),
            _keepalive: keepalive,
        }))
    }
}

pub struct CodexProviderRuntime {
    /// Sends the events a command projects outside the pump, in stream order.
    out: tokio::sync::mpsc::UnboundedSender<ProviderAdapterV2Event>,
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
    attachment_path: Option<AttachmentPathResolver>,
    /// `omitServiceTier`: a managed (ChatGPT token sharing) session never sends a tier.
    omit_service_tier: bool,
    /// Held for the session: the managed executable lease.
    _keepalive: Option<Box<dyn std::any::Any + Send + Sync>>,
}

impl Drop for CodexProviderRuntime {
    fn drop(&mut self) {
        self.pump.abort();
    }
}

/// `account/rateLimits/updated`: the pushed windows go to the snapshot.
fn push_usage_limits(sink: &UsageLimitsSink, event: &CodexEvent) {
    if event.method != "account/rateLimits/updated" {
        return;
    }
    let Some(snapshot) = event
        .params
        .get("rateLimits")
        .and_then(|value| serde_json::from_value::<CodexRateLimitSnapshot>(value.clone()).ok())
    else {
        return;
    };
    if let Some(update) = codex_rate_limits_to_update(&snapshot) {
        sink(update, UtcDateTime::now().to_string());
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
    // A legacy approval names a conversation. While that thread's turn is still starting,
    // its turn is not registered yet, so the request waits for it.
    if let Parked::Request(request) = item
        && is_legacy_approval(&request.method)
        && let Some(thread) = request.params.get("conversationId").and_then(Value::as_str)
    {
        return active_turn_on_thread(state, thread).is_some()
            || !(state.starting.contains(thread) || state.pending_root_turns.contains_key(thread));
    }
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
                            && (state.pending_root_turns.contains_key(thread)
                                || subagent::opens_child_turn(state, thread, turn))
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

#[allow(clippy::too_many_arguments)]
async fn project_pump(
    mut events: Option<tokio::sync::mpsc::UnboundedReceiver<CodexEvent>>,
    mut requests: Option<tokio::sync::mpsc::UnboundedReceiver<PendingRequest>>,
    state: Arc<Mutex<RuntimeState>>,
    session: Arc<CodexSession>,
    registered: Arc<tokio::sync::Notify>,
    session_id: ProviderSessionId,
    out: tokio::sync::mpsc::UnboundedSender<ProviderAdapterV2Event>,
    usage_sink: Option<UsageLimitsSink>,
) {
    let mut parked = VecDeque::new();
    // Owned by the pump, so ending the session aborts them.
    let mut background = tokio::task::JoinSet::new();
    loop {
        // Reap finished tasks so the set does not grow.
        while background.try_join_next().is_some() {}
        let flush = projection::next_flush(&state.lock());
        let mut batch = tokio::select! {
            raw = recv_raw(&mut events) => match raw {
                Some(raw) => {
                    if let Some(sink) = &usage_sink {
                        push_usage_limits(sink, &raw);
                    }
                    // The callback must not wait on the injection's response, which this
                    // pump reads.
                    if raw.method == "item/completed"
                        && raw.params.pointer("/item/type").and_then(Value::as_str)
                            == Some("contextCompaction")
                        && let Some(thread) = raw.params.get("threadId").and_then(Value::as_str)
                    {
                        let session = session.clone();
                        let thread = thread.to_owned();
                        background.spawn(async move {
                            session.restore_additional_context(&thread).await;
                        });
                    }
                    ingest(&mut parked, Parked::Event(raw), &mut state.lock(), &session_id)
                }
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
        let unanswered = std::mem::take(&mut state.lock().unanswered);
        for (id, decision) in unanswered {
            session.broker.reply(id, decision);
        }
        let interrupts = std::mem::take(&mut state.lock().interrupts);
        for (thread, turn) in interrupts {
            // Upstream interrupts a late child turn and does not wait on it: the
            // interrupt is bounded by a timeout, and its failure is only logged.
            let session = session.clone();
            tokio::spawn(async move {
                match tokio::time::timeout(
                    Duration::from_secs(10),
                    session.interrupt_turn(&thread, &turn),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(cause)) => {
                        tracing::warn!(thread, turn, %cause, "late subagent interrupt failed")
                    }
                    Err(_) => tracing::warn!(thread, turn, "late subagent interrupt timed out"),
                }
            });
        }
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
        // Upstream derives it from the driver and native id alone, so a T3 database imports.
        id: derive::provider_thread(&driver(), None, id),
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
    app_context: Option<&[ProviderAdapterV2AppContext]>,
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
        // Turin has no T3 MCP server (mcp/* is cut), so `hasT3Mcp` is always false:
        // no developer instructions and no T3 orchestration context. The turn still
        // carries the interaction mode (plan sends `collaborationMode`) and the
        // thread's app context.
        turn_context: TurnContext {
            app_context: app_context
                .unwrap_or_default()
                .iter()
                .map(|entry| (entry.key.clone(), entry.text.clone()))
                .collect(),
            ..TurnContext::default()
        },
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

impl CodexProviderRuntime {
    /// `toCodexInput`.
    async fn codex_input(
        &self,
        text: &str,
        attachments: &[turin_contracts::chat_attachment::ChatAttachment],
    ) -> Result<Vec<Value>, crate::codex_input::CodexInputError> {
        to_codex_input(text, attachments, self.attachment_path.as_ref()).await
    }

    fn turn_start_error(
        &self,
        input: &ProviderAdapterV2TurnInput,
        cause: impl std::fmt::Display,
    ) -> ProviderAdapterV2Error {
        error(
            ProviderAdapterErrorKind::TurnStart {
                driver: self.driver.clone(),
                thread_id: input.thread_id.clone(),
                provider_thread_id: input.provider_thread.id.clone(),
                run_id: input.run_id.clone(),
            },
            cause,
        )
    }

    fn send(&self, events: Vec<ProviderAdapterV2Event>) {
        for event in events {
            let _ = self.out.send(event);
        }
    }

    /// Upstream `runGoalCommand`. `/goal` runs through Codex's native goal API, as the
    /// Codex TUI does. Set and resume start the goal's first turn, which carries the
    /// objective, and Codex continues later goal turns itself. The other commands
    /// settle with a reply and start no Codex turn.
    #[allow(clippy::result_large_err)]
    async fn run_goal_command(
        &self,
        input: ProviderAdapterV2TurnInput,
        command: GoalCommand,
    ) -> Result<(), ProviderAdapterV2Error> {
        let thread = native_id(&input.provider_thread)?.to_owned();
        let fail = |cause: AppServerError| {
            error(
                ProviderAdapterErrorKind::TurnStart {
                    driver: self.driver.clone(),
                    thread_id: input.thread_id.clone(),
                    provider_thread_id: input.provider_thread.id.clone(),
                    run_id: input.run_id.clone(),
                },
                cause,
            )
        };
        let goal_of = |response: Option<Value>| {
            response
                .as_ref()
                .and_then(|response| response.get("goal"))
                .filter(|goal| !goal.is_null())
                .and_then(goal::provider_goal_from_codex)
        };
        let client = self.session.client();
        self.state
            .lock()
            .goal_threads
            .insert(thread.clone(), input.provider_thread.clone());
        let current = goal_of(
            client
                .request("thread/goal/get", Some(json!({"threadId": thread})))
                .await
                .map_err(fail)?,
        );
        let events = goal::set_goal(&mut self.state.lock(), &thread, current.clone());
        self.send(events);

        match &command {
            GoalCommand::Show => {
                let reply = current
                    .as_ref()
                    .map_or_else(|| "No goal is set.".to_owned(), goal::describe);
                self.send(goal::reply_events(&input, &reply));
                return Ok(());
            }
            GoalCommand::Clear => {
                let response = client
                    .request("thread/goal/clear", Some(json!({"threadId": thread})))
                    .await
                    .map_err(fail)?;
                let cleared = response
                    .as_ref()
                    .and_then(|response| response.get("cleared"))
                    .and_then(Value::as_bool)
                    == Some(true);
                let events = goal::set_goal(&mut self.state.lock(), &thread, None);
                self.send(events);
                let reply = if cleared {
                    "Goal cleared."
                } else {
                    "No goal to clear."
                };
                self.send(goal::reply_events(&input, reply));
                return Ok(());
            }
            GoalCommand::Pause if current.is_none() => {
                self.send(goal::reply_events(&input, "No goal is set."));
                return Ok(());
            }
            GoalCommand::Resume if current.is_none() => {
                self.send(goal::reply_events(&input, "No goal is set."));
                return Ok(());
            }
            GoalCommand::Pause => {
                let response = client
                    .request(
                        "thread/goal/set",
                        Some(json!({"threadId": thread, "status": "paused"})),
                    )
                    .await
                    .map_err(fail)?;
                let events = goal::set_goal(&mut self.state.lock(), &thread, goal_of(response));
                self.send(events);
                self.send(goal::reply_events(
                    &input,
                    "Goal paused. Send /goal resume to continue.",
                ));
                return Ok(());
            }
            GoalCommand::Set(_) | GoalCommand::Resume => {}
        }

        // Set and resume start the goal's first turn.
        let objective = match &command {
            GoalCommand::Set(objective) => Some(objective.clone()),
            _ => None,
        };
        if let Some(objective) = &objective {
            if current.is_some() {
                client
                    .request("thread/goal/clear", Some(json!({"threadId": thread})))
                    .await
                    .map_err(fail)?;
            }
            // Paused until our turn runs, so Codex does not start one first.
            let response = client
                .request(
                    "thread/goal/set",
                    Some(json!({"threadId": thread, "objective": objective, "status": "paused"})),
                )
                .await
                .map_err(fail)?;
            let events = goal::set_goal(&mut self.state.lock(), &thread, goal_of(response));
            self.send(events);
        }
        // Until the goal is active, a fast first turn holds the run.
        self.state.lock().goal_activations.insert(thread.clone());
        let mut turn_input = input.clone();
        let objective_text = objective.clone();
        if let Some(objective) = objective {
            turn_input.message.text = objective;
        }
        // The first goal turn carries the objective; a resume continues from history.
        let codex_input = match &objective_text {
            Some(objective) => self
                .codex_input(objective, &[])
                .await
                .map_err(|cause| self.turn_start_error(&input, cause))?,
            None => Vec::new(),
        };
        let outcome = async {
            self.start_native_turn(turn_input, codex_input).await?;
            let response = client
                .request(
                    "thread/goal/set",
                    Some(json!({"threadId": thread, "status": "active"})),
                )
                .await
                .map_err(fail)?;
            Ok::<_, ProviderAdapterV2Error>(goal_of(response))
        }
        .await;
        let mut state = self.state.lock();
        state.goal_activations.remove(&thread);
        let active = goal::is_active(&state, &thread);
        let mut events = match &outcome {
            Ok(goal) => goal::set_goal(&mut state, &thread, goal.clone()),
            Err(_) => vec![],
        };
        // A run held for an activation that did not happen settles now.
        if !goal::is_active(&state, &thread) || !active {
            events.extend(goal::release_hold(&mut state, &thread));
        }
        drop(state);
        self.send(events);
        outcome.map(|_| ())
    }
    /// Starts a native Codex turn for this run's configuration. Upstream `startNativeTurn`.
    #[allow(clippy::result_large_err)]
    async fn start_native_turn(
        &self,
        input: ProviderAdapterV2TurnInput,
        codex_input: Vec<Value>,
    ) -> Result<(), ProviderAdapterV2Error> {
        let thread = native_id(&input.provider_thread)?.to_owned();
        let policy = runtime_policy(
            &input.runtime_policy,
            &self.config.cwd,
            input.app_context.as_deref(),
        );
        let model = input.model_selection.model.as_str();
        // The thread is starting a turn until the turn is registered, so a child turn
        // that starts first waits for its parent rather than running under a stale one.
        self.state.lock().starting.insert(thread.clone());
        let started = self
            .session
            .start_turn(
                &thread,
                codex_input,
                model,
                option(&input.model_selection, "reasoningEffort"),
                option(&input.model_selection, "serviceTier").filter(|_| !self.omit_service_tier),
                &policy,
            )
            .await;
        let native_turn = match started {
            Ok(native_turn) => native_turn,
            Err(e) => {
                self.state.lock().starting.remove(&thread);
                return Err(error(
                    ProviderAdapterErrorKind::TurnStart {
                        driver: self.driver.clone(),
                        thread_id: input.thread_id.clone(),
                        provider_thread_id: input.provider_thread.id.clone(),
                        run_id: input.run_id.clone(),
                    },
                    e,
                ));
            }
        };
        {
            let mut state = self.state.lock();
            state.starting.remove(&thread);
            state
                .goal_threads
                .insert(thread.clone(), input.provider_thread.clone());
            state
                .turns
                .insert(native_turn.clone(), TurnState::new(native_turn, input));
        }
        self.turn_registered.notify_one();
        Ok(())
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
    /// `canReuseCodexContextUsage`: the same selection, ignoring the reasoning effort.
    fn can_reuse_context_usage(
        &self,
        previous: &ModelSelection,
        next: &ModelSelection,
    ) -> Option<bool> {
        let without_effort = |selection: &ModelSelection| {
            let mut selection = selection.clone();
            selection.options = Some(
                selection
                    .options
                    .take()
                    .into_iter()
                    .flatten()
                    .filter(|option| option.id.as_str() != "reasoningEffort")
                    .collect(),
            );
            selection
        };
        Some(model_selections_equal(
            &without_effort(previous),
            &without_effort(next),
        ))
    }

    /// Idle release must not stop the app-server while a command, tool or subagent runs.
    fn has_pending_background_work(
        &self,
    ) -> Option<BoxFuture<'_, Result<bool, ProviderAdapterV2Error>>> {
        Some(Box::pin(async move {
            Ok(projection::has_pending_background_work(&self.state.lock()))
        }))
    }

    fn has_pending_background_work_for_thread(
        &self,
        provider_thread: &OrchestrationV2ProviderThread,
    ) -> Option<BoxFuture<'_, Result<bool, ProviderAdapterV2Error>>> {
        let thread = provider_thread.id.as_str().to_owned();
        Some(Box::pin(async move {
            Ok(projection::has_pending_background_work_for_thread(
                &self.state.lock(),
                &thread,
            ))
        }))
    }

    /// `compactThread`: `/compact` runs through `thread/compact/start`. The compaction's
    /// turn is adopted when Codex reports it (`pendingRootTurns`).
    fn compact_thread(
        &self,
        input: ProviderAdapterV2TurnInput,
    ) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        Some(Box::pin(async move {
            let thread = native_id(&input.provider_thread)
                .map_err(|cause| self.turn_start_error(&input, format!("{cause:?}")))?
                .to_owned();
            self.state
                .lock()
                .pending_root_turns
                .insert(thread.clone(), input.clone());
            // Cleared on failure too: an unstarted compaction must not adopt a later turn.
            match self.session.compact_thread(&thread).await {
                Ok(_) => Ok(()),
                Err(cause) => {
                    self.state.lock().pending_root_turns.remove(&thread);
                    Err(self.turn_start_error(&input, cause))
                }
            }
        }))
    }

    /// `injectHistory`: `thread/inject_items`. `false` when the app-server predates it
    /// (-32601); other failures are ambiguous and must not fall through to a second delivery.
    fn inject_history(
        &self,
        provider_thread: &OrchestrationV2ProviderThread,
        context: &ProviderAdapterV2HistoricalContext,
    ) -> Option<BoxFuture<'_, Result<bool, ProviderAdapterV2Error>>> {
        let provider_thread = provider_thread.clone();
        let items = history_response_items(&context.messages, &context.context);
        Some(Box::pin(async move {
            let protocol = |cause: &dyn std::fmt::Display| {
                error(
                    ProviderAdapterErrorKind::Protocol {
                        driver: self.driver.clone(),
                        detail: "Failed to inject historical context".to_owned(),
                        payload: None,
                    },
                    cause,
                )
            };
            let thread = native_id(&provider_thread)
                .map_err(|cause| protocol(&format!("{cause:?}")))?
                .to_owned();
            match self
                .session
                .client()
                .request(
                    "thread/inject_items",
                    Some(json!({"threadId": thread, "items": items})),
                )
                .await
            {
                Ok(_) => Ok(true),
                Err(AppServerError::Request { error, .. }) if error.code == -32601 => Ok(false),
                Err(cause) => Err(protocol(&cause)),
            }
        }))
    }

    /// `unloadThread`: forgets the thread's session approvals and drops this connection's
    /// subscription, so the shared app-server can shut an idle thread down.
    fn unload_thread(
        &self,
        provider_thread: &OrchestrationV2ProviderThread,
    ) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        let provider_thread = provider_thread.clone();
        Some(Box::pin(async move {
            let protocol = |cause: &dyn std::fmt::Display| {
                error(
                    ProviderAdapterErrorKind::Protocol {
                        driver: self.driver.clone(),
                        detail: format!(
                            "Failed to unload Codex thread for provider thread {}",
                            provider_thread.id
                        ),
                        payload: None,
                    },
                    cause,
                )
            };
            let thread = native_id(&provider_thread)
                .map_err(|cause| protocol(&format!("{cause:?}")))?
                .to_owned();
            self.session
                .unload_thread(&thread)
                .await
                .map_err(|cause| protocol(&cause))
        }))
    }

    /// `uploadFeedback`: `/feedback` uploads the thread and Codex logs. Returns the feedback id.
    fn upload_feedback(
        &self,
        input: ProviderAdapterV2UploadFeedbackInput,
    ) -> Option<BoxFuture<'_, Result<String, ProviderAdapterV2Error>>> {
        Some(Box::pin(async move {
            let protocol = |cause: &dyn std::fmt::Display| {
                error(
                    ProviderAdapterErrorKind::Protocol {
                        driver: self.driver.clone(),
                        detail: "Failed to upload Codex thread feedback.".to_owned(),
                        payload: None,
                    },
                    cause,
                )
            };
            let thread = native_id(&input.provider_thread)
                .map_err(|cause| protocol(&format!("{cause:?}")))?
                .to_owned();
            let mut params = json!({
                "classification": "bug",
                "includeLogs": true,
                "threadId": thread,
            });
            if let Some(reason) = input.reason.as_deref().filter(|reason| !reason.is_empty()) {
                params["reason"] = json!(reason);
            }
            let response = self
                .session
                .client()
                .request("feedback/upload", Some(params))
                .await
                .map_err(|cause| protocol(&cause))?;
            response
                .as_ref()
                .and_then(|response| response.get("threadId"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| protocol(&"feedback/upload returned no thread id"))
        }))
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
        // A `/goal` message runs through Codex's goal API, not a turn of its own.
        if input.message.attachments.is_empty()
            && input.restart_continuation_of_run_id.is_none()
            && let Some(command) = goal::parse_command(&input.message.text)
        {
            return self.run_goal_command(input, command).await;
        }
        // A restart continuation resumes from history: it sends no new user input.
        let codex_input = if input.restart_continuation_of_run_id.is_none() {
            self.codex_input(&input.message.text, &input.message.attachments)
                .await
                .map_err(|cause| self.turn_start_error(&input, cause))?
        } else {
            Vec::new()
        };
        self.start_native_turn(input, codex_input).await
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
        let codex_input = self
            .codex_input(&input.message.text, &input.message.attachments)
            .await
            .map_err(|cause| {
                error(
                    ProviderAdapterErrorKind::SteerRun {
                        driver: self.driver.clone(),
                        provider_thread_id: input.provider_thread.id.clone(),
                        provider_turn_id: input.provider_turn_id.clone(),
                    },
                    cause,
                )
            })?;
        self.session
            .steer_turn(&thread, &native_turn, codex_input)
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

/// The runtime request kind for a Codex server request. A permissions request takes
/// the kind of what it asks for (`providerRequestKindFromPermissions`): writes are a
/// file change, reads a file read, anything else a command.
fn runtime_kind(method: &str, params: &Value) -> OrchestrationV2RuntimeRequestKind {
    let asks = |access: &str| {
        params
            .pointer(&format!("/permissions/fileSystem/{access}"))
            .and_then(Value::as_array)
            .is_some_and(|paths| !paths.is_empty())
    };
    match method {
        "item/fileChange/requestApproval" | "applyPatchApproval" => {
            OrchestrationV2RuntimeRequestKind::FileChange
        }
        "item/permissions/requestApproval" if asks("write") => {
            OrchestrationV2RuntimeRequestKind::FileChange
        }
        "item/permissions/requestApproval" if asks("read") => {
            OrchestrationV2RuntimeRequestKind::FileRead
        }
        "item/permissions/requestApproval" => OrchestrationV2RuntimeRequestKind::Command,
        "mcpServer/elicitation/request" => OrchestrationV2RuntimeRequestKind::McpElicitation,
        "item/tool/requestUserInput" => OrchestrationV2RuntimeRequestKind::UserInput,
        _ => OrchestrationV2RuntimeRequestKind::Command,
    }
}

/// The approval kind an approval node is raised for. Plan questions are not
/// approvals, so they have none.
fn approval_kind(method: &str, params: &Value) -> Option<ProviderRequestKind> {
    match runtime_kind(method, params) {
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

fn is_legacy_approval(method: &str) -> bool {
    matches!(method, "execCommandApproval" | "applyPatchApproval")
}

/// `findActiveTurnByNativeThreadId`: the active turn running on a native thread.
fn active_turn_on_thread(state: &RuntimeState, thread: &str) -> Option<String> {
    state
        .turns
        .values()
        .find(|turn| {
            let on = match &turn.subagent {
                Some(child) => Some(child.as_str()),
                None => turn
                    .input
                    .provider_thread
                    .native_thread_ref
                    .as_ref()
                    .and_then(|native| native.native_id.as_ref())
                    .map(|id| id.as_str()),
            };
            on == Some(thread)
        })
        .map(|turn| turn.native_turn.clone())
}

/// The request's prompt: what the user is asked to approve.
fn approval_prompt(method: &str, params: &Value) -> Option<String> {
    let text = |key: &str| params.get(key).and_then(Value::as_str).map(str::to_owned);
    match method {
        // `payload.reason ?? payload.command.join(" ")`
        "execCommandApproval" => text("reason").or_else(|| {
            params
                .get("command")
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
        }),
        "item/fileChange/requestApproval" | "applyPatchApproval" => {
            codex_file_change_approval_prompt(params)
        }
        // `payload.reason ?? payload.command`
        _ => text("reason").or_else(|| text("command")),
    }
}

/// A request nobody can show is answered at once, so Codex does not wait on it: an
/// elicitation is declined, anything else is a protocol error.
fn reject_unanswered(state: &mut RuntimeState, request: &PendingRequest) {
    let param = |key: &str| {
        request
            .params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
    };
    let decision = match request.method.as_str() {
        "mcpServer/elicitation/request" => {
            tracing::warn!(
                server_name = param("serverName"),
                "Declined an MCP elicitation without an active Codex turn context."
            );
            Decision::Approval("decline")
        }
        "execCommandApproval" => Decision::Reject(format!(
            "No active Codex turn context for exec approval thread {}.",
            param("conversationId")
        )),
        "applyPatchApproval" => Decision::Reject(format!(
            "No active Codex turn context for apply patch approval thread {}.",
            param("conversationId")
        )),
        "item/fileChange/requestApproval" => Decision::Reject(format!(
            "No active Codex turn context for file change approval turn {}.",
            param("turnId")
        )),
        "item/permissions/requestApproval" => Decision::Reject(format!(
            "No active Codex turn context for permissions approval turn {}.",
            param("turnId")
        )),
        "item/tool/requestUserInput" => Decision::Reject(format!(
            "No active Codex turn context for user input request turn {}.",
            param("turnId")
        )),
        _ => Decision::Reject(format!(
            "No active Codex turn context for approval turn {}.",
            param("turnId")
        )),
    };
    state.unanswered.push((request.id, decision));
}

/// Upstream `item/tool/requestUserInput`: a plan question is answered on the turn
/// that asked it, with its node and item under that turn's root.
fn project_user_input(
    request: PendingRequest,
    state: &mut RuntimeState,
    session_id: &ProviderSessionId,
) -> Vec<ProviderAdapterV2Event> {
    let native_turn = request.params.get("turnId").and_then(Value::as_str);
    let Some(turn) = native_turn.and_then(|native_turn| state.turns.get_mut(native_turn)) else {
        reject_unanswered(state, &request);
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
    if runtime_kind(&request.method, &request.params)
        == OrchestrationV2RuntimeRequestKind::UserInput
    {
        return project_user_input(request, state, session_id);
    }
    // The legacy approvals name a conversation, not a turn: the active turn on that thread.
    let native_turn = if is_legacy_approval(&request.method) {
        request
            .params
            .get("conversationId")
            .and_then(Value::as_str)
            .and_then(|thread| active_turn_on_thread(state, thread))
    } else {
        request
            .params
            .get("turnId")
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let Some(context) = native_turn
        .as_deref()
        .and_then(|native_turn| state.turns.get(native_turn))
    else {
        reject_unanswered(state, &request);
        return vec![];
    };
    let owner_native = subagent::owner_native_turn(state, context);
    // An MCP elicitation names no item or approval: upstream keys it by the
    // elicitation id (url mode) or by its server.
    let elicitation_request = (request.method == "mcpServer/elicitation/request").then(|| {
        if request.params.get("mode").and_then(Value::as_str) == Some("url") {
            request
                .params
                .get("elicitationId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        } else {
            format!(
                "mcp-elicitation:{}",
                request
                    .params
                    .get("serverName")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            )
        }
    });
    let param = |key: &str| request.params.get(key).and_then(Value::as_str);
    // Legacy approvals name the item `callId`; their request is the approval, or the call.
    let native_item = elicitation_request
        .as_deref()
        .or_else(|| param("itemId").or_else(|| param("callId")));
    let native_req = elicitation_request.as_deref().or_else(|| {
        param("approvalId")
            .or_else(|| param("itemId"))
            .or_else(|| param("callId"))
    });
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
    let kind = runtime_kind(&request.method, &request.params);
    let Some(item_kind) = approval_kind(&request.method, &request.params) else {
        return vec![];
    };
    let ordinal_key = format!(
        "{}:approval:{}",
        native_item.or(native_req).unwrap_or_default(),
        native_req.unwrap_or_default()
    );
    let (ordinal, _) = items::resolve_position(owner, &ordinal_key, None, now);
    let (prompt, app_name, options) = if elicitation_request.is_some() {
        let (app_name, options) = describe_mcp_elicitation(
            &request.params,
            request
                .params
                .get("serverName")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
        (
            request
                .params
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned),
            Some(app_name),
            Some(options),
        )
    } else {
        (
            approval_prompt(&request.method, &request.params),
            None,
            None,
        )
    };
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
        app_name,
        options,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `providerRequestKindFromPermissions`.
    #[test]
    fn a_permissions_request_takes_the_kind_of_what_it_asks_for() {
        let kind = |permissions: Value| {
            runtime_kind(
                "item/permissions/requestApproval",
                &json!({"permissions": permissions}),
            )
        };
        assert_eq!(
            kind(json!({"fileSystem": {"write": ["/w"], "read": ["/r"]}})),
            OrchestrationV2RuntimeRequestKind::FileChange
        );
        assert_eq!(
            kind(json!({"fileSystem": {"read": ["/r"]}})),
            OrchestrationV2RuntimeRequestKind::FileRead
        );
        assert_eq!(
            kind(json!({"fileSystem": {"write": [], "read": []}})),
            OrchestrationV2RuntimeRequestKind::Command
        );
        assert_eq!(
            kind(json!({"network": {"enabled": true}})),
            OrchestrationV2RuntimeRequestKind::Command
        );
        assert_eq!(
            approval_kind(
                "item/permissions/requestApproval",
                &json!({"permissions": {"fileSystem": {"read": ["/r"]}}})
            ),
            Some(ProviderRequestKind::FileRead)
        );
    }
}
