//! Behaviour tests for [`ProviderSessionManager`]. Upstream's
//! `ProviderSessionManager.test.ts` drives these paths through Effect layers;
//! here a fake adapter and runtime stand in for the provider, and a recording
//! sink stands in for `EventSink`. Idle timing uses paused tokio time.

use std::{
    collections::VecDeque,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};

use async_trait::async_trait;
use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use turin_contracts::{
    NodeId, ProviderSessionId, RunAttemptId, RunId, ThreadId,
    model_selection::ModelSelection,
    orchestration_v2::{OrchestrationV2ProviderSession, OrchestrationV2ProviderThread},
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
};
use turin_provider_core::provider_adapter::{
    ProviderAdapterErrorKind, ProviderAdapterV2, ProviderAdapterV2Error, ProviderAdapterV2Event,
    ProviderAdapterV2OpenSessionInput, ProviderAdapterV2RuntimePolicy, ProviderAdapterV2TurnInput,
    ProviderEventStream, ProviderSessionRuntime,
};
use turin_provider_core::rt::BoxFuture;

use super::*;
use crate::projection::test_support::capabilities;
use turin_contracts::ProviderTurnId;

const T0: &str = "2026-01-01T00:00:00.000Z";
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

type RuntimeItem = Result<ProviderAdapterV2Event, ProviderAdapterV2Error>;

fn session_json(id: &str, status: &str, multiple: bool) -> Value {
    let mut capabilities = capabilities();
    capabilities["sessions"]["supportsMultipleProviderThreadsPerSession"] = json!(multiple);
    json!({
        "id": id,
        "driver": "codex",
        "providerInstanceId": "codex",
        "status": status,
        "cwd": "/",
        "model": null,
        "capabilities": capabilities,
        "createdAt": T0,
        "updatedAt": T0,
        "lastError": null,
    })
}

fn session(id: &str, multiple: bool) -> OrchestrationV2ProviderSession {
    serde_json::from_value(session_json(id, "ready", multiple)).expect("session fixture")
}

fn thread_json(thread: &str) -> Value {
    json!({
        "createdBy": "user",
        "creationSource": "web",
        "id": thread,
        "projectId": "project:test",
        "title": thread,
        "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt-5.4"},
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "branch": null,
        "worktreePath": null,
        "activeProviderThreadId": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": thread},
        "forkedFrom": null,
        "createdAt": T0,
        "updatedAt": T0,
        "archivedAt": null,
        "settledOverride": null,
        "settledAt": null,
        "lastVisitedAt": null,
        "deletedAt": null,
    })
}

fn provider_thread(thread: &str, session: &str) -> OrchestrationV2ProviderThread {
    serde_json::from_value(json!({
        "id": format!("provider-thread:{thread}"),
        "driver": "codex",
        "providerInstanceId": "codex",
        "providerSessionId": session,
        "appThreadId": thread,
        "ownerNodeId": null,
        "nativeThreadRef": null,
        "nativeConversationHeadRef": null,
        "status": "idle",
        "firstRunOrdinal": null,
        "lastRunOrdinal": null,
        "handoffIds": [],
        "forkedFrom": null,
        "pendingBackgroundTasks": [],
        "contextUsage": null,
        "nativeMetadata": null,
        "goal": null,
        "createdAt": T0,
        "updatedAt": T0,
    }))
    .expect("provider thread fixture")
}

fn open_input(thread: &str, session: &str) -> ProviderAdapterV2OpenSessionInput {
    ProviderAdapterV2OpenSessionInput {
        thread_id: ThreadId::new(thread).expect("fixture id"),
        provider_session_id: ProviderSessionId::new(session).expect("fixture id"),
        model_selection: serde_json::from_value(json!({"instanceId": "codex", "model": "gpt-5.4"}))
            .expect("model selection fixture"),
        runtime_policy: runtime_policy(None),
        resume_from_session: None,
        initial_native_thread_id: None,
        initial_provider_item_identity_version: None,
    }
}

fn runtime_policy(cwd: Option<&str>) -> ProviderAdapterV2RuntimePolicy {
    serde_json::from_value(json!({
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "cwd": cwd,
    }))
    .expect("runtime policy fixture")
}

