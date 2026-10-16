//! Port of upstream/apps/web/src/questionAttachments.ts (+ test).
//!
//! A pending provider question keeps its attachments in a composer draft of
//! its own, keyed by environment, thread, request and question, so they stay
//! apart from the thread's normal draft and from the other questions. This is
//! the key scheme, the per-draft count of attachments still being prepared,
//! and the cleanup that releases a draft.
//!
//! Upstream's zustand store of preparation counts is
//! [`QuestionAttachmentPreparation`], owned by whoever owns the composer
//! draft store. `releaseDraftAttachments` (the upload queue) is the
//! [`DraftAttachmentRelease`] seam. Revoking `blob:` preview URLs is DOM-only
//! and drops out: Turin's attachments hold their bytes, not an object URL.

use std::collections::HashMap;

use turin_shared::js_string::encode_uri_component;

use crate::composer::draft_store::{
    ComposerDraftStore, ComposerFileAttachment, ComposerImageAttachment,
};

/// `encodeURIComponent(JSON.stringify(value))` of a string.
fn encoded_json_string(value: &str) -> String {
    encode_uri_component(&serde_json::to_string(value).expect("a string always serializes"))
}

pub fn question_attachment_draft_prefix(environment_id: &str, thread_id: &str) -> String {
    format!(
        "{}:question-{}-",
        encoded_json_string(environment_id),
        encoded_json_string(thread_id)
    )
}

/// `questionAttachmentDraftId`: the `DraftId` key of one question's draft.
pub fn question_attachment_draft_id(
    environment_id: &str,
    thread_id: &str,
    request_id: &str,
    question_id: &str,
) -> String {
    let ids = serde_json::to_string(&[request_id, question_id]).expect("strings serialize");
    format!(
        "{}{}",
        question_attachment_draft_prefix(environment_id, thread_id),
        encode_uri_component(&ids)
    )
}

/// `releaseDraftAttachments` (lib/attachmentUploadQueue.ts): lets go of the
/// uploads behind a draft's attachments.
pub trait DraftAttachmentRelease {
    fn release_draft_attachments(
        &self,
        images: &[ComposerImageAttachment],
        files: &[ComposerFileAttachment],
    );
}

/// `useQuestionAttachmentPreparation`: attachments being prepared (read,
/// downscaled, uploaded) per question draft, counted against the shared
/// question limit before they are staged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuestionAttachmentPreparation {
    counts: HashMap<String, usize>,
}

impl QuestionAttachmentPreparation {
    pub fn new() -> Self {
        Self::default()
    }

    /// `counts[key]`.
    pub fn count(&self, key: &str) -> Option<usize> {
        self.counts.get(key).copied()
    }

    /// `changeQuestionAttachmentPreparation`: clamped at zero. A decrease for
    /// a key that was never counted changes nothing.
    pub fn change(&mut self, key: &str, delta: i64) {
        let current = self.counts.get(key).copied();
        if delta < 0 && current.is_none() {
            return;
        }
        let next = (current.unwrap_or(0) as i64 + delta).max(0) as usize;
        self.counts.insert(key.to_owned(), next);
    }
}

/// `countQuestionAttachments`: both staged files and in-flight preparation
/// count against the shared question limit.
pub fn count_question_attachments<S: AsRef<str>>(
    store: &ComposerDraftStore,
    preparation: &QuestionAttachmentPreparation,
    keys: &[S],
) -> usize {
    keys.iter()
        .map(|key| {
            let key = key.as_ref();
            let staged = store
                .get_composer_draft(key)
                .map_or(0, |draft| draft.images.len() + draft.files.len());
            staged + preparation.count(key).unwrap_or(0)
        })
        .sum()
}

