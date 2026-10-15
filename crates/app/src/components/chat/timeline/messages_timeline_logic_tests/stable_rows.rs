//! `computeStableMessagesTimelineRows` from MessagesTimeline.logic.test.ts.

use super::*;
use crate::components::chat::timeline::work_log_presentation::{
    ToolGroupAction, ToolGroupSummaryKind,
};

fn toggle_row(tool_icon: Option<ToolActivityIcon>) -> Arc<MessagesTimelineRow> {
    Arc::new(
        Row::WorkToggle {
            id: "work-toggle:1".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            run_id: None,
            group_id: "work-group:1".into(),
            hidden_count: 1,
            expanded: false,
            summary: "Used Browser".into(),
            summary_kind: ToolGroupSummaryKind::Action(ToolGroupAction::Other),
            tool_surface: Some(ToolActivitySurface::Browser),
            tool_icon,
            summary_tool_icon: None,
            has_failure: false,
        }
        .into(),
    )
}

#[test]
fn replaces_a_cached_work_toggle_when_its_icon_presentation_changes() {
    let initial =
        compute_stable_messages_timeline_rows(vec![toggle_row(None)], &Default::default());
    let enriched = toggle_row(Some(ToolActivityIcon::Website {
        page_url: "https://example.com".into(),
        favicon_url: None,
    }));
    let updated = compute_stable_messages_timeline_rows(vec![enriched.clone()], &initial);
    assert!(!Arc::ptr_eq(&updated.result, &initial.result));
    assert!(Arc::ptr_eq(&updated.result[0], &enriched));
}

#[test]
fn keeps_thinking_after_assistant_content_grows() {
    for text in ["", " \n"] {
        let started_at = "2026-01-01T00:00:00Z";
        let input = |text: &str| MessagesTimelineRowsInput {
            running_run_id: Some("turn-1".into()),
            active_turn_started_at: Some(started_at.into()),
            ..rows_input(
                vec![message_entry(
                    "assistant-entry",
                    started_at,
                    assistant_message(
                        "assistant-1",
                        text,
                        Some("turn-1"),
                        started_at,
                        started_at,
                        true,
                    ),
                )],
                true,
            )
        };
        let initial =
            compute_stable_messages_timeline_rows(derive(&input(text)), &Default::default());
        let updated = compute_stable_messages_timeline_rows(
            derive(&input("I will inspect the repository.")),
            &initial,
        );
        let initial_thinking = initial.by_id.get("live-activity-row").cloned();
        let updated_thinking = updated.by_id.get("live-activity-row").cloned();
        assert!(matches!(
            initial_thinking.as_deref().map(|row| &row.content),
            Some(Row::Thinking { .. })
        ));
        assert!(Arc::ptr_eq(
            initial_thinking.as_ref().unwrap(),
            updated_thinking.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            updated.result.last().unwrap(),
            updated_thinking.as_ref().unwrap()
        ));
    }
}

#[test]
fn returns_the_previous_result_when_rows_are_unchanged() {
    let entries = vec![
        message_entry(
            "entry-user-1",
            "2026-01-01T00:00:00Z",
            user_message("user-1", "First", None, "2026-01-01T00:00:00Z"),
        ),
        message_entry(
            "entry-user-2",
            "2026-01-01T00:00:10Z",
            user_message("user-2", "Second", None, "2026-01-01T00:00:10Z"),
        ),
    ];
    let rows = derive(&rows_input(entries.clone(), false));
    let initial = compute_stable_messages_timeline_rows(rows, &Default::default());
    let again =
        compute_stable_messages_timeline_rows(derive(&rows_input(entries, false)), &initial);
    assert!(Arc::ptr_eq(&again.result, &initial.result));
}

#[test]
fn reuses_work_rows_when_equivalent_timeline_derivations_create_new_grouped_arrays() {
    let first_work = WorkLogEntry {
        detail: Some("Inspecting repository state".into()),
        ..work_log(
            "work-1",
            "2026-01-01T00:00:00Z",
            None,
            "thinking",
            WorkTone::Thinking,
        )
    };
    let second_work = WorkLogEntry {
        detail: Some("Reading package.json".into()),
        ..work_log(
            "work-2",
            "2026-01-01T00:00:01Z",
            None,
            "read",
            WorkTone::Tool,
        )
    };
    let create_rows = || {
        derive(&rows_input(
            vec![
                work_entry("entry-work-1", "2026-01-01T00:00:00Z", first_work.clone()),
                work_entry("entry-work-2", "2026-01-01T00:00:01Z", second_work.clone()),
            ],
            false,
        ))
    };

    let first_rows = create_rows();
    let initial = compute_stable_messages_timeline_rows(first_rows.clone(), &Default::default());
    let second_rows = create_rows();
    assert!(!Arc::ptr_eq(&second_rows[0], &first_rows[0]));

    let repeated = compute_stable_messages_timeline_rows(second_rows, &initial);
    assert!(Arc::ptr_eq(&repeated.result, &initial.result));
    assert!(Arc::ptr_eq(&repeated.result[0], &initial.result[0]));
}

#[test]
fn returns_a_new_result_when_row_order_changes_without_content_changes() {
    let rows = derive(&rows_input(
        vec![
            message_entry(
                "entry-user-1",
                "2026-01-01T00:00:00Z",
                user_message("user-1", "First", None, "2026-01-01T00:00:00Z"),
            ),
            message_entry(
                "entry-user-2",
                "2026-01-01T00:00:10Z",
                user_message("user-2", "Second", None, "2026-01-01T00:00:10Z"),
            ),
        ],
        false,
    ));
    let initial = compute_stable_messages_timeline_rows(rows.clone(), &Default::default());

    let reordered =
        compute_stable_messages_timeline_rows(vec![rows[1].clone(), rows[0].clone()], &initial);

    assert!(!Arc::ptr_eq(&reordered.result, &initial.result));
    assert_eq!(
        *reordered.result,
        vec![initial.result[1].clone(), initial.result[0].clone()]
    );
}
