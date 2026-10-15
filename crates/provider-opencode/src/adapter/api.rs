//! Mirror of `turin_provider_core`'s in-progress `ProviderAdapterV2`.
//!
//! Port of upstream/packages/provider-core/src/server/ProviderAdapter.ts.
//! T08 has not landed the trait on this branch. Method names and inputs match
//! the trait in `task/T08-provider-core` so the impl can move onto it when
//! that module is public. Optional capabilities OpenCode does not offer
//! (MCP apps, feedback upload, history injection) are absent.

use std::pin::Pin;

use futures::Stream;
use serde_json::Value;
use turin_contracts::{
    CheckpointId, MessageId, NodeId, ProviderSessionId, ProviderThreadId, ProviderTurnId, RunAttemptId,
    RunId, RuntimeRequestId, ScheduledTaskId, ThreadId,
    chat_attachment::ChatAttachment,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2AppThread, OrchestrationV2ConversationMessage,
        OrchestrationV2CreationSource, OrchestrationV2ProviderCapabilities,
        OrchestrationV2ProviderFailure, OrchestrationV2ProviderSession,
        OrchestrationV2ProviderThread, OrchestrationV2ProviderTurn, OrchestrationV2RuntimeRequest,
        OrchestrationV2Subagent, OrchestrationV2TurnItem,
    },
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
    provider_policy::{
        ProviderApprovalDecision, ProviderInteractionMode, ProviderUserInputAnswers, RuntimeMode,
    },
};

use crate::adapter::logic::{ProviderAdapterV2TurnTerminalStatus, ThreadDisposition};

