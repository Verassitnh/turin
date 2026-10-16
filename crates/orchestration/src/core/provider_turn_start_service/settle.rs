//! Port of upstream/apps/server/src/orchestration-v2/ProviderTurnStartService.ts (lines 283-372).
//!
//! `settleRunBeforeStart`: settles a run that never reached the provider with one signal turn item
//! and terminal run, attempt and root node events, written only while the run
//! is still the current starting attempt.

use turin_contracts::{
    ProviderThreadId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2DomainEvent, OrchestrationV2ExecutionNode,
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2ProviderFailure,
        OrchestrationV2ProviderThread, OrchestrationV2Run, OrchestrationV2RunAttempt,
        OrchestrationV2RunAttemptStatus, OrchestrationV2RunStatus, OrchestrationV2TurnItem,
        OrchestrationV2TurnItemStatus,
    },
    provider_instance::ProviderInstanceId,
};
use turin_provider_core::id_allocator::{IdAllocator, derive};

use crate::{
    pipeline::{
        event_sink::{RunCurrentWrite, WriteInput},
        orchestrator_port::BoxError,
    },
    projection::store::contexts::TurnStartContext,
};

use super::ProviderTurnStartService;

/// The terminal state of a settled run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SettledStatus {
    Completed,
    Failed,
}

/// The item the settle writes: an error, or a local command's result.
pub(super) enum SettleItem {
    Error {
        title: String,
        failure: OrchestrationV2ProviderFailure,
    },
    Command {
        title: String,
        input: String,
        output: String,
        exit_code: i64,
    },
}

pub(super) struct SettleInput {
    pub signal: &'static str,
    pub status: SettledStatus,
    pub now: UtcDateTime,
    /// Omitted when the run never started, so `startedAt` stays as projected.
    pub started_at: Option<UtcDateTime>,
    pub provider_instance_id: ProviderInstanceId,
    pub item_provider_thread_id: ProviderThreadId,
    pub item: SettleItem,
    /// Emitted after the run events when the provider thread should go idle.
    pub provider_thread_update: Option<OrchestrationV2ProviderThread>,
}

/// The run's rows the start flow settles or advances.
pub(super) struct RunRows<'a> {
    pub projection: &'a TurnStartContext,
    pub run: &'a OrchestrationV2Run,
    pub attempt: &'a OrchestrationV2RunAttempt,
    pub root_node: &'a OrchestrationV2ExecutionNode,
}

impl ProviderTurnStartService {
    pub(super) async fn settle_run_before_start(
        &self,
        rows: &RunRows<'_>,
        input: SettleInput,
    ) -> Result<(), BoxError> {
        let RunRows {
            projection,
            run,
            attempt,
            root_node,
        } = rows;
        let now = input.now;
        let thread_id = &projection.thread.id;
        let (item_status, run_status, attempt_status, node_status) = match input.status {
            SettledStatus::Completed => (
                OrchestrationV2TurnItemStatus::Completed,
                OrchestrationV2RunStatus::Completed,
                OrchestrationV2RunAttemptStatus::Completed,
                OrchestrationV2ExecutionNodeStatus::Completed,
            ),
            SettledStatus::Failed => (
                OrchestrationV2TurnItemStatus::Failed,
                OrchestrationV2RunStatus::Failed,
                OrchestrationV2RunAttemptStatus::Failed,
                OrchestrationV2ExecutionNodeStatus::Failed,
            ),
        };
        let ordinal = projection
            .turn_items
            .iter()
            .map(|item| crate::projection::turn_item::common(item))
            .filter(|common| common.run_id == Some(&run.id))
            .map(|common| common.ordinal)
            .max()
            .unwrap_or(0)
            + 1;
        let id = derive::run_signal_turn_item(&run.id, input.signal);
        let item = match input.item {
            SettleItem::Error { title, failure } => OrchestrationV2TurnItem::Error {
                tool_non_execution_kind: None,
                tool_surface: None,
                tool_icon: None,
                tool_source: None,
                id,
                thread_id: thread_id.clone(),
                run_id: Some(run.id.clone()),
                node_id: Some(root_node.id.clone()),
                provider_thread_id: Some(input.item_provider_thread_id.clone()),
                provider_turn_id: None,
                native_item_ref: None,
                parent_item_id: None,
                ordinal,
                status: item_status,
                title: Some(title),
                started_at: Some(now),
                completed_at: Some(now),
                updated_at: now,
                failure,
                retry: None,
            },
            SettleItem::Command {
                title,
                input: command,
                output,
                exit_code,
            } => OrchestrationV2TurnItem::CommandExecution {
                tool_non_execution_kind: None,
                tool_surface: None,
                tool_icon: None,
                tool_source: None,
                id,
                thread_id: thread_id.clone(),
                run_id: Some(run.id.clone()),
                node_id: Some(root_node.id.clone()),
                provider_thread_id: Some(input.item_provider_thread_id.clone()),
                provider_turn_id: None,
                native_item_ref: None,
                parent_item_id: None,
                ordinal,
                status: item_status,
                title: Some(title),
                started_at: Some(now),
                completed_at: Some(now),
                updated_at: now,
                input: command,
                output: Some(output),
                output_omitted: None,
                output_indicates_failure: None,
                exit_code: Some(exit_code),
            },
        };
        let mut settled_run = (*run).clone();
        settled_run.status = run_status;
        settled_run.completed_at = Some(now);
        let mut settled_attempt = (*attempt).clone();
        settled_attempt.status = attempt_status;
        settled_attempt.completed_at = Some(now);
        let mut settled_node = (*root_node).clone();
        settled_node.status = node_status;
        settled_node.completed_at = Some(now);
        if let Some(started_at) = input.started_at {
            settled_run.started_at = Some(started_at);
            settled_attempt.started_at = Some(started_at);
            settled_node.started_at = Some(started_at);
        }
        let allocate = || IdAllocator.event(Some(thread_id), None, None);
        let provider_instance_id = Some(input.provider_instance_id);
        let run_id = Some(run.id.clone());
        let node_id = Some(root_node.id.clone());
        macro_rules! scoped {
            ($variant:ident, $payload:expr) => {
                OrchestrationV2DomainEvent::$variant {
                    id: allocate(),
                    thread_id: thread_id.clone(),
                    run_id: run_id.clone(),
                    node_id: node_id.clone(),
                    driver: None,
                    provider_instance_id: provider_instance_id.clone(),
                    raw_event_id: None,
                    occurred_at: now,
                    payload: $payload,
                }
            };
        }
        let mut events = vec![
            scoped!(TurnItemUpdated, item),
            scoped!(RunUpdated, settled_run),
            scoped!(RunAttemptUpdated, settled_attempt),
            scoped!(NodeUpdated, settled_node),
        ];
        if let Some(provider_thread) = input.provider_thread_update {
            events.push(scoped!(ProviderThreadUpdated, provider_thread));
        }
        self.events
            .write_if_run_current(RunCurrentWrite {
                write: WriteInput::events(events),
                thread_id: thread_id.clone(),
                run_id: run.id.clone(),
                active_attempt_id: attempt.id.clone(),
                expected_status: OrchestrationV2RunStatus::Starting,
            })
            .await?;
        Ok(())
    }
}
