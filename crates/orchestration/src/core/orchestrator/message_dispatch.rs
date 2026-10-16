//! Port of upstream/apps/server/src/orchestration-v2/Orchestrator.ts `dispatchMessage`
//! (`message.dispatch`, around line 4568).
//!
//! Slices in this file (see docs/handoff/T13c.md, item F):
//! * guards and pre-steps: manual and usage-limit continuation, unsettle and
//!   unsnooze before a new turn, the title seed, the notification guard;
//! * queued runs: a message that waits behind a blocking run (or a worktree
//!   that is missing after a failed setup) becomes a `queued` run;
//! * start: a new `starting` run with its provider turn effect, or a
//!   `preparing` run for `defer_start` that waits for its workspace.
//!
//! * steer and restart into the active run: `message_steer.rs`.
//!
//! * delegated-completion deliveries (wake text, mailbox steer routing) and
//!   source-plan starts.
//!
//! * provider switches, pending fork and merge-back transfers: `message_transfer.rs`.
//!
//! Refused until its slice lands: restart continuation (`continueRestartedRun`, T13e).

use turin_contracts::{
    NodeId, PositiveInt, ProviderThreadId, RunId, ThreadId, UtcDateTime,
    chat_attachment::ChatAttachment,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2AppThread, OrchestrationV2CheckpointScope,
        OrchestrationV2Command as C, OrchestrationV2CompletionWake,
        OrchestrationV2ContextTransferStatus, OrchestrationV2ContextTransferType,
        OrchestrationV2ConversationMessage, OrchestrationV2ConversationMessageRole,
        OrchestrationV2CreationSource, OrchestrationV2DelegatedCompletionCohortDisposition,
        OrchestrationV2DeliveryIntent, OrchestrationV2DispatchMode, OrchestrationV2DomainEvent,
        OrchestrationV2ExecutionNode, OrchestrationV2ExecutionNodeKind,
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2LimitRecovery,
        OrchestrationV2MessageDelegatedCompletion, OrchestrationV2PlanArtifact,
        OrchestrationV2PlanArtifactStatus, OrchestrationV2ProviderFailureClass,
        OrchestrationV2ProviderThread, OrchestrationV2ProviderThreadStatus,
        OrchestrationV2ProviderTurnStatus, OrchestrationV2Run, OrchestrationV2RunAttempt,
        OrchestrationV2RunAttemptReason, OrchestrationV2RunAttemptStatus, OrchestrationV2RunStatus,
        OrchestrationV2RuntimeRequestStatus, OrchestrationV2ServerCommand,
        OrchestrationV2SettleOverride, OrchestrationV2SourcePlanRef,
        OrchestrationV2ThreadHistoryOrigin, OrchestrationV2ThreadLaunchWorkspaceStrategy,
        OrchestrationV2ThreadProjection, OrchestrationV2TitleRegeneration, OrchestrationV2TurnItem,
        OrchestrationV2TurnItemStatus, OrchestrationV2UserMessageInputIntent,
        latest_provider_turn_for_attempt, orchestration_v2_run_work_started_at,
    },
    provider_instance::ProviderInstanceId,
};
use turin_provider_core::{
    id_allocator::{IdAllocator, derive as ids},
    notification::{NotificationMessage, notification_turn_item},
    provider_adapter::ProviderAdapterV2,
};

use crate::pipeline::command_policy::{
    CapabilityCheck, DeliveryIntent, RequestedDispatchMode, ensure_queued_messages,
    resolve_message_dispatch_intent,
};
use crate::pipeline::effect_outbox::{EffectRequest, PendingEffect, ThreadTitleKind};
use crate::pipeline::notification_mailbox::is_undelivered_mailbox_steer;
use crate::pipeline::orchestrator_port::OrchestratorError;
use crate::pipeline::runtime_policy::{self, RuntimePolicyOverride};
use turin_shared::orchestration_v2_thread_error::{
    latest_executed_run, latest_root_provider_failure, run_ran_after, usage_limit_blocked_run,
};

use super::Orchestrator;
use super::dispatch::{Decision, command_id, dispatch_failure};
use super::message_steer::{SteerInput, steer_into_run};
use super::message_transfer::{
    read_handoff_items, should_prepare_legacy_import_handoff, start_run_with_transfers,
};
use super::run_stop::stop_reached_run;
use super::services::PrepareRootRunScope;
use crate::core::context_handoff_service::{
    ContextHandoffService, PrepareLegacyImportInput as LegacyImportInput,
};
use crate::core::restart_background_note::{ProviderTurnState, is_restart_continuation_source};

/// The workspace-preparation placeholder, upstream `WORKSPACE_PREPARATION_INPUT`.
pub(super) const WORKSPACE_PREPARATION_INPUT: &str = "Preparing workspace";

/// Emits one run-scoped event. Every such event carries the same envelope.
macro_rules! run_scoped_event {
    ($decision:expr, $variant:ident, $thread:expr, $run:expr, $node:expr, $instance:expr, $payload:expr) => {{
        let id = $decision.event_id($thread);
        let occurred_at = $decision.now();
        $decision.emit(
            turin_contracts::orchestration_v2::OrchestrationV2DomainEvent::$variant {
                id,
                thread_id: $thread.clone(),
                run_id: Some($run.clone()),
                node_id: Some($node.clone()),
                driver: None,
                provider_instance_id: Some($instance.clone()),
                raw_event_id: None,
                occurred_at,
                payload: $payload,
            },
        );
    }};
}

pub(super) use run_scoped_event;

/// Decides `message.dispatch`. `Ok(false)` leaves the command to another group.
pub(super) async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    let OrchestrationV2ServerCommand::Client(client @ C::MessageDispatch { .. }) = command else {
        return Ok(false);
    };
    let C::MessageDispatch {
        command_id,
        thread_id,
        ..
    } = client
    else {
        unreachable!("matched above");
    };
    // A provider's own subagent takes no messages: the provider runs it.
    let thread = orchestrator
        .projection
        .get_thread(thread_id)
        .await
        .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;
    if turin_contracts::orchestration_v2::is_provider_native_subagent_thread(&thread) {
        return Err(OrchestratorError::SubagentThreadReadOnly {
            command_id: command_id.clone(),
            thread_id: thread_id.clone(),
        });
    }
    dispatch_message(orchestrator, command, client, decision).await?;
    super::subagent_followup::record(orchestrator, command, client, decision).await?;
    Ok(true)
}

