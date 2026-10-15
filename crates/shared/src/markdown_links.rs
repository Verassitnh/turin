//! Port of upstream/packages/shared/src/markdownLinks.ts

use crate::file_links::{
    FilePathPosition, format_file_path_position, parse_file_url_href, resolve_path_link_target,
    split_file_path_position,
};
use crate::js_string::{decode_uri_component, is_js_whitespace_char};
use crate::path::{is_windows_absolute_path, strip_slash_prefixed_windows_drive};
use turin_contracts::base_schemas::js_trim;

// /^(~\/|\.{1,2}\/)/
fn is_relative_path_prefix(value: &str) -> bool {
    value.starts_with("~/") || value.starts_with("./") || value.starts_with("../")
}

fn looks_like_hostname(segment: &str, has_position: bool) -> bool {
    if segment.starts_with('.') {
        return false;
    }
    let lowered = segment.to_lowercase();
    if lowered == "localhost" {
        return true;
    }
    if is_numeric_dotted(segment) {
        return true;
    }
    let labels: Vec<&str> = lowered.split('.').collect();
    let Some(last_label) = labels.last().copied() else {
        return false;
    };
    if labels.len() < 2 {
        return false;
    }
    if GENERIC_HOSTNAME_TLDS.contains(&last_label) {
        return true;
    }
    !has_position && COUNTRY_HOSTNAME_TLDS.contains(&last_label)
}

// These allowlists avoid classifying dotted directories such as `conf.d/`
// or filenames such as `Makefile.in:12` as hosts.
const GENERIC_HOSTNAME_TLDS: &[&str] = &[
    "com", "net", "org", "io", "dev", "app", "ai", "co", "edu", "gov", "mil", "info", "biz", "xyz",
    "me", "tv", "cc", "gg", "chat", "cloud", "site", "online", "tech", "store", "link",
];

// Country codes also name file extensions. A :line suffix makes `.pl`
// and `.pt` files more likely than hostnames.
const COUNTRY_HOSTNAME_TLDS: &[&str] = &[
    "uk", "de", "fr", "nl", "se", "no", "fi", "dk", "pl", "ch", "at", "be", "es", "it", "pt", "eu",
    "us", "ca", "au", "nz", "jp", "kr", "cn", "br", "ru", "mx", "ie", "cz", "tr", "sg", "hk",
];

// `Name:digits` also matches `error:1`, `port:3000`, and `TODO:12`.
const EXTENSIONLESS_FILE_NAMES: &[&str] = &[
    "Makefile",
    "makefile",
    "GNUmakefile",
    "Dockerfile",
    "Containerfile",
    "Justfile",
    "justfile",
    "Rakefile",
    "Gemfile",
    "Procfile",
    "Brewfile",
    "Caddyfile",
    "Vagrantfile",
    "Jenkinsfile",
    "Podfile",
    "Fastfile",
    "BUILD",
    "WORKSPACE",
    "LICENSE",
    "LICENCE",
    "COPYING",
    "NOTICE",
    "AUTHORS",
    "CONTRIBUTORS",
    "CHANGELOG",
    "README",
    "CODEOWNERS",
];

// Standard OS and dev-container roots; deliberately excludes app-route-ish
// prefixes like /app/ or /chat/ so SPA routes never read as files.
const POSIX_FILE_ROOT_PREFIXES: &[&str] = &[
    "/Users/",
    "/home/",
    "/tmp/",
    "/var/",
    "/etc/",
    "/opt/",
    "/mnt/",
    "/Volumes/",
    "/private/",
    "/root/",
    "/usr/",
    "/bin/",
    "/sbin/",
    "/lib/",
    "/lib64/",
    "/srv/",
    "/dev/",
    "/proc/",
    "/sys/",
    "/run/",
    "/boot/",
    "/media/",
    "/workspace/",
    "/workspaces/",
];

// /^(?:[A-Za-z0-9._-]+(?: +[A-Za-z0-9._-]+)*\/)+[A-Za-z0-9._-]+(?: +[A-Za-z0-9._-]+)*(?::\d+){0,2}$/
static RELATIVE_FILE_PATH_PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

