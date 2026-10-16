//! `notification.delivery.accept`, `delegated_task.wake-policy`,
//! `delegated_task.completion-delivery.*` and `thread.created.record`
//! (`delegated_commands.rs`).

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::{
    OrchestrationV2CompletionWake, OrchestrationV2DelegatedCompletionTaskDeliveryState as State,
    OrchestrationV2TurnItem,
};

use super::delegated_task_tests::{TASK, child_with_result, parent_task};
use super::message_steer_tests::harness;
use super::tests::thread;
use crate::pipeline::orchestrator_port::{OrchestratorError, OrchestratorPort};

fn command(value: Value) -> turin_contracts::orchestration_v2::OrchestrationV2ServerCommand {
    serde_json::from_value(value).expect("command decodes")
}

fn cause(error: &OrchestratorError) -> String {
    std::error::Error::source(error).map_or_else(|| error.to_string(), ToString::to_string)
}

fn wake_policy(id: &str, wake: &str) -> Value {
    json!({
        "type": "delegated_task.wake-policy",
        "commandId": id,
        "parentThreadId": "thread:parent",
        "taskId": TASK,
        "completionWake": wake,
    })
}

#[tokio::test]
async fn upgrading_the_wake_of_a_running_task_only_updates_the_row() {
    let h = harness(false, None).await;
    child_with_result(&h, "settled_only").await;
    h.orchestrator
        .dispatch(command(wake_policy("command:wake:1", "always")))
        .await
        .expect("wake policy applies");
    let task = parent_task(&h).await;
    assert_eq!(
        task.completion_wake,
        Some(OrchestrationV2CompletionWake::Always)
    );
}

#[tokio::test]
async fn a_repeated_wake_policy_and_a_foreign_task_are_rejected() {
    let h = harness(false, None).await;
    child_with_result(&h, "always").await;
    let error = h
        .orchestrator
        .dispatch(command(wake_policy("command:wake:2", "always")))
        .await
        .expect_err("same wake rejects");
    assert!(
        cause(&error).contains("already wakes the parent"),
        "{}",
        cause(&error)
    );
    let mut unknown = wake_policy("command:wake:3", "always");
    unknown["taskId"] = json!("node:task:nobody");
    let error = h
        .orchestrator
        .dispatch(command(unknown))
        .await
        .expect_err("unknown task rejects");
    assert!(
        cause(&error).contains("is not an app-owned task"),
        "{}",
        cause(&error)
    );
}

#[tokio::test]
async fn upgrading_a_finished_task_claims_the_wake_while_the_parent_runs() {
    let h = harness(false, None).await;
    let done = child_with_result(&h, "settled_only").await;
    h.orchestrator.handle_terminal_run(&done).await;
    assert!(parent_task(&h).await.completion_delivery.is_none());

    h.orchestrator
        .dispatch(command(wake_policy("command:wake:4", "always")))
        .await
        .expect("wake policy applies");
    let task = parent_task(&h).await;
    assert_eq!(
        task.completion_delivery.map(|d| d.state),
        Some(State::Claimed)
    );
}

#[tokio::test]
async fn acknowledging_a_claimed_task_clears_the_parent_delivery() {
    let h = harness(false, None).await;
    let done = child_with_result(&h, "always").await;
    h.orchestrator.handle_terminal_run(&done).await;
    assert_eq!(
        parent_task(&h).await.completion_delivery.map(|d| d.state),
        Some(State::Claimed)
    );

    let ack = json!({
        "type": "delegated_task.completion-delivery.acknowledge",
        "commandId": "command:ack:1",
        "parentThreadId": "thread:parent",
        "taskId": TASK,
        "observedByRunId": null,
    });
    h.orchestrator
        .dispatch(command(ack.clone()))
        .await
        .expect("acknowledge applies");
    assert_eq!(
        parent_task(&h).await.completion_delivery.map(|d| d.state),
        Some(State::Acknowledged)
    );
    let parent = h
        .orchestrator
        .get_thread_projection(&thread("thread:parent"))
        .await
        .expect("projection reads");
    assert!(
        parent.runs[0]
            .delegated_completion
            .as_ref()
            .is_none_or(|cohort| cohort.delivery.is_none())
    );

    // A second observation is an idempotent no-op, not an error.
    let mut again = ack;
    again["commandId"] = json!("command:ack:2");
    h.orchestrator
        .dispatch(command(again))
        .await
        .expect("second acknowledge is a no-op");
    let dispose = json!({
        "type": "delegated_task.completion-delivery.dispose",
        "commandId": "command:dispose:1",
        "parentThreadId": "thread:parent",
        "taskId": TASK,
    });
    h.orchestrator
        .dispatch(command(dispose))
        .await
        .expect("dispose applies");
    assert_eq!(
        parent_task(&h).await.completion_delivery.map(|d| d.state),
        Some(State::Disposed)
    );
}

#[tokio::test]
async fn an_acceptance_for_an_unknown_message_is_a_harmless_no_op() {
    let h = harness(false, None).await;
    child_with_result(&h, "always").await;
    h.orchestrator
        .dispatch(command(json!({
            "type": "notification.delivery.accept",
            "commandId": "command:accept:1",
            "threadId": "thread:parent",
            "messageId": "message:nope",
        })))
        .await
        .expect("stale acceptance is accepted");
}

#[tokio::test]
async fn recording_a_created_thread_adds_a_timeline_item_to_the_parent() {
    let h = harness(false, None).await;
    child_with_result(&h, "always").await;
    let parent = h
        .orchestrator
        .get_thread_projection(&thread("thread:parent"))
        .await
        .expect("projection reads");
    let run = &parent.runs[0];
    let record = json!({
        "type": "thread.created.record",
        "commandId": "command:created:1",
        "parentThreadId": "thread:parent",
        "parentRunId": run.id,
        "parentNodeId": run.root_node_id,
        "targetThreadId": "thread:child",
        "targetRunId": null,
    });
    h.orchestrator
        .dispatch(command(record))
        .await
        .expect("record applies");
    let parent = h
        .orchestrator
        .get_thread_projection(&thread("thread:parent"))
        .await
        .expect("projection reads");
    assert!(parent.turn_items.iter().any(|item| matches!(
        item,
        OrchestrationV2TurnItem::ThreadCreated { target_thread_id, .. }
            if target_thread_id.as_str() == "thread:child"
    )));

    let wrong_node = json!({
        "type": "thread.created.record",
        "commandId": "command:created:2",
        "parentThreadId": "thread:parent",
        "parentRunId": run.id,
        "parentNodeId": "node:other",
        "targetThreadId": "thread:child",
        "targetRunId": null,
    });
    let error = h
        .orchestrator
        .dispatch(command(wrong_node))
        .await
        .expect_err("a non-root node rejects");
    assert!(
        cause(&error).contains("is not the root of run"),
        "{}",
        cause(&error)
    );
}
