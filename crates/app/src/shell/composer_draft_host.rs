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
use crate::hooks::thread_undo::{ThreadUndo, ThreadUndoNotices, UndoNotice, UndoPlan, UndoRun};

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
        Self::with_store(SharedDraftStore::new())
    }

    /// The live host: the stash is kept in the data directory.
    pub fn persistent() -> Self {
        Self::with_store(SharedDraftStore::with_stash_persistence(Box::new(
            crate::composer::stash_persistence::PromptStashPersistence::over(
                crate::hooks::use_local_storage::FileLocalStorage::shared_in_data_dir(
                    "local-storage.json",
                ),
            ),
        )))
    }

    fn with_store(store: SharedDraftStore) -> Self {
        Self {
            store,
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

    /// Takes the newest notice's group. Discarded drafts are restored here; the thread actions
    /// (unarchive, re-pin, un-settle, wake) are returned for the sidebar, which owns the backend.
    pub fn undo_latest(&mut self) -> UndoLatest {
        let Some(notice) = self.notices.current_notice() else {
            return UndoLatest::default();
        };
        let releaser = Arc::clone(&self.releaser);
        let discarded = self.discarded.clone();
        let mut outcome = UndoLatest::default();
        for run in self.notices.take_undo(&notice.ids) {
            let UndoPlan::RestoreDraft { discard_id } = run.plan else {
                outcome.thread_runs.push(run);
                continue;
            };
            if let Err(message) =
                restore_discarded_draft(self, &discarded, releaser.as_ref(), &discard_id)
            {
                outcome.draft_failure = Some(message);
            }
        }
        outcome
    }
}

/// What [`ComposerDraftHost::undo_latest`] left for its caller.
#[derive(Default)]
pub struct UndoLatest {
    /// A discarded draft that could not be restored.
    pub draft_failure: Option<String>,
    /// Thread actions to undo, newest first.
    pub thread_runs: Vec<UndoRun>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::thread_undo::UndoAction;
    use turin_contracts::environment::ScopedThreadRef;
    use turin_contracts::{EnvironmentId, ThreadId};

    fn thread() -> ScopedThreadRef {
        ScopedThreadRef {
            environment_id: EnvironmentId::new("local").unwrap(),
            thread_id: ThreadId::new("t1").unwrap(),
        }
    }

    #[test]
    fn cmd_z_hands_the_newest_parked_threads_to_the_sidebar_once() {
        let mut host = ComposerDraftHost::new();
        let claim = host.undo.begin("archive", "local:t1");
        host.notices.show(
            UndoAction::Archived,
            claim,
            UndoPlan::Unarchive {
                thread: thread(),
                navigate: true,
            },
            "Failed to undo archive",
        );
        assert_eq!(host.current_notice().map(|notice| notice.count), Some(1));

        let outcome = host.undo_latest();
        assert_eq!(outcome.draft_failure, None);
        assert_eq!(outcome.thread_runs.len(), 1);
        assert_eq!(
            outcome.thread_runs[0].plan,
            UndoPlan::Unarchive {
                thread: thread(),
                navigate: true
            }
        );
        assert_eq!(
            outcome.thread_runs[0].failure_title,
            "Failed to undo archive"
        );
        // The claim was consumed: a repeated Cmd+Z restores nothing twice.
        assert!(host.undo_latest().thread_runs.is_empty());
        assert!(host.current_notice().is_none());
    }

    #[test]
    fn a_later_action_of_the_same_kind_expires_the_earlier_undo() {
        let mut host = ComposerDraftHost::new();
        let first = host.undo.begin("snooze", "local:t1");
        host.notices.show(
            UndoAction::Snoozed,
            first,
            UndoPlan::Unsnooze { thread: thread() },
            "Failed to wake thread",
        );
        // Waking the thread by hand (`unsnoozeThread`) supersedes the Undo.
        host.undo.invalidate("snooze", "local:t1");
        assert!(host.current_notice().is_none());
        assert!(host.undo_latest().thread_runs.is_empty());
    }
}
