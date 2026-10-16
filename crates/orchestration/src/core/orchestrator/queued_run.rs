//! Port of upstream `Orchestrator.ts` queued-run commands:
//! `dispatchQueuedRunReorder` (7764), `dispatchQueuedRunCancel` (7855),
//! `dispatchQueuedRunEdit` (7945) and `dispatchQueuedMessagePromoteToSteer`
//! (7624).
//!
//! `queued-run.cancel` ends a queued run, or, when the run carries a
//! delegated-completion delivery, disposes the cohort that owns it. Stop
//! also cancels buffered provider wakes through [`cancel_queued_run`].

use turin_contracts::{
    PositiveInt, RunId, ThreadId,
    chat_attachment::ChatAttachment,
    composer_context::OrchestrationMessageContext,
    orchestration_v2::{
        OrchestrationV2Command as C, OrchestrationV2ConversationMessageRole,
        OrchestrationV2DomainEvent, OrchestrationV2ExecutionNode,
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2Run, OrchestrationV2RunAttempt,
        OrchestrationV2RunAttemptStatus, OrchestrationV2RunStatus, OrchestrationV2ServerCommand,
        OrchestrationV2TurnItem,
    },
};

use super::Orchestrator;
use super::cohort::{CohortEnd, dispose_delegated_completion_cohort, emit_queued_run_cancellation};
use super::dispatch::{Decision, dispatch_failure};
use super::message_dispatch::run_scoped_event;
use super::message_steer::{SteerInput, steer_into_run};
use crate::pipeline::orchestrator_port::{
    OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
};
use crate::projection::queued_run_order::{
    is_automatic_completion_run, queued_runs_in_delivery_order,
};

