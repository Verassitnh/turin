//! Port of upstream `Orchestrator.ts` Stop and interrupt:
//! `dispatchRunInterrupt` (9192), `dispatchThreadStop` (9588),
//! `dispatchSubagentStop` (9109), `dispatchBackgroundWorkSettle` (8960) and
//! their helpers `settleInterruptedRun` (8601), `settleBackgroundWork` (8425),
//! `stopReachedRun` (8789), `markStoppedRun` (8805) and `holdStoppedThread`
//! (8863).
//!
//! Each effect upstream runs after the command commits is queued here as a
//! `provider-turn.interrupt` effect; the turn services (T13e) run it.

use std::collections::HashMap;
use std::convert::Infallible;

use indexmap::IndexMap;
use turin_contracts::{
    EventId, NodeId, ProviderThreadId, ProviderTurnId, RunId, ThreadId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2BackgroundWorkSettleOutcomeStatus, OrchestrationV2Command as C,
        OrchestrationV2ConversationMessageRole, OrchestrationV2DomainEvent,
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2InternalCommand as I,
        OrchestrationV2NativeRefStrength, OrchestrationV2ProviderThreadStatus,
        OrchestrationV2ProviderTurn, OrchestrationV2ProviderTurnStatus, OrchestrationV2Run,
        OrchestrationV2RunAttemptStatus, OrchestrationV2RunStatus,
        OrchestrationV2RuntimeRequestStatus, OrchestrationV2ServerCommand,
        OrchestrationV2SubagentOrigin, OrchestrationV2SubagentStatus,
        OrchestrationV2ThreadProjection, OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
    },
};
use turin_provider_core::{
    id_allocator::{IdAllocator, derive as ids},
    subagent_projection::{
        EndOrphanedNativeSubagentInput, EndRunlessRootTurnsInput, EndedWorkStatus,
        end_orphaned_native_subagent, end_runless_root_turns,
    },
    turn_item::turn_item_common,
};

use super::Orchestrator;
use super::cohort::{CohortEnd, dispose_delegated_completion_cohort, with_pending_events};
use super::dispatch::{Decision, command_id, dispatch_failure};
use super::message_dispatch::{
    WORKSPACE_PREPARATION_INPUT, next_turn_item_ordinal, run_scoped_event,
};
use super::queued_run::cancel_queued_run;
use super::thread_archive::without_watches;
use crate::core::projection_reads::{ProviderControlReads, ProviderControlTarget};
use crate::core::run_execution_service::{
    OpenRunOwnedSubagentProjection, RunOwnedSubagentTerminalStatus,
    cascade_terminalize_run_owned_subagents, make_interrupt_result_turn_item,
};
use crate::pipeline::command_policy::{CapabilityCheck, ensure_interrupt};
use crate::pipeline::effect_outbox::{EffectRequest, EffectType, InterruptSubagent, PendingEffect};
use crate::pipeline::event_sink::CancelUnsettled;
use crate::pipeline::notification_mailbox::is_buffered_provider_notification;
use crate::pipeline::orchestrator_port::{
    OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
};
use crate::projection::shared::pending_background_work::{
    DerivePendingBackgroundWorkInput, PendingBackgroundWorkItem,
    PendingBackgroundWorkProviderThread, PendingBackgroundWorkRun, derive_pending_background_work,
    pending_background_turn_items,
};
use turin_shared::orchestration_v2_thread_error::latest_executed_run;

/// The turn items Stop reads thread-wide, like the Waiting strip: a settled
/// thread's background work can belong to an earlier run than the one Stop
/// targets.
fn background_item_filter() -> RecordFilter {
    RecordFilter {
        turn_item_types: Some(
            [
                "command_execution",
                "dynamic_tool",
                "subagent",
                "assistant_message",
                "reasoning",
            ]
            .map(str::to_owned)
            .to_vec(),
        ),
        turn_item_statuses: Some(vec![
            OrchestrationV2TurnItemStatus::Pending,
            OrchestrationV2TurnItemStatus::Running,
            OrchestrationV2TurnItemStatus::Waiting,
        ]),
        ..RecordFilter::default()
    }
}

