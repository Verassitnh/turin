//! Port of upstream/apps/server/src/orchestration-v2/RunExecutionService.ts.
//!
//! Slice 1 ports the pure helpers at the top of the file and at the bottom
//! (`cascadeTerminalizeRunOwnedSubagents`, `makeInterruptResultTurnItem`). The
//! orchestrator's run-interrupt handler calls them, so they land first. The
//! effectful `RunExecutionService` follows in file order (see
//! docs/handoff/T13e.md).

use std::collections::{HashMap, HashSet};

use indexmap::{IndexMap, IndexSet};
use serde_json::{Value, json};
use turin_contracts::{
    EventId, NodeId, ProviderThreadId, ProviderTurnId, RunAttemptId, RunId, ThreadId, TurnItemId,
    UtcDateTime,
    orchestration_v2::{
        OrchestrationV2DomainEvent, OrchestrationV2ExecutionNode,
        OrchestrationV2LineageRelationship, OrchestrationV2PlanArtifact,
        OrchestrationV2ProviderThread, OrchestrationV2ProviderThreadDisposition,
        OrchestrationV2ProviderThreadStatus, OrchestrationV2Run, OrchestrationV2RunStatus,
        OrchestrationV2Subagent, OrchestrationV2SubagentStatus, OrchestrationV2TurnItem,
        OrchestrationV2TurnItemStatus,
    },
};
use turin_provider_core::{id_allocator::derive, provider_adapter::ProviderAdapterV2Event};

use crate::projection::turn_item;

mod finalize;
#[cfg(test)]
mod ingestion_tests;
mod service;
#[cfg(test)]
mod start_tests;
#[cfg(test)]
mod termination_tests;
#[cfg(test)]
pub(crate) mod test_support;

pub(crate) use service::failure_cause;
pub use service::{
    AfterTurnRefresh, AsyncBool, FailureAndWriteCause, InheritedBackgroundLoader,
    NoAfterTurnRefresh, RunBaselineCapture, RunEventIngest, RunEventWriter,
    RunExecutionIngestError, RunExecutionService, RunExecutionServiceError, RunExecutionStartError,
    StartRootRunInput, TerminalSummary,
};
use service::{RefreshOnce, RunExecutionInner};

/// `RunOwnedSubagentTerminalStatus`: the statuses a run's open subagent
/// work is cascaded to when the run stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOwnedSubagentTerminalStatus {
    Interrupted,
    Failed,
    Cancelled,
}

impl RunOwnedSubagentTerminalStatus {
    fn subagent_status(self) -> OrchestrationV2SubagentStatus {
        match self {
            Self::Interrupted => OrchestrationV2SubagentStatus::Interrupted,
            Self::Failed => OrchestrationV2SubagentStatus::Failed,
            Self::Cancelled => OrchestrationV2SubagentStatus::Cancelled,
        }
    }

    fn turn_item_status(self) -> OrchestrationV2TurnItemStatus {
        match self {
            Self::Interrupted => OrchestrationV2TurnItemStatus::Interrupted,
            Self::Failed => OrchestrationV2TurnItemStatus::Failed,
            Self::Cancelled => OrchestrationV2TurnItemStatus::Cancelled,
        }
    }

    fn node_status(self) -> turin_contracts::orchestration_v2::OrchestrationV2ExecutionNodeStatus {
        use turin_contracts::orchestration_v2::OrchestrationV2ExecutionNodeStatus as NodeStatus;
        match self {
            Self::Interrupted => NodeStatus::Interrupted,
            Self::Failed => NodeStatus::Failed,
            Self::Cancelled => NodeStatus::Cancelled,
        }
    }
}

/// `OpenRunOwnedSubagentProjection`: the open rows a cascade may terminalize.
/// `turn_items` holds only subagent items (upstream `SubagentTurnItem`).
#[derive(Debug, Clone, Default)]
pub struct OpenRunOwnedSubagentProjection {
    pub subagents: IndexMap<NodeId, OrchestrationV2Subagent>,
    pub turn_items: IndexMap<NodeId, OrchestrationV2TurnItem>,
    pub child_turn_items: IndexMap<TurnItemId, OrchestrationV2TurnItem>,
    pub nodes: IndexMap<NodeId, OrchestrationV2ExecutionNode>,
    /// Child threads once linked by a root-run subagent row; kept for cascade.
    pub linked_child_thread_ids: IndexSet<ThreadId>,
}

/// `isOrchestrationV2WorkActive` for a subagent status.
fn is_subagent_work_active(status: OrchestrationV2SubagentStatus) -> bool {
    matches!(
        status,
        OrchestrationV2SubagentStatus::Pending
            | OrchestrationV2SubagentStatus::Running
            | OrchestrationV2SubagentStatus::Waiting
    )
}

/// `isOrchestrationV2WorkActive` for a turn item status.
fn is_turn_item_work_active(status: OrchestrationV2TurnItemStatus) -> bool {
    matches!(
        status,
        OrchestrationV2TurnItemStatus::Pending
            | OrchestrationV2TurnItemStatus::Running
            | OrchestrationV2TurnItemStatus::Waiting
    )
}

