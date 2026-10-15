//! Port of upstream/packages/shared/src/filePreview.ts

use std::collections::HashMap;

use crate::js_string::{JS_DOT, decode_uri_component};
use crate::video::video_mime_type;
use turin_contracts::base_schemas::js_trim;
use url::Url;

const ICON_EXTENSION_BY_MIME_TYPE: &[(&str, &str)] = &[
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/gif", "gif"),
    ("image/webp", "webp"),
    ("application/pdf", "pdf"),
    ("application/zip", "zip"),
    ("application/vnd.sqlite3", "sqlite"),
    ("text/x-python", "py"),
    ("text/javascript", "js"),
    ("text/x-ruby", "rb"),
    ("text/x-shellscript", "sh"),
];

/// A detected type fills in an unknown filename's icon; callers keep any
/// specific icon already supplied by the filename.
pub fn file_icon_path_for_mime_type(path: &str, mime_type: Option<&str>) -> String {
    let extension = mime_type.and_then(|mime| {
        ICON_EXTENSION_BY_MIME_TYPE
            .iter()
            .find(|(candidate, _)| *candidate == mime)
            .map(|(_, extension)| *extension)
    });
    match extension {
        Some(extension) => format!("file.{extension}"),
        None => path.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePreviewKind {
    Image,
    Video,
    Audio,
    Pdf,
    Html,
    Markdown,
    Text,
    Unsupported,
}

/// Content classification is identical for captured attachments and workspace references.
pub fn file_preview_kind(name: &str, mime_type: Option<&str>) -> FilePreviewKind {
    let mime = mime_type
        .map(|value| {
            let head = value.split(';').next().unwrap_or("");
            js_trim(head).to_ascii_lowercase()
        })
        .unwrap_or_default();
    let lowered = name.to_ascii_lowercase();
    let extension = lowered
        .rfind('.')
        .map(|index| lowered[index..].to_string())
        .unwrap_or_default();
    let generic = mime.is_empty() || mime == "application/octet-stream" || mime == "text/plain";
    if mime == "application/pdf" {
        return FilePreviewKind::Pdf;
    }
    if mime == "text/html" {
        return FilePreviewKind::Html;
    }
    if mime == "text/markdown" || mime == "text/x-markdown" {
        return FilePreviewKind::Markdown;
    }
    if mime.starts_with("image/") {
        return FilePreviewKind::Image;
    }
    if mime.starts_with("video/") {
        return FilePreviewKind::Video;
    }
    if mime.starts_with("audio/") {
        return FilePreviewKind::Audio;
    }
    if generic {
        if extension == ".pdf" {
            return FilePreviewKind::Pdf;
        }
        if regex_html(&extension) {
            return FilePreviewKind::Html;
        }
        if regex_markdown(&extension) {
            return FilePreviewKind::Markdown;
        }
        if let Some(media) = media_mime_type_from_extension(&extension) {
            if media.starts_with("image/") {
                return FilePreviewKind::Image;
            }
            if media.starts_with("video/") {
                return FilePreviewKind::Video;
            }
        }
        if audio_mime_type_from_extension(&extension).is_some() {
            return FilePreviewKind::Audio;
        }
        let base = lowered.rsplit(['\\', '/']).next().unwrap_or("");
        if TEXT_EXTENSION.is_match(&extension) || TEXT_BASENAME.is_match(base) {
            return FilePreviewKind::Text;
        }
    }
    if mime.starts_with("text/") || TEXT_MIME.is_match(&mime) {
        return FilePreviewKind::Text;
    }
    FilePreviewKind::Unsupported
}

fn regex_html(extension: &str) -> bool {
    extension == ".htm" || extension == ".html"
}

fn regex_markdown(extension: &str) -> bool {
    matches!(extension, ".md" | ".markdown" | ".mdown" | ".mkd" | ".mdx")
}

static TEXT_EXTENSION: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)^\.(txt|log|json|jsonc|jsonl|ndjson|yaml|yml|toml|ini|conf|config|env|csv|tsv|xml|css|scss|sass|less|js|jsx|mjs|cjs|ts|tsx|mts|cts|py|pyi|rb|go|rs|swift|kt|kts|java|c|h|cc|cpp|hpp|cs|php|sh|bash|zsh|fish|sql|graphql|gql|vue|svelte|r|lua|ex|exs|erl|hs|clj|dart|diff|patch|lock|properties|gradle)$",
    )
    .expect("text extension regex")
});

