//! Port of upstream `Orchestrator.ts` delegated-task settling:
//! `finalizeAppOwnedSubagent` (10438), `finalizeAppOwnedSubagentTask` (10137),
//! `planDelegatedCompletionDelivery` (9932), `finalizeDelegatedCompletionDelivery`
//! (10481), `offerDelegatedCompletionDelivery(ies)` (1061), `recoverDelegatedTasks`
//! (11333) and `recoverDelegatedTask` (11466).
//!
//! An app-owned subagent is a child thread. When its run ends, its result moves
//! into the parent thread's task row, node and turn item, and the parent is
//! offered a wake to read it. Every write targets the PARENT thread, so these
//! take the parent's dispatch lock, never the child's.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use turin_contracts::{
    NodeId, RunId, ThreadId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2ContextHandoff, OrchestrationV2ContextHandoffStatus,
        OrchestrationV2ContextHandoffStrategy, OrchestrationV2ContextTransfer,
        OrchestrationV2ContextTransferResolution, OrchestrationV2ContextTransferStatus,
        OrchestrationV2ContextTransferType, OrchestrationV2ConversationMessage,
        OrchestrationV2CoveredRunOrdinals, OrchestrationV2DelegatedCompletionCohort,
        OrchestrationV2DelegatedCompletionCohortDisposition,
        OrchestrationV2DelegatedCompletionDelivery, OrchestrationV2DelegatedCompletionTaskDelivery,
        OrchestrationV2DelegatedCompletionTaskDeliveryState, OrchestrationV2DomainEvent,
        OrchestrationV2ExecutionNode, OrchestrationV2ExecutionNodeStatus,
        OrchestrationV2ForkSource, OrchestrationV2LineageRelationship,
        OrchestrationV2MessageDelegatedCompletion, OrchestrationV2Run, OrchestrationV2RunStatus,
        OrchestrationV2Subagent, OrchestrationV2SubagentOrigin, OrchestrationV2SubagentStatus,
        OrchestrationV2ThreadProjection, OrchestrationV2TurnItem,
    },
};
use turin_provider_core::{
    id_allocator::IdAllocator,
    provider_continuation_requests::{
        ProviderContinuationDelegatedCompletion, ProviderContinuationDelivery,
        ProviderContinuationRequest,
    },
    subagent_projection::{
        DelegatedTaskProgressInput, DelegatedTaskProgressState, delegated_task_progress,
        subagent_result_for_run,
    },
    turn_item::turn_item_common,
};

use super::Orchestrator;
use super::cohort::{completion_delivery_run, patch_turn_item};
use super::dispatch::Decision;
use super::message_dispatch::delegated_completion_wake_detail;
use super::thread_fork::context_source_point_for_run;
use crate::core::restart_background_note::run_outcome;
use crate::core::restart_continuation_service::DelegatedTaskRecovery;
use crate::pipeline::notification_mailbox::is_undelivered_mailbox_steer;
use crate::pipeline::orchestrator_port::{
    OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
};
use crate::projection::shared::thread_error::run_ran_after;
use crate::projection::store::recovery::ProjectionRecoveryKind;

/// `finalizeAppOwnedSubagent`'s options.
#[derive(Debug, Clone, Default)]
pub struct FinalizeOptions {
    /// The restart continuation that just declined or failed, so its own
    /// still-running effect does not hold the child.
    pub settled_continuation_of: Option<RunId>,
}

/// `isTerminalDelegatedTaskStatus`.
fn is_terminal_task_status(status: OrchestrationV2SubagentStatus) -> bool {
    matches!(
        status,
        OrchestrationV2SubagentStatus::Completed
            | OrchestrationV2SubagentStatus::Failed
            | OrchestrationV2SubagentStatus::Cancelled
            | OrchestrationV2SubagentStatus::Interrupted
    )
}

/// `delegatedTaskTerminalStatus`.
fn task_terminal_status(status: OrchestrationV2RunStatus) -> Option<OrchestrationV2SubagentStatus> {
    match status {
        OrchestrationV2RunStatus::Completed => Some(OrchestrationV2SubagentStatus::Completed),
        OrchestrationV2RunStatus::Failed => Some(OrchestrationV2SubagentStatus::Failed),
        OrchestrationV2RunStatus::Cancelled | OrchestrationV2RunStatus::RolledBack => {
            Some(OrchestrationV2SubagentStatus::Cancelled)
        }
        OrchestrationV2RunStatus::Interrupted => Some(OrchestrationV2SubagentStatus::Interrupted),
        _ => None,
    }
}

fn node_status(status: OrchestrationV2SubagentStatus) -> OrchestrationV2ExecutionNodeStatus {
    match status {
        OrchestrationV2SubagentStatus::Completed => OrchestrationV2ExecutionNodeStatus::Completed,
        OrchestrationV2SubagentStatus::Failed => OrchestrationV2ExecutionNodeStatus::Failed,
        OrchestrationV2SubagentStatus::Interrupted => {
            OrchestrationV2ExecutionNodeStatus::Interrupted
        }
        _ => OrchestrationV2ExecutionNodeStatus::Cancelled,
    }
}

