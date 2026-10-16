//! Boot-time wiring for live settings (client prefs hydration).

use std::sync::Arc;

pub use crate::settings_logic::use_settings::{
    ClientSettingsHydrationError, ClientSettingsPersistence, ensure_client_settings_hydrated,
    set_client_settings_persistence,
};

/// Installs the desktop persistence backend. Call once during app startup,
/// before the shell or settings UI read client settings.
pub fn boot_client_settings(persistence: Arc<dyn ClientSettingsPersistence>) {
    set_client_settings_persistence(persistence);
}

/// Loads saved client settings into the in-memory store. Safe to call from a
/// tokio task after [`boot_client_settings`].
pub async fn hydrate_client_settings() -> Result<(), ClientSettingsHydrationError> {
    ensure_client_settings_hydrated().await
}

/// Boot persistence and hydrate (typical runtime `main` / composition-root task).
pub async fn initialize_live_settings(
    persistence: Arc<dyn ClientSettingsPersistence>,
) -> Result<(), ClientSettingsHydrationError> {
    boot_client_settings(persistence);
    hydrate_client_settings().await
}
