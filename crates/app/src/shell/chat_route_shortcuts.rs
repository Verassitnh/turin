//! Port of upstream/apps/web/src/routes/_chat.tsx global shortcut table (subset
//! until terminal/preview focus contexts land in T22).

use gpui_kit::{App, KeyBinding, actions};

use super::command_palette_bus::{is_command_palette_open, open_command_palette};

const KEY_CONTEXT: &str = "AppShell";

actions!(
    chat_route,
    [
        /// `commandPalette.open` (mod+k).
        OpenCommandPalette,
        /// `view.reopenClosed` (mod+shift+t).
        ReopenClosedView,
    ]
);

pub fn bind_chat_route_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-k", OpenCommandPalette, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-t", ReopenClosedView, Some(KEY_CONTEXT)),
    ]);
}

pub fn open_command_palette_if_closed() {
    if !is_command_palette_open() {
        open_command_palette(None);
    }
}
