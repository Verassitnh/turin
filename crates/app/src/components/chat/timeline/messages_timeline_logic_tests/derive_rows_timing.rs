//! `deriveMessagesTimelineRows` fold cases that depend on turn timing: the
//! "Worked for" duration, the stopped label, and the fold of every assistant
//! message before the terminal one.

use super::*;

#[test]
fn folds_all_assistant_messages_before_the_terminal_message() {
    let rows = derive(&rows_input(
        vec![
            message_entry(
                "assistant-first-entry",
                "2026-01-01T00:00:01Z",
                assistant_message(
                    "assistant-first",
                    "The main result is ready.",
                    Some("turn-1"),
                    "2026-01-01T00:00:01Z",
                    "2026-01-01T00:00:02Z",
                    false,
                ),
            ),
            message_entry(
                "assistant-middle-entry",
                "2026-01-01T00:00:03Z",
                assistant_message(
                    "assistant-middle",
                    "I am checking one more detail.",
                    Some("turn-1"),
                    "2026-01-01T00:00:03Z",
                    "2026-01-01T00:00:04Z",
                    false,
                ),
            ),
            message_entry(
                "assistant-final-entry",
                "2026-01-01T00:00:05Z",
                assistant_message(
                    "assistant-final",
                    "Verification finished.",
                    Some("turn-1"),
                    "2026-01-01T00:00:05Z",
                    "2026-01-01T00:00:06Z",
                    false,
                ),
            ),
        ],
        false,
    ));

    assert_eq!(
        ids(&rows),
        vec!["turn-fold:turn-1", "assistant-final-entry"]
    );
}

#[test]
fn derives_a_sane_duration_for_a_steer_superseded_turn_with_one_instant_commentary_message() {
    // A steer ends the previous turn early: its only message completes the
    // instant it is created, and trailing work entries land after it. The
    // fold duration spans from the user message that started the turn to the
    // last entry, not message createdAt → message updatedAt (~0ms).
    let rows = derive(&MessagesTimelineRowsInput {
        latest_run: Some(latest_run(
            "turn-2",
            RunStatus::Running,
            Some("2026-01-01T00:00:14Z"),
            None,
        )),
        active_turn_started_at: Some("2026-01-01T00:00:14Z".into()),
        ..rows_input(
            vec![
                message_entry(
                    "user-entry",
                    "2026-01-01T00:00:00Z",
                    user_message("user-1", "do it once more", None, "2026-01-01T00:00:00Z"),
                ),
                work_entry(
                    "work-entry-before-message",
                    "2026-01-01T00:00:07Z",
                    work_log(
                        "work-before-message",
                        "2026-01-01T00:00:07Z",
                        Some("turn-1"),
                        "Status updated",
                        WorkTone::Info,
                    ),
                ),
                message_entry(
                    "assistant-commentary-entry",
                    "2026-01-01T00:00:09Z",
                    assistant_message(
                        "assistant-commentary",
                        "Kicking off call 1.",
                        Some("turn-1"),
                        "2026-01-01T00:00:09Z",
                        "2026-01-01T00:00:09Z",
                        false,
                    ),
                ),
                work_entry(
                    "work-entry-1",
                    "2026-01-01T00:00:12Z",
                    work_log(
                        "work-1",
                        "2026-01-01T00:00:12Z",
                        Some("turn-1"),
                        "Ran command",
                        WorkTone::Tool,
                    ),
                ),
                message_entry(
                    "steer-user-entry",
                    "2026-01-01T00:00:14Z",
                    user_message("user-2", "actually do 15", None, "2026-01-01T00:00:14Z"),
                ),
                message_entry(
                    "assistant-next-turn-entry",
                    "2026-01-01T00:00:17Z",
                    assistant_message(
                        "assistant-next",
                        "One down — adjusting.",
                        Some("turn-2"),
                        "2026-01-01T00:00:17Z",
                        "2026-01-01T00:00:17Z",
                        true,
                    ),
                ),
            ],
            true,
        )
    });

    // User message (00:00:00) → trailing work entry (00:00:12).
    let Some(Row::TurnFold { run_id, label, .. }) =
        find_kind(&rows, MessagesTimelineRowKind::TurnFold)
    else {
        panic!("expected a turn fold");
    };
    assert_eq!(run_id, "turn-1");
    assert_eq!(label, "Worked for 12s");
}

#[test]
fn uses_latest_turn_timings_and_the_stopped_label_for_an_interrupted_latest_turn() {
    let rows = derive(&MessagesTimelineRowsInput {
        latest_run: Some(latest_run(
            "turn-1",
            RunStatus::Interrupted,
            Some("2026-01-01T00:00:00Z"),
            Some("2026-01-01T00:00:47Z"),
        )),
        ..rows_input(
            vec![work_entry(
                "work-entry-1",
                "2026-01-01T00:00:05Z",
                work_log(
                    "work-1",
                    "2026-01-01T00:00:05Z",
                    Some("turn-1"),
                    "Ran command",
                    WorkTone::Tool,
                ),
            )],
            false,
        )
    });

    match rows.as_slice() {
        [row] => match &row.content {
            Row::TurnFold {
                run_id,
                label,
                expanded,
                ..
            } => {
                assert_eq!(run_id, "turn-1");
                assert_eq!(label, "You stopped after 47s");
                assert!(!expanded);
            }
            other => panic!("expected a turn fold, got {other:?}"),
        },
        other => panic!("expected a single row, got {:?}", ids(other)),
    }
}
