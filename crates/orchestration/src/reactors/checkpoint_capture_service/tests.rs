//! Guard tests for [`plan_capture`]: the branches of `CheckpointCaptureService.execute`
//! before any write. The commit itself runs against the projection and event sink.

use turin_contracts::CheckpointId;

use super::*;
use crate::pipeline::test_support::run;

fn waiting_run() -> OrchestrationV2Run {
    run("run:capture", "thread:capture", 2, "waiting")
}

fn with_checkpoint(mut run: OrchestrationV2Run) -> OrchestrationV2Run {
    run.checkpoint_id = Some(CheckpointId::new("checkpoint:existing").expect("checkpoint id"));
    run
}

#[test]
fn a_waiting_run_with_its_root_scope_and_provider_thread_captures() {
    let plan = plan_capture(Some(&waiting_run()), true, true);
    assert_eq!(plan, CapturePlan::Capture { stopped: false });
}

#[test]
fn a_completed_run_that_names_a_checkpoint_is_skipped() {
    let completed = with_checkpoint(run("run:capture", "thread:capture", 2, "completed"));
    assert_eq!(
        plan_capture(Some(&completed), true, true),
        CapturePlan::Skip
    );
}

#[test]
fn a_completed_run_without_a_checkpoint_is_not_waiting_so_it_is_invalid() {
    let completed = run("run:capture", "thread:capture", 2, "completed");
    assert_eq!(
        plan_capture(Some(&completed), true, true),
        CapturePlan::Invalid
    );
}

#[test]
fn a_rolled_back_run_is_skipped_even_though_it_has_no_checkpoint() {
    let rolled_back = run("run:capture", "thread:capture", 2, "rolled_back");
    assert_eq!(
        plan_capture(Some(&rolled_back), true, true),
        CapturePlan::Skip
    );
}

#[test]
fn an_interrupted_run_captures_without_changing_its_status_and_skips_once_checkpointed() {
    let interrupted = run("run:capture", "thread:capture", 2, "interrupted");
    assert_eq!(
        plan_capture(Some(&interrupted), true, true),
        CapturePlan::Capture { stopped: true }
    );
    assert_eq!(
        plan_capture(Some(&with_checkpoint(interrupted)), true, true),
        CapturePlan::Skip
    );
}

#[test]
fn a_cancelled_run_is_treated_as_stopped() {
    let cancelled = run("run:capture", "thread:capture", 2, "cancelled");
    assert_eq!(
        plan_capture(Some(&cancelled), true, true),
        CapturePlan::Capture { stopped: true }
    );
}

#[test]
fn a_root_node_on_another_scope_is_invalid() {
    assert_eq!(
        plan_capture(Some(&waiting_run()), false, true),
        CapturePlan::Invalid
    );
}

#[test]
fn a_missing_provider_thread_is_invalid() {
    assert_eq!(
        plan_capture(Some(&waiting_run()), true, false),
        CapturePlan::Invalid
    );
}

#[test]
fn a_missing_run_is_invalid() {
    assert_eq!(plan_capture(None, true, true), CapturePlan::Invalid);
}

// ---------------------------------------------------------------------------
// Capture-and-commit against a real projection and event sink.
//
// The fixture writes thread, provider thread, run, root node and checkpoint scope through
// domain events, then runs `execute` on a real git repo. The sink's projection applier is the
// test adapter below: it folds events into the projection and keeps no other state.

use std::sync::Arc;

use serde_json::{Value, json};
use turin_persistence::{Db, OrchestrationEventStore, PersistenceError};

use crate::pipeline::effect_outbox::EffectOutbox;
use crate::pipeline::event_sink::{EventSink, ProjectionApplier};
use crate::projection::store::ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION;
use crate::projection::store::ProjectionStore;
use crate::projection::test_support::{apply_all, event, run as run_json, thread as thread_json};
use crate::reactors::checkpoint_git::TurinCheckpointGit;
use crate::reactors::checkpoint_service::CheckpointService;
use turin_contracts::orchestration_v2::{
    OrchestrationV2CheckpointStatus, OrchestrationV2DomainEvent, OrchestrationV2TurnItem,
};
use turin_git::git_vcs_driver::GitVcsDriver;
use turin_git::process::ProcessRunner;
use turin_git::vcs_process::VcsProcess;

const T0: &str = "2026-01-01T00:00:01.000Z";
const THREAD: &str = "thread:capture-store";
const RUN: &str = "run:capture-store";
const ROOT: &str = "node:capture-store:root";
const SCOPE: &str = "checkpoint-scope:capture-store";
const PROVIDER_THREAD: &str = "provider-thread:capture-store";
const FILE: &str = "tracked.txt";

