//! Port of upstream/apps/web/src/lib/attachmentUploadState.ts
//!
//! Whether the composer may send, and the label on an in-flight upload.
//! The queue in `attachment_upload_queue` owns the states this reads.

/// A finished upload the composer can reference, and that a replacement
/// keeps around so a failed move can fall back to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyAttachmentUpload {
    pub environment_id: String,
    pub attachment_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttachmentUploadState {
    Uploading {
        environment_id: String,
        progress: f64,
        previous: Option<ReadyAttachmentUpload>,
    },
    Ready(ReadyAttachmentUpload),
    Failed {
        environment_id: String,
        reason: String,
        /// A pending upload this queue minted for the failed attempt.
        /// Never a draft's persisted attachment id: retry and release delete it.
        attachment_id: Option<String>,
        previous: Option<ReadyAttachmentUpload>,
    },
}

impl AttachmentUploadState {
    pub fn environment_id(&self) -> &str {
        match self {
            Self::Uploading { environment_id, .. } | Self::Failed { environment_id, .. } => {
                environment_id
            }
            Self::Ready(ready) => &ready.environment_id,
        }
    }

    pub fn is_ready_for(&self, environment_id: &str) -> bool {
        matches!(self, Self::Ready(ready) if ready.environment_id == environment_id)
    }

    pub fn previous(&self) -> Option<&ReadyAttachmentUpload> {
        match self {
            Self::Uploading { previous, .. } | Self::Failed { previous, .. } => previous.as_ref(),
            Self::Ready(_) => None,
        }
    }
}

/// Why Send is disabled, or `None` when every attachment is ready for `environment_id`.
pub fn attachment_upload_block_reason(
    image_ids: &[String],
    uploads_by_image_id: &std::collections::HashMap<String, AttachmentUploadState>,
    environment_id: &str,
) -> Option<String> {
    let mut pending = 0;
    let mut failed = 0;
    for image_id in image_ids {
        let upload = uploads_by_image_id.get(image_id);
        if matches!(upload, Some(AttachmentUploadState::Failed { environment_id: failed_env, .. }) if failed_env == environment_id)
        {
            failed += 1;
        } else if upload.is_none_or(|upload| !upload.is_ready_for(environment_id)) {
            pending += 1;
        }
    }
    if failed > 0 {
        return Some(
            if failed == 1 {
                "Retry or remove the failed attachment"
            } else {
                "Retry or remove the failed attachments"
            }
            .to_owned(),
        );
    }
    if pending > 0 {
        return Some(
            if pending == 1 {
                "Attachment still uploading"
            } else {
                "Attachments still uploading"
            }
            .to_owned(),
        );
    }
    None
}

pub fn format_attachment_upload_progress(progress: f64) -> String {
    let bounded = if progress.is_finite() {
        progress.clamp(0.0, 1.0)
    } else {
        0.0
    };
    format!("{}%", (bounded * 100.0).floor() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(environment_id: &str, attachment_id: &str) -> AttachmentUploadState {
        AttachmentUploadState::Ready(ReadyAttachmentUpload {
            environment_id: environment_id.to_owned(),
            attachment_id: attachment_id.to_owned(),
        })
    }

    #[test]
    fn allows_uploaded_images_from_the_active_environment() {
        let mut uploads = std::collections::HashMap::new();
        uploads.insert("image-1".to_owned(), ready("environment-1", "pending-1"));
        assert_eq!(
            attachment_upload_block_reason(&["image-1".to_owned()], &uploads, "environment-1"),
            None
        );
    }

    #[test]
    fn blocks_images_that_are_missing_or_uploading() {
        let mut uploads = std::collections::HashMap::new();
        uploads.insert(
            "image-1".to_owned(),
            AttachmentUploadState::Uploading {
                environment_id: "environment-1".to_owned(),
                progress: 0.5,
                previous: None,
            },
        );
        assert_eq!(
            attachment_upload_block_reason(
                &["image-1".to_owned(), "image-2".to_owned()],
                &uploads,
                "environment-1"
            ),
            Some("Attachments still uploading".to_owned())
        );
    }

    #[test]
    fn asks_the_user_to_retry_or_remove_failed_uploads() {
        let mut uploads = std::collections::HashMap::new();
        uploads.insert(
            "image-1".to_owned(),
            AttachmentUploadState::Failed {
                environment_id: "environment-1".to_owned(),
                reason: "Upload failed".to_owned(),
                attachment_id: None,
                previous: None,
            },
        );
        assert_eq!(
            attachment_upload_block_reason(&["image-1".to_owned()], &uploads, "environment-1"),
            Some("Retry or remove the failed attachment".to_owned())
        );
    }

    #[test]
    fn does_not_accept_an_upload_from_another_environment() {
        let mut uploads = std::collections::HashMap::new();
        uploads.insert("image-1".to_owned(), ready("environment-2", "pending-1"));
        assert_eq!(
            attachment_upload_block_reason(&["image-1".to_owned()], &uploads, "environment-1"),
            Some("Attachment still uploading".to_owned())
        );
    }

    #[test]
    fn formats_bounded_whole_percentages() {
        assert_eq!(format_attachment_upload_progress(0.429), "42%");
        assert_eq!(format_attachment_upload_progress(2.0), "100%");
        assert_eq!(format_attachment_upload_progress(f64::NAN), "0%");
    }
}
