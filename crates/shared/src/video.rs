//! Port of upstream/packages/shared/src/video.ts

use crate::image::is_generic_mime_type;
use turin_contracts::base_schemas::js_trim;

const VIDEO_MIME_TYPE_BY_EXTENSION: &[(&str, &str)] = &[
    ("avi", "video/x-msvideo"),
    ("m4v", "video/mp4"),
    ("mkv", "video/x-matroska"),
    ("mov", "video/quicktime"),
    ("mp4", "video/mp4"),
    ("ogv", "video/ogg"),
    ("webm", "video/webm"),
];

pub fn video_file_extensions() -> Vec<&'static str> {
    VIDEO_MIME_TYPE_BY_EXTENSION
        .iter()
        .map(|(ext, _)| *ext)
        .collect()
}

/// Recognizes videos even when the file picker omitted their MIME type.
///
/// A declared `video/*` type is returned as given (parameters stripped, lowercased),
/// including types that are not in the extension map.
pub fn video_mime_type(name: &str, mime_type: &str) -> Option<String> {
    let mime_head = mime_type.split(';').next().unwrap_or("");
    let mime_type = js_trim(mime_head).to_ascii_lowercase();
    if mime_type.starts_with("video/") {
        return Some(mime_type);
    }
    // The name is only evidence when nothing recorded what this is. A definite type already
    // answers the question, and a `.mp4` on a PDF must not override it.
    if !mime_type.is_empty() && !is_generic_mime_type(&mime_type) {
        return None;
    }
    let dot_index = name.rfind('.')?;
    let extension = name[dot_index + 1..].to_ascii_lowercase();
    VIDEO_MIME_TYPE_BY_EXTENSION
        .iter()
        .find(|(ext, _)| *ext == extension)
        .map(|(_, mime)| (*mime).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_a_saved_video_with_a_generic_picker_mime_type() {
        assert_eq!(
            video_mime_type("Recording.MOV", "application/octet-stream"),
            Some("video/quicktime".to_string())
        );
    }

    #[test]
    fn keeps_an_explicit_video_mime_type_authoritative() {
        assert_eq!(
            video_mime_type("recording.mp4", " VIDEO/WebM; codecs=vp9 "),
            Some("video/webm".to_string())
        );
    }

    #[test]
    fn does_not_mistake_non_videos() {
        for name in ["README", "report.pdf", "file.constructor", "file.__proto__"] {
            assert_eq!(
                video_mime_type(name, "application/octet-stream"),
                None,
                "{name}"
            );
        }
    }

    #[test]
    fn keeps_a_declared_non_video_mime_type() {
        assert_eq!(video_mime_type("recording.mp4", "application/pdf"), None);
        assert_eq!(video_mime_type("recording.mp4", "image/png"), None);
    }
}
