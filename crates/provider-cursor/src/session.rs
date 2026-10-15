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
use futures::stream::{self, unfold};
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use turin_acp::compat::InitializeResponse;
use turin_acp::cursor::{AskQuestionParams, CreatePlanParams};
use turin_acp::protocol::RequestContext;
use turin_acp::{AcpClient, AcpError, SpawnCommand, spawn};
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::orchestration_v2::{
    OrchestrationV2NativeRefStrength, OrchestrationV2ProviderCapabilities,
    OrchestrationV2ProviderRef, OrchestrationV2ProviderSession,
    OrchestrationV2ProviderSessionStatus, OrchestrationV2ProviderThread,
    OrchestrationV2ProviderThreadDisposition, OrchestrationV2ProviderThreadStatus,
    OrchestrationV2ResponseCapability, OrchestrationV2RuntimeRequest,
    OrchestrationV2RuntimeRequestKind, OrchestrationV2RuntimeRequestStatus,
    OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
};
use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};
use turin_contracts::provider_policy::ProviderApprovalDecision;
use turin_contracts::{
    PositiveInt, ProviderSessionId, ProviderThreadId, ProviderTurnId, RuntimeRequestId, ThreadId,
    TrimmedNonEmptyString, UtcDateTime,
};
use turin_provider_core::failure::ProviderFailureCause;
use turin_provider_core::id_allocator::derive;
use turin_provider_core::provider_adapter::{
    ProviderAdapterErrorKind, ProviderAdapterV2, ProviderAdapterV2EnsureThreadInput,
    ProviderAdapterV2Error, ProviderAdapterV2Event, ProviderAdapterV2ForkThreadInput,
    ProviderAdapterV2InterruptInput, ProviderAdapterV2OpenSessionInput,
    ProviderAdapterV2ReadThreadSnapshotInput, ProviderAdapterV2ResumeThreadInput,
    ProviderAdapterV2RollbackThreadInput, ProviderAdapterV2RuntimeRequestResponseInput,
    ProviderAdapterV2SteerInput, ProviderAdapterV2TurnInput, ProviderAdapterV2TurnTerminalStatus,
    ProviderEventStream, ProviderSessionRuntime,
};
use turin_provider_core::selection_transition::{
    ProviderSelectionTransitionInput, ProviderSelectionTransitionPlan, acp_selection_transition,
};

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

struct ActiveTurn {
    thread_id: ThreadId,
    run_id: turin_contracts::RunId,
    provider_thread_id: ProviderThreadId,
    provider_turn_id: ProviderTurnId,
    node_id: turin_contracts::NodeId,
    ordinal: std::sync::atomic::AtomicU64,
}

struct SessionInner {
    client: AcpClient,
    events: mpsc::UnboundedSender<CursorEvent>,
    provider_events: mpsc::UnboundedSender<ProviderAdapterV2Event>,
    pending: Mutex<HashMap<String, Pending>>,
    pending_kind: Mutex<HashMap<String, PendingKind>>,
    asks: Mutex<HashMap<String, turin_acp::cursor::AskQuestionParams>>,
    grants: Mutex<HashSet<String>>,
    thread: Mutex<Option<CursorThread>>,
    provider_thread: Mutex<Option<OrchestrationV2ProviderThread>>,
    active_turn: Mutex<Option<ActiveTurn>>,
    home: Option<PathBuf>,
    image_prompts: AtomicBool,
    capabilities: Mutex<OrchestrationV2ProviderCapabilities>,
    auth: Mutex<CursorSignInStatus>,
    initialize: Mutex<Option<InitializeResponse>>,
    instance_id: ProviderInstanceId,
    driver: ProviderDriverKind,
    provider_session_id: ProviderSessionId,
    provider_session: OrchestrationV2ProviderSession,
}

#[derive(Clone, Copy)]
enum PendingKind {
    Permission,
    Ask,
    Plan,
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

    pub fn instance_id(&self) -> &ProviderInstanceId {
        &self.instance_id
    }

    pub fn driver(&self) -> &ProviderDriverKind {
        &self.driver
    }
}

#[derive(Clone)]
pub struct CursorSession {
    inner: Arc<SessionInner>,
    events: Arc<AsyncMutex<mpsc::UnboundedReceiver<CursorEvent>>>,
    provider_events: Arc<Mutex<Option<mpsc::UnboundedReceiver<ProviderAdapterV2Event>>>>,
}

