//! Port of upstream/packages/shared/src/orchestrationV2ThreadError.ts
//!
//! Lives here until `turin-shared` (T05b) carries its contract-dependent files.

use turin_contracts::IsoDateTime;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ProviderFailure, OrchestrationV2ProviderFailureClass, OrchestrationV2Run,
    OrchestrationV2RunStatus, OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
};

use crate::projection::turn_item;

/// Only a failed root turn of the current run owns the thread's failure state.
pub fn latest_root_provider_failure<'a>(
    run: Option<&OrchestrationV2Run>,
    turn_items: &'a [OrchestrationV2TurnItem],
) -> Option<&'a OrchestrationV2ProviderFailure> {
    let run = run.filter(|run| run.status == OrchestrationV2RunStatus::Failed)?;
    let mut latest: Option<(
        &'a OrchestrationV2ProviderFailure,
        turn_item::TurnItemCommon<'a>,
    )> = None;
    for item in turn_items {
        let OrchestrationV2TurnItem::Error { failure, .. } = item else {
            continue;
        };
        let common = turn_item::common(item);
        if common.status != OrchestrationV2TurnItemStatus::Failed
            || common.run_id != Some(&run.id)
            || common.node_id != run.root_node_id.as_ref()
        {
            continue;
        }
        let newer = match &latest {
            None => true,
            Some((_, current)) => {
                common.updated_at > current.updated_at
                    || (common.updated_at == current.updated_at
                        && (common.ordinal > current.ordinal
                            || (common.ordinal == current.ordinal && common.id > current.id)))
            }
        };
        if newer {
            latest = Some((failure, common));
        }
    }
    latest.map(|(failure, _)| failure)
}

/// `threadErrorSummary`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThreadErrorSummary {
    pub usage_limit_reset_at: Option<IsoDateTime>,
    pub last_error: Option<String>,
    pub last_error_class: Option<OrchestrationV2ProviderFailureClass>,
}

/// A distinct session failure supersedes the turn's classification.
pub fn thread_error_summary(
    failure: Option<&OrchestrationV2ProviderFailure>,
    session_error: Option<&str>,
) -> ThreadErrorSummary {
    let failure_message = failure.map(|failure| failure.message.as_str());
    let session_differs = session_error.is_some() && session_error != failure_message;
    let current_failure = if session_differs { None } else { failure };
    ThreadErrorSummary {
        usage_limit_reset_at: current_failure
            .filter(|failure| failure.class == OrchestrationV2ProviderFailureClass::UsageLimit)
            .and_then(|failure| failure.reset_at.clone().flatten()),
        last_error: session_error
            .map(str::to_owned)
            .or_else(|| failure_message.map(str::to_owned)),
        last_error_class: if session_differs {
            None
        } else {
            failure.map(|failure| failure.class)
        },
    }
}

pub fn latest_executed_run(runs: &[OrchestrationV2Run]) -> Option<&OrchestrationV2Run> {
    let mut latest: Option<&OrchestrationV2Run> = None;
    for run in runs {
        if run.status == OrchestrationV2RunStatus::Queued {
            continue;
        }
        if run.status == OrchestrationV2RunStatus::Cancelled && run.started_at.is_none() {
            continue;
        }
        if latest.is_none_or(|latest| run_ran_after(run, latest)) {
            latest = Some(run);
        }
    }
    latest
}

/// The slice of a run `run_ran_after` compares.
pub trait RunEnd {
    fn ordinal(&self) -> u64;
    fn completed_at_millis(&self) -> Option<i64>;
}

impl RunEnd for OrchestrationV2Run {
    fn ordinal(&self) -> u64 {
        self.ordinal.get()
    }
    fn completed_at_millis(&self) -> Option<i64> {
        self.completed_at.map(|at| at.0.timestamp_millis())
    }
}

