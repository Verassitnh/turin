//! `run.interrupt`, `thread.stop`, `subagent.stop` and
//! `thread.background-work.settle` (`run_stop.rs`). Upstream drives these
//! through provider-fixture integration suites; here a seeded running run and a
//! fake live session stand in, and the tests read the events and effects.

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::{
    OrchestrationV2ProviderTurnStatus, OrchestrationV2RunAttemptStatus, OrchestrationV2RunStatus,
    OrchestrationV2TurnItem,
};

use super::message_steer_tests::{Steer, T1, harness};
use super::tests::{command_id, message_command, thread};
use crate::pipeline::effect_outbox::{EffectOutbox, EffectRequest};
use crate::pipeline::orchestrator_port::{OrchestratorError, OrchestratorPort};
use crate::projection::test_support::{apply_all, event};

fn command(value: Value) -> turin_contracts::orchestration_v2::OrchestrationV2ServerCommand {
    serde_json::from_value(value).expect("command decodes")
}

fn interrupt(command_id: &str, thread_id: &str, run_id: &str, extra: Value) -> Value {
    let mut body = json!({
        "type": "run.interrupt",
        "commandId": command_id,
        "threadId": thread_id,
        "runId": run_id,
    });
    body.as_object_mut()
        .expect("object")
        .extend(extra.as_object().expect("object").clone());
    body
}

fn cause(error: &OrchestratorError) -> String {
    std::error::Error::source(error).map_or_else(|| error.to_string(), ToString::to_string)
}

fn item_kinds(items: &[OrchestrationV2TurnItem]) -> Vec<&'static str> {
    items
        .iter()
        .filter_map(|item| match item {
            OrchestrationV2TurnItem::RunInterruptRequest { .. } => Some("request"),
            OrchestrationV2TurnItem::RunInterruptResult { .. } => Some("result"),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn interrupting_a_running_turn_records_the_request_and_asks_the_provider_to_stop() {
    let h = harness(true, None).await;
    let run_id = h.running_run("thread:stop-live").await;
    h.orchestrator
        .dispatch(command(interrupt(
            "command:interrupt",
            "thread:stop-live",
            &run_id,
            json!({}),
        )))
        .await
        .expect("the interrupt commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:stop-live"))
        .await
        .expect("projection reads");
    // The provider's terminal event settles the run, so it stays running.
    assert_eq!(projection.runs[0].status, OrchestrationV2RunStatus::Running);
    assert_eq!(item_kinds(&projection.turn_items), vec!["request"]);
    let effects = h.effects("command:interrupt").await;
    let [EffectRequest::ProviderTurnInterrupt { subagent, .. }] = effects.as_slice() else {
        panic!("one interrupt effect: {effects:?}");
    };
    assert!(subagent.is_none());
}

#[tokio::test]
async fn interrupting_a_run_before_its_provider_turn_ends_it_locally_and_cancels_the_start() {
    let h = harness(true, None).await;
    h.orchestrator
        .dispatch(super::tests::create_command(
            "command:create",
            "thread:early",
            "Thread",
        ))
        .await
        .expect("create commits");
    h.orchestrator
        .dispatch(message_command("command:first", "thread:early", json!({})))
        .await
        .expect("the first message starts a run");
    let run_id = h
        .orchestrator
        .get_thread_projection(&thread("thread:early"))
        .await
        .expect("projection reads")
        .runs[0]
        .id
        .to_string();

    h.orchestrator
        .dispatch(command(interrupt(
            "command:early",
            "thread:early",
            &run_id,
            json!({"reason": "changed my mind"}),
        )))
        .await
        .expect("the interrupt commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:early"))
        .await
        .expect("projection reads");
    let run = &projection.runs[0];
    assert_eq!(run.status, OrchestrationV2RunStatus::Interrupted);
    assert_eq!(
        projection.attempts[0].status,
        OrchestrationV2RunAttemptStatus::Interrupted
    );
    assert_eq!(
        item_kinds(&projection.turn_items),
        vec!["request", "result"]
    );
    let request = projection
        .turn_items
        .iter()
        .find_map(|item| match item {
            OrchestrationV2TurnItem::RunInterruptRequest { message, .. } => Some(message.clone()),
            _ => None,
        })
        .expect("request item");
    assert_eq!(request, "changed my mind");
    // The queued `provider-turn.start` effect of the first message is cancelled.
    let started = h
        .outbox
        .list_by_command_id(command_id("command:first"))
        .await
        .expect("effects read");
    assert!(
        started.iter().all(|effect| {
            !matches!(effect.request, EffectRequest::ProviderTurnStart { .. })
                || effect.status == crate::pipeline::effect_outbox::EffectStatus::Cancelled
        }),
        "{started:?}"
    );
    assert!(h.effects("command:early").await.is_empty());
}

