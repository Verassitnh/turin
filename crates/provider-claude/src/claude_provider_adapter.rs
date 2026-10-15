//! Port of upstream/apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts provider-core boundary.
// `ProviderAdapterV2Error` is the provider-core trait's error type; boxing it
// here would only diverge from the trait.
#![allow(clippy::result_large_err)]
use crate::claude_adapter_v2::project_frame;
use crate::claude_background_tasks::{
    Roster, is_opaque_task_type, parse_roster_entry, pending_task, task_id as roster_task_id,
};
use crate::claude_goal::next_claude_goal;
use crate::claude_model_catalog::resolve_claude_catalog_context_window_tokens;
use crate::claude_tool_titles::{ToolPresentation, fallback_presentation, presentations_from_meta};
use crate::claude_wake::{
    PromptEchoMode, WakeEvidence, WakeState, echoed_prompt_uuids, frame_tool_use_ids,
    is_gated_frame, is_result_for_other_turn, is_task_notification, is_task_started, is_turn_start,
    prompt_lifecycle, summary_of, task_id_of, tool_use_ids,
};
use crate::{
    claude_adapter_v2::{ClaudeAdapterEvent, ClaudeAdapterV2, ClaudeHostRequest},
    claude_model_catalog::ClaudeModelCatalog,
    claude_model_options::{apply_claude_prompt_effort_prefix, compile_claude_model_selection},
    claude_permission::ApprovalDecision,
    claude_plan_items::{
        PlanBody, PlanState, apply_task_tool, project_plan, task_steps, todo_steps,
    },
    claude_query_options::{ClaudeQueryOptionsInput, make_claude_query_options},
    claude_subagents::{SubagentRegistry, SubagentScope},
    claude_turn_items::{
        ItemOrdinals, ToolCall, TurnScope, assistant_artifacts, compaction_item, item_ordinal,
        reasoning_item, streaming_reasoning_item, tool_call_artifacts,
    },
    claude_turn_token_usage::normalize_claude_turn_token_usage,
    claude_usage_limits::ClaudeUsageLimitHooks,
    claude_workflow_progress::parse_claude_workflow_run_handles,
};
use crate::{
    claude_skill_dispatch::dispatch_content_blocks, claude_skills::user_invocable_skill_names,
};
use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_stream::wrappers::UnboundedReceiverStream;
use turin_contracts::{
    PositiveInt, ProviderSessionId, RunId, RuntimeRequestId, ThreadId, TrimmedNonEmptyString,
    UtcDateTime,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2ConversationMessage, OrchestrationV2CreationSource,
        OrchestrationV2NativeRefStrength, OrchestrationV2NotificationOutcome,
        OrchestrationV2PendingBackgroundTask, OrchestrationV2ProviderCapabilities,
        OrchestrationV2ProviderGoal, OrchestrationV2ProviderGoalStatus, OrchestrationV2ProviderRef,
        OrchestrationV2ProviderSession, OrchestrationV2ProviderSessionStatus,
        OrchestrationV2ProviderThread, OrchestrationV2ProviderThreadDisposition,
        OrchestrationV2ProviderThreadForkedFrom, OrchestrationV2ProviderThreadStatus,
        OrchestrationV2ProviderTurn, OrchestrationV2ProviderTurnStatus,
        OrchestrationV2ResponseCapability, OrchestrationV2RuntimeRequest,
        OrchestrationV2RuntimeRequestKind, OrchestrationV2RuntimeRequestStatus,
    },
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
    provider_policy::{ProviderApprovalDecision, ProviderInteractionMode},
    provider_runtime::RuntimeTurnState,
    settings::ClaudeSettings,
};
use turin_provider_core::{
    failure::{ProviderFailureInput, make_provider_failure},
    id_allocator::{IdAllocator, derive},
    notification::{
        BackgroundWork, BackgroundWorkKind, BackgroundWorkReport, background_work_notification,
    },
    provider_adapter::{
        ProviderAdapterErrorKind, ProviderAdapterV2, ProviderAdapterV2EnsureThreadInput,
        ProviderAdapterV2Error, ProviderAdapterV2Event, ProviderAdapterV2ForkThreadInput,
        ProviderAdapterV2InterruptInput, ProviderAdapterV2OpenSessionInput,
        ProviderAdapterV2ReadThreadSnapshotInput, ProviderAdapterV2ResumeThreadInput,
        ProviderAdapterV2RollbackTarget, ProviderAdapterV2RollbackThreadInput,
        ProviderAdapterV2RuntimeRequestResponseInput, ProviderAdapterV2SteerInput,
        ProviderAdapterV2StopSubagentInput, ProviderAdapterV2StopTaskInput,
        ProviderAdapterV2SubagentOutcome, ProviderAdapterV2ThreadSnapshot,
        ProviderAdapterV2TurnInput, ProviderAdapterV2TurnTerminalStatus, ProviderEventStream,
        ProviderSessionRuntime,
    },
    provider_continuation_requests::{ProviderContinuationRequest, ProviderContinuationRequests},
    rt::BoxFuture,
    selection_transition::{ProviderSelectionTransitionInput, ProviderSelectionTransitionPlan},
};

pub fn claude_provider_capabilities_v2() -> OrchestrationV2ProviderCapabilities {
    serde_json::from_value(json!({
        "sessions":{"supportsMultipleProviderThreadsPerSession":false,"supportsModelSwitchInSession":true,"supportsProviderSwitchingViaHandoff":true,"supportsRuntimeModeSwitchInSession":false,"pendingRequestsSurviveRestart":false},
        "threads":{"canCreateEmptyThread":true,"canReadThreadSnapshot":false,"canRollbackThread":true,"canForkThread":true,"canForkFromTurn":true,"canForkFromSubagentThread":false,"exposesNativeThreadId":true},
        "turns":{"exposesNativeTurnId":false,"emitsTurnStarted":true,"emitsTurnCompleted":true,"supportsInterrupt":true,"supportsActiveSteering":true,"activeSteeringInterruptsTools":true,"supportsSteeringByInterruptRestart":false,"supportsQueuedMessages":true,"terminalStatusQuality":"strong"},
        "streaming":{"streamsAssistantText":true,"streamsReasoning":true,"streamsToolOutput":false,"streamsPlanText":false,"emitsMessageCompleted":true},
        "tools":{"exposesToolItemIds":true,"emitsToolStarted":true,"emitsToolCompleted":true,"emitsToolOutput":true,"supportsMcpTools":true,"supportsDynamicToolCallbacks":true},
        "approvals":{"supportsCommandApproval":true,"supportsFileReadApproval":true,"supportsFileChangeApproval":true,"supportsApplyPatchApproval":false,"approvalsHaveNativeRequestIds":true,"approvalCallbacksAreLiveOnly":true,"approvalsCanOriginateFromSubagents":false},
        "planning":{"emitsPlanUpdated":true,"emitsTodoList":true,"emitsProposedPlan":true,"supportsStructuredQuestions":true,"planDeltasHaveItemIds":false},
        "subagents":{"supportsSubagents":true,"exposesSubagentThreadIds":false,"emitsSubagentLifecycle":true,"canWaitForSubagents":false,"canCloseSubagents":false,"canForkSubagentThread":false},
        "context":{"acceptsSystemContext":true,"acceptsDeveloperContext":true,"acceptsSyntheticUserContext":true,"canGenerateSummaries":true,"canConsumeHandoffSummaries":true,"supportsDeltaHandoff":true,"supportsFullThreadHandoff":true,"maxRecommendedHandoffChars":null},
        "checkpointing":{"appCanCheckpointFilesystem":true,"supportsNestedCheckpointScopes":true,"providerCanRollbackConversation":true,"providerRollbackReturnsSnapshot":true,"providerCanReadConversationSnapshot":false},
        "identity":{"nativeThreadIds":"strong","nativeTurnIds":"weak","nativeItemIds":"strong","nativeRequestIds":"strong"},
        "runtimePolicy":{"enforcement":"native"}
    })).expect("Claude capabilities match contracts")
}

fn driver() -> ProviderDriverKind {
    ProviderDriverKind::new("claudeAgent").expect("valid Claude driver")
}
fn tne(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).expect("nonempty provider value")
}
fn protocol_error(detail: impl Into<String>) -> ProviderAdapterV2Error {
    ProviderAdapterV2Error::new(ProviderAdapterErrorKind::Protocol {
        driver: driver(),
        detail: detail.into(),
        payload: None,
    })
}
fn native_id(thread: &OrchestrationV2ProviderThread) -> Result<&str, ProviderAdapterV2Error> {
    thread
        .native_thread_ref
        .as_ref()
        .and_then(|r| r.native_id.as_ref())
        .map(TrimmedNonEmptyString::as_str)
        .ok_or_else(|| {
            protocol_error(format!(
                "Provider thread {} is missing a native Claude session id",
                thread.id
            ))
        })
}
fn weak_native_ref(id: &str) -> OrchestrationV2ProviderRef {
    OrchestrationV2ProviderRef {
        driver: driver(),
        native_id: Some(tne(id)),
        strength: OrchestrationV2NativeRefStrength::Weak,
        fingerprint: None,
        ordinal: None,
    }
}
fn is_synthetic_turn_id(native: &str) -> bool {
    native.starts_with("turn:")
}
fn is_terminal_turn(turn: &OrchestrationV2ProviderTurn) -> bool {
    matches!(
        turn.status,
        OrchestrationV2ProviderTurnStatus::Completed
            | OrchestrationV2ProviderTurnStatus::Interrupted
            | OrchestrationV2ProviderTurnStatus::Failed
            | OrchestrationV2ProviderTurnStatus::Cancelled
    )
}
fn thread_snapshot(
    provider_thread: OrchestrationV2ProviderThread,
) -> ProviderAdapterV2ThreadSnapshot {
    ProviderAdapterV2ThreadSnapshot {
        provider_thread,
        provider_turns: vec![],
        messages: vec![],
        runtime_requests: vec![],
        provider_payload: None,
    }
}
/// Upstream `getNativeConversationHeadId`: the cursor a rollback recorded.
fn head_native_id(
    thread: &OrchestrationV2ProviderThread,
) -> Result<Option<String>, ProviderAdapterV2Error> {
    let Some(head) = thread.native_conversation_head_ref.as_ref() else {
        return Ok(None);
    };
    if head.driver != driver() {
        return Err(protocol_error(format!(
            "Provider thread {} has a non-Claude native conversation head reference.",
            thread.id
        )));
    }
    head.native_id
        .as_ref()
        .map(|id| Some(id.as_str().to_owned()))
        .ok_or_else(|| {
            protocol_error(format!(
                "Provider thread {} has a Claude native conversation head reference without a native id.",
                thread.id
            ))
        })
}
/// Upstream `resolveClaudeRollbackResumeSessionAt` for a provider turn target.
/// A turn with no recorded cursor can only be the head when nothing ran after it.
fn rollback_resume_cursor(
    input: &ProviderAdapterV2RollbackThreadInput,
    target: &OrchestrationV2ProviderTurn,
) -> Result<Option<String>, ProviderAdapterV2Error> {
    if target.provider_thread_id != input.provider_thread.id {
        return Err(protocol_error(format!(
            "Cannot roll back Claude thread {} to provider turn {}: target turn belongs to provider thread {}.",
            input.provider_thread.id, target.id, target.provider_thread_id
        )));
    }
    if let Some(native) = target
        .native_turn_ref
        .as_ref()
        .filter(|r| r.driver == driver())
        .and_then(|r| r.native_id.as_ref())
        .map(TrimmedNonEmptyString::as_str)
        .filter(|native| !is_synthetic_turn_id(native))
    {
        return Ok(Some(native.to_owned()));
    }
    let later_terminal = input
        .provider_thread_turns
        .iter()
        .any(|turn| turn.ordinal > target.ordinal && is_terminal_turn(turn));
    if later_terminal {
        return Err(protocol_error(format!(
            "Cannot roll back Claude thread {} to provider turn {}: no SDK assistant message cursor was recorded for that turn.",
            input.provider_thread.id, target.id
        )));
    }
    Ok(None)
}
/// Upstream `resolveClaudeForkUpToMessageId`: the cursor of the fork boundary.
fn fork_up_to_message_id(
    input: &ProviderAdapterV2ForkThreadInput,
) -> Result<Option<String>, ProviderAdapterV2Error> {
    let (Some(turn_id), Some(turns)) = (&input.provider_turn_id, &input.source_provider_turns)
    else {
        return Ok(None);
    };
    let mut source: Vec<&OrchestrationV2ProviderTurn> = turns
        .iter()
        .filter(|turn| turn.provider_thread_id == input.source_provider_thread.id)
        .collect();
    source.sort_by_key(|turn| turn.ordinal);
    let Some(boundary) = source.iter().position(|turn| &turn.id == turn_id) else {
        return Err(protocol_error(format!(
            "Cannot fork Claude thread from provider turn {turn_id}: source turn was not found in provider thread {}.",
            input.source_provider_thread.id
        )));
    };
    if let Some(native) = source[boundary]
        .native_turn_ref
        .as_ref()
        .and_then(|r| r.native_id.as_ref())
        .map(TrimmedNonEmptyString::as_str)
        .filter(|native| !is_synthetic_turn_id(native))
    {
        return Ok(Some(native.to_owned()));
    }
    if source[boundary + 1..]
        .iter()
        .any(|turn| is_terminal_turn(turn))
    {
        return Err(protocol_error(format!(
            "Cannot fork Claude thread from prior provider turn {turn_id}: no SDK assistant message cursor was recorded for that turn."
        )));
    }
    Ok(None)
}
fn native_ref(id: &str) -> OrchestrationV2ProviderRef {
    OrchestrationV2ProviderRef {
        driver: driver(),
        native_id: Some(tne(id)),
        strength: OrchestrationV2NativeRefStrength::Strong,
        fingerprint: None,
        ordinal: None,
    }
}
fn make_thread(
    instance_id: &ProviderInstanceId,
    session_id: &ProviderSessionId,
    app_thread_id: ThreadId,
    native: &str,
    owner_node_id: Option<turin_contracts::NodeId>,
    forked_from: Option<OrchestrationV2ProviderThreadForkedFrom>,
) -> OrchestrationV2ProviderThread {
    let now = UtcDateTime::now();
    OrchestrationV2ProviderThread {
        id: derive::provider_thread(&driver(), Some(instance_id), native),
        driver: driver(),
        provider_instance_id: instance_id.clone(),
        provider_session_id: Some(session_id.clone()),
        app_thread_id: Some(app_thread_id),
        owner_node_id,
        native_thread_ref: Some(native_ref(native)),
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
        created_at: now,
        updated_at: now,
    }
}

