//! Port of upstream/packages/client-runtime/src/state/projects.ts
//! (path helpers for the add-project flow and project lookup).

use std::sync::LazyLock;

use regex::Regex;

use crate::state::support::path::{
    is_explicit_relative_path, is_unc_path, is_windows_absolute_path, is_windows_drive_path,
};

#[allow(unused_imports)]
pub use crate::state::support::path::is_explicit_relative_path as is_explicit_relative_project_path;
pub use crate::state::support::path::{
    normalize_project_path_for_comparison, normalize_project_path_for_dispatch,
};

pub fn is_windows_platform(platform: &str) -> bool {
    static WINDOWS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^win(dows)?").expect("static regex"));
    WINDOWS.is_match(platform)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AbsolutePathKind {
    Unix,
    Windows,
}

fn absolute_path_kind(value: &str) -> Option<AbsolutePathKind> {
    if is_windows_drive_path(value) || is_unc_path(value) {
        Some(AbsolutePathKind::Windows)
    } else if value.starts_with('/') {
        Some(AbsolutePathKind::Unix)
    } else {
        None
    }
}

fn preferred_path_separator(value: &str) -> char {
    match absolute_path_kind(value) {
        Some(AbsolutePathKind::Windows) => '\\',
        Some(AbsolutePathKind::Unix) => '/',
        None => {
            if value.contains('\\') {
                '\\'
            } else {
                '/'
            }
        }
    }
}

pub fn has_trailing_path_separator(value: &str) -> bool {
    if absolute_path_kind(value) == Some(AbsolutePathKind::Unix) {
        value.ends_with('/')
    } else {
        value.ends_with(['\\', '/'])
    }
}

fn split_path_segments(value: &str, separator: char) -> Vec<&str> {
    let is_separator = |c: char| {
        if separator == '/' {
            c == '/'
        } else {
            c == '/' || c == '\\'
        }
    };
    value
        .split(is_separator)
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// UTF-16 index in JS; a byte index here, used only to slice the same string.
fn last_path_separator_index(value: &str) -> Option<usize> {
    if absolute_path_kind(value) == Some(AbsolutePathKind::Unix) {
        value.rfind('/')
    } else {
        value.rfind(['/', '\\'])
    }
}

struct AbsolutePath {
    root: String,
    separator: char,
    segments: Vec<String>,
}

fn split_absolute_path(value: &str) -> Option<AbsolutePath> {
    if is_windows_drive_path(value) {
        let root = format!("{}\\", &value[..2]);
        let segments = split_path_segments(&value[root.len().min(value.len())..], '\\')
            .into_iter()
            .map(str::to_owned)
            .collect();
        return Some(AbsolutePath {
            root,
            separator: '\\',
            segments,
        });
    }
    if is_unc_path(value) {
        let segments = split_path_segments(value, '\\');
        let (server, share) = (segments.first()?, segments.get(1)?);
        return Some(AbsolutePath {
            root: format!("\\\\{server}\\{share}\\"),
            separator: '\\',
            segments: segments[2..].iter().map(|s| (*s).to_owned()).collect(),
        });
    }
    if let Some(rest) = value.strip_prefix('/') {
        return Some(AbsolutePath {
            root: "/".to_owned(),
            separator: '/',
            segments: split_path_segments(rest, '/')
                .into_iter()
                .map(str::to_owned)
                .collect(),
        });
    }
    None
}

pub fn is_filesystem_browse_query(value: &str, platform: &str) -> bool {
    let allow_windows_paths = is_windows_platform(platform);
    value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with(".\\")
        || value.starts_with("..\\")
        || value.starts_with('/')
        || value.starts_with("~/")
        || (allow_windows_paths && is_windows_absolute_path(value))
}

pub fn is_unsupported_windows_project_path(value: &str, platform: &str) -> bool {
    is_windows_absolute_path(value) && !is_windows_platform(platform)
}

pub fn resolve_project_path_for_dispatch(value: &str, cwd: Option<&str>) -> String {
    let trimmed = crate::state::contracts::js_trim(value);
    let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) else {
        return normalize_project_path_for_dispatch(trimmed);
    };
    if !is_explicit_relative_path(trimmed) {
        return normalize_project_path_for_dispatch(trimmed);
    }
    let Some(absolute_base) = split_absolute_path(&normalize_project_path_for_dispatch(cwd)) else {
        return normalize_project_path_for_dispatch(trimmed);
    };

    let mut next_segments = absolute_base.segments.clone();
    for segment in trimmed.split(['\\', '/']) {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            next_segments.pop();
            continue;
        }
        next_segments.push(segment.to_owned());
    }

    let joined = next_segments.join(&absolute_base.separator.to_string());
    normalize_project_path_for_dispatch(&if joined.is_empty() {
        absolute_base.root
    } else {
        format!("{}{joined}", absolute_base.root)
    })
}

