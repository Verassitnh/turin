//! Port of upstream `Orchestrator.ts` `startNextQueuedRun` (1294),
//! `failQueuedRunStart` (1190), `resumeQueuedRuns` (1930) and the
//! `queue.resume` decision (10925).
//!
//! Every terminal run checks the queue; the next deliverable queued run
//! becomes a `starting` run with a `provider-turn.start` effect. These are
//! system writes (no client command), under the thread's lock.
//!
//! Not ported, and failed loudly through [`Orchestrator::fail_queued_run_start`]
//! rather than skipped: a queued run that needs a provider handoff
//! (`ContextHandoffService` is not ported) or a legacy-import recovery handoff.

use turin_contracts::{
    CommandId, RunId, ThreadId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2Command as C, OrchestrationV2DomainEvent, OrchestrationV2ExecutionNode,
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2ProviderFailureClass,
        OrchestrationV2ProviderThread, OrchestrationV2ProviderThreadStatus, OrchestrationV2Run,
        OrchestrationV2RunAttemptStatus, OrchestrationV2RunStatus, OrchestrationV2ServerCommand,
        OrchestrationV2ThreadHistoryOrigin, OrchestrationV2ThreadLaunchWorkspaceStrategy,
        OrchestrationV2ThreadProjection, OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
        OrchestrationV2UserMessageInputIntent,
    },
};
use turin_provider_core::{
    failure::{ProviderFailureInput, make_provider_failure},
    id_allocator::{IdAllocator, derive as ids},
    notification::{NotificationMessage, notification_turn_item},
};

use super::Orchestrator;
use super::dispatch::{Decision, dispatch_failure};
use super::message_dispatch::{
    is_blocking_run, next_turn_item_ordinal, prepare_root_scope_raw, provider_session_id_for_raw,
    wake_work_started_at,
};
use super::provider_detach;
use crate::core::provider_session_transition_policy::{
    ProviderSessionTransition, model_selections_equal,
};
use crate::pipeline::effect_outbox::{EffectRequest, PendingEffect};
use crate::pipeline::event_sink::WriteInput;
use crate::pipeline::orchestrator_port::OrchestratorError;
use crate::projection::queued_run_order::queued_runs_in_delivery_order;
use crate::projection::shared::thread_error::{
    latest_executed_run, latest_root_provider_failure, usage_limit_blocked_run,
};

/// What `handleTerminalRun` tells the queue about the run that just ended.
#[derive(Debug, Clone, Default)]
pub struct QueueStartOptions {
    pub failed_run_id: Option<RunId>,
    pub ended_run_id: Option<RunId>,
}

/// Decides `queue.resume`. `Ok(false)` leaves the command to another group.
pub(super) async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    let OrchestrationV2ServerCommand::Client(C::QueueResume { thread_id, .. }) = command else {
        return Ok(false);
    };
    let projection = orchestrator
        .projection
        .get_thread_projection(thread_id)
        .await
        .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;
    if projection.thread.archived_at.is_some() || projection.thread.deleted_at.is_some() {
        return Err(dispatch_failure(
            command,
            format!("Thread {thread_id} is not active."),
        ));
    }
    if usage_limit_blocked_run(
        &projection.runs,
        &projection.turn_items,
        session_error(&projection).as_deref(),
    )
    .is_some()
    {
        return Err(dispatch_failure(
            command,
            "Continue the limited thread before resuming its queue.",
        ));
    }
    let now = decision.now();
    let queued: Vec<_> = projection
        .runs
        .iter()
        .filter(|run| run.status == OrchestrationV2RunStatus::Queued)
        .collect();
    for run in &queued {
        let id = decision.event_id(thread_id);
        decision.emit(OrchestrationV2DomainEvent::RunUpdated {
            id,
            thread_id: thread_id.clone(),
            run_id: Some(run.id.clone()),
            node_id: None,
            driver: None,
            provider_instance_id: Some(run.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: OrchestrationV2Run {
                queue_held: Some(false),
                ..(*run).clone()
            },
        });
    }
    if queued.is_empty() {
        let id = decision.event_id(thread_id);
        decision.emit(OrchestrationV2DomainEvent::ThreadMetadataUpdated {
            id,
            thread_id: thread_id.clone(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id: None,
            raw_event_id: None,
            occurred_at: now,
            payload: projection.thread.clone(),
        });
    }
    Ok(true)
}

/// The latest `lastError` of a session on the thread's current provider.
fn session_error(projection: &OrchestrationV2ThreadProjection) -> Option<String> {
    projection
        .provider_sessions
        .iter()
        .filter(|session| session.provider_instance_id == projection.thread.provider_instance_id)
        .max_by_key(|session| session.updated_at)
        .and_then(|session| session.last_error.clone())
}

fn is_handoff_source_run(run: &OrchestrationV2Run) -> bool {
    matches!(
        run.status,
        OrchestrationV2RunStatus::Completed
            | OrchestrationV2RunStatus::Failed
            | OrchestrationV2RunStatus::Interrupted
    )
}

/// `lastDeliveredRunForProviderThread`.
fn last_delivered_run_for_provider_thread<'a>(
    projection: &'a OrchestrationV2ThreadProjection,
    provider_thread_id: &turin_contracts::ProviderThreadId,
) -> Option<&'a OrchestrationV2Run> {
    projection.runs.iter().rev().find(|run| {
        is_handoff_source_run(run)
            && run.provider_thread_id.as_ref() == Some(provider_thread_id)
            && (run.status == OrchestrationV2RunStatus::Completed
                || projection
                    .provider_turns
                    .iter()
                    .any(|turn| turn.run_attempt_id == run.active_attempt_id))
    })
}

