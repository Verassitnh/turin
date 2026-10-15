//! Port of upstream/apps/server/src/orchestration-v2/Orchestrator.ts:
//! `dispatchThreadCreate` and `dispatchThreadVisit`.
//!
//! Not ported yet: `thread.create` with `importedNativeThread` (it also writes
//! a `provider-thread.updated`), and the rest of `dispatchThreadMutation`
//! (archive, settle, pin, metadata, pull requests, mode and model updates).
//! Those kinds are left to other groups, so `decide` returns `false` for them.

use turin_contracts::{
    UtcDateTime,
    orchestration_v2::{
        OrchestrationV2AppThread, OrchestrationV2AppThreadLineage, OrchestrationV2Command,
        OrchestrationV2DomainEvent, OrchestrationV2ServerCommand,
    },
};

use super::Orchestrator;
use super::dispatch::{Decision, dispatch_failure};
use crate::pipeline::orchestrator_port::OrchestratorError;

/// Decides the thread-lifecycle kinds ported here. `Ok(false)` leaves the
/// command to another group.
pub(super) async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    if thread_create(command, decision) {
        return Ok(true);
    }
    thread_visit(orchestrator, command, decision).await
}

/// `dispatchThreadCreate`: the new thread row, with every optional field
/// absent except the nulls upstream writes explicitly.
fn thread_create(command: &OrchestrationV2ServerCommand, decision: &mut Decision) -> bool {
    let OrchestrationV2ServerCommand::Client(OrchestrationV2Command::ThreadCreate {
        created_by,
        creation_source,
        thread_id,
        project_id,
        title,
        model_selection,
        runtime_mode,
        interaction_mode,
        branch,
        worktree_path,
        imported_native_thread,
        ..
    }) = command
    else {
        return false;
    };
    if imported_native_thread.is_some() {
        return false;
    }

    let now = decision.now();
    let thread = OrchestrationV2AppThread {
        created_by: *created_by,
        creation_source: *creation_source,
        id: thread_id.clone(),
        project_id: project_id.clone(),
        title: title.clone(),
        provider_instance_id: model_selection.instance_id.clone(),
        model_selection: model_selection.clone(),
        runtime_mode: *runtime_mode,
        interaction_mode: *interaction_mode,
        branch: branch.clone(),
        worktree_path: worktree_path.clone(),
        linked_pull_request: None,
        pull_requests: None,
        branch_pull_request: None,
        active_provider_thread_id: None,
        history_origin: None,
        lineage: OrchestrationV2AppThreadLineage {
            parent_thread_id: None,
            relationship_to_parent: None,
            root_thread_id: thread_id.clone(),
        },
        forked_from: None,
        created_at: now,
        updated_at: now,
        archived_at: None,
        settled_override: None,
        settled_at: None,
        unsettled_at: None,
        snoozed_until: Some(None),
        snoozed_at: Some(None),
        last_snooze_wake_at: None,
        limit_recovery: None,
        pinned_at: None,
        auto_settle_disabled_at: None,
        pin_order_key: None,
        active_order_key: None,
        last_visited_at: None,
        title_regeneration: None,
        rollback_request_id: None,
        rollback_failure: None,
        deleted_at: None,
    };
    let event_id = decision.event_id(thread_id);
    decision.emit(OrchestrationV2DomainEvent::ThreadCreated {
        id: event_id,
        thread_id: thread_id.clone(),
        run_id: None,
        node_id: None,
        driver: None,
        provider_instance_id: Some(model_selection.instance_id.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: thread,
    });
    true
}

/// `dispatchThreadVisit`: read state only. A visit moves `lastVisitedAt`
/// forward and never backward, and it does not bump `updatedAt`, so a routine
/// read does not scale with the thread's history.
async fn thread_visit(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    let OrchestrationV2ServerCommand::Client(OrchestrationV2Command::ThreadVisit {
        thread_id,
        visited_at,
        ..
    }) = command
    else {
        return Ok(false);
    };

    let thread = orchestrator
        .projection
        .get_thread(thread_id)
        .await
        .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;
    if thread.deleted_at.is_some() {
        return Err(dispatch_failure(
            command,
            format!("Thread {thread_id} is deleted."),
        ));
    }
    let visited_at = visited_at.parse::<UtcDateTime>().map_err(|_| {
        dispatch_failure(
            command,
            format!("Thread {thread_id} visit time {visited_at} is not a valid timestamp."),
        )
    })?;

    let moves_forward = thread
        .last_visited_at
        .is_none_or(|last_visited_at| visited_at > last_visited_at);
    let provider_instance_id = thread.provider_instance_id.clone();
    let mut payload = thread;
    if moves_forward {
        payload.last_visited_at = Some(visited_at);
    }
    let event_id = decision.event_id(thread_id);
    decision.emit(OrchestrationV2DomainEvent::ThreadVisited {
        id: event_id,
        thread_id: thread_id.clone(),
        run_id: None,
        node_id: None,
        driver: None,
        provider_instance_id: Some(provider_instance_id),
        raw_event_id: None,
        occurred_at: decision.now(),
        payload,
    });
    Ok(true)
}
