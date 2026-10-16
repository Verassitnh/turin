//! Upstream ships no test file for composerDraftUploads.ts; these cover its
//! two functions against fake draft and queue seams.

use std::cell::RefCell;
use std::collections::HashMap;

use super::*;
use crate::composer::draft_store::{ComposerFileAttachment, ComposerImageAttachment};

#[derive(Default)]
struct FakeDrafts {
    by_key: HashMap<String, ComposerThreadDraft>,
    sessions: Vec<DraftSessionRef>,
}

impl DraftUploadSource for FakeDrafts {
    fn composer_draft(&self, target: &ComposerThreadTarget) -> Option<ComposerThreadDraft> {
        let key = match target {
            ComposerThreadTarget::Draft(id) => id.clone(),
            ComposerThreadTarget::Thread {
                environment_id,
                thread_id,
            } => format!("{environment_id}:{thread_id}"),
        };
        self.by_key.get(&key).cloned()
    }
    fn draft_sessions(&self) -> Vec<DraftSessionRef> {
        self.sessions.clone()
    }
    fn draft_by_key(&self, key: &str) -> Option<ComposerThreadDraft> {
        self.by_key.get(key).cloned()
    }
}

#[derive(Default)]
struct Releases(RefCell<Vec<Vec<String>>>);

impl DraftAttachmentReleaser for Releases {
    fn release_draft_attachments(&self, attachments: &[ComposerUploadAttachment]) {
        self.0.borrow_mut().push(
            attachments
                .iter()
                .map(|attachment| match attachment {
                    ComposerUploadAttachment::Image { id, .. }
                    | ComposerUploadAttachment::File { id, .. } => id.clone(),
                })
                .collect(),
        );
    }
}

fn draft(image_ids: &[&str], file_ids: &[&str]) -> ComposerThreadDraft {
    ComposerThreadDraft {
        images: image_ids
            .iter()
            .map(|id| ComposerImageAttachment::new(id, "a.png", "image/png", vec![1]))
            .collect(),
        files: file_ids
            .iter()
            .map(|id| ComposerFileAttachment {
                id: (*id).into(),
                name: "a.txt".into(),
                mime_type: "text/plain".into(),
                size_bytes: 1,
                data: Some(std::sync::Arc::new(vec![1])),
                uploaded_attachment_id: None,
                upload_environment_id: None,
            })
            .collect(),
        ..ComposerThreadDraft::default()
    }
}

fn session(key: &str, environment_id: &str, project_id: &str) -> DraftSessionRef {
    DraftSessionRef {
        draft_key: key.into(),
        environment_id: environment_id.into(),
        project_id: project_id.into(),
    }
}

#[test]
fn releases_a_drafts_images_then_files() {
    let mut drafts = FakeDrafts::default();
    drafts
        .by_key
        .insert("draft-1".into(), draft(&["img-1"], &["file-1"]));
    let releases = Releases::default();
    release_composer_draft_uploads(
        &drafts,
        &releases,
        &ComposerThreadTarget::Draft("draft-1".into()),
    );
    assert_eq!(*releases.0.borrow(), [vec!["img-1", "file-1"]]);
}

#[test]
fn releases_nothing_when_the_target_has_no_draft() {
    let releases = Releases::default();
    release_composer_draft_uploads(
        &FakeDrafts::default(),
        &releases,
        &ComposerThreadTarget::Draft("missing".into()),
    );
    assert!(releases.0.borrow().is_empty());
}

#[test]
fn releases_the_project_draft_sessions_and_its_thread_drafts() {
    let mut drafts = FakeDrafts {
        sessions: vec![
            session("draft-1", "env-1", "project-1"),
            session("draft-2", "env-1", "project-2"),
            session("draft-3", "env-2", "project-1"),
            session("draft-4", "env-1", "project-1"),
        ],
        ..FakeDrafts::default()
    };
    drafts.by_key.insert("draft-1".into(), draft(&["a"], &[]));
    drafts
        .by_key
        .insert("draft-2".into(), draft(&["other-project"], &[]));
    drafts
        .by_key
        .insert("draft-3".into(), draft(&["other-env"], &[]));
    drafts
        .by_key
        .insert("env-1:thread-1".into(), draft(&[], &["t"]));
    let releases = Releases::default();

    release_project_draft_uploads(
        &drafts,
        &releases,
        "env-1",
        "project-1",
        &[
            ("env-1".into(), "thread-1".into()),
            ("env-1".into(), "thread-without-draft".into()),
        ],
    );

    // A session without a stored draft still releases (an empty list), as
    // upstream does.
    assert_eq!(
        *releases.0.borrow(),
        [vec!["a"], Vec::<&str>::new(), vec!["t"]]
    );
}
