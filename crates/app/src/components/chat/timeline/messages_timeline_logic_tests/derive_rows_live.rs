//! `deriveMessagesTimelineRows` cases about the live work row: the active
//! response after a restart, the latest thought carried beside a running tool,
//! and the single shared activity row.

use super::*;

/// A work entry whose id, entry id and timeline id follow upstream's
/// `work(id, at, fields)` helper (`${id}-entry` for the timeline id).
fn work(id: &str, at: &str, fields: impl FnOnce(WorkLogEntry) -> WorkLogEntry) -> TimelineEntry {
    work_entry(
        &format!("{id}-entry"),
        at,
        fields(work_log(id, at, Some("turn-1"), id, WorkTone::Tool)),
    )
}

#[test]
fn keeps_a_promptless_restart_in_one_active_visual_response() {
    let rows = derive(&MessagesTimelineRowsInput {
        latest_run: Some(latest_run(
            "turn-after-restart",
            RunStatus::Running,
            Some("2026-01-01T00:01:00Z"),
            None,
        )),
        active_turn_started_at: Some("2026-01-01T00:01:00Z".into()),
        ..rows_input(
            vec![
                message_entry(
                    "user-entry",
                    "2026-01-01T00:00:00Z",
                    user_message("user-1", "keep going", None, "2026-01-01T00:00:00Z"),
                ),
                work_entry(
                    "old-work-entry",
                    "2026-01-01T00:00:05Z",
                    WorkLogEntry {
                        command: Some("rg restart".into()),
                        tool_lifecycle_status: Some(ToolLifecycleStatus::Completed),
                        ..work_log(
                            "old-work",
                            "2026-01-01T00:00:05Z",
                            Some("turn-before-restart"),
                            "Searched files",
                            WorkTone::Tool,
                        )
                    },
                ),
                work_entry(
                    "old-stale-work-entry",
                    "2026-01-01T00:00:06Z",
                    WorkLogEntry {
                        command: Some("rg stale".into()),
                        tool_lifecycle_status: Some(ToolLifecycleStatus::InProgress),
                        ..work_log(
                            "old-stale-work",
                            "2026-01-01T00:00:06Z",
                            Some("turn-before-restart"),
                            "Running stale command",
                            WorkTone::Tool,
                        )
                    },
                ),
                message_entry(
                    "old-commentary-entry",
                    "2026-01-01T00:00:08Z",
                    assistant_message(
                        "old-commentary",
                        "the server restarted, continuing here.",
                        Some("turn-before-restart"),
                        "2026-01-01T00:00:08Z",
                        "2026-01-01T00:00:08Z",
                        false,
                    ),
                ),
                work_entry(
                    "new-work-entry",
                    "2026-01-01T00:01:05Z",
                    WorkLogEntry {
                        command: Some("vp test run".into()),
                        tool_lifecycle_status: Some(ToolLifecycleStatus::InProgress),
                        ..work_log(
                            "new-work",
                            "2026-01-01T00:01:05Z",
                            Some("turn-after-restart"),
                            "Running tests",
                            WorkTone::Tool,
                        )
                    },
                ),
            ],
            true,
        )
    });

    assert!(!has_kind(&rows, MessagesTimelineRowKind::TurnFold));
    let working: Vec<_> = rows
        .iter()
        .filter(|row| row.id() == "working-indicator-row")
        .collect();
    assert_eq!(working.len(), 1);
    assert_eq!(
        working[0].content.created_at(),
        Some("2026-01-01T00:01:00Z")
    );
    let position = |id: &str| rows.iter().position(|row| row.id() == id);
    assert!(position("working-indicator-row") < position("old-work-entry"));

    match find_id(&rows, "old-commentary-entry") {
        Some(Row::Message {
            show_assistant_meta,
            show_assistant_copy_button,
            assistant_copy_streaming,
            ..
        }) => {
            assert!(!show_assistant_meta);
            assert!(!show_assistant_copy_button);
            assert!(assistant_copy_streaming);
        }
        other => panic!("expected the old commentary message, got {other:?}"),
    }

    let active_live: Vec<_> = live_rows(&rows)
        .into_iter()
        .filter(|row| matches!(row, Row::WorkLive { active: true, .. }))
        .collect();
    assert_eq!(active_live.len(), 1);
    assert_eq!(live_entry_id(active_live[0]), Some("new-work"));
    assert!(!has_kind(&rows, MessagesTimelineRowKind::Thinking));
}

