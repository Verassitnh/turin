//! Port of the timeline-facing types in upstream/apps/web/src/types.ts and
//! upstream/apps/web/src/session-logic.ts (`ChatMessage`, `ProposedPlan`,
//! `TurnDiffSummary`, `WorkLogEntry`, `TimelineEntry`), plus the slices of
//! client-runtime state the thread view reads (`ThreadRunSummary`,
//! `ThreadPendingApproval`, `ThreadPendingUserInput`).
//!
//! [`ThreadTimelineSnapshot`] is the seam between the orchestration read
//! model and the view: everything `MessagesTimeline` and the composer's
//! request panels consume, in upstream's shapes. T14 builds it from the real
//! reducers (`deriveTimelineEntriesFromVisibleTurnItems` and friends); until
//! then `super::mock_source` builds it from fixtures.

use super::turn_item::{
    CheckpointFileSummary, ProjectedTurnItem, ProviderApprovalOption, ProviderRequestKind,
    ToolActivityIcon, ToolActivitySource, ToolActivitySurface, TurnItem, TurnItemType,
    UserInputAnswerPayload, UserInputQuestion,
};

/// `ChatMessage["role"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

/// `OrchestrationV2Actor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Actor {
    User,
    Agent,
    System,
}

/// `OrchestrationV2CreationSource`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreationSource {
    Web,
    Mobile,
    Mcp,
    Provider,
    Server,
}

/// `OrchestrationV2UserMessageInputIntent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputIntent {
    TurnStart,
    QueuedTurn,
    Steer,
    PromotedQueuedToSteer,
}

/// `ChatAttachment` (image | file | unknown).
#[derive(Clone, Debug, PartialEq)]
pub struct ChatAttachment {
    pub id: String,
    /// `image`, `file`, or a type this build does not know.
    pub kind: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    /// Local file the preview loads from (upstream's `previewUrl`).
    pub preview_path: Option<String>,
}

impl ChatAttachment {
    /// `isImageAttachment`: `image`, or a legacy `file` with an image MIME type.
    pub fn is_image(&self) -> bool {
        self.kind == "image" || (self.kind == "file" && self.mime_type.starts_with("image/"))
    }

    /// `isFileAttachment`.
    pub fn is_file(&self) -> bool {
        self.kind == "file" && !self.is_image()
    }
}

/// `ChatMessage`.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatMessage {
    pub id: String,
    pub role: MessageRole,
    pub text: String,
    pub attachments: Vec<ChatAttachment>,
    pub run_id: Option<String>,
    pub streaming: bool,
    pub created_by: Option<Actor>,
    pub creation_source: Option<CreationSource>,
    pub scheduled_task_id: Option<String>,
    pub sender_thread_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub input_intent: Option<InputIntent>,
}

/// `OrchestrationV2PlanArtifact["status"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanStatus {
    Draft,
    Active,
    Completed,
    Superseded,
}

/// `ProposedPlan`.
#[derive(Clone, Debug, PartialEq)]
pub struct ProposedPlan {
    pub id: String,
    pub run_id: Option<String>,
    pub plan_markdown: String,
    pub status: PlanStatus,
    pub created_at: String,
    pub updated_at: String,
}

/// `ThreadCheckpointSummary["status"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointStatus {
    Ready,
    Missing,
    Error,
    Stale,
}

/// `TurnDiffSummary` (= `ThreadCheckpointSummary`).
#[derive(Clone, Debug, PartialEq)]
pub struct TurnDiffSummary {
    pub checkpoint_id: Option<String>,
    pub scope_id: Option<String>,
    pub run_id: String,
    pub checkpoint_turn_count: u64,
    pub checkpoint_ref: String,
    pub status: CheckpointStatus,
    pub files: Vec<CheckpointFileSummary>,
    pub assistant_message_id: Option<String>,
    pub completed_at: String,
}

/// `WorkLogToolLifecycleStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolLifecycleStatus {
    Idle,
    InProgress,
    Completed,
    Failed,
    Declined,
    Stopped,
}

/// `WorkLogEntry["tone"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorkTone {
    Thinking,
    Tool,
    Info,
    Error,
}

/// `WorkLogEntry`. `structuredPayload` and `projectedItem` keep upstream's
/// split: the payload is the item the entry was derived from, the projected
/// item is the row that carries its visibility/source.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkLogEntry {
    pub question_answer: Option<UserInputAnswerPayload>,
    pub id: String,
    pub created_at: String,
    pub run_id: Option<String>,
    pub label: String,
    pub detail: Option<String>,
    pub command: Option<String>,
    pub raw_command: Option<String>,
    pub changed_files: Option<Vec<String>>,
    pub tone: WorkTone,
    pub tool_title: Option<String>,
    pub tool_call_id: Option<String>,
    pub viewed_image_path: Option<String>,
    pub tool_surface: Option<ToolActivitySurface>,
    pub tool_icon: Option<ToolActivityIcon>,
    pub tool_source: Option<ToolActivitySource>,
    pub source_activity_kind: Option<String>,
    pub task_id: Option<String>,
    pub agent_role: Option<String>,
    pub tool_data: Option<serde_json::Value>,
    pub request_kind: Option<String>,
    pub item_type: Option<TurnItemType>,
    pub tool_lifecycle_status: Option<ToolLifecycleStatus>,
    pub structured_payload: Option<TurnItem>,
    pub source_item_type: Option<TurnItemType>,
    pub projected_item: Option<ProjectedTurnItem>,
}

