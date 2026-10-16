//! Settling cancels what only wakes the agent: pending message-capable
//! questions and queued notification runs (`thread_archive.rs`).

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::{
    OrchestrationV2RunStatus, OrchestrationV2RuntimeRequestStatus,
};

use super::message_steer_tests::{Steer, T1, harness};
use super::runtime_request_tests::request;
use super::tests::{message_command, thread};
use crate::pipeline::orchestrator_port::{OrchestratorError, OrchestratorPort};
use crate::projection::test_support::{apply_all, event};

fn command(value: Value) -> turin_contracts::orchestration_v2::OrchestrationV2ServerCommand {
    serde_json::from_value(value).expect("command decodes")
}

fn cause(error: &OrchestratorError) -> String {
    std::error::Error::source(error).map_or_else(|| error.to_string(), ToString::to_string)
}

fn settle(id: &str, thread_id: &str) -> Value {
    json!({"type": "thread.settle", "commandId": id, "threadId": thread_id})
}

/// The thread's first run, completed.
async fn complete_first_run(h: &Steer, thread_id: &str) {
    let projection = h
        .orchestrator
        .get_thread_projection(&thread(thread_id))
        .await
        .expect("projection reads");
    let mut run = serde_json::to_value(&projection.runs[0]).expect("run encodes");
    run["status"] = json!("completed");
    run["completedAt"] = json!(T1);
    apply_all(
        &h.projection,
        vec![event("run.updated", thread_id, T1, run)],
    )
    .await;
}

#[tokio::test]
async fn settling_cancels_a_pending_message_question() {
    let h = harness(false, None).await;
    request(&h, "thread:settle-q", "message", "user_input_request").await;
    complete_first_run(&h, "thread:settle-q").await;

    h.orchestrator
        .dispatch(command(settle("command:settle-q", "thread:settle-q")))
        .await
        .expect("settle commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:settle-q"))
        .await
        .expect("projection reads");
    assert!(
        projection
            .runtime_requests
            .iter()
            .all(|request| request.status != OrchestrationV2RuntimeRequestStatus::Pending),
        "{:?}",
        projection.runtime_requests
    );
    assert!(projection.thread.settled_at.is_some());
}

#[tokio::test]
async fn settling_still_refuses_a_pending_approval() {
    let h = harness(false, None).await;
    request(&h, "thread:settle-a", "live", "approval_request").await;
    complete_first_run(&h, "thread:settle-a").await;
    let error = h
        .orchestrator
        .dispatch(command(settle("command:settle-a", "thread:settle-a")))
        .await
        .expect_err("an approval blocks settling");
    assert!(
        cause(&error).contains("is waiting on an approval or question"),
        "{}",
        cause(&error)
    );
}

#[tokio::test]
async fn settling_cancels_queued_automatic_runs() {
    let h = harness(false, None).await;
    h.running_run("thread:settle-n").await;
    h.orchestrator
        .dispatch(message_command(
            "command:wake",
            "thread:settle-n",
            json!({
                "createdBy": "agent",
                "creationSource": "server",
                "dispatchMode": {"type": "queue_after_active"},
                "notification": {
                    "source": {"kind": "background_task"},
                    "outcome": "updated",
                    "summary": "CI finished",
                },
            }),
        ))
        .await
        .expect("the wake queues");
    complete_first_run(&h, "thread:settle-n").await;

    h.orchestrator
        .dispatch(command(settle("command:settle-n", "thread:settle-n")))
        .await
        .expect("settle commits");
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:settle-n"))
        .await
        .expect("projection reads");
    assert!(
        projection
            .runs
            .iter()
            .all(|run| run.status != OrchestrationV2RunStatus::Queued),
        "{:?}",
        projection
            .runs
            .iter()
            .map(|run| run.status)
            .collect::<Vec<_>>()
    );
}
