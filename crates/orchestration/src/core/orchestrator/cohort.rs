//! Delegated-completion cohort disposal and queued-run cancellation, the
//! helpers `Orchestrator.ts` shares between archive, stop and the queue
//! commands: `disposeDelegatedCompletionCohort` (2129),
//! `disposeAllDelegatedCompletionCohorts` (2212), `completionDeliveryRun`
//! (1048) and `emitQueuedRunCancellation` (1124).
//!
//! Also `with_pending_events`, the stand-in for upstream's
//! `getProjectionWithPendingEvents` (see PORTING.md): later steps of one
//! command read what the steps before them wrote.

use std::collections::HashSet;

use turin_contracts::{
    RunId, ThreadId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2DelegatedCompletionCohort,
        OrchestrationV2DelegatedCompletionCohortDisposition,
        OrchestrationV2DelegatedCompletionDelivery, OrchestrationV2DelegatedCompletionTaskDelivery,
        OrchestrationV2DelegatedCompletionTaskDeliveryState, OrchestrationV2DomainEvent,
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2Run, OrchestrationV2RunAttemptStatus,
        OrchestrationV2RunStatus, OrchestrationV2SubagentOrigin, OrchestrationV2ThreadProjection,
    },
};
use turin_provider_core::turn_item::turn_item_common;

use super::dispatch::Decision;
use crate::projection::event::event_thread_id;

/// Upserts `item` by `same`, as an applied `*.updated` event would.
fn upsert<T>(items: &mut Vec<T>, item: T, same: impl Fn(&T) -> bool) {
    match items.iter_mut().find(|existing| same(existing)) {
        Some(existing) => *existing = item,
        None => items.push(item),
    }
}

/// `getProjectionWithPendingEvents`: `projection` with the events this
/// command has decided so far laid over it. Upstream folds them through the
/// full projection reducer. This overlay upserts the payload of each
/// `*.updated` or `*.created` event that the stop, archive and queue paths
/// write, and takes a `thread.*` payload as the thread row. It does not
/// recompute derived fields (counters, shell rows), which no caller reads.
pub(super) fn with_pending_events(
    mut projection: OrchestrationV2ThreadProjection,
    events: &[OrchestrationV2DomainEvent],
    thread_id: &ThreadId,
) -> OrchestrationV2ThreadProjection {
    use OrchestrationV2DomainEvent as E;
    for event in events
        .iter()
        .filter(|event| event_thread_id(event) == thread_id)
    {
        match event {
            E::RunCreated { payload, .. } | E::RunUpdated { payload, .. } => {
                upsert(&mut projection.runs, payload.clone(), |run| {
                    run.id == payload.id
                });
            }
            E::RunAttemptCreated { payload, .. } | E::RunAttemptUpdated { payload, .. } => {
                upsert(&mut projection.attempts, payload.clone(), |attempt| {
                    attempt.id == payload.id
                });
            }
            E::NodeUpdated { payload, .. } => {
                upsert(&mut projection.nodes, payload.clone(), |node| {
                    node.id == payload.id
                });
            }
            E::SubagentUpdated { payload, .. } => {
                upsert(&mut projection.subagents, payload.clone(), |task| {
                    task.id == payload.id
                });
            }
            E::ProviderThreadUpdated { payload, .. } => {
                upsert(&mut projection.provider_threads, payload.clone(), |row| {
                    row.id == payload.id
                });
            }
            E::ProviderTurnUpdated { payload, .. } => {
                upsert(&mut projection.provider_turns, payload.clone(), |turn| {
                    turn.id == payload.id
                });
            }
            E::RuntimeRequestUpdated { payload, .. } => {
                upsert(
                    &mut projection.runtime_requests,
                    payload.clone(),
                    |request| request.id == payload.id,
                );
            }
            E::MessageUpdated { payload, .. } => {
                upsert(&mut projection.messages, payload.clone(), |message| {
                    message.id == payload.id
                });
            }
            E::TurnItemUpdated { payload, .. } => {
                let id = turn_item_common(payload).id.clone();
                upsert(&mut projection.turn_items, payload.clone(), |item| {
                    turn_item_common(item).id == &id
                });
            }
            E::ThreadCreated { payload, .. }
            | E::ThreadArchived { payload, .. }
            | E::ThreadUnarchived { payload, .. }
            | E::ThreadDeleted { payload, .. }
            | E::ThreadSettled { payload, .. }
            | E::ThreadUnsettled { payload, .. }
            | E::ThreadSnoozed { payload, .. }
            | E::ThreadUnsnoozed { payload, .. }
            | E::ThreadPinned { payload, .. }
            | E::ThreadAutoSettleSet { payload, .. }
            | E::ThreadUnpinned { payload, .. }
            | E::ThreadPinReordered { payload, .. }
            | E::ThreadActiveReordered { payload, .. }
            | E::ThreadVisited { payload, .. }
            | E::ThreadMarkedUnread { payload, .. }
            | E::ThreadMetadataUpdated { payload, .. }
            | E::ThreadPullRequestSynced { payload, .. }
            | E::ThreadRuntimeModeUpdated { payload, .. }
            | E::ThreadInteractionModeUpdated { payload, .. }
            | E::ThreadModelSelectionUpdated { payload, .. }
            | E::ThreadProviderSwitched { payload, .. } => {
                projection.thread = payload.clone();
            }
            _ => {}
        }
    }
    projection
}

