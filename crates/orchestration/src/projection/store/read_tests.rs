//! `getTurnItem`, `getTurnStartHistory`, and `getRecoveryThreadIds` against
//! projection rows written through the real apply path.

use serde_json::json;
use turin_contracts::orchestration_v2::OrchestrationV2TurnItem;
use turin_contracts::{RunId, ThreadId, TurnItemId};

use crate::projection::store::recovery::ProjectionRecoveryKind;
use crate::projection::test_support::{
    apply_all, assistant_item, event, file_store, run, thread, user_message_item,
};
use crate::projection::turn_item;

use super::ProjectionStore;

const T0: &str = "2026-01-01T00:00:01.000Z";

fn tid(id: &str) -> ThreadId {
    ThreadId::new(id).expect("thread id")
}

fn run_id(id: &str) -> RunId {
    RunId::new(id).expect("run id")
}

async fn create_thread(store: &ProjectionStore, id: &str) {
    apply_all(store, vec![event("thread.created", id, T0, thread(id))]).await;
}

async fn create_run(
    store: &ProjectionStore,
    thread_id: &str,
    id: &str,
    ordinal: u64,
    status: &str,
) {
    apply_all(
        store,
        vec![event(
            "run.created",
            thread_id,
            T0,
            run(thread_id, id, ordinal, status),
        )],
    )
    .await;
}

#[tokio::test]
async fn get_turn_item_returns_the_stored_item_and_none_for_unknown_ids() {
    let (store, _dir) = file_store().await;
    create_thread(&store, "thread:turn-item").await;
    apply_all(
        &store,
        vec![event(
            "turn-item.updated",
            "thread:turn-item",
            T0,
            assistant_item("thread:turn-item", "item:answer", "run:one", 1, "hello"),
        )],
    )
    .await;
    let thread_id = tid("thread:turn-item");

    let found = store
        .get_turn_item(&thread_id, &TurnItemId::new("item:answer").unwrap())
        .await
        .expect("read item");
    let missing = store
        .get_turn_item(&thread_id, &TurnItemId::new("item:missing").unwrap())
        .await
        .expect("read missing item");

    assert_eq!(
        found.map(|item| turn_item::common(&item).id.as_str().to_owned()),
        Some("item:answer".to_owned())
    );
    assert!(missing.is_none());
}

#[tokio::test]
async fn turn_start_history_keeps_visible_item_types_and_honours_run_filters() {
    let (store, _dir) = file_store().await;
    create_thread(&store, "thread:history").await;
    apply_all(
        &store,
        vec![
            event(
                "turn-item.updated",
                "thread:history",
                T0,
                user_message_item(
                    "thread:history",
                    "item:ask-one",
                    "run:one",
                    1,
                    "first",
                    "turn_start",
                ),
            ),
            event(
                "turn-item.updated",
                "thread:history",
                T0,
                assistant_item("thread:history", "item:answer-one", "run:one", 2, "one"),
            ),
            event(
                "turn-item.updated",
                "thread:history",
                T0,
                assistant_item("thread:history", "item:answer-two", "run:two", 3, "two"),
            ),
        ],
    )
    .await;
    let thread_id = tid("thread:history");

    let all = store
        .get_turn_start_history(&thread_id, None)
        .await
        .expect("all runs");
    let only_two = store
        .get_turn_start_history(&thread_id, Some(&[run_id("run:two")]))
        .await
        .expect("one run");
    let none = store
        .get_turn_start_history(&thread_id, Some(&[]))
        .await
        .expect("no runs");

    let ids = |items: &[OrchestrationV2TurnItem]| {
        items
            .iter()
            .map(|item| turn_item::common(item).id.as_str().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids(&all),
        vec!["item:ask-one", "item:answer-one", "item:answer-two"]
    );
    assert_eq!(ids(&only_two), vec!["item:answer-two"]);
    assert!(none.is_empty());
}

#[tokio::test]
async fn queued_run_recovery_selects_unarchived_threads_without_an_active_run() {
    let (store, _dir) = file_store().await;
    create_thread(&store, "thread:queued").await;
    create_run(&store, "thread:queued", "run:queued", 1, "queued").await;

    create_thread(&store, "thread:busy").await;
    create_run(&store, "thread:busy", "run:busy-queued", 1, "queued").await;
    create_run(&store, "thread:busy", "run:busy-running", 2, "running").await;

    let mut archived = thread("thread:archived");
    archived["archivedAt"] = json!(T0);
    apply_all(
        &store,
        vec![
            event("thread.created", "thread:archived", T0, archived),
            event(
                "run.created",
                "thread:archived",
                T0,
                run("thread:archived", "run:archived", 1, "queued"),
            ),
        ],
    )
    .await;

    let selected = store
        .get_recovery_thread_ids(ProjectionRecoveryKind::QueuedRuns)
        .await
        .expect("select queued");

    assert_eq!(selected, vec![tid("thread:queued")]);
}

#[tokio::test]
async fn runtime_recovery_selects_threads_with_a_running_run() {
    let (store, _dir) = file_store().await;
    create_thread(&store, "thread:running").await;
    create_run(&store, "thread:running", "run:running", 1, "running").await;
    create_thread(&store, "thread:idle").await;
    create_run(&store, "thread:idle", "run:done", 1, "completed").await;

    let selected = store
        .get_recovery_thread_ids(ProjectionRecoveryKind::Runtime)
        .await
        .expect("select runtime");

    assert_eq!(selected, vec![tid("thread:running")]);
}
