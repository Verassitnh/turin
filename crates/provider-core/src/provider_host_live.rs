//! Port of upstream/apps/server/src/provider/ProviderHostLive.ts for a
//! desktop, single-process settings owner.
//!
//! Upstream builds the host from `ServerConfig`, `ServerSettingsService`,
//! `BackgroundPolicy` and the server secret store. Here the app owns those:
//! `DesktopProviderHost` holds the settings value and the background-work
//! answer, and wires the paths and credential store under the T3 home.

use std::{path::Path, sync::Arc};

use parking_lot::Mutex;
use tokio::sync::{RwLock, mpsc};
use turin_contracts::settings::{ServerSettings, ServerSettingsError};

use crate::{
    attachment_paths::resolve_attachment_path,
    background_scope::BackgroundScope,
    credential_store::{FileSecretStore, ProviderCredentials, SecretStore, SecretStoreError},
    provider_host::{ProviderHost, ProviderHostPaths, SettingsSnapshotJob},
    rt::BoxFuture,
};

/// Answers `ProviderHost.shouldRunBackgroundWork`. The app supplies it from
/// its own background policy; there is no default because "no leases" means
/// "do not run" upstream, and only the app knows its leases.
pub type ShouldRunBackgroundWork =
    Arc<dyn Fn(BackgroundScope) -> BoxFuture<'static, bool> + Send + Sync>;

struct DesktopSettings {
    current: RwLock<ServerSettings>,
    /// Upstream's `writeSemaphore`: a settings write waits for a running
    /// snapshot job. Reads never take it, so a job can read settings freely.
    write_gate: tokio::sync::Mutex<()>,
    subscribers: Mutex<Vec<mpsc::UnboundedSender<ServerSettings>>>,
}

pub struct DesktopProviderHost {
    pub host: ProviderHost,
    settings: Arc<DesktopSettings>,
}

/// The settings side of a [`DesktopProviderHost`], kept after `host` is moved
/// into an `Arc` for the drivers (`Arc::new(live.host)`). Cloning is cheap; every
/// clone writes to the same settings value the host reads.
#[derive(Clone)]
pub struct DesktopSettingsHandle {
    settings: Arc<DesktopSettings>,
}

impl DesktopSettingsHandle {
    /// The app's settings writer calls this after persisting a new settings
    /// value. Publishing never waits on a provider driver.
    pub async fn update_settings(&self, next: ServerSettings) {
        let _write = self.settings.write_gate.lock().await;
        *self.settings.current.write().await = next.clone();
        self.settings
            .subscribers
            .lock()
            .retain(|sender| sender.send(next.clone()).is_ok());
    }

    pub async fn settings(&self) -> ServerSettings {
        self.settings.current.read().await.clone()
    }
}