fn delivered_or_settled(task: &OrchestrationV2Subagent) -> bool {
    matches!(
        task.completion_delivery
            .as_ref()
            .map(|delivery| delivery.state),
        Some(
            OrchestrationV2DelegatedCompletionTaskDeliveryState::Acknowledged
                | OrchestrationV2DelegatedCompletionTaskDeliveryState::Delivered
                | OrchestrationV2DelegatedCompletionTaskDeliveryState::Disposed
        )
    )
}

fn task_delivery(
    state: OrchestrationV2DelegatedCompletionTaskDeliveryState,
) -> Option<OrchestrationV2DelegatedCompletionTaskDelivery> {
    Some(OrchestrationV2DelegatedCompletionTaskDelivery {
        state,
        observed_by_run_id: None,
    })
}

/// `planDelegatedCompletionDelivery`'s result.
struct CompletionPlan {
    task: OrchestrationV2Subagent,
    parent_run: Option<OrchestrationV2Run>,
    message: Option<OrchestrationV2ConversationMessage>,
    offer: bool,
}

fn plan_completion_delivery(
    parent: &OrchestrationV2ThreadProjection,
    parent_run: Option<&OrchestrationV2Run>,
    task: &OrchestrationV2Subagent,
    updated_task: &OrchestrationV2Subagent,
    now: UtcDateTime,
    new_message_id: impl FnOnce() -> turin_contracts::MessageId,
) -> CompletionPlan {
    use OrchestrationV2DelegatedCompletionTaskDeliveryState as State;
    let unchanged = |task: OrchestrationV2Subagent| CompletionPlan {
        task,
        parent_run: None,
        message: None,
        offer: false,
    };
    // Delivered ownership has already settled through a completed wake run. A
    // later wake-policy upgrade must not re-claim the task or offer again.
    if delivered_or_settled(updated_task) {
        return unchanged(updated_task.clone());
    }
    let cohort = parent_run.and_then(|run| run.delegated_completion.as_ref());
    let Some(parent_run) = parent_run.filter(|_| {
        parent.thread.archived_at.is_none()
            && parent.thread.deleted_at.is_none()
            && cohort.is_none_or(|cohort| {
                cohort.disposition == OrchestrationV2DelegatedCompletionCohortDisposition::Open
            })
    }) else {
        return unchanged(OrchestrationV2Subagent {
            completion_delivery: task_delivery(State::Disposed),
            ..updated_task.clone()
        });
    };
    // settled_only defers to a blocking delegate_task wait, which lives only as
    // long as the turn that spawned the task. Once that turn is over (a restart
    // continues it as a new run), nothing else delivers the result.
    if task
        .completion_wake
        .unwrap_or(turin_contracts::orchestration_v2::OrchestrationV2CompletionWake::SettledOnly)
        == turin_contracts::orchestration_v2::OrchestrationV2CompletionWake::SettledOnly
        && matches!(
            parent_run.status,
            OrchestrationV2RunStatus::Preparing
                | OrchestrationV2RunStatus::Starting
                | OrchestrationV2RunStatus::Running
        )
    {
        return unchanged(updated_task.clone());
    }
    let claimed = |task: &OrchestrationV2Subagent| OrchestrationV2Subagent {
        completion_delivery: task_delivery(State::Claimed),
        ..task.clone()
    };
    let pending = |task: &OrchestrationV2Subagent| OrchestrationV2Subagent {
        completion_delivery: task_delivery(State::Pending),
        ..task.clone()
    };
    let delivery = cohort.and_then(|cohort| cohort.delivery.as_ref());
    if let Some(delivery) = delivery {
        let delivery_run = completion_delivery_run(parent, Some(delivery));
        if let Some(delivery_run) = delivery_run
            && delivery_run.status == OrchestrationV2RunStatus::Queued
            && delivery_run.user_message_id == delivery.message_id
        {
            let mut task_ids = delivery.task_ids.clone();
            if !task_ids.contains(&task.id) {
                task_ids.push(task.id.clone());
            }
            let message = parent
                .messages
                .iter()
                .find(|candidate| candidate.id == delivery.message_id)
                .map(|message| OrchestrationV2ConversationMessage {
                    text: delegated_completion_wake_detail(&task_ids),
                    delegated_completion: Some(OrchestrationV2MessageDelegatedCompletion {
                        parent_run_id: parent_run.id.clone(),
                        generation: delivery.generation,
                        task_ids: task_ids.clone(),
                    }),
                    updated_at: now,
                    ..message.clone()
                });
            return CompletionPlan {
                task: claimed(updated_task),
                parent_run: Some(OrchestrationV2Run {
                    delegated_completion: Some(OrchestrationV2DelegatedCompletionCohort {
                        delivery: Some(OrchestrationV2DelegatedCompletionDelivery {
                            task_ids,
                            ..delivery.clone()
                        }),
                        ..cohort.cloned().expect("a delivery implies a cohort")
                    }),
                    ..parent_run.clone()
                }),
                message,
                offer: false,
            };
        }
        if delivery_run.is_some_and(|run| {
            matches!(
                run.status,
                OrchestrationV2RunStatus::Preparing
                    | OrchestrationV2RunStatus::Starting
                    | OrchestrationV2RunStatus::Running
                    | OrchestrationV2RunStatus::Waiting
            )
        }) {
            return unchanged(pending(updated_task));
        }
        if delivery_run.is_some() {
            // The terminal-run listener owns reconciliation of a completed wake.
            // A sibling that wins the parent lock first remains pending for the
            // successor that listener reserves, rather than creating a competing
            // delivery.
            return unchanged(pending(updated_task));
        }
        let mut task_ids = delivery.task_ids.clone();
        if !task_ids.contains(&task.id) {
            task_ids.push(task.id.clone());
        }
        return CompletionPlan {
            task: claimed(updated_task),
            parent_run: Some(OrchestrationV2Run {
                delegated_completion: Some(OrchestrationV2DelegatedCompletionCohort {
                    delivery: Some(OrchestrationV2DelegatedCompletionDelivery {
                        task_ids,
                        ..delivery.clone()
                    }),
                    ..cohort.cloned().expect("a delivery implies a cohort")
                }),
                ..parent_run.clone()
            }),
            message: None,
            offer: true,
        };
    }

    let generation = cohort.map_or_else(
        || turin_contracts::PositiveInt::new(1).expect("positive"),
        |cohort| cohort.next_generation,
    );
    let next_generation =
        turin_contracts::PositiveInt::new(generation.get() + 1).expect("positive");
    CompletionPlan {
        task: claimed(updated_task),
        parent_run: Some(OrchestrationV2Run {
            delegated_completion: Some(OrchestrationV2DelegatedCompletionCohort {
                disposition: OrchestrationV2DelegatedCompletionCohortDisposition::Open,
                next_generation,
                delivery: Some(OrchestrationV2DelegatedCompletionDelivery {
                    generation,
                    message_id: new_message_id(),
                    task_ids: vec![task.id.clone()],
                }),
            }),
            ..parent_run.clone()
        }),
        message: None,
        offer: true,
    }
}

