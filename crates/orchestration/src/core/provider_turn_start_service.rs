//! Port of upstream/apps/server/src/orchestration-v2/ProviderTurnStartService.ts.
//!
//! The consumer of the `provider-turn.start` effect: it moves a `starting`
//! run to `running` and hands it to `RunExecutionService`. Before that it
//! settles runs that never reach a provider (local commands, a session or
//! thread that will not load), opens the provider session, loads or forks the
//! provider thread, and prepares any context to deliver with the turn.

use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use turin_contracts::{
    CommandId, ProjectId, RunAttemptId, RunId, ThreadId, TurnItemId, UtcDateTime,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2AppThread, OrchestrationV2ContextHandoff,
        OrchestrationV2ContextHandoffDeliveryStatus, OrchestrationV2ContextHandoffStatus,
        OrchestrationV2ContextHandoffStrategy, OrchestrationV2ContextTransfer,
        OrchestrationV2ContextTransferResolution, OrchestrationV2ContextTransferStatus,
        OrchestrationV2ContextTransferType, OrchestrationV2DomainEvent,
        OrchestrationV2ProviderFailureClass, OrchestrationV2ProviderThread,
        OrchestrationV2ProviderThreadStatus, OrchestrationV2ProviderTurn,
        OrchestrationV2RunAttempt, OrchestrationV2RunStatus, OrchestrationV2ThreadProjection,
        OrchestrationV2TurnItem,
    },
    provider_instance::ProviderInstanceId,
};
use turin_provider_core::{
    failure::{ProviderFailureCause, ProviderFailureInput, make_provider_failure},
    handoff_budget::{
        ContextUsageForHandoffInput, DEFAULT_HANDOFF_TOKEN_CAP, context_usage_for_handoff,
        handoff_token_cap_config, latest_native_context_usage,
    },
    id_allocator::{IdAllocator, derive},
    provider_adapter::{
        ProviderAdapterErrorKind, ProviderAdapterV2EnsureThreadInput, ProviderAdapterV2Error,
        ProviderAdapterV2ForkThreadInput, ProviderAdapterV2OpenSessionInput,
        ProviderAdapterV2ResumeThreadInput, ProviderAdapterV2RuntimePolicy,
        ProviderAdapterV2TurnMessage, ProviderSessionRuntime,
    },
};

use super::{
    context_handoff_service::{
        ContextHandoffService, PrepareProviderHandoffInput, ProviderHandoffStrategy,
    },
    orchestrator::services::{ProjectRoots, TurnStartInput},
    restart_background_note::{
        PendingRestartWork, ProviderTurnState, RunAttemptRef, is_restart_note_continuation,
        pending_restart_cancelled_background_work, restart_cancelled_background_work_note,
    },
    run_execution_service::{
        AsyncBool, InheritedBackgroundLoader, RunEventWriter, RunExecutionService,
        RunExecutionServiceError, StartRootRunInput, can_route_related_subagent, failure_cause,
        select_inherited_background_turn_items,
    },
};
use crate::{
    pipeline::{
        event_sink::{RunCurrentWrite, WriteInput},
        orchestrator_port::BoxError,
        seams::ProjectDirectory,
    },
    projection::{
        error::ProjectionStoreError,
        store::{
            ProjectionStore, canonical::ProjectionRecordField, contexts::TurnStartContext,
            runtime_recovery::RuntimeRecoveryState,
        },
    },
};

mod handoff_session;
mod settle;

use handoff_session::{DeliverySession, EstimateInput, HandoffPlan};
use settle::{RunRows, SettleInput, SettleItem, SettledStatus};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// `ProviderTurnStartError`.
#[derive(Debug, thiserror::Error)]
#[error("Failed to start the provider turn for run {run_id}.")]
pub struct ProviderTurnStartError {
    pub run_id: RunId,
    #[source]
    pub cause: BoxError,
}

/// A failure that already is a [`ProviderTurnStartError`] keeps its identity
/// (`isProviderTurnStartError`); anything else is wrapped.
fn start_error(run_id: &RunId, cause: BoxError) -> BoxError {
    if cause.is::<ProviderTurnStartError>() {
        cause
    } else {
        Box::new(ProviderTurnStartError {
            run_id: run_id.clone(),
            cause,
        })
    }
}

/// A plain-text cause, as upstream passes a string to `ProviderTurnStartError`.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Reason(String);

// ---------------------------------------------------------------------------
// Seams
// ---------------------------------------------------------------------------

/// The `ProjectionStoreV2` reads a turn start makes.
#[async_trait]
pub trait TurnStartReads: Send + Sync {
    async fn turn_start_context(
        &self,
        thread_id: &ThreadId,
        run_id: &RunId,
    ) -> Result<TurnStartContext, BoxError>;
    async fn runtime_recovery_projection(
        &self,
        thread_id: &ThreadId,
    ) -> Result<RuntimeRecoveryState, BoxError>;
    async fn turn_start_history(
        &self,
        thread_id: &ThreadId,
        run_ids: Option<&[RunId]>,
    ) -> Result<Vec<OrchestrationV2TurnItem>, BoxError>;
    /// `getThreadRecords` for a native fork's source thread.
    async fn thread_records(
        &self,
        thread_id: &ThreadId,
        fields: Vec<ProjectionRecordField>,
    ) -> Result<OrchestrationV2ThreadProjection, BoxError>;
    async fn has_unpaired_run_interrupt_request(
        &self,
        thread_id: &ThreadId,
        request_id: &str,
        result_id: &str,
    ) -> Result<bool, BoxError>;
}

fn boxed(error: ProjectionStoreError) -> BoxError {
    Box::new(error)
}

#[async_trait]
impl TurnStartReads for ProjectionStore {
    async fn turn_start_context(
        &self,
        thread_id: &ThreadId,
        run_id: &RunId,
    ) -> Result<TurnStartContext, BoxError> {
        self.get_turn_start_context(thread_id, run_id)
            .await
            .map_err(boxed)
    }

    async fn runtime_recovery_projection(
        &self,
        thread_id: &ThreadId,
    ) -> Result<RuntimeRecoveryState, BoxError> {
        self.get_runtime_recovery_projection(thread_id)
            .await
            .map_err(boxed)
    }

    async fn turn_start_history(
        &self,
        thread_id: &ThreadId,
        run_ids: Option<&[RunId]>,
    ) -> Result<Vec<OrchestrationV2TurnItem>, BoxError> {
        self.get_turn_start_history(thread_id, run_ids)
            .await
            .map_err(boxed)
    }

    async fn thread_records(
        &self,
        thread_id: &ThreadId,
        fields: Vec<ProjectionRecordField>,
    ) -> Result<OrchestrationV2ThreadProjection, BoxError> {
        self.get_thread_records(thread_id, fields, None)
            .await
            .map_err(boxed)
    }

    async fn has_unpaired_run_interrupt_request(
        &self,
        thread_id: &ThreadId,
        request_id: &str,
        result_id: &str,
    ) -> Result<bool, BoxError> {
        ProjectionStore::has_unpaired_run_interrupt_request(self, thread_id, request_id, result_id)
            .await
            .map_err(boxed)
    }
}

