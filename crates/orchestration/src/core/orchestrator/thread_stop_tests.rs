//! Upstream `ThreadStop.test.ts`: Stop ends the thread's pull request watches,
//! holds its queue, and refuses what a stopped run still tries to start. The
//! tests that drive `ThreadManagementService.stopDelegatedTasks` live with that
//! service; the restart-continuation case is in `message_restart_tests.rs`.

use serde_json::{Value, json};

use super::message_steer_tests::{Steer, T1, harness};
use super::tests::{create_command, message_command, thread};
use crate::pipeline::effect_outbox::EffectRequest;
use crate::pipeline::orchestrator_port::{OrchestratorError, OrchestratorPort};
use crate::projection::test_support::{apply_all, event, turn_item};

const REPO: &str = "pingdotgg/t3code";

fn command(value: Value) -> turin_contracts::orchestration_v2::OrchestrationV2ServerCommand {
    serde_json::from_value(value).expect("command decodes")
}

fn watch_command(id: &str, thread_id: &str, number: u64, watching: bool, link: bool) -> Value {
    let mut body = json!({
        "type": "thread.pull-request.watch",
        "commandId": id,
        "threadId": thread_id,
        "host": "github.com",
        "repository": REPO,
        "number": number,
        "watching": watching,
    });
    if link {
        body["link"] = json!({
            "url": format!("https://github.com/{REPO}/pull/{number}"),
            "source": "agent",
        });
    }
    body
}

async fn watch(h: &Steer, thread_id: &str, number: u64) -> Result<(), OrchestratorError> {
    h.orchestrator
        .dispatch(command(watch_command(
            &format!("command:watch:{thread_id}:{number}"),
            thread_id,
            number,
            true,
            true,
        )))
        .await
        .map(|_| ())
}

/// `runs` statuses with a held-queue marker, and the watched pull requests.
async fn thread_state(h: &Steer, thread_id: &str) -> (Vec<String>, Vec<u64>) {
    let projection = h
        .orchestrator
        .get_thread_projection(&thread(thread_id))
        .await
        .expect("projection reads");
    let runs = projection
        .runs
        .iter()
        .map(|run| {
            let status = serde_json::to_value(run.status).expect("status encodes");
            format!(
                "{}{}",
                status.as_str().unwrap_or_default(),
                if run.queue_held == Some(true) {
                    ":held"
                } else {
                    ""
                }
            )
        })
        .collect();
    let watched = projection
        .thread
        .pull_requests
        .unwrap_or_default()
        .iter()
        .filter(|link| link.watch.is_some())
        .map(|link| link.number.get())
        .collect();
    (runs, watched)
}

async fn watching_idle_thread(h: &Steer, thread_id: &str, number: u64) {
    h.orchestrator
        .dispatch(create_command(
            &format!("command:create:{thread_id}"),
            thread_id,
            "Thread",
        ))
        .await
        .expect("create commits");
    watch(h, thread_id, number).await.expect("the watch starts");
}

async fn set_run(h: &Steer, thread_id: &str, patch: Value) {
    let run = h
        .orchestrator
        .get_thread_projection(&thread(thread_id))
        .await
        .expect("projection reads")
        .runs
        .remove(0);
    let mut value = serde_json::to_value(&run).expect("run encodes");
    for (key, field) in patch.as_object().expect("patch is an object") {
        value[key] = field.clone();
    }
    apply_all(
        &h.projection,
        vec![event("run.updated", thread_id, T1, value)],
    )
    .await;
}

fn delegate(id: &str, thread_id: &str, run_id: &str, node_id: &Value) -> Value {
    json!({
        "type": "delegated_task.request",
        "createdBy": "agent",
        "creationSource": "mcp",
        "commandId": id,
        "parentThreadId": thread_id,
        "parentRunId": run_id,
        "parentNodeId": node_id,
        "task": "late task",
        "modelSelection": {"instanceId": "codex", "model": "gpt-test"},
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "completionWake": "always",
    })
}

#[tokio::test]
async fn stop_ends_watches_and_holds_the_queue() {
    let h = harness(true, None).await;
    let run_id = h.running_run("thread:stop-watch").await;
    watch(&h, "thread:stop-watch", 1)
        .await
        .expect("the watch starts");
    h.orchestrator
        .dispatch(message_command(
            "command:queued",
            "thread:stop-watch",
            json!({"dispatchMode": {"type": "queue_after_active"}}),
        ))
        .await
        .expect("the message queues");
    assert_eq!(thread_state(&h, "thread:stop-watch").await.1, vec![1]);

    h.orchestrator
        .dispatch(command(json!({
            "type": "run.interrupt",
            "commandId": "command:stop-watch",
            "threadId": "thread:stop-watch",
            "runId": run_id,
            "holdQueue": true,
        })))
        .await
        .expect("the stop commits");

    let (runs, watched) = thread_state(&h, "thread:stop-watch").await;
    assert_eq!(runs, vec!["running", "queued:held"]);
    assert!(watched.is_empty(), "Stop ends the watches");
    assert!(
        h.effects("command:stop-watch")
            .await
            .iter()
            .any(|effect| matches!(effect, EffectRequest::DelegatedTasksStop { .. })),
        "Stop also stops the delegated tasks"
    );
}

