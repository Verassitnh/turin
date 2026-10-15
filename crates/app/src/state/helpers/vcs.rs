//! Port of upstream/packages/client-runtime/src/state/vcs.ts (pure parts).
//!
//! Kept: the offline branch-list policy, the ref-stream retry delay, the generation-checked
//! refresh commit and the cached-snapshot read. Cut: the atom families (`listRefs`, `status`,
//! the RPC commands, `worktreeSetup`), the connection-generation stream
//! (`makeCachedVcsRefsChanges`) and `applyGitStatusStreamEvent` wiring; those are transport.

use std::time::Duration;

use turin_contracts::EnvironmentId;
use turin_contracts::git::{VcsListRefsInput, VcsListRefsResult};

use super::vcs_ref_invalidation::{VcsRefsCache, VcsRefsCacheState, VcsRefsStore};

pub const OFFLINE_BRANCH_LIST_LIMIT: u64 = 100;
pub const VCS_REFS_IDLE_TTL_MS: u64 = 30_000;
/// Rows keep the last status they rendered, so the live stream only needs a
/// short grace period when virtualization or scrolling releases its consumer.
pub const VCS_STATUS_IDLE_TTL_MS: u64 = 10_000;

/// `VCS_REFS_RETRY_SCHEDULE`: `Schedule.exponential("1 second")` capped at 30 seconds. `attempt`
/// is zero-based: the delay before the first retry is one second.
pub fn vcs_refs_retry_delay(attempt: u32) -> Duration {
    let seconds = 2u64.checked_pow(attempt).unwrap_or(u64::MAX).min(30);
    Duration::from_secs(seconds)
}

/// Whether a request is the unfiltered branch list the offline cache stands in for.
pub fn can_use_vcs_refs_cache(input: &VcsListRefsInput) -> bool {
    input.query.is_none()
        && input.cursor.is_none()
        && input.include_matching_remote_refs.is_none()
        && input.ref_kind.is_none()
        && input.limit.map(|limit| limit.get()) == Some(OFFLINE_BRANCH_LIST_LIMIT)
}

/// `commitVcsRefsRefresh` input.
#[derive(Debug, Clone)]
pub struct CommitVcsRefsRefreshInput {
    pub environment_id: EnvironmentId,
    pub cwd: String,
    pub refs: VcsListRefsResult,
    pub expected_revision: u64,
    pub persist: bool,
}

/// Persists a refresh unless a ref-affecting mutation invalidated the cache since the refresh
/// began. Returns whether the refresh was current.
pub async fn commit_vcs_refs_refresh(
    cache: &VcsRefsCache,
    store: &dyn VcsRefsStore,
    input: &CommitVcsRefsRefreshInput,
) -> bool {
    let _guard = cache.lock_persistence(&input.environment_id).await;
    let state = cache.state_of(&input.environment_id);
    if state.revision != input.expected_revision {
        return false;
    }
    let mut persisted_cache_readable = state.persisted_cache_readable;
    if input.persist {
        if !persisted_cache_readable {
            persisted_cache_readable = match store.clear_vcs_refs(&input.environment_id).await {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(
                        environment_id = %input.environment_id,
                        cwd = %input.cwd,
                        error = %error,
                        "Could not recover invalidated cached Git refs."
                    );
                    false
                }
            };
        }
        if let Err(error) = store
            .save_vcs_refs(&input.environment_id, &input.cwd, &input.refs)
            .await
        {
            tracing::warn!(
                environment_id = %input.environment_id,
                cwd = %input.cwd,
                error = %error,
                "Could not persist cached Git refs."
            );
        }
        if persisted_cache_readable != state.persisted_cache_readable {
            let expected = input.expected_revision;
            cache.update_state(&input.environment_id, |current| {
                if current.revision == expected {
                    VcsRefsCacheState {
                        persisted_cache_readable,
                        ..current
                    }
                } else {
                    current
                }
            });
        }
    }
    true
}

