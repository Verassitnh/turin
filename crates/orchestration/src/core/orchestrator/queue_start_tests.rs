//! Starting the next queued run (`queue_start.rs`, upstream
//! `startNextQueuedRun`) after a run ends or the queue is resumed. Upstream
//! covers it in the queued-run and restart suites with real providers.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use turin_contracts::{
    ProjectId,
    orchestration_v2::{OrchestrationV2RunStatus, OrchestrationV2TurnItem},
    provider_instance::ProviderInstanceId,
};

use super::message_steer_tests::{Steer, T1, harness, harness_with_settings};
use super::queue_start::QueueStartOptions;
use super::services::ProviderInstanceSettings;
use super::tests::{message_command, thread};
use crate::pipeline::effect_outbox::EffectRequest;
use crate::pipeline::orchestrator_port::{BoxError, OrchestratorPort};
use crate::projection::test_support::{apply_all, event};

/// A running run with a message queued behind it. Returns the running run
/// as the projection holds it.
async fn running_with_queue(
    h: &Steer,
    thread_id: &str,
) -> turin_contracts::orchestration_v2::OrchestrationV2Run {
    h.running_run(thread_id).await;
    h.orchestrator
        .dispatch(message_command(
            &format!("command:queued:{thread_id}"),
            thread_id,
            json!({"text": "later", "messageId": format!("message:later:{thread_id}")}),
        ))
        .await
        .expect("the second message queues");
    h.orchestrator
        .get_thread_projection(&thread(thread_id))
        .await
        .expect("projection reads")
        .runs
        .remove(0)
}

/// Ends `run` as the provider's terminal event would.
async fn complete(
    h: &Steer,
    thread_id: &str,
    run: &turin_contracts::orchestration_v2::OrchestrationV2Run,
) -> turin_contracts::orchestration_v2::OrchestrationV2Run {
    let mut value = serde_json::to_value(run).expect("run encodes");
    value["status"] = json!("completed");
    value["completedAt"] = json!(T1);
    apply_all(
        &h.projection,
        vec![event("run.updated", thread_id, T1, value.clone())],
    )
    .await;
    serde_json::from_value(value).expect("run decodes")
}

#[tokio::test]
async fn a_finished_run_starts_the_next_queued_run() {
    let h = harness(true, None).await;
    let running = running_with_queue(&h, "thread:next").await;
    let done = complete(&h, "thread:next", &running).await;

    h.orchestrator.handle_terminal_run(&done).await;

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:next"))
        .await
        .expect("projection reads");
    let [_, next] = projection.runs.as_slice() else {
        panic!("two runs: {:?}", projection.runs);
    };
    assert_eq!(next.status, OrchestrationV2RunStatus::Starting);
    assert!(
        projection.turn_items.iter().any(|item| matches!(
            item,
            OrchestrationV2TurnItem::UserMessage { text, input_intent, .. }
                if text == "later"
                    && *input_intent == turin_contracts::orchestration_v2::OrchestrationV2UserMessageInputIntent::QueuedTurn
        )),
        "the queued message is delivered as a queued turn"
    );
    let effects = h
        .outbox
        .list_by_command_id(
            turin_contracts::CommandId::new(format!("command:system:start-queued:{}", next.id))
                .expect("command id"),
        )
        .await
        .expect("effects read");
    let [effect] = effects.as_slice() else {
        panic!("one start effect: {effects:?}");
    };
    assert!(matches!(
        &effect.request,
        EffectRequest::ProviderTurnStart { run_id } if run_id == &next.id
    ));
}

#[tokio::test]
async fn nothing_starts_while_a_run_is_still_active() {
    let h = harness(true, None).await;
    running_with_queue(&h, "thread:busy").await;
    h.orchestrator
        .start_next_queued_run(&thread("thread:busy"), &QueueStartOptions::default())
        .await
        .expect("a busy thread is a no-op");
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:busy"))
        .await
        .expect("projection reads");
    assert_eq!(projection.runs[1].status, OrchestrationV2RunStatus::Queued);
}

#[tokio::test]
async fn resuming_a_held_queue_starts_the_next_run() {
    let h = harness(true, None).await;
    let running = running_with_queue(&h, "thread:resume").await;
    // Stop holds the queue; the run ends; the held queue does not start.
    h.orchestrator
        .dispatch(
            serde_json::from_value(json!({
                "type": "run.interrupt",
                "commandId": "command:hold",
                "threadId": "thread:resume",
                "runId": running.id,
                "holdQueue": true,
            }))
            .expect("decodes"),
        )
        .await
        .expect("stop commits");
    let done = complete(&h, "thread:resume", &running).await;
    h.orchestrator.handle_terminal_run(&done).await;
    let held = h
        .orchestrator
        .get_thread_projection(&thread("thread:resume"))
        .await
        .expect("projection reads");
    assert_eq!(held.runs[1].status, OrchestrationV2RunStatus::Queued);
    assert_eq!(held.runs[1].queue_held, Some(true));

    h.orchestrator
        .dispatch(
            serde_json::from_value(json!({
                "type": "queue.resume",
                "commandId": "command:resume",
                "threadId": "thread:resume",
            }))
            .expect("decodes"),
        )
        .await
        .expect("resume commits");

    let resumed = h
        .orchestrator
        .get_thread_projection(&thread("thread:resume"))
        .await
        .expect("projection reads");
    assert_eq!(resumed.runs[1].status, OrchestrationV2RunStatus::Starting);
    assert!(
        !h.outbox
            .list_by_command_id(
                turin_contracts::CommandId::new(format!(
                    "command:system:start-queued:{}",
                    resumed.runs[1].id
                ))
                .expect("command id")
            )
            .await
            .expect("effects read")
            .is_empty()
    );
}

/// Every instance is turned off for every project.
struct AllOff;

#[async_trait]
impl ProviderInstanceSettings for AllOff {
    async fn disabled_for_project(
        &self,
        _project_id: &ProjectId,
        _instance_id: &ProviderInstanceId,
    ) -> Result<bool, BoxError> {
        Ok(true)
    }
}

#[tokio::test]
async fn a_provider_the_project_turned_off_holds_the_queue() {
    let h = harness_with_settings(true, true, None, Arc::new(AllOff)).await;
    let running = running_with_queue(&h, "thread:off").await;
    let done = complete(&h, "thread:off", &running).await;

    h.orchestrator.handle_terminal_run(&done).await;

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:off"))
        .await
        .expect("projection reads");
    assert_eq!(projection.runs[1].status, OrchestrationV2RunStatus::Queued);
    assert_eq!(projection.runs[1].queue_held, Some(true));
}

#[tokio::test]
async fn resuming_an_archived_thread_is_refused() {
    let h = harness(true, None).await;
    h.running_run("thread:gone").await;
    h.orchestrator
        .dispatch(
            serde_json::from_value(json!({
                "type": "thread.archive",
                "commandId": "command:archive",
                "threadId": "thread:gone",
            }))
            .expect("decodes"),
        )
        .await
        .expect("archive commits");
    let error = h
        .orchestrator
        .dispatch(
            serde_json::from_value(json!({
                "type": "queue.resume",
                "commandId": "command:resume-archived",
                "threadId": "thread:gone",
            }))
            .expect("decodes"),
        )
        .await
        .expect_err("an archived thread has no queue to resume");
    assert_eq!(
        std::error::Error::source(&error).map(ToString::to_string),
        Some("Thread thread:gone is not active.".to_owned())
    );
}
