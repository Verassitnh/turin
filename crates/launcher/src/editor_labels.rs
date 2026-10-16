//! Port of upstream/apps/web/src/editorLabels.ts.

use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::editor::{EDITORS, EditorId};

static MAC_PLATFORM_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)mac|iphone|ipad|ipod").expect("static pattern"));
static WINDOWS_PLATFORM_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^win(dows)?").expect("static pattern"));

/// `isMacPlatform` from `@t3tools/shared/keybindings`. Copied because that
/// module is not in this crate's scope.
pub fn is_mac_platform(platform: &str) -> bool {
    MAC_PLATFORM_PATTERN.is_match(platform)
}

/// `isWindowsPlatform` from `lib/utils.ts`. Copied for the same reason.
pub fn is_windows_platform(platform: &str) -> bool {
    WINDOWS_PLATFORM_PATTERN.is_match(platform)
}

/// `getLocalFileManagerName` from `lib/utils.ts`: what the OS calls its
/// file manager.
fn local_file_manager_name(platform: &str) -> &'static str {
    if is_mac_platform(platform) {
        "Finder"
    } else if is_windows_platform(platform) {
        "File Explorer"
    } else {
        "Files"
    }
}

/// Display name for an editor on a platform. The file manager is named after
/// the platform's file browser. An unknown id falls back to `Editor`.
pub fn editor_label_for_platform(editor_id: EditorId, platform: &str) -> String {
    if editor_id == EditorId::FileManager {
        return local_file_manager_name(platform).to_owned();
    }
    editor_label(editor_id).to_owned()
}

/// Label for the "Open in" menu entry. The default file handler and a missing
/// preference both use the generic label.
pub fn open_in_editor_menu_label(editor_id: Option<EditorId>) -> String {
    match editor_id {
        None | Some(EditorId::FileManager) => "Open in editor".to_owned(),
        Some(editor_id) => format!("Open in {}", editor_label(editor_id)),
    }
}

fn editor_label(editor_id: EditorId) -> &'static str {
    EDITORS
        .iter()
        .find(|editor| editor.id == editor_id)
        .map_or("Editor", |editor| editor.label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_the_editor_name_from_the_shared_editor_definitions() {
        assert_eq!(
            editor_label_for_platform(EditorId::Cursor, "MacIntel"),
            "Cursor"
        );
        assert_eq!(
            editor_label_for_platform(EditorId::VscodeInsiders, "Win32"),
            "VS Code Insiders"
        );
    }

    #[test]
    fn uses_the_platform_file_manager_name() {
        assert_eq!(
            editor_label_for_platform(EditorId::FileManager, "MacIntel"),
            "Finder"
        );
        assert_eq!(
            editor_label_for_platform(EditorId::FileManager, "Win32"),
            "File Explorer"
        );
        assert_eq!(
            editor_label_for_platform(EditorId::FileManager, "Linux x86_64"),
            "Files"
        );
    }

    #[test]
    fn names_the_preferred_editor_in_the_menu_label() {
        assert_eq!(
            open_in_editor_menu_label(Some(EditorId::Zed)),
            "Open in Zed"
        );
    }

    #[test]
    fn keeps_the_generic_label_for_the_default_file_handler_and_missing_preferences() {
        assert_eq!(
            open_in_editor_menu_label(Some(EditorId::FileManager)),
            "Open in editor"
        );
        assert_eq!(open_in_editor_menu_label(None), "Open in editor");
    }
}