/// Folds sink writes into the projection, in the sink's transaction.
struct ProjectionAdapter;

impl ProjectionApplier for ProjectionAdapter {
    fn normalize_turn_item(
        &self,
        _tx: &rusqlite::Transaction<'_>,
        event: OrchestrationV2DomainEvent,
        _run_ordinal: Option<u64>,
    ) -> Result<OrchestrationV2DomainEvent, PersistenceError> {
        Ok(event)
    }

    fn apply_event(
        &self,
        tx: &rusqlite::Transaction<'_>,
        event: &OrchestrationV2DomainEvent,
    ) -> Result<(), PersistenceError> {
        crate::projection::store::apply::apply_event(tx, event).map_err(|error| {
            PersistenceError::sql(
                "test.projection.apply",
                &rusqlite::Error::InvalidParameterName(error.to_string()),
            )
        })
    }

    fn apply_project_event(
        &self,
        _tx: &rusqlite::Transaction<'_>,
        _event: &turin_persistence::ProjectEvent,
    ) -> Result<(), PersistenceError> {
        Ok(())
    }

    fn schema_version(&self) -> i64 {
        ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION as i64
    }
}

fn patch(mut base: Value, fields: Value) -> Value {
    for (key, value) in fields.as_object().cloned().unwrap_or_default() {
        base[key] = value;
    }
    base
}

fn git(repo: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t13d")
        .env("GIT_AUTHOR_EMAIL", "t13d@example.invalid")
        .env("GIT_COMMITTER_NAME", "t13d")
        .env("GIT_COMMITTER_EMAIL", "t13d@example.invalid")
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
}

struct Fixture {
    _repo: tempfile::TempDir,
    cwd: String,
    store: ProjectionStore,
    service: CheckpointCaptureService,
}

/// Seeds a waiting-or-stopped run whose root node names the scope, on a real repo whose
/// baseline (ordinal 0) is already captured, as it is at turn start.
async fn fixture(run_status: &str, node_status: &str) -> Fixture {
    let repo = tempfile::tempdir().expect("temp dir");
    git(repo.path(), &["init", "-q"]);
    std::fs::write(repo.path().join(FILE), "one\n").expect("write");
    git(repo.path(), &["add", FILE]);
    git(repo.path(), &["commit", "-q", "-m", "init"]);
    let cwd = repo.path().to_str().expect("utf-8 temp path").to_owned();

    let db = Db::open_in_memory().await.expect("in-memory db");
    let store = ProjectionStore::new(db.clone());
    let sink = Arc::new(EventSink::new(
        OrchestrationEventStore::new(db.clone()),
        EffectOutbox::new(db.clone()),
        Arc::new(ProjectionAdapter),
    ));

    let provider_thread = json!({
        "id": PROVIDER_THREAD, "driver": "codex", "providerInstanceId": "codex",
        "providerSessionId": null, "appThreadId": THREAD, "ownerNodeId": null,
        "nativeThreadRef": null, "nativeConversationHeadRef": null, "status": "active",
        "firstRunOrdinal": 1, "lastRunOrdinal": 1, "handoffIds": [], "forkedFrom": null,
        "createdAt": T0, "updatedAt": T0
    });
    let scope = json!({
        "id": SCOPE, "threadId": THREAD, "runId": RUN, "nodeId": ROOT,
        "parentScopeId": null, "providerThreadId": PROVIDER_THREAD, "kind": "root_run",
        "ordinalWithinParent": 0, "advancesAppRunCount": true, "cwd": cwd, "createdAt": T0,
    });
    let run = patch(
        run_json(THREAD, RUN, 1, run_status),
        json!({
            "rootNodeId": ROOT, "providerThreadId": PROVIDER_THREAD,
            "checkpointId": null, "completedAt": null
        }),
    );
    let root_node = json!({
        "id": ROOT, "threadId": THREAD, "runId": RUN, "parentNodeId": null,
        "rootNodeId": ROOT, "kind": "root_turn", "status": node_status, "countsForRun": true,
        "providerThreadId": PROVIDER_THREAD, "providerTurnId": null, "nativeItemRef": null,
        "runtimeRequestId": null, "checkpointScopeId": SCOPE, "startedAt": T0, "completedAt": null
    });
    apply_all(
        &store,
        vec![
            event("thread.created", THREAD, T0, thread_json(THREAD)),
            event("provider-thread.updated", THREAD, T0, provider_thread),
            event("run.created", THREAD, T0, run),
            event("node.updated", THREAD, T0, root_node),
            event("checkpoint-scope.created", THREAD, T0, scope),
        ],
    )
    .await;

    let checkpoints = Arc::new(CheckpointService::new(Arc::new(TurinCheckpointGit::new(
        Arc::new(GitVcsDriver::new(VcsProcess::new(ProcessRunner))),
    ))));
    let scope_row = store
        .get_thread_projection(&ThreadId::new(THREAD).expect("thread id"))
        .await
        .expect("projection reads")
        .checkpoint_scopes
        .into_iter()
        .next()
        .expect("scope projected");
    checkpoints
        .capture_baseline(&scope_row, 0)
        .await
        .expect("baseline captured");

    let service = CheckpointCaptureService::new(
        checkpoints.clone(),
        sink,
        Arc::new(ProjectionStore::new(db)),
    );
    Fixture {
        _repo: repo,
        cwd,
        store,
        service,
    }
}

