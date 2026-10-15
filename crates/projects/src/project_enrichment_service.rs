//! Port of upstream/apps/server/src/project/ProjectEnrichmentService.ts
//!
//! Background resolution of repository identity and favicon for projects.
//! Readers take what is cached (`peek`) and schedule the rest (`request`), so
//! a snapshot never waits on git or the filesystem.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::{Mutex as AsyncMutex, broadcast, mpsc};
use tokio::task::JoinSet;
use turin_contracts::environment::RepositoryIdentity;

use crate::project_favicon_resolver::{ProjectFaviconResolutionError, ProjectFaviconResolver};
use crate::repository_identity_resolver::{RepositoryIdentityResolver, ResolveOptions};
use crate::ttl_cache::{Clock, TtlCache, system_clock};

const DEFAULT_CACHE_CAPACITY: usize = 512;
const DEFAULT_MAX_PENDING: usize = 512;
const DEFAULT_CONCURRENCY: usize = 4;
const DEFAULT_SUCCESS_TTL: Duration = Duration::from_secs(60);
// A favicon scan checks about 20 files. Shell reads (and PR sync through them) ask for every
// linked project about once a minute, and icons rarely change, so a resolved favicon is kept
// longer than the identity, which is only a hit on RepositoryIdentityResolver's own cache.
// Moving a project's workspace root, and deleting it, invalidate both at once.
const DEFAULT_FAVICON_TTL: Duration = Duration::from_secs(15 * 60);
const DEFAULT_FAILURE_TTL: Duration = Duration::from_secs(5);
/// Upstream `PubSub.sliding(256)`. Lagging subscribers miss the oldest changes.
const CHANGES_CAPACITY: usize = 256;

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectEnrichment {
    pub repository_identity: Option<RepositoryIdentity>,
    pub favicon_path: Option<String>,
    /// True when identity resolution completed, including a cached `None`.
    pub repository_identity_resolved: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectEnrichmentChange {
    pub workspace_root: String,
    pub enrichment: ProjectEnrichment,
    pub repository_identity_resolved: bool,
}

pub struct ProjectEnrichmentServiceOptions {
    pub cache_capacity: usize,
    /// Maximum active and queued roots for each enrichment field.
    pub max_pending: usize,
    /// Worker concurrency for each enrichment field.
    pub concurrency: usize,
    pub success_ttl: Duration,
    pub favicon_ttl: Duration,
    pub failure_ttl: Duration,
    pub clock: Clock,
}

impl Default for ProjectEnrichmentServiceOptions {
    fn default() -> Self {
        Self {
            cache_capacity: DEFAULT_CACHE_CAPACITY,
            max_pending: DEFAULT_MAX_PENDING,
            concurrency: DEFAULT_CONCURRENCY,
            success_ttl: DEFAULT_SUCCESS_TTL,
            favicon_ttl: DEFAULT_FAVICON_TTL,
            failure_ttl: DEFAULT_FAILURE_TTL,
            clock: system_clock(),
        }
    }
}

/// Repository identity lookup. Upstream `RepositoryIdentityResolver.resolve`
/// has no error channel (git failures resolve to `None`), so neither does this.
#[async_trait]
pub trait RepositoryIdentitySource: Send + Sync {
    async fn resolve(&self, workspace_root: &str) -> Option<RepositoryIdentity>;
}

#[async_trait]
impl RepositoryIdentitySource for RepositoryIdentityResolver {
    async fn resolve(&self, workspace_root: &str) -> Option<RepositoryIdentity> {
        RepositoryIdentityResolver::resolve(self, workspace_root, ResolveOptions::default()).await
    }
}

/// Favicon lookup, the `resolvePath` seam of `ProjectFaviconResolver`.
#[async_trait]
pub trait FaviconSource: Send + Sync {
    async fn resolve_path(
        &self,
        workspace_root: &str,
    ) -> Result<Option<String>, ProjectFaviconResolutionError>;
}

#[async_trait]
impl FaviconSource for ProjectFaviconResolver {
    async fn resolve_path(
        &self,
        workspace_root: &str,
    ) -> Result<Option<String>, ProjectFaviconResolutionError> {
        ProjectFaviconResolver::resolve_path(self, workspace_root, None).await
    }
}

type FaviconResult = Result<Option<String>, Arc<ProjectFaviconResolutionError>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum LaneKind {
    RepositoryIdentity,
    Favicon,
}

impl LaneKind {
    fn field(self) -> &'static str {
        match self {
            Self::RepositoryIdentity => "repositoryIdentity",
            Self::Favicon => "faviconPath",
        }
    }
}

