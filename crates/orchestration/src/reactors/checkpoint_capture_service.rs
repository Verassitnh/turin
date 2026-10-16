//! Port of upstream/apps/server/src/orchestration-v2/CheckpointCaptureService.ts
//!
//! Runs after a turn that is waiting for its checkpoint. It snapshots the
//! workspace through [`CheckpointService`], then commits the checkpoint, its
//! turn item, and the run and root node settlement in one command.

use std::sync::Arc;

use turin_contracts::orchestration_v2::{
    OrchestrationV2Checkpoint, OrchestrationV2DomainEvent, OrchestrationV2ExecutionNode,
    OrchestrationV2ExecutionNodeStatus, OrchestrationV2ProviderThread, OrchestrationV2Run,
    OrchestrationV2RunStatus, OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
};
use turin_contracts::{CheckpointScopeId, CommandId, RunId, ThreadId, UtcDateTime};
use turin_provider_core::id_allocator::{IdAllocator, derive};

use super::checkpoint_service::{CaptureInput, CheckpointService};
use crate::pipeline::event_sink::{CommandCommit, EventSink};
use crate::projection::store::ProjectionStore;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Checkpoint capture for run {run_id} on scope {scope_id} failed: {cause}")]
pub struct CheckpointCaptureExecutionError {
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub scope_id: CheckpointScopeId,
    pub cause: String,
}

/// What [`plan_capture`] decided for a persisted capture target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturePlan {
    /// Already committed, or rolled back: nothing to write.
    Skip,
    /// The run is not waiting for a checkpoint, or the scope and root node disagree.
    Invalid,
    /// Capture now. `stopped` means the run was interrupted or cancelled: its
    /// status is left alone and the root node is not settled.
    Capture { stopped: bool },
}

/// The capture guard, over the persisted run and two facts about the scope:
/// whether the root node names this scope, and whether a provider thread exists.
///
/// A settled run that already names a checkpoint means an earlier execution
/// committed (at-least-once delivery).
pub fn plan_capture(
    run: Option<&OrchestrationV2Run>,
    root_matches_scope: bool,
    has_provider_thread: bool,
) -> CapturePlan {
    // A stopped run is already terminal. Its checkpoint is the rollback point for the message
    // after it, so capture leaves its status alone.
    let stopped = run.is_some_and(|run| {
        matches!(
            run.status,
            OrchestrationV2RunStatus::Interrupted | OrchestrationV2RunStatus::Cancelled
        )
    });
    if let Some(run) = run {
        // Rollback shares this lane, so it can only land before a capture runs, e.g. while a
        // failed capture waits to retry. The workspace now holds the rollback target and the run
        // must stay discarded.
        if run.status == OrchestrationV2RunStatus::RolledBack {
            return CapturePlan::Skip;
        }
        if run.checkpoint_id.is_some()
            && (run.status == OrchestrationV2RunStatus::Completed || stopped)
        {
            return CapturePlan::Skip;
        }
    }
    let waiting = run.is_some_and(|run| run.status == OrchestrationV2RunStatus::Waiting);
    if (waiting || stopped) && root_matches_scope && has_provider_thread {
        CapturePlan::Capture { stopped }
    } else {
        CapturePlan::Invalid
    }
}

pub struct CheckpointCaptureService {
    checkpoints: Arc<CheckpointService>,
    sink: Arc<EventSink>,
    projections: Arc<ProjectionStore>,
    ids: IdAllocator,
}

/// Everything a capture needs, once the guard has passed.
struct CaptureTarget {
    run: OrchestrationV2Run,
    root_node: OrchestrationV2ExecutionNode,
    scope: turin_contracts::orchestration_v2::OrchestrationV2CheckpointScope,
    provider_thread: OrchestrationV2ProviderThread,
}

impl CheckpointCaptureService {
    pub fn new(
        checkpoints: Arc<CheckpointService>,
        sink: Arc<EventSink>,
        projections: Arc<ProjectionStore>,
    ) -> Self {
        Self {
            checkpoints,
            sink,
            projections,
            ids: IdAllocator,
        }
    }