/// `ProviderSessionManagerV2.open`.
#[async_trait]
pub trait ProviderSessionOpener: Send + Sync {
    async fn open(
        &self,
        input: ProviderAdapterV2OpenSessionInput,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, BoxError>;
}

#[async_trait]
impl ProviderSessionOpener for super::provider_session_manager::ProviderSessionManager {
    async fn open(
        &self,
        input: ProviderAdapterV2OpenSessionInput,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, BoxError> {
        super::provider_session_manager::ProviderSessionManager::open(self, input)
            .await
            .map_err(|error| Box::new(error) as BoxError)
    }
}

/// `RunExecutionServiceV2.startRootRun`.
#[async_trait]
pub trait RootRunStarter: Send + Sync {
    async fn start_root_run(&self, input: StartRootRunInput) -> Result<(), BoxError>;
}

#[async_trait]
impl RootRunStarter for RunExecutionService {
    async fn start_root_run(&self, input: StartRootRunInput) -> Result<(), BoxError> {
        RunExecutionService::start_root_run(self, input)
            .await
            .map_err(|error: RunExecutionServiceError| Box::new(error) as BoxError)
    }
}

/// `RuntimePolicyV2.resolve`.
#[async_trait]
pub trait RuntimePolicyResolver: Send + Sync {
    async fn resolve(
        &self,
        thread: &OrchestrationV2AppThread,
        model_selection: &ModelSelection,
    ) -> Result<ProviderAdapterV2RuntimePolicy, BoxError>;
}

/// `RuntimePolicyV2.layerFromProjectStore`: the working directory is the
/// thread's worktree, else its project's workspace root. The supported
/// runtime modes come from the provider's snapshot upstream; no snapshot is
/// read here, so the thread's own mode is used (as message dispatch does).
pub struct ProjectRootsRuntimePolicy {
    roots: Arc<dyn ProjectRoots>,
}

impl ProjectRootsRuntimePolicy {
    pub fn new(roots: Arc<dyn ProjectRoots>) -> Self {
        Self { roots }
    }
}

#[async_trait]
impl RuntimePolicyResolver for ProjectRootsRuntimePolicy {
    async fn resolve(
        &self,
        thread: &OrchestrationV2AppThread,
        model_selection: &ModelSelection,
    ) -> Result<ProviderAdapterV2RuntimePolicy, BoxError> {
        let root = match &thread.worktree_path {
            Some(_) => String::new(),
            None => self
                .roots
                .workspace_root(&thread.project_id)
                .await?
                .ok_or_else(|| {
                    format!(
                        "Failed to resolve runtime policy for provider instance {} in project {}: Project not found.",
                        model_selection.instance_id, thread.project_id
                    )
                })?,
        };
        let policy = crate::pipeline::runtime_policy::resolve(
            thread,
            &root,
            None,
            &crate::pipeline::runtime_policy::RuntimePolicyOverride::default(),
        );
        Ok(ProviderAdapterV2RuntimePolicy {
            runtime_mode: policy.runtime_mode,
            interaction_mode: policy.interaction_mode,
            cwd: Some(policy.cwd),
            approval_policy: policy.approval_policy,
            sandbox_policy: policy.sandbox_policy,
            reasoning_effort: policy.reasoning_effort,
        })
    }
}

/// `ProviderAuthService.tryHandlePromptCommand` failed (`ProviderSetupError`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct PromptCommandFailure {
    pub detail: String,
}

/// `ProviderAuthService.tryHandlePromptCommand`: `Ok(true)` when the text was
/// an account command the provider handled itself.
#[async_trait]
pub trait PromptCommands: Send + Sync {
    async fn try_handle_prompt_command(
        &self,
        instance_id: &ProviderInstanceId,
        text: &str,
        has_attachments: bool,
    ) -> Result<bool, PromptCommandFailure>;
}

/// [`PromptCommands`] until provider account commands are wired: nothing is handled.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoPromptCommands;

#[async_trait]
impl PromptCommands for NoPromptCommands {
    async fn try_handle_prompt_command(
        &self,
        _instance_id: &ProviderInstanceId,
        _text: &str,
        _has_attachments: bool,
    ) -> Result<bool, PromptCommandFailure> {
        Ok(false)
    }
}

/// The filesystem, project and git reads of the missing-worktree repair.
#[async_trait]
pub trait TurnStartWorkspace: Send + Sync {
    /// `fileSystem.exists`, where a failed check counts as existing.
    async fn path_exists(&self, path: &str) -> bool;
    /// `projects.getById`, where a failed lookup counts as no project.
    async fn project_workspace_root(&self, project_id: &ProjectId) -> Option<String>;
    /// `gitWorkflow.pruneWorktrees`, then `createWorktree` for `ref_name` at `path`.
    async fn recreate_worktree(
        &self,
        cwd: &str,
        ref_name: &str,
        path: &str,
    ) -> Result<(), BoxError>;
}

/// [`TurnStartWorkspace`] over the local disk, the project directory and git.
pub struct LocalTurnStartWorkspace {
    projects: Arc<dyn ProjectDirectory>,
    git: Option<Arc<turin_git::git_workflow_service::GitWorkflowService>>,
}

impl LocalTurnStartWorkspace {
    pub fn new(
        projects: Arc<dyn ProjectDirectory>,
        git: Option<Arc<turin_git::git_workflow_service::GitWorkflowService>>,
    ) -> Self {
        Self { projects, git }
    }
}

#[async_trait]
impl TurnStartWorkspace for LocalTurnStartWorkspace {
    async fn path_exists(&self, path: &str) -> bool {
        tokio::fs::try_exists(path).await.unwrap_or(true)
    }

    async fn project_workspace_root(&self, project_id: &ProjectId) -> Option<String> {
        self.projects
            .get(project_id)
            .await
            .ok()
            .flatten()
            .map(|project| project.workspace_root)
    }

    async fn recreate_worktree(
        &self,
        cwd: &str,
        ref_name: &str,
        path: &str,
    ) -> Result<(), BoxError> {
        let Some(git) = &self.git else {
            return Err("git workflow is not wired".into());
        };
        git.prune_worktrees(cwd).await?;
        let trimmed = |value: &str| {
            turin_contracts::TrimmedNonEmptyString::new(value).map_err(|error| error.to_string())
        };
        git.create_worktree(
            &turin_contracts::git::VcsCreateWorktreeInput {
                cwd: trimmed(cwd)?,
                ref_name: trimmed(ref_name)?,
                new_ref_name: None,
                base_ref_name: None,
                path: Some(trimmed(path)?),
            },
            Default::default(),
        )
        .await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

/// `ProviderTurnStartServiceV2`.
pub struct ProviderTurnStartService {
    events: Arc<dyn RunEventWriter>,
    handoffs: ContextHandoffService,
    reads: Arc<dyn TurnStartReads>,
    sessions: Arc<dyn ProviderSessionOpener>,
    run_execution: Arc<dyn RootRunStarter>,
    runtime_policy: Arc<dyn RuntimePolicyResolver>,
    prompt_commands: Arc<dyn PromptCommands>,
    workspace: Arc<dyn TurnStartWorkspace>,
}

impl ProviderTurnStartService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        events: Arc<dyn RunEventWriter>,
        handoffs: ContextHandoffService,
        reads: Arc<dyn TurnStartReads>,
        sessions: Arc<dyn ProviderSessionOpener>,
        run_execution: Arc<dyn RootRunStarter>,
        runtime_policy: Arc<dyn RuntimePolicyResolver>,
        prompt_commands: Arc<dyn PromptCommands>,
        workspace: Arc<dyn TurnStartWorkspace>,
    ) -> Self {
        Self {
            events,
            handoffs,
            reads,
            sessions,
            run_execution,
            runtime_policy,
            prompt_commands,
            workspace,
        }
    }

    /// `start`. When `will_retry` is true, a session open failure is returned
    /// so the caller can retry. Otherwise the run is settled as failed.
    pub async fn start(&self, input: TurnStartInput) -> Result<(), BoxError> {
        let run_id = input.run_id.clone();
        self.start_inner(input)
            .await
            .map_err(|cause| start_error(&run_id, cause))
    }

