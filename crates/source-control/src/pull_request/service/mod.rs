//! Port of upstream/apps/server/src/pullRequest/PullRequestService.ts
//!
//! The pull request service: project resolution over the workspace's projects, listings with
//! filters/cursors/involvement/batched search, summaries, previews, details (core + activity),
//! checks, diffs, files viewed, actions, comments/reviews/reactions/reviewers/labels, stacks,
//! routing identity, and the caches, epochs and rate-limit backoff that sit between a reader and
//! the host.
//!
//! Layout (upstream is one 3600-line closure, here split by concern):
//! - [`errors`]: `toPullRequestError`, `isProviderUnusable`, refusal sentences
//! - [`ports`]: the traits the rest of the server implements for it
//! - `provider_backoff`: `withRateLimitBackoff`
//! - `cursor`, `epochs`, `cache`: list cursors, invalidation epochs, `Cache`/`runFork` stand-ins
//! - `projects`, `viewers`, `routing`: which checkout serves a reference, who is signed in
//! - `list`, `detail`, `diff`, `actions`: the reads and writes themselves
//!
//! RPC/auth/permission layers are cut (PORTING.md): the public async methods on
//! [`PullRequestService`] replace the Effect service interface. Effect `Context.Reference`s are
//! task-locals ([`crate::source_control_core::ambient`] and `routing::RoutingCredential`).

mod actions;
mod cache;
mod cursor;
mod detail;
mod diff;
mod epochs;
pub mod errors;
mod list;
pub mod ports;
mod projects;
mod provider_backoff;
mod routing;
mod viewers;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use indexmap::IndexMap;
use parking_lot::Mutex;
use tokio::sync::broadcast;
use turin_contracts::base_schemas::{PositiveInt, ProjectId, TrimmedNonEmptyString};
use turin_contracts::pull_request::{
    PullRequestActionInput, PullRequestActivity, PullRequestChecks, PullRequestCommentInput,
    PullRequestCommentUpdateInput, PullRequestDetail, PullRequestDiffFileContentsInput,
    PullRequestDiffFileContentsResult, PullRequestDiffInput, PullRequestDiffResult,
    PullRequestDiffStat, PullRequestFilesViewedResult, PullRequestInvalidateInput,
    PullRequestLabelCandidateList, PullRequestLabelChangeInput, PullRequestListInput,
    PullRequestListResult, PullRequestListStatsInput, PullRequestListStatsResult,
    PullRequestPreview, PullRequestReactionInput, PullRequestRef, PullRequestReportStateInput,
    PullRequestReviewerCandidateList, PullRequestReviewerRequestInput, PullRequestRoutingIdentityInput,
    PullRequestRoutingIdentityResult, PullRequestRoutingResult, PullRequestSetFilesViewedInput,
    PullRequestStack, PullRequestState, PullRequestSubmitReviewInput, PullRequestSummary,
    PullRequestThreadCommentsInput, PullRequestThreadCommentsResult, PullRequestThreadReplyInput,
    PullRequestThreadResolutionInput, PullRequestUpdateInput,
};
use turin_contracts::thread_pull_request::ThreadPullRequestKey;
use turin_persistence::Db;

use self::cache::{Background, LastGood, StaleDiff, TtlCache};
use self::detail::{PullRequestReading, detail_time_to_live};
use self::epochs::{CredentialRef, Epochs, ref_scope};
use self::errors::{PullRequestError, error_tag, tne};
use self::ports::{PullRequestProjectSource, PullRequestSettings, RepositoryIdentityResolver};
use self::viewers::{HeldViewer, ResolvedViewer, ViewerTarget};
pub use self::errors::{is_provider_unusable, to_pull_request_error};
pub use self::projects::SupportedProject;
pub use self::routing::RoutingCredential;
use crate::pull_request::provider_registry::PullRequestProviderRegistry;
use crate::pull_request::read_cache::PullRequestReadCache;
use crate::pull_request::review_guide_service::ReviewGuidePullRequests;
use crate::pull_request::viewed_files::{
    FilesViewedProject, PullRequestViewedFiles, ViewedFilesDependencies,
};
use crate::source_control_core::clock::Clock;
use crate::source_control_core::pull_request_provider::{
    ProviderChangeRequestWatchFingerprint, PullRequestProviderError,
};
use crate::source_control_core::rate_limit::SourceControlRateLimit;
use crate::source_control_core::source_control_provider::ResolveProviderContext;

/// A pull request the service saw merge, for immediate settlement of its linked threads
/// (upstream `PullRequestMergeEvent extends PullRequestRef`).
#[derive(Debug, Clone, PartialEq)]
pub struct PullRequestMergeEvent {
    pub project_id: ProjectId,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    pub merged_at: String,
}

