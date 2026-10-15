//! Port of upstream/packages/shared/src/path.ts

use crate::js_string::{slice_utf16, utf16_len};
use turin_contracts::base_schemas::js_trim;
use unicode_normalization::UnicodeNormalization;

pub fn is_windows_drive_path(value: &str) -> bool {
    // /^[a-zA-Z]:([/\\]|$)/
    let mut chars = value.chars();
    match (chars.next(), chars.next(), chars.next()) {
        (Some(drive), Some(':'), third) if drive.is_ascii_alphabetic() => {
            matches!(third, None | Some('/') | Some('\\'))
        }
        _ => false,
    }
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
    if value == "/" || value == "\\" {
        return true;
    }
    let mut chars = value.chars();
    matches!((chars.next(), chars.next(), chars.next(), chars.next()),
        (Some(drive), Some(':'), Some(sep), None) if drive.is_ascii_alphabetic() && (sep == '/' || sep == '\\'))
}

fn trim_trailing_path_separators(value: &str) -> String {
    if value.is_empty() || is_root_path(value) {
        return value.to_owned();
    }
    let trimmed = if value.starts_with('/') {
        value.trim_end_matches('/')
    } else {
        value.trim_end_matches(['/', '\\'])
    };
    if trimmed.is_empty() {
        return value.to_owned();
    }
    if is_bare_drive(trimmed) {
        return format!("{trimmed}\\");
    }
    trimmed.to_owned()
}

fn is_bare_drive(value: &str) -> bool {
    // /^[a-zA-Z]:$/
    let mut chars = value.chars();
    matches!((chars.next(), chars.next(), chars.next()),
        (Some(drive), Some(':'), None) if drive.is_ascii_alphabetic())
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

// Windows refuses these as file names, with or without an extension.
fn is_windows_reserved_name(slug: &str) -> bool {
    let lowered = slug.to_ascii_lowercase();
    matches!(lowered.as_str(), "con" | "prn" | "aux" | "nul")
        || (lowered.len() == 4
            && (lowered.starts_with("com") || lowered.starts_with("lpt"))
            && lowered.as_bytes()[3].is_ascii_digit()
            && lowered.as_bytes()[3] != b'0')
}

/// Folder name for a project started from just a name ("Pinball Stats" becomes
/// "pinball-stats").
pub fn new_project_folder_name(name: &str) -> String {
    let nfkd: String = name.nfkd().collect();
    // Strip combining marks (U+0300..U+036F), like upstream's literal class.
    let stripped: String = nfkd
        .chars()
        .filter(|c| !('\u{0300}'..='\u{036f}').contains(c))
        .collect();
    let lowered = stripped.to_lowercase();
    let mut slug = String::new();
    let mut in_run = false;
    for ch in lowered.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            slug.push(ch);
            in_run = false;
        } else if !in_run && !slug.is_empty() {
            slug.push('-');
            in_run = true;
        } else if !in_run && slug.is_empty() {
            // Upstream's replace does not create a leading '-' because the
            // leading '-' strip happens after; replicate by skipping leading
            // non-alphanumerics entirely (they would be stripped anyway).
            in_run = true;
        }
    }
    // Slice to 64 UTF-16 units (ASCII-only here, so chars == units).
    let slug: String = slug.chars().take(64).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        return "project".to_owned();
    }
    if is_windows_reserved_name(slug) {
        return format!("{slug}-project");
    }
    slug.to_owned()
}

/// Browser URL parsers write `C:/foo` as `/C:/foo` for file URLs.
pub fn strip_slash_prefixed_windows_drive(path: &str) -> &str {
    // /^\/[A-Za-z]:[\\/]/
    let mut chars = path.chars();
    match (chars.next(), chars.next(), chars.next(), chars.next()) {
        (Some('/'), Some(drive), Some(':'), Some(sep))
            if drive.is_ascii_alphabetic() && (sep == '/' || sep == '\\') =>
        {
            &path[1..]
        }
        _ => path,
    }
}

