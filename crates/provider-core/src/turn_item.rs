//! Common-field access for `OrchestrationV2TurnItem`.
//!
//! Upstream reads `item.id`, `item.status`, … on the union directly; every
//! member shares one base struct. The Rust contracts model the members as
//! struct variants, so this module gives provider-core (and its callers) the
//! same uniform view.

use turin_contracts::{
    NodeId, ProviderThreadId, ProviderTurnId, RunId, ThreadId, TurnItemId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2ProviderRef, OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
    },
};

/// Borrowed base fields shared by every turn item member.
#[derive(Debug, Clone, Copy)]
pub struct TurnItemCommon<'a> {
    pub id: &'a TurnItemId,
    pub thread_id: &'a ThreadId,
    pub run_id: Option<&'a RunId>,
    pub node_id: Option<&'a NodeId>,
    pub provider_thread_id: Option<&'a ProviderThreadId>,
    pub provider_turn_id: Option<&'a ProviderTurnId>,
    pub native_item_ref: Option<&'a OrchestrationV2ProviderRef>,
    pub parent_item_id: Option<&'a TurnItemId>,
    pub ordinal: u64,
    pub status: OrchestrationV2TurnItemStatus,
    pub title: Option<&'a str>,
    pub started_at: Option<UtcDateTime>,
    pub completed_at: Option<UtcDateTime>,
    pub updated_at: UtcDateTime,
}

macro_rules! common_of {
    ($item:expr; $($variant:ident),+ $(,)?) => {
        match $item {
            $(OrchestrationV2TurnItem::$variant {
                id, thread_id, run_id, node_id, provider_thread_id, provider_turn_id,
                native_item_ref, parent_item_id, ordinal, status, title, started_at,
                completed_at, updated_at, ..
            } => TurnItemCommon {
                id,
                thread_id,
                run_id: run_id.as_ref(),
                node_id: node_id.as_ref(),
                provider_thread_id: provider_thread_id.as_ref(),
                provider_turn_id: provider_turn_id.as_ref(),
                native_item_ref: native_item_ref.as_ref(),
                parent_item_id: parent_item_id.as_ref(),
                ordinal: *ordinal,
                status: *status,
                title: title.as_deref(),
                started_at: *started_at,
                completed_at: *completed_at,
                updated_at: *updated_at,
            },)+
        }
    };
}

/// The base fields of any turn item.
pub fn turn_item_common(item: &OrchestrationV2TurnItem) -> TurnItemCommon<'_> {
    common_of!(
        item;
        Notification, UserMessage, AssistantMessage, Reasoning, ProposedPlan, TodoList,
        UserInputRequest, FileChange, CommandExecution, FileSearch, WebSearch,
        ApprovalRequest, Checkpoint, RunInterruptRequest, RunInterruptResult, SystemNotice,
        Error, Compaction, Handoff, Fork, ThreadCreated, SecretRequest, Subagent, DynamicTool,
    )
}