/// `isOpenExecutionNodeStatus`.
fn is_open_execution_node_status(
    status: turin_contracts::orchestration_v2::OrchestrationV2ExecutionNodeStatus,
) -> bool {
    use turin_contracts::orchestration_v2::OrchestrationV2ExecutionNodeStatus as NodeStatus;
    matches!(
        status,
        NodeStatus::Pending | NodeStatus::Running | NodeStatus::Waiting
    )
}

/// The child thread a subagent turn item points at. Only subagent items carry one.
fn subagent_child_thread_id(item: &OrchestrationV2TurnItem) -> Option<&ThreadId> {
    match item {
        OrchestrationV2TurnItem::Subagent {
            child_thread_id, ..
        } => child_thread_id.as_ref(),
        _ => None,
    }
}

/// `{ ...turnItem, status, completedAt, updatedAt }`, plus `streaming: false`
/// when the item streams. The contract enum has no per-field setters, so the
/// spread runs on the wire shape, which is the same object upstream spreads.
fn terminal_turn_item(
    item: &OrchestrationV2TurnItem,
    status: OrchestrationV2TurnItemStatus,
    completed_at: &UtcDateTime,
) -> Result<OrchestrationV2TurnItem, serde_json::Error> {
    let mut wire = serde_json::to_value(item)?;
    if let Value::Object(fields) = &mut wire {
        if fields.contains_key("streaming") {
            fields.insert("streaming".to_owned(), json!(false));
        }
        fields.insert("status".to_owned(), serde_json::to_value(status)?);
        fields.insert(
            "completedAt".to_owned(),
            serde_json::to_value(completed_at)?,
        );
        fields.insert("updatedAt".to_owned(), serde_json::to_value(completed_at)?);
    }
    serde_json::from_value(wire)
}

/// `cascadeTerminalizeRunOwnedSubagents`: the events that close a run's open
/// subagent work, its child-thread nodes and its turn items. `allocate_event_id`
/// mints each event id (upstream `allocateEventId`).
///
/// Errors only if an item fails to round-trip through its wire shape, which
/// cannot happen for a value the contracts decoded.
pub fn cascade_terminalize_run_owned_subagents(
    run: &OrchestrationV2Run,
    open: &OpenRunOwnedSubagentProjection,
    status: RunOwnedSubagentTerminalStatus,
    completed_at: &UtcDateTime,
    allocate_event_id: &mut dyn FnMut() -> EventId,
) -> Result<Vec<OrchestrationV2DomainEvent>, serde_json::Error> {
    let mut events = Vec::new();
    // Prefer lifetime linkage over currently-open rows: subagent/turn-item
    // snapshots may terminalize before the linked child-thread node settles.
    let mut child_thread_ids = open.linked_child_thread_ids.clone();
    for child_thread_id in open
        .subagents
        .values()
        .filter_map(|subagent| subagent.child_thread_id.as_ref())
        .chain(
            open.turn_items
                .values()
                .filter_map(subagent_child_thread_id),
        )
    {
        child_thread_ids.insert(child_thread_id.clone());
    }

    let keys: IndexSet<NodeId> = open
        .subagents
        .keys()
        .chain(open.turn_items.keys())
        .chain(open.nodes.keys())
        .cloned()
        .collect();
    for key in &keys {
        if let Some(subagent) = open
            .subagents
            .get(key)
            .filter(|subagent| is_subagent_work_active(subagent.status))
        {
            let mut payload = subagent.clone();
            payload.status = status.subagent_status();
            payload.completed_at = Some(*completed_at);
            payload.updated_at = *completed_at;
            events.push(OrchestrationV2DomainEvent::SubagentUpdated {
                id: allocate_event_id(),
                thread_id: subagent.thread_id.clone(),
                run_id: Some(run.id.clone()),
                node_id: Some(subagent.id.clone()),
                driver: Some(subagent.driver.clone()),
                provider_instance_id: Some(subagent.provider_instance_id.clone()),
                raw_event_id: None,
                occurred_at: *completed_at,
                payload,
            });
        }
        if let Some(node) = open.nodes.get(key) {
            let owned = node.thread_id == run.thread_id && node.run_id.as_ref() == Some(&run.id);
            if (owned || child_thread_ids.contains(&node.thread_id))
                && is_open_execution_node_status(node.status)
            {
                let mut payload = node.clone();
                payload.status = status.node_status();
                payload.completed_at = Some(*completed_at);
                events.push(OrchestrationV2DomainEvent::NodeUpdated {
                    id: allocate_event_id(),
                    thread_id: node.thread_id.clone(),
                    run_id: Some(node.run_id.clone().unwrap_or_else(|| run.id.clone())),
                    node_id: Some(node.id.clone()),
                    driver: None,
                    provider_instance_id: Some(run.provider_instance_id.clone()),
                    raw_event_id: None,
                    occurred_at: *completed_at,
                    payload,
                });
            }
        }
        if let Some(item) = open.turn_items.get(key) {
            let common = turn_item::common(item);
            if common.run_id == Some(&run.id) && is_turn_item_work_active(common.status) {
                events.push(turn_item_updated(
                    allocate_event_id(),
                    item,
                    run,
                    status,
                    completed_at,
                )?);
            }
        }
    }
    for item in open.child_turn_items.values() {
        let common = turn_item::common(item);
        if !child_thread_ids.contains(common.thread_id) || !is_turn_item_work_active(common.status)
        {
            continue;
        }
        events.push(turn_item_updated(
            allocate_event_id(),
            item,
            run,
            status,
            completed_at,
        )?);
    }
    Ok(events)
}

