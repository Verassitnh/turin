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
