//! `deriveMessagesTimelineRows` turn-fold cases from
//! MessagesTimeline.logic.test.ts: what folds behind a "Worked for" header and
//! what stays visible after the terminal response.

use super::*;

/// Upstream `{ ...entry, entry: { ...entry.entry, toolLifecycleStatus } }` for
/// the work entry with `id`.
fn with_lifecycle(
    entries: &[TimelineEntry],
    id: &str,
    status: ToolLifecycleStatus,
) -> Vec<TimelineEntry> {
    entries
        .iter()
        .map(|entry| match &entry.kind {
            TimelineEntryKind::Work { entry: work } if entry.id == id => TimelineEntry {
                kind: TimelineEntryKind::Work {
                    entry: WorkLogEntry {
                        tool_lifecycle_status: Some(status),
                        ..work.clone()
                    },
                },
                ..entry.clone()
            },
            _ => entry.clone(),
        })
        .collect()
}

#[test]
fn folds_the_first_assistant_message_and_settled_work_before_the_terminal_response() {
    let entries = vec![
        message_entry(
            "user-entry",
            "2026-01-01T00:00:00Z",
            user_message("user-1", "Build it", None, "2026-01-01T00:00:00Z"),
        ),
        message_entry(
            "assistant-first-entry",
            "2026-01-01T00:00:05Z",
            assistant_message(
                "assistant-first",
                "Synthetic deployment checklist\n1. Confirm the deployment is ready.",
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
    let Some(Row::TurnFold {
        run_id,
        expanded,
        label,
        ..
    }) = find_kind(&collapsed, MessagesTimelineRowKind::TurnFold)
    else {
        panic!("expected a turn fold");
    };
    assert_eq!(run_id, "turn-1");
    assert!(!expanded);
    // User message boundary (00:00:00) → terminal message updatedAt (00:00:22).
    assert_eq!(label, "Worked for 22s");
    assert_eq!(
        ids(&collapsed),
        vec!["user-entry", "turn-fold:turn-1", "assistant-final-entry"]
    );

    let expanded_rows = derive(&MessagesTimelineRowsInput {
        expanded_run_ids: set(&["turn-1"]),
        ..rows_input(entries, false)
    });
    assert_eq!(
        ids(&expanded_rows),
        vec![
            "user-entry",
            "turn-fold:turn-1",
            "assistant-first-entry",
            "work-entry-1",
            "assistant-final-entry",
        ]
    );
    assert!(matches!(
        find_kind(&expanded_rows, MessagesTimelineRowKind::TurnFold),
        Some(Row::TurnFold { expanded: true, .. })
    ));
}

#[test]
fn folds_completed_activities_after_the_terminal_response() {
    let run_id = "turn-1";
    for count in 1..=3 {
        let mut entries = vec![
            work_entry(
                "work-entry-before-text",
                "2026-01-01T00:00:01Z",
                work_log(
                    "work-before-text",
                    "2026-01-01T00:00:01Z",
                    Some(run_id),
                    "Status updated",
                    WorkTone::Info,
                ),
            ),
            message_entry(
                "assistant-final-entry",
                "2026-01-01T00:00:05Z",
                assistant_message(
                    "assistant-final",
                    "CI is re-running. Waiting.",
                    Some(run_id),
                    "2026-01-01T00:00:05Z",
                    "2026-01-01T00:00:06Z",
                    false,
                ),
            ),
        ];
        entries.extend((0..count).map(|index| {
            let at = format!("2026-01-01T00:00:0{}Z", index + 7);
            work_entry(
                &format!("work-entry-after-text-{index}"),
                &at,
                WorkLogEntry {
                    item_type: Some(TurnItemType::CommandExecution),
                    tool_lifecycle_status: Some(ToolLifecycleStatus::Completed),
                    ..work_log(
                        &format!("work-after-text-{index}"),
                        &at,
                        Some(run_id),
                        "Ran command",
                        WorkTone::Tool,
                    )
                },
            )
        }));

        let input = MessagesTimelineRowsInput {
            latest_run: Some(latest_run(
                run_id,
                RunStatus::Completed,
                Some("2026-01-01T00:00:00Z"),
                Some("2026-01-01T00:00:10Z"),
            )),
            ..rows_input(entries.clone(), false)
        };
        let rows = derive(&input);
        assert_eq!(
            ids(&rows),
            vec!["turn-fold:turn-1", "assistant-final-entry"],
            "count {count}"
        );

        let expanded = derive(&MessagesTimelineRowsInput {
            expanded_run_ids: set(&[run_id]),
            ..input.clone()
        });
        assert!(
            expanded.iter().any(|row| match &row.content {
                Row::WorkToggle {
                    id, hidden_count, ..
                } => id == "work-toggle:work-entry-after-text-0" && *hidden_count == count,
                _ => row.id() == "work-entry-after-text-0",
            }),
            "count {count}"
        );

        // A late failure stays visible even though successful work is folded.
        let failed_rows = derive(&rows_input(
            with_lifecycle(
                &entries,
                "work-entry-after-text-0",
                ToolLifecycleStatus::Failed,
            ),
            false,
        ));
        assert!(
            failed_rows
                .iter()
                .any(|row| row.id() == "work-entry-after-text-0"),
            "count {count}"
        );

        // An in-progress tool while the run is still going is the live row.
        let pending = MessagesTimelineRowsInput {
            timeline_entries: Arc::new(with_lifecycle(
                &entries,
                "work-entry-after-text-0",
                ToolLifecycleStatus::InProgress,
            )),
            latest_run: Some(latest_run(
                run_id,
                RunStatus::Running,
                Some("2026-01-01T00:00:00Z"),
                None,
            )),
            is_working: true,
            active_turn_started_at: Some("2026-01-01T00:00:00Z".into()),
            ..Default::default()
        };
        let pending_rows = derive(&pending);
        assert!(
            pending_rows.iter().any(|row| matches!(
                &row.content,
                Row::WorkLive { entry, .. } if entry.id == "work-after-text-0"
            )),
            "count {count}"
        );
    }
}