impl Orchestrator {
    /// Parent thread of an app-owned delegated child, or `None` when the thread
    /// is not one. Thread lineage and fork origin are immutable, so this is safe
    /// to read without holding either thread's dispatch lock.
    pub async fn app_owned_subagent_parent_thread_id(
        &self,
        child_thread_id: &ThreadId,
    ) -> Result<Option<ThreadId>, OrchestratorError> {
        let child = self
            .projection
            .get_thread(child_thread_id)
            .await
            .map_err(|cause| OrchestratorError::projection(child_thread_id.clone(), cause))?;
        Ok(
            if child.lineage.relationship_to_parent
                == Some(OrchestrationV2LineageRelationship::Subagent)
                && matches!(
                    child.forked_from,
                    Some(OrchestrationV2ForkSource::Node { .. })
                )
            {
                child.lineage.parent_thread_id
            } else {
                None
            },
        )
    }

    /// `childAwaitsRestartContinuation`: a delegated child's result is held
    /// while its result run, or a run after it that a second restart cut before
    /// it started, still has a continuation pending.
    async fn child_awaits_restart_continuation(
        &self,
        runs: &[OrchestrationV2Run],
        result_run: &OrchestrationV2Run,
        settled_continuation_of: Option<&RunId>,
    ) -> bool {
        for run in runs.iter().filter(|run| {
            Some(&run.id) != settled_continuation_of
                && (run.id == result_run.id || run_ran_after(*run, result_run))
        }) {
            if self.recovery.restart_continuation_pending(run).await {
                return true;
            }
        }
        false
    }

    /// `finalizeAppOwnedSubagent` under the parent's dispatch lock: moves the
    /// result of a terminal app-owned child into its parent and offers the wake.
    pub async fn finalize_app_owned_subagent(
        &self,
        child_thread_id: &ThreadId,
        options: &FinalizeOptions,
    ) -> Result<(), OrchestratorError> {
        let Some(parent_thread_id) = self
            .app_owned_subagent_parent_thread_id(child_thread_id)
            .await?
        else {
            return Ok(());
        };
        self.locks
            .with_lock(&parent_thread_id, || {
                self.finalize_app_owned_subagent_locked(child_thread_id, options)
            })
            .await
    }

    async fn finalize_app_owned_subagent_locked(
        &self,
        child_thread_id: &ThreadId,
        options: &FinalizeOptions,
    ) -> Result<(), OrchestratorError> {
        let child = self.child_controls(child_thread_id).await?;
        let Some(parent_thread_id) = child.thread.lineage.parent_thread_id.clone() else {
            return Ok(());
        };
        let Some(OrchestrationV2ForkSource::Node {
            node_id: forked_node,
        }) = child.thread.forked_from.clone()
        else {
            return Ok(());
        };
        if child.thread.lineage.relationship_to_parent
            != Some(OrchestrationV2LineageRelationship::Subagent)
        {
            return Ok(());
        }
        let progress = delegated_task_progress(DelegatedTaskProgressInput::from(&child));
        let result_available = progress.state == DelegatedTaskProgressState::ResultAvailable;
        let result_run = progress.result_run.cloned();
        let parent = self
            .get_thread_records(
                &parent_thread_id,
                &[RecordField::Subagents, RecordField::ContextTransfers],
                &RecordFilter::default(),
            )
            .await?;
        for task in &parent.subagents {
            let has_transfer = parent.context_transfers.iter().any(|transfer| {
                transfer.r#type == OrchestrationV2ContextTransferType::SubagentResult
                    && &transfer.source_thread_id == child_thread_id
            });
            if task.origin != OrchestrationV2SubagentOrigin::AppOwned
                || task.child_thread_id.as_ref() != Some(child_thread_id)
                || (task.id == forked_node && !result_available)
                || (task.result.is_some() && has_transfer)
            {
                continue;
            }
            self.finalize_app_owned_subagent_task(
                &child,
                &forked_node,
                &task.id,
                result_run.as_ref(),
                options,
            )
            .await?;
        }
        Ok(())
    }