/// `completionDeliveryRun`: the run that carries a cohort's delivery message.
pub(super) fn completion_delivery_run<'a>(
    projection: &'a OrchestrationV2ThreadProjection,
    delivery: Option<&OrchestrationV2DelegatedCompletionDelivery>,
) -> Option<&'a OrchestrationV2Run> {
    let delivery = delivery?;
    let message = projection
        .messages
        .iter()
        .find(|candidate| candidate.id == delivery.message_id)?;
    projection
        .runs
        .iter()
        .find(|candidate| Some(&candidate.id) == message.run_id.as_ref())
}

/// `emitQueuedRunCancellation`: a queued run, its attempt and its root node
/// end cancelled.
pub(super) fn emit_queued_run_cancellation(
    decision: &mut Decision,
    projection: &OrchestrationV2ThreadProjection,
    run: &OrchestrationV2Run,
    now: UtcDateTime,
) {
    let root_node = run.root_node_id.as_ref().and_then(|id| {
        projection
            .nodes
            .iter()
            .find(|candidate| &candidate.id == id)
    });
    let attempt = run.active_attempt_id.as_ref().and_then(|id| {
        projection
            .attempts
            .iter()
            .find(|candidate| &candidate.id == id)
    });
    let id = decision.event_id(&run.thread_id);
    decision.emit(OrchestrationV2DomainEvent::RunUpdated {
        id,
        thread_id: run.thread_id.clone(),
        run_id: Some(run.id.clone()),
        node_id: run.root_node_id.clone(),
        driver: None,
        provider_instance_id: Some(run.provider_instance_id.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: OrchestrationV2Run {
            status: OrchestrationV2RunStatus::Cancelled,
            queue_position: Some(None),
            completed_at: Some(now),
            ..run.clone()
        },
    });
    if let (Some(attempt), Some(root_node)) = (attempt, root_node) {
        let id = decision.event_id(&run.thread_id);
        decision.emit(OrchestrationV2DomainEvent::RunAttemptUpdated {
            id,
            thread_id: run.thread_id.clone(),
            run_id: Some(run.id.clone()),
            node_id: Some(root_node.id.clone()),
            driver: None,
            provider_instance_id: Some(run.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: turin_contracts::orchestration_v2::OrchestrationV2RunAttempt {
                status: OrchestrationV2RunAttemptStatus::Cancelled,
                completed_at: Some(now),
                ..attempt.clone()
            },
        });
    }
    if let Some(root_node) = root_node {
        let id = decision.event_id(&run.thread_id);
        decision.emit(OrchestrationV2DomainEvent::NodeUpdated {
            id,
            thread_id: run.thread_id.clone(),
            run_id: Some(run.id.clone()),
            node_id: Some(root_node.id.clone()),
            driver: None,
            provider_instance_id: Some(run.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: turin_contracts::orchestration_v2::OrchestrationV2ExecutionNode {
                status: OrchestrationV2ExecutionNodeStatus::Cancelled,
                completed_at: Some(now),
                ..root_node.clone()
            },
        });
    }
}

/// How a cohort ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // `Stopped` is used by run.interrupt (G3).
pub(super) enum CohortEnd {
    Stopped,
    Disposed,
}

/// `disposeDelegatedCompletionCohort`: the parent run's cohort ends, its
/// tasks' undelivered results are dropped, and (unless `cancel_queued_delivery`
/// is false) a delivery run still waiting in the queue is cancelled.
pub(super) fn dispose_delegated_completion_cohort(
    decision: &mut Decision,
    projection: &OrchestrationV2ThreadProjection,
    parent_run_id: &RunId,
    end: CohortEnd,
    now: UtcDateTime,
    cancel_queued_delivery: bool,
) {
    let Some(parent_run) = projection.runs.iter().find(|run| &run.id == parent_run_id) else {
        return;
    };
    let cohort = parent_run.delegated_completion.as_ref();
    let tasks: Vec<_> = projection
        .subagents
        .iter()
        .filter(|task| {
            task.origin == OrchestrationV2SubagentOrigin::AppOwned
                && task.run_id.as_ref() == Some(parent_run_id)
        })
        .collect();
    if cohort.is_none() && tasks.is_empty() {
        return;
    }
    let disposition = if cohort.map(|cohort| cohort.disposition)
        == Some(OrchestrationV2DelegatedCompletionCohortDisposition::Disposed)
    {
        OrchestrationV2DelegatedCompletionCohortDisposition::Disposed
    } else {
        match end {
            CohortEnd::Stopped => OrchestrationV2DelegatedCompletionCohortDisposition::Stopped,
            CohortEnd::Disposed => OrchestrationV2DelegatedCompletionCohortDisposition::Disposed,
        }
    };
    let next_cohort = OrchestrationV2DelegatedCompletionCohort {
        disposition,
        next_generation: cohort.map_or_else(
            || turin_contracts::PositiveInt::new(1).expect("1 is positive"),
            |cohort| cohort.next_generation,
        ),
        delivery: None,
    };
    let id = decision.event_id(&parent_run.thread_id);
    decision.emit(OrchestrationV2DomainEvent::RunUpdated {
        id,
        thread_id: parent_run.thread_id.clone(),
        run_id: Some(parent_run.id.clone()),
        node_id: parent_run.root_node_id.clone(),
        driver: None,
        provider_instance_id: Some(parent_run.provider_instance_id.clone()),
        raw_event_id: None,
        occurred_at: now,
        payload: OrchestrationV2Run {
            delegated_completion: Some(next_cohort),
            ..parent_run.clone()
        },
    });
    for task in tasks {
        if matches!(
            task.completion_delivery
                .as_ref()
                .map(|delivery| delivery.state),
            Some(
                OrchestrationV2DelegatedCompletionTaskDeliveryState::Acknowledged
                    | OrchestrationV2DelegatedCompletionTaskDeliveryState::Delivered
                    | OrchestrationV2DelegatedCompletionTaskDeliveryState::Disposed
            )
        ) {
            continue;
        }
        let id = decision.event_id(&parent_run.thread_id);
        decision.emit(OrchestrationV2DomainEvent::SubagentUpdated {
            id,
            thread_id: parent_run.thread_id.clone(),
            run_id: task.run_id.clone(),
            node_id: Some(task.id.clone()),
            driver: Some(task.driver.clone()),
            provider_instance_id: Some(task.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at: now,
            payload: turin_contracts::orchestration_v2::OrchestrationV2Subagent {
                completion_delivery: Some(OrchestrationV2DelegatedCompletionTaskDelivery {
                    state: OrchestrationV2DelegatedCompletionTaskDeliveryState::Disposed,
                    observed_by_run_id: None,
                }),
                updated_at: now,
                ..task.clone()
            },
        });
    }
    if cancel_queued_delivery
        && let Some(delivery_run) = completion_delivery_run(
            projection,
            cohort.and_then(|cohort| cohort.delivery.as_ref()),
        )
        && delivery_run.status == OrchestrationV2RunStatus::Queued
    {
        emit_queued_run_cancellation(decision, projection, delivery_run, now);
    }
}

/// `disposeAllDelegatedCompletionCohorts`: every run that has a cohort or
/// owns app-owned tasks is disposed, each reading the events before it.
pub(super) fn dispose_all_delegated_completion_cohorts(
    decision: &mut Decision,
    projection: &OrchestrationV2ThreadProjection,
    now: UtcDateTime,
    cancel_queued_delivery: bool,
) {
    let mut seen = HashSet::new();
    let mut parents = Vec::new();
    let from_runs = projection
        .runs
        .iter()
        .filter(|run| run.delegated_completion.is_some())
        .map(|run| run.id.clone());
    let from_tasks = projection
        .subagents
        .iter()
        .filter(|task| task.origin == OrchestrationV2SubagentOrigin::AppOwned)
        .filter_map(|task| task.run_id.clone());
    for id in from_runs.chain(from_tasks) {
        if seen.insert(id.clone()) {
            parents.push(id);
        }
    }
    // Upstream passes the one projection to each disposal, so a later disposal
    // does not see an earlier one's cohort write. Keep that.
    for parent in parents {
        dispose_delegated_completion_cohort(
            decision,
            projection,
            &parent,
            CohortEnd::Disposed,
            now,
            cancel_queued_delivery,
        );
    }
}
