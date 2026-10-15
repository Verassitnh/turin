//! Port of upstream/apps/server/src/orchestration-v2/ThreadSettlementService.ts
//!
//! The pure decisions: whether a thread is an auto-settlement candidate,
//! when (if ever) it settles, whether it is snoozed, and the settings key
//! that tells a settings edit apart from an unrelated one.

use super::orchestrator_port::{BoxError, OrchestratorPort};
use crate::projection::shared::pending_background_work::background_work_holds_completion;
use crate::projection::store::shell::ProjectionSettlementCandidate;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::FutureExt;
use futures::future::BoxFuture;
use futures::stream::{BoxStream, StreamExt};
use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::task::JoinHandle;
use turin_contracts::orchestration_project::OrchestrationProjectShell;
use turin_contracts::orchestration_v2::{
    OrchestrationV2AppThread, OrchestrationV2Command, OrchestrationV2DomainEvent,
    OrchestrationV2ProviderSessionStatus, OrchestrationV2RunStatus, OrchestrationV2ServerCommand,
    OrchestrationV2SettleOverride, OrchestrationV2ShellThreadStatus,
};
use turin_contracts::pull_request::PullRequestState;
use turin_contracts::settings::{ProjectSettingsOverrides, ServerSettings};
use turin_contracts::thread_pull_request::ThreadLinkedPullRequest;
use turin_contracts::{CommandId, ProjectId, ThreadId, UtcDateTime};
use turin_shared::drainable_worker::{DrainableWorker, WorkerItemError};

const DAY_MS: i64 = 24 * 60 * 60 * 1_000;
/// A recent user message stays queued until a run adopts its timestamp.
pub const QUEUED_TURN_START_GRACE_MS: i64 = 2 * 60 * 1_000;

/// The pull request an auto-settlement decision reads. Upstream's
/// `SettlementPullRequest`: the state plus the terminal timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementPullRequest {
    pub state: PullRequestState,
    pub closed_at: Option<String>,
    pub merged_at: Option<String>,
}

/// One settlement decision's inputs: the thread and the settings in force for
/// its project.
#[derive(Debug, Clone)]
pub struct AutoSettlementInput<'a> {
    pub thread: &'a ProjectionSettlementCandidate,
    pub pull_request: Option<SettlementPullRequest>,
    pub now_ms: i64,
    pub auto_settle_after_days: Option<f64>,
    pub auto_settle_on_merge: bool,
    pub keep_pull_requests: bool,
}

/// Settlement settings resolved for one project.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SettlementSettings {
    pub after_days: Option<f64>,
    pub on_merge: bool,
    pub keep_pull_requests: bool,
}

pub(crate) fn to_millis(value: Option<&UtcDateTime>) -> Option<i64> {
    value.map(|value| value.as_datetime().timestamp_millis())
}

/// `Option<Option<T>>` fields: the inner `None` is an explicit null.
fn flatten_millis(value: Option<&Option<UtcDateTime>>) -> Option<i64> {
    to_millis(value.and_then(Option::as_ref))
}

pub(crate) fn utc_from_millis(ms: i64) -> UtcDateTime {
    // Only ever fed millisecond values read from a `UtcDateTime`, which are in range.
    UtcDateTime::from_datetime(
        DateTime::<Utc>::from_timestamp_millis(ms).expect("millis came from a UtcDateTime"),
    )
}

fn latest_millis(values: &[Option<i64>]) -> Option<i64> {
    values.iter().flatten().copied().reduce(i64::max)
}

static AZURE_SSH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:ssh\.dev\.azure\.com|vs-ssh\.visualstudio\.com)/v3/([^/]+)/([^/]+)/([^/]+)$")
        .expect("static regex")
});
static AZURE_HTTPS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([^.]+)\.visualstudio\.com/(?:defaultcollection/)?([^/]+)/_git/([^/]+)$")
        .expect("static regex")
});

pub fn canonical_repository_key(key: &str) -> String {
    let key = AZURE_SSH.replace(key, "dev.azure.com/$1/$2/_git/$3");
    AZURE_HTTPS
        .replace(&key, "dev.azure.com/$1/$2/_git/$3")
        .into_owned()
}

/// A merged or closed pull request settles the thread unless the user wrote to
/// it afterwards. Runs that background work, a PR watch, or another agent
/// started do not count, so they cannot hold a merged thread open.
fn pull_request_settles(
    thread: &ProjectionSettlementCandidate,
    pull_request: &SettlementPullRequest,
    auto_settle_on_merge: bool,
) -> bool {
    if pull_request.state != PullRequestState::Closed
        && (pull_request.state != PullRequestState::Merged || !auto_settle_on_merge)
    {
        return false;
    }
    let terminal_at = match pull_request.state {
        PullRequestState::Merged => pull_request.merged_at.as_deref(),
        _ => pull_request.closed_at.as_deref(),
    };
    let Some(terminal_at) = terminal_at else {
        return false;
    };
    let user_anchor_ms = latest_millis(&[
        to_millis(Some(&thread.created_at)),
        to_millis(thread.latest_user_authored_message_at.as_ref()),
    ]);
    let Some(user_anchor_ms) = user_anchor_ms else {
        return false;
    };
    match DateTime::parse_from_rfc3339(terminal_at) {
        Ok(at) => at.timestamp_millis() >= user_anchor_ms,
        Err(_) => false,
    }
}

