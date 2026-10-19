//! Pure parts of upstream/apps/server/src/orchestration-v2/PullRequestSyncReactor.ts
//!
//! The sweep itself reads the host through `PullRequestService` and `GitManager`
//! (T07 and the git port). What it decides without the host lives here: what a
//! host summary contributes to a stored snapshot, when two snapshots differ,
//! when two stacks differ, and whether a link is due for another read.
//!
//! Port of upstream/apps/server/src/orchestration-v2/PullRequestSyncReactor.ts (pure parts; sweep in pull_request_sync/sweep.rs).

use std::collections::{HashMap, HashSet};

use turin_contracts::IsoDateTime;
use turin_contracts::base_schemas::{NonNegativeInt, TrimmedNonEmptyString};
use turin_contracts::orchestration_v2::OrchestrationV2SettleOverride;
use turin_contracts::pull_request::{
    PullRequestActor, PullRequestChecksState, PullRequestMergeability, PullRequestReviewDecision,
    PullRequestState, PullRequestSummary,
};
use turin_contracts::thread_pull_request::{
    ThreadPullRequestLink, ThreadPullRequestSnapshot, ThreadPullRequestStack,
};

use crate::projection::store::shell::ProjectionThreadPullRequests;

/// A closed or merged request is re-read at most this often (`SLOW_SYNC_INTERVAL_MS`).
pub const SLOW_SYNC_INTERVAL_MS: i64 = 15 * 60 * 1_000;

/// `snapshotFieldsOf`: a snapshot without `syncedAt`. Upstream's `?? null` on
/// closed and merged times is applied here, so an absent time and a null time
/// both store as `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotFields {
    pub state: PullRequestState,
    pub title: TrimmedNonEmptyString,
    pub head_branch: TrimmedNonEmptyString,
    pub base_branch: TrimmedNonEmptyString,
    pub is_draft: bool,
    pub updated_at: Option<IsoDateTime>,
    pub closed_at: Option<String>,
    pub merged_at: Option<String>,
    /// `undefined` (not summarized), `null`, or an actor. Equality reads the login and avatar only.
    pub author: Option<Option<PullRequestActor>>,
    pub additions: Option<NonNegativeInt>,
    pub deletions: Option<NonNegativeInt>,
    pub changed_files: Option<NonNegativeInt>,
    pub review_decision: Option<Option<PullRequestReviewDecision>>,
    pub checks_state: Option<Option<PullRequestChecksState>>,
    pub mergeability: Option<PullRequestMergeability>,
}

pub fn snapshot_fields_of(summary: &PullRequestSummary) -> SnapshotFields {
    SnapshotFields {
        state: summary.state,
        title: summary.title.clone(),
        head_branch: summary.head_branch.clone(),
        base_branch: summary.base_branch.clone(),
        is_draft: summary.is_draft.unwrap_or(false),
        updated_at: Some(summary.updated_at.clone()),
        closed_at: summary.closed_at.clone().flatten(),
        merged_at: summary.merged_at.clone().flatten(),
        author: summary.author.clone(),
        additions: summary.additions,
        deletions: summary.deletions,
        changed_files: summary.changed_files,
        review_decision: summary.review_decision,
        checks_state: summary.checks_state,
        mergeability: summary.mergeability,
    }
}

/// A stored snapshot's fields, for comparison with a fresh read.
pub fn stored_snapshot_fields(snapshot: &ThreadPullRequestSnapshot) -> SnapshotFields {
    SnapshotFields {
        state: snapshot.state,
        title: snapshot.title.clone(),
        head_branch: snapshot.head_branch.clone(),
        base_branch: snapshot.base_branch.clone(),
        is_draft: snapshot.is_draft,
        updated_at: snapshot.updated_at.clone(),
        closed_at: snapshot.closed_at.clone().flatten(),
        merged_at: snapshot.merged_at.clone().flatten(),
        author: snapshot.author.clone(),
        additions: snapshot.additions,
        deletions: snapshot.deletions,
        changed_files: snapshot.changed_files,
        review_decision: snapshot.review_decision,
        checks_state: snapshot.checks_state,
        mergeability: snapshot.mergeability,
    }
}

fn actor_login(actor: Option<&Option<PullRequestActor>>) -> Option<&str> {
    actor
        .and_then(|actor| actor.as_ref())
        .map(|actor| actor.login.as_str())
}

fn actor_avatar(actor: Option<&Option<PullRequestActor>>) -> Option<&str> {
    actor
        .and_then(|actor| actor.as_ref())
        .and_then(|actor| actor.avatar_url.as_deref())
}

