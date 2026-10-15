//! Port of upstream/apps/web/src/filePathDisplay.ts
//!
//! Also carries the pieces of upstream/packages/shared/src/path.ts
//! (`isWindowsDrivePath`, `isUncPath`, `isWindowsAbsolutePath`,
//! `stripSlashPrefixedWindowsDrive`, `fileBasename`) and
//! upstream/packages/shared/src/fileLinks.ts (`FilePathPosition`,
//! `splitFilePathPosition`, `formatFilePathPosition`) that it needs, so the
//! timeline has no dependency on a shared crate yet.

use std::sync::LazyLock;

use regex::Regex;

// ---------------------------------------------------------------------------
// upstream/packages/shared/src/path.ts
// ---------------------------------------------------------------------------

/// `/^[a-zA-Z]:([/\\]|$)/`
pub fn is_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes.get(2), None | Some(b'/' | b'\\'))
}

pub fn is_unc_path(value: &str) -> bool {
    value.starts_with("\\\\")
}

pub fn is_windows_absolute_path(value: &str) -> bool {
    is_unc_path(value) || is_windows_drive_path(value)
}

/// Browser URL parsers write `C:/foo` as `/C:/foo` for file URLs.
pub fn strip_slash_prefixed_windows_drive(path: &str) -> &str {
    // SLASH_PREFIXED_WINDOWS_DRIVE_PATTERN = /^\/[A-Za-z]:[\\/]/
    let bytes = path.as_bytes();
    let slash_prefixed_drive = bytes.len() >= 4
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && matches!(bytes[3], b'/' | b'\\');
    if slash_prefixed_drive {
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
        Some(separator_index) => &trimmed[separator_index + 1..],
        None => trimmed,
    }
}

// ---------------------------------------------------------------------------
// upstream/packages/shared/src/fileLinks.ts
// ---------------------------------------------------------------------------

// JS `\d` is ASCII-only; Rust's is Unicode, so the digit classes are spelled out.
static POSITION_SUFFIX_CAPTURE_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r":([0-9]+)(?::([0-9]+))?$").expect("static regex"));

static POSITION_HASH_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^#L([0-9]+)(?:C([0-9]+))?$").expect("static regex"));

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilePathPosition {
    pub path: String,
    pub line: Option<u64>,
    pub column: Option<u64>,
}

/// `Number.parseInt(digits, 10)`. The capture is all ASCII digits, so the only
/// failure is overflow; JS keeps a huge (positive) float there, so saturate.
fn parse_position_number(digits: &str) -> u64 {
    digits.parse().unwrap_or(u64::MAX)
}

/// `splitFilePathPosition(path, hash = "")`; pass `""` for upstream's default.
pub fn split_file_path_position(path: &str, hash: &str) -> FilePathPosition {
    let suffix_match = POSITION_SUFFIX_CAPTURE_PATTERN.captures(path);
    let hash_match = match suffix_match {
        Some(_) => None,
        None => POSITION_HASH_PATTERN.captures(hash),
    };
    let Some(line_match) = suffix_match
        .as_ref()
        .or(hash_match.as_ref())
        .and_then(|captures| captures.get(1))
    else {
        return FilePathPosition {
            path: path.to_string(),
            ..FilePathPosition::default()
        };
    };

    let line = parse_position_number(line_match.as_str());
    let column = suffix_match
        .as_ref()
        .or(hash_match.as_ref())
        .and_then(|captures| captures.get(2))
        .map(|column| parse_position_number(column.as_str()));
    let path = match suffix_match.as_ref().and_then(|captures| captures.get(0)) {
        Some(suffix) => &path[..suffix.start()],
        None => path,
    };
    FilePathPosition {
        path: path.to_string(),
        line: (line > 0).then_some(line),
        column: column.filter(|column| *column > 0),
    }
}

pub fn format_file_path_position(position: &FilePathPosition) -> String {
    let Some(line) = position.line.filter(|line| *line != 0) else {
        return position.path.clone();
    };
    match position.column.filter(|column| *column != 0) {
        Some(column) => format!("{}:{line}:{column}", position.path),
        None => format!("{}:{line}", position.path),
    }
}

// ---------------------------------------------------------------------------
// upstream/apps/web/src/filePathDisplay.ts
// ---------------------------------------------------------------------------

fn normalize_path_separators(path: &str) -> String {
    path.replace('\\', "/")
}

/// `path.replace(/[\\/]+$/, "")`
fn trim_trailing_path_separators(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
}

/// `path.replace(/^\.\/+/, "").replace(/^\/+/, "")`
fn strip_relative_prefixes(path: &str) -> &str {
    let path = match path.strip_prefix("./") {
        Some(rest) => rest.trim_start_matches('/'),
        None => path,
    };
    path.trim_start_matches('/')
}

/// `value.slice(utf16_units)`: JS lengths count UTF-16 code units, and the
/// lower-cased compare string can differ in length from the original.
fn slice_from_utf16(value: &str, utf16_units: usize) -> &str {
    let mut seen = 0;
    for (index, character) in value.char_indices() {
        if seen >= utf16_units {
            return &value[index..];
        }
        seen += character.len_utf16();
    }
    ""
}

