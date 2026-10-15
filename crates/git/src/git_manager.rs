//! Port of upstream/apps/server/src/git/GitManager.ts
//!
//! Source control, text generation, project setup scripts, and the
//! orchestration effects that link or refresh a pull request are traits on
//! this struct. Their default implementations do not talk to a host. Thread
//! launch worktrees are not created here; `create_worktree` is the explicit
//! and pull-request worktree path.

#![allow(clippy::result_large_err)]

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use regex::Regex;
use std::sync::LazyLock;
use tokio::sync::broadcast;
use turin_contracts::base_schemas::{PositiveInt, TrimmedNonEmptyString};
use turin_contracts::environment::WorktreeSubmodules;
use turin_contracts::git::{
    GitActionProgressEvent, GitActionProgressPhase, GitBranchStepStatus, GitCommandError,
    GitCommitStepStatus, GitManagerError, GitManagerServiceError, GitPrStepStatus,
    GitPreparePullRequestThreadInput, GitPreparePullRequestThreadResult, GitPullRequestRefInput,
    GitPullRequestState, GitPushStepStatus, GitResolvePullRequestResult, GitResolvedPullRequest,
    GitRunStackedActionInput, GitRunStackedActionResult, GitRunStackedActionToast,
    GitRunStackedActionToastCta, GitRunStackedActionToastRunAction, GitStackedAction,
    GitStackedBranchStep, GitStackedCommitStep, GitStackedPrStep, GitStackedPushStep,
    TextGenerationError, VcsCreateRefInput, VcsCreateWorktreeInput, VcsCreateWorktreeResult,
    VcsPullResult, VcsStatusChangeRequest, VcsStatusInput, VcsStatusLocalResult,
    VcsStatusRemoteResult, VcsStatusResult, VcsSwitchRefInput,
};
use turin_contracts::source_control::{SourceControlProviderError, SourceControlProviderKind};
use turin_contracts::thread_pull_request::ThreadPullRequestKey;

use crate::git_vcs_driver_core::{
    CreateWorktreeOptions, ExecuteGitInput, GitCommitOptions, GitPushOptions, GitPushStatus,
    GitRemoteStatusOptions as DriverRemoteStatusOptions, GitVcsDriverCore,
};
use crate::link_created_pull_request::{
    CreatedPullRequestKey, ProjectRepositoryIdentity, created_pull_request_key,
};
use crate::shared_git::{
    merge_git_status_parts, normalize_git_remote_url, resolve_auto_feature_branch_name,
};

const SHORT_SHA_LENGTH: usize = 7;
const TOAST_DESCRIPTION_MAX: usize = 72;
const STATUS_CACHE_TTL: Duration = Duration::from_secs(1);
const STATUS_CACHE_CAPACITY: usize = 2_048;
const PR_LOOKUP_CACHE_TTL: Duration = Duration::from_secs(60);
const PR_LOOKUP_MISSING_TTL: Duration = Duration::from_secs(5 * 60);
const PR_LOOKUP_GIT_STATE_TTL: Duration = Duration::from_secs(5 * 60);
const PR_LOOKUP_CACHE_CAPACITY: usize = 2_048;
const PR_LOOKUP_FAILURE_BASE: Duration = Duration::from_secs(20);
const PR_LOOKUP_FAILURE_MAX: Duration = Duration::from_secs(15 * 60);
/// Pre-commit and commit-msg hooks can run lint or tests, so this is the
/// 10 minute bound upstream uses instead of the 30s driver default.
const COMMIT_TIMEOUT: Duration = Duration::from_secs(10 * 60);

static PR_REPOSITORY_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(.*)(?:/pull/|/-/merge_requests/|/pull-requests/|/pullrequest/)\d+(?:/.*)?$")
        .unwrap()
});
static REPOSITORY_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:[^@/\s]+@[^:/\s]+:|(?:ssh|https?|git)://[^/]+/)((?:[^/\s]+/)+[^/\s]+?)(?:\.git)?/?$",
    )
    .unwrap()
});

/// Backoff for a branch whose pull-request lookup keeps failing.
///
/// One failure waits 20s. Each further failure doubles that, capped at 15
/// minutes, so a rate-limited branch is not polled faster than a healthy one.
pub fn pr_lookup_failure_ttl(consecutive_failures: u32) -> Duration {
    let exponent = consecutive_failures.saturating_sub(1);
    let shift = exponent.min(16);
    let backoff = PR_LOOKUP_FAILURE_BASE.saturating_mul(1_u32 << shift);
    if backoff > PR_LOOKUP_FAILURE_MAX {
        PR_LOOKUP_FAILURE_MAX
    } else {
        backoff
    }
}

/// Repository key named by a pull request URL, or `None` for an issues URL.
pub fn pull_request_repository_key(value: &str) -> Option<String> {
    let url = parse_http_url(value)?;
    let caps = PR_REPOSITORY_PATH.captures(&url.pathname)?;
    let path = caps.get(1)?.as_str();
    Some(normalize_git_remote_url(&format!(
        "https://{}{path}",
        url.hostname
    )))
}

