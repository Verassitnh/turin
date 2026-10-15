//! Accessors over `OrchestrationV2TurnItem`.
//!
//! Upstream reads the shared base fields (`item.id`, `item.runId`, …) straight
//! off the union. The Rust contract is one struct-like variant per item type,
//! so the base fields are reached through this trait.

use crate::state::contracts::{
    NodeId, OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus, RunId, ThreadId, TurnItemId,
    UtcDateTime,
};

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

macro_rules! turn_item_dispatch {
    ($self:expr, $pattern:ident => $body:expr) => {
        match $self {
            OrchestrationV2TurnItem::Notification { $pattern, .. }
            | OrchestrationV2TurnItem::UserMessage { $pattern, .. }
            | OrchestrationV2TurnItem::AssistantMessage { $pattern, .. }
            | OrchestrationV2TurnItem::Reasoning { $pattern, .. }
            | OrchestrationV2TurnItem::ProposedPlan { $pattern, .. }
            | OrchestrationV2TurnItem::TodoList { $pattern, .. }
            | OrchestrationV2TurnItem::UserInputRequest { $pattern, .. }
            | OrchestrationV2TurnItem::FileChange { $pattern, .. }
            | OrchestrationV2TurnItem::CommandExecution { $pattern, .. }
            | OrchestrationV2TurnItem::FileSearch { $pattern, .. }
            | OrchestrationV2TurnItem::WebSearch { $pattern, .. }
            | OrchestrationV2TurnItem::ApprovalRequest { $pattern, .. }
            | OrchestrationV2TurnItem::Checkpoint { $pattern, .. }
            | OrchestrationV2TurnItem::RunInterruptRequest { $pattern, .. }
            | OrchestrationV2TurnItem::RunInterruptResult { $pattern, .. }
            | OrchestrationV2TurnItem::SystemNotice { $pattern, .. }
            | OrchestrationV2TurnItem::Error { $pattern, .. }
            | OrchestrationV2TurnItem::Compaction { $pattern, .. }
            | OrchestrationV2TurnItem::Handoff { $pattern, .. }
            | OrchestrationV2TurnItem::Fork { $pattern, .. }
            | OrchestrationV2TurnItem::ThreadCreated { $pattern, .. }
            | OrchestrationV2TurnItem::SecretRequest { $pattern, .. }
            | OrchestrationV2TurnItem::Subagent { $pattern, .. }
            | OrchestrationV2TurnItem::DynamicTool { $pattern, .. } => $body,
        }
    };
}

/// The base fields every turn item carries.
pub trait TurnItemExt {
    fn id(&self) -> &TurnItemId;
    fn thread_id(&self) -> &ThreadId;
    fn run_id(&self) -> Option<&RunId>;
    fn node_id(&self) -> Option<&NodeId>;
    fn ordinal(&self) -> u64;
    fn status(&self) -> OrchestrationV2TurnItemStatus;
    fn updated_at(&self) -> &UtcDateTime;
    fn item_type(&self) -> TurnItemType;
}

impl TurnItemExt for OrchestrationV2TurnItem {
    fn id(&self) -> &TurnItemId {
        turn_item_dispatch!(self, id => id)
    }

    fn thread_id(&self) -> &ThreadId {
        turn_item_dispatch!(self, thread_id => thread_id)
    }

    fn run_id(&self) -> Option<&RunId> {
        turn_item_dispatch!(self, run_id => run_id.as_ref())
    }

    fn node_id(&self) -> Option<&NodeId> {
        turn_item_dispatch!(self, node_id => node_id.as_ref())
    }

    fn ordinal(&self) -> u64 {
        turn_item_dispatch!(self, ordinal => *ordinal)
    }

    fn status(&self) -> OrchestrationV2TurnItemStatus {
        turn_item_dispatch!(self, status => *status)
    }

    fn updated_at(&self) -> &UtcDateTime {
        turn_item_dispatch!(self, updated_at => updated_at)
    }

    fn item_type(&self) -> TurnItemType {
        match self {
            OrchestrationV2TurnItem::Notification { .. } => TurnItemType::Notification,
            OrchestrationV2TurnItem::UserMessage { .. } => TurnItemType::UserMessage,
            OrchestrationV2TurnItem::AssistantMessage { .. } => TurnItemType::AssistantMessage,
            OrchestrationV2TurnItem::Reasoning { .. } => TurnItemType::Reasoning,
            OrchestrationV2TurnItem::ProposedPlan { .. } => TurnItemType::ProposedPlan,
            OrchestrationV2TurnItem::TodoList { .. } => TurnItemType::TodoList,
            OrchestrationV2TurnItem::UserInputRequest { .. } => TurnItemType::UserInputRequest,
            OrchestrationV2TurnItem::FileChange { .. } => TurnItemType::FileChange,
            OrchestrationV2TurnItem::CommandExecution { .. } => TurnItemType::CommandExecution,
            OrchestrationV2TurnItem::FileSearch { .. } => TurnItemType::FileSearch,
            OrchestrationV2TurnItem::WebSearch { .. } => TurnItemType::WebSearch,
            OrchestrationV2TurnItem::ApprovalRequest { .. } => TurnItemType::ApprovalRequest,
            OrchestrationV2TurnItem::Checkpoint { .. } => TurnItemType::Checkpoint,
            OrchestrationV2TurnItem::RunInterruptRequest { .. } => {
                TurnItemType::RunInterruptRequest
            }
            OrchestrationV2TurnItem::RunInterruptResult { .. } => TurnItemType::RunInterruptResult,
            OrchestrationV2TurnItem::SystemNotice { .. } => TurnItemType::SystemNotice,
            OrchestrationV2TurnItem::Error { .. } => TurnItemType::Error,
            OrchestrationV2TurnItem::Compaction { .. } => TurnItemType::Compaction,
            OrchestrationV2TurnItem::Handoff { .. } => TurnItemType::Handoff,
            OrchestrationV2TurnItem::Fork { .. } => TurnItemType::Fork,
            OrchestrationV2TurnItem::ThreadCreated { .. } => TurnItemType::ThreadCreated,
            OrchestrationV2TurnItem::SecretRequest { .. } => TurnItemType::SecretRequest,
            OrchestrationV2TurnItem::Subagent { .. } => TurnItemType::Subagent,
            OrchestrationV2TurnItem::DynamicTool { .. } => TurnItemType::DynamicTool,
        }
    }
}

/// `isOrchestrationV2WorkActive` for a turn item status: idle work is
/// resumable but does not keep a turn or its subscription alive.
pub fn is_turn_item_status_active(status: OrchestrationV2TurnItemStatus) -> bool {
    matches!(
        status,
        OrchestrationV2TurnItemStatus::Pending
            | OrchestrationV2TurnItemStatus::Running
            | OrchestrationV2TurnItemStatus::Waiting
    )
}

/// `isOrchestrationV2WorkActive` for a subagent status.
pub fn is_subagent_status_active(
    status: crate::state::contracts::OrchestrationV2SubagentStatus,
) -> bool {
    use crate::state::contracts::OrchestrationV2SubagentStatus as S;
    matches!(status, S::Pending | S::Running | S::Waiting)
}
