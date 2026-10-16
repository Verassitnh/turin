//! Port of upstream/apps/server/src/storageCleanup.ts
//!
//! Removes managed worktrees that the retention rules have released, and
//! expires old browser artifacts and rotated logs. Sweeps are serialized:
//! one automatic sweep waits at a time, and each manual run gets its own report.
//!
//! Settings, the projection snapshot, deleted-thread rows, open provider
//! sessions, terminal metadata, and pull-request lookup are read from
//! [`CleanupHost`]. Production wiring fills that host. This module does not
//! open the live database.
//!
//! Deviations from upstream:
//! - Effect services are the host plus [`StorageCleanup`]. `threadHasQueuedTurnStart`
//!   is applied to the shell here; the settlement helper takes a projection
//!   candidate this crate does not depend on. The grace is the same two minutes.
//! - Terminal metadata is a host snapshot. The terminal manager is a cut feature,
//!   and the tests subscribe to nothing.
//! - The removal body is not wrapped in `withWorkspaceLease`. The production host
//!   leases `worktree_path` for the sweep until workspace leases land here.
//! - Git subprocesses set `LC_ALL=C` and `GIT_TERMINAL_PROMPT=0` so failure text
//!   matches the English strings the tests assert and a prompt cannot hang a sweep.
//! - A git failure's `cause` is not retained. The report reason is
//!   `command: detail`, the same string `cleanupFailureReason` builds.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, SecondsFormat, Utc};
use parking_lot::Mutex;
use tokio::sync::{Notify, oneshot, watch};
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ShellThreadStatus, OrchestrationV2ThreadShell,
};
use turin_contracts::provider_instance::{ProviderInstanceConfigMap, ProviderInstanceId};
use turin_contracts::settings::{
    ServerSettings, StorageCleanupReport, StorageCleanupReportEntry, StorageCleanupReportEntryKind,
    StorageCleanupReportEntryOutcome, StorageCleanupReportTrigger, StorageCleanupSettings,
    WorktreeCleanupRules, WorktreeKeepWhen,
};
use turin_contracts::{ProjectId, ThreadId};
use turin_shared::drainable_worker::{DrainableWorker, WorkerItemError};
use turin_shared::project_settings::{TextGenerationFallback, resolve_worktree_cleanup};

use crate::config::ServerConfig;
use crate::support::symlink::{lexical_absolute, resolve_against};

const DAY_MS: i64 = 86_400_000;
/// Same window as `ThreadSettlementService.QUEUED_TURN_START_GRACE_MS`.
const QUEUED_TURN_START_GRACE_MS: i64 = 2 * 60 * 1_000;
const REPORT_ENTRY_LIMIT: usize = 200;
const MAX_MEASURE_ENTRIES: u64 = 2_000_000;
const MAX_GIT_OUTPUT: usize = 64 * 1024;
const SHORT_GIT: Duration = Duration::from_secs(30);
const LONG_GIT: Duration = Duration::from_secs(300);

fn no_text_fallback(
    _: &ProviderInstanceConfigMap,
    _: &dyn Fn(&ProviderInstanceId) -> bool,
    _: Option<&ModelSelection>,
) -> Option<ModelSelection> {
    None
}

fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

fn floor_div(value: i64, divisor: i64) -> i64 {
    let quotient = value / divisor;
    let remainder = value % divisor;
    if remainder != 0 && value < 0 {
        quotient - 1
    } else {
        quotient
    }
}

fn resolve_path(path: &str) -> PathBuf {
    lexical_absolute(Path::new(path)).unwrap_or_else(|_| PathBuf::from(path))
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn inside(root: &Path, target: &Path) -> bool {
    target
        .strip_prefix(root)
        .is_ok_and(|relative| relative.components().next().is_some())
}

fn is_filesystem_root(directory: &Path) -> bool {
    directory.parent().is_none_or(|parent| parent == directory)
}

fn millis_of(value: &turin_contracts::UtcDateTime) -> i64 {
    value.as_datetime().timestamp_millis()
}

fn nested_millis(value: &Option<Option<turin_contracts::UtcDateTime>>) -> Option<i64> {
    value.as_ref()?.as_ref().map(millis_of)
}

fn system_millis(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
}

fn iso_millis(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or_else(Utc::now)
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn now_millis() -> i64 {
    system_millis(SystemTime::now()).unwrap_or(0)
}

fn first_line(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or("");
    let line = if line.is_empty() {
        "command failed"
    } else {
        line
    };
    line.chars().take(1000).collect()
}

fn worktree_cleanup_enabled(rules: &WorktreeCleanupRules) -> bool {
    rules.worktree_after_days.is_some()
        || rules.worktree_on_merge
        || rules.worktree_on_delete
        || rules.worktree_unchanged
}

fn resolved_rules(
    settings: &ServerSettings,
    project_id: Option<&ProjectId>,
) -> WorktreeCleanupRules {
    resolve_worktree_cleanup(
        settings,
        project_id,
        &no_text_fallback as TextGenerationFallback<'_>,
    )
}

fn any_worktree_policy(
    settings: &ServerSettings,
    predicate: impl Fn(&WorktreeCleanupRules) -> bool,
) -> bool {
    if predicate(&resolved_rules(settings, None)) {
        return true;
    }
    settings
        .project_settings_overrides
        .keys()
        .any(|project_id| predicate(&resolved_rules(settings, Some(project_id))))
}

fn project_worktree_cleanup(
    settings: &ServerSettings,
    project_id: &ProjectId,
) -> Option<Option<turin_contracts::settings::WorktreeCleanup>> {
    match settings.project_settings_overrides.get(project_id) {
        None => None,
        Some(entry) => entry.worktree_cleanup.clone(),
    }
}

fn same_project_worktree_policies(left: &ServerSettings, right: &ServerSettings) -> bool {
    let mut keys: Vec<&ProjectId> = left
        .project_settings_overrides
        .keys()
        .chain(right.project_settings_overrides.keys())
        .collect();
    keys.sort();
    keys.dedup();
    keys.into_iter().all(|project_id| {
        project_worktree_cleanup(left, project_id) == project_worktree_cleanup(right, project_id)
    })
}

/// Whether a settings update can change what the next sweep removes.
pub fn cleanup_settings_changed(previous: &ServerSettings, next: &ServerSettings) -> bool {
    previous.storage_cleanup != next.storage_cleanup
        || previous.worktree_cleanup != next.worktree_cleanup
        || !same_project_worktree_policies(previous, next)
}

fn thread_has_queued_turn_start(thread: &OrchestrationV2ThreadShell, now_ms: i64) -> bool {
    let Some(message_at_ms) = thread.latest_user_message_at.as_ref().map(millis_of) else {
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
        nested_millis(&thread.latest_run_requested_at),
        nested_millis(&thread.latest_run_started_at),
        nested_millis(&thread.latest_run_completed_at),
    ]
    .into_iter()
    .all(|value| value.is_none_or(|value| value < message_at_ms))
}

/// Live sessions keep their cwd even when no turn is currently running.
pub fn storage_cleanup_thread_idle(thread: &OrchestrationV2ThreadShell, now_ms: i64) -> bool {
    let settled = matches!(
        thread.status,
        OrchestrationV2ShellThreadStatus::Idle
            | OrchestrationV2ShellThreadStatus::Completed
            | OrchestrationV2ShellThreadStatus::Interrupted
            | OrchestrationV2ShellThreadStatus::Failed
            | OrchestrationV2ShellThreadStatus::Cancelled
            | OrchestrationV2ShellThreadStatus::RolledBack
    );
    thread.branch.is_some()
        && thread.worktree_path.is_some()
        && thread.active_run_id.is_none()
        && settled
        && thread.pending_background_tasks.is_empty()
        && thread.pending_runtime_request.is_none()
        && !thread_has_queued_turn_start(thread, now_ms)
}

/// PR metadata refreshes must not reset the inactivity clock.
pub fn storage_cleanup_activity_at(thread: &OrchestrationV2ThreadShell) -> i64 {
    let mut latest = millis_of(&thread.created_at);
    if let Some(message_at) = thread.latest_user_message_at.as_ref() {
        latest = latest.max(millis_of(message_at));
    }
    for value in [
        &thread.latest_run_requested_at,
        &thread.latest_run_started_at,
        &thread.latest_run_completed_at,
    ] {
        if let Some(ms) = nested_millis(value) {
            latest = latest.max(ms);
        }
    }
    latest
}

/// Pull request fields the merge check reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupPullRequest<'a> {
    pub state: &'a str,
    pub head_ref: &'a str,
    pub base_ref: &'a str,
    pub head_sha: Option<&'a str>,
}

/// Worktree head the merge check compares to that pull request.
#[derive(Debug, Clone, Copy)]
pub struct CleanupMergeWorktree<'a> {
    pub branch: &'a str,
    pub default_branch: &'a str,
    pub head_sha: &'a str,
    pub integrated: bool,
}

/// Whether the host's pull request proves this worktree's head was merged.
pub fn storage_cleanup_pull_request_merged(
    pull_request: Option<&CleanupPullRequest<'_>>,
    worktree: CleanupMergeWorktree<'_>,
) -> bool {
    let Some(pull_request) = pull_request else {
        return false;
    };
    pull_request.state == "merged"
        && (worktree.integrated
            || (pull_request.head_ref == worktree.branch
                && pull_request.base_ref == worktree.default_branch
                && pull_request.head_sha == Some(worktree.head_sha)))
}

#[derive(Debug)]
enum CleanupFail {
    Git {
        command: String,
        detail: String,
        reason: Option<String>,
    },
    Other(String),
}

fn failure_reason(error: &CleanupFail) -> String {
    match error {
        CleanupFail::Git {
            command,
            detail,
            reason,
        } => match reason {
            Some(reason) if !reason.is_empty() => format!("{command}: {detail} ({reason})"),
            _ => format!("{command}: {detail}"),
        },
        CleanupFail::Other(message) => first_line(message),
    }
}

fn git_fail(command: &str, stderr: &str) -> CleanupFail {
    CleanupFail::Git {
        command: command.to_owned(),
        detail: first_line(stderr),
        reason: None,
    }
}

struct GitOutput {
    stdout: String,
    stderr: String,
    exit_code: i32,
    stdout_truncated: bool,
}

fn porcelain_count(stdout: &str) -> usize {
    let records: Vec<&str> = stdout
        .split('\0')
        .filter(|record| !record.is_empty())
        .collect();
    let mut count = 0;
    let mut index = 0;
    while index < records.len() {
        count += 1;
        let bytes = records[index].as_bytes();
        let rename = bytes
            .first()
            .is_some_and(|byte| *byte == b'R' || *byte == b'C')
            || bytes
                .get(1)
                .is_some_and(|byte| *byte == b'R' || *byte == b'C');
        if rename {
            index += 1;
        }
        index += 1;
    }
    count
}

fn is_node_modules_dir(entry: &str) -> bool {
    entry == "node_modules/" || entry.ends_with("/node_modules/")
}

fn is_rotated_log(name: &str) -> bool {
    let Some((rest, digits)) = name.rsplit_once('.') else {
        return false;
    };
    !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && (rest.ends_with(".log") || rest.ends_with(".ndjson"))
}