/// Rows per repository when the client does not ask for a page size, and rows per slice when a
/// listing is carried on from a cursor.
///
/// 99 and not 100, because every provider asks its host for one row over this to probe for a next
/// page: 99 requests 100, which is exactly what a page of GitHub's API serves — GraphQL refuses
/// `first` over 100 with EXCESSIVE_PAGINATION and REST clamps `per_page` to it — and what GitLab
/// caps `per_page` at. Asking for 100 here would request 101 and buy a whole second round trip for
/// one row (measured: `gh pr list --limit 100` makes 1 HTTP request, `--limit 101` makes 2).
pub(crate) const DEFAULT_REPOSITORY_LIST_LIMIT: i64 = 99;
/// Repositories read at once. Each one is a CLI process that spends nearly all its wall clock
/// waiting on the host, so the useful ceiling is far above the core count; measured over 12
/// repositories on this listing's own command, 4 took ~12.7s, 8 ~8.9s and 12 ~4.9s, with 16 and 24
/// no faster because 12 already reads every repository in one wave.
pub(crate) const REPOSITORY_CONCURRENCY: usize = 12;
/// Repositories named in one read across a host. Measured against GitHub's search: six hundred
/// `repo:` qualifiers in one query — 14.7KB of it — were all still honoured, and the answer took
/// the same three to six seconds at twelve repositories as at four hundred. A hundred is well
/// inside that and past the size of a workspace anyone opens, so a larger one reads in a handful
/// of searches rather than in a request per repository.
pub(crate) const REPOSITORY_SEARCH_CHUNK: usize = 100;

// Every read leaves the process — a CLI per repository, against hosts whose limits are low
// (GitHub's search API allows ~30 requests a minute) — so answers are shared for a short
// while and concurrent identical reads share one request. The windows sit near the clients'
// own stale times: long enough that two people opening the same page cost one round trip,
// short enough that "cached" and "fresh" never need telling apart on screen. Reads that
// must not share — the refresh button, a client reloading after its own action — go through
// `invalidate` rather than a flag on the read, so an ordinary read can never opt out.
pub(crate) const LIST_CACHE_TTL_MS: i64 = 30_000;
// Each connected client re-reads the pull request it shows on focus, on a timer, and after every
// turn, so detail, activity, and preview answers are shared for the clients' own minute of
// staleness. A merged change request cannot change again and is held for ten. A closed one is
// not: it can reopen, and a watch started on it reads it to learn whether it did.
pub(crate) const DETAIL_CACHE_TTL_MS: i64 = 60_000;
pub(crate) const MERGED_DETAIL_CACHE_TTL_MS: i64 = 10 * 60_000;
// Checks and the watch fingerprint stay short: they are how a running check's result reaches the
// page and a watched pull request's change reaches its watch.
pub(crate) const CHECKS_CACHE_TTL_MS: i64 = 15_000;
/// How long a repeated reported state is skipped, so a wrong one cannot hide a real change for long.
pub(crate) const REPORTED_STATE_DEDUPE_MS: i64 = 5 * 60 * 1_000;
pub(crate) const DIFF_CACHE_TTL_MS: i64 = 60_000;
/// A commit is content-addressed, so its own diff cannot change under its key.
pub(crate) const COMMIT_DIFF_CACHE_TTL_MS: i64 = 10 * 60_000;
/// Sized like the client's own stale time; a row's counts move only when somebody pushes.
pub(crate) const LIST_STATS_CACHE_TTL_MS: i64 = 60_000;
/// Short, and with no stale window behind it: this is the reader's own bookkeeping, and the
/// press that changes it is the same press the page is already showing optimistically. Held at
/// all only so opening a change request on two devices costs one read.
pub(crate) const FILES_VIEWED_CACHE_TTL_MS: i64 = 15_000;
/// A diff can stay interactive while its next cached value is fetched off the critical path.
pub(crate) const DIFF_STALE_WINDOW_MS: i64 = 10 * 60_000;
/// How long an expired diff may stay held before it is dropped unread.
pub(crate) const DIFF_SWEEP_INTERVAL: Duration = Duration::from_secs(60);
/// How long one host's signed-in login is believed without asking its CLI again.
pub(crate) const VIEWER_CACHE_TTL_MS: i64 = 10 * 60_000;
pub(crate) const SEARCH_VISIBILITY_TTL_MS: i64 = 10 * 60_000;
pub(crate) const STALE_DETAIL_WINDOW_MS: i64 = 10 * 60_000;
pub(crate) const LIST_CACHE_CAPACITY: usize = 64;
pub(crate) const LIST_STATS_CACHE_CAPACITY: usize = 32;
pub(crate) const DETAIL_CACHE_CAPACITY: usize = 128;
pub(crate) const DIFF_CACHE_CAPACITY: usize = 128;
/// Each diff cache can retain at most 64 MiB of patch text, counting UTF-16 storage.
const MAX_CACHED_DIFF_PATCH_BYTES: usize = 512 * 1024;
pub(crate) const FILES_VIEWED_CACHE_CAPACITY: usize = 128;
pub(crate) const VIEWER_CACHE_CAPACITY: usize = 32;

