//! Control-plane reads (`getPendingNativeUserInputs`, `getPlan`, `getRuntimeRequest`)
//! against projection rows written through the apply path.

use serde_json::{Value, json};
use turin_contracts::ThreadId;
use turin_contracts::orchestration_v2::OrchestrationV2TurnItem;

use crate::projection::store::ProjectionStore;
use crate::projection::test_support::{apply_all, event, file_store, thread, turn_item};

const T0: &str = "2026-01-01T00:00:01.000Z";

async fn create_thread(store: &ProjectionStore, id: &str) {
    apply_all(store, vec![event("thread.created", id, T0, thread(id))]).await;
}

fn node(thread_id: &str, id: &str, turn: &str) -> Value {
    json!({
        "id": id,
        "threadId": thread_id,
        "runId": null,
        "parentNodeId": null,
        "rootNodeId": id,
        "kind": "root_turn",
        "status": "running",
        "countsForRun": false,
        "providerThreadId": null,
        "providerTurnId": turn,
        "nativeItemRef": null,
        "runtimeRequestId": null,
        "checkpointScopeId": null,
        "startedAt": T0,
        "completedAt": null
    })
}

fn runtime_request(
    thread_id: &str,
    id: &str,
    node_id: &str,
    turn: &str,
    capability: Value,
) -> Value {
    json!({
        "id": id,
        "threadId": thread_id,
        "nodeId": node_id,
        "providerTurnId": turn,
        "nativeRequestRef": null,
        "kind": "user_input",
        "status": "pending",
        "responseCapability": capability,
        "createdAt": T0,
        "resolvedAt": null
    })
}

fn user_input_item(thread_id: &str, id: &str, turn: &str, request_id: &str) -> Value {
    let mut item = turn_item(
        thread_id,
        id,
        None,
        1,
        "user_input_request",
        "pending",
        json!({ "requestId": request_id, "questions": [] }),
    );
    item["providerTurnId"] = json!(turn);
    item
}

#[tokio::test]
async fn pending_native_user_inputs_keep_native_requests_with_their_node_and_item() {
    let (store, _dir) = file_store().await;
    create_thread(&store, "thread:inputs").await;
    let live = json!({ "type": "live", "providerSessionId": "session:inputs" });
    apply_all(
        &store,
        vec![
            event(
                "node.updated",
                "thread:inputs",
                T0,
                node("thread:inputs", "node:inputs", "turn:inputs"),
            ),
            event(
                "runtime-request.updated",
                "thread:inputs",
                T0,
                runtime_request(
                    "thread:inputs",
                    "request:native",
                    "node:inputs",
                    "turn:inputs",
                    live,
                ),
            ),
            // Message-answerable requests are answered by dispatching a message, not here.
            event(
                "runtime-request.updated",
                "thread:inputs",
                T0,
                runtime_request(
                    "thread:inputs",
                    "request:message",
                    "node:inputs",
                    "turn:inputs",
                    json!({ "type": "message" }),
                ),
            ),
            event(
                "turn-item.updated",
                "thread:inputs",
                T0,
                user_input_item(
                    "thread:inputs",
                    "item:native",
                    "turn:inputs",
                    "request:native",
                ),
            ),
            event(
                "turn-item.updated",
                "thread:inputs",
                T0,
                user_input_item(
                    "thread:inputs",
                    "item:other",
                    "turn:inputs",
                    "request:unrelated",
                ),
            ),
        ],
    )
    .await;

    let pending = store
        .get_pending_native_user_inputs(
            &ThreadId::new("thread:inputs").unwrap(),
            &turin_contracts::ProviderTurnId::new("turn:inputs").unwrap(),
        )
        .await
        .expect("read pending inputs");

    let request_ids: Vec<&str> = pending
        .runtime_requests
        .iter()
        .map(|request| request.id.as_str())
        .collect();
    assert_eq!(request_ids, vec!["request:native"]);
    let node_ids: Vec<&str> = pending.nodes.iter().map(|node| node.id.as_str()).collect();
    assert_eq!(node_ids, vec!["node:inputs"]);
    let item_ids: Vec<String> = pending
        .turn_items
        .iter()
        .map(|item| match item {
            OrchestrationV2TurnItem::UserInputRequest { id, .. } => id.as_str().to_owned(),
            other => panic!("unexpected item {other:?}"),
        })
        .collect();
    assert_eq!(item_ids, vec!["item:native"]);
}

#[tokio::test]
async fn pending_native_user_inputs_are_empty_for_a_turn_without_requests() {
    let (store, _dir) = file_store().await;
    create_thread(&store, "thread:no-inputs").await;

    let pending = store
        .get_pending_native_user_inputs(
            &ThreadId::new("thread:no-inputs").unwrap(),
            &turin_contracts::ProviderTurnId::new("turn:none").unwrap(),
        )
        .await
        .expect("read pending inputs");

    assert!(pending.runtime_requests.is_empty());
    assert!(pending.nodes.is_empty());
    assert!(pending.turn_items.is_empty());
}

#[tokio::test]
async fn runtime_request_reads_one_request_and_none_when_absent() {
    let (store, _dir) = file_store().await;
    create_thread(&store, "thread:request").await;
    apply_all(
        &store,
        vec![event(
            "runtime-request.updated",
            "thread:request",
            T0,
            runtime_request(
                "thread:request",
                "request:one",
                "node:one",
                "turn:one",
                json!({ "type": "message" }),
            ),
        )],
    )
    .await;
    let thread_id = ThreadId::new("thread:request").unwrap();

    let found = store
        .get_runtime_request(&thread_id, "request:one")
        .await
        .expect("read request");
    let missing = store
        .get_runtime_request(&thread_id, "request:missing")
        .await
        .expect("read missing request");

    assert_eq!(
        found.map(|request| request.id.as_str().to_owned()),
        Some("request:one".to_owned())
    );
    assert!(missing.is_none());
}

#[tokio::test]
async fn plan_reads_one_plan_and_reports_a_missing_thread() {
    let (store, _dir) = file_store().await;
    create_thread(&store, "thread:plan").await;
    apply_all(
        &store,
        vec![event(
            "plan.updated",
            "thread:plan",
            T0,
            json!({
                "kind": "proposed_plan",
                "id": "plan:one",
                "threadId": "thread:plan",
                "runId": null,
                "nodeId": "node:plan",
                "status": "draft",
                "markdown": "# Plan"
            }),
        )],
    )
    .await;
    let thread_id = ThreadId::new("thread:plan").unwrap();

    let found = store
        .get_plan(&thread_id, "plan:one")
        .await
        .expect("read plan");
    let missing = store
        .get_plan(&thread_id, "plan:missing")
        .await
        .expect("read missing plan");
    let no_thread = store
        .get_plan(&ThreadId::new("thread:absent").unwrap(), "plan:one")
        .await;

    assert!(found.is_some());
    assert!(missing.is_none());
    assert!(matches!(
        no_thread,
        Err(crate::projection::error::ProjectionStoreError::ThreadNotFound { .. })
    ));
}
