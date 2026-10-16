//! Behaviour tests for [`CheckpointService`]. Port of CheckpointService.test.ts
//! (lookup failure reads as `missing`) plus the capture, restore and stale-ref
//! paths, run against an in-memory git fake.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use turin_contracts::base_schemas::CheckpointRef;
use turin_contracts::orchestration_v2::{
    OrchestrationV2CheckpointScope, OrchestrationV2CheckpointStatus,
};
use turin_contracts::vcs::{VcsError, VcsProcessTimeoutError};
use turin_contracts::{NodeId, PositiveInt, ProviderThreadId, RunId, ThreadId, UtcDateTime};

use super::*;
use crate::reactors::checkpoint_git::CheckpointGit;

const CWD: &str = "/repo";
const NUMSTAT: &str = concat!("3\t1\tsrc/a.ts\0", "5\t0\tvendor/x.ts\0");

fn utc(value: &str) -> UtcDateTime {
    value.parse().expect("fixture timestamp parses")
}

fn timeout_error(operation: &str) -> VcsError {
    VcsError::Timeout(VcsProcessTimeoutError::VcsProcessTimeoutError {
        operation: operation.to_owned(),
        command: "git".to_owned(),
        cwd: CWD.to_owned(),
        argument_count: None,
        timeout_ms: 30_000.0,
    })
}

fn scope() -> OrchestrationV2CheckpointScope {
    OrchestrationV2CheckpointScope {
        id: CheckpointScopeId::new("checkpoint-scope:test").expect("scope id"),
        thread_id: ThreadId::new("thread:test").expect("thread id"),
        run_id: Some(RunId::new("run:test:3").expect("run id")),
        node_id: NodeId::new("node:test:3").expect("node id"),
        parent_scope_id: None,
        provider_thread_id: Some(ProviderThreadId::new("provider-thread:test").expect("pt id")),
        kind: OrchestrationV2CheckpointScopeKind::RootRun,
        ordinal_within_parent: 0,
        advances_app_run_count: true,
        cwd: TrimmedNonEmptyString::new(CWD).expect("cwd"),
        created_at: utc("2026-07-28T00:00:00.000Z"),
    }
}

/// In-memory stand-in for the git layer. Refs are a set; the diff and the
/// authored-path answers are fixed per test.
struct FakeGit {
    repository: bool,
    refs: Mutex<HashSet<String>>,
    has_ref_error: Option<VcsError>,
    capture_error: Option<VcsError>,
    restore_result: bool,
    numstat: String,
    authored: Option<HashSet<String>>,
    calls: Mutex<Vec<String>>,
}

