//! `recordSubagentFollowup` and the provider-native subagent guard
//! (`subagent_followup.rs`).

use serde_json::json;
use turin_contracts::orchestration_v2::OrchestrationV2SubagentStatus;

use super::delegated_task_tests::child_with_result;
use super::message_steer_tests::{T1, harness};
use super::tests::{message_command, thread};
use crate::pipeline::orchestrator_port::{OrchestratorError, OrchestratorPort};
use crate::projection::test_support::{apply_all, event};

#[tokio::test]
async fn a_follow_up_after_the_result_becomes_its_own_task_on_the_parent() {
    let h = harness(false, None).await;
    let done = child_with_result(&h, "always").await;
    h.orchestrator.handle_terminal_run(&done).await;

    h.orchestrator
        .dispatch(message_command(
            "command:follow-up",
            "thread:child",
            json!({"text": "and one more thing"}),
        ))
        .await
        .expect("the follow-up commits");

    let parent = h
        .orchestrator
        .get_thread_projection(&thread("thread:parent"))
        .await
        .expect("projection reads");
    assert_eq!(parent.subagents.len(), 2, "{:?}", parent.subagents);
    let follow_up = parent
        .subagents
        .iter()
        .find(|task| task.prompt == "and one more thing")
        .expect("the follow-up task");
    assert_eq!(follow_up.status, OrchestrationV2SubagentStatus::Running);
    assert!(follow_up.result.is_none());
    let child = h
        .orchestrator
        .get_thread_projection(&thread("thread:child"))
        .await
        .expect("child reads");
    assert!(
        child
            .runs
            .iter()
            .any(|run| run.delegated_task_id.as_ref() == Some(&follow_up.id))
    );
}

#[tokio::test]
async fn a_follow_up_before_any_result_stays_part_of_the_original_task() {
    let h = harness(false, None).await;
    child_with_result(&h, "always").await;
    h.orchestrator
        .dispatch(message_command(
            "command:early",
            "thread:child",
            json!({"text": "early note"}),
        ))
        .await
        .expect("the message commits");
    let parent = h
        .orchestrator
        .get_thread_projection(&thread("thread:parent"))
        .await
        .expect("projection reads");
    assert_eq!(parent.subagents.len(), 1);
}

#[tokio::test]
async fn a_provider_native_subagent_thread_takes_no_messages() {
    let h = harness(false, None).await;
    let mut native = crate::projection::test_support::thread("thread:native");
    native["creationSource"] = json!("provider");
    native["lineage"] = json!({
        "parentThreadId": "thread:parent",
        "relationshipToParent": "subagent",
        "rootThreadId": "thread:parent",
    });
    h.running_run("thread:parent").await;
    apply_all(
        &h.projection,
        vec![event("thread.created", "thread:native", T1, native)],
    )
    .await;
    let error = h
        .orchestrator
        .dispatch(message_command(
            "command:native",
            "thread:native",
            json!({}),
        ))
        .await
        .expect_err("a provider-run subagent is read only");
    assert!(
        matches!(error, OrchestratorError::SubagentThreadReadOnly { .. }),
        "{error:?}"
    );
}
