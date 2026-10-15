//! Port of the state-only arms of upstream `Orchestrator.ts`
//! `dispatchThreadMutation`: unarchive, pin, unpin, pin and active reorder,
//! auto-settle, and mark-unread. Each one only rewrites the thread row.
//!
//! Not here: archive, settle, unsettle and snooze (they also cancel runs or
//! detach provider sessions), and the metadata, pull-request, mode and model
//! kinds. Those return `false` from [`decide`] and stay unported.

use turin_contracts::{
    UtcDateTime,
    orchestration_v2::{
        OrchestrationV2AppThread, OrchestrationV2Command as C, OrchestrationV2DomainEvent,
        OrchestrationV2ServerCommand, OrchestrationV2SettleOverride,
    },
};

use super::Orchestrator;
use super::dispatch::{Decision, dispatch_failure};
use crate::pipeline::orchestrator_port::{
    OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
};

/// Emits the `thread.*` event for `$variant` with `$thread` as its payload.
/// Every thread-row event carries the same envelope, so the fields are spelled
/// out once here.
macro_rules! thread_event {
    ($decision:expr, $variant:ident, $thread:expr) => {{
        let thread: OrchestrationV2AppThread = $thread;
        let id = $decision.event_id(&thread.id);
        let occurred_at = $decision.now();
        $decision.emit(OrchestrationV2DomainEvent::$variant {
            id,
            thread_id: thread.id.clone(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id: Some(thread.provider_instance_id.clone()),
            raw_event_id: None,
            occurred_at,
            payload: thread,
        });
    }};
}

/// Decides the thread-row kinds ported here. `Ok(false)` leaves the command to
/// another group.
pub(super) async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    let OrchestrationV2ServerCommand::Client(client) = command else {
        return Ok(false);
    };
    match client {
        C::ThreadUnarchive { thread_id, .. } => {
            let thread = load_thread(orchestrator, command, thread_id).await?;
            if thread.archived_at.is_none() {
                return Err(dispatch_failure(
                    command,
                    format!("Thread {thread_id} is not archived."),
                ));
            }
            let now = decision.now();
            let updated = OrchestrationV2AppThread {
                archived_at: None,
                updated_at: now,
                ..thread
            };
            thread_event!(decision, ThreadUnarchived, updated);
        }
        C::ThreadPin {
            thread_id,
            order_key,
            ..
        } => {
            let thread = load_live_thread(orchestrator, command, thread_id).await?;
            let now = decision.now();
            let already_pinned = thread.pinned_at.flatten().is_some();
            // Pinning promotes: an explicit settle is cleared and a snooze's
            // return ticket is spent, so the thread is on top now.
            let promotes = thread.settled_override == Some(OrchestrationV2SettleOverride::Settled)
                || thread.snoozed_until.flatten().is_some();
            let was_snoozed = thread.snoozed_until.flatten().is_some();
            let was_settled =
                thread.settled_override == Some(OrchestrationV2SettleOverride::Settled);
            let updated = OrchestrationV2AppThread {
                pinned_at: if already_pinned {
                    thread.pinned_at
                } else {
                    Some(Some(now))
                },
                // A fresh pin takes the client's slot; a re-pin keeps the existing
                // key so a raced duplicate cannot move a placed thread.
                pin_order_key: match order_key {
                    Some(key) if !already_pinned => Some(Some(key.clone())),
                    _ => thread.pin_order_key.clone(),
                },
                settled_override: if was_settled {
                    Some(OrchestrationV2SettleOverride::Active)
                } else {
                    thread.settled_override
                },
                settled_at: if was_settled { None } else { thread.settled_at },
                snoozed_until: Some(None),
                snoozed_at: Some(None),
                last_snooze_wake_at: if was_snoozed {
                    Some(Some(now))
                } else {
                    thread.last_snooze_wake_at
                },
                updated_at: if already_pinned && !promotes {
                    thread.updated_at
                } else {
                    now
                },
                ..thread
            };
            thread_event!(decision, ThreadPinned, updated);
        }
        C::ThreadUnpin { thread_id, .. } => {
            let thread = load_live_thread(orchestrator, command, thread_id).await?;
            let now = decision.now();
            let already_unpinned = thread.pinned_at.flatten().is_none();
            let updated = OrchestrationV2AppThread {
                pinned_at: Some(None),
                // Unpin clears the slot: re-pinning is "pin again", not "restore an
                // old position".
                pin_order_key: Some(None),
                updated_at: if already_unpinned {
                    thread.updated_at
                } else {
                    now
                },
                ..thread
            };
            thread_event!(decision, ThreadUnpinned, updated);
        }
        C::ThreadPinReorder {
            thread_id,
            order_key,
            ..
        } => {
            let thread = load_live_thread(orchestrator, command, thread_id).await?;
            if thread.pinned_at.flatten().is_none() {
                return Err(dispatch_failure(
                    command,
                    format!("Thread {thread_id} is not pinned and cannot be reordered."),
                ));
            }
            let now = decision.now();
            let key_unchanged = thread.pin_order_key.clone().flatten().as_ref() == Some(order_key);
            let updated = OrchestrationV2AppThread {
                pin_order_key: Some(Some(order_key.clone())),
                updated_at: if key_unchanged {
                    thread.updated_at
                } else {
                    now
                },
                ..thread
            };
            thread_event!(decision, ThreadPinReordered, updated);
        }
        C::ThreadActiveReorder {
            thread_id,
            order_key,
            ..
        } => {
            let thread = load_live_thread(orchestrator, command, thread_id).await?;
            if thread.pinned_at.flatten().is_some()
                || thread.settled_override == Some(OrchestrationV2SettleOverride::Settled)
            {
                return Err(dispatch_failure(
                    command,
                    format!("Thread {thread_id} is not active and cannot be reordered."),
                ));
            }
            // Arranging the active list is not thread activity, so updatedAt stays.
            let updated = OrchestrationV2AppThread {
                active_order_key: Some(Some(order_key.clone())),
                ..thread
            };
            thread_event!(decision, ThreadActiveReordered, updated);
        }
        C::ThreadAutoSettleSet {
            thread_id, enabled, ..
        } => {
            let thread = load_live_thread(orchestrator, command, thread_id).await?;
            let now = decision.now();
            let unchanged = *enabled == thread.auto_settle_disabled_at.flatten().is_none();
            let updated = OrchestrationV2AppThread {
                auto_settle_disabled_at: if *enabled {
                    Some(None)
                } else {
                    Some(Some(
                        thread.auto_settle_disabled_at.flatten().unwrap_or(now),
                    ))
                },
                updated_at: if unchanged { thread.updated_at } else { now },
                ..thread
            };
            thread_event!(decision, ThreadAutoSettleSet, updated);
        }
        C::ThreadMarkUnread { thread_id, .. } => {
            let thread = load_thread(orchestrator, command, thread_id).await?;
            let records = orchestrator
                .get_thread_records(thread_id, &[RecordField::Runs], &RecordFilter::default())
                .await?;
            // Upstream reads the latest run by position, not by time.
            let latest_completed_at = records.runs.last().and_then(|run| run.completed_at);
            let Some(completed_at) = latest_completed_at else {
                return Err(dispatch_failure(
                    command,
                    format!("Thread {thread_id} has no completed run to mark unread."),
                ));
            };
            // One millisecond before the run finished, so the thread shows unread.
            let marked_at = UtcDateTime::from_datetime(
                completed_at.as_datetime() - chrono::Duration::milliseconds(1),
            );
            let updated = OrchestrationV2AppThread {
                last_visited_at: Some(marked_at),
                ..thread
            };
            thread_event!(decision, ThreadMarkedUnread, updated);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// Loads the thread a mutation addresses. A deleted thread refuses every kind.
async fn load_thread(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    thread_id: &turin_contracts::ThreadId,
) -> Result<OrchestrationV2AppThread, OrchestratorError> {
    let thread = orchestrator
        .projection
        .get_thread(thread_id)
        .await
        .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;
    if thread.deleted_at.is_some() {
        return Err(dispatch_failure(
            command,
            format!("Thread {thread_id} is deleted."),
        ));
    }
    Ok(thread)
}

/// [`load_thread`], then refuses an archived thread. Used by the kinds upstream
/// lists as rejected on archive.
async fn load_live_thread(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    thread_id: &turin_contracts::ThreadId,
) -> Result<OrchestrationV2AppThread, OrchestratorError> {
    let thread = load_thread(orchestrator, command, thread_id).await?;
    if thread.archived_at.is_some() {
        return Err(dispatch_failure(
            command,
            format!("Thread {thread_id} is archived."),
        ));
    }
    Ok(thread)
}
