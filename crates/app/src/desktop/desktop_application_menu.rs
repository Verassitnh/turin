//! Port of upstream/apps/desktop/src/window/DesktopApplicationMenu.ts (menu
//! actions and accelerators; NSMenu wiring uses GPUI key bindings for now).

use gpui_kit::{App, KeyBinding, actions};

actions!(
    desktop_menu,
    [
        OpenSettings,
        PasteAsText,
        Reload,
        ForceReload,
        ToggleDevTools,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        CheckForUpdates,
    ]
);

/// Registers upstream menu accelerators on the app shell context.
pub fn bind_menu_keys(cx: &mut App, context: &'static str) {
    cx.bind_keys([
        KeyBinding::new("cmd-comma", OpenSettings, Some(context)),
        KeyBinding::new("cmd-shift-v", PasteAsText, Some(context)),
        KeyBinding::new("cmd-r", Reload, Some(context)),
        KeyBinding::new("cmd-shift-r", ForceReload, Some(context)),
        KeyBinding::new("cmd-alt-i", ToggleDevTools, Some(context)),
        KeyBinding::new("cmd-0", ZoomReset, Some(context)),
        KeyBinding::new("cmd-=", ZoomIn, Some(context)),
        KeyBinding::new("cmd-plus", ZoomIn, Some(context)),
        KeyBinding::new("cmd--", ZoomOut, Some(context)),
    ]);
}
