//! Port of the helpers of upstream/packages/shared/src/path.ts that the
//! view-model layer reads.

use std::sync::LazyLock;

use regex::Regex;

use crate::state::contracts::js_trim;

pub fn is_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes.len() == 2 || bytes[2] == b'/' || bytes[2] == b'\\')
}

pub fn is_unc_path(value: &str) -> bool {
    value.starts_with("\\\\")
}

pub fn is_windows_absolute_path(value: &str) -> bool {
    is_unc_path(value) || is_windows_drive_path(value)
}

pub fn is_explicit_relative_path(value: &str) -> bool {
    value == "."
        || value == ".."
        || value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with(".\\")
        || value.starts_with("..\\")
}

fn is_root_path(value: &str) -> bool {
    // The drive separator is required: a bare `C:` is not the drive root (it
    // means "current directory on C:"), and treating it as already-canonical
    // would leave it as `C:` while `C:\` and `C:/` normalize to the drive root,
    // so the same location would fail project identity/dedup comparisons.
    let bytes = value.as_bytes();
    value == "/"
        || value == "\\"
        || (bytes.len() == 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'/' || bytes[2] == b'\\'))
}

fn trim_trailing_path_separators(value: &str) -> String {
    if value.is_empty() || is_root_path(value) {
        return value.to_owned();
    }
    let trimmed = if value.starts_with('/') {
        value.trim_end_matches('/')
    } else {
        value.trim_end_matches(['\\', '/'])
    };
    if trimmed.is_empty() {
        return value.to_owned();
    }
    let bytes = trimmed.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        format!("{trimmed}\\")
    } else {
        trimmed.to_owned()
    }
}

pub fn normalize_project_path_for_dispatch(value: &str) -> String {
    trim_trailing_path_separators(js_trim(value))
}

pub fn normalize_project_path_for_comparison(value: &str) -> String {
    let normalized = normalize_project_path_for_dispatch(value);
    if is_windows_drive_path(&normalized) || is_unc_path(&normalized) {
        return normalized.replace('/', "\\").to_lowercase();
    }
    normalized
}

static SLASH_PREFIXED_WINDOWS_DRIVE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/[A-Za-z]:[\\/]").expect("static regex"));

/// Browser URL parsers write `C:/foo` as `/C:/foo` for file URLs.
pub fn strip_slash_prefixed_windows_drive(path: &str) -> &str {
    if SLASH_PREFIXED_WINDOWS_DRIVE.is_match(path) {
        &path[1..]
    } else {
        path
    }
}

pub fn file_basename(path: &str) -> &str {
    // A trailing separator is a valid way to write a directory. Trim it before
    // taking the final segment so the label is never empty.
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return path;
    }
    match trimmed.rfind(['/', '\\']) {
        Some(index) => &trimmed[index + 1..],
        None => trimmed,
    }
}

pub fn workspace_relative_file_path(path: &str, workspace_root: Option<&str>) -> Option<String> {
    let workspace_root = workspace_root.filter(|root| !root.is_empty())?;
    let normalized_path = strip_slash_prefixed_windows_drive(&path.replace('\\', "/")).to_owned();
    let normalized_root = strip_slash_prefixed_windows_drive(&workspace_root.replace('\\', "/"))
        .trim_end_matches('/')
        .to_owned();
    let case_insensitive =
        is_windows_absolute_path(strip_slash_prefixed_windows_drive(workspace_root));
    let (path_for_compare, root_for_compare) = if case_insensitive {
        (
            normalized_path.to_lowercase(),
            normalized_root.to_lowercase(),
        )
    } else {
        (normalized_path.clone(), normalized_root.clone())
    };
    if path_for_compare.trim_end_matches('/') == root_for_compare {
        return Some(".".to_owned());
    }
    if !path_for_compare.starts_with(&format!("{root_for_compare}/")) {
        return None;
    }
    // `slice(normalizedRoot.length + 1)` counts UTF-16 units upstream; the
    // root prefix matched above, so a byte offset is the same position.
    Some(normalized_path[normalized_root.len() + 1..].to_owned())
}

pub fn is_absolute_path(value: &str) -> bool {
    value.starts_with('/') || is_windows_absolute_path(value)
}

/// Resolve a literal filesystem path without interpreting file-link positions.
/// Home-relative paths stay intact for the server to expand.
pub fn resolve_workspace_file_path(path: &str, workspace_root: &str) -> String {
    if is_absolute_path(path) || path.starts_with("~/") || path.starts_with("~\\") {
        return path.to_owned();
    }
    let base = workspace_root.trim_end_matches(['/', '\\']);
    if is_windows_absolute_path(workspace_root) {
        format!("{base}\\{}", path.replace('/', "\\"))
    } else {
        format!("{base}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_ignores_trailing_separators() {
        assert_eq!(file_basename("/worktrees/agents"), "agents");
        assert_eq!(file_basename("C:\\work\\agents\\"), "agents");
        assert_eq!(file_basename("/"), "/");
        assert_eq!(file_basename("plain"), "plain");
    }

    #[test]
    fn comparison_normalizes_windows_paths() {
        assert_eq!(
            normalize_project_path_for_comparison("C:/Repo/"),
            "c:\\repo"
        );
        assert_eq!(normalize_project_path_for_comparison(" /repo/ "), "/repo");
        assert_eq!(normalize_project_path_for_comparison("C:"), "c:\\");
        assert_eq!(normalize_project_path_for_comparison("/"), "/");
    }

    #[test]
    fn relative_paths_resolve_against_the_workspace() {
        assert_eq!(
            workspace_relative_file_path("/repo/src/a.ts", Some("/repo")).as_deref(),
            Some("src/a.ts")
        );
        assert_eq!(
            workspace_relative_file_path("/repo", Some("/repo/")).as_deref(),
            Some(".")
        );
        assert_eq!(
            workspace_relative_file_path("/other/a", Some("/repo")),
            None
        );
        assert_eq!(
            workspace_relative_file_path("C:\\Repo\\a.ts", Some("c:/repo")).as_deref(),
            Some("a.ts")
        );
        assert_eq!(resolve_workspace_file_path("a/b", "/repo/"), "/repo/a/b");
        assert_eq!(
            resolve_workspace_file_path("a/b", "C:\\repo"),
            "C:\\repo\\a\\b"
        );
        assert_eq!(resolve_workspace_file_path("~/x", "/repo"), "~/x");
    }
}
