//! The reactor cases from upstream `ThreadSettlementService.test.ts` that run
//! through narrow fakes: sweeps, merge reevaluation, settled-thread cleanup.

use super::*;
use crate::pipeline::orchestrator_port::BoxError;
use futures::future::BoxFuture;
use parking_lot::Mutex as PlMutex;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::{Notify, mpsc};
use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::orchestration_v2::OrchestrationV2AppThread;
use turin_contracts::thread_pull_request::ThreadPullRequestLink;

const NOW: &str = "2026-08-28T12:00:00.000Z";
const PROJECT: &str = "settlement-project";
const OTHER_PROJECT: &str = "other-project";

fn now_ms() -> i64 {
    DateTime::parse_from_rfc3339(NOW)
        .unwrap()
        .timestamp_millis()
}

fn utc(value: &str) -> UtcDateTime {
    value.parse().unwrap()
}

/// Settings from a JSON patch over the contract defaults.
fn settings(patch: Value) -> ServerSettings {
    let mut value = json!({});
    for (key, entry) in patch.as_object().cloned().unwrap_or_default() {
        value[key] = entry;
    }
    serde_json::from_value(value).expect("test settings decode")
}

fn project(id: &str, root: &str) -> OrchestrationProjectShell {
    serde_json::from_value(json!({
        "id": id,
        "title": format!("Project {id}"),
        "workspaceRoot": root,
        "defaultModelSelection": null,
        "scripts": [],
        "createdAt": "2026-08-01T00:00:00.000Z",
        "updatedAt": NOW,
    }))
    .expect("test project decode")
}

/// A settlement candidate with the upstream `makeThread` defaults.
fn candidate(
    id: &str,
    edit: impl FnOnce(&mut ProjectionSettlementCandidate),
) -> ProjectionSettlementCandidate {
    let mut candidate = ProjectionSettlementCandidate {
        id: ThreadId::new(id).unwrap(),
        project_id: ProjectId::new(PROJECT).unwrap(),
        branch: None,
        worktree_path: None,
        pull_requests: None,
        linked_pull_request: None,
        branch_pull_request: None,
        created_at: utc("2026-08-01T00:00:00.000Z"),
        updated_at: utc("2026-08-20T00:00:00.000Z"),
        archived_at: None,
        settled_override: None,
        pinned_at: None,
        auto_settle_disabled_at: None,
        snoozed_until: None,
        snoozed_at: None,
        last_snooze_wake_at: None,
        latest_run_id: None,
        latest_run_requested_at: None,
        latest_run_started_at: None,
        latest_run_completed_at: None,
        latest_user_message_at: Some(utc("2026-08-20T00:00:00.000Z")),
        latest_user_authored_message_at: None,
        status: OrchestrationV2ShellThreadStatus::Idle,
        activity_run_started_at: None,
        activity_run_status: None,
        pending_runtime_request: None,
        pending_background_tasks: vec![],
    };
    edit(&mut candidate);
    if candidate.latest_user_authored_message_at.is_none() {
        candidate.latest_user_authored_message_at = candidate.latest_user_message_at;
    }
    candidate
}

fn app_thread(
    id: &str,
    override_: Option<OrchestrationV2SettleOverride>,
    worktree: Option<&str>,
) -> OrchestrationV2AppThread {
    let settled = override_ == Some(OrchestrationV2SettleOverride::Settled);
    serde_json::from_value(json!({
        "createdBy": "user",
        "creationSource": "web",
        "id": id,
        "projectId": PROJECT,
        "title": id,
        "providerInstanceId": "codex",
        "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "branch": null,
        "worktreePath": worktree,
        "activeProviderThreadId": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
        "forkedFrom": null,
        "createdAt": NOW,
        "updatedAt": NOW,
        "archivedAt": null,
        "settledOverride": override_,
        "settledAt": if settled { json!(NOW) } else { Value::Null },
        "lastVisitedAt": null,
        "deletedAt": null,
    }))
    .expect("test app thread decode")
}

fn settled_event(thread: &OrchestrationV2AppThread) -> OrchestrationV2DomainEvent {
    serde_json::from_value(json!({
        "type": "thread.settled",
        "id": format!("event:settled:{}", thread.id),
        "threadId": thread.id,
        "occurredAt": NOW,
        "payload": serde_json::to_value(thread).unwrap(),
    }))
    .expect("test event decode")
}

fn detached_event(thread_id: &str) -> OrchestrationV2DomainEvent {
    serde_json::from_value(json!({
        "type": "provider-session.detached",
        "id": "event:detached",
        "threadId": thread_id,
        "occurredAt": NOW,
        "payload": {
            "providerSessionId": "provider-session:finished",
            "detachedAt": NOW,
        },
    }))
    .expect("test event decode")
}

fn merge_event() -> PullRequestMergeEvent {
    PullRequestMergeEvent {
        project_id: ProjectId::new(PROJECT).unwrap(),
        repository: "owner/repository".to_owned(),
        number: 42,
        merged_at: NOW.to_owned(),
    }
}

