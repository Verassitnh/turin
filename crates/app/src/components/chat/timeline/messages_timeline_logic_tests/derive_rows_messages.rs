//! `deriveMessagesTimelineRows` message-row cases from
//! MessagesTimeline.logic.test.ts: copy controls, streaming copy state, and
//! the diff summaries and revert counts projected onto message rows.

use super::*;

fn assistant_message_rows(rows: &[Arc<MessagesTimelineRow>]) -> Vec<&MessagesTimelineRowContent> {
    rows.iter()
        .map(|row| &row.content)
        .filter(|content| {
            matches!(content, Row::Message { message, .. } if message.role == MessageRole::Assistant)
        })
        .collect()
}

#[test]
fn only_the_terminal_assistant_message_in_a_turn_gets_the_copy_button() {
    let rows = derive(&MessagesTimelineRowsInput {
        expanded_run_ids: set(&["turn-1"]),
        ..rows_input(
            vec![
                message_entry(
                    "user-1-entry",
                    "2026-01-01T00:00:00Z",
                    user_message("user-1", "Write a poem", None, "2026-01-01T00:00:00Z"),
                ),
                message_entry(
                    "assistant-thought-entry",
                    "2026-01-01T00:00:10Z",
                    assistant_message(
                        "assistant-thought",
                        "I should ground this first.",
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
                        "Here is the poem.",
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

    let assistant = assistant_message_rows(&rows);
    assert_eq!(assistant.len(), 2);
    let copy_button = |content: &MessagesTimelineRowContent| match content {
        Row::Message {
            show_assistant_copy_button,
            ..
        } => *show_assistant_copy_button,
        other => panic!("expected a message row, got {other:?}"),
    };
    assert!(!copy_button(assistant[0]));
    assert!(copy_button(assistant[1]));
}

#[test]
fn marks_only_the_active_assistant_turn_as_streaming_for_copy_controls() {
    let rows = derive(&MessagesTimelineRowsInput {
        latest_run: Some(latest_run(
            "turn-2",
            RunStatus::Running,
            Some("2026-01-01T00:00:19Z"),
            None,
        )),
        ..rows_input(
            vec![
                message_entry(
                    "assistant-one-entry",
                    "2026-01-01T00:00:10Z",
                    assistant_message(
                        "assistant-one",
                        "Earlier response.",
                        Some("turn-1"),
                        "2026-01-01T00:00:10Z",
                        "2026-01-01T00:00:11Z",
                        false,
                    ),
                ),
                message_entry(
                    "assistant-two-entry",
                    "2026-01-01T00:00:20Z",
                    assistant_message(
                        "assistant-two",
                        "Active response.",
                        Some("turn-2"),
                        "2026-01-01T00:00:20Z",
                        "2026-01-01T00:00:30Z",
                        false,
                    ),
                ),
            ],
            false,
        )
    });

    let streaming = |content: &MessagesTimelineRowContent| match content {
        Row::Message {
            assistant_copy_streaming,
            ..
        } => *assistant_copy_streaming,
        other => panic!("expected a message row, got {other:?}"),
    };
    let assistant = assistant_message_rows(&rows);
    assert!(!streaming(assistant[0]));
    assert!(streaming(assistant[1]));
}

#[test]
fn projects_assistant_diff_summaries_and_user_revert_counts_onto_the_affected_rows() {
    let summary = TurnDiffSummary {
        checkpoint_id: None,
        scope_id: None,
        run_id: "turn-1".into(),
        completed_at: "2026-01-01T00:00:30Z".into(),
        assistant_message_id: Some("assistant-1".into()),
        checkpoint_turn_count: 2,
        checkpoint_ref: "checkpoint-1".into(),
        status: CheckpointStatus::Ready,
        files: vec![CheckpointFileSummary {
            path: "src/index.ts".into(),
            kind: "modified".into(),
            additions: 3,
            deletions: 1,
        }],
    };

    let rows = derive(&MessagesTimelineRowsInput {
        turn_diff_summaries: vec![summary.clone()],
        supports_conversation_rollback: true,
        ..rows_input(
            vec![
                message_entry(
                    "user-entry",
                    "2026-01-01T00:00:00Z",
                    ChatMessage {
                        input_intent: Some(InputIntent::TurnStart),
                        ..user_message(
                            "user-1",
                            "Do the thing",
                            Some("turn-1"),
                            "2026-01-01T00:00:00Z",
                        )
                    },
                ),
                message_entry(
                    "assistant-entry",
                    "2026-01-01T00:00:20Z",
                    assistant_message(
                        "assistant-1",
                        "Done",
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

    let user = rows
        .iter()
        .find_map(|row| match &row.content {
            Row::Message {
                message,
                revert_turn_count,
                ..
            } if message.role == MessageRole::User => Some(*revert_turn_count),
            _ => None,
        })
        .expect("user row");
    assert_eq!(user, Some(1));

    let assistant = rows
        .iter()
        .find_map(|row| match &row.content {
            Row::Message {
                message,
                assistant_turn_diff_summary,
                ..
            } if message.role == MessageRole::Assistant => {
                Some(assistant_turn_diff_summary.clone())
            }
            _ => None,
        })
        .expect("assistant row");
    assert_eq!(assistant, Some(summary));
}