#[derive(Debug, Clone)]
pub struct CleanupProject {
    pub id: ProjectId,
    pub workspace_root: String,
}

#[derive(Debug, Clone)]
pub struct DeletedCleanupThread {
    pub shell: OrchestrationV2ThreadShell,
    pub workspace_root: String,
}

#[derive(Debug, Clone)]
pub struct CleanupTerminal {
    pub status: String,
    pub cwd: String,
    pub worktree_path: Option<String>,
}

/// Domain events that can queue an automatic sweep. The host's event loop
/// calls [`StorageCleanup::handle_event`] for each one.
#[derive(Debug, Clone)]
pub enum CleanupDomainEvent {
    ThreadDeleted,
    ProviderSessionUpdated { status: String },
}

/// Snapshot the sweep reads. Tests mutate it while a measurement is paused.
pub struct CleanupHost {
    settings: Mutex<ServerSettings>,
    threads: Mutex<Vec<OrchestrationV2ThreadShell>>,
    projects: Mutex<Vec<CleanupProject>>,
    deleted: Mutex<Vec<DeletedCleanupThread>>,
    open_session_cwds: Mutex<Vec<String>>,
    pending_deletion: Mutex<Vec<ThreadId>>,
    terminals: Mutex<Vec<CleanupTerminal>>,
    pull_request: Mutex<Option<StoredPullRequest>>,
    late_edit: AtomicBool,
    late_edit_path: Mutex<Option<PathBuf>>,
    full_status_calls: AtomicU64,
    snapshot_reads: AtomicU64,
    fail_read_dir: AtomicBool,
    record_walks: AtomicBool,
    walked: Mutex<Vec<String>>,
    pause_at: Mutex<Option<PathBuf>>,
    measuring: AtomicBool,
    measuring_notify: Notify,
    resume: AtomicBool,
    resume_notify: Notify,
    invalidated: AtomicU64,
}

#[derive(Debug, Clone)]
struct StoredPullRequest {
    state: String,
    head_ref: String,
    base_ref: String,
    head_sha: Option<String>,
}

impl CleanupHost {
    pub fn new(
        settings: ServerSettings,
        projects: Vec<CleanupProject>,
        threads: Vec<OrchestrationV2ThreadShell>,
    ) -> Self {
        Self {
            settings: Mutex::new(settings),
            threads: Mutex::new(threads),
            projects: Mutex::new(projects),
            deleted: Mutex::new(Vec::new()),
            open_session_cwds: Mutex::new(Vec::new()),
            pending_deletion: Mutex::new(Vec::new()),
            terminals: Mutex::new(Vec::new()),
            pull_request: Mutex::new(None),
            late_edit: AtomicBool::new(false),
            late_edit_path: Mutex::new(None),
            full_status_calls: AtomicU64::new(0),
            snapshot_reads: AtomicU64::new(0),
            fail_read_dir: AtomicBool::new(false),
            record_walks: AtomicBool::new(false),
            walked: Mutex::new(Vec::new()),
            pause_at: Mutex::new(None),
            measuring: AtomicBool::new(false),
            measuring_notify: Notify::new(),
            resume: AtomicBool::new(false),
            resume_notify: Notify::new(),
            invalidated: AtomicU64::new(0),
        }
    }

    pub fn settings(&self) -> ServerSettings {
        self.settings.lock().clone()
    }

    pub fn update_storage(&self, update: impl FnOnce(&mut StorageCleanupSettings)) {
        update(&mut self.settings.lock().storage_cleanup);
    }

    pub fn set_threads(&self, threads: Vec<OrchestrationV2ThreadShell>) {
        *self.threads.lock() = threads;
    }

    pub fn set_open_session_cwd(&self, cwd: Option<String>) {
        *self.open_session_cwds.lock() = cwd.into_iter().collect();
    }

    pub fn arm_late_edit(&self, tracked_file: PathBuf) {
        *self.late_edit_path.lock() = Some(tracked_file);
        self.late_edit.store(true, Ordering::Release);
    }

    pub fn set_pause_at(&self, path: PathBuf) {
        *self.pause_at.lock() = Some(path);
    }

    pub fn fail_read_dir(&self) {
        self.fail_read_dir.store(true, Ordering::Release);
    }

    pub fn record_walks(&self) {
        self.record_walks.store(true, Ordering::Release);
    }

    pub fn walked(&self) -> Vec<String> {
        self.walked.lock().clone()
    }

    pub fn snapshot_reads(&self) -> u64 {
        self.snapshot_reads.load(Ordering::Acquire)
    }

    pub fn full_status_calls(&self) -> u64 {
        self.full_status_calls.load(Ordering::Acquire)
    }

    pub async fn wait_measuring(&self) {
        wait_flag(&self.measuring, &self.measuring_notify).await;
    }

    pub fn resume(&self) {
        signal_flag(&self.resume, &self.resume_notify);
    }

    fn threads(&self) -> Vec<OrchestrationV2ThreadShell> {
        self.threads.lock().clone()
    }

    fn projects(&self) -> Vec<CleanupProject> {
        self.projects.lock().clone()
    }

    fn deleted_threads(&self) -> Vec<DeletedCleanupThread> {
        self.deleted.lock().clone()
    }

    fn note_snapshot(&self) {
        self.snapshot_reads.fetch_add(1, Ordering::AcqRel);
    }

    fn thread_shell(&self, thread_id: &ThreadId) -> Option<OrchestrationV2ThreadShell> {
        self.threads
            .lock()
            .iter()
            .find(|thread| &thread.id == thread_id)
            .cloned()
    }

    fn worktree_paths(&self) -> Vec<(ThreadId, String)> {
        self.threads
            .lock()
            .iter()
            .filter_map(|thread| {
                thread
                    .worktree_path
                    .as_ref()
                    .map(|path| (thread.id.clone(), path.as_str().to_owned()))
            })
            .collect()
    }

    fn deletion_pending(&self, thread_id: &ThreadId) -> bool {
        self.pending_deletion
            .lock()
            .iter()
            .any(|id| id == thread_id)
    }

    fn session_blocks(&self, worktree: &Path) -> bool {
        self.open_session_cwds.lock().iter().any(|cwd| {
            let cwd = resolve_path(cwd);
            cwd == worktree || inside(worktree, &cwd)
        })
    }

    fn has_terminal(&self, worktree: &Path) -> bool {
        self.terminals.lock().iter().any(|terminal| {
            if terminal.status != "starting" && terminal.status != "running" {
                return false;
            }
            let cwd = resolve_path(&terminal.cwd);
            let linked = terminal
                .worktree_path
                .as_ref()
                .is_some_and(|path| resolve_path(path) == worktree);
            linked || cwd == worktree || inside(worktree, &cwd)
        })
    }

    fn stored_pull_request(&self) -> Option<StoredPullRequest> {
        self.pull_request.lock().clone()
    }

    fn invalidate_status(&self) {
        self.invalidated.fetch_add(1, Ordering::AcqRel);
    }

    async fn read_dir(&self, path: &Path) -> std::io::Result<Vec<String>> {
        if self
            .pause_at
            .lock()
            .as_ref()
            .is_some_and(|pause| pause == path)
        {
            signal_flag(&self.measuring, &self.measuring_notify);
            wait_flag(&self.resume, &self.resume_notify).await;
        }
        if self.fail_read_dir.load(Ordering::Acquire) {
            return Err(std::io::Error::other("missing directory"));
        }
        if self.record_walks.load(Ordering::Acquire) {
            self.walked.lock().push(path_string(path));
        }
        let mut dir = tokio::fs::read_dir(path).await?;
        let mut names = Vec::new();
        while let Some(entry) = dir.next_entry().await? {
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_owned());
            }
        }
        Ok(names)
    }
}

async fn wait_flag(flag: &AtomicBool, notify: &Notify) {
    loop {
        let notified = notify.notified();
        tokio::pin!(notified);
        if flag.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }
}

fn signal_flag(flag: &AtomicBool, notify: &Notify) {
    flag.store(true, Ordering::Release);
    notify.notify_waiters();
}

enum Job {
    Automatic,
    Manual(oneshot::Sender<StorageCleanupReport>),
}

struct Inner {
    config: ServerConfig,
    host: CleanupHost,
    reports: watch::Sender<Option<StorageCleanupReport>>,
    automatic_queued: AtomicBool,
    last_settings: Mutex<ServerSettings>,
    activated: AtomicBool,
    worktree_cleanup_unavailable: Option<String>,
}

/// Serialized storage sweeps and the report they publish.
#[derive(Clone)]
pub struct StorageCleanup {
    inner: Arc<Inner>,
    worker: Arc<DrainableWorker<Job>>,
}

impl StorageCleanup {
    pub fn new(config: ServerConfig, host: CleanupHost) -> Self {
        Self::build(config, host, None)
    }

    /// Artifact/log cleanup for hosts that cannot yet supply current workspace
    /// leases and session/deletion snapshots. Worktree policies are reported as
    /// unavailable rather than interpreted against an empty host snapshot.
    pub fn files_only(config: ServerConfig, host: CleanupHost, reason: impl Into<String>) -> Self {
        Self::build(config, host, Some(reason.into()))
    }

    fn build(
        config: ServerConfig,
        host: CleanupHost,
        worktree_cleanup_unavailable: Option<String>,
    ) -> Self {
        let settings = host.settings();
        let (reports, _) = watch::channel(None);
        let inner = Arc::new(Inner {
            config,
            host,
            reports,
            automatic_queued: AtomicBool::new(false),
            last_settings: Mutex::new(settings),
            activated: AtomicBool::new(false),
            worktree_cleanup_unavailable,
        });
        let worker_inner = Arc::clone(&inner);
        let worker = DrainableWorker::spawn(move |job| {
            let inner = Arc::clone(&worker_inner);
            async move { inner.finish(job).await }
        });
        Self {
            inner,
            worker: Arc::new(worker),
        }
    }

    pub fn host(&self) -> &CleanupHost {
        &self.inner.host
    }

