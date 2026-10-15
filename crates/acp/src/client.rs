//! Port of upstream/packages/effect-acp/src/client.ts
//!
//! Extension requests and extension notifications are separate tables, matching
//! `extRequestHandlers` and `extNotificationHandlers`. A JSON-RPC request is
//! never delivered to a notification handler.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::compat::{
    ClientCapabilities, ImplementationInfo, InitializeRequest, InitializeResponse,
    ProtocolGeneration, idle_prompt_response, negotiating_initialize_params,
    normalize_initialize_response, normalize_permission_request, normalize_session_notification,
    normalize_v2_session_setup, v2_config_option_params, with_v2_mcp_servers,
};
use crate::cursor::{
    self, AskQuestionParams, AskQuestionResult, CreatePlanParams, CreatePlanResult,
    GenerateImageParams, ListAvailableModelsResult, TaskParams, UpdateTodosParams,
};
use crate::errors::{AcpError, AcpRequestError, SchemaIssue};
use crate::meta::{self, agent, client as client_methods, v1_agent, v1_client};
use crate::protocol::{
    Connection, ConnectionConfig, IncomingNotification, NotificationCallback, RequestCallback,
    RequestContext,
};
use crate::schema::is_create_elicitation_request;

pub type RequestHandler =
    Arc<dyn Fn(Value, RequestContext) -> BoxFuture<'static, Result<Value, AcpError>> + Send + Sync>;
pub type NotifyHandler =
    Arc<dyn Fn(Value) -> BoxFuture<'static, Result<(), AcpError>> + Send + Sync>;
type UnknownNotificationHandler =
    Arc<dyn Fn(String, Value) -> BoxFuture<'static, Result<(), AcpError>> + Send + Sync>;
type PromptWaiters = HashMap<String, (u64, oneshot::Sender<Result<Value, AcpError>>)>;

#[derive(Default)]
struct Handlers {
    core: HashMap<String, RequestHandler>,
    ext_requests: HashMap<String, RequestHandler>,
    ext_notifications: HashMap<String, NotifyHandler>,
    unknown_ext_request: Option<RequestHandler>,
    unknown_ext_notification: Option<UnknownNotificationHandler>,
    session_updates: Arc<ReplayQueue>,
    elicitation_complete: Arc<ReplayQueue>,
}

/// Bound on elicitation completions buffered before a handler registers. Upstream
/// leaves its buffer unbounded; Turin drops the oldest (see PORTING.md Deviations).
const MAX_BUFFERED_ELICITATIONS: usize = 32;

/// Notification handlers plus the notifications that arrived before any handler
/// was registered. Replay and live delivery share one FIFO and a single drainer,
/// so they cannot interleave or reorder. Upstream flushes inline in the
/// registering fiber (client.ts handleSessionUpdate), and live updates arriving
/// during the flush queue behind it.
#[derive(Default)]
struct ReplayQueue {
    state: Mutex<ReplayState>,
}

#[derive(Default)]
struct ReplayState {
    handlers: Vec<NotifyHandler>,
    pending: VecDeque<Value>,
    draining: bool,
}

impl ReplayQueue {
    /// Queues one notification, then drains unless a drain is already running.
    /// `capacity` bounds the pre-registration buffer, dropping the oldest entry.
    async fn deliver(&self, params: Value, capacity: Option<usize>) {
        let start = {
            let mut state = self.state.lock();
            if let Some(capacity) = capacity
                && state.pending.len() >= capacity
            {
                state.pending.pop_front();
            }
            state.pending.push_back(params);
            claim_drain(&mut state)
        };
        if start {
            self.drain().await;
        }
    }

    /// Adds a handler and replays anything buffered. The replay runs on a
    /// spawned task because registration is synchronous. Without a runtime the
    /// buffer waits for the next delivery, which drains it first.
    fn register(self: Arc<Self>, handler: NotifyHandler) {
        let start = {
            let mut state = self.state.lock();
            state.handlers.push(handler);
            claim_drain(&mut state)
        };
        if !start {
            return;
        }
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move { self.drain().await });
            }
            Err(_) => self.state.lock().draining = false,
        }
    }

    async fn drain(&self) {
        loop {
            let next = {
                let mut state = self.state.lock();
                match state.pending.pop_front() {
                    Some(params) => (state.handlers.clone(), params),
                    None => {
                        state.draining = false;
                        return;
                    }
                }
            };
            run_notify_handlers(&next.0, next.1).await;
        }
    }
}

/// Marks a drain as running when there is work and a handler to run it.
fn claim_drain(state: &mut ReplayState) -> bool {
    if state.draining || state.handlers.is_empty() || state.pending.is_empty() {
        return false;
    }
    state.draining = true;
    true
}

#[derive(Clone)]
pub struct AcpClient {
    connection: Connection,
    handlers: Arc<Mutex<Handlers>>,
    generation: Arc<Mutex<Option<ProtocolGeneration>>>,
    prompts: Arc<Mutex<PromptWaiters>>,
    next_prompt: Arc<AtomicU64>,
    shutdown: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

impl Drop for AcpClient {
    fn drop(&mut self) {
        // Signal the child process group before the connection tasks are aborted.
        drop(self.shutdown.take());
    }
}

impl AcpClient {
    pub fn connect<R, W>(read: R, write: W) -> Self
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        Self::connect_with(read, write, None, None)
    }