    /// `makeRunControls`. These callbacks outlive startup while a run drains
    /// background work, so they are built outside `start`'s scope and hold
    /// only the ids, never the thread's history.
    fn make_run_controls(&self, controls: RunControlsInput) -> RunControls {
        let reads = self.reads.clone();
        let is_current_attempt_in_status = {
            let reads = reads.clone();
            let thread_id = controls.thread_id.clone();
            let run_id = controls.run_id.clone();
            let attempt_id = controls.attempt_id.clone();
            move |accepts: fn(OrchestrationV2RunStatus) -> bool| {
                let reads = reads.clone();
                let thread_id = thread_id.clone();
                let run_id = run_id.clone();
                let attempt_id = attempt_id.clone();
                Box::pin(async move {
                    // `false` means the run moved on or is gone. A failed read is
                    // an error, so the caller fails the start or the run instead
                    // of skipping it.
                    let current = reads.runtime_recovery_projection(&thread_id).await?;
                    Ok(current.runs.iter().any(|run| {
                        run.id == run_id
                            && run.active_attempt_id.as_ref() == Some(&attempt_id)
                            && accepts(run.status)
                    }))
                })
                    as turin_provider_core::rt::BoxFuture<'static, Result<bool, BoxError>>
            }
        };
        let in_status_running = {
            let check = is_current_attempt_in_status.clone();
            Arc::new(move || check(|status| status == OrchestrationV2RunStatus::Running))
                as AsyncBool
        };
        let in_status_starting = {
            let check = is_current_attempt_in_status.clone();
            Arc::new(move || check(|status| status == OrchestrationV2RunStatus::Starting))
                as AsyncBool
        };
        let should_finalize_run = {
            let check = is_current_attempt_in_status;
            Arc::new(move || {
                check(|status| {
                    matches!(
                        status,
                        OrchestrationV2RunStatus::Starting | OrchestrationV2RunStatus::Running
                    )
                })
            }) as AsyncBool
        };
        let load_inherited_background_turn_items: InheritedBackgroundLoader = {
            let reads = reads.clone();
            let thread_id = controls.thread_id.clone();
            let provider_thread_id = controls.provider_thread_id.clone();
            let run_ordinal = controls.run_ordinal;
            let fallback = controls.inherited_background_turn_items;
            Arc::new(move || {
                let reads = reads.clone();
                let thread_id = thread_id.clone();
                let provider_thread_id = provider_thread_id.clone();
                let fallback = fallback.clone();
                Box::pin(async move {
                    Ok(match reads.runtime_recovery_projection(&thread_id).await {
                        Ok(current) => select_inherited_background_turn_items(
                            &thread_id,
                            &provider_thread_id,
                            run_ordinal,
                            &current.runs,
                            &current.turn_items,
                        ),
                        Err(_) => fallback,
                    })
                })
            })
        };
        let has_unpaired_run_interrupt_request: AsyncBool = {
            let reads = reads.clone();
            let thread_id = controls.thread_id;
            let run_id = controls.run_id;
            Arc::new(move || {
                let reads = reads.clone();
                let thread_id = thread_id.clone();
                let run_id = run_id.clone();
                Box::pin(async move {
                    Ok(reads
                        .has_unpaired_run_interrupt_request(
                            &thread_id,
                            derive::run_signal_turn_item(&run_id, "interrupt-request").as_str(),
                            derive::run_signal_turn_item(&run_id, "interrupt-result").as_str(),
                        )
                        .await
                        .unwrap_or(false))
                })
            })
        };
        RunControls {
            is_current_attempt_starting: in_status_starting,
            is_current_attempt_running: in_status_running,
            load_inherited_background_turn_items,
            should_finalize_run,
            has_unpaired_run_interrupt_request,
        }
    }

    async fn start_inner(&self, input: TurnStartInput) -> Result<(), BoxError> {
        let run_id = input.run_id.clone();
        let projection = self
            .reads
            .turn_start_context(&input.thread_id, &run_id)
            .await?;
        let Some(run) = projection
            .runs
            .iter()
            .find(|candidate| candidate.id == run_id)
        else {
            return Err(Box::new(ProviderTurnStartError {
                run_id: run_id.clone(),
                cause: Box::new(Reason(format!("Run {run_id} was not found."))),
            }));
        };
        if run.status != OrchestrationV2RunStatus::Starting {
            // The effect is idempotent once the run has advanced or terminalized.
            return Ok(());
        }
        let root_node = run.root_node_id.as_ref().and_then(|id| {
            projection
                .nodes
                .iter()
                .find(|candidate| &candidate.id == id)
        });
        let attempt = run.active_attempt_id.as_ref().and_then(|id| {
            projection
                .attempts
                .iter()
                .find(|candidate| &candidate.id == id)
        });
        let provider_thread = run.provider_thread_id.as_ref().and_then(|id| {
            projection
                .provider_threads
                .iter()
                .find(|candidate| &candidate.id == id)
        });
        let message = projection
            .messages
            .iter()
            .find(|candidate| candidate.id == run.user_message_id);
        let checkpoint_scope = root_node
            .and_then(|node| node.checkpoint_scope_id.as_ref())
            .and_then(|id| {
                projection
                    .checkpoint_scopes
                    .iter()
                    .find(|candidate| &candidate.id == id)
            });
        let handoffs = ready_handoffs(&projection, run);
        let native_fork_transfer = projection.context_transfers.iter().find(|transfer| {
            transfer.r#type == OrchestrationV2ContextTransferType::Fork
                && transfer.target_thread_id == input.thread_id
                && transfer.target_run_id.as_ref() == Some(&run.id)
                && transfer.status == OrchestrationV2ContextTransferStatus::Pending
                && transfer.resolution.is_none()
        });
        let (
            Some(root_node),
            Some(attempt),
            Some(provider_thread),
            Some(message),
            Some(checkpoint_scope),
        ) = (
            root_node,
            attempt,
            provider_thread,
            message,
            checkpoint_scope,
        )
        else {
            return Err(Box::new(ProviderTurnStartError {
                run_id: run_id.clone(),
                cause: Box::new(Reason(format!(
                    "Run {run_id} is missing its execution projection state."
                ))),
            }));
        };
        let Some(provider_session_id) = provider_thread.provider_session_id.clone() else {
            return Err(Box::new(ProviderTurnStartError {
                run_id: run_id.clone(),
                cause: Box::new(Reason(format!(
                    "Run {run_id} is missing its execution projection state."
                ))),
            }));
        };
        let rows = RunRows {
            projection: &projection,
            run,
            attempt,
            root_node,
        };

        if message.attachments.is_empty() && message.text.trim_start().starts_with('/') {
            let is_empty_compaction =
                turin_contracts::base_schemas::js_trim(&message.text).to_lowercase() == "/compact"
                    && !projection.has_conversation;
            // Preparing a run may already point the thread at a newly selected
            // provider. Account commands still belong to its last native session.
            let native_threads: Vec<&OrchestrationV2ProviderThread> = projection
                .provider_threads
                .iter()
                .filter(|candidate| {
                    candidate.owner_node_id.is_none() && candidate.native_thread_ref.is_some()
                })
                .collect();
            let previous_native_run = projection
                .runs
                .iter()
                .filter(|candidate| {
                    candidate.ordinal.get() < run.ordinal.get()
                        && candidate
                            .provider_thread_id
                            .as_ref()
                            .is_some_and(|id| native_threads.iter().any(|thread| &thread.id == id))
                })
                .max_by_key(|candidate| candidate.ordinal.get());
            let native_thread_id = previous_native_run
                .and_then(|previous| previous.provider_thread_id.as_ref())
                .or(projection.thread.active_provider_thread_id.as_ref())
                .unwrap_or(&provider_thread.id);
            let native_thread = native_threads
                .iter()
                .find(|thread| &thread.id == native_thread_id)
                .copied();
            let auth_instance_id = native_thread
                .map(|thread| thread.provider_instance_id.clone())
                .unwrap_or_else(|| run.provider_instance_id.clone());
            let auth_result = if is_empty_compaction {
                None
            } else {
                let records = message
                    .context
                    .as_ref()
                    .map(|context| context.records.as_slice())
                    .unwrap_or_default();
                Some(
                    self.prompt_commands
                        .try_handle_prompt_command(
                            &auth_instance_id,
                            &turin_shared::composer_context_references::project_composer_context_for_provider(
                                &message.text,
                                records,
                            ),
                            false,
                        )
                        .await,
                )
            };
            if is_empty_compaction || matches!(auth_result, Some(Err(_)) | Some(Ok(true))) {
                let now = UtcDateTime::now();
                let failure = if is_empty_compaction {
                    Some(make_provider_failure(ProviderFailureInput {
                        cause: None,
                        message: Some("Start a conversation before compacting this thread."),
                        code: None,
                        class: Some(OrchestrationV2ProviderFailureClass::ValidationError),
                        retryable: None,
                        reset_at: None,
                    }))
                } else if let Some(Err(error)) = &auth_result {
                    Some(make_provider_failure(ProviderFailureInput {
                        cause: None,
                        message: Some(&error.detail),
                        code: None,
                        class: Some(OrchestrationV2ProviderFailureClass::PermissionError),
                        retryable: None,
                        reset_at: None,
                    }))
                } else {
                    None
                };
                let mut idle_thread = provider_thread.clone();
                idle_thread.status = if provider_thread.native_thread_ref.is_none() {
                    OrchestrationV2ProviderThreadStatus::NotLoaded
                } else {
                    OrchestrationV2ProviderThreadStatus::Idle
                };
                idle_thread.updated_at = now;
                self.settle_run_before_start(
                    &rows,
                    SettleInput {
                        signal: if is_empty_compaction {
                            "empty-compaction"
                        } else {
                            "provider-sign-out"
                        },
                        status: if failure.is_none() {
                            SettledStatus::Completed
                        } else {
                            SettledStatus::Failed
                        },
                        now,
                        started_at: Some(now),
                        provider_instance_id: auth_instance_id,
                        item_provider_thread_id: native_thread
                            .map(|thread| thread.id.clone())
                            .unwrap_or_else(|| provider_thread.id.clone()),
                        item: match failure {
                            Some(failure) => SettleItem::Error {
                                title: if is_empty_compaction {
                                    "Cannot compact an empty thread"
                                } else {
                                    "Provider sign-out failed"
                                }
                                .to_owned(),
                                failure,
                            },
                            None => SettleItem::Command {
                                title: "Provider signed out".to_owned(),
                                input: turin_contracts::base_schemas::js_trim(&message.text)
                                    .to_owned(),
                                output: "Provider signed out".to_owned(),
                                exit_code: 0,
                            },
                        },
                        provider_thread_update: Some(idle_thread),
                    },
                )
                .await?;
                return Ok(());
            }
        }

        if let (Some(worktree_path), Some(branch)) =
            (&projection.thread.worktree_path, &projection.thread.branch)
            && !self.workspace.path_exists(worktree_path.as_str()).await
            && let Some(workspace_root) = self
                .workspace
                .project_workspace_root(&projection.thread.project_id)
                .await
        {
            tracing::warn!(
                thread_id = %projection.thread.id,
                worktree_path = %worktree_path,
                branch = %branch,
                "provider turn start recreating missing worktree"
            );
            if let Err(cause) = self
                .workspace
                .recreate_worktree(&workspace_root, branch.as_str(), worktree_path.as_str())
                .await
            {
                tracing::warn!(
                    thread_id = %projection.thread.id,
                    worktree_path = %worktree_path,
                    error = %cause,
                    "provider turn start failed to recreate worktree"
                );
            }
        }

        let inherited_background_turn_items = {
            let current = self
                .reads
                .runtime_recovery_projection(&projection.thread.id)
                .await?;
            select_inherited_background_turn_items(
                &current.thread.id,
                &provider_thread.id,
                run.ordinal.get(),
                &current.runs,
                &current.turn_items,
            )
        };
        let controls = self.make_run_controls(RunControlsInput {
            thread_id: projection.thread.id.clone(),
            run_id: run.id.clone(),
            attempt_id: attempt.id.clone(),
            provider_thread_id: provider_thread.id.clone(),
            run_ordinal: run.ordinal.get(),
            inherited_background_turn_items,
        });

        let resolved_runtime_policy = self
            .runtime_policy
            .resolve(&projection.thread, &run.model_selection)
            .await?;
        let existing_session_projection = projection
            .provider_sessions
            .iter()
            .find(|candidate| candidate.id == provider_session_id);
        let session_result = self
            .sessions
            .open(ProviderAdapterV2OpenSessionInput {
                thread_id: projection.thread.id.clone(),
                provider_session_id: provider_session_id.clone(),
                model_selection: run.model_selection.clone(),
                runtime_policy: resolved_runtime_policy.clone(),
                resume_from_session: existing_session_projection.cloned(),
                initial_native_thread_id: provider_thread
                    .native_thread_ref
                    .as_ref()
                    .and_then(|reference| reference.native_id.as_ref())
                    .map(|id| id.as_str().to_owned()),
                initial_provider_item_identity_version: provider_thread
                    .native_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.item_identity_version.map(|_| 2)),
            })
            .await;