/// Upstream `EnrichmentWorkLane`: a deduplicated, bounded queue.
struct WorkLane {
    max_pending: usize,
    pending: Mutex<HashSet<String>>,
    sender: mpsc::Sender<String>,
    receiver: AsyncMutex<mpsc::Receiver<String>>,
}

impl WorkLane {
    fn new(max_pending: usize) -> Self {
        let (sender, receiver) = mpsc::channel(max_pending);
        Self {
            max_pending,
            pending: Mutex::new(HashSet::new()),
            sender,
            receiver: AsyncMutex::new(receiver),
        }
    }

    fn reserve_pending(&self, workspace_root: &str) -> bool {
        let mut pending = self.pending.lock();
        if pending.contains(workspace_root) || pending.len() >= self.max_pending {
            return false;
        }
        pending.insert(workspace_root.to_owned());
        true
    }

    fn remove_pending(&self, workspace_root: &str) {
        self.pending.lock().remove(workspace_root);
    }
}

struct Inner {
    identity_source: Arc<dyn RepositoryIdentitySource>,
    favicon_source: Arc<dyn FaviconSource>,
    success_ttl: Duration,
    favicon_ttl: Duration,
    failure_ttl: Duration,
    identity_cache: TtlCache<String, Option<RepositoryIdentity>>,
    favicon_cache: TtlCache<String, FaviconResult>,
    identity_lane: WorkLane,
    favicon_lane: WorkLane,
    changes: broadcast::Sender<ProjectEnrichmentChange>,
}

/// Upstream `ProjectEnrichmentService`. Dropping it aborts its workers.
pub struct ProjectEnrichmentService {
    inner: Arc<Inner>,
    workers: JoinSet<()>,
}

impl ProjectEnrichmentService {
    /// Spawns the lane workers, so this must run inside a tokio runtime.
    pub fn new(
        identity_source: Arc<dyn RepositoryIdentitySource>,
        favicon_source: Arc<dyn FaviconSource>,
        options: ProjectEnrichmentServiceOptions,
    ) -> Self {
        let cache_capacity = options.cache_capacity.max(1);
        let max_pending = options.max_pending.max(1);
        let concurrency = options.concurrency.max(1);
        let (changes, _) = broadcast::channel(CHANGES_CAPACITY);
        let inner = Arc::new(Inner {
            identity_source,
            favicon_source,
            success_ttl: options.success_ttl,
            favicon_ttl: options.favicon_ttl,
            failure_ttl: options.failure_ttl,
            identity_cache: TtlCache::new(cache_capacity, options.clock.clone()),
            favicon_cache: TtlCache::new(cache_capacity, options.clock),
            identity_lane: WorkLane::new(max_pending),
            favicon_lane: WorkLane::new(max_pending),
            changes,
        });
        let mut workers = JoinSet::new();
        for _ in 0..concurrency {
            workers.spawn(run_lane_worker(
                Arc::clone(&inner),
                LaneKind::RepositoryIdentity,
            ));
            workers.spawn(run_lane_worker(Arc::clone(&inner), LaneKind::Favicon));
        }
        Self { inner, workers }
    }

    /// Cached metadata only; never starts or awaits filesystem work.
    pub fn peek(&self, workspace_root: &str) -> ProjectEnrichment {
        self.inner.peek(workspace_root)
    }

