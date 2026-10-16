//! Epochs are the invalidation mechanism: a key carries its scope's epoch, so bumping the
//! epoch strands every entry made under the old one — no enumerating a cache whose keys
//! (cursors, commits) nothing holds a list of. The counter is shared and monotonic so a
//! scope re-entering `refEpochs` after eviction can never mint a key an old entry still has.
//!
//! The reference keys of `PullRequestService.ts` (`refScope`, `refCacheKey`, `refOfCacheKey`)
//! live here too, since the epochs are what they are made of.

use std::ops::Deref;

use indexmap::IndexMap;
use serde_json::{Value, json};
use turin_contracts::base_schemas::ProjectId;
use turin_contracts::pull_request::PullRequestRef;

use super::errors::{positive, tne};

pub(crate) const REF_EPOCH_CAPACITY: usize = 2_048;

/// `CredentialRef`: a reference plus the credential namespace the client cannot choose. Internal
/// only: it is part of the cache key, so two credentials never share a cached answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CredentialRef {
    pub reference: PullRequestRef,
    pub credential_namespace: Option<String>,
}

impl CredentialRef {
    pub(crate) fn new(reference: PullRequestRef) -> Self {
        Self {
            reference,
            credential_namespace: None,
        }
    }
}

impl Deref for CredentialRef {
    type Target = PullRequestRef;
    fn deref(&self) -> &PullRequestRef {
        &self.reference
    }
}

/// `refScope`
pub(crate) fn ref_scope(reference: &PullRequestRef) -> String {
    json!([
        reference.project_id.as_str(),
        reference
            .host
            .as_ref()
            .map_or_else(String::new, |host| host.as_str().to_lowercase()),
        reference.repository.as_str().to_lowercase(),
        reference.number.get(),
    ])
    .to_string()
}

/// `refOfCacheKey`: the reference back out of a key. Keys carry the reference back out of the
/// cache loader, so the slot layout is shared with [`ref_cache_key`] rather than read
/// positionally at every loader.
pub(crate) fn ref_of_cache_key(key: &str) -> CredentialRef {
    let slots: Vec<Value> = serde_json::from_str(key).unwrap_or_default();
    let text = |index: usize| slots.get(index).and_then(Value::as_str).map(str::to_owned);
    CredentialRef {
        reference: PullRequestRef {
            project_id: ProjectId::new(text(1).unwrap_or_default())
                .unwrap_or_else(|_| ProjectId::new("unknown").expect("valid id")),
            host: text(2).map(tne),
            expected_account_id: text(5).map(tne),
            allow_stale: None,
            repository: tne(text(3).unwrap_or_default()),
            number: positive(slots.get(4).and_then(Value::as_u64).unwrap_or(1)),
        },
        credential_namespace: text(6),
    }
}

/// Every epoch table of the service. Guarded by one lock; nothing awaits while holding it.
#[derive(Default)]
pub(crate) struct Epochs {
    epoch_counter: u64,
    /// Bumped by a whole-workspace refresh and by every turn and mutation.
    pub listings_epoch: u64,
    ref_epochs: IndexMap<String, u64>,
    project_epochs: IndexMap<String, u64>,
    project_epoch_floor: u64,
    /// The project epoch a pull request was first read merged under. A merged pull request cannot
    /// change again, so a turn in its project no longer moves its key; otherwise every reader
    /// showing it would read the host again after every turn. A refresh or a mutation still moves
    /// it.
    merged_epochs: IndexMap<String, u64>,
    /// Its own scope, so a press forgets the reader's ticks and nothing else. The read's key
    /// carries both epochs, which is what makes an ordinary refresh re-ask for these too.
    files_viewed_epochs: IndexMap<String, u64>,
    /// Bumped by a whole-workspace refresh, the one drop no single reference's epoch covers.
    pub every_file_revision_epoch: u64,
}