fn branch_pr(state: PullRequestState) -> BranchPullRequest {
    BranchPullRequest {
        state,
        repository_key: Some("example.test/owner/repository".to_owned()),
        closed_at: (state == PullRequestState::Closed).then(|| NOW.to_owned()),
        merged_at: (state == PullRequestState::Merged).then(|| NOW.to_owned()),
    }
}

fn summary(state: PullRequestState, merged_at: Option<&str>) -> SettlementPullRequest {
    SettlementPullRequest {
        state,
        closed_at: None,
        merged_at: merged_at.map(str::to_owned),
    }
}

/// A one-shot gate a test opens to release a blocked lookup.
#[derive(Default)]
struct Latch {
    open: AtomicBool,
    notify: Notify,
}

impl Latch {
    fn open(&self) {
        self.open.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.open.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

type BranchHook = Arc<dyn Fn(usize) -> BoxFuture<'static, Option<BranchPullRequest>> + Send + Sync>;
type SummaryHook = Arc<dyn Fn(usize) -> BoxFuture<'static, SettlementPullRequest> + Send + Sync>;

/// One fake standing in for every settlement port. Channels carry the events
/// the tests publish; `*_reads` and `closed`/`scripts` let tests wait on work.
struct Fake {
    now_ms: i64,
    snapshot_threads: PlMutex<Vec<ProjectionSettlementCandidate>>,
    projects: Vec<OrchestrationProjectShell>,
    settings: PlMutex<ServerSettings>,
    current_threads: Arc<PlMutex<Vec<OrchestrationV2AppThread>>>,
    existing_paths: Vec<String>,

    settings_tx: mpsc::UnboundedSender<ServerSettings>,
    settings_rx: PlMutex<Option<mpsc::UnboundedReceiver<ServerSettings>>>,
    merges_tx: mpsc::UnboundedSender<PullRequestMergeEvent>,
    merges_rx: PlMutex<Option<mpsc::UnboundedReceiver<PullRequestMergeEvent>>>,
    events_tx: mpsc::UnboundedSender<OrchestrationV2DomainEvent>,
    events_rx: PlMutex<Option<mpsc::UnboundedReceiver<OrchestrationV2DomainEvent>>>,

    candidate_reads: PlMutex<Vec<Option<String>>>,
    read_tx: mpsc::UnboundedSender<usize>,
    read_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<usize>>,
    reads: AtomicUsize,

    commands: PlMutex<Vec<(String, Option<UtcDateTime>, UtcDateTime)>>,
    on_dispatch: Arc<dyn Fn() + Send + Sync>,
    branch_calls: PlMutex<Vec<(String, String)>>,
    branch_count: AtomicUsize,
    branch: BranchHook,
    summary_calls: PlMutex<Vec<u64>>,
    summary_count: AtomicUsize,
    summary: SummaryHook,
    invalidated: PlMutex<Vec<String>>,

    closed_tx: mpsc::UnboundedSender<String>,
    closed_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<String>>,
    on_close_idle: Arc<dyn Fn() + Send + Sync>,
    script_tx: mpsc::UnboundedSender<(String, String)>,
    script_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<(String, String)>>,
    script_starts: AtomicUsize,
    script_fails_first: bool,
}

#[derive(Default)]
struct Options {
    snapshot_threads: Vec<ProjectionSettlementCandidate>,
    projects: Vec<OrchestrationProjectShell>,
    settings: Option<ServerSettings>,
    current_threads: Vec<OrchestrationV2AppThread>,
    shared_current_threads: Option<Arc<PlMutex<Vec<OrchestrationV2AppThread>>>>,
    existing_paths: Vec<String>,
    branch: Option<BranchHook>,
    summary: Option<SummaryHook>,
    on_dispatch: Option<Arc<dyn Fn() + Send + Sync>>,
    on_close_idle: Option<Arc<dyn Fn() + Send + Sync>>,
    script_fails_first: bool,
}

fn no_branch() -> BranchHook {
    Arc::new(|_| Box::pin(async { None }))
}

fn open_summary() -> SummaryHook {
    Arc::new(|_| Box::pin(async { summary(PullRequestState::Open, None) }))
}

fn fake(options: Options) -> Arc<Fake> {
    let (settings_tx, settings_rx) = mpsc::unbounded_channel();
    let (merges_tx, merges_rx) = mpsc::unbounded_channel();
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let (read_tx, read_rx) = mpsc::unbounded_channel();
    let (closed_tx, closed_rx) = mpsc::unbounded_channel();
    let (script_tx, script_rx) = mpsc::unbounded_channel();
    let projects = if options.projects.is_empty() {
        vec![project(PROJECT, "/workspace/project")]
    } else {
        options.projects
    };
    Arc::new(Fake {
        now_ms: now_ms(),
        snapshot_threads: PlMutex::new(options.snapshot_threads),
        projects,
        settings: PlMutex::new(options.settings.unwrap_or_else(|| settings(json!({})))),
        current_threads: options
            .shared_current_threads
            .unwrap_or_else(|| Arc::new(PlMutex::new(options.current_threads))),
        existing_paths: options.existing_paths,
        settings_tx,
        settings_rx: PlMutex::new(Some(settings_rx)),
        merges_tx,
        merges_rx: PlMutex::new(Some(merges_rx)),
        events_tx,
        events_rx: PlMutex::new(Some(events_rx)),
        candidate_reads: PlMutex::new(vec![]),
        read_tx,
        read_rx: tokio::sync::Mutex::new(read_rx),
        reads: AtomicUsize::new(0),
        commands: PlMutex::new(vec![]),
        on_dispatch: options.on_dispatch.unwrap_or_else(|| Arc::new(|| {})),
        branch_calls: PlMutex::new(vec![]),
        branch_count: AtomicUsize::new(0),
        branch: options.branch.unwrap_or_else(no_branch),
        summary_calls: PlMutex::new(vec![]),
        summary_count: AtomicUsize::new(0),
        summary: options.summary.unwrap_or_else(open_summary),
        invalidated: PlMutex::new(vec![]),
        closed_tx,
        closed_rx: tokio::sync::Mutex::new(closed_rx),
        on_close_idle: options.on_close_idle.unwrap_or_else(|| Arc::new(|| {})),
        script_tx,
        script_rx: tokio::sync::Mutex::new(script_rx),
        script_starts: AtomicUsize::new(0),
        script_fails_first: options.script_fails_first,
    })
}

struct FakeStore(Arc<Fake>);
struct FakeSettings(Arc<Fake>);
struct FakeGit(Arc<Fake>);
struct FakePullRequests(Arc<Fake>);
struct FakeTerminals(Arc<Fake>);
struct FakeScripts(Arc<Fake>);
struct FakeFiles(Arc<Fake>);
struct FakeOrchestrator(Arc<Fake>);

#[async_trait::async_trait]
impl SettlementStore for FakeStore {
    async fn settlement_candidates(
        &self,
        thread_id: Option<&ThreadId>,
    ) -> Result<Vec<ProjectionSettlementCandidate>, BoxError> {
        self.0
            .candidate_reads
            .lock()
            .push(thread_id.map(|id| id.as_str().to_owned()));
        let count = self.0.reads.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.0.read_tx.send(count);
        Ok(self
            .0
            .snapshot_threads
            .lock()
            .iter()
            .filter(|thread| thread_id.is_none_or(|id| &thread.id == id))
            .cloned()
            .collect())
    }

    async fn app_thread(&self, thread_id: &ThreadId) -> Result<OrchestrationV2AppThread, BoxError> {
        self.0
            .current_threads
            .lock()
            .iter()
            .find(|thread| &thread.id == thread_id)
            .cloned()
            .ok_or_else(|| format!("Unexpected thread read: {thread_id}").into())
    }

    async fn project_shells(&self) -> Result<Vec<OrchestrationProjectShell>, BoxError> {
        Ok(self.0.projects.clone())
    }
}

#[async_trait::async_trait]
impl SettlementSettingsSource for FakeSettings {
    async fn settings(&self) -> Result<ServerSettings, BoxError> {
        Ok(self.0.settings.lock().clone())
    }

    async fn subscribe_changes(&self) -> Result<BoxStream<'static, ServerSettings>, BoxError> {
        let rx = self.0.settings_rx.lock().take().expect("subscribed once");
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|value| (value, rx))
        })
        .boxed())
    }
}

