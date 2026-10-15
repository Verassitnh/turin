//! Context reads (provider, runtime response, checkpoint, run completion) against
//! projection rows written through the apply path.

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::OrchestrationV2ExecutionNode;
use turin_contracts::{ProviderThreadId, ProviderTurnId, ThreadId};

use crate::projection::error::ProjectionStoreError;
use crate::projection::store::ProjectionStore;
use crate::projection::store::contexts::ProviderControlTarget;
use crate::projection::test_support::{apply_all, event, file_store, run, thread};

const T0: &str = "2026-01-01T00:00:01.000Z";

fn tid(id: &str) -> ThreadId {
    ThreadId::new(id).expect("thread id")
}

async fn create_thread(store: &ProjectionStore, id: &str) {
    apply_all(store, vec![event("thread.created", id, T0, thread(id))]).await;
}

fn execution_node(thread_id: &str, id: &str, status: &str) -> Value {
    json!({
        "id": id,
        "threadId": thread_id,
        "runId": null,
        "parentNodeId": null,
        "rootNodeId": id,
        "kind": "root_turn",
        "status": status,
        "countsForRun": false,
        "providerThreadId": null,
        "providerTurnId": null,
        "nativeItemRef": null,
        "runtimeRequestId": null,
        "checkpointScopeId": null,
        "startedAt": T0,
        "completedAt": null
    })
}

fn is_missing_thread(error: &ProjectionStoreError) -> bool {
    matches!(error, ProjectionStoreError::ThreadNotFound { .. })
}

#[tokio::test]
async fn every_context_read_reports_a_missing_thread() {
    let (store, _dir) = file_store().await;
    let absent = tid("thread:absent-context");

    let provider = store
        .get_thread_provider_context(&absent, None)
        .await
        .unwrap_err();
    let running = store.get_running_turn_context(&absent).await.unwrap_err();
    let runtime = store
        .get_runtime_response_context(&absent, "request:any")
        .await
        .unwrap_err();
    let checkpoints = store.get_checkpoint_context(&absent).await.unwrap_err();
    let capture = store
        .get_checkpoint_capture_context(&absent, "run:any", "scope:any")
        .await
        .unwrap_err();
    let control = store
        .get_provider_control_context(
            &absent,
            ProviderControlTarget {
                provider_thread_id: ProviderThreadId::new("provider-thread:any").unwrap(),
                provider_turn_id: ProviderTurnId::new("provider-turn:any").unwrap(),
                attempt_id: None,
                message_id: None,
            },
        )
        .await
        .unwrap_err();

    for error in [provider, running, runtime, checkpoints, capture, control] {
        assert!(is_missing_thread(&error), "{error:?}");
    }
}

#[tokio::test]
async fn running_turn_context_returns_the_running_run_without_provider_rows() {
    let (store, _dir) = file_store().await;
    create_thread(&store, "thread:running-turn").await;
    apply_all(
        &store,
        vec![event(
            "run.created",
            "thread:running-turn",
            T0,
            run("thread:running-turn", "run:live", 1, "running"),
        )],
    )
    .await;

    let context = store
        .get_running_turn_context(&tid("thread:running-turn"))
        .await
        .expect("read running turn");

    assert_eq!(
        context.run.map(|run| run.id.as_str().to_owned()),
        Some("run:live".to_owned())
    );
    assert!(context.provider_thread.is_none());
    assert!(context.provider_turn.is_none());
}

#[tokio::test]
async fn checkpoint_context_lists_runs_and_scopes_for_the_thread() {
    let (store, _dir) = file_store().await;
    create_thread(&store, "thread:checkpoints").await;
    apply_all(
        &store,
        vec![event(
            "run.created",
            "thread:checkpoints",
            T0,
            run("thread:checkpoints", "run:checkpointed", 1, "completed"),
        )],
    )
    .await;

    let context = store
        .get_checkpoint_context(&tid("thread:checkpoints"))
        .await
        .expect("read checkpoint context");

    assert_eq!(context.runs.len(), 1);
    assert_eq!(context.runs[0].id, "run:checkpointed");
    assert_eq!(context.runs[0].status, "completed");
    assert!(context.checkpoint_scopes.is_empty());
    assert!(context.checkpoints.is_empty());
}

