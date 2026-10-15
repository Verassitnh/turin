//! Tests for [`ThreadPullRequestService`]. Port of the guard and read-path cases in
//! ThreadPullRequestService.test.ts. The discovery integration (repository identity, provider
//! lookups, dispatch) needs fixtures for the project shell and identity, and is not covered here.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use turin_contracts::orchestration_project::OrchestrationProjectShell;
use turin_contracts::orchestration_v2::OrchestrationV2DomainEvent;
use turin_contracts::pull_request::{PullRequestRef, PullRequestSummary};
use turin_contracts::thread_pull_request::ThreadLinkedPullRequest;
use turin_contracts::{ProjectId, ThreadId};
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
    async fn resolve(&self, _: &str) -> Option<turin_contracts::environment::RepositoryIdentity> {
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
