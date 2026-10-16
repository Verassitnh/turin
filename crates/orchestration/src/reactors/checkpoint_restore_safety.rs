//! Port of upstream/apps/server/src/orchestration-v2/CheckpointRestoreSafety.ts
//!
//! A checkpoint snapshots the whole checkout, so file restore is only safe
//! when no other thread can write to that checkout. The check runs at command
//! admission and again before provider rollback, so a worktree shared later is
//! still rejected.
//!
//! Turin: the other threads' paths come from [`WorkspaceClaimReads`]. The
//! projection reads it stands for (checkpoint scopes, live provider sessions)
//! are not in `turin-orchestration` yet.

use std::path::Path;

use async_trait::async_trait;
use turin_contracts::ThreadId;

use crate::pipeline::orchestrator_port::BoxError;

pub const SHARED_WORKSPACE_RESTORE_MESSAGE: &str = "File restore requires an isolated worktree. This workspace may contain changes from another thread. Rewind the conversation without restoring files instead.";

/// What one other, non-deleted thread can write to. Upstream builds this from
/// the thread's worktree, its checkpoint scope cwds, its live provider session
/// cwds, and, for a thread without a worktree, its project's workspace root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OtherWorkspaceClaim {
    /// Candidate directories, in upstream's order. Missing paths are skipped.
    pub paths: Vec<String>,
    /// A thread without a worktree whose project record no longer exists.
    /// Upstream refuses the restore in that case.
    pub project_root_missing: bool,
}

/// The other threads' claims. Includes archived threads, excludes `thread_id`
/// and threads that are deleted.
#[async_trait]
pub trait WorkspaceClaimReads: Send + Sync {
    async fn other_thread_claims(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Vec<OtherWorkspaceClaim>, BoxError>;
}

/// `isCheckpointRestoreIsolated`. `Ok(true)` means files may be restored.
///
/// A thread with no worktree always reads as shared: it works in the project's
/// checkout, which other threads may also use.
pub async fn is_checkpoint_restore_isolated(
    thread_id: &ThreadId,
    worktree_path: Option<&str>,
    scope_cwd: &str,
    reads: &dyn WorkspaceClaimReads,
) -> Result<bool, BoxError> {
    let Some(worktree_path) = worktree_path else {
        return Ok(false);
    };
    let cwd = realpath(scope_cwd).await?;
    let worktree_cwd = realpath(worktree_path).await?;
    if cwd != worktree_cwd {
        return Ok(false);
    }
    let mut checked = std::collections::HashSet::new();
    for claim in reads.other_thread_claims(thread_id).await? {
        if claim.project_root_missing {
            return Ok(false);
        }
        for candidate in claim.paths {
            if !checked.insert(candidate.clone()) {
                continue;
            }
            let Some(other_cwd) = realpath_if_exists(&candidate).await? else {
                continue;
            };
            if contains(&cwd, &other_cwd) || contains(&other_cwd, &cwd) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// `path.relative(parent, child)` is empty, or stays inside `parent`.
fn contains(parent: &Path, child: &Path) -> bool {
    child.starts_with(parent)
}

async fn realpath(path: &str) -> Result<std::path::PathBuf, BoxError> {
    Ok(tokio::fs::canonicalize(path).await?)
}

/// `realPath` that maps NotFound to `None`, as the upstream catch does.
async fn realpath_if_exists(path: &str) -> Result<Option<std::path::PathBuf>, BoxError> {
    match tokio::fs::canonicalize(path).await {
        Ok(resolved) => Ok(Some(resolved)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Box::new(error)),
    }
}

#[cfg(test)]
mod tests;