/// A recent user message stays queued until a run adopts its timestamp.
/// Absolute age bounds client clock skew in both directions and stops stale
/// pre-adoption data from blocking the thread forever. A failed run start
/// clears the block immediately.
pub fn thread_has_queued_turn_start(thread: &ProjectionSettlementCandidate, now_ms: i64) -> bool {
    let Some(message_at_ms) = to_millis(thread.latest_user_message_at.as_ref()) else {
        return false;
    };
    if thread.status == OrchestrationV2ShellThreadStatus::Failed {
        return false;
    }
    let age = now_ms - message_at_ms;
    if age.abs() > QUEUED_TURN_START_GRACE_MS {
        return false;
    }
    if thread.latest_run_id.is_none() {
        return true;
    }
    [
        to_millis(thread.latest_run_requested_at.as_ref()),
        to_millis(thread.latest_run_started_at.as_ref()),
        to_millis(thread.latest_run_completed_at.as_ref()),
    ]
    .iter()
    .all(|value| value.is_none_or(|value| value < message_at_ms))
}

/// Cheap checks that run before any source control lookup.
pub fn is_auto_settlement_candidate(thread: &ProjectionSettlementCandidate, now_ms: i64) -> bool {
    if thread.archived_at.is_some() || thread.settled_override.is_some() {
        return false;
    }
    if thread.pinned_at.is_some() || thread.auto_settle_disabled_at.is_some() {
        return false;
    }
    // Blocked-on-you work must never park behind a settled override.
    if thread.pending_runtime_request.is_some() {
        return false;
    }
    // A live run, or background work that will wake the agent, is not
    // staleness. A dev server left running is: the agent is done.
    if thread.activity_run_status.is_some() {
        return false;
    }
    if background_work_holds_completion(&thread.pending_background_tasks) {
        return false;
    }
    if thread_has_queued_turn_start(thread, now_ms) {
        return false;
    }
    let snoozed_until_ms = to_millis(thread.snoozed_until.as_ref());
    if snoozed_until_ms.is_none_or(|until| until <= now_ms) {
        return true;
    }
    // A snoozed thread that woke early (error or completed work) can settle;
    // one still parked on its wake time keeps its stronger statement.
    let snoozed_at_ms = to_millis(thread.snoozed_at.as_ref());
    let completed_at_ms = to_millis(thread.latest_run_completed_at.as_ref());
    let woke_on_error = thread.status == OrchestrationV2ShellThreadStatus::Failed
        && (snoozed_at_ms.is_none()
            || completed_at_ms.is_some_and(|completed| Some(completed) > snoozed_at_ms));
    let woke_on_completion = match (snoozed_at_ms, completed_at_ms) {
        (Some(snoozed), Some(completed)) => completed > snoozed,
        _ => false,
    };
    woke_on_error || woke_on_completion
}

/// Whether a thread is parked on its snooze: its wake time is in the future and
/// it has not raised its hand with a pending request, a fresh failure, or work
/// that completed after the snooze. Server twin of the client's
/// `effectiveSnoozed`.
pub fn is_snoozed(thread: &ProjectionSettlementCandidate, now_ms: i64) -> bool {
    let Some(snoozed_until_ms) = to_millis(thread.snoozed_until.as_ref()) else {
        return false;
    };
    if snoozed_until_ms <= now_ms {
        return false;
    }
    if thread.pending_runtime_request.is_some() {
        return false;
    }
    let snoozed_at_ms = to_millis(thread.snoozed_at.as_ref());
    let completed_at_ms = to_millis(thread.latest_run_completed_at.as_ref());
    let woke_on_error = thread.status == OrchestrationV2ShellThreadStatus::Failed
        && (snoozed_at_ms.is_none()
            || completed_at_ms.is_some_and(|completed| Some(completed) > snoozed_at_ms));
    // Like the client, only a run that completed wakes it; an interrupt or cancel does not.
    let woke_on_completion = thread.status == OrchestrationV2ShellThreadStatus::Completed
        && match (snoozed_at_ms, completed_at_ms) {
            (Some(snoozed), Some(completed)) => completed > snoozed,
            _ => false,
        };
    !woke_on_error && !woke_on_completion
}