pub struct ClaudeProviderAdapter {
    /// Offers for wake turns. Dropped unless the host supplies a queue.
    continuation_requests: Arc<ProviderContinuationRequests>,
    instance_id: ProviderInstanceId,
    driver: ProviderDriverKind,
    settings: ClaudeSettings,
    catalog: ClaudeModelCatalog,
    environment: Option<std::collections::BTreeMap<String, String>>,
    attachments_dir: Option<PathBuf>,
    runtime_instructions: String,
    /// `$HOME` for the skill config dir fallback (`~/.claude`).
    home_directory: String,
    usage_limit_hooks: Option<ClaudeUsageLimitHooks>,
}
impl ClaudeProviderAdapter {
    /// Reports `rate_limit_event` frames through `hooks` (upstream `onUsageLimits`).
    pub fn with_usage_limit_hooks(mut self, hooks: ClaudeUsageLimitHooks) -> Self {
        self.usage_limit_hooks = Some(hooks);
        self
    }

    pub fn new(
        instance_id: ProviderInstanceId,
        settings: ClaudeSettings,
        catalog: ClaudeModelCatalog,
        environment: Option<std::collections::BTreeMap<String, String>>,
        attachments_dir: Option<PathBuf>,
        runtime_instructions: String,
        home_directory: String,
    ) -> Self {
        Self {
            continuation_requests: Arc::new(ProviderContinuationRequests::dropping()),
            instance_id,
            driver: driver(),
            settings,
            catalog,
            environment,
            attachments_dir,
            runtime_instructions,
            home_directory,
            usage_limit_hooks: None,
        }
    }
}

impl ClaudeProviderAdapter {
    /// Sends wake-turn offers to `requests`, which the host drains.
    pub fn with_continuation_requests(
        mut self,
        requests: Arc<ProviderContinuationRequests>,
    ) -> Self {
        self.continuation_requests = requests;
        self
    }
}

#[async_trait]
impl ProviderAdapterV2 for ClaudeProviderAdapter {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.instance_id
    }
    fn driver(&self) -> &ProviderDriverKind {
        &self.driver
    }
    async fn get_capabilities(
        &self,
    ) -> Result<OrchestrationV2ProviderCapabilities, ProviderAdapterV2Error> {
        Ok(claude_provider_capabilities_v2())
    }
    async fn plan_selection_transition(
        &self,
        _input: ProviderSelectionTransitionInput,
    ) -> Result<ProviderSelectionTransitionPlan, ProviderAdapterV2Error> {
        Ok(ProviderSelectionTransitionPlan::ApplyOnNextTurn)
    }
    async fn open_session(
        &self,
        input: ProviderAdapterV2OpenSessionInput,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderAdapterV2Error> {
        let now = UtcDateTime::now();
        let cwd = input.runtime_policy.cwd.clone().unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
        let session = OrchestrationV2ProviderSession {
            id: input.provider_session_id.clone(),
            driver: self.driver.clone(),
            provider_instance_id: self.instance_id.clone(),
            status: OrchestrationV2ProviderSessionStatus::Ready,
            cwd: tne(&cwd),
            model: Some(input.model_selection.model.clone()),
            capabilities: claude_provider_capabilities_v2(),
            created_at: now,
            updated_at: now,
            last_error: None,
        };
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (events, event_rx) = mpsc::unbounded_channel();
        let actor = ClaudeActor {
            instance_id: self.instance_id.clone(),
            session_id: input.provider_session_id.clone(),
            settings: self.settings.clone(),
            catalog: self.catalog.clone(),
            environment: self.environment.clone(),
            attachments_dir: self.attachments_dir.clone(),
            runtime_instructions: self.runtime_instructions.clone(),
            home_directory: self.home_directory.clone(),
            usage_limit_hooks: self.usage_limit_hooks.clone(),
            selection: input.model_selection,
            claude: None,
            host_requests: None,
            active: None,
            plans: PlanState::default(),
            subagents: SubagentRegistry::default(),
            stop_waiters: HashMap::new(),
            roster: Roster::default(),
            monitor_tool_uses: HashSet::new(),
            monitor_tasks: HashSet::new(),
            last_thread: None,
            goal: None,
            goal_seeded: false,
            echo_mode: PromptEchoMode::Unknown,
            wake: WakeState::default(),
            continuation: Arc::clone(&self.continuation_requests),
            pending_requests: HashMap::new(),
            commands: command_rx,
            events,
        };
        let handle = tokio::spawn(actor.run());
        Ok(Arc::new(ClaudeSessionRuntime {
            instance_id: self.instance_id.clone(),
            driver: self.driver.clone(),
            catalog: self.catalog.clone(),
            session_id: input.provider_session_id,
            session,
            commands,
            event_rx: Mutex::new(Some(event_rx)),
            handle: Mutex::new(Some(handle)),
        }))
    }
}

struct ClaudeSessionRuntime {
    instance_id: ProviderInstanceId,
    driver: ProviderDriverKind,
    /// Resolves a selection's context window (upstream `getModelContextWindow`).
    catalog: ClaudeModelCatalog,
    session_id: ProviderSessionId,
    session: OrchestrationV2ProviderSession,
    commands: mpsc::UnboundedSender<Command>,
    event_rx: Mutex<
        Option<mpsc::UnboundedReceiver<Result<ProviderAdapterV2Event, ProviderAdapterV2Error>>>,
    >,
    handle: Mutex<Option<JoinHandle<()>>>,
}
impl Drop for ClaudeSessionRuntime {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.lock().take() {
            handle.abort();
        }
    }
}

/// Short-lived request messages; boxing the large variants would add an
/// The reply to a `stopSubagent`, held until the task's native outcome arrives.
type StopReply =
    oneshot::Sender<Result<Option<ProviderAdapterV2SubagentOutcome>, ProviderAdapterV2Error>>;

/// Upstream `makeProviderTextDeltaCoalescer({ flushIntervalMs: 50 })`: thinking
/// deltas reach the reasoning item at most once per interval, trailing.
const THINKING_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Upstream `subagentStopOutcomes`: how long a Stop waits for the CLI's outcome.
const SUBAGENT_STOP_CONFIRM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// allocation per call for no lasting size win.
#[allow(clippy::large_enum_variant)]
enum Command {
    Ensure(
        ProviderAdapterV2EnsureThreadInput,
        oneshot::Sender<Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error>>,
    ),
    Resume(
        ProviderAdapterV2ResumeThreadInput,
        oneshot::Sender<Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error>>,
    ),
    Start(
        ProviderAdapterV2TurnInput,
        oneshot::Sender<Result<(), ProviderAdapterV2Error>>,
    ),
    Steer(
        ProviderAdapterV2SteerInput,
        oneshot::Sender<Result<(), ProviderAdapterV2Error>>,
    ),
    Interrupt(
        ProviderAdapterV2InterruptInput,
        oneshot::Sender<Result<(), ProviderAdapterV2Error>>,
    ),
    Respond(
        ProviderAdapterV2RuntimeRequestResponseInput,
        oneshot::Sender<Result<(), ProviderAdapterV2Error>>,
    ),
    StopTask(
        ProviderAdapterV2StopTaskInput,
        oneshot::Sender<Result<(), ProviderAdapterV2Error>>,
    ),
    StopSubagent(
        ProviderAdapterV2StopSubagentInput,
        oneshot::Sender<Result<Option<ProviderAdapterV2SubagentOutcome>, ProviderAdapterV2Error>>,
    ),
    Fork(
        ProviderAdapterV2ForkThreadInput,
        oneshot::Sender<Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error>>,
    ),
    Rollback(
        ProviderAdapterV2RollbackThreadInput,
        oneshot::Sender<Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error>>,
    ),
}
async fn ask<T>(
    commands: &mpsc::UnboundedSender<Command>,
    make: impl FnOnce(oneshot::Sender<Result<T, ProviderAdapterV2Error>>) -> Command,
) -> Result<T, ProviderAdapterV2Error> {
    let (tx, rx) = oneshot::channel();
    commands
        .send(make(tx))
        .map_err(|_| protocol_error("Claude session runtime closed"))?;
    rx.await
        .map_err(|_| protocol_error("Claude session runtime closed"))?
}

#[async_trait]
impl ProviderSessionRuntime for ClaudeSessionRuntime {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.instance_id
    }
    fn driver(&self) -> &ProviderDriverKind {
        &self.driver
    }
    fn provider_session_id(&self) -> &ProviderSessionId {
        &self.session_id
    }
    fn provider_session(&self) -> &OrchestrationV2ProviderSession {
        &self.session
    }
    fn events(&self) -> ProviderEventStream {
        match self.event_rx.lock().take() {
            Some(rx) => Box::pin(UnboundedReceiverStream::new(rx)),
            None => Box::pin(stream::empty()),
        }
    }
    fn background_tasks_end_with_process(&self) -> bool {
        true
    }
    async fn ensure_thread(
        &self,
        input: ProviderAdapterV2EnsureThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        ask(&self.commands, |tx| Command::Ensure(input, tx)).await
    }
    async fn resume_thread(
        &self,
        input: ProviderAdapterV2ResumeThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        ask(&self.commands, |tx| Command::Resume(input, tx)).await
    }
    async fn start_turn(
        &self,
        input: ProviderAdapterV2TurnInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        ask(&self.commands, |tx| Command::Start(input, tx)).await
    }
    async fn steer_turn(
        &self,
        input: ProviderAdapterV2SteerInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        ask(&self.commands, |tx| Command::Steer(input, tx)).await
    }
    async fn interrupt_turn(
        &self,
        input: ProviderAdapterV2InterruptInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        ask(&self.commands, |tx| Command::Interrupt(input, tx)).await
    }
    async fn respond_to_runtime_request(
        &self,
        input: ProviderAdapterV2RuntimeRequestResponseInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        ask(&self.commands, |tx| Command::Respond(input, tx)).await
    }
    async fn read_thread_snapshot(
        &self,
        input: ProviderAdapterV2ReadThreadSnapshotInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        Err(ProviderAdapterV2Error::new(
            ProviderAdapterErrorKind::ReadThreadSnapshot {
                driver: driver(),
                provider_thread_id: input.provider_thread.id,
            },
        ))
    }
    async fn rollback_thread(
        &self,
        input: ProviderAdapterV2RollbackThreadInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        ask(&self.commands, |tx| Command::Rollback(input, tx)).await
    }
    async fn fork_thread(
        &self,
        input: ProviderAdapterV2ForkThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        ask(&self.commands, |tx| Command::Fork(input, tx)).await
    }
    fn get_model_context_window(
        &self,
        model_selection: &ModelSelection,
        _cwd: Option<&str>,
    ) -> Option<Option<u64>> {
        Some(context_window(&self.catalog, model_selection))
    }
    fn stop_task(
        &self,
        input: ProviderAdapterV2StopTaskInput,
    ) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        Some(Box::pin(ask(&self.commands, |tx| {
            Command::StopTask(input, tx)
        })))
    }
    fn stop_subagent(
        &self,
        input: ProviderAdapterV2StopSubagentInput,
    ) -> Option<
        BoxFuture<'_, Result<Option<ProviderAdapterV2SubagentOutcome>, ProviderAdapterV2Error>>,
    > {
        Some(Box::pin(async move {
            match tokio::time::timeout(
                SUBAGENT_STOP_CONFIRM_TIMEOUT,
                ask(&self.commands, |tx| Command::StopSubagent(input, tx)),
            )
            .await
            {
                Ok(outcome) => outcome,
                Err(_) => Err(protocol_error(
                    "Claude subagent Stop was not confirmed by a native outcome.",
                )),
            }
        }))
    }
}

