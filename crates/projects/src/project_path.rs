//! Port of the project path helpers in upstream/packages/shared/src/path.ts
//! (`normalizeProjectPathForDispatch`, `normalizeProjectPathForComparison`).
//! They live here until the Rust `shared` crate ports `path.ts`.

use turin_contracts::js_trim;

/// Upstream `normalizeProjectPathForDispatch`: trims whitespace and trailing separators.
pub fn normalize_project_path_for_dispatch(value: &str) -> String {
    trim_trailing_path_separators(js_trim(value))
}

/// Upstream `normalizeProjectPathForComparison`. Windows drive and UNC paths
/// compare with backslashes and case-folded, since the filesystem ignores case there.
pub fn normalize_project_path_for_comparison(value: &str) -> String {
    let normalized = normalize_project_path_for_dispatch(value);
    if is_windows_drive_path(&normalized) || is_unc_path(&normalized) {
        return normalized.replace('/', "\\").to_lowercase();
    }
    normalized
}

fn is_windows_drive_path(value: &str) -> bool {
    // `/^[a-zA-Z]:([/\\]|$)/`
    let bytes = value.as_bytes();
    bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes.len() == 2 || bytes[2] == b'/' || bytes[2] == b'\\')
}

fn is_unc_path(value: &str) -> bool {
    value.starts_with("\\\\")
}

fn is_root_path(value: &str) -> bool {
    // A bare `C:` is not the drive root (it means "current directory on C:").
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
        value.trim_end_matches(&['\\', '/'][..])
    };
    if trimmed.is_empty() {
        return value.to_owned();
    }
    // `C:` gains a separator so it reads as the drive root, not the drive's cwd.
    let bytes = trimmed.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return format!("{trimmed}\\");
    }
    trimmed.to_owned()
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::{normalize_project_path_for_comparison, normalize_project_path_for_dispatch};

    #[test]
    fn dispatch_trims_whitespace_and_trailing_separators() {
        assert_eq!(
            normalize_project_path_for_dispatch("  /repo/app/  "),
            "/repo/app"
        );
        assert_eq!(normalize_project_path_for_dispatch("/"), "/");
        assert_eq!(normalize_project_path_for_dispatch("C:\\"), "C:\\");
        assert_eq!(normalize_project_path_for_dispatch("C:"), "C:\\");
        assert_eq!(
            normalize_project_path_for_dispatch("C:\\work\\"),
            "C:\\work"
        );
    }

    #[test]
    fn comparison_folds_case_only_for_windows_paths() {
        assert_eq!(
            normalize_project_path_for_comparison("/Repo/App"),
            "/Repo/App"
        );
        assert_eq!(
            normalize_project_path_for_comparison("C:/Work/App/"),
            "c:\\work\\app"
        );
        assert_eq!(
            normalize_project_path_for_comparison("\\\\Server\\Share/Dir"),
            "\\\\server\\share\\dir"
        );
    }
}

/// Upstream `expandHomePath` (`packages/provider-core/src/server/pathExpansion.ts`).
/// Expands a leading `~`, `~/…`, or `~\…` to `home`. Spawned processes get no
/// shell expansion, so `CODEX_HOME=~/.codex-work` would otherwise stay literal.
/// `~user` is not expanded.
pub fn expand_home_path(value: &str, home: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if value == "~" {
        return home.to_owned();
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        return std::path::Path::new(home)
            .join(rest)
            .to_string_lossy()
            .into_owned();
    }
    value.to_owned()
}
