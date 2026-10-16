//! Port of the pull-request helpers of upstream `Orchestrator.ts`
//! (`legacyPullRequestLink`, `withPullRequestWatch`) and the pull-request arms
//! of `dispatchThreadMutation` (`thread.pull-request.link`, `unlink`,
//! `thread.pull-request-link.sync`, `thread.pull-request.watch`,
//! `thread.pull-request.sync`) plus `dispatchPullRequestWatchSync`.
//!
//! The watch-sync wake (`dispatchMessage` with `queue_after_active`) is not
//! here: it needs message dispatch (T13c-F). A watch-sync that carries a wake
//! returns `Ok(false)` after its guard, so the command is rejected unported
//! and leaves no receipt.

use turin_contracts::{
    PositiveInt, TrimmedNonEmptyString, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2AppThread, OrchestrationV2Command as C, OrchestrationV2DomainEvent,
        OrchestrationV2InternalCommand as I, OrchestrationV2ServerCommand,
        OrchestrationV2SettleOverride, is_provider_native_subagent_thread,
    },
    thread_pull_request::{
        ThreadLinkedPullRequest, ThreadPullRequestLink, ThreadPullRequestLinkSource,
        ThreadPullRequestWatch,
    },
};

use super::Orchestrator;
use super::dispatch::{Decision, command_id, dispatch_failure};
use super::message_dispatch::dispatch_message;
use super::thread_mutation::{load_live_thread, load_thread, thread_event};
use crate::pipeline::orchestrator_port::{
    OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
};
use crate::projection::shared::thread_pull_requests::{
    NormalizedPullRequestKey, legacy_thread_pull_request_key, normalized_pull_request_key,
    thread_pull_request_key_of, thread_pull_requests_of,
};
use crate::projection::shared::timeline::TimelineTurnItem;
use turin_shared::orchestration_v2_thread_error::latest_executed_run;

/// `legacyThreadPullRequestKey(linked)` as the `host/repository#number`
/// string that [`pull_request_key_string`] produces for a stored link.
pub(super) fn legacy_pull_request_key_string(linked: &ThreadLinkedPullRequest) -> String {
    key_string(&legacy_thread_pull_request_key(linked, None))
}

/// The identity of a stored link, in the same form as [`legacy_pull_request_key_string`].
pub(super) fn pull_request_key_string(link: &ThreadPullRequestLink) -> String {
    thread_pull_request_key_of(link)
}

/// `legacyPullRequestLink`: a single-PR link as a link entry. Re-linking a pull
/// request keeps the watch it already had.
pub(super) fn legacy_pull_request_link(
    thread: &OrchestrationV2AppThread,
    linked: &ThreadLinkedPullRequest,
    now: UtcDateTime,
) -> ThreadPullRequestLink {
    let key_text = legacy_pull_request_key_string(linked);
    let watch = thread_pull_requests_of(thread)
        .into_iter()
        .find(|link| pull_request_key_string(link) == key_text)
        .and_then(|link| link.watch);
    let key = legacy_thread_pull_request_key(linked, None);
    ThreadPullRequestLink {
        host: non_blank(key.host),
        repository: non_blank(key.repository),
        number: PositiveInt::new(key.number).unwrap_or(linked.number),
        url: linked.url.clone(),
        source: ThreadPullRequestLinkSource::Manual,
        linked_at: now.to_string(),
        snapshot: None,
        stack: None,
        watch,
    }
}

