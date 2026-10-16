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
fn fake_claude(dir: &Path, transcript: &str) -> PathBuf {
    std::fs::create_dir_all(dir).expect("cli dir");
    let script = dir.join("claude");
    std::fs::copy(format!("{FIXTURES}/fake_claude.py"), &script).expect("script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::fs::copy(
        format!("{FIXTURES}/{transcript}"),
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
    harness_with("claude_single_turn.ndjson").await
}

async fn harness_with(transcript: &str) -> Harness {
    let data = tempfile::tempdir().expect("data dir");
    let workspace = tempfile::tempdir().expect("workspace");
    let cli = tempfile::tempdir().expect("cli dir");
    let binary = fake_claude(cli.path(), transcript);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thread_actions_and_project_rename_reach_the_read_model() {
    use super::bus::CommandBus;
    use super::thread_commands::LiveThreadCommands;
    use crate::hooks::use_thread_actions::ThreadCommands as _;
    use turin_contracts::environment::ScopedThreadRef;

    let harness = harness().await;
    let commands = &harness.commands;
    let project_id = commands
        .create_project("Before", harness.workspace.to_str().expect("utf-8"))
        .await
        .expect("create project");
    commands
        .rename_project(&project_id, "After")
        .await
        .expect("rename project");
    let project = harness
        .stores
        .projects
        .get(project_id.clone(), false)
        .await
        .expect("read")
        .expect("project");
    assert_eq!(project.title, "After");

    // A second project on the same folder is refused with the upstream wording.
    let conflict = commands
        .create_project("Again", harness.workspace.to_str().expect("utf-8"))
        .await
        .expect_err("workspace conflict");
    assert!(
        conflict
            .message
            .contains("already exists for workspace root"),
        "{conflict}"
    );

    let thread = commands
        .create_thread(CreateThread {
            project_id,
            title: "Pin me".into(),
            model_selection: json!({"instanceId": "claudeAgent", "model": "claude-sonnet-4-6"}),
            runtime_mode: "auto".into(),
            interaction_mode: "default".into(),
        })
        .await
        .expect("create thread");

    let bus = CommandBus::new(tokio::runtime::Handle::current());
    bus.install(commands.clone());
    let target = ScopedThreadRef {
        environment_id: crate::state::local_environment_id(),
        thread_id: thread.clone(),
    };
    // The blocking seam must run off the runtime's workers.
    let archived_target = target.clone();
    tokio::task::spawn_blocking(move || {
        let mut thread_commands = LiveThreadCommands::new(bus);
        thread_commands.pin(&target, None).expect("pin");
        thread_commands.unpin(&target).expect("unpin");
        thread_commands.archive(&archived_target).expect("archive");
        // Archived threads refuse a second archive: the failure carries the decider's text.
        let failure = thread_commands
            .pin(&archived_target, None)
            .expect_err("pin archived");
        assert!(failure.to_string().contains("is archived"), "{failure}");
    })
    .await
    .expect("blocking task");

    let shell = harness
        .stores
        .projections
        .get_thread_shell(&thread)
        .await
        .expect("read shell")
        .expect("shell");
    assert!(shell.archived_at.is_some(), "archived");
    // `Some(None)` is the explicit "unpinned" the server sends.
    assert_eq!(shell.pinned_at, Some(None), "unpinned before archive");
}

async fn project_and_thread(harness: &Harness) -> turin_contracts::ThreadId {
    let project_id = harness
        .commands
        .create_project("Fixture", harness.workspace.to_str().expect("utf-8"))
        .await
        .expect("create project");
    harness
        .commands
        .create_thread(CreateThread {
            project_id,
            title: "Stop me".into(),
            model_selection: json!({"instanceId": "claudeAgent", "model": "claude-sonnet-4-6"}),
            runtime_mode: "auto".into(),
            interaction_mode: "default".into(),
        })
        .await
        .expect("create thread")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_interrupts_a_running_turn() {
    // The CLI never answers the prompt; it only reacts to the interrupt.
    let harness = harness_with("claude_turn_interrupt.ndjson").await;
    let thread = project_and_thread(&harness).await;
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
    harness
        .commands
        .send_message(SendMessage {
            thread_id: thread.clone(),
            text: "Do not answer immediately.".into(),
            attachments: vec![],
            model_selection: None,
            delivery: Delivery::StartImmediately,
            title_seed: None,
        })
        .await
        .expect("send");

    let mut stopped = false;
    let status = tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(item) = events.next().await {
            let OrchestrationV2DomainEvent::RunUpdated { payload, .. } = item.expect("event").event
            else {
                continue;
            };
            match payload.status {
                OrchestrationV2RunStatus::Running if !stopped => {
                    stopped = true;
                    harness.commands.stop_thread(&thread).await.expect("stop");
                }
                OrchestrationV2RunStatus::Completed
                | OrchestrationV2RunStatus::Failed
                | OrchestrationV2RunStatus::Interrupted
                | OrchestrationV2RunStatus::Cancelled => return payload.status,
                _ => {}
            }
        }
        panic!("event stream ended before the run settled");
    })
    .await
    .expect("run settled in time");
    assert!(stopped, "the run reached running before it settled");
    assert_eq!(status, OrchestrationV2RunStatus::Interrupted);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_approval_is_raised_answered_from_a_command_and_the_turn_finishes() {
    use turin_contracts::orchestration_v2::OrchestrationV2RuntimeRequestStatus;
    use turin_orchestration::pipeline::orchestrator_port::{
        OrchestratorPort as _, RecordField, RecordFilter,
    };

    let harness = harness_with("claude_approval.ndjson").await;
    let project_id = harness
        .commands
        .create_project("Fixture", harness.workspace.to_str().expect("utf-8"))
        .await
        .expect("create project");
    let thread = harness
        .commands
        .create_thread(CreateThread {
            project_id,
            title: "Approve me".into(),
            model_selection: json!({"instanceId": "claudeAgent", "model": "claude-sonnet-4-6"}),
            runtime_mode: "approval-required".into(),
            interaction_mode: "default".into(),
        })
        .await
        .expect("create thread");
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
            event_type: None,
        })
        .await
        .expect("stream");
    harness
        .commands
        .send_message(SendMessage {
            thread_id: thread.clone(),
            text: "Run pwd".into(),
            attachments: vec![],
            model_selection: None,
            delivery: Delivery::StartImmediately,
            title_seed: None,
        })
        .await
        .expect("send");

    let orchestrator = harness.commands.runtime().orchestrator.clone();
    let mut answered = false;
    let status = tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(item) = events.next().await {
            match item.expect("event").event {
                OrchestrationV2DomainEvent::RuntimeRequestUpdated { .. } if !answered => {
                    let records = orchestrator
                        .get_thread_records(
                            &thread,
                            &[RecordField::RuntimeRequests],
                            &RecordFilter::default(),
                        )
                        .await
                        .expect("records");
                    let Some(request) = records.runtime_requests.iter().find(|request| {
                        request.status == OrchestrationV2RuntimeRequestStatus::Pending
                    }) else {
                        continue;
                    };
                    answered = true;
                    harness
                        .commands
                        .respond_to_request(&thread, request.id.as_str(), Some("accept"), None)
                        .await
                        .expect("respond");
                }
                OrchestrationV2DomainEvent::RunUpdated { payload, .. } => match payload.status {
                    OrchestrationV2RunStatus::Completed
                    | OrchestrationV2RunStatus::Failed
                    | OrchestrationV2RunStatus::Interrupted
                    | OrchestrationV2RunStatus::Cancelled => return payload.status,
                    _ => {}
                },
                _ => {}
            }
        }
        panic!("event stream ended before the run settled");
    })
    .await
    .expect("run settled in time");
    assert!(answered, "an approval was raised");
    assert_eq!(status, OrchestrationV2RunStatus::Completed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "WIP: the answered AskUserQuestion turn never settles; under investigation"]
