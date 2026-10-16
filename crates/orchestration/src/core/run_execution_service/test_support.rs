//! Doubles for the `RunExecutionService` tests (upstream `Layer.mock`s and
//! the `as unknown as ProviderAdapterV2SessionRuntime` literals). Reused by
//! the `ProviderTurnStartService` tests, which drive the same service.

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::Notify;
use turin_contracts::{
    ProviderSessionId,
    orchestration_v2::{
        OrchestrationV2DomainEvent, OrchestrationV2ProviderSession, OrchestrationV2ProviderThread,
    },
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
};
use turin_provider_core::{
    provider_adapter::{
        ProviderAdapterErrorKind, ProviderAdapterV2EnsureThreadInput, ProviderAdapterV2Error,
        ProviderAdapterV2Event, ProviderAdapterV2ForkThreadInput, ProviderAdapterV2InterruptInput,
        ProviderAdapterV2ReadThreadSnapshotInput, ProviderAdapterV2ResumeThreadInput,
        ProviderAdapterV2RollbackThreadInput, ProviderAdapterV2RuntimeRequestResponseInput,
        ProviderAdapterV2SteerInput, ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2TurnInput,
        ProviderEventStream, ProviderSessionRuntime,
    },
    rt::BoxFuture,
};

use super::{
    AfterTurnRefresh, RunBaselineCapture, RunEventIngest, RunEventWriter, RunExecutionService,
    StartRootRunInput,
};
use crate::{
    core::provider_event_ingestor::{IngestGuard, ProviderEventInput},
    pipeline::{
        event_sink::{RunCurrentWrite, WriteInput},
        orchestrator_port::BoxError,
        seams::ServerSettingsSource,
        test_support::StaticSettings,
    },
};

pub(crate) const NOW: &str = "2026-06-22T00:00:00.000Z";

/// Everything the service wrote or called, with the knobs the tests turn.
#[derive(Default)]
pub(crate) struct Recorder {
    pub state: Mutex<RecorderState>,
    pub changed: Notify,
}

#[derive(Default)]
pub(crate) struct RecorderState {
    /// `writeWithEffects` calls.
    pub writes: Vec<WriteInput>,
    /// `writeIfRunCurrent` calls.
    pub guarded: Vec<RunCurrentWrite>,
    /// Events of every write that committed, in order.
    pub committed: Vec<OrchestrationV2DomainEvent>,
    /// `ingestNormalized` calls.
    pub ingested: Vec<(ProviderEventInput, IngestGuard)>,
    pub baseline_calls: Vec<u64>,
    pub refreshes: usize,
    pub refresh_fails: bool,
    /// `run:<status>` per settled run and `pull-requests-refreshed`, in order.
    pub observed: Vec<String>,
    /// `committed` of a guarded write.
    pub reject_guarded: bool,
    pub baseline_fails: bool,
    /// How many events an ingest reports stored; defaults to one.
    pub stored_per_ingest: VecDeque<usize>,
    /// What happened, in order: one label per ingested event, and
    /// `root-finalized` for the write that settles a run.
    pub log: Vec<String>,
    /// Decides how many events an ingest stores (overrides `stored_per_ingest`).
    #[allow(clippy::type_complexity)]
    pub ingest_hook: Option<Box<dyn FnMut(&ProviderEventInput, &IngestGuard) -> usize + Send>>,
}

/// A short label for an adapter event, e.g. `turn_item:running`.
pub(crate) fn event_label(event: &ProviderAdapterV2Event) -> String {
    let wire = serde_json::to_value(event).expect("event serializes");
    let kind = wire["type"].as_str().unwrap_or_default().to_owned();
    let status = match kind.as_str() {
        "turn_item.updated" => wire["turn_item"]["status"].as_str(),
        "subagent.updated" => wire["subagent"]["status"].as_str(),
        "turn.terminal" => wire["status"].as_str(),
        _ => None,
    };
    match kind.as_str() {
        "app_thread.created" => {
            return format!(
                "app_thread:{}",
                wire["app_thread"]["id"].as_str().unwrap_or_default()
            );
        }
        "message.updated" => {
            return format!(
                "message:{}",
                wire["message"]["threadId"].as_str().unwrap_or_default()
            );
        }
        _ => {}
    }
    let kind = kind.trim_end_matches(".updated").replace('.', "_");
    match status {
        Some(status) => format!("{kind}:{status}"),
        None => kind,
    }
}

