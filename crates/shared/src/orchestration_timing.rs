//! Port of upstream/packages/shared/src/orchestrationTiming.ts

use chrono::DateTime;
use turin_contracts::orchestration_v2::OrchestrationV2SubagentStatus;

pub struct SubagentTiming<'a> {
    pub status: OrchestrationV2SubagentStatus,
    pub started_at: Option<&'a str>,
    pub completed_at: Option<&'a str>,
}

fn epoch_millis(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.timestamp_millis())
}

pub fn derive_subagent_elapsed_ms(agent: SubagentTiming<'_>, now_ms: i64) -> Option<i64> {
    let start = epoch_millis(agent.started_at?)?;
    let active = matches!(
        agent.status,
        OrchestrationV2SubagentStatus::Pending
            | OrchestrationV2SubagentStatus::Running
            | OrchestrationV2SubagentStatus::Waiting
    );
    let end = if active {
        now_ms
    } else {
        epoch_millis(agent.completed_at?)?
    };
    Some((end - start).max(0))
}

pub fn format_duration(duration_ms: f64) -> String {
    if !duration_ms.is_finite() || duration_ms < 0.0 {
        return "0ms".into();
    }
    if duration_ms < 1_000.0 {
        return format!("{}ms", duration_ms.round().max(1.0) as i64);
    }
    if duration_ms < 10_000.0 {
        let tenths = (duration_ms / 100.0).round() / 10.0;
        return if tenths >= 10.0 {
            "10s".into()
        } else {
            format!("{tenths:.1}s")
        };
    }
    if duration_ms < 60_000.0 {
        return format!("{}s", (duration_ms / 1_000.0).round() as i64);
    }
    let total_seconds = (duration_ms / 1_000.0).round() as i64;
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    let mut parts = Vec::new();
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 {
        parts.push(format!("{seconds}s"));
    }
    parts.join(" ")
}

pub struct LatestRunTiming<'a> {
    pub run_id: Option<&'a str>,
    pub requested_at: Option<&'a str>,
    pub started_at: Option<&'a str>,
    pub completed_at: Option<&'a str>,
}

pub struct RuntimeActivityState<'a> {
    pub orchestration_status: &'a str,
    pub active_run_id: Option<&'a str>,
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
    if let Some(active_id) = runtime
        .and_then(|state| state.active_run_id)
        .filter(|id| !id.is_empty())
        && Some(active_id) != latest_run.and_then(|run| run.run_id)
    {
        return send_started_at;
    }
    if !is_latest_run_settled(latest_run, runtime) {
        return latest_run
            .and_then(|run| run.started_at.or(run.requested_at))
            .or(send_started_at);
    }
    send_started_at
}

fn is_latest_run_settled(
    latest_run: Option<&LatestRunTiming<'_>>,
    runtime: Option<&RuntimeActivityState<'_>>,
) -> bool {
    let Some(latest_run) = latest_run else {
        return false;
    };
    if latest_run.completed_at.is_none_or(str::is_empty) {
        return false;
    }
    runtime.is_none_or(|state| state.orchestration_status != "running")
}

#[cfg(test)]
mod tests {
    use super::*;
    use OrchestrationV2SubagentStatus as Status;

    #[test]
    fn formats_all_upstream_duration_cases() {
        let cases = [
            (0.0, "1ms"),
            (250.0, "250ms"),
            (1500.0, "1.5s"),
            (9950.0, "10s"),
            (22000.0, "22s"),
            (60000.0, "1m"),
            (65000.0, "1m 5s"),
            (119500.0, "2m"),
            (3599499.0, "59m 59s"),
            (3599500.0, "1h"),
            (3600000.0, "1h"),
            (3601000.0, "1h 1s"),
            (3660000.0, "1h 1m"),
            (3661000.0, "1h 1m 1s"),
            (7199500.0, "2h"),
            (25190000.0, "6h 59m 50s"),
            (90061000.0, "25h 1m 1s"),
        ];
        for (value, expected) in cases {
            assert_eq!(format_duration(value), expected);
        }
    }

