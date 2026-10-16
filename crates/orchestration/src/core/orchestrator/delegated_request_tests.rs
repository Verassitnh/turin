//! `delegated_task.request` (`delegated_request.rs`). Upstream covers it in
//! the delegated-task integration suites; here a running parent run stands in.

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::{
    OrchestrationV2ContextTransferType, OrchestrationV2SubagentOrigin,
    OrchestrationV2SubagentStatus, OrchestrationV2TurnItem,
};

use super::message_steer_tests::harness;
use super::tests::thread;
use crate::pipeline::orchestrator_port::{OrchestratorError, OrchestratorPort};

fn command(value: Value) -> turin_contracts::orchestration_v2::OrchestrationV2ServerCommand {
    serde_json::from_value(value).expect("command decodes")
}

fn cause(error: &OrchestratorError) -> String {
    std::error::Error::source(error).map_or_else(|| error.to_string(), ToString::to_string)
}

fn request(id: &str, run: &str, node: &Value, wake: Option<&str>) -> Value {
    let mut body = json!({
        "type": "delegated_task.request",
        "createdBy": "agent",
        "creationSource": "mcp",
        "commandId": id,
        "parentThreadId": "thread:parent",
        "parentRunId": run,
        "parentNodeId": node,
        "task": "Look into the failing build",
        "modelSelection": {"instanceId": "codex", "model": "gpt-test"},
        "runtimeMode": "full-access",
        "interactionMode": "default",
    });
    if let Some(wake) = wake {
        body["completionWake"] = json!(wake);
    }
    body
}

#[tokio::test]
async fn a_request_creates_the_child_thread_task_and_first_run() {
    let h = harness(false, None).await;
    let run = h.running_run("thread:parent").await;
    let parent = h
        .orchestrator
        .get_thread_projection(&thread("thread:parent"))
        .await
        .expect("projection reads");
    let root = json!(parent.runs[0].root_node_id);
    h.orchestrator
        .dispatch(command(request(
            "command:delegate:1",
            &run,
            &root,
            Some("always"),
        )))
        .await
        .expect("request commits");

    let parent = h
        .orchestrator
        .get_thread_projection(&thread("thread:parent"))
        .await
        .expect("projection reads");
    let task = parent.subagents.first().expect("the task row");
    assert_eq!(task.origin, OrchestrationV2SubagentOrigin::AppOwned);
    assert_eq!(task.status, OrchestrationV2SubagentStatus::Running);
    assert_eq!(task.prompt, "Look into the failing build");
    let child_id = task.child_thread_id.clone().expect("a child thread");
    assert!(parent.nodes.iter().any(|node| node.id == task.id));
    assert!(parent.turn_items.iter().any(|item| matches!(
        item,
        OrchestrationV2TurnItem::Subagent { child_thread_id: Some(id), .. } if id == &child_id
    )));

    let child = h
        .orchestrator
        .get_thread_projection(&child_id)
        .await
        .expect("child projection reads");
    assert_eq!(child.runs.len(), 1, "the task starts the child's first run");
    assert!(
        child
            .messages
            .iter()
            .any(|message| message.text == "Look into the failing build")
    );
    assert!(child.context_transfers.iter().any(|transfer| {
        transfer.r#type == OrchestrationV2ContextTransferType::SubagentSpawn
            && transfer.source_thread_id.as_str() == "thread:parent"
    }));
}

#[tokio::test]
async fn a_request_against_a_run_that_is_not_active_is_refused() {
    let h = harness(false, None).await;
    h.running_run("thread:parent").await;
    let error = h
        .orchestrator
        .dispatch(command(request(
            "command:delegate:2",
            "run:nobody",
            &json!("node:nobody"),
            None,
        )))
        .await
        .expect_err("an unknown run refuses");
    assert!(cause(&error).contains("is not active"), "{}", cause(&error));
}