static TEXT_BASENAME: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)^(dockerfile|makefile|gemfile|rakefile|license|readme|\.gitignore|\.gitattributes|\.editorconfig|\.env)(\.|$)",
    )
    .expect("text basename regex")
});

static TEXT_MIME: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(&format!(
        r"^(application/(json|{JS_DOT}*\+json|xml|{JS_DOT}*\+xml|javascript|x-javascript|yaml|x-yaml|toml|sql))$"
    ))
    .expect("text mime regex")
});

pub const FILE_TEXT_PREVIEW_MAX_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePreviewText {
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FilePreviewTextError {
    #[error("This file contains binary data and cannot be shown as text.")]
    Binary,
    #[error("This file is not UTF-8 text. Open it in another app to view its contents.")]
    NotUtf8,
}

/// Reject binary data rather than displaying replacement characters as a document.
///
/// `truncated` matches `TextDecoder` `{ stream: true }`: a partial code point at the
/// end of the bounded slice is withheld instead of failing the decode.
pub fn decode_file_preview_text(
    bytes: &[u8],
    truncated: bool,
) -> Result<FilePreviewText, FilePreviewTextError> {
    let bounded = if bytes.len() > FILE_TEXT_PREVIEW_MAX_BYTES {
        &bytes[..FILE_TEXT_PREVIEW_MAX_BYTES]
    } else {
        bytes
    };
    if bounded.contains(&0) {
        return Err(FilePreviewTextError::Binary);
    }
    let stream = truncated || bytes.len() > bounded.len();
    let complete = if stream {
        utf8_stream_prefix(bounded).ok_or(FilePreviewTextError::NotUtf8)?
    } else {
        bounded
    };
    let text = std::str::from_utf8(complete)
        .map_err(|_| FilePreviewTextError::NotUtf8)?
        .to_string();
    Ok(FilePreviewText {
        text,
        truncated: truncated || bytes.len() > bounded.len(),
    })
}

/// Drop an incomplete trailing UTF-8 sequence. Invalid bytes anywhere else fail.
fn utf8_stream_prefix(bytes: &[u8]) -> Option<&[u8]> {
    let mut index = 0;
    let mut last_complete = 0;
    while index < bytes.len() {
        let width = utf8_width(bytes[index])?;
        if index + width > bytes.len() {
            return Some(&bytes[..last_complete]);
        }
        if std::str::from_utf8(&bytes[index..index + width]).is_err() {
            return None;
        }
        index += width;
        last_complete = index;
    }
    Some(bytes)
}

fn utf8_width(byte: u8) -> Option<usize> {
    if byte < 0x80 {
        Some(1)
    } else if byte & 0xe0 == 0xc0 {
        Some(2)
    } else if byte & 0xf0 == 0xe0 {
        Some(3)
    } else if byte & 0xf8 == 0xf0 {
        Some(4)
    } else {
        None
    }
}

pub const WORKSPACE_BROWSER_PREVIEW_EXTENSIONS: &[&str] = &[".htm", ".html", ".pdf"];

pub const WORKSPACE_IMAGE_PREVIEW_EXTENSIONS: &[&str] = &[
    ".avif", ".gif", ".ico", ".jpeg", ".jpg", ".png", ".svg", ".webp",
];

fn image_mime_by_extension() -> &'static HashMap<&'static str, &'static str> {
    use std::sync::OnceLock;
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        HashMap::from([
            (".avif", "image/avif"),
            (".gif", "image/gif"),
            (".ico", "image/x-icon"),
            (".jpeg", "image/jpeg"),
            (".jpg", "image/jpeg"),
            (".png", "image/png"),
            (".svg", "image/svg+xml"),
            (".webp", "image/webp"),
        ])
    })
}

fn browser_mime_by_extension() -> &'static HashMap<&'static str, &'static str> {
    use std::sync::OnceLock;
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        HashMap::from([
            (".htm", "text/html"),
            (".html", "text/html"),
            (".pdf", "application/pdf"),
        ])
    })
}

fn audio_mime_by_extension() -> &'static HashMap<&'static str, &'static str> {
    use std::sync::OnceLock;
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        HashMap::from([
            (".mp3", "audio/mpeg"),
            (".wav", "audio/wav"),
            (".ogg", "audio/ogg"),
            (".oga", "audio/ogg"),
            (".flac", "audio/flac"),
            (".aac", "audio/aac"),
            (".m4a", "audio/mp4"),
            (".opus", "audio/ogg"),
            (".aiff", "audio/aiff"),
        ])
    })
}