    /// `execute`.
    pub async fn execute(
        &self,
        thread_id: &ThreadId,
        run_id: &RunId,
        scope_id: &CheckpointScopeId,
    ) -> Result<(), CheckpointCaptureExecutionError> {
        let error = |cause: String| CheckpointCaptureExecutionError {
            thread_id: thread_id.clone(),
            run_id: run_id.clone(),
            scope_id: scope_id.clone(),
            cause,
        };
        let context = self
            .projections
            .get_checkpoint_capture_context(thread_id, run_id.as_str(), scope_id.as_str())
            .await
            .map_err(|source| error(source.to_string()))?;

        let root_matches_scope = match (&context.root_node, &context.scope) {
            (Some(root), Some(scope)) => root.checkpoint_scope_id.as_ref() == Some(&scope.id),
            _ => false,
        };
        let has_provider_thread = context.provider_thread.is_some();
        let stopped = match plan_capture(
            context.run.as_ref(),
            root_matches_scope,
            has_provider_thread,
        ) {
            CapturePlan::Skip => return Ok(()),
            CapturePlan::Invalid => {
                return Err(error(
                    "The persisted checkpoint capture target is incomplete or no longer waiting."
                        .to_owned(),
                ));
            }
            CapturePlan::Capture { stopped } => stopped,
        };
        // `plan_capture` only returns Capture when all four are present.
        let (Some(run), Some(root_node), Some(scope), Some(provider_thread)) = (
            context.run,
            context.root_node,
            context.scope,
            context.provider_thread,
        ) else {
            return Err(error("capture target vanished between reads".to_owned()));
        };
        let ready_ordinals: Vec<u64> = context
            .ready_checkpoint_ordinals
            .iter()
            .filter_map(|ordinal| u64::try_from(*ordinal).ok())
            .collect();
        let target = CaptureTarget {
            run,
            root_node,
            scope,
            provider_thread,
        };
        self.capture_and_commit(thread_id, target, &ready_ordinals, stopped)
            .await
            .map_err(error)
    }

