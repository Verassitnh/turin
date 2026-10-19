//! Port of upstream/packages/shared/src/orchestrationV2Timeline.ts
//!
//! Upstream's `Pick<...>` inputs become small borrowed structs over the
//! timeline mirror enums (`super::turn_item` / `super::types`), and the
//! `{ item, runs, attempts, items }` input objects become positional
//! arguments with the same names.
//!
//! Port of upstream/apps/web/src/lib/orchestrationV2Timeline.ts.

use std::collections::{HashMap, HashSet};

use super::turn_item::TurnItemType;
use super::types::{AttemptStatus, InputIntent, RunStatus};

/// `TimelineRun`: `Pick<OrchestrationV2Run, "id" | "status">`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimelineRun<'a> {
    pub id: &'a str,
    pub status: RunStatus,
}

/// `TimelineRunAttempt`: `Pick<OrchestrationV2RunAttempt, "runId" | "rootNodeId" | "status">`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimelineRunAttempt<'a> {
    pub run_id: &'a str,
    pub root_node_id: &'a str,
    pub status: AttemptStatus,
}

/// `TimelineTurnItem`: `Pick<OrchestrationV2TurnItem, "type" | "runId" | "nodeId">`
/// plus the user message's optional `inputIntent`. `type` is `item_type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimelineTurnItem<'a> {
    pub item_type: TurnItemType,
    pub run_id: Option<&'a str>,
    pub node_id: Option<&'a str>,
    pub input_intent: Option<InputIntent>,
}

