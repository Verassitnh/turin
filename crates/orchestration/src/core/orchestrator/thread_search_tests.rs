//! The port's `search_thread` over a thread with a message (`thread_find.rs`).

use futures::StreamExt;
use serde_json::json;

use super::message_steer_tests::harness;
use crate::pipeline::orchestrator_port::OrchestratorPort;

fn input(
    thread: &str,
    query: &str,
) -> turin_contracts::orchestration_v2::OrchestrationV2SearchThreadInput {
    serde_json::from_value(json!({"threadId": thread, "query": query})).expect("input decodes")
}

#[tokio::test]
async fn the_port_searches_the_messages_of_a_thread() {
    let h = harness(false, None).await;
    h.running_run("thread:find").await;
    let found = h
        .orchestrator
        .search_thread(&input("thread:find", "hell"))
        .await
        .expect("search runs");
    assert_eq!(found.total_matches, 1, "{found:?}");
    assert!(found.r#match.is_some());

    let frames: Vec<_> = h
        .orchestrator
        .search_thread_stream(&input("thread:find", "hell"))
        .await
        .expect("stream opens")
        .collect()
        .await;
    assert_eq!(frames.len(), 2, "an early frame then the final one");
    assert_eq!(frames[0].complete, Some(false));

    let none = h
        .orchestrator
        .search_thread(&input("thread:find", "absent"))
        .await
        .expect("search runs");
    assert_eq!(none.total_matches, 0);
    assert!(none.r#match.is_none());
}

#[tokio::test]
async fn searching_an_unknown_thread_fails() {
    let h = harness(false, None).await;
    assert!(
        h.orchestrator
            .search_thread(&input("thread:missing", "x"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn the_port_reads_pages_snapshots_and_items_from_the_store() {
    use crate::projection::store::ThreadSnapshotWindowOptions;
    use crate::projection::store::history::TimelinePageOptions;
    let h = harness(false, None).await;
    h.running_run("thread:reads").await;
    let id = super::tests::thread("thread:reads");

    assert_eq!(
        h.orchestrator.get_message_count(&id).await.expect("count"),
        1
    );

    let page = h
        .orchestrator
        .get_timeline_page(
            &id,
            TimelinePageOptions {
                limit: 10,
                ..TimelinePageOptions::default()
            },
        )
        .await
        .expect("timeline page");
    assert_eq!(page.items.len(), 1);
    assert!(!page.has_more);

    let item_id = turin_provider_core::turn_item::turn_item_common(&page.items[0].item)
        .id
        .clone();
    let item = h
        .orchestrator
        .get_turn_item(&id, &item_id)
        .await
        .expect("turn item");
    assert!(item.is_some());

    let snapshot = h
        .orchestrator
        .get_thread_snapshot(&id)
        .await
        .expect("snapshot");
    assert!(snapshot.snapshot_sequence > 0);
    assert_eq!(snapshot.projection.runs.len(), 1);

    let window = h
        .orchestrator
        .get_thread_snapshot_window(
            &id,
            ThreadSnapshotWindowOptions {
                row_limit: 50,
                user_turn_limit: Some(1),
                ..ThreadSnapshotWindowOptions::default()
            },
        )
        .await
        .expect("window");
    assert_eq!(window.projection.runs.len(), 1);

    let context = h
        .orchestrator
        .get_checkpoint_context(&id)
        .await
        .expect("context");
    assert_eq!(context.runs.len(), 1);

    let history = h
        .orchestrator
        .get_thread_history_page(&id, "bogus", None, false)
        .await;
    assert!(history.is_err(), "a malformed cursor is a projection error");

    assert!(
        h.orchestrator
            .get_message_count(&super::tests::thread("thread:none"))
            .await
            .is_ok()
    );
}

/// `readCommandProjection`: control records only. Finished leaf nodes of
/// finished runs and non-control turn items stay unread.
#[tokio::test]
async fn the_command_projection_leaves_out_history_and_settled_leaf_nodes() {
    use crate::projection::test_support::{apply_all, event, turn_item};
    let h = harness(false, None).await;
    let run_id = h.running_run("thread:cmd").await;
    let id = super::tests::thread("thread:cmd");
    let full = h
        .orchestrator
        .get_thread_projection(&id)
        .await
        .expect("full");
    let root = full.nodes[0].clone();
    let mut run = serde_json::to_value(&full.runs[0]).expect("run encodes");
    run["status"] = json!("completed");
    run["completedAt"] = json!(super::message_steer_tests::T1);
    let mut leaf = serde_json::to_value(&root).expect("node encodes");
    leaf["id"] = json!("node:leaf");
    leaf["kind"] = json!("assistant_message");
    leaf["status"] = json!("completed");
    leaf["parentNodeId"] = json!(root.id);
    leaf["checkpointScopeId"] = serde_json::Value::Null;
    let assistant = turn_item(
        "thread:cmd",
        "item:assistant",
        Some(&run_id),
        90,
        "assistant_message",
        "completed",
        json!({"messageId": "message:assistant", "text": "long answer", "streaming": false}),
    );
    apply_all(
        &h.projection,
        vec![
            event(
                "run.updated",
                "thread:cmd",
                super::message_steer_tests::T1,
                run,
            ),
            event(
                "node.updated",
                "thread:cmd",
                super::message_steer_tests::T1,
                leaf,
            ),
            event(
                "turn-item.updated",
                "thread:cmd",
                super::message_steer_tests::T1,
                assistant,
            ),
        ],
    )
    .await;

    let full = h
        .orchestrator
        .get_thread_projection(&id)
        .await
        .expect("full");
    assert!(
        full.nodes
            .iter()
            .any(|node| node.id.as_str() == "node:leaf")
    );
    assert!(!full.visible_turn_items.is_empty());

    let command = h
        .orchestrator
        .read_command_projection(&id)
        .await
        .expect("command");
    assert!(
        command
            .nodes
            .iter()
            .all(|node| node.id.as_str() != "node:leaf"),
        "a finished leaf of a finished run stays unread"
    );
    assert!(command.turn_items.iter().all(|item| matches!(
        item,
        turin_contracts::orchestration_v2::OrchestrationV2TurnItem::UserMessage { .. }
    )));
    assert!(command.visible_turn_items.is_empty());
    assert!(command.checkpoints.is_empty() && command.plans.is_empty());
    assert_eq!(command.runs.len(), 1);
}
