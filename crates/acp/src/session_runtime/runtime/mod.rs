//! Port of upstream `AcpSessionRuntime.ts` `make()` service.

mod events;
mod helpers;
mod session_update;

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex as SyncMutex;
use serde_json::{Value, json};
use tokio::sync::{Mutex, Semaphore, broadcast};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

use crate::client::AcpClient;
use crate::compat::{
    ClientCapabilities, ImplementationInfo, InitializeRequest, InitializeResponse,
};
use crate::errors::{
    AcpError, AcpRequestError, AcpSpawnError, AcpTransportError, TransportOperation,
};
use crate::process::{SpawnCommand, spawn};
use crate::runtime_model::{
    AcpSessionModeState, AcpSessionSetupResponse, SessionLoadGate, extract_model_config_id,
    parse_session_mode_state, session_update_counts_as_load_replay_activity,
    session_update_is_replay, wait_for_session_load_replay_idle,
};
use crate::session_runtime::auth::select_acp_agent_auth_method;
use crate::session_runtime::{
    AcpLinuxCgroupController, AcpLinuxCgroupLease, AcpOwnedPosixProcess, AcpPosixOwnershipRoot,
    AcpPosixProcessTreeController, AcpProcessGroupTerminationError,
    TerminatePosixOwnedProcessTreeInput, make_linux_cgroup_controller,
    make_posix_process_tree_controller, observe_posix_ownership_ledger,
    signal_linux_cgroup_root_term, signal_process_group, terminate_linux_cgroup_lease,
    terminate_posix_owned_process_tree, wrap_command_for_linux_cgroup,
};

pub use events::AcpSessionRuntimeEvent;
pub use helpers::{ConfigOptionValue, session_config_options_from_setup};

use helpers::{
    auth_methods_from_initialize, config_option_current_value_matches, invalid_config_option_value,
    is_acp_authentication_required, is_startup_metadata_update,
};
use session_update::{
    AssistantSegmentState, SessionUpdateContext, ShownToolCallIds, close_active_assistant_segment,
    handle_session_update,
};

const DEFAULT_SESSION_LOAD_TIMEOUT: Duration = Duration::from_secs(90);
const DEFAULT_CANCEL_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_SESSION_LOAD_REPLAY_IDLE_GAP: Duration = Duration::from_secs(2);
const MAX_STARTUP_METADATA_UPDATES: usize = 32;

type SessionUpdateTransform = Arc<dyn Fn(Value) -> Value + Send + Sync>;
type StderrHook = Arc<dyn Fn(String) -> Result<(), AcpError> + Send + Sync>;
type InitializedHook = Arc<dyn Fn(InitializeResponse) + Send + Sync>;
type TerminationHook = Arc<dyn Fn(AcpError) + Send + Sync>;
type RequestLoggerHook = Arc<dyn Fn(RequestLogEvent) + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessContainment {
    None,
    ProcessGroup,
    ProcessLedgerReducedGuarantee,
    CgroupV2,
}

