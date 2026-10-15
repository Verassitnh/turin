//! Port of upstream/packages/provider-core/src/server/driver.ts.
//!
//! `ProviderDriver` is a trait because every kept provider implements it.
//! The registry type-erases config to `serde_json::Value`: `decode_config`
//! applies the driver's defaults, and `create` reads that value back into
//! its own struct. ACP session management on the instance is cut.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::watch;
use turin_contracts::{
    TrimmedNonEmptyString,
    provider_instance::{ProviderDriverKind, ProviderInstanceEnvironment, ProviderInstanceId},
    provider_usage_limits::ProviderConsumeResetCreditOutcome,
};

use crate::{
    auth::ProviderAuthController, errors::ProviderDriverError, provider_adapter::ProviderAdapterV2,
    rt::BoxFuture, server_provider::ProviderWorkspaceSnapshot, snapshot::ManagedServerProvider,
    text_generation::ProviderTextGeneration, usage::ProviderUsageReader,
};

#[derive(Debug, Clone)]
pub struct ProviderDriverMetadata {
    /// Human-readable name for the driver itself (for example "Codex").
    pub display_name: String,
    /// `false` for a driver that wraps one global resource. Defaults to true.
    pub supports_multiple_instances: bool,
    /// `false` when the driver needs user input before a default instance can
    /// run. Defaults to true. The ACP registry (cut) was the opt-out.
    pub has_default_instance: bool,
}

impl ProviderDriverMetadata {
    pub fn new(display_name: impl Into<String>) -> Self {
        Self {
            display_name: display_name.into(),
            supports_multiple_instances: true,
            has_default_instance: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderContinuationIdentity {
    pub driver_kind: ProviderDriverKind,
    pub continuation_key: String,
}

pub fn default_provider_continuation_identity(
    driver_kind: &ProviderDriverKind,
    instance_id: &ProviderInstanceId,
) -> ProviderContinuationIdentity {
    ProviderContinuationIdentity {
        driver_kind: driver_kind.clone(),
        continuation_key: format!("{driver_kind}:instance:{instance_id}"),
    }
}

/// Child lifetime of one materialized instance.
///
/// Upstream runs `create` inside an Effect `Scope`. Closing the scope (on
/// remove, replace, or registry shutdown) runs the driver's finalizers in
/// reverse order and is idempotent. `cancelled()` resolves once close begins
/// so in-flight work can stop.
pub struct ProviderInstanceScope {
    cancel: watch::Sender<bool>,
    finalizers: parking_lot::Mutex<Vec<BoxFuture<'static, ()>>>,
    closed: AtomicBool,
}

impl ProviderInstanceScope {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::open())
    }

    fn open() -> Self {
        // `watch::Sender::send` is a no-op with no receivers, and this
        // constructor keeps none. Closing uses `send_replace`, which stores
        // `true` regardless, so `is_cancelled()` and later `cancelled()`
        // calls still see the close.
        let (cancel, _) = watch::channel(false);
        Self {
            cancel,
            finalizers: parking_lot::Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        *self.cancel.borrow()
    }

    pub async fn cancelled(&self) {
        let mut receiver = self.cancel.subscribe();
        if *receiver.borrow() {
            return;
        }
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }

    pub fn on_close(&self, finalizer: impl std::future::Future<Output = ()> + Send + 'static) {
        let mut finalizers = self.finalizers.lock();
        if self.closed.load(Ordering::SeqCst) {
            drop(finalizers);
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(finalizer);
            }
            return;
        }
        finalizers.push(Box::pin(finalizer));
    }

    pub async fn close(self: &Arc<Self>) {
        let finalizers = {
            let mut finalizers = self.finalizers.lock();
            if self.closed.swap(true, Ordering::SeqCst) {
                return;
            }
            self.cancel.send_replace(true);
            std::mem::take(&mut *finalizers)
        };
        for finalizer in finalizers.into_iter().rev() {
            finalizer.await;
        }
    }
}

impl Drop for ProviderInstanceScope {
    fn drop(&mut self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.cancel.send_replace(true);
        let finalizers = std::mem::take(&mut *self.finalizers.lock());
        if finalizers.is_empty() {
            return;
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                for finalizer in finalizers.into_iter().rev() {
                    finalizer.await;
                }
            });
        }
    }
}

