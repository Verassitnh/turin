//! Port of the metadata arms of upstream `Orchestrator.ts` `dispatchThreadMutation`:
//! `thread.metadata.update` (title, branch, worktree, limit recovery, linked
//! pull request, title regeneration) and `thread.title.regeneration.complete`.
//!
//! A worktree change detaches every live provider session (upstream's
//! `detachSessionIds`). The metadata event comes first, then the title effect,
//! then the detaches, as upstream queues them.

use chrono::DateTime;
use turin_contracts::{
    ThreadId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2AppThread, OrchestrationV2Command as C, OrchestrationV2LimitRecovery,
        OrchestrationV2LimitRecoveryUpdate, OrchestrationV2ProviderFailureClass,
        OrchestrationV2RuntimeRequestStatus, OrchestrationV2ServerCommand,
        OrchestrationV2SettleOverride, OrchestrationV2TitleRegeneration,
    },
};

use super::Orchestrator;
use super::dispatch::{Decision, command_id, dispatch_failure};
use super::provider_detach;
use super::thread_mutation::{load_thread, thread_event};
use super::thread_pull_request::{
    legacy_pull_request_key_string, legacy_pull_request_link, pull_request_key_string,
};
use crate::pipeline::effect_outbox::{EffectRequest, PendingEffect, ThreadTitleKind};
use crate::pipeline::orchestrator_port::{
    OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
};
use turin_shared::orchestration_v2_thread_error::{
    latest_root_provider_failure, usage_limit_blocked_run,
};
use crate::projection::shared::thread_pull_requests::thread_pull_requests_of;

