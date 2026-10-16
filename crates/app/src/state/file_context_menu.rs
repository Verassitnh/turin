//! Port of upstream/apps/web/src/fileContextMenu.ts (+ test), with the reveal
//! wording of `components/preview/fileExplorerLabel.ts` it reads.
//!
//! Right-click actions for a workspace file: reveal it in the file manager and
//! open it in an editor. Turin is one macOS process, so reveal always goes
//! through Finder and the environment checks collapse to the local host. The
//! menu model and the activation decision stay here; the GPUI popup and the
//! `launcher` call are the caller's.
// Seams for consumers the shell wires (T22); nothing calls these yet.
#![allow(dead_code)]

use turin_contracts::editor::{EDITORS, EditorId, FileManagerRevealKind};
use turin_contracts::environment::ExecutionEnvironmentPlatformOs;
use turin_shared::file_links::resolve_path_link_target;

use crate::diff::diff_file_actions::resolve_diff_path_for_workspace;

/// `FileContextMenuAction`. `OpenWith` is a submenu parent and is never the
/// activated id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileContextMenuAction {
    RevealInFolder,
    Open,
    OpenWith,
    Editor(EditorId),
}

/// `FileContextMenuTarget`. `environment_id` is null upstream when there is no
/// environment; the local host always has one, so the caller passes that fact
/// to [`plan_file_context_menu_activation`] instead of an id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileContextMenuTarget {
    /// Repo- or workspace-relative file path, as shown in diffs.
    pub file_path: String,
    pub workspace_root: Option<String>,
    pub repository_root: Option<String>,
}

/// What the server's config advertises for this host. Upstream derives it from
/// `serverConfig` inside the hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileContextMenuCapabilities {
    pub reveal_label: Option<String>,
    pub can_open_default: bool,
    pub editor_ids: Vec<EditorId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileContextMenuItem {
    pub action: FileContextMenuAction,
    pub label: String,
    /// Icon name as upstream spells it (`pencil`, `folder-tree`). `None` on
    /// the submenu parent and on editor children.
    pub icon: Option<&'static str>,
    pub children: Vec<FileContextMenuItem>,
}

/// Editor launch that [`plan_file_context_menu_activation`] decided on. The
/// caller runs `openInEditor` with these fields and, on failure, shows the
/// toast text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileContextMenuLaunch {
    pub cwd: String,
    pub editor: EditorId,
    pub reveal: bool,
    pub failure_title: String,
    pub failure_description: String,
}

/// Absolute path on the host for a diff-style target, resolving repo-relative
/// paths through the workspace root like every other diff surface. `None`
/// means "no file actions available".
pub fn resolve_file_context_menu_absolute_path(target: &FileContextMenuTarget) -> Option<String> {
    let workspace_file_path = resolve_diff_path_for_workspace(
        &target.file_path,
        target.workspace_root.as_deref(),
        target.repository_root.as_deref(),
    )?;
    match target.workspace_root.as_deref() {
        None => {
            let absolute = workspace_file_path.starts_with('/')
                || has_windows_drive_prefix(&workspace_file_path);
            absolute.then_some(workspace_file_path)
        }
        Some(root) => Some(resolve_path_link_target(&workspace_file_path, root)),
    }
}

/// `/^[a-zA-Z]:/`.
fn has_windows_drive_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// `EDITOR_LABEL_BY_ID.get(id) ?? id`.
fn editor_label(editor: EditorId) -> String {
    EDITORS
        .iter()
        .find(|definition| definition.id == editor)
        .map_or_else(
            || editor.to_string(),
            |definition| definition.label.to_owned(),
        )
}

/// `revealInFileExplorerLabelForOs`.
pub fn reveal_label_for_os(os: ExecutionEnvironmentPlatformOs) -> &'static str {
    match os {
        ExecutionEnvironmentPlatformOs::Darwin => "Reveal in Finder",
        ExecutionEnvironmentPlatformOs::Windows => "Reveal in File Explorer",
        _ => "Reveal in Files",
    }
}

/// `revealInFileExplorerLabelForKind`.
pub fn reveal_label_for_kind(kind: FileManagerRevealKind) -> &'static str {
    match kind {
        FileManagerRevealKind::Finder => "Reveal in Finder",
        FileManagerRevealKind::FileExplorer => "Reveal in File Explorer",
        FileManagerRevealKind::Files => "Reveal in Files",
    }
}

