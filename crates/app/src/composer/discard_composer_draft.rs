//! Port of upstream/apps/web/src/lib/discardComposerDraft.ts
//!
//! Discards a draft's unsent content behind the sidebar undo notice. A
//! new-thread draft (a `Draft` target) loses its whole session; a thread
//! draft only loses its composer content. Uploads are released once the undo
//! window closes.
//!
//! Upstream's `undo` and `commit` are closures over the discarded draft. Here
//! the discarded data waits in a [`DiscardedDrafts`] registry under a
//! `discard_id`; the notice carries `UndoPlan::RestoreDraft { discard_id }`
//! and runs [`restore_discarded_draft`], and the commit hook releases the
//! uploads and drops the entry. The draft store is [`DiscardableDraftStore`].

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::composer::attachment_upload_queue::ComposerThreadTarget;
use crate::composer::composer_draft_uploads::{DraftAttachmentReleaser, draft_upload_attachments};
use crate::composer::draft_store::{ComposerThreadDraft, composer_draft_has_user_content};
use crate::hooks::thread_undo::{ThreadUndo, ThreadUndoNotices, UndoAction, UndoCommit, UndoPlan};

/// The draft-store operations discard and restore use.
pub trait DiscardableDraftStore {
    /// A draft-thread session (`draftThreadsByThreadKey[key]`).
    type Session: Clone;

    /// `resolveComposerDraftKey`.
    fn resolve_draft_key(&self, target: &ComposerThreadTarget) -> Option<String>;
    /// `draftsByThreadKey[key]`.
    fn draft_by_key(&self, key: &str) -> Option<ComposerThreadDraft>;
    /// `draftThreadsByThreadKey[key]`.
    fn session(&self, key: &str) -> Option<Self::Session>;
    /// The logical project keys whose draft thread key is `key`.
    fn logical_project_keys_for(&self, key: &str) -> Vec<String>;
    /// `clearDraftThread(draftId)`.
    fn clear_draft_thread(&mut self, draft_id: &str);
    /// `clearComposerContent(target)`.
    fn clear_composer_content(&mut self, target: &ComposerThreadTarget);
    /// The undo's `setState`: puts the draft back under `key`, restores the
    /// session when there was one, and re-maps each logical project key that
    /// has no mapping (`??=`).
    fn restore_draft(
        &mut self,
        key: &str,
        draft: ComposerThreadDraft,
        session: Option<Self::Session>,
        logical_project_keys: &[String],
    );
}

struct PendingDiscard<S> {
    key: String,
    draft: ComposerThreadDraft,
    session: Option<S>,
    logical_project_keys: Vec<String>,
}

/// The data behind each live discard Undo.
pub struct DiscardedDrafts<S> {
    pending: Arc<Mutex<HashMap<String, PendingDiscard<S>>>>,
    next_id: Arc<Mutex<u64>>,
}

impl<S> Clone for DiscardedDrafts<S> {
    fn clone(&self) -> Self {
        Self {
            pending: Arc::clone(&self.pending),
            next_id: Arc::clone(&self.next_id),
        }
    }
}

impl<S> Default for DiscardedDrafts<S> {
    fn default() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(Mutex::new(0)),
        }
    }
}

impl<S> DiscardedDrafts<S> {
    fn insert(&self, pending: PendingDiscard<S>) -> String {
        let mut next_id = self.next_id.lock();
        *next_id += 1;
        let id = format!("discard-{}", *next_id);
        self.pending.lock().insert(id.clone(), pending);
        id
    }

    fn take(&self, id: &str) -> Option<PendingDiscard<S>> {
        self.pending.lock().remove(id)
    }

    pub fn len(&self) -> usize {
        self.pending.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// `discardComposerDraft`.
pub fn discard_composer_draft<St>(
    store: &mut St,
    target: &ComposerThreadTarget,
    undo: &ThreadUndo,
    notices: &ThreadUndoNotices,
    discarded: &DiscardedDrafts<St::Session>,
    releaser: Arc<dyn DraftAttachmentReleaser + Send + Sync>,
) where
    St: DiscardableDraftStore,
    St::Session: Send + 'static,
{
    let Some(key) = store.resolve_draft_key(target) else {
        return;
    };
    let Some(draft) = store.draft_by_key(&key) else {
        return;
    };
    let session = store.session(&key);
    let logical_project_keys = store.logical_project_keys_for(&key);

    let claim = undo.begin("discard", &key);
    match target {
        ComposerThreadTarget::Draft(draft_id) => store.clear_draft_thread(draft_id),
        ComposerThreadTarget::Thread { .. } => store.clear_composer_content(target),
    }
    // Removing a draft session revokes its image previews upstream and the
    // undo recreates them; images here are byte buffers, so neither applies.

    let attachments = draft_upload_attachments(&draft);
    let discard_id = discarded.insert(PendingDiscard {
        key,
        draft,
        session,
        logical_project_keys,
    });
    let commit_discarded = discarded.clone();
    let commit_id = discard_id.clone();
    notices.show_with_commit(
        UndoAction::Discarded,
        claim,
        UndoPlan::RestoreDraft { discard_id },
        "Failed to restore draft",
        Some(UndoCommit::new(move || {
            commit_discarded.take(&commit_id);
            releaser.release_draft_attachments(&attachments);
        })),
    );
}

/// The notice's undo for a discard. `Ok` when restored, or when the discard
/// already committed; `Err` carries the toast description.
pub fn restore_discarded_draft<St: DiscardableDraftStore>(
    store: &mut St,
    discarded: &DiscardedDrafts<St::Session>,
    releaser: &dyn DraftAttachmentReleaser,
    discard_id: &str,
) -> Result<(), String> {
    let Some(pending) = discarded.take(discard_id) else {
        return Ok(());
    };
    let current = store.draft_by_key(&pending.key);
    if composer_draft_has_user_content(current.as_ref()) {
        releaser.release_draft_attachments(&draft_upload_attachments(&pending.draft));
        return Err("The draft has new content.".to_owned());
    }
    store.restore_draft(
        &pending.key,
        pending.draft,
        pending.session,
        &pending.logical_project_keys,
    );
    Ok(())
}

#[cfg(test)]
#[path = "discard_composer_draft_tests.rs"]
mod tests;