/// `canCacheDiff`: a patch past the bound is returned intact but retained by neither cache.
pub(crate) fn can_cache_diff(value: &PullRequestDiffResult) -> bool {
    value.patch.encode_utf16().count() * 2 <= MAX_CACHED_DIFF_PATCH_BYTES
}

/// The reference fields every input type carries.
pub trait AsPullRequestRef {
    fn pull_request_ref(&self) -> PullRequestRef;
}

impl AsPullRequestRef for PullRequestRef {
    fn pull_request_ref(&self) -> PullRequestRef {
        self.clone()
    }
}

macro_rules! as_pull_request_ref {
    ($($input:ty),* $(,)?) => {$(
        impl AsPullRequestRef for $input {
            fn pull_request_ref(&self) -> PullRequestRef {
                PullRequestRef {
                    project_id: self.project_id.clone(),
                    host: self.host.clone(),
                    expected_account_id: self.expected_account_id.clone(),
                    allow_stale: self.allow_stale,
                    repository: self.repository.clone(),
                    number: self.number,
                }
            }
        }
    )*};
}

as_pull_request_ref!(
    PullRequestDiffInput,
    PullRequestDiffFileContentsInput,
    PullRequestSetFilesViewedInput,
    PullRequestActionInput,
    PullRequestCommentInput,
    PullRequestUpdateInput,
    PullRequestCommentUpdateInput,
    PullRequestSubmitReviewInput,
    PullRequestThreadCommentsInput,
    PullRequestThreadReplyInput,
    PullRequestThreadResolutionInput,
    PullRequestReactionInput,
    PullRequestReviewerRequestInput,
    PullRequestLabelChangeInput,
);

/// Everything the service is built from.
pub struct PullRequestServiceDependencies {
    /// `PullRequestProviderRegistry`
    pub registry: PullRequestProviderRegistry,
    /// `ServerSettingsService` (merge settings only)
    pub settings: Arc<dyn PullRequestSettings>,
    /// `ProjectService`
    pub projects: Arc<dyn PullRequestProjectSource>,
    /// `RepositoryIdentityResolver`
    pub repository_identities: Arc<dyn RepositoryIdentityResolver>,
    /// `SourceControlProviderRegistry.resolveHandle`
    pub provider_context: Arc<dyn ResolveProviderContext>,
    /// `SourceControlRateLimit`
    pub rate_limits: Arc<SourceControlRateLimit>,
    /// The database `PullRequestFilesViewedRepository` writes to.
    pub db: Db,
    /// `PullRequestReadCache`
    pub read_cache: Arc<PullRequestReadCache>,
    pub clock: Arc<dyn Clock>,
}

/// The state every method shares, behind one lock. Nothing awaits while holding it.
#[derive(Default)]
pub(crate) struct State {
    pub epochs: Epochs,
    /// Who is signed in per host, believed for `VIEWER_CACHE_TTL_MS`.
    pub viewers_by_host: std::collections::HashMap<String, HeldViewer>,
    /// A repository that has appeared in a host search is known to be indexed there. Empty
    /// authored/reviewing searches for that same repository are therefore real empty answers, not
    /// a reason to issue the two-command per-repository fallback again.
    pub search_visible_at: IndexMap<String, i64>,
    /// The newest state a detail read found per pull request, whatever cache epoch or credential
    /// it read under. Summary reads stay out: they would hide a change from the detail read after.
    pub detail_states: IndexMap<String, PullRequestReading>,
    /// The last state each routed read reported, kept apart from `detail_states`: an unverified
    /// report must never outrank a reading this environment made itself.
    pub reported_states: IndexMap<String, (PullRequestState, i64)>,
    pub recent_stats: IndexMap<String, (i64, PullRequestDiffStat)>,
}

/// `SubscriptionRef<number>` with `changes`: the current value, then each value set.
struct Refreshes {
    current: Mutex<u64>,
    sender: broadcast::Sender<u64>,
}

impl Refreshes {
    fn new() -> Self {
        Self {
            current: Mutex::new(0),
            sender: broadcast::channel(1024).0,
        }
    }

    fn set(&self, value: u64) {
        let mut current = self.current.lock();
        *current = value;
        let _ = self.sender.send(value);
    }

    fn changes(&self) -> BoxStream<'static, u64> {
        let (initial, receiver) = {
            let current = self.current.lock();
            (*current, self.sender.subscribe())
        };
        stream::once(async move { initial })
            .chain(receiver_stream(receiver))
            .filter(|revision| futures::future::ready(*revision > 0))
            .boxed()
    }
}

