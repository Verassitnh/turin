//! Taking attachments into the composer: reading picked, dropped and pasted
//! files, and the limits and messages upstream's `addComposerFiles` applies
//! (ChatComposer.tsx `addComposerAttachments`).
//!
//! Nothing here touches the window. [`load_paths`] reads files (call it on a
//! background executor), [`plan_staging`] decides what the draft may take, and
//! [`prepare_image`] downscales an image that exceeds the wire cap.

use std::path::{Path, PathBuf};

use turin_contracts::chat_attachment::{
    PROVIDER_SEND_TURN_MAX_ATTACHMENTS, PROVIDER_SEND_TURN_MAX_FILE_BYTES,
    PROVIDER_SEND_TURN_MAX_IMAGE_BYTES,
};

use crate::composer::attachment_files::{
    ComposerAttachmentFileKind, FileAttachmentCapabilityState, PickedFile,
    classify_composer_attachment_file, file_attachment_staging_limit,
    file_attachment_too_large_message, infer_image_mime_type_from_name,
    normalize_composer_image_file_mime_type,
};
use crate::state::helpers::image_compression::{
    ImageCompressionFailureReason, ImageCrateCodec, ImageFile, MAX_COMPRESSIBLE_SOURCE_BYTES,
    SipsHeicConverter, prepare_image_for_attachment,
};

/// Files read from disk, and what could not be taken.
#[derive(Debug, Default)]
pub struct Intake {
    pub loaded: Vec<(PickedFile, Vec<u8>)>,
    pub errors: Vec<String>,
}

impl Intake {
    /// Already-read attachments (clipboard images).
    pub fn from_loaded(loaded: Vec<(PickedFile, Vec<u8>)>) -> Self {
        Self {
            loaded,
            errors: Vec::new(),
        }
    }
}

pub fn guess_mime(name: &str) -> String {
    if let Some(mime) = infer_image_mime_type_from_name(name) {
        return mime.to_owned();
    }
    match name.rsplit('.').next().map(str::to_lowercase).as_deref() {
        Some("txt" | "md" | "log") => "text/plain",
        Some("json") => "application/json",
        Some("pdf") => "application/pdf",
        Some("csv") => "text/csv",
        _ => "application/octet-stream",
    }
    .to_owned()
}

fn unreadable(name: &str) -> String {
    format!("'{name}' is empty or could not be read.")
}

/// Reads one file for attachment. The size is checked before the bytes are
/// read, so an enormous file is refused without being loaded.
pub fn load_path(path: &Path) -> Result<(PickedFile, Vec<u8>), String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let metadata = std::fs::metadata(path).map_err(|_| unreadable(&name))?;
    if !metadata.is_file() {
        return Err(format!("'{name}' is not a file and cannot be attached."));
    }
    let probe = PickedFile::new(&name, &guess_mime(&name), metadata.len());
    match classify_composer_attachment_file(&probe) {
        // Rejected later with its own message; no need to read it.
        ComposerAttachmentFileKind::UnsupportedImage => return Ok((probe, Vec::new())),
        ComposerAttachmentFileKind::Image if metadata.len() > MAX_COMPRESSIBLE_SOURCE_BYTES => {
            return Err(too_large_after_compression(&name));
        }
        ComposerAttachmentFileKind::File if metadata.len() > PROVIDER_SEND_TURN_MAX_FILE_BYTES => {
            return Err(file_attachment_too_large_message(
                &name,
                PROVIDER_SEND_TURN_MAX_FILE_BYTES,
            ));
        }
        _ => {}
    }
    let data = std::fs::read(path).map_err(|_| unreadable(&name))?;
    let picked = PickedFile::new(&name, &probe.mime_type, data.len() as u64);
    Ok((picked, data))
}

/// Reads every path, collecting what could not be read.
pub fn load_paths(paths: impl IntoIterator<Item = PathBuf>) -> Intake {
    let mut intake = Intake::default();
    for path in paths {
        match load_path(&path) {
            Ok(file) => intake.loaded.push(file),
            Err(error) => intake.errors.push(error),
        }
    }
    intake
}

