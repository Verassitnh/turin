//! Port of upstream/packages/shared/src/fileLinks.ts

use crate::path::{is_absolute_path, is_windows_absolute_path, strip_slash_prefixed_windows_drive};

/// `path.match(/:(\d+)(?::(\d+))?$/)`
fn position_suffix_capture(path: &str) -> Option<(usize, &str, Option<&str>)> {
    // Find the last `:digits(:digits)?` run that ends the string.
    let mut cursor = path.len();
    let mut segments: Vec<(usize, &str)> = Vec::new();
    while segments.len() < 2 {
        let Some(colon) = path[..cursor].rfind(':') else {
            break;
        };
        let digits = &path[colon + 1..cursor];
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            break;
        }
        segments.push((colon, digits));
        cursor = colon;
    }
    let (second_colon, second) = segments.first().copied()?;
    let (first_colon, first) = match segments.get(1) {
        Some((colon, digits)) => (*colon, *digits),
        None => (second_colon, second),
    };
    if segments.len() == 1 {
        Some((second_colon, second, None))
    } else {
        // `:digits:digits` — but the first segment must be the complete tail
        // after the second-to-last colon (no extra colons between).
        Some((first_colon, first, Some(second)))
    }
}

/// `hash.match(/^#L(\d+)(?:C(\d+))?$/i)`
fn position_hash_capture(hash: &str) -> Option<(String, Option<String>)> {
    let rest = hash
        .strip_prefix("#L")
        .or_else(|| hash.strip_prefix("#l"))?;
    let Some((line, column)) = rest.split_once(['C', 'c']) else {
        if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
            return Some((rest.to_owned(), None));
        }
        return None;
    };
    let line_valid = !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit());
    let column_valid = !column.is_empty() && column.bytes().all(|b| b.is_ascii_digit());
    if line_valid && column_valid {
        Some((line.to_owned(), Some(column.to_owned())))
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilePathPosition {
    pub path: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

/// Turns a `file:` URL into a host path, still percent-encoded so callers that
/// decode every destination in one place do not decode file URLs twice. A
/// non-localhost authority becomes a UNC share.
pub fn parse_file_url_href(href: &str) -> Option<(String, String)> {
    let Ok(parsed) = url::Url::parse(href) else {
        return None;
    };
    if parsed.scheme().to_ascii_lowercase() != "file" {
        return None;
    }

    let hostname = parsed.hostname();
    let unc_hostname = if hostname.eq_ignore_ascii_case("localhost") {
        ""
    } else {
        hostname
    };
    let pathname = parsed.path();
    let path = if !unc_hostname.is_empty() {
        format!("\\\\{}{}", unc_hostname, pathname.replace('/', "\\"))
    } else {
        pathname.to_owned()
    };
    if path.is_empty() {
        return None;
    }
    let hash = parsed.fragment().map(|fragment| format!("#{fragment}")).unwrap_or_default();
    Some((strip_slash_prefixed_windows_drive(&path).to_owned(), hash))
}

pub fn split_file_path_position(path: &str, hash: &str) -> FilePathPosition {
    let suffix = position_suffix_capture(path);
    let capture = match suffix {
        Some((_colon, line, column)) => Some((line.to_owned(), column.map(str::to_owned))),
        None => position_hash_capture(hash).map(|(line, column)| (line, column)),
    };
    let Some((line, column)) = capture else {
        return FilePathPosition { path: path.to_owned(), line: None, column: None };
    };

    let line: Option<u32> = line.parse().ok();
    let column: Option<u32> = column.as_deref().and_then(|value| value.parse().ok());
    FilePathPosition {
        path: match suffix {
            Some((colon, _, _)) => path[..colon].to_owned(),
            None => path.to_owned(),
        },
        line: line.filter(|line| *line > 0),
        column: column.filter(|column| *column > 0),
    }
}

pub fn format_file_path_position(position: &FilePathPosition) -> String {
    let Some(line) = position.line else {
        return position.path.clone();
    };
    match position.column {
        Some(column) => format!("{}:{line}:{column}", position.path),
        None => format!("{}:{line}", position.path),
    }
}

fn is_windows_path_style(value: &str) -> bool {
    is_windows_absolute_path(value) || {
        // /[A-Za-z]:\\/
        let mut chars = value.chars();
        matches!((chars.next(), chars.next(), chars.next()),
            (Some(drive), Some(':'), Some('\\')) if drive.is_ascii_alphabetic())
    }
}

fn join_path(base: &str, next: &str, separator: char) -> String {
    let clean_base = base.trim_end_matches(['/', '\\']);
    if separator == '\\' {
        format!("{}\\{}", clean_base, next.replace('/', "\\"))
    } else {
        format!("{}/{}", clean_base, next.trim_start_matches('/'))
    }
}

fn infer_home_from_cwd(cwd: &str) -> Option<String> {
    // /^\/Users\/([^/]+)/
    if let Some(rest) = cwd.strip_prefix("/Users/") {
        if let Some(user) = rest.split('/').next() {
            if !user.is_empty() {
                return Some(format!("/Users/{user}"));
            }
        }
    }

    // /^\/home\/([^/]+)/
    if let Some(rest) = cwd.strip_prefix("/home/") {
        if let Some(user) = rest.split('/').next() {
            if !user.is_empty() {
                return Some(format!("/home/{user}"));
            }
        }
    }

    // /^([A-Za-z]:[\\/]Users[\\/][^\\/]+)/
    let mut chars = cwd.chars();
    if matches!((chars.next(), chars.next()), (Some(drive), Some(':')) if drive.is_ascii_alphabetic()) {
        let rest: &str = &cwd[2..];
        let rest = rest.strip_prefix(['/', '\\'])?;
        let rest = rest.strip_prefix("Users")?;
        let rest = rest.strip_prefix(['/', '\\'])?;
        let user = rest.split(['/', '\\']).next()?;
        if !user.is_empty() {
            return Some(format!("{}:\\Users\\{}", &cwd[..1].to_uppercase(), user));
        }
    }

    None
}

pub fn resolve_path_link_target(raw_path: &str, cwd: &str) -> String {
    let position = split_file_path_position(raw_path, "");
    let path = position.path.as_str();

    let mut resolved_path = path.to_owned();
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = infer_home_from_cwd(cwd) {
            let separator = if is_windows_path_style(&home) { '\\' } else { '/' };
            resolved_path = join_path(&home, rest, separator);
        }
    } else if !is_absolute_path(path) {
        let separator = if is_windows_path_style(cwd) { '\\' } else { '/' };
        resolved_path = join_path(cwd, path, separator);
    }

    format_file_path_position(&FilePathPosition { path: resolved_path, ..position })
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
            assert_eq!(parse_file_url_href(href), Some((path.to_owned(), hash.to_owned())), "{href}");
        }
    }

    #[test]
    fn keeps_percent_encoding_so_the_caller_decodes_once() {
        assert_eq!(
            parse_file_url_href("file:///Users/julius/project/file%2520name.md").map(|(path, _)| path),
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
            ("src/main.ts", "", FilePathPosition { path: "src/main.ts".into(), line: None, column: None }),
            ("src/main.ts:12", "", FilePathPosition { path: "src/main.ts".into(), line: Some(12), column: None }),
            ("src/main.ts:12:5", "", FilePathPosition { path: "src/main.ts".into(), line: Some(12), column: Some(5) }),
            ("src/main.ts", "#L18C2", FilePathPosition { path: "src/main.ts".into(), line: Some(18), column: Some(2) }),
            ("src/main.ts:3", "#L18C2", FilePathPosition { path: "src/main.ts".into(), line: Some(3), column: None }),
            ("src/main.ts:0", "", FilePathPosition { path: "src/main.ts".into(), line: None, column: None }),
            ("src/main.ts", "#section", FilePathPosition { path: "src/main.ts".into(), line: None, column: None }),
        ];
        for (path, hash, expected) in cases {
            assert_eq!(split_file_path_position(path, hash), *expected, "{path}{hash}");
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
            resolve_path_link_target("/Users/julius/project/src/main.ts:12", "/Users/julius/project"),
            "/Users/julius/project/src/main.ts:12"
        );
    }

    #[test]
    fn keeps_windows_absolute_paths_with_forward_slashes_unchanged() {
        assert_eq!(
            resolve_path_link_target("C:/Users/julius/project/src/main.ts:12", "C:\\Users\\julius\\project"),
            "C:/Users/julius/project/src/main.ts:12"
        );
    }

    #[test]
    fn resolves_home_paths_against_windows_cwds() {
        let cases = [
            ("C:\\Users\\julius\\project", "C:\\Users\\julius\\notes.md:3"),
            ("C:/Users/julius/project", "C:/Users\\julius\\notes.md:3"),
        ];
        for (cwd, expected) in cases {
            assert_eq!(resolve_path_link_target("~/notes.md:3", cwd), expected);
        }
    }
}