#[tokio::test]
async fn interrupting_with_a_dead_session_settles_the_run_locally() {
    let h = harness(true, None).await;
    let run_id = h.running_run("thread:dead").await;
    *h.sessions.runtime.lock() = None;
    h.orchestrator
        .dispatch(command(interrupt(
            "command:dead",
            "thread:dead",
            &run_id,
            json!({}),
        )))
        .await
        .expect("the interrupt commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:dead"))
        .await
        .expect("projection reads");
    assert_eq!(
        projection.runs[0].status,
        OrchestrationV2RunStatus::Interrupted
    );
    assert_eq!(
        projection.attempts[0].status,
        OrchestrationV2RunAttemptStatus::Interrupted
    );
    assert_eq!(
        projection.provider_turns[0].status,
        OrchestrationV2ProviderTurnStatus::Interrupted
    );
    assert_eq!(
        item_kinds(&projection.turn_items),
        vec!["request", "result"]
    );
    // The scope the run captured into is captured once it ends.
    let effects = EffectOutbox::list_by_command_id(
        &h.outbox,
        command_id(&format!("command:effect:checkpoint.capture:{run_id}")),
    )
    .await
    .expect("effects read");
    assert_eq!(effects.len(), 1, "{effects:?}");
}

#[tokio::test]
async fn an_unknown_run_is_not_interruptible() {
    let h = harness(true, None).await;
    h.running_run("thread:nope").await;
    let error = h
        .orchestrator
        .dispatch(command(interrupt(
            "command:nope",
            "thread:nope",
            "run:missing",
            json!({}),
        )))
        .await
        .expect_err("no such run");
    assert_eq!(cause(&error), "Run run:missing is not interruptible.");
}

/// A thread with a running run and a message queued behind it.
async fn running_with_queue(h: &Steer, thread_id: &str) -> String {
    let run_id = h.running_run(thread_id).await;
    h.orchestrator
        .dispatch(message_command(
            &format!("command:queued:{thread_id}"),
            thread_id,
            json!({"text": "later", "messageId": format!("message:later:{thread_id}")}),
        ))
        .await
        .expect("the second message queues");
    run_id
}

#[tokio::test]
async fn stop_interrupts_the_run_and_holds_the_queue() {
    let h = harness(true, None).await;
    let run_id = running_with_queue(&h, "thread:hold").await;
    h.orchestrator
        .dispatch(command(interrupt(
            "command:hold",
            "thread:hold",
            &run_id,
            json!({"holdQueue": true, "reason": "enough"}),
        )))
        .await
        .expect("the stop commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:hold"))
        .await
        .expect("projection reads");
    let queued = projection
        .runs
        .iter()
        .find(|run| run.status == OrchestrationV2RunStatus::Queued)
        .expect("the queued run stays queued");
    assert_eq!(queued.queue_held, Some(true), "Stop holds the queue");
    // Stop also stops every delegated task under the thread, with the reason.
    let effects = h.effects("command:hold").await;
    assert_eq!(effects.len(), 2, "{effects:?}");
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, EffectRequest::ProviderTurnInterrupt { .. }))
    );
    assert!(
        effects.iter().any(|effect| matches!(
            effect,
            EffectRequest::DelegatedTasksStop { reason: Some(reason) } if reason == "enough"
        )),
        "{effects:?}"
    );
}