fn settles_run(events: &[OrchestrationV2DomainEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, OrchestrationV2DomainEvent::RunUpdated { .. }))
}

fn observe_runs(state: &mut RecorderState, events: &[OrchestrationV2DomainEvent]) {
    for event in events {
        if let OrchestrationV2DomainEvent::RunUpdated { payload, .. } = event {
            let status = serde_json::to_value(payload.status).expect("status");
            state
                .observed
                .push(format!("run:{}", status.as_str().unwrap_or_default()));
        }
    }
}

impl Recorder {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Every domain event that was committed, in order, whichever write path carried it.
    pub fn events(&self) -> Vec<OrchestrationV2DomainEvent> {
        self.state.lock().committed.clone()
    }

    fn notify(&self) {
        self.changed.notify_waiters();
    }
}

#[async_trait]
impl RunEventWriter for Recorder {
    async fn write(&self, input: WriteInput) -> Result<(), BoxError> {
        {
            let mut state = self.state.lock();
            if settles_run(&input.events) {
                state.log.push("root-finalized".to_owned());
            }
            observe_runs(&mut state, &input.events);
            let events = input.events.clone();
            state.committed.extend(events);
            state.writes.push(input);
        }
        self.notify();
        Ok(())
    }

    async fn write_if_run_current(&self, input: RunCurrentWrite) -> Result<bool, BoxError> {
        let committed = {
            let mut state = self.state.lock();
            let rejected = state.reject_guarded;
            if !rejected && settles_run(&input.write.events) {
                state.log.push("root-finalized".to_owned());
            }
            if !rejected {
                observe_runs(&mut state, &input.write.events);
                let events = input.write.events.clone();
                state.committed.extend(events);
            }
            state.guarded.push(input);
            !rejected
        };
        self.notify();
        Ok(committed)
    }
}

#[async_trait]
impl RunEventIngest for Recorder {
    async fn ingest_normalized(
        &self,
        input: &ProviderEventInput,
        guard: &IngestGuard,
    ) -> Result<usize, BoxError> {
        let stored = {
            let mut state = self.state.lock();
            state.ingested.push((input.clone(), guard.clone()));
            state.log.push(event_label(&input.event));
            let hooked = state.ingest_hook.as_mut().map(|hook| hook(input, guard));
            hooked.unwrap_or_else(|| state.stored_per_ingest.pop_front().unwrap_or(1))
        };
        self.notify();
        Ok(stored)
    }
}

#[async_trait]
impl RunBaselineCapture for Recorder {
    async fn capture_baseline(
        &self,
        _scope: &turin_contracts::orchestration_v2::OrchestrationV2CheckpointScope,
        ordinal_within_scope: u64,
    ) -> Result<(), BoxError> {
        let fails = {
            let mut state = self.state.lock();
            state.baseline_calls.push(ordinal_within_scope);
            state.baseline_fails
        };
        if fails {
            return Err("VCS process timed out".into());
        }
        Ok(())
    }
}

#[async_trait]
impl AfterTurnRefresh for Recorder {
    async fn refresh_after_turn(
        &self,
        _project_id: &turin_contracts::ProjectId,
    ) -> Result<(), BoxError> {
        let fails = {
            let mut state = self.state.lock();
            state.refreshes += 1;
            state.observed.push("pull-requests-refreshed".to_owned());
            state.refresh_fails
        };
        self.notify();
        if fails {
            return Err("refresh failed".into());
        }
        Ok(())
    }
}

pub(crate) struct FailingSettings;

#[async_trait]
impl ServerSettingsSource for FailingSettings {
    async fn get_settings(&self) -> Result<turin_contracts::settings::ServerSettings, BoxError> {
        Err("settings read failed".into())
    }
}

pub(crate) fn service(
    recorder: &Arc<Recorder>,
    settings: Arc<dyn ServerSettingsSource>,
) -> RunExecutionService {
    RunExecutionService::new(
        recorder.clone(),
        recorder.clone(),
        recorder.clone(),
        settings,
        recorder.clone(),
    )
}

pub(crate) fn default_service(recorder: &Arc<Recorder>) -> RunExecutionService {
    service(recorder, StaticSettings::default_settings())
}

