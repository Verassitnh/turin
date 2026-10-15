//! Field access shared by every `OrchestrationV2TurnItem` variant.
//!
//! Upstream reads `item.runId`, `item.ordinal`, `item.type`, … off the TS
//! union directly. The Rust contracts model the union as an enum whose 24
//! variants repeat the base fields, so this module names them once.

use turin_contracts::orchestration_v2::{
    OrchestrationV2Actor, OrchestrationV2ProviderRef, OrchestrationV2TurnItem,
    OrchestrationV2TurnItemStatus, OrchestrationV2UserMessageInputIntent,
};
use turin_contracts::{
    MessageId, NodeId, ProviderThreadId, ProviderTurnId, RunId, ThreadId, TurnItemId, UtcDateTime,
};

/// The fields every turn item carries.
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
    pub started_at: Option<&'a UtcDateTime>,
    pub completed_at: Option<&'a UtcDateTime>,
    pub updated_at: &'a UtcDateTime,
}

macro_rules! variants {
    ($($variant:ident => $tag:literal),+ $(,)?) => {
        /// The item's `type` literal.
        pub fn type_tag(item: &OrchestrationV2TurnItem) -> &'static str {
            match item {
                $(OrchestrationV2TurnItem::$variant { .. } => $tag,)+
            }
        }

        /// The base fields of any variant.
        pub fn common(item: &OrchestrationV2TurnItem) -> TurnItemCommon<'_> {
            match item {
                $(
                    OrchestrationV2TurnItem::$variant {
                        id,
                        thread_id,
                        run_id,
                        node_id,
                        provider_thread_id,
                        provider_turn_id,
                        native_item_ref,
                        parent_item_id,
                        ordinal,
                        status,
                        title,
                        started_at,
                        completed_at,
                        updated_at,
                        ..
                    }
                )|+ => TurnItemCommon {
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
                    started_at: started_at.as_ref(),
                    completed_at: completed_at.as_ref(),
                    updated_at,
                },
            }
        }

        /// Replace the ordinal in place (`{ ...item, ordinal }`).
        pub fn set_ordinal(item: &mut OrchestrationV2TurnItem, next: u64) {
            match item {
                $(OrchestrationV2TurnItem::$variant { ordinal, .. })|+ => *ordinal = next,
            }
        }

        /// Replace the status in place.
        pub fn set_status(item: &mut OrchestrationV2TurnItem, next: OrchestrationV2TurnItemStatus) {
            match item {
                $(OrchestrationV2TurnItem::$variant { status, .. })|+ => *status = next,
            }
        }
    };
}

variants! {
    Notification => "notification",
    UserMessage => "user_message",
    AssistantMessage => "assistant_message",
    Reasoning => "reasoning",
    ProposedPlan => "proposed_plan",
    TodoList => "todo_list",
    UserInputRequest => "user_input_request",
    FileChange => "file_change",
    CommandExecution => "command_execution",
    FileSearch => "file_search",
    WebSearch => "web_search",
    ApprovalRequest => "approval_request",
    Checkpoint => "checkpoint",
    RunInterruptRequest => "run_interrupt_request",
    RunInterruptResult => "run_interrupt_result",
    SystemNotice => "system_notice",
    Error => "error",
    Compaction => "compaction",
    Handoff => "handoff",
    Fork => "fork",
    ThreadCreated => "thread_created",
    SecretRequest => "secret_request",
    Subagent => "subagent",
    DynamicTool => "dynamic_tool",
}

/// `messageIdForTurnItem`: the conversation message a user/assistant item mirrors.
pub fn message_id(item: &OrchestrationV2TurnItem) -> Option<&MessageId> {
    match item {
        OrchestrationV2TurnItem::UserMessage { message_id, .. }
        | OrchestrationV2TurnItem::AssistantMessage { message_id, .. } => Some(message_id),
        _ => None,
    }
}

/// `inputIntent` of a user message.
pub fn input_intent(
    item: &OrchestrationV2TurnItem,
) -> Option<OrchestrationV2UserMessageInputIntent> {
    match item {
        OrchestrationV2TurnItem::UserMessage { input_intent, .. } => Some(*input_intent),
        _ => None,
    }
}

/// `createdBy` of a user message.
pub fn created_by(item: &OrchestrationV2TurnItem) -> Option<OrchestrationV2Actor> {
    match item {
        OrchestrationV2TurnItem::UserMessage { created_by, .. } => Some(*created_by),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn item(value: serde_json::Value) -> OrchestrationV2TurnItem {
        serde_json::from_value(value).expect("turn item fixture")
    }

    #[test]
    fn exposes_base_fields_and_tags_for_every_variant_shape() {
        let command = item(json!({
            "id": "item-1", "threadId": "thread-1", "runId": "run-1", "nodeId": null,
            "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
            "parentItemId": null, "ordinal": 7, "status": "running", "title": "ls",
            "startedAt": null, "completedAt": null, "updatedAt": "2026-01-01T00:00:00.000Z",
            "type": "command_execution", "input": "ls"
        }));
        let view = common(&command);
        assert_eq!(view.id.as_str(), "item-1");
        assert_eq!(view.run_id.map(RunId::as_str), Some("run-1"));
        assert_eq!(view.ordinal, 7);
        assert_eq!(view.title, Some("ls"));
        assert_eq!(type_tag(&command), "command_execution");
    }

    #[test]
    fn setters_rewrite_in_place() {
        let mut fork = item(json!({
            "id": "item-2", "threadId": "thread-1", "runId": null, "nodeId": null,
            "providerTurnId": null, "nativeItemRef": null, "parentItemId": null,
            "ordinal": 0, "status": "completed", "title": null, "startedAt": null,
            "completedAt": null, "updatedAt": "2026-01-01T00:00:00.000Z", "type": "fork",
            "source": {"type": "run", "threadId": "thread-0", "runId": "run-0"},
            "targetThreadId": "thread-1"
        }));
        set_ordinal(&mut fork, 9);
        set_status(&mut fork, OrchestrationV2TurnItemStatus::Failed);
        let view = common(&fork);
        assert_eq!(view.ordinal, 9);
        assert_eq!(view.status, OrchestrationV2TurnItemStatus::Failed);
    }
}