struct ActiveTurn {
    input: ProviderAdapterV2TurnInput,
    turn: OrchestrationV2ProviderTurn,
    thread: OrchestrationV2ProviderThread,
    earlier_usage: Vec<Value>,
    steer_pending: bool,
    interrupted: bool,
    /// The uuid of the latest assistant message. It is the turn's native ref,
    /// so a later rollback or fork can resume at it.
    native_message_cursor: Option<String>,
    /// Thinking blocks seen per assistant message id, for their item ids.
    thinking_blocks: HashMap<String, u64>,
    /// The message and thinking block the stream is writing now.
    stream: StreamState,
    /// The thinking block the last content block left open.
    last_thinking: Option<LastThinking>,
    last_assistant: Option<OrchestrationV2ConversationMessage>,
    /// A root assistant frame of this turn was checked against an active goal.
    goal_checked: bool,
    /// When the buffered thinking text is next flushed, if any is waiting.
    thinking_flush_at: Option<tokio::time::Instant>,
    /// The uuid this turn's prompt was sent with. `None` for a continuation,
    /// which has no prompt to echo (upstream `promptUuid`).
    prompt_uuid: Option<String>,
    /// The echo has confirmed the prompt, so its root output is no longer gated.
    echo_confirmed: bool,
    prompt_started: bool,
    /// Root frames that arrived before the echo (upstream `gatedFramesBeforeEcho`).
    gated_before_echo: u64,
    /// Root output held for the echo (upstream `heldRootFrames`), in arrival order.
    held: Vec<ClaudeAdapterEvent>,
    scope: TurnScope,
    item_ordinals: ItemOrdinals,
    tool_calls: HashMap<String, TrackedTool>,
}

/// A completed thinking block that the next content block may turn into text.
struct LastThinking {
    message_id: String,
    native: String,
    text: String,
    /// The block before it was also thinking in the same message.
    follows_thinking: bool,
}

/// The streamed `stream_event` view of the current message. Thinking blocks
/// stream into reasoning items, which the completed block then replaces.
#[derive(Default)]
struct StreamState {
    message_id: Option<String>,
    /// Keyed by the stream's block index.
    thinking: HashMap<u64, StreamedThinking>,
    /// Thinking blocks started in this message. Numbers their item ids the way
    /// the completed message does.
    thinking_started: u64,
    /// Blocks whose text grew since the last flush, by the stream's block index.
    dirty: HashSet<u64>,
}
struct StreamedThinking {
    native: String,
    ordinal: u64,
    started_at: UtcDateTime,
    text: String,
}

/// A tool call from its `tool_use` block until its `tool_result`.
struct TrackedTool {
    name: String,
    input: Value,
    presentation: ToolPresentation,
    ordinal: u64,
    started_at: UtcDateTime,
    finished: bool,
}
enum PendingRequest {
    Approval(
        oneshot::Sender<ApprovalDecision>,
        OrchestrationV2RuntimeRequest,
    ),
    Question(
        oneshot::Sender<Option<Value>>,
        OrchestrationV2RuntimeRequest,
    ),
    Dialog(
        oneshot::Sender<Option<Value>>,
        OrchestrationV2RuntimeRequest,
    ),
}
struct ClaudeActor {
    instance_id: ProviderInstanceId,
    session_id: ProviderSessionId,
    settings: ClaudeSettings,
    catalog: ClaudeModelCatalog,
    environment: Option<std::collections::BTreeMap<String, String>>,
    attachments_dir: Option<PathBuf>,
    runtime_instructions: String,
    home_directory: String,
    usage_limit_hooks: Option<ClaudeUsageLimitHooks>,
    selection: ModelSelection,
    claude: Option<ClaudeAdapterV2>,
    host_requests: Option<mpsc::UnboundedReceiver<ClaudeHostRequest>>,
    active: Option<ActiveTurn>,
    plans: PlanState,
    /// Session-scoped: a background subagent reports after its turn has settled.
    subagents: SubagentRegistry,
    /// Stops waiting for the native outcome of a task, by CLI task id.
    stop_waiters: HashMap<String, Vec<StopReply>>,
    /// Background work that is not a subagent (upstream `pendingBackgroundTasksByNativeThread`).
    roster: Roster,
    /// The tool_use ids of Monitor calls, and the task ids they started.
    monitor_tool_uses: HashSet<String>,
    monitor_tasks: HashSet<String>,
    /// The thread as last emitted, for a roster change between turns.
    last_thread: Option<OrchestrationV2ProviderThread>,
    /// The session's `/goal` (upstream `goalsByNativeThread`), seeded once from the thread.
    goal: Option<OrchestrationV2ProviderGoal>,
    goal_seeded: bool,
    /// Upstream `PromptEchoMode` of the live query. Reset when the process is replaced.
    echo_mode: PromptEchoMode,
    /// The thread's wake state (see `claude_wake.rs`).
    wake: WakeState,
    /// Where wake turns are offered (upstream `continuationRequests`).
    continuation: Arc<ProviderContinuationRequests>,
    pending_requests: HashMap<RuntimeRequestId, PendingRequest>,
    commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<Result<ProviderAdapterV2Event, ProviderAdapterV2Error>>,
}

