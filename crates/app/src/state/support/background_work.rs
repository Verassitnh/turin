//! Port of upstream/packages/shared/src/orchestrationV2PendingBackgroundWork.ts
//! (the client-side parts: the completion gate and the turn-item helpers; the
//! roster derivation belongs to the orchestrator).

use crate::state::contracts::OrchestrationV2PendingBackgroundTask;

use super::turn_item::{TurnItemType, is_turn_item_status_active};
use crate::state::contracts::OrchestrationV2TurnItemStatus;

/// Whether a turn-item update can end background work that a settled run is
/// still waiting on: an item of a background type that is no longer active.
/// Streaming output on a running item, and every other item type, cannot.
pub fn turn_item_update_can_end_background_work(
    item_type: TurnItemType,
    status: OrchestrationV2TurnItemStatus,
) -> bool {
    matches!(
        item_type,
        TurnItemType::CommandExecution | TurnItemType::DynamicTool | TurnItemType::Subagent
    ) && !is_turn_item_status_active(status)
}

/// Whether background work left behind by a completed root run holds back its
/// completion alert (desktop/web notification and the mobile push). Commands,
/// such as dev servers and other long-lived shells, do not: the agent is done
/// and may leave them running for hours. Subagents and monitors do, because
/// they wake the agent and it continues (#13625). Work the adapter cannot name,
/// including kinds this build does not know, holds as the conservative choice.
pub fn background_work_holds_completion(tasks: &[OrchestrationV2PendingBackgroundTask]) -> bool {
    tasks.iter().any(background_work_kind_holds_completion)
}

fn background_work_kind_holds_completion(task: &OrchestrationV2PendingBackgroundTask) -> bool {
    match task {
        OrchestrationV2PendingBackgroundTask::Command { .. } => false,
        OrchestrationV2PendingBackgroundTask::Subagent { .. }
        | OrchestrationV2PendingBackgroundTask::Monitor { .. }
        | OrchestrationV2PendingBackgroundTask::BackgroundTask { .. } => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::TrimmedNonEmptyString;

    fn id(value: &str) -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(value).unwrap()
    }

    #[test]
    fn commands_do_not_hold_completion_but_the_rest_do() {
        let command = OrchestrationV2PendingBackgroundTask::Command {
            task_id: id("a"),
            description: None,
        };
        let monitor = OrchestrationV2PendingBackgroundTask::Monitor {
            task_id: id("b"),
            description: None,
        };
        assert!(!background_work_holds_completion(&[]));
        assert!(!background_work_holds_completion(std::slice::from_ref(
            &command
        )));
        assert!(background_work_holds_completion(&[command, monitor]));
    }

    #[test]
    fn only_settled_background_items_end_background_work() {
        assert!(turn_item_update_can_end_background_work(
            TurnItemType::Subagent,
            OrchestrationV2TurnItemStatus::Completed
        ));
        assert!(!turn_item_update_can_end_background_work(
            TurnItemType::CommandExecution,
            OrchestrationV2TurnItemStatus::Running
        ));
        assert!(!turn_item_update_can_end_background_work(
            TurnItemType::AssistantMessage,
            OrchestrationV2TurnItemStatus::Completed
        ));
    }
}