#[async_trait::async_trait]
impl SettlementGit for FakeGit {
    async fn branch_pull_request(
        &self,
        cwd: &str,
        branch: &str,
        _refresh: bool,
    ) -> Result<Option<BranchPullRequest>, BoxError> {
        self.0
            .branch_calls
            .lock()
            .push((cwd.to_owned(), branch.to_owned()));
        let count = self.0.branch_count.fetch_add(1, Ordering::SeqCst) + 1;
        Ok((self.0.branch)(count).await)
    }

    async fn invalidate_status(&self, cwd: &str) {
        self.0.invalidated.lock().push(cwd.to_owned());
    }
}

#[async_trait::async_trait]
impl SettlementPullRequests for FakePullRequests {
    async fn summary(
        &self,
        reference: &PullRequestRef,
        _recover_transient_failure: bool,
    ) -> Result<SettlementPullRequest, BoxError> {
        self.0.summary_calls.lock().push(reference.number);
        let count = self.0.summary_count.fetch_add(1, Ordering::SeqCst) + 1;
        Ok((self.0.summary)(count).await)
    }

    async fn subscribe_merges(
        &self,
    ) -> Result<BoxStream<'static, PullRequestMergeEvent>, BoxError> {
        let rx = self.0.merges_rx.lock().take().expect("subscribed once");
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|value| (value, rx))
        })
        .boxed())
    }
}

#[async_trait::async_trait]
impl SettlementTerminals for FakeTerminals {
    async fn close_idle(&self, thread_id: &ThreadId) -> Result<(), BoxError> {
        let _ = self.0.closed_tx.send(thread_id.as_str().to_owned());
        (self.0.on_close_idle)();
        Ok(())
    }
}

