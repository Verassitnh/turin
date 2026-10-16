//! Port of upstream `Orchestrator.ts` prepared-run commands
//! (`dispatchPreparedRunProgress`, `Release`, `Fail`, `Retry`, 8040-8420).
//!
//! A run started with `defer_start` waits in `preparing` while its workspace
//! (a worktree and its setup script) is made. The launch service reports
//! progress, then releases the run to start, fails it, or retries it.

use serde_json::{Value, json};
use turin_contracts::{
    ProviderThreadId, RunId, ThreadId,
    orchestration_v2::{
        ORCHESTRATION_V2_WORKSPACE_PREPARATION_FAILURE_CODE, OrchestrationV2Command as C,
        OrchestrationV2ExecutionNode, OrchestrationV2ExecutionNodeStatus,
        OrchestrationV2PreparedRunPhase, OrchestrationV2ProviderFailure,
        OrchestrationV2ProviderFailureCode, OrchestrationV2ProviderThread, OrchestrationV2Run,
        OrchestrationV2RunAttempt, OrchestrationV2RunAttemptStatus, OrchestrationV2RunStatus,
        OrchestrationV2ServerCommand, OrchestrationV2ThreadProjection, OrchestrationV2TurnItem,
        OrchestrationV2TurnItemStatus,
    },
};
use turin_provider_core::id_allocator::derive as ids;

use super::Orchestrator;
use super::cohort::patch_turn_item;
use super::dispatch::{Decision, command_id, dispatch_failure};
use super::message_dispatch::{
    WORKSPACE_PREPARATION_INPUT, is_blocking_run, next_turn_item_ordinal, prepare_root_scope,
    run_scoped_event,
};
use crate::pipeline::effect_outbox::{EffectRequest, PendingEffect};
use crate::pipeline::orchestrator_port::{
    OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
};
use turin_shared::orchestration_v2_thread_error::latest_executed_run;

/// Decides the four prepared-run commands. `Ok(false)` leaves the command to another group.
pub(super) async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    let OrchestrationV2ServerCommand::Client(client) = command else {
        return Ok(false);
    };
    match client {
        C::PreparedRunProgress {
            thread_id,
            run_id,
            phase,
            ..
        } => progress(orchestrator, command, decision, thread_id, run_id, *phase).await?,
        C::PreparedRunRelease {
            thread_id, run_id, ..
        } => release(orchestrator, command, decision, thread_id, run_id).await?,
        C::PreparedRunFail {
            thread_id,
            run_id,
            failure,
            ..
        } => fail(orchestrator, command, decision, thread_id, run_id, failure).await?,
        C::PreparedRunRetry {
            thread_id, run_id, ..
        } => retry(orchestrator, command, decision, thread_id, run_id).await?,
        _ => return Ok(false),
    }
    Ok(true)
}

/// `preparedRunState`: the run and the records a preparation step touches.
struct PreparedState<'a> {
    run: &'a OrchestrationV2Run,
    attempt: &'a OrchestrationV2RunAttempt,
    root_node: &'a OrchestrationV2ExecutionNode,
    provider_thread: &'a OrchestrationV2ProviderThread,
    preparation_item: &'a OrchestrationV2TurnItem,
}

fn prepared_state<'a>(
    projection: &'a OrchestrationV2ThreadProjection,
    run_id: &RunId,
    expected: OrchestrationV2RunStatus,
) -> Option<PreparedState<'a>> {
    let run = projection
        .runs
        .iter()
        .find(|candidate| &candidate.id == run_id)?;
    let attempt = run.active_attempt_id.as_ref().and_then(|id| {
        projection
            .attempts
            .iter()
            .find(|candidate| &candidate.id == id)
    })?;
    let root_node = run.root_node_id.as_ref().and_then(|id| {
        projection
            .nodes
            .iter()
            .find(|candidate| &candidate.id == id)
    })?;
    let provider_thread = run
        .provider_thread_id
        .as_ref()
        .and_then(|id: &ProviderThreadId| {
            projection
                .provider_threads
                .iter()
                .find(|candidate| &candidate.id == id)
        })?;
    let preparation_item = projection.turn_items.iter().find(|candidate| {
        matches!(
            candidate,
            OrchestrationV2TurnItem::CommandExecution { run_id: item_run, input, .. }
                if item_run.as_ref() == Some(run_id) && input == WORKSPACE_PREPARATION_INPUT
        )
    })?;
    (run.status == expected).then_some(PreparedState {
        run,
        attempt,
        root_node,
        provider_thread,
        preparation_item,
    })
}

