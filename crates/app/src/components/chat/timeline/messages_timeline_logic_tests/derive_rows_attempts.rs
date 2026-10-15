//! `deriveMessagesTimelineRows` cases where a steer supersedes an attempt: a
//! reply that finished before the steer stays visible, and a reply the steer
//! cut off folds as partial output.

use super::derive_rows_v2::in_attempt;
use super::*;

/// `2026-01-01T00:00:SS.000Z`-style timestamps, as upstream's `at(second)`.
fn at(second: u32) -> String {
    format!("2026-01-01T00:00:{second:02}Z")
}

/// An assistant message in `attempt` whose turn item completed at `completed`.
fn assistant_reply(id: &str, attempt: &TimelineAttempt, completed: u32) -> TimelineEntry {
    let mut entry = message_entry(
        id,
        &at(2),
        assistant_message(
            id,
            &format!("{id} text"),
            Some("steered-run"),
            &at(2),
            &at(completed),
            false,
        ),
    );
    let mut item = turn_item(
        id,
        &at(completed),
        assistant_message_payload(id, &format!("{id} text"), false),
    );
    item.completed_at = Some(at(completed));
    if let TimelineEntryKind::Message { projected_item, .. } = &mut entry.kind {
        *projected_item = Some(projected(0, item));
    }
    in_attempt(entry, attempt)
}

/// The superseded attempt (stamped at 0:10) and the attempt that replaced it.
fn attempts() -> (TimelineAttempt, TimelineAttempt) {
    let superseded = TimelineAttempt {
        attempt_ordinal: 1,
        ..attempt(
            "attempt-1",
            "steered-run",
            AttemptStatus::Superseded,
            Some(at(10).as_str()),
        )
    };
    let next = TimelineAttempt {
        attempt_ordinal: 2,
        ..attempt(
            "attempt-2",
            "steered-run",
            AttemptStatus::Completed,
            Some(at(14).as_str()),
        )
    };
    (superseded, next)
}

/// The timeline for a steer at 0:10 that replaced the reply finishing at
/// `reply_completed`.
fn rows_for(reply_completed: u32, run_status: RunStatus) -> Vec<Arc<MessagesTimelineRow>> {
    let (superseded, next) = attempts();
    let completed = run_status == RunStatus::Completed;
    derive(&MessagesTimelineRowsInput {
        latest_run: Some(latest_run(
            "steered-run",
            run_status,
            Some(&at(0)),
            completed.then(|| at(14)).as_deref(),
        )),
        ..rows_input(
            vec![
                in_attempt(
                    message_entry(
                        "initial-user-entry",
                        &at(0),
                        ChatMessage {
                            input_intent: Some(InputIntent::TurnStart),
                            ..user_message("initial-user", "Build it", Some("steered-run"), &at(0))
                        },
                    ),
                    &superseded,
                ),
                in_attempt(
                    work_entry(
                        "old-work-entry",
                        &at(1),
                        work_log(
                            "old-work",
                            &at(1),
                            Some("steered-run"),
                            "Read",
                            WorkTone::Tool,
                        ),
                    ),
                    &superseded,
                ),
                assistant_reply("old-reply-entry", &superseded, reply_completed),
                in_attempt(
                    message_entry(
                        "steer-user-entry",
                        &at(10),
                        ChatMessage {
                            input_intent: Some(InputIntent::Steer),
                            ..user_message(
                                "steer-user",
                                "What are you working on?",
                                Some("steered-run"),
                                &at(10),
                            )
                        },
                    ),
                    &next,
                ),
                assistant_reply("new-reply-entry", &next, 14),
            ],
            false,
        )
    })
}

#[test]
fn keeps_a_reply_that_finished_before_the_steer_visible_while_the_run_is_running_or_completed() {
    for run_status in [RunStatus::Running, RunStatus::Completed] {
        let rows = rows_for(4, run_status);
        assert!(!has_kind(&rows, MessagesTimelineRowKind::AttemptFold));
        assert!(ids(&rows).contains(&"old-reply-entry"), "{run_status:?}");
    }
}

#[test]
fn folds_a_reply_the_steer_cut_off_as_partial_output() {
    let rows = rows_for(11, RunStatus::Running);
    match find_kind(&rows, MessagesTimelineRowKind::AttemptFold) {
        Some(Row::AttemptFold {
            attempt_id, label, ..
        }) => {
            assert_eq!(attempt_id, "attempt-1");
            assert_eq!(label, "Superseded attempt");
        }
        other => panic!("expected an attempt fold, got {other:?}"),
    }
    assert!(!ids(&rows).contains(&"old-reply-entry"));
}
