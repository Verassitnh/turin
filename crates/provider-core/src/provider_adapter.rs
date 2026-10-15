//! Port of upstream/packages/provider-core/src/server/ProviderAdapter.ts.
//!
//! `ProviderAdapterV2` is a trait because every kept provider (Codex, Claude,
//! OpenCode, Cursor) implements it. Optional runtime methods return `None`
//! when upstream leaves the property unset; a `Some` is the capability.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use turin_contracts::{
    CheckpointId, MessageId, NodeId, PositiveInt, ProviderSessionId, ProviderThreadId,
    ProviderTurnId, RunAttemptId, RunId, RuntimeRequestId, ScheduledTaskId, ThreadId, UtcDateTime,
    chat_attachment::ChatAttachment,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2AppThread, OrchestrationV2ConversationMessage,
        OrchestrationV2CreationSource, OrchestrationV2ExecutionNode,
        OrchestrationV2HistoricalMessage, OrchestrationV2PlanArtifact,
        OrchestrationV2ProviderCapabilities, OrchestrationV2ProviderFailure,
        OrchestrationV2ProviderRetry, OrchestrationV2ProviderSession,
        OrchestrationV2ProviderThread, OrchestrationV2ProviderThreadDisposition,
        OrchestrationV2ProviderTurn, OrchestrationV2RuntimeRequest, OrchestrationV2Subagent,
        OrchestrationV2TurnItem,
    },
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
    provider_policy::{
        ProviderApprovalDecision, ProviderInteractionMode, ProviderUserInputAnswers, RuntimeMode,
    },
};

use crate::{
    failure::ProviderFailureCause,
    rt::{BoxFuture, BoxStream},
    selection_transition::{ProviderSelectionTransitionInput, ProviderSelectionTransitionPlan},
};