impl DesktopProviderHost {
    /// `base_dir` is the T3 home. Runtime state lives under `base_dir/userdata`
    /// (upstream's non-dev `stateDir`), with `secrets` and `attachments`
    /// beneath it, matching `secretsDir` and `attachmentsDir` in `config.ts`.
    pub fn new(
        base_dir: impl AsRef<Path>,
        cwd: impl AsRef<Path>,
        initial_settings: ServerSettings,
        should_run_background_work: ShouldRunBackgroundWork,
    ) -> Result<Self, SecretStoreError> {
        let base_dir = base_dir.as_ref();
        let state_dir = base_dir.join("userdata");
        let paths = ProviderHostPaths {
            cwd: cwd.as_ref().to_string_lossy().into_owned(),
            base_dir: base_dir.to_string_lossy().into_owned(),
            state_dir: state_dir.to_string_lossy().into_owned(),
            provider_status_cache_dir: base_dir.join("caches").to_string_lossy().into_owned(),
            attachments_dir: state_dir.join("attachments").to_string_lossy().into_owned(),
        };
        let secrets: Arc<dyn SecretStore> =
            Arc::new(FileSecretStore::open(state_dir.join("secrets"))?);
        let settings = Arc::new(DesktopSettings {
            current: RwLock::new(initial_settings),
            write_gate: tokio::sync::Mutex::new(()),
            subscribers: Mutex::new(Vec::new()),
        });
        let get_settings = {
            let settings = Arc::clone(&settings);
            Arc::new(move || -> BoxFuture<'static, _> {
                let settings = Arc::clone(&settings);
                Box::pin(async move { Ok(settings.current.read().await.clone()) })
            })
        };
        // Like upstream's `withSettingsSnapshot`, a settings write cannot land
        // while the job runs: the job holds the write gate, not the value's lock,
        // so it can still read settings (`get_settings`) without waiting behind a
        // queued write. As upstream, a job must not write settings itself.
        let with_snapshot = {
            let settings = Arc::clone(&settings);
            Arc::new(move |job: SettingsSnapshotJob| -> BoxFuture<'static, _> {
                let settings = Arc::clone(&settings);
                Box::pin(async move {
                    let _write = settings.write_gate.lock().await;
                    let snapshot = settings.current.read().await.clone();
                    Ok(job(snapshot).await)
                })
            })
        };
        let subscribe = {
            let settings = Arc::clone(&settings);
            Arc::new(move || {
                let (sender, receiver) = mpsc::unbounded_channel();
                settings.subscribers.lock().push(sender);
                receiver
            })
        };
        let attachments_dir = state_dir.join("attachments");
        let host = ProviderHost::new(
            paths,
            get_settings,
            with_snapshot,
            subscribe,
            should_run_background_work,
            Arc::new(move |attachment| resolve_attachment_path(&attachments_dir, attachment)),
            Arc::new(move |namespace: String, binding_id: String| {
                ProviderCredentials::open(Arc::clone(&secrets), &namespace, &binding_id)
            }),
        );
        Ok(Self { host, settings })
    }

    /// The app's settings writer calls this after persisting a new settings
    /// value. Publishing never waits on a provider driver.
    pub async fn update_settings(&self, next: ServerSettings) {
        self.settings_handle().update_settings(next).await;
    }

    /// Drivers take `Arc<ProviderHost>`, so the app moves `host` into an `Arc`
    /// and keeps this handle to publish settings changes afterwards.
    pub fn settings_handle(&self) -> DesktopSettingsHandle {
        DesktopSettingsHandle {
            settings: Arc::clone(&self.settings),
        }
    }

    #[allow(clippy::result_large_err)]
    pub async fn settings(&self) -> Result<ServerSettings, ServerSettingsError> {
        self.host.get_settings().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::settings::default_server_settings;

    fn host(answer: bool) -> DesktopProviderHost {
        let dir = tempfile::tempdir().unwrap();
        let should_run: ShouldRunBackgroundWork =
            Arc::new(move |_scope| -> BoxFuture<'static, bool> { Box::pin(async move { answer }) });
        DesktopProviderHost::new(
            dir.path(),
            dir.path(),
            default_server_settings(),
            should_run,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn settings_changes_reach_subscribers() {
        let live = host(true);
        let mut changes = live.host.subscribe_settings();
        let mut next = default_server_settings();
        next.enable_provider_update_checks = false;
        live.update_settings(next).await;
        assert!(!changes.recv().await.unwrap().enable_provider_update_checks);
        assert!(
            !live
                .host
                .get_settings()
                .await
                .unwrap()
                .enable_provider_update_checks
        );
    }

    #[tokio::test]
    async fn the_settings_handle_outlives_the_host_moved_into_an_arc() {
        let live = host(true);
        let handle = live.settings_handle();
        let shared = Arc::new(live.host);
        let mut changes = shared.subscribe_settings();
        let mut next = default_server_settings();
        next.enable_provider_update_checks = false;
        handle.update_settings(next).await;
        assert!(!changes.recv().await.unwrap().enable_provider_update_checks);
        assert!(
            !shared
                .get_settings()
                .await
                .unwrap()
                .enable_provider_update_checks
        );
        assert!(!handle.settings().await.enable_provider_update_checks);
    }

    #[tokio::test]
    async fn a_snapshot_job_can_read_settings_while_a_write_is_queued() {
        // The read lock must not be held across the job: a queued writer would
        // block the job's own read, and the writer waits on the job.
        let live = host(true);
        let handle = live.settings_handle();
        let shared = Arc::new(live.host);
        let in_job = Arc::new(tokio::sync::Notify::new());
        let write_queued = Arc::new(tokio::sync::Notify::new());

        let job = {
            let shared = Arc::clone(&shared);
            let in_job = Arc::clone(&in_job);
            let write_queued = Arc::clone(&write_queued);
            tokio::spawn(async move {
                let inner = Arc::clone(&shared);
                shared
                    .with_settings_snapshot(move |snapshot| async move {
                        in_job.notify_one();
                        write_queued.notified().await;
                        // Let the write reach its wait.
                        for _ in 0..20 {
                            tokio::task::yield_now().await;
                        }
                        let read_during_job = inner.get_settings().await.unwrap();
                        (
                            snapshot.enable_provider_update_checks,
                            read_during_job.enable_provider_update_checks,
                        )
                    })
                    .await
                    .unwrap()
            })
        };
        in_job.notified().await;
        let write = {
            let handle = handle.clone();
            tokio::spawn(async move {
                let mut next = default_server_settings();
                next.enable_provider_update_checks = false;
                handle.update_settings(next).await;
            })
        };
        write_queued.notify_one();

        let (snapshot_value, read_during_job) =
            tokio::time::timeout(std::time::Duration::from_secs(5), job)
                .await
                .expect("the job deadlocked behind the queued write")
                .unwrap();
        write.await.unwrap();
        // The write waited for the job: the job saw the old value throughout.
        assert!(snapshot_value);
        assert!(read_during_job);
        assert!(!handle.settings().await.enable_provider_update_checks);
    }

    #[tokio::test]
    async fn background_work_is_answered_by_the_app_policy() {
        assert!(
            host(true)
                .host
                .should_run_background_work(BackgroundScope::Diagnostics)
                .await
        );
        assert!(
            !host(false)
                .host
                .should_run_background_work(BackgroundScope::Diagnostics)
                .await
        );
    }

    #[test]
    fn paths_sit_under_the_t3_home_userdata() {
        let live = host(true);
        let paths = &live.host.paths;
        assert!(paths.state_dir.ends_with("userdata"));
        assert!(paths.attachments_dir.ends_with("userdata/attachments"));
        assert!(paths.provider_status_cache_dir.ends_with("caches"));
    }
}
