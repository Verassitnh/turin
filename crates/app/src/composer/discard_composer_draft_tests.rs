//! Port of upstream/apps/web/src/lib/discardComposerDraft.test.ts
//!
//! The zustand store, fake timers and toast are a [`FakeStore`], explicit
//! `expire` calls, and the `Err` the notice's undo would toast.

use std::sync::atomic::{AtomicUsize, Ordering};

use indexmap::IndexMap;

use super::*;
use crate::composer::attachment_upload_queue::ComposerUploadAttachment;

#[derive(Debug, Clone, PartialEq, Default)]
struct FakeStore {
    drafts: HashMap<String, ComposerThreadDraft>,
    sessions: HashMap<String, String>,
    logical: IndexMap<String, String>,
}

impl FakeStore {
    fn key(target: &ComposerThreadTarget) -> String {
        match target {
            ComposerThreadTarget::Draft(id) => id.clone(),
            ComposerThreadTarget::Thread {
                environment_id,
                thread_id,
            } => format!("{environment_id}:{thread_id}"),
        }
    }

    fn set_prompt(&mut self, target: &ComposerThreadTarget, prompt: &str) {
        let draft = self.drafts.entry(Self::key(target)).or_default();
        draft.prompt = prompt.to_owned();
        if draft.prompt.is_empty() {
            self.drafts.remove(&Self::key(target));
        }
    }

    fn prompt(&self, target: &ComposerThreadTarget) -> String {
        self.drafts
            .get(&Self::key(target))
            .map(|draft| draft.prompt.clone())
            .unwrap_or_default()
    }
}

impl DiscardableDraftStore for FakeStore {
    type Session = String;

    fn resolve_draft_key(&self, target: &ComposerThreadTarget) -> Option<String> {
        Some(Self::key(target))
    }
    fn draft_by_key(&self, key: &str) -> Option<ComposerThreadDraft> {
        self.drafts.get(key).cloned()
    }
    fn session(&self, key: &str) -> Option<String> {
        self.sessions.get(key).cloned()
    }
    fn logical_project_keys_for(&self, key: &str) -> Vec<String> {
        self.logical
            .iter()
            .filter(|(_, draft_key)| draft_key.as_str() == key)
            .map(|(logical, _)| logical.clone())
            .collect()
    }
    fn clear_draft_thread(&mut self, draft_id: &str) {
        self.drafts.remove(draft_id);
        self.sessions.remove(draft_id);
        self.logical.retain(|_, draft_key| draft_key != draft_id);
    }
    fn clear_composer_content(&mut self, target: &ComposerThreadTarget) {
        self.drafts.remove(&Self::key(target));
    }
    fn restore_draft(
        &mut self,
        key: &str,
        draft: ComposerThreadDraft,
        session: Option<String>,
        logical_project_keys: &[String],
    ) {
        for logical in logical_project_keys {
            self.logical
                .entry(logical.clone())
                .or_insert_with(|| key.to_owned());
        }
        self.drafts.insert(key.to_owned(), draft);
        if let Some(session) = session {
            self.sessions.insert(key.to_owned(), session);
        }
    }
}

#[derive(Default)]
struct CountingReleaser(AtomicUsize);

impl CountingReleaser {
    fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

impl DraftAttachmentReleaser for CountingReleaser {
    fn release_draft_attachments(&self, _attachments: &[ComposerUploadAttachment]) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct Fixture {
    store: FakeStore,
    undo: ThreadUndo,
    notices: ThreadUndoNotices,
    discarded: DiscardedDrafts<String>,
    releaser: Arc<CountingReleaser>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            store: FakeStore::default(),
            undo: ThreadUndo::default(),
            notices: ThreadUndoNotices::default(),
            discarded: DiscardedDrafts::default(),
            releaser: Arc::new(CountingReleaser::default()),
        }
    }

    fn discard(&mut self, target: &ComposerThreadTarget) {
        discard_composer_draft(
            &mut self.store,
            target,
            &self.undo,
            &self.notices,
            &self.discarded,
            self.releaser.clone(),
        );
    }

