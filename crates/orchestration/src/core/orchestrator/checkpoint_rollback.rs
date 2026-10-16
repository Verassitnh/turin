//! Port of `dispatchCheckpointRollback` and `dispatchCheckpointRollbackFail`
//! (upstream `Orchestrator.ts` 9667, 9849). A rollback names a ready checkpoint
//! of the thread's active provider thread; the decision records the request on
//! the thread and queues the `provider-thread.rollback` effect that rewinds the
//! provider (and, unless declined, the files).

use turin_contracts::{
    TrimmedNonEmptyString,
    orchestration_v2::{
        OrchestrationV2CheckpointRollbackRequest, OrchestrationV2CheckpointStatus,
        OrchestrationV2Command as C, OrchestrationV2DomainEvent,
        OrchestrationV2InternalCommand as I, OrchestrationV2RollbackFailure,
        OrchestrationV2ServerCommand,
    },
};

use super::Orchestrator;
use super::dispatch::{Decision, command_id, dispatch_failure};
use super::thread_fork::provider_turn_for_run;
use crate::pipeline::command_policy::{CapabilityCheck, ensure_rollback};
use crate::pipeline::effect_outbox::{EffectRequest, PendingEffect};
use crate::pipeline::orchestrator_port::{
    OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
};
use crate::reactors::checkpoint_restore_safety::{
    SHARED_WORKSPACE_RESTORE_MESSAGE, is_checkpoint_restore_isolated,
};