impl Epochs {
    pub(crate) fn next(&mut self) -> u64 {
        self.epoch_counter += 1;
        self.epoch_counter
    }

    /// `refEpoch`
    pub(crate) fn ref_epoch(&self, reference: &PullRequestRef) -> u64 {
        let scope = ref_scope(reference);
        let base = self
            .merged_epochs
            .get(&scope)
            .or_else(|| self.project_epochs.get(reference.project_id.as_str()))
            .copied()
            .unwrap_or(self.project_epoch_floor);
        base.max(self.ref_epochs.get(&scope).copied().unwrap_or(0))
    }

    /// `noteMerged`
    pub(crate) fn note_merged(&mut self, reference: &PullRequestRef) {
        let scope = ref_scope(reference);
        if self.merged_epochs.contains_key(&scope) {
            return;
        }
        if self.merged_epochs.len() >= REF_EPOCH_CAPACITY {
            self.merged_epochs.shift_remove_index(0);
        }
        let epoch = self
            .project_epochs
            .get(reference.project_id.as_str())
            .copied()
            .unwrap_or(self.project_epoch_floor);
        self.merged_epochs.insert(scope, epoch);
    }

    fn bump(epochs: &mut IndexMap<String, u64>, counter: &mut u64, reference: &PullRequestRef) {
        let scope = ref_scope(reference);
        if !epochs.contains_key(&scope) && epochs.len() >= REF_EPOCH_CAPACITY {
            epochs.shift_remove_index(0);
        }
        *counter += 1;
        epochs.insert(scope, *counter);
    }

    /// `bumpRefEpoch`
    pub(crate) fn bump_ref_epoch(&mut self, reference: &PullRequestRef) {
        Self::bump(&mut self.ref_epochs, &mut self.epoch_counter, reference);
    }

    /// `filesViewedEpoch`
    pub(crate) fn files_viewed_epoch(&self, reference: &PullRequestRef) -> u64 {
        self.files_viewed_epochs
            .get(&ref_scope(reference))
            .copied()
            .unwrap_or(0)
    }

    /// `bumpFilesViewedEpoch`
    pub(crate) fn bump_files_viewed_epoch(&mut self, reference: &PullRequestRef) {
        Self::bump(
            &mut self.files_viewed_epochs,
            &mut self.epoch_counter,
            reference,
        );
    }

    /// `refreshAfterTurn`'s bookkeeping: the new listings epoch, which is also the project's.
    pub(crate) fn refresh_project(&mut self, project_id: &ProjectId) -> u64 {
        self.listings_epoch = self.next();
        self.project_epochs.shift_remove(project_id.as_str());
        if self.project_epochs.len() >= REF_EPOCH_CAPACITY
            && let Some((_, floor)) = self.project_epochs.shift_remove_index(0)
        {
            self.project_epoch_floor = floor;
        }
        self.project_epochs
            .insert(project_id.as_str().to_owned(), self.listings_epoch);
        self.listings_epoch
    }

    /// The whole-workspace half of `invalidate`.
    pub(crate) fn refresh_everything(&mut self) {
        self.listings_epoch = self.next();
        self.every_file_revision_epoch = self.next();
    }

    /// `listingsEpoch = ++epochCounter`
    pub(crate) fn bump_listings(&mut self) -> u64 {
        self.listings_epoch = self.next();
        self.listings_epoch
    }
}

/// `refCacheKey`
pub(crate) fn ref_cache_key(epochs: &Epochs, reference: &CredentialRef) -> String {
    json!([
        epochs.ref_epoch(&reference.reference),
        reference.project_id.as_str(),
        reference
            .host
            .as_ref()
            .map(|host| host.as_str().to_lowercase()),
        reference.repository.as_str().to_lowercase(),
        reference.number.get(),
        reference.expected_account_id.as_ref().map(|id| id.as_str()),
        reference.credential_namespace,
    ])
    .to_string()
}