/// `snapshotFieldsEqual`. Null and absent compare equal where upstream uses `?? null`.
pub fn snapshot_fields_equal(left: &SnapshotFields, right: &SnapshotFields) -> bool {
    left.state == right.state
        && left.title == right.title
        && left.head_branch == right.head_branch
        && left.base_branch == right.base_branch
        && left.is_draft == right.is_draft
        && left.updated_at == right.updated_at
        && left.closed_at == right.closed_at
        && left.merged_at == right.merged_at
        && actor_login(left.author.as_ref()) == actor_login(right.author.as_ref())
        && actor_avatar(left.author.as_ref()) == actor_avatar(right.author.as_ref())
        && left.additions == right.additions
        && left.deletions == right.deletions
        && left.changed_files == right.changed_files
        && left.review_decision.flatten() == right.review_decision.flatten()
        && left.checks_state.flatten() == right.checks_state.flatten()
        && left.mergeability == right.mergeability
}

/// `stacksEqual`: the stack's identity and each layer's number, branch and state.
pub fn stacks_equal(
    left: Option<&ThreadPullRequestStack>,
    right: Option<&ThreadPullRequestStack>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.kind == right.kind
                && left.id == right.id
                && left.number == right.number
                && left.url == right.url
                && left.base == right.base
                && left.layers.len() == right.layers.len()
                && left.layers.iter().zip(&right.layers).all(|(layer, other)| {
                    layer.number == other.number
                        && layer.head_branch == other.head_branch
                        && layer.state == other.state
                })
        }
        _ => false,
    }
}

/// `isUnsettled`: a thread still watched by its links.
pub fn is_unsettled(thread: &ProjectionThreadPullRequests) -> bool {
    thread.settled_override != Some(OrchestrationV2SettleOverride::Settled)
        && thread.settled_at.is_none()
}

/// One thread's view of a link, the inputs `isDue` reads.
pub struct LinkEntry<'a> {
    /// `isUnsettled` of the link's thread. See [`is_unsettled`].
    pub unsettled: bool,
    pub link: &'a ThreadPullRequestLink,
}

/// The cadence state the sweep keeps across passes (`lastSyncedAt`, `requested`,
/// `retryStacks`).
#[derive(Debug, Default)]
pub struct SyncCadence {
    last_synced_at: HashMap<String, i64>,
    requested: HashMap<String, i64>,
    retry_stacks: HashSet<String>,
}

impl SyncCadence {
    pub fn record_sync(&mut self, key: &str, now_ms: i64) {
        self.last_synced_at.insert(key.to_owned(), now_ms);
    }

    pub fn request(&mut self, key: &str, generation: i64) {
        self.requested.insert(key.to_owned(), generation);
    }

    pub fn retry_stack(&mut self, key: &str) {
        self.retry_stacks.insert(key.to_owned());
    }

    /// `isDue`: is this pull request's read owed on this pass.
    ///
    /// A request asked for explicitly, a retry, a link with no snapshot, or any
    /// active link that is open is due now. Merged-only sets are never due. Closed
    /// requests can reopen on the host, so they are re-read on the slow interval.
    pub fn is_due(&self, key: &str, entries: &[LinkEntry<'_>], now_ms: i64) -> bool {
        if self.requested.contains_key(key) || self.retry_stacks.contains(key) {
            return true;
        }
        if entries.iter().any(|entry| entry.link.snapshot.is_none()) {
            return true;
        }
        // Settled threads stop watching their pull requests. Unsettling one makes its links due
        // on the next sweep, since the cadence clock kept running while it was settled.
        let active: Vec<&LinkEntry<'_>> = entries.iter().filter(|entry| entry.unsettled).collect();
        let state_of =
            |entry: &&LinkEntry<'_>| entry.link.snapshot.as_ref().map(|snapshot| snapshot.state);
        if active
            .iter()
            .all(|entry| state_of(entry) == Some(PullRequestState::Merged))
        {
            return false;
        }
        if active
            .iter()
            .any(|entry| state_of(entry) == Some(PullRequestState::Open))
        {
            return true;
        }
        match self.last_synced_at.get(key) {
            None => true,
            Some(last) => now_ms - last >= SLOW_SYNC_INTERVAL_MS,
        }
    }
}

impl SyncCadence {
    /// The generation of an explicit request for `key`, if one is pending.
    pub fn requested_generation(&self, key: &str) -> Option<i64> {
        self.requested.get(key).copied()
    }

    pub fn request_is_set(&self, key: &str) -> bool {
        self.requested.contains_key(key)
    }

    pub fn retry_set(&self, key: &str) -> bool {
        self.retry_stacks.contains(key)
    }

    pub fn set_retry(&mut self, key: &str, on: bool) {
        if on {
            self.retry_stacks.insert(key.to_owned());
        } else {
            self.retry_stacks.remove(key);
        }
    }

    /// Clears the request only if no newer one arrived while a host read was in flight.
    pub fn clear_request_if(&mut self, key: &str, generation: Option<i64>) {
        if self.requested.get(key).copied() == generation {
            self.requested.remove(key);
        }
    }

    /// Drops state for keys no active link holds any more.
    pub fn prune(&mut self, keep: impl Fn(&str) -> bool) {
        self.last_synced_at.retain(|key, _| keep(key));
        self.retry_stacks.retain(|key| keep(key));
        self.requested.retain(|key, _| keep(key));
    }
}

pub mod adapters;
pub mod sweep;

#[cfg(test)]
mod tests;
