//! Services the composition root passes into live settings. See
//! `docs/handoff/WIRE-settings.md` and `docs/handoff/WIRE-runtime.md`.
//!
//! ## WIRE-runtime Sol: populate from `WriteRuntime`
//!
//! Build [`LiveSettingsServices`] after `WriteRuntime::build` and `bus.install(commands)`:
//!
//! | Field | Source on `WriteRuntime` / composition root |
//! |-------|---------------------------------------------|
//! | `bus` | `CommandBus` with `Commands` installed |
//! | `server_settings` | `runtime.server_settings` (reads + `subscribe`; writes use [`super::live_commands`]) |
//! | `keybindings` | started `Keybindings` for `paths.keybindings_config_path` |
//! | `provider_auth` | `runtime.provider_auth` |
//! | `provider_installation` | `runtime.provider_installation` |
//! | `source_control` | `SourceControlDiscovery::new(runtime.source_control_host, runtime.source_control, cwd)` |
//! | `storage_cleanup` | started `StorageCleanup` |
//! | `provider_registry` | `runtime.providers.registry` |
//! | `provider_maintenance` | runner built with that registry |
//! | `environment`, `platform` | shell (`EnvironmentInfo`, `navigator.platform`) |
//!
//! Shell must call [`super::live_init::initialize_live_settings`] with desktop
//! client-settings persistence before opening settings.

use std::sync::Arc;

use turin_providers::provider_auth_service::ProviderAuthService;
use turin_providers::provider_installation::ProviderInstallation;
use turin_providers::provider_maintenance_runner::ProviderMaintenanceRunner;
use turin_providers::provider_registry::ProviderRegistry;
use turin_runtime::keybindings::Keybindings;
use turin_runtime::server_settings::ServerSettingsService;
use turin_runtime::storage_cleanup::StorageCleanup;
use turin_source_control::source_control::discovery::SourceControlDiscovery;

use crate::app_runtime::write::bus::CommandBus;

use super::view_model::EnvironmentInfo;

/// Everything live settings reads besides the client store snapshot.
#[derive(Clone)]
pub struct LiveSettingsServices {
    pub bus: CommandBus,
    pub server_settings: Arc<ServerSettingsService>,
    pub keybindings: Arc<Keybindings>,
    pub provider_auth: Arc<ProviderAuthService>,
    pub provider_installation: Arc<ProviderInstallation>,
    pub source_control: Arc<SourceControlDiscovery>,
    pub storage_cleanup: Arc<StorageCleanup>,
    pub provider_registry: Arc<ProviderRegistry>,
    pub provider_maintenance: Arc<ProviderMaintenanceRunner>,
    pub environment: EnvironmentInfo,
    /// `navigator.platform` for modifier labels.
    pub platform: String,
}

/// Stable owner id for every settings-window auth subscription and request.
pub const SETTINGS_AUTH_OWNER_SESSION: &str = "turin-settings-ui";