    pub fn latest_report(&self) -> Option<StorageCleanupReport> {
        self.inner.reports.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Option<StorageCleanupReport>> {
        self.inner.reports.subscribe()
    }

    pub async fn drain(&self) {
        self.worker.drain().await;
    }

    pub async fn run_now(&self) -> StorageCleanupReport {
        let (tx, rx) = oneshot::channel();
        self.worker.enqueue(Job::Manual(tx));
        rx.await
            .expect("storage cleanup worker dropped the manual run")
    }

    /// Queues and waits for one automatic sweep; the composition root owns the timer.
    pub async fn run_automatic(&self) {
        self.request_automatic();
        self.drain().await;
    }

    /// Starts the hourly automatic sweep. Further calls do not start another loop.
    /// Domain events are delivered with [`Self::handle_event`].
    pub fn activate(&self) {
        *self.inner.last_settings.lock() = self.inner.host.settings();
        if self.inner.activated.swap(true, Ordering::AcqRel) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                this.request_automatic();
                this.drain().await;
                tokio::time::sleep(Duration::from_secs(60 * 60)).await;
            }
        });
    }

    /// `forkParked` event consumer: deleted threads and stopped or errored
    /// sessions queue one automatic sweep when some project deletes worktrees.
    pub fn handle_event(&self, event: &CleanupDomainEvent) {
        let relevant = match event {
            CleanupDomainEvent::ThreadDeleted => true,
            CleanupDomainEvent::ProviderSessionUpdated { status } => {
                status == "stopped" || status == "error"
            }
        };
        if !relevant {
            return;
        }
        let settings = self.inner.last_settings.lock().clone();
        if any_worktree_policy(&settings, |rules| rules.worktree_on_delete) {
            self.request_automatic();
        }
    }

    /// Settings subscription: a cleanup-policy change queues one automatic sweep.
    pub fn note_settings(&self, settings: ServerSettings) {
        *self.inner.host.settings.lock() = settings.clone();
        let mut last = self.inner.last_settings.lock();
        if !cleanup_settings_changed(&last, &settings) {
            return;
        }
        *last = settings;
        drop(last);
        self.request_automatic();
    }

    fn request_automatic(&self) {
        if self.inner.automatic_queued.swap(true, Ordering::AcqRel) {
            return;
        }
        self.worker.enqueue(Job::Automatic);
    }
}

struct WorktreeCandidate<'a> {
    thread: &'a OrchestrationV2ThreadShell,
    deleted: bool,
    workspace_root: Option<&'a str>,
    shared: usize,
    projects: &'a [CleanupProject],
    rules: &'a WorktreeCleanupRules,
    now: i64,
    roots: &'a [PathBuf],
}

impl Inner {
    async fn finish(self: &Arc<Self>, job: Job) -> Result<(), WorkerItemError> {
        let manual = match &job {
            Job::Automatic => {
                self.automatic_queued.store(false, Ordering::Release);
                false
            }
            Job::Manual(_) => true,
        };
        let trigger = if manual {
            StorageCleanupReportTrigger::Manual
        } else {
            StorageCleanupReportTrigger::Automatic
        };
        let report = self.sweep(trigger).await;
        if let Job::Manual(tx) = job {
            let _ = tx.send(report);
        }
        Ok(())
    }

