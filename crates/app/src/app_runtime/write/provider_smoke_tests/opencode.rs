//! OpenCode through the real composition root against
//! `tests/fixtures/fake_opencode.py`: the provider probes it with
//! `--version`, launches `opencode serve --hostname= --port=`, waits for the
//! readiness line, then talks HTTP + SSE to it (v1 API).

use serde_json::json;

use super::{
    Smoke, assert_approval_round_trip, assert_reply_lands, assert_stop_interrupts, install_script,
    model, smoke,
};

async fn opencode_smoke() -> Smoke {
    let cli = tempfile::tempdir().expect("cli dir");
    let binary = install_script(cli.path(), "fake_opencode.py", "opencode");
    // The folded form the settings service stores: the enabled flag lives on the
    // envelope only (`fold_provider_instance_enabled_flags` strips `config.enabled`).
    let settings = json!({
        "providerInstances": {
            "opencode": {
                "driver": "opencode",
                "enabled": true,
                "config": { "binaryPath": binary.display().to_string() }
            }
        }
    });
    smoke(settings, model("opencode", "openai/gpt"), vec![cli]).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opencode_streams_a_fake_server_reply_into_the_store() {
    assert_reply_lands(&opencode_smoke().await, "opencode fixture reply").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opencode_stop_aborts_the_running_session() {
    assert_stop_interrupts(
        &opencode_smoke().await,
        "Please hang until aborted",
        "/abort",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opencode_permission_request_is_answered_and_the_turn_finishes() {
    assert_approval_round_trip(&opencode_smoke().await, "Run pwd", "permission once").await;
}