/// `dispatchMessage`. Emits the decision's events and effects; the caller
/// commits them under the thread lock.
pub(super) async fn dispatch_message(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    client: &C,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    let C::MessageDispatch {
        notification,
        created_by,
        creation_source,
        thread_id,
        message_id,
        text,
        attachments,
        title_seed,
        model_selection,
        restart_continuation_of_run_id,
        usage_limit_continuation_of_run_id,
        manual_continuation_of_run_id,
        usage_limit_recovery_request_id,
        delivery_intent,
        dispatch_mode,
        ..
    } = client
    else {
        unreachable!("decide routes only message.dispatch here");
    };

    let mut projection = match orchestrator.read_command_projection(thread_id).await {
        Ok(projection) => projection,
        // A thread this command's own decision creates (a delegated child)
        // exists only as a pending event.
        Err(error) if super::super::orchestrator::is_thread_not_found(&error) => {
            pending_thread_projection(decision, thread_id).ok_or(error)?
        }
        Err(error) => return Err(error),
    };

    if let Some(source_id) = manual_continuation_of_run_id {
        let source = projection.runs.iter().find(|run| &run.id == source_id);
        let limited = latest_root_provider_failure(source, &projection.turn_items);
        let resumable = source.is_some_and(|source| {
            source.status == OrchestrationV2RunStatus::Interrupted
                || (source.status == OrchestrationV2RunStatus::Failed
                    && limited.is_some_and(|failure| {
                        failure.class == OrchestrationV2ProviderFailureClass::UsageLimit
                    }))
        });
        let latest_is_source = source.is_some_and(|source| {
            latest_executed_run(&projection.runs).map(|run| &run.id) == Some(&source.id)
        });
        if !matches!(dispatch_mode, OrchestrationV2DispatchMode::StartImmediately)
            || !resumable
            || !latest_is_source
            || projection.thread.archived_at.is_some()
            || projection.thread.deleted_at.is_some()
            || projection
                .runtime_requests
                .iter()
                .any(|request| request.status == OrchestrationV2RuntimeRequestStatus::Pending)
        {
            return Err(dispatch_failure(
                command,
                "This thread can no longer be resumed from that run.",
            ));
        }
    }

    if let Some(source_id) = usage_limit_continuation_of_run_id {
        let now = decision.now();
        let run = usage_limit_blocked_run(&projection.runs, &projection.turn_items, None);
        let failure = latest_root_provider_failure(run, &projection.turn_items);
        let recovery = projection.thread.limit_recovery.clone().flatten();
        let snoozed_past_now = projection
            .thread
            .snoozed_until
            .flatten()
            .is_some_and(|until| millis(until) > millis(now));
        let blocked = run.is_none_or(|run| &run.id != source_id)
            || failure.is_none_or(|failure| {
                failure.class != OrchestrationV2ProviderFailureClass::UsageLimit
            })
            || recovery.as_ref().is_none_or(|recovery| {
                !recovery.auto_resume
                    || Some(&recovery.run_id) != run.map(|run| &run.id)
                    || chrono::DateTime::parse_from_rfc3339(recovery.reset_at.as_str())
                        .is_ok_and(|reset| reset.timestamp_millis() > millis(now))
                    || recovery.request_id.as_ref() != usage_limit_recovery_request_id.as_ref()
                    || failure
                        .and_then(|failure| failure.reset_at.as_ref())
                        .and_then(Option::as_ref)
                        != Some(&recovery.reset_at)
            })
            || run.is_some_and(|run| {
                projection.thread.provider_instance_id != run.provider_instance_id
            })
            || projection.thread.archived_at.is_some()
            || projection.thread.deleted_at.is_some()
            || projection.thread.settled_override == Some(OrchestrationV2SettleOverride::Settled)
            || projection
                .runtime_requests
                .iter()
                .any(|request| request.status == OrchestrationV2RuntimeRequestStatus::Pending)
            || snoozed_past_now;
        if blocked {
            // A stale automatic delivery gets an accepted receipt and no work.
            // Upstream rewrites the row, and re-keys a still-snoozed recovery to
            // this command so the next delivery can match it.
            let mut thread = projection.thread.clone();
            if let Some(recovery) = recovery
                && recovery.auto_resume
                && recovery.request_id.as_ref() == usage_limit_recovery_request_id.as_ref()
                && &recovery.run_id == source_id
                && snoozed_past_now
            {
                thread.limit_recovery = Some(Some(OrchestrationV2LimitRecovery {
                    request_id: Some(command_id(command).clone()),
                    ..recovery
                }));
            }
            emit_thread_event(decision, "metadata-updated", &thread, now);
            return Ok(());
        }
    }

    if let Some(source_id) = restart_continuation_of_run_id {
        let turn_states: Vec<ProviderTurnState> =
            projection.provider_turns.iter().map(Into::into).collect();
        let source = projection.runs.iter().find(|run| &run.id == source_id);
        // Held queued runs never started; they wait behind the continuation.
        let stale = match source {
            None => true,
            Some(source) => {
                !is_restart_continuation_source(source, &turn_states)
                    || projection.thread.archived_at.is_some()
                    || projection.thread.deleted_at.is_some()
                    || projection.thread.provider_instance_id != source.provider_instance_id
                    || projection.runs.iter().any(|run| {
                        run.id != source.id
                            && run.status != OrchestrationV2RunStatus::Queued
                            && run_ran_after(run, source)
                    })
                    || stop_reached_run(orchestrator, command, thread_id, &source.id).await?
            }
        };
        if stale {
            // Preserve the current row so stale automatic deliveries receive an
            // accepted receipt without changing work or repeatedly retrying.
            let now = decision.now();
            emit_thread_event(decision, "metadata-updated", &projection.thread, now);
            return Ok(());
        }
    }

    // A new turn settles and un-snoozes the thread first, as upstream does.
    if let Some(override_) = projection.thread.settled_override {
        let now = decision.now();
        let mut thread = projection.thread.clone();
        thread.settled_override = None;
        thread.settled_at = None;
        thread.unsettled_at = if override_
            == turin_contracts::orchestration_v2::OrchestrationV2SettleOverride::Active
        {
            projection.thread.unsettled_at
        } else {
            Some(Some(now))
        };
        thread.updated_at = now;
        emit_thread_event(decision, "unsettled", &thread, now);
        projection.thread = thread;
    }
    // `snoozedUntil != null`: an explicit `null` (a fresh thread) is not a snooze.
    if projection.thread.snoozed_until.flatten().is_some() {
        let now = decision.now();
        let mut thread = projection.thread.clone();
        thread.snoozed_until = Some(None);
        thread.snoozed_at = Some(None);
        thread.updated_at = now;
        emit_thread_event(decision, "unsnoozed", &thread, now);
        projection.thread = thread;
    }

    let user_messages: Vec<_> = projection
        .messages
        .iter()
        .filter(|message| message.role == OrchestrationV2ConversationMessageRole::User)
        .collect();
    let only_maintenance_history = !user_messages.is_empty()
        && user_messages
            .iter()
            .all(|message| is_native_maintenance_command(&message.text, &message.attachments));
    let is_maintenance = is_native_maintenance_command(text, attachments);
    if !is_maintenance
        && ((title_seed.is_some()
            && orchestrator
                .projection
                .get_message_count(thread_id)
                .await
                .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?
                == 0)
            || (*created_by == OrchestrationV2Actor::User && only_maintenance_history))
    {
        let now = decision.now();
        let mut thread = projection.thread.clone();
        if let Some(seed) = title_seed {
            thread.title = seed.clone();
        }
        thread.title_regeneration = Some(Some(OrchestrationV2TitleRegeneration {
            request_id: command_id(command).clone(),
            started_at: now,
        }));
        thread.updated_at = now;
        emit_thread_event(decision, "metadata-updated", &thread, now);
        decision.effects.push(title_generation_effect(
            command_id(command),
            thread_id,
            ThreadTitleKind::Initial {
                message_id: message_id.clone(),
            },
        ));
        projection.thread = thread;
    }

    let requested_model = model_selection
        .clone()
        .unwrap_or_else(|| projection.thread.model_selection.clone());
    let dispatch = resolve_dispatch_mode(&projection, dispatch_mode, *delivery_intent);
    // A restart continuation resumes the interrupted turn, so it starts
    // immediately rather than queueing like other wakes.
    if notification.is_some()
        && (*created_by != OrchestrationV2Actor::Agent
            || !matches!(
                creation_source,
                turin_contracts::orchestration_v2::OrchestrationV2CreationSource::Server
                    | turin_contracts::orchestration_v2::OrchestrationV2CreationSource::Provider
            )
            || (dispatch != Dispatch::QueueAfterActive
                && !(restart_continuation_of_run_id.is_some()
                    && dispatch == Dispatch::Start(Start::Immediate))))
    {
        return Err(dispatch_failure(
            command,
            "Notifications must be server- or provider-created queued messages.",
        ));
    }
    let delegated = resolve_delegated_completion(command, client, &projection, &dispatch)?;
    // Route durable mailbox deliveries under the thread lock, using the live
    // session's capabilities. Never interrupt or restart a turn for a
    // notification.
    let dispatch = match &delegated {
        Some(completion) => {
            route_mailbox_steer(orchestrator, &projection, completion, dispatch).await
        }
        None => dispatch,
    };
    let text = match &delegated {
        Some(completion) => delegated_completion_wake_detail(&completion.task_ids),
        None => text.clone(),
    };
    let resolved = Resolved {
        continued_task_id: restart_continuation_of_run_id
            .as_ref()
            .and_then(|source_id| projection.runs.iter().find(|run| &run.id == source_id))
            .and_then(|source| source.delegated_task_id.clone()),
        text,
        delegated_completion: delegated,
        source_plan: resolve_source_plan(orchestrator, command, client, &projection).await?,
    };
    if let Dispatch::Steer {
        target_run_id,
        restart,
    } = &dispatch
    {
        // A delegated completion carries the run's selection, not a user choice.
        let model_selection = match (
            &resolved.delegated_completion,
            projection.runs.iter().find(|run| &run.id == target_run_id),
        ) {
            (Some(_), Some(target)) => target.model_selection.clone(),
            _ => requested_model,
        };
        let mut message = message_template(client, &projection.thread.id, decision.now());
        resolved.apply(&mut message);
        return steer_into_run(
            orchestrator,
            command,
            &projection,
            SteerInput {
                message,
                model_selection,
                target_run_id: target_run_id.clone(),
                force_restart: *restart,
                promoted: false,
            },
            decision,
        )
        .await;
    }

    let active_run = projection
        .runs
        .iter()
        .find(|run| is_blocking_run(run))
        .cloned();
    let latest_run = latest_executed_run(&projection.runs).cloned();
    // A message queued during worktree setup can arrive after that setup
    // failed without a worktree. It waits, held, instead of starting in the
    // project checkout.
    let worktree_missing_run = match (&active_run, &latest_run) {
        (None, Some(latest))
            if dispatch == Dispatch::QueueAfterActive
                && matches!(
                    latest.status,
                    OrchestrationV2RunStatus::Failed | OrchestrationV2RunStatus::Interrupted
                )
                && matches!(
                    latest.workspace_preparation,
                    Some(OrchestrationV2ThreadLaunchWorkspaceStrategy::Worktree { .. })
                )
                && projection.thread.worktree_path.is_none() =>
        {
            Some(latest.clone())
        }
        _ => None,
    };
    // Upstream's shouldQueue: any non-steer dispatch waits behind a blocking run.
    if let Some(queue_behind) = active_run.clone().or_else(|| worktree_missing_run.clone()) {
        return queue_run(
            orchestrator,
            command,
            &projection,
            QueueInput {
                client,
                queue_behind: &queue_behind,
                model_selection: &requested_model,
                worktree_missing: worktree_missing_run.is_some(),
                resolved: &resolved,
            },
            decision,
        )
        .await;
    }

    // A queue-after-active with nothing to wait behind starts like any other
    // message, as upstream does.
    let start = match dispatch {
        Dispatch::Start(start) => start,
        Dispatch::QueueAfterActive | Dispatch::Steer { .. } => Start::Immediate,
    };
    start_run(
        orchestrator,
        command,
        &projection,
        StartInput {
            client,
            requested_model: &requested_model,
            start,
            resolved: &resolved,
        },
        decision,
    )
    .await
}

