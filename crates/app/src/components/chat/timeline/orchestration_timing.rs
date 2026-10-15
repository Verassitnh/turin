//! Port of upstream/packages/shared/src/orchestrationTiming.ts
//!
//! `OrchestrationV2Subagent["status"]` has the same literals as
//! `OrchestrationV2TurnItemStatus`, so the subagent input reuses
//! [`TurnItemStatus`] from the timeline mirror types. The other inputs are
//! upstream's local `LatestRunTiming` / `RuntimeActivityState` shapes,
//! borrowed so callers can pass fields of their own models without cloning.

use chrono::DateTime;

use super::turn_item::TurnItemStatus;

/// `isOrchestrationV2WorkActive` from upstream/packages/contracts/src/orchestrationV2.ts.
///
/// Idle work is resumable, but does not keep a turn or its subscription alive.
pub fn is_orchestration_v2_work_active(status: TurnItemStatus) -> bool {
    matches!(
        status,
        TurnItemStatus::Pending | TurnItemStatus::Running | TurnItemStatus::Waiting
    )
}

/// The fields of `OrchestrationV2Subagent` that [`derive_subagent_elapsed_ms`] reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubagentTiming<'a> {
    pub status: TurnItemStatus,
    pub started_at: Option<&'a str>,
    pub completed_at: Option<&'a str>,
}

/// JS `Date.parse` for the ISO timestamps the contracts carry; `NaN` is `None`.
fn parse_iso_ms(value: &str) -> Option<f64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.timestamp_millis() as f64)
}

/// Unknown settled timing must not turn a task's age into its work duration.
pub fn derive_subagent_elapsed_ms(agent: SubagentTiming<'_>, now_ms: f64) -> Option<f64> {
    let started_at = agent.started_at?;
    let end = if is_orchestration_v2_work_active(agent.status) {
        Some(now_ms)
    } else {
        // `Date.parse` of an unparseable completion is NaN, which fails the
        // finiteness check below; `None` covers both.
        parse_iso_ms(agent.completed_at?)
    };
    let start = parse_iso_ms(started_at);
    match (start, end) {
        (Some(start), Some(end)) if start.is_finite() && end.is_finite() => {
            Some(0_f64.max(end - start))
        }
        _ => None,
    }
}

/// Upstream's `LatestRunTiming`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LatestRunTiming<'a> {
    pub run_id: Option<&'a str>,
    /// Set when the turn is created; `startedAt` waits for the provider.
    pub requested_at: Option<&'a str>,
    pub started_at: Option<&'a str>,
    pub completed_at: Option<&'a str>,
}

/// Upstream's `RuntimeActivityState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeActivityState<'a> {
    pub orchestration_status: &'a str,
    pub active_run_id: Option<&'a str>,
}

pub fn format_duration(duration_ms: f64) -> String {
    if !duration_ms.is_finite() || duration_ms < 0.0 {
        return "0ms".to_string();
    }
    // `duration_ms` is non-negative from here on, where `f64::round` (half away
    // from zero) agrees with JS `Math.round` (half towards +infinity).
    if duration_ms < 1_000.0 {
        return format!("{}ms", 1_f64.max(duration_ms.round()));
    }
    if duration_ms < 10_000.0 {
        let tenths = (duration_ms / 100.0).round() / 10.0;
        return if tenths >= 10.0 {
            "10s".to_string()
        } else {
            format!("{tenths:.1}s")
        };
    }
    if duration_ms < 60_000.0 {
        return format!("{}s", (duration_ms / 1_000.0).round());
    }
    let total_seconds = (duration_ms / 1_000.0).round();
    let hours = (total_seconds / 3_600.0).floor();
    let minutes = ((total_seconds % 3_600.0) / 60.0).floor();
    let seconds = total_seconds % 60.0;
    let mut parts: Vec<String> = Vec::new();
    if hours > 0.0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0.0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0.0 {
        parts.push(format!("{seconds}s"));
    }
    parts.join(" ")
}

fn is_latest_run_settled(
    latest_run: Option<&LatestRunTiming<'_>>,
    runtime: Option<&RuntimeActivityState<'_>>,
) -> bool {
    let Some(latest_run) = latest_run else {
        return false;
    };
    // `!latestRun.completedAt`: an empty string is falsy too.
    if latest_run.completed_at.is_none_or(str::is_empty) {
        return false;
    }
    let Some(runtime) = runtime else {
        return true;
    };
    if runtime.orchestration_status == "running" {
        return false;
    }
    true
}