#[allow(clippy::large_enum_variant)]
enum Action {
    Command(Option<Command>),
    Event(Option<ClaudeAdapterEvent>),
    Host(Option<ClaudeHostRequest>),
    Flush,
}
impl ClaudeActor {
    async fn run(mut self) {
        loop {
            let action = tokio::select! {
                command = self.commands.recv() => Action::Command(command),
                event = async { match self.claude.as_mut() { Some(claude) => claude.next_event().await, None => std::future::pending().await } } => Action::Event(event),
                request = async { match self.host_requests.as_mut() { Some(requests) => requests.recv().await, None => std::future::pending().await } } => Action::Host(request),
                () = async { match self.active.as_ref().and_then(|active| active.thinking_flush_at) { Some(at) => tokio::time::sleep_until(at).await, None => std::future::pending().await } } => Action::Flush,
            };
            match action {
                Action::Command(Some(command)) => self.command(command).await,
                Action::Command(None) => break,
                Action::Event(Some(event)) => {
                    self.recover_resumed_subagent(&event).await;
                    self.event(event)
                }
                Action::Event(None) => {
                    self.claude = None;
                    self.host_requests = None;
                    self.fail_active("Claude event stream closed before turn result");
                }
                Action::Host(Some(request)) => self.host_request(request),
                Action::Host(None) => self.host_requests = None,
                Action::Flush => self.flush_thinking(),
            }
        }
        if let Some(mut claude) = self.claude.take() {
            let _ = claude.close().await;
        }
    }
    fn emit(&self, event: ProviderAdapterV2Event) {
        let _ = self.events.send(Ok(event));
    }
    async fn command(&mut self, command: Command) {
        match command {
            Command::Ensure(input, reply) => {
                let native = uuid::Uuid::new_v4().to_string();
                let _ = reply.send(Ok(make_thread(
                    &self.instance_id,
                    &self.session_id,
                    input.thread_id,
                    &native,
                    None,
                    None,
                )));
            }
            Command::Resume(input, reply) => {
                let mut thread = input.provider_thread;
                thread.provider_session_id = Some(self.session_id.clone());
                thread.status = OrchestrationV2ProviderThreadStatus::Idle;
                thread.updated_at = UtcDateTime::now();
                let _ = reply.send(Ok(thread));
            }
            Command::Start(input, reply) => {
                let result = self.start(input).await;
                let _ = reply.send(result);
            }
            Command::Steer(input, reply) => {
                let result = self.steer(input);
                let _ = reply.send(result);
            }
            Command::Interrupt(input, reply) => {
                let result = self.interrupt(input).await;
                let _ = reply.send(result);
            }
            Command::Respond(input, reply) => {
                let result = self.respond(input);
                let _ = reply.send(result);
            }
            Command::StopTask(input, reply) => {
                let result = match self.claude.as_ref() {
                    Some(c) => c
                        .stop_task(&input.task_id)
                        .await
                        .map_err(|e| protocol_error(e.to_string())),
                    None => Err(protocol_error("Claude session is not running")),
                };
                let _ = reply.send(result);
            }
            Command::StopSubagent(input, reply) => self.stop_subagent(input, reply).await,
            Command::Fork(input, reply) => {
                let result = self.fork(input).await;
                let _ = reply.send(result);
            }
            Command::Rollback(input, reply) => {
                let result = self.rollback(input).await;
                let _ = reply.send(result);
            }
        }
    }
    async fn start(
        &mut self,
        input: ProviderAdapterV2TurnInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        if self.active.is_some() {
            return Err(protocol_error("Claude provider turn is still active"));
        }
        let native = native_id(&input.provider_thread)?.to_owned();
        let replace = self
            .claude
            .as_ref()
            .is_none_or(|claude| claude.session_id() != Some(native.as_str()));
        if replace {
            if let Some(mut prior) = self.claude.take() {
                prior
                    .close()
                    .await
                    .map_err(|e| protocol_error(e.to_string()))?;
            }
            let resume_session_at = head_native_id(&input.provider_thread)?;
            // A rollback head resumes the session even if the thread has no
            // persisted turn count.
            let resume = resume_session_at.is_some()
                || input
                    .native_thread_has_turns
                    .unwrap_or(input.provider_turn_ordinal > 1);
            let options = make_claude_query_options(ClaudeQueryOptionsInput {
                model_selection: &input.model_selection,
                catalog: &self.catalog,
                native_thread_id: &native,
                resume,
                resume_session_at: resume_session_at.as_deref(),
                cwd: input.runtime_policy.cwd.as_ref().map(PathBuf::from),
                attachments_dir: self.attachments_dir.clone(),
                settings: Some(&self.settings),
                sdk_settings: None,
                environment: self.environment.clone(),
                permission_mode: Some(
                    if input.runtime_policy.interaction_mode == ProviderInteractionMode::Plan {
                        "plan"
                    } else {
                        "default"
                    },
                ),
                tools: None,
                allowed_tools: vec![],
                disallowed_tools: vec![],
                runtime_instructions: &self.runtime_instructions,
            });
            let mut claude = ClaudeAdapterV2::open(options, true)
                .await
                .map_err(|e| protocol_error(e.to_string()))?;
            self.host_requests = claude.take_requests();
            self.claude = Some(claude);
            // The echo mode is per live query.
            self.echo_mode = PromptEchoMode::Unknown;
            self.selection = input.model_selection.clone();
        } else if self.selection != input.model_selection {
            // Upstream applies a changed selection to the live query.
            let compiled = compile_claude_model_selection(&input.model_selection, &self.catalog);
            self.claude
                .as_ref()
                .ok_or_else(|| protocol_error("Claude session is not running"))?
                .set_model(Some(&compiled.api_model_id))
                .await
                .map_err(|e| protocol_error(e.to_string()))?;
            self.selection = input.model_selection.clone();
        }
        // Upstream: a continuation is offered by the adapter and has no prompt of its own.
        let continuation = input.message.created_by == OrchestrationV2Actor::Agent
            && input.message.creation_source == OrchestrationV2CreationSource::Provider;
        let native_turn_id = format!("turn:{}", input.attempt_id);
        let now = UtcDateTime::now();
        let ordinal = PositiveInt::new(input.provider_turn_ordinal.max(1) as u64)
            .expect("positive turn ordinal");
        let turn = OrchestrationV2ProviderTurn {
            id: derive::provider_turn(&driver(), &native_turn_id),
            provider_thread_id: input.provider_thread.id.clone(),
            node_id: input.root_node_id.clone(),
            run_attempt_id: Some(input.attempt_id.clone()),
            native_turn_ref: Some(OrchestrationV2ProviderRef {
                driver: driver(),
                native_id: Some(tne(&native_turn_id)),
                strength: OrchestrationV2NativeRefStrength::Weak,
                fingerprint: None,
                ordinal: None,
            }),
            ordinal,
            status: OrchestrationV2ProviderTurnStatus::Running,
            started_at: Some(now),
            completed_at: None,
            token_usage: None,
            turn_token_usage: None,
        };
        let mut thread = input.provider_thread.clone();
        thread.status = OrchestrationV2ProviderThreadStatus::Active;
        thread.updated_at = now;
        self.emit(ProviderAdapterV2Event::ProviderTurnUpdated {
            driver: driver(),
            thread_id: Some(input.thread_id.clone()),
            provider_turn: turn.clone(),
        });
        self.emit(ProviderAdapterV2Event::ProviderThreadUpdated {
            driver: driver(),
            provider_thread: thread.clone(),
            retained_native_turn_ids: None,
        });
        self.wake.route = Some((input.thread_id.clone(), input.provider_thread.id.clone()));
        // A continuation has no prompt: it drains what the wake buffered.
        let prompt_uuid = if continuation {
            if self.claude.is_none() {
                return Err(protocol_error("Claude session is not running"));
            }
            None
        } else {
            let prompt_uuid = uuid::Uuid::new_v4().to_string();
            self.wake.start_user_turn();
            let compiled = compile_claude_model_selection(&input.model_selection, &self.catalog);
            let text = apply_claude_prompt_effort_prefix(
                &input.message.text,
                compiled.prompt_effort.as_deref(),
            );
            // A `$skill` mention becomes the trailing `/skill` block Claude Code expands.
            let skill_names = user_invocable_skill_names(
                &self.settings,
                input
                    .runtime_policy
                    .cwd
                    .as_ref()
                    .map(PathBuf::from)
                    .as_deref(),
                &self
                    .environment
                    .as_ref()
                    .map(|environment| {
                        environment
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect()
                    })
                    .unwrap_or_default(),
                &self.home_directory,
            );
            let session = self
                .claude
                .as_mut()
                .ok_or_else(|| protocol_error("Claude session is not running"))?;
            match dispatch_content_blocks(&text, &skill_names) {
                Some(blocks) => session.send_blocks(blocks, Some(prompt_uuid.clone()), None),
                None => session.send_prompt(text, Some(prompt_uuid.clone()), None),
            }
            .map_err(|e| protocol_error(e.to_string()))?;
            Some(prompt_uuid)
        };
        let scope = TurnScope {
            driver: driver(),
            thread_id: input.thread_id.clone(),
            run_id: Some(input.run_id.clone()),
            root_node_id: input.root_node_id.clone(),
            provider_thread_id: input.provider_thread.id.clone(),
            provider_turn_id: turn.id.clone(),
        };
        self.subagents.set_scope(SubagentScope {
            driver: driver(),
            provider_instance_id: self.instance_id.clone(),
            thread_id: input.thread_id.clone(),
            run_id: input.run_id.clone(),
            root_node_id: input.root_node_id.clone(),
            provider_thread_id: input.provider_thread.id.clone(),
            provider_turn_id: turn.id.clone(),
            app_thread: input.app_thread.clone(),
            model_selection: input.model_selection.clone(),
            catalog: self.catalog.clone(),
            default_effort: compile_claude_model_selection(&input.model_selection, &self.catalog)
                .effort,
        });
        if !self.goal_seeded {
            self.goal = input.provider_thread.goal.clone();
            self.goal_seeded = true;
        }
        self.last_thread = Some(thread.clone());
        self.active = Some(ActiveTurn {
            input,
            turn,
            thread,
            earlier_usage: vec![],
            steer_pending: false,
            interrupted: false,
            native_message_cursor: None,
            thinking_blocks: HashMap::new(),
            stream: StreamState::default(),
            last_thinking: None,
            last_assistant: None,
            goal_checked: false,
            thinking_flush_at: None,
            prompt_uuid: prompt_uuid.clone(),
            echo_confirmed: false,
            prompt_started: false,
            gated_before_echo: 0,
            held: Vec::new(),
            scope,
            item_ordinals: ItemOrdinals::new(),
            tool_calls: HashMap::new(),
        });
        if continuation {
            self.drain_wake();
        }
        Ok(())
    }
    /// Upstream `stopSubagent`. A confirmed task answers at once. Otherwise the
    /// CLI is asked to stop it, and the reply waits for the task's native
    /// terminal frame (`resolve_stop_waiters`). The caller bounds the wait.
    async fn stop_subagent(&mut self, input: ProviderAdapterV2StopSubagentInput, reply: StopReply) {
        let task_id = input.native_task_id.clone();
        let Some(state) = self.subagents.stop_state(&task_id) else {
            let _ = reply.send(Err(protocol_error(
                "Claude subagent has no known owning task to confirm its outcome.",
            )));
            return;
        };
        if input.provider_thread.app_thread_id.as_ref() != Some(&state.thread_id) {
            let _ = reply.send(Err(protocol_error(
                "Claude subagent has no known owning task to confirm its outcome.",
            )));
            return;
        }
        // A Stop requested before this run of the task started is stale.
        if let (Some(requested), Some(started)) = (input.requested_at, state.started_at)
            && started > requested
        {
            let _ = reply.send(Ok(None));
            return;
        }
        if let Some(confirmed) = state.confirmed {
            let _ = reply.send(Ok(Some(confirmed)));
            return;
        }
        let Some(claude) = self.claude.as_ref() else {
            let _ = reply.send(Err(protocol_error(
                "Claude subagent has no live owning query with task stopping support.",
            )));
            return;
        };
        match claude.stop_task(&task_id).await {
            Ok(()) => self.stop_waiters.entry(task_id).or_default().push(reply),
            Err(error) => {
                let _ = reply.send(Err(protocol_error(error.to_string())));
            }
        }
    }
    /// Answers the Stop waiters of a task once it has a native outcome.
    fn resolve_stop_waiters(&mut self, task_id: &str) {
        let Some(outcome) = self.subagents.confirmed_outcome(task_id) else {
            return;
        };
        for waiter in self.stop_waiters.remove(task_id).unwrap_or_default() {
            let _ = waiter.send(Ok(Some(outcome.clone())));
        }
    }
    fn steer(&mut self, input: ProviderAdapterV2SteerInput) -> Result<(), ProviderAdapterV2Error> {
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| protocol_error("Claude provider thread has no active turn"))?;
        if active.turn.id != input.provider_turn_id {
            return Err(protocol_error(
                "Claude provider turn is not the active turn",
            ));
        }
        let compiled = compile_claude_model_selection(&active.input.model_selection, &self.catalog);
        let text = apply_claude_prompt_effort_prefix(
            &input.message.text,
            compiled.prompt_effort.as_deref(),
        );
        self.claude
            .as_mut()
            .ok_or_else(|| protocol_error("Claude session is not running"))?
            .steer(text, Some(uuid::Uuid::new_v4().to_string()))
            .map_err(|e| protocol_error(e.to_string()))?;
        active.steer_pending = true;
        Ok(())
    }
    async fn interrupt(
        &mut self,
        input: ProviderAdapterV2InterruptInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| protocol_error("Claude provider thread has no active turn"))?;
        if active.turn.id != input.provider_turn_id {
            return Err(protocol_error(
                "Claude provider turn is not the active turn",
            ));
        }
        active.interrupted = true;
        self.claude
            .as_ref()
            .ok_or_else(|| protocol_error("Claude session is not running"))?
            .interrupt()
            .await
            .map_err(|e| protocol_error(e.to_string()))?;
        Ok(())
    }
    fn respond(
        &mut self,
        input: ProviderAdapterV2RuntimeRequestResponseInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        let pending = self
            .pending_requests
            .remove(&input.request_id)
            .ok_or_else(|| {
                protocol_error(format!(
                    "No pending Claude runtime request {}",
                    input.request_id
                ))
            })?;
        let now = UtcDateTime::now();
        match pending {
            PendingRequest::Approval(reply, mut request) => {
                let decision = input
                    .decision
                    .ok_or_else(|| protocol_error("Claude approval requires a decision"))?;
                let answer = match decision {
                    ProviderApprovalDecision::Accept | ProviderApprovalDecision::AcceptAlways => {
                        ApprovalDecision::Accept
                    }
                    ProviderApprovalDecision::AcceptForSession => {
                        ApprovalDecision::AcceptForSession
                    }
                    ProviderApprovalDecision::Decline => ApprovalDecision::Decline,
                    ProviderApprovalDecision::Cancel => ApprovalDecision::Cancel,
                };
                reply
                    .send(answer)
                    .map_err(|_| protocol_error("Claude approval request expired"))?;
                request.status = OrchestrationV2RuntimeRequestStatus::Resolved;
                request.resolved_at = Some(now);
                request.decision = Some(decision);
                self.emit(ProviderAdapterV2Event::RuntimeRequestUpdated {
                    driver: driver(),
                    thread_id: self.active.as_ref().map(|a| a.input.thread_id.clone()),
                    runtime_request: request,
                });
            }
            PendingRequest::Question(reply, mut request) => {
                let answers = input.answers.unwrap_or_default();
                reply
                    .send(Some(
                        serde_json::to_value(&answers)
                            .map_err(|e| protocol_error(e.to_string()))?,
                    ))
                    .map_err(|_| protocol_error("Claude user question expired"))?;
                request.status = OrchestrationV2RuntimeRequestStatus::Resolved;
                request.resolved_at = Some(now);
                request.answers = Some(answers);
                self.emit(ProviderAdapterV2Event::RuntimeRequestUpdated {
                    driver: driver(),
                    thread_id: self.active.as_ref().map(|a| a.input.thread_id.clone()),
                    runtime_request: request,
                });
            }
            PendingRequest::Dialog(reply, mut request) => {
                reply
                    .send(input.response)
                    .map_err(|_| protocol_error("Claude dialog expired"))?;
                request.status = OrchestrationV2RuntimeRequestStatus::Resolved;
                request.resolved_at = Some(now);
                self.emit(ProviderAdapterV2Event::RuntimeRequestUpdated {
                    driver: driver(),
                    thread_id: self.active.as_ref().map(|a| a.input.thread_id.clone()),
                    runtime_request: request,
                });
            }
        }
        Ok(())
    }
    /// Upstream `rollbackThread`. Rolling back does not rewrite the transcript.
    /// It records the cursor the next turn resumes at, and closes the live
    /// query so that turn reopens with it.
    async fn rollback(
        &mut self,
        input: ProviderAdapterV2RollbackThreadInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        if let Some(active) = self.active.as_ref() {
            return Err(protocol_error(format!(
                "Cannot roll back Claude provider thread {} while provider turn {} is active.",
                input.provider_thread.id, active.turn.id
            )));
        }
        let native = native_id(&input.provider_thread)?.to_owned();
        if self.claude.as_ref().and_then(|c| c.session_id()) == Some(native.as_str()) {
            self.close_live().await?;
        }
        let now = UtcDateTime::now();
        match &input.target {
            ProviderAdapterV2RollbackTarget::ThreadStart { .. } => {
                let app_thread_id =
                    input.provider_thread.app_thread_id.clone().ok_or_else(|| {
                        protocol_error("Claude provider thread has no app thread")
                    })?;
                let fresh = uuid::Uuid::new_v4().to_string();
                let mut thread = make_thread(
                    &self.instance_id,
                    &self.session_id,
                    app_thread_id,
                    &fresh,
                    input.provider_thread.owner_node_id.clone(),
                    input.provider_thread.forked_from.clone(),
                );
                thread.handoff_ids = input.provider_thread.handoff_ids.clone();
                Ok(thread_snapshot(thread))
            }
            ProviderAdapterV2RollbackTarget::ProviderTurn {
                provider_turn,
                app_run_ordinal,
                ..
            } => {
                let resume = rollback_resume_cursor(&input, provider_turn)?;
                let mut thread = input.provider_thread.clone();
                thread.provider_session_id = Some(self.session_id.clone());
                thread.native_conversation_head_ref = resume.as_deref().map(weak_native_ref);
                thread.status = OrchestrationV2ProviderThreadStatus::Idle;
                thread.last_run_ordinal = u64::try_from(*app_run_ordinal)
                    .ok()
                    .and_then(PositiveInt::new);
                thread.updated_at = now;
                Ok(thread_snapshot(thread))
            }
        }
    }
    async fn close_live(&mut self) -> Result<(), ProviderAdapterV2Error> {
        self.host_requests = None;
        if let Some(mut claude) = self.claude.take() {
            claude
                .close()
                .await
                .map_err(|e| protocol_error(e.to_string()))?;
        }
        Ok(())
    }
    async fn fork(
        &mut self,
        input: ProviderAdapterV2ForkThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        if self.active.is_some() {
            return Err(protocol_error(
                "Cannot fork Claude thread during an active turn",
            ));
        }
        let native = native_id(&input.source_provider_thread)?.to_owned();
        let up_to = fork_up_to_message_id(&input)?;
        let forked = if let Some(claude) = self
            .claude
            .as_mut()
            .filter(|c| c.session_id() == Some(native.as_str()))
        {
            let id = claude
                .fork(up_to.as_deref(), None)
                .await
                .map_err(|e| protocol_error(e.to_string()))?;
            self.claude = None;
            self.host_requests = None;
            id
        } else {
            let options = turin_claude_protocol::TranscriptOptions {
                dir: input
                    .runtime_policy
                    .as_ref()
                    .and_then(|p| p.cwd.as_ref())
                    .map(PathBuf::from),
                ..Default::default()
            };
            turin_claude_protocol::fork_session(&native, &options, up_to.as_deref(), None)
                .await
                .map_err(|e| protocol_error(e.to_string()))?
        };
        Ok(make_thread(
            &self.instance_id,
            &self.session_id,
            input.target_thread_id,
            &forked,
            input.owner_node_id,
            Some(OrchestrationV2ProviderThreadForkedFrom {
                provider_thread_id: input.source_provider_thread.id,
                provider_turn_id: input.provider_turn_id,
                checkpoint_id: None,
            }),
        ))
    }
    fn host_request(&mut self, request: ClaudeHostRequest) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        match request {
            ClaudeHostRequest::Approval { request, reply } => {
                self.register_request(
                    request.request_id,
                    request.body,
                    OrchestrationV2RuntimeRequestKind::Command,
                    PendingRequest::Approval(reply, self.placeholder_request(active)),
                );
            }
            ClaudeHostRequest::Question { request, reply } => {
                self.register_request(
                    request.request_id,
                    request.body,
                    OrchestrationV2RuntimeRequestKind::UserInput,
                    PendingRequest::Question(reply, self.placeholder_request(active)),
                );
            }
            ClaudeHostRequest::Dialog { request, reply } => {
                self.register_request(
                    request.request_id,
                    request.body,
                    OrchestrationV2RuntimeRequestKind::UserInput,
                    PendingRequest::Dialog(reply, self.placeholder_request(active)),
                );
            }
            ClaudeHostRequest::Hook { reply, .. } => {
                let _ = reply.send(Some(json!({})));
            }
            ClaudeHostRequest::ProposedPlan {
                tool_use_id,
                markdown,
                ack,
            } => {
                let events = match self.active.as_mut() {
                    Some(active) => project_plan(
                        &mut self.plans,
                        &active.scope,
                        active.input.provider_turn_ordinal,
                        &mut active.item_ordinals,
                        &tool_use_id,
                        PlanBody::ProposedPlan(&markdown),
                    ),
                    None => vec![],
                };
                for event in events {
                    self.emit(event);
                }
                let _ = ack.send(());
            }
        }
    }
    fn placeholder_request(&self, active: &ActiveTurn) -> OrchestrationV2RuntimeRequest {
        OrchestrationV2RuntimeRequest {
            id: RuntimeRequestId::new("placeholder").expect("valid"),
            node_id: active.input.root_node_id.clone(),
            provider_turn_id: Some(active.turn.id.clone()),
            native_request_ref: None,
            kind: OrchestrationV2RuntimeRequestKind::Command,
            status: OrchestrationV2RuntimeRequestStatus::Pending,
            response_capability: OrchestrationV2ResponseCapability::Live {
                provider_session_id: self.session_id.clone(),
            },
            created_at: UtcDateTime::now(),
            resolved_at: None,
            decision: None,
            answers: None,
        }
    }
    fn register_request(
        &mut self,
        native_request_id: String,
        _body: Value,
        kind: OrchestrationV2RuntimeRequestKind,
        mut pending: PendingRequest,
    ) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        let id =
            IdAllocator.runtime_request(&driver(), Some(&active.turn.id), Some(&native_request_id));
        let request = match &mut pending {
            PendingRequest::Approval(_, request)
            | PendingRequest::Question(_, request)
            | PendingRequest::Dialog(_, request) => request,
        };
        request.id = id.clone();
        request.kind = kind;
        request.native_request_ref = Some(native_ref(&native_request_id));
        self.emit(ProviderAdapterV2Event::RuntimeRequestUpdated {
            driver: driver(),
            thread_id: Some(active.input.thread_id.clone()),
            runtime_request: request.clone(),
        });
        self.pending_requests.insert(id, pending);
    }
    /// Upstream `handleSdkMessage`: while the active turn's prompt is unconfirmed,
    /// root output waits for the echo that says which turn it belongs to.
    fn event(&mut self, event: ClaudeAdapterEvent) {
        let gated = match (self.active.as_ref(), event_raw(&event)) {
            (Some(active), Some(raw)) if !active.echo_confirmed => active
                .prompt_uuid
                .clone()
                .map(|prompt_uuid| (prompt_uuid, raw.clone())),
            _ => None,
        };
        match gated {
            Some((prompt_uuid, raw)) => self.gate(&prompt_uuid, &raw, event),
            None => self.route(event),
        }
    }
    /// Routes one event to its handler, without the prompt-echo gate.
    fn route(&mut self, event: ClaudeAdapterEvent) {
        match event {
            ClaudeAdapterEvent::Assistant(frame) => self.assistant(frame),
            ClaudeAdapterEvent::User(frame) => self.user(frame),
            ClaudeAdapterEvent::Compaction(frame) => self.compaction(frame),
            ClaudeAdapterEvent::Background(frame) => self.background(frame),
            ClaudeAdapterEvent::Stream(frame) => self.stream(frame),
            ClaudeAdapterEvent::Result(frame) => self.result(frame),
            ClaudeAdapterEvent::RateLimit(frame) => {
                if let Some(hooks) = &self.usage_limit_hooks {
                    hooks.emit_rate_limit_event(&frame.raw);
                }
                self.rate_limit(frame);
            }
            ClaudeAdapterEvent::Init { frame, .. } => self.init(frame),
            ClaudeAdapterEvent::Error(error) => self.fail_active(&error),
            ClaudeAdapterEvent::Eof => {
                self.fail_active("Claude Code process exited before turn result")
            }
            _ => {}
        }
    }
    /// Upstream `recoverResumedClaudeSubagent`, run before the frame routes: a
    /// task_started that resumes a subagent this process never saw, from a tool
    /// call of the running turn, is recorded from its transcript first. The
    /// transcript names the launch tool call, which the registry keys it by.
    async fn recover_resumed_subagent(&mut self, event: &ClaudeAdapterEvent) {
        let ClaudeAdapterEvent::Background(frame) = event else {
            return;
        };
        let raw = &frame.raw;
        if !is_task_started(raw) {
            return;
        }
        let (Some(task_id), Some(tool_use_id)) = (
            task_id_of(raw),
            raw.get("tool_use_id").and_then(Value::as_str),
        ) else {
            return;
        };
        let in_turn = self
            .active
            .as_ref()
            .is_some_and(|active| active.tool_calls.contains_key(tool_use_id));
        if !in_turn || self.subagents.knows(task_id) {
            return;
        }
        let Some(claude) = self.claude.as_ref() else {
            return;
        };
        let history = claude.subagent_history(task_id).await;
        let launch = history
            .ok()
            .and_then(|messages| messages.into_iter().find_map(|m| m.parent_tool_use_id));
        let Some(launch) = launch.filter(|launch| launch != tool_use_id) else {
            return;
        };
        let active = &mut self.active;
        let mut turn_item_ordinal = |native: &str| match active.as_mut() {
            Some(active) => item_ordinal(
                &mut active.item_ordinals,
                active.input.provider_turn_ordinal,
                native,
            ),
            None => 0,
        };
        self.subagents.recover_resumed(
            task_id,
            &launch,
            UtcDateTime::now(),
            &mut turn_item_ordinal,
        );
    }
    /// Upstream `handleSdkMessage` for an unconfirmed prompt. The echo confirms
    /// it, a lifecycle frame names it, and root output is gated until then.
    fn gate(&mut self, prompt_uuid: &str, raw: &Value, event: ClaudeAdapterEvent) {
        if echoed_prompt_uuids(raw).contains(&prompt_uuid) {
            if matches!(
                self.echo_mode,
                PromptEchoMode::Unknown | PromptEchoMode::Acknowledged
            ) {
                let first = self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.gated_before_echo == 0);
                self.echo_mode = if type_is(raw, "result") || !first {
                    PromptEchoMode::ResultOnly
                } else {
                    PromptEchoMode::Early
                };
            }
            self.release_held();
            self.route(event);
            return;
        }
        if let Some((uuid, state)) = prompt_lifecycle(raw)
            && uuid == prompt_uuid
        {
            if self.echo_mode == PromptEchoMode::Unknown {
                self.echo_mode = PromptEchoMode::Acknowledged;
            }
            if state == "started"
                && let Some(active) = self.active.as_mut()
            {
                active.prompt_started = true;
            }
            // Claude finishes a prompt whose uuid its transcript already holds
            // without starting it, and no turn will ever answer it.
            let unstarted = state == "completed"
                && self
                    .active
                    .as_ref()
                    .is_some_and(|active| !active.prompt_started);
            if unstarted {
                self.fail_active(
                    "Claude acknowledged the message but never started a turn for it.",
                );
                return;
            }
        }
        if !is_gated_frame(raw) {
            // Frames the held turn produced through its own tool uses follow it.
            let held_tools = self.held_tool_use_ids();
            if frame_tool_use_ids(raw)
                .iter()
                .any(|id| held_tools.iter().any(|held| held == id))
            {
                if let Some(active) = self.active.as_mut() {
                    active.held.push(event);
                }
                return;
            }
            self.route(event);
            return;
        }
        if let Some(active) = self.active.as_mut() {
            active.gated_before_echo += 1;
        }
        if type_is(raw, "result") && is_result_for_other_turn(raw, prompt_uuid, self.echo_mode) {
            let held = self
                .active
                .as_mut()
                .map(|active| {
                    active.gated_before_echo = 0;
                    std::mem::take(&mut active.held)
                })
                .unwrap_or_default();
            // A result with no turns is lifecycle debris, so the held output waits for its owner.
            if raw.get("num_turns").and_then(Value::as_u64) == Some(0) {
                if let Some(active) = self.active.as_mut() {
                    active.held.extend(held);
                }
                return;
            }
            if self.echo_mode == PromptEchoMode::Early {
                // The held turn ran before this prompt's turn, which still follows on the stream.
                for frame in held.iter().filter_map(event_raw) {
                    self.buffer_wake(frame);
                }
                self.buffer_wake(raw);
                return;
            }
            // Its output already streamed into this turn, so only its result stays out.
            if raw.pointer("/origin/kind").and_then(Value::as_str) == Some("task-notification") {
                self.wake.reports.clear();
            }
            return;
        }
        if self.echo_mode != PromptEchoMode::Early {
            self.route(event);
            return;
        }
        if !type_is(raw, "result") {
            if let Some(active) = self.active.as_mut() {
                active.held.push(event);
            }
            return;
        }
        // A result from no other turn settles the prompt's turn.
        self.release_held();
        self.route(event);
    }
    /// The tool_use ids of the root frames held for the echo.
    fn held_tool_use_ids(&self) -> Vec<String> {
        self.active
            .as_ref()
            .into_iter()
            .flat_map(|active| active.held.iter().filter_map(event_raw))
            .flat_map(|raw| tool_use_ids(raw).into_iter().map(str::to_owned))
            .collect()
    }
    /// Gives the held root output to the active turn (upstream `releaseHeldRootFrames`).
    fn release_held(&mut self) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        active.echo_confirmed = true;
        let held = std::mem::take(&mut active.held);
        for event in held {
            self.route(event);
        }
    }
    fn assistant(&mut self, frame: turin_claude_protocol::Frame) {
        let owner = parent_tool_use_id(&frame).map(str::to_owned);
        for block in frame
            .raw
            .pointer("/message/content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let Some(id) = block.get("id").and_then(Value::as_str) else {
                continue;
            };
            match block.get("name").and_then(Value::as_str) {
                Some("Agent") => {
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    self.subagents.remember_launch(id, &input, owner.as_deref());
                }
                // A Monitor runs as a local_bash task; its task_started names this call.
                Some("Monitor") => {
                    self.monitor_tool_uses.insert(id.to_owned());
                }
                _ => {}
            }
        }
        if owner.is_none() {
            self.track_goal(&frame.raw);
        }
        // A subagent's frames belong to the subagent. Its text and tool calls go
        // to its child thread; its uuid must not reach the parent turn's rollback cursor.
        if let Some(owner) = owner.as_deref() {
            let now = UtcDateTime::now();
            for event in self.subagents.child_assistant(owner, &frame.raw, now) {
                self.emit(event);
            }
            return;
        }
        if self.active.is_none() {
            self.buffer_wake(&frame.raw);
            return;
        }
        let events = match self.active.as_mut() {
            Some(active) => {
                if let Some(uuid) = frame.raw.get("uuid").and_then(Value::as_str) {
                    active.native_message_cursor = Some(uuid.to_owned());
                }
                assistant_events(&mut self.plans, active, &frame)
            }
            None => return,
        };
        for event in events {
            self.emit(event);
        }
    }
    /// Writes the thinking text that grew since the last flush (upstream
    /// `flushDirty`). A block that stopped is not dirty any more.
    fn flush_thinking(&mut self) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        active.thinking_flush_at = None;
        let now = UtcDateTime::now();
        let mut dirty: Vec<u64> = active.stream.dirty.drain().collect();
        dirty.sort_unstable();
        let events: Vec<ProviderAdapterV2Event> = dirty
            .iter()
            .filter_map(|index| active.stream.thinking.get(index))
            .filter(|block| !block.text.is_empty())
            .map(|block| reasoning_update(&active.scope, block, now, false))
            .collect();
        for event in events {
            self.emit(event);
        }
    }
    /// Stream deltas. A subagent's stream belongs to the subagent.
    fn stream(&mut self, frame: turin_claude_protocol::Frame) {
        if parent_tool_use_id(&frame).is_some() {
            return;
        }
        let events = match self.active.as_mut() {
            Some(active) => stream_events(active, &frame),
            None => return,
        };
        for event in events {
            self.emit(event);
        }
    }
    /// Subagent lifecycle frames. Task lifecycle is also reported while no
    /// turn is active, so this does not depend on one.
    fn background(&mut self, frame: turin_claude_protocol::Frame) {
        let now = UtcDateTime::now();
        let subtype = frame.raw.get("subtype").and_then(Value::as_str);
        let idle = self.active.is_none();
        // While the root is idle, a frame is wake material before the roster and
        // registry see it: a notification's wake eligibility and its report read
        // the roster as it was.
        if idle {
            self.buffer_wake(&frame.raw);
        }
        let roster_changed = self.background_roster(&frame.raw);
        // Upstream projects an idle task only when a notification settles it. Its
        // start (and any progress) is replayed with the continuation that drains it.
        let buffered_start = task_id_of(&frame.raw).is_some_and(|id| {
            self.wake
                .has_buffered(|entry| is_task_started(entry) && task_id_of(entry) == Some(id))
        });
        let skip_registry = idle && (subtype != Some("task_notification") || buffered_start);
        let events = if skip_registry {
            Vec::new()
        } else {
            match subtype {
                Some("task_started") => {
                    // A task's turn item is numbered in the turn that is running, if any.
                    let active = &mut self.active;
                    let mut turn_item_ordinal = |native: &str| match active.as_mut() {
                        Some(active) => item_ordinal(
                            &mut active.item_ordinals,
                            active.input.provider_turn_ordinal,
                            native,
                        ),
                        None => 0,
                    };
                    self.subagents
                        .started(&frame.raw, now, &mut turn_item_ordinal)
                }
                Some("task_progress") => self.subagents.progress(&frame.raw, now),
                Some("task_updated") => self.subagents.updated(&frame.raw, now),
                Some("task_notification") => self.subagents.notified(&frame.raw, now),
                _ => Vec::new(),
            }
        };
        for event in events {
            self.emit(event);
        }
        if let Some(task_id) = frame.raw.get("task_id").and_then(Value::as_str) {
            let task_id = task_id.to_owned();
            self.resolve_stop_waiters(&task_id);
        }
        if roster_changed {
            self.emit_roster();
        }
    }
    /// Upstream `bufferWakeMessage`. Parks a frame that arrived while the root is
    /// idle, and offers a continuation when the frame says Claude is waking the
    /// thread: a subagent's notification, a native turn's start or output after a
    /// background task's notification, or a result.
    fn buffer_wake(&mut self, raw: &Value) {
        let task_id = task_id_of(raw).map(str::to_owned);
        let view = task_id
            .as_deref()
            .and_then(|id| self.subagents.wake_view(id));
        let notification = is_task_notification(raw);
        let pending_task = notification
            && task_id
                .as_deref()
                .is_some_and(|id| self.wake.eligible.contains(id));
        let buffered_start = task_id.as_deref().is_some_and(|id| {
            self.wake
                .has_buffered(|entry| is_task_started(entry) && task_id_of(entry) == Some(id))
        });
        let nested = notification && !pending_task && view.as_ref().is_some_and(|view| view.nested);
        let pending_subagent = notification
            && !pending_task
            && !nested
            && view.as_ref().is_some_and(|view| view.running)
            || (notification && !pending_task && !nested && buffered_start);
        let evidence = WakeEvidence {
            pending_task_notification: pending_task,
            pending_subagent_notification: pending_subagent,
            nested_subagent_notification: nested,
            known_subagent_task_started: is_task_started(raw) && view.is_some(),
            new_subagent_task_started: is_task_started(raw)
                && !is_opaque_task_type(raw.get("task_type").and_then(Value::as_str)),
        };
        if !evidence.is_evidence(raw) {
            return;
        }
        let summary = if notification { summary_of(raw) } else { None };
        self.wake.buffer.push(raw.clone());
        if let Some(summary) = summary {
            self.wake.detail = Some(summary.to_owned());
        }
        if pending_task {
            let report = self.opaque_task_report(task_id.as_deref().unwrap_or_default(), raw);
            self.wake
                .record_report(task_id.as_deref().unwrap_or_default(), report);
        } else if pending_subagent {
            let label = view
                .as_ref()
                .and_then(|view| view.title.clone())
                .or_else(|| {
                    self.wake
                        .buffer
                        .iter()
                        .find(|entry| {
                            is_task_started(entry) && task_id_of(entry) == task_id.as_deref()
                        })
                        .and_then(|entry| entry.get("description").and_then(Value::as_str))
                        .map(str::to_owned)
                });
            let report = BackgroundWorkReport {
                work: BackgroundWork {
                    label,
                    kind: BackgroundWorkKind::Subagent {
                        child_thread_id: view
                            .as_ref()
                            .and_then(|view| view.child_thread_id.clone()),
                    },
                },
                outcome: notification_outcome(raw),
            };
            self.wake
                .record_report(task_id.as_deref().unwrap_or_default(), report);
        }
        if pending_task && let Some(id) = task_id.as_deref() {
            // The first idle notification wakes the root once. A duplicate is buffered only.
            self.wake.eligible.remove(id);
            self.wake.tombstones.insert(id.to_owned());
        }
        // A notification counts as a wake only with a root frame behind it, since
        // a background subagent keeps streaming while the root is idle.
        let native_opaque_wake = self.wake.has_buffered(is_task_notification)
            && matches!(type_of(raw), Some("assistant" | "user"))
            && raw.get("parent_tool_use_id").is_none_or(Value::is_null);
        let offers =
            pending_subagent || native_opaque_wake || is_turn_start(raw) || type_is(raw, "result");
        if offers {
            self.offer_continuation();
        }
    }
    /// Upstream `opaqueTaskWakeReport`: reads the roster, or the task's last
    /// known entry, before the notification clears it.
    fn opaque_task_report(&mut self, task_id: &str, notification: &Value) -> BackgroundWorkReport {
        let task = self
            .roster
            .tasks()
            .iter()
            .find(|task| roster_task_id(task) == task_id)
            .cloned()
            .or_else(|| self.wake.last_known.remove(task_id));
        let kind = match task {
            Some(OrchestrationV2PendingBackgroundTask::Monitor { .. }) => {
                BackgroundWorkKind::Monitor
            }
            Some(OrchestrationV2PendingBackgroundTask::BackgroundTask { .. }) => {
                BackgroundWorkKind::BackgroundTask
            }
            _ => BackgroundWorkKind::Command { exit_code: None },
        };
        let label = task
            .as_ref()
            .and_then(|task| task.description().map(|d| d.as_str().to_owned()));
        BackgroundWorkReport {
            work: BackgroundWork { label, kind },
            outcome: notification_outcome(notification),
        }
    }
    /// Upstream `offer` in `bufferWakeMessage`: asks the host for a continuation
    /// once per wake, and hands it the reports and the notification summary.
    fn offer_continuation(&mut self) {
        let Some((thread_id, provider_thread_id)) = self.wake.route.clone() else {
            return;
        };
        if self.wake.requested {
            return;
        }
        self.wake.requested = true;
        let reports = self.wake.take_reports();
        self.continuation.offer(ProviderContinuationRequest {
            thread_id,
            provider_thread_id,
            driver: driver(),
            detail: self.wake.detail.clone(),
            notification: background_work_notification(&reports),
            delegated_completion: None,
            delivery: None,
            message_id: None,
            dispatch_if_current: None,
            clear_if_current: None,
        });
    }
    /// A rate-limit frame while idle is parked, so a continuation can show it (upstream).
    fn rate_limit(&mut self, frame: turin_claude_protocol::Frame) {
        if self.active.is_none() {
            self.buffer_wake(&frame.raw);
        }
    }
    /// The `init` every turn opens with. While idle it is the wake turn starting.
    fn init(&mut self, frame: turin_claude_protocol::Frame) {
        if self.active.is_none() {
            self.buffer_wake(&frame.raw);
        }
    }
    /// Upstream `startTurn` for a continuation: the frames the wake buffered replay
    /// into this turn, the result last. With no buffer the turn settles at once.
    /// A buffered `init` or native output means Claude is still working on the wake
    /// turn, so the turn stays open for it.
    fn drain_wake(&mut self) {
        self.wake.requested = false;
        let drained = std::mem::take(&mut self.wake.buffer);
        if drained.is_empty() {
            self.finish_turn(ProviderAdapterV2TurnTerminalStatus::Completed, &json!({}));
            return;
        }
        let has_opaque_notification = drained.iter().any(|entry| {
            is_task_notification(entry)
                && task_id_of(entry).is_some_and(|id| self.wake.tombstones.contains(id))
        });
        let has_native_wake = drained.iter().any(|entry| {
            matches!(type_of(entry), Some("user" | "assistant")) || is_turn_start(entry)
        });
        let (results, rest): (Vec<Value>, Vec<Value>) = drained
            .into_iter()
            .partition(|entry| type_is(entry, "result"));
        for entry in rest {
            if let Some(id) = task_id_of(&entry).filter(|_| is_task_notification(&entry)) {
                self.wake.tombstones.remove(id);
            }
            self.replay(&entry);
        }
        if let Some(last) = results.last() {
            self.replay(last);
            return;
        }
        if has_opaque_notification && !has_native_wake {
            self.finish_turn(ProviderAdapterV2TurnTerminalStatus::Completed, &json!({}));
        }
    }
    /// Routes a buffered frame as if it had arrived live.
    fn replay(&mut self, raw: &Value) {
        let Ok(frame) = turin_claude_protocol::Frame::decode(raw.clone()) else {
            return;
        };
        self.route(project_frame(frame));
    }
    /// The roster edges of a background frame: a `background_tasks_changed`
    /// snapshot, a non-subagent task_started, or its task_notification.
    /// Returns whether the roster changed.
    fn background_roster(&mut self, raw: &Value) -> bool {
        let text = |key: &str| raw.get(key).and_then(Value::as_str);
        match text("subtype") {
            Some("background_tasks_changed") => {
                let Some(entries) = raw.get("tasks").and_then(Value::as_array) else {
                    return false;
                };
                let listed: HashSet<&str> = entries
                    .iter()
                    .filter_map(|entry| entry.get("task_id").and_then(Value::as_str))
                    .collect();
                let tasks: Vec<_> = entries
                    .iter()
                    .filter_map(|entry| {
                        parse_roster_entry(entry, |id| self.monitor_tasks.contains(id))
                    })
                    .collect();
                // A monitor the snapshot no longer lists has ended.
                self.monitor_tasks.retain(|id| listed.contains(id.as_str()));
                for task in &tasks {
                    let id = roster_task_id(task).to_owned();
                    self.wake.last_known.insert(id.clone(), task.clone());
                    self.wake.eligible.insert(id);
                }
                self.roster.replace(tasks);
                true
            }
            Some("task_started") => {
                // A foreground task blocks its tool call, so it is not background work.
                let task_type = text("task_type");
                if !is_opaque_task_type(task_type)
                    || raw.get("is_backgrounded").and_then(Value::as_bool) == Some(false)
                {
                    return false;
                }
                let Some(task_id) = text("task_id").filter(|id| !id.is_empty()) else {
                    return false;
                };
                let task_id = task_id.to_owned();
                if text("tool_use_id").is_some_and(|id| self.monitor_tool_uses.contains(id)) {
                    self.monitor_tasks.insert(task_id.clone());
                }
                let by_monitor = self.monitor_tasks.contains(&task_id);
                let Some(task) = pending_task(&task_id, task_type, by_monitor, text("description"))
                else {
                    return false;
                };
                let id = roster_task_id(&task).to_owned();
                self.wake.last_known.insert(id.clone(), task.clone());
                self.wake.eligible.insert(id);
                self.roster.upsert(task);
                true
            }
            Some("task_notification") => {
                let Some(task_id) = text("task_id") else {
                    return false;
                };
                self.monitor_tasks.remove(task_id);
                self.roster.remove(task_id)
            }
            _ => false,
        }
    }
    /// Upstream `emitProviderThreadRoster`: the thread with its roster. Between
    /// turns it is idle; during a turn an idle thread is active.
    fn emit_roster(&mut self) {
        self.emit_thread(true);
    }
    /// Emits the thread with the roster and goal as they now are. With
    /// `update_status`, the status follows whether a turn is running.
    fn emit_thread(&mut self, update_status: bool) {
        let base = match self.active.as_ref() {
            Some(active) => Some(active.thread.clone()),
            None => self.last_thread.clone(),
        };
        let Some(mut thread) = base else {
            return;
        };
        if update_status {
            thread.status = match (self.active.is_some(), thread.status) {
                (false, _) => OrchestrationV2ProviderThreadStatus::Idle,
                (true, OrchestrationV2ProviderThreadStatus::Idle) => {
                    OrchestrationV2ProviderThreadStatus::Active
                }
                (true, status) => status,
            };
        }
        thread.pending_background_tasks = self.roster.tasks().to_vec();
        thread.goal = self.goal.clone();
        thread.updated_at = UtcDateTime::now();
        self.last_thread = Some(thread.clone());
        self.emit(ProviderAdapterV2Event::ProviderThreadUpdated {
            driver: driver(),
            provider_thread: thread,
            retained_native_turn_ids: None,
        });
    }
    /// Upstream `trackClaudeGoal`: a root frame may change the goal, and a
    /// checked assistant frame marks the turn as checked against it.
    fn track_goal(&mut self, raw: &Value) {
        let checked_assistant = raw.get("type").and_then(Value::as_str) == Some("assistant")
            && raw.pointer("/message/model").and_then(Value::as_str) != Some("<synthetic>");
        if checked_assistant
            && self
                .goal
                .as_ref()
                .is_some_and(|goal| goal.status == OrchestrationV2ProviderGoalStatus::Active)
            && let Some(active) = self.active.as_mut()
        {
            active.goal_checked = true;
        }
        let Some(next) = next_claude_goal(self.goal.as_ref(), raw) else {
            return;
        };
        if next == self.goal {
            return;
        }
        self.goal = next;
        self.emit_thread(false);
    }
    /// Upstream `compact_boundary`: a completed "Context compacted" item. A
    /// positive post-compaction size also becomes the turn's used tokens.
    fn compaction(&mut self, frame: turin_claude_protocol::Frame) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let Some(native) = frame.raw.get("uuid").and_then(Value::as_str) else {
            return;
        };
        let now = UtcDateTime::now();
        let metadata = frame.raw.get("compact_metadata");
        let before = metadata
            .and_then(|m| m.get("pre_tokens"))
            .and_then(Value::as_u64);
        let after = metadata
            .and_then(|m| m.get("post_tokens"))
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && *n > 0.0)
            .map(f64::round)
            .map(|n| n as u64);
        let mut events = vec![];
        if let Some(used) = after {
            let mut turn = active.turn.clone();
            let max_tokens = context_window(&self.catalog, &active.input.model_selection);
            turn.token_usage = serde_json::from_value(json!({
                "usedTokens": used, "maxTokens": max_tokens, "updatedAt": now,
            }))
            .ok();
            events.push(ProviderAdapterV2Event::ProviderTurnUpdated {
                driver: driver(),
                thread_id: Some(active.input.thread_id.clone()),
                provider_turn: turn,
            });
        }
        let ordinal = item_ordinal(
            &mut active.item_ordinals,
            active.input.provider_turn_ordinal,
            native,
        );
        events.push(ProviderAdapterV2Event::TurnItemUpdated {
            driver: driver(),
            turn_item: compaction_item(&active.scope, native, ordinal, before, after, now),
        });
        for event in events {
            self.emit(event);
        }
    }
    fn user(&mut self, frame: turin_claude_protocol::Frame) {
        // A subagent's tool results finish its tool calls in its child thread.
        if let Some(owner) = parent_tool_use_id(&frame).map(str::to_owned) {
            let now = UtcDateTime::now();
            for event in self.subagents.child_user(&owner, &frame.raw, now) {
                self.emit(event);
            }
            return;
        }
        self.track_goal(&frame.raw);
        let mut events = self.workflow_handles(&frame);
        if self.active.is_none() {
            self.buffer_wake(&frame.raw);
        } else if let Some(active) = self.active.as_mut() {
            events.extend(tool_result_events(&mut self.plans, active, &frame));
        }
        for event in events {
            self.emit(event);
        }
    }
    /// A Workflow tool's result carries the handles of the coordinator it
    /// started. That tool call is an ordinary tool, so this reads the result
    /// wherever it lands, with or without an active turn.
    fn workflow_handles(
        &mut self,
        frame: &turin_claude_protocol::Frame,
    ) -> Vec<ProviderAdapterV2Event> {
        let now = UtcDateTime::now();
        let results: Vec<&Value> = frame
            .raw
            .pointer("/message/content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .collect();
        let structured = (results.len() == 1)
            .then(|| frame.raw.get("tool_use_result"))
            .flatten();
        results
            .into_iter()
            .filter_map(|block| {
                let tool_use_id = block.get("tool_use_id").and_then(Value::as_str)?;
                let output = structured
                    .cloned()
                    .unwrap_or_else(|| block.get("content").cloned().unwrap_or(Value::Null));
                let handles = parse_claude_workflow_run_handles(&output)?;
                Some(self.subagents.run_handles(tool_use_id, &handles, now))
            })
            .flatten()
            .collect()
    }
    fn result(&mut self, frame: turin_claude_protocol::Frame) {
        // The turn's buffered thinking text is written before the turn ends (upstream `flushTurn`).
        self.flush_thinking();
        let Some(active) = self.active.as_mut() else {
            self.buffer_wake(&frame.raw);
            return;
        };
        let result = &frame.raw;
        let failed = result
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let aborted =
            result.get("terminal_reason").and_then(Value::as_str) == Some("aborted_streaming");
        if active.steer_pending && failed && aborted {
            if let Some(usage) = result.get("usage") {
                active.earlier_usage.push(usage.clone());
            }
            active.steer_pending = false;
            return;
        }
        let status = if active.interrupted {
            ProviderAdapterV2TurnTerminalStatus::Interrupted
        } else if failed {
            ProviderAdapterV2TurnTerminalStatus::Failed
        } else {
            ProviderAdapterV2TurnTerminalStatus::Completed
        };
        self.finish_turn(status, &frame.raw);
    }
    /// Ends the active turn with `status`, reading the native `result` frame for usage and failure.
    fn finish_turn(&mut self, status: ProviderAdapterV2TurnTerminalStatus, result: &Value) {
        if let Some(run_id) = self
            .active
            .as_ref()
            .map(|active| active.input.run_id.clone())
        {
            self.stop_run_workflows(&run_id, status);
        }
        let Some(active) = self.active.as_mut() else {
            return;
        };
        active.held.clear();
        let now = UtcDateTime::now();
        let usage = normalize_claude_turn_token_usage(
            Some(result),
            false,
            match status {
                ProviderAdapterV2TurnTerminalStatus::Completed => RuntimeTurnState::Completed,
                ProviderAdapterV2TurnTerminalStatus::Interrupted => RuntimeTurnState::Interrupted,
                ProviderAdapterV2TurnTerminalStatus::Cancelled => RuntimeTurnState::Cancelled,
                ProviderAdapterV2TurnTerminalStatus::Failed => RuntimeTurnState::Failed,
            },
            &active.earlier_usage,
        );
        active.turn.turn_token_usage = Some(usage);
        active.turn.status = match status {
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
        active.turn.completed_at = Some(now);
        if let Some(cursor) = active.native_message_cursor.as_deref() {
            active.turn.native_turn_ref = Some(weak_native_ref(cursor));
        }
        // A completed turn has consumed the rollback head, so the next turn
        // resumes the session normally.
        if status == ProviderAdapterV2TurnTerminalStatus::Completed {
            active.thread.native_conversation_head_ref = None;
        }
        active.thread.status = OrchestrationV2ProviderThreadStatus::Idle;
        active.thread.updated_at = now;
        // Claude ends a goal's turn on its own only after the goal's check
        // passes; the check is deferred while background work runs. A turn that
        // was checked and finished that way completes the goal.
        let terminal_reason = result.get("terminal_reason").and_then(Value::as_str);
        if let Some(goal) = self.goal.as_mut()
            && goal.status == OrchestrationV2ProviderGoalStatus::Active
            && active.goal_checked
            && status == ProviderAdapterV2TurnTerminalStatus::Completed
            && matches!(terminal_reason, None | Some("completed"))
            && self.roster.tasks().is_empty()
        {
            *goal = serde_json::from_value(json!({
                "objective": goal.objective.as_str(),
                "status": "complete",
                "checks": goal.checks,
            }))
            .expect("goal matches the contract shape");
        }
        active.thread.goal = self.goal.clone();
        let closing = finish_running_tools(active, terminal_item_status(status));
        let turn = active.turn.clone();
        let thread = active.thread.clone();
        let thread_id = active.input.thread_id.clone();
        let run_ordinal = PositiveInt::new(active.input.run_ordinal.max(1) as u64).unwrap();
        for event in closing {
            self.emit(event);
        }
        self.emit(ProviderAdapterV2Event::ProviderTurnUpdated {
            driver: driver(),
            thread_id: Some(thread_id),
            provider_turn: turn.clone(),
        });
        self.emit(ProviderAdapterV2Event::ProviderThreadUpdated {
            driver: driver(),
            provider_thread: thread,
            retained_native_turn_ids: None,
        });
        let failure = if status == ProviderAdapterV2TurnTerminalStatus::Failed {
            Some(make_provider_failure(ProviderFailureInput {
                cause: None,
                message: result
                    .get("errors")
                    .and_then(Value::as_array)
                    .and_then(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .find(|s| !s.trim().starts_with("[ede_diagnostic]"))
                    })
                    .or_else(|| result.get("result").and_then(Value::as_str)),
                code: None,
                class: None,
                retryable: None,
                reset_at: None,
            }))
        } else {
            None
        };
        self.emit(ProviderAdapterV2Event::TurnTerminal {
            driver: driver(),
            provider_thread_id: turn.provider_thread_id,
            provider_turn_id: turn.id,
            run_ordinal,
            status,
            failure,
            failure_item_ordinal: None,
            retry: None,
            retry_started_at: None,
            thread_disposition: OrchestrationV2ProviderThreadDisposition::Reusable,
        });
        self.active = None;
    }
    /// Upstream `finalizeActiveTurn` for a turn that does not complete: the run
    /// stops following its workflows, so each one still running is stopped and
    /// settled, failed for a failed turn and cancelled otherwise. The stop is
    /// sent without waiting, so an unanswered request cannot hold the turn open.
    fn stop_run_workflows(&mut self, run_id: &RunId, status: ProviderAdapterV2TurnTerminalStatus) {
        if status == ProviderAdapterV2TurnTerminalStatus::Completed {
            return;
        }
        let settled = if status == ProviderAdapterV2TurnTerminalStatus::Failed {
            "failed"
        } else {
            "cancelled"
        };
        let now = UtcDateTime::now();
        for task_id in self.subagents.running_workflows(run_id) {
            if let Some(claude) = self.claude.as_ref()
                && let Err(error) = claude.stop_task_detached(&task_id)
            {
                tracing::warn!(%error, %task_id, "Claude workflow stop was not sent");
            }
            let events = self.subagents.settle(&task_id, settled, None, now);
            for event in events {
                self.emit(event);
            }
        }
    }
    /// The CLI went away before a result. An interrupt that was asked for
    /// ends the turn as interrupted, and the process is reusable only then.
    fn fail_active(&mut self, error: &str) {
        self.flush_thinking();
        // The process or its turn failed: its background work died with it.
        self.monitor_tasks.clear();
        if self.roster.clear() {
            self.emit_roster();
        }
        let Some(mut active) = self.active.take() else {
            return;
        };
        let run_ordinal = PositiveInt::new(active.input.run_ordinal.max(1) as u64).unwrap();
        let (status, failure, disposition) = if active.interrupted {
            (
                ProviderAdapterV2TurnTerminalStatus::Interrupted,
                None,
                OrchestrationV2ProviderThreadDisposition::Reusable,
            )
        } else {
            (
                ProviderAdapterV2TurnTerminalStatus::Failed,
                Some(make_provider_failure(ProviderFailureInput {
                    cause: None,
                    message: Some(error),
                    code: None,
                    class: None,
                    retryable: None,
                    reset_at: None,
                })),
                OrchestrationV2ProviderThreadDisposition::Broken,
            )
        };
        self.stop_run_workflows(&active.input.run_id, status);
        let closing = finish_running_tools(&mut active, terminal_item_status(status));
        for event in closing {
            self.emit(event);
        }
        if let Some(cursor) = active.native_message_cursor.as_deref() {
            active.turn.native_turn_ref = Some(weak_native_ref(cursor));
        }
        let mut turn = active.turn;
        turn.status = if status == ProviderAdapterV2TurnTerminalStatus::Interrupted {
            OrchestrationV2ProviderTurnStatus::Interrupted
        } else {
            OrchestrationV2ProviderTurnStatus::Failed
        };
        turn.completed_at = Some(UtcDateTime::now());
        self.emit(ProviderAdapterV2Event::ProviderTurnUpdated {
            driver: driver(),
            thread_id: Some(active.input.thread_id.clone()),
            provider_turn: turn.clone(),
        });
        self.emit(ProviderAdapterV2Event::TurnTerminal {
            driver: driver(),
            provider_thread_id: turn.provider_thread_id,
            provider_turn_id: turn.id,
            run_ordinal,
            status,
            failure,
            failure_item_ordinal: None,
            retry: None,
            retry_started_at: None,
            thread_disposition: disposition,
        });
    }
}

