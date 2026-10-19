//! Cursor (ACP) through the real composition root against
//! `tests/fixtures/fake_cursor_agent.py`, launched as the instance's
//! `binaryPath` (the program `cursor-agent acp` would be).

use serde_json::json;

use super::{
    Smoke, assert_approval_round_trip, assert_reply_lands, assert_stop_interrupts, install_script,
    model, smoke,
};

async fn cursor_smoke() -> Smoke {
    let cli = tempfile::tempdir().expect("cli dir");
    let binary = install_script(cli.path(), "fake_cursor_agent.py", "cursor-agent");
    // The folded form the settings service stores: the enabled flag lives on the
    // envelope only (`fold_provider_instance_enabled_flags` strips `config.enabled`).
    let settings = json!({
        "providerInstances": {
            "cursor": {
                "driver": "cursor",
                "enabled": true,
                "config": { "binaryPath": binary.display().to_string() }
            }
        }
    });
    smoke(settings, model("cursor", "composer-2"), vec![cli]).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cursor_streams_a_fake_acp_reply_into_the_store() {
    assert_reply_lands(&cursor_smoke().await, "cursor fixture reply").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cursor_stop_cancels_the_running_acp_prompt() {
    assert_stop_interrupts(
        &cursor_smoke().await,
        "Please hang until cancelled",
        "session/cancel",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cursor_permission_request_is_answered_and_the_turn_finishes() {
    assert_approval_round_trip(&cursor_smoke().await, "Run pwd", "permission once").await;
}
