//! Codex through the real composition root against `tests/fixtures/fake_codex.py`,
//! launched as the instance's `binaryPath` (`codex app-server`, JSON-RPC over stdio).

use serde_json::json;

use super::{
    Smoke, assert_approval_round_trip, assert_reply_lands, assert_stop_interrupts, install_script,
    model, smoke,
};

async fn codex_smoke() -> Smoke {
    let cli = tempfile::tempdir().expect("cli dir");
    let binary = install_script(cli.path(), "fake_codex.py", "codex");
    let settings = json!({
        "providerInstances": {
            "codex": {
                "driver": "codex",
                "enabled": true,
                "config": { "enabled": true, "binaryPath": binary.display().to_string() }
            }
        }
    });
    smoke(settings, model("codex", "gpt-6-luna"), vec![cli]).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn codex_streams_a_fake_app_server_reply_into_the_store() {
    assert_reply_lands(&codex_smoke().await, "codex fixture reply").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn codex_stop_interrupts_the_running_turn() {
    assert_stop_interrupts(
        &codex_smoke().await,
        "Please hang until interrupted",
        "turn/interrupt",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn codex_command_approval_is_answered_and_the_turn_finishes() {
    assert_approval_round_trip(&codex_smoke().await, "Run pwd", "approval accept").await;
}
