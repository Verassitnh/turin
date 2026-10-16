//! `thread.model-selection.set`, `provider.switch` and `thread.auto-settle`
//! (`thread_mutation.rs`, `thread_archive.rs`). Upstream covers the switch
//! through `ProviderSwitch.integration.test.ts` with real providers.

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::OrchestrationV2ProviderSessionStatus;

use super::message_steer_tests::{Steer, harness};
use super::tests::{create_command, thread};
use crate::pipeline::effect_outbox::EffectRequest;
use crate::pipeline::orchestrator_port::{OrchestratorError, OrchestratorPort};

fn command(value: Value) -> turin_contracts::orchestration_v2::OrchestrationV2ServerCommand {
    serde_json::from_value(value).expect("command decodes")
}

fn cause(error: &OrchestratorError) -> String {
    std::error::Error::source(error).map_or_else(|| error.to_string(), ToString::to_string)
}

fn select(kind: &str, command_id: &str, thread_id: &str, instance: &str, model: &str) -> Value {
    json!({
        "type": kind,
        "commandId": command_id,
        "threadId": thread_id,
        "modelSelection": {"instanceId": instance, "model": model},
    })
}

async fn created(h: &Steer, thread_id: &str) {
    h.orchestrator
        .dispatch(create_command("command:create", thread_id, "Thread"))
        .await
        .expect("create commits");
}

#[tokio::test]
async fn a_model_change_on_the_same_instance_updates_the_selection() {
    let h = harness(true, None).await;
    created(&h, "thread:model").await;
    let result = h
        .orchestrator
        .dispatch(command(select(
            "thread.model-selection.set",
            "command:model",
            "thread:model",
            "codex",
            "gpt-next",
        )))
        .await
        .expect("the change commits");
    let types: Vec<_> = result
        .stored_events
        .iter()
        .map(|stored| serde_json::to_value(&stored.event).unwrap()["type"].clone())
        .collect();
    assert_eq!(types, vec![json!("thread.model-selection-updated")]);
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:model"))
        .await
        .expect("projection reads");
    assert_eq!(
        serde_json::to_value(&projection.thread.model_selection).unwrap()["model"],
        "gpt-next"
    );
}

#[tokio::test]
async fn provider_switch_always_records_a_provider_switch() {
    let h = harness(true, None).await;
    created(&h, "thread:switch").await;
    let result = h
        .orchestrator
        .dispatch(command(select(
            "provider.switch",
            "command:switch",
            "thread:switch",
            "codex",
            "gpt-same-instance",
        )))
        .await
        .expect("the switch commits");
    assert_eq!(
        serde_json::to_value(&result.stored_events[0].event).unwrap()["type"],
        "thread.provider-switched"
    );
}

#[tokio::test]
async fn an_unknown_target_instance_is_refused() {
    let h = harness(true, None).await;
    created(&h, "thread:nowhere").await;
    let error = h
        .orchestrator
        .dispatch(command(select(
            "thread.model-selection.set",
            "command:nowhere",
            "thread:nowhere",
            "mystery",
            "m",
        )))
        .await
        .expect_err("no such provider instance");
    assert!(
        matches!(error, OrchestratorError::ProviderAdapter { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn the_sessions_the_plan_releases_are_detached() {
    let h = harness(true, None).await;
    let run_id = h.running_run("thread:release").await;
    let _ = run_id;
    let projection = h
        .orchestrator
        .get_thread_projection(&thread("thread:release"))
        .await
        .expect("projection reads");
    let session_id = projection.provider_threads[0]
        .provider_session_id
        .clone()
        .expect("session");
    // Attach the session to the projection so the switch can see it live.
    let session = json!({
        "id": session_id,
        "driver": "codex",
        "providerInstanceId": "codex",
        "status": "running",
        "cwd": "/turin-fixture/project",
        "model": "gpt-test",
        "capabilities": h.capabilities,
        "createdAt": super::message_steer_tests::T1,
        "updatedAt": super::message_steer_tests::T1,
        "lastError": null,
    });
    crate::projection::test_support::apply_all(
        &h.projection,
        vec![crate::projection::test_support::event(
            "provider-session.attached",
            "thread:release",
            super::message_steer_tests::T1,
            session,
        )],
    )
    .await;
    h.planner.release.lock().push(session_id.clone());

    let result = h
        .orchestrator
        .dispatch(command(select(
            "thread.model-selection.set",
            "command:release",
            "thread:release",
            "codex",
            "gpt-restart",
        )))
        .await
        .expect("the change commits");
    let types: Vec<_> = result
        .stored_events
        .iter()
        .map(|stored| serde_json::to_value(&stored.event).unwrap()["type"].clone())
        .collect();
    assert_eq!(
        types,
        vec![
            json!("thread.model-selection-updated"),
            json!("provider-session.detached")
        ]
    );
    assert!(matches!(
        h.effects("command:release").await.as_slice(),
        [EffectRequest::ProviderSessionDetach { provider_session_id, .. }]
            if provider_session_id == &session_id
    ));
    let _ = OrchestrationV2ProviderSessionStatus::Stopped;
}

#[tokio::test]
async fn auto_settle_settles_an_untouched_thread_and_yields_to_later_changes() {
    let h = harness(true, None).await;
    created(&h, "thread:auto").await;
    let thread_row = h
        .orchestrator
        .get_thread_projection(&thread("thread:auto"))
        .await
        .expect("projection reads")
        .thread;
    // A snapshot older than the thread's last change loses to that change.
    let stale = h
        .orchestrator
        .dispatch(command(json!({
            "type": "thread.auto-settle",
            "commandId": "command:auto-stale",
            "threadId": "thread:auto",
            "snapshotAt": "2020-01-01T00:00:00.000Z",
        })))
        .await
        .expect_err("the thread changed after the snapshot");
    assert_eq!(
        cause(&stale),
        "Thread thread:auto changed before automatic settlement."
    );

    h.orchestrator
        .dispatch(command(json!({
            "type": "thread.auto-settle",
            "commandId": "command:auto",
            "threadId": "thread:auto",
            "snapshotAt": thread_row.updated_at,
        })))
        .await
        .expect("an untouched thread settles");
    let settled = h
        .orchestrator
        .get_thread_projection(&thread("thread:auto"))
        .await
        .expect("projection reads")
        .thread;
    assert!(settled.settled_at.is_some());
}