    async fn git(
        &self,
        cwd: &Path,
        args: &[&str],
        timeout: Duration,
    ) -> Result<GitOutput, CleanupFail> {
        if args.contains(&"remove")
            && self.host.late_edit.load(Ordering::Acquire)
            && let Some(path) = self.host.late_edit_path.lock().clone()
        {
            std::fs::write(&path, "late edit\n")
                .map_err(|error| CleanupFail::Other(error.to_string()))?;
        }
        let mut command = tokio::process::Command::new("git");
        command
            .args(args)
            .current_dir(cwd)
            .env("LC_ALL", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .kill_on_drop(true);
        let output = match tokio::time::timeout(timeout, command.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => return Err(CleanupFail::Other(error.to_string())),
            Err(_) => return Err(CleanupFail::Other("command failed".to_owned())),
        };
        Ok(GitOutput {
            stdout_truncated: output.stdout.len() > MAX_GIT_OUTPUT,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(1),
        })
    }

    async fn require_git(
        &self,
        cwd: &Path,
        args: &[&str],
        command: &str,
    ) -> Result<GitOutput, CleanupFail> {
        let result = self.git(cwd, args, SHORT_GIT).await?;
        if result.exit_code != 0 {
            return Err(git_fail(command, &result.stderr));
        }
        Ok(result)
    }

    async fn cleanup_git(
        &self,
        cwd: &Path,
        args: &[&str],
        command: &str,
    ) -> Result<(), CleanupFail> {
        let result = self.git(cwd, args, LONG_GIT).await?;
        if result.exit_code != 0 {
            return Err(git_fail(command, &result.stderr));
        }
        Ok(())
    }

    async fn branch_read(
        &self,
        cwd: &Path,
        args: &[&str],
        allowed_exit: Option<i32>,
    ) -> Result<Option<GitOutput>, CleanupFail> {
        let result = match self.git(cwd, args, SHORT_GIT).await {
            Ok(result) => result,
            Err(error) => {
                if !cwd.exists() {
                    return Ok(None);
                }
                return Err(error);
            }
        };
        if result.exit_code == 0 || allowed_exit == Some(result.exit_code) {
            return Ok(Some(result));
        }
        if result
            .stderr
            .to_lowercase()
            .contains("not a git repository")
        {
            return Ok(None);
        }
        Err(git_fail("git", &result.stderr))
    }

    async fn branch_changed(&self, cwd: &Path, branch: Option<&str>) -> Result<bool, CleanupFail> {
        let Some(index) = self
            .branch_read(cwd, &["rev-parse", "--git-path", "index"], None)
            .await?
        else {
            return Ok(true);
        };
        let index_path = resolve_against(cwd, Path::new(index.stdout.trim()));
        let lock = PathBuf::from(format!("{}.lock", index_path.display()));
        if tokio::fs::try_exists(&lock).await.unwrap_or(false) {
            return Err(CleanupFail::Git {
                command: "git".to_owned(),
                detail: "Git index is locked. Status will resume when the index lock is removed."
                    .to_owned(),
                reason: None,
            });
        }
        let Some(head) = self
            .branch_read(cwd, &["symbolic-ref", "--quiet", "HEAD"], Some(1))
            .await?
        else {
            return Ok(true);
        };
        let name = if head.exit_code == 0 {
            head.stdout
                .trim()
                .strip_prefix("refs/heads/")
                .map(str::trim)
                .filter(|name| !name.starts_with('('))
                .map(str::to_owned)
        } else {
            None
        };
        Ok(name.as_deref() != branch)
    }

    async fn local_changes(
        &self,
        cwd: &Path,
        rules: &WorktreeCleanupRules,
    ) -> Result<Option<String>, CleanupFail> {
        let untracked = if rules.worktree_keep_when == WorktreeKeepWhen::TrackedChanges {
            "--untracked-files=no"
        } else {
            "--untracked-files=all"
        };
        let status = self
            .require_git(
                cwd,
                &[
                    "status",
                    "--porcelain=v1",
                    "-z",
                    "--ignore-submodules=none",
                    untracked,
                ],
                "git",
            )
            .await?;
        if status.stdout_truncated {
            return Ok(Some("local changes exceed the inspection limit".to_owned()));
        }
        let count = porcelain_count(&status.stdout);
        if count > 0 {
            let noun = if count == 1 { "file" } else { "files" };
            return Ok(Some(format!("has uncommitted changes ({count} {noun})")));
        }
        if rules.worktree_keep_when != WorktreeKeepWhen::AnyLocalFiles {
            return Ok(None);
        }
        let ignored = self
            .require_git(
                cwd,
                &[
                    "ls-files",
                    "--others",
                    "--ignored",
                    "--exclude-standard",
                    "--directory",
                    "-z",
                ],
                "git",
            )
            .await?;
        if ignored.stdout_truncated {
            return Ok(Some("ignored files exceed the inspection limit".to_owned()));
        }
        let file = ignored
            .stdout
            .split('\0')
            .find(|entry| !entry.is_empty() && !is_node_modules_dir(entry));
        Ok(file.map(|file| format!("has ignored files ({file})")))
    }

    async fn resolve_commit(&self, cwd: &Path, revision: &str) -> Result<String, CleanupFail> {
        let spec = format!("{revision}^{{commit}}");
        let result = self
            .require_git(cwd, &["rev-parse", "--verify", &spec], "git")
            .await?;
        Ok(result.stdout.trim().to_owned())
    }

    async fn primary_remote(&self, cwd: &Path) -> Result<String, CleanupFail> {
        let result = self.require_git(cwd, &["remote"], "git").await?;
        let remotes: Vec<&str> = result
            .stdout
            .lines()
            .filter(|line| !line.is_empty())
            .collect();
        if remotes.contains(&"origin") {
            return Ok("origin".to_owned());
        }
        remotes
            .first()
            .map(|remote| (*remote).to_owned())
            .ok_or_else(|| CleanupFail::Git {
                command: "git".to_owned(),
                detail: "No git remote is configured for this repository.".to_owned(),
                reason: None,
            })
    }

    async fn default_branch(
        &self,
        cwd: &Path,
        remote: &str,
    ) -> Result<Option<String>, CleanupFail> {
        let spec = format!("refs/remotes/{remote}/HEAD");
        let result = self
            .git(cwd, &["symbolic-ref", "--quiet", &spec], SHORT_GIT)
            .await?;
        if result.exit_code != 0 {
            return Ok(None);
        }
        let prefix = format!("refs/remotes/{remote}/");
        Ok(result
            .stdout
            .trim()
            .strip_prefix(&prefix)
            .map(|branch| branch.trim().to_owned()))
    }

    async fn measure(&self, worktree: &Path) -> Option<u64> {
        match tokio::time::timeout(Duration::from_secs(30), self.measure_walk(worktree)).await {
            Ok(Ok(bytes)) => bytes,
            _ => None,
        }
    }

    async fn measure_walk(&self, worktree: &Path) -> std::io::Result<Option<u64>> {
        let root = tokio::fs::canonicalize(worktree).await?;
        let mut pending = vec![root.clone()];
        let mut entries = 1u64;
        let mut bytes = 0u64;
        while let Some(target) = pending.pop() {
            let meta = tokio::fs::symlink_metadata(&target).await?;
            if meta.file_type().is_symlink() {
                continue;
            }
            let real = tokio::fs::canonicalize(&target).await?;
            if real != target {
                return Ok(None);
            }
            if meta.is_file() {
                bytes += meta.len();
            } else if meta.is_dir() {
                let names = self.host.read_dir(&target).await?;
                entries += names.len() as u64;
                if entries > MAX_MEASURE_ENTRIES {
                    return Ok(None);
                }
                for name in names {
                    let child = target.join(name);
                    if !inside(&root, &child) {
                        return Ok(None);
                    }
                    pending.push(child);
                }
            }
        }
        Ok(Some(bytes))
    }

    async fn contains_project_root(&self, worktree: &Path, projects: &[CleanupProject]) -> bool {
        for project in projects {
            let project_path = resolve_path(&project.workspace_root);
            if project_path == worktree || inside(worktree, &project_path) {
                return true;
            }
            let real = tokio::fs::canonicalize(&project_path)
                .await
                .unwrap_or(project_path);
            if real == worktree || inside(worktree, &real) {
                return true;
            }
        }
        false
    }

    async fn managed_roots(&self, settings: &ServerSettings) -> Vec<PathBuf> {
        let mut directories = vec![self.config.worktrees_dir.clone()];
        let home = dirs::home_dir().unwrap_or_default();
        let extras = std::iter::once(&settings.worktrees_directory)
            .chain(settings.previous_worktrees_directories.iter());
        for setting in extras {
            let setting = setting.as_str();
            if setting.is_empty() {
                continue;
            }
            let expanded = crate::config::expand_home_path(setting, &home);
            if !expanded.is_absolute() {
                continue;
            }
            let resolved = crate::support::symlink::normalize_lexical(&expanded);
            if !is_filesystem_root(&resolved) && !directories.contains(&resolved) {
                directories.push(resolved);
            }
        }
        let mut roots = Vec::new();
        for directory in directories {
            if !tokio::fs::try_exists(&directory).await.unwrap_or(false) {
                continue;
            }
            let Ok(root) = tokio::fs::canonicalize(&directory).await else {
                continue;
            };
            if !is_filesystem_root(&root) {
                roots.push(root);
            }
        }
        roots
    }

    async fn blocked_since_check(
        &self,
        worktree: &Path,
        thread: &OrchestrationV2ThreadShell,
        deleted: bool,
        now: i64,
        activity: i64,
    ) -> Option<String> {
        let projects = self.host.projects();
        if self.contains_project_root(worktree, &projects).await {
            return Some("contains a project checkout".to_owned());
        }
        let latest: Vec<(ThreadId, String)> = self
            .host
            .worktree_paths()
            .into_iter()
            .filter(|(_, path)| resolve_path(path) == worktree)
            .collect();
        if self.host.has_terminal(worktree) {
            return Some("open terminal".to_owned());
        }
        if deleted {
            let rules = resolved_rules(&self.host.settings(), Some(&thread.project_id));
            if !latest.is_empty() || !rules.worktree_on_delete {
                return Some("thread or cleanup settings changed since check".to_owned());
            }
            if self.host.deletion_pending(&thread.id) {
                return Some("thread deletion is still pending".to_owned());
            }
        } else {
            let latest_shell = if latest.len() == 1 && latest[0].0 == thread.id {
                self.host.thread_shell(&thread.id)
            } else {
                None
            };
            let changed = match &latest_shell {
                None => true,
                Some(shell) => {
                    !storage_cleanup_thread_idle(shell, now)
                        || storage_cleanup_activity_at(shell) != activity
                }
            };
            if changed {
                return Some("thread activity or shared worktree changed since check".to_owned());
            }
        }
        if self.host.session_blocks(worktree) {
            Some("provider session is still open".to_owned())
        } else {
            None
        }
    }

    async fn consider(
        &self,
        candidate: WorktreeCandidate<'_>,
        refreshed: &mut HashMap<String, HashSet<String>>,
    ) -> Result<Option<StorageCleanupReportEntry>, CleanupFail> {
        let WorktreeCandidate {
            thread,
            deleted,
            workspace_root,
            shared,
            projects,
            rules,
            now,
            roots,
        } = candidate;
        let Some(raw_path) = thread.worktree_path.as_ref().map(|path| path.as_str()) else {
            return Ok(None);
        };
        let worktree_path = resolve_path(raw_path);
        let worktree_string = path_string(&worktree_path);
        let mut entry = StorageCleanupReportEntry {
            kind: StorageCleanupReportEntryKind::Worktree,
            outcome: StorageCleanupReportEntryOutcome::Kept,
            reason: String::new(),
            path: Some(worktree_string.clone()),
            thread_id: Some(thread.id.clone()),
            thread_title: Some(thread.title.clone()),
            bytes: None,
            files: None,
        };
        if !tokio::fs::try_exists(&worktree_path).await.unwrap_or(false) {
            return Ok(None);
        }
        if deleted && !rules.worktree_on_delete {
            entry.reason = sentence("no rules apply");
            return Ok(Some(entry));
        }
        if shared > 1 {
            entry.reason = sentence(&format!("shared by {shared} threads"));
            return Ok(Some(entry));
        }
        let project_root = if deleted {
            workspace_root.map(str::to_owned)
        } else {
            projects
                .iter()
                .find(|project| project.id == thread.project_id)
                .map(|project| project.workspace_root.clone())
        };
        let Some(project_root) = project_root else {
            entry.reason = sentence("project is unavailable");
            return Ok(Some(entry));
        };
        if !deleted && !storage_cleanup_thread_idle(thread, now) {
            entry.reason = sentence("thread is running or has pending work");
            return Ok(Some(entry));
        }
        if self.host.has_terminal(&worktree_path) {
            entry.reason = sentence("open terminal");
            return Ok(Some(entry));
        }
        let real_path = tokio::fs::canonicalize(&worktree_path)
            .await
            .map_err(|error| CleanupFail::Other(error.to_string()))?;
        let parent = worktree_path.parent().unwrap_or(Path::new("/"));
        let real_parent = tokio::fs::canonicalize(parent)
            .await
            .map_err(|error| CleanupFail::Other(error.to_string()))?;
        let joined = real_parent.join(worktree_path.file_name().unwrap_or_default());
        if real_path != joined {
            entry.reason = sentence("worktree is a symbolic link");
            return Ok(Some(entry));
        }
        if !roots.iter().any(|root| inside(root, &real_path)) {
            entry.reason = sentence("outside the managed worktree folder");
            return Ok(Some(entry));
        }
        let mut project_rows = projects.to_vec();
        if deleted
            && !project_rows
                .iter()
                .any(|project| project.workspace_root == project_root)
        {
            project_rows.push(CleanupProject {
                id: thread.project_id.clone(),
                workspace_root: project_root.clone(),
            });
        }
        if self
            .contains_project_root(&worktree_path, &project_rows)
            .await
        {
            entry.reason = sentence("contains a project checkout");
            return Ok(Some(entry));
        }
        let git_meta = tokio::fs::symlink_metadata(worktree_path.join(".git"))
            .await
            .map_err(|error| CleanupFail::Other(error.to_string()))?;
        if !git_meta.is_file() {
            entry.reason = sentence("not a linked worktree");
            return Ok(Some(entry));
        }
        let branch = thread.branch.as_ref().map(|branch| branch.as_str());
        if self.branch_changed(&worktree_path, branch).await? {
            entry.reason = sentence("repository or branch changed");
            return Ok(Some(entry));
        }
        if let Some(changes) = self.local_changes(&worktree_path, rules).await? {
            entry.reason = sentence(&changes);
            return Ok(Some(entry));
        }
        let head_sha = self.resolve_commit(&worktree_path, "HEAD").await?;
        let activity = storage_cleanup_activity_at(thread);
        let after_days = rules.worktree_after_days.map(|days| days.get());
        let old = !deleted && after_days.is_some_and(|days| activity < now - days * DAY_MS);
        let mut eligible = deleted || old;
        let inactive_days = floor_div(now - activity, DAY_MS);
        let mut removal_reason = if deleted {
            "thread was deleted".to_owned()
        } else {
            format!("inactive for {inactive_days} days")
        };
        if !eligible && (rules.worktree_unchanged || rules.worktree_on_merge) {
            let repository = resolve_path(&project_root);
            let remote = self.primary_remote(&repository).await?;
            let Some(default_branch) = self.default_branch(&repository, &remote).await? else {
                entry.reason = sentence("default branch is unavailable");
                return Ok(Some(entry));
            };
            let default_ref = format!("refs/remotes/{remote}/{default_branch}");
            let refreshed = refreshed.entry(path_string(&repository)).or_default();
            if refreshed.insert(default_ref.clone()) {
                let refspec =
                    format!("+refs/heads/{default_branch}:refs/remotes/{remote}/{default_branch}");
                let fetched = self
                    .git(
                        &repository,
                        &["fetch", "--quiet", "--no-tags", &remote, &refspec],
                        SHORT_GIT,
                    )
                    .await?;
                if fetched.exit_code != 0 {
                    return Err(git_fail("git", &fetched.stderr));
                }
            }
            let base_sha = self.resolve_commit(&worktree_path, &default_ref).await?;
            let ancestor = self
                .git(
                    &worktree_path,
                    &["merge-base", "--is-ancestor", &head_sha, &base_sha],
                    SHORT_GIT,
                )
                .await?;
            let integrated = ancestor.exit_code == 0;
            eligible = integrated && rules.worktree_unchanged;
            if eligible {
                removal_reason = "no commits beyond the default branch".to_owned();
            }
            if !eligible && rules.worktree_on_merge && branch.is_some() {
                let stored = self.host.stored_pull_request();
                let pull_request = stored.as_ref().map(|pull_request| CleanupPullRequest {
                    state: &pull_request.state,
                    head_ref: &pull_request.head_ref,
                    base_ref: &pull_request.base_ref,
                    head_sha: pull_request.head_sha.as_deref(),
                });
                eligible = storage_cleanup_pull_request_merged(
                    pull_request.as_ref(),
                    CleanupMergeWorktree {
                        branch: branch.unwrap_or(""),
                        default_branch: &default_branch,
                        head_sha: &head_sha,
                        integrated,
                    },
                );
                if eligible {
                    removal_reason = "pull request was merged".to_owned();
                }
            }
        }
        if !eligible {
            entry.reason = sentence(&if !deleted && after_days.is_some() {
                format!(
                    "inactive for {inactive_days} of {} days",
                    after_days.unwrap_or(0)
                )
            } else if rules.worktree_on_merge {
                "not merged".to_owned()
            } else {
                "has commits beyond the default branch".to_owned()
            });
            return Ok(Some(entry));
        }
        if let Some(blocked) = self
            .blocked_since_check(&worktree_path, thread, deleted, now, activity)
            .await
        {
            entry.reason = sentence(&blocked);
            return Ok(Some(entry));
        }
        let bytes = self.measure(&worktree_path).await;
        if let Some(blocked) = self
            .blocked_since_check(&worktree_path, thread, deleted, now, activity)
            .await
        {
            entry.reason = sentence(&blocked);
            return Ok(Some(entry));
        }
        if self.branch_changed(&worktree_path, branch).await? {
            entry.reason = sentence("repository or branch changed since check");
            return Ok(Some(entry));
        }
        if self.resolve_commit(&worktree_path, "HEAD").await? != head_sha {
            entry.reason = sentence("commit changed since check");
            return Ok(Some(entry));
        }
        if let Some(changes) = self.local_changes(&worktree_path, rules).await? {
            entry.reason = sentence(&changes);
            return Ok(Some(entry));
        }
        let current = resolved_rules(&self.host.settings(), Some(&thread.project_id));
        if &current != rules {
            entry.reason = sentence("settings changed since check");
            return Ok(Some(entry));
        }
        if rules.worktree_keep_when == WorktreeKeepWhen::TrackedChanges {
            self.cleanup_git(&worktree_path, &["clean", "-ffdx"], "git clean")
                .await?;
        }
        let repository = resolve_path(&project_root);
        self.cleanup_git(
            &repository,
            &[
                "-c",
                "status.showUntrackedFiles=normal",
                "worktree",
                "remove",
                &worktree_string,
            ],
            "git worktree remove",
        )
        .await?;
        self.host.invalidate_status();
        tracing::info!(thread_id = %thread.id, "storage cleanup removed worktree");
        entry.outcome = StorageCleanupReportEntryOutcome::Removed;
        entry.reason = sentence(&removal_reason);
        entry.bytes = bytes.map(|bytes| bytes as f64);
        Ok(Some(entry))
    }

    async fn clean_worktrees(
        &self,
        settings: &ServerSettings,
        now: i64,
    ) -> Vec<StorageCleanupReportEntry> {
        if let Some(reason) = &self.worktree_cleanup_unavailable {
            return if any_worktree_policy(settings, worktree_cleanup_enabled) {
                vec![StorageCleanupReportEntry {
                    kind: StorageCleanupReportEntryKind::Worktree,
                    outcome: StorageCleanupReportEntryOutcome::Kept,
                    reason: reason.clone(),
                    path: Some(path_string(&self.config.worktrees_dir)),
                    thread_id: None,
                    thread_title: None,
                    bytes: None,
                    files: None,
                }]
            } else {
                Vec::new()
            };
        }
        let mut entries = Vec::new();
        let roots = self.managed_roots(settings).await;
        let projects = self.host.projects();
        let deleted = self.host.deleted_threads();
        let threads = self.host.threads();
        self.host.note_snapshot();
        let mut groups: HashMap<PathBuf, Vec<OrchestrationV2ThreadShell>> = HashMap::new();
        for thread in threads {
            let Some(path) = thread.worktree_path.as_ref() else {
                continue;
            };
            groups
                .entry(resolve_path(path.as_str()))
                .or_default()
                .push(thread);
        }
        let mut candidates: Vec<(OrchestrationV2ThreadShell, bool, Option<String>, usize)> = groups
            .values()
            .filter_map(|group| {
                group
                    .first()
                    .map(|thread| (thread.clone(), false, None, group.len()))
            })
            .collect();
        for thread in &deleted {
            let Some(path) = thread.shell.worktree_path.as_ref() else {
                continue;
            };
            if thread.shell.branch.is_none() {
                continue;
            }
            let path = resolve_path(path.as_str());
            if groups.contains_key(&path) {
                continue;
            }
            candidates.push((
                thread.shell.clone(),
                true,
                Some(thread.workspace_root.clone()),
                0,
            ));
        }
        let mut refreshed = HashMap::new();
        for (thread, deleted, workspace, shared) in candidates {
            let rules = resolved_rules(settings, Some(&thread.project_id));
            if !worktree_cleanup_enabled(&rules) {
                continue;
            }
            match self
                .consider(
                    WorktreeCandidate {
                        thread: &thread,
                        deleted,
                        workspace_root: workspace.as_deref(),
                        shared,
                        projects: &projects,
                        rules: &rules,
                        now,
                        roots: &roots,
                    },
                    &mut refreshed,
                )
                .await
            {
                Ok(Some(entry)) => entries.push(entry),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(thread_id = %thread.id, reason = %failure_reason(&error), "storage cleanup failed for worktree");
                    let path = thread
                        .worktree_path
                        .as_ref()
                        .map(|path| path_string(&resolve_path(path.as_str())));
                    entries.push(StorageCleanupReportEntry {
                        kind: StorageCleanupReportEntryKind::Worktree,
                        outcome: StorageCleanupReportEntryOutcome::Failed,
                        reason: failure_reason(&error),
                        path,
                        thread_id: Some(thread.id.clone()),
                        thread_title: Some(thread.title.clone()),
                        bytes: None,
                        files: None,
                    });
                }
            }
        }
        entries
    }