fn is_relative_file_path_shape(value: &str) -> bool {
    RELATIVE_FILE_PATH_PATTERN
        .get_or_init(|| {
            regex::Regex::new(
                r"^(?:[A-Za-z0-9._-]+(?: +[A-Za-z0-9._-]+)*/)+[A-Za-z0-9._-]+(?: +[A-Za-z0-9._-]+)*(?::\d+){0,2}$",
            )
            .expect("valid regex")
        })
        .is_match(value)
}

// /^[A-Za-z0-9._-]+(?: +[A-Za-z0-9._-]+)*\.[A-Za-z0-9_-]+(?::\d+){0,2}$/
static RELATIVE_FILE_NAME_PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

fn is_relative_file_name_shape(value: &str) -> bool {
    RELATIVE_FILE_NAME_PATTERN
        .get_or_init(|| {
            regex::Regex::new(
                r"^[A-Za-z0-9._-]+(?: +[A-Za-z0-9._-]+)*\.[A-Za-z0-9_-]+(?::\d+){0,2}$",
            )
            .expect("valid regex")
        })
        .is_match(value)
}

// /^[A-Za-z][A-Za-z0-9+.-]*:(.*)$/
fn external_scheme_rest(path: &str) -> Option<&str> {
    let mut chars = path.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    let mut index = first.len_utf8();
    for ch in chars {
        if ch == ':' {
            return Some(&path[index + 1..]);
        }
        if !(ch.is_ascii_alphanumeric() || ch == '+' || ch == '.' || ch == '-') {
            return None;
        }
        index += ch.len_utf8();
    }
    None
}

// /^\d+(?::\d+)?$/
fn is_position_only(value: &str) -> bool {
    let (number, second) = match value.split_once(':') {
        Some((number, second)) => (number, Some(second)),
        None => (value, None),
    };
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    digits(number) && second.is_none_or(digits)
}

// /\d\.\d[^.]*$/ — a final dot between digits marks a version or model id
// (`glm-5.3`, `Qwen2.5-Coder`), not an extension. `ls.1` and `libfoo.so.1`
// stay files.
fn has_version_suffix(value: &str) -> bool {
    let Some(dot) = value.rfind('.') else {
        return false;
    };
    let bytes = value.as_bytes();
    dot > 0
        && dot + 1 < bytes.len()
        && bytes[dot - 1].is_ascii_digit()
        && bytes[dot + 1].is_ascii_digit()
        && !bytes[dot + 2..].contains(&b'.')
}

// /^\d+(?:\.\d+)+$/
fn is_numeric_dotted(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    let mut saw_dot = false;
    let mut digit_run = 0usize;
    for ch in value.chars() {
        if ch.is_ascii_digit() {
            digit_run += 1;
        } else if ch == '.' {
            if digit_run == 0 {
                return false;
            }
            saw_dot = true;
            digit_run = 0;
        } else {
            return false;
        }
    }
    saw_dot && digit_run > 0
}

// /\.[A-Za-z0-9_-]+$/
fn has_file_extension(value: &str) -> bool {
    match value.rfind('.') {
        Some(dot) if dot > 0 || value.len() > 1 => {
            let extension = &value[dot + 1..];
            !extension.is_empty()
                && extension
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        }
        _ => false,
    }
}

// /:\d+(?::\d+)?$/
fn strip_position_suffix(value: &str) -> &str {
    let bytes = value.as_bytes();
    let mut cursor = bytes.len();
    for _ in 0..2 {
        let Some(colon) = bytes[..cursor].iter().rposition(|b| *b == b':') else {
            break;
        };
        let digits = &value[colon + 1..cursor];
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            break;
        }
        cursor = colon;
    }
    &value[..cursor]
}

/// Picks path-shaped inline code for the client's markdown file-link resolver.
/// It does not resolve paths or turn plain prose and fenced code into links.
pub fn inline_code_file_path_candidate(code_text: &str) -> Option<String> {
    let trimmed = js_trim(code_text);
    if trimmed.is_empty()
        || trimmed
            .chars()
            .any(|c| is_js_whitespace_char(c) || c == '`')
    {
        return None;
    }

    let candidate = if is_windows_absolute_path(trimmed) {
        trimmed.to_owned()
    } else {
        trimmed.replace('\\', "/")
    };
    let has_position = ends_with_position_suffix(&candidate);
    if !has_position && !candidate.chars().any(|c| c == '/' || c == '\\') {
        return None;
    }

    let has_explicit_path_shape = is_relative_path_prefix(&candidate)
        || candidate.starts_with('/')
        || is_windows_absolute_path(&candidate);
    if !has_explicit_path_shape {
        let without_position = strip_position_suffix(&candidate);
        let first_segment = without_position
            .split(['/', '\\'])
            .next()
            .unwrap_or(without_position);
        if looks_like_hostname(first_segment, has_position) {
            return None;
        }
        let basename = without_position
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("");
        if has_version_suffix(basename) {
            return None;
        }
        if !has_position && !has_file_extension(basename) {
            return None;
        }
    }
    Some(candidate)
}

