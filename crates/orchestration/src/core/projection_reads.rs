//! Turin seam, no upstream counterpart (upstream reads `ProjectionStoreV2`
//! directly). Projection reads the orchestrator core needs, implemented over T13a's
//! `ProjectionStore`. Upstream reads these through `ProjectionStoreV2` inside
//! `ProviderSessionManager` and `ProviderTurnControlService`; here the
//! session manager depends on the [`ProviderSessionReads`] trait and this
//! module is the production implementation.

use async_trait::async_trait;
use turin_contracts::{
    MessageId, PlanId, ProviderSessionId, ProviderThreadId, ProviderTurnId, RunAttemptId, RunId,
    ThreadId,
    orchestration_v2::{
        OrchestrationV2AppThread, OrchestrationV2ConversationMessage,
        OrchestrationV2DelegatedCompletionCohort, OrchestrationV2ExecutionNode,
        OrchestrationV2PendingBackgroundTask, OrchestrationV2PlanArtifact,
        OrchestrationV2ProviderThread, OrchestrationV2ProviderTurn,
        OrchestrationV2ProviderTurnStatus, OrchestrationV2Run, OrchestrationV2RunAttempt,
    },
};

use super::provider_event_ingestor::{IngestReads, PendingNativeUserInputs};
use super::provider_session_manager::{
    ProviderSessionReads, ReleasedBackgroundTasks, RunningProviderTurn,
};
use crate::pipeline::orchestrator_port::BoxError;
use crate::projection::{
    error::ProjectionStoreError,
    store::{
        ProjectionStore,
        canonical::{ProjectionRecordField, ProjectionRecordFilter},
    },
};

#[async_trait]
impl ProviderSessionReads for ProjectionStore {
    async fn running_turns(
        &self,
        thread_id: &ThreadId,
        provider_session_id: &ProviderSessionId,
    ) -> Result<Vec<RunningProviderTurn>, BoxError> {
        let projection = self
            .get_thread_records(
                thread_id,
                vec![
                    ProjectionRecordField::ProviderThreads,
                    ProjectionRecordField::ProviderTurns,
                ],
                None,
            )
            .await
            .map_err(|error| Box::new(error) as BoxError)?;
        let session_threads: Vec<_> = projection
            .provider_threads
            .iter()
            .filter(|thread| thread.provider_session_id.as_ref() == Some(provider_session_id))
            .collect();
        let turns = projection
            .provider_turns
            .iter()
            .filter(|turn| turn.status == OrchestrationV2ProviderTurnStatus::Running)
            .filter_map(|turn| {
                session_threads
                    .iter()
                    .find(|thread| thread.id == turn.provider_thread_id)
                    .map(|thread| RunningProviderTurn {
                        provider_thread: (*thread).clone(),
                        provider_turn_id: turn.id.clone(),
                    })
            })
            .collect();
        Ok(turns)
    }

    async fn pending_background_tasks(
        &self,
        thread_ids: &[ThreadId],
        provider_session_id: &ProviderSessionId,
    ) -> Result<Vec<ReleasedBackgroundTasks>, BoxError> {
        let mut released = Vec::new();
        for thread_id in thread_ids {
            let projection = self
                .get_thread_records(
                    thread_id,
                    vec![ProjectionRecordField::ProviderThreads],
                    None::<ProjectionRecordFilter>,
                )
                .await
                .map_err(|error| Box::new(error) as BoxError)?;
            for provider_thread in &projection.provider_threads {
                if provider_thread.provider_session_id.as_ref() != Some(provider_session_id) {
                    continue;
                }
                let task_ids: Vec<String> = provider_thread
                    .pending_background_tasks
                    .iter()
                    .map(|task| task_id(task).to_owned())
                    .collect();
                if task_ids.is_empty() {
                    continue;
                }
                released.push(ReleasedBackgroundTasks {
                    thread_id: thread_id.clone(),
                    provider_thread_id: provider_thread.id.clone(),
                    task_ids,
                });
            }
        }
        Ok(released)
    }
}

/// The task id of any roster entry, whatever its kind.
pub fn task_id(task: &OrchestrationV2PendingBackgroundTask) -> &str {
    match task {
        OrchestrationV2PendingBackgroundTask::Subagent { task_id, .. }
        | OrchestrationV2PendingBackgroundTask::Command { task_id, .. }
        | OrchestrationV2PendingBackgroundTask::Monitor { task_id, .. }
        | OrchestrationV2PendingBackgroundTask::BackgroundTask { task_id, .. } => task_id.as_str(),
    }
}