/// The pull-request links a relink leaves behind: every link except the one
/// that matches the old or the new legacy key, then the new link (with its watch).
pub(super) fn relink(
    thread: &OrchestrationV2AppThread,
    linked: Option<&ThreadLinkedPullRequest>,
    now: UtcDateTime,
) -> Vec<ThreadPullRequestLink> {
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

/// Decides the pull-request kinds ported here. `Ok(false)` leaves the command
/// to another group (or, for a watch-sync with a wake, rejects it unported).
pub(super) async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    match command {
        OrchestrationV2ServerCommand::Client(client) => match client {
            C::ThreadPullRequestLink {
                thread_id,
                host,
                repository,
                number,
                url,
                source,
                ..
            } => {
                let thread = load_thread(orchestrator, command, thread_id).await?;
                let now = decision.now();
                let key = command_key(host, repository, *number);
                let key_text = key_string(&key);
                let links = thread_pull_requests_of(&thread);
                let existing = links
                    .iter()
                    .position(|link| pull_request_key_string(link) == key_text);
                // Linking a stack-dismissed link again un-dismisses it only for a
                // manual or agent link; a stack member stays dismissed.
                let undismisses = existing.is_some_and(|index| {
                    links[index].source == ThreadPullRequestLinkSource::StackDismissed
                }) && !matches!(
                    source,
                    ThreadPullRequestLinkSource::Stack
                        | ThreadPullRequestLinkSource::StackDismissed
                );
                let mut updated = thread.clone();
                if existing.is_none() || undismisses {
                    let link = match existing {
                        Some(index) => ThreadPullRequestLink {
                            source: *source,
                            url: url.clone(),
                            ..links[index].clone()
                        },
                        None => new_link(&key, *number, url, *source, now),
                    };
                    let mut pulls = Vec::new();
                    // A manual link of the thread's own branch PR rides along the first
                    // link, so the branch's PR is not lost when the user links another.
                    if *source == ThreadPullRequestLinkSource::Manual
                        && let Some(branch) = thread.branch_pull_request.clone().flatten()
                    {
                        let branch_key = legacy_pull_request_key_string(&branch);
                        let visible = links
                            .iter()
                            .filter(|link| {
                                link.source != ThreadPullRequestLinkSource::StackDismissed
                            })
                            .count();
                        if visible == 0
                            && branch_key != key_text
                            && !links
                                .iter()
                                .any(|link| pull_request_key_string(link) == branch_key)
                        {
                            pulls.push(legacy_pull_request_link(&thread, &branch, now));
                        }
                    }
                    pulls.extend(
                        links
                            .iter()
                            .filter(|link| pull_request_key_string(link) != key_text)
                            .cloned(),
                    );
                    pulls.push(link);
                    updated.linked_pull_request = Some(kept_legacy_link(&thread, &pulls));
                    updated.pull_requests = Some(pulls);
                    updated.updated_at = now;
                }
                thread_event!(decision, ThreadPullRequestSynced, updated);
            }
            C::ThreadPullRequestUnlink {
                thread_id,
                host,
                repository,
                number,
                ..
            } => {
                let thread = load_thread(orchestrator, command, thread_id).await?;
                let now = decision.now();
                let key = command_key(host, repository, *number);
                let key_text = key_string(&key);
                let links = thread_pull_requests_of(&thread);
                let mut updated = thread.clone();
                if let Some(existing) = links
                    .iter()
                    .position(|link| pull_request_key_string(link) == key_text)
                {
                    // A stack member is dismissed rather than removed, so the sync
                    // reactor does not add it back from the native stack.
                    let belongs_to_stack = links[existing].source
                        == ThreadPullRequestLinkSource::Stack
                        || links[existing].stack.is_some()
                        || links.iter().any(|link| {
                            link.host.as_str().to_lowercase() == key.host
                                && link.repository.as_str().to_lowercase() == key.repository
                                && link.stack.as_ref().is_some_and(|stack| {
                                    stack
                                        .layers
                                        .iter()
                                        .any(|layer| layer.number.get() == key.number)
                                })
                        });
                    let pulls: Vec<_> = if belongs_to_stack {
                        links
                            .iter()
                            .enumerate()
                            .map(|(index, link)| {
                                if index == existing {
                                    ThreadPullRequestLink {
                                        watch: None,
                                        source: ThreadPullRequestLinkSource::StackDismissed,
                                        ..link.clone()
                                    }
                                } else {
                                    link.clone()
                                }
                            })
                            .collect()
                    } else {
                        links
                            .iter()
                            .enumerate()
                            .filter(|(index, _)| *index != existing)
                            .map(|(_, link)| link.clone())
                            .collect()
                    };
                    updated.linked_pull_request = Some(kept_legacy_link(&thread, &pulls));
                    updated.pull_requests = Some(pulls);
                    updated.updated_at = now;
                }
                thread_event!(decision, ThreadPullRequestSynced, updated);
            }
            C::ThreadPullRequestLinkSync {
                thread_id,
                host,
                repository,
                number,
                snapshot,
                stack,
                ..
            } => {
                let thread = load_thread(orchestrator, command, thread_id).await?;
                let key_text = key_string(&command_key(host, repository, *number));
                let links = thread_pull_requests_of(&thread);
                let Some(existing) = links
                    .iter()
                    .position(|link| pull_request_key_string(link) == key_text)
                else {
                    return Ok(true);
                };
                let pulls: Vec<_> = links
                    .iter()
                    .enumerate()
                    .map(|(index, link)| {
                        if index == existing {
                            ThreadPullRequestLink {
                                snapshot: Some(snapshot.clone()),
                                stack: stack.clone(),
                                ..link.clone()
                            }
                        } else {
                            link.clone()
                        }
                    })
                    .collect();
                let updated = OrchestrationV2AppThread {
                    linked_pull_request: Some(kept_legacy_link(&thread, &pulls)),
                    pull_requests: Some(pulls.clone()),
                    ..thread.clone()
                };
                emit_link_updated(decision, &updated, pulls[existing].clone());
            }
            C::ThreadPullRequestWatch {
                thread_id,
                host,
                repository,
                number,
                watching,
                link,
                ..
            } => {
                let thread = load_thread(orchestrator, command, thread_id).await?;
                if *watching && is_provider_native_subagent_thread(&thread) {
                    return Err(OrchestratorError::SubagentThreadReadOnly {
                        command_id: super::dispatch::command_id(command).clone(),
                        thread_id: thread_id.clone(),
                    });
                }
                if *watching
                    && (thread.settled_override == Some(OrchestrationV2SettleOverride::Settled)
                        || thread.settled_at.is_some()
                        || thread.archived_at.is_some())
                {
                    return Err(dispatch_failure(
                        command,
                        format!(
                            "Thread {thread_id} is settled or archived and cannot watch pull requests."
                        ),
                    ));
                }
                // A subagent or delegated task reports to its parent thread, which owns the
                // pull request. Its own watch would wake it on every change of a PR it was
                // not asked to babysit.
                if *watching
                    && thread.lineage.relationship_to_parent
                        == Some(
                            turin_contracts::orchestration_v2::OrchestrationV2LineageRelationship::Subagent,
                        )
                {
                    return Err(dispatch_failure(
                        command,
                        format!(
                            "Thread {thread_id} is a subagent; its parent thread watches pull requests."
                        ),
                    ));
                }
                // Only the agent's watch_pull_request links as "agent". A call that raced a Stop
                // may land after its run ended, so the latest run that executed decides.
                if *watching
                    && link
                        .as_ref()
                        .is_some_and(|link| link.source == ThreadPullRequestLinkSource::Agent)
                {
                    let records = orchestrator
                        .get_thread_records(
                            thread_id,
                            &[RecordField::Runs],
                            &RecordFilter::default(),
                        )
                        .await?;
                    if let Some(latest) = latest_executed_run(&records.runs)
                        && stop_reached_run(orchestrator, thread_id, &latest.id).await?
                    {
                        return Err(dispatch_failure(
                            command,
                            format!("Thread {thread_id} was stopped."),
                        ));
                    }
                }

                let now = decision.now();
                let started_at = now.to_string();
                let key = command_key(host, repository, *number);
                let key_text = key_string(&key);
                let linked = thread_pull_requests_of(&thread);
                let is_visible = |link: &ThreadPullRequestLink| {
                    link.source != ThreadPullRequestLinkSource::StackDismissed
                        && pull_request_key_string(link) == key_text
                };
                // A watch started on an unlinked (or dismissed) pull request links it in the same step.
                let (links, added) = match link {
                    Some(link) if *watching && !linked.iter().any(is_visible) => {
                        let mut links: Vec<_> = linked
                            .iter()
                            .filter(|entry| pull_request_key_string(entry) != key_text)
                            .cloned()
                            .collect();
                        links.push(new_link(&key, *number, &link.url, link.source, now));
                        (links, true)
                    }
                    _ => (linked.clone(), false),
                };
                let Some(existing) = links.iter().position(is_visible) else {
                    return Ok(unchanged_thread_event(decision, thread));
                };
                let watch = if *watching {
                    Some(
                        links[existing]
                            .watch
                            .clone()
                            .unwrap_or_else(|| new_watch(&started_at)),
                    )
                } else {
                    None
                };
                if watch == links[existing].watch && !added {
                    return Ok(unchanged_thread_event(decision, thread));
                }
                let pulls: Vec<_> = links
                    .iter()
                    .enumerate()
                    .map(|(index, entry)| {
                        if index == existing {
                            ThreadPullRequestLink {
                                watch: watch.clone(),
                                ..entry.clone()
                            }
                        } else {
                            entry.clone()
                        }
                    })
                    .collect();
                let updated = OrchestrationV2AppThread {
                    pull_requests: Some(pulls),
                    // A user or agent starting or stopping a watch is activity.
                    updated_at: now,
                    ..thread
                };
                thread_event!(decision, ThreadPullRequestSynced, updated);
            }
            C::ThreadPullRequestSync {
                thread_id,
                project_id,
                snapshot_sequence,
                expected,
                branch_pull_request,
                linked_pull_request,
                ..
            } => {
                let thread = load_live_thread(orchestrator, command, thread_id).await?;
                let project_root = orchestrator
                    .seams
                    .project_roots
                    .workspace_root(project_id)
                    .await
                    .map_err(|cause| dispatch_failure(command, cause))?;
                let current_sequence = orchestrator
                    .sink
                    .latest_sequence(Some(thread_id.clone()))
                    .await
                    .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;
                let current_sequence = u64::try_from(current_sequence).unwrap_or(0);
                let changed = project_root
                    .as_ref()
                    .is_none_or(|root| root != expected.workspace_root.as_str())
                    || current_sequence > *snapshot_sequence
                    || thread.project_id != *project_id
                    || thread.branch != expected.branch
                    || thread.worktree_path != expected.worktree_path
                    || thread.linked_pull_request.clone().flatten() != expected.linked_pull_request
                    || thread.branch_pull_request.clone().flatten() != expected.branch_pull_request;
                if changed {
                    return Err(dispatch_failure(
                        command,
                        format!("Thread {thread_id} changed before pull request discovery."),
                    ));
                }

                let now = decision.now();
                let mut updated = OrchestrationV2AppThread {
                    branch_pull_request: Some(branch_pull_request.clone()),
                    ..thread.clone()
                };
                if let Some(linked) = linked_pull_request {
                    updated.linked_pull_request = Some(Some(linked.clone()));
                    updated.pull_requests = Some(relink(&thread, Some(linked), now));
                }
                thread_event!(decision, ThreadPullRequestSynced, updated);
            }
            _ => return Ok(false),
        },
        OrchestrationV2ServerCommand::Internal(internal) => match internal {
            I::ThreadPullRequestWatchSync {
                thread_id,
                host,
                repository,
                number,
                started_at,
                watch,
                wake,
                ..
            } => {
                // Read without the deleted check: upstream reads the projection
                // directly here and checks deletion in the mutation below.
                let thread = orchestrator
                    .projection
                    .get_thread(thread_id)
                    .await
                    .map_err(|cause| OrchestratorError::projection(thread_id.clone(), cause))?;
                let key_text = key_string(&command_key(host, repository, *number));
                let links = thread_pull_requests_of(&thread);
                let existing = links.iter().position(|link| {
                    link.source != ThreadPullRequestLinkSource::StackDismissed
                        && pull_request_key_string(link) == key_text
                });
                // Same rule as a direct message.dispatch: a provider-native subagent takes no messages.
                let inactive = thread.archived_at.is_some()
                    || thread.settled_override == Some(OrchestrationV2SettleOverride::Settled)
                    || thread.settled_at.is_some()
                    || is_provider_native_subagent_thread(&thread);
                let watch_started_at = existing
                    .and_then(|index| links[index].watch.as_ref())
                    .map(|watch| watch.started_at.as_str());
                if watch_started_at != Some(started_at.as_str()) || (wake.is_some() && inactive) {
                    return Err(dispatch_failure(
                        command,
                        "The pull request watch ended or its thread settled while it was read.",
                    ));
                }
                'mutate: {
                    let thread = load_thread(orchestrator, command, thread_id).await?;
                    let Some(existing) = existing else {
                        break 'mutate;
                    };
                    // Progress read before a stop or restart must not bring the old watch back.
                    let next_watch = if links[existing]
                        .watch
                        .as_ref()
                        .map(|w| w.started_at.as_str())
                        == Some(started_at.as_str())
                    {
                        watch.clone()
                    } else {
                        links[existing].watch.clone()
                    };
                    if next_watch == links[existing].watch {
                        break 'mutate;
                    }
                    let pulls: Vec<_> = links
                        .iter()
                        .enumerate()
                        .map(|(index, link)| {
                            if index == existing {
                                ThreadPullRequestLink {
                                    watch: next_watch.clone(),
                                    ..link.clone()
                                }
                            } else {
                                link.clone()
                            }
                        })
                        .collect();
                    let updated = OrchestrationV2AppThread {
                        pull_requests: Some(pulls.clone()),
                        ..thread
                    };
                    emit_link_updated(decision, &updated, pulls[existing].clone());
                }
                if let Some(wake) = wake {
                    // The wake is a queued message from the server's agent.
                    let message: C = serde_json::from_value(serde_json::json!({
                        "type": "message.dispatch",
                        "commandId": command_id(command),
                        "threadId": thread_id,
                        "messageId": wake.message_id,
                        "text": wake.text,
                        "notification": wake.notification,
                        "attachments": [],
                        "dispatchMode": {"type": "queue_after_active"},
                        "createdBy": "agent",
                        "creationSource": "server",
                    }))
                    .map_err(|cause| dispatch_failure(command, cause.to_string()))?;
                    let message_command = OrchestrationV2ServerCommand::Client(message.clone());
                    dispatch_message(orchestrator, &message_command, &message, decision).await?;
                }
            }
            _ => return Ok(false),
        },
    }
    Ok(true)
}