        // The last start attempt fails the run with the provider's own reason
        // instead of leaving it `starting` after the effect gives up. A run
        // that already left `starting` is not overwritten, and a failed write
        // returns its error to the effect worker.
        let settle_start_failure = |signal: &'static str, title: &'static str, error: BoxError| {
            let rows = &rows;
            async move {
                let message = nested_cause_message(&*error);
                self.settle_run_before_start(
                    rows,
                    SettleInput {
                        signal,
                        status: SettledStatus::Failed,
                        now: UtcDateTime::now(),
                        started_at: None,
                        provider_instance_id: run.provider_instance_id.clone(),
                        item_provider_thread_id: provider_thread.id.clone(),
                        item: SettleItem::Error {
                            title: title.to_owned(),
                            failure: make_provider_failure(ProviderFailureInput {
                                cause: Some(&failure_cause(&*error)),
                                message: Some(&message),
                                code: None,
                                class: Some(OrchestrationV2ProviderFailureClass::ProviderError),
                                retryable: None,
                                reset_at: None,
                            }),
                        },
                        provider_thread_update: None,
                    },
                )
                .await
            }
        };
        let session = match session_result {
            Ok(session) => session,
            Err(failure) => {
                if input.will_retry {
                    return Err(failure);
                }
                settle_start_failure(
                    "provider-session-open-failure",
                    "Provider session failed to open",
                    failure,
                )
                .await?;
                return Ok(());
            }
        };

        // Only the provider's own thread load fails the run on the last
        // attempt; store, id and handoff failures around it keep their typed
        // errors.
        let load_from_provider =
            |loaded: Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error>| {
                let settle = &settle_start_failure;
                let will_retry = input.will_retry;
                async move {
                    match loaded {
                        Ok(thread) => Ok(Some(thread)),
                        Err(failure) => {
                            let failure: BoxError = Box::new(failure);
                            if will_retry {
                                return Err(failure);
                            }
                            settle(
                                "provider-thread-load-failure",
                                "Provider turn failed to start",
                                failure,
                            )
                            .await?;
                            Ok(None)
                        }
                    }
                }
            };

