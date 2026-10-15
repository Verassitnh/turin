//! A runtime notice is a non-failing warning row, and it stays visible inside
//! its work group whether the run is still going, completed, or superseded.

use super::*;
use crate::components::chat::timeline::work_log_presentation::work_entry_display_indicates_tool_failure;

#[test]
fn keeps_system_notices_visible_as_non_failing_warnings_through_work() {
    for state in ["running", "completed", "superseded"] {
        let fixture = make_streaming_timeline_fixture("");
        let message = "Claude changed its safety mode.\nReview the active permission settings before continuing.";
        let source = projected(
            0,
            TurnItem {
                run_id: Some(fixture.run_id.clone()),
                ordinal: 1,
                started_at: Some(fixture.time(1)),
                completed_at: Some(fixture.time(1)),
                title: Some(message.into()),
                ..TurnItem::new(
                    "safety-notice",
                    &fixture.thread_id,
                    TurnItemStatus::Completed,
                    fixture.time(1),
                    TurnItemPayload::SystemNotice {
                        message: message.into(),
                    },
                )
            },
        );
        let notice = entries_from(vec![source.clone()])
            .into_iter()
            .next()
            .expect("notice entry");
        let TimelineEntryKind::Work { entry } = &notice.kind else {
            panic!("expected a notice work entry, got {:?}", notice.kind);
        };
        assert_eq!(entry.projected_item.as_ref(), Some(&source));
        assert_eq!(
            entry.source_activity_kind.as_deref(),
            Some("runtime.warning")
        );
        assert_eq!(work_entry_display_label(entry, None), message);
        assert!(!work_entry_display_indicates_tool_failure(entry));

        let running = state == "running";
        let notice_entry = TimelineEntry {
            attempt: (state == "superseded").then(|| TimelineAttempt {
                attempt_ordinal: 0,
                ..attempt(
                    "old-attempt",
                    &fixture.run_id,
                    AttemptStatus::Superseded,
                    None,
                )
            }),
            ..notice.clone()
        };
        let rows = derive(&MessagesTimelineRowsInput {
            latest_run: Some(latest_run(
                &fixture.run_id,
                if running {
                    RunStatus::Running
                } else {
                    RunStatus::Completed
                },
                Some(&notice.created_at),
                (!running).then_some(notice.created_at.as_str()),
            )),
            active_turn_started_at: Some(notice.created_at.clone()),
            ..rows_input(
                vec![
                    notice_entry.clone(),
                    work_entry(
                        "nearby-tool",
                        &notice.created_at,
                        WorkLogEntry {
                            tool_lifecycle_status: Some(ToolLifecycleStatus::Completed),
                            ..work_log(
                                "nearby-tool",
                                &notice.created_at,
                                Some(&fixture.run_id),
                                "Ran command",
                                WorkTone::Tool,
                            )
                        },
                    ),
                    message_entry(
                        "terminal-message",
                        &notice.created_at,
                        assistant_message(
                            "terminal-message",
                            "Continuing with the selected settings.",
                            Some(&fixture.run_id),
                            &notice.created_at,
                            &notice.created_at,
                            running,
                        ),
                    ),
                ],
                running,
            )
        });

        let grouped = rows.iter().find_map(|row| match &row.content {
            Row::Work {
                grouped_entries, ..
            } if grouped_entries.contains(entry) => Some(grouped_entries.clone()),
            _ => None,
        });
        assert_eq!(grouped, Some(vec![entry.clone()]), "{state}");
    }
}