#[tokio::test]
async fn thread_stop_interrupts_the_running_turn_and_holds_the_queue() {
    let h = harness(true, None).await;
    running_with_queue(&h, "thread:tstop").await;
    h.orchestrator
        .dispatch(command(json!({
            "type": "thread.stop",
            "commandId": "command:tstop",
            "threadId": "thread:tstop",
            "reason": "enough",
        })))
        .await
        .expect("thread.stop commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:tstop"))
        .await
        .expect("projection reads");
    assert!(
        projection
            .runs
            .iter()
            .any(|run| run.queue_held == Some(true)),
        "the queue is held"
    );
    assert!(matches!(
        h.effects("command:tstop").await.as_slice(),
        [EffectRequest::ProviderTurnInterrupt { .. }]
    ));
}

#[tokio::test]
async fn thread_stop_with_nothing_running_is_an_accepted_no_op() {
    let h = harness(true, None).await;
    h.orchestrator
        .dispatch(super::tests::create_command(
            "command:create",
            "thread:idle",
            "Thread",
        ))
        .await
        .expect("create commits");
    let before = h
        .orchestrator
        .get_thread_event_sequence(&thread("thread:idle"))
        .await
        .unwrap();
    let result = h
        .orchestrator
        .dispatch(command(json!({
            "type": "thread.stop",
            "commandId": "command:idle-stop",
            "threadId": "thread:idle",
        })))
        .await
        .expect("an idle stop is accepted");
    assert!(result.stored_events.is_empty());
    assert_eq!(result.sequence as i64, before);
}

/// A running provider-native claude subagent on `run_id`, owned by its
/// provider thread.
async fn seed_native_subagent(h: &Steer, thread_id: &str, run_id: &str, driver: &str) {
    let projection = h
        .orchestrator
        .get_thread_projection(&thread(thread_id))
        .await
        .expect("projection reads");
    let provider_thread = &projection.provider_threads[0];
    let subagent = json!({
        "id": "node:subagent:1",
        "threadId": thread_id,
        "runId": run_id,
        "parentNodeId": projection.runs[0].root_node_id,
        "origin": "provider_native",
        "createdBy": "agent",
        "driver": driver,
        "providerInstanceId": "codex",
        "providerThreadId": provider_thread.id,
        "childThreadId": null,
        "nativeTaskRef": {"driver": driver, "nativeId": "task-1", "strength": "strong"},
        "prompt": "look",
        "title": null,
        "model": null,
        "status": "running",
        "result": null,
        "startedAt": T1,
        "completedAt": null,
        "updatedAt": T1,
    });
    apply_all(
        &h.projection,
        vec![event("subagent.updated", thread_id, T1, subagent)],
    )
    .await;
}

