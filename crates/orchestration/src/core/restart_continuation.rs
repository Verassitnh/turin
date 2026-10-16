//! Port of the pure part of upstream/apps/server/src/orchestration-v2/RestartContinuation.ts:
//! `restartContinuationRun`, `restartContinuationBlocked` and `hasOpenAwaitedWork`.
//!
//! Decides which run a restart continues, from the persisted projection alone.
//! The effectful half (`continueRestartedRun`, which dispatches the continuation
//! prompt through `ThreadManagementService`) is not ported yet; it reuses the
//! blocked check here.

use chrono::{DateTime, Utc};
use turin_contracts::orchestration_v2::{
    OrchestrationV2AppThread, OrchestrationV2NativeRefStrength,
    OrchestrationV2PendingBackgroundTask, OrchestrationV2ProviderSession,
    OrchestrationV2ProviderSessionStatus, OrchestrationV2ProviderThread,
    OrchestrationV2ProviderThreadStatus, OrchestrationV2ProviderTurn,
    OrchestrationV2ResponseCapability, OrchestrationV2Run, OrchestrationV2RunStatus,
    OrchestrationV2RuntimeRequest, OrchestrationV2RuntimeRequestKind,
    OrchestrationV2RuntimeRequestStatus, OrchestrationV2SecretRequestStatus,
    OrchestrationV2SettleOverride, OrchestrationV2SubagentOrigin, OrchestrationV2ThreadProjection,
    OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus, latest_provider_turn_for_attempt,
};

use super::restart_background_note::{
    cancelled_roster_task_work, cancelled_turn_item_work, is_awaited_work,
};
use crate::projection::shared::thread_error::run_ran_after;
use crate::projection::store::runtime_recovery::RuntimeRecoveryState;
use crate::projection::turn_item;

/// The slice of a projection the restart selection reads. Upstream types these
/// functions as a `Pick` of `ProjectionRuntimeRecoveryState`, so the full thread
/// projection and the recovery projection both fit.
#[derive(Debug, Clone, Copy)]
pub struct RestartProjection<'a> {
    pub thread: &'a OrchestrationV2AppThread,
    pub runs: &'a [OrchestrationV2Run],
    pub provider_sessions: &'a [OrchestrationV2ProviderSession],
    pub provider_threads: &'a [OrchestrationV2ProviderThread],
    pub provider_turns: &'a [OrchestrationV2ProviderTurn],
    pub runtime_requests: &'a [OrchestrationV2RuntimeRequest],
    pub turn_items: &'a [OrchestrationV2TurnItem],
}

impl<'a> From<&'a OrchestrationV2ThreadProjection> for RestartProjection<'a> {
    fn from(projection: &'a OrchestrationV2ThreadProjection) -> Self {
        Self {
            thread: &projection.thread,
            runs: &projection.runs,
            provider_sessions: &projection.provider_sessions,
            provider_threads: &projection.provider_threads,
            provider_turns: &projection.provider_turns,
            runtime_requests: &projection.runtime_requests,
            turn_items: &projection.turn_items,
        }
    }
}

impl<'a> From<&'a RuntimeRecoveryState> for RestartProjection<'a> {
    fn from(projection: &'a RuntimeRecoveryState) -> Self {
        Self {
            thread: &projection.thread,
            runs: &projection.runs,
            provider_sessions: &projection.provider_sessions,
            provider_threads: &projection.provider_threads,
            provider_turns: &projection.provider_turns,
            runtime_requests: &projection.runtime_requests,
            turn_items: &projection.turn_items,
        }
    }
}

