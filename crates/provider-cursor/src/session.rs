//! One `cursor-agent acp` process per Turin thread.
//!
//! `authenticate` is sent only by [`CursorSession::sign_in`]. `session/new`
//! and `session/load` that fail with authentication required stay unsigned-in.
//! Model changes and session open/load snapshot the Cursor user files they
//! rewrite. `session/cancel` interrupts the active prompt.
//!
//! Provider events are the only output. A turn streams items while its prompt
//! runs, and every card (approval, question, plan) is a runtime request plus a
//! turn item, so a client sees the same content whichever way it renders them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use async_trait::async_trait;
use futures::stream::{self, unfold};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use turin_acp::compat::ImplementationInfo;
use turin_acp::compat::InitializeResponse;
use turin_acp::cursor::{AskQuestionParams, CreatePlanParams, UpdateTodosParams};
use turin_acp::protocol::RequestContext;
use turin_acp::{
    AcpClient, AcpError, AcpSessionRuntime, AcpSessionRuntimeOptions, AcpSpawnInput, CancelBehavior,
};
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ExecutionNodeKind, OrchestrationV2ProviderCapabilities,
    OrchestrationV2ProviderSession, OrchestrationV2ProviderSessionStatus,
    OrchestrationV2ProviderThread, OrchestrationV2ProviderThreadDisposition,
    OrchestrationV2ProviderThreadStatus, OrchestrationV2ProviderTurn,
    OrchestrationV2ProviderTurnStatus, OrchestrationV2ResponseCapability,
    OrchestrationV2RuntimeRequest, OrchestrationV2RuntimeRequestKind,
    OrchestrationV2RuntimeRequestStatus, OrchestrationV2TurnItemStatus,
};
use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};
use turin_contracts::provider_policy::{ProviderApprovalDecision, ProviderInteractionMode};
use turin_contracts::provider_runtime::ThreadTokenUsageSnapshot;
use turin_contracts::{
    PositiveInt, ProviderSessionId, RunAttemptId, RuntimeRequestId, ThreadId,
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
use crate::events::{self, CardItem, Events, ItemBody, Scope, TurnStream};
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

/// What a waiting card asked, read without closing the card.
enum Waiting {
    Permission,
    Ask(AskQuestionParams),
    Plan,
}

/// The agent's call, held until the user answers the card.
enum Waiter {
    Permission(oneshot::Sender<ProviderApprovalDecision>),
    Ask {
        sender: oneshot::Sender<AskDisposition>,
        params: AskQuestionParams,
    },
    Plan(oneshot::Sender<PlanDisposition>),
}

/// The runtime request for a card, plus its turn item when a turn is open.
/// Without a turn there is no node or item to attach to.
struct Card {
    request: OrchestrationV2RuntimeRequest,
    thread_id: Option<ThreadId>,
    item: Option<CardItem>,
}

struct Pending {
    waiter: Waiter,
    card: Card,
}

/// Where one prompt is running. Upstream `ProviderAdapterV2TurnInput` fields the
/// turn needs after `startTurn` returns.
#[derive(Clone)]
struct TurnContext {
    scope: Scope,
    run_ordinal: i64,
    provider_turn_ordinal: i64,
    attempt_id: RunAttemptId,
}

struct ActiveTurn {
    context: TurnContext,
    stream: TurnStream,
}

struct SessionInner {
    runtime: AcpSessionRuntime,
    provider_events: mpsc::UnboundedSender<ProviderAdapterV2Event>,
    pending: Mutex<HashMap<String, Pending>>,
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
    /// The session as published to clients, once `initialize` succeeded.
    published: OnceLock<OrchestrationV2ProviderSession>,
    /// Latest context-window usage from the agent. Kept so a thread bound later still has it.
    context_usage: Mutex<Option<ThreadTokenUsageSnapshot>>,
}

impl Drop for SessionInner {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.pending.lock() {
            for (_, item) in pending.drain() {
                cancel_waiter(item.waiter);
            }
        }
    }
}

