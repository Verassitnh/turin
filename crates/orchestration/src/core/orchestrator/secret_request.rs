//! Port of upstream `Orchestrator.ts` `dispatchSecretRequestRecord` (7269) and
//! `dispatchProviderSessionDetach` (3520).
//!
//! `secret_request.record` is internal, so no client can mark a request saved
//! without the value being stored. `provider-session.detach` ends a thread's
//! session on request.

use turin_contracts::{
    ProviderSessionId, ThreadId,
    orchestration_v2::{
        OrchestrationV2Command as C, OrchestrationV2DomainEvent,
        OrchestrationV2InternalCommand as I, OrchestrationV2ProviderSessionDetached,
        OrchestrationV2SecretRequestStatus, OrchestrationV2ServerCommand, OrchestrationV2TurnItem,
        OrchestrationV2TurnItemStatus,
    },
};
use turin_provider_core::turn_item::turn_item_common;

use super::Orchestrator;
use super::dispatch::{Decision, command_id, dispatch_failure};
use super::message_dispatch::next_turn_item_ordinal;
use crate::pipeline::effect_outbox::{EffectRequest, PendingEffect};
use crate::pipeline::orchestrator_port::{
    OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
};

/// Decides `secret_request.record` and `provider-session.detach`.
/// `Ok(false)` leaves the command to another group.
pub(super) async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    use OrchestrationV2ServerCommand::{Client, Internal};
    match command {
        Internal(I::SecretRequestRecord {
            thread_id,
            run_id,
            node_id,
            turn_item_id,
            label,
            reason,
            placeholder,
            secret_status,
            ..
        }) => {
            let projection = orchestrator
                .get_thread_records(
                    thread_id,
                    &[
                        RecordField::Runs,
                        RecordField::Nodes,
                        RecordField::TurnItems,
                        RecordField::Attempts,
                        RecordField::ProviderTurns,
                    ],
                    &RecordFilter {
                        turn_item_types: Some(vec!["secret_request".to_owned()]),
                        message_roles: Some(Vec::new()),
                        ..RecordFilter::default()
                    },
                )
                .await?;
            let run = projection
                .runs
                .iter()
                .find(|candidate| &candidate.id == run_id);
            let Some(run) = run.filter(|run| run.root_node_id.as_ref() == Some(node_id)) else {
                return Err(dispatch_failure(
                    command,
                    format!("Node {node_id} is not the root of run {run_id}."),
                ));
            };
            let existing = projection
                .turn_items
                .iter()
                .find(|item| turn_item_common(item).id == turn_item_id);
            if let Some(existing) = existing {
                if !matches!(existing, OrchestrationV2TurnItem::SecretRequest { .. }) {
                    return Err(dispatch_failure(
                        command,
                        format!("Turn item {turn_item_id} is not a secret request."),
                    ));
                }
                if turn_item_common(existing).run_id != Some(run_id) {
                    return Err(dispatch_failure(
                        command,
                        format!("Secret request {turn_item_id} belongs to another run."),
                    ));
                }
            }
            let now = decision.now();
            // A request is answered once, and a retry finds its card as it was
            // asked: either one records the card unchanged.
            let unchanged = matches!(
                existing,
                Some(OrchestrationV2TurnItem::SecretRequest { secret_status: existing_status, .. })
                    if *existing_status != OrchestrationV2SecretRequestStatus::Pending
                        || *secret_status == OrchestrationV2SecretRequestStatus::Pending
            );
            let pending = *secret_status == OrchestrationV2SecretRequestStatus::Pending;
            let payload = match existing {
                Some(existing) if unchanged => existing.clone(),
                _ => {
                    let existing_common = existing.map(turn_item_common);
                    let ordinal = match existing_common {
                        Some(common) => common.ordinal,
                        None => next_turn_item_ordinal(orchestrator, &projection).await?,
                    };
                    let provider_turn_id =
                        super::thread_fork::latest_turn_for_run(&projection, run);
                    OrchestrationV2TurnItem::SecretRequest {
                        tool_non_execution_kind: None,
                        tool_surface: None,
                        tool_icon: None,
                        tool_source: None,
                        id: turn_item_id.clone(),
                        thread_id: thread_id.clone(),
                        run_id: Some(run_id.clone()),
                        node_id: Some(node_id.clone()),
                        provider_thread_id: run.provider_thread_id.clone(),
                        provider_turn_id,
                        native_item_ref: None,
                        parent_item_id: None,
                        ordinal,
                        status: if pending {
                            OrchestrationV2TurnItemStatus::Waiting
                        } else if *secret_status == OrchestrationV2SecretRequestStatus::Saved {
                            OrchestrationV2TurnItemStatus::Completed
                        } else {
                            OrchestrationV2TurnItemStatus::Cancelled
                        },
                        title: Some(label.to_string()),
                        started_at: Some(
                            existing_common
                                .and_then(|common| common.started_at)
                                .unwrap_or(now),
                        ),
                        completed_at: (!pending).then_some(now),
                        updated_at: now,
                        label: label.clone(),
                        reason: reason.clone(),
                        placeholder: placeholder.clone(),
                        secret_status: *secret_status,
                    }
                }
            };
            let id = decision.event_id(thread_id);
            decision.emit(OrchestrationV2DomainEvent::TurnItemUpdated {
                id,
                thread_id: thread_id.clone(),
                run_id: Some(run_id.clone()),
                node_id: Some(node_id.clone()),
                driver: None,
                provider_instance_id: Some(run.provider_instance_id.clone()),
                raw_event_id: None,
                occurred_at: now,
                payload,
            });
        }
        Client(C::ProviderSessionDetach {
            thread_id,
            provider_session_id,
            reason,
            ..
        }) => {
            detach_session(
                orchestrator,
                command,
                decision,
                thread_id,
                provider_session_id,
                reason.as_deref(),
            )
            .await?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

async fn detach_session(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    thread_id: &ThreadId,
    provider_session_id: &ProviderSessionId,
    reason: Option<&str>,
) -> Result<(), OrchestratorError> {
    let projection = orchestrator
        .get_thread_records(
            thread_id,
            &[RecordField::ProviderSessions],
            &RecordFilter::default(),
        )
        .await?;
    let Some(session) = projection
        .provider_sessions
        .iter()
        .find(|candidate| &candidate.id == provider_session_id)
    else {
        return Err(dispatch_failure(
            command,
            format!(
                "Provider session {provider_session_id} does not belong to thread {thread_id}."
            ),
        ));
    };
    let now = decision.now();
    let id = decision.event_id(thread_id);
    decision.emit(OrchestrationV2DomainEvent::ProviderSessionDetached {
        id,
        thread_id: thread_id.clone(),
        run_id: None,
        node_id: None,
        driver: Some(session.driver.clone()),
        provider_instance_id: Some(session.provider_instance_id.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: OrchestrationV2ProviderSessionDetached {
            provider_session_id: session.id.clone(),
            detached_at: now,
            reason: reason.map(str::to_owned),
        },
    });
    decision.effects.push(PendingEffect {
        id: format!(
            "effect:{}:provider-session.detach:{provider_session_id}",
            command_id(command)
        ),
        command_id: command_id(command).clone(),
        thread_id: thread_id.clone(),
        request: EffectRequest::ProviderSessionDetach {
            provider_session_id: provider_session_id.clone(),
            detail: reason.map(str::to_owned),
            revoke_mcp_credential: None,
        },
        available_at: None,
    });
    Ok(())
}
