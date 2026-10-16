//! Port of upstream/packages/client-runtime/src/state/itemSupport.ts
//! Also ports upstream/apps/web/src/state/v2ItemSupport.ts.

use std::sync::{Arc, LazyLock};

use crate::state::contracts::{
    OrchestrationV2Checkpoint, OrchestrationV2ContextHandoff, OrchestrationV2ContextTransfer,
    OrchestrationV2ExecutionNode, OrchestrationV2ProviderSession, OrchestrationV2ProviderThread,
    OrchestrationV2ProviderTurn, OrchestrationV2Run, OrchestrationV2RunAttempt,
    OrchestrationV2RuntimeRequest, OrchestrationV2Subagent, OrchestrationV2TurnItem, TurnItemId,
};
use crate::state::support::turn_item::TurnItemExt;
use crate::state::thread_projection::ThreadProjection;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct V2ItemSupport {
    pub item: Option<Arc<OrchestrationV2TurnItem>>,
    pub run: Option<Arc<OrchestrationV2Run>>,
    pub attempts: Vec<Arc<OrchestrationV2RunAttempt>>,
    pub node: Option<Arc<OrchestrationV2ExecutionNode>>,
    pub provider_session: Option<Arc<OrchestrationV2ProviderSession>>,
    pub provider_thread: Option<Arc<OrchestrationV2ProviderThread>>,
    pub provider_turn: Option<Arc<OrchestrationV2ProviderTurn>>,
    pub runtime_request: Option<Arc<OrchestrationV2RuntimeRequest>>,
    pub checkpoint: Option<Arc<OrchestrationV2Checkpoint>>,
    pub subagent: Option<Arc<OrchestrationV2Subagent>>,
    pub context_handoff: Option<Arc<OrchestrationV2ContextHandoff>>,
    pub context_transfer: Option<Arc<OrchestrationV2ContextTransfer>>,
}

/// The stable empty support, shared by every unknown item.
pub static EMPTY_V2_ITEM_SUPPORT: LazyLock<V2ItemSupport> = LazyLock::new(V2ItemSupport::default);

/// Resolves the relational V2 entities that enrich one projected item. The
/// caller supplies the item's source-thread projection, including for inherited
/// rows, so every returned entity retains its original projection identity.
pub fn resolve_v2_item_support(
    projection: &ThreadProjection,
    item_id: &TurnItemId,
) -> V2ItemSupport {
    let item = projection
        .turn_items
        .iter()
        .find(|candidate| candidate.id() == item_id)
        .cloned()
        .or_else(|| {
            projection
                .visible_turn_items
                .iter()
                .find(|candidate| candidate.source_item_id == *item_id)
                .map(|row| Arc::clone(&row.item))
        });
    let Some(item) = item else {
        return EMPTY_V2_ITEM_SUPPORT.clone();
    };

    let run = item.run_id().and_then(|run_id| {
        projection
            .runs
            .iter()
            .find(|candidate| candidate.id == *run_id)
            .cloned()
    });
    let attempts = item.run_id().map_or_else(Vec::new, |run_id| {
        projection
            .attempts
            .iter()
            .filter(|candidate| candidate.run_id == *run_id)
            .cloned()
            .collect()
    });
    let node = item.node_id().and_then(|node_id| {
        projection
            .nodes
            .iter()
            .find(|candidate| candidate.id == *node_id)
            .cloned()
    });
    let provider_thread = item.provider_thread_id().and_then(|id| {
        projection
            .provider_threads
            .iter()
            .find(|candidate| candidate.id == *id)
            .cloned()
    });
    let provider_session = provider_thread
        .as_ref()
        .and_then(|thread| thread.provider_session_id.as_ref())
        .and_then(|session_id| {
            projection
                .provider_sessions
                .iter()
                .find(|candidate| candidate.id == *session_id)
                .cloned()
        });
    let provider_turn = item.provider_turn_id().and_then(|id| {
        projection
            .provider_turns
            .iter()
            .find(|candidate| candidate.id == *id)
            .cloned()
    });
    let request_id = match &*item {
        OrchestrationV2TurnItem::ApprovalRequest { request_id, .. }
        | OrchestrationV2TurnItem::UserInputRequest { request_id, .. } => Some(request_id),
        _ => node
            .as_ref()
            .and_then(|node| node.runtime_request_id.as_ref()),
    };
    let runtime_request = request_id.and_then(|request_id| {
        projection
            .runtime_requests
            .iter()
            .find(|candidate| candidate.id == *request_id)
            .cloned()
    });
    let checkpoint = match &*item {
        OrchestrationV2TurnItem::Checkpoint { checkpoint_id, .. } => projection
            .checkpoints
            .iter()
            .find(|candidate| candidate.id == *checkpoint_id)
            .cloned(),
        _ => None,
    };
    let subagent = match &*item {
        OrchestrationV2TurnItem::Subagent { subagent_id, .. } => projection
            .subagents
            .iter()
            .find(|candidate| candidate.id == *subagent_id)
            .cloned(),
        _ => None,
    };
    let context_handoff = match &*item {
        OrchestrationV2TurnItem::Handoff {
            context_handoff_id, ..
        } => projection
            .context_handoffs
            .iter()
            .find(|candidate| candidate.id == *context_handoff_id)
            .cloned(),
        _ => None,
    };
    let context_transfer = context_handoff
        .as_ref()
        .and_then(|handoff| handoff.transfer_id.as_ref().and_then(|id| id.as_ref()))
        .and_then(|transfer_id| {
            projection
                .context_transfers
                .iter()
                .find(|candidate| candidate.id == *transfer_id)
                .cloned()
        });

    V2ItemSupport {
        item: Some(item),
        run,
        attempts,
        node,
        provider_session,
        provider_thread,
        provider_turn,
        runtime_request,
        checkpoint,
        subagent,
        context_handoff,
        context_transfer,
    }
}

