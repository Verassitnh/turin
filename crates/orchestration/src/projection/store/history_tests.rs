//! Timeline reads (`getTimelinePage`, `getMessageCount`, `getNextTurnItemOrdinal`,
//! `getThreadAttachmentIds`) against projection rows written through the apply path.

use serde_json::json;
use turin_contracts::{ThreadId, TurnItemId};

use crate::projection::store::ProjectionStore;
use crate::projection::store::history::{TimelinePageOptions, TimelineView};
use crate::projection::test_support::{
    apply_all, assistant_item, event, file_store, message, run, thread, user_message_item,
};

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