/// Decides `checkpoint.rollback` and `checkpoint.rollback.fail`.
/// `Ok(false)` leaves the command to another group.
pub(super) async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    match command {
        OrchestrationV2ServerCommand::Client(C::CheckpointRollback {
            restore_files,
            thread_id,
            scope_id,
            checkpoint_id,
            ..
        }) => {
            rollback(
                orchestrator,
                command,
                decision,
                thread_id,
                scope_id,
                checkpoint_id,
                *restore_files,
            )
            .await?;
            Ok(true)
        }
        OrchestrationV2ServerCommand::Internal(I::CheckpointRollbackFail {
            thread_id,
            request_id,
            message,
            ..
        }) => {
            rollback_fail(
                orchestrator,
                command,
                decision,
                thread_id,
                request_id,
                message,
            )
            .await?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

async fn rollback(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    thread_id: &turin_contracts::ThreadId,
    scope_id: &turin_contracts::CheckpointScopeId,
    checkpoint_id: &turin_contracts::CheckpointId,
    restore_files: Option<bool>,
) -> Result<(), OrchestratorError> {
    let projection = orchestrator
        .get_thread_records(
            thread_id,
            &[
                RecordField::ProviderThreads,
                RecordField::Checkpoints,
                RecordField::CheckpointScopes,
                RecordField::Runs,
                RecordField::ProviderTurns,
                RecordField::Attempts,
            ],
            &RecordFilter {
                turn_item_types: Some(Vec::new()),
                message_roles: Some(vec![
                    turin_contracts::orchestration_v2::OrchestrationV2ConversationMessageRole::User,
                ]),
                ..RecordFilter::default()
            },
        )
        .await?;
    let provider_thread = projection.provider_threads.iter().find(|candidate| {
        Some(&candidate.id) == projection.thread.active_provider_thread_id.as_ref()
    });
    let Some(provider_thread) = provider_thread else {
        return Err(dispatch_failure(
            command,
            "No active provider thread exists for rollback.",
        ));
    };
    if provider_thread.provider_session_id.is_none() {
        return Err(dispatch_failure(
            command,
            format!(
                "Provider thread {} has no provider session.",
                provider_thread.id
            ),
        ));
    }

    let model_selection = &projection.thread.model_selection;
    let adapter = orchestrator
        .seams
        .adapters
        .adapter(&model_selection.instance_id)
        .ok_or_else(|| {
            dispatch_failure(
                command,
                format!(
                    "Provider instance {} is not available.",
                    model_selection.instance_id
                ),
            )
        })?;
    let capabilities = adapter
        .get_capabilities()
        .await
        .map_err(|cause| dispatch_failure(command, cause.to_string()))?;
    ensure_rollback(&CapabilityCheck {
        command_id: command_id(command),
        thread_id,
        provider_instance_id: &model_selection.instance_id,
        capabilities: &capabilities,
    })
    .map_err(|cause| dispatch_failure(command, cause.to_string()))?;

    let Some(target_checkpoint) = projection
        .checkpoints
        .iter()
        .find(|candidate| &candidate.id == checkpoint_id)
    else {
        return Err(dispatch_failure(
            command,
            format!("Checkpoint {checkpoint_id} was not found."),
        ));
    };
    if target_checkpoint.status != OrchestrationV2CheckpointStatus::Ready {
        return Err(dispatch_failure(
            command,
            format!(
                "Checkpoint {checkpoint_id} is {} and cannot be restored.",
                serde_json::to_value(target_checkpoint.status)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default()
            ),
        ));
    }
    let Some(target_scope) = projection
        .checkpoint_scopes
        .iter()
        .find(|candidate| candidate.id == target_checkpoint.scope_id)
    else {
        return Err(dispatch_failure(
            command,
            format!(
                "Checkpoint scope {} was not found.",
                target_checkpoint.scope_id
            ),
        ));
    };
    if &target_scope.id != scope_id {
        return Err(dispatch_failure(
            command,
            format!(
                "Checkpoint {checkpoint_id} belongs to scope {}, not {scope_id}.",
                target_scope.id
            ),
        ));
    }
    if restore_files != Some(false) {
        let isolated = is_checkpoint_restore_isolated(
            thread_id,
            projection
                .thread
                .worktree_path
                .as_ref()
                .map(TrimmedNonEmptyString::as_str),
            target_scope.cwd.as_str(),
            orchestrator.seams.workspace_claims.as_ref(),
        )
        .await
        .map_err(|cause| dispatch_failure(command, cause.to_string()))?;
        if !isolated {
            return Err(dispatch_failure(command, SHARED_WORKSPACE_RESTORE_MESSAGE));
        }
    }

    let target_ordinal = target_checkpoint
        .app_run_ordinal
        .map_or(0, |ordinal| ordinal.get());
    if target_ordinal > 0 {
        let target_run = projection
            .runs
            .iter()
            .find(|run| run.ordinal.get() == target_ordinal);
        let target_turn = target_run.and_then(|run| provider_turn_for_run(&projection, run));
        let Some(target_turn) = target_turn else {
            return Err(dispatch_failure(
                command,
                format!(
                    "Cannot roll back to checkpoint {}: its provider turn is unavailable.",
                    target_checkpoint.id
                ),
            ));
        };
        if target_turn.provider_thread_id != provider_thread.id {
            return Err(dispatch_failure(
                command,
                format!(
                    "Cannot roll back provider thread {} to checkpoint {}: target provider turn {} belongs to provider thread {}.",
                    provider_thread.id,
                    target_checkpoint.id,
                    target_turn.id,
                    target_turn.provider_thread_id
                ),
            ));
        }
    }

    let now = decision.now();
    // This rollback becomes the only one whose failure the thread records.
    let id = decision.event_id(thread_id);
    decision.emit(OrchestrationV2DomainEvent::ThreadMetadataUpdated {
        id,
        thread_id: thread_id.clone(),
        run_id: None,
        node_id: None,
        driver: None,
        provider_instance_id: Some(projection.thread.provider_instance_id.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: turin_contracts::orchestration_v2::OrchestrationV2AppThread {
            rollback_request_id: Some(command_id(command).clone()),
            rollback_failure: Some(None),
            updated_at: now,
            ..projection.thread.clone()
        },
    });
    let id = decision.event_id(thread_id);
    decision.emit(OrchestrationV2DomainEvent::CheckpointRollbackRequested {
        id,
        thread_id: thread_id.clone(),
        run_id: None,
        node_id: None,
        driver: None,
        provider_instance_id: Some(model_selection.instance_id.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: OrchestrationV2CheckpointRollbackRequest {
            scope_id: target_scope.id.clone(),
            checkpoint_id: target_checkpoint.id.clone(),
            requested_at: now,
        },
    });
    decision.effects.push(PendingEffect {
        id: format!(
            "effect:{}:provider-thread.rollback:{}:{}",
            command_id(command),
            provider_thread.id,
            target_checkpoint.id
        ),
        command_id: command_id(command).clone(),
        thread_id: thread_id.clone(),
        request: EffectRequest::ProviderThreadRollback {
            restore_files,
            provider_thread_id: provider_thread.id.clone(),
            checkpoint_id: target_checkpoint.id.clone(),
            scope_id: target_scope.id.clone(),
        },
        available_at: None,
    });
    Ok(())
}

/// Records a provider rollback that failed after every retry, so clients
/// waiting on it stop and show the reason. A newer rollback clears it, and a
/// late failure from a rollback it superseded is rejected.
async fn rollback_fail(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    thread_id: &turin_contracts::ThreadId,
    request_id: &turin_contracts::CommandId,
    message: &TrimmedNonEmptyString,
) -> Result<(), OrchestratorError> {
    let thread = orchestrator
        .projection
        .get_thread(thread_id)
        .await
        .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;
    if thread.deleted_at.is_some() {
        return Ok(());
    }
    if thread.rollback_request_id.as_ref() != Some(request_id) {
        return Err(dispatch_failure(
            command,
            format!("Rollback {request_id} is no longer the thread's current rollback."),
        ));
    }
    let now = decision.now();
    let id = decision.event_id(thread_id);
    decision.emit(OrchestrationV2DomainEvent::ThreadMetadataUpdated {
        id,
        thread_id: thread_id.clone(),
        run_id: None,
        node_id: None,
        driver: None,
        provider_instance_id: Some(thread.provider_instance_id.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: turin_contracts::orchestration_v2::OrchestrationV2AppThread {
            rollback_failure: Some(Some(OrchestrationV2RollbackFailure {
                request_id: request_id.clone(),
                message: message.clone(),
            })),
            updated_at: now,
            ..thread
        },
    });
    Ok(())
}
