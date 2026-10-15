//! One `cursor-agent acp` process per Turin thread.
//!
//! `authenticate` is sent only by [`CursorSession::sign_in`]. `session/new`
//! and `session/load` that fail with authentication required stay unsigned-in.
//! Model changes and session open/load snapshot the Cursor user files they
//! rewrite. `session/cancel` interrupts the active prompt.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use turin_acp::compat::InitializeResponse;
use turin_acp::cursor::{AskQuestionParams, CreatePlanParams};
use turin_acp::protocol::RequestContext;
use turin_acp::{AcpClient, AcpError, SpawnCommand, spawn};
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::orchestration_v2::OrchestrationV2ProviderCapabilities;
use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};
use turin_contracts::provider_policy::ProviderApprovalDecision;

use crate::acp_config::{default_cursor_home, with_user_config};
use crate::auth::{self, CursorSignInStatus};
use crate::capabilities::{self, SelectionTransition};
use crate::error::{CursorError, CursorOperation};
use crate::events::{self, CursorUpdate};
use crate::extensions::{self, AskDisposition, PlanDisposition};
use crate::models::config_updates;
use crate::permissions;
use crate::settings::CursorSettings;

#[derive(Debug, Clone, PartialEq)]
pub struct CursorThread {
    pub session_id: String,
    pub raw: Value,
}

#[derive(Debug, Clone)]
pub struct PromptImage {
    pub mime_type: String,
    pub data_base64: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptOutcome {
    pub stop_reason: Option<String>,
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeReply {
    Approval(ProviderApprovalDecision),
    UserInput(AskDisposition),
    Plan(PlanDisposition),
}

#[derive(Debug, Clone, PartialEq)]
pub enum CursorEvent {
    Update(CursorUpdate),
    Approval {
        request_id: String,
        title: Option<String>,
        kind: turin_contracts::orchestration_v2::OrchestrationV2RuntimeRequestKind,
        options: Vec<turin_contracts::provider_policy::ProviderApprovalOption>,
        ask_question_fallback: bool,
    },
    UserInput {
        request_id: String,
        tool_call_id: String,
        questions: Vec<turin_contracts::orchestration_v2::OrchestrationV2UserInputQuestion>,
        raw: Value,
    },
    Plan {
        request_id: String,
        tool_call_id: String,
        markdown: String,
        steps: Vec<turin_contracts::orchestration_v2::OrchestrationV2PlanStep>,
        raw: Value,
    },
    Todos {
        raw: Value,
    },
    Task {
        raw: Value,
    },
    Image {
        raw: Value,
    },
    TurnSettled {
        stop_reason: Option<String>,
    },
}

enum Pending {
    Permission(oneshot::Sender<ProviderApprovalDecision>),
    Ask(oneshot::Sender<AskDisposition>),
    Plan(oneshot::Sender<PlanDisposition>),
}

struct SessionInner {
    client: AcpClient,
    events: mpsc::UnboundedSender<CursorEvent>,
    pending: Mutex<HashMap<String, Pending>>,
    grants: Mutex<HashSet<String>>,
    thread: Mutex<Option<CursorThread>>,
    home: Option<PathBuf>,
    image_prompts: AtomicBool,
    capabilities: Mutex<OrchestrationV2ProviderCapabilities>,
    auth: Mutex<CursorSignInStatus>,
    initialize: Mutex<Option<InitializeResponse>>,
}

impl Drop for SessionInner {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.pending.lock() {
            for (_, item) in pending.drain() {
                cancel_pending(item);
            }
        }
    }
}

fn cancel_pending(item: Pending) {
    match item {
        Pending::Permission(sender) => {
            let _ = sender.send(ProviderApprovalDecision::Cancel);
        }
        Pending::Ask(sender) => {
            let _ = sender.send(AskDisposition::Cancelled);
        }
        Pending::Plan(sender) => {
            let _ = sender.send(PlanDisposition::Cancelled);
        }
    }
}

/// Adapter surface matching `ProviderAdapterV2` for the methods Cursor ACP can serve.
#[async_trait]
pub trait CursorProviderAdapter: Send + Sync {
    fn instance_id(&self) -> &ProviderInstanceId;
    fn driver(&self) -> &ProviderDriverKind;
    async fn get_capabilities(&self) -> Result<OrchestrationV2ProviderCapabilities, CursorError>;
    async fn plan_selection_transition(
        &self,
        runtime_mode_changed: bool,
    ) -> Result<SelectionTransition, CursorError>;
    async fn open_session(&self, cwd: &Path) -> Result<CursorSession, CursorError>;
}

/// Session surface matching `ProviderSessionRuntime` for this transport.
#[async_trait]
pub trait CursorSessionRuntime: Send + Sync {
    async fn ensure_thread(&self, cwd: &Path) -> Result<CursorThread, CursorError>;
    async fn resume_thread(
        &self,
        session_id: &str,
        cwd: &Path,
    ) -> Result<CursorThread, CursorError>;
    async fn start_turn(
        &self,
        text: &str,
        images: &[PromptImage],
    ) -> Result<PromptOutcome, CursorError>;
    async fn steer_turn(&self) -> Result<(), CursorError>;
    async fn interrupt_turn(&self) -> Result<(), CursorError>;
    async fn respond_to_runtime_request(
        &self,
        request_id: &str,
        reply: RuntimeReply,
    ) -> Result<(), CursorError>;
    async fn read_thread_snapshot(&self) -> Result<Value, CursorError>;
    async fn rollback_thread(&self) -> Result<Value, CursorError>;
    async fn fork_thread(&self) -> Result<CursorThread, CursorError>;
}

#[derive(Clone)]
pub struct CursorAdapter {
    instance_id: ProviderInstanceId,
    driver: ProviderDriverKind,
    program: String,
    cursor_home: Option<PathBuf>,
}

impl CursorAdapter {
    pub fn new(instance_id: ProviderInstanceId) -> Self {
        Self {
            instance_id,
            driver: ProviderDriverKind::new("cursor").expect("cursor driver slug"),
            program: "cursor-agent".to_owned(),
            cursor_home: default_cursor_home(),
        }
    }

