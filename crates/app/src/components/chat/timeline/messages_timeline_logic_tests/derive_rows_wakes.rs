//! Notification wakes: a notification stays visible outside folded work, and
//! completed responses stay folded across the wakes that follow them.

use super::*;

/// `2026-01-01T00:00:SS` for the fixtures below.
fn time(second: u32) -> String {
    format!("2026-01-01T00:00:{second:02}Z")
}

#[test]
fn keeps_a_wake_notification_visible_outside_folded_work() {
    for is_working in [true, false] {
        let fixture = make_streaming_timeline_fixture("");
        let mut visible = fixture.visible_turn_items.clone();
        let source = visible
            .iter_mut()
            .find(|row| matches!(row.item.payload, TurnItemPayload::UserMessage { .. }))
            .expect("user fixture");
        source.item.payload = TurnItemPayload::Notification {
            source: NotificationSource::BackgroundTask,
            outcome: NotificationOutcome::Completed,
            summary: "Delegated task finished".into(),
            detail: None,
        };
        let entries = entries_from(visible);
        let rows = derive(&MessagesTimelineRowsInput {
            running_run_id: Some(fixture.run_id.clone()),
            active_turn_started_at: Some(fixture.time(0)),
            ..rows_input(entries, is_working)
        });

        assert!(
            rows.iter().any(|row| match &row.content {
                Row::Work {
                    grouped_entries, ..
                } => grouped_entries
                    .iter()
                    .any(|entry| entry.label == "Delegated task finished"),
                _ => false,
            }),
            "working {is_working}"
        );
    }
}

#[test]
fn keeps_completed_responses_folded_across_notification_wakes() {
    for is_working in [true, false] {
        let work = |id: &str, run: &str, second: u32, notification: bool| {
            work_entry(
                id,
                &time(second),
                WorkLogEntry {
                    item_type: notification.then_some(TurnItemType::Notification),
                    ..work_log(id, &time(second), Some(run), id, WorkTone::Info)
                },
            )
        };
        let answer = |id: &str, run: &str, second: u32| {
            message_entry(
                id,
                &time(second),
                assistant_message(id, id, Some(run), &time(second), &time(second), false),
            )
        };
        let mut entries = vec![
            work("launch-tools", "initial", 1, false),
            answer("launched", "initial", 5),
            work("notification-a", "wake-a", 20, true),
            work("read-a", "wake-a", 21, false),
            answer("ack-a", "wake-a", 25),
            work("notification-b", "wake-b", 40, true),
            work("read-b", "wake-b", 41, false),
        ];
        if !is_working {
            entries.push(answer("ack-b", "wake-b", 45));
        }
        let rows = derive(&MessagesTimelineRowsInput {
            latest_run: Some(latest_run(
                "wake-b",
                if is_working {
                    RunStatus::Running
                } else {
                    RunStatus::Completed
                },
                Some(&time(40)),
                (!is_working).then(|| time(45)).as_deref(),
            )),
            active_turn_started_at: Some(time(40)),
            ..rows_input(entries, is_working)
        });

        let ids = ids(&rows);
        let position = |id: &str| ids.iter().position(|row_id| *row_id == id);
        assert!(ids.contains(&"turn-fold:initial"));
        assert!(!ids.contains(&"launch-tools"));
        assert!(ids.contains(&"turn-fold:wake-a"));
        assert!(position("notification-a") < position("turn-fold:wake-a"));
        assert!(position("turn-fold:wake-a") < position("ack-a"));
        let boundary = if is_working {
            "working-indicator-row"
        } else {
            "turn-fold:wake-b"
        };
        assert!(ids.contains(&boundary), "working {is_working}");
        assert!(position("notification-b") < position(boundary));
        if is_working {
            let boundary_row = rows
                .iter()
                .find(|row| row.id() == boundary)
                .expect("boundary row");
            assert_eq!(boundary_row.content.created_at(), Some(time(40).as_str()));
        }
    }
}