/// `clearQuestionAttachmentDraft`.
pub fn clear_question_attachment_draft(
    store: &mut ComposerDraftStore,
    preparation: &mut QuestionAttachmentPreparation,
    release: &dyn DraftAttachmentRelease,
    key: &str,
) {
    if let Some(draft) = store.get_composer_draft(key) {
        release.release_draft_attachments(&draft.images, &draft.files);
    }
    store.clear_composer_content(key);
    preparation.counts.remove(key);
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::composer::draft_store::AddAttachmentsOptions;

    const ENVIRONMENT_ID: &str = "environment-1";
    const THREAD_ID: &str = "thread-1";
    const REQUEST_ID: &str = "request-1";

    #[derive(Default)]
    struct RecordingRelease {
        released: RefCell<Vec<(Vec<ComposerImageAttachment>, Vec<ComposerFileAttachment>)>>,
    }

    impl DraftAttachmentRelease for RecordingRelease {
        fn release_draft_attachments(
            &self,
            images: &[ComposerImageAttachment],
            files: &[ComposerFileAttachment],
        ) {
            self.released
                .borrow_mut()
                .push((images.to_vec(), files.to_vec()));
        }
    }

    fn image(id: &str, name: &str) -> ComposerImageAttachment {
        ComposerImageAttachment::new(id, name, "image/png", b"image".to_vec())
    }

    fn text_file(id: &str, name: &str) -> ComposerFileAttachment {
        ComposerFileAttachment {
            id: id.to_owned(),
            name: name.to_owned(),
            mime_type: "text/plain".to_owned(),
            size_bytes: 4,
            data: Some(std::sync::Arc::new(b"spec".to_vec())),
            uploaded_attachment_id: None,
            upload_environment_id: None,
        }
    }

    fn draft_id(question_id: &str) -> String {
        question_attachment_draft_id(ENVIRONMENT_ID, THREAD_ID, REQUEST_ID, question_id)
    }

    #[test]
    fn keeps_question_files_separate_from_the_normal_draft_and_other_questions() {
        let mut store = ComposerDraftStore::new();
        let mut preparation = QuestionAttachmentPreparation::new();
        let release = RecordingRelease::default();
        let first = draft_id("first");
        let second = draft_id("second");
        let normal_draft = "thread-draft-key";
        store.set_prompt(normal_draft, "Unsent prompt");
        let staged_image = image("image-1", "image.png");
        store.add_images(
            &first,
            vec![staged_image.clone()],
            AddAttachmentsOptions::default(),
        );
        store.add_files(
            &second,
            vec![text_file("file-1", "spec.txt")],
            AddAttachmentsOptions::default(),
        );
        preparation.change(&first, 1);
        assert_eq!(store.get_composer_draft(&first).unwrap().images.len(), 1);
        assert_eq!(store.get_composer_draft(&second).unwrap().files.len(), 1);

        clear_question_attachment_draft(&mut store, &mut preparation, &release, &first);

        assert!(store.get_composer_draft(&first).is_none());
        assert_eq!(store.get_composer_draft(&second).unwrap().files.len(), 1);
        assert_eq!(
            store.get_composer_draft(normal_draft).unwrap().prompt,
            "Unsent prompt"
        );
        assert_eq!(preparation.count(&first), None);
        let released = release.released.borrow();
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].0, vec![staged_image]);
        assert!(released[0].1.is_empty());
    }

    #[test]
    fn scopes_provider_request_ids_to_their_environment_and_thread() {
        let keys: std::collections::HashSet<String> = [
            question_attachment_draft_id(ENVIRONMENT_ID, THREAD_ID, REQUEST_ID, "q"),
            question_attachment_draft_id("environment-2", THREAD_ID, REQUEST_ID, "q"),
            question_attachment_draft_id(ENVIRONMENT_ID, "thread-2", REQUEST_ID, "q"),
        ]
        .into_iter()
        .collect();
        assert_eq!(keys.len(), 3);
    }

    #[test]
    fn does_not_match_drafts_belonging_to_threads_with_a_shared_id_prefix() {
        let prefix = question_attachment_draft_prefix(ENVIRONMENT_ID, THREAD_ID);
        assert!(draft_id("q").starts_with(&prefix));
        for suffix in ["-extra", ":extra", "/extra", "%extra"] {
            let other_thread = format!("{THREAD_ID}{suffix}");
            assert!(
                !question_attachment_draft_id(ENVIRONMENT_ID, &other_thread, REQUEST_ID, "q")
                    .starts_with(&prefix),
                "suffix {suffix}"
            );
        }
    }

    #[test]
    fn does_not_clear_another_environment_whose_id_contains_a_question_prefix() {
        let other_environment = format!(
            "{ENVIRONMENT_ID}:question-{}-nested",
            encoded_json_string(THREAD_ID)
        );
        let other_key =
            question_attachment_draft_id(&other_environment, THREAD_ID, REQUEST_ID, "q");
        let own_key = draft_id("q");
        let mut store = ComposerDraftStore::new();
        let mut preparation = QuestionAttachmentPreparation::new();
        let release = RecordingRelease::default();
        store.set_prompt(&other_key, "Keep this answer");
        store.set_prompt(&own_key, "Discard this answer");

        let prefix = question_attachment_draft_prefix(ENVIRONMENT_ID, THREAD_ID);
        for key in [&own_key, &other_key] {
            if key.starts_with(&prefix) {
                clear_question_attachment_draft(&mut store, &mut preparation, &release, key);
            }
        }

        assert!(store.get_composer_draft(&own_key).is_none());
        assert_eq!(
            store.get_composer_draft(&other_key).unwrap().prompt,
            "Keep this answer"
        );
    }

    #[test]
    fn counts_files_images_and_pending_preparation_across_the_shared_question_budget() {
        let first = draft_id("first");
        let second = draft_id("second");
        let other = question_attachment_draft_id("other", THREAD_ID, REQUEST_ID, "first");
        let mut store = ComposerDraftStore::new();
        let mut preparation = QuestionAttachmentPreparation::new();
        store.add_files(
            &second,
            (0..6)
                .map(|index| text_file(&format!("file-{index}"), &format!("spec-{index}.txt")))
                .collect(),
            AddAttachmentsOptions::default(),
        );
        store.add_images(
            &first,
            vec![image("image", "image.png")],
            AddAttachmentsOptions::default(),
        );
        preparation.change(&second, 1);
        preparation.change(&other, 8);

        assert_eq!(
            count_question_attachments(&store, &preparation, &[&first, &second]),
            8
        );
        assert_eq!(
            count_question_attachments(&store, &preparation, &[&second]),
            7
        );
        preparation.change(&second, -1);
        assert_eq!(
            count_question_attachments(&store, &preparation, &[&first, &second]),
            7
        );
    }

    #[test]
    fn preparation_counts_clamp_at_zero_and_ignore_unknown_decreases() {
        let mut preparation = QuestionAttachmentPreparation::new();
        preparation.change("unknown", -1);
        assert_eq!(preparation.count("unknown"), None);

        preparation.change("key", 2);
        preparation.change("key", -5);
        assert_eq!(preparation.count("key"), Some(0));
    }
}
