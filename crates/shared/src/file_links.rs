//! Port of upstream/packages/shared/src/fileLinks.ts

use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

use regex::Regex;

use crate::path::{
    file_basename, is_absolute_path, is_windows_absolute_path, strip_slash_prefixed_windows_drive,
};

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

/// `/:(\d+)(?::(\d+))?$/`
fn position_suffix_capture() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex(&CELL, r":([0-9]+)(?::([0-9]+))?$")
}

/// `/^#L(\d+)(?:C(\d+))?$/i`
fn position_hash_capture() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    regex(&CELL, r"^#[lL]([0-9]+)(?:[cC]([0-9]+))?$")
}

/// Turns a `file:` URL into a host path, still percent-encoded so callers that
/// decode every destination in one place do not decode file URLs twice. A
/// non-localhost authority becomes a UNC share.
pub fn parse_file_url_href(href: &str) -> Option<(String, String)> {
    let parsed = url::Url::parse(href).ok()?;
    if !parsed.scheme().eq_ignore_ascii_case("file") {
        return None;
    }

    let hostname = parsed.host_str().unwrap_or("");
    let unc_hostname = if hostname.eq_ignore_ascii_case("localhost") {
        ""
    } else {
        hostname
    };
    let pathname = parsed.path();
    let path = if unc_hostname.is_empty() {
        pathname.to_owned()
    } else {
        format!("\\\\{unc_hostname}{}", pathname.replace('/', "\\"))
    };
    if path.is_empty() {
        return None;
    }
    // WHATWG `URL.hash` is "" for an empty fragment.
    let hash = match parsed.fragment() {
        Some(fragment) if !fragment.is_empty() => format!("#{fragment}"),
        _ => String::new(),
    };
    Some((strip_slash_prefixed_windows_drive(&path).to_owned(), hash))
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilePathPosition {
    pub path: String,
    pub line: Option<u64>,
    pub column: Option<u64>,
}

/// `Number.parseInt(digits, 10)`; absurdly long digit runs saturate (JS would
/// lose precision but stay positive).
fn parse_position_number(digits: &str) -> u64 {
    digits.parse().unwrap_or(u64::MAX)
}

pub fn split_file_path_position(path: &str, hash: &str) -> FilePathPosition {
    let suffix_match = position_suffix_capture().captures(path);
    let captures = suffix_match.or_else(|| position_hash_capture().captures(hash));
    let Some(captures) = captures else {
        return FilePathPosition {
            path: path.to_owned(),
            line: None,
            column: None,
        };
    };
    let Some(line) = captures.get(1) else {
        return FilePathPosition {
            path: path.to_owned(),
            line: None,
            column: None,
        };
    };

    let line = parse_position_number(line.as_str());
    let column = captures
        .get(2)
        .map(|column| parse_position_number(column.as_str()));
    // Only a suffix match shortens the path.
    let path = if position_suffix_capture().is_match(path) {
        let whole = captures.get(0).expect("group 0");
        path[..whole.start()].to_owned()
    } else {
        path.to_owned()
    };
    FilePathPosition {
        path,
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

fn is_windows_path_style(value: &str) -> bool {
    static CELL: OnceLock<Regex> = OnceLock::new();
    is_windows_absolute_path(value) || regex(&CELL, r"[A-Za-z]:\\").is_match(value)
}

fn join_path(base: &str, next: &str, separator: char) -> String {
    let clean_base = base.trim_end_matches(['/', '\\']);
    if separator == '\\' {
        format!("{clean_base}\\{}", next.replace('/', "\\"))
    } else {
        format!("{clean_base}/{}", next.trim_start_matches('/'))
    }
}

fn infer_home_from_cwd(cwd: &str) -> Option<String> {
    static POSIX_USER: OnceLock<Regex> = OnceLock::new();
    static POSIX_HOME: OnceLock<Regex> = OnceLock::new();
    static WINDOWS_USER: OnceLock<Regex> = OnceLock::new();
    if let Some(found) = regex(&POSIX_USER, r"^/Users/[^/]+").find(cwd) {
        return Some(found.as_str().to_owned());
    }
    if let Some(found) = regex(&POSIX_HOME, r"^/home/[^/]+").find(cwd) {
        return Some(found.as_str().to_owned());
    }
    regex(&WINDOWS_USER, r"^[A-Za-z]:[\\/]Users[\\/][^\\/]+")
        .find(cwd)
        .map(|found| found.as_str().to_owned())
}

pub fn resolve_path_link_target(raw_path: &str, cwd: &str) -> String {
    let position = split_file_path_position(raw_path, "");
    let path = position.path.as_str();

    let mut resolved_path = path.to_owned();
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = infer_home_from_cwd(cwd) {
            let separator = if is_windows_path_style(&home) {
                '\\'
            } else {
                '/'
            };
            resolved_path = join_path(&home, rest, separator);
        }
    } else if !is_absolute_path(path) {
        let separator = if is_windows_path_style(cwd) {
            '\\'
        } else {
            '/'
        };
        resolved_path = join_path(cwd, path, separator);
    }

    format_file_path_position(&FilePathPosition {
        path: resolved_path,
        ..position
    })
}

fn path_parent_segments(path: &str) -> Vec<String> {
    let normalized = path.replace('\\', "/");
    let mut segments: Vec<String> = normalized
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect();
    segments.pop();
    segments
}

pub fn build_file_link_parent_suffix_by_path(file_paths: &[String]) -> HashMap<String, String> {
    // Insertion-ordered groups: the result is a map, so order only matters for
    // determinism; BTreeSet keeps each group's paths stable.
    let mut groups: Vec<(String, BTreeSet<String>)> = Vec::new();
    for file_path in file_paths {
        let normalized_path = file_path.replace('\\', "/");
        let Some(basename) = normalized_path
            .split('/')
            .rfind(|segment| !segment.is_empty())
        else {
            continue;
        };
        match groups.iter_mut().find(|(name, _)| name == basename) {
            Some((_, group)) => {
                group.insert(normalized_path);
            }
            None => groups.push((basename.to_owned(), BTreeSet::from([normalized_path]))),
        }
    }

    let mut suffix_by_path: HashMap<String, String> = HashMap::new();
    for (_, group) in &groups {
        let unique_paths: Vec<&String> = group.iter().collect();
        if unique_paths.len() < 2 {
            continue;
        }

        let parent_segments: Vec<Vec<String>> = unique_paths
            .iter()
            .map(|path| path_parent_segments(path))
            .collect();
        let tail = |segments: &[String], depth: usize| -> String {
            let start = segments.len().saturating_sub(depth);
            segments[start..].join("/")
        };

        let mut min_unique_depth: Vec<usize> = Vec::with_capacity(unique_paths.len());
        for (index, segments) in parent_segments.iter().enumerate() {
            let mut resolved_depth = segments.len();
            for depth in 1..=segments.len() {
                let candidate = tail(segments, depth);
                let collision =
                    parent_segments
                        .iter()
                        .enumerate()
                        .any(|(other, other_segments)| {
                            other != index && tail(other_segments, depth) == candidate
                        });
                if !collision {
                    resolved_depth = depth;
                    break;
                }
            }
            min_unique_depth.push(resolved_depth);
        }

        for (index, file_path) in unique_paths.iter().enumerate() {
            let segments = &parent_segments[index];
            if segments.is_empty() {
                continue;
            }
            let suffix_depth = segments.len().min(min_unique_depth[index].max(2));
            suffix_by_path.insert((*file_path).clone(), tail(segments, suffix_depth));
        }
    }

    suffix_by_path
}

/// Visible text of a file chip, shared by Markdown rendering and thread search.
pub fn file_link_label(
    file: &FilePathPosition,
    parent_suffix_by_path: &HashMap<String, String>,
) -> String {
    let suffix = parent_suffix_by_path.get(&file.path.replace('\\', "/"));
    let line_label = file.line.filter(|line| *line != 0).map(|line| {
        match file.column.filter(|column| *column != 0) {
            Some(column) => format!("L{line}:C{column}"),
            None => format!("L{line}"),
        }
    });
    [Some(file_basename(&file.path)), suffix.cloned(), line_label]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_file_url_hrefs() {
        let cases = [
            (
                "file:///Users/julius/project/src/main.ts#L42",
                "/Users/julius/project/src/main.ts",
                "#L42",
            ),
            (
                "file:///D:/Programme/t3code/OpenInPicker.tsx#L69",
                "D:/Programme/t3code/OpenInPicker.tsx",
                "#L69",
            ),
            (
                "file://server/share/workspace-image.svg",
                "\\\\server\\share\\workspace-image.svg",
                "",
            ),
            ("file://localhost/home/me/notes.md", "/home/me/notes.md", ""),
        ];
        for (href, path, hash) in cases {
            assert_eq!(
                parse_file_url_href(href),
                Some((path.to_owned(), hash.to_owned())),
                "{href}"
            );
        }
    }

    #[test]
    fn keeps_percent_encoding_so_the_caller_decodes_once() {
        assert_eq!(
            parse_file_url_href("file:///Users/julius/project/file%2520name.md")
                .map(|(path, _)| path),
            Some("/Users/julius/project/file%2520name.md".to_owned())
        );
        assert_eq!(
            parse_file_url_href("file:///c%3A/Users/x/shot.png").map(|(path, _)| path),
            Some("/c%3A/Users/x/shot.png".to_owned())
        );
    }

    #[test]
    fn rejects_non_file_hrefs() {
        for href in ["https://example.com/a.ts", "file://%", "/Users/julius/a.ts"] {
            assert_eq!(parse_file_url_href(href), None, "{href}");
        }
    }

    #[test]
    fn splits_file_path_positions() {
        let cases: &[(&str, &str, FilePathPosition)] = &[
            (
                "src/main.ts",
                "",
                FilePathPosition {
                    path: "src/main.ts".into(),
                    line: None,
                    column: None,
                },
            ),
            (
                "src/main.ts:12",
                "",
                FilePathPosition {
                    path: "src/main.ts".into(),
                    line: Some(12),
                    column: None,
                },
            ),
            (
                "src/main.ts:12:5",
                "",
                FilePathPosition {
                    path: "src/main.ts".into(),
                    line: Some(12),
                    column: Some(5),
                },
            ),
            (
                "src/main.ts",
                "#L18C2",
                FilePathPosition {
                    path: "src/main.ts".into(),
                    line: Some(18),
                    column: Some(2),
                },
            ),
            (
                "src/main.ts:3",
                "#L18C2",
                FilePathPosition {
                    path: "src/main.ts".into(),
                    line: Some(3),
                    column: None,
                },
            ),
            (
                "src/main.ts:0",
                "",
                FilePathPosition {
                    path: "src/main.ts".into(),
                    line: None,
                    column: None,
                },
            ),
            (
                "src/main.ts",
                "#section",
                FilePathPosition {
                    path: "src/main.ts".into(),
                    line: None,
                    column: None,
                },
            ),
        ];
        for (path, hash, expected) in cases {
            assert_eq!(
                split_file_path_position(path, hash),
                *expected,
                "{path}{hash}"
            );
        }
    }

    #[test]
    fn resolves_relative_paths_against_cwd() {
        assert_eq!(
            resolve_path_link_target(
                "src/components/ThreadTerminalDrawer.tsx:42:7",
                "/Users/julius/project"
            ),
            "/Users/julius/project/src/components/ThreadTerminalDrawer.tsx:42:7"
        );
    }

    #[test]
    fn keeps_absolute_paths_unchanged() {
        assert_eq!(
            resolve_path_link_target(
                "/Users/julius/project/src/main.ts:12",
                "/Users/julius/project"
            ),
            "/Users/julius/project/src/main.ts:12"
        );
    }

    #[test]
    fn keeps_windows_absolute_paths_with_forward_slashes_unchanged() {
        assert_eq!(
            resolve_path_link_target(
                "C:/Users/julius/project/src/main.ts:12",
                "C:\\Users\\julius\\project"
            ),
            "C:/Users/julius/project/src/main.ts:12"
        );
    }

    #[test]
    fn resolves_home_paths_against_windows_cwds() {
        let cases = [
            (
                "C:\\Users\\julius\\project",
                "C:\\Users\\julius\\notes.md:3",
            ),
            ("C:/Users/julius/project", "C:/Users/julius\\notes.md:3"),
        ];
        for (cwd, expected) in cases {
            assert_eq!(resolve_path_link_target("~/notes.md:3", cwd), expected);
        }
    }
}
