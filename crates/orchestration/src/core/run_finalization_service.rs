//! Port of upstream/apps/server/src/orchestration-v2/RunFinalizationService.ts.
//!
//! Once a run settles, its checkpoint is captured, then the workspace it wrote
//! to is refreshed so the sidebar and the PR cache see the finished turn.
//! `RunFinalizationObserver` is the refresh: `layerObserver` in upstream, here
//! [`WorkspaceRunObserver`]. The capture, the scope read, and the workspace
//! refresh are seams (`CheckpointCapture`, `CheckpointScopeReads`,
//! `WorkspaceRefresher`) because their owners live outside this crate.

use std::{fmt, sync::Arc};

use async_trait::async_trait;
use tracing::warn;
use turin_contracts::{CheckpointScopeId, RunId, ThreadId};

use crate::{
    pipeline::orchestrator_port::BoxError,
    projection::store::{ProjectionStore, canonical::ProjectionRecordField},
};

/// The `operation` of a [`RunFinalizationError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunFinalizationOperation {
    CaptureCheckpoint,
    RefreshWorkspace,
}

impl RunFinalizationOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::CaptureCheckpoint => "capture-checkpoint",
            Self::RefreshWorkspace => "refresh-workspace",
        }
    }
}

/// `RunFinalizationError`.
#[derive(Debug)]
pub struct RunFinalizationError {
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub scope_id: CheckpointScopeId,
    pub operation: RunFinalizationOperation,
    pub cause: BoxError,
}

impl fmt::Display for RunFinalizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Run {} finalization failed during {} for scope {}.",
            self.run_id.as_str(),
            self.operation.as_str(),
            self.scope_id.as_str()
        )
    }
}

impl std::error::Error for RunFinalizationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.cause.as_ref())
    }
}

/// `RunFinalizationRefreshError`: the workspace refresh at `cwd` failed.
#[derive(Debug)]
pub struct RunFinalizationRefreshError {
    pub cwd: String,
    pub cause: BoxError,
}

impl fmt::Display for RunFinalizationRefreshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Failed to refresh workspace at {}.", self.cwd)
    }
}

impl std::error::Error for RunFinalizationRefreshError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.cause.as_ref())
    }
}

/// The `finalize` input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizeInput {
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub scope_id: CheckpointScopeId,
}

/// `CheckpointCaptureServiceV2.execute`. Owned by the checkpoint service (T06).
#[async_trait]
pub trait CheckpointCapture: Send + Sync {
    async fn execute(&self, input: &FinalizeInput) -> Result<(), BoxError>;
}

/// A checkpoint scope's working directory, the only scope field finalization reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointScopeCwd {
    pub id: CheckpointScopeId,
    pub cwd: String,
}

/// The checkpoint scopes of a thread (`getCheckpointContext`).
#[async_trait]
pub trait CheckpointScopeReads: Send + Sync {
    async fn checkpoint_scopes(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Vec<CheckpointScopeCwd>, BoxError>;
}

/// `RunFinalizationObserver`'s refresh, per workspace.
#[async_trait]
pub trait RunFinalizationObserver: Send + Sync {
    async fn refresh(
        &self,
        cwd: &str,
        thread_id: &ThreadId,
        run_id: &RunId,
    ) -> Result<(), RunFinalizationRefreshError>;
}

/// `RunFinalizationService`.
pub struct RunFinalizationService {
    checkpoint_capture: Arc<dyn CheckpointCapture>,
    scopes: Arc<dyn CheckpointScopeReads>,
    observer: Arc<dyn RunFinalizationObserver>,
}

impl RunFinalizationService {
    pub fn new(
        checkpoint_capture: Arc<dyn CheckpointCapture>,
        scopes: Arc<dyn CheckpointScopeReads>,
        observer: Arc<dyn RunFinalizationObserver>,
    ) -> Self {
        Self {
            checkpoint_capture,
            scopes,
            observer,
        }
    }