/// Decides the metadata kinds ported here. `Ok(false)` leaves the command to
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
        C::ThreadMetadataUpdate {
            thread_id,
            title,
            regenerate_title,
            branch,
            worktree_path,
            expected_worktree_path,
            expected_empty,
            limit_recovery,
            linked_pull_request,
            ..
        } => {
            let thread = load_thread(orchestrator, command, thread_id).await?;
            if let Some(expected) = expected_worktree_path
                && expected.as_ref() != thread.worktree_path.as_ref()
            {
                return Err(dispatch_failure(
                    command,
                    format!(
                        "Thread {thread_id} worktree changed before the metadata update could be applied."
                    ),
                ));
            }
            if *expected_empty == Some(true) {
                let records = orchestrator
                    .get_thread_records(thread_id, &[RecordField::Runs], &RecordFilter::default())
                    .await?;
                let message_count = orchestrator
                    .projection
                    .get_message_count(thread_id)
                    .await
                    .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;
                if message_count > 0 || !records.runs.is_empty() {
                    return Err(dispatch_failure(
                        command,
                        format!("Thread {thread_id} is no longer empty."),
                    ));
                }
            }
            if let Some(Some(update)) = limit_recovery {
                check_limit_recovery(orchestrator, command, &thread, thread_id, update).await?;
            }

            let now = decision.now();
            let previous_recovery = match (limit_recovery, &thread.limit_recovery) {
                (Some(Some(update)), Some(Some(previous)))
                    if previous.run_id == update.run_id && previous.reset_at == update.reset_at =>
                {
                    Some(previous.clone())
                }
                _ => None,
            };
            let next_recovery: Option<Option<OrchestrationV2LimitRecovery>> = match limit_recovery {
                None => None,
                Some(None) => Some(None),
                Some(Some(update)) => Some(Some(OrchestrationV2LimitRecovery {
                    request_id: Some(command_id(command).clone()),
                    run_id: update.run_id.clone(),
                    reset_at: update.reset_at.clone(),
                    auto_resume: update
                        .auto_resume
                        .or(previous_recovery.as_ref().map(|r| r.auto_resume))
                        .unwrap_or(false),
                    snooze: Some(
                        update
                            .snooze
                            .or(previous_recovery.as_ref().and_then(|r| r.snooze))
                            .unwrap_or(false),
                    ),
                })),
            };

            let mut updated = OrchestrationV2AppThread {
                updated_at: now,
                ..thread.clone()
            };
            if let Some(title) = title {
                updated.title = title.clone();
            }
            if let Some(recovery) = &next_recovery {
                updated.limit_recovery = Some(recovery.clone());
            }
            // A recovery that snoozes until its reset wakes the thread then;
            // a recovery that stops snoozing removes the wake it set.
            let snooze_until_reset = next_recovery
                .as_ref()
                .and_then(Option::as_ref)
                .filter(|recovery| recovery.snooze == Some(true))
                .filter(|recovery| {
                    parse_millis(&recovery.reset_at).is_some_and(|ms| ms > millis(now))
                });
            if let Some(recovery) = snooze_until_reset {
                updated.snoozed_until = Some(Some(parse_utc(&recovery.reset_at)));
                updated.snoozed_at = Some(Some(now));
            } else if limit_recovery.is_some() && previous_snooze_reset_wake(&thread) {
                updated.snoozed_until = Some(None);
                updated.snoozed_at = Some(None);
                updated.last_snooze_wake_at = Some(Some(now));
            }
            if let Some(branch) = branch {
                updated.branch = branch.clone();
            }
            if let Some(worktree_path) = worktree_path {
                updated.worktree_path = worktree_path.clone();
            }
            if let Some(linked) = linked_pull_request {
                updated.linked_pull_request = Some(linked.clone());
                updated.pull_requests = Some(relink(&thread, linked.as_ref(), now));
            }
            // `regenerateTitle: true` arms the in-flight marker. A landing title or
            // an explicit false (generation failed or abandoned) clears it.
            if *regenerate_title == Some(true) {
                updated.title_regeneration = Some(Some(OrchestrationV2TitleRegeneration {
                    request_id: command_id(command).clone(),
                    started_at: now,
                }));
            } else if *regenerate_title == Some(false) || title.is_some() {
                updated.title_regeneration = Some(None);
            }
            let workspace_changed = worktree_path
                .as_ref()
                .is_some_and(|path| path.as_ref() != thread.worktree_path.as_ref());

            thread_event!(decision, ThreadMetadataUpdated, updated);
            if *regenerate_title == Some(true) {
                decision.effects.push(title_generation_effect(
                    command_id(command),
                    thread_id,
                    ThreadTitleKind::Regenerate,
                ));
            }
            if workspace_changed {
                let context = orchestrator
                    .projection
                    .get_thread_provider_context(thread_id, None)
                    .await
                    .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;
                provider_detach::detach_sessions(
                    decision,
                    thread_id,
                    &context.provider_sessions,
                    "Workspace changed.",
                    false,
                );
            }
        }
        C::ThreadTitleRegenerationComplete {
            thread_id,
            request_id,
            title,
            ..
        } => {
            let thread = load_thread(orchestrator, command, thread_id).await?;
            let landed = thread
                .title_regeneration
                .as_ref()
                .and_then(Option::as_ref)
                .is_some_and(|marker| marker.request_id == *request_id);
            // A completion for a superseded request changes nothing, but it is
            // still recorded as a metadata update, as upstream does.
            let updated = if landed {
                OrchestrationV2AppThread {
                    title: title.clone().unwrap_or_else(|| thread.title.clone()),
                    title_regeneration: Some(None),
                    updated_at: decision.now(),
                    ..thread
                }
            } else {
                thread
            };
            thread_event!(decision, ThreadMetadataUpdated, updated);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// Upstream's `thread.metadata.update` limit-recovery guard. A choice is only
/// accepted while the thread still shows the usage-limit failure it names.
async fn check_limit_recovery(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    thread: &OrchestrationV2AppThread,
    thread_id: &ThreadId,
    update: &OrchestrationV2LimitRecoveryUpdate,
) -> Result<(), OrchestratorError> {
    let records = orchestrator
        .get_thread_records(
            thread_id,
            &[
                RecordField::Runs,
                RecordField::RuntimeRequests,
                RecordField::TurnItems,
            ],
            &RecordFilter {
                turn_item_types: Some(vec!["error".to_owned()]),
                ..RecordFilter::default()
            },
        )
        .await?;
    let now = millis(UtcDateTime::now());
    let run = usage_limit_blocked_run(&records.runs, &records.turn_items, None);
    let failure = latest_root_provider_failure(run, &records.turn_items);
    let reset_ms = parse_millis(&update.reset_at);
    if update.snooze == Some(true) && reset_ms.is_some_and(|ms| ms <= now) {
        return Err(dispatch_failure(
            command,
            "The reset time has passed. Retry the thread manually.",
        ));
    }
    // A reset must land after the run the limit stopped, or it cannot be the same limit.
    let stopped_at = run.map(|run| millis(run.completed_at.unwrap_or(run.requested_at)));
    let changed = reset_ms.is_none()
        || thread.archived_at.is_some()
        || thread.settled_override == Some(OrchestrationV2SettleOverride::Settled)
        || run.is_none_or(|run| run.id != update.run_id)
        || failure.is_none_or(|failure| {
            failure.class != OrchestrationV2ProviderFailureClass::UsageLimit
                || failure.reset_at.as_ref().and_then(Option::as_ref) != Some(&update.reset_at)
        })
        || reset_ms.is_some_and(|ms| stopped_at.is_some_and(|at| ms <= at))
        || records
            .runtime_requests
            .iter()
            .any(|request| request.status == OrchestrationV2RuntimeRequestStatus::Pending);
    if changed {
        return Err(dispatch_failure(
            command,
            "The provider limit changed before recovery could be configured.",
        ));
    }
    Ok(())
}

/// True when the thread's current limit recovery snoozes until its reset and
/// the thread's wake is still that reset (so clearing the recovery clears the wake).
fn previous_snooze_reset_wake(thread: &OrchestrationV2AppThread) -> bool {
    let Some(Some(previous)) = &thread.limit_recovery else {
        return false;
    };
    if previous.snooze != Some(true) {
        return false;
    }
    let Some(Some(until)) = &thread.snoozed_until else {
        return false;
    };
    parse_millis(&previous.reset_at) == Some(millis(*until))
}

/// The pull-request links a relink leaves behind: every link except the one
/// that matches the old or the new legacy key, then the new link (with its watch).
fn relink(
    thread: &OrchestrationV2AppThread,
    linked: Option<&turin_contracts::thread_pull_request::ThreadLinkedPullRequest>,
    now: UtcDateTime,
) -> Vec<turin_contracts::thread_pull_request::ThreadPullRequestLink> {
    let old_key = thread
        .linked_pull_request
        .as_ref()
        .and_then(Option::as_ref)
        .map(legacy_pull_request_key_string);
    let new_key = linked.map(legacy_pull_request_key_string);
    let mut links: Vec<_> = thread_pull_requests_of(thread)
        .into_iter()
        .filter(|link| {
            let key = pull_request_key_string(link);
            Some(&key) != old_key.as_ref() && Some(&key) != new_key.as_ref()
        })
        .collect();
    if let Some(linked) = linked {
        links.push(legacy_pull_request_link(thread, linked, now));
    }
    links
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

fn parse_millis(iso: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|parsed| parsed.timestamp_millis())
}

fn parse_utc(iso: &str) -> UtcDateTime {
    UtcDateTime::from_datetime(
        DateTime::from_timestamp_millis(parse_millis(iso).unwrap_or_default()).unwrap_or_default(),
    )
}

fn millis(at: UtcDateTime) -> i64 {
    at.as_datetime().timestamp_millis()
}
