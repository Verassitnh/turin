//! Port of upstream/apps/web/src/routes/__root.tsx notification polling hooks
//! (per-environment shell polling).

use std::sync::Arc;

use turin_contracts::settings::ClientSettings;

use crate::components::thread_notification_coordinator::{
    ThreadNotice, ThreadNotificationCoordinator, notifications_enabled,
};
use crate::state::client_state::ClientState;

/// Scans the shell's threads for the ones that just finished or need the
/// user. Returns nothing while every notification channel is off, and forgets
/// what it saw, so turning one on later does not announce old events.
pub fn poll_environment_notifications(
    coordinator: &mut ThreadNotificationCoordinator,
    snapshot: Option<&ClientState>,
    settings: &ClientSettings,
) -> Vec<ThreadNotice> {
    coordinator.sync_mode(settings);
    if !notifications_enabled(
        settings.notification_mode,
        settings.in_app_notifications_enabled,
    ) {
        coordinator.reset_scan();
        return Vec::new();
    }
    let threads: Option<&[Arc<_>]> = snapshot
        .and_then(|state| state.shell_view.as_ref())
        .map(|view| view.threads.as_slice());
    coordinator.scan(threads)
}
