//! Port of upstream/packages/provider-core/src/server/providerAuthFlow.ts.
//!
//! Adapters do login and credential handling. This owns client consent, flow
//! lifetime, and the published sign-in state (`ProviderAuthState`).
//!
//! Upstream fibers are tokio tasks. Each is held in a [`TaskSlot`] that can be
//! aborted, and drained (aborted, then awaited until its cleanup has run).
//! Upstream `Effect.uninterruptible` sections run on a spawned task that the
//! caller awaits. Flow tasks hold `Weak` references, so dropping the controller
//! aborts them. Dropping does not await their cleanup; that needs the async
//! path that cancel, logout, and success already take.

// `ProviderSetupError` is the upstream error type for this flow; boxing it
// would change nothing on the wire.
#![allow(clippy::result_large_err, clippy::type_complexity)]

use std::{
    any::Any,
    future::Future,
    panic::{AssertUnwindSafe, resume_unwind},
    sync::{Arc, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use futures::{FutureExt, Stream, StreamExt};
use tokio::{
    sync::{Mutex as AsyncMutex, oneshot, watch},
    task::AbortHandle,
};
use tokio_stream::wrappers::WatchStream;
use turin_contracts::{
    provider_instance::ProviderInstanceId,
    provider_setup::{
        ProviderAuthInteraction, ProviderAuthMethod, ProviderAuthPhase, ProviderAuthRespondInput,
        ProviderAuthResponse, ProviderAuthState, ProviderSetupError, SetupOperationId,
    },
};

use crate::{
    auth::{
        AdmittedFailure, AdmittedTask, ProviderAuthCallbackMode, ProviderAuthCompleteInput,
        ProviderAuthController, ProviderCredentialBinding, SessionScope,
    },
    rt::BoxFuture,
};

pub type FlowResult<T = ()> = Result<T, ProviderSetupError>;
pub type RespondFn =
    Arc<dyn Fn(ProviderAuthResponse) -> BoxFuture<'static, FlowResult> + Send + Sync>;
pub type CompleteFn = Arc<dyn Fn(String) -> BoxFuture<'static, FlowResult> + Send + Sync>;
pub type MethodsFn =
    Arc<dyn Fn() -> BoxFuture<'static, FlowResult<Vec<ProviderAuthMethod>>> + Send + Sync>;
pub type AuthenticateFn =
    Arc<dyn Fn(String, FlowContext) -> BoxFuture<'static, FlowResult> + Send + Sync>;
pub type LogoutFn = Arc<dyn Fn() -> BoxFuture<'static, FlowResult<Option<String>>> + Send + Sync>;
pub type SetInteractionFn = Arc<
    dyn Fn(ProviderAuthInteraction, Option<RespondFn>, Option<CompleteFn>) -> BoxFuture<'static, ()>
        + Send
        + Sync,
>;
pub type VerifyingFn = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// `ProviderAuthFlowContext`: what the adapter's `authenticate` may use.
#[derive(Clone)]
pub struct FlowContext {
    pub flow_id: String,
    /// Unix milliseconds, as upstream.
    pub expires_at: i64,
    pub return_url: Option<String>,
    pub callback_mode: Option<ProviderAuthCallbackMode>,
    pub set_interaction: SetInteractionFn,
    pub verifying: VerifyingFn,
}

pub struct ProviderAuthFlowOptions {
    pub instance_id: ProviderInstanceId,
    pub credential_binding: ProviderCredentialBinding,
    pub methods: MethodsFn,
    pub default_method_id: Option<String>,
    /// Stored account profiles can change the advertised methods after auth/logout.
    pub refresh_methods_after_auth: bool,
    /// Fails with safe text for the user, never native token data.
    pub authenticate: AuthenticateFn,
    pub logout: LogoutFn,
    /// Upstream `timeoutMs`; the default is 300 s.
    pub timeout: Duration,
}

impl ProviderAuthFlowOptions {
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
}

/// Build the controller for one provider instance. Method discovery starts
/// immediately, so this must run inside a tokio runtime.
pub fn make(options: ProviderAuthFlowOptions) -> ProviderAuthController {
    let inner = Inner::new(options);
    let refresh_inner = Arc::downgrade(&inner);
    let refresh = TaskSlot::spawn(async move {
        if let Some(inner) = refresh_inner.upgrade() {
            inner.refresh_methods().await;
        }
    });
    *inner.refresh.lock() = Some(refresh);
    controller(inner)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Idle,
    Auth,
    Stopping,
}

#[derive(Clone)]
struct Snapshot {
    owner: Option<String>,
    state: ProviderAuthState,
}

/// A spawned flow task. `drain` aborts it and waits until its state is dropped.
struct TaskSlot {
    abort: AbortHandle,
    done: watch::Receiver<bool>,
}

impl TaskSlot {
    /// The join handle is dropped. The task is owned through the abort handle,
    /// which the slot's owner aborts.
    fn spawn(fut: impl Future<Output = ()> + Send + 'static) -> Self {
        let (done_tx, done) = watch::channel(false);
        let handle = tokio::spawn(async move {
            let _signal = DoneSignal(done_tx);
            fut.await;
        });
        Self {
            abort: handle.abort_handle(),
            done,
        }
    }

    async fn drain(&self) {
        self.abort.abort();
        let mut done = self.done.clone();
        let _ = done.wait_for(|finished| *finished).await;
    }
}

/// Signals completion when the task ends, including when it is aborted. The
/// task's own locals drop first, so its cleanup has run when this fires.
struct DoneSignal(watch::Sender<bool>);

impl Drop for DoneSignal {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

struct ResponseSlot {
    id: u64,
    task: TaskSlot,
}

struct Flow {
    key: u64,
    id: String,
    owner: String,
    expires_at: i64,
    complete: Option<CompleteFn>,
    respond: Option<RespondFn>,
    fiber: Option<TaskSlot>,
    response: Option<ResponseSlot>,
}

struct Core {
    operation: Operation,
    active: Option<Flow>,
    sessions: Vec<SessionScope>,
    next_id: u64,
}

struct Inner {
    instance_id: ProviderInstanceId,
    binding: ProviderCredentialBinding,
    methods: MethodsFn,
    default_method_id: Option<String>,
    refresh_after_auth: bool,
    authenticate: AuthenticateFn,
    logout: LogoutFn,
    timeout: Duration,
    /// Upstream `lock`. Held across operations that may await.
    permit: AsyncMutex<()>,
    /// Short sections only. Never held across an await.
    core: parking_lot::Mutex<Core>,
    snapshot: watch::Sender<Snapshot>,
    refresh: parking_lot::Mutex<Option<TaskSlot>>,
}

impl Inner {
    fn new(options: ProviderAuthFlowOptions) -> Arc<Self> {
        let empty = ProviderAuthState {
            instance_id: options.instance_id.clone(),
            phase: ProviderAuthPhase::Idle,
            flow_id: None,
            authorization_url: None,
            expires_at: None,
            message: None,
            methods: None,
            interaction: Some(None),
            credential_owner: Some(options.credential_binding.owner),
        };
        let (snapshot, _) = watch::channel(Snapshot {
            owner: None,
            state: empty,
        });
        Arc::new(Self {
            instance_id: options.instance_id,
            binding: options.credential_binding,
            methods: options.methods,
            default_method_id: options.default_method_id,
            refresh_after_auth: options.refresh_methods_after_auth,
            authenticate: options.authenticate,
            logout: options.logout,
            timeout: options.timeout,
            permit: AsyncMutex::new(()),
            core: parking_lot::Mutex::new(Core {
                operation: Operation::Idle,
                active: None,
                sessions: Vec::new(),
                next_id: 0,
            }),
            snapshot,
            refresh: parking_lot::Mutex::new(None),
        })
    }

    fn failure(&self, operation: &str, detail: &str) -> ProviderSetupError {
        setup_error(&self.instance_id, operation, detail)
    }

    fn idle_state(&self, methods: Option<Vec<ProviderAuthMethod>>) -> ProviderAuthState {
        ProviderAuthState {
            instance_id: self.instance_id.clone(),
            phase: ProviderAuthPhase::Idle,
            flow_id: None,
            authorization_url: None,
            expires_at: None,
            message: None,
            methods,
            interaction: Some(None),
            credential_owner: Some(self.binding.owner),
        }
    }

    fn snapshot_state(&self) -> ProviderAuthState {
        self.snapshot.borrow().state.clone()
    }

    fn methods_or_empty(&self) -> Vec<ProviderAuthMethod> {
        self.snapshot_state().methods.unwrap_or_default()
    }

    /// Writes only while `key` is still the active flow, as upstream `publish`.
    fn publish(&self, key: u64, patch: impl FnOnce(&mut ProviderAuthState)) {
        let core = self.core.lock();
        self.publish_in(&core, key, patch);
    }

    /// `publish` for a caller that already holds the core lock. The lock is not
    /// re-entrant, so calling `publish` there deadlocks.
    fn publish_in(&self, core: &Core, key: u64, patch: impl FnOnce(&mut ProviderAuthState)) {
        let Some(flow) = core.active.as_ref().filter(|flow| flow.key == key) else {
            return;
        };
        let owner = flow.owner.clone();
        self.snapshot.send_modify(|snapshot| {
            snapshot.owner = Some(owner);
            patch(&mut snapshot.state);
        });
    }

    /// Upstream `methods` refresh. A failed refresh keeps the last known
    /// methods and reports the failure text.
    async fn refresh_methods(&self) {
        match (self.methods)().await {
            Ok(methods) => self.snapshot.send_modify(|snapshot| {
                snapshot.state.methods = Some(methods);
            }),
            Err(error) => self.snapshot.send_modify(|snapshot| {
                snapshot.state.methods.get_or_insert_with(Vec::new);
                snapshot.state.message = Some(detail_of(&error));
            }),
        }
    }

    fn take_sessions(&self) -> Vec<SessionScope> {
        std::mem::take(&mut self.core.lock().sessions)
    }

    /// Upstream `stopOwnedSessions`. Finalizers run after the lock is released.
    fn stop_owned_sessions(&self) {
        for session in self.take_sessions() {
            session.close();
        }
    }

    fn require_flow(&self, core: &Core, owner: &str, id: &str) -> FlowResult<u64> {
        let now = now_ms();
        match core.active.as_ref() {
            Some(flow)
                if core.operation == Operation::Auth
                    && flow.owner == owner
                    && flow.id == id
                    && now < flow.expires_at =>
            {
                Ok(flow.key)
            }
            _ => Err(self.failure(
                "respond",
                "This sign-in is no longer active in this client.",
            )),
        }
    }

    fn next_id(core: &mut Core) -> u64 {
        core.next_id += 1;
        core.next_id
    }

    async fn start(
        self: Arc<Self>,
        owner: String,
        stop_sessions: Option<BoxFuture<'static, FlowResult>>,
        selected: Option<String>,
        return_url: Option<String>,
        callback_mode: Option<ProviderAuthCallbackMode>,
    ) -> FlowResult<ProviderAuthState> {
        uninterruptible(async move {
            let _permit = self.permit.lock().await;
            let (key, flow_id, expires_at) = {
                let mut core = self.core.lock();
                if core.operation == Operation::Auth
                    && core.active.as_ref().is_some_and(|flow| flow.owner == owner)
                {
                    return Ok(self.snapshot_state());
                }
                if core.operation != Operation::Idle {
                    return Err(self.failure("start", "Provider setup is already in progress."));
                }
                let key = Self::next_id(&mut core);
                let flow_id = uuid::Uuid::new_v4().to_string();
                let expires_at = now_ms() + self.timeout.as_millis() as i64;
                core.active = Some(Flow {
                    key,
                    id: flow_id.clone(),
                    owner: owner.clone(),
                    expires_at,
                    complete: None,
                    respond: None,
                    fiber: None,
                    response: None,
                });
                core.operation = Operation::Auth;
                (key, flow_id, expires_at)
            };
            let state = ProviderAuthState {
                flow_id: Some(
                    SetupOperationId::new(&flow_id).expect("uuid is a valid setup operation id"),
                ),
                phase: ProviderAuthPhase::Starting,
                expires_at: Some(iso_millis(expires_at)),
                message: Some("Starting sign-in.".to_owned()),
                methods: Some(self.methods_or_empty()),
                ..self.idle_state(None)
            };
            self.snapshot.send_replace(Snapshot {
                owner: Some(owner.clone()),
                state: state.clone(),
            });
            let run = FlowRun {
                instance_id: self.instance_id.clone(),
                key,
                id: flow_id,
                stop_sessions,
                selected,
                return_url,
                callback_mode,
                expires_at,
                methods: Arc::clone(&self.methods),
                default_method_id: self.default_method_id.clone(),
                authenticate: Arc::clone(&self.authenticate),
                timeout: self.timeout,
                refresh_after_auth: self.refresh_after_auth,
            };
            let fiber = TaskSlot::spawn(run_flow(Arc::downgrade(&self), run));
            let mut core = self.core.lock();
            match core.active.as_mut().filter(|flow| flow.key == key) {
                Some(flow) => flow.fiber = Some(fiber),
                None => fiber.abort.abort(),
            }
            Ok(state)
        })
        .await
    }

    async fn begin_response(
        &self,
        owner: String,
        input: ProviderAuthRespondInput,
    ) -> FlowResult<(u64, oneshot::Receiver<FlowResult>)> {
        let _permit = self.permit.lock().await;
        let mut core = self.core.lock();
        let key = self.require_flow(&core, &owner, input.flow_id.as_str())?;
        let interaction = self.snapshot_interaction();
        let matches = interaction.as_ref().is_some_and(|interaction| {
            interaction_id(interaction) == input.interaction_id.as_str()
                && interaction_kind(interaction) == response_kind(&input.response)
        });
        let id = Self::next_id(&mut core);
        let Some(flow) = core.active.as_mut().filter(|flow| flow.key == key) else {
            return Err(self.failure(
                "respond",
                "This sign-in is no longer active in this client.",
            ));
        };
        let callback = match (&flow.respond, matches) {
            (Some(callback), true) => Arc::clone(callback),
            _ => {
                return Err(self.failure(
                    "respond",
                    "This sign-in interaction is no longer available.",
                ));
            }
        };
        if flow.response.is_some() {
            return Err(self.failure("respond", "A sign-in response is already in progress."));
        }
        let response = input.response;
        let (tx, rx) = oneshot::channel();
        let task = TaskSlot::spawn(async move {
            let result = callback(response).await;
            let _ = tx.send(result);
        });
        flow.response = Some(ResponseSlot { id, task });
        Ok((id, rx))
    }

    fn snapshot_interaction(&self) -> Option<ProviderAuthInteraction> {
        self.snapshot_state().interaction.flatten()
    }

    fn clear_response(&self, id: u64) {
        let mut core = self.core.lock();
        if let Some(flow) = core.active.as_mut()
            && flow
                .response
                .as_ref()
                .is_some_and(|response| response.id == id)
        {
            flow.response = None;
        }
    }

    async fn respond(
        self: Arc<Self>,
        owner: String,
        input: ProviderAuthRespondInput,
    ) -> FlowResult<ProviderAuthState> {
        let inner = Arc::clone(&self);
        let (id, rx) =
            uninterruptible(async move { inner.begin_response(owner, input).await }).await?;
        // The caller going away aborts the response, as upstream interrupts it.
        let guard = ResponseGuard {
            inner: Arc::downgrade(&self),
            id,
        };
        let result = rx.await;
        drop(guard);
        self.clear_response(id);
        match result {
            Ok(result) => {
                result?;
                Ok(self.snapshot_state())
            }
            Err(_) => Err(self.failure(
                "respond",
                "This sign-in response was interrupted. Start again.",
            )),
        }
    }

    async fn complete(
        &self,
        owner: String,
        input: ProviderAuthCompleteInput,
    ) -> FlowResult<ProviderAuthState> {
        let _permit = self.permit.lock().await;
        let callback = {
            let core = self.core.lock();
            let key = self.require_flow(&core, &owner, &input.flow_id);
            let flow_key = key?;
            let flow = core.active.as_ref().filter(|flow| flow.key == flow_key);
            let accepts = matches!(
                self.snapshot_interaction(),
                Some(ProviderAuthInteraction::Browser {
                    accepts_callback: Some(true),
                    ..
                })
            ) && self.snapshot_state().phase == ProviderAuthPhase::Waiting;
            match flow.and_then(|flow| flow.complete.clone()) {
                Some(callback) if accepts => callback,
                _ => {
                    return Err(
                        self.failure("complete", "This sign-in does not accept a redirect URL.")
                    );
                }
            }
        };
        callback(input.callback_url).await?;
        Ok(self.snapshot_state())
    }

    async fn cancel(&self, owner: String, id: String) -> FlowResult<ProviderAuthState> {
        // Upstream runs cancel uninterruptibly: the drain below always finishes.
        let mut flow = {
            let _permit = self.permit.lock().await;
            let mut core = self.core.lock();
            let key = self.require_flow(&core, &owner, &id)?;
            self.publish_in(&core, key, |state| {
                state.phase = ProviderAuthPhase::Cancelled;
                state.interaction = Some(None);
                state.authorization_url = None;
                state.expires_at = None;
                state.message = Some("Sign-in cancelled.".to_owned());
            });
            let flow = core.active.take().expect("required flow is active");
            core.operation = Operation::Stopping;
            flow
        };
        if let Some(response) = flow.response.take() {
            response.task.drain().await;
        }
        if let Some(fiber) = flow.fiber.take() {
            fiber.drain().await;
        }
        self.core.lock().operation = Operation::Idle;
        Ok(self.snapshot_state())
    }

    async fn logout(
        &self,
        stop_sessions: BoxFuture<'static, FlowResult>,
    ) -> FlowResult<ProviderAuthState> {
        let mut flow = {
            let _permit = self.permit.lock().await;
            let mut core = self.core.lock();
            if core.operation != Operation::Idle && core.operation != Operation::Auth {
                return Err(self.failure("logout", "Provider setup is already stopping."));
            }
            core.operation = Operation::Stopping;
            core.active.take()
        };
        if let Some(flow) = flow.as_mut() {
            if let Some(response) = flow.response.take() {
                response.task.drain().await;
            }
            if let Some(fiber) = flow.fiber.take() {
                fiber.drain().await;
            }
        }
        let result: FlowResult<Option<String>> = async {
            let stopped = stop_sessions.await;
            self.stop_owned_sessions();
            stopped?;
            let message = (self.logout)().await?;
            if self.refresh_after_auth {
                self.refresh_methods().await;
            }
            Ok(message)
        }
        .await;
        let state = match &result {
            Ok(message) => ProviderAuthState {
                phase: ProviderAuthPhase::Idle,
                message: Some(message.clone().unwrap_or_else(|| "Signed out.".to_owned())),
                ..self.idle_state(Some(self.methods_or_empty()))
            },
            Err(_) => ProviderAuthState {
                phase: ProviderAuthPhase::Failed,
                message: Some("Could not sign out. Try again.".to_owned()),
                ..self.idle_state(Some(self.methods_or_empty()))
            },
        };
        {
            let _permit = self.permit.lock().await;
            self.snapshot.send_replace(Snapshot {
                owner: None,
                state: state.clone(),
            });
            self.core.lock().operation = Operation::Idle;
        }
        result.map(|_| state)
    }

    async fn adopt_credentials(
        self: Arc<Self>,
        update: BoxFuture<'static, FlowResult>,
        stop_sessions: BoxFuture<'static, FlowResult>,
    ) -> FlowResult<ProviderAuthState> {
        uninterruptible(async move {
            {
                let _permit = self.permit.lock().await;
                let mut core = self.core.lock();
                if core.operation != Operation::Idle {
                    return Err(
                        self.failure("import", "Finish or cancel the existing sign-in first.")
                    );
                }
                core.operation = Operation::Stopping;
            }
            let result: FlowResult = async {
                self.stop_owned_sessions();
                stop_sessions.await?;
                update.await?;
                self.refresh_methods().await;
                Ok(())
            }
            .await;
            let state = match &result {
                Ok(()) => ProviderAuthState {
                    phase: ProviderAuthPhase::Succeeded,
                    ..self.idle_state(Some(self.methods_or_empty()))
                },
                Err(error) => ProviderAuthState {
                    phase: ProviderAuthPhase::Failed,
                    message: Some(detail_of(error)),
                    ..self.idle_state(Some(self.methods_or_empty()))
                },
            };
            {
                let _permit = self.permit.lock().await;
                self.snapshot.send_replace(Snapshot {
                    owner: None,
                    state: state.clone(),
                });
                self.core.lock().operation = Operation::Idle;
            }
            result.map(|()| state)
        })
        .await
    }

    async fn invalidate(&self) {
        let _permit = self.permit.lock().await;
        if self.core.lock().operation != Operation::Idle {
            return;
        }
        self.stop_owned_sessions();
        let methods = self.methods_or_empty();
        self.snapshot.send_replace(Snapshot {
            owner: None,
            state: ProviderAuthState {
                message: Some("This provider's shared sign-in changed.".to_owned()),
                ..self.idle_state(Some(methods))
            },
        });
    }

    /// Upstream `withAccess`. A task admitted while idle keeps its session
    /// open until the next stop, so its finalizers run then. A task that
    /// fails, or whose caller goes away, closes its session.
    async fn with_access(
        self: Arc<Self>,
        task: AdmittedTask,
    ) -> Result<Box<dyn Any + Send>, AdmittedFailure> {
        let session = {
            let mut core = self.core.lock();
            if core.operation != Operation::Idle {
                return Err(AdmittedFailure::Setup(self.failure(
                    "session",
                    "Provider sign-in is changing. Try again after it finishes.",
                )));
            }
            let session = SessionScope::default();
            core.sessions.push(session.clone());
            session
        };
        let handle = tokio::spawn((task.make)(session.clone()));
        let abort = handle.abort_handle();
        session.add_finalizer(move || abort.abort());
        let mut close_on_exit = CloseOnExit(Some(session.clone()));
        let joined = handle.await;
        let outcome = match joined {
            Ok(outcome) => outcome,
            Err(error) if error.is_panic() => resume_unwind(error.into_panic()),
            Err(_) => Err(AdmittedFailure::Setup(self.failure(
                "session",
                "Provider sign-in is changing. Try again after it finishes.",
            ))),
        };
        match outcome {
            Ok(value) => {
                close_on_exit.0 = None;
                Ok(value)
            }
            Err(failure) => {
                session.close();
                self.core
                    .lock()
                    .sessions
                    .retain(|open| !open.same_session(&session));
                Err(failure)
            }
        }
    }

    fn subscribe(
        self: &Arc<Self>,
        owner: String,
    ) -> Box<dyn Stream<Item = ProviderAuthState> + Send> {
        let inner = Arc::clone(self);
        Box::new(
            WatchStream::new(self.snapshot.subscribe()).map(move |snapshot| {
                if snapshot.owner.is_none() || snapshot.owner.as_deref() == Some(owner.as_str()) {
                    return snapshot.state;
                }
                let in_progress = inner.core.lock().active.is_some();
                ProviderAuthState {
                    flow_id: None,
                    authorization_url: None,
                    interaction: Some(None),
                    expires_at: None,
                    message: if in_progress {
                        Some("Sign-in is in progress in another client.".to_owned())
                    } else {
                        snapshot.state.message
                    },
                    ..snapshot.state
                }
            }),
        )
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(refresh) = self.refresh.get_mut().take() {
            refresh.abort.abort();
        }
        let core = self.core.get_mut();
        if let Some(flow) = core.active.take() {
            if let Some(response) = flow.response {
                response.task.abort.abort();
            }
            if let Some(fiber) = flow.fiber {
                fiber.abort.abort();
            }
        }
    }
}

/// Aborts an in-flight response when its caller goes away. The response task
/// finishes its cleanup after the abort.
struct ResponseGuard {
    inner: Weak<Inner>,
    id: u64,
}

impl Drop for ResponseGuard {
    fn drop(&mut self) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let mut core = inner.core.lock();
        if let Some(flow) = core.active.as_mut()
            && let Some(response) = flow.response.as_ref()
            && response.id == self.id
        {
            response.task.abort.abort();
        }
    }
}

/// Closes an admitted session if the admitting future is dropped first.
struct CloseOnExit(Option<SessionScope>);

impl Drop for CloseOnExit {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            session.close();
        }
    }
}

