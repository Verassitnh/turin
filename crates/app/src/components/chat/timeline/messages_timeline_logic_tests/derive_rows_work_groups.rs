//! `deriveMessagesTimelineRows` work-group cases: a group collapses to one
//! toggle that summarizes its hidden entries, and expands through the same
//! activity group.

use super::*;

fn detailed_entry(id: &str, at: &str, label: &str, tone: WorkTone, detail: &str) -> WorkLogEntry {
    WorkLogEntry {
        detail: Some(detail.into()),
        ..work_log(id, at, None, label, tone)
    }
}

#[test]
fn expands_tools_and_status_updates_through_the_same_activity_group() {
    for (middle_tone, summary) in [
        (WorkTone::Tool, "Used 3 tools"),
        (WorkTone::Info, "Used 2 tools and received 1 update"),
    ] {
        let timeline_entries = vec![
            work_entry(
                "work-entry-1",
                "2026-01-01T00:00:01Z",
                detailed_entry(
                    "work-1",
                    "2026-01-01T00:00:01Z",
                    "read",
                    WorkTone::Tool,
                    "Reading package.json",
                ),
            ),
            work_entry(
                "work-entry-2",
                "2026-01-01T00:00:02Z",
                WorkLogEntry {
                    tool_surface: Some(ToolActivitySurface::Computer),
                    ..detailed_entry(
                        "work-2",
                        "2026-01-01T00:00:02Z",
                        "Status updated",
                        middle_tone,
                        "Editing MessagesTimeline.tsx",
                    )
                },
            ),
            work_entry(
                "work-entry-3",
                "2026-01-01T00:00:03Z",
                WorkLogEntry {
                    tool_surface: Some(ToolActivitySurface::Browser),
                    tool_icon: Some(ToolActivityIcon::Website {
                        page_url: "https://example.com/checkout".into(),
                        favicon_url: None,
                    }),
                    ..detailed_entry(
                        "work-3",
                        "2026-01-01T00:00:03Z",
                        "test",
                        WorkTone::Tool,
                        "Running tests",
                    )
                },
            ),
        ];
        let input = rows_input(timeline_entries.clone(), false);
        let collapsed = derive(&input);
        let expanded = derive(&MessagesTimelineRowsInput {
            expanded_work_group_ids: set(&["work-group:work-entry-1"]),
            ..input.clone()
        });

        assert_eq!(
            ids(&collapsed),
            vec!["work-toggle:work-entry-1"],
            "{summary}"
        );
        match find_kind(&collapsed, MessagesTimelineRowKind::WorkToggle) {
            Some(Row::WorkToggle {
                group_id,
                hidden_count,
                expanded,
                summary: row_summary,
                tool_surface,
                tool_icon,
                ..
            }) => {
                assert_eq!(group_id, "work-group:work-entry-1");
                assert_eq!(*hidden_count, 3);
                assert!(!expanded);
                assert_eq!(row_summary, summary);
                assert_eq!(*tool_surface, Some(ToolActivitySurface::Browser));
                assert_eq!(
                    *tool_icon,
                    Some(ToolActivityIcon::Website {
                        page_url: "https://example.com/checkout".into(),
                        favicon_url: None,
                    })
                );
            }
            other => panic!("expected a work toggle, got {other:?}"),
        }

        assert_eq!(
            ids(&expanded),
            vec![
                "work-toggle:work-entry-1",
                "work-group:work-entry-1:details",
            ]
        );
        match find_kind(&expanded, MessagesTimelineRowKind::Work) {
            Some(Row::Work {
                is_expanded_tool_group,
                grouped_entries,
                ..
            }) => {
                assert!(is_expanded_tool_group);
                let expected: Vec<WorkLogEntry> = timeline_entries
                    .iter()
                    .filter_map(|entry| match &entry.kind {
                        TimelineEntryKind::Work { entry } => Some(entry.clone()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(grouped_entries, &expected);
            }
            other => panic!("expected an expanded work row, got {other:?}"),
        }
        assert!(matches!(
            find_kind(&expanded, MessagesTimelineRowKind::WorkToggle),
            Some(Row::WorkToggle { expanded: true, .. })
        ));
    }
}

#[test]
fn deduplicates_integration_sources_and_uses_the_first_source_icon_for_the_group() {
    let chrome = ToolActivitySource {
        key: "browser-use:chrome".into(),
        name: "Chrome".into(),
        kind: "integration".into(),
        icon: Some(ToolActivityIcon::NativeApp {
            app: "Google Chrome".into(),
        }),
    };
    let browser = |id: &str, at: &str, label: &str, page_url: &str| {
        work_entry(
            &format!("{id}-entry"),
            at,
            WorkLogEntry {
                tool_surface: Some(ToolActivitySurface::Browser),
                tool_source: Some(chrome.clone()),
                tool_icon: Some(ToolActivityIcon::Website {
                    page_url: page_url.into(),
                    favicon_url: None,
                }),
                ..work_log(id, at, None, label, WorkTone::Tool)
            },
        )
    };
    let rows = derive(&rows_input(
        vec![
            browser(
                "browser-1",
                "2026-01-01T00:00:01Z",
                "Open MATLAB",
                "https://www.mathworks.com/help/matlab/",
            ),
            browser(
                "browser-2",
                "2026-01-01T00:00:02Z",
                "Show summary",
                "https://www.mathworks.com/help/matlab/summary.html",
            ),
            work_entry(
                "command-1-entry",
                "2026-01-01T00:00:03Z",
                WorkLogEntry {
                    command: Some("git status".into()),
                    item_type: Some(TurnItemType::CommandExecution),
                    ..work_log(
                        "command-1",
                        "2026-01-01T00:00:03Z",
                        None,
                        "Ran command",
                        WorkTone::Tool,
                    )
                },
            ),
        ],
        false,
    ));

    match rows.first().map(|row| &row.content) {
        Some(Row::WorkToggle {
            summary,
            tool_surface,
            tool_icon,
            ..
        }) => {
            assert_eq!(summary, "Used Chrome integration and ran 1 command");
            assert_eq!(*tool_surface, Some(ToolActivitySurface::Browser));
            assert_eq!(
                *tool_icon,
                Some(ToolActivityIcon::Website {
                    page_url: "https://www.mathworks.com/help/matlab/".into(),
                    favicon_url: None,
                })
            );
        }
        other => panic!("expected a work toggle, got {other:?}"),
    }
}

#[test]
fn keeps_a_large_expanded_tool_run_inside_one_timeline_item() {
    for is_working in [true, false] {
        let run_id = "turn-many-tools";
        let created_at = "2026-09-01T12:00:00Z";
        let timeline_entries: Vec<TimelineEntry> = (0..1_000)
            .map(|index| {
                let status = if is_working && index == 999 {
                    ToolLifecycleStatus::InProgress
                } else {
                    ToolLifecycleStatus::Completed
                };
                work_entry(
                    &format!("tool-entry-{index}"),
                    created_at,
                    WorkLogEntry {
                        tool_call_id: Some(format!("call-{index}")),
                        tool_lifecycle_status: Some(status),
                        ..work_log(
                            &format!("tool-{index}"),
                            created_at,
                            Some(run_id),
                            "t3-code.preview_snapshot",
                            WorkTone::Tool,
                        )
                    },
                )
            })
            .collect();
        let input = MessagesTimelineRowsInput {
            expanded_run_ids: set(&[run_id]),
            running_run_id: is_working.then(|| run_id.to_string()),
            active_turn_started_at: is_working.then(|| created_at.to_string()),
            ..rows_input(timeline_entries.clone(), is_working)
        };

        let collapsed = derive(&input);
        let group_id = collapsed
            .iter()
            .find_map(|row| match &row.content {
                Row::WorkToggle { group_id, .. } | Row::WorkLive { group_id, .. } => {
                    Some(group_id.clone())
                }
                _ => None,
            })
            .expect("a collapsed group");

        let expanded = derive(&MessagesTimelineRowsInput {
            expanded_work_group_ids: set(&[&group_id]),
            ..input.clone()
        });
        let groups: Vec<_> = expanded
            .iter()
            .filter(|row| row.kind() == MessagesTimelineRowKind::Work)
            .collect();
        assert_eq!(groups.len(), 1, "working {is_working}");
        let expected: Vec<String> = timeline_entries
            .iter()
            .filter_map(|entry| match &entry.kind {
                TimelineEntryKind::Work { entry } => Some(entry.id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(grouped_ids(&groups[0].content), expected);
        assert_eq!(groups[0].id(), format!("{group_id}:details"));
        assert!(
            !derive(&input)
                .iter()
                .any(|row| row.kind() == MessagesTimelineRowKind::Work)
        );
    }
}

#[test]
fn uses_the_final_call_for_tool_groups_that_recovered_or_failed() {
    // (recovered?, statuses, has_failure) per upstream's it.each table.
    let cases: [(&[&str], bool); 3] = [
        (&["failed", "completed"], false),
        (&["completed", "failed"], true),
        (&["failed", "failed"], true),
    ];
    for (statuses, has_failure) in cases {
        let timeline_entries: Vec<TimelineEntry> = statuses
            .iter()
            .enumerate()
            .map(|(index, status)| {
                let at = format!("2026-01-01T00:00:0{index}Z");
                work_entry(
                    &format!("work-entry-{index}"),
                    &at,
                    WorkLogEntry {
                        item_type: Some(TurnItemType::CommandExecution),
                        tool_lifecycle_status: Some(lifecycle(status)),
                        ..work_log(
                            &format!("work-{index}"),
                            &at,
                            None,
                            "Ran command",
                            WorkTone::Tool,
                        )
                    },
                )
            })
            .collect();
        let rows = derive(&rows_input(timeline_entries, false));

        match find_kind(&rows, MessagesTimelineRowKind::WorkToggle) {
            Some(Row::WorkToggle {
                hidden_count,
                has_failure: row_failure,
                ..
            }) => {
                assert_eq!(*hidden_count, 2, "{statuses:?}");
                assert_eq!(*row_failure, has_failure, "{statuses:?}");
            }
            other => panic!("{statuses:?}: expected a work toggle, got {other:?}"),
        }
    }
}

#[test]
fn uses_the_final_tool_call_for_mixed_work_groups() {
    // Upstream's it.each table: "info" is a status update, "error" an error-toned entry.
    let cases: [(&[&str], bool); 6] = [
        (&["failed", "completed", "info"], false),
        (&["failed", "info", "completed"], false),
        (&["error", "info", "completed"], false),
        (&["completed", "failed", "info"], true),
        (&["failed", "info", "failed"], true),
        (&["completed", "info", "failed"], true),
    ];
    for (statuses, has_failure) in cases {
        let timeline_entries: Vec<TimelineEntry> = statuses
            .iter()
            .enumerate()
            .map(|(index, status)| {
                let id = format!("work-{index}");
                let at = format!("2026-01-01T00:00:0{index}Z");
                let entry = match *status {
                    "info" => work_log(&id, &at, None, "Status updated", WorkTone::Info),
                    "error" => work_log(&id, &at, None, "Command failed", WorkTone::Error),
                    _ => WorkLogEntry {
                        tool_lifecycle_status: Some(lifecycle(status)),
                        ..work_log(&id, &at, None, "Ran command", WorkTone::Tool)
                    },
                };
                work_entry(&format!("work-entry-{index}"), &at, entry)
            })
            .collect();
        let has_error = statuses.contains(&"error");
        let rows = derive(&rows_input(timeline_entries, false));

        match find_kind(&rows, MessagesTimelineRowKind::WorkToggle) {
            Some(Row::WorkToggle {
                hidden_count,
                summary,
                has_failure: row_failure,
                ..
            }) => {
                assert_eq!(*hidden_count, if has_error { 2 } else { 3 }, "{statuses:?}");
                assert_eq!(
                    summary,
                    if has_error {
                        "Received 1 update and used 1 tool"
                    } else {
                        "Used 2 tools and received 1 update"
                    },
                    "{statuses:?}"
                );
                assert_eq!(*row_failure, has_failure, "{statuses:?}");
            }
            other => panic!("{statuses:?}: expected a work toggle, got {other:?}"),
        }
        if has_error {
            match &rows[0].content {
                Row::Work {
                    grouped_entries, ..
                } => {
                    assert_eq!(grouped_entries[0].tone, WorkTone::Error);
                    assert_eq!(grouped_entries[0].label, "Command failed");
                }
                other => panic!("expected the error entry first, got {other:?}"),
            }
        }
    }
}

/// `ToolLifecycleStatus` for a status name in the upstream tables.
fn lifecycle(status: &str) -> ToolLifecycleStatus {
    match status {
        "failed" => ToolLifecycleStatus::Failed,
        "completed" => ToolLifecycleStatus::Completed,
        "inProgress" => ToolLifecycleStatus::InProgress,
        other => panic!("unknown status {other}"),
    }
}