#[derive(Debug, Clone)]
pub struct AcpSpawnInput {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Option<HashMap<String, String>>,
    pub extend_env: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelBehavior {
    Interrupt,
    WaitForPrompt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeMethod {
    Load,
    Resume,
}

pub struct AcpSessionRuntimeOptions {
    pub spawn: AcpSpawnInput,
    pub cwd: PathBuf,
    /// When set, used for the ACP `initialize` request (Cursor uses protocol 1).
    pub protocol_version: Option<i64>,
    pub resume_session_id: Option<String>,
    pub resume_method: Option<ResumeMethod>,
    pub session_load_timeout: Option<Duration>,
    pub session_load_replay_idle_gap: Option<Duration>,
    pub interrupt_prompt_on_cancel: bool,
    pub cancel_meta: Option<Value>,
    pub initialize_meta: Option<Value>,
    pub own_detached_process_group: bool,
    pub own_descendant_process_groups: bool,
    pub process_group_platform: Option<String>,
    pub process_group_termination_grace: Option<Duration>,
    pub cancel_behavior: CancelBehavior,
    pub cancel_timeout: Option<Duration>,
    pub client_capabilities: Option<ClientCapabilities>,
    pub client_info: ImplementationInfo,
    pub auth_method_id: Option<String>,
    pub authenticate_eagerly: bool,
    pub authenticate_on_auth_required: bool,
    pub mcp_servers: Option<Value>,
    pub acp_mcp_servers: Option<Value>,
    pub additional_directories: Option<Vec<String>>,
    pub custom_models: Option<Vec<String>>,
    pub transform_session_update: Option<SessionUpdateTransform>,
    pub on_stderr: Option<StderrHook>,
    pub on_initialized: Option<InitializedHook>,
    pub on_termination: Option<TerminationHook>,
    pub request_logger: Option<RequestLoggerHook>,
}

#[derive(Debug, Clone)]
pub struct RequestLogEvent {
    pub method: String,
    pub payload: Value,
    pub status: RequestLogStatus,
    pub result: Option<Value>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestLogStatus {
    Started,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone)]
pub struct AcpSessionRuntimeStartResult {
    pub session_id: String,
    pub initialize_result: InitializeResponse,
    pub session_setup_result: Value,
    pub model_config_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AcpSessionActivationOptions {
    pub mcp_servers: Option<Value>,
    pub acp_mcp_servers: Option<Value>,
}

struct ActivePrompt {
    abort: tokio::task::AbortHandle,
    done: tokio::sync::watch::Receiver<bool>,
}

struct Inner {
    options: AcpSessionRuntimeOptions,
    client: AcpClient,
    pid: u32,
    process_containment: ProcessContainment,
    cgroup_lease: Option<Arc<dyn AcpLinuxCgroupLease>>,
    posix_controller: Option<Arc<dyn AcpPosixProcessTreeController>>,
    ownership_ledger: Arc<SyncMutex<HashMap<String, AcpOwnedPosixProcess>>>,
    ownership_root: Arc<SyncMutex<AcpPosixOwnershipRoot>>,
    observe_stop: Arc<AtomicBool>,
    events: broadcast::Sender<AcpSessionRuntimeEvent>,
    mode_state: Mutex<Option<AcpSessionModeState>>,
    config_options: Mutex<Value>,
    initialize_result: Mutex<Option<InitializeResponse>>,
    initialize_semaphore: Semaphore,
    start_result: Mutex<Option<AcpSessionRuntimeStartResult>>,
    start_semaphore: Semaphore,
    starting: Mutex<bool>,
    startup_metadata: Mutex<Vec<Value>>,
    session_load_gate: Arc<Mutex<Option<SessionLoadGate>>>,
    termination_error: Mutex<Option<AcpError>>,
    stopping: Mutex<bool>,
    notification_semaphore: Semaphore,
    prompt_serialization_semaphore: Semaphore,
    prompt_dispatch_semaphore: Semaphore,
    session_load_semaphore: Semaphore,
    active_prompt: Mutex<Option<ActivePrompt>>,
    assistant_updates_open: Mutex<bool>,
    update_ctx: SyncMutex<SessionUpdateContext>,
    runtime_closed: Arc<tokio::sync::Notify>,
}

#[derive(Clone)]
pub struct AcpSessionRuntime {
    inner: Arc<Inner>,
}

impl AcpSessionRuntime {
    /// Wrap an already-connected client (duplex tests and injected transports).
    pub fn from_connected_client(client: AcpClient, options: AcpSessionRuntimeOptions) -> Self {
        Self::from_client_with_containment(client, options, 0, SpawnedContainment::none())
    }

    pub async fn make(options: AcpSessionRuntimeOptions) -> Result<Self, AcpError> {
        let platform = options
            .process_group_platform
            .clone()
            .unwrap_or_else(|| std::env::consts::OS.to_owned());
        let containment = prepare_containment(&options, &platform)?;
        let (program, args) = contained_spawn_command(&options, containment.lease.as_deref());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut spawn_env = options.spawn.env.clone();
        if containment.lease.is_some() {
            let env = spawn_env.get_or_insert_with(HashMap::new);
            env.insert("ELECTRON_RUN_AS_NODE".to_owned(), "1".to_owned());
            env.insert("T3_ACP_CGROUP_WRAPPER".to_owned(), "1".to_owned());
        }
        let client = spawn(SpawnCommand {
            program: &program,
            args: &arg_refs,
            cwd: options.spawn.cwd.as_deref(),
            env: spawn_env.as_ref(),
            extend_env: options.spawn.extend_env,
            detached: options.own_detached_process_group,
            on_stderr: options.on_stderr.clone(),
        })
        .await?;
        let pid = client
            .shutdown_guard()
            .and_then(|guard| guard.downcast_ref::<crate::process::ProcessGuard>())
            .map(|guard| guard.pid())
            .unwrap_or(0);
        if let Some(controller) = containment.controller.clone() {
            let mut child_queues = HashMap::new();
            let mut frontier = HashMap::new();
            {
                let mut ledger = containment.ledger.lock();
                let mut root = containment.root.lock();
                observe_posix_ownership_ledger(
                    &mut child_queues,
                    controller.as_ref(),
                    &mut frontier,
                    &mut ledger,
                    &mut root,
                    pid as i32,
                    None,
                )
                .map_err(|error| {
                    AcpError::from(AcpSpawnError {
                        command: Some(options.spawn.command.clone()),
                        cause: error.to_string(),
                    })
                })?;
            }
            spawn_ownership_observer(
                controller,
                Arc::clone(&containment.ledger),
                Arc::clone(&containment.root),
                Arc::clone(&containment.observe_stop),
                child_queues,
                frontier,
                pid as i32,
            );
        }
        Ok(Self::from_client_with_containment(
            client,
            options,
            pid,
            containment,
        ))
    }

    fn from_client_with_containment(
        client: AcpClient,
        options: AcpSessionRuntimeOptions,
        pid: u32,
        containment: SpawnedContainment,
    ) -> Self {
        let (events_tx, _events_rx) = broadcast::channel(256);
        let runtime_closed = Arc::new(tokio::sync::Notify::new());
        let assistant_item_runtime_id = Uuid::new_v4().to_string();
        let inner = Arc::new(Inner {
            options,
            client,
            pid,
            process_containment: containment.containment,
            cgroup_lease: containment.lease,
            posix_controller: containment.controller,
            ownership_ledger: containment.ledger,
            ownership_root: containment.root,
            observe_stop: containment.observe_stop,
            events: events_tx,
            mode_state: Mutex::new(None),
            config_options: Mutex::new(session_config_options_from_setup(None)),
            initialize_result: Mutex::new(None),
            initialize_semaphore: Semaphore::new(1),
            start_result: Mutex::new(None),
            start_semaphore: Semaphore::new(1),
            starting: Mutex::new(false),
            startup_metadata: Mutex::new(Vec::new()),
            session_load_gate: Arc::new(Mutex::new(None)),
            termination_error: Mutex::new(None),
            stopping: Mutex::new(false),
            notification_semaphore: Semaphore::new(1),
            prompt_serialization_semaphore: Semaphore::new(1),
            prompt_dispatch_semaphore: Semaphore::new(1),
            session_load_semaphore: Semaphore::new(1),
            active_prompt: Mutex::new(None),
            assistant_updates_open: Mutex::new(true),
            update_ctx: SyncMutex::new(SessionUpdateContext {
                mode_state: None,
                config_options: session_config_options_from_setup(None),
                tool_calls: HashMap::new(),
                shown_tool_call_ids: ShownToolCallIds::default(),
                assistant_segment: AssistantSegmentState::default(),
                assistant_item_runtime_id,
            }),
            runtime_closed: Arc::clone(&runtime_closed),
        });
        let runtime = Self {
            inner: Arc::clone(&inner),
        };
        let hook_inner = Arc::downgrade(&inner);
        let closed_hook = Arc::clone(&runtime_closed);
        inner.client.on_termination(move |error| {
            let error = error.clone();
            let inner = hook_inner.clone();
            let closed_hook = Arc::clone(&closed_hook);
            tokio::spawn(async move {
                let Some(inner) = inner.upgrade() else {
                    closed_hook.notify_waiters();
                    return;
                };
                inner.record_termination(error.clone()).await;
                if let Some(on_termination) = inner.options.on_termination.as_ref() {
                    on_termination(error);
                }
                closed_hook.notify_waiters();
            });
        });
        let session_inner = Arc::downgrade(&inner);
        inner.client.on_session_update(move |notification| {
            let inner = session_inner.clone();
            async move {
                let Some(inner) = inner.upgrade() else {
                    return Ok(());
                };
                let _sem = inner.notification_semaphore.acquire().await;
                if inner.termination_error.lock().await.is_some() {
                    return Ok(());
                }
                inner.apply_session_update_notification(notification).await;
                Ok(())
            }
        });
        runtime
    }

    /// Records session setup after an external `session/new` or `session/load`.
    pub async fn adopt_started_session(
        &self,
        session_id: String,
        session_setup_result: Value,
    ) -> Result<(), AcpError> {
        let initialize_result = self
            .inner
            .initialize_result
            .lock()
            .await
            .clone()
            .ok_or_else(|| AcpError::transport("ACP session runtime is not initialized"))?;
        let setup_response: AcpSessionSetupResponse =
            serde_json::from_value(session_setup_result.clone()).unwrap_or_default();
        let mode_state = parse_session_mode_state(&setup_response);
        *self.inner.mode_state.lock().await = mode_state;
        *self.inner.config_options.lock().await =
            session_config_options_from_setup(Some(&session_setup_result));
        *self.inner.start_result.lock().await = Some(AcpSessionRuntimeStartResult {
            session_id,
            initialize_result,
            model_config_id: extract_model_config_id(&setup_response),
            session_setup_result,
        });
        Ok(())
    }

    pub fn process_containment(&self) -> ProcessContainment {
        self.inner.process_containment
    }

    pub fn client(&self) -> &AcpClient {
        &self.inner.client
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<AcpSessionRuntimeEvent> {
        self.inner.events.subscribe()
    }

    pub async fn initialize(&self) -> Result<InitializeResponse, AcpError> {
        self.inner.ensure_connected().await?;
        if let Some(result) = self.inner.initialize_result.lock().await.clone() {
            return Ok(result);
        }
        let _permit = self
            .inner
            .initialize_semaphore
            .acquire()
            .await
            .map_err(|_| AcpError::transport("ACP initialize lock closed"))?;
        if let Some(result) = self.inner.initialize_result.lock().await.clone() {
            return Ok(result);
        }
        let result = self.inner.initialize_once().await?;
        *self.inner.initialize_result.lock().await = Some(result.clone());
        Ok(result)
    }

    pub async fn start(&self) -> Result<AcpSessionRuntimeStartResult, AcpError> {
        self.inner.ensure_connected().await?;
        if let Some(result) = self.inner.start_result.lock().await.clone() {
            return Ok(result);
        }
        let _permit = self
            .inner
            .start_semaphore
            .acquire()
            .await
            .map_err(|_| AcpError::transport("ACP start lock closed"))?;
        if let Some(result) = self.inner.start_result.lock().await.clone() {
            return Ok(result);
        }
        self.inner.start_once().await
    }

    pub async fn get_mode_state(&self) -> Option<AcpSessionModeState> {
        self.inner.mode_state.lock().await.clone()
    }

    pub async fn get_config_options(&self) -> Value {
        self.inner.config_options.lock().await.clone()
    }

    pub async fn drain_events(&self) {
        let _ = self.inner.drain_events().await;
    }

    pub async fn terminate_process_group(&self) -> Result<(), AcpProcessGroupTerminationError> {
        self.inner.terminate_process_group().await
    }

    pub async fn prompt(&self, payload: Value) -> Result<Value, AcpError> {
        let _serialization = self
            .inner
            .prompt_serialization_semaphore
            .acquire()
            .await
            .map_err(|_| AcpError::transport("prompt serialization lock closed"))?;
        let started = self.inner.get_started_state().await?;
        let (done_tx, done_rx) = tokio::sync::watch::channel(false);
        let task = {
            let _notify = self
                .inner
                .notification_semaphore
                .acquire()
                .await
                .map_err(|_| AcpError::transport("prompt notification lock closed"))?;
            let _dispatch = self
                .inner
                .prompt_dispatch_semaphore
                .acquire()
                .await
                .map_err(|_| AcpError::transport("prompt dispatch lock closed"))?;
            close_active_assistant_segment(&mut self.inner.update_ctx.lock(), &self.inner.events);
            *self.inner.assistant_updates_open.lock().await = true;
            let mut request = payload;
            if let Some(object) = request.as_object_mut() {
                object.insert("sessionId".to_owned(), json!(started.session_id));
            }
            let inner = Arc::clone(&self.inner);
            let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
            let task = tokio::spawn(async move {
                if release_rx.await.is_err() {
                    return Err(AcpError::transport(
                        "The ACP prompt stopped before the agent confirmed completion.",
                    ));
                }
                inner
                    .run_logged_request("session/prompt", request.clone(), async {
                        inner.client.prompt(request).await
                    })
                    .await
            });
            *self.inner.active_prompt.lock().await = Some(ActivePrompt {
                abort: task.abort_handle(),
                done: done_rx,
            });
            let _ = release_tx.send(());
            task
        };
        let mut abort_on_drop = AbortOnDrop {
            abort: Some(task.abort_handle()),
        };
        let mut retire_on_drop = PromptRetireOnDrop {
            inner: Arc::clone(&self.inner),
            armed: self.inner.options.cancel_behavior == CancelBehavior::WaitForPrompt,
        };
        let joined = task.await;
        abort_on_drop.abort.take();
        retire_on_drop.armed = false;
        *self.inner.active_prompt.lock().await = None;
        close_active_assistant_segment(&mut self.inner.update_ctx.lock(), &self.inner.events);
        let _ = done_tx.send(true);
        match joined {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(error),
            Err(join)
                if join.is_cancelled()
                    && self.inner.options.cancel_behavior != CancelBehavior::WaitForPrompt =>
            {
                Ok(json!({ "stopReason": "cancelled" }))
            }
            Err(_) => {
                let error = prompt_stopped_error(self.inner.pid);
                self.inner.retire_runtime(error.clone()).await;
                Err(error)
            }
        }
    }

    pub async fn cancel(&self) -> Result<(), AcpError> {
        if self.inner.options.cancel_behavior == CancelBehavior::WaitForPrompt {
            let _permit = self
                .inner
                .prompt_dispatch_semaphore
                .acquire()
                .await
                .map_err(|_| AcpError::transport("cancel dispatch lock closed"))?;
        }
        self.inner.cancel().await
    }

    pub async fn authenticate(&self, method_id: &str) -> Result<(), AcpError> {
        self.initialize().await?;
        self.inner
            .run_logged_request("authenticate", json!({ "methodId": method_id }), async {
                self.inner
                    .client
                    .authenticate(json!({ "methodId": method_id }))
                    .await
            })
            .await?;
        Ok(())
    }

    pub async fn rpc(&self, method: &str, payload: Value) -> Result<Value, AcpError> {
        self.inner.ensure_connected().await?;
        self.inner
            .run_logged_request(method, payload.clone(), async {
                self.inner.client.rpc(method, payload).await
            })
            .await
    }

    pub async fn notify(&self, method: &str, payload: Value) -> Result<(), AcpError> {
        self.inner.ensure_connected().await?;
        self.inner.client.ext_notify(method, payload).await
    }

    pub async fn set_config_option(
        &self,
        config_id: &str,
        value: ConfigOptionValue,
    ) -> Result<Value, AcpError> {
        self.inner.set_config_option(config_id, value).await
    }
}

impl Inner {
    async fn get_started_state(&self) -> Result<AcpSessionRuntimeStartResult, AcpError> {
        self.ensure_connected().await?;
        self.start_result
            .lock()
            .await
            .clone()
            .ok_or_else(|| AcpError::transport("ACP session runtime has not been started"))
    }

    async fn cancel(&self) -> Result<(), AcpError> {
        let started = self.get_started_state().await?;
        let mut cancel_payload = json!({ "sessionId": started.session_id });
        if let Some(meta) = &self.options.cancel_meta
            && let Some(object) = cancel_payload.as_object_mut()
        {
            object.insert("_meta".to_owned(), meta.clone());
        }
        if self.options.cancel_behavior != CancelBehavior::WaitForPrompt {
            if !self.options.interrupt_prompt_on_cancel {
                return self.client.cancel(cancel_payload).await;
            }
            if let Some(active) = self.active_prompt.lock().await.as_ref() {
                active.abort.abort();
            }
            return self.client.cancel(cancel_payload).await;
        }
        self.client.cancel(cancel_payload).await?;
        let done = self
            .active_prompt
            .lock()
            .await
            .as_ref()
            .map(|active| active.done.clone());
        let Some(mut done) = done else {
            return Ok(());
        };
        let limit = self
            .options
            .cancel_timeout
            .unwrap_or(DEFAULT_CANCEL_TIMEOUT);
        let completed = timeout(limit, async {
            if !*done.borrow() {
                let _ = done.changed().await;
            }
            if self.termination_error.lock().await.is_none() {
                let _ = self.drain_events().await;
            }
        })
        .await;
        if completed.is_err() {
            let error = cancel_timeout_error(self.pid);
            self.retire_runtime(error.clone()).await;
            return Err(error);
        }
        Ok(())
    }

    async fn ensure_connected(&self) -> Result<(), AcpError> {
        if let Some(error) = self.termination_error.lock().await.clone() {
            return Err(error);
        }
        if *self.stopping.lock().await {
            return Err(AcpError::transport("The ACP session runtime is closed."));
        }
        Ok(())
    }

    async fn initialize_once(&self) -> Result<InitializeResponse, AcpError> {
        let caps = self.options.client_capabilities.clone().unwrap_or_default();
        let request = InitializeRequest {
            protocol_version: self.options.protocol_version.unwrap_or(2),
            client_capabilities: Some(caps),
            client_info: Some(self.options.client_info.clone()),
            meta: self.options.initialize_meta.clone(),
        };
        self.run_logged_request("initialize", json!({}), async {
            self.client.initialize(request).await
        })
        .await
        .inspect(|response| {
            if let Some(on_initialized) = self.options.on_initialized.as_ref() {
                on_initialized(response.clone());
            }
        })
    }

    async fn apply_session_update_notification(&self, notification: Value) {
        let gate = self.session_load_gate.lock().await.clone();
        if let Some(gate) = gate.as_ref() {
            let loading = gate.active
                && notification.get("sessionId").and_then(Value::as_str)
                    == Some(gate.session_id.as_str());
            if loading
                && session_update_counts_as_load_replay_activity(
                    &notification,
                    Some(&gate.session_id),
                )
            {
                let now = now_millis();
                let mut guard = self.session_load_gate.lock().await;
                if let Some(current) = guard.as_mut() {
                    current.last_activity_at_millis = Some(now);
                }
            }
        }
        if session_update_is_replay(&notification) {
            return;
        }
        if *self.starting.lock().await {
            if is_startup_metadata_update(&notification) {
                let mut metadata = self.startup_metadata.lock().await;
                metadata.retain(|previous| {
                    previous.get("sessionId") != notification.get("sessionId")
                        || previous
                            .get("update")
                            .and_then(|update| update.get("sessionUpdate"))
                            != notification
                                .get("update")
                                .and_then(|update| update.get("sessionUpdate"))
                });
                metadata.push(notification);
                if metadata.len() > MAX_STARTUP_METADATA_UPDATES {
                    metadata.remove(0);
                }
            }
            return;
        }
        let Some(started) = self.start_result.lock().await.clone() else {
            return;
        };
        if notification.get("sessionId").and_then(Value::as_str)
            != Some(started.session_id.as_str())
        {
            return;
        }
        if !*self.assistant_updates_open.lock().await {
            let update_kind = notification
                .get("update")
                .and_then(|update| update.get("sessionUpdate"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if matches!(update_kind, "agent_message_chunk" | "agent_thought_chunk") {
                return;
            }
        }
        let mode_state = self.mode_state.lock().await.clone();
        let config_options = self.config_options.lock().await.clone();
        let (next_mode, next_config) = {
            let mut ctx = self.update_ctx.lock();
            ctx.mode_state = mode_state;
            ctx.config_options = config_options;
            handle_session_update(&mut ctx, &self.events, &notification);
            (ctx.mode_state.clone(), ctx.config_options.clone())
        };
        if let Some(mode_state) = next_mode {
            *self.mode_state.lock().await = Some(mode_state);
        }
        *self.config_options.lock().await = next_config;
    }

    async fn start_once(&self) -> Result<AcpSessionRuntimeStartResult, AcpError> {
        *self.starting.lock().await = true;
        let result = match self.start_once_inner().await {
            Ok(result) => result,
            Err(error) => {
                self.reset_start_attempt().await;
                return Err(error);
            }
        };
        *self.start_result.lock().await = Some(result.clone());
        *self.starting.lock().await = false;
        let metadata = std::mem::take(&mut *self.startup_metadata.lock().await);
        for notification in metadata {
            if notification.get("sessionId").and_then(Value::as_str)
                == Some(result.session_id.as_str())
            {
                self.apply_session_update_notification(notification).await;
            }
        }
        Ok(result)
    }

    async fn start_once_inner(&self) -> Result<AcpSessionRuntimeStartResult, AcpError> {
        let initialize_result = self.initialize_once().await?;
        *self.initialize_result.lock().await = Some(initialize_result.clone());

        if self.options.authenticate_eagerly {
            let method_id = self.options.auth_method_id.clone().unwrap_or_default();
            self.run_logged_request("authenticate", json!({ "methodId": method_id }), async {
                self.client
                    .authenticate(json!({ "methodId": method_id }))
                    .await
            })
            .await?;
        }

        let setup = match self.setup_session(&initialize_result).await {
            Ok(setup) => setup,
            Err(error)
                if is_acp_authentication_required(&error)
                    && self.options.authenticate_on_auth_required =>
            {
                self.authenticate_after_required(&initialize_result, &error)
                    .await?;
                self.setup_session(&initialize_result).await?
            }
            Err(error) => return Err(error),
        };

        let setup_response: AcpSessionSetupResponse =
            serde_json::from_value(setup.session_setup_result.clone()).unwrap_or_default();
        let mode_state = parse_session_mode_state(&setup_response);
        *self.mode_state.lock().await = mode_state;
        *self.config_options.lock().await =
            session_config_options_from_setup(Some(&setup.session_setup_result));

        Ok(AcpSessionRuntimeStartResult {
            session_id: setup.session_id,
            initialize_result,
            model_config_id: extract_model_config_id(&setup_response),
            session_setup_result: setup.session_setup_result,
        })
    }

    async fn setup_session(
        &self,
        initialize_result: &InitializeResponse,
    ) -> Result<SessionSetup, AcpError> {
        let mcp_servers = self.session_mcp_servers(initialize_result, None);
        let additional = self
            .options
            .additional_directories
            .as_ref()
            .filter(|dirs| !dirs.is_empty())
            .map(|dirs| json!({ "additionalDirectories": dirs }));
        if let Some(resume_id) = self.options.resume_session_id.as_deref() {
            let load_supported = initialize_result
                .agent_capabilities
                .get("loadSession")
                .and_then(Value::as_bool)
                == Some(true);
            if load_supported && self.options.resume_method != Some(ResumeMethod::Resume) {
                let mut payload = json!({
                    "sessionId": resume_id,
                    "cwd": self.options.cwd.to_string_lossy(),
                    "mcpServers": mcp_servers,
                });
                merge_additional_into_payload(&mut payload, additional.as_ref());
                let response = self
                    .run_load_session_with_replay_idle(payload, initialize_result)
                    .await?;
                return Ok(SessionSetup {
                    session_id: resume_id.to_owned(),
                    session_setup_result: response,
                });
            }
            let mut payload = json!({
                "sessionId": resume_id,
                "cwd": self.options.cwd.to_string_lossy(),
                "mcpServers": mcp_servers,
            });
            merge_additional_into_payload(&mut payload, additional.as_ref());
            if !session_resume_advertised(initialize_result) {
                return Err(AcpError::from(AcpRequestError::method_unavailable(
                    "ACP agent does not advertise session/load or session/resume support",
                )));
            }
            let response = self
                .run_logged_request("session/resume", payload.clone(), async {
                    self.client.resume_session(payload).await
                })
                .await?;
            return Ok(SessionSetup {
                session_id: resume_id.to_owned(),
                session_setup_result: response,
            });
        }
        let mut payload = json!({
            "cwd": self.options.cwd.to_string_lossy(),
            "mcpServers": mcp_servers,
        });
        merge_additional_into_payload(&mut payload, additional.as_ref());
        let response = self
            .run_logged_request("session/new", payload.clone(), async {
                self.client.create_session(payload).await
            })
            .await?;
        let session_id = response
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        Ok(SessionSetup {
            session_id,
            session_setup_result: response,
        })
    }

    async fn authenticate_after_required(
        &self,
        initialize_result: &InitializeResponse,
        auth_required_error: &AcpError,
    ) -> Result<(), AcpError> {
        let auth_methods = auth_methods_from_initialize(initialize_result);
        let configured = self.options.auth_method_id.as_deref();
        let auth_method = select_acp_agent_auth_method(Some(&auth_methods), configured);
        if configured.is_some()
            && !initialize_result.auth_methods.is_empty()
            && auth_method.is_none()
        {
            return Err(AcpError::transport(format!(
                "ACP agent did not advertise configured authentication method \"{}\"",
                configured.unwrap_or_default()
            )));
        }
        if let Some(method) = auth_method.as_ref()
            && let Some(method_type) = method.method_type.as_deref()
            && method_type != "agent"
        {
            return Err(AcpError::transport(format!(
                "ACP authentication method \"{}\" requires {method_type} authentication, which cannot run inside a headless provider session",
                method.id
            )));
        }
        let method_id = auth_method
            .map(|method| method.id)
            .or_else(|| configured.map(str::to_owned));
        let Some(method_id) = method_id else {
            return Err(auth_required_error.clone());
        };
        self.run_logged_request("authenticate", json!({ "methodId": method_id }), async {
            self.client
                .authenticate(json!({ "methodId": method_id }))
                .await
        })
        .await?;
        Ok(())
    }

    fn session_mcp_servers(
        &self,
        initialize_result: &InitializeResponse,
        activation: Option<&AcpSessionActivationOptions>,
    ) -> Value {
        let acp_servers = activation
            .and_then(|options| options.acp_mcp_servers.clone())
            .or_else(|| self.options.acp_mcp_servers.clone())
            .unwrap_or_else(|| json!([]));
        let mcp_acp = initialize_result
            .agent_capabilities
            .get("mcpCapabilities")
            .and_then(|value| value.get("acp"))
            .and_then(Value::as_bool)
            == Some(true);
        if mcp_acp
            && acp_servers
                .as_array()
                .is_some_and(|servers| !servers.is_empty())
        {
            return acp_servers;
        }
        activation
            .and_then(|options| options.mcp_servers.clone())
            .or_else(|| self.options.mcp_servers.clone())
            .unwrap_or_else(|| json!([]))
    }

    async fn run_load_session_with_replay_idle(
        &self,
        load_payload: Value,
        initialize_result: &InitializeResponse,
    ) -> Result<Value, AcpError> {
        let _permit = self
            .session_load_semaphore
            .acquire()
            .await
            .map_err(|_| AcpError::transport("session load lock closed"))?;
        let session_id = load_payload
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let idle_gap = self
            .options
            .session_load_replay_idle_gap
            .unwrap_or(DEFAULT_SESSION_LOAD_REPLAY_IDLE_GAP);
        *self.session_load_gate.lock().await = Some(SessionLoadGate {
            active: true,
            session_id: session_id.clone(),
            last_activity_at_millis: None,
            idle_gap_ms: idle_gap.as_millis() as u64,
            initialize_result: initialize_result_to_value(initialize_result),
            candidate_config_options: None,
        });
        let gate = Arc::clone(&self.session_load_gate);
        let idle = wait_for_session_load_replay_idle(&gate);
        let load_timeout = self
            .options
            .session_load_timeout
            .unwrap_or(DEFAULT_SESSION_LOAD_TIMEOUT);
        let loaded = timeout(load_timeout, async {
            tokio::select! {
                loaded = self.client.load_session(load_payload.clone()) => loaded,
                idle = idle => Ok(idle),
            }
        })
        .await;
        *self.session_load_gate.lock().await = None;
        loaded.map_err(|_| {
            AcpError::transport(
                "session/load timed out waiting for RPC response or replay idle gap",
            )
        })?
    }

    async fn drain_events(&self) -> Result<(), AcpError> {
        if *self.stopping.lock().await {
            return Ok(());
        }
        let notify = Arc::new(tokio::sync::Notify::new());
        let _permit = self.notification_semaphore.acquire().await.unwrap();
        if self.active_prompt.lock().await.is_none() {
            *self.assistant_updates_open.lock().await = false;
            close_active_assistant_segment(&mut self.update_ctx.lock(), &self.events);
        }
        if self.events.receiver_count() == 0 {
            return Ok(());
        }
        let _ = self
            .events
            .send(AcpSessionRuntimeEvent::EventStreamBarrier {
                acknowledge: Arc::clone(&notify),
            });
        tokio::select! {
            _ = notify.notified() => {}
            _ = self.runtime_closed.notified() => {}
        }
        Ok(())
    }

    async fn terminate_process_group(&self) -> Result<(), AcpProcessGroupTerminationError> {
        if !self.options.own_detached_process_group {
            return Ok(());
        }
        let platform = self
            .options
            .process_group_platform
            .as_deref()
            .unwrap_or(std::env::consts::OS);
        if platform == "win32" {
            return Err(AcpProcessGroupTerminationError::new(
                "Windows ACP process-group termination is not supported on Turin",
            ));
        }
        let grace = self
            .options
            .process_group_termination_grace
            .unwrap_or(Duration::from_secs(1));
        self.observe_stop.store(true, Ordering::SeqCst);
        if let Some(lease) = &self.cgroup_lease {
            if let Some(controller) = &self.posix_controller {
                let root = self.ownership_root.lock().clone();
                signal_linux_cgroup_root_term(controller.as_ref(), lease.as_ref(), &root);
            }
            if !grace.is_zero() {
                sleep(grace).await;
            }
            return terminate_linux_cgroup_lease(lease.as_ref()).await;
        }
        if self.options.own_descendant_process_groups {
            let controller = match &self.posix_controller {
                Some(controller) => Arc::clone(controller),
                None => Arc::from(
                    make_posix_process_tree_controller(platform)
                        .map_err(|error| AcpProcessGroupTerminationError::new(error.to_string()))?,
                ),
            };
            let mut ledger = std::mem::take(&mut *self.ownership_ledger.lock());
            let mut root = std::mem::take(&mut *self.ownership_root.lock());
            return terminate_posix_owned_process_tree(TerminatePosixOwnedProcessTreeInput {
                controller: controller.as_ref(),
                grace: Some(grace),
                ledger: Some(&mut ledger),
                root: Some(&mut root),
                root_pid: self.pid as i32,
                discovery_passes: None,
            })
            .await;
        }
        match signal_process_group(i64::from(self.pid), libc::SIGTERM) {
            Ok(()) => {}
            Err(error) if is_esrch(&error) => return Ok(()),
            Err(error) => {
                return Err(AcpProcessGroupTerminationError::new(error.to_string()));
            }
        }
        if !grace.is_zero() {
            sleep(grace).await;
        }
        match signal_process_group(i64::from(self.pid), libc::SIGKILL) {
            Ok(()) => Ok(()),
            Err(error) if is_esrch(&error) => Ok(()),
            Err(error) => Err(AcpProcessGroupTerminationError::new(error.to_string())),
        }
    }

    async fn run_logged_request<T, Fut>(
        &self,
        method: &str,
        payload: Value,
        effect: Fut,
    ) -> Result<T, AcpError>
    where
        Fut: Future<Output = Result<T, AcpError>>,
    {
        if let Some(logger) = self.options.request_logger.as_ref() {
            logger(RequestLogEvent {
                method: method.to_owned(),
                payload: payload.clone(),
                status: RequestLogStatus::Started,
                result: None,
                error: None,
            });
        }
        let result = effect.await;
        if let Some(logger) = self.options.request_logger.as_ref() {
            match &result {
                Ok(_value) => logger(RequestLogEvent {
                    method: method.to_owned(),
                    payload,
                    status: RequestLogStatus::Succeeded,
                    result: None,
                    error: None,
                }),
                Err(error) => logger(RequestLogEvent {
                    method: method.to_owned(),
                    payload,
                    status: RequestLogStatus::Failed,
                    result: None,
                    error: Some(error.to_string()),
                }),
            }
        }
        result
    }

    async fn set_config_option(
        &self,
        config_id: &str,
        value: ConfigOptionValue,
    ) -> Result<Value, AcpError> {
        let config_options = self.config_options.lock().await.clone();
        if let Some(option) = find_config_option_value(&config_options, config_id) {
            let option_type = option.get("type").and_then(Value::as_str).unwrap_or("");
            if option_type == "boolean" {
                let ConfigOptionValue::Boolean(_) = value else {
                    return Err(invalid_config_option_value(config_id, &value, "boolean"));
                };
            } else if !matches!(value, ConfigOptionValue::String(_)) {
                return Err(invalid_config_option_value(config_id, &value, "string"));
            }
        }
        let started = self.get_started_state().await?;
        if let Some(existing) = find_config_option_value(&config_options, config_id)
            && config_option_current_value_matches(existing, &value)
        {
            return Ok(json!({ "configOptions": config_options }));
        }
        let request = match value {
            ConfigOptionValue::Boolean(boolean) => json!({
                "sessionId": started.session_id,
                "configId": config_id,
                "type": "boolean",
                "value": boolean,
            }),
            ConfigOptionValue::String(text) => json!({
                "sessionId": started.session_id,
                "configId": config_id,
                "value": text,
            }),
        };
        let request_payload = request.clone();
        let response = self
            .run_logged_request("session/set_config_option", request, async {
                self.client.set_session_config_option(request_payload).await
            })
            .await?;
        let next = session_config_options_from_setup(Some(&response));
        *self.config_options.lock().await = next.clone();
        let _ = self
            .events
            .send(AcpSessionRuntimeEvent::ConfigOptionsUpdated {
                config_options: next,
                raw_payload: response.clone(),
            });
        Ok(response)
    }

    async fn record_termination(&self, error: AcpError) {
        if *self.stopping.lock().await {
            return;
        }
        let mut slot = self.termination_error.lock().await;
        if slot.is_some() {
            return;
        }
        *slot = Some(error.clone());
        close_active_assistant_segment(&mut self.update_ctx.lock(), &self.events);
        let _ = self
            .events
            .send(AcpSessionRuntimeEvent::ConnectionTerminated { error });
    }

    async fn reset_start_attempt(&self) {
        *self.starting.lock().await = false;
        self.startup_metadata.lock().await.clear();
        *self.session_load_gate.lock().await = None;
    }

    async fn retire_runtime(&self, error: AcpError) {
        self.record_termination(error).await;
        if signal_process_group(i64::from(self.pid), libc::SIGTERM).is_ok() {
            sleep(Duration::from_secs(1)).await;
            let _ = signal_process_group(i64::from(self.pid), libc::SIGKILL);
        }
    }
}

struct SessionSetup {
    session_id: String,
    session_setup_result: Value,
}

fn merge_additional_into_payload(payload: &mut Value, additional: Option<&Value>) {
    if let (Some(additional), Some(object)) = (additional, payload.as_object_mut())
        && let Some(map) = additional.as_object()
    {
        object.extend(map.clone());
    }
}

fn find_config_option_value<'a>(config_options: &'a Value, config_id: &str) -> Option<&'a Value> {
    config_options.as_array()?.iter().find(|option| {
        option
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id == config_id)
    })
}

fn initialize_result_to_value(initialize_result: &InitializeResponse) -> Value {
    json!({
        "protocolVersion": initialize_result.protocol_version,
        "agentCapabilities": initialize_result.agent_capabilities,
        "authMethods": initialize_result.auth_methods,
        "agentInfo": initialize_result.agent_info,
        "_meta": initialize_result.meta,
    })
}

struct SpawnedContainment {
    containment: ProcessContainment,
    lease: Option<Arc<dyn AcpLinuxCgroupLease>>,
    controller: Option<Arc<dyn AcpPosixProcessTreeController>>,
    ledger: Arc<SyncMutex<HashMap<String, AcpOwnedPosixProcess>>>,
    root: Arc<SyncMutex<AcpPosixOwnershipRoot>>,
    observe_stop: Arc<AtomicBool>,
}

impl SpawnedContainment {
    fn none() -> Self {
        Self {
            containment: ProcessContainment::None,
            lease: None,
            controller: None,
            ledger: Arc::new(SyncMutex::new(HashMap::new())),
            root: Arc::new(SyncMutex::new(AcpPosixOwnershipRoot::default())),
            observe_stop: Arc::new(AtomicBool::new(false)),
        }
    }
}

fn prepare_containment(
    options: &AcpSessionRuntimeOptions,
    platform: &str,
) -> Result<SpawnedContainment, AcpError> {
    let mut spawned = SpawnedContainment::none();
    if options.own_descendant_process_groups {
        if platform == "linux"
            && let Some(lease) = make_linux_cgroup_controller().create()
        {
            spawned.lease = Some(Arc::from(lease));
        }
        match platform {
            "linux" | "macos" | "darwin" => {
                let controller = make_posix_process_tree_controller(platform).map_err(|error| {
                    AcpError::from(AcpSpawnError {
                        command: Some(options.spawn.command.clone()),
                        cause: error.to_string(),
                    })
                })?;
                spawned.controller = Some(Arc::from(controller));
            }
            other => {
                return Err(AcpError::from(AcpSpawnError {
                    command: Some(options.spawn.command.clone()),
                    cause: format!("Detached ACP descendant ownership is unsupported on {other}"),
                }));
            }
        }
        spawned.containment = if spawned.lease.is_some() {
            ProcessContainment::CgroupV2
        } else {
            ProcessContainment::ProcessLedgerReducedGuarantee
        };
    } else if options.own_detached_process_group {
        spawned.containment = ProcessContainment::ProcessGroup;
    }
    Ok(spawned)
}

fn contained_spawn_command(
    options: &AcpSessionRuntimeOptions,
    lease: Option<&dyn AcpLinuxCgroupLease>,
) -> (String, Vec<String>) {
    match lease {
        Some(lease) => {
            wrap_command_for_linux_cgroup(lease, &options.spawn.command, &options.spawn.args)
        }
        None => (options.spawn.command.clone(), options.spawn.args.clone()),
    }
}

fn spawn_ownership_observer(
    controller: Arc<dyn AcpPosixProcessTreeController>,
    ledger: Arc<SyncMutex<HashMap<String, AcpOwnedPosixProcess>>>,
    root: Arc<SyncMutex<AcpPosixOwnershipRoot>>,
    stop: Arc<AtomicBool>,
    mut child_queues: HashMap<i32, Vec<i32>>,
    mut frontier: HashMap<i32, AcpOwnedPosixProcess>,
    root_pid: i32,
) {
    tokio::spawn(async move {
        loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let started = tokio::time::Instant::now();
            let frontier_len = {
                let mut ledger = ledger.lock();
                let mut root = root.lock();
                if observe_posix_ownership_ledger(
                    &mut child_queues,
                    controller.as_ref(),
                    &mut frontier,
                    &mut ledger,
                    &mut root,
                    root_pid,
                    Some(64),
                )
                .is_err()
                {
                    break;
                }
                frontier.len()
            };
            let target: u64 = if frontier_len <= 32 {
                25
            } else if frontier_len <= 128 {
                50
            } else {
                100
            };
            let elapsed = started.elapsed().as_millis() as u64;
            sleep(Duration::from_millis(target.saturating_sub(elapsed).max(5))).await;
        }
    });
}

fn session_resume_advertised(initialize_result: &InitializeResponse) -> bool {
    initialize_result
        .agent_capabilities
        .get("sessionCapabilities")
        .and_then(|capabilities| capabilities.get("resume"))
        .is_some_and(|value| !value.is_null())
}

fn is_esrch(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(3) || error.kind() == std::io::ErrorKind::NotFound
}

fn cancel_timeout_error(pid: u32) -> AcpError {
    AcpError::from(AcpTransportError {
        operation: Some(TransportOperation::CallRpc),
        method: Some("session/cancel".to_owned()),
        detail: Some(
            "The ACP agent did not finish cancellation. Its process was stopped.".to_owned(),
        ),
        pid: Some(i64::from(pid)),
        cause: None,
    })
}

fn prompt_stopped_error(pid: u32) -> AcpError {
    AcpError::from(AcpTransportError {
        operation: None,
        method: Some("session/prompt".to_owned()),
        detail: Some("The ACP prompt stopped before the agent confirmed completion.".to_owned()),
        pid: Some(i64::from(pid)),
        cause: None,
    })
}

struct AbortOnDrop {
    abort: Option<tokio::task::AbortHandle>,
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(abort) = self.abort.take() {
            abort.abort();
        }
    }
}

struct PromptRetireOnDrop {
    inner: Arc<Inner>,
    armed: bool,
}

impl Drop for PromptRetireOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        if let Ok(mut active) = self.inner.active_prompt.try_lock() {
            *active = None;
        }
        let inner = Arc::clone(&self.inner);
        let error = prompt_stopped_error(inner.pid);
        tokio::spawn(async move {
            inner.retire_runtime(error).await;
        });
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::ImplementationInfo;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    fn test_options() -> AcpSessionRuntimeOptions {
        AcpSessionRuntimeOptions {
            spawn: AcpSpawnInput {
                command: String::new(),
                args: Vec::new(),
                cwd: None,
                env: None,
                extend_env: true,
            },
            cwd: PathBuf::from("/"),
            protocol_version: None,
            resume_session_id: None,
            resume_method: None,
            session_load_timeout: None,
            session_load_replay_idle_gap: None,
            interrupt_prompt_on_cancel: true,
            cancel_meta: None,
            initialize_meta: None,
            own_detached_process_group: false,
            own_descendant_process_groups: false,
            process_group_platform: None,
            process_group_termination_grace: None,
            cancel_behavior: CancelBehavior::Interrupt,
            cancel_timeout: None,
            client_capabilities: None,
            client_info: ImplementationInfo {
                name: "turin".to_owned(),
                version: "0".to_owned(),
            },
            auth_method_id: None,
            authenticate_eagerly: false,
            authenticate_on_auth_required: false,
            mcp_servers: None,
            acp_mcp_servers: None,
            additional_directories: None,
            custom_models: None,
            transform_session_update: None,
            on_stderr: None,
            on_initialized: None,
            on_termination: None,
            request_logger: None,
        }
    }