fn turn_input(thread: &str, session: &str) -> ProviderAdapterV2TurnInput {
    ProviderAdapterV2TurnInput {
        app_thread: serde_json::from_value(thread_json(thread)).expect("app thread fixture"),
        thread_id: ThreadId::new(thread).expect("fixture id"),
        run_id: RunId::new("run:1").expect("fixture id"),
        run_ordinal: 1,
        native_thread_has_turns: None,
        provider_turn_ordinal: 1,
        restart_continuation_of_run_id: None,
        attempt_id: RunAttemptId::new("attempt:1").expect("fixture id"),
        root_node_id: NodeId::new("node:root").expect("fixture id"),
        provider_thread: provider_thread(thread, session),
        message: serde_json::from_value(json!({
            "messageId": "message:1",
            "text": "hi",
            "attachments": [],
            "createdBy": "user",
            "creationSource": "web",
            "scheduledTaskId": null,
            "senderThreadId": null,
        }))
        .expect("turn message fixture"),
        model_selection: serde_json::from_value::<ModelSelection>(
            json!({"instanceId": "codex", "model": "gpt-5.4"}),
        )
        .expect("model selection fixture"),
        runtime_policy: runtime_policy(None),
        app_context: None,
    }
}

fn provider_session_updated(session_status: &str) -> ProviderAdapterV2Event {
    serde_json::from_value(json!({
        "type": "provider_session.updated",
        "driver": "codex",
        "provider_session": session_json("provider-session:1", session_status, false),
    }))
    .expect("provider session event fixture")
}

fn turn_terminal(thread: &str) -> ProviderAdapterV2Event {
    serde_json::from_value(json!({
        "type": "turn.terminal",
        "driver": "codex",
        "provider_thread_id": format!("provider-thread:{thread}"),
        "provider_turn_id": "provider-turn:1",
        "run_ordinal": 1,
        "status": "completed",
        "failure": null,
        "thread_disposition": "reusable",
    }))
    .expect("turn terminal fixture")
}

/// One provider session's runtime. Tests push events through `push`, end the
/// stream with `end`, and can make the next `start_turn` fail.
struct FakeRuntime {
    session: OrchestrationV2ProviderSession,
    events_tx: Mutex<Option<mpsc::UnboundedSender<RuntimeItem>>>,
    events_rx: Mutex<Option<mpsc::UnboundedReceiver<RuntimeItem>>>,
    fail_start: AtomicBool,
    closes: AtomicUsize,
    interrupts: AtomicUsize,
    tasks_end_with_process: AtomicBool,
}

impl FakeRuntime {
    fn new(session: OrchestrationV2ProviderSession) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            session,
            events_tx: Mutex::new(Some(tx)),
            events_rx: Mutex::new(Some(rx)),
            fail_start: AtomicBool::new(false),
            closes: AtomicUsize::new(0),
            interrupts: AtomicUsize::new(0),
            tasks_end_with_process: AtomicBool::new(false),
        })
    }

    fn push(&self, item: RuntimeItem) {
        if let Some(tx) = self.events_tx.lock().as_ref() {
            tx.send(item).expect("pump is listening");
        }
    }

    fn end(&self) {
        self.events_tx.lock().take();
    }
}

