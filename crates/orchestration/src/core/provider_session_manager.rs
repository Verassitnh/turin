//! Port of upstream/apps/server/src/orchestration-v2/ProviderSessionManager.ts.
//!
//! Owns live session residency: open or reuse one provider session per
//! `ProviderSessionId`, attach app threads to it, fan provider events out to
//! run subscribers, track busy turns, and release sessions on idle timeout,
//! runtime failure, detach, or shutdown. It never resurrects persisted
//! sessions; a later command opens one lazily.
//!
//! Persistence goes through [`ProviderSessionSink`]. The orchestrator
//! implements it over `EventSink`, and tests use a recording fake.
//!
//! Not ported here (see docs/handoff/T13c-needs.md): MCP credential issuance,
//! agent access settings, background-task roster cleanup on release, shared
//! runtime idle thread unload, interrupting running turns on detach (needs
//! projection reads), and retried release-record writes.

use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures::{Stream, StreamExt, future::join_all};
use indexmap::IndexSet;
use parking_lot::Mutex;
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio_util::task::AbortOnDropHandle;
use tracing::{error, warn};
use turin_contracts::{
    ProviderSessionId, ProviderThreadId, ThreadId,
    orchestration_v2::{OrchestrationV2ProviderSession, OrchestrationV2ProviderSessionStatus},
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
};
use turin_provider_core::{
    failure::ProviderFailureCause,
    provider_adapter::{
        ProviderAdapterErrorKind, ProviderAdapterV2, ProviderAdapterV2Error,
        ProviderAdapterV2Event, ProviderAdapterV2EventSubscription,
        ProviderAdapterV2OpenSessionInput, ProviderAdapterV2TurnInput, ProviderEventStream,
        ProviderSessionRuntime,
    },
    provider_errors::{ProviderInstanceNotFoundError, ProviderWorkspaceMissingError},
    provider_instance_registry::ProviderInstanceRegistry,
    rt::BoxFuture,
};

use crate::pipeline::orchestrator_port::BoxError;

const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const DEFAULT_MAX_IDLE_PIN: Duration = Duration::from_secs(4 * 60 * 60);

/// `ProviderSessionReleaseReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderSessionReleaseReason {
    IdleTimeout,
    RuntimeError,
    ManualShutdown,
    ServerShutdown,
}

impl ProviderSessionReleaseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IdleTimeout => "idle_timeout",
            Self::RuntimeError => "runtime_error",
            Self::ManualShutdown => "manual_shutdown",
            Self::ServerShutdown => "server_shutdown",
        }
    }
}

/// `releaseStatusFor`: the persisted session status a release leaves behind.
pub fn release_status_for(
    reason: ProviderSessionReleaseReason,
) -> OrchestrationV2ProviderSessionStatus {
    match reason {
        ProviderSessionReleaseReason::RuntimeError => OrchestrationV2ProviderSessionStatus::Error,
        _ => OrchestrationV2ProviderSessionStatus::Stopped,
    }
}

/// `ProviderSessionManagerV2LayerOptions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderSessionManagerOptions {
    pub idle_timeout: Duration,
    /// Cap on how long idle release may be deferred for pending background work.
    pub max_idle_pin: Duration,
}

impl Default for ProviderSessionManagerOptions {
    fn default() -> Self {
        Self {
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_idle_pin: DEFAULT_MAX_IDLE_PIN,
        }
    }
}

/// `ProviderSessionManagerV2Error`.
#[derive(Debug, thiserror::Error)]
pub enum ProviderSessionManagerError {
    #[error(transparent)]
    WorkspaceMissing(#[from] ProviderWorkspaceMissingError),
    #[error("Failed to open provider instance {instance_id} session {provider_session_id}.")]
    Open {
        instance_id: ProviderInstanceId,
        provider_session_id: ProviderSessionId,
        #[source]
        cause: BoxError,
    },
    #[error("Failed to release provider session {provider_session_id}.")]
    Release {
        provider_session_id: ProviderSessionId,
        #[source]
        cause: BoxError,
    },
}

/// The identity a sink writes rows under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSessionIdentity {
    pub provider_session_id: ProviderSessionId,
    pub instance_id: ProviderInstanceId,
    pub driver: ProviderDriverKind,
}

/// What a released session leaves behind for the sink to record.
#[derive(Debug, Clone)]
pub struct SessionRelease {
    pub identity: ProviderSessionIdentity,
    pub reason: ProviderSessionReleaseReason,
    pub detail: Option<String>,
    /// Threads still attached at release. Their live runtime requests are
    /// settled; requests created after `released_at` belong to a replacement.
    pub thread_ids: Vec<ThreadId>,
    /// Last session snapshot the runtime reported.
    pub last_session: OrchestrationV2ProviderSession,
    pub released_at: chrono::DateTime<chrono::Utc>,
}

/// The writes the session manager makes. `EventSink` backs it in production.
#[async_trait]
pub trait ProviderSessionSink: Send + Sync {
    /// `provider-session.attached` for a thread that joined the session.
    async fn session_attached(
        &self,
        identity: &ProviderSessionIdentity,
        thread_ids: &[ThreadId],
        provider_session: &OrchestrationV2ProviderSession,
    ) -> Result<(), BoxError>;

    /// `provider-session.updated` for each attached thread.
    async fn session_updated(
        &self,
        identity: &ProviderSessionIdentity,
        thread_ids: &[ThreadId],
        provider_session: &OrchestrationV2ProviderSession,
    ) -> Result<(), BoxError>;