        let mut effective_handoffs = handoffs;
        let loaded_provider_thread: Option<OrchestrationV2ProviderThread> = 'load: {
            if let Some(transfer) = native_fork_transfer {
                let source_projection = self
                    .reads
                    .thread_records(
                        &transfer.source_thread_id,
                        vec![
                            ProjectionRecordField::Runs,
                            ProjectionRecordField::ProviderThreads,
                            ProjectionRecordField::Attempts,
                            ProjectionRecordField::ProviderTurns,
                        ],
                    )
                    .await?;
                let source_run_id = source_point_run_id(transfer);
                let source_run = source_run_id.and_then(|id| {
                    source_projection
                        .runs
                        .iter()
                        .find(|candidate| &candidate.id == id)
                });
                let source_provider_thread = source_run
                    .and_then(|source| source.provider_thread_id.as_ref())
                    .and_then(|id| {
                        source_projection
                            .provider_threads
                            .iter()
                            .find(|candidate| &candidate.id == id)
                    });
                let source_attempt = source_run
                    .and_then(|source| source.active_attempt_id.as_ref())
                    .and_then(|id| {
                        source_projection
                            .attempts
                            .iter()
                            .find(|candidate| &candidate.id == id)
                    });
                let source_provider_turn: Option<&OrchestrationV2ProviderTurn> = source_attempt
                    .and_then(|attempt| {
                        turin_contracts::orchestration_v2::latest_provider_turn_for_attempt(
                            &source_projection.provider_turns,
                            Some(&attempt.id),
                        )
                    })
                    .or_else(|| {
                        source_attempt
                            .and_then(|attempt| attempt.provider_turn_id.as_ref())
                            .and_then(|id| {
                                source_projection
                                    .provider_turns
                                    .iter()
                                    .find(|candidate| &candidate.id == id)
                            })
                    });
                let (Some(_), Some(source_provider_thread)) = (source_run, source_provider_thread)
                else {
                    return Err(Box::new(ProviderTurnStartError {
                        run_id: run_id.clone(),
                        cause: Box::new(Reason(format!(
                            "Native fork transfer {} has no source provider execution.",
                            transfer.id
                        ))),
                    }));
                };
                break 'load load_from_provider(
                    session
                        .fork_thread(ProviderAdapterV2ForkThreadInput {
                            source_provider_thread: source_provider_thread.clone(),
                            source_provider_turns: Some(source_projection.provider_turns.clone()),
                            provider_turn_id: source_provider_turn.map(|turn| turn.id.clone()),
                            target_thread_id: projection.thread.id.clone(),
                            owner_node_id: None,
                            model_selection: Some(run.model_selection.clone()),
                            runtime_policy: Some(resolved_runtime_policy.clone()),
                        })
                        .await,
                )
                .await?;
            }
            let ensure =
                |existing: OrchestrationV2ProviderThread| ProviderAdapterV2EnsureThreadInput {
                    thread_id: projection.thread.id.clone(),
                    title: Some(projection.thread.title.to_string()),
                    model_selection: run.model_selection.clone(),
                    runtime_policy: resolved_runtime_policy.clone(),
                    provider_session_id: Some(provider_session_id.clone()),
                    existing_provider_thread: Some(existing),
                };
            if provider_thread.native_thread_ref.is_none() {
                // Hand the run's provider thread to the adapter so it adopts this
                // row's identity when attaching native state. An adapter that mints
                // its own row instead leaves two live rows per app thread, and
                // `activeProviderThreadId` then flaps between them on every update.
                break 'load load_from_provider(
                    session.ensure_thread(ensure(provider_thread.clone())).await,
                )
                .await?;
            }
            let native_id = provider_thread
                .native_thread_ref
                .as_ref()
                .and_then(|reference| reference.native_id.as_ref());
            let uncertain_delivery = projection.context_handoffs.iter().any(|handoff| {
                handoff.to_provider_thread_id == provider_thread.id
                    && handoff.delivery.as_ref().is_some_and(|delivery| {
                        Some(delivery.native_thread_id.as_str()) == native_id.map(|id| id.as_str())
                            && delivery.status
                                == OrchestrationV2ContextHandoffDeliveryStatus::Pending
                    })
            });
            let resumed = if uncertain_delivery {
                Err(
                    ProviderAdapterV2Error::new(ProviderAdapterErrorKind::TurnStart {
                        driver: session.driver().clone(),
                        thread_id: projection.thread.id.clone(),
                        provider_thread_id: provider_thread.id.clone(),
                        run_id: run_id.clone(),
                    })
                    .with_cause(
                        ProviderFailureCause::tagged("Error")
                            .with_message("Uncertain native history injection"),
                    ),
                )
            } else {
                session
                    .resume_thread(ProviderAdapterV2ResumeThreadInput {
                        provider_thread: provider_thread.clone(),
                        thread_id: Some(projection.thread.id.clone()),
                        model_selection: Some(run.model_selection.clone()),
                        runtime_policy: Some(resolved_runtime_policy.clone()),
                    })
                    .await
            };
            let resumed_error = match resumed {
                Ok(thread) => break 'load Some(thread),
                Err(error) => error,
            };
            tracing::warn!(
                driver = %session.driver(),
                provider_thread_id = %provider_thread.id,
                run_id = %run_id,
                reason = if uncertain_delivery { "uncertain_history_delivery" } else { "resume_failed" },
                error_tag = resumed_error.tag(),
                "Provider resume failed; attempting a fresh native session"
            );
            // The native ref is dropped so the adapter binds a fresh native
            // session instead of retrying the resume that just failed, while
            // still adopting this row's identity.
            let mut without_native_ref = provider_thread.clone();
            without_native_ref.native_thread_ref = None;
            let replacement =
                load_from_provider(session.ensure_thread(ensure(without_native_ref)).await).await?;
            let Some(replacement) = replacement else {
                break 'load None;
            };
            let transfer_id = IdAllocator.context_transfer(
                &projection.thread.id,
                &projection.thread.id,
                "provider_resume_fallback",
            );
            let created_at = UtcDateTime::now();
            let history: Vec<OrchestrationV2TurnItem> = self
                .reads
                .turn_start_history(&input.thread_id, None)
                .await?
                .into_iter()
                .filter(|item| {
                    let common = crate::projection::turn_item::common(item);
                    common.run_id.is_none_or(|item_run| {
                        projection.runs.iter().any(|source| {
                            &source.id == item_run && source.ordinal.get() < run.ordinal.get()
                        })
                    })
                })
                .collect();
            let handoff = self
                .handoffs
                .prepare_provider_handoff(&PrepareProviderHandoffInput {
                    thread_id: &projection.thread.id,
                    target_run_id: &run.id,
                    transfer_id: Some(transfer_id.clone()),
                    from_provider_thread_ids: std::slice::from_ref(&provider_thread.id),
                    to_provider_thread_id: &provider_thread.id,
                    from_provider_instance_id: &provider_thread.provider_instance_id,
                    to_provider_instance_id: &run.provider_instance_id,
                    covered_run_ordinals:
                        turin_contracts::orchestration_v2::OrchestrationV2CoveredRunOrdinals {
                            from: turin_contracts::PositiveInt::new(1).expect("positive"),
                            to: turin_contracts::PositiveInt::new(
                                (run.ordinal.get().saturating_sub(1)).max(1),
                            )
                            .expect("positive"),
                        },
                    runs: Some(&projection.runs),
                    strategy: ProviderHandoffStrategy::FullThreadSummary,
                    items: &history,
                    created_at,
                });
            effective_handoffs.insert(0, handoff.clone());
            let handoff_event_id = IdAllocator.event(Some(&projection.thread.id), None, None);
            let transfer_event_id = IdAllocator.event(Some(&projection.thread.id), None, None);
            self.events
                .write(WriteInput::events(vec![
                    OrchestrationV2DomainEvent::ContextHandoffUpdated {
                        id: handoff_event_id,
                        thread_id: projection.thread.id.clone(),
                        run_id: Some(run.id.clone()),
                        node_id: None,
                        driver: None,
                        provider_instance_id: Some(run.provider_instance_id.clone()),
                        raw_event_id: None,
                        occurred_at: created_at,
                        payload: handoff.clone(),
                    },
                    OrchestrationV2DomainEvent::ContextTransferUpdated {
                        id: transfer_event_id,
                        thread_id: projection.thread.id.clone(),
                        run_id: Some(run.id.clone()),
                        node_id: None,
                        driver: None,
                        provider_instance_id: Some(run.provider_instance_id.clone()),
                        raw_event_id: None,
                        occurred_at: created_at,
                        payload: provider_handoff_transfer(
                            transfer_id,
                            &projection.thread.id,
                            &provider_thread.provider_instance_id,
                            run,
                            &handoff,
                            created_at,
                        ),
                    },
                ]))
                .await?;
            Some(replacement)
        };
        // The last attempt already failed the run.
        let Some(loaded_provider_thread) = loaded_provider_thread else {
            return Ok(());
        };
        if !(controls.is_current_attempt_starting)().await? {
            return Ok(());
        }
        let now = UtcDateTime::now();

        // Only started runs reached the provider-thread update below. Queued
        // runs and failures during session setup cannot establish a new
        // telemetry selection.
        let telemetry_view = telemetry_projection(&projection);
        let measured_context = latest_native_context_usage(&telemetry_view, provider_thread);
        let previous_selection: Option<ModelSelection> = measured_context
            .as_ref()
            .map(|measured| measured.model_selection.clone())
            .or_else(|| {
                projection
                    .runs
                    .iter()
                    .rev()
                    .find(|source| {
                        source.ordinal.get() < run.ordinal.get()
                            && source.started_at.is_some()
                            && source.provider_thread_id.as_ref() == Some(&provider_thread.id)
                    })
                    .map(|source| source.model_selection.clone())
            });
        let same_selection = previous_selection.as_ref().is_none_or(|previous| {
            turin_shared::model::model_selections_equal(previous, &run.model_selection)
        });
        let native_id_of = |thread: &OrchestrationV2ProviderThread| {
            thread
                .native_thread_ref
                .as_ref()
                .and_then(|reference| reference.native_id.clone())
        };
        let same_native_thread =
            native_id_of(&loaded_provider_thread) == native_id_of(provider_thread);
        let thread_usage = loaded_provider_thread
            .context_usage
            .clone()
            .or_else(|| provider_thread.context_usage.clone());
        let previous_usage = match (&measured_context, thread_usage) {
            (Some(measured), Some(mut base)) => {
                base.used_tokens = measured.usage.used_tokens;
                if measured.usage.max_tokens.is_some() {
                    base.max_tokens = measured.usage.max_tokens;
                }
                Some(base)
            }
            (Some(measured), None) => Some(measured.usage.clone()),
            (None, usage) => usage,
        };
        let reuse_telemetry = same_selection
            || previous_selection.as_ref().is_some_and(|previous| {
                session.can_reuse_context_usage(previous, &run.model_selection) == Some(true)
            });
        let known_model_window = session
            .get_model_context_window(&run.model_selection, resolved_runtime_policy.cwd.as_deref())
            .flatten();
        // Persist before delivery. Keep this native transcript's measured
        // occupancy. A different model drops compaction telemetry and uses the
        // new window when that window is known.
        let handoff_usage = context_usage_for_handoff(&ContextUsageForHandoffInput {
            same_native_thread,
            same_selection,
            reuse_telemetry,
            previous_usage: previous_usage.as_ref(),
            known_model_window,
        });
        let mut running_provider_thread = loaded_provider_thread;
        running_provider_thread.context_usage = handoff_usage.clone();
        running_provider_thread.id = provider_thread.id.clone();
        running_provider_thread.driver = session.driver().clone();
        running_provider_thread.provider_instance_id = run.provider_instance_id.clone();
        running_provider_thread.provider_session_id = Some(provider_session_id.clone());
        running_provider_thread.app_thread_id = Some(projection.thread.id.clone());
        running_provider_thread.owner_node_id = provider_thread.owner_node_id.clone();
        running_provider_thread.first_run_ordinal =
            provider_thread.first_run_ordinal.or(Some(run.ordinal));
        running_provider_thread.last_run_ordinal = Some(run.ordinal);
        running_provider_thread.handoff_ids = provider_thread.handoff_ids.clone();
        running_provider_thread.forked_from = provider_thread.forked_from.clone();
        running_provider_thread.status = OrchestrationV2ProviderThreadStatus::Active;
        running_provider_thread.created_at = provider_thread.created_at;
        running_provider_thread.updated_at = now;

        let mut running_run = run.clone();
        running_run.status = OrchestrationV2RunStatus::Running;
        running_run.started_at = Some(now);
        let mut running_attempt: OrchestrationV2RunAttempt = attempt.clone();
        if let Some(native_id) = native_id_of(&running_provider_thread) {
            running_attempt.native_thread_id = Some(native_id.as_str().to_owned());
        }
        running_attempt.status =
            turin_contracts::orchestration_v2::OrchestrationV2RunAttemptStatus::Running;
        running_attempt.started_at = Some(now);
        let mut running_root_node = root_node.clone();
        running_root_node.status =
            turin_contracts::orchestration_v2::OrchestrationV2ExecutionNodeStatus::Running;
        running_root_node.started_at = Some(now);

        let thread_id = &projection.thread.id;
        let driver = Some(session.driver().clone());
        let provider_instance_id = Some(run.provider_instance_id.clone());
        let mut events = vec![
            OrchestrationV2DomainEvent::ProviderSessionUpdated {
                id: IdAllocator.event(Some(thread_id), None, Some(&provider_session_id)),
                thread_id: thread_id.clone(),
                run_id: None,
                node_id: None,
                driver: driver.clone(),
                provider_instance_id: provider_instance_id.clone(),
                raw_event_id: None,
                occurred_at: now,
                payload: session.provider_session().clone(),
            },
            OrchestrationV2DomainEvent::ProviderThreadUpdated {
                id: IdAllocator.event(Some(thread_id), None, None),
                thread_id: thread_id.clone(),
                run_id: None,
                node_id: None,
                driver: driver.clone(),
                provider_instance_id: provider_instance_id.clone(),
                raw_event_id: None,
                occurred_at: now,
                payload: running_provider_thread.clone(),
            },
        ];
        if let (Some(transfer), Some(provider_thread_ref)) = (
            native_fork_transfer,
            running_provider_thread.native_thread_ref.clone(),
        ) {
            let mut consumed: OrchestrationV2ContextTransfer = transfer.clone();
            consumed.target_provider_instance_id = Some(run.provider_instance_id.clone());
            consumed.target_run_id = Some(run.id.clone());
            consumed.status = OrchestrationV2ContextTransferStatus::Consumed;
            consumed.resolution = Some(OrchestrationV2ContextTransferResolution::NativeFork {
                provider_thread_ref,
            });
            consumed.error = None;
            consumed.updated_at = now;
            consumed.consumed_at = Some(now);
            events.push(OrchestrationV2DomainEvent::ContextTransferUpdated {
                id: IdAllocator.event(Some(thread_id), None, None),
                thread_id: thread_id.clone(),
                run_id: Some(run.id.clone()),
                node_id: None,
                driver: driver.clone(),
                provider_instance_id: provider_instance_id.clone(),
                raw_event_id: None,
                occurred_at: now,
                payload: consumed,
            });
        }
        macro_rules! run_scoped {
            ($variant:ident, $payload:expr) => {
                OrchestrationV2DomainEvent::$variant {
                    id: IdAllocator.event(Some(thread_id), None, None),
                    thread_id: thread_id.clone(),
                    run_id: Some(run.id.clone()),
                    node_id: Some(root_node.id.clone()),
                    driver: None,
                    provider_instance_id: provider_instance_id.clone(),
                    raw_event_id: None,
                    occurred_at: now,
                    payload: $payload,
                }
            };
        }
        events.push(run_scoped!(RunUpdated, running_run.clone()));
        events.push(run_scoped!(RunAttemptUpdated, running_attempt.clone()));
        events.push(run_scoped!(NodeUpdated, running_root_node.clone()));
        let running_write = self
            .events
            .write_if_run_current(RunCurrentWrite {
                write: WriteInput::events(events),
                thread_id: thread_id.clone(),
                run_id: run.id.clone(),
                active_attempt_id: attempt.id.clone(),
                expected_status: OrchestrationV2RunStatus::Starting,
            })
            .await?;
        if !running_write {
            return Ok(());
        }

        let routable_subagents: Vec<_> = projection
            .subagents
            .iter()
            .filter(|subagent| can_route_related_subagent(subagent.status))
            .collect();
        let user_text =
            turin_shared::composer_context_references::project_composer_context_for_provider(
                &message.text,
                message
                    .context
                    .as_ref()
                    .map(|context| context.records.as_slice())
                    .unwrap_or_default(),
            );
        // Delivered once: this run's provider turn marks the work as told. A
        // restart continuation is prompted by its own text or resumes natively.
        let turn_states: Vec<ProviderTurnState> = projection
            .provider_turns
            .iter()
            .map(ProviderTurnState::from)
            .collect();
        let attempt_refs: Vec<RunAttemptRef> = projection
            .attempts
            .iter()
            .map(|attempt| RunAttemptRef {
                id: attempt.id.clone(),
                run_id: attempt.run_id.clone(),
            })
            .collect();
        let note_continuation =
            is_restart_note_continuation(run, &projection.runs, &turn_states, &attempt_refs);
        let compaction_message_ids: HashSet<_> = projection
            .messages
            .iter()
            .filter(|candidate| {
                candidate.attachments.is_empty()
                    && turin_contracts::base_schemas::js_trim(&candidate.text).to_lowercase()
                        == "/compact"
            })
            .map(|candidate| candidate.id.clone())
            .collect();
        let restart_cancelled_work =
            pending_restart_cancelled_background_work(PendingRestartWork {
                runs: &projection.runs,
                provider_turns: &turn_states,
                compaction_message_ids: &compaction_message_ids,
                run,
                attempts: &attempt_refs,
            });
        let restart_note = if restart_cancelled_work.is_empty() {
            String::new()
        } else {
            restart_cancelled_background_work_note(&restart_cancelled_work)
        };
        let token_cap = handoff_token_cap_config(|name| std::env::var(name).ok())
            .unwrap_or(DEFAULT_HANDOFF_TOKEN_CAP);
        let running_native_id = native_id_of(&running_provider_thread);
        let settled_handoffs: Vec<&OrchestrationV2ContextHandoff> = projection
            .context_handoffs
            .iter()
            .filter(|handoff| {
                handoff.to_provider_thread_id == provider_thread.id
                    && handoff.delivery.as_ref().is_some_and(|delivery| {
                        Some(delivery.native_thread_id.as_str())
                            == running_native_id.as_ref().map(|id| id.as_str())
                            && delivery.status
                                != OrchestrationV2ContextHandoffDeliveryStatus::Pending
                    })
            })
            .collect();
        let delivered_item_ids: HashSet<TurnItemId> = settled_handoffs
            .iter()
            .flat_map(|handoff| {
                handoff
                    .delivery
                    .iter()
                    .flat_map(|delivery| delivery.item_ids.iter().cloned())
            })
            .collect();
        let covered_item_ids: HashSet<TurnItemId> = delivered_item_ids
            .iter()
            .cloned()
            .chain(settled_handoffs.iter().flat_map(|handoff| {
                handoff
                    .delivery
                    .iter()
                    .flat_map(|delivery| delivery.omitted_item_ids.iter().flatten().cloned())
            }))
            .collect();
        let delivered_attempt_ids: HashSet<Option<RunAttemptId>> = projection
            .provider_turns
            .iter()
            .map(|turn| turn.run_attempt_id.clone())
            .collect();
        let accepted_attempts: Vec<&OrchestrationV2RunAttempt> = projection
            .attempts
            .iter()
            .filter(|source| {
                source.provider_thread_id == provider_thread.id
                    && delivered_attempt_ids.contains(&Some(source.id.clone()))
            })
            .collect();
        let native_input_run_ids: HashSet<RunId> = accepted_attempts
            .iter()
            .filter(|source| {
                source.native_thread_id.is_some()
                    && source.native_thread_id.as_deref()
                        == running_native_id.as_ref().map(|id| id.as_str())
            })
            .map(|source| source.run_id.clone())
            .collect();
        let legacy_input_run_ids: HashSet<RunId> = accepted_attempts
            .iter()
            .filter(|source| source.native_thread_id.is_none())
            .map(|source| source.run_id.clone())
            .collect();
        let legacy_recovered_run_ids: HashSet<RunId> = projection
            .runs
            .iter()
            .filter(|source| {
                source.provider_thread_id.as_ref() == Some(&provider_thread.id)
                    && settled_handoffs.iter().any(|handoff| {
                        handoff.strategy == OrchestrationV2ContextHandoffStrategy::FullThreadSummary
                            && handoff
                                .from_provider_thread_ids
                                .contains(&provider_thread.id)
                            && source.ordinal.get() >= handoff.covered_run_ordinals.from.get()
                            && source.ordinal.get() <= handoff.covered_run_ordinals.to.get()
                    })
            })
            .map(|source| source.id.clone())
            .collect();
        let model_context_window = known_model_window.map(|window| window as i64).or_else(|| {
            if handoff_usage.is_some() || reuse_telemetry {
                previous_usage
                    .as_ref()
                    .and_then(|usage| usage.max_tokens)
                    .map(|max| max.get() as i64)
            } else {
                None
            }
        });
        let missed_runs: Vec<_> = projection
            .runs
            .iter()
            .filter(|source| {
                source.ordinal.get() < run.ordinal.get()
                    && source.provider_thread_id.as_ref() == Some(&provider_thread.id)
                    && matches!(
                        source.status,
                        OrchestrationV2RunStatus::Failed | OrchestrationV2RunStatus::Interrupted
                    )
                    && !delivered_attempt_ids.contains(&source.active_attempt_id)
            })
            .collect();
        let missed_run_ids: HashSet<RunId> =
            missed_runs.iter().map(|source| source.id.clone()).collect();
        let missed_items: Vec<OrchestrationV2TurnItem> = if missed_run_ids.is_empty() {
            Vec::new()
        } else {
            let ids: Vec<RunId> = missed_run_ids.iter().cloned().collect();
            self.reads
                .turn_start_history(&input.thread_id, Some(&ids))
                .await?
                .into_iter()
                .filter(|item| {
                    let common = crate::projection::turn_item::common(item);
                    common
                        .run_id
                        .is_some_and(|item_run| missed_run_ids.contains(item_run))
                        && !covered_item_ids.contains(common.id)
                        && turin_provider_core::handoff_budget::historical_message(item).is_some()
                })
                .collect()
        };
        let missed_run_ordinals = match (missed_runs.first(), missed_runs.last()) {
            (Some(first), Some(last)) => Some((first.ordinal.get(), last.ordinal.get())),
            _ => None,
        };
        let session: Arc<dyn ProviderSessionRuntime> = if effective_handoffs.is_empty()
            && missed_items.is_empty()
            && restart_note.is_empty()
            && !note_continuation
        {
            session
        } else {
            Arc::new(DeliverySession::new(
                session.clone(),
                HandoffPlan {
                    reads: self.reads.clone(),
                    events: self.events.clone(),
                    handoff_service: self.handoffs,
                    thread_id: projection.thread.id.clone(),
                    run_id: run.id.clone(),
                    run_provider_instance_id: run.provider_instance_id.clone(),
                    provider_thread_id: provider_thread.id.clone(),
                    driver: session.driver().clone(),
                    runs: projection.runs.clone(),
                    effective_handoffs,
                    missed_items,
                    missed_run_ordinals,
                    running_provider_thread: running_provider_thread.clone(),
                    restart_note,
                    note_continuation,
                    user_text: user_text.clone(),
                    attachments: message.attachments.clone(),
                    token_cap,
                    model_context_window,
                    delivered_item_ids: delivered_item_ids.clone(),
                    estimate: EstimateInput {
                        same_native_thread,
                        run_id: run.id.clone(),
                        provider_thread_id: provider_thread.id.clone(),
                        missed_run_ids,
                        delivered_item_ids,
                        covered_item_ids,
                        native_input_run_ids: native_input_run_ids.clone(),
                        legacy_input_run_ids: legacy_input_run_ids.clone(),
                        legacy_recovered_run_ids,
                    },
                    is_running: controls.is_current_attempt_running.clone(),
                },
            ))
        };

        let native_thread_has_turns = !native_input_run_ids.is_empty()
            || (!legacy_input_run_ids.is_empty()
                && same_native_thread
                && !accepted_attempts
                    .iter()
                    .any(|source| source.native_thread_id.is_some()));
        let provider_turn_ordinal = projection
            .provider_turns
            .iter()
            .filter(|turn| turn.provider_thread_id == provider_thread.id)
            .map(|turn| turn.ordinal.get())
            .max()
            .unwrap_or(0)
            + 1;
        self.run_execution
            .start_root_run(StartRootRunInput {
                command_id: CommandId::new(format!(
                    "command:effect:provider-turn.start:{}",
                    run.id
                ))
                .map_err(|error| Box::new(error) as BoxError)?,
                app_thread: projection.thread.clone(),
                provider_session_id,
                session,
                run: running_run,
                root_node: running_root_node,
                checkpoint_scope: checkpoint_scope.clone(),
                provider_thread: running_provider_thread,
                attempt: running_attempt,
                attempt_id: attempt.id.clone(),
                provider_turn_ordinal: i64::try_from(provider_turn_ordinal).unwrap_or(i64::MAX),
                native_thread_has_turns: Some(native_thread_has_turns),
                load_inherited_background_turn_items: Some(
                    controls.load_inherited_background_turn_items,
                ),
                related_thread_ids: routable_subagents
                    .iter()
                    .filter_map(|subagent| subagent.child_thread_id.clone())
                    .collect(),
                related_provider_thread_ids: routable_subagents
                    .iter()
                    .filter_map(|subagent| subagent.provider_thread_id.clone())
                    .collect(),
                should_start_provider_turn: Some(controls.is_current_attempt_running),
                should_finalize_run: Some(controls.should_finalize_run),
                has_unpaired_run_interrupt_request: Some(
                    controls.has_unpaired_run_interrupt_request,
                ),
                message: ProviderAdapterV2TurnMessage {
                    message_id: message.id.clone(),
                    text: user_text,
                    attachments: message.attachments.clone(),
                    created_by: message.created_by,
                    creation_source: message.creation_source,
                    scheduled_task_id: message.scheduled_task_id.clone(),
                    sender_thread_id: message.sender_thread_id.clone(),
                },
                model_selection: run.model_selection.clone(),
                runtime_policy: resolved_runtime_policy,
            })
            .await
    }
}