    pub fn from_settings(instance_id: ProviderInstanceId, settings: &CursorSettings) -> Self {
        Self {
            program: settings.agent_program().to_owned(),
            ..Self::new(instance_id)
        }
    }

    pub fn with_program(mut self, program: impl Into<String>) -> Self {
        self.program = program.into();
        self
    }

    pub fn with_cursor_home(mut self, home: Option<PathBuf>) -> Self {
        self.cursor_home = home;
        self
    }
}

#[async_trait]
impl CursorProviderAdapter for CursorAdapter {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.instance_id
    }

    fn driver(&self) -> &ProviderDriverKind {
        &self.driver
    }

    async fn get_capabilities(&self) -> Result<OrchestrationV2ProviderCapabilities, CursorError> {
        Ok(capabilities::cursor_capabilities())
    }

    async fn plan_selection_transition(
        &self,
        runtime_mode_changed: bool,
    ) -> Result<SelectionTransition, CursorError> {
        Ok(capabilities::plan_selection_transition(
            &capabilities::cursor_capabilities(),
            runtime_mode_changed,
        ))
    }

    async fn open_session(&self, cwd: &Path) -> Result<CursorSession, CursorError> {
        let client = spawn(SpawnCommand {
            program: &self.program,
            args: auth::ACP_ARGS,
            cwd: Some(cwd),
        })
        .await
        .map_err(|error| CursorError::from_acp(CursorOperation::Spawn, &error))?;
        let session = CursorSession::from_client(client, self.cursor_home.clone());
        session.initialize().await?;
        Ok(session)
    }
}

#[derive(Clone)]
pub struct CursorSession {
    inner: Arc<SessionInner>,
    events: Arc<AsyncMutex<mpsc::UnboundedReceiver<CursorEvent>>>,
}

impl CursorSession {
    pub fn from_client(client: AcpClient, cursor_home: Option<PathBuf>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let inner = Arc::new(SessionInner {
            client: client.clone(),
            events: tx,
            pending: Mutex::new(HashMap::new()),
            grants: Mutex::new(HashSet::new()),
            thread: Mutex::new(None),
            home: cursor_home,
            image_prompts: AtomicBool::new(false),
            capabilities: Mutex::new(capabilities::cursor_capabilities()),
            auth: Mutex::new(CursorSignInStatus::idle()),
            initialize: Mutex::new(None),
        });
        register_handlers(&client, &inner);
        Self {
            inner,
            events: Arc::new(AsyncMutex::new(rx)),
        }
    }

    pub async fn initialize(&self) -> Result<InitializeResponse, CursorError> {
        let response = self
            .inner
            .client
            .initialize(AcpClient::cursor_initialize(
                "turin",
                env!("CARGO_PKG_VERSION"),
            ))
            .await
            .map_err(|error| CursorError::from_acp(CursorOperation::Initialize, &error))?;
        self.inner.image_prompts.store(
            capabilities::image_prompts_supported(&response.agent_capabilities),
            Ordering::SeqCst,
        );
        *self.inner.auth.lock().expect("auth") =
            auth::status_from_auth_methods(&response.auth_methods);
        *self.inner.initialize.lock().expect("initialize") = Some(response.clone());
        self.refresh_capabilities(None);
        Ok(response)
    }

    pub async fn recv(&self) -> Option<CursorEvent> {
        self.events.lock().await.recv().await
    }

    pub fn auth_status(&self) -> CursorSignInStatus {
        self.inner.auth.lock().expect("auth").clone()
    }