#[async_trait]
impl ProviderSessionRuntime for FakeRuntime {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.session.provider_instance_id
    }

    fn background_tasks_end_with_process(&self) -> bool {
        self.tasks_end_with_process.load(Ordering::SeqCst)
    }

    fn driver(&self) -> &ProviderDriverKind {
        &self.session.driver
    }

    fn provider_session_id(&self) -> &ProviderSessionId {
        &self.session.id
    }

    fn provider_session(&self) -> &OrchestrationV2ProviderSession {
        &self.session
    }

    fn events(&self) -> ProviderEventStream {
        let rx = self
            .events_rx
            .lock()
            .take()
            .expect("the session pump takes the stream once");
        Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        }))
    }

    async fn ensure_thread(
        &self,
        _input: turin_provider_core::provider_adapter::ProviderAdapterV2EnsureThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        unimplemented!("not exercised by session manager tests")
    }

    async fn resume_thread(
        &self,
        _input: turin_provider_core::provider_adapter::ProviderAdapterV2ResumeThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        unimplemented!("not exercised by session manager tests")
    }

    async fn start_turn(
        &self,
        _input: ProviderAdapterV2TurnInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        if self.fail_start.load(Ordering::SeqCst) {
            return Err(ProviderAdapterV2Error::new(
                ProviderAdapterErrorKind::EventStream {
                    driver: self.session.driver.clone(),
                    provider_session_id: self.session.id.clone(),
                },
            ));
        }
        Ok(())
    }

    async fn steer_turn(
        &self,
        _input: turin_provider_core::provider_adapter::ProviderAdapterV2SteerInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        unimplemented!("not exercised by session manager tests")
    }

    async fn interrupt_turn(
        &self,
        _input: turin_provider_core::provider_adapter::ProviderAdapterV2InterruptInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        self.interrupts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn close(&self) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        Some(Box::pin(async { Ok(()) }))
    }

    async fn respond_to_runtime_request(
        &self,
        _input: turin_provider_core::provider_adapter::ProviderAdapterV2RuntimeRequestResponseInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        unimplemented!("not exercised by session manager tests")
    }

    async fn read_thread_snapshot(
        &self,
        _input: turin_provider_core::provider_adapter::ProviderAdapterV2ReadThreadSnapshotInput,
    ) -> Result<
        turin_provider_core::provider_adapter::ProviderAdapterV2ThreadSnapshot,
        ProviderAdapterV2Error,
    > {
        unimplemented!("not exercised by session manager tests")
    }

    async fn rollback_thread(
        &self,
        _input: turin_provider_core::provider_adapter::ProviderAdapterV2RollbackThreadInput,
    ) -> Result<
        turin_provider_core::provider_adapter::ProviderAdapterV2ThreadSnapshot,
        ProviderAdapterV2Error,
    > {
        unimplemented!("not exercised by session manager tests")
    }

    async fn fork_thread(
        &self,
        _input: turin_provider_core::provider_adapter::ProviderAdapterV2ForkThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        unimplemented!("not exercised by session manager tests")
    }
}

/// Hands out queued runtimes from `open_session`, one per open.
struct FakeAdapter {
    instance: ProviderInstanceId,
    runtimes: Mutex<VecDeque<Arc<FakeRuntime>>>,
}

#[async_trait]
impl ProviderAdapterV2 for FakeAdapter {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.instance
    }

    fn driver(&self) -> &ProviderDriverKind {
        static DRIVER: std::sync::OnceLock<ProviderDriverKind> = std::sync::OnceLock::new();
        DRIVER.get_or_init(|| serde_json::from_value(json!("codex")).expect("driver fixture"))
    }

    async fn get_capabilities(
        &self,
    ) -> Result<
        turin_contracts::orchestration_v2::OrchestrationV2ProviderCapabilities,
        ProviderAdapterV2Error,
    > {
        unimplemented!("not exercised by session manager tests")
    }

    async fn plan_selection_transition(
        &self,
        _input: turin_provider_core::selection_transition::ProviderSelectionTransitionInput,
    ) -> Result<
        turin_provider_core::selection_transition::ProviderSelectionTransitionPlan,
        ProviderAdapterV2Error,
    > {
        unimplemented!("not exercised by session manager tests")
    }

    async fn open_session(
        &self,
        _input: ProviderAdapterV2OpenSessionInput,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderAdapterV2Error> {
        let runtime = self
            .runtimes
            .lock()
            .pop_front()
            .expect("test queued a runtime for this open");
        Ok(runtime)
    }
}

struct FixedLookup(Arc<dyn ProviderAdapterV2>);

impl ProviderAdapterLookup for FixedLookup {
    fn adapter(&self, _instance_id: &ProviderInstanceId) -> Option<Arc<dyn ProviderAdapterV2>> {
        Some(Arc::clone(&self.0))
    }
}