#[test]
fn carries_the_latest_thought_on_the_live_row_while_a_later_tool_runs() {
    let rows = derive(&MessagesTimelineRowsInput {
        latest_run: Some(latest_run(
            "turn-1",
            RunStatus::Running,
            Some("2026-01-01T00:00:00Z"),
            None,
        )),
        active_turn_started_at: Some("2026-01-01T00:00:00Z".into()),
        ..rows_input(
            vec![
                work("old-thought", "2026-01-01T00:00:01Z", |entry| {
                    WorkLogEntry {
                        tone: WorkTone::Thinking,
                        item_type: Some(TurnItemType::Reasoning),
                        detail: Some("First idea.".into()),
                        tool_lifecycle_status: Some(ToolLifecycleStatus::Completed),
                        ..entry
                    }
                }),
                work("new-thought", "2026-01-01T00:00:02Z", |entry| {
                    WorkLogEntry {
                        tone: WorkTone::Thinking,
                        item_type: Some(TurnItemType::Reasoning),
                        detail: Some("Found the cause.".into()),
                        tool_lifecycle_status: Some(ToolLifecycleStatus::Completed),
                        ..entry
                    }
                }),
                work("running-command", "2026-01-01T00:00:03Z", |entry| {
                    WorkLogEntry {
                        command: Some("rg cause".into()),
                        request_kind: Some("command".into()),
                        tool_lifecycle_status: Some(ToolLifecycleStatus::InProgress),
                        ..entry
                    }
                }),
            ],
            true,
        )
    });

    match find_kind(&rows, MessagesTimelineRowKind::WorkLive) {
        Some(Row::WorkLive { entry, thought, .. }) => {
            assert_eq!(entry.id, "running-command");
            assert_eq!(
                thought.as_ref().map(|thought| thought.id.as_str()),
                Some("new-thought")
            );
        }
        other => panic!("expected a live work row, got {other:?}"),
    }
}

#[test]
fn keeps_an_actually_running_tool_in_the_shared_activity_row() {
    let rows = derive(&MessagesTimelineRowsInput {
        latest_run: Some(latest_run(
            "turn-1",
            RunStatus::Running,
            Some("2026-01-01T00:00:00Z"),
            None,
        )),
        active_turn_started_at: Some("2026-01-01T00:00:00Z".into()),
        ..rows_input(
            vec![
                work_entry(
                    "running-command-entry",
                    "2026-01-01T00:00:05Z",
                    WorkLogEntry {
                        command: Some("rg toolCall".into()),
                        request_kind: Some("command".into()),
                        tool_lifecycle_status: Some(ToolLifecycleStatus::InProgress),
                        ..work_log(
                            "running-command",
                            "2026-01-01T00:00:05Z",
                            Some("turn-1"),
                            "Running rg",
                            WorkTone::Tool,
                        )
                    },
                ),
                work_entry(
                    "completed-edit-entry",
                    "2026-01-01T00:00:06Z",
                    WorkLogEntry {
                        request_kind: Some("file-change".into()),
                        changed_files: Some(vec!["src/one.ts".into(), "src/two.ts".into()]),
                        tool_lifecycle_status: Some(ToolLifecycleStatus::Completed),
                        ..work_log(
                            "completed-edit",
                            "2026-01-01T00:00:06Z",
                            Some("turn-1"),
                            "Edited files",
                            WorkTone::Tool,
                        )
                    },
                ),
                work_entry(
                    "completed-command-entry",
                    "2026-01-01T00:00:07Z",
                    WorkLogEntry {
                        command: Some("vp test run".into()),
                        request_kind: Some("command".into()),
                        tool_lifecycle_status: Some(ToolLifecycleStatus::Completed),
                        ..work_log(
                            "completed-command",
                            "2026-01-01T00:00:07Z",
                            Some("turn-1"),
                            "Ran tests",
                            WorkTone::Tool,
                        )
                    },
                ),
            ],
            true,
        )
    });

    assert_eq!(kinds(&rows), vec!["working", "work-live"]);
    assert!(!has_kind(&rows, MessagesTimelineRowKind::Thinking));
    match find_kind(&rows, MessagesTimelineRowKind::WorkLive) {
        Some(Row::WorkLive {
            entry,
            active,
            grouped_entries,
            ..
        }) => {
            assert_eq!(entry.id, "running-command");
            assert!(active);
            assert_eq!(
                grouped_entries
                    .iter()
                    .map(|entry| entry.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["running-command", "completed-edit", "completed-command"]
            );
        }
        other => panic!("expected a live work row, got {other:?}"),
    }
}
