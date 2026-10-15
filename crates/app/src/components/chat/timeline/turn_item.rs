//! UI-side mirror of `OrchestrationV2TurnItem` / `OrchestrationV2ProjectedTurnItem`
//! from upstream/packages/contracts/src/orchestrationV2.ts.
//!
//! The thread view reads projected turn items directly (status pills, error
//! rows, subagent groups, tool presentation). `turin-contracts` does not have
//! the orchestration-v2 schemas yet, so this module keeps the fields the
//! timeline reads, with upstream's names. It is plain data, not a wire type:
//! ids are strings, `DateTimeUtc` values are ISO strings, and `Schema.Unknown`
//! payloads are `serde_json::Value`.
//!
//! Seam: T14 converts the real projection into these values. When the
//! contracts land this module can become a re-export.

use serde_json::Value;

/// `OrchestrationV2TurnItemStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TurnItemStatus {
    Idle,
    Pending,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl TurnItemStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    /// `pending | running | waiting`: the item is still live.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Pending | Self::Running | Self::Waiting)
    }
}

/// `OrchestrationV2TurnItem["type"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TurnItemType {
    Notification,
    UserMessage,
    AssistantMessage,
    Reasoning,
    ProposedPlan,
    TodoList,
    UserInputRequest,
    FileChange,
    CommandExecution,
    FileSearch,
    WebSearch,
    ApprovalRequest,
    Checkpoint,
    RunInterruptRequest,
    RunInterruptResult,
    SystemNotice,
    Error,
    Compaction,
    Handoff,
    Fork,
    ThreadCreated,
    SecretRequest,
    Subagent,
    DynamicTool,
}

impl TurnItemType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Notification => "notification",
            Self::UserMessage => "user_message",
            Self::AssistantMessage => "assistant_message",
            Self::Reasoning => "reasoning",
            Self::ProposedPlan => "proposed_plan",
            Self::TodoList => "todo_list",
            Self::UserInputRequest => "user_input_request",
            Self::FileChange => "file_change",
            Self::CommandExecution => "command_execution",
            Self::FileSearch => "file_search",
            Self::WebSearch => "web_search",
            Self::ApprovalRequest => "approval_request",
            Self::Checkpoint => "checkpoint",
            Self::RunInterruptRequest => "run_interrupt_request",
            Self::RunInterruptResult => "run_interrupt_result",
            Self::SystemNotice => "system_notice",
            Self::Error => "error",
            Self::Compaction => "compaction",
            Self::Handoff => "handoff",
            Self::Fork => "fork",
            Self::ThreadCreated => "thread_created",
            Self::SecretRequest => "secret_request",
            Self::Subagent => "subagent",
            Self::DynamicTool => "dynamic_tool",
        }
    }
}

/// `ToolActivitySurface`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolActivitySurface {
    Browser,
    Computer,
}

/// `ToolActivityIcon` (favicons and app logos). Turin renders the fallback
/// glyph for these until remote images land; the variant still decides tint.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ToolActivityIcon {
    Website {
        page_url: String,
        favicon_url: Option<String>,
    },
    NativeApp {
        app: String,
    },
    ThemedLogo {
        logo_url: String,
    },
}

/// `ToolActivitySource`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ToolActivitySource {
    pub key: String,
    pub name: String,
    /// `browser | computer | integration`.
    pub kind: String,
    pub icon: Option<ToolActivityIcon>,
}

/// `OrchestrationV2ProviderFailureClass`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderFailureClass {
    UsageLimit,
    ProviderError,
    TransportError,
    PermissionError,
    ValidationError,
    Unknown,
}

/// `OrchestrationV2ProviderFailure`.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderFailure {
    pub class: ProviderFailureClass,
    pub message: String,
    pub code: Option<String>,
    pub retryable: Option<bool>,
    pub reset_at: Option<String>,
}

/// `OrchestrationV2ProviderRetry`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderRetry {
    pub attempt: u64,
    pub max_attempts: Option<u64>,
    pub retry_delay_ms: Option<u64>,
}