    /// `writeReleaseRecords`: stops the session row (status from
    /// [`release_status_for`]) and resolves its runtime requests.
    async fn session_released(&self, release: &SessionRelease) -> Result<(), BoxError>;

    /// Runless runtime requests, nodes and turn items have no run subscriber,
    /// so the ingestor persists them directly. Some providers block on these
    /// (trust, login, session-switch hooks) before any run exists.
    async fn ingest_session_scoped_event(
        &self,
        identity: &ProviderSessionIdentity,
        thread_id: ThreadId,
        event: ProviderAdapterV2Event,
    ) -> Result<(), BoxError>;
}

/// Resolves an instance id to its orchestration adapter. The registry is the
/// production implementation; tests supply a fake.
pub trait ProviderAdapterLookup: Send + Sync {
    fn adapter(&self, instance_id: &ProviderInstanceId) -> Option<Arc<dyn ProviderAdapterV2>>;
}

impl ProviderAdapterLookup for ProviderInstanceRegistry {
    fn adapter(&self, instance_id: &ProviderInstanceId) -> Option<Arc<dyn ProviderAdapterV2>> {
        self.get_instance(instance_id)
            .map(|instance| Arc::clone(&instance.orchestration_adapter))
    }
}

type SubscriberTx = mpsc::UnboundedSender<Result<ProviderAdapterV2Event, ProviderAdapterV2Error>>;
type SubscriberRx = mpsc::UnboundedReceiver<Result<ProviderAdapterV2Event, ProviderAdapterV2Error>>;

/// `busyTurnKey`: a turn's start and its `turn.terminal` share this identity.
type BusyTurnKey = (ProviderThreadId, i64);

struct SessionState {
    attached_thread_ids: IndexSet<ThreadId>,
    busy_turns: HashSet<BusyTurnKey>,
    idle_generation: u64,
    /// Aborted (dropped) whenever activity reschedules the idle release.
    idle_task: Option<AbortOnDropHandle<()>>,
    /// Consumes the runtime's single event stream. Aborted on release.
    pump: Option<AbortOnDropHandle<()>>,
    last_activity: Instant,
    pinned_since: Option<Instant>,
    subscribers: HashMap<u64, SubscriberTx>,
    next_subscriber_id: u64,
}

struct LiveSession {
    provider_session_id: ProviderSessionId,
    runtime: Arc<dyn ProviderSessionRuntime>,
    supports_multiple_provider_threads: bool,
    state: Mutex<SessionState>,
}

impl LiveSession {
    fn new(
        runtime: Arc<dyn ProviderSessionRuntime>,
        supports_multiple_provider_threads: bool,
        first_thread_id: ThreadId,
    ) -> Arc<Self> {
        let mut attached_thread_ids = IndexSet::new();
        attached_thread_ids.insert(first_thread_id);
        Arc::new(Self {
            provider_session_id: runtime.provider_session_id().clone(),
            runtime,
            supports_multiple_provider_threads,
            state: Mutex::new(SessionState {
                attached_thread_ids,
                busy_turns: HashSet::new(),
                idle_generation: 0,
                idle_task: None,
                pump: None,
                last_activity: Instant::now(),
                pinned_since: None,
                subscribers: HashMap::new(),
                next_subscriber_id: 0,
            }),
        })
    }

