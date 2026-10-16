//! Port of upstream/apps/web/src/routes/__root.tsx notification polling hooks
//! (per-environment shell polling until orchestrator feeds live attention).

use turin_contracts::settings::ClientSettings;

use crate::components::thread_notification_coordinator::ThreadNotificationCoordinator;
use crate::state::client_state::ClientState;
use crate::state::contracts::EnvironmentId;

/// Refreshes dock badge policy from settings and store snapshots.
pub fn poll_environment_notifications(
    coordinator: &mut ThreadNotificationCoordinator,
    environment_ids: &[EnvironmentId],
    snapshot: Option<&ClientState>,
    settings: &ClientSettings,
) {
    coordinator.sync_mode(settings);
    let _ = environment_ids;
    let pending = snapshot.map(pending_notification_count).unwrap_or(0);
    coordinator.set_pending_count(pending, settings.notification_mode);
}

/// Placeholder until thread attention selectors from upstream land on live shell data.
fn pending_notification_count(state: &ClientState) -> u32 {
    let _ = state;
    0
}
