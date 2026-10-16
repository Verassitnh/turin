//! The prepared-run commands (`prepared_run.rs`): progress, release, fail and
//! retry of a run that waits in `preparing` for its workspace. Upstream
//! covers them through `ThreadLaunchService.test.ts`.

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::{
    ORCHESTRATION_V2_WORKSPACE_PREPARATION_FAILURE_CODE, OrchestrationV2RunStatus,
    OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
};

use super::message_steer_tests::{Steer, harness};
use super::tests::{create_command, message_command, thread};
use crate::pipeline::effect_outbox::EffectRequest;
use crate::pipeline::orchestrator_port::{OrchestratorError, OrchestratorPort};

fn command(value: Value) -> turin_contracts::orchestration_v2::OrchestrationV2ServerCommand {
    serde_json::from_value(value).expect("command decodes")
}

fn cause(error: &OrchestratorError) -> String {
    std::error::Error::source(error).map_or_else(|| error.to_string(), ToString::to_string)
}

/// A thread with one deferred run waiting in `preparing`. Returns its run id.
async fn preparing(h: &Steer, thread_id: &str) -> String {
    h.orchestrator
        .dispatch(create_command("command:create", thread_id, "Thread"))
        .await
        .expect("create commits");
    h.orchestrator
        .dispatch(message_command(
            "command:defer",
            thread_id,
            json!({"dispatchMode": {"type": "defer_start", "workspaceStrategy": {"type": "worktree", "baseRef": "main"}}}),
        ))
        .await
        .expect("the deferred start commits");
    h.orchestrator
        .get_thread_projection(&thread(thread_id))
        .await
        .expect("projection reads")
        .runs[0]
        .id
        .to_string()
}

fn prepared(kind: &str, command_id: &str, thread_id: &str, run_id: &str, extra: Value) -> Value {
    let mut body = json!({
        "type": kind,
        "commandId": command_id,
        "threadId": thread_id,
        "runId": run_id,
    });
    body.as_object_mut()
        .expect("object")
        .extend(extra.as_object().expect("object").clone());
    body
}

async fn items(h: &Steer, thread_id: &str) -> Vec<OrchestrationV2TurnItem> {
    h.orchestrator
        .get_thread_projection(&thread(thread_id))
        .await
        .expect("projection reads")
        .turn_items
}

fn preparation(items: &[OrchestrationV2TurnItem]) -> &OrchestrationV2TurnItem {
    items
        .iter()
        .find(|item| {
            matches!(item, OrchestrationV2TurnItem::CommandExecution { input, .. } if input == "Preparing workspace")
        })
        .expect("the preparation item")
}

#[tokio::test]
async fn progress_retitles_the_preparation_item_by_phase() {
    let h = harness(true, None).await;
    let run_id = preparing(&h, "thread:prep-progress").await;
    for (phase, title) in [
        ("worktree", "Preparing worktree"),
        ("setup", "Starting setup script"),
    ] {
        h.orchestrator
            .dispatch(command(prepared(
                "prepared-run.progress",
                &format!("command:progress-{phase}"),
                "thread:prep-progress",
                &run_id,
                json!({"phase": phase}),
            )))
            .await
            .expect("the progress commits");
        let listed = items(&h, "thread:prep-progress").await;
        let OrchestrationV2TurnItem::CommandExecution {
            title: item_title, ..
        } = preparation(&listed)
        else {
            unreachable!()
        };
        assert_eq!(item_title.as_deref(), Some(title));
    }
}