impl Orchestrator {
    /// `startNextQueuedRun` under the thread's lock. A failure to start the
    /// queued run marks it failed (`failQueuedRunStart`), so one bad message
    /// does not stall the queue.
    pub async fn start_next_queued_run(
        &self,
        thread_id: &ThreadId,
        options: &QueueStartOptions,
    ) -> Result<(), OrchestratorError> {
        self.locks
            .with_lock(thread_id, || {
                self.start_next_queued_run_locked(thread_id, options)
            })
            .await
    }

    /// [`Self::start_next_queued_run`] for a caller that already holds the lock.
    pub(super) async fn start_next_queued_run_locked(
        &self,
        thread_id: &ThreadId,
        options: &QueueStartOptions,
    ) -> Result<(), OrchestratorError> {
        match self.try_start_next_queued_run(thread_id, options).await {
            Ok(()) => Ok(()),
            Err(cause) => self.fail_queued_run_start(thread_id, &cause).await,
        }
    }

    /// `resumeQueuedRuns`: after a restart, every thread with a startable
    /// queued run gets its next run started. Returns how many resumed.
    pub async fn resume_queued_runs(&self) -> Result<usize, OrchestratorError> {
        let thread_ids = self
            .projection
            .get_recovery_thread_ids(
                crate::projection::store::recovery::ProjectionRecoveryKind::QueuedRuns,
            )
            .await
            .map_err(|cause| OrchestratorError::projection(super::shell_error_thread(), cause))?;
        let mut resumed = 0;
        for thread_id in thread_ids {
            let outcome: Result<bool, OrchestratorError> = async {
                if !self
                    .projection
                    .can_start_queued_run(&thread_id)
                    .await
                    .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?
                {
                    return Ok(false);
                }
                self.start_next_queued_run(&thread_id, &QueueStartOptions::default())
                    .await?;
                Ok(true)
            }
            .await;
            match outcome {
                Ok(true) => resumed += 1,
                Ok(false) => {}
                Err(cause) => tracing::warn!(
                    %thread_id,
                    %cause,
                    "Failed to resume queued V2 run after recovery"
                ),
            }
        }
        Ok(resumed)
    }

    /// `handleTerminalRun`'s queue step: a run reached a terminal status, so
    /// the thread's next queued run may start.
    pub async fn handle_terminal_run(&self, run: &OrchestrationV2Run) {
        let options = QueueStartOptions {
            ended_run_id: Some(run.id.clone()),
            failed_run_id: (run.status == OrchestrationV2RunStatus::Failed).then(|| run.id.clone()),
        };
        if let Err(cause) = self.start_next_queued_run(&run.thread_id, &options).await {
            tracing::warn!(
                thread_id = %run.thread_id,
                %cause,
                "Failed to start the next queued V2 run"
            );
        }
    }