/// When the working indicator should be counting, and from when.
///
/// `requestedAt` is the floor for an unsettled turn. The projector only stamps
/// `startedAt` in the same update that moves the session to "running", so while
/// the provider spins up (session "starting") a requested turn has no
/// `startedAt` at all — and returning null there blinks the indicator out for
/// the whole spin-up. A settled turn still falls through to `sendStartedAt`, so
/// this cannot leave the indicator counting after the work is done.
pub fn derive_active_work_started_at<'a>(
    latest_run: Option<&LatestRunTiming<'a>>,
    runtime: Option<&RuntimeActivityState<'a>>,
    send_started_at: Option<&'a str>,
) -> Option<&'a str> {
    // `runtime?.activeRunId && runtime.activeRunId !== latestRun?.runId`
    if let Some(active_run_id) = runtime
        .and_then(|runtime| runtime.active_run_id)
        .filter(|id| !id.is_empty())
        && latest_run.and_then(|run| run.run_id) != Some(active_run_id)
    {
        return send_started_at;
    }
    if !is_latest_run_settled(latest_run, runtime) {
        return latest_run
            .and_then(|run| run.started_at)
            .or_else(|| latest_run.and_then(|run| run.requested_at))
            .or(send_started_at);
    }
    send_started_at
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn format_duration_formats_durations() {
        for (duration_ms, expected) in [
            (0.0, "1ms"),
            (250.0, "250ms"),
            (1_500.0, "1.5s"),
            (9_950.0, "10s"),
            (22_000.0, "22s"),
            (60_000.0, "1m"),
            (65_000.0, "1m 5s"),
            (119_500.0, "2m"),
            (3_599_499.0, "59m 59s"),
            (3_599_500.0, "1h"),
            (3_600_000.0, "1h"),
            (3_601_000.0, "1h 1s"),
            (3_660_000.0, "1h 1m"),
            (3_661_000.0, "1h 1m 1s"),
            (7_199_500.0, "2h"),
            (25_190_000.0, "6h 59m 50s"),
            (90_061_000.0, "25h 1m 1s"),
        ] {
            assert_eq!(format_duration(duration_ms), expected, "{duration_ms}");
        }
    }

    #[test]
    fn format_duration_handles_invalid_durations() {
        for duration_ms in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(format_duration(duration_ms), "0ms", "{duration_ms}");
        }
    }

    #[test]
    fn does_not_time_a_superseded_turn_when_the_active_turn_differs() {
        for send_started_at in [None, Some("2026-09-06T23:34:00.000Z")] {
            assert_eq!(
                derive_active_work_started_at(
                    Some(&LatestRunTiming {
                        run_id: Some("old"),
                        requested_at: Some("2026-09-06T23:33:00.000Z"),
                        started_at: None,
                        completed_at: None,
                    }),
                    Some(&RuntimeActivityState {
                        orchestration_status: "running",
                        active_run_id: Some("new"),
                    }),
                    send_started_at,
                ),
                send_started_at
            );
        }
    }

    #[test]
    fn stops_timing_a_turn_that_failed_before_its_provider_started() {
        assert_eq!(
            derive_active_work_started_at(
                Some(&LatestRunTiming {
                    run_id: Some("turn-1"),
                    requested_at: Some("2026-09-06T23:33:00.000Z"),
                    started_at: None,
                    completed_at: Some("2026-09-06T23:33:05.000Z"),
                }),
                Some(&RuntimeActivityState {
                    orchestration_status: "error",
                    active_run_id: None,
                }),
                None,
            ),
            None
        );
    }

    // The gap this closes. The projector stamps startedAt in the same update
    // that moves the session to "running", so during provider spin-up the turn
    // is requested with no startedAt and the session is "starting". Returning
    // null there blinks the working indicator out between "Setting up
    // worktree..." and "Working for 0s".
    #[test]
    fn counts_from_requested_at_while_the_provider_is_still_starting() {
        assert_eq!(
            derive_active_work_started_at(
                Some(&LatestRunTiming {
                    run_id: Some("turn-1"),
                    requested_at: Some("2026-09-06T23:33:00.000Z"),
                    started_at: None,
                    completed_at: None,
                }),
                Some(&RuntimeActivityState {
                    orchestration_status: "starting",
                    active_run_id: None,
                }),
                None,
            ),
            Some("2026-09-06T23:33:00.000Z")
        );
    }

    #[test]
    fn prefers_the_turns_own_started_at_once_the_provider_reports_it() {
        assert_eq!(
            derive_active_work_started_at(
                Some(&LatestRunTiming {
                    run_id: Some("turn-1"),
                    requested_at: Some("2026-09-06T23:33:00.000Z"),
                    started_at: Some("2026-09-06T23:33:05.000Z"),
                    completed_at: None,
                }),
                Some(&RuntimeActivityState {
                    orchestration_status: "running",
                    active_run_id: Some("turn-1"),
                }),
                None,
            ),
            Some("2026-09-06T23:33:05.000Z")
        );
    }

    // requestedAt must not leak past the end of the work.
    #[test]
    fn stops_counting_once_the_turn_has_settled() {
        assert_eq!(
            derive_active_work_started_at(
                Some(&LatestRunTiming {
                    run_id: Some("turn-1"),
                    requested_at: Some("2026-09-06T23:33:00.000Z"),
                    started_at: Some("2026-09-06T23:33:05.000Z"),
                    completed_at: Some("2026-09-06T23:33:09.000Z"),
                }),
                Some(&RuntimeActivityState {
                    orchestration_status: "idle",
                    active_run_id: None,
                }),
                None,
            ),
            None
        );
    }

    // A session restarting with no new turn must not resurrect the old one.
    #[test]
    fn does_not_count_a_settled_turn_while_a_session_is_starting_again() {
        assert_eq!(
            derive_active_work_started_at(
                Some(&LatestRunTiming {
                    run_id: Some("turn-1"),
                    requested_at: Some("2026-09-06T23:33:00.000Z"),
                    started_at: Some("2026-09-06T23:33:05.000Z"),
                    completed_at: Some("2026-09-06T23:33:09.000Z"),
                }),
                Some(&RuntimeActivityState {
                    orchestration_status: "starting",
                    active_run_id: None,
                }),
                None,
            ),
            None
        );
    }

    #[test]
    fn falls_back_to_the_callers_send_timestamp_when_there_is_no_turn_yet() {
        assert_eq!(
            derive_active_work_started_at(None, None, Some("2026-09-06T23:33:00.000Z")),
            Some("2026-09-06T23:33:00.000Z")
        );
    }

    const STARTED_AT: &str = "2026-09-21T12:00:00.000Z";
    const COMPLETED_AT: &str = "2026-09-21T12:00:10.000Z";

    fn now() -> f64 {
        parse_iso_ms("2026-09-21T13:00:00.000Z").expect("valid timestamp")
    }

    #[test]
    fn does_not_count_the_age_of_settled_work_with_unknown_completion_timing() {
        for status in [
            TurnItemStatus::Idle,
            TurnItemStatus::Completed,
            TurnItemStatus::Failed,
            TurnItemStatus::Cancelled,
            TurnItemStatus::Interrupted,
        ] {
            assert_eq!(
                derive_subagent_elapsed_ms(
                    SubagentTiming {
                        status,
                        started_at: Some(STARTED_AT),
                        completed_at: None,
                    },
                    now(),
                ),
                None,
                "{status:?}"
            );
            assert_eq!(
                derive_subagent_elapsed_ms(
                    SubagentTiming {
                        status,
                        started_at: Some(STARTED_AT),
                        completed_at: Some(COMPLETED_AT),
                    },
                    now(),
                ),
                Some(10_000.0),
                "{status:?}"
            );
        }
    }

    #[test]
    fn counts_a_resumed_activation_despite_a_stale_previous_completion_timestamp() {
        assert_eq!(
            derive_subagent_elapsed_ms(
                SubagentTiming {
                    status: TurnItemStatus::Running,
                    started_at: Some(STARTED_AT),
                    completed_at: Some(COMPLETED_AT),
                },
                now(),
            ),
            Some(3_600_000.0)
        );
        assert_eq!(
            derive_subagent_elapsed_ms(
                SubagentTiming {
                    status: TurnItemStatus::Running,
                    started_at: None,
                    completed_at: None,
                },
                now(),
            ),
            None
        );
    }
}