#[async_trait::async_trait]
impl SettlementScripts for FakeScripts {
    async fn run_settle_script(
        &self,
        request: SettleScriptRequest,
    ) -> Result<SettleScriptRun, BoxError> {
        let starts = self.0.script_starts.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.0.script_tx.send((
            request.thread_id.as_str().to_owned(),
            request.worktree_path.clone(),
        ));
        if self.0.script_fails_first && starts == 1 {
            return Err("terminal failed to open".into());
        }
        Ok(SettleScriptRun::NoScript)
    }
}

#[async_trait::async_trait]
impl SettlementFiles for FakeFiles {
    async fn exists(&self, path: &str) -> Result<bool, BoxError> {
        Ok(self
            .0
            .existing_paths
            .iter()
            .any(|existing| existing == path))
    }
}

#[async_trait::async_trait]
impl SettlementOrchestrator for FakeOrchestrator {
    async fn dispatch_command(
        &self,
        command: OrchestrationV2ServerCommand,
    ) -> Result<(), BoxError> {
        match command {
            OrchestrationV2ServerCommand::Client(OrchestrationV2Command::ThreadAutoSettle {
                thread_id,
                snapshot_at,
                settled_at,
                ..
            }) => {
                self.0.commands.lock().push((
                    thread_id.as_str().to_owned(),
                    settled_at,
                    snapshot_at,
                ));
                (self.0.on_dispatch)();
                Ok(())
            }
            _ => Err("Unexpected command".into()),
        }
    }

    async fn domain_events(
        &self,
    ) -> Result<BoxStream<'static, OrchestrationV2DomainEvent>, BoxError> {
        let rx = self.0.events_rx.lock().take().expect("subscribed once");
        Ok(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|value| (value, rx))
        })
        .boxed())
    }
}

struct Harness {
    fake: Arc<Fake>,
    service: ThreadSettlementService,
}

impl Harness {
    fn new(options: Options) -> Self {
        Self::with_store(options, |fake| Arc::new(FakeStore(fake)))
    }

    fn with_store(
        options: Options,
        store: impl FnOnce(Arc<Fake>) -> Arc<dyn SettlementStore>,
    ) -> Self {
        let fake = fake(options);
        let deps = SettlementDeps {
            orchestrator: Arc::new(FakeOrchestrator(fake.clone())),
            store: store(fake.clone()),
            settings: Arc::new(FakeSettings(fake.clone())),
            git: Arc::new(FakeGit(fake.clone())),
            pull_requests: Arc::new(FakePullRequests(fake.clone())),
            terminals: Arc::new(FakeTerminals(fake.clone())),
            scripts: Arc::new(FakeScripts(fake.clone())),
            files: Arc::new(FakeFiles(fake.clone())),
            now_ms: {
                let now = fake.now_ms;
                Arc::new(move || now)
            },
        };
        Self {
            service: ThreadSettlementService::new(deps),
            fake,
        }
    }

    /// `startHarness`: start, then wait for the first sweep to read candidates.
    async fn start(&self) {
        self.service.start().await.expect("start");
        self.fake
            .read_rx
            .lock()
            .await
            .recv()
            .await
            .expect("first sweep");
        self.fake.candidate_reads.lock().clear();
    }

    async fn await_read(&self) {
        self.fake
            .read_rx
            .lock()
            .await
            .recv()
            .await
            .expect("sweep read");
    }

    fn commands(&self) -> Vec<(String, Option<UtcDateTime>, UtcDateTime)> {
        self.fake.commands.lock().clone()
    }

    fn publish(&self, event: OrchestrationV2DomainEvent) {
        self.fake.events_tx.send(event).expect("event stream open");
    }

    fn publish_merge(&self) {
        self.fake
            .merges_tx
            .send(merge_event())
            .expect("merge stream open");
    }

    fn update_settings(&self, next: ServerSettings) {
        *self.fake.settings.lock() = next.clone();
        self.fake
            .settings_tx
            .send(next)
            .expect("settings stream open");
    }

    async fn next_closed(&self) -> String {
        self.fake
            .closed_rx
            .lock()
            .await
            .recv()
            .await
            .expect("close")
    }

    async fn next_script(&self) -> (String, String) {
        self.fake
            .script_rx
            .lock()
            .await
            .recv()
            .await
            .expect("script run")
    }
}

fn thread_ids(commands: &[(String, Option<UtcDateTime>, UtcDateTime)]) -> Vec<String> {
    commands.iter().map(|(id, _, _)| id.clone()).collect()
}

fn pr_link(number: u64, state: PullRequestState, merged_at: Option<&str>) -> ThreadPullRequestLink {
    serde_json::from_value(json!({
        "host": "example.test",
        "repository": "owner/repository",
        "number": number,
        "url": format!("https://example.test/owner/repository/pull/{number}"),
        "source": "manual",
        "linkedAt": "2026-08-20T00:00:00.000Z",
        "snapshot": {
            "state": state,
            "title": "Pull request",
            "headBranch": "feature",
            "baseBranch": "main",
            "isDraft": false,
            "updatedAt": NOW,
            "syncedAt": NOW,
            "closedAt": null,
            "mergedAt": merged_at,
        },
        "stack": null,
    }))
    .expect("test link decode")
}

