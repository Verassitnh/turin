//! `secret_request.record` and `provider-session.detach` (`secret_request.rs`).
//! Upstream covers the secret card through the agent secret tool's tests.

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::{
    OrchestrationV2SecretRequestStatus, OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
};

use super::message_steer_tests::{Steer, harness};
use super::tests::thread;
use crate::pipeline::effect_outbox::EffectRequest;
use crate::pipeline::orchestrator_port::{OrchestratorError, OrchestratorPort};

fn command(value: Value) -> turin_contracts::orchestration_v2::OrchestrationV2ServerCommand {
    serde_json::from_value(value).expect("command decodes")
}

fn cause(error: &OrchestratorError) -> String {
    std::error::Error::source(error).map_or_else(|| error.to_string(), ToString::to_string)
}

async fn record(
    h: &Steer,
    command_id: &str,
    run: &str,
    node: &str,
    status: &str,
) -> Result<(), OrchestratorError> {
    h.orchestrator
        .dispatch(command(json!({
            "type": "secret_request.record",
            "commandId": command_id,
            "threadId": "thread:secret",
            "runId": run,
            "nodeId": node,
            "turnItemId": "turn-item:secret:1",
            "label": "API key",
            "reason": "To call the service",
            "secretStatus": status,
        })))
        .await
        .map(|_| ())
}

async fn secret_item(
    h: &Steer,
) -> (
    OrchestrationV2TurnItemStatus,
    OrchestrationV2SecretRequestStatus,
) {
    h.orchestrator
        .get_thread_projection(&thread("thread:secret"))
        .await
        .expect("projection reads")
        .turn_items
        .iter()
        .find_map(|item| match item {
            OrchestrationV2TurnItem::SecretRequest {
                status,
                secret_status,
                ..
            } => Some((*status, *secret_status)),
            _ => None,
        })
        .expect("the secret card")
}

#[tokio::test]
async fn a_secret_request_waits_then_settles_once() {
    let h = harness(true, None).await;
    let run = h.running_run("thread:secret").await;
    let node = h
        .orchestrator
        .get_thread_projection(&thread("thread:secret"))
        .await
        .unwrap()
        .runs[0]
        .root_node_id
        .clone()
        .unwrap()
        .to_string();

    record(&h, "command:ask", &run, &node, "pending")
        .await
        .expect("the card is asked");
    assert_eq!(
        secret_item(&h).await,
        (
            OrchestrationV2TurnItemStatus::Waiting,
            OrchestrationV2SecretRequestStatus::Pending
        )
    );
    record(&h, "command:save", &run, &node, "saved")
        .await
        .expect("the card is saved");
    assert_eq!(
        secret_item(&h).await,
        (
            OrchestrationV2TurnItemStatus::Completed,
            OrchestrationV2SecretRequestStatus::Saved
        )
    );
    // A late retry of the question leaves the answered card as it was.
    record(&h, "command:late", &run, &node, "pending")
        .await
        .expect("accepted");
    assert_eq!(
        secret_item(&h).await,
        (
            OrchestrationV2TurnItemStatus::Completed,
            OrchestrationV2SecretRequestStatus::Saved
        )
    );
}

#[tokio::test]
async fn a_secret_request_must_name_the_runs_root_node() {
    let h = harness(true, None).await;
    let run = h.running_run("thread:secret").await;
    let error = record(&h, "command:wrong-node", &run, "node:elsewhere", "pending")
        .await
        .expect_err("not the root node");
    assert_eq!(
        cause(&error),
        format!("Node node:elsewhere is not the root of run {run}.")
    );
}

#[tokio::test]
async fn a_session_detach_names_the_session_and_queues_the_effect() {
    let h = harness(true, None).await;
    h.running_run("thread:detach").await;
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:detach"))
        .await
        .unwrap();
    let session_id = projection.provider_threads[0]
        .provider_session_id
        .clone()
        .unwrap();
    let session = json!({
        "id": session_id,
        "driver": "codex",
        "providerInstanceId": "codex",
        "status": "running",
        "cwd": "/turin-fixture/project",
        "model": "gpt-test",
        "capabilities": h.capabilities,
        "createdAt": super::message_steer_tests::T1,
        "updatedAt": super::message_steer_tests::T1,
        "lastError": null,
    });
    crate::projection::test_support::apply_all(
        &h.projection,
        vec![crate::projection::test_support::event(
            "provider-session.attached",
            "thread:detach",
            super::message_steer_tests::T1,
            session,
        )],
    )
    .await;

    h.orchestrator
        .dispatch(command(json!({
            "type": "provider-session.detach",
            "commandId": "command:detach",
            "threadId": "thread:detach",
            "providerSessionId": session_id,
            "reason": "Done with it.",
        })))
        .await
        .expect("the detach commits");
    assert!(matches!(
        h.effects("command:detach").await.as_slice(),
        [EffectRequest::ProviderSessionDetach { detail: Some(detail), .. }] if detail == "Done with it."
    ));

    let foreign = h
        .orchestrator
        .dispatch(command(json!({
            "type": "provider-session.detach",
            "commandId": "command:detach-foreign",
            "threadId": "thread:detach",
            "providerSessionId": "provider-session:other",
        })))
        .await
        .expect_err("not this thread's session");
    assert_eq!(
        cause(&foreign),
        "Provider session provider-session:other does not belong to thread thread:detach."
    );
}
