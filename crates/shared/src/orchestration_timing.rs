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

pub fn derive_active_work_started_at<'a>(
    latest_run: Option<&LatestRunTiming<'a>>,
    runtime: Option<&RuntimeActivityState<'a>>,
    send_started_at: Option<&'a str>,
) -> Option<&'a str> {
    if runtime
        .and_then(|state| state.active_run_id)
        .is_some_and(|active_id| active_id != latest_run.and_then(|run| run.run_id).unwrap_or(""))
    {
        return send_started_at;
    }
    let settled = latest_run.is_some_and(|run| run.completed_at.is_some())
        && runtime.is_none_or(|state| state.orchestration_status != "running");
    if !settled {
        return latest_run
            .and_then(|run| run.started_at.or(run.requested_at))
            .or(send_started_at);
    }
    send_started_at
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for value in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(format_duration(value), "0ms");
        }
    }

    #[test]
    fn times_pending_and_settled_work() {
        let run = LatestRunTiming {
            run_id: Some("turn-1"),
            requested_at: Some("requested"),
            started_at: None,
            completed_at: None,
        };
        let starting = RuntimeActivityState {
            orchestration_status: "starting",
            active_run_id: None,
        };
        assert_eq!(
            derive_active_work_started_at(Some(&run), Some(&starting), None),
            Some("requested")
        );
        let other = RuntimeActivityState {
            orchestration_status: "running",
            active_run_id: Some("turn-2"),
        };
        assert_eq!(
            derive_active_work_started_at(Some(&run), Some(&other), Some("sent")),
            Some("sent")
        );
        let settled = LatestRunTiming {
            completed_at: Some("done"),
            ..run
        };
        assert_eq!(
            derive_active_work_started_at(Some(&settled), Some(&starting), None),
            None
        );
    }

    #[test]
    fn settled_subagent_needs_completion_time() {
        let agent = SubagentTiming {
            status: OrchestrationV2SubagentStatus::Completed,
            started_at: Some("2026-09-21T12:00:00.000Z"),
            completed_at: None,
        };
        assert_eq!(derive_subagent_elapsed_ms(agent, 0), None);
        let agent = SubagentTiming {
            status: OrchestrationV2SubagentStatus::Running,
            started_at: Some("2026-09-21T12:00:00.000Z"),
            completed_at: Some("2026-09-21T12:00:10.000Z"),
        };
        let now = epoch_millis("2026-09-21T13:00:00.000Z").unwrap();
        assert_eq!(derive_subagent_elapsed_ms(agent, now), Some(3_600_000));
    }
}