#[derive(Default)]
struct RecordingSink {
    attached: Mutex<Vec<(String, Vec<ThreadId>)>>,
    releases: Mutex<Vec<SessionRelease>>,
    fail_attach: AtomicBool,
}

#[async_trait]
impl ProviderSessionSink for RecordingSink {
    async fn session_attached(
        &self,
        identity: &ProviderSessionIdentity,
        thread_ids: &[ThreadId],
        _provider_session: &OrchestrationV2ProviderSession,
    ) -> Result<(), BoxError> {
        if self.fail_attach.load(Ordering::SeqCst) {
            return Err("attach write failed".into());
        }
        self.attached.lock().push((
            identity.provider_session_id.as_str().to_owned(),
            thread_ids.to_vec(),
        ));
        Ok(())
    }

    async fn session_updated(
        &self,
        _identity: &ProviderSessionIdentity,
        _thread_ids: &[ThreadId],
        _provider_session: &OrchestrationV2ProviderSession,
    ) -> Result<(), BoxError> {
        Ok(())
    }

    async fn session_released(&self, release: &SessionRelease) -> Result<(), BoxError> {
        self.releases.lock().push(release.clone());
        Ok(())
    }

    async fn ingest_session_scoped_event(
        &self,
        _identity: &ProviderSessionIdentity,
        _thread_id: ThreadId,
        _event: ProviderAdapterV2Event,
    ) -> Result<(), BoxError> {
        Ok(())
    }
}

/// Projection reads that report nothing, unless a test sets `turns`.
#[derive(Default)]
struct FakeReads {
    turns: Mutex<Vec<RunningProviderTurn>>,
    /// The thread lists the roster read was asked for.
    roster_reads: Mutex<Vec<Vec<ThreadId>>>,
}

#[async_trait]
impl ProviderSessionReads for FakeReads {
    async fn running_turns(
        &self,
        _thread_id: &ThreadId,
        _provider_session_id: &ProviderSessionId,
    ) -> Result<Vec<RunningProviderTurn>, BoxError> {
        Ok(self.turns.lock().clone())
    }

    async fn pending_background_tasks(
        &self,
        thread_ids: &[ThreadId],
        _provider_session_id: &ProviderSessionId,
    ) -> Result<Vec<ReleasedBackgroundTasks>, BoxError> {
        self.roster_reads.lock().push(thread_ids.to_vec());
        Ok(Vec::new())
    }
}

struct Harness {
    manager: ProviderSessionManager,
    sink: Arc<RecordingSink>,
}

fn harness(runtimes: Vec<Arc<FakeRuntime>>) -> Harness {
    harness_with_reads(runtimes, Arc::new(FakeReads::default()))
}

fn harness_with_reads(runtimes: Vec<Arc<FakeRuntime>>, reads: Arc<FakeReads>) -> Harness {
    let adapter = Arc::new(FakeAdapter {
        instance: serde_json::from_value(json!("codex")).expect("instance fixture"),
        runtimes: Mutex::new(runtimes.iter().cloned().collect()),
    });
    let sink = Arc::new(RecordingSink::default());
    let manager = ProviderSessionManager::new(
        Arc::new(FixedLookup(adapter)),
        Arc::clone(&sink) as Arc<dyn ProviderSessionSink>,
        reads,
        ProviderSessionManagerOptions {
            idle_timeout: IDLE_TIMEOUT,
            max_idle_pin: Duration::from_secs(4 * 60 * 60),
        },
    );
    Harness { manager, sink }
}

/// Lets spawned tasks (pump, idle timer) run until they block. Paused time
/// only advances explicitly, so this does not depend on wall-clock timing.
async fn settle() {
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
}

fn provider_session_id(session: &str) -> ProviderSessionId {
    ProviderSessionId::new(session).expect("fixture id")
}

