//! Port of upstream/apps/server/src/vcs/VcsStatusBroadcaster.ts
//!
//! One remote poller per normalized cwd. Further subscribers only bump a
//! demand count (the coalesce). Timing:
//! - no cached remote: refresh immediately, then on the configured interval
//! - cached remote: wait out the interval before the first refresh
//! - interval 0: one immediate read with `refreshUpstream: false`, then the
//!   loop wakes on the 30s default and does not fetch
//! - failures: `30s * 2^(n-1)`, cap 15 minutes, never shorter than the active interval
//!
//! Snapshots fan out on per-subscriber unbounded `mpsc` channels. `send` does
//! not wait, so a slow subscriber cannot stall publishing. The subscription
//! yields its snapshot before any update queued during load. Pollers are
//! `AbortOnDropHandle`s gated by a `CancellationToken`.

#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::future::join_all;
use futures::lock::Mutex as AsyncMutex;
use parking_lot::Mutex;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use turin_contracts::git::{
    GitCommandError, GitManagerError, GitManagerServiceError, TextGenerationError, VcsPullResult,
    VcsStatusInput, VcsStatusLocalResult, VcsStatusRemoteResult, VcsStatusResult,
    VcsStatusStreamEvent, VcsStatusSubscriptionInput,
};
use turin_contracts::source_control::SourceControlProviderError;

use crate::shared_git::merge_git_status_parts;

const DEFAULT_VCS_STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const VCS_STATUS_REFRESH_FAILURE_BASE_DELAY: Duration = Duration::from_secs(30);
const VCS_STATUS_REFRESH_FAILURE_MAX_DELAY: Duration = Duration::from_secs(15 * 60);
const MAX_FAILURE_DIAGNOSTIC_VALUES: usize = 8;
const MAX_FAILURE_DIAGNOSTIC_VALUE_LENGTH: usize = 128;

/// Methods of upstream `GitWorkflowService` this broadcaster calls.
#[async_trait]
pub trait GitWorkflowService: Send + Sync {
    async fn local_status(&self, cwd: &str)
    -> Result<VcsStatusLocalResult, GitManagerServiceError>;
    async fn remote_status(
        &self,
        cwd: &str,
        options: GitRemoteStatusOptions,
    ) -> Result<Option<VcsStatusRemoteResult>, GitManagerServiceError>;
    async fn invalidate_local_status(&self, cwd: &str);
    async fn invalidate_remote_status(&self, cwd: &str);
    async fn invalidate_status(&self, cwd: &str);
    async fn pull_current_branch(&self, cwd: &str)
    -> Result<VcsPullResult, GitManagerServiceError>;
}

/// `BackgroundPolicy.shouldRunScopeWork({ type: "vcs-status", cwd })`.
#[async_trait]
pub trait BackgroundPolicy: Send + Sync {
    async fn should_run_vcs_status(&self, cwd: &str) -> bool;
}

