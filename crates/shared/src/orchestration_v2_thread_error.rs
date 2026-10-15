//! Port of upstream/packages/shared/src/orchestrationV2ThreadError.ts
//!
//! Upstream reads run and turn-item fields off the decoded objects. Here the run ordering rules
//! are written against [`RunOrder`], which `OrchestrationV2Run` implements, so they can be tested
//! without building a full run. Upstream has no test file for this module.

use turin_contracts::base_schemas::{IsoDateTime, NodeId, RunId};
use turin_contracts::orchestration_v2::{
    OrchestrationV2ProviderFailure, OrchestrationV2ProviderFailureClass, OrchestrationV2Run,
    OrchestrationV2RunStatus, OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
};

/// The run fields the thread-error rules read.
pub trait RunOrder {
    fn run_id(&self) -> &RunId;
    fn root_node_id(&self) -> Option<&NodeId>;
    fn ordinal(&self) -> u64;
    fn status(&self) -> OrchestrationV2RunStatus;
    fn started(&self) -> bool;
    /// `completedAt` as epoch milliseconds, when set.
    fn completed_at_millis(&self) -> Option<i64>;
    fn queue_held(&self) -> bool;
}

impl RunOrder for OrchestrationV2Run {
    fn run_id(&self) -> &RunId {
        &self.id
    }

    fn root_node_id(&self) -> Option<&NodeId> {
        self.root_node_id.as_ref()
    }

    fn ordinal(&self) -> u64 {
        self.ordinal.get()
    }

    fn status(&self) -> OrchestrationV2RunStatus {
        self.status
    }

    fn started(&self) -> bool {
        self.started_at.is_some()
    }

    fn completed_at_millis(&self) -> Option<i64> {
        self.completed_at
            .as_ref()
            .map(|at| at.as_datetime().timestamp_millis())
    }

    fn queue_held(&self) -> bool {
        self.queue_held == Some(true)
    }
}

/// `Infinity` for a run that has not completed, so it sorts after every finished run.
fn end_key(run: &impl RunOrder) -> (bool, i64) {
    match run.completed_at_millis() {
        Some(millis) => (false, millis),
        None => (true, 0),
    }
}

/// Whether started `run` ran after `other`. Ordinals follow submission, but a run can start ahead
/// of a held queue (a restart continuation, or a message sent while the queue is held), so a queued
/// run resumed later can have a lower ordinal than one that already ended. An unfinished run is the
/// latest.
pub fn run_ran_after(run: &impl RunOrder, other: &impl RunOrder) -> bool {
    let (run_end, other_end) = (end_key(run), end_key(other));
    if run_end == other_end {
        run.ordinal() > other.ordinal()
    } else {
        run_end > other_end
    }
}

/// The newest run that actually started or finished, skipping queued runs and cancelled runs that
/// never started.
pub fn latest_executed_run<R: RunOrder>(runs: &[R]) -> Option<&R> {
    let mut latest: Option<&R> = None;
    for run in runs {
        if run.status() == OrchestrationV2RunStatus::Queued {
            continue;
        }
        if run.status() == OrchestrationV2RunStatus::Cancelled && !run.started() {
            continue;
        }
        if latest.is_none_or(|current| run_ran_after(run, current)) {
            latest = Some(run);
        }
    }
    latest
}

/// The newest run that is not waiting in a held queue, or None when only held runs exist. A held
/// queue waits for the user, so its runs never stand for the thread's outcome.
pub fn latest_unheld_run<R: RunOrder>(runs: &[R]) -> Option<&R> {
    let mut latest: Option<&R> = None;
    for run in runs {
        if run.status() == OrchestrationV2RunStatus::Queued && run.queue_held() {
            continue;
        }
        if latest.is_none_or(|current| run.ordinal() > current.ordinal()) {
            latest = Some(run);
        }
    }
    latest
}

/// Only a failed root turn of the current run owns the thread's failure state.
pub fn latest_root_provider_failure<'a>(
    run: Option<&impl RunOrder>,
    turn_items: &'a [OrchestrationV2TurnItem],
) -> Option<&'a OrchestrationV2ProviderFailure> {
    let run = run.filter(|run| run.status() == OrchestrationV2RunStatus::Failed)?;
    // (updated_at millis, ordinal, id, failure) of the latest qualifying error item so far.
    let mut latest: Option<(i64, u64, &str, &'a OrchestrationV2ProviderFailure)> = None;
    for item in turn_items {
        let OrchestrationV2TurnItem::Error {
            id,
            run_id,
            node_id,
            ordinal,
            status,
            updated_at,
            failure,
            ..
        } = item
        else {
            continue;
        };
        if *status != OrchestrationV2TurnItemStatus::Failed
            || run_id.as_ref() != Some(run.run_id())
            || node_id.as_ref() != run.root_node_id()
        {
            continue;
        }
        let candidate = (
            updated_at.as_datetime().timestamp_millis(),
            *ordinal,
            id.as_str(),
            failure,
        );
        let replaces = match latest {
            None => true,
            Some((latest_updated, latest_ordinal, latest_id, _)) => {
                candidate.0 > latest_updated
                    || (candidate.0 == latest_updated
                        && (candidate.1 > latest_ordinal
                            || (candidate.1 == latest_ordinal && candidate.2 > latest_id)))
            }
        };
        if replaces {
            latest = Some(candidate);
        }
    }
    latest.map(|(_, _, _, failure)| failure)
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    let superseded = session_error.is_some_and(|error| Some(error) != failure_message);
    let current_failure = if superseded { None } else { failure };
    ThreadErrorSummary {
        usage_limit_reset_at: current_failure
            .filter(|failure| failure.class == OrchestrationV2ProviderFailureClass::UsageLimit)
            .and_then(|failure| failure.reset_at.clone().flatten()),
        last_error: session_error
            .map(str::to_owned)
            .or_else(|| failure_message.map(str::to_owned)),
        last_error_class: if superseded {
            None
        } else {
            failure.map(|failure| failure.class)
        },
    }
}

