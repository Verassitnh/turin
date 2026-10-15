//! The small `packages/shared` functions the client-runtime helpers import.
//!
//! `packages/shared` is not ported into the app yet (the `turin-shared` crate lives on another
//! branch), so the pieces these helpers need are written here, each tagged with the upstream file
//! it comes from. Once the app depends on `turin-shared`, replace the matching section with a
//! `use`.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::model::model_slug_aliases_by_provider;
use turin_contracts::thread_pull_request::{
    ThreadLinkedPullRequest, ThreadPullRequestKey, ThreadPullRequestLink,
    ThreadPullRequestLinkSource,
};
use turin_contracts::{TrimmedNonEmptyString, js_trim};
use url::Url;

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
// upstream: packages/shared/src/path.ts
// ---------------------------------------------------------------------------------------------

/// `/^[a-zA-Z]:([/\\]|$)/`
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

/// Browser URL parsers write `C:/foo` as `/C:/foo` for file URLs.
/// (`/^\/[A-Za-z]:[\\/]/`)
pub fn strip_slash_prefixed_windows_drive(path: &str) -> &str {
    let bytes = path.as_bytes();
    if bytes.len() >= 4
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && (bytes[3] == b'\\' || bytes[3] == b'/')
    {
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
// upstream: packages/shared/src/model.ts
// ---------------------------------------------------------------------------------------------

/// `SelectableModelOption`: what `resolve_selectable_model` reads from a model entry.
pub trait SelectableModelOption {
    fn slug(&self) -> &str;
    fn name(&self) -> &str;
    fn aliases(&self) -> Option<&[String]>;
}

/// Custom model identifiers are provider-owned, so only trim them; never expand aliases.
pub fn normalize_custom_model_slug(model: Option<&str>) -> Option<&str> {
    model.map(js_trim).filter(|trimmed| !trimmed.is_empty())
}

pub fn normalize_model_slug<'a>(model: Option<&'a str>, provider: &str) -> Option<Cow<'a, str>> {
    let trimmed = normalize_custom_model_slug(model)?;
    let aliased = model_slug_aliases_by_provider(provider).and_then(|aliases| {
        aliases
            .iter()
            .find(|(alias, _)| *alias == trimmed)
            .map(|(_, canonical)| *canonical)
    });
    Some(match aliased {
        Some(canonical) => Cow::Borrowed(canonical),
        None => Cow::Borrowed(trimmed),
    })
}

pub fn resolve_selectable_model<'a, T: SelectableModelOption>(
    provider: &str,
    value: Option<&str>,
    options: &'a [T],
) -> Option<&'a str> {
    let trimmed = js_trim(value?);
    if trimmed.is_empty() {
        return None;
    }

    if let Some(direct) = options.iter().find(|option| option.slug() == trimmed) {
        return Some(direct.slug());
    }

    let lowered = trimmed.to_lowercase();
    if let Some(by_name) = options
        .iter()
        .find(|option| option.name().to_lowercase() == lowered)
    {
        return Some(by_name.slug());
    }

    let by_alias = options.iter().find(|option| {
        option
            .aliases()
            .is_some_and(|aliases| aliases.iter().any(|alias| alias.to_lowercase() == lowered))
    });
    if let Some(by_alias) = by_alias {
        return Some(by_alias.slug());
    }

    let normalized = normalize_model_slug(Some(trimmed), provider)?;
    options
        .iter()
        .find(|option| option.slug() == normalized)
        .map(|option| option.slug())
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

// ---------------------------------------------------------------------------------------------
// upstream: packages/shared/src/changeRequestUrl.ts
// ---------------------------------------------------------------------------------------------

/// A change request named the way a thread link names one: the host below which the repository
/// is addressed, the repository path as that host writes it, and the number.
///
/// The two strings are what `pullRequestHostOf` and the project's `repositoryIdentity` produce
/// from a git remote: lower case, no port, the full path below the host. Links are matched
/// against those, so anything else matches nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRequestLink {
    pub host: String,
    pub repository: String,
    pub number: u64,
    /// Forgejo's HTTP host and port, separate from the portless repository identity.
    pub authority: Option<String>,
}

/// The host itself, one of its subdomains, or an install named after the provider.
fn is_host_of(hostname: &str, apex: &str, label: Option<&str>) -> bool {
    if hostname == apex || hostname.ends_with(&format!(".{apex}")) {
        return true;
    }
    label.is_some_and(|label| hostname.split('.').any(|part| part == label))
}

