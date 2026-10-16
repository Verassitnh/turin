//! Upstream `ProviderTurnStartService.test.ts`. The projection is the real
//! store (in memory), seeded with domain events and advanced by the events the
//! service writes, as upstream's harness advances its projection with
//! `applyToProjection`. The session, run execution, runtime policy, prompt
//! commands and workspace are doubles.
//!
//! Not ported: "does not terminalize a provider-session open interruption"
//! (an `Effect.interrupt` of the open; dropping the future is the only
//! interruption in Rust), and `ProviderTurnStartService.memory.test.ts` (a
//! Node `--expose-gc` heap measurement; the plan is dropped when the
//! delivery session starts its turn, see `handoff_session.rs`).

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{Value, json};
use turin_contracts::{
    ProjectId, RunId, ThreadId,
    model_selection::ModelSelection,
    orchestration_v2::{OrchestrationV2ProviderThread, OrchestrationV2ThreadProjection},
    provider_instance::ProviderInstanceId,
};
use turin_persistence::{Db, OrchestrationEventStore};
use turin_provider_core::provider_adapter::{
    ProviderAdapterV2Error, ProviderAdapterV2OpenSessionInput, ProviderAdapterV2RuntimePolicy,
    ProviderAdapterV2TurnInput, ProviderSessionRuntime,
};

use super::*;
use crate::{
    core::run_execution_service::test_support::{ScriptedSession, turn_start_error},
    pipeline::{
        effect_outbox::EffectOutbox,
        event_sink::{EventSink, SinkError},
    },
    projection::test_support::{event, message},
};

const T0: &str = "2026-09-04T12:00:00.000Z";
const THREAD: &str = "thread-native-account-command";
const RUN: &str = "run-native-account-command";
const ROOT_NODE: &str = "root-native-account-command";
const ATTEMPT: &str = "attempt-native-account-command";
const PROVIDER_THREAD: &str = "new-provider-thread";
const PROVIDER_SESSION: &str = "new-provider-session";
const OLD_PROVIDER_THREAD: &str = "existing-native-provider-thread";
const OLD_INSTANCE: &str = "antigravity-personal";
const NEW_INSTANCE: &str = "codex-personal";
const SCOPE: &str = "scope-native-account-command";
const MESSAGE: &str = "message-native-account-command";

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct FakeFailure(String);

fn fail(message: &str) -> BoxError {
    Box::new(FakeFailure(message.to_owned()))
}

// ---------------------------------------------------------------------------
// Doubles
// ---------------------------------------------------------------------------

/// `EventSink` behind the writer seam, with a switch that fails the write and
/// a log of the guarded writes.
struct TestWriter {
    sink: Arc<EventSink>,
    fail_guarded: AtomicBool,
    guarded: Mutex<
        Vec<(
            RunId,
            turin_contracts::RunAttemptId,
            OrchestrationV2RunStatus,
        )>,
    >,
    committed: Mutex<Vec<OrchestrationV2DomainEvent>>,
}

#[async_trait]
impl RunEventWriter for TestWriter {
    async fn write(&self, input: WriteInput) -> Result<(), BoxError> {
        self.committed.lock().extend(input.events.clone());
        RunEventWriter::write(self.sink.as_ref(), input).await
    }

    async fn write_if_run_current(&self, input: RunCurrentWrite) -> Result<bool, BoxError> {
        self.guarded.lock().push((
            input.run_id.clone(),
            input.active_attempt_id.clone(),
            input.expected_status,
        ));
        if self.fail_guarded.load(Ordering::SeqCst) {
            return Err(Box::new(SinkError::Write {
                event_count: input.write.events.len(),
                command_id: None,
                cause: turin_persistence::PersistenceError::Decode(
                    turin_persistence::PersistenceDecodeError::new("test", "database unavailable"),
                ),
            }));
        }
        let events = input.write.events.clone();
        let committed = RunEventWriter::write_if_run_current(self.sink.as_ref(), input).await?;
        if committed {
            self.committed.lock().extend(events);
        }
        Ok(committed)
    }
}

