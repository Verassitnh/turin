//! Delegated-completion cohort disposal (`cohort.rs`) through archive.
//! Upstream has no isolated test for `disposeAllDelegatedCompletionCohorts`;
//! it runs inside the archive path, so that is where it is exercised.

use serde_json::json;
use turin_contracts::orchestration_v2::{
    OrchestrationV2DelegatedCompletionCohortDisposition,
    OrchestrationV2DelegatedCompletionTaskDeliveryState,
};

use super::message_plan_tests::open_delivery;
use super::message_steer_tests::harness;
use super::tests::thread;
use crate::pipeline::orchestrator_port::OrchestratorPort;

#[tokio::test]
async fn archive_disposes_the_delegated_completion_cohort_and_its_undelivered_tasks() {
    let h = harness(false, None).await;
    let run_id = h.running_run("thread:cohort").await;
    open_delivery(&h, "thread:cohort", &run_id, "always").await;

    h.orchestrator
        .dispatch(
            serde_json::from_value(json!({
                "type": "thread.archive",
                "commandId": "command:archive-cohort",
                "threadId": "thread:cohort",
            }))
            .expect("thread.archive decodes"),
        )
        .await
        .expect("archive commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:cohort"))
        .await
        .expect("projection reads");
    let cohort = projection.runs[0]
        .delegated_completion
        .as_ref()
        .expect("the run keeps its cohort record");
    assert_eq!(
        cohort.disposition,
        OrchestrationV2DelegatedCompletionCohortDisposition::Disposed
    );
    assert!(cohort.delivery.is_none(), "the pending delivery is dropped");
    assert_eq!(
        cohort.next_generation.get(),
        2,
        "the generation counter is kept"
    );
    let delivery = projection.subagents[0]
        .completion_delivery
        .as_ref()
        .expect("the task records its disposal");
    assert_eq!(
        delivery.state,
        OrchestrationV2DelegatedCompletionTaskDeliveryState::Disposed
    );
    assert!(projection.thread.archived_at.is_some());
}
