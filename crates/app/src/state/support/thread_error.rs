//! Port of upstream/packages/shared/src/orchestrationV2ThreadError.ts

use std::sync::Arc;

use crate::state::contracts::{
    IsoDateTime, OrchestrationV2ProviderFailure, OrchestrationV2ProviderFailureClass,
    OrchestrationV2Run, OrchestrationV2RunStatus, OrchestrationV2TurnItem,
    OrchestrationV2TurnItemStatus,
};
use crate::state::support::turn_item::TurnItemExt;
use crate::state::time::epoch_millis;

/// Only a failed root turn of the current run owns the thread's failure state.
pub fn latest_root_provider_failure<'a>(
    run: Option<&OrchestrationV2Run>,
    turn_items: &'a [Arc<OrchestrationV2TurnItem>],
) -> Option<&'a OrchestrationV2ProviderFailure> {
    let run = run.filter(|run| run.status == OrchestrationV2RunStatus::Failed)?;
    let mut latest: Option<(&OrchestrationV2TurnItem, &OrchestrationV2ProviderFailure)> = None;
    for item in turn_items {
        let OrchestrationV2TurnItem::Error { failure, .. } = &**item else {
            continue;
        };
        if item.status() != OrchestrationV2TurnItemStatus::Failed
            || item.run_id() != Some(&run.id)
            || item.node_id() != run.root_node_id.as_ref()
        {
            continue;
        }
        let newer = match latest {
            None => true,
            Some((current, _)) => {
                let (item_ms, current_ms) = (
                    epoch_millis(item.updated_at()),
                    epoch_millis(current.updated_at()),
                );
                item_ms > current_ms
                    || (item_ms == current_ms
                        && (item.ordinal() > current.ordinal()
                            || (item.ordinal() == current.ordinal()
                                && item.id().as_str() > current.id().as_str())))
            }
        };
        if newer {
            latest = Some((&**item, failure));
        }
    }
    latest.map(|(_, failure)| failure)
}

#[derive(Clone, Debug, PartialEq, Eq)]
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

pub fn latest_executed_run(runs: &[Arc<OrchestrationV2Run>]) -> Option<&Arc<OrchestrationV2Run>> {
    let mut latest: Option<&Arc<OrchestrationV2Run>> = None;
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

/// Whether started `run` ran after `other`. Ordinals follow submission, but a
/// run can start ahead of a held queue (a restart continuation, or a message
/// sent while the queue is held), so a queued run resumed later can have a
/// lower ordinal than one that already ended. An unfinished run is the latest.
pub fn run_ran_after(run: &OrchestrationV2Run, other: &OrchestrationV2Run) -> bool {
    let end = |candidate: &OrchestrationV2Run| match &candidate.completed_at {
        None => f64::INFINITY,
        Some(completed_at) => epoch_millis(completed_at) as f64,
    };
    if end(run) == end(other) {
        run.ordinal > other.ordinal
    } else {
        end(run) > end(other)
    }
}

/// The latest run that actually started, when it stopped because the
/// subscription limit was reached. Queued messages after that run must stay
/// queued instead of being sent into the same limit.
pub fn usage_limit_blocked_run<'a>(
    runs: &'a [Arc<OrchestrationV2Run>],
    turn_items: &[Arc<OrchestrationV2TurnItem>],
    session_error: Option<&str>,
) -> Option<&'a Arc<OrchestrationV2Run>> {
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
    runs: &'a [Arc<OrchestrationV2Run>],
    turn_items: &[Arc<OrchestrationV2TurnItem>],
    session_error: Option<&str>,
) -> Option<&'a Arc<OrchestrationV2Run>> {
    let blocked = usage_limit_blocked_run(runs, turn_items, session_error)?;
    runs.iter()
        .any(|run| run.ordinal > blocked.ordinal)
        .then_some(blocked)
}

/// The newest run that is not waiting in a held queue, or `None` when only held
/// runs exist. A held queue waits for the user, so its runs never stand for the
/// thread's outcome. The SQL thread shell selects the same run.
pub fn latest_unheld_run(runs: &[Arc<OrchestrationV2Run>]) -> Option<&Arc<OrchestrationV2Run>> {
    let mut latest: Option<&Arc<OrchestrationV2Run>> = None;
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