/// The callbacks `makeRunControls` builds for `RunExecutionService`.
struct RunControls {
    is_current_attempt_starting: AsyncBool,
    is_current_attempt_running: AsyncBool,
    load_inherited_background_turn_items: InheritedBackgroundLoader,
    should_finalize_run: AsyncBool,
    has_unpaired_run_interrupt_request: AsyncBool,
}

struct RunControlsInput {
    thread_id: ThreadId,
    run_id: RunId,
    attempt_id: RunAttemptId,
    provider_thread_id: turin_contracts::ProviderThreadId,
    run_ordinal: u64,
    inherited_background_turn_items:
        Vec<super::run_execution_service::InheritedBackgroundTurnItemRoute>,
}

/// The handoffs a run delivers: ready ones targeting it, and ready ones for
/// its provider thread whose source run failed, was interrupted, or was an
/// empty-delivery compaction.
fn ready_handoffs(
    projection: &TurnStartContext,
    run: &turin_contracts::orchestration_v2::OrchestrationV2Run,
) -> Vec<OrchestrationV2ContextHandoff> {
    projection
        .context_handoffs
        .iter()
        .filter(|handoff| {
            handoff.status == OrchestrationV2ContextHandoffStatus::Ready
                && (handoff.target_run_id == run.id
                    || (run.provider_thread_id.as_ref() == Some(&handoff.to_provider_thread_id)
                        && projection.runs.iter().any(|source| {
                            source.id == handoff.target_run_id
                                && (matches!(
                                    source.status,
                                    OrchestrationV2RunStatus::Failed
                                        | OrchestrationV2RunStatus::Interrupted
                                ) || (source.status == OrchestrationV2RunStatus::Completed
                                    && handoff.delivery.is_none()
                                    && projection.messages.iter().any(|message| {
                                        message.id == source.user_message_id
                                            && message.attachments.is_empty()
                                            && turin_contracts::base_schemas::js_trim(&message.text)
                                                .to_lowercase()
                                                == "/compact"
                                    })))
                        })))
        })
        .cloned()
        .collect()
}

