//! Shared composer draft store and discard-undo registry for the shell.
//! Runtime does not own drafts (see WIRE-runtime handoff).

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::{App, Entity, Global};
use indexmap::IndexMap;

use crate::composer::attachment_upload_queue::ComposerThreadTarget;
use crate::composer::composer_draft_uploads::DraftAttachmentReleaser;
use crate::composer::discard_composer_draft::{
    DiscardableDraftStore, DiscardedDrafts, discard_composer_draft, restore_discarded_draft,
};
use crate::composer::draft_store::{ComposerThreadDraft, SharedDraftStore};
use crate::hooks::thread_undo::{ThreadUndo, ThreadUndoNotices, UndoNotice, UndoPlan};

struct NoopReleaser;

impl DraftAttachmentReleaser for NoopReleaser {
    fn release_draft_attachments(
        &self,
        _attachments: &[crate::composer::attachment_upload_queue::ComposerUploadAttachment],
    ) {
    }
}

fn target_key(target: &ComposerThreadTarget) -> Option<String> {
    match target {
        ComposerThreadTarget::Draft(id) => Some(id.clone()),
        ComposerThreadTarget::Thread {
            environment_id,
            thread_id,
        } => Some(format!("{environment_id}:{thread_id}")),
    }
}

/// Draft store + undo queue; one instance per app shell.
pub struct ComposerDraftHost {
    pub store: SharedDraftStore,
    sessions: HashMap<String, ()>,
    logical: IndexMap<String, String>,
    pub undo: ThreadUndo,
    pub notices: ThreadUndoNotices,
    discarded: DiscardedDrafts<()>,
    releaser: Arc<dyn DraftAttachmentReleaser + Send + Sync>,
}

impl ComposerDraftHost {
    pub fn new() -> Self {
        Self {
            store: SharedDraftStore::new(),
            sessions: HashMap::new(),
            logical: IndexMap::new(),
            undo: ThreadUndo::default(),
            notices: ThreadUndoNotices::default(),
            discarded: DiscardedDrafts::default(),
            releaser: Arc::new(NoopReleaser) as Arc<dyn DraftAttachmentReleaser + Send + Sync>,
        }
    }

    pub fn current_notice(&self) -> Option<UndoNotice> {
        self.notices.current_notice()
    }

    pub fn discard_draft_id(&mut self, draft_id: &str) {
        let undo = self.undo.clone();
        let notices = self.notices.clone();
        let discarded = self.discarded.clone();
        let releaser = Arc::clone(&self.releaser);
        discard_composer_draft(
            self,
            &ComposerThreadTarget::Draft(draft_id.to_owned()),
            &undo,
            &notices,
            &discarded,
            releaser,
        );
    }

    pub fn undo_latest(&mut self) -> Option<String> {
        let notice = self.notices.current_notice()?;
        let ids = notice.ids;
        let releaser = Arc::clone(&self.releaser);
        let discarded = self.discarded.clone();
        let mut failure: Option<String> = None;
        for run in self.notices.take_undo(&ids) {
            let UndoPlan::RestoreDraft { discard_id } = run.plan else {
                continue;
            };
            if let Err(message) =
                restore_discarded_draft(self, &discarded, releaser.as_ref(), &discard_id)
            {
                failure = Some(message);
            }
        }
        failure
    }
}

impl DiscardableDraftStore for ComposerDraftHost {
    type Session = ();

    fn resolve_draft_key(&self, target: &ComposerThreadTarget) -> Option<String> {
        target_key(target)
    }

    fn draft_by_key(&self, key: &str) -> Option<ComposerThreadDraft> {
        self.store.get_composer_draft(key)
    }

    fn session(&self, key: &str) -> Option<()> {
        self.sessions.get(key).copied()
    }

    fn logical_project_keys_for(&self, key: &str) -> Vec<String> {
        self.logical
            .iter()
            .filter(|(_, draft_key)| draft_key.as_str() == key)
            .map(|(logical, _)| logical.clone())
            .collect()
    }

    fn clear_draft_thread(&mut self, draft_id: &str) {
        self.store.clear_composer_content(draft_id);
        self.sessions.remove(draft_id);
        self.logical.retain(|_, draft_key| draft_key != draft_id);
    }

    fn clear_composer_content(&mut self, target: &ComposerThreadTarget) {
        if let Some(key) = target_key(target) {
            self.store.clear_composer_content(&key);
        }
    }

    fn restore_draft(
        &mut self,
        key: &str,
        draft: ComposerThreadDraft,
        session: Option<()>,
        logical_project_keys: &[String],
    ) {
        for logical in logical_project_keys {
            self.logical
                .entry(logical.clone())
                .or_insert_with(|| key.to_owned());
        }
        self.store.replace_composer_draft(key, draft);
        if let Some(session) = session {
            self.sessions.insert(key.to_owned(), session);
        }
    }
}

pub struct ComposerDraftHostGlobal(pub Entity<ComposerDraftHost>);

impl Global for ComposerDraftHostGlobal {}

impl ComposerDraftHost {
    pub fn register(entity: Entity<ComposerDraftHost>, cx: &mut App) {
        cx.set_global(ComposerDraftHostGlobal(entity));
    }
}