struct FlowRun {
    instance_id: ProviderInstanceId,
    key: u64,
    id: String,
    stop_sessions: Option<BoxFuture<'static, FlowResult>>,
    selected: Option<String>,
    return_url: Option<String>,
    callback_mode: Option<ProviderAuthCallbackMode>,
    expires_at: i64,
    methods: MethodsFn,
    default_method_id: Option<String>,
    authenticate: AuthenticateFn,
    timeout: Duration,
    refresh_after_auth: bool,
}

/// The `start` fiber: discovery, consent, adapter authentication, then the
/// terminal publish. The whole body runs under the flow timeout.
async fn run_flow(weak: Weak<Inner>, run: FlowRun) {
    let key = run.key;
    let timeout = run.timeout;
    let refresh_after_auth = run.refresh_after_auth;
    let instance_id = run.instance_id.clone();
    let body = AssertUnwindSafe(flow_body(weak.clone(), run)).catch_unwind();
    let result: FlowResult = match tokio::time::timeout(timeout, body).await {
        Err(_) => Err(setup_error(
            &instance_id,
            "start",
            "Sign-in expired. Start again.",
        )),
        Ok(Ok(result)) => result,
        Ok(Err(_panic)) => Err(setup_error(
            &instance_id,
            "start",
            "Sign-in failed. Start again.",
        )),
    };
    finish_flow(weak, key, result, refresh_after_auth).await;
}