/// Upstream `stream_event`, thinking only. A thinking block streams into its
/// reasoning item as each delta arrives. Upstream coalesces these at 50 ms
/// (`ProviderTextDeltaCoalescer`, not yet ported), so each delta emits here.
/// Text deltas are not streamed: the completed assistant message carries them.
fn stream_events(
    active: &mut ActiveTurn,
    frame: &turin_claude_protocol::Frame,
) -> Vec<ProviderAdapterV2Event> {
    let event = frame.raw.get("event").unwrap_or(&Value::Null);
    let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
    let now = UtcDateTime::now();
    let mut events = Vec::new();
    match event.get("type").and_then(Value::as_str) {
        Some("message_start") => {
            active.stream = StreamState {
                message_id: event
                    .pointer("/message/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                ..Default::default()
            };
        }
        Some("content_block_start")
            if event.pointer("/content_block/type").and_then(Value::as_str) == Some("thinking") =>
        {
            let Some(message_id) = active.stream.message_id.clone() else {
                return events;
            };
            let in_message = active.stream.thinking_started;
            active.stream.thinking_started += 1;
            let native = format!("{message_id}:thinking:{in_message}");
            let ordinal = item_ordinal(
                &mut active.item_ordinals,
                active.input.provider_turn_ordinal,
                &native,
            );
            let text = event
                .pointer("/content_block/thinking")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let block = StreamedThinking {
                native,
                ordinal,
                started_at: now,
                text,
            };
            // A block with no text yet has nothing to show.
            if !block.text.is_empty() {
                events.push(reasoning_update(&active.scope, &block, now, false));
            }
            active.stream.thinking.insert(index, block);
        }
        Some("content_block_delta")
            if event.pointer("/delta/type").and_then(Value::as_str) == Some("thinking_delta") =>
        {
            let delta = event
                .pointer("/delta/thinking")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if let Some(block) = active.stream.thinking.get_mut(&index) {
                block.text.push_str(delta);
                // Flushed on the timer, not per delta.
                active.stream.dirty.insert(index);
                active
                    .thinking_flush_at
                    .get_or_insert_with(|| tokio::time::Instant::now() + THINKING_FLUSH_INTERVAL);
            }
        }
        Some("content_block_stop") => {
            // The final text below supersedes any flush still waiting for this block.
            active.stream.dirty.remove(&index);
            if let Some(block) = active.stream.thinking.remove(&index)
                && !block.text.is_empty()
            {
                events.push(reasoning_update(&active.scope, &block, now, true));
            }
        }
        _ => {}
    }
    events
}

fn reasoning_update(
    scope: &TurnScope,
    block: &StreamedThinking,
    now: UtcDateTime,
    completed: bool,
) -> ProviderAdapterV2Event {
    ProviderAdapterV2Event::TurnItemUpdated {
        driver: driver(),
        turn_item: streaming_reasoning_item(
            scope,
            &block.native,
            block.ordinal,
            &block.text,
            block.started_at,
            now,
            completed,
        ),
    }
}

/// Assistant text blocks become a node, message, and turn item. `tool_use`
/// blocks start a tool call. Completed thinking blocks become reasoning items.
fn assistant_events(
    plans: &mut PlanState,
    active: &mut ActiveTurn,
    frame: &turin_claude_protocol::Frame,
) -> Vec<ProviderAdapterV2Event> {
    let message_id = frame
        .raw
        .pointer("/message/id")
        .and_then(Value::as_str)
        .or_else(|| frame.raw.get("uuid").and_then(Value::as_str))
        .unwrap_or("assistant")
        .to_owned();
    let blocks = frame
        .raw
        .pointer("/message/content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let now = UtcDateTime::now();
    let mut events = Vec::new();
    let tool_names: HashMap<String, String> = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
        .filter_map(|b| {
            let id = b.get("id")?.as_str()?;
            let name = b.get("name")?.as_str()?;
            Some((id.to_owned(), name.to_owned()))
        })
        .collect();
    let presentations = presentations_from_meta(&frame.raw, &tool_names);
    let model = frame
        .raw
        .pointer("/message/model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    for (index, block) in blocks.iter().enumerate() {
        let block_type = block.get("type").and_then(Value::as_str);
        if block_type != Some("thinking") {
            // Any other block closes the open thinking block. A tool call right
            // after a second thinking block takes that block's text instead.
            let update = active.last_thinking.take();
            if let Some(update) = update.filter(|u| {
                block_type == Some("tool_use")
                    && u.message_id == message_id
                    && u.follows_thinking
                    && claude_writes_updates_as_thinking(model)
            }) {
                events.extend(assistant_text_events(
                    active,
                    &update.native,
                    &update.text,
                    now,
                ));
            }
        }
        match block_type {
            Some("text") => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                let native = format!("{message_id}:text:{index}");
                events.extend(assistant_text_events(active, &native, text, now));
            }
            Some("thinking") => {
                let index = active
                    .thinking_blocks
                    .entry(message_id.clone())
                    .or_insert(0);
                let block_index = *index;
                *index += 1;
                let text = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let native = format!("{message_id}:thinking:{block_index}");
                let previous = active.last_thinking.take();
                let follows_thinking = previous.is_some_and(|p| p.message_id == message_id);
                active.last_thinking = (!text.trim().is_empty()).then(|| LastThinking {
                    message_id: message_id.clone(),
                    native: native.clone(),
                    text: text.to_owned(),
                    follows_thinking,
                });
                if text.is_empty() {
                    continue;
                }
                let ordinal = item_ordinal(
                    &mut active.item_ordinals,
                    active.input.provider_turn_ordinal,
                    &native,
                );
                events.push(ProviderAdapterV2Event::TurnItemUpdated {
                    driver: driver(),
                    turn_item: reasoning_item(&active.scope, &native, ordinal, text, now),
                });
            }
            Some("tool_use") => {
                let Some(native) = block.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_owned();
                // An Agent call is the subagent, not a tool call. The registry
                // took its launch in `assistant`.
                if name == "Agent" {
                    continue;
                }
                let input = block.get("input").cloned().unwrap_or(Value::Null);
                let presentation = presentations
                    .get(native)
                    .cloned()
                    .unwrap_or_else(|| fallback_presentation(&name));
                // TodoWrite projects its own list before the call item, as upstream does.
                if name == "TodoWrite" {
                    events.extend(project_plan(
                        plans,
                        &active.scope,
                        active.input.provider_turn_ordinal,
                        &mut active.item_ordinals,
                        native,
                        PlanBody::TodoList(todo_steps(&input)),
                    ));
                }
                let ordinal = item_ordinal(
                    &mut active.item_ordinals,
                    active.input.provider_turn_ordinal,
                    native,
                );
                let call = ToolCall {
                    native_item_id: native,
                    tool_name: &name,
                    input: &input,
                    presentation: &presentation,
                    output: None,
                    output_fallback: None,
                    status: "running",
                    ordinal,
                    started_at: now,
                    updated_at: now,
                };
                let artifacts = tool_call_artifacts(&active.scope, &call);
                events.push(ProviderAdapterV2Event::NodeUpdated {
                    driver: driver(),
                    node: artifacts.node,
                });
                events.push(ProviderAdapterV2Event::TurnItemUpdated {
                    driver: driver(),
                    turn_item: artifacts.turn_item,
                });
                active.tool_calls.insert(
                    native.to_owned(),
                    TrackedTool {
                        name,
                        input,
                        presentation,
                        ordinal,
                        started_at: now,
                        finished: false,
                    },
                );
            }
            _ => {}
        }
    }
    events
}

/// Upstream `claudeWritesUpdatesAsThinking`: Claude 5.1 and later write the
/// text before a tool call as a thinking block.
fn claude_writes_updates_as_thinking(model: &str) -> bool {
    static MODEL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"claude-[a-z]+-(\d+)(?:-(\d{1,2})\b)?").expect("valid model pattern")
    });
    let Some(captures) = MODEL.captures(model) else {
        return false;
    };
    let major: u64 = captures[1].parse().unwrap_or(0);
    let minor: u64 = captures
        .get(2)
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(0);
    major > 5 || (major == 5 && minor >= 1)
}