    async fn child_controls(
        &self,
        child_thread_id: &ThreadId,
    ) -> Result<OrchestrationV2ThreadProjection, OrchestratorError> {
        self.get_thread_records(
            child_thread_id,
            &[
                RecordField::Runs,
                RecordField::Messages,
                RecordField::Subagents,
                RecordField::ProviderThreads,
                RecordField::ProviderTurns,
                RecordField::Attempts,
            ],
            &RecordFilter {
                message_roles: Some(vec![
                    turin_contracts::orchestration_v2::OrchestrationV2ConversationMessageRole::User,
                ]),
                ..RecordFilter::default()
            },
        )
        .await
    }

    /// `finalizeAppOwnedSubagentTask`.
    async fn finalize_app_owned_subagent_task(
        &self,
        child: &OrchestrationV2ThreadProjection,
        forked_node: &NodeId,
        task_id: &NodeId,
        progress_result_run: Option<&OrchestrationV2Run>,
        options: &FinalizeOptions,
    ) -> Result<(), OrchestratorError> {
        let child_thread_id = &child.thread.id;
        let Some(parent_thread_id) = child.thread.lineage.parent_thread_id.clone() else {
            return Ok(());
        };
        let mut task_runs: Vec<&OrchestrationV2Run> = child
            .runs
            .iter()
            .filter(|run| run.delegated_task_id.as_ref() == Some(task_id))
            .collect();
        task_runs.sort_by(|a, b| b.ordinal.get().cmp(&a.ordinal.get()));
        let child_run: &OrchestrationV2Run = match task_runs.first() {
            Some(run) => run,
            None if task_id == forked_node => match progress_result_run {
                Some(run) => run,
                None => return Ok(()),
            },
            None => return Ok(()),
        };
        let outcome = run_outcome(child_run);
        let Some(terminal_status) = task_terminal_status(outcome) else {
            return Ok(());
        };
        let held_runs: Vec<OrchestrationV2Run> = if task_runs.is_empty() {
            child.runs.clone()
        } else {
            task_runs.iter().map(|run| (*run).clone()).collect()
        };
        if self
            .child_awaits_restart_continuation(
                &held_runs,
                child_run,
                options.settled_continuation_of.as_ref(),
            )
            .await
        {
            return Ok(());
        }

        let child_result = self
            .get_thread_records(
                child_thread_id,
                &[RecordField::Messages, RecordField::TurnItems],
                &RecordFilter {
                    message_roles: Some(vec![
                        turin_contracts::orchestration_v2::OrchestrationV2ConversationMessageRole::Assistant,
                    ]),
                    message_run_ids: Some(vec![child_run.id.clone()]),
                    turn_item_run_ids: Some(vec![Some(child_run.id.clone())]),
                    turn_item_types: Some(vec![
                        "assistant_message".to_owned(),
                        "error".to_owned(),
                    ]),
                    ..RecordFilter::default()
                },
            )
            .await?;
        let parent = self
            .get_thread_records(
                &parent_thread_id,
                &[
                    RecordField::Runs,
                    RecordField::Messages,
                    RecordField::Subagents,
                    RecordField::ProviderThreads,
                    RecordField::ProviderTurns,
                    RecordField::Nodes,
                    RecordField::TurnItems,
                    RecordField::ContextTransfers,
                ],
                &RecordFilter {
                    turn_item_types: Some(vec!["subagent".to_owned()]),
                    message_roles: Some(vec![
                        turin_contracts::orchestration_v2::OrchestrationV2ConversationMessageRole::User,
                    ]),
                    ..RecordFilter::default()
                },
            )
            .await?;
        let Some(task) = parent.subagents.iter().find(|candidate| {
            &candidate.id == task_id
                && candidate.origin == OrchestrationV2SubagentOrigin::AppOwned
                && candidate.child_thread_id.as_ref() == Some(child_thread_id)
        }) else {
            return Ok(());
        };
        if parent.context_transfers.iter().any(|transfer| {
            transfer.r#type == OrchestrationV2ContextTransferType::SubagentResult
                && &transfer.source_thread_id == child_thread_id
                && &transfer.target_thread_id == &parent_thread_id
                && (transfer.source_point.run_id.as_ref() == Some(&child_run.id)
                    || (task_id == forked_node && transfer.source_point.run_id.is_none()))
        }) {
            return Ok(());
        }

        let now = UtcDateTime::now();
        let ids = IdAllocator;
        let mut result_run = child_run.clone();
        result_run.status = outcome;
        let result = subagent_result_for_run(
            &child_result.messages,
            &child_result.turn_items,
            &result_run,
        );
        let parent_run = task
            .run_id
            .as_ref()
            .and_then(|id| parent.runs.iter().find(|candidate| &candidate.id == id));
        let parent_node = parent
            .nodes
            .iter()
            .find(|candidate| &candidate.id == &task.id);
        let parent_turn_item = parent.turn_items.iter().find(|candidate| {
            matches!(candidate, OrchestrationV2TurnItem::Subagent { subagent_id, .. } if subagent_id == &task.id)
        });
        let updated_task = OrchestrationV2Subagent {
            provider_thread_id: child_run.provider_thread_id.clone(),
            status: terminal_status,
            result: Some(result.text.clone()),
            completed_at: Some(now),
            updated_at: now,
            ..task.clone()
        };
        let message_count = self
            .projection
            .get_message_count(&parent_thread_id)
            .await
            .map_err(|cause| OrchestratorError::projection(parent_thread_id.clone(), cause))?;
        let plan = plan_completion_delivery(&parent, parent_run, task, &updated_task, now, || {
            ids.message(&parent_thread_id, message_count + 1)
        });
        let transfer_id =
            ids.context_transfer(child_thread_id, &parent_thread_id, "subagent_result");
        let child_provider_thread = child_run
            .provider_thread_id
            .as_ref()
            .and_then(|id| child.provider_threads.iter().find(|row| &row.id == id));
        let parent_provider_thread = parent_run
            .and_then(|run| run.provider_thread_id.as_ref())
            .and_then(|id| parent.provider_threads.iter().find(|row| &row.id == id));
        let result_handoff: Option<OrchestrationV2ContextHandoff> =
            match (parent_run, child_provider_thread, parent_provider_thread) {
                (Some(parent_run), Some(child_thread), Some(parent_thread)) => {
                    Some(OrchestrationV2ContextHandoff {
                        id: ids.context_handoff(
                            &parent_thread_id,
                            &child_run.provider_instance_id,
                            &parent_run.provider_instance_id,
                        ),
                        transfer_id: Some(Some(transfer_id.clone())),
                        thread_id: parent_thread_id.clone(),
                        target_run_id: parent_run.id.clone(),
                        from_provider_thread_ids: vec![child_thread.id.clone()],
                        to_provider_thread_id: parent_thread.id.clone(),
                        covered_run_ordinals: OrchestrationV2CoveredRunOrdinals {
                            from: child_run.ordinal,
                            to: child_run.ordinal,
                        },
                        strategy: OrchestrationV2ContextHandoffStrategy::ManualContext,
                        status: OrchestrationV2ContextHandoffStatus::Ready,
                        summary_message_id: result.message_id.clone(),
                        summary_text: result.text.clone(),
                        history: None,
                        delivery: None,
                        detail_in_turn_item: None,
                        created_by_provider_instance_id: Some(
                            child_run.provider_instance_id.clone(),
                        ),
                        created_at: now,
                        updated_at: now,
                    })
                }
                _ => None,
            };
        let mut source_point = context_source_point_for_run(child, child_run);
        source_point.turn_item_id = result.turn_item_id.clone();
        let result_transfer = OrchestrationV2ContextTransfer {
            id: transfer_id,
            r#type: OrchestrationV2ContextTransferType::SubagentResult,
            source_thread_id: child_thread_id.clone(),
            target_thread_id: parent_thread_id.clone(),
            source_point,
            base_point: None,
            source_provider_instance_id: Some(child_run.provider_instance_id.clone()),
            target_provider_instance_id: Some(
                parent_run
                    .map(|run| run.provider_instance_id.clone())
                    .unwrap_or_else(|| parent.thread.provider_instance_id.clone()),
            ),
            target_run_id: parent_run.map(|run| run.id.clone()),
            status: OrchestrationV2ContextTransferStatus::Consumed,
            resolution: result_handoff.as_ref().map(|handoff| {
                OrchestrationV2ContextTransferResolution::PortableContext {
                    context_handoff_id: handoff.id.clone(),
                }
            }),
            created_by: OrchestrationV2Actor::System,
            error: None,
            created_at: now,
            updated_at: now,
            consumed_at: Some(now),
        };

        let command_id = turin_contracts::CommandId::new(format!(
            "command:system:finalize-subagent:{}:{}",
            child_run.id, task_id
        ))
        .expect("system command id");
        let mut write = Decision::new(command_id, ids, None);
        let p = &parent_thread_id;
        macro_rules! emit {
            ($variant:ident, $run:expr, $node:expr, $driver:expr, $instance:expr, $payload:expr) => {{
                let id = write.event_id(p);
                write.emit(OrchestrationV2DomainEvent::$variant {
                    id,
                    thread_id: p.clone(),
                    run_id: $run,
                    node_id: $node,
                    driver: $driver,
                    provider_instance_id: $instance,
                    raw_event_id: None,
                    occurred_at: now,
                    payload: $payload,
                });
            }};
        }
        emit!(
            SubagentUpdated,
            task.run_id.clone(),
            Some(task.id.clone()),
            Some(task.driver.clone()),
            None,
            plan.task.clone()
        );
        if let Some(run) = &plan.parent_run {
            emit!(
                RunUpdated,
                Some(run.id.clone()),
                run.root_node_id.clone(),
                None,
                Some(run.provider_instance_id.clone()),
                run.clone()
            );
        }
        if let Some(message) = &plan.message {
            emit!(
                MessageUpdated,
                message.run_id.clone(),
                message.node_id.clone(),
                None,
                Some(
                    plan.parent_run
                        .as_ref()
                        .map(|run| run.provider_instance_id.clone())
                        .unwrap_or_else(|| parent.thread.provider_instance_id.clone())
                ),
                message.clone()
            );
        }
        if let Some(node) = parent_node {
            emit!(
                NodeUpdated,
                node.run_id.clone(),
                Some(node.id.clone()),
                Some(task.driver.clone()),
                None,
                OrchestrationV2ExecutionNode {
                    status: node_status(terminal_status),
                    provider_thread_id: child_run.provider_thread_id.clone(),
                    completed_at: Some(now),
                    ..node.clone()
                }
            );
        }
        if let Some(item) = parent_turn_item {
            let common = turn_item_common(item);
            let status = serde_json::to_value(terminal_status).expect("status encodes");
            emit!(
                TurnItemUpdated,
                common.run_id.cloned(),
                common.node_id.cloned(),
                Some(task.driver.clone()),
                None,
                patch_turn_item(
                    item,
                    vec![
                        ("status", status),
                        ("result", serde_json::json!(result.text)),
                        ("completedAt", serde_json::json!(now)),
                        ("updatedAt", serde_json::json!(now)),
                    ]
                )
            );
        }
        if let Some(handoff) = result_handoff {
            emit!(
                ContextHandoffUpdated,
                parent_run.map(|run| run.id.clone()),
                None,
                None,
                Some(child_run.provider_instance_id.clone()),
                handoff
            );
        }
        emit!(
            ContextTransferCreated,
            parent_run.map(|run| run.id.clone()),
            None,
            None,
            Some(child_run.provider_instance_id.clone()),
            result_transfer
        );
        self.system_write(&parent_thread_id, write).await?;
        if let (true, Some(run)) = (plan.offer, &plan.parent_run) {
            self.offer_delegated_completion_delivery(&parent_thread_id, &run.id)
                .await?;
        }
        Ok(())
    }

