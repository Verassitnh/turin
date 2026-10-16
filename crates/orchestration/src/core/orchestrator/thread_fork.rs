//! Port of upstream `Orchestrator.ts` `dispatchThreadFork` (`thread.fork`).
//!
//! Reads the source thread's run at the requested source point, plans the new
//! thread and its pending fork transfer with [`thread_fork_service`], and
//! records both. The source-point helpers here are shared with merge-back.
//!
//! The source is not under the command's lock, so a limited sender's
//! `DispatchModeLimit` is checked against the source copy read here.

use turin_contracts::orchestration_v2::OrchestrationV2Command as C;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ContextSourcePoint, OrchestrationV2DomainEvent, OrchestrationV2ProviderThread,
    OrchestrationV2ProviderTurn, OrchestrationV2Run, OrchestrationV2RunStatus,
    OrchestrationV2ServerCommand, OrchestrationV2ThreadForkSourcePoint,
    OrchestrationV2ThreadProjection, latest_provider_turn_for_attempt,
};

use super::Orchestrator;
use super::dispatch::{Decision, dispatch_failure, refuse_above_dispatch_mode_limit};
use crate::pipeline::dispatch_mode_limit::DispatchModes;
use crate::pipeline::orchestrator_port::{
    OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
};
use crate::pipeline::thread_fork_service::{
    ForkPlanInput, forkable_source_run_status_error, is_forkable_source_run_status, plan,
};

/// Decides `thread.fork`. `Ok(false)` leaves the command to another group.
pub(super) async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    let OrchestrationV2ServerCommand::Client(C::ThreadFork {
        created_by,
        creation_source,
        source_thread_id,
        target_thread_id,
        source_point,
        title,
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
    // The source is not under this command's lock, so check the modes this
    // command copies, not an earlier read the source's user could outrun.
    refuse_above_dispatch_mode_limit(
        decision.mode_limit(),
        command,
        source_thread_id,
        DispatchModes {
            runtime_mode: source.thread.runtime_mode,
            interaction_mode: source.thread.interaction_mode,
        },
    )?;

    let Some(source_run) = run_for_source_point(&source, source_point) else {
        return Err(dispatch_failure(
            command,
            format!(
                "No stable source run was found for fork source {}.",
                source_point_type(source_point)
            ),
        ));
    };
    if !is_forkable_source_run_status(source_run.status) {
        return Err(dispatch_failure(
            command,
            forkable_source_run_status_error(source_run),
        ));
    }

    let now = created_at.unwrap_or_else(|| decision.now());
    let transfer_id = orchestrator
        .ids
        .context_transfer(source_thread_id, target_thread_id, "fork");
    let forked = plan(ForkPlanInput {
        source_thread: &source.thread,
        source_run,
        source_provider_thread: provider_thread_for_run(&source, source_run),
        canonical_source_point: context_source_point_for_run(&source, source_run),
        transfer_id,
        target_thread_id: target_thread_id.clone(),
        title: title.as_ref().map(|title| title.as_str().to_owned()),
        created_by: *created_by,
        creation_source: *creation_source,
        created_at: now,
    })
    .map_err(|error| dispatch_failure(command, error))?;

    let provider_instance_id = forked.target_thread.provider_instance_id.clone();
    let thread_event = OrchestrationV2DomainEvent::ThreadCreated {
        id: decision.event_id(target_thread_id),
        thread_id: target_thread_id.clone(),
        run_id: None,
        node_id: None,
        driver: None,
        provider_instance_id: Some(provider_instance_id),
        raw_event_id: None,
        occurred_at: now,
        payload: forked.target_thread,
    };
    decision.emit(thread_event);
    decision.emit(OrchestrationV2DomainEvent::ContextTransferCreated {
        id: decision.event_id(target_thread_id),
        thread_id: target_thread_id.clone(),
        run_id: None,
        node_id: None,
        driver: None,
        provider_instance_id: Some(source_run.provider_instance_id.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: forked.transfer,
    });
    Ok(true)
}

/// The source-point type as upstream names it in its error text.
pub(super) fn source_point_type(
    source_point: &OrchestrationV2ThreadForkSourcePoint,
) -> &'static str {
    match source_point {
        OrchestrationV2ThreadForkSourcePoint::LatestStable => "latest_stable",
        OrchestrationV2ThreadForkSourcePoint::Run { .. } => "run",
        OrchestrationV2ThreadForkSourcePoint::Checkpoint { .. } => "checkpoint",
    }
}

/// `latestStableRun`: the newest completed run that has a checkpoint.
fn latest_stable_run(projection: &OrchestrationV2ThreadProjection) -> Option<&OrchestrationV2Run> {
    projection
        .runs
        .iter()
        .filter(|run| {
            run.status == OrchestrationV2RunStatus::Completed && run.checkpoint_id.is_some()
        })
        .max_by_key(|run| run.ordinal.0)
}

/// `runForSourcePoint`: the run a fork or merge-back copies from, if any.
pub(super) fn run_for_source_point<'a>(
    projection: &'a OrchestrationV2ThreadProjection,
    source_point: &OrchestrationV2ThreadForkSourcePoint,
) -> Option<&'a OrchestrationV2Run> {
    match source_point {
        OrchestrationV2ThreadForkSourcePoint::LatestStable => latest_stable_run(projection),
        OrchestrationV2ThreadForkSourcePoint::Run { run_id } => {
            projection.runs.iter().find(|run| &run.id == run_id)
        }
        OrchestrationV2ThreadForkSourcePoint::Checkpoint { checkpoint_id } => {
            let checkpoint = projection
                .checkpoints
                .iter()
                .find(|candidate| &candidate.id == checkpoint_id)?;
            let run_id = checkpoint.run_id.as_ref()?;
            projection.runs.iter().find(|run| &run.id == run_id)
        }
    }
}