async fn flow_body(weak: Weak<Inner>, run: FlowRun) -> FlowResult {
    let instance_id = run.instance_id.clone();
    let method_list = (run.methods)().await?;
    if let Some(inner) = weak.upgrade() {
        inner.publish(run.key, |state| state.methods = Some(method_list.clone()));
    }
    let method_id = run.selected.or(run.default_method_id).or_else(|| {
        method_list
            .first()
            .map(|method| method.id.as_str().to_owned())
    });
    let method_id =
        method_id.filter(|id| method_list.iter().any(|method| method.id.as_str() == id));
    let Some(method_id) = method_id else {
        return Err(setup_error(
            &instance_id,
            "start",
            "The provider did not advertise this sign-in method.",
        ));
    };
    // `stopSessions.pipe(Effect.ensuring(stopOwnedSessions))`
    let stopped = match run.stop_sessions {
        Some(stop) => stop.await,
        None => Ok(()),
    };
    if let Some(inner) = weak.upgrade() {
        inner.stop_owned_sessions();
    }
    stopped?;
    let context = FlowContext {
        flow_id: run.id,
        expires_at: run.expires_at,
        return_url: run.return_url,
        callback_mode: run.callback_mode,
        set_interaction: set_interaction_fn(weak.clone(), run.key),
        verifying: verifying_fn(weak.clone(), run.key),
    };
    (run.authenticate)(method_id, context).await
}

