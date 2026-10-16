//! Port of upstream/apps/web/src/components/settings/providerDriverMeta.ts for
//! the kept drivers (codex, claudeAgent, cursor, opencode).

use turin_provider_core::server_provider::ServerProvider;

/// Presentation order and labels of `providerClients`.
pub const PROVIDER_CLIENTS: &[(&str, &str)] = &[
    ("codex", "Codex"),
    ("claudeAgent", "Claude"),
    ("cursor", "Cursor"),
    ("opencode", "OpenCode"),
];

pub fn driver_label(driver: &str) -> String {
    PROVIDER_CLIENTS
        .iter()
        .find(|(kind, _)| *kind == driver)
        .map(|(_, label)| (*label).to_owned())
        .unwrap_or_else(|| driver.to_owned())
}

/// `instance.displayName ?? driver label`.
pub fn instance_label(provider: &ServerProvider) -> String {
    provider
        .display_name
        .as_ref()
        .map(|name| name.as_str().to_owned())
        .unwrap_or_else(|| driver_label(provider.driver.as_str()))
}
