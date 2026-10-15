//! Port of upstream/packages/client-runtime/src/state/vcsRefInvalidation.ts
//!
//! Upstream keeps one `VcsRefsCacheState` atom per environment in a global atom family and a
//! `PartitionedSemaphore` (one permit per environment) around persistence. Here the same state
//! and lock live on a `VcsRefsCache` value, and the persistence store is the `VcsRefsStore`
//! trait (`EnvironmentCacheStore.clearVcsRefs` / `saveVcsRefs` / `loadVcsRefs`).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use turin_contracts::EnvironmentId;
use turin_contracts::git::VcsListRefsResult;

/// `VcsRefsInvalidationTarget`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VcsRefsInvalidationTarget {
    pub environment_id: EnvironmentId,
}

/// `CachedVcsRefsInvalidationTarget`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedVcsRefsInvalidationTarget {
    pub environment_id: EnvironmentId,
    pub cwd: String,
}

/// `VcsRefsCacheState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VcsRefsCacheState {
    pub revision: u64,
    pub persisted_cache_readable: bool,
}

impl Default for VcsRefsCacheState {
    fn default() -> Self {
        Self {
            revision: 0,
            persisted_cache_readable: true,
        }
    }
}

/// The slice of upstream's `EnvironmentCacheStore` the refs cache reads and writes.
#[async_trait]
pub trait VcsRefsStore: Send + Sync {
    async fn load_vcs_refs(
        &self,
        environment_id: &EnvironmentId,
        cwd: &str,
    ) -> anyhow::Result<Option<VcsListRefsResult>>;
    async fn save_vcs_refs(
        &self,
        environment_id: &EnvironmentId,
        cwd: &str,
        refs: &VcsListRefsResult,
    ) -> anyhow::Result<()>;
    /// Drops every persisted snapshot of the environment.
    async fn clear_vcs_refs(&self, environment_id: &EnvironmentId) -> anyhow::Result<()>;
}

/// Per-environment revision/readability state plus the lock that serializes persistence.
#[derive(Default)]
pub struct VcsRefsCache {
    states: Mutex<HashMap<EnvironmentId, VcsRefsCacheState>>,
    locks: Mutex<HashMap<EnvironmentId, Arc<AsyncMutex<()>>>>,
}

impl VcsRefsCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// `registry.get(vcsRefsCacheStateAtom(target))`.
    pub fn state(&self, target: &VcsRefsInvalidationTarget) -> VcsRefsCacheState {
        self.state_of(&target.environment_id)
    }

    pub(crate) fn state_of(&self, environment_id: &EnvironmentId) -> VcsRefsCacheState {
        self.states
            .lock()
            .get(environment_id)
            .copied()
            .unwrap_or_default()
    }

    /// `registry.update(vcsRefsCacheStateAtom(target), update)`.
    pub(crate) fn update_state(
        &self,
        environment_id: &EnvironmentId,
        update: impl FnOnce(VcsRefsCacheState) -> VcsRefsCacheState,
    ) {
        let mut states = self.states.lock();
        let current = states.get(environment_id).copied().unwrap_or_default();
        states.insert(environment_id.clone(), update(current));
    }

    /// `invalidateVcsRefs`: bumps the revision so in-flight refreshes cannot repersist.
    pub fn invalidate(
        &self,
        target: &VcsRefsInvalidationTarget,
        persisted_cache_readable: Option<bool>,
    ) {
        self.update_state(&target.environment_id, |state| VcsRefsCacheState {
            revision: state.revision + 1,
            persisted_cache_readable: persisted_cache_readable
                .unwrap_or(state.persisted_cache_readable),
        });
    }

    /// `withVcsRefsPersistenceLock`: hold the guard for the critical section.
    pub async fn lock_persistence(&self, environment_id: &EnvironmentId) -> OwnedMutexGuard<()> {
        let lock = self
            .locks
            .lock()
            .entry(environment_id.clone())
            .or_default()
            .clone();
        lock.lock_owned().await
    }

    /// Clears persisted snapshots and then restarts live ref streams while holding
    /// the same lock used by generation-checked refresh writes. Clearing first
    /// prevents the restarted stream from rehydrating the invalidated snapshot. A
    /// refresh that wins the lock is cleared afterward; one that loses observes the
    /// new revision and cannot repersist its pre-mutation result.
    pub async fn invalidate_cached(
        &self,
        store: &dyn VcsRefsStore,
        target: &CachedVcsRefsInvalidationTarget,
    ) {
        let _guard = self.lock_persistence(&target.environment_id).await;
        let persisted_cache_readable = match store.clear_vcs_refs(&target.environment_id).await {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(
                    environment_id = %target.environment_id,
                    cwd = %target.cwd,
                    error = %error,
                    "Could not remove invalidated cached Git refs."
                );
                false
            }
        };
        self.invalidate(
            &VcsRefsInvalidationTarget {
                environment_id: target.environment_id.clone(),
            },
            Some(persisted_cache_readable),
        );
    }
}