    /// Schedule missing metadata for bounded background resolution.
    pub fn request(&self, workspace_root: &str) {
        self.inner.request(workspace_root);
    }

    /// Cached metadata now, plus scheduling anything missing.
    pub fn get_available(&self, workspace_root: &str) -> ProjectEnrichment {
        let available = self.peek(workspace_root);
        self.request(workspace_root);
        available
    }

    pub fn invalidate<'a>(&self, workspace_roots: impl IntoIterator<Item = &'a str>) {
        let roots: HashSet<&str> = workspace_roots.into_iter().collect();
        for workspace_root in roots {
            self.inner
                .identity_cache
                .invalidate(&workspace_root.to_owned());
            self.inner
                .favicon_cache
                .invalidate(&workspace_root.to_owned());
        }
    }

    pub fn subscribe_changes(&self) -> broadcast::Receiver<ProjectEnrichmentChange> {
        self.inner.changes.subscribe()
    }
}

impl Drop for ProjectEnrichmentService {
    fn drop(&mut self) {
        self.workers.abort_all();
    }
}

impl Inner {
    fn lane(&self, kind: LaneKind) -> &WorkLane {
        match kind {
            LaneKind::RepositoryIdentity => &self.identity_lane,
            LaneKind::Favicon => &self.favicon_lane,
        }
    }

    fn peek(&self, workspace_root: &str) -> ProjectEnrichment {
        let key = workspace_root.to_owned();
        let identity = self.identity_cache.get(&key);
        let favicon = self.favicon_cache.get(&key);
        ProjectEnrichment {
            repository_identity: identity.clone().flatten(),
            favicon_path: favicon.and_then(|result| result.ok().flatten()),
            repository_identity_resolved: identity.is_some(),
        }
    }

    fn request(&self, workspace_root: &str) {
        let key = workspace_root.to_owned();
        if !self.identity_cache.contains(&key) {
            self.request_lane(LaneKind::RepositoryIdentity, workspace_root);
        }
        if !self.favicon_cache.contains(&key) {
            self.request_lane(LaneKind::Favicon, workspace_root);
        }
    }

    fn request_lane(&self, kind: LaneKind, workspace_root: &str) {
        let lane = self.lane(kind);
        if !lane.reserve_pending(workspace_root) {
            return;
        }
        if lane.sender.try_send(workspace_root.to_owned()).is_err() {
            lane.remove_pending(workspace_root);
            tracing::warn!(
                workspace_root,
                field = kind.field(),
                "Project metadata enrichment queue is full"
            );
        }
    }

    /// Upstream `Cache.get` on the identity cache. Identity lookups always
    /// succeed, so the TTL is always the success TTL.
    async fn repository_identity(&self, workspace_root: &str) -> Option<RepositoryIdentity> {
        let key = workspace_root.to_owned();
        if let Some(identity) = self.identity_cache.get(&key) {
            return identity;
        }
        let epoch = self.identity_cache.epoch();
        let identity = self.identity_source.resolve(workspace_root).await;
        self.identity_cache
            .insert_if_unchanged(epoch, key, identity.clone(), self.success_ttl);
        identity
    }

    /// Upstream `Cache.get` on the favicon cache. Failures get the failure TTL.
    async fn favicon(&self, workspace_root: &str) -> FaviconResult {
        let key = workspace_root.to_owned();
        if let Some(result) = self.favicon_cache.get(&key) {
            return result;
        }
        let epoch = self.favicon_cache.epoch();
        let result = self
            .favicon_source
            .resolve_path(workspace_root)
            .await
            .map_err(Arc::new);
        let ttl = if result.is_ok() {
            self.favicon_ttl
        } else {
            self.failure_ttl
        };
        self.favicon_cache
            .insert_if_unchanged(epoch, key, result.clone(), ttl);
        result
    }

