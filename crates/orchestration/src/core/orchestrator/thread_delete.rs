//! Port of upstream `Orchestrator.ts` `thread.delete` (`planThreadDeletion`,
//! `dispatchOnce`). The plan itself is [`plan_thread_deletion`]; this file
//! reads the records it needs and records what it returns.

use turin_contracts::orchestration_v2::{
    OrchestrationV2Command as C, OrchestrationV2ServerCommand,
};

use super::Orchestrator;
use super::dispatch::{Decision, dispatch_failure};
use crate::pipeline::orchestrator_port::{
    OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
};
use crate::pipeline::thread_deletion::plan_thread_deletion;

/// Decides `thread.delete`. `Ok(false)` leaves the command to another group.
pub(super) async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    let OrchestrationV2ServerCommand::Client(C::ThreadDelete {
        command_id,
        thread_id,
    }) = command
    else {
        return Ok(false);
    };

    let projection = orchestrator
        .get_thread_records(
            thread_id,
            &[
                RecordField::Runs,
                RecordField::Attempts,
                RecordField::Nodes,
                RecordField::RuntimeRequests,
                RecordField::Subagents,
                RecordField::ProviderSessions,
            ],
            &RecordFilter::default(),
        )
        .await?;
    let attachment_ids = orchestrator
        .projection
        .get_thread_attachment_ids(thread_id)
        .await
        .map_err(|cause| dispatch_failure(command, cause))?;
    let planned = plan_thread_deletion(
        command_id,
        thread_id,
        &projection,
        &attachment_ids,
        decision.now(),
        &orchestrator.ids,
    )
    .map_err(|error| dispatch_failure(command, error))?;

    for event in planned.events {
        decision.emit(event);
    }
    decision.effects.extend(planned.effects);
    Ok(true)
}

#[cfg(test)]
mod tests;