/// Whether started `run` ran after `other`. Ordinals follow submission, but a
/// run can start ahead of a held queue (a restart continuation, or a message
/// sent while the queue is held), so a queued run resumed later can have a
/// lower ordinal than one that already ended. An unfinished run is the latest.
pub fn run_ran_after<A: RunEnd + ?Sized, B: RunEnd + ?Sized>(run: &A, other: &B) -> bool {
    // JS `!candidate.completedAt` is true only for null here; epoch 0 is a valid end.
    let end = |millis: Option<i64>| millis.map_or(f64::INFINITY, |millis| millis as f64);
    let (run_end, other_end) = (
        end(run.completed_at_millis()),
        end(other.completed_at_millis()),
    );
    if run_end == other_end {
        run.ordinal() > other.ordinal()
    } else {
        run_end > other_end
    }
}

/// The latest run that actually started, when it stopped because the
/// subscription limit was reached. Queued messages after that run must stay
/// queued instead of being sent into the same limit.
pub fn usage_limit_blocked_run<'a>(
    runs: &'a [OrchestrationV2Run],
    turn_items: &[OrchestrationV2TurnItem],
    session_error: Option<&str>,
) -> Option<&'a OrchestrationV2Run> {
    let executed = latest_executed_run(runs)?;
    if executed.status != OrchestrationV2RunStatus::Failed {
        return None;
    }
    let summary = thread_error_summary(
        latest_root_provider_failure(Some(executed), turn_items),
        session_error,
    );
    (summary.last_error_class == Some(OrchestrationV2ProviderFailureClass::UsageLimit))
        .then_some(executed)
}

/// That limited run, when newer runs were queued or cancelled before starting.
/// Callers that treat the highest-ordinal run as the thread outcome would
/// otherwise hide the limit.
pub fn usage_limit_run_presented_as_latest<'a>(
    runs: &'a [OrchestrationV2Run],
    turn_items: &[OrchestrationV2TurnItem],
    session_error: Option<&str>,
) -> Option<&'a OrchestrationV2Run> {
    let blocked = usage_limit_blocked_run(runs, turn_items, session_error)?;
    runs.iter()
        .any(|run| run.ordinal > blocked.ordinal)
        .then_some(blocked)
}

