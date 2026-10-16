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
use turin_provider_core::model_manifest::ModelManifest;
use turin_provider_core::provider_host::ProviderHost;
use turin_provider_core::provider_host_live::{
    DesktopProviderHost, DesktopSettingsHandle, ShouldRunBackgroundWork,
};
use turin_provider_core::provider_instance_registry::ProviderInstanceRegistry;
use turin_provider_core::provider_instance_registry_hydration::ProviderInstanceRegistryHydration;
use turin_provider_codex::CodexDriver;
use turin_provider_codex::codex_installation::{
    CodexInstallation, CodexInstallationOptions, CommandRuntimeValidator, HttpReleaseDownloader,
    ManifestCodexCompatibility, host_platform_and_arch,
};
use turin_provider_codex::codex_managed_runtime::CodexManagedHost;
use turin_provider_core::credential_store::{FileSecretStore, SecretStore};
use turin_providers::built_in_drivers::{BuiltInDriverSlots, built_in_drivers};

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
        let codex = codex_driver(base_dir, &host, &manifest).await?;
        let drivers = built_in_drivers(
            Arc::clone(&host),
            BuiltInDriverSlots {
                codex: Some(Box::new(move |_host| codex)),
                opencode: Some(Box::new(|_host| Arc::new(OpenCodeDriver::new()))),
            },
        );
        let hydration = ProviderInstanceRegistryHydration::new(drivers, &host)
            .await
            .context("hydrating the provider instance registry")?;
        let registry = Arc::new(
            ProviderRegistry::start(
                hydration.registry.clone(),
                manifest,
                ProviderRegistryOptions {
                    provider_status_cache_dir: (&host.paths.provider_status_cache_dir).into(),
                    t3_code_version: env!("CARGO_PKG_VERSION").to_owned(),
                    clock: None,
                },
            )
            .await,
        );
        Ok(Self {
            host,
            settings,
            hydration,
            registry,
        })
    }
}

/// The Codex driver with its managed host (T09d "App wiring"). Everything it
/// touches lives under `base_dir`: the secrets dir, the managed install dir and
/// the shadow `CODEX_HOME`s. Nothing is downloaded until a managed install is
/// requested. Turin runs no HTTP server, so the sign-in return URL is a
/// placeholder until managed ChatGPT sign-in is wired to a callback.
async fn codex_driver(
    base_dir: &Path,
    host: &Arc<ProviderHost>,
    manifest: &Arc<ModelManifest>,
) -> anyhow::Result<Arc<CodexDriver>> {
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
    Ok(Arc::new(CodexDriver::with_managed_host(CodexManagedHost {
        state_dir,
        user_home: dirs_home(),
        server_url: "http://localhost:3773".to_owned(),
        secrets,
        secrets_directory: Some(secrets_directory),
        environment_id: crate::state::local_environment_id().to_string(),
        installation,
        discovery_url: None,
        catalog_url: None,
    })))
}

fn dirs_home() -> String {
    std::env::var("HOME").unwrap_or_default()
}