#[tokio::test]
async fn open_attaches_thread_and_returns_live_session() {
    let runtime = FakeRuntime::new(session("provider-session:1", false));
    let harness = harness(vec![runtime]);

    let opened = harness
        .manager
        .open(open_input("thread:a", "provider-session:1"))
        .await
        .expect("open");

    assert_eq!(
        opened.provider_session_id(),
        &provider_session_id("provider-session:1")
    );
    assert!(
        harness
            .manager
            .get(&provider_session_id("provider-session:1"))
            .is_some()
    );
    let attached = harness.sink.attached.lock().clone();
    assert_eq!(
        attached,
        vec![(
            "provider-session:1".to_owned(),
            vec![ThreadId::new("thread:a").expect("fixture id")]
        )]
    );
}

#[tokio::test]
async fn open_reuses_multi_thread_session_and_rejects_second_thread_on_single_thread_session() {
    let shared = FakeRuntime::new(session("provider-session:shared", true));
    let single = FakeRuntime::new(session("provider-session:single", false));
    let harness = harness(vec![shared, single]);

    harness
        .manager
        .open(open_input("thread:a", "provider-session:shared"))
        .await
        .expect("first open");
    harness
        .manager
        .open(open_input("thread:b", "provider-session:shared"))
        .await
        .expect("second thread joins shared session");
    let attached = harness.sink.attached.lock().clone();
    assert_eq!(attached.len(), 2, "joining a thread writes an attach row");
    assert_eq!(
        attached[1].1,
        vec![ThreadId::new("thread:b").expect("fixture id")]
    );

    harness
        .manager
        .open(open_input("thread:a", "provider-session:single"))
        .await
        .expect("first open of single-thread session");
    let error = harness
        .manager
        .open(open_input("thread:b", "provider-session:single"))
        .await
        .err()
        .unwrap_or_else(|| panic!("single-thread session refuses a second thread"));
    assert!(
        error
            .to_string()
            .contains("Failed to open provider instance")
    );
}

#[tokio::test]
async fn open_fails_with_workspace_missing_when_cwd_is_gone() {
    let harness = harness(vec![FakeRuntime::new(session("provider-session:1", false))]);
    let mut input = open_input("thread:a", "provider-session:1");
    input.runtime_policy = runtime_policy(Some("/definitely/missing/turin-workspace"));

    let error = harness
        .manager
        .open(input)
        .await
        .err()
        .unwrap_or_else(|| panic!("missing workspace must fail the open"));

    assert!(matches!(
        error,
        ProviderSessionManagerError::WorkspaceMissing(_)
    ));
    assert!(harness.sink.attached.lock().is_empty());
}

#[tokio::test]
async fn attach_write_failure_releases_the_new_session() {
    let harness = harness(vec![FakeRuntime::new(session("provider-session:1", false))]);
    harness.sink.fail_attach.store(true, Ordering::SeqCst);

    harness
        .manager
        .open(open_input("thread:a", "provider-session:1"))
        .await
        .err()
        .unwrap_or_else(|| panic!("attach write failure must fail the open"));
    settle().await;

    assert!(
        harness
            .manager
            .get(&provider_session_id("provider-session:1"))
            .is_none()
    );
    let releases = harness.sink.releases.lock().clone();
    assert_eq!(releases.len(), 1);
    assert_eq!(
        releases[0].reason,
        ProviderSessionReleaseReason::RuntimeError
    );
}

#[tokio::test]
async fn manual_close_fails_subscribers_and_records_release() {
    let runtime = FakeRuntime::new(session("provider-session:1", false));
    let harness = harness(vec![Arc::clone(&runtime)]);
    let opened = harness
        .manager
        .open(open_input("thread:a", "provider-session:1"))
        .await
        .expect("open");
    let mut subscription = opened
        .subscribe_events()
        .expect("managed runtime subscribes")
        .await
        .expect("subscription");

    runtime.push(Ok(provider_session_updated("ready")));
    let first = subscription
        .events
        .next()
        .await
        .expect("event")
        .expect("ok event");
    assert!(matches!(
        first,
        ProviderAdapterV2Event::ProviderSessionUpdated { .. }
    ));

    harness
        .manager
        .close(&provider_session_id("provider-session:1"))
        .await
        .expect("close");

    let failure = subscription.events.next().await.expect("failure item");
    assert!(failure.is_err(), "subscriber sees the release as an error");
    assert!(
        subscription.events.next().await.is_none(),
        "stream ends after the error"
    );
    let releases = harness.sink.releases.lock().clone();
    assert_eq!(releases.len(), 1);
    assert_eq!(
        releases[0].reason,
        ProviderSessionReleaseReason::ManualShutdown
    );
    assert_eq!(
        releases[0].thread_ids,
        vec![ThreadId::new("thread:a").expect("fixture id")]
    );
}