fn literal_extension(extension: &str) -> Option<String> {
    let pattern = regex::Regex::new(r"(?i)^\.[a-z0-9]+$").expect("extension");
    if pattern.is_match(extension) {
        Some(extension.to_ascii_lowercase())
    } else {
        None
    }
}

/// Audio a player can request inline; the server serves these with byte ranges like video.
pub fn audio_mime_type_from_extension(extension: &str) -> Option<&'static str> {
    let lowered = literal_extension(extension)?;
    audio_mime_by_extension().get(lowered.as_str()).copied()
}

/// Classifies a literal filesystem extension, without URL decoding or suffix removal.
pub fn media_mime_type_from_extension(extension: &str) -> Option<String> {
    let lowered = literal_extension(extension)?;
    if let Some(mime) = image_mime_by_extension().get(lowered.as_str()) {
        return Some((*mime).to_string());
    }
    video_mime_type(&format!("media{lowered}"), "")
}

/// Files the server serves in place from anywhere on its host: media, audio and browser documents.
pub fn host_preview_mime_type_from_extension(extension: &str) -> Option<String> {
    let lowered = literal_extension(extension)?;
    media_mime_type_from_extension(&lowered)
        .or_else(|| audio_mime_type_from_extension(&lowered).map(str::to_string))
        .or_else(|| {
            browser_mime_by_extension()
                .get(lowered.as_str())
                .map(|mime| (*mime).to_string())
        })
}

static DATA_MIME: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)^data:((?:image|video)/[\w.+-]+)[;,]").expect("data mime")
});