/// A subscription to a sliding `PubSub`: a subscriber that falls behind skips what it missed.
fn receiver_stream<T: Clone + Send + 'static>(
    receiver: broadcast::Receiver<T>,
) -> BoxStream<'static, T> {
    stream::unfold(receiver, |mut receiver| async move {
        loop {
            match receiver.recv().await {
                Ok(value) => return Some((value, receiver)),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    })
    .boxed()
}

pub(crate) struct Inner {
    pub registry: PullRequestProviderRegistry,
    pub settings: Arc<dyn PullRequestSettings>,
    pub projects: Arc<dyn PullRequestProjectSource>,
    pub repository_identities: Arc<dyn RepositoryIdentityResolver>,
    pub provider_context: Arc<dyn ResolveProviderContext>,
    pub rate_limits: Arc<SourceControlRateLimit>,
    pub read_cache: Arc<PullRequestReadCache>,
    pub clock: Arc<dyn Clock>,
    pub state: Mutex<State>,
    pub merged: broadcast::Sender<PullRequestMergeEvent>,
    pub state_changes: broadcast::Sender<ThreadPullRequestKey>,
    refreshes: Refreshes,
    pub background: Arc<Background>,
    pub diff_sweep_pending: AtomicBool,
    pub last_good_summary: Arc<LastGood<PullRequestSummary>>,
    pub last_good_detail: Arc<LastGood<PullRequestDetail>>,
    pub stale_diff: Arc<StaleDiff>,
    pub viewer_flights: TtlCache<ResolvedViewer>,
    pub list_cache: TtlCache<PullRequestListResult>,
    pub list_stats_cache: TtlCache<(PullRequestListStatsResult, i64)>,
    pub checks_cache: TtlCache<Option<PullRequestChecks>>,
    pub watch_cache: TtlCache<Option<ProviderChangeRequestWatchFingerprint>>,
    pub detail_cache: TtlCache<PullRequestDetail>,
    pub activity_cache: TtlCache<PullRequestActivity>,
    pub preview_cache: TtlCache<PullRequestPreview>,
    pub diff_cache: TtlCache<PullRequestDiffResult>,
    pub files_viewed_cache: TtlCache<PullRequestFilesViewedResult>,
    pub viewed_files: PullRequestViewedFiles<ViewedDeps>,
}

/// What `ViewedFiles.make` receives from the service upstream as closures.
pub(crate) struct ViewedDeps {
    inner: std::sync::Weak<Inner>,
}

impl ViewedDeps {
    fn inner(&self) -> Result<Arc<Inner>, PullRequestError> {
        self.inner.upgrade().ok_or_else(|| {
            errors::operation_error("filesViewed", "The pull request service has shut down.")
        })
    }
}

#[async_trait]
impl ViewedFilesDependencies for ViewedDeps {
    type Error = PullRequestError;

    fn ref_epoch(&self, reference: &PullRequestRef) -> u64 {
        self.inner.upgrade().map_or(0, |inner| {
            inner.state.lock().epochs.ref_epoch(reference)
        })
    }

    fn file_revisions_epoch(&self) -> u64 {
        self.inner.upgrade().map_or(0, |inner| {
            inner.state.lock().epochs.every_file_revision_epoch
        })
    }

    fn to_pull_request_error(
        &self,
        operation: &str,
        error: PullRequestProviderError,
    ) -> PullRequestError {
        errors::to_pull_request_error(operation, error)
    }

    fn error_tag(&self, error: &PullRequestError) -> &'static str {
        error_tag(error)
    }

    async fn require_project(
        &self,
        reference: &PullRequestRef,
    ) -> Result<FilesViewedProject, PullRequestError> {
        let inner = self.inner()?;
        let project = inner.require_project(reference).await?;
        Ok(FilesViewedProject {
            project_id: project.project.id.clone(),
            workspace_root: project.project.workspace_root.as_str().to_owned(),
            api: project.api.clone(),
            repository: project.repository.clone(),
            host: project.host.clone(),
            remote: project.remote.clone(),
        })
    }

    async fn required_viewer_of(
        &self,
        project: &FilesViewedProject,
        operation: &str,
    ) -> Result<Option<String>, PullRequestError> {
        let inner = self.inner()?;
        inner
            .required_viewer_of(
                &ViewerTarget {
                    host: project.host.clone(),
                    api: project.api.clone(),
                    workspace_root: project.workspace_root.clone(),
                },
                operation,
            )
            .await
    }
}

impl Inner {
    /// `refreshAfterTurn`
    pub(crate) async fn refresh_after_turn(self: &Arc<Self>, project_id: &ProjectId) {
        let listings_epoch = self.state.lock().epochs.refresh_project(project_id);
        self.read_cache
            .invalidate(&format!("project:{project_id}"))
            .await;
        self.refreshes.set(listings_epoch);
    }