#[async_trait]
impl IngestReads for ProjectionStore {
    async fn app_thread(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Option<OrchestrationV2AppThread>, BoxError> {
        match self.get_thread(thread_id).await {
            Ok(thread) => Ok(Some(thread)),
            Err(ProjectionStoreError::ThreadNotFound { .. }) => Ok(None),
            Err(error) => Err(Box::new(error)),
        }
    }

    async fn rewind_records(
        &self,
        thread_id: &ThreadId,
    ) -> Result<super::provider_event_ingestor::RewindRecords, BoxError> {
        let projection = self
            .get_thread_records(
                thread_id,
                vec![
                    ProjectionRecordField::Runs,
                    ProjectionRecordField::Attempts,
                    ProjectionRecordField::ProviderTurns,
                    ProjectionRecordField::Nodes,
                    ProjectionRecordField::Checkpoints,
                ],
                None,
            )
            .await
            .map_err(|error| Box::new(error) as BoxError)?;
        Ok(super::provider_event_ingestor::RewindRecords {
            runs: projection.runs,
            attempts: projection.attempts,
            provider_turns: projection.provider_turns,
            nodes: projection.nodes,
            checkpoints: projection.checkpoints,
        })
    }

    async fn node_stored(&self, node: &OrchestrationV2ExecutionNode) -> Result<bool, BoxError> {
        self.is_node_stored(node)
            .await
            .map_err(|error| Box::new(error) as BoxError)
    }

    async fn pending_native_user_inputs(
        &self,
        thread_id: &ThreadId,
        provider_turn_id: &ProviderTurnId,
    ) -> Result<PendingNativeUserInputs, BoxError> {
        // Native requests only: message-answerable ones stay pending.
        let pending = self
            .get_pending_native_user_inputs(thread_id, provider_turn_id)
            .await
            .map_err(|error| Box::new(error) as BoxError)?;
        Ok(PendingNativeUserInputs {
            runtime_requests: pending.runtime_requests,
            nodes: pending.nodes,
            turn_items: pending.turn_items,
        })
    }

    async fn plan(
        &self,
        thread_id: &ThreadId,
        plan_id: &PlanId,
    ) -> Result<Option<OrchestrationV2PlanArtifact>, BoxError> {
        self.get_plan(thread_id, plan_id.as_str())
            .await
            .map_err(|error| Box::new(error) as BoxError)
    }
}

/// What a provider-turn control command targets: the provider thread and turn,
/// and optionally the attempt and the steering message.
#[derive(Debug, Clone)]
pub struct ProviderControlTarget {
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: ProviderTurnId,
    pub attempt_id: Option<RunAttemptId>,
    pub message_id: Option<MessageId>,
}

/// The records a control command reads. `run` is the message's run, and only
/// when the target names a message and the turn exists (upstream's rule).
#[derive(Debug, Clone, Default)]
pub struct ProviderControlContext {
    pub provider_thread: Option<OrchestrationV2ProviderThread>,
    pub provider_turn: Option<OrchestrationV2ProviderTurn>,
    pub attempt: Option<OrchestrationV2RunAttempt>,
    pub message: Option<OrchestrationV2ConversationMessage>,
    pub run: Option<OrchestrationV2Run>,
}

/// The provider-control read (`getProviderControlContext`), over the store's
/// targeted query.
#[async_trait]
pub trait ProviderControlReads: Send + Sync {
    async fn provider_control_context(
        &self,
        thread_id: &ThreadId,
        target: &ProviderControlTarget,
    ) -> Result<ProviderControlContext, BoxError>;

    /// The delegated-completion cohort of a run, if it has one (steer ownership).
    async fn run_delegated_completion(
        &self,
        thread_id: &ThreadId,
        run_id: &RunId,
    ) -> Result<Option<OrchestrationV2DelegatedCompletionCohort>, BoxError>;
}

#[async_trait]
impl ProviderControlReads for ProjectionStore {
    async fn provider_control_context(
        &self,
        thread_id: &ThreadId,
        target: &ProviderControlTarget,
    ) -> Result<ProviderControlContext, BoxError> {
        let context = self
            .get_provider_control_context(
                thread_id,
                crate::projection::store::contexts::ProviderControlTarget {
                    provider_thread_id: target.provider_thread_id.clone(),
                    provider_turn_id: target.provider_turn_id.clone(),
                    attempt_id: target.attempt_id.clone(),
                    message_id: target.message_id.clone(),
                },
            )
            .await
            .map_err(|error| Box::new(error) as BoxError)?;
        Ok(ProviderControlContext {
            provider_thread: context.provider_thread,
            provider_turn: context.provider_turn,
            attempt: context.attempt,
            message: context.message,
            run: context.run,
        })
    }

    async fn run_delegated_completion(
        &self,
        thread_id: &ThreadId,
        run_id: &RunId,
    ) -> Result<Option<OrchestrationV2DelegatedCompletionCohort>, BoxError> {
        let filter = ProjectionRecordFilter {
            run_ids: Some(vec![run_id.clone()]),
            ..ProjectionRecordFilter::default()
        };
        let projection = self
            .get_thread_records(thread_id, vec![ProjectionRecordField::Runs], Some(filter))
            .await
            .map_err(|error| Box::new(error) as BoxError)?;
        Ok(projection
            .runs
            .into_iter()
            .find(|run| &run.id == run_id)
            .and_then(|run| run.delegated_completion))
    }
}