/// Whether a restart must leave this run for the user: the thread is put away,
/// the provider turn already ended, or the user owes an answer. Checked when
/// intent is recorded and again when the continuation is delivered. A `settled`
/// run is woken for the awaited work it lost, so its ended provider turn does
/// not block it.
pub fn restart_continuation_blocked<'a>(
    projection: impl Into<RestartProjection<'a>>,
    source: &OrchestrationV2Run,
    now: DateTime<Utc>,
    settled: bool,
) -> bool {
    let projection = projection.into();
    let thread = projection.thread;
    let snoozed = thread
        .snoozed_until
        .flatten()
        .is_some_and(|until| until.as_datetime() > now);
    if thread.archived_at.is_some()
        || thread.deleted_at.is_some()
        || thread.settled_override == Some(OrchestrationV2SettleOverride::Settled)
        || snoozed
    {
        return true;
    }
    let turn = latest_provider_turn_for_attempt(
        projection.provider_turns,
        source.active_attempt_id.as_ref(),
    );
    if !settled
        && turn.is_some_and(|turn| {
            matches!(
            turn.status,
            turin_contracts::orchestration_v2::OrchestrationV2ProviderTurnStatus::Completed
                | turin_contracts::orchestration_v2::OrchestrationV2ProviderTurnStatus::Failed
                | turin_contracts::orchestration_v2::OrchestrationV2ProviderTurnStatus::Interrupted
        )
        })
    {
        return true;
    }
    let waiting_request = projection.runtime_requests.iter().any(|request| {
        // Tool calls and auth refreshes do not wait on a person, and a
        // message-capable question stays answerable after the restart.
        !matches!(
            request.kind,
            OrchestrationV2RuntimeRequestKind::DynamicToolCall
                | OrchestrationV2RuntimeRequestKind::AuthRefresh
        ) && !matches!(
            request.response_capability,
            OrchestrationV2ResponseCapability::Message
        ) && (request.status == OrchestrationV2RuntimeRequestStatus::Pending
            // Recovery closes live callbacks on the cut turn; it does not answer them.
            || ((request.status == OrchestrationV2RuntimeRequestStatus::Expired
                || request.status == OrchestrationV2RuntimeRequestStatus::Cancelled)
                && matches!(
                    request.response_capability,
                    OrchestrationV2ResponseCapability::NotResumable { .. }
                )
                && turn.is_some_and(|turn| Some(&turn.id) == request.provider_turn_id.as_ref())))
    });
    if waiting_request {
        return true;
    }
    projection.turn_items.iter().any(|item| match item {
        OrchestrationV2TurnItem::SecretRequest {
            run_id,
            secret_status,
            completed_at,
            ..
        } => {
            run_id.as_ref() == Some(&source.id)
                && (*secret_status == OrchestrationV2SecretRequestStatus::Pending
                    // Recovery cancels an open secret card and its run at the same instant.
                    || (*secret_status == OrchestrationV2SecretRequestStatus::Cancelled
                        && completed_at.is_some()
                        && source.completed_at.is_some()
                        && *completed_at == source.completed_at))
        }
        _ => false,
    })
}

/// Open awaited work (see `is_awaited_work`) on a provider thread. App-owned
/// delegations are not counted: their own threads continue and report back.
pub fn has_open_awaited_work<'a>(
    projection: impl Into<RestartProjection<'a>>,
    provider_thread: &OrchestrationV2ProviderThread,
) -> bool {
    let projection = projection.into();
    let open_item = projection.turn_items.iter().any(|item| {
        let common = turn_item::common(item);
        if !matches!(
            common.status,
            OrchestrationV2TurnItemStatus::Pending
                | OrchestrationV2TurnItemStatus::Running
                | OrchestrationV2TurnItemStatus::Waiting
        ) {
            return false;
        }
        if let OrchestrationV2TurnItem::Subagent {
            origin,
            child_thread_id,
            ..
        } = item
            && *origin == OrchestrationV2SubagentOrigin::AppOwned
            && child_thread_id.is_some()
        {
            return false;
        }
        let Some(work) = cancelled_turn_item_work(item) else {
            return false;
        };
        let provider_thread_id = common.provider_thread_id.or_else(|| {
            common.run_id.and_then(|run_id| {
                projection
                    .runs
                    .iter()
                    .find(|run| &run.id == run_id)
                    .and_then(|run| run.provider_thread_id.as_ref())
            })
        });
        is_awaited_work(&work) && provider_thread_id == Some(&provider_thread.id)
    });
    open_item
        || (provider_thread.owner_node_id.is_none()
            && provider_thread.pending_background_tasks.iter().any(
                |task: &OrchestrationV2PendingBackgroundTask| {
                    is_awaited_work(&cancelled_roster_task_work(task))
                },
            ))
}