    /// `invalidate`
    async fn invalidate(
        self: &Arc<Self>,
        input: &PullRequestInvalidateInput,
        notify_readers: bool,
    ) {
        let reference = input.reference.as_ref();
        if input.files_viewed_only == Some(true) {
            match reference {
                None => self.files_viewed_cache.invalidate_all(),
                Some(reference) => {
                    if let Ok(reference) = self.canonical_ref(reference).await {
                        self.state.lock().epochs.bump_files_viewed_epoch(&reference);
                    }
                }
            }
            return;
        }
        if let Some(reference) = reference {
            if let Ok(reference) = self.canonical_ref(reference).await {
                self.read_cache.invalidate(&ref_scope(&reference)).await;
                self.state.lock().epochs.bump_ref_epoch(&reference);
            }
        } else {
            {
                let mut state = self.state.lock();
                state.epochs.refresh_everything();
                state.viewers_by_host.clear();
            }
            self.viewer_flights.invalidate_all();
        }
        if notify_readers {
            let revision = self.state.lock().epochs.next();
            self.refreshes.set(revision);
        }
    }

    /// Invalidate before notifying every client so mounted readers immediately fetch the edit.
    async fn invalidated_by_mutation<F>(
        self: &Arc<Self>,
        input: &PullRequestRef,
        method: F,
    ) -> Result<(), PullRequestError>
    where
        F: Future<Output = Result<(), PullRequestError>>,
    {
        let reference = self.canonical_ref(input).await?;
        let scope = ref_scope(&reference);
        self.read_cache.invalidate(&scope).await;
        let result = method.await;
        self.read_cache.invalidate(&scope).await;
        result?;
        let listings_epoch = {
            let mut state = self.state.lock();
            state.epochs.bump_ref_epoch(&reference);
            state.epochs.bump_listings()
        };
        self.refreshes.set(listings_epoch);
        Ok(())
    }

    async fn run_action_and_invalidate(
        self: &Arc<Self>,
        input: &PullRequestActionInput,
    ) -> Result<(), PullRequestError> {
        let reference = self.canonical_ref(&input.pull_request_ref()).await?;
        let scope = ref_scope(&reference);
        self.read_cache.invalidate(&scope).await;
        let result = self.run_action(input).await;
        self.read_cache.invalidate(&scope).await;
        let repository = result?;
        let listings_epoch = {
            let mut state = self.state.lock();
            state.epochs.bump_ref_epoch(&PullRequestRef {
                repository: tne(&repository),
                ..reference
            });
            state.epochs.bump_listings()
        };
        self.refreshes.set(listings_epoch);
        if input.action == turin_contracts::pull_request::PullRequestAction::Merge {
            // A successful merge action can merely enqueue the PR or enable auto-merge.
            let confirmed = match self
                .summary_uncached(&PullRequestRef {
                    repository: tne(&repository),
                    ..input.pull_request_ref()
                })
                .await
            {
                Ok(summary) => Some(summary),
                Err(error) => {
                    tracing::warn!(?error, "failed to confirm pull request merge");
                    None
                }
            };
            if confirmed.map(|summary| summary.state) != Some(PullRequestState::Merged) {
                return Ok(());
            }
            let merged_at = chrono::DateTime::from_timestamp_millis(self.clock.now_millis())
                .unwrap_or_default()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let _ = self.merged.send(PullRequestMergeEvent {
                project_id: input.project_id.clone(),
                repository: tne(&repository),
                number: input.number,
                merged_at,
            });
        }
        Ok(())
    }

    /// `credentialCached`: the reference spelled the way the project spells it, filed under the
    /// credential a routed operation runs as.
    async fn credential_ref(
        self: &Arc<Self>,
        input: &PullRequestRef,
    ) -> Result<CredentialRef, PullRequestError> {
        let reference = self.canonical_ref(input).await?;
        Ok(CredentialRef {
            reference,
            credential_namespace: routing::routing_credential()
                .map(|credential| credential.credential_fingerprint),
        })
    }
}

/// The pull request service. Cheap to clone; clones share all state.
#[derive(Clone)]
pub struct PullRequestService {
    inner: Arc<Inner>,
}

