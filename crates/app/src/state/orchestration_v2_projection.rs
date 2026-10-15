//! Port of upstream/packages/client-runtime/src/state/orchestrationV2Projection.ts
//!
//! Applies one committed domain event to a thread projection. Pure: returns
//! the next projection (sharing every collection the event did not touch) or
//! `None` when the event changes nothing (upstream returns the same object).

use std::cmp::Ordering;
use std::sync::Arc;

use crate::state::contracts::{
    NonNegativeInt, OrchestrationV2DomainEvent, OrchestrationV2ProjectedTurnItemVisibility,
    OrchestrationV2Run, OrchestrationV2RunStatus, OrchestrationV2TurnItem,
};
use crate::state::support::collate::locale_compare;
use crate::state::support::domain_event::DomainEventExt;
use crate::state::support::thread_pull_requests::apply_thread_pull_request_link_update;
use crate::state::support::timeline_visibility::{
    create_orchestration_v2_turn_item_visibility, is_orchestration_v2_turn_item_visible,
};
use crate::state::support::turn_item::{TurnItemExt, TurnItemType};
use crate::state::thread_projection::{
    Rows, ThreadProjection, VisibleTurnItem, VisibleTurnItemRow,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApplyOrchestrationV2ProjectionEventOptions {
    pub partial_timeline: bool,
    pub latest_local_turn_ordinal: Option<NonNegativeInt>,
}

fn upsert_entity<T, K: PartialEq>(
    items: &Rows<T>,
    item: Arc<T>,
    key: impl Fn(&T) -> &K,
) -> Rows<T> {
    // Streaming updates hit the newest rows, so search from the end.
    let index = items
        .iter()
        .rposition(|candidate| key(candidate) == key(&item));
    let mut next = Vec::with_capacity(items.len() + usize::from(index.is_none()));
    next.extend(items.iter().cloned());
    match index {
        None => next.push(item),
        Some(index) => next[index] = item,
    }
    Arc::new(next)
}

fn renumber_visible_items(rows: Vec<VisibleTurnItemRow>) -> Arc<Vec<VisibleTurnItemRow>> {
    Arc::new(
        rows.into_iter()
            .enumerate()
            .map(|(position, row)| {
                if row.position == position as u64 {
                    row
                } else {
                    Arc::new(VisibleTurnItem {
                        position: position as u64,
                        ..(*row).clone()
                    })
                }
            })
            .collect(),
    )
}

fn remove_visible_item(
    rows: &Arc<Vec<VisibleTurnItemRow>>,
    source_item_id: &crate::state::contracts::TurnItemId,
) -> Arc<Vec<VisibleTurnItemRow>> {
    let Some(index) = rows
        .iter()
        .position(|row| row.source_item_id == *source_item_id)
    else {
        return Arc::clone(rows);
    };
    let mut next: Vec<VisibleTurnItemRow> = rows.to_vec();
    next.remove(index);
    renumber_visible_items(next)
}

fn should_show_local_turn_item(
    projection: &ThreadProjection,
    item: &OrchestrationV2TurnItem,
) -> bool {
    is_orchestration_v2_turn_item_visible(
        item,
        &projection.runs,
        &projection.attempts,
        &projection.turn_items,
    )
}

fn active_visible_turn_items(projection: &ThreadProjection) -> Arc<Vec<VisibleTurnItemRow>> {
    let rows = &projection.visible_turn_items;
    let visibility = create_orchestration_v2_turn_item_visibility(
        &projection.runs,
        &projection.attempts,
        &projection.turn_items,
    );
    let mut next: Option<Vec<VisibleTurnItemRow>> = None;
    for (index, row) in rows.iter().enumerate() {
        let keep = row.visibility != OrchestrationV2ProjectedTurnItemVisibility::Local
            || visibility.is_visible(&row.item);
        if !keep {
            next.get_or_insert_with(|| rows[..index].to_vec());
            continue;
        }
        if let Some(next) = next.as_mut() {
            next.push(Arc::clone(row));
        }
    }
    match next {
        None => Arc::clone(rows),
        Some(next) => renumber_visible_items(next),
    }
}

fn oldest_local_turn_ordinal(rows: &[VisibleTurnItemRow]) -> Option<u64> {
    rows.iter()
        .filter(|row| row.visibility == OrchestrationV2ProjectedTurnItemVisibility::Local)
        .map(|row| row.item.ordinal())
        .min()
}

fn is_unfinished_run(run: &OrchestrationV2Run) -> bool {
    matches!(
        run.status,
        OrchestrationV2RunStatus::Preparing
            | OrchestrationV2RunStatus::Queued
            | OrchestrationV2RunStatus::Starting
            | OrchestrationV2RunStatus::Running
            | OrchestrationV2RunStatus::Waiting
    )
}

fn should_drop_missing_partial_turn_item(
    projection: &ThreadProjection,
    item: &OrchestrationV2TurnItem,
    latest_local_turn_ordinal: Option<u64>,
) -> bool {
    if projection
        .visible_turn_items
        .iter()
        .any(|row| row.source_item_id == *item.id())
    {
        return false;
    }
    let Some(run_id) = item.run_id() else {
        return is_below_partial_window(projection, item, latest_local_turn_ordinal);
    };
    // A queued run keeps the ordinal it got when queued, so a steer sent later
    // can raise the watermark past it. Items of a run that has not finished are
    // new, not unloaded history: the first lands while the run is still queued,
    // and a bounded window can start after the ones before it.
    if projection
        .runs
        .iter()
        .any(|run| run.id == *run_id && is_unfinished_run(run))
    {
        return false;
    }
    // Later items of a run already in the window can raise the watermark first.
    if projection.visible_turn_items.iter().any(|row| {
        row.visibility == OrchestrationV2ProjectedTurnItemVisibility::Local
            && row.item.run_id() == Some(run_id)
            && row.item.ordinal() <= item.ordinal()
    }) {
        return false;
    }
    is_below_partial_window(projection, item, latest_local_turn_ordinal)
}

fn is_below_partial_window(
    projection: &ThreadProjection,
    item: &OrchestrationV2TurnItem,
    latest_local_turn_ordinal: Option<u64>,
) -> bool {
    if let Some(latest) = latest_local_turn_ordinal
        && item.ordinal() <= latest
    {
        return true;
    }
    oldest_local_turn_ordinal(&projection.visible_turn_items)
        .is_some_and(|oldest| item.ordinal() < oldest)
}

fn upsert_visible_turn_item(
    projection: &ThreadProjection,
    item: &Arc<OrchestrationV2TurnItem>,
    partial_timeline: bool,
    latest_local_turn_ordinal: Option<u64>,
) -> Arc<Vec<VisibleTurnItemRow>> {
    let rows = &projection.visible_turn_items;
    // Streaming updates hit the newest rows, so search from the end.
    let index = rows
        .iter()
        .rposition(|row| row.source_item_id == *item.id());
    let next = VisibleTurnItem {
        position: 0,
        visibility: OrchestrationV2ProjectedTurnItemVisibility::Local,
        source_thread_id: item.thread_id().clone(),
        source_item_id: item.id().clone(),
        item: Arc::clone(item),
    };
    if let Some(previous) = index.map(|index| &rows[index])
        && previous.visibility == next.visibility
        && previous.source_thread_id == next.source_thread_id
        && previous.source_item_id == next.source_item_id
        && Arc::ptr_eq(&previous.item, item)
    {
        return Arc::clone(rows);
    }
    if index.is_none()
        && partial_timeline
        && should_drop_missing_partial_turn_item(projection, item, latest_local_turn_ordinal)
    {
        return Arc::clone(rows);
    }
    let mut updated: Vec<VisibleTurnItemRow> = Vec::with_capacity(rows.len() + 1);
    updated.extend(
        rows.iter()
            .enumerate()
            .filter(|(position, _)| Some(*position) != index)
            .map(|(_, row)| Arc::clone(row)),
    );
    let insertion_index = updated.iter().position(|row| {
        row.visibility == OrchestrationV2ProjectedTurnItemVisibility::Local
            && (row.item.ordinal() > item.ordinal()
                || (row.item.ordinal() == item.ordinal()
                    && locale_compare(row.item.id().as_str(), item.id().as_str())
                        == Ordering::Greater))
    });
    updated.insert(insertion_index.unwrap_or(updated.len()), Arc::new(next));
    renumber_visible_items(updated)
}

/// Applies one committed event to a matching thread projection.
pub fn apply_orchestration_v2_projection_event(
    projection: &ThreadProjection,
    event: OrchestrationV2DomainEvent,
    options: Option<ApplyOrchestrationV2ProjectionEventOptions>,
) -> Option<ThreadProjection> {
    if *event.thread_id() != projection.thread.id {
        return None;
    }
    let options = options.unwrap_or_default();
    let partial_timeline = options.partial_timeline;
    let latest_local_turn_ordinal = options.latest_local_turn_ordinal;
    let occurred_at = *event.occurred_at();
    let base = || ThreadProjection {
        updated_at: occurred_at,
        ..projection.clone()
    };
    use OrchestrationV2DomainEvent as E;
    Some(match event {
        E::ThreadPullRequestLinkUpdated { payload, .. } => ThreadProjection {
            thread: Arc::new(apply_thread_pull_request_link_update(
                &projection.thread,
                &payload,
            )),
            ..base()
        },
        E::ThreadCreated { payload, .. }
        | E::ThreadArchived { payload, .. }
        | E::ThreadUnarchived { payload, .. }
        | E::ThreadDeleted { payload, .. }
        | E::ThreadSettled { payload, .. }
        | E::ThreadUnsettled { payload, .. }
        | E::ThreadSnoozed { payload, .. }
        | E::ThreadUnsnoozed { payload, .. }
        | E::ThreadAutoSettleSet { payload, .. }
        | E::ThreadPinned { payload, .. }
        | E::ThreadUnpinned { payload, .. }
        | E::ThreadPinReordered { payload, .. }
        | E::ThreadActiveReordered { payload, .. }
        | E::ThreadMetadataUpdated { payload, .. }
        | E::ThreadPullRequestSynced { payload, .. }
        | E::ThreadRuntimeModeUpdated { payload, .. }
        | E::ThreadInteractionModeUpdated { payload, .. }
        | E::ThreadModelSelectionUpdated { payload, .. }
        | E::ThreadProviderSwitched { payload, .. } => ThreadProjection {
            thread: Arc::new(payload),
            ..base()
        },
        // Visited tracking is read state, not activity: skip the updatedAt bump.
        E::ThreadVisited { payload, .. } | E::ThreadMarkedUnread { payload, .. } => {
            ThreadProjection {
                thread: Arc::new(payload),
                ..projection.clone()
            }
        }
        E::RunCreated { payload, .. } | E::RunUpdated { payload, .. } => {
            let mut next = base();
            next.runs = upsert_entity(&next.runs, Arc::new(payload), |run| &run.id);
            next.visible_turn_items = active_visible_turn_items(&next);
            next
        }
        E::RunBackgroundWorkCancelled { payload, .. } => {
            let mut next = base();
            next.runs = Arc::new(
                next.runs
                    .iter()
                    .map(|run| {
                        if run.id == payload.run_id {
                            Arc::new(OrchestrationV2Run {
                                restart_cancelled_background_work: Some(
                                    payload.restart_cancelled_background_work.clone(),
                                ),
                                ..(**run).clone()
                            })
                        } else {
                            Arc::clone(run)
                        }
                    })
                    .collect(),
            );
            next
        }
        E::RunAttemptCreated { payload, .. } | E::RunAttemptUpdated { payload, .. } => {
            let mut next = base();
            next.attempts = upsert_entity(&next.attempts, Arc::new(payload), |attempt| &attempt.id);
            next.visible_turn_items = active_visible_turn_items(&next);
            next
        }
        E::NodeUpdated { payload, .. } => {
            let mut next = base();
            next.nodes = upsert_entity(&next.nodes, Arc::new(payload), |node| &node.id);
            next
        }
        E::SubagentUpdated { payload, .. } => {
            let mut next = base();
            next.subagents = upsert_entity(&next.subagents, Arc::new(payload), |s| &s.id);
            next
        }
        E::ProviderSessionAttached { payload, .. } | E::ProviderSessionUpdated { payload, .. } => {
            let mut next = base();
            next.provider_sessions =
                upsert_entity(&next.provider_sessions, Arc::new(payload), |s| &s.id);
            next
        }
        E::ProviderSessionDetached { payload, .. } => {
            let mut next = base();
            next.provider_sessions = Arc::new(
                next.provider_sessions
                    .iter()
                    .filter(|session| session.id != payload.provider_session_id)
                    .cloned()
                    .collect(),
            );
            next
        }
        E::ProviderThreadUpdated { payload, .. } => {
            let mut next = base();
            next.provider_threads =
                upsert_entity(&next.provider_threads, Arc::new(payload), |t| &t.id);
            next
        }
        E::ProviderTurnUpdated { mut payload, .. } => {
            let mut next = base();
            if payload.token_usage.is_none() {
                payload.token_usage = next
                    .provider_turns
                    .iter()
                    .find(|turn| turn.id == payload.id)
                    .and_then(|turn| turn.token_usage.clone());
            }
            next.provider_turns = upsert_entity(&next.provider_turns, Arc::new(payload), |t| &t.id);
            next
        }
        E::RuntimeRequestUpdated { payload, .. } => {
            let mut next = base();
            next.runtime_requests =
                upsert_entity(&next.runtime_requests, Arc::new(payload), |r| &r.id);
            next
        }
        E::MessageUpdated { payload, .. } => {
            let mut next = base();
            next.messages = upsert_entity(&next.messages, Arc::new(payload), |m| &m.id);
            next
        }
        E::PlanUpdated { payload, .. } => {
            let mut next = base();
            next.plans = upsert_entity(&next.plans, Arc::new(payload), |plan| plan_id(plan));
            next
        }
        E::TurnItemUpdated { payload, .. } => {
            let item = Arc::new(payload);
            if partial_timeline
                && !projection
                    .turn_items
                    .iter()
                    .any(|candidate| candidate.id() == item.id())
                && should_drop_missing_partial_turn_item(
                    projection,
                    &item,
                    latest_local_turn_ordinal,
                )
            {
                return None;
            }
            let mut next = base();
            next.turn_items = upsert_entity(&next.turn_items, Arc::clone(&item), |i| i.id());
            // Only interrupt requests can change another item's visibility. Streaming
            // text/tool updates must not recheck every row against every run.
            let previous_was_interrupt_request = projection
                .turn_items
                .iter()
                .rfind(|candidate| candidate.id() == item.id())
                .is_some_and(|previous| previous.item_type() == TurnItemType::RunInterruptRequest);
            let visible = if item.item_type() == TurnItemType::RunInterruptRequest
                || previous_was_interrupt_request
            {
                ThreadProjection {
                    visible_turn_items: active_visible_turn_items(&next),
                    ..next.clone()
                }
            } else {
                next.clone()
            };
            next.visible_turn_items = if should_show_local_turn_item(&next, &item) {
                upsert_visible_turn_item(
                    &visible,
                    &item,
                    partial_timeline,
                    latest_local_turn_ordinal,
                )
            } else {
                remove_visible_item(&visible.visible_turn_items, item.id())
            };
            next
        }
        E::CheckpointScopeCreated { payload, .. } => {
            let mut next = base();
            next.checkpoint_scopes =
                upsert_entity(&next.checkpoint_scopes, Arc::new(payload), |s| &s.id);
            next
        }
        E::CheckpointCaptured { payload, .. } => {
            let mut next = base();
            next.checkpoints = upsert_entity(&next.checkpoints, Arc::new(payload), |c| &c.id);
            next
        }
        E::CheckpointRollbackRequested { .. } => base(),
        E::ContextHandoffUpdated { payload, .. } => {
            let mut next = base();
            next.context_handoffs =
                upsert_entity(&next.context_handoffs, Arc::new(payload), |h| &h.id);
            next
        }
        E::ContextTransferCreated { payload, .. } | E::ContextTransferUpdated { payload, .. } => {
            let mut next = base();
            next.context_transfers =
                upsert_entity(&next.context_transfers, Arc::new(payload), |t| &t.id);
            next
        }
    })
}

fn plan_id(
    plan: &crate::state::contracts::OrchestrationV2PlanArtifact,
) -> &crate::state::contracts::PlanId {
    match plan {
        crate::state::contracts::OrchestrationV2PlanArtifact::ProposedPlan { id, .. }
        | crate::state::contracts::OrchestrationV2PlanArtifact::TodoList { id, .. } => id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::{
        OrchestrationV2ThreadProjection, RunId, ThreadId, ThreadPullRequestLink, UtcDateTime,
    };
    use crate::state::test_fixtures::v2_projection;
    use serde_json::json;

    const NOW: &str = "2026-06-20T00:00:00.000Z";

    fn now() -> UtcDateTime {
        NOW.parse().unwrap()
    }

    fn thread_id() -> ThreadId {
        ThreadId::new("thread-v2").unwrap()
    }

    fn run_id() -> RunId {
        RunId::new("run-reducer").unwrap()
    }

    fn run(id: &str, ordinal: u64, status: &str) -> OrchestrationV2Run {
        serde_json::from_value(json!({
            "id": id,
            "threadId": "thread-v2",
            "ordinal": ordinal,
            "providerInstanceId": "codex",
            "modelSelection": { "instanceId": "codex", "model": "gpt-5.4" },
            "providerThreadId": null,
            "userMessageId": "message-reducer",
            "rootNodeId": null,
            "activeAttemptId": null,
            "status": status,
            "requestedAt": NOW,
            "startedAt": NOW,
            "completedAt": NOW,
            "checkpointId": null,
            "contextHandoffId": null,
        }))
        .unwrap()
    }

    fn command_item(id: &str, output: &str, ordinal: u64) -> OrchestrationV2TurnItem {
        run_item(id, "run-reducer", output, ordinal)
    }

    fn run_item(id: &str, run_id: &str, output: &str, ordinal: u64) -> OrchestrationV2TurnItem {
        serde_json::from_value(json!({
            "id": id,
            "threadId": "thread-v2",
            "runId": run_id,
            "nodeId": null,
            "providerThreadId": null,
            "providerTurnId": null,
            "nativeItemRef": null,
            "parentItemId": null,
            "ordinal": ordinal,
            "status": "completed",
            "title": null,
            "startedAt": NOW,
            "completedAt": NOW,
            "updatedAt": NOW,
            "type": "command_execution",
            "input": "pwd",
            "output": output,
            "exitCode": 0,
        }))
        .unwrap()
    }

    fn row(position: u64, item: &Arc<OrchestrationV2TurnItem>, local: bool) -> VisibleTurnItemRow {
        Arc::new(VisibleTurnItem {
            position,
            visibility: if local {
                OrchestrationV2ProjectedTurnItemVisibility::Local
            } else {
                OrchestrationV2ProjectedTurnItemVisibility::Inherited
            },
            source_thread_id: if local {
                thread_id()
            } else {
                ThreadId::new("thread-source").unwrap()
            },
            source_item_id: item.id().clone(),
            item: Arc::clone(item),
        })
    }

    fn empty() -> ThreadProjection {
        v2_projection().into()
    }

    fn event(value: serde_json::Value) -> OrchestrationV2DomainEvent {
        serde_json::from_value(value).unwrap()
    }

    fn base_event(kind: &str, payload: serde_json::Value) -> OrchestrationV2DomainEvent {
        event(json!({
            "id": format!("event-{kind}"),
            "type": kind,
            "threadId": "thread-v2",
            "occurredAt": NOW,
            "payload": payload,
        }))
    }

    fn to_json<T: serde::Serialize>(value: &T) -> serde_json::Value {
        serde_json::to_value(value).unwrap()
    }

    fn apply(projection: &ThreadProjection, event: OrchestrationV2DomainEvent) -> ThreadProjection {
        apply_orchestration_v2_projection_event(projection, event, None)
            .unwrap_or_else(|| projection.clone())
    }

    fn with_items(
        projection: ThreadProjection,
        runs: Vec<OrchestrationV2Run>,
        items: &[Arc<OrchestrationV2TurnItem>],
    ) -> ThreadProjection {
        ThreadProjection {
            runs: Arc::new(runs.into_iter().map(Arc::new).collect()),
            turn_items: Arc::new(items.to_vec()),
            visible_turn_items: Arc::new(
                items
                    .iter()
                    .enumerate()
                    .map(|(position, item)| row(position as u64, item, true))
                    .collect(),
            ),
            ..projection
        }
    }

    fn visible_ids(projection: &ThreadProjection) -> Vec<String> {
        projection
            .visible_turn_items
            .iter()
            .map(|row| row.item.id().to_string())
            .collect()
    }

    #[test]
    fn applies_a_compact_pr_sync_without_replacing_other_links_or_thread_metadata() {
        let link = |number: u64| -> ThreadPullRequestLink {
            serde_json::from_value(json!({
                "host": "github.com",
                "repository": "pingdotgg/t3code",
                "number": number,
                "url": format!("https://github.com/pingdotgg/t3code/pull/{number}"),
                "source": "manual",
                "linkedAt": NOW,
                "snapshot": null,
                "stack": null,
            }))
            .unwrap()
        };
        let links = vec![link(1), link(2)];
        let mut projection = empty();
        projection.thread = Arc::new(crate::state::contracts::OrchestrationV2AppThread {
            pull_requests: Some(links.clone()),
            ..(*projection.thread).clone()
        });
        let mut updated = links[1].clone();
        updated.source = crate::state::contracts::ThreadPullRequestLinkSource::Agent;
        let next = apply(
            &projection,
            base_event("thread.pull-request-link-updated", to_json(&updated)),
        );
        assert_eq!(
            *next.thread,
            crate::state::contracts::OrchestrationV2AppThread {
                pull_requests: Some(vec![links[0].clone(), updated]),
                ..(*projection.thread).clone()
            }
        );
    }

    #[test]
    fn keeps_live_token_usage_when_the_terminal_provider_turn_omits_it() {
        let running = json!({
            "id": "provider-turn-reducer",
            "providerThreadId": "provider-thread-reducer",
            "nodeId": "provider-node-reducer",
            "runAttemptId": null,
            "nativeTurnRef": null,
            "ordinal": 1,
            "status": "running",
            "startedAt": NOW,
            "completedAt": null,
            "tokenUsage": { "usedTokens": 50000, "maxTokens": 200000, "updatedAt": "2026-08-29T00:00:00.000Z" },
        });
        let mut projection = empty();
        projection.provider_turns = Arc::new(vec![Arc::new(
            serde_json::from_value(running.clone()).unwrap(),
        )]);
        let terminal = {
            let mut value = running.clone();
            value["status"] = json!("completed");
            value["completedAt"] = json!(NOW);
            value.as_object_mut().unwrap().remove("tokenUsage");
            value
        };
        let event = base_event("provider-turn.updated", terminal);
        let next = apply(&projection, event);
        assert_eq!(
            next.provider_turns[0].status,
            crate::state::contracts::OrchestrationV2ProviderTurnStatus::Completed
        );
        assert_eq!(
            next.provider_turns[0].token_usage,
            projection.provider_turns[0].token_usage
        );
    }

    #[test]
    fn applies_thread_lifecycle_payloads_instead_of_leaving_stale_metadata() {
        let archived_at: UtcDateTime = "2026-06-20T01:00:00.000Z".parse().unwrap();
        let payload = crate::state::contracts::OrchestrationV2AppThread {
            archived_at: Some(archived_at),
            updated_at: archived_at,
            ..(*empty().thread).clone()
        };
        let event = event(json!({
            "id": "event-archive",
            "type": "thread.archived",
            "threadId": "thread-v2",
            "occurredAt": "2026-06-20T01:00:00.000Z",
            "payload": to_json(&payload),
        }));
        let next = apply(&empty(), event);
        assert_eq!(next.thread.archived_at, Some(archived_at));
        assert_eq!(next.updated_at, archived_at);
    }

    #[test]
    fn ignores_events_for_another_thread() {
        let payload = crate::state::contracts::OrchestrationV2AppThread {
            id: ThreadId::new("thread-other").unwrap(),
            deleted_at: Some(now()),
            ..(*empty().thread).clone()
        };
        let event = event(json!({
            "id": "event-other",
            "type": "thread.deleted",
            "threadId": "thread-other",
            "occurredAt": NOW,
            "payload": to_json(&payload),
        }));
        assert_eq!(
            apply_orchestration_v2_projection_event(&empty(), event, None),
            None
        );
    }

    #[test]
    fn preserves_visible_row_identity_when_run_updates_do_not_change_membership() {
        let item = Arc::new(command_item("item-stable", "done", 1));
        let projection = with_items(
            empty(),
            vec![run("run-reducer", 1, "completed")],
            &[Arc::clone(&item)],
        );
        let next = apply(
            &projection,
            base_event("run.updated", to_json(&run("run-reducer", 1, "completed"))),
        );
        assert!(Arc::ptr_eq(
            &next.visible_turn_items,
            &projection.visible_turn_items
        ));
        assert!(Arc::ptr_eq(
            &next.visible_turn_items[0],
            &projection.visible_turn_items[0]
        ));
    }

    #[test]
    fn replaces_only_the_updated_visible_item_when_membership_is_unchanged() {
        let first = Arc::new(command_item("item-first", "first", 1));
        let second = Arc::new(command_item("item-second", "second", 2));
        let projection = with_items(
            empty(),
            vec![run("run-reducer", 1, "completed")],
            &[Arc::clone(&first), Arc::clone(&second)],
        );
        let updated = command_item("item-first", "streamed output", 1);
        let next = apply(
            &projection,
            base_event("turn-item.updated", to_json(&updated)),
        );
        assert!(!Arc::ptr_eq(
            &next.visible_turn_items,
            &projection.visible_turn_items
        ));
        assert!(!Arc::ptr_eq(
            &next.visible_turn_items[0],
            &projection.visible_turn_items[0]
        ));
        assert_eq!(*next.visible_turn_items[0].item, updated);
        assert!(Arc::ptr_eq(
            &next.visible_turn_items[1],
            &projection.visible_turn_items[1]
        ));
    }

    #[test]
    fn inserts_live_turn_items_by_authoritative_ordinal() {
        let queued_future = Arc::new(command_item("item-queued-future", "queued", 300));
        let projection = with_items(
            empty(),
            vec![run("run-reducer", 1, "completed")],
            &[Arc::clone(&queued_future)],
        );
        let active = command_item("item-active-assistant", "done", 201);
        let next = apply(
            &projection,
            base_event("turn-item.updated", to_json(&active)),
        );
        assert_eq!(
            visible_ids(&next),
            vec![
                "item-active-assistant".to_owned(),
                "item-queued-future".to_owned()
            ]
        );
        assert_eq!(
            next.visible_turn_items
                .iter()
                .map(|row| row.position)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    mod partial_timeline {
        use super::*;

        fn queued_run() -> OrchestrationV2Run {
            run("run-queued", 2, "queued")
        }

        fn steer_run() -> OrchestrationV2Run {
            run("run-steer", 3, "completed")
        }

        fn window_of(
            runs: Vec<OrchestrationV2Run>,
            items: Vec<OrchestrationV2TurnItem>,
        ) -> ThreadProjection {
            let items: Vec<_> = items.into_iter().map(Arc::new).collect();
            with_items(empty(), runs, &items)
        }

        fn item_event(payload: &OrchestrationV2TurnItem) -> OrchestrationV2DomainEvent {
            base_event("turn-item.updated", to_json(payload))
        }

        fn partial(latest: u64) -> Option<ApplyOrchestrationV2ProjectionEventOptions> {
            Some(ApplyOrchestrationV2ProjectionEventOptions {
                partial_timeline: true,
                latest_local_turn_ordinal: Some(latest),
            })
        }

        #[test]
        fn shows_a_queued_run_that_starts_after_a_newer_steer_raised_the_watermark() {
            // Run 2 was queued first, run 3 was steered in and ran first. Its items
            // sit in run 3's band and already raised the watermark past run 2's band.
            let steer_items = vec![
                run_item("item-steer-user", "run-steer", "x", 3_000_001),
                run_item("item-steer-reply", "run-steer", "x", 3_000_002),
            ];
            let mut projection = window_of(
                vec![
                    run("run-reducer", 1, "completed"),
                    queued_run(),
                    steer_run(),
                ],
                steer_items.clone(),
            );
            let mut latest = 3_000_002;
            let queued_user = run_item("item-queued-user", "run-queued", "x", 2_000_001);
            let queued_reply = run_item("item-queued-reply", "run-queued", "x", 2_000_002);
            let running = OrchestrationV2Run {
                status: OrchestrationV2RunStatus::Running,
                ..queued_run()
            };
            let completed = OrchestrationV2Run {
                status: OrchestrationV2RunStatus::Completed,
                ..queued_run()
            };
            // Same order the orchestrator commits a queued start: the user message
            // lands while the run is still queued, then the run starts and replies.
            let events = [
                (item_event(&queued_user), Some(queued_user.ordinal())),
                (base_event("run.updated", to_json(&running)), None),
                (item_event(&queued_reply), Some(queued_reply.ordinal())),
                (base_event("run.updated", to_json(&completed)), None),
            ];
            for (event, ordinal) in events {
                projection =
                    apply_orchestration_v2_projection_event(&projection, event, partial(latest))
                        .unwrap_or(projection);
                if let Some(ordinal) = ordinal {
                    latest = latest.max(ordinal);
                }
            }
            let mut expected = vec![
                "item-queued-user".to_owned(),
                "item-queued-reply".to_owned(),
            ];
            expected.extend(steer_items.iter().map(|item| item.id().to_string()));
            assert_eq!(visible_ids(&projection), expected);
            assert!(
                projection
                    .turn_items
                    .iter()
                    .any(|item| item.id().as_str() == "item-queued-reply")
            );
        }

        #[test]
        fn keeps_a_running_runs_item_when_the_window_starts_after_its_earlier_items() {
            // The bounded window begins inside run 3; run 2 started and kept going,
            // but none of its rows made it into the window.
            let running_run = OrchestrationV2Run {
                status: OrchestrationV2RunStatus::Running,
                ..queued_run()
            };
            let steer_items = vec![
                run_item("item-steer-user", "run-steer", "x", 3_000_001),
                run_item("item-steer-reply", "run-steer", "x", 3_000_002),
            ];
            let projection = window_of(
                vec![run("run-reducer", 1, "completed"), running_run, steer_run()],
                steer_items.clone(),
            );
            let reply = run_item("item-running-reply", "run-queued", "x", 2_000_003);
            let next = apply_orchestration_v2_projection_event(
                &projection,
                item_event(&reply),
                partial(3_000_002),
            )
            .unwrap();
            let mut expected = vec!["item-running-reply".to_owned()];
            expected.extend(steer_items.iter().map(|item| item.id().to_string()));
            assert_eq!(visible_ids(&next), expected);
        }

        #[test]
        fn items_arriving_below_the_watermark() {
            for (name, item_run, kept) in [
                (
                    "keeps an item of a run already in the window",
                    "run-steer",
                    true,
                ),
                (
                    "drops an item of a finished run outside the window",
                    "run-reducer",
                    false,
                ),
            ] {
                let first = run_item("item-first", "run-steer", "x", 3_000_001);
                let later = run_item("item-later", "run-steer", "x", 3_000_003);
                let late = run_item("item-late", item_run, "x", 3_000_002);
                let projection = window_of(
                    vec![run("run-reducer", 1, "completed"), steer_run()],
                    vec![first.clone(), later.clone()],
                );
                let next = apply_orchestration_v2_projection_event(
                    &projection,
                    item_event(&late),
                    partial(later.ordinal()),
                );
                if kept {
                    assert_eq!(
                        visible_ids(&next.unwrap()),
                        vec![
                            "item-first".to_owned(),
                            "item-late".to_owned(),
                            "item-later".to_owned()
                        ],
                        "{name}"
                    );
                } else {
                    assert_eq!(next, None, "{name}");
                }
            }
        }
    }

    #[test]
    fn removes_only_hidden_local_items_while_preserving_inherited_rows() {
        let inherited = Arc::new(command_item("item-inherited", "done", 1));
        let local = Arc::new(command_item("item-local", "done", 1));
        let inherited_row = row(0, &inherited, false);
        let local_row = row(1, &local, true);
        let projection = ThreadProjection {
            runs: Arc::new(vec![Arc::new(run("run-reducer", 1, "completed"))]),
            turn_items: Arc::new(vec![Arc::clone(&local)]),
            visible_turn_items: Arc::new(vec![Arc::clone(&inherited_row), local_row]),
            ..empty()
        };
        let next = apply(
            &projection,
            base_event(
                "run.updated",
                to_json(&run("run-reducer", 1, "rolled_back")),
            ),
        );
        assert_eq!(next.visible_turn_items.len(), 1);
        assert!(Arc::ptr_eq(&next.visible_turn_items[0], &inherited_row));
    }

    #[test]
    fn streaming_an_item_update_keeps_every_other_row() {
        let runs: Vec<OrchestrationV2Run> = (0..100)
            .map(|index| run(&format!("run-{index}"), 1 + index, "completed"))
            .collect();
        let items: Vec<Arc<OrchestrationV2TurnItem>> = (0..1000)
            .map(|index| Arc::new(command_item(&format!("item-{index}"), "before", index)))
            .collect();
        let projection = with_items(empty(), runs, &items);
        let payload = command_item("item-999", "after", 999);
        let next = apply(
            &projection,
            base_event("turn-item.updated", to_json(&payload)),
        );
        assert_eq!(*next.visible_turn_items.last().unwrap().item, payload);
        assert!(Arc::ptr_eq(
            &next.visible_turn_items[0],
            &projection.visible_turn_items[0]
        ));
        assert_eq!(next.turn_items.len(), 1000);
    }

    #[test]
    fn contract_round_trip_is_lossless() {
        let projection = empty();
        let contract: OrchestrationV2ThreadProjection = projection.to_contract();
        assert_eq!(contract, v2_projection());
    }
}