/// `nativeForkTransfer.sourcePoint.runId`.
fn source_point_run_id(transfer: &OrchestrationV2ContextTransfer) -> Option<&RunId> {
    transfer.source_point.run_id.as_ref()
}

/// The `provider_handoff` transfer a resume fallback records beside its handoff.
fn provider_handoff_transfer(
    id: turin_contracts::ContextTransferId,
    thread_id: &ThreadId,
    source_instance: &ProviderInstanceId,
    run: &turin_contracts::orchestration_v2::OrchestrationV2Run,
    handoff: &OrchestrationV2ContextHandoff,
    created_at: UtcDateTime,
) -> OrchestrationV2ContextTransfer {
    OrchestrationV2ContextTransfer {
        id,
        r#type: OrchestrationV2ContextTransferType::ProviderHandoff,
        source_thread_id: thread_id.clone(),
        target_thread_id: thread_id.clone(),
        source_point: turin_contracts::orchestration_v2::OrchestrationV2ContextSourcePoint {
            thread_id: thread_id.clone(),
            run_id: None,
            checkpoint_id: None,
            turn_item_id: None,
            provider_thread_ref: None,
            provider_turn_ref: None,
        },
        base_point: None,
        source_provider_instance_id: Some(source_instance.clone()),
        target_provider_instance_id: Some(run.provider_instance_id.clone()),
        target_run_id: Some(run.id.clone()),
        status: OrchestrationV2ContextTransferStatus::ResolvedPortable,
        resolution: Some(OrchestrationV2ContextTransferResolution::PortableContext {
            context_handoff_id: handoff.id.clone(),
        }),
        created_by: turin_contracts::orchestration_v2::OrchestrationV2Actor::System,
        error: None,
        created_at,
        updated_at: created_at,
        consumed_at: None,
    }
}