// /:\d+(?::\d+)?$/
fn ends_with_position_suffix(value: &str) -> bool {
    let stripped = strip_position_suffix(value);
    stripped.len() < value.len()
}

pub fn safe_decode_uri_component(value: &str) -> String {
    decode_uri_component(value).unwrap_or_else(|_| value.to_owned())
}

pub fn normalize_markdown_link_destination(value: &str) -> &str {
    let trimmed = js_trim(value);
    if trimmed.starts_with('<') && trimmed.ends_with('>') && trimmed.len() >= 2 {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    }
}

pub fn split_markdown_link_search_and_hash(value: &str) -> (String, String) {
    let hash_index = value.find('#');
    let path_with_search = match hash_index {
        Some(index) => &value[..index],
        None => value,
    };
    let hash = match hash_index {
        Some(index) => value[index..].to_owned(),
        None => String::new(),
    };
    let query_index = path_with_search.find('?');
    let path = match query_index {
        Some(index) => path_with_search[..index].to_owned(),
        None => path_with_search.to_owned(),
    };
    (path, hash)
}

/// Keeps filename and destination-path labels compact without discarding prose.
pub fn is_markdown_file_link_label(label: &str, href: &str) -> bool {
    let Some(destination) = parse_markdown_file_link(href) else {
        return false;
    };
    let normalize = |path: &str| -> String {
        let slashed = path.replace('\\', "/");
        let stripped = slashed.strip_prefix("./").unwrap_or(&slashed);
        stripped.trim_end_matches('/').to_owned()
    };
    let label_position = split_file_path_position(js_trim(label), "");
    if (label_position.line.is_some() && label_position.line != destination.line)
        || (label_position.column.is_some() && label_position.column != destination.column)
    {
        return false;
    }
    let mut label_path = normalize(&label_position.path);
    let mut destination_path = normalize(&destination.path);
    if label_path.is_empty() {
        return true;
    }
    if is_windows_absolute_path(&destination.path) {
        label_path = label_path.to_lowercase();
        destination_path = destination_path.to_lowercase();
    }
    destination_path == label_path || destination_path.ends_with(&format!("/{label_path}"))
}

pub fn is_relative_file_path(path: &str) -> bool {
    is_relative_path_prefix(path) || (!path.starts_with('/') && !is_windows_absolute_path(path))
}

fn looks_like_posix_filesystem_path(path: &str) -> bool {
    if !path.starts_with('/') {
        return false;
    }
    if POSIX_FILE_ROOT_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        return true;
    }
    if ends_with_position_suffix(path) {
        return true;
    }
    let basename = &path[path.rfind('/').map(|index| index + 1).unwrap_or(0)..];
    EXTENSIONLESS_FILE_NAMES.contains(&basename) || has_file_extension(basename)
}

/// Decides whether a decoded link destination is a file path rather than a
/// route or prose. Only a `:line` suffix the author wrote counts as evidence; a
/// `#L` anchor never turns `/chat/settings` into a file.
fn looks_like_file_path(path: &str, authored_path: &str) -> bool {
    if is_windows_absolute_path(path) || is_relative_path_prefix(path) {
        return true;
    }
    if path.starts_with('/') {
        return looks_like_posix_filesystem_path(authored_path);
    }
    if EXTENSIONLESS_FILE_NAMES.contains(&path) {
        return true;
    }
    is_relative_file_path_shape(authored_path) || is_relative_file_name_shape(path)
}

fn has_external_scheme(path: &str) -> bool {
    if is_windows_absolute_path(path) {
        return false;
    }
    let Some(rest) = external_scheme_rest(path) else {
        return false;
    };
    if rest.starts_with("//") {
        return true;
    }
    !is_position_only(rest)
}