#[tokio::test]
async fn provider_reported_stop_ends_subscribers_gracefully() {
    let runtime = FakeRuntime::new(session("provider-session:1", false));
    let harness = harness(vec![Arc::clone(&runtime)]);
    let opened = harness
        .manager
        .open(open_input("thread:a", "provider-session:1"))
        .await
        .expect("open");
    let mut subscription = opened
        .subscribe_events()
        .expect("managed runtime subscribes")
        .await
        .expect("subscription");

    runtime.push(Ok(provider_session_updated("stopped")));
    runtime.end();

    let stopped = subscription
        .events
        .next()
        .await
        .expect("stopped event")
        .expect("ok event");
    assert!(matches!(
        stopped,
        ProviderAdapterV2Event::ProviderSessionUpdated { .. }
    ));
    assert!(
        subscription.events.next().await.is_none(),
        "graceful end, no error"
    );
    settle().await;
    let releases = harness.sink.releases.lock().clone();
    assert_eq!(releases.len(), 1);
    assert_eq!(
        releases[0].reason,
        ProviderSessionReleaseReason::ManualShutdown
    );
    assert!(releases[0].detail.is_none());
}

#[tokio::test]
async fn event_stream_failure_releases_as_runtime_error() {
    let runtime = FakeRuntime::new(session("provider-session:1", false));
    let harness = harness(vec![Arc::clone(&runtime)]);
    harness
        .manager
        .open(open_input("thread:a", "provider-session:1"))
        .await
        .expect("open");

    runtime.push(Err(ProviderAdapterV2Error::new(
        ProviderAdapterErrorKind::EventStream {
            driver: serde_json::from_value(json!("codex")).expect("driver fixture"),
            provider_session_id: ProviderSessionId::new("provider-session:1").expect("fixture id"),
        },
    )));
    settle().await;

    assert!(
        harness
            .manager
            .get(&provider_session_id("provider-session:1"))
            .is_none()
    );
    let releases = harness.sink.releases.lock().clone();
    assert_eq!(releases.len(), 1);
    assert_eq!(
        releases[0].reason,
        ProviderSessionReleaseReason::RuntimeError
    );
    assert!(releases[0].detail.is_some());
}

#[tokio::test(start_paused = true)]
async fn idle_session_releases_after_timeout() {
    let harness = harness(vec![FakeRuntime::new(session("provider-session:1", false))]);
    harness
        .manager
        .open(open_input("thread:a", "provider-session:1"))
        .await
        .expect("open");
    settle().await;

    tokio::time::advance(IDLE_TIMEOUT + Duration::from_secs(1)).await;
    settle().await;

    assert!(
        harness
            .manager
            .get(&provider_session_id("provider-session:1"))
            .is_none()
    );
    let releases = harness.sink.releases.lock().clone();
    assert_eq!(releases.len(), 1);
    assert_eq!(
        releases[0].reason,
        ProviderSessionReleaseReason::IdleTimeout
    );
}

#[tokio::test(start_paused = true)]
async fn running_turn_holds_session_until_its_terminal_event() {
    let runtime = FakeRuntime::new(session("provider-session:1", false));
    let harness = harness(vec![Arc::clone(&runtime)]);
    let opened = harness
        .manager
        .open(open_input("thread:a", "provider-session:1"))
        .await
        .expect("open");
    opened
        .start_turn(turn_input("thread:a", "provider-session:1"))
        .await
        .expect("start turn");

    // A busy turn cancels the idle timer, so no amount of quiet releases the session.
    tokio::time::advance(IDLE_TIMEOUT * 10).await;
    settle().await;
    assert!(
        harness
            .manager
            .get(&provider_session_id("provider-session:1"))
            .is_some()
    );

    runtime.push(Ok(turn_terminal("thread:a")));
    settle().await;
    tokio::time::advance(IDLE_TIMEOUT + Duration::from_secs(1)).await;
    settle().await;

    assert!(
        harness
            .manager
            .get(&provider_session_id("provider-session:1"))
            .is_none()
    );
    assert_eq!(
        harness.sink.releases.lock()[0].reason,
        ProviderSessionReleaseReason::IdleTimeout
    );
}