    pub fn connect_with<R, W>(
        read: R,
        write: W,
        termination_on_eof: Option<
            Arc<dyn Fn() -> futures::future::BoxFuture<'static, AcpError> + Send + Sync>,
        >,
        shutdown: Option<Arc<dyn std::any::Any + Send + Sync>>,
    ) -> Self
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let handlers = Arc::new(Mutex::new(Handlers::default()));
        let generation = Arc::new(Mutex::new(None));
        let prompts = Arc::new(Mutex::new(HashMap::new()));
        let on_request = client_request_callback(Arc::clone(&handlers));
        let on_notification =
            client_notification_callback(Arc::clone(&handlers), Arc::clone(&prompts));
        let prompts_for_end = Arc::clone(&prompts);
        let transform_generation = Arc::clone(&generation);
        let connection = Connection::start(
            read,
            write,
            ConnectionConfig {
                on_request,
                on_notification,
                on_termination: Arc::new(move |error| fail_prompts(&prompts_for_end, error)),
                termination_on_eof,
                transform_session_update: Arc::new(move |value| {
                    normalize_session_notification(value, *transform_generation.lock())
                }),
            },
        );
        Self {
            connection,
            handlers,
            generation,
            prompts,
            next_prompt: Arc::new(AtomicU64::new(1)),
            shutdown,
        }
    }

    pub async fn initialize(
        &self,
        request: InitializeRequest,
    ) -> Result<InitializeResponse, AcpError> {
        let params = negotiating_initialize_params(&request);
        let response = self
            .connection
            .rpc_request(agent::INITIALIZE, params)
            .await?;
        let normalized = normalize_initialize_response(response);
        *self.generation.lock() = Some(normalized.generation);
        Ok(normalized)
    }

    pub async fn authenticate(&self, payload: Value) -> Result<Value, AcpError> {
        let method = if self.is_v1() {
            v1_agent::AUTHENTICATE
        } else {
            agent::AUTH_LOGIN
        };
        self.connection.rpc_request(method, payload).await
    }

    pub async fn logout(&self, payload: Value) -> Result<Value, AcpError> {
        let method = if self.is_v1() {
            v1_agent::LOGOUT
        } else {
            agent::AUTH_LOGOUT
        };
        self.connection.rpc_request(method, payload).await
    }

    pub async fn create_session(&self, payload: Value) -> Result<Value, AcpError> {
        let payload = if self.is_v1() {
            payload
        } else {
            with_v2_mcp_servers(payload)
        };
        let response = self
            .connection
            .rpc_request(agent::SESSION_NEW, payload)
            .await?;
        Ok(if self.is_v1() {
            response
        } else {
            normalize_v2_session_setup(&response)
        })
    }

    pub async fn load_session(&self, payload: Value) -> Result<Value, AcpError> {
        if self.is_v1() {
            return self
                .connection
                .rpc_request(v1_agent::SESSION_LOAD, payload)
                .await;
        }
        let mut payload = with_v2_mcp_servers(payload);
        if let Some(object) = payload.as_object_mut() {
            object.insert("replayFrom".to_owned(), json!({"type": "start"}));
        }
        let response = self
            .connection
            .rpc_request(agent::SESSION_RESUME, payload)
            .await?;
        Ok(normalize_v2_session_setup(&response))
    }

    pub async fn list_sessions(&self, payload: Value) -> Result<Value, AcpError> {
        self.connection
            .rpc_request(agent::SESSION_LIST, payload)
            .await
    }

    pub async fn fork_session(&self, payload: Value) -> Result<Value, AcpError> {
        let payload = if self.is_v1() {
            payload
        } else {
            with_v2_mcp_servers(payload)
        };
        let response = self
            .connection
            .rpc_request(agent::SESSION_FORK, payload)
            .await?;
        Ok(if self.is_v1() {
            response
        } else {
            normalize_v2_session_setup(&response)
        })
    }

    pub async fn resume_session(&self, payload: Value) -> Result<Value, AcpError> {
        let payload = if self.is_v1() {
            payload
        } else {
            with_v2_mcp_servers(payload)
        };
        let response = self
            .connection
            .rpc_request(agent::SESSION_RESUME, payload)
            .await?;
        Ok(if self.is_v1() {
            response
        } else {
            normalize_v2_session_setup(&response)
        })
    }

    pub async fn close_session(&self, payload: Value) -> Result<Value, AcpError> {
        self.connection
            .rpc_request(agent::SESSION_CLOSE, payload)
            .await
    }

    pub async fn delete_session(&self, payload: Value) -> Result<Value, AcpError> {
        if self.is_v1() {
            return Err(AcpError::from(AcpRequestError::method_not_found(
                agent::SESSION_DELETE,
            )));
        }
        self.connection
            .rpc_request(agent::SESSION_DELETE, payload)
            .await
    }

    pub async fn list_providers(&self, payload: Value) -> Result<Value, AcpError> {
        self.v2_only(agent::PROVIDERS_LIST, payload).await
    }

    pub async fn set_provider(&self, payload: Value) -> Result<Value, AcpError> {
        self.v2_only(agent::PROVIDERS_SET, payload).await
    }

    pub async fn disable_provider(&self, payload: Value) -> Result<Value, AcpError> {
        self.v2_only(agent::PROVIDERS_DISABLE, payload).await
    }

    pub async fn set_session_model(&self, payload: Value) -> Result<Value, AcpError> {
        if self.is_v1() {
            self.connection
                .rpc_request(v1_agent::SESSION_SET_MODEL, payload)
                .await
        } else {
            Err(AcpError::from(AcpRequestError::method_not_found(
                v1_agent::SESSION_SET_MODEL,
            )))
        }
    }

    pub async fn set_session_mode(&self, payload: Value) -> Result<Value, AcpError> {
        if self.is_v1() {
            self.connection
                .rpc_request(v1_agent::SESSION_SET_MODE, payload)
                .await
        } else {
            Err(AcpError::from(AcpRequestError::method_not_found(
                v1_agent::SESSION_SET_MODE,
            )))
        }
    }

    pub async fn set_session_config_option(&self, payload: Value) -> Result<Value, AcpError> {
        if self.is_v1() {
            return self
                .connection
                .rpc_request(agent::SESSION_SET_CONFIG_OPTION, payload)
                .await;
        }
        let response = self
            .connection
            .rpc_request(
                agent::SESSION_SET_CONFIG_OPTION,
                v2_config_option_params(payload),
            )
            .await?;
        let options = response
            .get("configOptions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(normalize_v2_session_setup(
            &json!({"configOptions": options}),
        ))
    }

    pub async fn prompt(&self, payload: Value) -> Result<Value, AcpError> {
        if self.is_v1() {
            return self
                .connection
                .rpc_request(agent::SESSION_PROMPT, payload)
                .await;
        }
        let session_id = payload
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let token = self.next_prompt.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        {
            let mut prompts = self.prompts.lock();
            if prompts.contains_key(&session_id) {
                return Err(AcpError::from(AcpRequestError::internal(format!(
                    "ACP session '{session_id}' already has an active prompt."
                ))));
            }
            prompts.insert(session_id.clone(), (token, tx));
        }
        // Armed before the first await. A dropped future (timeout, select!,
        // abort on interrupt) must not leave the session marked busy; upstream
        // uses Effect.ensuring here.
        let _active = ActivePrompt {
            prompts: Arc::clone(&self.prompts),
            session_id,
            token,
        };
        self.connection
            .rpc_request(agent::SESSION_PROMPT, payload)
            .await?;
        match rx.await {
            Ok(result) => result,
            Err(_) => Err(self
                .connection
                .termination_error()
                .unwrap_or_else(|| AcpError::transport("ACP prompt completion closed"))),
        }
    }

    pub async fn cancel(&self, payload: Value) -> Result<(), AcpError> {
        self.connection.notify(agent::SESSION_CANCEL, payload).await
    }

    pub async fn ext_request(&self, method: &str, payload: Value) -> Result<Value, AcpError> {
        self.connection.ext_request(method, payload).await
    }

    pub async fn ext_notify(&self, method: &str, payload: Value) -> Result<(), AcpError> {
        self.connection.notify(method, payload).await
    }

    pub async fn cursor_list_available_models(
        &self,
    ) -> Result<ListAvailableModelsResult, AcpError> {
        let value = self
            .ext_request(cursor::LIST_AVAILABLE_MODELS, json!({}))
            .await?;
        serde_json::from_value(value).map_err(|_| {
            AcpError::from(crate::errors::AcpProtocolParseError {
                operation: crate::errors::ProtocolParseOperation::DecodeNotificationPayload,
                method: Some(cursor::LIST_AVAILABLE_MODELS.to_owned()),
                request_id: None,
                issue_count: Some(1),
                issue_kinds: Some(vec!["InvalidType".to_owned()]),
                maximum_path_depth: Some(0),
                cause: Some("schema validation failed".to_owned()),
            })
        })
    }

    pub fn on_request_permission<F, Fut>(&self, handler: F)
    where
        F: Fn(Value, RequestContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, AcpError>> + Send + 'static,
    {
        self.insert_core(client_methods::SESSION_REQUEST_PERMISSION, handler);
    }

    pub fn on_elicitation<F, Fut>(&self, handler: F)
    where
        F: Fn(Value, RequestContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, AcpError>> + Send + 'static,
    {
        let handler = box_request(handler);
        let mut handlers = self.handlers.lock();
        handlers.core.insert(
            client_methods::ELICITATION_CREATE.to_owned(),
            Arc::clone(&handler),
        );
        handlers.core.insert(
            v1_client::SESSION_ELICITATION.to_owned(),
            Arc::clone(&handler),
        );
        handlers
            .core
            .insert(v1_client::UNSTABLE_SESSION_ELICITATION.to_owned(), handler);
    }

    pub fn on_session_update<F, Fut>(&self, handler: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AcpError>> + Send + 'static,
    {
        let queue = Arc::clone(&self.handlers.lock().session_updates);
        queue.register(box_notify(handler));
    }

    pub fn on_elicitation_complete<F, Fut>(&self, handler: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AcpError>> + Send + 'static,
    {
        let queue = Arc::clone(&self.handlers.lock().elicitation_complete);
        queue.register(box_notify(handler));
    }

    pub fn on_ext_request<F, Fut>(&self, method: impl Into<String>, handler: F)
    where
        F: Fn(Value, RequestContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, AcpError>> + Send + 'static,
    {
        self.handlers
            .lock()
            .ext_requests
            .insert(method.into(), box_request(handler));
    }

    pub fn on_ext_notification<F, Fut>(&self, method: impl Into<String>, handler: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AcpError>> + Send + 'static,
    {
        self.handlers
            .lock()
            .ext_notifications
            .insert(method.into(), box_notify(handler));
    }

    pub fn on_unknown_ext_request<F, Fut>(&self, handler: F)
    where
        F: Fn(Value, RequestContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, AcpError>> + Send + 'static,
    {
        self.handlers.lock().unknown_ext_request = Some(box_request(handler));
    }

    pub fn on_unknown_ext_notification<F, Fut>(&self, handler: F)
    where
        F: Fn(String, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AcpError>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        self.handlers.lock().unknown_ext_notification = Some(Arc::new(move |method, params| {
            let handler = Arc::clone(&handler);
            Box::pin(async move { handler(method, params).await })
        }));
    }

    pub fn on_cursor_ask_question<F, Fut>(&self, handler: F)
    where
        F: Fn(AskQuestionParams, RequestContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<AskQuestionResult, AcpError>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        self.on_ext_request(cursor::ASK_QUESTION, move |params, ctx| {
            let handler = Arc::clone(&handler);
            async move {
                let parsed = decode_extension::<AskQuestionParams>(cursor::ASK_QUESTION, params)?;
                let result = handler(parsed, ctx).await?;
                Ok(to_result(&result))
            }
        });
    }

    pub fn on_cursor_create_plan<F, Fut>(&self, handler: F)
    where
        F: Fn(CreatePlanParams, RequestContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<CreatePlanResult, AcpError>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        self.on_ext_request(cursor::CREATE_PLAN, move |params, ctx| {
            let handler = Arc::clone(&handler);
            async move {
                let parsed = decode_extension::<CreatePlanParams>(cursor::CREATE_PLAN, params)?;
                let result = handler(parsed, ctx).await?;
                Ok(to_result(&result))
            }
        });
    }

    pub fn on_cursor_update_todos<F, Fut>(&self, handler: F)
    where
        F: Fn(UpdateTodosParams, RequestContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AcpError>> + Send + 'static,
    {
        self.on_acked_request(cursor::UPDATE_TODOS, handler);
    }

    pub fn on_cursor_task<F, Fut>(&self, handler: F)
    where
        F: Fn(TaskParams, RequestContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AcpError>> + Send + 'static,
    {
        self.on_acked_request(cursor::TASK, handler);
    }

    pub fn on_cursor_generate_image<F, Fut>(&self, handler: F)
    where
        F: Fn(GenerateImageParams, RequestContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AcpError>> + Send + 'static,
    {
        self.on_acked_request(cursor::GENERATE_IMAGE, handler);
    }

    /// Registers `cursor/update_todos`, `cursor/task`, and `cursor/generate_image`
    /// as extension *requests* that reply `{}`.
    pub fn acknowledge_cursor_side_requests<F, Fut>(&self, handler: F)
    where
        F: Fn(String, Value, RequestContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AcpError>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        for method in cursor::SIDE_REQUESTS {
            let handler = Arc::clone(&handler);
            let method_name = (*method).to_owned();
            self.on_ext_request(method_name.clone(), move |params, ctx| {
                let handler = Arc::clone(&handler);
                let method_name = method_name.clone();
                async move {
                    handler(method_name, params, ctx).await?;
                    Ok(cursor::empty_result())
                }
            });
        }
    }

    pub fn generation(&self) -> Option<ProtocolGeneration> {
        *self.generation.lock()
    }

    #[cfg(test)]
    pub(crate) fn shutdown_guard(&self) -> Option<&(dyn std::any::Any + Send + Sync)> {
        self.shutdown.as_deref()
    }

    pub async fn next_notification(&self) -> Option<IncomingNotification> {
        self.connection.next_notification().await
    }

    fn on_acked_request<T, F, Fut>(&self, method: &'static str, handler: F)
    where
        T: DeserializeOwned + Send + 'static,
        F: Fn(T, RequestContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), AcpError>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        self.on_ext_request(method, move |params, ctx| {
            let handler = Arc::clone(&handler);
            async move {
                let parsed = decode_extension::<T>(method, params)?;
                handler(parsed, ctx).await?;
                Ok(cursor::empty_result())
            }
        });
    }

    fn insert_core<F, Fut>(&self, method: &str, handler: F)
    where
        F: Fn(Value, RequestContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, AcpError>> + Send + 'static,
    {
        self.handlers
            .lock()
            .core
            .insert(method.to_owned(), box_request(handler));
    }

    fn is_v1(&self) -> bool {
        *self.generation.lock() == Some(ProtocolGeneration::V1)
    }

    async fn v2_only(&self, method: &str, payload: Value) -> Result<Value, AcpError> {
        if self.is_v1() {
            Err(AcpError::from(AcpRequestError::method_not_found(method)))
        } else {
            self.connection.rpc_request(method, payload).await
        }
    }
}

/// Removes a session's active-prompt entry when the owning `prompt` call ends,
/// by return, error or drop. The token check keeps a newer prompt's entry.
struct ActivePrompt {
    prompts: Arc<Mutex<PromptWaiters>>,
    session_id: String,
    token: u64,
}

impl Drop for ActivePrompt {
    fn drop(&mut self) {
        let mut prompts = self.prompts.lock();
        if prompts
            .get(&self.session_id)
            .is_some_and(|(current, _)| *current == self.token)
        {
            prompts.remove(&self.session_id);
        }
    }
}

impl AcpClient {
    pub fn capabilities_with_meta(meta: Value) -> ClientCapabilities {
        ClientCapabilities {
            meta: Some(meta),
            fs_read_text_file: Some(false),
            fs_write_text_file: Some(false),
            terminal: Some(false),
            ..ClientCapabilities::default()
        }
    }

    pub fn cursor_initialize(name: &str, version: &str) -> InitializeRequest {
        InitializeRequest {
            protocol_version: 1,
            client_capabilities: Some(Self::capabilities_with_meta(json!({
                "parameterizedModelPicker": true
            }))),
            client_info: Some(ImplementationInfo {
                name: name.to_owned(),
                version: version.to_owned(),
            }),
            meta: None,
        }
    }
}

fn client_request_callback(handlers: Arc<Mutex<Handlers>>) -> RequestCallback {
    Arc::new(move |method, params, context| {
        let handlers = Arc::clone(&handlers);
        Box::pin(async move { dispatch_request(&handlers, method, params, context).await })
    })
}

fn client_notification_callback(
    handlers: Arc<Mutex<Handlers>>,
    prompts: Arc<Mutex<PromptWaiters>>,
) -> NotificationCallback {
    Arc::new(move |notification| {
        let handlers = Arc::clone(&handlers);
        let prompts = Arc::clone(&prompts);
        Box::pin(async move {
            match notification {
                IncomingNotification::SessionUpdate { params } => {
                    complete_idle_prompt(&prompts, &params);
                    let queue = Arc::clone(&handlers.lock().session_updates);
                    queue.deliver(params, None).await;
                }
                IncomingNotification::ElicitationComplete { params, .. } => {
                    let queue = Arc::clone(&handlers.lock().elicitation_complete);
                    queue.deliver(params, Some(MAX_BUFFERED_ELICITATIONS)).await;
                }
                IncomingNotification::Ext { method, params } => {
                    let (specific, unknown) = {
                        let handlers = handlers.lock();
                        (
                            handlers.ext_notifications.get(&method).cloned(),
                            handlers.unknown_ext_notification.clone(),
                        )
                    };
                    let result = if let Some(handler) = specific {
                        handler(params).await
                    } else if let Some(handler) = unknown {
                        handler(method.clone(), params).await
                    } else {
                        Ok(())
                    };
                    if let Err(error) = result {
                        tracing::debug!(%error, %method, "ACP extension notification handler failed");
                    }
                }
            }
        })
    })
}

async fn dispatch_request(
    handlers: &Mutex<Handlers>,
    method: String,
    params: Value,
    context: RequestContext,
) -> Result<Value, AcpError> {
    if (method == client_methods::ELICITATION_CREATE
        || method == v1_client::SESSION_ELICITATION
        || method == v1_client::UNSTABLE_SESSION_ELICITATION)
        && !is_create_elicitation_request(&params)
    {
        return Err(AcpError::from(AcpRequestError::invalid_extension_payload(
            &method,
            &SchemaIssue::invalid_type(1),
        )));
    }
    let prepared = if method == client_methods::SESSION_REQUEST_PERMISSION {
        normalize_permission_request(params, &context)
    } else {
        params
    };
    let (handler, extension) = {
        let handlers = handlers.lock();
        if meta::is_client_core_method(&method) {
            (handlers.core.get(&method).cloned(), false)
        } else if let Some(handler) = handlers.ext_requests.get(&method).cloned() {
            (Some(handler), true)
        } else {
            (handlers.unknown_ext_request.clone(), true)
        }
    };
    let Some(handler) = handler else {
        return Err(AcpError::from(AcpRequestError::method_not_found(&method)));
    };
    let result = match handler(prepared, context).await {
        Ok(value) => value,
        Err(error) => {
            return Err(AcpError::from(AcpRequestError::from_handler(
                error, &method, extension,
            )));
        }
    };
    if method == v1_client::SESSION_ELICITATION || method == v1_client::UNSTABLE_SESSION_ELICITATION
    {
        return Ok(wrap_legacy_elicitation(result));
    }
    if matches!(
        method.as_str(),
        v1_client::FS_WRITE_TEXT_FILE | v1_client::TERMINAL_KILL | v1_client::TERMINAL_RELEASE
    ) && result.is_null()
    {
        return Ok(json!({}));
    }
    Ok(result)
}

fn wrap_legacy_elicitation(result: Value) -> Value {
    let meta = result.get("_meta").cloned();
    let mut action = result;
    if let Some(object) = action.as_object_mut() {
        object.remove("_meta");
    }
    let mut wrapped = json!({"action": action});
    if let Some(meta) = meta {
        wrapped["_meta"] = meta;
    }
    wrapped
}

fn complete_idle_prompt(prompts: &Mutex<PromptWaiters>, params: &Value) {
    let update = params.get("update");
    let idle = update.is_some_and(|update| {
        update.get("sessionUpdate").and_then(Value::as_str) == Some("state_update")
            && update.get("state").and_then(Value::as_str) == Some("idle")
    });
    if !idle {
        return;
    }
    let Some(session_id) = params.get("sessionId").and_then(Value::as_str) else {
        return;
    };
    if let Some((_, tx)) = prompts.lock().remove(session_id) {
        let _ = tx.send(Ok(idle_prompt_response(update.unwrap())));
    }
}

fn fail_prompts(prompts: &Mutex<PromptWaiters>, error: &AcpError) {
    for (_, (_, tx)) in prompts.lock().drain() {
        let _ = tx.send(Err(error.clone()));
    }
}

async fn run_notify_handlers(handlers: &[NotifyHandler], params: Value) {
    for handler in handlers {
        if let Err(error) = handler(params.clone()).await {
            tracing::debug!(%error, "ACP notification handler failed");
        }
    }
}

fn box_request<F, Fut>(handler: F) -> RequestHandler
where
    F: Fn(Value, RequestContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, AcpError>> + Send + 'static,
{
    let handler = Arc::new(handler);
    Arc::new(move |params, context| {
        let handler = Arc::clone(&handler);
        Box::pin(async move { handler(params, context).await })
    })
}

fn box_notify<F, Fut>(handler: F) -> NotifyHandler
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), AcpError>> + Send + 'static,
{
    let handler = Arc::new(handler);
    Arc::new(move |params| {
        let handler = Arc::clone(&handler);
        Box::pin(async move { handler(params).await })
    })
}

fn decode_extension<T: DeserializeOwned>(method: &str, params: Value) -> Result<T, AcpError> {
    serde_json::from_value(params).map_err(|_| {
        AcpError::from(AcpRequestError::invalid_extension_payload(
            method,
            &SchemaIssue::invalid_type(0),
        ))
    })
}

fn to_result<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or_else(|_| json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::{AskAnswer, AskQuestionOutcome, CreatePlanOutcome};
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncWriteExt, DuplexStream};

    fn pair() -> (AcpClient, DuplexStream, DuplexStream) {
        let (client_read, agent_write) = tokio::io::duplex(1 << 20);
        let (agent_read, client_write) = tokio::io::duplex(1 << 20);
        (
            AcpClient::connect(client_read, client_write),
            agent_read,
            agent_write,
        )
    }

    async fn read_value(read: &mut DuplexStream) -> Value {
        use tokio::io::AsyncReadExt;
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

    #[tokio::test]
    async fn initialize_passes_client_capabilities_meta_and_forces_protocol_version_2() {
        let (client, mut read, mut write) = pair();
        let pending = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .initialize(AcpClient::cursor_initialize("turin", "0.0.0"))
                    .await
            }
        });
        let request = read_value(&mut read).await;
        assert_eq!(request["params"]["protocolVersion"], 2);
        assert_eq!(
            request["params"]["clientCapabilities"]["_meta"]["parameterizedModelPicker"],
            true
        );
        assert_eq!(
            request["params"]["capabilities"]["_meta"]["parameterizedModelPicker"],
            true
        );
        assert_eq!(request["params"]["clientCapabilities"]["terminal"], false);
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": {
                    "protocolVersion": 1,
                    "agentCapabilities": {"loadSession": true},
                    "agentInfo": {"name": "cursor", "version": "2026.09.15"}
                }
            }),
        )
        .await;
        let response = pending.await.unwrap().unwrap();
        assert_eq!(response.generation, ProtocolGeneration::V1);
        assert_eq!(response.protocol_version, 1);
        assert_eq!(response.agent_info.unwrap()["name"], "cursor");
    }

    #[tokio::test]
    async fn cursor_extension_requests_do_not_invoke_notification_handlers() {
        let (client, mut read, mut write) = pair();
        let requests = Arc::new(AtomicUsize::new(0));
        let notifications = Arc::new(AtomicUsize::new(0));
        let requests_cb = Arc::clone(&requests);
        let notifications_cb = Arc::clone(&notifications);
        client.on_ext_notification("cursor/update_todos", move |_params| {
            let notifications_cb = Arc::clone(&notifications_cb);
            async move {
                notifications_cb.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });
        client.on_cursor_update_todos(move |params, _ctx| {
            let requests_cb = Arc::clone(&requests_cb);
            async move {
                assert_eq!(params.tool_call_id, "tool-1");
                assert!(params.merge);
                requests_cb.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "cursor/update_todos",
                "params": {
                    "toolCallId": "tool-1",
                    "merge": true,
                    "todos": [{"id": "t", "content": "Ship", "status": "pending"}]
                }
            }),
        )
        .await;
        let reply = read_value(&mut read).await;
        assert_eq!(reply["id"], 9);
        assert_eq!(reply["result"], json!({}));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(notifications.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn ask_question_reply_is_a_nested_outcome() {
        let (client, mut read, mut write) = pair();
        client.on_cursor_ask_question(|params, ctx| async move {
            assert_eq!(params.tool_call_id, "tool-9");
            assert!(!ctx.request_id.is_empty());
            Ok(AskQuestionResult::answered(vec![AskAnswer {
                question_id: params.questions[0].id.clone(),
                selected_option_ids: vec!["a".to_owned()],
            }]))
        });
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "id": "q-1",
                "method": "cursor/ask_question",
                "params": {
                    "toolCallId": "tool-9",
                    "questions": [{
                        "id": "q1",
                        "prompt": "Which?",
                        "allowMultiple": false,
                        "options": [{"id": "a", "label": "A"}]
                    }]
                }
            }),
        )
        .await;
        let reply = read_value(&mut read).await;
        assert_eq!(reply["id"], "q-1");
        assert_eq!(reply["result"]["outcome"]["outcome"], "answered");
        assert_eq!(reply["result"]["outcome"]["answers"][0]["questionId"], "q1");
        let outcome: AskQuestionOutcome =
            serde_json::from_value(reply["result"]["outcome"].clone()).unwrap();
        assert!(matches!(outcome, AskQuestionOutcome::Answered { .. }));
    }

    #[tokio::test]
    async fn create_plan_reply_nests_accepted() {
        let (client, mut read, mut write) = pair();
        client.on_cursor_create_plan(|_params, _ctx| async {
            Ok(CreatePlanResult::accepted(Some("/tmp/plan.md".to_owned())))
        });
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "cursor/create_plan",
                "params": {"toolCallId": "tool-2", "plan": "# Plan", "todos": []}
            }),
        )
        .await;
        let reply = read_value(&mut read).await;
        assert_eq!(
            reply["result"],
            json!({"outcome": {"outcome": "accepted", "planUri": "/tmp/plan.md"}})
        );
        let outcome: CreatePlanOutcome =
            serde_json::from_value(reply["result"]["outcome"].clone()).unwrap();
        assert_eq!(
            outcome,
            CreatePlanOutcome::Accepted {
                plan_uri: Some("/tmp/plan.md".to_owned())
            }
        );
    }

    #[tokio::test]
    async fn side_requests_without_a_handler_are_method_not_found() {
        let (client, mut read, mut write) = pair();
        client.on_ext_notification(cursor::TASK, |_| async { Ok(()) });
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "id": 8,
                "method": "cursor/task",
                "params": {
                    "toolCallId": "tool-3",
                    "description": "Look",
                    "prompt": "Find it",
                    "subagentType": "shell"
                }
            }),
        )
        .await;
        let reply = read_value(&mut read).await;
        assert_eq!(reply["error"]["code"], -32601);
        drop(client);
    }

    #[tokio::test]
    async fn v2_prompt_waits_for_idle_and_permission_can_arrive_first() {
        let (client, mut read, mut write) = pair();
        client.on_request_permission(|_request, _ctx| async {
            Ok(json!({"outcome": {"outcome": "selected", "optionId": "allow"}}))
        });
        let init = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .initialize(InitializeRequest {
                        protocol_version: 1,
                        client_capabilities: None,
                        client_info: Some(ImplementationInfo {
                            name: "turin".to_owned(),
                            version: "0".to_owned(),
                        }),
                        meta: None,
                    })
                    .await
            }
        });
        let request = read_value(&mut read).await;
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": {"protocolVersion": 2, "info": {"name": "mock", "version": "0"}, "capabilities": {}}
            }),
        )
        .await;
        init.await.unwrap().unwrap();
        assert_eq!(client.generation(), Some(ProtocolGeneration::V2));
        let prompt = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .prompt(json!({"sessionId": "s1", "prompt": [{"type": "text", "text": "hi"}]}))
                    .await
            }
        });
        let prompt_request = read_value(&mut read).await;
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "id": 11,
                "method": "session/request_permission",
                "params": {"sessionId": "s1", "title": "Run", "toolCall": {"toolCallId": "t", "title": "Run"}, "options": []}
            }),
        )
        .await;
        let permission_reply = read_value(&mut read).await;
        assert_eq!(permission_reply["id"], 11);
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "s1",
                    "update": {"sessionUpdate": "state_update", "state": "idle", "stopReason": "end_turn"}
                }
            }),
        )
        .await;
        write_value(
            &mut write,
            json!({"jsonrpc": "2.0", "id": prompt_request["id"], "result": {}}),
        )
        .await;
        assert_eq!(prompt.await.unwrap().unwrap()["stopReason"], "end_turn");
    }

    #[tokio::test]
    async fn aborted_prompt_releases_the_session_for_the_next_prompt() {
        let (client, mut read, mut write) = pair();
        let init = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .initialize(InitializeRequest {
                        protocol_version: 1,
                        client_capabilities: None,
                        client_info: None,
                        meta: None,
                    })
                    .await
            }
        });
        let request = read_value(&mut read).await;
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": {"protocolVersion": 2, "info": {"name": "mock", "version": "0"}, "capabilities": {}}
            }),
        )
        .await;
        init.await.unwrap().unwrap();
        let first = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .prompt(json!({"sessionId": "s1", "prompt": []}))
                    .await
            }
        });
        let _first_request = read_value(&mut read).await;
        // Dropping the future is what a timeout, select! or interrupt does.
        first.abort();
        let _ = first.await;
        let second = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .prompt(json!({"sessionId": "s1", "prompt": []}))
                    .await
            }
        });
        let second_request =
            tokio::time::timeout(std::time::Duration::from_secs(2), read_value(&mut read))
                .await
                .expect("second prompt was rejected before reaching the wire");
        assert_eq!(second_request["method"], "session/prompt");
        // v2 prompts complete on the idle state_update, not the RPC reply.
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "s1",
                    "update": {"sessionUpdate": "state_update", "state": "idle", "stopReason": "end_turn"}
                }
            }),
        )
        .await;
        write_value(
            &mut write,
            json!({"jsonrpc": "2.0", "id": second_request["id"], "result": {}}),
        )
        .await;
        let second = tokio::time::timeout(std::time::Duration::from_secs(2), second)
            .await
            .expect("second prompt did not complete")
            .unwrap();
        assert_eq!(second.unwrap()["stopReason"], "end_turn");
    }

    // Needs a multi-thread runtime so the replay task and the reader can overlap.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn buffered_and_live_session_updates_replay_in_arrival_order() {
        let (client, _read, mut write) = pair();
        let update = |index: usize| {
            json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {"sessionId": format!("s{index}"), "update": {"sessionUpdate": "plan", "entries": []}}
            })
        };
        for index in 0..2_000 {
            write_value(&mut write, update(index)).await;
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while client
            .handlers
            .lock()
            .session_updates
            .state
            .lock()
            .pending
            .len()
            < 2_000
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "reader did not buffer"
            );
            tokio::task::yield_now().await;
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_cb = Arc::clone(&seen);
        client.on_session_update(move |params| {
            let seen_cb = Arc::clone(&seen_cb);
            async move {
                let index: usize = params["sessionId"].as_str().unwrap()[1..].parse().unwrap();
                seen_cb.lock().push(index);
                tokio::task::yield_now().await;
                Ok(())
            }
        });
        // Live updates arrive while the replay is still draining.
        for index in 2_000..2_050 {
            write_value(&mut write, update(index)).await;
        }
        while seen.lock().len() < 2_050 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "handler saw {} updates",
                seen.lock().len()
            );
            tokio::task::yield_now().await;
        }
        let order = seen.lock().clone();
        assert_eq!(order, (0..2_050).collect::<Vec<_>>());
    }

    #[test]
    fn transport_message_does_not_include_detail() {
        let error = crate::errors::AcpTransportError {
            operation: None,
            method: None,
            detail: Some("connection refused at a private endpoint".to_owned()),
            pid: None,
            cause: Some("connection refused at a private endpoint".to_owned()),
        };
        assert_eq!(error.to_string(), "ACP transport operation failed.");
        assert!(!error.to_string().contains("private endpoint"));
    }

    #[tokio::test]
    async fn v1_prompt_returns_the_rpc_body_and_delete_session_does_not_round_trip() {
        let (client, mut read, mut write) = pair();
        let init = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .initialize(AcpClient::cursor_initialize("turin", "0"))
                    .await
            }
        });
        let request = read_value(&mut read).await;
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": {"protocolVersion": 1, "agentCapabilities": {}}
            }),
        )
        .await;
        init.await.unwrap().unwrap();
        let prompt = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .prompt(json!({"sessionId": "s1", "prompt": []}))
                    .await
            }
        });
        let prompt_request = read_value(&mut read).await;
        write_value(
            &mut write,
            json!({"jsonrpc": "2.0", "id": prompt_request["id"], "result": {"stopReason": "end_turn"}}),
        )
        .await;
        assert_eq!(prompt.await.unwrap().unwrap()["stopReason"], "end_turn");
        let error = client
            .delete_session(json!({"sessionId": "s1"}))
            .await
            .unwrap_err();
        let AcpError::Request(error) = error else {
            panic!("expected a local method-not-found");
        };
        assert_eq!(error.code, -32601);
    }

    #[tokio::test]
    async fn malformed_elicitation_is_invalid_params_and_skips_the_handler() {
        let (client, mut read, mut write) = pair();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_cb = Arc::clone(&calls);
        client.on_elicitation(move |_params, _ctx| {
            let calls_cb = Arc::clone(&calls_cb);
            async move {
                calls_cb.fetch_add(1, Ordering::SeqCst);
                Ok(json!({"action": "accept"}))
            }
        });
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "elicitation/create",
                "params": {"mode": "url", "secret": "do-not-copy"}
            }),
        )
        .await;
        let reply = read_value(&mut read).await;
        assert_eq!(reply["error"]["code"], -32602);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(!reply.to_string().contains("do-not-copy"));
    }

    #[tokio::test]
    async fn v2_tool_call_update_becomes_unknown_and_one_handler_failure_does_not_stop_the_next() {
        let (client, _read, mut write) = pair();
        let seen = Arc::new(AtomicUsize::new(0));
        let seen_cb = Arc::clone(&seen);
        client.on_session_update(move |_params| {
            let seen_cb = Arc::clone(&seen_cb);
            async move {
                seen_cb.fetch_add(1, Ordering::SeqCst);
                Err(AcpError::transport("handler failed"))
            }
        });
        let seen_cb = Arc::clone(&seen);
        client.on_session_update(move |params| {
            let seen_cb = Arc::clone(&seen_cb);
            async move {
                assert_eq!(params["update"]["sessionUpdate"], "_t3_unknown");
                assert_eq!(params["update"]["originalSessionUpdate"], "tool_call");
                seen_cb.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });
        write_value(
            &mut write,
            json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "s1",
                    "update": {"sessionUpdate": "tool_call", "toolCallId": "t", "title": "Run"}
                }
            }),
        )
        .await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while seen.load(Ordering::SeqCst) < 2 {
            if tokio::time::Instant::now() > deadline {
                panic!("handlers observed {}", seen.load(Ordering::SeqCst));
            }
            tokio::task::yield_now().await;
        }
    }

    #[test]
    fn extension_payload_diagnostics_omit_the_rejected_value() {
        let secret = "acp-schema-payload-secret";
        let value = json!({"profile": {"token": secret}});
        let issue = crate::errors::expect_number_at(&value, &["profile", "token"]).unwrap_err();
        let error = AcpRequestError::invalid_extension_payload("x/private", &issue);
        assert_eq!(issue.maximum_path_depth, 2);
        assert!(issue.issue_kinds.iter().any(|kind| kind == "Pointer"));
        assert!(!error.to_string().contains(secret));
        assert!(!error.to_protocol_error().to_string().contains(secret));
    }
}
