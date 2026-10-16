//! Port of upstream/apps/server/src/provider/ProviderAuthService.ts
//!
//! Routes sign-in operations to the provider instance's [`ProviderAuthController`], and owns what
//! the controllers cannot: one credential change at a time, rejecting a change while another
//! instance with the same credential binding is mid-change, and releasing every provider session
//! the new credentials would invalidate before the controller starts or finishes.
//!
//! Upstream reads three services; here they are the narrow ports below so the logic is testable
//! without a database or live adapters. [`ProviderInstanceRegistry`], [`ProjectionStore`] and
//! [`ProviderSessionManager`] implement them.
//!
//! Deviation: `registry.getInstance`/`listInstances` are synchronous here (the Rust registry reads
//! a mutex), so `getController` and the shared-binding scan cannot be interrupted between reads.
//! The instance registry subscription is still opened before the first controller lookup, so a
//! replacement during that lookup is not missed.
//!
//! Deviation: the stop-sessions failures carry no `cause`. Upstream's `mapError(() => failure(..))`
//! also drops it, so private diagnostics never reach the caller.

#![allow(clippy::result_large_err)]

use std::{
    collections::HashSet,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use async_trait::async_trait;
use futures::{Stream, StreamExt, stream::BoxStream};
use tokio::sync::Mutex;
use turin_contracts::{
    ProviderSessionId, ThreadId,
    orchestration_v2::{OrchestrationV2ProviderSession, OrchestrationV2ProviderSessionStatus},
    provider_instance::ProviderInstanceId,
    provider_setup::{
        ChatGptReconnectProfile, ChatGptTransferredProfile, ProviderAuthCallbackMode as WireMode,
        ProviderAuthRespondInput, ProviderAuthStartInput, ProviderAuthState, ProviderSetupError,
    },
};
use turin_orchestration::{
    core::provider_session_manager::{ProviderSessionManager, ProviderSessionReleaseReason},
    projection::store::{
        ProjectionStore, canonical::ProjectionRecordField, recovery::ProjectionRecoveryKind,
    },
};
use turin_provider_core::{
    ProviderInstanceRegistry,
    auth::{
        ProviderAuthCallbackMode, ProviderAuthCompleteInput, ProviderAuthController,
        ProviderCredentialBinding,
    },
    rt::BoxFuture,
};

/// A registered provider instance as sign-in routing sees it.
#[derive(Clone)]
pub struct AuthInstance {
    pub instance_id: ProviderInstanceId,
    pub auth: Option<Arc<ProviderAuthController>>,
}

/// The slice of `ProviderInstanceRegistry` the auth service reads.
pub trait ProviderAuthInstances: Send + Sync {
    fn get_instance(&self, instance_id: &ProviderInstanceId) -> Option<AuthInstance>;
    fn list_instances(&self) -> Vec<AuthInstance>;
    /// Registers the subscription before returning, so a publish after this call cannot be missed.
    fn stream_changes(&self) -> BoxStream<'static, ()>;
}

impl ProviderAuthInstances for ProviderInstanceRegistry {
    fn get_instance(&self, instance_id: &ProviderInstanceId) -> Option<AuthInstance> {
        ProviderInstanceRegistry::get_instance(self, instance_id).map(|instance| AuthInstance {
            instance_id: instance.instance_id.clone(),
            auth: instance.auth.clone(),
        })
    }

    fn list_instances(&self) -> Vec<AuthInstance> {
        ProviderInstanceRegistry::list_instances(self)
            .into_iter()
            .map(|instance| AuthInstance {
                instance_id: instance.instance_id.clone(),
                auth: instance.auth.clone(),
            })
            .collect()
    }

    fn stream_changes(&self) -> BoxStream<'static, ()> {
        ProviderInstanceRegistry::stream_changes(self)
    }
}

/// The projection reads `stopSessions` makes.
#[async_trait]
pub trait ProviderAuthProjections: Send + Sync {
    /// `getRecoveryThreadIds("runtime")`.
    async fn runtime_thread_ids(&self) -> anyhow::Result<Vec<ThreadId>>;
    /// `getThreadRecords(threadId, ["providerSessions"])`, the `providerSessions` part.
    async fn provider_sessions(
        &self,
        thread_id: &ThreadId,
    ) -> anyhow::Result<Vec<OrchestrationV2ProviderSession>>;
}

#[async_trait]
impl ProviderAuthProjections for ProjectionStore {
    async fn runtime_thread_ids(&self) -> anyhow::Result<Vec<ThreadId>> {
        Ok(self
            .get_recovery_thread_ids(ProjectionRecoveryKind::Runtime)
            .await?)
    }

    async fn provider_sessions(
        &self,
        thread_id: &ThreadId,
    ) -> anyhow::Result<Vec<OrchestrationV2ProviderSession>> {
        Ok(self
            .get_thread_records(
                thread_id,
                vec![ProjectionRecordField::ProviderSessions],
                None,
            )
            .await?
            .provider_sessions)
    }
}

/// `ProviderSessionManagerV2.release`.
#[async_trait]
pub trait ProviderAuthSessions: Send + Sync {
    async fn release(
        &self,
        provider_session_id: &ProviderSessionId,
        reason: ProviderSessionReleaseReason,
        detail: &str,
    ) -> anyhow::Result<()>;
}

#[async_trait]
impl ProviderAuthSessions for ProviderSessionManager {
    async fn release(
        &self,
        provider_session_id: &ProviderSessionId,
        reason: ProviderSessionReleaseReason,
        detail: &str,
    ) -> anyhow::Result<()> {
        Ok(ProviderSessionManager::release(
            self,
            provider_session_id,
            reason,
            Some(detail.to_owned()),
        )
        .await?)
    }
}

fn setup_error(
    instance_id: &ProviderInstanceId,
    operation: &str,
    detail: impl Into<String>,
) -> ProviderSetupError {
    ProviderSetupError::SetupError {
        instance_id: instance_id.clone(),
        operation: operation.to_owned(),
        detail: detail.into(),
        cause: None,
    }
}

fn shares_binding(
    auth: Option<&Arc<ProviderAuthController>>,
    binding: &ProviderCredentialBinding,
) -> bool {
    auth.and_then(|auth| auth.credential_binding.as_ref()) == Some(binding)
}

struct Inner {
    registry: Arc<dyn ProviderAuthInstances>,
    projections: Arc<dyn ProviderAuthProjections>,
    provider_sessions: Arc<dyn ProviderAuthSessions>,
    /// `Semaphore.make(1)`: start, importProfile, logout and the prompt logout run one at a time.
    credential_changes: Mutex<()>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SharedOperation {
    Start,
    Logout,
}

impl SharedOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Logout => "logout",
        }
    }
}

impl Inner {
    fn get_controller(
        &self,
        instance_id: &ProviderInstanceId,
        operation: &str,
    ) -> Result<Arc<ProviderAuthController>, ProviderSetupError> {
        let instance = self.registry.get_instance(instance_id);
        match instance.as_ref().and_then(|instance| instance.auth.clone()) {
            Some(auth) => Ok(auth),
            None => Err(setup_error(
                instance_id,
                operation,
                if instance.is_some() {
                    "This provider does not support sign-in in T3 Code."
                } else {
                    "This provider instance is no longer available."
                },
            )),
        }
    }