    pub fn capabilities(&self) -> OrchestrationV2ProviderCapabilities {
        *self.inner.capabilities.lock().expect("capabilities")
    }

    pub fn selection_transition(&self, runtime_mode_changed: bool) -> SelectionTransition {
        capabilities::plan_selection_transition(&self.capabilities(), runtime_mode_changed)
    }

    /// Explicit browser sign-in. Never called from session open.
    pub async fn sign_in(&self) -> Result<Value, CursorError> {
        self.inner
            .client
            .authenticate(auth::authenticate_params())
            .await
            .map_err(|error| CursorError::from_acp(CursorOperation::Authenticate, &error))
    }

    pub async fn list_sessions(&self) -> Result<Value, CursorError> {
        self.call_session(CursorOperation::SessionList, |client| async move {
            client.list_sessions(json!({})).await
        })
        .await
    }

    pub async fn list_models(
        &self,
    ) -> Result<turin_acp::cursor::ListAvailableModelsResult, CursorError> {
        self.inner
            .client
            .cursor_list_available_models()
            .await
            .map_err(|error| CursorError::from_acp(CursorOperation::Models, &error))
    }

    pub async fn apply_model_selection(
        &self,
        selection: &ModelSelection,
    ) -> Result<(), CursorError> {
        let session_id = self.session_id()?;
        let catalog = self.list_models().await?;
        let updates = config_updates(&catalog.models, selection);
        let client = self.inner.client.clone();
        let home = self.inner.home.clone();
        with_user_config(home.as_deref(), || async move {
            for update in updates {
                client
                    .set_session_config_option(update.payload(&session_id))
                    .await
                    .map_err(|error| CursorError::from_acp(CursorOperation::Config, &error))?;
            }
            Ok(())
        })
        .await
    }

    pub async fn set_mode(&self, mode_id: &str) -> Result<Value, CursorError> {
        let session_id = self.session_id()?;
        let mode_id = mode_id.to_owned();
        self.inner
            .client
            .set_session_mode(json!({"sessionId": session_id, "modeId": mode_id}))
            .await
            .map_err(|error| CursorError::from_acp(CursorOperation::Config, &error))
    }

    fn session_id(&self) -> Result<String, CursorError> {
        self.inner
            .thread
            .lock()
            .expect("thread")
            .as_ref()
            .map(|thread| thread.session_id.clone())
            .ok_or_else(|| CursorError::new(CursorOperation::Prompt, "Cursor session is not open."))
    }

    fn refresh_capabilities(&self, setup: Option<&Value>) {
        let Some(initialize) = self.inner.initialize.lock().expect("initialize").clone() else {
            return;
        };
        *self.inner.capabilities.lock().expect("capabilities") =
            capabilities::negotiate_capabilities(&initialize, setup);
    }

    fn remember_thread(&self, setup: Value) -> Result<CursorThread, CursorError> {
        let session_id = setup
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                CursorError::new(
                    CursorOperation::SessionNew,
                    "Cursor session id was missing.",
                )
            })?
            .to_owned();
        self.refresh_capabilities(Some(&setup));
        let thread = CursorThread {
            session_id,
            raw: setup,
        };
        *self.inner.thread.lock().expect("thread") = Some(thread.clone());
        let mut auth = self.inner.auth.lock().expect("auth");
        auth.phase = auth::SignInPhase::Authenticated;
        auth.message = None;
        Ok(thread)
    }

    fn fail_auth(&self, operation: CursorOperation, error: AcpError) -> CursorError {
        let mapped = CursorError::from_acp(operation, &error);
        if mapped.not_signed_in {
            *self.inner.auth.lock().expect("auth") = CursorSignInStatus::not_signed_in();
        }
        mapped
    }

    async fn call_session<F, Fut>(
        &self,
        operation: CursorOperation,
        call: F,
    ) -> Result<Value, CursorError>
    where
        F: FnOnce(AcpClient) -> Fut,
        Fut: std::future::Future<Output = Result<Value, AcpError>>,
    {
        let client = self.inner.client.clone();
        match call(client).await {
            Ok(value) => Ok(value),
            Err(error) => Err(self.fail_auth(operation, error)),
        }
    }

    async fn open_with_snapshot<F, Fut>(
        &self,
        operation: CursorOperation,
        call: F,
    ) -> Result<CursorThread, CursorError>
    where
        F: FnOnce(AcpClient) -> Fut,
        Fut: std::future::Future<Output = Result<Value, AcpError>>,
    {
        let client = self.inner.client.clone();
        let home = self.inner.home.clone();
        let setup = with_user_config(home.as_deref(), || async move {
            call(client)
                .await
                .map_err(|error| CursorError::from_acp(operation, &error))
        })
        .await
        .inspect_err(|error| {
            if error.not_signed_in {
                *self.inner.auth.lock().expect("auth") = CursorSignInStatus::not_signed_in();
            }
        })?;
        self.remember_thread(setup)
    }
}