async fn load(
    orchestrator: &Orchestrator,
    thread_id: &ThreadId,
    run_id: &RunId,
    types: &[&str],
) -> Result<OrchestrationV2ThreadProjection, OrchestratorError> {
    orchestrator
        .get_thread_records(
            thread_id,
            &[
                RecordField::Runs,
                RecordField::Attempts,
                RecordField::Nodes,
                RecordField::ProviderThreads,
                RecordField::TurnItems,
            ],
            &RecordFilter {
                turn_item_types: Some(types.iter().map(|kind| (*kind).to_owned()).collect()),
                turn_item_run_ids: Some(vec![Some(run_id.clone())]),
                ..RecordFilter::default()
            },
        )
        .await
}

fn not_awaiting(command: &OrchestrationV2ServerCommand, run_id: &RunId) -> OrchestratorError {
    dispatch_failure(
        command,
        format!("Run {run_id} is not awaiting workspace preparation."),
    )
}

async fn progress(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    thread_id: &ThreadId,
    run_id: &RunId,
    phase: OrchestrationV2PreparedRunPhase,
) -> Result<(), OrchestratorError> {
    let projection = load(orchestrator, thread_id, run_id, &["command_execution"]).await?;
    let Some(state) = prepared_state(&projection, run_id, OrchestrationV2RunStatus::Preparing)
    else {
        return Err(not_awaiting(command, run_id));
    };
    let now = decision.now();
    let item = patch_turn_item(
        state.preparation_item,
        vec![
            (
                "title",
                json!(match phase {
                    OrchestrationV2PreparedRunPhase::Worktree => "Preparing worktree",
                    OrchestrationV2PreparedRunPhase::Setup => "Starting setup script",
                }),
            ),
            ("updatedAt", json!(now)),
        ],
    );
    run_scoped_event!(
        decision,
        TurnItemUpdated,
        thread_id,
        &state.run.id,
        &state.root_node.id,
        &state.run.provider_instance_id,
        item
    );
    Ok(())
}

async fn release(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    thread_id: &ThreadId,
    run_id: &RunId,
) -> Result<(), OrchestratorError> {
    let projection = load(orchestrator, thread_id, run_id, &["command_execution"]).await?;
    let Some(state) = prepared_state(&projection, run_id, OrchestrationV2RunStatus::Preparing)
    else {
        return Err(not_awaiting(command, run_id));
    };
    let now = decision.now();
    let scope = prepare_root_scope(
        orchestrator,
        command,
        &projection,
        &state.run.model_selection,
        None,
        &state.run.id,
        &state.root_node.id,
        &state.provider_thread.id,
        now,
    )
    .await?;
    let instance = &state.run.provider_instance_id;
    run_scoped_event!(
        decision,
        CheckpointScopeCreated,
        thread_id,
        &state.run.id,
        &state.root_node.id,
        instance,
        scope.clone()
    );
    run_scoped_event!(
        decision,
        NodeUpdated,
        thread_id,
        &state.run.id,
        &state.root_node.id,
        instance,
        OrchestrationV2ExecutionNode {
            checkpoint_scope_id: Some(scope.id.clone()),
            ..state.root_node.clone()
        }
    );
    let item = patch_turn_item(
        state.preparation_item,
        vec![
            ("status", json!("completed")),
            ("title", json!("Workspace ready")),
            ("output", json!("Workspace preparation completed.")),
            ("exitCode", json!(0)),
            ("completedAt", json!(now)),
            ("updatedAt", json!(now)),
        ],
    );
    run_scoped_event!(
        decision,
        TurnItemUpdated,
        thread_id,
        &state.run.id,
        &state.root_node.id,
        instance,
        item
    );
    run_scoped_event!(
        decision,
        RunUpdated,
        thread_id,
        &state.run.id,
        &state.root_node.id,
        instance,
        OrchestrationV2Run {
            status: OrchestrationV2RunStatus::Starting,
            ..state.run.clone()
        }
    );
    decision.effects.push(PendingEffect {
        id: format!(
            "effect:{}:provider-turn.start:{}",
            command_id(command),
            state.run.id
        ),
        command_id: command_id(command).clone(),
        thread_id: thread_id.clone(),
        request: EffectRequest::ProviderTurnStart {
            run_id: state.run.id.clone(),
        },
        available_at: None,
    });
    Ok(())
}