/// The item a text block or a thinking block turned into assistant text.
/// `native` is its provider item id, so a later update replaces it in place.
fn assistant_text_events(
    active: &mut ActiveTurn,
    native: &str,
    text: &str,
    now: UtcDateTime,
) -> Vec<ProviderAdapterV2Event> {
    let ordinal = item_ordinal(
        &mut active.item_ordinals,
        active.input.provider_turn_ordinal,
        native,
    );
    let artifacts = assistant_artifacts(&active.scope, native, ordinal, text, now);
    active.last_assistant = Some(artifacts.message.clone());
    vec![
        ProviderAdapterV2Event::NodeUpdated {
            driver: driver(),
            node: artifacts.node,
        },
        ProviderAdapterV2Event::MessageUpdated {
            driver: driver(),
            message: artifacts.message,
        },
        ProviderAdapterV2Event::TurnItemUpdated {
            driver: driver(),
            turn_item: artifacts.turn_item,
        },
    ]
}

fn parent_tool_use_id(frame: &turin_claude_protocol::Frame) -> Option<&str> {
    frame.raw.get("parent_tool_use_id").and_then(Value::as_str)
}

/// Upstream `claudeContextWindow`: the window a selection resolves to, when the catalog knows it.
fn context_window(catalog: &ClaudeModelCatalog, selection: &ModelSelection) -> Option<u64> {
    resolve_claude_catalog_context_window_tokens(catalog, Some(selection))
        .map(|tokens| tokens as u64)
}

