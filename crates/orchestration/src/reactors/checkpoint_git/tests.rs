//! Round trip through turin-git's real checkpoint store: baseline, a turn's
//! edit, capture with the file summary, then restore. Uses a throwaway repo
//! in a temp dir, so it needs the `git` binary on PATH.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use turin_contracts::NodeId;
use turin_contracts::orchestration_v2::{
    OrchestrationV2CheckpointScope, OrchestrationV2CheckpointScopeKind,
    OrchestrationV2CheckpointStatus,
};
use turin_contracts::{CheckpointScopeId, ProviderThreadId, RunId, ThreadId, UtcDateTime};
use turin_git::git_vcs_driver::GitVcsDriver;
use turin_git::process::ProcessRunner;
use turin_git::vcs_process::VcsProcess;

use crate::reactors::checkpoint_git::TurinCheckpointGit;
use crate::reactors::checkpoint_service::{CaptureInput, CheckpointService};
use turin_contracts::PositiveInt;
use turin_contracts::base_schemas::TrimmedNonEmptyString;

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
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

fn scope(cwd: &str) -> OrchestrationV2CheckpointScope {
    OrchestrationV2CheckpointScope {
        id: CheckpointScopeId::new("checkpoint-scope:real-git").expect("scope id"),
        thread_id: ThreadId::new("thread:real-git").expect("thread id"),
        run_id: Some(RunId::new("run:real-git:1").expect("run id")),
        node_id: NodeId::new("node:real-git:1").expect("node id"),
        parent_scope_id: None,
        provider_thread_id: Some(ProviderThreadId::new("provider-thread:real-git").expect("pt")),
        kind: OrchestrationV2CheckpointScopeKind::RootRun,
        ordinal_within_parent: 0,
        advances_app_run_count: true,
        cwd: TrimmedNonEmptyString::new(cwd).expect("cwd"),
        created_at: "2026-07-28T00:00:00.000Z"
            .parse::<UtcDateTime>()
            .expect("timestamp"),
    }
}

#[tokio::test]
async fn capture_reports_the_turn_edit_and_restore_brings_back_the_baseline() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = dir.path();
    git(repo, &["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "one\n").expect("write");
    git(repo, &["add", "a.txt"]);
    git(repo, &["commit", "-q", "-m", "init"]);
    let cwd = repo.to_str().expect("utf-8 temp path").to_owned();

    let driver = Arc::new(GitVcsDriver::new(VcsProcess::new(ProcessRunner)));
    let service = CheckpointService::new(Arc::new(TurinCheckpointGit::new(driver)));
    let target = scope(&cwd);

    // Ordinal 0 is the pre-run baseline, taken before the turn edits anything.
    service
        .capture_baseline(&target, 0)
        .await
        .expect("baseline captures");
    let baseline = service
        .materialize_baseline_checkpoint(&target, 0)
        .await
        .expect("baseline materializes");
    assert_eq!(baseline.status, OrchestrationV2CheckpointStatus::Ready);

    // The turn edits a.txt, then the post-turn capture runs.
    std::fs::write(repo.join("a.txt"), "two\nthree\n").expect("write");
    let captured = service
        .capture(CaptureInput {
            scope: &target,
            run_id: target.run_id.clone(),
            node_id: target.node_id.clone(),
            ordinal_within_scope: 1,
            app_run_ordinal: PositiveInt::new(1),
            captured_at: target.created_at,
        })
        .await
        .expect("capture");
    assert_eq!(captured.status, OrchestrationV2CheckpointStatus::Ready);
    assert_eq!(captured.files.len(), 1);
    assert_eq!(captured.files[0].path.as_str(), "a.txt");
    assert_eq!(captured.files[0].additions, 2);
    assert_eq!(captured.files[0].deletions, 1);

    // The user keeps editing, then rewinds files to the baseline.
    std::fs::write(repo.join("a.txt"), "scratch\n").expect("write");
    service.restore(&target, &baseline).await.expect("restore");
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).expect("read"),
        "one\n"
    );
}
