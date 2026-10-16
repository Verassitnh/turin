//! Port of upstream/apps/web/src/components/chat/AgentElapsed.tsx

use gpui_kit::{App, IntoElement, ParentElement as _, RenderOnce, Window, div};

use super::timeline::orchestration_timing::{SubagentTiming, derive_subagent_elapsed_ms};
use super::timeline::turn_item::TurnItemStatus;
use super::timeline_work_rows::format_elapsed_seconds;
use crate::state::subagent_runtime::RuntimeSubagentStatus;

/// Short form for tight rows: "45s", "12m", "1.5h", "14h". Rounds down.
pub fn format_compact_elapsed_seconds(total_seconds: f64) -> String {
    let seconds = total_seconds.max(0.).floor() as u64;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    if minutes >= 600 {
        return format!("{}h", minutes / 60);
    }
    let hours = (minutes / 6) as f64 / 10.;
    let label = format!("{hours:.1}").trim_end_matches(".0").to_string();
    format!("{label}h")
}

fn runtime_status_to_turn_status(status: RuntimeSubagentStatus) -> TurnItemStatus {
    match status {
        RuntimeSubagentStatus::Pending => TurnItemStatus::Pending,
        RuntimeSubagentStatus::Running => TurnItemStatus::Running,
        RuntimeSubagentStatus::Waiting => TurnItemStatus::Waiting,
        RuntimeSubagentStatus::Idle => TurnItemStatus::Idle,
        RuntimeSubagentStatus::Completed => TurnItemStatus::Completed,
        RuntimeSubagentStatus::Failed => TurnItemStatus::Failed,
        RuntimeSubagentStatus::Cancelled => TurnItemStatus::Cancelled,
        RuntimeSubagentStatus::Interrupted => TurnItemStatus::Interrupted,
    }
}

fn is_live_runtime_status(status: RuntimeSubagentStatus) -> bool {
    matches!(
        status,
        RuntimeSubagentStatus::Pending
            | RuntimeSubagentStatus::Running
            | RuntimeSubagentStatus::Waiting
    )
}

#[derive(IntoElement)]
pub struct AgentElapsed {
    pub status: RuntimeSubagentStatus,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub compact: bool,
}

impl AgentElapsed {
    fn timing(&self) -> SubagentTiming<'_> {
        SubagentTiming {
            status: runtime_status_to_turn_status(self.status),
            started_at: self.started_at.as_deref(),
            completed_at: self.completed_at.as_deref(),
        }
    }

    fn format_seconds(&self, seconds: f64) -> String {
        if self.compact {
            format_compact_elapsed_seconds(seconds)
        } else {
            format_elapsed_seconds(seconds)
        }
    }
}

impl RenderOnce for AgentElapsed {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let timing = self.timing();
        let now_ms = chrono::Utc::now().timestamp_millis() as f64;
        let elapsed_ms = derive_subagent_elapsed_ms(timing, now_ms);
        let Some(elapsed_ms) = elapsed_ms else {
            return div();
        };
        // Live self-tick is owned by the parent list (see `MessagesTimeline` ticker);
        // this element renders the current elapsed label on each parent refresh.
        let _live = is_live_runtime_status(self.status);
        div().child(self.format_seconds(elapsed_ms / 1000.))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_elapsed_labels() {
        for (seconds, label) in [
            (45., "45s"),
            (90., "1m"),
            (59. * 60. + 59., "59m"),
            (60. * 60., "1h"),
            (90. * 60., "1.5h"),
            (119. * 60., "1.9h"),
            (14. * 60. * 60. + 40. * 60., "14h"),
        ] {
            assert_eq!(format_compact_elapsed_seconds(seconds), label, "{seconds}");
        }
    }
}