/// The frame an event carries, for the events that carry one.
fn event_raw(event: &ClaudeAdapterEvent) -> Option<&Value> {
    match event {
        ClaudeAdapterEvent::Assistant(frame)
        | ClaudeAdapterEvent::User(frame)
        | ClaudeAdapterEvent::Compaction(frame)
        | ClaudeAdapterEvent::Background(frame)
        | ClaudeAdapterEvent::Stream(frame)
        | ClaudeAdapterEvent::Result(frame)
        | ClaudeAdapterEvent::RateLimit(frame)
        | ClaudeAdapterEvent::Hook(frame)
        | ClaudeAdapterEvent::Other(frame) => Some(&frame.raw),
        ClaudeAdapterEvent::Init { frame, .. } => Some(&frame.raw),
        _ => None,
    }
}

fn type_of(raw: &Value) -> Option<&str> {
    raw.get("type").and_then(Value::as_str)
}

fn type_is(raw: &Value, kind: &str) -> bool {
    type_of(raw) == Some(kind)
}

/// Upstream `claudeTaskOutcome` for a notification's status.
fn notification_outcome(raw: &Value) -> OrchestrationV2NotificationOutcome {
    match raw.get("status").and_then(Value::as_str) {
        Some("completed") => OrchestrationV2NotificationOutcome::Completed,
        Some("stopped") => OrchestrationV2NotificationOutcome::Cancelled,
        _ => OrchestrationV2NotificationOutcome::Failed,
    }
}

