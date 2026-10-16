//! Port of upstream/apps/web/src/hooks/threadUndo.ts (+ test), and the pure
//! grouping and undo queue of upstream/apps/web/src/hooks/showThreadUndoNotice.ts.
//!
//! Upstream keeps the claims in module state so the sidebar, the header and
//! the menus invalidate one another. Here they live in a shared handle
//! (`ThreadUndo`, cheap to clone) that every `ThreadActions` owner takes. The
//! queue is `ThreadUndoNotices`, shared the same way.
//!
//! Upstream's undo is a closure. Here it is `UndoPlan`, data that
//! `ThreadActions` runs, so a notice can be stored and compared without
//! capturing the hook. The 5 s expiry timer and the zustand notice store are
//! the caller's: `expire` and `current_notice` are the seams.
// Consumers are wired by T20/T22.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use turin_contracts::environment::ScopedThreadRef;

/// `UndoOptions["action"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndoAction {
    Settled,
    Snoozed,
    Unpinned,
    Archived,
    Discarded,
}

impl UndoAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Settled => "Settled",
            Self::Snoozed => "Snoozed",
            Self::Unpinned => "Unpinned",
            Self::Archived => "Archived",
            Self::Discarded => "Discarded",
        }
    }
}

/// What Undo does. `ThreadActions::run_undo` runs it through the same methods
/// the original action used, so the gates and claims apply again.
#[derive(Clone, Debug, PartialEq)]
pub enum UndoPlan {
    /// `unarchiveThread(threadRef, { navigate })`.
    Unarchive {
        thread: ScopedThreadRef,
        navigate: bool,
    },
    /// `pinThread(target, { orderKey })`.
    Pin {
        thread: ScopedThreadRef,
        order_key: Option<String>,
    },
    /// Un-settle, then re-pin and re-snooze what settling cleared.
    Settle {
        thread: ScopedThreadRef,
        was_pinned: bool,
        pin_order_key: Option<String>,
        snoozed_until: Option<String>,
    },
    /// `unsnoozeThread(target)`.
    Unsnooze { thread: ScopedThreadRef },
    /// `discardComposerDraft`'s undo: puts a discarded draft back. The data
    /// it needs lives in the discard registry under `discard_id`
    /// (`composer/discard_composer_draft.rs`).
    RestoreDraft { discard_id: String },
}

/// `ThreadUndo.begin`'s claim. Upstream's `Symbol` token is a counter here.
#[derive(Clone, Debug)]
pub struct ThreadUndoClaim {
    owner: ThreadUndo,
    key: (String, String),
    token: u64,
}

impl ThreadUndoClaim {
    /// `isCurrent`.
    pub fn is_current(&self) -> bool {
        self.owner.current_token(&self.key) == Some(self.token)
    }

    /// `finish`: releases the claim if it is still current.
    pub fn finish(&self) {
        if self.is_current() {
            self.owner.lock().current.remove(&self.key);
        }
    }
}

#[derive(Debug, Default)]
struct ClaimState {
    current: HashMap<(String, String), u64>,
    next_token: u64,
}

/// `begin` and `invalidate`, shared by every owner.
#[derive(Clone, Debug, Default)]
pub struct ThreadUndo {
    state: Arc<Mutex<ClaimState>>,
}

impl ThreadUndo {
    fn lock(&self) -> std::sync::MutexGuard<'_, ClaimState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn current_token(&self, key: &(String, String)) -> Option<u64> {
        self.lock().current.get(key).copied()
    }

    /// `begin`: claims one kind of thread action. A later claim of that kind
    /// expires the earlier one's Undo.
    pub fn begin(&self, kind: &str, thread_key: &str) -> ThreadUndoClaim {
        let key = (kind.to_owned(), thread_key.to_owned());
        let token = {
            let mut state = self.lock();
            state.next_token += 1;
            let token = state.next_token;
            state.current.insert(key.clone(), token);
            token
        };
        ThreadUndoClaim {
            owner: self.clone(),
            key,
            token,
        }
    }

    /// `invalidate`: expires only this action kind for this thread.
    pub fn invalidate(&self, kind: &str, thread_key: &str) {
        self.lock()
            .current
            .remove(&(kind.to_owned(), thread_key.to_owned()));
    }
}

/// `UndoOptions.commit`: runs once the action can no longer be undone.
#[derive(Clone)]
pub struct UndoCommit(Arc<dyn Fn() + Send + Sync>);

impl UndoCommit {
    pub fn new(commit: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(commit))
    }

    fn run(&self) {
        (self.0)();
    }
}

impl std::fmt::Debug for UndoCommit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UndoCommit")
    }
}

