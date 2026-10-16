//! Port of `dispatchDelegatedTaskRequest` (upstream `Orchestrator.ts` 6602):
//! a running parent run hands work to an app-owned child thread. The decision
//! creates the child thread, the task row, node and timeline item on the
//! parent, dispatches the task as the child's first message, and records the
//! spawn transfer that carries the parent's context to the child.

use serde_json::json;
use turin_contracts::{
    TrimmedNonEmptyString,
    orchestration_v2::{
        OrchestrationV2Command as C, OrchestrationV2ContextTransfer,
        OrchestrationV2ContextTransferStatus, OrchestrationV2ContextTransferType,
        OrchestrationV2DomainEvent, OrchestrationV2ExecutionNode, OrchestrationV2ExecutionNodeKind,
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2ServerCommand, OrchestrationV2Subagent,
        OrchestrationV2SubagentOrigin, OrchestrationV2SubagentStatus, OrchestrationV2TurnItem,
    },
};
use turin_provider_core::{
    id_allocator::{IdAllocator, derive as ids},
    subagent_projection::{
        SubagentChildThreadInput, make_subagent_child_thread, subagent_thread_title,
    },
};

use super::Orchestrator;
use super::dispatch::{Decision, command_id, dispatch_failure};
use super::message_dispatch::{dispatch_message, is_blocking_run, next_turn_item_ordinal};
use super::run_stop::stop_reached_run;
use super::thread_fork::{context_source_point_for_run, latest_turn_for_run};
use crate::pipeline::orchestrator_port::{
    OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
};

