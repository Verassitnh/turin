//! Port of the write side of upstream/apps/server/src/orchestration-v2/
//! ProviderSessionManager.ts (`writeProviderSessionEvents`,
//! `writeReleasedSessionEvents`, `writeReleasedRuntimeRequestEvents`,
//! `writeReleasedBackgroundWorkEvents`). Every write goes through `EventSink`,
//! so the event, projection rows, and effects commit in one transaction.

use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::{
    EventId, ProviderSessionId, ThreadId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2DomainEvent, OrchestrationV2ExecutionNodeStatus,
        OrchestrationV2ProviderSession, OrchestrationV2ResponseCapability,
        OrchestrationV2RuntimeRequestStatus, OrchestrationV2TurnItem,
        OrchestrationV2TurnItemStatus,
    },
};
use turin_provider_core::{id_allocator::IdAllocator, provider_adapter::ProviderAdapterV2Event};

use super::{
    projection_reads::task_id,
    provider_session_manager::{
        ProviderSessionIdentity, ProviderSessionReleaseReason, ProviderSessionSink,
        ReleasedBackgroundTasks, SessionRelease, release_status_for,
    },
};
use crate::{
    pipeline::{
        event_sink::{EventSink, WriteInput},
        orchestrator_port::BoxError,
    },
    projection::store::{
        ProjectionStore,
        canonical::{ProjectionRecordField, ProjectionRecordFilter},
    },
};

const RELEASE_BACKGROUND_WRITE_ATTEMPTS: usize = 3;

/// Accepts runless provider events for the ingestor (`ProviderEventIngestor`).
#[async_trait]
pub trait SessionEventIngest: Send + Sync {
    async fn ingest_normalized(
        &self,
        identity: &ProviderSessionIdentity,
        thread_id: ThreadId,
        event: ProviderAdapterV2Event,
    ) -> Result<(), BoxError>;
}

/// `ProviderSessionSink` over `EventSink` and the projection store.
pub struct EventSessionSink {
    events: Arc<EventSink>,
    projection: Arc<ProjectionStore>,
    ingestor: Arc<dyn SessionEventIngest>,
}

impl EventSessionSink {
    pub fn new(
        events: Arc<EventSink>,
        projection: Arc<ProjectionStore>,
        ingestor: Arc<dyn SessionEventIngest>,
    ) -> Self {
        Self {
            events,
            projection,
            ingestor,
        }
    }

    fn event_id(thread_id: &ThreadId, provider_session_id: &ProviderSessionId) -> EventId {
        IdAllocator.event(Some(thread_id), None, Some(provider_session_id))
    }

    async fn write(&self, events: Vec<OrchestrationV2DomainEvent>) -> Result<(), BoxError> {
        if events.is_empty() {
            return Ok(());
        }
        self.events
            .write(WriteInput::events(events))
            .await
            .map(|_| ())
            .map_err(|error| Box::new(error) as BoxError)
    }

    /// `provider-session.attached` or `provider-session.updated`, one per thread.
    fn session_events(
        identity: &ProviderSessionIdentity,
        thread_ids: &[ThreadId],
        provider_session: &OrchestrationV2ProviderSession,
        attached: bool,
    ) -> Vec<OrchestrationV2DomainEvent> {
        let occurred_at = UtcDateTime::now();
        thread_ids
            .iter()
            .map(|thread_id| {
                let id = Self::event_id(thread_id, &identity.provider_session_id);
                let driver = Some(identity.driver.clone());
                let provider_instance_id = Some(identity.instance_id.clone());
                let payload = provider_session.clone();
                if attached {
                    OrchestrationV2DomainEvent::ProviderSessionAttached {
                        id,
                        thread_id: thread_id.clone(),
                        run_id: None,
                        node_id: None,
                        driver,
                        provider_instance_id,
                        raw_event_id: None,
                        occurred_at,
                        payload,
                    }
                } else {
                    OrchestrationV2DomainEvent::ProviderSessionUpdated {
                        id,
                        thread_id: thread_id.clone(),
                        run_id: None,
                        node_id: None,
                        driver,
                        provider_instance_id,
                        raw_event_id: None,
                        occurred_at,
                        payload,
                    }
                }
            })
            .collect()
    }