/// A session whose provider calls are recorded and whose events the test feeds.
pub(crate) struct ScriptedSession {
    session: OrchestrationV2ProviderSession,
    session_id: ProviderSessionId,
    pub starts: Mutex<Vec<ProviderAdapterV2TurnInput>>,
    /// Scripted answers of `ensure_thread`, `resume_thread` and `fork_thread`,
    /// front first. An empty script panics: the call was not expected.
    pub ensure_results:
        Mutex<VecDeque<Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error>>>,
    pub resume_results:
        Mutex<VecDeque<Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error>>>,
    pub fork_results:
        Mutex<VecDeque<Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error>>>,
    pub ensure_inputs: Mutex<Vec<ProviderAdapterV2EnsureThreadInput>>,
    pub history_injections: Mutex<Vec<(usize, String)>>,
    /// `inject_history` answers, front first. `None` in the script means the
    /// runtime has no such hook.
    pub inject_answers: Mutex<Option<VecDeque<bool>>>,
    pub compacts: Mutex<Vec<ProviderAdapterV2TurnInput>>,
    pub start_error: Mutex<Option<ProviderAdapterV2Error>>,
    pub supports_compact: AtomicBool,
    /// Answers of the thread-scoped background probe, front first. Empty
    /// means "no pending work".
    pub pending_background: Mutex<VecDeque<bool>>,
    pub has_background_probe: AtomicBool,
    /// Decides the thread-scoped probe by provider thread (overrides
    /// `pending_background`).
    #[allow(clippy::type_complexity)]
    pub probe_by_thread: Mutex<Option<Box<dyn Fn(&OrchestrationV2ProviderThread) -> bool + Send>>>,
    /// The provider threads the thread-scoped probe was asked about.
    pub probed: Mutex<Vec<turin_contracts::ProviderThreadId>>,
    events: Mutex<Option<ProviderEventStream>>,
    /// Whether `subscribe_events` is offered (manager-owned runtimes).
    pub subscribable: AtomicBool,
    pub closed: Arc<ClosedSignal>,
    /// Runs when the event subscription opens, before the caller reads on.
    #[allow(clippy::type_complexity)]
    pub on_subscribe: Mutex<Option<Box<dyn Fn() + Send>>>,
}

/// The subscription's `close`, observable from a test.
#[derive(Default)]
pub(crate) struct ClosedSignal {
    closed: AtomicBool,
    changed: Notify,
}

impl ClosedSignal {
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.changed.notify_waiters();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Resolves when the subscription is closed, failing the test after ten seconds.
    pub async fn wait(&self) {
        let wait = async {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self.is_closed() {
                    return;
                }
                changed.await;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), wait)
            .await
            .expect("the event subscription was never closed");
    }
}

impl ScriptedSession {
    /// A session that never emits an event.
    pub fn quiet() -> Arc<Self> {
        Self::with_events(Box::pin(futures::stream::pending()))
    }

    /// A subscribable session that plays `events` in order, then ends the
    /// stream, or leaves it open when `keep_open`.
    pub fn scripted(events: Vec<ProviderAdapterV2Event>, keep_open: bool) -> Arc<Self> {
        let played = futures::stream::iter(events.into_iter().map(Ok));
        let stream: ProviderEventStream = if keep_open {
            Box::pin(played.chain(futures::stream::pending()))
        } else {
            Box::pin(played)
        };
        let session = Self::with_events(stream);
        session.subscribable.store(true, Ordering::SeqCst);
        session
    }

    /// A session whose event stream fails at once, as a provider process
    /// that exited does.
    pub fn erroring(error: ProviderAdapterV2Error) -> Arc<Self> {
        let session = Self::with_events(Box::pin(futures::stream::iter([Err(error)])));
        session.subscribable.store(true, Ordering::SeqCst);
        session
    }

