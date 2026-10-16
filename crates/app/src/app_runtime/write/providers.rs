//! The provider side of the composition root (R1's "What T22 should call"):
//! host, built-in drivers, instance registry, model manifest, status registry.
//!
//! Orchestration routes by provider instance id through
//! `hydration.registry` (`ProviderAdapterLookup`). The status
//! [`ProviderRegistry`] is the list the settings and composer views read.
//! The Codex and OpenCode slots are filled here (T22c): Codex with
//! `CodexDriver::with_managed_host`, OpenCode with the app's own minimal
//! [`OpenCodeDriver`] (the provider crate has an adapter but no driver).

use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use turin_contracts::settings::ServerSettings;
use turin_provider_codex::CodexDriver;
use turin_provider_codex::codex_installation::{
    CodexInstallation, CodexInstallationOptions, CommandRuntimeValidator, HttpReleaseDownloader,
    ManifestCodexCompatibility, host_platform_and_arch,
};
use turin_provider_codex::codex_managed_runtime::CodexManagedHost;
use turin_provider_core::credential_store::{FileSecretStore, SecretStore};
use turin_provider_core::model_manifest::ModelManifest;
use turin_provider_core::provider_host::ProviderHost;
use turin_provider_core::provider_host_live::{
    DesktopProviderHost, DesktopSettingsHandle, ShouldRunBackgroundWork,
};
use turin_provider_core::provider_instance_registry::ProviderInstanceRegistry;
use turin_provider_core::provider_instance_registry_hydration::{
    DefaultInstanceDriver, ProviderInstanceRegistryHydration,
};
use turin_providers::built_in_drivers::{BuiltInDriverSlots, built_in_drivers};
use turin_providers::provider_installation::{
    InstallationError, ManagedInstallation, ProviderInstallation,
};

use super::opencode_driver::OpenCodeDriver;
use turin_providers::provider_registry::{ProviderRegistry, ProviderRegistryOptions};

/// Everything built from the provider host. Hold it for the app's life:
/// dropping `hydration` stops the settings watcher and dropping `registry`
/// aborts its tasks.
pub struct Providers {
    pub host: Arc<ProviderHost>,
    pub settings: DesktopSettingsHandle,
    pub hydration: ProviderInstanceRegistryHydration,
    pub registry: Arc<ProviderRegistry>,
    pub installation: Arc<ProviderInstallation>,
    pub maintenance: Arc<turin_providers::provider_maintenance_runner::ProviderMaintenanceRunner>,
    pub usage_readers: Vec<turin_usage::usage_service::UsageReader>,
    pub defaults: Vec<DefaultInstanceDriver>,
}

impl Providers {
    pub fn instances(&self) -> Arc<ProviderInstanceRegistry> {
        Arc::clone(&self.hydration.registry)
    }

    /// `base_dir` is Turin's data dir (the "T3 home" of R1's host: runtime
    /// state lives in `<base_dir>/userdata`).
    pub async fn build(base_dir: &Path, initial_settings: ServerSettings) -> anyhow::Result<Self> {
        // No leases yet: the app does not run background probes or refreshes
        // on its own. Provider status refreshes on demand.
        let background: ShouldRunBackgroundWork = Arc::new(|_scope| Box::pin(async { false }));
        let live = DesktopProviderHost::new(base_dir, base_dir, initial_settings, background)
            .context("opening the provider host (secret store)")?;
        let settings = live.settings_handle();
        let host = Arc::new(live.host);
        let manifest = Arc::new(ModelManifest::new(
            &host.paths.state_dir,
            turin_provider_claude::claude_managed_provider::provider_update_checks(Arc::clone(
                &host,
            )),
        ));
        let (codex, codex_installation) = codex_driver(base_dir, &host, &manifest).await?;
        let drivers = built_in_drivers(
            Arc::clone(&host),
            BuiltInDriverSlots {
                codex: Some(Box::new(move |_host| codex)),
                opencode: Some(Box::new(|_host| Arc::new(OpenCodeDriver::new()))),
            },
        );
        let defaults: Vec<_> = drivers
            .iter()
            .map(|driver| DefaultInstanceDriver {
                driver_kind: driver.driver_kind().clone(),
                has_default_instance: driver.metadata().has_default_instance,
            })
            .collect();
        let mut usage_readers = Vec::new();
        for kind in turin_providers::built_in_drivers::BUILT_IN_USAGE_DRIVER_ORDER {
            if let Some(driver) = drivers
                .iter()
                .find(|driver| driver.driver_kind().as_str() == kind)
            {
                if let Some(reader) = driver.usage() {
                    usage_readers.push(turin_usage::usage_service::UsageReader {
                        driver_kind: kind.to_owned(),
                        reader: reader.clone(),
                    });
                }
            }
        }
        let hydration = ProviderInstanceRegistryHydration::new(drivers, &host)
            .await
            .context("hydrating the provider instance registry")?;
        let registry = Arc::new(
            ProviderRegistry::start(
                hydration.registry.clone(),
                manifest.clone(),
                ProviderRegistryOptions {
                    provider_status_cache_dir: (&host.paths.provider_status_cache_dir).into(),
                    t3_code_version: env!("CARGO_PKG_VERSION").to_owned(),
                    clock: None,
                },
            )
            .await,
        );
        let installation = Arc::new(ProviderInstallation::new(
            Arc::new(CodexInstaller(codex_installation)),
            hydration.registry.clone(),
            registry.clone(),
            host.clone(),
            defaults
                .iter()
                .map(|driver| DefaultInstanceDriver {
                    driver_kind: driver.driver_kind.clone(),
                    has_default_instance: driver.has_default_instance,
                })
                .collect(),
        ));
        use turin_providers::provider_maintenance_runner::{
            LiveMaintenanceVersions, ProviderMaintenanceRunner, ProviderMaintenanceRunnerDeps,
            TokioCommandSpawner,
        };
        let maintenance = Arc::new(ProviderMaintenanceRunner::new(ProviderMaintenanceRunnerDeps {
            registry: registry.clone(), manifest,
            spawner: Arc::new(TokioCommandSpawner),
            versions: Arc::new(LiveMaintenanceVersions::new(Arc::new(turin_provider_core::provider_latest_versions::ProviderLatestVersions::default()), reqwest::Client::new())),
            t3_code_version: env!("CARGO_PKG_VERSION").to_owned(),
        }));
        Ok(Self {
            maintenance,
            defaults,
            installation,
            usage_readers,
            host,
            settings,
            hydration,
            registry,
        })
    }
}