fn path_regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid regex")
}

static GITCAFE_PATH: LazyLock<Regex> =
    LazyLock::new(|| path_regex(r"^/([^/]+/[^/]+)/pulls/([0-9]+)(?:/|$)"));
static GITHUB_PATH: LazyLock<Regex> =
    LazyLock::new(|| path_regex(r"^/([^/]+/[^/]+)/pull/([0-9]+)(?:/|$)"));
static FORGEJO_PATH: LazyLock<Regex> =
    LazyLock::new(|| path_regex(r"^/([^/]+(?:/[^/]+)+)/pulls/([0-9]+)(?:/|$)"));
static GITLAB_PATH: LazyLock<Regex> =
    LazyLock::new(|| path_regex(r"^/([^/]+(?:/[^/]+)+)/-/merge_requests/([0-9]+)(?:/|$)"));
static BITBUCKET_PATH: LazyLock<Regex> =
    LazyLock::new(|| path_regex(r"^/([^/]+/[^/]+)/pull-requests/([0-9]+)(?:/|$)"));
static AZURE_PATH: LazyLock<Regex> =
    LazyLock::new(|| path_regex(r"^/((?:[^/]+/)*_git/[^/]+)/pullrequest/([0-9]+)(?:/|$)"));

/// JS `URL.host`: the hostname plus the port when it is not the scheme's default.
pub fn url_host_with_port(url: &Url) -> String {
    let hostname = url.host_str().unwrap_or("");
    match url.port() {
        Some(port) => format!("{hostname}:{port}"),
        None => hostname.to_owned(),
    }
}

/// The repository and number behind a change request URL on a host this can read, or `None` for
/// anything else: an issue, a commit, a repository root, a host this cannot tell apart from an
/// ordinary link. A doubtful match is worse than no match, so nothing here guesses.
///
/// Each host is recognised by the path shape it alone uses, guarded by a hostname it could
/// plausibly be served from, since self-hosted installs are named whatever their admin chose:
/// GitLab's `/-/` marker is unique enough to trust on any hostname, while `/pull/` is generic
/// enough that it is only believed from a GitHub-ish host.
///
/// Nothing here tries to tell a lookalike hostname from a real one: `github.com.evil.test` and
/// the rest are an open set, and blocking spellings of it costs real hosts (`gitlab.com.br` is a
/// registrable domain). What a claim is worth is decided where it is used.
pub fn parse_change_request_url(target_url: &str) -> Option<ChangeRequestLink> {
    let url = Url::parse(target_url).ok()?;
    // `javascript:`, `mailto:` and friends have no host to speak of and nothing to open.
    if url.scheme() != "https" && url.scheme() != "http" {
        return None;
    }
    let host = url.host_str().unwrap_or("").to_lowercase();
    let pathname = url.path();

    // GitCafe, only on its own two hosts: /{owner}/{repo}/pulls/{n}
    if host == "git.cafe" || host == "staging.git.cafe" {
        return claim(&host, GITCAFE_PATH.captures(pathname));
    }
    // GitHub, and any Enterprise install: /{owner}/{repo}/pull/{n}
    if is_host_of(&host, "github.com", Some("github"))
        && let Some(captures) = GITHUB_PATH.captures(pathname)
    {
        return claim(&host, Some(captures));
    }
    // Forgejo and Gitea use /pulls/ on arbitrary self-hosted domains.
    if let Some(forgejo) = FORGEJO_PATH.captures(pathname) {
        return claim(&host, Some(forgejo)).map(|link| ChangeRequestLink {
            authority: Some(url_host_with_port(&url).to_lowercase()),
            ..link
        });
    }
    // GitLab, self-hosted included: /{group}/[{subgroup}/...]{repo}/-/merge_requests/{n}. The `/-/`
    // separator is GitLab's own, so the hostname is not asked about.
    if let Some(gitlab) = GITLAB_PATH.captures(pathname) {
        return claim(&host, Some(gitlab));
    }
    // Bitbucket Cloud: /{workspace}/{repo}/pull-requests/{n}
    if is_host_of(&host, "bitbucket.org", Some("bitbucket")) {
        return claim(&host, BITBUCKET_PATH.captures(pathname));
    }
    // Azure DevOps, both the current host and the per-organisation one it replaced. `_git` is part
    // of the repository path there, as it is in the remote URL the identity is read from.
    if is_host_of(&host, "dev.azure.com", None) || host.ends_with(".visualstudio.com") {
        return claim(&host, AZURE_PATH.captures(pathname));
    }
    None
}