/// `OrchestrationV2NotificationSource` (decoded form).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationSource {
    DelegatedTask {
        task_ids: Vec<String>,
        child_thread_id: Option<String>,
    },
    Subagent {
        child_thread_id: Option<String>,
    },
    Command,
    Monitor,
    BackgroundTask,
    System,
}

/// `OrchestrationV2Notification["outcome"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationOutcome {
    Completed,
    Failed,
    Cancelled,
    Updated,
    Unknown,
}

/// `OrchestrationV2PlanStep`.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanStep {
    pub id: String,
    pub text: String,
    /// `pending | running | completed`.
    pub status: PlanStepStatus,
    pub duration_anchor_at: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanStepStatus {
    Pending,
    Running,
    Completed,
}

/// `OrchestrationV2UserInputQuestion`.
#[derive(Clone, Debug, PartialEq)]
pub struct UserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<UserInputQuestionOption>,
    pub multi_select: bool,
    pub allow_custom_answer: Option<bool>,
    pub initial_answer: Option<String>,
    pub required: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UserInputQuestionOption {
    pub label: String,
    pub description: String,
    pub value: Option<String>,
}

/// `UserInputAttachmentAnswerPayload` (attachments reduced to names).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct UserInputAnswerPayload {
    pub request_id: String,
    /// Insertion-ordered `questionTextById`.
    pub question_text_by_id: Vec<(String, String)>,
    /// Insertion-ordered `answers` (values are `Schema.Unknown`).
    pub answers: Vec<(String, Value)>,
    /// Insertion-ordered attachment names per question.
    pub attachments_by_question_id: Vec<(String, Vec<String>)>,
}

/// `ProviderRequestKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderRequestKind {
    Command,
    FileRead,
    FileChange,
    McpElicitation,
    Permission,
}

/// `ProviderApprovalDecision`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderApprovalDecision {
    Accept,
    AcceptForSession,
    AcceptAlways,
    Decline,
    Cancel,
}

/// `ProviderApprovalOption`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderApprovalOption {
    pub decision: ProviderApprovalDecision,
    pub label: String,
    pub warning: Option<String>,
}

/// `OrchestrationV2CheckpointFileSummary` / `ThreadCheckpointSummary["files"][number]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointFileSummary {
    pub path: String,
    pub kind: String,
    pub additions: u64,
    pub deletions: u64,
}

/// The variant-specific half of an `OrchestrationV2TurnItem`.
#[derive(Clone, Debug, PartialEq)]
pub enum TurnItemPayload {
    Notification {
        source: NotificationSource,
        outcome: NotificationOutcome,
        summary: String,
        detail: Option<String>,
    },
    UserMessage {
        message_id: String,
        text: String,
    },
    AssistantMessage {
        message_id: String,
        text: String,
        streaming: bool,
    },
    Reasoning {
        text: String,
        streaming: bool,
    },
    ProposedPlan {
        plan_id: String,
        markdown: String,
        streaming: bool,
    },
    TodoList {
        plan_id: String,
        steps: Vec<PlanStep>,
        explanation: Option<String>,
    },
    UserInputRequest {
        request_id: String,
        questions: Vec<UserInputQuestion>,
        question_answer: Option<UserInputAnswerPayload>,
    },
    FileChange {
        file_name: String,
        additions: Option<u64>,
        deletions: Option<u64>,
        diff: Option<String>,
    },
    CommandExecution {
        input: String,
        output: Option<String>,
        output_omitted: bool,
        output_indicates_failure: Option<bool>,
        exit_code: Option<i64>,
    },
    FileSearch {
        pattern: Option<String>,
        /// `fileName` of each result.
        results: Vec<String>,
    },
    WebSearch {
        patterns: Vec<String>,
        /// `(title, url)` of each result.
        results: Vec<(Option<String>, Option<String>)>,
    },
    ApprovalRequest {
        request_id: String,
        request_kind: ProviderRequestKind,
        prompt: Option<String>,
        app_name: Option<String>,
        options: Vec<ProviderApprovalOption>,
    },
    Checkpoint {
        checkpoint_id: String,
        scope_id: String,
        files: Vec<CheckpointFileSummary>,
    },
    RunInterruptRequest {
        message: String,
    },
    RunInterruptResult {
        message: String,
    },
    SystemNotice {
        message: String,
    },
    Error {
        failure: ProviderFailure,
        retry: Option<ProviderRetry>,
    },
    Compaction {
        summary: Option<String>,
        before_token_count: Option<u64>,
        after_token_count: Option<u64>,
    },
    Handoff {
        to_model: Option<String>,
        summary: Option<String>,
    },
    Fork {
        target_thread_id: String,
    },
    ThreadCreated {
        target_thread_id: String,
        target_model: String,
    },
    SecretRequest {
        label: String,
        reason: String,
    },
    Subagent {
        subagent_id: String,
        /// `provider_native | app_owned`.
        origin: String,
        child_thread_id: Option<String>,
        prompt: String,
        progress: Option<String>,
        result: Option<String>,
    },
    DynamicTool {
        tool_name: Option<String>,
        viewed_image_path: Option<String>,
        input: Value,
        output: Option<Value>,
        output_omitted: bool,
    },
}

