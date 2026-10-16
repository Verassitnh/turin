//! Port of upstream `Orchestrator.ts` `dispatchRuntimeRequestRespond` (7357)
//! and `dispatchThreadUserInputDismiss` (7586).
//!
//! Answering an approval or a user-input question resolves the request, its
//! node and its turn item, then either queues the provider reply
//! (`runtime-request.respond`, run by `RuntimeRequestService`) or, for a
//! question the provider can only take as a message, dispatches the answers
//! as the thread's next message in the same transaction.

use serde_json::json;
use turin_contracts::{
    RuntimeRequestId, ThreadId,
    orchestration_v2::{
        OrchestrationV2Command as C, OrchestrationV2DomainEvent, OrchestrationV2ExecutionNode,
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2ResponseCapability,
        OrchestrationV2RuntimeRequest, OrchestrationV2RuntimeRequestKind,
        OrchestrationV2RuntimeRequestStatus, OrchestrationV2ServerCommand, OrchestrationV2TurnItem,
        OrchestrationV2TurnItemStatus,
    },
    provider_policy::{ProviderApprovalDecision, ProviderUserInputAnswers, UserInputAttachments},
};

use super::Orchestrator;
use super::cohort::patch_turn_item;
use super::dispatch::{Decision, command_id, dispatch_failure};
use super::message_dispatch::dispatch_message;
use crate::pipeline::effect_outbox::{EffectRequest, PendingEffect};
use crate::pipeline::orchestrator_port::OrchestratorError;

/// Decides `runtime-request.respond` and `thread.user-input.dismiss`.
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
        C::RuntimeRequestRespond {
            thread_id,
            request_id,
            decision: reply,
            answers,
            attachments_by_question_id,
            ..
        } => {
            respond(
                orchestrator,
                command,
                decision,
                Response {
                    thread_id,
                    request_id,
                    decision: *reply,
                    answers: answers.as_ref(),
                    attachments_by_question_id: attachments_by_question_id.as_ref(),
                },
            )
            .await?;
        }
        C::ThreadUserInputDismiss {
            thread_id,
            request_id,
            ..
        } => dismiss(orchestrator, command, decision, thread_id, request_id).await?,
        _ => return Ok(false),
    }
    Ok(true)
}

pub(super) struct Response<'a> {
    pub(super) thread_id: &'a ThreadId,
    pub(super) request_id: &'a RuntimeRequestId,
    pub(super) decision: Option<ProviderApprovalDecision>,
    pub(super) answers: Option<&'a ProviderUserInputAnswers>,
    pub(super) attachments_by_question_id: Option<&'a UserInputAttachments>,
}