pub fn is_orchestration_v2_superseded_interrupt(
    item: &TimelineTurnItem<'_>,
    attempts: &[TimelineRunAttempt<'_>],
    items: &[TimelineTurnItem<'_>],
) -> bool {
    let (TurnItemType::RunInterruptResult, Some(run_id), Some(node_id)) =
        (item.item_type, item.run_id, item.node_id)
    else {
        return false;
    };

    let is_superseded = attempts.iter().any(|attempt| {
        attempt.run_id == run_id
            && attempt.root_node_id == node_id
            && attempt.status == AttemptStatus::Superseded
    });
    if !is_superseded {
        return false;
    }

    // Paired stop-then-steer results have a matching request on the same run and
    // must stay visible. Legacy plain-steer results have no request and stay hidden.
    let has_matching_request = items.iter().any(|candidate| {
        candidate.item_type == TurnItemType::RunInterruptRequest && candidate.run_id == Some(run_id)
    });
    !has_matching_request
}

pub fn is_orchestration_v2_turn_item_visible(
    item: &TimelineTurnItem<'_>,
    runs: &[TimelineRun<'_>],
    attempts: &[TimelineRunAttempt<'_>],
    items: &[TimelineTurnItem<'_>],
) -> bool {
    if let Some(run_id) = item.run_id
        && runs
            .iter()
            .any(|run| run.id == run_id && run.status == RunStatus::RolledBack)
    {
        return false;
    }

    // A queued message is composer state, not conversation history. When its run
    // is cancelled the input never reached the provider, so it must not surface
    // as a transcript row.
    if item.item_type == TurnItemType::UserMessage
        && item.input_intent == Some(InputIntent::QueuedTurn)
        && let Some(run_id) = item.run_id
        && runs
            .iter()
            .any(|run| run.id == run_id && run.status == RunStatus::Cancelled)
    {
        return false;
    }

    !is_orchestration_v2_superseded_interrupt(item, attempts, items)
}

/// Index once when reconciling a whole timeline after run/attempt state changes.
pub fn create_orchestration_v2_turn_item_visibility<'a>(
    runs: &[TimelineRun<'a>],
    attempts: &[TimelineRunAttempt<'a>],
    items: &[TimelineTurnItem<'a>],
) -> impl Fn(&TimelineTurnItem<'_>) -> bool + 'a {
    // `new Map(...)`: a later run with the same id wins, as `collect` does.
    let statuses: HashMap<&'a str, RunStatus> =
        runs.iter().map(|run| (run.id, run.status)).collect();
    let mut superseded_roots: HashMap<&'a str, HashSet<&'a str>> = HashMap::new();
    for attempt in attempts {
        if attempt.status != AttemptStatus::Superseded {
            continue;
        }
        superseded_roots
            .entry(attempt.run_id)
            .or_default()
            .insert(attempt.root_node_id);
    }
    // Upstream's set also holds `null` for run-less requests; lookups below
    // only ever use a non-null run id, so those entries are dropped.
    let interrupt_runs: HashSet<&'a str> = items
        .iter()
        .filter(|item| item.item_type == TurnItemType::RunInterruptRequest)
        .filter_map(|item| item.run_id)
        .collect();
    move |item| {
        let status = item.run_id.and_then(|run_id| statuses.get(run_id)).copied();
        if status == Some(RunStatus::RolledBack) {
            return false;
        }
        if status == Some(RunStatus::Cancelled)
            && item.item_type == TurnItemType::UserMessage
            && item.input_intent == Some(InputIntent::QueuedTurn)
        {
            return false;
        }
        !(item.item_type == TurnItemType::RunInterruptResult
            && match (item.run_id, item.node_id) {
                (Some(run_id), Some(node_id)) => {
                    superseded_roots
                        .get(run_id)
                        .is_some_and(|roots| roots.contains(node_id))
                        && !interrupt_runs.contains(run_id)
                }
                _ => false,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN_ID: &str = "run:timeline-visibility";
    const NODE_ID: &str = "node:timeline-visibility";

    struct Input<'a> {
        item: TimelineTurnItem<'a>,
        runs: Vec<TimelineRun<'a>>,
        attempts: Vec<TimelineRunAttempt<'a>>,
        items: Vec<TimelineTurnItem<'a>>,
    }

    type IsVisible = fn(&Input<'_>) -> bool;

    /// Upstream's `describe.each([["point", ...], ["indexed", ...]])`.
    const VARIANTS: [(&str, IsVisible); 2] = [
        ("point", |input| {
            is_orchestration_v2_turn_item_visible(
                &input.item,
                &input.runs,
                &input.attempts,
                &input.items,
            )
        }),
        ("indexed", |input| {
            create_orchestration_v2_turn_item_visibility(&input.runs, &input.attempts, &input.items)(
                &input.item,
            )
        }),
    ];

    fn check(input: Input<'_>, expected: bool) {
        for (name, is_visible) in VARIANTS {
            assert_eq!(is_visible(&input), expected, "{name} timeline visibility");
        }
    }

    fn item(item_type: TurnItemType) -> TimelineTurnItem<'static> {
        TimelineTurnItem {
            item_type,
            run_id: Some(RUN_ID),
            node_id: Some(NODE_ID),
            input_intent: None,
        }
    }

    fn user_message(input_intent: InputIntent) -> TimelineTurnItem<'static> {
        TimelineTurnItem {
            input_intent: Some(input_intent),
            ..item(TurnItemType::UserMessage)
        }
    }

    fn run(status: RunStatus) -> TimelineRun<'static> {
        TimelineRun { id: RUN_ID, status }
    }

    fn attempt(status: AttemptStatus) -> TimelineRunAttempt<'static> {
        TimelineRunAttempt {
            run_id: RUN_ID,
            root_node_id: NODE_ID,
            status,
        }
    }

    #[test]
    fn hides_unpaired_interruption_results_from_superseded_attempts() {
        check(
            Input {
                item: item(TurnItemType::RunInterruptResult),
                runs: vec![run(RunStatus::Running)],
                attempts: vec![attempt(AttemptStatus::Superseded)],
                items: vec![item(TurnItemType::RunInterruptResult)],
            },
            false,
        );
    }

    #[test]
    fn keeps_paired_interruption_results_from_superseded_attempts() {
        check(
            Input {
                item: item(TurnItemType::RunInterruptResult),
                runs: vec![run(RunStatus::Running)],
                attempts: vec![attempt(AttemptStatus::Superseded)],
                items: vec![
                    item(TurnItemType::RunInterruptRequest),
                    item(TurnItemType::RunInterruptResult),
                ],
            },
            true,
        );
    }

    #[test]
    fn keeps_interruption_results_from_terminal_attempts_without_a_request() {
        check(
            Input {
                item: item(TurnItemType::RunInterruptResult),
                runs: vec![run(RunStatus::Interrupted)],
                attempts: vec![attempt(AttemptStatus::Interrupted)],
                items: vec![item(TurnItemType::RunInterruptResult)],
            },
            true,
        );
    }

    #[test]
    fn keeps_interruption_results_from_terminal_attempts_with_a_request() {
        check(
            Input {
                item: item(TurnItemType::RunInterruptResult),
                runs: vec![run(RunStatus::Interrupted)],
                attempts: vec![attempt(AttemptStatus::Interrupted)],
                items: vec![
                    item(TurnItemType::RunInterruptRequest),
                    item(TurnItemType::RunInterruptResult),
                ],
            },
            true,
        );
    }

    #[test]
    fn hides_queued_user_messages_once_their_run_is_cancelled() {
        check(
            Input {
                item: user_message(InputIntent::QueuedTurn),
                runs: vec![run(RunStatus::Cancelled)],
                attempts: vec![],
                items: vec![user_message(InputIntent::QueuedTurn)],
            },
            false,
        );
    }

    #[test]
    fn keeps_queued_user_messages_while_their_run_is_queued() {
        check(
            Input {
                item: user_message(InputIntent::QueuedTurn),
                runs: vec![run(RunStatus::Queued)],
                attempts: vec![],
                items: vec![user_message(InputIntent::QueuedTurn)],
            },
            true,
        );
    }

    #[test]
    fn keeps_non_queued_user_messages_on_cancelled_runs() {
        check(
            Input {
                item: user_message(InputIntent::TurnStart),
                runs: vec![run(RunStatus::Cancelled)],
                attempts: vec![],
                items: vec![user_message(InputIntent::TurnStart)],
            },
            true,
        );
    }

    #[test]
    fn does_not_hide_an_interruption_because_another_attempt_was_superseded() {
        check(
            Input {
                item: item(TurnItemType::RunInterruptResult),
                runs: vec![run(RunStatus::Interrupted)],
                attempts: vec![
                    TimelineRunAttempt {
                        root_node_id: "node:timeline-visibility:older",
                        ..attempt(AttemptStatus::Superseded)
                    },
                    attempt(AttemptStatus::Interrupted),
                ],
                items: vec![item(TurnItemType::RunInterruptResult)],
            },
            true,
        );
    }
}
