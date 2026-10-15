//! Port of upstream/apps/server/src/orchestration-v2/Orchestrator.ts `dispatchMessage`
//! (`message.dispatch`, around line 4568).
//!
//! Slices in this file (see docs/handoff/T13c.md, item F):
//! * guards and pre-steps: manual and usage-limit continuation, unsettle and
//!   unsnooze before a new turn, the title seed, the notification guard;
//! * queued runs: a message that waits behind a blocking run (or a worktree
//!   that is missing after a failed setup) becomes a `queued` run.
//!
//! Refused until their slices land: restart continuation (needs
//! `stopReachedRun`, T13e), delegated-completion and source-plan dispatch
//! (S6, plan reads), steer and restart into the active run (S5), the
//! provider-start path (S3, which also needs a checkpoint scope). Not yet
//! ported: provider switch, fork and merge-back transfers.

use turin_contracts::{
    NodeId, PositiveInt, ProviderThreadId, RunId, ThreadId, UtcDateTime,
    chat_attachment::ChatAttachment,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2AppThread, OrchestrationV2CheckpointScope,
        OrchestrationV2Command as C, OrchestrationV2ContextTransferStatus,
        OrchestrationV2ContextTransferType, OrchestrationV2ConversationMessage,
        OrchestrationV2ConversationMessageRole, OrchestrationV2DeliveryIntent,
        OrchestrationV2DispatchMode, OrchestrationV2DomainEvent, OrchestrationV2ExecutionNode,
        OrchestrationV2ExecutionNodeKind, OrchestrationV2ExecutionNodeStatus,
        OrchestrationV2LimitRecovery, OrchestrationV2ProviderFailureClass,
        OrchestrationV2ProviderThread, OrchestrationV2ProviderThreadStatus,
        OrchestrationV2ProviderTurnStatus, OrchestrationV2Run, OrchestrationV2RunAttempt,
        OrchestrationV2RunAttemptReason, OrchestrationV2RunAttemptStatus, OrchestrationV2RunStatus,
        OrchestrationV2RuntimeRequestStatus, OrchestrationV2ServerCommand,
        OrchestrationV2SettleOverride, OrchestrationV2ThreadLaunchWorkspaceStrategy,
        OrchestrationV2ThreadProjection, OrchestrationV2TitleRegeneration, OrchestrationV2TurnItem,
        OrchestrationV2UserMessageInputIntent, latest_provider_turn_for_attempt,
    },
    provider_instance::ProviderInstanceId,
};
use turin_provider_core::id_allocator::derive as ids;

use crate::pipeline::command_policy::{
    CapabilityCheck, DeliveryIntent, RequestedDispatchMode, ensure_queued_messages,
    resolve_message_dispatch_intent,
};
use crate::pipeline::effect_outbox::{EffectRequest, PendingEffect, ThreadTitleKind};
use crate::pipeline::orchestrator_port::OrchestratorError;
use crate::pipeline::runtime_policy::{self, RuntimePolicyOverride};
use crate::projection::shared::thread_error::{
    latest_executed_run, latest_root_provider_failure, usage_limit_blocked_run,
};

use super::Orchestrator;
use super::dispatch::{Decision, command_id, dispatch_failure};
use super::services::PrepareRootRunScope;

/// Emits one run-scoped event. Every such event carries the same envelope.
macro_rules! run_scoped_event {
    ($decision:expr, $variant:ident, $thread:expr, $run:expr, $node:expr, $instance:expr, $payload:expr) => {{
        let id = $decision.event_id($thread);
        let occurred_at = $decision.now();
        $decision.emit(OrchestrationV2DomainEvent::$variant {
            id,
            thread_id: $thread.clone(),
            run_id: Some($run.clone()),
            node_id: Some($node.clone()),
            driver: None,
            provider_instance_id: Some($instance.clone()),
            raw_event_id: None,
            occurred_at,
            payload: $payload,
        });
    }};
}

/// Decides `message.dispatch`. `Ok(false)` leaves the command to another group.
pub(super) async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    let OrchestrationV2ServerCommand::Client(client @ C::MessageDispatch { .. }) = command else {
        return Ok(false);
    };
    dispatch_message(orchestrator, command, client, decision).await?;
    Ok(true)
}