    fn identity(&self) -> ProviderSessionIdentity {
        ProviderSessionIdentity {
            provider_session_id: self.provider_session_id.clone(),
            instance_id: self.runtime.instance_id().clone(),
            driver: self.runtime.driver().clone(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReleaseOrigin {
    Caller,
    /// The idle timer task is the one releasing; it must not abort itself.
    IdleTimer,
    /// The event pump is the one releasing; it must not abort itself.
    EventPump,
}

struct ReleaseRequest {
    provider_session_id: ProviderSessionId,
    reason: ProviderSessionReleaseReason,
    detail: Option<String>,
    /// Subscribers drain already-published events, then end.
    graceful_subscribers: bool,
    only_if_idle_generation: Option<u64>,
    origin: ReleaseOrigin,
}

struct Inner {
    lookup: Arc<dyn ProviderAdapterLookup>,
    sink: Arc<dyn ProviderSessionSink>,
    options: ProviderSessionManagerOptions,
    sessions: Mutex<HashMap<ProviderSessionId, Arc<LiveSession>>>,
    open_locks: Mutex<HashMap<ProviderSessionId, Arc<AsyncMutex<()>>>>,
    shutting_down: AtomicBool,
}

impl Inner {
    fn get_session(&self, provider_session_id: &ProviderSessionId) -> Option<Arc<LiveSession>> {
        self.sessions.lock().get(provider_session_id).cloned()
    }

    /// Whether `session` is still the live entry for its id (`current?.runtime === entry.runtime`).
    fn is_current(&self, session: &Arc<LiveSession>) -> bool {
        self.sessions
            .lock()
            .get(&session.provider_session_id)
            .is_some_and(|current| Arc::ptr_eq(current, session))
    }

    /// `touchActivity`. `provider_activity` clears the pin that deferred idle release.
    fn touch(self: &Arc<Self>, session: &Arc<LiveSession>, provider_activity: bool) {
        let mut state = session.state.lock();
        state.last_activity = Instant::now();
        self.reschedule_idle_release(session, &mut state, provider_activity);
    }

    /// `scheduleIdleReleaseInternal`: restarts the idle timer unless turns are busy.
    fn reschedule_idle_release(
        self: &Arc<Self>,
        session: &Arc<LiveSession>,
        state: &mut SessionState,
        provider_activity: bool,
    ) {
        if !state.busy_turns.is_empty() {
            return;
        }
        if provider_activity {
            state.pinned_since = None;
        }
        state.idle_generation += 1;
        let generation = state.idle_generation;
        // Dropping the previous handle aborts that timer.
        state.idle_task = None;
        let weak_inner = Arc::downgrade(self);
        let weak_session = Arc::downgrade(session);
        let idle_timeout = self.options.idle_timeout;
        let task = tokio::spawn(async move {
            tokio::time::sleep(idle_timeout).await;
            release_if_still_idle(weak_inner, weak_session, generation).await;
        });
        state.idle_task = Some(AbortOnDropHandle::new(task));
    }

    fn schedule_idle_release(self: &Arc<Self>, session: &Arc<LiveSession>) {
        let mut state = session.state.lock();
        self.reschedule_idle_release(session, &mut state, false);
    }

    /// `markBusy`: a turn started, so idle release cannot fire.
    fn mark_busy(&self, session: &Arc<LiveSession>, key: BusyTurnKey) {
        let mut state = session.state.lock();
        state.busy_turns.insert(key);
        state.idle_task = None;
        state.pinned_since = None;
        state.last_activity = Instant::now();
    }

    /// `markIdle`: a turn reached its terminal state or failed to start.
    fn mark_idle(self: &Arc<Self>, session: &Arc<LiveSession>, key: &BusyTurnKey) {
        {
            let mut state = session.state.lock();
            state.busy_turns.remove(key);
            state.last_activity = Instant::now();
        }
        self.schedule_idle_release(session);
    }

    /// `ensureThreadAttached`. A new thread joins and gets a `provider-session.attached` write.
    async fn attach_thread(
        self: &Arc<Self>,
        session: &Arc<LiveSession>,
        thread_id: &ThreadId,
    ) -> Result<(), BoxError> {
        {
            let mut state = session.state.lock();
            if state.attached_thread_ids.contains(thread_id) {
                return Ok(());
            }
            if !session.supports_multiple_provider_threads && !state.attached_thread_ids.is_empty()
            {
                return Err(format!(
                    "Provider {} does not support attaching multiple app threads to one session.",
                    session.runtime.driver()
                )
                .into());
            }
            state.attached_thread_ids.insert(thread_id.clone());
        }
        let identity = session.identity();
        let thread_ids = std::slice::from_ref(thread_id);
        if let Err(error) = self
            .sink
            .session_attached(&identity, thread_ids, session.runtime.provider_session())
            .await
        {
            session
                .state
                .lock()
                .attached_thread_ids
                .shift_remove(thread_id);
            return Err(error);
        }
        Ok(())
    }

    /// Hands every event to each subscriber's queue. Publishing never waits.
    fn publish(&self, session: &Arc<LiveSession>, event: ProviderAdapterV2Event) {
        let mut state = session.state.lock();
        state
            .subscribers
            .retain(|_, sender| sender.send(Ok(event.clone())).is_ok());
    }

    /// `ProviderAdapterEventStreamError` a subscriber sees when its session is released.
    fn release_error(session: &LiveSession, detail: &str) -> ProviderAdapterV2Error {
        ProviderAdapterV2Error::new(ProviderAdapterErrorKind::EventStream {
            driver: session.runtime.driver().clone(),
            provider_session_id: session.provider_session_id.clone(),
        })
        .with_cause(ProviderFailureCause::tagged("ProviderSessionReleased").with_message(detail))
    }

    /// `removeLiveEntry`: atomically takes the session out of the live map,
    /// unless an idle release was rescheduled in the meantime.
    fn remove_live_session(&self, request: &ReleaseRequest) -> Option<Arc<LiveSession>> {
        let mut sessions = self.sessions.lock();
        let session = sessions.get(&request.provider_session_id)?.clone();
        if let Some(generation) = request.only_if_idle_generation
            && session.state.lock().idle_generation != generation
        {
            return None;
        }
        sessions.remove(&request.provider_session_id);
        Some(session)
    }

    /// `releaseEntry`: removes the session, ends or fails its subscribers,
    /// stops its event pump and idle timer, drops the runtime, and records the release.
    async fn release(
        self: &Arc<Self>,
        request: ReleaseRequest,
    ) -> Result<(), ProviderSessionManagerError> {
        let Some(session) = self.remove_live_session(&request) else {
            return Ok(());
        };
        let (idle_task, pump, subscribers, attached) = {
            let mut state = session.state.lock();
            (
                state.idle_task.take(),
                state.pump.take(),
                std::mem::take(&mut state.subscribers),
                state
                    .attached_thread_ids
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
        // A task releasing itself is detached: it finishes on its own, and an
        // abort here would cut the release short at its next await.
        match (request.origin, idle_task) {
            (ReleaseOrigin::IdleTimer, Some(task)) => drop(task.detach()),
            (_, task) => drop(task),
        }
        match (request.origin, pump) {
            (ReleaseOrigin::EventPump, Some(task)) => drop(task.detach()),
            (_, task) => drop(task),
        }

        let detail = request
            .detail
            .clone()
            .unwrap_or_else(|| format!("Provider session released: {}.", request.reason.as_str()));
        let server_shutdown = request.reason == ProviderSessionReleaseReason::ServerShutdown;
        for sender in subscribers.into_values() {
            if !request.graceful_subscribers && !server_shutdown {
                let error = Self::release_error(&session, &detail);
                let _ = sender.send(Err(error));
            }
            // Dropping the sender ends the subscriber stream.
        }

        let identity = session.identity();
        let release = SessionRelease {
            identity,
            reason: request.reason,
            detail: request.detail,
            thread_ids: attached,
            last_session: session.runtime.provider_session().clone(),
            released_at: chrono::Utc::now(),
        };
        let provider_session_id = request.provider_session_id;
        // Last reference to the session: its runtime drops here, which is
        // where the adapter stops its process.
        drop(session);
        self.sink.session_released(&release).await.map_err(|cause| {
            ProviderSessionManagerError::Release {
                provider_session_id,
                cause,
            }
        })
    }

    async fn persist_session_update(
        self: &Arc<Self>,
        session: &Arc<LiveSession>,
        provider_session: &OrchestrationV2ProviderSession,
    ) {
        if !self.is_current(session) {
            return;
        }
        let thread_ids: Vec<ThreadId> = session
            .state
            .lock()
            .attached_thread_ids
            .iter()
            .cloned()
            .collect();
        if let Err(cause) = self
            .sink
            .session_updated(&session.identity(), &thread_ids, provider_session)
            .await
        {
            warn!(
                provider_session_id = %session.provider_session_id,
                %cause,
                "orchestration-v2.driver-session.status-persist-failed"
            );
        }
    }

    fn open_lock(&self, provider_session_id: &ProviderSessionId) -> Arc<AsyncMutex<()>> {
        Arc::clone(
            self.open_locks
                .lock()
                .entry(provider_session_id.clone())
                .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
        )
    }

    /// Drops the per-id open lock once nobody else holds it.
    fn prune_open_lock(&self, provider_session_id: &ProviderSessionId, lock: &Arc<AsyncMutex<()>>) {
        let mut locks = self.open_locks.lock();
        if let Some(existing) = locks.get(provider_session_id)
            && Arc::ptr_eq(existing, lock)
            && Arc::strong_count(existing) == 2
        {
            locks.remove(provider_session_id);
        }
    }
}

/// `releaseIfStillIdle`, run by the idle timer. Defers release while the
/// runtime reports background work, up to `max_idle_pin`.
async fn release_if_still_idle(
    weak_inner: Weak<Inner>,
    weak_session: Weak<LiveSession>,
    generation: u64,
) {
    loop {
        let (Some(inner), Some(session)) = (weak_inner.upgrade(), weak_session.upgrade()) else {
            return;
        };
        if !inner.is_current(&session) {
            return;
        }
        let still_idle = {
            let state = session.state.lock();
            state.busy_turns.is_empty() && state.idle_generation == generation
        };
        if !still_idle {
            return;
        }
        let pending = match session.runtime.has_pending_background_work() {
            Some(future) => future.await.unwrap_or(false),
            None => false,
        };
        if pending {
            let now = Instant::now();
            let pinned_since = session
                .state
                .lock()
                .pinned_since
                .get_or_insert(now)
                .to_owned();
            let pinned_for = now.duration_since(pinned_since);
            if pinned_for < inner.options.max_idle_pin {
                let idle_timeout = inner.options.idle_timeout;
                // Drop strong references before sleeping so the timer never
                // keeps the session alive.
                drop((inner, session));
                tokio::time::sleep(idle_timeout).await;
                continue;
            }
            warn!(
                provider_session_id = %session.provider_session_id,
                pinned_for_ms = pinned_for.as_millis() as u64,
                "orchestration-v2.driver-session.idle-release-pin-expired"
            );
        }
        let provider_session_id = session.provider_session_id.clone();
        drop(session);
        if let Err(cause) = inner
            .release(ReleaseRequest {
                provider_session_id,
                reason: ProviderSessionReleaseReason::IdleTimeout,
                detail: None,
                graceful_subscribers: false,
                only_if_idle_generation: Some(generation),
                origin: ReleaseOrigin::IdleTimer,
            })
            .await
        {
            warn!(%cause, "orchestration-v2.driver-session.idle-release-failed");
        }
        return;
    }
}

/// `sessionScopedRuntimeRequestThreadId`: runtime requests with no provider
/// turn belong to the live session itself, not to a run subscriber.
fn session_scoped_thread_id(event: &ProviderAdapterV2Event) -> Option<ThreadId> {
    use turin_contracts::orchestration_v2::OrchestrationV2TurnItem;
    match event {
        ProviderAdapterV2Event::RuntimeRequestUpdated {
            thread_id,
            runtime_request,
            ..
        } if runtime_request.provider_turn_id.is_none() => thread_id.clone(),
        ProviderAdapterV2Event::NodeUpdated { node, .. }
            if node.run_id.is_none() && node.runtime_request_id.is_some() =>
        {
            Some(node.thread_id.clone())
        }
        ProviderAdapterV2Event::TurnItemUpdated { turn_item, .. } => match turn_item {
            OrchestrationV2TurnItem::ApprovalRequest {
                thread_id, run_id, ..
            }
            | OrchestrationV2TurnItem::UserInputRequest {
                thread_id, run_id, ..
            } if run_id.is_none() => Some(thread_id.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// `startEventPump`: the one consumer of the runtime's event stream. Routes
/// each event to activity tracking, session persistence, the ingestor, or
/// run subscribers, and releases the session when the stream ends or fails.
fn spawn_event_pump(inner: &Arc<Inner>, session: &Arc<LiveSession>) -> AbortOnDropHandle<()> {
    let mut events: ProviderEventStream = session.runtime.events();
    let weak_inner = Arc::downgrade(inner);
    let weak_session = Arc::downgrade(session);
    let task = tokio::spawn(async move {
        let mut stopped_by_provider = false;
        let end: Result<(), ProviderAdapterV2Error> = loop {
            let item = match events.next().await {
                None => break Ok(()),
                Some(Err(error)) => break Err(error),
                Some(Ok(event)) => event,
            };
            let (Some(inner), Some(session)) = (weak_inner.upgrade(), weak_session.upgrade())
            else {
                return;
            };
            // Shutdown already owns the release; provider events after the
            // signal are dropped, as upstream does.
            if inner.shutting_down.load(Ordering::SeqCst) {
                continue;
            }
            if let ProviderAdapterV2Event::ProviderSessionUpdated {
                provider_session, ..
            } = &item
                && provider_session.status == OrchestrationV2ProviderSessionStatus::Stopped
            {
                stopped_by_provider = true;
            }
            match &item {
                ProviderAdapterV2Event::TurnTerminal {
                    provider_thread_id,
                    run_ordinal,
                    ..
                } => inner.mark_idle(
                    &session,
                    &(provider_thread_id.clone(), run_ordinal.get() as i64),
                ),
                _ => inner.touch(&session, true),
            }
            if let ProviderAdapterV2Event::ProviderSessionUpdated {
                provider_session, ..
            } = &item
            {
                inner
                    .persist_session_update(&session, provider_session)
                    .await;
            }
            if let Some(thread_id) = session_scoped_thread_id(&item) {
                if inner.is_current(&session)
                    && let Err(cause) = inner
                        .sink
                        .ingest_session_scoped_event(&session.identity(), thread_id, item)
                        .await
                {
                    warn!(
                        provider_session_id = %session.provider_session_id,
                        %cause,
                        "orchestration-v2.driver-session.runless-ingest-failed"
                    );
                }
                continue;
            }
            inner.publish(&session, item);
        };

        let (Some(inner), Some(session)) = (weak_inner.upgrade(), weak_session.upgrade()) else {
            return;
        };
        if inner.shutting_down.load(Ordering::SeqCst) || !inner.is_current(&session) {
            return;
        }
        let provider_session_id = session.provider_session_id.clone();
        drop(session);
        let request = match end {
            Ok(()) if stopped_by_provider => ReleaseRequest {
                provider_session_id,
                reason: ProviderSessionReleaseReason::ManualShutdown,
                detail: None,
                graceful_subscribers: true,
                only_if_idle_generation: None,
                origin: ReleaseOrigin::EventPump,
            },
            other => {
                let detail = match other {
                    Err(error) => error.to_string(),
                    Ok(()) => "Provider event stream ended unexpectedly.".to_owned(),
                };
                error!(
                    provider_session_id = %provider_session_id,
                    %detail,
                    "orchestration-v2.driver-session.event-stream-failed"
                );
                ReleaseRequest {
                    provider_session_id,
                    reason: ProviderSessionReleaseReason::RuntimeError,
                    detail: Some(detail),
                    graceful_subscribers: false,
                    only_if_idle_generation: None,
                    origin: ReleaseOrigin::EventPump,
                }
            }
        };
        if let Err(cause) = inner.release(request).await {
            warn!(%cause, "orchestration-v2.driver-session.pump-release-failed");
        }
    });
    AbortOnDropHandle::new(task)
}

/// `makeEventSubscription`: a subscriber's queue. Dropping the stream
/// unregisters it.
fn subscribe(session: &Arc<LiveSession>) -> ProviderAdapterV2EventSubscription {
    let (sender, receiver) = mpsc::unbounded_channel();
    let id = {
        let mut state = session.state.lock();
        let id = state.next_subscriber_id;
        state.next_subscriber_id += 1;
        state.subscribers.insert(id, sender);
        id
    };
    let guard = SubscriberGuard {
        session: Arc::downgrade(session),
        id,
    };
    let close_session = Arc::downgrade(session);
    let close: BoxFuture<'static, ()> = Box::pin(async move {
        if let Some(session) = close_session.upgrade() {
            session.state.lock().subscribers.remove(&id);
        }
    });
    ProviderAdapterV2EventSubscription {
        events: Box::pin(SubscriberStream {
            receiver,
            _guard: guard,
        }),
        close,
    }
}

struct SubscriberGuard {
    session: Weak<LiveSession>,
    id: u64,
}

impl Drop for SubscriberGuard {
    fn drop(&mut self) {
        if let Some(session) = self.session.upgrade() {
            session.state.lock().subscribers.remove(&self.id);
        }
    }
}

struct SubscriberStream {
    receiver: SubscriberRx,
    _guard: SubscriberGuard,
}

impl Stream for SubscriberStream {
    type Item = Result<ProviderAdapterV2Event, ProviderAdapterV2Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}

/// A session as handed to callers: the live runtime plus manager-aware
/// activity tracking (`decorateRuntime`).
struct ManagedRuntime {
    inner: Arc<Inner>,
    session: Arc<LiveSession>,
}

#[async_trait]
impl ProviderSessionRuntime for ManagedRuntime {
    fn instance_id(&self) -> &ProviderInstanceId {
        self.session.runtime.instance_id()
    }

    fn driver(&self) -> &ProviderDriverKind {
        self.session.runtime.driver()
    }

    fn provider_session_id(&self) -> &ProviderSessionId {
        self.session.runtime.provider_session_id()
    }

    fn provider_session(&self) -> &OrchestrationV2ProviderSession {
        self.session.runtime.provider_session()
    }

    fn events(&self) -> ProviderEventStream {
        Box::pin(subscribe(&self.session).events)
    }

    fn subscribe_events(
        &self,
    ) -> Option<BoxFuture<'_, Result<ProviderAdapterV2EventSubscription, ProviderAdapterV2Error>>>
    {
        let session = Arc::clone(&self.session);
        Some(Box::pin(async move { Ok(subscribe(&session)) }))
    }

    fn has_pending_background_work(
        &self,
    ) -> Option<BoxFuture<'_, Result<bool, ProviderAdapterV2Error>>> {
        self.session.runtime.has_pending_background_work()
    }

    fn has_pending_background_work_for_thread(
        &self,
        provider_thread: &turin_contracts::orchestration_v2::OrchestrationV2ProviderThread,
    ) -> Option<BoxFuture<'_, Result<bool, ProviderAdapterV2Error>>> {
        self.session
            .runtime
            .has_pending_background_work_for_thread(provider_thread)
    }

    fn background_tasks_end_with_process(&self) -> bool {
        self.session.runtime.background_tasks_end_with_process()
    }

    fn get_model_context_window(
        &self,
        model_selection: &turin_contracts::model_selection::ModelSelection,
        cwd: Option<&str>,
    ) -> Option<Option<u64>> {
        self.session
            .runtime
            .get_model_context_window(model_selection, cwd)
    }

    fn can_reuse_context_usage(
        &self,
        previous: &turin_contracts::model_selection::ModelSelection,
        next: &turin_contracts::model_selection::ModelSelection,
    ) -> Option<bool> {
        self.session.runtime.can_reuse_context_usage(previous, next)
    }

    async fn ensure_thread(
        &self,
        input: turin_provider_core::provider_adapter::ProviderAdapterV2EnsureThreadInput,
    ) -> Result<
        turin_contracts::orchestration_v2::OrchestrationV2ProviderThread,
        ProviderAdapterV2Error,
    > {
        self.inner.touch(&self.session, false);
        self.session.runtime.ensure_thread(input).await
    }

    async fn resume_thread(
        &self,
        input: turin_provider_core::provider_adapter::ProviderAdapterV2ResumeThreadInput,
    ) -> Result<
        turin_contracts::orchestration_v2::OrchestrationV2ProviderThread,
        ProviderAdapterV2Error,
    > {
        self.inner.touch(&self.session, false);
        self.session.runtime.resume_thread(input).await
    }

    fn inject_history(
        &self,
        provider_thread: &turin_contracts::orchestration_v2::OrchestrationV2ProviderThread,
        context: &turin_provider_core::provider_adapter::ProviderAdapterV2HistoricalContext,
    ) -> Option<BoxFuture<'_, Result<bool, ProviderAdapterV2Error>>> {
        self.session
            .runtime
            .inject_history(provider_thread, context)
    }

    /// `startTurn`: the thread attaches, the turn is busy until its terminal
    /// event, and a start that fails (or is cancelled) clears its own busy mark.
    async fn start_turn(
        &self,
        input: ProviderAdapterV2TurnInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        if let Err(cause) = self
            .inner
            .attach_thread(&self.session, &input.thread_id)
            .await
        {
            warn!(
                provider_session_id = %self.session.provider_session_id,
                %cause,
                "orchestration-v2.driver-session.activity-failed"
            );
        }
        let key = (input.provider_thread.id.clone(), input.run_ordinal);
        self.inner.mark_busy(&self.session, key.clone());
        let mut guard = BusyTurnGuard {
            inner: Arc::clone(&self.inner),
            session: Arc::clone(&self.session),
            key: Some(key),
        };
        let result = self.session.runtime.start_turn(input).await;
        if result.is_ok() {
            guard.key = None;
        }
        result
    }

    fn compact_thread(
        &self,
        input: ProviderAdapterV2TurnInput,
    ) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        self.inner.touch(&self.session, false);
        self.session.runtime.compact_thread(input)
    }

    async fn steer_turn(
        &self,
        input: turin_provider_core::provider_adapter::ProviderAdapterV2SteerInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        self.inner.touch(&self.session, false);
        self.session.runtime.steer_turn(input).await
    }

    fn stop_task(
        &self,
        input: turin_provider_core::provider_adapter::ProviderAdapterV2StopTaskInput,
    ) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        self.inner.touch(&self.session, false);
        self.session.runtime.stop_task(input)
    }

    async fn interrupt_turn(
        &self,
        input: turin_provider_core::provider_adapter::ProviderAdapterV2InterruptInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        self.inner.touch(&self.session, false);
        self.session.runtime.interrupt_turn(input).await
    }

    fn stop_subagent(
        &self,
        input: turin_provider_core::provider_adapter::ProviderAdapterV2StopSubagentInput,
    ) -> Option<
        BoxFuture<
            '_,
            Result<
                Option<turin_provider_core::provider_adapter::ProviderAdapterV2SubagentOutcome>,
                ProviderAdapterV2Error,
            >,
        >,
    > {
        self.inner.touch(&self.session, false);
        self.session.runtime.stop_subagent(input)
    }

    fn unload_thread(
        &self,
        provider_thread: &turin_contracts::orchestration_v2::OrchestrationV2ProviderThread,
    ) -> Option<BoxFuture<'_, Result<(), ProviderAdapterV2Error>>> {
        self.session.runtime.unload_thread(provider_thread)
    }

    async fn respond_to_runtime_request(
        &self,
        input: turin_provider_core::provider_adapter::ProviderAdapterV2RuntimeRequestResponseInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        self.inner.touch(&self.session, false);
        self.session.runtime.respond_to_runtime_request(input).await
    }

    async fn read_thread_snapshot(
        &self,
        input: turin_provider_core::provider_adapter::ProviderAdapterV2ReadThreadSnapshotInput,
    ) -> Result<
        turin_provider_core::provider_adapter::ProviderAdapterV2ThreadSnapshot,
        ProviderAdapterV2Error,
    > {
        self.inner.touch(&self.session, false);
        self.session.runtime.read_thread_snapshot(input).await
    }

    fn upload_feedback(
        &self,
        input: turin_provider_core::provider_adapter::ProviderAdapterV2UploadFeedbackInput,
    ) -> Option<BoxFuture<'_, Result<String, ProviderAdapterV2Error>>> {
        self.session.runtime.upload_feedback(input)
    }

    fn mcp_apps(
        &self,
    ) -> Option<Arc<dyn turin_provider_core::provider_adapter::ProviderAdapterV2McpApps>> {
        self.session.runtime.mcp_apps()
    }

    async fn rollback_thread(
        &self,
        input: turin_provider_core::provider_adapter::ProviderAdapterV2RollbackThreadInput,
    ) -> Result<
        turin_provider_core::provider_adapter::ProviderAdapterV2ThreadSnapshot,
        ProviderAdapterV2Error,
    > {
        self.inner.touch(&self.session, false);
        self.session.runtime.rollback_thread(input).await
    }

    async fn fork_thread(
        &self,
        input: turin_provider_core::provider_adapter::ProviderAdapterV2ForkThreadInput,
    ) -> Result<
        turin_contracts::orchestration_v2::OrchestrationV2ProviderThread,
        ProviderAdapterV2Error,
    > {
        self.inner.touch(&self.session, false);
        self.session.runtime.fork_thread(input).await
    }
}

/// Clears a turn's busy mark when its start future is dropped or fails.
struct BusyTurnGuard {
    inner: Arc<Inner>,
    session: Arc<LiveSession>,
    key: Option<BusyTurnKey>,
}

impl Drop for BusyTurnGuard {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            self.inner.mark_idle(&self.session, &key);
        }
    }
}