fn ids() -> (ThreadId, RunId, CheckpointScopeId) {
    (
        ThreadId::new(THREAD).expect("thread id"),
        RunId::new(RUN).expect("run id"),
        CheckpointScopeId::new(SCOPE).expect("scope id"),
    )
}

#[tokio::test]
async fn capture_commits_the_checkpoint_turn_item_and_settles_the_run() {
    let fixture = fixture("waiting", "waiting").await;
    // The turn edits the file after the baseline was taken.
    std::fs::write(
        std::path::Path::new(&fixture.cwd).join(FILE),
        "two\nthree\n",
    )
    .expect("write");
    let (thread, run, scope) = ids();

    fixture
        .service
        .execute(&thread, &run, &scope)
        .await
        .expect("capture commits");

    let projection = fixture
        .store
        .get_thread_projection(&thread)
        .await
        .expect("projection reads");
    let settled = projection
        .runs
        .iter()
        .find(|candidate| candidate.id == run)
        .expect("run projected");
    assert_eq!(settled.status, OrchestrationV2RunStatus::Completed);
    let checkpoint = projection
        .checkpoints
        .iter()
        .find(|candidate| candidate.ordinal_within_scope == 1)
        .expect("turn checkpoint stored");
    assert_eq!(settled.checkpoint_id.as_ref(), Some(&checkpoint.id));
    assert_eq!(checkpoint.status, OrchestrationV2CheckpointStatus::Ready);
    assert_eq!(checkpoint.files.len(), 1);
    assert_eq!(checkpoint.files[0].path.as_str(), FILE);

    let node = projection
        .nodes
        .iter()
        .find(|candidate| candidate.id.as_str() == ROOT)
        .expect("root node projected");
    assert_eq!(
        node.status,
        turin_contracts::orchestration_v2::OrchestrationV2ExecutionNodeStatus::Completed
    );

    let turn_item = projection
        .turn_items
        .iter()
        .find_map(|item| match item {
            OrchestrationV2TurnItem::Checkpoint {
                checkpoint_id,
                files,
                ..
            } => Some((checkpoint_id.clone(), files.len())),
            _ => None,
        })
        .expect("checkpoint turn item projected");
    assert_eq!(turn_item, (checkpoint.id.clone(), 1));
}

#[tokio::test]
async fn a_second_execute_after_commit_writes_nothing() {
    let fixture = fixture("waiting", "waiting").await;
    let (thread, run, scope) = ids();
    fixture
        .service
        .execute(&thread, &run, &scope)
        .await
        .expect("first capture");
    let before = fixture
        .store
        .get_thread_projection(&thread)
        .await
        .expect("projection reads")
        .checkpoints
        .len();

    fixture
        .service
        .execute(&thread, &run, &scope)
        .await
        .expect("replay is a no-op");

    let after = fixture
        .store
        .get_thread_projection(&thread)
        .await
        .expect("projection reads")
        .checkpoints
        .len();
    assert_eq!(before, after);
    assert_eq!(after, 2, "baseline ordinal 0 and the turn checkpoint");
}

#[tokio::test]
async fn a_stopped_run_keeps_its_status_and_its_unsettled_root_node() {
    let fixture = fixture("interrupted", "waiting").await;
    let (thread, run, scope) = ids();

    fixture
        .service
        .execute(&thread, &run, &scope)
        .await
        .expect("stopped capture commits");

    let projection = fixture
        .store
        .get_thread_projection(&thread)
        .await
        .expect("projection reads");
    let stopped = projection
        .runs
        .iter()
        .find(|candidate| candidate.id == run)
        .expect("run projected");
    assert_eq!(stopped.status, OrchestrationV2RunStatus::Interrupted);
    assert!(
        stopped.checkpoint_id.is_some(),
        "the rollback point is still recorded"
    );
    let node = projection
        .nodes
        .iter()
        .find(|candidate| candidate.id.as_str() == ROOT)
        .expect("root node projected");
    assert_eq!(
        node.status,
        turin_contracts::orchestration_v2::OrchestrationV2ExecutionNodeStatus::Waiting
    );
}
