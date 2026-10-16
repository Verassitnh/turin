//! Port of upstream/apps/web/src/components/ThreadNotificationCoordinator.tsx
//! (in-app toasts and dock badge; desktop Notification API is native GPUI later).

use turin_contracts::settings::{ClientSettings, NotificationMode};

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

    pub fn set_pending_count(&mut self, count: u32, mode: NotificationMode) {
        self.pending_count = count;
        if has_desktop_notifications(mode) {
            set_notification_badge(count);
        }
    }

    pub fn clear(&mut self) {
        self.pending_count = 0;
        set_notification_badge(0);
    }

    pub fn pending_count(&self) -> u32 {
        self.pending_count
    }
}

impl Default for ThreadNotificationCoordinator {
    fn default() -> Self {
        Self::new()
    }
}
