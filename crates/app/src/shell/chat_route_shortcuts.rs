//! Port of upstream/apps/web/src/routes/_chat.tsx global shortcut table (subset
//! until terminal/preview focus contexts land in T22).

use gpui_kit::{App, KeyBinding, actions};

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
    cx.bind_keys(bindings());
}

fn bindings() -> Vec<KeyBinding> {
    vec![
        // mod+k is bound once, to `ToggleCommandPalette` in components/command_palette.rs. A second
        // binding here would shadow it (the later binding wins) and reach a bus nobody listens to.
        KeyBinding::new("cmd-shift-t", ReopenClosedView, Some(KEY_CONTEXT)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(bindings: &[KeyBinding]) -> Vec<String> {
        bindings
            .iter()
            .map(|binding| {
                binding
                    .keystrokes()
                    .iter()
                    .map(|keystroke| keystroke.inner().unparse())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    /// The later of two bindings for one key wins, so a duplicate silently disables the first.
    /// `cmd-k` once lived here and in the palette's table, and the palette's lost.
    #[test]
    fn route_shortcuts_do_not_shadow_the_command_palette_keys() {
        let palette = keys(&crate::components::command_palette::bindings(KEY_CONTEXT));
        assert!(palette.iter().any(|key| key == "cmd-k"), "{palette:?}");
        for key in keys(&bindings()) {
            assert!(!palette.contains(&key), "{key} is also a palette key");
        }
    }
}
