//! Persisted server settings go through [`Commands`] so provider reconciliation
//! runs (`docs/handoff/WIRE-runtime.md`). Read/subscribe still use
//! [`ServerSettingsService`] on [`LiveSettingsServices`].

use turin_contracts::provider_instance::ProviderInstanceMutation;
use turin_contracts::settings::{ServerSettings, ServerSettingsPatch};

use crate::app_runtime::write::bus::CommandBus;
use crate::app_runtime::write::commands::CommandError;

pub async fn server_settings(bus: &CommandBus) -> Result<ServerSettings, CommandError> {
    let commands = bus.commands()?;
    commands.server_settings().await
}

pub async fn update_server_settings(
    bus: &CommandBus,
    patch: ServerSettingsPatch,
) -> Result<ServerSettings, CommandError> {
    let commands = bus.commands()?;
    commands.update_server_settings(patch).await
}

pub async fn update_provider_instance(
    bus: &CommandBus,
    mutation: ProviderInstanceMutation,
) -> Result<ServerSettings, CommandError> {
    let commands = bus.commands()?;
    commands.update_provider_instance(mutation).await
}
