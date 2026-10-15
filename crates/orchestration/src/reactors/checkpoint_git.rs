//! Narrow seam between the checkpoint services and turin-git's
//! `CheckpointStore` (T06, `crates/git/src/checkpointing`).
//!
//! Upstream `CheckpointService` resolves `CheckpointStore` from the layer
//! graph. Here the services take this trait so they never depend on the VCS
//! driver registry, and tests can run against an in-memory fake. The methods
//! are exactly the calls the orchestrator makes, with the fixed options
//! `CheckpointService.ts` passes (no HEAD fallback, whitespace kept, numstat).

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::base_schemas::CheckpointRef;
use turin_contracts::vcs::VcsError;
use turin_git::checkpointing::checkpoint_store::CheckpointStore;
use turin_git::vcs_driver::{
    CheckpointDiffFormat, VcsCaptureCheckpointInput, VcsDeleteCheckpointRefsInput,
    VcsDiffCheckpointsInput, VcsDriver, VcsHasCheckpointInput, VcsListAuthoredPathsInput,
    VcsRestoreCheckpointInput,
};

#[async_trait]
pub trait CheckpointGit: Send + Sync {
    /// `CheckpointStore.isGitRepository`.
    async fn is_git_repository(&self, cwd: &str) -> Result<bool, VcsError>;

    /// `CheckpointStore.captureCheckpoint`: writes a hidden ref at `checkpoint_ref`.
    async fn capture_checkpoint(
        &self,
        cwd: &str,
        checkpoint_ref: &CheckpointRef,
    ) -> Result<(), VcsError>;

    /// `CheckpointStore.hasCheckpointRef`.
    async fn has_checkpoint_ref(
        &self,
        cwd: &str,
        checkpoint_ref: &CheckpointRef,
    ) -> Result<bool, VcsError>;

    /// `CheckpointStore.restoreCheckpoint`. `Ok(false)` when the ref is missing
    /// and `fallback_to_head` is off.
    async fn restore_checkpoint(
        &self,
        cwd: &str,
        checkpoint_ref: &CheckpointRef,
        fallback_to_head: bool,
    ) -> Result<bool, VcsError>;

    /// `CheckpointStore.diffCheckpoints` in numstat form, without HEAD fallback
    /// and with whitespace significant (the options `captureCheckpoint` uses).
    async fn diff_numstat(
        &self,
        cwd: &str,
        from: &CheckpointRef,
        to: &CheckpointRef,
    ) -> Result<String, VcsError>;

    /// `CheckpointStore.listAuthoredPaths`. `None` means HEAD did not move.
    async fn list_authored_paths(
        &self,
        cwd: &str,
        from: &CheckpointRef,
        to: &CheckpointRef,
    ) -> Result<Option<HashSet<String>>, VcsError>;

    /// `CheckpointStore.deleteCheckpointRefs`. Missing refs are tolerated.
    async fn delete_checkpoint_refs(
        &self,
        cwd: &str,
        checkpoint_refs: Vec<CheckpointRef>,
    ) -> Result<(), VcsError>;
}

/// [`CheckpointGit`] over turin-git's checkpoint store for one VCS driver.
pub struct TurinCheckpointGit {
    store: CheckpointStore,
}

impl TurinCheckpointGit {
    pub fn new(driver: Arc<dyn VcsDriver>) -> Self {
        Self {
            store: CheckpointStore::new(driver),
        }
    }
}

#[async_trait]
impl CheckpointGit for TurinCheckpointGit {
    async fn is_git_repository(&self, cwd: &str) -> Result<bool, VcsError> {
        self.store.is_git_repository(cwd).await
    }

    async fn capture_checkpoint(
        &self,
        cwd: &str,
        checkpoint_ref: &CheckpointRef,
    ) -> Result<(), VcsError> {
        self.store
            .capture_checkpoint(&VcsCaptureCheckpointInput {
                cwd: cwd.to_owned(),
                checkpoint_ref: checkpoint_ref.clone(),
            })
            .await
    }

    async fn has_checkpoint_ref(
        &self,
        cwd: &str,
        checkpoint_ref: &CheckpointRef,
    ) -> Result<bool, VcsError> {
        self.store
            .has_checkpoint_ref(&VcsHasCheckpointInput {
                cwd: cwd.to_owned(),
                checkpoint_ref: checkpoint_ref.clone(),
            })
            .await
    }

    async fn restore_checkpoint(
        &self,
        cwd: &str,
        checkpoint_ref: &CheckpointRef,
        fallback_to_head: bool,
    ) -> Result<bool, VcsError> {
        self.store
            .restore_checkpoint(&VcsRestoreCheckpointInput {
                cwd: cwd.to_owned(),
                checkpoint_ref: checkpoint_ref.clone(),
                fallback_to_head,
            })
            .await
    }

    async fn diff_numstat(
        &self,
        cwd: &str,
        from: &CheckpointRef,
        to: &CheckpointRef,
    ) -> Result<String, VcsError> {
        self.store
            .diff_checkpoints(&VcsDiffCheckpointsInput {
                cwd: cwd.to_owned(),
                from_checkpoint_ref: from.clone(),
                to_checkpoint_ref: to.clone(),
                fallback_from_to_head: false,
                ignore_whitespace: false,
                format: CheckpointDiffFormat::Numstat,
                file_paths: None,
            })
            .await
    }

    async fn list_authored_paths(
        &self,
        cwd: &str,
        from: &CheckpointRef,
        to: &CheckpointRef,
    ) -> Result<Option<HashSet<String>>, VcsError> {
        self.store
            .list_authored_paths(&VcsListAuthoredPathsInput {
                cwd: cwd.to_owned(),
                from_checkpoint_ref: from.clone(),
                to_checkpoint_ref: to.clone(),
            })
            .await
    }

    async fn delete_checkpoint_refs(
        &self,
        cwd: &str,
        checkpoint_refs: Vec<CheckpointRef>,
    ) -> Result<(), VcsError> {
        self.store
            .delete_checkpoint_refs(&VcsDeleteCheckpointRefsInput {
                cwd: cwd.to_owned(),
                checkpoint_refs,
            })
            .await
    }
}

#[cfg(test)]
mod tests;