#[tokio::test]
async fn checkpoint_capture_context_is_empty_for_an_unknown_run_and_scope() {
    let (store, _dir) = file_store().await;
    create_thread(&store, "thread:capture").await;

    let context = store
        .get_checkpoint_capture_context(&tid("thread:capture"), "run:none", "scope:none")
        .await
        .expect("read capture context");

    assert!(context.run.is_none());
    assert!(context.root_node.is_none());
    assert!(context.scope.is_none());
    assert!(context.provider_thread.is_none());
    assert!(context.ready_checkpoint_ordinals.is_empty());
}

#[tokio::test]
async fn can_start_queued_run_needs_a_queued_run_on_an_unarchived_thread() {
    let (store, _dir) = file_store().await;
    create_thread(&store, "thread:queue-ready").await;
    apply_all(
        &store,
        vec![event(
            "run.created",
            "thread:queue-ready",
            T0,
            run("thread:queue-ready", "run:queued", 1, "queued"),
        )],
    )
    .await;

    let mut archived = thread("thread:queue-archived");
    archived["archivedAt"] = json!(T0);
    apply_all(
        &store,
        vec![
            event("thread.created", "thread:queue-archived", T0, archived),
            event(
                "run.created",
                "thread:queue-archived",
                T0,
                run("thread:queue-archived", "run:queued-archived", 1, "queued"),
            ),
        ],
    )
    .await;

    let ready = store
        .can_start_queued_run(&tid("thread:queue-ready"))
        .await
        .unwrap();
    let archived_ready = store
        .can_start_queued_run(&tid("thread:queue-archived"))
        .await
        .unwrap();

    assert!(ready);
    assert!(!archived_ready);
}

#[tokio::test]
async fn unpaired_run_interrupt_request_is_true_until_its_result_arrives() {
    let (store, _dir) = file_store().await;
    create_thread(&store, "thread:interrupt").await;
    let request = crate::projection::test_support::turn_item(
        "thread:interrupt",
        "item:interrupt-request",
        None,
        1,
        "run_interrupt_request",
        "completed",
        json!({ "message": "stop" }),
    );
    apply_all(
        &store,
        vec![event("turn-item.updated", "thread:interrupt", T0, request)],
    )
    .await;
    let thread_id = tid("thread:interrupt");

    let before = store
        .has_unpaired_run_interrupt_request(
            &thread_id,
            "item:interrupt-request",
            "item:interrupt-result",
        )
        .await
        .unwrap();
    let result = crate::projection::test_support::turn_item(
        "thread:interrupt",
        "item:interrupt-result",
        None,
        2,
        "run_interrupt_result",
        "completed",
        json!({ "message": "stopped" }),
    );
    apply_all(
        &store,
        vec![event("turn-item.updated", "thread:interrupt", T0, result)],
    )
    .await;
    let after = store
        .has_unpaired_run_interrupt_request(
            &thread_id,
            "item:interrupt-request",
            "item:interrupt-result",
        )
        .await
        .unwrap();

    assert!(before);
    assert!(!after);
}

