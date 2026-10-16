//! Port of upstream `Orchestrator.ts` archive and settle (`dispatchThreadMutation`
//! plus the cancel, detach and terminal-cleanup blocks that follow it).
//!
//! Archive cancels the thread's queued runs (with their attempts and nodes),
//! detaches live provider sessions with MCP revocation, and queues the
//! terminal cleanup effect. Settle is a row update plus session detach.
//!
//! Archive also disposes every delegated-completion cohort.
//!
//! Not ported, and refused loudly rather than skipped:
//! * settle with a pending message-capable question (its cancel is the
//!   runtime-request respond path, owned by T13e);
//! * settle with an automatic queued run (its cancel is `queued-run.cancel`).

use std::collections::HashSet;

use turin_contracts::{
    MessageId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2AppThread, OrchestrationV2Command as C,
        OrchestrationV2ConversationMessageRole, OrchestrationV2DomainEvent,
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2ResponseCapability,
        OrchestrationV2RunAttemptStatus, OrchestrationV2RunStatus,
        OrchestrationV2RuntimeRequestKind, OrchestrationV2RuntimeRequestStatus,
        OrchestrationV2ServerCommand, OrchestrationV2ThreadProjection,
    },
};

use super::Orchestrator;
use super::cohort::{dispose_all_delegated_completion_cohorts, with_pending_events};
use super::dispatch::{Decision, dispatch_failure};
use super::provider_detach;
use crate::pipeline::effect_outbox::{EffectRequest, PendingEffect};
use crate::pipeline::orchestrator_port::{
    OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
};