fn linked(number: u64) -> ThreadLinkedPullRequest {
    serde_json::from_value(json!({
        "projectId": PROJECT,
        "repository": "owner/repository",
        "number": number,
        "url": format!("https://example.test/owner/repository/pull/{number}"),
    }))
    .expect("test linked decode")
}

fn after_days(days: Option<f64>) -> ServerSettings {
    settings(json!({ "sidebarAutoSettleAfterDays": days }))
}

// -- worker -----------------------------------------------------------------

#[tokio::test]
async fn settles_a_merged_pull_request_stored_only_in_the_thread_links() {
    let thread = candidate("merged-link", |t| {
        t.pull_requests = Some(vec![pr_link(42, PullRequestState::Merged, Some(NOW))]);
    });
    let harness = Harness::new(Options {
        snapshot_threads: vec![thread.clone()],
        settings: Some(settings(json!({ "sidebarAutoSettleAfterDays": null }))),
        ..Default::default()
    });
    harness.start().await;
    harness.service.drain().await;
    assert_eq!(
        thread_ids(&harness.commands()),
        vec![thread.id.as_str().to_owned()]
    );
}

#[tokio::test]
async fn settles_only_the_project_opted_in_while_environment_settlement_is_disabled() {
    let thread = candidate("project-opt-in", |t| {
        t.latest_run_completed_at = Some(utc("2026-08-25T00:00:00.000Z"));
    });
    let other = candidate("environment-off", |t| {
        t.project_id = ProjectId::new(OTHER_PROJECT).unwrap()
    });
    let harness = Harness::new(Options {
        snapshot_threads: vec![thread.clone(), other],
        settings: Some(settings(json!({
            "sidebarAutoSettleAfterDays": null,
            "sidebarAutoSettleOnMerge": false,
            "projectSettingsOverrides": { PROJECT: { "sidebarAutoSettleAfterDays": 2 } },
        }))),
        projects: vec![
            project(PROJECT, "/workspace/project"),
            project(OTHER_PROJECT, "/workspace/other"),
        ],
        ..Default::default()
    });
    harness.start().await;
    harness.service.drain().await;
    assert_eq!(
        thread_ids(&harness.commands()),
        vec![thread.id.as_str().to_owned()]
    );
}

#[tokio::test]
async fn dispatches_the_last_activity_time_with_the_v2_snapshot_guard() {
    let thread = candidate("inactive-open-pr", |t| {
        t.branch = TrimmedNonEmptyString::new("feature").ok();
        t.latest_run_completed_at = Some(utc("2026-08-25T00:00:00.000Z"));
    });
    let harness = Harness::new(Options {
        snapshot_threads: vec![thread.clone()],
        settings: Some(after_days(Some(2.0))),
        branch: Some(Arc::new(|_| {
            Box::pin(async { Some(branch_pr(PullRequestState::Open)) })
        })),
        ..Default::default()
    });
    harness.start().await;
    harness.service.drain().await;
    let commands = harness.commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].1, thread.latest_run_completed_at);
    assert_eq!(commands[0].2, thread.updated_at);
}

#[tokio::test]
async fn skips_the_branch_recheck_when_a_terminal_link_would_settle_nothing() {
    let resumed = candidate("resumed-manual", |t| {
        t.branch = TrimmedNonEmptyString::new("main").ok();
        t.linked_pull_request = Some(Some(linked(1)));
        t.latest_user_message_at = Some(utc("2026-08-28T00:00:00.000Z"));
    });
    let harness = Harness::new(Options {
        snapshot_threads: vec![resumed],
        settings: Some(settings(json!({
            "sidebarAutoSettleAfterDays": null,
            "sidebarAutoSettleOnMerge": true,
        }))),
        branch: Some(Arc::new(|_| {
            Box::pin(async { Some(branch_pr(PullRequestState::Open)) })
        })),
        summary: Some(Arc::new(|_| {
            Box::pin(async { summary(PullRequestState::Merged, Some("2026-08-27T00:00:00.000Z")) })
        })),
        ..Default::default()
    });
    harness.start().await;
    harness.service.drain().await;
    assert!(harness.commands().is_empty());
    assert_eq!(harness.fake.summary_calls.lock().len(), 1);
    assert!(harness.fake.branch_calls.lock().is_empty());
}

