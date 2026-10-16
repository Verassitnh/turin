//! Port of upstream/apps/web/src/components/chat/ThreadRelationshipIcon.tsx

/// Human-readable status for lineage and timeline relationship badges.
pub fn thread_relationship_status_label(status: Option<&str>) -> &'static str {
    match status.unwrap_or("") {
        "preparing" | "starting" => "Starting",
        "running" | "in_progress" => "Running",
        "pending" | "queued" => "Queued",
        "waiting" | "blocked" => "Waiting",
        "completed" => "Done",
        "failed" | "error" => "Failed",
        "cancelled" | "interrupted" => "Stopped",
        "rolled_back" => "Reverted",
        "resolved_native" => "Resolved (native)",
        "resolved_portable" => "Resolved (portable)",
        "consumed" => "Consumed",
        "superseded" => "Superseded",
        "idle" => "Idle",
        _ => "Unknown",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadRelationshipBadgeTone {
    Info,
    Destructive,
    Success,
    Muted,
}

pub fn thread_relationship_badge_tone(status: Option<&str>) -> ThreadRelationshipBadgeTone {
    match status.unwrap_or("") {
        "running" | "in_progress" | "pending" | "waiting" => ThreadRelationshipBadgeTone::Info,
        "failed" | "error" => ThreadRelationshipBadgeTone::Destructive,
        "completed" => ThreadRelationshipBadgeTone::Success,
        _ => ThreadRelationshipBadgeTone::Muted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_wire_statuses_to_labels() {
        assert_eq!(thread_relationship_status_label(Some("queued")), "Queued");
        assert_eq!(
            thread_relationship_status_label(Some("resolved_portable")),
            "Resolved (portable)"
        );
        assert_eq!(thread_relationship_status_label(None), "Unknown");
    }

    #[test]
    fn maps_wire_statuses_to_badge_tones() {
        assert_eq!(
            thread_relationship_badge_tone(Some("running")),
            ThreadRelationshipBadgeTone::Info
        );
        assert_eq!(
            thread_relationship_badge_tone(Some("failed")),
            ThreadRelationshipBadgeTone::Destructive
        );
    }
}
