//! Port of upstream/apps/web/src/components/chat/composerAttachmentFiles.ts
//! plus `videoMimeType` (shared/video.ts), `isHeicImageFile`
//! (lib/imageCompression.ts) and the attachment-size helpers of
//! client-runtime/state/attachments.ts.
//!
//! A `File` handle is [`PickedFile`]: name, mime type and size (the bytes live
//! with the draft attachment once it is accepted).

use turin_contracts::chat_attachment::{
    PROVIDER_SEND_TURN_MAX_FILE_BYTES, is_provider_send_turn_supported_image_mime_type,
};

use crate::composer::draft_store::{ComposerFileAttachment, ComposerImageAttachment};

/// The `Pick<File, "name" | "type">` the classifiers read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickedFile {
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
}

impl PickedFile {
    pub fn new(name: &str, mime_type: &str, size_bytes: u64) -> Self {
        Self {
            name: name.to_owned(),
            mime_type: mime_type.to_owned(),
            size_bytes,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerAttachmentFileKind {
    Image,
    File,
    UnsupportedImage,
}

/// What a picker writes when it did not recognize the file.
fn is_generic_mime_type(mime_type: &str) -> bool {
    matches!(mime_type, "application/octet-stream" | "binary/octet-stream" | "application/unknown")
}

fn extension_of(name: &str) -> Option<String> {
    let dot = name.rfind('.')?;
    Some(name[dot + 1..].to_lowercase())
}

/// `inferImageMimeTypeFromName`: maps the extension to a provider-supported
/// image type so a plain `photo.jpg` still lands on the image path.
pub fn infer_image_mime_type_from_name(name: &str) -> Option<&'static str> {
    // `dotIndex <= 0`: a leading dot alone is not an extension.
    let dot = name.rfind('.')?;
    if dot == 0 {
        return None;
    }
    match extension_of(name)?.as_str() {
        "gif" => Some("image/gif"),
        "jpeg" | "jpg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn infer_image_mime_type_for_unknown_file(file: &PickedFile) -> Option<&'static str> {
    let mime_type = file.mime_type.to_lowercase();
    if !mime_type.is_empty() && mime_type != "application/octet-stream" {
        return None;
    }
    infer_image_mime_type_from_name(&file.name)
}

/// `isHeicImageFile`.
pub fn is_heic_image_file(file: &PickedFile) -> bool {
    let mime = file.mime_type.to_lowercase();
    if mime == "image/heic" || mime == "image/heif" {
        return true;
    }
    (mime.is_empty() || mime == "application/octet-stream")
        && matches!(extension_of(&file.name).as_deref(), Some("heic" | "heif"))
}

/// `normalizeComposerImageFileMimeType`: gives extension-recognized images a
/// concrete type before compression.
pub fn normalize_composer_image_file_mime_type(file: PickedFile) -> PickedFile {
    match infer_image_mime_type_for_unknown_file(&file) {
        Some(inferred) => PickedFile {
            mime_type: inferred.to_owned(),
            ..file
        },
        None => file,
    }
}

/// `classifyComposerAttachmentFile`.
pub fn classify_composer_attachment_file(file: &PickedFile) -> ComposerAttachmentFileKind {
    if is_heic_image_file(file) || infer_image_mime_type_for_unknown_file(file).is_some() {
        return ComposerAttachmentFileKind::Image;
    }
    if !file.mime_type.to_lowercase().starts_with("image/") {
        return ComposerAttachmentFileKind::File;
    }
    if is_provider_send_turn_supported_image_mime_type(&file.mime_type) {
        ComposerAttachmentFileKind::Image
    } else {
        ComposerAttachmentFileKind::UnsupportedImage
    }
}

/// `videoMimeType`: recognizes videos even when the picker omitted the type.
pub fn video_mime_type(name: &str, mime_type: &str) -> Option<String> {
    let mime = mime_type.split(';').next().unwrap_or("").trim().to_lowercase();
    if mime.starts_with("video/") {
        return Some(mime);
    }
    // The name is only evidence when nothing recorded what this is.
    if !mime.is_empty() && !is_generic_mime_type(&mime) {
        return None;
    }
    let by_extension = match extension_of(name)?.as_str() {
        "avi" => "video/x-msvideo",
        "m4v" | "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "ogv" => "video/ogg",
        "webm" => "video/webm",
        _ => return None,
    };
    name.contains('.').then(|| by_extension.to_owned())
}

/// `isVideoAttachment`.
pub fn is_video_attachment(name: &str, mime_type: &str) -> bool {
    video_mime_type(name, mime_type).is_some()
}

/// `isPreviewableComposerVideo`.
pub fn is_previewable_composer_video(file: &ComposerFileAttachment, environment_id: &str) -> bool {
    is_video_attachment(&file.name, &file.mime_type)
        && (file.has_bytes()
            || (file.uploaded_attachment_id.is_some() && file.upload_environment_id.as_deref() == Some(environment_id)))
}

/// `composerOtherFilesForPresentation`: non-media files without an inline
/// reference still need the legacy attachment row.
pub fn composer_other_files_for_presentation<'a>(
    files: &'a [ComposerFileAttachment],
    environment_id: &str,
    inline_file_ids: &std::collections::HashSet<String>,
) -> Vec<&'a ComposerFileAttachment> {
    files
        .iter()
        .filter(|file| !is_previewable_composer_video(file, environment_id) && !inline_file_ids.contains(&file.id))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileAttachmentCapabilityState {
    pub attachment_uploads_capability_known: bool,
    pub supports_attachment_uploads: bool,
    pub max_file_attachment_bytes: Option<u64>,
}

/// `clampFileAttachmentUploadBytes`.
pub fn clamp_file_attachment_upload_bytes(advertised_max_upload_bytes: u64) -> u64 {
    advertised_max_upload_bytes.min(PROVIDER_SEND_TURN_MAX_FILE_BYTES)
}

/// `formatAttachmentSize`: "3.2 MB" / "48 KB". Never shows "0 KB".
pub fn format_attachment_size(size_bytes: u64) -> String {
    if size_bytes >= 1024 * 1024 {
        let tenths = (size_bytes as f64 / (1024.0 * 1024.0) * 10.0).round() / 10.0;
        format!("{tenths:.1} MB")
    } else {
        format!("{} KB", size_bytes.div_ceil(1024).max(1))
    }
}

/// `fileAttachmentTooLargeMessage`.
pub fn file_attachment_too_large_message(name: &str, max_upload_bytes: u64) -> String {
    let size = if max_upload_bytes >= 1024 * 1024 && max_upload_bytes % (1024 * 1024) == 0 {
        format!("{} MB", max_upload_bytes / (1024 * 1024))
    } else if max_upload_bytes >= 1024 && max_upload_bytes % 1024 == 0 {
        format!("{} KB", max_upload_bytes / 1024)
    } else {
        format!("{max_upload_bytes} {}", if max_upload_bytes == 1 { "byte" } else { "bytes" })
    };
    format!("'{name}' exceeds the {size} attachment limit.")
}

/// `fileAttachmentStagingLimit`: byte limit for adding a generic file to the
/// local composer draft.
pub fn file_attachment_staging_limit(input: &FileAttachmentCapabilityState) -> Option<u64> {
    if !input.attachment_uploads_capability_known {
        return Some(PROVIDER_SEND_TURN_MAX_FILE_BYTES);
    }
    if !input.supports_attachment_uploads {
        return None;
    }
    input.max_file_attachment_bytes.map(clamp_file_attachment_upload_bytes)
}

/// `fileAttachmentCapabilityBlockReason`: why retained generic files cannot
/// send with the current server config.
pub fn file_attachment_capability_block_reason(
    input: &FileAttachmentCapabilityState,
    files: &[(String, u64)],
) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    if !input.attachment_uploads_capability_known {
        return Some("Waiting for the server before file attachments can send".to_owned());
    }
    let Some(max) = file_attachment_staging_limit(input) else {
        return Some("This server does not accept file attachments right now. Remove the files to send.".to_owned());
    };
    files
        .iter()
        .find(|(_, size)| *size > max)
        .map(|(name, _)| file_attachment_too_large_message(name, max))
}

/// A draft attachment of either kind.
pub enum DraftAttachment<'a> {
    Image(&'a ComposerImageAttachment),
    File(&'a ComposerFileAttachment),
}

/// `attachmentsToReleaseOnUploadCapabilityLoss`: only uploads not yet
/// persisted onto a draft file are torn down.
pub fn attachment_ids_to_release_on_upload_capability_loss(attachments: &[DraftAttachment<'_>]) -> Vec<String> {
    attachments
        .iter()
        .filter_map(|attachment| match attachment {
            DraftAttachment::Image(image) => Some(image.id.clone()),
            DraftAttachment::File(file) if file.uploaded_attachment_id.is_none() => Some(file.id.clone()),
            DraftAttachment::File(_) => None,
        })
        .collect()
}

/// `shouldHandleComposerAttachmentPaste`: whether a paste's files should be
/// claimed as composer attachments instead of falling through to the default
/// text paste.
pub fn should_handle_composer_attachment_paste(files: &[PickedFile], plain_text: &str) -> bool {
    if files.iter().any(|file| {
        matches!(
            classify_composer_attachment_file(file),
            ComposerAttachmentFileKind::Image | ComposerAttachmentFileKind::UnsupportedImage
        )
    }) {
        return true;
    }
    if !plain_text.is_empty() {
        return false;
    }
    files
        .iter()
        .any(|file| classify_composer_attachment_file(file) == ComposerAttachmentFileKind::File)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn file(name: &str, mime: &str) -> PickedFile {
        PickedFile::new(name, mime, 1)
    }

    fn composer_file(id: &str, name: &str, mime: &str) -> ComposerFileAttachment {
        ComposerFileAttachment {
            id: id.into(),
            name: name.into(),
            mime_type: mime.into(),
            size_bytes: 12,
            data: Some(std::sync::Arc::new(vec![1])),
            uploaded_attachment_id: None,
            upload_environment_id: None,
        }
    }

    #[test]
    fn keeps_inline_non_media_files_out_of_the_legacy_attachment_row() {
        let files = vec![
            composer_file("inline-file", "inline.txt", "text/plain"),
            composer_file("legacy-file", "legacy.zip", "application/zip"),
        ];
        let inline = HashSet::from(["inline-file".to_owned()]);
        let other = composer_other_files_for_presentation(&files, "env-1", &inline);
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].id, "legacy-file");
    }

    #[test]
    fn classifies_images_heic_and_unsupported_images() {
        use ComposerAttachmentFileKind::*;
        assert_eq!(classify_composer_attachment_file(&file("photo.png", "image/png")), Image);
        assert_eq!(classify_composer_attachment_file(&file("photo.heic", "")), Image);
        assert_eq!(classify_composer_attachment_file(&file("diagram.svg", "image/svg+xml")), UnsupportedImage);
        assert_eq!(classify_composer_attachment_file(&file("photo.tiff", "image/tiff")), UnsupportedImage);
        assert_eq!(classify_composer_attachment_file(&file("report.pdf", "application/pdf")), File);
    }

    #[test]
    fn falls_back_to_the_extension_when_an_image_arrives_without_a_mime_type() {
        use ComposerAttachmentFileKind::*;
        assert_eq!(classify_composer_attachment_file(&file("photo.jpg", "")), Image);
        assert_eq!(classify_composer_attachment_file(&file("shot.PNG", "")), Image);
        assert_eq!(classify_composer_attachment_file(&file("archive.zip", "")), File);
        assert_eq!(classify_composer_attachment_file(&file("no-extension", "")), File);
        assert_eq!(infer_image_mime_type_from_name("photo.jpg"), Some("image/jpeg"));
        assert_eq!(infer_image_mime_type_from_name("archive.zip"), None);
        assert_eq!(infer_image_mime_type_from_name(".png"), None);
    }

    #[test]
    fn infers_supported_image_types_from_octet_stream_files() {
        use ComposerAttachmentFileKind::*;
        let jpeg = file("photo.jpg", "application/octet-stream");
        let png = file("shot.PNG", "application/octet-stream");
        assert_eq!(classify_composer_attachment_file(&jpeg), Image);
        assert_eq!(classify_composer_attachment_file(&png), Image);
        assert_eq!(normalize_composer_image_file_mime_type(jpeg).mime_type, "image/jpeg");
        assert_eq!(normalize_composer_image_file_mime_type(png).mime_type, "image/png");
    }

    #[test]
    fn does_not_infer_images_for_unknown_extensions_or_specific_conflicting_types() {
        use ComposerAttachmentFileKind::*;
        let binary = file("archive.bin", "application/octet-stream");
        let unknown_document = file("report.pdf", "application/octet-stream");
        let document = file("photo.jpg", "application/pdf");
        let explicit = file("photo.jpg", "image/png");
        assert_eq!(classify_composer_attachment_file(&binary), File);
        assert_eq!(classify_composer_attachment_file(&unknown_document), File);
        assert_eq!(classify_composer_attachment_file(&document), File);
        assert_eq!(classify_composer_attachment_file(&explicit), Image);
        assert_eq!(normalize_composer_image_file_mime_type(binary.clone()), binary);
        assert_eq!(normalize_composer_image_file_mime_type(explicit.clone()), explicit);
    }

    #[test]
    fn paste_handling_claims_images_and_text_free_files() {
        assert!(!should_handle_composer_attachment_paste(&[file("clipboard.rtf", "application/rtf")], "Copied text"));
        for image in [file("diagram.svg", "image/svg+xml"), file("photo.tiff", "image/tiff"), file("photo.heic", "image/heic")] {
            assert!(should_handle_composer_attachment_paste(&[image], "Image caption"));
        }
        assert!(should_handle_composer_attachment_paste(&[file("report.pdf", "application/pdf")], ""));
        assert!(should_handle_composer_attachment_paste(&[file("empty.txt", "text/plain")], ""));
        assert!(!should_handle_composer_attachment_paste(&[], ""));
    }

    fn state(known: bool, supports: bool, max: Option<u64>) -> FileAttachmentCapabilityState {
        FileAttachmentCapabilityState {
            attachment_uploads_capability_known: known,
            supports_attachment_uploads: supports,
            max_file_attachment_bytes: max,
        }
    }

    #[test]
    fn staging_limit_follows_the_server_capability() {
        assert_eq!(file_attachment_staging_limit(&state(false, false, None)), Some(PROVIDER_SEND_TURN_MAX_FILE_BYTES));
        assert_eq!(
            file_attachment_capability_block_reason(
                &state(false, false, None),
                &[("pending.zip".into(), PROVIDER_SEND_TURN_MAX_FILE_BYTES)]
            )
            .as_deref(),
            Some("Waiting for the server before file attachments can send")
        );
        assert_eq!(file_attachment_staging_limit(&state(true, true, None)), None);
        assert_eq!(file_attachment_staging_limit(&state(true, false, Some(50 * 1024 * 1024))), None);
        let unsupported = "This server does not accept file attachments right now. Remove the files to send.";
        let files = [("report.pdf".to_owned(), 1024)];
        assert_eq!(file_attachment_capability_block_reason(&state(true, true, None), &files).as_deref(), Some(unsupported));
        assert_eq!(
            file_attachment_capability_block_reason(&state(true, false, Some(50 * 1024 * 1024)), &files).as_deref(),
            Some(unsupported)
        );
        assert_eq!(
            file_attachment_capability_block_reason(&state(true, true, Some(1024 * 1024)), &[("large.zip".into(), 2 * 1024 * 1024)])
                .as_deref(),
            Some("'large.zip' exceeds the 1 MB attachment limit.")
        );
        assert_eq!(file_attachment_staging_limit(&state(true, true, Some(1024 * 1024))), Some(1024 * 1024));
        assert_eq!(
            file_attachment_staging_limit(&state(true, true, Some(PROVIDER_SEND_TURN_MAX_FILE_BYTES * 2))),
            Some(PROVIDER_SEND_TURN_MAX_FILE_BYTES)
        );
        assert_eq!(file_attachment_capability_block_reason(&state(true, true, Some(50 * 1024 * 1024)), &files), None);
        assert_eq!(file_attachment_capability_block_reason(&state(true, false, None), &[]), None);
        assert_eq!(file_attachment_capability_block_reason(&state(false, false, None), &[]), None);
    }

    #[test]
    fn keeps_draft_persisted_file_uploads_when_the_upload_capability_flips_off() {
        let image = ComposerImageAttachment::new("image-1", "photo.png", "image/png", vec![1, 2, 3]);
        let uploading = composer_file("file-uploading", "fresh.pdf", "application/pdf");
        let hydrated = ComposerFileAttachment {
            data: None,
            uploaded_attachment_id: Some("pending-report-pdf".into()),
            upload_environment_id: Some("environment-1".into()),
            ..composer_file("file-hydrated", "report.pdf", "application/pdf")
        };
        let uploaded_local = ComposerFileAttachment {
            uploaded_attachment_id: Some("pending-fresh-pdf".into()),
            upload_environment_id: Some("environment-1".into()),
            ..composer_file("file-uploaded-local", "fresh.pdf", "application/pdf")
        };
        let released = attachment_ids_to_release_on_upload_capability_loss(&[
            DraftAttachment::Image(&image),
            DraftAttachment::File(&uploading),
            DraftAttachment::File(&hydrated),
            DraftAttachment::File(&uploaded_local),
        ]);
        assert_eq!(released, ["image-1", "file-uploading"]);
    }

    #[test]
    fn restored_videos_stay_previewable_only_in_their_upload_environment() {
        let video = ComposerFileAttachment {
            data: None,
            uploaded_attachment_id: Some("uploaded-video-1".into()),
            upload_environment_id: Some("environment-1".into()),
            ..composer_file("video-1", "clip.mp4", "video/mp4")
        };
        assert!(is_previewable_composer_video(&video, "environment-1"));
        assert!(!is_previewable_composer_video(&video, "environment-2"));
    }

    #[test]
    fn recognizes_common_video_extensions_without_a_mime_type() {
        for (name, expected) in [
            ("clip.mp4", "video/mp4"),
            ("clip.mov", "video/quicktime"),
            ("clip.webm", "video/webm"),
            ("clip.m4v", "video/mp4"),
            ("clip.mkv", "video/x-matroska"),
            ("clip.avi", "video/x-msvideo"),
            ("clip.ogv", "video/ogg"),
        ] {
            assert_eq!(video_mime_type(name, "application/octet-stream").as_deref(), Some(expected));
            assert!(is_video_attachment(name, "application/octet-stream"));
        }
        assert_eq!(video_mime_type("clip.mp4", "application/pdf"), None);
    }

    #[test]
    fn formats_attachment_sizes() {
        assert_eq!(format_attachment_size(0), "1 KB");
        assert_eq!(format_attachment_size(48 * 1024), "48 KB");
        assert_eq!(format_attachment_size(3_355_443), "3.2 MB");
        assert_eq!(file_attachment_too_large_message("a.zip", 1), "'a.zip' exceeds the 1 byte attachment limit.");
        assert_eq!(file_attachment_too_large_message("a.zip", 2048), "'a.zip' exceeds the 2 KB attachment limit.");
    }
}
