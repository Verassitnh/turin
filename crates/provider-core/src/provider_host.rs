//! Port of upstream/packages/provider-core/src/server/ProviderHost.ts.
//!
//! Drivers ask the process for directories, settings, background policy,
//! attachment paths, and credentials through this struct. They do not import
//! the app. The app fills the callbacks when it constructs the host.

use std::{any::Any, future::Future, sync::Arc};

use tokio::sync::mpsc;
use turin_contracts::{
    chat_attachment::ChatAttachment,
    settings::{ServerSettings, ServerSettingsError},
};

use crate::{
    background_scope::BackgroundScope, credential_store::ProviderCredentials, rt::BoxFuture,
};

#[derive(Debug, Clone)]
pub struct ProviderHostPaths {
    /// Directory the process was started in. Fallback cwd when a session has no project.
    pub cwd: String,
    /// Turin home.
    pub base_dir: String,
    /// Process-owned state directory under the home.
    pub state_dir: String,
    /// Scratch space for provider probes and generated helper files.
    pub provider_status_cache_dir: String,
    /// Where chat attachments are stored. Sandboxed providers must be granted it.
    pub attachments_dir: String,
}

/// Closure the host runs while it holds a settings snapshot.
pub type SettingsSnapshotJob =
    Box<dyn FnOnce(ServerSettings) -> BoxFuture<'static, Box<dyn Any + Send>> + Send>;

type SettingsSnapshot = Arc<
    dyn Fn(
            SettingsSnapshotJob,
        ) -> BoxFuture<'static, Result<Box<dyn Any + Send>, ServerSettingsError>>
        + Send
        + Sync,
>;
type GetSettings =
    Arc<dyn Fn() -> BoxFuture<'static, Result<ServerSettings, ServerSettingsError>> + Send + Sync>;
type SubscribeSettings = Arc<dyn Fn() -> mpsc::UnboundedReceiver<ServerSettings> + Send + Sync>;
type ShouldRunBackgroundWork =
    Arc<dyn Fn(BackgroundScope) -> BoxFuture<'static, bool> + Send + Sync>;
type ResolveAttachmentPath = Arc<dyn Fn(&ChatAttachment) -> Option<String> + Send + Sync>;
type OpenCredentials = Arc<dyn Fn(String, String) -> ProviderCredentials + Send + Sync>;

pub struct ProviderHost {
    pub paths: ProviderHostPaths,
    get_settings: GetSettings,
    with_settings_snapshot: SettingsSnapshot,
    subscribe_settings: SubscribeSettings,
    should_run_background_work: ShouldRunBackgroundWork,
    resolve_attachment_path: ResolveAttachmentPath,
    open_credentials: OpenCredentials,
}

impl ProviderHost {
    pub fn new(
        paths: ProviderHostPaths,
        get_settings: GetSettings,
        with_settings_snapshot: SettingsSnapshot,
        subscribe_settings: SubscribeSettings,
        should_run_background_work: ShouldRunBackgroundWork,
        resolve_attachment_path: ResolveAttachmentPath,
        open_credentials: OpenCredentials,
    ) -> Self {
        Self {
            paths,
            get_settings,
            with_settings_snapshot,
            subscribe_settings,
            should_run_background_work,
            resolve_attachment_path,
            open_credentials,
        }
    }

    #[allow(clippy::result_large_err)]
    pub async fn get_settings(&self) -> Result<ServerSettings, ServerSettingsError> {
        (self.get_settings)().await
    }

    /// Registers the subscription before returning.
    pub fn subscribe_settings(&self) -> mpsc::UnboundedReceiver<ServerSettings> {
        (self.subscribe_settings)()
    }

    /// Runs `use_settings` on a snapshot no settings write can change until it finishes.
    ///
    /// The host's own failure is [`ServerSettingsError`]. The closure's value
    /// is whatever the caller returns, including a `Result` of its own.
    #[allow(clippy::result_large_err)]
    pub async fn with_settings_snapshot<T, F, Fut>(
        &self,
        use_settings: F,
    ) -> Result<T, ServerSettingsError>
    where
        T: Send + 'static,
        F: FnOnce(ServerSettings) -> Fut + Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        let job: SettingsSnapshotJob = Box::new(move |settings| {
            Box::pin(async move {
                let value = use_settings(settings).await;
                Box::new(value) as Box<dyn Any + Send>
            })
        });
        let value = (self.with_settings_snapshot)(job).await?;
        value.downcast::<T>().map(|boxed| *boxed).map_err(|_| {
            ServerSettingsError::ServerSettingsError {
                settings_path: String::new(),
                operation: turin_contracts::settings::ServerSettingsOperation::ReadFile,
                provider_instance_id: None,
                environment_variable: None,
                cause: None,
            }
        })
    }

    pub async fn should_run_background_work(&self, scope: BackgroundScope) -> bool {
        (self.should_run_background_work)(scope).await
    }

    pub fn resolve_attachment_path(&self, attachment: &ChatAttachment) -> Option<String> {
        (self.resolve_attachment_path)(attachment)
    }

    /// Credentials for `binding_id` (usually an instance id) under a provider-chosen namespace.
    pub fn credentials(
        &self,
        namespace: impl Into<String>,
        binding_id: impl Into<String>,
    ) -> ProviderCredentials {
        (self.open_credentials)(namespace.into(), binding_id.into())
    }
}
