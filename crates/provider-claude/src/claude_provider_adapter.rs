//! Port of upstream/apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts provider-core boundary.
// `ProviderAdapterV2Error` is the provider-core trait's error type; boxing it
// here would only diverge from the trait.
#![allow(clippy::result_large_err)]
use crate::{
    claude_adapter_v2::{ClaudeAdapterEvent, ClaudeAdapterV2, ClaudeHostRequest},
    claude_model_catalog::ClaudeModelCatalog,
    claude_model_options::{apply_claude_prompt_effort_prefix, compile_claude_model_selection},
    claude_permission::ApprovalDecision,
    claude_plan_items::{
        PlanBody, PlanState, apply_task_tool, project_plan, task_steps, todo_steps,
    },
    claude_query_options::{ClaudeQueryOptionsInput, make_claude_query_options},
    claude_turn_items::{
        ItemOrdinals, ToolCall, TurnScope, assistant_artifacts, item_ordinal, tool_call_artifacts,
    },
    claude_turn_token_usage::normalize_claude_turn_token_usage,
};
use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_stream::wrappers::UnboundedReceiverStream;
use turin_contracts::{
    PositiveInt, ProviderSessionId, RuntimeRequestId, ThreadId, TrimmedNonEmptyString, UtcDateTime,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2ConversationMessage, OrchestrationV2NativeRefStrength,
        OrchestrationV2ProviderCapabilities, OrchestrationV2ProviderRef,
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
    provider_adapter::{
        ProviderAdapterErrorKind, ProviderAdapterV2, ProviderAdapterV2EnsureThreadInput,
        ProviderAdapterV2Error, ProviderAdapterV2Event, ProviderAdapterV2ForkThreadInput,
        ProviderAdapterV2InterruptInput, ProviderAdapterV2OpenSessionInput,
        ProviderAdapterV2ReadThreadSnapshotInput, ProviderAdapterV2ResumeThreadInput,
        ProviderAdapterV2RollbackThreadInput, ProviderAdapterV2RuntimeRequestResponseInput,
        ProviderAdapterV2SteerInput, ProviderAdapterV2StopTaskInput,
        ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2TurnInput,
        ProviderAdapterV2TurnTerminalStatus, ProviderEventStream, ProviderSessionRuntime,
    },
    rt::BoxFuture,
    selection_transition::{ProviderSelectionTransitionInput, ProviderSelectionTransitionPlan},
};