#[async_trait]
impl CursorSessionRuntime for CursorSession {
    async fn ensure_thread(&self, cwd: &Path) -> Result<CursorThread, CursorError> {
        if let Some(thread) = self.inner.thread.lock().expect("thread").clone() {
            return Ok(thread);
        }
        let cwd = cwd.display().to_string();
        self.open_with_snapshot(CursorOperation::SessionNew, |client| async move {
            client
                .create_session(json!({"cwd": cwd, "mcpServers": []}))
                .await
        })
        .await
    }

    async fn resume_thread(
        &self,
        session_id: &str,
        cwd: &Path,
    ) -> Result<CursorThread, CursorError> {
        let session_id = session_id.to_owned();
        let cwd = cwd.display().to_string();
        self.open_with_snapshot(CursorOperation::SessionLoad, |client| async move {
            client
                .load_session(json!({
                    "sessionId": session_id,
                    "cwd": cwd,
                    "mcpServers": []
                }))
                .await
        })
        .await
    }

    async fn start_turn(
        &self,
        text: &str,
        images: &[PromptImage],
    ) -> Result<PromptOutcome, CursorError> {
        if !images.is_empty() && !self.inner.image_prompts.load(Ordering::SeqCst) {
            return Err(CursorError::new(
                CursorOperation::Prompt,
                "This Cursor agent does not accept image prompts.",
            ));
        }
        let session_id = self.session_id()?;
        let mut prompt = vec![json!({"type": "text", "text": text})];
        for image in images {
            prompt.push(json!({
                "type": "image",
                "data": image.data_base64,
                "mimeType": image.mime_type,
            }));
        }
        let result = self
            .inner
            .client
            .prompt(json!({"sessionId": session_id, "prompt": prompt}))
            .await
            .map_err(|error| self.fail_auth(CursorOperation::Prompt, error))?;
        let stop_reason = result
            .get("stopReason")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let _ = self.inner.events.send(CursorEvent::TurnSettled {
            stop_reason: stop_reason.clone(),
        });
        Ok(PromptOutcome {
            stop_reason,
            raw: result,
        })
    }

    async fn steer_turn(&self) -> Result<(), CursorError> {
        // ACP has no steer RPC. Interrupt, then the next `start_turn` is the steered prompt.
        self.interrupt_turn().await
    }

    async fn interrupt_turn(&self) -> Result<(), CursorError> {
        self.cancel_waiters();
        let session_id = self.session_id()?;
        self.inner
            .client
            .cancel(json!({"sessionId": session_id}))
            .await
            .map_err(|error| CursorError::from_acp(CursorOperation::Cancel, &error))
    }

    async fn respond_to_runtime_request(
        &self,
        request_id: &str,
        reply: RuntimeReply,
    ) -> Result<(), CursorError> {
        let pending = self
            .inner
            .pending
            .lock()
            .expect("pending")
            .remove(request_id)
            .ok_or_else(|| {
                CursorError::new(
                    CursorOperation::Permission,
                    "That Cursor request is no longer waiting.",
                )
            })?;
        let matched = match (pending, reply) {
            (Pending::Permission(sender), RuntimeReply::Approval(decision)) => {
                sender.send(decision).is_ok()
            }
            (Pending::Ask(sender), RuntimeReply::UserInput(disposition)) => {
                sender.send(disposition).is_ok()
            }
            (Pending::Plan(sender), RuntimeReply::Plan(disposition)) => {
                sender.send(disposition).is_ok()
            }
            (other, _) => {
                cancel_pending(other);
                false
            }
        };
        if matched {
            Ok(())
        } else {
            Err(CursorError::new(
                CursorOperation::Permission,
                "That Cursor request is no longer waiting.",
            ))
        }
    }

    async fn read_thread_snapshot(&self) -> Result<Value, CursorError> {
        Err(CursorError::unsupported(
            CursorOperation::SessionLoad,
            "Cursor resumes with session/load. It does not return a conversation snapshot.",
        ))
    }

    async fn rollback_thread(&self) -> Result<Value, CursorError> {
        Err(CursorError::unsupported(
            CursorOperation::SessionLoad,
            "Cursor ACP cannot truncate its conversation. The app checkpoint is the rollback.",
        ))
    }

    async fn fork_thread(&self) -> Result<CursorThread, CursorError> {
        Err(CursorError::unsupported(
            CursorOperation::SessionNew,
            "Cursor ACP does not support session/fork. session/load resumes a saved session.",
        ))
    }
}

impl CursorSession {
    fn cancel_waiters(&self) {
        let pending = std::mem::take(&mut *self.inner.pending.lock().expect("pending"));
        for (_, item) in pending {
            cancel_pending(item);
        }
    }
}