/// One Undo offered by the notice (`UndoOptions`), without its closure.
#[derive(Clone, Debug)]
pub struct UndoOffer {
    pub id: u64,
    pub action: UndoAction,
    pub plan: UndoPlan,
    pub failure_title: &'static str,
    pub claim: ThreadUndoClaim,
    pub commit: Option<UndoCommit>,
}

/// `UndoNotice`: the newest action and how many consecutive ones of its kind
/// are waiting. `ids` names the group `undo` restores.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoNotice {
    pub action: UndoAction,
    pub count: usize,
    pub ids: Vec<u64>,
}

/// A group the caller must now run. Each claim is already finished.
#[derive(Clone, Debug)]
pub struct UndoRun {
    pub plan: UndoPlan,
    pub failure_title: &'static str,
}

#[derive(Debug, Default)]
struct NoticeState {
    live: Vec<UndoOffer>,
    next_id: u64,
}

/// `showThreadUndoNotice`'s queue, minus the timer and the store.
#[derive(Clone, Debug, Default)]
pub struct ThreadUndoNotices {
    state: Arc<Mutex<NoticeState>>,
}

impl ThreadUndoNotices {
    fn lock(&self) -> std::sync::MutexGuard<'_, NoticeState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// `showThreadUndoNotice`. A claim that is already stale shows nothing.
    pub fn show(
        &self,
        action: UndoAction,
        claim: ThreadUndoClaim,
        plan: UndoPlan,
        failure_title: &'static str,
    ) {
        self.show_with_commit(action, claim, plan, failure_title, None);
    }

    /// `showThreadUndoNotice` with `commit`. The commit runs when the offer's
    /// claim goes stale or the notice expires, never when it is undone.
    pub fn show_with_commit(
        &self,
        action: UndoAction,
        claim: ThreadUndoClaim,
        plan: UndoPlan,
        failure_title: &'static str,
        commit: Option<UndoCommit>,
    ) {
        if !claim.is_current() {
            return;
        }
        {
            let mut state = self.lock();
            state.next_id += 1;
            let id = state.next_id;
            state.live.push(UndoOffer {
                id,
                action,
                plan,
                failure_title,
                claim,
                commit,
            });
        }
        self.refresh();
    }

    /// `refreshNotice`'s first step: drops offers whose claim went stale and
    /// commits them.
    fn refresh(&self) {
        let stale: Vec<UndoOffer> = {
            let mut state = self.lock();
            let (live, stale) = std::mem::take(&mut state.live)
                .into_iter()
                .partition(|offer| offer.claim.is_current());
            state.live = live;
            stale
        };
        for commit in stale.iter().filter_map(|offer| offer.commit.as_ref()) {
            commit.run();
        }
    }

    /// The notice to show: the newest offer and the consecutive offers of its
    /// kind before it. `None` when nothing is left.
    pub fn current_notice(&self) -> Option<UndoNotice> {
        self.refresh();
        let state = self.lock();
        let latest = state.live.last()?;
        let group: Vec<u64> = state
            .live
            .iter()
            .rev()
            .take_while(|offer| offer.action == latest.action)
            .map(|offer| offer.id)
            .collect();
        Some(UndoNotice {
            action: latest.action,
            count: group.len(),
            ids: group,
        })
    }

    /// `undo` on a notice: consumes the live offers named by `ids` whose
    /// claims are still current. Offers come back in `ids` order.
    /// `current_notice` builds that list newest first, which is the order
    /// upstream dispatches (`group` walked from the end, then `current.map`).
    /// Each claim is finished before the caller runs anything, so a repeated
    /// click restores nothing twice. A stale handle restores nothing.
    pub fn take_undo(&self, ids: &[u64]) -> Vec<UndoRun> {
        let mut taken: Vec<UndoOffer> = {
            let mut state = self.lock();
            let mut taken = Vec::new();
            state.live.retain(|offer| {
                let take = ids.contains(&offer.id) && offer.claim.is_current();
                if take {
                    taken.push(offer.clone());
                }
                !take
            });
            taken
        };
        // `retain` visits oldest first. Match the notice's ids instead.
        taken.sort_by_key(|offer| {
            ids.iter()
                .position(|id| *id == offer.id)
                .unwrap_or(usize::MAX)
        });
        for offer in &taken {
            offer.claim.finish();
        }
        self.refresh();
        taken
            .into_iter()
            .map(|offer| UndoRun {
                plan: offer.plan,
                failure_title: offer.failure_title,
            })
            .collect()
    }

    /// The 5 s expiry: every live offer's claim is finished and committed,
    /// and the queue is emptied. The caller owns the timer.
    pub fn expire(&self) {
        let expired: Vec<UndoOffer> = std::mem::take(&mut self.lock().live);
        for offer in &expired {
            offer.claim.finish();
            if let Some(commit) = &offer.commit {
                commit.run();
            }
        }
    }
}

#[cfg(test)]
#[path = "thread_undo_tests.rs"]
mod tests;