    /// Native sessions may still belong to the previous provider after the selected model
    /// changes. Read session bindings, not the selected model, when invalidating credentials for
    /// sign-in or sign-out.
    async fn stop_sessions(
        &self,
        instance_id: &ProviderInstanceId,
        binding: Option<&ProviderCredentialBinding>,
    ) -> Result<(), ProviderSetupError> {
        let failure = |detail: &str| setup_error(instance_id, "stopSessions", detail);
        let mut affected_ids: HashSet<ProviderInstanceId> = HashSet::new();
        affected_ids.insert(instance_id.clone());
        if let Some(binding) = binding {
            for instance in self.registry.list_instances() {
                if shares_binding(instance.auth.as_ref(), binding) {
                    affected_ids.insert(instance.instance_id);
                }
            }
        }
        let thread_ids =
            self.projections.runtime_thread_ids().await.map_err(|_| {
                failure("Could not read the provider's active sessions. Try again.")
            })?;
        let stop_failure = || failure("Could not stop all sessions for this provider. Try again.");
        let mut released: HashSet<ProviderSessionId> = HashSet::new();
        for thread_id in &thread_ids {
            let sessions = self
                .projections
                .provider_sessions(thread_id)
                .await
                .map_err(|_| stop_failure())?;
            let to_release: Vec<OrchestrationV2ProviderSession> = sessions
                .into_iter()
                .filter(|session| {
                    affected_ids.contains(&session.provider_instance_id)
                        && session.status != OrchestrationV2ProviderSessionStatus::Stopped
                        && session.status != OrchestrationV2ProviderSessionStatus::Error
                        && !released.contains(&session.id)
                })
                .collect();
            for session in to_release {
                if &session.provider_instance_id != instance_id {
                    let current = self.registry.get_instance(&session.provider_instance_id);
                    let shares = binding.is_some_and(|binding| {
                        shares_binding(current.as_ref().and_then(|c| c.auth.as_ref()), binding)
                    });
                    if !shares {
                        continue;
                    }
                }
                self.provider_sessions
                    .release(
                        &session.id,
                        ProviderSessionReleaseReason::ManualShutdown,
                        "Provider sign-in changed.",
                    )
                    .await
                    .map_err(|_| stop_failure())?;
                released.insert(session.id);
            }
        }
        if let Some(binding) = binding {
            for instance in self
                .registry
                .list_instances()
                .into_iter()
                .filter(|instance| {
                    &instance.instance_id != instance_id
                        && affected_ids.contains(&instance.instance_id)
                        && shares_binding(instance.auth.as_ref(), binding)
                })
            {
                if let Some(invalidate) = instance
                    .auth
                    .as_ref()
                    .and_then(|auth| auth.invalidate.as_ref())
                {
                    invalidate().await;
                }
            }
        }
        Ok(())
    }

    async fn check_shared_binding(
        &self,
        instance_id: &ProviderInstanceId,
        operation: SharedOperation,
        auth: &ProviderAuthController,
    ) -> Result<(), ProviderSetupError> {
        let Some(binding) = &auth.credential_binding else {
            return Ok(());
        };
        for instance in self.registry.list_instances() {
            if &instance.instance_id == instance_id
                || !shares_binding(instance.auth.as_ref(), binding)
            {
                continue;
            }
            let Some(is_changing) = instance
                .auth
                .as_ref()
                .and_then(|auth| auth.is_changing_credentials.as_ref())
            else {
                continue;
            };
            if is_changing().await {
                return Err(setup_error(
                    instance_id,
                    operation.as_str(),
                    "Another provider instance is changing this shared sign-in. Finish or cancel it first.",
                ));
            }
        }
        Ok(())
    }
}

/// `stopSessions` as the lazily-run effect the controller awaits at the right moment.
fn stop_sessions_future(
    inner: &Arc<Inner>,
    instance_id: &ProviderInstanceId,
    binding: Option<ProviderCredentialBinding>,
) -> BoxFuture<'static, Result<(), ProviderSetupError>> {
    let inner = Arc::clone(inner);
    let instance_id = instance_id.clone();
    Box::pin(async move { inner.stop_sessions(&instance_id, binding.as_ref()).await })
}

fn auth_callback_mode(mode: WireMode) -> ProviderAuthCallbackMode {
    match mode {
        WireMode::Server => ProviderAuthCallbackMode::Server,
        WireMode::Client => ProviderAuthCallbackMode::Client,
    }
}

pub struct ProviderAuthService {
    inner: Arc<Inner>,
    /// Composition-root policy. A desktop owns its local callback listener;
    /// a browser client must not select Client mode without a redirect receiver.
    callback_mode: Option<ProviderAuthCallbackMode>,
}