    /// `writeReleasedRuntimeRequestEvents` for one thread: settles the
    /// session's live requests so nothing waits on an answer that cannot come.
    async fn settle_runtime_requests(
        &self,
        release: &SessionRelease,
        thread_id: &ThreadId,
    ) -> Result<Vec<OrchestrationV2DomainEvent>, BoxError> {
        let projection = self
            .projection
            .get_thread_records(
                thread_id,
                vec![
                    ProjectionRecordField::RuntimeRequests,
                    ProjectionRecordField::Nodes,
                    ProjectionRecordField::TurnItems,
                ],
                Some(ProjectionRecordFilter {
                    turn_item_types: Some(vec![
                        "approval_request".to_owned(),
                        "user_input_request".to_owned(),
                    ]),
                    ..ProjectionRecordFilter::default()
                }),
            )
            .await
            .map_err(|error| Box::new(error) as BoxError)?;

        let released_at = UtcDateTime::from_datetime(release.released_at);
        let runtime_error = release.reason == ProviderSessionReleaseReason::RuntimeError;
        let (request_status, reason) = match release.reason {
            ProviderSessionReleaseReason::RuntimeError => (
                OrchestrationV2RuntimeRequestStatus::Expired,
                "Provider session failed before this runtime request was resolved.",
            ),
            ProviderSessionReleaseReason::ManualShutdown
            | ProviderSessionReleaseReason::ServerShutdown => (
                OrchestrationV2RuntimeRequestStatus::Cancelled,
                "Provider session was closed before this runtime request was resolved.",
            ),
            ProviderSessionReleaseReason::IdleTimeout => (
                OrchestrationV2RuntimeRequestStatus::Expired,
                "Provider session was closed before this runtime request was resolved.",
            ),
        };
        let now = UtcDateTime::now();
        let session_id = &release.identity.provider_session_id;
        let identity = &release.identity;
        let mut events = Vec::new();

        for request in &projection.runtime_requests {
            let live_here = matches!(
                &request.response_capability,
                OrchestrationV2ResponseCapability::Live { provider_session_id }
                    if provider_session_id == session_id
            );
            if request.status != OrchestrationV2RuntimeRequestStatus::Pending
                || !live_here
                || request.created_at > released_at
            {
                continue;
            }

            let mut settled = request.clone();
            settled.status = request_status;
            settled.response_capability = OrchestrationV2ResponseCapability::NotResumable {
                reason: reason.to_owned(),
            };
            settled.resolved_at = Some(now);
            events.push(OrchestrationV2DomainEvent::RuntimeRequestUpdated {
                id: Self::event_id(thread_id, session_id),
                thread_id: thread_id.clone(),
                run_id: None,
                node_id: Some(request.node_id.clone()),
                driver: Some(identity.driver.clone()),
                provider_instance_id: Some(identity.instance_id.clone()),
                raw_event_id: None,
                occurred_at: now,
                payload: settled,
            });

            if let Some(node) = projection
                .nodes
                .iter()
                .find(|node| node.id == request.node_id)
            {
                let mut node = node.clone();
                node.status = if runtime_error {
                    OrchestrationV2ExecutionNodeStatus::Failed
                } else {
                    OrchestrationV2ExecutionNodeStatus::Cancelled
                };
                node.completed_at = Some(now);
                events.push(OrchestrationV2DomainEvent::NodeUpdated {
                    id: Self::event_id(thread_id, session_id),
                    thread_id: thread_id.clone(),
                    run_id: node.run_id.clone(),
                    node_id: Some(node.id.clone()),
                    driver: Some(identity.driver.clone()),
                    provider_instance_id: Some(identity.instance_id.clone()),
                    raw_event_id: None,
                    occurred_at: now,
                    payload: node,
                });
            }

            let item = projection.turn_items.iter().find(|item| {
                matches!(
                    item,
                    OrchestrationV2TurnItem::ApprovalRequest { request_id, .. }
                        | OrchestrationV2TurnItem::UserInputRequest { request_id, .. }
                        if request_id == &request.id
                )
            });
            if let Some(item) = item {
                let mut item = item.clone();
                let status = if runtime_error {
                    OrchestrationV2TurnItemStatus::Failed
                } else {
                    OrchestrationV2TurnItemStatus::Cancelled
                };
                set_turn_item_status(&mut item, status, now);
                let (run_id, node_id) = turn_item_scope(&item);
                events.push(OrchestrationV2DomainEvent::TurnItemUpdated {
                    id: Self::event_id(thread_id, session_id),
                    thread_id: thread_id.clone(),
                    run_id,
                    node_id,
                    driver: Some(identity.driver.clone()),
                    provider_instance_id: Some(identity.instance_id.clone()),
                    raw_event_id: None,
                    occurred_at: now,
                    payload: item,
                });
            }
        }
        Ok(events)
    }