/// A `turn-item.updated` that closes `item` with the run's terminal status.
fn turn_item_updated(
    id: EventId,
    item: &OrchestrationV2TurnItem,
    run: &OrchestrationV2Run,
    status: RunOwnedSubagentTerminalStatus,
    completed_at: &UtcDateTime,
) -> Result<OrchestrationV2DomainEvent, serde_json::Error> {
    let common = turn_item::common(item);
    let payload = terminal_turn_item(item, status.turn_item_status(), completed_at)?;
    Ok(OrchestrationV2DomainEvent::TurnItemUpdated {
        id,
        thread_id: common.thread_id.clone(),
        run_id: Some(common.run_id.cloned().unwrap_or_else(|| run.id.clone())),
        node_id: common.node_id.cloned(),
        driver: None,
        provider_instance_id: Some(run.provider_instance_id.clone()),
        raw_event_id: None,
        occurred_at: *completed_at,
        payload,
    })
}

/// `makeInterruptResultTurnItem`: the `run_interrupt_result` item that closes a
/// hard-stopped run's root node. Its id is derived, so a replayed interrupt
/// lands on the same row.
pub fn make_interrupt_result_turn_item(
    run: &OrchestrationV2Run,
    root_node: &OrchestrationV2ExecutionNode,
    provider_thread: &OrchestrationV2ProviderThread,
    completed_at: &UtcDateTime,
) -> OrchestrationV2TurnItem {
    OrchestrationV2TurnItem::RunInterruptResult {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: derive::run_signal_turn_item(&run.id, "interrupt-result"),
        thread_id: run.thread_id.clone(),
        run_id: Some(run.id.clone()),
        node_id: Some(root_node.id.clone()),
        provider_thread_id: Some(provider_thread.id.clone()),
        provider_turn_id: root_node.provider_turn_id.clone(),
        native_item_ref: None,
        parent_item_id: Some(derive::run_signal_turn_item(&run.id, "interrupt-request")),
        ordinal: run.ordinal.get() * 100 + 98,
        status: OrchestrationV2TurnItemStatus::Interrupted,
        title: Some("Interrupted".to_owned()),
        started_at: Some(*completed_at),
        completed_at: Some(*completed_at),
        updated_at: *completed_at,
        message: "Run interrupted by user".to_owned(),
    }
}

/// `ProviderEventRoutingState`. Upstream threads an immutable copy through
/// `routeProviderEvent`; here the state is updated in place, which gives the
/// same routing decisions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderEventRoutingState {
    pub owned_thread_ids: HashSet<ThreadId>,
    /// Set once this run's root turn ended. A child thread created after that
    /// belongs to the run that is live then, so this one no longer adopts it,
    /// except a workflow member enrolled below: its coordinator keeps this run
    /// open, so the member is this run's to follow.
    pub root_turn_ended: bool,
    /// Member thread id to the coordinator child thread that spawned it.
    pub workflow_member_parents: HashMap<ThreadId, ThreadId>,
    pub owned_provider_thread_ids: HashSet<ProviderThreadId>,
    pub owned_provider_turn_ids: HashSet<ProviderTurnId>,
    pub inherited_background_turn_items: HashMap<TurnItemId, RunId>,
    pub root_provider_turn_id: Option<ProviderTurnId>,
}

/// The run, attempt and provider thread a routed event is checked against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderEventRouteIdentity {
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub attempt_id: RunAttemptId,
    pub provider_thread_id: ProviderThreadId,
}

/// A background turn item a new run may deliver, because its run is settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InheritedBackgroundTurnItemRoute {
    pub id: TurnItemId,
    pub run_id: RunId,
}

/// `makeProviderEventRoutingState`.
pub fn make_provider_event_routing_state(
    identity: &ProviderEventRouteIdentity,
    inherited_background_turn_items: &[InheritedBackgroundTurnItemRoute],
    provider_turn_id: Option<&ProviderTurnId>,
    related_thread_ids: &[ThreadId],
    related_provider_thread_ids: &[ProviderThreadId],
) -> ProviderEventRoutingState {
    let mut owned_thread_ids: HashSet<ThreadId> = related_thread_ids.iter().cloned().collect();
    owned_thread_ids.insert(identity.thread_id.clone());
    let mut owned_provider_thread_ids: HashSet<ProviderThreadId> =
        related_provider_thread_ids.iter().cloned().collect();
    owned_provider_thread_ids.insert(identity.provider_thread_id.clone());
    ProviderEventRoutingState {
        owned_thread_ids,
        root_turn_ended: false,
        workflow_member_parents: HashMap::new(),
        owned_provider_thread_ids,
        owned_provider_turn_ids: provider_turn_id.cloned().into_iter().collect(),
        inherited_background_turn_items: inherited_background_turn_items
            .iter()
            .map(|item| (item.id.clone(), item.run_id.clone()))
            .collect(),
        root_provider_turn_id: provider_turn_id.cloned(),
    }
}