impl ProviderAuthService {
    pub fn new(
        registry: Arc<dyn ProviderAuthInstances>,
        projections: Arc<dyn ProviderAuthProjections>,
        provider_sessions: Arc<dyn ProviderAuthSessions>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                registry,
                projections,
                provider_sessions,
                credential_changes: Mutex::new(()),
            }),
            callback_mode: None,
        }
    }

    /// Set the callback transport owned by this host. Native desktop roots use
    /// Server so the provider binds a loopback socket before publishing its
    /// browser interaction, even if an older UI adapter still asks for Client.
    /// Other callers keep the wire-specified mode by leaving this unset.
    pub fn with_callback_mode(mut self, mode: ProviderAuthCallbackMode) -> Self {
        self.callback_mode = Some(mode);
        self
    }

    pub async fn reconnect_profile(
        &self,
        instance_id: &ProviderInstanceId,
        method_id: &str,
    ) -> Result<Option<ChatGptReconnectProfile>, ProviderSetupError> {
        let auth = self.inner.get_controller(instance_id, "export")?;
        let Some(reconnect_profile) = &auth.reconnect_profile else {
            return Err(setup_error(
                instance_id,
                "export",
                "This provider does not support ChatGPT profile transfer.",
            ));
        };
        reconnect_profile(method_id.to_owned()).await
    }

    pub async fn import_profile(
        &self,
        instance_id: &ProviderInstanceId,
        profile: ChatGptTransferredProfile,
    ) -> Result<ProviderAuthState, ProviderSetupError> {
        let _permit = self.inner.credential_changes.lock().await;
        let auth = self.inner.get_controller(instance_id, "import")?;
        self.inner
            .check_shared_binding(instance_id, SharedOperation::Start, &auth)
            .await?;
        let Some(import_profile) = &auth.import_profile else {
            return Err(setup_error(
                instance_id,
                "import",
                "This provider does not support ChatGPT profile transfer.",
            ));
        };
        import_profile(
            profile,
            stop_sessions_future(&self.inner, instance_id, auth.credential_binding.clone()),
        )
        .await
    }

    pub async fn start(
        &self,
        input: &ProviderAuthStartInput,
        owner_session_id: &str,
    ) -> Result<ProviderAuthState, ProviderSetupError> {
        let _permit = self.inner.credential_changes.lock().await;
        let auth = self.inner.get_controller(&input.instance_id, "start")?;
        self.inner
            .check_shared_binding(&input.instance_id, SharedOperation::Start, &auth)
            .await?;
        (auth.start)(
            owner_session_id.to_owned(),
            Some(stop_sessions_future(
                &self.inner,
                &input.instance_id,
                auth.credential_binding.clone(),
            )),
            input.method_id.as_ref().map(|id| id.as_str().to_owned()),
            input.return_url.clone(),
            self.callback_mode
                .or_else(|| input.callback_mode.map(auth_callback_mode)),
        )
        .await
    }

    pub async fn respond(
        &self,
        input: ProviderAuthRespondInput,
        owner_session_id: &str,
    ) -> Result<ProviderAuthState, ProviderSetupError> {
        let auth = self.inner.get_controller(&input.instance_id, "respond")?;
        let Some(respond) = &auth.respond else {
            return Err(setup_error(
                &input.instance_id,
                "respond",
                "This provider does not accept this sign-in interaction.",
            ));
        };
        respond(owner_session_id.to_owned(), input).await
    }

    pub async fn complete(
        &self,
        instance_id: &ProviderInstanceId,
        flow_id: &str,
        callback_url: &str,
        owner_session_id: &str,
    ) -> Result<ProviderAuthState, ProviderSetupError> {
        let auth = self.inner.get_controller(instance_id, "complete")?;
        (auth.complete)(
            owner_session_id.to_owned(),
            ProviderAuthCompleteInput {
                flow_id: flow_id.to_owned(),
                callback_url: callback_url.to_owned(),
            },
        )
        .await
    }

    pub async fn cancel(
        &self,
        instance_id: &ProviderInstanceId,
        flow_id: &str,
        owner_session_id: &str,
    ) -> Result<ProviderAuthState, ProviderSetupError> {
        let auth = self.inner.get_controller(instance_id, "cancel")?;
        (auth.cancel)(owner_session_id.to_owned(), flow_id.to_owned()).await
    }

    pub async fn logout(
        &self,
        instance_id: &ProviderInstanceId,
    ) -> Result<ProviderAuthState, ProviderSetupError> {
        let _permit = self.inner.credential_changes.lock().await;
        let auth = self.inner.get_controller(instance_id, "logout")?;
        self.inner
            .check_shared_binding(instance_id, SharedOperation::Logout, &auth)
            .await?;
        (auth.logout)(stop_sessions_future(
            &self.inner,
            instance_id,
            auth.credential_binding.clone(),
        ))
        .await
    }

    /// The instance's auth state for one owner. Follows the registry: when a reconcile replaces
    /// the controller, the old subscription is dropped and the new controller's takes over. The
    /// stream ends with a setup error when the instance disappears or loses its controller.
    pub fn subscribe(
        &self,
        instance_id: &ProviderInstanceId,
        owner_session_id: &str,
    ) -> BoxStream<'static, Result<ProviderAuthState, ProviderSetupError>> {
        // Subscribed before the first lookup, so a replacement during it is not missed.
        let changes = self.inner.registry.stream_changes();
        let initial = self.inner.get_controller(instance_id, "subscribe");
        let inner = Arc::clone(&self.inner);
        let lookup_id = instance_id.clone();
        let controllers = futures::stream::once(async move { initial })
            .chain(changes.map(move |()| inner.get_controller(&lookup_id, "subscribe")))
            .boxed();
        Box::pin(SwitchMap {
            controllers: Some(controllers),
            previous: None,
            inner: None,
            owner_session_id: owner_session_id.to_owned(),
        })
    }

    /// Handles a standalone logout prompt (`/logout`) for providers whose controller recognizes
    /// it. Returns whether the prompt was consumed.
    pub async fn try_handle_prompt_command(
        &self,
        instance_id: &ProviderInstanceId,
        text: &str,
        has_attachments: bool,
    ) -> Result<bool, ProviderSetupError> {
        let recognized = self
            .inner
            .registry
            .get_instance(instance_id)
            .and_then(|instance| instance.auth)
            .and_then(|auth| auth.is_logout_prompt.clone())
            .is_some_and(|is_logout_prompt| is_logout_prompt(text, has_attachments));
        if !recognized {
            return Ok(false);
        }
        let _permit = self.inner.credential_changes.lock().await;
        // Resolved again under the lock: a reconcile may have replaced the controller while this
        // prompt queued behind another credential change.
        let auth = self.inner.get_controller(instance_id, "logout")?;
        if !auth
            .is_logout_prompt
            .as_ref()
            .is_some_and(|is_logout_prompt| is_logout_prompt(text, has_attachments))
        {
            return Ok(false);
        }
        self.inner
            .check_shared_binding(instance_id, SharedOperation::Logout, &auth)
            .await?;
        (auth.logout)(stop_sessions_future(
            &self.inner,
            instance_id,
            auth.credential_binding.clone(),
        ))
        .await?;
        Ok(true)
    }
}

/// `Stream.changesWith(previous === next)` then `Stream.switchMap(auth => auth.subscribe(owner))`.
/// A failed controller lookup fails the stream and drops the live inner subscription.
struct SwitchMap {
    controllers:
        Option<BoxStream<'static, Result<Arc<ProviderAuthController>, ProviderSetupError>>>,
    previous: Option<Arc<ProviderAuthController>>,
    inner: Option<Pin<Box<dyn Stream<Item = ProviderAuthState> + Send>>>,
    owner_session_id: String,
}