/// What the delegated-completion and source-plan reads settle for one
/// dispatch: the message text (a delegated completion replaces it with a wake
/// detail), the delivery it carries, and the plan it completes.
pub(super) struct Resolved {
    /// The delegated task a restart continuation carries on
    /// (`continuedTaskId`).
    pub(super) continued_task_id: Option<NodeId>,
    text: String,
    delegated_completion: Option<OrchestrationV2MessageDelegatedCompletion>,
    source_plan: Option<OrchestrationV2PlanArtifact>,
}

impl Resolved {
    /// The command's message as this dispatch writes it.
    pub(super) fn apply(&self, message: &mut OrchestrationV2ConversationMessage) {
        message.text.clone_from(&self.text);
        message.delegated_completion = self.delegated_completion.clone();
    }

    /// `completeSourcePlan`: the plan a run was started from is done once the
    /// run exists.
    pub(super) fn complete_source_plan(&self, decision: &mut Decision, now: UtcDateTime) {
        let Some(OrchestrationV2PlanArtifact::ProposedPlan {
            id,
            thread_id,
            run_id,
            node_id,
            detail_in_turn_item,
            markdown,
            ..
        }) = &self.source_plan
        else {
            return;
        };
        let event_id = decision.event_id(thread_id);
        decision.emit(OrchestrationV2DomainEvent::PlanUpdated {
            id: event_id,
            thread_id: thread_id.clone(),
            run_id: run_id.clone(),
            node_id: Some(node_id.clone()),
            driver: None,
            provider_instance_id: None,
            raw_event_id: None,
            occurred_at: now,
            payload: OrchestrationV2PlanArtifact::ProposedPlan {
                id: id.clone(),
                thread_id: thread_id.clone(),
                run_id: run_id.clone(),
                node_id: node_id.clone(),
                status: OrchestrationV2PlanArtifactStatus::Completed,
                detail_in_turn_item: *detail_in_turn_item,
                markdown: markdown.clone(),
            },
        });
    }
}

pub(super) fn source_plan_ref_of(client: &C) -> Option<OrchestrationV2SourcePlanRef> {
    let C::MessageDispatch {
        source_plan_ref, ..
    } = client
    else {
        unreachable!("source_plan_ref_of is called only for message.dispatch");
    };
    source_plan_ref.clone()
}

/// The command's message id.
pub(super) fn message_id_of(client: &C) -> turin_contracts::MessageId {
    let C::MessageDispatch { message_id, .. } = client else {
        unreachable!("message_id_of is called only for message.dispatch");
    };
    message_id.clone()
}

/// The run a restart continuation resumes, if the command is one.
pub(super) fn restart_of(client: &C) -> Option<RunId> {
    let C::MessageDispatch {
        restart_continuation_of_run_id,
        ..
    } = client
    else {
        unreachable!("restart_of is called only for message.dispatch");
    };
    restart_continuation_of_run_id.clone()
}

/// `delegatedCompletionWakeDetail`.
pub(super) fn delegated_completion_wake_detail(task_ids: &[NodeId]) -> String {
    let list = task_ids
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    if task_ids.len() == 1 {
        format!(
            "Delegated task {list} reached a terminal state. Use task_status with taskId {list} to read the result."
        )
    } else {
        format!(
            "Delegated tasks {list} reached terminal states. Use task_status with each taskId to read the results."
        )
    }
}

/// The delivery a `delegatedCompletion` command carries, if the parent run's
/// cohort still expects it.
fn resolve_delegated_completion(
    command: &OrchestrationV2ServerCommand,
    client: &C,
    projection: &OrchestrationV2ThreadProjection,
    dispatch: &Dispatch,
) -> Result<Option<OrchestrationV2MessageDelegatedCompletion>, OrchestratorError> {
    let C::MessageDispatch {
        created_by,
        creation_source,
        message_id,
        delegated_completion,
        ..
    } = client
    else {
        unreachable!("resolve_delegated_completion is called only for message.dispatch");
    };
    let Some(requested) = delegated_completion else {
        return Ok(None);
    };
    if *created_by != OrchestrationV2Actor::Agent
        || *creation_source != OrchestrationV2CreationSource::Server
        || *dispatch != Dispatch::QueueAfterActive
    {
        return Err(dispatch_failure(
            command,
            "Delegated completion delivery must be a server-created queued message.",
        ));
    }
    let cohort = projection
        .runs
        .iter()
        .find(|candidate| candidate.id == requested.parent_run_id)
        .and_then(|parent| parent.delegated_completion.as_ref());
    let delivery = cohort.and_then(|cohort| cohort.delivery.as_ref());
    let dispatchable = cohort.is_some_and(|cohort| {
        cohort.disposition == OrchestrationV2DelegatedCompletionCohortDisposition::Open
    }) && delivery.is_some_and(|delivery| {
        delivery.generation == requested.generation
            && &delivery.message_id == message_id
            && (!projection
                .messages
                .iter()
                .any(|candidate| &candidate.id == message_id)
                || is_undelivered_mailbox_steer(projection, message_id))
    });
    let (true, Some(delivery)) = (dispatchable, delivery) else {
        return Err(dispatch_failure(
            command,
            "Delegated completion delivery is no longer dispatchable.",
        ));
    };
    Ok(Some(OrchestrationV2MessageDelegatedCompletion {
        parent_run_id: requested.parent_run_id.clone(),
        generation: delivery.generation,
        task_ids: delivery.task_ids.clone(),
    }))
}

/// A delegated completion whose tasks always wake the parent steers the
/// running turn when the live session can take a steer without interrupting
/// its tools; otherwise the dispatch stands.
async fn route_mailbox_steer(
    orchestrator: &Orchestrator,
    projection: &OrchestrationV2ThreadProjection,
    completion: &OrchestrationV2MessageDelegatedCompletion,
    dispatch: Dispatch,
) -> Dispatch {
    let always = completion.task_ids.iter().all(|id| {
        projection
            .subagents
            .iter()
            .find(|task| &task.id == id)
            .is_some_and(|task| task.completion_wake == Some(OrchestrationV2CompletionWake::Always))
    });
    if !always {
        return dispatch;
    }
    let Some(active) = projection
        .runs
        .iter()
        .find(|run| run.status == OrchestrationV2RunStatus::Running)
    else {
        return dispatch;
    };
    let session_id = projection
        .provider_threads
        .iter()
        .find(|row| Some(&row.id) == active.provider_thread_id.as_ref())
        .and_then(|row| row.provider_session_id.clone());
    let turn_running = projection.provider_turns.iter().any(|turn| {
        turn.run_attempt_id.is_some()
            && turn.run_attempt_id == active.active_attempt_id
            && turn.status == OrchestrationV2ProviderTurnStatus::Running
    });
    let maintenance = projection
        .messages
        .iter()
        .find(|message| message.id == active.user_message_id)
        .is_some_and(|message| is_native_maintenance_command(&message.text, &message.attachments));
    let Some(session_id) = session_id.filter(|_| turn_running && !maintenance) else {
        return dispatch;
    };
    // A failed lookup leaves the dispatch as it is, as upstream's `orElseSucceed`.
    let Ok(Some(session)) = orchestrator.seams.sessions.get_session(&session_id).await else {
        return dispatch;
    };
    let turns = &session.provider_session().capabilities.turns;
    if turns.supports_active_steering && turns.active_steering_interrupts_tools != Some(true) {
        return Dispatch::Steer {
            target_run_id: active.id.clone(),
            restart: false,
        };
    }
    dispatch
}

