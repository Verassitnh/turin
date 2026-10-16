//! Port of upstream/apps/web/src/components/ChatView.logic.ts (`shouldReleaseTimelineAnchorForToolActivity`).

use crate::components::chat::timeline::types::{
    TimelineEntry, TimelineEntryKind, WorkLogEntry, WorkTone,
};

pub fn should_release_timeline_anchor_for_tool_activity(
    anchor_message_id: Option<&str>,
    live_follow_enabled: bool,
    running_turn_id: Option<&str>,
    timeline_entries: &[TimelineEntry],
) -> bool {
    let (Some(_anchor), true, Some(running_turn_id)) =
        (anchor_message_id, live_follow_enabled, running_turn_id)
    else {
        return false;
    };

    timeline_entries.iter().any(|timeline_entry| {
        let TimelineEntryKind::Work { entry } = &timeline_entry.kind else {
            return false;
        };
        entry.run_id.as_deref() != Some(running_turn_id) || work_entry_has_tool_activity(entry)
    })
}

fn work_entry_has_tool_activity(entry: &WorkLogEntry) -> bool {
    entry.tone == WorkTone::Tool
        || entry.item_type.is_some()
        || entry.request_kind.is_some()
        || entry
            .command
            .as_deref()
            .is_some_and(|command| !command.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::chat::timeline::types::{TimelineEntry, WorkLogEntry};

    fn tool_entry(run_id: &str) -> TimelineEntry {
        TimelineEntry {
            id: "tool-entry".to_owned(),
            created_at: "2026-03-29T00:00:00.000Z".to_owned(),
            kind: TimelineEntryKind::Work {
                entry: WorkLogEntry {
                    question_answer: None,
                    id: "active-tool".to_owned(),
                    created_at: "2026-03-29T00:00:00.000Z".to_owned(),
                    run_id: Some(run_id.to_owned()),
                    label: "Run command".to_owned(),
                    detail: None,
                    command: Some("git status".to_owned()),
                    raw_command: None,
                    changed_files: None,
                    tone: WorkTone::Tool,
                    tool_title: None,
                    tool_call_id: None,
                    viewed_image_path: None,
                    tool_surface: None,
                    tool_icon: None,
                    tool_source: None,
                    source_activity_kind: None,
                    task_id: None,
                    agent_role: None,
                    tool_data: None,
                    request_kind: None,
                    item_type: None,
                    tool_lifecycle_status: None,
                    structured_payload: None,
                    source_item_type: None,
                    projected_item: None,
                },
            },
            attempt: None,
        }
    }

    #[test]
    fn releases_anchor_for_tool_activity_in_the_active_turn() {
        assert!(should_release_timeline_anchor_for_tool_activity(
            Some("anchored-message"),
            true,
            Some("active-turn"),
            &[tool_entry("active-turn")],
        ));
    }

    #[test]
    fn keeps_anchor_while_reading_history() {
        assert!(!should_release_timeline_anchor_for_tool_activity(
            Some("anchored-message"),
            false,
            Some("active-turn"),
            &[tool_entry("active-turn")],
        ));
    }
}