pub fn file_basename(path: &str) -> String {
    // A trailing separator is a valid way to write a directory. Trim it before
    // taking the final segment so the label is never empty.
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return path.to_owned();
    }
    match (trimmed.rfind('/'), trimmed.rfind('\\')) {
        (None, None) => trimmed.to_owned(),
        (a, b) => {
            let separator_index = a.max(b).expect("one separator exists");
            trimmed[separator_index + 1..].to_owned()
        }
    }
}

pub fn workspace_relative_file_path(path: &str, workspace_root: Option<&str>) -> Option<String> {
    let workspace_root = workspace_root.filter(|root| !root.is_empty())?;
    let slashed_path = path.replace('\\', "/");
    let normalized_path = strip_slash_prefixed_windows_drive(&slashed_path);
    let slashed_root = workspace_root.replace('\\', "/");
    let normalized_root = strip_slash_prefixed_windows_drive(&slashed_root)
        .trim_end_matches('/')
        .to_owned();
    let case_insensitive =
        is_windows_absolute_path(strip_slash_prefixed_windows_drive(workspace_root));
    let path_for_compare = if case_insensitive {
        normalized_path.to_lowercase()
    } else {
        normalized_path.to_owned()
    };
    let root_for_compare = if case_insensitive {
        normalized_root.to_lowercase()
    } else {
        normalized_root.clone()
    };
    if path_for_compare.trim_end_matches('/') == root_for_compare {
        return Some(".".to_owned());
    }
    if !path_for_compare.starts_with(&format!("{root_for_compare}/")) {
        return None;
    }
    // Upstream slices by the original-case root's UTF-16 length.
    let skip = utf16_len(&normalized_root) + 1;
    Some(slice_utf16(normalized_path, skip, usize::MAX).to_owned())
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
        format!("{}\\{}", base, path.replace('/', "\\"))
    } else {
        format!("{base}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_literal_paths_within_roots() {
        let cases = [
            ("report:123", "/workspace", "/workspace/report:123"),
            ("/workspace/report:12:3", "/other", "/workspace/report:12:3"),
            ("src/file", "C:\\Workspace\\", "C:\\Workspace\\src\\file"),
            ("C:/Workspace/file", "/other", "C:/Workspace/file"),
            ("file", "\\\\host\\share\\", "\\\\host\\share\\file"),
            ("file", "/", "/file"),
            ("~/report:123", "/workspace", "~/report:123"),
        ];
        for (path, root, expected) in cases {
            assert_eq!(resolve_workspace_file_path(path, root), expected);
        }
    }

    #[test]
    fn detects_windows_drive_paths() {
        assert!(is_windows_drive_path("C:\\repo"));
        assert!(is_windows_drive_path("D:/repo"));
        assert!(!is_windows_drive_path("/repo"));
    }

    #[test]
    fn detects_unc_paths() {
        assert!(is_unc_path("\\\\server\\share\\repo"));
        assert!(!is_unc_path("C:\\repo"));
    }

    #[test]
    fn detects_windows_absolute_paths() {
        assert!(is_windows_absolute_path("C:\\repo"));
        assert!(is_windows_absolute_path("\\\\server\\share\\repo"));
        assert!(!is_windows_absolute_path("./repo"));
    }

    #[test]
    fn detects_explicit_relative_paths() {
        assert!(is_explicit_relative_path("."));
        assert!(is_explicit_relative_path(".."));
        assert!(is_explicit_relative_path("./repo"));
        assert!(is_explicit_relative_path("..\\repo"));
        assert!(!is_explicit_relative_path("~/repo"));
    }

    #[test]
    fn normalizes_a_bare_windows_drive_root_like_one_with_a_separator() {
        assert_eq!(normalize_project_path_for_dispatch("C:"), "C:\\");
        assert_eq!(normalize_project_path_for_comparison("C:"), "c:\\");
        assert_eq!(
            normalize_project_path_for_comparison("C:"),
            normalize_project_path_for_comparison("C:\\")
        );
        assert_eq!(
            normalize_project_path_for_comparison("C:"),
            normalize_project_path_for_comparison("C:/")
        );
        assert_eq!(
            normalize_project_path_for_dispatch("C:\\repo\\"),
            "C:\\repo"
        );
    }

    #[test]
    fn names_a_new_projects_folder_from_any_typed_name() {
        assert_eq!(new_project_folder_name("Pinball Stats"), "pinball-stats");
        assert_eq!(new_project_folder_name("  Café & Crème!  "), "cafe-creme");
        assert_eq!(new_project_folder_name("../../etc"), "etc");
        // Nothing usable left, so the server falls back to a fixed name.
        assert_eq!(new_project_folder_name("🎱🎱"), "project");
        assert_eq!(
            new_project_folder_name(&format!("{} b", "a".repeat(63))),
            "a".repeat(63)
        );
        // Windows cannot make folders with device names.
        assert_eq!(new_project_folder_name("Con"), "con-project");
        assert_eq!(new_project_folder_name("LPT1"), "lpt1-project");
        assert_eq!(new_project_folder_name("console"), "console");
    }

    #[test]
    fn labels_paths_with_file_basename() {
        let cases = [
            ("/tmp/favicons/", "favicons"),
            ("C:\\Users\\kelchm\\.claude\\", ".claude"),
            ("/tmp/", "tmp"),
            ("AGENTS.md", "AGENTS.md"),
            ("/", "/"),
        ];
        for (path, basename) in cases {
            assert_eq!(file_basename(path), basename, "{path}");
        }
    }

    #[test]
    fn computes_workspace_relative_paths() {
        let cases: &[(&str, &str, Option<&str>)] = &[
            (
                "/repo/project/src/main.ts",
                "/repo/project",
                Some("src/main.ts"),
            ),
            (
                "/repo/project/src/main.ts",
                "/repo/project/",
                Some("src/main.ts"),
            ),
            (
                "C:\\Users\\mike\\t3code\\apps\\web\\a.ts",
                "C:/Users/mike/t3code",
                Some("apps/web/a.ts"),
            ),
            (
                "/C:/Users/mike/t3code/apps/web/a.ts",
                "C:/Users/mike/t3code",
                Some("apps/web/a.ts"),
            ),
            ("/Repo/Project/src/main.ts", "/repo/project", None),
            ("/tmp/case/project/probe.txt", "/tmp/case/Project", None),
            ("//tmp/case/project/probe.txt", "//tmp/case/Project", None),
            (
                "/tmp/case/Project/probe.txt",
                "/tmp/case/Project",
                Some("probe.txt"),
            ),
            (
                "C:/USERS/mike/t3code/main.ts",
                "c:/users/MIKE/t3code",
                Some("main.ts"),
            ),
            (
                "/C:/USERS/mike/t3code/main.ts",
                "/c:/users/MIKE/t3code",
                Some("main.ts"),
            ),
            (
                "\\\\server\\share\\PROJECT\\main.ts",
                "\\\\Server\\Share\\Project",
                Some("main.ts"),
            ),
            ("/tmp/repo/file.ts", "/", Some("tmp/repo/file.ts")),
            ("C:/Users/MIKE/main.ts", "c:/", Some("Users/MIKE/main.ts")),
            (
                "\\\\server\\SHARE\\file.ts",
                "\\\\Server\\Share\\",
                Some("file.ts"),
            ),
            ("/tmp/repo/file.ts ", "/tmp/repo", Some("file.ts ")),
            ("/tmp/report.ts", "/repo/project", None),
            ("/repo/project-two/a.ts", "/repo/project", None),
        ];
        for (path, root, expected) in cases {
            assert_eq!(
                workspace_relative_file_path(path, Some(root)),
                expected.map(str::to_owned),
                "{path}"
            );
        }
        assert_eq!(workspace_relative_file_path("/repo/a", None), None);
    }
}
