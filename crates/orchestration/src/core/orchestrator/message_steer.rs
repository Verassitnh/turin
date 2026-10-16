//! Port of upstream/apps/server/src/orchestration-v2/Orchestrator.ts
//! `dispatchSteerIntoRun` (line 3835): a message sent into a run that is
//! already running either joins its turn (active steering) or interrupts the
//! turn and restarts the run on a new attempt (`steer_active` and
//! `restart_active`, and `queued-message.promote-to-steer`).
//!

use turin_contracts::{
    ProviderSessionId, RunId,
    chat_attachment::ChatAttachment,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2ContextHandoff, OrchestrationV2ContextTransfer,
        OrchestrationV2ContextTransferResolution, OrchestrationV2ContextTransferStatus,
        OrchestrationV2ContextTransferType, OrchestrationV2ConversationMessage,
        OrchestrationV2CoveredRunOrdinals, OrchestrationV2DomainEvent,
        OrchestrationV2ExecutionNode, OrchestrationV2ExecutionNodeStatus,
        OrchestrationV2ProviderThread, OrchestrationV2ProviderThreadStatus,
        OrchestrationV2ProviderTurnStatus, OrchestrationV2Run, OrchestrationV2RunAttempt,
        OrchestrationV2RunAttemptReason, OrchestrationV2RunAttemptStatus, OrchestrationV2RunStatus,
        OrchestrationV2ServerCommand, OrchestrationV2ThreadProjection, OrchestrationV2TurnItem,
        OrchestrationV2TurnItemStatus, OrchestrationV2UserMessageInputIntent,
    },
};
use turin_provider_core::{
    id_allocator::{IdAllocator, derive as ids},
    notification::{NotificationMessage, notification_turn_item},
    selection_transition::{ProviderSelectionTransitionInput, ProviderSelectionTransitionPlan},
};

use super::Orchestrator;
use super::dispatch::{Decision, command_id, dispatch_failure};
use super::message_dispatch::{
    emit_thread_event, is_native_maintenance_command, next_turn_item_ordinal, prepare_root_scope,
    provider_session_id_for, root_provider_threads_for_provider, run_scoped_event,
};
use super::message_transfer::read_handoff_items;
use super::thread_fork::context_source_point_for_run;
use crate::core::context_handoff_service::{
    ContextHandoffService, PrepareProviderHandoffInput as ProviderHandoffInput,
    ProviderHandoffStrategy,
};
use crate::core::provider_session_transition_policy::{
    ProviderSessionTransition, model_selections_equal,
};
use crate::pipeline::command_policy::{
    CapabilityCheck, HandoffStrategy, SteeringExecutionPolicy, decide_steering_execution,
    ensure_context_handoff,
};
use crate::pipeline::effect_outbox::{EffectRequest, PendingEffect, SessionTransition};
use crate::pipeline::orchestrator_port::OrchestratorError;

/// What `dispatchSteerIntoRun` is given beyond the projection. `message` is
/// the user message as the command carries it; its run and node are set here
/// to the run the steer lands in.
pub(super) struct SteerInput {
    pub message: OrchestrationV2ConversationMessage,
    pub model_selection: ModelSelection,
    pub target_run_id: RunId,
    pub force_restart: bool,
    /// `queued-message.promote-to-steer` (tags the turn item's input intent).
    pub promoted: bool,
}

/// A native `/goal` command. It changes the provider's goal, so it never
/// steers a running turn (`isGoalCommand`).
fn is_goal_command(text: &str, attachments: &[ChatAttachment]) -> bool {
    let text = text.trim();
    attachments.is_empty()
        && text
            .strip_prefix("/goal")
            .is_some_and(|rest| rest.chars().next().is_none_or(char::is_whitespace))
}

fn status_text<T: serde::Serialize>(status: &T) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