impl PullRequestService {
    pub fn new(dependencies: PullRequestServiceDependencies) -> Self {
        let PullRequestServiceDependencies {
            registry,
            settings,
            projects,
            repository_identities,
            provider_context,
            rate_limits,
            db,
            read_cache,
            clock,
        } = dependencies;
        let background = Background::new();
        let last_good_summary = LastGood::new(
            DETAIL_CACHE_CAPACITY,
            STALE_DETAIL_WINDOW_MS,
            clock.clone(),
            background.clone(),
        );
        let last_good_detail = LastGood::new(
            DETAIL_CACHE_CAPACITY,
            STALE_DETAIL_WINDOW_MS,
            clock.clone(),
            background.clone(),
        );
        let stale_diff = Arc::new(StaleDiff::new(
            DIFF_CACHE_CAPACITY,
            DIFF_STALE_WINDOW_MS,
            clock.clone(),
            background.clone(),
        ));
        let inner = Arc::new_cyclic(|weak| {
            let cache = |capacity| capacity;
            // The host-wide success map holds the real ten-minute answer. This short entry exists
            // to keep simultaneous cold page reads on one in-flight lookup; failures remain
            // retryable.
            let viewer_flights = TtlCache::new(
                cache(VIEWER_CACHE_CAPACITY),
                clock.clone(),
                |value: &ResolvedViewer, _| if value.error.is_none() { 1_000 } else { 0 },
            );
            let activity_summary = last_good_summary.clone();
            Inner {
                registry,
                settings,
                projects,
                repository_identities,
                provider_context,
                rate_limits,
                read_cache,
                state: Mutex::new(State::default()),
                merged: broadcast::channel(64).0,
                state_changes: broadcast::channel(64).0,
                refreshes: Refreshes::new(),
                background,
                diff_sweep_pending: AtomicBool::new(false),
                last_good_summary,
                last_good_detail,
                stale_diff,
                viewer_flights,
                list_cache: TtlCache::new(LIST_CACHE_CAPACITY, clock.clone(), |_, _| {
                    LIST_CACHE_TTL_MS
                }),
                list_stats_cache: TtlCache::new(
                    LIST_STATS_CACHE_CAPACITY,
                    clock.clone(),
                    |_, _| LIST_STATS_CACHE_TTL_MS,
                ),
                checks_cache: TtlCache::new(DETAIL_CACHE_CAPACITY, clock.clone(), |_, _| {
                    CHECKS_CACHE_TTL_MS
                }),
                watch_cache: TtlCache::new(DETAIL_CACHE_CAPACITY, clock.clone(), |_, _| {
                    CHECKS_CACHE_TTL_MS
                }),
                detail_cache: TtlCache::new(
                    DETAIL_CACHE_CAPACITY,
                    clock.clone(),
                    |value: &PullRequestDetail, _| detail_time_to_live(Some(value.state)),
                ),
                // Activity carries no state of its own; the detail or summary read under the same
                // key does.
                activity_cache: TtlCache::new(
                    DETAIL_CACHE_CAPACITY,
                    clock.clone(),
                    move |_, key| {
                        detail_time_to_live(activity_summary.peek(key).map(|summary| summary.state))
                    },
                ),
                preview_cache: TtlCache::new(
                    DETAIL_CACHE_CAPACITY,
                    clock.clone(),
                    |value: &PullRequestPreview, _| detail_time_to_live(Some(value.state)),
                ),
                diff_cache: TtlCache::new(DIFF_CACHE_CAPACITY, clock.clone(), |_, key| {
                    let commit_is_null = serde_json::from_str::<Vec<serde_json::Value>>(key)
                        .ok()
                        .and_then(|slots| slots.get(2).map(serde_json::Value::is_null))
                        .unwrap_or(true);
                    if commit_is_null {
                        DIFF_CACHE_TTL_MS
                    } else {
                        COMMIT_DIFF_CACHE_TTL_MS
                    }
                }),
                files_viewed_cache: TtlCache::new(
                    FILES_VIEWED_CACHE_CAPACITY,
                    clock.clone(),
                    |_, _| FILES_VIEWED_CACHE_TTL_MS,
                ),
                // Built after the epochs because it reads two of them.
                viewed_files: PullRequestViewedFiles::new(
                    ViewedDeps {
                        inner: weak.clone(),
                    },
                    db,
                    clock.clone(),
                ),
                clock,
            }
        });
        Self { inner }
    }

    pub async fn list(
        &self,
        input: &PullRequestListInput,
    ) -> Result<PullRequestListResult, PullRequestError> {
        self.inner.list(input).await
    }

    /// The line counts for rows already on the page.
    pub async fn list_stats(
        &self,
        input: &PullRequestListStatsInput,
    ) -> Result<PullRequestListStatsResult, PullRequestError> {
        self.inner.list_stats(input).await
    }

    pub async fn routing(
        &self,
        input: &PullRequestRef,
    ) -> Result<PullRequestRoutingResult, PullRequestError> {
        self.inner.routing(input).await
    }

    pub async fn routing_identity(
        &self,
        input: &PullRequestRoutingIdentityInput,
    ) -> Result<PullRequestRoutingIdentityResult, PullRequestError> {
        self.inner.routing_identity(input).await
    }