/// `dispatchMessage`. Emits the decision's events and effects; the caller
/// commits them under the thread lock.
async fn dispatch_message(
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
        source_plan_ref,
        restart_continuation_of_run_id,
        usage_limit_continuation_of_run_id,
        manual_continuation_of_run_id,
        usage_limit_recovery_request_id,
        delivery_intent,
        delegated_completion,
        dispatch_mode,
        ..
    } = client
    else {
        unreachable!("decide routes only message.dispatch here");
    };

    let mut projection = orchestrator
        .projection
        .get_thread_projection(thread_id)
        .await
        .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;

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

    if restart_continuation_of_run_id.is_some() {
        return Err(dispatch_failure(
            command,
            "Restart continuation is not ported yet (T13e).",
        ));
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
    if projection.thread.snoozed_until.is_some() {
        let now = decision.now();
        let mut thread = projection.thread.clone();
        thread.snoozed_until = None;
        thread.snoozed_at = None;
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

    // Not ported (S6 and the plan reads): these return before any event.
    if delegated_completion.is_some() {
        return Err(dispatch_failure(
            command,
            "Delegated completion dispatch is not ported yet.",
        ));
    }
    if source_plan_ref.is_some() {
        return Err(dispatch_failure(
            command,
            "Proposed-plan dispatch is not ported yet.",
        ));
    }

    if notification.is_some()
        && (*created_by != OrchestrationV2Actor::Agent
            || !matches!(
                creation_source,
                turin_contracts::orchestration_v2::OrchestrationV2CreationSource::Server
                    | turin_contracts::orchestration_v2::OrchestrationV2CreationSource::Provider
            ))
    {
        return Err(dispatch_failure(
            command,
            "Notifications must be server- or provider-created queued messages.",
        ));
    }

    let requested_model = model_selection
        .clone()
        .unwrap_or_else(|| projection.thread.model_selection.clone());
    let dispatch = resolve_dispatch_mode(&projection, dispatch_mode, *delivery_intent);
    if dispatch == Dispatch::SteerOrRestart {
        return Err(dispatch_failure(
            command,
            "Steering or restarting the active run is not ported yet (T13c S5).",
        ));
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
            },
            decision,
        )
        .await;
    }

    Err(dispatch_failure(
        command,
        "Starting a provider turn is not ported yet (T13c S3).",
    ))
}

/// The dispatch mode after the server resolves delivery intent. `Start`
/// covers `start_immediately` and `defer_start`; the defer distinction only
/// matters to the start path (S3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dispatch {
    QueueAfterActive,
    Start,
    SteerOrRestart,
}

fn resolve_dispatch_mode(
    projection: &OrchestrationV2ThreadProjection,
    requested: &OrchestrationV2DispatchMode,
    delivery: Option<OrchestrationV2DeliveryIntent>,
) -> Dispatch {
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
        RequestedDispatchMode::SteerActive { .. } | RequestedDispatchMode::RestartActive { .. } => {
            Dispatch::SteerOrRestart
        }
        RequestedDispatchMode::QueueAfterActive => Dispatch::QueueAfterActive,
        RequestedDispatchMode::StartImmediately => Dispatch::Start,
    }
}

fn is_blocking_run(run: &OrchestrationV2Run) -> bool {
    matches!(
        run.status,
        OrchestrationV2RunStatus::Preparing
            | OrchestrationV2RunStatus::Starting
            | OrchestrationV2RunStatus::Running
            | OrchestrationV2RunStatus::Waiting
    )
}

/// `isNativeMaintenanceCommand`: `/compact` and `/logout` without attachments.
fn is_native_maintenance_command(text: &str, attachments: &[ChatAttachment]) -> bool {
    attachments.is_empty() && matches!(text.trim().to_lowercase().as_str(), "/compact" | "/logout")
}

fn emit_thread_event(
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

/// What the queued branch needs: the command itself (its fields are read
/// where they are used), the run it waits behind and the model it runs on.
struct QueueInput<'a> {
    client: &'a C,
    queue_behind: &'a OrchestrationV2Run,
    model_selection: &'a ModelSelection,
    /// The run was held because its worktree never came up.
    worktree_missing: bool,
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
        created_by,
        creation_source,
        scheduled_task_id,
        sender_thread_id,
        message_id,
        text,
        context,
        attachments,
        notification,
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
        delegated_task_id: None,
        work_started_at: None,
        restart_cancelled_background_work: None,
        source_plan_ref: None,
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
    let message = OrchestrationV2ConversationMessage {
        created_by: *created_by,
        creation_source: *creation_source,
        scheduled_task_id: scheduled_task_id.clone(),
        sender_thread_id: sender_thread_id.clone(),
        id: message_id.clone(),
        thread_id: thread.id.clone(),
        run_id: Some(run_id.clone()),
        node_id: Some(root_node_id.clone()),
        role: OrchestrationV2ConversationMessageRole::User,
        text: text.to_owned(),
        context: context.clone(),
        attachments: attachments.to_vec(),
        streaming: false,
        created_at: now,
        updated_at: now,
        delegated_completion: None,
        notification: notification.clone(),
    };

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
async fn prepare_root_scope(
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
    let thread = &projection.thread;
    let project_root = orchestrator
        .seams
        .project_roots
        .workspace_root(&thread.project_id)
        .await
        .map_err(|cause| dispatch_failure(command, cause.to_string()))?
        .ok_or_else(|| {
            dispatch_failure(
                command,
                format!("Project {} has no workspace root.", thread.project_id),
            )
        })?;
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
        .map_err(|cause| dispatch_failure(command, cause.to_string()))
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
fn next_run_ordinal(projection: &OrchestrationV2ThreadProjection) -> u64 {
    projection.runs.len() as u64 + 1
}

/// `rootProviderThreadsForProvider`: the thread's own provider threads on
/// this instance, newest first.
fn root_provider_threads_for_provider(
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
