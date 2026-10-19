//! Port of the path rules in upstream/apps/server/src/attachmentPaths.ts,
//! `attachmentRelativePath` / `attachmentFileExtension` from
//! upstream/apps/server/src/attachmentStore.ts, and `inferImageExtension` from
//! upstream/apps/server/src/imageMime.ts.
//!
//! Providers receive attachment files by path, so this is the only place a
//! chat attachment id turns into a file under the attachments directory.
//!
//! Port of upstream/apps/server/src/imageMime.ts.
//! Port of upstream/apps/server/src/attachmentPaths.ts (path rules).

use std::{path::Path, sync::LazyLock};

use regex::Regex;
use turin_contracts::{chat_attachment::ChatAttachment, js_trim};

/// `SAFE_IMAGE_FILE_EXTENSIONS`.
const SAFE_IMAGE_FILE_EXTENSIONS: [&str; 12] = [
    ".avif", ".bmp", ".gif", ".heic", ".heif", ".ico", ".jpeg", ".jpg", ".png", ".svg", ".tiff",
    ".webp",
];

/// `IMAGE_EXTENSION_BY_MIME_TYPE`.
const IMAGE_EXTENSION_BY_MIME_TYPE: [(&str, &str); 11] = [
    ("image/avif", ".avif"),
    ("image/bmp", ".bmp"),
    ("image/gif", ".gif"),
    ("image/heic", ".heic"),
    ("image/heif", ".heif"),
    ("image/jpeg", ".jpg"),
    ("image/jpg", ".jpg"),
    ("image/png", ".png"),
    ("image/svg+xml", ".svg"),
    ("image/tiff", ".tiff"),
    ("image/webp", ".webp"),
];

static FILENAME_EXTENSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.([a-z0-9]{1,8})$").expect("valid regex"));
static FILE_EXTENSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\.[a-z0-9]{1,10}$").expect("valid regex"));

/// POSIX `path.normalize` for the string shapes attachment ids produce.
fn posix_normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let absolute = path.starts_with('/');
    let trailing = path.ends_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => match parts.last() {
                Some(last) if *last != ".." => {
                    parts.pop();
                }
                _ if !absolute => parts.push(".."),
                _ => {}
            },
            other => parts.push(other),
        }
    }
    let mut out = parts.join("/");
    if out.is_empty() && !absolute {
        out.push('.');
    }
    if absolute {
        out.insert(0, '/');
    }
    if trailing && !out.ends_with('/') {
        out.push('/');
    }
    out
}

/// `normalizeAttachmentRelativePath`: `None` for anything that escapes the
/// directory (leading `..`, including `..foo`, which upstream also rejects).
pub fn normalize_attachment_relative_path(raw: &str) -> Option<String> {
    let normalized = posix_normalize(raw);
    let normalized = normalized.trim_start_matches(['/', '\\']);
    if normalized.is_empty() || normalized.starts_with("..") || normalized.contains('\0') {
        return None;
    }
    Some(normalized.replace('\\', "/"))
}

/// `resolveAttachmentRelativePath`: the absolute file path, or `None` if the
/// normalized relative path resolves outside `attachments_dir`.
pub fn resolve_attachment_relative_path(
    attachments_dir: &Path,
    relative_path: &str,
) -> Option<String> {
    let relative = normalize_attachment_relative_path(relative_path)?;
    let root = posix_normalize(&std::path::absolute(attachments_dir).ok()?.to_string_lossy());
    let file = posix_normalize(&format!("{root}/{relative}"));
    let prefix = format!("{}/", root.trim_end_matches('/'));
    file.starts_with(&prefix).then_some(file)
}

/// `attachmentFileExtension`: lowercased `extname`, or `.bin` when it is
/// missing, reserved (`.part`, which sweeps delete) or not a plain extension.
pub fn attachment_file_extension(file_name: &str) -> String {
    let base = file_name.rsplit('/').next().unwrap_or(file_name);
    let extension = match base.rfind('.') {
        Some(index) if index > 0 => base[index..].to_ascii_lowercase(),
        _ => String::new(),
    };
    if extension == ".part" || !FILE_EXTENSION.is_match(&extension) {
        ".bin".to_owned()
    } else {
        extension
    }
}

/// `inferImageExtension`. The MIME table covers upstream's explicit map; the
/// registry fallback (`Mime.getExtension`) is limited to `image/x-icon`-style
/// types that map to a safe extension, since no other safe extension is
/// reachable from a MIME type the map does not already cover.
pub fn infer_image_extension(mime_type: &str, file_name: Option<&str>) -> &'static str {
    let key = mime_type.to_ascii_lowercase();
    if let Some((_, extension)) = IMAGE_EXTENSION_BY_MIME_TYPE
        .iter()
        .find(|(mime, _)| *mime == key)
    {
        return extension;
    }
    if matches!(key.as_str(), "image/x-icon" | "image/vnd.microsoft.icon") {
        return ".ico";
    }
    let file_name = file_name.unwrap_or("").trim();
    if let Some(captures) = FILENAME_EXTENSION.captures(file_name) {
        let dotted = format!(".{}", captures[1].to_ascii_lowercase());
        if let Some(safe) = SAFE_IMAGE_FILE_EXTENSIONS
            .iter()
            .find(|candidate| **candidate == dotted)
        {
            return safe;
        }
    }
    ".bin"
}