fn claim(host: &str, captures: Option<regex::Captures<'_>>) -> Option<ChangeRequestLink> {
    let captures = captures?;
    let repository = captures
        .get(1)
        .map(|m| m.as_str())
        .filter(|s| !s.is_empty())?;
    let number: u64 = captures.get(2)?.as_str().parse().ok()?;
    // `Number.isSafeInteger(number) && number > 0`
    (number > 0 && number <= 9_007_199_254_740_991).then(|| ChangeRequestLink {
        host: host.to_owned(),
        repository: repository.to_lowercase(),
        number,
        authority: None,
    })
}

// ---------------------------------------------------------------------------------------------
// upstream: packages/shared/src/sourceControl.ts
// ---------------------------------------------------------------------------------------------

static AZURE_SSH_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:ssh\.dev\.azure\.com|vs-ssh\.visualstudio\.com)/v3/([^/]+)/([^/]+)/([^/]+)$")
        .expect("valid regex")
});
static AZURE_LEGACY_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([^.]+)\.visualstudio\.com/(?:defaultcollection/)?([^/]+)/_git/([^/]+)$")
        .expect("valid regex")
});

pub fn canonical_repository_key(key: &str) -> String {
    let first = AZURE_SSH_KEY.replace(key, "dev.azure.com/${1}/${2}/_git/${3}");
    AZURE_LEGACY_KEY
        .replace(&first, "dev.azure.com/${1}/${2}/_git/${3}")
        .into_owned()
}

// ---------------------------------------------------------------------------------------------
// upstream: packages/shared/src/threadPullRequests.ts (the key helpers)
// ---------------------------------------------------------------------------------------------

/// `ThreadPullRequestKey & { authority?, url? }`.
#[derive(Debug, Clone, Copy)]
pub struct ThreadPullRequestKeySource<'a> {
    pub host: &'a str,
    pub repository: &'a str,
    pub number: u64,
    pub authority: Option<&'a str>,
    pub url: Option<&'a str>,
}

impl<'a> From<&'a ThreadPullRequestLink> for ThreadPullRequestKeySource<'a> {
    fn from(link: &'a ThreadPullRequestLink) -> Self {
        Self {
            host: link.host.as_str(),
            repository: link.repository.as_str(),
            number: link.number.get(),
            authority: None,
            url: Some(link.url.as_str()),
        }
    }
}

impl<'a> From<&'a ThreadPullRequestKey> for ThreadPullRequestKeySource<'a> {
    fn from(key: &'a ThreadPullRequestKey) -> Self {
        Self {
            host: key.host.as_str(),
            repository: key.repository.as_str(),
            number: key.number.get(),
            authority: None,
            url: None,
        }
    }
}

/// A normalized `ThreadPullRequestKey`. Plain strings: normalizing can in principle yield a part
/// the contract's non-empty type would refuse, and comparing keys must never fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedThreadPullRequestKey {
    pub host: String,
    pub repository: String,
    pub number: u64,
}

/// Normalize stored links, recovering Forgejo HTTP ports from old links' URLs.
pub fn normalize_thread_pull_request_key(
    key: &ThreadPullRequestKeySource<'_>,
) -> NormalizedThreadPullRequestKey {
    let parsed = key.url.and_then(parse_change_request_url);
    let authority: Option<String> = match key.authority {
        Some(authority) => Some(authority.to_owned()),
        None => parsed
            .as_ref()
            .filter(|parsed| {
                parsed.repository == js_trim(key.repository).to_lowercase()
                    && parsed.number == key.number
            })
            .and_then(|parsed| parsed.authority.clone()),
    };
    let canonical = canonical_repository_key(&format!(
        "{}/{}",
        js_trim(authority.as_deref().unwrap_or(key.host)).to_lowercase(),
        js_trim(key.repository).to_lowercase()
    ));
    let separator = canonical.find('/').unwrap_or(canonical.len());
    NormalizedThreadPullRequestKey {
        host: canonical[..separator].to_owned(),
        repository: canonical.get(separator + 1..).unwrap_or("").to_owned(),
        number: key.number,
    }
}

