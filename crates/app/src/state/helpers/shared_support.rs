//! The small `packages/shared` functions the client-runtime helpers import.
//!
//! `packages/shared` is not ported into the app yet (the `turin-shared` crate lives on another
//! branch), so the pieces these helpers need are written here, each tagged with the upstream file
//! it comes from. Once the app depends on `turin-shared`, replace the matching section with a
//! `use`.
//!
//! The path, model, change-request URL, thread pull-request and source-control key helpers are
//! not here: each has exactly one copy under `state/support/` (see PORTING.md, DEDUP).

use std::cmp::Ordering;
use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::js_trim;
use url::Url;

use crate::state::support::path::strip_slash_prefixed_windows_drive;

/// The members of JS `\s` (ECMAScript WhiteSpace and LineTerminator), as the inside of a regex
/// character class. Rust's `\s` differs: it has U+0085 and lacks U+FEFF.
pub const JS_WS_CLASS: &str = r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";

/// JS `.` (no `s` flag): anything but a line terminator, as the inside of a character class.
pub const JS_NOT_LINE_TERMINATOR_CLASS: &str = r"^\n\r\x{2028}\x{2029}";

/// ECMAScript WhiteSpace and LineTerminator, i.e. what `String.prototype.trim` strips.
pub fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

// ---------------------------------------------------------------------------------------------
// upstream: packages/shared/src/markdownLinks.ts
// ---------------------------------------------------------------------------------------------

/// JS `decodeURIComponent`; `None` where JS throws a `URIError`.
fn decode_uri_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            let high = (hex[0] as char).to_digit(16)?;
            let low = (hex[1] as char).to_digit(16)?;
            decoded.push((high * 16 + low) as u8);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

pub fn safe_decode_uri_component(value: &str) -> String {
    decode_uri_component(value).unwrap_or_else(|| value.to_owned())
}

/// JS `encodeURIComponent`: everything but `A-Za-z0-9 - _ . ! ~ * ' ( )` is escaped.
pub fn encode_uri_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            )
        {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub fn normalize_markdown_link_destination(value: &str) -> &str {
    let trimmed = js_trim(value);
    if trimmed.len() >= 2 && trimmed.starts_with('<') && trimmed.ends_with('>') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    }
}

/// A markdown link destination split at its first `#` and then its first `?`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownLinkSearchAndHash {
    pub path: String,
    pub hash: String,
}

pub fn split_markdown_link_search_and_hash(value: &str) -> MarkdownLinkSearchAndHash {
    let hash_index = value.find('#');
    let path_with_search = hash_index.map_or(value, |index| &value[..index]);
    let hash = hash_index.map_or("", |index| &value[index..]);
    let query_index = path_with_search.find('?');
    let path = query_index.map_or(path_with_search, |index| &path_with_search[..index]);
    MarkdownLinkSearchAndHash {
        path: path.to_owned(),
        hash: hash.to_owned(),
    }
}

// ---------------------------------------------------------------------------------------------
// upstream: packages/shared/src/fileLinks.ts
// ---------------------------------------------------------------------------------------------

/// A `file:` URL as a host path (still percent-encoded) and its fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileUrlTarget {
    pub path: String,
    pub hash: String,
}

/// Turns a `file:` URL into a host path, still percent-encoded so callers that
/// decode every destination in one place do not decode file URLs twice. A
/// non-localhost authority becomes a UNC share.
pub fn parse_file_url_href(href: &str) -> Option<FileUrlTarget> {
    let parsed = Url::parse(href).ok()?;
    if !parsed.scheme().eq_ignore_ascii_case("file") {
        return None;
    }

    let hostname = parsed.host_str().unwrap_or("");
    let unc_hostname = if hostname.eq_ignore_ascii_case("localhost") {
        ""
    } else {
        hostname
    };
    let path = if unc_hostname.is_empty() {
        parsed.path().to_owned()
    } else {
        format!("\\\\{unc_hostname}{}", parsed.path().replace('/', "\\"))
    };
    if path.is_empty() {
        return None;
    }
    // JS `URL.hash` is "" for both a missing and an empty fragment.
    let hash = match parsed.fragment() {
        Some(fragment) if !fragment.is_empty() => format!("#{fragment}"),
        _ => String::new(),
    };
    Some(FileUrlTarget {
        path: strip_slash_prefixed_windows_drive(&path).to_owned(),
        hash,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePathPosition {
    pub path: String,
    pub line: Option<u64>,
    pub column: Option<u64>,
}

static POSITION_SUFFIX_CAPTURE_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r":([0-9]+)(?::([0-9]+))?$").expect("valid regex"));
static POSITION_HASH_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i-u)^#L([0-9]+)(?:C([0-9]+))?$").expect("valid regex"));