/// The run a restart continues: an unfinished root turn, or a settled one
/// waiting on awaited work that the restart cancels. A settled run needs a live
/// session, so stale records from an older crash never wake it. Other leftover
/// work is cleaned up and reported on the next user turn.
pub fn restart_continuation_run<'a>(
    projection: impl Into<RestartProjection<'a>>,
    now: DateTime<Utc>,
) -> Option<&'a OrchestrationV2Run> {
    let projection = projection.into();
    // Queued runs never started; recovery holds them behind the cut run.
    let run = projection
        .runs
        .iter()
        .filter(|candidate| candidate.status != OrchestrationV2RunStatus::Queued)
        .fold(
            None::<&OrchestrationV2Run>,
            |latest, candidate| match latest {
                Some(latest) if !run_ran_after(candidate, latest) => Some(latest),
                _ => Some(candidate),
            },
        )?;
    let prepared_continuation = run.status == OrchestrationV2RunStatus::Starting
        && run.restart_continuation_of_run_id.is_some();
    let settled = matches!(
        run.status,
        OrchestrationV2RunStatus::Completed | OrchestrationV2RunStatus::Waiting
    );
    if run.status != OrchestrationV2RunStatus::Running && !prepared_continuation && !settled {
        return None;
    }
    if restart_continuation_blocked(projection, run, now, settled) {
        return None;
    }
    let live_turn_required = !prepared_continuation && !settled;
    if projection.thread.provider_instance_id != run.provider_instance_id {
        return None;
    }
    let provider_thread = projection
        .provider_threads
        .iter()
        .find(|thread| Some(&thread.id) == run.provider_thread_id.as_ref())?;
    let native_ref = provider_thread.native_thread_ref.as_ref();
    if provider_thread.app_thread_id.as_ref() != Some(&projection.thread.id)
        || provider_thread.owner_node_id.is_some()
        || provider_thread.provider_instance_id != run.provider_instance_id
        || native_ref.is_none_or(|native| native.native_id.is_none())
        || native_ref
            .is_some_and(|native| native.strength != OrchestrationV2NativeRefStrength::Strong)
        || native_ref.is_some_and(|native| native.driver != provider_thread.driver)
        || (live_turn_required
            && provider_thread.status != OrchestrationV2ProviderThreadStatus::Active)
        || matches!(
            provider_thread.status,
            OrchestrationV2ProviderThreadStatus::Closed
                | OrchestrationV2ProviderThreadStatus::Archived
        )
    {
        return None;
    }
    let session = projection
        .provider_sessions
        .iter()
        .find(|candidate| Some(&candidate.id) == provider_thread.provider_session_id.as_ref())?;
    // Most adapters keep a live session "ready" through its turns, so only a
    // stopped or failed session rules out a live turn.
    if session.provider_instance_id != run.provider_instance_id
        || session.driver != provider_thread.driver
        || (!prepared_continuation && session_is_dead(session))
    {
        return None;
    }
    if settled {
        return has_open_awaited_work(projection, provider_thread).then_some(run);
    }
    if live_turn_required
        && !projection.provider_turns.iter().any(|turn| {
            turn.provider_thread_id == provider_thread.id
                && turn.run_attempt_id == run.active_attempt_id
                && turn.status
                    == turin_contracts::orchestration_v2::OrchestrationV2ProviderTurnStatus::Running
        })
    {
        return None;
    }
    Some(run)
}

