//! Per-provider smoke tests for the write path: composer command → orchestrator
//! → provider driver → a fake provider process or server → the thread store.
//!
//! `e2e_tests.rs` proves this for Claude. Here every other provider gets the
//! same three checks against its own fake (never a real provider binary, never
//! the network, never a real data dir):
//!
//! 1. project + thread, send, the streamed reply lands in the client store;
//! 2. Stop interrupts a running turn and the thread settles;
//! 3. (where the fake supports it) one approval round trip.
//!
//! The fakes live in `crates/app/tests/fixtures`. This module only holds the
//! shared harness and flows; each provider has a file with its fake wiring.

#![cfg(unix)]

mod cursor;
mod opencode;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::Duration;

use futures::StreamExt as _;
use serde_json::{Value, json};
use turin_contracts::ThreadId;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ConversationMessageRole, OrchestrationV2DomainEvent,
    OrchestrationV2RuntimeRequestStatus, OrchestrationV2RunStatus,
};
use turin_contracts::settings::ServerSettings;
use turin_orchestration::pipeline::event_sink::StreamOptions;
use turin_orchestration::pipeline::orchestrator_port::{
    OrchestratorPort as _, RecordField, RecordFilter,
};

use super::commands::{Commands, CreateThread, Delivery, SendMessage};
use super::runtime::WriteRuntime;
use crate::app_runtime::Stores;
use crate::app_runtime::tests::{start, thread_id, wait_for};
use crate::state::store::ClientStateHandle;

pub(super) const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// Provider drivers read the user's home (`~/.cursor`, `~/.claude`, ...). Point
/// it at a throwaway dir for the whole test process, once, before any session.
fn isolate_home() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let home = tempfile::tempdir().expect("home dir").keep();
        // SAFETY: runs once, before a provider session of these tests exists;
        // std serializes env access against `Command` spawns.
        unsafe { std::env::set_var("HOME", &home) };
    });
}