#[tokio::test]
async fn thread_stop_ends_an_idle_threads_watches_and_accepts_a_thread_with_nothing_to_stop() {
    let h = harness(true, None).await;
    watching_idle_thread(&h, "thread:stop-idle", 3).await;
    h.orchestrator
        .dispatch(command(json!({
            "type": "thread.stop", "commandId": "command:stop-idle", "threadId": "thread:stop-idle",
        })))
        .await
        .expect("the stop commits");
    assert_eq!(thread_state(&h, "thread:stop-idle").await, (vec![], vec![]));

    let again = h
        .orchestrator
        .dispatch(command(json!({
            "type": "thread.stop", "commandId": "command:stop-idle-again",
            "threadId": "thread:stop-idle",
        })))
        .await
        .expect("an idle stop is accepted");
    assert!(again.stored_events.is_empty());
}

#[tokio::test]
async fn a_run_stop_reached_cannot_delegate_or_start_a_watch_even_after_it_ends() {
    let h = harness(true, None).await;
    let run_id = h.running_run("thread:stop-barrier").await;
    watch(&h, "thread:stop-barrier", 4)
        .await
        .expect("the watch starts");
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:stop-barrier"))
        .await
        .expect("projection reads");
    let root = json!(projection.runs[0].root_node_id);
    // Stop reached the run, but its provider has not stopped it yet.
    let request = turn_item(
        "thread:stop-barrier",
        "turn-item:stop-barrier:interrupt-request",
        Some(&run_id),
        1_000,
        "run_interrupt_request",
        "completed",
        json!({"message": "Stop"}),
    );
    apply_all(
        &h.projection,
        vec![event(
            "turn-item.updated",
            "thread:stop-barrier",
            T1,
            request,
        )],
    )
    .await;

    let error = h
        .orchestrator
        .dispatch(command(delegate(
            "command:late",
            "thread:stop-barrier",
            &run_id,
            &root,
        )))
        .await
        .expect_err("a stopped run cannot delegate");
    assert!(
        std::error::Error::source(&error).is_some_and(|c| c.to_string().contains("is stopping"))
    );
    assert!(watch(&h, "thread:stop-barrier", 5).await.is_err());

    // A slow watch call can land after the stopped run ended.
    set_run(
        &h,
        "thread:stop-barrier",
        json!({"status": "interrupted", "completedAt": T1}),
    )
    .await;
    assert!(watch(&h, "thread:stop-barrier", 5).await.is_err());
    assert_eq!(
        thread_state(&h, "thread:stop-barrier").await,
        (vec!["interrupted".to_owned()], vec![4])
    );

    // The user can still stop and restart a watch by hand.
    for watching in [false, true] {
        h.orchestrator
            .dispatch(command(watch_command(
                &format!("command:manual-watch:{watching}"),
                "thread:stop-barrier",
                4,
                watching,
                false,
            )))
            .await
            .expect("a manual watch change commits");
    }
    assert_eq!(thread_state(&h, "thread:stop-barrier").await.1, vec![4]);
}

#[tokio::test]
async fn thread_stop_on_a_finished_thread_refuses_a_late_agent_watch() {
    let h = harness(true, None).await;
    h.running_run("thread:stop-finished").await;
    watch(&h, "thread:stop-finished", 9)
        .await
        .expect("the watch starts");
    set_run(
        &h,
        "thread:stop-finished",
        json!({"status": "completed", "completedAt": T1}),
    )
    .await;
    h.orchestrator
        .dispatch(command(json!({
            "type": "thread.stop", "commandId": "command:stop-finished",
            "threadId": "thread:stop-finished",
        })))
        .await
        .expect("the stop commits");
    assert!(watch(&h, "thread:stop-finished", 10).await.is_err());
    assert_eq!(
        thread_state(&h, "thread:stop-finished").await,
        (vec!["completed".to_owned()], vec![])
    );
}

#[tokio::test]
async fn thread_stop_marks_a_turn_it_cannot_interrupt_so_a_late_agent_watch_is_refused() {
    let h = harness(true, None).await;
    h.running_run("thread:stop-lost").await;
    watch(&h, "thread:stop-lost", 11)
        .await
        .expect("the watch starts");
    // The turn is running, but its provider session is gone, so the interrupt fails.
    *h.sessions.runtime.lock() = None;
    h.orchestrator
        .dispatch(command(json!({
            "type": "thread.stop", "commandId": "command:stop-lost", "threadId": "thread:stop-lost",
        })))
        .await
        .expect("the stop commits");
    assert!(watch(&h, "thread:stop-lost", 12).await.is_err());
    let (_, watched) = thread_state(&h, "thread:stop-lost").await;
    assert!(watched.is_empty());
}
