//! `deriveMessagesTimelineRows` cases about assistant metadata (duration,
//! copy) visibility, and about a `latestRun` that regresses behind the
//! session's running turn.

use super::*;

/// `showAssistantMeta` of every assistant message row, in order.
fn assistant_meta_flags(rows: &[Arc<MessagesTimelineRow>]) -> Vec<bool> {
    rows.iter()
        .filter_map(|row| match &row.content {
            Row::Message {
                message,
                show_assistant_meta,
                ..
            } if message.role == MessageRole::Assistant => Some(*show_assistant_meta),
            _ => None,
        })
        .collect()
}

#[test]
fn does_not_fold_the_session_running_turn_when_latest_run_regresses() {
    let rows = derive(&MessagesTimelineRowsInput {
        latest_run: Some(latest_run(
            "turn-1",
            RunStatus::Completed,
            Some("2026-01-01T00:00:00Z"),
            Some("2026-01-01T00:00:25Z"),
        )),
        running_run_id: Some("turn-2".into()),
        active_turn_started_at: Some("2026-01-01T00:01:00Z".into()),
        ..rows_input(
            vec![
                work_entry(
                    "previous-work-entry",
                    "2026-01-01T00:00:05Z",
                    work_log(
                        "previous-work",
                        "2026-01-01T00:00:05Z",
                        Some("turn-1"),
                        "Read files",
                        WorkTone::Tool,
                    ),
                ),
                message_entry(
                    "user-followup-entry",
                    "2026-01-01T00:01:00Z",
                    user_message("user-followup", "continue", None, "2026-01-01T00:01:00Z"),
                ),
                work_entry(
                    "running-work-entry",
                    "2026-01-01T00:01:05Z",
                    work_log(
                        "running-work",
                        "2026-01-01T00:01:05Z",
                        Some("turn-2"),
                        "Searched files",
                        WorkTone::Tool,
                    ),
                ),
            ],
            true,
        )
    });

    let folded: Vec<&str> = rows
        .iter()
        .filter_map(|row| match &row.content {
            Row::TurnFold { run_id, .. } => Some(run_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(folded, vec!["turn-1"]);
    assert!(ids(&rows).contains(&"live-activity-row"));
}

#[test]
fn only_shows_assistant_metadata_on_the_terminal_assistant_message() {
    let rows = derive(&MessagesTimelineRowsInput {
        expanded_run_ids: set(&["turn-1"]),
        ..rows_input(
            vec![
                message_entry(
                    "assistant-thought-entry",
                    "2026-01-01T00:00:10Z",
                    assistant_message(
                        "assistant-thought",
                        "Checking first.",
                        Some("turn-1"),
                        "2026-01-01T00:00:10Z",
                        "2026-01-01T00:00:11Z",
                        false,
                    ),
                ),
                message_entry(
                    "assistant-final-entry",
                    "2026-01-01T00:00:20Z",
                    assistant_message(
                        "assistant-final",
                        "Done.",
                        Some("turn-1"),
                        "2026-01-01T00:00:20Z",
                        "2026-01-01T00:00:30Z",
                        false,
                    ),
                ),
            ],
            false,
        )
    });

    assert_eq!(assistant_meta_flags(&rows), vec![false, true]);
}

#[test]
fn withholds_assistant_metadata_while_the_active_turn_is_still_in_progress() {
    let rows = derive(&MessagesTimelineRowsInput {
        latest_run: Some(latest_run(
            "turn-1",
            RunStatus::Running,
            Some("2026-01-01T00:00:00Z"),
            None,
        )),
        active_turn_started_at: Some("2026-01-01T00:00:00Z".into()),
        ..rows_input(
            vec![message_entry(
                "assistant-thought-entry",
                "2026-01-01T00:00:10Z",
                assistant_message(
                    "assistant-thought",
                    "Working on it.",
                    Some("turn-1"),
                    "2026-01-01T00:00:10Z",
                    "2026-01-01T00:00:11Z",
                    false,
                ),
            )],
            true,
        )
    });

    match find_kind(&rows, MessagesTimelineRowKind::Message) {
        Some(Row::Message {
            show_assistant_meta,
            show_assistant_copy_button,
            ..
        }) => {
            assert!(!show_assistant_meta);
            assert!(!show_assistant_copy_button);
        }
        other => panic!("expected an assistant message row, got {other:?}"),
    }
    assert_eq!(
        rows.last().map(|row| row.kind()),
        Some(MessagesTimelineRowKind::Thinking)
    );
}