/// The `capabilities` memo of `useFileContextMenu`. Reveal is offered only when
/// the server reports `shellRevealInFileManager` and lists the file manager.
/// The server's kind wins over the OS wording (WSL reaches File Explorer from a
/// Linux host).
pub fn file_context_menu_capabilities(
    reveal_in_file_manager: bool,
    available_editors: &[EditorId],
    reveal_kind: Option<FileManagerRevealKind>,
    os: ExecutionEnvironmentPlatformOs,
) -> FileContextMenuCapabilities {
    let has_file_manager = available_editors.contains(&EditorId::FileManager);
    let reveal_label = (reveal_in_file_manager && has_file_manager).then(|| match reveal_kind {
        None => reveal_label_for_os(os).to_owned(),
        Some(kind) => reveal_label_for_kind(kind).to_owned(),
    });
    FileContextMenuCapabilities {
        reveal_label,
        can_open_default: has_file_manager,
        editor_ids: available_editors.to_vec(),
    }
}

/// Menu items for a resolved file, offering only what the environment's config
/// advertises: default-app open, reveal (with server-provided wording), and an
/// "Open with" submenu of detected editors. Empty when nothing can act.
pub fn build_file_context_menu_items(
    has_absolute_path: bool,
    capabilities: &FileContextMenuCapabilities,
) -> Vec<FileContextMenuItem> {
    // Without a resolvable absolute path nothing here can act on the file.
    if !has_absolute_path {
        return Vec::new();
    }
    let mut items = Vec::new();
    if capabilities.can_open_default {
        items.push(FileContextMenuItem {
            action: FileContextMenuAction::Open,
            label: "Open".to_owned(),
            icon: Some("pencil"),
            children: Vec::new(),
        });
    }
    if let Some(reveal_label) = &capabilities.reveal_label {
        items.push(FileContextMenuItem {
            action: FileContextMenuAction::RevealInFolder,
            label: reveal_label.clone(),
            icon: Some("folder-tree"),
            children: Vec::new(),
        });
    }
    let editor_ids: Vec<EditorId> = capabilities
        .editor_ids
        .iter()
        .copied()
        .filter(|id| *id != EditorId::FileManager)
        .collect();
    if !editor_ids.is_empty() {
        items.push(FileContextMenuItem {
            action: FileContextMenuAction::OpenWith,
            label: "Open with".to_owned(),
            icon: None,
            children: editor_ids
                .into_iter()
                .map(|editor| FileContextMenuItem {
                    action: FileContextMenuAction::Editor(editor),
                    label: editor_label(editor),
                    icon: None,
                    children: Vec::new(),
                })
                .collect(),
        });
    }
    items
}