/// Finds the project whose workspace root is `candidate_path` (after normalization).
pub fn find_project_by_path<'a, T>(
    projects: &'a [T],
    candidate_path: &str,
    cwd_of: impl Fn(&T) -> Option<String>,
) -> Option<&'a T> {
    let normalized_candidate = normalize_project_path_for_comparison(candidate_path);
    if normalized_candidate.is_empty() {
        return None;
    }
    projects.iter().find(|project| {
        cwd_of(project)
            .is_some_and(|cwd| normalize_project_path_for_comparison(&cwd) == normalized_candidate)
    })
}

/// Whether a project is its environment's Scratch project (`ServerConfig.scratchWorkspaceRoot`).
pub fn is_scratch_project(workspace_root: &str, scratch_workspace_root: Option<&str>) -> bool {
    scratch_workspace_root.is_some_and(|scratch| {
        let candidate = normalize_project_path_for_comparison(scratch);
        !candidate.is_empty() && normalize_project_path_for_comparison(workspace_root) == candidate
    })
}

pub fn infer_project_title_from_path(value: &str) -> String {
    let normalized = normalize_project_path_for_dispatch(value);
    if let Some(absolute) = split_absolute_path(&normalized) {
        return absolute
            .segments
            .iter()
            .rfind(|segment| !segment.is_empty())
            .cloned()
            .unwrap_or(normalized);
    }
    normalized
        .split(['/', '\\'])
        .rfind(|segment| !segment.is_empty())
        .map(str::to_owned)
        .unwrap_or(normalized)
}

pub fn append_browse_path_segment(current_path: &str, segment: &str) -> String {
    let separator = preferred_path_separator(current_path);
    format!(
        "{}{segment}{separator}",
        get_browse_directory_path(current_path)
    )
}

pub fn get_browse_leaf_path_segment(current_path: &str) -> &str {
    match last_path_separator_index(current_path) {
        Some(index) => &current_path[index + 1..],
        None => current_path,
    }
}

pub fn get_browse_directory_path(current_path: &str) -> &str {
    if has_trailing_path_separator(current_path) {
        return current_path;
    }
    match last_path_separator_index(current_path) {
        Some(index) => &current_path[..index + 1],
        None => current_path,
    }
}

pub fn ensure_browse_directory_path(current_path: &str) -> String {
    let trimmed = crate::state::contracts::js_trim(current_path);
    if trimmed.is_empty() || has_trailing_path_separator(trimmed) {
        return trimmed.to_owned();
    }
    format!("{trimmed}{}", preferred_path_separator(trimmed))
}

pub fn get_browse_parent_path(current_path: &str) -> Option<String> {
    let trimmed = normalize_project_path_for_dispatch(current_path);
    if let Some(absolute) = split_absolute_path(&trimmed) {
        if absolute.segments.is_empty() {
            return None;
        }
        if absolute.segments.len() == 1 {
            return Some(absolute.root);
        }
        let parent =
            absolute.segments[..absolute.segments.len() - 1].join(&absolute.separator.to_string());
        return Some(format!("{}{parent}{}", absolute.root, absolute.separator));
    }

    let separator = preferred_path_separator(current_path);
    let index = last_path_separator_index(&trimmed)?;
    let bytes = trimmed.as_bytes();
    if index == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Some(format!("{}{separator}", &trimmed[..2]));
    }
    Some(trimmed[..index + 1].to_owned())
}