/// `tool_result` blocks finish the tracked tool call they answer. A finished
/// task tool updates the thread's task list, which is then re-projected.
fn tool_result_events(
    plans: &mut PlanState,
    active: &mut ActiveTurn,
    frame: &turin_claude_protocol::Frame,
) -> Vec<ProviderAdapterV2Event> {
    let blocks = frame
        .raw
        .pointer("/message/content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let now = UtcDateTime::now();
    let mut events = Vec::new();
    let results: Vec<&Value> = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        .collect();
    // Upstream prefers the structured `tool_use_result` when the message
    // carries exactly one tool result.
    let structured = (results.len() == 1)
        .then(|| frame.raw.get("tool_use_result"))
        .flatten();
    let native_thread = native_id(&active.input.provider_thread)
        .ok()
        .map(str::to_owned);
    for block in results {
        let Some(native) = block.get("tool_use_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(tracked) = active.tool_calls.get_mut(native) else {
            continue;
        };
        if tracked.finished {
            continue;
        }
        tracked.finished = true;
        let failed = block
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let content = block.get("content").cloned().unwrap_or(Value::Null);
        // Upstream reads the structured result as the native output when the message has one.
        let output = structured.cloned().unwrap_or_else(|| content.clone());
        let call = ToolCall {
            native_item_id: native,
            tool_name: &tracked.name,
            input: &tracked.input,
            presentation: &tracked.presentation,
            output: Some(&output),
            output_fallback: structured.map(|_| &content),
            status: if failed { "failed" } else { "completed" },
            ordinal: tracked.ordinal,
            started_at: tracked.started_at,
            updated_at: now,
        };
        let artifacts = tool_call_artifacts(&active.scope, &call);
        events.push(ProviderAdapterV2Event::NodeUpdated {
            driver: driver(),
            node: artifacts.node,
        });
        events.push(ProviderAdapterV2Event::TurnItemUpdated {
            driver: driver(),
            turn_item: artifacts.turn_item,
        });
        if let Some(native_thread) = native_thread.as_deref().filter(|_| !failed) {
            let current = plans.task_list(native_thread);
            if let Some(tasks) =
                apply_task_tool(&current, call.tool_name, call.input, Some(&output))
            {
                plans.set_task_list(native_thread, tasks.clone());
                // One list per turn, updated in place; a later turn supersedes it.
                let native_item_id =
                    format!("claude-tasks:{}", active.scope.provider_turn_id.as_str());
                events.extend(project_plan(
                    plans,
                    &active.scope,
                    active.input.provider_turn_ordinal,
                    &mut active.item_ordinals,
                    &native_item_id,
                    PlanBody::TodoList(task_steps(&tasks)),
                ));
            }
        }
    }
    events
}

/// A turn that ends with tool calls still open closes them with the turn's
/// outcome, so no item is left running.
fn finish_running_tools(
    active: &mut ActiveTurn,
    status: &'static str,
) -> Vec<ProviderAdapterV2Event> {
    let now = UtcDateTime::now();
    let mut events = Vec::new();
    let open: Vec<String> = active
        .tool_calls
        .iter()
        .filter(|(_, tool)| !tool.finished)
        .map(|(id, _)| id.clone())
        .collect();
    for native in open {
        let Some(tracked) = active.tool_calls.get_mut(&native) else {
            continue;
        };
        tracked.finished = true;
        let call = ToolCall {
            native_item_id: &native,
            tool_name: &tracked.name,
            input: &tracked.input,
            presentation: &tracked.presentation,
            output: None,
            output_fallback: None,
            status,
            ordinal: tracked.ordinal,
            started_at: tracked.started_at,
            updated_at: now,
        };
        let artifacts = tool_call_artifacts(&active.scope, &call);
        events.push(ProviderAdapterV2Event::NodeUpdated {
            driver: driver(),
            node: artifacts.node,
        });
        events.push(ProviderAdapterV2Event::TurnItemUpdated {
            driver: driver(),
            turn_item: artifacts.turn_item,
        });
    }
    events
}

fn terminal_item_status(status: ProviderAdapterV2TurnTerminalStatus) -> &'static str {
    match status {
        ProviderAdapterV2TurnTerminalStatus::Completed => "completed",
        ProviderAdapterV2TurnTerminalStatus::Interrupted => "interrupted",
        ProviderAdapterV2TurnTerminalStatus::Cancelled => "cancelled",
        ProviderAdapterV2TurnTerminalStatus::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_writes_updates_from_claude_5_1() {
        for model in [
            "claude-sonnet-5-1",
            "claude-haiku-5-5",
            "claude-fable-6",
            "claude-opus-5-12",
        ] {
            assert!(claude_writes_updates_as_thinking(model), "{model}");
        }
        for model in [
            "claude-opus-5",
            "claude-sonnet-4-6",
            "claude-haiku-4-5-20251001",
            "gpt-5",
        ] {
            assert!(!claude_writes_updates_as_thinking(model), "{model}");
        }
    }
}