impl FakeGit {
    fn new() -> Self {
        Self {
            repository: true,
            refs: Mutex::new(HashSet::new()),
            has_ref_error: None,
            capture_error: None,
            restore_result: true,
            numstat: String::new(),
            authored: None,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn with_ref(self, checkpoint_ref: &CheckpointRef) -> Self {
        self.refs.lock().insert(checkpoint_ref.as_str().to_owned());
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().clone()
    }
}

#[async_trait]
impl CheckpointGit for FakeGit {
    async fn is_git_repository(&self, _cwd: &str) -> Result<bool, VcsError> {
        Ok(self.repository)
    }

    async fn capture_checkpoint(
        &self,
        _cwd: &str,
        checkpoint_ref: &CheckpointRef,
    ) -> Result<(), VcsError> {
        self.calls
            .lock()
            .push(format!("capture:{}", checkpoint_ref.as_str()));
        if let Some(error) = &self.capture_error {
            return Err(error.clone());
        }
        self.refs.lock().insert(checkpoint_ref.as_str().to_owned());
        Ok(())
    }

    async fn has_checkpoint_ref(
        &self,
        _cwd: &str,
        checkpoint_ref: &CheckpointRef,
    ) -> Result<bool, VcsError> {
        self.calls
            .lock()
            .push(format!("has:{}", checkpoint_ref.as_str()));
        if let Some(error) = &self.has_ref_error {
            return Err(error.clone());
        }
        Ok(self.refs.lock().contains(checkpoint_ref.as_str()))
    }

    async fn restore_checkpoint(
        &self,
        _cwd: &str,
        checkpoint_ref: &CheckpointRef,
        fallback_to_head: bool,
    ) -> Result<bool, VcsError> {
        self.calls.lock().push(format!(
            "restore:{}:{fallback_to_head}",
            checkpoint_ref.as_str()
        ));
        Ok(self.restore_result)
    }

    async fn diff_numstat(
        &self,
        _cwd: &str,
        from: &CheckpointRef,
        to: &CheckpointRef,
    ) -> Result<String, VcsError> {
        self.calls
            .lock()
            .push(format!("diff:{}..{}", from.as_str(), to.as_str()));
        // Git reports nothing when both sides name the same tree (ordinal 0 diffs against itself).
        if from == to {
            return Ok(String::new());
        }
        Ok(self.numstat.clone())
    }

    async fn list_authored_paths(
        &self,
        _cwd: &str,
        _from: &CheckpointRef,
        _to: &CheckpointRef,
    ) -> Result<Option<HashSet<String>>, VcsError> {
        Ok(self.authored.clone())
    }

    async fn delete_checkpoint_refs(
        &self,
        _cwd: &str,
        checkpoint_refs: Vec<CheckpointRef>,
    ) -> Result<(), VcsError> {
        let mut refs = self.refs.lock();
        for checkpoint_ref in checkpoint_refs {
            refs.remove(checkpoint_ref.as_str());
        }
        Ok(())
    }
}

fn service(git: FakeGit) -> (CheckpointService, Arc<FakeGit>) {
    let git = Arc::new(git);
    (CheckpointService::new(git.clone()), git)
}

fn capture_input(scope: &OrchestrationV2CheckpointScope, ordinal: u64) -> CaptureInput<'_> {
    CaptureInput {
        scope,
        run_id: scope.run_id.clone(),
        node_id: scope.node_id.clone(),
        ordinal_within_scope: ordinal,
        app_run_ordinal: PositiveInt::new(1),
        captured_at: scope.created_at,
    }
}

#[test]
fn checkpoint_ref_uses_the_orchestration_v2_prefix_and_is_stable() {
    let scope_id = CheckpointScopeId::new("checkpoint-scope:test").expect("scope id");
    let first = checkpoint_ref_for_scope_ordinal(&scope_id, 2);
    let second = checkpoint_ref_for_scope_ordinal(&scope_id, 2);
    assert_eq!(first, second);
    assert!(
        first
            .as_str()
            .starts_with("refs/t3/orchestration-v2/checkpoints/")
    );
    assert!(first.as_str().ends_with("/ordinal/2"));
    // The key is base64url of the first 32 hex characters of the digest.
    let key = first
        .as_str()
        .trim_start_matches("refs/t3/orchestration-v2/checkpoints/")
        .trim_end_matches("/ordinal/2");
    assert_eq!(key.len(), 43, "base64url of 32 ASCII bytes is 43 chars");
}

#[test]
fn prepare_root_run_scope_is_a_root_run_with_no_parent() {
    let (checkpoints, _) = service(FakeGit::new());
    let scope = checkpoints
        .prepare_root_run_scope(PrepareRootRunScope {
            thread_id: ThreadId::new("thread:test").expect("thread id"),
            run_id: RunId::new("run:test:3").expect("run id"),
            root_node_id: NodeId::new("node:test:3").expect("node id"),
            provider_thread_id: ProviderThreadId::new("provider-thread:test").expect("pt id"),
            cwd: CWD.to_owned(),
            created_at: utc("2026-07-28T00:00:00.000Z"),
        })
        .expect("scope prepares");
    assert_eq!(scope.kind, OrchestrationV2CheckpointScopeKind::RootRun);
    assert_eq!(scope.parent_scope_id, None);
    assert_eq!(scope.ordinal_within_parent, 0);
    assert!(scope.advances_app_run_count);
    assert_eq!(scope.cwd.as_str(), CWD);
}

#[tokio::test]
async fn materialized_baseline_is_ready_when_the_ref_exists() {
    let target = scope();
    let ref_ = checkpoint_ref_for_scope_ordinal(&target.id, 2);
    let (checkpoints, git) = service(FakeGit::new().with_ref(&ref_));

    let baseline = checkpoints
        .materialize_baseline_checkpoint(&target, 2)
        .await
        .expect("materializes");

    assert_eq!(baseline.ordinal_within_scope, 2);
    assert_eq!(baseline.ref_, ref_);
    assert_eq!(baseline.status, OrchestrationV2CheckpointStatus::Ready);
    assert!(baseline.files.is_empty());
    assert_eq!(baseline.captured_at, target.created_at);
    assert_eq!(git.calls(), vec![format!("has:{}", ref_.as_str())]);
}

#[tokio::test]
async fn a_failed_ref_lookup_materializes_as_missing() {
    let target = scope();
    let mut fake = FakeGit::new();
    fake.has_ref_error = Some(timeout_error("test.ref"));
    let (checkpoints, _) = service(fake);

    let baseline = checkpoints
        .materialize_baseline_checkpoint(&target, 2)
        .await
        .expect("a lookup failure is not an error");

    assert_eq!(baseline.status, OrchestrationV2CheckpointStatus::Missing);
}

#[tokio::test]
async fn a_non_repository_materializes_as_missing_without_a_lookup() {
    let target = scope();
    let mut fake = FakeGit::new();
    fake.repository = false;
    let (checkpoints, git) = service(fake);

    let baseline = checkpoints
        .materialize_baseline_checkpoint(&target, 0)
        .await
        .expect("materializes");

    assert_eq!(baseline.status, OrchestrationV2CheckpointStatus::Missing);
    assert!(git.calls().is_empty());
}

#[tokio::test]
async fn capture_baseline_skips_an_existing_ref() {
    let target = scope();
    let ref_ = checkpoint_ref_for_scope_ordinal(&target.id, 0);
    let (checkpoints, git) = service(FakeGit::new().with_ref(&ref_));

    checkpoints
        .capture_baseline(&target, 0)
        .await
        .expect("baseline");

    assert_eq!(git.calls(), vec![format!("has:{}", ref_.as_str())]);
}

#[tokio::test]
async fn capture_baseline_writes_a_missing_ref() {
    let target = scope();
    let ref_ = checkpoint_ref_for_scope_ordinal(&target.id, 0);
    let (checkpoints, git) = service(FakeGit::new());

    checkpoints
        .capture_baseline(&target, 0)
        .await
        .expect("baseline");

    assert_eq!(
        git.calls(),
        vec![
            format!("has:{}", ref_.as_str()),
            format!("capture:{}", ref_.as_str()),
        ]
    );
}

#[tokio::test]
async fn capture_without_a_previous_checkpoint_reports_no_files() {
    let target = scope();
    let mut fake = FakeGit::new();
    fake.numstat = NUMSTAT.to_owned();
    let (checkpoints, git) = service(fake);

    let checkpoint = checkpoints
        .capture(capture_input(&target, 0))
        .await
        .expect("captures");

    assert_eq!(checkpoint.status, OrchestrationV2CheckpointStatus::Ready);
    assert!(checkpoint.files.is_empty());
    assert_eq!(checkpoint.parent_checkpoint_id, None);
    assert_eq!(checkpoint.app_run_ordinal, PositiveInt::new(1));
    // Ordinal 0 has no earlier ref, so its "previous" is itself and the diff is empty.
    let same = checkpoint_ref_for_scope_ordinal(&target.id, 0);
    assert!(
        git.calls()
            .contains(&format!("diff:{0}..{0}", same.as_str()))
    );
}

#[tokio::test]
async fn capture_reports_turn_files_and_drops_imported_ones() {
    let target = scope();
    let previous = checkpoint_ref_for_scope_ordinal(&target.id, 0);
    let mut fake = FakeGit::new().with_ref(&previous);
    fake.numstat = NUMSTAT.to_owned();
    // The turn touched src/a.ts only. vendor/x.ts came in with a pull.
    fake.authored = Some(HashSet::from(["src/a.ts".to_owned()]));
    let (checkpoints, _) = service(fake);

    let checkpoint = checkpoints
        .capture(capture_input(&target, 1))
        .await
        .expect("captures");

    assert_eq!(checkpoint.status, OrchestrationV2CheckpointStatus::Ready);
    assert_eq!(checkpoint.files.len(), 1);
    assert_eq!(checkpoint.files[0].path.as_str(), "src/a.ts");
    assert_eq!(checkpoint.files[0].kind.as_str(), "modified");
    assert_eq!(checkpoint.files[0].additions, 3);
    assert_eq!(checkpoint.files[0].deletions, 1);
    assert_eq!(
        checkpoint.parent_checkpoint_id,
        Some(checkpoint_id(&target, 0))
    );
}

#[tokio::test]
async fn capture_keeps_every_file_when_head_did_not_move() {
    let target = scope();
    let previous = checkpoint_ref_for_scope_ordinal(&target.id, 0);
    let mut fake = FakeGit::new().with_ref(&previous);
    fake.numstat = NUMSTAT.to_owned();
    fake.authored = None;
    let (checkpoints, _) = service(fake);

    let checkpoint = checkpoints
        .capture(capture_input(&target, 1))
        .await
        .expect("captures");

    assert_eq!(checkpoint.files.len(), 2);
}

#[tokio::test]
async fn a_failed_capture_is_recorded_as_error() {
    let target = scope();
    let mut fake = FakeGit::new();
    fake.capture_error = Some(timeout_error("test.capture"));
    let (checkpoints, _) = service(fake);

    let checkpoint = checkpoints
        .capture(capture_input(&target, 0))
        .await
        .expect("a failed capture is a status, not an error");

    assert_eq!(checkpoint.status, OrchestrationV2CheckpointStatus::Error);
    assert!(checkpoint.files.is_empty());
}

#[tokio::test]
async fn restore_fails_for_a_checkpoint_that_is_not_ready() {
    let target = scope();
    let (checkpoints, git) = service(FakeGit::new());
    let mut checkpoint = checkpoints
        .materialize_baseline_checkpoint(&target, 0)
        .await
        .expect("materializes");
    checkpoint.status = OrchestrationV2CheckpointStatus::Missing;

    let error = checkpoints
        .restore(&target, &checkpoint)
        .await
        .expect_err("missing checkpoints do not restore");

    assert_eq!(
        error,
        CheckpointServiceError::Restore {
            scope_id: target.id.clone(),
            checkpoint_id: checkpoint.id.clone(),
            cause: "Checkpoint status is missing.".to_owned(),
        }
    );
    assert!(git.calls().iter().all(|call| !call.starts_with("restore:")));
}

#[tokio::test]
async fn restore_never_falls_back_to_head() {
    let target = scope();
    let ref_ = checkpoint_ref_for_scope_ordinal(&target.id, 0);
    let (checkpoints, git) = service(FakeGit::new().with_ref(&ref_));
    let mut checkpoint = checkpoints
        .materialize_baseline_checkpoint(&target, 0)
        .await
        .expect("materializes");
    checkpoint.status = OrchestrationV2CheckpointStatus::Ready;

    checkpoints
        .restore(&target, &checkpoint)
        .await
        .expect("restores");

    assert_eq!(
        git.calls().last(),
        Some(&format!("restore:{}:false", ref_.as_str()))
    );
}

#[tokio::test]
async fn restore_reports_an_unavailable_ref() {
    let target = scope();
    let mut fake = FakeGit::new();
    fake.restore_result = false;
    let (checkpoints, _) = service(fake);
    let mut checkpoint = checkpoints
        .materialize_baseline_checkpoint(&target, 0)
        .await
        .expect("materializes");
    checkpoint.status = OrchestrationV2CheckpointStatus::Ready;

    let error = checkpoints
        .restore(&target, &checkpoint)
        .await
        .expect_err("a missing ref fails the restore");

    assert!(matches!(
        error,
        CheckpointServiceError::Restore { ref cause, .. } if cause == "Checkpoint ref is unavailable."
    ));
}

#[tokio::test]
async fn delete_stale_refs_removes_each_checkpoint_ref() {
    let target = scope();
    let ref_ = checkpoint_ref_for_scope_ordinal(&target.id, 0);
    let (checkpoints, git) = service(FakeGit::new().with_ref(&ref_));
    let checkpoint = checkpoints
        .materialize_baseline_checkpoint(&target, 0)
        .await
        .expect("materializes");
    assert!(git.refs.lock().contains(ref_.as_str()));

    checkpoints
        .delete_stale_refs(&target, std::slice::from_ref(&checkpoint))
        .await
        .expect("deletes");

    assert!(!git.refs.lock().contains(ref_.as_str()));
}

fn checkpoint_id(target: &OrchestrationV2CheckpointScope, ordinal: u64) -> CheckpointId {
    IdAllocator.checkpoint(&target.id, &ordinal.to_string())
}
