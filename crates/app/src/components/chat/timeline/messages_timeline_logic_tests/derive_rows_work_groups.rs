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