/// The part of `activate` that decides the launch. `None` means do nothing:
/// no absolute path, no environment, or an editor the environment did not
/// advertise. `OpenWith` never reaches here as an activated id.
pub fn plan_file_context_menu_activation(
    action: FileContextMenuAction,
    target: &FileContextMenuTarget,
    environment_available: bool,
    capabilities: &FileContextMenuCapabilities,
) -> Option<FileContextMenuLaunch> {
    let absolute_path = resolve_file_context_menu_absolute_path(target)?;
    if !environment_available {
        return None;
    }

    let reveal = action == FileContextMenuAction::RevealInFolder;
    let editor = match action {
        FileContextMenuAction::Open | FileContextMenuAction::RevealInFolder => {
            EditorId::FileManager
        }
        FileContextMenuAction::Editor(editor) => editor,
        // The submenu parent is never activated.
        FileContextMenuAction::OpenWith => return None,
    };
    if action != FileContextMenuAction::Open
        && !reveal
        && !capabilities.editor_ids.contains(&editor)
    {
        return None;
    }

    let failure_title = match action {
        FileContextMenuAction::Open => "Could not open file".to_owned(),
        _ if reveal => "Unable to reveal file".to_owned(),
        _ => format!("Could not open in {}", editor_label(editor)),
    };
    Some(FileContextMenuLaunch {
        cwd: absolute_path.clone(),
        editor,
        reveal,
        failure_title,
        failure_description: absolute_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_target() -> FileContextMenuTarget {
        FileContextMenuTarget {
            file_path: "src/index.ts".to_owned(),
            workspace_root: Some("/workspace/project".to_owned()),
            repository_root: None,
        }
    }

    fn empty_capabilities() -> FileContextMenuCapabilities {
        FileContextMenuCapabilities {
            reveal_label: None,
            can_open_default: false,
            editor_ids: Vec::new(),
        }
    }

    fn ids(items: &[FileContextMenuItem]) -> Vec<FileContextMenuAction> {
        items.iter().map(|item| item.action).collect()
    }

    #[test]
    fn joins_workspace_relative_diff_paths_onto_the_workspace_root() {
        assert_eq!(
            resolve_file_context_menu_absolute_path(&base_target()).as_deref(),
            Some("/workspace/project/src/index.ts")
        );
    }

    #[test]
    fn strips_the_repository_prefix_when_the_repo_root_is_nested_in_the_workspace() {
        let target = FileContextMenuTarget {
            workspace_root: Some("/workspace/project/packages/app".to_owned()),
            repository_root: Some("/workspace/project".to_owned()),
            file_path: "packages/app/src/index.ts".to_owned(),
        };
        assert_eq!(
            resolve_file_context_menu_absolute_path(&target).as_deref(),
            Some("/workspace/project/packages/app/src/index.ts")
        );
    }

    #[test]
    fn returns_none_for_paths_outside_the_workspace_when_a_repository_root_is_set() {
        let target = FileContextMenuTarget {
            workspace_root: Some("/workspace/project/packages/app".to_owned()),
            repository_root: Some("/workspace/project".to_owned()),
            file_path: "other/src/index.ts".to_owned(),
        };
        assert_eq!(resolve_file_context_menu_absolute_path(&target), None);
    }

    #[test]
    fn rejects_absolute_paths_without_a_workspace_root_matching_diff_path_resolution() {
        let target = FileContextMenuTarget {
            workspace_root: None,
            file_path: "/absolute/src/index.ts".to_owned(),
            ..base_target()
        };
        assert_eq!(resolve_file_context_menu_absolute_path(&target), None);
    }

    #[test]
    fn offers_open_reveal_and_an_open_with_submenu_when_all_are_available() {
        let capabilities = FileContextMenuCapabilities {
            reveal_label: Some("Reveal in Finder".to_owned()),
            can_open_default: true,
            editor_ids: vec![EditorId::Vscode, EditorId::Cursor, EditorId::FileManager],
        };
        let items = build_file_context_menu_items(true, &capabilities);

        assert_eq!(
            ids(&items),
            vec![
                FileContextMenuAction::Open,
                FileContextMenuAction::RevealInFolder,
                FileContextMenuAction::OpenWith,
            ]
        );
        assert_eq!(items[0].label, "Open");
        assert_eq!(items[1].label, "Reveal in Finder");
        assert_eq!(
            ids(&items[2].children),
            vec![
                FileContextMenuAction::Editor(EditorId::Vscode),
                FileContextMenuAction::Editor(EditorId::Cursor),
            ]
        );
    }

    #[test]
    fn offers_only_the_reveal_item_when_just_reveal_is_enabled() {
        let capabilities = FileContextMenuCapabilities {
            reveal_label: Some("Reveal in File Explorer".to_owned()),
            can_open_default: false,
            editor_ids: Vec::new(),
        };
        let items = build_file_context_menu_items(true, &capabilities);

        assert_eq!(ids(&items), vec![FileContextMenuAction::RevealInFolder]);
        assert_eq!(items[0].label, "Reveal in File Explorer");
    }

    #[test]
    fn offers_nothing_when_the_path_cannot_be_resolved() {
        let capabilities = FileContextMenuCapabilities {
            reveal_label: Some("Reveal in Finder".to_owned()),
            can_open_default: true,
            editor_ids: vec![EditorId::Vscode],
        };
        assert!(build_file_context_menu_items(false, &capabilities).is_empty());
    }

    #[test]
    fn capabilities_take_the_server_reveal_kind_over_the_os_wording() {
        let capabilities = file_context_menu_capabilities(
            true,
            &[EditorId::FileManager],
            Some(FileManagerRevealKind::FileExplorer),
            ExecutionEnvironmentPlatformOs::Linux,
        );
        assert_eq!(
            capabilities.reveal_label.as_deref(),
            Some("Reveal in File Explorer")
        );
        assert!(capabilities.can_open_default);
    }

    #[test]
    fn capabilities_hide_reveal_without_the_file_manager_editor() {
        let capabilities = file_context_menu_capabilities(
            true,
            &[EditorId::Vscode],
            None,
            ExecutionEnvironmentPlatformOs::Darwin,
        );
        assert_eq!(capabilities.reveal_label, None);
        assert!(!capabilities.can_open_default);
    }

    #[test]
    fn reveal_wording_falls_back_to_the_os_when_the_server_sends_no_kind() {
        let capabilities = file_context_menu_capabilities(
            true,
            &[EditorId::FileManager],
            None,
            ExecutionEnvironmentPlatformOs::Darwin,
        );
        assert_eq!(
            capabilities.reveal_label.as_deref(),
            Some("Reveal in Finder")
        );
    }

    #[test]
    fn plans_an_open_through_the_file_manager_with_the_resolved_path() {
        let capabilities = FileContextMenuCapabilities {
            reveal_label: None,
            can_open_default: true,
            editor_ids: vec![EditorId::FileManager],
        };
        let launch = plan_file_context_menu_activation(
            FileContextMenuAction::Open,
            &base_target(),
            true,
            &capabilities,
        )
        .expect("open is planned");
        assert_eq!(launch.cwd, "/workspace/project/src/index.ts");
        assert_eq!(launch.editor, EditorId::FileManager);
        assert!(!launch.reveal);
        assert_eq!(launch.failure_title, "Could not open file");
    }

    #[test]
    fn plans_a_reveal_as_a_file_manager_launch_with_the_reveal_flag() {
        let capabilities = FileContextMenuCapabilities {
            reveal_label: Some("Reveal in Finder".to_owned()),
            can_open_default: false,
            editor_ids: vec![EditorId::FileManager],
        };
        let launch = plan_file_context_menu_activation(
            FileContextMenuAction::RevealInFolder,
            &base_target(),
            true,
            &capabilities,
        )
        .expect("reveal is planned");
        assert_eq!(launch.editor, EditorId::FileManager);
        assert!(launch.reveal);
        assert_eq!(launch.failure_title, "Unable to reveal file");
        assert_eq!(launch.failure_description, launch.cwd);
    }

    #[test]
    fn plans_an_editor_launch_only_for_an_advertised_editor() {
        let capabilities = FileContextMenuCapabilities {
            reveal_label: None,
            can_open_default: false,
            editor_ids: vec![EditorId::Vscode],
        };
        let launch = plan_file_context_menu_activation(
            FileContextMenuAction::Editor(EditorId::Vscode),
            &base_target(),
            true,
            &capabilities,
        )
        .expect("advertised editor is planned");
        assert_eq!(launch.editor, EditorId::Vscode);
        assert!(!launch.reveal);
        assert_eq!(launch.failure_title, "Could not open in VS Code");

        assert_eq!(
            plan_file_context_menu_activation(
                FileContextMenuAction::Editor(EditorId::Cursor),
                &base_target(),
                true,
                &capabilities,
            ),
            None
        );
    }

    #[test]
    fn plans_nothing_without_an_environment_or_an_absolute_path() {
        let capabilities = FileContextMenuCapabilities {
            reveal_label: Some("Reveal in Finder".to_owned()),
            can_open_default: true,
            editor_ids: vec![EditorId::Vscode],
        };
        assert_eq!(
            plan_file_context_menu_activation(
                FileContextMenuAction::Open,
                &base_target(),
                false,
                &capabilities,
            ),
            None
        );
        let unresolvable = FileContextMenuTarget {
            workspace_root: None,
            ..base_target()
        };
        assert_eq!(
            plan_file_context_menu_activation(
                FileContextMenuAction::Open,
                &unresolvable,
                true,
                &capabilities,
            ),
            None
        );
    }

    #[test]
    fn the_submenu_parent_is_never_activated() {
        let capabilities = empty_capabilities();
        assert_eq!(
            plan_file_context_menu_activation(
                FileContextMenuAction::OpenWith,
                &base_target(),
                true,
                &capabilities,
            ),
            None
        );
    }
}
