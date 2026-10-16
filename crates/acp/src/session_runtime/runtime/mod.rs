//! Port of upstream `AcpSessionRuntime.ts` `make()` service.

mod events;
mod helpers;
mod session_update;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
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
use crate::errors::AcpError;
use crate::process::{SpawnCommand, spawn};
use crate::runtime_model::{
    AcpSessionModeState, AcpSessionSetupResponse, SessionLoadGate, extract_model_config_id,
    parse_session_mode_state, session_update_counts_as_load_replay_activity,
    session_update_is_replay, wait_for_session_load_replay_idle,
};
use crate::session_runtime::auth::select_acp_agent_auth_method;
use crate::session_runtime::{AcpProcessGroupTerminationError, signal_process_group};

pub use events::AcpSessionRuntimeEvent;
pub use helpers::{ConfigOptionValue, session_config_options_from_setup};

use helpers::{
    auth_methods_from_initialize, config_option_current_value_matches, invalid_config_option_value,
    is_acp_authentication_required, is_startup_metadata_update,
};
use session_update::{
    AssistantSegmentState, SessionUpdateContext, close_active_assistant_segment,
    handle_session_update,
};

const DEFAULT_SESSION_LOAD_TIMEOUT: Duration = Duration::from_secs(90);
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
    task: tokio::task::JoinHandle<Result<Value, AcpError>>,
}

struct Inner {
    options: AcpSessionRuntimeOptions,
    client: AcpClient,
    pid: u32,
    process_containment: ProcessContainment,
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
        Self::from_client_with_containment(client, options, 0, ProcessContainment::None)
    }

    pub async fn make(options: AcpSessionRuntimeOptions) -> Result<Self, AcpError> {
        if options.own_descendant_process_groups {
            let platform = options
                .process_group_platform
                .clone()
                .unwrap_or_else(|| std::env::consts::OS.to_owned());
            if platform != "linux" {
                return Err(AcpError::from(crate::errors::AcpSpawnError {
                    command: Some(options.spawn.command.clone()),
                    cause: format!(
                        "Detached ACP descendant ownership is unsupported on {platform}"
                    ),
                }));
            }
        }
        let args: Vec<&str> = options.spawn.args.iter().map(String::as_str).collect();
        let client = spawn(SpawnCommand {
            program: &options.spawn.command,
            args: &args,
            cwd: options.spawn.cwd.as_deref(),
        })
        .await?;
        let pid = client
            .shutdown_guard()
            .and_then(|guard| guard.downcast_ref::<crate::process::ProcessGuard>())
            .map(|guard| guard.pid())
            .unwrap_or(0);
        let process_containment = if options.own_descendant_process_groups {
            ProcessContainment::ProcessLedgerReducedGuarantee
        } else if options.own_detached_process_group {
            ProcessContainment::ProcessGroup
        } else {
            ProcessContainment::None
        };
        Ok(Self::from_client_with_containment(
            client,
            options,
            pid,
            process_containment,
        ))
    }

    fn from_client_with_containment(
        client: AcpClient,
        options: AcpSessionRuntimeOptions,
        pid: u32,
        process_containment: ProcessContainment,
    ) -> Self {
        let (events_tx, _events_rx) = broadcast::channel(256);
        let runtime_closed = Arc::new(tokio::sync::Notify::new());
        let assistant_item_runtime_id = Uuid::new_v4().to_string();
        let inner = Arc::new(Inner {
            options,
            client,
            pid,
            process_containment,
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
                shown_tool_call_ids: HashSet::new(),
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
        let _dispatch = self
            .inner
            .prompt_dispatch_semaphore
            .acquire()
            .await
            .map_err(|_| AcpError::transport("prompt dispatch lock closed"))?;
        let started = self.inner.get_started_state().await?;
        close_active_assistant_segment(&mut self.inner.update_ctx.lock(), &self.inner.events);
        *self.inner.assistant_updates_open.lock().await = true;
        let mut request = payload;
        if let Some(object) = request.as_object_mut() {
            object.insert("sessionId".to_owned(), json!(started.session_id));
        }
        let result = self
            .inner
            .run_logged_request("session/prompt", request.clone(), async {
                self.inner.client.prompt(request).await
            })
            .await;
        close_active_assistant_segment(&mut self.inner.update_ctx.lock(), &self.inner.events);
        result
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
            if self.options.interrupt_prompt_on_cancel
                && let Some(active) = self.active_prompt.lock().await.take()
            {
                active.task.abort();
            }
            let _ = self.client.cancel(cancel_payload).await;
            return Ok(());
        }
        self.client.cancel(cancel_payload).await
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
        let result = self.start_once_inner().await?;
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
        .await
        .map_err(|_| {
            AcpError::transport(
                "session/load timed out waiting for RPC response or replay idle gap",
            )
        })?;
        let loaded = match loaded {
            Ok(value) => value,
            Err(error) => {
                *self.session_load_gate.lock().await = None;
                return Err(error);
            }
        };
        *self.session_load_gate.lock().await = None;
        Ok(loaded)
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
        let signaled = signal_process_group(i64::from(self.pid), libc::SIGTERM).is_ok();
        if signaled && !grace.is_zero() {
            sleep(grace).await;
        }
        signal_process_group(i64::from(self.pid), libc::SIGKILL)
            .map_err(|error| AcpProcessGroupTerminationError::new(error.to_string()))?;
        Ok(())
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

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