/// `dispatchRuntimeRequestRespond`.
pub(super) async fn respond(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    response: Response<'_>,
) -> Result<(), OrchestratorError> {
    let Response {
        thread_id,
        request_id,
        decision: reply,
        answers,
        attachments_by_question_id,
    } = response;
    let context = orchestrator
        .projection
        .get_runtime_response_context(thread_id, request_id.as_str())
        .await
        .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;
    let Some(request) = context.request else {
        return Err(dispatch_failure(
            command,
            format!("Runtime request {request_id} was not found."),
        ));
    };
    if request.status != OrchestrationV2RuntimeRequestStatus::Pending {
        return Err(dispatch_failure(
            command,
            format!(
                "Runtime request {request_id} is {}.",
                serde_json::to_value(request.status)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default()
            ),
        ));
    }
    if let OrchestrationV2ResponseCapability::NotResumable { reason } = &request.response_capability
    {
        return Err(dispatch_failure(command, reason.clone()));
    }
    let provider_session_id = match &request.response_capability {
        OrchestrationV2ResponseCapability::Live {
            provider_session_id,
        } => Some(provider_session_id.clone()),
        _ => None,
    };
    let session = context.session;
    if let (Some(id), None) = (&provider_session_id, &session) {
        return Err(dispatch_failure(
            command,
            format!("Provider session {id} was not found."),
        ));
    }

    let now = decision.now();
    let resolved_request = OrchestrationV2RuntimeRequest {
        status: OrchestrationV2RuntimeRequestStatus::Resolved,
        resolved_at: Some(now),
        decision: reply.or(request.decision),
        answers: answers.cloned().or_else(|| request.answers.clone()),
        ..request.clone()
    };
    let request_node = context.node;
    let resolved_node_status = if matches!(
        reply,
        Some(ProviderApprovalDecision::Decline | ProviderApprovalDecision::Cancel)
    ) {
        OrchestrationV2ExecutionNodeStatus::Cancelled
    } else {
        OrchestrationV2ExecutionNodeStatus::Completed
    };
    let (driver, instance) = match &session {
        Some(session) => (
            Some(session.driver.clone()),
            Some(session.provider_instance_id.clone()),
        ),
        None => (None, None),
    };
    let id = decision.event_id(thread_id);
    decision.emit(OrchestrationV2DomainEvent::RuntimeRequestUpdated {
        id,
        thread_id: thread_id.clone(),
        run_id: request_node.as_ref().and_then(|node| node.run_id.clone()),
        node_id: Some(request.node_id.clone()),
        driver: driver.clone(),
        provider_instance_id: instance.clone(),
        raw_event_id: None,
        occurred_at: now,
        payload: resolved_request,
    });
    if let Some(node) = &request_node {
        let id = decision.event_id(thread_id);
        decision.emit(OrchestrationV2DomainEvent::NodeUpdated {
            id,
            thread_id: thread_id.clone(),
            run_id: node.run_id.clone(),
            node_id: Some(node.id.clone()),
            driver: driver.clone(),
            provider_instance_id: instance.clone(),
            raw_event_id: None,
            occurred_at: now,
            payload: OrchestrationV2ExecutionNode {
                status: resolved_node_status,
                completed_at: Some(now),
                ..node.clone()
            },
        });
    }

    let approval_item = context.item;
    if let Some(item) = &approval_item {
        let item_status = match resolved_node_status {
            OrchestrationV2ExecutionNodeStatus::Cancelled => {
                OrchestrationV2TurnItemStatus::Cancelled
            }
            _ => OrchestrationV2TurnItemStatus::Completed,
        };
        let mut patch = vec![
            ("status", json!(item_status)),
            ("completedAt", json!(now)),
            ("updatedAt", json!(now)),
        ];
        if let (OrchestrationV2TurnItem::UserInputRequest { questions, .. }, Some(answers)) =
            (item, answers)
        {
            let question_text_by_id: serde_json::Map<String, serde_json::Value> = questions
                .iter()
                .map(|question| {
                    (
                        question.id.as_str().to_owned(),
                        json!(question.question.as_str()),
                    )
                })
                .collect();
            patch.push((
                "questionAnswer",
                json!({
                    "requestId": request_id,
                    "answers": answers,
                    "attachmentsByQuestionId": attachments_by_question_id
                        .map_or_else(|| json!({}), |attachments| json!(attachments)),
                    "questionTextById": question_text_by_id,
                }),
            ));
        }
        let payload = patch_turn_item(item, patch);
        let common = turin_provider_core::turn_item::turn_item_common(&payload);
        let (run_id, node_id) = (common.run_id.cloned(), common.node_id.cloned());
        let id = decision.event_id(thread_id);
        decision.emit(OrchestrationV2DomainEvent::TurnItemUpdated {
            id,
            thread_id: thread_id.clone(),
            run_id,
            node_id,
            driver,
            provider_instance_id: instance,
            raw_event_id: None,
            occurred_at: now,
            payload,
        });
    }

    if matches!(
        request.response_capability,
        OrchestrationV2ResponseCapability::Message
    ) {
        if resolved_node_status == OrchestrationV2ExecutionNodeStatus::Cancelled {
            return Ok(());
        }
        let Some(OrchestrationV2TurnItem::UserInputRequest { questions, .. }) = &approval_item
        else {
            return Err(dispatch_failure(
                command,
                "The question for this request was not found.",
            ));
        };
        let mut replies: Vec<String> = Vec::new();
        for question in questions {
            let answer = answers
                .and_then(|answers| answers.get(question.id.as_str()))
                .and_then(|answer| answer.as_str())
                .map(turin_contracts::base_schemas::js_trim)
                .filter(|answer| !answer.is_empty());
            let Some(answer) = answer else {
                if question.required == Some(false) {
                    continue;
                }
                return Err(dispatch_failure(
                    command,
                    "Answer each question before sending.",
                ));
            };
            replies.push(format!("{}\n{answer}", question.question.as_str()));
        }
        if replies.is_empty() {
            return Err(dispatch_failure(command, "Enter an answer before sending."));
        }
        return dispatch_answers_as_message(
            orchestrator,
            command,
            decision,
            thread_id,
            request_id,
            replies.join("\n\n"),
        )
        .await;
    }
    let Some(provider_session_id) = provider_session_id else {
        return Ok(());
    };
    decision.effects.push(PendingEffect {
        id: format!(
            "effect:{}:runtime-request.respond:{request_id}",
            command_id(command)
        ),
        command_id: command_id(command).clone(),
        thread_id: thread_id.clone(),
        request: EffectRequest::RuntimeRequestRespond {
            provider_session_id,
            request_id: request_id.clone(),
            decision: reply,
            answers: answers.cloned(),
        },
        available_at: None,
    });
    Ok(())
}