pub fn claude_provider_capabilities_v2() -> OrchestrationV2ProviderCapabilities {
    serde_json::from_value(json!({
        "sessions":{"supportsMultipleProviderThreadsPerSession":false,"supportsModelSwitchInSession":true,"supportsProviderSwitchingViaHandoff":true,"supportsRuntimeModeSwitchInSession":false,"pendingRequestsSurviveRestart":false},
        "threads":{"canCreateEmptyThread":true,"canReadThreadSnapshot":false,"canRollbackThread":false,"canForkThread":true,"canForkFromTurn":true,"canForkFromSubagentThread":false,"exposesNativeThreadId":true},
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
    instance_id: ProviderInstanceId,
    driver: ProviderDriverKind,
    settings: ClaudeSettings,
    catalog: ClaudeModelCatalog,
    environment: Option<std::collections::BTreeMap<String, String>>,
    attachments_dir: Option<PathBuf>,
    runtime_instructions: String,
}
impl ClaudeProviderAdapter {
    pub fn new(
        instance_id: ProviderInstanceId,
        settings: ClaudeSettings,
        catalog: ClaudeModelCatalog,
        environment: Option<std::collections::BTreeMap<String, String>>,
        attachments_dir: Option<PathBuf>,
        runtime_instructions: String,
    ) -> Self {
        Self {
            instance_id,
            driver: driver(),
            settings,
            catalog,
            environment,
            attachments_dir,
            runtime_instructions,
        }
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
            selection: input.model_selection,
            claude: None,
            host_requests: None,
            active: None,
            plans: PlanState::default(),
            pending_requests: HashMap::new(),
            commands: command_rx,
            events,
        };
        let handle = tokio::spawn(actor.run());
        Ok(Arc::new(ClaudeSessionRuntime {
            instance_id: self.instance_id.clone(),
            driver: self.driver.clone(),
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
    Fork(
        ProviderAdapterV2ForkThreadInput,
        oneshot::Sender<Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error>>,
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
        Err(ProviderAdapterV2Error::new(
            ProviderAdapterErrorKind::RollbackThread {
                driver: driver(),
                provider_thread_id: input.provider_thread.id,
                checkpoint_id: None,
            },
        ))
    }
    async fn fork_thread(
        &self,
        input: ProviderAdapterV2ForkThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        ask(&self.commands, |tx| Command::Fork(input, tx)).await
    }
    fn stop_task(
        &self,
        input: ProviderAdapterV2StopTaskInput,
    ) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        Some(Box::pin(ask(&self.commands, |tx| {
            Command::StopTask(input, tx)
        })))
    }
}

struct ActiveTurn {
    input: ProviderAdapterV2TurnInput,
    turn: OrchestrationV2ProviderTurn,
    thread: OrchestrationV2ProviderThread,
    earlier_usage: Vec<Value>,
    steer_pending: bool,
    interrupted: bool,
    last_assistant: Option<OrchestrationV2ConversationMessage>,
    scope: TurnScope,
    item_ordinals: ItemOrdinals,
    tool_calls: HashMap<String, TrackedTool>,
}

/// A tool call from its `tool_use` block until its `tool_result`.
struct TrackedTool {
    name: String,
    input: Value,
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
    selection: ModelSelection,
    claude: Option<ClaudeAdapterV2>,
    host_requests: Option<mpsc::UnboundedReceiver<ClaudeHostRequest>>,
    active: Option<ActiveTurn>,
    plans: PlanState,
    pending_requests: HashMap<RuntimeRequestId, PendingRequest>,
    commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<Result<ProviderAdapterV2Event, ProviderAdapterV2Error>>,
}

#[allow(clippy::large_enum_variant)]
enum Action {
    Command(Option<Command>),
    Event(Option<ClaudeAdapterEvent>),
    Host(Option<ClaudeHostRequest>),
}
impl ClaudeActor {
    async fn run(mut self) {
        loop {
            let action = tokio::select! {
                command = self.commands.recv() => Action::Command(command),
                event = async { match self.claude.as_mut() { Some(claude) => claude.next_event().await, None => std::future::pending().await } } => Action::Event(event),
                request = async { match self.host_requests.as_mut() { Some(requests) => requests.recv().await, None => std::future::pending().await } } => Action::Host(request),
            };
            match action {
                Action::Command(Some(command)) => self.command(command).await,
                Action::Command(None) => break,
                Action::Event(Some(event)) => self.event(event).await,
                Action::Event(None) => {
                    self.claude = None;
                    self.host_requests = None;
                    self.fail_active("Claude event stream closed before turn result");
                }
                Action::Host(Some(request)) => self.host_request(request),
                Action::Host(None) => self.host_requests = None,
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
            Command::Fork(input, reply) => {
                let result = self.fork(input).await;
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
            let resume = input.native_thread_has_turns.unwrap_or(false);
            let options = make_claude_query_options(ClaudeQueryOptionsInput {
                model_selection: &input.model_selection,
                catalog: &self.catalog,
                native_thread_id: &native,
                resume,
                resume_session_at: None,
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
        let compiled = compile_claude_model_selection(&input.model_selection, &self.catalog);
        let text = apply_claude_prompt_effort_prefix(
            &input.message.text,
            compiled.prompt_effort.as_deref(),
        );
        let prompt_uuid = uuid::Uuid::new_v4().to_string();
        self.claude
            .as_mut()
            .ok_or_else(|| protocol_error("Claude session is not running"))?
            .send_prompt(text, Some(prompt_uuid), None)
            .map_err(|e| protocol_error(e.to_string()))?;
        let scope = TurnScope {
            driver: driver(),
            thread_id: input.thread_id.clone(),
            run_id: input.run_id.clone(),
            root_node_id: input.root_node_id.clone(),
            provider_thread_id: input.provider_thread.id.clone(),
            provider_turn_id: turn.id.clone(),
        };
        self.active = Some(ActiveTurn {
            input,
            turn,
            thread,
            earlier_usage: vec![],
            steer_pending: false,
            interrupted: false,
            last_assistant: None,
            scope,
            item_ordinals: ItemOrdinals::new(),
            tool_calls: HashMap::new(),
        });
        Ok(())
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
        let forked = if let Some(claude) = self
            .claude
            .as_mut()
            .filter(|c| c.session_id() == Some(native.as_str()))
        {
            let id = claude
                .fork(None, None)
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
            turin_claude_protocol::fork_session(&native, &options, None, None)
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
    async fn event(&mut self, event: ClaudeAdapterEvent) {
        match event {
            ClaudeAdapterEvent::Assistant(frame) => self.assistant(frame),
            ClaudeAdapterEvent::User(frame) => self.user(frame),
            ClaudeAdapterEvent::Result(frame) => self.result(frame),
            ClaudeAdapterEvent::Error(error) => self.fail_active(&error),
            ClaudeAdapterEvent::Eof => {
                self.fail_active("Claude Code process exited before turn result")
            }
            _ => {}
        }
    }
    fn assistant(&mut self, frame: turin_claude_protocol::Frame) {
        let events = match self.active.as_mut() {
            Some(active) => assistant_events(&mut self.plans, active, &frame),
            None => return,
        };
        for event in events {
            self.emit(event);
        }
    }
    fn user(&mut self, frame: turin_claude_protocol::Frame) {
        let events = match self.active.as_mut() {
            Some(active) => tool_result_events(&mut self.plans, active, &frame),
            None => return,
        };
        for event in events {
            self.emit(event);
        }
    }
    fn result(&mut self, frame: turin_claude_protocol::Frame) {
        let Some(active) = self.active.as_mut() else {
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
        active.thread.status = OrchestrationV2ProviderThreadStatus::Idle;
        active.thread.updated_at = now;
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
    /// The CLI went away before a result. An interrupt that was asked for
    /// ends the turn as interrupted, and the process is reusable only then.
    fn fail_active(&mut self, error: &str) {
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
        let closing = finish_running_tools(&mut active, terminal_item_status(status));
        for event in closing {
            self.emit(event);
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

/// Assistant text blocks become a node, message, and turn item. `tool_use`
/// blocks start a tool call. Thinking blocks are not ported yet.
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
    for (index, block) in blocks.iter().enumerate() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                let native = format!("{message_id}:text:{index}");
                let ordinal = item_ordinal(
                    &mut active.item_ordinals,
                    active.input.provider_turn_ordinal,
                    &native,
                );
                let artifacts = assistant_artifacts(&active.scope, &native, ordinal, text, now);
                active.last_assistant = Some(artifacts.message.clone());
                events.push(ProviderAdapterV2Event::NodeUpdated {
                    driver: driver(),
                    node: artifacts.node,
                });
                events.push(ProviderAdapterV2Event::MessageUpdated {
                    driver: driver(),
                    message: artifacts.message,
                });
                events.push(ProviderAdapterV2Event::TurnItemUpdated {
                    driver: driver(),
                    turn_item: artifacts.turn_item,
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
                let input = block.get("input").cloned().unwrap_or(Value::Null);
                // TodoWrite projects its own list before the call item, as
                // upstream does. Subagent frames never touch the main list.
                if name == "TodoWrite" && parent_tool_use_id(frame).is_none() {
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
                    output: None,
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

fn parent_tool_use_id(frame: &turin_claude_protocol::Frame) -> Option<&str> {
    frame.raw.get("parent_tool_use_id").and_then(Value::as_str)
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
        let output = block.get("content").cloned().unwrap_or(Value::Null);
        let task_output = structured.cloned().unwrap_or_else(|| output.clone());
        let call = ToolCall {
            native_item_id: native,
            tool_name: &tracked.name,
            input: &tracked.input,
            output: Some(&output),
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
        // A subagent's task tools must not replace the parent's list.
        if let Some(native_thread) = native_thread
            .as_deref()
            .filter(|_| !failed && parent_tool_use_id(frame).is_none())
        {
            let current = plans.task_list(native_thread);
            if let Some(tasks) =
                apply_task_tool(&current, call.tool_name, call.input, Some(&task_output))
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
            output: None,
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