impl WorkLogEntry {
    /// An entry with only the required fields set; tests and fixtures fill
    /// the rest with struct update syntax.
    pub fn new(
        id: impl Into<String>,
        created_at: impl Into<String>,
        label: impl Into<String>,
        tone: WorkTone,
    ) -> Self {
        Self {
            question_answer: None,
            id: id.into(),
            created_at: created_at.into(),
            run_id: None,
            label: label.into(),
            detail: None,
            command: None,
            raw_command: None,
            changed_files: None,
            tone,
            tool_title: None,
            tool_call_id: None,
            viewed_image_path: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            source_activity_kind: None,
            task_id: None,
            agent_role: None,
            tool_data: None,
            request_kind: None,
            item_type: None,
            tool_lifecycle_status: None,
            structured_payload: None,
            source_item_type: None,
            projected_item: None,
        }
    }
}

/// `TimelineAttempt`: `Pick<OrchestrationV2RunAttempt, …>`.
#[derive(Clone, Debug, PartialEq)]
pub struct TimelineAttempt {
    pub id: String,
    pub run_id: String,
    pub attempt_ordinal: u64,
    pub root_node_id: String,
    /// `pending | running | completed | interrupted | failed | cancelled | superseded`.
    pub status: AttemptStatus,
    pub completed_at: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptStatus {
    Pending,
    Running,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    Superseded,
}

/// The variant half of `TimelineEntry`. HTML render and MCP app entries are
/// cut (PORTING cut list); the session logic drops those items.
#[derive(Clone, Debug, PartialEq)]
pub enum TimelineEntryKind {
    Message {
        message: ChatMessage,
        projected_item: Option<ProjectedTurnItem>,
    },
    ProposedPlan {
        proposed_plan: ProposedPlan,
    },
    Work {
        entry: WorkLogEntry,
    },
    Event {
        projected_item: ProjectedTurnItem,
    },
}

/// `TimelineEntry`.
#[derive(Clone, Debug, PartialEq)]
pub struct TimelineEntry {
    pub id: String,
    pub created_at: String,
    pub kind: TimelineEntryKind,
    /// V2 identity resolved from the item's execution node, when locally available.
    pub attempt: Option<TimelineAttempt>,
}

/// `OrchestrationV2RunStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RunStatus {
    Preparing,
    Queued,
    Starting,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    RolledBack,
}

/// `TimelineLatestRun`: `Pick<ThreadRunSummary, "runId" | "status" | "startedAt" | "completedAt">`.
#[derive(Clone, Debug, PartialEq)]
pub struct TimelineLatestRun {
    pub run_id: String,
    pub status: RunStatus,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// `ThreadPendingApproval["responseCapability"]` / user input's capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseCapability {
    Live,
    NotResumable,
}

/// `ThreadPendingApproval`.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingApproval {
    pub request_id: String,
    pub request_kind: ProviderRequestKind,
    pub created_at: String,
    pub detail: Option<String>,
    pub app_name: Option<String>,
    pub options: Option<Vec<ProviderApprovalOption>>,
    pub response_capability: ResponseCapability,
}

/// `ThreadPendingUserInput`.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingUserInput {
    pub request_id: String,
    pub created_at: String,
    pub questions: Vec<UserInputQuestion>,
    pub response_capability: ResponseCapability,
    pub dismissible: bool,
}

/// Everything the thread view renders for one thread, in upstream's shapes.
///
/// Field names follow the `deriveMessagesTimelineRows` input and
/// `MessagesTimelineProps` in upstream/apps/web/src/components/chat.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThreadTimelineSnapshot {
    pub thread_id: String,
    pub timeline_entries: Vec<TimelineEntry>,
    pub latest_run: Option<TimelineLatestRun>,
    pub running_run_id: Option<String>,
    pub is_working: bool,
    pub runless_work_active: bool,
    pub active_turn_started_at: Option<String>,
    pub turn_diff_summaries: Vec<TurnDiffSummary>,
    pub supports_conversation_rollback: bool,
    pub live_agent_task_ids: Vec<String>,
    pub is_compacting: bool,
    pub is_reverting_checkpoint: bool,
    pub pending_approvals: Vec<PendingApproval>,
    pub pending_user_inputs: Vec<PendingUserInput>,
    /// The project's workspace root, for workspace-relative labels.
    pub workspace_root: Option<String>,
}
