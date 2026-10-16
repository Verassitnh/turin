//! Port of upstream `Orchestrator.ts` `dispatchQueuedRunCancel` (7855).
//!
//! `queued-run.cancel` ends a queued run, or, when the run carries a
//! delegated-completion delivery, disposes the cohort that owns it. Stop
//! also cancels buffered provider wakes through [`cancel_queued_run`].

use turin_contracts::{
    RunId, ThreadId,
    orchestration_v2::{
        OrchestrationV2Command as C, OrchestrationV2ConversationMessageRole,
        OrchestrationV2RunStatus, OrchestrationV2ServerCommand,
    },
};

use super::Orchestrator;
use super::cohort::{CohortEnd, dispose_delegated_completion_cohort, emit_queued_run_cancellation};
use super::dispatch::{Decision, dispatch_failure};
use crate::pipeline::orchestrator_port::{
    OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
};

/// Decides `queued-run.cancel`. `Ok(false)` leaves the command to another group.
pub(super) async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    let OrchestrationV2ServerCommand::Client(C::QueuedRunCancel {
        thread_id, run_id, ..
    }) = command
    else {
        return Ok(false);
    };
    cancel_queued_run(orchestrator, command, decision, thread_id, run_id).await?;
    Ok(true)
}

/// `dispatchQueuedRunCancel`'s body for one queued run of `thread_id`.
pub(super) async fn cancel_queued_run(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    thread_id: &ThreadId,
    run_id: &RunId,
) -> Result<(), OrchestratorError> {
    let projection = orchestrator
        .get_thread_records(
            thread_id,
            &[
                RecordField::Runs,
                RecordField::Messages,
                RecordField::Nodes,
                RecordField::Attempts,
                RecordField::Subagents,
            ],
            &RecordFilter {
                message_roles: Some(vec![OrchestrationV2ConversationMessageRole::User]),
                turn_item_types: Some(Vec::new()),
                ..RecordFilter::default()
            },
        )
        .await?;
    let Some(queued) = projection
        .runs
        .iter()
        .find(|candidate| &candidate.id == run_id)
        .filter(|candidate| candidate.status == OrchestrationV2RunStatus::Queued)
    else {
        return Err(dispatch_failure(
            command,
            format!("Run {run_id} is not queued."),
        ));
    };
    let now = decision.now();
    let queued_message = projection
        .messages
        .iter()
        .find(|candidate| candidate.id == queued.user_message_id);
    if let Some(completion) =
        queued_message.and_then(|message| message.delegated_completion.as_ref())
    {
        dispose_delegated_completion_cohort(
            decision,
            &projection,
            &completion.parent_run_id,
            CohortEnd::Disposed,
            now,
            true,
        );
        return Ok(());
    }
    emit_queued_run_cancellation(decision, &projection, queued, now);
    Ok(())
}
