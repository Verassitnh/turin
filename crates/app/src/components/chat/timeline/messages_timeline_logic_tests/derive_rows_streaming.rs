//! Streaming v2 row projection: the stateful derivation must match a fresh
//! derivation at every step, and rows that did not change keep their identity.

use super::*;
use crate::components::chat::timeline::messages_timeline_logic::derive_messages_timeline_rows_with_state;
use crate::components::chat::timeline::session_logic::{
    TimelineEntriesProjection, derive_timeline_entries_from_visible_turn_items_with_state,
};

struct StreamingState {
    source: StreamingTimelineFixture,
    timeline_input: TimelineEntriesInput,
    timeline: TimelineEntriesProjection,
    input: MessagesTimelineRowsInput,
}

/// `fixture(text)`: the streaming fixture with a running turn and a history
/// turn that has a checkpoint.
fn streaming_state(text: &str) -> StreamingState {
    let source = make_streaming_timeline_fixture(text);
    let timeline_input = source.entries_input();
    let timeline =
        derive_timeline_entries_from_visible_turn_items_with_state(timeline_input.clone(), None);
    let input = MessagesTimelineRowsInput {
        timeline_entries: Arc::new(timeline.entries.clone()),
        latest_run: Some(latest_run(
            &source.run_id,
            RunStatus::Running,
            Some(&source.time(5)),
            None,
        )),
        running_run_id: Some(source.run_id.clone()),
        is_working: true,
        active_turn_started_at: Some(source.time(5)),
        turn_diff_summaries: vec![TurnDiffSummary {
            checkpoint_id: None,
            scope_id: None,
            run_id: source.history_run_id.clone(),
            checkpoint_turn_count: 1,
            checkpoint_ref: "refs/t3/checkpoints/history".into(),
            status: CheckpointStatus::Ready,
            files: Vec::new(),
            assistant_message_id: Some("history-assistant".into()),
            completed_at: source.time(4),
        }],
        supports_conversation_rollback: true,
        ..Default::default()
    };
    StreamingState {
        source,
        timeline_input,
        timeline,
        input,
    }
}

/// Upstream `updateText`: rewrites the streaming assistant items' text.
fn update_text(items: &[ProjectedTurnItem], text: &str) -> Vec<ProjectedTurnItem> {
    items
        .iter()
        .map(|row| match &row.item.payload {
            TurnItemPayload::AssistantMessage {
                streaming: true, ..
            } => {
                let mut row = row.clone();
                if let TurnItemPayload::AssistantMessage { text: current, .. } =
                    &mut row.item.payload
                {
                    *current = text.to_string();
                }
                row.item.updated_at = "2026-09-04T00:00:08.000Z".into();
                row
            }
            _ => row.clone(),
        })
        .collect()
}

/// The live assistant message's `(text, projected item)` in a row list.
fn live_message(rows: &[Arc<MessagesTimelineRow>]) -> Option<(&str, Option<&ProjectedTurnItem>)> {
    rows.iter().find_map(|row| match &row.content {
        Row::Message {
            message,
            projected_item,
            ..
        } if message.id == "live-assistant" => {
            Some((message.text.as_str(), projected_item.as_ref()))
        }
        _ => None,
    })
}