    async fn system_write(
        &self,
        thread_id: &ThreadId,
        decision: Decision,
    ) -> Result<(), OrchestratorError> {
        if decision.events.is_empty() && decision.effects.is_empty() {
            return Ok(());
        }
        self.sink
            .write(WriteInput {
                command_id: None,
                events: decision.events,
                effects: decision.effects,
                guard_pending_user_input_cancellations: false,
            })
            .await
            .map(|_| ())
            .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))
    }

    async fn try_start_next_queued_run(
        &self,
        thread_id: &ThreadId,
        options: &QueueStartOptions,
    ) -> Result<(), OrchestratorError> {
        let read_error = |cause: crate::projection::error::ProjectionStoreError| {
            OrchestratorError::projection(thread_id.clone(), cause)
        };
        // Every terminal run checks the queue. Only a deliverable queued run
        // needs the transcript for provider handoff and legacy import context.
        if !self
            .projection
            .can_start_queued_run(thread_id)
            .await
            .map_err(read_error)?
        {
            return Ok(());
        }
        let projection = self
            .projection
            .get_thread_projection(thread_id)
            .await
            .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;
        if projection.thread.archived_at.is_some()
            || projection.thread.deleted_at.is_some()
            || projection.runs.iter().any(is_blocking_run)
            || projection.runs.iter().any(|run| {
                run.status == OrchestrationV2RunStatus::Queued && run.queue_held == Some(true)
            })
        {
            return Ok(());
        }
        // The limit already stopped this thread. Starting the queue would send
        // every waiting message and drop it from the queue as each one fails.
        if usage_limit_blocked_run(
            &projection.runs,
            &projection.turn_items,
            session_error(&projection).as_deref(),
        )
        .is_some()
        {
            return Ok(());
        }
        let Some(queued_run) =
            queued_runs_in_delivery_order(&projection.runs, &projection.messages)
                .first()
                .map(|run| (*run).clone())
        else {
            return Ok(());
        };
        let command_id = CommandId::new(format!("command:system:start-queued:{}", queued_run.id))
            .expect("system command id");
        let ids = IdAllocator;
        let now = UtcDateTime::now();

        // A provider that just failed will likely fail the next message too.
        // Hold the queue so the user decides when to resume it. Validation
        // failures (setup, unsupported handoff) belong to that message alone,
        // and a message queued for another provider is how users recover. A
        // message queued behind a worktree that was cancelled or never created
        // would run in the project checkout instead, so it waits for the user.
        let failed_run = latest_executed_run(&projection.runs);
        let failure_class = failed_run
            .filter(|run| Some(&run.id) == options.failed_run_id.as_ref())
            .and_then(|run| latest_root_provider_failure(Some(run), &projection.turn_items))
            .map(|failure| failure.class);
        let worktree_missing = failed_run.is_some_and(|run| {
            Some(&run.id) == options.ended_run_id.as_ref()
                && matches!(
                    run.workspace_preparation,
                    Some(OrchestrationV2ThreadLaunchWorkspaceStrategy::Worktree { .. })
                )
                && projection.thread.worktree_path.is_none()
        });
        // A project can turn a provider off while a message waits in the queue,
        // including between a run ending and this check, or while the queue is
        // held and someone clicks resume. Neither path may start that run.
        let instance_disabled = self
            .seams
            .provider_instances
            .disabled_for_project(
                &projection.thread.project_id,
                &queued_run.provider_instance_id,
            )
            .await
            .map_err(|cause| dispatch_error_for(&command_id, cause.to_string()))?;
        if worktree_missing
            || instance_disabled
            || (failure_class
                .is_some_and(|class| class != OrchestrationV2ProviderFailureClass::ValidationError)
                && failed_run
                    .is_some_and(|run| run.provider_instance_id == queued_run.provider_instance_id))
        {
            let mut hold = Decision::new(command_id.clone(), ids, None);
            for run in projection
                .runs
                .iter()
                .filter(|run| run.status == OrchestrationV2RunStatus::Queued)
            {
                let id = hold.event_id(thread_id);
                hold.emit(OrchestrationV2DomainEvent::RunUpdated {
                    id,
                    thread_id: thread_id.clone(),
                    run_id: Some(run.id.clone()),
                    node_id: None,
                    driver: None,
                    provider_instance_id: Some(run.provider_instance_id.clone()),
                    raw_event_id: None,
                    occurred_at: now,
                    payload: OrchestrationV2Run {
                        queue_held: Some(true),
                        ..run.clone()
                    },
                });
            }
            return self.system_write(thread_id, hold).await;
        }

        let missing = |what: &str| {
            dispatch_error_for(
                &CommandId::new(format!("command:system:start-queued:{}", queued_run.id))
                    .expect("system command id"),
                format!("Queued run {} is missing {what}.", queued_run.id),
            )
        };
        let (Some(root_node_id), Some(attempt_id), Some(provider_thread_id)) = (
            queued_run.root_node_id.clone(),
            queued_run.active_attempt_id.clone(),
            queued_run.provider_thread_id.clone(),
        ) else {
            return Err(missing("execution identity"));
        };
        let root_node = projection.nodes.iter().find(|node| node.id == root_node_id);
        let attempt = projection
            .attempts
            .iter()
            .find(|entry| entry.id == attempt_id);
        let queued_message = projection
            .messages
            .iter()
            .find(|message| message.id == queued_run.user_message_id);
        let legacy_queued_turn_item = projection.turn_items.iter().find(|item| {
            matches!(
                item,
                OrchestrationV2TurnItem::UserMessage { run_id, message_id, .. }
                    if run_id.as_ref() == Some(&queued_run.id)
                        && message_id == &queued_run.user_message_id
            )
        });
        let queued_provider_thread = projection
            .provider_threads
            .iter()
            .find(|candidate| candidate.id == provider_thread_id);
        let stored_checkpoint_scope = root_node
            .and_then(|node| node.checkpoint_scope_id.as_ref())
            .and_then(|id| {
                projection
                    .checkpoint_scopes
                    .iter()
                    .find(|scope| &scope.id == id)
            });
        let (Some(root_node), Some(_attempt), Some(queued_message), Some(queued_provider_thread)) =
            (root_node, attempt, queued_message, queued_provider_thread)
        else {
            return Err(missing("projection state"));
        };
        if root_node.checkpoint_scope_id.is_some() && stored_checkpoint_scope.is_none() {
            return Err(missing("projection state"));
        }

        let selection_changed = !model_selections_equal(
            &projection.thread.model_selection,
            &queued_run.model_selection,
        );
        let switch_plan = if selection_changed {
            Some(
                self.seams
                    .switch_planner
                    .plan(&projection, &queued_run.model_selection)
                    .await
                    .map_err(|cause| dispatch_error_for(&command_id, cause.to_string()))?,
            )
        } else {
            None
        };
        let active_provider_thread = projection.provider_threads.iter().find(|candidate| {
            Some(&candidate.id) == projection.thread.active_provider_thread_id.as_ref()
        });
        let can_resume_across_instances = switch_plan.as_ref().is_some_and(|plan| {
            plan.instance_changed && plan.transition == ProviderSessionTransition::RestartAndResume
        }) && active_provider_thread
            .is_some_and(|active| active.native_thread_ref.is_some());
        let delivery_provider_thread = match (can_resume_across_instances, active_provider_thread) {
            (true, Some(active)) => OrchestrationV2ProviderThread {
                native_thread_ref: active.native_thread_ref.clone(),
                native_conversation_head_ref: active.native_conversation_head_ref.clone(),
                native_metadata: active.native_metadata.clone(),
                ..queued_provider_thread.clone()
            },
            _ => queued_provider_thread.clone(),
        };
        let target_adapter = self
            .seams
            .adapters
            .adapter(&queued_run.provider_instance_id)
            .ok_or_else(|| {
                dispatch_error_for(
                    &command_id,
                    format!(
                        "Provider instance {} is not available.",
                        queued_run.provider_instance_id
                    ),
                )
            })?;
        let latest_completed_run = projection
            .runs
            .iter()
            .rev()
            .find(|run| run.status == OrchestrationV2RunStatus::Completed);
        let latest_handoff_run = projection
            .runs
            .iter()
            .rev()
            .find(|run| is_handoff_source_run(run));
        let target_last_completed_run =
            last_delivered_run_for_provider_thread(&projection, &queued_provider_thread.id);
        let covered_runs: Vec<&OrchestrationV2Run> = match latest_handoff_run {
            Some(latest)
                if !can_resume_across_instances
                    && latest.provider_instance_id != queued_run.provider_instance_id =>
            {
                projection
                    .runs
                    .iter()
                    .filter(|run| {
                        is_handoff_source_run(run)
                            && run.ordinal.get()
                                > target_last_completed_run.map_or(0, |run| run.ordinal.get())
                            && run.ordinal.get() <= latest.ordinal.get()
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        let needs_full_context = delivery_provider_thread.native_thread_ref.is_none();
        if !covered_runs.is_empty() {
            return Err(dispatch_error_for(
                &command_id,
                "Handing the conversation to another provider needs the context handoff service, which is not ported yet.".to_owned(),
            ));
        }
        if latest_completed_run.is_none()
            && needs_full_context
            && projection.thread.history_origin
                == Some(OrchestrationV2ThreadHistoryOrigin::V1Import)
        {
            return Err(dispatch_error_for(
                &command_id,
                "Legacy import handoff is not ported yet.".to_owned(),
            ));
        }

        let checkpoint_scope = match stored_checkpoint_scope {
            Some(scope) => scope.clone(),
            None => prepare_root_scope_raw(
                self,
                &projection.thread,
                None,
                &queued_run.id,
                &root_node.id,
                &queued_provider_thread.id,
                now,
            )
            .await
            .map_err(|cause| dispatch_error_for(&command_id, cause))?,
        };
        let release_ids = switch_plan
            .as_ref()
            .map(|plan| plan.release_provider_session_ids.clone())
            .unwrap_or_default();
        let provider_session_id = match queued_provider_thread.provider_session_id.clone() {
            Some(existing) if !can_resume_across_instances && !release_ids.contains(&existing) => {
                existing
            }
            _ => provider_session_id_for_raw(
                target_adapter.as_ref(),
                &queued_run.provider_instance_id,
                thread_id,
            )
            .await
            .map_err(|cause| dispatch_error_for(&command_id, cause))?,
        };
        let provider_thread = OrchestrationV2ProviderThread {
            provider_session_id: Some(provider_session_id),
            status: OrchestrationV2ProviderThreadStatus::NotLoaded,
            first_run_ordinal: queued_provider_thread
                .first_run_ordinal
                .or(Some(queued_run.ordinal)),
            last_run_ordinal: Some(queued_run.ordinal),
            updated_at: now,
            ..delivery_provider_thread
        };
        let starting_run = OrchestrationV2Run {
            status: OrchestrationV2RunStatus::Starting,
            queue_position: Some(None),
            started_at: None,
            context_handoff_id: None,
            work_started_at: wake_work_started_at(
                &projection.runs,
                queued_message.notification.is_some()
                    || queued_message.delegated_completion.is_some()
                    || queued_run.restart_continuation_of_run_id.is_some(),
            ),
            ..queued_run.clone()
        };
        let user_turn_item = match legacy_queued_turn_item {
            Some(item) => {
                let mut wire = serde_json::to_value(item).expect("turn item encodes");
                if let Some(fields) = wire.as_object_mut() {
                    fields.insert("inputIntent".to_owned(), serde_json::json!("queued_turn"));
                    fields.insert("startedAt".to_owned(), serde_json::json!(now));
                    fields.insert("completedAt".to_owned(), serde_json::json!(now));
                    fields.insert("updatedAt".to_owned(), serde_json::json!(now));
                }
                serde_json::from_value(wire).expect("queued turn item decodes")
            }
            None => OrchestrationV2TurnItem::UserMessage {
                tool_non_execution_kind: None,
                tool_surface: None,
                tool_icon: None,
                tool_source: None,
                id: ids::user_turn_item(&queued_message.id),
                thread_id: thread_id.clone(),
                run_id: Some(queued_run.id.clone()),
                node_id: Some(root_node_id.clone()),
                provider_thread_id: Some(queued_provider_thread.id.clone()),
                provider_turn_id: None,
                native_item_ref: None,
                parent_item_id: None,
                ordinal: queued_run.ordinal.get() * 100,
                status: OrchestrationV2TurnItemStatus::Completed,
                title: None,
                started_at: Some(now),
                completed_at: Some(now),
                updated_at: now,
                created_by: queued_message.created_by,
                creation_source: queued_message.creation_source,
                message_id: queued_message.id.clone(),
                scheduled_task_id: queued_message.scheduled_task_id.clone(),
                sender_thread_id: queued_message.sender_thread_id.clone(),
                input_intent: OrchestrationV2UserMessageInputIntent::QueuedTurn,
                text: queued_message.text.clone(),
                context: queued_message.context.clone(),
                attachments: queued_message.attachments.clone(),
            },
        };
        let sessions_to_detach: Vec<_> = projection
            .provider_sessions
            .iter()
            .filter(|session| release_ids.contains(&session.id))
            .cloned()
            .collect();

        let mut write = Decision::new(command_id.clone(), ids, None);
        let instance = &queued_run.provider_instance_id;
        if selection_changed {
            let same_instance = *instance == projection.thread.provider_instance_id;
            let mut thread = projection.thread.clone();
            thread.provider_instance_id = instance.clone();
            thread.model_selection = queued_run.model_selection.clone();
            thread.updated_at = now;
            super::message_dispatch::emit_thread_event(
                &mut write,
                if same_instance {
                    "model-selection-updated"
                } else {
                    "provider-switched"
                },
                &thread,
                now,
            );
        }
        provider_detach::detach_sessions(
            &mut write,
            thread_id,
            &sessions_to_detach,
            "Provider or model selection changed.",
            false,
        );
        if stored_checkpoint_scope.is_none() {
            super::message_dispatch::run_scoped_event!(
                write,
                CheckpointScopeCreated,
                thread_id,
                &queued_run.id,
                &root_node.id,
                instance,
                checkpoint_scope.clone()
            );
            super::message_dispatch::run_scoped_event!(
                write,
                NodeUpdated,
                thread_id,
                &queued_run.id,
                &root_node.id,
                instance,
                OrchestrationV2ExecutionNode {
                    checkpoint_scope_id: Some(checkpoint_scope.id.clone()),
                    ..root_node.clone()
                }
            );
        }
        let id = write.event_id(thread_id);
        write.emit(OrchestrationV2DomainEvent::ProviderThreadUpdated {
            id,
            thread_id: thread_id.clone(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id: Some(instance.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: provider_thread,
        });
        let item = notification_turn_item(
            &user_turn_item,
            NotificationMessage::from(queued_message),
            &projection.subagents,
        );
        super::message_dispatch::run_scoped_event!(
            write,
            TurnItemUpdated,
            thread_id,
            &queued_run.id,
            &root_node_id,
            instance,
            item
        );
        super::message_dispatch::run_scoped_event!(
            write,
            RunUpdated,
            thread_id,
            &queued_run.id,
            &root_node_id,
            instance,
            starting_run
        );
        write.effects.push(PendingEffect {
            id: format!("effect:{command_id}:provider-turn.start:{}", queued_run.id),
            command_id: command_id.clone(),
            thread_id: thread_id.clone(),
            request: EffectRequest::ProviderTurnStart {
                run_id: queued_run.id.clone(),
            },
            available_at: None,
        });
        self.system_write(thread_id, write).await
    }

    /// `failQueuedRunStart`: the next queued run could not start, so it ends
    /// failed with an error item and the queue moves on.
    async fn fail_queued_run_start(
        &self,
        thread_id: &ThreadId,
        cause: &OrchestratorError,
    ) -> Result<(), OrchestratorError> {
        let projection = self.get_thread_records_for_fail(thread_id).await?;
        let Some(queued_run) =
            queued_runs_in_delivery_order(&projection.runs, &projection.messages)
                .first()
                .map(|run| (*run).clone())
        else {
            return Ok(());
        };
        let now = UtcDateTime::now();
        let root_node = queued_run
            .root_node_id
            .as_ref()
            .and_then(|id| projection.nodes.iter().find(|node| &node.id == id));
        let attempt = queued_run
            .active_attempt_id
            .as_ref()
            .and_then(|id| projection.attempts.iter().find(|entry| &entry.id == id));
        let provider_thread = queued_run.provider_thread_id.as_ref().and_then(|id| {
            projection
                .provider_threads
                .iter()
                .find(|entry| &entry.id == id)
        });
        let message = match cause {
            OrchestratorError::Dispatch { cause, .. } => cause.to_string(),
            other => other.to_string(),
        };
        let failure = make_provider_failure(ProviderFailureInput {
            cause: None,
            message: Some(&message),
            code: Some(Some("queued_start_failed")),
            class: Some(OrchestrationV2ProviderFailureClass::Unknown),
            retryable: None,
            reset_at: None,
        });
        let command_id = CommandId::new(format!(
            "command:system:queued-start-failed:{}",
            queued_run.id
        ))
        .expect("system command id");
        let mut write = Decision::new(command_id, IdAllocator, None);
        let instance = &queued_run.provider_instance_id;
        if let (Some(attempt), Some(root_node)) = (attempt, root_node) {
            super::message_dispatch::run_scoped_event!(
                write,
                RunAttemptUpdated,
                thread_id,
                &queued_run.id,
                &root_node.id,
                instance,
                turin_contracts::orchestration_v2::OrchestrationV2RunAttempt {
                    status: OrchestrationV2RunAttemptStatus::Failed,
                    completed_at: Some(now),
                    ..attempt.clone()
                }
            );
        }
        if let Some(root_node) = root_node {
            super::message_dispatch::run_scoped_event!(
                write,
                NodeUpdated,
                thread_id,
                &queued_run.id,
                &root_node.id,
                instance,
                OrchestrationV2ExecutionNode {
                    status: OrchestrationV2ExecutionNodeStatus::Failed,
                    completed_at: Some(now),
                    ..root_node.clone()
                }
            );
        }
        if let (Some(root_node), Some(provider_thread)) = (root_node, provider_thread) {
            let ordinal = next_turn_item_ordinal(self, &projection).await?;
            let item = OrchestrationV2TurnItem::Error {
                tool_non_execution_kind: None,
                tool_surface: None,
                tool_icon: None,
                tool_source: None,
                id: ids::turn_item_from_provider_item(
                    &provider_thread.driver,
                    &format!("queued-start-failure:{}", queued_run.id),
                ),
                thread_id: thread_id.clone(),
                run_id: Some(queued_run.id.clone()),
                node_id: Some(root_node.id.clone()),
                provider_thread_id: Some(provider_thread.id.clone()),
                provider_turn_id: None,
                native_item_ref: None,
                parent_item_id: None,
                ordinal,
                status: OrchestrationV2TurnItemStatus::Failed,
                title: Some("Queued provider could not start".to_owned()),
                started_at: Some(now),
                completed_at: Some(now),
                updated_at: now,
                failure,
                retry: None,
            };
            super::message_dispatch::run_scoped_event!(
                write,
                TurnItemUpdated,
                thread_id,
                &queued_run.id,
                &root_node.id,
                instance,
                item
            );
        }
        let id = write.event_id(thread_id);
        write.emit(OrchestrationV2DomainEvent::RunUpdated {
            id,
            thread_id: thread_id.clone(),
            run_id: Some(queued_run.id.clone()),
            node_id: root_node.map(|node| node.id.clone()),
            driver: None,
            provider_instance_id: Some(instance.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: OrchestrationV2Run {
                status: OrchestrationV2RunStatus::Failed,
                queue_position: Some(None),
                completed_at: Some(now),
                ..queued_run
            },
        });
        self.system_write(thread_id, write).await
    }

    async fn get_thread_records_for_fail(
        &self,
        thread_id: &ThreadId,
    ) -> Result<OrchestrationV2ThreadProjection, OrchestratorError> {
        use crate::pipeline::orchestrator_port::{OrchestratorPort, RecordField, RecordFilter};
        self.get_thread_records(
            thread_id,
            &[
                RecordField::Runs,
                RecordField::Nodes,
                RecordField::Attempts,
                RecordField::ProviderThreads,
                RecordField::TurnItems,
                RecordField::Messages,
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
}

/// A `message.dispatch` failure for a system-started queued run, which has no
/// client command to blame.
fn dispatch_error_for(command_id: &CommandId, cause: String) -> OrchestratorError {
    OrchestratorError::Dispatch {
        command_id: command_id.clone(),
        command_type: "message.dispatch".to_owned(),
        cause: cause.into(),
    }
}