    /// Runs `operation` as the account the reference expects (a no-op wrapper when it expects
    /// none). Reads made inside it are cached apart from every other credential's.
    pub async fn with_routing_credential<T, E, F>(
        &self,
        input: &PullRequestRef,
        operation: F,
    ) -> Result<T, E>
    where
        F: Future<Output = Result<T, E>>,
        E: From<PullRequestError>,
    {
        self.inner.with_routing_credential(input, operation).await
    }

    /// The live fields of a pull request. `recover_transient_failure: Some(false)` asks for a
    /// strict read (a held value is used only for a merged pull request).
    pub async fn summary(
        &self,
        input: &PullRequestRef,
        recover_transient_failure: Option<bool>,
    ) -> Result<PullRequestSummary, PullRequestError> {
        let reference = self.inner.credential_ref(input).await?;
        self.inner.summary(&reference, recover_transient_failure).await
    }

    /// The host-native stack the pull request belongs to, or `None` when it is not in one or the
    /// host keeps no such object. Cached like a summary; a stack changes about as often.
    pub async fn stack(
        &self,
        input: &PullRequestRef,
        include_details: Option<bool>,
    ) -> Result<Option<PullRequestStack>, PullRequestError> {
        let reference = self.inner.credential_ref(input).await?;
        self.inner.stack(&reference, include_details).await
    }