/// Publishes the registry's status list to the UI bus, and asks the registry
/// for a first refresh: nothing probes in the background, so without it the
/// list would hold only what the disk cache remembered.
pub fn spawn_status_feed(providers: &Providers, bus: super::bus::CommandBus) {
    use futures::StreamExt as _;
    let registry = Arc::clone(&providers.registry);
    bus.publish_providers(registry.get_providers());
    let mut changes = registry.stream_changes();
    tokio::spawn(async move {
        while let Some(list) = changes.next().await {
            bus.publish_providers(list);
        }
    });
    let refresh = Arc::clone(&providers.registry);
    tokio::spawn(async move {
        refresh.refresh(None).await;
    });
}

/// The Codex driver with its managed host (T09d "App wiring"). Everything it
/// touches lives under `base_dir`: the secrets dir, the managed install dir and
/// the shadow `CODEX_HOME`s. Nothing is downloaded until a managed install is
/// requested. The desktop has no HTTP welcome page, so the return URL is
/// absent. Native sign-in can use the controller's own loopback callback server.
async fn codex_driver(
    base_dir: &Path,
    host: &Arc<ProviderHost>,
    manifest: &Arc<ModelManifest>,
) -> anyhow::Result<(Arc<CodexDriver>, Arc<CodexInstallation>)> {
    let state_dir = std::path::PathBuf::from(&host.paths.state_dir);
    let secrets_directory = state_dir.join("secrets");
    let secrets: Arc<dyn SecretStore> = Arc::new(
        FileSecretStore::open(&secrets_directory).context("opening the Codex secret store")?,
    );
    let (platform, arch) = host_platform_and_arch();
    let installation = CodexInstallation::open(CodexInstallationOptions {
        base_dir: base_dir.join("codex"),
        platform,
        arch,
        environment: std::env::vars().collect(),
        release_asset: None,
        compatibility: Arc::new(ManifestCodexCompatibility::new(
            Arc::clone(manifest) as _,
            env!("CARGO_PKG_VERSION"),
        )),
        downloader: Arc::new(HttpReleaseDownloader::new()),
        validator: Arc::new(CommandRuntimeValidator),
    })
    .await;
    let driver = Arc::new(CodexDriver::with_managed_host(CodexManagedHost {
        state_dir,
        user_home: dirs_home(),
        server_url: String::new(),
        secrets,
        secrets_directory: Some(secrets_directory),
        environment_id: crate::state::local_environment_id().to_string(),
        installation: installation.clone(),
        discovery_url: None,
        catalog_url: None,
    }));
    Ok((driver, installation))
}

fn dirs_home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// Shares the exact installer leased by Codex's managed runtime.
struct CodexInstaller(Arc<CodexInstallation>);

fn install_error(
    error: turin_provider_codex::codex_installation::CodexInstallationError,
) -> InstallationError {
    InstallationError {
        operation: error.operation.to_owned(),
        detail: error.detail,
    }
}

#[async_trait::async_trait]
impl ManagedInstallation for CodexInstaller {
    async fn start(
        &self,
    ) -> Result<turin_contracts::provider_setup::ProviderInstallState, InstallationError> {
        self.0.start().await.map_err(install_error)
    }
    async fn cancel(
        &self,
        operation_id: &str,
    ) -> Result<turin_contracts::provider_setup::ProviderInstallState, InstallationError> {
        self.0.cancel(operation_id).await.map_err(install_error)
    }
    async fn state(&self) -> turin_contracts::provider_setup::ProviderInstallState {
        self.0.state()
    }
    fn changes(
        &self,
    ) -> futures::stream::BoxStream<'static, turin_contracts::provider_setup::ProviderInstallState>
    {
        let receiver = self.0.subscribe();
        Box::pin(futures::stream::unfold(
            (receiver, true),
            |(mut receiver, first)| async move {
                if !first && receiver.changed().await.is_err() {
                    return None;
                }
                let state = receiver.borrow_and_update().clone();
                Some((state, (receiver, false)))
            },
        ))
    }
    async fn remove(&self, paths: Vec<String>) -> Result<(), InstallationError> {
        let paths: Vec<std::path::PathBuf> = paths.into_iter().map(Into::into).collect();
        self.0.remove(&paths).await.map_err(install_error)
    }
}