fn session_is_dead(session: &OrchestrationV2ProviderSession) -> bool {
    matches!(
        session.status,
        OrchestrationV2ProviderSessionStatus::Stopped | OrchestrationV2ProviderSessionStatus::Error
    )
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use serde_json::{Value, json};

    use super::*;
    use crate::projection::test_support;

    const THREAD: &str = "thread:restart";
    const RUN: &str = "run:restart";
    const INSTANCE: &str = "codex";
    const PROVIDER_THREAD: &str = "provider-thread:restart";
    const SESSION: &str = "session:restart";
    const ATTEMPT: &str = "attempt:restart";
    const T0: &str = "2026-10-03T12:00:00.000Z";

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 3, 12, 0, 0).unwrap()
    }

    /// The base fixture: a running root run with a live native thread, a ready
    /// session and an in-flight provider turn. Mirrors upstream's `makeProjection`.
    fn base() -> Value {
        let mut thread = test_support::thread(THREAD);
        thread["projectId"] = json!("restart-project");
        thread["providerInstanceId"] = json!(INSTANCE);
        let mut run = test_support::run(THREAD, RUN, 1, "running");
        run["providerInstanceId"] = json!(INSTANCE);
        run["providerThreadId"] = json!(PROVIDER_THREAD);
        run["activeAttemptId"] = json!(ATTEMPT);
        let mut session = session_json(SESSION, "running");
        session["providerInstanceId"] = json!(INSTANCE);
        json!({
            "thread": thread,
            "runs": [run],
            "attempts": [],
            "nodes": [],
            "subagents": [],
            "providerSessions": [session],
            "providerThreads": [provider_thread_json(THREAD, "active", true)],
            "providerTurns": [provider_turn_json("running")],
            "runtimeRequests": [],
            "messages": [],
            "plans": [],
            "turnItems": [],
            "checkpointScopes": [],
            "checkpoints": [],
            "contextHandoffs": [],
            "contextTransfers": [],
            "visibleTurnItems": [],
            "updatedAt": T0,
        })
    }

    fn session_json(id: &str, status: &str) -> Value {
        json!({
            "id": id,
            "driver": "codex",
            "providerInstanceId": INSTANCE,
            "status": status,
            "cwd": "/repo",
            "model": null,
            "capabilities": test_support::capabilities(),
            "createdAt": T0,
            "updatedAt": T0,
            "lastError": null,
        })
    }

    fn provider_thread_json(app_thread: &str, status: &str, native: bool) -> Value {
        json!({
            "id": PROVIDER_THREAD,
            "driver": "codex",
            "providerInstanceId": INSTANCE,
            "providerSessionId": SESSION,
            "appThreadId": app_thread,
            "ownerNodeId": null,
            "nativeThreadRef": if native {
                json!({"driver": "codex", "nativeId": "native-thread", "strength": "strong"})
            } else {
                Value::Null
            },
            "nativeConversationHeadRef": null,
            "status": status,
            "firstRunOrdinal": null,
            "lastRunOrdinal": null,
            "handoffIds": [],
            "forkedFrom": null,
            "createdAt": T0,
            "updatedAt": T0,
        })
    }

    fn provider_turn_json(status: &str) -> Value {
        json!({
            "id": "turn:restart",
            "providerThreadId": PROVIDER_THREAD,
            "nodeId": "node:restart",
            "runAttemptId": ATTEMPT,
            "nativeTurnRef": null,
            "ordinal": 1,
            "status": status,
            "startedAt": T0,
            "completedAt": null,
            "tokenUsage": null,
            "turnTokenUsage": null,
        })
    }

    fn projection(value: Value) -> OrchestrationV2ThreadProjection {
        serde_json::from_value(value).expect("projection fixture")
    }

    /// The base fixture with one edit applied.
    fn mutate(edit: impl FnOnce(&mut Value)) -> Value {
        let mut value = base();
        edit(&mut value);
        value
    }

    #[test]
    fn requires_matching_saved_native_state_for_an_unfinished_root_run() {
        let valid = projection(base());
        assert_eq!(
            restart_continuation_run(&valid, now()).map(|run| run.id.as_str()),
            Some(RUN)
        );
        let invalid = [
            mutate(|v| v["thread"]["archivedAt"] = json!(T0)),
            mutate(|v| v["thread"]["deletedAt"] = json!(T0)),
            mutate(|v| v["thread"]["providerInstanceId"] = json!("other")),
            mutate(|v| v["providerThreads"][0]["nativeThreadRef"] = Value::Null),
            mutate(|v| v["providerSessions"][0]["providerInstanceId"] = json!("other")),
            mutate(|v| v["providerTurns"] = json!([])),
        ];
        for value in invalid {
            assert!(restart_continuation_run(&projection(value), now()).is_none());
        }
        for status in [
            "queued",
            "preparing",
            "starting",
            "waiting",
            "completed",
            "cancelled",
            "failed",
            "interrupted",
        ] {
            let value = mutate(|v| v["runs"][0]["status"] = json!(status));
            assert!(
                restart_continuation_run(&projection(value), now()).is_none(),
                "{status}"
            );
        }
    }

    #[test]
    fn continues_a_live_turn_whose_session_the_adapter_never_marked_running() {
        for status in ["starting", "ready", "running", "waiting"] {
            let value = mutate(|v| v["providerSessions"][0]["status"] = json!(status));
            assert!(
                restart_continuation_run(&projection(value), now()).is_some(),
                "{status}"
            );
        }
        for status in ["stopped", "error"] {
            let value = mutate(|v| v["providerSessions"][0]["status"] = json!(status));
            assert!(
                restart_continuation_run(&projection(value), now()).is_none(),
                "{status}"
            );
        }
    }

    #[test]
    fn recovers_an_admitted_continuation_after_another_crash_before_provider_start() {
        let mut value = base();
        value["runs"][0]["status"] = json!("starting");
        value["runs"][0]["restartContinuationOfRunId"] = json!("run:previous-crash");
        value["providerThreads"][0]["status"] = json!("idle");
        value["providerSessions"][0]["status"] = json!("stopped");
        value["providerTurns"] = json!([]);
        assert_eq!(
            restart_continuation_run(&projection(value), now()).map(|run| run.id.as_str()),
            Some(RUN)
        );
    }

    #[test]
    fn does_not_continue_settled_root_runs_with_restart_cancelled_background_work() {
        for project_id in ["project:scratch", "restart-project"] {
            for status in ["completed", "waiting"] {
                let mut value = base();
                value["thread"]["projectId"] = json!(project_id);
                value["runs"][0]["status"] = json!(status);
                value["runs"][0]["restartCancelledBackgroundWork"] =
                    json!([{"kind": "shell", "label": "sleep 25"}]);
                value["providerThreads"][0]["status"] = json!("idle");
                value["providerSessions"][0]["status"] = json!("stopped");
                value["providerTurns"] = json!([]);
                assert!(
                    restart_continuation_run(&projection(value), now()).is_none(),
                    "{project_id} {status}"
                );
            }
        }
    }
}
