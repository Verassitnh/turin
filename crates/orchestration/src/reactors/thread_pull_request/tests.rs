//! Tests for [`ThreadPullRequestService`]. Port of the guard, read-path and identity cases in
//! ThreadPullRequestService.test.ts, plus discovery integration over fake project shells,
//! identity resolver, git and orchestrator.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::json;
use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::orchestration_project::OrchestrationProjectShell;
use turin_contracts::orchestration_v2::{
    OrchestrationV2Command, OrchestrationV2DomainEvent, OrchestrationV2ServerCommand,
    OrchestrationV2ThreadShell, OrchestrationV2ThreadShellSnapshot,
};
use turin_contracts::pull_request::{PullRequestRef, PullRequestSummary};
use turin_contracts::thread_pull_request::ThreadLinkedPullRequest;
use turin_contracts::{ProjectId, ThreadId};
use turin_git::git_manager::GitBranchPullRequest;
use turin_source_control::pull_request::service::errors::PullRequestError;
use turin_source_control::pull_request::service::ports::RepositoryIdentityResolver;

use super::*;
use crate::pipeline::test_support::MockOrchestrator;
use crate::projection::test_support::{event, run as run_json, thread as thread_json};

fn project_shell(root: &str) -> OrchestrationProjectShell {
    serde_json::from_value(json!({
        "id": "project:test",
        "title": "Test",
        "workspaceRoot": root,
        "repositoryIdentity": null,
        "defaultModelSelection": null,
        "defaultThreadEnvMode": null,
        "scripts": [],
        "createdAt": "2026-10-02T12:00:00.000Z",
        "updatedAt": "2026-10-02T12:00:00.000Z",
    }))
    .expect("project shell fixture decodes")
}

fn linked(repository: &str, number: u64, url: &str) -> ThreadLinkedPullRequest {
    serde_json::from_value(json!({
        "projectId": "project:test",
        "repository": repository,
        "number": number,
        "url": url,
    }))
    .expect("linked pull request fixture decodes")
}

// ---- Stubs for the seams the constructor needs. The guard and event tests never call them. ----

struct NoProjects;

#[async_trait]
impl ProjectShells for NoProjects {
    async fn list(&self) -> Result<Vec<OrchestrationProjectShell>, BoxError> {
        Ok(Vec::new())
    }

    async fn get(&self, _: &ProjectId) -> Result<Option<OrchestrationProjectShell>, BoxError> {
        Ok(None)
    }
}

struct NoGit;

#[async_trait]
impl BranchPullRequests for NoGit {
    async fn branch_pull_request(
        &self,
        _: &str,
        _: &str,
        _: bool,
    ) -> Result<Option<turin_git::git_manager::GitBranchPullRequest>, BoxError> {
        Ok(None)
    }
}

struct NoSummaries;

#[async_trait]
impl PullRequestSummaries for NoSummaries {
    async fn summary(&self, _: &PullRequestRef) -> Result<PullRequestSummary, PullRequestError> {
        panic!("no summary is read in these tests")
    }
}

struct NoIdentities;

#[async_trait]
impl RepositoryIdentityResolver for NoIdentities {
    async fn resolve(&self, _: &str, _: bool) -> Option<RepositoryIdentity> {
        None
    }
}

fn service() -> ThreadPullRequestService {
    ThreadPullRequestService::new(
        Arc::new(MockOrchestrator::default()),
        Arc::new(NoProjects),
        Arc::new(NoGit),
        Arc::new(NoSummaries),
        Arc::new(NoIdentities),
    )
}

// ---- Guards ----

#[test]
fn a_result_is_applied_while_the_project_root_is_unchanged() {
    let current = project_shell("/repo");
    assert!(project_workspace_matches_snapshot(Some(&current), "/repo"));
}

#[test]
fn a_result_is_rejected_when_the_project_root_changed_before_dispatch() {
    let current = project_shell("/elsewhere");
    assert!(!project_workspace_matches_snapshot(Some(&current), "/repo"));
}

#[test]
fn a_result_is_rejected_when_the_project_was_deleted_before_dispatch() {
    assert!(!project_workspace_matches_snapshot(None, "/repo"));
}