fn register_handlers(client: &AcpClient, inner: &Arc<SessionInner>) {
    {
        let inner = Arc::clone(inner);
        client.on_session_update(move |params| {
            let inner = Arc::clone(&inner);
            async move {
                for update in events::parse_session_update(&params) {
                    let _ = inner.events.send(CursorEvent::Update(update));
                }
                Ok(())
            }
        });
    }
    {
        let inner = Arc::clone(inner);
        client.on_request_permission(move |params, ctx| {
            let inner = Arc::clone(&inner);
            async move { handle_permission(inner, params, ctx).await }
        });
    }
    register_ask(client, inner);
    register_plan(client, inner);
    register_side(
        client,
        inner,
        turin_acp::cursor::UPDATE_TODOS,
        SideKind::Todos,
    );
    register_side(client, inner, turin_acp::cursor::TASK, SideKind::Task);
    register_side(
        client,
        inner,
        turin_acp::cursor::GENERATE_IMAGE,
        SideKind::Image,
    );
}

async fn handle_permission(
    inner: Arc<SessionInner>,
    params: Value,
    ctx: RequestContext,
) -> Result<Value, AcpError> {
    let request = permissions::parse_permission_request(&params);
    let key = permissions::session_grant_key(&request);
    if inner.grants.lock().expect("grants").contains(&key) {
        return Ok(permissions::permission_result(
            &permissions::answer_for_decision(&request, ProviderApprovalDecision::Accept),
        ));
    }
    let (tx, rx) = oneshot::channel();
    inner
        .pending
        .lock()
        .expect("pending")
        .insert(ctx.request_id.clone(), Pending::Permission(tx));
    let _ = inner.events.send(CursorEvent::Approval {
        request_id: ctx.request_id,
        title: request.title.clone(),
        kind: permissions::request_kind(&request),
        options: permissions::approval_options(&request),
        ask_question_fallback: permissions::is_ask_question_fallback(&request),
    });
    let decision = rx.await.unwrap_or(ProviderApprovalDecision::Cancel);
    if decision == ProviderApprovalDecision::AcceptForSession {
        inner.grants.lock().expect("grants").insert(key);
    }
    Ok(permissions::permission_result(
        &permissions::answer_for_decision(&request, decision),
    ))
}

fn register_ask(client: &AcpClient, inner: &Arc<SessionInner>) {
    let inner = Arc::clone(inner);
    client.on_ext_request(turin_acp::cursor::ASK_QUESTION, move |params, ctx| {
        let inner = Arc::clone(&inner);
        async move {
            let parsed: AskQuestionParams =
                serde_json::from_value(params.clone()).map_err(|_| {
                    AcpError::from(turin_acp::errors::AcpRequestError::invalid_params(
                        "Invalid cursor/ask_question payload.",
                    ))
                })?;
            let (tx, rx) = oneshot::channel();
            inner
                .pending
                .lock()
                .expect("pending")
                .insert(ctx.request_id.clone(), Pending::Ask(tx));
            let _ = inner.events.send(CursorEvent::UserInput {
                request_id: ctx.request_id,
                tool_call_id: parsed.tool_call_id.clone(),
                questions: extensions::user_input_questions(&parsed),
                raw: params,
            });
            let disposition = rx.await.unwrap_or(AskDisposition::Cancelled);
            Ok(extensions::nested_outcome(&extensions::ask_result(
                disposition,
            )))
        }
    });
}

fn register_plan(client: &AcpClient, inner: &Arc<SessionInner>) {
    let inner = Arc::clone(inner);
    client.on_ext_request(turin_acp::cursor::CREATE_PLAN, move |params, ctx| {
        let inner = Arc::clone(&inner);
        async move {
            let parsed: CreatePlanParams =
                serde_json::from_value(params.clone()).map_err(|_| {
                    AcpError::from(turin_acp::errors::AcpRequestError::invalid_params(
                        "Invalid cursor/create_plan payload.",
                    ))
                })?;
            let (tx, rx) = oneshot::channel();
            inner
                .pending
                .lock()
                .expect("pending")
                .insert(ctx.request_id.clone(), Pending::Plan(tx));
            let _ = inner.events.send(CursorEvent::Plan {
                request_id: ctx.request_id,
                tool_call_id: parsed.tool_call_id.clone(),
                markdown: parsed.plan.clone(),
                steps: extensions::plan_steps(&parsed.todos),
                raw: params,
            });
            let disposition = rx.await.unwrap_or(PlanDisposition::Cancelled);
            Ok(extensions::nested_outcome(&extensions::plan_result(
                disposition,
            )))
        }
    });
}

#[derive(Clone, Copy)]
enum SideKind {
    Todos,
    Task,
    Image,
}

