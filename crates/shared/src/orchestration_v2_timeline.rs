//! Port of upstream/packages/shared/src/orchestrationV2Timeline.ts

use std::collections::{HashMap, HashSet};
use turin_contracts::base_schemas::{NodeId, RunId};
use turin_contracts::orchestration_v2::{
    OrchestrationV2RunAttemptStatus, OrchestrationV2RunStatus,
    OrchestrationV2UserMessageInputIntent,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineItemType {
    UserMessage,
    RunInterruptRequest,
    RunInterruptResult,
    Other,
}

#[derive(Debug, Clone)]
pub struct TimelineTurnItem {
    pub item_type: TimelineItemType,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub input_intent: Option<OrchestrationV2UserMessageInputIntent>,
}

#[derive(Debug, Clone)]
pub struct TimelineRun {
    pub id: RunId,
    pub status: OrchestrationV2RunStatus,
}

#[derive(Debug, Clone)]
pub struct TimelineRunAttempt {
    pub run_id: RunId,
    pub root_node_id: NodeId,
    pub status: OrchestrationV2RunAttemptStatus,
}

pub fn is_orchestration_v2_superseded_interrupt(
    item: &TimelineTurnItem,
    attempts: &[TimelineRunAttempt],
    items: &[TimelineTurnItem],
) -> bool {
    if item.item_type != TimelineItemType::RunInterruptResult {
        return false;
    }
    let (Some(run_id), Some(node_id)) = (&item.run_id, &item.node_id) else {
        return false;
    };
    attempts.iter().any(|attempt| {
        attempt.run_id == *run_id
            && attempt.root_node_id == *node_id
            && attempt.status == OrchestrationV2RunAttemptStatus::Superseded
    }) && !items.iter().any(|candidate| {
        candidate.item_type == TimelineItemType::RunInterruptRequest
            && candidate.run_id.as_ref() == Some(run_id)
    })
}

pub fn is_orchestration_v2_turn_item_visible(
    item: &TimelineTurnItem,
    runs: &[TimelineRun],
    attempts: &[TimelineRunAttempt],
    items: &[TimelineTurnItem],
) -> bool {
    let status = item.run_id.as_ref().and_then(|run_id| {
        runs.iter()
            .find(|run| &run.id == run_id)
            .map(|run| run.status)
    });
    if status == Some(OrchestrationV2RunStatus::RolledBack) {
        return false;
    }
    if status == Some(OrchestrationV2RunStatus::Cancelled)
        && item.item_type == TimelineItemType::UserMessage
        && item.input_intent == Some(OrchestrationV2UserMessageInputIntent::QueuedTurn)
    {
        return false;
    }
    !is_orchestration_v2_superseded_interrupt(item, attempts, items)
}

pub struct TurnItemVisibility {
    statuses: HashMap<RunId, OrchestrationV2RunStatus>,
    superseded_roots: HashMap<RunId, HashSet<NodeId>>,
    interrupt_runs: HashSet<Option<RunId>>,
}

pub fn create_orchestration_v2_turn_item_visibility(
    runs: &[TimelineRun],
    attempts: &[TimelineRunAttempt],
    items: &[TimelineTurnItem],
) -> TurnItemVisibility {
    let statuses = runs
        .iter()
        .map(|run| (run.id.clone(), run.status))
        .collect();
    let mut superseded_roots: HashMap<RunId, HashSet<NodeId>> = HashMap::new();
    for attempt in attempts {
        if attempt.status == OrchestrationV2RunAttemptStatus::Superseded {
            superseded_roots
                .entry(attempt.run_id.clone())
                .or_default()
                .insert(attempt.root_node_id.clone());
        }
    }
    let interrupt_runs = items
        .iter()
        .filter(|item| item.item_type == TimelineItemType::RunInterruptRequest)
        .map(|item| item.run_id.clone())
        .collect();
    TurnItemVisibility {
        statuses,
        superseded_roots,
        interrupt_runs,
    }
}

impl TurnItemVisibility {
    pub fn is_visible(&self, item: &TimelineTurnItem) -> bool {
        let status = item
            .run_id
            .as_ref()
            .and_then(|run_id| self.statuses.get(run_id));
        if status == Some(&OrchestrationV2RunStatus::RolledBack) {
            return false;
        }
        if status == Some(&OrchestrationV2RunStatus::Cancelled)
            && item.item_type == TimelineItemType::UserMessage
            && item.input_intent == Some(OrchestrationV2UserMessageInputIntent::QueuedTurn)
        {
            return false;
        }
        !(item.item_type == TimelineItemType::RunInterruptResult
            && item.run_id.as_ref().is_some_and(|run_id| {
                item.node_id.as_ref().is_some_and(|node_id| {
                    self.superseded_roots
                        .get(run_id)
                        .is_some_and(|roots| roots.contains(node_id))
                })
            })
            && !self.interrupt_runs.contains(&item.run_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use OrchestrationV2RunAttemptStatus as Attempt;
    use OrchestrationV2RunStatus as Run;
    use TimelineItemType::*;

    fn run_id() -> RunId {
        RunId::new("run:timeline-visibility").unwrap()
    }

    fn node_id() -> NodeId {
        NodeId::new("node:timeline-visibility").unwrap()
    }

    fn item(item_type: TimelineItemType) -> TimelineTurnItem {
        TimelineTurnItem {
            item_type,
            run_id: Some(run_id()),
            node_id: Some(node_id()),
            input_intent: None,
        }
    }

    fn user(intent: OrchestrationV2UserMessageInputIntent) -> TimelineTurnItem {
        TimelineTurnItem {
            input_intent: Some(intent),
            ..item(UserMessage)
        }
    }

    fn runs(status: Run) -> Vec<TimelineRun> {
        vec![TimelineRun {
            id: run_id(),
            status,
        }]
    }

    fn attempt(root: NodeId, status: Attempt) -> TimelineRunAttempt {
        TimelineRunAttempt {
            run_id: run_id(),
            root_node_id: root,
            status,
        }
    }

    /// Mirrors upstream's `describe.each` over the point and indexed checks.
    fn is_visible(
        item: &TimelineTurnItem,
        runs: &[TimelineRun],
        attempts: &[TimelineRunAttempt],
        items: &[TimelineTurnItem],
    ) -> bool {
        let point = is_orchestration_v2_turn_item_visible(item, runs, attempts, items);
        let indexed =
            create_orchestration_v2_turn_item_visibility(runs, attempts, items).is_visible(item);
        assert_eq!(point, indexed, "point and indexed visibility disagree");
        point
    }

    #[test]
    fn hides_unpaired_interruption_results_from_superseded_attempts() {
        assert!(!is_visible(
            &item(RunInterruptResult),
            &runs(Run::Running),
            &[attempt(node_id(), Attempt::Superseded)],
            &[item(RunInterruptResult)],
        ));
    }

    #[test]
    fn keeps_paired_interruption_results_from_superseded_attempts() {
        assert!(is_visible(
            &item(RunInterruptResult),
            &runs(Run::Running),
            &[attempt(node_id(), Attempt::Superseded)],
            &[item(RunInterruptRequest), item(RunInterruptResult)],
        ));
    }

    #[test]
    fn keeps_interruption_results_from_terminal_attempts_without_a_request() {
        assert!(is_visible(
            &item(RunInterruptResult),
            &runs(Run::Interrupted),
            &[attempt(node_id(), Attempt::Interrupted)],
            &[item(RunInterruptResult)],
        ));
    }

    #[test]
    fn keeps_interruption_results_from_terminal_attempts_with_a_request() {
        assert!(is_visible(
            &item(RunInterruptResult),
            &runs(Run::Interrupted),
            &[attempt(node_id(), Attempt::Interrupted)],
            &[item(RunInterruptRequest), item(RunInterruptResult)],
        ));
    }

    #[test]
    fn hides_queued_user_messages_once_their_run_is_cancelled() {
        let queued = user(OrchestrationV2UserMessageInputIntent::QueuedTurn);
        assert!(!is_visible(
            &queued,
            &runs(Run::Cancelled),
            &[],
            std::slice::from_ref(&queued),
        ));
    }

    #[test]
    fn keeps_queued_user_messages_while_their_run_is_queued() {
        let queued = user(OrchestrationV2UserMessageInputIntent::QueuedTurn);
        assert!(is_visible(
            &queued,
            &runs(Run::Queued),
            &[],
            std::slice::from_ref(&queued),
        ));
    }

    #[test]
    fn keeps_non_queued_user_messages_on_cancelled_runs() {
        let start = user(OrchestrationV2UserMessageInputIntent::TurnStart);
        assert!(is_visible(
            &start,
            &runs(Run::Cancelled),
            &[],
            std::slice::from_ref(&start),
        ));
    }

    #[test]
    fn does_not_hide_an_interruption_because_another_attempt_was_superseded() {
        assert!(is_visible(
            &item(RunInterruptResult),
            &runs(Run::Interrupted),
            &[
                attempt(
                    NodeId::new("node:timeline-visibility:older").unwrap(),
                    Attempt::Superseded,
                ),
                attempt(node_id(), Attempt::Interrupted),
            ],
            &[item(RunInterruptResult)],
        ));
    }

    #[test]
    fn rolled_back_runs_hide_their_items() {
        assert!(!is_visible(
            &item(RunInterruptResult),
            &runs(Run::RolledBack),
            &[],
            &[item(RunInterruptResult)],
        ));
    }
}