async fn a_user_input_question_is_answered_from_a_command_and_the_turn_finishes() {
    use turin_contracts::orchestration_v2::{
        OrchestrationV2RuntimeRequestStatus, OrchestrationV2TurnItem,
    };
    use turin_orchestration::pipeline::orchestrator_port::{
        OrchestratorPort as _, RecordField, RecordFilter,
    };

    let harness = harness_with("claude_user_input.ndjson").await;
    let thread = project_and_thread(&harness).await;
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
            event_type: None,
        })
        .await
        .expect("stream");
    harness
        .commands
        .send_message(SendMessage {
            thread_id: thread.clone(),
            text: "Ask me something".into(),
            attachments: vec![],
            model_selection: None,
            delivery: Delivery::StartImmediately,
            title_seed: None,
        })
        .await
        .expect("send");

    let orchestrator = harness.commands.runtime().orchestrator.clone();
    let mut answered = false;
    let status = tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(item) = events.next().await {
            match item.expect("event").event {
                OrchestrationV2DomainEvent::RunUpdated { payload, .. } => match payload.status {
                    OrchestrationV2RunStatus::Completed
                    | OrchestrationV2RunStatus::Failed
                    | OrchestrationV2RunStatus::Interrupted
                    | OrchestrationV2RunStatus::Cancelled => return payload.status,
                    _ if !answered => {
                        let records = orchestrator
                            .get_thread_records(
                                &thread,
                                &[RecordField::RuntimeRequests, RecordField::TurnItems],
                                &RecordFilter::default(),
                            )
                            .await
                            .expect("records");
                        let Some(request) = records.runtime_requests.iter().find(|request| {
                            request.status == OrchestrationV2RuntimeRequestStatus::Pending
                        }) else {
                            continue;
                        };
                        let Some(question_id) =
                            records.turn_items.iter().find_map(|item| match item {
                                OrchestrationV2TurnItem::UserInputRequest { questions, .. } => {
                                    questions.first().map(|question| question.id.to_string())
                                }
                                _ => None,
                            })
                        else {
                            // The request lands one event before its turn item.
                            continue;
                        };
                        answered = true;
                        harness
                            .commands
                            .respond_to_request(
                                &thread,
                                request.id.as_str(),
                                None,
                                Some(json!({ question_id: "Blue" })),
                            )
                            .await
                            .expect("respond");
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        panic!("event stream ended before the run settled");
    })
    .await
    .expect("run settled in time");
    assert!(answered, "a question was raised");
    assert_eq!(status, OrchestrationV2RunStatus::Completed);
}