#[test]
fn keeps_row_parity_and_history_identity_as_the_live_text_changes() {
    let cases = [
        ("", "Now visible"),
        (" \n", "Now visible"),
        ("Visible", ""),
        ("", " \t\n"),
        ("Visible", "★ Insight\nKeep these line breaks"),
    ];
    for (before, after) in cases {
        let initial = streaming_state(before);
        let previous = derive_messages_timeline_rows_with_state(initial.input.clone(), None);
        let timeline_input = TimelineEntriesInput {
            visible_turn_items: update_text(&initial.source.visible_turn_items, after),
            ..initial.timeline_input.clone()
        };
        let timeline = derive_timeline_entries_from_visible_turn_items_with_state(
            timeline_input.clone(),
            Some(&initial.timeline),
        );
        // The v2 selectors recreate the run's summaries on every projection
        // event, even when their contents did not change.
        let input = MessagesTimelineRowsInput {
            timeline_entries: Arc::new(timeline.entries.clone()),
            ..initial.input.clone()
        };
        let next = derive_messages_timeline_rows_with_state(input.clone(), Some(&previous));
        let fresh = derive(&MessagesTimelineRowsInput {
            timeline_entries: Arc::new(derive_timeline_entries_from_visible_turn_items(
                &timeline_input,
            )),
            ..input.clone()
        });
        assert_eq!(next.rows, fresh, "{before:?} → {after:?}");

        for (index, row) in previous.rows.iter().enumerate() {
            let is_live = matches!(
                &row.content,
                Row::Message { message, .. } | Row::AssistantMeta { message, .. }
                    if message.id == "live-assistant"
            );
            if is_live {
                let (text, projected) = live_message(&next.rows).expect("live message");
                assert_eq!(text, after, "{before:?} → {after:?}");
                assert_eq!(projected, timeline_input.visible_turn_items.last());
                assert_eq!(
                    row_message_text(row),
                    Some(before),
                    "previous rows are unchanged"
                );
            } else {
                assert!(
                    Arc::ptr_eq(&next.rows[index], row),
                    "row {index} should keep its identity"
                );
            }
        }
    }
}

fn row_message_text(row: &MessagesTimelineRow) -> Option<&str> {
    match &row.content {
        Row::Message { message, .. } | Row::AssistantMeta { message, .. } => {
            Some(message.text.as_str())
        }
        _ => None,
    }
}

#[test]
fn leaves_emitted_rows_immutable_when_a_projection_branches() {
    let initial = streaming_state("Initial");
    let previous = derive_messages_timeline_rows_with_state(initial.input.clone(), None);
    let branch = |text: &str| {
        let timeline_input = TimelineEntriesInput {
            visible_turn_items: update_text(&initial.source.visible_turn_items, text),
            ..initial.timeline_input.clone()
        };
        let timeline = derive_timeline_entries_from_visible_turn_items_with_state(
            timeline_input,
            Some(&initial.timeline),
        );
        let input = MessagesTimelineRowsInput {
            timeline_entries: Arc::new(timeline.entries),
            ..initial.input.clone()
        };
        let next = derive_messages_timeline_rows_with_state(input.clone(), Some(&previous));
        assert_eq!(next.rows, derive(&input));
        next
    };

    let first = branch("First");
    branch("Second");
    assert_eq!(
        live_message(&first.rows).map(|(text, _)| text),
        Some("First")
    );
    assert_eq!(
        live_message(&previous.rows).map(|(text, _)| text),
        Some("Initial")
    );
}

/// Upstream `check` in "rebuilds for completion, late failure, …": each step
/// derives incrementally from the previous projection and must equal a fresh
/// derivation.
struct Rebuild {
    timeline: TimelineEntriesProjection,
    timeline_input: TimelineEntriesInput,
    input: MessagesTimelineRowsInput,
    projection: MessagesTimelineRowsProjection,
}

impl Rebuild {
    fn check(&mut self, change: impl FnOnce(&mut MessagesTimelineRowsInput)) {
        let previous = self.projection.clone();
        self.timeline = derive_timeline_entries_from_visible_turn_items_with_state(
            self.timeline_input.clone(),
            Some(&self.timeline),
        );
        change(&mut self.input);
        self.input.timeline_entries = Arc::new(self.timeline.entries.clone());
        self.projection =
            derive_messages_timeline_rows_with_state(self.input.clone(), Some(&previous));
        let fresh = derive(&MessagesTimelineRowsInput {
            timeline_entries: Arc::new(derive_timeline_entries_from_visible_turn_items(
                &self.timeline_input,
            )),
            ..self.input.clone()
        });
        assert_eq!(self.projection.rows, fresh);
        assert_eq!(previous.rows, derive(&previous.input));
    }
}