impl Default for ProviderInstanceScope {
    fn default() -> Self {
        Self::open()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn close_marks_cancelled_without_a_parked_waiter() {
        let scope = ProviderInstanceScope::new();
        assert!(!scope.is_cancelled());

        scope.close().await;

        assert!(scope.is_cancelled());
        tokio::time::timeout(Duration::from_secs(1), scope.cancelled())
            .await
            .expect("cancelled() resolves after close with no waiter parked");
    }

    #[tokio::test]
    async fn close_wakes_a_parked_waiter() {
        let scope = ProviderInstanceScope::new();
        let waiter = {
            let scope = Arc::clone(&scope);
            tokio::spawn(async move { scope.cancelled().await })
        };
        tokio::task::yield_now().await;

        scope.close().await;

        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("parked waiter wakes")
            .expect("waiter task completes");
    }

    #[tokio::test]
    async fn default_scope_cancels_on_close() {
        let scope = Arc::new(ProviderInstanceScope::default());
        scope.close().await;
        assert!(scope.is_cancelled());
    }
}

pub struct ProviderDriverCreateInput {
    pub instance_id: ProviderInstanceId,
    pub display_name: Option<TrimmedNonEmptyString>,
    pub accent_color: Option<TrimmedNonEmptyString>,
    pub environment: ProviderInstanceEnvironment,
    pub enabled: bool,
    /// Decoded config. Produced by [`ProviderDriver::decode_config`].
    pub config: Value,
    pub scope: Arc<ProviderInstanceScope>,
}

/// One materialized provider instance. Looked up by `instance_id`.
pub struct ProviderInstance {
    pub instance_id: ProviderInstanceId,
    pub driver_kind: ProviderDriverKind,
    pub continuation_identity: ProviderContinuationIdentity,
    pub display_name: Option<String>,
    pub accent_color: Option<String>,
    pub enabled: bool,
    pub snapshot: Arc<dyn ManagedServerProvider>,
    pub snapshot_for_cwd: Option<SnapshotForCwd>,
    pub refresh_models: Option<RefreshModels>,
    pub invalidate_caches: Option<InvalidateCaches>,
    pub consume_reset_credit: Option<ConsumeResetCredit>,
    pub orchestration_adapter: Arc<dyn ProviderAdapterV2>,
    pub text_generation: Arc<dyn ProviderTextGeneration>,
    pub auth: Option<Arc<ProviderAuthController>>,
}

pub type SnapshotForCwd = Arc<
    dyn Fn(String) -> BoxFuture<'static, Result<ProviderWorkspaceSnapshot, ProviderDriverError>>
        + Send
        + Sync,
>;

pub type RefreshModels =
    Arc<dyn Fn() -> BoxFuture<'static, Result<(), ProviderDriverError>> + Send + Sync>;

pub type InvalidateCaches = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

pub type ConsumeResetCredit = Arc<
    dyn Fn() -> BoxFuture<'static, Result<ProviderConsumeResetCreditOutcome, ProviderDriverError>>
        + Send
        + Sync,
>;

#[async_trait]
pub trait ProviderDriver: Send + Sync {
    fn driver_kind(&self) -> &ProviderDriverKind;
    fn metadata(&self) -> &ProviderDriverMetadata;

    /// Config used when an instance has no config blob.
    fn default_config(&self) -> Value;

    /// Decode the opaque envelope once. A failure becomes an unavailable
    /// shadow snapshot; the driver must not panic.
    fn decode_config(&self, raw: &Value) -> Result<Value, String>;

    async fn create(
        &self,
        input: ProviderDriverCreateInput,
    ) -> Result<ProviderInstance, ProviderDriverError>;