#[tokio::test]
async fn reevaluates_immediately_after_a_pull_request_merge() {
    let periodic_started = Arc::new(Latch::default());
    let release_periodic = Arc::new(Latch::default());
    let merged_settled = Arc::new(Latch::default());
    let lookups = Arc::new(AtomicUsize::new(0));
    let branch: BranchHook = {
        let lookups = lookups.clone();
        let periodic_started = periodic_started.clone();
        let release_periodic = release_periodic.clone();
        Arc::new(move |_| {
            let count = lookups.fetch_add(1, Ordering::SeqCst) + 1;
            let periodic_started = periodic_started.clone();
            let release_periodic = release_periodic.clone();
            Box::pin(async move {
                if count == 1 {
                    return Some(branch_pr(PullRequestState::Open));
                }
                periodic_started.open();
                release_periodic.wait().await;
                Some(branch_pr(PullRequestState::Open))
            })
        })
    };
    let merged_for_dispatch = merged_settled.clone();
    let harness = Harness::new(Options {
        snapshot_threads: vec![
            candidate("merged-in-app", |t| {
                t.latest_user_message_at = Some(utc("2026-08-27T00:00:00.000Z"));
                t.linked_pull_request = Some(Some(ThreadLinkedPullRequest {
                    repository: TrimmedNonEmptyString::new("Owner/Repository").unwrap(),
                    ..linked(42)
                }));
            }),
            candidate("slow-periodic-lookup", |t| {
                t.branch = TrimmedNonEmptyString::new("another-feature").ok();
                t.latest_user_message_at = Some(utc("2026-08-27T00:00:00.000Z"));
            }),
        ],
        branch: Some(branch),
        on_dispatch: Some(Arc::new(move || merged_for_dispatch.open())),
        ..Default::default()
    });
    harness.start().await;
    harness.update_settings(after_days(Some(4.0)));
    periodic_started.wait().await;

    harness.publish_merge();
    merged_settled.wait().await;
    assert_eq!(
        thread_ids(&harness.commands()),
        vec!["merged-in-app".to_owned()]
    );

    release_periodic.open();
    harness.service.drain().await;
}

#[tokio::test]
async fn settles_branch_threads_on_a_merge_without_waiting_for_the_next_sweep() {
    let state = Arc::new(PlMutex::new(PullRequestState::Open));
    let merged_settled = Arc::new(Latch::default());
    let branch: BranchHook = {
        let state = state.clone();
        Arc::new(move |_| {
            let state = *state.lock();
            Box::pin(async move { Some(branch_pr(state)) })
        })
    };
    let merged_for_dispatch = merged_settled.clone();
    let harness = Harness::new(Options {
        snapshot_threads: vec![candidate("branch-thread", |t| {
            t.branch = TrimmedNonEmptyString::new("saved-feature").ok();
            t.latest_user_message_at = Some(utc("2026-08-27T00:00:00.000Z"));
        })],
        branch: Some(branch),
        on_dispatch: Some(Arc::new(move || merged_for_dispatch.open())),
        ..Default::default()
    });
    harness.start().await;
    assert!(harness.commands().is_empty());
    assert!(harness.fake.invalidated.lock().is_empty());

    *state.lock() = PullRequestState::Merged;
    harness.publish_merge();
    merged_settled.wait().await;
    assert_eq!(
        thread_ids(&harness.commands()),
        vec!["branch-thread".to_owned()]
    );
    assert_eq!(
        *harness.fake.invalidated.lock(),
        vec!["/workspace/project".to_owned()]
    );
    harness.service.drain().await;
}

#[tokio::test]
async fn a_merge_does_not_settle_threads_linked_to_an_unrelated_pull_request() {
    let merged_settled = Arc::new(Latch::default());
    let lookup_started = Arc::new(Latch::default());
    let release_lookup = Arc::new(Latch::default());
    let summary_hook: SummaryHook = {
        let lookup_started = lookup_started.clone();
        let release_lookup = release_lookup.clone();
        Arc::new(move |count| {
            let lookup_started = lookup_started.clone();
            let release_lookup = release_lookup.clone();
            Box::pin(async move {
                // The initial sweep looks up both linked threads; the merge sweep
                // only looks up the unrelated one, since the merged thread settles
                // from the event itself.
                if count == 3 {
                    lookup_started.open();
                    release_lookup.wait().await;
                }
                summary(PullRequestState::Open, None)
            })
        })
    };
    let merged_for_dispatch = merged_settled.clone();
    let harness = Harness::new(Options {
        snapshot_threads: vec![
            candidate("merged-in-app", |t| {
                t.latest_user_message_at = Some(utc("2026-08-27T00:00:00.000Z"));
                t.linked_pull_request = Some(Some(linked(42)));
            }),
            candidate("unrelated-linked", |t| {
                t.latest_user_message_at = Some(utc("2026-08-27T00:00:00.000Z"));
                t.linked_pull_request = Some(Some(linked(99)));
            }),
        ],
        summary: Some(summary_hook),
        on_dispatch: Some(Arc::new(move || merged_for_dispatch.open())),
        ..Default::default()
    });
    harness.start().await;
    assert!(harness.commands().is_empty());

    harness.publish_merge();
    lookup_started.wait().await;
    merged_settled.wait().await;
    release_lookup.open();
    harness.service.drain().await;

    assert_eq!(
        thread_ids(&harness.commands()),
        vec!["merged-in-app".to_owned()]
    );
    let mut numbers = harness.fake.summary_calls.lock().clone();
    numbers.sort_unstable();
    assert_eq!(numbers, vec![42, 99, 99]);
}