fn set_interaction_fn(weak: Weak<Inner>, key: u64) -> SetInteractionFn {
    Arc::new(move |interaction, respond, complete| {
        let weak = weak.clone();
        Box::pin(async move {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            {
                let mut core = inner.core.lock();
                let Some(flow) = core.active.as_mut().filter(|flow| flow.key == key) else {
                    return;
                };
                flow.respond = respond;
                flow.complete = complete;
            }
            let authorization_url = match &interaction {
                ProviderAuthInteraction::Browser { url, .. }
                | ProviderAuthInteraction::DeviceCode { url, .. } => Some(url.as_str().to_owned()),
                _ => None,
            };
            inner.publish(key, |state| {
                state.phase = ProviderAuthPhase::Waiting;
                state.interaction = Some(Some(interaction));
                state.authorization_url = authorization_url;
                state.message = Some("Complete sign-in to continue.".to_owned());
            });
        })
    })
}

fn verifying_fn(weak: Weak<Inner>, key: u64) -> VerifyingFn {
    Arc::new(move || {
        let weak = weak.clone();
        Box::pin(async move {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            {
                let mut core = inner.core.lock();
                if let Some(flow) = core.active.as_mut().filter(|flow| flow.key == key) {
                    flow.respond = None;
                    flow.complete = None;
                }
            }
            inner.publish(key, |state| {
                state.phase = ProviderAuthPhase::Verifying;
                state.interaction = Some(None);
                state.authorization_url = None;
                state.message = Some("Checking provider sign-in.".to_owned());
            });
        })
    })
}