#[test]
fn a_missing_link_matches_only_a_missing_link() {
    assert!(same_pull_request(None, None));
    let a = linked(
        "pingdotgg/t3code",
        7,
        "https://github.com/pingdotgg/t3code/pull/7",
    );
    assert!(!same_pull_request(Some(&a), None));
    assert!(!same_pull_request(None, Some(&a)));
}

#[test]
fn repository_names_compare_case_insensitively() {
    let a = linked(
        "PingDotGG/T3Code",
        7,
        "https://github.com/pingdotgg/t3code/pull/7",
    );
    let b = linked(
        "pingdotgg/t3code",
        7,
        "https://github.com/pingdotgg/t3code/pull/7",
    );
    assert!(same_pull_request(Some(&a), Some(&b)));
    let other_number = linked(
        "pingdotgg/t3code",
        8,
        "https://github.com/pingdotgg/t3code/pull/7",
    );
    assert!(!same_pull_request(Some(&a), Some(&other_number)));
}

// ---- Which pass an event asks for ----

fn thread_event(kind: &str, payload: serde_json::Value) -> OrchestrationV2DomainEvent {
    event(kind, "thread:pr", "2026-10-02T12:00:00.000Z", payload)
}

#[test]
fn thread_creation_reads_that_thread_without_a_forced_refresh() {
    let request = ThreadPullRequestService::request_for_event(&thread_event(
        "thread.created",
        thread_json("thread:pr"),
    ))
    .expect("creation asks for a pass");
    assert_eq!(
        request.thread_id,
        Some(ThreadId::new("thread:pr").expect("thread id"))
    );
    assert!(!request.refresh);
    assert!(!request.backfill);
}

#[test]
fn a_finished_run_reads_that_thread_with_a_fresh_host_lookup() {
    let finished = thread_event(
        "run.updated",
        run_json("thread:pr", "run:pr", 1, "completed"),
    );
    let request = ThreadPullRequestService::request_for_event(&finished).expect("a finish asks");
    assert!(request.refresh);
    assert_eq!(
        request.thread_id,
        Some(ThreadId::new("thread:pr").expect("thread id"))
    );
}

#[test]
fn a_run_still_going_asks_for_nothing() {
    let running = thread_event("run.updated", run_json("thread:pr", "run:pr", 1, "running"));
    assert!(ThreadPullRequestService::request_for_event(&running).is_none());
}

#[test]
fn the_periodic_request_covers_every_thread() {
    let request = ThreadPullRequestService::periodic_request();
    assert_eq!(request.thread_id, None);
    assert!(!request.refresh);
}

// ---- Which reads a pass makes ----

#[test]
fn a_periodic_sweep_reads_only_unsettled_threads_once_backfill_is_done() {
    let periodic = ThreadPullRequestService::periodic_request();
    assert!(periodic_reads_unsettled_only(&periodic, false));
}

#[test]
fn a_periodic_sweep_reads_settled_threads_while_backfill_is_owed() {
    let periodic = ThreadPullRequestService::periodic_request();
    assert!(!periodic_reads_unsettled_only(&periodic, true));
    let backfill = RefreshRequest {
        thread_id: None,
        refresh: false,
        backfill: true,
    };
    assert!(!periodic_reads_unsettled_only(&backfill, false));
}

// ---- Backfill countdown ----

#[test]
fn a_failed_backfill_counts_down_and_stops_after_the_last_attempt() {
    let service = service();
    let id = ThreadId::new("thread:backfill").expect("thread id");
    service.pending_backfill.lock().insert(id.clone(), 2);

    service.fail_backfill_ids(std::slice::from_ref(&id));
    assert_eq!(service.pending_backfill.lock().get(&id).copied(), Some(1));

    service.fail_backfill_ids(std::slice::from_ref(&id));
    assert!(!service.pending_backfill.lock().contains_key(&id));
}

#[test]
fn a_finished_backfill_is_removed_without_counting_down() {
    let service = service();
    let id = ThreadId::new("thread:backfill").expect("thread id");
    service
        .pending_backfill
        .lock()
        .insert(id.clone(), BACKFILL_ATTEMPTS);

    service.finish_backfill_ids(std::slice::from_ref(&id));

    assert!(service.pending_backfill.lock().is_empty());
}

// ---- Repository identity and discovery ----

const WORKSPACE: &str = "/workspace/project";
const CANONICAL: &str = "github.com/pingdotgg/t3code";

