//! Projection reads the orchestrator core needs, implemented over T13a's
//! `ProjectionStore`. Upstream reads these through `ProjectionStoreV2` inside
//! `ProviderSessionManager` and `ProviderTurnControlService`; here the
//! session manager depends on the [`ProviderSessionReads`] trait and this
//! module is the production implementation.

use async_trait::async_trait;
use turin_contracts::{
    ProviderSessionId, ThreadId,
    orchestration_v2::{OrchestrationV2PendingBackgroundTask, OrchestrationV2ProviderTurnStatus},
};

use super::provider_session_manager::{
    ProviderSessionReads, ReleasedBackgroundTasks, RunningProviderTurn,
};
use crate::pipeline::orchestrator_port::BoxError;
use crate::projection::store::{
    ProjectionStore,
    canonical::{ProjectionRecordField, ProjectionRecordFilter},
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