/// When a thread settles, or `None` when it stays. A thread with pull request
/// links settles from those links' terminal state and never from inactivity
/// alone unless `keep_pull_requests` is off.
pub fn resolve_auto_settlement_at(input: &AutoSettlementInput<'_>) -> Option<UtcDateTime> {
    let thread = input.thread;
    let mut pull_request = input.pull_request.clone();
    let links = turin_shared::thread_pull_requests::visible_thread_pull_requests(
        thread.pull_requests.as_deref().unwrap_or_default(),
    );
    if links.iter().any(|link| {
        link.snapshot
            .as_ref()
            .is_none_or(|s| s.state == PullRequestState::Open)
    }) {
        return None;
    }
    if !links.is_empty() {
        let terminal_at = |link: &&turin_contracts::thread_pull_request::ThreadPullRequestLink| {
            let Some(snapshot) = link.snapshot.as_ref() else {
                return f64::NEG_INFINITY;
            };
            let value = match snapshot.state {
                PullRequestState::Merged => snapshot.merged_at.as_ref().and_then(Option::as_ref),
                _ => snapshot.closed_at.as_ref().and_then(Option::as_ref),
            };
            value
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map_or(f64::NEG_INFINITY, |at| at.timestamp_millis() as f64)
        };
        // The first of equal timestamps wins, as in upstream's reduce.
        let latest = links
            .iter()
            .copied()
            .reduce(|current, candidate| {
                if terminal_at(&candidate) > terminal_at(&current) {
                    candidate
                } else {
                    current
                }
            })
            .expect("links is non-empty");
        pull_request = latest
            .snapshot
            .as_ref()
            .map(|snapshot| SettlementPullRequest {
                state: snapshot.state,
                merged_at: snapshot
                    .merged_at
                    .clone()
                    .flatten()
                    .map(|at| at.to_string()),
                closed_at: snapshot.closed_at.clone().flatten(),
            });
    }
    if !is_auto_settlement_candidate(thread, input.now_ms) {
        return None;
    }
    let activity_at_ms = latest_millis(&[
        to_millis(thread.latest_user_message_at.as_ref()),
        to_millis(thread.latest_run_requested_at.as_ref()),
        to_millis(thread.latest_run_started_at.as_ref()),
        to_millis(thread.latest_run_completed_at.as_ref()),
    ]);
    if let Some(pr) = &pull_request
        && pull_request_settles(thread, pr, input.auto_settle_on_merge)
    {
        return Some(match activity_at_ms {
            Some(ms) => utc_from_millis(ms),
            None => thread.created_at,
        });
    }
    let has_pull_request = pull_request.is_some()
        || thread
            .linked_pull_request
            .as_ref()
            .is_some_and(Option::is_some)
        || thread
            .branch_pull_request
            .as_ref()
            .is_some_and(Option::is_some);
    if input.keep_pull_requests && has_pull_request {
        return None;
    }
    let after_days = input.auto_settle_after_days?;
    let activity_at_ms = activity_at_ms?;
    // A passed wake timer restarts inactivity. An earlier wake can only delay settlement.
    let snoozed_until_ms = to_millis(thread.snoozed_until.as_ref());
    let timer_wake_at_ms = snoozed_until_ms.filter(|until| *until <= input.now_ms);
    let inactivity_at_ms = latest_millis(&[
        Some(activity_at_ms),
        flatten_millis(thread.last_snooze_wake_at.as_ref()),
        timer_wake_at_ms,
    ])
    .unwrap_or(activity_at_ms);
    ((inactivity_at_ms as f64) < (input.now_ms as f64) - after_days * DAY_MS as f64)
        .then(|| utc_from_millis(inactivity_at_ms))
}

/// The settlement settings in force for one project: its override where set,
/// otherwise the environment value. `after_days` set to an explicit null in an
/// override disables inactivity settlement for that project.
pub fn resolve_settlement_settings(
    settings: &ServerSettings,
    project_id: &ProjectId,
) -> SettlementSettings {
    let overrides: Option<&ProjectSettingsOverrides> =
        settings.project_settings_overrides.get(project_id);
    let after_days = match overrides.and_then(|o| o.sidebar_auto_settle_after_days) {
        Some(value) => value.map(|days| days.get()),
        None => settings
            .sidebar_auto_settle_after_days
            .map(|days| days.get()),
    };
    SettlementSettings {
        after_days,
        on_merge: overrides
            .and_then(|o| o.sidebar_auto_settle_on_merge)
            .unwrap_or(settings.sidebar_auto_settle_on_merge),
        keep_pull_requests: overrides
            .and_then(|o| o.sidebar_auto_settle_keep_pull_requests)
            .unwrap_or(settings.sidebar_auto_settle_keep_pull_requests),
    }
}

/// Whether any setting can settle a thread at all.
pub fn auto_settlement_configured(settings: &ServerSettings) -> bool {
    if settings.sidebar_auto_settle_on_merge || settings.sidebar_auto_settle_after_days.is_some() {
        return true;
    }
    settings.project_settings_overrides.values().any(|entry| {
        entry.sidebar_auto_settle_on_merge == Some(true)
            || matches!(entry.sidebar_auto_settle_after_days, Some(Some(_)))
    })
}

