//! Port of upstream/apps/web/src/components/ThreadNotificationCoordinator.tsx
//! (in-app toasts and dock badge; desktop Notification API is native GPUI later).

use turin_contracts::settings::{ClientSettings, NotificationMode};

use crate::desktop::notification_badge::displayed_badge_count;
use crate::shell::thread_notifications::{has_desktop_notifications, set_notification_badge};

/// Applies client settings to the dock badge policy.
pub struct ThreadNotificationCoordinator {
    pending_count: u32,
}

impl ThreadNotificationCoordinator {
    pub fn new() -> Self {
        Self { pending_count: 0 }
    }

    pub fn sync_mode(&mut self, settings: &ClientSettings) {
        if !has_desktop_notifications(settings.notification_mode) {
            self.clear();
        }
    }

    /// `applyNotificationBadge`: a focused window shows no badge.
    pub fn set_pending_count(&mut self, count: u32, mode: NotificationMode, focused: bool) {
        self.pending_count = count;
        if let Some(shown) = badge_to_apply(count, mode, focused) {
            set_notification_badge(shown);
        }
    }

    pub fn clear(&mut self) {
        self.pending_count = 0;
        set_notification_badge(0);
    }

    #[allow(dead_code)]
    pub fn pending_count(&self) -> u32 {
        self.pending_count
    }
}

/// The dock count to write for `count` pending threads, or `None` when the
/// notification mode leaves the badge alone. A focused window always shows 0.
fn badge_to_apply(count: u32, mode: NotificationMode, focused: bool) -> Option<u32> {
    has_desktop_notifications(mode).then(|| displayed_badge_count(count, focused))
}

impl Default for ThreadNotificationCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_focused_window_shows_no_badge_even_with_pending_threads() {
        let mode = NotificationMode::Notifications;
        assert_eq!(badge_to_apply(3, mode, true), Some(0));
        assert_eq!(badge_to_apply(3, mode, false), Some(3));
        assert_eq!(badge_to_apply(3, NotificationMode::Off, false), None);
    }
}