/// Decides archive and settle. `Ok(false)` leaves the command to another group.
pub(super) async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    let OrchestrationV2ServerCommand::Client(client) = command else {
        return Ok(false);
    };
    match client {
        C::ThreadArchive { thread_id, .. } => {
            let thread =
                super::thread_mutation::load_thread(orchestrator, command, thread_id).await?;
            if thread.archived_at.is_some() {
                return Err(dispatch_failure(
                    command,
                    format!("Thread {thread_id} is already archived."),
                ));
            }
            let records = orchestrator
                .get_thread_records(
                    thread_id,
                    &[
                        RecordField::Runs,
                        RecordField::Attempts,
                        RecordField::Nodes,
                        RecordField::Subagents,
                        RecordField::ProviderSessions,
                    ],
                    &RecordFilter::default(),
                )
                .await?;
            let now = decision.now();
            let updated = OrchestrationV2AppThread {
                archived_at: Some(now),
                title_regeneration: Some(None),
                pull_requests: without_watches(thread.pull_requests.clone()),
                updated_at: now,
                ..thread
            };
            let id = decision.event_id(thread_id);
            let occurred_at = now;
            decision.emit(OrchestrationV2DomainEvent::ThreadArchived {
                id,
                thread_id: thread_id.clone(),
                run_id: None,
                node_id: None,
                driver: None,
                provider_instance_id: Some(updated.provider_instance_id.clone()),
                raw_event_id: None,
                occurred_at,
                payload: updated,
            });
            cancel_queued_runs(&records, decision, thread_id, now);
            // The disposals read what the cancel block just wrote.
            let pending = with_pending_events(records.clone(), &decision.events, thread_id);
            dispose_all_delegated_completion_cohorts(decision, &pending, now, false);
            provider_detach::detach_sessions(
                decision,
                thread_id,
                &records.provider_sessions,
                "Thread archived.",
                true,
            );
            let command_id = decision.command_id().clone();
            decision.effects.push(PendingEffect {
                id: format!("effect:{command_id}:terminal.cleanup"),
                command_id,
                thread_id: thread_id.clone(),
                request: EffectRequest::TerminalCleanup {},
                available_at: None,
            });
        }
        C::ThreadSettle {
            thread_id,
            settled_at,
            ..
        } => {
            let thread =
                super::thread_mutation::load_live_thread(orchestrator, command, thread_id).await?;
            let records = orchestrator
                .get_thread_records(
                    thread_id,
                    &[
                        RecordField::Runs,
                        RecordField::RuntimeRequests,
                        RecordField::Messages,
                        RecordField::ProviderSessions,
                    ],
                    &RecordFilter {
                        message_roles: Some(vec![OrchestrationV2ConversationMessageRole::User]),
                        ..RecordFilter::default()
                    },
                )
                .await?;
            let automatic_ids: HashSet<&MessageId> = records
                .messages
                .iter()
                .filter(|message| {
                    message.notification.is_some() || message.delegated_completion.is_some()
                })
                .map(|message| &message.id)
                .collect();
            let queued = |run: &&turin_contracts::orchestration_v2::OrchestrationV2Run| {
                run.status == OrchestrationV2RunStatus::Queued
            };
            let automatic_queued: Vec<_> = records
                .runs
                .iter()
                .filter(queued)
                .filter(|run| automatic_ids.contains(&run.user_message_id))
                .collect();
            let running = records.runs.iter().any(|run| {
                matches!(
                    run.status,
                    OrchestrationV2RunStatus::Preparing
                        | OrchestrationV2RunStatus::Starting
                        | OrchestrationV2RunStatus::Running
                        | OrchestrationV2RunStatus::Waiting
                )
            });
            let queued_message = records
                .runs
                .iter()
                .filter(queued)
                .any(|run| !automatic_ids.contains(&run.user_message_id));
            let pending: Vec<_> = records
                .runtime_requests
                .iter()
                .filter(|request| request.status == OrchestrationV2RuntimeRequestStatus::Pending)
                .collect();
            let blocking = pending.iter().any(|request| {
                request.kind != OrchestrationV2RuntimeRequestKind::UserInput
                    || request.response_capability != OrchestrationV2ResponseCapability::Message
            });
            let blocker = if blocking {
                Some("is waiting on an approval or question. Answer it before settling.")
            } else if running {
                Some("is still running. Stop it before settling.")
            } else if queued_message {
                Some("has a queued message. Send it or remove it from the queue before settling.")
            } else {
                None
            };
            if let Some(blocker) = blocker {
                return Err(dispatch_failure(
                    command,
                    format!("Thread {thread_id} {blocker}"),
                ));
            }
            if !pending.is_empty() {
                return Err(dispatch_failure(
                    command,
                    format!(
                        "Thread {thread_id} has a pending question; cancelling it needs the runtime-request port."
                    ),
                ));
            }
            if !automatic_queued.is_empty() {
                return Err(dispatch_failure(
                    command,
                    format!(
                        "Thread {thread_id} has queued automatic work; cancelling it needs the queued-run port."
                    ),
                ));
            }
            let now = decision.now();
            // Settling again with the same settle time keeps updatedAt, so the repeat is a no-op.
            let was_pinned = thread.pinned_at.flatten().is_some();
            let already_settled = thread.settled_override
                == Some(turin_contracts::orchestration_v2::OrchestrationV2SettleOverride::Settled)
                && thread.settled_at.is_some()
                && !was_pinned;
            let updated = OrchestrationV2AppThread {
                settled_override: Some(
                    turin_contracts::orchestration_v2::OrchestrationV2SettleOverride::Settled,
                ),
                settled_at: if already_settled {
                    thread.settled_at
                } else {
                    Some(settled_at.unwrap_or(now))
                },
                pull_requests: without_watches(thread.pull_requests.clone()),
                unsettled_at: Some(None),
                pinned_at: Some(None),
                pin_order_key: Some(None),
                active_order_key: Some(None),
                updated_at: if already_settled {
                    thread.updated_at
                } else {
                    now
                },
                ..thread
            };
            let id = decision.event_id(thread_id);
            let occurred_at = now;
            decision.emit(OrchestrationV2DomainEvent::ThreadSettled {
                id,
                thread_id: thread_id.clone(),
                run_id: None,
                node_id: None,
                driver: None,
                provider_instance_id: Some(updated.provider_instance_id.clone()),
                raw_event_id: None,
                occurred_at,
                payload: updated,
            });
            provider_detach::detach_sessions(
                decision,
                thread_id,
                &records.provider_sessions,
                "Thread settled.",
                false,
            );
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// Upstream `withPullRequestWatch(link, undefined)`: archive and settle drop
/// every pull-request watch.
pub(super) fn without_watches(
    links: Option<Vec<turin_contracts::thread_pull_request::ThreadPullRequestLink>>,
) -> Option<Vec<turin_contracts::thread_pull_request::ThreadPullRequestLink>> {
    links.map(|links| {
        links
            .into_iter()
            .map(|mut link| {
                link.watch = None;
                link
            })
            .collect()
    })
}

/// Archive's cancel block: queued runs end cancelled, and so do their attempts
/// and nodes that are still in flight.
fn cancel_queued_runs(
    records: &OrchestrationV2ThreadProjection,
    decision: &mut Decision,
    thread_id: &turin_contracts::ThreadId,
    now: UtcDateTime,
) {
    let queued: Vec<_> = records
        .runs
        .iter()
        .filter(|run| run.status == OrchestrationV2RunStatus::Queued)
        .cloned()
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
            payload: turin_contracts::orchestration_v2::OrchestrationV2Run {
                status: OrchestrationV2RunStatus::Cancelled,
                queue_position: Some(None),
                completed_at: Some(now),
                ..run.clone()
            },
        });
    }
    let run_ids: HashSet<_> = queued.iter().map(|run| run.id.clone()).collect();
    for attempt in records.attempts.iter().filter(|attempt| {
        run_ids.contains(&attempt.run_id)
            && matches!(
                attempt.status,
                OrchestrationV2RunAttemptStatus::Pending | OrchestrationV2RunAttemptStatus::Running
            )
    }) {
        let provider_instance_id = queued
            .iter()
            .find(|run| run.id == attempt.run_id)
            .map(|run| run.provider_instance_id.clone());
        let id = decision.event_id(thread_id);
        decision.emit(OrchestrationV2DomainEvent::RunAttemptUpdated {
            id,
            thread_id: thread_id.clone(),
            run_id: Some(attempt.run_id.clone()),
            node_id: Some(attempt.root_node_id.clone()),
            driver: None,
            provider_instance_id,
            raw_event_id: None,
            occurred_at: now,
            payload: turin_contracts::orchestration_v2::OrchestrationV2RunAttempt {
                status: OrchestrationV2RunAttemptStatus::Cancelled,
                completed_at: Some(now),
                ..attempt.clone()
            },
        });
    }
    for node in records.nodes.iter().filter(|node| {
        node.run_id
            .as_ref()
            .is_some_and(|run_id| run_ids.contains(run_id))
            && matches!(
                node.status,
                OrchestrationV2ExecutionNodeStatus::Pending
                    | OrchestrationV2ExecutionNodeStatus::Running
                    | OrchestrationV2ExecutionNodeStatus::Waiting
            )
    }) {
        let provider_instance_id = queued
            .iter()
            .find(|run| Some(&run.id) == node.run_id.as_ref())
            .map(|run| run.provider_instance_id.clone());
        let id = decision.event_id(thread_id);
        decision.emit(OrchestrationV2DomainEvent::NodeUpdated {
            id,
            thread_id: thread_id.clone(),
            run_id: node.run_id.clone(),
            node_id: Some(node.id.clone()),
            driver: None,
            provider_instance_id,
            raw_event_id: None,
            occurred_at: now,
            payload: turin_contracts::orchestration_v2::OrchestrationV2ExecutionNode {
                status: OrchestrationV2ExecutionNodeStatus::Cancelled,
                completed_at: Some(now),
                ..node.clone()
            },
        });
    }
}