/// A question the provider takes as a message: its answers become the thread's
/// next message, steered into the running turn when the live session can steer.
async fn dispatch_answers_as_message(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    thread_id: &ThreadId,
    request_id: &RuntimeRequestId,
    text: String,
) -> Result<(), OrchestratorError> {
    let mut dispatch_mode = json!({"type": "queue_after_active"});
    let running = orchestrator
        .projection
        .get_running_turn_context(thread_id)
        .await
        .map_err(|cause| dispatch_failure(command, cause.to_string()))?;
    if let (Some(run), Some(_turn), Some(session_id)) = (
        running.run.as_ref(),
        running.provider_turn.as_ref(),
        running
            .provider_thread
            .as_ref()
            .and_then(|thread| thread.provider_session_id.as_ref()),
    ) {
        let session = orchestrator
            .seams
            .sessions
            .get_session(session_id)
            .await
            .map_err(|cause| dispatch_failure(command, cause.to_string()))?;
        if session.is_some_and(|session| {
            session
                .provider_session()
                .capabilities
                .turns
                .supports_active_steering
        }) {
            dispatch_mode = json!({"type": "steer_active", "targetRunId": run.id});
        }
    }
    // The resolution and normal message dispatch share one event transaction.
    let synthetic: OrchestrationV2ServerCommand = serde_json::from_value(json!({
        "type": "message.dispatch",
        "commandId": command_id(command),
        "threadId": thread_id,
        "messageId": format!("async-answer:{request_id}"),
        "text": text,
        "attachments": [],
        "createdBy": "user",
        "creationSource": "server",
        "dispatchMode": dispatch_mode,
    }))
    .map_err(|cause| dispatch_failure(command, cause.to_string()))?;
    let OrchestrationV2ServerCommand::Client(client) = &synthetic else {
        unreachable!("message.dispatch is a client command");
    };
    dispatch_message(orchestrator, &synthetic, client, decision).await
}

/// `dispatchThreadUserInputDismiss`: declines a question the provider takes as
/// a message, which needs no answer.
async fn dismiss(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    thread_id: &ThreadId,
    request_id: &RuntimeRequestId,
) -> Result<(), OrchestratorError> {
    let context = orchestrator
        .projection
        .get_runtime_response_context(thread_id, request_id.as_str())
        .await
        .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;
    let Some(request) = context
        .request
        .filter(|request| request.status == OrchestrationV2RuntimeRequestStatus::Pending)
    else {
        return Err(dispatch_failure(
            command,
            "This question has already been answered.",
        ));
    };
    if request.kind != OrchestrationV2RuntimeRequestKind::UserInput
        || !matches!(
            request.response_capability,
            OrchestrationV2ResponseCapability::Message
        )
    {
        return Err(dispatch_failure(
            command,
            "This question needs an answer. Answer it or stop the turn.",
        ));
    }
    respond(
        orchestrator,
        command,
        decision,
        Response {
            thread_id,
            request_id,
            decision: Some(ProviderApprovalDecision::Cancel),
            answers: None,
            attachments_by_question_id: None,
        },
    )
    .await
}