    /// `offerDelegatedCompletionDelivery`: asks the continuation worker to wake
    /// the parent with a cohort's delivery message.
    pub async fn offer_delegated_completion_delivery(
        &self,
        thread_id: &ThreadId,
        parent_run_id: &RunId,
    ) -> Result<(), OrchestratorError> {
        let projection = self
            .get_thread_records(
                thread_id,
                &[
                    RecordField::Runs,
                    RecordField::Messages,
                    RecordField::ProviderTurns,
                    RecordField::ProviderThreads,
                ],
                &RecordFilter {
                    message_roles: Some(vec![
                        turin_contracts::orchestration_v2::OrchestrationV2ConversationMessageRole::User,
                    ]),
                    ..RecordFilter::default()
                },
            )
            .await?;
        let Some(parent_run) = projection.runs.iter().find(|run| &run.id == parent_run_id) else {
            return Ok(());
        };
        let Some(cohort) = parent_run.delegated_completion.as_ref() else {
            return Ok(());
        };
        let Some(delivery) = cohort.delivery.as_ref() else {
            return Ok(());
        };
        let message_exists = projection
            .messages
            .iter()
            .any(|message| message.id == delivery.message_id);
        if cohort.disposition != OrchestrationV2DelegatedCompletionCohortDisposition::Open
            || delivery.task_ids.is_empty()
            || projection.thread.archived_at.is_some()
            || projection.thread.deleted_at.is_some()
            || (message_exists && !is_undelivered_mailbox_steer(&projection, &delivery.message_id))
        {
            return Ok(());
        }
        let Some(provider_thread) = parent_run
            .provider_thread_id
            .as_ref()
            .and_then(|id| projection.provider_threads.iter().find(|row| &row.id == id))
        else {
            return Ok(());
        };
        self.seams
            .continuation_requests
            .offer(ProviderContinuationRequest {
                thread_id: thread_id.clone(),
                provider_thread_id: provider_thread.id.clone(),
                driver: provider_thread.driver.clone(),
                detail: None,
                notification: None,
                delegated_completion: Some(ProviderContinuationDelegatedCompletion {
                    parent_run_id: parent_run_id.clone(),
                    generation: delivery.generation.get() as i64,
                    message_id: delivery.message_id.clone(),
                }),
                delivery: Some(ProviderContinuationDelivery::MessageText),
                message_id: None,
                dispatch_if_current: None,
                clear_if_current: None,
            });
        Ok(())
    }

