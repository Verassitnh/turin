//! Behaviour tests for [`is_checkpoint_restore_isolated`], over real temp
//! directories so the realpath and containment logic is exercised.

use async_trait::async_trait;
use turin_contracts::ThreadId;

use super::*;

struct FakeClaims(Vec<OtherWorkspaceClaim>);

#[async_trait]
impl WorkspaceClaimReads for FakeClaims {
    async fn other_thread_claims(
        &self,
        _thread_id: &ThreadId,
    ) -> Result<Vec<OtherWorkspaceClaim>, BoxError> {
        Ok(self.0.clone())
    }
}

fn thread() -> ThreadId {
    ThreadId::new("thread:self").expect("thread id")
}

/// A canonical root with a worktree directory and a sibling directory.
struct Layout {
    _dir: tempfile::TempDir,
    worktree: String,
    sibling: String,
    root: String,
}

fn layout() -> Layout {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical root");
    let worktree = root.join("worktree");
    let sibling = root.join("sibling");
    std::fs::create_dir_all(&worktree).expect("worktree");
    std::fs::create_dir_all(&sibling).expect("sibling");
    Layout {
        _dir: dir,
        worktree: worktree.to_str().expect("utf-8").to_owned(),
        sibling: sibling.to_str().expect("utf-8").to_owned(),
        root: root.to_str().expect("utf-8").to_owned(),
    }
}

#[tokio::test]
async fn a_thread_without_a_worktree_is_never_isolated() {
    let layout = layout();
    let reads = FakeClaims(Vec::new());
    let isolated = is_checkpoint_restore_isolated(&thread(), None, &layout.root, &reads)
        .await
        .expect("check runs");
    assert!(!isolated);
}

#[tokio::test]
async fn a_worktree_with_no_other_claims_is_isolated() {
    let layout = layout();
    let reads = FakeClaims(Vec::new());
    let isolated =
        is_checkpoint_restore_isolated(&thread(), Some(&layout.worktree), &layout.worktree, &reads)
            .await
            .expect("check runs");
    assert!(isolated);
}

#[tokio::test]
async fn a_scope_cwd_that_differs_from_the_worktree_is_not_isolated() {
    let layout = layout();
    let reads = FakeClaims(Vec::new());
    let isolated =
        is_checkpoint_restore_isolated(&thread(), Some(&layout.worktree), &layout.sibling, &reads)
            .await
            .expect("check runs");
    assert!(!isolated);
}

#[tokio::test]
async fn another_thread_on_the_same_worktree_shares_it() {
    let layout = layout();
    let reads = FakeClaims(vec![OtherWorkspaceClaim {
        paths: vec![layout.worktree.clone()],
        project_root_missing: false,
    }]);
    let isolated =
        is_checkpoint_restore_isolated(&thread(), Some(&layout.worktree), &layout.worktree, &reads)
            .await
            .expect("check runs");
    assert!(!isolated);
}

#[tokio::test]
async fn another_thread_rooted_above_the_worktree_shares_it() {
    let layout = layout();
    let reads = FakeClaims(vec![OtherWorkspaceClaim {
        paths: vec![layout.root.clone()],
        project_root_missing: false,
    }]);
    let isolated =
        is_checkpoint_restore_isolated(&thread(), Some(&layout.worktree), &layout.worktree, &reads)
            .await
            .expect("check runs");
    assert!(!isolated);
}

#[tokio::test]
async fn a_sibling_checkout_does_not_share_the_worktree() {
    let layout = layout();
    let reads = FakeClaims(vec![OtherWorkspaceClaim {
        paths: vec![layout.sibling.clone()],
        project_root_missing: false,
    }]);
    let isolated =
        is_checkpoint_restore_isolated(&thread(), Some(&layout.worktree), &layout.worktree, &reads)
            .await
            .expect("check runs");
    assert!(isolated);
}

#[tokio::test]
async fn a_missing_project_record_refuses_the_restore() {
    let layout = layout();
    let reads = FakeClaims(vec![OtherWorkspaceClaim {
        paths: Vec::new(),
        project_root_missing: true,
    }]);
    let isolated =
        is_checkpoint_restore_isolated(&thread(), Some(&layout.worktree), &layout.worktree, &reads)
            .await
            .expect("check runs");
    assert!(!isolated);
}

#[tokio::test]
async fn a_claimed_path_that_no_longer_exists_is_skipped() {
    let layout = layout();
    let gone = format!("{}/gone", layout.root);
    let reads = FakeClaims(vec![OtherWorkspaceClaim {
        paths: vec![gone],
        project_root_missing: false,
    }]);
    let isolated =
        is_checkpoint_restore_isolated(&thread(), Some(&layout.worktree), &layout.worktree, &reads)
            .await
            .expect("check runs");
    assert!(isolated);
}
