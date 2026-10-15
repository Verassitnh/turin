//! Port of upstream/apps/server/src/orchestration-v2/CheckpointService.ts
//!
//! Git work goes through [`CheckpointGit`]. Every git call for one workspace
//! runs under that workspace's lock, so a capture and a restore for the same
//! checkout never interleave.
//!
//! Turin deviation: upstream derives checkpoint refs under
//! `refs/t3/orchestration-v2/checkpoints`. turin-git's
//! `checkpoint_ref_for_scope_ordinal` uses `refs/t3/checkpoints` instead, so
//! this module computes the ref itself with the upstream prefix.

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tokio::sync::OwnedMutexGuard;
use turin_contracts::base_schemas::{CheckpointRef, TrimmedNonEmptyString};
use turin_contracts::orchestration_v2::{
    OrchestrationV2Checkpoint, OrchestrationV2CheckpointFileSummary,
    OrchestrationV2CheckpointScope, OrchestrationV2CheckpointScopeKind,
    OrchestrationV2CheckpointStatus,
};
use turin_contracts::{
    CheckpointId, CheckpointScopeId, NodeId, PositiveInt, ProviderThreadId, RunId, ThreadId,
    UtcDateTime,
};
use turin_git::checkpointing::diffs::{is_git_import, parse_turn_diff_files_from_numstat};

use super::checkpoint_git::CheckpointGit;
use turin_provider_core::id_allocator::IdAllocator;

const CHECKPOINT_REFS_PREFIX: &str = "refs/t3/orchestration-v2/checkpoints";
const ROOT_CHECKPOINT_SCOPE_NAME: &str = "root";

/// `CheckpointServiceV2Error`. Causes are carried as text: upstream's
/// `Schema.Defect()` holds whatever was thrown, and the only consumers log it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckpointServiceError {
    #[error("Failed to prepare root checkpoint scope for run {run_id}.")]
    RootScopePrepare {
        thread_id: ThreadId,
        run_id: RunId,
        cause: String,
    },
    #[error("Failed to ensure checkpoint scope {scope_id}.")]
    ScopeEnsure {
        scope_id: CheckpointScopeId,
        cause: String,
    },
    #[error("Failed to capture checkpoint baseline {ordinal_within_scope} for scope {scope_id}.")]
    BaselineCapture {
        scope_id: CheckpointScopeId,
        ordinal_within_scope: u64,
        cause: String,
    },
    #[error("Failed to capture checkpoint for scope {scope_id}.")]
    Capture {
        scope_id: CheckpointScopeId,
        cause: String,
    },
    #[error("Failed to restore checkpoint {checkpoint_id} for scope {scope_id}.")]
    Restore {
        scope_id: CheckpointScopeId,
        checkpoint_id: CheckpointId,
        cause: String,
    },
    #[error("Failed to delete stale checkpoint refs for scope {scope_id}.")]
    DeleteStaleRefs {
        scope_id: CheckpointScopeId,
        checkpoint_ids: Vec<CheckpointId>,
        cause: String,
    },
}

pub struct PrepareRootRunScope {
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub root_node_id: NodeId,
    pub provider_thread_id: ProviderThreadId,
    pub cwd: String,
    pub created_at: UtcDateTime,
}

pub struct CaptureInput<'a> {
    pub scope: &'a OrchestrationV2CheckpointScope,
    pub run_id: Option<RunId>,
    pub node_id: NodeId,
    pub ordinal_within_scope: u64,
    pub app_run_ordinal: Option<PositiveInt>,
    pub captured_at: UtcDateTime,
}

