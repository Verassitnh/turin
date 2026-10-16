//! The queued-run commands (`queued_run.rs`): reorder, cancel, edit and
//! promote-to-steer. Upstream covers them in the queue suites with real
//! providers; here the decisions run on a seeded running run.

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::{
    OrchestrationV2RunStatus, OrchestrationV2TurnItem, OrchestrationV2UserMessageInputIntent,
};

use super::message_steer_tests::{Steer, harness};
use super::tests::{message_command, thread};
use crate::pipeline::effect_outbox::EffectRequest;
use crate::pipeline::orchestrator_port::{OrchestratorError, OrchestratorPort};

fn command(value: Value) -> turin_contracts::orchestration_v2::OrchestrationV2ServerCommand {
    serde_json::from_value(value).expect("command decodes")
}

fn cause(error: &OrchestratorError) -> String {
    std::error::Error::source(error).map_or_else(|| error.to_string(), ToString::to_string)
}

/// A running run with `count` messages queued behind it. Returns the queued
/// run ids in queue order.
async fn queued(h: &Steer, thread_id: &str, count: usize) -> Vec<String> {
    h.running_run(thread_id).await;
    for index in 0..count {
        h.orchestrator
            .dispatch(message_command(
                &format!("command:queue-{index}:{thread_id}"),
                thread_id,
                json!({"text": format!("queued {index}"), "messageId": format!("message:q{index}:{thread_id}")}),
            ))
            .await
            .expect("the message queues");
    }
    let projection = h
        .orchestrator
        .get_thread_projection(&thread(thread_id))
        .await
        .expect("projection reads");
    projection
        .runs
        .iter()
        .filter(|run| run.status == OrchestrationV2RunStatus::Queued)
        .map(|run| run.id.to_string())
        .collect()
}

async fn order(h: &Steer, thread_id: &str) -> Vec<(String, u64)> {
    let mut rows: Vec<_> = h
        .orchestrator
        .get_thread_projection(&thread(thread_id))
        .await
        .expect("projection reads")
        .runs
        .into_iter()
        .filter(|run| run.status == OrchestrationV2RunStatus::Queued)
        .map(|run| {
            (
                run.id.to_string(),
                run.queue_position.flatten().expect("a position").get(),
            )
        })
        .collect();
    rows.sort_by_key(|(_, position)| *position);
    rows
}

#[tokio::test]
async fn reorder_moves_a_queued_run_before_another() {
    let h = harness(true, None).await;
    let runs = queued(&h, "thread:reorder", 3).await;
    h.orchestrator
        .dispatch(command(json!({
            "type": "queued-run.reorder",
            "commandId": "command:reorder",
            "threadId": "thread:reorder",
            "runId": runs[2],
            "beforeRunId": runs[0],
        })))
        .await
        .expect("the reorder commits");
    let after: Vec<_> = order(&h, "thread:reorder")
        .await
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(
        after,
        vec![runs[2].clone(), runs[0].clone(), runs[1].clone()]
    );

    // Moving the first run to the end (no target).
    h.orchestrator
        .dispatch(command(json!({
            "type": "queued-run.reorder",
            "commandId": "command:reorder-last",
            "threadId": "thread:reorder",
            "runId": runs[2],
            "beforeRunId": null,
        })))
        .await
        .expect("the reorder commits");
    let last: Vec<_> = order(&h, "thread:reorder")
        .await
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(
        last,
        vec![runs[0].clone(), runs[1].clone(), runs[2].clone()]
    );
}

#[tokio::test]
async fn reorder_refuses_a_run_that_is_not_queued_and_an_unknown_target() {
    let h = harness(true, None).await;
    let runs = queued(&h, "thread:badorder", 2).await;
    let not_queued = h
        .orchestrator
        .dispatch(command(json!({
            "type": "queued-run.reorder",
            "commandId": "command:bad-run",
            "threadId": "thread:badorder",
            "runId": "run:missing",
            "beforeRunId": null,
        })))
        .await
        .expect_err("no such queued run");
    assert_eq!(cause(&not_queued), "Run run:missing is not queued.");
    let bad_target = h
        .orchestrator
        .dispatch(command(json!({
            "type": "queued-run.reorder",
            "commandId": "command:bad-target",
            "threadId": "thread:badorder",
            "runId": runs[0],
            "beforeRunId": "run:elsewhere",
        })))
        .await
        .expect_err("no such target");
    assert_eq!(
        cause(&bad_target),
        "Queue target run:elsewhere is not queued."
    );
}

