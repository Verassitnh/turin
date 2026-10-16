//! Port of upstream/apps/desktop/src/ipc/methods/notificationBadge.ts.

/// Dock label. `0` clears the badge. Counts stay decimal (`12`, not `9+`).
pub fn dock_badge_label(count: u32) -> Option<String> {
    if count == 0 {
        None
    } else {
        Some(count.to_string())
    }
}

/// Clears the badge when the app is focused (Electron `browser-window-focus`).
pub fn clear_notification_badge_on_focus() {
    apply_notification_badge_focused(0, true);
}

/// `applyNotificationBadge` (darwin: dock badge count; win32 overlay is cut).
pub fn apply_notification_badge(count: u32) {
    apply_notification_badge_focused(count, false);
}

pub fn displayed_badge_count(count: u32, focused: bool) -> u32 {
    if focused { 0 } else { count }
}

pub fn apply_notification_badge_focused(count: u32, focused: bool) {
    let count = displayed_badge_count(count, focused);
    #[cfg(target_os = "macos")]
    set_dock_badge_count(count);
    #[cfg(not(target_os = "macos"))]
    let _ = count;
}

#[cfg(target_os = "macos")]
fn set_dock_badge_count(count: u32) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let label = dock_badge_label(count).map(|text| NSString::from_str(&text));
    NSApplication::sharedApplication(mtm)
        .dockTile()
        .setBadgeLabel(label.as_deref());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_label_is_the_decimal_count() {
        assert_eq!(dock_badge_label(0), None);
        assert_eq!(dock_badge_label(10).as_deref(), Some("10"));
        assert_eq!(dock_badge_label(12).as_deref(), Some("12"));
        assert_eq!(displayed_badge_count(12, true), 0);
        assert_eq!(displayed_badge_count(12, false), 12);
    }
}
