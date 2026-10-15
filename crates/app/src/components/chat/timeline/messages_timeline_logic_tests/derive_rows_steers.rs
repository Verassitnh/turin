//! `deriveMessagesTimelineRows` cases about steers, queued sends, and which
//! turn is the active one: the working header and the turn folds around them.

use super::*;

/// `new Date(Date.UTC(2026, 0, 1, 0, 0, second)).toISOString()`.
fn at(second: u32) -> String {
    format!("2026-01-01T00:00:{second:02}.000Z")
}

#[test]
fn keeps_the_working_header_below_the_initiating_prompt_across_repeated_steers() {
    let run_id = "steered-run";
    let started_at = "2026-01-01T00:00:00Z";
    let prompt = |id: &str, steer: bool| {
        message_entry(
            id,
            started_at,
            ChatMessage {
                input_intent: steer.then_some(InputIntent::Steer),
                ..user_message(id, id, Some(run_id), started_at)
            },
        )
    };
    let mut entries = vec![prompt("initial-prompt", false)];
    for index in 0..3 {
        let rows = derive(&MessagesTimelineRowsInput {
            latest_run: Some(latest_run(
                run_id,
                RunStatus::Running,
                Some(started_at),
                None,
            )),
            active_turn_started_at: Some(started_at.into()),
            ..rows_input(entries.clone(), true)
        });
        assert_eq!(
            ids(&rows[..2]),
            vec!["initial-prompt", "working-indicator-row"],
            "steer {index}"
        );
        let working: Vec<_> = rows
            .iter()
            .filter_map(|row| match &row.content {
                Row::Working { created_at, .. } => Some(created_at.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(working, vec![Some(started_at.to_string())]);
        let messages: Vec<&str> = rows
            .iter()
            .filter(|row| row.kind() == MessagesTimelineRowKind::Message)
            .map(|row| row.id())
            .collect();
        let expected: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(messages, expected, "steer {index}");
        entries.push(prompt(&format!("steer-{index}"), true));
    }
}

#[test]
fn keeps_the_duration_header_at_the_initiating_prompt_when_a_steer_arrives_before_output() {
    let run_id = "steered-run";
    for steer_intent in [InputIntent::Steer, InputIntent::PromotedQueuedToSteer] {
        let prompt = |id: &str, second: u32, intent: InputIntent| {
            message_entry(
                id,
                &at(second),
                ChatMessage {
                    input_intent: Some(intent),
                    ..user_message(id, id, Some(run_id), &at(second))
                },
            )
        };
        let entries = vec![
            prompt("initial-prompt", 0, InputIntent::TurnStart),
            prompt("steer", 5, steer_intent),
        ];
        let work = work_entry(
            "work",
            &at(8),
            work_log("work", &at(8), Some(run_id), "Ran command", WorkTone::Tool),
        );
        let final_message = message_entry(
            "final",
            &at(20),
            assistant_message("final", "Done", Some(run_id), &at(20), &at(20), false),
        );

        for is_working in [true, false] {
            for expanded in [true, false] {
                let mut timeline = entries.clone();
                timeline.push(work.clone());
                timeline.push(final_message.clone());
                let rows = derive(&MessagesTimelineRowsInput {
                    latest_run: Some(latest_run(
                        run_id,
                        if is_working {
                            RunStatus::Running
                        } else {
                            RunStatus::Completed
                        },
                        Some(&at(0)),
                        (!is_working).then(|| at(20)).as_deref(),
                    )),
                    active_turn_started_at: Some(at(0)),
                    expanded_run_ids: if expanded {
                        set(&[run_id])
                    } else {
                        Default::default()
                    },
                    ..rows_input(timeline, is_working)
                });

                let fold_or_working = if is_working {
                    "working-indicator-row".to_string()
                } else {
                    format!("turn-fold:{run_id}")
                };
                assert_eq!(
                    ids(&rows[..3]),
                    vec!["initial-prompt", fold_or_working.as_str(), "steer"],
                    "working {is_working} expanded {expanded}"
                );
                assert_eq!(rows[1].content.created_at(), Some(at(0).as_str()));
                if !is_working {
                    match &rows[1].content {
                        Row::TurnFold {
                            label,
                            expanded: row_expanded,
                            ..
                        } => {
                            assert_eq!(label, "Worked for 20s");
                            assert_eq!(*row_expanded, expanded);
                        }
                        other => panic!("expected a turn fold, got {other:?}"),
                    }
                }
                assert!(ids(&rows).contains(&"final"));
                assert_eq!(ids(&rows).contains(&"work"), is_working || expanded);
            }
        }
    }
}

#[test]
fn keeps_the_previous_turn_folded_while_a_newly_sent_message_awaits_its_turn() {
    // Right after send, isWorking is true but latestRun still points at the
    // previous, settled turn — it must stay folded through that window.
    let rows = derive(&MessagesTimelineRowsInput {
        latest_run: Some(latest_run(
            "turn-1",
            RunStatus::Completed,
            Some("2026-01-01T00:00:00Z"),
            Some("2026-01-01T00:00:22Z"),
        )),
        active_turn_started_at: Some("2026-01-01T00:01:00Z".into()),
        ..rows_input(
            vec![
                work_entry(
                    "work-entry-1",
                    "2026-01-01T00:00:05Z",
                    work_log(
                        "work-1",
                        "2026-01-01T00:00:05Z",
                        Some("turn-1"),
                        "Ran command",
                        WorkTone::Tool,
                    ),
                ),
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
                message_entry(
                    "user-followup-entry",
                    "2026-01-01T00:01:00Z",
                    user_message("user-followup", "yooo", None, "2026-01-01T00:01:00Z"),
                ),
            ],
            true,
        )
    });

    assert_eq!(
        ids(&rows),
        vec![
            "turn-fold:turn-1",
            "assistant-final-entry",
            "user-followup-entry",
            "working-indicator-row",
            "live-activity-row",
        ]
    );
    let final_row = rows
        .iter()
        .find(|row| row.id() == "assistant-final-entry")
        .expect("final row");
    assert!(matches!(
        &final_row.content,
        Row::Message {
            show_assistant_meta: true,
            ..
        }
    ));
    assert_eq!(
        rows.last().map(|row| row.kind()),
        Some(MessagesTimelineRowKind::Thinking)
    );
}

#[test]
fn does_not_fold_the_active_in_progress_turn() {
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
                message_entry(
                    "assistant-thought-entry",
                    "2026-01-01T00:00:05Z",
                    assistant_message(
                        "assistant-thought",
                        "Working on it.",
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
            ],
            true,
        )
    });

    assert!(!has_kind(&rows, MessagesTimelineRowKind::TurnFold));
    assert_eq!(
        ids(&rows),
        vec![
            "working-indicator-row",
            "assistant-thought-entry",
            "live-activity-row",
        ]
    );
}