/// `Number.parseInt(digits, 10)`; a run too long for `u64` saturates.
fn parse_int(digits: &str) -> u64 {
    digits.parse().unwrap_or(u64::MAX)
}

pub fn split_file_path_position(path: &str, hash: &str) -> FilePathPosition {
    let suffix_match = POSITION_SUFFIX_CAPTURE_PATTERN.captures(path);
    // Where the position suffix starts in `path`, when it came from the path rather than the hash.
    let suffix_start = suffix_match
        .as_ref()
        .and_then(|captures| captures.get(0))
        .map(|whole| whole.start());
    let captures = suffix_match.or_else(|| POSITION_HASH_PATTERN.captures(hash));
    let Some(captures) = captures else {
        return FilePathPosition {
            path: path.to_owned(),
            line: None,
            column: None,
        };
    };
    let Some(line_digits) = captures
        .get(1)
        .map(|m| m.as_str())
        .filter(|s| !s.is_empty())
    else {
        return FilePathPosition {
            path: path.to_owned(),
            line: None,
            column: None,
        };
    };

    let line = parse_int(line_digits);
    let column = captures.get(2).map(|m| parse_int(m.as_str()));
    FilePathPosition {
        path: match suffix_start {
            Some(start) => path[..start].to_owned(),
            None => path.to_owned(),
        },
        line: (line > 0).then_some(line),
        column: column.filter(|column| *column > 0),
    }
}

// ---------------------------------------------------------------------------------------------
// upstream: packages/shared/src/{filePreview,video,image}.ts
// ---------------------------------------------------------------------------------------------

fn image_mime_type_by_extension(extension: &str) -> Option<&'static str> {
    Some(match extension {
        ".avif" => "image/avif",
        ".gif" => "image/gif",
        ".ico" => "image/x-icon",
        ".jpeg" => "image/jpeg",
        ".jpg" => "image/jpeg",
        ".png" => "image/png",
        ".svg" => "image/svg+xml",
        ".webp" => "image/webp",
        _ => return None,
    })
}

fn video_mime_type_by_extension(extension: &str) -> Option<&'static str> {
    Some(match extension {
        "avi" => "video/x-msvideo",
        "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "mp4" => "video/mp4",
        "ogv" => "video/ogg",
        "webm" => "video/webm",
        _ => return None,
    })
}

const GENERIC_MIME_TYPES: [&str; 3] = [
    "application/octet-stream",
    "binary/octet-stream",
    "application/unknown",
];

/// Recognizes videos even when the file picker omitted their MIME type.
pub fn video_mime_type(name: &str, mime_type: &str) -> Option<String> {
    let mime_type = js_trim(mime_type.split(';').next().unwrap_or("")).to_lowercase();
    if mime_type.starts_with("video/") {
        return Some(mime_type);
    }
    // The name is only evidence when nothing recorded what this is. A definite type already
    // answers the question, and a `.mp4` on a PDF must not override it.
    if !mime_type.is_empty() && !GENERIC_MIME_TYPES.contains(&mime_type.as_str()) {
        return None;
    }
    let dot_index = name.rfind('.')?;
    video_mime_type_by_extension(&name[dot_index + 1..].to_lowercase()).map(str::to_owned)
}

/// Classifies a literal filesystem extension, without URL decoding or suffix removal.
pub fn media_mime_type_from_extension(extension: &str) -> Option<String> {
    // `/^\.[a-z0-9]+$/i`
    let digits = extension.strip_prefix('.')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    if let Some(image) = image_mime_type_by_extension(&extension.to_lowercase()) {
        return Some(image.to_owned());
    }
    video_mime_type(&format!("media{extension}"), "")
}