async fn fail(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    thread_id: &ThreadId,
    run_id: &RunId,
    failure: &OrchestrationV2ProviderFailure,
) -> Result<(), OrchestratorError> {
    let projection = load(orchestrator, thread_id, run_id, &["command_execution"]).await?;
    let Some(state) = prepared_state(&projection, run_id, OrchestrationV2RunStatus::Preparing)
    else {
        return Err(not_awaiting(command, run_id));
    };
    let now = decision.now();
    let instance = &state.run.provider_instance_id;
    run_scoped_event!(
        decision,
        RunAttemptUpdated,
        thread_id,
        &state.run.id,
        &state.root_node.id,
        instance,
        OrchestrationV2RunAttempt {
            status: OrchestrationV2RunAttemptStatus::Failed,
            completed_at: Some(now),
            ..state.attempt.clone()
        }
    );
    run_scoped_event!(
        decision,
        NodeUpdated,
        thread_id,
        &state.run.id,
        &state.root_node.id,
        instance,
        OrchestrationV2ExecutionNode {
            status: OrchestrationV2ExecutionNodeStatus::Failed,
            completed_at: Some(now),
            ..state.root_node.clone()
        }
    );
    let item = patch_turn_item(
        state.preparation_item,
        vec![
            ("status", json!("failed")),
            ("title", json!("Workspace preparation failed")),
            ("output", json!(failure.message)),
            ("exitCode", json!(1)),
            ("completedAt", json!(now)),
            ("updatedAt", json!(now)),
        ],
    );
    run_scoped_event!(
        decision,
        TurnItemUpdated,
        thread_id,
        &state.run.id,
        &state.root_node.id,
        instance,
        item
    );
    let ordinal = next_turn_item_ordinal(orchestrator, &projection).await?;
    let error_item = OrchestrationV2TurnItem::Error {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: ids::turn_item_from_provider_item(
            &state.provider_thread.driver,
            &format!("workspace-preparation-failure:{}", state.run.id),
        ),
        thread_id: thread_id.clone(),
        run_id: Some(state.run.id.clone()),
        node_id: Some(state.root_node.id.clone()),
        provider_thread_id: Some(state.provider_thread.id.clone()),
        provider_turn_id: None,
        native_item_ref: None,
        parent_item_id: None,
        ordinal,
        status: OrchestrationV2TurnItemStatus::Failed,
        title: Some("Workspace preparation failed".to_owned()),
        started_at: Some(now),
        completed_at: Some(now),
        updated_at: now,
        failure: OrchestrationV2ProviderFailure {
            code: Some(
                OrchestrationV2ProviderFailureCode::new(
                    ORCHESTRATION_V2_WORKSPACE_PREPARATION_FAILURE_CODE,
                )
                .expect("the failure code is valid"),
            ),
            ..failure.clone()
        },
        retry: None,
    };
    run_scoped_event!(
        decision,
        TurnItemUpdated,
        thread_id,
        &state.run.id,
        &state.root_node.id,
        instance,
        error_item
    );
    run_scoped_event!(
        decision,
        RunUpdated,
        thread_id,
        &state.run.id,
        &state.root_node.id,
        instance,
        OrchestrationV2Run {
            status: OrchestrationV2RunStatus::Failed,
            completed_at: Some(now),
            ..state.run.clone()
        }
    );
    Ok(())
}