    fn usage(&self) -> Option<&ProviderUsageReader> {
        None
    }
}

/// Where the usage page reads this driver's token history, when it keeps one.
pub fn driver_error(
    driver: &ProviderDriverKind,
    instance_id: &ProviderInstanceId,
    detail: impl Into<String>,
) -> ProviderDriverError {
    ProviderDriverError {
        driver: driver.clone(),
        instance_id: instance_id.clone(),
        detail: detail.into(),
        cause: None,
    }
}

/// A static snapshot used by registry tests and by drivers that have not
/// probed yet. Not part of the upstream SPI.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::{
        maintenance_resolver::make_manual_only_provider_maintenance_capabilities,
        provider_adapter::{
            ProviderAdapterErrorKind, ProviderAdapterV2Error, ProviderAdapterV2OpenSessionInput,
        },
        selection_transition::{ProviderSelectionTransitionInput, ProviderSelectionTransitionPlan},
        server_provider::ServerProvider,
        text_generation::{
            BranchNameGenerationInput, BranchNameGenerationResult, CommitMessageGenerationInput,
            CommitMessageGenerationResult, PrContentGenerationInput, PrContentGenerationResult,
            ReviewGuideGenerationInput, ReviewGuideGenerationResult, ThreadTitleGenerationInput,
            ThreadTitleGenerationResult,
        },
    };
    use futures::stream;
    use turin_contracts::{
        git::TextGenerationError, orchestration_v2::OrchestrationV2ProviderCapabilities,
        provider_usage_limits::ProviderUsageLimitsUpdate,
    };

    pub struct StubSnapshot {
        pub provider: ServerProvider,
    }

    #[async_trait]
    impl ManagedServerProvider for StubSnapshot {
        async fn resolve_maintenance(
            &self,
            _fresh: bool,
        ) -> crate::maintenance::ProviderMaintenanceCapabilities {
            make_manual_only_provider_maintenance_capabilities(self.provider.driver.clone())
        }

        async fn get_snapshot(&self) -> ServerProvider {
            self.provider.clone()
        }

        async fn refresh(&self) -> ServerProvider {
            self.provider.clone()
        }

        fn stream_changes(&self) -> crate::rt::BoxStream<'static, ServerProvider> {
            Box::pin(stream::empty())
        }

        async fn apply_usage_limits(&self, _update: ProviderUsageLimitsUpdate, _checked_at: &str) {}
    }

    pub struct StubAdapter {
        pub instance_id: ProviderInstanceId,
        pub driver: ProviderDriverKind,
    }

    #[async_trait]
    impl ProviderAdapterV2 for StubAdapter {
        fn instance_id(&self) -> &ProviderInstanceId {
            &self.instance_id
        }

        fn driver(&self) -> &ProviderDriverKind {
            &self.driver
        }

        async fn get_capabilities(
            &self,
        ) -> Result<OrchestrationV2ProviderCapabilities, ProviderAdapterV2Error> {
            Err(ProviderAdapterV2Error::new(
                ProviderAdapterErrorKind::Capabilities {
                    driver: self.driver.clone(),
                },
            ))
        }

        async fn plan_selection_transition(
            &self,
            _input: ProviderSelectionTransitionInput,
        ) -> Result<ProviderSelectionTransitionPlan, ProviderAdapterV2Error> {
            Err(ProviderAdapterV2Error::new(
                ProviderAdapterErrorKind::Capabilities {
                    driver: self.driver.clone(),
                },
            ))
        }

        async fn open_session(
            &self,
            input: ProviderAdapterV2OpenSessionInput,
        ) -> Result<Arc<dyn crate::provider_adapter::ProviderSessionRuntime>, ProviderAdapterV2Error>
        {
            Err(ProviderAdapterV2Error::new(
                ProviderAdapterErrorKind::OpenSession {
                    driver: self.driver.clone(),
                    provider_session_id: input.provider_session_id,
                },
            ))
        }
    }

    pub struct StubText;

    fn text_error() -> TextGenerationError {
        TextGenerationError::TextGenerationError {
            operation: "stub".to_owned(),
            detail: "stub text generation".to_owned(),
            cause: None,
        }
    }

    #[async_trait]
    impl ProviderTextGeneration for StubText {
        async fn generate_commit_message(
            &self,
            _input: CommitMessageGenerationInput,
        ) -> Result<CommitMessageGenerationResult, TextGenerationError> {
            Err(text_error())
        }
        async fn generate_pr_content(
            &self,
            _input: PrContentGenerationInput,
        ) -> Result<PrContentGenerationResult, TextGenerationError> {
            Err(text_error())
        }
        async fn generate_branch_name(
            &self,
            _input: BranchNameGenerationInput,
        ) -> Result<BranchNameGenerationResult, TextGenerationError> {
            Err(text_error())
        }
        async fn generate_thread_title(
            &self,
            _input: ThreadTitleGenerationInput,
        ) -> Result<ThreadTitleGenerationResult, TextGenerationError> {
            Err(text_error())
        }
        async fn generate_review_guide(
            &self,
            _input: ReviewGuideGenerationInput,
        ) -> Result<ReviewGuideGenerationResult, TextGenerationError> {
            Err(text_error())
        }
    }

    pub fn stub_provider(
        instance_id: &ProviderInstanceId,
        driver: &ProviderDriverKind,
    ) -> ServerProvider {
        let mut value = serde_json::json!({
            "instanceId": instance_id.as_str(),
            "driver": driver.as_str(),
            "displayName": driver.as_str(),
            "enabled": true,
            "installed": false,
            "version": null,
            "status": "disabled",
            "auth": {"status": "unknown"},
            "checkedAt": "2026-01-01T00:00:00.000Z",
            "models": [],
            "slashCommands": [],
            "skills": [],
        });
        let _ = &mut value;
        serde_json::from_value(value).expect("stub provider snapshot")
    }
}