/// `owner/name` (or a deeper group path) from a remote URL.
pub fn repository_path_from_remote_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = REPOSITORY_PATH.captures(trimmed)?.get(1)?.as_str().trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchHeadContext {
    pub head_branch: String,
    pub head_repository_name_with_owner: Option<String>,
    pub head_repository_owner_login: Option<String>,
    pub is_cross_repository: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestHead {
    pub url: String,
    pub head_ref_name: String,
    pub is_cross_repository: Option<bool>,
    pub head_repository_name_with_owner: Option<String>,
    pub head_repository_owner_login: Option<String>,
}

pub fn matches_branch_head_context(pr: &PullRequestHead, head: &BranchHeadContext) -> bool {
    if pr.head_ref_name != head.head_branch {
        return false;
    }
    let expected = head_identity(
        head.head_repository_name_with_owner.as_deref(),
        head.head_repository_owner_login.as_deref(),
    );
    let actual = pull_request_head_identity(pr);
    if let Some(expected_repo) = expected.repository.as_deref()
        && let Some(actual_repo) = actual.repository.as_deref()
        && expected_repo != actual_repo
    {
        return false;
    }
    if let Some(expected_owner) = expected.owner.as_deref()
        && let Some(actual_owner) = actual.owner.as_deref()
        && expected_owner != actual_owner
    {
        return false;
    }
    if head.is_cross_repository {
        if pr.is_cross_repository == Some(false) {
            return false;
        }
        if (expected.repository.is_some() || expected.owner.is_some())
            && actual.repository.is_none()
            && actual.owner.is_none()
        {
            return false;
        }
        return true;
    }
    if pr.is_cross_repository == Some(true)
        && ((expected.repository.is_none() && expected.owner.is_none())
            || (actual.repository.is_none() && actual.owner.is_none()))
    {
        return false;
    }
    true
}

#[derive(Debug, Clone)]
pub struct HostChangeRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub base_ref_name: String,
    pub head_ref_name: String,
    pub state: GitPullRequestState,
    pub is_draft: Option<bool>,
    pub updated_at: Option<String>,
    pub closed_at: Option<String>,
    pub merged_at: Option<String>,
    pub head_sha: Option<String>,
    pub is_cross_repository: Option<bool>,
    pub head_repository_name_with_owner: Option<String>,
    pub head_repository_owner_login: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CreateChangeRequest {
    pub cwd: String,
    pub title: String,
    pub body: String,
    pub base_branch: String,
    pub head_branch: String,
}

/// Source-control host. The default lists nothing and fails get/create.
#[async_trait]
pub trait ChangeRequestHost: Send + Sync {
    async fn list_for_branch(
        &self,
        cwd: &str,
        head_selectors: &[String],
    ) -> Result<Vec<HostChangeRequest>, SourceControlProviderError>;

    async fn get(
        &self,
        cwd: &str,
        reference: &str,
    ) -> Result<HostChangeRequest, SourceControlProviderError>;

    async fn create(
        &self,
        cwd: &str,
        input: &CreateChangeRequest,
    ) -> Result<HostChangeRequest, SourceControlProviderError>;
}

pub struct UnavailableChangeRequestHost;

#[async_trait]
impl ChangeRequestHost for UnavailableChangeRequestHost {
    async fn list_for_branch(
        &self,
        _: &str,
        _: &[String],
    ) -> Result<Vec<HostChangeRequest>, SourceControlProviderError> {
        Ok(Vec::new())
    }

    async fn get(
        &self,
        cwd: &str,
        reference: &str,
    ) -> Result<HostChangeRequest, SourceControlProviderError> {
        Err(provider_error(
            "ChangeRequestHost.get",
            cwd,
            &format!("No source control provider is wired for {reference}."),
        ))
    }

    async fn create(
        &self,
        cwd: &str,
        _: &CreateChangeRequest,
    ) -> Result<HostChangeRequest, SourceControlProviderError> {
        Err(provider_error(
            "ChangeRequestHost.create",
            cwd,
            "No source control provider is wired.",
        ))
    }
}

#[derive(Debug, Clone)]
pub struct SuggestedCommit {
    pub subject: String,
    pub body: String,
    pub branch: Option<String>,
}

/// Commit and branch text. `Ok(None)` uses the upstream empty-subject fallback.
#[async_trait]
pub trait CommitMessageSource: Send + Sync {
    async fn suggest(
        &self,
        cwd: &str,
        staged_summary: &str,
        staged_patch: &str,
    ) -> Result<Option<SuggestedCommit>, TextGenerationError>;
}

pub struct FallbackCommitMessageSource;

#[async_trait]
impl CommitMessageSource for FallbackCommitMessageSource {
    async fn suggest(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<Option<SuggestedCommit>, TextGenerationError> {
        Ok(None)
    }
}

/// Project setup script run after a pull-request worktree is prepared.
/// The default succeeds and does nothing. Failures are logged, not returned.
#[async_trait]
pub trait ProjectSetupScript: Send + Sync {
    async fn run(&self, cwd: &str, worktree_path: &str) -> Result<(), String>;
}

pub struct SkipProjectSetupScript;

#[async_trait]
impl ProjectSetupScript for SkipProjectSetupScript {
    async fn run(&self, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Orchestration seam for `thread.pull-request.link`. Never fails the git action.
#[async_trait]
pub trait CreatedPullRequestLinker: Send + Sync {
    async fn link(&self, thread_id: &str, key: &CreatedPullRequestKey);
}

pub struct LogCreatedPullRequestLinker;

#[async_trait]
impl CreatedPullRequestLinker for LogCreatedPullRequestLinker {
    async fn link(&self, thread_id: &str, key: &CreatedPullRequestKey) {
        tracing::info!(
            thread_id,
            host = %key.host,
            repository = %key.repository,
            number = key.number,
            "link created pull request"
        );
    }
}

/// Orchestration seam for refreshing pull requests after a push.
#[async_trait]
pub trait PushedPullRequestRefresher: Send + Sync {
    async fn refresh(&self, cwd: &str);
}

pub struct LogPushedPullRequestRefresher;

#[async_trait]
impl PushedPullRequestRefresher for LogPushedPullRequestRefresher {
    async fn refresh(&self, cwd: &str) {
        tracing::info!(cwd, "refresh pushed pull requests");
    }
}

#[derive(Debug, Clone)]
pub struct GitBranchPullRequest {
    pub change_request: VcsStatusChangeRequest,
    pub repository_key: Option<String>,
    pub closed_at: Option<String>,
    pub merged_at: Option<String>,
    pub head_sha: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GitManagerRemoteStatusOptions {
    pub refresh_upstream: Option<bool>,
    pub refresh_missing_pull_request: bool,
}

#[derive(Default)]
pub struct GitRunStackedActionOptions {
    pub progress: Option<Arc<dyn Fn(GitActionProgressEvent) + Send + Sync>>,
    /// When the pull request URL cannot be parsed, link under this identity.
    pub project: Option<ProjectRepositoryIdentity>,
}

struct Timed<T> {
    value: T,
    at: Instant,
    ttl: Duration,
}

struct Lru<T> {
    map: HashMap<String, Timed<T>>,
    order: VecDeque<String>,
    capacity: usize,
}

impl<T: Clone> Lru<T> {
    fn new(capacity: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            capacity,
        }
    }

    fn get(&mut self, key: &str) -> Option<T> {
        let expired = self
            .map
            .get(key)
            .is_some_and(|entry| entry.at.elapsed() >= entry.ttl);
        if expired {
            self.remove(key);
            return None;
        }
        self.map.get(key).map(|entry| entry.value.clone())
    }

    fn insert(&mut self, key: String, value: T, ttl: Duration) {
        if self.map.contains_key(&key) {
            self.touch(&key);
            self.map.insert(
                key,
                Timed {
                    value,
                    at: Instant::now(),
                    ttl,
                },
            );
            return;
        }
        while self.map.len() >= self.capacity {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.map.remove(&oldest);
        }
        self.order.push_back(key.clone());
        self.map.insert(
            key,
            Timed {
                value,
                at: Instant::now(),
                ttl,
            },
        );
    }

    fn remove(&mut self, key: &str) {
        self.map.remove(key);
        if let Some(position) = self.order.iter().position(|item| item == key) {
            self.order.remove(position);
        }
    }

    fn touch(&mut self, key: &str) {
        if let Some(position) = self.order.iter().position(|item| item == key) {
            self.order.remove(position);
            self.order.push_back(key.to_owned());
        }
    }
}

/// Consecutive failed PR lookups per cache key. A success clears the key.
/// Bounded like upstream's `Map`: at capacity a new key evicts the oldest
/// insertion, and an existing key keeps its place.
struct FailureStreaks {
    counts: HashMap<String, u32>,
    order: VecDeque<String>,
    capacity: usize,
}

impl FailureStreaks {
    fn new(capacity: usize) -> Self {
        Self {
            counts: HashMap::new(),
            order: VecDeque::new(),
            capacity,
        }
    }

    /// Counts one more failure for `key` and returns the new streak.
    fn record_failure(&mut self, key: &str) -> u32 {
        if let Some(count) = self.counts.get_mut(key) {
            *count += 1;
            return *count;
        }
        if self.counts.len() >= self.capacity
            && let Some(oldest) = self.order.pop_front()
        {
            self.counts.remove(&oldest);
        }
        self.order.push_back(key.to_owned());
        self.counts.insert(key.to_owned(), 1);
        1
    }

    fn clear(&mut self, key: &str) {
        if self.counts.remove(key).is_some() {
            self.order.retain(|item| item != key);
        }
    }
}

#[derive(Clone)]
enum PrLookup {
    Found(Box<GitBranchPullRequest>),
    Missing,
    Failed(Box<GitManagerServiceError>),
}

pub struct GitManager {
    git: Arc<GitVcsDriverCore>,
    host: Arc<dyn ChangeRequestHost>,
    messages: Arc<dyn CommitMessageSource>,
    setup: Arc<dyn ProjectSetupScript>,
    linker: Arc<dyn CreatedPullRequestLinker>,
    refresher: Arc<dyn PushedPullRequestRefresher>,
    worktrees_directory: String,
    submodules: Option<WorktreeSubmodules>,
    local_cache: parking_lot::Mutex<Lru<VcsStatusLocalResult>>,
    remote_cache: parking_lot::Mutex<Lru<VcsStatusRemoteResult>>,
    pr_cache: parking_lot::Mutex<Lru<PrLookup>>,
    pr_failure_streaks: parking_lot::Mutex<FailureStreaks>,
    git_state: parking_lot::Mutex<Lru<String>>,
    pr_epoch: AtomicU64,
    pull_requests: broadcast::Sender<ThreadPullRequestKey>,
}

impl GitManager {
    pub fn new(git: Arc<GitVcsDriverCore>) -> Self {
        Self::with_host(git, Arc::new(UnavailableChangeRequestHost))
    }

    pub fn with_host(git: Arc<GitVcsDriverCore>, host: Arc<dyn ChangeRequestHost>) -> Self {
        let (sender, _) = broadcast::channel(64);
        Self {
            git,
            host,
            messages: Arc::new(FallbackCommitMessageSource),
            setup: Arc::new(SkipProjectSetupScript),
            linker: Arc::new(LogCreatedPullRequestLinker),
            refresher: Arc::new(LogPushedPullRequestRefresher),
            worktrees_directory: String::new(),
            submodules: None,
            local_cache: parking_lot::Mutex::new(Lru::new(STATUS_CACHE_CAPACITY)),
            remote_cache: parking_lot::Mutex::new(Lru::new(STATUS_CACHE_CAPACITY)),
            pr_cache: parking_lot::Mutex::new(Lru::new(PR_LOOKUP_CACHE_CAPACITY)),
            pr_failure_streaks: parking_lot::Mutex::new(FailureStreaks::new(
                PR_LOOKUP_CACHE_CAPACITY,
            )),
            git_state: parking_lot::Mutex::new(Lru::new(PR_LOOKUP_CACHE_CAPACITY)),
            pr_epoch: AtomicU64::new(0),
            pull_requests: sender,
        }
    }

    pub fn set_messages(&mut self, messages: Arc<dyn CommitMessageSource>) {
        self.messages = messages;
    }

    pub fn set_setup(&mut self, setup: Arc<dyn ProjectSetupScript>) {
        self.setup = setup;
    }

    pub fn set_linker(&mut self, linker: Arc<dyn CreatedPullRequestLinker>) {
        self.linker = linker;
    }

    pub fn set_refresher(&mut self, refresher: Arc<dyn PushedPullRequestRefresher>) {
        self.refresher = refresher;
    }

    pub fn set_worktree_settings(
        &mut self,
        directory: impl Into<String>,
        submodules: Option<WorktreeSubmodules>,
    ) {
        self.worktrees_directory = directory.into();
        self.submodules = submodules;
    }

    pub fn subscribe_pull_request_state_changes(
        &self,
    ) -> broadcast::Receiver<ThreadPullRequestKey> {
        self.pull_requests.subscribe()
    }

    pub async fn local_status(
        &self,
        input: &VcsStatusInput,
    ) -> Result<VcsStatusLocalResult, GitManagerServiceError> {
        let key = cache_key(input.cwd.as_str());
        if let Some(hit) = self.local_cache.lock().get(&key) {
            return Ok(hit);
        }
        let details = self
            .git
            .status_details_local(input.cwd.as_str(), Default::default())
            .await
            .map_err(GitManagerServiceError::Command)?;
        let local = local_from_details(details);
        self.local_cache
            .lock()
            .insert(key, local.clone(), STATUS_CACHE_TTL);
        Ok(local)
    }

    pub async fn remote_status(
        &self,
        input: &VcsStatusInput,
        options: GitManagerRemoteStatusOptions,
    ) -> Result<VcsStatusRemoteResult, GitManagerServiceError> {
        let key = cache_key(input.cwd.as_str());
        let bypass =
            options.refresh_upstream == Some(false) || options.refresh_missing_pull_request;
        if !bypass && let Some(hit) = self.remote_cache.lock().get(&key) {
            return Ok(hit);
        }
        let details = self
            .git
            .status_details_remote(
                input.cwd.as_str(),
                DriverRemoteStatusOptions {
                    refresh_upstream: options.refresh_upstream,
                },
            )
            .await
            .map_err(GitManagerServiceError::Command)?;
        let mut remote = VcsStatusRemoteResult {
            has_upstream: details.has_upstream,
            ahead_count: details.ahead_count,
            behind_count: details.behind_count,
            ahead_of_default_count: Some(details.ahead_of_default_count),
            pr: None,
        };
        if details.is_repo
            && let Some(branch) = details.branch.as_deref()
        {
            match self
                .branch_pull_request(
                    input.cwd.as_str(),
                    branch,
                    options.refresh_missing_pull_request,
                )
                .await
            {
                Ok(Some(found)) => remote.pr = Some(found.change_request),
                Ok(None) => {}
                Err(error) => return Err(error),
            }
        }
        if !bypass {
            self.remote_cache
                .lock()
                .insert(key, remote.clone(), STATUS_CACHE_TTL);
        }
        Ok(remote)
    }

    pub async fn status(
        &self,
        input: &VcsStatusInput,
    ) -> Result<VcsStatusResult, GitManagerServiceError> {
        let (local, remote) = tokio::try_join!(
            self.local_status(input),
            self.remote_status(input, GitManagerRemoteStatusOptions::default()),
        )?;
        Ok(merge_git_status_parts(local, Some(remote)))
    }

    pub fn invalidate_local_status(&self, cwd: &str) {
        self.local_cache.lock().remove(&cache_key(cwd));
    }

    pub fn invalidate_remote_status(&self, cwd: &str) {
        self.remote_cache.lock().remove(&cache_key(cwd));
    }

    pub fn invalidate_status(&self, cwd: &str) {
        let key = cache_key(cwd);
        self.local_cache.lock().remove(&key);
        self.remote_cache.lock().remove(&key);
        self.pr_epoch.fetch_add(1, Ordering::Relaxed);
        self.git_state.lock().remove(&format!("{key}\0remotes"));
    }

    pub async fn branch_pull_request(
        &self,
        cwd: &str,
        branch: &str,
        refresh: bool,
    ) -> Result<Option<GitBranchPullRequest>, GitManagerServiceError> {
        let key = format!(
            "{}\0{}\0{branch}",
            self.pr_epoch.load(Ordering::Relaxed),
            cache_key(cwd)
        );
        if !refresh && let Some(hit) = self.pr_cache.lock().get(&key) {
            return pr_lookup_result(hit);
        }
        if refresh {
            let failed = matches!(self.pr_cache.lock().get(&key), Some(PrLookup::Failed(_)));
            if !failed {
                self.pr_cache.lock().remove(&key);
            } else if let Some(hit) = self.pr_cache.lock().get(&key) {
                return pr_lookup_result(hit);
            }
        }
        match self.lookup_branch_pull_request(cwd, branch).await {
            Ok(Some(found)) => {
                self.pr_failure_streaks.lock().clear(&key);
                self.pr_cache.lock().insert(
                    key,
                    PrLookup::Found(Box::new(found.clone())),
                    PR_LOOKUP_CACHE_TTL,
                );
                Ok(Some(found))
            }
            Ok(None) => {
                self.pr_failure_streaks.lock().clear(&key);
                self.pr_cache
                    .lock()
                    .insert(key, PrLookup::Missing, PR_LOOKUP_MISSING_TTL);
                Ok(None)
            }
            Err(error) => {
                let failures = self.pr_failure_streaks.lock().record_failure(&key);
                self.pr_cache.lock().insert(
                    key,
                    PrLookup::Failed(Box::new(error.clone())),
                    pr_lookup_failure_ttl(failures),
                );
                Err(error)
            }
        }
    }

    pub async fn resolve_pull_request(
        &self,
        input: &GitPullRequestRefInput,
    ) -> Result<GitResolvePullRequestResult, GitManagerServiceError> {
        let reference = normalize_pull_request_reference(input.reference.as_str());
        let found = self
            .host
            .get(input.cwd.as_str(), &reference)
            .await
            .map_err(GitManagerServiceError::SourceControlProvider)?;
        Ok(GitResolvePullRequestResult {
            pull_request: resolved_from_host(&found)?,
        })
    }

    pub async fn prepare_pull_request_thread(
        &self,
        input: &GitPreparePullRequestThreadInput,
    ) -> Result<GitPreparePullRequestThreadResult, GitManagerServiceError> {
        let reference = normalize_pull_request_reference(input.reference.as_str());
        let found = self
            .host
            .get(input.cwd.as_str(), &reference)
            .await
            .map_err(GitManagerServiceError::SourceControlProvider)?;
        let pull_request = resolved_from_host(&found)?;
        let branch = pull_request.head_branch.clone();
        let (worktree_path, checkout) = match input.mode {
            turin_contracts::git::GitPreparePullRequestThreadMode::Local => {
                let switched = self
                    .git
                    .switch_ref(&VcsSwitchRefInput {
                        cwd: input.cwd.clone(),
                        ref_name: branch.clone(),
                    })
                    .await
                    .map_err(GitManagerServiceError::Command)?;
                (None, switched.ref_name.unwrap_or(branch))
            }
            turin_contracts::git::GitPreparePullRequestThreadMode::Worktree => {
                let created = self
                    .create_worktree(
                        &VcsCreateWorktreeInput {
                            cwd: input.cwd.clone(),
                            ref_name: branch.clone(),
                            new_ref_name: None,
                            base_ref_name: None,
                            path: None,
                        },
                        CreateWorktreeOptions::default(),
                    )
                    .await
                    .map_err(GitManagerServiceError::Command)?;
                (Some(created.worktree.path), created.worktree.ref_name)
            }
        };
        if input.thread_id.is_some()
            && let Some(path) = worktree_path.as_ref()
            && let Err(error) = self.setup.run(input.cwd.as_str(), path.as_str()).await
        {
            tracing::warn!(%error, "prepare pull request thread setup script failed");
        }
        Ok(GitPreparePullRequestThreadResult {
            pull_request,
            branch: checkout,
            worktree_path,
            is_on_pull_request_head: true,
        })
    }

    pub async fn create_worktree(
        &self,
        input: &VcsCreateWorktreeInput,
        mut options: CreateWorktreeOptions,
    ) -> Result<VcsCreateWorktreeResult, GitCommandError> {
        if options.worktrees_directory.is_none() && !self.worktrees_directory.is_empty() {
            options.worktrees_directory = Some(self.worktrees_directory.clone());
        }
        if options.submodules.is_none() {
            options.submodules = self.submodules;
        }
        self.git.create_worktree(input, options).await
    }

    pub async fn pull_current_branch(&self, cwd: &str) -> Result<VcsPullResult, GitCommandError> {
        let result = self.git.pull_current_branch(cwd).await;
        self.invalidate_status(cwd);
        result
    }

    pub async fn run_stacked_action(
        &self,
        input: &GitRunStackedActionInput,
        options: GitRunStackedActionOptions,
    ) -> Result<GitRunStackedActionResult, GitManagerServiceError> {
        let outcome = self.run_stacked_action_inner(input, &options).await;
        self.invalidate_status(input.cwd.as_str());
        if let Err(error) = &outcome {
            emit(
                options.progress.as_ref(),
                GitActionProgressEvent::ActionFailed {
                    action_id: input.action_id.clone(),
                    cwd: input.cwd.clone(),
                    action: input.action,
                    phase: None,
                    message: trimmed_or(&error.to_string(), "Git action failed"),
                },
            );
        }
        outcome
    }

    pub async fn link_created_pull_request(
        &self,
        thread_id: &str,
        result: &GitRunStackedActionResult,
        project: Option<&ProjectRepositoryIdentity>,
    ) {
        let Some(key) = created_pull_request_key(&result.pr, project) else {
            return;
        };
        if let (Ok(host), Ok(repository), Some(number)) = (
            TrimmedNonEmptyString::new(&key.host),
            TrimmedNonEmptyString::new(&key.repository),
            PositiveInt::new(key.number),
        ) {
            let _ = self.pull_requests.send(ThreadPullRequestKey {
                host,
                repository,
                number,
            });
        }
        self.linker.link(thread_id, &key).await;
    }

    pub async fn refresh_pushed_pull_requests(&self, cwd: &str) {
        self.refresher.refresh(cwd).await;
    }

    async fn run_stacked_action_inner(
        &self,
        input: &GitRunStackedActionInput,
        options: &GitRunStackedActionOptions,
    ) -> Result<GitRunStackedActionResult, GitManagerServiceError> {
        let cwd = input.cwd.as_str();
        let initial = self
            .git
            .status_details(cwd)
            .await
            .map_err(GitManagerServiceError::Command)?;
        let feature_branch = input.feature_branch == Some(true);
        let wants_commit = is_commit_action(input.action);
        let wants_push = matches!(
            input.action,
            GitStackedAction::Push | GitStackedAction::CommitPush | GitStackedAction::CommitPushPr
        ) || (input.action == GitStackedAction::CreatePr
            && (!initial.has_upstream || initial.ahead_count > 0));
        let wants_pr = matches!(
            input.action,
            GitStackedAction::CreatePr | GitStackedAction::CommitPushPr
        );
        if feature_branch && !wants_commit {
            return Err(manager(
                "runStackedAction",
                cwd,
                "Feature-branch checkout is only supported for commit actions.",
            ));
        }
        let mut phases = Vec::new();
        if feature_branch {
            phases.push(GitActionProgressPhase::Branch);
        }
        if wants_commit {
            phases.push(GitActionProgressPhase::Commit);
        }
        if wants_push {
            phases.push(GitActionProgressPhase::Push);
        }
        if wants_pr {
            phases.push(GitActionProgressPhase::Pr);
        }
        emit(
            options.progress.as_ref(),
            GitActionProgressEvent::ActionStarted {
                action_id: input.action_id.clone(),
                cwd: input.cwd.clone(),
                action: input.action,
                phases,
            },
        );
        if !feature_branch && wants_push && initial.branch.is_none() {
            return Err(manager(
                "runStackedAction",
                cwd,
                "Cannot push from detached HEAD.",
            ));
        }
        if !feature_branch && wants_pr && initial.branch.is_none() {
            return Err(manager(
                "runStackedAction",
                cwd,
                "Cannot create a pull request from detached HEAD.",
            ));
        }

        let file_paths = input.file_paths.as_ref().map(|paths| {
            paths
                .iter()
                .map(|path| path.as_str().to_owned())
                .collect::<Vec<_>>()
        });
        let (branch_step, resolved_message) = if feature_branch {
            emit_phase(
                options,
                input,
                GitActionProgressPhase::Branch,
                "Preparing feature branch...",
            )?;
            self.feature_branch_step(
                cwd,
                input.commit_message.as_ref().map(|m| m.as_str()),
                file_paths.as_deref(),
            )
            .await?
        } else {
            (
                GitStackedBranchStep {
                    status: GitBranchStepStatus::SkippedNotRequested,
                    name: None,
                },
                input.commit_message.clone(),
            )
        };
        let current_branch = branch_step.name.clone().or_else(|| {
            initial
                .branch
                .as_deref()
                .and_then(|name| trimmed(name).ok())
        });

        let commit = if wants_commit {
            emit_phase(
                options,
                input,
                GitActionProgressPhase::Commit,
                "Committing...",
            )?;
            self.commit_step(
                cwd,
                resolved_message.as_ref().map(|message| message.as_str()),
                file_paths.as_deref(),
            )
            .await?
        } else {
            GitStackedCommitStep {
                status: GitCommitStepStatus::SkippedNotRequested,
                commit_sha: None,
                subject: None,
            }
        };
        let push = if wants_push {
            emit_phase(options, input, GitActionProgressPhase::Push, "Pushing...")?;
            let pushed = self
                .git
                .push_current_branch(
                    cwd,
                    current_branch.as_ref().map(|name| name.as_str()),
                    GitPushOptions::default(),
                )
                .await
                .map_err(GitManagerServiceError::Command)?;
            if pushed.status == GitPushStatus::Pushed {
                self.refresh_pushed_pull_requests(cwd).await;
            }
            GitStackedPushStep {
                status: match pushed.status {
                    GitPushStatus::Pushed => GitPushStepStatus::Pushed,
                    GitPushStatus::SkippedUpToDate => GitPushStepStatus::SkippedUpToDate,
                },
                branch: trimmed(&pushed.branch).ok(),
                upstream_branch: pushed
                    .upstream_branch
                    .as_deref()
                    .and_then(|name| trimmed(name).ok()),
                set_upstream: pushed.set_upstream,
            }
        } else {
            GitStackedPushStep {
                status: GitPushStepStatus::SkippedNotRequested,
                branch: None,
                upstream_branch: None,
                set_upstream: None,
            }
        };
        let pr = if wants_pr {
            emit_phase(
                options,
                input,
                GitActionProgressPhase::Pr,
                "Preparing PR...",
            )?;
            self.pr_step(
                cwd,
                current_branch.as_ref().map(|name| name.as_str()),
                commit.subject.as_ref().map(|subject| subject.as_str()),
            )
            .await?
        } else {
            skipped_pr()
        };
        let result = GitRunStackedActionResult {
            action: input.action,
            branch: branch_step,
            commit,
            push,
            pr,
            toast: GitRunStackedActionToast {
                title: trimmed("Done").expect("title"),
                description: None,
                cta: GitRunStackedActionToastCta::None,
            },
        };
        let toast = self.completion_toast(cwd, &result).await?;
        let result = GitRunStackedActionResult { toast, ..result };
        if let Some(thread_id) = input.thread_id.as_ref() {
            self.link_created_pull_request(thread_id.as_str(), &result, options.project.as_ref())
                .await;
        }
        emit(
            options.progress.as_ref(),
            GitActionProgressEvent::ActionFinished {
                action_id: input.action_id.clone(),
                cwd: input.cwd.clone(),
                action: input.action,
                result: result.clone(),
            },
        );
        Ok(result)
    }

    async fn feature_branch_step(
        &self,
        cwd: &str,
        commit_message: Option<&str>,
        file_paths: Option<&[String]>,
    ) -> Result<(GitStackedBranchStep, Option<TrimmedNonEmptyString>), GitManagerServiceError> {
        let prepared = self
            .git
            .prepare_commit_context(cwd, file_paths)
            .await
            .map_err(GitManagerServiceError::Command)?;
        if prepared.is_none() {
            return Err(manager(
                "runStackedAction",
                cwd,
                "Cannot create a feature branch because there are no changes to commit.",
            ));
        }
        let suggested = self
            .suggest_commit(cwd, commit_message, prepared.as_ref())
            .await?;
        let preferred = suggested
            .branch
            .clone()
            .unwrap_or_else(|| suggested.subject.clone());
        let existing = self
            .git
            .list_local_branch_names(cwd)
            .await
            .map_err(GitManagerServiceError::Command)?;
        let name = resolve_auto_feature_branch_name(&existing, Some(&preferred));
        let ref_name = trimmed(&name)?;
        self.git
            .create_ref(&VcsCreateRefInput {
                cwd: trimmed(cwd)?,
                ref_name: ref_name.clone(),
                switch_ref: Some(true),
            })
            .await
            .map_err(GitManagerServiceError::Command)?;
        Ok((
            GitStackedBranchStep {
                status: GitBranchStepStatus::Created,
                name: Some(ref_name),
            },
            Some(trimmed(&format_commit_message(
                &suggested.subject,
                &suggested.body,
            ))?),
        ))
    }

    async fn commit_step(
        &self,
        cwd: &str,
        commit_message: Option<&str>,
        file_paths: Option<&[String]>,
    ) -> Result<GitStackedCommitStep, GitManagerServiceError> {
        let prepared = self
            .git
            .prepare_commit_context(cwd, file_paths)
            .await
            .map_err(GitManagerServiceError::Command)?;
        let Some(prepared) = prepared else {
            return Ok(GitStackedCommitStep {
                status: GitCommitStepStatus::SkippedNoChanges,
                commit_sha: None,
                subject: None,
            });
        };
        let suggested = self
            .suggest_commit(cwd, commit_message, Some(&prepared))
            .await?;
        let committed = self
            .git
            .commit(
                cwd,
                &suggested.subject,
                &suggested.body,
                commit_step_options(file_paths),
            )
            .await
            .map_err(GitManagerServiceError::Command)?;
        Ok(GitStackedCommitStep {
            status: GitCommitStepStatus::Created,
            commit_sha: trimmed(&committed.commit_sha).ok(),
            subject: trimmed(&suggested.subject).ok(),
        })
    }

    async fn suggest_commit(
        &self,
        cwd: &str,
        commit_message: Option<&str>,
        prepared: Option<&crate::git_vcs_driver_core::GitPreparedCommitContext>,
    ) -> Result<SuggestedCommit, GitManagerServiceError> {
        // A user-written message is committed as typed. Sanitizing is for generated text only.
        if let Some((subject, body)) = commit_message.and_then(parse_custom_commit_message) {
            return Ok(SuggestedCommit {
                subject,
                body,
                branch: None,
            });
        }
        let generated = if let Some(prepared) = prepared {
            self.messages
                .suggest(cwd, &prepared.staged_summary, &prepared.staged_patch)
                .await
                .map_err(GitManagerServiceError::TextGeneration)?
        } else {
            None
        };
        Ok(match generated {
            Some(suggested) => {
                sanitize_commit_message(suggested.subject, suggested.body, suggested.branch)
            }
            None => sanitize_commit_message(String::new(), String::new(), None),
        })
    }

    async fn pr_step(
        &self,
        cwd: &str,
        branch: Option<&str>,
        subject: Option<&str>,
    ) -> Result<GitStackedPrStep, GitManagerServiceError> {
        let Some(branch) = branch else {
            return Err(manager(
                "runStackedAction",
                cwd,
                "Cannot create a pull request from detached HEAD.",
            ));
        };
        if let Some(existing) = self.branch_pull_request(cwd, branch, true).await?
            && existing.change_request.state == GitPullRequestState::Open
        {
            return Ok(pr_from_host_status(
                &existing,
                GitPrStepStatus::OpenedExisting,
            ));
        }
        let base = self
            .git
            .resolve_default_branch_name(cwd, "origin")
            .await
            .map_err(GitManagerServiceError::Command)?
            .unwrap_or_else(|| "main".to_owned());
        let title = subject.unwrap_or("Update project files");
        let created = self
            .host
            .create(
                cwd,
                &CreateChangeRequest {
                    cwd: cwd.to_owned(),
                    title: title.to_owned(),
                    body: String::new(),
                    base_branch: base,
                    head_branch: branch.to_owned(),
                },
            )
            .await
            .map_err(GitManagerServiceError::SourceControlProvider)?;
        Ok(GitStackedPrStep {
            status: GitPrStepStatus::Created,
            url: Some(created.url),
            number: PositiveInt::new(created.number),
            base_branch: trimmed(&created.base_ref_name).ok(),
            head_branch: trimmed(&created.head_ref_name).ok(),
            title: trimmed(&created.title).ok(),
        })
    }

    async fn completion_toast(
        &self,
        cwd: &str,
        result: &GitRunStackedActionResult,
    ) -> Result<GitRunStackedActionToast, GitManagerServiceError> {
        let summary = summarize_git_action_result(result);
        let mut current_branch_is_default = false;
        let mut final_branch = None;
        if result.action != GitStackedAction::Commit {
            let status = self
                .git
                .status_details(cwd)
                .await
                .map_err(GitManagerServiceError::Command)?;
            if status.branch.is_some() {
                current_branch_is_default = status.is_default_branch;
                final_branch = Some((status.branch, status.has_upstream));
            }
        }
        let explicit = matches!(
            result.pr.status,
            GitPrStepStatus::Created | GitPrStepStatus::OpenedExisting
        )
        .then(|| result.pr.url.clone())
        .flatten();
        let lookup = matches!(
            result.action,
            GitStackedAction::CommitPush | GitStackedAction::Push
        ) && result.push.status == GitPushStepStatus::Pushed
            && result.branch.status != GitBranchStepStatus::Created
            && !current_branch_is_default
            && explicit.is_none()
            && final_branch.as_ref().is_some_and(|(_, upstream)| *upstream);
        let mut open_url = explicit;
        if lookup
            && let Some((Some(branch), _)) = final_branch
            && let Some(found) = self.branch_pull_request(cwd, &branch, false).await?
            && found.change_request.state == GitPullRequestState::Open
        {
            open_url = Some(found.change_request.url);
        }
        let cta = if result.action == GitStackedAction::Commit
            && result.commit.status == GitCommitStepStatus::Created
        {
            GitRunStackedActionToastCta::RunAction {
                label: trimmed("Push")?,
                action: GitRunStackedActionToastRunAction {
                    kind: GitStackedAction::Push,
                },
            }
        } else if open_url.is_some()
            && matches!(
                result.action,
                GitStackedAction::Push
                    | GitStackedAction::CreatePr
                    | GitStackedAction::CommitPush
                    | GitStackedAction::CommitPushPr
            )
            && (!current_branch_is_default
                || matches!(
                    result.pr.status,
                    GitPrStepStatus::Created | GitPrStepStatus::OpenedExisting
                ))
        {
            GitRunStackedActionToastCta::OpenPr {
                label: trimmed("View PR")?,
                url: open_url.unwrap_or_default(),
            }
        } else if matches!(
            result.action,
            GitStackedAction::Push | GitStackedAction::CommitPush
        ) && result.push.status == GitPushStepStatus::Pushed
            && !current_branch_is_default
        {
            GitRunStackedActionToastCta::RunAction {
                label: trimmed("Create PR")?,
                action: GitRunStackedActionToastRunAction {
                    kind: GitStackedAction::CreatePr,
                },
            }
        } else {
            GitRunStackedActionToastCta::None
        };
        Ok(GitRunStackedActionToast {
            title: trimmed(&summary.0)?,
            description: summary.1.as_deref().and_then(|text| trimmed(text).ok()),
            cta,
        })
    }

    async fn lookup_branch_pull_request(
        &self,
        cwd: &str,
        branch: &str,
    ) -> Result<Option<GitBranchPullRequest>, GitManagerServiceError> {
        let remotes = self
            .remember_git(cwd, "remotes", || async {
                self.git_stdout("GitManager.branchPullRequest.remotes", cwd, &["remote"])
                    .await
            })
            .await?;
        let remote_names: Vec<String> = remotes
            .split('\n')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        let Some(first_remote) = remote_names.first() else {
            return Ok(None);
        };
        let branch_ref = self
            .git_stdout(
                "GitManager.branchPullRequest.branchRef",
                cwd,
                &[
                    "for-each-ref",
                    "--format=%(refname)%00%(upstream:short)%00%(upstream:remotename)%00%(upstream:remoteref)",
                    &format!("refs/heads/{branch}"),
                ],
            )
            .await?;
        let expected = format!("refs/heads/{branch}");
        let exact = branch_ref
            .split('\n')
            .find(|line| line.split('\0').next() == Some(expected.as_str()));
        let fields: Vec<&str> = exact
            .map(|line| line.split('\0').collect())
            .unwrap_or_default();
        let local_exists = !fields.first().copied().unwrap_or("").is_empty();
        let saved_upstream = fields.get(1).copied().unwrap_or("");
        let saved_remote = fields.get(2).copied().unwrap_or("");
        let saved_remote_ref = fields.get(3).copied().unwrap_or("");
        let remote_name = if !saved_upstream.is_empty() {
            if saved_remote.is_empty() || saved_remote_ref.is_empty() {
                return Err(manager(
                    "branchPullRequest",
                    cwd,
                    &format!("Saved upstream for {branch} is incomplete."),
                ));
            }
            Some(saved_remote.to_owned())
        } else if !local_exists {
            let tracking = self
                .git_stdout(
                    "GitManager.branchPullRequest.remoteTrackingRefs",
                    cwd,
                    &["for-each-ref", "--format=%(refname)", "refs/remotes"],
                )
                .await?;
            let names: Vec<&str> = remote_names
                .iter()
                .map(String::as_str)
                .filter(|candidate| {
                    tracking
                        .lines()
                        .any(|line| line.trim() == format!("refs/remotes/{candidate}/{branch}"))
                })
                .collect();
            if names.len() > 1 {
                return Err(manager(
                    "branchPullRequest",
                    cwd,
                    &format!("Multiple remotes track {branch}. Its pull request is ambiguous."),
                ));
            }
            names.first().map(|name| (*name).to_owned())
        } else {
            None
        };
        let default_remote = if remote_names.iter().any(|name| name == "origin") {
            "origin"
        } else {
            first_remote
        };
        let default_branch = self
            .git
            .resolve_default_branch_name(cwd, default_remote)
            .await
            .ok()
            .flatten();
        let identity_remote = remote_name.as_deref().unwrap_or(default_remote);
        let head_url = self
            .git_stdout(
                "GitManager.branchPullRequest.remoteUrl",
                cwd,
                &["remote", "get-url", identity_remote],
            )
            .await
            .unwrap_or_default();
        if remote_name.is_some() && head_url.trim().is_empty() {
            return Err(manager(
                "branchPullRequest",
                cwd,
                &format!("Repository identity for {branch} could not be verified."),
            ));
        }
        let head_path = repository_path_from_remote_url(head_url.trim());
        let listed = self
            .host
            .list_for_branch(cwd, &[branch.to_owned()])
            .await
            .map_err(GitManagerServiceError::SourceControlProvider)?;
        let head = BranchHeadContext {
            head_branch: branch.to_owned(),
            head_repository_owner_login: head_path
                .as_deref()
                .and_then(|path| path.split('/').next())
                .map(str::to_owned),
            head_repository_name_with_owner: head_path,
            is_cross_repository: false,
        };
        let mut matches: Vec<&HostChangeRequest> = listed
            .iter()
            .filter(|pr| {
                matches_branch_head_context(
                    &PullRequestHead {
                        url: pr.url.clone(),
                        head_ref_name: pr.head_ref_name.clone(),
                        is_cross_repository: pr.is_cross_repository,
                        head_repository_name_with_owner: pr.head_repository_name_with_owner.clone(),
                        head_repository_owner_login: pr.head_repository_owner_login.clone(),
                    },
                    &head,
                )
            })
            .collect();
        matches.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        let Some(latest) = matches.first() else {
            return Ok(None);
        };
        if (Some(branch) == default_branch.as_deref()
            || (default_branch.is_none() && (branch == "main" || branch == "master")))
            && latest.state != GitPullRequestState::Open
        {
            return Ok(None);
        }
        let number = PositiveInt::new(latest.number).ok_or_else(|| {
            manager(
                "branchPullRequest",
                cwd,
                "Pull request number must be at least 1.",
            )
        })?;
        Ok(Some(GitBranchPullRequest {
            change_request: VcsStatusChangeRequest {
                number,
                title: trimmed(&latest.title)?,
                url: latest.url.clone(),
                base_ref: trimmed(&latest.base_ref_name)?,
                head_ref: trimmed(&latest.head_ref_name)?,
                state: latest.state,
                is_draft: latest.is_draft.filter(|draft| *draft),
                updated_at: Some(latest.updated_at.clone()),
            },
            repository_key: pull_request_repository_key(&latest.url),
            closed_at: latest.closed_at.clone(),
            merged_at: latest.merged_at.clone(),
            head_sha: latest.head_sha.clone(),
        }))
    }

    async fn remember_git<F, Fut>(
        &self,
        cwd: &str,
        name: &str,
        read: F,
    ) -> Result<String, GitManagerServiceError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<String, GitManagerServiceError>>,
    {
        let key = format!("{}\0{name}", cache_key(cwd));
        if let Some(hit) = self.git_state.lock().get(&key) {
            return Ok(hit);
        }
        let value = read().await?;
        self.git_state
            .lock()
            .insert(key, value.clone(), PR_LOOKUP_GIT_STATE_TTL);
        Ok(value)
    }

    async fn git_stdout(
        &self,
        operation: &str,
        cwd: &str,
        args: &[&str],
    ) -> Result<String, GitManagerServiceError> {
        let result = self
            .git
            .execute(ExecuteGitInput::new(
                operation,
                cwd,
                args.iter().copied().map(str::to_owned).collect(),
            ))
            .await
            .map_err(GitManagerServiceError::Command)?;
        Ok(result.stdout)
    }
}

fn pr_lookup_result(
    value: PrLookup,
) -> Result<Option<GitBranchPullRequest>, GitManagerServiceError> {
    match value {
        PrLookup::Found(found) => Ok(Some(*found)),
        PrLookup::Missing => Ok(None),
        PrLookup::Failed(error) => Err(*error),
    }
}

fn local_from_details(
    details: crate::git_vcs_driver_core::GitStatusDetails,
) -> VcsStatusLocalResult {
    VcsStatusLocalResult {
        is_repo: details.is_repo,
        source_control_provider: None,
        has_primary_remote: details.has_origin_remote,
        is_default_ref: details.is_default_branch,
        ref_name: details
            .branch
            .as_deref()
            .and_then(|name| trimmed(name).ok()),
        has_working_tree_changes: details.has_working_tree_changes,
        working_tree: details.working_tree,
        branch_changes: details.branch_changes,
    }
}

fn is_commit_action(action: GitStackedAction) -> bool {
    matches!(
        action,
        GitStackedAction::Commit | GitStackedAction::CommitPush | GitStackedAction::CommitPushPr
    )
}

fn skipped_pr() -> GitStackedPrStep {
    GitStackedPrStep {
        status: GitPrStepStatus::SkippedNotRequested,
        url: None,
        number: None,
        base_branch: None,
        head_branch: None,
        title: None,
    }
}

fn pr_from_host_status(
    existing: &GitBranchPullRequest,
    status: GitPrStepStatus,
) -> GitStackedPrStep {
    GitStackedPrStep {
        status,
        url: Some(existing.change_request.url.clone()),
        number: Some(existing.change_request.number),
        base_branch: Some(existing.change_request.base_ref.clone()),
        head_branch: Some(existing.change_request.head_ref.clone()),
        title: Some(existing.change_request.title.clone()),
    }
}

fn resolved_from_host(
    found: &HostChangeRequest,
) -> Result<GitResolvedPullRequest, GitManagerServiceError> {
    Ok(GitResolvedPullRequest {
        number: PositiveInt::new(found.number).ok_or_else(|| {
            manager(
                "resolvePullRequest",
                "",
                "Pull request number must be at least 1.",
            )
        })?,
        title: trimmed(&found.title)?,
        url: found.url.clone(),
        base_branch: trimmed(&found.base_ref_name)?,
        head_branch: trimmed(&found.head_ref_name)?,
        state: found.state,
    })
}

fn normalize_pull_request_reference(reference: &str) -> String {
    let trimmed = reference.trim();
    trimmed
        .strip_prefix('#')
        .filter(|rest| rest.chars().all(|ch| ch.is_ascii_digit()) && !rest.is_empty())
        .unwrap_or(trimmed)
        .to_owned()
}

fn sanitize_commit_message(
    subject: String,
    body: String,
    branch: Option<String>,
) -> SuggestedCommit {
    let raw = subject.trim().lines().next().unwrap_or("").trim();
    let stripped = raw.trim_end_matches('.').trim();
    let safe = if stripped.is_empty() {
        "Update project files".to_owned()
    } else {
        stripped
            .chars()
            .take(72)
            .collect::<String>()
            .trim_end()
            .to_owned()
    };
    SuggestedCommit {
        subject: safe,
        body: body.trim().to_owned(),
        branch,
    }
}

fn parse_custom_commit_message(raw: &str) -> Option<(String, String)> {
    let normalized = raw.replace("\r\n", "\n");
    let normalized = normalized.trim();
    if normalized.is_empty() {
        return None;
    }
    let mut lines = normalized.split('\n');
    let subject = lines.next()?.trim();
    if subject.is_empty() {
        return None;
    }
    Some((
        subject.to_owned(),
        lines.collect::<Vec<_>>().join("\n").trim().to_owned(),
    ))
}

fn commit_step_options(file_paths: Option<&[String]>) -> GitCommitOptions {
    GitCommitOptions {
        timeout: Some(COMMIT_TIMEOUT),
        stage: Some(file_paths.unwrap_or(&[]).to_vec()),
        ..GitCommitOptions::default()
    }
}

fn format_commit_message(subject: &str, body: &str) -> String {
    if body.trim().is_empty() {
        subject.to_owned()
    } else {
        format!("{subject}\n\n{}", body.trim())
    }
}

fn summarize_git_action_result(result: &GitRunStackedActionResult) -> (String, Option<String>) {
    if matches!(
        result.pr.status,
        GitPrStepStatus::Created | GitPrStepStatus::OpenedExisting
    ) {
        let verb = if result.pr.status == GitPrStepStatus::Created {
            "Created"
        } else {
            "Opened"
        };
        let number = result
            .pr
            .number
            .map(|number| format!(" #{}", number.get()))
            .unwrap_or_default();
        return (
            format!("{verb} PR{number}"),
            result
                .pr
                .title
                .as_ref()
                .map(|title| truncate_text(title.as_str())),
        );
    }
    if result.push.status == GitPushStepStatus::Pushed {
        let sha = result.commit.commit_sha.as_ref().map(|sha| {
            format!(
                " {}",
                &sha.as_str()[..sha.as_str().len().min(SHORT_SHA_LENGTH)]
            )
        });
        let branch = result
            .push
            .upstream_branch
            .as_ref()
            .or(result.push.branch.as_ref())
            .map(|branch| format!(" to {}", branch.as_str()));
        return (
            format!(
                "Pushed{}{}",
                sha.unwrap_or_default(),
                branch.unwrap_or_default()
            ),
            result
                .commit
                .subject
                .as_ref()
                .map(|subject| truncate_text(subject.as_str())),
        );
    }
    if result.commit.status == GitCommitStepStatus::Created {
        let title = result
            .commit
            .commit_sha
            .as_ref()
            .map(|sha| {
                format!(
                    "Committed {}",
                    &sha.as_str()[..sha.as_str().len().min(SHORT_SHA_LENGTH)]
                )
            })
            .unwrap_or_else(|| "Committed changes".to_owned());
        return (
            title,
            result
                .commit
                .subject
                .as_ref()
                .map(|subject| truncate_text(subject.as_str())),
        );
    }
    ("Done".to_owned(), None)
}

fn truncate_text(value: &str) -> String {
    if value.chars().count() <= TOAST_DESCRIPTION_MAX {
        return value.to_owned();
    }
    let mut shortened: String = value.chars().take(TOAST_DESCRIPTION_MAX - 3).collect();
    shortened = shortened.trim_end().to_owned();
    shortened.push_str("...");
    shortened
}

fn emit(
    progress: Option<&Arc<dyn Fn(GitActionProgressEvent) + Send + Sync>>,
    event: GitActionProgressEvent,
) {
    if let Some(progress) = progress {
        progress(event);
    }
}

fn emit_phase(
    options: &GitRunStackedActionOptions,
    input: &GitRunStackedActionInput,
    phase: GitActionProgressPhase,
    label: &str,
) -> Result<(), GitManagerServiceError> {
    emit(
        options.progress.as_ref(),
        GitActionProgressEvent::PhaseStarted {
            action_id: input.action_id.clone(),
            cwd: input.cwd.clone(),
            action: input.action,
            phase,
            label: trimmed(label)?,
        },
    );
    Ok(())
}

fn manager(operation: &str, cwd: &str, detail: &str) -> GitManagerServiceError {
    GitManagerServiceError::Manager(GitManagerError::GitManagerError {
        operation: operation.to_owned(),
        cwd: cwd.to_owned(),
        detail: detail.to_owned(),
        cause: None,
    })
}

fn trimmed(value: &str) -> Result<TrimmedNonEmptyString, GitManagerServiceError> {
    TrimmedNonEmptyString::new(value)
        .map_err(|_| manager("gitManager", "", "Expected a non-empty string."))
}

fn trimmed_or(value: &str, fallback: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value)
        .unwrap_or_else(|_| TrimmedNonEmptyString::new(fallback).expect("fallback"))
}

fn provider_error(operation: &str, cwd: &str, detail: &str) -> SourceControlProviderError {
    SourceControlProviderError::SourceControlProviderError {
        provider: SourceControlProviderKind::new("unknown").expect("kind"),
        operation: operation.to_owned(),
        cwd: cwd.to_owned(),
        command: None,
        repository: None,
        reference: None,
        detail: detail.to_owned(),
        cause: None,
    }
}

struct HeadIdentity {
    repository: Option<String>,
    owner: Option<String>,
}

fn head_identity(repository: Option<&str>, owner: Option<&str>) -> HeadIdentity {
    let repository = normalize_repo(repository);
    HeadIdentity {
        owner: normalize_owner(owner).or_else(|| {
            repository
                .as_deref()
                .and_then(|path| path.split('/').next())
                .map(str::to_owned)
        }),
        repository,
    }
}

fn pull_request_head_identity(pr: &PullRequestHead) -> HeadIdentity {
    let explicit = normalize_optional(pr.head_repository_name_with_owner.as_deref());
    let repository = explicit.or_else(|| {
        if pr.is_cross_repository != Some(true) {
            return None;
        }
        let owner = normalize_optional(pr.head_repository_owner_login.as_deref())?;
        let name = repository_name_from_pull_url(&pr.url)?;
        Some(format!("{owner}/{name}"))
    });
    let repository = normalize_repo(repository.as_deref());
    HeadIdentity {
        owner: normalize_owner(pr.head_repository_owner_login.as_deref()).or_else(|| {
            repository
                .as_deref()
                .and_then(|path| path.split('/').next())
                .map(str::to_owned)
        }),
        repository,
    }
}

fn repository_name_from_pull_url(url: &str) -> Option<String> {
    let caps = Regex::new(r"(?i)^https?://[^/]+/[^/]+/([^/]+)/pull/\d+(?:/.*)?$")
        .ok()?
        .captures(url.trim())?;
    let name = caps.get(1)?.as_str().trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

fn normalize_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn normalize_repo(value: Option<&str>) -> Option<String> {
    normalize_optional(value).map(|value| value.to_lowercase())
}

fn normalize_owner(value: Option<&str>) -> Option<String> {
    normalize_optional(value).map(|value| value.to_lowercase())
}

struct ParsedHttp {
    hostname: String,
    pathname: String,
}

fn parse_http_url(value: &str) -> Option<ParsedHttp> {
    let rest = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let host = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    let hostname = host.split(':').next().unwrap_or(host);
    if hostname.is_empty() {
        return None;
    }
    Some(ParsedHttp {
        hostname: hostname.to_owned(),
        pathname: format!("/{path}"),
    })
}

fn cache_key(cwd: &str) -> String {
    std::fs::canonicalize(cwd)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| cwd.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn commit_step_waits_for_slow_hooks_like_upstream() {
        let options = commit_step_options(None);
        assert_eq!(options.timeout, Some(Duration::from_secs(600)));
        assert_eq!(options.stage, Some(Vec::new()));
    }

    #[tokio::test]
    async fn custom_commit_message_is_committed_unchanged() {
        let manager = GitManager::new(Arc::new(GitVcsDriverCore::new()));
        let custom = manager
            .suggest_commit(".", Some("feat: custom summary line"), None)
            .await
            .expect("custom message");
        assert_eq!(custom.subject, "feat: custom summary line");
        assert_eq!(custom.body, "");

        let long = format!("Fix bug. {}", "x".repeat(90));
        let body = "Keeps the trailing period.\n\nSecond paragraph.";
        let custom = manager
            .suggest_commit(".", Some(&format!("{long}.\n\n{body}")), None)
            .await
            .expect("custom message");
        assert_eq!(custom.subject, format!("{long}."));
        assert_eq!(custom.body, body);
    }

    #[test]
    fn failure_streaks_count_up_per_key_and_clear_on_success() {
        let mut streaks = FailureStreaks::new(2);
        assert_eq!(streaks.record_failure("a"), 1);
        assert_eq!(streaks.record_failure("a"), 2);
        assert_eq!(streaks.record_failure("b"), 1);
        // A new key at capacity evicts the oldest insertion ("a").
        assert_eq!(streaks.record_failure("c"), 1);
        assert_eq!(streaks.record_failure("a"), 1);
        streaks.clear("b");
        assert_eq!(streaks.record_failure("b"), 1);
    }

    #[test]
    fn repeated_failures_escalate_the_cached_backoff() {
        let mut streaks = FailureStreaks::new(PR_LOOKUP_CACHE_CAPACITY);
        let first = pr_lookup_failure_ttl(streaks.record_failure("branch"));
        let second = pr_lookup_failure_ttl(streaks.record_failure("branch"));
        let third = pr_lookup_failure_ttl(streaks.record_failure("branch"));
        assert_eq!(first, Duration::from_secs(20));
        assert_eq!(second, Duration::from_secs(40));
        assert_eq!(third, Duration::from_secs(80));
    }

    #[test]
    fn backs_off_repeated_pr_lookup_failures() {
        assert_eq!(pr_lookup_failure_ttl(1), Duration::from_millis(20_000));
        assert_eq!(pr_lookup_failure_ttl(2), Duration::from_millis(40_000));
        assert!(pr_lookup_failure_ttl(4) > Duration::from_millis(120_000));
        assert_eq!(pr_lookup_failure_ttl(20), Duration::from_millis(900_000));
    }

    #[test]
    fn reads_the_repository_from_the_returned_pr_url() {
        let cases = [
            (
                "https://github.example.com/team/repository/pull/42?tab=files",
                Some("github.example.com/team/repository"),
            ),
            (
                "https://gitlab.example.com/group/subgroup/repository/-/merge_requests/42",
                Some("gitlab.example.com/group/subgroup/repository"),
            ),
            (
                "https://bitbucket.org/team/repository/pull-requests/42",
                Some("bitbucket.org/team/repository"),
            ),
            (
                "https://dev.azure.com/org/project/_git/repository/pullrequest/42",
                Some("dev.azure.com/org/project/_git/repository"),
            ),
            (
                "https://org.visualstudio.com/project/_git/repository/pullrequest/42",
                Some("org.visualstudio.com/project/_git/repository"),
            ),
            (
                "https://gitlab.example/group/pull/123/repository/-/merge_requests/42",
                Some("gitlab.example/group/pull/123/repository"),
            ),
            ("https://github.example.com/team/repository/issues/42", None),
        ];
        for (url, expected) in cases {
            assert_eq!(
                pull_request_repository_key(url).as_deref(),
                expected,
                "{url}"
            );
        }
    }

    #[test]
    fn matches_forks_and_rejects_same_repo_metadata_for_a_cross_repo_head() {
        let head = BranchHeadContext {
            head_branch: "statemachine".into(),
            head_repository_name_with_owner: Some("pingdotgg/codething-mvp".into()),
            head_repository_owner_login: Some("pingdotgg".into()),
            is_cross_repository: true,
        };
        assert!(!matches_branch_head_context(
            &PullRequestHead {
                url: "https://github.com/pingdotgg/codething-mvp/pull/41".into(),
                head_ref_name: "statemachine".into(),
                is_cross_repository: Some(false),
                head_repository_name_with_owner: Some("pingdotgg/codething-mvp".into()),
                head_repository_owner_login: Some("pingdotgg".into()),
            },
            &head,
        ));
        assert!(matches_branch_head_context(
            &PullRequestHead {
                url: "https://github.com/pingdotgg/codething-mvp/pull/142".into(),
                head_ref_name: "statemachine".into(),
                is_cross_repository: Some(true),
                head_repository_name_with_owner: Some("pingdotgg/codething-mvp".into()),
                head_repository_owner_login: Some("pingdotgg".into()),
            },
            &head,
        ));
        assert_eq!(
            repository_path_from_remote_url("https://gitlab.example/group/maria/project.git")
                .as_deref(),
            Some("group/maria/project")
        );
    }

    fn git(cwd: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .status()
            .expect("git");
        assert!(status.success());
    }

    #[tokio::test]
    async fn commits_a_provided_message_and_reports_a_dirty_tree() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        git(cwd, &["init", "-b", "main"]);
        git(cwd, &["config", "user.email", "test@test.com"]);
        git(cwd, &["config", "user.name", "Test"]);
        git(cwd, &["config", "commit.gpgsign", "false"]);
        std::fs::write(cwd.join("README.md"), "hello\n").unwrap();
        git(cwd, &["add", "."]);
        git(cwd, &["commit", "-m", "initial"]);
        std::fs::write(cwd.join("note.txt"), "changed\n").unwrap();

        let manager = GitManager::new(Arc::new(GitVcsDriverCore::new()));
        let cwd_name = trimmed(cwd.to_str().unwrap()).unwrap();
        let status = manager
            .status(&VcsStatusInput {
                cwd: cwd_name.clone(),
            })
            .await
            .unwrap();
        assert!(status.is_repo);
        assert!(status.has_working_tree_changes);

        let result = manager
            .run_stacked_action(
                &GitRunStackedActionInput {
                    action_id: trimmed("action-1").unwrap(),
                    cwd: cwd_name,
                    action: GitStackedAction::Commit,
                    commit_message: Some(trimmed("Ship the note.").unwrap()),
                    feature_branch: None,
                    file_paths: None,
                    thread_id: None,
                    project_id: None,
                },
                GitRunStackedActionOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(result.commit.status, GitCommitStepStatus::Created);
        assert_eq!(result.commit.subject.unwrap().as_str(), "Ship the note.");
        assert!(matches!(
            result.toast.cta,
            GitRunStackedActionToastCta::RunAction { .. }
        ));
        let log = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["log", "-1", "--format=%s"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "Ship the note.");
    }
}
