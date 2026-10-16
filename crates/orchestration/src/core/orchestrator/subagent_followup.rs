//! Port of `recordSubagentFollowup` (upstream `Orchestrator.ts` 6879). A
//! follow-up to a delegated child that has already published a result is new
//! requested work: it gets its own task on the parent so results,
//! acknowledgments, Stop and recovery use the same lifecycle as a fresh
//! delegation, without replacing the original result.

use serde_json::json;
use turin_contracts::orchestration_v2::{
    OrchestrationV2Actor, OrchestrationV2Command as C, OrchestrationV2CompletionWake,
    OrchestrationV2CreationSource, OrchestrationV2DelegatedCompletionTaskDelivery,
    OrchestrationV2DelegatedCompletionTaskDeliveryState as State, OrchestrationV2DomainEvent,
    OrchestrationV2ExecutionNode, OrchestrationV2ExecutionNodeKind,
    OrchestrationV2ExecutionNodeStatus, OrchestrationV2ForkSource,
    OrchestrationV2LineageRelationship, OrchestrationV2Run, OrchestrationV2RunStatus,
    OrchestrationV2ServerCommand, OrchestrationV2Subagent, OrchestrationV2SubagentOrigin,
    OrchestrationV2SubagentStatus, OrchestrationV2TurnItem,
};
use turin_provider_core::id_allocator::derive as ids;
use turin_shared::orchestration_v2_thread_error::latest_executed_run;

use super::Orchestrator;
use super::dispatch::{Decision, command_id, dispatch_failure};
use super::message_dispatch::{is_blocking_run, next_turn_item_ordinal};
use super::thread_fork::latest_turn_for_run;
use crate::pipeline::orchestrator_port::{
    OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
};

