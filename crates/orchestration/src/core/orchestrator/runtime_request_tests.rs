//! `runtime-request.respond` and `thread.user-input.dismiss`
//! (`runtime_request.rs`). Upstream covers them through the approval and
//! user-input integration suites; here requests are seeded into the projection.

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::{
    OrchestrationV2ExecutionNodeStatus, OrchestrationV2RuntimeRequestStatus,
    OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
};

use super::message_steer_tests::{Steer, T1, harness};
use super::tests::thread;
use crate::pipeline::effect_outbox::EffectRequest;
use crate::pipeline::orchestrator_port::{OrchestratorError, OrchestratorPort};
use crate::projection::test_support::{apply_all, event, turn_item};

fn command(value: Value) -> turin_contracts::orchestration_v2::OrchestrationV2ServerCommand {
    serde_json::from_value(value).expect("command decodes")
}

fn cause(error: &OrchestratorError) -> String {
    std::error::Error::source(error).map_or_else(|| error.to_string(), ToString::to_string)
}

/// A running run with a request open on a node under it. `capability` is
/// `live` (answerable through the run's session), `message` or `not_resumable`;
/// `item_kind` is `approval_request` or `user_input_request`.
async fn request(h: &Steer, thread_id: &str, capability: &str, item_kind: &str) -> String {
    let run_id = h.running_run(thread_id).await;
    let projection = h
        .orchestrator
        .get_thread_projection(&thread(thread_id))
        .await
        .expect("projection reads");
    let session_id = projection.provider_threads[0]
        .provider_session_id
        .clone()
        .expect("session");
    // The projection needs the session row for the response context.
    let session = json!({
        "id": session_id,
        "driver": "codex",
        "providerInstanceId": "codex",
        "status": "running",
        "cwd": "/turin-fixture/project",
        "model": "gpt-test",
        "capabilities": h.capabilities,
        "createdAt": T1,
        "updatedAt": T1,
        "lastError": null,
    });
    let node_id = format!("node:{item_kind}:1");
    let mut node = serde_json::to_value(&projection.nodes[0]).expect("node encodes");
    node["id"] = json!(node_id);
    node["kind"] = json!(item_kind);
    node["status"] = json!("waiting");
    node["parentNodeId"] = json!(projection.nodes[0].id);
    node["checkpointScopeId"] = Value::Null;
    node["providerTurnId"] = Value::Null;
    let response_capability = match capability {
        "live" => json!({"type": "live", "providerSessionId": session_id}),
        "message" => json!({"type": "message"}),
        _ => json!({"type": "not_resumable", "reason": "The run ended."}),
    };
    let request = json!({
        "id": "request:1",
        "threadId": thread_id,
        "nodeId": node_id,
        "providerTurnId": null,
        "nativeRequestRef": null,
        "kind": if item_kind == "user_input_request" { "user_input" } else { "command" },
        "status": "pending",
        "responseCapability": response_capability,
        "createdAt": T1,
        "resolvedAt": null,
    });
    let mut item = turn_item(
        thread_id,
        "item:request:1",
        Some(&run_id),
        50,
        item_kind,
        "waiting",
        if item_kind == "user_input_request" {
            json!({
                "requestId": "request:1",
                "questions": [{
                    "id": "q1", "header": "Name", "question": "What is your name?",
                    "options": [], "required": true
                }],
            })
        } else {
            json!({"requestId": "request:1", "requestKind": "command"})
        },
    );
    item["nodeId"] = json!(node_id);
    apply_all(
        &h.projection,
        vec![
            event("provider-session.attached", thread_id, T1, session),
            event("node.updated", thread_id, T1, node),
            event("runtime-request.updated", thread_id, T1, request),
            event("turn-item.updated", thread_id, T1, item),
        ],
    )
    .await;
    run_id
}

fn respond(command_id: &str, thread_id: &str, extra: Value) -> Value {
    let mut body = json!({
        "type": "runtime-request.respond",
        "commandId": command_id,
        "threadId": thread_id,
        "requestId": "request:1",
    });
    body.as_object_mut()
        .expect("object")
        .extend(extra.as_object().expect("object").clone());
    body
}

#[tokio::test]
async fn an_approval_resolves_the_request_its_node_and_item_and_queues_the_provider_reply() {
    let h = harness(true, None).await;
    request(&h, "thread:approve", "live", "approval_request").await;
    h.orchestrator
        .dispatch(command(respond(
            "command:approve",
            "thread:approve",
            json!({"decision": "accept"}),
        )))
        .await
        .expect("the response commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:approve"))
        .await
        .expect("projection reads");
    let request = &projection.runtime_requests[0];
    assert_eq!(
        request.status,
        OrchestrationV2RuntimeRequestStatus::Resolved
    );
    let node = projection
        .nodes
        .iter()
        .find(|node| node.id.as_str() == "node:approval_request:1")
        .expect("the request node");
    assert_eq!(node.status, OrchestrationV2ExecutionNodeStatus::Completed);
    let effects = h.effects("command:approve").await;
    let [
        EffectRequest::RuntimeRequestRespond {
            request_id,
            decision,
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("one respond effect: {effects:?}");
    };
    assert_eq!(request_id.as_str(), "request:1");
    assert!(decision.is_some());
}