#[tokio::test]
async fn a_native_subagent_stop_marks_the_record_and_interrupts_through_its_task() {
    let h = harness(true, None).await;
    let run_id = h.running_run("thread:sub").await;
    seed_native_subagent(&h, "thread:sub", &run_id, "claudeAgent").await;
    // The subagent's provider thread must be the same driver.
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:sub"))
        .await
        .expect("projection reads");
    let mut provider_thread =
        serde_json::to_value(&projection.provider_threads[0]).expect("thread encodes");
    provider_thread["driver"] = json!("claudeAgent");
    apply_all(
        &h.projection,
        vec![event(
            "provider-thread.updated",
            "thread:sub",
            T1,
            provider_thread,
        )],
    )
    .await;

    h.orchestrator
        .dispatch(command(json!({
            "type": "subagent.stop",
            "commandId": "command:sub-stop",
            "threadId": "thread:sub",
            "runId": run_id,
            "subagentId": "node:subagent:1",
        })))
        .await
        .expect("the stop commits");

    let effects = h.effects("command:sub-stop").await;
    let [
        EffectRequest::ProviderTurnInterrupt {
            subagent: Some(task),
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("one interrupt effect naming the task: {effects:?}");
    };
    assert_eq!(task.native_task_id, "task-1");
    assert_eq!(task.id.as_str(), "node:subagent:1");
    // The record is touched (updatedAt) but not ended: the provider confirms.
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:sub"))
        .await
        .expect("projection reads");
    assert_eq!(
        projection.subagents[0].status,
        turin_contracts::orchestration_v2::OrchestrationV2SubagentStatus::Running
    );
}

#[tokio::test]
async fn a_subagent_that_is_not_a_strong_native_claude_task_cannot_be_stopped() {
    let h = harness(true, None).await;
    let run_id = h.running_run("thread:codex-sub").await;
    seed_native_subagent(&h, "thread:codex-sub", &run_id, "codex").await;
    let error = h
        .orchestrator
        .dispatch(command(json!({
            "type": "subagent.stop",
            "commandId": "command:codex-stop",
            "threadId": "thread:codex-sub",
            "runId": run_id,
            "subagentId": "node:subagent:1",
        })))
        .await
        .expect_err("only claude native subagents stop");
    assert_eq!(
        cause(&error),
        "The requested native subagent cannot be stopped on this run."
    );
}

#[tokio::test]
async fn background_work_settle_ends_a_run_the_provider_stopped_without_a_terminal_event() {
    let h = harness(true, None).await;
    let run_id = h.running_run("thread:settle").await;
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:settle"))
        .await
        .expect("projection reads");
    h.orchestrator
        .dispatch(command(json!({
            "type": "thread.background-work.settle",
            "commandId": "command:settle",
            "threadId": "thread:settle",
            "providerThreadId": projection.provider_threads[0].id,
            "providerTurnId": projection.provider_turns[0].id,
        })))
        .await
        .expect("the settle commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:settle"))
        .await
        .expect("projection reads");
    assert_eq!(projection.runs[0].id.as_str(), run_id);
    assert_eq!(
        projection.runs[0].status,
        OrchestrationV2RunStatus::Interrupted
    );
    assert_eq!(
        projection.provider_turns[0].status,
        OrchestrationV2ProviderTurnStatus::Interrupted
    );
}

#[tokio::test]
async fn background_work_settle_ends_a_native_subagent_with_the_providers_outcome() {
    let h = harness(true, None).await;
    let run_id = h.running_run("thread:sub-settle").await;
    seed_native_subagent(&h, "thread:sub-settle", &run_id, "claudeAgent").await;
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:sub-settle"))
        .await
        .expect("projection reads");
    h.orchestrator
        .dispatch(command(json!({
            "type": "thread.background-work.settle",
            "commandId": "command:sub-settle",
            "threadId": "thread:sub-settle",
            "providerThreadId": projection.provider_threads[0].id,
            "providerTurnId": projection.provider_turns[0].id,
            "subagent": {
                "id": "node:subagent:1",
                "stoppedAt": "2026-01-01T00:02:00.000Z",
                "outcome": {
                    "status": "cancelled",
                    "completedAt": "2026-01-01T00:02:00.000Z",
                    "result": "stopped by user",
                },
            },
        })))
        .await
        .expect("the settle commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:sub-settle"))
        .await
        .expect("projection reads");
    let subagent = &projection.subagents[0];
    assert_eq!(
        subagent.status,
        turin_contracts::orchestration_v2::OrchestrationV2SubagentStatus::Cancelled
    );
    assert_eq!(subagent.result.as_deref(), Some("stopped by user"));
    // The run itself is untouched: only the subagent's work ended.
    assert_eq!(projection.runs[0].status, OrchestrationV2RunStatus::Running);
}