/// What `addComposerFiles` lets into the draft.
#[derive(Debug, Default)]
pub struct Staged {
    pub images: Vec<(PickedFile, Vec<u8>)>,
    pub files: Vec<(PickedFile, Vec<u8>)>,
    pub errors: Vec<String>,
}

/// Validates `loaded` against the attachment count (`reserved` slots are
/// already taken) and the server's file capability. The last error wins in
/// the UI, as upstream's `setThreadError(error)` does.
pub fn plan_staging(
    loaded: Vec<(PickedFile, Vec<u8>)>,
    mut reserved: usize,
    capability: &FileAttachmentCapabilityState,
) -> Staged {
    let file_limit = file_attachment_staging_limit(capability);
    let mut staged = Staged::default();
    for (picked, data) in loaded {
        if reserved >= PROVIDER_SEND_TURN_MAX_ATTACHMENTS {
            staged.errors.push(format!(
                "You can attach up to {PROVIDER_SEND_TURN_MAX_ATTACHMENTS} files per message."
            ));
            continue;
        }
        match classify_composer_attachment_file(&picked) {
            ComposerAttachmentFileKind::UnsupportedImage => {
                staged.errors.push(format!(
                    "'{}' is not a supported image type. Attach GIF, HEIC, HEIF, JPEG, PNG, or WebP images.",
                    picked.name
                ));
                continue;
            }
            ComposerAttachmentFileKind::Image => {
                let picked = normalize_composer_image_file_mime_type(picked);
                staged.images.push((picked, data));
            }
            ComposerAttachmentFileKind::File => {
                let Some(limit) = file_limit else {
                    staged
                        .errors
                        .push("This server does not support file attachments.".to_owned());
                    continue;
                };
                if data.is_empty() {
                    staged.errors.push(unreadable(&picked.name));
                    continue;
                }
                if data.len() as u64 > limit {
                    staged
                        .errors
                        .push(file_attachment_too_large_message(&picked.name, limit));
                    continue;
                }
                staged.files.push((picked, data));
            }
        }
        reserved += 1;
    }
    staged
}

fn too_large_after_compression(name: &str) -> String {
    format!("'{name}' is too large to attach, even after compression.")
}

