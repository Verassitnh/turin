//! Port of upstream `Orchestrator.ts` `dispatchThreadMergeBack` (`thread.merge_back`).
//!
//! Plans a pending `merge_back` transfer from a fork back into its parent. Any
//! pending merge-back for the same pair is superseded first, so only the newest
//! transfer stays pending.
//!
//! Not ported: the dispatch mode limit check (see `thread_fork`).

use turin_contracts::orchestration_v2::{
    OrchestrationV2Command as C, OrchestrationV2ContextTransfer,
    OrchestrationV2ContextTransferStatus, OrchestrationV2ContextTransferType,
    OrchestrationV2DomainEvent, OrchestrationV2LineageRelationship, OrchestrationV2RunStatus,
    OrchestrationV2ServerCommand,
};

use super::Orchestrator;
use super::dispatch::{Decision, dispatch_failure};
use super::thread_fork::{
    context_source_point_for_run, provider_thread_for_run, run_for_source_point, source_point_type,
};
use crate::pipeline::orchestrator_port::{
    OrchestratorError, OrchestratorPort, RecordField, RecordFilter, mode_text,
};

/// Decides `thread.merge_back`. `Ok(false)` leaves the command to another group.
pub(super) async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    let OrchestrationV2ServerCommand::Client(C::ThreadMergeBack {
        created_by,
        source_thread_id,
        target_thread_id,
        source_point,
        created_at,
        ..
    }) = command
    else {
        return Ok(false);
    };

    let source = orchestrator
        .get_thread_records(
            source_thread_id,
            &[
                RecordField::Runs,
                RecordField::Checkpoints,
                RecordField::ProviderThreads,
                RecordField::ProviderTurns,
                RecordField::Attempts,
                RecordField::ContextTransfers,
            ],
            &RecordFilter::default(),
        )
        .await?;
    let target = orchestrator
        .get_thread_records(
            target_thread_id,
            &[RecordField::ContextTransfers],
            &RecordFilter::default(),
        )
        .await?;

    let lineage = &source.thread.lineage;
    if lineage.relationship_to_parent != Some(OrchestrationV2LineageRelationship::Fork)
        || lineage.parent_thread_id.as_ref() != Some(target_thread_id)
    {
        return Err(dispatch_failure(
            command,
            format!("Thread {source_thread_id} is not a fork of {target_thread_id}."),
        ));
    }

    let Some(source_run) = run_for_source_point(&source, source_point) else {
        return Err(dispatch_failure(
            command,
            format!(
                "No stable source run was found for merge-back source {}.",
                source_point_type(source_point)
            ),
        ));
    };
    if !matches!(
        source_run.status,
        OrchestrationV2RunStatus::Completed | OrchestrationV2RunStatus::Waiting
    ) {
        return Err(dispatch_failure(
            command,
            format!(
                "Merge-back source run {} is {}; only provider-finished runs are supported.",
                source_run.id,
                mode_text(&source_run.status)
            ),
        ));
    }

    let fork_transfer = source
        .context_transfers
        .iter()
        .rev()
        .find(|transfer| {
            transfer.r#type == OrchestrationV2ContextTransferType::Fork
                && &transfer.source_thread_id == target_thread_id
                && &transfer.target_thread_id == source_thread_id
        })
        .ok_or_else(|| {
            dispatch_failure(
                command,
                format!(
                    "No fork transfer exists between {target_thread_id} and {source_thread_id}."
                ),
            )
        })?;

    let now = created_at.unwrap_or_else(|| decision.now());
    let transfer_id =
        orchestrator
            .ids
            .context_transfer(source_thread_id, target_thread_id, "merge_back");
    let superseded: Vec<&OrchestrationV2ContextTransfer> = target
        .context_transfers
        .iter()
        .filter(|transfer| {
            transfer.r#type == OrchestrationV2ContextTransferType::MergeBack
                && transfer.status == OrchestrationV2ContextTransferStatus::Pending
                && &transfer.source_thread_id == source_thread_id
                && &transfer.target_thread_id == target_thread_id
        })
        .collect();
    let source_provider_thread = provider_thread_for_run(&source, source_run);
    let transfer = OrchestrationV2ContextTransfer {
        id: transfer_id.clone(),
        r#type: OrchestrationV2ContextTransferType::MergeBack,
        source_thread_id: source_thread_id.clone(),
        target_thread_id: target_thread_id.clone(),
        source_point: context_source_point_for_run(&source, source_run),
        base_point: Some(fork_transfer.source_point.clone()),
        source_provider_instance_id: Some(source_run.provider_instance_id.clone()),
        target_provider_instance_id: Some(target.thread.model_selection.instance_id.clone()),
        target_run_id: None,
        status: OrchestrationV2ContextTransferStatus::Pending,
        resolution: None,
        created_by: *created_by,
        error: source_provider_thread
            .is_none()
            .then(|| "Source merge-back run has no provider thread.".to_owned()),
        created_at: now,
        updated_at: now,
        consumed_at: None,
    };

    for pending in superseded {
        decision.emit(OrchestrationV2DomainEvent::ContextTransferUpdated {
            id: decision.event_id(target_thread_id),
            thread_id: target_thread_id.clone(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id: Some(source_run.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: OrchestrationV2ContextTransfer {
                status: OrchestrationV2ContextTransferStatus::Superseded,
                error: Some(format!("Superseded by merge-back transfer {transfer_id}.")),
                updated_at: now,
                ..pending.clone()
            },
        });
    }
    decision.emit(OrchestrationV2DomainEvent::ContextTransferCreated {
        id: decision.event_id(target_thread_id),
        thread_id: target_thread_id.clone(),
        run_id: None,
        node_id: None,
        driver: None,
        provider_instance_id: Some(source_run.provider_instance_id.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: transfer,
    });
    Ok(true)
}

#[cfg(test)]
mod tests;
