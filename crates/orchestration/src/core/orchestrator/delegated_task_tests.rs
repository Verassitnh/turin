//! `finalize_app_owned_subagent` and the delegated-completion settling
//! (`delegated_task.rs`). Upstream covers them in the delegated-task
//! integration suites with real providers; here a parent run with an
//! app-owned task row and a seeded child thread stand in.

use serde_json::json;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ContextTransferType, OrchestrationV2DelegatedCompletionTaskDeliveryState,
    OrchestrationV2Run, OrchestrationV2SubagentStatus,
};

use super::delegated_task::FinalizeOptions;
use super::message_steer_tests::{Steer, T1, harness};
use super::tests::{message_command, thread};
use crate::pipeline::orchestrator_port::OrchestratorPort;
use crate::projection::test_support::{apply_all, event, message};

const TASK: &str = "node:task:child";

/// A running parent with an app-owned task whose child thread has a run that
/// ended (`completed`) with an assistant answer. Returns the child's run.
async fn child_with_result(h: &Steer, wake: &str) -> OrchestrationV2Run {
    let parent_run = h.running_run("thread:parent").await;
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:parent"))
        .await
        .expect("projection reads");
    let provider_thread = projection.provider_threads[0].clone();
    // The child thread: a subagent forked from the task node.
    let mut child = crate::projection::test_support::thread("thread:child");
    child["lineage"] = json!({
        "parentThreadId": "thread:parent",
        "relationshipToParent": "subagent",
        "rootThreadId": "thread:parent",
    });
    child["forkedFrom"] = json!({"type": "node", "nodeId": TASK});
    let task = json!({
        "id": TASK,
        "threadId": "thread:parent",
        "runId": parent_run,
        "parentNodeId": projection.runs[0].root_node_id,
        "origin": "app_owned",
        "createdBy": "agent",
        "driver": "codex",
        "providerInstanceId": "codex",
        "providerThreadId": provider_thread.id,
        "childThreadId": "thread:child",
        "nativeTaskRef": null,
        "prompt": "look into it",
        "title": null,
        "model": null,
        "completionWake": wake,
        "status": "running",
        "result": null,
        "startedAt": T1,
        "completedAt": null,
        "updatedAt": T1,
    });
    apply_all(
        &h.projection,
        vec![
            event("thread.created", "thread:child", T1, child),
            event("subagent.updated", "thread:parent", T1, task),
        ],
    )
    .await;
    // The child's first message starts its run.
    h.orchestrator
        .dispatch(message_command("command:child", "thread:child", json!({})))
        .await
        .ok();
    let child_run = h
        .orchestrator
        .get_thread_projection(&thread("thread:child"))
        .await
        .map(|p| p.runs.into_iter().next())
        .ok()
        .flatten();
    let child_run = match child_run {
        Some(run) => run,
        None => {
            // The child thread needs creating through the orchestrator first.
            panic!("child thread has no run");
        }
    };
    let mut value = serde_json::to_value(&child_run).expect("run encodes");
    value["status"] = json!("completed");
    value["completedAt"] = json!(T1);
    value["startedAt"] = json!(T1);
    value["delegatedTaskId"] = json!(TASK);
    apply_all(
        &h.projection,
        vec![
            event("run.updated", "thread:child", T1, value.clone()),
            event(
                "message.updated",
                "thread:child",
                T1,
                message(
                    "thread:child",
                    "message:child-answer",
                    Some(child_run.id.as_str()),
                    "assistant",
                    "The answer is 42.",
                    T1,
                ),
            ),
        ],
    )
    .await;
    serde_json::from_value(value).expect("run decodes")
}

async fn parent_task(h: &Steer) -> turin_contracts::orchestration_v2::OrchestrationV2Subagent {
    h.orchestrator
        .get_thread_projection(&thread("thread:parent"))
        .await
        .expect("projection reads")
        .subagents
        .into_iter()
        .find(|task| task.id.as_str() == TASK)
        .expect("the task")
}

#[tokio::test]
async fn a_finished_child_hands_its_result_to_the_parent_and_claims_the_wake() {
    let h = harness(false, None).await;
    let done = child_with_result(&h, "always").await;
    h.orchestrator.handle_terminal_run(&done).await;

    let task = parent_task(&h).await;
    assert_eq!(task.status, OrchestrationV2SubagentStatus::Completed);
    assert_eq!(task.result.as_deref(), Some("The answer is 42."));
    assert_eq!(
        task.completion_delivery.map(|delivery| delivery.state),
        Some(OrchestrationV2DelegatedCompletionTaskDeliveryState::Claimed)
    );
    let parent = h
        .orchestrator
        .get_thread_projection(&thread("thread:parent"))
        .await
        .expect("projection reads");
    assert!(parent.context_transfers.iter().any(|transfer| {
        transfer.r#type == OrchestrationV2ContextTransferType::SubagentResult
            && transfer.source_thread_id.as_str() == "thread:child"
    }));
    let delivery = parent.runs[0]
        .delegated_completion
        .as_ref()
        .and_then(|cohort| cohort.delivery.as_ref())
        .expect("the parent run owes the wake");
    assert!(delivery.task_ids.iter().any(|id| id.as_str() == TASK));
}

#[tokio::test]
async fn a_settled_only_task_does_not_claim_a_wake_while_the_parent_runs() {
    let h = harness(false, None).await;
    let done = child_with_result(&h, "settled_only").await;
    h.orchestrator.handle_terminal_run(&done).await;

    let task = parent_task(&h).await;
    assert_eq!(task.status, OrchestrationV2SubagentStatus::Completed);
    assert!(
        task.completion_delivery.is_none(),
        "{:?}",
        task.completion_delivery
    );
    let parent = h
        .orchestrator
        .get_thread_projection(&thread("thread:parent"))
        .await
        .expect("projection reads");
    assert!(parent.runs[0].delegated_completion.is_none());
}

#[tokio::test]
async fn finalizing_twice_records_the_result_once() {
    let h = harness(false, None).await;
    child_with_result(&h, "always").await;
    for _ in 0..2 {
        h.orchestrator
            .finalize_app_owned_subagent(&thread("thread:child"), &FinalizeOptions::default())
            .await
            .expect("finalize succeeds");
    }
    let parent = h
        .orchestrator
        .get_thread_projection(&thread("thread:parent"))
        .await
        .expect("projection reads");
    let transfers = parent
        .context_transfers
        .iter()
        .filter(|transfer| transfer.r#type == OrchestrationV2ContextTransferType::SubagentResult)
        .count();
    assert_eq!(transfers, 1);
}

#[tokio::test]
async fn a_thread_that_is_not_a_delegated_child_is_left_alone() {
    let h = harness(false, None).await;
    h.running_run("thread:plain").await;
    h.orchestrator
        .finalize_app_owned_subagent(&thread("thread:plain"), &FinalizeOptions::default())
        .await
        .expect("a plain thread is a no-op");
}