    fn with_events(events: ProviderEventStream) -> Arc<Self> {
        Arc::new(Self {
            session: serde_json::from_value(json!({
                "id": "session:scripted", "driver": "codex", "providerInstanceId": "codex",
                "status": "running", "cwd": "/workspace", "model": "gpt-5.4",
                "capabilities": crate::projection::test_support::capabilities(),
                "createdAt": NOW, "updatedAt": NOW, "lastError": null,
            }))
            .expect("session fixture"),
            session_id: ProviderSessionId::new("session:scripted").expect("session id"),
            starts: Mutex::default(),
            ensure_results: Mutex::default(),
            resume_results: Mutex::default(),
            fork_results: Mutex::default(),
            ensure_inputs: Mutex::default(),
            history_injections: Mutex::default(),
            inject_answers: Mutex::default(),
            compacts: Mutex::default(),
            start_error: Mutex::default(),
            supports_compact: AtomicBool::new(true),
            pending_background: Mutex::default(),
            has_background_probe: AtomicBool::new(false),
            probe_by_thread: Mutex::new(None),
            probed: Mutex::default(),
            events: Mutex::new(Some(events)),
            subscribable: AtomicBool::new(false),
            closed: Arc::new(ClosedSignal::default()),
            on_subscribe: Mutex::new(None),
        })
    }
}

fn unused() -> ! {
    unreachable!("RunExecutionService only starts turns on this fake")
}

#[async_trait]
impl ProviderSessionRuntime for ScriptedSession {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.session.provider_instance_id
    }
    fn driver(&self) -> &ProviderDriverKind {
        &self.session.driver
    }
    fn provider_session_id(&self) -> &ProviderSessionId {
        &self.session_id
    }
    fn provider_session(&self) -> &OrchestrationV2ProviderSession {
        &self.session
    }
    fn events(&self) -> ProviderEventStream {
        self.events
            .lock()
            .take()
            .unwrap_or_else(|| Box::pin(futures::stream::empty()))
    }
    fn subscribe_events(
        &self,
    ) -> Option<
        BoxFuture<
            '_,
            Result<
                turin_provider_core::provider_adapter::ProviderAdapterV2EventSubscription,
                ProviderAdapterV2Error,
            >,
        >,
    > {
        if !self.subscribable.load(Ordering::SeqCst) {
            return None;
        }
        if let Some(hook) = self.on_subscribe.lock().as_ref() {
            hook();
        }
        let events = self.events();
        let closed = self.closed.clone();
        Some(Box::pin(async move {
            Ok(
                turin_provider_core::provider_adapter::ProviderAdapterV2EventSubscription {
                    events,
                    close: Box::pin(async move { closed.close() }),
                },
            )
        }))
    }
    fn has_pending_background_work_for_thread(
        &self,
        provider_thread: &OrchestrationV2ProviderThread,
    ) -> Option<BoxFuture<'_, Result<bool, ProviderAdapterV2Error>>> {
        if !self.has_background_probe.load(Ordering::SeqCst) {
            return None;
        }
        self.probed.lock().push(provider_thread.id.clone());
        let answer = match self.probe_by_thread.lock().as_ref() {
            Some(decide) => decide(provider_thread),
            None => self.pending_background.lock().pop_front().unwrap_or(false),
        };
        Some(Box::pin(async move { Ok(answer) }))
    }
    async fn ensure_thread(
        &self,
        input: ProviderAdapterV2EnsureThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        self.ensure_inputs.lock().push(input);
        self.ensure_results
            .lock()
            .pop_front()
            .expect("ensure_thread was not expected")
    }
    async fn resume_thread(
        &self,
        _input: ProviderAdapterV2ResumeThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        self.resume_results
            .lock()
            .pop_front()
            .expect("resume_thread was not expected")
    }
    fn inject_history(
        &self,
        _provider_thread: &OrchestrationV2ProviderThread,
        context: &turin_provider_core::provider_adapter::ProviderAdapterV2HistoricalContext,
    ) -> Option<BoxFuture<'_, Result<bool, ProviderAdapterV2Error>>> {
        let mut answers = self.inject_answers.lock();
        let answers = answers.as_mut()?;
        // An empty probe context is the hook-presence check, not a delivery.
        if context.messages.is_empty() && context.context.is_empty() {
            return Some(Box::pin(async { Ok(false) }));
        }
        self.history_injections
            .lock()
            .push((context.messages.len(), context.context.clone()));
        let answer = answers.pop_front().unwrap_or(true);
        Some(Box::pin(async move { Ok(answer) }))
    }
    async fn start_turn(
        &self,
        input: ProviderAdapterV2TurnInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        self.starts.lock().push(input);
        match self.start_error.lock().take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    fn compact_thread(
        &self,
        input: ProviderAdapterV2TurnInput,
    ) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        if !self.supports_compact.load(Ordering::SeqCst) {
            return None;
        }
        self.compacts.lock().push(input);
        Some(Box::pin(async { Ok(()) }))
    }
    async fn steer_turn(
        &self,
        _input: ProviderAdapterV2SteerInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        unused()
    }
    async fn interrupt_turn(
        &self,
        _input: ProviderAdapterV2InterruptInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        unused()
    }
    async fn respond_to_runtime_request(
        &self,
        _input: ProviderAdapterV2RuntimeRequestResponseInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        unused()
    }
    async fn read_thread_snapshot(
        &self,
        _input: ProviderAdapterV2ReadThreadSnapshotInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        unused()
    }
    async fn rollback_thread(
        &self,
        _input: ProviderAdapterV2RollbackThreadInput,
    ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
        unused()
    }
    async fn fork_thread(
        &self,
        _input: ProviderAdapterV2ForkThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        self.fork_results
            .lock()
            .pop_front()
            .expect("fork_thread was not expected")
    }
}

