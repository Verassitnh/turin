//! A small stand-in for Effect's `RequestResolver.makeGrouped` + `setDelay` + `batchN`: requests
//! are grouped by a key, a group is dispatched when its first request has waited out the delay
//! or when it holds `batchN` entries, whichever comes first.
//!
//! Each waiting request carries the ambient references (`credential_scope`, `interactive`, the
//! pinned credential) of its caller, because a spawned flush does not inherit them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use tokio::sync::oneshot;

use super::errors::GitHubPullRequestApiError;
use crate::source_control_core::ambient::Captured;

pub(crate) type BatchResult<Res> = Result<Res, GitHubPullRequestApiError>;

pub(crate) struct Entry<Req, Res> {
    pub request: Req,
    pub ambient: Captured,
    pub reply: oneshot::Sender<BatchResult<Res>>,
}

struct Group<Req, Res> {
    id: u64,
    entries: Vec<Entry<Req, Res>>,
}

/// What a caller must do after queueing a request.
pub(crate) enum Queued<Req, Res> {
    /// The request joined a group whose timer is already running.
    Joined,
    /// The request opened a group: start its delay timer for this id.
    Opened(u64),
    /// The group reached its size: dispatch these entries now.
    Full(Vec<Entry<Req, Res>>),
}

pub(crate) struct Batcher<Req, Res> {
    groups: Mutex<HashMap<String, Group<Req, Res>>>,
    next_id: AtomicU64,
}

impl<Req, Res> Batcher<Req, Res> {
    pub fn new() -> Self {
        Self {
            groups: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn push(&self, key: &str, entry: Entry<Req, Res>, max: usize) -> Queued<Req, Res> {
        let mut groups = self.groups.lock();
        let opened = !groups.contains_key(key);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let group = groups.entry(key.to_owned()).or_insert_with(|| Group {
            id,
            entries: Vec::new(),
        });
        group.entries.push(entry);
        if group.entries.len() >= max {
            let group = groups.remove(key).expect("group was just filled");
            return Queued::Full(group.entries);
        }
        if opened {
            Queued::Opened(group.id)
        } else {
            Queued::Joined
        }
    }

    /// The entries of group `id` once its delay elapsed; empty when it was already dispatched.
    pub fn take(&self, key: &str, id: u64) -> Vec<Entry<Req, Res>> {
        let mut groups = self.groups.lock();
        if groups.get(key).is_some_and(|group| group.id == id) {
            groups
                .remove(key)
                .map(|group| group.entries)
                .unwrap_or_default()
        } else {
            Vec::new()
        }
    }
}