pub type ProviderEventStream =
    BoxStream<'static, Result<ProviderAdapterV2Event, ProviderAdapterV2Error>>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAdapterV2RuntimePolicy {
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_policy: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAdapterV2TurnMessage {
    pub message_id: MessageId,
    pub text: String,
    pub attachments: Vec<ChatAttachment>,
    pub created_by: OrchestrationV2Actor,
    pub creation_source: OrchestrationV2CreationSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_task_id: Option<ScheduledTaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_thread_id: Option<ThreadId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderAdapterV2SessionStatus {
    Starting,
    Ready,
    Running,
    Waiting,
    Stopped,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderAdapterV2TurnTerminalStatus {
    Completed,
    Interrupted,
    Cancelled,
    Failed,
}

/// `ProviderAdapterV2Event`. `type` is the upstream discriminator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderAdapterV2Event {
    #[serde(rename = "app_thread.created")]
    AppThreadCreated {
        driver: ProviderDriverKind,
        app_thread: OrchestrationV2AppThread,
    },
    #[serde(rename = "provider_session.updated")]
    ProviderSessionUpdated {
        driver: ProviderDriverKind,
        provider_session: OrchestrationV2ProviderSession,
    },
    /// `retained_native_turn_ids` is set when the provider rewound its own
    /// conversation outside Turin. The ingestor rolls back earlier runs whose
    /// turns fell off the active branch.
    #[serde(rename = "provider_thread.updated")]
    ProviderThreadUpdated {
        driver: ProviderDriverKind,
        provider_thread: OrchestrationV2ProviderThread,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retained_native_turn_ids: Option<Vec<String>>,
    },
    #[serde(rename = "provider_turn.updated")]
    ProviderTurnUpdated {
        driver: ProviderDriverKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_id: Option<ThreadId>,
        provider_turn: OrchestrationV2ProviderTurn,
    },
    #[serde(rename = "node.updated")]
    NodeUpdated {
        driver: ProviderDriverKind,
        node: OrchestrationV2ExecutionNode,
    },
    #[serde(rename = "subagent.updated")]
    SubagentUpdated {
        driver: ProviderDriverKind,
        subagent: OrchestrationV2Subagent,
    },
    #[serde(rename = "message.updated")]
    MessageUpdated {
        driver: ProviderDriverKind,
        message: OrchestrationV2ConversationMessage,
    },
    #[serde(rename = "turn_item.updated")]
    TurnItemUpdated {
        driver: ProviderDriverKind,
        turn_item: OrchestrationV2TurnItem,
    },
    #[serde(rename = "runtime_request.updated")]
    RuntimeRequestUpdated {
        driver: ProviderDriverKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_id: Option<ThreadId>,
        runtime_request: OrchestrationV2RuntimeRequest,
    },
    #[serde(rename = "plan.updated")]
    PlanUpdated {
        driver: ProviderDriverKind,
        plan: OrchestrationV2PlanArtifact,
    },
    #[serde(rename = "turn.terminal")]
    TurnTerminal {
        driver: ProviderDriverKind,
        provider_thread_id: ProviderThreadId,
        provider_turn_id: ProviderTurnId,
        run_ordinal: PositiveInt,
        status: ProviderAdapterV2TurnTerminalStatus,
        /// `null` unless `status` is `failed`.
        failure: Option<OrchestrationV2ProviderFailure>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure_item_ordinal: Option<PositiveInt>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry: Option<OrchestrationV2ProviderRetry>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_started_at: Option<UtcDateTime>,
        thread_disposition: OrchestrationV2ProviderThreadDisposition,
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
    CloseSession {
        driver: ProviderDriverKind,
        provider_session_id: ProviderSessionId,
    },
    ResumeThread {
        driver: ProviderDriverKind,
        provider_session_id: ProviderSessionId,
        provider_thread_id: ProviderThreadId,
    },
    EnsureThread {
        driver: ProviderDriverKind,
        thread_id: ThreadId,
    },
    ReadThreadSnapshot {
        driver: ProviderDriverKind,
        provider_thread_id: ProviderThreadId,
    },
    RollbackThread {
        driver: ProviderDriverKind,
        provider_thread_id: ProviderThreadId,
        checkpoint_id: Option<CheckpointId>,
    },
    ForkThread {
        driver: ProviderDriverKind,
        provider_thread_id: ProviderThreadId,
    },
    TurnStart {
        driver: ProviderDriverKind,
        thread_id: ThreadId,
        provider_thread_id: ProviderThreadId,
        run_id: RunId,
    },
    SteerRunUnsupported {
        driver: ProviderDriverKind,
        provider_thread_id: ProviderThreadId,
    },
    SteerRun {
        driver: ProviderDriverKind,
        provider_thread_id: ProviderThreadId,
        provider_turn_id: ProviderTurnId,
    },
    Interrupt {
        driver: ProviderDriverKind,
        provider_thread_id: ProviderThreadId,
        provider_turn_id: ProviderTurnId,
    },
    RuntimeRequestResponse {
        driver: ProviderDriverKind,
        request_id: RuntimeRequestId,
    },
    EventStream {
        driver: ProviderDriverKind,
        provider_session_id: ProviderSessionId,
    },
    Protocol {
        driver: ProviderDriverKind,
        detail: String,
        payload: Option<Value>,
    },
}

impl ProviderAdapterErrorKind {
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Capabilities { .. } => "ProviderAdapterCapabilitiesError",
            Self::OpenSession { .. } => "ProviderAdapterOpenSessionError",
            Self::CloseSession { .. } => "ProviderAdapterCloseSessionError",
            Self::ResumeThread { .. } => "ProviderAdapterResumeThreadError",
            Self::EnsureThread { .. } => "ProviderAdapterEnsureThreadError",
            Self::ReadThreadSnapshot { .. } => "ProviderAdapterReadThreadSnapshotError",
            Self::RollbackThread { .. } => "ProviderAdapterRollbackThreadError",
            Self::ForkThread { .. } => "ProviderAdapterForkThreadError",
            Self::TurnStart { .. } => "ProviderAdapterTurnStartError",
            Self::SteerRunUnsupported { .. } => "ProviderAdapterSteerRunUnsupportedError",
            Self::SteerRun { .. } => "ProviderAdapterSteerRunError",
            Self::Interrupt { .. } => "ProviderAdapterInterruptError",
            Self::RuntimeRequestResponse { .. } => "ProviderAdapterRuntimeRequestResponseError",
            Self::EventStream { .. } => "ProviderAdapterEventStreamError",
            Self::Protocol { .. } => "ProviderAdapterProtocolError",
        }
    }

    pub fn driver(&self) -> &ProviderDriverKind {
        match self {
            Self::Capabilities { driver }
            | Self::OpenSession { driver, .. }
            | Self::CloseSession { driver, .. }
            | Self::ResumeThread { driver, .. }
            | Self::EnsureThread { driver, .. }
            | Self::ReadThreadSnapshot { driver, .. }
            | Self::RollbackThread { driver, .. }
            | Self::ForkThread { driver, .. }
            | Self::TurnStart { driver, .. }
            | Self::SteerRunUnsupported { driver, .. }
            | Self::SteerRun { driver, .. }
            | Self::Interrupt { driver, .. }
            | Self::RuntimeRequestResponse { driver, .. }
            | Self::EventStream { driver, .. }
            | Self::Protocol { driver, .. } => driver,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderAdapterV2Error {
    pub kind: ProviderAdapterErrorKind,
    pub cause: Option<ProviderFailureCause>,
}

impl ProviderAdapterV2Error {
    pub fn new(kind: ProviderAdapterErrorKind) -> Self {
        Self { kind, cause: None }
    }

    pub fn with_cause(mut self, cause: ProviderFailureCause) -> Self {
        self.cause = Some(cause);
        self
    }

    pub fn tag(&self) -> &'static str {
        self.kind.tag()
    }

    /// Tagged cause so [`crate::failure::make_provider_failure`] can classify it.
    pub fn as_cause(&self) -> ProviderFailureCause {
        ProviderFailureCause::Tagged {
            tag: self.tag().to_owned(),
            message: Some(self.to_string()),
            code: None,
            cause: self.cause.clone().map(Box::new),
        }
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
            ProviderAdapterErrorKind::CloseSession {
                driver,
                provider_session_id,
            } => write!(
                f,
                "Failed to close {driver} provider session {provider_session_id}."
            ),
            ProviderAdapterErrorKind::ResumeThread {
                driver,
                provider_thread_id,
                ..
            } => write!(
                f,
                "Failed to resume {driver} provider thread {provider_thread_id}."
            ),
            ProviderAdapterErrorKind::EnsureThread { driver, thread_id } => write!(
                f,
                "Failed to ensure {driver} provider thread for app thread {thread_id}."
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
                ..
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
            ProviderAdapterErrorKind::TurnStart {
                driver,
                provider_thread_id,
                run_id,
                ..
            } => write!(
                f,
                "Failed to start run {run_id} on {driver} provider thread {provider_thread_id}."
            ),
            ProviderAdapterErrorKind::SteerRunUnsupported {
                driver,
                provider_thread_id,
            } => write!(
                f,
                "{driver} provider thread {provider_thread_id} does not support active-run steering."
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
                ..
            } => write!(
                f,
                "Failed to interrupt {driver} provider turn {provider_turn_id}."
            ),
            ProviderAdapterErrorKind::RuntimeRequestResponse { driver, request_id } => {
                write!(
                    f,
                    "Failed to respond to {driver} runtime request {request_id}."
                )
            }
            ProviderAdapterErrorKind::EventStream {
                driver,
                provider_session_id,
            } => write!(
                f,
                "Failed while streaming {driver} provider session {provider_session_id} events."
            ),
            ProviderAdapterErrorKind::Protocol { driver, detail, .. } => {
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
    /// Native thread to activate while an eager adapter opens its provider process.
    pub initial_native_thread_id: Option<String>,
    /// Preserves provider item identity across eager activation of a persisted thread.
    pub initial_provider_item_identity_version: Option<u8>,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2EnsureThreadInput {
    pub thread_id: ThreadId,
    /// The app thread's title, for providers that name their native sessions.
    pub title: Option<String>,
    pub model_selection: ModelSelection,
    pub runtime_policy: ProviderAdapterV2RuntimePolicy,
    pub provider_session_id: Option<ProviderSessionId>,
    pub existing_provider_thread: Option<OrchestrationV2ProviderThread>,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2AppContext {
    pub key: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2TurnInput {
    pub app_thread: OrchestrationV2AppThread,
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub run_ordinal: i64,
    /// Whether the current native session has an accepted turn; omitted when unknown.
    pub native_thread_has_turns: Option<bool>,
    pub provider_turn_ordinal: i64,
    pub restart_continuation_of_run_id: Option<RunId>,
    pub attempt_id: RunAttemptId,
    pub root_node_id: NodeId,
    pub provider_thread: OrchestrationV2ProviderThread,
    pub message: ProviderAdapterV2TurnMessage,
    pub model_selection: ModelSelection,
    pub runtime_policy: ProviderAdapterV2RuntimePolicy,
    /// What the thread's MCP Apps want the agent to know. The apps host is
    /// cut; adapters that still receive a list deliver it as application context.
    pub app_context: Option<Vec<ProviderAdapterV2AppContext>>,
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
    /// When true, the next `start_turn` may respawn the provider runtime.
    pub request_runtime_restart: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2RuntimeRequestResponseInput {
    pub request_id: RuntimeRequestId,
    pub decision: Option<ProviderApprovalDecision>,
    pub answers: Option<ProviderUserInputAnswers>,
    pub response: Option<Value>,
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
#[allow(clippy::large_enum_variant)]
pub enum ProviderAdapterV2RollbackTarget {
    ThreadStart {
        checkpoint_id: CheckpointId,
        app_run_ordinal: u8,
    },
    ProviderTurn {
        checkpoint_id: CheckpointId,
        app_run_ordinal: i64,
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
    pub owner_node_id: Option<NodeId>,
    pub model_selection: Option<ModelSelection>,
    pub runtime_policy: Option<ProviderAdapterV2RuntimePolicy>,
}

pub struct ProviderAdapterV2EventSubscription {
    pub events: ProviderEventStream,
    pub close: BoxFuture<'static, ()>,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2HistoricalContext {
    pub messages: Vec<OrchestrationV2HistoricalMessage>,
    pub context: String,
}

/// An MCP tool as the provider's MCP client lists it.
#[derive(Debug, Clone)]
pub struct ProviderAdapterV2McpTool {
    pub name: String,
    pub meta: Option<Value>,
    pub annotations: Option<Value>,
}

/// MCP `CallToolResult`, passed through unchanged.
#[derive(Debug, Clone)]
pub struct ProviderAdapterV2McpCallToolResult {
    pub content: Vec<Value>,
    pub structured_content: Option<Value>,
    pub is_error: Option<bool>,
    pub meta: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2McpReadResourceResult {
    pub contents: Vec<Value>,
}

#[async_trait::async_trait]
pub trait ProviderAdapterV2McpApps: Send + Sync {
    async fn list_tools(
        &self,
        provider_thread: &OrchestrationV2ProviderThread,
        server: &str,
    ) -> Result<Vec<ProviderAdapterV2McpTool>, ProviderAdapterV2Error>;

    async fn call_tool(
        &self,
        provider_thread: &OrchestrationV2ProviderThread,
        server: &str,
        tool: &str,
        arguments: &serde_json::Map<String, Value>,
    ) -> Result<ProviderAdapterV2McpCallToolResult, ProviderAdapterV2Error>;

    async fn read_resource(
        &self,
        provider_thread: &OrchestrationV2ProviderThread,
        server: &str,
        uri: &str,
    ) -> Result<ProviderAdapterV2McpReadResourceResult, ProviderAdapterV2Error>;
}

/// A terminal task outcome observed by the provider, not inferred from a stop timeout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAdapterV2SubagentOutcome {
    pub status: ProviderAdapterV2SubagentOutcomeStatus,
    pub completed_at: UtcDateTime,
    pub result: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderAdapterV2SubagentOutcomeStatus {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2ResumeThreadInput {
    pub provider_thread: OrchestrationV2ProviderThread,
    pub thread_id: Option<ThreadId>,
    pub model_selection: Option<ModelSelection>,
    pub runtime_policy: Option<ProviderAdapterV2RuntimePolicy>,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2StopTaskInput {
    pub provider_thread: OrchestrationV2ProviderThread,
    pub task_id: String,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2StopSubagentInput {
    pub provider_thread: OrchestrationV2ProviderThread,
    pub native_task_id: String,
    /// A delayed Stop must not interrupt a task resumed after this request.
    pub requested_at: Option<UtcDateTime>,
}

#[derive(Debug, Clone)]
pub struct ProviderAdapterV2UploadFeedbackInput {
    pub provider_thread: OrchestrationV2ProviderThread,
    pub reason: Option<String>,
}

#[async_trait::async_trait]
pub trait ProviderSessionRuntime: Send + Sync {
    fn instance_id(&self) -> &ProviderInstanceId;
    fn driver(&self) -> &ProviderDriverKind;
    fn provider_session_id(&self) -> &ProviderSessionId;
    fn provider_session(&self) -> &OrchestrationV2ProviderSession;
    /// Single-consumer event stream. Manager-owned runtimes also expose
    /// [`Self::subscribe_events`] so concurrent threads each get a copy.
    fn events(&self) -> ProviderEventStream;

    fn subscribe_events(
        &self,
    ) -> Option<BoxFuture<'_, Result<ProviderAdapterV2EventSubscription, ProviderAdapterV2Error>>>
    {
        None
    }

    fn has_pending_background_work(
        &self,
    ) -> Option<BoxFuture<'_, Result<bool, ProviderAdapterV2Error>>> {
        None
    }

    fn has_pending_background_work_for_thread(
        &self,
        _provider_thread: &OrchestrationV2ProviderThread,
    ) -> Option<BoxFuture<'_, Result<bool, ProviderAdapterV2Error>>> {
        None
    }

    /// `true` only when every background task ends with this session's process.
    fn background_tasks_end_with_process(&self) -> bool {
        false
    }

    /// `None` when the runtime has no probe. `Some(None)` is an unknown window.
    fn get_model_context_window(
        &self,
        _model_selection: &ModelSelection,
        _cwd: Option<&str>,
    ) -> Option<Option<u64>> {
        None
    }

    /// `None` when the transition is unknown and usage must be invalidated.
    fn can_reuse_context_usage(
        &self,
        _previous: &ModelSelection,
        _next: &ModelSelection,
    ) -> Option<bool> {
        None
    }

    async fn ensure_thread(
        &self,
        input: ProviderAdapterV2EnsureThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error>;

    async fn resume_thread(
        &self,
        input: ProviderAdapterV2ResumeThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error>;

    /// `None` means the runtime has no history-injection hook. A `Some` future
    /// resolves `false` when the native protocol explicitly does not support it.
    fn inject_history(
        &self,
        _provider_thread: &OrchestrationV2ProviderThread,
        _context: &ProviderAdapterV2HistoricalContext,
    ) -> Option<BoxFuture<'_, Result<bool, ProviderAdapterV2Error>>> {
        None
    }

    async fn start_turn(
        &self,
        input: ProviderAdapterV2TurnInput,
    ) -> Result<(), ProviderAdapterV2Error>;

    fn compact_thread(
        &self,
        _input: ProviderAdapterV2TurnInput,
    ) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        None
    }

    async fn steer_turn(
        &self,
        input: ProviderAdapterV2SteerInput,
    ) -> Result<(), ProviderAdapterV2Error>;

    fn stop_task(
        &self,
        _input: ProviderAdapterV2StopTaskInput,
    ) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        None
    }

    async fn interrupt_turn(
        &self,
        input: ProviderAdapterV2InterruptInput,
    ) -> Result<(), ProviderAdapterV2Error>;

    /// `None` from the future is upstream's `void` outcome (the owner keeps running).
    fn stop_subagent(
        &self,
        _input: ProviderAdapterV2StopSubagentInput,
    ) -> Option<
        BoxFuture<'_, Result<Option<ProviderAdapterV2SubagentOutcome>, ProviderAdapterV2Error>>,
    > {
        None
    }

    fn unload_thread(
        &self,
        _provider_thread: &OrchestrationV2ProviderThread,
    ) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        None
    }

    async fn respond_to_runtime_request(
        &self,
        input: ProviderAdapterV2RuntimeRequestResponseInput,
    ) -> Result<(), ProviderAdapterV2Error>;

    async fn read_thread_snapshot(
        &self,
        input: ProviderAdapterV2ReadThreadSnapshotInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error>;

    fn upload_feedback(
        &self,
        _input: ProviderAdapterV2UploadFeedbackInput,
    ) -> Option<BoxFuture<'_, Result<String, ProviderAdapterV2Error>>> {
        None
    }

    fn mcp_apps(&self) -> Option<Arc<dyn ProviderAdapterV2McpApps>> {
        None
    }

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
        input: ProviderSelectionTransitionInput,
    ) -> Result<ProviderSelectionTransitionPlan, ProviderAdapterV2Error>;

    async fn open_session(
        &self,
        input: ProviderAdapterV2OpenSessionInput,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderAdapterV2Error>;
}