fn identity() -> RepositoryIdentity {
    serde_json::from_value(json!({
        "canonicalKey": CANONICAL,
        "locator": {
            "source": "git-remote",
            "remoteName": "origin",
            "remoteUrl": "git@github.com:pingdotgg/t3code.git",
        },
        "provider": "github",
        "displayName": "pingdotgg/t3code",
        "owner": "pingdotgg",
        "name": "t3code",
    }))
    .expect("identity fixture decodes")
}

/// `resolver.resolve` fake: records each call's refresh flag and answers with one identity.
struct FakeIdentities {
    answer: Option<RepositoryIdentity>,
    refreshes: Mutex<Vec<bool>>,
}

#[async_trait]
impl RepositoryIdentityResolver for FakeIdentities {
    async fn resolve(&self, _: &str, refresh: bool) -> Option<RepositoryIdentity> {
        self.refreshes.lock().push(refresh);
        self.answer.clone()
    }
}

struct FakeProjects(Vec<OrchestrationProjectShell>);

#[async_trait]
impl ProjectShells for FakeProjects {
    async fn list(&self) -> Result<Vec<OrchestrationProjectShell>, BoxError> {
        Ok(self.0.clone())
    }

    async fn get(&self, id: &ProjectId) -> Result<Option<OrchestrationProjectShell>, BoxError> {
        Ok(self.0.iter().find(|project| &project.id == id).cloned())
    }
}

/// `GitManager.branchPullRequest` fake: the same answer on every lookup.
struct FakeGit(Option<GitBranchPullRequest>);

#[async_trait]
impl BranchPullRequests for FakeGit {
    async fn branch_pull_request(
        &self,
        _: &str,
        _: &str,
        _: bool,
    ) -> Result<Option<GitBranchPullRequest>, BoxError> {
        Ok(self.0.clone())
    }
}

/// An open pull request on `repository_key`, numbered `number`, as the branch lookup reports it.
fn branch_pull_request(repository_key: &str, number: u64) -> GitBranchPullRequest {
    GitBranchPullRequest {
        change_request: serde_json::from_value(json!({
            "number": number,
            "title": "Ship it",
            "url": format!("https://github.com/pingdotgg/t3code/pull/{number}"),
            "baseRef": "main",
            "headRef": "feature/pr",
            "state": "open",
        }))
        .expect("change request fixture decodes"),
        repository_key: Some(repository_key.to_owned()),
        closed_at: None,
        merged_at: None,
        head_sha: None,
    }
}

/// A thread on `feature/pr` in the `project:test` project, with no worktree. The event payload
/// fixture lacks the run and message fields a shell carries, so they are added here.
fn branch_thread(id: &str) -> OrchestrationV2ThreadShell {
    let mut value = thread_json(id);
    let shell_fields = json!({
        "branch": "feature/pr",
        "status": "idle",
        "activeRunId": null,
        "latestRunId": null,
        "latestVisibleMessage": null,
        "latestUserMessageAt": null,
        "hasActionableProposedPlan": false,
        "itemCount": 0,
        "visibleItemCount": 0,
        "pendingRuntimeRequest": null,
        "pendingBackgroundTasks": [],
        "providerInstanceHistory": [],
    });
    value
        .as_object_mut()
        .expect("thread fixture is an object")
        .extend(
            shell_fields
                .as_object()
                .expect("shell fields are an object")
                .clone(),
        );
    serde_json::from_value(value).expect("thread shell fixture decodes")
}

fn discovery_service(
    orchestrator: Arc<MockOrchestrator>,
    answer: Option<RepositoryIdentity>,
    detected: Option<GitBranchPullRequest>,
) -> (ThreadPullRequestService, Arc<FakeIdentities>) {
    let identities = Arc::new(FakeIdentities {
        answer,
        refreshes: Mutex::new(Vec::new()),
    });
    let service = ThreadPullRequestService::new(
        orchestrator,
        Arc::new(FakeProjects(vec![project_shell(WORKSPACE)])),
        Arc::new(FakeGit(detected)),
        Arc::new(NoSummaries),
        identities.clone(),
    );
    (service, identities)
}

fn scripted_snapshot(
    threads: Vec<OrchestrationV2ThreadShell>,
) -> OrchestrationV2ThreadShellSnapshot {
    serde_json::from_value(json!({
        "schemaVersion": 2,
        "snapshotSequence": 1,
        "threads": threads,
        "archivedThreads": [],
    }))
    .expect("snapshot fixture decodes")
}