/// `routeProviderEvent`: whether `event` belongs to this run, and the state
/// after it. A `true` event is delivered; the state is updated in place.
pub fn route_provider_event(
    event: &ProviderAdapterV2Event,
    input: &ProviderEventRouteIdentity,
    state: &mut ProviderEventRoutingState,
) -> bool {
    let owns_thread = |state: &ProviderEventRoutingState, thread_id: &ThreadId| {
        state.owned_thread_ids.contains(thread_id)
    };
    let owns_child_thread = |state: &ProviderEventRoutingState, thread_id: &ThreadId| {
        thread_id != &input.thread_id && owns_thread(state, thread_id)
    };
    let owns_run = |run_id: Option<&RunId>| run_id == Some(&input.run_id);

    match event {
        ProviderAdapterV2Event::ProviderSessionUpdated { .. } => false,
        ProviderAdapterV2Event::AppThreadCreated { app_thread, .. } => {
            if app_thread.id == input.thread_id {
                return true;
            }
            let lineage = &app_thread.lineage;
            let owned_subagent = lineage.relationship_to_parent
                == Some(OrchestrationV2LineageRelationship::Subagent)
                && lineage
                    .parent_thread_id
                    .as_ref()
                    .is_some_and(|parent| owns_thread(state, parent))
                && (!state.root_turn_ended
                    || state.workflow_member_parents.get(&app_thread.id)
                        == lineage.parent_thread_id.as_ref());
            if owned_subagent {
                state.owned_thread_ids.insert(app_thread.id.clone());
            }
            owned_subagent
        }
        ProviderAdapterV2Event::ProviderThreadUpdated {
            provider_thread, ..
        } => {
            let belongs = state
                .owned_provider_thread_ids
                .contains(&provider_thread.id)
                || provider_thread
                    .app_thread_id
                    .as_ref()
                    .is_some_and(|thread| owns_thread(state, thread));
            if belongs {
                state
                    .owned_provider_thread_ids
                    .insert(provider_thread.id.clone());
            }
            belongs
        }
        ProviderAdapterV2Event::ProviderTurnUpdated {
            thread_id,
            provider_turn,
            ..
        } => {
            let is_root = provider_turn.run_attempt_id.as_ref() == Some(&input.attempt_id);
            let belongs = is_root
                || (provider_turn.provider_thread_id != input.provider_thread_id
                    && state
                        .owned_provider_thread_ids
                        .contains(&provider_turn.provider_thread_id))
                || state.owned_provider_turn_ids.contains(&provider_turn.id)
                || thread_id
                    .as_ref()
                    .is_some_and(|thread| owns_child_thread(state, thread));
            if belongs {
                state
                    .owned_provider_turn_ids
                    .insert(provider_turn.id.clone());
                if is_root {
                    state.root_provider_turn_id = Some(provider_turn.id.clone());
                }
            }
            belongs
        }
        ProviderAdapterV2Event::NodeUpdated { node, .. } => {
            let belongs =
                owns_run(node.run_id.as_ref()) || owns_child_thread(state, &node.thread_id);
            match &node.provider_thread_id {
                Some(provider_thread_id) if belongs => {
                    state
                        .owned_provider_thread_ids
                        .insert(provider_thread_id.clone());
                    true
                }
                _ => belongs,
            }
        }
        ProviderAdapterV2Event::SubagentUpdated { subagent, .. } => {
            let belongs =
                owns_run(subagent.run_id.as_ref()) || owns_child_thread(state, &subagent.thread_id);
            let (Some(workflow), Some(coordinator_child)) =
                (&subagent.workflow, &subagent.child_thread_id)
            else {
                return belongs;
            };
            if !owns_run(subagent.run_id.as_ref()) {
                return belongs;
            }
            for member in &workflow.agents {
                if let Some(member_child) = &member.child_thread_id {
                    state
                        .workflow_member_parents
                        .insert(member_child.clone(), coordinator_child.clone());
                }
            }
            true
        }
        ProviderAdapterV2Event::MessageUpdated { message, .. } => {
            owns_run(message.run_id.as_ref()) || owns_child_thread(state, &message.thread_id)
        }
        ProviderAdapterV2Event::TurnItemUpdated { turn_item, .. } => {
            let common = turn_item::common(turn_item);
            if owns_run(common.run_id) || owns_child_thread(state, common.thread_id) {
                return true;
            }
            // Keep the item's original ownership, but let the one live run
            // deliver an exact carryover identity selected from the projection.
            let inherited_run_id = state.inherited_background_turn_items.get(common.id);
            let is_inherited_background_item = common.thread_id == &input.thread_id
                && common.run_id.is_some()
                && inherited_run_id == common.run_id
                && is_background_capable(turn_item::type_tag(turn_item));
            if !is_inherited_background_item {
                return false;
            }
            if is_turn_item_work_active(common.status) {
                return true;
            }
            state.inherited_background_turn_items.remove(common.id);
            true
        }
        ProviderAdapterV2Event::PlanUpdated { plan, .. } => {
            let (thread_id, run_id) = match plan {
                OrchestrationV2PlanArtifact::ProposedPlan {
                    thread_id, run_id, ..
                }
                | OrchestrationV2PlanArtifact::TodoList {
                    thread_id, run_id, ..
                } => (thread_id, run_id),
            };
            owns_run(run_id.as_ref()) || owns_child_thread(state, thread_id)
        }
        ProviderAdapterV2Event::RuntimeRequestUpdated {
            thread_id,
            runtime_request,
            ..
        } => {
            thread_id
                .as_ref()
                .is_some_and(|thread| owns_child_thread(state, thread))
                || runtime_request
                    .provider_turn_id
                    .as_ref()
                    .is_some_and(|turn| state.owned_provider_turn_ids.contains(turn))
        }
        ProviderAdapterV2Event::TurnTerminal {
            provider_turn_id, ..
        } => {
            if state.root_provider_turn_id.as_ref() == Some(provider_turn_id) {
                state.root_turn_ended = true;
                true
            } else {
                false
            }
        }
    }
}