/// A provider-side turn start failure, as an adapter reports it.
pub(crate) fn turn_start_error(key: &str) -> ProviderAdapterV2Error {
    ProviderAdapterV2Error::new(ProviderAdapterErrorKind::TurnStart {
        driver: ProviderDriverKind::new("codex").expect("driver"),
        thread_id: turin_contracts::ThreadId::new(format!("thread:{key}")).expect("thread id"),
        provider_thread_id: turin_contracts::ProviderThreadId::new(format!(
            "provider-thread:{key}"
        ))
        .expect("provider thread id"),
        run_id: turin_contracts::RunId::new(format!("run:{key}")).expect("run id"),
    })
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("fixture decodes")
}

/// The JSON fixtures behind `StartRootRunInput`, keyed by test name so ids stay unique.
pub(crate) struct Fixture {
    pub key: String,
}

impl Fixture {
    pub fn new(key: &str) -> Self {
        Self {
            key: key.to_owned(),
        }
    }

    pub fn thread_id(&self) -> String {
        format!("thread:{}", self.key)
    }
    pub fn run_id(&self) -> String {
        format!("run:{}", self.key)
    }
    pub fn attempt_id(&self) -> String {
        format!("attempt:{}", self.key)
    }
    pub fn provider_thread_id(&self) -> String {
        format!("provider-thread:{}", self.key)
    }
    pub fn node_id(&self) -> String {
        format!("node:{}", self.key)
    }

    pub fn run_json(&self) -> Value {
        json!({
            "id": self.run_id(), "threadId": self.thread_id(), "ordinal": 1,
            "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "gpt-5.4"},
            "providerThreadId": self.provider_thread_id(),
            "userMessageId": format!("message:{}", self.key),
            "rootNodeId": self.node_id(), "activeAttemptId": self.attempt_id(),
            "status": "running", "requestedAt": NOW, "startedAt": NOW, "completedAt": null,
            "checkpointId": null, "contextHandoffId": null,
        })
    }

    pub fn node_json(&self) -> Value {
        json!({
            "id": self.node_id(), "threadId": self.thread_id(), "runId": self.run_id(),
            "parentNodeId": null, "rootNodeId": self.node_id(), "kind": "root_turn",
            "status": "running", "countsForRun": true,
            "providerThreadId": self.provider_thread_id(), "providerTurnId": null,
            "nativeItemRef": null, "runtimeRequestId": null, "checkpointScopeId": null,
            "startedAt": NOW, "completedAt": null,
        })
    }

    pub fn attempt_json(&self) -> Value {
        json!({
            "id": self.attempt_id(), "runId": self.run_id(), "attemptOrdinal": 1,
            "rootNodeId": self.node_id(), "providerInstanceId": "codex",
            "providerThreadId": self.provider_thread_id(), "providerTurnId": null,
            "reason": "initial", "status": "running", "startedAt": NOW, "completedAt": null,
        })
    }

    pub fn provider_thread_json(&self) -> Value {
        json!({
            "id": self.provider_thread_id(), "driver": "codex", "providerInstanceId": "codex",
            "providerSessionId": "session:scripted", "appThreadId": self.thread_id(),
            "ownerNodeId": null, "nativeThreadRef": null, "nativeConversationHeadRef": null,
            "status": "active", "firstRunOrdinal": 1, "lastRunOrdinal": 1, "handoffIds": [],
            "forkedFrom": null, "createdAt": NOW, "updatedAt": NOW,
        })
    }