/// Identity of every settlement input, so unrelated settings edits do not
/// trigger a sweep. Inherit (absent) and never (null) get distinct marks.
pub fn auto_settlement_settings_key(settings: &ServerSettings) -> String {
    let mut entries: Vec<_> = settings
        .project_settings_overrides
        .iter()
        .filter(|(_, entry)| {
            entry.sidebar_auto_settle_on_merge.is_some()
                || entry.sidebar_auto_settle_after_days.is_some()
                || entry.sidebar_auto_settle_keep_pull_requests.is_some()
        })
        .map(|(project_id, entry)| {
            let on_merge = match entry.sidebar_auto_settle_on_merge {
                Some(value) => serde_json::json!(value),
                None => serde_json::json!("inherit"),
            };
            let after_days = match &entry.sidebar_auto_settle_after_days {
                None => serde_json::json!("inherit"),
                Some(None) => serde_json::Value::Null,
                Some(Some(days)) => serde_json::json!(days.get()),
            };
            let keep = match entry.sidebar_auto_settle_keep_pull_requests {
                Some(value) => serde_json::json!(value),
                None => serde_json::json!("inherit"),
            };
            (project_id.as_str().to_owned(), on_merge, after_days, keep)
        })
        .map(|(id, on_merge, after_days, keep)| serde_json::json!([id, on_merge, after_days, keep]))
        .collect();
    entries.sort_by(|left, right| left[0].as_str().cmp(&right[0].as_str()));
    serde_json::json!([
        settings.sidebar_auto_settle_on_merge,
        settings
            .sidebar_auto_settle_after_days
            .map(|days| days.get()),
        settings.sidebar_auto_settle_keep_pull_requests,
        entries,
    ])
    .to_string()
}

/// Upstream's `Schedule.spaced("1 minute")` between periodic sweeps.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// The thread-store reads the sweeps need. Backed by the T13a projection store.
#[async_trait]
pub trait SettlementStore: Send + Sync {
    /// `getSettlementCandidates`: every candidate, or only `thread_id`'s row.
    async fn settlement_candidates(
        &self,
        thread_id: Option<&ThreadId>,
    ) -> Result<Vec<ProjectionSettlementCandidate>, BoxError>;
    /// `getThread`: the thread as the settled-event cleanup reads it.
    async fn app_thread(&self, thread_id: &ThreadId) -> Result<OrchestrationV2AppThread, BoxError>;
    /// `ProjectStoreV2.listShells`.
    async fn project_shells(&self) -> Result<Vec<OrchestrationProjectShell>, BoxError>;
}

#[async_trait]
pub trait SettlementSettingsSource: Send + Sync {
    async fn settings(&self) -> Result<ServerSettings, BoxError>;
    /// `subscribeChanges`: each settings value committed from now on.
    async fn subscribe_changes(&self) -> Result<BoxStream<'static, ServerSettings>, BoxError>;
}

/// The subset of `GitBranchPullRequest` a settlement decision reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchPullRequest {
    pub state: PullRequestState,
    pub repository_key: Option<String>,
    pub closed_at: Option<String>,
    pub merged_at: Option<String>,
}

impl From<BranchPullRequest> for SettlementPullRequest {
    fn from(value: BranchPullRequest) -> Self {
        Self {
            state: value.state,
            closed_at: value.closed_at,
            merged_at: value.merged_at,
        }
    }
}

#[async_trait]
pub trait SettlementGit: Send + Sync {
    /// `branchPullRequest`; `refresh` bypasses the lookup cache.
    async fn branch_pull_request(
        &self,
        cwd: &str,
        branch: &str,
        refresh: bool,
    ) -> Result<Option<BranchPullRequest>, BoxError>;
    /// `invalidateStatus`: drops cached answers for a checkout.
    async fn invalidate_status(&self, cwd: &str);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestRef {
    pub project_id: ProjectId,
    pub repository: String,
    pub number: u64,
}

/// `PullRequestMergeEvent`: a merge the sync layer observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestMergeEvent {
    pub project_id: ProjectId,
    pub repository: String,
    pub number: u64,
    pub merged_at: String,
}

#[async_trait]
pub trait SettlementPullRequests: Send + Sync {
    /// `summary`, reduced to the terminal fields a settlement reads.
    async fn summary(
        &self,
        reference: &PullRequestRef,
        recover_transient_failure: bool,
    ) -> Result<SettlementPullRequest, BoxError>;
    async fn subscribe_merges(&self)
    -> Result<BoxStream<'static, PullRequestMergeEvent>, BoxError>;
}

#[async_trait]
pub trait SettlementTerminals: Send + Sync {
    /// `closeIdle`: closes the thread's shells sitting at an idle prompt.
    async fn close_idle(&self, thread_id: &ThreadId) -> Result<(), BoxError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettleScriptRequest {
    pub thread_id: ThreadId,
    pub project_id: ProjectId,
    pub worktree_path: String,
}

pub enum SettleScriptRun {
    NoScript,
    /// `completion` resolves when the script's shell exits; it is detached.
    Started {
        completion: Option<BoxFuture<'static, ()>>,
    },
}

#[async_trait]
pub trait SettlementScripts: Send + Sync {
    /// `ProjectSetupScriptRunner.runForThread` with trigger `settle`.
    async fn run_settle_script(
        &self,
        request: SettleScriptRequest,
    ) -> Result<SettleScriptRun, BoxError>;
}

#[async_trait]
pub trait SettlementFiles: Send + Sync {
    async fn exists(&self, path: &str) -> Result<bool, BoxError>;
}

/// The two orchestrator calls the reactor makes. Every `OrchestratorPort`
/// satisfies it.
#[async_trait]
pub trait SettlementOrchestrator: Send + Sync {
    async fn dispatch_command(&self, command: OrchestrationV2ServerCommand)
    -> Result<(), BoxError>;
    /// `streamDomainEvents`: every committed thread event from now on.
    async fn domain_events(
        &self,
    ) -> Result<BoxStream<'static, OrchestrationV2DomainEvent>, BoxError>;
}

#[async_trait]
impl<T: OrchestratorPort + ?Sized> SettlementOrchestrator for T {
    async fn dispatch_command(
        &self,
        command: OrchestrationV2ServerCommand,
    ) -> Result<(), BoxError> {
        OrchestratorPort::dispatch(self, command)
            .await
            .map(|_| ())
            .map_err(|error| Box::new(error) as BoxError)
    }