/// Decides the queued-run commands. `Ok(false)` leaves the command to another group.
pub(super) async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    let OrchestrationV2ServerCommand::Client(client) = command else {
        return Ok(false);
    };
    match client {
        C::QueuedRunCancel {
            thread_id, run_id, ..
        } => cancel_queued_run(orchestrator, command, decision, thread_id, run_id).await?,
        C::QueuedRunReorder {
            thread_id,
            run_id,
            before_run_id,
            ..
        } => {
            reorder(
                orchestrator,
                command,
                decision,
                thread_id,
                run_id,
                before_run_id.as_ref(),
            )
            .await?
        }
        C::QueuedRunEdit {
            thread_id,
            run_id,
            text,
            attachments,
            context,
            ..
        } => {
            edit(
                orchestrator,
                command,
                decision,
                thread_id,
                run_id,
                text,
                attachments.as_deref(),
                context.as_ref(),
            )
            .await?;
        }
        C::QueuedMessagePromoteToSteer {
            thread_id,
            queued_run_id,
            target_run_id,
            ..
        } => {
            promote_to_steer(
                orchestrator,
                command,
                decision,
                thread_id,
                queued_run_id,
                target_run_id,
            )
            .await?
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// `dispatchQueuedRunReorder`: moves a queued run before another, or last.
/// Automatic completion deliveries stay at the head and never move.
async fn reorder(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    thread_id: &ThreadId,
    run_id: &RunId,
    before_run_id: Option<&RunId>,
) -> Result<(), OrchestratorError> {
    let projection = orchestrator
        .get_thread_records(
            thread_id,
            &[RecordField::Runs, RecordField::Messages],
            &RecordFilter {
                message_roles: Some(vec![OrchestrationV2ConversationMessageRole::User]),
                ..RecordFilter::default()
            },
        )
        .await?;
    let queued = queued_runs_in_delivery_order(&projection.runs, &projection.messages);
    let Some(moving) = queued.iter().copied().find(|run| &run.id == run_id) else {
        return Err(dispatch_failure(
            command,
            format!("Run {run_id} is not queued."),
        ));
    };
    let moving_message = projection
        .messages
        .iter()
        .find(|candidate| candidate.id == moving.user_message_id);
    if moving_message.is_some_and(|message| {
        message.delegated_completion.is_some() || message.notification.is_some()
    }) {
        return Err(dispatch_failure(
            command,
            "Automatic completion deliveries cannot be reordered.",
        ));
    }
    let (automatic, reorderable): (Vec<&OrchestrationV2Run>, Vec<&OrchestrationV2Run>) = queued
        .iter()
        .copied()
        .partition(|run| is_automatic_completion_run(&projection.messages, run));
    if let Some(before) = before_run_id
        && automatic.iter().any(|run| &run.id == before)
    {
        return Err(dispatch_failure(
            command,
            "Queued messages cannot be reordered ahead of automatic completion delivery.",
        ));
    }
    let without_moving: Vec<&OrchestrationV2Run> = reorderable
        .into_iter()
        .filter(|run| &run.id != run_id)
        .collect();
    let before_index = match before_run_id {
        None => without_moving.len(),
        Some(before) => without_moving
            .iter()
            .position(|run| &run.id == before)
            .ok_or_else(|| {
                dispatch_failure(command, format!("Queue target {before} is not queued."))
            })?,
    };
    let mut reordered = automatic;
    reordered.extend(&without_moving[..before_index]);
    reordered.push(moving);
    reordered.extend(&without_moving[before_index..]);
    let now = decision.now();
    for (index, run) in reordered.into_iter().enumerate() {
        let queue_position = PositiveInt::new(index as u64 + 1).expect("positions start at 1");
        if run.queue_position == Some(Some(queue_position)) {
            continue;
        }
        let id = decision.event_id(thread_id);
        decision.emit(OrchestrationV2DomainEvent::RunUpdated {
            id,
            thread_id: thread_id.clone(),
            run_id: Some(run.id.clone()),
            node_id: run.root_node_id.clone(),
            driver: None,
            provider_instance_id: Some(run.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: OrchestrationV2Run {
                queue_position: Some(Some(queue_position)),
                ..run.clone()
            },
        });
    }
    Ok(())
}

/// `dispatchQueuedRunEdit`: rewrites a queued message and its turn item.
#[allow(clippy::too_many_arguments)]
async fn edit(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    thread_id: &ThreadId,
    run_id: &RunId,
    text: &str,
    attachments: Option<&[ChatAttachment]>,
    context: Option<&OrchestrationMessageContext>,
) -> Result<(), OrchestratorError> {
    if turin_contracts::base_schemas::js_trim(text).is_empty() {
        return Err(dispatch_failure(
            command,
            format!("Queued run {run_id} cannot be edited to an empty message."),
        ));
    }
    let projection = orchestrator
        .get_thread_records(
            thread_id,
            &[
                RecordField::Runs,
                RecordField::Messages,
                RecordField::TurnItems,
            ],
            &RecordFilter {
                turn_item_types: Some(vec!["user_message".to_owned()]),
                message_roles: Some(vec![OrchestrationV2ConversationMessageRole::User]),
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
    let Some(message) = projection
        .messages
        .iter()
        .find(|candidate| candidate.id == queued.user_message_id)
    else {
        return Err(dispatch_failure(
            command,
            format!("Queued run {} has no user message.", queued.id),
        ));
    };
    if message.delegated_completion.is_some() || message.notification.is_some() {
        return Err(dispatch_failure(
            command,
            "Automatic completion deliveries cannot be edited.",
        ));
    }
    let turn_item = projection.turn_items.iter().find(|candidate| {
        matches!(
            candidate,
            OrchestrationV2TurnItem::UserMessage { message_id, .. } if message_id == &message.id
        )
    });
    let now = decision.now();
    let mut edited = message.clone();
    edited.text = text.to_owned();
    if let Some(attachments) = attachments {
        edited.attachments = attachments.to_vec();
    }
    if let Some(context) = context {
        edited.context = Some(context.clone());
    }
    edited.updated_at = now;
    let id = decision.event_id(thread_id);
    decision.emit(OrchestrationV2DomainEvent::MessageUpdated {
        id,
        thread_id: thread_id.clone(),
        run_id: Some(queued.id.clone()),
        node_id: queued.root_node_id.clone(),
        driver: None,
        provider_instance_id: Some(queued.provider_instance_id.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: edited.clone(),
    });
    if let Some(item) = turn_item {
        let mut wire = serde_json::to_value(item).expect("turn item encodes");
        if let Some(fields) = wire.as_object_mut() {
            fields.insert("text".to_owned(), serde_json::json!(text));
            if attachments.is_some() {
                fields.insert(
                    "attachments".to_owned(),
                    serde_json::json!(edited.attachments),
                );
            }
            if let Some(context) = context {
                fields.insert("context".to_owned(), serde_json::json!(context));
            }
            fields.insert("updatedAt".to_owned(), serde_json::json!(now));
        }
        let payload = serde_json::from_value(wire).expect("edited turn item decodes");
        let id = decision.event_id(thread_id);
        decision.emit(OrchestrationV2DomainEvent::TurnItemUpdated {
            id,
            thread_id: thread_id.clone(),
            run_id: Some(queued.id.clone()),
            node_id: queued.root_node_id.clone(),
            driver: None,
            provider_instance_id: Some(queued.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload,
        });
    }
    Ok(())
}

/// `dispatchQueuedMessagePromoteToSteer`: cancels the queued run and sends its
/// message into the active run as a steer.
async fn promote_to_steer(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    thread_id: &ThreadId,
    queued_run_id: &RunId,
    target_run_id: &RunId,
) -> Result<(), OrchestratorError> {
    let projection = orchestrator
        .get_thread_records(
            thread_id,
            &[
                RecordField::Runs,
                RecordField::Nodes,
                RecordField::Attempts,
                RecordField::Messages,
                RecordField::ProviderThreads,
                RecordField::ProviderTurns,
                RecordField::ProviderSessions,
                RecordField::TurnItems,
                RecordField::RuntimeRequests,
                RecordField::Subagents,
            ],
            &RecordFilter {
                turn_item_types: Some(Vec::new()),
                message_roles: Some(vec![OrchestrationV2ConversationMessageRole::User]),
                ..RecordFilter::default()
            },
        )
        .await?;
    if projection.thread.archived_at.is_some() || projection.thread.deleted_at.is_some() {
        return Err(dispatch_failure(
            command,
            format!("Thread {thread_id} is not active."),
        ));
    }
    let Some(queued) = projection
        .runs
        .iter()
        .find(|candidate| &candidate.id == queued_run_id)
        .filter(|candidate| candidate.status == OrchestrationV2RunStatus::Queued)
    else {
        return Err(dispatch_failure(
            command,
            format!("Queued run {queued_run_id} is not queued."),
        ));
    };
    let root_node = queued.root_node_id.as_ref().and_then(|id| {
        projection
            .nodes
            .iter()
            .find(|candidate| &candidate.id == id)
    });
    let attempt = queued.active_attempt_id.as_ref().and_then(|id| {
        projection
            .attempts
            .iter()
            .find(|candidate| &candidate.id == id)
    });
    let message = projection
        .messages
        .iter()
        .find(|candidate| candidate.id == queued.user_message_id);
    let (Some(root_node), Some(attempt), Some(message)) = (root_node, attempt, message) else {
        return Err(dispatch_failure(
            command,
            format!(
                "Queued run {} is missing message or execution state.",
                queued.id
            ),
        ));
    };
    if message.delegated_completion.is_some() || message.notification.is_some() {
        return Err(dispatch_failure(
            command,
            "Automatic completion deliveries cannot be promoted to Steer.",
        ));
    }
    let now = decision.now();
    let instance = &queued.provider_instance_id;
    run_scoped_event!(
        decision,
        RunUpdated,
        thread_id,
        &queued.id,
        &root_node.id,
        instance,
        OrchestrationV2Run {
            status: OrchestrationV2RunStatus::Cancelled,
            queue_position: Some(None),
            completed_at: Some(now),
            ..queued.clone()
        }
    );
    run_scoped_event!(
        decision,
        RunAttemptUpdated,
        thread_id,
        &queued.id,
        &root_node.id,
        instance,
        OrchestrationV2RunAttempt {
            status: OrchestrationV2RunAttemptStatus::Cancelled,
            completed_at: Some(now),
            ..attempt.clone()
        }
    );
    run_scoped_event!(
        decision,
        NodeUpdated,
        thread_id,
        &queued.id,
        &root_node.id,
        instance,
        OrchestrationV2ExecutionNode {
            status: OrchestrationV2ExecutionNodeStatus::Cancelled,
            completed_at: Some(now),
            ..root_node.clone()
        }
    );
    steer_into_run(
        orchestrator,
        command,
        &projection,
        SteerInput {
            message: message.clone(),
            model_selection: projection.thread.model_selection.clone(),
            target_run_id: target_run_id.clone(),
            force_restart: false,
            promoted: true,
        },
        decision,
    )
    .await
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
