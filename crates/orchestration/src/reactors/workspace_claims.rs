//! Projection-backed [`WorkspaceClaimReads`] for [`super::checkpoint_restore_safety`].
//!
//! Upstream reads every other thread's checkpoint scopes and live provider
//! sessions inside `isCheckpointRestoreIsolated`. This is the Rust form of
//! those reads, built on the T13a projection store and the project store.

use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::ThreadId;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ProviderSessionStatus, OrchestrationV2ThreadShell,
};

use super::checkpoint_restore_safety::{OtherWorkspaceClaim, WorkspaceClaimReads};
use crate::pipeline::orchestrator_port::BoxError;
use crate::projection::project_store::ProjectStore;
use crate::projection::store::ProjectionStore;
use crate::projection::store::shell::ShellSnapshotOptions;

pub struct ProjectionWorkspaceClaims {
    projections: Arc<ProjectionStore>,
    projects: Arc<ProjectStore>,
}

impl ProjectionWorkspaceClaims {
    pub fn new(projections: Arc<ProjectionStore>, projects: Arc<ProjectStore>) -> Self {
        Self {
            projections,
            projects,
        }
    }

    async fn claim_for(
        &self,
        thread: &OrchestrationV2ThreadShell,
    ) -> Result<OtherWorkspaceClaim, BoxError> {
        let checkpoints = self.projections.get_checkpoint_context(&thread.id).await?;
        let providers = self
            .projections
            .get_thread_provider_context(&thread.id, None)
            .await?;

        let mut paths: Vec<String> = thread
            .worktree_path
            .iter()
            .map(|path| path.as_str().to_owned())
            .collect();
        paths.extend(
            checkpoints
                .checkpoint_scopes
                .into_iter()
                .filter_map(|scope| scope.cwd),
        );
        // A failed turn can leave an errored session with a live event stream. A session
        // shared across threads keeps the cwd it was opened with, often another thread's.
        // Each turn runs in the thread's own worktree or project root, which is covered above.
        paths.extend(
            providers
                .provider_sessions
                .iter()
                .filter(|session| {
                    session.status != OrchestrationV2ProviderSessionStatus::Stopped
                        && !session
                            .capabilities
                            .sessions
                            .supports_multiple_provider_threads_per_session
                })
                .map(|session| session.cwd.as_str().to_owned()),
        );

        let mut project_root_missing = false;
        if thread.worktree_path.is_none() {
            // A thread without a worktree works in its project's checkout.
            match self.projects.get(thread.project_id.clone(), true).await? {
                Some(project) => paths.push(project.workspace_root),
                None => project_root_missing = true,
            }
        }
        Ok(OtherWorkspaceClaim {
            paths,
            project_root_missing,
        })
    }
}

#[async_trait]
impl WorkspaceClaimReads for ProjectionWorkspaceClaims {
    async fn other_thread_claims(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Vec<OtherWorkspaceClaim>, BoxError> {
        // The shell holds live and archived threads; deleted threads are not in it.
        let shell = self
            .projections
            .get_shell_snapshot(ShellSnapshotOptions::default())
            .await?;
        let mut claims = Vec::new();
        for thread in shell.threads.iter().chain(shell.archived_threads.iter()) {
            if &thread.id == thread_id {
                continue;
            }
            claims.push(self.claim_for(thread).await?);
        }
        Ok(claims)
    }
}