/// Decides `run.interrupt`, `thread.stop`, `subagent.stop` and
/// `thread.background-work.settle`. `Ok(false)` leaves the command to another group.
pub(super) async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    use OrchestrationV2ServerCommand::{Client, Internal};
    match command {
        Client(C::RunInterrupt {
            thread_id,
            run_id,
            reason,
            hold_queue,
            ..
        }) => {
            let cancel = run_interrupt(
                orchestrator,
                command,
                decision,
                thread_id,
                run_id,
                reason.as_deref(),
                *hold_queue == Some(true),
            )
            .await?;
            decision.cancel_unsettled_effects = cancel;
            // Stop also stops every delegated task under the thread once it commits.
            if *hold_queue == Some(true) {
                decision.effects.push(PendingEffect {
                    id: format!("effect:{}:delegated-tasks.stop", command_id(command)),
                    command_id: command_id(command).clone(),
                    thread_id: thread_id.clone(),
                    request: EffectRequest::DelegatedTasksStop {
                        reason: reason.clone(),
                    },
                    available_at: None,
                });
            }
        }
        Client(C::SubagentStop {
            thread_id,
            run_id,
            subagent_id,
            ..
        }) => {
            subagent_stop(
                orchestrator,
                command,
                decision,
                thread_id,
                run_id,
                subagent_id,
            )
            .await?
        }
        Internal(I::ThreadStop {
            thread_id, reason, ..
        }) => {
            let cancel = thread_stop(
                orchestrator,
                command,
                decision,
                thread_id,
                reason.as_deref(),
            )
            .await?;
            decision.cancel_unsettled_effects = cancel;
        }
        Internal(I::ThreadBackgroundWorkSettle {
            thread_id,
            provider_thread_id,
            provider_turn_id,
            subagent,
            ..
        }) => {
            background_work_settle(
                orchestrator,
                command,
                decision,
                thread_id,
                provider_thread_id,
                provider_turn_id,
                subagent.as_ref(),
            )
            .await?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// An event id minted for `command` on `thread_id`, as `allocateEventId`.
fn event_id_source(decision: &Decision, thread_id: &ThreadId) -> impl Fn() -> EventId + use<> {
    let command_id = decision.command_id().clone();
    let thread_id = thread_id.clone();
    move || IdAllocator.event(Some(&thread_id), Some(&command_id), None)
}

fn immediate<T>(value: T) -> std::future::Ready<Result<T, Infallible>> {
    std::future::ready(Ok(value))
}

/// The pending-work list `derivePendingBackgroundWork` builds for `latest_run`.
fn has_pending_background_work(
    projection: &OrchestrationV2ThreadProjection,
    latest_run: Option<&OrchestrationV2Run>,
) -> bool {
    let runs: Vec<_> = projection.runs.iter().map(run_view).collect();
    let provider_threads: Vec<_> = projection
        .provider_threads
        .iter()
        .map(|row| PendingBackgroundWorkProviderThread {
            id: row.id.as_str(),
            pending_background_tasks: &row.pending_background_tasks,
        })
        .collect();
    let turn_items: Vec<_> = projection
        .turn_items
        .iter()
        .map(PendingBackgroundWorkItem::from_turn_item)
        .collect();
    !derive_pending_background_work(&DerivePendingBackgroundWorkInput {
        latest_run: latest_run.map(run_view),
        provider_threads,
        turn_items,
        active_provider_thread_id: projection
            .thread
            .active_provider_thread_id
            .as_ref()
            .map(|id| id.as_str()),
        has_active_run: None,
        runs: Some(runs),
        pull_requests: None,
    })
    .is_empty()
}

fn run_view(run: &OrchestrationV2Run) -> PendingBackgroundWorkRun<'_> {
    PendingBackgroundWorkRun {
        id: run.id.as_str(),
        ordinal: run.ordinal.get(),
        status: run.status,
    }
}

/// `projection.runs.findLast((run) => run.status !== "queued")`.
fn last_unqueued_run(runs: &[OrchestrationV2Run]) -> Option<&OrchestrationV2Run> {
    runs.iter()
        .rev()
        .find(|run| run.status != OrchestrationV2RunStatus::Queued)
}

/// `hasLiveRun`: a run is still producing agent output.
fn has_live_run(runs: &[&OrchestrationV2Run]) -> bool {
    runs.iter().any(|run| {
        matches!(
            run.status,
            OrchestrationV2RunStatus::Preparing
                | OrchestrationV2RunStatus::Starting
                | OrchestrationV2RunStatus::Running
        )
    })
}

/// `stopReachedRun`: whether Stop reached a run, as its interrupt request
/// records. Its agent can call tools until the provider stops it, and a slow
/// call can land after that, but nothing it starts may outlive the Stop. A
/// restart continuation of that run must not start either.
pub(super) async fn stop_reached_run(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    thread_id: &ThreadId,
    run_id: &RunId,
) -> Result<bool, OrchestratorError> {
    let records = orchestrator
        .get_thread_records(
            thread_id,
            &[RecordField::TurnItems],
            &RecordFilter {
                turn_item_types: Some(vec!["run_interrupt_request".to_owned()]),
                turn_item_run_ids: Some(vec![Some(run_id.clone())]),
                ..RecordFilter::default()
            },
        )
        .await
        .map_err(|cause| dispatch_failure(command, cause.to_string()))?;
    Ok(!records.turn_items.is_empty())
}

/// The next turn-item ordinal past the store's and this command's own writes.
async fn next_ordinal_with_pending(
    orchestrator: &Orchestrator,
    projection: &OrchestrationV2ThreadProjection,
    decision: &Decision,
) -> Result<u64, OrchestratorError> {
    let stored = next_turn_item_ordinal(orchestrator, projection).await?;
    let pending = decision
        .events
        .iter()
        .filter_map(|event| match event {
            OrchestrationV2DomainEvent::TurnItemUpdated { payload, .. } => {
                Some(turn_item_common(payload).ordinal + 1)
            }
            _ => None,
        })
        .max()
        .unwrap_or(0);
    Ok(stored.max(pending))
}

fn interrupt_request_item(
    run: &OrchestrationV2Run,
    root_node_id: &NodeId,
    provider_thread_id: &ProviderThreadId,
    provider_turn_id: Option<ProviderTurnId>,
    ordinal: u64,
    now: UtcDateTime,
    reason: Option<&str>,
) -> OrchestrationV2TurnItem {
    OrchestrationV2TurnItem::RunInterruptRequest {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: ids::run_signal_turn_item(&run.id, "interrupt-request"),
        thread_id: run.thread_id.clone(),
        run_id: Some(run.id.clone()),
        node_id: Some(root_node_id.clone()),
        provider_thread_id: Some(provider_thread_id.clone()),
        provider_turn_id,
        native_item_ref: None,
        parent_item_id: None,
        ordinal,
        status: OrchestrationV2TurnItemStatus::Completed,
        title: Some("Interrupt requested".to_owned()),
        started_at: Some(now),
        completed_at: Some(now),
        updated_at: now,
        message: reason.unwrap_or("Interrupt requested").to_owned(),
    }
}

/// `markStoppedRun`: records Stop for a run without replacing a marker already
/// written by its interrupt.
async fn mark_stopped_run(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    projection: &OrchestrationV2ThreadProjection,
    run: &OrchestrationV2Run,
    reason: Option<&str>,
    now: UtcDateTime,
) -> Result<(), OrchestratorError> {
    let (Some(root_node_id), Some(provider_thread_id)) =
        (run.root_node_id.as_ref(), run.provider_thread_id.as_ref())
    else {
        return Ok(());
    };
    let written = decision.events.iter().any(|event| {
        matches!(
            event,
            OrchestrationV2DomainEvent::TurnItemUpdated {
                payload: OrchestrationV2TurnItem::RunInterruptRequest { run_id, .. },
                ..
            } if run_id.as_ref() == Some(&run.id)
        )
    });
    if written || stop_reached_run(orchestrator, command, &projection.thread.id, &run.id).await? {
        return Ok(());
    }
    let ordinal = next_ordinal_with_pending(orchestrator, projection, decision).await?;
    let item = interrupt_request_item(
        run,
        root_node_id,
        provider_thread_id,
        None,
        ordinal,
        now,
        reason,
    );
    run_scoped_event!(
        decision,
        TurnItemUpdated,
        &projection.thread.id,
        &run.id,
        root_node_id,
        &run.provider_instance_id,
        item
    );
    Ok(())
}

/// What Stop holds besides the run it interrupts: durable queued messages wait
/// for the user, buffered provider wakes are cancelled, and the thread's pull
/// request watches end. Every wake its delegated tasks still owe is dropped,
/// since Stop stops those tasks too. `cohort_run_ids` are dropped even when
/// they owe nothing, like a plain interrupt's.
async fn hold_stopped_thread(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    projection: &OrchestrationV2ThreadProjection,
    cohort_run_ids: Vec<RunId>,
    reason: Option<&str>,
    now: UtcDateTime,
) -> Result<(), OrchestratorError> {
    let thread = &projection.thread;
    let queued_message_ids: Vec<_> = projection
        .runs
        .iter()
        .filter(|run| run.status == OrchestrationV2RunStatus::Queued)
        .map(|run| run.user_message_id.clone())
        .collect();
    let buffered: Vec<_> = if queued_message_ids.is_empty() {
        Vec::new()
    } else {
        orchestrator
            .get_thread_records(
                &thread.id,
                &[RecordField::Messages],
                &RecordFilter {
                    message_ids: Some(queued_message_ids),
                    ..RecordFilter::default()
                },
            )
            .await?
            .messages
            .into_iter()
            .filter(is_buffered_provider_notification)
            .map(|message| message.id)
            .collect()
    };
    for run in &projection.runs {
        // A resumed queue can have a lower ordinal than a pending continuation
        // source. Stop must reach those sources even when interrupting the
        // resumed run succeeds.
        if run.status == OrchestrationV2RunStatus::Cancelled
            && orchestrator
                .recovery
                .awaits_restart_continuation(run)
                .await
                .map_err(|cause| dispatch_failure(command, cause.to_string()))?
        {
            mark_stopped_run(
                orchestrator,
                command,
                decision,
                projection,
                run,
                reason,
                now,
            )
            .await?;
        }
        if run.status != OrchestrationV2RunStatus::Queued {
            continue;
        }
        // Stop discards the provider's wake buffer, so its queued placeholders
        // cannot resume.
        if buffered.contains(&run.user_message_id) {
            cancel_queued_run(orchestrator, command, decision, &thread.id, &run.id).await?;
            continue;
        }
        if run.queue_held == Some(true) {
            continue;
        }
        let id = decision.event_id(&thread.id);
        decision.emit(OrchestrationV2DomainEvent::RunUpdated {
            id,
            thread_id: thread.id.clone(),
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
    let pull_requests = thread.pull_requests.clone().unwrap_or_default();
    if pull_requests.iter().any(|link| link.watch.is_some()) {
        let id = decision.event_id(&thread.id);
        decision.emit(OrchestrationV2DomainEvent::ThreadPullRequestSynced {
            id,
            thread_id: thread.id.clone(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id: Some(thread.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: turin_contracts::orchestration_v2::OrchestrationV2AppThread {
                pull_requests: without_watches(Some(pull_requests)),
                ..thread.clone()
            },
        });
    }
    let mut cohorts = cohort_run_ids;
    let mut add = |id: RunId| {
        if !cohorts.contains(&id) {
            cohorts.push(id);
        }
    };
    for run in &projection.runs {
        if run
            .delegated_completion
            .as_ref()
            .is_some_and(|cohort| cohort.delivery.is_some())
        {
            add(run.id.clone());
        }
    }
    for task in &projection.subagents {
        let delivered = matches!(
            task.completion_delivery.as_ref().map(|delivery| delivery.state),
            Some(
                turin_contracts::orchestration_v2::OrchestrationV2DelegatedCompletionTaskDeliveryState::Acknowledged
                    | turin_contracts::orchestration_v2::OrchestrationV2DelegatedCompletionTaskDeliveryState::Delivered
                    | turin_contracts::orchestration_v2::OrchestrationV2DelegatedCompletionTaskDeliveryState::Disposed
            )
        );
        if task.origin == OrchestrationV2SubagentOrigin::AppOwned
            && !delivered
            && let Some(run_id) = &task.run_id
        {
            add(run_id.clone());
        }
    }
    // Each disposal reads the events the ones before it wrote: the stored
    // rows are read once, and the pending events are laid over them again.
    let stored = orchestrator.read_command_projection(&thread.id).await?;
    for parent in cohorts {
        let pending = with_pending_events(stored.clone(), &decision.events, &thread.id);
        dispose_delegated_completion_cohort(
            decision,
            &pending,
            &parent,
            CohortEnd::Stopped,
            now,
            true,
        );
    }
    Ok(())
}

/// `settleBackgroundWork`: ends the work a stopped provider thread left behind.
async fn settle_background_work(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    projection: &OrchestrationV2ThreadProjection,
    stopped_provider_thread_id: Option<&ProviderThreadId>,
    through_run_ordinal: u64,
    now: UtcDateTime,
) -> Result<(), OrchestratorError> {
    let thread_id = command_thread(command);
    let run_ordinals: HashMap<&RunId, u64> = projection
        .runs
        .iter()
        .map(|run| (&run.id, run.ordinal.get()))
        .collect();
    let mut liveness: HashMap<ProviderThreadId, bool> = HashMap::new();
    let mut child_thread_ids: Vec<ThreadId> = Vec::new();
    let item_views: Vec<_> = projection
        .turn_items
        .iter()
        .map(PendingBackgroundWorkItem::from_turn_item)
        .collect();
    let run_views: Vec<_> = projection.runs.iter().map(run_view).collect();
    let by_id: HashMap<&str, &OrchestrationV2TurnItem> = projection
        .turn_items
        .iter()
        .map(|item| (turn_item_common(item).id.as_str(), item))
        .collect();
    let pending: Vec<&OrchestrationV2TurnItem> =
        pending_background_turn_items(&item_views, Some(&run_views))
            .into_iter()
            .filter_map(|view| by_id.get(view.id).copied())
            .collect();
    for item in pending {
        let common = turn_item_common(item);
        // An item without a run counts as the stopped run's.
        let item_run_ordinal = common.run_id.and_then(|id| run_ordinals.get(id).copied());
        if item_run_ordinal.is_some_and(|ordinal| ordinal > through_run_ordinal) {
            continue;
        }
        if let Some(provider_thread_id) = common.provider_thread_id
            && Some(provider_thread_id) != stopped_provider_thread_id
            && has_live_session(orchestrator, projection, provider_thread_id, &mut liveness).await
        {
            continue;
        }
        let run = common
            .run_id
            .and_then(|id| projection.runs.iter().find(|run| &run.id == id));
        let interrupted = interrupted_item(item, now);
        let id = decision.event_id(&thread_id);
        decision.emit(OrchestrationV2DomainEvent::TurnItemUpdated {
            id,
            thread_id: thread_id.clone(),
            run_id: common.run_id.cloned(),
            node_id: common.node_id.cloned(),
            driver: None,
            provider_instance_id: run.map(|run| run.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: interrupted,
        });
        if !matches!(item, OrchestrationV2TurnItem::Subagent { .. }) {
            continue;
        }
        // A native subagent died with the process too: end its record, its own
        // thread's runless root turn, and the subagents it started there, which
        // a provider can record on that thread rather than this one.
        let mut dying: Vec<(OrchestrationV2ThreadProjection, DyingSubagent)> =
            vec![(projection.clone(), DyingSubagent::from_item(item))];
        let mut cursor = 0;
        while cursor < dying.len() {
            let (scope, subagent) = dying[cursor].clone();
            cursor += 1;
            let ended = end_orphaned_native_subagent(
                EndOrphanedNativeSubagentInput {
                    nodes: &scope.nodes,
                    subagents: &scope.subagents,
                    turn_items: &scope.turn_items,
                    subagent_id: &subagent.subagent_id,
                    status: EndedWorkStatus::Interrupted,
                    completed_at: None,
                    result: None,
                    now,
                    emitted: &decision.events,
                },
                {
                    let next = event_id_source(decision, &thread_id);
                    move || immediate(next())
                },
            )
            .await
            .map_err(|never: Infallible| match never {})?;
            decision.events.extend(ended);
            let Some(child_thread_id) = subagent.child_thread_id.clone() else {
                continue;
            };
            if subagent.origin != OrchestrationV2SubagentOrigin::ProviderNative
                || child_thread_ids.contains(&child_thread_id)
            {
                continue;
            }
            child_thread_ids.push(child_thread_id.clone());
            let Some(child) =
                read_child_runless_work(orchestrator, command, &child_thread_id).await?
            else {
                continue;
            };
            let child_ended = end_runless_root_turns(
                EndRunlessRootTurnsInput {
                    thread_id: &child_thread_id,
                    provider_instance_id: &subagent.provider_instance_id,
                    nodes: &child.nodes,
                    turn_items: &child.turn_items,
                    status: EndedWorkStatus::Interrupted,
                    completed_at: None,
                    now,
                    emitted: &decision.events,
                },
                {
                    let next = event_id_source(decision, &thread_id);
                    move || immediate(next())
                },
            )
            .await
            .map_err(|never: Infallible| match never {})?;
            decision.events.extend(child_ended);
            for nested in &child.turn_items {
                if matches!(nested, OrchestrationV2TurnItem::Subagent { .. }) {
                    dying.push((child.clone(), DyingSubagent::from_item(nested)));
                }
            }
            // Workflow members have records and child roots without subagent turn items.
            for nested in &child.subagents {
                if nested.origin == OrchestrationV2SubagentOrigin::ProviderNative
                    && nested.run_id.is_none()
                {
                    dying.push((
                        child.clone(),
                        DyingSubagent {
                            subagent_id: nested.id.clone(),
                            origin: nested.origin,
                            child_thread_id: nested.child_thread_id.clone(),
                            provider_instance_id: nested.provider_instance_id.clone(),
                        },
                    ));
                }
            }
        }
    }
    for provider_thread in &projection.provider_threads {
        // A live process owns its roster and reports clearing it.
        if provider_thread.pending_background_tasks.is_empty()
            || has_live_session(orchestrator, projection, &provider_thread.id, &mut liveness).await
        {
            continue;
        }
        let status = if provider_thread.status == OrchestrationV2ProviderThreadStatus::Active
            && provider_thread
                .last_run_ordinal
                .is_some_and(|ordinal| ordinal.get() <= through_run_ordinal)
        {
            OrchestrationV2ProviderThreadStatus::Idle
        } else {
            provider_thread.status
        };
        let id = decision.event_id(&thread_id);
        decision.emit(OrchestrationV2DomainEvent::ProviderThreadUpdated {
            id,
            thread_id: thread_id.clone(),
            run_id: None,
            node_id: None,
            driver: Some(provider_thread.driver.clone()),
            provider_instance_id: Some(provider_thread.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: turin_contracts::orchestration_v2::OrchestrationV2ProviderThread {
                status,
                pending_background_tasks: Vec::new(),
                updated_at: now,
                ..provider_thread.clone()
            },
        });
    }
    Ok(())
}

/// The fields of a dying native subagent that the cascade reads.
#[derive(Clone)]
struct DyingSubagent {
    subagent_id: NodeId,
    origin: OrchestrationV2SubagentOrigin,
    child_thread_id: Option<ThreadId>,
    provider_instance_id: turin_contracts::provider_instance::ProviderInstanceId,
}

impl DyingSubagent {
    fn from_item(item: &OrchestrationV2TurnItem) -> Self {
        let OrchestrationV2TurnItem::Subagent {
            subagent_id,
            origin,
            child_thread_id,
            provider_instance_id,
            ..
        } = item
        else {
            unreachable!("DyingSubagent is built only from subagent items");
        };
        Self {
            subagent_id: subagent_id.clone(),
            origin: *origin,
            child_thread_id: child_thread_id.clone(),
            provider_instance_id: provider_instance_id.clone(),
        }
    }
}

/// A turn item as `{ ...item, status: "interrupted", completedAt, updatedAt }`.
fn interrupted_item(item: &OrchestrationV2TurnItem, now: UtcDateTime) -> OrchestrationV2TurnItem {
    let mut wire = serde_json::to_value(item).expect("turn item encodes");
    if let Some(fields) = wire.as_object_mut() {
        fields.insert("status".to_owned(), serde_json::json!("interrupted"));
        fields.insert("completedAt".to_owned(), serde_json::json!(now));
        fields.insert("updatedAt".to_owned(), serde_json::json!(now));
    }
    serde_json::from_value(wire).expect("interrupted turn item decodes")
}

/// A child thread's runless root turns and the subagents under it; `None`
/// when the thread is gone.
async fn read_child_runless_work(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    child_thread_id: &ThreadId,
) -> Result<Option<OrchestrationV2ThreadProjection>, OrchestratorError> {
    match orchestrator
        .get_thread_records(
            child_thread_id,
            &[
                RecordField::Nodes,
                RecordField::Subagents,
                RecordField::TurnItems,
            ],
            &RecordFilter {
                // Runless items are the subagent's; any run there is the user's own.
                turn_item_run_ids: Some(vec![None]),
                turn_item_statuses: Some(vec![
                    OrchestrationV2TurnItemStatus::Pending,
                    OrchestrationV2TurnItemStatus::Running,
                    OrchestrationV2TurnItemStatus::Waiting,
                ]),
                ..RecordFilter::default()
            },
        )
        .await
    {
        Ok(child) => Ok(Some(child)),
        Err(OrchestratorError::Projection { cause, .. })
            if matches!(
                cause.downcast_ref::<crate::projection::error::ProjectionStoreError>(),
                Some(crate::projection::error::ProjectionStoreError::ThreadNotFound { .. })
            ) =>
        {
            Ok(None)
        }
        Err(cause) => Err(dispatch_failure(command, cause.to_string())),
    }
}

/// Whether a provider thread's session is live. A failed lookup counts as gone.
async fn has_live_session(
    orchestrator: &Orchestrator,
    projection: &OrchestrationV2ThreadProjection,
    provider_thread_id: &ProviderThreadId,
    liveness: &mut HashMap<ProviderThreadId, bool>,
) -> bool {
    if let Some(known) = liveness.get(provider_thread_id) {
        return *known;
    }
    let session_id = projection
        .provider_threads
        .iter()
        .find(|row| &row.id == provider_thread_id)
        .and_then(|row| row.provider_session_id.clone());
    let live = match session_id {
        Some(session_id) => matches!(
            orchestrator.seams.sessions.get_session(&session_id).await,
            Ok(Some(_))
        ),
        None => false,
    };
    liveness.insert(provider_thread_id.clone(), live);
    live
}

fn command_thread(command: &OrchestrationV2ServerCommand) -> ThreadId {
    super::dispatch::command_thread_id(command).clone()
}

/// `settleInterruptedRun`: a successful Stop may have no terminal event to
/// ingest, because the session or native turn already died, or its final write
/// failed when storage filled. The run ends interrupted locally.
fn settle_interrupted_run(
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    projection: &OrchestrationV2ThreadProjection,
    provider_turn: &OrchestrationV2ProviderTurn,
    now: UtcDateTime,
) -> Result<(), OrchestratorError> {
    let thread_id = command_thread(command);
    let Some(attempt) = projection
        .attempts
        .iter()
        .find(|candidate| Some(&candidate.id) == provider_turn.run_attempt_id.as_ref())
    else {
        return Ok(());
    };
    let Some(run) = projection
        .runs
        .iter()
        .find(|candidate| candidate.id == attempt.run_id)
    else {
        return Ok(());
    };
    let Some(root_node) = run
        .root_node_id
        .as_ref()
        .and_then(|id| projection.nodes.iter().find(|node| &node.id == id))
    else {
        return Ok(());
    };
    if run.active_attempt_id.as_ref() != Some(&attempt.id)
        || run.status != OrchestrationV2RunStatus::Running
        || attempt.status != OrchestrationV2RunAttemptStatus::Running
    {
        return Ok(());
    }
    let instance = &run.provider_instance_id;
    if provider_turn.status == OrchestrationV2ProviderTurnStatus::Running {
        run_scoped_event!(
            decision,
            ProviderTurnUpdated,
            &thread_id,
            &run.id,
            &root_node.id,
            instance,
            OrchestrationV2ProviderTurn {
                status: OrchestrationV2ProviderTurnStatus::Interrupted,
                completed_at: Some(now),
                ..provider_turn.clone()
            }
        );
    }
    run_scoped_event!(
        decision,
        RunAttemptUpdated,
        &thread_id,
        &run.id,
        &root_node.id,
        instance,
        turin_contracts::orchestration_v2::OrchestrationV2RunAttempt {
            status: OrchestrationV2RunAttemptStatus::Interrupted,
            completed_at: Some(now),
            ..attempt.clone()
        }
    );
    let provider_thread = projection
        .provider_threads
        .iter()
        .find(|candidate| candidate.id == provider_turn.provider_thread_id);
    if let Some(row) = provider_thread
        && row.status == OrchestrationV2ProviderThreadStatus::Active
        && row.last_run_ordinal == Some(run.ordinal)
    {
        run_scoped_event!(
            decision,
            ProviderThreadUpdated,
            &thread_id,
            &run.id,
            &root_node.id,
            instance,
            turin_contracts::orchestration_v2::OrchestrationV2ProviderThread {
                status: OrchestrationV2ProviderThreadStatus::Idle,
                updated_at: now,
                ..row.clone()
            }
        );
    }
    for message in &projection.messages {
        if message.run_id.as_ref() != Some(&run.id) || !message.streaming {
            continue;
        }
        let node = message
            .node_id
            .clone()
            .unwrap_or_else(|| root_node.id.clone());
        run_scoped_event!(
            decision,
            MessageUpdated,
            &thread_id,
            &run.id,
            &node,
            instance,
            turin_contracts::orchestration_v2::OrchestrationV2ConversationMessage {
                streaming: false,
                updated_at: now,
                ..message.clone()
            }
        );
    }
    let delegated_node_ids: std::collections::HashSet<_> = projection
        .subagents
        .iter()
        .filter(|task| task.origin == OrchestrationV2SubagentOrigin::AppOwned)
        .map(|task| task.id.clone())
        .collect();
    let open = OpenRunOwnedSubagentProjection {
        subagents: projection
            .subagents
            .iter()
            .filter(|task| {
                task.run_id.as_ref() == Some(&run.id)
                    && task.origin != OrchestrationV2SubagentOrigin::AppOwned
            })
            .map(|task| (task.id.clone(), task.clone()))
            .collect(),
        nodes: projection
            .nodes
            .iter()
            .filter(|node| {
                node.run_id.as_ref() == Some(&run.id) && !delegated_node_ids.contains(&node.id)
            })
            .map(|node| (node.id.clone(), node.clone()))
            .collect(),
        turn_items: IndexMap::new(),
        child_turn_items: projection
            .turn_items
            .iter()
            .filter(|item| {
                matches!(
                    item,
                    OrchestrationV2TurnItem::AssistantMessage { .. }
                        | OrchestrationV2TurnItem::Reasoning { .. }
                ) && turn_item_common(item).run_id == Some(&run.id)
            })
            .map(|item| (turn_item_common(item).id.clone(), item.clone()))
            .collect(),
        linked_child_thread_ids: [run.thread_id.clone()].into_iter().collect(),
    };
    let allocate = event_id_source(decision, &thread_id);
    let cascaded = cascade_terminalize_run_owned_subagents(
        run,
        &open,
        RunOwnedSubagentTerminalStatus::Interrupted,
        &now,
        &mut || allocate(),
    )
    .map_err(|cause| dispatch_failure(command, cause.to_string()))?;
    decision.events.extend(cascaded);
    for request in &projection.runtime_requests {
        if request.status != OrchestrationV2RuntimeRequestStatus::Pending
            || !projection
                .nodes
                .iter()
                .any(|node| node.id == request.node_id && node.run_id.as_ref() == Some(&run.id))
        {
            continue;
        }
        run_scoped_event!(
            decision,
            RuntimeRequestUpdated,
            &thread_id,
            &run.id,
            &request.node_id,
            instance,
            turin_contracts::orchestration_v2::OrchestrationV2RuntimeRequest {
                status: OrchestrationV2RuntimeRequestStatus::Cancelled,
                resolved_at: Some(now),
                response_capability: turin_contracts::orchestration_v2::OrchestrationV2ResponseCapability::NotResumable {
                    reason: "The run was interrupted.".to_owned(),
                },
                ..request.clone()
            }
        );
    }
    if let Some(row) = provider_thread {
        let item = make_interrupt_result_turn_item(run, root_node, row, &now);
        run_scoped_event!(
            decision,
            TurnItemUpdated,
            &thread_id,
            &run.id,
            &root_node.id,
            instance,
            item
        );
    }
    run_scoped_event!(
        decision,
        RunUpdated,
        &thread_id,
        &run.id,
        &root_node.id,
        instance,
        OrchestrationV2Run {
            delegated_completion: None,
            status: OrchestrationV2RunStatus::Interrupted,
            completed_at: Some(now),
            ..run.clone()
        }
    );
    if let Some(scope_id) = root_node.checkpoint_scope_id.clone() {
        decision.effects.push(PendingEffect {
            id: format!("effect:checkpoint.capture:{}", run.id),
            command_id: turin_contracts::CommandId::new(format!(
                "command:effect:checkpoint.capture:{}",
                run.id
            ))
            .expect("effect command id"),
            thread_id: thread_id.clone(),
            request: EffectRequest::CheckpointCapture {
                run_id: run.id.clone(),
                scope_id,
            },
            available_at: None,
        });
    }
    Ok(())
}

/// How a run interrupt was asked for.
#[derive(Clone, Copy)]
struct StopContext<'a> {
    hold_queue: bool,
    reason: Option<&'a str>,
    now: UtcDateTime,
}

/// `stopRemainingWork` of `dispatchRunInterrupt`: Stop holds the rest of the
/// thread too; a plain interrupt only drops the wakes owed by this run's
/// delegated tasks, or on a wake run, by the cohort that woke it.
async fn stop_remaining_work(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    projection: &OrchestrationV2ThreadProjection,
    cohort_run_id: RunId,
    stop: StopContext<'_>,
) -> Result<(), OrchestratorError> {
    let StopContext {
        hold_queue,
        reason,
        now,
    } = stop;
    if hold_queue {
        return hold_stopped_thread(
            orchestrator,
            command,
            decision,
            projection,
            vec![cohort_run_id],
            reason,
            now,
        )
        .await;
    }
    let thread_id = &projection.thread.id;
    let full = orchestrator.read_command_projection(thread_id).await?;
    let pending = with_pending_events(full, &decision.events, thread_id);
    dispose_delegated_completion_cohort(
        decision,
        &pending,
        &cohort_run_id,
        CohortEnd::Stopped,
        now,
        true,
    );
    Ok(())
}

/// `dispatchRunInterrupt`. The returned unsettled-effect cancel is set when the
/// run never reached its provider turn.
async fn run_interrupt(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    thread_id: &ThreadId,
    run_id: &RunId,
    reason: Option<&str>,
    hold_queue: bool,
) -> Result<Option<CancelUnsettled>, OrchestratorError> {
    let projection = orchestrator
        .get_thread_records(
            thread_id,
            &[
                RecordField::Runs,
                RecordField::Nodes,
                RecordField::ProviderThreads,
                RecordField::ProviderTurns,
                RecordField::Messages,
                RecordField::Attempts,
                RecordField::TurnItems,
                RecordField::Subagents,
                RecordField::RuntimeRequests,
            ],
            &background_item_filter(),
        )
        .await?;
    let run = projection
        .runs
        .iter()
        .find(|candidate| &candidate.id == run_id);
    let root_node = run
        .and_then(|run| run.root_node_id.as_ref())
        .and_then(|id| {
            projection
                .nodes
                .iter()
                .find(|candidate| &candidate.id == id)
        });
    let provider_thread = run
        .and_then(|run| run.provider_thread_id.as_ref())
        .and_then(|id| {
            projection
                .provider_threads
                .iter()
                .find(|candidate| &candidate.id == id)
        });
    let has_background_work = run.is_some_and(|run| {
        last_unqueued_run(&projection.runs).is_some_and(|last| last.id == run.id)
            && has_pending_background_work(&projection, Some(run))
    });
    // A failed start has no provider turn. Background work still belongs to
    // the provider thread, so Stop reaches its latest accepted turn.
    let provider_turn = run.and_then(|run| {
        projection
            .provider_turns
            .iter()
            .rev()
            .find(|candidate| {
                candidate.run_attempt_id == run.active_attempt_id
                    && (run.status == OrchestrationV2RunStatus::Running
                        || candidate.status == OrchestrationV2ProviderTurnStatus::Running
                        || has_background_work)
            })
            .or_else(|| {
                has_background_work
                    .then(|| {
                        projection.provider_turns.iter().rev().find(|candidate| {
                            Some(&candidate.provider_thread_id) == run.provider_thread_id.as_ref()
                        })
                    })
                    .flatten()
            })
    });
    let (Some(run), Some(root_node), Some(provider_thread)) = (run, root_node, provider_thread)
    else {
        return Err(dispatch_failure(
            command,
            format!("Run {run_id} is not interruptible."),
        ));
    };
    let now = decision.now();
    let completion_cohort_run_id = projection
        .messages
        .iter()
        .find(|candidate| candidate.id == run.user_message_id)
        .and_then(|message| message.delegated_completion.as_ref())
        .map_or_else(
            || run.id.clone(),
            |completion| completion.parent_run_id.clone(),
        );
    let request_ordinal = next_ordinal_with_pending(orchestrator, &projection, decision).await?;
    let request_item = interrupt_request_item(
        run,
        &root_node.id,
        &provider_thread.id,
        provider_turn.map(|turn| turn.id.clone()),
        request_ordinal,
        now,
        reason,
    );

    // Background work can outlive a provider switch, such as a Codex dev server
    // left running when the thread moved to Claude. Stop also reaches each
    // other live provider thread that owns pending work; that interrupt's
    // settle follow-up ends what its provider leaves behind. Work on a dead
    // session is settled with this run's below.
    let mut other_provider_interrupts: Vec<PendingEffect> = Vec::new();
    if has_background_work || hold_queue {
        let run_ordinals: HashMap<&RunId, i64> = projection
            .runs
            .iter()
            .map(|candidate| (&candidate.id, candidate.ordinal.get() as i64))
            .collect();
        let item_views: Vec<_> = projection
            .turn_items
            .iter()
            .map(PendingBackgroundWorkItem::from_turn_item)
            .collect();
        let run_views: Vec<_> = projection.runs.iter().map(run_view).collect();
        let by_id: HashMap<&str, &OrchestrationV2TurnItem> = projection
            .turn_items
            .iter()
            .map(|item| (turn_item_common(item).id.as_str(), item))
            .collect();
        // Each provider thread is interrupted at its latest pending work: that
        // turn's run bounds the settle follow-up, which must cover all of the
        // thread's work. The target comes from the item's provider turn, since
        // a native subagent item names its own provider thread but its
        // parent's turn; interrupting the parent's turn reaches the subagent.
        let mut latest: Vec<(ProviderThreadId, &OrchestrationV2ProviderTurn, i64)> = Vec::new();
        for view in pending_background_turn_items(&item_views, Some(&run_views)) {
            let Some(item) = by_id.get(view.id).copied() else {
                continue;
            };
            let common = turn_item_common(item);
            let Some(turn) = projection
                .provider_turns
                .iter()
                .find(|candidate| Some(&candidate.id) == common.provider_turn_id)
            else {
                continue;
            };
            if turn.provider_thread_id == provider_thread.id {
                continue;
            }
            let run_ordinal = common
                .run_id
                .map_or(-1, |id| run_ordinals.get(id).copied().unwrap_or(-1));
            match latest
                .iter_mut()
                .find(|(thread, _, _)| thread == &turn.provider_thread_id)
            {
                Some(entry) if run_ordinal > entry.2 => {
                    *entry = (turn.provider_thread_id.clone(), turn, run_ordinal)
                }
                Some(_) => {}
                None => latest.push((turn.provider_thread_id.clone(), turn, run_ordinal)),
            }
        }
        for (_, turn, _) in latest {
            let Some(owner) = projection
                .provider_threads
                .iter()
                .find(|candidate| candidate.id == turn.provider_thread_id)
            else {
                continue;
            };
            let Some(owner_session_id) = owner.provider_session_id.clone() else {
                continue;
            };
            if !matches!(
                orchestrator
                    .seams
                    .sessions
                    .get_session(&owner_session_id)
                    .await,
                Ok(Some(_))
            ) {
                continue;
            }
            other_provider_interrupts.push(PendingEffect {
                id: format!(
                    "effect:{}:provider-turn.interrupt:{}",
                    command_id(command),
                    turn.id
                ),
                command_id: command_id(command).clone(),
                thread_id: thread_id.clone(),
                request: EffectRequest::ProviderTurnInterrupt {
                    provider_session_id: owner_session_id,
                    provider_thread_id: owner.id.clone(),
                    provider_turn_id: turn.id.clone(),
                    subagent: None,
                },
                available_at: None,
            });
        }
    }

    let instance = &run.provider_instance_id;
    if provider_turn.is_none()
        && matches!(
            run.status,
            OrchestrationV2RunStatus::Preparing
                | OrchestrationV2RunStatus::Starting
                | OrchestrationV2RunStatus::Running
        )
    {
        let Some(attempt) = projection
            .attempts
            .iter()
            .find(|candidate| Some(&candidate.id) == run.active_attempt_id.as_ref())
        else {
            return Err(dispatch_failure(
                command,
                format!("Run {run_id} has no active attempt to interrupt."),
            ));
        };
        let result_item = OrchestrationV2TurnItem::RunInterruptResult {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id: ids::run_signal_turn_item(&run.id, "interrupt-result"),
            thread_id: thread_id.clone(),
            run_id: Some(run.id.clone()),
            node_id: Some(root_node.id.clone()),
            provider_thread_id: Some(provider_thread.id.clone()),
            provider_turn_id: None,
            native_item_ref: None,
            parent_item_id: Some(turn_item_common(&request_item).id.clone()),
            ordinal: request_ordinal + 1,
            status: OrchestrationV2TurnItemStatus::Interrupted,
            title: Some("Interrupted".to_owned()),
            started_at: Some(now),
            completed_at: Some(now),
            updated_at: now,
            message: "Run interrupted before provider start".to_owned(),
        };
        run_scoped_event!(
            decision,
            TurnItemUpdated,
            thread_id,
            &run.id,
            &root_node.id,
            instance,
            request_item
        );
        run_scoped_event!(
            decision,
            TurnItemUpdated,
            thread_id,
            &run.id,
            &root_node.id,
            instance,
            result_item
        );
        let preparation = projection.turn_items.iter().find(|candidate| {
            matches!(
                candidate,
                OrchestrationV2TurnItem::CommandExecution { input, status, run_id, .. }
                    if run_id.as_ref() == Some(&run.id)
                        && input == WORKSPACE_PREPARATION_INPUT
                        && status == &OrchestrationV2TurnItemStatus::Running
            )
        });
        if let Some(preparation) = preparation {
            let mut wire = serde_json::to_value(preparation).expect("turn item encodes");
            if let Some(fields) = wire.as_object_mut() {
                fields.insert("status".to_owned(), serde_json::json!("interrupted"));
                fields.insert(
                    "title".to_owned(),
                    serde_json::json!("Workspace preparation interrupted"),
                );
                fields.insert(
                    "output".to_owned(),
                    serde_json::json!(reason.unwrap_or("Interrupted before provider start")),
                );
                fields.insert("completedAt".to_owned(), serde_json::json!(now));
                fields.insert("updatedAt".to_owned(), serde_json::json!(now));
            }
            let item: OrchestrationV2TurnItem =
                serde_json::from_value(wire).expect("interrupted preparation decodes");
            run_scoped_event!(
                decision,
                TurnItemUpdated,
                thread_id,
                &run.id,
                &root_node.id,
                instance,
                item
            );
        }
        run_scoped_event!(
            decision,
            RunAttemptUpdated,
            thread_id,
            &run.id,
            &root_node.id,
            instance,
            turin_contracts::orchestration_v2::OrchestrationV2RunAttempt {
                status: OrchestrationV2RunAttemptStatus::Interrupted,
                completed_at: Some(now),
                ..attempt.clone()
            }
        );
        run_scoped_event!(
            decision,
            NodeUpdated,
            thread_id,
            &run.id,
            &root_node.id,
            instance,
            turin_contracts::orchestration_v2::OrchestrationV2ExecutionNode {
                status: OrchestrationV2ExecutionNodeStatus::Interrupted,
                completed_at: Some(now),
                ..root_node.clone()
            }
        );
        run_scoped_event!(
            decision,
            RunUpdated,
            thread_id,
            &run.id,
            &root_node.id,
            instance,
            OrchestrationV2Run {
                status: OrchestrationV2RunStatus::Interrupted,
                completed_at: Some(now),
                ..run.clone()
            }
        );
        stop_remaining_work(
            orchestrator,
            command,
            decision,
            &projection,
            completion_cohort_run_id,
            StopContext {
                hold_queue,
                reason,
                now,
            },
        )
        .await?;
        decision.effects.extend(other_provider_interrupts);
        return Ok(Some(CancelUnsettled {
            effect_types: vec![
                EffectType::ProviderTurnStart,
                EffectType::ProviderTurnRestart,
            ],
            reason: format!(
                "Run {} was interrupted before its provider turn started.",
                run.id
            ),
        }));
    }

    let Some(provider_turn) = provider_turn else {
        return Err(dispatch_failure(
            command,
            format!("Run {run_id} is not interruptible."),
        ));
    };
    // A dead session cannot report a terminal event for either a running turn
    // or a settled turn's background work. Stop ends both locally.
    let session = match provider_thread.provider_session_id.as_ref() {
        None => None,
        Some(session_id) => orchestrator
            .seams
            .sessions
            .get_session(session_id)
            .await
            .map_err(|cause| OrchestratorError::ProviderAdapter {
                command_id: command_id(command).clone(),
                provider_instance_id: run.provider_instance_id.clone(),
                cause,
            })?,
    };
    let Some(session) = session else {
        run_scoped_event!(
            decision,
            TurnItemUpdated,
            thread_id,
            &run.id,
            &root_node.id,
            instance,
            request_item
        );
        stop_remaining_work(
            orchestrator,
            command,
            decision,
            &projection,
            completion_cohort_run_id,
            StopContext {
                hold_queue,
                reason,
                now,
            },
        )
        .await?;
        settle_interrupted_run(command, decision, &projection, provider_turn, now)?;
        settle_background_work(
            orchestrator,
            command,
            decision,
            &projection,
            Some(&provider_thread.id),
            run.ordinal.get(),
            now,
        )
        .await?;
        decision.effects.extend(other_provider_interrupts);
        return Ok(None);
    };
    let provider_session_id = session.provider_session_id().clone();
    ensure_interrupt(&CapabilityCheck {
        command_id: command_id(command),
        thread_id,
        provider_instance_id: &run.provider_instance_id,
        capabilities: &session.provider_session().capabilities,
    })
    .map_err(|cause| dispatch_failure(command, cause.to_string()))?;

    run_scoped_event!(
        decision,
        TurnItemUpdated,
        thread_id,
        &run.id,
        &root_node.id,
        instance,
        request_item
    );
    stop_remaining_work(
        orchestrator,
        command,
        decision,
        &projection,
        completion_cohort_run_id,
        StopContext {
            hold_queue,
            reason,
            now,
        },
    )
    .await?;
    decision.effects.push(PendingEffect {
        id: format!(
            "effect:{}:provider-turn.interrupt:{}",
            command_id(command),
            provider_turn.id
        ),
        command_id: command_id(command).clone(),
        thread_id: thread_id.clone(),
        request: EffectRequest::ProviderTurnInterrupt {
            provider_session_id,
            provider_thread_id: provider_thread.id.clone(),
            provider_turn_id: provider_turn.id.clone(),
            subagent: None,
        },
        available_at: None,
    });
    decision.effects.extend(other_provider_interrupts);
    Ok(None)
}

/// `dispatchThreadStop`: Stop for a thread whatever it is doing, the way the
/// Stop button stops the run it shows. The running turn (or one whose
/// background work is still pending) is interrupted, and the rest of the
/// thread is held. A turn that cannot be interrupted still leaves the thread
/// held. Delegated tasks under the thread are stopped by the sender.
async fn thread_stop(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    thread_id: &ThreadId,
    reason: Option<&str>,
) -> Result<Option<CancelUnsettled>, OrchestratorError> {
    let projection = orchestrator
        .get_thread_records(
            thread_id,
            &[
                RecordField::Runs,
                RecordField::ProviderThreads,
                RecordField::TurnItems,
                RecordField::Subagents,
            ],
            &RecordFilter {
                turn_item_types: Some(
                    ["command_execution", "dynamic_tool", "subagent"]
                        .map(str::to_owned)
                        .to_vec(),
                ),
                ..background_item_filter()
            },
        )
        .await?;
    if projection.thread.deleted_at.is_some() {
        return Ok(None);
    }
    // Queued messages never started and cannot own background work.
    let latest_run = last_unqueued_run(&projection.runs);
    let target = projection
        .runs
        .iter()
        .rev()
        .find(|run| {
            matches!(
                run.status,
                OrchestrationV2RunStatus::Preparing
                    | OrchestrationV2RunStatus::Starting
                    | OrchestrationV2RunStatus::Running
            )
        })
        .or_else(|| {
            has_pending_background_work(&projection, latest_run)
                .then_some(latest_run)
                .flatten()
        });
    if let Some(target) = target {
        let mut interrupt = decision.fork();
        // A failed interrupt may have written part of its events, so they are
        // kept apart.
        match run_interrupt(
            orchestrator,
            command,
            &mut interrupt,
            thread_id,
            &target.id,
            reason,
            true,
        )
        .await
        {
            Ok(cancel) => {
                decision.absorb(interrupt);
                return Ok(cancel);
            }
            Err(cause) => {
                tracing::warn!(
                    thread_id = %thread_id,
                    run_id = %target.id,
                    %cause,
                    "thread.stop could not interrupt the running turn"
                );
            }
        }
    }
    let now = decision.now();
    // Record that Stop reached the turn it could not interrupt and the
    // thread's last executed run, so a late tool call from their agent starts
    // nothing.
    let executed = latest_executed_run(&projection.runs);
    let mut reached: Vec<&OrchestrationV2Run> = Vec::new();
    if let Some(target) = target {
        reached.push(target);
    }
    if let Some(executed) = executed
        && !has_live_run(&[executed])
        && !reached.iter().any(|run| run.id == executed.id)
    {
        reached.push(executed);
    }
    for run in reached {
        mark_stopped_run(
            orchestrator,
            command,
            decision,
            &projection,
            run,
            reason,
            now,
        )
        .await?;
    }
    hold_stopped_thread(
        orchestrator,
        command,
        decision,
        &projection,
        Vec::new(),
        reason,
        now,
    )
    .await?;
    Ok(None)
}

/// `dispatchSubagentStop`: asks the provider to stop one native subagent.
async fn subagent_stop(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    thread_id: &ThreadId,
    run_id: &RunId,
    subagent_id: &NodeId,
) -> Result<(), OrchestratorError> {
    let projection = orchestrator
        .get_thread_records(
            thread_id,
            &[
                RecordField::Runs,
                RecordField::Subagents,
                RecordField::ProviderThreads,
                RecordField::ProviderTurns,
            ],
            &RecordFilter::default(),
        )
        .await?;
    let run = projection
        .runs
        .iter()
        .find(|candidate| &candidate.id == run_id);
    let subagent = projection
        .subagents
        .iter()
        .find(|candidate| &candidate.id == subagent_id)
        .filter(|subagent| {
            subagent.run_id.as_ref() == Some(run_id)
                && subagent.origin == OrchestrationV2SubagentOrigin::ProviderNative
                && subagent.driver.as_str() == "claudeAgent"
        });
    let native_task_id = subagent
        .and_then(|subagent| subagent.native_task_ref.as_ref())
        .filter(|reference| reference.strength == OrchestrationV2NativeRefStrength::Strong)
        .and_then(|reference| reference.native_id.as_ref());
    let (Some(subagent), Some(native_task_id)) = (subagent, native_task_id) else {
        return Err(dispatch_failure(
            command,
            "The requested native subagent cannot be stopped on this run.",
        ));
    };
    if !matches!(
        subagent.status,
        OrchestrationV2SubagentStatus::Pending
            | OrchestrationV2SubagentStatus::Running
            | OrchestrationV2SubagentStatus::Waiting
    ) {
        return Ok(());
    }
    let provider_thread = run
        .and_then(|run| run.provider_thread_id.as_ref())
        .and_then(|id| {
            projection
                .provider_threads
                .iter()
                .find(|candidate| &candidate.id == id)
        });
    let provider_turn = provider_thread.and_then(|row| {
        projection
            .provider_turns
            .iter()
            .rev()
            .find(|candidate| candidate.provider_thread_id == row.id)
    });
    let (Some(provider_thread), Some(provider_turn)) = (provider_thread, provider_turn) else {
        return Err(dispatch_failure(
            command,
            "The requested native subagent has no owning provider session.",
        ));
    };
    let Some(provider_session_id) = provider_thread.provider_session_id.clone() else {
        return Err(dispatch_failure(
            command,
            "The requested native subagent has no owning provider session.",
        ));
    };
    if provider_thread.driver != subagent.driver
        || provider_thread.provider_instance_id != subagent.provider_instance_id
    {
        return Err(dispatch_failure(
            command,
            "The requested native subagent has no owning provider session.",
        ));
    }
    let now = decision.now();
    // The effect can repair stale projections only after the provider confirms an outcome.
    let id = decision.event_id(thread_id);
    decision.emit(OrchestrationV2DomainEvent::SubagentUpdated {
        id,
        thread_id: thread_id.clone(),
        run_id: Some(run_id.clone()),
        node_id: Some(subagent.id.clone()),
        driver: None,
        provider_instance_id: Some(subagent.provider_instance_id.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: turin_contracts::orchestration_v2::OrchestrationV2Subagent {
            updated_at: now,
            ..subagent.clone()
        },
    });
    decision.effects.push(PendingEffect {
        id: format!(
            "effect:{}:subagent.stop:{}",
            command_id(command),
            subagent.id
        ),
        command_id: command_id(command).clone(),
        thread_id: thread_id.clone(),
        request: EffectRequest::ProviderTurnInterrupt {
            provider_session_id,
            provider_thread_id: provider_thread.id.clone(),
            provider_turn_id: provider_turn.id.clone(),
            subagent: Some(InterruptSubagent {
                id: subagent.id.clone(),
                native_task_id: native_task_id.as_str().to_owned(),
            }),
        },
        available_at: None,
    });
    Ok(())
}

/// `dispatchBackgroundWorkSettle`: the follow-up to a Stop. Ends what the
/// stopped provider left behind once its terminal outcome is known.
async fn background_work_settle(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
    thread_id: &ThreadId,
    provider_thread_id: &ProviderThreadId,
    provider_turn_id: &ProviderTurnId,
    settled_subagent: Option<
        &turin_contracts::orchestration_v2::OrchestrationV2BackgroundWorkSettleSubagent,
    >,
) -> Result<(), OrchestratorError> {
    let projection = orchestrator
        .get_thread_records(
            thread_id,
            &[
                RecordField::Runs,
                RecordField::Attempts,
                RecordField::Nodes,
                RecordField::Subagents,
                RecordField::RuntimeRequests,
                RecordField::TurnItems,
                RecordField::ProviderThreads,
            ],
            &background_item_filter(),
        )
        .await?;
    let stopped = orchestrator
        .projection
        .provider_control_context(
            thread_id,
            &ProviderControlTarget {
                provider_thread_id: provider_thread_id.clone(),
                provider_turn_id: provider_turn_id.clone(),
                attempt_id: None,
                message_id: None,
            },
        )
        .await
        .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;
    if let Some(settled) = settled_subagent {
        let outcome = &settled.outcome;
        let Some(subagent) = projection
            .subagents
            .iter()
            .find(|task| task.id == settled.id)
            .filter(|task| {
                task.origin == OrchestrationV2SubagentOrigin::ProviderNative
                    && matches!(
                        task.status,
                        OrchestrationV2SubagentStatus::Pending
                            | OrchestrationV2SubagentStatus::Running
                            | OrchestrationV2SubagentStatus::Waiting
                    )
                    && !task.started_at.is_some_and(|started| {
                        started.as_datetime() > settled.stopped_at.as_datetime()
                    })
            })
        else {
            return Ok(());
        };
        let now = decision.now();
        let status = match outcome.status {
            OrchestrationV2BackgroundWorkSettleOutcomeStatus::Completed => {
                EndedWorkStatus::Completed
            }
            OrchestrationV2BackgroundWorkSettleOutcomeStatus::Failed => EndedWorkStatus::Failed,
            OrchestrationV2BackgroundWorkSettleOutcomeStatus::Cancelled => {
                EndedWorkStatus::Cancelled
            }
        };
        let ended = end_orphaned_native_subagent(
            EndOrphanedNativeSubagentInput {
                nodes: &projection.nodes,
                subagents: &projection.subagents,
                turn_items: &projection.turn_items,
                subagent_id: &subagent.id,
                status,
                completed_at: Some(outcome.completed_at),
                // An absent result keeps the recorded one.
                result: outcome.result.clone().map(Some),
                now,
                emitted: &decision.events,
            },
            {
                let next = event_id_source(decision, thread_id);
                move || immediate(next())
            },
        )
        .await
        .map_err(|never: Infallible| match never {})?;
        decision.events.extend(ended);
        if let Some(child_thread_id) = subagent.child_thread_id.clone()
            && let Some(child) =
                read_child_runless_work(orchestrator, command, &child_thread_id).await?
        {
            let child_ended = end_runless_root_turns(
                EndRunlessRootTurnsInput {
                    thread_id: &child_thread_id,
                    provider_instance_id: &subagent.provider_instance_id,
                    nodes: &child.nodes,
                    turn_items: &child.turn_items,
                    status,
                    completed_at: Some(outcome.completed_at),
                    now,
                    emitted: &decision.events,
                },
                {
                    let next = event_id_source(decision, thread_id);
                    move || immediate(next())
                },
            )
            .await
            .map_err(|never: Infallible| match never {})?;
            decision.events.extend(child_ended);
        }
        return Ok(());
    }
    let stopped_run = stopped
        .provider_turn
        .as_ref()
        .and_then(|turn| turn.run_attempt_id.as_ref())
        .and_then(|attempt_id| {
            projection
                .attempts
                .iter()
                .find(|attempt| &attempt.id == attempt_id)
        })
        .and_then(|attempt| projection.runs.iter().find(|run| run.id == attempt.run_id));
    let Some(stopped_run) = stopped_run else {
        return Ok(());
    };
    if matches!(
        stopped_run.status,
        OrchestrationV2RunStatus::Preparing
            | OrchestrationV2RunStatus::Starting
            | OrchestrationV2RunStatus::Running
    ) && stopped_run.active_attempt_id.as_ref()
        != stopped
            .provider_turn
            .as_ref()
            .and_then(|turn| turn.run_attempt_id.as_ref())
    {
        return Ok(());
    }
    let now = decision.now();
    if let Some(turn) = &stopped.provider_turn
        && stopped_run.status == OrchestrationV2RunStatus::Running
    {
        let output = orchestrator
            .get_thread_records(
                thread_id,
                &[RecordField::Messages],
                &RecordFilter {
                    message_run_ids: Some(vec![stopped_run.id.clone()]),
                    message_roles: Some(vec![OrchestrationV2ConversationMessageRole::Assistant]),
                    ..RecordFilter::default()
                },
            )
            .await?;
        let with_messages = OrchestrationV2ThreadProjection {
            messages: output.messages,
            ..projection.clone()
        };
        settle_interrupted_run(command, decision, &with_messages, turn, now)?;
    }
    // A new turn may have started since Stop; its work is not this Stop's.
    if projection.runs.iter().any(|run| {
        run.id != stopped_run.id
            && matches!(
                run.status,
                OrchestrationV2RunStatus::Preparing
                    | OrchestrationV2RunStatus::Starting
                    | OrchestrationV2RunStatus::Running
            )
    }) {
        return Ok(());
    }
    settle_background_work(
        orchestrator,
        command,
        decision,
        &projection,
        Some(provider_thread_id),
        stopped_run.ordinal.get(),
        now,
    )
    .await
}