#[test]
fn rebuilds_for_completion_late_failure_metadata_expansion_and_older_history() {
    let initial = streaming_state("Partial");
    let source = &initial.source;
    let mut rebuild = Rebuild {
        timeline: initial.timeline.clone(),
        timeline_input: initial.timeline_input.clone(),
        input: initial.input.clone(),
        projection: derive_messages_timeline_rows_with_state(initial.input.clone(), None),
    };
    let set_visible = |rebuild: &mut Rebuild, items: Vec<ProjectedTurnItem>| {
        rebuild.timeline_input.visible_turn_items = items;
    };

    // The streaming reply completes.
    let completed_items: Vec<ProjectedTurnItem> = rebuild
        .timeline_input
        .visible_turn_items
        .iter()
        .map(|row| match &row.item.payload {
            TurnItemPayload::AssistantMessage {
                streaming: true, ..
            } => {
                let mut row = row.clone();
                row.item.status = TurnItemStatus::Completed;
                row.item.updated_at = source.time(9);
                row.item.payload = assistant_message_payload("live-assistant", "Complete", false);
                row
            }
            _ => row.clone(),
        })
        .collect();
    set_visible(&mut rebuild, completed_items);
    rebuild.check(|input| {
        input.latest_run = Some(latest_run(
            &source.run_id,
            RunStatus::Completed,
            Some(&source.time(5)),
            Some(&source.time(9)),
        ));
        input.running_run_id = None;
        input.is_working = false;
    });

    // A failure arrives late, after the completed work.
    let late_failure = rebuild.timeline_input.visible_turn_items[1].clone();
    let mut items = rebuild.timeline_input.visible_turn_items.clone();
    items.push(ProjectedTurnItem {
        position: 6,
        source_item_id: "late-failure".into(),
        item: TurnItem {
            id: "late-failure".into(),
            run_id: Some(source.run_id.clone()),
            ordinal: 10,
            started_at: Some(source.time(10)),
            ..late_failure.item.clone()
        },
        ..late_failure
    });
    set_visible(&mut rebuild, items);
    rebuild.check(|_| {});

    // The live message is corrected (metadata changes with its text).
    let mut items = rebuild.timeline_input.visible_turn_items.clone();
    for row in &mut items {
        if let TurnItemPayload::AssistantMessage {
            message_id, text, ..
        } = &mut row.item.payload
            && message_id == "live-assistant"
        {
            *text = "Corrected final text".into();
            row.item.updated_at = source.time(11);
        }
    }
    set_visible(&mut rebuild, items);
    rebuild.check(|_| {});

    rebuild.check(|input| {
        input.latest_run = Some(latest_run(
            &source.run_id,
            RunStatus::Interrupted,
            Some(&source.time(5)),
            Some(&source.time(12)),
        ));
    });

    rebuild.check(|input| {
        input.expanded_run_ids = set(&[&source.history_run_id, &source.run_id]);
    });
    let group_id = rebuild
        .projection
        .rows
        .iter()
        .find_map(|row| match &row.content {
            Row::WorkToggle { group_id, .. } => Some(group_id.clone()),
            _ => None,
        });
    rebuild.check(|input| {
        input.expanded_work_group_ids = group_id.iter().cloned().collect();
    });

    // An older user prompt is prepended and positions are renumbered.
    let first = rebuild.timeline_input.visible_turn_items[0].clone();
    let mut items = vec![ProjectedTurnItem {
        source_item_id: "older-user-item".into(),
        item: TurnItem {
            id: "older-user-item".into(),
            started_at: Some("2026-09-03T23:59:00.000Z".into()),
            payload: user_message_payload("older-user", "Prompt"),
            ..first.item.clone()
        },
        ..first.clone()
    }];
    items.extend(rebuild.timeline_input.visible_turn_items.clone());
    for (position, row) in items.iter_mut().enumerate() {
        row.position = position as u64;
    }
    set_visible(&mut rebuild, items);
    rebuild.check(|_| {});

    rebuild.check(|input| {
        input.supports_conversation_rollback = false;
    });
    rebuild.check(|input| {
        for summary in &mut input.turn_diff_summaries {
            summary.status = CheckpointStatus::Stale;
        }
    });
}
