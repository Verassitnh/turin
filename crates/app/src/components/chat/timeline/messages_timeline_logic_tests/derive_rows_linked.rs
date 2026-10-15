//! Linked timeline resources: subagent cards and thread-created events are
//! separate from worklogs, and adjacent cards group without crossing a
//! resource or run boundary.

use super::*;

const RESOURCE_RUN: &str = "resource-run";
const EVENT_AT: &str = "2026-09-08T10:00:02Z";

/// A linked event (subagent card or thread creation) in `run`.
fn linked_event(id: &str, run: &str, payload: TurnItemPayload) -> TimelineEntry {
    event_entry(
        id,
        EVENT_AT,
        projected(
            0,
            TurnItem {
                run_id: Some(run.into()),
                ..turn_item(id, EVENT_AT, payload)
            },
        ),
    )
}

fn subagent_event(id: &str, run: &str) -> TimelineEntry {
    linked_event(id, run, subagent_payload(id, "app_owned", None))
}

fn thread_created_event(id: &str, run: &str) -> TimelineEntry {
    linked_event(
        id,
        run,
        TurnItemPayload::ThreadCreated {
            target_thread_id: format!("{id}-thread"),
            target_model: "gpt-5".into(),
        },
    )
}

#[test]
fn previews_a_thought_and_separates_subagent_cards_from_worklogs() {
    let rows = derive(&MessagesTimelineRowsInput {
        expanded_run_ids: set(&[RESOURCE_RUN]),
        ..rows_input(
            vec![
                work_entry(
                    "thought",
                    "2026-09-08T10:00:01Z",
                    WorkLogEntry {
                        item_type: Some(TurnItemType::Reasoning),
                        detail: Some("First paragraph.\n\nSecond paragraph.".into()),
                        ..work_log(
                            "thought",
                            "2026-09-08T10:00:01Z",
                            Some(RESOURCE_RUN),
                            "Thought",
                            WorkTone::Thinking,
                        )
                    },
                ),
                subagent_event("child", RESOURCE_RUN),
                message_entry(
                    "answer",
                    "2026-09-08T10:00:03Z",
                    assistant_message(
                        "answer",
                        "Done",
                        Some(RESOURCE_RUN),
                        "2026-09-08T10:00:03Z",
                        "2026-09-08T10:00:03Z",
                        false,
                    ),
                ),
            ],
            false,
        )
    });

    match find_kind(&rows, MessagesTimelineRowKind::Work) {
        Some(Row::Work { display_label, .. }) => {
            assert_eq!(
                display_label.as_deref(),
                Some("First paragraph. Second paragraph.")
            );
        }
        other => panic!("expected a work row, got {other:?}"),
    }
    let work_row = rows
        .iter()
        .find(|row| row.kind() == MessagesTimelineRowKind::Work)
        .expect("work row");
    assert!(!work_row.continues_work_log);
    let child_row = rows
        .iter()
        .find(|row| row.id() == "child")
        .expect("child row");
    assert!(!child_row.continues_work_log);
}

#[test]
fn groups_adjacent_subagents_without_merging_across_a_resource_or_run_boundary() {
    let rows = derive(&MessagesTimelineRowsInput {
        expanded_run_ids: set(&[RESOURCE_RUN, "other-run"]),
        ..rows_input(
            vec![
                subagent_event("a", RESOURCE_RUN),
                subagent_event("b", RESOURCE_RUN),
                thread_created_event("c", RESOURCE_RUN),
                subagent_event("d", RESOURCE_RUN),
                subagent_event("e", "other-run"),
            ],
            false,
        )
    });

    assert_eq!(
        ids(&rows),
        vec![
            "turn-fold:resource-run",
            "a",
            "c",
            "d",
            "turn-fold:other-run",
            "e",
        ]
    );
    match &rows[1].content {
        Row::Event {
            subagents: Some(subagents),
            ..
        } => {
            let grouped: Vec<&str> = subagents.iter().map(|item| item.item.id.as_str()).collect();
            assert_eq!(grouped, vec!["a", "b"]);
        }
        other => panic!("expected a grouped subagent row, got {other:?}"),
    }
}