/// The finishing step: drain the response, refresh methods, and publish the
/// terminal state. Expiry and success both pass through here.
async fn finish_flow(weak: Weak<Inner>, key: u64, result: FlowResult, refresh_after_auth: bool) {
    let Some(inner) = weak.upgrade() else {
        return;
    };
    let permit = inner.permit.lock().await;
    let finishing = {
        let mut core = inner.core.lock();
        if core.active.as_ref().is_some_and(|flow| flow.key == key) {
            core.operation = Operation::Stopping;
            true
        } else {
            false
        }
    };
    if !finishing {
        return;
    }
    let response = inner
        .core
        .lock()
        .active
        .as_mut()
        .and_then(|flow| flow.response.take());
    drop(permit);
    if let Some(response) = response {
        response.task.drain().await;
    }
    let _permit = inner.permit.lock().await;
    if !inner
        .core
        .lock()
        .active
        .as_ref()
        .is_some_and(|flow| flow.key == key)
    {
        return;
    }
    if refresh_after_auth {
        inner.refresh_methods().await;
    }
    let message = match &result {
        Ok(()) => "Sign-in complete.".to_owned(),
        Err(error) => detail_of(error),
    };
    let phase = if result.is_ok() {
        ProviderAuthPhase::Succeeded
    } else {
        ProviderAuthPhase::Failed
    };
    inner.publish(key, |state| {
        state.phase = phase;
        state.interaction = Some(None);
        state.authorization_url = None;
        state.expires_at = None;
        state.message = Some(message);
    });
    let mut core = inner.core.lock();
    core.active = None;
    core.operation = Operation::Idle;
}

fn controller(inner: Arc<Inner>) -> ProviderAuthController {
    let binding = inner.binding.clone();
    let refresh_methods = Arc::clone(&inner);
    let invalidate = Arc::clone(&inner);
    let busy = Arc::clone(&inner);
    let adopt = Arc::clone(&inner);
    let access = Arc::clone(&inner);
    let start = Arc::clone(&inner);
    let complete = Arc::clone(&inner);
    let cancel = Arc::clone(&inner);
    let respond = Arc::clone(&inner);
    let logout = Arc::clone(&inner);
    let subscribe = Arc::clone(&inner);
    ProviderAuthController {
        credential_binding: Some(binding),
        reconnect_profile: None,
        import_profile: None,
        adopt_credentials: Some(Arc::new(move |update, stop_sessions| {
            Box::pin(Arc::clone(&adopt).adopt_credentials(update, stop_sessions))
        })),
        is_changing_credentials: Some(Arc::new(move || {
            let changing = busy.core.lock().operation != Operation::Idle;
            Box::pin(async move { changing })
        })),
        invalidate: Some(Arc::new(move || {
            let invalidate = Arc::clone(&invalidate);
            Box::pin(async move { invalidate.invalidate().await })
        })),
        refresh_methods: Some(Arc::new(move || {
            let refresh_methods = Arc::clone(&refresh_methods);
            Box::pin(async move { refresh_methods.refresh_methods().await })
        })),
        with_access: Some(Arc::new(move |task| {
            Box::pin(Arc::clone(&access).with_access(task))
        })),
        start: Arc::new(
            move |owner, stop_sessions, selected, return_url, callback_mode| {
                Box::pin(Arc::clone(&start).start(
                    owner,
                    stop_sessions,
                    selected,
                    return_url,
                    callback_mode,
                ))
            },
        ),
        complete: Arc::new(move |owner, input| {
            let complete = Arc::clone(&complete);
            Box::pin(async move { complete.complete(owner, input).await })
        }),
        cancel: Arc::new(move |owner, id| {
            let cancel = Arc::clone(&cancel);
            Box::pin(async move { uninterruptible_boxed(cancel, owner, id).await })
        }),
        respond: Some(Arc::new(move |owner, input| {
            Box::pin(Arc::clone(&respond).respond(owner, input))
        })),
        logout: Arc::new(move |stop_sessions| {
            let logout = Arc::clone(&logout);
            Box::pin(async move {
                uninterruptible(async move { logout.logout(stop_sessions).await }).await
            })
        }),
        subscribe: Arc::new(move |owner| subscribe.subscribe(owner)),
        is_logout_prompt: None,
    }
}

async fn uninterruptible_boxed(
    inner: Arc<Inner>,
    owner: String,
    id: String,
) -> FlowResult<ProviderAuthState> {
    uninterruptible(async move { inner.cancel(owner, id).await }).await
}

/// Upstream `Effect.uninterruptible`: the work runs on its own task and is
/// awaited. Dropping the caller does not stop it.
async fn uninterruptible<T: Send + 'static>(fut: impl Future<Output = T> + Send + 'static) -> T {
    match tokio::spawn(fut).await {
        Ok(value) => value,
        Err(error) if error.is_panic() => resume_unwind(error.into_panic()),
        Err(_) => panic!("uninterruptible section was cancelled by the runtime"),
    }
}

fn setup_error(
    instance_id: &ProviderInstanceId,
    operation: &str,
    detail: &str,
) -> ProviderSetupError {
    ProviderSetupError::SetupError {
        instance_id: instance_id.clone(),
        operation: operation.to_owned(),
        detail: detail.to_owned(),
        cause: None,
    }
}

