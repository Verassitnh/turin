//! Port of the small delegated-task commands in upstream `Orchestrator.ts`:
//! `dispatchNotificationAccepted` (10624), `dispatchDelegatedTaskWakePolicy`
//! (7040), `dispatchDelegatedTaskCompletionDeliveryResolution` (1963) and
//! `dispatchCreatedThreadRecord` (7154). `delegated_task.request` is in
//! `delegated_request.rs`.

use turin_contracts::{
    ThreadId,
    orchestration_v2::{
        OrchestrationV2Command as C, OrchestrationV2CompletionWake,
        OrchestrationV2DelegatedCompletionCohort, OrchestrationV2DelegatedCompletionDelivery,
        OrchestrationV2DelegatedCompletionTaskDeliveryState as State, OrchestrationV2DomainEvent,
        OrchestrationV2Run, OrchestrationV2ServerCommand, OrchestrationV2Subagent,
        OrchestrationV2SubagentOrigin, OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
    },
};
use turin_provider_core::id_allocator::{IdAllocator, derive as ids};

use super::Orchestrator;
use super::cohort::{completion_delivery_run, emit_queued_run_cancellation};
use super::delegated_task::{is_terminal_task_status, plan_completion_delivery};
use super::dispatch::{Decision, dispatch_failure};
use super::message_dispatch::{delegated_completion_wake_detail, next_turn_item_ordinal};
use super::thread_fork::latest_turn_for_run;
use crate::pipeline::orchestrator_port::{
    OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
};

