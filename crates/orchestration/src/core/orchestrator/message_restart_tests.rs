//! The restart-continuation preamble of `message.dispatch` (upstream
//! `dispatchMessage`, 4638-4680). `continueRestartedRun` itself (T13e) sends
//! these commands after a server restart cut a run.

use serde_json::json;
use turin_contracts::orchestration_v2::OrchestrationV2RunStatus;

use super::message_steer_tests::{Steer, T1, harness};
use super::tests::{message_command, thread};
use crate::pipeline::orchestrator_port::OrchestratorPort;
use crate::projection::test_support::{apply_all, event};

/// A run that restart reconciliation cancelled mid-turn.
async fn cancelled_run(h: &Steer, thread_id: &str) -> String {
    h.running_run(thread_id).await;
    let run = h
        .orchestrator
        .get_thread_projection(&thread(thread_id))
        .await
        .expect("projection reads")
        .runs
        .remove(0);
    let mut value = serde_json::to_value(&run).expect("run encodes");
    value["status"] = json!("cancelled");
    value["completedAt"] = json!(T1);
    apply_all(
        &h.projection,
        vec![event("run.updated", thread_id, T1, value)],
    )
    .await;
    run.id.to_string()
}

#[tokio::test]
async fn a_continuation_of_a_cancelled_run_starts_a_run_that_names_its_source() {
    let h = harness(true, None).await;
    let source = cancelled_run(&h, "thread:restart-ok").await;
    h.orchestrator
        .dispatch(message_command(
            "command:continue",
            "thread:restart-ok",
            json!({"restartContinuationOfRunId": source, "createdBy": "agent", "creationSource": "server"}),
        ))
        .await
        .expect("the continuation commits");
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:restart-ok"))
        .await
        .expect("projection reads");
    let next = projection.runs.last().expect("a new run");
    assert_eq!(next.status, OrchestrationV2RunStatus::Starting);
    assert_eq!(
        next.restart_continuation_of_run_id
            .as_ref()
            .map(ToString::to_string),
        Some(source)
    );
}

#[tokio::test]
async fn a_stopped_source_is_not_continued() {
    let h = harness(true, None).await;
    let source = cancelled_run(&h, "thread:restart-stopped").await;
    // Stop reached the run: it left an interrupt request.
    h.orchestrator
        .dispatch(
            serde_json::from_value(json!({
                "type": "thread.stop",
                "commandId": "command:stop",
                "threadId": "thread:restart-stopped",
            }))
            .expect("decodes"),
        )
        .await
        .expect("stop commits");
    let before = h
        .orchestrator
        .get_thread_projection(&thread("thread:restart-stopped"))
        .await
        .expect("projection reads")
        .runs
        .len();
    h.orchestrator
        .dispatch(message_command(
            "command:continue-stopped",
            "thread:restart-stopped",
            json!({"restartContinuationOfRunId": source, "createdBy": "agent", "creationSource": "server"}),
        ))
        .await
        .expect("a stale continuation is accepted without work");
    let after = h
        .orchestrator
        .get_thread_projection(&thread("thread:restart-stopped"))
        .await
        .expect("projection reads");
    assert_eq!(after.runs.len(), before, "no run starts after Stop");
}

#[tokio::test]
async fn a_continuation_of_an_unknown_run_is_accepted_without_work() {
    let h = harness(true, None).await;
    h.running_run("thread:restart-unknown").await;
    let result = h
        .orchestrator
        .dispatch(message_command(
            "command:continue-unknown",
            "thread:restart-unknown",
            json!({"restartContinuationOfRunId": "run:missing", "createdBy": "agent", "creationSource": "server"}),
        ))
        .await
        .expect("accepted");
    assert_eq!(
        result.stored_events.len(),
        1,
        "only the unchanged thread row is recorded"
    );
}