fn register_side(client: &AcpClient, inner: &Arc<SessionInner>, method: &str, kind: SideKind) {
    let inner = Arc::clone(inner);
    client.on_ext_request(method, move |params, _ctx| {
        let inner = Arc::clone(&inner);
        async move {
            let event = match kind {
                SideKind::Todos => CursorEvent::Todos { raw: params },
                SideKind::Task => CursorEvent::Task { raw: params },
                SideKind::Image => CursorEvent::Image { raw: params },
            };
            let _ = inner.events.send(event);
            Ok(json!({}))
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
    use turin_contracts::base_schemas::TrimmedNonEmptyString;
    use turin_contracts::model::ProviderOptionSelection;
    use turin_contracts::provider_policy::ProviderApprovalDecision as Decision;

    fn pair() -> (AcpClient, DuplexStream, DuplexStream) {
        let (client_read, agent_write) = tokio::io::duplex(1 << 20);
        let (agent_read, client_write) = tokio::io::duplex(1 << 20);
        (
            AcpClient::connect(client_read, client_write),
            agent_read,
            agent_write,
        )
    }

    async fn read_frame(read: &mut DuplexStream) -> Value {
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

    async fn write_frame(write: &mut DuplexStream, value: Value) {
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        write.write_all(&bytes).await.unwrap();
    }

    fn v1_initialized(id: &Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": 1,
                "agentCapabilities": {
                    "loadSession": true,
                    "promptCapabilities": {"image": {}}
                },
                "authMethods": [{"id": "cursor_login"}]
            }
        })
    }

    #[tokio::test]
    async fn session_round_trip_answers_cards_with_nested_outcomes() {
        let (client, mut read, mut write) = pair();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("acp-config.json"),
            json!({"selectedModelVariantId": "keep-me", "theme": "dark"}).to_string(),
        )
        .unwrap();
        std::fs::write(
            home.path().join("cli-config.json"),
            json!({"model": "composer-2", "permissions": {"allow": ["Shell(ls)"]}}).to_string(),
        )
        .unwrap();
        let home_path = home.path().to_path_buf();
        let peer_home = home_path.clone();
        let peer = tokio::spawn(async move {
            let init = read_frame(&mut read).await;
            assert_eq!(
                init["params"]["clientCapabilities"]["_meta"]["parameterizedModelPicker"],
                true
            );
            assert_eq!(init["params"]["clientCapabilities"]["terminal"], false);
            assert_eq!(
                init["params"]["clientCapabilities"]["fs"]["readTextFile"],
                false
            );
            write_frame(&mut write, v1_initialized(&init["id"])).await;

            let created = read_frame(&mut read).await;
            assert_eq!(created["method"], "session/new");
            assert_eq!(created["params"]["mcpServers"], json!([]));
            std::fs::write(peer_home.join("acp-config.json"), b"{}").unwrap();
            std::fs::write(
                peer_home.join("cli-config.json"),
                json!({"model": "gpt-5.5", "permissions": {"allow": ["Shell(ls)"]}}).to_string(),
            )
            .unwrap();
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": created["id"], "result": {
                    "sessionId": "sess-1",
                    "configOptions": [{"id": "model", "category": "model"}]
                }}),
            )
            .await;

            let models = read_frame(&mut read).await;
            assert_eq!(models["method"], "cursor/list_available_models");
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": models["id"], "result": {"models": [{
                    "value": "gpt-5.5",
                    "name": "GPT-5.5",
                    "configOptions": [{
                        "id": "reasoning",
                        "name": "Reasoning",
                        "type": "select",
                        "currentValue": "low",
                        "options": [{"value": "high", "name": "High"}]
                    }]
                }]}}),
            )
            .await;

            let config = read_frame(&mut read).await;
            assert_eq!(config["method"], "session/set_config_option");
            assert_eq!(config["params"]["configId"], "model");
            assert_eq!(config["params"]["value"], "gpt-5.5");
            std::fs::write(peer_home.join("acp-config.json"), b"{}").unwrap();
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": config["id"], "result": {"configOptions": []}}),
            )
            .await;
            let reasoning = read_frame(&mut read).await;
            assert_eq!(reasoning["params"]["configId"], "reasoning");
            assert_eq!(reasoning["params"]["value"], "high");
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": reasoning["id"], "result": {}}),
            )
            .await;

            let prompt = read_frame(&mut read).await;
            assert_eq!(prompt["method"], "session/prompt");
            assert_eq!(prompt["params"]["prompt"][0]["text"], "hello");
            write_frame(
                &mut write,
                json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": "sess-1",
                        "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Hi"}}
                    }
                }),
            )
            .await;
            write_frame(
                &mut write,
                json!({
                    "jsonrpc": "2.0",
                    "id": 7,
                    "method": "session/request_permission",
                    "params": {
                        "sessionId": "sess-1",
                        "toolCall": {
                            "toolCallId": "call-1",
                            "title": "Run ls",
                            "kind": "execute",
                            "rawInput": {"command": "ls"}
                        },
                        "options": [
                            {"optionId": "once", "name": "Allow once", "kind": "allow_once"},
                            {"optionId": "always", "name": "Always", "kind": "allow_always"},
                            {"optionId": "no", "name": "Reject", "kind": "reject_once"}
                        ]
                    }
                }),
            )
            .await;
            let permission = read_frame(&mut read).await;
            assert_eq!(permission["id"], 7);
            assert_eq!(permission["result"]["outcome"]["optionId"], "once");
            assert_ne!(permission["result"]["outcome"]["optionId"], "always");

            write_frame(
                &mut write,
                json!({
                    "jsonrpc": "2.0",
                    "id": 8,
                    "method": "cursor/ask_question",
                    "params": {
                        "toolCallId": "tool-q",
                        "title": "Pick",
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
            let asked = read_frame(&mut read).await;
            assert_eq!(asked["result"]["outcome"]["outcome"], "answered");
            assert_eq!(asked["result"]["outcome"]["answers"][0]["questionId"], "q1");

            write_frame(
                &mut write,
                json!({
                    "jsonrpc": "2.0",
                    "id": 9,
                    "method": "cursor/create_plan",
                    "params": {
                        "toolCallId": "tool-p",
                        "plan": "# Plan",
                        "todos": [{"id": "t1", "content": "Inspect", "status": "pending"}]
                    }
                }),
            )
            .await;
            let planned = read_frame(&mut read).await;
            assert_eq!(planned["result"]["outcome"]["outcome"], "accepted");
            assert_eq!(planned["result"]["outcome"]["planUri"], "file:///plan.md");

            for (id, method, params) in [
                (
                    10,
                    "cursor/update_todos",
                    json!({"toolCallId": "t", "todos": [], "merge": true, "extra": 1}),
                ),
                (
                    11,
                    "cursor/task",
                    json!({"toolCallId": "t", "description": "Look", "prompt": "Find", "subagentType": "explore"}),
                ),
                (
                    12,
                    "cursor/generate_image",
                    json!({"toolCallId": "t", "description": "Diagram"}),
                ),
            ] {
                write_frame(
                    &mut write,
                    json!({"jsonrpc":"2.0","id": id, "method": method, "params": params}),
                )
                .await;
                let ack = read_frame(&mut read).await;
                assert_eq!(ack["result"], json!({}));
            }
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": prompt["id"], "result": {"stopReason": "end_turn"}}),
            )
            .await;

            let again = read_frame(&mut read).await;
            assert_eq!(again["method"], "session/prompt");
            write_frame(
                &mut write,
                json!({
                    "jsonrpc": "2.0",
                    "id": 13,
                    "method": "session/request_permission",
                    "params": {
                        "sessionId": "sess-1",
                        "toolCall": {
                            "toolCallId": "call-2",
                            "title": "Run ls",
                            "kind": "execute",
                            "rawInput": {"command": "ls"}
                        },
                        "options": [
                            {"optionId": "once", "name": "Allow once", "kind": "allow_once"},
                            {"optionId": "always", "name": "Always", "kind": "allow_always"}
                        ]
                    }
                }),
            )
            .await;
            let repeated = read_frame(&mut read).await;
            assert_eq!(repeated["result"]["outcome"]["optionId"], "once");
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": again["id"], "result": {"stopReason": "end_turn"}}),
            )
            .await;
        });

        let session = CursorSession::from_client(client, Some(home_path.clone()));
        session.initialize().await.unwrap();
        const { assert!(!auth::AUTHENTICATE_ON_AUTH_REQUIRED) };
        let thread = session.ensure_thread(Path::new("/work")).await.unwrap();
        assert_eq!(thread.session_id, "sess-1");
        let acp: Value = serde_json::from_str(
            &std::fs::read_to_string(home_path.join("acp-config.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(acp["selectedModelVariantId"], "keep-me");
        assert_eq!(acp["theme"], "dark");

        let selection = ModelSelection {
            instance_id: ProviderInstanceId::new("cursor").unwrap(),
            model: TrimmedNonEmptyString::new("gpt-5.5").unwrap(),
            options: Some(vec![ProviderOptionSelection {
                id: TrimmedNonEmptyString::new("reasoning").unwrap(),
                value: turin_contracts::model::ProviderOptionSelectionValue::Str(
                    TrimmedNonEmptyString::new("high").unwrap(),
                ),
            }]),
        };
        session.apply_model_selection(&selection).await.unwrap();
        let cli: Value = serde_json::from_str(
            &std::fs::read_to_string(home_path.join("cli-config.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(cli["model"], "composer-2");

        let running = session.clone();
        let turn = tokio::spawn(async move { running.start_turn("hello", &[]).await });
        loop {
            match tokio::time::timeout(Duration::from_secs(5), session.recv())
                .await
                .unwrap()
                .unwrap()
            {
                CursorEvent::Update(_)
                | CursorEvent::Todos { .. }
                | CursorEvent::Task { .. }
                | CursorEvent::Image { .. } => {}
                CursorEvent::Approval {
                    request_id,
                    options,
                    ..
                } => {
                    assert!(
                        options
                            .iter()
                            .any(|option| option.label.as_str() == permissions::ALWAYS_ALLOW_LABEL)
                    );
                    session
                        .respond_to_runtime_request(
                            &request_id,
                            RuntimeReply::Approval(Decision::AcceptForSession),
                        )
                        .await
                        .unwrap();
                }
                CursorEvent::UserInput { request_id, .. } => {
                    session
                        .respond_to_runtime_request(
                            &request_id,
                            RuntimeReply::UserInput(AskDisposition::Answered(vec![
                                turin_acp::cursor::AskAnswer {
                                    question_id: "q1".to_owned(),
                                    selected_option_ids: vec!["a".to_owned()],
                                },
                            ])),
                        )
                        .await
                        .unwrap();
                }
                CursorEvent::Plan { request_id, .. } => {
                    session
                        .respond_to_runtime_request(
                            &request_id,
                            RuntimeReply::Plan(PlanDisposition::Accepted {
                                plan_uri: Some("file:///plan.md".to_owned()),
                            }),
                        )
                        .await
                        .unwrap();
                }
                CursorEvent::TurnSettled { stop_reason } => {
                    assert_eq!(stop_reason.as_deref(), Some("end_turn"));
                    break;
                }
            }
        }
        assert_eq!(
            turn.await.unwrap().unwrap().stop_reason.as_deref(),
            Some("end_turn")
        );
        let second = tokio::time::timeout(Duration::from_secs(5), session.start_turn("again", &[]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.stop_reason.as_deref(), Some("end_turn"));
        assert!(session.fork_thread().await.unwrap_err().unsupported);
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn auth_required_does_not_call_authenticate() {
        let (client, mut read, mut write) = pair();
        let peer = tokio::spawn(async move {
            let init = read_frame(&mut read).await;
            write_frame(&mut write, v1_initialized(&init["id"])).await;
            let created = read_frame(&mut read).await;
            assert_eq!(created["method"], "session/new");
            write_frame(
                &mut write,
                json!({
                    "jsonrpc": "2.0",
                    "id": created["id"],
                    "error": {
                        "code": -32603,
                        "message": "Authentication required. Please run 'cursor-agent login' first, then call authenticate() with methodId 'cursor_login'."
                    }
                }),
            )
            .await;
            let login = read_frame(&mut read).await;
            assert_eq!(login["method"], "authenticate");
            assert_eq!(login["params"]["methodId"], "cursor_login");
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": login["id"], "result": {}}),
            )
            .await;
        });
        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        let error = session.ensure_thread(Path::new("/work")).await.unwrap_err();
        assert!(error.not_signed_in);
        assert_eq!(session.auth_status().phase, auth::SignInPhase::NotSignedIn);
        session.sign_in().await.unwrap();
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn interrupt_sends_session_cancel() {
        let (client, mut read, mut write) = pair();
        let peer = tokio::spawn(async move {
            let init = read_frame(&mut read).await;
            write_frame(&mut write, v1_initialized(&init["id"])).await;
            let created = read_frame(&mut read).await;
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": created["id"], "result": {"sessionId": "sess-9"}}),
            )
            .await;
            let prompt = read_frame(&mut read).await;
            assert_eq!(prompt["method"], "session/prompt");
            write_frame(
                &mut write,
                json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": "sess-9",
                        "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "working"}}
                    }
                }),
            )
            .await;
            let cancel = read_frame(&mut read).await;
            assert_eq!(cancel["method"], "session/cancel");
            assert_eq!(cancel["params"]["sessionId"], "sess-9");
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": prompt["id"], "result": {"stopReason": "cancelled"}}),
            )
            .await;
        });
        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        session.ensure_thread(Path::new("/work")).await.unwrap();
        let running = session.clone();
        let turn = tokio::spawn(async move { running.start_turn("stop", &[]).await });
        match tokio::time::timeout(Duration::from_secs(5), session.recv())
            .await
            .unwrap()
            .unwrap()
        {
            CursorEvent::Update(_) => {}
            other => panic!("unexpected {other:?}"),
        }
        session.interrupt_turn().await.unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(5), turn)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(outcome.stop_reason.as_deref(), Some("cancelled"));
        peer.await.unwrap();
    }

    #[test]
    fn launch_never_passes_force_or_yolo() {
        assert!(auth::launch_is_unforced(auth::ACP_ARGS));
        assert_eq!(auth::ACP_ARGS, &["acp"]);
    }
}