    pub fn scope_json(&self) -> Value {
        json!({
            "id": format!("checkpoint-scope:{}", self.key), "threadId": self.thread_id(),
            "runId": self.run_id(), "nodeId": self.node_id(), "parentScopeId": null,
            "providerThreadId": self.provider_thread_id(), "kind": "root_run",
            "ordinalWithinParent": 0, "advancesAppRunCount": true, "cwd": "/workspace",
            "createdAt": NOW,
        })
    }

    /// The input with the optional hooks off. Tests set the fields they need.
    pub fn input(&self, session: Arc<ScriptedSession>, text: &str) -> StartRootRunInput {
        StartRootRunInput {
            command_id: turin_contracts::CommandId::new(format!("command:{}", self.key))
                .expect("command id"),
            app_thread: decode(crate::projection::test_support::thread(&self.thread_id())),
            provider_session_id: ProviderSessionId::new("session:scripted").expect("session id"),
            session,
            run: decode(self.run_json()),
            root_node: decode(self.node_json()),
            checkpoint_scope: decode(self.scope_json()),
            provider_thread: decode(self.provider_thread_json()),
            attempt: decode(self.attempt_json()),
            attempt_id: turin_contracts::RunAttemptId::new(self.attempt_id()).expect("attempt id"),
            provider_turn_ordinal: 1,
            native_thread_has_turns: None,
            load_inherited_background_turn_items: None,
            related_thread_ids: Vec::new(),
            related_provider_thread_ids: Vec::new(),
            should_start_provider_turn: None,
            should_finalize_run: None,
            has_unpaired_run_interrupt_request: None,
            message: decode(json!({
                "messageId": format!("message:{}", self.key), "text": text, "attachments": [],
                "createdBy": "user", "creationSource": "web",
            })),
            model_selection: decode(json!({"instanceId": "codex", "model": "gpt-5.4"})),
            runtime_policy: decode(json!({
                "runtimeMode": "full-access", "interactionMode": "default", "cwd": "/workspace",
                "approvalPolicy": "never",
                "sandboxPolicy": {"type": "readOnly", "access": {"type": "fullAccess"}, "networkAccess": false},
            })),
        }
    }
}

/// An [`AsyncBool`](super::AsyncBool) that answers from a script, last answer repeating.
pub(crate) fn scripted_bool(
    script: Vec<Result<bool, &'static str>>,
) -> (super::AsyncBool, Arc<Mutex<usize>>) {
    let calls = Arc::new(Mutex::new(0usize));
    let counter = calls.clone();
    let probe: super::AsyncBool = Arc::new(move || {
        let index = {
            let mut calls = counter.lock();
            let index = *calls;
            *calls += 1;
            index
        };
        let answer = script
            .get(index)
            .or_else(|| script.last())
            .copied()
            .unwrap_or(Ok(true));
        Box::pin(async move { answer.map_err(BoxError::from) })
    });
    (probe, calls)
}

/// Typed adapter events from compact JSON, standing in for the upstream
/// `as ProviderAdapterV2Event` literals.
pub(crate) mod ev {
    use serde_json::{Value, json};
    use turin_provider_core::provider_adapter::ProviderAdapterV2Event;

    use super::{Fixture, NOW};

    fn event(value: Value) -> ProviderAdapterV2Event {
        serde_json::from_value(value).expect("adapter event fixture decodes")
    }

    pub fn app_thread_created(
        id: &str,
        parent: Option<&str>,
        root: &str,
    ) -> ProviderAdapterV2Event {
        let mut thread = crate::projection::test_support::thread(id);
        thread["lineage"] = json!({
            "parentThreadId": parent, "relationshipToParent": parent.map(|_| "subagent"),
            "rootThreadId": root,
        });
        event(json!({"type": "app_thread.created", "driver": "codex", "app_thread": thread}))
    }

    pub fn provider_thread(
        fixture: &Fixture,
        id: &str,
        app_thread_id: &str,
        status: &str,
        pending: Value,
    ) -> ProviderAdapterV2Event {
        let mut thread = fixture.provider_thread_json();
        thread["id"] = json!(id);
        thread["appThreadId"] = json!(app_thread_id);
        thread["status"] = json!(status);
        thread["pendingBackgroundTasks"] = pending;
        event(
            json!({"type": "provider_thread.updated", "driver": "codex", "provider_thread": thread}),
        )
    }

