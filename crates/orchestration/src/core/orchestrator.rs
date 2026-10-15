//! Port of upstream/apps/server/src/orchestration-v2/Orchestrator.ts
//! (`OrchestratorV2`): the decider behind [`OrchestratorPort`].
//!
//! This file holds the port's read half and the constructor. The command
//! path (receipts, the per-thread lock, commit, rejection) is in
//! [`orchestrator::dispatch`](self::dispatch), and each command group has its
//! own file under `core/orchestrator/`, so parallel ports merge cleanly.
//!
//! Not ported yet: `resumeQueuedRuns`, `searchThread(Stream)`, timeline and
//! history paging, checkpoint context, and the thread-snapshot reads. Those
//! return an error here. Command kinds not yet ported are rejected in
//! `dispatch::decide`, which reports `OrchestratorError::CommandRejected`.

pub mod dispatch;
mod provider_detach;
pub mod services;
#[cfg(test)]
mod tests;
mod thread_archive;
mod thread_lifecycle;
mod thread_mutation;

use std::sync::Arc;

use async_trait::async_trait;
use futures::{StreamExt, future, stream::BoxStream};
use turin_contracts::{
    RunId, ThreadId,
    orchestration_v2::{
        OrchestrationV2DomainEvent, OrchestrationV2Run, OrchestrationV2ServerCommand,
        OrchestrationV2ThreadProjection, OrchestrationV2ThreadShell,
        OrchestrationV2ThreadShellSnapshot,
    },
};

use crate::pipeline::command_receipt_store::CommandReceiptStore;
use crate::pipeline::event_sink::{EventSink, SinkStream, StreamOptions};
use crate::pipeline::orchestrator_port::{
    BoxError, DispatchResult, OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
    ShellLocation, ShellSnapshotOptions, ThreadRecords,
};
use crate::pipeline::thread_command_executor::ThreadCommandExecutor;
use crate::projection::store::{
    ProjectionStore,
    canonical::{ProjectionRecordField, ProjectionRecordFilter},
    shell::{ShellSnapshotLocation, ShellSnapshotOptions as ProjectionShellSnapshotOptions},
};
use services::RestartRecovery;
use turin_provider_core::id_allocator::IdAllocator;

/// Thread id upstream attaches to shell-snapshot errors (`shellProjectionError`).
fn shell_error_thread() -> ThreadId {
    // A literal that passes `ThreadId`'s non-empty check.
    ThreadId::new("thread:shell").expect("literal thread id is non-empty")
}

pub struct Orchestrator {
    projection: Arc<ProjectionStore>,
    sink: EventSink,
    receipts: CommandReceiptStore,
    locks: ThreadCommandExecutor,
    ids: IdAllocator,
    recovery: Arc<dyn RestartRecovery>,
}

impl Orchestrator {
    pub fn new(
        projection: Arc<ProjectionStore>,
        sink: EventSink,
        receipts: CommandReceiptStore,
        recovery: Arc<dyn RestartRecovery>,
    ) -> Self {
        Self {
            projection,
            sink,
            receipts,
            locks: ThreadCommandExecutor::new(),
            ids: IdAllocator,
            recovery,
        }
    }

    fn projection_error(thread_id: &ThreadId, cause: impl Into<BoxError>) -> OrchestratorError {
        OrchestratorError::projection(thread_id.clone(), cause)
    }

    fn stream_error(cause: crate::pipeline::event_sink::SinkError) -> OrchestratorError {
        OrchestratorError::DomainEventStream {
            cause: Box::new(cause),
        }
    }
}

fn projection_record_field(field: RecordField) -> ProjectionRecordField {
    match field {
        RecordField::Runs => ProjectionRecordField::Runs,
        RecordField::Attempts => ProjectionRecordField::Attempts,
        RecordField::Nodes => ProjectionRecordField::Nodes,
        RecordField::Subagents => ProjectionRecordField::Subagents,
        RecordField::ProviderSessions => ProjectionRecordField::ProviderSessions,
        RecordField::ProviderThreads => ProjectionRecordField::ProviderThreads,
        RecordField::ProviderTurns => ProjectionRecordField::ProviderTurns,
        RecordField::RuntimeRequests => ProjectionRecordField::RuntimeRequests,
        RecordField::Messages => ProjectionRecordField::Messages,
        RecordField::TurnItems => ProjectionRecordField::TurnItems,
    }
}