/// `workspace` check from `open`. Only a NotFound stat means the folder is gone.
async fn workspace_is_directory(cwd: &str) -> bool {
    match tokio::fs::metadata(cwd).await {
        Ok(metadata) => metadata.is_dir(),
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

/// `ProviderSessionManagerV2`.
pub struct ProviderSessionManager {
    inner: Arc<Inner>,
}

impl ProviderSessionManager {
    pub fn new(
        lookup: Arc<dyn ProviderAdapterLookup>,
        sink: Arc<dyn ProviderSessionSink>,
        options: ProviderSessionManagerOptions,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                lookup,
                sink,
                options,
                sessions: Mutex::new(HashMap::new()),
                open_locks: Mutex::new(HashMap::new()),
                shutting_down: AtomicBool::new(false),
            }),
        }
    }

    /// `open`: reuses a live session for this id (attaching the thread), or
    /// opens one through the instance's adapter. Opens for one id are serialized.
    pub async fn open(
        &self,
        input: ProviderAdapterV2OpenSessionInput,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderSessionManagerError> {
        let inner = &self.inner;
        let provider_session_id = input.provider_session_id.clone();
        let lock = inner.open_lock(&provider_session_id);
        let guard = lock.lock().await;
        let result = Self::open_locked(inner, input).await;
        drop(guard);
        inner.prune_open_lock(&provider_session_id, &lock);
        result
    }

    async fn open_locked(
        inner: &Arc<Inner>,
        input: ProviderAdapterV2OpenSessionInput,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderSessionManagerError> {
        if let Some(cwd) = input.runtime_policy.cwd.as_deref()
            && !workspace_is_directory(cwd).await
        {
            return Err(ProviderWorkspaceMissingError {
                thread_id: input.thread_id.as_str().to_owned(),
                cwd: cwd.to_owned(),
            }
            .into());
        }

        let instance_id = input.model_selection.instance_id.clone();
        let provider_session_id = input.provider_session_id.clone();
        let thread_id = input.thread_id.clone();
        let open_error = |cause: BoxError| ProviderSessionManagerError::Open {
            instance_id: instance_id.clone(),
            provider_session_id: provider_session_id.clone(),
            cause,
        };

        if let Some(existing) = inner.get_session(&provider_session_id) {
            let attached = existing
                .state
                .lock()
                .attached_thread_ids
                .contains(&thread_id);
            if !attached && !existing.supports_multiple_provider_threads {
                return Err(open_error(
                    format!(
                        "Provider {} does not support attaching multiple app threads to one session.",
                        existing.runtime.driver()
                    )
                    .into(),
                ));
            }
            inner
                .attach_thread(&existing, &thread_id)
                .await
                .map_err(open_error)?;
            inner.touch(&existing, false);
            return Ok(managed(inner, existing));
        }

        let adapter = inner.lookup.adapter(&instance_id).ok_or_else(|| {
            open_error(Box::new(ProviderInstanceNotFoundError {
                instance_id: instance_id.as_str().to_owned(),
                cause: None,
            }))
        })?;
        let runtime = adapter
            .open_session(input)
            .await
            .map_err(|cause| open_error(Box::new(cause)))?;
        let supports_multiple = runtime
            .provider_session()
            .capabilities
            .sessions
            .supports_multiple_provider_threads_per_session;

        let session = LiveSession::new(Arc::clone(&runtime), supports_multiple, thread_id.clone());
        inner
            .sessions
            .lock()
            .insert(provider_session_id.clone(), Arc::clone(&session));

        // The entry is live before the attach write, so a failure below
        // releases it through the normal path.
        if let Err(cause) = inner
            .sink
            .session_attached(
                &session.identity(),
                std::slice::from_ref(&thread_id),
                runtime.provider_session(),
            )
            .await
        {
            let release = inner
                .release(ReleaseRequest {
                    provider_session_id: provider_session_id.clone(),
                    reason: ProviderSessionReleaseReason::RuntimeError,
                    detail: Some("Failed to persist the provider-session attachment.".to_owned()),
                    graceful_subscribers: false,
                    only_if_idle_generation: None,
                    origin: ReleaseOrigin::Caller,
                })
                .await;
            if let Err(release_cause) = release {
                warn!(%release_cause, "orchestration-v2.driver-session.open-release-failed");
            }
            return Err(open_error(cause));
        }

        let pump = spawn_event_pump(inner, &session);
        session.state.lock().pump = Some(pump);
        inner.schedule_idle_release(&session);
        Ok(managed(inner, session))
    }

    /// `get`: the live runtime for an id, with activity recorded.
    pub fn get(
        &self,
        provider_session_id: &ProviderSessionId,
    ) -> Option<Arc<dyn ProviderSessionRuntime>> {
        let session = self.inner.get_session(provider_session_id)?;
        self.inner.touch(&session, false);
        Some(managed(&self.inner, session))
    }

    /// `close`: manual release of one session.
    pub async fn close(
        &self,
        provider_session_id: &ProviderSessionId,
    ) -> Result<(), ProviderSessionManagerError> {
        self.release(
            provider_session_id,
            ProviderSessionReleaseReason::ManualShutdown,
            None,
        )
        .await
    }

    /// `release`: releases one session for the given reason.
    pub async fn release(
        &self,
        provider_session_id: &ProviderSessionId,
        reason: ProviderSessionReleaseReason,
        detail: Option<String>,
    ) -> Result<(), ProviderSessionManagerError> {
        self.inner
            .release(ReleaseRequest {
                provider_session_id: provider_session_id.clone(),
                reason,
                detail,
                graceful_subscribers: false,
                only_if_idle_generation: None,
                origin: ReleaseOrigin::Caller,
            })
            .await
    }

    /// `closeInstance`: releases every live session of one provider instance.
    pub async fn close_instance(
        &self,
        instance_id: &ProviderInstanceId,
    ) -> Result<(), ProviderSessionManagerError> {
        let ids: Vec<ProviderSessionId> = self
            .inner
            .sessions
            .lock()
            .values()
            .filter(|session| session.runtime.instance_id() == instance_id)
            .map(|session| session.provider_session_id.clone())
            .collect();
        let detail = format!("Provider instance {instance_id} logged out.");
        let outcomes = join_all(ids.iter().map(|id| {
            self.release(
                id,
                ProviderSessionReleaseReason::ManualShutdown,
                Some(detail.clone()),
            )
        }))
        .await;
        outcomes.into_iter().find(Result::is_err).unwrap_or(Ok(()))
    }

    /// `detach`: a thread leaves the session (for example on a workspace
    /// change). A single-thread session is released once its last thread
    /// leaves; a shared one stays up for the others.
    pub async fn detach(
        &self,
        provider_session_id: &ProviderSessionId,
        thread_id: &ThreadId,
        detail: Option<String>,
    ) -> Result<(), ProviderSessionManagerError> {
        let Some(session) = self.inner.get_session(provider_session_id) else {
            return Ok(());
        };
        let remaining = {
            let mut state = session.state.lock();
            if !state.attached_thread_ids.shift_remove(thread_id) {
                return Ok(());
            }
            state.attached_thread_ids.len()
        };
        if remaining == 0 && !session.supports_multiple_provider_threads {
            return self
                .inner
                .release(ReleaseRequest {
                    provider_session_id: provider_session_id.clone(),
                    reason: ProviderSessionReleaseReason::ManualShutdown,
                    detail,
                    graceful_subscribers: false,
                    only_if_idle_generation: None,
                    origin: ReleaseOrigin::Caller,
                })
                .await;
        }
        self.inner.schedule_idle_release(&session);
        Ok(())
    }

    /// Server shutdown: releases every live session and stops accepting provider events.
    pub async fn shutdown(&self) {
        self.inner.shutting_down.store(true, Ordering::SeqCst);
        let ids: Vec<ProviderSessionId> = self.inner.sessions.lock().keys().cloned().collect();
        let outcomes = join_all(ids.into_iter().map(|provider_session_id| {
            self.inner.release(ReleaseRequest {
                provider_session_id,
                reason: ProviderSessionReleaseReason::ServerShutdown,
                detail: None,
                graceful_subscribers: false,
                only_if_idle_generation: None,
                origin: ReleaseOrigin::Caller,
            })
        }))
        .await;
        for outcome in outcomes {
            if let Err(cause) = outcome {
                error!(%cause, "orchestration-v2.driver-session.shutdown-release-failed");
            }
        }
    }
}

fn managed(inner: &Arc<Inner>, session: Arc<LiveSession>) -> Arc<dyn ProviderSessionRuntime> {
    Arc::new(ManagedRuntime {
        inner: Arc::clone(inner),
        session,
    })
}

#[cfg(test)]
mod tests;
