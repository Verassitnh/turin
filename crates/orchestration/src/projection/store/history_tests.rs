//! Timeline reads (`getTimelinePage`, `getMessageCount`, `getNextTurnItemOrdinal`,
//! `getThreadAttachmentIds`) against projection rows written through the apply path.

use serde_json::json;
use turin_contracts::{ThreadId, TurnItemId};

use turin_contracts::orchestration_v2::OrchestrationV2ProjectedTurnItem;

use crate::projection::store::ProjectionStore;
use crate::projection::store::history::{TimelinePageOptions, TimelineView};
use crate::projection::test_support::{
    apply_all, assistant_item, event, file_store, message, run, thread, user_message_item,
};
use crate::projection::thread_history_paging::encode_thread_history_cursor;

const T0: &str = "2026-01-01T00:00:01.000Z";

fn tid(id: &str) -> ThreadId {
    ThreadId::new(id).expect("thread id")
}

async fn seeded_thread(name: &str) -> (ProjectionStore, tempfile::TempDir) {
    let (store, dir) = file_store().await;
    apply_all(
        &store,
        vec![
            event("thread.created", name, T0, thread(name)),
            event(
                "run.created",
                name,
                T0,
                run(name, "run:history", 1, "completed"),
            ),
            event(
                "turn-item.updated",
                name,
                T0,
                user_message_item(name, "item:ask", "run:history", 1, "hello", "turn_start"),
            ),
            event(
                "turn-item.updated",
                name,
                T0,
                assistant_item(name, "item:answer", "run:history", 2, "hi"),
            ),
        ],
    )
    .await;
    (store, dir)
}