/// Upstream's `thread.pull-request.watch` returns the thread unchanged for a
/// no-op, and the generic emit still records it as `thread.pull-request-synced`.
fn unchanged_thread_event(decision: &mut Decision, thread: OrchestrationV2AppThread) -> bool {
    thread_event!(decision, ThreadPullRequestSynced, thread);
    true
}

/// `thread.pull-request-link-updated`, with the link as its payload.
fn emit_link_updated(
    decision: &mut Decision,
    thread: &OrchestrationV2AppThread,
    link: ThreadPullRequestLink,
) {
    let id = decision.event_id(&thread.id);
    let occurred_at = decision.now();
    decision.emit(OrchestrationV2DomainEvent::ThreadPullRequestLinkUpdated {
        id,
        thread_id: thread.id.clone(),
        run_id: None,
        node_id: None,
        driver: None,
        provider_instance_id: Some(thread.provider_instance_id.clone()),
        raw_event_id: None,
        occurred_at,
        payload: link,
    });
}

/// `linkedPullRequest: thread.linkedPullRequest && pullRequests.some(...) ? thread.linkedPullRequest : null`.
fn kept_legacy_link(
    thread: &OrchestrationV2AppThread,
    pulls: &[ThreadPullRequestLink],
) -> Option<ThreadLinkedPullRequest> {
    thread
        .linked_pull_request
        .clone()
        .flatten()
        .filter(|linked| {
            let linked_key = legacy_pull_request_key_string(linked);
            pulls.iter().any(|link| {
                link.source != ThreadPullRequestLinkSource::StackDismissed
                    && pull_request_key_string(link) == linked_key
            })
        })
}