    /// `offerDelegatedCompletionDeliveries`: every run of the thread that still
    /// has a delivery outstanding.
    pub async fn offer_delegated_completion_deliveries(
        &self,
        thread_id: &ThreadId,
    ) -> Result<(), OrchestratorError> {
        let projection = self
            .get_thread_records(thread_id, &[RecordField::Runs], &RecordFilter::default())
            .await?;
        for run in &projection.runs {
            if run
                .delegated_completion
                .as_ref()
                .is_some_and(|cohort| cohort.delivery.is_some())
            {
                self.offer_delegated_completion_delivery(thread_id, &run.id)
                    .await?;
            }
        }
        Ok(())
    }

    /// `finalizeDelegatedCompletionDelivery`, under the thread's lock: the run
    /// that consumed a cohort's delivery message ended, so the tasks it carried
    /// become delivered (or pending again if it was cancelled), and results that
    /// arrived meanwhile are reserved for one successor delivery.
    pub async fn finalize_delegated_completion_delivery(
        &self,
        thread_id: &ThreadId,
        run_id: &RunId,
    ) -> Result<(), OrchestratorError> {
        self.locks
            .with_lock(thread_id, || {
                self.finalize_delegated_completion_delivery_locked(thread_id, run_id)
            })
            .await
    }

