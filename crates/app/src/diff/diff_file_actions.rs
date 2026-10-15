//! Port of upstream/apps/web/src/diffFileActions.ts (+ test), with the
//! `packages/shared/src/path.ts` helpers it needs.
//!
//! `useRightPanelStore.openFile` is not ported yet (the file viewer is a cut
//! surface); [`DiffFileTargets`] stands in for it so the action's decision
//! logic stays testable and the owner of the right panel plugs in later.

use super::diff_panel_store::ScopedThreadRef;
use crate::components::chat::timeline::file_path_display::{
    is_unc_path, is_windows_absolute_path, is_windows_drive_path,
};

/// Where a diff file's primary action lands.
pub trait DiffFileTargets {
    /// `useRightPanelStore.openFile(threadRef, path)`.
    fn open_in_thread_file_viewer(&mut self, thread: &ScopedThreadRef, workspace_file_path: &str);
    /// `openInEditor(targetPath)`.
    fn open_in_editor(&mut self, target_path: &str);
}

fn is_root_path(value: &str) -> bool {
    value == "/"
        || value == "\\"
        || (value.len() == 3
            && is_windows_drive_path(value)
            && matches!(value.as_bytes()[2], b'/' | b'\\'))
}

fn trim_trailing_path_separators(value: &str) -> String {
    if value.is_empty() || is_root_path(value) {
        return value.to_string();
    }
    let trimmed = if value.starts_with('/') {
        value.trim_end_matches('/')
    } else {
        value.trim_end_matches(['/', '\\'])
    };
    if trimmed.is_empty() {
        return value.to_string();
    }
    let bytes = trimmed.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return format!("{trimmed}\\");
    }
    trimmed.to_string()
}

/// `normalizeProjectPathForDispatch`.
pub fn normalize_project_path_for_dispatch(value: &str) -> String {
    trim_trailing_path_separators(value.trim())
}

/// `normalizeProjectPathForComparison`.
pub fn normalize_project_path_for_comparison(value: &str) -> String {
    let normalized = normalize_project_path_for_dispatch(value);
    if is_windows_drive_path(&normalized) || is_unc_path(&normalized) {
        return normalized.replace('/', "\\").to_lowercase();
    }
    normalized
}