    async fn resolve_repository_identity(&self, workspace_root: &str) {
        let repository_identity = self.repository_identity(workspace_root).await;
        // Cached favicon only: a favicon scan belongs to its own lane.
        let favicon_path = self
            .favicon_cache
            .get(&workspace_root.to_owned())
            .and_then(|result| result.ok().flatten());
        // Ignored when nobody is subscribed.
        let _ = self.changes.send(ProjectEnrichmentChange {
            workspace_root: workspace_root.to_owned(),
            repository_identity_resolved: true,
            enrichment: ProjectEnrichment {
                repository_identity,
                favicon_path,
                repository_identity_resolved: true,
            },
        });
    }

    async fn resolve_favicon(&self, workspace_root: &str) {
        let Err(error) = self.favicon(workspace_root).await else {
            return;
        };
        // A removed workspace is expected, so it is not worth a warning.
        if error.is_missing_workspace_root() {
            return;
        }
        tracing::warn!(
            workspace_root,
            field = "faviconPath",
            error = %error,
            "Failed to enrich optional project metadata"
        );
    }
}

async fn run_lane_worker(inner: Arc<Inner>, kind: LaneKind) {
    loop {
        let workspace_root = {
            let mut receiver = inner.lane(kind).receiver.lock().await;
            match receiver.recv().await {
                Some(workspace_root) => workspace_root,
                None => return,
            }
        };
        match kind {
            LaneKind::RepositoryIdentity => {
                inner.resolve_repository_identity(&workspace_root).await
            }
            LaneKind::Favicon => inner.resolve_favicon(&workspace_root).await,
        }
        inner.lane(kind).remove_pending(&workspace_root);
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::future::pending;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use parking_lot::Mutex;
    use pretty_assertions::assert_eq;
    use tokio::sync::Notify;
    use turin_contracts::base_schemas::TrimmedNonEmptyString;
    use turin_contracts::environment::{
        GitRemoteSource, RepositoryIdentity, RepositoryIdentityLocator,
    };

    use super::{
        FaviconSource, ProjectEnrichment, ProjectEnrichmentService,
        ProjectEnrichmentServiceOptions, RepositoryIdentitySource,
    };
    use crate::project_favicon_resolver::{
        ProjectFaviconCause, ProjectFaviconOperation, ProjectFaviconResolutionError,
    };
    use crate::ttl_cache::test_support::TestClock;

    fn identity(workspace_root: &str, version: u32) -> RepositoryIdentity {
        RepositoryIdentity {
            canonical_key: TrimmedNonEmptyString::new(format!(
                "example.test/v{version}{workspace_root}"
            ))
            .expect("key"),
            locator: RepositoryIdentityLocator {
                source: GitRemoteSource,
                remote_name: TrimmedNonEmptyString::new("origin").expect("name"),
                remote_url: TrimmedNonEmptyString::new(format!(
                    "https://example.test/v{version}{workspace_root}.git"
                ))
                .expect("url"),
            },
            web_url: None,
            root_path: TrimmedNonEmptyString::new(workspace_root).ok(),
            display_name: None,
            provider: None,
            owner: None,
            name: None,
            origin: None,
        }
    }

    /// Closure-backed identity source.
    struct FnIdentity<F>(F);

    #[async_trait]
    impl<F, Fut> RepositoryIdentitySource for FnIdentity<F>
    where
        F: Fn(String) -> Fut + Send + Sync,
        Fut: Future<Output = Option<RepositoryIdentity>> + Send,
    {
        async fn resolve(&self, workspace_root: &str) -> Option<RepositoryIdentity> {
            (self.0)(workspace_root.to_owned()).await
        }
    }

    /// Closure-backed favicon source.
    struct FnFavicon<F>(F);

    #[async_trait]
    impl<F, Fut> FaviconSource for FnFavicon<F>
    where
        F: Fn(String) -> Fut + Send + Sync,
        Fut: Future<Output = Result<Option<String>, ProjectFaviconResolutionError>> + Send,
    {
        async fn resolve_path(
            &self,
            workspace_root: &str,
        ) -> Result<Option<String>, ProjectFaviconResolutionError> {
            (self.0)(workspace_root.to_owned()).await
        }
    }

    async fn ok_favicon(root: String) -> Result<Option<String>, ProjectFaviconResolutionError> {
        Ok(Some(format!("{root}/favicon.svg")))
    }

    fn service(
        identity_source: impl RepositoryIdentitySource + 'static,
        favicon_source: impl FaviconSource + 'static,
        options: ProjectEnrichmentServiceOptions,
    ) -> ProjectEnrichmentService {
        ProjectEnrichmentService::new(Arc::new(identity_source), Arc::new(favicon_source), options)
    }

    async fn wait_for_available(
        service: &ProjectEnrichmentService,
        workspace_root: &str,
        predicate: impl Fn(&ProjectEnrichment) -> bool,
    ) -> ProjectEnrichment {
        for _ in 0..100 {
            let available = service.peek(workspace_root);
            if predicate(&available) {
                return available;
            }
            tokio::task::yield_now().await;
        }
        panic!("Project metadata for {workspace_root} was not resolved in time.");
    }

    #[tokio::test]
    async fn preserves_repository_identity_when_the_favicon_resolver_fails() {
        let service = service(
            FnIdentity(|root: String| async move { Some(identity(&root, 1)) }),
            FnFavicon(|root: String| async move {
                if root == "/favicon-fails" {
                    return Err(ProjectFaviconResolutionError {
                        operation: ProjectFaviconOperation::StatCandidate,
                        workspace_root: root,
                        relative_path: None,
                        absolute_path: None,
                        cause: Box::new(ProjectFaviconCause::Io(std::io::Error::other(
                            "favicon resolver failed",
                        ))),
                    });
                }
                ok_favicon(root).await
            }),
            ProjectEnrichmentServiceOptions::default(),
        );

        service.request("/favicon-fails");
        let failed = wait_for_available(&service, "/favicon-fails", |value| {
            value.repository_identity.is_some()
        })
        .await;
        assert_eq!(
            failed
                .repository_identity
                .map(|identity| identity.canonical_key.as_str().to_owned()),
            Some("example.test/v1/favicon-fails".to_owned())
        );
        assert_eq!(failed.favicon_path, None);
    }

    #[tokio::test]
    async fn publishes_repository_completion_while_favicon_enrichment_is_still_pending() {
        let release = Arc::new(Notify::new());
        let favicon_release = Arc::clone(&release);
        let service = service(
            FnIdentity(|root: String| async move { Some(identity(&root, 1)) }),
            FnFavicon(move |root: String| {
                let favicon_release = Arc::clone(&favicon_release);
                async move {
                    favicon_release.notified().await;
                    Ok(Some(format!("{root}/favicon.svg")))
                }
            }),
            ProjectEnrichmentServiceOptions::default(),
        );
        let mut changes = service.subscribe_changes();

        service.request("/completed");
        let change = changes.recv().await.expect("change");
        assert_eq!(change.workspace_root, "/completed");
        assert!(change.repository_identity_resolved);
        assert_eq!(
            change
                .enrichment
                .repository_identity
                .map(|identity| identity.canonical_key.as_str().to_owned()),
            Some("example.test/v1/completed".to_owned())
        );
        assert_eq!(change.enrichment.favicon_path, None);
        release.notify_waiters();
    }

    #[tokio::test]
    async fn keeps_repository_workers_available_when_every_favicon_worker_is_hung() {
        let started = Arc::new(Mutex::new(0usize));
        let all_started = Arc::new(Notify::new());
        let favicon_started = Arc::clone(&started);
        let favicon_all_started = Arc::clone(&all_started);
        let service = service(
            FnIdentity(|root: String| async move { Some(identity(&root, 1)) }),
            FnFavicon(move |_root: String| {
                let started = Arc::clone(&favicon_started);
                let all_started = Arc::clone(&favicon_all_started);
                async move {
                    let count = {
                        let mut started = started.lock();
                        *started += 1;
                        *started
                    };
                    if count == 2 {
                        all_started.notify_one();
                    }
                    pending::<()>().await;
                    Ok(None)
                }
            }),
            ProjectEnrichmentServiceOptions {
                cache_capacity: 8,
                max_pending: 4,
                concurrency: 2,
                ..Default::default()
            },
        );

        service.request("/favicon-hung-a");
        service.request("/favicon-hung-b");
        all_started.notified().await;

        service.request("/repository-later");
        let available = wait_for_available(&service, "/repository-later", |value| {
            value.repository_identity.is_some()
        })
        .await;
        assert_eq!(
            available
                .repository_identity
                .map(|identity| identity.canonical_key.as_str().to_owned()),
            Some("example.test/v1/repository-later".to_owned())
        );
        assert_eq!(*started.lock(), 2);
    }

    #[tokio::test]
    async fn get_available_returns_immediately_while_repository_identity_is_still_unresolved() {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let identity_started = Arc::clone(&started);
        let identity_release = Arc::clone(&release);
        let service = service(
            FnIdentity(move |root: String| {
                let identity_started = Arc::clone(&identity_started);
                let identity_release = Arc::clone(&identity_release);
                async move {
                    identity_started.notify_one();
                    identity_release.notified().await;
                    Some(identity(&root, 1))
                }
            }),
            FnFavicon(|root: String| ok_favicon(root)),
            ProjectEnrichmentServiceOptions::default(),
        );

        // The non-blocking path must not wait on hung identity resolution.
        let immediate = service.get_available("/pending-identity");
        assert_eq!(immediate.repository_identity, None);
        assert_eq!(immediate.favicon_path, None);
        assert!(!immediate.repository_identity_resolved);

        started.notified().await;
        release.notify_one();
        let resolved = wait_for_available(&service, "/pending-identity", |value| {
            value.repository_identity.is_some()
        })
        .await;
        assert_eq!(
            resolved
                .repository_identity
                .map(|identity| identity.canonical_key.as_str().to_owned()),
            Some("example.test/v1/pending-identity".to_owned())
        );
        assert!(resolved.repository_identity_resolved);
    }

    #[tokio::test]
    async fn reports_successful_cached_null_as_resolved_while_cold_lookups_stay_unresolved() {
        let started = Arc::new(Notify::new());
        let identity_started = Arc::clone(&started);
        let service = service(
            FnIdentity(move |root: String| {
                let identity_started = Arc::clone(&identity_started);
                async move {
                    if root == "/no-remote" {
                        return None;
                    }
                    identity_started.notify_one();
                    pending::<()>().await;
                    None
                }
            }),
            FnFavicon(|_root: String| async move { Ok(None) }),
            ProjectEnrichmentServiceOptions::default(),
        );
        let mut changes = service.subscribe_changes();

        let cold = service.peek("/never-requested");
        assert_eq!(cold.repository_identity, None);
        assert!(!cold.repository_identity_resolved);

        let in_flight = service.get_available("/in-flight");
        assert_eq!(in_flight.repository_identity, None);
        assert!(!in_flight.repository_identity_resolved);
        started.notified().await;
        assert!(!service.peek("/in-flight").repository_identity_resolved);

        service.request("/no-remote");
        let null_change = changes.recv().await.expect("change");
        assert_eq!(null_change.workspace_root, "/no-remote");
        assert!(null_change.repository_identity_resolved);
        assert_eq!(null_change.enrichment.repository_identity, None);
        assert!(null_change.enrichment.repository_identity_resolved);

        // A warm null stays resolved and does not republish.
        let warm_null = service.get_available("/no-remote");
        assert_eq!(warm_null.repository_identity, None);
        assert!(warm_null.repository_identity_resolved);
    }

    #[tokio::test]
    async fn deduplicates_requests_bounds_pending_work_and_reloads_invalidated_roots() {
        let first_started = Arc::new(Notify::new());
        let release_first = Arc::new(Notify::new());
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let version = Arc::new(Mutex::new(1u32));
        let identity_calls = Arc::clone(&calls);
        let identity_version = Arc::clone(&version);
        let first_started_signal = Arc::clone(&first_started);
        let release_first_signal = Arc::clone(&release_first);
        let service = service(
            FnIdentity(move |root: String| {
                let calls = Arc::clone(&identity_calls);
                let version = Arc::clone(&identity_version);
                let first_started_signal = Arc::clone(&first_started_signal);
                let release_first_signal = Arc::clone(&release_first_signal);
                async move {
                    calls.lock().push(root.clone());
                    let current = *version.lock();
                    if root == "/first" && current == 1 {
                        first_started_signal.notify_one();
                        release_first_signal.notified().await;
                    }
                    Some(identity(&root, *version.lock()))
                }
            }),
            FnFavicon(|root: String| ok_favicon(root)),
            ProjectEnrichmentServiceOptions {
                cache_capacity: 8,
                max_pending: 2,
                concurrency: 1,
                ..Default::default()
            },
        );

        for _ in 0..20 {
            service.request("/first");
        }
        first_started.notified().await;
        service.request("/second");
        service.request("/dropped");
        assert_eq!(*calls.lock(), vec!["/first".to_owned()]);

        release_first.notify_one();
        wait_for_available(&service, "/second", |value| {
            value.repository_identity.is_some()
        })
        .await;
        assert_eq!(
            *calls.lock(),
            vec!["/first".to_owned(), "/second".to_owned()]
        );

        service.request("/dropped");
        wait_for_available(&service, "/dropped", |value| {
            value.repository_identity.is_some()
        })
        .await;
        assert_eq!(
            *calls.lock(),
            vec![
                "/first".to_owned(),
                "/second".to_owned(),
                "/dropped".to_owned()
            ]
        );

        *version.lock() = 2;
        service.invalidate(["/first"]);
        assert_eq!(service.get_available("/first").repository_identity, None);
        let refreshed = wait_for_available(&service, "/first", |value| {
            value
                .repository_identity
                .as_ref()
                .is_some_and(|identity| identity.canonical_key.as_str() == "example.test/v2/first")
        })
        .await;
        assert!(refreshed.repository_identity.is_some());
        assert_eq!(
            *calls.lock(),
            vec![
                "/first".to_owned(),
                "/second".to_owned(),
                "/dropped".to_owned(),
                "/first".to_owned()
            ]
        );
    }

    #[tokio::test]
    async fn rescans_a_favicon_only_after_15_minutes() {
        let clock = TestClock::new();
        let scans = Arc::new(Mutex::new(0usize));
        let favicon_scans = Arc::clone(&scans);
        let service = service(
            FnIdentity(|root: String| async move { Some(identity(&root, 1)) }),
            // Each scan returns a new path, so a wait can tell a rescan from the cached value.
            FnFavicon(move |root: String| {
                let favicon_scans = Arc::clone(&favicon_scans);
                async move {
                    let scan = {
                        let mut scans = favicon_scans.lock();
                        *scans += 1;
                        *scans
                    };
                    Ok(Some(format!("{root}/favicon-{scan}.svg")))
                }
            }),
            ProjectEnrichmentServiceOptions {
                clock: clock.clock(),
                ..Default::default()
            },
        );

        service.get_available("/repo");
        wait_for_available(&service, "/repo", |value| {
            value.favicon_path.as_deref() == Some("/repo/favicon-1.svg")
        })
        .await;

        // Callers such as the shell stream read projects far more often than this.
        for _ in 1..15 {
            clock.adjust(Duration::from_secs(60));
            service.get_available("/repo");
        }
        assert_eq!(*scans.lock(), 1);

        clock.adjust(Duration::from_secs(60));
        service.get_available("/repo");
        wait_for_available(&service, "/repo", |value| {
            value.favicon_path.as_deref() == Some("/repo/favicon-2.svg")
        })
        .await;
        assert_eq!(*scans.lock(), 2);
    }

    #[tokio::test]
    async fn follows_repository_identity_changes_within_a_minute_without_rescanning_the_favicon() {
        let clock = TestClock::new();
        // 0: no repository yet, then one version per remote.
        let remote_version = Arc::new(Mutex::new(0u32));
        let scans = Arc::new(Mutex::new(0usize));
        let identity_version = Arc::clone(&remote_version);
        let favicon_scans = Arc::clone(&scans);
        let service = service(
            FnIdentity(move |root: String| {
                let version = *identity_version.lock();
                async move { (version != 0).then(|| identity(&root, version)) }
            }),
            FnFavicon(move |_root: String| {
                let favicon_scans = Arc::clone(&favicon_scans);
                async move {
                    *favicon_scans.lock() += 1;
                    Ok(None)
                }
            }),
            ProjectEnrichmentServiceOptions {
                clock: clock.clock(),
                ..Default::default()
            },
        );

        service.get_available("/folder");
        wait_for_available(&service, "/folder", |value| {
            value.repository_identity_resolved
        })
        .await;
        assert_eq!(service.peek("/folder").repository_identity, None);

        // The folder is published as a repository, then its remote moves to another host.
        for version in [1, 2] {
            *remote_version.lock() = version;
            clock.adjust(Duration::from_secs(60));
            service.get_available("/folder");
            let expected = format!("example.test/v{version}/folder");
            let available = wait_for_available(&service, "/folder", |value| {
                value
                    .repository_identity
                    .as_ref()
                    .is_some_and(|identity| identity.canonical_key.as_str() == expected)
            })
            .await;
            assert!(available.repository_identity.is_some());
        }
        assert_eq!(*scans.lock(), 1);
    }

    #[test]
    fn enrichment_defaults_match_upstream_ttls() {
        let options = ProjectEnrichmentServiceOptions::default();
        assert_eq!(options.success_ttl, Duration::from_secs(60));
        assert_eq!(options.favicon_ttl, Duration::from_secs(15 * 60));
        assert_eq!(options.failure_ttl, Duration::from_secs(5));
        assert_eq!(options.max_pending, 512);
        assert_eq!(options.concurrency, 4);
    }

    #[tokio::test]
    async fn an_invalidation_during_a_lookup_is_not_refilled_by_its_result() {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let identity_source = {
            let (started, release, calls) = (
                Arc::clone(&started),
                Arc::clone(&release),
                Arc::clone(&calls),
            );
            FnIdentity(move |root: String| {
                let (started, release, calls) = (
                    Arc::clone(&started),
                    Arc::clone(&release),
                    Arc::clone(&calls),
                );
                async move {
                    let call = calls.fetch_add(1, Ordering::SeqCst) as u32 + 1;
                    if call == 1 {
                        started.notify_one();
                        release.notified().await;
                    }
                    Some(identity(&root, call))
                }
            })
        };
        let service = service(
            identity_source,
            FnFavicon(ok_favicon),
            ProjectEnrichmentServiceOptions::default(),
        );
        let mut changes = service.subscribe_changes();

        service.request("/moved");
        started.notified().await;
        // A move or delete invalidates the root while the first lookup is still running.
        service.invalidate(["/moved"]);
        release.notify_one();

        // The identity lane reports once its lookup is stored or dropped.
        changes.recv().await.expect("identity change");
        assert_eq!(
            service.peek("/moved").repository_identity,
            None,
            "the pre-invalidation result must not be stored"
        );

        let mut resolved = None;
        for _ in 0..100 {
            service.request("/moved");
            let available = service.peek("/moved");
            if available.repository_identity.is_some() {
                resolved = Some(available);
                break;
            }
            tokio::task::yield_now().await;
        }
        let resolved = resolved.expect("the next lookup resolves");
        assert_eq!(
            resolved
                .repository_identity
                .map(|identity| identity.canonical_key.as_str().to_owned()),
            Some("example.test/v2/moved".to_owned())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
