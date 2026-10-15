//! Port of upstream/packages/shared/src/image.ts

use std::collections::HashSet;

use turin_contracts::base_schemas::js_trim;

/// Only the types a provider turn accepts. A picture the provider would reject is still a
/// file, so widening this map would promote attachments the send path cannot carry.
/// Mirrors `PROVIDER_SEND_TURN_SUPPORTED_IMAGE_MIME_TYPES`.
const IMAGE_MIME_TYPE_BY_EXTENSION: &[(&str, &str)] = &[
    ("gif", "image/gif"),
    ("jpeg", "image/jpeg"),
    ("jpg", "image/jpeg"),
    ("png", "image/png"),
    ("webp", "image/webp"),
];

fn supported_image_mime_types() -> HashSet<&'static str> {
    IMAGE_MIME_TYPE_BY_EXTENSION
        .iter()
        .map(|(_, mime)| *mime)
        .collect()
}

/// What a picker writes when it did not recognize the file; the name is better evidence.
pub fn generic_mime_types() -> &'static HashSet<&'static str> {
    use std::sync::OnceLock;
    static TYPES: OnceLock<HashSet<&'static str>> = OnceLock::new();
    TYPES.get_or_init(|| {
        HashSet::from([
            "application/octet-stream",
            "binary/octet-stream",
            "application/unknown",
        ])
    })
}

pub fn is_generic_mime_type(mime_type: &str) -> bool {
    generic_mime_types().contains(mime_type)
}

/// Recognizes pictures even when the picker omitted their MIME type. A picture chosen through
/// the document picker arrives typed as a plain file, so what it *is* has to come from its own
/// name and type rather than from which picker produced it.
pub fn image_mime_type(name: &str, mime_type: &str) -> Option<&'static str> {
    let mime_head = mime_type.split(';').next().unwrap_or("");
    let mime_type = js_trim(mime_head).to_ascii_lowercase();
    let supported = supported_image_mime_types();
    if supported.contains(mime_type.as_str()) {
        return IMAGE_MIME_TYPE_BY_EXTENSION
            .iter()
            .find(|(_, mime)| *mime == mime_type)
            .map(|(_, mime)| *mime);
    }
    // A declared but unsupported image type stays a file: the send path cannot carry it.
    if mime_type.starts_with("image/") {
        return None;
    }
    // The name is only evidence when nothing recorded what this is. A definite type already
    // answers the question, and a `.png` on a PDF must not override it.
    if !mime_type.is_empty() && !is_generic_mime_type(&mime_type) {
        return None;
    }
    let dot_index = name.rfind('.')?;
    let extension = js_trim(&name[dot_index + 1..]).to_ascii_lowercase();
    IMAGE_MIME_TYPE_BY_EXTENSION
        .iter()
        .find(|(ext, _)| *ext == extension)
        .map(|(_, mime)| *mime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_a_picture_the_file_picker_left_untyped() {
        assert_eq!(image_mime_type("IMG_4997.PNG", ""), Some("image/png"));
        assert_eq!(
            image_mime_type("shot.jpg", "application/octet-stream"),
            Some("image/jpeg")
        );
    }

    #[test]
    fn trusts_a_declared_supported_type() {
        assert_eq!(
            image_mime_type("no-extension", "image/webp"),
            Some("image/webp")
        );
        assert_eq!(
            image_mime_type("x.png", "image/png; charset=binary"),
            Some("image/png")
        );
    }

    #[test]
    fn leaves_images_the_provider_cannot_accept_as_files() {
        assert_eq!(image_mime_type("scan.heic", "image/heic"), None);
        assert_eq!(image_mime_type("logo.svg", "image/svg+xml"), None);
    }

    #[test]
    fn leaves_documents_and_videos_alone() {
        assert_eq!(image_mime_type("notes.txt", "text/plain"), None);
        assert_eq!(image_mime_type("clip.mp4", "video/mp4"), None);
        assert_eq!(image_mime_type("noext", ""), None);
    }

    #[test]
    fn a_declared_non_image_type_wins_over_a_misleading_extension() {
        assert_eq!(image_mime_type("report.png", "application/pdf"), None);
        assert_eq!(image_mime_type("archive.jpg", "application/zip"), None);
        assert_eq!(image_mime_type("clip.png", "video/mp4"), None);
        assert_eq!(
            image_mime_type("IMG_4996.PNG", "application/octet-stream"),
            Some("image/png")
        );
        assert_eq!(image_mime_type("1000000020.png", ""), Some("image/png"));
    }
}
