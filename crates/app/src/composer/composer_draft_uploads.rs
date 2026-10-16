//! Port of upstream/apps/web/src/lib/composerDraftUploads.ts
//!
//! Releases the uploads a draft still holds when the draft goes away.
//! Upstream reads `useComposerDraftStore.getState()` and calls
//! `releaseDraftAttachments`; here the draft store's read side is
//! [`DraftUploadSource`] and the queue's release is [`DraftAttachmentReleaser`]
//! (implemented by [`AttachmentUploadQueue`]).

use crate::composer::attachment_upload_queue::{
    AttachmentUploadQueue, ComposerThreadTarget, ComposerUploadAttachment,
};
use crate::composer::draft_store::ComposerThreadDraft;

/// `releaseDraftAttachments`.
pub trait DraftAttachmentReleaser {
    fn release_draft_attachments(&self, attachments: &[ComposerUploadAttachment]);
}

impl DraftAttachmentReleaser for AttachmentUploadQueue {
    fn release_draft_attachments(&self, attachments: &[ComposerUploadAttachment]) {
        AttachmentUploadQueue::release_draft_attachments(self, attachments);
    }
}

/// A draft session (`draftThreadsByThreadKey[key]`): where a new-thread draft
/// will be created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftSessionRef {
    pub draft_key: String,
    pub environment_id: String,
    pub project_id: String,
}

/// The composer draft store's reads these functions need.
pub trait DraftUploadSource {
    /// `getComposerDraft(target)`.
    fn composer_draft(&self, target: &ComposerThreadTarget) -> Option<ComposerThreadDraft>;
    /// `draftThreadsByThreadKey`, in store order.
    fn draft_sessions(&self) -> Vec<DraftSessionRef>;
    /// `draftsByThreadKey[key]`. For a thread draft the key is
    /// `scopedThreadKey(threadRef)`.
    fn draft_by_key(&self, key: &str) -> Option<ComposerThreadDraft>;
}

/// `[...draft.images, ...draft.files]` as the queue's attachment type.
pub fn draft_upload_attachments(draft: &ComposerThreadDraft) -> Vec<ComposerUploadAttachment> {
    let images = draft
        .images
        .iter()
        .map(|image| ComposerUploadAttachment::Image {
            id: image.id.clone(),
            name: image.name.clone(),
            mime_type: image.mime_type.clone(),
            size_bytes: image.size_bytes,
            bytes: Some(image.data.as_ref().clone()),
            source: None,
        });
    let files = draft
        .files
        .iter()
        .map(|file| ComposerUploadAttachment::File {
            id: file.id.clone(),
            name: file.name.clone(),
            mime_type: file.mime_type.clone(),
            size_bytes: file.size_bytes,
            bytes: file.data.as_ref().map(|data| data.as_ref().clone()),
            uploaded_attachment_id: file.uploaded_attachment_id.clone(),
            upload_environment_id: file.upload_environment_id.clone(),
            source: None,
        });
    images.chain(files).collect()
}

/// `releaseComposerDraftUploads`.
pub fn release_composer_draft_uploads(
    source: &dyn DraftUploadSource,
    releaser: &dyn DraftAttachmentReleaser,
    target: &ComposerThreadTarget,
) {
    if let Some(draft) = source.composer_draft(target) {
        releaser.release_draft_attachments(&draft_upload_attachments(&draft));
    }
}

/// `releaseProjectDraftUploads`: every upload a deleted project's drafts still
/// hold. Draft-thread sessions carry their project ref, but drafts on the
/// project's real threads live under scoped thread keys with no project in
/// the key, so the caller passes the project's thread refs alongside as
/// `(environment_id, thread_id)` pairs.
pub fn release_project_draft_uploads(
    source: &dyn DraftUploadSource,
    releaser: &dyn DraftAttachmentReleaser,
    environment_id: &str,
    project_id: &str,
    project_thread_refs: &[(String, String)],
) {
    for session in source.draft_sessions() {
        if session.environment_id == environment_id && session.project_id == project_id {
            let attachments = source
                .draft_by_key(&session.draft_key)
                .map(|draft| draft_upload_attachments(&draft))
                .unwrap_or_default();
            releaser.release_draft_attachments(&attachments);
        }
    }
    for (thread_environment_id, thread_id) in project_thread_refs {
        let key = format!("{thread_environment_id}:{thread_id}");
        if let Some(draft) = source.draft_by_key(&key) {
            releaser.release_draft_attachments(&draft_upload_attachments(&draft));
        }
    }
}

#[cfg(test)]
#[path = "composer_draft_uploads_tests.rs"]
mod tests;