    /// `undoNotice().undo()`, returning what the toast would say per failure.
    fn undo_notice(&mut self) -> Vec<String> {
        let ids = self
            .notices
            .current_notice()
            .map(|notice| notice.ids)
            .unwrap_or_default();
        let mut failures = Vec::new();
        for run in self.notices.take_undo(&ids) {
            let UndoPlan::RestoreDraft { discard_id } = run.plan else {
                panic!("expected a draft restore");
            };
            assert_eq!(run.failure_title, "Failed to restore draft");
            if let Err(message) = restore_discarded_draft(
                &mut self.store,
                &self.discarded,
                self.releaser.as_ref(),
                &discard_id,
            ) {
                failures.push(message);
            }
        }
        failures
    }
}

fn draft_id() -> ComposerThreadTarget {
    ComposerThreadTarget::Draft("draft-1".into())
}

fn thread_ref() -> ComposerThreadTarget {
    ComposerThreadTarget::Thread {
        environment_id: "environment-local".into(),
        thread_id: "thread-1".into(),
    }
}

#[test]
fn restores_a_discarded_new_thread_draft_with_its_project_mapping() {
    let mut fixture = Fixture::new();
    fixture
        .store
        .sessions
        .insert("draft-1".into(), "session".into());
    fixture
        .store
        .logical
        .insert("project-1".into(), "draft-1".into());
    fixture.store.set_prompt(&draft_id(), "half-written prompt");
    let before = fixture.store.clone();

    fixture.discard(&draft_id());
    assert_eq!(fixture.store.session("draft-1"), None);
    let notice = fixture.notices.current_notice().unwrap();
    assert_eq!(notice.action, UndoAction::Discarded);
    assert_eq!(notice.count, 1);

    assert!(fixture.undo_notice().is_empty());
    assert_eq!(fixture.store.session("draft-1"), before.session("draft-1"));
    assert_eq!(fixture.store.prompt(&draft_id()), "half-written prompt");
    assert_eq!(fixture.store.logical, before.logical);
    fixture.notices.expire();
    assert_eq!(fixture.releaser.count(), 0);
    assert!(fixture.discarded.is_empty());
}

#[test]
fn clears_a_thread_draft_for_good_and_releases_its_uploads_once_undo_expires() {
    let mut fixture = Fixture::new();
    fixture.store.set_prompt(&thread_ref(), "reply in progress");

    fixture.discard(&thread_ref());
    let ids = fixture.notices.current_notice().unwrap().ids;
    assert_eq!(fixture.store.prompt(&thread_ref()), "");
    assert_eq!(fixture.releaser.count(), 0);

    fixture.notices.expire();
    assert_eq!(fixture.notices.current_notice(), None);
    assert_eq!(fixture.releaser.count(), 1);
    // Undoing the expired notice restores nothing.
    assert!(fixture.notices.take_undo(&ids).is_empty());
    assert_eq!(fixture.store.prompt(&thread_ref()), "");
}

#[test]
fn keeps_text_typed_after_the_discard_and_releases_the_old_uploads() {
    let mut fixture = Fixture::new();
    fixture.store.set_prompt(&thread_ref(), "old reply");
    fixture.discard(&thread_ref());
    fixture.store.set_prompt(&thread_ref(), "new reply");

    let failures = fixture.undo_notice();
    assert_eq!(failures, ["The draft has new content."]);
    assert_eq!(fixture.store.prompt(&thread_ref()), "new reply");
    assert_eq!(fixture.releaser.count(), 1);
}

#[test]
fn a_target_without_a_draft_shows_no_notice() {
    let mut fixture = Fixture::new();
    fixture.discard(&thread_ref());
    assert_eq!(fixture.notices.current_notice(), None);
    assert!(fixture.discarded.is_empty());
}

#[test]
fn a_second_discard_of_the_same_draft_commits_the_first_without_touching_the_second() {
    let mut fixture = Fixture::new();
    fixture.store.set_prompt(&thread_ref(), "first");
    fixture.discard(&thread_ref());
    fixture.store.set_prompt(&thread_ref(), "second");
    fixture.discard(&thread_ref());

    // The first claim went stale: its upload release ran, its entry is gone.
    assert_eq!(fixture.notices.current_notice().unwrap().count, 1);
    assert_eq!(fixture.releaser.count(), 1);
    assert_eq!(fixture.discarded.len(), 1);

    assert!(fixture.undo_notice().is_empty());
    assert_eq!(fixture.store.prompt(&thread_ref()), "second");
}