/// How the opener answers.
enum Open {
    Session(Arc<ScriptedSession>),
    Fail(&'static str),
    /// The test must not open a native session.
    Unexpected,
}

struct FakeOpener {
    open: Mutex<Open>,
    calls: Mutex<Vec<ProviderAdapterV2OpenSessionInput>>,
    /// Runs before the answer: lets a test move the run while the open is in flight.
    before_answer: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

#[async_trait]
impl ProviderSessionOpener for FakeOpener {
    async fn open(
        &self,
        input: ProviderAdapterV2OpenSessionInput,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, BoxError> {
        self.calls.lock().push(input);
        if let Some(hook) = self.before_answer.lock().take() {
            hook();
        }
        match &*self.open.lock() {
            Open::Session(session) => Ok(session.clone()),
            Open::Fail(message) => Err(Box::new(
                crate::core::provider_session_manager::ProviderSessionManagerError::Open {
                    instance_id: ProviderInstanceId::new(NEW_INSTANCE).unwrap(),
                    provider_session_id: turin_contracts::ProviderSessionId::new(PROVIDER_SESSION)
                        .unwrap(),
                    cause: fail(message),
                },
            )),
            Open::Unexpected => panic!("A local command must not open a native session."),
        }
    }
}

struct RecordingStarter {
    inputs: Mutex<Vec<StartRootRunInput>>,
    unexpected: bool,
}

#[async_trait]
impl RootRunStarter for RecordingStarter {
    async fn start_root_run(&self, input: StartRootRunInput) -> Result<(), BoxError> {
        assert!(
            !self.unexpected,
            "A local command must not start a native turn."
        );
        self.inputs.lock().push(input);
        Ok(())
    }
}

struct FixedPolicy;

#[async_trait]
impl RuntimePolicyResolver for FixedPolicy {
    async fn resolve(
        &self,
        _thread: &OrchestrationV2AppThread,
        _model_selection: &ModelSelection,
    ) -> Result<ProviderAdapterV2RuntimePolicy, BoxError> {
        Ok(serde_json::from_value(json!({
            "runtimeMode": "full-access", "interactionMode": "default",
            "cwd": "/tmp/native-account-command",
        }))
        .expect("policy"))
    }
}

struct RecordingPromptCommands {
    calls: Mutex<Vec<(ProviderInstanceId, String, bool)>>,
    logout_failure: Option<String>,
    handles: bool,
}

#[async_trait]
impl PromptCommands for RecordingPromptCommands {
    async fn try_handle_prompt_command(
        &self,
        instance_id: &ProviderInstanceId,
        text: &str,
        has_attachments: bool,
    ) -> Result<bool, PromptCommandFailure> {
        self.calls
            .lock()
            .push((instance_id.clone(), text.to_owned(), has_attachments));
        match &self.logout_failure {
            Some(detail) => Err(PromptCommandFailure {
                detail: detail.clone(),
            }),
            None => Ok(self.handles),
        }
    }
}

#[derive(Default)]
struct FakeWorkspace {
    exists: bool,
    root: Option<String>,
    recreated: Mutex<Vec<(String, String, String)>>,
}

#[async_trait]
impl TurnStartWorkspace for FakeWorkspace {
    async fn path_exists(&self, _path: &str) -> bool {
        self.exists
    }
    async fn project_workspace_root(&self, _project_id: &ProjectId) -> Option<String> {
        self.root.clone()
    }
    async fn recreate_worktree(
        &self,
        cwd: &str,
        ref_name: &str,
        path: &str,
    ) -> Result<(), BoxError> {
        self.recreated
            .lock()
            .push((cwd.to_owned(), ref_name.to_owned(), path.to_owned()));
        Ok(())
    }
}

/// The real store with the failures upstream's harness injects.
struct FakeReads {
    store: Arc<ProjectionStore>,
    recovery_reads: AtomicUsize,
    context_reads: AtomicUsize,
    /// Every runtime-recovery read fails.
    fail_recovery: bool,
    /// Runtime-recovery reads fail once the run is `running`.
    fail_recovery_when_running: bool,
    /// `getTurnStartHistory` fails with this text.
    fail_history: Option<&'static str>,
    run_id: RunId,
}

#[async_trait]
impl TurnStartReads for FakeReads {
    async fn turn_start_context(
        &self,
        thread_id: &ThreadId,
        run_id: &RunId,
    ) -> Result<TurnStartContext, BoxError> {
        self.context_reads.fetch_add(1, Ordering::SeqCst);
        self.store
            .as_ref()
            .turn_start_context(thread_id, run_id)
            .await
    }
    async fn runtime_recovery_projection(
        &self,
        thread_id: &ThreadId,
    ) -> Result<RuntimeRecoveryState, BoxError> {
        self.recovery_reads.fetch_add(1, Ordering::SeqCst);
        if self.fail_recovery {
            return Err(fail("simulated inherited-background projection failure"));
        }
        let state = self
            .store
            .as_ref()
            .runtime_recovery_projection(thread_id)
            .await?;
        if self.fail_recovery_when_running
            && state
                .runs
                .iter()
                .any(|run| run.id == self.run_id && run.status == OrchestrationV2RunStatus::Running)
        {
            return Err(fail("database unavailable"));
        }
        Ok(state)
    }
    async fn turn_start_history(
        &self,
        thread_id: &ThreadId,
        run_ids: Option<&[RunId]>,
    ) -> Result<Vec<OrchestrationV2TurnItem>, BoxError> {
        if let Some(message) = self.fail_history {
            return Err(fail(message));
        }
        self.store
            .as_ref()
            .turn_start_history(thread_id, run_ids)
            .await
    }
    async fn thread_records(
        &self,
        thread_id: &ThreadId,
        fields: Vec<ProjectionRecordField>,
    ) -> Result<OrchestrationV2ThreadProjection, BoxError> {
        self.store.as_ref().thread_records(thread_id, fields).await
    }
    async fn has_unpaired_run_interrupt_request(
        &self,
        thread_id: &ThreadId,
        request_id: &str,
        result_id: &str,
    ) -> Result<bool, BoxError> {
        TurnStartReads::has_unpaired_run_interrupt_request(
            self.store.as_ref(),
            thread_id,
            request_id,
            result_id,
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// The harness (`makeLocalCommandHarness`)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Options {
    text: &'static str,
    previous_native_session: bool,
    previous_messages: Vec<&'static str>,
    logout_failure: Option<&'static str>,
    open_failure: Option<&'static str>,
    /// Opens the session, then fails loading its provider thread.
    ensure_thread_failure: Option<&'static str>,
    /// Resumes a thread that has a native ref: resume fails, the fresh-thread
    /// fallback succeeds, then reading history for its handoff fails.
    history_read_failure_after_fallback: Option<&'static str>,
    interrupt_run_before_failure: bool,
    write_failure: bool,
    /// Loads the thread and starts the run, then fails every later state read.
    fail_reads_after_running: bool,
    fail_recovery: bool,
    worktree: Option<(&'static str, &'static str)>,
    /// Lets the session open and load, and seeds extra rows.
    healthy_session: bool,
    extra_events: Vec<OrchestrationV2DomainEvent>,
    previous_native_thread_ref: bool,
    inject_hook: bool,
}

struct Harness {
    store: Arc<ProjectionStore>,
    writer: Arc<TestWriter>,
    opener: Arc<FakeOpener>,
    starter: Arc<RecordingStarter>,
    prompts: Arc<RecordingPromptCommands>,
    workspace: Arc<FakeWorkspace>,
    reads: Arc<FakeReads>,
    session: Arc<ScriptedSession>,
    service: ProviderTurnStartService,
}

fn seed_events(options: &Options, now: &str) -> Vec<OrchestrationV2DomainEvent> {
    let mut thread = crate::projection::test_support::thread(THREAD);
    thread["activeProviderThreadId"] = json!(PROVIDER_THREAD);
    thread["createdAt"] = json!(now);
    thread["updatedAt"] = json!(now);
    if let Some((branch, path)) = options.worktree {
        thread["branch"] = json!(branch);
        thread["worktreePath"] = json!(path);
    }
    let run = |id: &str, ordinal: u64, status: &str, instance: &str, provider_thread: &str| {
        json!({
            "id": id, "threadId": THREAD, "ordinal": ordinal, "providerInstanceId": instance,
            "modelSelection": {"instanceId": instance, "model": "gpt-5.4"},
            "providerThreadId": provider_thread, "userMessageId": format!("message:{id}"),
            "rootNodeId": ROOT_NODE, "activeAttemptId": null, "status": status,
            "requestedAt": now, "startedAt": null, "completedAt": null,
            "checkpointId": null, "contextHandoffId": null,
        })
    };
    let mut current_run = run(RUN, 2, "starting", NEW_INSTANCE, PROVIDER_THREAD);
    current_run["userMessageId"] = json!(MESSAGE);
    current_run["activeAttemptId"] = json!(ATTEMPT);
    let provider_thread = |id: &str,
                           instance: &str,
                           driver: &str,
                           native: Option<&str>,
                           ordinal: u64| {
        json!({
            "id": id, "driver": driver, "providerInstanceId": instance,
            "providerSessionId": PROVIDER_SESSION, "appThreadId": THREAD, "ownerNodeId": null,
            "nativeThreadRef": native.map(|native| json!({"driver": driver, "strength": "strong", "nativeId": native})),
            "nativeConversationHeadRef": null, "status": "not_loaded",
            "firstRunOrdinal": ordinal, "lastRunOrdinal": ordinal, "handoffIds": [],
            "forkedFrom": null, "createdAt": now, "updatedAt": now,
        })
    };
    let mut events = vec![event("thread.created", THREAD, now, thread)];
    if options.previous_native_session {
        events.push(event(
            "provider-thread.updated",
            THREAD,
            now,
            provider_thread(
                OLD_PROVIDER_THREAD,
                OLD_INSTANCE,
                "antigravity",
                Some("existing-session"),
                1,
            ),
        ));
        let mut previous = run(
            "previous-native-run",
            1,
            "completed",
            OLD_INSTANCE,
            OLD_PROVIDER_THREAD,
        );
        previous["rootNodeId"] = json!("previous-root");
        events.push(event("run.created", THREAD, now, previous));
    }
    let native = (options.history_read_failure_after_fallback.is_some()
        || options.previous_native_thread_ref)
        .then_some("native-resume-thread");
    events.push(event(
        "provider-thread.updated",
        THREAD,
        now,
        provider_thread(PROVIDER_THREAD, NEW_INSTANCE, "codex", native, 2),
    ));
    for (index, text) in options.previous_messages.iter().enumerate() {
        events.push(event(
            "message.updated",
            THREAD,
            now,
            message(
                THREAD,
                &format!("previous-message-{index}"),
                Some("previous-native-run"),
                "user",
                text,
                now,
            ),
        ));
    }
    events.push(event("run.created", THREAD, now, current_run));
    events.push(event(
        "run-attempt.created",
        THREAD,
        now,
        json!({
            "id": ATTEMPT, "runId": RUN, "attemptOrdinal": 1, "rootNodeId": ROOT_NODE,
            "providerInstanceId": NEW_INSTANCE, "providerThreadId": PROVIDER_THREAD,
            "providerTurnId": null, "reason": "initial", "status": "pending",
            "startedAt": null, "completedAt": null,
        }),
    ));
    events.push(event(
        "node.updated",
        THREAD,
        now,
        json!({
            "id": ROOT_NODE, "threadId": THREAD, "runId": RUN, "parentNodeId": null,
            "rootNodeId": ROOT_NODE, "kind": "root_turn", "status": "pending", "countsForRun": true,
            "providerThreadId": PROVIDER_THREAD, "providerTurnId": null, "nativeItemRef": null,
            "runtimeRequestId": null, "checkpointScopeId": SCOPE, "startedAt": null,
            "completedAt": null,
        }),
    ));
    events.push(event(
        "checkpoint-scope.created",
        THREAD,
        now,
        json!({
            "id": SCOPE, "threadId": THREAD, "runId": RUN, "nodeId": ROOT_NODE,
            "parentScopeId": null, "providerThreadId": PROVIDER_THREAD, "kind": "root_run",
            "ordinalWithinParent": 0, "advancesAppRunCount": true,
            "cwd": "/tmp/native-account-command", "createdAt": now,
        }),
    ));
    let mut user_message = message(THREAD, MESSAGE, Some(RUN), "user", options.text, now);
    user_message["nodeId"] = json!(ROOT_NODE);
    events.push(event("message.updated", THREAD, now, user_message));
    events
}

async fn harness(options: Options) -> Harness {
    let db = Db::open_in_memory().await.expect("in-memory db");
    let store = Arc::new(ProjectionStore::new(db.clone()));
    let sink = Arc::new(EventSink::new(
        OrchestrationEventStore::new(db.clone()),
        EffectOutbox::new(db),
        store.clone(),
    ));
    for seed in seed_events(&options, T0)
        .into_iter()
        .chain(options.extra_events.clone())
    {
        store.apply(seed).await.expect("seed");
    }
    let session = ScriptedSession::quiet();
    let provider_thread_json = |status: &str| -> OrchestrationV2ProviderThread {
        serde_json::from_value(json!({
            "id": PROVIDER_THREAD, "driver": "codex", "providerInstanceId": NEW_INSTANCE,
            "providerSessionId": PROVIDER_SESSION, "appThreadId": THREAD, "ownerNodeId": null,
            "nativeThreadRef": {"driver": "codex", "strength": "strong", "nativeId": "native-loaded"},
            "nativeConversationHeadRef": null, "status": status, "firstRunOrdinal": 2,
            "lastRunOrdinal": 2, "handoffIds": [], "forkedFrom": null,
            "createdAt": T0, "updatedAt": T0,
        }))
        .expect("provider thread")
    };
    let open = if let Some(message) = options.open_failure {
        Open::Fail(message)
    } else if let Some(message) = options.ensure_thread_failure {
        session
            .ensure_results
            .lock()
            .push_back(Err(turn_start_error_with("Pi RPC read failed", message)));
        Open::Session(session.clone())
    } else if let Some(_failure) = options.history_read_failure_after_fallback {
        session
            .resume_results
            .lock()
            .push_back(Err(turn_start_error_with(
                "resume",
                "native thread is gone",
            )));
        session
            .ensure_results
            .lock()
            .push_back(Ok(provider_thread_json("not_loaded")));
        Open::Session(session.clone())
    } else if options.fail_reads_after_running || options.healthy_session {
        session
            .ensure_results
            .lock()
            .push_back(Ok(provider_thread_json("not_loaded")));
        session
            .resume_results
            .lock()
            .push_back(Ok(provider_thread_json("idle")));
        Open::Session(session.clone())
    } else {
        Open::Unexpected
    };
    if options.inject_hook {
        *session.inject_answers.lock() = Some(Default::default());
    }
    let opener = Arc::new(FakeOpener {
        open: Mutex::new(open),
        calls: Mutex::default(),
        before_answer: Mutex::default(),
    });
    if options.interrupt_run_before_failure {
        let store = store.clone();
        *opener.before_answer.lock() = Some(Box::new(move || {
            futures::executor::block_on(async {
                let mut interrupted = json!({
                    "id": RUN, "threadId": THREAD, "ordinal": 2, "providerInstanceId": NEW_INSTANCE,
                    "modelSelection": {"instanceId": NEW_INSTANCE, "model": "gpt-5.4"},
                    "providerThreadId": PROVIDER_THREAD, "userMessageId": MESSAGE,
                    "rootNodeId": ROOT_NODE, "activeAttemptId": ATTEMPT, "status": "interrupted",
                    "requestedAt": T0, "startedAt": null, "completedAt": T0,
                    "checkpointId": null, "contextHandoffId": null,
                });
                interrupted["updatedAt"] = json!(T0);
                store
                    .apply(event(
                        "run.updated",
                        THREAD,
                        "2026-09-04T12:00:01.000Z",
                        interrupted,
                    ))
                    .await
                    .expect("interrupt");
            });
        }));
    }
    let writer = Arc::new(TestWriter {
        sink,
        fail_guarded: AtomicBool::new(options.write_failure),
        guarded: Mutex::default(),
        committed: Mutex::default(),
    });
    let reads = Arc::new(FakeReads {
        store: store.clone(),
        recovery_reads: AtomicUsize::new(0),
        context_reads: AtomicUsize::new(0),
        fail_recovery: options.fail_recovery,
        fail_recovery_when_running: options.fail_reads_after_running,
        fail_history: options.history_read_failure_after_fallback,
        run_id: RunId::new(RUN).unwrap(),
    });
    let starter = Arc::new(RecordingStarter {
        inputs: Mutex::default(),
        unexpected: !(options.fail_reads_after_running || options.healthy_session),
    });
    let prompts = Arc::new(RecordingPromptCommands {
        calls: Mutex::default(),
        logout_failure: options.logout_failure.map(str::to_owned),
        handles: true,
    });
    let workspace = Arc::new(FakeWorkspace {
        exists: false,
        root: Some("/tmp/provider-turn-start-project".to_owned()),
        recreated: Mutex::default(),
    });
    let service = ProviderTurnStartService::new(
        writer.clone(),
        ContextHandoffService::default(),
        reads.clone(),
        opener.clone(),
        starter.clone(),
        Arc::new(FixedPolicy),
        prompts.clone(),
        workspace.clone(),
    );
    Harness {
        store,
        writer,
        opener,
        starter,
        prompts,
        workspace,
        reads,
        session,
        service,
    }
}

/// A provider-side failure whose nested cause is `cause` (text).
fn turn_start_error_with(_label: &str, cause: &str) -> ProviderAdapterV2Error {
    turn_start_error("settings").with_cause(
        turin_provider_core::failure::ProviderFailureCause::tagged("Error").with_message(cause),
    )
}

impl Harness {
    fn input(&self, will_retry: bool) -> TurnStartInput {
        TurnStartInput {
            thread_id: ThreadId::new(THREAD).unwrap(),
            run_id: RunId::new(RUN).unwrap(),
            will_retry,
        }
    }

    async fn start(&self) -> Result<(), BoxError> {
        self.service.start(self.input(false)).await
    }

    async fn start_with_retry(&self) -> Result<(), BoxError> {
        self.service.start(self.input(true)).await
    }

    async fn projection(&self) -> OrchestrationV2ThreadProjection {
        self.store
            .get_thread_records(
                &ThreadId::new(THREAD).unwrap(),
                vec![
                    ProjectionRecordField::Runs,
                    ProjectionRecordField::Attempts,
                    ProjectionRecordField::Nodes,
                    ProjectionRecordField::TurnItems,
                    ProjectionRecordField::ProviderTurns,
                    ProjectionRecordField::Checkpoints,
                    ProjectionRecordField::ProviderThreads,
                ],
                None,
            )
            .await
            .expect("projection")
    }

    async fn run(&self) -> Value {
        let projection = self.projection().await;
        let run = projection
            .runs
            .iter()
            .find(|run| run.id.as_str() == RUN)
            .expect("run");
        serde_json::to_value(run).unwrap()
    }
}

fn as_start_error(error: &BoxError) -> &ProviderTurnStartError {
    error
        .downcast_ref::<ProviderTurnStartError>()
        .expect("a ProviderTurnStartError")
}

fn first_error_item(projection: &OrchestrationV2ThreadProjection) -> Value {
    serde_json::to_value(&projection.turn_items[0]).unwrap()
}

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn does_not_commit_running_state_when_inherited_background_routing_cannot_be_read() {
    let harness = harness(Options {
        text: "Continue",
        fail_recovery: true,
        worktree: Some((
            "feature/restore",
            "/tmp/missing-provider-turn-start-worktree",
        )),
        ..Options::default()
    })
    .await;

    let error = harness.start().await.expect_err("the read fails the start");

    assert!(as_start_error(&error).run_id.as_str() == RUN);
    // The context read, then the one failing recovery read.
    assert_eq!(harness.reads.context_reads.load(Ordering::SeqCst), 1);
    assert_eq!(harness.reads.recovery_reads.load(Ordering::SeqCst), 1);
    assert_eq!(
        *harness.workspace.recreated.lock(),
        vec![(
            "/tmp/provider-turn-start-project".to_owned(),
            "feature/restore".to_owned(),
            "/tmp/missing-provider-turn-start-worktree".to_owned(),
        )]
    );
    assert!(harness.writer.guarded.lock().is_empty());
    assert!(harness.starter.inputs.lock().is_empty());
}

#[tokio::test]
async fn terminalizes_a_starting_run_when_its_provider_session_cannot_open() {
    let harness = harness(Options {
        text: "Continue",
        open_failure: Some("DESCRIPTION is not valid ACP JSON"),
        ..Options::default()
    })
    .await;

    harness.start().await.expect("settled, not failed");

    assert_eq!(harness.opener.calls.lock().len(), 1);
    assert!(harness.starter.inputs.lock().is_empty());
    {
        let guarded = harness.writer.guarded.lock();
        assert_eq!(guarded.len(), 1);
        assert_eq!(guarded[0].1.as_str(), ATTEMPT);
        assert_eq!(guarded[0].2, OrchestrationV2RunStatus::Starting);
    }
    let projection = harness.projection().await;
    let run = harness.run().await;
    assert_eq!(run["status"], "failed");
    assert_eq!(run["startedAt"], Value::Null);
    let attempt = serde_json::to_value(&projection.attempts[0]).unwrap();
    assert_eq!(attempt["status"], "failed");
    assert_eq!(attempt["startedAt"], Value::Null);
    let node = serde_json::to_value(&projection.nodes[0]).unwrap();
    assert_eq!(node["status"], "failed");
    assert_eq!(node["startedAt"], Value::Null);
    assert_eq!(projection.turn_items.len(), 1);
    let item = first_error_item(&projection);
    assert_eq!(item["type"], "error");
    assert_eq!(item["status"], "failed");
    assert_eq!(item["failure"]["class"], "provider_error");
    assert_eq!(
        item["failure"]["message"],
        "DESCRIPTION is not valid ACP JSON"
    );
}

#[tokio::test]
async fn leaves_the_run_starting_when_a_session_open_failure_will_be_retried() {
    let harness = harness(Options {
        text: "Continue",
        open_failure: Some("provider session rejected"),
        ..Options::default()
    })
    .await;

    let error = harness
        .start_with_retry()
        .await
        .expect_err("returned to the worker");

    as_start_error(&error);
    assert!(harness.writer.guarded.lock().is_empty());
    assert_eq!(harness.run().await["status"], "starting");
}

#[tokio::test]
async fn keeps_a_session_open_failure_retryable_when_terminal_persistence_fails() {
    let harness = harness(Options {
        text: "Continue",
        open_failure: Some("provider session rejected"),
        write_failure: true,
        ..Options::default()
    })
    .await;

    let error = harness
        .start()
        .await
        .expect_err("the write error is returned");

    as_start_error(&error);
    assert_eq!(harness.writer.guarded.lock().len(), 1);
    assert!(harness.starter.inputs.lock().is_empty());
    assert_eq!(harness.run().await["status"], "starting");
    assert!(harness.writer.committed.lock().is_empty());
}

#[tokio::test]
async fn does_not_overwrite_a_run_interrupted_while_its_provider_session_opens() {
    let harness = harness(Options {
        text: "Continue",
        open_failure: Some("provider session rejected"),
        interrupt_run_before_failure: true,
        ..Options::default()
    })
    .await;

    harness
        .start()
        .await
        .expect("a superseded start is not an error");

    assert_eq!(harness.writer.guarded.lock().len(), 1);
    assert!(harness.starter.inputs.lock().is_empty());
    let projection = harness.projection().await;
    assert_eq!(harness.run().await["status"], "interrupted");
    assert_eq!(
        serde_json::to_value(&projection.attempts[0]).unwrap()["status"],
        "pending"
    );
    assert_eq!(
        serde_json::to_value(&projection.nodes[0]).unwrap()["status"],
        "pending"
    );
    assert!(projection.turn_items.is_empty());
    assert!(harness.writer.committed.lock().is_empty());
}

#[tokio::test]
async fn fails_a_starting_run_when_its_last_start_attempt_cannot_load_the_thread() {
    let harness = harness(Options {
        text: "Continue",
        ensure_thread_failure: Some("Pi RPC read failed: pi process exited with code 1."),
        ..Options::default()
    })
    .await;

    harness.start().await.expect("settled, not failed");

    assert!(harness.starter.inputs.lock().is_empty());
    let projection = harness.projection().await;
    let run = harness.run().await;
    assert_eq!(run["status"], "failed");
    assert_eq!(run["startedAt"], Value::Null);
    assert_eq!(
        serde_json::to_value(&projection.attempts[0]).unwrap()["status"],
        "failed"
    );
    assert_eq!(
        serde_json::to_value(&projection.nodes[0]).unwrap()["status"],
        "failed"
    );
    let item = first_error_item(&projection);
    assert_eq!(item["type"], "error");
    assert_eq!(item["title"], "Provider turn failed to start");
    assert_eq!(item["failure"]["class"], "provider_error");
    assert_eq!(
        item["failure"]["message"],
        "Pi RPC read failed: pi process exited with code 1."
    );
}

#[tokio::test]
async fn leaves_the_run_starting_when_a_thread_load_failure_will_be_retried() {
    let harness = harness(Options {
        text: "Continue",
        ensure_thread_failure: Some("pi process exited with code 1"),
        ..Options::default()
    })
    .await;

    let error = harness
        .start_with_retry()
        .await
        .expect_err("returned to the worker");

    as_start_error(&error);
    assert!(harness.writer.guarded.lock().is_empty());
    assert_eq!(harness.run().await["status"], "starting");
}

#[tokio::test]
async fn keeps_a_thread_load_failure_retryable_when_terminal_persistence_fails() {
    let harness = harness(Options {
        text: "Continue",
        ensure_thread_failure: Some("pi process exited with code 1"),
        write_failure: true,
        ..Options::default()
    })
    .await;

    let error = harness
        .start()
        .await
        .expect_err("the write error is returned");

    as_start_error(&error);
    assert_eq!(harness.run().await["status"], "starting");
    assert!(harness.writer.committed.lock().is_empty());
}

#[tokio::test]
async fn keeps_a_store_failure_after_the_provider_loaded_the_thread_typed_and_retryable() {
    let harness = harness(Options {
        text: "Continue",
        history_read_failure_after_fallback: Some("database unavailable"),
        ..Options::default()
    })
    .await;

    let error = harness.start().await.expect_err("the read fails the start");

    // The provider succeeded; the failing stage is the projection read, so
    // the run is not failed as a provider error on the last attempt.
    let start = as_start_error(&error);
    assert!(start.cause.is::<FakeFailure>(), "{:?}", start.cause);
    assert!(harness.writer.guarded.lock().is_empty());
    assert_eq!(harness.run().await["status"], "starting");
    assert!(harness.writer.committed.lock().is_empty());
}

#[tokio::test]
async fn does_not_mistake_a_failed_state_read_for_a_superseded_run() {
    let harness = harness(Options {
        text: "Continue",
        fail_reads_after_running: true,
        ..Options::default()
    })
    .await;

    harness.start().await.expect("start");

    assert_eq!(harness.run().await["status"], "running");
    let controls = harness
        .starter
        .inputs
        .lock()
        .pop()
        .expect("run execution started");
    // `false` would skip the provider turn or the terminal write and leave the
    // run active. A read failure must reach the caller instead.
    let start_check = (controls.should_start_provider_turn.expect("start check"))().await;
    let finalize_check = (controls.should_finalize_run.expect("finalize check"))().await;
    assert!(start_check.is_err());
    assert!(finalize_check.is_err());
}

#[tokio::test]
async fn does_not_overwrite_a_run_interrupted_while_its_thread_loads() {
    let harness = harness(Options {
        text: "Continue",
        ensure_thread_failure: Some("pi process exited with code 1"),
        interrupt_run_before_failure: true,
        ..Options::default()
    })
    .await;

    harness
        .start()
        .await
        .expect("a superseded start is not an error");

    assert_eq!(harness.run().await["status"], "interrupted");
    assert!(harness.projection().await.turn_items.is_empty());
    assert!(harness.writer.committed.lock().is_empty());
}

#[tokio::test]
async fn signs_out_the_existing_native_provider_before_opening_the_newly_selected_provider() {
    let harness = harness(Options {
        text: "/logout",
        previous_native_session: true,
        ..Options::default()
    })
    .await;

    harness.start().await.expect("start");
    harness.start().await.expect("a settled run is a no-op");

    assert_eq!(
        *harness.prompts.calls.lock(),
        vec![(
            ProviderInstanceId::new(OLD_INSTANCE).unwrap(),
            "/logout".to_owned(),
            false
        )]
    );
    assert!(harness.opener.calls.lock().is_empty());
    assert!(harness.starter.inputs.lock().is_empty());
    let projection = harness.projection().await;
    assert_eq!(harness.run().await["status"], "completed");
    assert_eq!(
        serde_json::to_value(&projection.attempts[0]).unwrap()["status"],
        "completed"
    );
    assert_eq!(
        serde_json::to_value(&projection.nodes[0]).unwrap()["status"],
        "completed"
    );
    assert_eq!(projection.turn_items.len(), 1);
    let item = first_error_item(&projection);
    assert_eq!(item["type"], "command_execution");
    assert_eq!(item["title"], "Provider signed out");
    assert_eq!(item["output"], "Provider signed out");
    assert_eq!(item["status"], "completed");
    assert!(projection.provider_turns.is_empty());
    assert!(projection.checkpoints.is_empty());
}

#[tokio::test]
async fn persists_a_failed_sign_out_without_starting_a_provider_turn() {
    let harness = harness(Options {
        text: "/logout",
        logout_failure: Some("Could not stop all sessions for this provider. Try again."),
        ..Options::default()
    })
    .await;

    harness.start().await.expect("start");

    assert!(harness.opener.calls.lock().is_empty());
    assert!(harness.starter.inputs.lock().is_empty());
    assert_eq!(harness.run().await["status"], "failed");
    let item = first_error_item(&harness.projection().await);
    assert_eq!(item["type"], "error");
    assert_eq!(item["title"], "Provider sign-out failed");
    assert_eq!(item["failure"]["class"], "permission_error");
    assert_eq!(
        item["failure"]["message"],
        "Could not stop all sessions for this provider. Try again."
    );
}

async fn rejects_compaction_without_conversation_context(previous: Vec<&'static str>) {
    let harness = harness(Options {
        text: "/compact",
        previous_messages: previous,
        ..Options::default()
    })
    .await;

    harness.start().await.expect("start");

    assert!(harness.opener.calls.lock().is_empty());
    assert!(harness.prompts.calls.lock().is_empty());
    assert!(harness.starter.inputs.lock().is_empty());
    assert_eq!(harness.run().await["status"], "failed");
    let item = first_error_item(&harness.projection().await);
    assert_eq!(item["type"], "error");
    assert_eq!(item["failure"]["class"], "validation_error");
    assert_eq!(
        item["failure"]["message"],
        "Start a conversation before compacting this thread."
    );
}

#[tokio::test]
async fn rejects_compaction_without_conversation_context_after_0_prior_compactions() {
    rejects_compaction_without_conversation_context(vec![]).await;
}

#[tokio::test]
async fn rejects_compaction_without_conversation_context_after_2_prior_compactions() {
    rejects_compaction_without_conversation_context(vec!["/compact", " /COMPACT "]).await;
}

// -- Own cases: the happy path the local-command harness never reaches. ------

#[tokio::test]
async fn moves_the_run_to_running_and_hands_it_to_run_execution() {
    let harness = harness(Options {
        text: "Continue the work",
        healthy_session: true,
        previous_native_thread_ref: true,
        ..Options::default()
    })
    .await;

    harness.start().await.expect("start");

    let projection = harness.projection().await;
    let run = harness.run().await;
    assert_eq!(run["status"], "running");
    assert!(run["startedAt"].is_string());
    assert_eq!(
        serde_json::to_value(&projection.attempts[0]).unwrap()["status"],
        "running"
    );
    assert_eq!(
        serde_json::to_value(&projection.attempts[0]).unwrap()["nativeThreadId"],
        "native-loaded"
    );
    let started = harness
        .starter
        .inputs
        .lock()
        .pop()
        .expect("run execution started");
    assert_eq!(started.run.status, OrchestrationV2RunStatus::Running);
    assert_eq!(started.attempt_id.as_str(), ATTEMPT);
    assert_eq!(started.provider_turn_ordinal, 1);
    assert_eq!(started.message.text, "Continue the work");
    assert_eq!(
        started.command_id.as_str(),
        format!("command:effect:provider-turn.start:{RUN}")
    );
    assert_eq!(
        started.provider_thread.status,
        OrchestrationV2ProviderThreadStatus::Active
    );
    assert_eq!(
        started.provider_thread.last_run_ordinal.map(|o| o.get()),
        Some(2)
    );
    assert_eq!(started.native_thread_has_turns, Some(false));
    // Resume succeeded, so the thread is not re-created and no fallback ran.
    assert!(harness.session.ensure_inputs.lock().is_empty());
}

#[tokio::test]
async fn ignores_a_run_that_already_left_starting() {
    let harness = harness(Options {
        text: "Continue",
        healthy_session: true,
        ..Options::default()
    })
    .await;
    harness.start().await.expect("start");
    harness.start().await.expect("a running run is a no-op");

    assert_eq!(harness.starter.inputs.lock().len(), 1);
    assert_eq!(harness.opener.calls.lock().len(), 1);
}

#[tokio::test]
async fn delivers_a_ready_handoff_with_the_first_provider_turn() {
    let handoff = json!({
        "id": "handoff:ready", "threadId": THREAD, "targetRunId": RUN,
        "fromProviderThreadIds": [], "toProviderThreadId": PROVIDER_THREAD,
        "coveredRunOrdinals": {"from": 1, "to": 1}, "strategy": "manual_context",
        "status": "ready", "summaryMessageId": null, "summaryText": "Earlier we chose SQLite.",
        "createdByProviderInstanceId": null, "createdAt": T0, "updatedAt": T0,
    });
    let harness = harness(Options {
        text: "Why SQLite?",
        healthy_session: true,
        previous_native_thread_ref: true,
        extra_events: vec![event("context-handoff.updated", THREAD, T0, handoff)],
        ..Options::default()
    })
    .await;

    harness.start().await.expect("start");

    let started = harness
        .starter
        .inputs
        .lock()
        .pop()
        .expect("run execution started");
    // The run execution drives the delivery wrapper, which injects the
    // handoff into the first turn's prompt.
    let turn = ProviderAdapterV2TurnInput {
        app_thread: started.app_thread.clone(),
        thread_id: started.run.thread_id.clone(),
        run_id: started.run.id.clone(),
        run_ordinal: 2,
        native_thread_has_turns: None,
        provider_turn_ordinal: 1,
        restart_continuation_of_run_id: None,
        attempt_id: started.attempt_id.clone(),
        root_node_id: started.root_node.id.clone(),
        provider_thread: started.provider_thread.clone(),
        message: started.message.clone(),
        model_selection: started.model_selection.clone(),
        runtime_policy: started.runtime_policy.clone(),
        app_context: None,
    };
    started
        .session
        .start_turn(turn.clone())
        .await
        .expect("turn starts");
    let sent = harness.session.starts.lock().pop().expect("provider call");
    assert!(
        sent.message.text.contains("Earlier we chose SQLite."),
        "{}",
        sent.message.text
    );
    assert!(sent.message.text.ends_with("User message:\nWhy SQLite?"));
    // The handoff was recorded as delivered, inline (this session has no
    // injection hook).
    let delivered = harness
        .store
        .get_thread_records(
            &ThreadId::new(THREAD).unwrap(),
            vec![ProjectionRecordField::ContextHandoffs],
            None,
        )
        .await
        .expect("handoffs");
    let record = serde_json::to_value(&delivered.context_handoffs[0]).unwrap();
    assert_eq!(record["delivery"]["status"], "inline");
    assert_eq!(record["delivery"]["nativeThreadId"], "native-loaded");
    // A second start on the same wrapper is a plain provider call: the plan is gone.
    started.session.start_turn(turn).await.expect("second turn");
    let plain = harness.session.starts.lock().pop().expect("provider call");
    assert_eq!(plain.message.text, "Why SQLite?");
}

#[tokio::test]
async fn skips_the_start_when_the_run_moved_while_the_thread_loaded() {
    let harness = harness(Options {
        text: "Continue",
        healthy_session: true,
        ..Options::default()
    })
    .await;
    // The thread loads, but the run is no longer the starting attempt.
    let store = harness.store.clone();
    *harness.opener.before_answer.lock() = Some(Box::new(move || {
        futures::executor::block_on(async {
            store
                .apply(event(
                    "run.updated",
                    THREAD,
                    "2026-09-04T12:00:01.000Z",
                    json!({
                        "id": RUN, "threadId": THREAD, "ordinal": 2, "providerInstanceId": NEW_INSTANCE,
                        "modelSelection": {"instanceId": NEW_INSTANCE, "model": "gpt-5.4"},
                        "providerThreadId": PROVIDER_THREAD, "userMessageId": MESSAGE,
                        "rootNodeId": ROOT_NODE, "activeAttemptId": ATTEMPT, "status": "interrupted",
                        "requestedAt": T0, "startedAt": null, "completedAt": T0,
                        "checkpointId": null, "contextHandoffId": null,
                    }),
                ))
                .await
                .expect("interrupt");
        });
    }));

    harness
        .start()
        .await
        .expect("a superseded start is not an error");

    assert!(harness.starter.inputs.lock().is_empty());
    assert!(harness.writer.guarded.lock().is_empty());
    assert_eq!(harness.run().await["status"], "interrupted");
}