/// The persisted snapshot to show before the live refresh, if this request may use one.
///
/// Retains the last unfiltered branch-list response for the new-task picker.
/// Filtered or paginated lists intentionally stay live-only: treating a
/// partial result as a complete offline list would make branch selection
/// misleading. A cache the last invalidation could not clear is never read.
pub async fn load_cached_vcs_refs(
    store: &dyn VcsRefsStore,
    environment_id: &EnvironmentId,
    input: &VcsListRefsInput,
    persisted_cache_readable: bool,
) -> Option<VcsListRefsResult> {
    if !(can_use_vcs_refs_cache(input) && persisted_cache_readable) {
        return None;
    }
    match store
        .load_vcs_refs(environment_id, input.cwd.as_str())
        .await
    {
        Ok(refs) => refs,
        Err(error) => {
            tracing::warn!(
                environment_id = %environment_id,
                cwd = %input.cwd,
                error = %error,
                "Could not load cached Git refs."
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::vcs_ref_invalidation::{
        CachedVcsRefsInvalidationTarget, VcsRefsInvalidationTarget,
    };
    use super::*;
    use async_trait::async_trait;
    use parking_lot::Mutex;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use turin_contracts::git::{VcsListRefsRefKind, VcsRef};
    use turin_contracts::{PositiveInt, TrimmedNonEmptyString};

    fn environment() -> EnvironmentId {
        EnvironmentId::new("environment-1").unwrap()
    }

    fn target(environment_id: EnvironmentId) -> VcsRefsInvalidationTarget {
        VcsRefsInvalidationTarget { environment_id }
    }

    fn refs(name: &str) -> VcsListRefsResult {
        VcsListRefsResult {
            refs: vec![VcsRef {
                name: TrimmedNonEmptyString::new(name).unwrap(),
                is_remote: None,
                remote_name: None,
                current: true,
                is_default: true,
                worktree_path: Some(TrimmedNonEmptyString::new("/repo").unwrap()),
            }],
            is_repo: true,
            has_primary_remote: true,
            next_cursor: None,
            total_count: 1,
        }
    }

    fn list_input(limit: Option<u64>) -> VcsListRefsInput {
        VcsListRefsInput {
            cwd: TrimmedNonEmptyString::new("/repo").unwrap(),
            query: None,
            cursor: None,
            include_matching_remote_refs: None,
            ref_kind: None,
            refresh: None,
            limit: limit.map(|limit| PositiveInt::new(limit).unwrap()),
        }
    }

    /// In-memory `EnvironmentCacheStore` slice that records what the cache does to it.
    #[derive(Default)]
    struct FakeStore {
        persisted: Mutex<Option<VcsListRefsResult>>,
        saved: Mutex<Vec<VcsListRefsResult>>,
        loads: AtomicUsize,
        clears: AtomicUsize,
        fail_first_clear: bool,
        cache: Option<Arc<VcsRefsCache>>,
        revisions_during_clear: Mutex<Vec<u64>>,
    }

    #[async_trait]
    impl VcsRefsStore for FakeStore {
        async fn load_vcs_refs(
            &self,
            _environment_id: &EnvironmentId,
            _cwd: &str,
        ) -> anyhow::Result<Option<VcsListRefsResult>> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            Ok(self.persisted.lock().clone())
        }
        async fn save_vcs_refs(
            &self,
            _environment_id: &EnvironmentId,
            _cwd: &str,
            refs: &VcsListRefsResult,
        ) -> anyhow::Result<()> {
            self.saved.lock().push(refs.clone());
            *self.persisted.lock() = Some(refs.clone());
            Ok(())
        }
        async fn clear_vcs_refs(&self, environment_id: &EnvironmentId) -> anyhow::Result<()> {
            let attempt = self.clears.fetch_add(1, Ordering::SeqCst) + 1;
            if let Some(cache) = &self.cache {
                self.revisions_during_clear
                    .lock()
                    .push(cache.state(&target(environment_id.clone())).revision);
            }
            if self.fail_first_clear && attempt == 1 {
                anyhow::bail!("storage unavailable");
            }
            *self.persisted.lock() = None;
            Ok(())
        }
    }

    #[test]
    fn invalidates_all_ref_streams_in_the_mutated_environment() {
        let cache = VcsRefsCache::new();
        let environment = target(environment());
        let other = target(EnvironmentId::new("environment-2").unwrap());
        let initial = VcsRefsCacheState {
            revision: 0,
            persisted_cache_readable: true,
        };
        assert_eq!(cache.state(&environment), initial);
        assert_eq!(cache.state(&other), initial);

        cache.invalidate(&environment, None);

        assert_eq!(
            cache.state(&environment),
            VcsRefsCacheState {
                revision: 1,
                persisted_cache_readable: true
            }
        );
        assert_eq!(cache.state(&other), initial);
    }

    #[tokio::test]
    async fn does_not_repersist_a_refresh_superseded_by_ref_invalidation() {
        let cache = Arc::new(VcsRefsCache::new());
        let store = FakeStore {
            cache: Some(cache.clone()),
            ..Default::default()
        };
        cache
            .invalidate_cached(
                &store,
                &CachedVcsRefsInvalidationTarget {
                    environment_id: environment(),
                    cwd: "/repo-worktree".into(),
                },
            )
            .await;

        let commit = |refs, expected_revision| CommitVcsRefsRefreshInput {
            environment_id: environment(),
            cwd: "/repo".into(),
            refs,
            expected_revision,
            persist: true,
        };
        assert!(!commit_vcs_refs_refresh(&cache, &store, &commit(refs("main"), 0)).await);

        assert!(store.saved.lock().is_empty());
        assert_eq!(store.clears.load(Ordering::SeqCst), 1);
        assert_eq!(*store.revisions_during_clear.lock(), vec![0]);
        assert_eq!(
            cache.state(&target(environment())),
            VcsRefsCacheState {
                revision: 1,
                persisted_cache_readable: true
            }
        );

        assert!(commit_vcs_refs_refresh(&cache, &store, &commit(refs("release"), 1)).await);
        assert_eq!(*store.saved.lock(), vec![refs("release")]);
    }

    #[tokio::test]
    async fn suppresses_persisted_snapshots_after_an_environment_wide_clear_fails() {
        let cache = VcsRefsCache::new();
        let store = FakeStore {
            persisted: Mutex::new(Some(refs("main"))),
            fail_first_clear: true,
            ..Default::default()
        };
        cache
            .invalidate_cached(
                &store,
                &CachedVcsRefsInvalidationTarget {
                    environment_id: environment(),
                    cwd: "/repo".into(),
                },
            )
            .await;
        let state = cache.state(&target(environment()));
        assert_eq!(
            state,
            VcsRefsCacheState {
                revision: 1,
                persisted_cache_readable: false
            }
        );

        let input = list_input(Some(100));
        assert_eq!(
            load_cached_vcs_refs(
                &store,
                &environment(),
                &input,
                state.persisted_cache_readable
            )
            .await,
            None
        );
        assert_eq!(store.loads.load(Ordering::SeqCst), 0);

        let live = CommitVcsRefsRefreshInput {
            environment_id: environment(),
            cwd: "/repo".into(),
            refs: refs("release"),
            expected_revision: state.revision,
            persist: true,
        };
        assert!(commit_vcs_refs_refresh(&cache, &store, &live).await);
        let recovered = cache.state(&target(environment()));
        assert_eq!(
            recovered,
            VcsRefsCacheState {
                revision: 1,
                persisted_cache_readable: true
            }
        );
        assert_eq!(store.clears.load(Ordering::SeqCst), 2);

        assert_eq!(
            load_cached_vcs_refs(
                &store,
                &environment(),
                &input,
                recovered.persisted_cache_readable
            )
            .await,
            Some(refs("release"))
        );
        assert_eq!(store.loads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn filtered_lists_never_read_the_persisted_snapshot() {
        let store = FakeStore {
            persisted: Mutex::new(Some(refs("main"))),
            ..Default::default()
        };
        let mut filtered = list_input(Some(100));
        filtered.ref_kind = Some(VcsListRefsRefKind::Remote);
        assert_eq!(
            load_cached_vcs_refs(&store, &environment(), &filtered, true).await,
            None
        );
        assert_eq!(store.loads.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn only_the_unfiltered_offline_branch_list_uses_the_cache() {
        assert!(can_use_vcs_refs_cache(&list_input(Some(100))));
        assert!(!can_use_vcs_refs_cache(&list_input(Some(20))));
        assert!(!can_use_vcs_refs_cache(&list_input(None)));
        let mut with_query = list_input(Some(100));
        with_query.query = Some(TrimmedNonEmptyString::new("release").unwrap());
        assert!(!can_use_vcs_refs_cache(&with_query));
        let mut with_cursor = list_input(Some(100));
        with_cursor.cursor = Some(0);
        assert!(!can_use_vcs_refs_cache(&with_cursor));
        let mut with_remotes = list_input(Some(100));
        with_remotes.include_matching_remote_refs = Some(false);
        assert!(!can_use_vcs_refs_cache(&with_remotes));
    }

    #[test]
    fn retry_delay_doubles_from_one_second_and_caps_at_thirty() {
        let seconds: Vec<u64> = (0..8).map(|n| vcs_refs_retry_delay(n).as_secs()).collect();
        assert_eq!(seconds, vec![1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(vcs_refs_retry_delay(u32::MAX).as_secs(), 30);
    }
}