#[tokio::test]
async fn a_decline_cancels_the_node_and_item() {
    let h = harness(true, None).await;
    request(&h, "thread:decline", "live", "approval_request").await;
    h.orchestrator
        .dispatch(command(respond(
            "command:decline",
            "thread:decline",
            json!({"decision": "decline"}),
        )))
        .await
        .expect("the response commits");
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:decline"))
        .await
        .expect("projection reads");
    let node = projection
        .nodes
        .iter()
        .find(|node| node.id.as_str() == "node:approval_request:1")
        .expect("the request node");
    assert_eq!(node.status, OrchestrationV2ExecutionNodeStatus::Cancelled);
    assert!(projection.turn_items.iter().any(|item| matches!(
        item,
        OrchestrationV2TurnItem::ApprovalRequest { status, .. }
            if *status == OrchestrationV2TurnItemStatus::Cancelled
    )));
}

#[tokio::test]
async fn an_unknown_resolved_or_unresumable_request_is_refused() {
    let h = harness(true, None).await;
    request(&h, "thread:refuse", "not_resumable", "approval_request").await;
    let unknown = h
        .orchestrator
        .dispatch(command(json!({
            "type": "runtime-request.respond",
            "commandId": "command:unknown",
            "threadId": "thread:refuse",
            "requestId": "request:none",
            "decision": "accept",
        })))
        .await
        .expect_err("no such request");
    assert_eq!(
        cause(&unknown),
        "Runtime request request:none was not found."
    );
    let stale = h
        .orchestrator
        .dispatch(command(respond(
            "command:stale",
            "thread:refuse",
            json!({"decision": "accept"}),
        )))
        .await
        .expect_err("not resumable");
    assert_eq!(cause(&stale), "The run ended.");
}

#[tokio::test]
async fn an_answered_question_becomes_the_threads_next_message() {
    let h = harness(false, None).await;
    request(&h, "thread:question", "message", "user_input_request").await;
    h.orchestrator
        .dispatch(command(respond(
            "command:answer",
            "thread:question",
            json!({"answers": {"q1": "  Ada  "}}),
        )))
        .await
        .expect("the answer commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:question"))
        .await
        .expect("projection reads");
    assert_eq!(
        projection.runtime_requests[0].status,
        OrchestrationV2RuntimeRequestStatus::Resolved
    );
    let message = projection
        .messages
        .iter()
        .find(|message| message.id.as_str() == "async-answer:request:1")
        .expect("the answers are dispatched as a message");
    assert_eq!(
        message.text,
        "What is your name?
Ada"
    );
    // A provider with no active steering queues the answer behind the run.
    assert!(projection.runs.iter().any(
        |run| run.status == turin_contracts::orchestration_v2::OrchestrationV2RunStatus::Queued
    ));
}

#[tokio::test]
async fn an_unanswered_question_is_refused() {
    let h = harness(false, None).await;
    request(&h, "thread:blank", "message", "user_input_request").await;
    let error = h
        .orchestrator
        .dispatch(command(respond(
            "command:blank",
            "thread:blank",
            json!({"answers": {"q1": "   "}}),
        )))
        .await
        .expect_err("a blank answer");
    assert_eq!(cause(&error), "Answer each question before sending.");
}

#[tokio::test]
async fn dismissing_a_message_question_cancels_it_and_a_live_request_cannot_be_dismissed() {
    let h = harness(false, None).await;
    request(&h, "thread:dismiss", "message", "user_input_request").await;
    h.orchestrator
        .dispatch(command(json!({
            "type": "thread.user-input.dismiss",
            "commandId": "command:dismiss",
            "threadId": "thread:dismiss",
            "requestId": "request:1",
        })))
        .await
        .expect("the dismissal commits");
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:dismiss"))
        .await
        .expect("projection reads");
    assert!(projection.turn_items.iter().any(|item| matches!(
        item,
        OrchestrationV2TurnItem::UserInputRequest { status, .. }
            if *status == OrchestrationV2TurnItemStatus::Cancelled
    )));
    // Nothing was sent to the thread.
    assert!(
        projection
            .messages
            .iter()
            .all(|message| message.id.as_str() != "async-answer:request:1")
    );

    let live = harness(false, None).await;
    request(&live, "thread:live-question", "live", "user_input_request").await;
    let error = live
        .orchestrator
        .dispatch(command(json!({
            "type": "thread.user-input.dismiss",
            "commandId": "command:dismiss-live",
            "threadId": "thread:live-question",
            "requestId": "request:1",
        })))
        .await
        .expect_err("a live question needs an answer");
    assert_eq!(
        cause(&error),
        "This question needs an answer. Answer it or stop the turn."
    );
}
