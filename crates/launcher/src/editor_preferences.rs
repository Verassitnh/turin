//! Port of the pure selection logic in upstream/apps/web/src/editorPreferences.ts.
//!
//! Upstream keeps the last-used editor in localStorage and wraps the choice
//! in React hooks and atom commands. Here the caller reads and writes the
//! stored value (`LAST_EDITOR_KEY`) and calls these functions. Cut parts:
//! the per-environment grant check (auth scopes are cut) and the
//! `PreferredEditorEnvironmentRequiredError` for a missing environment (Turin
//! has one local environment). See PORTING.md Deviations.

use thiserror::Error;
use turin_contracts::editor::{EDITORS, EditorId};

/// Storage key for the last-used editor (upstream `LAST_EDITOR_KEY`).
pub const LAST_EDITOR_KEY: &str = "t3code:last-editor";

/// The editor to use: the stored one when it is available, otherwise the
/// first available editor in `EDITORS` order.
pub fn preferred_editor(available: &[EditorId], stored: Option<EditorId>) -> Option<EditorId> {
    if let Some(stored) = stored.filter(|stored| available.contains(stored)) {
        return Some(stored);
    }
    EDITORS
        .iter()
        .find(|editor| available.contains(&editor.id))
        .map(|editor| editor.id)
}

/// Whether `resolved` differs from what is stored, so the caller must write
/// it back (upstream `setLocalStorageItem` in `resolveAndPersistPreferredEditor`).
pub fn preferred_editor_needs_save(stored: Option<EditorId>, resolved: Option<EditorId>) -> bool {
    resolved.is_some() && resolved != stored
}

/// No available editor can open the target.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("No available editor can open {target_path}.")]
pub struct PreferredEditorUnavailableError {
    pub target_path: String,
    pub available_editor_ids: Vec<EditorId>,
}

/// An editor chosen for an "open in" request, and whether to save it as the
/// new preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreferredEditorChoice {
    pub editor: EditorId,
    pub save: bool,
}

/// Choice for opening `target_path`, or the unavailable error when no editor
/// can open it.
pub fn resolve_open_in_preferred_editor(
    target_path: &str,
    available: &[EditorId],
    stored: Option<EditorId>,
) -> Result<PreferredEditorChoice, PreferredEditorUnavailableError> {
    let Some(editor) = preferred_editor(available, stored) else {
        return Err(PreferredEditorUnavailableError {
            target_path: target_path.to_owned(),
            available_editor_ids: available.to_vec(),
        });
    };
    Ok(PreferredEditorChoice {
        editor,
        save: preferred_editor_needs_save(stored, Some(editor)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_stored_available_editor_without_saving() {
        let choice = resolve_open_in_preferred_editor(
            "/work",
            &[EditorId::Vscode, EditorId::Cursor],
            Some(EditorId::Cursor),
        )
        .unwrap();

        assert_eq!(
            choice,
            PreferredEditorChoice {
                editor: EditorId::Cursor,
                save: false,
            }
        );
    }

    #[test]
    fn saves_the_first_available_editor_in_editor_order_when_nothing_is_stored() {
        let choice = resolve_open_in_preferred_editor(
            "/work/readme.md:5",
            &[EditorId::Zed, EditorId::Vscode],
            None,
        )
        .unwrap();

        assert_eq!(
            choice,
            PreferredEditorChoice {
                editor: EditorId::Vscode,
                save: true,
            }
        );
    }

    #[test]
    fn replaces_a_stored_editor_that_is_no_longer_available() {
        assert_eq!(
            preferred_editor(&[EditorId::Zed], Some(EditorId::Cursor)),
            Some(EditorId::Zed)
        );
        assert!(preferred_editor_needs_save(
            Some(EditorId::Cursor),
            Some(EditorId::Zed)
        ));
    }

    #[test]
    fn reports_the_target_when_no_editor_is_available() {
        let error = resolve_open_in_preferred_editor("/work", &[], None).unwrap_err();

        assert_eq!(
            error,
            PreferredEditorUnavailableError {
                target_path: "/work".to_owned(),
                available_editor_ids: Vec::new(),
            }
        );
        assert_eq!(error.to_string(), "No available editor can open /work.");
    }
}
