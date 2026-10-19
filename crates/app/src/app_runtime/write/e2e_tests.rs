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
    // The app's own loader: the fake binary override plus the Claude text-generation default.
    let empty = tempfile::tempdir().expect("settings dir");
    super::settings::load_server_settings(empty.path(), binary.to_str())
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

    // Sidebar message search finds the thread by its reply.
    let found = commands
        .search_messages("fixture turn", 10)
        .await
        .expect("search");
    assert!(found.iter().any(|hit| hit.thread_id == thread), "{found:?}");
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
#[ignore = "provider-claude emits no user_input_request turn item (register_request drops the control request body), so the question id is unknowable; see docs/handoff/T22.md"]
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
            if let OrchestrationV2DomainEvent::RunUpdated { payload, .. } =
                item.expect("event").event
                && matches!(
                    payload.status,
                    OrchestrationV2RunStatus::Completed
                        | OrchestrationV2RunStatus::Failed
                        | OrchestrationV2RunStatus::Interrupted
                        | OrchestrationV2RunStatus::Cancelled
                )
            {
                return payload.status;
            }
            if answered {
                continue;
            }
            let records = orchestrator
                .get_thread_records(
                    &thread,
                    &[RecordField::RuntimeRequests, RecordField::TurnItems],
                    &RecordFilter::default(),
                )
                .await
                .expect("records");
            let Some(request) = records
                .runtime_requests
                .iter()
                .find(|request| request.status == OrchestrationV2RuntimeRequestStatus::Pending)
            else {
                continue;
            };
            // The request lands one event before its turn item.
            let Some(question_id) = records.turn_items.iter().find_map(|item| match item {
                OrchestrationV2TurnItem::UserInputRequest { questions, .. } => {
                    questions.first().map(|question| question.id.to_string())
                }
                _ => None,
            }) else {
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
        panic!("event stream ended before the run settled");
    })
    .await
    .expect("run settled in time");
    assert!(answered, "a question was raised");
    assert_eq!(status, OrchestrationV2RunStatus::Completed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_runs_can_be_reordered_edited_and_cancelled() {
    use turin_contracts::orchestration_v2::OrchestrationV2ConversationMessageRole as Role;
    use turin_orchestration::pipeline::orchestrator_port::{
        OrchestratorPort as _, RecordField, RecordFilter,
    };

    // The CLI never answers the first prompt, so later sends queue behind it.
    let harness = harness_with("claude_turn_interrupt.ndjson").await;
    let thread = project_and_thread(&harness).await;
    let send = |text: &'static str, delivery: Delivery| {
        let commands = harness.commands.clone();
        let thread_id = thread.clone();
        async move {
            commands
                .send_message(SendMessage {
                    thread_id,
                    text: text.into(),
                    attachments: vec![],
                    model_selection: None,
                    delivery,
                    title_seed: None,
                })
                .await
                .expect("send")
        }
    };
    send("first", Delivery::StartImmediately).await;
    send("second", Delivery::QueueAfterActive).await;
    send("third", Delivery::QueueAfterActive).await;

    let orchestrator = harness.commands.runtime().orchestrator.clone();
    // Queued runs in queue order, as `(run id, message text)`.
    let queue = || {
        let orchestrator = orchestrator.clone();
        let thread = thread.clone();
        async move {
            let records = orchestrator
                .get_thread_records(
                    &thread,
                    &[RecordField::Runs, RecordField::Messages],
                    &RecordFilter::default(),
                )
                .await
                .expect("records");
            let mut queued: Vec<_> = records
                .runs
                .iter()
                .filter(|run| run.status == OrchestrationV2RunStatus::Queued)
                .collect();
            queued.sort_by_key(|run| {
                let ordinal = run.ordinal.get();
                (
                    run.queue_position
                        .flatten()
                        .map_or(ordinal, |position| position.get()),
                    ordinal,
                )
            });
            queued
                .into_iter()
                .map(|run| {
                    let text = records
                        .messages
                        .iter()
                        .find(|message| {
                            message.run_id.as_ref() == Some(&run.id) && message.role == Role::User
                        })
                        .map(|message| message.text.clone())
                        .unwrap_or_default();
                    (run.id.to_string(), text)
                })
                .collect::<Vec<_>>()
        }
    };
    let initial = queue().await;
    assert_eq!(
        initial
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>(),
        ["second", "third"]
    );
    let (second, third) = (initial[0].0.clone(), initial[1].0.clone());

    // Move the third run in front of the second.
    harness
        .commands
        .thread_command(
            "queued-run.reorder",
            &thread,
            json!({"runId": third, "beforeRunId": second}),
        )
        .await
        .expect("reorder");
    let reordered = queue().await;
    assert_eq!(
        reordered
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>(),
        ["third", "second"]
    );

    harness
        .commands
        .thread_command(
            "queued-run.edit",
            &thread,
            json!({"runId": second, "text": "second, edited"}),
        )
        .await
        .expect("edit");
    harness
        .commands
        .thread_command("queued-run.cancel", &thread, json!({"runId": third}))
        .await
        .expect("cancel");
    let remaining = queue().await;
    assert_eq!(remaining, vec![(second, "second, edited".to_owned())]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn staged_uploads_are_claimed_into_the_thread_with_the_message() {
    use turin_orchestration::pipeline::attachment_store::resolve_attachment_path;
    use turin_orchestration::pipeline::orchestrator_port::{
        OrchestratorPort as _, RecordField, RecordFilter,
    };

    let harness = harness().await;
    let thread = project_and_thread(&harness).await;
    let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3];
    let image = harness
        .commands
        .stage_attachment(true, "shot.png", "image/png", png.clone())
        .await
        .expect("stage image");
    let file = harness
        .commands
        .stage_attachment(false, "notes.txt", "text/plain", b"hello".to_vec())
        .await
        .expect("stage file");
    assert!(image["id"].as_str().expect("id").starts_with("pending-"));

    harness
        .commands
        .send_message(SendMessage {
            thread_id: thread.clone(),
            text: "Look at these".into(),
            attachments: vec![image, file],
            model_selection: None,
            delivery: Delivery::StartImmediately,
            title_seed: None,
        })
        .await
        .expect("send with attachments");

    let records = harness
        .commands
        .runtime()
        .orchestrator
        .get_thread_records(&thread, &[RecordField::Messages], &RecordFilter::default())
        .await
        .expect("records");
    let message = records
        .messages
        .iter()
        .find(|message| message.text == "Look at these")
        .expect("user message");
    assert_eq!(message.attachments.len(), 2);
    let dir = &harness.commands.runtime().attachments_dir;
    for attachment in &message.attachments {
        let id = serde_json::to_value(attachment).expect("json")["id"]
            .as_str()
            .expect("id")
            .to_owned();
        assert!(!id.starts_with("pending-"), "claimed into the thread: {id}");
        let path = resolve_attachment_path(dir, attachment).expect("path");
        assert!(path.exists(), "{} exists", path.display());
    }
    let image_path = resolve_attachment_path(dir, &message.attachments[0]).expect("path");
    assert_eq!(std::fs::read(image_path).expect("read"), png);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_message_gets_a_generated_title() {
    use turin_orchestration::pipeline::orchestrator_port::{
        OrchestratorPort as _, RecordField, RecordFilter,
    };

    let harness = harness().await;
    let thread = project_and_thread(&harness).await;
    harness
        .commands
        .send_message(SendMessage {
            thread_id: thread.clone(),
            text: "Respond with exactly: first fixture turn complete".into(),
            attachments: vec![],
            model_selection: None,
            delivery: Delivery::StartImmediately,
            title_seed: Some("Respond with exactly".into()),
        })
        .await
        .expect("send");
    let orchestrator = harness.commands.runtime().orchestrator.clone();
    let title = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let records = orchestrator
                .get_thread_records(&thread, &[RecordField::Messages], &RecordFilter::default())
                .await
                .expect("records");
            if records.thread.title.as_str() == "Fixture title" {
                return records.thread.title.to_string();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the generated title arrives");
    assert_eq!(title, "Fixture title");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn at_completion_finds_workspace_files_and_folders() {
    let harness = harness().await;
    std::fs::create_dir_all(harness.workspace.join("src/components")).expect("dirs");
    std::fs::write(harness.workspace.join("src/components/Button.tsx"), "").expect("file");
    std::fs::write(harness.workspace.join("README.md"), "").expect("file");
    let cwd = harness.workspace.to_str().expect("utf-8");

    let found = harness
        .commands
        .search_paths(cwd, "button", 20)
        .await
        .expect("search");
    assert_eq!(
        found.first(),
        Some(&("src/components/Button.tsx".to_owned(), false)),
        "{found:?}"
    );

    let folders = harness
        .commands
        .search_paths(cwd, "components", 20)
        .await
        .expect("search");
    assert!(
        folders
            .iter()
            .any(|(path, directory)| path == "src/components" && *directory),
        "{folders:?}"
    );
}

fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A repository with one commit on `main`.
fn git_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "one\n").expect("write");
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

// The live git control: commit, pull, init, create/switch ref and the thread
// branch update all run through the runtime instead of reporting "not wired".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn git_actions_run_against_a_real_repository() {
    use turin_contracts::git::{
        GitRunStackedActionInput, GitStackedAction, VcsCreateRefInput, VcsListRefsInput,
        VcsSwitchRefInput,
    };
    use turin_contracts::{PositiveInt, TrimmedNonEmptyString};

    let harness = harness().await;
    let commands = &harness.commands;
    let repo = harness.workspace.clone();
    git_repo(&repo);
    let cwd = repo.to_str().expect("utf-8");
    let text = |value: &str| TrimmedNonEmptyString::new(value).expect("text");

    // Status is a real read: a dirty tree is visible.
    std::fs::write(repo.join("a.txt"), "one\ntwo\n").expect("edit");
    let status = commands.git_status(cwd).await.expect("status");
    assert!(status.is_repo && status.has_working_tree_changes);
    assert_eq!(status.ref_name.as_ref().map(|n| n.as_str()), Some("main"));

    // Commit through the stacked action, hearing its progress.
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = events.clone();
    let result = commands
        .git_run_stacked_action(
            GitRunStackedActionInput {
                action_id: text("run-1"),
                cwd: text(cwd),
                action: GitStackedAction::Commit,
                commit_message: Some(text("feat: two")),
                feature_branch: None,
                file_paths: None,
                thread_id: None,
                project_id: None,
            },
            Some(Arc::new(move |event| sink.lock().unwrap().push(event))),
        )
        .await
        .expect("commit");
    assert_eq!(
        result.commit.subject.as_ref().map(|s| s.as_str()),
        Some("feat: two")
    );
    assert!(
        !events.lock().unwrap().is_empty(),
        "progress events are reported"
    );
    let status = commands.git_status(cwd).await.expect("status");
    assert!(
        !status.has_working_tree_changes,
        "the commit cleaned the tree"
    );

    // Pull without an upstream reports git's reason, not a stub.
    let pull = commands.git_pull(cwd).await.expect_err("no upstream");
    assert!(!pull.message.contains("not wired"), "{pull}");

    // Create + switch refs, and the list shows them.
    commands
        .git_create_ref(VcsCreateRefInput {
            cwd: text(cwd),
            ref_name: text("feature/x"),
            switch_ref: Some(true),
        })
        .await
        .expect("create ref");
    let status = commands.git_status(cwd).await.expect("status");
    assert_eq!(
        status.ref_name.as_ref().map(|n| n.as_str()),
        Some("feature/x")
    );
    let switched = commands
        .git_switch_branch(VcsSwitchRefInput {
            cwd: text(cwd),
            ref_name: text("main"),
        })
        .await
        .expect("switch");
    assert_eq!(switched.ref_name.as_ref().map(|n| n.as_str()), Some("main"));
    let listed = commands
        .git_list_refs(&VcsListRefsInput {
            cwd: text(cwd),
            query: None,
            cursor: None,
            include_matching_remote_refs: Some(true),
            ref_kind: None,
            refresh: Some(true),
            limit: PositiveInt::new(100),
        })
        .await
        .expect("list refs");
    let names: Vec<_> = listed.refs.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&"main") && names.contains(&"feature/x"),
        "{names:?}"
    );

    // Init in a folder that is not a repository.
    let plain = tempfile::tempdir().expect("plain dir");
    let plain_cwd = plain.path().to_str().expect("utf-8");
    assert!(
        !commands
            .git_status(plain_cwd)
            .await
            .expect("status")
            .is_repo
    );
    commands.git_init(plain_cwd).await.expect("init");
    assert!(
        commands
            .git_status(plain_cwd)
            .await
            .expect("status")
            .is_repo
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_thread_branch_update_reaches_the_read_model() {
    let harness = harness().await;
    let thread = project_and_thread(&harness).await;
    harness
        .commands
        .update_thread_branch(&thread, Some("feature/x".into()), Some(None), false)
        .await
        .expect("update branch");
    let shell = harness
        .stores
        .projections
        .get_thread_shell(&thread)
        .await
        .expect("read shell")
        .expect("shell");
    assert_eq!(shell.branch.as_ref().map(|b| b.as_str()), Some("feature/x"));
}