/// `finalProviderThreadStatus`.
pub fn final_provider_thread_status(
    disposition: OrchestrationV2ProviderThreadDisposition,
) -> OrchestrationV2ProviderThreadStatus {
    match disposition {
        OrchestrationV2ProviderThreadDisposition::Broken => {
            OrchestrationV2ProviderThreadStatus::Error
        }
        OrchestrationV2ProviderThreadDisposition::Reusable => {
            OrchestrationV2ProviderThreadStatus::Idle
        }
    }
}

/// `canRouteRelatedSubagent`: only a completed subagent's child thread is resumed.
pub fn can_route_related_subagent(status: OrchestrationV2SubagentStatus) -> bool {
    status == OrchestrationV2SubagentStatus::Completed
}

/// `selectInheritedBackgroundTurnItems`: background items of a settled earlier
/// run on the same provider thread that are still open. The new run takes
/// over their delivery.
pub fn select_inherited_background_turn_items(
    thread_id: &ThreadId,
    current_provider_thread_id: &ProviderThreadId,
    current_run_ordinal: u64,
    runs: &[OrchestrationV2Run],
    turn_items: &[OrchestrationV2TurnItem],
) -> Vec<InheritedBackgroundTurnItemRoute> {
    let settled_prior_run_ids: HashSet<&RunId> = runs
        .iter()
        .filter(|run| {
            &run.thread_id == thread_id
                && run.ordinal.get() < current_run_ordinal
                && matches!(
                    run.status,
                    OrchestrationV2RunStatus::Interrupted
                        | OrchestrationV2RunStatus::Failed
                        | OrchestrationV2RunStatus::Cancelled
                )
        })
        .map(|run| &run.id)
        .collect();
    turn_items
        .iter()
        .filter_map(|item| {
            let common = turn_item::common(item);
            let run_id = common.run_id?;
            (common.thread_id == thread_id
                && common.provider_thread_id == Some(current_provider_thread_id)
                && settled_prior_run_ids.contains(run_id)
                && is_background_capable(turn_item::type_tag(item))
                && is_turn_item_work_active(common.status))
            .then(|| InheritedBackgroundTurnItemRoute {
                id: common.id.clone(),
                run_id: run_id.clone(),
            })
        })
        .collect()
}