#[tokio::test]
async fn looks_up_the_branch_pull_request_from_a_threads_live_worktree() {
    let harness = Harness::new(Options {
        snapshot_threads: vec![
            candidate("live-worktree", |t| {
                t.branch = TrimmedNonEmptyString::new("feature/live").ok();
                t.worktree_path =
                    TrimmedNonEmptyString::new("/workspace/project-root/.worktrees/live").ok();
            }),
            candidate("deleted-worktree", |t| {
                t.branch = TrimmedNonEmptyString::new("feature/deleted").ok();
                t.worktree_path =
                    TrimmedNonEmptyString::new("/workspace/project-root/.worktrees/deleted").ok();
            }),
        ],
        projects: vec![project(PROJECT, "/workspace/project-root")],
        existing_paths: vec!["/workspace/project-root/.worktrees/live".to_owned()],
        settings: Some(settings(json!({
            "sidebarAutoSettleAfterDays": null,
            "sidebarAutoSettleOnMerge": true,
        }))),
        ..Default::default()
    });
    harness.start().await;
    harness.service.drain().await;
    let calls: HashSet<(String, String)> =
        harness.fake.branch_calls.lock().iter().cloned().collect();
    let expected: HashSet<(String, String)> = [
        ("/workspace/project-root/.worktrees/live", "feature/live"),
        ("/workspace/project-root", "feature/deleted"),
    ]
    .into_iter()
    .map(|(cwd, branch)| (cwd.to_owned(), branch.to_owned()))
    .collect();
    assert_eq!(calls, expected);
}

// -- terminals and settle scripts ------------------------------------------

fn app(
    id: &str,
    override_: Option<OrchestrationV2SettleOverride>,
    worktree: Option<&str>,
) -> OrchestrationV2AppThread {
    app_thread(id, override_, worktree)
}

#[tokio::test]
async fn closes_the_threads_idle_terminals_when_it_settles() {
    let thread = app(
        "settled-thread",
        Some(OrchestrationV2SettleOverride::Settled),
        None,
    );
    let harness = Harness::new(Options {
        current_threads: vec![thread.clone()],
        ..Default::default()
    });
    harness.start().await;
    harness.publish(settled_event(&thread));
    assert_eq!(harness.next_closed().await, "settled-thread");
    harness.service.drain().await;
}

#[tokio::test]
async fn keeps_the_terminals_of_a_thread_reengaged_before_the_event_ran() {
    let settled = app(
        "reengaged-thread",
        Some(OrchestrationV2SettleOverride::Settled),
        None,
    );
    let reengaged = app(
        "reengaged-thread",
        Some(OrchestrationV2SettleOverride::Active),
        None,
    );
    let marker = app(
        "marker-thread",
        Some(OrchestrationV2SettleOverride::Settled),
        None,
    );
    let harness = Harness::new(Options {
        current_threads: vec![reengaged, marker.clone()],
        ..Default::default()
    });
    harness.start().await;
    harness.publish(settled_event(&settled));
    // Events run in order, so the first close belongs to the later event only
    // if the re-engaged thread was skipped.
    harness.publish(settled_event(&marker));
    assert_eq!(harness.next_closed().await, "marker-thread");
    harness.service.drain().await;
}

#[tokio::test]
async fn runs_the_settle_script_in_the_threads_own_worktree() {
    let shared = app(
        "shared-checkout-thread",
        Some(OrchestrationV2SettleOverride::Settled),
        None,
    );
    let missing = app(
        "removed-worktree-thread",
        Some(OrchestrationV2SettleOverride::Settled),
        Some("/worktrees/removed"),
    );
    let worktree = app(
        "worktree-thread",
        Some(OrchestrationV2SettleOverride::Settled),
        Some("/worktrees/thread"),
    );
    let harness = Harness::new(Options {
        current_threads: vec![shared.clone(), missing.clone(), worktree.clone()],
        existing_paths: vec!["/worktrees/thread".to_owned()],
        ..Default::default()
    });
    harness.start().await;
    // Events run in order, so the first run belongs to the last event only if
    // the shared checkout and the removed worktree were skipped.
    harness.publish(settled_event(&shared));
    harness.publish(settled_event(&missing));
    harness.publish(settled_event(&worktree));
    assert_eq!(
        harness.next_script().await,
        ("worktree-thread".to_owned(), "/worktrees/thread".to_owned())
    );
    harness.service.drain().await;
}

#[tokio::test]
async fn skips_the_settle_script_for_a_thread_reengaged_while_its_shells_close() {
    let settled = app(
        "reengaging-thread",
        Some(OrchestrationV2SettleOverride::Settled),
        Some("/worktrees/reengaging"),
    );
    let marker = app(
        "marker-thread",
        Some(OrchestrationV2SettleOverride::Settled),
        Some("/worktrees/marker"),
    );
    let current = Arc::new(PlMutex::new(vec![settled.clone(), marker.clone()]));
    // The user sends a message while the first close checks processes.
    let on_close_idle: Arc<dyn Fn() + Send + Sync> = {
        let current = current.clone();
        let settled_id = settled.id.clone();
        Arc::new(move || {
            let mut threads = current.lock();
            if let Some(entry) = threads.iter_mut().find(|t| t.id == settled_id) {
                entry.settled_override = Some(OrchestrationV2SettleOverride::Active);
            }
        })
    };
    let harness = Harness::new(Options {
        shared_current_threads: Some(current.clone()),
        existing_paths: vec![
            "/worktrees/reengaging".to_owned(),
            "/worktrees/marker".to_owned(),
        ],
        on_close_idle: Some(on_close_idle),
        ..Default::default()
    });
    harness.start().await;
    harness.publish(settled_event(&settled));
    harness.publish(settled_event(&marker));
    assert_eq!(harness.next_script().await.0, "marker-thread");
    harness.service.drain().await;
}