pub type ProviderEventStream =
    Pin<Box<dyn Stream<Item = Result<ProviderAdapterV2Event, ProviderAdapterV2Error>> + Send>>;

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2RuntimePolicy {
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub cwd: Option<String>,
    pub approval_policy: Option<Value>,
    pub sandbox_policy: Option<Value>,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2TurnMessage {
    pub message_id: MessageId,
    pub text: String,
    pub attachments: Vec<ChatAttachment>,
    pub created_by: OrchestrationV2Actor,
    pub creation_source: OrchestrationV2CreationSource,
    pub scheduled_task_id: Option<ScheduledTaskId>,
    pub sender_thread_id: Option<ThreadId>,
}

#[derive(Debug, Clone)]
pub enum ProviderAdapterV2Event {
    ProviderSessionUpdated {
        driver: ProviderDriverKind,
        provider_session: OrchestrationV2ProviderSession,
    },
    ProviderThreadUpdated {
        driver: ProviderDriverKind,
        provider_thread: OrchestrationV2ProviderThread,
    },
    ProviderTurnUpdated {
        driver: ProviderDriverKind,
        thread_id: Option<ThreadId>,
        provider_turn: OrchestrationV2ProviderTurn,
    },
    SubagentUpdated {
        driver: ProviderDriverKind,
        subagent: OrchestrationV2Subagent,
    },
    TurnItemUpdated {
        driver: ProviderDriverKind,
        turn_item: OrchestrationV2TurnItem,
    },
    RuntimeRequestUpdated {
        driver: ProviderDriverKind,
        thread_id: Option<ThreadId>,
        runtime_request: OrchestrationV2RuntimeRequest,
    },
    TurnTerminal {
        driver: ProviderDriverKind,
        provider_thread_id: ProviderThreadId,
        provider_turn_id: ProviderTurnId,
        run_ordinal: u64,
        status: ProviderAdapterV2TurnTerminalStatus,
        failure: Option<OrchestrationV2ProviderFailure>,
        thread_disposition: ThreadDisposition,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderAdapterErrorKind {
    Capabilities {
        driver: ProviderDriverKind,
    },
    OpenSession {
        driver: ProviderDriverKind,
        provider_session_id: ProviderSessionId,
    },
    EnsureThread {
        driver: ProviderDriverKind,
        thread_id: ThreadId,
    },
    ResumeThread {
        driver: ProviderDriverKind,
        provider_thread_id: ProviderThreadId,
    },
    TurnStart {
        driver: ProviderDriverKind,
        thread_id: ThreadId,
        provider_thread_id: ProviderThreadId,
        run_id: RunId,
    },
    SteerRun {
        driver: ProviderDriverKind,
        provider_thread_id: ProviderThreadId,
        provider_turn_id: ProviderTurnId,
    },
    Interrupt {
        driver: ProviderDriverKind,
        provider_turn_id: ProviderTurnId,
    },
    RuntimeRequestResponse {
        driver: ProviderDriverKind,
        request_id: RuntimeRequestId,
    },
    ReadThreadSnapshot {
        driver: ProviderDriverKind,
        provider_thread_id: ProviderThreadId,
    },
    RollbackThread {
        driver: ProviderDriverKind,
        provider_thread_id: ProviderThreadId,
    },
    ForkThread {
        driver: ProviderDriverKind,
        provider_thread_id: ProviderThreadId,
    },
    EventStream {
        driver: ProviderDriverKind,
        provider_session_id: ProviderSessionId,
    },
    Protocol {
        driver: ProviderDriverKind,
        detail: String,
    },
}

impl ProviderAdapterErrorKind {
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Capabilities { .. } => "ProviderAdapterCapabilitiesError",
            Self::OpenSession { .. } => "ProviderAdapterOpenSessionError",
            Self::EnsureThread { .. } => "ProviderAdapterEnsureThreadError",
            Self::ResumeThread { .. } => "ProviderAdapterResumeThreadError",
            Self::TurnStart { .. } => "ProviderAdapterTurnStartError",
            Self::SteerRun { .. } => "ProviderAdapterSteerRunError",
            Self::Interrupt { .. } => "ProviderAdapterInterruptError",
            Self::RuntimeRequestResponse { .. } => "ProviderAdapterRuntimeRequestResponseError",
            Self::ReadThreadSnapshot { .. } => "ProviderAdapterReadThreadSnapshotError",
            Self::RollbackThread { .. } => "ProviderAdapterRollbackThreadError",
            Self::ForkThread { .. } => "ProviderAdapterForkThreadError",
            Self::EventStream { .. } => "ProviderAdapterEventStreamError",
            Self::Protocol { .. } => "ProviderAdapterProtocolError",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAdapterV2Error {
    pub kind: ProviderAdapterErrorKind,
}

impl ProviderAdapterV2Error {
    pub fn new(kind: ProviderAdapterErrorKind) -> Self {
        Self { kind }
    }

    pub fn protocol(detail: impl Into<String>) -> Self {
        Self::new(ProviderAdapterErrorKind::Protocol {
            driver: crate::adapter::logic::opencode_driver(),
            detail: detail.into(),
        })
    }

    pub fn tag(&self) -> &'static str {
        self.kind.tag()
    }
}

impl std::fmt::Display for ProviderAdapterV2Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            ProviderAdapterErrorKind::Capabilities { driver } => {
                write!(f, "Failed to read {driver} provider capabilities.")
            }
            ProviderAdapterErrorKind::OpenSession {
                driver,
                provider_session_id,
            } => write!(
                f,
                "Failed to open {driver} provider session {provider_session_id}."
            ),
            ProviderAdapterErrorKind::EnsureThread { driver, thread_id } => write!(
                f,
                "Failed to ensure {driver} provider thread for app thread {thread_id}."
            ),
            ProviderAdapterErrorKind::ResumeThread {
                driver,
                provider_thread_id,
            } => write!(
                f,
                "Failed to resume {driver} provider thread {provider_thread_id}."
            ),
            ProviderAdapterErrorKind::TurnStart {
                driver,
                provider_thread_id,
                run_id,
                ..
            } => write!(
                f,
                "Failed to start run {run_id} on {driver} provider thread {provider_thread_id}."
            ),
            ProviderAdapterErrorKind::SteerRun {
                driver,
                provider_thread_id,
                provider_turn_id,
            } => write!(
                f,
                "Failed to steer active run {provider_turn_id} on {driver} provider thread {provider_thread_id}."
            ),
            ProviderAdapterErrorKind::Interrupt {
                driver,
                provider_turn_id,
            } => write!(f, "Failed to interrupt {driver} provider turn {provider_turn_id}."),
            ProviderAdapterErrorKind::RuntimeRequestResponse { driver, request_id } => write!(
                f,
                "Failed to respond to {driver} runtime request {request_id}."
            ),
            ProviderAdapterErrorKind::ReadThreadSnapshot {
                driver,
                provider_thread_id,
            } => write!(
                f,
                "Failed to read {driver} provider thread snapshot {provider_thread_id}."
            ),
            ProviderAdapterErrorKind::RollbackThread {
                driver,
                provider_thread_id,
            } => write!(
                f,
                "Failed to roll back {driver} provider thread {provider_thread_id}."
            ),
            ProviderAdapterErrorKind::ForkThread {
                driver,
                provider_thread_id,
            } => write!(
                f,
                "Failed to fork {driver} provider thread {provider_thread_id}."
            ),
            ProviderAdapterErrorKind::EventStream {
                driver,
                provider_session_id,
            } => write!(
                f,
                "Failed while streaming {driver} provider session {provider_session_id} events."
            ),
            ProviderAdapterErrorKind::Protocol { driver, detail } => {
                write!(f, "{driver} provider protocol error: {detail}.")
            }
        }
    }
}