/// The proposed plan a command starts from, once it is known to be usable.
async fn resolve_source_plan(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    client: &C,
    projection: &OrchestrationV2ThreadProjection,
) -> Result<Option<OrchestrationV2PlanArtifact>, OrchestratorError> {
    let Some(reference) = source_plan_ref_of(client) else {
        return Ok(None);
    };
    let source_project_id = if reference.thread_id == projection.thread.id {
        projection.thread.project_id.clone()
    } else {
        orchestrator
            .projection
            .get_thread_projection(&reference.thread_id)
            .await
            .map_err(|cause| OrchestratorError::projection(reference.thread_id.clone(), cause))?
            .thread
            .project_id
    };
    // Command projections leave plans out, so read the source plan directly.
    let artifact = orchestrator
        .projection
        .get_plan(&reference.thread_id, reference.plan_id.as_str())
        .await
        .map_err(|cause| dispatch_failure(command, cause.to_string()))?;
    let Some(plan) = artifact
        .filter(|artifact| matches!(artifact, OrchestrationV2PlanArtifact::ProposedPlan { .. }))
    else {
        return Err(dispatch_failure(
            command,
            format!(
                "Proposed plan {} does not exist on thread {}.",
                reference.plan_id, reference.thread_id
            ),
        ));
    };
    if source_project_id != projection.thread.project_id {
        return Err(dispatch_failure(
            command,
            format!(
                "Proposed plan {} belongs to a different project.",
                reference.plan_id
            ),
        ));
    }
    if let OrchestrationV2PlanArtifact::ProposedPlan { id, status, .. } = &plan
        && *status != OrchestrationV2PlanArtifactStatus::Active
    {
        return Err(dispatch_failure(
            command,
            format!("Proposed plan {id} is not active."),
        ));
    }
    Ok(Some(plan))
}

/// The dispatch mode after the server resolves delivery intent.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Dispatch {
    QueueAfterActive,
    Start(Start),
    /// `steer_active`, or `restart_active` when `restart` is set.
    Steer {
        target_run_id: RunId,
        restart: bool,
    },
}

/// How a start-type dispatch begins. A deferred start waits for its workspace
/// to be prepared before the provider turn runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Start {
    Immediate,
    Deferred {
        workspace_strategy: Option<OrchestrationV2ThreadLaunchWorkspaceStrategy>,
    },
}

fn resolve_dispatch_mode(
    projection: &OrchestrationV2ThreadProjection,
    requested: &OrchestrationV2DispatchMode,
    delivery: Option<OrchestrationV2DeliveryIntent>,
) -> Dispatch {
    // Upstream's resolve passes `defer_start` through when no delivery intent
    // is given, so the deferred start survives to the start path.
    if let (OrchestrationV2DispatchMode::DeferStart { workspace_strategy }, None) =
        (requested, delivery)
    {
        return Dispatch::Start(Start::Deferred {
            workspace_strategy: workspace_strategy.clone(),
        });
    }
    let requested_mode = match requested {
        OrchestrationV2DispatchMode::StartImmediately
        | OrchestrationV2DispatchMode::DeferStart { .. } => RequestedDispatchMode::StartImmediately,
        OrchestrationV2DispatchMode::QueueAfterActive => RequestedDispatchMode::QueueAfterActive,
        OrchestrationV2DispatchMode::SteerActive { target_run_id } => {
            RequestedDispatchMode::SteerActive {
                target_run_id: target_run_id.clone(),
            }
        }
        OrchestrationV2DispatchMode::RestartActive { target_run_id } => {
            RequestedDispatchMode::RestartActive {
                target_run_id: target_run_id.clone(),
            }
        }
    };
    let delivery = delivery.map(|intent| match intent {
        OrchestrationV2DeliveryIntent::Auto => DeliveryIntent::Auto,
        OrchestrationV2DeliveryIntent::Steer => DeliveryIntent::Steer,
        OrchestrationV2DeliveryIntent::Restart => DeliveryIntent::Restart,
    });
    let mut mode = resolve_message_dispatch_intent(projection, requested_mode, delivery);
    // A steer submitted after its target finished (or whose turn already
    // completed) becomes its own turn, not a steer.
    if let RequestedDispatchMode::SteerActive { target_run_id } = &mode {
        let target = projection.runs.iter().find(|run| &run.id == target_run_id);
        let turn = target.and_then(|target| {
            latest_provider_turn_for_attempt(
                projection
                    .provider_turns
                    .iter()
                    .filter(|candidate| Some(&candidate.node_id) == target.root_node_id.as_ref()),
                target.active_attempt_id.as_ref(),
            )
        });
        let too_late = target.is_some_and(|target| {
            target.status == OrchestrationV2RunStatus::Completed
                || ((target.status == OrchestrationV2RunStatus::Running
                    || target.status == OrchestrationV2RunStatus::Waiting)
                    && turn.is_some_and(|turn| {
                        turn.status == OrchestrationV2ProviderTurnStatus::Completed
                    }))
        });
        if too_late {
            mode = RequestedDispatchMode::StartImmediately;
        }
    }
    match mode {
        RequestedDispatchMode::SteerActive { target_run_id } => Dispatch::Steer {
            target_run_id,
            restart: false,
        },
        RequestedDispatchMode::RestartActive { target_run_id } => Dispatch::Steer {
            target_run_id,
            restart: true,
        },
        RequestedDispatchMode::QueueAfterActive => Dispatch::QueueAfterActive,
        RequestedDispatchMode::StartImmediately => Dispatch::Start(Start::Immediate),
    }
}

pub(super) fn is_blocking_run(run: &OrchestrationV2Run) -> bool {
    matches!(
        run.status,
        OrchestrationV2RunStatus::Preparing
            | OrchestrationV2RunStatus::Starting
            | OrchestrationV2RunStatus::Running
            | OrchestrationV2RunStatus::Waiting
    )
}

/// `isNativeMaintenanceCommand`: `/compact` and `/logout` without attachments.
pub(crate) fn is_native_maintenance_command(text: &str, attachments: &[ChatAttachment]) -> bool {
    attachments.is_empty() && matches!(text.trim().to_lowercase().as_str(), "/compact" | "/logout")
}