/// Upstream `PENDING_ATTACHMENT_THREAD_SEGMENT`: the thread segment of uploads not yet claimed.
pub const PENDING_ATTACHMENT_THREAD_SEGMENT: &str = "pending";
const THREAD_SEGMENT_MAX_CHARS: usize = 80;

static NON_SEGMENT_CHARACTERS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-z0-9_-]+").expect("valid regex"));
static REPEATED_DASHES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-+").expect("valid regex"));

/// `toSafeThreadAttachmentSegment`: a directory-safe slug of a thread id, or
/// `None` when nothing usable is left.
pub fn to_safe_thread_attachment_segment(thread_id: &str) -> Option<String> {
    let lowered = js_trim(thread_id).to_lowercase();
    let segment = NON_SEGMENT_CHARACTERS.replace_all(&lowered, "-");
    let segment = REPEATED_DASHES.replace_all(&segment, "-");
    let segment = segment.trim_matches(|c| c == '-' || c == '_');
    let segment: String = segment.chars().take(THREAD_SEGMENT_MAX_CHARS).collect();
    let segment = segment.trim_end_matches(['-', '_']);
    if segment.is_empty() {
        None
    } else if segment == PENDING_ATTACHMENT_THREAD_SEGMENT {
        Some("_pending".to_owned())
    } else {
        Some(segment.to_owned())
    }
}

/// `attachmentRelativePath`: `<id><extension>`, or `None` for unknown kinds.
pub fn attachment_relative_path(attachment: &ChatAttachment) -> Option<String> {
    match attachment {
        ChatAttachment::Image(image) => Some(format!(
            "{}{}",
            image.id.as_str(),
            infer_image_extension(image.mime_type.as_str(), Some(image.name.as_str()))
        )),
        ChatAttachment::File(file) => Some(format!(
            "{}{}",
            file.id.as_str(),
            attachment_file_extension(file.name.as_str())
        )),
        ChatAttachment::Unknown(_) => None,
    }
}

/// `resolveAttachmentPath`: where an attachment's bytes live, if anywhere
/// sensible. It does not check that the file exists.
pub fn resolve_attachment_path(
    attachments_dir: &Path,
    attachment: &ChatAttachment,
) -> Option<String> {
    resolve_attachment_relative_path(attachments_dir, &attachment_relative_path(attachment)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn normalization_strips_leading_separators_and_rejects_escapes() {
        assert_eq!(
            normalize_attachment_relative_path("//a/./b\\c.png").as_deref(),
            Some("a/b/c.png")
        );
        assert_eq!(normalize_attachment_relative_path("../secret"), None);
        assert_eq!(normalize_attachment_relative_path("a/../../secret"), None);
        assert_eq!(normalize_attachment_relative_path("..hidden"), None);
        assert_eq!(normalize_attachment_relative_path("/"), None);
        assert_eq!(normalize_attachment_relative_path("a\0b"), None);
    }

    #[test]
    fn resolution_stays_under_the_attachments_directory() {
        let dir = Path::new("/state/attachments");
        assert_eq!(
            resolve_attachment_relative_path(dir, "thread-1/img.png").as_deref(),
            Some("/state/attachments/thread-1/img.png")
        );
        assert_eq!(
            resolve_attachment_relative_path(dir, "a/../b.png").as_deref(),
            Some("/state/attachments/b.png")
        );
        assert_eq!(resolve_attachment_relative_path(dir, "../x.png"), None);
    }

    #[test]
    fn file_extension_keeps_plain_lowercase_extensions_only() {
        assert_eq!(attachment_file_extension("Notes.TXT"), ".txt");
        assert_eq!(attachment_file_extension("archive.part"), ".bin");
        assert_eq!(attachment_file_extension(".bashrc"), ".bin");
        assert_eq!(attachment_file_extension("no-extension"), ".bin");
        assert_eq!(attachment_file_extension("weird.a-b"), ".bin");
        assert_eq!(attachment_file_extension("dir.v2/file"), ".bin");
    }

    #[test]
    fn thread_segments_are_directory_safe() {
        assert_eq!(
            to_safe_thread_attachment_segment("  Thread One!! ").as_deref(),
            Some("thread-one")
        );
        assert_eq!(
            to_safe_thread_attachment_segment("../../x").as_deref(),
            Some("x")
        );
        assert_eq!(to_safe_thread_attachment_segment("!!!"), None);
        assert_eq!(
            to_safe_thread_attachment_segment("Pending").as_deref(),
            Some("_pending")
        );
    }

    #[test]
    fn image_extension_prefers_the_mime_type_then_a_safe_file_name() {
        assert_eq!(infer_image_extension("image/JPEG", Some("x.gif")), ".jpg");
        assert_eq!(
            infer_image_extension("application/octet-stream", Some("shot.PNG")),
            ".png"
        );
        assert_eq!(infer_image_extension("image/x-icon", None), ".ico");
        assert_eq!(
            infer_image_extension("image/apng", Some("anim.exe")),
            ".bin"
        );
        assert_eq!(
            infer_image_extension("image/apng", Some("anim.apng")),
            ".bin"
        );
    }
}
