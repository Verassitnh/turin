//! Port of upstream/apps/web/src/threadNotifications.ts (pure helpers; dock
//! badge and sound playback live in `desktop::notification_badge` and future
//! audio assets).

use turin_contracts::settings::NotificationMode;

/// `NOTIFICATION_MODE_LABELS`.
pub fn notification_mode_label(mode: NotificationMode) -> &'static str {
    match mode {
        NotificationMode::Off => "Off",
        NotificationMode::Notifications => "Notifications only",
        NotificationMode::Sound => "Sound only",
        NotificationMode::NotificationsAndSound => "Notifications with sound",
    }
}

/// `hasNotificationSound`.
pub fn has_notification_sound(mode: NotificationMode) -> bool {
    matches!(
        mode,
        NotificationMode::Sound | NotificationMode::NotificationsAndSound
    )
}

/// `hasDesktopNotifications`.
pub fn has_desktop_notifications(mode: NotificationMode) -> bool {
    matches!(
        mode,
        NotificationMode::Notifications | NotificationMode::NotificationsAndSound
    )
}

/// Forwards to the native dock badge (`setNotificationBadge`).
pub fn set_notification_badge(count: u32) {
    crate::desktop::apply_notification_badge(count);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_flags_match_upstream() {
        assert!(!has_notification_sound(NotificationMode::Off));
        assert!(has_notification_sound(NotificationMode::Sound));
        assert!(!has_desktop_notifications(NotificationMode::Sound));
        assert!(has_desktop_notifications(NotificationMode::Notifications));
    }
}