/// `VcsAutoPullPolicy`. Upstream turns policy errors into `false`.
#[async_trait]
pub trait VcsAutoPullPolicy: Send + Sync {
    async fn is_enabled(&self, cwd: &str) -> bool;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GitRemoteStatusOptions {
    pub refresh_upstream: Option<bool>,
    pub refresh_missing_pull_request: Option<bool>,
}

type IntervalFn = Arc<dyn Fn() -> Duration + Send + Sync>;

#[derive(Clone)]
pub struct StreamStatusOptions {
    interval: IntervalFn,
}

impl StreamStatusOptions {
    pub fn every(duration: Duration) -> Self {
        Self {
            interval: Arc::new(move || duration),
        }
    }
}

impl Default for StreamStatusOptions {
    fn default() -> Self {
        Self::every(DEFAULT_VCS_STATUS_REFRESH_INTERVAL)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRefreshFailureDiagnostics {
    pub reason_count: usize,
    pub failure_count: usize,
    pub failure_tags: Vec<String>,
    pub failure_operations: Vec<String>,
    pub defect_count: usize,
    pub defect_tags: Vec<String>,
    pub interruption_count: usize,
}

#[derive(Debug, Clone)]
pub struct RefreshCause {
    pub reasons: Vec<RefreshCauseReason>,
}

#[derive(Debug, Clone)]
pub enum RefreshCauseReason {
    Fail(RefreshDiagnosticValue),
    Die(RefreshDiagnosticValue),
    Interrupt,
}

#[derive(Debug, Clone)]
pub enum RefreshDiagnosticValue {
    Service(Box<GitManagerServiceError>),
    Error { name: String },
    Type(&'static str),
    Uninspectable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRefreshFailureLog {
    pub cwd_length: usize,
    pub diagnostics: RemoteRefreshFailureDiagnostics,
    pub consecutive_failures: u32,
    pub next_delay_ms: u128,
}

type WarningHook = Arc<dyn Fn(&RemoteRefreshFailureLog) + Send + Sync>;

pub fn remote_refresh_failure_delay(consecutive_failures: u32, configured: Duration) -> Duration {
    let exponent = consecutive_failures.saturating_sub(1).min(16);
    let backoff_ms = VCS_STATUS_REFRESH_FAILURE_BASE_DELAY.as_millis() * (1u128 << exponent);
    let capped_ms = backoff_ms.min(VCS_STATUS_REFRESH_FAILURE_MAX_DELAY.as_millis());
    configured.max(Duration::from_millis(capped_ms as u64))
}

pub fn remote_refresh_failure_diagnostics(cause: &RefreshCause) -> RemoteRefreshFailureDiagnostics {
    let mut failure_tags = Vec::new();
    let mut failure_operations = Vec::new();
    let mut defect_tags = Vec::new();
    let mut failure_count = 0usize;
    let mut defect_count = 0usize;
    let mut interruption_count = 0usize;
    for reason in &cause.reasons {
        match reason {
            RefreshCauseReason::Fail(value) => {
                failure_count += 1;
                add_unique(&mut failure_tags, Some(diagnostic_tag(value)));
                add_unique(&mut failure_operations, diagnostic_operation(value));
            }
            RefreshCauseReason::Die(value) => {
                defect_count += 1;
                add_unique(&mut defect_tags, Some(diagnostic_tag(value)));
            }
            RefreshCauseReason::Interrupt => interruption_count += 1,
        }
    }
    RemoteRefreshFailureDiagnostics {
        reason_count: cause.reasons.len(),
        failure_count,
        failure_tags,
        failure_operations,
        defect_count,
        defect_tags,
        interruption_count,
    }
}

#[async_trait]
trait Sleeper: Send + Sync {
    async fn sleep(&self, duration: Duration);
}

struct TokioSleeper;

#[async_trait]
impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration: Duration) {
        if !duration.is_zero() {
            tokio::time::sleep(duration).await;
        }
    }
}

#[derive(Clone)]
struct CachedValue<T> {
    fingerprint: String,
    value: T,
}

#[derive(Clone, Default)]
struct CachedVcsStatus {
    local: Option<CachedValue<VcsStatusLocalResult>>,
    remote: Option<CachedValue<Option<VcsStatusRemoteResult>>>,
}

struct Subscriber {
    id: u64,
    cwd: String,
    tx: mpsc::UnboundedSender<VcsStatusStreamEvent>,
}

struct RefreshRemoteOptions {
    refresh_upstream: Option<bool>,
    policy_cwds: Vec<String>,
}

struct PulledStatus {
    local: VcsStatusLocalResult,
    remote: Option<VcsStatusRemoteResult>,
}

struct Inner {
    workflow: Arc<dyn GitWorkflowService>,
    background: Arc<dyn BackgroundPolicy>,
    auto_pull: Arc<dyn VcsAutoPullPolicy>,
    sleeper: Arc<dyn Sleeper>,
    warnings: Option<WarningHook>,
    cache: Mutex<HashMap<String, CachedVcsStatus>>,
    subscribers: Mutex<Vec<Subscriber>>,
    next_sub_id: Mutex<u64>,
    remote_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

struct ActivePoller {
    cancel: CancellationToken,
    /// Kept so dropping the poller aborts the task. The field is read by `Drop` of the handle.
    #[allow(dead_code)]
    handle: AbortOnDropHandle<()>,
    subscriber_count: usize,
    demand: Arc<Mutex<HashMap<String, usize>>>,
}

impl Drop for ActivePoller {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct PollerSet {
    pollers: Mutex<HashMap<String, ActivePoller>>,
}

pub struct VcsStatusBroadcaster {
    inner: Arc<Inner>,
    pollers: Arc<PollerSet>,
}

pub struct StatusSubscription {
    snapshot: Option<VcsStatusStreamEvent>,
    rx: mpsc::UnboundedReceiver<VcsStatusStreamEvent>,
    inner: Arc<Inner>,
    pollers: Arc<PollerSet>,
    sub_id: u64,
    normalized_cwd: String,
    demand_cwd: String,
    include_remote: bool,
}

impl VcsStatusBroadcaster {
    pub fn new(
        workflow: Arc<dyn GitWorkflowService>,
        background: Arc<dyn BackgroundPolicy>,
        auto_pull: Arc<dyn VcsAutoPullPolicy>,
    ) -> Self {
        Self::build(
            workflow,
            background,
            auto_pull,
            Arc::new(TokioSleeper),
            None,
        )
    }

    fn build(
        workflow: Arc<dyn GitWorkflowService>,
        background: Arc<dyn BackgroundPolicy>,
        auto_pull: Arc<dyn VcsAutoPullPolicy>,
        sleeper: Arc<dyn Sleeper>,
        warnings: Option<WarningHook>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                workflow,
                background,
                auto_pull,
                sleeper,
                warnings,
                cache: Mutex::new(HashMap::new()),
                subscribers: Mutex::new(Vec::new()),
                next_sub_id: Mutex::new(0),
                remote_locks: Mutex::new(HashMap::new()),
            }),
            pollers: Arc::new(PollerSet {
                pollers: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub async fn get_status(
        &self,
        input: &VcsStatusInput,
    ) -> Result<VcsStatusResult, GitManagerServiceError> {
        self.inner.get_status(input).await
    }

    pub async fn refresh_local_status(
        &self,
        cwd: &str,
    ) -> Result<VcsStatusLocalResult, GitManagerServiceError> {
        self.inner.refresh_local_status(cwd).await
    }

    pub async fn refresh_status(
        &self,
        cwd: &str,
    ) -> Result<VcsStatusResult, GitManagerServiceError> {
        self.inner.refresh_status(cwd).await
    }

    pub async fn refresh_pull_request_status(
        &self,
        raw_cwd: &str,
    ) -> Result<Option<VcsStatusRemoteResult>, GitManagerServiceError> {
        let cwd = normalize_cwd(raw_cwd).await;
        let lock = self.inner.remote_lock(&cwd);
        let _guard = lock.lock().await;
        if self
            .inner
            .cached(&cwd)
            .and_then(|item| item.remote)
            .and_then(|remote| remote.value)
            .is_none()
        {
            return Ok(None);
        }
        let demand_cwds = self.pollers.demand_cwds(&cwd, raw_cwd);
        let should_run = join_all(demand_cwds.iter().map(|demand| {
            let background = Arc::clone(&self.inner.background);
            let demand = demand.clone();
            async move { background.should_run_vcs_status(&demand).await }
        }))
        .await
        .into_iter()
        .any(|enabled| enabled);
        if !should_run {
            return Ok(None);
        }
        let remote = self
            .inner
            .workflow
            .remote_status(
                &cwd,
                GitRemoteStatusOptions {
                    refresh_upstream: Some(false),
                    refresh_missing_pull_request: Some(true),
                },
            )
            .await?;
        Ok(self.inner.update_cached_remote(&cwd, remote, true))
    }

    pub async fn stream_status(
        &self,
        input: &VcsStatusSubscriptionInput,
        options: Option<StreamStatusOptions>,
    ) -> Result<StatusSubscription, GitManagerServiceError> {
        let cwd = normalize_cwd(input.cwd.as_str()).await;
        let (sub_id, rx) = self.inner.subscribe(cwd.clone());
        let initial_local = match self.inner.get_or_load_local(&cwd).await {
            Ok(local) => local,
            Err(error) => {
                self.inner.unsubscribe(sub_id);
                return Err(error);
            }
        };
        let cached = self.inner.cached(&cwd);
        let initial_remote = cached
            .as_ref()
            .and_then(|item| item.remote.as_ref())
            .and_then(|remote| remote.value.clone());
        let include_remote = input.include_remote != Some(false);
        if include_remote {
            let refresh_immediately = cached
                .as_ref()
                .map(|item| item.remote.is_none())
                .unwrap_or(true);
            let options = options.unwrap_or_default();
            self.retain_poller(
                &cwd,
                input.cwd.as_str(),
                options.interval,
                refresh_immediately,
            );
        }
        Ok(StatusSubscription {
            snapshot: Some(VcsStatusStreamEvent::Snapshot {
                local: initial_local,
                remote: initial_remote,
            }),
            rx,
            inner: Arc::clone(&self.inner),
            pollers: Arc::clone(&self.pollers),
            sub_id,
            normalized_cwd: cwd,
            demand_cwd: input.cwd.as_str().to_owned(),
            include_remote,
        })
    }

    fn retain_poller(
        &self,
        cwd: &str,
        demand_cwd: &str,
        interval: IntervalFn,
        refresh_immediately: bool,
    ) {
        let mut pollers = self.pollers.pollers.lock();
        if let Some(existing) = pollers.get_mut(cwd) {
            existing.subscriber_count += 1;
            *existing
                .demand
                .lock()
                .entry(demand_cwd.to_owned())
                .or_insert(0) += 1;
            return;
        }
        let demand = Arc::new(Mutex::new(HashMap::from([(demand_cwd.to_owned(), 1usize)])));
        let cancel = CancellationToken::new();
        let inner = Arc::clone(&self.inner);
        let demand_task = Arc::clone(&demand);
        let cancel_task = cancel.clone();
        let cwd_owned = cwd.to_owned();
        let handle = AbortOnDropHandle::new(tokio::spawn(async move {
            remote_refresh_loop(
                inner,
                cwd_owned,
                demand_task,
                interval,
                refresh_immediately,
                cancel_task,
            )
            .await;
        }));
        pollers.insert(
            cwd.to_owned(),
            ActivePoller {
                cancel,
                handle,
                subscriber_count: 1,
                demand,
            },
        );
    }
}

impl StatusSubscription {
    pub async fn recv(&mut self) -> Option<VcsStatusStreamEvent> {
        if let Some(event) = self.snapshot.take() {
            return Some(event);
        }
        self.rx.recv().await
    }

    pub fn try_recv(&mut self) -> Option<VcsStatusStreamEvent> {
        if let Some(event) = self.snapshot.take() {
            return Some(event);
        }
        self.rx.try_recv().ok()
    }
}

impl Drop for StatusSubscription {
    fn drop(&mut self) {
        self.inner.unsubscribe(self.sub_id);
        if self.include_remote {
            self.pollers.release(&self.normalized_cwd, &self.demand_cwd);
        }
    }
}

impl futures::Stream for StatusSubscription {
    type Item = VcsStatusStreamEvent;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(event) = this.snapshot.take() {
            return std::task::Poll::Ready(Some(event));
        }
        this.rx.poll_recv(cx)
    }
}

impl Inner {
    async fn get_status(
        &self,
        input: &VcsStatusInput,
    ) -> Result<VcsStatusResult, GitManagerServiceError> {
        let cwd = normalize_cwd(input.cwd.as_str()).await;
        if let Some(status) = self.complete_status(&cwd) {
            return Ok(status);
        }
        let lock = self.remote_lock(&cwd);
        let _guard = lock.lock().await;
        if let Some(status) = self.complete_status(&cwd) {
            return Ok(status);
        }
        let latest = self.cached(&cwd);
        let cached_local = latest
            .as_ref()
            .and_then(|item| item.local.as_ref())
            .map(|local| local.value.clone());
        let cached_remote = latest
            .as_ref()
            .and_then(|item| item.remote.as_ref().map(|remote| remote.value.clone()));
        let workflow_local = Arc::clone(&self.workflow);
        let workflow_remote = Arc::clone(&self.workflow);
        let cwd_local = cwd.clone();
        let cwd_remote = cwd.clone();
        let (local, remote) = tokio::try_join!(
            async move {
                if let Some(local) = cached_local {
                    Ok(local)
                } else {
                    workflow_local.local_status(&cwd_local).await
                }
            },
            async move {
                if let Some(remote) = cached_remote {
                    Ok(remote)
                } else {
                    workflow_remote
                        .remote_status(&cwd_remote, GitRemoteStatusOptions::default())
                        .await
                }
            }
        )?;
        Ok(self.update_cached_status(&cwd, local, remote, false))
    }

    async fn refresh_local_status(
        &self,
        raw_cwd: &str,
    ) -> Result<VcsStatusLocalResult, GitManagerServiceError> {
        let cwd = normalize_cwd(raw_cwd).await;
        self.refresh_local_status_core(&cwd).await
    }

    async fn refresh_local_status_core(
        &self,
        cwd: &str,
    ) -> Result<VcsStatusLocalResult, GitManagerServiceError> {
        self.workflow.invalidate_local_status(cwd).await;
        let local = self.workflow.local_status(cwd).await?;
        Ok(self.update_cached_local(cwd, local, true))
    }

    async fn refresh_status(
        &self,
        raw_cwd: &str,
    ) -> Result<VcsStatusResult, GitManagerServiceError> {
        let cwd = normalize_cwd(raw_cwd).await;
        let lock = self.remote_lock(&cwd);
        let _guard = lock.lock().await;
        self.workflow.invalidate_status(&cwd).await;
        let remote = self
            .workflow
            .remote_status(&cwd, GitRemoteStatusOptions::default())
            .await?;
        let local = self.workflow.local_status(&cwd).await?;
        if let Some(pulled) = self
            .maybe_auto_pull(&cwd, remote.as_ref(), &[raw_cwd.to_owned()])
            .await
        {
            return Ok(merge_git_status_parts(pulled.local, pulled.remote));
        }
        Ok(self.update_cached_status(&cwd, local, remote, true))
    }

    async fn refresh_remote_status(
        &self,
        cwd: &str,
        options: RefreshRemoteOptions,
    ) -> Result<Option<VcsStatusRemoteResult>, GitManagerServiceError> {
        let lock = self.remote_lock(cwd);
        let _guard = lock.lock().await;
        if options.refresh_upstream != Some(false) {
            self.workflow.invalidate_remote_status(cwd).await;
        }
        let previous = self
            .cached(cwd)
            .and_then(|item| item.remote)
            .and_then(|remote| remote.value);
        let remote = self
            .workflow
            .remote_status(
                cwd,
                GitRemoteStatusOptions {
                    refresh_upstream: options.refresh_upstream,
                    refresh_missing_pull_request: None,
                },
            )
            .await?;
        if let Some(pulled) = self
            .maybe_auto_pull(cwd, remote.as_ref(), &options.policy_cwds)
            .await
        {
            return Ok(pulled.remote);
        }
        if let Some(remote_value) = remote.as_ref()
            && divergence_moved(previous.as_ref(), remote_value)
        {
            self.refresh_local_status_core(cwd).await?;
        }
        Ok(self.update_cached_remote(cwd, remote, true))
    }

    async fn maybe_auto_pull(
        &self,
        cwd: &str,
        remote: Option<&VcsStatusRemoteResult>,
        policy_cwds: &[String],
    ) -> Option<PulledStatus> {
        match self.try_auto_pull(cwd, remote, policy_cwds).await {
            Ok(pulled) => pulled,
            Err(_) => {
                tracing::warn!(cwd = cwd, "Automatic project pull failed");
                None
            }
        }
    }

    async fn try_auto_pull(
        &self,
        cwd: &str,
        remote: Option<&VcsStatusRemoteResult>,
        policy_cwds: &[String],
    ) -> Result<Option<PulledStatus>, GitManagerServiceError> {
        let remote = remote.cloned();
        let enabled = join_all(policy_cwds.iter().map(|policy_cwd| {
            let auto_pull = Arc::clone(&self.auto_pull);
            let policy_cwd = policy_cwd.clone();
            async move { auto_pull.is_enabled(&policy_cwd).await }
        }))
        .await
        .into_iter()
        .any(|flag| flag);
        let Some(remote) = remote else {
            return Ok(None);
        };
        if !remote.has_upstream || remote.ahead_count > 0 || remote.behind_count == 0 || !enabled {
            return Ok(None);
        }
        self.workflow.invalidate_local_status(cwd).await;
        let local = self.workflow.local_status(cwd).await?;
        if !local.is_repo || !local.is_default_ref || local.has_working_tree_changes {
            return Ok(None);
        }
        self.workflow.pull_current_branch(cwd).await?;
        self.workflow.invalidate_status(cwd).await;
        let workflow_local = Arc::clone(&self.workflow);
        let workflow_remote = Arc::clone(&self.workflow);
        let cwd_local = cwd.to_owned();
        let cwd_remote = cwd.to_owned();
        let (refreshed_local, refreshed_remote) = tokio::try_join!(
            async move { workflow_local.local_status(&cwd_local).await },
            async move {
                workflow_remote
                    .remote_status(
                        &cwd_remote,
                        GitRemoteStatusOptions {
                            refresh_upstream: Some(false),
                            refresh_missing_pull_request: None,
                        },
                    )
                    .await
            }
        )?;
        self.update_cached_status(cwd, refreshed_local.clone(), refreshed_remote.clone(), true);
        Ok(Some(PulledStatus {
            local: refreshed_local,
            remote: refreshed_remote,
        }))
    }

    async fn get_or_load_local(
        &self,
        cwd: &str,
    ) -> Result<VcsStatusLocalResult, GitManagerServiceError> {
        if let Some(local) = self
            .cached(cwd)
            .and_then(|item| item.local)
            .map(|local| local.value)
        {
            return Ok(local);
        }
        let local = self.workflow.local_status(cwd).await?;
        Ok(self.update_cached_local(cwd, local, false))
    }

    fn complete_status(&self, cwd: &str) -> Option<VcsStatusResult> {
        let cached = self.cached(cwd)?;
        Some(merge_git_status_parts(
            cached.local?.value,
            cached.remote?.value,
        ))
    }

    fn cached(&self, cwd: &str) -> Option<CachedVcsStatus> {
        self.cache.lock().get(cwd).cloned()
    }

    fn update_cached_local(
        &self,
        cwd: &str,
        local: VcsStatusLocalResult,
        publish: bool,
    ) -> VcsStatusLocalResult {
        let fingerprint = fingerprint_status(&local);
        let should_publish = {
            let mut cache = self.cache.lock();
            let previous = cache.get(cwd).cloned().unwrap_or_default();
            let changed = previous
                .local
                .as_ref()
                .map(|item| item.fingerprint.as_str())
                != Some(fingerprint.as_str());
            cache.insert(
                cwd.to_owned(),
                CachedVcsStatus {
                    local: Some(CachedValue {
                        fingerprint,
                        value: local.clone(),
                    }),
                    remote: previous.remote,
                },
            );
            changed
        };
        if publish && should_publish {
            self.publish(
                cwd,
                VcsStatusStreamEvent::LocalUpdated {
                    local: local.clone(),
                },
            );
        }
        local
    }

    fn update_cached_remote(
        &self,
        cwd: &str,
        remote: Option<VcsStatusRemoteResult>,
        publish: bool,
    ) -> Option<VcsStatusRemoteResult> {
        let fingerprint = fingerprint_status(&remote);
        let should_publish = {
            let mut cache = self.cache.lock();
            let previous = cache.get(cwd).cloned().unwrap_or_default();
            let changed = previous
                .remote
                .as_ref()
                .map(|item| item.fingerprint.as_str())
                != Some(fingerprint.as_str());
            cache.insert(
                cwd.to_owned(),
                CachedVcsStatus {
                    local: previous.local,
                    remote: Some(CachedValue {
                        fingerprint,
                        value: remote.clone(),
                    }),
                },
            );
            changed
        };
        if publish && should_publish {
            self.publish(
                cwd,
                VcsStatusStreamEvent::RemoteUpdated {
                    remote: remote.clone(),
                },
            );
        }
        remote
    }

    fn update_cached_status(
        &self,
        cwd: &str,
        local: VcsStatusLocalResult,
        remote: Option<VcsStatusRemoteResult>,
        publish: bool,
    ) -> VcsStatusResult {
        let local_fingerprint = fingerprint_status(&local);
        let remote_fingerprint = fingerprint_status(&remote);
        let should_publish = {
            let mut cache = self.cache.lock();
            let previous = cache.get(cwd).cloned().unwrap_or_default();
            let changed = previous
                .local
                .as_ref()
                .map(|item| item.fingerprint.as_str())
                != Some(local_fingerprint.as_str())
                || previous
                    .remote
                    .as_ref()
                    .map(|item| item.fingerprint.as_str())
                    != Some(remote_fingerprint.as_str());
            cache.insert(
                cwd.to_owned(),
                CachedVcsStatus {
                    local: Some(CachedValue {
                        fingerprint: local_fingerprint,
                        value: local.clone(),
                    }),
                    remote: Some(CachedValue {
                        fingerprint: remote_fingerprint,
                        value: remote.clone(),
                    }),
                },
            );
            changed
        };
        if publish && should_publish {
            self.publish(
                cwd,
                VcsStatusStreamEvent::Snapshot {
                    local: local.clone(),
                    remote: remote.clone(),
                },
            );
        }
        merge_git_status_parts(local, remote)
    }

    fn publish(&self, cwd: &str, event: VcsStatusStreamEvent) {
        self.subscribers.lock().retain(|subscriber| {
            subscriber.cwd != cwd || subscriber.tx.send(event.clone()).is_ok()
        });
    }

    fn subscribe(&self, cwd: String) -> (u64, mpsc::UnboundedReceiver<VcsStatusStreamEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let id = {
            let mut next = self.next_sub_id.lock();
            *next += 1;
            *next
        };
        self.subscribers.lock().push(Subscriber { id, cwd, tx });
        (id, rx)
    }

    fn unsubscribe(&self, id: u64) {
        self.subscribers
            .lock()
            .retain(|subscriber| subscriber.id != id);
    }

    fn remote_lock(&self, cwd: &str) -> Arc<AsyncMutex<()>> {
        self.remote_locks
            .lock()
            .entry(cwd.to_owned())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    fn log_refresh_failure(
        &self,
        cwd: &str,
        error: &GitManagerServiceError,
        consecutive_failures: u32,
        delay: Duration,
    ) {
        let diagnostics = remote_refresh_failure_diagnostics(&RefreshCause {
            reasons: vec![RefreshCauseReason::Fail(RefreshDiagnosticValue::Service(
                Box::new(error.clone()),
            ))],
        });
        let next_delay_ms = delay.as_millis();
        tracing::warn!(
            cwd_length = cwd.len(),
            reason_count = diagnostics.reason_count,
            failure_count = diagnostics.failure_count,
            failure_tags = ?diagnostics.failure_tags,
            failure_operations = ?diagnostics.failure_operations,
            defect_count = diagnostics.defect_count,
            defect_tags = ?diagnostics.defect_tags,
            interruption_count = diagnostics.interruption_count,
            consecutive_failures,
            next_delay_ms,
            "VCS remote status refresh failed"
        );
        if let Some(hook) = &self.warnings {
            hook(&RemoteRefreshFailureLog {
                cwd_length: cwd.len(),
                diagnostics,
                consecutive_failures,
                next_delay_ms,
            });
        }
    }
}

impl PollerSet {
    fn demand_cwds(&self, cwd: &str, raw_cwd: &str) -> Vec<String> {
        let pollers = self.pollers.lock();
        let Some(poller) = pollers.get(cwd) else {
            return vec![raw_cwd.to_owned()];
        };
        poller.demand.lock().keys().cloned().collect()
    }

    fn release(&self, cwd: &str, demand_cwd: &str) {
        let mut pollers = self.pollers.lock();
        let remove = match pollers.get_mut(cwd) {
            None => return,
            Some(existing) if existing.subscriber_count > 1 => {
                existing.subscriber_count -= 1;
                let mut demand = existing.demand.lock();
                let count = demand.get(demand_cwd).copied().unwrap_or(0);
                if count <= 1 {
                    demand.remove(demand_cwd);
                } else {
                    demand.insert(demand_cwd.to_owned(), count - 1);
                }
                false
            }
            Some(_) => true,
        };
        if remove && let Some(poller) = pollers.remove(cwd) {
            drop(pollers);
            poller.cancel.cancel();
            drop(poller);
        }
    }
}

async fn remote_refresh_loop(
    inner: Arc<Inner>,
    cwd: String,
    demand: Arc<Mutex<HashMap<String, usize>>>,
    interval: IntervalFn,
    refresh_immediately: bool,
    cancel: CancellationToken,
) {
    let mut consecutive_failures = 0u32;
    let mut needs_initial = refresh_immediately;
    if !refresh_immediately && !sleep_cancellable(&inner, &cancel, active_interval(&interval)).await
    {
        return;
    }
    loop {
        if cancel.is_cancelled() {
            return;
        }
        let delay = match refresh_remote_if_enabled(
            &inner,
            &cwd,
            &demand,
            &interval,
            &mut needs_initial,
            &mut consecutive_failures,
        )
        .await
        {
            Some(delay) => delay,
            None => return,
        };
        if !sleep_cancellable(&inner, &cancel, delay).await {
            return;
        }
    }
}

fn active_interval(interval: &IntervalFn) -> Duration {
    let configured = interval();
    if configured.is_zero() {
        DEFAULT_VCS_STATUS_REFRESH_INTERVAL
    } else {
        configured
    }
}

async fn refresh_remote_if_enabled(
    inner: &Inner,
    cwd: &str,
    demand: &Mutex<HashMap<String, usize>>,
    interval: &IntervalFn,
    needs_initial: &mut bool,
    consecutive_failures: &mut u32,
) -> Option<Duration> {
    let configured = interval();
    let active = active_interval(interval);
    if configured.is_zero() && !*needs_initial {
        return Some(active);
    }
    let demand_cwds: Vec<String> = demand.lock().keys().cloned().collect();
    let should_run = *needs_initial
        || join_all(demand_cwds.iter().map(|demand_cwd| {
            let background = Arc::clone(&inner.background);
            let demand_cwd = demand_cwd.clone();
            async move { background.should_run_vcs_status(&demand_cwd).await }
        }))
        .await
        .into_iter()
        .any(|enabled| enabled);
    if !should_run {
        return Some(active);
    }
    match inner
        .refresh_remote_status(
            cwd,
            RefreshRemoteOptions {
                refresh_upstream: Some(!configured.is_zero()),
                policy_cwds: demand_cwds,
            },
        )
        .await
    {
        Ok(_) => {
            *needs_initial = false;
            *consecutive_failures = 0;
            Some(active)
        }
        Err(error) => {
            *consecutive_failures += 1;
            let delay = remote_refresh_failure_delay(*consecutive_failures, active);
            inner.log_refresh_failure(cwd, &error, *consecutive_failures, delay);
            Some(delay)
        }
    }
}

async fn sleep_cancellable(inner: &Inner, cancel: &CancellationToken, duration: Duration) -> bool {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => false,
        _ = inner.sleeper.sleep(duration) => true,
    }
}

fn divergence_moved(
    previous: Option<&VcsStatusRemoteResult>,
    remote: &VcsStatusRemoteResult,
) -> bool {
    match previous {
        None => true,
        Some(previous) => {
            previous.ahead_count != remote.ahead_count
                || previous.behind_count != remote.behind_count
                || previous.ahead_of_default_count != remote.ahead_of_default_count
        }
    }
}

fn fingerprint_status(status: &impl Serialize) -> String {
    serde_json::to_string(status).unwrap_or_else(|_| "unserializable".to_owned())
}

async fn normalize_cwd(cwd: &str) -> String {
    match tokio::fs::canonicalize(cwd).await {
        Ok(path) => path.display().to_string(),
        Err(_) => cwd.to_owned(),
    }
}

fn add_unique(values: &mut Vec<String>, value: Option<String>) {
    let Some(value) = value else { return };
    if values.len() < MAX_FAILURE_DIAGNOSTIC_VALUES && !values.contains(&value) {
        values.push(value);
    }
}

fn bounded(value: &str) -> String {
    value
        .chars()
        .take(MAX_FAILURE_DIAGNOSTIC_VALUE_LENGTH)
        .collect()
}

fn diagnostic_tag(value: &RefreshDiagnosticValue) -> String {
    let raw = match value {
        RefreshDiagnosticValue::Service(error) => service_tag(error).to_owned(),
        RefreshDiagnosticValue::Error { name } => name.clone(),
        RefreshDiagnosticValue::Type(type_name) => (*type_name).to_owned(),
        RefreshDiagnosticValue::Uninspectable => "Uninspectable".to_owned(),
    };
    bounded(&raw)
}

fn diagnostic_operation(value: &RefreshDiagnosticValue) -> Option<String> {
    match value {
        RefreshDiagnosticValue::Service(error) => service_operation(error).map(bounded),
        _ => None,
    }
}

fn service_tag(error: &GitManagerServiceError) -> &'static str {
    match error {
        GitManagerServiceError::Manager(_) => "GitManagerError",
        GitManagerServiceError::Materialization(_) => "GitPullRequestMaterializationError",
        GitManagerServiceError::Command(_) => "GitCommandError",
        GitManagerServiceError::SourceControlProvider(_) => "SourceControlProviderError",
        GitManagerServiceError::TextGeneration(_) => "TextGenerationError",
    }
}

fn service_operation(error: &GitManagerServiceError) -> Option<&str> {
    match error {
        GitManagerServiceError::Manager(GitManagerError::GitManagerError { operation, .. }) => {
            Some(operation)
        }
        GitManagerServiceError::Command(GitCommandError::GitCommandError { operation, .. }) => {
            Some(operation)
        }
        GitManagerServiceError::TextGeneration(TextGenerationError::TextGenerationError {
            operation,
            ..
        }) => Some(operation),
        GitManagerServiceError::SourceControlProvider(
            SourceControlProviderError::SourceControlProviderError { operation, .. },
        ) => Some(operation),
        GitManagerServiceError::Materialization(_) => None,
    }
}