fn cancel_waiter(waiter: Waiter) {
    match waiter {
        Waiter::Permission(sender) => {
            let _ = sender.send(ProviderApprovalDecision::Cancel);
        }
        Waiter::Ask { sender, .. } => {
            let _ = sender.send(AskDisposition::Cancelled);
        }
        Waiter::Plan(sender) => {
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
    provider_events: Arc<Mutex<Option<mpsc::UnboundedReceiver<ProviderAdapterV2Event>>>>,
}

impl CursorSession {
    pub fn from_client(client: AcpClient, cursor_home: Option<PathBuf>) -> Self {
        let instance_id = ProviderInstanceId::new("cursor").expect("cursor instance id");
        let driver = ProviderDriverKind::new("cursor").expect("cursor driver slug");
        let provider_session_id = derive::provider_session(&instance_id);
        let runtime =
            AcpSessionRuntime::from_connected_client(client, connected_client_runtime_options());
        Self::from_runtime_with(
            runtime,
            cursor_home,
            instance_id,
            driver,
            provider_session_id,
            "/".to_owned(),
            None,
        )
    }

    fn from_runtime_with(
        runtime: AcpSessionRuntime,
        cursor_home: Option<PathBuf>,
        instance_id: ProviderInstanceId,
        driver: ProviderDriverKind,
        provider_session_id: ProviderSessionId,
        cwd: String,
        model: Option<TrimmedNonEmptyString>,
    ) -> Self {
        let client = runtime.client().clone();
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
            runtime,
            provider_events: provider_tx,
            pending: Mutex::new(HashMap::new()),
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
            published: OnceLock::new(),
            context_usage: Mutex::new(None),
        });
        register_handlers(&client, &inner);
        Self {
            inner,
            provider_events: Arc::new(Mutex::new(Some(provider_rx))),
        }
    }

    pub async fn initialize(&self) -> Result<InitializeResponse, CursorError> {
        let response = self
            .inner
            .runtime
            .initialize()
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
            .runtime
            .client()
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
            .runtime
            .client()
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
        let runtime = self.inner.runtime.clone();
        let home = self.inner.home.clone();
        with_user_config(home.as_deref(), || async move {
            for update in updates {
                runtime
                    .client()
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
            .runtime
            .client()
            .set_session_mode(json!({"sessionId": session_id, "modeId": mode_id}))
            .await
            .map_err(|error| CursorError::from_acp(CursorOperation::Config, &error))
    }

    /// Upstream sends `mode: "plan"` with a plan-mode turn and `"agent"` otherwise.
    async fn apply_interaction_mode(
        &self,
        mode: ProviderInteractionMode,
    ) -> Result<(), CursorError> {
        let mode_id = match mode {
            ProviderInteractionMode::Plan => "plan",
            ProviderInteractionMode::Default => "agent",
        };
        self.set_mode(mode_id).await.map(|_| ())
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

    async fn remember_thread(&self, setup: Value) -> Result<CursorThread, CursorError> {
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
            session_id: session_id.clone(),
            raw: setup.clone(),
        };
        if let Err(error) = self
            .inner
            .runtime
            .adopt_started_session(session_id, setup)
            .await
        {
            return Err(CursorError::from_acp(CursorOperation::SessionNew, &error));
        }
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
        let client = self.inner.runtime.client().clone();
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
        let client = self.inner.runtime.client().clone();
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
        self.remember_thread(setup).await
    }
}

impl CursorSession {
    /// `session/new`, once per process. Later calls return the open thread.
    pub async fn new_session(&self, cwd: &Path) -> Result<CursorThread, CursorError> {
        if let Some(thread) = self.inner.thread.lock().expect("thread").clone() {
            return Ok(thread);
        }
        let cwd = absolute_path(cwd).display().to_string();
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
        let cwd = absolute_path(cwd).display().to_string();
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
            .runtime
            .prompt(json!({"sessionId": session_id, "prompt": prompt}))
            .await;
        // The turn is over either way. A card still waiting here would never get
        // an answer, because the agent stopped asking.
        cancel_all(&self.inner);
        let result = result.map_err(|error| self.fail_auth(CursorOperation::Prompt, error))?;
        let stop_reason = result
            .get("stopReason")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(PromptOutcome {
            stop_reason,
            raw: result,
        })
    }

    /// Marks the active turn interrupted, cancels waiting cards, then `session/cancel`.
    pub async fn cancel_turn(&self) -> Result<(), CursorError> {
        if let Some(active) = self.inner.active_turn.lock().expect("turn").as_mut() {
            active.stream.interrupted = true;
        }
        cancel_all(&self.inner);
        let _session_id = self.session_id()?;
        self.inner
            .runtime
            .cancel()
            .await
            .map_err(|error| CursorError::from_acp(CursorOperation::Cancel, &error))
    }

    /// Answers the card for `request_id`, emits its resolution, and releases the
    /// agent's call.
    pub fn answer_request(&self, request_id: &str, reply: RuntimeReply) -> Result<(), CursorError> {
        let pending = self
            .inner
            .pending
            .lock()
            .expect("pending")
            .remove(request_id)
            .ok_or_else(not_waiting)?;
        let decision = reply_decision(&reply);
        let Pending { waiter, card } = pending;
        let matched = match (waiter, reply) {
            (Waiter::Permission(sender), RuntimeReply::Approval(decision)) => {
                sender.send(decision).is_ok()
            }
            (Waiter::Ask { sender, .. }, RuntimeReply::UserInput(disposition)) => {
                sender.send(disposition).is_ok()
            }
            (Waiter::Plan(sender), RuntimeReply::Plan(disposition)) => {
                sender.send(disposition).is_ok()
            }
            (other, _) => {
                cancel_waiter(other);
                false
            }
        };
        resolve_card(&self.inner, card, decision);
        if matched { Ok(()) } else { Err(not_waiting()) }
    }

    /// The kind of card still open for `request_id`, without closing it.
    fn waiting(&self, request_id: &str) -> Option<Waiting> {
        let pending = self.inner.pending.lock().expect("pending");
        match &pending.get(request_id)?.waiter {
            Waiter::Permission(_) => Some(Waiting::Permission),
            Waiter::Ask { params, .. } => Some(Waiting::Ask(params.clone())),
            Waiter::Plan(_) => Some(Waiting::Plan),
        }
    }

    fn publish_session(&self) {
        let mut session = self.inner.provider_session.clone();
        session.status = OrchestrationV2ProviderSessionStatus::Ready;
        session.capabilities = self.capabilities();
        session.updated_at = UtcDateTime::now();
        let _ = self.inner.published.set(session.clone());
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
            native_thread_ref: Some(
                turin_contracts::orchestration_v2::OrchestrationV2ProviderRef {
                    driver: self.inner.driver.clone(),
                    native_id: TrimmedNonEmptyString::new(native_session_id).ok(),
                    strength:
                        turin_contracts::orchestration_v2::OrchestrationV2NativeRefStrength::Strong,
                    fingerprint: None,
                    ordinal: None,
                },
            ),
            native_conversation_head_ref: None,
            status: OrchestrationV2ProviderThreadStatus::Idle,
            first_run_ordinal: None,
            last_run_ordinal: None,
            handoff_ids: Vec::new(),
            forked_from: None,
            pending_background_tasks: Vec::new(),
            context_usage: self.inner.context_usage.lock().expect("usage").clone(),
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

    /// One prompt as a turn: open the turn, stream its items, and close it with a
    /// terminal status. Dropping this future mid-prompt closes the turn as
    /// interrupted and cancels the agent's work.
    async fn run_turn(
        &self,
        context: TurnContext,
        text: &str,
    ) -> Result<PromptOutcome, CursorError> {
        begin_turn(&self.inner, &context)?;
        let mut guard = TurnGuard {
            inner: Arc::clone(&self.inner),
            armed: true,
        };
        let outcome = self.prompt(text, &[]).await;
        guard.armed = false;
        match &outcome {
            Ok(outcome) => finish_turn(
                &self.inner,
                turn_status(outcome.stop_reason.as_deref()),
                None,
            ),
            Err(error) => finish_turn(
                &self.inner,
                OrchestrationV2ProviderTurnStatus::Failed,
                Some(error.to_string()),
            ),
        }
        outcome
    }
}

/// Closes the turn when its future is dropped before the prompt returns.
struct TurnGuard {
    inner: Arc<SessionInner>,
    armed: bool,
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        cancel_all(&self.inner);
        finish_turn(
            &self.inner,
            OrchestrationV2ProviderTurnStatus::Interrupted,
            None,
        );
        let session_id = self
            .inner
            .thread
            .lock()
            .ok()
            .and_then(|thread| thread.as_ref().map(|thread| thread.session_id.clone()));
        if let (Some(_session_id), Ok(handle)) = (session_id, tokio::runtime::Handle::try_current())
        {
            let runtime = self.inner.runtime.clone();
            handle.spawn(async move {
                let _ = runtime.cancel().await;
            });
        }
    }
}

fn not_waiting() -> CursorError {
    CursorError::new(
        CursorOperation::Permission,
        "That Cursor request is no longer waiting.",
    )
}

fn reply_decision(reply: &RuntimeReply) -> Option<ProviderApprovalDecision> {
    match reply {
        RuntimeReply::Approval(decision) => Some(*decision),
        RuntimeReply::UserInput(AskDisposition::Cancelled) => {
            Some(ProviderApprovalDecision::Cancel)
        }
        RuntimeReply::UserInput(_) => None,
        RuntimeReply::Plan(PlanDisposition::Accepted { .. }) => {
            Some(ProviderApprovalDecision::Accept)
        }
        RuntimeReply::Plan(PlanDisposition::Rejected { .. }) => {
            Some(ProviderApprovalDecision::Decline)
        }
        RuntimeReply::Plan(PlanDisposition::Cancelled) => Some(ProviderApprovalDecision::Cancel),
    }
}

fn send(inner: &SessionInner, events: Events) {
    for event in events {
        let _ = inner.provider_events.send(event);
    }
}

/// Runs `f` against the open turn's stream and returns what it produced. With no
/// open turn there is nothing to attach items to, so the result is empty.
fn turn_events(inner: &SessionInner, f: impl FnOnce(&mut TurnStream) -> Events) -> Events {
    match inner.active_turn.lock().expect("turn").as_mut() {
        Some(active) => f(&mut active.stream),
        None => Vec::new(),
    }
}

fn begin_turn(inner: &SessionInner, context: &TurnContext) -> Result<(), CursorError> {
    let now = UtcDateTime::now();
    let provider_turn_ordinal = u64::try_from(context.provider_turn_ordinal)
        .unwrap_or(1)
        .max(1);
    let stream = TurnStream::new(context.scope.clone(), provider_turn_ordinal, now);
    {
        let mut active = inner.active_turn.lock().expect("turn");
        if active.is_some() {
            return Err(CursorError::new(
                CursorOperation::Prompt,
                format!(
                    "Cursor provider turn {} is still active.",
                    context.scope.provider_turn_id.as_str()
                ),
            ));
        }
        *active = Some(ActiveTurn {
            context: context.clone(),
            stream,
        });
    }
    send(
        inner,
        vec![ProviderAdapterV2Event::ProviderTurnUpdated {
            driver: inner.driver.clone(),
            thread_id: Some(context.scope.thread_id.clone()),
            provider_turn: provider_turn(
                context,
                OrchestrationV2ProviderTurnStatus::Running,
                now,
                None,
            ),
        }],
    );
    Ok(())
}

fn provider_turn(
    context: &TurnContext,
    status: OrchestrationV2ProviderTurnStatus,
    started_at: UtcDateTime,
    completed_at: Option<UtcDateTime>,
) -> OrchestrationV2ProviderTurn {
    OrchestrationV2ProviderTurn {
        id: context.scope.provider_turn_id.clone(),
        provider_thread_id: context.scope.provider_thread_id.clone(),
        node_id: context.scope.root_node_id.clone(),
        run_attempt_id: Some(context.attempt_id.clone()),
        native_turn_ref: Some(events::native_ref(
            &context.scope.driver,
            context.scope.run_id.as_str(),
            true,
        )),
        ordinal: positive(context.provider_turn_ordinal),
        status,
        started_at: Some(started_at),
        completed_at,
        token_usage: None,
        turn_token_usage: None,
    }
}

fn positive(value: i64) -> PositiveInt {
    PositiveInt::new(u64::try_from(value).unwrap_or(1).max(1)).expect("positive ordinal")
}

/// Closes the open turn. Upstream `finalizeTurn`: open items close first, then the
/// provider turn and thread update, then the terminal event.
fn finish_turn(
    inner: &SessionInner,
    status: OrchestrationV2ProviderTurnStatus,
    failure: Option<String>,
) {
    let Some(ActiveTurn {
        context,
        mut stream,
    }) = inner.active_turn.lock().expect("turn").take()
    else {
        return;
    };
    let now = UtcDateTime::now();
    let status = if stream.interrupted {
        OrchestrationV2ProviderTurnStatus::Interrupted
    } else {
        status
    };
    let mut events = stream.finish(status, now);
    events.push(ProviderAdapterV2Event::ProviderTurnUpdated {
        driver: inner.driver.clone(),
        thread_id: Some(context.scope.thread_id.clone()),
        provider_turn: provider_turn(&context, status, stream.started_at(), Some(now)),
    });
    if let Some(thread) = inner
        .provider_thread
        .lock()
        .expect("provider thread")
        .as_mut()
    {
        let run_ordinal = positive(context.run_ordinal);
        thread.status = OrchestrationV2ProviderThreadStatus::Active;
        thread.first_run_ordinal = Some(thread.first_run_ordinal.unwrap_or(run_ordinal));
        thread.last_run_ordinal = Some(run_ordinal);
        thread.updated_at = now;
        events.push(ProviderAdapterV2Event::ProviderThreadUpdated {
            driver: inner.driver.clone(),
            provider_thread: thread.clone(),
            retained_native_turn_ids: None,
        });
    }
    let (terminal, failure, failure_item_ordinal, disposition) = match status {
        OrchestrationV2ProviderTurnStatus::Failed => {
            let ordinal = stream.item_ordinal(&format!(
                "terminal-failure:{}",
                context.scope.provider_turn_id.as_str()
            ));
            (
                ProviderAdapterV2TurnTerminalStatus::Failed,
                Some(turin_provider_core::failure::make_provider_failure(
                    turin_provider_core::failure::ProviderFailureInput {
                        cause: None,
                        message: failure.as_deref(),
                        code: None,
                        class: Some(
                            turin_contracts::orchestration_v2::OrchestrationV2ProviderFailureClass::ProviderError,
                        ),
                        retryable: Some(Some(true)),
                        reset_at: None,
                    },
                )),
                Some(positive(ordinal as i64)),
                OrchestrationV2ProviderThreadDisposition::Broken,
            )
        }
        OrchestrationV2ProviderTurnStatus::Interrupted => (
            ProviderAdapterV2TurnTerminalStatus::Interrupted,
            None,
            None,
            OrchestrationV2ProviderThreadDisposition::Reusable,
        ),
        OrchestrationV2ProviderTurnStatus::Cancelled => (
            ProviderAdapterV2TurnTerminalStatus::Cancelled,
            None,
            None,
            OrchestrationV2ProviderThreadDisposition::Reusable,
        ),
        _ => (
            ProviderAdapterV2TurnTerminalStatus::Completed,
            None,
            None,
            OrchestrationV2ProviderThreadDisposition::Reusable,
        ),
    };
    events.push(ProviderAdapterV2Event::TurnTerminal {
        driver: inner.driver.clone(),
        provider_thread_id: context.scope.provider_thread_id.clone(),
        provider_turn_id: context.scope.provider_turn_id.clone(),
        run_ordinal: positive(context.run_ordinal),
        status: terminal,
        failure,
        failure_item_ordinal,
        retry: None,
        retry_started_at: None,
        thread_disposition: disposition,
    });
    send(inner, events);
}

/// Upstream `terminalStatus` without the interrupt case, which `finish_turn` checks.
fn turn_status(stop_reason: Option<&str>) -> OrchestrationV2ProviderTurnStatus {
    match stop_reason {
        Some("cancelled" | "canceled") => OrchestrationV2ProviderTurnStatus::Cancelled,
        Some("refusal") => OrchestrationV2ProviderTurnStatus::Failed,
        _ => OrchestrationV2ProviderTurnStatus::Completed,
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

/// Every open card resolves as cancelled: the agent's calls are answered and the
/// cards close. Used when a prompt ends, a turn is interrupted, or the session drops.
fn cancel_all(inner: &SessionInner) {
    let pending: Vec<Pending> = inner
        .pending
        .lock()
        .expect("pending")
        .drain()
        .map(|(_, pending)| pending)
        .collect();
    for Pending { waiter, card } in pending {
        cancel_waiter(waiter);
        resolve_card(inner, card, Some(ProviderApprovalDecision::Cancel));
    }
}

/// Closes a card: the runtime request resolves, and its item and node follow.
fn resolve_card(inner: &SessionInner, mut card: Card, decision: Option<ProviderApprovalDecision>) {
    let now = UtcDateTime::now();
    let cancelled = decision == Some(ProviderApprovalDecision::Cancel);
    card.request.status = if cancelled {
        OrchestrationV2RuntimeRequestStatus::Cancelled
    } else {
        OrchestrationV2RuntimeRequestStatus::Resolved
    };
    card.request.decision = decision;
    card.request.resolved_at = Some(now);
    let mut events = vec![runtime_request_event(
        inner,
        card.thread_id.clone(),
        card.request,
    )];
    if let Some(item) = card.item {
        let status = if cancelled {
            OrchestrationV2TurnItemStatus::Cancelled
        } else {
            OrchestrationV2TurnItemStatus::Completed
        };
        events.extend(TurnStream::card_events(&inner.driver, &item, status, now));
    }
    send(inner, events);
}

fn runtime_request_event(
    inner: &SessionInner,
    thread_id: Option<ThreadId>,
    runtime_request: OrchestrationV2RuntimeRequest,
) -> ProviderAdapterV2Event {
    ProviderAdapterV2Event::RuntimeRequestUpdated {
        driver: inner.driver.clone(),
        thread_id,
        runtime_request,
    }
}

/// A new waiting card: its request, and its node and item when a turn is open.
/// Events are not sent here, so the caller can register the waiter first.
fn open_card(
    inner: &SessionInner,
    request_id: &RuntimeRequestId,
    request_kind: OrchestrationV2RuntimeRequestKind,
    spec: CardSpec,
) -> (Card, Events) {
    let now = UtcDateTime::now();
    let (thread_id, provider_turn_id, item) = match inner.active_turn.lock().expect("turn").as_mut()
    {
        Some(active) => {
            let item =
                active
                    .stream
                    .request_card(request_id, spec.node_kind, spec.body, spec.title, now);
            (
                Some(active.context.scope.thread_id.clone()),
                Some(active.context.scope.provider_turn_id.clone()),
                Some(item),
            )
        }
        None => (None, None, None),
    };
    let request = OrchestrationV2RuntimeRequest {
        id: request_id.clone(),
        node_id: derive::approval_node(request_id),
        provider_turn_id,
        native_request_ref: Some(events::native_ref(
            &inner.driver,
            request_id.as_str(),
            false,
        )),
        kind: request_kind,
        status: OrchestrationV2RuntimeRequestStatus::Pending,
        response_capability: OrchestrationV2ResponseCapability::Live {
            provider_session_id: inner.provider_session_id.clone(),
        },
        created_at: now,
        resolved_at: None,
        decision: None,
        answers: None,
    };
    let mut events = vec![runtime_request_event(
        inner,
        thread_id.clone(),
        request.clone(),
    )];
    if let Some(item) = &item {
        events.extend(TurnStream::card_events(
            &inner.driver,
            item,
            OrchestrationV2TurnItemStatus::Waiting,
            now,
        ));
    }
    (
        Card {
            request,
            thread_id,
            item,
        },
        events,
    )
}

struct CardSpec {
    node_kind: OrchestrationV2ExecutionNodeKind,
    body: ItemBody,
    title: Option<String>,
}

fn request_id_of(raw: &str) -> Option<RuntimeRequestId> {
    RuntimeRequestId::new(raw).ok()
}

/// Projects one ACP `session/update` into the open turn.
fn project_update(inner: &SessionInner, update: &events::CursorUpdate) {
    if let events::CursorUpdate::Usage { raw } = update {
        record_usage(inner, raw);
        return;
    }
    let now = UtcDateTime::now();
    let events = turn_events(inner, |stream| events::project(stream, update, now));
    send(inner, events);
}

/// Upstream `UsageUpdated`: keep the snapshot and republish the provider thread
/// with `contextUsage`. Usage can arrive between turns, so no turn is required.
fn record_usage(inner: &SessionInner, raw: &Value) {
    let Some(usage) = events::context_usage(raw) else {
        return;
    };
    *inner.context_usage.lock().expect("usage") = Some(usage.clone());
    let mut thread = inner.provider_thread.lock().expect("provider thread");
    if let Some(thread) = thread.as_mut() {
        thread.context_usage = Some(usage);
        thread.updated_at = UtcDateTime::now();
        send(
            inner,
            vec![ProviderAdapterV2Event::ProviderThreadUpdated {
                driver: inner.driver.clone(),
                provider_thread: thread.clone(),
                retained_native_turn_ids: None,
            }],
        );
    }
}

fn native_session_id(thread: &OrchestrationV2ProviderThread) -> Option<String> {
    thread
        .native_thread_ref
        .as_ref()
        .and_then(|reference| reference.native_id.as_ref())
        .map(|id| id.as_str().to_owned())
}

/// `session/new` and `session/load` require an absolute `cwd`. A relative path
/// resolves against the Turin process cwd, as upstream's `process.cwd()` does.
fn absolute_path(path: &Path) -> PathBuf {
    let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    if path.as_os_str().is_empty() {
        current
    } else if path.is_absolute() {
        path.to_path_buf()
    } else {
        current.join(path)
    }
}

fn absolute_cwd(cwd: Option<&str>) -> PathBuf {
    absolute_path(Path::new(cwd.map_or("", str::trim)))
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
        let cwd = absolute_cwd(input.runtime_policy.cwd.as_deref())
            .display()
            .to_string();
        let cwd_path = absolute_cwd(input.runtime_policy.cwd.as_deref());
        let runtime =
            AcpSessionRuntime::make(cursor_agent_runtime_options(&self.program, cwd_path))
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
        let session = CursorSession::from_runtime_with(
            runtime,
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
        self.inner
            .published
            .get()
            .unwrap_or(&self.inner.provider_session)
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
        let cwd = absolute_cwd(input.runtime_policy.cwd.as_deref());
        let native = input
            .existing_provider_thread
            .as_ref()
            .and_then(native_session_id);
        let opened = match native {
            Some(native) => self.load_session(&native, &cwd).await,
            None => self.new_session(&cwd).await,
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
        let cwd = absolute_cwd(
            input
                .runtime_policy
                .as_ref()
                .and_then(|policy| policy.cwd.as_deref()),
        );
        let opened = self.load_session(&native, &cwd).await.map_err(|error| {
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
        self.apply_interaction_mode(input.runtime_policy.interaction_mode)
            .await
            .map_err(|error| map_error(kind.clone(), error))?;
        let native = native_session_id(&input.provider_thread).unwrap_or_default();
        let context = TurnContext {
            scope: Scope {
                driver: self.inner.driver.clone(),
                thread_id: input.thread_id.clone(),
                run_id: input.run_id.clone(),
                root_node_id: input.root_node_id.clone(),
                provider_thread_id: input.provider_thread.id.clone(),
                provider_turn_id: derive::provider_turn(
                    &self.inner.driver,
                    &format!("{native}:{}", input.run_id.as_str()),
                ),
            },
            run_ordinal: input.run_ordinal,
            provider_turn_ordinal: input.provider_turn_ordinal,
            attempt_id: input.attempt_id.clone(),
        };
        self.run_turn(context, &input.message.text)
            .await
            .map(|_| ())
            .map_err(|error| map_error(kind, error))
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
        let reply = match self.waiting(request_id) {
            Some(Waiting::Ask(params)) => {
                let cancelled = input.decision == Some(ProviderApprovalDecision::Cancel);
                RuntimeReply::UserInput(extensions::ask_disposition_from_answers(
                    &params,
                    input.answers.as_ref().unwrap_or(&Default::default()),
                    cancelled,
                ))
            }
            Some(Waiting::Plan) => RuntimeReply::Plan(plan_disposition(input.decision)),
            _ => RuntimeReply::Approval(input.decision.unwrap_or(ProviderApprovalDecision::Cancel)),
        };
        self.answer_request(request_id, reply).map_err(|error| {
            map_error(
                ProviderAdapterErrorKind::RuntimeRequestResponse {
                    driver: self.inner.driver.clone(),
                    request_id: input.request_id.clone(),
                },
                error,
            )
        })
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
    // The client's handler map holds these closures, and the client lives inside
    // `SessionInner`. A strong `Arc` here would make the cycle permanent: the
    // cursor-agent child is never killed and open cards never resolve. Handlers
    // hold `Weak` and drop their strong reference before waiting on the user.
    {
        let weak = Arc::downgrade(inner);
        client.on_session_update(move |params| {
            let weak = weak.clone();
            async move {
                if let Some(inner) = weak.upgrade() {
                    for update in events::parse_session_update(&params) {
                        project_update(&inner, &update);
                    }
                }
                Ok(())
            }
        });
    }
    {
        let weak = Arc::downgrade(inner);
        client.on_request_permission(move |params, ctx| {
            let weak = weak.clone();
            async move { handle_permission(weak, params, ctx).await }
        });
    }
    register_ask(client, inner);
    register_plan(client, inner);
    register_todos(client, inner);
    // Acknowledged, not projected: the CLI ignores the reply, and the task and
    // image payloads have no turn item in this port.
    register_ack(client, turin_acp::cursor::TASK);
    register_ack(client, turin_acp::cursor::GENERATE_IMAGE);
}

async fn handle_permission(
    weak: Weak<SessionInner>,
    params: Value,
    ctx: RequestContext,
) -> Result<Value, AcpError> {
    let request = permissions::parse_permission_request(&params);
    let key = permissions::session_grant_key(&request);
    let cancelled = || permissions::permission_result(&permissions::PermissionAnswer::Cancelled);
    let Some(request_id) = request_id_of(&ctx.request_id) else {
        return Ok(cancelled());
    };
    let (tx, rx) = oneshot::channel();
    {
        let Some(inner) = weak.upgrade() else {
            return Ok(cancelled());
        };
        if key
            .as_ref()
            .is_some_and(|key| inner.grants.lock().expect("grants").contains(key))
        {
            return Ok(permissions::permission_result(
                &permissions::answer_for_decision(&request, ProviderApprovalDecision::Accept),
            ));
        }
        if permissions::is_ask_question_fallback(&request) {
            tracing::debug!(tool_call_id = ?request.tool_call_id, "cursor ask_question fallback permission");
        }
        let detail = permissions::card_detail(&request);
        let (card, events) = open_card(
            &inner,
            &request_id,
            permissions::request_kind(&request),
            CardSpec {
                node_kind: OrchestrationV2ExecutionNodeKind::ApprovalRequest,
                body: ItemBody::ApprovalRequest {
                    request_id: request_id.clone(),
                    request_kind: permissions::provider_request_kind(&request),
                    prompt: Some(detail.clone()),
                    options: permissions::approval_options(&request),
                },
                title: Some(detail),
            },
        );
        inner.pending.lock().expect("pending").insert(
            ctx.request_id.clone(),
            Pending {
                waiter: Waiter::Permission(tx),
                card,
            },
        );
        send(&inner, events);
    }
    let decision = rx.await.unwrap_or(ProviderApprovalDecision::Cancel);
    let answer = permissions::answer_for_decision(&request, decision);
    // A grant the agent cannot honour (no allow_once option) would cancel every later match.
    if let Some(key) = key
        && decision == ProviderApprovalDecision::AcceptForSession
        && matches!(answer, permissions::PermissionAnswer::Selected { .. })
        && let Some(inner) = weak.upgrade()
    {
        inner.grants.lock().expect("grants").insert(key);
    }
    Ok(permissions::permission_result(&answer))
}

fn register_ask(client: &AcpClient, inner: &Arc<SessionInner>) {
    let weak = Arc::downgrade(inner);
    client.on_ext_request(turin_acp::cursor::ASK_QUESTION, move |params, ctx| {
        let weak = weak.clone();
        async move {
            let parsed: AskQuestionParams = serde_json::from_value(params).map_err(|_| {
                AcpError::from(turin_acp::errors::AcpRequestError::invalid_params(
                    "Invalid cursor/ask_question payload.",
                ))
            })?;
            let cancelled =
                || extensions::nested_outcome(&extensions::ask_result(AskDisposition::Cancelled));
            let Some(request_id) = request_id_of(&ctx.request_id) else {
                return Ok(cancelled());
            };
            let (tx, rx) = oneshot::channel();
            {
                let Some(inner) = weak.upgrade() else {
                    return Ok(cancelled());
                };
                let questions = extensions::user_input_questions(&parsed);
                let (card, events) = open_card(
                    &inner,
                    &request_id,
                    OrchestrationV2RuntimeRequestKind::UserInput,
                    CardSpec {
                        node_kind: OrchestrationV2ExecutionNodeKind::UserInputRequest,
                        body: ItemBody::UserInputRequest {
                            request_id: request_id.clone(),
                            questions,
                        },
                        title: parsed.title.clone(),
                    },
                );
                inner.pending.lock().expect("pending").insert(
                    ctx.request_id.clone(),
                    Pending {
                        waiter: Waiter::Ask {
                            sender: tx,
                            params: parsed,
                        },
                        card,
                    },
                );
                send(&inner, events);
            }
            let disposition = rx.await.unwrap_or(AskDisposition::Cancelled);
            Ok(extensions::nested_outcome(&extensions::ask_result(
                disposition,
            )))
        }
    });
}

fn register_plan(client: &AcpClient, inner: &Arc<SessionInner>) {
    let weak = Arc::downgrade(inner);
    client.on_ext_request(turin_acp::cursor::CREATE_PLAN, move |params, ctx| {
        let weak = weak.clone();
        async move {
            let parsed: CreatePlanParams = serde_json::from_value(params).map_err(|_| {
                AcpError::from(turin_acp::errors::AcpRequestError::invalid_params(
                    "Invalid cursor/create_plan payload.",
                ))
            })?;
            let cancelled =
                || extensions::nested_outcome(&extensions::plan_result(PlanDisposition::Cancelled));
            let Some(request_id) = request_id_of(&ctx.request_id) else {
                return Ok(cancelled());
            };
            // A blank plan is never offered for approval.
            let Some(markdown) = extensions::plan_markdown(&parsed) else {
                return Ok(extensions::nested_outcome(&extensions::plan_result(
                    PlanDisposition::Rejected {
                        reason: Some("The plan is empty.".to_owned()),
                    },
                )));
            };
            let (tx, rx) = oneshot::channel();
            {
                let Some(inner) = weak.upgrade() else {
                    return Ok(cancelled());
                };
                let plan_native = format!("plan:{}", parsed.tool_call_id);
                let now = UtcDateTime::now();
                let plan_id = events::plan_id(&inner.driver, &plan_native);
                let steps = extensions::plan_steps(&parsed.todos);
                let (card, mut events) = open_card(
                    &inner,
                    &request_id,
                    OrchestrationV2RuntimeRequestKind::UserInput,
                    CardSpec {
                        node_kind: OrchestrationV2ExecutionNodeKind::UserInputRequest,
                        body: ItemBody::ProposedPlan {
                            plan_id: plan_id.clone(),
                            markdown: markdown.clone(),
                            streaming: false,
                        },
                        title: parsed.name.clone(),
                    },
                );
                events.extend(turn_events(&inner, |stream| {
                    stream.proposed_plan(&plan_id, &request_id, &markdown, now)
                }));
                if !steps.is_empty() {
                    events.extend(turn_events(&inner, |stream| {
                        stream.plan_update(
                            &format!("todos:{}", parsed.tool_call_id),
                            steps,
                            OrchestrationV2TurnItemStatus::Running,
                            events::plan_artifact_status(OrchestrationV2TurnItemStatus::Running),
                            now,
                        )
                    }));
                }
                inner.pending.lock().expect("pending").insert(
                    ctx.request_id.clone(),
                    Pending {
                        waiter: Waiter::Plan(tx),
                        card,
                    },
                );
                send(&inner, events);
            }
            let disposition = rx.await.unwrap_or(PlanDisposition::Cancelled);
            Ok(extensions::nested_outcome(&extensions::plan_result(
                disposition,
            )))
        }
    });
}

/// `cursor/update_todos`: the CLI sends it after the tool completes, so the list
/// is finished when it arrives.
fn register_todos(client: &AcpClient, inner: &Arc<SessionInner>) {
    let weak = Arc::downgrade(inner);
    client.on_ext_request(turin_acp::cursor::UPDATE_TODOS, move |params, _ctx| {
        let weak = weak.clone();
        async move {
            if let Some(inner) = weak.upgrade() {
                let Ok(parsed) = serde_json::from_value::<UpdateTodosParams>(params) else {
                    tracing::warn!("cursor/update_todos payload did not parse");
                    return Ok(json!({}));
                };
                let now = UtcDateTime::now();
                let native = format!("todos:{}", parsed.tool_call_id);
                let steps = extensions::plan_steps(&parsed.todos);
                let events = turn_events(&inner, |stream| {
                    stream.plan_update(
                        &native,
                        steps,
                        OrchestrationV2TurnItemStatus::Completed,
                        events::plan_artifact_status(OrchestrationV2TurnItemStatus::Completed),
                        now,
                    )
                });
                send(&inner, events);
            }
            Ok(json!({}))
        }
    });
}

fn register_ack(client: &AcpClient, method: &str) {
    client.on_ext_request(method, |_params, _ctx| async { Ok(json!({})) });
}

fn cursor_initialize_request() -> turin_acp::compat::InitializeRequest {
    AcpClient::cursor_initialize("turin", env!("CARGO_PKG_VERSION"))
}

fn cursor_session_runtime_options(spawn: AcpSpawnInput, cwd: PathBuf) -> AcpSessionRuntimeOptions {
    let init = cursor_initialize_request();
    AcpSessionRuntimeOptions {
        spawn,
        cwd,
        protocol_version: Some(init.protocol_version),
        resume_session_id: None,
        resume_method: None,
        session_load_timeout: None,
        session_load_replay_idle_gap: None,
        interrupt_prompt_on_cancel: true,
        cancel_meta: None,
        initialize_meta: init.meta,
        own_detached_process_group: false,
        own_descendant_process_groups: false,
        process_group_platform: None,
        process_group_termination_grace: None,
        cancel_behavior: CancelBehavior::Interrupt,
        cancel_timeout: None,
        client_capabilities: init.client_capabilities,
        client_info: init.client_info.unwrap_or(ImplementationInfo {
            name: "turin".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }),
        auth_method_id: Some(auth::CURSOR_LOGIN_METHOD.to_owned()),
        authenticate_eagerly: false,
        authenticate_on_auth_required: auth::AUTHENTICATE_ON_AUTH_REQUIRED,
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

fn cursor_agent_runtime_options(program: &str, cwd: PathBuf) -> AcpSessionRuntimeOptions {
    cursor_session_runtime_options(
        AcpSpawnInput {
            command: program.to_owned(),
            args: auth::ACP_ARGS.iter().map(|arg| (*arg).to_owned()).collect(),
            cwd: Some(cwd.clone()),
            env: None,
            extend_env: false,
        },
        cwd,
    )
}

fn connected_client_runtime_options() -> AcpSessionRuntimeOptions {
    cursor_session_runtime_options(
        AcpSpawnInput {
            command: String::new(),
            args: Vec::new(),
            cwd: None,
            env: None,
            extend_env: false,
        },
        PathBuf::from("/"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
    use turin_acp::{SpawnCommand, spawn};
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

    async fn next_event(events: &mut ProviderEventStream) -> ProviderAdapterV2Event {
        use futures::StreamExt;
        tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .expect("provider event within 5s")
            .expect("provider stream open")
            .expect("provider event ok")
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
        let mut events = session.events();
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
        // Cards arrive in the order the peer sends them: approval, question, plan.
        let mut answered = 0;
        while answered < 3 {
            let ProviderAdapterV2Event::RuntimeRequestUpdated {
                runtime_request, ..
            } = next_event(&mut events).await
            else {
                continue;
            };
            if runtime_request.status != OrchestrationV2RuntimeRequestStatus::Pending {
                continue;
            }
            let id = runtime_request.id.as_str().to_owned();
            match answered {
                0 => session
                    .answer_request(&id, RuntimeReply::Approval(Decision::AcceptForSession))
                    .unwrap(),
                1 => session
                    .answer_request(
                        &id,
                        RuntimeReply::UserInput(AskDisposition::Answered(vec![
                            turin_acp::cursor::AskAnswer {
                                question_id: "q1".to_owned(),
                                selected_option_ids: vec!["a".to_owned()],
                            },
                        ])),
                    )
                    .unwrap(),
                _ => session
                    .answer_request(
                        &id,
                        RuntimeReply::Plan(PlanDisposition::Accepted {
                            plan_uri: Some("file:///plan.md".to_owned()),
                        }),
                    )
                    .unwrap(),
            }
            answered += 1;
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
    async fn interaction_mode_selects_plan_or_agent_on_the_open_process() {
        let (client, mut read, mut write) = pair();
        let peer = tokio::spawn(async move {
            let init = read_frame(&mut read).await;
            write_frame(&mut write, v1_initialized(&init["id"])).await;
            let created = read_frame(&mut read).await;
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": created["id"], "result": {"sessionId": "sess-mode"}}),
            )
            .await;
            for expected in ["plan", "agent"] {
                let mode = read_frame(&mut read).await;
                assert_eq!(mode["method"], "session/set_mode");
                assert_eq!(mode["params"]["sessionId"], "sess-mode");
                assert_eq!(mode["params"]["modeId"], expected);
                write_frame(
                    &mut write,
                    json!({"jsonrpc":"2.0","id": mode["id"], "result": {}}),
                )
                .await;
            }
        });
        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        session.new_session(Path::new("/work")).await.unwrap();
        session
            .apply_interaction_mode(ProviderInteractionMode::Plan)
            .await
            .unwrap();
        session
            .apply_interaction_mode(ProviderInteractionMode::Default)
            .await
            .unwrap();
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn interrupt_sends_session_cancel() {
        let (client, mut read, mut write) = pair();
        let (sent_tx, sent_rx) = oneshot::channel();
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
            let _ = sent_tx.send(());
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
        tokio::time::timeout(Duration::from_secs(5), sent_rx)
            .await
            .unwrap()
            .unwrap();
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

    #[test]
    fn missing_or_relative_cwd_resolves_to_an_absolute_path() {
        assert!(absolute_cwd(None).is_absolute());
        assert!(absolute_cwd(Some("  ")).is_absolute());
        assert_eq!(absolute_cwd(Some("/work")), PathBuf::from("/work"));
        let relative = absolute_cwd(Some("work/app"));
        assert!(relative.is_absolute());
        assert!(relative.ends_with("work/app"));
    }

    #[tokio::test]
    async fn new_session_sends_an_absolute_cwd_on_the_wire() {
        let (client, mut read, mut write) = pair();
        let peer = tokio::spawn(async move {
            let init = read_frame(&mut read).await;
            write_frame(&mut write, v1_initialized(&init["id"])).await;
            let created = read_frame(&mut read).await;
            assert_eq!(created["method"], "session/new");
            assert!(Path::new(created["params"]["cwd"].as_str().unwrap()).is_absolute());
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": created["id"], "result": {"sessionId": "sess-cwd"}}),
            )
            .await;
        });
        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        let thread = session
            .new_session(Path::new("relative/project"))
            .await
            .unwrap();
        assert_eq!(thread.session_id, "sess-cwd");
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn failed_turn_resolves_open_permission_cards() {
        let (client, mut read, mut write) = pair();
        let peer = tokio::spawn(async move {
            let init = read_frame(&mut read).await;
            write_frame(&mut write, v1_initialized(&init["id"])).await;
            let created = read_frame(&mut read).await;
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": created["id"], "result": {"sessionId": "sess-f"}}),
            )
            .await;
            let prompt = read_frame(&mut read).await;
            assert_eq!(prompt["method"], "session/prompt");
            write_frame(
                &mut write,
                json!({
                    "jsonrpc": "2.0",
                    "id": 21,
                    "method": "session/request_permission",
                    "params": {
                        "sessionId": "sess-f",
                        "toolCall": {"toolCallId": "call-f", "title": "Run ls", "kind": "execute", "rawInput": {"command": "ls"}},
                        "options": [{"optionId": "once", "name": "Allow once", "kind": "allow_once"}]
                    }
                }),
            )
            .await;
            // The agent dies mid-turn: the prompt fails before the card is answered.
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": prompt["id"], "error": {"code": -32603, "message": "agent exited"}}),
            )
            .await;
            let answered = read_frame(&mut read).await;
            assert_eq!(answered["id"], 21);
            assert_eq!(answered["result"]["outcome"]["outcome"], "cancelled");
        });
        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        session.new_session(Path::new("/work")).await.unwrap();
        let error = session.prompt("hi", &[]).await.unwrap_err();
        assert!(error.to_string().contains("agent exited"));
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn dropping_the_session_kills_the_agent_process() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let script = format!("echo $$ > '{}'; exec sleep 60", pid_file.display());
        let client = spawn(SpawnCommand {
            program: "sh",
            args: &["-c", script.as_str()],
            cwd: None,
        })
        .await
        .unwrap();
        let session = CursorSession::from_client(client, None);
        let pid = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(&pid_file)
                    && let Ok(pid) = pid.trim().parse::<u32>()
                {
                    return pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        drop(session);
        // Dropping the guard SIGKILLs the group but does not reap it: tokio
        // reaps the leader in the background, and until then `kill -0` still
        // succeeds on the zombie. Wait for the pid to disappear, bounded.
        let gone = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let alive = std::process::Command::new("kill")
                    .arg("-0")
                    .arg(pid.to_string())
                    .status()
                    .unwrap()
                    .success();
                if !alive {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            gone.is_ok(),
            "cursor-agent child {pid} survived the session drop"
        );
    }

    /// Peer side of `initialize` and `session/new`.
    async fn peer_open(read: &mut DuplexStream, write: &mut DuplexStream, session_id: &str) {
        let init = read_frame(read).await;
        write_frame(write, v1_initialized(&init["id"])).await;
        let created = read_frame(read).await;
        write_frame(
            write,
            json!({"jsonrpc":"2.0","id": created["id"], "result": {"sessionId": session_id}}),
        )
        .await;
    }

    fn update(params: Value) -> Value {
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "sess-t", "update": params}})
    }

    fn turn_context(session: &CursorSession, thread: &str) -> TurnContext {
        let driver = session.inner.driver.clone();
        let thread_id = ThreadId::new(thread).unwrap();
        let run_id = derive::run(&thread_id, 1);
        TurnContext {
            scope: Scope {
                driver: driver.clone(),
                thread_id,
                run_id: run_id.clone(),
                root_node_id: derive::root_node(&run_id),
                provider_thread_id: derive::provider_thread(
                    &driver,
                    Some(&session.inner.instance_id),
                    "sess-t",
                ),
                provider_turn_id: derive::provider_turn(
                    &driver,
                    &format!("sess-t:{}", run_id.as_str()),
                ),
            },
            run_ordinal: 1,
            provider_turn_ordinal: 1,
            attempt_id: derive::run_attempt(&run_id, 1),
        }
    }

    /// Collects provider events up to the turn's terminal event. Runtime cards are
    /// answered with `Accept` as they arrive, so a turn never waits on the test.
    async fn collect_turn(session: &CursorSession, events: &mut ProviderEventStream) -> Vec<Value> {
        let mut seen = Vec::new();
        loop {
            let event = next_event(events).await;
            let value = serde_json::to_value(&event).unwrap();
            if let ProviderAdapterV2Event::RuntimeRequestUpdated {
                runtime_request, ..
            } = &event
                && runtime_request.status == OrchestrationV2RuntimeRequestStatus::Pending
            {
                session
                    .respond_to_runtime_request(ProviderAdapterV2RuntimeRequestResponseInput {
                        request_id: runtime_request.id.clone(),
                        decision: Some(Decision::Accept),
                        answers: None,
                        response: None,
                    })
                    .await
                    .unwrap();
            }
            let terminal = matches!(event, ProviderAdapterV2Event::TurnTerminal { .. });
            seen.push(value);
            if terminal {
                return seen;
            }
        }
    }

    /// Turn items as the client sees them, in emission order.
    fn turn_items(events: &[Value]) -> Vec<Value> {
        events
            .iter()
            .filter(|event| event["type"] == "turn_item.updated")
            .map(|event| event["turn_item"].clone())
            .collect()
    }

    fn last_item_with_id(items: &[Value], id: &Value) -> Value {
        items
            .iter()
            .rev()
            .find(|item| &item["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("no item {id}"))
    }

    #[tokio::test]
    async fn assistant_text_accumulates_into_one_streaming_item() {
        let (client, mut read, mut write) = pair();
        let peer = tokio::spawn(async move {
            peer_open(&mut read, &mut write, "sess-t").await;
            let prompt = read_frame(&mut read).await;
            for text in ["Hel", "lo"] {
                write_frame(
                    &mut write,
                    update(json!({"sessionUpdate": "agent_message_chunk", "messageId": "m1", "content": {"type": "text", "text": text}})),
                )
                .await;
            }
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": prompt["id"], "result": {"stopReason": "end_turn"}}),
            )
            .await;
        });
        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        session.new_session(Path::new("/work")).await.unwrap();
        let mut events = session.events();
        let context = turn_context(&session, "thread-text");
        let run = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(context, "hi").await }
        });
        let seen = collect_turn(&session, &mut events).await;
        run.await.unwrap().unwrap();
        peer.await.unwrap();

        let assistant: Vec<Value> = turn_items(&seen)
            .into_iter()
            .filter(|item| item["type"] == "assistant_message")
            .collect();
        let id = assistant[0]["id"].clone();
        // Each chunk re-emits the same item with the text so far, then one completed update.
        let texts: Vec<(&Value, &Value, &Value)> = assistant
            .iter()
            .map(|item| (&item["text"], &item["status"], &item["streaming"]))
            .collect();
        assert_eq!(texts.len(), 3, "{texts:?}");
        assert!(assistant.iter().all(|item| item["id"] == id));
        assert_eq!(assistant[0]["text"], "Hel");
        assert_eq!(assistant[0]["status"], "running");
        assert_eq!(assistant[0]["streaming"], true);
        let last = last_item_with_id(&assistant, &id);
        assert_eq!(last["text"], "Hello");
        assert_eq!(last["status"], "completed");
        assert_eq!(last["streaming"], false);
    }

    #[tokio::test]
    async fn a_new_message_id_starts_a_new_assistant_item() {
        let (client, mut read, mut write) = pair();
        let peer = tokio::spawn(async move {
            peer_open(&mut read, &mut write, "sess-t").await;
            let prompt = read_frame(&mut read).await;
            for (message, text) in [("m1", "First"), ("m2", "Second")] {
                write_frame(
                    &mut write,
                    update(json!({"sessionUpdate": "agent_message_chunk", "messageId": message, "content": {"type": "text", "text": text}})),
                )
                .await;
            }
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": prompt["id"], "result": {"stopReason": "end_turn"}}),
            )
            .await;
        });
        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        session.new_session(Path::new("/work")).await.unwrap();
        let mut events = session.events();
        let context = turn_context(&session, "thread-msg");
        let run = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(context, "hi").await }
        });
        let seen = collect_turn(&session, &mut events).await;
        run.await.unwrap().unwrap();
        peer.await.unwrap();
        let completed: Vec<Value> = turn_items(&seen)
            .into_iter()
            .filter(|item| item["type"] == "assistant_message" && item["status"] == "completed")
            .collect();
        assert_eq!(completed.len(), 2);
        assert_eq!(completed[0]["text"], "First");
        assert_eq!(completed[1]["text"], "Second");
        assert_ne!(completed[0]["id"], completed[1]["id"]);
    }

    #[tokio::test]
    async fn tool_calls_stream_output_and_project_command_file_and_search_items() {
        let (client, mut read, mut write) = pair();
        let peer = tokio::spawn(async move {
            peer_open(&mut read, &mut write, "sess-t").await;
            let prompt = read_frame(&mut read).await;
            for params in [
                json!({"sessionUpdate": "tool_call", "toolCallId": "call-1", "title": "Run ls", "kind": "execute", "status": "pending", "rawInput": {"command": "ls -la"}}),
                json!({"sessionUpdate": "tool_call_update", "toolCallId": "call-1", "status": "in_progress", "content": [{"type": "content", "content": {"type": "text", "text": "total 0\n"}}]}),
                json!({"sessionUpdate": "tool_call_update", "toolCallId": "call-1", "status": "completed", "content": [{"type": "content", "content": {"type": "text", "text": "total 0\ndone\n"}}], "rawOutput": {"exitCode": 0}}),
                json!({"sessionUpdate": "tool_call", "toolCallId": "call-2", "title": "Edit a.rs", "kind": "edit", "status": "completed", "locations": [{"path": "src/a.rs"}], "content": [{"type": "diff", "path": "src/a.rs", "oldText": "a", "newText": "b"}]}),
                json!({"sessionUpdate": "tool_call", "toolCallId": "call-3", "title": "Find", "kind": "search", "status": "completed", "rawInput": {"pattern": "TODO"}, "rawOutput": {"files": ["src/a.rs", "src/b.rs"]}}),
            ] {
                write_frame(&mut write, update(params)).await;
            }
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": prompt["id"], "result": {"stopReason": "end_turn"}}),
            )
            .await;
        });
        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        session.new_session(Path::new("/work")).await.unwrap();
        let mut events = session.events();
        let context = turn_context(&session, "thread-tools");
        let run = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(context, "hi").await }
        });
        let seen = collect_turn(&session, &mut events).await;
        run.await.unwrap().unwrap();
        peer.await.unwrap();

        let items = turn_items(&seen);
        let command_id = items
            .iter()
            .find(|item| item["type"] == "command_execution")
            .unwrap()["id"]
            .clone();
        let streaming = items
            .iter()
            .filter(|item| item["id"] == command_id)
            .map(|item| item["output"].clone())
            .collect::<Vec<_>>();
        // Output streams in as the tool updates; the last update is the full text.
        assert!(streaming.contains(&json!("total 0\n")), "{streaming:?}");
        assert_eq!(streaming.last().unwrap(), "total 0\ndone\n");
        let command = last_item_with_id(&items, &command_id);
        assert_eq!(command["input"], "ls -la");
        assert_eq!(command["exitCode"], 0);
        assert_eq!(command["status"], "completed");

        let file = items
            .iter()
            .rev()
            .find(|item| item["type"] == "file_change")
            .unwrap();
        assert_eq!(file["fileName"], "src/a.rs");
        assert_eq!(file["oldStr"], "a");
        assert_eq!(file["newStr"], "b");

        let search = items
            .iter()
            .rev()
            .find(|item| item["type"] == "file_search")
            .unwrap();
        assert_eq!(search["pattern"], "TODO");
        assert_eq!(search["results"][0]["fileName"], "src/a.rs");
        assert_eq!(search["results"][1]["fileName"], "src/b.rs");

        let nodes: Vec<&Value> = seen
            .iter()
            .filter(|event| event["type"] == "node.updated")
            .map(|event| &event["node"]["kind"])
            .collect();
        assert!(nodes.iter().any(|kind| **kind == "tool_call"));
    }

    #[tokio::test]
    async fn failed_search_keeps_its_error_under_the_searched_path() {
        // Upstream adapter.test.ts: a failed grep shows its error on the searched path.
        let (client, mut read, mut write) = pair();
        let peer = tokio::spawn(async move {
            peer_open(&mut read, &mut write, "sess-t").await;
            let prompt = read_frame(&mut read).await;
            write_frame(
                &mut write,
                update(json!({"sessionUpdate": "tool_call", "toolCallId": "call-f", "title": "Grep", "kind": "search", "status": "failed", "rawInput": {"pattern": "TODO", "path": "src"}, "content": [{"type": "content", "content": {"type": "text", "text": "search failed"}}]})),
            )
            .await;
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": prompt["id"], "result": {"stopReason": "end_turn"}}),
            )
            .await;
        });
        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        session.new_session(Path::new("/work")).await.unwrap();
        let mut events = session.events();
        let context = turn_context(&session, "thread-search");
        let run = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(context, "hi").await }
        });
        let seen = collect_turn(&session, &mut events).await;
        run.await.unwrap().unwrap();
        peer.await.unwrap();
        let search = turn_items(&seen)
            .into_iter()
            .rev()
            .find(|item| item["type"] == "file_search")
            .unwrap();
        assert_eq!(search["pattern"], "TODO");
        assert_eq!(search["status"], "failed");
        assert_eq!(search["results"][0]["fileName"], "src");
        assert_eq!(search["results"][0]["preview"], "search failed");
    }

    #[tokio::test]
    async fn acp_plan_and_cursor_todos_project_todo_lists_without_cancelled_rows() {
        let (client, mut read, mut write) = pair();
        let peer = tokio::spawn(async move {
            peer_open(&mut read, &mut write, "sess-t").await;
            let prompt = read_frame(&mut read).await;
            write_frame(
                &mut write,
                update(json!({"sessionUpdate": "plan", "entries": [{"content": "Inspect", "status": "in_progress", "priority": "high"}, {"content": "Ship", "status": "pending", "priority": "low"}]})),
            )
            .await;
            write_frame(
                &mut write,
                json!({"jsonrpc": "2.0", "id": 30, "method": "cursor/update_todos", "params": {"toolCallId": "t1", "merge": false, "todos": [
                    {"id": "a", "content": "Read", "status": "completed"},
                    {"id": "b", "content": "Skip", "status": "cancelled"},
                    {"id": "c", "content": "   ", "status": "pending"},
                    {"id": "d", "content": "Write", "status": "in_progress"}
                ]}}),
            )
            .await;
            let ack = read_frame(&mut read).await;
            assert_eq!(ack["result"], json!({}));
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": prompt["id"], "result": {"stopReason": "end_turn"}}),
            )
            .await;
        });
        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        session.new_session(Path::new("/work")).await.unwrap();
        let mut events = session.events();
        let context = turn_context(&session, "thread-plan");
        let run = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(context, "hi").await }
        });
        let seen = collect_turn(&session, &mut events).await;
        run.await.unwrap().unwrap();
        peer.await.unwrap();

        let plans: Vec<&Value> = seen
            .iter()
            .filter(|event| event["type"] == "plan.updated")
            .map(|event| &event["plan"])
            .collect();
        assert_eq!(plans[0]["kind"], "todo_list");
        assert_eq!(plans[0]["steps"][0]["text"], "Inspect");
        assert_eq!(plans[0]["steps"][0]["status"], "running");
        // The open ACP plan is re-emitted when the turn ends, so the todo list is
        // found by its first step rather than by position.
        let todo_steps = &plans
            .iter()
            .rev()
            .find(|plan| plan["steps"][0]["text"] == "Read")
            .unwrap()["steps"];
        assert_eq!(todo_steps.as_array().unwrap().len(), 2);
        assert_eq!(todo_steps[0]["id"], "todo-1");
        assert_eq!(todo_steps[0]["text"], "Read");
        assert_eq!(todo_steps[0]["status"], "completed");
        assert_eq!(todo_steps[1]["id"], "todo-2");
        assert_eq!(todo_steps[1]["text"], "Write");
        assert_eq!(todo_steps[1]["status"], "running");
        let todo_items: Vec<Value> = turn_items(&seen)
            .into_iter()
            .filter(|item| item["type"] == "todo_list")
            .collect();
        assert_eq!(
            todo_items.last().unwrap()["steps"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn approval_and_question_cards_carry_their_content_on_turn_items() {
        let (client, mut read, mut write) = pair();
        let peer = tokio::spawn(async move {
            peer_open(&mut read, &mut write, "sess-t").await;
            let prompt = read_frame(&mut read).await;
            write_frame(
                &mut write,
                json!({
                    "jsonrpc": "2.0",
                    "id": 7,
                    "method": "session/request_permission",
                    "params": {
                        "sessionId": "sess-t",
                        "toolCall": {"toolCallId": "call-p", "title": "Run ls", "kind": "execute", "rawInput": {"command": "ls"}},
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
            assert_eq!(permission["result"]["outcome"]["optionId"], "once");
            write_frame(
                &mut write,
                json!({"jsonrpc": "2.0", "id": 8, "method": "cursor/ask_question", "params": {
                    "toolCallId": "tool-q", "title": "Pick",
                    "questions": [{"id": "q1", "prompt": "Which?", "allowMultiple": false, "options": [{"id": "a", "label": "A"}]}]
                }}),
            )
            .await;
            let asked = read_frame(&mut read).await;
            assert_eq!(asked["result"]["outcome"]["outcome"], "skipped");
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": prompt["id"], "result": {"stopReason": "end_turn"}}),
            )
            .await;
        });
        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        session.new_session(Path::new("/work")).await.unwrap();
        let mut events = session.events();
        let context = turn_context(&session, "thread-cards");
        let run = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(context, "hi").await }
        });
        let seen = collect_turn(&session, &mut events).await;
        run.await.unwrap().unwrap();
        peer.await.unwrap();

        let items = turn_items(&seen);
        let approval = items
            .iter()
            .rev()
            .find(|item| item["type"] == "approval_request")
            .unwrap();
        // The command is the detail when rawInput has one, as upstream shows it.
        assert_eq!(approval["prompt"], "ls");
        let labels: Vec<&str> = approval["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|option| option["label"].as_str().unwrap())
            .collect();
        assert!(labels.contains(&permissions::ALLOW_FOR_THREAD_LABEL));
        assert!(labels.contains(&permissions::ALWAYS_ALLOW_LABEL));
        assert!(labels.contains(&"Decline"));
        assert_eq!(approval["status"], "completed");

        let question = items
            .iter()
            .rev()
            .find(|item| item["type"] == "user_input_request")
            .unwrap();
        assert_eq!(question["questions"][0]["id"], "q1");
        assert_eq!(question["questions"][0]["question"], "Which?");
        assert_eq!(question["questions"][0]["options"][0]["label"], "A");
    }

    #[tokio::test]
    async fn create_plan_card_carries_the_plan_markdown() {
        let (client, mut read, mut write) = pair();
        let peer = tokio::spawn(async move {
            peer_open(&mut read, &mut write, "sess-t").await;
            let prompt = read_frame(&mut read).await;
            write_frame(
                &mut write,
                json!({"jsonrpc": "2.0", "id": 9, "method": "cursor/create_plan", "params": {
                    "toolCallId": "p1", "plan": "# Plan\n- one", "todos": [{"id": "x", "content": "One", "status": "pending"}]
                }}),
            )
            .await;
            let planned = read_frame(&mut read).await;
            assert_eq!(planned["result"]["outcome"]["outcome"], "accepted");
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": prompt["id"], "result": {"stopReason": "end_turn"}}),
            )
            .await;
        });
        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        session.new_session(Path::new("/work")).await.unwrap();
        let mut events = session.events();
        let context = turn_context(&session, "thread-plan-card");
        let run = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(context, "hi").await }
        });
        let seen = collect_turn(&session, &mut events).await;
        run.await.unwrap().unwrap();
        peer.await.unwrap();

        let artifact = seen
            .iter()
            .find(|event| {
                event["type"] == "plan.updated" && event["plan"]["kind"] == "proposed_plan"
            })
            .unwrap();
        assert_eq!(artifact["plan"]["markdown"], "# Plan\n- one");
        let proposed = turn_items(&seen)
            .into_iter()
            .rev()
            .find(|item| item["type"] == "proposed_plan")
            .unwrap();
        assert_eq!(proposed["markdown"], "# Plan\n- one");
        assert_eq!(proposed["status"], "completed");
    }

    #[tokio::test]
    async fn interrupted_turn_reports_interrupted_not_cancelled() {
        let (client, mut read, mut write) = pair();
        let (sent_tx, sent_rx) = oneshot::channel();
        let peer = tokio::spawn(async move {
            peer_open(&mut read, &mut write, "sess-t").await;
            let prompt = read_frame(&mut read).await;
            let _ = sent_tx.send(());
            write_frame(
                &mut write,
                update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "working"}})),
            )
            .await;
            let cancel = read_frame(&mut read).await;
            assert_eq!(cancel["method"], "session/cancel");
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": prompt["id"], "result": {"stopReason": "cancelled"}}),
            )
            .await;
        });
        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        session.new_session(Path::new("/work")).await.unwrap();
        let mut events = session.events();
        let context = turn_context(&session, "thread-interrupt");
        let run = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(context, "stop").await }
        });
        tokio::time::timeout(Duration::from_secs(5), sent_rx)
            .await
            .unwrap()
            .unwrap();
        session.cancel_turn().await.unwrap();
        let seen = collect_turn(&session, &mut events).await;
        assert_eq!(
            run.await.unwrap().unwrap().stop_reason.as_deref(),
            Some("cancelled")
        );
        peer.await.unwrap();
        let terminal = seen.last().unwrap();
        assert_eq!(terminal["type"], "turn.terminal");
        assert_eq!(terminal["status"], "interrupted");
        let assistant = turn_items(&seen)
            .into_iter()
            .rev()
            .find(|item| item["type"] == "assistant_message")
            .unwrap();
        // Upstream closes open text as completed when the turn ends, whatever the status.
        assert_eq!(assistant["status"], "completed");
        assert_eq!(assistant["streaming"], false);
    }

    #[tokio::test]
    async fn provider_session_reports_the_published_record() {
        let (client, mut read, mut write) = pair();
        let peer = tokio::spawn(async move {
            let init = read_frame(&mut read).await;
            write_frame(&mut write, v1_initialized(&init["id"])).await;
        });
        let session = CursorSession::from_client(client, None);
        assert_eq!(
            session.provider_session().status,
            OrchestrationV2ProviderSessionStatus::Starting
        );
        session.initialize().await.unwrap();
        session.publish_session();
        assert_eq!(
            session.provider_session().status,
            OrchestrationV2ProviderSessionStatus::Ready
        );
        assert_eq!(
            session.provider_session().capabilities,
            session.capabilities()
        );
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn session_grant_does_not_cover_a_request_without_title_or_input() {
        let (client, mut read, mut write) = pair();
        let peer = tokio::spawn(async move {
            peer_open(&mut read, &mut write, "sess-grant").await;
            for (id, call) in [(41, "call-a"), (42, "call-b")] {
                write_frame(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": "sess-grant",
                            "toolCall": {"toolCallId": call, "kind": "execute"},
                            "options": [{"optionId": "once", "name": "Allow once", "kind": "allow_once"}]
                        }
                    }),
                )
                .await;
            }
            let first = read_frame(&mut read).await;
            assert_eq!(first["result"]["outcome"]["optionId"], "once");
            let second = read_frame(&mut read).await;
            assert_eq!(second["id"], 42);
            assert_eq!(second["result"]["outcome"]["outcome"], "cancelled");
        });
        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        session.new_session(Path::new("/work")).await.unwrap();
        let mut events = session.events();
        // Each request gets its own card: an untitled, input-less request is never granted.
        let mut answered = 0;
        while answered < 2 {
            let event = next_event(&mut events).await;
            if let ProviderAdapterV2Event::RuntimeRequestUpdated {
                runtime_request, ..
            } = event
                && runtime_request.status == OrchestrationV2RuntimeRequestStatus::Pending
            {
                let decision = if answered == 0 {
                    Decision::AcceptForSession
                } else {
                    Decision::Cancel
                };
                session
                    .respond_to_runtime_request(ProviderAdapterV2RuntimeRequestResponseInput {
                        request_id: runtime_request.id,
                        decision: Some(decision),
                        answers: None,
                        response: None,
                    })
                    .await
                    .unwrap();
                answered += 1;
            }
        }
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn dropping_a_turn_mid_prompt_closes_it_as_interrupted_and_cancels() {
        let (client, mut read, mut write) = pair();
        let (sent_tx, sent_rx) = oneshot::channel();
        let peer = tokio::spawn(async move {
            peer_open(&mut read, &mut write, "sess-drop").await;
            let _prompt = read_frame(&mut read).await;
            let _ = sent_tx.send(());
            let cancel = read_frame(&mut read).await;
            assert_eq!(cancel["method"], "session/cancel");
            assert_eq!(cancel["params"]["sessionId"], "sess-drop");
        });
        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        session.new_session(Path::new("/work")).await.unwrap();
        let mut events = session.events();
        let context = turn_context(&session, "thread-drop");
        let run = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(context, "long").await }
        });
        tokio::time::timeout(Duration::from_secs(5), sent_rx)
            .await
            .unwrap()
            .unwrap();
        run.abort();
        let seen = collect_turn(&session, &mut events).await;
        let terminal = seen.last().unwrap();
        assert_eq!(terminal["type"], "turn.terminal");
        assert_eq!(terminal["status"], "interrupted");
        // The turn is closed, so a late update is not projected into it.
        assert!(session.inner.active_turn.lock().unwrap().is_none());
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn approval_card_falls_back_to_the_session_when_nothing_names_the_tool() {
        let (client, mut read, mut write) = pair();
        let peer = tokio::spawn(async move {
            peer_open(&mut read, &mut write, "sess-t").await;
            let prompt = read_frame(&mut read).await;
            write_frame(
                &mut write,
                json!({
                    "jsonrpc": "2.0",
                    "id": 51,
                    "method": "session/request_permission",
                    "params": {
                        "sessionId": "sess-t",
                        "toolCall": {"toolCallId": "call-blank", "kind": "other"},
                        "options": [{"optionId": "once", "name": "Allow once", "kind": "allow_once"}]
                    }
                }),
            )
            .await;
            let answered = read_frame(&mut read).await;
            assert_eq!(answered["result"]["outcome"]["optionId"], "once");
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": prompt["id"], "result": {"stopReason": "end_turn"}}),
            )
            .await;
        });
        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        session.new_session(Path::new("/work")).await.unwrap();
        let mut events = session.events();
        let context = turn_context(&session, "thread-blank-card");
        let run = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(context, "hi").await }
        });
        let seen = collect_turn(&session, &mut events).await;
        run.await.unwrap().unwrap();
        peer.await.unwrap();
        let approval = turn_items(&seen)
            .into_iter()
            .find(|item| item["type"] == "approval_request")
            .unwrap();
        assert_eq!(approval["prompt"], "Session sess-t");
    }

    #[tokio::test]
    async fn a_blank_plan_is_rejected_without_a_card() {
        let (client, mut read, mut write) = pair();
        let peer = tokio::spawn(async move {
            peer_open(&mut read, &mut write, "sess-t").await;
            let prompt = read_frame(&mut read).await;
            write_frame(
                &mut write,
                json!({"jsonrpc": "2.0", "id": 61, "method": "cursor/create_plan", "params": {
                    "toolCallId": "p-blank", "plan": "   \n", "overview": " ", "todos": [
                        {"id": "x", "content": " ", "status": "pending"},
                        {"id": "y", "content": "Gone", "status": "cancelled"}
                    ]
                }}),
            )
            .await;
            let planned = read_frame(&mut read).await;
            assert_eq!(planned["result"]["outcome"]["outcome"], "rejected");
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": prompt["id"], "result": {"stopReason": "end_turn"}}),
            )
            .await;
        });
        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        session.new_session(Path::new("/work")).await.unwrap();
        let mut events = session.events();
        let context = turn_context(&session, "thread-blank-plan");
        let run = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(context, "hi").await }
        });
        let seen = collect_turn(&session, &mut events).await;
        run.await.unwrap().unwrap();
        peer.await.unwrap();
        assert!(
            seen.iter()
                .all(|event| event["type"] != "runtime_request.updated")
        );
        assert!(
            turn_items(&seen)
                .iter()
                .all(|item| item["type"] != "proposed_plan")
        );
    }

    #[tokio::test]
    async fn a_session_grant_without_allow_once_is_not_recorded() {
        let (client, mut read, mut write) = pair();
        let peer = tokio::spawn(async move {
            peer_open(&mut read, &mut write, "sess-t").await;
            let prompt = read_frame(&mut read).await;
            for id in [71, 72] {
                write_frame(
                    &mut write,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": "sess-t",
                            "toolCall": {"toolCallId": format!("call-{id}"), "title": "Deploy", "kind": "execute", "rawInput": {"command": "deploy"}},
                            "options": [{"optionId": "no", "name": "Reject", "kind": "reject_once"}]
                        }
                    }),
                )
                .await;
                let answered = read_frame(&mut read).await;
                assert_eq!(answered["result"]["outcome"]["outcome"], "cancelled");
            }
            write_frame(
                &mut write,
                json!({"jsonrpc":"2.0","id": prompt["id"], "result": {"stopReason": "end_turn"}}),
            )
            .await;
        });
        let session = CursorSession::from_client(client, None);
        session.initialize().await.unwrap();
        session.new_session(Path::new("/work")).await.unwrap();
        let mut events = session.events();
        let context = turn_context(&session, "thread-grant");
        let run = tokio::spawn({
            let session = session.clone();
            async move { session.run_turn(context, "hi").await }
        });
        let seen = collect_turn(&session, &mut events).await;
        run.await.unwrap().unwrap();
        peer.await.unwrap();
        // The second identical request still gets its own card: nothing was granted.
        let approvals: std::collections::HashSet<String> = turn_items(&seen)
            .into_iter()
            .filter(|item| item["type"] == "approval_request")
            .map(|item| item["id"].to_string())
            .collect();
        assert_eq!(approvals.len(), 2);
    }
}