    async fn domain_events(
        &self,
    ) -> Result<BoxStream<'static, OrchestrationV2DomainEvent>, BoxError> {
        OrchestratorPort::stream_domain_events(self)
            .await
            .map_err(|error| Box::new(error) as BoxError)
    }
}

/// Everything the reactor reads or writes, behind narrow traits.
#[derive(Clone)]
pub struct SettlementDeps {
    pub orchestrator: Arc<dyn SettlementOrchestrator>,
    pub store: Arc<dyn SettlementStore>,
    pub settings: Arc<dyn SettlementSettingsSource>,
    pub git: Arc<dyn SettlementGit>,
    pub pull_requests: Arc<dyn SettlementPullRequests>,
    pub terminals: Arc<dyn SettlementTerminals>,
    pub scripts: Arc<dyn SettlementScripts>,
    pub files: Arc<dyn SettlementFiles>,
    /// `DateTime.now` as epoch milliseconds.
    pub now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
}

struct Inner {
    deps: SettlementDeps,
    worker: DrainableWorker<Option<ThreadId>>,
    /// Settling a settled thread re-emits `thread.settled` with the same
    /// `settledAt`, so the settle script runs once per settlement.
    settle_action_run_at: Mutex<HashMap<ThreadId, i64>>,
}

/// `ThreadSettlementServiceV2`: sweeps for auto-settlement and runs the
/// settle cleanup when a thread settles.
pub struct ThreadSettlementService {
    inner: Arc<Inner>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl ThreadSettlementService {
    pub fn new(deps: SettlementDeps) -> Self {
        let sweep_deps = deps.clone();
        let worker = DrainableWorker::spawn(move |thread_id: Option<ThreadId>| {
            let deps = sweep_deps.clone();
            async move {
                run_sweep(&deps, None, thread_id.as_ref()).await;
                Ok::<(), WorkerItemError>(())
            }
        });
        Self {
            inner: Arc::new(Inner {
                deps,
                worker,
                settle_action_run_at: Mutex::new(HashMap::new()),
            }),
            tasks: Mutex::new(Vec::new()),
        }
    }

    /// Subscribes to settings, merges and domain events, then runs the sweep
    /// loops. A subscription failure leaves nothing running.
    pub async fn start(&self) -> Result<(), BoxError> {
        let deps = &self.inner.deps;
        let mut settings_changes = deps.settings.subscribe_changes().await?;
        let mut merges = deps.pull_requests.subscribe_merges().await?;
        let mut events = deps.orchestrator.domain_events().await?;
        let initial = deps.settings.settings().await?;
        let mut last_key = auto_settlement_settings_key(&initial);

        let mut tasks = self.tasks.lock();
        let inner = Arc::clone(&self.inner);
        tasks.push(tokio::spawn(async move {
            loop {
                inner.worker.enqueue(None);
                inner.worker.drain().await;
                tokio::time::sleep(SWEEP_INTERVAL).await;
            }
        }));

        let inner = Arc::clone(&self.inner);
        tasks.push(tokio::spawn(async move {
            while let Some(settings) = settings_changes.next().await {
                let key = auto_settlement_settings_key(&settings);
                if key == last_key {
                    continue;
                }
                last_key = key;
                inner.worker.enqueue(None);
            }
        }));

        let inner = Arc::clone(&self.inner);
        tasks.push(tokio::spawn(async move {
            while let Some(event) = merges.next().await {
                run_sweep(&inner.deps, Some(&event), None).await;
            }
        }));

        let inner = Arc::clone(&self.inner);
        tasks.push(tokio::spawn(async move {
            while let Some(event) = events.next().await {
                process_event(&inner, event).await;
            }
        }));
        Ok(())
    }

    /// Resolves when the sweep queue is empty and idle.
    pub async fn drain(&self) {
        self.inner.worker.drain().await;
    }
}

impl Drop for ThreadSettlementService {
    fn drop(&mut self) {
        for task in self.tasks.lock().drain(..) {
            task.abort();
        }
    }
}

async fn process_event(inner: &Inner, event: OrchestrationV2DomainEvent) {
    match event {
        OrchestrationV2DomainEvent::ThreadSettled { thread_id, .. } => {
            clean_up_settled_thread(inner, thread_id).await;
        }
        OrchestrationV2DomainEvent::ThreadPullRequestLinkUpdated { thread_id, .. }
        | OrchestrationV2DomainEvent::ThreadPullRequestSynced { thread_id, .. }
        | OrchestrationV2DomainEvent::ProviderSessionDetached { thread_id, .. } => {
            inner.worker.enqueue(Some(thread_id));
        }
        OrchestrationV2DomainEvent::ProviderSessionUpdated {
            thread_id, payload, ..
        } => {
            if !matches!(
                payload.status,
                OrchestrationV2ProviderSessionStatus::Starting
                    | OrchestrationV2ProviderSessionStatus::Running
            ) {
                inner.worker.enqueue(Some(thread_id));
            }
        }
        OrchestrationV2DomainEvent::RunUpdated {
            thread_id, payload, ..
        } => {
            if matches!(
                payload.status,
                OrchestrationV2RunStatus::Completed
                    | OrchestrationV2RunStatus::Interrupted
                    | OrchestrationV2RunStatus::Failed
                    | OrchestrationV2RunStatus::Cancelled
                    | OrchestrationV2RunStatus::RolledBack
            ) {
                inner.worker.enqueue(Some(thread_id));
            }
        }
        _ => {}
    }
}

/// Settling closes the thread's shells that sit at an idle prompt, then runs
/// the project's settle script in the thread's own worktree. A thread in the
/// shared checkout skips the script, because other threads may be working there.
async fn clean_up_settled_thread(inner: &Inner, thread_id: ThreadId) {
    if let Err(error) = clean_up_settled_thread_inner(inner, &thread_id).await {
        tracing::warn!(thread_id = %thread_id, error = %error, "cleaning up a settled thread failed");
    }
}

async fn clean_up_settled_thread_inner(
    inner: &Inner,
    thread_id: &ThreadId,
) -> Result<(), BoxError> {
    let deps = &inner.deps;
    // A thread re-engaged before this event ran keeps its shells.
    let settled = deps.store.app_thread(thread_id).await?;
    if settled.settled_override != Some(OrchestrationV2SettleOverride::Settled) {
        return Ok(());
    }
    deps.terminals.close_idle(thread_id).await?;
    let Some(worktree_path) = settled.worktree_path.clone() else {
        return Ok(());
    };
    if !deps.files.exists(worktree_path.as_str()).await? {
        return Ok(());
    }
    // Closing and the worktree check wait on I/O. A thread re-engaged
    // meanwhile is working again, so its worktree is no place for cleanup.
    let thread = deps.store.app_thread(thread_id).await?;
    if thread.settled_override != Some(OrchestrationV2SettleOverride::Settled) {
        return Ok(());
    }
    let Some(settled_at_ms) = thread
        .settled_at
        .as_ref()
        .map(|at| at.as_datetime().timestamp_millis())
    else {
        return Ok(());
    };
    if inner.settle_action_run_at.lock().get(thread_id) == Some(&settled_at_ms) {
        return Ok(());
    }
    let run = deps
        .scripts
        .run_settle_script(SettleScriptRequest {
            thread_id: thread_id.clone(),
            project_id: thread.project_id.clone(),
            worktree_path: worktree_path.as_str().to_owned(),
        })
        .await?;
    // Recorded after a successful start, so a failed start retries on the next event.
    inner
        .settle_action_run_at
        .lock()
        .insert(thread_id.clone(), settled_at_ms);
    if let SettleScriptRun::Started {
        completion: Some(completion),
    } = run
    {
        tokio::spawn(completion);
    }
    Ok(())
}

/// The PR link a thread is tracked by: its own link first, then its branch's.
fn linked_reference(thread: &ProjectionSettlementCandidate) -> Option<&ThreadLinkedPullRequest> {
    thread
        .linked_pull_request
        .as_ref()
        .and_then(Option::as_ref)
        .or(thread.branch_pull_request.as_ref().and_then(Option::as_ref))
}

fn pull_request_matches_project(
    pull_request: &BranchPullRequest,
    project_key: Option<&str>,
) -> bool {
    match (&pull_request.repository_key, project_key) {
        (Some(repository), Some(project)) => {
            canonical_repository_key(repository) == canonical_repository_key(project)
        }
        _ => false,
    }
}

/// Groups candidates by what one lookup answers, so each branch or PR is
/// asked about once per sweep.
fn lookup_key(thread: &ProjectionSettlementCandidate, cwd: Option<&String>) -> String {
    if let Some(reference) = linked_reference(thread) {
        return json!([
            "linked",
            reference.project_id,
            reference.repository,
            reference.number,
            cwd,
            thread.branch,
        ])
        .to_string();
    }
    match (&thread.branch, cwd) {
        (None, _) => json!(["none", thread.id]).to_string(),
        (Some(_), None) => json!(["missing-project", thread.id]).to_string(),
        (Some(branch), Some(cwd)) => json!(["branch", cwd, branch]).to_string(),
    }
}

/// `resolveProjectSettings(...)` for the sidebar keys, then the decision.
async fn settle_thread(
    deps: &SettlementDeps,
    thread: &ProjectionSettlementCandidate,
    pull_request: Option<SettlementPullRequest>,
) -> Result<bool, BoxError> {
    let current = deps.settings.settings().await?;
    let resolved = resolve_settlement_settings(&current, &thread.project_id);
    let settled_at = resolve_auto_settlement_at(&AutoSettlementInput {
        thread,
        pull_request,
        now_ms: (deps.now_ms)(),
        auto_settle_after_days: resolved.after_days,
        auto_settle_on_merge: resolved.on_merge,
        keep_pull_requests: resolved.keep_pull_requests,
    });
    let Some(settled_at) = settled_at else {
        return Ok(false);
    };
    let uuid = uuid::Uuid::new_v4();
    let command_id = CommandId::new(format!("server:auto-settle:{}:{uuid}", thread.id.as_str()))
        .expect("a formatted command id is non-empty");
    deps.orchestrator
        .dispatch_command(OrchestrationV2ServerCommand::Client(
            OrchestrationV2Command::ThreadAutoSettle {
                command_id,
                thread_id: thread.id.clone(),
                snapshot_at: thread.updated_at,
                settled_at: Some(settled_at),
            },
        ))
        .await?;
    Ok(true)
}

/// Whether settling any thread in `group` would follow from `pull_request`.
async fn would_settle(
    deps: &SettlementDeps,
    group: &[ProjectionSettlementCandidate],
    pull_request: &SettlementPullRequest,
) -> Result<bool, BoxError> {
    let current = deps.settings.settings().await?;
    let now_ms = (deps.now_ms)();
    Ok(group.iter().any(|thread| {
        let resolved = resolve_settlement_settings(&current, &thread.project_id);
        resolve_auto_settlement_at(&AutoSettlementInput {
            thread,
            pull_request: Some(pull_request.clone()),
            now_ms,
            auto_settle_after_days: resolved.after_days,
            auto_settle_on_merge: resolved.on_merge,
            keep_pull_requests: resolved.keep_pull_requests,
        })
        .is_some()
    }))
}

/// What a group settles from: `None` skips the group (a recheck found nothing
/// to do), `Some(None)` settles without a PR, `Some(Some(pr))` settles from `pr`.
type GroupPullRequest = Option<Option<SettlementPullRequest>>;

struct SweepContext<'a> {
    deps: &'a SettlementDeps,
    merged: Option<&'a PullRequestMergeEvent>,
    projects: &'a HashMap<ProjectId, OrchestrationProjectShell>,
    cwd_by_thread: &'a HashMap<ThreadId, String>,
}

