//! `deriveMessagesTimelineRows` cases over v2 turn items: settled turns fold
//! behind a "Worked for" row, and subagent cards fold with their turn.

use super::*;

/// A turn item that belongs to `turn-1`.
fn in_turn_one(id: &str, at: &str, payload: TurnItemPayload) -> TurnItem {
    TurnItem {
        run_id: Some("turn-1".into()),
        ..turn_item(id, at, payload)
    }
}

#[test]
fn folds_settled_turn_commentary_and_work_behind_a_worked_for_row() {
    let thread_created = event_entry(
        "thread-created-entry",
        "2026-01-01T00:00:10Z",
        projected(
            0,
            turn_item(
                "thread-created",
                "2026-01-01T00:00:10Z",
                TurnItemPayload::ThreadCreated {
                    target_thread_id: "thread-2".into(),
                    target_model: "gpt-5".into(),
                },
            ),
        ),
    );
    let entries = vec![
        message_entry(
            "user-entry",
            "2026-01-01T00:00:00Z",
            user_message("user-1", "Build it", None, "2026-01-01T00:00:00Z"),
        ),
        message_entry(
            "assistant-thought-entry",
            "2026-01-01T00:00:05Z",
            assistant_message(
                "assistant-thought",
                "Looking around first.",
                Some("turn-1"),
                "2026-01-01T00:00:05Z",
                "2026-01-01T00:00:06Z",
                false,
            ),
        ),
        work_entry(
            "work-entry-1",
            "2026-01-01T00:00:08Z",
            work_log(
                "work-1",
                "2026-01-01T00:00:08Z",
                Some("turn-1"),
                "Ran command",
                WorkTone::Tool,
            ),
        ),
        work_entry(
            "provider-recovered-entry",
            "2026-01-01T00:00:09Z",
            WorkLogEntry {
                item_type: Some(TurnItemType::Error),
                tool_lifecycle_status: Some(ToolLifecycleStatus::Completed),
                ..work_log(
                    "provider-recovered",
                    "2026-01-01T00:00:09Z",
                    Some("turn-1"),
                    "Provider recovered (2/5 retries)",
                    WorkTone::Info,
                )
            },
        ),
        thread_created.clone(),
        message_entry(
            "assistant-final-entry",
            "2026-01-01T00:00:20Z",
            assistant_message(
                "assistant-final",
                "Done",
                Some("turn-1"),
                "2026-01-01T00:00:20Z",
                "2026-01-01T00:00:22Z",
                false,
            ),
        ),
    ];

    let collapsed = derive(&rows_input(entries.clone(), false));
    match find_kind(&collapsed, MessagesTimelineRowKind::TurnFold) {
        Some(Row::TurnFold {
            run_id,
            expanded,
            label,
            ..
        }) => {
            assert_eq!(run_id, "turn-1");
            assert!(!expanded);
            // User message boundary (00:00:00) → terminal message updatedAt (00:00:22).
            assert_eq!(label, "Worked for 22s");
        }
        other => panic!("expected a turn fold, got {other:?}"),
    }
    assert_eq!(
        ids(&collapsed),
        vec![
            "user-entry",
            "turn-fold:turn-1",
            "thread-created-entry",
            "assistant-final-entry",
        ]
    );

    let expanded_rows = derive(&MessagesTimelineRowsInput {
        expanded_run_ids: set(&["turn-1"]),
        ..rows_input(entries.clone(), false)
    });
    assert_eq!(
        ids(&expanded_rows),
        vec![
            "user-entry",
            "turn-fold:turn-1",
            "assistant-thought-entry",
            "work-toggle:work-entry-1",
            "thread-created-entry",
            "assistant-final-entry",
        ]
    );
    assert!(matches!(
        find_kind(&expanded_rows, MessagesTimelineRowKind::TurnFold),
        Some(Row::TurnFold { expanded: true, .. })
    ));

    // Expanding the activity preserves tool output and provider recovery details.
    let opened = derive(&MessagesTimelineRowsInput {
        expanded_run_ids: set(&["turn-1"]),
        expanded_work_group_ids: set(&["work-group:work-entry-1"]),
        ..rows_input(entries, false)
    });
    let expanded_work = opened
        .iter()
        .find(|row| row.kind() == MessagesTimelineRowKind::Work)
        .expect("expanded work row");
    assert_eq!(
        grouped_ids(&expanded_work.content),
        vec!["work-1", "provider-recovered"]
    );
}

#[test]
fn hides_subagents_in_folded_turns_when_they_arrive_before_commentary() {
    let rows = derive(&rows_input(
        vec![
            message_entry(
                "user-entry",
                "2026-01-01T00:00:00Z",
                user_message("user-1", "Spawn a subagent", None, "2026-01-01T00:00:00Z"),
            ),
            event_entry(
                "subagent-card-entry",
                "2026-01-01T00:00:03Z",
                projected(
                    0,
                    in_turn_one(
                        "subagent-card",
                        "2026-01-01T00:00:03Z",
                        subagent_payload("subagent-card", "app_owned", None),
                    ),
                ),
            ),
            message_entry(
                "assistant-commentary-entry",
                "2026-01-01T00:00:05Z",
                assistant_message(
                    "assistant-commentary",
                    "I spawned the subagent.",
                    Some("turn-1"),
                    "2026-01-01T00:00:05Z",
                    "2026-01-01T00:00:06Z",
                    false,
                ),
            ),
            message_entry(
                "assistant-final-entry",
                "2026-01-01T00:00:20Z",
                assistant_message(
                    "assistant-final",
                    "Subagent says: Hello.",
                    Some("turn-1"),
                    "2026-01-01T00:00:20Z",
                    "2026-01-01T00:00:22Z",
                    false,
                ),
            ),
        ],
        false,
    ));

    assert_eq!(
        ids(&rows),
        vec!["user-entry", "turn-fold:turn-1", "assistant-final-entry"]
    );
}

