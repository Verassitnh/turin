//! Port of upstream/apps/desktop/src/ipc/methods/notificationBadge.ts.

/// Clears the badge when the app is focused (Electron `browser-window-focus`).
pub fn clear_notification_badge_on_focus() {
    apply_notification_badge(0);
}

/// `applyNotificationBadge` (darwin: dock badge count; win32 overlay is cut).
pub fn apply_notification_badge(count: u32) {
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
    let label = if count > 0 {
        let text = if count > 9 {
            "9+".to_owned()
        } else {
            count.to_string()
        };
        Some(NSString::from_str(&text))
    } else {
        None
    };
    // SAFETY: main thread after NSApplication exists.
    unsafe {
        NSApplication::sharedApplication(mtm)
            .dockTile()
            .setBadgeLabel(label.as_deref());
    }
}
