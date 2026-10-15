//! Port of upstream/packages/client-runtime/src/state/threadSubagents.ts
//!
//! The subagent roster for one turn, and the compact label the mobile composer
//! pill renders for it.
//!
//! Scoped to a run rather than the whole thread: a thread accumulates every
//! subagent it ever spawned, while the pill and its sheet answer "what is this
//! turn doing right now". Statuses come from the v2 projection directly, so
//! this stays a pure fold over `runs` and `subagents`.

use std::sync::Arc;

use crate::state::contracts::{OrchestrationV2Run, OrchestrationV2Subagent, RunId};
use crate::state::subagent_runtime::{
    RuntimeSubagentStatus, is_active_subagent_status, is_terminal_subagent_status,
};
use crate::state::support::collate::locale_compare;
use crate::state::thread_workflows::resolve_active_thread_run;
use crate::state::time::epoch_millis;

#[derive(Clone, Debug, PartialEq)]
pub struct ThreadTurnSubagents {
    /// The run the roster belongs to: the active one, else the most recent one that spawned agents.
    pub run_id: Option<RunId>,
    pub turn_active: bool,
    /// Spawn order: startedAt, falling back to updatedAt for agents that never started.
    pub subagents: Vec<Arc<OrchestrationV2Subagent>>,
    pub live_count: usize,
    pub settled_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentPillSegment {
    pub label: String,
    pub accessibility_label: String,
}

fn order_key(subagent: &OrchestrationV2Subagent) -> i64 {
    epoch_millis(subagent.started_at.as_ref().unwrap_or(&subagent.updated_at))
}

/// `None` when the thread has never spawned a subagent.
pub fn derive_thread_turn_subagents(
    runs: &[Arc<OrchestrationV2Run>],
    subagents: &[Arc<OrchestrationV2Subagent>],
) -> Option<ThreadTurnSubagents> {
    if subagents.is_empty() {
        return None;
    }
    let active_run = resolve_active_thread_run(runs);
    // With no live run the newest roster is still worth showing: a turn that
    // just finished leaves results the user has not read yet.
    let latest_updated = subagents
        .iter()
        .skip(1)
        .fold(&subagents[0], |latest, subagent| {
            if epoch_millis(&subagent.updated_at) > epoch_millis(&latest.updated_at) {
                subagent
            } else {
                latest
            }
        });
    let run_id = active_run
        .map(|run| run.id.clone())
        .or_else(|| latest_updated.run_id.clone());
    let mut roster: Vec<Arc<OrchestrationV2Subagent>> = subagents
        .iter()
        .filter(|subagent| subagent.run_id == run_id)
        .cloned()
        .collect();
    roster.sort_by(|left, right| {
        order_key(left)
            .cmp(&order_key(right))
            .then_with(|| locale_compare(left.id.as_str(), right.id.as_str()))
    });
    if roster.is_empty() {
        return None;
    }

    let (mut live_count, mut settled_count) = (0, 0);
    for subagent in &roster {
        let status = RuntimeSubagentStatus::from(subagent.status);
        if is_active_subagent_status(status) {
            live_count += 1;
        } else if is_terminal_subagent_status(status) {
            settled_count += 1;
        }
    }
    Some(ThreadTurnSubagents {
        run_id,
        turn_active: active_run.is_some(),
        subagents: roster,
        live_count,
        settled_count,
    })
}

/// What the pill's agents segment says, or `None` while it should stay hidden.
/// The segment is for work in flight: once the turn settles it goes away
/// rather than leaving a stale count above the composer.
pub fn resolve_subagent_pill_segment(
    turn: Option<&ThreadTurnSubagents>,
) -> Option<SubagentPillSegment> {
    let turn = turn?;
    if !turn.turn_active && turn.live_count == 0 {
        return None;
    }
    let total = turn.subagents.len();
    if turn.live_count > 0 {
        return Some(SubagentPillSegment {
            label: format!("{}/{total}", turn.live_count),
            accessibility_label: format!("{} of {total} agents working", turn.live_count),
        });
    }
    Some(SubagentPillSegment {
        label: format!("{total} done"),
        accessibility_label: format!(
            "{total} {} done",
            if total == 1 { "agent" } else { "agents" }
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fixtures::{from_json, v2_run};
    use serde_json::json;

    fn subagent(id: &str, extra: serde_json::Value) -> Arc<OrchestrationV2Subagent> {
        let mut value = json!({
            "id": id,
            "threadId": "thread-1",
            "runId": "run-1",
            "parentNodeId": "node-1",
            "origin": "app_owned",
            "createdBy": "agent",
            "driver": "codex",
            "providerInstanceId": "codex",
            "providerThreadId": null,
            "childThreadId": "thread-child",
            "nativeTaskRef": null,
            "prompt": "Do the thing",
            "title": "Worker",
            "model": "gpt-5.4",
            "status": "running",
            "result": null,
            "startedAt": "2026-06-20T00:00:00.000Z",
            "completedAt": null,
            "updatedAt": "2026-06-20T00:00:01.000Z",
        });
        for (key, field) in extra.as_object().unwrap() {
            value[key] = field.clone();
        }
        Arc::new(from_json(value))
    }

    fn run(id: &str, status: &str) -> Arc<OrchestrationV2Run> {
        Arc::new(v2_run(id, 1, status))
    }

    fn ids(turn: &ThreadTurnSubagents) -> Vec<&str> {
        turn.subagents
            .iter()
            .map(|entry| entry.id.as_str())
            .collect()
    }

    #[test]
    fn returns_none_when_the_thread_has_never_spawned_an_agent() {
        assert_eq!(
            derive_thread_turn_subagents(&[run("run-1", "running")], &[]),
            None
        );
    }

    #[test]
    fn scopes_the_roster_to_the_active_run_and_orders_it_by_start_time() {
        let turn = derive_thread_turn_subagents(
            &[run("run-0", "completed"), run("run-1", "running")],
            &[
                subagent("b", json!({ "startedAt": "2026-06-20T00:00:05.000Z" })),
                subagent("old", json!({ "runId": "run-0" })),
                subagent("a", json!({ "startedAt": "2026-06-20T00:00:02.000Z" })),
            ],
        )
        .unwrap();
        assert_eq!(turn.run_id.as_ref().unwrap().as_str(), "run-1");
        assert_eq!(ids(&turn), vec!["a", "b"]);
        assert!(turn.turn_active);
    }

    #[test]
    fn falls_back_to_the_most_recently_updated_agents_run_once_the_turn_settles() {
        let turn = derive_thread_turn_subagents(
            &[run("run-1", "completed"), run("run-2", "completed")],
            &[
                subagent("old", json!({ "status": "completed", "updatedAt": "2026-06-20T00:00:01.000Z" })),
                subagent("recent", json!({ "runId": "run-2", "status": "completed", "updatedAt": "2026-06-20T00:00:09.000Z" })),
            ],
        )
        .unwrap();
        assert_eq!(turn.run_id.as_ref().unwrap().as_str(), "run-2");
        assert!(!turn.turn_active);
        assert_eq!(ids(&turn), vec!["recent"]);
    }

    #[test]
    fn counts_idle_as_neither_working_nor_done() {
        let turn = derive_thread_turn_subagents(
            &[run("run-1", "running")],
            &[
                subagent("a", json!({ "status": "running" })),
                subagent("b", json!({ "status": "idle" })),
                subagent("c", json!({ "status": "failed" })),
            ],
        )
        .unwrap();
        assert_eq!(turn.live_count, 1);
        assert_eq!(turn.settled_count, 1);
        assert_eq!(turn.subagents.len(), 3);
    }

    fn segment(run_status: &str, statuses: &[&str]) -> Option<SubagentPillSegment> {
        let agents: Vec<_> = statuses
            .iter()
            .enumerate()
            .map(|(index, status)| subagent(&format!("agent-{index}"), json!({ "status": status })))
            .collect();
        resolve_subagent_pill_segment(
            derive_thread_turn_subagents(&[run("run-1", run_status)], &agents).as_ref(),
        )
    }

    #[test]
    fn shows_how_many_of_the_turns_agents_are_still_working() {
        assert_eq!(
            segment("running", &["running", "waiting", "completed"]),
            Some(SubagentPillSegment {
                label: "2/3".into(),
                accessibility_label: "2 of 3 agents working".into(),
            })
        );
    }

    #[test]
    fn reports_the_finished_roster_while_the_turn_is_still_running() {
        assert_eq!(
            segment("running", &["completed"]),
            Some(SubagentPillSegment {
                label: "1 done".into(),
                accessibility_label: "1 agent done".into(),
            })
        );
    }

    #[test]
    fn hides_itself_once_the_turn_is_over_and_nothing_is_working() {
        assert_eq!(segment("completed", &["completed"]), None);
        assert_eq!(resolve_subagent_pill_segment(None), None);
    }

    #[test]
    fn stays_visible_after_the_turn_ends_while_an_agent_is_still_working() {
        assert_eq!(segment("completed", &["running"]).unwrap().label, "1/1");
    }
}
