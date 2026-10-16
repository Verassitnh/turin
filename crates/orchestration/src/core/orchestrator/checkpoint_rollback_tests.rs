//! `checkpoint.rollback` and `checkpoint.rollback.fail`
//! (`checkpoint_rollback.rs`). Upstream covers them in the checkpoint
//! integration suites with real git; here a seeded checkpoint stands in.

use serde_json::{Value, json};

use super::message_steer_tests::{Steer, T1, harness};
use super::tests::{command_id, thread};
use crate::pipeline::effect_outbox::EffectRequest;
use crate::pipeline::orchestrator_port::{OrchestratorError, OrchestratorPort};
use crate::projection::test_support::{apply_all, event};

fn command(value: Value) -> turin_contracts::orchestration_v2::OrchestrationV2ServerCommand {
    serde_json::from_value(value).expect("command decodes")
}

fn cause(error: &OrchestratorError) -> String {
    std::error::Error::source(error).map_or_else(|| error.to_string(), ToString::to_string)
}

/// A running thread with one ready checkpoint on its root scope. Returns the
/// scope and checkpoint ids.
async fn with_checkpoint(h: &Steer, status: &str) -> (String, String) {
    h.running_run("thread:rb").await;
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:rb"))
        .await
        .expect("projection reads");
    let scope = projection.checkpoint_scopes[0].clone();
    let run = &projection.runs[0];
    let checkpoint = json!({
        "id": "checkpoint:rb:1",
        "threadId": "thread:rb",
        "scopeId": scope.id,
        "runId": run.id,
        "nodeId": scope.node_id,
        "parentCheckpointId": null,
        "ordinalWithinScope": 1,
        "appRunOrdinal": 1,
        "ref": "refs/checkpoints/rb-1",
        "status": status,
        "files": [],
        "capturedAt": T1,
    });
    apply_all(
        &h.projection,
        vec![event("checkpoint.captured", "thread:rb", T1, checkpoint)],
    )
    .await;
    (scope.id.to_string(), "checkpoint:rb:1".to_owned())
}

fn rollback(id: &str, scope: &str, checkpoint: &str, restore_files: Option<bool>) -> Value {
    let mut body = json!({
        "type": "checkpoint.rollback",
        "commandId": id,
        "threadId": "thread:rb",
        "scopeId": scope,
        "checkpointId": checkpoint,
    });
    if let Some(restore) = restore_files {
        body["restoreFiles"] = json!(restore);
    }
    body
}

#[tokio::test]
async fn a_rollback_records_the_request_and_queues_the_provider_rewind() {
    let h = harness(false, None).await;
    let (scope, checkpoint) = with_checkpoint(&h, "ready").await;
    h.orchestrator
        .dispatch(command(rollback(
            "command:rb:1",
            &scope,
            &checkpoint,
            Some(false),
        )))
        .await
        .expect("rollback commits");

    let thread_row = h
        .orchestrator
        .get_thread_projection(&thread("thread:rb"))
        .await
        .expect("projection reads")
        .thread;
    assert_eq!(
        thread_row
            .rollback_request_id
            .as_ref()
            .map(|id| id.as_str()),
        Some("command:rb:1")
    );
    let effects = h
        .outbox
        .list_by_command_id(command_id("command:rb:1"))
        .await
        .expect("effects read");
    assert!(
        effects.iter().any(|effect| matches!(
            effect.request,
            EffectRequest::ProviderThreadRollback {
                restore_files: Some(false),
                ..
            }
        )),
        "{effects:?}"
    );
}

#[tokio::test]
async fn a_rollback_to_a_checkpoint_that_is_not_ready_or_unknown_is_refused() {
    let h = harness(false, None).await;
    let (scope, checkpoint) = with_checkpoint(&h, "stale").await;
    let error = h
        .orchestrator
        .dispatch(command(rollback(
            "command:rb:2",
            &scope,
            &checkpoint,
            Some(false),
        )))
        .await
        .expect_err("stale checkpoint refuses");
    assert!(
        cause(&error).contains("cannot be restored"),
        "{}",
        cause(&error)
    );
    let error = h
        .orchestrator
        .dispatch(command(rollback(
            "command:rb:3",
            &scope,
            "checkpoint:nope",
            Some(false),
        )))
        .await
        .expect_err("unknown checkpoint refuses");
    assert!(cause(&error).contains("was not found"), "{}", cause(&error));
}

#[tokio::test]
async fn restoring_files_needs_an_isolated_worktree() {
    let h = harness(false, None).await;
    let (scope, checkpoint) = with_checkpoint(&h, "ready").await;
    // The thread has no worktree, so its files are shared with its project;
    // the unwired claims reader is never reached.
    let error = h
        .orchestrator
        .dispatch(command(rollback("command:rb:4", &scope, &checkpoint, None)))
        .await
        .expect_err("a shared workspace refuses file restore");
    assert!(
        cause(&error).contains("File restore requires an isolated worktree"),
        "{}",
        cause(&error)
    );
}

#[tokio::test]
async fn a_failure_is_recorded_only_for_the_current_rollback() {
    let h = harness(false, None).await;
    let (scope, checkpoint) = with_checkpoint(&h, "ready").await;
    h.orchestrator
        .dispatch(command(rollback(
            "command:rb:5",
            &scope,
            &checkpoint,
            Some(false),
        )))
        .await
        .expect("rollback commits");
    let fail = |id: &str, request: &str| {
        command(json!({
            "type": "checkpoint.rollback.fail",
            "commandId": id,
            "threadId": "thread:rb",
            "requestId": request,
            "message": "provider said no",
        }))
    };
    let error = h
        .orchestrator
        .dispatch(fail("command:rb:fail-old", "command:rb:old"))
        .await
        .expect_err("a superseded rollback is rejected");
    assert!(
        cause(&error).contains("no longer the thread's current rollback"),
        "{}",
        cause(&error)
    );
    h.orchestrator
        .dispatch(fail("command:rb:fail", "command:rb:5"))
        .await
        .expect("the current rollback's failure commits");
    let failure = h
        .orchestrator
        .get_thread_projection(&thread("thread:rb"))
        .await
        .expect("projection reads")
        .thread
        .rollback_failure
        .flatten()
        .expect("the failure is on the thread");
    assert_eq!(failure.message.as_str(), "provider said no");
}
