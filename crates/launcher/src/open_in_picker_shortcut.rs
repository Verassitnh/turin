//! Port of the decision in upstream/apps/web/src/components/chat/OpenInPickerShortcut.ts.
//!
//! Upstream installs a window `keydown` listener in a React effect. The
//! listener checks the favorite-editor keybinding and, when it matches with a
//! cwd and an editor, prevents the default action and opens the editor. Here
//! the key-matching and listener wiring stay in the UI. This is the decision
//! that follows a match.

use turin_contracts::editor::EditorId;

/// What a keydown should do for the favorite-editor shortcut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FavoriteEditorShortcut {
    /// Not handled. The key passes through untouched.
    Ignore,
    /// The UI prevents the default action, then opens `cwd` in `editor`.
    Open { cwd: String, editor: EditorId },
}

/// Decides the favorite-editor shortcut for one keydown.
///
/// `enabled` is whether the listener is installed. `matches_shortcut` is the
/// caller's keybinding check. `is_repeat` is the keydown's `repeat` flag: a
/// held key auto-repeats, and upstream's `event.repeat !== true` guard ignores
/// those so the editor opens once per press. An empty `open_in_cwd` counts as
/// none, as in upstream's truthiness test.
pub fn favorite_editor_shortcut(
    enabled: bool,
    matches_shortcut: bool,
    is_repeat: bool,
    open_in_cwd: Option<&str>,
    preferred_editor: Option<EditorId>,
) -> FavoriteEditorShortcut {
    if !enabled || is_repeat || !matches_shortcut {
        return FavoriteEditorShortcut::Ignore;
    }
    match (open_in_cwd.filter(|cwd| !cwd.is_empty()), preferred_editor) {
        (Some(cwd), Some(editor)) => FavoriteEditorShortcut::Open {
            cwd: cwd.to_owned(),
            editor,
        },
        _ => FavoriteEditorShortcut::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_the_preferred_editor_for_a_matching_shortcut() {
        assert_eq!(
            favorite_editor_shortcut(true, true, false, Some("/work"), Some(EditorId::Cursor)),
            FavoriteEditorShortcut::Open {
                cwd: "/work".to_owned(),
                editor: EditorId::Cursor,
            }
        );
    }

    #[test]
    fn ignores_keys_that_are_not_the_shortcut_or_when_disabled() {
        assert_eq!(
            favorite_editor_shortcut(true, false, false, Some("/work"), Some(EditorId::Cursor)),
            FavoriteEditorShortcut::Ignore
        );
        assert_eq!(
            favorite_editor_shortcut(false, true, false, Some("/work"), Some(EditorId::Cursor)),
            FavoriteEditorShortcut::Ignore
        );
    }

    #[test]
    fn ignores_auto_repeated_keydowns_of_the_shortcut() {
        // Holding the shortcut sends repeated keydowns with `repeat` set. Only
        // the first press opens the editor, as upstream's `event.repeat` guard.
        assert_eq!(
            favorite_editor_shortcut(true, true, true, Some("/work"), Some(EditorId::Cursor)),
            FavoriteEditorShortcut::Ignore
        );
    }

    #[test]
    fn ignores_the_shortcut_without_a_cwd_or_an_editor() {
        assert_eq!(
            favorite_editor_shortcut(true, true, false, None, Some(EditorId::Cursor)),
            FavoriteEditorShortcut::Ignore
        );
        assert_eq!(
            favorite_editor_shortcut(true, true, false, Some(""), Some(EditorId::Cursor)),
            FavoriteEditorShortcut::Ignore
        );
        assert_eq!(
            favorite_editor_shortcut(true, true, false, Some("/work"), None),
            FavoriteEditorShortcut::Ignore
        );
    }
}