    fn pair() -> (crate::client::AcpClient, DuplexStream, DuplexStream) {
        let (client_read, agent_write) = tokio::io::duplex(1 << 20);
        let (agent_read, client_write) = tokio::io::duplex(1 << 20);
        (
            crate::client::AcpClient::connect(client_read, client_write),
            agent_read,
            agent_write,
        )
    }

    async fn read_value(read: &mut DuplexStream) -> Value {
        let mut line = Vec::new();
        let mut byte = [0_u8; 1];
        loop {
            let n = read.read(&mut byte).await.unwrap();
            if n == 0 || byte[0] == b'\n' {
                break;
            }
            line.push(byte[0]);
        }
        serde_json::from_slice(line.trim_ascii()).unwrap()
    }

    async fn write_value(write: &mut DuplexStream, value: Value) {
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        write.write_all(&bytes).await.unwrap();
    }

    fn v1_initialized(id: &Value, capabilities: Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": capabilities,
            }
        })
    }

    async fn start_new_session(
        runtime: &AcpSessionRuntime,
        read: &mut DuplexStream,
        write: &mut DuplexStream,
    ) {
        let pending = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.start().await }
        });
        let init = read_value(read).await;
        write_value(write, v1_initialized(&init["id"], json!({}))).await;
        let created = read_value(read).await;
        assert_eq!(created["method"], "session/new");
        write_value(
            write,
            json!({"jsonrpc":"2.0","id": created["id"], "result": {"sessionId": "sess-1"}}),
        )
        .await;
        pending.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn interrupt_cancel_aborts_the_prompt_and_returns_cancelled() {
        let (client, mut read, mut write) = pair();
        let runtime = AcpSessionRuntime::from_connected_client(client, test_options());
        start_new_session(&runtime, &mut read, &mut write).await;
        let prompting = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.prompt(json!({"prompt": []})).await }
        });
        let prompt = read_value(&mut read).await;
        assert_eq!(prompt["method"], "session/prompt");
        runtime.cancel().await.unwrap();
        let cancel = read_value(&mut read).await;
        assert_eq!(cancel["method"], "session/cancel");
        let outcome = tokio::time::timeout(Duration::from_secs(2), prompting)
            .await
            .expect("prompt waited for the agent")
            .unwrap()
            .unwrap();
        assert_eq!(outcome["stopReason"], "cancelled");
    }

    #[tokio::test]
    async fn interrupt_cancel_returns_a_session_cancel_write_failure() {
        let (client, mut read, mut write) = pair();
        let runtime = AcpSessionRuntime::from_connected_client(client, test_options());
        start_new_session(&runtime, &mut read, &mut write).await;
        drop(read);
        let error = runtime.cancel().await.unwrap_err();
        assert!(!error.to_string().is_empty());
    }

    #[tokio::test]
    async fn wait_for_prompt_cancel_retires_the_runtime_after_the_timeout() {
        let (client, mut read, mut write) = pair();
        let mut options = test_options();
        options.cancel_behavior = CancelBehavior::WaitForPrompt;
        options.cancel_timeout = Some(Duration::from_millis(40));
        let runtime = AcpSessionRuntime::from_connected_client(client, options);
        start_new_session(&runtime, &mut read, &mut write).await;
        let prompting = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.prompt(json!({"prompt": []})).await }
        });
        let prompt = read_value(&mut read).await;
        assert_eq!(prompt["method"], "session/prompt");
        let error = runtime.cancel().await.unwrap_err();
        let AcpError::Transport(error) = error else {
            panic!("expected a transport error");
        };
        assert_eq!(error.method.as_deref(), Some("session/cancel"));
        assert_eq!(
            error.detail.as_deref(),
            Some("The ACP agent did not finish cancellation. Its process was stopped.")
        );
        let again = runtime.cancel().await.unwrap_err();
        assert!(again.to_string().contains("session/cancel"));
        drop(prompting);
    }

    #[tokio::test]
    async fn start_failure_clears_startup_state_including_the_load_timeout() {
        let (client, mut read, mut write) = pair();
        let mut options = test_options();
        options.resume_session_id = Some("sess".to_owned());
        options.session_load_timeout = Some(Duration::from_millis(80));
        let runtime = AcpSessionRuntime::from_connected_client(client, options);
        let pending = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.start().await }
        });
        let init = read_value(&mut read).await;
        write_value(
            &mut write,
            v1_initialized(&init["id"], json!({"loadSession": true})),
        )
        .await;
        let load = read_value(&mut read).await;
        assert_eq!(load["method"], "session/load");
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "sess",
                    "update": {"sessionUpdate": "current_mode_update", "currentModeId": "stale"}
                }
            }),
        )
        .await;
        let error = tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .expect("start hung")
            .unwrap()
            .unwrap_err();
        let AcpError::Transport(transport) = &error else {
            panic!("expected the load timeout, got {error}");
        };
        assert!(
            transport
                .detail
                .as_deref()
                .unwrap_or("")
                .contains("timed out"),
            "{transport:?}"
        );
        assert!(!*runtime.inner.starting.lock().await);
        assert!(runtime.inner.startup_metadata.lock().await.is_empty());
        assert!(runtime.inner.session_load_gate.lock().await.is_none());
    }

    #[tokio::test]
    async fn resume_is_skipped_unless_the_agent_advertises_it() {
        let (client, mut read, mut write) = pair();
        let mut options = test_options();
        options.resume_session_id = Some("sess".to_owned());
        let runtime = AcpSessionRuntime::from_connected_client(client, options);
        let pending = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.start().await }
        });
        let init = read_value(&mut read).await;
        write_value(&mut write, v1_initialized(&init["id"], json!({}))).await;
        let error = pending.await.unwrap().unwrap_err();
        let AcpError::Request(error) = error else {
            panic!("expected a request error, got {error}");
        };
        assert_eq!(error.code, -32601);
        assert!(error.error_message.contains("session/resume"));
    }

    #[tokio::test]
    async fn resume_is_sent_when_session_capabilities_advertise_it() {
        let (client, mut read, mut write) = pair();
        let mut options = test_options();
        options.resume_session_id = Some("sess".to_owned());
        let runtime = AcpSessionRuntime::from_connected_client(client, options);
        let pending = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.start().await }
        });
        let init = read_value(&mut read).await;
        write_value(
            &mut write,
            v1_initialized(&init["id"], json!({"sessionCapabilities": {"resume": {}}})),
        )
        .await;
        let resume = read_value(&mut read).await;
        assert_eq!(resume["method"], "session/resume");
        assert_eq!(resume["params"]["sessionId"], "sess");
        write_value(
            &mut write,
            json!({"jsonrpc":"2.0","id": resume["id"], "result": {"sessionId": "sess"}}),
        )
        .await;
        pending.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn esrch_skips_the_grace_period_and_further_signals() {
        let mut options = test_options();
        options.own_detached_process_group = true;
        options.process_group_termination_grace = Some(Duration::from_secs(5));
        let runtime = AcpSessionRuntime::from_connected_client(
            crate::client::AcpClient::connect(tokio::io::duplex(16).0, tokio::io::duplex(16).1),
            options,
        );
        let started = tokio::time::Instant::now();
        runtime.terminate_process_group().await.unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "ESRCH still waited out the grace period"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_and_darwin_build_a_posix_process_tree_controller() {
        for platform in ["macos", "darwin"] {
            let controller = make_posix_process_tree_controller(platform).unwrap();
            let me = std::process::id() as i32;
            let identity = controller.identity(me).expect("current process");
            assert_eq!(identity.pid, me);
            assert!(identity.sid > 0);
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn linux_without_a_cgroup_lease_falls_back_to_the_ledger() {
        let mut options = test_options();
        options.own_descendant_process_groups = true;
        options.spawn.command = "agent".to_owned();
        let spawned = prepare_containment(&options, "linux").unwrap();
        assert!(spawned.lease.is_none());
        assert!(spawned.controller.is_some());
        assert_eq!(
            spawned.containment,
            ProcessContainment::ProcessLedgerReducedGuarantee
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_descendant_ownership_terminates_through_the_posix_ledger() {
        let dir = std::env::temp_dir().join(format!(
            "turin-acp-ledger-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("child.pid");
        let script = format!(
            "python3 -c 'import os,sys,time\npath=sys.argv[1]\npid=os.fork()\nif pid==0:\n os.setsid()\n os.execv(\"/bin/sleep\",[\"sleep\",\"30\"])\nopen(path,\"w\").write(str(pid))\ntime.sleep(30)\n' '{}'",
            pid_file.display()
        );
        let mut options = test_options();
        options.spawn.command = "/bin/sh".to_owned();
        options.spawn.args = vec!["-c".to_owned(), script];
        options.own_descendant_process_groups = true;
        options.own_detached_process_group = true;
        options.process_group_platform = Some("macos".to_owned());
        options.process_group_termination_grace = Some(Duration::from_millis(20));
        let runtime = AcpSessionRuntime::make(options).await.unwrap();
        assert_eq!(
            runtime.process_containment(),
            ProcessContainment::ProcessLedgerReducedGuarantee
        );
        let grandchild = {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                if let Ok(text) = std::fs::read_to_string(&pid_file)
                    && let Ok(pid) = text.trim().parse::<i32>()
                {
                    break pid;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "grandchild never started"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::sleep(Duration::from_millis(150)).await;
        runtime.terminate_process_group().await.unwrap();
        let mut gone = false;
        for _ in 0..100 {
            if unsafe { libc::kill(grandchild, 0) } != 0 {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        drop(runtime);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(gone, "grandchild {grandchild} survived ledger termination");
    }
}
