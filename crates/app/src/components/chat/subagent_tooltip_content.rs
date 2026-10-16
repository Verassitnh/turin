//! Port of upstream/apps/web/src/components/chat/SubagentTooltipContent.tsx
//!
//! Status line copy and tone for subagent hover cards (lineage and timeline).

use crate::state::contracts::OrchestrationV2TurnItemStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentTooltipStatusTone {
    Info,
    Error,
    Success,
    Muted,
}

pub fn subagent_tooltip_status_tone(
    status: OrchestrationV2TurnItemStatus,
) -> SubagentTooltipStatusTone {
    if matches!(
        status,
        OrchestrationV2TurnItemStatus::Running
            | OrchestrationV2TurnItemStatus::Pending
            | OrchestrationV2TurnItemStatus::Waiting
    ) {
        return SubagentTooltipStatusTone::Info;
    }
    if status == OrchestrationV2TurnItemStatus::Failed {
        return SubagentTooltipStatusTone::Error;
    }
    if status == OrchestrationV2TurnItemStatus::Completed {
        return SubagentTooltipStatusTone::Success;
    }
    SubagentTooltipStatusTone::Muted
}

pub fn subagent_tooltip_status_label(status: OrchestrationV2TurnItemStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
        .replace('_', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_active_statuses_to_info_tone() {
        assert_eq!(
            subagent_tooltip_status_tone(OrchestrationV2TurnItemStatus::Running),
            SubagentTooltipStatusTone::Info
        );
    }

    #[test]
    fn humanizes_status_labels() {
        assert_eq!(
            subagent_tooltip_status_label(OrchestrationV2TurnItemStatus::Running),
            "running"
        );
    }
}