    async fn clean_files(
        &self,
        root: &Path,
        days: i64,
        now: i64,
        rotated_logs: bool,
    ) -> Result<FileRemoved, (CleanupFail, FileRemoved)> {
        let mut removed = FileRemoved::default();
        if !tokio::fs::try_exists(root).await.unwrap_or(false) {
            return Ok(removed);
        }
        let real_root = match tokio::fs::canonicalize(root).await {
            Ok(path) => path,
            Err(error) => return Err((CleanupFail::Other(error.to_string()), removed)),
        };
        // A symlinked directory is not the folder the setting names.
        if real_root != resolve_path(&path_string(root)) {
            return Ok(removed);
        }
        if let Err(error) = self
            .visit_files(
                &real_root,
                &real_root,
                days,
                now,
                rotated_logs,
                &mut removed,
            )
            .await
        {
            return Err((error, removed));
        }
        Ok(removed)
    }

    async fn visit_files(
        &self,
        real_root: &Path,
        directory: &Path,
        days: i64,
        now: i64,
        rotated_logs: bool,
        removed: &mut FileRemoved,
    ) -> Result<(), CleanupFail> {
        let names = self
            .host
            .read_dir(directory)
            .await
            .map_err(|error| CleanupFail::Other(error.to_string()))?;
        for name in names {
            let target = directory.join(&name);
            let meta = match tokio::fs::symlink_metadata(&target).await {
                Ok(meta) => meta,
                Err(error) => return Err(CleanupFail::Other(error.to_string())),
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            let real = match tokio::fs::canonicalize(&target).await {
                Ok(path) => path,
                Err(error) => return Err(CleanupFail::Other(error.to_string())),
            };
            if real != target || !inside(real_root, &target) {
                continue;
            }
            if meta.is_dir() && rotated_logs {
                Box::pin(self.visit_files(real_root, &target, days, now, rotated_logs, removed))
                    .await?;
            } else if meta.is_file() && (!rotated_logs || is_rotated_log(&name)) {
                let Some(modified) = meta.modified().ok().and_then(system_millis) else {
                    continue;
                };
                if modified >= now - days * DAY_MS {
                    continue;
                }
                let current = self.host.settings().storage_cleanup;
                let current_days = if rotated_logs {
                    current.logs_after_days
                } else {
                    current.browser_artifacts_after_days
                };
                if current_days.map(|days| days.get()) != Some(days) {
                    return Ok(());
                }
                tokio::fs::remove_file(&target)
                    .await
                    .map_err(|error| CleanupFail::Other(error.to_string()))?;
                removed.files += 1;
                removed.bytes += meta.len();
            }
        }
        Ok(())
    }

    async fn sweep(&self, trigger: StorageCleanupReportTrigger) -> StorageCleanupReport {
        let started = now_millis();
        let settings = self.host.settings();
        let mut entries = self.clean_worktrees(&settings, started).await;
        let storage = settings.storage_cleanup;
        for category in [
            FileJob {
                kind: StorageCleanupReportEntryKind::BrowserArtifacts,
                root: self.config.browser_artifacts_dir.clone(),
                days: storage.browser_artifacts_after_days.map(|days| days.get()),
                label: "browser artifacts",
                rotated_logs: false,
            },
            FileJob {
                kind: StorageCleanupReportEntryKind::Logs,
                root: self.config.logs_dir.clone(),
                days: storage.logs_after_days.map(|days| days.get()),
                label: "rotated logs",
                rotated_logs: true,
            },
        ] {
            let Some(days) = category.days else {
                continue;
            };
            match self
                .clean_files(&category.root, days, started, category.rotated_logs)
                .await
            {
                Ok(removed) => {
                    entries.push(file_entry(category.kind, category.label, &removed, false))
                }
                Err((error, removed)) => {
                    tracing::warn!(kind = category.label, reason = %failure_reason(&error), "storage file cleanup failed");
                    entries.push(file_entry(category.kind, category.label, &removed, true));
                    entries.last_mut().expect("file entry").reason = failure_reason(&error);
                    entries.last_mut().expect("file entry").outcome =
                        StorageCleanupReportEntryOutcome::Failed;
                }
            }
        }
        let report = publish_report(trigger, started, now_millis(), entries);
        let _ = self.reports.send_replace(Some(report.clone()));
        report
    }
}

#[derive(Default)]
struct FileRemoved {
    files: u64,
    bytes: u64,
}

struct FileJob {
    kind: StorageCleanupReportEntryKind,
    root: PathBuf,
    days: Option<i64>,
    label: &'static str,
    rotated_logs: bool,
}

fn file_entry(
    kind: StorageCleanupReportEntryKind,
    label: &str,
    removed: &FileRemoved,
    failed: bool,
) -> StorageCleanupReportEntry {
    let removed_any = removed.files > 0;
    StorageCleanupReportEntry {
        kind,
        outcome: if failed {
            StorageCleanupReportEntryOutcome::Failed
        } else if removed_any {
            StorageCleanupReportEntryOutcome::Removed
        } else {
            StorageCleanupReportEntryOutcome::Kept
        },
        reason: if failed {
            String::new()
        } else if removed_any {
            format!("Removed {} {label}", removed.files)
        } else {
            "No expired files".to_owned()
        },
        path: None,
        thread_id: None,
        thread_title: None,
        bytes: removed_any.then_some(removed.bytes as f64),
        files: removed_any.then_some(removed.files as f64),
    }
}

fn publish_report(
    trigger: StorageCleanupReportTrigger,
    started: i64,
    finished: i64,
    mut entries: Vec<StorageCleanupReportEntry>,
) -> StorageCleanupReport {
    let mut counts = turin_contracts::settings::StorageCleanupCounts {
        removed: 0.0,
        kept: 0.0,
        failed: 0.0,
    };
    let mut bytes_freed = 0.0;
    for entry in &entries {
        match entry.outcome {
            StorageCleanupReportEntryOutcome::Removed => counts.removed += 1.0,
            StorageCleanupReportEntryOutcome::Kept => counts.kept += 1.0,
            StorageCleanupReportEntryOutcome::Failed => counts.failed += 1.0,
        }
        bytes_freed += entry.bytes.unwrap_or(0.0);
    }
    let omitted = entries.len().saturating_sub(REPORT_ENTRY_LIMIT) as f64;
    entries.sort_by_key(|entry| match entry.outcome {
        StorageCleanupReportEntryOutcome::Failed => 0,
        StorageCleanupReportEntryOutcome::Removed => 1,
        StorageCleanupReportEntryOutcome::Kept => 2,
    });
    entries.truncate(REPORT_ENTRY_LIMIT);
    StorageCleanupReport {
        trigger,
        started_at: iso_millis(started),
        finished_at: iso_millis(finished),
        entries,
        counts,
        bytes_freed,
        omitted_count: omitted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, SecondsFormat, Utc};
    use serde_json::{Value, json};
    use std::process::Command;
    use turin_contracts::settings::{
        StorageCleanupCounts, StorageCleanupReport, StorageRetentionDays, default_server_settings,
    };

    const NOW: &str = "2026-06-10T12:00:00.000Z";

    fn now_ms() -> i64 {
        DateTime::parse_from_rfc3339(NOW)
            .unwrap()
            .timestamp_millis()
    }

    fn at(offset_ms: i64) -> String {
        let base = DateTime::parse_from_rfc3339(NOW)
            .unwrap()
            .with_timezone(&Utc);
        (base + chrono::Duration::milliseconds(offset_ms))
            .to_rfc3339_opts(SecondsFormat::Millis, true)
    }

    fn shell(overrides: Value) -> OrchestrationV2ThreadShell {
        let created = at(-30 * DAY_MS);
        let updated = at(-10 * DAY_MS);
        let mut value = json!({
            "id": "thread-1",
            "projectId": "project-1",
            "title": "Thread",
            "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "gpt-5.4"},
            "runtimeMode": "full-access",
            "interactionMode": "default",
            "worktreePath": null,
            "activeProviderThreadId": null,
            "lineage": {"rootThreadId": "thread-1", "parentThreadId": null, "relationshipToParent": null},
            "forkedFrom": null,
            "createdBy": "user",
            "creationSource": "web",
            "activeRunId": null,
            "latestVisibleMessage": null,
            "hasActionableProposedPlan": false,
            "itemCount": 0,
            "visibleItemCount": 0,
            "branch": null,
            "status": "idle",
            "pendingRuntimeRequest": null,
            "pendingBackgroundTasks": [],
            "latestRunId": null,
            "latestUserMessageAt": null,
            "createdAt": created,
            "updatedAt": updated,
            "archivedAt": null,
            "settledOverride": null,
            "settledAt": null,
            "deletedAt": null,
        });
        if let Some(map) = overrides.as_object() {
            for (key, field) in map {
                value[key] = field.clone();
            }
        }
        serde_json::from_value(value).expect("shell fixture")
    }

    fn candidate() -> OrchestrationV2ThreadShell {
        shell(json!({"branch": "feature", "worktreePath": "/worktrees/feature"}))
    }

    fn with_status(status: &str) -> OrchestrationV2ThreadShell {
        shell(json!({"branch": "feature", "worktreePath": "/worktrees/feature", "status": status}))
    }

    #[test]
    fn allows_an_idle_worktree_and_rejects_the_project_checkout() {
        assert!(storage_cleanup_thread_idle(&candidate(), now_ms()));
        assert!(!storage_cleanup_thread_idle(&shell(json!({})), now_ms()));
    }

    #[test]
    fn retains_a_worktree_while_its_thread_is_running() {
        for status in ["running", "starting", "preparing", "waiting", "queued"] {
            assert!(
                !storage_cleanup_thread_idle(&with_status(status), now_ms()),
                "{status}"
            );
        }
    }

    #[test]
    fn allows_cleanup_once_its_thread_is_settled() {
        for status in [
            "idle",
            "completed",
            "interrupted",
            "failed",
            "cancelled",
            "rolled_back",
        ] {
            assert!(
                storage_cleanup_thread_idle(&with_status(status), now_ms()),
                "{status}"
            );
        }
    }

    #[test]
    fn retains_settled_threads_while_background_work_is_pending() {
        for status in ["completed", "interrupted", "cancelled", "rolled_back"] {
            let thread = shell(json!({
                "branch": "feature",
                "worktreePath": "/worktrees/feature",
                "status": status,
                "pendingBackgroundTasks": [{"taskId": "task-1", "kind": "command"}],
            }));
            assert!(!storage_cleanup_thread_idle(&thread, now_ms()), "{status}");
        }
    }

    #[test]
    fn retains_settled_threads_while_a_runtime_request_is_pending() {
        for status in ["completed", "interrupted", "cancelled", "rolled_back"] {
            let thread = shell(json!({
                "branch": "feature",
                "worktreePath": "/worktrees/feature",
                "status": status,
                "pendingRuntimeRequest": {"id": "request-1", "kind": "command", "createdAt": at(0)},
            }));
            assert!(!storage_cleanup_thread_idle(&thread, now_ms()), "{status}");
        }
    }

    #[test]
    fn retains_an_active_run_even_if_the_shell_status_is_idle() {
        let thread = shell(json!({
            "branch": "feature",
            "worktreePath": "/worktrees/feature",
            "activeRunId": "run",
        }));
        assert!(!storage_cleanup_thread_idle(&thread, now_ms()));
    }

    #[test]
    fn retains_a_queued_prompt_before_the_new_run_has_been_projected() {
        let thread = shell(json!({
            "branch": "feature",
            "worktreePath": "/worktrees/feature",
            "latestUserMessageAt": at(-1_000),
        }));
        assert!(!storage_cleanup_thread_idle(&thread, now_ms()));
    }

    #[test]
    fn uses_run_activity_instead_of_metadata_refreshes() {
        let run_time = at(-3 * DAY_MS);
        let thread = shell(json!({
            "branch": "feature",
            "worktreePath": "/worktrees/feature",
            "latestRunCompletedAt": run_time,
            "updatedAt": at(0),
        }));
        let expected = DateTime::parse_from_rfc3339(&run_time)
            .unwrap()
            .timestamp_millis();
        assert_eq!(storage_cleanup_activity_at(&thread), expected);
    }

    fn pull<'a>(
        head_sha: Option<&'a str>,
        base_ref: &'a str,
        head_ref: &'a str,
        state: &'a str,
    ) -> CleanupPullRequest<'a> {
        CleanupPullRequest {
            state,
            head_ref,
            base_ref,
            head_sha,
        }
    }

    #[test]
    fn removes_a_worktree_whose_head_reached_the_default_branch() {
        let integrated = CleanupMergeWorktree {
            branch: "feature",
            default_branch: "main",
            head_sha: &"a".repeat(40),
            integrated: true,
        };
        let request = pull(None, "main", "feature", "merged");
        assert!(storage_cleanup_pull_request_merged(
            Some(&request),
            integrated
        ));
    }

    #[test]
    fn removes_a_squash_merged_worktree_when_the_pull_request_names_its_head() {
        let head = "a".repeat(40);
        let squashed = CleanupMergeWorktree {
            branch: "feature",
            default_branch: "main",
            head_sha: &head,
            integrated: false,
        };
        let request = pull(Some(&head), "main", "feature", "merged");
        assert!(storage_cleanup_pull_request_merged(
            Some(&request),
            squashed
        ));
    }

    #[test]
    fn keeps_a_squash_worktree_whose_pull_request_does_not_prove_this_head() {
        let head = "a".repeat(40);
        let squashed = CleanupMergeWorktree {
            branch: "feature",
            default_branch: "main",
            head_sha: &head,
            integrated: false,
        };
        let later = "c".repeat(40);
        let cases = [
            pull(Some(&later), "main", "feature", "merged"),
            pull(Some(&head), "release", "feature", "merged"),
            pull(Some(&head), "stack-parent", "feature", "merged"),
            pull(None, "main", "feature", "merged"),
            pull(Some(&head), "main", "other", "merged"),
            pull(Some(&head), "main", "feature", "open"),
            pull(Some(&head), "main", "feature", "closed"),
        ];
        for request in cases {
            assert!(!storage_cleanup_pull_request_merged(
                Some(&request),
                squashed
            ));
        }
    }

    #[test]
    fn keeps_a_worktree_with_no_pull_request_or_one_that_is_not_merged() {
        let head = "a".repeat(40);
        let integrated = CleanupMergeWorktree {
            branch: "feature",
            default_branch: "main",
            head_sha: &head,
            integrated: true,
        };
        let squashed = CleanupMergeWorktree {
            integrated: false,
            ..integrated
        };
        assert!(!storage_cleanup_pull_request_merged(None, squashed));
        assert!(!storage_cleanup_pull_request_merged(None, integrated));
        let open = pull(Some(&head), "main", "feature", "open");
        assert!(!storage_cleanup_pull_request_merged(
            Some(&open),
            integrated
        ));
    }

    struct Fixture {
        service: StorageCleanup,
        _base: tempfile::TempDir,
        worktree: PathBuf,
        config: ServerConfig,
    }

    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_stdout(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    async fn fixture() -> Fixture {
        let base = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(base.path()).unwrap();
        let config = ServerConfig::for_test(&root, &root).await.unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "cleanup@example.test"]);
        git(&repo, &["config", "user.name", "Cleanup test"]);
        std::fs::write(repo.join("tracked.txt"), "original\n").unwrap();
        std::fs::write(repo.join(".gitignore"), ".env\ndist/\nnode_modules/\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "initial"]);
        let worktree = config.worktrees_dir.join("feature");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                worktree.to_str().unwrap(),
            ],
        );
        let thread = shell(json!({
            "branch": "feature",
            "worktreePath": worktree.to_str().unwrap(),
        }));
        let mut settings = default_server_settings();
        settings.storage_cleanup.worktree_after_days = Some(StorageRetentionDays::new(8).unwrap());
        settings.storage_cleanup.worktree_on_delete = true;
        let host = CleanupHost::new(
            settings,
            vec![CleanupProject {
                id: thread.project_id.clone(),
                workspace_root: path_string(&repo),
            }],
            vec![thread],
        );
        let service = StorageCleanup::new(config.clone(), host);
        Fixture {
            service,
            _base: base,
            worktree,
            config,
        }
    }

    fn listed_bytes(dir: &Path) -> u64 {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum()
    }

    fn worktree_entry(report: &StorageCleanupReport) -> &StorageCleanupReportEntry {
        report
            .entries
            .iter()
            .find(|entry| entry.kind == StorageCleanupReportEntryKind::Worktree)
            .expect("worktree entry")
    }

    #[tokio::test]
    async fn keeps_a_worktree_when_the_remote_default_branch_is_rewound() {
        let base = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(base.path()).unwrap();
        let config = ServerConfig::for_test(&root, &root).await.unwrap();
        let origin = root.join("origin.git");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "cleanup@example.test"]);
        git(&repo, &["config", "user.name", "Cleanup test"]);
        std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        git(&repo, &["add", "tracked.txt"]);
        git(&repo, &["commit", "-m", "base"]);
        let old = git_stdout(&repo, &["rev-parse", "HEAD"]);
        let old = old.trim();

        git(
            &root,
            &["init", "--bare", "-b", "main", origin.to_str().unwrap()],
        );
        git(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        // A plain `git fetch origin main` exits 0 without moving
        // refs/remotes/origin/main when no fetch refspec is configured.
        git(&repo, &["config", "--unset-all", "remote.origin.fetch"]);
        git(&repo, &["push", "origin", "main"]);

        std::fs::write(repo.join("tracked.txt"), "unique\n").unwrap();
        git(&repo, &["commit", "-am", "unique"]);
        let unique = git_stdout(&repo, &["rev-parse", "HEAD"]);
        let unique = unique.trim();
        git(&repo, &["push", "origin", "main"]);
        git(&origin, &["update-ref", "refs/heads/main", old]);
        git(&repo, &["update-ref", "refs/remotes/origin/main", unique]);
        git(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );

        let worktree = config.worktrees_dir.join("feature");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                worktree.to_str().unwrap(),
                unique,
            ],
        );
        let thread = shell(json!({
            "branch": "feature",
            "worktreePath": worktree.to_str().unwrap(),
        }));
        let mut settings = default_server_settings();
        settings.storage_cleanup.worktree_after_days = None;
        settings.storage_cleanup.worktree_on_delete = false;
        settings.storage_cleanup.worktree_on_merge = false;
        settings.storage_cleanup.worktree_unchanged = true;
        let host = CleanupHost::new(
            settings,
            vec![CleanupProject {
                id: thread.project_id.clone(),
                workspace_root: path_string(&repo),
            }],
            vec![thread],
        );
        let service = StorageCleanup::new(config, host);
        let report = service.run_now().await;
        let entry = worktree_entry(&report);
        assert_eq!(entry.outcome, StorageCleanupReportEntryOutcome::Kept);
        assert_eq!(entry.reason, "Has commits beyond the default branch");
        assert!(worktree.exists());
        assert_eq!(git_stdout(&worktree, &["rev-parse", "HEAD"]).trim(), unique);
        assert_eq!(
            git_stdout(&repo, &["rev-parse", "refs/remotes/origin/main"]).trim(),
            old
        );
    }

    #[tokio::test]
    async fn measures_nested_regular_files_without_following_symlinks() {
        let fx = fixture().await;
        fx.service.host().update_storage(|settings| {
            settings.worktree_keep_when = WorktreeKeepWhen::TrackedChanges;
        });
        let sizes = listed_bytes(&fx.worktree);
        let outside = tempfile::tempdir().unwrap();
        let outside_root = std::fs::canonicalize(outside.path()).unwrap();
        std::fs::write(outside_root.join("external"), "must not count").unwrap();
        std::fs::create_dir_all(fx.worktree.join("node_modules/nested")).unwrap();
        std::fs::write(fx.worktree.join("node_modules/nested/file"), "count me").unwrap();
        std::os::unix::fs::symlink(
            &outside_root,
            fx.worktree.join("node_modules/directory-link"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            outside_root.join("external"),
            fx.worktree.join("node_modules/file-link"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            outside_root.join("missing"),
            fx.worktree.join("node_modules/dangling-link"),
        )
        .unwrap();
        let report = fx.service.run_now().await;
        let bytes = sizes + 8;
        let entry = worktree_entry(&report);
        assert_eq!(entry.outcome, StorageCleanupReportEntryOutcome::Removed);
        assert_eq!(entry.bytes, Some(bytes as f64));
        assert_eq!(entry.files, None);
        assert_eq!(report.bytes_freed, bytes as f64);
        assert_eq!(
            std::fs::read_to_string(outside_root.join("external")).unwrap(),
            "must not count"
        );
    }

    #[tokio::test]
    async fn still_removes_a_worktree_when_measuring_its_size_fails() {
        let fx = fixture().await;
        fx.service.host().fail_read_dir();
        let report = fx.service.run_now().await;
        let entry = worktree_entry(&report);
        assert_eq!(entry.outcome, StorageCleanupReportEntryOutcome::Removed);
        assert_eq!(entry.bytes, None);
        assert_eq!(entry.files, None);
        assert_eq!(report.bytes_freed, 0.0);
        assert!(!fx.worktree.exists());
    }

    async fn keeps_when_threads_change_during_measure(share: bool) {
        let fx = fixture().await;
        fx.service.host().update_storage(|settings| {
            settings.worktree_keep_when = WorktreeKeepWhen::TrackedChanges;
        });
        fx.service
            .host()
            .set_pause_at(std::fs::canonicalize(&fx.worktree).unwrap());
        std::fs::write(fx.worktree.join("notes.txt"), "untracked\n").unwrap();
        let path = path_string(&fx.worktree);
        let threads = if share {
            let shared = path.clone();
            vec![
                shell(json!({"branch": "feature", "worktreePath": path})),
                shell(json!({"id": "thread-2", "branch": "feature", "worktreePath": shared})),
            ]
        } else {
            vec![shell(json!({
                "branch": "feature",
                "worktreePath": path,
                "status": "running",
            }))]
        };
        let service = fx.service.clone();
        let cleanup = tokio::spawn(async move { service.run_now().await });
        fx.service.host().wait_measuring().await;
        fx.service.host().set_threads(threads);
        fx.service.host().resume();
        let report = cleanup.await.unwrap();
        let entry = worktree_entry(&report);
        assert_eq!(entry.outcome, StorageCleanupReportEntryOutcome::Kept);
        assert_eq!(
            entry.reason,
            "Thread activity or shared worktree changed since check"
        );
        assert_eq!(entry.bytes, None);
        assert_eq!(report.bytes_freed, 0.0);
        assert!(fx.worktree.exists());
        assert_eq!(
            std::fs::read_to_string(fx.worktree.join("notes.txt")).unwrap(),
            "untracked\n"
        );
    }

    #[tokio::test]
    async fn keeps_a_worktree_when_its_thread_starts_during_size_measurement() {
        keeps_when_threads_change_during_measure(false).await;
    }

    #[tokio::test]
    async fn keeps_a_worktree_when_another_thread_starts_using_it_during_measurement() {
        keeps_when_threads_change_during_measure(true).await;
    }

    #[tokio::test]
    async fn reads_the_thread_snapshot_once_per_sweep() {
        let fx = fixture().await;
        let report = fx.service.run_now().await;
        assert_eq!(
            worktree_entry(&report).outcome,
            StorageCleanupReportEntryOutcome::Removed
        );
        assert_eq!(fx.service.host().snapshot_reads(), 1);
    }

    async fn session_updates_queue_sweeps(first: &'static str, second: &'static str, sweeps: u64) {
        let fx = fixture().await;
        fx.service
            .host()
            .set_pause_at(std::fs::canonicalize(&fx.worktree).unwrap());
        let waiter = fx.service.clone();
        fx.service.activate();
        let (done_tx, done_rx) = oneshot::channel();
        tokio::spawn(async move {
            waiter.host().wait_measuring().await;
            for index in 0..20 {
                let status = if index % 2 == 0 { first } else { second };
                waiter.handle_event(&CleanupDomainEvent::ProviderSessionUpdated {
                    status: status.to_owned(),
                });
            }
            let _ = done_tx.send(());
        });
        done_rx.await.unwrap();
        fx.service.host().resume();
        fx.service.drain().await;
        assert_eq!(fx.service.host().snapshot_reads(), sweeps);
    }

    #[tokio::test]
    async fn running_or_ready_session_updates_do_not_queue_another_sweep() {
        session_updates_queue_sweeps("running", "ready", 1).await;
    }

    #[tokio::test]
    async fn stopped_or_error_session_updates_queue_one_extra_sweep() {
        session_updates_queue_sweeps("stopped", "error", 2).await;
    }

    #[tokio::test]
    async fn does_not_measure_a_worktree_kept_for_an_open_provider_session() {
        let fx = fixture().await;
        fx.service.host().record_walks();
        let root = path_string(&std::fs::canonicalize(&fx.worktree).unwrap());
        fx.service
            .host()
            .set_open_session_cwd(Some(path_string(&fx.worktree)));
        let kept = fx.service.run_now().await;
        let entry = worktree_entry(&kept);
        assert_eq!(entry.outcome, StorageCleanupReportEntryOutcome::Kept);
        assert_eq!(entry.reason, "Provider session is still open");
        assert_eq!(entry.bytes, None);
        assert!(
            fx.service
                .host()
                .walked()
                .iter()
                .all(|dir| !dir.starts_with(&root))
        );
        fx.service.host().set_open_session_cwd(None);
        let removed = fx.service.run_now().await;
        let entry = worktree_entry(&removed);
        assert_eq!(entry.outcome, StorageCleanupReportEntryOutcome::Removed);
        assert!(entry.bytes.unwrap_or(0.0) > 0.0);
        assert!(fx.service.host().walked().iter().any(|dir| dir == &root));
    }

    #[tokio::test]
    async fn compares_the_checked_out_branch_without_a_full_status() {
        let fx = fixture().await;
        let changed = "Repository or branch changed";
        git(&fx.worktree, &["checkout", "--detach"]);
        assert_eq!(worktree_entry(&fx.service.run_now().await).reason, changed);
        git(&fx.worktree, &["switch", "--orphan", "other"]);
        assert_eq!(worktree_entry(&fx.service.run_now().await).reason, changed);
        git(&fx.worktree, &["switch", "-c", "(feature)", "feature"]);
        fx.service.host().set_threads(vec![shell(json!({
            "branch": "(feature)",
            "worktreePath": path_string(&fx.worktree),
        }))]);
        assert_eq!(worktree_entry(&fx.service.run_now().await).reason, changed);
        fx.service.host().set_threads(vec![shell(json!({
            "branch": "feature",
            "worktreePath": path_string(&fx.worktree),
        }))]);
        git(&fx.worktree, &["tag", "feature", "feature"]);
        git(
            &fx.worktree,
            &["update-ref", "refs/tags/HEAD", "refs/heads/feature"],
        );
        git(&fx.worktree, &["switch", "feature"]);
        let index = git_stdout(&fx.worktree, &["rev-parse", "--git-path", "index"]);
        let lock = format!("{}.lock", index.trim());
        std::fs::write(&lock, "").unwrap();
        let locked = worktree_entry(&fx.service.run_now().await).clone();
        assert_eq!(locked.outcome, StorageCleanupReportEntryOutcome::Failed);
        assert!(
            locked.reason.contains("Git index is locked"),
            "{}",
            locked.reason
        );
        std::fs::remove_file(&lock).unwrap();
        let git_file = fx.worktree.join(".git");
        let original = std::fs::read_to_string(&git_file).unwrap();
        std::fs::write(&git_file, "gitdir: /missing-git-directory\n").unwrap();
        assert_eq!(worktree_entry(&fx.service.run_now().await).reason, changed);
        std::fs::write(&git_file, original).unwrap();
        assert_eq!(
            worktree_entry(&fx.service.run_now().await).outcome,
            StorageCleanupReportEntryOutcome::Removed
        );
        assert_eq!(fx.service.host().full_status_calls(), 0);
    }

    #[test]
    fn decodes_reports_from_servers_without_size_fields() {
        let report: StorageCleanupReport = serde_json::from_value(json!({
            "trigger": "manual",
            "startedAt": "2026-06-10T12:00:00.000Z",
            "finishedAt": "2026-06-10T12:00:01.000Z",
            "entries": [{
                "kind": "worktree",
                "outcome": "removed",
                "reason": "Removed",
                "path": null,
                "threadId": null,
                "threadTitle": null,
            }],
            "counts": {"removed": 1, "kept": 0, "failed": 0},
            "omittedCount": 0,
        }))
        .unwrap();
        assert_eq!(report.bytes_freed, 0.0);
        assert_eq!(report.entries[0].bytes, None);
        assert_eq!(report.entries[0].files, None);
    }

    async fn applies(policy: WorktreeKeepWhen, file: &str, removed: bool) {
        let fx = fixture().await;
        fx.service
            .host()
            .update_storage(|settings| settings.worktree_keep_when = policy);
        assert!(fx.service.latest_report().is_none());
        std::fs::write(fx.worktree.join(file), "local data\n").unwrap();
        let bytes = listed_bytes(&fx.worktree);
        let report = fx.service.run_now().await;
        fx.service.drain().await;
        assert_eq!(report.trigger, StorageCleanupReportTrigger::Manual);
        let entry = worktree_entry(&report);
        assert_eq!(entry.path.as_deref(), fx.worktree.to_str());
        assert_eq!(
            entry.thread_id.as_ref().map(|id| id.as_str()),
            Some("thread-1")
        );
        assert_eq!(entry.thread_title.as_deref(), Some("Thread"));
        assert_eq!(entry.files, None);
        if removed {
            assert_eq!(entry.outcome, StorageCleanupReportEntryOutcome::Removed);
            assert_eq!(entry.bytes, Some(bytes as f64));
            assert_eq!(report.bytes_freed, bytes as f64);
            assert!(!fx.worktree.exists());
        } else {
            assert_eq!(entry.outcome, StorageCleanupReportEntryOutcome::Kept);
            assert_eq!(entry.bytes, None);
            assert_eq!(report.bytes_freed, 0.0);
            assert!(fx.worktree.exists());
            let needle = if file == ".env" {
                "ignored files (.env)"
            } else {
                "uncommitted changes (1 file)"
            };
            assert!(entry.reason.contains(needle), "{}", entry.reason);
        }
        assert_eq!(fx.service.latest_report().as_ref(), Some(&report));
    }

    #[tokio::test]
    async fn any_local_files_keeps_ignored_env() {
        applies(WorktreeKeepWhen::AnyLocalFiles, ".env", false).await;
    }

    #[tokio::test]
    async fn uncommitted_changes_removes_ignored_env() {
        applies(WorktreeKeepWhen::UncommittedChanges, ".env", true).await;
    }

    #[tokio::test]
    async fn tracked_changes_removes_ignored_env() {
        applies(WorktreeKeepWhen::TrackedChanges, ".env", true).await;
    }

    #[tokio::test]
    async fn any_local_files_keeps_untracked_notes() {
        applies(WorktreeKeepWhen::AnyLocalFiles, "notes.txt", false).await;
    }

    #[tokio::test]
    async fn uncommitted_changes_keeps_untracked_notes() {
        applies(WorktreeKeepWhen::UncommittedChanges, "notes.txt", false).await;
    }

    #[tokio::test]
    async fn tracked_changes_removes_untracked_notes() {
        applies(WorktreeKeepWhen::TrackedChanges, "notes.txt", true).await;
    }

    #[tokio::test]
    async fn any_local_files_keeps_tracked_edits() {
        applies(WorktreeKeepWhen::AnyLocalFiles, "tracked.txt", false).await;
    }

    #[tokio::test]
    async fn uncommitted_changes_keeps_tracked_edits() {
        applies(WorktreeKeepWhen::UncommittedChanges, "tracked.txt", false).await;
    }

    #[tokio::test]
    async fn tracked_changes_keeps_tracked_edits() {
        applies(WorktreeKeepWhen::TrackedChanges, "tracked.txt", false).await;
    }

    #[tokio::test]
    async fn preserves_tracked_edits_written_after_the_final_status_check() {
        let fx = fixture().await;
        fx.service.host().update_storage(|settings| {
            settings.worktree_keep_when = WorktreeKeepWhen::TrackedChanges;
        });
        fx.service
            .host()
            .arm_late_edit(fx.worktree.join("tracked.txt"));
        std::fs::write(fx.worktree.join("notes.txt"), "untracked\n").unwrap();
        let report = fx.service.run_now().await;
        let entry = worktree_entry(&report);
        assert_eq!(entry.outcome, StorageCleanupReportEntryOutcome::Failed);
        assert_eq!(entry.bytes, None);
        assert_eq!(entry.files, None);
        assert_eq!(report.bytes_freed, 0.0);
        assert!(
            entry
                .reason
                .contains("contains modified or untracked files"),
            "{}",
            entry.reason
        );
        assert_eq!(
            std::fs::read_to_string(fx.worktree.join("tracked.txt")).unwrap(),
            "late edit\n"
        );
        assert!(!fx.worktree.join("notes.txt").exists());
    }

    #[tokio::test]
    async fn streams_the_current_report_and_subsequent_runs() {
        let fx = fixture().await;
        let mut reports = fx.service.subscribe();
        let mut seen = Vec::new();
        loop {
            let current = reports.borrow_and_update().clone();
            let first_null = current.is_none() && seen.is_empty();
            seen.push(current);
            if first_null {
                fx.service.run_now().await;
            }
            if seen.len() == 2 {
                break;
            }
            reports.changed().await.unwrap();
        }
        assert!(seen[0].is_none());
        assert_eq!(seen[1], fx.service.latest_report());
        assert_eq!(
            seen[1].as_ref().unwrap().counts,
            StorageCleanupCounts {
                removed: 1.0,
                kept: 0.0,
                failed: 0.0
            }
        );
    }

    #[tokio::test]
    async fn caps_entries_while_preserving_totals_and_prioritizing_removals() {
        let fx = fixture().await;
        let mut threads = Vec::new();
        for index in 0..205 {
            let path = PathBuf::from(format!("{}-{index}", path_string(&fx.worktree)));
            std::fs::create_dir(&path).unwrap();
            threads.push(shell(json!({
                "id": format!("thread-{}", index + 2),
                "branch": "feature",
                "worktreePath": path_string(&path),
                "status": "running",
            })));
        }
        threads.push(shell(json!({
            "branch": "feature",
            "worktreePath": path_string(&fx.worktree),
        })));
        fx.service.host().set_threads(threads);
        let report = fx.service.run_now().await;
        assert_eq!(report.entries.len(), 200);
        assert_eq!(report.omitted_count, 6.0);
        assert_eq!(
            report.counts,
            StorageCleanupCounts {
                removed: 1.0,
                kept: 205.0,
                failed: 0.0
            }
        );
        assert_eq!(
            report.entries[0].outcome,
            StorageCleanupReportEntryOutcome::Removed
        );
        assert!(report.entries[0].bytes.unwrap_or(0.0) > 0.0);
        assert_eq!(report.bytes_freed, report.entries[0].bytes.unwrap());
    }

    #[tokio::test]
    async fn keeps_staged_edits_in_tracked_only_mode() {
        let fx = fixture().await;
        fx.service.host().update_storage(|settings| {
            settings.worktree_keep_when = WorktreeKeepWhen::TrackedChanges;
        });
        std::fs::write(fx.worktree.join("tracked.txt"), "staged\n").unwrap();
        git(&fx.worktree, &["add", "tracked.txt"]);
        let report = fx.service.run_now().await;
        let entry = worktree_entry(&report);
        assert_eq!(entry.outcome, StorageCleanupReportEntryOutcome::Kept);
        assert_eq!(entry.reason, "Has uncommitted changes (1 file)");
        assert!(fx.worktree.exists());
    }

    #[tokio::test]
    async fn reports_shared_and_running_worktrees_but_omits_disabled_rules_and_missing_directories()
    {
        let fx = fixture().await;
        let path = path_string(&fx.worktree);
        fx.service.host().set_threads(vec![shell(json!({
            "branch": "feature",
            "worktreePath": path,
            "status": "running",
        }))]);
        assert!(
            worktree_entry(&fx.service.run_now().await)
                .reason
                .contains("Thread is running")
        );
        let again = path_string(&fx.worktree);
        let shared = again.clone();
        fx.service.host().set_threads(vec![
            shell(json!({"branch": "feature", "worktreePath": again})),
            shell(json!({"id": "thread-2", "branch": "feature", "worktreePath": shared})),
        ]);
        assert_eq!(
            worktree_entry(&fx.service.run_now().await).reason,
            "Shared by 2 threads"
        );
        fx.service.host().update_storage(|settings| {
            settings.worktree_after_days = None;
            settings.worktree_on_delete = false;
        });
        assert!(fx.service.run_now().await.entries.is_empty());
        std::fs::remove_dir_all(&fx.worktree).unwrap();
        assert!(
            fx.service
                .run_now()
                .await
                .entries
                .iter()
                .all(|entry| { entry.kind != StorageCleanupReportEntryKind::Worktree })
        );
    }

    #[tokio::test]
    async fn serializes_concurrent_requests_and_returns_each_runs_report() {
        let fx = fixture().await;
        let first = fx.service.clone();
        let second = fx.service.clone();
        let (left, right) = tokio::join!(first.run_now(), second.run_now());
        fx.service.drain().await;
        let worktrees: Vec<_> = left
            .entries
            .iter()
            .chain(right.entries.iter())
            .filter(|entry| entry.kind == StorageCleanupReportEntryKind::Worktree)
            .collect();
        assert_eq!(worktrees.len(), 1);
        assert_eq!(
            worktrees[0].outcome,
            StorageCleanupReportEntryOutcome::Removed
        );
        assert_eq!(fx.service.latest_report(), Some(right));
    }

    fn touch_old(path: &Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
    }

    #[tokio::test]
    async fn aggregates_expired_artifacts_and_rotated_logs() {
        let fx = fixture().await;
        fx.service.host().update_storage(|settings| {
            settings.worktree_after_days = None;
            settings.worktree_on_delete = false;
            settings.browser_artifacts_after_days = Some(StorageRetentionDays::new(8).unwrap());
            settings.logs_after_days = Some(StorageRetentionDays::new(8).unwrap());
        });
        std::fs::create_dir_all(&fx.config.browser_artifacts_dir).unwrap();
        for name in ["one.png", "two.webm"] {
            touch_old(&fx.config.browser_artifacts_dir.join(name), "capture");
        }
        for name in ["server.log.1", "server.log.2", "server.log"] {
            touch_old(&fx.config.logs_dir.join(name), "log");
        }
        let report = fx.service.run_now().await;
        let files: Vec<_> = report
            .entries
            .iter()
            .filter(|entry| entry.kind != StorageCleanupReportEntryKind::Worktree)
            .collect();
        assert_eq!(files.len(), 2);
        assert_eq!(
            files[0].kind,
            StorageCleanupReportEntryKind::BrowserArtifacts
        );
        assert_eq!(files[0].outcome, StorageCleanupReportEntryOutcome::Removed);
        assert_eq!(files[0].reason, "Removed 2 browser artifacts");
        assert_eq!(files[0].bytes, Some(14.0));
        assert_eq!(files[0].files, Some(2.0));
        assert_eq!(files[1].kind, StorageCleanupReportEntryKind::Logs);
        assert_eq!(files[1].outcome, StorageCleanupReportEntryOutcome::Removed);
        assert_eq!(files[1].reason, "Removed 2 rotated logs");
        assert_eq!(files[1].bytes, Some(6.0));
        assert_eq!(files[1].files, Some(2.0));
        assert_eq!(report.bytes_freed, 20.0);
        assert!(
            std::fs::read_dir(&fx.config.browser_artifacts_dir)
                .unwrap()
                .next()
                .is_none()
        );
        assert!(fx.config.logs_dir.join("server.log").exists());
        assert!(!fx.config.logs_dir.join("server.log.1").exists());
        assert!(!fx.config.logs_dir.join("server.log.2").exists());
    }

    #[tokio::test]
    async fn reports_git_failures_without_aborting_the_remaining_categories() {
        let fx = fixture().await;
        fx.service.host().update_storage(|settings| {
            settings.browser_artifacts_after_days = Some(StorageRetentionDays::new(8).unwrap());
            settings.logs_after_days = Some(StorageRetentionDays::new(8).unwrap());
        });
        git(
            &fx.worktree,
            &["worktree", "lock", fx.worktree.to_str().unwrap()],
        );
        let report = fx.service.run_now().await;
        assert_eq!(
            report.entries[0].outcome,
            StorageCleanupReportEntryOutcome::Failed
        );
        assert!(
            report.entries[0]
                .reason
                .contains("git worktree remove: fatal:"),
            "{}",
            report.entries[0].reason
        );
        assert_eq!(
            report
                .entries
                .iter()
                .map(|entry| entry.kind)
                .collect::<Vec<_>>(),
            vec![
                StorageCleanupReportEntryKind::Worktree,
                StorageCleanupReportEntryKind::BrowserArtifacts,
                StorageCleanupReportEntryKind::Logs,
            ]
        );
    }
}