pub(super) async fn steer_into_run(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    projection: &OrchestrationV2ThreadProjection,
    input: SteerInput,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    let thread = &projection.thread;
    let target_run_id = &input.target_run_id;
    let Some(target_run) = projection.runs.iter().find(|run| &run.id == target_run_id) else {
        return Err(dispatch_failure(
            command,
            format!("Target run {target_run_id} was not found."),
        ));
    };
    let text = &input.message.text;
    let attachments = &input.message.attachments;
    if is_native_maintenance_command(text, attachments) {
        return Err(dispatch_failure(
            command,
            if text.trim().to_lowercase() == "/compact" {
                "Context compaction must run as a separate turn. Queue it or wait for the active turn to finish."
            } else {
                "Signing out must run as a separate turn. Queue it or wait for the active turn to finish."
            },
        ));
    }
    if is_goal_command(text, attachments) {
        return Err(dispatch_failure(
            command,
            "Goal commands must run as a separate turn. Queue it or wait for the active turn to finish.",
        ));
    }
    if let Some(target_message) = projection
        .messages
        .iter()
        .find(|message| message.id == target_run.user_message_id)
        && is_native_maintenance_command(&target_message.text, &target_message.attachments)
    {
        return Err(dispatch_failure(
            command,
            if target_message.text.trim().to_lowercase() == "/compact" {
                "Wait for context compaction to finish before steering the thread."
            } else {
                "Wait for sign-out to finish before steering the thread."
            },
        ));
    }
    let Some(root_node_id) = target_run.root_node_id.clone() else {
        return Err(dispatch_failure(
            command,
            format!("Target run {} has no root node.", target_run.id),
        ));
    };
    if target_run.status != OrchestrationV2RunStatus::Running {
        return Err(dispatch_failure(
            command,
            format!(
                "Target run {} is {} and cannot be steered.",
                target_run.id,
                status_text(&target_run.status)
            ),
        ));
    }
    let provider_thread = projection
        .provider_threads
        .iter()
        .find(|candidate| Some(&candidate.id) == target_run.provider_thread_id.as_ref());
    let Some((provider_thread, provider_session_id)) = provider_thread.and_then(|thread| {
        thread
            .provider_session_id
            .clone()
            .map(|session_id| (thread, session_id))
    }) else {
        return Err(dispatch_failure(
            command,
            format!(
                "Provider thread {} has no active provider session for steering.",
                target_run
                    .provider_thread_id
                    .as_ref()
                    .map_or_else(|| "null".to_owned(), ToString::to_string)
            ),
        ));
    };
    let Some(provider_turn) = projection.provider_turns.iter().find(|candidate| {
        candidate.run_attempt_id.is_some()
            && candidate.run_attempt_id == target_run.active_attempt_id
            && candidate.status == OrchestrationV2ProviderTurnStatus::Running
    }) else {
        return Err(dispatch_failure(
            command,
            format!(
                "No running provider turn found for active run {}.",
                target_run.id
            ),
        ));
    };
    let Some(session) = orchestrator
        .seams
        .sessions
        .get_session(&provider_session_id)
        .await
        .map_err(|cause| dispatch_failure(command, cause.to_string()))?
    else {
        return Err(dispatch_failure(
            command,
            format!("Provider session {provider_session_id} is not active."),
        ));
    };
    let session_capabilities = session.provider_session().capabilities;

    let now = decision.now();
    let selection_changed =
        !model_selections_equal(&target_run.model_selection, &input.model_selection);
    let provider_instance_changed =
        target_run.provider_instance_id != input.model_selection.instance_id;
    let selection_transition = if selection_changed && !provider_instance_changed {
        let adapter = orchestrator
            .seams
            .adapters
            .adapter(&target_run.provider_instance_id)
            .ok_or_else(|| {
                dispatch_failure(
                    command,
                    format!(
                        "Provider instance {} is not available.",
                        target_run.provider_instance_id
                    ),
                )
            })?;
        let plan = adapter
            .plan_selection_transition(ProviderSelectionTransitionInput {
                current: target_run.model_selection.clone(),
                target: input.model_selection.clone(),
                session_capabilities,
            })
            .await
            .map_err(|cause| OrchestratorError::ProviderAdapter {
                command_id: command_id(command).clone(),
                provider_instance_id: target_run.provider_instance_id.clone(),
                cause: Box::new(cause),
            })?;
        if let ProviderSelectionTransitionPlan::Reject { reason } = &plan {
            return Err(dispatch_failure(command, reason.clone()));
        }
        Some(plan)
    } else {
        None
    };

    // A selection the provider applies on its next turn restarts the run when
    // the provider can restart it. Otherwise the steer joins the running turn
    // and the selection waits for the next one, rather than failing the steer.
    let turns = &session_capabilities.turns;
    let selection_must_apply_now = selection_changed
        && (provider_instance_changed
            || selection_transition != Some(ProviderSelectionTransitionPlan::ApplyOnNextTurn));
    let steering_policy = decide_steering_execution(
        &CapabilityCheck {
            command_id: command_id(command),
            thread_id: &thread.id,
            provider_instance_id: &target_run.provider_instance_id,
            capabilities: &session_capabilities,
        },
        input.force_restart
            || selection_must_apply_now
            || (selection_changed
                && turns.supports_interrupt
                && turns.supports_steering_by_interrupt_restart),
    )
    .map_err(|cause| dispatch_failure(command, cause.to_string()))?;

    if steering_policy == SteeringExecutionPolicy::ActiveSteering {
        // The steer's selection becomes the saved next-turn choice, even when
        // it matches the running run again. A delegated completion carries the
        // run's selection, not a user choice, so it never replaces the saved
        // one. The saved choice may name another instance, so it moves with
        // the steer.
        let instance_changed = thread.provider_instance_id != input.model_selection.instance_id;
        if input.message.delegated_completion.is_none()
            && (instance_changed
                || !model_selections_equal(&thread.model_selection, &input.model_selection))
        {
            let mut updated = thread.clone();
            updated.provider_instance_id = input.model_selection.instance_id.clone();
            updated.model_selection = input.model_selection.clone();
            updated.updated_at = now;
            emit_thread_event(
                decision,
                if instance_changed {
                    "provider-switched"
                } else {
                    "model-selection-updated"
                },
                &updated,
                now,
            );
        }
        append_steering_message(
            orchestrator,
            projection,
            &input,
            decision,
            Landing {
                run_id: &target_run.id,
                node_id: &root_node_id,
                provider_turn_id: Some(provider_turn.id.clone()),
                provider_thread_id: &provider_thread.id,
                provider_instance_id: &target_run.provider_instance_id,
            },
        )
        .await?;
        decision.effects.push(PendingEffect {
            id: format!(
                "effect:{}:provider-turn.steer:{}",
                command_id(command),
                provider_turn.id
            ),
            command_id: command_id(command).clone(),
            thread_id: thread.id.clone(),
            request: EffectRequest::ProviderTurnSteer {
                provider_session_id,
                provider_thread_id: provider_thread.id.clone(),
                provider_turn_id: provider_turn.id.clone(),
                message_id: input.message.id.clone(),
            },
            available_at: None,
        });
        return Ok(());
    }

    let current_attempt = projection
        .attempts
        .iter()
        .find(|candidate| Some(&candidate.id) == target_run.active_attempt_id.as_ref());
    let current_root_node = projection
        .nodes
        .iter()
        .find(|candidate| candidate.id == root_node_id);
    let attempt_ordinal = projection
        .attempts
        .iter()
        .filter(|candidate| candidate.run_id == target_run.id)
        .map(|candidate| candidate.attempt_ordinal.get())
        .max()
        .unwrap_or(0)
        + 1;
    let next_attempt_id = ids::run_attempt(&target_run.id, attempt_ordinal);
    let next_root_node_id = ids::root_node_attempt(&target_run.id, attempt_ordinal);

    let can_resume_across_instances =
        if provider_instance_changed && provider_thread.native_thread_ref.is_some() {
            let mut planning = projection.clone();
            planning.thread.model_selection = target_run.model_selection.clone();
            let plan = orchestrator
                .seams
                .switch_planner
                .plan(&planning, &input.model_selection)
                .await
                .map_err(|cause| dispatch_failure(command, cause.to_string()))?;
            plan.transition == ProviderSessionTransition::RestartAndResume
        } else {
            false
        };
    let requires_provider_thread_handoff = (provider_instance_changed
        && !can_resume_across_instances)
        || selection_transition == Some(ProviderSelectionTransitionPlan::CreateWithHandoff);
    let requires_provider_session_restart = can_resume_across_instances
        || selection_transition == Some(ProviderSelectionTransitionPlan::RestartSession);
    let mut restart_provider_thread: OrchestrationV2ProviderThread = provider_thread.clone();
    let mut restart_session_transition: Option<SessionTransition> = None;
    let mut restart_handoff: Option<OrchestrationV2ContextHandoff> = None;
    let mut restart_transfer: Option<OrchestrationV2ContextTransfer> = None;
    if requires_provider_thread_handoff {
        let target_adapter = orchestrator
            .seams
            .adapters
            .adapter(&input.model_selection.instance_id)
            .ok_or_else(|| {
                dispatch_failure(
                    command,
                    format!(
                        "Provider instance {} is not available.",
                        input.model_selection.instance_id
                    ),
                )
            })?;
        let target_capabilities = target_adapter.get_capabilities().await.map_err(|cause| {
            OrchestratorError::ProviderAdapter {
                command_id: command_id(command).clone(),
                provider_instance_id: input.model_selection.instance_id.clone(),
                cause: Box::new(cause),
            }
        })?;
        ensure_context_handoff(
            &CapabilityCheck {
                command_id: command_id(command),
                thread_id: &thread.id,
                provider_instance_id: &input.model_selection.instance_id,
                capabilities: &target_capabilities,
            },
            HandoffStrategy::FullThreadSummary,
        )
        .map_err(|cause| dispatch_failure(command, cause.to_string()))?;
        let existing_target_provider_thread =
            root_provider_threads_for_provider(projection, &input.model_selection.instance_id)
                .into_iter()
                .find(|candidate| candidate.id != provider_thread.id);
        let target_provider_session_id = match existing_target_provider_thread
            .as_ref()
            .and_then(|row| row.provider_session_id.clone())
        {
            Some(existing) => existing,
            None => {
                provider_session_id_for(
                    target_adapter.as_ref(),
                    &input.model_selection.instance_id,
                    &thread.id,
                    command,
                )
                .await?
            }
        };
        let target_base = match existing_target_provider_thread {
            None => OrchestrationV2ProviderThread {
                id: ids::provider_thread(
                    target_adapter.driver(),
                    None,
                    &format!("pending:{}:attempt:{attempt_ordinal}", target_run.id),
                ),
                driver: target_adapter.driver().clone(),
                provider_instance_id: input.model_selection.instance_id.clone(),
                provider_session_id: Some(target_provider_session_id),
                app_thread_id: Some(thread.id.clone()),
                owner_node_id: None,
                native_thread_ref: None,
                native_conversation_head_ref: None,
                status: OrchestrationV2ProviderThreadStatus::NotLoaded,
                first_run_ordinal: Some(target_run.ordinal),
                last_run_ordinal: Some(target_run.ordinal),
                handoff_ids: Vec::new(),
                forked_from: None,
                pending_background_tasks: Vec::new(),
                context_usage: None,
                native_metadata: None,
                goal: None,
                created_at: now,
                updated_at: now,
            },
            Some(existing) => OrchestrationV2ProviderThread {
                provider_session_id: Some(target_provider_session_id),
                last_run_ordinal: Some(target_run.ordinal),
                updated_at: now,
                ..existing
            },
        };
        let transfer_id = IdAllocator.context_transfer(&thread.id, &thread.id, "provider_handoff");
        let items = read_handoff_items(orchestrator, &thread.id, None).await?;
        let handoff =
            ContextHandoffService::default().prepare_provider_handoff(&ProviderHandoffInput {
                thread_id: &thread.id,
                target_run_id: &target_run.id,
                transfer_id: Some(transfer_id.clone()),
                from_provider_thread_ids: std::slice::from_ref(&provider_thread.id),
                to_provider_thread_id: &target_base.id,
                from_provider_instance_id: &target_run.provider_instance_id,
                to_provider_instance_id: &input.model_selection.instance_id,
                covered_run_ordinals: OrchestrationV2CoveredRunOrdinals {
                    from: turin_contracts::PositiveInt::new(1).expect("1 is positive"),
                    to: target_run.ordinal,
                },
                runs: Some(&projection.runs),
                strategy: ProviderHandoffStrategy::FullThreadSummary,
                items: &items,
                created_at: now,
            });
        restart_provider_thread = {
            let mut handoff_ids = target_base.handoff_ids.clone();
            if !handoff_ids.contains(&handoff.id) {
                handoff_ids.push(handoff.id.clone());
            }
            OrchestrationV2ProviderThread {
                handoff_ids,
                ..target_base
            }
        };
        restart_transfer = Some(OrchestrationV2ContextTransfer {
            id: transfer_id,
            r#type: OrchestrationV2ContextTransferType::ProviderHandoff,
            source_thread_id: thread.id.clone(),
            target_thread_id: thread.id.clone(),
            source_point: context_source_point_for_run(projection, target_run),
            base_point: None,
            source_provider_instance_id: Some(target_run.provider_instance_id.clone()),
            target_provider_instance_id: Some(input.model_selection.instance_id.clone()),
            target_run_id: Some(target_run.id.clone()),
            status: OrchestrationV2ContextTransferStatus::Consumed,
            resolution: Some(OrchestrationV2ContextTransferResolution::PortableContext {
                context_handoff_id: handoff.id.clone(),
            }),
            created_by: input.message.created_by,
            error: None,
            created_at: now,
            updated_at: now,
            consumed_at: Some(now),
        });
        restart_handoff = Some(handoff);
        restart_session_transition = Some(SessionTransition::Detach);
    } else if requires_provider_session_restart {
        let next_provider_session_id: ProviderSessionId =
            IdAllocator.provider_session(&input.model_selection.instance_id, &thread.id);
        restart_provider_thread = OrchestrationV2ProviderThread {
            provider_instance_id: input.model_selection.instance_id.clone(),
            provider_session_id: Some(next_provider_session_id.clone()),
            status: OrchestrationV2ProviderThreadStatus::NotLoaded,
            updated_at: now,
            ..provider_thread.clone()
        };
        restart_session_transition = Some(SessionTransition::Replace {
            replacement_provider_session_id: next_provider_session_id,
        });
    }

    let checkpoint_scope = prepare_root_scope(
        orchestrator,
        command,
        projection,
        &input.model_selection,
        None,
        &target_run.id,
        &next_root_node_id,
        &restart_provider_thread.id,
        now,
    )
    .await?;
    let ensured_scope = orchestrator
        .seams
        .checkpoints
        .ensure_scope(checkpoint_scope)
        .await
        .map_err(|cause| dispatch_failure(command, cause.to_string()))?;

    let restarted_run = OrchestrationV2Run {
        provider_instance_id: input.model_selection.instance_id.clone(),
        model_selection: input.model_selection.clone(),
        provider_thread_id: Some(restart_provider_thread.id.clone()),
        root_node_id: Some(next_root_node_id.clone()),
        active_attempt_id: Some(next_attempt_id.clone()),
        user_message_id: input.message.id.clone(),
        status: OrchestrationV2RunStatus::Starting,
        context_handoff_id: restart_handoff
            .as_ref()
            .map(|handoff| handoff.id.clone())
            .or_else(|| target_run.context_handoff_id.clone()),
        ..target_run.clone()
    };
    let next_attempt = OrchestrationV2RunAttempt {
        id: next_attempt_id,
        native_thread_id: None,
        run_id: target_run.id.clone(),
        attempt_ordinal: turin_contracts::PositiveInt::new(attempt_ordinal)
            .expect("attempt ordinals start at 1"),
        root_node_id: next_root_node_id.clone(),
        provider_instance_id: input.model_selection.instance_id.clone(),
        provider_thread_id: restart_provider_thread.id.clone(),
        provider_turn_id: None,
        reason: OrchestrationV2RunAttemptReason::SteeringRestart,
        status: OrchestrationV2RunAttemptStatus::Pending,
        started_at: None,
        completed_at: None,
    };
    let next_root_node = OrchestrationV2ExecutionNode {
        id: next_root_node_id.clone(),
        thread_id: thread.id.clone(),
        run_id: Some(target_run.id.clone()),
        parent_node_id: None,
        root_node_id: next_root_node_id.clone(),
        kind: turin_contracts::orchestration_v2::OrchestrationV2ExecutionNodeKind::RootTurn,
        status: OrchestrationV2ExecutionNodeStatus::Pending,
        counts_for_run: true,
        provider_thread_id: Some(restart_provider_thread.id.clone()),
        provider_turn_id: None,
        native_item_ref: None,
        runtime_request_id: None,
        checkpoint_scope_id: Some(ensured_scope.id.clone()),
        started_at: None,
        completed_at: None,
    };

    let instance = &target_run.provider_instance_id;
    if let Some(attempt) = current_attempt {
        run_scoped_event!(
            decision,
            RunAttemptUpdated,
            &thread.id,
            &target_run.id,
            &root_node_id,
            instance,
            OrchestrationV2RunAttempt {
                status: OrchestrationV2RunAttemptStatus::Superseded,
                completed_at: Some(now),
                ..attempt.clone()
            }
        );
    }
    if let Some(node) = current_root_node {
        run_scoped_event!(
            decision,
            NodeUpdated,
            &thread.id,
            &target_run.id,
            &root_node_id,
            instance,
            OrchestrationV2ExecutionNode {
                status: OrchestrationV2ExecutionNodeStatus::Interrupted,
                completed_at: Some(now),
                ..node.clone()
            }
        );
    }
    let new_instance = &input.model_selection.instance_id;
    if selection_changed {
        let mut updated = thread.clone();
        updated.provider_instance_id = new_instance.clone();
        updated.model_selection = input.model_selection.clone();
        updated.updated_at = now;
        emit_thread_event(
            decision,
            if provider_instance_changed {
                "provider-switched"
            } else {
                "model-selection-updated"
            },
            &updated,
            now,
        );
    }
    if requires_provider_thread_handoff || requires_provider_session_restart {
        let id = decision.event_id(&thread.id);
        decision.emit(OrchestrationV2DomainEvent::ProviderThreadUpdated {
            id,
            thread_id: thread.id.clone(),
            run_id: None,
            node_id: None,
            driver: Some(restart_provider_thread.driver.clone()),
            provider_instance_id: Some(new_instance.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: restart_provider_thread.clone(),
        });
    }
    if let Some(handoff) = restart_handoff {
        run_scoped_event!(
            decision,
            ContextHandoffUpdated,
            &thread.id,
            &target_run.id,
            &next_root_node_id,
            new_instance,
            handoff
        );
    }
    if let Some(transfer) = restart_transfer {
        run_scoped_event!(
            decision,
            ContextTransferCreated,
            &thread.id,
            &target_run.id,
            &next_root_node_id,
            new_instance,
            transfer
        );
    }
    run_scoped_event!(
        decision,
        RunUpdated,
        &thread.id,
        &target_run.id,
        &next_root_node_id,
        new_instance,
        restarted_run
    );
    run_scoped_event!(
        decision,
        RunAttemptCreated,
        &thread.id,
        &target_run.id,
        &next_root_node_id,
        new_instance,
        next_attempt
    );
    run_scoped_event!(
        decision,
        NodeUpdated,
        &thread.id,
        &target_run.id,
        &next_root_node_id,
        new_instance,
        next_root_node
    );
    run_scoped_event!(
        decision,
        CheckpointScopeCreated,
        &thread.id,
        &target_run.id,
        &next_root_node_id,
        new_instance,
        ensured_scope
    );
    append_steering_message(
        orchestrator,
        projection,
        &input,
        decision,
        Landing {
            run_id: &target_run.id,
            node_id: &next_root_node_id,
            provider_turn_id: None,
            provider_thread_id: &restart_provider_thread.id,
            provider_instance_id: new_instance,
        },
    )
    .await?;
    let Some(interrupted_attempt_id) = target_run.active_attempt_id.clone() else {
        return Err(dispatch_failure(
            command,
            format!("Active run {} has no attempt to interrupt.", target_run.id),
        ));
    };
    decision.effects.push(PendingEffect {
        id: format!(
            "effect:{}:provider-turn.restart:{}",
            command_id(command),
            provider_turn.id
        ),
        command_id: command_id(command).clone(),
        thread_id: thread.id.clone(),
        request: EffectRequest::ProviderTurnRestart {
            provider_session_id,
            provider_thread_id: provider_thread.id.clone(),
            provider_turn_id: provider_turn.id.clone(),
            interrupted_attempt_id,
            run_id: target_run.id.clone(),
            session_transition: restart_session_transition,
        },
        available_at: None,
    });
    Ok(())
}

/// Where the steering message lands: the run and node it joins (the target's
/// own for an active steer, the restarted attempt's for a restart).
struct Landing<'a> {
    run_id: &'a RunId,
    node_id: &'a turin_contracts::NodeId,
    provider_turn_id: Option<turin_contracts::ProviderTurnId>,
    provider_thread_id: &'a turin_contracts::ProviderThreadId,
    provider_instance_id: &'a turin_contracts::provider_instance::ProviderInstanceId,
}