/// Decides `delegated_task.request`; `Ok(false)` leaves other kinds alone.
pub(super) async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    let OrchestrationV2ServerCommand::Client(
        client @ C::DelegatedTaskRequest {
            created_by,
            creation_source,
            parent_thread_id,
            parent_run_id,
            parent_node_id,
            task: task_text,
            title,
            model_selection,
            runtime_mode,
            interaction_mode,
            workspace_strategy,
            completion_wake,
            created_at,
            ..
        },
    ) = command
    else {
        return Ok(false);
    };
    let _ = client;
    let parent = orchestrator
        .get_thread_records(
            parent_thread_id,
            &[
                RecordField::Runs,
                RecordField::Nodes,
                RecordField::Subagents,
                RecordField::ProviderThreads,
                RecordField::ProviderTurns,
                RecordField::Attempts,
            ],
            &RecordFilter::default(),
        )
        .await?;
    let parent_run = parent.runs.iter().find(|run| &run.id == parent_run_id);
    let Some(parent_run) = parent_run.filter(|run| is_blocking_run(run)) else {
        return Err(dispatch_failure(
            command,
            format!("Parent run {parent_run_id} is not active."),
        ));
    };
    if stop_reached_run(orchestrator, command, parent_thread_id, &parent_run.id).await? {
        return Err(dispatch_failure(
            command,
            format!("Parent run {parent_run_id} is stopping."),
        ));
    }
    let parent_node = parent.nodes.iter().find(|node| &node.id == parent_node_id);
    let Some(root_node_id) = parent_run
        .root_node_id
        .clone()
        .filter(|_| parent_node.is_some_and(|node| node.run_id.as_ref() == Some(&parent_run.id)))
    else {
        return Err(dispatch_failure(
            command,
            format!(
                "Parent node {parent_node_id} is not part of active run {}.",
                parent_run.id
            ),
        ));
    };

    let instance = &model_selection.instance_id;
    let adapter = orchestrator
        .seams
        .adapters
        .adapter(instance)
        .ok_or_else(|| {
            dispatch_failure(
                command,
                format!("Provider instance {instance} is not available."),
            )
        })?;
    let driver = adapter.driver();

    let now = created_at.unwrap_or_else(|| decision.now());
    let this_command_id = command_id(command);
    let task_node_id = ids::delegated_task_node(this_command_id);
    let child_thread_id = ids::delegated_task_thread(this_command_id);
    let child_message_id = ids::delegated_task_message(this_command_id);
    let task_turn_item_id = ids::delegated_task_turn_item(this_command_id);
    let task_title = subagent_thread_title(
        parent.thread.title.as_str(),
        title.as_ref().map(TrimmedNonEmptyString::as_str),
        task_text.as_str(),
        parent.subagents.len() as u64 + 1,
    );
    let mut child_thread = make_subagent_child_thread(SubagentChildThreadInput {
        parent_thread: &parent.thread,
        child_thread_id: child_thread_id.clone(),
        parent_node_id: task_node_id.clone(),
        active_provider_thread_id: None,
        provider_instance_id: instance.clone(),
        model_selection: model_selection.clone(),
        title: TrimmedNonEmptyString::new(task_title.clone())
            .map_err(|cause| dispatch_failure(command, cause.to_string()))?,
        now,
        created_by: *created_by,
        creation_source: *creation_source,
    });
    child_thread.runtime_mode = *runtime_mode;
    child_thread.interaction_mode = *interaction_mode;
    if workspace_strategy.is_some() {
        child_thread.branch = None;
        child_thread.worktree_path = None;
    }
    let task = OrchestrationV2Subagent {
        id: task_node_id.clone(),
        thread_id: parent_thread_id.clone(),
        run_id: Some(parent_run.id.clone()),
        parent_node_id: parent_node_id.clone(),
        origin: OrchestrationV2SubagentOrigin::AppOwned,
        created_by: *created_by,
        driver: driver.clone(),
        provider_instance_id: instance.clone(),
        provider_thread_id: None,
        child_thread_id: Some(child_thread_id.clone()),
        native_task_ref: None,
        prompt: task_text.to_string(),
        title: title.as_ref().map(ToString::to_string),
        model: Some(model_selection.model.to_string()),
        model_selection: None,
        completion_wake: *completion_wake,
        completion_delivery: None,
        status: OrchestrationV2SubagentStatus::Running,
        progress: None,
        workflow: None,
        result: None,
        started_at: Some(now),
        completed_at: None,
        updated_at: now,
    };
    let task_node = OrchestrationV2ExecutionNode {
        id: task_node_id.clone(),
        thread_id: parent_thread_id.clone(),
        run_id: Some(parent_run.id.clone()),
        parent_node_id: Some(parent_node_id.clone()),
        root_node_id,
        kind: OrchestrationV2ExecutionNodeKind::Subagent,
        status: OrchestrationV2ExecutionNodeStatus::Running,
        counts_for_run: false,
        provider_thread_id: None,
        provider_turn_id: None,
        native_item_ref: None,
        runtime_request_id: None,
        checkpoint_scope_id: None,
        started_at: Some(now),
        completed_at: None,
    };
    let ordinal = next_turn_item_ordinal(orchestrator, &parent).await?;
    let task_turn_item: OrchestrationV2TurnItem = serde_json::from_value(json!({
        "type": "subagent",
        "id": task_turn_item_id,
        "threadId": parent_thread_id,
        "runId": parent_run.id,
        "nodeId": task_node_id,
        "providerThreadId": parent_run.provider_thread_id,
        "providerTurnId": latest_turn_for_run(&parent, parent_run),
        "nativeItemRef": null,
        "parentItemId": null,
        "ordinal": ordinal,
        "status": "running",
        "title": title.as_ref().map_or(task_title.clone(), ToString::to_string),
        "startedAt": now,
        "completedAt": null,
        "updatedAt": now,
        "subagentId": task_node_id,
        "origin": "app_owned",
        "driver": driver,
        "providerInstanceId": instance,
        "childThreadId": child_thread_id,
        "prompt": task_text,
        "result": null,
    }))
    .map_err(|cause| dispatch_failure(command, cause.to_string()))?;

    let id = decision.event_id(&child_thread_id);
    decision.emit(OrchestrationV2DomainEvent::ThreadCreated {
        id,
        thread_id: child_thread_id.clone(),
        run_id: None,
        node_id: None,
        driver: Some(driver.clone()),
        provider_instance_id: Some(instance.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: child_thread,
    });
    let id = decision.event_id(parent_thread_id);
    decision.emit(OrchestrationV2DomainEvent::NodeUpdated {
        id,
        thread_id: parent_thread_id.clone(),
        run_id: Some(parent_run.id.clone()),
        node_id: Some(task_node_id.clone()),
        driver: Some(driver.clone()),
        provider_instance_id: Some(instance.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: task_node,
    });
    let id = decision.event_id(parent_thread_id);
    decision.emit(OrchestrationV2DomainEvent::SubagentUpdated {
        id,
        thread_id: parent_thread_id.clone(),
        run_id: Some(parent_run.id.clone()),
        node_id: Some(task_node_id.clone()),
        driver: Some(driver.clone()),
        provider_instance_id: Some(instance.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: task,
    });
    let id = decision.event_id(parent_thread_id);
    decision.emit(OrchestrationV2DomainEvent::TurnItemUpdated {
        id,
        thread_id: parent_thread_id.clone(),
        run_id: Some(parent_run.id.clone()),
        node_id: Some(task_node_id.clone()),
        driver: Some(driver.clone()),
        provider_instance_id: Some(instance.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: task_turn_item,
    });

    // The task is the child's first message.
    let child_message: C = serde_json::from_value(json!({
        "type": "message.dispatch",
        "createdBy": created_by,
        "creationSource": creation_source,
        "commandId": this_command_id,
        "threadId": child_thread_id,
        "senderThreadId": parent_thread_id,
        "messageId": child_message_id,
        "text": task_text,
        "attachments": [],
        "modelSelection": model_selection,
        "dispatchMode": match workspace_strategy {
            None => json!({"type": "start_immediately"}),
            Some(strategy) => json!({"type": "defer_start", "workspaceStrategy": strategy}),
        },
    }))
    .map_err(|cause| dispatch_failure(command, cause.to_string()))?;
    let child_command = OrchestrationV2ServerCommand::Client(child_message.clone());
    dispatch_message(orchestrator, &child_command, &child_message, decision).await?;

    let child_run_id = decision.events.iter().find_map(|event| match event {
        OrchestrationV2DomainEvent::RunCreated { payload, .. }
            if payload.thread_id == child_thread_id =>
        {
            Some(payload.id.clone())
        }
        _ => None,
    });
    let Some(child_run_id) = child_run_id else {
        return Err(dispatch_failure(
            command,
            format!("Delegated child thread {child_thread_id} did not create a run."),
        ));
    };
    let transfer_id =
        IdAllocator.context_transfer(parent_thread_id, &child_thread_id, "subagent_spawn");
    let mut source_point = context_source_point_for_run(&parent, parent_run);
    source_point.turn_item_id = Some(task_turn_item_id);
    let transfer = OrchestrationV2ContextTransfer {
        id: transfer_id,
        r#type: OrchestrationV2ContextTransferType::SubagentSpawn,
        source_thread_id: parent_thread_id.clone(),
        target_thread_id: child_thread_id.clone(),
        source_point,
        base_point: None,
        source_provider_instance_id: Some(parent_run.provider_instance_id.clone()),
        target_provider_instance_id: Some(instance.clone()),
        target_run_id: Some(child_run_id.clone()),
        status: OrchestrationV2ContextTransferStatus::Consumed,
        resolution: None,
        created_by: *created_by,
        error: None,
        created_at: now,
        updated_at: now,
        consumed_at: Some(now),
    };
    let id = decision.event_id(&child_thread_id);
    decision.emit(OrchestrationV2DomainEvent::ContextTransferCreated {
        id,
        thread_id: child_thread_id,
        run_id: Some(child_run_id),
        node_id: None,
        driver: None,
        provider_instance_id: Some(instance.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: transfer,
    });
    Ok(true)
}
