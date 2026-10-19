//! The collaborators `provider-thread.rollback` needs, over the stores and
//! services the composition root already owns. They back
//! [`CheckpointRollbackService`] (upstream `CheckpointRollbackService.ts`),
//! which rewinds the provider conversation, restores the checkpoint's files
//! and records the rolled-back runs.

use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::orchestration_v2::{
    OrchestrationV2Checkpoint, OrchestrationV2CheckpointScope, OrchestrationV2DomainEvent,
    OrchestrationV2ProviderThread, OrchestrationV2ThreadProjection,
};
use turin_contracts::{ProjectId, ThreadId};
use turin_orchestration::core::checkpoint_rollback_service::{
    CheckpointFiles, CheckpointRollbackDeps, CheckpointRollbackService, OpenProviderSession,
    ProviderConversationRollback, ProviderSessionClaim, ProviderSessionOpener,
    RestoreIsolationReads, RollbackEventWriter, RollbackRecords, RuntimePolicyResolver,
    ShellThreadClaim,
};
use turin_orchestration::core::orchestrator::Orchestrator;
use turin_orchestration::core::provider_session_manager::ProviderSessionManager;
use turin_orchestration::core::provider_turn_start_service::{
    ProjectRootsRuntimePolicy, RuntimePolicyResolver as TurnRuntimePolicyResolver,
};
use turin_orchestration::pipeline::event_sink::{EventSink, WriteInput};
use turin_orchestration::pipeline::orchestrator_port::{BoxError, OrchestratorPort as _};
use turin_orchestration::pipeline::thread_command_executor::ThreadCommandExecutor;
use turin_orchestration::projection::project_store::ProjectStore;
use turin_orchestration::projection::store::ProjectionStore;
use turin_orchestration::projection::store::shell::ShellSnapshotOptions;
use turin_orchestration::reactors::checkpoint_service::CheckpointService;
use turin_provider_core::provider_adapter::{
    ProviderAdapterV2OpenSessionInput, ProviderAdapterV2RollbackThreadInput,
    ProviderAdapterV2RuntimePolicy, ProviderSessionRuntime,
};

use super::adapters::StoredProjects;

/// Everything [`build`] reads.
pub struct RollbackSources {
    pub orchestrator: Arc<Orchestrator>,
    pub projections: Arc<ProjectionStore>,
    pub projects: Arc<ProjectStore>,
    pub stored_projects: Arc<StoredProjects>,
    pub checkpoints: Arc<CheckpointService>,
    pub sink: Arc<EventSink>,
    pub sessions: Arc<ProviderSessionManager>,
    pub thread_commands: ThreadCommandExecutor,
}

pub fn build(sources: RollbackSources) -> CheckpointRollbackService {
    let RollbackSources {
        orchestrator,
        projections,
        projects,
        stored_projects,
        checkpoints,
        sink,
        sessions,
        thread_commands,
    } = sources;
    CheckpointRollbackService::new(CheckpointRollbackDeps {
        records: Arc::new(OrchestratorRecords(orchestrator)),
        isolation: Arc::new(ProjectionIsolation {
            projections,
            projects,
        }),
        checkpoints: Arc::new(ServiceCheckpointFiles(checkpoints)),
        events: Arc::new(SinkEvents(sink)),
        sessions: Arc::new(ManagerOpener(sessions)),
        runtime_policy: Arc::new(RollbackPolicy(ProjectRootsRuntimePolicy::new(
            stored_projects,
        ))),
        thread_commands: Arc::new(thread_commands),
    })
}

/// `getThreadRecords`: the whole projection is cheap to read for one thread.
struct OrchestratorRecords(Arc<Orchestrator>);

#[async_trait]
impl RollbackRecords for OrchestratorRecords {
    async fn get_thread_records(
        &self,
        thread_id: &ThreadId,
    ) -> Result<OrchestrationV2ThreadProjection, BoxError> {
        Ok(self.0.get_thread_projection(thread_id).await?)
    }
}

/// The other threads' claims on a checkout, read from the projections
/// (the same reads `ProjectionWorkspaceClaims` makes for the decider).
struct ProjectionIsolation {
    projections: Arc<ProjectionStore>,
    projects: Arc<ProjectStore>,
}

