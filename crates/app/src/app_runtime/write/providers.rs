//! The provider side of the composition root (R1's "What T22 should call"):
//! host, built-in drivers, instance registry, model manifest, status registry.
//!
//! Orchestration routes by provider instance id through
//! `hydration.registry` (`ProviderAdapterLookup`). The status
//! [`ProviderRegistry`] is the list the settings and composer views read.
//! Codex and OpenCode slots stay empty: their drivers are not on this branch,
//! so configured instances of those kinds show as unavailable shadows.

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
use turin_providers::built_in_drivers::{BuiltInDriverSlots, built_in_drivers};
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
        let drivers = built_in_drivers(Arc::clone(&host), BuiltInDriverSlots::default());
        let hydration = ProviderInstanceRegistryHydration::new(drivers, &host)
            .await
            .context("hydrating the provider instance registry")?;
        let manifest = Arc::new(ModelManifest::new(
            &host.paths.state_dir,
            turin_provider_claude::claude_managed_provider::provider_update_checks(Arc::clone(
                &host,
            )),
        ));
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