/// `backgroundCapableTurnItemTypes`: work whose lifecycle can outlive the root turn.
fn is_background_capable(type_tag: &str) -> bool {
    matches!(type_tag, "command_execution" | "dynamic_tool" | "subagent")
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use turin_contracts::{EventId, orchestration_v2::OrchestrationV2DomainEvent};

    use super::*;

    const NOW: &str = "2026-06-22T00:00:00.000Z";

    fn now() -> UtcDateTime {
        serde_json::from_value(json!(NOW)).expect("utc")
    }

    fn run() -> OrchestrationV2Run {
        serde_json::from_value(json!({
            "id": "run-1", "threadId": "thread-1", "ordinal": 1,
            "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "gpt-5.4"},
            "providerThreadId": null, "userMessageId": "message-1",
            "rootNodeId": "node-root", "activeAttemptId": null, "status": "running",
            "requestedAt": NOW, "startedAt": NOW, "completedAt": null,
            "checkpointId": null, "contextHandoffId": null,
        }))
        .expect("run fixture")
    }

    fn subagent(status: &str) -> OrchestrationV2Subagent {
        serde_json::from_value(json!({
            "id": "node-sub", "threadId": "thread-1", "runId": "run-1",
            "parentNodeId": "node-root", "origin": "provider_native", "createdBy": "agent",
            "driver": "codex", "providerInstanceId": "codex", "providerThreadId": null,
            "childThreadId": "thread-child", "nativeTaskRef": null, "prompt": "hold",
            "title": "Worker", "model": null, "status": status,
            "progress": "partial progress", "result": "partial result",
            "startedAt": NOW, "completedAt": null, "updatedAt": NOW,
        }))
        .expect("subagent fixture")
    }

    fn node(
        id: &str,
        thread: &str,
        run_id: Option<&str>,
        kind: &str,
        status: &str,
    ) -> OrchestrationV2ExecutionNode {
        serde_json::from_value(json!({
            "id": id, "threadId": thread, "runId": run_id, "parentNodeId": null,
            "rootNodeId": id, "kind": kind, "status": status, "countsForRun": false,
            "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
            "runtimeRequestId": null, "checkpointScopeId": null,
            "startedAt": NOW, "completedAt": null,
        }))
        .expect("node fixture")
    }

    fn subagent_item(status: &str) -> OrchestrationV2TurnItem {
        serde_json::from_value(json!({
            "type": "subagent", "id": "turn-item:sub", "threadId": "thread-1",
            "runId": "run-1", "nodeId": "node-sub", "providerThreadId": null,
            "providerTurnId": null, "nativeItemRef": null, "parentItemId": null,
            "ordinal": 3, "status": status, "title": "hold", "startedAt": NOW,
            "completedAt": null, "updatedAt": NOW, "subagentId": "node-sub",
            "origin": "provider_native", "driver": "codex", "providerInstanceId": "codex",
            "childThreadId": "thread-child", "prompt": "hold",
            "progress": "partial progress", "result": "partial result",
        }))
        .expect("subagent item fixture")
    }

    fn child_reasoning(status: &str) -> OrchestrationV2TurnItem {
        serde_json::from_value(json!({
            "type": "reasoning", "id": "turn-item:child", "threadId": "thread-child",
            "runId": null, "nodeId": "node-child", "providerThreadId": null,
            "providerTurnId": null, "nativeItemRef": null, "parentItemId": null,
            "ordinal": 1, "status": status, "title": "Working", "startedAt": NOW,
            "completedAt": null, "updatedAt": NOW, "text": "partial progress",
            "streaming": true,
        }))
        .expect("reasoning fixture")
    }

    /// Hands out `event-1`, `event-2`, ... so the test can check the order.
    fn ids() -> impl FnMut() -> EventId {
        let mut next = 0;
        move || {
            next += 1;
            serde_json::from_value(json!(format!("event-{next}"))).expect("event id")
        }
    }

    fn wire(event: &OrchestrationV2DomainEvent) -> Value {
        serde_json::to_value(event).expect("event serializes")
    }

    #[test]
    fn cascades_run_owned_subagent_work_to_the_terminal_status() {
        let mut open = OpenRunOwnedSubagentProjection::default();
        open.subagents
            .insert(NodeId::new("node-sub").expect("id"), subagent("running"));
        open.turn_items.insert(
            NodeId::new("node-sub").expect("id"),
            subagent_item("running"),
        );
        open.nodes.insert(
            NodeId::new("node-sub").expect("id"),
            node("node-sub", "thread-1", Some("run-1"), "subagent", "running"),
        );
        open.linked_child_thread_ids
            .insert(ThreadId::new("thread-child").expect("id"));

        let events = cascade_terminalize_run_owned_subagents(
            &run(),
            &open,
            RunOwnedSubagentTerminalStatus::Interrupted,
            &now(),
            &mut ids(),
        )
        .expect("cascade");

        // subagent + node + turn item.
        assert_eq!(events.len(), 3);
        let subagent_event = wire(&events[0]);
        assert_eq!(subagent_event["type"], "subagent.updated");
        assert_eq!(subagent_event["payload"]["status"], "interrupted");
        assert_eq!(subagent_event["payload"]["childThreadId"], "thread-child");
        assert_eq!(subagent_event["payload"]["progress"], "partial progress");
        assert_eq!(subagent_event["payload"]["result"], "partial result");
        assert_eq!(subagent_event["payload"]["completedAt"], NOW);
        let node_event = wire(&events[1]);
        assert_eq!(node_event["type"], "node.updated");
        assert_eq!(node_event["payload"]["status"], "interrupted");
        let item_event = wire(&events[2]);
        assert_eq!(item_event["type"], "turn-item.updated");
        assert_eq!(item_event["payload"]["type"], "subagent");
        assert_eq!(item_event["payload"]["status"], "interrupted");
        assert_eq!(item_event["payload"]["childThreadId"], "thread-child");
    }

    #[test]
    fn linked_child_node_and_turn_item_cascade_after_link_rows_terminalize() {
        let mut open = OpenRunOwnedSubagentProjection::default();
        open.child_turn_items.insert(
            TurnItemId::new("turn-item:child").expect("id"),
            child_reasoning("running"),
        );
        open.nodes.insert(
            NodeId::new("node-child").expect("id"),
            node("node-child", "thread-child", None, "root_turn", "running"),
        );
        open.linked_child_thread_ids
            .insert(ThreadId::new("thread-child").expect("id"));

        let events = cascade_terminalize_run_owned_subagents(
            &run(),
            &open,
            RunOwnedSubagentTerminalStatus::Cancelled,
            &now(),
            &mut ids(),
        )
        .expect("cascade");

        // The child node is in the linked child thread, so it cascades too.
        // Its turn item closes without the stream flag.
        assert_eq!(events.len(), 2);
        let child_node = wire(&events[0]);
        assert_eq!(child_node["type"], "node.updated");
        assert_eq!(child_node["payload"]["status"], "cancelled");
        assert_eq!(child_node["payload"]["completedAt"], NOW);
        let child_item = wire(&events[1]);
        assert_eq!(child_item["type"], "turn-item.updated");
        assert_eq!(child_item["payload"]["status"], "cancelled");
        assert_eq!(child_item["payload"]["streaming"], false);
    }

    #[test]
    fn leaves_settled_rows_and_unlinked_child_work_alone() {
        let mut open = OpenRunOwnedSubagentProjection::default();
        // A settled subagent still names its child thread; clear it so the
        // child rows below are not linked through it.
        let mut settled = subagent("completed");
        settled.child_thread_id = None;
        open.subagents
            .insert(NodeId::new("node-sub").expect("id"), settled);
        open.child_turn_items.insert(
            TurnItemId::new("turn-item:child").expect("id"),
            child_reasoning("running"),
        );
        open.nodes.insert(
            NodeId::new("node-child").expect("id"),
            node("node-child", "thread-other", None, "root_turn", "running"),
        );

        let events = cascade_terminalize_run_owned_subagents(
            &run(),
            &open,
            RunOwnedSubagentTerminalStatus::Failed,
            &now(),
            &mut ids(),
        )
        .expect("cascade");

        assert!(events.is_empty());
    }

    #[test]
    fn interrupt_result_closes_the_root_node_with_derived_ids() {
        let provider_thread: OrchestrationV2ProviderThread = serde_json::from_value(json!({
            "id": "provider-thread:1", "driver": "codex", "providerInstanceId": "codex",
            "providerSessionId": "provider-session:1", "appThreadId": "thread-1",
            "ownerNodeId": null, "nativeThreadRef": null, "nativeConversationHeadRef": null,
            "status": "idle", "firstRunOrdinal": null, "lastRunOrdinal": null,
            "handoffIds": [], "forkedFrom": null,
            "createdAt": NOW, "updatedAt": NOW,
        }))
        .expect("provider thread fixture");
        let root = node(
            "node-root",
            "thread-1",
            Some("run-1"),
            "root_turn",
            "running",
        );

        let item = make_interrupt_result_turn_item(&run(), &root, &provider_thread, &now());

        let wire_item = serde_json::to_value(&item).expect("item serializes");
        assert_eq!(wire_item["type"], "run_interrupt_result");
        assert_eq!(wire_item["status"], "interrupted");
        assert_eq!(wire_item["ordinal"], 198);
        assert_eq!(wire_item["title"], "Interrupted");
        assert_eq!(wire_item["message"], "Run interrupted by user");
        assert_eq!(wire_item["nodeId"], "node-root");
        assert_eq!(wire_item["providerThreadId"], "provider-thread:1");
        assert_eq!(
            wire_item["id"],
            serde_json::to_value(derive::run_signal_turn_item(&run().id, "interrupt-result"))
                .expect("id")
        );
        assert_eq!(
            wire_item["parentItemId"],
            serde_json::to_value(derive::run_signal_turn_item(&run().id, "interrupt-request"))
                .expect("id")
        );
    }

    fn identity() -> ProviderEventRouteIdentity {
        ProviderEventRouteIdentity {
            thread_id: ThreadId::new("thread-1").expect("id"),
            run_id: RunId::new("run-1").expect("id"),
            attempt_id: RunAttemptId::new("attempt-1").expect("id"),
            provider_thread_id: ProviderThreadId::new("provider-thread-1").expect("id"),
        }
    }

    fn provider_turn_event(
        turn_id: &str,
        attempt: &str,
        provider_thread: &str,
    ) -> ProviderAdapterV2Event {
        serde_json::from_value(json!({
            "type": "provider_turn.updated", "driver": "codex", "provider_turn": {
                "id": turn_id, "providerThreadId": provider_thread, "nodeId": "node-root",
                "runAttemptId": attempt, "nativeTurnRef": null, "ordinal": 1,
                "status": "running", "startedAt": NOW, "completedAt": null,
                "tokenUsage": null, "turnTokenUsage": null,
            },
        }))
        .expect("provider turn event")
    }

    fn terminal_event(turn_id: &str) -> ProviderAdapterV2Event {
        serde_json::from_value(json!({
            "type": "turn.terminal", "driver": "codex",
            "provider_thread_id": "provider-thread-1", "provider_turn_id": turn_id,
            "run_ordinal": 1, "status": "completed", "failure": null,
            "thread_disposition": "reusable",
        }))
        .expect("terminal event")
    }

    #[test]
    fn root_turn_terminal_routes_only_for_the_root_provider_turn() {
        let identity = identity();
        let mut state = make_provider_event_routing_state(&identity, &[], None, &[], &[]);

        // Another attempt's turn is not ours, so it does not become the root.
        let other = provider_turn_event("turn-other", "attempt-other", "provider-thread-other");
        assert!(!route_provider_event(&other, &identity, &mut state));
        assert!(!route_provider_event(
            &terminal_event("turn-other"),
            &identity,
            &mut state
        ));
        assert!(!state.root_turn_ended);

        let root = provider_turn_event("turn-root", "attempt-1", "provider-thread-1");
        assert!(route_provider_event(&root, &identity, &mut state));
        assert_eq!(
            state.root_provider_turn_id,
            Some(ProviderTurnId::new("turn-root").expect("id"))
        );

        assert!(route_provider_event(
            &terminal_event("turn-root"),
            &identity,
            &mut state
        ));
        assert!(state.root_turn_ended);
    }

    #[test]
    fn node_on_this_run_adopts_its_provider_thread() {
        let identity = identity();
        let mut state = make_provider_event_routing_state(&identity, &[], None, &[], &[]);
        let mut node = node("node-x", "thread-1", Some("run-1"), "root_turn", "running");
        node.provider_thread_id = Some(ProviderThreadId::new("provider-thread-new").expect("id"));
        let event = ProviderAdapterV2Event::NodeUpdated {
            driver: serde_json::from_value(json!("codex")).expect("driver"),
            node,
        };
        assert!(route_provider_event(&event, &identity, &mut state));
        assert!(
            state
                .owned_provider_thread_ids
                .contains(&ProviderThreadId::new("provider-thread-new").expect("id"))
        );
    }

    #[test]
    fn inherited_background_item_routes_until_it_settles() {
        let identity = identity();
        let item_id = TurnItemId::new("turn-item:bg").expect("id");
        let inherited = vec![InheritedBackgroundTurnItemRoute {
            id: item_id.clone(),
            run_id: RunId::new("run-0").expect("id"),
        }];
        let mut state = make_provider_event_routing_state(&identity, &inherited, None, &[], &[]);
        let item = |status: &str| {
            let mut value = json!({
                "type": "command_execution", "id": "turn-item:bg", "threadId": "thread-1",
                "runId": "run-0", "nodeId": null, "providerThreadId": "provider-thread-1",
                "providerTurnId": null, "nativeItemRef": null, "parentItemId": null,
                "ordinal": 2, "status": status, "title": "sleep", "startedAt": NOW,
                "completedAt": null, "updatedAt": NOW, "input": "sleep 300",
            });
            if status != "running" {
                value["completedAt"] = json!(NOW);
            }
            ProviderAdapterV2Event::TurnItemUpdated {
                driver: serde_json::from_value(json!("codex")).expect("driver"),
                turn_item: serde_json::from_value(value).expect("item"),
            }
        };

        assert!(route_provider_event(
            &item("running"),
            &identity,
            &mut state
        ));
        assert!(state.inherited_background_turn_items.contains_key(&item_id));
        assert!(route_provider_event(
            &item("completed"),
            &identity,
            &mut state
        ));
        assert!(!state.inherited_background_turn_items.contains_key(&item_id));
    }

    #[test]
    fn selects_only_open_background_items_of_settled_prior_runs_on_this_provider_thread() {
        let run = |id: &str, ordinal: u64, status: &str| -> OrchestrationV2Run {
            let mut value =
                crate::pipeline::test_support::run_json(id, "thread-1", ordinal, status);
            value["providerThreadId"] = json!("provider-thread-1");
            serde_json::from_value(value).expect("run fixture")
        };
        let runs = vec![
            run("run-prior", 1, "interrupted"),
            run("run-live", 2, "running"),
            run("run-done", 3, "completed"),
        ];
        let item = |id: &str, run_id: &str, status: &str| -> OrchestrationV2TurnItem {
            serde_json::from_value(json!({
                "type": "command_execution", "id": id, "threadId": "thread-1",
                "runId": run_id, "nodeId": null, "providerThreadId": "provider-thread-1",
                "providerTurnId": null, "nativeItemRef": null, "parentItemId": null,
                "ordinal": 2, "status": status, "title": "sleep", "startedAt": NOW,
                "completedAt": null, "updatedAt": NOW, "input": "sleep 300",
            }))
            .expect("item fixture")
        };
        let items = vec![
            item("open-prior", "run-prior", "running"),
            item("settled-prior", "run-prior", "completed"),
            item("open-live", "run-live", "running"),
            item("open-done", "run-done", "running"),
        ];
        let selected = select_inherited_background_turn_items(
            &ThreadId::new("thread-1").expect("id"),
            &ProviderThreadId::new("provider-thread-1").expect("id"),
            4,
            &runs,
            &items,
        );
        assert_eq!(
            selected,
            vec![InheritedBackgroundTurnItemRoute {
                id: TurnItemId::new("open-prior").expect("id"),
                run_id: RunId::new("run-prior").expect("id"),
            }]
        );
    }
}