impl TurnItemPayload {
    pub fn item_type(&self) -> TurnItemType {
        match self {
            Self::Notification { .. } => TurnItemType::Notification,
            Self::UserMessage { .. } => TurnItemType::UserMessage,
            Self::AssistantMessage { .. } => TurnItemType::AssistantMessage,
            Self::Reasoning { .. } => TurnItemType::Reasoning,
            Self::ProposedPlan { .. } => TurnItemType::ProposedPlan,
            Self::TodoList { .. } => TurnItemType::TodoList,
            Self::UserInputRequest { .. } => TurnItemType::UserInputRequest,
            Self::FileChange { .. } => TurnItemType::FileChange,
            Self::CommandExecution { .. } => TurnItemType::CommandExecution,
            Self::FileSearch { .. } => TurnItemType::FileSearch,
            Self::WebSearch { .. } => TurnItemType::WebSearch,
            Self::ApprovalRequest { .. } => TurnItemType::ApprovalRequest,
            Self::Checkpoint { .. } => TurnItemType::Checkpoint,
            Self::RunInterruptRequest { .. } => TurnItemType::RunInterruptRequest,
            Self::RunInterruptResult { .. } => TurnItemType::RunInterruptResult,
            Self::SystemNotice { .. } => TurnItemType::SystemNotice,
            Self::Error { .. } => TurnItemType::Error,
            Self::Compaction { .. } => TurnItemType::Compaction,
            Self::Handoff { .. } => TurnItemType::Handoff,
            Self::Fork { .. } => TurnItemType::Fork,
            Self::ThreadCreated { .. } => TurnItemType::ThreadCreated,
            Self::SecretRequest { .. } => TurnItemType::SecretRequest,
            Self::Subagent { .. } => TurnItemType::Subagent,
            Self::DynamicTool { .. } => TurnItemType::DynamicTool,
        }
    }
}

/// `OrchestrationV2TurnItem`: the shared base fields plus the variant payload.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnItem {
    pub id: String,
    pub thread_id: String,
    pub run_id: Option<String>,
    pub node_id: Option<String>,
    pub parent_item_id: Option<String>,
    pub ordinal: u64,
    pub status: TurnItemStatus,
    pub title: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub updated_at: String,
    pub tool_surface: Option<ToolActivitySurface>,
    pub tool_icon: Option<ToolActivityIcon>,
    pub tool_source: Option<ToolActivitySource>,
    pub payload: TurnItemPayload,
}

impl TurnItem {
    pub fn item_type(&self) -> TurnItemType {
        self.payload.item_type()
    }
}

/// `OrchestrationV2ProjectedTurnItem["visibility"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectedVisibility {
    Local,
    Inherited,
    Synthetic,
}

/// `OrchestrationV2ProjectedTurnItem`.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectedTurnItem {
    pub position: u64,
    pub visibility: ProjectedVisibility,
    pub source_thread_id: String,
    pub source_item_id: String,
    pub item: TurnItem,
}