    async fn capture_and_commit(
        &self,
        thread_id: &ThreadId,
        target: CaptureTarget,
        ready_ordinals: &[u64],
        stopped: bool,
    ) -> Result<(), String> {
        let CaptureTarget {
            run,
            root_node,
            scope,
            provider_thread,
        } = target;
        let captured_at = UtcDateTime::now();
        let run_ordinal = run.ordinal.get();
        let baseline_ordinal = run_ordinal.saturating_sub(1);
        let has_ready = |ordinal: u64| ready_ordinals.contains(&ordinal);

        let thread_start = if baseline_ordinal == 0 || has_ready(0) {
            None
        } else {
            Some(
                self.checkpoints
                    .materialize_baseline_checkpoint(&scope, 0)
                    .await
                    .map_err(|source| source.to_string())?,
            )
        };
        let baseline = if has_ready(baseline_ordinal) {
            None
        } else {
            Some(
                self.checkpoints
                    .materialize_baseline_checkpoint(&scope, baseline_ordinal)
                    .await
                    .map_err(|source| source.to_string())?,
            )
        };
        let checkpoint = self
            .checkpoints
            .capture(CaptureInput {
                scope: &scope,
                run_id: Some(run.id.clone()),
                node_id: root_node.id.clone(),
                ordinal_within_scope: run_ordinal,
                app_run_ordinal: Some(run.ordinal),
                captured_at,
            })
            .await
            .map_err(|source| source.to_string())?;

        // Match RunExecutionService: capture loaded the waiting run before materializing
        // baselines. Clearing delegatedCompletion leaves a newer cohort write alone, since the
        // projection keeps that field when a payload omits it.
        let mut run_payload = run.clone();
        run_payload.delegated_completion = None;
        let command_id = CommandId::new(format!("command:effect:checkpoint.capture:{}", run.id))
            .map_err(|source| source.to_string())?;
        let event_id = || self.ids.event(Some(thread_id), Some(&command_id), None);
        let driver = Some(provider_thread.driver.clone());
        let provider_instance_id = Some(run.provider_instance_id.clone());

        let mut events = Vec::new();
        for baseline in [thread_start, baseline].into_iter().flatten() {
            events.push(OrchestrationV2DomainEvent::CheckpointCaptured {
                id: event_id(),
                thread_id: thread_id.clone(),
                run_id: None,
                node_id: Some(baseline.node_id.clone()),
                driver: driver.clone(),
                provider_instance_id: provider_instance_id.clone(),
                raw_event_id: None,
                occurred_at: captured_at,
                payload: baseline,
            });
        }
        events.push(OrchestrationV2DomainEvent::CheckpointCaptured {
            id: event_id(),
            thread_id: thread_id.clone(),
            run_id: Some(run.id.clone()),
            node_id: Some(root_node.id.clone()),
            driver: driver.clone(),
            provider_instance_id: provider_instance_id.clone(),
            raw_event_id: None,
            occurred_at: captured_at,
            payload: checkpoint.clone(),
        });
        events.push(OrchestrationV2DomainEvent::TurnItemUpdated {
            id: event_id(),
            thread_id: thread_id.clone(),
            run_id: Some(run.id.clone()),
            node_id: Some(root_node.id.clone()),
            driver,
            provider_instance_id: provider_instance_id.clone(),
            raw_event_id: None,
            occurred_at: captured_at,
            payload: checkpoint_turn_item(
                &run,
                &root_node,
                &provider_thread,
                &checkpoint,
                captured_at,
            ),
        });
        run_payload.checkpoint_id = Some(checkpoint.id.clone());
        if !stopped {
            run_payload.status = OrchestrationV2RunStatus::Completed;
            run_payload.completed_at = Some(captured_at);
        }
        events.push(OrchestrationV2DomainEvent::RunUpdated {
            id: event_id(),
            thread_id: thread_id.clone(),
            run_id: Some(run.id.clone()),
            node_id: Some(root_node.id.clone()),
            driver: None,
            provider_instance_id: provider_instance_id.clone(),
            raw_event_id: None,
            occurred_at: captured_at,
            payload: run_payload,
        });
        if !stopped {
            let mut node_payload = root_node.clone();
            node_payload.status = OrchestrationV2ExecutionNodeStatus::Completed;
            node_payload.completed_at = Some(captured_at);
            node_payload.checkpoint_scope_id = Some(scope.id.clone());
            events.push(OrchestrationV2DomainEvent::NodeUpdated {
                id: event_id(),
                thread_id: thread_id.clone(),
                run_id: Some(run.id.clone()),
                node_id: Some(root_node.id.clone()),
                driver: None,
                provider_instance_id,
                raw_event_id: None,
                occurred_at: captured_at,
                payload: node_payload,
            });
        }

        self.sink
            .commit_command(CommandCommit {
                command_id,
                thread_id: thread_id.clone(),
                command_type: "checkpoint.capture".to_owned(),
                accepted_at: captured_at,
                events,
                effects: Vec::new(),
                cancel_unsettled_effects: None,
            })
            .await
            .map(|_| ())
            .map_err(|source| source.to_string())
    }
}

/// `makeCheckpointTurnItem`: the timeline row that shows the checkpoint's file chip.
fn checkpoint_turn_item(
    run: &OrchestrationV2Run,
    root_node: &OrchestrationV2ExecutionNode,
    provider_thread: &OrchestrationV2ProviderThread,
    checkpoint: &OrchestrationV2Checkpoint,
    completed_at: UtcDateTime,
) -> OrchestrationV2TurnItem {
    OrchestrationV2TurnItem::Checkpoint {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: derive::turn_item_from_provider_item(
            &provider_thread.driver,
            &format!("checkpoint:{}", checkpoint.id),
        ),
        thread_id: run.thread_id.clone(),
        run_id: Some(run.id.clone()),
        node_id: Some(root_node.id.clone()),
        provider_thread_id: Some(provider_thread.id.clone()),
        provider_turn_id: root_node.provider_turn_id.clone(),
        native_item_ref: None,
        parent_item_id: None,
        ordinal: run.ordinal.get() * 100 + 99,
        status: OrchestrationV2TurnItemStatus::Completed,
        title: None,
        started_at: Some(completed_at),
        completed_at: Some(completed_at),
        updated_at: completed_at,
        checkpoint_id: checkpoint.id.clone(),
        scope_id: checkpoint.scope_id.clone(),
        files: checkpoint.files.clone(),
    }
}

#[cfg(test)]
mod tests;