#[tokio::test]
async fn release_starts_the_run_with_a_scope_and_a_turn_effect() {
    let h = harness(true, None).await;
    let run_id = preparing(&h, "thread:prep-release").await;
    h.orchestrator
        .dispatch(command(prepared(
            "prepared-run.release",
            "command:release",
            "thread:prep-release",
            &run_id,
            json!({}),
        )))
        .await
        .expect("the release commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:prep-release"))
        .await
        .expect("projection reads");
    assert_eq!(
        projection.runs[0].status,
        OrchestrationV2RunStatus::Starting
    );
    assert!(projection.nodes[0].checkpoint_scope_id.is_some());
    let OrchestrationV2TurnItem::CommandExecution {
        status,
        exit_code,
        title,
        ..
    } = preparation(&projection.turn_items)
    else {
        unreachable!()
    };
    assert_eq!(*status, OrchestrationV2TurnItemStatus::Completed);
    assert_eq!(*exit_code, Some(0));
    assert_eq!(title.as_deref(), Some("Workspace ready"));
    assert!(matches!(
        h.effects("command:release").await.as_slice(),
        [EffectRequest::ProviderTurnStart { .. }]
    ));
    // A released run is no longer awaiting preparation.
    let again = h
        .orchestrator
        .dispatch(command(prepared(
            "prepared-run.release",
            "command:release-again",
            "thread:prep-release",
            &run_id,
            json!({}),
        )))
        .await
        .expect_err("already released");
    assert_eq!(
        cause(&again),
        format!("Run {run_id} is not awaiting workspace preparation.")
    );
}

fn failure() -> Value {
    json!({"class": "unknown", "message": "git worktree add failed", "code": null, "retryable": null})
}

#[tokio::test]
async fn a_failed_preparation_ends_the_run_with_a_coded_error_item() {
    let h = harness(true, None).await;
    let run_id = preparing(&h, "thread:prep-fail").await;
    h.orchestrator
        .dispatch(command(prepared(
            "prepared-run.fail",
            "command:fail",
            "thread:prep-fail",
            &run_id,
            json!({"failure": failure()}),
        )))
        .await
        .expect("the failure commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:prep-fail"))
        .await
        .expect("projection reads");
    assert_eq!(projection.runs[0].status, OrchestrationV2RunStatus::Failed);
    let coded = projection.turn_items.iter().any(|item| {
        matches!(
            item,
            OrchestrationV2TurnItem::Error { failure, .. }
                if failure.code.as_ref().map(|code| code.as_str())
                    == Some(ORCHESTRATION_V2_WORKSPACE_PREPARATION_FAILURE_CODE)
        )
    });
    assert!(coded, "the error item carries the preparation failure code");
    let OrchestrationV2TurnItem::CommandExecution {
        output, exit_code, ..
    } = preparation(&projection.turn_items)
    else {
        unreachable!()
    };
    assert_eq!(output.as_deref(), Some("git worktree add failed"));
    assert_eq!(*exit_code, Some(1));
}

#[tokio::test]
async fn retry_returns_a_failed_preparation_to_preparing_and_cancels_the_failure_item() {
    let h = harness(true, None).await;
    let run_id = preparing(&h, "thread:prep-retry").await;
    // Not failed yet: nothing to retry.
    let early = h
        .orchestrator
        .dispatch(command(prepared(
            "prepared-run.retry",
            "command:retry-early",
            "thread:prep-retry",
            &run_id,
            json!({}),
        )))
        .await
        .expect_err("a preparing run has no failure to retry");
    assert_eq!(
        cause(&early),
        format!("Run {run_id} has no failed workspace preparation to retry.")
    );

    h.orchestrator
        .dispatch(command(prepared(
            "prepared-run.fail",
            "command:fail",
            "thread:prep-retry",
            &run_id,
            json!({"failure": failure()}),
        )))
        .await
        .expect("the failure commits");
    h.orchestrator
        .dispatch(command(prepared(
            "prepared-run.retry",
            "command:retry",
            "thread:prep-retry",
            &run_id,
            json!({}),
        )))
        .await
        .expect("the retry commits");

    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:prep-retry"))
        .await
        .expect("projection reads");
    assert_eq!(
        projection.runs[0].status,
        OrchestrationV2RunStatus::Preparing
    );
    let OrchestrationV2TurnItem::CommandExecution {
        status,
        output,
        exit_code,
        ..
    } = preparation(&projection.turn_items)
    else {
        unreachable!()
    };
    assert_eq!(*status, OrchestrationV2TurnItemStatus::Running);
    assert!(
        output.is_none() && exit_code.is_none(),
        "the old result is cleared"
    );
    assert!(projection.turn_items.iter().any(|item| matches!(
        item,
        OrchestrationV2TurnItem::Error { status, .. }
            if *status == OrchestrationV2TurnItemStatus::Cancelled
    )));
}