static URLISH: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)^(?:https?:|file:|//)").expect("url"));

/// Classifies an authored media path or URL. Filesystem validation uses the literal extension.
pub fn media_mime_type(path: &str) -> Option<String> {
    let trimmed = js_trim(path);
    let source = if trimmed.starts_with('<') && trimmed.ends_with('>') && trimmed.len() >= 2 {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    if let Some(capture) = DATA_MIME.captures(source) {
        return Some(capture.get(1)?.as_str().to_ascii_lowercase());
    }
    let mut source_path = source.split(['?', '#']).next().unwrap_or("").to_string();
    if URLISH.is_match(source) {
        let base = Url::parse("https://media.invalid").ok()?;
        let parsed = Url::options().base_url(Some(&base)).parse(source).ok()?;
        source_path = parsed.path().to_string();
    }
    if let Ok(decoded) = decode_uri_component(&source_path) {
        source_path = decoded;
    }
    let basename = source_path.rsplit(['\\', '/']).next().unwrap_or("");
    let extension_index = basename.rfind('.')?;
    media_mime_type_from_extension(&basename[extension_index..])
}

pub fn media_kind_from_path(path: &str) -> Option<&'static str> {
    let mime = media_mime_type(path)?;
    if mime.starts_with("video/") {
        Some("video")
    } else {
        Some("image")
    }
}

fn has_preview_extension(path: &str, extensions: &[&str]) -> bool {
    let literal = path.to_ascii_lowercase();
    extensions
        .iter()
        .any(|extension| literal.ends_with(extension))
}

pub fn is_workspace_browser_preview_path(path: &str) -> bool {
    has_preview_extension(path, WORKSPACE_BROWSER_PREVIEW_EXTENSIONS)
}

pub fn is_workspace_image_preview_path(path: &str) -> bool {
    has_preview_extension(path, WORKSPACE_IMAGE_PREVIEW_EXTENSIONS)
}

/// File viewers receive literal filesystem paths, not Markdown URLs.
pub fn is_workspace_video_preview_path(path: &str) -> bool {
    video_mime_type(path, "").is_some()
}

pub fn is_workspace_audio_preview_path(path: &str) -> bool {
    let Some(extension_index) = path.rfind('.') else {
        return false;
    };
    audio_mime_type_from_extension(&path[extension_index..]).is_some()
}

pub fn is_workspace_preview_entry_path(path: &str) -> bool {
    is_workspace_browser_preview_path(path) || is_workspace_image_preview_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_browser_and_image_preview_paths() {
        for path in [
            "report.html",
            "report.HTM",
            "document#draft.pdf",
            "reports?old/document.pdf",
        ] {
            assert!(is_workspace_browser_preview_path(path), "{path}");
            assert!(is_workspace_preview_entry_path(path), "{path}");
        }
        for path in [
            "icon.png",
            "photo.JPEG",
            "animation.gif",
            "vector#mark.svg",
            "photo?edited.JPEG",
            "images#archive/icon.png",
            "texture.webp",
            "image.avif",
        ] {
            assert!(is_workspace_image_preview_path(path), "{path}");
            assert!(is_workspace_preview_entry_path(path), "{path}");
        }
    }

    #[test]
    fn rejects_non_preview_paths() {
        for path in [
            "README.md",
            "src/index.ts",
            "image.png.ts",
            "png",
            "image.png#notes.txt",
            "image.svg?notes.txt",
            "document.pdf?download=1",
            "report.html#notes.txt",
            "image%2Epng",
        ] {
            assert!(!is_workspace_preview_entry_path(path), "{path}");
        }
    }

    #[test]
    fn serves_audio_in_place() {
        assert!(is_workspace_audio_preview_path("notes/recording.WAV"));
        assert!(!is_workspace_audio_preview_path("recording.wav.ts"));
        assert_eq!(
            host_preview_mime_type_from_extension(".m4a").as_deref(),
            Some("audio/mp4")
        );
        assert_eq!(
            host_preview_mime_type_from_extension(".mp4").as_deref(),
            Some("video/mp4")
        );
        assert_eq!(host_preview_mime_type_from_extension(".txt"), None);
    }

    #[test]
    fn classifies_decoded_pathnames() {
        let cases = [
            (
                "https://cdn.example/clip.webm?download=1#t=2",
                Some("video"),
            ),
            ("https://example.com/download?name=recording.mp4", None),
            ("https://example.png", None),
            ("images%2Fresult%2Epng", Some("image")),
            ("images/result%23v2.png", Some("image")),
            ("images/result.png%23secret.txt", None),
            ("images/result.png%3Fsecret.txt", None),
            ("/tmp/100%.png", Some("image")),
        ];
        for (source, kind) in cases {
            assert_eq!(media_kind_from_path(source), kind, "{source}");
        }
    }

    #[test]
    fn distinguishes_authored_urls_from_literal_filenames() {
        let cases = [
            ("recording.mp4#t=2", Some("video"), false),
            ("recording%2Emp4", Some("video"), false),
            ("recording#take2.mp4", None, true),
            ("recording?take2.mp4", None, true),
        ];
        for (source, kind, literal) in cases {
            assert_eq!(media_kind_from_path(source), kind, "{source}");
            assert_eq!(is_workspace_video_preview_path(source), literal, "{source}");
        }
    }

    #[test]
    fn classifies_attachments() {
        let cases = [
            (
                "example.json",
                Some("application/octet-stream"),
                FilePreviewKind::Text,
            ),
            ("README.md", Some("text/plain"), FilePreviewKind::Markdown),
            ("component.tsx", Some(""), FilePreviewKind::Text),
            ("report.pdf", Some("application/pdf"), FilePreviewKind::Pdf),
            ("page.HTML", Some(""), FilePreviewKind::Html),
            ("recording.mp3", Some(""), FilePreviewKind::Audio),
            (
                "payload",
                Some("application/problem+json"),
                FilePreviewKind::Text,
            ),
            (
                "archive.zip",
                Some("application/zip"),
                FilePreviewKind::Unsupported,
            ),
            (
                "misleading.json",
                Some("application/pdf"),
                FilePreviewKind::Pdf,
            ),
            (
                "misleading.pdf",
                Some("application/zip"),
                FilePreviewKind::Unsupported,
            ),
        ];
        for (name, mime, expected) in cases {
            assert_eq!(file_preview_kind(name, mime), expected, "{name}");
        }
    }

    #[test]
    fn rejects_binary_and_invalid_utf8() {
        assert!(matches!(
            decode_file_preview_text(&[65, 0, 66], false),
            Err(FilePreviewTextError::Binary)
        ));
        assert!(matches!(
            decode_file_preview_text(&[255], false),
            Err(FilePreviewTextError::NotUtf8)
        ));
    }

    #[test]
    fn does_not_corrupt_a_multibyte_character_at_the_preview_boundary() {
        let mut bytes = vec![b'a'; FILE_TEXT_PREVIEW_MAX_BYTES + 1];
        bytes[FILE_TEXT_PREVIEW_MAX_BYTES - 1] = 0xe2;
        bytes[FILE_TEXT_PREVIEW_MAX_BYTES] = 0x82;
        let preview = decode_file_preview_text(&bytes, false).expect("decode");
        assert!(preview.truncated);
        assert!(!preview.text.ends_with('\u{fffd}'));
        assert_eq!(
            preview.text.chars().count(),
            FILE_TEXT_PREVIEW_MAX_BYTES - 1
        );
    }
}