    pub fn provider_turn(
        id: &str,
        provider_thread_id: &str,
        node_id: &str,
        thread_id: Option<&str>,
        status: &str,
    ) -> ProviderAdapterV2Event {
        event(json!({
            "type": "provider_turn.updated", "driver": "codex", "thread_id": thread_id,
            "provider_turn": {
                "id": id, "providerThreadId": provider_thread_id, "nodeId": node_id,
                "runAttemptId": null, "nativeTurnRef": null, "ordinal": 1, "status": status,
                "startedAt": NOW, "completedAt": null,
            },
        }))
    }

    pub fn terminal(
        provider_thread_id: &str,
        provider_turn_id: &str,
        status: &str,
    ) -> ProviderAdapterV2Event {
        event(json!({
            "type": "turn.terminal", "driver": "codex",
            "provider_thread_id": provider_thread_id, "provider_turn_id": provider_turn_id,
            "run_ordinal": 1, "status": status, "failure": null,
            "thread_disposition": "reusable",
        }))
    }

    pub fn subagent_json(
        id: &str,
        thread_id: &str,
        run_id: Option<&str>,
        origin: &str,
        child_thread_id: Option<&str>,
        status: &str,
    ) -> Value {
        json!({
            "id": id, "threadId": thread_id, "runId": run_id, "parentNodeId": "node:parent",
            "origin": origin, "createdBy": "agent", "driver": "codex",
            "providerInstanceId": "codex", "providerThreadId": null,
            "childThreadId": child_thread_id, "nativeTaskRef": null, "prompt": "hold",
            "title": "Worker", "model": null, "status": status, "progress": null,
            "result": null, "startedAt": NOW, "completedAt": null, "updatedAt": NOW,
        })
    }

    pub fn subagent(subagent: Value) -> ProviderAdapterV2Event {
        event(json!({"type": "subagent.updated", "driver": "codex", "subagent": subagent}))
    }

    pub fn node(
        id: &str,
        thread_id: &str,
        run_id: Option<&str>,
        kind: &str,
        status: &str,
    ) -> ProviderAdapterV2Event {
        event(json!({
            "type": "node.updated", "driver": "codex",
            "node": {
                "id": id, "threadId": thread_id, "runId": run_id, "parentNodeId": null,
                "rootNodeId": id, "kind": kind, "status": status, "countsForRun": false,
                "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
                "runtimeRequestId": null, "checkpointScopeId": null,
                "startedAt": NOW, "completedAt": null,
            },
        }))
    }

    pub fn message(id: &str, thread_id: &str, node_id: &str, text: &str) -> ProviderAdapterV2Event {
        let mut message =
            crate::projection::test_support::message(thread_id, id, None, "assistant", text, NOW);
        message["nodeId"] = json!(node_id);
        event(json!({"type": "message.updated", "driver": "codex", "message": message}))
    }

    /// A turn item of the background-capable kinds. `extra` is merged in.
    #[allow(clippy::too_many_arguments)]
    pub fn turn_item(
        thread_id: &str,
        id: &str,
        run_id: Option<&str>,
        provider_turn_id: Option<&str>,
        ordinal: u64,
        kind: &str,
        status: &str,
    ) -> ProviderAdapterV2Event {
        let extra = match kind {
            "command_execution" => json!({"input": "sleep 60"}),
            "dynamic_tool" => json!({"toolName": "monitor", "input": {}}),
            "subagent" => json!({
                "subagentId": "node:bg:subagent", "origin": "provider_native", "driver": "codex",
                "providerInstanceId": "codex", "childThreadId": null, "prompt": "hold",
                "progress": null, "result": null,
            }),
            "assistant_message" => {
                json!({"messageId": format!("message:{id}"), "text": "", "streaming": false})
            }
            "reasoning" => json!({"text": "", "streaming": false}),
            other => panic!("no fixture for turn item kind {other}"),
        };
        let mut item = crate::projection::test_support::turn_item(
            thread_id, id, run_id, ordinal, kind, status, extra,
        );
        item["providerTurnId"] = json!(provider_turn_id);
        event(json!({"type": "turn_item.updated", "driver": "codex", "turn_item": item}))
    }
}