fn projection_record_filter(filter: &RecordFilter) -> ProjectionRecordFilter {
    ProjectionRecordFilter {
        message_roles: filter.message_roles.clone(),
        message_ids: filter
            .message_ids
            .as_ref()
            .map(|ids| ids.iter().map(|id| id.as_str().to_owned()).collect()),
        run_ids: filter.run_ids.clone(),
        turn_item_types: filter.turn_item_types.clone(),
        omit_settled_leaf_nodes: filter.omit_settled_leaf_nodes,
        ..ProjectionRecordFilter::default()
    }
}

fn projection_shell_options(options: &ShellSnapshotOptions) -> ProjectionShellSnapshotOptions {
    ProjectionShellSnapshotOptions {
        location: Some(match options.location {
            ShellLocation::Active => ShellSnapshotLocation::Active,
            ShellLocation::Archive => ShellSnapshotLocation::Archive,
        }),
        project_id: options.project_id.clone(),
        unsettled_only: options.unsettled_only,
    }
}

#[async_trait]
impl OrchestratorPort for Orchestrator {
    async fn dispatch(
        &self,
        command: OrchestrationV2ServerCommand,
    ) -> Result<DispatchResult, OrchestratorError> {
        dispatch::dispatch_with_receipt(self, command).await
    }

    async fn get_thread_records(
        &self,
        thread_id: &ThreadId,
        fields: &[RecordField],
        filter: &RecordFilter,
    ) -> Result<ThreadRecords, OrchestratorError> {
        let fields = fields
            .iter()
            .copied()
            .map(projection_record_field)
            .collect();
        self.projection
            .get_thread_records(thread_id, fields, Some(projection_record_filter(filter)))
            .await
            .map_err(|cause| Self::projection_error(thread_id, cause))
    }

    async fn get_thread_projection(
        &self,
        thread_id: &ThreadId,
    ) -> Result<OrchestrationV2ThreadProjection, OrchestratorError> {
        self.projection
            .get_thread_projection(thread_id)
            .await
            .map_err(|cause| Self::projection_error(thread_id, cause))
    }

    async fn get_thread_shell(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Option<OrchestrationV2ThreadShell>, OrchestratorError> {
        self.projection
            .get_thread_shell(thread_id)
            .await
            .map_err(|cause| Self::projection_error(thread_id, cause))
    }

    async fn get_shell_snapshot(
        &self,
        options: &ShellSnapshotOptions,
    ) -> Result<OrchestrationV2ThreadShellSnapshot, OrchestratorError> {
        self.projection
            .get_shell_snapshot(projection_shell_options(options))
            .await
            .map_err(|cause| Self::projection_error(&shell_error_thread(), cause))
    }

    async fn get_thread_event_sequence(
        &self,
        thread_id: &ThreadId,
    ) -> Result<i64, OrchestratorError> {
        self.sink
            .latest_sequence(Some(thread_id.clone()))
            .await
            .map_err(|cause| Self::projection_error(thread_id, cause))
    }

    async fn restart_continuation_pending(&self, run: &OrchestrationV2Run) -> bool {
        self.recovery.restart_continuation_pending(run).await
    }

    async fn await_restart_continuation(&self, run: &OrchestrationV2Run) {
        self.recovery.await_restart_continuation(run).await;
    }

    async fn recover_delegated_task(&self, thread_id: &ThreadId, source_run_id: &RunId) {
        self.recovery
            .recover_delegated_task(thread_id, source_run_id)
            .await;
    }

    async fn stream_stored_events_from(
        &self,
        options: StreamOptions,
    ) -> Result<SinkStream, OrchestratorError> {
        self.sink.stream(options).await.map_err(Self::stream_error)
    }

    /// Live tail only: the stream starts after the log's current high-water
    /// mark. Replaying the whole store first would make every domain-event
    /// subscriber redo its work after each boot.
    async fn stream_domain_events(
        &self,
    ) -> Result<BoxStream<'static, OrchestrationV2DomainEvent>, OrchestratorError> {
        let latest = self
            .sink
            .latest_sequence(None)
            .await
            .map_err(Self::stream_error)?;
        let stored = self
            .sink
            .stream(StreamOptions {
                after_sequence: Some(latest),
                ..StreamOptions::default()
            })
            .await
            .map_err(Self::stream_error)?;
        // The port carries no per-item error, so a failed read ends the stream
        // after logging. Subscribers treat the end of the stream as shutdown.
        let events = stored
            .take_while(|item| {
                if let Err(error) = item {
                    tracing::warn!(error = ?error, "domain event stream failed; ending live tail");
                }
                future::ready(item.is_ok())
            })
            .filter_map(|item| future::ready(item.ok().map(|stored| stored.event)));
        Ok(events.boxed())
    }
}
