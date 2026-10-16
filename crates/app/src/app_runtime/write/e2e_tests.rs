//! The write path end to end: a temp data dir, a seeded project, a fake
//! `claude` CLI (a script replaying a recorded transcript; no paid turn), the
//! real composition root and the real client store.
//!
//! `send_message` goes composer command → `message.dispatch` → outbox
//! `provider-turn.start` → ProviderTurnStartService → Claude driver session →
//! fake CLI → ProviderEventIngestor → projections → the open thread feed.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ConversationMessageRole, OrchestrationV2DomainEvent, OrchestrationV2RunStatus,
};
use turin_contracts::settings::ServerSettings;
use turin_orchestration::pipeline::event_sink::StreamOptions;

use super::commands::{Commands, CreateThread, Delivery, SendMessage};
use super::runtime::WriteRuntime;
use crate::app_runtime::Stores;
use crate::app_runtime::tests::{start, thread_id, wait_for};
use futures::StreamExt as _;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// A fake CLI directory: the script plus the transcript it replays.
fn fake_claude(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).expect("cli dir");
    let script = dir.join("claude");
    std::fs::copy(format!("{FIXTURES}/fake_claude.py"), &script).expect("script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::fs::copy(
        format!("{FIXTURES}/claude_single_turn.ndjson"),
        dir.join("transcript.ndjson"),
    )
    .expect("transcript");
    script
}

fn settings_with_claude(binary: &Path) -> ServerSettings {
    serde_json::from_value(json!({
        "providerInstances": {
            "claudeAgent": {
                "driver": "claudeAgent",
                "enabled": true,
                "config": { "binaryPath": binary.display().to_string() }
            }
        }
    }))
    .expect("settings")
}

struct Harness {
    commands: Commands,
    stores: Arc<Stores>,
    _dirs: Vec<tempfile::TempDir>,
    workspace: PathBuf,
}

async fn harness() -> Harness {
    let data = tempfile::tempdir().expect("data dir");
    let workspace = tempfile::tempdir().expect("workspace");
    let cli = tempfile::tempdir().expect("cli dir");
    let binary = fake_claude(cli.path());
    let stores = Arc::new(Stores::open(data.path()).await.expect("stores"));
    let runtime = WriteRuntime::build(stores.clone(), data.path(), settings_with_claude(&binary))
        .await
        .expect("write runtime");
    let workspace_path = workspace.path().to_path_buf();
    Harness {
        commands: Commands::new(runtime),
        stores,
        workspace: workspace_path,
        _dirs: vec![data, workspace, cli],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn send_message_streams_a_fake_claude_reply_into_the_store() {
    let harness = harness().await;
    let commands = &harness.commands;

    let project_id = commands
        .create_project("Fixture", harness.workspace.to_str().expect("utf-8"))
        .await
        .expect("create project");
    let thread = commands
        .create_thread(CreateThread {
            project_id,
            title: "Say hello".into(),
            model_selection: json!({"instanceId": "claudeAgent", "model": "claude-sonnet-4-6"}),
            runtime_mode: "auto".into(),
            interaction_mode: "default".into(),
        })
        .await
        .expect("create thread");

    // The UI side: the read path over the same stores, with the thread open.
    let (client, _serve) = {
        let handle = start(harness.stores.clone());
        (handle, ())
    };
    wait_for(&client, "the shell", |state| {
        state
            .shell_view
            .as_ref()
            .is_some_and(|view| view.threads.len() == 1)
    })
    .await;
    client.open_thread(thread_id(thread.as_str()));
    wait_for(&client, "the empty thread", |state| {
        state
            .thread(&thread_id(thread.as_str()))
            .is_some_and(|entry| entry.state.data.is_some())
    })
    .await;

    // Watch the thread's events before sending, so the terminal run is not missed.
    let after = harness
        .stores
        .sink
        .latest_sequence(Some(thread.clone()))
        .await
        .expect("sequence");
    let mut events = harness
        .stores
        .sink
        .stream(StreamOptions {
            thread_id: Some(thread.clone()),
            after_sequence: Some(after),
            event_type: Some("run.updated".to_owned()),
        })
        .await
        .expect("stream");

    let receipt = commands
        .send_message(SendMessage {
            thread_id: thread.clone(),
            text: "Respond with exactly: first fixture turn complete".into(),
            attachments: vec![],
            model_selection: None,
            delivery: Delivery::StartImmediately,
            title_seed: None,
        })
        .await
        .expect("send");
    assert!(receipt.sequence > 0);

    let status = tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(item) = events.next().await {
            if let OrchestrationV2DomainEvent::RunUpdated { payload, .. } =
                item.expect("event").event
            {
                match payload.status {
                    OrchestrationV2RunStatus::Completed
                    | OrchestrationV2RunStatus::Failed
                    | OrchestrationV2RunStatus::Interrupted
                    | OrchestrationV2RunStatus::Cancelled => return payload.status,
                    _ => {}
                }
            }
        }
        panic!("event stream ended before the run finished");
    })
    .await
    .expect("run finished in time");
    assert_eq!(status, OrchestrationV2RunStatus::Completed);

    // The store's snapshot (what the thread view renders) has both messages.
    let state = wait_for(&client, "the assistant reply", |state| {
        state
            .thread(&thread_id(thread.as_str()))
            .is_some_and(|entry| {
                entry.state.data.as_ref().is_some_and(|data| {
                    data.messages.iter().any(|message| {
                        message.role == OrchestrationV2ConversationMessageRole::Assistant
                            && message.text.contains("first fixture turn complete")
                    })
                })
            })
    })
    .await;
    let data = state
        .thread(&thread_id(thread.as_str()))
        .and_then(|entry| entry.state.data.clone())
        .expect("thread data");
    let user = data
        .messages
        .iter()
        .find(|message| message.role == OrchestrationV2ConversationMessageRole::User)
        .expect("user message");
    assert_eq!(
        user.text,
        "Respond with exactly: first fixture turn complete"
    );
    assert_eq!(user.id, receipt.message_id);
}