/// `appendSteeringMessage`: the user message and its turn item.
async fn append_steering_message(
    orchestrator: &Orchestrator,
    projection: &OrchestrationV2ThreadProjection,
    input: &SteerInput,
    decision: &mut Decision,
    landing: Landing<'_>,
) -> Result<(), OrchestratorError> {
    let thread_id = &projection.thread.id;
    let now = decision.now();
    let mut message = input.message.clone();
    message.run_id = Some(landing.run_id.clone());
    message.node_id = Some(landing.node_id.clone());
    message.notification = None;
    message.streaming = false;
    message.created_at = now;
    message.updated_at = now;
    let ordinal = next_turn_item_ordinal(orchestrator, projection).await?;
    let user_item = OrchestrationV2TurnItem::UserMessage {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: ids::user_turn_item(&message.id),
        thread_id: thread_id.clone(),
        run_id: Some(landing.run_id.clone()),
        node_id: Some(landing.node_id.clone()),
        provider_thread_id: Some(landing.provider_thread_id.clone()),
        provider_turn_id: landing.provider_turn_id,
        native_item_ref: None,
        parent_item_id: None,
        ordinal,
        status: OrchestrationV2TurnItemStatus::Completed,
        title: None,
        started_at: Some(now),
        completed_at: Some(now),
        updated_at: now,
        created_by: message.created_by,
        creation_source: message.creation_source,
        message_id: message.id.clone(),
        scheduled_task_id: message.scheduled_task_id.clone(),
        sender_thread_id: message.sender_thread_id.clone(),
        input_intent: if input.promoted {
            OrchestrationV2UserMessageInputIntent::PromotedQueuedToSteer
        } else {
            OrchestrationV2UserMessageInputIntent::Steer
        },
        text: message.text.clone(),
        context: message.context.clone(),
        attachments: message.attachments.clone(),
    };
    let item = notification_turn_item(
        &user_item,
        NotificationMessage::from(&message),
        &projection.subagents,
    );
    run_scoped_event!(
        decision,
        MessageUpdated,
        thread_id,
        landing.run_id,
        landing.node_id,
        landing.provider_instance_id,
        message
    );
    run_scoped_event!(
        decision,
        TurnItemUpdated,
        thread_id,
        landing.run_id,
        landing.node_id,
        landing.provider_instance_id,
        item
    );
    Ok(())
}