/// Returns a run whose workspace preparation failed to preparing. The failure
/// item turns cancelled so clients stop offering the retry; the launch service
/// runs the recorded preparation again once this commits.
async fn retry(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    thread_id: &ThreadId,
    run_id: &RunId,
) -> Result<(), OrchestratorError> {
    let projection = load(
        orchestrator,
        thread_id,
        run_id,
        &["command_execution", "error"],
    )
    .await?;
    let state = prepared_state(&projection, run_id, OrchestrationV2RunStatus::Failed);
    let failure_item = projection.turn_items.iter().find(|candidate| {
        matches!(
            candidate,
            OrchestrationV2TurnItem::Error { status, failure, .. }
                if *status == OrchestrationV2TurnItemStatus::Failed
                    && failure.code.as_ref().map(OrchestrationV2ProviderFailureCode::as_str)
                        == Some(ORCHESTRATION_V2_WORKSPACE_PREPARATION_FAILURE_CODE)
        )
    });
    let (Some(state), Some(failure_item)) = (state, failure_item) else {
        return Err(no_retry(command, run_id));
    };
    if state.run.workspace_preparation.is_none()
        || projection.thread.archived_at.is_some()
        || projection.thread.deleted_at.is_some()
    {
        return Err(no_retry(command, run_id));
    }
    if projection
        .runs
        .iter()
        .any(|run| run.id != state.run.id && is_blocking_run(run))
    {
        return Err(dispatch_failure(
            command,
            "Another run is active on this thread.",
        ));
    }
    let now = decision.now();
    let instance = &state.run.provider_instance_id;
    run_scoped_event!(
        decision,
        TurnItemUpdated,
        thread_id,
        &state.run.id,
        &state.root_node.id,
        instance,
        patch_turn_item(
            failure_item,
            vec![("status", json!("cancelled")), ("updatedAt", json!(now))]
        )
    );
    // `withoutPreparationResult`: the result fields of the last attempt go.
    run_scoped_event!(
        decision,
        TurnItemUpdated,
        thread_id,
        &state.run.id,
        &state.root_node.id,
        instance,
        patch_turn_item(
            state.preparation_item,
            vec![
                ("output", Value::Null),
                ("exitCode", Value::Null),
                ("outputIndicatesFailure", Value::Null),
                ("status", json!("running")),
                ("title", json!(WORKSPACE_PREPARATION_INPUT)),
                ("completedAt", Value::Null),
                ("updatedAt", json!(now)),
            ]
        )
    );
    run_scoped_event!(
        decision,
        RunAttemptUpdated,
        thread_id,
        &state.run.id,
        &state.root_node.id,
        instance,
        OrchestrationV2RunAttempt {
            status: OrchestrationV2RunAttemptStatus::Pending,
            completed_at: None,
            ..state.attempt.clone()
        }
    );
    run_scoped_event!(
        decision,
        NodeUpdated,
        thread_id,
        &state.run.id,
        &state.root_node.id,
        instance,
        OrchestrationV2ExecutionNode {
            status: OrchestrationV2ExecutionNodeStatus::Pending,
            completed_at: None,
            ..state.root_node.clone()
        }
    );
    run_scoped_event!(
        decision,
        RunUpdated,
        thread_id,
        &state.run.id,
        &state.root_node.id,
        instance,
        OrchestrationV2Run {
            status: OrchestrationV2RunStatus::Preparing,
            completed_at: None,
            ..state.run.clone()
        }
    );
    // Messages held when this preparation failed follow the run again. A hold
    // from a later run (a stop, say) is not this failure's to release.
    let failure_owns_queue =
        latest_executed_run(&projection.runs).is_some_and(|run| run.id == state.run.id);
    for run in &projection.runs {
        if !failure_owns_queue
            || run.status != OrchestrationV2RunStatus::Queued
            || run.queue_held != Some(true)
        {
            continue;
        }
        let id = decision.event_id(thread_id);
        decision.emit(
            turin_contracts::orchestration_v2::OrchestrationV2DomainEvent::RunUpdated {
                id,
                thread_id: thread_id.clone(),
                run_id: Some(run.id.clone()),
                node_id: None,
                driver: None,
                provider_instance_id: Some(run.provider_instance_id.clone()),
                raw_event_id: None,
                occurred_at: now,
                payload: OrchestrationV2Run {
                    queue_held: Some(false),
                    ..run.clone()
                },
            },
        );
    }
    Ok(())
}

fn no_retry(command: &OrchestrationV2ServerCommand, run_id: &RunId) -> OrchestratorError {
    dispatch_failure(
        command,
        format!("Run {run_id} has no failed workspace preparation to retry."),
    )
}