    /// `writeReleasedBackgroundWorkEvents` for one provider thread. The write
    /// commits only if the stored row still equals what was read; a provider
    /// update that lands in between is retried a few times.
    async fn clear_background_tasks(
        &self,
        identity: &ProviderSessionIdentity,
        released: &ReleasedBackgroundTasks,
    ) -> Result<(), BoxError> {
        for _ in 0..RELEASE_BACKGROUND_WRITE_ATTEMPTS {
            let projection = self
                .projection
                .get_thread_records(
                    &released.thread_id,
                    vec![ProjectionRecordField::ProviderThreads],
                    None,
                )
                .await
                .map_err(|error| Box::new(error) as BoxError)?;
            let Some(provider_thread) = projection
                .provider_threads
                .iter()
                .find(|thread| thread.id == released.provider_thread_id)
            else {
                return Ok(());
            };
            let tasks = &provider_thread.pending_background_tasks;
            let remaining: Vec<_> = tasks
                .iter()
                .filter(|task| !released.task_ids.iter().any(|id| id == task_id(task)))
                .cloned()
                .collect();
            if remaining.len() == tasks.len() {
                return Ok(());
            }
            let now = UtcDateTime::now();
            let mut updated = provider_thread.clone();
            updated.pending_background_tasks = remaining;
            updated.updated_at = now;
            let event = OrchestrationV2DomainEvent::ProviderThreadUpdated {
                id: Self::event_id(&released.thread_id, &identity.provider_session_id),
                thread_id: released.thread_id.clone(),
                run_id: None,
                node_id: None,
                driver: Some(identity.driver.clone()),
                provider_instance_id: Some(identity.instance_id.clone()),
                raw_event_id: None,
                occurred_at: now,
                payload: updated,
            };
            let result = self
                .events
                .write_if_provider_thread_unchanged(provider_thread.clone(), vec![event])
                .await
                .map_err(|error| Box::new(error) as BoxError)?;
            if result.committed {
                return Ok(());
            }
        }
        Ok(())
    }
}

/// Sets the status and the time fields every turn item variant carries.
fn set_turn_item_status(
    item: &mut OrchestrationV2TurnItem,
    status: OrchestrationV2TurnItemStatus,
    now: UtcDateTime,
) {
    match item {
        OrchestrationV2TurnItem::ApprovalRequest {
            status: current,
            completed_at,
            updated_at,
            ..
        }
        | OrchestrationV2TurnItem::UserInputRequest {
            status: current,
            completed_at,
            updated_at,
            ..
        } => {
            *current = status;
            *completed_at = Some(now);
            *updated_at = now;
        }
        _ => {}
    }
}

/// The run and node a turn item belongs to.
fn turn_item_scope(
    item: &OrchestrationV2TurnItem,
) -> (
    Option<turin_contracts::RunId>,
    Option<turin_contracts::NodeId>,
) {
    match item {
        OrchestrationV2TurnItem::ApprovalRequest {
            run_id, node_id, ..
        }
        | OrchestrationV2TurnItem::UserInputRequest {
            run_id, node_id, ..
        } => (run_id.clone(), node_id.clone()),
        _ => (None, None),
    }
}

#[async_trait]
impl ProviderSessionSink for EventSessionSink {
    async fn session_attached(
        &self,
        identity: &ProviderSessionIdentity,
        thread_ids: &[ThreadId],
        provider_session: &OrchestrationV2ProviderSession,
    ) -> Result<(), BoxError> {
        self.write(Self::session_events(
            identity,
            thread_ids,
            provider_session,
            true,
        ))
        .await
    }

    async fn session_updated(
        &self,
        identity: &ProviderSessionIdentity,
        thread_ids: &[ThreadId],
        provider_session: &OrchestrationV2ProviderSession,
    ) -> Result<(), BoxError> {
        self.write(Self::session_events(
            identity,
            thread_ids,
            provider_session,
            false,
        ))
        .await
    }

    /// `writeReleaseRecords`. Each step runs even if an earlier one fails;
    /// the first failure is returned.
    async fn session_released(&self, release: &SessionRelease) -> Result<(), BoxError> {
        let mut first_error: Option<BoxError> = None;
        let mut keep_first = |result: Result<(), BoxError>| {
            if let Err(error) = result
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        };

        let mut status_session = release.last_session.clone();
        status_session.status = release_status_for(release.reason);
        status_session.updated_at = UtcDateTime::now();
        status_session.last_error = match release.reason {
            ProviderSessionReleaseReason::RuntimeError => Some(
                release
                    .detail
                    .clone()
                    .unwrap_or_else(|| "Provider runtime failed.".to_owned()),
            ),
            _ => None,
        };
        keep_first(
            self.session_updated(&release.identity, &release.thread_ids, &status_session)
                .await,
        );

        for thread_id in &release.thread_ids {
            match self.settle_runtime_requests(release, thread_id).await {
                Ok(events) => keep_first(self.write(events).await),
                Err(error) => keep_first(Err(error)),
            }
        }

        for released in &release.released_background_tasks {
            keep_first(
                self.clear_background_tasks(&release.identity, released)
                    .await,
            );
        }

        first_error.map_or(Ok(()), Err)
    }

    async fn ingest_session_scoped_event(
        &self,
        identity: &ProviderSessionIdentity,
        thread_id: ThreadId,
        event: ProviderAdapterV2Event,
    ) -> Result<(), BoxError> {
        self.ingestor
            .ingest_normalized(identity, thread_id, event)
            .await
    }
}
