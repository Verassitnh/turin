//! Port of upstream/packages/provider-core/src/server/snapshotSettings.ts.
//!
//! A snapshot reads its instance's settings paired with the host's
//! `enableProviderUpdateChecks`, so a snapshot refreshes when either changes.

use futures::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use turin_contracts::settings::{ServerSettings, ServerSettingsError};

use crate::{provider_host::ProviderHost, rt::BoxStream};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSnapshotSettings<Settings> {
    pub provider: Settings,
    pub enable_provider_update_checks: bool,
}

fn make_provider_snapshot_settings<Settings>(
    provider: Settings,
    settings: &ServerSettings,
) -> ProviderSnapshotSettings<Settings> {
    ProviderSnapshotSettings {
        provider,
        enable_provider_update_checks: settings.enable_provider_update_checks,
    }
}

/// Upstream `haveProviderSnapshotSettingsChanged` (`Equal.equals`).
pub fn have_provider_snapshot_settings_changed<Settings: PartialEq>(
    previous: &ProviderSnapshotSettings<Settings>,
    next: &ProviderSnapshotSettings<Settings>,
) -> bool {
    previous != next
}

/// The instance's settings paired with the host's server settings, as snapshots read them.
pub struct ProviderSnapshotSettingsSource<'a, Settings> {
    provider: Settings,
    host: &'a ProviderHost,
}

impl<'a, Settings: Clone + Send + 'static> ProviderSnapshotSettingsSource<'a, Settings> {
    pub fn new(provider: Settings, host: &'a ProviderHost) -> Self {
        Self { provider, host }
    }

    #[allow(clippy::result_large_err)] // ServerSettingsError is built only on settings-read failure paths; boxing it would thread `Box::new` through every `?`.
    pub async fn get_settings(
        &self,
    ) -> Result<ProviderSnapshotSettings<Settings>, ServerSettingsError> {
        let settings = self.host.get_settings().await?;
        Ok(make_provider_snapshot_settings(
            self.provider.clone(),
            &settings,
        ))
    }

    pub fn stream_settings(&self) -> BoxStream<'static, ProviderSnapshotSettings<Settings>> {
        let provider = self.provider.clone();
        let changes = self.host.subscribe_settings();
        Box::pin(
            UnboundedReceiverStream::new(changes)
                .map(move |settings| make_provider_snapshot_settings(provider.clone(), &settings)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_a_change_in_either_the_instance_settings_or_the_update_check_flag() {
        let base = ProviderSnapshotSettings {
            provider: "codex".to_owned(),
            enable_provider_update_checks: true,
        };
        assert!(!have_provider_snapshot_settings_changed(
            &base,
            &base.clone()
        ));
        let other_provider = ProviderSnapshotSettings {
            provider: "codex_personal".to_owned(),
            ..base.clone()
        };
        assert!(have_provider_snapshot_settings_changed(
            &base,
            &other_provider
        ));
        let checks_off = ProviderSnapshotSettings {
            enable_provider_update_checks: false,
            ..base.clone()
        };
        assert!(have_provider_snapshot_settings_changed(&base, &checks_off));
    }
}