/// `prepareImageForAttachment` for one picked image: HEIC becomes JPEG and an
/// image over the wire cap is downscaled to fit. CPU work: call it off the UI
/// thread.
pub fn prepare_image(picked: PickedFile, data: Vec<u8>) -> Result<(PickedFile, Vec<u8>), String> {
    let file = ImageFile::new(&picked.name, &picked.mime_type, data);
    match prepare_image_for_attachment(
        &ImageCrateCodec,
        &SipsHeicConverter,
        &file,
        PROVIDER_SEND_TURN_MAX_IMAGE_BYTES,
    ) {
        Ok(prepared) => {
            let bytes = std::sync::Arc::try_unwrap(prepared.file.bytes)
                .unwrap_or_else(|shared| (*shared).clone());
            Ok((
                PickedFile::new(
                    &prepared.file.name,
                    &prepared.file.mime_type,
                    bytes.len() as u64,
                ),
                bytes,
            ))
        }
        Err(ImageCompressionFailureReason::Unreadable) => {
            Err(format!("'{}' could not be read as an image.", picked.name))
        }
        Err(ImageCompressionFailureReason::TooLarge) => {
            Err(too_large_after_compression(&picked.name))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(max: u64) -> FileAttachmentCapabilityState {
        FileAttachmentCapabilityState {
            attachment_uploads_capability_known: true,
            supports_attachment_uploads: true,
            max_file_attachment_bytes: Some(max),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("turin-intake-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_directory_and_a_missing_file_are_reported_not_dropped_silently() {
        let dir = scratch("dir");
        let intake = load_paths([dir.clone(), dir.join("missing.txt")]);
        assert!(intake.loaded.is_empty());
        assert_eq!(intake.errors.len(), 2, "{:?}", intake.errors);
        assert!(intake.errors[0].contains("is not a file"));
        assert_eq!(
            intake.errors[1],
            "'missing.txt' is empty or could not be read."
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_readable_file_loads_with_its_type() {
        let dir = scratch("ok");
        let path = dir.join("notes.md");
        std::fs::write(&path, "hello").unwrap();
        let intake = load_paths([path]);
        assert!(intake.errors.is_empty());
        let (picked, data) = &intake.loaded[0];
        assert_eq!((picked.name.as_str(), picked.size_bytes), ("notes.md", 5));
        assert_eq!(picked.mime_type, "text/plain");
        assert_eq!(data, b"hello");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn files_over_the_staging_limit_are_refused_with_upstreams_message() {
        let big = (
            PickedFile::new("big.bin", "application/octet-stream", 2048),
            vec![0u8; 2048],
        );
        let ok = (
            PickedFile::new("ok.bin", "application/octet-stream", 10),
            vec![0u8; 10],
        );
        let staged = plan_staging(vec![big, ok], 0, &known(1024));
        assert_eq!(staged.files.len(), 1);
        assert_eq!(
            staged.errors,
            vec!["'big.bin' exceeds the 1 KB attachment limit."]
        );
    }

    #[test]
    fn empty_files_and_servers_without_file_support_are_refused() {
        let empty = (PickedFile::new("e.txt", "text/plain", 0), Vec::new());
        let staged = plan_staging(vec![empty.clone()], 0, &known(1024));
        assert_eq!(
            staged.errors,
            vec!["'e.txt' is empty or could not be read."]
        );
        let unsupported = FileAttachmentCapabilityState {
            attachment_uploads_capability_known: true,
            supports_attachment_uploads: false,
            max_file_attachment_bytes: None,
        };
        let staged = plan_staging(
            vec![(PickedFile::new("a.txt", "text/plain", 1), vec![1])],
            0,
            &unsupported,
        );
        assert_eq!(
            staged.errors,
            vec!["This server does not support file attachments."]
        );
    }

    #[test]
    fn attachments_past_the_per_message_cap_are_reported() {
        let file = |name: &str| (PickedFile::new(name, "text/plain", 1), vec![1u8]);
        let staged = plan_staging(
            vec![file("a.txt"), file("b.txt")],
            PROVIDER_SEND_TURN_MAX_ATTACHMENTS - 1,
            &known(1024),
        );
        assert_eq!(staged.files.len(), 1);
        assert_eq!(
            staged.errors,
            vec!["You can attach up to 100 files per message."]
        );
    }

    #[test]
    fn unsupported_images_name_the_accepted_types() {
        let svgish = (PickedFile::new("x.bmp", "image/bmp", 1), vec![1u8]);
        let staged = plan_staging(vec![svgish], 0, &known(1024));
        assert!(staged.errors[0].starts_with("'x.bmp' is not a supported image type."));
    }

    #[test]
    fn an_oversized_image_that_cannot_be_decoded_is_reported() {
        let size = PROVIDER_SEND_TURN_MAX_IMAGE_BYTES as usize + 1;
        let error = prepare_image(
            PickedFile::new("bad.png", "image/png", size as u64),
            vec![7u8; size],
        )
        .unwrap_err();
        assert!(
            error == "'bad.png' could not be read as an image."
                || error == "'bad.png' is too large to attach, even after compression.",
            "{error}"
        );
    }

    #[test]
    fn an_image_within_the_cap_passes_through_untouched() {
        let (picked, data) =
            prepare_image(PickedFile::new("a.png", "image/png", 3), vec![1, 2, 3]).unwrap();
        assert_eq!((picked.name.as_str(), data), ("a.png", vec![1, 2, 3]));
    }
}