/// The latest run that actually started, when it stopped because the subscription limit was
/// reached. Queued messages after that run must stay queued instead of being sent into the same
/// limit.
pub fn usage_limit_blocked_run<'a, R: RunOrder>(
    runs: &'a [R],
    turn_items: &[OrchestrationV2TurnItem],
    session_error: Option<&str>,
) -> Option<&'a R> {
    let executed = latest_executed_run(runs)?;
    if executed.status() != OrchestrationV2RunStatus::Failed {
        return None;
    }
    let summary = thread_error_summary(
        latest_root_provider_failure(Some(executed), turn_items),
        session_error,
    );
    (summary.last_error_class == Some(OrchestrationV2ProviderFailureClass::UsageLimit))
        .then_some(executed)
}

/// The limited run, when newer runs were queued or cancelled before starting. Callers that treat
/// the highest-ordinal run as the thread outcome would otherwise hide the limit.
pub fn usage_limit_run_presented_as_latest<'a, R: RunOrder>(
    runs: &'a [R],
    turn_items: &[OrchestrationV2TurnItem],
    session_error: Option<&str>,
) -> Option<&'a R> {
    let blocked = usage_limit_blocked_run(runs, turn_items, session_error)?;
    runs.iter()
        .any(|run| run.ordinal() > blocked.ordinal())
        .then_some(blocked)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stands in for a run: only the fields the ordering rules read.
    struct Stub {
        ordinal: u64,
        status: OrchestrationV2RunStatus,
        started: bool,
        completed_at: Option<i64>,
        held: bool,
    }

    impl RunOrder for Stub {
        fn run_id(&self) -> &RunId {
            unreachable!("ordering tests do not read the run id")
        }
        fn root_node_id(&self) -> Option<&NodeId> {
            None
        }
        fn ordinal(&self) -> u64 {
            self.ordinal
        }
        fn status(&self) -> OrchestrationV2RunStatus {
            self.status
        }
        fn started(&self) -> bool {
            self.started
        }
        fn completed_at_millis(&self) -> Option<i64> {
            self.completed_at
        }
        fn queue_held(&self) -> bool {
            self.held
        }
    }

    fn stub(ordinal: u64, status: OrchestrationV2RunStatus) -> Stub {
        Stub {
            ordinal,
            status,
            started: true,
            completed_at: None,
            held: false,
        }
    }

    #[test]
    fn an_unfinished_run_is_later_than_every_finished_run() {
        let finished = Stub {
            completed_at: Some(1_000),
            ..stub(1, OrchestrationV2RunStatus::Completed)
        };
        let running = stub(0, OrchestrationV2RunStatus::Running);
        assert!(run_ran_after(&running, &finished));
        assert!(!run_ran_after(&finished, &running));
    }

    #[test]
    fn finished_runs_compare_by_completion_then_ordinal() {
        let earlier = Stub {
            completed_at: Some(1_000),
            ..stub(5, OrchestrationV2RunStatus::Completed)
        };
        let later = Stub {
            completed_at: Some(2_000),
            ..stub(1, OrchestrationV2RunStatus::Completed)
        };
        assert!(run_ran_after(&later, &earlier));
        let same_end = Stub {
            completed_at: Some(1_000),
            ..stub(6, OrchestrationV2RunStatus::Completed)
        };
        assert!(run_ran_after(&same_end, &earlier));
    }

    #[test]
    fn latest_executed_run_skips_queued_and_never_started_cancelled_runs() {
        let runs = [
            stub(1, OrchestrationV2RunStatus::Completed),
            stub(2, OrchestrationV2RunStatus::Queued),
            Stub {
                started: false,
                ..stub(3, OrchestrationV2RunStatus::Cancelled)
            },
        ];
        assert_eq!(latest_executed_run(&runs).map(|run| run.ordinal), Some(1));
    }

    #[test]
    fn latest_unheld_run_ignores_only_held_queued_runs() {
        let runs = [
            stub(1, OrchestrationV2RunStatus::Completed),
            Stub {
                held: true,
                ..stub(2, OrchestrationV2RunStatus::Queued)
            },
        ];
        assert_eq!(latest_unheld_run(&runs).map(|run| run.ordinal), Some(1));
        let only_held = [Stub {
            held: true,
            ..stub(1, OrchestrationV2RunStatus::Queued)
        }];
        assert!(latest_unheld_run(&only_held).is_none());
    }

    #[test]
    fn session_error_supersedes_the_turn_classification() {
        let summary = thread_error_summary(None, Some("boom"));
        assert_eq!(summary.last_error.as_deref(), Some("boom"));
        assert_eq!(summary.last_error_class, None);
        assert_eq!(summary.usage_limit_reset_at, None);
        let empty = thread_error_summary(None, None);
        assert_eq!(
            empty,
            ThreadErrorSummary {
                usage_limit_reset_at: None,
                last_error: None,
                last_error_class: None,
            }
        );
    }
}
