//! `deriveMessagesTimelineRows` cases about context compaction: the row stays
//! visible outside folded work, and a live compaction takes the activity slot
//! until it completes.

use super::*;

#[test]
fn keeps_context_compaction_visible_outside_folded_work() {
    let rows = derive(&rows_input(
        vec![work_entry(
            "compaction-entry",
            "2026-01-01T00:00:00Z",
            WorkLogEntry {
                source_activity_kind: Some("context-compaction".into()),
                ..work_log(
                    "compaction",
                    "2026-01-01T00:00:00Z",
                    None,
                    "Compacted context 899K → 19K tokens",
                    WorkTone::Info,
                )
            },
        )],
        false,
    ));

    assert_eq!(ids(&rows), vec!["compaction-entry"]);
    match &rows[0].content {
        Row::ContextCompaction {
            created_at,
            label,
            active,
            ..
        } => {
            assert_eq!(created_at, "2026-01-01T00:00:00Z");
            assert_eq!(label, "Compacted context 899K → 19K tokens");
            assert!(!active);
        }
        other => panic!("expected a compaction row, got {other:?}"),
    }
}

#[test]
fn gives_live_compaction_the_activity_slot_and_restores_thinking_when_it_completes() {
    let fixture = make_streaming_timeline_fixture("");
    let mut visible = fixture.visible_turn_items.clone();
    let last = visible.pop().expect("fixture items");
    assert!(matches!(
        last.item.payload,
        TurnItemPayload::AssistantMessage { .. }
    ));
    let compaction = |status: TurnItemStatus, after: Option<u64>| {
        let mut compact = last.clone();
        compact.item.status = status;
        compact.item.payload = TurnItemPayload::Compaction {
            summary: None,
            before_token_count: Some(899_000),
            after_token_count: after,
        };
        compact
    };
    let with_compaction = |item: ProjectedTurnItem| {
        let mut items = visible.clone();
        items.push(item);
        entries_from(items)
    };
    let input = MessagesTimelineRowsInput {
        is_working: true,
        running_run_id: Some(fixture.run_id.clone()),
        active_turn_started_at: Some(fixture.time(5)),
        ..Default::default()
    };

    let running_entries = with_compaction(compaction(TurnItemStatus::Running, None));
    let running_rows = derive(&MessagesTimelineRowsInput {
        timeline_entries: Arc::new(running_entries.clone()),
        ..input.clone()
    });
    match find_kind(&running_rows, MessagesTimelineRowKind::ContextCompaction) {
        Some(Row::ContextCompaction { active, label, .. }) => {
            assert!(active);
            assert_eq!(label, "Compacting context");
        }
        other => panic!("expected a live compaction row, got {other:?}"),
    }
    assert!(!has_kind(&running_rows, MessagesTimelineRowKind::Thinking));

    let completed_rows = derive(&MessagesTimelineRowsInput {
        timeline_entries: Arc::new(with_compaction(compaction(
            TurnItemStatus::Completed,
            Some(19_000),
        ))),
        ..input.clone()
    });
    match find_kind(&completed_rows, MessagesTimelineRowKind::ContextCompaction) {
        Some(Row::ContextCompaction { active, label, .. }) => {
            assert!(!active);
            assert_eq!(label, "Context compacted 899K → 19K tokens");
        }
        other => panic!("expected a completed compaction row, got {other:?}"),
    }
    assert_eq!(
        completed_rows.last().map(|row| row.kind()),
        Some(MessagesTimelineRowKind::Thinking)
    );

    let settled_rows = derive(&MessagesTimelineRowsInput {
        is_working: false,
        timeline_entries: Arc::new(running_entries.clone()),
        ..input.clone()
    });
    match find_kind(&settled_rows, MessagesTimelineRowKind::ContextCompaction) {
        Some(Row::ContextCompaction { active, .. }) => assert!(!active),
        other => panic!("expected a settled compaction row, got {other:?}"),
    }

    let stable = compute_stable_messages_timeline_rows(
        running_rows.clone(),
        &StableMessagesTimelineRowsState::default(),
    );
    let next = compute_stable_messages_timeline_rows(settled_rows.clone(), &stable);
    assert_eq!(*next.result, settled_rows);
}