pub fn parse_markdown_file_link(href: &str) -> Option<FilePathPosition> {
    let normalized = normalize_markdown_link_destination(href);
    if normalized.is_empty() || normalized.starts_with('#') || normalized.starts_with("//") {
        return None;
    }

    let (path, hash) = if normalized.to_lowercase().starts_with("file:") {
        parse_file_url_href(normalized)
            .unwrap_or_else(|| split_markdown_link_search_and_hash(normalized))
    } else {
        split_markdown_link_search_and_hash(normalized)
    };
    // A percent-encoded drive colon (`/c%3A/`) only becomes strippable once decoded.
    let decoded_path = safe_decode_uri_component(js_trim(&path));
    let path = strip_slash_prefixed_windows_drive(&decoded_path);
    let hash = safe_decode_uri_component(js_trim(&hash));
    if path.is_empty() || has_external_scheme(path) {
        return None;
    }

    let position = split_file_path_position(path, &hash);
    if looks_like_file_path(&position.path, path) {
        Some(position)
    } else {
        None
    }
}

// /(```[\s\S]*?(?:```|$))/
fn split_fenced_code_segments(markdown: &str) -> Vec<&str> {
    static FENCE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let fence =
        FENCE.get_or_init(|| regex::Regex::new(r"```[\s\S]*?(?:```|$)").expect("valid regex"));
    let mut segments: Vec<&str> = Vec::new();
    let mut cursor = 0usize;
    for found in fence.find_iter(markdown) {
        if found.start() > cursor {
            segments.push(&markdown[cursor..found.start()]);
        } else {
            // A zero-length outside segment keeps the even/odd rhythm JS
            // `split` produces when the string starts with a fence.
            segments.push("");
        }
        segments.push(found.as_str());
        cursor = found.end();
    }
    if cursor < markdown.len() || segments.is_empty() {
        segments.push(&markdown[cursor..]);
    }
    segments
}

// /`([^`\n]+)`/g
static INLINE_CODE_SPAN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

pub fn extract_inline_code_spans(text: &str) -> Vec<String> {
    let pattern =
        INLINE_CODE_SPAN.get_or_init(|| regex::Regex::new(r"`([^`\n]+)`").expect("valid regex"));
    let mut spans: Vec<String> = Vec::new();
    let segments = split_fenced_code_segments(text);
    for (index, segment) in segments.iter().enumerate() {
        if index % 2 != 0 {
            continue;
        }
        for captures in pattern.captures_iter(segment) {
            let span = js_trim(captures.get(1).map(|m| m.as_str()).unwrap_or_default());
            if !span.is_empty() {
                spans.push(span.to_owned());
            }
        }
    }
    spans
}

/// Reuses delimiter searches when successive candidates fall in the same run.
/// Upstream drives a `g`-flagged single-char regex with `lastIndex`; this port
/// scans for the next matching char at or after `start` (equivalent for the
/// char classes used) and caches the run the same way.
fn markdown_run_end_finder<'a>(
    markdown: &'a str,
    predicate: fn(char) -> bool,
) -> impl FnMut(usize) -> usize + 'a {
    let mut run_start: usize = usize::MAX;
    let mut run_end: usize = usize::MAX;
    move |start: usize| {
        if start >= run_start && start <= run_end {
            return run_end;
        }
        run_start = start;
        run_end = markdown[start..]
            .char_indices()
            .find(|(_, ch)| predicate(*ch))
            .map(|(offset, _)| start + offset)
            .unwrap_or(markdown.len());
        run_end
    }
}

fn is_not_whitespace(ch: char) -> bool {
    !is_js_whitespace_char(ch)
}
fn is_quote(ch: char) -> bool {
    ch == '"' || ch == '\''
}
fn is_angle_end(ch: char) -> bool {
    ch == '>' || ch == '\n'
}
fn is_bare_end(ch: char) -> bool {
    is_js_whitespace_char(ch) || ch == ')'
}

fn markdown_link_tail_scanner(markdown: &str) -> impl FnMut(usize) -> Option<usize> + '_ {
    let mut whitespace_end = markdown_run_end_finder(markdown, is_not_whitespace);
    let mut title_end = markdown_run_end_finder(markdown, is_quote);
    let mut title_whitespace_end = markdown_run_end_finder(markdown, is_not_whitespace);
    move |start: usize| {
        let end = whitespace_end(start);
        if char_at(markdown, end) == Some(')') {
            return Some(end + 1);
        }
        if end == start
            || (char_at(markdown, end) != Some('"') && char_at(markdown, end) != Some('\''))
        {
            return None;
        }
        let quote_end = title_end(end + 1);
        if quote_end == markdown.len() {
            return None;
        }
        let close = title_whitespace_end(quote_end + 1);
        if char_at(markdown, close) == Some(')') {
            Some(close + 1)
        } else {
            None
        }
    }
}