fn same_entity<T>(left: &Option<Arc<T>>, right: &Option<Arc<T>>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
        _ => false,
    }
}

/// Compares support structurally while retaining entity identity semantics.
pub fn v2_item_support_equal(left: &V2ItemSupport, right: &V2ItemSupport) -> bool {
    same_entity(&left.item, &right.item)
        && same_entity(&left.run, &right.run)
        && left.attempts.len() == right.attempts.len()
        && left
            .attempts
            .iter()
            .zip(&right.attempts)
            .all(|(left, right)| Arc::ptr_eq(left, right))
        && same_entity(&left.node, &right.node)
        && same_entity(&left.provider_session, &right.provider_session)
        && same_entity(&left.provider_thread, &right.provider_thread)
        && same_entity(&left.provider_turn, &right.provider_turn)
        && same_entity(&left.runtime_request, &right.runtime_request)
        && same_entity(&left.checkpoint, &right.checkpoint)
        && same_entity(&left.subagent, &right.subagent)
        && same_entity(&left.context_handoff, &right.context_handoff)
        && same_entity(&left.context_transfer, &right.context_transfer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::OrchestrationV2ProjectedTurnItemVisibility;
    use crate::state::test_fixtures::{from_json, v2_projection};
    use crate::state::thread_projection::VisibleTurnItem;
    use serde_json::json;

    const NOW: &str = "2026-06-20T00:00:00.000Z";

    fn command_item() -> OrchestrationV2TurnItem {
        from_json(json!({
            "id": "item-1", "threadId": "thread-v2", "runId": "run-1", "nodeId": "node-1",
            "providerThreadId": "provider-thread-1", "providerTurnId": "provider-turn-1",
            "nativeItemRef": null, "parentItemId": null, "ordinal": 0, "status": "running",
            "title": null, "startedAt": NOW, "completedAt": null, "updatedAt": NOW,
            "type": "command_execution", "input": "vp check",
        }))
    }

    fn attempt(id: &str, ordinal: u64, status: &str, reason: &str) -> OrchestrationV2RunAttempt {
        from_json(json!({
            "id": id, "runId": "run-1", "attemptOrdinal": ordinal, "rootNodeId": "node-1",
            "providerInstanceId": "codex", "providerThreadId": "provider-thread-1",
            "providerTurnId": "provider-turn-1", "reason": reason, "status": status,
            "startedAt": NOW, "completedAt": null,
        }))
    }

    #[test]
    fn retains_identity_while_linking_a_turn_item_to_its_execution_and_provider_entities() {
        let mut contract = v2_projection();
        contract.runs = vec![from_json(json!({
            "id": "run-1", "threadId": "thread-v2", "ordinal": 1, "providerInstanceId": "codex",
            "modelSelection": { "instanceId": "codex", "model": "gpt-5.4" },
            "providerThreadId": "provider-thread-1", "userMessageId": "message-1",
            "rootNodeId": "node-1", "activeAttemptId": "attempt-2", "status": "running",
            "requestedAt": NOW, "startedAt": NOW, "completedAt": null, "checkpointId": null,
            "contextHandoffId": null,
        }))];
        contract.attempts = vec![
            attempt("attempt-1", 1, "superseded", "initial"),
            attempt("attempt-2", 2, "running", "steering_restart"),
        ];
        contract.nodes = vec![from_json(json!({
            "id": "node-1", "threadId": "thread-v2", "runId": "run-1", "parentNodeId": null,
            "rootNodeId": "node-1", "kind": "tool_call", "status": "running", "countsForRun": true,
            "providerThreadId": "provider-thread-1", "providerTurnId": "provider-turn-1",
            "nativeItemRef": null, "runtimeRequestId": "request-1", "checkpointScopeId": null,
            "startedAt": NOW, "completedAt": null,
        }))];
        contract.provider_threads = vec![from_json(json!({
            "id": "provider-thread-1", "driver": "codex", "providerInstanceId": "codex",
            "providerSessionId": null, "appThreadId": "thread-v2", "ownerNodeId": "node-1",
            "nativeThreadRef": null, "nativeConversationHeadRef": null, "status": "active",
            "firstRunOrdinal": 1, "lastRunOrdinal": 1, "handoffIds": [], "forkedFrom": null,
            "createdAt": NOW, "updatedAt": NOW,
        }))];
        contract.provider_turns = vec![from_json(json!({
            "id": "provider-turn-1", "providerThreadId": "provider-thread-1", "nodeId": "node-1",
            "runAttemptId": "attempt-2", "nativeTurnRef": null, "ordinal": 1, "status": "running",
            "startedAt": NOW, "completedAt": null,
        }))];
        contract.runtime_requests = vec![from_json(json!({
            "id": "request-1", "nodeId": "node-1", "providerTurnId": "provider-turn-1",
            "nativeRequestRef": null, "kind": "dynamic_tool_call", "status": "pending",
            "responseCapability": { "type": "live", "providerSessionId": "session-1" },
            "createdAt": NOW, "resolvedAt": null,
        }))];
        contract.turn_items = vec![command_item()];
        let projection = ThreadProjection::from(contract);

        let support = resolve_v2_item_support(&projection, &TurnItemId::new("item-1").unwrap());
        assert!(Arc::ptr_eq(
            support.item.as_ref().unwrap(),
            &projection.turn_items[0]
        ));
        assert!(Arc::ptr_eq(
            support.run.as_ref().unwrap(),
            &projection.runs[0]
        ));
        assert_eq!(support.attempts.len(), 2);
        assert!(Arc::ptr_eq(&support.attempts[0], &projection.attempts[0]));
        assert!(Arc::ptr_eq(
            support.node.as_ref().unwrap(),
            &projection.nodes[0]
        ));
        assert!(Arc::ptr_eq(
            support.provider_thread.as_ref().unwrap(),
            &projection.provider_threads[0]
        ));
        assert!(Arc::ptr_eq(
            support.provider_turn.as_ref().unwrap(),
            &projection.provider_turns[0]
        ));
        assert!(Arc::ptr_eq(
            support.runtime_request.as_ref().unwrap(),
            &projection.runtime_requests[0]
        ));
    }

    #[test]
    fn resolves_synthetic_items_from_the_authoritative_visible_sequence() {
        let item = Arc::new(command_item());
        let mut projection = ThreadProjection::from(v2_projection());
        projection.visible_turn_items = Arc::new(vec![Arc::new(VisibleTurnItem {
            position: 0,
            visibility: OrchestrationV2ProjectedTurnItemVisibility::Synthetic,
            source_thread_id: projection.thread.id.clone(),
            source_item_id: item.id().clone(),
            item: Arc::clone(&item),
        })]);
        let support = resolve_v2_item_support(&projection, &TurnItemId::new("item-1").unwrap());
        assert!(Arc::ptr_eq(support.item.as_ref().unwrap(), &item));
    }

    #[test]
    fn returns_the_stable_empty_support_for_unknown_items() {
        let projection = ThreadProjection::from(v2_projection());
        let support = resolve_v2_item_support(&projection, &TurnItemId::new("item-1").unwrap());
        assert_eq!(support, *EMPTY_V2_ITEM_SUPPORT);
    }

    #[test]
    fn compares_support_while_retaining_entity_identity_semantics() {
        assert!(v2_item_support_equal(
            &EMPTY_V2_ITEM_SUPPORT,
            &EMPTY_V2_ITEM_SUPPORT.clone()
        ));
        let other = V2ItemSupport {
            attempts: vec![Arc::new(attempt("attempt", 1, "running", "initial"))],
            ..V2ItemSupport::default()
        };
        assert!(!v2_item_support_equal(&EMPTY_V2_ITEM_SUPPORT, &other));
    }
}
