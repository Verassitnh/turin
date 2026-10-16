//! The start branch that carries context (`message_transfer.rs`) and the queue
//! start's handoff (`queue_start.rs`): a provider switch, a pending fork, a
//! pending merge-back. Upstream covers them in `ProviderSwitch.integration.test.ts`,
//! `ThreadFork.integration.test.ts` and `ThreadMergeBack.integration.test.ts`
//! with real providers; here the decisions run on seeded completed runs.

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::{
    OrchestrationV2ContextTransferResolution, OrchestrationV2ContextTransferStatus,
    OrchestrationV2ContextTransferType, OrchestrationV2Run, OrchestrationV2RunStatus,
    OrchestrationV2TurnItem,
};

use super::message_steer_tests::{Steer, T1, harness};
use super::tests::{message_command, thread};
use crate::pipeline::effect_outbox::EffectRequest;
use crate::pipeline::orchestrator_port::OrchestratorPort;
use crate::projection::test_support::{apply_all, event};

fn command(value: Value) -> turin_contracts::orchestration_v2::OrchestrationV2ServerCommand {
    serde_json::from_value(value).expect("command decodes")
}

fn claude() -> Value {
    json!({"instanceId": "claudeAgent", "model": "claude-test"})
}

/// Ends `run` as the provider would, with a checkpoint so it can be forked.
async fn complete(h: &Steer, thread_id: &str, run: &OrchestrationV2Run) -> OrchestrationV2Run {
    let mut value = serde_json::to_value(run).expect("run encodes");
    value["status"] = json!("completed");
    value["completedAt"] = json!(T1);
    value["checkpointId"] = json!(format!("checkpoint:{}", run.id));
    apply_all(
        &h.projection,
        vec![event("run.updated", thread_id, T1, value.clone())],
    )
    .await;
    serde_json::from_value(value).expect("run decodes")
}

/// A thread whose first run, on codex, has completed. Returns that run.
async fn completed_thread(h: &Steer, thread_id: &str) -> OrchestrationV2Run {
    h.running_run(thread_id).await;
    let run = h
        .orchestrator
        .get_thread_projection(&thread(thread_id))
        .await
        .expect("projection reads")
        .runs
        .remove(0);
    complete(h, thread_id, &run).await
}

