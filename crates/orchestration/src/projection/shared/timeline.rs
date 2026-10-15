//! Port of upstream/packages/shared/src/orchestrationV2Timeline.ts
//!
//! Which turn items a timeline shows. Lives here until `turin-shared` (T05b)
//! carries its contract-dependent files; the logic is identical.

use std::collections::{HashMap, HashSet};

use turin_contracts::orchestration_v2::{
    OrchestrationV2Run, OrchestrationV2RunAttempt, OrchestrationV2RunAttemptStatus,
    OrchestrationV2RunStatus, OrchestrationV2TurnItem, OrchestrationV2UserMessageInputIntent,
};
use turin_contracts::{NodeId, RunId};

use crate::projection::turn_item;

/// `TimelineTurnItem`: the slice of a turn item visibility reads. The
/// timeline index carries only these fields, never the item body.
pub trait TimelineTurnItem {
    fn type_tag(&self) -> &str;
    fn run_id(&self) -> Option<&RunId>;
    fn node_id(&self) -> Option<&NodeId>;
    fn input_intent(&self) -> Option<OrchestrationV2UserMessageInputIntent>;
}

impl TimelineTurnItem for OrchestrationV2TurnItem {
    fn type_tag(&self) -> &str {
        turn_item::type_tag(self)
    }
    fn run_id(&self) -> Option<&RunId> {
        turn_item::common(self).run_id
    }
    fn node_id(&self) -> Option<&NodeId> {
        turn_item::common(self).node_id
    }
    fn input_intent(&self) -> Option<OrchestrationV2UserMessageInputIntent> {
        turn_item::input_intent(self)
    }
}

/// `Pick<OrchestrationV2Run, "id" | "status">`.
#[derive(Debug, Clone, Copy)]
pub struct TimelineRun<'a> {
    pub id: &'a RunId,
    pub status: OrchestrationV2RunStatus,
}

impl<'a> From<&'a OrchestrationV2Run> for TimelineRun<'a> {
    fn from(run: &'a OrchestrationV2Run) -> Self {
        Self {
            id: &run.id,
            status: run.status,
        }
    }
}

/// `Pick<OrchestrationV2RunAttempt, "runId" | "rootNodeId" | "status">`.
#[derive(Debug, Clone, Copy)]
pub struct TimelineRunAttempt<'a> {
    pub run_id: &'a RunId,
    pub root_node_id: &'a NodeId,
    pub status: OrchestrationV2RunAttemptStatus,
}

impl<'a> From<&'a OrchestrationV2RunAttempt> for TimelineRunAttempt<'a> {
    fn from(attempt: &'a OrchestrationV2RunAttempt) -> Self {
        Self {
            run_id: &attempt.run_id,
            root_node_id: &attempt.root_node_id,
            status: attempt.status,
        }
    }
}

pub fn timeline_runs(runs: &[OrchestrationV2Run]) -> Vec<TimelineRun<'_>> {
    runs.iter().map(TimelineRun::from).collect()
}

pub fn timeline_attempts(attempts: &[OrchestrationV2RunAttempt]) -> Vec<TimelineRunAttempt<'_>> {
    attempts.iter().map(TimelineRunAttempt::from).collect()
}