fn char_at(markdown: &str, byte_index: usize) -> Option<char> {
    if byte_index >= markdown.len() {
        return None;
    }
    // Callers only inspect delimiter bytes at ASCII boundaries.
    markdown[byte_index..].chars().next()
}

struct LinkDestinationSpan {
    href_start: usize,
    href_end: usize,
    end: usize,
}

fn markdown_link_destination_scanner(
    markdown: &str,
) -> impl FnMut(usize) -> Option<LinkDestinationSpan> + '_ {
    let mut whitespace_end = markdown_run_end_finder(markdown, is_not_whitespace);
    let mut angle_end = markdown_run_end_finder(markdown, is_angle_end);
    let mut bare_end = markdown_run_end_finder(markdown, is_bare_end);
    // Separate caches keep each branch's searches moving forward even when an
    // angle destination fails and falls back to an ordinary destination.
    let mut angle_tail = markdown_link_tail_scanner(markdown);
    let mut bare_tail = markdown_link_tail_scanner(markdown);
    move |start: usize| {
        if char_at(markdown, start) != Some('(') {
            return None;
        }
        let href_start = whitespace_end(start + 1);
        if char_at(markdown, href_start) == Some('<') {
            let href_end = angle_end(href_start + 1);
            if href_end > href_start + 1
                && char_at(markdown, href_end) == Some('>')
                && let Some(end) = angle_tail(href_end + 1)
            {
                return Some(LinkDestinationSpan {
                    href_start: href_start + 1,
                    href_end,
                    end,
                });
            }
        }
        let href_end = bare_end(href_start);
        if href_end == href_start {
            return None;
        }
        let end = bare_tail(href_end)?;
        Some(LinkDestinationSpan {
            href_start,
            href_end,
            end,
        })
    }
}

pub fn extract_markdown_link_hrefs(markdown: &str) -> Vec<String> {
    let mut hrefs: Vec<String> = Vec::new();
    let mut scan_destination = markdown_link_destination_scanner(markdown);
    let mut cursor = 0usize;
    while cursor < markdown.len() {
        let Some(label_start) = markdown[cursor..].find('[').map(|offset| cursor + offset) else {
            break;
        };
        let Some(label_end) = markdown[label_start + 1..]
            .find(']')
            .map(|offset| label_start + 1 + offset)
        else {
            break;
        };

        // Nested opening brackets share this closing bracket. On failure, skip the
        // whole label instead of rescanning its suffix from each opening bracket.
        cursor = label_end + 1;
        let Some(destination) = scan_destination(cursor) else {
            continue;
        };
        cursor = destination.end;
        let href = js_trim(&markdown[destination.href_start..destination.href_end]);
        if !href.is_empty() {
            hrefs.push(href.to_owned());
        }
    }
    hrefs
}

/// `baseDir` anchors relative links; it defaults to the workspace root and is
/// the file's own directory when rendering a markdown file. `cwd` stays the
/// workspace root so the result still knows whether the target is inside it.
pub fn resolve_markdown_file_link_target(
    href: Option<&str>,
    cwd: Option<&str>,
    base_dir: Option<&str>,
) -> Option<String> {
    let href = href?;
    let target = parse_markdown_file_link(href)?;

    let path_with_position = format_file_path_position(&target);
    if !is_relative_file_path(&path_with_position) {
        return Some(path_with_position);
    }
    let base_dir = base_dir.or(cwd).filter(|dir| !dir.is_empty())?;
    Some(resolve_path_link_target(&path_with_position, base_dir))
}

pub fn is_windows_drive_path_href(href: &str) -> bool {
    // /^[A-Za-z]:[\\/]/
    let decoded = safe_decode_uri_component(href);
    let mut chars = decoded.chars();
    matches!((chars.next(), chars.next(), chars.next()),
        (Some(drive), Some(':'), Some(sep)) if drive.is_ascii_alphabetic() && (sep == '/' || sep == '\\'))
}

#[cfg(test)]
mod tests;