#[tokio::test]
async fn a_message_on_another_provider_starts_with_a_handoff_of_the_conversation() {
    let h = harness(true, None).await;
    completed_thread(&h, "thread:handoff").await;
    h.orchestrator
        .dispatch(message_command(
            "command:next",
            "thread:handoff",
            json!({"text": "continue", "messageId": "message:next", "modelSelection": claude()}),
        ))
        .await
        .expect("the switch starts a run");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:handoff"))
        .await
        .expect("projection reads");
    let [_, next] = projection.runs.as_slice() else {
        panic!("two runs: {:?}", projection.runs);
    };
    assert_eq!(next.provider_instance_id.as_str(), "claudeAgent");
    assert_eq!(next.status, OrchestrationV2RunStatus::Starting);
    let handoff_id = next.context_handoff_id.clone().expect("a handoff");
    let handoff = projection
        .context_handoffs
        .iter()
        .find(|handoff| handoff.id == handoff_id)
        .expect("the handoff is recorded");
    assert!(
        handoff.summary_text.contains("hello"),
        "the first turn is summarized"
    );
    let transfer = projection
        .context_transfers
        .iter()
        .find(|transfer| transfer.r#type == OrchestrationV2ContextTransferType::ProviderHandoff)
        .expect("the switch records its transfer");
    assert_eq!(
        transfer.status,
        OrchestrationV2ContextTransferStatus::Consumed
    );
    assert!(matches!(
        transfer.resolution,
        Some(OrchestrationV2ContextTransferResolution::PortableContext { .. })
    ));
    assert!(projection.turn_items.iter().any(|item| matches!(
        item,
        OrchestrationV2TurnItem::Handoff { title, .. } if title.as_deref() == Some("Provider handoff")
    )));
    // Two provider threads now: codex's and the new claude one.
    assert_eq!(projection.provider_threads.len(), 2);
    assert!(matches!(
        h.effects("command:next").await.as_slice(),
        [EffectRequest::ProviderTurnStart { .. }]
    ));
}

#[tokio::test]
async fn switching_back_hands_over_only_what_the_target_has_not_seen() {
    let h = harness(true, None).await;
    let first = completed_thread(&h, "thread:roundtrip").await;
    h.orchestrator
        .dispatch(message_command(
            "command:to-claude",
            "thread:roundtrip",
            json!({"text": "on claude", "messageId": "message:claude", "modelSelection": claude()}),
        ))
        .await
        .expect("the switch starts a run");
    let second = h
        .orchestrator
        .get_thread_projection(&thread("thread:roundtrip"))
        .await
        .expect("projection reads")
        .runs
        .remove(1);
    complete(&h, "thread:roundtrip", &second).await;
    let _ = first;
    h.orchestrator
        .dispatch(message_command(
            "command:back",
            "thread:roundtrip",
            json!({"text": "back on codex", "messageId": "message:back",
                   "modelSelection": {"instanceId": "codex", "model": "gpt-test"}}),
        ))
        .await
        .expect("the switch back starts a run");
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:roundtrip"))
        .await
        .expect("projection reads");
    let third = projection.runs.last().expect("third run");
    assert_eq!(third.provider_instance_id.as_str(), "codex");
    let transfers: Vec<_> = projection
        .context_transfers
        .iter()
        .filter(|transfer| transfer.r#type == OrchestrationV2ContextTransferType::ProviderHandoff)
        .collect();
    assert_eq!(transfers.len(), 2);
    // Codex already saw run 1, so only the claude run is handed over.
    assert!(matches!(
        transfers[1].resolution,
        Some(OrchestrationV2ContextTransferResolution::DeltaContext { .. })
    ));
    assert_eq!(
        projection.provider_threads.len(),
        2,
        "codex's thread is reused"
    );
}

#[tokio::test]
async fn a_pending_fork_gives_the_new_thread_its_source_conversation() {
    let h = harness(true, None).await;
    let run = completed_thread(&h, "thread:parent").await;
    h.orchestrator
        .dispatch(command(json!({
            "type": "thread.fork",
            "commandId": "command:fork",
            "createdBy": "user",
            "creationSource": "web",
            "sourceThreadId": "thread:parent",
            "targetThreadId": "thread:child",
            "sourcePoint": {"type": "run", "runId": run.id},
        })))
        .await
        .expect("the fork commits");
    h.orchestrator
        .dispatch(message_command(
            "command:in-fork",
            "thread:child",
            json!({"text": "carry on", "messageId": "message:in-fork"}),
        ))
        .await
        .expect("the first message in the fork starts a run");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:child"))
        .await
        .expect("projection reads");
    let transfer = projection
        .context_transfers
        .iter()
        .find(|transfer| transfer.r#type == OrchestrationV2ContextTransferType::Fork)
        .expect("the fork transfer");
    assert_eq!(
        transfer.status,
        OrchestrationV2ContextTransferStatus::Consumed
    );
    assert!(matches!(
        transfer.resolution,
        Some(OrchestrationV2ContextTransferResolution::PortableContext { .. })
    ));
    assert!(projection.turn_items.iter().any(|item| matches!(
        item,
        OrchestrationV2TurnItem::Handoff { title, .. } if title.as_deref() == Some("Fork context")
    )));
    assert_eq!(
        projection.runs[0].status,
        OrchestrationV2RunStatus::Starting
    );
}

#[tokio::test]
async fn a_pending_merge_back_gives_the_parent_the_forks_delta() {
    let h = harness(true, None).await;
    let run = completed_thread(&h, "thread:mb-parent").await;
    h.orchestrator
        .dispatch(command(json!({
            "type": "thread.fork",
            "commandId": "command:mb-fork",
            "createdBy": "user",
            "creationSource": "web",
            "sourceThreadId": "thread:mb-parent",
            "targetThreadId": "thread:mb-child",
            "sourcePoint": {"type": "run", "runId": run.id},
        })))
        .await
        .expect("the fork commits");
    h.orchestrator
        .dispatch(message_command(
            "command:mb-child",
            "thread:mb-child",
            json!({"text": "fork work", "messageId": "message:mb-child"}),
        ))
        .await
        .expect("the fork starts a run");
    let child_run = h
        .orchestrator
        .get_thread_projection(&thread("thread:mb-child"))
        .await
        .expect("projection reads")
        .runs
        .remove(0);
    let child_run = complete(&h, "thread:mb-child", &child_run).await;
    h.orchestrator
        .dispatch(command(json!({
            "type": "thread.merge_back",
            "commandId": "command:merge",
            "createdBy": "user",
            "creationSource": "web",
            "sourceThreadId": "thread:mb-child",
            "targetThreadId": "thread:mb-parent",
            "sourcePoint": {"type": "run", "runId": child_run.id},
        })))
        .await
        .expect("the merge-back commits");

    h.orchestrator
        .dispatch(message_command(
            "command:after-merge",
            "thread:mb-parent",
            json!({"text": "use the fork's work", "messageId": "message:after-merge"}),
        ))
        .await
        .expect("the next message consumes the merge-back");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:mb-parent"))
        .await
        .expect("projection reads");
    let transfer = projection
        .context_transfers
        .iter()
        .find(|transfer| transfer.r#type == OrchestrationV2ContextTransferType::MergeBack)
        .expect("the merge-back transfer");
    assert_eq!(
        transfer.status,
        OrchestrationV2ContextTransferStatus::Consumed
    );
    assert!(matches!(
        transfer.resolution,
        Some(OrchestrationV2ContextTransferResolution::ForkDeltaContext { .. })
    ));
    let handoff_id = projection
        .runs
        .last()
        .unwrap()
        .context_handoff_id
        .clone()
        .expect("handoff");
    let handoff = projection
        .context_handoffs
        .iter()
        .find(|handoff| handoff.id == handoff_id)
        .expect("recorded");
    assert!(
        handoff
            .summary_text
            .starts_with("Merge-back context from forked conversation.")
    );
}

#[tokio::test]
async fn a_queued_message_for_another_provider_starts_with_a_handoff() {
    let h = harness(true, None).await;
    // A running codex run with a claude message queued behind it.
    h.running_run("thread:queued-switch").await;
    h.orchestrator
        .dispatch(message_command(
            "command:queued",
            "thread:queued-switch",
            json!({"text": "later on claude", "messageId": "message:later", "modelSelection": claude()}),
        ))
        .await
        .expect("the message queues");
    let running = h
        .orchestrator
        .get_thread_projection(&thread("thread:queued-switch"))
        .await
        .expect("projection reads")
        .runs
        .remove(0);
    let done = complete(&h, "thread:queued-switch", &running).await;

    h.orchestrator.handle_terminal_run(&done).await;

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:queued-switch"))
        .await
        .expect("projection reads");
    let next = projection.runs.last().expect("the queued run");
    assert_eq!(next.status, OrchestrationV2RunStatus::Starting);
    assert_eq!(next.provider_instance_id.as_str(), "claudeAgent");
    assert!(
        next.context_handoff_id.is_some(),
        "the queued start carries a handoff"
    );
    assert!(projection.context_transfers.iter().any(|transfer| {
        transfer.r#type == OrchestrationV2ContextTransferType::ProviderHandoff
            && transfer.status == OrchestrationV2ContextTransferStatus::Consumed
    }));
}