/// `latestNativeContextUsage` reads runs, attempts and provider turns only,
/// so the telemetry view carries just those.
fn telemetry_projection(projection: &TurnStartContext) -> OrchestrationV2ThreadProjection {
    OrchestrationV2ThreadProjection {
        thread: projection.thread.clone(),
        runs: projection.runs.clone(),
        attempts: projection.attempts.clone(),
        nodes: Vec::new(),
        subagents: Vec::new(),
        provider_sessions: Vec::new(),
        provider_threads: Vec::new(),
        provider_turns: projection.provider_turns.clone(),
        runtime_requests: Vec::new(),
        messages: Vec::new(),
        plans: Vec::new(),
        turn_items: Vec::new(),
        checkpoint_scopes: Vec::new(),
        checkpoints: Vec::new(),
        context_handoffs: Vec::new(),
        context_transfers: Vec::new(),
        visible_turn_items: Vec::new(),
        updated_at: projection.thread.updated_at,
    }
}

/// `settleStartFailure`'s message: the nested `cause` of the error (its message
/// when it is an error, itself when it is text), else the error's own message.
fn nested_cause_message(error: &(dyn std::error::Error + 'static)) -> String {
    if let Some(adapter) = error.downcast_ref::<ProviderAdapterV2Error>() {
        return match &adapter.cause {
            Some(ProviderFailureCause::Tagged {
                message: Some(message),
                ..
            }) => message.clone(),
            _ => adapter.to_string(),
        };
    }
    match error.source() {
        Some(source) => source.to_string(),
        None => error.to_string(),
    }
}

#[async_trait]
impl super::orchestrator::services::ProviderTurnStart for ProviderTurnStartService {
    async fn start(&self, input: TurnStartInput) -> Result<(), BoxError> {
        ProviderTurnStartService::start(self, input).await
    }
}

#[cfg(test)]
mod tests;