    /// `finalize`: captures the run's checkpoint, then refreshes the scope's workspace.
    pub async fn finalize(&self, input: FinalizeInput) -> Result<(), RunFinalizationError> {
        let failed = |operation, cause: BoxError| RunFinalizationError {
            thread_id: input.thread_id.clone(),
            run_id: input.run_id.clone(),
            scope_id: input.scope_id.clone(),
            operation,
            cause,
        };
        self.checkpoint_capture
            .execute(&input)
            .await
            .map_err(|cause| failed(RunFinalizationOperation::CaptureCheckpoint, cause))?;
        let scopes = self
            .scopes
            .checkpoint_scopes(&input.thread_id)
            .await
            .map_err(|cause| failed(RunFinalizationOperation::RefreshWorkspace, cause))?;
        let Some(scope) = scopes.iter().find(|scope| scope.id == input.scope_id) else {
            return Ok(());
        };
        self.observer
            .refresh(&scope.cwd, &input.thread_id, &input.run_id)
            .await
            .map_err(|error| failed(RunFinalizationOperation::RefreshWorkspace, Box::new(error)))
    }
}

/// The local VCS state the observer reads (`refreshLocalStatus`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalVcsStatus {
    /// The checked-out branch, `None` when detached or not a repository.
    pub ref_name: Option<String>,
    pub is_default_ref: bool,
}

/// Workspace refresh operations used by [`WorkspaceRunObserver`]. Each one
/// wraps a service outside this crate (workspace entries, VCS status, PR cache).
#[async_trait]
pub trait WorkspaceRefresher: Send + Sync {
    async fn refresh_entries(&self, cwd: &str) -> Result<(), BoxError>;
    async fn refresh_local_status(&self, cwd: &str) -> Result<LocalVcsStatus, BoxError>;
    async fn refresh_pull_request_status(&self, cwd: &str) -> Result<(), BoxError>;
}

/// The thread fields the observer decides on (`getThreadShell`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadRefreshState {
    pub branch: Option<String>,
    pub active_run_id: Option<RunId>,
}

#[async_trait]
pub trait ThreadRefreshReads: Send + Sync {
    /// `None` when the thread does not exist.
    async fn thread_refresh_state(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Option<ThreadRefreshState>, BoxError>;
}

/// `layerObserver`'s refresh. After a run settles, refreshes the workspace.
/// It refreshes the PR cache only when the completed run's branch is the one
/// checked out and no newer run is active. The default branch's PR cache is left alone.
pub struct WorkspaceRunObserver {
    refresher: Arc<dyn WorkspaceRefresher>,
    threads: Arc<dyn ThreadRefreshReads>,
}

impl WorkspaceRunObserver {
    pub fn new(
        refresher: Arc<dyn WorkspaceRefresher>,
        threads: Arc<dyn ThreadRefreshReads>,
    ) -> Self {
        Self { refresher, threads }
    }