impl CursorSession {
    pub fn from_client(client: AcpClient, cursor_home: Option<PathBuf>) -> Self {
        let instance_id = ProviderInstanceId::new("cursor").expect("cursor instance id");
        let driver = ProviderDriverKind::new("cursor").expect("cursor driver slug");
        let provider_session_id = derive::provider_session(&instance_id);
        Self::from_client_with(
            client,
            cursor_home,
            instance_id,
            driver,
            provider_session_id,
            "/".to_owned(),
            None,
        )
    }

    fn from_client_with(
        client: AcpClient,
        cursor_home: Option<PathBuf>,
        instance_id: ProviderInstanceId,
        driver: ProviderDriverKind,
        provider_session_id: ProviderSessionId,
        cwd: String,
        model: Option<TrimmedNonEmptyString>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (provider_tx, provider_rx) = mpsc::unbounded_channel();
        let now = UtcDateTime::now();
        let provider_session = OrchestrationV2ProviderSession {
            id: provider_session_id.clone(),
            driver: driver.clone(),
            provider_instance_id: instance_id.clone(),
            status: OrchestrationV2ProviderSessionStatus::Starting,
            cwd: TrimmedNonEmptyString::new(&cwd)
                .unwrap_or_else(|_| TrimmedNonEmptyString::new("/").expect("slash cwd")),
            model,
            capabilities: capabilities::cursor_capabilities(),
            created_at: now,
            updated_at: now,
            last_error: None,
        };
        let inner = Arc::new(SessionInner {
            client: client.clone(),
            events: tx,
            provider_events: provider_tx,
            pending: Mutex::new(HashMap::new()),
            pending_kind: Mutex::new(HashMap::new()),
            asks: Mutex::new(HashMap::new()),
            grants: Mutex::new(HashSet::new()),
            thread: Mutex::new(None),
            provider_thread: Mutex::new(None),
            active_turn: Mutex::new(None),
            home: cursor_home,
            image_prompts: AtomicBool::new(false),
            capabilities: Mutex::new(capabilities::cursor_capabilities()),
            auth: Mutex::new(CursorSignInStatus::idle()),
            initialize: Mutex::new(None),
            instance_id,
            driver,
            provider_session_id,
            provider_session,
        });
        register_handlers(&client, &inner);
        Self {
            inner,
            events: Arc::new(AsyncMutex::new(rx)),
            provider_events: Arc::new(Mutex::new(Some(provider_rx))),
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

impl CursorSession {
    /// `session/new`, once per process. Later calls return the open thread.
    pub async fn new_session(&self, cwd: &Path) -> Result<CursorThread, CursorError> {
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

    /// `session/load`, Cursor's resume.
    pub async fn load_session(
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

    pub async fn prompt(
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

    /// Cancels waiting cards, then `session/cancel`.
    pub async fn cancel_turn(&self) -> Result<(), CursorError> {
        self.cancel_waiters();
        let session_id = self.session_id()?;
        self.inner
            .client
            .cancel(json!({"sessionId": session_id}))
            .await
            .map_err(|error| CursorError::from_acp(CursorOperation::Cancel, &error))
    }

    pub fn answer_request(&self, request_id: &str, reply: RuntimeReply) -> Result<(), CursorError> {
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
}

impl CursorSession {
    fn publish_session(&self) {
        let mut session = self.inner.provider_session.clone();
        session.status = OrchestrationV2ProviderSessionStatus::Ready;
        session.capabilities = self.capabilities();
        session.updated_at = UtcDateTime::now();
        let _ = self
            .inner
            .provider_events
            .send(ProviderAdapterV2Event::ProviderSessionUpdated {
                driver: self.inner.driver.clone(),
                provider_session: session,
            });
    }

    fn bind_provider_thread(
        &self,
        native_session_id: &str,
        app_thread_id: ThreadId,
    ) -> OrchestrationV2ProviderThread {
        let now = UtcDateTime::now();
        let thread = OrchestrationV2ProviderThread {
            id: derive::provider_thread(
                &self.inner.driver,
                Some(&self.inner.instance_id),
                native_session_id,
            ),
            driver: self.inner.driver.clone(),
            provider_instance_id: self.inner.instance_id.clone(),
            provider_session_id: Some(self.inner.provider_session_id.clone()),
            app_thread_id: Some(app_thread_id),
            owner_node_id: None,
            native_thread_ref: Some(OrchestrationV2ProviderRef {
                driver: self.inner.driver.clone(),
                native_id: TrimmedNonEmptyString::new(native_session_id).ok(),
                strength: OrchestrationV2NativeRefStrength::Strong,
                fingerprint: None,
                ordinal: None,
            }),
            native_conversation_head_ref: None,
            status: OrchestrationV2ProviderThreadStatus::Idle,
            first_run_ordinal: None,
            last_run_ordinal: None,
            handoff_ids: Vec::new(),
            forked_from: None,
            pending_background_tasks: Vec::new(),
            context_usage: None,
            native_metadata: None,
            goal: None,
            created_at: now,
            updated_at: now,
        };
        *self.inner.provider_thread.lock().expect("provider thread") = Some(thread.clone());
        let _ = self
            .inner
            .provider_events
            .send(ProviderAdapterV2Event::ProviderThreadUpdated {
                driver: self.inner.driver.clone(),
                provider_thread: thread.clone(),
                retained_native_turn_ids: None,
            });
        thread
    }

    fn emit_terminal(
        &self,
        provider_thread_id: &ProviderThreadId,
        provider_turn_id: &ProviderTurnId,
        run_ordinal: i64,
        status: ProviderAdapterV2TurnTerminalStatus,
        failure: Option<&str>,
    ) {
        let failure = match status {
            ProviderAdapterV2TurnTerminalStatus::Failed => Some(
                turin_provider_core::failure::make_provider_failure(
                    turin_provider_core::failure::ProviderFailureInput {
                        cause: None,
                        message: failure,
                        code: None,
                        class: Some(
                            turin_contracts::orchestration_v2::OrchestrationV2ProviderFailureClass::ProviderError,
                        ),
                        retryable: Some(Some(true)),
                        reset_at: None,
                    },
                ),
            ),
            _ => None,
        };
        let disposition = if failure.is_some() {
            OrchestrationV2ProviderThreadDisposition::Broken
        } else {
            OrchestrationV2ProviderThreadDisposition::Reusable
        };
        let _ = self
            .inner
            .provider_events
            .send(ProviderAdapterV2Event::TurnTerminal {
                driver: self.inner.driver.clone(),
                provider_thread_id: provider_thread_id.clone(),
                provider_turn_id: provider_turn_id.clone(),
                run_ordinal: PositiveInt::new(u64::try_from(run_ordinal).unwrap_or(0).max(1))
                    .expect("run ordinal"),
                status,
                failure,
                failure_item_ordinal: None,
                retry: None,
                retry_started_at: None,
                thread_disposition: disposition,
            });
    }

    fn cancel_waiters(&self) {
        let pending = std::mem::take(&mut *self.inner.pending.lock().expect("pending"));
        for (_, item) in pending {
            cancel_pending(item);
        }
    }
}

fn native_session_id(thread: &OrchestrationV2ProviderThread) -> Option<String> {
    thread
        .native_thread_ref
        .as_ref()
        .and_then(|reference| reference.native_id.as_ref())
        .map(|id| id.as_str().to_owned())
}

fn terminal_status(stop_reason: Option<&str>) -> ProviderAdapterV2TurnTerminalStatus {
    match stop_reason {
        Some("cancelled" | "canceled") => ProviderAdapterV2TurnTerminalStatus::Cancelled,
        Some("refusal") => ProviderAdapterV2TurnTerminalStatus::Failed,
        _ => ProviderAdapterV2TurnTerminalStatus::Completed,
    }
}

fn plan_disposition(decision: Option<ProviderApprovalDecision>) -> PlanDisposition {
    match decision {
        Some(ProviderApprovalDecision::Decline) => PlanDisposition::Rejected { reason: None },
        Some(ProviderApprovalDecision::Cancel) | None => PlanDisposition::Cancelled,
        Some(_) => PlanDisposition::Accepted { plan_uri: None },
    }
}

fn rollback_checkpoint(
    input: &ProviderAdapterV2RollbackThreadInput,
) -> turin_contracts::CheckpointId {
    match &input.target {
        turin_provider_core::provider_adapter::ProviderAdapterV2RollbackTarget::ThreadStart {
            checkpoint_id,
            ..
        }
        | turin_provider_core::provider_adapter::ProviderAdapterV2RollbackTarget::ProviderTurn {
            checkpoint_id,
            ..
        } => checkpoint_id.clone(),
    }
}

fn map_error(kind: ProviderAdapterErrorKind, error: CursorError) -> ProviderAdapterV2Error {
    ProviderAdapterV2Error::new(kind)
        .with_cause(ProviderFailureCause::tagged("CursorError").with_message(error.to_string()))
}

fn emit_runtime_request(
    inner: &SessionInner,
    request_id: &str,
    kind: OrchestrationV2RuntimeRequestKind,
    status: OrchestrationV2RuntimeRequestStatus,
    decision: Option<ProviderApprovalDecision>,
) {
    let Ok(id) = RuntimeRequestId::new(request_id) else {
        return;
    };
    let turn = inner.active_turn.lock().expect("turn");
    let now = UtcDateTime::now();
    let request = OrchestrationV2RuntimeRequest {
        id: id.clone(),
        node_id: derive::approval_node(&id),
        provider_turn_id: turn.as_ref().map(|turn| turn.provider_turn_id.clone()),
        native_request_ref: Some(OrchestrationV2ProviderRef {
            driver: inner.driver.clone(),
            native_id: TrimmedNonEmptyString::new(request_id).ok(),
            strength: OrchestrationV2NativeRefStrength::Weak,
            fingerprint: None,
            ordinal: None,
        }),
        kind,
        status,
        response_capability: OrchestrationV2ResponseCapability::Live {
            provider_session_id: inner.provider_session_id.clone(),
        },
        created_at: now,
        resolved_at: matches!(
            status,
            OrchestrationV2RuntimeRequestStatus::Resolved
                | OrchestrationV2RuntimeRequestStatus::Cancelled
        )
        .then(UtcDateTime::now),
        decision,
        answers: None,
    };
    let thread_id = turn.as_ref().map(|turn| turn.thread_id.clone());
    drop(turn);
    let _ = inner
        .provider_events
        .send(ProviderAdapterV2Event::RuntimeRequestUpdated {
            driver: inner.driver.clone(),
            thread_id,
            runtime_request: request,
        });
}

fn emit_update(inner: &SessionInner, update: &CursorUpdate) {
    let guard = inner.active_turn.lock().expect("turn");
    let Some(turn) = guard.as_ref() else {
        return;
    };
    let ordinal = turn.ordinal.fetch_add(1, Ordering::SeqCst);
    let native = format!("cursor-item-{ordinal}");
    let now = UtcDateTime::now();
    let item = match update {
        CursorUpdate::Text { text, .. } => OrchestrationV2TurnItem::AssistantMessage {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id: derive::turn_item_from_provider_item(&inner.driver, &native),
            thread_id: turn.thread_id.clone(),
            run_id: Some(turn.run_id.clone()),
            node_id: Some(turn.node_id.clone()),
            provider_thread_id: Some(turn.provider_thread_id.clone()),
            provider_turn_id: Some(turn.provider_turn_id.clone()),
            native_item_ref: None,
            parent_item_id: None,
            ordinal,
            status: OrchestrationV2TurnItemStatus::Completed,
            title: None,
            started_at: Some(now),
            completed_at: Some(now),
            updated_at: now,
            message_id: derive::message_from_provider_item(&inner.driver, &native),
            text: text.clone(),
            attachments: None,
            streaming: true,
        },
        CursorUpdate::Thought { text } => OrchestrationV2TurnItem::Reasoning {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id: derive::turn_item_from_provider_item(&inner.driver, &native),
            thread_id: turn.thread_id.clone(),
            run_id: Some(turn.run_id.clone()),
            node_id: Some(turn.node_id.clone()),
            provider_thread_id: Some(turn.provider_thread_id.clone()),
            provider_turn_id: Some(turn.provider_turn_id.clone()),
            native_item_ref: None,
            parent_item_id: None,
            ordinal,
            status: OrchestrationV2TurnItemStatus::Completed,
            title: None,
            started_at: Some(now),
            completed_at: Some(now),
            updated_at: now,
            text: text.clone(),
            streaming: true,
        },
        _ => return,
    };
    let driver = inner.driver.clone();
    drop(guard);
    let _ = inner
        .provider_events
        .send(ProviderAdapterV2Event::TurnItemUpdated {
            driver,
            turn_item: item,
        });
}

#[async_trait]
impl ProviderAdapterV2 for CursorAdapter {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.instance_id
    }

    fn driver(&self) -> &ProviderDriverKind {
        &self.driver
    }

    async fn get_capabilities(
        &self,
    ) -> Result<OrchestrationV2ProviderCapabilities, ProviderAdapterV2Error> {
        Ok(capabilities::cursor_capabilities())
    }

    async fn plan_selection_transition(
        &self,
        input: ProviderSelectionTransitionInput,
    ) -> Result<ProviderSelectionTransitionPlan, ProviderAdapterV2Error> {
        Ok(acp_selection_transition(&input))
    }

    async fn open_session(
        &self,
        input: ProviderAdapterV2OpenSessionInput,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderAdapterV2Error> {
        let cwd = input
            .runtime_policy
            .cwd
            .clone()
            .unwrap_or_else(|| ".".to_owned());
        let client = spawn(SpawnCommand {
            program: &self.program,
            args: auth::ACP_ARGS,
            cwd: Some(Path::new(&cwd)),
        })
        .await
        .map_err(|error| {
            map_error(
                ProviderAdapterErrorKind::OpenSession {
                    driver: self.driver.clone(),
                    provider_session_id: input.provider_session_id.clone(),
                },
                CursorError::from_acp(CursorOperation::Spawn, &error),
            )
        })?;
        let session = CursorSession::from_client_with(
            client,
            self.cursor_home.clone(),
            self.instance_id.clone(),
            self.driver.clone(),
            input.provider_session_id.clone(),
            cwd,
            Some(input.model_selection.model),
        );
        session.initialize().await.map_err(|error| {
            map_error(
                ProviderAdapterErrorKind::OpenSession {
                    driver: self.driver.clone(),
                    provider_session_id: input.provider_session_id,
                },
                error,
            )
        })?;
        session.publish_session();
        Ok(Arc::new(session))
    }
}

#[async_trait]
impl ProviderSessionRuntime for CursorSession {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.inner.instance_id
    }

    fn driver(&self) -> &ProviderDriverKind {
        &self.inner.driver
    }

    fn provider_session_id(&self) -> &ProviderSessionId {
        &self.inner.provider_session_id
    }

    fn provider_session(&self) -> &OrchestrationV2ProviderSession {
        &self.inner.provider_session
    }

    fn events(&self) -> ProviderEventStream {
        let receiver = self.provider_events.lock().expect("provider events").take();
        match receiver {
            Some(rx) => Box::pin(unfold(rx, |mut rx| async move {
                let event = rx.recv().await?;
                Some((Ok(event), rx))
            })),
            None => {
                let error = ProviderAdapterV2Error::new(ProviderAdapterErrorKind::EventStream {
                    driver: self.inner.driver.clone(),
                    provider_session_id: self.inner.provider_session_id.clone(),
                });
                Box::pin(stream::once(async move { Err(error) }))
            }
        }
    }

    async fn ensure_thread(
        &self,
        input: ProviderAdapterV2EnsureThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        if let Some(thread) = self.inner.provider_thread.lock().expect("thread").clone() {
            return Ok(thread);
        }
        let cwd = input
            .runtime_policy
            .cwd
            .clone()
            .unwrap_or_else(|| ".".to_owned());
        let native = input
            .existing_provider_thread
            .as_ref()
            .and_then(native_session_id);
        let opened = match native {
            Some(native) => self.load_session(&native, Path::new(&cwd)).await,
            None => self.new_session(Path::new(&cwd)).await,
        };
        let opened = opened.map_err(|error| {
            map_error(
                ProviderAdapterErrorKind::EnsureThread {
                    driver: self.inner.driver.clone(),
                    thread_id: input.thread_id.clone(),
                },
                error,
            )
        })?;
        self.apply_model_selection(&input.model_selection)
            .await
            .map_err(|error| {
                map_error(
                    ProviderAdapterErrorKind::EnsureThread {
                        driver: self.inner.driver.clone(),
                        thread_id: input.thread_id.clone(),
                    },
                    error,
                )
            })?;
        Ok(self.bind_provider_thread(&opened.session_id, input.thread_id))
    }

    async fn resume_thread(
        &self,
        input: ProviderAdapterV2ResumeThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        let native = native_session_id(&input.provider_thread).ok_or_else(|| {
            ProviderAdapterV2Error::new(ProviderAdapterErrorKind::ResumeThread {
                driver: self.inner.driver.clone(),
                provider_session_id: self.inner.provider_session_id.clone(),
                provider_thread_id: input.provider_thread.id.clone(),
            })
        })?;
        let cwd = input
            .runtime_policy
            .as_ref()
            .and_then(|policy| policy.cwd.clone())
            .unwrap_or_else(|| ".".to_owned());
        let opened = self
            .load_session(&native, Path::new(&cwd))
            .await
            .map_err(|error| {
                map_error(
                    ProviderAdapterErrorKind::ResumeThread {
                        driver: self.inner.driver.clone(),
                        provider_session_id: self.inner.provider_session_id.clone(),
                        provider_thread_id: input.provider_thread.id.clone(),
                    },
                    error,
                )
            })?;
        let app_thread = input
            .thread_id
            .clone()
            .or_else(|| input.provider_thread.app_thread_id.clone())
            .unwrap_or_else(|| {
                derive::thread_from_provider_thread(
                    &self.inner.driver,
                    Some(&self.inner.instance_id),
                    &native,
                )
            });
        Ok(self.bind_provider_thread(&opened.session_id, app_thread))
    }

    async fn start_turn(
        &self,
        input: ProviderAdapterV2TurnInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        if !input.message.attachments.is_empty() {
            return Err(ProviderAdapterV2Error::new(
                ProviderAdapterErrorKind::Protocol {
                    driver: self.inner.driver.clone(),
                    detail: "Cursor ACP prompts take inline image bytes. This turn message's attachments do not carry them.".into(),
                    payload: None,
                },
            ));
        }
        let kind = ProviderAdapterErrorKind::TurnStart {
            driver: self.inner.driver.clone(),
            thread_id: input.thread_id.clone(),
            provider_thread_id: input.provider_thread.id.clone(),
            run_id: input.run_id.clone(),
        };
        self.apply_model_selection(&input.model_selection)
            .await
            .map_err(|error| map_error(kind.clone(), error))?;
        let native = native_session_id(&input.provider_thread).unwrap_or_default();
        let provider_turn_id = derive::provider_turn(
            &self.inner.driver,
            &format!("{native}:{}", input.run_id.as_str()),
        );
        *self.inner.active_turn.lock().expect("turn") = Some(ActiveTurn {
            thread_id: input.thread_id.clone(),
            run_id: input.run_id.clone(),
            provider_thread_id: input.provider_thread.id.clone(),
            provider_turn_id: provider_turn_id.clone(),
            node_id: input.root_node_id.clone(),
            ordinal: std::sync::atomic::AtomicU64::new(0),
        });
        let outcome = self.prompt(&input.message.text, &[]).await;
        *self.inner.active_turn.lock().expect("turn") = None;
        match outcome {
            Ok(outcome) => {
                self.emit_terminal(
                    &input.provider_thread.id,
                    &provider_turn_id,
                    input.run_ordinal,
                    terminal_status(outcome.stop_reason.as_deref()),
                    None,
                );
                Ok(())
            }
            Err(error) => {
                self.emit_terminal(
                    &input.provider_thread.id,
                    &provider_turn_id,
                    input.run_ordinal,
                    ProviderAdapterV2TurnTerminalStatus::Failed,
                    Some(&error.to_string()),
                );
                Err(map_error(kind, error))
            }
        }
    }

    async fn steer_turn(
        &self,
        input: ProviderAdapterV2SteerInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        Err(ProviderAdapterV2Error::new(
            ProviderAdapterErrorKind::SteerRunUnsupported {
                driver: self.inner.driver.clone(),
                provider_thread_id: input.provider_thread.id,
            },
        ))
    }

    async fn interrupt_turn(
        &self,
        input: ProviderAdapterV2InterruptInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        self.cancel_turn().await.map_err(|error| {
            map_error(
                ProviderAdapterErrorKind::Interrupt {
                    driver: self.inner.driver.clone(),
                    provider_thread_id: input.provider_thread.id,
                    provider_turn_id: input.provider_turn_id,
                },
                error,
            )
        })
    }

    async fn respond_to_runtime_request(
        &self,
        input: ProviderAdapterV2RuntimeRequestResponseInput,
    ) -> Result<(), ProviderAdapterV2Error> {
        let request_id = input.request_id.as_str();
        let kind = self
            .inner
            .pending_kind
            .lock()
            .expect("pending kind")
            .get(request_id)
            .copied();
        let reply = match kind {
            Some(PendingKind::Ask) => {
                let params = self
                    .inner
                    .asks
                    .lock()
                    .expect("asks")
                    .get(request_id)
                    .cloned();
                let cancelled = input.decision == Some(ProviderApprovalDecision::Cancel);
                let disposition = match params {
                    Some(params) => extensions::ask_disposition_from_answers(
                        &params,
                        input.answers.as_ref().unwrap_or(&Default::default()),
                        cancelled,
                    ),
                    None => AskDisposition::Cancelled,
                };
                RuntimeReply::UserInput(disposition)
            }
            Some(PendingKind::Plan) => RuntimeReply::Plan(plan_disposition(input.decision)),
            _ => RuntimeReply::Approval(input.decision.unwrap_or(ProviderApprovalDecision::Cancel)),
        };
        let request_kind = match kind {
            Some(PendingKind::Ask) | Some(PendingKind::Plan) => {
                OrchestrationV2RuntimeRequestKind::UserInput
            }
            _ => OrchestrationV2RuntimeRequestKind::Permission,
        };
        self.answer_request(request_id, reply).map_err(|error| {
            map_error(
                ProviderAdapterErrorKind::RuntimeRequestResponse {
                    driver: self.inner.driver.clone(),
                    request_id: input.request_id.clone(),
                },
                error,
            )
        })?;
        self.inner
            .pending_kind
            .lock()
            .expect("pending kind")
            .remove(request_id);
        self.inner.asks.lock().expect("asks").remove(request_id);
        let status = if input.decision == Some(ProviderApprovalDecision::Cancel) {
            OrchestrationV2RuntimeRequestStatus::Cancelled
        } else {
            OrchestrationV2RuntimeRequestStatus::Resolved
        };
        emit_runtime_request(
            &self.inner,
            request_id,
            request_kind,
            status,
            input.decision,
        );
        Ok(())
    }

    async fn read_thread_snapshot(
        &self,
        input: ProviderAdapterV2ReadThreadSnapshotInput,
    ) -> Result<
        turin_provider_core::provider_adapter::ProviderAdapterV2ThreadSnapshot,
        ProviderAdapterV2Error,
    > {
        Err(ProviderAdapterV2Error::new(
            ProviderAdapterErrorKind::ReadThreadSnapshot {
                driver: self.inner.driver.clone(),
                provider_thread_id: input.provider_thread.id,
            },
        ))
    }

    async fn rollback_thread(
        &self,
        input: ProviderAdapterV2RollbackThreadInput,
    ) -> Result<
        turin_provider_core::provider_adapter::ProviderAdapterV2ThreadSnapshot,
        ProviderAdapterV2Error,
    > {
        Err(ProviderAdapterV2Error::new(
            ProviderAdapterErrorKind::RollbackThread {
                driver: self.inner.driver.clone(),
                provider_thread_id: input.provider_thread.id.clone(),
                checkpoint_id: Some(rollback_checkpoint(&input)),
            },
        ))
    }

    async fn fork_thread(
        &self,
        input: ProviderAdapterV2ForkThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
        Err(ProviderAdapterV2Error::new(
            ProviderAdapterErrorKind::ForkThread {
                driver: self.inner.driver.clone(),
                provider_thread_id: input.source_provider_thread.id,
            },
        ))
    }
}

fn register_handlers(client: &AcpClient, inner: &Arc<SessionInner>) {
    {
        let inner = Arc::clone(inner);
        client.on_session_update(move |params| {
            let inner = Arc::clone(&inner);
            async move {
                for update in events::parse_session_update(&params) {
                    emit_update(&inner, &update);
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
    inner
        .pending_kind
        .lock()
        .expect("pending kind")
        .insert(ctx.request_id.clone(), PendingKind::Permission);
    emit_runtime_request(
        &inner,
        &ctx.request_id,
        permissions::request_kind(&request),
        OrchestrationV2RuntimeRequestStatus::Pending,
        None,
    );
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
            inner
                .pending_kind
                .lock()
                .expect("pending kind")
                .insert(ctx.request_id.clone(), PendingKind::Ask);
            inner
                .asks
                .lock()
                .expect("asks")
                .insert(ctx.request_id.clone(), parsed.clone());
            emit_runtime_request(
                &inner,
                &ctx.request_id,
                OrchestrationV2RuntimeRequestKind::UserInput,
                OrchestrationV2RuntimeRequestStatus::Pending,
                None,
            );
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
            inner
                .pending_kind
                .lock()
                .expect("pending kind")
                .insert(ctx.request_id.clone(), PendingKind::Plan);
            emit_runtime_request(
                &inner,
                &ctx.request_id,
                OrchestrationV2RuntimeRequestKind::UserInput,
                OrchestrationV2RuntimeRequestStatus::Pending,
                None,
            );
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
        let thread = session.new_session(Path::new("/work")).await.unwrap();
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
        let turn = tokio::spawn(async move { running.prompt("hello", &[]).await });
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
                        .answer_request(
                            &request_id,
                            RuntimeReply::Approval(Decision::AcceptForSession),
                        )
                        .unwrap();
                }
                CursorEvent::UserInput { request_id, .. } => {
                    session
                        .answer_request(
                            &request_id,
                            RuntimeReply::UserInput(AskDisposition::Answered(vec![
                                turin_acp::cursor::AskAnswer {
                                    question_id: "q1".to_owned(),
                                    selected_option_ids: vec!["a".to_owned()],
                                },
                            ])),
                        )
                        .unwrap();
                }
                CursorEvent::Plan { request_id, .. } => {
                    session
                        .answer_request(
                            &request_id,
                            RuntimeReply::Plan(PlanDisposition::Accepted {
                                plan_uri: Some("file:///plan.md".to_owned()),
                            }),
                        )
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
        let second = tokio::time::timeout(Duration::from_secs(5), session.prompt("again", &[]))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.stop_reason.as_deref(), Some("end_turn"));
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
        let error = session.new_session(Path::new("/work")).await.unwrap_err();
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
        session.new_session(Path::new("/work")).await.unwrap();
        let running = session.clone();
        let turn = tokio::spawn(async move { running.prompt("stop", &[]).await });
        match tokio::time::timeout(Duration::from_secs(5), session.recv())
            .await
            .unwrap()
            .unwrap()
        {
            CursorEvent::Update(_) => {}
            other => panic!("unexpected {other:?}"),
        }
        session.cancel_turn().await.unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(5), turn)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(outcome.stop_reason.as_deref(), Some("cancelled"));
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn runtime_trait_binds_the_native_session_and_refuses_fork() {
        use futures::StreamExt;
        use turin_contracts::provider_policy::{ProviderInteractionMode, RuntimeMode};
        use turin_provider_core::provider_adapter::ProviderAdapterV2RuntimePolicy;

        let (client, mut read, mut write) = pair();
        let peer = tokio::spawn(async move {
            let init = read_frame(&mut read).await;
            write_frame(&mut write, v1_initialized(&init["id"])).await;
            let created = read_frame(&mut read).await;
            assert_eq!(created["method"], "session/new");
            assert_eq!(created["params"]["cwd"], "/work");
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": created["id"], "result": {"sessionId": "sess-7"}}),
            )
            .await;
            let models = read_frame(&mut read).await;
            assert_eq!(models["method"], "cursor/list_available_models");
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": models["id"], "result": {"models": []}}),
            )
            .await;
            let config = read_frame(&mut read).await;
            assert_eq!(config["params"]["configId"], "model");
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": config["id"], "result": {}}),
            )
            .await;
        });

        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        let runtime: Arc<dyn ProviderSessionRuntime> = Arc::new(session.clone());
        let mut events = runtime.events();
        let thread_id = ThreadId::new("thread-1").unwrap();
        let input = ProviderAdapterV2EnsureThreadInput {
            thread_id: thread_id.clone(),
            title: None,
            model_selection: ModelSelection {
                instance_id: ProviderInstanceId::new("cursor").unwrap(),
                model: TrimmedNonEmptyString::new("gpt-5.5").unwrap(),
                options: None,
            },
            runtime_policy: ProviderAdapterV2RuntimePolicy {
                runtime_mode: RuntimeMode::ApprovalRequired,
                interaction_mode: ProviderInteractionMode::Default,
                cwd: Some("/work".to_owned()),
                approval_policy: None,
                sandbox_policy: None,
                reasoning_effort: None,
            },
            provider_session_id: None,
            existing_provider_thread: None,
        };
        let thread = runtime.ensure_thread(input.clone()).await.unwrap();
        assert_eq!(native_session_id(&thread).as_deref(), Some("sess-7"));
        assert_eq!(thread.app_thread_id.as_ref(), Some(&thread_id));
        assert_eq!(runtime.ensure_thread(input).await.unwrap().id, thread.id);

        match events.next().await.unwrap().unwrap() {
            ProviderAdapterV2Event::ProviderThreadUpdated {
                provider_thread, ..
            } => assert_eq!(provider_thread.id, thread.id),
            other => panic!("unexpected {other:?}"),
        }
        assert!(runtime.events().next().await.unwrap().is_err());

        let fork = runtime
            .fork_thread(ProviderAdapterV2ForkThreadInput {
                source_provider_thread: thread.clone(),
                source_provider_turns: None,
                provider_turn_id: None,
                target_thread_id: ThreadId::new("thread-2").unwrap(),
                owner_node_id: None,
                model_selection: None,
                runtime_policy: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(
            fork.kind,
            ProviderAdapterErrorKind::ForkThread { .. }
        ));
        peer.await.unwrap();
    }

    #[test]
    fn launch_never_passes_force_or_yolo() {
        assert!(auth::launch_is_unforced(auth::ACP_ARGS));
        assert_eq!(auth::ACP_ARGS, &["acp"]);
    }
}