/// Decides the delegated-task commands that touch only existing rows.
/// `Ok(false)` leaves the command to another group.
pub(super) async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    let OrchestrationV2ServerCommand::Client(client) = command else {
        return Ok(false);
    };
    match client {
        C::NotificationDeliveryAccept {
            thread_id,
            message_id,
            ..
        } => notification_accepted(orchestrator, decision, thread_id, message_id).await?,
        C::DelegatedTaskWakePolicy {
            parent_thread_id,
            task_id,
            completion_wake,
            ..
        } => {
            wake_policy(
                orchestrator,
                command,
                decision,
                parent_thread_id,
                task_id,
                *completion_wake,
            )
            .await?;
        }
        C::DelegatedTaskCompletionDeliveryAcknowledge {
            parent_thread_id,
            task_id,
            observed_by_run_id,
            ..
        } => {
            resolve_delivery(
                orchestrator,
                command,
                decision,
                parent_thread_id,
                task_id,
                State::Acknowledged,
                observed_by_run_id.clone(),
            )
            .await?;
        }
        C::DelegatedTaskCompletionDeliveryDispose {
            parent_thread_id,
            task_id,
            ..
        } => {
            resolve_delivery(
                orchestrator,
                command,
                decision,
                parent_thread_id,
                task_id,
                State::Disposed,
                None,
            )
            .await?;
        }
        C::ThreadCreatedRecord {
            parent_thread_id,
            parent_run_id,
            parent_node_id,
            target_thread_id,
            target_run_id,
            ..
        } => {
            created_thread_record(
                orchestrator,
                command,
                decision,
                parent_thread_id,
                parent_run_id,
                parent_node_id,
                target_thread_id,
                target_run_id.as_ref(),
            )
            .await?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn live(run: &OrchestrationV2Run) -> bool {
    matches!(
        run.status,
        turin_contracts::orchestration_v2::OrchestrationV2RunStatus::Preparing
            | turin_contracts::orchestration_v2::OrchestrationV2RunStatus::Starting
            | turin_contracts::orchestration_v2::OrchestrationV2RunStatus::Running
    )
}

fn task_delivery(
    state: State,
    observed_by_run_id: Option<turin_contracts::RunId>,
) -> Option<turin_contracts::orchestration_v2::OrchestrationV2DelegatedCompletionTaskDelivery> {
    Some(
        turin_contracts::orchestration_v2::OrchestrationV2DelegatedCompletionTaskDelivery {
            state,
            observed_by_run_id,
        },
    )
}

/// `notification.delivery.accept`: the provider took a delegated-completion
/// message. Its batch is drained (not acknowledged: `task_status` owns that)
/// and results that arrived meanwhile are reserved for one successor.
async fn notification_accepted(
    orchestrator: &Orchestrator,
    decision: &mut Decision,
    thread_id: &ThreadId,
    message_id: &turin_contracts::MessageId,
) -> Result<(), OrchestratorError> {
    let projection = orchestrator.read_command_projection(thread_id).await?;
    let message = projection.messages.iter().find(|row| &row.id == message_id);
    let ownership = message.and_then(|message| message.delegated_completion.as_ref());
    let parent_run = ownership.and_then(|ownership| {
        projection
            .runs
            .iter()
            .find(|run| run.id == ownership.parent_run_id)
    });
    let cohort = parent_run.and_then(|run| run.delegated_completion.as_ref());
    let delivery = cohort.and_then(|cohort| cohort.delivery.as_ref());
    let now = decision.now();
    let (Some(parent_run), Some(cohort), Some(delivery)) = (parent_run, cohort, delivery) else {
        return stale(decision, &projection, now);
    };
    if cohort.disposition
        != turin_contracts::orchestration_v2::OrchestrationV2DelegatedCompletionCohortDisposition::Open
        || &delivery.message_id != message_id
        || Some(delivery.generation) != ownership.map(|ownership| ownership.generation)
    {
        return stale(decision, &projection, now);
    }
    // settled_only tasks defer only to a blocking wait in their spawning run.
    let parent_is_live = live(parent_run);
    let pending_task_ids: Vec<_> = if projection.thread.archived_at.is_none()
        && projection.thread.deleted_at.is_none()
    {
        projection
            .subagents
            .iter()
            .filter(|task| {
                task.origin == OrchestrationV2SubagentOrigin::AppOwned
                    && task.run_id.as_ref() == Some(&parent_run.id)
                    && task.completion_delivery.as_ref().map(|d| d.state) == Some(State::Pending)
                    && is_terminal_task_status(task.status)
                    && (!parent_is_live
                        || task.completion_wake == Some(OrchestrationV2CompletionWake::Always))
            })
            .map(|task| task.id.clone())
            .collect()
    } else {
        Vec::new()
    };
    let next_delivery = if pending_task_ids.is_empty() {
        None
    } else {
        let count = orchestrator
            .projection
            .get_message_count(thread_id)
            .await
            .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;
        Some(OrchestrationV2DelegatedCompletionDelivery {
            generation: cohort.next_generation,
            message_id: IdAllocator.message(thread_id, count + 1),
            task_ids: pending_task_ids.clone(),
        })
    };
    for task in &projection.subagents {
        let state = if pending_task_ids.contains(&task.id) {
            State::Claimed
        } else if delivery.task_ids.contains(&task.id)
            && task.completion_delivery.as_ref().map(|d| d.state) == Some(State::Claimed)
        {
            State::Delivered
        } else {
            continue;
        };
        let id = decision.event_id(thread_id);
        decision.emit(OrchestrationV2DomainEvent::SubagentUpdated {
            id,
            thread_id: thread_id.clone(),
            run_id: Some(parent_run.id.clone()),
            node_id: Some(task.id.clone()),
            driver: Some(task.driver.clone()),
            provider_instance_id: Some(task.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: OrchestrationV2Subagent {
                completion_delivery: task_delivery(state, None),
                updated_at: now,
                ..task.clone()
            },
        });
    }
    let next_generation = if next_delivery.is_some() {
        turin_contracts::PositiveInt::new(cohort.next_generation.get() + 1).expect("positive")
    } else {
        cohort.next_generation
    };
    let id = decision.event_id(thread_id);
    decision.emit(OrchestrationV2DomainEvent::RunUpdated {
        id,
        thread_id: thread_id.clone(),
        run_id: Some(parent_run.id.clone()),
        node_id: None,
        driver: None,
        provider_instance_id: Some(parent_run.provider_instance_id.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: OrchestrationV2Run {
            delegated_completion: Some(OrchestrationV2DelegatedCompletionCohort {
                next_generation,
                delivery: next_delivery,
                ..cohort.clone()
            }),
            ..parent_run.clone()
        },
    });
    Ok(())
}

/// A stale acceptance records the thread row unchanged, so it is accepted
/// without doing any work.
fn stale(
    decision: &mut Decision,
    projection: &turin_contracts::orchestration_v2::OrchestrationV2ThreadProjection,
    now: turin_contracts::UtcDateTime,
) -> Result<(), OrchestratorError> {
    let id = decision.event_id(&projection.thread.id);
    decision.emit(OrchestrationV2DomainEvent::ThreadMetadataUpdated {
        id,
        thread_id: projection.thread.id.clone(),
        run_id: None,
        node_id: None,
        driver: None,
        provider_instance_id: None,
        raw_event_id: None,
        occurred_at: now,
        payload: projection.thread.clone(),
    });
    Ok(())
}

/// `delegated_task.wake-policy`.
async fn wake_policy(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    parent_thread_id: &ThreadId,
    task_id: &turin_contracts::NodeId,
    completion_wake: OrchestrationV2CompletionWake,
) -> Result<(), OrchestratorError> {
    let parent = orchestrator
        .get_thread_records(
            parent_thread_id,
            &[
                RecordField::Subagents,
                RecordField::Runs,
                RecordField::Messages,
                RecordField::ProviderTurns,
            ],
            &RecordFilter::default(),
        )
        .await?;
    // No-op commands reject with a descriptive cause, matching the thread
    // mutation handlers ("already archived", "not archived").
    let Some(task) = parent
        .subagents
        .iter()
        .find(|task| &task.id == task_id && task.origin == OrchestrationV2SubagentOrigin::AppOwned)
    else {
        return Err(dispatch_failure(
            command,
            format!(
                "Delegated task {task_id} is not an app-owned task of thread {parent_thread_id}."
            ),
        ));
    };
    if task.completion_wake == Some(completion_wake) {
        return Err(dispatch_failure(
            command,
            format!(
                "Delegated task {task_id} already wakes the parent with completionWake {}.",
                serde_json::to_value(completion_wake)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default()
            ),
        ));
    }
    let now = decision.now();
    let updated_task = OrchestrationV2Subagent {
        completion_wake: Some(completion_wake),
        updated_at: now,
        ..task.clone()
    };
    // A non-terminal task needs no offer here: finalize reads the upgraded
    // policy when the child terminalizes. Both writers hold this parent lock,
    // so a terminal task means finalize already committed the terminal row and
    // already made its offer decision under the pre-upgrade policy: under
    // settled_only it offered iff the spawning run was not live. Plan a
    // delivery only when that run is live now, which is precisely the case
    // where finalize skipped. When that run is not live, finalize already
    // offered and a second offer would wake the parent twice.
    let parent_run = task
        .run_id
        .as_ref()
        .and_then(|id| parent.runs.iter().find(|run| &run.id == id));
    let plan = match parent_run {
        Some(parent_run)
            if completion_wake == OrchestrationV2CompletionWake::Always
                && is_terminal_task_status(task.status)
                && live(parent_run) =>
        {
            let count = orchestrator
                .projection
                .get_message_count(parent_thread_id)
                .await
                .map_err(|cause| OrchestratorError::projection(parent_thread_id.clone(), cause))?;
            Some(plan_completion_delivery(
                &parent,
                Some(parent_run),
                &updated_task,
                &updated_task,
                now,
                || IdAllocator.message(parent_thread_id, count + 1),
            ))
        }
        _ => None,
    };
    let id = decision.event_id(parent_thread_id);
    decision.emit(OrchestrationV2DomainEvent::SubagentUpdated {
        id,
        thread_id: parent_thread_id.clone(),
        run_id: task.run_id.clone(),
        node_id: Some(task.id.clone()),
        driver: Some(task.driver.clone()),
        provider_instance_id: Some(task.provider_instance_id.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: plan
            .as_ref()
            .map_or_else(|| updated_task.clone(), |plan| plan.task.clone()),
    });
    let Some(plan) = plan else {
        return Ok(());
    };
    if let Some(run) = &plan.parent_run {
        let id = decision.event_id(parent_thread_id);
        decision.emit(OrchestrationV2DomainEvent::RunUpdated {
            id,
            thread_id: parent_thread_id.clone(),
            run_id: Some(run.id.clone()),
            node_id: run.root_node_id.clone(),
            driver: None,
            provider_instance_id: Some(run.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: run.clone(),
        });
    }
    if let Some(message) = &plan.message {
        let id = decision.event_id(parent_thread_id);
        decision.emit(OrchestrationV2DomainEvent::MessageUpdated {
            id,
            thread_id: parent_thread_id.clone(),
            run_id: message.run_id.clone(),
            node_id: message.node_id.clone(),
            driver: None,
            provider_instance_id: Some(
                plan.parent_run
                    .as_ref()
                    .map(|run| run.provider_instance_id.clone())
                    .unwrap_or_else(|| parent.thread.provider_instance_id.clone()),
            ),
            raw_event_id: None,
            occurred_at: now,
            payload: message.clone(),
        });
    }
    Ok(())
}

/// `delegated_task.completion-delivery.acknowledge` and `.dispose`.
async fn resolve_delivery(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    parent_thread_id: &ThreadId,
    task_id: &turin_contracts::NodeId,
    state: State,
    observed_by_run_id: Option<turin_contracts::RunId>,
) -> Result<(), OrchestratorError> {
    let projection = orchestrator
        .get_thread_records(
            parent_thread_id,
            &[
                RecordField::Subagents,
                RecordField::Runs,
                RecordField::Messages,
                RecordField::Nodes,
                RecordField::Attempts,
                RecordField::TurnItems,
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
    let Some(task) = projection
        .subagents
        .iter()
        .find(|task| &task.id == task_id && task.origin == OrchestrationV2SubagentOrigin::AppOwned)
    else {
        return Err(dispatch_failure(
            command,
            format!(
                "Delegated task {task_id} is not an app-owned task of thread {parent_thread_id}."
            ),
        ));
    };
    let now = decision.now();
    let emit_task = |decision: &mut Decision, payload: OrchestrationV2Subagent| {
        let id = decision.event_id(parent_thread_id);
        decision.emit(OrchestrationV2DomainEvent::SubagentUpdated {
            id,
            thread_id: parent_thread_id.clone(),
            run_id: task.run_id.clone(),
            node_id: Some(task.id.clone()),
            driver: Some(task.driver.clone()),
            provider_instance_id: Some(task.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload,
        });
    };
    let current = task.completion_delivery.as_ref().map(|d| d.state);
    // task_status and t3_thread_read use distinct command IDs, so two valid
    // observations can race after their read preflight. Re-emit the existing
    // task row so the second dispatch is a successful idempotent no-op rather
    // than "already acknowledged/disposed" or empty-events.
    if current == Some(state) || (state == State::Acknowledged && current == Some(State::Disposed))
    {
        emit_task(decision, task.clone());
        return Ok(());
    }
    emit_task(
        decision,
        OrchestrationV2Subagent {
            completion_delivery: task_delivery(state, observed_by_run_id),
            updated_at: now,
            ..task.clone()
        },
    );
    let parent_run = task
        .run_id
        .as_ref()
        .and_then(|id| projection.runs.iter().find(|run| &run.id == id));
    let Some((parent_run, cohort, delivery)) = parent_run.and_then(|run| {
        let cohort = run.delegated_completion.as_ref()?;
        let delivery = cohort.delivery.as_ref()?;
        Some((run, cohort, delivery))
    }) else {
        return Ok(());
    };
    if !delivery.task_ids.contains(&task.id) {
        return Ok(());
    }
    let remaining: Vec<_> = delivery
        .task_ids
        .iter()
        .filter(|id| **id != task.id)
        .cloned()
        .collect();
    let delivery_run = completion_delivery_run(&projection, Some(delivery));
    let clear = remaining.is_empty()
        && delivery_run.is_none_or(|run| {
            run.status == turin_contracts::orchestration_v2::OrchestrationV2RunStatus::Queued
        });
    let id = decision.event_id(parent_thread_id);
    decision.emit(OrchestrationV2DomainEvent::RunUpdated {
        id,
        thread_id: parent_thread_id.clone(),
        run_id: Some(parent_run.id.clone()),
        node_id: parent_run.root_node_id.clone(),
        driver: None,
        provider_instance_id: Some(parent_run.provider_instance_id.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: OrchestrationV2Run {
            delegated_completion: Some(OrchestrationV2DelegatedCompletionCohort {
                delivery: if clear {
                    None
                } else {
                    Some(OrchestrationV2DelegatedCompletionDelivery {
                        task_ids: remaining.clone(),
                        ..delivery.clone()
                    })
                },
                ..cohort.clone()
            }),
            ..parent_run.clone()
        },
    });
    if let Some(delivery_run) = delivery_run
        && delivery_run.status
            == turin_contracts::orchestration_v2::OrchestrationV2RunStatus::Queued
    {
        if remaining.is_empty() {
            emit_queued_run_cancellation(decision, &projection, delivery_run, now);
            return Ok(());
        }
        if let Some(message) = projection
            .messages
            .iter()
            .find(|message| message.id == delivery.message_id)
        {
            let id = decision.event_id(parent_thread_id);
            decision.emit(OrchestrationV2DomainEvent::MessageUpdated {
                id,
                thread_id: parent_thread_id.clone(),
                run_id: Some(delivery_run.id.clone()),
                node_id: delivery_run.root_node_id.clone(),
                driver: None,
                provider_instance_id: Some(delivery_run.provider_instance_id.clone()),
                raw_event_id: None,
                occurred_at: now,
                payload: turin_contracts::orchestration_v2::OrchestrationV2ConversationMessage {
                    text: delegated_completion_wake_detail(&remaining),
                    delegated_completion: Some(
                        turin_contracts::orchestration_v2::OrchestrationV2MessageDelegatedCompletion {
                            parent_run_id: parent_run.id.clone(),
                            generation: delivery.generation,
                            task_ids: remaining,
                        },
                    ),
                    updated_at: now,
                    ..message.clone()
                },
            });
        }
    }
    Ok(())
}

/// `thread.created.record`: the parent's timeline notes a thread an agent made.
#[allow(clippy::too_many_arguments)]
async fn created_thread_record(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    parent_thread_id: &ThreadId,
    parent_run_id: &turin_contracts::RunId,
    parent_node_id: &turin_contracts::NodeId,
    target_thread_id: &ThreadId,
    target_run_id: Option<&turin_contracts::RunId>,
) -> Result<(), OrchestratorError> {
    let read = |thread: &ThreadId| {
        let thread = thread.clone();
        async move {
            orchestrator
                .get_thread_records(
                    &thread,
                    &[
                        RecordField::Runs,
                        RecordField::Nodes,
                        RecordField::TurnItems,
                        RecordField::Attempts,
                        RecordField::ProviderTurns,
                    ],
                    &RecordFilter {
                        turn_item_types: Some(Vec::new()),
                        message_roles: Some(vec![
                            turin_contracts::orchestration_v2::OrchestrationV2ConversationMessageRole::User,
                        ]),
                        ..RecordFilter::default()
                    },
                )
                .await
        }
    };
    let parent = read(parent_thread_id).await?;
    let target = read(target_thread_id).await?;
    let parent_run = parent.runs.iter().find(|run| &run.id == parent_run_id);
    let parent_node = parent.nodes.iter().find(|node| &node.id == parent_node_id);
    let (Some(parent_run), Some(parent_node)) = (parent_run, parent_node) else {
        return Err(dispatch_failure(
            command,
            format!("Parent node {parent_node_id} is not the root of run {parent_run_id}."),
        ));
    };
    if parent_node.run_id.as_ref() != Some(parent_run_id)
        || parent_run.root_node_id.as_ref() != Some(parent_node_id)
    {
        return Err(dispatch_failure(
            command,
            format!("Parent node {parent_node_id} is not the root of run {parent_run_id}."),
        ));
    }
    if parent.thread.project_id != target.thread.project_id {
        return Err(dispatch_failure(
            command,
            format!("Target thread {target_thread_id} belongs to another project."),
        ));
    }
    if let Some(target_run_id) = target_run_id
        && !target.runs.iter().any(|run| &run.id == target_run_id)
    {
        return Err(dispatch_failure(
            command,
            format!("Target run {target_run_id} does not belong to thread {target_thread_id}."),
        ));
    }
    let now = decision.now();
    let command_id = super::dispatch::command_id(command);
    let ordinal = next_turn_item_ordinal(orchestrator, &parent).await?;
    let item = OrchestrationV2TurnItem::ThreadCreated {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: ids::created_thread_turn_item(command_id),
        thread_id: parent_thread_id.clone(),
        run_id: Some(parent_run_id.clone()),
        node_id: Some(parent_node_id.clone()),
        provider_thread_id: parent_run.provider_thread_id.clone(),
        provider_turn_id: latest_turn_for_run(&parent, parent_run),
        native_item_ref: None,
        parent_item_id: None,
        ordinal,
        status: OrchestrationV2TurnItemStatus::Completed,
        title: Some(target.thread.title.to_string()),
        started_at: Some(now),
        completed_at: Some(now),
        updated_at: now,
        target_thread_id: target_thread_id.clone(),
        target_run_id: target_run_id.cloned(),
        target_provider_instance_id: target.thread.model_selection.instance_id.clone(),
        target_model: turin_contracts::TrimmedNonEmptyString::new(
            &target.thread.model_selection.model,
        )
        .map_err(|cause| dispatch_failure(command, cause.to_string()))?,
    };
    let id = decision.event_id(parent_thread_id);
    decision.emit(OrchestrationV2DomainEvent::TurnItemUpdated {
        id,
        thread_id: parent_thread_id.clone(),
        run_id: Some(parent_run_id.clone()),
        node_id: Some(parent_node_id.clone()),
        driver: None,
        provider_instance_id: Some(parent_run.provider_instance_id.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: item,
    });
    Ok(())
}