pub fn is_orchestration_v2_superseded_interrupt<'a, I: TimelineTurnItem + 'a>(
    item: &I,
    attempts: &[TimelineRunAttempt<'_>],
    items: impl IntoIterator<Item = &'a I>,
) -> bool {
    let (Some(run_id), Some(node_id)) = (item.run_id(), item.node_id()) else {
        return false;
    };
    if item.type_tag() != "run_interrupt_result" {
        return false;
    }

    let is_superseded = attempts.iter().any(|attempt| {
        attempt.run_id == run_id
            && attempt.root_node_id == node_id
            && attempt.status == OrchestrationV2RunAttemptStatus::Superseded
    });
    if !is_superseded {
        return false;
    }

    // Paired stop-then-steer results have a matching request on the same run and
    // must stay visible. Legacy plain-steer results have no request and stay hidden.
    let has_matching_request = items.into_iter().any(|candidate| {
        candidate.type_tag() == "run_interrupt_request" && candidate.run_id() == Some(run_id)
    });
    !has_matching_request
}

pub fn is_orchestration_v2_turn_item_visible<'a, I: TimelineTurnItem + 'a>(
    item: &I,
    runs: &[TimelineRun<'_>],
    attempts: &[TimelineRunAttempt<'_>],
    items: impl IntoIterator<Item = &'a I>,
) -> bool {
    if let Some(run_id) = item.run_id()
        && runs
            .iter()
            .any(|run| run.id == run_id && run.status == OrchestrationV2RunStatus::RolledBack)
    {
        return false;
    }

    // A queued message is composer state, not conversation history. When its run
    // is cancelled the input never reached the provider, so it must not surface
    // as a transcript row.
    if item.type_tag() == "user_message"
        && item.input_intent() == Some(OrchestrationV2UserMessageInputIntent::QueuedTurn)
        && let Some(run_id) = item.run_id()
        && runs
            .iter()
            .any(|run| run.id == run_id && run.status == OrchestrationV2RunStatus::Cancelled)
    {
        return false;
    }

    !is_orchestration_v2_superseded_interrupt(item, attempts, items)
}

/// `createOrchestrationV2TurnItemVisibility` result: indexed once when
/// reconciling a whole timeline after run/attempt state changes.
pub struct TurnItemVisibility {
    statuses: HashMap<RunId, OrchestrationV2RunStatus>,
    superseded_roots: HashMap<RunId, HashSet<NodeId>>,
    interrupt_runs: HashSet<Option<RunId>>,
}

impl TurnItemVisibility {
    pub fn is_visible<I: TimelineTurnItem>(&self, item: &I) -> bool {
        let status = item.run_id().and_then(|run_id| self.statuses.get(run_id));
        if status == Some(&OrchestrationV2RunStatus::RolledBack) {
            return false;
        }
        if status == Some(&OrchestrationV2RunStatus::Cancelled)
            && item.type_tag() == "user_message"
            && item.input_intent() == Some(OrchestrationV2UserMessageInputIntent::QueuedTurn)
        {
            return false;
        }
        let superseded_result = item.type_tag() == "run_interrupt_result"
            && match (item.run_id(), item.node_id()) {
                (Some(run_id), Some(node_id)) => {
                    self.superseded_roots
                        .get(run_id)
                        .is_some_and(|roots| roots.contains(node_id))
                        && !self.interrupt_runs.contains(&Some(run_id.clone()))
                }
                _ => false,
            };
        !superseded_result
    }
}

pub fn create_orchestration_v2_turn_item_visibility<'a, I: TimelineTurnItem + 'a>(
    runs: &[TimelineRun<'_>],
    attempts: &[TimelineRunAttempt<'_>],
    items: impl IntoIterator<Item = &'a I>,
) -> TurnItemVisibility {
    let statuses = runs
        .iter()
        .map(|run| (run.id.clone(), run.status))
        .collect::<HashMap<_, _>>();
    let mut superseded_roots: HashMap<RunId, HashSet<NodeId>> = HashMap::new();
    for attempt in attempts {
        if attempt.status != OrchestrationV2RunAttemptStatus::Superseded {
            continue;
        }
        superseded_roots
            .entry(attempt.run_id.clone())
            .or_default()
            .insert(attempt.root_node_id.clone());
    }
    let interrupt_runs = items
        .into_iter()
        .filter(|item| item.type_tag() == "run_interrupt_request")
        .map(|item| item.run_id().cloned())
        .collect();
    TurnItemVisibility {
        statuses,
        superseded_roots,
        interrupt_runs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare item in the shape the upstream test passes.
    struct Item {
        tag: &'static str,
        run_id: Option<RunId>,
        node_id: Option<NodeId>,
        intent: Option<OrchestrationV2UserMessageInputIntent>,
    }

    impl TimelineTurnItem for Item {
        fn type_tag(&self) -> &str {
            self.tag
        }
        fn run_id(&self) -> Option<&RunId> {
            self.run_id.as_ref()
        }
        fn node_id(&self) -> Option<&NodeId> {
            self.node_id.as_ref()
        }
        fn input_intent(&self) -> Option<OrchestrationV2UserMessageInputIntent> {
            self.intent
        }
    }

    fn run_id() -> RunId {
        RunId::new("run:timeline-visibility").unwrap()
    }
    fn node_id() -> NodeId {
        NodeId::new("node:timeline-visibility").unwrap()
    }
    fn item(tag: &'static str) -> Item {
        Item {
            tag,
            run_id: Some(run_id()),
            node_id: Some(node_id()),
            intent: None,
        }
    }
    fn user(intent: OrchestrationV2UserMessageInputIntent) -> Item {
        Item {
            intent: Some(intent),
            ..item("user_message")
        }
    }

    /// Both entry points must agree, as upstream's `describe.each` checks.
    fn visible(
        target: &Item,
        status: OrchestrationV2RunStatus,
        attempts: &[(NodeId, OrchestrationV2RunAttemptStatus)],
        items: &[&Item],
    ) -> bool {
        let id = run_id();
        let runs = [TimelineRun { id: &id, status }];
        let attempts = attempts
            .iter()
            .map(|(root, status)| TimelineRunAttempt {
                run_id: &id,
                root_node_id: root,
                status: *status,
            })
            .collect::<Vec<_>>();
        let point =
            is_orchestration_v2_turn_item_visible(target, &runs, &attempts, items.iter().copied());
        let indexed =
            create_orchestration_v2_turn_item_visibility(&runs, &attempts, items.iter().copied())
                .is_visible(target);
        assert_eq!(point, indexed, "point and indexed visibility must agree");
        point
    }

    use OrchestrationV2RunAttemptStatus as Attempt;
    use OrchestrationV2RunStatus as Run;
    use OrchestrationV2UserMessageInputIntent as Intent;

    #[test]
    fn hides_unpaired_interruption_results_from_superseded_attempts() {
        let result = item("run_interrupt_result");
        assert!(!visible(
            &result,
            Run::Running,
            &[(node_id(), Attempt::Superseded)],
            &[&result]
        ));
    }

    #[test]
    fn keeps_paired_interruption_results_from_superseded_attempts() {
        let request = item("run_interrupt_request");
        let result = item("run_interrupt_result");
        assert!(visible(
            &result,
            Run::Running,
            &[(node_id(), Attempt::Superseded)],
            &[&request, &result]
        ));
    }

    #[test]
    fn keeps_interruption_results_from_terminal_attempts_without_a_request() {
        let result = item("run_interrupt_result");
        assert!(visible(
            &result,
            Run::Interrupted,
            &[(node_id(), Attempt::Interrupted)],
            &[&result]
        ));
    }

    #[test]
    fn keeps_interruption_results_from_terminal_attempts_with_a_request() {
        let request = item("run_interrupt_request");
        let result = item("run_interrupt_result");
        assert!(visible(
            &result,
            Run::Interrupted,
            &[(node_id(), Attempt::Interrupted)],
            &[&request, &result]
        ));
    }

    #[test]
    fn hides_queued_user_messages_once_their_run_is_cancelled() {
        let message = user(Intent::QueuedTurn);
        assert!(!visible(&message, Run::Cancelled, &[], &[&message]));
    }

    #[test]
    fn keeps_queued_user_messages_while_their_run_is_queued() {
        let message = user(Intent::QueuedTurn);
        assert!(visible(&message, Run::Queued, &[], &[&message]));
    }

    #[test]
    fn keeps_non_queued_user_messages_on_cancelled_runs() {
        let message = user(Intent::TurnStart);
        assert!(visible(&message, Run::Cancelled, &[], &[&message]));
    }

    #[test]
    fn does_not_hide_an_interruption_because_another_attempt_was_superseded() {
        let result = item("run_interrupt_result");
        let older = NodeId::new("node:timeline-visibility:older").unwrap();
        assert!(visible(
            &result,
            Run::Interrupted,
            &[
                (older, Attempt::Superseded),
                (node_id(), Attempt::Interrupted)
            ],
            &[&result]
        ));
    }

    #[test]
    fn hides_items_of_rolled_back_runs() {
        let message = user(Intent::TurnStart);
        assert!(!visible(&message, Run::RolledBack, &[], &[&message]));
    }
}