/// Records the follow-up task for a `message.dispatch` this decision just made.
pub(super) async fn record(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    client: &C,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    let C::MessageDispatch {
        created_by,
        creation_source,
        thread_id,
        message_id,
        text,
        sender_thread_id,
        scheduled_task_id,
        notification,
        restart_continuation_of_run_id,
        ..
    } = client
    else {
        return Ok(());
    };
    if scheduled_task_id.is_some()
        || notification.is_some()
        || restart_continuation_of_run_id.is_some()
        || (*created_by != OrchestrationV2Actor::User
            && *creation_source != OrchestrationV2CreationSource::Mcp)
    {
        return Ok(());
    }
    let message = decision.events.iter().rev().find_map(|event| match event {
        OrchestrationV2DomainEvent::MessageUpdated { payload, .. } if &payload.id == message_id => {
            Some(payload.clone())
        }
        _ => None,
    });
    let Some(message) = message else {
        return Ok(());
    };
    let Some(message_run_id) = message.run_id.clone() else {
        return Ok(());
    };
    let child = orchestrator
        .projection
        .get_thread(thread_id)
        .await
        .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;
    let Some(parent_thread_id) = child.lineage.parent_thread_id.clone() else {
        return Ok(());
    };
    if child.lineage.relationship_to_parent != Some(OrchestrationV2LineageRelationship::Subagent)
        || (*created_by != OrchestrationV2Actor::User
            && sender_thread_id.as_ref() != Some(&parent_thread_id))
    {
        return Ok(());
    }
    let run = match decision.events.iter().rev().find_map(|event| match event {
        OrchestrationV2DomainEvent::RunCreated { payload, .. }
        | OrchestrationV2DomainEvent::RunUpdated { payload, .. }
            if payload.id == message_run_id =>
        {
            Some(payload.clone())
        }
        _ => None,
    }) {
        Some(run) => run,
        None => {
            let records = orchestrator
                .get_thread_records(thread_id, &[RecordField::Runs], &RecordFilter::default())
                .await?;
            match records
                .runs
                .into_iter()
                .find(|row| row.id == message_run_id)
            {
                Some(run) => run,
                None => return Ok(()),
            }
        }
    };
    if run.delegated_task_id.is_some() {
        return Ok(());
    }
    let parent = orchestrator
        .get_thread_records(
            &parent_thread_id,
            &[
                RecordField::Runs,
                RecordField::Subagents,
                RecordField::ProviderTurns,
                RecordField::Attempts,
            ],
            &RecordFilter::default(),
        )
        .await?;
    let child_task_node = match &child.forked_from {
        Some(OrchestrationV2ForkSource::Node { node_id }) => Some(node_id),
        _ => None,
    };
    let original = parent.subagents.iter().find(|task| {
        task.origin == OrchestrationV2SubagentOrigin::AppOwned
            && task.child_thread_id.as_ref() == Some(&child.id)
            && Some(&task.id) == child_task_node
    });
    // Before first publication, follow-ups remain part of the original task.
    let Some(original) = original.filter(|task| task.result.is_some()) else {
        return Ok(());
    };
    let mut blocking: Vec<&OrchestrationV2Run> = parent
        .runs
        .iter()
        .filter(|run| is_blocking_run(run))
        .collect();
    blocking.sort_by_key(|run| std::cmp::Reverse(run.ordinal));
    let Some(owner) = blocking
        .first()
        .copied()
        .or_else(|| latest_executed_run(&parent.runs))
    else {
        return Ok(());
    };
    let Some(owner_root) = owner.root_node_id.clone() else {
        return Ok(());
    };
    let now = message.created_at;
    let adapter = orchestrator
        .seams
        .adapters
        .adapter(&run.provider_instance_id)
        .ok_or_else(|| {
            dispatch_failure(
                command,
                format!(
                    "Provider instance {} is not available.",
                    run.provider_instance_id
                ),
            )
        })?;
    let this_command_id = command_id(command);
    let task_id = ids::delegated_task_node(this_command_id);
    let (task_status, node_status) = if run.status == OrchestrationV2RunStatus::Queued {
        (
            OrchestrationV2SubagentStatus::Pending,
            OrchestrationV2ExecutionNodeStatus::Pending,
        )
    } else {
        (
            OrchestrationV2SubagentStatus::Running,
            OrchestrationV2ExecutionNodeStatus::Running,
        )
    };
    let task = OrchestrationV2Subagent {
        created_by: *created_by,
        driver: adapter.driver().clone(),
        id: task_id.clone(),
        run_id: Some(owner.id.clone()),
        parent_node_id: owner_root.clone(),
        provider_instance_id: run.provider_instance_id.clone(),
        provider_thread_id: run.provider_thread_id.clone(),
        model: Some(run.model_selection.model.to_string()),
        model_selection: Some(run.model_selection.clone()),
        prompt: text.to_string(),
        title: Some(child.title.to_string()),
        completion_wake: Some(OrchestrationV2CompletionWake::Always),
        completion_delivery: Some(OrchestrationV2DelegatedCompletionTaskDelivery {
            state: State::Pending,
            observed_by_run_id: None,
        }),
        status: task_status,
        progress: None,
        result: None,
        started_at: Some(now),
        completed_at: None,
        updated_at: now,
        ..original.clone()
    };
    let node = OrchestrationV2ExecutionNode {
        id: task_id.clone(),
        thread_id: parent_thread_id.clone(),
        run_id: Some(owner.id.clone()),
        parent_node_id: Some(owner_root.clone()),
        root_node_id: owner_root.clone(),
        kind: OrchestrationV2ExecutionNodeKind::Subagent,
        status: node_status,
        counts_for_run: false,
        provider_thread_id: run.provider_thread_id.clone(),
        provider_turn_id: None,
        native_item_ref: None,
        runtime_request_id: None,
        checkpoint_scope_id: None,
        started_at: Some(now),
        completed_at: None,
    };
    let ordinal = next_turn_item_ordinal(orchestrator, &parent).await?;
    let turn_item: OrchestrationV2TurnItem = serde_json::from_value(json!({
        "type": "subagent",
        "id": ids::delegated_task_turn_item(this_command_id),
        "threadId": parent_thread_id,
        "runId": owner.id,
        "nodeId": task_id,
        "providerThreadId": owner.provider_thread_id,
        "providerTurnId": latest_turn_for_run(&parent, owner),
        "nativeItemRef": null,
        "parentItemId": null,
        "ordinal": ordinal,
        "status": task_status,
        "title": child.title,
        "startedAt": now,
        "completedAt": null,
        "updatedAt": now,
        "subagentId": task_id,
        "origin": "app_owned",
        "driver": task.driver,
        "providerInstanceId": task.provider_instance_id,
        "childThreadId": child.id,
        "prompt": text,
        "result": null,
    }))
    .map_err(|cause| dispatch_failure(command, cause.to_string()))?;

    let id = decision.event_id(thread_id);
    decision.emit(OrchestrationV2DomainEvent::RunUpdated {
        id,
        thread_id: thread_id.clone(),
        run_id: Some(run.id.clone()),
        node_id: None,
        driver: None,
        provider_instance_id: None,
        raw_event_id: None,
        occurred_at: now,
        payload: OrchestrationV2Run {
            delegated_task_id: Some(task_id.clone()),
            ..run
        },
    });
    let id = decision.event_id(&parent_thread_id);
    decision.emit(OrchestrationV2DomainEvent::NodeUpdated {
        id,
        thread_id: parent_thread_id.clone(),
        run_id: Some(owner.id.clone()),
        node_id: Some(task_id.clone()),
        driver: None,
        provider_instance_id: None,
        raw_event_id: None,
        occurred_at: now,
        payload: node,
    });
    let id = decision.event_id(&parent_thread_id);
    decision.emit(OrchestrationV2DomainEvent::SubagentUpdated {
        id,
        thread_id: parent_thread_id.clone(),
        run_id: Some(owner.id.clone()),
        node_id: Some(task_id.clone()),
        driver: None,
        provider_instance_id: None,
        raw_event_id: None,
        occurred_at: now,
        payload: task,
    });
    let id = decision.event_id(&parent_thread_id);
    decision.emit(OrchestrationV2DomainEvent::TurnItemUpdated {
        id,
        thread_id: parent_thread_id,
        run_id: Some(owner.id.clone()),
        node_id: Some(task_id),
        driver: None,
        provider_instance_id: None,
        raw_event_id: None,
        occurred_at: now,
        payload: turn_item,
    });
    Ok(())
}