#[tokio::test]
async fn timeline_page_walks_message_rows_by_position_and_reports_more() {
    let (store, _dir) = seeded_thread("thread:timeline").await;
    let thread_id = tid("thread:timeline");

    let first = store
        .get_timeline_page(
            &thread_id,
            TimelinePageOptions {
                after_position: None,
                item_id: None,
                view: TimelineView::Messages,
                limit: 1,
            },
        )
        .await
        .expect("first page");
    let second = store
        .get_timeline_page(
            &thread_id,
            TimelinePageOptions {
                after_position: Some(0),
                item_id: None,
                view: TimelineView::Messages,
                limit: 10,
            },
        )
        .await
        .expect("second page");

    assert_eq!(first.total_items, 2);
    assert!(first.has_more);
    assert_eq!(
        first
            .items
            .iter()
            .map(|row| row.source_item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["item:ask"]
    );
    assert_eq!(
        second
            .items
            .iter()
            .map(|row| row.source_item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["item:answer"]
    );
    assert!(!second.has_more);
}

#[tokio::test]
async fn timeline_page_by_item_id_returns_only_that_item() {
    let (store, _dir) = seeded_thread("thread:timeline-item").await;

    let page = store
        .get_timeline_page(
            &tid("thread:timeline-item"),
            TimelinePageOptions {
                after_position: None,
                item_id: Some(TurnItemId::new("item:answer").unwrap()),
                view: TimelineView::Activity,
                limit: 10,
            },
        )
        .await
        .expect("item page");

    assert_eq!(
        page.items
            .iter()
            .map(|row| row.source_item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["item:answer"]
    );
    assert!(!page.has_more);
}

#[tokio::test]
async fn next_turn_item_ordinal_follows_the_highest_stored_ordinal() {
    let (store, _dir) = seeded_thread("thread:ordinal").await;

    let next = store
        .get_next_turn_item_ordinal(&tid("thread:ordinal"))
        .await
        .expect("next ordinal");

    assert_eq!(next, 3);
}

#[tokio::test]
async fn message_count_and_attachment_ids_read_the_message_rows() {
    let (store, _dir) = file_store().await;
    let name = "thread:messages";
    let mut with_attachment = message(
        name,
        "message:attached",
        Some("run:messages"),
        "user",
        "see file",
        T0,
    );
    with_attachment["attachments"] = json!([{
        "type": "image",
        "id": "attachment-one",
        "name": "shot.png",
        "mimeType": "image/png",
        "sizeBytes": 4
    }]);
    apply_all(
        &store,
        vec![
            event("thread.created", name, T0, thread(name)),
            event(
                "message.updated",
                name,
                T0,
                message(name, "message:plain", None, "assistant", "hi", T0),
            ),
            event("message.updated", name, T0, with_attachment),
        ],
    )
    .await;
    let thread_id = tid(name);

    let count = store.get_message_count(&thread_id).await.expect("count");
    let attachments = store
        .get_thread_attachment_ids(&thread_id)
        .await
        .expect("attachments");

    assert_eq!(count, 2);
    assert_eq!(attachments, vec!["attachment-one".to_owned()]);
}

/// `seeded_thread` plus a second turn, so cursors have something before them.
async fn seeded_two_turns(name: &str) -> (ProjectionStore, tempfile::TempDir) {
    let (store, dir) = seeded_thread(name).await;
    apply_all(
        &store,
        vec![
            event(
                "run.created",
                name,
                T0,
                run(name, "run:second", 2, "completed"),
            ),
            event(
                "turn-item.updated",
                name,
                T0,
                user_message_item(name, "item:ask-two", "run:second", 3, "again", "turn_start"),
            ),
            event(
                "turn-item.updated",
                name,
                T0,
                assistant_item(name, "item:answer-two", "run:second", 4, "ok"),
            ),
        ],
    )
    .await;
    (store, dir)
}

fn item_ids(rows: &[OrchestrationV2ProjectedTurnItem]) -> Vec<String> {
    rows.iter()
        .map(|row| row.source_item_id.as_str().to_owned())
        .collect()
}

#[tokio::test]
async fn history_cursor_returns_the_items_before_its_anchor() {
    let (store, _dir) = seeded_two_turns("thread:cursor").await;
    let thread_id = tid("thread:cursor");
    // Anchor on the final answer: the page holds everything before it.
    let cursor = encode_thread_history_cursor(0, "thread:cursor", "item:answer-two", 3);

    let page = store
        .get_thread_history_page(&thread_id, &cursor, None, false)
        .await
        .expect("history page");

    assert_eq!(
        item_ids(&page.items),
        vec!["item:ask", "item:answer", "item:ask-two"]
    );
    assert!(!page.has_more_history);
    assert!(page.next_cursor.is_none());
    assert_eq!(
        page.items
            .iter()
            .map(|row| row.position)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
}

#[tokio::test]
async fn history_cursor_rejects_a_malformed_cursor() {
    let (store, _dir) = seeded_two_turns("thread:bad-cursor").await;

    let error = store
        .get_thread_history_page(&tid("thread:bad-cursor"), "not-a-cursor", None, false)
        .await
        .expect_err("malformed cursor");

    assert!(matches!(
        error,
        crate::projection::error::ProjectionStoreError::Read { .. }
    ));
}

#[tokio::test]
async fn timeline_pages_walk_every_visible_row_once_by_position() {
    let (store, _dir) = seeded_two_turns("thread:walk").await;
    let thread_id = tid("thread:walk");

    let mut seen: Vec<String> = Vec::new();
    let mut after = None;
    loop {
        let page = store
            .get_timeline_page(
                &thread_id,
                TimelinePageOptions {
                    after_position: after,
                    item_id: None,
                    view: TimelineView::Activity,
                    limit: 1,
                },
            )
            .await
            .expect("timeline page");
        assert_eq!(page.total_items, 4);
        seen.extend(item_ids(&page.items));
        match page.items.last() {
            Some(row) if page.has_more => after = Some(row.position),
            _ => break,
        }
    }

    assert_eq!(
        seen,
        vec!["item:ask", "item:answer", "item:ask-two", "item:answer-two"]
    );
}

#[tokio::test]
async fn history_cursor_chain_reassembles_every_older_item_across_pages() {
    // Older-history pages hold 20 user turns, so 24 turns need a second cursor.
    let (store, _dir) = file_store().await;
    let name = "thread:paged";
    let mut events = vec![event("thread.created", name, T0, thread(name))];
    for turn in 1..=24u64 {
        let run_id = format!("run:paged-{turn}");
        events.push(event(
            "run.created",
            name,
            T0,
            run(name, &run_id, turn, "completed"),
        ));
        events.push(event(
            "turn-item.updated",
            name,
            T0,
            user_message_item(
                name,
                &format!("item:ask-{turn}"),
                &run_id,
                turn * 2 - 1,
                "question",
                "turn_start",
            ),
        ));
        events.push(event(
            "turn-item.updated",
            name,
            T0,
            assistant_item(
                name,
                &format!("item:answer-{turn}"),
                &run_id,
                turn * 2,
                "answer",
            ),
        ));
    }
    apply_all(&store, events).await;
    let thread_id = tid(name);

    // Anchor on the final answer; everything before it is history.
    let mut cursor = encode_thread_history_cursor(0, name, "item:answer-24", 47);
    let mut pages = Vec::new();
    loop {
        let page = store
            .get_thread_history_page(&thread_id, &cursor, None, false)
            .await
            .expect("history page");
        let next = page.next_cursor.clone();
        pages.push(item_ids(&page.items));
        match next {
            Some(next) if page.has_more_history => cursor = next.as_str().to_owned(),
            _ => break,
        }
    }

    // Each page is older than the one before it, so reverse for chronological order.
    let reassembled: Vec<String> = pages.iter().rev().flatten().cloned().collect();
    let expected: Vec<String> = (1..=24u64)
        .flat_map(|turn| [format!("item:ask-{turn}"), format!("item:answer-{turn}")])
        .filter(|id| id != "item:answer-24")
        .collect();
    assert!(
        pages.len() >= 2,
        "expected a second page, got {}",
        pages.len()
    );
    assert_eq!(reassembled, expected);
}