impl Stream for SwitchMap {
    type Item = Result<ProviderAuthState, ProviderSetupError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        // The outer stream is polled first so a replacement takes effect before the old
        // controller's next state is delivered.
        while let Some(controllers) = this.controllers.as_mut() {
            match controllers.poll_next_unpin(cx) {
                Poll::Ready(Some(Ok(auth))) => {
                    if this
                        .previous
                        .as_ref()
                        .is_some_and(|previous| Arc::ptr_eq(previous, &auth))
                    {
                        continue;
                    }
                    this.inner = Some(Box::into_pin((auth.subscribe)(
                        this.owner_session_id.clone(),
                    )));
                    this.previous = Some(auth);
                }
                Poll::Ready(Some(Err(error))) => {
                    this.controllers = None;
                    this.inner = None;
                    return Poll::Ready(Some(Err(error)));
                }
                Poll::Ready(None) => this.controllers = None,
                Poll::Pending => break,
            }
        }
        if let Some(inner) = this.inner.as_mut() {
            match inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(state)) => return Poll::Ready(Some(Ok(state))),
                Poll::Ready(None) => this.inner = None,
                Poll::Pending => return Poll::Pending,
            }
        }
        if this.controllers.is_none() && this.inner.is_none() {
            return Poll::Ready(None);
        }
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    //! Port of upstream/apps/server/src/provider/ProviderAuthService.test.ts. Upstream drives
    //! Antigravity-shaped controllers; the Rust tests use the same fake controller (the driver is
    //! irrelevant to the routing under test). The `Effect` layer mocks become the port fakes below.

    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use futures::StreamExt;
    use parking_lot::Mutex as SyncMutex;
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};
    use tokio::sync::{Notify, mpsc};

    use super::*;

    const OWNER: &str = "paired-client-owner";
    const OTHER_OWNER: &str = "paired-client-other";
    const FLOW_ID: &str = "test-sign-in-flow";
    const CALLBACK_URL: &str = "http://127.0.0.1:48123/?state=test-state&code=test-code";
    const NOW: &str = "2026-09-02T00:00:00.000Z";

    fn instance_id() -> ProviderInstanceId {
        ProviderInstanceId::new("antigravity-personal").unwrap()
    }

    fn other_instance_id() -> ProviderInstanceId {
        ProviderInstanceId::new("antigravity-work").unwrap()
    }

    fn unsupported_instance_id() -> ProviderInstanceId {
        ProviderInstanceId::new("codex").unwrap()
    }

    fn auth_state(phase: &str, flow_id: Option<&str>) -> ProviderAuthState {
        serde_json::from_value(json!({
            "instanceId": "antigravity-personal",
            "phase": phase,
            "flowId": flow_id,
            "authorizationUrl": flow_id.map(|_| "https://accounts.google.com/o/oauth2/v2/auth?state=test-state"),
            "expiresAt": flow_id.map(|_| "2026-09-02T00:05:00.000Z"),
            "message": null,
        }))
        .unwrap()
    }

    fn idle() -> ProviderAuthState {
        auth_state("idle", None)
    }

    fn waiting() -> ProviderAuthState {
        auth_state("waiting", Some(FLOW_ID))
    }

    fn capabilities() -> Value {
        json!({
            "sessions": {"supportsMultipleProviderThreadsPerSession": false, "supportsModelSwitchInSession": false,
                "supportsProviderSwitchingViaHandoff": false, "supportsRuntimeModeSwitchInSession": false,
                "pendingRequestsSurviveRestart": false},
            "threads": {"canCreateEmptyThread": false, "canReadThreadSnapshot": false, "canRollbackThread": false,
                "canForkThread": false, "canForkFromTurn": false, "canForkFromSubagentThread": false,
                "exposesNativeThreadId": false},
            "turns": {"exposesNativeTurnId": false, "emitsTurnStarted": false, "emitsTurnCompleted": false,
                "supportsInterrupt": false, "supportsActiveSteering": false,
                "supportsSteeringByInterruptRestart": false, "supportsQueuedMessages": false,
                "terminalStatusQuality": "weak"},
            "streaming": {"streamsAssistantText": false, "streamsReasoning": false, "streamsToolOutput": false,
                "streamsPlanText": false, "emitsMessageCompleted": false},
            "tools": {"exposesToolItemIds": false, "emitsToolStarted": false, "emitsToolCompleted": false,
                "emitsToolOutput": false, "supportsMcpTools": false, "supportsDynamicToolCallbacks": false},
            "approvals": {"supportsCommandApproval": false, "supportsFileReadApproval": false,
                "supportsFileChangeApproval": false, "supportsApplyPatchApproval": false,
                "approvalsHaveNativeRequestIds": false, "approvalCallbacksAreLiveOnly": false,
                "approvalsCanOriginateFromSubagents": false},
            "planning": {"emitsPlanUpdated": false, "emitsTodoList": false, "emitsProposedPlan": false,
                "supportsStructuredQuestions": false, "planDeltasHaveItemIds": false},
            "subagents": {"supportsSubagents": false, "exposesSubagentThreadIds": false,
                "emitsSubagentLifecycle": false, "canWaitForSubagents": false, "canCloseSubagents": false,
                "canForkSubagentThread": false},
            "context": {"acceptsSystemContext": false, "acceptsDeveloperContext": false,
                "acceptsSyntheticUserContext": false, "canGenerateSummaries": false,
                "canConsumeHandoffSummaries": false, "supportsDeltaHandoff": false,
                "supportsFullThreadHandoff": false, "maxRecommendedHandoffChars": null},
            "checkpointing": {"appCanCheckpointFilesystem": false, "supportsNestedCheckpointScopes": false,
                "providerCanRollbackConversation": false, "providerRollbackReturnsSnapshot": false,
                "providerCanReadConversationSnapshot": false},
            "identity": {"nativeThreadIds": "weak", "nativeTurnIds": "weak", "nativeItemIds": "weak",
                "nativeRequestIds": "weak"}
        })
    }

    fn session(
        id: &str,
        status: &str,
        provider_instance_id: &ProviderInstanceId,
    ) -> OrchestrationV2ProviderSession {
        serde_json::from_value(json!({
            "id": id,
            "driver": "antigravity",
            "providerInstanceId": provider_instance_id.as_str(),
            "status": status,
            "cwd": "/workspace",
            "model": null,
            "capabilities": capabilities(),
            "createdAt": NOW,
            "updatedAt": NOW,
            "lastError": null,
        }))
        .unwrap()
    }

    fn ready(id: &str) -> OrchestrationV2ProviderSession {
        session(id, "ready", &instance_id())
    }

    fn thread(id: &str) -> ThreadId {
        ThreadId::new(id).unwrap()
    }

    #[derive(Default)]
    struct Options {
        enabled_unused: bool,
        threads: Vec<&'static str>,
        sessions: Vec<(&'static str, Vec<OrchestrationV2ProviderSession>)>,
        shell_error: bool,
        stop_error: bool,
        logout_error: Option<ProviderSetupError>,
        shared_credentials: bool,
        shared_busy: bool,
        responds: bool,
        /// Called inside the native logout, after the sessions stopped.
        before_logout: Option<Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>>,
    }

    /// Everything the fake controllers, registry, projections and session manager share.
    struct Shared {
        actions: SyncMutex<Vec<String>>,
        released: SyncMutex<Vec<String>>,
        sessions: SyncMutex<Vec<(String, Vec<OrchestrationV2ProviderSession>)>>,
        threads: Vec<String>,
        gate_closed: AtomicBool,
        shell_error: bool,
        stop_error: bool,
        state: SyncMutex<ProviderAuthState>,
        flow_owner: SyncMutex<Option<String>>,
        instances: SyncMutex<Vec<AuthInstance>>,
        change_senders: SyncMutex<Vec<mpsc::UnboundedSender<()>>>,
        lookups: AtomicUsize,
    }

    impl Shared {
        fn push(&self, action: impl Into<String>) {
            self.actions.lock().push(action.into());
        }

        fn actions(&self) -> Vec<String> {
            self.actions.lock().clone()
        }

        fn publish_change(&self) {
            self.change_senders
                .lock()
                .retain(|sender| sender.send(()).is_ok());
        }
    }

    struct FakeRegistry(Arc<Shared>);

    impl ProviderAuthInstances for FakeRegistry {
        fn get_instance(&self, id: &ProviderInstanceId) -> Option<AuthInstance> {
            self.0.lookups.fetch_add(1, Ordering::SeqCst);
            self.0
                .instances
                .lock()
                .iter()
                .find(|instance| &instance.instance_id == id)
                .cloned()
        }

        fn list_instances(&self) -> Vec<AuthInstance> {
            self.0.instances.lock().clone()
        }

        fn stream_changes(&self) -> BoxStream<'static, ()> {
            let (sender, receiver) = mpsc::unbounded_channel();
            self.0.change_senders.lock().push(sender);
            tokio_stream::wrappers::UnboundedReceiverStream::new(receiver).boxed()
        }
    }

    #[async_trait]
    impl ProviderAuthProjections for FakeRegistry {
        async fn runtime_thread_ids(&self) -> anyhow::Result<Vec<ThreadId>> {
            assert!(self.0.gate_closed.load(Ordering::SeqCst));
            self.0.push("list-threads");
            if self.0.shell_error {
                anyhow::bail!("private database diagnostics");
            }
            Ok(self.0.threads.iter().map(|id| thread(id)).collect())
        }

        async fn provider_sessions(
            &self,
            thread_id: &ThreadId,
        ) -> anyhow::Result<Vec<OrchestrationV2ProviderSession>> {
            self.0.push(format!("sessions:{}", thread_id.as_str()));
            Ok(self
                .0
                .sessions
                .lock()
                .iter()
                .find(|(id, _)| id == thread_id.as_str())
                .map(|(_, list)| list.clone())
                .unwrap_or_default())
        }
    }

    #[async_trait]
    impl ProviderAuthSessions for FakeRegistry {
        async fn release(
            &self,
            provider_session_id: &ProviderSessionId,
            reason: ProviderSessionReleaseReason,
            detail: &str,
        ) -> anyhow::Result<()> {
            assert!(self.0.gate_closed.load(Ordering::SeqCst));
            assert_eq!(reason, ProviderSessionReleaseReason::ManualShutdown);
            assert_eq!(detail, "Provider sign-in changed.");
            self.0
                .push(format!("stop:{}", provider_session_id.as_str()));
            if self.0.stop_error {
                anyhow::bail!("private process diagnostics");
            }
            self.0
                .released
                .lock()
                .push(provider_session_id.as_str().to_owned());
            let mut sessions = self.0.sessions.lock();
            for (_, list) in sessions.iter_mut() {
                list.retain(|session| &session.id != provider_session_id);
            }
            sessions.retain(|(_, list)| !list.is_empty());
            Ok(())
        }
    }

    fn owner_check(
        shared: &Shared,
        owner: &str,
        flow_id: &str,
        operation: &str,
    ) -> Result<(), ProviderSetupError> {
        let state = shared.state.lock();
        let flow_owner = shared.flow_owner.lock();
        if flow_owner.as_deref() != Some(owner)
            || state.flow_id.as_ref().map(|id| id.as_str()) != Some(flow_id)
        {
            return Err(setup_error(
                &instance_id(),
                operation,
                "This sign-in belongs to another client or has expired.",
            ));
        }
        Ok(())
    }

    /// The upstream test controller: a stateful fake with close-gate / native-logout markers.
    fn make_controller(
        shared: &Arc<Shared>,
        options: &Options,
        shared_peer: bool,
    ) -> ProviderAuthController {
        let logout_error = options.logout_error.clone();
        let before_logout = options.before_logout.clone();
        let responds = options.responds;
        let s = Arc::clone(shared);
        let start: Arc<ProviderAuthStartFn> = Arc::new(move |owner, stop, _method, _url, _mode| {
            let s = Arc::clone(&s);
            Box::pin(async move {
                s.gate_closed.store(true, Ordering::SeqCst);
                s.push("close-gate");
                if let Some(stop) = stop {
                    stop.await?;
                }
                *s.flow_owner.lock() = Some(owner);
                *s.state.lock() = waiting();
                s.push("start-sign-in");
                Ok(s.state.lock().clone())
            })
        });
        let s = Arc::clone(shared);
        let complete: Arc<ProviderAuthCompleteFn> = Arc::new(move |owner, input| {
            let s = Arc::clone(&s);
            Box::pin(async move {
                owner_check(&s, &owner, &input.flow_id, "complete")?;
                if input.callback_url != CALLBACK_URL {
                    return Err(setup_error(
                        &instance_id(),
                        "complete",
                        "The redirect URL does not match this sign-in.",
                    ));
                }
                *s.state.lock() = auth_state("succeeded", Some(FLOW_ID)).without_url();
                Ok(s.state.lock().clone())
            })
        });
        let s = Arc::clone(shared);
        let cancel: Arc<ProviderAuthCancelFn> = Arc::new(move |owner, flow_id| {
            let s = Arc::clone(&s);
            Box::pin(async move {
                owner_check(&s, &owner, &flow_id, "cancel")?;
                *s.state.lock() = auth_state("cancelled", Some(FLOW_ID)).without_url();
                Ok(s.state.lock().clone())
            })
        });
        let s = Arc::clone(shared);
        let logout: Arc<ProviderAuthLogoutFn> = Arc::new(move |stop| {
            let s = Arc::clone(&s);
            let logout_error = logout_error.clone();
            let before_logout = before_logout.clone();
            Box::pin(async move {
                s.gate_closed.store(true, Ordering::SeqCst);
                s.push("close-gate");
                stop.await?;
                if let Some(error) = logout_error {
                    return Err(error);
                }
                if let Some(before_logout) = before_logout {
                    before_logout().await;
                }
                s.push("native-logout");
                *s.state.lock() = idle();
                *s.flow_owner.lock() = None;
                Ok(s.state.lock().clone())
            })
        });
        let s = Arc::clone(shared);
        let subscribe: Arc<ProviderAuthSubscribeFn> = Arc::new(move |owner| {
            let state = if s.flow_owner.lock().as_deref() == Some(owner.as_str()) {
                s.state.lock().clone()
            } else {
                idle()
            };
            Box::new(futures::stream::once(async move { state }))
        });
        let s = Arc::clone(shared);
        let respond: Option<Arc<ProviderAuthRespondFn>> = responds.then(|| {
            Arc::new(move |owner: String, request: ProviderAuthRespondInput| {
                let s = Arc::clone(&s);
                Box::pin(async move {
                    owner_check(&s, &owner, request.flow_id.as_str(), "respond")?;
                    let kind = match request.response {
                        turin_contracts::provider_setup::ProviderAuthResponse::Browser {
                            ..
                        } => "browser",
                        turin_contracts::provider_setup::ProviderAuthResponse::Terminal {
                            ..
                        } => "terminal",
                        turin_contracts::provider_setup::ProviderAuthResponse::Credentials {
                            ..
                        } => "credentials",
                    };
                    s.push(format!("respond:{kind}"));
                    Ok(s.state.lock().clone())
                }) as BoxFuture<'static, _>
            }) as Arc<ProviderAuthRespondFn>
        });
        let s = Arc::clone(shared);
        ProviderAuthController {
            credential_binding: options
                .shared_credentials
                .then(|| ProviderCredentialBinding {
                    owner: turin_contracts::provider_setup::ProviderCredentialOwner::Provider,
                    key: "shared".to_owned(),
                }),
            reconnect_profile: None,
            import_profile: None,
            adopt_credentials: None,
            is_changing_credentials: shared_peer.then(|| {
                let busy = options.shared_busy;
                Arc::new(move || Box::pin(async move { busy }) as BoxFuture<'static, bool>)
                    as Arc<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync>
            }),
            invalidate: shared_peer.then(|| {
                Arc::new(move || {
                    let s = Arc::clone(&s);
                    Box::pin(async move { s.push("invalidate-shared") }) as BoxFuture<'static, ()>
                }) as Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>
            }),
            refresh_methods: None,
            with_access: None,
            start,
            complete,
            cancel,
            respond,
            logout,
            subscribe,
            is_logout_prompt: Some(Arc::new(|text, has_attachments| {
                !has_attachments && text.trim() == "/logout"
            })),
        }
    }

    trait WithoutUrl {
        fn without_url(self) -> Self;
    }

    impl WithoutUrl for ProviderAuthState {
        fn without_url(mut self) -> Self {
            self.authorization_url = None;
            self.expires_at = None;
            self
        }
    }

    type ProviderAuthStartFn = turin_provider_core::auth::ProviderAuthStart;
    type ProviderAuthCompleteFn = turin_provider_core::auth::ProviderAuthComplete;
    type ProviderAuthCancelFn = turin_provider_core::auth::ProviderAuthCancel;
    type ProviderAuthLogoutFn = turin_provider_core::auth::ProviderAuthLogout;
    type ProviderAuthSubscribeFn = turin_provider_core::auth::ProviderAuthSubscribe;
    type ProviderAuthRespondFn = turin_provider_core::auth::ProviderAuthRespond;

    struct Harness {
        service: Arc<ProviderAuthService>,
        shared: Arc<Shared>,
        options: Options,
    }

    impl Harness {
        fn new(options: Options) -> Self {
            let _ = options.enabled_unused;
            let shared = Arc::new(Shared {
                actions: SyncMutex::new(Vec::new()),
                released: SyncMutex::new(Vec::new()),
                sessions: SyncMutex::new(
                    options
                        .sessions
                        .iter()
                        .map(|(id, list)| ((*id).to_owned(), list.clone()))
                        .collect(),
                ),
                threads: options.threads.iter().map(|id| (*id).to_owned()).collect(),
                gate_closed: AtomicBool::new(false),
                shell_error: options.shell_error,
                stop_error: options.stop_error,
                state: SyncMutex::new(idle()),
                flow_owner: SyncMutex::new(None),
                instances: SyncMutex::new(Vec::new()),
                change_senders: SyncMutex::new(Vec::new()),
                lookups: AtomicUsize::new(0),
            });
            let mut instances = vec![
                AuthInstance {
                    instance_id: instance_id(),
                    auth: Some(Arc::new(make_controller(&shared, &options, false))),
                },
                AuthInstance {
                    instance_id: unsupported_instance_id(),
                    auth: None,
                },
            ];
            if options.shared_credentials {
                instances.push(AuthInstance {
                    instance_id: other_instance_id(),
                    auth: Some(Arc::new(make_controller(&shared, &options, true))),
                });
            }
            *shared.instances.lock() = instances;
            let fake = Arc::new(FakeRegistry(Arc::clone(&shared)));
            let service = Arc::new(ProviderAuthService::new(
                fake.clone() as Arc<dyn ProviderAuthInstances>,
                fake.clone() as Arc<dyn ProviderAuthProjections>,
                fake as Arc<dyn ProviderAuthSessions>,
            ));
            Self {
                service,
                shared,
                options,
            }
        }

        fn start_input(&self, id: &ProviderInstanceId) -> ProviderAuthStartInput {
            ProviderAuthStartInput {
                instance_id: id.clone(),
                method_id: None,
                return_url: None,
                callback_mode: None,
            }
        }

        fn actions(&self) -> Vec<String> {
            self.shared.actions()
        }

        fn replace_auth(&self, controller: ProviderAuthController) {
            self.shared.instances.lock()[0] = AuthInstance {
                instance_id: instance_id(),
                auth: Some(Arc::new(controller)),
            };
        }
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn detail(error: &ProviderSetupError) -> &str {
        let ProviderSetupError::SetupError { detail, .. } = error;
        detail
    }

    fn operation(error: &ProviderSetupError) -> &str {
        let ProviderSetupError::SetupError { operation, .. } = error;
        operation
    }

    #[tokio::test]
    async fn stops_sessions_sharing_credentials_and_invalidates_their_processes_before_logout() {
        let harness = Harness::new(Options {
            shared_credentials: true,
            threads: vec!["target", "shared", "unrelated"],
            sessions: vec![
                ("target", vec![ready("target")]),
                (
                    "shared",
                    vec![session("shared", "ready", &other_instance_id())],
                ),
                (
                    "unrelated",
                    vec![session("unrelated", "ready", &unsupported_instance_id())],
                ),
            ],
            ..Default::default()
        });
        harness.service.logout(&instance_id()).await.unwrap();

        let remaining: Vec<String> = harness
            .shared
            .sessions
            .lock()
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        assert_eq!(remaining, strings(&["unrelated"]));
        let actions = harness.actions();
        let position = |name: &str| actions.iter().position(|action| action == name).unwrap();
        assert!(position("stop:shared") < position("invalidate-shared"));
        assert!(position("invalidate-shared") < position("native-logout"));
    }

    #[tokio::test]
    async fn rejects_overlapping_changes_to_a_shared_sign_in() {
        let harness = Harness::new(Options {
            shared_credentials: true,
            shared_busy: true,
            ..Default::default()
        });
        let start = harness
            .service
            .start(&harness.start_input(&instance_id()), OWNER)
            .await
            .unwrap_err();
        assert!(detail(&start).contains("shared sign-in"));
        let logout = harness.service.logout(&instance_id()).await.unwrap_err();
        assert!(detail(&logout).contains("shared sign-in"));
        assert_eq!(harness.actions(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn routes_typed_interactions_to_the_flow_owner_and_rejects_unsupported_controllers() {
        let harness = Harness::new(Options {
            responds: true,
            ..Default::default()
        });
        harness
            .service
            .start(&harness.start_input(&instance_id()), OWNER)
            .await
            .unwrap();
        let request = || -> ProviderAuthRespondInput {
            serde_json::from_value(json!({
                "instanceId": "antigravity-personal",
                "flowId": FLOW_ID,
                "interactionId": "consent",
                "response": {"type": "browser", "action": "accept"},
            }))
            .unwrap()
        };
        let rejected = harness
            .service
            .respond(request(), OTHER_OWNER)
            .await
            .unwrap_err();
        assert_eq!(operation(&rejected), "respond");
        harness.service.respond(request(), OWNER).await.unwrap();
        assert_eq!(harness.actions().last().unwrap(), "respond:browser");

        let unsupported = Harness::new(Options::default());
        let error = unsupported
            .service
            .respond(request(), OWNER)
            .await
            .unwrap_err();
        assert!(detail(&error).contains("does not accept"));
    }

    #[tokio::test]
    async fn stops_routed_sessions_before_sign_in() {
        let harness = Harness::new(Options {
            threads: vec!["active"],
            sessions: vec![("active", vec![ready("active-session")])],
            ..Default::default()
        });
        let state = harness
            .service
            .start(&harness.start_input(&instance_id()), OWNER)
            .await
            .unwrap();
        assert_eq!(state.instance_id, instance_id());
        assert_eq!(state, waiting());
        assert_eq!(
            *harness.shared.released.lock(),
            strings(&["active-session"])
        );
        assert_eq!(
            harness.actions(),
            strings(&[
                "close-gate",
                "list-threads",
                "sessions:active",
                "stop:active-session",
                "start-sign-in",
            ])
        );
    }

    #[tokio::test]
    async fn keeps_sign_in_state_private_and_accepts_the_owners_redirect_url() {
        let harness = Harness::new(Options::default());
        let waiting_state = harness
            .service
            .start(&harness.start_input(&instance_id()), OWNER)
            .await
            .unwrap();
        let owner_states: Vec<_> = harness
            .service
            .subscribe(&instance_id(), OWNER)
            .take(1)
            .collect()
            .await;
        let other_states: Vec<_> = harness
            .service
            .subscribe(&instance_id(), OTHER_OWNER)
            .take(1)
            .collect()
            .await;
        assert_eq!(owner_states, vec![Ok(waiting_state)]);
        let other = other_states[0].as_ref().unwrap();
        assert_eq!(other.authorization_url, None);
        assert_eq!(other.flow_id, None);

        let other_error = harness
            .service
            .complete(&instance_id(), FLOW_ID, CALLBACK_URL, OTHER_OWNER)
            .await
            .unwrap_err();
        assert_eq!(operation(&other_error), "complete");
        let complete = harness
            .service
            .complete(&instance_id(), FLOW_ID, CALLBACK_URL, OWNER)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(complete.phase).unwrap(),
            json!("succeeded")
        );
        assert_eq!(complete.authorization_url, None);
    }

    #[tokio::test]
    async fn lets_only_the_flow_owner_cancel_sign_in() {
        let harness = Harness::new(Options::default());
        harness
            .service
            .start(&harness.start_input(&instance_id()), OWNER)
            .await
            .unwrap();
        let error = harness
            .service
            .cancel(&instance_id(), FLOW_ID, OTHER_OWNER)
            .await
            .unwrap_err();
        assert_eq!(operation(&error), "cancel");
        let cancelled = harness
            .service
            .cancel(&instance_id(), FLOW_ID, OWNER)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(cancelled.phase).unwrap(),
            json!("cancelled")
        );
        assert_eq!(cancelled.authorization_url, None);
    }

    /// A controller whose subscription replays a state channel and ends when `close` is set,
    /// recording each owner whose subscription was dropped (`Stream.ensuring`).
    struct Streaming {
        auth: Arc<ProviderAuthController>,
        states: tokio::sync::watch::Sender<ProviderAuthState>,
        closed: Arc<SyncMutex<Vec<String>>>,
        close: Arc<Notify>,
    }

    struct DropRecord(String, Arc<SyncMutex<Vec<String>>>);

    impl Drop for DropRecord {
        fn drop(&mut self) {
            self.1.lock().push(self.0.clone());
        }
    }

    fn streaming_controller(flow_owner: &'static str) -> Streaming {
        let (states, _) = tokio::sync::watch::channel(idle());
        let closed = Arc::new(SyncMutex::new(Vec::new()));
        let close = Arc::new(Notify::new());
        let subscribe_states = states.clone();
        let subscribe_closed = Arc::clone(&closed);
        let subscribe_close = Arc::clone(&close);
        let subscribe: Arc<ProviderAuthSubscribeFn> = Arc::new(move |owner: String| {
            let guard = DropRecord(owner.clone(), Arc::clone(&subscribe_closed));
            let receiver = subscribe_states.subscribe();
            let close = Arc::clone(&subscribe_close);
            let stream = tokio_stream::wrappers::WatchStream::new(receiver)
                .map(move |current| {
                    let _keep = &guard;
                    if owner == flow_owner { current } else { idle() }
                })
                .take_until(async move { close.notified().await });
            Box::new(stream)
        });
        let unused_start: Arc<ProviderAuthStartFn> = Arc::new(|_, _, _, _, _| {
            unreachable!("Unexpected auth operation in a subscription test.")
        });
        let unused_complete: Arc<ProviderAuthCompleteFn> =
            Arc::new(|_, _| unreachable!("Unexpected auth operation in a subscription test."));
        let unused_cancel: Arc<ProviderAuthCancelFn> =
            Arc::new(|_, _| unreachable!("Unexpected auth operation in a subscription test."));
        let unused_logout: Arc<ProviderAuthLogoutFn> =
            Arc::new(|_| unreachable!("Unexpected auth operation in a subscription test."));
        Streaming {
            auth: Arc::new(ProviderAuthController {
                credential_binding: None,
                reconnect_profile: None,
                import_profile: None,
                adopt_credentials: None,
                is_changing_credentials: None,
                invalidate: None,
                refresh_methods: None,
                with_access: None,
                start: unused_start,
                complete: unused_complete,
                cancel: unused_cancel,
                respond: None,
                logout: unused_logout,
                subscribe,
                is_logout_prompt: None,
            }),
            states,
            closed,
            close,
        }
    }

    fn subscription_harness(initial: Option<Arc<ProviderAuthController>>) -> Harness {
        let harness = Harness::new(Options::default());
        *harness.shared.instances.lock() = initial
            .map(|auth| {
                vec![AuthInstance {
                    instance_id: instance_id(),
                    auth: Some(auth),
                }]
            })
            .unwrap_or_default();
        harness
    }

    async fn next(
        stream: &mut BoxStream<'static, Result<ProviderAuthState, ProviderSetupError>>,
    ) -> Result<ProviderAuthState, ProviderSetupError> {
        tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .expect("stream item within 5s")
            .expect("stream not ended")
    }

    #[tokio::test]
    async fn keeps_private_auth_subscriptions_current_after_an_instance_config_change() {
        let first = streaming_controller(OWNER);
        let replacement = streaming_controller(OTHER_OWNER);
        first.states.send_replace(waiting());
        let harness = subscription_harness(Some(Arc::clone(&first.auth)));
        let mut first_client = harness.service.subscribe(&instance_id(), OWNER);
        let mut second_client = harness.service.subscribe(&instance_id(), OTHER_OWNER);
        assert_eq!(next(&mut first_client).await.unwrap(), waiting());
        assert_eq!(next(&mut second_client).await.unwrap(), idle());

        harness.shared.instances.lock()[0].auth = Some(Arc::clone(&replacement.auth));
        harness.shared.publish_change();
        assert_eq!(next(&mut first_client).await.unwrap(), idle());
        assert_eq!(next(&mut second_client).await.unwrap(), idle());
        let mut closed = first.closed.lock().clone();
        closed.sort();
        assert_eq!(closed, vec![OTHER_OWNER.to_owned(), OWNER.to_owned()]);

        // The replaced controller's later states are not delivered.
        first.states.send_replace(auth_state("cancelled", None));
        let new_waiting = auth_state("waiting", Some("replacement-flow"));
        replacement.states.send_replace(new_waiting.clone());
        assert_eq!(next(&mut first_client).await.unwrap(), idle());
        assert_eq!(next(&mut second_client).await.unwrap(), new_waiting);
        drop((first.close, replacement.close));
    }

    #[tokio::test]
    async fn does_not_miss_a_replacement_during_the_first_instance_lookup() {
        let first = streaming_controller(OWNER);
        let replacement = streaming_controller(OWNER);
        replacement.states.send_replace(waiting());
        let harness = subscription_harness(Some(Arc::clone(&first.auth)));
        // The change is published between opening the change stream and the first lookup is not
        // observable here; the order (stream first, then lookup) is what matters, so replace
        // right after `subscribe` returns, before the stream is polled.
        let stream = harness.service.subscribe(&instance_id(), OWNER);
        harness.shared.instances.lock()[0].auth = Some(Arc::clone(&replacement.auth));
        harness.shared.publish_change();
        let mut waiting_states = Box::pin(stream.filter_map(|state| async move {
            state
                .ok()
                .filter(|state| serde_json::to_value(state.phase).unwrap() == json!("waiting"))
        }));
        let state = tokio::time::timeout(std::time::Duration::from_secs(5), waiting_states.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state, waiting());
    }

    #[tokio::test]
    async fn ends_the_subscription_with_a_setup_error_when_the_instance_is_removed() {
        let controller = streaming_controller(OWNER);
        let harness = subscription_harness(Some(Arc::clone(&controller.auth)));
        let mut stream = harness.service.subscribe(&instance_id(), OWNER);
        assert_eq!(next(&mut stream).await.unwrap(), idle());

        harness.shared.instances.lock().clear();
        harness.shared.publish_change();
        let error = next(&mut stream).await.unwrap_err();
        let ProviderSetupError::SetupError {
            instance_id: error_instance,
            operation,
            detail,
            ..
        } = error;
        assert_eq!(error_instance, instance_id());
        assert_eq!(operation, "subscribe");
        assert!(detail.contains("no longer available"));
        assert!(stream.next().await.is_none());
        assert_eq!(*controller.closed.lock(), vec![OWNER.to_owned()]);
    }

    #[tokio::test]
    async fn rejects_setup_for_unavailable_or_unsupported_instances() {
        for (id, expected) in [
            (
                ProviderInstanceId::new("missing").unwrap(),
                "no longer available",
            ),
            (unsupported_instance_id(), "does not support sign-in"),
        ] {
            let harness = Harness::new(Options::default());
            let service = &harness.service;
            let errors = vec![
                service
                    .start(&harness.start_input(&id), OWNER)
                    .await
                    .unwrap_err(),
                service
                    .complete(&id, FLOW_ID, CALLBACK_URL, OWNER)
                    .await
                    .unwrap_err(),
                service.cancel(&id, FLOW_ID, OWNER).await.unwrap_err(),
                service.logout(&id).await.unwrap_err(),
                service
                    .subscribe(&id, OWNER)
                    .collect::<Vec<_>>()
                    .await
                    .remove(0)
                    .unwrap_err(),
            ];
            for error in errors {
                let ProviderSetupError::SetupError {
                    instance_id: error_instance,
                    detail,
                    ..
                } = &error;
                assert_eq!(error_instance, &id);
                assert!(detail.contains(expected), "{detail}");
            }
            assert_eq!(harness.actions(), Vec::<String>::new());
        }
    }

    #[tokio::test]
    async fn stops_every_live_session_on_the_instance_before_native_logout() {
        let other = other_instance_id();
        let harness = Harness::new(Options {
            threads: vec!["running", "starting", "idle", "other-instance"],
            sessions: vec![
                (
                    "running",
                    vec![
                        session("running-session", "running", &instance_id()),
                        session("stopped-session", "stopped", &instance_id()),
                        session("errored-session", "error", &instance_id()),
                        session("foreign-session", "ready", &other),
                    ],
                ),
                (
                    "starting",
                    vec![session("starting-session", "starting", &instance_id())],
                ),
                ("idle", vec![ready("idle-session")]),
                (
                    "other-instance",
                    vec![session("other-session", "ready", &other)],
                ),
            ],
            ..Default::default()
        });
        let state = harness.service.logout(&instance_id()).await.unwrap();
        assert_eq!(state, idle());
        assert_eq!(
            harness.actions(),
            strings(&[
                "close-gate",
                "list-threads",
                "sessions:running",
                "stop:running-session",
                "sessions:starting",
                "stop:starting-session",
                "sessions:idle",
                "stop:idle-session",
                "sessions:other-instance",
                "native-logout",
            ])
        );
        assert_eq!(
            *harness.shared.released.lock(),
            strings(&["running-session", "starting-session", "idle-session"])
        );
    }

    #[tokio::test]
    async fn releases_a_shared_native_session_once_after_a_threads_selected_provider_changes() {
        let shared_session = ready("shared-native-session");
        let harness = Harness::new(Options {
            threads: vec!["changed-selection", "same-session"],
            sessions: vec![
                ("changed-selection", vec![shared_session.clone()]),
                ("same-session", vec![shared_session]),
            ],
            ..Default::default()
        });
        harness.service.logout(&instance_id()).await.unwrap();
        // The fake removes a released session from every thread, as the real store would after
        // the release; a second `release` would show up here.
        assert_eq!(
            *harness.shared.released.lock(),
            strings(&["shared-native-session"])
        );
    }

    #[tokio::test]
    async fn handles_only_a_standalone_logout_command() {
        for (text, has_attachments, handled) in [
            ("/logout", false, true),
            (" \n/logout\t", false, true),
            ("/logout", true, false),
            ("/logout please", false, false),
            ("Explain /logout", false, false),
            ("/Logout", false, false),
        ] {
            let harness = Harness::new(Options::default());
            let result = harness
                .service
                .try_handle_prompt_command(&instance_id(), text, has_attachments)
                .await
                .unwrap();
            assert_eq!(result, handled, "{text:?}");
            assert_eq!(
                harness.actions(),
                if handled {
                    strings(&["close-gate", "list-threads", "native-logout"])
                } else {
                    Vec::new()
                }
            );
        }
    }

    #[tokio::test]
    async fn does_not_intercept_commands_for_providers_without_an_auth_controller() {
        let harness = Harness::new(Options::default());
        for id in [
            unsupported_instance_id(),
            ProviderInstanceId::new("missing").unwrap(),
        ] {
            assert!(
                !harness
                    .service
                    .try_handle_prompt_command(&id, "/logout", false)
                    .await
                    .unwrap()
            );
        }
        assert_eq!(harness.actions(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn does_not_log_out_when_the_thread_shell_cannot_be_read() {
        let harness = Harness::new(Options {
            shell_error: true,
            ..Default::default()
        });
        let error = harness.service.logout(&instance_id()).await.unwrap_err();
        let ProviderSetupError::SetupError {
            instance_id: error_instance,
            operation,
            detail,
            ..
        } = &error;
        assert_eq!(error_instance, &instance_id());
        assert_eq!(operation, "stopSessions");
        assert!(!detail.contains("private database diagnostics"));
        assert_eq!(harness.actions(), strings(&["close-gate", "list-threads"]));
    }

    #[tokio::test]
    async fn does_not_log_out_or_consume_the_command_when_stopping_a_session_fails() {
        let harness = Harness::new(Options {
            threads: vec!["active"],
            sessions: vec![("active", vec![ready("active-session")])],
            stop_error: true,
            ..Default::default()
        });
        let error = harness
            .service
            .try_handle_prompt_command(&instance_id(), "/logout", false)
            .await
            .unwrap_err();
        assert_eq!(operation(&error), "stopSessions");
        assert!(!detail(&error).contains("private process diagnostics"));
        assert!(harness.shared.released.lock().is_empty());
        assert_eq!(
            harness.actions(),
            strings(&[
                "close-gate",
                "list-threads",
                "sessions:active",
                "stop:active-session",
            ])
        );
    }

    #[tokio::test]
    async fn returns_native_logout_failures_to_the_command_caller() {
        let logout_error = setup_error(
            &instance_id(),
            "logout",
            "Native sign-out failed. Try again.",
        );
        let harness = Harness::new(Options {
            logout_error: Some(logout_error.clone()),
            ..Default::default()
        });
        let error = harness
            .service
            .try_handle_prompt_command(&instance_id(), "/logout", false)
            .await
            .unwrap_err();
        assert_eq!(error, logout_error);
    }

    #[tokio::test]
    async fn queued_logout_prompts_resolve_the_current_controller_after_provider_replacement() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let (entered_hook, release_hook) = (Arc::clone(&entered), Arc::clone(&release));
        let harness = Arc::new(Harness::new(Options {
            before_logout: Some(Arc::new(move || {
                let (entered, release) = (Arc::clone(&entered_hook), Arc::clone(&release_hook));
                Box::pin(async move {
                    entered.notify_one();
                    release.notified().await;
                })
            })),
            ..Default::default()
        }));
        let first = {
            let harness = Arc::clone(&harness);
            tokio::spawn(async move { harness.service.logout(&instance_id()).await })
        };
        entered.notified().await;
        let lookups_before = harness.shared.lookups.load(Ordering::SeqCst);
        let queued = {
            let harness = Arc::clone(&harness);
            tokio::spawn(async move {
                harness
                    .service
                    .try_handle_prompt_command(&instance_id(), "/logout", false)
                    .await
            })
        };
        // The prompt has recognized `/logout` once it has looked the instance up; it then waits
        // for the credential lock held by the first logout.
        while harness.shared.lookups.load(Ordering::SeqCst) == lookups_before {
            tokio::task::yield_now().await;
        }
        let replacement_logged_out = Arc::new(AtomicBool::new(false));
        let mut replacement = make_controller(&harness.shared, &harness.options, false);
        let flag = Arc::clone(&replacement_logged_out);
        replacement.logout = Arc::new(move |stop| {
            let flag = Arc::clone(&flag);
            Box::pin(async move {
                stop.await?;
                flag.store(true, Ordering::SeqCst);
                Ok(idle())
            })
        });
        harness.replace_auth(replacement);
        release.notify_one();
        first.await.unwrap().unwrap();
        assert!(queued.await.unwrap().unwrap());
        assert!(replacement_logged_out.load(Ordering::SeqCst));
        assert_eq!(
            harness
                .actions()
                .iter()
                .filter(|action| *action == "native-logout")
                .count(),
            1
        );
    }
}