fn is_windows_drive_like(file_path: &str) -> bool {
    let bytes = file_path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn normalized_relative_path_segments(file_path: &str) -> Option<Vec<String>> {
    if file_path.starts_with('/')
        || is_windows_absolute_path(file_path)
        || is_windows_drive_like(file_path)
    {
        return None;
    }
    let segments: Vec<String> = file_path
        .replace('\\', "/")
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .map(str::to_string)
        .collect();
    if segments.is_empty() || segments.iter().any(|s| s == "..") {
        return None;
    }
    Some(segments)
}

fn repository_relative_workspace_segments(
    workspace_root: Option<&str>,
    repository_root: Option<&str>,
) -> Option<Vec<String>> {
    let (workspace_root, repository_root) = (workspace_root?, repository_root?);
    if workspace_root.is_empty() || repository_root.is_empty() {
        return None;
    }
    let workspace = normalize_project_path_for_comparison(workspace_root);
    let repository = normalize_project_path_for_comparison(repository_root);
    if workspace == repository {
        return Some(Vec::new());
    }
    let separator = if repository.contains('\\') { '\\' } else { '/' };
    let prefix = if repository.ends_with(separator) {
        repository.clone()
    } else {
        format!("{repository}{separator}")
    };
    let rest = workspace.strip_prefix(&prefix)?;
    Some(
        rest.split(['\\', '/'])
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// `resolveDiffPathForWorkspace`.
pub fn resolve_diff_path_for_workspace(
    file_path: &str,
    workspace_root: Option<&str>,
    repository_root: Option<&str>,
) -> Option<String> {
    let file_segments = normalized_relative_path_segments(file_path)?;
    let workspace_segments =
        repository_relative_workspace_segments(workspace_root, repository_root);
    let Some(workspace_segments) = workspace_segments.filter(|s| !s.is_empty()) else {
        return Some(file_segments.join("/"));
    };
    let case_insensitive = repository_root.is_some_and(is_windows_absolute_path);
    let belongs = workspace_segments
        .iter()
        .enumerate()
        .all(|(index, segment)| {
            let Some(candidate) = file_segments.get(index) else {
                return false;
            };
            if case_insensitive {
                candidate.to_lowercase() == *segment
            } else {
                candidate == segment
            }
        });
    if !belongs {
        return None;
    }
    let relative = &file_segments[workspace_segments.len()..];
    (!relative.is_empty()).then(|| relative.join("/"))
}

/// `resolvePathLinkTarget` for a plain workspace-relative path (diff paths
/// carry no `:line:col` position).
fn resolve_path_link_target(raw_path: &str, cwd: &str) -> String {
    if raw_path.starts_with('/') || is_windows_absolute_path(raw_path) {
        return raw_path.to_string();
    }
    let windows_style = is_windows_absolute_path(cwd) || cwd.contains(":\\");
    let clean_base = cwd.trim_end_matches(['/', '\\']);
    if windows_style {
        format!("{clean_base}\\{}", raw_path.replace('/', "\\"))
    } else {
        format!("{clean_base}/{}", raw_path.trim_start_matches('/'))
    }
}

pub struct OpenDiffFilePrimaryActionInput<'a> {
    pub thread: Option<&'a ScopedThreadRef>,
    pub file_path: &'a str,
    pub active_cwd: Option<&'a str>,
    pub repository_root: Option<&'a str>,
}

/// `openDiffFilePrimaryAction`.
pub fn open_diff_file_primary_action(
    input: OpenDiffFilePrimaryActionInput<'_>,
    targets: &mut dyn DiffFileTargets,
) {
    // `activeCwd ? … : …` upstream: an empty cwd is absent, not a root-relative base.
    let cwd = input.active_cwd.filter(|cwd| !cwd.is_empty());
    let Some(workspace_file_path) =
        resolve_diff_path_for_workspace(input.file_path, cwd, input.repository_root)
    else {
        return;
    };
    if let Some(thread) = input.thread {
        targets.open_in_thread_file_viewer(thread, &workspace_file_path);
        return;
    }
    let target = match cwd {
        Some(cwd) => resolve_path_link_target(&workspace_file_path, cwd),
        None => workspace_file_path,
    };
    targets.open_in_editor(&target);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        viewer: Vec<String>,
        editor: Vec<String>,
    }

    impl DiffFileTargets for Recorder {
        fn open_in_thread_file_viewer(&mut self, _: &ScopedThreadRef, path: &str) {
            self.viewer.push(path.to_string());
        }
        fn open_in_editor(&mut self, target_path: &str) {
            self.editor.push(target_path.to_string());
        }
    }

    fn thread() -> ScopedThreadRef {
        ScopedThreadRef::new("environment-local", "thread-1")
    }

    #[test]
    fn opens_diff_files_in_the_thread_file_viewer() {
        let mut targets = Recorder::default();
        open_diff_file_primary_action(
            OpenDiffFilePrimaryActionInput {
                thread: Some(&thread()),
                file_path: "apps/web/src/components/DiffPanel.tsx",
                active_cwd: Some("/repo/project"),
                repository_root: None,
            },
            &mut targets,
        );
        assert_eq!(
            targets.viewer,
            vec!["apps/web/src/components/DiffPanel.tsx"]
        );
        assert!(targets.editor.is_empty());
    }

    #[test]
    fn falls_back_to_the_editor_without_thread_context() {
        let mut targets = Recorder::default();
        open_diff_file_primary_action(
            OpenDiffFilePrimaryActionInput {
                thread: None,
                file_path: "apps/web/src/components/DiffPanel.tsx",
                active_cwd: Some("/repo/project"),
                repository_root: None,
            },
            &mut targets,
        );
        assert_eq!(
            targets.editor,
            vec!["/repo/project/apps/web/src/components/DiffPanel.tsx"]
        );
    }

    #[test]
    fn treats_an_empty_cwd_as_absent_when_opening_in_the_editor() {
        let mut targets = Recorder::default();
        open_diff_file_primary_action(
            OpenDiffFilePrimaryActionInput {
                thread: None,
                file_path: "apps/web/src/components/DiffPanel.tsx",
                active_cwd: Some(""),
                repository_root: None,
            },
            &mut targets,
        );
        assert_eq!(
            targets.editor,
            vec!["apps/web/src/components/DiffPanel.tsx"]
        );
    }

    #[test]
    fn opens_repository_relative_diff_files_from_a_nested_project() {
        let mut targets = Recorder::default();
        open_diff_file_primary_action(
            OpenDiffFilePrimaryActionInput {
                thread: Some(&thread()),
                file_path: "frontend/Dockerfile",
                active_cwd: Some("/repo/frontend"),
                repository_root: Some("/repo"),
            },
            &mut targets,
        );
        assert_eq!(targets.viewer, vec!["Dockerfile"]);
        assert!(targets.editor.is_empty());
    }

    #[test]
    fn preserves_repository_relative_paths_in_a_separate_worktree() {
        assert_eq!(
            resolve_diff_path_for_workspace(
                "frontend/Dockerfile",
                Some("/worktrees/feature"),
                Some("/repo")
            )
            .as_deref(),
            Some("frontend/Dockerfile")
        );
    }

    #[test]
    fn handles_windows_roots_and_mixed_diff_separators() {
        assert_eq!(
            resolve_diff_path_for_workspace(
                "Frontend/src\\index.ts",
                Some("C:\\repo\\frontend"),
                Some("C:\\repo")
            )
            .as_deref(),
            Some("src/index.ts")
        );
    }

    #[test]
    fn handles_filesystem_roots() {
        for (workspace_root, repository_root) in [("/frontend", "/"), ("C:\\frontend", "C:\\")] {
            assert_eq!(
                resolve_diff_path_for_workspace(
                    "frontend/index.ts",
                    Some(workspace_root),
                    Some(repository_root)
                )
                .as_deref(),
                Some("index.ts"),
                "{repository_root}"
            );
        }
    }

    #[test]
    fn does_not_open_an_out_of_project_diff_path() {
        for file_path in [
            "backend/server.ts",
            "frontend2/app.ts",
            "frontend/../secret.ts",
            "C:secret.ts",
        ] {
            let mut targets = Recorder::default();
            open_diff_file_primary_action(
                OpenDiffFilePrimaryActionInput {
                    thread: Some(&thread()),
                    file_path,
                    active_cwd: Some("/repo/frontend"),
                    repository_root: Some("/repo"),
                },
                &mut targets,
            );
            assert!(targets.viewer.is_empty(), "{file_path}");
            assert!(targets.editor.is_empty(), "{file_path}");
        }
    }
}