static DATA_MIME_TYPE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i-u)^data:((?:image|video)/[\w.+-]+)[;,]").expect("valid regex")
});
static URL_LIKE_SOURCE_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i-u)^(?:https?:|file:|//)").expect("valid regex"));

/// Classifies an authored media path or URL. Filesystem validation uses the literal extension.
pub fn media_mime_type(path: &str) -> Option<String> {
    let trimmed = js_trim(path);
    let source = if trimmed.len() >= 2 && trimmed.starts_with('<') && trimmed.ends_with('>') {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    if let Some(data_mime_type) = DATA_MIME_TYPE_PATTERN
        .captures(source)
        .and_then(|c| c.get(1))
    {
        return Some(data_mime_type.as_str().to_lowercase());
    }

    let mut source_path: String = source.split(['?', '#']).next().unwrap_or("").to_owned();
    if URL_LIKE_SOURCE_PATTERN.is_match(source) {
        let base = Url::parse("https://media.invalid").expect("valid base");
        source_path = base.join(source).ok()?.path().to_owned();
    }
    // A literal percent character is valid in a filename.
    let source_path = safe_decode_uri_component(&source_path);
    let basename = source_path.rsplit(['\\', '/']).next().unwrap_or("");
    let extension_index = basename.rfind('.')?;
    media_mime_type_from_extension(&basename[extension_index..])
}

// ---------------------------------------------------------------------------------------------
// upstream: packages/shared/src/semver.ts
// ---------------------------------------------------------------------------------------------

struct ParsedSemver {
    major: f64,
    minor: f64,
    patch: f64,
    prerelease: Vec<String>,
}

fn is_semver_number_segment(value: &str) -> bool {
    // `/^\d+$/` (ASCII)
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
}

/// JS `value.split("-", 2)`: the first two pieces, the rest dropped (not kept in the second).
fn split_dash_limit_two(value: &str) -> (&str, Option<&str>) {
    let mut pieces = value.split('-');
    (pieces.next().unwrap_or(""), pieces.next())
}

pub fn normalize_semver_version(version: &str) -> String {
    let (main, prerelease) = split_dash_limit_two(js_trim(version));
    let mut segments: Vec<&str> = main
        .split('.')
        .map(js_trim)
        .filter(|segment| !segment.is_empty())
        .collect();

    // Pad shorthand versions ("20" or "20.1") up to three segments so major-only
    // and minor-only inputs parse and compare numerically.
    while !segments.is_empty() && segments.len() < 3 {
        segments.push("0");
    }

    match prerelease {
        Some(prerelease) if !prerelease.is_empty() => {
            format!("{}-{prerelease}", segments.join("."))
        }
        _ => segments.join("."),
    }
}

fn parse_semver(value: &str) -> Option<ParsedSemver> {
    let normalized = normalize_semver_version(value);
    let normalized = normalized.strip_prefix('v').unwrap_or(&normalized);
    let (main, prerelease) = split_dash_limit_two(normalized);
    let segments: Vec<&str> = main.split('.').collect();
    if segments.len() != 3 {
        return None;
    }
    if !segments
        .iter()
        .all(|segment| is_semver_number_segment(segment))
    {
        return None;
    }
    let number = |segment: &str| segment.parse::<f64>().ok();
    Some(ParsedSemver {
        major: number(segments[0])?,
        minor: number(segments[1])?,
        patch: number(segments[2])?,
        prerelease: prerelease
            .map(|prerelease| {
                prerelease
                    .split('.')
                    .map(js_trim)
                    .filter(|segment| !segment.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn compare_prerelease_identifier(left: &str, right: &str) -> Ordering {
    let left_numeric = is_semver_number_segment(left);
    let right_numeric = is_semver_number_segment(right);
    if left_numeric && right_numeric {
        let left: f64 = left.parse().unwrap_or(f64::NAN);
        let right: f64 = right.parse().unwrap_or(f64::NAN);
        return left.partial_cmp(&right).unwrap_or(Ordering::Equal);
    }
    if left_numeric {
        return Ordering::Less;
    }
    if right_numeric {
        return Ordering::Greater;
    }
    // Upstream: `localeCompare`. ICU collation is not available here; identifiers are ASCII
    // in practice, where byte order agrees for the cases that matter (same-case words).
    left.cmp(right)
}

pub fn compare_semver_versions(left: &str, right: &str) -> Ordering {
    let (Some(parsed_left), Some(parsed_right)) = (parse_semver(left), parse_semver(right)) else {
        // Upstream: `left.localeCompare(right)`; see `compare_prerelease_identifier`.
        return left.cmp(right);
    };

    for (l, r) in [
        (parsed_left.major, parsed_right.major),
        (parsed_left.minor, parsed_right.minor),
        (parsed_left.patch, parsed_right.patch),
    ] {
        if l != r {
            return l.partial_cmp(&r).unwrap_or(Ordering::Equal);
        }
    }

    match (
        parsed_left.prerelease.is_empty(),
        parsed_right.prerelease.is_empty(),
    ) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }

    let length = parsed_left
        .prerelease
        .len()
        .max(parsed_right.prerelease.len());
    for index in 0..length {
        let (Some(left_identifier), Some(right_identifier)) = (
            parsed_left.prerelease.get(index),
            parsed_right.prerelease.get(index),
        ) else {
            return if parsed_left.prerelease.get(index).is_none() {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        };
        let comparison = compare_prerelease_identifier(left_identifier, right_identifier);
        if comparison != Ordering::Equal {
            return comparison;
        }
    }
    Ordering::Equal
}

// ---------------------------------------------------------------------------------------------
// upstream: packages/shared/src/inlineSkills.ts
// ---------------------------------------------------------------------------------------------

fn title_case_words(value: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    for segment in value.split(|c: char| c == '_' || c == '-' || is_js_whitespace(c) || c == ':') {
        let mut chars = segment.chars();
        let Some(first) = chars.next() else { continue };
        let mut word: String = first.to_uppercase().collect();
        word.push_str(chars.as_str());
        words.push(word);
    }
    words.join(" ")
}

pub fn format_provider_skill_display_name(name: &str, display_name: Option<&str>) -> String {
    if let Some(display_name) = display_name.map(js_trim).filter(|name| !name.is_empty()) {
        return display_name.to_owned();
    }
    title_case_words(name)
}

// ---------------------------------------------------------------------------------------------
// upstream: packages/shared/src/promptLinks.ts (types only)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptLinkRepository {
    pub owner: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptLinkPullRequest {
    pub owner: String,
    pub name: String,
    pub number: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_file_path_positions_from_suffixes_and_hashes() {
        let position = |path: &str, hash: &str| {
            let split = split_file_path_position(path, hash);
            (split.path, split.line, split.column)
        };
        assert_eq!(
            position("/a/b.ts:12:3", ""),
            ("/a/b.ts".to_owned(), Some(12), Some(3))
        );
        assert_eq!(
            position("/a/b.ts", "#L7C2"),
            ("/a/b.ts".to_owned(), Some(7), Some(2))
        );
        assert_eq!(
            position("/a/b.ts:0", ""),
            ("/a/b.ts".to_owned(), None, None)
        );
        assert_eq!(position("/a/b.ts", ""), ("/a/b.ts".to_owned(), None, None));
    }

    #[test]
    fn compares_semver_versions_like_upstream() {
        use Ordering::{Equal, Greater, Less};
        assert_eq!(compare_semver_versions("2.1.10", "2.1.9"), Greater);
        assert_eq!(compare_semver_versions("v2.1", "2.1.0"), Equal);
        assert_eq!(compare_semver_versions("2.1.0-beta", "2.1.0"), Less);
        assert_eq!(
            compare_semver_versions("2.1.0-beta.2", "2.1.0-beta.10"),
            Less
        );
        assert_eq!(compare_semver_versions("2.1.0-beta", "2.1.0-beta.1"), Less);
        // `split("-", 2)` drops everything past the second piece.
        assert_eq!(normalize_semver_version("1.0.0-beta-1"), "1.0.0-beta");
    }

    #[test]
    fn mime_types_follow_extensions_and_data_urls() {
        assert_eq!(media_mime_type("a/b.PNG?x=1").as_deref(), Some("image/png"));
        assert_eq!(
            media_mime_type("data:Video/MP4;base64,AA").as_deref(),
            Some("video/mp4")
        );
        assert_eq!(media_mime_type("notes.txt"), None);
        assert_eq!(
            media_mime_type_from_extension(".mkv").as_deref(),
            Some("video/x-matroska")
        );
        assert_eq!(media_mime_type_from_extension("mp4"), None);
    }
}