    async fn refresh_inner(
        &self,
        cwd: &str,
        thread_id: &ThreadId,
        run_id: &RunId,
    ) -> Result<(), BoxError> {
        let (_, local) = tokio::try_join!(
            self.refresher.refresh_entries(cwd),
            self.refresher.refresh_local_status(cwd),
        )?;
        let ref_name = match local.ref_name {
            Some(ref_name) if !local.is_default_ref => ref_name,
            _ => return Ok(()),
        };
        let Some(thread) = self.threads.thread_refresh_state(thread_id).await? else {
            return Ok(());
        };
        if thread.branch.as_deref() != Some(ref_name.as_str()) {
            return Ok(());
        }
        if thread
            .active_run_id
            .as_ref()
            .is_some_and(|active| active != run_id)
        {
            return Ok(());
        }
        // A failed PR lookup is logged, not surfaced: the run itself finished.
        if let Err(cause) = self.refresher.refresh_pull_request_status(cwd).await {
            warn!(
                thread_id = %thread_id.as_str(),
                cwd,
                %cause,
                "failed to refresh pull request status after run completion"
            );
        }
        Ok(())
    }
}

#[async_trait]
impl RunFinalizationObserver for WorkspaceRunObserver {
    async fn refresh(
        &self,
        cwd: &str,
        thread_id: &ThreadId,
        run_id: &RunId,
    ) -> Result<(), RunFinalizationRefreshError> {
        self.refresh_inner(cwd, thread_id, run_id)
            .await
            .map_err(|cause| RunFinalizationRefreshError {
                cwd: cwd.to_owned(),
                cause,
            })
    }
}

#[async_trait]
impl CheckpointScopeReads for ProjectionStore {
    async fn checkpoint_scopes(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Vec<CheckpointScopeCwd>, BoxError> {
        let projection = self
            .get_thread_records(
                thread_id,
                vec![ProjectionRecordField::CheckpointScopes],
                None,
            )
            .await
            .map_err(|error| Box::new(error) as BoxError)?;
        Ok(projection
            .checkpoint_scopes
            .into_iter()
            .map(|scope| CheckpointScopeCwd {
                id: scope.id,
                cwd: scope.cwd.as_str().to_owned(),
            })
            .collect())
    }
}

#[async_trait]
impl ThreadRefreshReads for ProjectionStore {
    async fn thread_refresh_state(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Option<ThreadRefreshState>, BoxError> {
        let shell = self
            .get_thread_shell(thread_id)
            .await
            .map_err(|error| Box::new(error) as BoxError)?;
        Ok(shell.map(|shell| ThreadRefreshState {
            branch: shell.branch.map(|branch| branch.as_str().to_owned()),
            active_run_id: shell.active_run_id,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use parking_lot::Mutex;

    use super::*;

    const THREAD: &str = "thread-finalize";
    const RUN: &str = "run-finalize";
    const SCOPE: &str = "scope-finalize";

    fn input() -> FinalizeInput {
        FinalizeInput {
            thread_id: ThreadId::new(THREAD).expect("thread id"),
            run_id: RunId::new(RUN).expect("run id"),
            scope_id: CheckpointScopeId::new(SCOPE).expect("scope id"),
        }
    }

    struct FakeCapture {
        calls: AtomicUsize,
        fail: bool,
    }

    #[async_trait]
    impl CheckpointCapture for FakeCapture {
        async fn execute(&self, _input: &FinalizeInput) -> Result<(), BoxError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err("capture failed".into());
            }
            Ok(())
        }
    }

    struct FakeScopes;

    #[async_trait]
    impl CheckpointScopeReads for FakeScopes {
        async fn checkpoint_scopes(
            &self,
            _thread_id: &ThreadId,
        ) -> Result<Vec<CheckpointScopeCwd>, BoxError> {
            Ok(vec![CheckpointScopeCwd {
                id: CheckpointScopeId::new(SCOPE).expect("scope id"),
                cwd: "/repo".to_owned(),
            }])
        }
    }

    #[derive(Default)]
    struct RecordingObserver {
        refreshes: Mutex<Vec<(String, ThreadId, RunId)>>,
    }

    #[async_trait]
    impl RunFinalizationObserver for RecordingObserver {
        async fn refresh(
            &self,
            cwd: &str,
            thread_id: &ThreadId,
            run_id: &RunId,
        ) -> Result<(), RunFinalizationRefreshError> {
            self.refreshes
                .lock()
                .push((cwd.to_owned(), thread_id.clone(), run_id.clone()));
            Ok(())
        }
    }

    #[tokio::test]
    async fn refreshes_workspace_after_checkpoint_capture() {
        let capture = Arc::new(FakeCapture {
            calls: AtomicUsize::new(0),
            fail: false,
        });
        let observer = Arc::new(RecordingObserver::default());
        let service =
            RunFinalizationService::new(capture.clone(), Arc::new(FakeScopes), observer.clone());
        service.finalize(input()).await.expect("finalize");
        assert_eq!(capture.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *observer.refreshes.lock(),
            vec![(
                "/repo".to_owned(),
                ThreadId::new(THREAD).expect("thread id"),
                RunId::new(RUN).expect("run id"),
            )]
        );
    }

    #[tokio::test]
    async fn a_failed_capture_skips_the_refresh_and_names_the_operation() {
        let observer = Arc::new(RecordingObserver::default());
        let service = RunFinalizationService::new(
            Arc::new(FakeCapture {
                calls: AtomicUsize::new(0),
                fail: true,
            }),
            Arc::new(FakeScopes),
            observer.clone(),
        );
        let error = service
            .finalize(input())
            .await
            .expect_err("capture failure");
        assert_eq!(error.operation, RunFinalizationOperation::CaptureCheckpoint);
        assert_eq!(
            error.to_string(),
            format!("Run {RUN} finalization failed during capture-checkpoint for scope {SCOPE}.")
        );
        assert!(observer.refreshes.lock().is_empty());
    }

    struct FakeRefresher {
        ref_name: Option<String>,
        is_default_ref: bool,
        pull_request_refreshes: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl WorkspaceRefresher for FakeRefresher {
        async fn refresh_entries(&self, _cwd: &str) -> Result<(), BoxError> {
            Ok(())
        }
        async fn refresh_local_status(&self, _cwd: &str) -> Result<LocalVcsStatus, BoxError> {
            Ok(LocalVcsStatus {
                ref_name: self.ref_name.clone(),
                is_default_ref: self.is_default_ref,
            })
        }
        async fn refresh_pull_request_status(&self, cwd: &str) -> Result<(), BoxError> {
            self.pull_request_refreshes.lock().push(cwd.to_owned());
            Ok(())
        }
    }

    struct FakeThreads {
        state: Option<ThreadRefreshState>,
    }

    #[async_trait]
    impl ThreadRefreshReads for FakeThreads {
        async fn thread_refresh_state(
            &self,
            _thread_id: &ThreadId,
        ) -> Result<Option<ThreadRefreshState>, BoxError> {
            Ok(self.state.clone())
        }
    }

    /// The observer's PR refresh decision for one scenario.
    async fn pull_request_refreshes(
        branch: &str,
        checked_out: &str,
        active_run: Option<&str>,
    ) -> Vec<String> {
        let refresher = Arc::new(FakeRefresher {
            ref_name: Some(checked_out.to_owned()),
            is_default_ref: checked_out == "main",
            pull_request_refreshes: Mutex::new(Vec::new()),
        });
        let observer = WorkspaceRunObserver::new(
            refresher.clone(),
            Arc::new(FakeThreads {
                state: Some(ThreadRefreshState {
                    branch: Some(branch.to_owned()),
                    active_run_id: active_run.map(|run| RunId::new(run).expect("run id")),
                }),
            }),
        );
        observer
            .refresh(
                "/repo",
                &ThreadId::new("thread-pr-refresh").expect("thread id"),
                &RunId::new("completed-run").expect("run id"),
            )
            .await
            .expect("refresh");
        let refreshed = refresher.pull_request_refreshes.lock().clone();
        refreshed
    }

    #[tokio::test]
    async fn discovers_a_new_pr_for_the_completed_runs_branch() {
        assert_eq!(
            pull_request_refreshes("feature", "feature", None).await,
            vec!["/repo".to_owned()]
        );
    }

    #[tokio::test]
    async fn leaves_the_default_branchs_pr_cache_alone() {
        assert!(
            pull_request_refreshes("main", "main", None)
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn does_not_refresh_another_threads_checkout() {
        assert!(
            pull_request_refreshes("feature", "other", None)
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn does_not_refresh_during_a_newer_active_run() {
        assert!(
            pull_request_refreshes("feature", "feature", Some("newer-run"))
                .await
                .is_empty()
        );
    }
}