#[tokio::test(start_paused = true)]
async fn failed_turn_start_clears_its_busy_mark() {
    let runtime = FakeRuntime::new(session("provider-session:1", false));
    runtime.fail_start.store(true, Ordering::SeqCst);
    let harness = harness(vec![runtime]);
    let opened = harness
        .manager
        .open(open_input("thread:a", "provider-session:1"))
        .await
        .expect("open");

    opened
        .start_turn(turn_input("thread:a", "provider-session:1"))
        .await
        .expect_err("start fails");
    settle().await;
    tokio::time::advance(IDLE_TIMEOUT + Duration::from_secs(1)).await;
    settle().await;

    assert!(
        harness
            .manager
            .get(&provider_session_id("provider-session:1"))
            .is_none(),
        "a failed start must not pin the session busy"
    );
}

#[tokio::test]
async fn close_instance_releases_only_that_instances_sessions() {
    let harness = harness(vec![FakeRuntime::new(session("provider-session:1", false))]);
    harness
        .manager
        .open(open_input("thread:a", "provider-session:1"))
        .await
        .expect("open");

    harness
        .manager
        .close_instance(&serde_json::from_value(json!("codex")).expect("instance fixture"))
        .await
        .expect("close instance");
    settle().await;

    assert!(
        harness
            .manager
            .get(&provider_session_id("provider-session:1"))
            .is_none()
    );
    let releases = harness.sink.releases.lock().clone();
    assert_eq!(releases.len(), 1);
    assert_eq!(
        releases[0].detail.as_deref(),
        Some("Provider instance codex logged out.")
    );
}

#[tokio::test]
async fn detach_of_last_thread_releases_single_thread_session() {
    let harness = harness(vec![FakeRuntime::new(session("provider-session:1", false))]);
    harness
        .manager
        .open(open_input("thread:a", "provider-session:1"))
        .await
        .expect("open");

    harness
        .manager
        .detach(
            &provider_session_id("provider-session:1"),
            &ThreadId::new("thread:a").expect("fixture id"),
            None,
        )
        .await
        .expect("detach");

    assert!(
        harness
            .manager
            .get(&provider_session_id("provider-session:1"))
            .is_none()
    );
}

/// The detaching thread already left the attached set, but its background
/// tasks end with the session's process, so its roster is read for the release.
#[tokio::test]
async fn a_detach_that_releases_the_session_reads_the_detached_threads_roster() {
    let runtime = FakeRuntime::new(session("provider-session:1", false));
    runtime.tasks_end_with_process.store(true, Ordering::SeqCst);
    let reads = Arc::new(FakeReads::default());
    let harness = harness_with_reads(vec![runtime], reads.clone());
    harness
        .manager
        .open(open_input("thread:a", "provider-session:1"))
        .await
        .expect("open");

    harness
        .manager
        .detach(
            &provider_session_id("provider-session:1"),
            &ThreadId::new("thread:a").expect("fixture id"),
            None,
        )
        .await
        .expect("detach");

    let reads = reads.roster_reads.lock().clone();
    assert_eq!(
        reads,
        vec![vec![ThreadId::new("thread:a").expect("fixture id")]],
        "the detached thread is in the roster read"
    );
}