#[tokio::test]
async fn runtime_response_context_has_no_session_for_a_message_answered_request() {
    let (store, _dir) = file_store().await;
    create_thread(&store, "thread:response").await;
    apply_all(
        &store,
        vec![event(
            "runtime-request.updated",
            "thread:response",
            T0,
            json!({
                "id": "request:response",
                "threadId": "thread:response",
                "nodeId": "node:response",
                "providerTurnId": null,
                "nativeRequestRef": null,
                "kind": "user_input",
                "status": "pending",
                "responseCapability": { "type": "message" },
                "createdAt": T0,
                "resolvedAt": null
            }),
        )],
    )
    .await;
    let thread_id = tid("thread:response");

    let missing = store
        .get_runtime_response_context(&thread_id, "request:absent")
        .await
        .unwrap();
    let found = store
        .get_runtime_response_context(&thread_id, "request:response")
        .await
        .unwrap();

    assert!(missing.request.is_none());
    assert_eq!(
        found.request.map(|request| request.id.as_str().to_owned()),
        Some("request:response".to_owned())
    );
    assert!(found.session.is_none());
    assert!(found.node.is_none());
    assert!(found.item.is_none());
}

#[tokio::test]
async fn thread_provider_context_skips_provider_threads_without_a_target_instance() {
    let (store, _dir) = file_store().await;
    create_thread(&store, "thread:provider-context").await;

    let without_target = store
        .get_thread_provider_context(&tid("thread:provider-context"), None)
        .await
        .unwrap();

    assert_eq!(without_target.thread.id.as_str(), "thread:provider-context");
    assert!(without_target.provider_sessions.is_empty());
    assert!(without_target.provider_threads.is_empty());
}

#[tokio::test]
async fn is_node_stored_matches_only_the_exact_stored_node() {
    let (store, _dir) = file_store().await;
    create_thread(&store, "thread:node-stored").await;
    let stored: OrchestrationV2ExecutionNode = serde_json::from_value(execution_node(
        "thread:node-stored",
        "node:stored",
        "running",
    ))
    .expect("node fixture");
    apply_all(
        &store,
        vec![event(
            "node.updated",
            "thread:node-stored",
            T0,
            execution_node("thread:node-stored", "node:stored", "running"),
        )],
    )
    .await;

    let mut changed = stored.clone();
    changed.status = serde_json::from_value(json!("completed")).expect("status");

    assert!(store.is_node_stored(&stored).await.unwrap());
    assert!(!store.is_node_stored(&changed).await.unwrap());
}

#[tokio::test]
async fn turn_start_context_reads_the_thread_runs_and_run_scoped_items() {
    let (store, _dir) = file_store().await;
    create_thread(&store, "thread:turn-start").await;
    apply_all(
        &store,
        vec![
            event(
                "run.created",
                "thread:turn-start",
                T0,
                run("thread:turn-start", "run:turn-start", 1, "queued"),
            ),
            event(
                "message.updated",
                "thread:turn-start",
                T0,
                crate::projection::test_support::message(
                    "thread:turn-start",
                    "message:turn-start",
                    Some("run:turn-start"),
                    "user",
                    "first",
                    T0,
                ),
            ),
            event(
                "turn-item.updated",
                "thread:turn-start",
                T0,
                crate::projection::test_support::user_message_item(
                    "thread:turn-start",
                    "item:turn-start",
                    "run:turn-start",
                    1,
                    "first",
                    "turn_start",
                ),
            ),
        ],
    )
    .await;
    let thread_id = tid("thread:turn-start");

    let context = store
        .get_turn_start_context(
            &thread_id,
            &turin_contracts::RunId::new("run:turn-start").unwrap(),
        )
        .await
        .expect("turn start context");
    let missing = store
        .get_turn_start_context(
            &tid("thread:turn-start-absent"),
            &turin_contracts::RunId::new("run:turn-start").unwrap(),
        )
        .await;

    assert_eq!(context.thread.id.as_str(), "thread:turn-start");
    assert_eq!(context.runs.len(), 1);
    assert_eq!(context.turn_items.len(), 1);
    assert!(context.checkpoint_scopes.is_empty());
    // A user message with text and no attachments is conversation, not a compact command.
    assert!(context.has_conversation);
    assert!(matches!(
        missing,
        Err(ProjectionStoreError::ThreadNotFound { .. })
    ));
}