/// `providerThreadForRun`.
pub(super) fn provider_thread_for_run<'a>(
    projection: &'a OrchestrationV2ThreadProjection,
    run: &OrchestrationV2Run,
) -> Option<&'a OrchestrationV2ProviderThread> {
    let provider_thread_id = run.provider_thread_id.as_ref()?;
    projection
        .provider_threads
        .iter()
        .find(|candidate| &candidate.id == provider_thread_id)
}

/// The id of `providerTurnForRun(projection, run)`, if there is one.
pub(super) fn latest_turn_for_run(
    projection: &OrchestrationV2ThreadProjection,
    run: &OrchestrationV2Run,
) -> Option<turin_contracts::ProviderTurnId> {
    provider_turn_for_run(projection, run).map(|turn| turn.id.clone())
}

/// `providerTurnForRun`: the latest turn of the run's active attempt, or the
/// turn the attempt itself names.
pub(super) fn provider_turn_for_run<'a>(
    projection: &'a OrchestrationV2ThreadProjection,
    run: &OrchestrationV2Run,
) -> Option<&'a OrchestrationV2ProviderTurn> {
    let attempt_id = run.active_attempt_id.as_ref()?;
    latest_provider_turn_for_attempt(&projection.provider_turns, Some(attempt_id)).or_else(|| {
        let attempt = projection
            .attempts
            .iter()
            .find(|candidate| &candidate.id == attempt_id)?;
        let turn_id = attempt.provider_turn_id.as_ref()?;
        projection
            .provider_turns
            .iter()
            .find(|turn| &turn.id == turn_id)
    })
}

/// `contextSourcePointForRun`: where a transfer reads its conversation from.
pub(super) fn context_source_point_for_run(
    projection: &OrchestrationV2ThreadProjection,
    run: &OrchestrationV2Run,
) -> OrchestrationV2ContextSourcePoint {
    OrchestrationV2ContextSourcePoint {
        thread_id: projection.thread.id.clone(),
        run_id: Some(run.id.clone()),
        checkpoint_id: run.checkpoint_id.clone(),
        turn_item_id: None,
        provider_thread_ref: provider_thread_for_run(projection, run)
            .and_then(|thread| thread.native_thread_ref.clone()),
        provider_turn_ref: provider_turn_for_run(projection, run)
            .and_then(|turn| turn.native_turn_ref.clone()),
    }
}

#[cfg(test)]
mod tests;
