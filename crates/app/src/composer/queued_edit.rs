//! Port of upstream/apps/web/src/components/chat/queuedMessageEdit.ts: what
//! happens to an edited queued message when its run leaves the queue, and how
//! its attachments are rebuilt for saving.

use crate::composer::draft_store::{
    ComposerDraftStore, ComposerFileAttachment, ComposerImageAttachment,
    composer_draft_has_user_content,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuedEditRecovery {
    Kept,
    Discarded,
    Clean,
}

/// `recoverQueuedMessageEdit`: keep an unsaved edit when its queued run starts
/// or is removed remotely, unless the thread already holds its own draft.
pub fn recover_queued_message_edit(
    store: &mut ComposerDraftStore,
    edit_key: &str,
    thread_key: &str,
    original_text: &str,
) -> QueuedEditRecovery {
    let dirty = store.get_composer_draft(edit_key).is_some_and(|draft| {
        draft.prompt != original_text || !draft.images.is_empty() || !draft.files.is_empty()
    });
    if dirty && !composer_draft_has_user_content(store.get_composer_draft(thread_key)) {
        store.move_composer_prompt_and_images(edit_key, thread_key);
        return QueuedEditRecovery::Kept;
    }
    store.clear_composer_content(edit_key);
    if dirty {
        QueuedEditRecovery::Discarded
    } else {
        QueuedEditRecovery::Clean
    }
}

/// A saved attachment of a queued message.
#[derive(Debug, Clone, PartialEq)]
pub enum QueuedAttachment {
    Image {
        id: String,
        name: String,
        mime_type: String,
        size_bytes: u64,
        /// `data:` URL of the image bytes.
        data_url: String,
    },
    File {
        id: String,
        name: String,
        mime_type: String,
        size_bytes: u64,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("Retry or remove failed uploads before saving.")]
pub struct FailedUpload;

/// `prepareQueuedEditAttachments`: generic files need upload references;
/// images also support the inline `data:` transport.
pub fn prepare_queued_edit_attachments(
    existing: &[QueuedAttachment],
    images: &[ComposerImageAttachment],
    files: &[ComposerFileAttachment],
    upload_files: impl FnOnce(&[ComposerFileAttachment]) -> Vec<QueuedAttachment>,
    read_image: impl Fn(&ComposerImageAttachment) -> String,
) -> Result<Vec<QueuedAttachment>, FailedUpload> {
    let uploaded = if files.is_empty() {
        Vec::new()
    } else {
        upload_files(files)
    };
    if uploaded.len() != files.len() {
        return Err(FailedUpload);
    }
    let mut attachments = existing.to_vec();
    attachments.extend(images.iter().map(|image| QueuedAttachment::Image {
        id: image.id.clone(),
        name: image.name.clone(),
        mime_type: image.mime_type.clone(),
        size_bytes: image.size_bytes,
        data_url: read_image(image),
    }));
    attachments.extend(uploaded);
    Ok(attachments)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDIT: &str = "queued-edit:test";
    const THREAD: &str = "remote-environment:thread:edit";

    fn file() -> ComposerFileAttachment {
        ComposerFileAttachment {
            id: "file:report".into(),
            name: "report.pdf".into(),
            mime_type: "application/pdf".into(),
            size_bytes: 6,
            data: Some(std::sync::Arc::new(b"report".to_vec())),
            uploaded_attachment_id: None,
            upload_environment_id: None,
        }
    }

    fn image() -> ComposerImageAttachment {
        ComposerImageAttachment::new("image:screen", "screen.png", "image/png", b"image".to_vec())
    }

    fn uploaded() -> QueuedAttachment {
        QueuedAttachment::File {
            id: "upload:report".into(),
            name: "report.pdf".into(),
            mime_type: "application/pdf".into(),
            size_bytes: 6,
        }
    }

    #[test]
    fn preserves_a_generic_file_in_file_only_and_mixed_saves() {
        for images in [Vec::new(), vec![image()]] {
            let attachments = prepare_queued_edit_attachments(
                &[],
                &images,
                &[file()],
                |_| vec![uploaded()],
                |_| "data:image/png;base64,aW1hZ2U=".into(),
            )
            .unwrap();
            assert_eq!(attachments.last(), Some(&uploaded()));
            assert_eq!(attachments.len(), images.len() + 1);
            if !images.is_empty() {
                assert!(
                    matches!(&attachments[0], QueuedAttachment::Image { data_url, .. } if data_url == "data:image/png;base64,aW1hZ2U=")
                );
            }
        }
    }

    #[test]
    fn retains_saved_attachments_alongside_newly_uploaded_files() {
        let saved = QueuedAttachment::File {
            id: "saved:earlier".into(),
            name: "report.pdf".into(),
            mime_type: "application/pdf".into(),
            size_bytes: 6,
        };
        let attachments = prepare_queued_edit_attachments(
            std::slice::from_ref(&saved),
            &[],
            &[file()],
            |_| vec![uploaded()],
            |_| "unused".into(),
        )
        .unwrap();
        assert_eq!(attachments, [saved, uploaded()]);
    }

    #[test]
    fn fails_a_save_instead_of_dropping_a_file_whose_upload_is_missing() {
        let result = prepare_queued_edit_attachments(
            &[],
            &[image()],
            &[file()],
            |_| Vec::new(),
            |_| "image".into(),
        );
        assert_eq!(result, Err(FailedUpload));
        assert!(result.unwrap_err().to_string().contains("Retry or remove"));
    }

    #[test]
    fn keeps_file_only_edits_when_another_client_starts_the_queued_run() {
        let mut store = ComposerDraftStore::new();
        store.set_prompt(EDIT, "Original message");
        store.add_files(EDIT, vec![file()], Default::default());
        assert_eq!(
            recover_queued_message_edit(&mut store, EDIT, THREAD, "Original message"),
            QueuedEditRecovery::Kept
        );
        let draft = store.get_composer_draft(THREAD).unwrap();
        assert_eq!(draft.prompt, "Original message");
        assert_eq!(draft.files, [file()]);
        assert!(store.get_composer_draft(EDIT).is_none());
    }

    #[test]
    fn preserves_uploaded_file_references_when_a_remote_queue_advance_interrupts_the_edit() {
        let mut store = ComposerDraftStore::new();
        let reference = ComposerFileAttachment {
            data: None,
            uploaded_attachment_id: Some("upload:report".into()),
            upload_environment_id: Some("remote-environment".into()),
            ..file()
        };
        store.add_files(EDIT, vec![reference.clone()], Default::default());
        assert_eq!(
            recover_queued_message_edit(&mut store, EDIT, THREAD, ""),
            QueuedEditRecovery::Kept
        );
        assert_eq!(store.get_composer_draft(THREAD).unwrap().files, [reference]);
    }

    #[test]
    fn does_not_overwrite_a_separate_draft_when_the_queued_run_leaves() {
        let mut store = ComposerDraftStore::new();
        store.set_prompt(THREAD, "Separate draft");
        store.add_files(EDIT, vec![file()], Default::default());
        assert_eq!(
            recover_queued_message_edit(&mut store, EDIT, THREAD, ""),
            QueuedEditRecovery::Discarded
        );
        assert_eq!(
            store.get_composer_draft(THREAD).unwrap().prompt,
            "Separate draft"
        );
        assert!(store.get_composer_draft(EDIT).is_none());
    }

    #[test]
    fn an_untouched_edit_is_clean() {
        let mut store = ComposerDraftStore::new();
        store.set_prompt(EDIT, "Same");
        assert_eq!(
            recover_queued_message_edit(&mut store, EDIT, THREAD, "Same"),
            QueuedEditRecovery::Clean
        );
        assert!(store.get_composer_draft(EDIT).is_none());
    }
}