pub fn can_navigate_up(current_path: &str) -> bool {
    has_trailing_path_separator(current_path) && get_browse_parent_path(current_path).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_windows_platforms() {
        assert!(is_windows_platform("win32"));
        assert!(is_windows_platform("Windows"));
        assert!(!is_windows_platform("darwin"));
    }

    #[test]
    fn resolves_explicit_relative_paths_against_the_current_project() {
        assert_eq!(
            resolve_project_path_for_dispatch("../other", Some("/work/repo")),
            "/work/other"
        );
        assert_eq!(
            resolve_project_path_for_dispatch("./sub/dir/", Some("/work/repo")),
            "/work/repo/sub/dir"
        );
        assert_eq!(
            resolve_project_path_for_dispatch("../../..", Some("/work/repo")),
            "/"
        );
        assert_eq!(
            resolve_project_path_for_dispatch("../x", Some("C:\\work\\repo")),
            "C:\\work\\x"
        );
        // Not relative, or nothing to resolve against: just normalized.
        assert_eq!(
            resolve_project_path_for_dispatch(" /abs/path/ ", Some("/work")),
            "/abs/path"
        );
        assert_eq!(resolve_project_path_for_dispatch("../x", None), "../x");
    }

    #[test]
    fn finds_projects_by_normalized_path() {
        struct P(&'static str);
        let projects = [P("/work/a"), P("C:\\Repo\\B")];
        fn cwd(project: &P) -> Option<String> {
            Some(project.0.to_owned())
        }
        assert_eq!(
            find_project_by_path(&projects, "/work/a/", cwd).map(|p| p.0),
            Some("/work/a")
        );
        assert_eq!(
            find_project_by_path(&projects, "c:/repo/b", cwd).map(|p| p.0),
            Some("C:\\Repo\\B")
        );
        assert!(find_project_by_path(&projects, "/work/missing", cwd).is_none());
        assert!(find_project_by_path(&projects, "", cwd).is_none());
    }

    #[test]
    fn recognises_the_scratch_project() {
        assert!(is_scratch_project(
            "/home/u/.t3/scratch/",
            Some("/home/u/.t3/scratch")
        ));
        assert!(!is_scratch_project("/repo", Some("/home/u/.t3/scratch")));
        assert!(!is_scratch_project("/repo", None));
    }

    #[test]
    fn infers_titles_from_paths() {
        assert_eq!(infer_project_title_from_path("/work/my-repo/"), "my-repo");
        assert_eq!(infer_project_title_from_path("C:\\work\\repo"), "repo");
        assert_eq!(infer_project_title_from_path("/"), "/");
        assert_eq!(infer_project_title_from_path("relative/dir"), "dir");
    }

    #[test]
    fn browses_directories() {
        assert_eq!(append_browse_path_segment("/work/", "repo"), "/work/repo/");
        assert_eq!(
            append_browse_path_segment("/work/re", "repo"),
            "/work/repo/"
        );
        assert_eq!(get_browse_leaf_path_segment("/work/re"), "re");
        assert_eq!(get_browse_directory_path("/work/re"), "/work/");
        assert_eq!(ensure_browse_directory_path(" /work "), "/work/");
        assert_eq!(ensure_browse_directory_path("C:\\work"), "C:\\work\\");
        assert_eq!(
            get_browse_parent_path("/work/repo/"),
            Some("/work/".to_owned())
        );
        assert_eq!(get_browse_parent_path("/work/"), Some("/".to_owned()));
        assert_eq!(get_browse_parent_path("/"), None);
        assert_eq!(
            get_browse_parent_path("C:\\work\\repo\\"),
            Some("C:\\work\\".to_owned())
        );
        assert!(can_navigate_up("/work/repo/"));
        assert!(!can_navigate_up("/work/repo"));
        assert!(!can_navigate_up("/"));
    }

    #[test]
    fn classifies_browse_queries() {
        assert!(is_filesystem_browse_query("./a", ""));
        assert!(is_filesystem_browse_query("~/a", ""));
        assert!(is_filesystem_browse_query("C:\\a", "win32"));
        assert!(!is_filesystem_browse_query("C:\\a", "darwin"));
        assert!(!is_filesystem_browse_query("plain", ""));
        assert!(is_unsupported_windows_project_path("C:\\a", "darwin"));
        assert!(!is_unsupported_windows_project_path("C:\\a", "win32"));
    }
}