    #[test]
    fn handles_invalid_durations() {
        for value in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(format_duration(value), "0ms");
        }
    }

    fn run<'a>(
        requested_at: &'a str,
        started_at: Option<&'a str>,
        completed_at: Option<&'a str>,
    ) -> LatestRunTiming<'a> {
        LatestRunTiming {
            run_id: Some("turn-1"),
            requested_at: Some(requested_at),
            started_at,
            completed_at,
        }
    }

    fn runtime<'a>(status: &'a str, active_run_id: Option<&'a str>) -> RuntimeActivityState<'a> {
        RuntimeActivityState {
            orchestration_status: status,
            active_run_id,
        }
    }

    const REQUESTED: &str = "2026-09-06T23:33:00.000Z";
    const STARTED: &str = "2026-09-06T23:33:05.000Z";

    #[test]
    fn does_not_time_a_superseded_turn_when_the_active_turn_differs() {
        for send_started_at in [None, Some("2026-09-06T23:34:00.000Z")] {
            let latest = LatestRunTiming {
                run_id: Some("old"),
                requested_at: Some(REQUESTED),
                started_at: None,
                completed_at: None,
            };
            assert_eq!(
                derive_active_work_started_at(
                    Some(&latest),
                    Some(&runtime("running", Some("new"))),
                    send_started_at,
                ),
                send_started_at
            );
        }
    }

    #[test]
    fn stops_timing_a_turn_that_failed_before_its_provider_started() {
        let latest = run(REQUESTED, None, Some(STARTED));
        assert_eq!(
            derive_active_work_started_at(Some(&latest), Some(&runtime("error", None)), None),
            None
        );
    }

    #[test]
    fn counts_from_requested_at_while_the_provider_is_still_starting() {
        let latest = run(REQUESTED, None, None);
        assert_eq!(
            derive_active_work_started_at(Some(&latest), Some(&runtime("starting", None)), None),
            Some(REQUESTED)
        );
    }

    #[test]
    fn prefers_the_turns_own_started_at_once_the_provider_reports_it() {
        let latest = run(REQUESTED, Some(STARTED), None);
        assert_eq!(
            derive_active_work_started_at(
                Some(&latest),
                Some(&runtime("running", Some("turn-1"))),
                None
            ),
            Some(STARTED)
        );
    }

    #[test]
    fn stops_counting_once_the_turn_has_settled() {
        let latest = run(REQUESTED, Some(STARTED), Some("2026-09-06T23:33:09.000Z"));
        assert_eq!(
            derive_active_work_started_at(Some(&latest), Some(&runtime("idle", None)), None),
            None
        );
    }

    #[test]
    fn does_not_count_a_settled_turn_while_a_session_is_starting_again() {
        let latest = run(REQUESTED, Some(STARTED), Some("2026-09-06T23:33:09.000Z"));
        assert_eq!(
            derive_active_work_started_at(Some(&latest), Some(&runtime("starting", None)), None),
            None
        );
    }

    #[test]
    fn falls_back_to_the_callers_send_timestamp_when_there_is_no_turn_yet() {
        assert_eq!(
            derive_active_work_started_at(None, None, Some(REQUESTED)),
            Some(REQUESTED)
        );
    }

    const SUB_STARTED: &str = "2026-09-21T12:00:00.000Z";
    const SUB_COMPLETED: &str = "2026-09-21T12:00:10.000Z";

    fn now() -> i64 {
        epoch_millis("2026-09-21T13:00:00.000Z").unwrap()
    }

    #[test]
    fn does_not_count_the_age_of_settled_work_with_unknown_completion_timing() {
        for status in [
            Status::Idle,
            Status::Completed,
            Status::Failed,
            Status::Cancelled,
            Status::Interrupted,
        ] {
            let unknown = SubagentTiming {
                status,
                started_at: Some(SUB_STARTED),
                completed_at: None,
            };
            assert_eq!(derive_subagent_elapsed_ms(unknown, now()), None);
            let known = SubagentTiming {
                status,
                started_at: Some(SUB_STARTED),
                completed_at: Some(SUB_COMPLETED),
            };
            assert_eq!(derive_subagent_elapsed_ms(known, now()), Some(10_000));
        }
    }

    #[test]
    fn counts_a_resumed_activation_despite_a_stale_previous_completion_timestamp() {
        let resumed = SubagentTiming {
            status: Status::Running,
            started_at: Some(SUB_STARTED),
            completed_at: Some(SUB_COMPLETED),
        };
        assert_eq!(derive_subagent_elapsed_ms(resumed, now()), Some(3_600_000));
        let unstarted = SubagentTiming {
            status: Status::Running,
            started_at: None,
            completed_at: None,
        };
        assert_eq!(derive_subagent_elapsed_ms(unstarted, now()), None);
    }
}