#[async_trait]
impl RestoreIsolationReads for ProjectionIsolation {
    async fn shell_threads(&self) -> Result<Vec<ShellThreadClaim>, BoxError> {
        // The shell holds live and archived threads; deleted threads are not in it.
        let shell = self
            .projections
            .get_shell_snapshot(ShellSnapshotOptions::default())
            .await?;
        Ok(shell
            .threads
            .iter()
            .chain(shell.archived_threads.iter())
            .map(|thread| ShellThreadClaim {
                id: thread.id.clone(),
                project_id: thread.project_id.clone(),
                worktree_path: thread
                    .worktree_path
                    .as_ref()
                    .map(|path| path.as_str().to_owned()),
                deleted_at: None,
            })
            .collect())
    }

    async fn checkpoint_scope_cwds(&self, thread_id: &ThreadId) -> Result<Vec<String>, BoxError> {
        let context = self.projections.get_checkpoint_context(thread_id).await?;
        Ok(context
            .checkpoint_scopes
            .into_iter()
            .filter_map(|scope| scope.cwd)
            .collect())
    }

    async fn provider_session_claims(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Vec<ProviderSessionClaim>, BoxError> {
        let context = self
            .projections
            .get_thread_provider_context(thread_id, None)
            .await?;
        Ok(context
            .provider_sessions
            .iter()
            .map(|session| ProviderSessionClaim {
                status: session.status,
                supports_multiple_provider_threads_per_session: session
                    .capabilities
                    .sessions
                    .supports_multiple_provider_threads_per_session,
                cwd: session.cwd.as_str().to_owned(),
            })
            .collect())
    }

    async fn project_workspace_root(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<String>, BoxError> {
        Ok(self
            .projects
            .get(project_id.clone(), true)
            .await?
            .map(|project| project.workspace_root))
    }
}

struct ServiceCheckpointFiles(Arc<CheckpointService>);

#[async_trait]
impl CheckpointFiles for ServiceCheckpointFiles {
    async fn restore(
        &self,
        scope: &OrchestrationV2CheckpointScope,
        checkpoint: &OrchestrationV2Checkpoint,
    ) -> Result<(), BoxError> {
        Ok(self.0.restore(scope, checkpoint).await?)
    }

    async fn delete_stale_refs(
        &self,
        scope: &OrchestrationV2CheckpointScope,
        checkpoints: &[OrchestrationV2Checkpoint],
    ) -> Result<(), BoxError> {
        Ok(self.0.delete_stale_refs(scope, checkpoints).await?)
    }
}

struct SinkEvents(Arc<EventSink>);

#[async_trait]
impl RollbackEventWriter for SinkEvents {
    async fn write(&self, events: Vec<OrchestrationV2DomainEvent>) -> Result<(), BoxError> {
        self.0.write(WriteInput::events(events)).await?;
        Ok(())
    }
}

struct ManagerOpener(Arc<ProviderSessionManager>);

#[async_trait]
impl ProviderSessionOpener for ManagerOpener {
    async fn open(
        &self,
        input: OpenProviderSession,
    ) -> Result<Arc<dyn ProviderConversationRollback>, BoxError> {
        let session = self
            .0
            .open(ProviderAdapterV2OpenSessionInput {
                thread_id: input.thread_id,
                provider_session_id: input.provider_session_id,
                model_selection: input.model_selection,
                runtime_policy: input.runtime_policy,
                resume_from_session: input.resume_from_session,
                initial_native_thread_id: input.initial_native_thread_id,
                initial_provider_item_identity_version: input
                    .initial_provider_item_identity_version
                    .map(|_| 2),
            })
            .await?;
        Ok(Arc::new(SessionRollback(session)))
    }
}

struct SessionRollback(Arc<dyn ProviderSessionRuntime>);

#[async_trait]
impl ProviderConversationRollback for SessionRollback {
    async fn rollback_thread(
        &self,
        input: ProviderAdapterV2RollbackThreadInput,
    ) -> Result<OrchestrationV2ProviderThread, BoxError> {
        Ok(self.0.rollback_thread(input).await?.provider_thread)
    }
}

/// The turn-start policy (the thread's worktree, else its project root),
/// offered through the rollback service's own resolver trait.
struct RollbackPolicy(ProjectRootsRuntimePolicy);

#[async_trait]
impl RuntimePolicyResolver for RollbackPolicy {
    async fn resolve(
        &self,
        thread: &turin_contracts::orchestration_v2::OrchestrationV2AppThread,
        model_selection: &turin_contracts::model_selection::ModelSelection,
    ) -> Result<ProviderAdapterV2RuntimePolicy, BoxError> {
        TurnRuntimePolicyResolver::resolve(&self.0, thread, model_selection).await
    }
}