impl std::error::Error for ProviderAdapterV2Error {}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2OpenSessionInput {
    pub thread_id: ThreadId,
    pub provider_session_id: ProviderSessionId,
    pub model_selection: ModelSelection,
    pub runtime_policy: ProviderAdapterV2RuntimePolicy,
    pub resume_from_session: Option<OrchestrationV2ProviderSession>,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2EnsureThreadInput {
    pub thread_id: ThreadId,
    pub title: Option<String>,
    pub model_selection: ModelSelection,
    pub runtime_policy: ProviderAdapterV2RuntimePolicy,
    pub existing_provider_thread: Option<OrchestrationV2ProviderThread>,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2TurnInput {
    pub app_thread: OrchestrationV2AppThread,
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub run_ordinal: i64,
    pub provider_turn_ordinal: i64,
    pub attempt_id: RunAttemptId,
    pub root_node_id: NodeId,
    pub provider_thread: OrchestrationV2ProviderThread,
    pub message: ProviderAdapterV2TurnMessage,
    pub model_selection: ModelSelection,
    pub runtime_policy: ProviderAdapterV2RuntimePolicy,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2SteerInput {
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub provider_thread: OrchestrationV2ProviderThread,
    pub provider_turn_id: ProviderTurnId,
    pub message: ProviderAdapterV2TurnMessage,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2InterruptInput {
    pub provider_thread: OrchestrationV2ProviderThread,
    pub provider_turn_id: ProviderTurnId,
    pub request_runtime_restart: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2RuntimeRequestResponseInput {
    pub request_id: RuntimeRequestId,
    pub decision: Option<ProviderApprovalDecision>,
    pub answers: Option<ProviderUserInputAnswers>,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2ThreadSnapshot {
    pub provider_thread: OrchestrationV2ProviderThread,
    pub provider_turns: Vec<OrchestrationV2ProviderTurn>,
    pub messages: Vec<OrchestrationV2ConversationMessage>,
    pub runtime_requests: Vec<OrchestrationV2RuntimeRequest>,
    pub provider_payload: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2ReadThreadSnapshotInput {
    pub provider_thread: OrchestrationV2ProviderThread,
}

#[derive(Debug, Clone)]
pub enum ProviderAdapterV2RollbackTarget {
    ThreadStart { checkpoint_id: CheckpointId },
    ProviderTurn {
        checkpoint_id: CheckpointId,
        provider_turn: OrchestrationV2ProviderTurn,
    },
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2RollbackThreadInput {
    pub provider_thread: OrchestrationV2ProviderThread,
    pub target: ProviderAdapterV2RollbackTarget,
    pub provider_thread_turns: Vec<OrchestrationV2ProviderTurn>,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2ForkThreadInput {
    pub source_provider_thread: OrchestrationV2ProviderThread,
    pub source_provider_turns: Option<Vec<OrchestrationV2ProviderTurn>>,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub target_thread_id: ThreadId,
    pub model_selection: Option<ModelSelection>,
    pub runtime_policy: Option<ProviderAdapterV2RuntimePolicy>,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2ResumeThreadInput {
    pub provider_thread: OrchestrationV2ProviderThread,
    pub thread_id: Option<ThreadId>,
    pub model_selection: Option<ModelSelection>,
    pub runtime_policy: Option<ProviderAdapterV2RuntimePolicy>,
}

#[derive(Debug, Clone)]
pub struct ProviderSelectionTransitionInput {
    pub current: ModelSelection,
    pub target: ModelSelection,
    pub session_capabilities: OrchestrationV2ProviderCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderSelectionTransitionPlan {
    ApplyOnNextTurn,
}

pub fn turn_scoped_selection_transition() -> ProviderSelectionTransitionPlan {
    ProviderSelectionTransitionPlan::ApplyOnNextTurn
}

#[async_trait::async_trait]
pub trait ProviderSessionRuntime: Send + Sync {
    fn instance_id(&self) -> &ProviderInstanceId;
    fn driver(&self) -> &ProviderDriverKind;
    fn provider_session_id(&self) -> &ProviderSessionId;
    fn provider_session(&self) -> OrchestrationV2ProviderSession;
    fn events(&self) -> ProviderEventStream;
    async fn close(&self);
    fn has_pending_background_work(&self) -> bool;
    fn get_model_context_window(&self, model: &str) -> Option<u64>;

    async fn ensure_thread(
        &self,
        input: ProviderAdapterV2EnsureThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error>;

    async fn resume_thread(
        &self,
        input: ProviderAdapterV2ResumeThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error>;

    async fn start_turn(
        &self,
        input: ProviderAdapterV2TurnInput,
    ) -> Result<(), ProviderAdapterV2Error>;

    async fn compact_thread(
        &self,
        input: ProviderAdapterV2TurnInput,
    ) -> Result<(), ProviderAdapterV2Error>;

    async fn steer_turn(
        &self,
        input: ProviderAdapterV2SteerInput,
    ) -> Result<(), ProviderAdapterV2Error>;

    async fn interrupt_turn(
        &self,
        input: ProviderAdapterV2InterruptInput,
    ) -> Result<(), ProviderAdapterV2Error>;

    async fn respond_to_runtime_request(
        &self,
        input: ProviderAdapterV2RuntimeRequestResponseInput,
    ) -> Result<(), ProviderAdapterV2Error>;

    async fn read_thread_snapshot(
        &self,
        input: ProviderAdapterV2ReadThreadSnapshotInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error>;

    async fn rollback_thread(
        &self,
        input: ProviderAdapterV2RollbackThreadInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error>;

    async fn fork_thread(
        &self,
        input: ProviderAdapterV2ForkThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error>;
}

#[async_trait::async_trait]
pub trait ProviderAdapterV2: Send + Sync {
    fn instance_id(&self) -> &ProviderInstanceId;
    fn driver(&self) -> &ProviderDriverKind;

    async fn get_capabilities(
        &self,
    ) -> Result<OrchestrationV2ProviderCapabilities, ProviderAdapterV2Error>;

    async fn plan_selection_transition(
        &self,
        _input: ProviderSelectionTransitionInput,
    ) -> Result<ProviderSelectionTransitionPlan, ProviderAdapterV2Error> {
        Ok(turn_scoped_selection_transition())
    }

    async fn open_session(
        &self,
        input: ProviderAdapterV2OpenSessionInput,
    ) -> Result<std::sync::Arc<dyn ProviderSessionRuntime>, ProviderAdapterV2Error>;
}
