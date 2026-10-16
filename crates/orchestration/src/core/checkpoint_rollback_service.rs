//! Port of upstream/apps/server/src/orchestration-v2/CheckpointRollbackService.ts
//!
//! Rewinds a provider conversation to a ready checkpoint and, unless the caller
//! opted out, restores that checkpoint's files. File restore is refused when
//! the checkout is shared with another thread.
//!
//! The provider session, runtime policy, and thread-command lock are not owned
//! here. Neither is checkpoint git (that is T13d's `CheckpointService`) nor the
//! projection/event stores. Each is a narrow trait in this file; see
//! `docs/handoff/R5.md`.
//!
//! Deviations from upstream:
//! - Collaborators are traits instead of Effect services. `IdAllocator` is the
//!   real one: it cannot fail, so allocation errors are not wrapped.
//! - `cause` on an unexpected failure is the error's text. Upstream's
//!   `Schema.Defect()` holds the thrown value.
//! - `is_checkpoint_restore_isolated` takes the shell and per-thread claims
//!   through [`RestoreIsolationReads`]. Canonical paths use `Path::starts_with`,
//!   which matches `path.relative` once both sides have been `realpath`'d.
//! - A missing `nativeMetadata.itemIdentityVersion` and an explicit null are
//!   both `None` (the contract field does not distinguish them).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use turin_contracts::base_schemas::PositiveInt;
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::orchestration_v2::{
    OrchestrationV2Checkpoint, OrchestrationV2CheckpointScope, OrchestrationV2CheckpointStatus,
    OrchestrationV2DomainEvent, OrchestrationV2ExecutionNode, OrchestrationV2ExecutionNodeStatus,
    OrchestrationV2ProviderSessionStatus, OrchestrationV2ProviderThread, OrchestrationV2Run,
    OrchestrationV2RunStatus, OrchestrationV2ThreadProjection, latest_provider_turn_for_attempt,
};
use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};
use turin_contracts::{
    CheckpointId, CheckpointScopeId, EventId, ProjectId, ProviderSessionId, ProviderThreadId,
    ThreadId, UtcDateTime,
};
use turin_provider_core::id_allocator::IdAllocator;
use turin_provider_core::provider_adapter::{
    ProviderAdapterV2RollbackTarget, ProviderAdapterV2RollbackThreadInput,
    ProviderAdapterV2RuntimePolicy,
};

use crate::pipeline::orchestrator_port::BoxError;

pub const ROLLBACK_FAILED_MESSAGE: &str = "The provider could not roll back this conversation. Try again; if it keeps failing, check the provider and server logs.";

pub const SHARED_WORKSPACE_RESTORE_MESSAGE: &str = "File restore requires an isolated worktree. This workspace may contain changes from another thread. Rewind the conversation without restoring files instead.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointRollbackReason {
    RollbackTargetInvalid,
    ActiveProviderChanged,
    ProviderTurnUnavailable,
    UnexpectedFailure,
    SharedWorkspace,
}

impl CheckpointRollbackReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RollbackTargetInvalid => "rollback-target-invalid",
            Self::ActiveProviderChanged => "active-provider-changed",
            Self::ProviderTurnUnavailable => "provider-turn-unavailable",
            Self::UnexpectedFailure => "unexpected-failure",
            Self::SharedWorkspace => "shared-workspace",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointRollbackExecutionError {
    pub reason: CheckpointRollbackReason,
    pub thread_id: ThreadId,
    pub provider_thread_id: ProviderThreadId,
    pub checkpoint_id: CheckpointId,
    pub cause: Option<String>,
}

impl std::fmt::Display for CheckpointRollbackExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.reason {
            CheckpointRollbackReason::RollbackTargetInvalid => write!(
                f,
                "Rollback target {} for provider thread {} on thread {} is incomplete or invalid.",
                self.checkpoint_id, self.provider_thread_id, self.thread_id
            ),
            CheckpointRollbackReason::ActiveProviderChanged => write!(
                f,
                "Active provider changed before rollback target {} could execute on thread {}.",
                self.checkpoint_id, self.thread_id
            ),
            CheckpointRollbackReason::ProviderTurnUnavailable => write!(
                f,
                "Provider turn for rollback target {} is unavailable on provider thread {}.",
                self.checkpoint_id, self.provider_thread_id
            ),
            CheckpointRollbackReason::SharedWorkspace => {
                f.write_str(SHARED_WORKSPACE_RESTORE_MESSAGE)
            }
            CheckpointRollbackReason::UnexpectedFailure => f.write_str(ROLLBACK_FAILED_MESSAGE),
        }
    }
}

impl std::error::Error for CheckpointRollbackExecutionError {}

/// `getThreadRecords` for the kinds rollback reads: provider threads, provider
/// sessions, checkpoints, checkpoint scopes, runs, attempts, nodes, and
/// provider turns. Unrequested kinds may be empty.
#[async_trait]
pub trait RollbackRecords: Send + Sync {
    async fn get_thread_records(
        &self,
        thread_id: &ThreadId,
    ) -> Result<OrchestrationV2ThreadProjection, BoxError>;
}

/// One sidebar row isolation inspects. Deleted rows and the thread being
/// restored are skipped by [`is_checkpoint_restore_isolated`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellThreadClaim {
    pub id: ThreadId,
    pub project_id: ProjectId,
    pub worktree_path: Option<String>,
    pub deleted_at: Option<UtcDateTime>,
}

/// A provider session cwd that might still be writing the checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSessionClaim {
    pub status: OrchestrationV2ProviderSessionStatus,
    pub supports_multiple_provider_threads_per_session: bool,
    pub cwd: String,
}