pub(super) fn emit_thread_event(
    decision: &mut Decision,
    kind: &str,
    thread: &OrchestrationV2AppThread,
    now: UtcDateTime,
) {
    let id = decision.event_id(&thread.id);
    let provider_instance_id = Some(thread.provider_instance_id.clone());
    let payload = thread.clone();
    let event = match kind {
        "unsettled" => OrchestrationV2DomainEvent::ThreadUnsettled {
            id,
            thread_id: thread.id.clone(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id,
            raw_event_id: None,
            occurred_at: now,
            payload,
        },
        "unsnoozed" => OrchestrationV2DomainEvent::ThreadUnsnoozed {
            id,
            thread_id: thread.id.clone(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id,
            raw_event_id: None,
            occurred_at: now,
            payload,
        },
        "model-selection-updated" => OrchestrationV2DomainEvent::ThreadModelSelectionUpdated {
            id,
            thread_id: thread.id.clone(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id,
            raw_event_id: None,
            occurred_at: now,
            payload,
        },
        "provider-switched" => OrchestrationV2DomainEvent::ThreadProviderSwitched {
            id,
            thread_id: thread.id.clone(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id,
            raw_event_id: None,
            occurred_at: now,
            payload,
        },
        _ => OrchestrationV2DomainEvent::ThreadMetadataUpdated {
            id,
            thread_id: thread.id.clone(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id,
            raw_event_id: None,
            occurred_at: now,
            payload,
        },
    };
    decision.emit(event);
}

fn title_generation_effect(
    command_id: &turin_contracts::CommandId,
    thread_id: &ThreadId,
    kind: ThreadTitleKind,
) -> PendingEffect {
    PendingEffect {
        id: format!("effect:{command_id}:thread-title.generate"),
        command_id: command_id.clone(),
        thread_id: thread_id.clone(),
        request: EffectRequest::ThreadTitleGenerate { kind },
        available_at: None,
    }
}

fn millis(at: UtcDateTime) -> i64 {
    at.as_datetime().timestamp_millis()
}

/// What the start branch needs beyond the projection.
pub(super) struct StartInput<'a> {
    pub(super) client: &'a C,
    pub(super) requested_model: &'a ModelSelection,
    pub(super) start: Start,
    pub(super) resolved: &'a Resolved,
}

/// The start branch of `dispatchMessage`: a `starting` run that gets its
/// provider turn from the effect, or a `preparing` run for `defer_start`. A
/// deferred run has no effect here; it starts once its workspace is ready.
///
/// A provider switch (upstream's `isProviderSwitch`) or a pending fork or
/// merge-back transfer takes `message_transfer.rs` instead.
async fn start_run(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    projection: &OrchestrationV2ThreadProjection,
    input: StartInput<'_>,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    let StartInput {
        client,
        requested_model,
        start,
        resolved,
    } = input;
    let C::MessageDispatch {
        created_by,
        creation_source,
        message_id,
        ..
    } = client
    else {
        unreachable!("start_run is called only for message.dispatch");
    };
    let thread = &projection.thread;
    let instance = &requested_model.instance_id;
    let active_provider_thread = thread.active_provider_thread_id.as_ref().and_then(|id| {
        projection
            .provider_threads
            .iter()
            .find(|candidate| &candidate.id == id)
    });
    let now = decision.now();
    if thread.model_selection != *requested_model || thread.provider_instance_id != *instance {
        // A same-instance change is a model update; across instances it is a
        // provider switch that the path above has already refused unless no
        // provider thread is active yet.
        let same_instance = thread.provider_instance_id == *instance;
        let mut updated = thread.clone();
        updated.provider_instance_id = instance.clone();
        updated.model_selection = requested_model.clone();
        updated.updated_at = now;
        emit_thread_event(
            decision,
            if same_instance {
                "model-selection-updated"
            } else {
                "provider-switched"
            },
            &updated,
            now,
        );
    }

    let is_provider_switch =
        active_provider_thread.is_some_and(|active| &active.provider_instance_id != instance);
    let carries_transfer = projection.context_transfers.iter().any(|transfer| {
        transfer.target_thread_id == thread.id
            && transfer.status == OrchestrationV2ContextTransferStatus::Pending
            && matches!(
                transfer.r#type,
                OrchestrationV2ContextTransferType::Fork
                    | OrchestrationV2ContextTransferType::MergeBack
            )
    });
    if is_provider_switch || carries_transfer {
        return start_run_with_transfers(
            orchestrator,
            command,
            projection,
            StartInput {
                client,
                requested_model,
                start,
                resolved,
            },
            decision,
        )
        .await;
    }

    let ordinal = next_run_ordinal(projection);
    let run_id = ids::run(&thread.id, ordinal);
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
    let provider_session_id = match active_provider_thread
        .and_then(|active| active.provider_session_id.clone())
    {
        Some(session_id) => session_id,
        None => provider_session_id_for(adapter.as_ref(), instance, &thread.id, command).await?,
    };
    let provider_thread_id = active_provider_thread.map_or_else(
        || ids::provider_thread(adapter.driver(), None, &format!("pending:{run_id}")),
        |active| active.id.clone(),
    );
    // A thread imported from the previous orchestrator hands its history to
    // the first native turn.
    let imported = thread.history_origin == Some(OrchestrationV2ThreadHistoryOrigin::V1Import);
    let legacy_import_items = if imported {
        read_handoff_items(orchestrator, &thread.id, Some(vec![None])).await?
    } else {
        Vec::new()
    };
    let legacy_import_handoff = should_prepare_legacy_import_handoff(
        imported,
        projection
            .runs
            .iter()
            .any(|run| run.status == OrchestrationV2RunStatus::Completed),
        legacy_import_items.len(),
    )
    .then(|| {
        ContextHandoffService::default().prepare_legacy_import(&LegacyImportInput {
            thread_id: &thread.id,
            target_run_id: &run_id,
            to_provider_thread_id: &provider_thread_id,
            to_provider_instance_id: instance,
            items: &legacy_import_items,
            created_at: now,
        })
    });
    let legacy_handoff_ids = |existing: &[turin_contracts::ContextHandoffId]| {
        append_context_handoff_id(existing, legacy_import_handoff.as_ref().map(|h| &h.id))
    };
    let provider_thread = match active_provider_thread {
        Some(active) => OrchestrationV2ProviderThread {
            provider_session_id: Some(provider_session_id.clone()),
            last_run_ordinal: PositiveInt::new(ordinal),
            handoff_ids: legacy_handoff_ids(&active.handoff_ids),
            updated_at: now,
            ..active.clone()
        },
        None => OrchestrationV2ProviderThread {
            id: provider_thread_id.clone(),
            driver: adapter.driver().clone(),
            provider_instance_id: instance.clone(),
            provider_session_id: Some(provider_session_id.clone()),
            app_thread_id: Some(thread.id.clone()),
            owner_node_id: None,
            native_thread_ref: None,
            native_conversation_head_ref: None,
            status: OrchestrationV2ProviderThreadStatus::NotLoaded,
            first_run_ordinal: PositiveInt::new(ordinal),
            last_run_ordinal: PositiveInt::new(ordinal),
            handoff_ids: legacy_handoff_ids(&[]),
            forked_from: None,
            pending_background_tasks: Vec::new(),
            context_usage: None,
            native_metadata: None,
            goal: None,
            created_at: now,
            updated_at: now,
        },
    };
    let attempt_id = ids::run_attempt(&run_id, 1);
    let root_node_id = ids::root_node(&run_id);
    let deferred = matches!(start, Start::Deferred { .. });
    // A deferred run's scope is prepared once its workspace exists.
    let checkpoint_scope = if deferred {
        None
    } else {
        Some(
            prepare_root_scope(
                orchestrator,
                command,
                projection,
                requested_model,
                None,
                &run_id,
                &root_node_id,
                &provider_thread_id,
                now,
            )
            .await?,
        )
    };
    let (status, workspace_preparation) = match &start {
        Start::Immediate => (OrchestrationV2RunStatus::Starting, None),
        Start::Deferred { workspace_strategy } => (
            OrchestrationV2RunStatus::Preparing,
            workspace_strategy.clone(),
        ),
    };
    let run = OrchestrationV2Run {
        id: run_id.clone(),
        thread_id: thread.id.clone(),
        ordinal: PositiveInt::new(ordinal).expect("run ordinals start at 1"),
        provider_instance_id: instance.clone(),
        model_selection: requested_model.clone(),
        provider_thread_id: Some(provider_thread_id.clone()),
        user_message_id: message_id.clone(),
        root_node_id: Some(root_node_id.clone()),
        active_attempt_id: Some(attempt_id.clone()),
        status,
        queue_position: None,
        queue_held: None,
        requested_at: now,
        started_at: None,
        completed_at: None,
        checkpoint_id: None,
        context_handoff_id: legacy_import_handoff
            .as_ref()
            .map(|handoff| handoff.id.clone()),
        restart_continuation_of_run_id: restart_of(client),
        delegated_task_id: resolved.continued_task_id.clone(),
        work_started_at: wake_work_started_at(&projection.runs, is_wake(client)),
        restart_cancelled_background_work: None,
        source_plan_ref: source_plan_ref_of(client),
        delegated_completion: None,
        workspace_preparation,
    };
    let attempt = OrchestrationV2RunAttempt {
        id: attempt_id,
        native_thread_id: None,
        run_id: run_id.clone(),
        attempt_ordinal: PositiveInt::new(1).expect("1 is positive"),
        root_node_id: root_node_id.clone(),
        provider_instance_id: instance.clone(),
        provider_thread_id: provider_thread_id.clone(),
        provider_turn_id: None,
        reason: OrchestrationV2RunAttemptReason::Initial,
        status: OrchestrationV2RunAttemptStatus::Pending,
        started_at: None,
        completed_at: None,
    };
    let root_node = OrchestrationV2ExecutionNode {
        id: root_node_id.clone(),
        thread_id: thread.id.clone(),
        run_id: Some(run_id.clone()),
        parent_node_id: None,
        root_node_id: root_node_id.clone(),
        kind: OrchestrationV2ExecutionNodeKind::RootTurn,
        status: OrchestrationV2ExecutionNodeStatus::Pending,
        counts_for_run: true,
        provider_thread_id: Some(provider_thread_id.clone()),
        provider_turn_id: None,
        native_item_ref: None,
        runtime_request_id: None,
        checkpoint_scope_id: checkpoint_scope.as_ref().map(|scope| scope.id.clone()),
        started_at: None,
        completed_at: None,
    };
    let mut message = conversation_message(client, &thread.id, &run_id, &root_node_id, now);
    resolved.apply(&mut message);
    // The user's turn item is new on the start path. Its ordinal is the
    // store's next one, which already sits past every stored item.
    let turn_item_ordinal = next_turn_item_ordinal(orchestrator, projection).await?;
    let user_item = OrchestrationV2TurnItem::UserMessage {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: ids::user_turn_item(message_id),
        thread_id: thread.id.clone(),
        run_id: Some(run_id.clone()),
        node_id: Some(root_node_id.clone()),
        provider_thread_id: Some(provider_thread_id.clone()),
        provider_turn_id: None,
        native_item_ref: None,
        parent_item_id: None,
        ordinal: turn_item_ordinal,
        status: OrchestrationV2TurnItemStatus::Completed,
        title: None,
        started_at: Some(now),
        completed_at: Some(now),
        updated_at: now,
        created_by: *created_by,
        creation_source: *creation_source,
        message_id: message_id.clone(),
        scheduled_task_id: message.scheduled_task_id.clone(),
        sender_thread_id: message.sender_thread_id.clone(),
        input_intent: OrchestrationV2UserMessageInputIntent::TurnStart,
        text: message.text.clone(),
        context: message.context.clone(),
        attachments: message.attachments.clone(),
    };
    let preparation_item = deferred.then(|| OrchestrationV2TurnItem::CommandExecution {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: ids::turn_item_from_provider_item(
            adapter.driver(),
            &format!("workspace-preparation:{run_id}"),
        ),
        thread_id: thread.id.clone(),
        run_id: Some(run_id.clone()),
        node_id: Some(root_node_id.clone()),
        provider_thread_id: Some(provider_thread_id.clone()),
        provider_turn_id: None,
        native_item_ref: None,
        parent_item_id: None,
        ordinal: turn_item_ordinal + 1,
        status: OrchestrationV2TurnItemStatus::Running,
        title: Some(WORKSPACE_PREPARATION_INPUT.to_owned()),
        started_at: Some(now),
        completed_at: None,
        updated_at: now,
        input: WORKSPACE_PREPARATION_INPUT.to_owned(),
        output: None,
        output_omitted: None,
        output_indicates_failure: None,
        exit_code: None,
    });

    let id = decision.event_id(&thread.id);
    decision.emit(OrchestrationV2DomainEvent::ProviderThreadUpdated {
        id,
        thread_id: thread.id.clone(),
        run_id: None,
        node_id: None,
        driver: None,
        provider_instance_id: Some(instance.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: provider_thread,
    });
    if let Some(handoff) = legacy_import_handoff {
        run_scoped_event!(
            decision,
            ContextHandoffUpdated,
            &thread.id,
            &run_id,
            &root_node_id,
            instance,
            handoff
        );
    }
    run_scoped_event!(
        decision,
        RunCreated,
        &thread.id,
        &run_id,
        &root_node_id,
        instance,
        run
    );
    resolved.complete_source_plan(decision, now);
    run_scoped_event!(
        decision,
        RunAttemptCreated,
        &thread.id,
        &run_id,
        &root_node_id,
        instance,
        attempt
    );
    run_scoped_event!(
        decision,
        NodeUpdated,
        &thread.id,
        &run_id,
        &root_node_id,
        instance,
        root_node
    );
    if let Some(scope) = checkpoint_scope {
        let stored = orchestrator
            .seams
            .checkpoints
            .ensure_scope(scope)
            .await
            .map_err(|cause| dispatch_failure(command, cause.to_string()))?;
        run_scoped_event!(
            decision,
            CheckpointScopeCreated,
            &thread.id,
            &run_id,
            &root_node_id,
            instance,
            stored
        );
    }
    // The notification item is projected from the message, so it is built
    // before the message moves into its event.
    let notification_item = notification_turn_item(
        &user_item,
        NotificationMessage::from(&message),
        &projection.subagents,
    );
    run_scoped_event!(
        decision,
        MessageUpdated,
        &thread.id,
        &run_id,
        &root_node_id,
        instance,
        message
    );
    run_scoped_event!(
        decision,
        TurnItemUpdated,
        &thread.id,
        &run_id,
        &root_node_id,
        instance,
        notification_item
    );
    if let Some(item) = preparation_item {
        run_scoped_event!(
            decision,
            TurnItemUpdated,
            &thread.id,
            &run_id,
            &root_node_id,
            instance,
            item
        );
    }
    if !deferred {
        decision.effects.push(PendingEffect {
            id: format!(
                "effect:{}:provider-turn.start:{run_id}",
                command_id(command)
            ),
            command_id: command_id(command).clone(),
            thread_id: thread.id.clone(),
            request: EffectRequest::ProviderTurnStart {
                run_id: run_id.clone(),
            },
            available_at: None,
        });
    }
    Ok(())
}

/// `providerSessionIdFor`: one shared session per instance when the adapter
/// runs several provider threads in a session, else a session for this thread.
pub(super) async fn provider_session_id_for(
    adapter: &dyn ProviderAdapterV2,
    instance: &ProviderInstanceId,
    thread_id: &ThreadId,
    command: &OrchestrationV2ServerCommand,
) -> Result<turin_contracts::ProviderSessionId, OrchestratorError> {
    provider_session_id_for_raw(adapter, instance, thread_id)
        .await
        .map_err(|cause| dispatch_failure(command, cause))
}

/// [`provider_session_id_for`] for callers with no command to blame.
pub(super) async fn provider_session_id_for_raw(
    adapter: &dyn ProviderAdapterV2,
    instance: &ProviderInstanceId,
    thread_id: &ThreadId,
) -> Result<turin_contracts::ProviderSessionId, String> {
    let capabilities = adapter
        .get_capabilities()
        .await
        .map_err(|cause| cause.to_string())?;
    Ok(
        if capabilities
            .sessions
            .supports_multiple_provider_threads_per_session
        {
            ids::provider_session(instance)
        } else {
            IdAllocator.provider_session(instance, thread_id)
        },
    )
}

/// The user message a dispatch writes, under the run and root node it starts
/// or queues.
pub(super) fn conversation_message(
    client: &C,
    thread_id: &ThreadId,
    run_id: &RunId,
    root_node_id: &NodeId,
    now: UtcDateTime,
) -> OrchestrationV2ConversationMessage {
    OrchestrationV2ConversationMessage {
        run_id: Some(run_id.clone()),
        node_id: Some(root_node_id.clone()),
        ..message_template(client, thread_id, now)
    }
}

/// The command's user message before it lands in a run: no run or node yet.
pub(super) fn message_template(
    client: &C,
    thread_id: &ThreadId,
    now: UtcDateTime,
) -> OrchestrationV2ConversationMessage {
    let C::MessageDispatch {
        created_by,
        creation_source,
        scheduled_task_id,
        sender_thread_id,
        message_id,
        text,
        context,
        attachments,
        notification,
        ..
    } = client
    else {
        unreachable!("message_template is called only for message.dispatch");
    };
    OrchestrationV2ConversationMessage {
        created_by: *created_by,
        creation_source: *creation_source,
        scheduled_task_id: scheduled_task_id.clone(),
        sender_thread_id: sender_thread_id.clone(),
        id: message_id.clone(),
        thread_id: thread_id.clone(),
        run_id: None,
        node_id: None,
        role: OrchestrationV2ConversationMessageRole::User,
        text: text.to_owned(),
        context: context.clone(),
        attachments: attachments.to_vec(),
        streaming: false,
        created_at: now,
        updated_at: now,
        delegated_completion: None,
        notification: notification.clone(),
    }
}

/// `wakeWorkStartedAt`: a wake (a notification, a delegated-completion
/// delivery or a restart continuation) carries on the work of the run that
/// started last, so it keeps that run's work start. Other runs start new work.
pub(super) fn wake_work_started_at(runs: &[OrchestrationV2Run], wake: bool) -> Option<UtcDateTime> {
    if !wake {
        return None;
    }
    runs.iter()
        .filter(|run| run.started_at.is_some())
        .max_by_key(|run| (run.started_at, run.ordinal.get()))
        .map(|run| *orchestration_v2_run_work_started_at(run))
}

/// Whether the command is a wake, as `wakeWorkStartedAt` reads its trigger.
pub(super) fn is_wake(client: &C) -> bool {
    let C::MessageDispatch {
        notification,
        delegated_completion,
        restart_continuation_of_run_id,
        ..
    } = client
    else {
        unreachable!("is_wake is called only for message.dispatch");
    };
    notification.is_some()
        || delegated_completion.is_some()
        || restart_continuation_of_run_id.is_some()
}

/// The projection of a thread that only this decision's `thread.created`
/// event has made so far: an empty thread with the pending events laid over.
fn pending_thread_projection(
    decision: &Decision,
    thread_id: &ThreadId,
) -> Option<OrchestrationV2ThreadProjection> {
    let thread = decision.events.iter().rev().find_map(|event| match event {
        OrchestrationV2DomainEvent::ThreadCreated { payload, .. } if &payload.id == thread_id => {
            Some(payload.clone())
        }
        _ => None,
    })?;
    let empty = OrchestrationV2ThreadProjection {
        updated_at: thread.updated_at,
        thread,
        runs: Vec::new(),
        attempts: Vec::new(),
        nodes: Vec::new(),
        subagents: Vec::new(),
        provider_sessions: Vec::new(),
        provider_threads: Vec::new(),
        provider_turns: Vec::new(),
        runtime_requests: Vec::new(),
        messages: Vec::new(),
        plans: Vec::new(),
        turn_items: Vec::new(),
        checkpoint_scopes: Vec::new(),
        checkpoints: Vec::new(),
        context_handoffs: Vec::new(),
        context_transfers: Vec::new(),
        visible_turn_items: Vec::new(),
    };
    Some(super::cohort::with_pending_events(
        empty,
        &decision.events,
        thread_id,
    ))
}

/// `appendContextHandoffId`: a reissued legacy handoff is recorded on the
/// provider thread once.
pub(super) fn append_context_handoff_id(
    existing: &[turin_contracts::ContextHandoffId],
    handoff_id: Option<&turin_contracts::ContextHandoffId>,
) -> Vec<turin_contracts::ContextHandoffId> {
    let mut ids = existing.to_vec();
    if let Some(handoff_id) = handoff_id
        && !ids.contains(handoff_id)
    {
        ids.push(handoff_id.clone());
    }
    ids
}

/// `nextTurnItemOrdinal` for a projection read from the store: the store's
/// next ordinal already sits past every stored turn item.
pub(super) async fn next_turn_item_ordinal(
    orchestrator: &Orchestrator,
    projection: &OrchestrationV2ThreadProjection,
) -> Result<u64, OrchestratorError> {
    orchestrator
        .projection
        .get_next_turn_item_ordinal(&projection.thread.id)
        .await
        .map_err(|cause| OrchestratorError::projection(projection.thread.id.clone(), cause))
}

/// What the queued branch needs: the command itself (its fields are read
/// where they are used), the run it waits behind and the model it runs on.
struct QueueInput<'a> {
    client: &'a C,
    queue_behind: &'a OrchestrationV2Run,
    model_selection: &'a ModelSelection,
    /// The run was held because its worktree never came up.
    worktree_missing: bool,
    resolved: &'a Resolved,
}

/// The queued branch of `dispatchMessage`: the message becomes a `queued` run
/// behind the blocking one (or a held run behind a missing worktree).
async fn queue_run(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    projection: &OrchestrationV2ThreadProjection,
    input: QueueInput<'_>,
    decision: &mut Decision,
) -> Result<(), OrchestratorError> {
    let C::MessageDispatch {
        message_id,
        restart_continuation_of_run_id,
        ..
    } = input.client
    else {
        unreachable!("queue_run is called only for message.dispatch");
    };
    let thread = &projection.thread;
    if pending_merge_back_transfers(projection) {
        return Err(dispatch_failure(
            command,
            format!(
                "Thread {} has a pending merge-back transfer; queued merge-back consumption is not implemented yet.",
                thread.id
            ),
        ));
    }
    let active_provider_thread = thread.active_provider_thread_id.as_ref().and_then(|id| {
        projection
            .provider_threads
            .iter()
            .find(|candidate| &candidate.id == id)
    });
    let queue_provider_thread = active_provider_thread
        .or_else(|| {
            input
                .queue_behind
                .provider_thread_id
                .as_ref()
                .and_then(|id| {
                    projection
                        .provider_threads
                        .iter()
                        .find(|candidate| &candidate.id == id)
                })
        })
        .ok_or_else(|| {
            dispatch_failure(
                command,
                format!(
                    "Active run {} has no provider thread for queued dispatch.",
                    input.queue_behind.id
                ),
            )
        })?;

    let now = decision.now();
    let ordinal = next_run_ordinal(projection);
    let run_id = ids::run(&thread.id, ordinal);
    let target_provider_thread =
        if input.model_selection.instance_id == queue_provider_thread.provider_instance_id {
            Some(queue_provider_thread.clone())
        } else {
            root_provider_threads_for_provider(projection, &input.model_selection.instance_id)
                .into_iter()
                .next()
        };
    let adapter = orchestrator
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
    let selected_session = target_provider_thread
        .as_ref()
        .and_then(|candidate| candidate.provider_session_id.as_ref())
        .and_then(|session_id| {
            projection
                .provider_sessions
                .iter()
                .find(|candidate| &candidate.id == session_id)
        });
    let capabilities = match selected_session {
        Some(session) => session.capabilities,
        None => adapter
            .get_capabilities()
            .await
            .map_err(|cause| dispatch_failure(command, cause.to_string()))?,
    };
    ensure_queued_messages(&CapabilityCheck {
        command_id: command_id(command),
        thread_id: &thread.id,
        provider_instance_id: &input.model_selection.instance_id,
        capabilities: &capabilities,
    })
    .map_err(|cause| dispatch_failure(command, cause.to_string()))?;

    // A new provider thread is created `not_loaded` under a pending native id,
    // until the provider starts the run and names its thread.
    let (provider_thread, thread_is_new) = match target_provider_thread {
        Some(existing) => (existing, false),
        None => (
            OrchestrationV2ProviderThread {
                id: ids::provider_thread(adapter.driver(), None, &format!("pending:{run_id}")),
                driver: adapter.driver().clone(),
                provider_instance_id: input.model_selection.instance_id.clone(),
                provider_session_id: None,
                app_thread_id: Some(thread.id.clone()),
                owner_node_id: None,
                native_thread_ref: None,
                native_conversation_head_ref: None,
                status: OrchestrationV2ProviderThreadStatus::NotLoaded,
                first_run_ordinal: None,
                last_run_ordinal: None,
                handoff_ids: Vec::new(),
                forked_from: None,
                pending_background_tasks: Vec::new(),
                context_usage: None,
                native_metadata: None,
                goal: None,
                created_at: now,
                updated_at: now,
            },
            true,
        ),
    };
    let attempt_id = ids::run_attempt(&run_id, 1);
    let root_node_id = ids::root_node(&run_id);
    // A run queued behind one still preparing, or behind a missing worktree,
    // gets its checkpoint scope when it starts.
    let checkpoint_scope = if input.queue_behind.status == OrchestrationV2RunStatus::Preparing
        || input.worktree_missing
    {
        None
    } else {
        Some(
            prepare_root_scope(
                orchestrator,
                command,
                projection,
                input.model_selection,
                selected_session.map(|session| session.cwd.to_string()),
                &run_id,
                &root_node_id,
                &provider_thread.id,
                now,
            )
            .await?,
        )
    };
    let held = input.worktree_missing
        || projection.runs.iter().any(|candidate| {
            candidate.status == OrchestrationV2RunStatus::Queued
                && candidate.queue_held == Some(true)
        });
    let queue_position = projection
        .runs
        .iter()
        .filter(|candidate| candidate.status == OrchestrationV2RunStatus::Queued)
        .map(|candidate| {
            candidate
                .queue_position
                .flatten()
                .map_or(candidate.ordinal.get(), PositiveInt::get)
        })
        .max()
        .unwrap_or(0)
        + 1;
    let run = OrchestrationV2Run {
        id: run_id.clone(),
        thread_id: thread.id.clone(),
        ordinal: PositiveInt::new(ordinal).expect("run ordinals start at 1"),
        provider_instance_id: input.model_selection.instance_id.clone(),
        model_selection: input.model_selection.clone(),
        provider_thread_id: Some(provider_thread.id.clone()),
        user_message_id: message_id.clone(),
        root_node_id: Some(root_node_id.clone()),
        active_attempt_id: Some(attempt_id.clone()),
        status: OrchestrationV2RunStatus::Queued,
        queue_position: Some(PositiveInt::new(queue_position)),
        queue_held: held.then_some(true),
        requested_at: now,
        started_at: None,
        completed_at: None,
        checkpoint_id: None,
        context_handoff_id: None,
        restart_continuation_of_run_id: restart_continuation_of_run_id.clone(),
        delegated_task_id: input.resolved.continued_task_id.clone(),
        work_started_at: wake_work_started_at(&projection.runs, is_wake(input.client)),
        restart_cancelled_background_work: None,
        source_plan_ref: source_plan_ref_of(input.client),
        delegated_completion: None,
        workspace_preparation: None,
    };
    let attempt = OrchestrationV2RunAttempt {
        id: attempt_id,
        native_thread_id: None,
        run_id: run_id.clone(),
        attempt_ordinal: PositiveInt::new(1).expect("1 is positive"),
        root_node_id: root_node_id.clone(),
        provider_instance_id: input.model_selection.instance_id.clone(),
        provider_thread_id: provider_thread.id.clone(),
        provider_turn_id: None,
        reason: OrchestrationV2RunAttemptReason::Initial,
        status: OrchestrationV2RunAttemptStatus::Pending,
        started_at: None,
        completed_at: None,
    };
    let root_node = OrchestrationV2ExecutionNode {
        id: root_node_id.clone(),
        thread_id: thread.id.clone(),
        run_id: Some(run_id.clone()),
        parent_node_id: None,
        root_node_id: root_node_id.clone(),
        kind: OrchestrationV2ExecutionNodeKind::RootTurn,
        status: OrchestrationV2ExecutionNodeStatus::Pending,
        counts_for_run: true,
        provider_thread_id: Some(provider_thread.id.clone()),
        provider_turn_id: None,
        native_item_ref: None,
        runtime_request_id: None,
        checkpoint_scope_id: checkpoint_scope.as_ref().map(|scope| scope.id.clone()),
        started_at: None,
        completed_at: None,
    };
    let mut message = conversation_message(input.client, &thread.id, &run_id, &root_node_id, now);
    input.resolved.apply(&mut message);

    let instance = &input.model_selection.instance_id;
    if thread_is_new {
        let id = decision.event_id(&thread.id);
        decision.emit(OrchestrationV2DomainEvent::ProviderThreadUpdated {
            id,
            thread_id: thread.id.clone(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id: Some(instance.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: provider_thread.clone(),
        });
    }
    run_scoped_event!(
        decision,
        RunCreated,
        &thread.id,
        &run_id,
        &root_node_id,
        instance,
        run
    );
    input.resolved.complete_source_plan(decision, now);
    run_scoped_event!(
        decision,
        RunAttemptCreated,
        &thread.id,
        &run_id,
        &root_node_id,
        instance,
        attempt
    );
    run_scoped_event!(
        decision,
        NodeUpdated,
        &thread.id,
        &run_id,
        &root_node_id,
        instance,
        root_node
    );
    if let Some(scope) = checkpoint_scope {
        let stored = orchestrator
            .seams
            .checkpoints
            .ensure_scope(scope)
            .await
            .map_err(|cause| dispatch_failure(command, cause.to_string()))?;
        run_scoped_event!(
            decision,
            CheckpointScopeCreated,
            &thread.id,
            &run_id,
            &root_node_id,
            instance,
            stored
        );
    }
    run_scoped_event!(
        decision,
        MessageUpdated,
        &thread.id,
        &run_id,
        &root_node_id,
        instance,
        message
    );
    // The user's turn item may already exist (a stored user message); it is
    // re-pointed at the queued run and marked as a queued turn.
    let existing = projection.turn_items.iter().find_map(|item| match item {
        OrchestrationV2TurnItem::UserMessage {
            message_id: item_message_id,
            ..
        } if item_message_id == message_id => Some(item.clone()),
        _ => None,
    });
    if let Some(mut item) = existing
        && let OrchestrationV2TurnItem::UserMessage {
            run_id: item_run_id,
            node_id,
            provider_thread_id,
            provider_turn_id,
            input_intent,
            updated_at,
            ..
        } = &mut item
    {
        *item_run_id = Some(run_id.clone());
        *node_id = Some(root_node_id.clone());
        *provider_thread_id = Some(provider_thread.id.clone());
        *provider_turn_id = None;
        *input_intent = OrchestrationV2UserMessageInputIntent::QueuedTurn;
        *updated_at = now;
        run_scoped_event!(
            decision,
            TurnItemUpdated,
            &thread.id,
            &run_id,
            &root_node_id,
            instance,
            item
        );
    }
    Ok(())
}

/// `checkpointService.prepareRootRunScope` for a queued or started root run.
/// The cwd follows `RuntimePolicyV2.resolve`: the thread's worktree, else the
/// project root; the provider session's cwd wins over both, as upstream.
#[allow(clippy::too_many_arguments)]
pub(super) async fn prepare_root_scope(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    projection: &OrchestrationV2ThreadProjection,
    model_selection: &ModelSelection,
    session_cwd: Option<String>,
    run_id: &RunId,
    root_node_id: &NodeId,
    provider_thread_id: &ProviderThreadId,
    now: UtcDateTime,
) -> Result<OrchestrationV2CheckpointScope, OrchestratorError> {
    let _ = model_selection;
    prepare_root_scope_raw(
        orchestrator,
        &projection.thread,
        session_cwd,
        run_id,
        root_node_id,
        provider_thread_id,
        now,
    )
    .await
    .map_err(|cause| dispatch_failure(command, cause))
}

/// [`prepare_root_scope`] for callers with no command to blame.
pub(super) async fn prepare_root_scope_raw(
    orchestrator: &Orchestrator,
    thread: &OrchestrationV2AppThread,
    session_cwd: Option<String>,
    run_id: &RunId,
    root_node_id: &NodeId,
    provider_thread_id: &ProviderThreadId,
    now: UtcDateTime,
) -> Result<OrchestrationV2CheckpointScope, String> {
    let project_root = orchestrator
        .seams
        .project_roots
        .workspace_root(&thread.project_id)
        .await
        .map_err(|cause| cause.to_string())?
        .ok_or_else(|| format!("Project {} has no workspace root.", thread.project_id))?;
    let policy = runtime_policy::resolve(
        thread,
        &project_root,
        None,
        &RuntimePolicyOverride::default(),
    );
    let cwd = session_cwd.unwrap_or(policy.cwd);
    orchestrator
        .seams
        .checkpoints
        .prepare_root_run_scope(PrepareRootRunScope {
            thread_id: thread.id.clone(),
            run_id: run_id.clone(),
            root_node_id: root_node_id.clone(),
            provider_thread_id: provider_thread_id.clone(),
            cwd,
            created_at: now,
        })
        .await
        .map_err(|cause| cause.to_string())
}

/// `pendingMergeBackTransfersForThread` is non-empty.
fn pending_merge_back_transfers(projection: &OrchestrationV2ThreadProjection) -> bool {
    projection.context_transfers.iter().any(|transfer| {
        transfer.r#type == OrchestrationV2ContextTransferType::MergeBack
            && transfer.target_thread_id == projection.thread.id
            && transfer.status == OrchestrationV2ContextTransferStatus::Pending
    })
}

/// `nextRunOrdinal`.
pub(super) fn next_run_ordinal(projection: &OrchestrationV2ThreadProjection) -> u64 {
    projection.runs.len() as u64 + 1
}

/// `rootProviderThreadsForProvider`: the thread's own provider threads on
/// this instance, newest first.
pub(super) fn root_provider_threads_for_provider(
    projection: &OrchestrationV2ThreadProjection,
    instance_id: &ProviderInstanceId,
) -> Vec<OrchestrationV2ProviderThread> {
    let mut threads: Vec<_> = projection
        .provider_threads
        .iter()
        .filter(|candidate| {
            &candidate.provider_instance_id == instance_id
                && candidate.app_thread_id.as_ref() == Some(&projection.thread.id)
                && candidate.owner_node_id.is_none()
        })
        .cloned()
        .collect();
    threads.sort_by(|left, right| {
        right
            .last_run_ordinal
            .map(PositiveInt::get)
            .cmp(&left.last_run_ordinal.map(PositiveInt::get))
            .then_with(|| right.updated_at.cmp(&left.updated_at))
    });
    threads
}