    async fn finalize_delegated_completion_delivery_locked(
        &self,
        thread_id: &ThreadId,
        run_id: &RunId,
    ) -> Result<(), OrchestratorError> {
        use OrchestrationV2DelegatedCompletionTaskDeliveryState as State;
        // Ordinary turns do not own a delegated delivery. Read just their input
        // before loading the cohort state needed to settle an actual delivery.
        let opening = self
            .projection
            .get_run_message(thread_id, run_id.as_str())
            .await
            .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;
        if opening.is_none_or(|message| message.delegated_completion.is_none()) {
            return Ok(());
        }
        let projection = self
            .get_thread_records(
                thread_id,
                &[RecordField::Runs, RecordField::Messages, RecordField::Subagents],
                &RecordFilter {
                    message_roles: Some(vec![
                        turin_contracts::orchestration_v2::OrchestrationV2ConversationMessageRole::User,
                    ]),
                    ..RecordFilter::default()
                },
            )
            .await?;
        let Some(delivery_run) = projection.runs.iter().find(|run| &run.id == run_id) else {
            return Ok(());
        };
        let Some(ownership) = projection
            .messages
            .iter()
            .find(|message| message.id == delivery_run.user_message_id)
            .and_then(|message| message.delegated_completion.as_ref())
        else {
            return Ok(());
        };
        let Some(parent_run) = projection
            .runs
            .iter()
            .find(|run| run.id == ownership.parent_run_id)
        else {
            return Ok(());
        };
        let Some(cohort) = parent_run.delegated_completion.as_ref() else {
            return Ok(());
        };
        let Some(delivery) = cohort.delivery.as_ref() else {
            return Ok(());
        };
        if delivery.message_id != delivery_run.user_message_id
            || delivery.generation != ownership.generation
        {
            return Ok(());
        }

        let now = UtcDateTime::now();
        let ids = IdAllocator;
        let mut next_states: HashMap<NodeId, State> = HashMap::new();
        for task in &projection.subagents {
            if task.origin == OrchestrationV2SubagentOrigin::AppOwned
                && task.run_id.as_ref() == Some(&parent_run.id)
                && delivery.task_ids.contains(&task.id)
                && task.completion_delivery.as_ref().map(|d| d.state) == Some(State::Claimed)
            {
                next_states.insert(
                    task.id.clone(),
                    if delivery_run.status == OrchestrationV2RunStatus::Cancelled {
                        State::Pending
                    } else {
                        State::Delivered
                    },
                );
            }
        }
        let pending_task_ids: Vec<NodeId> = projection
            .subagents
            .iter()
            .filter(|task| {
                task.origin == OrchestrationV2SubagentOrigin::AppOwned
                    && task.run_id.as_ref() == Some(&parent_run.id)
                    && is_terminal_task_status(task.status)
                    && (task.completion_delivery.as_ref().map(|d| d.state) == Some(State::Pending)
                        || next_states.get(&task.id) == Some(&State::Pending))
            })
            .map(|task| task.id.clone())
            .collect();
        // Results that arrived while this delivery was outstanding go out
        // together in one successor. Each child becomes pending once, so a
        // cohort's successors are bounded by its children.
        let can_reserve_follow_up = cohort.disposition
            == OrchestrationV2DelegatedCompletionCohortDisposition::Open
            && projection.thread.archived_at.is_none()
            && projection.thread.deleted_at.is_none()
            && !pending_task_ids.is_empty();
        let next_delivery = if can_reserve_follow_up {
            let count = self
                .projection
                .get_message_count(thread_id)
                .await
                .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;
            Some(OrchestrationV2DelegatedCompletionDelivery {
                generation: cohort.next_generation,
                message_id: ids.message(thread_id, count + 1),
                task_ids: pending_task_ids.clone(),
            })
        } else {
            None
        };
        if next_delivery.is_some() {
            for id in &pending_task_ids {
                next_states.insert(id.clone(), State::Claimed);
            }
        }
        let updated_parent_run = OrchestrationV2Run {
            delegated_completion: Some(OrchestrationV2DelegatedCompletionCohort {
                disposition: cohort.disposition,
                next_generation: if next_delivery.is_none() {
                    cohort.next_generation
                } else {
                    turin_contracts::PositiveInt::new(cohort.next_generation.get() + 1)
                        .expect("positive")
                },
                delivery: next_delivery.clone(),
            }),
            ..parent_run.clone()
        };
        let command_id =
            turin_contracts::CommandId::new(format!("command:system:finalize-delivery:{run_id}"))
                .expect("system command id");
        let mut write = Decision::new(command_id, ids, None);
        for task in &projection.subagents {
            let Some(state) = next_states.get(&task.id) else {
                continue;
            };
            let id = write.event_id(thread_id);
            write.emit(OrchestrationV2DomainEvent::SubagentUpdated {
                id,
                thread_id: thread_id.clone(),
                run_id: task.run_id.clone(),
                node_id: Some(task.id.clone()),
                driver: Some(task.driver.clone()),
                provider_instance_id: Some(task.provider_instance_id.clone()),
                raw_event_id: None,
                occurred_at: now,
                payload: OrchestrationV2Subagent {
                    completion_delivery: task_delivery(*state),
                    updated_at: now,
                    ..task.clone()
                },
            });
        }
        let id = write.event_id(thread_id);
        write.emit(OrchestrationV2DomainEvent::RunUpdated {
            id,
            thread_id: thread_id.clone(),
            run_id: Some(updated_parent_run.id.clone()),
            node_id: updated_parent_run.root_node_id.clone(),
            driver: None,
            provider_instance_id: Some(updated_parent_run.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: updated_parent_run.clone(),
        });
        self.system_write(thread_id, write).await?;
        if next_delivery.is_some() {
            self.offer_delegated_completion_delivery(thread_id, &parent_run.id)
                .await?;
        }
        Ok(())
    }

    /// `recoverDelegatedTasks`: settles child results and completion deliveries
    /// whose runs ended without the terminal-run listener (before this boot, or
    /// in runtime reconciliation, which it skips). Startup runs this after
    /// reconciliation and before the effect worker.
    pub async fn recover_app_owned_subagents(&self) {
        match self
            .projection
            .get_recovery_thread_ids(ProjectionRecoveryKind::SubagentResults)
            .await
        {
            Err(cause) => {
                tracing::warn!(%cause, "Failed to inspect app-owned subagents during recovery");
            }
            Ok(thread_ids) => {
                for child_thread_id in thread_ids {
                    if let Err(cause) = self
                        .finalize_app_owned_subagent(&child_thread_id, &FinalizeOptions::default())
                        .await
                    {
                        tracing::warn!(%child_thread_id, %cause, "Failed to recover terminal app-owned subagent");
                    }
                }
            }
        }
        match self
            .projection
            .get_recovery_thread_ids(ProjectionRecoveryKind::DelegatedCompletions)
            .await
        {
            Err(cause) => {
                tracing::warn!(%cause, "Failed to inspect delegated completion delivery during recovery");
            }
            Ok(thread_ids) => {
                for thread_id in thread_ids {
                    if let Err(cause) = self.recover_delegated_completions(&thread_id).await {
                        tracing::warn!(%thread_id, %cause, "Failed to recover delegated completion delivery");
                    }
                }
            }
        }
    }

    async fn recover_delegated_completions(
        &self,
        thread_id: &ThreadId,
    ) -> Result<(), OrchestratorError> {
        let projection = self
            .get_thread_records(thread_id, &[RecordField::Runs], &RecordFilter::default())
            .await?;
        // Only the run that consumed a cohort's current delivery message can
        // settle it. A long-lived parent keeps every past delivery run, and
        // finalizing each of them would reload the thread once per historical
        // delivery.
        let open_message_ids: HashSet<_> = projection
            .runs
            .iter()
            .filter_map(|run| {
                run.delegated_completion
                    .as_ref()
                    .and_then(|cohort| cohort.delivery.as_ref())
                    .map(|delivery| delivery.message_id.clone())
            })
            .collect();
        let terminal_delivery_runs: Vec<RunId> = projection
            .runs
            .iter()
            .filter(|run| {
                task_terminal_status(run.status).is_some()
                    && open_message_ids.contains(&run.user_message_id)
            })
            .map(|run| run.id.clone())
            .collect();
        for run_id in &terminal_delivery_runs {
            self.finalize_delegated_completion_delivery(thread_id, run_id)
                .await?;
        }
        let refreshed = if terminal_delivery_runs.is_empty() {
            projection
        } else {
            self.get_thread_records(thread_id, &[RecordField::Runs], &RecordFilter::default())
                .await?
        };
        for run in &refreshed.runs {
            if run
                .delegated_completion
                .as_ref()
                .is_some_and(|cohort| cohort.delivery.is_some())
            {
                self.offer_delegated_completion_delivery(thread_id, &run.id)
                    .await?;
            }
        }
        Ok(())
    }

    /// `recoverDelegatedTask`: a delegated child whose restart continuation
    /// declined or failed settles its parent.
    pub async fn settle_delegated_task_after_continuation(
        &self,
        thread_id: &ThreadId,
        source_run_id: &RunId,
    ) {
        if let Err(cause) = self
            .finalize_app_owned_subagent(
                thread_id,
                &FinalizeOptions {
                    settled_continuation_of: Some(source_run_id.clone()),
                },
            )
            .await
        {
            tracing::warn!(child_thread_id = %thread_id, %cause, "Failed to recover delegated task after restart");
        }
    }
}

/// [`DelegatedTaskRecovery`] over the orchestrator, bound after construction:
/// the orchestrator takes its `RestartRecovery` (which holds this) at
/// construction. Build the restart service over a `LateDelegatedTaskRecovery`,
/// build the orchestrator, then [`bind`](Self::bind) it. Until then both calls
/// do nothing.
#[derive(Default)]
pub struct LateDelegatedTaskRecovery {
    inner: OnceLock<Arc<Orchestrator>>,
}

impl LateDelegatedTaskRecovery {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Binds the orchestrator. Returns false if already bound.
    pub fn bind(&self, orchestrator: Arc<Orchestrator>) -> bool {
        self.inner.set(orchestrator).is_ok()
    }
}

#[async_trait]
impl DelegatedTaskRecovery for LateDelegatedTaskRecovery {
    async fn recover_delegated_tasks(&self) {
        if let Some(orchestrator) = self.inner.get() {
            orchestrator.recover_app_owned_subagents().await;
        }
    }

    async fn recover_delegated_task(&self, thread_id: &ThreadId, source_run_id: &RunId) {
        if let Some(orchestrator) = self.inner.get() {
            orchestrator
                .settle_delegated_task_after_continuation(thread_id, source_run_id)
                .await;
        }
    }
}