async fn pull_request_for(
    context: &SweepContext<'_>,
    group: &[ProjectionSettlementCandidate],
) -> Result<GroupPullRequest, BoxError> {
    let thread = &group[0];
    let deps = context.deps;
    if let Some(reference) = linked_reference(thread) {
        // The event carries the merged state, so only the threads linked to
        // that exact pull request settle from it. Every other linked thread
        // falls through to a fresh summary lookup.
        let matches_merge = context.merged.is_some_and(|merge| {
            reference.project_id == merge.project_id
                && reference.repository.as_str().to_lowercase() == merge.repository.to_lowercase()
                && reference.number.get() == merge.number
        });
        if !matches_merge && !context.projects.contains_key(&reference.project_id) {
            return Err("linked pull request project not found".into());
        }
        let terminal = match (matches_merge, context.merged) {
            (true, Some(merge)) => SettlementPullRequest {
                state: PullRequestState::Merged,
                closed_at: None,
                merged_at: Some(merge.merged_at.clone()),
            },
            _ => {
                deps.pull_requests
                    .summary(
                        &PullRequestRef {
                            project_id: reference.project_id.clone(),
                            repository: reference.repository.as_str().to_owned(),
                            number: reference.number.get(),
                        },
                        false,
                    )
                    .await?
            }
        };
        let cwd = context.cwd_by_thread.get(&thread.id);
        if let (false, Some(branch), Some(cwd)) = (
            terminal.state == PullRequestState::Open,
            thread.branch.as_ref(),
            cwd,
        ) {
            // Recheck reused branches only when this sweep would settle a thread.
            if !would_settle(deps, group, &terminal).await? {
                return Ok(None);
            }
            let current = deps
                .git
                .branch_pull_request(cwd, branch.as_str(), true)
                .await?;
            let project_key = context
                .projects
                .get(&thread.project_id)
                .and_then(|project| project.repository_identity.as_ref())
                .and_then(Option::as_ref)
                .map(|identity| identity.canonical_key.as_str());
            if let Some(current) = current
                && current.state == PullRequestState::Open
                && pull_request_matches_project(&current, project_key)
            {
                return Ok(Some(Some(current.into())));
            }
        }
        return Ok(Some(Some(terminal)));
    }
    let Some(branch) = thread.branch.as_ref() else {
        return Ok(Some(None));
    };
    let cwd = context
        .cwd_by_thread
        .get(&thread.id)
        .ok_or("thread project not found")?;
    let current = deps
        .git
        .branch_pull_request(cwd, branch.as_str(), false)
        .await?;
    Ok(Some(current.map(Into::into)))
}