/// Legacy Azure selectors omit the organization and project; recover those from the PR URL.
pub fn legacy_thread_pull_request_key(
    linked: &ThreadLinkedPullRequest,
    fallback_host: Option<&str>,
) -> NormalizedThreadPullRequestKey {
    if let Some(parsed) = parse_change_request_url(linked.url.as_str())
        && parsed.number == linked.number.get()
    {
        let canonical = canonical_repository_key(&format!("{}/{}", parsed.host, parsed.repository));
        if parsed.authority.is_some() || canonical.starts_with("dev.azure.com/") {
            return normalize_thread_pull_request_key(&ThreadPullRequestKeySource {
                host: &parsed.host,
                repository: &parsed.repository,
                number: parsed.number,
                authority: parsed.authority.as_deref(),
                url: None,
            });
        }
    }
    let host: String = match fallback_host {
        Some(host) => host.to_owned(),
        None => match Url::parse(linked.url.as_str()) {
            Ok(url) => url.host_str().unwrap_or("").to_owned(),
            Err(_) => "unknown".to_owned(),
        },
    };
    let host = js_trim(&host).to_lowercase();
    NormalizedThreadPullRequestKey {
        host: if host.is_empty() {
            "unknown".to_owned()
        } else {
            host
        },
        repository: js_trim(linked.repository.as_str()).to_lowercase(),
        number: linked.number.get(),
    }
}

pub fn thread_pull_request_key_of(key: &ThreadPullRequestKeySource<'_>) -> String {
    let normalized = normalize_thread_pull_request_key(key);
    format!(
        "{}/{}#{}",
        normalized.host, normalized.repository, normalized.number
    )
}

/// Links a user should see. Tombstoned stack members stay in the array only so the
/// sync reactor does not re-add them.
pub fn visible_thread_pull_requests(
    links: &[ThreadPullRequestLink],
) -> Vec<&ThreadPullRequestLink> {
    links
        .iter()
        .filter(|link| link.source != ThreadPullRequestLinkSource::StackDismissed)
        .collect()
}

/// Older V2 event payloads stored one link; an explicit empty array means it was unlinked.
pub fn thread_pull_requests_of<'a>(
    pull_requests: Option<&'a [ThreadPullRequestLink]>,
    linked_pull_request: Option<&ThreadLinkedPullRequest>,
) -> Cow<'a, [ThreadPullRequestLink]> {
    if let Some(pull_requests) = pull_requests {
        return Cow::Borrowed(pull_requests);
    }
    let Some(linked) = linked_pull_request else {
        return Cow::Borrowed(&[]);
    };
    let key = legacy_thread_pull_request_key(linked, None);
    // The legacy key's parts are non-empty by construction; a link that still cannot be built is
    // dropped rather than invented.
    let link = (|| {
        Some(ThreadPullRequestLink {
            host: TrimmedNonEmptyString::new(&key.host).ok()?,
            repository: TrimmedNonEmptyString::new(&key.repository).ok()?,
            number: linked.number,
            url: linked.url.clone(),
            source: ThreadPullRequestLinkSource::Manual,
            linked_at: "1970-01-01T00:00:00.000Z".to_owned(),
            snapshot: None,
            stack: None,
            watch: None,
        })
    })();
    Cow::Owned(link.into_iter().collect())
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
    fn parses_change_request_urls_by_host_shape() {
        let link = |url: &str| {
            parse_change_request_url(url).map(|l| (l.host, l.repository, l.number, l.authority))
        };
        assert_eq!(
            link("https://github.com/Acme/Web/pull/7/files"),
            Some(("github.com".into(), "acme/web".into(), 7, None))
        );
        assert_eq!(
            link("http://forge.example:3000/team/repo/pulls/7"),
            Some((
                "forge.example".into(),
                "team/repo".into(),
                7,
                Some("forge.example:3000".into())
            ))
        );
        assert_eq!(
            link("https://gitlab.com/g/s/r/-/merge_requests/3"),
            Some(("gitlab.com".into(), "g/s/r".into(), 3, None))
        );
        assert_eq!(link("https://github.com/acme/web/issues/7"), None);
        assert_eq!(link("javascript:alert(1)"), None);
        assert_eq!(link("https://github.com/acme/web/pull/0"), None);
    }

    #[test]
    fn canonicalizes_azure_repository_keys() {
        assert_eq!(
            canonical_repository_key("ssh.dev.azure.com/v3/org/project/web"),
            "dev.azure.com/org/project/_git/web"
        );
        assert_eq!(
            canonical_repository_key("org.visualstudio.com/defaultcollection/project/_git/web"),
            "dev.azure.com/org/project/_git/web"
        );
        assert_eq!(
            canonical_repository_key("github.com/acme/web"),
            "github.com/acme/web"
        );
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
