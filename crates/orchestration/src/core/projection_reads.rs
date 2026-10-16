//! Projection reads the orchestrator core needs, implemented over T13a's
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
        OrchestrationV2RuntimeRequestKind, OrchestrationV2RuntimeRequestStatus,
        OrchestrationV2TurnItem,
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

    async fn node_stored(&self, node: &OrchestrationV2ExecutionNode) -> Result<bool, BoxError> {
        let projection = self
            .get_thread_records(&node.thread_id, vec![ProjectionRecordField::Nodes], None)
            .await
            .map_err(|error| Box::new(error) as BoxError)?;
        Ok(projection.nodes.iter().any(|stored| stored == node))
    }

    async fn pending_native_user_inputs(
        &self,
        thread_id: &ThreadId,
        provider_turn_id: &ProviderTurnId,
    ) -> Result<PendingNativeUserInputs, BoxError> {
        let projection = self
            .get_thread_records(
                thread_id,
                vec![
                    ProjectionRecordField::RuntimeRequests,
                    ProjectionRecordField::Nodes,
                    ProjectionRecordField::TurnItems,
                ],
                None,
            )
            .await
            .map_err(|error| Box::new(error) as BoxError)?;
        let runtime_requests: Vec<_> = projection
            .runtime_requests
            .iter()
            .filter(|request| {
                request.provider_turn_id.as_ref() == Some(provider_turn_id)
                    && request.kind == OrchestrationV2RuntimeRequestKind::UserInput
                    && request.status == OrchestrationV2RuntimeRequestStatus::Pending
            })
            .cloned()
            .collect();
        let nodes = projection
            .nodes
            .iter()
            .filter(|node| {
                runtime_requests
                    .iter()
                    .any(|request| request.node_id == node.id)
            })
            .cloned()
            .collect();
        let turn_items = projection
            .turn_items
            .iter()
            .filter(|item| match item {
                OrchestrationV2TurnItem::UserInputRequest { request_id, .. } => runtime_requests
                    .iter()
                    .any(|request| &request.id == request_id),
                _ => false,
            })
            .cloned()
            .collect();
        Ok(PendingNativeUserInputs {
            runtime_requests,
            nodes,
            turn_items,
        })
    }

    async fn plan(
        &self,
        thread_id: &ThreadId,
        plan_id: &PlanId,
    ) -> Result<Option<OrchestrationV2PlanArtifact>, BoxError> {
        let projection = self
            .get_thread_records(thread_id, vec![ProjectionRecordField::Plans], None)
            .await
            .map_err(|error| Box::new(error) as BoxError)?;
        Ok(projection.plans.into_iter().find(|plan| {
            serde_json::to_value(plan)
                .ok()
                .and_then(|value| {
                    value
                        .get("id")
                        .and_then(|id| id.as_str().map(str::to_owned))
                })
                .as_deref()
                == Some(plan_id.as_str())
        }))
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

/// The provider-control read (`getProviderControlContext`). Reads the whole
/// record kinds through `get_thread_records`, narrowed by the target's ids. A
/// targeted SQL read is a perf follow-up.
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
        let filter = ProjectionRecordFilter {
            message_ids: target
                .message_id
                .as_ref()
                .map(|id| vec![id.as_str().to_owned()]),
            ..ProjectionRecordFilter::default()
        };
        let projection = self
            .get_thread_records(
                thread_id,
                vec![
                    ProjectionRecordField::ProviderThreads,
                    ProjectionRecordField::ProviderTurns,
                    ProjectionRecordField::Attempts,
                    ProjectionRecordField::Messages,
                    ProjectionRecordField::Runs,
                ],
                Some(filter),
            )
            .await
            .map_err(|error| Box::new(error) as BoxError)?;
        let provider_thread = projection
            .provider_threads
            .iter()
            .find(|thread| thread.id == target.provider_thread_id)
            .cloned();
        let provider_turn = projection
            .provider_turns
            .iter()
            .find(|turn| turn.id == target.provider_turn_id)
            .cloned();
        let attempt = target.attempt_id.as_ref().and_then(|id| {
            projection
                .attempts
                .iter()
                .find(|attempt| &attempt.id == id)
                .cloned()
        });
        let message = target.message_id.as_ref().and_then(|id| {
            projection
                .messages
                .iter()
                .find(|message| &message.id == id)
                .cloned()
        });
        let run = match (&message, &provider_turn) {
            (Some(message), Some(_)) => message.run_id.as_ref().and_then(|run_id| {
                projection
                    .runs
                    .iter()
                    .find(|run| &run.id == run_id)
                    .cloned()
            }),
            _ => None,
        };
        Ok(ProviderControlContext {
            provider_thread,
            provider_turn,
            attempt,
            message,
            run,
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
