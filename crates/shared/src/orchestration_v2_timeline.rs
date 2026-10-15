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

    fn item(item_type: TimelineItemType) -> TimelineTurnItem {
        TimelineTurnItem {
            item_type,
            run_id: Some(RunId::new("run:timeline-visibility")),
            node_id: Some(NodeId::new("node:timeline-visibility")),
            input_intent: None,
        }
    }

    #[test]
    fn timeline_visibility_matches_both_paths() {
        let run = TimelineRun {
            id: RunId::new("run:timeline-visibility"),
            status: OrchestrationV2RunStatus::Running,
        };
        let superseded = TimelineRunAttempt {
            run_id: run.id.clone(),
            root_node_id: NodeId::new("node:timeline-visibility"),
            status: OrchestrationV2RunAttemptStatus::Superseded,
        };
        let result = item(TimelineItemType::RunInterruptResult);
        let request = item(TimelineItemType::RunInterruptRequest);
        let cases = [
            (
                vec![run.clone()],
                vec![superseded.clone()],
                vec![result.clone()],
                false,
            ),
            (
                vec![run.clone()],
                vec![superseded.clone()],
                vec![request, result.clone()],
                true,
            ),
            (vec![run.clone()], vec![], vec![result.clone()], true),
            (
                vec![TimelineRun {
                    status: OrchestrationV2RunStatus::RolledBack,
                    ..run.clone()
                }],
                vec![],
                vec![result.clone()],
                false,
            ),
        ];
        for (runs, attempts, items, expected) in cases {
            assert_eq!(
                is_orchestration_v2_turn_item_visible(&result, &runs, &attempts, &items),
                expected
            );
            assert_eq!(
                create_orchestration_v2_turn_item_visibility(&runs, &attempts, &items)
                    .is_visible(&result),
                expected
            );
        }
        let mut queued = item(TimelineItemType::UserMessage);
        queued.input_intent = Some(OrchestrationV2UserMessageInputIntent::QueuedTurn);
        let runs = [TimelineRun {
            status: OrchestrationV2RunStatus::Cancelled,
            ..run
        }];
        assert!(!is_orchestration_v2_turn_item_visible(
            &queued,
            &runs,
            &[],
            &[]
        ));
        assert!(!create_orchestration_v2_turn_item_visibility(&runs, &[], &[]).is_visible(&queued));
    }
}