/// `checkpointRefForScopeOrdinal`: SHA-256 of the scope id, the first 32 hex
/// characters of that digest, then base64url of those characters.
pub fn checkpoint_ref_for_scope_ordinal(
    scope_id: &CheckpointScopeId,
    ordinal_within_scope: u64,
) -> CheckpointRef {
    let digest = Sha256::digest(scope_id.as_str().as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let scope_key = &hex[..32];
    let encoded = URL_SAFE_NO_PAD.encode(scope_key.as_bytes());
    CheckpointRef::new(format!(
        "{CHECKPOINT_REFS_PREFIX}/{encoded}/ordinal/{ordinal_within_scope}"
    ))
    .expect("scope checkpoint ref is non-empty and trimmed")
}

/// Per-workspace mutexes (`KeyedLock.make<string>()`).
#[derive(Default)]
struct WorkspaceLocks {
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl WorkspaceLocks {
    async fn lock(&self, cwd: &str) -> OwnedMutexGuard<()> {
        let mutex = self
            .locks
            .lock()
            .entry(cwd.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        mutex.lock_owned().await
    }
}

pub struct CheckpointService {
    git: Arc<dyn CheckpointGit>,
    ids: IdAllocator,
    locks: WorkspaceLocks,
}

impl CheckpointService {
    pub fn new(git: Arc<dyn CheckpointGit>) -> Self {
        Self {
            git,
            ids: IdAllocator,
            locks: WorkspaceLocks::default(),
        }
    }

    /// `prepareRootRunScope`. Pure: no git work happens until a baseline or capture.
    pub fn prepare_root_run_scope(
        &self,
        input: PrepareRootRunScope,
    ) -> Result<OrchestrationV2CheckpointScope, CheckpointServiceError> {
        let id = self
            .ids
            .checkpoint_scope(&input.thread_id, ROOT_CHECKPOINT_SCOPE_NAME);
        let cwd = TrimmedNonEmptyString::new(&input.cwd).map_err(|error| {
            CheckpointServiceError::RootScopePrepare {
                thread_id: input.thread_id.clone(),
                run_id: input.run_id.clone(),
                cause: error.to_string(),
            }
        })?;
        Ok(OrchestrationV2CheckpointScope {
            id,
            thread_id: input.thread_id,
            run_id: Some(input.run_id),
            node_id: input.root_node_id,
            parent_scope_id: None,
            provider_thread_id: Some(input.provider_thread_id),
            kind: OrchestrationV2CheckpointScopeKind::RootRun,
            ordinal_within_parent: 0,
            advances_app_run_count: true,
            cwd,
            created_at: input.created_at,
        })
    }

    /// `ensureScope`: identity.
    pub fn ensure_scope(
        &self,
        scope: OrchestrationV2CheckpointScope,
    ) -> Result<OrchestrationV2CheckpointScope, CheckpointServiceError> {
        Ok(scope)
    }

    /// `captureBaseline`: stores the pre-run snapshot at `ordinal`, unless the
    /// ref already exists or the workspace is not a git repository.
    pub async fn capture_baseline(
        &self,
        scope: &OrchestrationV2CheckpointScope,
        ordinal_within_scope: u64,
    ) -> Result<(), CheckpointServiceError> {
        let cwd = scope.cwd.as_str();
        let _guard = self.locks.lock(cwd).await;
        let baseline_error = |cause: String| CheckpointServiceError::BaselineCapture {
            scope_id: scope.id.clone(),
            ordinal_within_scope,
            cause,
        };
        if !self.is_git_checkpointable(cwd).await {
            return Ok(());
        }
        let checkpoint_ref = checkpoint_ref_for_scope_ordinal(&scope.id, ordinal_within_scope);
        let exists = self
            .git
            .has_checkpoint_ref(cwd, &checkpoint_ref)
            .await
            .map_err(|error| baseline_error(error.to_string()))?;
        if exists {
            return Ok(());
        }
        self.git
            .capture_checkpoint(cwd, &checkpoint_ref)
            .await
            .map_err(|error| baseline_error(error.to_string()))
    }

    /// `materializeBaselineCheckpoint`: a checkpoint record for a baseline ref.
    /// Status is `ready` when the ref exists. A failed lookup reads as `missing`.
    pub async fn materialize_baseline_checkpoint(
        &self,
        scope: &OrchestrationV2CheckpointScope,
        ordinal_within_scope: u64,
    ) -> Result<OrchestrationV2Checkpoint, CheckpointServiceError> {
        let cwd = scope.cwd.as_str();
        let _guard = self.locks.lock(cwd).await;
        let checkpoint_ref = checkpoint_ref_for_scope_ordinal(&scope.id, ordinal_within_scope);
        let checkpoint_id = self.checkpoint_id(&scope.id, ordinal_within_scope);
        let available = if self.is_git_checkpointable(cwd).await {
            match self.git.has_checkpoint_ref(cwd, &checkpoint_ref).await {
                Ok(found) => found,
                Err(error) => {
                    tracing::warn!(
                        scope_id = %scope.id,
                        checkpoint_ref = %checkpoint_ref,
                        error = %error,
                        "orchestration V2 baseline ref lookup failed"
                    );
                    false
                }
            }
        } else {
            false
        };
        Ok(make_checkpoint(MakeCheckpoint {
            id: checkpoint_id,
            scope,
            run_id: None,
            node_id: scope.node_id.clone(),
            parent_checkpoint_id: None,
            ordinal_within_scope,
            app_run_ordinal: None,
            checkpoint_ref,
            status: if available {
                OrchestrationV2CheckpointStatus::Ready
            } else {
                OrchestrationV2CheckpointStatus::Missing
            },
            files: Vec::new(),
            captured_at: scope.created_at,
        }))
    }

    /// `capture`: snapshots the workspace at `ordinal` and reports the files the
    /// turn itself touched. Imported changes (a pull or rebase) are left out.
    pub async fn capture(
        &self,
        input: CaptureInput<'_>,
    ) -> Result<OrchestrationV2Checkpoint, CheckpointServiceError> {
        let scope = input.scope;
        let cwd = scope.cwd.as_str();
        let _guard = self.locks.lock(cwd).await;
        let ordinal = input.ordinal_within_scope;
        let checkpoint_id = self.checkpoint_id(&scope.id, ordinal);
        let parent_checkpoint_id =
            (ordinal > 0).then(|| self.checkpoint_id(&scope.id, ordinal - 1));
        let checkpoint_ref = checkpoint_ref_for_scope_ordinal(&scope.id, ordinal);
        let previous_ref = checkpoint_ref_for_scope_ordinal(&scope.id, ordinal.saturating_sub(1));
        let build = |status, files| {
            make_checkpoint(MakeCheckpoint {
                id: checkpoint_id.clone(),
                scope,
                run_id: input.run_id.clone(),
                node_id: input.node_id.clone(),
                parent_checkpoint_id: parent_checkpoint_id.clone(),
                ordinal_within_scope: ordinal,
                app_run_ordinal: input.app_run_ordinal,
                checkpoint_ref: checkpoint_ref.clone(),
                status,
                files,
                captured_at: input.captured_at,
            })
        };

        if !self.is_git_checkpointable(cwd).await {
            return Ok(build(OrchestrationV2CheckpointStatus::Missing, Vec::new()));
        }

        let captured = match self.git.capture_checkpoint(cwd, &checkpoint_ref).await {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(
                    scope_id = %scope.id,
                    checkpoint_ref = %checkpoint_ref,
                    error = %error,
                    "orchestration V2 checkpoint capture failed"
                );
                false
            }
        };
        if !captured {
            return Ok(build(OrchestrationV2CheckpointStatus::Error, Vec::new()));
        }

        let previous_exists = match self.git.has_checkpoint_ref(cwd, &previous_ref).await {
            Ok(found) => found,
            Err(error) => {
                tracing::warn!(
                    scope_id = %scope.id,
                    checkpoint_ref = %previous_ref,
                    error = %error,
                    "orchestration V2 previous checkpoint ref lookup failed"
                );
                false
            }
        };
        // A pull or rebase can change thousands of files the turn did not write.
        // Keep only the files the turn's own work touched.
        let files = if previous_exists {
            self.summarize_turn_files(scope, cwd, &previous_ref, &checkpoint_ref)
                .await
        } else {
            Vec::new()
        };

        Ok(build(OrchestrationV2CheckpointStatus::Ready, files))
    }

    async fn summarize_turn_files(
        &self,
        scope: &OrchestrationV2CheckpointScope,
        cwd: &str,
        previous_ref: &CheckpointRef,
        checkpoint_ref: &CheckpointRef,
    ) -> Vec<OrchestrationV2CheckpointFileSummary> {
        let numstat = self
            .git
            .diff_numstat(cwd, previous_ref, checkpoint_ref)
            .await;
        // Upstream's `listAuthoredPaths` catch yields `null`, which keeps every file.
        let authored_paths = match self
            .git
            .list_authored_paths(cwd, previous_ref, checkpoint_ref)
            .await
        {
            Ok(paths) => paths,
            Err(error) => {
                tracing::warn!(
                    scope_id = %scope.id,
                    checkpoint_ref = %checkpoint_ref,
                    error = %error,
                    "orchestration V2 checkpoint authored paths failed"
                );
                None
            }
        };
        match numstat {
            Ok(diff) => parse_turn_diff_files_from_numstat(&diff)
                .into_iter()
                .filter(|file| !is_git_import(file, authored_paths.as_ref()))
                .filter_map(|file| {
                    Some(OrchestrationV2CheckpointFileSummary {
                        path: TrimmedNonEmptyString::new(&file.path).ok()?,
                        kind: TrimmedNonEmptyString::new("modified").ok()?,
                        additions: file.additions,
                        deletions: file.deletions,
                    })
                })
                .collect(),
            Err(error) => {
                tracing::warn!(
                    scope_id = %scope.id,
                    checkpoint_ref = %checkpoint_ref,
                    error = %error,
                    "orchestration V2 checkpoint diff summary failed"
                );
                Vec::new()
            }
        }
    }

    /// `restore`: resets the workspace to a `ready` checkpoint. A missing ref
    /// fails; there is no HEAD fallback.
    pub async fn restore(
        &self,
        scope: &OrchestrationV2CheckpointScope,
        checkpoint: &OrchestrationV2Checkpoint,
    ) -> Result<(), CheckpointServiceError> {
        let cwd = scope.cwd.as_str();
        let _guard = self.locks.lock(cwd).await;
        let restore_error = |cause: String| CheckpointServiceError::Restore {
            scope_id: scope.id.clone(),
            checkpoint_id: checkpoint.id.clone(),
            cause,
        };
        if checkpoint.status != OrchestrationV2CheckpointStatus::Ready {
            return Err(restore_error(format!(
                "Checkpoint status is {}.",
                checkpoint_status_name(checkpoint.status)
            )));
        }
        let restored = self
            .git
            .restore_checkpoint(cwd, &checkpoint.ref_, false)
            .await
            .map_err(|error| restore_error(error.to_string()))?;
        if !restored {
            return Err(restore_error("Checkpoint ref is unavailable.".to_owned()));
        }
        Ok(())
    }

    /// `deleteStaleRefs`: best-effort; the git layer tolerates missing refs.
    pub async fn delete_stale_refs(
        &self,
        scope: &OrchestrationV2CheckpointScope,
        checkpoints: &[OrchestrationV2Checkpoint],
    ) -> Result<(), CheckpointServiceError> {
        let cwd = scope.cwd.as_str();
        let _guard = self.locks.lock(cwd).await;
        let refs = checkpoints.iter().map(|checkpoint| checkpoint.ref_.clone());
        self.git
            .delete_checkpoint_refs(cwd, refs.collect())
            .await
            .map_err(|error| CheckpointServiceError::DeleteStaleRefs {
                scope_id: scope.id.clone(),
                checkpoint_ids: checkpoints.iter().map(|c| c.id.clone()).collect(),
                cause: error.to_string(),
            })
    }

    /// `isGitCheckpointable`: a lookup failure reads as "not a repository".
    async fn is_git_checkpointable(&self, cwd: &str) -> bool {
        self.git.is_git_repository(cwd).await.unwrap_or(false)
    }

    fn checkpoint_id(&self, scope_id: &CheckpointScopeId, ordinal: u64) -> CheckpointId {
        self.ids.checkpoint(scope_id, &ordinal.to_string())
    }
}

fn checkpoint_status_name(status: OrchestrationV2CheckpointStatus) -> &'static str {
    match status {
        OrchestrationV2CheckpointStatus::Ready => "ready",
        OrchestrationV2CheckpointStatus::Missing => "missing",
        OrchestrationV2CheckpointStatus::Error => "error",
        OrchestrationV2CheckpointStatus::Stale => "stale",
    }
}

struct MakeCheckpoint<'a> {
    id: CheckpointId,
    scope: &'a OrchestrationV2CheckpointScope,
    run_id: Option<RunId>,
    node_id: NodeId,
    parent_checkpoint_id: Option<CheckpointId>,
    ordinal_within_scope: u64,
    app_run_ordinal: Option<PositiveInt>,
    checkpoint_ref: CheckpointRef,
    status: OrchestrationV2CheckpointStatus,
    files: Vec<OrchestrationV2CheckpointFileSummary>,
    captured_at: UtcDateTime,
}

fn make_checkpoint(input: MakeCheckpoint<'_>) -> OrchestrationV2Checkpoint {
    OrchestrationV2Checkpoint {
        id: input.id,
        thread_id: input.scope.thread_id.clone(),
        scope_id: input.scope.id.clone(),
        run_id: input.run_id,
        node_id: input.node_id,
        parent_checkpoint_id: input.parent_checkpoint_id,
        ordinal_within_scope: input.ordinal_within_scope,
        app_run_ordinal: input.app_run_ordinal,
        ref_: input.checkpoint_ref,
        status: input.status,
        files: input.files,
        captured_at: input.captured_at,
    }
}

#[cfg(test)]
mod tests;