fn detail_of(error: &ProviderSetupError) -> String {
    let ProviderSetupError::SetupError { detail, .. } = error;
    detail.clone()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// `DateTime.formatIso`: UTC with millisecond precision.
fn iso_millis(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|at| at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_default()
}

fn interaction_id(interaction: &ProviderAuthInteraction) -> &str {
    match interaction {
        ProviderAuthInteraction::Browser { id, .. }
        | ProviderAuthInteraction::DeviceCode { id, .. } => id.as_str(),
        ProviderAuthInteraction::Terminal { id, .. }
        | ProviderAuthInteraction::Credentials { id, .. } => id.as_str(),
    }
}

fn interaction_kind(interaction: &ProviderAuthInteraction) -> &'static str {
    match interaction {
        ProviderAuthInteraction::Browser { .. } => "browser",
        ProviderAuthInteraction::DeviceCode { .. } => "deviceCode",
        ProviderAuthInteraction::Terminal { .. } => "terminal",
        ProviderAuthInteraction::Credentials { .. } => "credentials",
    }
}

fn response_kind(response: &ProviderAuthResponse) -> &'static str {
    match response {
        ProviderAuthResponse::Browser { .. } => "browser",
        ProviderAuthResponse::Terminal { .. } => "terminal",
        ProviderAuthResponse::Credentials { .. } => "credentials",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use turin_contracts::{
        base_schemas::TrimmedNonEmptyString,
        provider_setup::{
            ProviderAuthBrowserAction, ProviderAuthMethodType, ProviderCredentialOwner,
        },
    };

    use super::*;
    use crate::auth::AdmittedTask;

    const WAIT: Duration = Duration::from_secs(5);

    fn instance() -> ProviderInstanceId {
        ProviderInstanceId::new("auth-flow-test").expect("instance id")
    }

    fn method() -> ProviderAuthMethod {
        ProviderAuthMethod {
            id: SetupOperationId::new("browser").expect("method id"),
            account_email: None,
            name: TrimmedNonEmptyString::new("Browser").expect("name"),
            description: None,
            type_: ProviderAuthMethodType::Agent,
        }
    }

    fn consent() -> ProviderAuthInteraction {
        ProviderAuthInteraction::Browser {
            id: SetupOperationId::new("consent").expect("interaction id"),
            url: TrimmedNonEmptyString::new("https://example.com/login").expect("url"),
            requires_consent: true,
            accepts_callback: None,
        }
    }

    fn authenticate(
        f: impl Fn(String, FlowContext) -> BoxFuture<'static, FlowResult> + Send + Sync + 'static,
    ) -> AuthenticateFn {
        Arc::new(f)
    }

    /// The owner's state stream, pinned so tests can poll it.
    fn states(
        controller: &ProviderAuthController,
        owner: &str,
    ) -> std::pin::Pin<Box<dyn Stream<Item = ProviderAuthState> + Send>> {
        Box::into_pin((controller.subscribe)(owner.to_owned()))
    }

    fn options(authenticate: AuthenticateFn) -> ProviderAuthFlowOptions {
        ProviderAuthFlowOptions {
            instance_id: instance(),
            credential_binding: ProviderCredentialBinding {
                owner: ProviderCredentialOwner::T3,
                key: "binding".to_owned(),
            },
            methods: Arc::new(|| Box::pin(async { Ok(vec![method()]) })),
            default_method_id: None,
            refresh_methods_after_auth: false,
            authenticate,
            logout: Arc::new(|| Box::pin(async { Ok(None) })),
            timeout: WAIT,
        }
    }

    async fn start(controller: &ProviderAuthController, owner: &str) -> ProviderAuthState {
        (controller.start)(owner.to_owned(), None, None, None, None)
            .await
            .expect("start")
    }

    /// The first state in `owner`'s view whose phase is `wanted`.
    async fn phase(
        controller: &ProviderAuthController,
        owner: &str,
        wanted: ProviderAuthPhase,
    ) -> ProviderAuthState {
        let mut stream = states(controller, owner);
        tokio::time::timeout(WAIT, async {
            while let Some(state) = stream.next().await {
                if state.phase == wanted {
                    return state;
                }
            }
            panic!("state stream ended before {wanted:?}");
        })
        .await
        .expect("phase reached")
    }

    fn browser_response(
        interaction_id: &str,
        flow_id: &SetupOperationId,
    ) -> ProviderAuthRespondInput {
        ProviderAuthRespondInput {
            instance_id: instance(),
            flow_id: flow_id.clone(),
            interaction_id: SetupOperationId::new(interaction_id).expect("interaction id"),
            response: ProviderAuthResponse::Browser {
                action: ProviderAuthBrowserAction::Accept,
            },
        }
    }

    async fn respond(
        controller: &ProviderAuthController,
        owner: &str,
        input: ProviderAuthRespondInput,
    ) -> FlowResult<ProviderAuthState> {
        (controller.respond.as_ref().expect("respond"))(owner.to_owned(), input).await
    }

    /// Admits a task whose finalizer flips `closed`, as the upstream fixtures do.
    async fn admit_closing(controller: &ProviderAuthController, closed: Arc<AtomicBool>) {
        let access = controller.with_access.as_ref().expect("with_access");
        let task = AdmittedTask::scoped(move |scope| async move {
            scope.add_finalizer(move || closed.store(true, Ordering::SeqCst));
            Ok::<(), ProviderSetupError>(())
        });
        if access(task).await.is_err() {
            panic!("admitted");
        }
    }

    async fn admitted_access(
        controller: &ProviderAuthController,
    ) -> Result<String, AdmittedFailure> {
        let access = controller.with_access.as_ref().expect("with_access");
        let value = access(AdmittedTask::new(async {
            Ok::<String, ProviderSetupError>("process".to_owned())
        }))
        .await?;
        Ok(*value.downcast::<String>().expect("string"))
    }

    fn consent_flow(
        approved: Arc<tokio::sync::Notify>,
        verified: Arc<tokio::sync::Notify>,
        started: Arc<tokio::sync::Notify>,
        attempts: Arc<AtomicUsize>,
    ) -> AuthenticateFn {
        authenticate(move |_, context| {
            let (approved, verified, started, attempts) = (
                Arc::clone(&approved),
                Arc::clone(&verified),
                Arc::clone(&started),
                Arc::clone(&attempts),
            );
            Box::pin(async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                let approve = Arc::clone(&approved);
                (context.set_interaction)(
                    consent(),
                    Some(Arc::new(move |response| {
                        let approve = Arc::clone(&approve);
                        Box::pin(async move {
                            if matches!(
                                response,
                                ProviderAuthResponse::Browser {
                                    action: ProviderAuthBrowserAction::Accept
                                }
                            ) {
                                approve.notify_one();
                            }
                            Ok(())
                        })
                    })),
                    None,
                )
                .await;
                started.notify_one();
                approved.notified().await;
                (context.verifying)().await;
                verified.notified().await;
                Ok(())
            })
        })
    }

    #[tokio::test]
    async fn pending_discovery_is_not_an_empty_method_list() {
        let (release, discovered) = tokio::sync::oneshot::channel::<()>();
        let discovered = Arc::new(parking_lot::Mutex::new(Some(discovered)));
        let methods: MethodsFn = Arc::new(move || {
            let discovered = discovered.lock().take();
            Box::pin(async move {
                if let Some(discovered) = discovered {
                    let _ = discovered.await;
                }
                Ok(Vec::new())
            })
        });
        let mut options = options(authenticate(|_, _| Box::pin(async { Ok(()) })));
        options.methods = methods;
        let controller = make(options);
        let pending = states(&controller, "owner").next().await.expect("state");
        assert!(pending.methods.is_none());
        release.send(()).expect("discovery waiting");
        let ready = phase_when_methods(&controller).await;
        assert_eq!(ready.methods, Some(Vec::new()));
    }

    async fn phase_when_methods(controller: &ProviderAuthController) -> ProviderAuthState {
        let mut stream = states(controller, "owner");
        tokio::time::timeout(WAIT, async {
            while let Some(state) = stream.next().await {
                if state.methods.is_some() {
                    return state;
                }
            }
            panic!("no methods");
        })
        .await
        .expect("methods discovered")
    }

    #[tokio::test]
    async fn consent_and_provider_verification_precede_success() {
        let approved = Arc::new(tokio::sync::Notify::new());
        let verified = Arc::new(tokio::sync::Notify::new());
        let started = Arc::new(tokio::sync::Notify::new());
        let attempts = Arc::new(AtomicUsize::new(0));
        let controller = make(options(consent_flow(
            Arc::clone(&approved),
            Arc::clone(&verified),
            Arc::clone(&started),
            Arc::clone(&attempts),
        )));
        let state = start(&controller, "owner").await;
        started.notified().await;
        let flow_id = state.flow_id.expect("flow id");
        assert!(
            phase(&controller, "other", ProviderAuthPhase::Waiting)
                .await
                .interaction
                .flatten()
                .is_none()
        );
        assert!(
            respond(&controller, "other", browser_response("consent", &flow_id))
                .await
                .is_err()
        );
        assert!(
            respond(&controller, "owner", browser_response("stale", &flow_id))
                .await
                .is_err()
        );
        start(&controller, "owner").await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        respond(&controller, "owner", browser_response("consent", &flow_id))
            .await
            .expect("accept");
        phase(&controller, "owner", ProviderAuthPhase::Verifying).await;
        verified.notify_one();
        assert_eq!(
            phase(&controller, "owner", ProviderAuthPhase::Succeeded)
                .await
                .phase,
            ProviderAuthPhase::Succeeded
        );
    }

    #[tokio::test]
    async fn cancellation_rejects_late_consent_and_permits_another_login() {
        let controller = make(options(consent_flow(
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(AtomicUsize::new(0)),
        )));
        let started = start(&controller, "owner").await;
        let flow_id = started.flow_id.clone().expect("flow id");
        tokio::time::sleep(Duration::from_millis(20)).await;
        (controller.cancel)("owner".to_owned(), flow_id.as_str().to_owned())
            .await
            .expect("cancel");
        assert_eq!(
            phase(&controller, "owner", ProviderAuthPhase::Cancelled)
                .await
                .phase,
            ProviderAuthPhase::Cancelled
        );
        assert!(
            respond(&controller, "owner", browser_response("consent", &flow_id))
                .await
                .is_err()
        );
        let next = start(&controller, "other").await;
        assert_ne!(next.flow_id, Some(flow_id));
    }

    #[tokio::test]
    async fn expiry_fails_the_login_and_rejects_its_response() {
        let mut options = options(authenticate(|_, context| {
            Box::pin(async move {
                (context.set_interaction)(consent(), None, None).await;
                std::future::pending::<()>().await;
                Ok(())
            })
        }));
        options.timeout = Duration::from_millis(50);
        let controller = make(options);
        let state = start(&controller, "owner").await;
        let failed = phase(&controller, "owner", ProviderAuthPhase::Failed).await;
        assert!(failed.message.unwrap_or_default().contains("expired"));
        assert!(
            respond(
                &controller,
                "owner",
                browser_response("consent", &state.flow_id.expect("flow id"))
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn an_admitted_session_closes_when_sign_in_starts() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let adapter_entered = Arc::clone(&entered);
        let controller = make(options(authenticate(move |_, _| {
            let entered = Arc::clone(&adapter_entered);
            Box::pin(async move {
                entered.notify_one();
                std::future::pending::<FlowResult>().await
            })
        })));
        let closed = Arc::new(AtomicBool::new(false));
        admit_closing(&controller, Arc::clone(&closed)).await;
        assert!(!closed.load(Ordering::SeqCst));
        start(&controller, "owner").await;
        entered.notified().await;
        assert!(closed.load(Ordering::SeqCst));
        assert!(admitted_access(&controller).await.is_err());
    }

    #[tokio::test]
    async fn invalidation_closes_admitted_sessions_before_reuse() {
        let controller = make(options(authenticate(|_, _| Box::pin(async { Ok(()) }))));
        let closed = Arc::new(AtomicBool::new(false));
        admit_closing(&controller, Arc::clone(&closed)).await;
        (controller.invalidate.as_ref().expect("invalidate"))().await;
        assert!(closed.load(Ordering::SeqCst));
        assert_eq!(
            admitted_access(&controller).await.expect("admitted"),
            "process"
        );
    }

    #[tokio::test]
    async fn a_device_code_is_shown_only_to_its_owner() {
        let controller = make(options(authenticate(|_, context| {
            Box::pin(async move {
                (context.set_interaction)(
                    ProviderAuthInteraction::DeviceCode {
                        id: SetupOperationId::new("code").expect("id"),
                        url: TrimmedNonEmptyString::new("https://example.com/device").expect("url"),
                        user_code: TrimmedNonEmptyString::new("ABCD-EFGH").expect("code"),
                    },
                    None,
                    None,
                )
                .await;
                std::future::pending::<()>().await;
                Ok(())
            })
        })));
        let started = start(&controller, "owner").await;
        let waiting = phase(&controller, "owner", ProviderAuthPhase::Waiting).await;
        assert!(matches!(
            waiting.interaction,
            Some(Some(ProviderAuthInteraction::DeviceCode { .. }))
        ));
        assert_eq!(
            waiting.authorization_url.as_deref(),
            Some("https://example.com/device")
        );
        let other = phase(&controller, "other", ProviderAuthPhase::Waiting).await;
        assert!(other.interaction.flatten().is_none());
        assert!(other.authorization_url.is_none());
        (controller.cancel)(
            "owner".to_owned(),
            started.flow_id.expect("flow id").as_str().to_owned(),
        )
        .await
        .expect("cancel");
    }

    #[tokio::test]
    async fn only_safe_failure_text_is_published() {
        let controller = make(options(authenticate(|_, _| {
            Box::pin(async {
                Err(ProviderSetupError::SetupError {
                    instance_id: instance(),
                    operation: "authenticate".to_owned(),
                    detail: "This account cannot sign in. Choose another account.".to_owned(),
                    cause: None,
                })
            })
        })));
        start(&controller, "owner").await;
        let failed = phase(&controller, "owner", ProviderAuthPhase::Failed).await;
        assert_eq!(
            failed.message.as_deref(),
            Some("This account cannot sign in. Choose another account.")
        );

        let panicking = make(options(authenticate(|_, _| {
            Box::pin(async { panic!("private-token in native diagnostics") })
        })));
        start(&panicking, "owner").await;
        let failed = phase(&panicking, "owner", ProviderAuthPhase::Failed).await;
        assert_eq!(
            failed.message.as_deref(),
            Some("Sign-in failed. Start again.")
        );
    }

    /// A response that never finishes. Its drop flips `cleanup_done`, the
    /// synchronous stand-in for the upstream `ensuring` block.
    struct BlockingResponse {
        cleanup_done: Arc<AtomicBool>,
    }

    impl Drop for BlockingResponse {
        fn drop(&mut self) {
            self.cleanup_done.store(true, Ordering::SeqCst);
        }
    }

    fn blocking_controller(
        cleanup_done: Arc<AtomicBool>,
        entered: Arc<tokio::sync::Notify>,
        responses: Arc<AtomicUsize>,
    ) -> ProviderAuthController {
        make(options(authenticate(move |_, context| {
            let (cleanup_done, entered, responses) = (
                Arc::clone(&cleanup_done),
                Arc::clone(&entered),
                Arc::clone(&responses),
            );
            Box::pin(async move {
                (context.set_interaction)(
                    ProviderAuthInteraction::Browser {
                        id: SetupOperationId::new("blocked").expect("id"),
                        url: TrimmedNonEmptyString::new("https://example.com/login").expect("url"),
                        requires_consent: true,
                        accepts_callback: None,
                    },
                    Some(Arc::new(move |_| {
                        let (cleanup_done, entered, responses) = (
                            Arc::clone(&cleanup_done),
                            Arc::clone(&entered),
                            Arc::clone(&responses),
                        );
                        Box::pin(async move {
                            responses.fetch_add(1, Ordering::SeqCst);
                            let _guard = BlockingResponse { cleanup_done };
                            entered.notify_one();
                            std::future::pending::<FlowResult>().await
                        })
                    })),
                    None,
                )
                .await;
                std::future::pending::<()>().await;
                Ok(())
            })
        })))
    }

    #[tokio::test]
    async fn cancel_drains_a_blocked_response_before_admitting_another_sign_in() {
        let cleanup_done = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(tokio::sync::Notify::new());
        let responses = Arc::new(AtomicUsize::new(0));
        let controller = Arc::new(blocking_controller(
            Arc::clone(&cleanup_done),
            Arc::clone(&entered),
            Arc::clone(&responses),
        ));
        let started = start(&controller, "owner").await;
        let flow_id = started.flow_id.expect("flow id");
        phase(&controller, "owner", ProviderAuthPhase::Waiting).await;
        let input = browser_response("blocked", &flow_id);
        let responding = tokio::spawn({
            let controller = Arc::clone(&controller);
            let input = input.clone();
            async move { respond(&controller, "owner", input).await }
        });
        entered.notified().await;
        let duplicate = respond(&controller, "owner", input.clone())
            .await
            .expect_err("duplicate");
        assert!(detail_of(&duplicate).contains("already in progress"));
        assert_eq!(responses.load(Ordering::SeqCst), 1);
        (controller.cancel)("owner".to_owned(), flow_id.as_str().to_owned())
            .await
            .expect("cancel");
        assert!(
            cleanup_done.load(Ordering::SeqCst),
            "cancel returned before the response cleaned up"
        );
        assert!(responding.await.expect("join").is_err());
        assert!(!(controller.is_changing_credentials.as_ref().expect("busy"))().await);
        assert_ne!(start(&controller, "other").await.flow_id, Some(flow_id));
    }

    #[tokio::test]
    async fn a_failed_method_refresh_keeps_the_last_discovered_methods() {
        let fail = Arc::new(AtomicBool::new(false));
        let methods_fail = Arc::clone(&fail);
        let mut options = options(authenticate(|_, _| Box::pin(async { Ok(()) })));
        options.methods = Arc::new(move || {
            let failing = methods_fail.load(Ordering::SeqCst);
            Box::pin(async move {
                if failing {
                    Err(ProviderSetupError::SetupError {
                        instance_id: instance(),
                        operation: "status".to_owned(),
                        detail: "interrupted".to_owned(),
                        cause: None,
                    })
                } else {
                    Ok(vec![method()])
                }
            })
        });
        let controller = make(options);
        (controller.refresh_methods.as_ref().expect("refresh"))().await;
        fail.store(true, Ordering::SeqCst);
        (controller.refresh_methods.as_ref().expect("refresh"))().await;
        let state = states(&controller, "owner").next().await.expect("state");
        assert_eq!(state.methods, Some(vec![method()]));
        assert_eq!(state.message.as_deref(), Some("interrupted"));
    }

    #[tokio::test]
    async fn rebuilding_a_controller_leaves_admitted_sessions_to_their_owners() {
        let controller = make(options(authenticate(|_, _| Box::pin(async { Ok(()) }))));
        let closed = Arc::new(AtomicBool::new(false));
        admit_closing(&controller, Arc::clone(&closed)).await;
        drop(controller);
        assert!(!closed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn adopting_credentials_stops_sessions_and_gates_access_until_saved() {
        let controller = make(options(authenticate(|_, _| Box::pin(async { Ok(()) }))));
        let closed = Arc::new(AtomicBool::new(false));
        admit_closing(&controller, Arc::clone(&closed)).await;
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let (writing_tx, writing_rx) = tokio::sync::oneshot::channel::<()>();
        let closed_at_stop = Arc::clone(&closed);
        let adopt = controller.adopt_credentials.as_ref().expect("adopt");
        let importing = tokio::spawn(adopt(
            Box::pin(async move {
                let _ = writing_tx.send(());
                let _ = released.await;
                Ok(())
            }),
            Box::pin(async move {
                assert!(closed_at_stop.load(Ordering::SeqCst));
                Ok(())
            }),
        ));
        writing_rx.await.expect("writing");
        assert!(admitted_access(&controller).await.is_err());
        release.send(()).expect("release");
        let state = importing.await.expect("join").expect("adopted");
        assert_eq!(state.phase, ProviderAuthPhase::Succeeded);
        assert_eq!(
            admitted_access(&controller).await.expect("admitted"),
            "process"
        );
    }
}