async fn run_sweep(
    deps: &SettlementDeps,
    merged: Option<&PullRequestMergeEvent>,
    thread_id: Option<&ThreadId>,
) {
    // A panic here must not unwind into the DrainableWorker task: that task
    // would die, and the next `drain()` would hang, silently stopping the
    // periodic sweep. Upstream `runSweep` uses `Effect.catchCause`, which
    // contains defects too, so a panicking sweep is logged as a failed item.
    match AssertUnwindSafe(sweep(deps, merged, thread_id))
        .catch_unwind()
        .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(error = %error, "automatic thread settlement sweep failed");
        }
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".to_owned());
            tracing::error!(panic = %message, "automatic thread settlement sweep panicked");
        }
    }
}

async fn sweep(
    deps: &SettlementDeps,
    merged: Option<&PullRequestMergeEvent>,
    thread_id: Option<&ThreadId>,
) -> Result<(), BoxError> {
    let settings = deps.settings.settings().await?;
    if !auto_settlement_configured(&settings) {
        return Ok(());
    }
    // A sweep for one thread reads only that thread's candidate row.
    let threads = deps.store.settlement_candidates(thread_id).await?;
    if threads.is_empty() {
        return Ok(());
    }
    let projects: HashMap<ProjectId, OrchestrationProjectShell> = deps
        .store
        .project_shells()
        .await?
        .into_iter()
        .map(|project| (project.id.clone(), project))
        .collect();
    // A merge event re-sweeps every candidate, not just the threads linked to
    // the merged pull request: most threads carry no link and settle from
    // their branch lookup.
    let now_ms = (deps.now_ms)();
    let candidates: Vec<ProjectionSettlementCandidate> = threads
        .into_iter()
        .filter(|thread| is_auto_settlement_candidate(thread, now_ms))
        .collect();

    // Inactivity is entirely projection-backed. Complete those decisions before
    // a source-control lookup can delay or fail an otherwise eligible thread.
    let mut lookup_candidates = Vec::new();
    for thread in candidates {
        match settle_thread(deps, &thread, None).await {
            Ok(true) => {}
            Ok(false) => lookup_candidates.push(thread),
            Err(error) => {
                tracing::warn!(thread_id = %thread.id, error = %error, "automatic thread settlement skipped");
            }
        }
    }
    let lookup_candidates: Vec<ProjectionSettlementCandidate> = lookup_candidates
        .into_iter()
        .filter(visible_thread_pull_requests_is_empty)
        .collect();

    // Use the same cwd as the sidebar so both paths share GitManager's PR cache.
    let mut cwd_by_thread: HashMap<ThreadId, String> = HashMap::new();
    for thread in &lookup_candidates {
        let Some(project) = projects.get(&thread.project_id) else {
            continue;
        };
        if thread.branch.is_none() {
            continue;
        }
        let worktree_exists = match &thread.worktree_path {
            Some(path) => deps.files.exists(path.as_str()).await.unwrap_or(false),
            None => false,
        };
        let cwd = match (&thread.worktree_path, worktree_exists) {
            (Some(path), true) => path.as_str().to_owned(),
            _ => project.workspace_root.as_str().to_owned(),
        };
        cwd_by_thread.insert(thread.id.clone(), cwd);
    }
    if merged.is_some() {
        // The merge just confirmed a terminal state the lookup caches can still
        // call open. Drop the swept checkouts' cached answers so the merge
        // settles its branch threads now instead of on a later sweep.
        let cwds: HashSet<&String> = cwd_by_thread.values().collect();
        for cwd in cwds {
            deps.git.invalidate_status(cwd).await;
        }
    }

    let mut groups: IndexMap<String, Vec<ProjectionSettlementCandidate>> = IndexMap::new();
    for thread in lookup_candidates {
        let key = lookup_key(&thread, cwd_by_thread.get(&thread.id));
        groups.entry(key).or_default().push(thread);
    }

    let context = SweepContext {
        deps,
        merged,
        projects: &projects,
        cwd_by_thread: &cwd_by_thread,
    };
    for group in groups.values() {
        let pull_request = match pull_request_for(&context, group).await {
            Ok(pull_request) => pull_request,
            Err(error) => {
                let thread_ids: Vec<String> = group
                    .iter()
                    .map(|thread| thread.id.as_str().to_owned())
                    .collect();
                tracing::warn!(?thread_ids, error = %error, "automatic thread settlement skipped");
                continue;
            }
        };
        let Some(pull_request) = pull_request else {
            continue;
        };
        for thread in group {
            if let Err(error) = settle_thread(deps, thread, pull_request.clone()).await {
                tracing::warn!(thread_id = %thread.id, error = %error, "automatic thread settlement skipped");
            }
        }
    }
    Ok(())
}

fn visible_thread_pull_requests_is_empty(thread: &ProjectionSettlementCandidate) -> bool {
    turin_shared::thread_pull_requests::visible_thread_pull_requests(
        thread.pull_requests.as_deref().unwrap_or_default(),
    )
    .is_empty()
}

#[cfg(test)]
mod pure_tests;
#[cfg(test)]
mod service_tests;