/// `{ ...entry, attempt }`: the entry as attributed to `attempt`.
pub(super) fn in_attempt(entry: TimelineEntry, attempt: &TimelineAttempt) -> TimelineEntry {
    TimelineEntry {
        attempt: Some(attempt.clone()),
        ..entry
    }
}

#[test]
fn collapses_only_output_from_a_superseded_v2_attempt_within_the_active_logical_run() {
    let run_id = "run-steered";
    let superseded = TimelineAttempt {
        attempt_ordinal: 1,
        ..attempt("attempt-1", run_id, AttemptStatus::Superseded, None)
    };
    let active = TimelineAttempt {
        attempt_ordinal: 2,
        ..attempt("attempt-2", run_id, AttemptStatus::Running, None)
    };
    let timeline_entries = vec![
        in_attempt(
            message_entry(
                "initial-user-entry",
                "2026-01-01T00:00:00Z",
                ChatMessage {
                    input_intent: Some(InputIntent::TurnStart),
                    ..user_message(
                        "initial-user",
                        "Build it",
                        Some(run_id),
                        "2026-01-01T00:00:00Z",
                    )
                },
            ),
            &superseded,
        ),
        in_attempt(
            message_entry(
                "superseded-assistant-entry",
                "2026-01-01T00:00:02Z",
                assistant_message(
                    "superseded-assistant",
                    "Partial old response",
                    Some(run_id),
                    "2026-01-01T00:00:02Z",
                    "2026-01-01T00:00:03Z",
                    false,
                ),
            ),
            &superseded,
        ),
        in_attempt(
            work_entry(
                "superseded-work-entry",
                "2026-01-01T00:00:04Z",
                work_log(
                    "superseded-work",
                    "2026-01-01T00:00:04Z",
                    Some(run_id),
                    "Old command",
                    WorkTone::Tool,
                ),
            ),
            &superseded,
        ),
        in_attempt(
            event_entry(
                "superseded-thread-created-entry",
                "2026-01-01T00:00:04.500Z",
                projected(
                    0,
                    turn_item(
                        "superseded-thread-created",
                        "2026-01-01T00:00:04.500Z",
                        TurnItemPayload::ThreadCreated {
                            target_thread_id: "thread-2".into(),
                            target_model: "gpt-5".into(),
                        },
                    ),
                ),
            ),
            &superseded,
        ),
        in_attempt(
            message_entry(
                "steer-user-entry",
                "2026-01-01T00:00:05Z",
                ChatMessage {
                    input_intent: Some(InputIntent::Steer),
                    ..user_message(
                        "steer-user",
                        "Change direction",
                        Some(run_id),
                        "2026-01-01T00:00:05Z",
                    )
                },
            ),
            &active,
        ),
        in_attempt(
            message_entry(
                "active-assistant-entry",
                "2026-01-01T00:00:06Z",
                assistant_message(
                    "active-assistant",
                    "Current response",
                    Some(run_id),
                    "2026-01-01T00:00:06Z",
                    "2026-01-01T00:00:07Z",
                    true,
                ),
            ),
            &active,
        ),
    ];
    let input = MessagesTimelineRowsInput {
        latest_run: Some(latest_run(
            run_id,
            RunStatus::Running,
            Some("2026-01-01T00:00:00Z"),
            None,
        )),
        ..rows_input(timeline_entries, false)
    };

    let collapsed = derive(&input);
    assert_eq!(
        ids(&collapsed),
        vec![
            "initial-user-entry",
            "attempt-fold:attempt-1",
            "superseded-thread-created-entry",
            "steer-user-entry",
            "active-assistant-entry",
        ]
    );
    match find_kind(&collapsed, MessagesTimelineRowKind::AttemptFold) {
        Some(Row::AttemptFold {
            attempt_id,
            run_id: row_run,
            label,
            expanded,
            ..
        }) => {
            assert_eq!(attempt_id, "attempt-1");
            assert_eq!(row_run, run_id);
            assert_eq!(label, "Superseded attempt");
            assert!(!expanded);
        }
        other => panic!("expected an attempt fold, got {other:?}"),
    }

    let expanded = derive(&MessagesTimelineRowsInput {
        expanded_attempt_ids: set(&["attempt-1"]),
        ..input
    });
    assert_eq!(
        ids(&expanded),
        vec![
            "initial-user-entry",
            "attempt-fold:attempt-1",
            "superseded-assistant-entry",
            "superseded-work-entry",
            "superseded-thread-created-entry",
            "steer-user-entry",
            "active-assistant-entry",
        ]
    );
}
