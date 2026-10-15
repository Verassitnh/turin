//! Port of upstream/apps/server/src/orchestration-v2/RunExecutionService.ts.
//!
//! Slice 1 ports the pure helpers at the top of the file and at the bottom
//! (`cascadeTerminalizeRunOwnedSubagents`, `makeInterruptResultTurnItem`). The
//! orchestrator's run-interrupt handler calls them, so they land first. The
//! effectful `RunExecutionService` follows in file order (see
//! docs/handoff/T13e.md).

use indexmap::{IndexMap, IndexSet};
use serde_json::{Value, json};
use turin_contracts::{
    EventId, NodeId, ThreadId, TurnItemId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2DomainEvent, OrchestrationV2ExecutionNode, OrchestrationV2ProviderThread,
        OrchestrationV2Run, OrchestrationV2Subagent, OrchestrationV2SubagentStatus,
        OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
    },
};
use turin_provider_core::id_allocator::derive;

use crate::projection::turn_item;

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
}