/// Copies a fixture script into `dir` under `name`, executable.
pub(super) fn install_script(dir: &Path, fixture: &str, name: &str) -> PathBuf {
    std::fs::create_dir_all(dir).expect("cli dir");
    let script = dir.join(name);
    std::fs::copy(format!("{FIXTURES}/{fixture}"), &script).expect("script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    script
}

pub(super) struct Smoke {
    pub commands: Commands,
    pub stores: Arc<Stores>,
    pub workspace: PathBuf,
    /// The model selection a thread of this provider is created with.
    pub model_selection: Value,
    client: ClientStateHandle,
    _dirs: Vec<tempfile::TempDir>,
}

/// A write runtime over temp dirs. `settings` carries the provider instance
/// pointing at the fake; `dirs` keeps the fake's own temp dirs alive.
pub(super) async fn smoke(
    settings: Value,
    model_selection: Value,
    mut dirs: Vec<tempfile::TempDir>,
) -> Smoke {
    isolate_home();
    let data = tempfile::tempdir().expect("data dir");
    let workspace = tempfile::tempdir().expect("workspace");
    let settings: ServerSettings = serde_json::from_value(settings).expect("settings");
    let stores = Arc::new(Stores::open(data.path()).await.expect("stores"));
    let runtime = WriteRuntime::build(stores.clone(), data.path(), settings)
        .await
        .expect("write runtime");
    let workspace_path = workspace.path().to_path_buf();
    dirs.extend([data, workspace]);
    Smoke {
        commands: Commands::new(runtime),
        client: start(stores.clone()),
        stores,
        workspace: workspace_path,
        model_selection,
        _dirs: dirs,
    }
}

impl Smoke {
    /// Project + thread, with the thread open in the client store.
    pub async fn thread(&self, title: &str, runtime_mode: &str) -> ThreadId {
        let project_id = self
            .commands
            .create_project("Fixture", self.workspace.to_str().expect("utf-8"))
            .await
            .expect("create project");
        let thread = self
            .commands
            .create_thread(CreateThread {
                project_id,
                title: title.into(),
                model_selection: self.model_selection.clone(),
                runtime_mode: runtime_mode.into(),
                interaction_mode: "default".into(),
            })
            .await
            .expect("create thread");
        wait_for(&self.client, "the shell", |state| {
            state
                .shell_view
                .as_ref()
                .is_some_and(|view| view.threads.len() == 1)
        })
        .await;
        self.client.open_thread(thread_id(thread.as_str()));
        wait_for(&self.client, "the empty thread", |state| {
            state
                .thread(&thread_id(thread.as_str()))
                .is_some_and(|entry| entry.state.data.is_some())
        })
        .await;
        thread
    }

    /// Sends `text` and drives the turn to its terminal run status. A Stop is
    /// issued as soon as the run is `running` when `stop_when_running`; a
    /// pending runtime request is answered with `approve` (accept/decline)
    /// when given. Returns the terminal status.
    pub async fn run_turn(
        &self,
        thread: &ThreadId,
        text: &str,
        stop_when_running: bool,
        approve: Option<&str>,
    ) -> TurnOutcome {
        let after = self
            .stores
            .sink
            .latest_sequence(Some(thread.clone()))
            .await
            .expect("sequence");
        let mut events = self
            .stores
            .sink
            .stream(StreamOptions {
                thread_id: Some(thread.clone()),
                after_sequence: Some(after),
                event_type: None,
            })
            .await
            .expect("stream");
        self.commands
            .send_message(SendMessage {
                thread_id: thread.clone(),
                text: text.into(),
                attachments: vec![],
                model_selection: None,
                delivery: Delivery::StartImmediately,
                title_seed: None,
            })
            .await
            .expect("send");

        let orchestrator = self.commands.runtime().orchestrator.clone();
        let (mut stopped, mut answered) = (false, false);
        let status = tokio::time::timeout(Duration::from_secs(60), async {
            while let Some(item) = events.next().await {
                match item.expect("event").event {
                    OrchestrationV2DomainEvent::RunUpdated { payload, .. } => match payload.status
                    {
                        OrchestrationV2RunStatus::Running if stop_when_running && !stopped => {
                            stopped = true;
                            self.commands.stop_thread(thread).await.expect("stop");
                        }
                        OrchestrationV2RunStatus::Completed
                        | OrchestrationV2RunStatus::Failed
                        | OrchestrationV2RunStatus::Interrupted
                        | OrchestrationV2RunStatus::Cancelled => return payload.status,
                        _ => {}
                    },
                    OrchestrationV2DomainEvent::RuntimeRequestUpdated { .. } if !answered => {
                        let Some(decision) = approve else { continue };
                        let records = orchestrator
                            .get_thread_records(
                                thread,
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
                        self.commands
                            .respond_to_request(thread, request.id.as_str(), Some(decision), None)
                            .await
                            .expect("respond");
                    }
                    _ => {}
                }
            }
            panic!("event stream ended before the run settled");
        })
        .await
        .expect("run settled in time");
        TurnOutcome {
            status,
            stopped,
            answered,
        }
    }

    /// Waits until the client store (what the views render) holds an assistant
    /// message containing `needle`, and returns the user message text beside it.
    pub async fn assistant_reply_in_store(&self, thread: &ThreadId, needle: &str) -> String {
        let state = wait_for(&self.client, "the assistant reply", |state| {
            state
                .thread(&thread_id(thread.as_str()))
                .and_then(|entry| entry.state.data.as_ref())
                .is_some_and(|data| {
                    data.messages.iter().any(|message| {
                        message.role == OrchestrationV2ConversationMessageRole::Assistant
                            && message.text.contains(needle)
                    })
                })
        })
        .await;
        let data = state
            .thread(&thread_id(thread.as_str()))
            .and_then(|entry| entry.state.data.clone())
            .expect("thread data");
        data.messages
            .iter()
            .find(|message| message.role == OrchestrationV2ConversationMessageRole::User)
            .expect("user message")
            .text
            .to_string()
    }

    /// The thread has settled in the store: its latest run is `status`, and the
    /// sidebar row no longer shows it running.
    pub async fn settled_in_store(&self, thread: &ThreadId, status: OrchestrationV2RunStatus) {
        let id = thread_id(thread.as_str());
        wait_for(&self.client, "the settled thread", |state| {
            let run_settled = state
                .thread(&id)
                .and_then(|entry| entry.state.data.as_ref())
                .and_then(|data| data.runs.last())
                .is_some_and(|run| run.status == status);
            let row_idle = state.shell_view.as_ref().is_some_and(|view| {
                use crate::state::models::RunStatus as S;
                let live = |status: S| {
                    matches!(
                        status,
                        S::Preparing | S::Queued | S::Starting | S::Running | S::Waiting
                    )
                };
                view.threads.iter().all(|row| {
                    !row.latest_run.as_ref().is_some_and(|run| live(run.status))
                        && !row.runtime.as_ref().is_some_and(|runtime| live(runtime.status))
                })
            });
            run_settled && row_idle
        })
        .await;
    }
}

pub(super) struct TurnOutcome {
    pub status: OrchestrationV2RunStatus,
    pub stopped: bool,
    pub answered: bool,
}

/// The shared reply check: send, completed, the streamed reply is in the store.
pub(super) async fn assert_reply_lands(smoke: &Smoke, reply: &str) {
    let thread = smoke.thread("Say hello", "auto").await;
    let prompt = format!("Respond with exactly: {reply}");
    let outcome = smoke.run_turn(&thread, &prompt, false, None).await;
    assert_eq!(outcome.status, OrchestrationV2RunStatus::Completed);
    let user = smoke.assistant_reply_in_store(&thread, reply).await;
    assert_eq!(user, prompt);
    smoke
        .settled_in_store(&thread, OrchestrationV2RunStatus::Completed)
        .await;
}

/// The shared Stop check: the turn never ends by itself; Stop interrupts it.
pub(super) async fn assert_stop_interrupts(smoke: &Smoke, hang_prompt: &str) {
    let thread = smoke.thread("Stop me", "auto").await;
    let outcome = smoke.run_turn(&thread, hang_prompt, true, None).await;
    assert!(outcome.stopped, "the run reached running before it settled");
    assert_eq!(outcome.status, OrchestrationV2RunStatus::Interrupted);
    smoke
        .settled_in_store(&thread, OrchestrationV2RunStatus::Interrupted)
        .await;
}

/// The shared approval check: the provider asks, the command answers, the turn finishes.
pub(super) async fn assert_approval_round_trip(smoke: &Smoke, prompt: &str, reply: &str) {
    let thread = smoke.thread("Approve me", "approval-required").await;
    let outcome = smoke.run_turn(&thread, prompt, false, Some("accept")).await;
    assert!(outcome.answered, "an approval was raised");
    assert_eq!(outcome.status, OrchestrationV2RunStatus::Completed);
    smoke.assistant_reply_in_store(&thread, reply).await;
}

#[allow(dead_code)]
pub(super) fn model(instance: &str, model: &str) -> Value {
    json!({"instanceId": instance, "model": model})
}