/// `threadPullRequestKeysEqual` side for a command: the normalized key of its
/// `host`/`repository`/`number`, with no URL to recover from.
fn command_key(
    host: &TrimmedNonEmptyString,
    repository: &TrimmedNonEmptyString,
    number: PositiveInt,
) -> NormalizedPullRequestKey {
    normalized_pull_request_key(host.as_str(), repository.as_str(), number.get())
}

/// `host/repository#number`, the form [`pull_request_key_string`] produces.
fn key_string(key: &NormalizedPullRequestKey) -> String {
    format!("{}/{}#{}", key.host, key.repository, key.number)
}

/// A new link for a command's key, with no snapshot, stack or watch.
fn new_link(
    key: &NormalizedPullRequestKey,
    number: PositiveInt,
    url: &TrimmedNonEmptyString,
    source: ThreadPullRequestLinkSource,
    now: UtcDateTime,
) -> ThreadPullRequestLink {
    ThreadPullRequestLink {
        host: non_blank(key.host.clone()),
        repository: non_blank(key.repository.clone()),
        number,
        url: url.clone(),
        source,
        linked_at: now.to_string(),
        snapshot: None,
        stack: None,
        watch: None,
    }
}

/// A watch started now, with nothing reported to the agent yet.
fn new_watch(started_at: &str) -> ThreadPullRequestWatch {
    serde_json::from_value(serde_json::json!({
        "startedAt": started_at,
        "headSha": null,
        "failedChecks": [],
        "passed": false,
        "passedChecks": [],
        "remarksThrough": started_at,
        "remarkIds": [],
        "conflicting": false,
        "wakes": 0,
    }))
    .expect("a watch of literal fields decodes")
}

/// Whether Stop reached a run, as its interrupt request records. The agent can
/// call tools until the provider stops it, so a call that lands after Stop must
/// not start a watch.
async fn stop_reached_run(
    orchestrator: &Orchestrator,
    thread_id: &turin_contracts::ThreadId,
    run_id: &turin_contracts::RunId,
) -> Result<bool, OrchestratorError> {
    let records = orchestrator
        .get_thread_records(
            thread_id,
            &[RecordField::TurnItems],
            &RecordFilter {
                turn_item_types: Some(vec!["run_interrupt_request".to_owned()]),
                ..RecordFilter::default()
            },
        )
        .await?;
    Ok(records
        .turn_items
        .iter()
        .any(|item| item.run_id() == Some(run_id)))
}

fn non_blank(value: String) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(&value)
        .unwrap_or_else(|_| TrimmedNonEmptyString::new("unknown").expect("literal is non-empty"))
}