/// The projection and project reads [`is_checkpoint_restore_isolated`] makes.
/// `project_workspace_root` includes deleted projects (upstream
/// `{ includeDeleted: true }`). `Ok(None)` means the project record is gone.
#[async_trait]
pub trait RestoreIsolationReads: Send + Sync {
    async fn shell_threads(&self) -> Result<Vec<ShellThreadClaim>, BoxError>;
    async fn checkpoint_scope_cwds(&self, thread_id: &ThreadId) -> Result<Vec<String>, BoxError>;
    async fn provider_session_claims(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Vec<ProviderSessionClaim>, BoxError>;
    async fn project_workspace_root(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<String>, BoxError>;
}

/// `CheckpointService.restore` and `deleteStaleRefs` (T13d `CheckpointService`).
#[async_trait]
pub trait CheckpointFiles: Send + Sync {
    async fn restore(
        &self,
        scope: &OrchestrationV2CheckpointScope,
        checkpoint: &OrchestrationV2Checkpoint,
    ) -> Result<(), BoxError>;

    async fn delete_stale_refs(
        &self,
        scope: &OrchestrationV2CheckpointScope,
        checkpoints: &[OrchestrationV2Checkpoint],
    ) -> Result<(), BoxError>;
}

/// `EventSink.write`. The owner maps this onto `EventSink::write(WriteInput::events)`.
#[async_trait]
pub trait RollbackEventWriter: Send + Sync {
    async fn write(&self, events: Vec<OrchestrationV2DomainEvent>) -> Result<(), BoxError>;
}

/// What `ProviderSessionManager.open` receives for a rollback.
#[derive(Debug, Clone)]
pub struct OpenProviderSession {
    pub thread_id: ThreadId,
    pub provider_session_id: ProviderSessionId,
    pub model_selection: ModelSelection,
    pub runtime_policy: ProviderAdapterV2RuntimePolicy,
    pub resume_from_session:
        Option<turin_contracts::orchestration_v2::OrchestrationV2ProviderSession>,
    pub initial_native_thread_id: Option<String>,
    pub initial_provider_item_identity_version:
        Option<turin_contracts::orchestration_v2::ItemIdentityVersionV2>,
}

/// `session.rollbackThread`. Returns the provider thread from the adapter
/// snapshot (`ProviderAdapterV2ThreadSnapshot.provider_thread`).
#[async_trait]
pub trait ProviderConversationRollback: Send + Sync {
    async fn rollback_thread(
        &self,
        input: ProviderAdapterV2RollbackThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, BoxError>;
}

/// `ProviderSessionManagerV2.open` (session owner).
#[async_trait]
pub trait ProviderSessionOpener: Send + Sync {
    async fn open(
        &self,
        input: OpenProviderSession,
    ) -> Result<Arc<dyn ProviderConversationRollback>, BoxError>;
}

/// `RuntimePolicyV2.resolve({ thread, modelSelection })`.
#[async_trait]
pub trait RuntimePolicyResolver: Send + Sync {
    async fn resolve(
        &self,
        thread: &turin_contracts::orchestration_v2::OrchestrationV2AppThread,
        model_selection: &ModelSelection,
    ) -> Result<ProviderAdapterV2RuntimePolicy, BoxError>;
}

/// `ThreadCommandExecutor.withLock`. The future runs while the thread's command
/// lock is held, so a message planned against the old native session cannot
/// commit after the rollback.
#[async_trait]
pub trait ThreadCommandLock: Send + Sync {
    async fn with_lock(
        &self,
        thread_id: &ThreadId,
        action: BoxFuture<'static, Result<(), CheckpointRollbackExecutionError>>,
    ) -> Result<(), CheckpointRollbackExecutionError>;
}

#[async_trait]
impl ThreadCommandLock for crate::pipeline::thread_command_executor::ThreadCommandExecutor {
    async fn with_lock(
        &self,
        thread_id: &ThreadId,
        action: BoxFuture<'static, Result<(), CheckpointRollbackExecutionError>>,
    ) -> Result<(), CheckpointRollbackExecutionError> {
        crate::pipeline::thread_command_executor::ThreadCommandExecutor::with_lock(
            self,
            thread_id,
            || action,
        )
        .await
    }
}

/// `isCheckpointRestoreIsolated`. `Ok(true)` means files may be restored.
///
/// A thread with no worktree always reads as shared: it works in the project's
/// checkout, which other threads may also use.
pub async fn is_checkpoint_restore_isolated(
    thread_id: &ThreadId,
    worktree_path: Option<&str>,
    scope_cwd: &str,
    reads: &dyn RestoreIsolationReads,
) -> Result<bool, BoxError> {
    let Some(worktree_path) = worktree_path else {
        return Ok(false);
    };
    let cwd = tokio::fs::canonicalize(scope_cwd).await?;
    let worktree_cwd = tokio::fs::canonicalize(worktree_path).await?;
    if cwd != worktree_cwd {
        return Ok(false);
    }
    let mut checked = HashSet::new();
    for other in reads.shell_threads().await? {
        if &other.id == thread_id || other.deleted_at.is_some() {
            continue;
        }
        let mut paths = Vec::new();
        if let Some(path) = other.worktree_path.clone() {
            paths.push(path);
        }
        paths.extend(reads.checkpoint_scope_cwds(&other.id).await?);
        for session in reads.provider_session_claims(&other.id).await? {
            // A failed turn can leave an errored session with a live event
            // stream. A session shared across threads keeps the cwd it was
            // opened with; each turn runs in the thread's own worktree or
            // project root, which this list already covers.
            if session.status != OrchestrationV2ProviderSessionStatus::Stopped
                && !session.supports_multiple_provider_threads_per_session
            {
                paths.push(session.cwd);
            }
        }
        if other.worktree_path.is_none() {
            // A thread without a worktree works in its project's checkout.
            let Some(root) = reads.project_workspace_root(&other.project_id).await? else {
                return Ok(false);
            };
            paths.push(root);
        }
        for candidate in paths {
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

fn contains(parent: &Path, child: &Path) -> bool {
    child.starts_with(parent)
}

async fn realpath_if_exists(path: &str) -> Result<Option<PathBuf>, BoxError> {
    match tokio::fs::canonicalize(path).await {
        Ok(resolved) => Ok(Some(resolved)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Box::new(error)),
    }
}

pub struct ExecuteInput {
    pub thread_id: ThreadId,
    pub provider_thread_id: ProviderThreadId,
    pub checkpoint_id: CheckpointId,
    pub scope_id: CheckpointScopeId,
    /// `None` restores files, matching upstream's omitted `restoreFiles`.
    pub restore_files: Option<bool>,
}

pub struct CheckpointRollbackDeps {
    pub records: Arc<dyn RollbackRecords>,
    pub isolation: Arc<dyn RestoreIsolationReads>,
    pub checkpoints: Arc<dyn CheckpointFiles>,
    pub events: Arc<dyn RollbackEventWriter>,
    pub sessions: Arc<dyn ProviderSessionOpener>,
    pub runtime_policy: Arc<dyn RuntimePolicyResolver>,
    pub thread_commands: Arc<dyn ThreadCommandLock>,
}

pub struct CheckpointRollbackService {
    records: Arc<dyn RollbackRecords>,
    isolation: Arc<dyn RestoreIsolationReads>,
    checkpoints: Arc<dyn CheckpointFiles>,
    events: Arc<dyn RollbackEventWriter>,
    sessions: Arc<dyn ProviderSessionOpener>,
    runtime_policy: Arc<dyn RuntimePolicyResolver>,
    thread_commands: Arc<dyn ThreadCommandLock>,
    ids: IdAllocator,
}

impl CheckpointRollbackService {
    pub fn new(deps: CheckpointRollbackDeps) -> Self {
        Self {
            records: deps.records,
            isolation: deps.isolation,
            checkpoints: deps.checkpoints,
            events: deps.events,
            sessions: deps.sessions,
            runtime_policy: deps.runtime_policy,
            thread_commands: deps.thread_commands,
            ids: IdAllocator,
        }
    }

    pub async fn execute(
        &self,
        input: ExecuteInput,
    ) -> Result<(), CheckpointRollbackExecutionError> {
        let thread_id = input.thread_id.clone();
        let provider_thread_id = input.provider_thread_id.clone();
        let checkpoint_id = input.checkpoint_id.clone();
        match self.execute_inner(input).await {
            Ok(()) => Ok(()),
            Err(StepError::Rollback(error)) => Err(error),
            Err(StepError::Other(cause)) => Err(CheckpointRollbackExecutionError {
                reason: CheckpointRollbackReason::UnexpectedFailure,
                thread_id,
                provider_thread_id,
                checkpoint_id,
                cause: Some(cause),
            }),
        }
    }

    async fn execute_inner(&self, input: ExecuteInput) -> Result<(), StepError> {
        let projection = self
            .records
            .get_thread_records(&input.thread_id)
            .await
            .map_err(StepError::other)?;
        let provider_thread = projection
            .provider_threads
            .iter()
            .find(|candidate| candidate.id == input.provider_thread_id)
            .cloned();
        let checkpoint = projection
            .checkpoints
            .iter()
            .find(|candidate| candidate.id == input.checkpoint_id)
            .cloned();
        let scope = projection
            .checkpoint_scopes
            .iter()
            .find(|candidate| candidate.id == input.scope_id)
            .cloned();
        let (provider_thread, checkpoint, scope) = match (provider_thread, checkpoint, scope) {
            (Some(provider_thread), Some(checkpoint), Some(scope))
                if provider_thread.provider_session_id.is_some()
                    && checkpoint.scope_id == scope.id
                    && checkpoint.status == OrchestrationV2CheckpointStatus::Ready =>
            {
                (provider_thread, checkpoint, scope)
            }
            _ => {
                return Err(rollback_error(
                    &input,
                    CheckpointRollbackReason::RollbackTargetInvalid,
                )
                .into());
            }
        };
        if projection.thread.active_provider_thread_id.as_ref() != Some(&provider_thread.id)
            || projection.thread.model_selection.instance_id != provider_thread.provider_instance_id
        {
            return Err(
                rollback_error(&input, CheckpointRollbackReason::ActiveProviderChanged).into(),
            );
        }

        let restore_files = input.restore_files != Some(false);
        if restore_files {
            let isolated = is_checkpoint_restore_isolated(
                &projection.thread.id,
                projection
                    .thread
                    .worktree_path
                    .as_ref()
                    .map(|path| path.as_str()),
                scope.cwd.as_str(),
                self.isolation.as_ref(),
            )
            .await
            .map_err(StepError::other)?;
            if !isolated {
                return Err(
                    rollback_error(&input, CheckpointRollbackReason::SharedWorkspace).into(),
                );
            }
        }

        let model_selection = projection.thread.model_selection.clone();
        let runtime_policy = self
            .runtime_policy
            .resolve(&projection.thread, &model_selection)
            .await
            .map_err(StepError::other)?;
        let provider_session_id = provider_thread
            .provider_session_id
            .clone()
            .expect("validated above");
        let resume_from_session = projection
            .provider_sessions
            .iter()
            .find(|session| session.id == provider_session_id)
            .cloned();
        let session = self
            .sessions
            .open(OpenProviderSession {
                thread_id: input.thread_id.clone(),
                provider_session_id,
                model_selection,
                runtime_policy,
                resume_from_session,
                initial_native_thread_id: provider_thread
                    .native_thread_ref
                    .as_ref()
                    .and_then(|reference| reference.native_id.as_ref())
                    .map(|id| id.as_str().to_owned()),
                initial_provider_item_identity_version: provider_thread
                    .native_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.item_identity_version),
            })
            .await
            .map_err(StepError::other)?;

        let target_ordinal = checkpoint
            .app_run_ordinal
            .map(PositiveInt::get)
            .unwrap_or(0);
        // Stopped and failed runs after the target leave the provider
        // conversation too, so they must not stay visible.
        let runs_to_rollback: Vec<OrchestrationV2Run> = projection
            .runs
            .iter()
            .filter(|run| run.ordinal.get() > target_ordinal && is_rollback_visible(run.status))
            .cloned()
            .collect();
        // Rolled-back turns stay in the audit history, but no longer exist in
        // the provider conversation and must not be counted by a later rewind.
        let rolled_back_run_ids: HashSet<&turin_contracts::RunId> = projection
            .runs
            .iter()
            .filter(|run| run.status == OrchestrationV2RunStatus::RolledBack)
            .map(|run| &run.id)
            .collect();
        let rolled_back_attempt_ids: HashSet<&turin_contracts::RunAttemptId> = projection
            .attempts
            .iter()
            .filter(|attempt| rolled_back_run_ids.contains(&attempt.run_id))
            .map(|attempt| &attempt.id)
            .collect();
        let provider_thread_turns: Vec<_> = projection
            .provider_turns
            .iter()
            .filter(|turn| {
                turn.provider_thread_id == provider_thread.id
                    && match &turn.run_attempt_id {
                        None => true,
                        Some(attempt_id) => !rolled_back_attempt_ids.contains(attempt_id),
                    }
            })
            .cloned()
            .collect();
        let rollback_target = rollback_target(
            &input,
            &checkpoint,
            &provider_thread,
            &projection,
            target_ordinal,
        )?;
        let snapshot_thread = if runs_to_rollback.is_empty() {
            provider_thread.clone()
        } else {
            session
                .rollback_thread(ProviderAdapterV2RollbackThreadInput {
                    provider_thread: provider_thread.clone(),
                    target: rollback_target,
                    provider_thread_turns,
                })
                .await
                .map_err(StepError::other)?
        };
        if restore_files {
            self.checkpoints
                .restore(&scope, &checkpoint)
                .await
                .map_err(StepError::other)?;
        }
        let stale_checkpoints: Vec<OrchestrationV2Checkpoint> = projection
            .checkpoints
            .iter()
            .filter(|candidate| {
                candidate.scope_id == scope.id
                    && candidate
                        .app_run_ordinal
                        .is_some_and(|ordinal| ordinal.get() > target_ordinal)
                    && candidate.status == OrchestrationV2CheckpointStatus::Ready
            })
            .cloned()
            .collect();
        if !stale_checkpoints.is_empty() {
            self.checkpoints
                .delete_stale_refs(&scope, &stale_checkpoints)
                .await
                .map_err(StepError::other)?;
        }

        let events = Arc::clone(&self.events);
        let ids = self.ids;
        let thread_id = input.thread_id.clone();
        let driver = provider_thread.driver.clone();
        let provider_instance_id = provider_thread.provider_instance_id.clone();
        let nodes = projection.nodes.clone();
        let failure = rollback_error(&input, CheckpointRollbackReason::UnexpectedFailure);
        // Thread commands write full provider thread rows under this lock. Without
        // it, a message sent during the rollback can plan against the old native
        // session and commit after this, undoing the provider's fork.
        self.thread_commands
            .with_lock(
                &input.thread_id,
                Box::pin(async move {
                    commit_rollback(CommitRollback {
                        events: events.as_ref(),
                        ids,
                        thread_id,
                        driver,
                        provider_instance_id,
                        snapshot_thread,
                        target_ordinal,
                        stale_checkpoints,
                        runs_to_rollback,
                        nodes,
                        failure,
                    })
                    .await
                }),
            )
            .await
            .map_err(StepError::Rollback)?;
        Ok(())
    }
}

struct CommitRollback<'a> {
    events: &'a dyn RollbackEventWriter,
    ids: IdAllocator,
    thread_id: ThreadId,
    driver: ProviderDriverKind,
    provider_instance_id: ProviderInstanceId,
    snapshot_thread: OrchestrationV2ProviderThread,
    target_ordinal: u64,
    stale_checkpoints: Vec<OrchestrationV2Checkpoint>,
    runs_to_rollback: Vec<OrchestrationV2Run>,
    nodes: Vec<OrchestrationV2ExecutionNode>,
    failure: CheckpointRollbackExecutionError,
}

async fn commit_rollback(
    mut input: CommitRollback<'_>,
) -> Result<(), CheckpointRollbackExecutionError> {
    let now = UtcDateTime::now();
    input.snapshot_thread.last_run_ordinal = (input.target_ordinal != 0)
        .then(|| PositiveInt::new(input.target_ordinal).expect("nonzero ordinal"));
    input.snapshot_thread.updated_at = now;
    let mut events = Vec::new();
    events.push(OrchestrationV2DomainEvent::ProviderThreadUpdated {
        id: allocate_event(&input.ids, &input.thread_id),
        thread_id: input.thread_id.clone(),
        run_id: None,
        node_id: None,
        driver: Some(input.driver),
        provider_instance_id: Some(input.provider_instance_id.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: input.snapshot_thread,
    });
    for mut stale in input.stale_checkpoints {
        let run_id = stale.run_id.clone();
        let node_id = stale.node_id.clone();
        stale.status = OrchestrationV2CheckpointStatus::Stale;
        events.push(OrchestrationV2DomainEvent::CheckpointCaptured {
            id: allocate_event(&input.ids, &input.thread_id),
            thread_id: input.thread_id.clone(),
            run_id,
            node_id: Some(node_id),
            driver: None,
            provider_instance_id: Some(input.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: stale,
        });
    }
    for mut run in input.runs_to_rollback {
        let root_node = run.root_node_id.as_ref().and_then(|id| {
            input
                .nodes
                .iter()
                .find(|candidate| &candidate.id == id)
                .cloned()
        });
        let run_id = run.id.clone();
        let run_instance = run.provider_instance_id.clone();
        run.status = OrchestrationV2RunStatus::RolledBack;
        run.completed_at = Some(now);
        events.push(OrchestrationV2DomainEvent::RunUpdated {
            id: allocate_event(&input.ids, &input.thread_id),
            thread_id: input.thread_id.clone(),
            run_id: Some(run_id.clone()),
            node_id: root_node.as_ref().map(|node| node.id.clone()),
            driver: None,
            provider_instance_id: Some(run_instance.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: run,
        });
        if let Some(mut root_node) = root_node {
            root_node.status = OrchestrationV2ExecutionNodeStatus::RolledBack;
            root_node.completed_at = Some(now);
            events.push(OrchestrationV2DomainEvent::NodeUpdated {
                id: allocate_event(&input.ids, &input.thread_id),
                thread_id: input.thread_id.clone(),
                run_id: Some(run_id),
                node_id: Some(root_node.id.clone()),
                driver: None,
                provider_instance_id: Some(run_instance),
                raw_event_id: None,
                occurred_at: now,
                payload: root_node,
            });
        }
    }
    input
        .events
        .write(events)
        .await
        .map_err(|cause| CheckpointRollbackExecutionError {
            cause: Some(cause.to_string()),
            ..input.failure
        })
}

fn allocate_event(ids: &IdAllocator, thread_id: &ThreadId) -> EventId {
    ids.event(Some(thread_id), None, None)
}

fn is_rollback_visible(status: OrchestrationV2RunStatus) -> bool {
    matches!(
        status,
        OrchestrationV2RunStatus::Completed
            | OrchestrationV2RunStatus::Interrupted
            | OrchestrationV2RunStatus::Failed
            | OrchestrationV2RunStatus::Cancelled
    )
}

fn rollback_target(
    input: &ExecuteInput,
    checkpoint: &OrchestrationV2Checkpoint,
    provider_thread: &OrchestrationV2ProviderThread,
    projection: &OrchestrationV2ThreadProjection,
    target_ordinal: u64,
) -> Result<ProviderAdapterV2RollbackTarget, CheckpointRollbackExecutionError> {
    if target_ordinal == 0 {
        return Ok(ProviderAdapterV2RollbackTarget::ThreadStart {
            checkpoint_id: checkpoint.id.clone(),
            app_run_ordinal: 0,
        });
    }
    let target_run = projection
        .runs
        .iter()
        .find(|run| run.ordinal.get() == target_ordinal);
    let target_attempt = target_run.and_then(|run| {
        run.active_attempt_id.as_ref().and_then(|attempt_id| {
            projection
                .attempts
                .iter()
                .find(|attempt| &attempt.id == attempt_id)
        })
    });
    // A goal run can span several native turns; roll back to its last.
    let latest = latest_provider_turn_for_attempt(
        projection.provider_turns.iter(),
        target_attempt.map(|attempt| &attempt.id),
    );
    let fallback = target_attempt.and_then(|attempt| {
        attempt.provider_turn_id.as_ref().and_then(|turn_id| {
            projection
                .provider_turns
                .iter()
                .find(|turn| &turn.id == turn_id)
        })
    });
    match latest.or(fallback) {
        Some(turn) if turn.provider_thread_id == provider_thread.id => {
            Ok(ProviderAdapterV2RollbackTarget::ProviderTurn {
                checkpoint_id: checkpoint.id.clone(),
                app_run_ordinal: i64::try_from(target_ordinal).expect("app run ordinal fits i64"),
                provider_turn: turn.clone(),
            })
        }
        _ => Err(rollback_error(
            input,
            CheckpointRollbackReason::ProviderTurnUnavailable,
        )),
    }
}

fn rollback_error(
    input: &ExecuteInput,
    reason: CheckpointRollbackReason,
) -> CheckpointRollbackExecutionError {
    CheckpointRollbackExecutionError {
        reason,
        thread_id: input.thread_id.clone(),
        provider_thread_id: input.provider_thread_id.clone(),
        checkpoint_id: input.checkpoint_id.clone(),
        cause: None,
    }
}

enum StepError {
    Rollback(CheckpointRollbackExecutionError),
    Other(String),
}

impl StepError {
    fn other(error: impl ToString) -> Self {
        Self::Other(error.to_string())
    }
}

impl From<CheckpointRollbackExecutionError> for StepError {
    fn from(error: CheckpointRollbackExecutionError) -> Self {
        Self::Rollback(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use turin_contracts::orchestration_v2::{
        OrchestrationV2ProviderTurn, OrchestrationV2ProviderTurnStatus,
    };
    use turin_contracts::provider_policy::{ProviderInteractionMode, RuntimeMode};

    const T0: &str = "2026-01-01T00:00:00.000Z";

    fn decode<T: DeserializeOwned>(value: Value) -> T {
        serde_json::from_value(value).unwrap_or_else(|error| panic!("fixture: {error}"))
    }

    fn cwd() -> String {
        std::env::current_dir()
            .expect("cwd")
            .to_string_lossy()
            .into_owned()
    }

    fn thread_value(
        id: &str,
        worktree: Option<&str>,
        active: &str,
        instance: &str,
        model: &str,
    ) -> Value {
        json!({
            "createdBy": "user",
            "creationSource": "web",
            "id": id,
            "projectId": "project:test",
            "title": id,
            "providerInstanceId": instance,
            "modelSelection": {"instanceId": instance, "model": model},
            "runtimeMode": "full-access",
            "interactionMode": "default",
            "branch": null,
            "worktreePath": worktree,
            "activeProviderThreadId": active,
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
            "forkedFrom": null,
            "createdAt": T0,
            "updatedAt": T0,
            "archivedAt": null,
            "settledOverride": null,
            "settledAt": null,
            "lastVisitedAt": null,
            "deletedAt": null
        })
    }

    fn provider_thread_value(id: &str, session: &str, instance: &str, app_thread: &str) -> Value {
        json!({
            "id": id,
            "driver": "codex",
            "providerInstanceId": instance,
            "providerSessionId": session,
            "appThreadId": app_thread,
            "ownerNodeId": null,
            "nativeThreadRef": null,
            "nativeConversationHeadRef": null,
            "status": "idle",
            "firstRunOrdinal": null,
            "lastRunOrdinal": null,
            "handoffIds": [],
            "forkedFrom": null,
            "createdAt": T0,
            "updatedAt": T0
        })
    }

    fn scope_value(id: &str, thread: &str, path: &str) -> Value {
        json!({
            "id": id,
            "threadId": thread,
            "runId": null,
            "nodeId": "node:scope",
            "parentScopeId": null,
            "providerThreadId": null,
            "kind": "root_run",
            "ordinalWithinParent": 0,
            "advancesAppRunCount": true,
            "cwd": path,
            "createdAt": T0
        })
    }

    fn checkpoint_value(
        id: &str,
        thread: &str,
        scope: &str,
        status: &str,
        app_run_ordinal: Option<u64>,
    ) -> Value {
        json!({
            "id": id,
            "threadId": thread,
            "scopeId": scope,
            "runId": null,
            "nodeId": "node:scope",
            "parentCheckpointId": null,
            "ordinalWithinScope": 0,
            "appRunOrdinal": app_run_ordinal,
            "ref": format!("refs/t3/checkpoints/{id}"),
            "status": status,
            "files": [],
            "capturedAt": T0
        })
    }

    fn projection(parts: Value) -> OrchestrationV2ThreadProjection {
        let mut base = json!({
            "thread": parts["thread"].clone(),
            "runs": [],
            "attempts": [],
            "nodes": [],
            "subagents": [],
            "providerSessions": [],
            "providerThreads": [],
            "providerTurns": [],
            "runtimeRequests": [],
            "messages": [],
            "plans": [],
            "turnItems": [],
            "checkpointScopes": [],
            "checkpoints": [],
            "contextHandoffs": [],
            "contextTransfers": [],
            "visibleTurnItems": [],
            "updatedAt": T0
        });
        let Some(map) = parts.as_object() else {
            panic!("projection parts");
        };
        let Some(base_map) = base.as_object_mut() else {
            panic!("projection base");
        };
        for (key, value) in map {
            base_map.insert(key.clone(), value.clone());
        }
        decode(base)
    }

    struct World {
        projection: OrchestrationV2ThreadProjection,
        shell: Vec<ShellThreadClaim>,
        scope_cwds: Vec<String>,
        project_root: Option<String>,
        open_calls: AtomicUsize,
        policy_calls: AtomicUsize,
        restore_calls: AtomicUsize,
        allow_open: bool,
        allow_policy: bool,
        allow_restore: bool,
        check_events: bool,
        calls: std::sync::Mutex<Vec<&'static str>>,
        held: AtomicBool,
        expect_thread: Option<ThreadId>,
        /// When set, `rollback_thread` checks the Codex turn count equals
        /// `2 - ordinal`, the upstream rewind assertion.
        rollback_ordinal: Option<u64>,
        provider_thread: Option<OrchestrationV2ProviderThread>,
    }

    struct Handle(pub Arc<World>);

    #[async_trait]
    impl RollbackRecords for Handle {
        async fn get_thread_records(
            &self,
            _thread_id: &ThreadId,
        ) -> Result<OrchestrationV2ThreadProjection, BoxError> {
            Ok(self.0.projection.clone())
        }
    }

    #[async_trait]
    impl RestoreIsolationReads for Handle {
        async fn shell_threads(&self) -> Result<Vec<ShellThreadClaim>, BoxError> {
            Ok(self.0.shell.clone())
        }
        async fn checkpoint_scope_cwds(
            &self,
            _thread_id: &ThreadId,
        ) -> Result<Vec<String>, BoxError> {
            Ok(self.0.scope_cwds.clone())
        }
        async fn provider_session_claims(
            &self,
            _thread_id: &ThreadId,
        ) -> Result<Vec<ProviderSessionClaim>, BoxError> {
            Ok(Vec::new())
        }
        async fn project_workspace_root(
            &self,
            _project_id: &ProjectId,
        ) -> Result<Option<String>, BoxError> {
            Ok(self.0.project_root.clone())
        }
    }

    #[async_trait]
    impl CheckpointFiles for Handle {
        async fn restore(
            &self,
            _scope: &OrchestrationV2CheckpointScope,
            _checkpoint: &OrchestrationV2Checkpoint,
        ) -> Result<(), BoxError> {
            self.0.restore_calls.fetch_add(1, Ordering::SeqCst);
            if !self.0.allow_restore {
                return Err("checkpoint restore must not run".into());
            }
            self.0.calls.lock().expect("calls").push("files");
            Ok(())
        }
        async fn delete_stale_refs(
            &self,
            _scope: &OrchestrationV2CheckpointScope,
            _checkpoints: &[OrchestrationV2Checkpoint],
        ) -> Result<(), BoxError> {
            self.0.calls.lock().expect("calls").push("stale");
            Ok(())
        }
    }

    #[async_trait]
    impl RollbackEventWriter for Handle {
        async fn write(&self, events: Vec<OrchestrationV2DomainEvent>) -> Result<(), BoxError> {
            if self.0.check_events {
                assert!(
                    self.0.held.load(Ordering::SeqCst),
                    "thread command lock must be held while the rollback commits"
                );
                assert!(
                    events.iter().any(|event| matches!(
                        event,
                        OrchestrationV2DomainEvent::RunUpdated { payload, .. }
                            if payload.status == OrchestrationV2RunStatus::RolledBack
                    )),
                    "rollback must mark a run rolled_back"
                );
            }
            self.0.calls.lock().expect("calls").push("projection");
            Ok(())
        }
    }

    #[async_trait]
    impl ProviderConversationRollback for Handle {
        async fn rollback_thread(
            &self,
            input: ProviderAdapterV2RollbackThreadInput,
        ) -> Result<OrchestrationV2ProviderThread, BoxError> {
            let ordinal = self.0.rollback_ordinal.expect("rollback must not run");
            let count = codex_rollback_turn_count(&input).expect("codex rollback count");
            assert_eq!(count, 2 - ordinal as usize);
            self.0.calls.lock().expect("calls").push("provider");
            Ok(self.0.provider_thread.clone().expect("provider thread"))
        }
    }

    #[async_trait]
    impl ProviderSessionOpener for Handle {
        async fn open(
            &self,
            _input: OpenProviderSession,
        ) -> Result<Arc<dyn ProviderConversationRollback>, BoxError> {
            self.0.open_calls.fetch_add(1, Ordering::SeqCst);
            if !self.0.allow_open {
                return Err("provider session open must not run".into());
            }
            Ok(Arc::new(Handle(Arc::clone(&self.0))))
        }
    }

    #[async_trait]
    impl RuntimePolicyResolver for Handle {
        async fn resolve(
            &self,
            _thread: &turin_contracts::orchestration_v2::OrchestrationV2AppThread,
            _model_selection: &ModelSelection,
        ) -> Result<ProviderAdapterV2RuntimePolicy, BoxError> {
            self.0.policy_calls.fetch_add(1, Ordering::SeqCst);
            if !self.0.allow_policy {
                return Err("runtime policy resolution must not run".into());
            }
            Ok(ProviderAdapterV2RuntimePolicy {
                runtime_mode: RuntimeMode::FullAccess,
                interaction_mode: ProviderInteractionMode::Default,
                cwd: None,
                approval_policy: None,
                sandbox_policy: None,
                reasoning_effort: None,
            })
        }
    }

    #[async_trait]
    impl ThreadCommandLock for Handle {
        async fn with_lock(
            &self,
            thread_id: &ThreadId,
            action: BoxFuture<'static, Result<(), CheckpointRollbackExecutionError>>,
        ) -> Result<(), CheckpointRollbackExecutionError> {
            if let Some(expected) = &self.0.expect_thread {
                assert_eq!(thread_id, expected);
            }
            self.0.held.store(true, Ordering::SeqCst);
            let result = action.await;
            self.0.held.store(false, Ordering::SeqCst);
            result
        }
    }

    fn service(handle: &Arc<Handle>) -> CheckpointRollbackService {
        CheckpointRollbackService::new(CheckpointRollbackDeps {
            records: handle.clone(),
            isolation: handle.clone(),
            checkpoints: handle.clone(),
            events: handle.clone(),
            sessions: handle.clone(),
            runtime_policy: handle.clone(),
            thread_commands: handle.clone(),
        })
    }

    fn world(projection: OrchestrationV2ThreadProjection) -> Arc<World> {
        Arc::new(World {
            projection,
            shell: Vec::new(),
            scope_cwds: Vec::new(),
            project_root: None,
            open_calls: AtomicUsize::new(0),
            policy_calls: AtomicUsize::new(0),
            restore_calls: AtomicUsize::new(0),
            allow_open: false,
            allow_policy: false,
            allow_restore: false,
            check_events: false,
            calls: std::sync::Mutex::new(Vec::new()),
            held: AtomicBool::new(false),
            expect_thread: None,
            rollback_ordinal: None,
            provider_thread: None,
        })
    }

    fn input(
        thread: &str,
        provider_thread: &str,
        checkpoint: &str,
        scope: &str,
        restore_files: Option<bool>,
    ) -> ExecuteInput {
        ExecuteInput {
            thread_id: ThreadId::new(thread).unwrap(),
            provider_thread_id: ProviderThreadId::new(provider_thread).unwrap(),
            checkpoint_id: CheckpointId::new(checkpoint).unwrap(),
            scope_id: CheckpointScopeId::new(scope).unwrap(),
            restore_files,
        }
    }

    /// Upstream `resolveCodexRollbackTurnCount`. A turn counts when it has a
    /// native ref and a terminal status. `/goal` control turns carry no native
    /// ref and must not consume a native turn.
    fn codex_rollback_turn_count(
        input: &ProviderAdapterV2RollbackThreadInput,
    ) -> Result<usize, String> {
        let terminal = |turn: &OrchestrationV2ProviderTurn| {
            turn.native_turn_ref.is_some()
                && matches!(
                    turn.status,
                    OrchestrationV2ProviderTurnStatus::Completed
                        | OrchestrationV2ProviderTurnStatus::Interrupted
                        | OrchestrationV2ProviderTurnStatus::Failed
                        | OrchestrationV2ProviderTurnStatus::Cancelled
                )
        };
        match &input.target {
            ProviderAdapterV2RollbackTarget::ThreadStart { .. } => Ok(input
                .provider_thread_turns
                .iter()
                .filter(|turn| terminal(turn))
                .count()),
            ProviderAdapterV2RollbackTarget::ProviderTurn { provider_turn, .. } => {
                if provider_turn.provider_thread_id != input.provider_thread.id {
                    return Err("target turn belongs to another provider thread".into());
                }
                let boundary = input
                    .provider_thread_turns
                    .iter()
                    .find(|turn| turn.id == provider_turn.id)
                    .ok_or("target turn was not found")?;
                Ok(input
                    .provider_thread_turns
                    .iter()
                    .filter(|turn| turn.ordinal > boundary.ordinal && terminal(turn))
                    .count())
            }
        }
    }

    #[tokio::test]
    async fn rejects_a_non_ready_checkpoint_before_opening_a_session_or_restoring_files() {
        let path = cwd();
        let thread = "thread:rollback-non-ready";
        let provider_thread = "provider-thread:rollback-non-ready";
        let checkpoint = "checkpoint:rollback-non-ready";
        let scope = "checkpoint-scope:rollback-non-ready";
        let instance = "provider_rollback_non_ready";
        let world = world(projection(json!({
            "thread": thread_value(thread, Some(&path), provider_thread, instance, "test-model"),
            "providerThreads": [provider_thread_value(provider_thread, "provider-session:rollback-non-ready", instance, thread)],
            "checkpoints": [checkpoint_value(checkpoint, thread, scope, "stale", None)],
            "checkpointScopes": [scope_value(scope, thread, &path)],
        })));
        let handle = Arc::new(Handle(Arc::clone(&world)));
        let error = service(&handle)
            .execute(input(thread, provider_thread, checkpoint, scope, None))
            .await
            .expect_err("non-ready checkpoint");
        assert_eq!(error.reason.as_str(), "rollback-target-invalid");
        assert_eq!(
            error.to_string(),
            format!(
                "Rollback target {checkpoint} for provider thread {provider_thread} on thread {thread} is incomplete or invalid."
            )
        );
        assert!(error.cause.is_none());
        assert_eq!(world.policy_calls.load(Ordering::SeqCst), 0);
        assert_eq!(world.open_calls.load(Ordering::SeqCst), 0);
        assert_eq!(world.restore_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn rejects_a_rollback_when_another_provider_thread_became_active() {
        let path = cwd();
        let thread = "thread:rollback-inactive-provider-thread";
        let requested = "provider-thread:rollback-inactive-provider-thread:requested";
        let active = "provider-thread:rollback-inactive-provider-thread:active";
        let checkpoint = "checkpoint:rollback-inactive-provider-thread";
        let scope = "checkpoint-scope:rollback-inactive-provider-thread";
        let instance = "provider_rollback_inactive_provider_thread";
        let world = world(projection(json!({
            "thread": thread_value(thread, None, active, instance, "test-model"),
            "providerThreads": [provider_thread_value(requested, "provider-session:rollback-inactive-provider-thread", instance, thread)],
            "checkpoints": [checkpoint_value(checkpoint, thread, scope, "ready", None)],
            "checkpointScopes": [scope_value(scope, thread, &path)],
        })));
        let handle = Arc::new(Handle(Arc::clone(&world)));
        let error = service(&handle)
            .execute(input(thread, requested, checkpoint, scope, None))
            .await
            .expect_err("inactive provider");
        assert_eq!(error.reason.as_str(), "active-provider-changed");
        assert_eq!(
            error.to_string(),
            format!(
                "Active provider changed before rollback target {checkpoint} could execute on thread {thread}."
            )
        );
        assert!(error.cause.is_none());
        assert_eq!(world.policy_calls.load(Ordering::SeqCst), 0);
        assert_eq!(world.open_calls.load(Ordering::SeqCst), 0);
        assert_eq!(world.restore_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn rejects_a_rollback_when_provider_selection_changed_before_execution() {
        let path = cwd();
        let thread = "thread:rollback-provider-selection-changed";
        let provider_thread = "provider-thread:rollback-provider-selection-changed";
        let checkpoint = "checkpoint:rollback-provider-selection-changed";
        let scope = "checkpoint-scope:rollback-provider-selection-changed";
        let original = "provider_rollback_provider_selection_changed_original";
        let selected = "provider_rollback_provider_selection_changed_selected";
        let world = world(projection(json!({
            "thread": thread_value(thread, Some(&path), provider_thread, selected, "test-model"),
            "providerThreads": [provider_thread_value(provider_thread, "provider-session:rollback-provider-selection-changed", original, thread)],
            "checkpoints": [checkpoint_value(checkpoint, thread, scope, "ready", None)],
            "checkpointScopes": [scope_value(scope, thread, &path)],
        })));
        let handle = Arc::new(Handle(Arc::clone(&world)));
        let error = service(&handle)
            .execute(input(thread, provider_thread, checkpoint, scope, None))
            .await
            .expect_err("provider selection");
        assert_eq!(error.reason.as_str(), "active-provider-changed");
        assert_eq!(
            error.to_string(),
            format!(
                "Active provider changed before rollback target {checkpoint} could execute on thread {thread}."
            )
        );
        assert!(error.cause.is_none());
        assert_eq!(world.policy_calls.load(Ordering::SeqCst), 0);
        assert_eq!(world.open_calls.load(Ordering::SeqCst), 0);
        assert_eq!(world.restore_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn reports_a_missing_provider_turn_as_a_structured_rollback_failure() {
        let path = cwd();
        let thread = "thread:rollback-provider-turn-unavailable";
        let provider_thread = "provider-thread:rollback-provider-turn-unavailable";
        let checkpoint = "checkpoint:rollback-provider-turn-unavailable";
        let scope = "checkpoint-scope:rollback-provider-turn-unavailable";
        let instance = "provider_rollback_provider_turn_unavailable";
        let projection = projection(json!({
            "thread": thread_value(thread, Some(&path), provider_thread, instance, "test-model"),
            "providerThreads": [provider_thread_value(provider_thread, "provider-session:rollback-provider-turn-unavailable", instance, thread)],
            "checkpoints": [checkpoint_value(checkpoint, thread, scope, "ready", Some(1))],
            "checkpointScopes": [scope_value(scope, thread, &path)],
        }));
        let world = Arc::new(World {
            projection,
            shell: Vec::new(),
            scope_cwds: Vec::new(),
            project_root: None,
            open_calls: AtomicUsize::new(0),
            policy_calls: AtomicUsize::new(0),
            restore_calls: AtomicUsize::new(0),
            allow_open: true,
            allow_policy: true,
            allow_restore: false,
            check_events: false,
            calls: std::sync::Mutex::new(Vec::new()),
            held: AtomicBool::new(false),
            expect_thread: None,
            rollback_ordinal: None,
            provider_thread: None,
        });
        let handle = Arc::new(Handle(Arc::clone(&world)));
        let error = service(&handle)
            .execute(input(thread, provider_thread, checkpoint, scope, None))
            .await
            .expect_err("missing turn");
        assert_eq!(error.reason.as_str(), "provider-turn-unavailable");
        assert_eq!(
            error.to_string(),
            format!(
                "Provider turn for rollback target {checkpoint} is unavailable on provider thread {provider_thread}."
            )
        );
        assert!(error.cause.is_none());
        assert_eq!(world.restore_calls.load(Ordering::SeqCst), 0);
    }

    fn native_turn(ordinal: u64) -> Value {
        json!({
            "driver": "codex",
            "nativeId": format!("native-{ordinal}"),
            "strength": "strong"
        })
    }

    async fn rewinds_safely(restore_files: bool, shared: &str, target_ordinal: u64) {
        let path = cwd();
        let thread = "rewind-files";
        let provider_thread_id = "rewind-provider";
        let instance = "rewind-instance";
        let checkpoint = "rewind-start";
        let scope = "rewind-scope";
        // Upstream writes `appRunOrdinal: targetOrdinal || null`, so ordinal 0
        // is stored as null and the service treats that as thread start.
        let app_run_ordinal = (target_ordinal != 0).then_some(target_ordinal);
        let turns: Vec<Value> = (1..=3)
            .map(|ordinal| {
                json!({
                    "id": format!("turn-{ordinal}"),
                    "providerThreadId": provider_thread_id,
                    "nodeId": format!("node:turn-{ordinal}"),
                    "runAttemptId": format!("attempt-{ordinal}"),
                    // The upstream fixture omits nativeTurnRef. JS
                    // `undefined !== null` still counts the turn. A Rust None
                    // would not, so the ref is set and the oracle matches
                    // `2 - targetOrdinal`.
                    "nativeTurnRef": native_turn(ordinal),
                    "ordinal": ordinal,
                    "status": "completed",
                    "startedAt": null,
                    "completedAt": null
                })
            })
            .collect();
        let attempts: Vec<Value> = (1..=3)
            .map(|ordinal| {
                json!({
                    "id": format!("attempt-{ordinal}"),
                    "runId": format!("run-{ordinal}"),
                    "attemptOrdinal": ordinal,
                    "rootNodeId": format!("node:run-{ordinal}"),
                    "providerInstanceId": instance,
                    "providerThreadId": provider_thread_id,
                    "providerTurnId": null,
                    "reason": "initial",
                    "status": "completed",
                    "startedAt": null,
                    "completedAt": null
                })
            })
            .collect();
        let runs: Vec<Value> = (1..=3)
            .map(|ordinal| {
                json!({
                    "id": format!("run-{ordinal}"),
                    "threadId": thread,
                    "ordinal": ordinal,
                    "providerInstanceId": instance,
                    "modelSelection": {"instanceId": instance, "model": "test"},
                    "providerThreadId": provider_thread_id,
                    "userMessageId": format!("message:run-{ordinal}"),
                    "rootNodeId": null,
                    "activeAttemptId": format!("attempt-{ordinal}"),
                    "status": if ordinal == 3 { "rolled_back" } else { "completed" },
                    "requestedAt": T0,
                    "startedAt": T0,
                    "completedAt": T0,
                    "checkpointId": null,
                    "contextHandoffId": null
                })
            })
            .collect();
        let worktree = if shared == "root" {
            None
        } else {
            Some(path.as_str())
        };
        let provider_thread =
            provider_thread_value(provider_thread_id, "rewind-session", instance, thread);
        let decoded_thread: OrchestrationV2ProviderThread = decode(provider_thread.clone());
        let shell = if shared == "worktree" || shared == "historical" {
            vec![ShellThreadClaim {
                id: ThreadId::new("other-thread").unwrap(),
                project_id: ProjectId::new("project:test").unwrap(),
                worktree_path: (shared == "worktree").then(|| path.clone()),
                deleted_at: None,
            }]
        } else {
            Vec::new()
        };
        let world = Arc::new(World {
            projection: projection(json!({
                "thread": thread_value(thread, worktree, provider_thread_id, instance, "test"),
                "providerThreads": [provider_thread],
                "providerTurns": turns,
                "nodes": [],
                "attempts": attempts,
                "checkpoints": [checkpoint_value(checkpoint, thread, scope, "ready", app_run_ordinal)],
                "checkpointScopes": [scope_value(scope, thread, &path)],
                "runs": runs,
            })),
            shell,
            scope_cwds: vec![path.clone()],
            project_root: Some("/nonexistent/t3-rollback-project".to_owned()),
            open_calls: AtomicUsize::new(0),
            policy_calls: AtomicUsize::new(0),
            restore_calls: AtomicUsize::new(0),
            allow_open: true,
            allow_policy: true,
            allow_restore: true,
            check_events: true,
            calls: std::sync::Mutex::new(Vec::new()),
            held: AtomicBool::new(false),
            expect_thread: Some(ThreadId::new(thread).unwrap()),
            rollback_ordinal: Some(target_ordinal),
            provider_thread: Some(decoded_thread),
        });
        let handle = Arc::new(Handle(Arc::clone(&world)));
        let service = service(&handle);
        let run = service.execute(input(
            thread,
            provider_thread_id,
            checkpoint,
            scope,
            Some(restore_files),
        ));
        if restore_files && shared != "none" {
            let error = run.await.expect_err("shared workspace");
            assert_eq!(error.reason.as_str(), "shared-workspace");
            assert_eq!(error.to_string(), SHARED_WORKSPACE_RESTORE_MESSAGE);
            assert!(world.calls.lock().expect("calls").is_empty());
            return;
        }
        run.await.expect("rewind");
        let calls = world.calls.lock().expect("calls").clone();
        if restore_files {
            assert_eq!(calls, ["provider", "files", "projection"]);
        } else {
            assert_eq!(calls, ["provider", "projection"]);
        }
    }

    #[tokio::test]
    async fn rewinds_files_in_an_isolated_worktree_to_thread_start() {
        rewinds_safely(true, "none", 0).await;
    }

    #[tokio::test]
    async fn rewinds_without_files_when_the_thread_has_no_worktree() {
        rewinds_safely(false, "root", 0).await;
    }

    #[tokio::test]
    async fn refuses_file_restore_when_the_thread_has_no_worktree() {
        rewinds_safely(true, "root", 0).await;
    }

    #[tokio::test]
    async fn refuses_file_restore_when_another_thread_uses_the_worktree() {
        rewinds_safely(true, "worktree", 0).await;
    }

    #[tokio::test]
    async fn rewinds_without_files_when_another_thread_uses_the_worktree() {
        rewinds_safely(false, "worktree", 0).await;
    }

    #[tokio::test]
    async fn refuses_file_restore_when_an_archived_thread_checkpoint_overlaps() {
        rewinds_safely(true, "historical", 0).await;
    }

    #[tokio::test]
    async fn rewinds_to_a_provider_turn_without_restoring_files() {
        rewinds_safely(false, "none", 1).await;
    }

    #[tokio::test]
    async fn rejects_an_archived_thread_sharing_a_worktree_through_a_symlink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path();
        let alias = cwd.join("alias");
        std::os::unix::fs::symlink(cwd, &alias).expect("symlink");
        let thread = ThreadId::new("restore-alias-current").unwrap();
        let other = ThreadId::new("restore-alias-archived").unwrap();
        let world = Arc::new(World {
            projection: projection(json!({
                "thread": thread_value("restore-alias-current", None, "provider-thread:unused", "codex", "test"),
            })),
            shell: vec![ShellThreadClaim {
                id: other,
                project_id: ProjectId::new("project:unrelated").unwrap(),
                worktree_path: Some(alias.to_string_lossy().into_owned()),
                deleted_at: None,
            }],
            scope_cwds: Vec::new(),
            project_root: Some("/nonexistent/t3-rollback-project".to_owned()),
            open_calls: AtomicUsize::new(0),
            policy_calls: AtomicUsize::new(0),
            restore_calls: AtomicUsize::new(0),
            allow_open: false,
            allow_policy: false,
            allow_restore: false,
            check_events: false,
            calls: std::sync::Mutex::new(Vec::new()),
            held: AtomicBool::new(false),
            expect_thread: None,
            rollback_ordinal: None,
            provider_thread: None,
        });
        let handle = Handle(world);
        let isolated = is_checkpoint_restore_isolated(
            &thread,
            Some(cwd.to_str().expect("utf8 path")),
            cwd.to_str().expect("utf8 path"),
            &handle,
        )
        .await
        .expect("isolation");
        assert!(!isolated);
    }
}