    /// Pull requests an action saw merge.
    pub fn subscribe_merges(&self) -> BoxStream<'static, PullRequestMergeEvent> {
        receiver_stream(self.inner.merged.subscribe())
    }

    /// Pull requests a detail read saw for the first time or in a new state (opened, closed,
    /// merged), so thread links can catch up before the next sync sweep. Best effort.
    pub fn subscribe_state_changes(&self) -> BoxStream<'static, ThreadPullRequestKey> {
        receiver_stream(self.inner.state_changes.subscribe())
    }

    /// The state a read routed to another environment saw. A new one reaches
    /// `subscribe_state_changes`, so the host is asked again; the report itself is never trusted.
    pub async fn report_state(
        &self,
        input: &PullRequestReportStateInput,
    ) -> Result<(), PullRequestError> {
        self.inner.report_state(input).await
    }

    /// The current refresh revision, then every later one (revisions above zero only).
    pub fn subscribe_refreshes(&self) -> BoxStream<'static, u64> {
        self.inner.refreshes.changes()
    }

    pub async fn refresh_after_turn(&self, project_id: &ProjectId) {
        self.inner.refresh_after_turn(project_id).await;
    }

    pub async fn detail(
        &self,
        input: &PullRequestRef,
    ) -> Result<PullRequestDetail, PullRequestError> {
        let reference = self.inner.credential_ref(input).await?;
        self.inner.detail(&reference).await
    }

    pub async fn preview(
        &self,
        input: &PullRequestRef,
    ) -> Result<PullRequestPreview, PullRequestError> {
        let reference = self.inner.credential_ref(input).await?;
        self.inner.preview(&reference).await
    }

    pub async fn checks(
        &self,
        input: &PullRequestRef,
    ) -> Result<Option<PullRequestChecks>, PullRequestError> {
        let reference = self.inner.credential_ref(input).await?;
        self.inner.checks(&reference).await
    }

    /// What a pull request watch compares between passes, so it reads detail and activity only
    /// when something moved. `None` when the host has no fingerprint or gave none for this one.
    pub async fn watch_fingerprint(
        &self,
        input: &PullRequestRef,
    ) -> Result<Option<ProviderChangeRequestWatchFingerprint>, PullRequestError> {
        let reference = self.inner.credential_ref(input).await?;
        self.inner.watch_fingerprint(&reference).await
    }

    pub async fn activity(
        &self,
        input: &PullRequestRef,
    ) -> Result<PullRequestActivity, PullRequestError> {
        let reference = self.inner.credential_ref(input).await?;
        self.inner.activity(&reference).await
    }

    pub async fn thread_comments(
        &self,
        input: &PullRequestThreadCommentsInput,
    ) -> Result<PullRequestThreadCommentsResult, PullRequestError> {
        self.inner.thread_comments(input).await
    }

    pub async fn diff(
        &self,
        input: &PullRequestDiffInput,
    ) -> Result<PullRequestDiffResult, PullRequestError> {
        let reference = self.inner.credential_ref(&input.pull_request_ref()).await?;
        self.inner.diff(&reference, input).await
    }

    pub async fn diff_file_contents(
        &self,
        input: &PullRequestDiffFileContentsInput,
    ) -> Result<PullRequestDiffFileContentsResult, PullRequestError> {
        self.inner.diff_file_contents(input).await
    }

    pub async fn files_viewed(
        &self,
        input: &PullRequestRef,
    ) -> Result<PullRequestFilesViewedResult, PullRequestError> {
        let reference = self.inner.credential_ref(input).await?;
        self.inner.files_viewed(&reference).await
    }

    pub async fn set_files_viewed(
        &self,
        input: &PullRequestSetFilesViewedInput,
    ) -> Result<(), PullRequestError> {
        self.inner.set_files_viewed(input).await
    }

    pub async fn run_action(&self, input: &PullRequestActionInput) -> Result<(), PullRequestError> {
        self.inner.run_action_and_invalidate(input).await
    }

    pub async fn update(&self, input: &PullRequestUpdateInput) -> Result<(), PullRequestError> {
        self.inner
            .invalidated_by_mutation(&input.pull_request_ref(), self.inner.update(input))
            .await
    }

    pub async fn comment(&self, input: &PullRequestCommentInput) -> Result<(), PullRequestError> {
        self.inner
            .invalidated_by_mutation(&input.pull_request_ref(), self.inner.comment(input))
            .await
    }

    pub async fn update_comment(
        &self,
        input: &PullRequestCommentUpdateInput,
    ) -> Result<(), PullRequestError> {
        self.inner
            .invalidated_by_mutation(&input.pull_request_ref(), self.inner.update_comment(input))
            .await
    }

    pub async fn submit_review(
        &self,
        input: &PullRequestSubmitReviewInput,
    ) -> Result<(), PullRequestError> {
        self.inner
            .invalidated_by_mutation(&input.pull_request_ref(), self.inner.submit_review(input))
            .await
    }

    pub async fn reply_to_thread(
        &self,
        input: &PullRequestThreadReplyInput,
    ) -> Result<(), PullRequestError> {
        self.inner
            .invalidated_by_mutation(&input.pull_request_ref(), self.inner.reply_to_thread(input))
            .await
    }

    pub async fn set_thread_resolution(
        &self,
        input: &PullRequestThreadResolutionInput,
    ) -> Result<(), PullRequestError> {
        self.inner
            .invalidated_by_mutation(
                &input.pull_request_ref(),
                self.inner.set_thread_resolution(input),
            )
            .await
    }

    pub async fn set_reaction(
        &self,
        input: &PullRequestReactionInput,
    ) -> Result<(), PullRequestError> {
        self.inner
            .invalidated_by_mutation(&input.pull_request_ref(), self.inner.set_reaction(input))
            .await
    }

    /// The candidate list is deliberately read fresh per menu-open, so it stays uncached.
    pub async fn reviewer_candidates(
        &self,
        input: &PullRequestRef,
    ) -> Result<PullRequestReviewerCandidateList, PullRequestError> {
        self.inner.reviewer_candidates(input).await
    }

    pub async fn request_reviewers(
        &self,
        input: &PullRequestReviewerRequestInput,
    ) -> Result<(), PullRequestError> {
        self.inner
            .invalidated_by_mutation(
                &input.pull_request_ref(),
                self.inner.request_reviewers(input),
            )
            .await
    }

    pub async fn label_candidates(
        &self,
        input: &PullRequestRef,
    ) -> Result<PullRequestLabelCandidateList, PullRequestError> {
        self.inner.label_candidates(input).await
    }

    pub async fn set_labels(
        &self,
        input: &PullRequestLabelChangeInput,
    ) -> Result<(), PullRequestError> {
        self.inner
            .invalidated_by_mutation(&input.pull_request_ref(), self.inner.set_labels(input))
            .await
    }

    /// Forget what the service has cached, so the next read asks the host. With a reference it
    /// forgets that one change request's detail and diff; without one it forgets the listings.
    /// `notify_readers` also tells every subscriber to read again.
    pub async fn invalidate(&self, input: &PullRequestInvalidateInput, notify_readers: bool) {
        self.inner.invalidate(input, notify_readers).await;
    }
}

/// What the review guide needs of the service.
#[async_trait]
impl ReviewGuidePullRequests for PullRequestService {
    async fn detail(&self, input: &PullRequestRef) -> Result<PullRequestDetail, PullRequestError> {
        PullRequestService::detail(self, input).await
    }

    async fn activity(
        &self,
        input: &PullRequestRef,
    ) -> Result<PullRequestActivity, PullRequestError> {
        PullRequestService::activity(self, input).await
    }

    async fn stack(
        &self,
        input: &PullRequestRef,
    ) -> Result<Option<PullRequestStack>, PullRequestError> {
        PullRequestService::stack(self, input, None).await
    }

    async fn invalidate(&self, reference: &PullRequestRef) {
        PullRequestService::invalidate(
            self,
            &PullRequestInvalidateInput {
                reference: Some(reference.clone()),
                files_viewed_only: None,
            },
            false,
        )
        .await;
    }
}