pub fn format_workspace_relative_path(
    path_with_position: &str,
    workspace_root: Option<&str>,
) -> String {
    let position = split_file_path_position(path_with_position, "");
    let normalized_path =
        strip_slash_prefixed_windows_drive(&normalize_path_separators(&position.path)).to_string();

    let mut display_path = normalized_path.clone();
    if let Some(workspace_root) = workspace_root.filter(|root| !root.is_empty()) {
        let normalized_workspace_root = strip_slash_prefixed_windows_drive(
            &normalize_path_separators(trim_trailing_path_separators(workspace_root)),
        )
        .to_string();
        let workspace_label = file_basename(&normalized_workspace_root);
        let case_insensitive =
            is_windows_absolute_path(strip_slash_prefixed_windows_drive(workspace_root));
        let path_for_compare = if case_insensitive {
            normalized_path.to_lowercase()
        } else {
            normalized_path.clone()
        };
        let workspace_for_compare = if case_insensitive {
            normalized_workspace_root.to_lowercase()
        } else {
            normalized_workspace_root.clone()
        };
        let workspace_with_separator = format!("{workspace_for_compare}/");
        let workspace_label_with_separator = format!(
            "{}/",
            if case_insensitive {
                workspace_label.to_lowercase()
            } else {
                workspace_label.to_string()
            }
        );

        if path_for_compare == workspace_for_compare {
            display_path = workspace_label.to_string();
        } else if path_for_compare.starts_with(&workspace_with_separator) {
            let relative_suffix = slice_from_utf16(
                &normalized_path,
                normalized_workspace_root.encode_utf16().count() + 1,
            );
            display_path = format!("{workspace_label}/{relative_suffix}");
        } else if !normalized_path.starts_with('/') {
            let relative_path = strip_relative_prefixes(&normalized_path);
            display_path = if path_for_compare.starts_with(&workspace_label_with_separator) {
                normalized_path.clone()
            } else {
                format!("{workspace_label}/{relative_path}")
            };
        }
    }

    format_file_path_position(&FilePathPosition {
        path: display_path,
        ..position
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // filePathDisplay.test.ts

    #[test]
    fn formats_absolute_workspace_paths_from_the_workspace_root() {
        assert_eq!(
            format_workspace_relative_path(
                "C:/Users/mike/dev-stuff/t3code/apps/web/src/session-logic.ts:501",
                Some("C:/Users/mike/dev-stuff/t3code"),
            ),
            "t3code/apps/web/src/session-logic.ts:501"
        );
    }

    #[test]
    fn prefixes_relative_paths_with_the_workspace_root_label() {
        assert_eq!(
            format_workspace_relative_path(
                "apps/web/src/session-logic.ts:501",
                Some("C:/Users/mike/dev-stuff/t3code"),
            ),
            "t3code/apps/web/src/session-logic.ts:501"
        );
    }

    #[test]
    fn keeps_paths_already_rooted_at_the_workspace_label_stable() {
        assert_eq!(
            format_workspace_relative_path(
                "t3code/apps/web/src/session-logic.ts:501",
                Some("C:/Users/mike/dev-stuff/t3code"),
            ),
            "t3code/apps/web/src/session-logic.ts:501"
        );
    }

    #[test]
    fn preserves_columns_when_present() {
        assert_eq!(
            format_workspace_relative_path(
                "/C:/Users/mike/dev-stuff/t3code/apps/web/src/session-logic.ts:501:9",
                Some("C:/Users/mike/dev-stuff/t3code"),
            ),
            "t3code/apps/web/src/session-logic.ts:501:9"
        );
    }

    #[test]
    fn keeps_double_slash_posix_paths_case_sensitive() {
        assert_eq!(
            format_workspace_relative_path("//tmp/project/probe.txt", Some("//tmp/Project")),
            "//tmp/project/probe.txt"
        );
    }

    // path.test.ts (cases for the helpers ported here)

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
    fn file_basename_labels_paths() {
        for (path, basename) in [
            ("/tmp/favicons/", "favicons"),
            ("C:\\Users\\kelchm\\.claude\\", ".claude"),
            ("/tmp/", "tmp"),
            ("AGENTS.md", "AGENTS.md"),
            ("/", "/"),
        ] {
            assert_eq!(file_basename(path), basename, "{path}");
        }
    }

    // fileLinks.test.ts

    #[test]
    fn splits_file_path_positions() {
        let position = |path: &str, line: Option<u64>, column: Option<u64>| FilePathPosition {
            path: path.to_string(),
            line,
            column,
        };
        for (path, hash, expected) in [
            ("src/main.ts", "", position("src/main.ts", None, None)),
            (
                "src/main.ts:12",
                "",
                position("src/main.ts", Some(12), None),
            ),
            (
                "src/main.ts:12:5",
                "",
                position("src/main.ts", Some(12), Some(5)),
            ),
            (
                "src/main.ts",
                "#L18C2",
                position("src/main.ts", Some(18), Some(2)),
            ),
            (
                "src/main.ts:3",
                "#L18C2",
                position("src/main.ts", Some(3), None),
            ),
            ("src/main.ts:0", "", position("src/main.ts", None, None)),
            (
                "src/main.ts",
                "#section",
                position("src/main.ts", None, None),
            ),
        ] {
            assert_eq!(
                split_file_path_position(path, hash),
                expected,
                "{path}{hash}"
            );
        }
    }
}