#[tokio::test]
async fn runs_the_settle_script_once_when_a_settlement_is_reemitted() {
    let repeated = app(
        "repeated-thread",
        Some(OrchestrationV2SettleOverride::Settled),
        Some("/worktrees/repeated"),
    );
    let marker = app(
        "marker-thread",
        Some(OrchestrationV2SettleOverride::Settled),
        Some("/worktrees/marker"),
    );
    let harness = Harness::new(Options {
        current_threads: vec![repeated.clone(), marker.clone()],
        existing_paths: vec![
            "/worktrees/repeated".to_owned(),
            "/worktrees/marker".to_owned(),
        ],
        ..Default::default()
    });
    harness.start().await;
    // Settling an already settled thread emits the same settlement again.
    harness.publish(settled_event(&repeated));
    harness.publish(settled_event(&repeated));
    harness.publish(settled_event(&marker));
    assert_eq!(harness.next_script().await.0, "repeated-thread");
    assert_eq!(harness.next_script().await.0, "marker-thread");
    harness.service.drain().await;
}

#[tokio::test]
async fn retries_the_settle_script_on_a_reemitted_settlement_after_a_failed_start() {
    let thread = app(
        "failed-start-thread",
        Some(OrchestrationV2SettleOverride::Settled),
        Some("/worktrees/failed-start"),
    );
    let harness = Harness::new(Options {
        current_threads: vec![thread.clone()],
        existing_paths: vec!["/worktrees/failed-start".to_owned()],
        script_fails_first: true,
        ..Default::default()
    });
    harness.start().await;
    harness.publish(settled_event(&thread));
    harness.publish(settled_event(&thread));
    assert_eq!(harness.next_script().await.0, "failed-start-thread");
    assert_eq!(harness.next_script().await.0, "failed-start-thread");
    harness.service.drain().await;
}

// -- single-thread sweep ----------------------------------------------------

#[tokio::test]
async fn a_finished_run_reads_only_its_own_threads_settlement_candidate() {
    let finished = candidate("finished-run", |_| {});
    let other = candidate("other-thread", |_| {});
    let harness = Harness::new(Options {
        snapshot_threads: vec![finished.clone(), other],
        settings: Some(after_days(Some(3.0))),
        ..Default::default()
    });
    harness.start().await;
    harness.publish(detached_event("finished-run"));
    harness.await_read().await;
    harness.service.drain().await;
    assert_eq!(
        *harness.fake.candidate_reads.lock(),
        vec![Some(finished.id.as_str().to_owned())]
    );
}

/// A store whose first candidate read panics, then behaves like the fake.
struct PanicOnceStore {
    inner: FakeStore,
    panicked: AtomicBool,
}

#[async_trait::async_trait]
impl SettlementStore for PanicOnceStore {
    async fn settlement_candidates(
        &self,
        thread_id: Option<&ThreadId>,
    ) -> Result<Vec<ProjectionSettlementCandidate>, BoxError> {
        if !self.panicked.swap(true, Ordering::SeqCst) {
            panic!("candidate read blew up");
        }
        self.inner.settlement_candidates(thread_id).await
    }

    async fn app_thread(&self, thread_id: &ThreadId) -> Result<OrchestrationV2AppThread, BoxError> {
        self.inner.app_thread(thread_id).await
    }

    async fn project_shells(&self) -> Result<Vec<OrchestrationProjectShell>, BoxError> {
        self.inner.project_shells().await
    }
}

#[tokio::test]
async fn a_panicking_sweep_does_not_wedge_the_worker_for_later_sweeps() {
    let thread = candidate("inactive-open-pr", |t| {
        t.branch = TrimmedNonEmptyString::new("feature").ok();
        t.latest_run_completed_at = Some(utc("2026-08-25T00:00:00.000Z"));
    });
    let harness = Harness::with_store(
        Options {
            snapshot_threads: vec![thread.clone()],
            settings: Some(after_days(Some(2.0))),
            branch: Some(Arc::new(|_| {
                Box::pin(async { Some(branch_pr(PullRequestState::Open)) })
            })),
            ..Default::default()
        },
        |fake| {
            Arc::new(PanicOnceStore {
                inner: FakeStore(fake),
                panicked: AtomicBool::new(false),
            })
        },
    );
    // The first sweep panics inside its candidate read. Drain must return.
    harness.service.inner.worker.enqueue(None);
    tokio::time::timeout(Duration::from_secs(10), harness.service.drain())
        .await
        .expect("drain hung after a panicking sweep");
    assert!(harness.commands().is_empty());
    // The worker survived, so the next sweep reads the candidate and settles it.
    harness.service.inner.worker.enqueue(None);
    tokio::time::timeout(Duration::from_secs(10), harness.service.drain())
        .await
        .expect("drain hung after a panicking sweep");
    let commands = harness.commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].1, thread.latest_run_completed_at);
}