/// The branch pull requests recorded by `thread.pull-request.sync` dispatches, in order.
fn recorded_branch_pull_requests(
    orchestrator: &MockOrchestrator,
) -> Vec<Option<ThreadLinkedPullRequest>> {
    orchestrator
        .dispatched
        .lock()
        .iter()
        .filter_map(|command| match command {
            OrchestrationV2ServerCommand::Client(
                OrchestrationV2Command::ThreadPullRequestSync {
                    branch_pull_request,
                    ..
                },
            ) => Some(branch_pull_request.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_project_shell_without_enrichment_resolves_its_repository() {
    // Upstream ThreadPullRequestService.test.ts: 'discovers a repository from a project shell
    // without enrichment'.
    let project = project_shell(WORKSPACE);
    assert_eq!(project.repository_identity, Some(None));
    let identities = FakeIdentities {
        answer: Some(identity()),
        refreshes: Mutex::new(Vec::new()),
    };

    let (resolved, repository) = resolve_project_for_discovery(&project, &identities, false).await;

    assert_eq!(repository.as_deref(), Some("pingdotgg/t3code"));
    let resolved_identity = resolved
        .repository_identity
        .as_ref()
        .and_then(Option::as_ref)
        .expect("the resolved project carries the identity");
    assert_eq!(resolved_identity.canonical_key.as_str(), CANONICAL);
    assert!(
        pull_request_matches_project(&branch_pull_request(CANONICAL, 7), &resolved),
        "a pull request in the project's repository matches once resolved"
    );
}

#[tokio::test]
async fn the_identity_is_refreshed_only_when_asked() {
    // Upstream ThreadPullRequestService.test.ts: 'refreshes the cached repository identity only
    // when asked'.
    let project = project_shell(WORKSPACE);
    let identities = FakeIdentities {
        answer: None,
        refreshes: Mutex::new(Vec::new()),
    };

    resolve_project_for_discovery(&project, &identities, false).await;
    resolve_project_for_discovery(&project, &identities, true).await;

    assert_eq!(identities.refreshes.lock().as_slice(), &[false, true]);
}

#[tokio::test]
async fn a_branch_pull_request_is_recorded_for_a_shell_without_enrichment() {
    let orchestrator = Arc::new(MockOrchestrator::default());
    *orchestrator.shell_snapshot.lock() = Some(scripted_snapshot(vec![branch_thread("thread:pr")]));
    let (service, _) = discovery_service(
        orchestrator.clone(),
        Some(identity()),
        Some(branch_pull_request(CANONICAL, 7)),
    );

    service
        .refresh(ThreadPullRequestService::periodic_request())
        .await;

    let recorded = recorded_branch_pull_requests(&orchestrator);
    assert_eq!(
        recorded.len(),
        1,
        "the detected pull request is recorded once"
    );
    let linked = recorded[0]
        .as_ref()
        .expect("a branch pull request is linked");
    assert_eq!(linked.number.get(), 7);
    assert_eq!(linked.repository.as_str(), "pingdotgg/t3code");
}

#[tokio::test]
async fn a_branch_pull_request_in_another_repository_is_not_recorded() {
    let orchestrator = Arc::new(MockOrchestrator::default());
    *orchestrator.shell_snapshot.lock() = Some(scripted_snapshot(vec![branch_thread("thread:pr")]));
    let (service, _) = discovery_service(
        orchestrator.clone(),
        Some(identity()),
        Some(branch_pull_request("github.com/someone/fork", 9)),
    );

    service
        .refresh(ThreadPullRequestService::periodic_request())
        .await;

    assert!(recorded_branch_pull_requests(&orchestrator).is_empty());
}

#[tokio::test]
async fn a_branch_without_a_resolvable_repository_records_nothing() {
    let orchestrator = Arc::new(MockOrchestrator::default());
    *orchestrator.shell_snapshot.lock() = Some(scripted_snapshot(vec![branch_thread("thread:pr")]));
    let (service, _) = discovery_service(
        orchestrator.clone(),
        None,
        Some(branch_pull_request(CANONICAL, 7)),
    );

    service
        .refresh(ThreadPullRequestService::periodic_request())
        .await;

    assert!(recorded_branch_pull_requests(&orchestrator).is_empty());
}