/// The newest run that is not waiting in a held queue, or `None` when only
/// held runs exist. A held queue waits for the user, so its runs never stand
/// for the thread's outcome. The SQL thread shell selects the same run.
pub fn latest_unheld_run(runs: &[OrchestrationV2Run]) -> Option<&OrchestrationV2Run> {
    let mut latest: Option<&OrchestrationV2Run> = None;
    for run in runs {
        if run.status == OrchestrationV2RunStatus::Queued && run.queue_held == Some(true) {
            continue;
        }
        if latest.is_none_or(|latest| run.ordinal > latest.ordinal) {
            latest = Some(run);
        }
    }
    latest
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(id: &str, ordinal: u64, status: &str, completed_at: Option<&str>) -> OrchestrationV2Run {
        serde_json::from_value(json!({
            "id": id, "threadId": "thread-1", "ordinal": ordinal,
            "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "gpt-5.4"},
            "providerThreadId": null, "userMessageId": "message-1",
            "rootNodeId": format!("root:{id}"), "activeAttemptId": null, "status": status,
            "requestedAt": "2026-01-01T00:00:00.000Z",
            "startedAt": "2026-01-01T00:00:01.000Z", "completedAt": completed_at,
            "checkpointId": null, "contextHandoffId": null
        }))
        .unwrap()
    }

    fn failure_item(
        run_id: &str,
        ordinal: u64,
        class: &str,
        message: &str,
    ) -> OrchestrationV2TurnItem {
        serde_json::from_value(json!({
            "id": format!("error:{run_id}:{ordinal}"), "threadId": "thread-1",
            "runId": run_id, "nodeId": format!("root:{run_id}"),
            "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
            "parentItemId": null, "ordinal": ordinal, "status": "failed", "title": null,
            "startedAt": null, "completedAt": null, "updatedAt": "2026-01-01T00:00:02.000Z",
            "type": "error",
            "failure": {"class": class, "message": message, "code": null, "retryable": null,
                "resetAt": "2026-01-02T00:00:00.000Z"}
        }))
        .unwrap()
    }

    #[test]
    fn root_failure_is_the_newest_failed_error_of_a_failed_run() {
        let runs = [run("run-1", 1, "failed", Some("2026-01-01T00:00:03.000Z"))];
        let items = [
            failure_item("run-1", 1, "provider_error", "first"),
            failure_item("run-1", 2, "usage_limit", "limit"),
        ];
        let failure = latest_root_provider_failure(runs.first(), &items).unwrap();
        assert_eq!(failure.message.as_str(), "limit");
        assert!(latest_root_provider_failure(None, &items).is_none());
        let running = [run("run-1", 1, "running", None)];
        assert!(latest_root_provider_failure(running.first(), &items).is_none());
    }

    #[test]
    fn a_distinct_session_error_supersedes_the_turn_classification() {
        let runs = [run("run-1", 1, "failed", Some("2026-01-01T00:00:03.000Z"))];
        let items = [failure_item("run-1", 1, "usage_limit", "limit")];
        let failure = latest_root_provider_failure(runs.first(), &items);
        let same = thread_error_summary(failure, Some("limit"));
        assert_eq!(
            same.last_error_class,
            Some(OrchestrationV2ProviderFailureClass::UsageLimit)
        );
        assert_eq!(
            same.usage_limit_reset_at.as_deref(),
            Some("2026-01-02T00:00:00.000Z")
        );
        let other = thread_error_summary(failure, Some("session died"));
        assert_eq!(other.last_error.as_deref(), Some("session died"));
        assert_eq!(other.last_error_class, None);
        assert_eq!(other.usage_limit_reset_at, None);
        let none = thread_error_summary(None, None);
        assert_eq!(none, ThreadErrorSummary::default());
    }

    #[test]
    fn run_order_uses_end_time_then_ordinal_and_unfinished_runs_are_latest() {
        let ended_late_low_ordinal = run("a", 1, "completed", Some("2026-01-01T00:10:00.000Z"));
        let ended_early_high_ordinal = run("b", 2, "completed", Some("2026-01-01T00:05:00.000Z"));
        assert!(run_ran_after(
            &ended_late_low_ordinal,
            &ended_early_high_ordinal
        ));
        let unfinished = run("c", 0 + 3, "running", None);
        assert!(run_ran_after(&unfinished, &ended_late_low_ordinal));
        let same_end = run("d", 9, "completed", Some("2026-01-01T00:10:00.000Z"));
        assert!(run_ran_after(&same_end, &ended_late_low_ordinal));
        let runs = [ended_late_low_ordinal, ended_early_high_ordinal];
        assert_eq!(latest_executed_run(&runs).unwrap().id.as_str(), "a");
    }

    #[test]
    fn executed_run_skips_queued_and_never_started_cancelled_runs() {
        let mut cancelled = run("x", 2, "cancelled", None);
        cancelled.started_at = None;
        let queued = run("y", 3, "queued", None);
        let done = run("z", 1, "completed", Some("2026-01-01T00:00:03.000Z"));
        let runs = [done, cancelled, queued];
        assert_eq!(latest_executed_run(&runs).unwrap().id.as_str(), "z");
    }

    #[test]
    fn a_usage_limited_run_is_presented_when_newer_runs_never_started() {
        let limited = run("limited", 1, "failed", Some("2026-01-01T00:00:03.000Z"));
        let mut queued = run("queued", 2, "queued", None);
        queued.started_at = None;
        let runs = [limited, queued];
        let items = [failure_item("limited", 1, "usage_limit", "limit")];
        assert_eq!(
            usage_limit_run_presented_as_latest(&runs, &items, None).map(|run| run.id.as_str()),
            Some("limited")
        );
        // No newer run: nothing to present.
        assert!(usage_limit_run_presented_as_latest(&runs[..1], &items, None).is_none());
        // A different failure class is not a usage limit.
        let other = [failure_item("limited", 1, "provider_error", "boom")];
        assert!(usage_limit_blocked_run(&runs, &other, None).is_none());
    }

    #[test]
    fn unheld_run_skips_held_queue_entries() {
        let done = run("done", 1, "completed", Some("2026-01-01T00:00:03.000Z"));
        let mut held = run("held", 2, "queued", None);
        held.queue_held = Some(true);
        let runs = [done, held];
        assert_eq!(latest_unheld_run(&runs).unwrap().id.as_str(), "done");
        assert!(latest_unheld_run(&runs[1..]).is_none());
    }
}