/// Upstream `removeLiveEntry` also refuses an idle removal while turns are
/// busy: `mark_busy` does not bump the idle generation.
#[tokio::test]
async fn an_idle_removal_never_takes_a_session_with_a_busy_turn() {
    let harness = harness(vec![FakeRuntime::new(session("provider-session:1", false))]);
    harness
        .manager
        .open(open_input("thread:a", "provider-session:1"))
        .await
        .expect("open");
    let id = provider_session_id("provider-session:1");
    let live = harness
        .manager
        .inner
        .get_session(&id)
        .expect("live session");
    let generation = live.state.lock().idle_generation;
    let request = || ReleaseRequest {
        provider_session_id: id.clone(),
        reason: ProviderSessionReleaseReason::IdleTimeout,
        detail: None,
        graceful_subscribers: true,
        only_if_idle_generation: Some(generation),
        origin: ReleaseOrigin::IdleTimer,
        detached_thread_id: None,
    };

    harness.manager.inner.mark_busy(
        &live,
        (
            turin_contracts::ProviderThreadId::new("provider-thread:1").expect("id"),
            1,
        ),
    );
    assert!(
        harness
            .manager
            .inner
            .remove_live_session(&request())
            .is_none(),
        "a busy session stays"
    );
    assert!(harness.manager.get(&id).is_some());
    live.state.lock().busy_turns.clear();
    assert!(
        harness
            .manager
            .inner
            .remove_live_session(&request())
            .is_some()
    );
}

#[tokio::test]
async fn shutdown_releases_every_session_with_server_shutdown() {
    let harness = harness(vec![FakeRuntime::new(session("provider-session:1", false))]);
    harness
        .manager
        .open(open_input("thread:a", "provider-session:1"))
        .await
        .expect("open");

    harness.manager.shutdown().await;

    assert!(
        harness
            .manager
            .get(&provider_session_id("provider-session:1"))
            .is_none()
    );
    let releases = harness.sink.releases.lock().clone();
    assert_eq!(releases.len(), 1);
    assert_eq!(
        releases[0].reason,
        ProviderSessionReleaseReason::ServerShutdown
    );
}

#[test]
fn release_status_matches_upstream_mapping() {
    assert_eq!(
        release_status_for(ProviderSessionReleaseReason::RuntimeError),
        OrchestrationV2ProviderSessionStatus::Error
    );
    for reason in [
        ProviderSessionReleaseReason::IdleTimeout,
        ProviderSessionReleaseReason::ManualShutdown,
        ProviderSessionReleaseReason::ServerShutdown,
    ] {
        assert_eq!(
            release_status_for(reason),
            OrchestrationV2ProviderSessionStatus::Stopped
        );
    }
}

#[tokio::test]
async fn release_closes_the_runtime_before_recording() {
    let runtime = FakeRuntime::new(session("provider-session:1", false));
    let harness = harness(vec![Arc::clone(&runtime)]);
    harness
        .manager
        .open(open_input("thread:a", "provider-session:1"))
        .await
        .expect("open");

    harness
        .manager
        .close(&provider_session_id("provider-session:1"))
        .await
        .expect("close");

    assert_eq!(runtime.closes.load(Ordering::SeqCst), 1);
    assert_eq!(harness.sink.releases.lock().len(), 1);
}

#[tokio::test]
async fn detach_interrupts_the_threads_running_turns_on_a_shared_session() {
    let runtime = FakeRuntime::new(session("provider-session:shared", true));
    let reads = Arc::new(FakeReads::default());
    reads.turns.lock().push(RunningProviderTurn {
        provider_thread: provider_thread("thread:a", "provider-session:shared"),
        provider_turn_id: ProviderTurnId::new("provider-turn:1").expect("fixture id"),
    });
    let harness = harness_with_reads(vec![Arc::clone(&runtime)], reads);
    harness
        .manager
        .open(open_input("thread:a", "provider-session:shared"))
        .await
        .expect("open");

    harness
        .manager
        .detach(
            &provider_session_id("provider-session:shared"),
            &ThreadId::new("thread:a").expect("fixture id"),
            None,
        )
        .await
        .expect("detach");

    assert_eq!(runtime.interrupts.load(Ordering::SeqCst), 1);
    assert!(
        harness
            .manager
            .get(&provider_session_id("provider-session:shared"))
            .is_some(),
        "a shared session stays up after its thread detaches"
    );
}
