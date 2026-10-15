//! Port of upstream/packages/shared/src/filePreview.ts
//!
//! Only `isWorkspaceImagePreviewPath` (with `WORKSPACE_IMAGE_PREVIEW_EXTENSIONS`
//! and `hasPreviewExtension`) is ported; the browser, video, audio and
//! attachment preview classification belongs to cut or not-yet-ported
//! surfaces.

pub const WORKSPACE_IMAGE_PREVIEW_EXTENSIONS: [&str; 8] = [
    ".avif", ".gif", ".ico", ".jpeg", ".jpg", ".png", ".svg", ".webp",
];

fn has_preview_extension(path: &str, extensions: &[&str]) -> bool {
    let literal_path = path.to_lowercase();
    extensions
        .iter()
        .any(|extension| literal_path.ends_with(extension))
}

pub fn is_workspace_image_preview_path(path: &str) -> bool {
    has_preview_extension(path, &WORKSPACE_IMAGE_PREVIEW_EXTENSIONS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_image_preview_paths() {
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
        }
    }

    /// Upstream asserts these against `isWorkspacePreviewEntryPath` (image or
    /// browser document); rejecting the entry implies rejecting the image.
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
            assert!(!is_workspace_image_preview_path(path), "{path}");
        }
    }
}