#[tokio::test]
async fn cancel_ends_the_queued_run_and_refuses_one_that_is_not_queued() {
    let h = harness(true, None).await;
    let runs = queued(&h, "thread:cancel", 1).await;
    h.orchestrator
        .dispatch(command(json!({
            "type": "queued-run.cancel",
            "commandId": "command:cancel",
            "threadId": "thread:cancel",
            "runId": runs[0],
        })))
        .await
        .expect("the cancel commits");
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:cancel"))
        .await
        .expect("projection reads");
    assert_eq!(
        projection.runs[1].status,
        OrchestrationV2RunStatus::Cancelled
    );
    assert_eq!(
        projection.attempts[1].status,
        turin_contracts::orchestration_v2::OrchestrationV2RunAttemptStatus::Cancelled
    );

    let error = h
        .orchestrator
        .dispatch(command(json!({
            "type": "queued-run.cancel",
            "commandId": "command:cancel-again",
            "threadId": "thread:cancel",
            "runId": runs[0],
        })))
        .await
        .expect_err("a cancelled run is not queued");
    assert_eq!(cause(&error), format!("Run {} is not queued.", runs[0]));
}

#[tokio::test]
async fn edit_rewrites_the_queued_message() {
    let h = harness(true, None).await;
    let runs = queued(&h, "thread:edit", 1).await;
    let result = h
        .orchestrator
        .dispatch(command(json!({
            "type": "queued-run.edit",
            "commandId": "command:edit",
            "threadId": "thread:edit",
            "runId": runs[0],
            "text": "edited text",
        })))
        .await
        .expect("the edit commits");
    // A queued message has no turn item until its run starts, so only the
    // message is rewritten.
    assert_eq!(result.stored_events.len(), 1);
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:edit"))
        .await
        .expect("projection reads");
    assert!(
        projection
            .messages
            .iter()
            .any(|message| message.text == "edited text")
    );
    assert!(
        !projection
            .messages
            .iter()
            .any(|message| message.text == "queued 0")
    );
}

#[tokio::test]
async fn an_empty_edit_is_rejected_for_the_sender_and_replays() {
    let h = harness(true, None).await;
    let runs = queued(&h, "thread:empty", 1).await;
    let body = json!({
        "type": "queued-run.edit",
        "commandId": "command:edit-empty",
        "threadId": "thread:empty",
        "runId": runs[0],
        "text": "   ",
    });
    let error = h
        .orchestrator
        .dispatch(command(body.clone()))
        .await
        .expect_err("an empty edit is refused");
    // The sender of `queued-run.edit` gets a plain rejection, not the generic failure.
    assert!(
        matches!(error, OrchestratorError::CommandRejected { .. }),
        "{error:?}"
    );
    assert_eq!(
        cause(&error),
        format!(
            "Queued run {} cannot be edited to an empty message.",
            runs[0]
        )
    );
}

#[tokio::test]
async fn promote_to_steer_cancels_the_queued_run_and_steers_its_message_in() {
    let h = harness(true, None).await;
    let runs = queued(&h, "thread:promote", 1).await;
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:promote"))
        .await
        .expect("projection reads");
    let target = projection.runs[0].id.to_string();
    h.orchestrator
        .dispatch(command(json!({
            "type": "queued-message.promote-to-steer",
            "commandId": "command:promote",
            "threadId": "thread:promote",
            "queuedRunId": runs[0],
            "targetRunId": target,
        })))
        .await
        .expect("the promotion commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:promote"))
        .await
        .expect("projection reads");
    assert_eq!(
        projection.runs[1].status,
        OrchestrationV2RunStatus::Cancelled
    );
    let intent = projection.turn_items.iter().find_map(|item| match item {
        OrchestrationV2TurnItem::UserMessage {
            text, input_intent, ..
        } if text == "queued 0" => Some(*input_intent),
        _ => None,
    });
    assert_eq!(
        intent,
        Some(OrchestrationV2UserMessageInputIntent::PromotedQueuedToSteer)
    );
    assert!(matches!(
        h.effects("command:promote").await.as_slice(),
        [EffectRequest::ProviderTurnSteer { .. }]
    ));
}
