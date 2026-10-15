//! Node, message and plan records that upstream's OpenCode adapters emit beside each
//! turn item: `node.updated` (`emitTextPart`, `emitToolPart`, `emitTodo` in
//! `server/adapter.ts`), `message.updated` for assistant text, and `plan.updated` for a
//! todo list. Both generations build them here, so the field mapping lives in one place.

use turin_contracts::{
    NodeId, PlanId, ProviderThreadId, ProviderTurnId, ThreadId, TrimmedNonEmptyString, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2ConversationMessage,
        OrchestrationV2ConversationMessageRole, OrchestrationV2CreationSource,
        OrchestrationV2ExecutionNode, OrchestrationV2ExecutionNodeKind,
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2PlanArtifact,
        OrchestrationV2PlanArtifactStatus, OrchestrationV2PlanStep, OrchestrationV2PlanStepStatus,
        OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
    },
    provider_instance::ProviderDriverKind,
};
use turin_provider_core::id_allocator::derive;

use crate::adapter::logic::native_ref;

/// `dateTimeFromEpoch`: a provider timestamp in epoch milliseconds, or the fallback.
pub fn epoch_ms_or(value: Option<i64>, fallback: UtcDateTime) -> UtcDateTime {
    value
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(UtcDateTime::from_datetime)
        .unwrap_or(fallback)
}

/// The identity a node is built from: the item it stands for and the turn it belongs to.
pub struct NodeOf<'a> {
    pub thread_id: &'a ThreadId,
    pub root_node_id: &'a NodeId,
    pub provider_thread_id: &'a ProviderThreadId,
    pub provider_turn_id: &'a ProviderTurnId,
    /// The native item id the node id derives from.
    pub native_item_id: &'a str,
    /// A weak ref when the id is synthesised (a todo list has no native part id).
    pub weak_ref: bool,
}

pub struct NodeTiming {
    pub kind: OrchestrationV2ExecutionNodeKind,
    pub status: OrchestrationV2ExecutionNodeStatus,
    pub started_at: UtcDateTime,
    pub completed_at: Option<UtcDateTime>,
}

/// `node.updated` for one item. `countsForRun` is false for every OpenCode item.
pub fn execution_node(
    driver: &ProviderDriverKind,
    of: &NodeOf<'_>,
    timing: NodeTiming,
) -> OrchestrationV2ExecutionNode {
    OrchestrationV2ExecutionNode {
        id: derive::node_from_provider_item(driver, of.native_item_id),
        thread_id: of.thread_id.clone(),
        run_id: None,
        parent_node_id: Some(of.root_node_id.clone()),
        root_node_id: of.root_node_id.clone(),
        kind: timing.kind,
        status: timing.status,
        counts_for_run: false,
        provider_thread_id: Some(of.provider_thread_id.clone()),
        provider_turn_id: Some(of.provider_turn_id.clone()),
        native_item_ref: Some(native_ref(of.native_item_id, of.weak_ref)),
        runtime_request_id: None,
        checkpoint_scope_id: None,
        started_at: Some(timing.started_at),
        completed_at: timing.completed_at,
    }
}

/// The assistant message a text part projects as (`emitTextPart`, text parts only).
pub fn assistant_message(
    driver: &ProviderDriverKind,
    of: &NodeOf<'_>,
    node_id: &NodeId,
    text: &str,
    streaming: bool,
    created_at: UtcDateTime,
    updated_at: UtcDateTime,
) -> OrchestrationV2ConversationMessage {
    OrchestrationV2ConversationMessage {
        notification: None,
        created_by: OrchestrationV2Actor::Agent,
        creation_source: OrchestrationV2CreationSource::Provider,
        scheduled_task_id: None,
        sender_thread_id: None,
        id: derive::message_from_provider_item(driver, of.native_item_id),
        thread_id: of.thread_id.clone(),
        run_id: None,
        node_id: Some(node_id.clone()),
        role: OrchestrationV2ConversationMessageRole::Assistant,
        text: text.to_owned(),
        context: None,
        attachments: Vec::new(),
        streaming,
        created_at,
        updated_at,
        delegated_completion: None,
    }
}

/// `plan.updated` for a todo list, `kind: "todo_list"`. The status maps from the
/// `completed` flag the way `emitTodo` does.
pub fn todo_plan(
    plan_id: &PlanId,
    thread_id: &ThreadId,
    node_id: &NodeId,
    completed: bool,
    steps: Vec<OrchestrationV2PlanStep>,
) -> OrchestrationV2PlanArtifact {
    OrchestrationV2PlanArtifact::TodoList {
        id: plan_id.clone(),
        thread_id: thread_id.clone(),
        run_id: None,
        node_id: node_id.clone(),
        status: if completed {
            OrchestrationV2PlanArtifactStatus::Completed
        } else {
            OrchestrationV2PlanArtifactStatus::Active
        },
        detail_in_turn_item: None,
        steps,
        explanation: None,
    }
}

/// One todo step, as `emitTodo` builds it from a `todo.updated` entry.
pub fn todo_steps(
    native_item_id: &str,
    todos: &[(String, String)],
) -> Vec<OrchestrationV2PlanStep> {
    todos
        .iter()
        .enumerate()
        .map(|(index, (content, status))| {
            let trimmed = content.trim();
            let text = if trimmed.is_empty() {
                format!("Todo {}", index + 1)
            } else {
                trimmed.to_owned()
            };
            OrchestrationV2PlanStep {
                id: TrimmedNonEmptyString::new(format!("{native_item_id}:{}", index + 1))
                    .expect("todo step id"),
                text: TrimmedNonEmptyString::new(text).expect("todo step text"),
                status: match status.as_str() {
                    "completed" => OrchestrationV2PlanStepStatus::Completed,
                    "in_progress" => OrchestrationV2PlanStepStatus::Running,
                    _ => OrchestrationV2PlanStepStatus::Pending,
                },
                duration_anchor_at: None,
                duration_ms: None,
            }
        })
        .collect()
}

/// The todo list's native id. Upstream is `${sessionId}:todo:${providerTurnId}`.
pub fn todo_native_id(session_id: &str, provider_turn_id: &str) -> String {
    format!("{session_id}:todo:{provider_turn_id}")
}

/// `completed` for a todo list: at least one step and all of them done.
pub fn todos_complete(steps: &[OrchestrationV2PlanStep]) -> bool {
    !steps.is_empty()
        && steps
            .iter()
            .all(|step| step.status == OrchestrationV2PlanStepStatus::Completed)
}

/// One text or reasoning block's turn item, as `emitText` and `emitTextPart` build it.
pub struct TextBlock<'a> {
    pub reasoning: bool,
    pub text: &'a str,
    pub streaming: bool,
    pub status: OrchestrationV2TurnItemStatus,
    pub ordinal: u64,
    pub started_at: UtcDateTime,
    pub completed_at: Option<UtcDateTime>,
    pub updated_at: UtcDateTime,
}

pub fn text_turn_item(
    driver: &ProviderDriverKind,
    of: &NodeOf<'_>,
    node_id: &NodeId,
    block: TextBlock<'_>,
) -> OrchestrationV2TurnItem {
    let id = derive::turn_item_from_provider_item(driver, of.native_item_id);
    let native_item_ref = Some(native_ref(of.native_item_id, of.weak_ref));
    let provider_thread_id = Some(of.provider_thread_id.clone());
    let provider_turn_id = Some(of.provider_turn_id.clone());
    if block.reasoning {
        return OrchestrationV2TurnItem::Reasoning {
            tool_non_execution_kind: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            id,
            thread_id: of.thread_id.clone(),
            run_id: None,
            node_id: Some(node_id.clone()),
            provider_thread_id,
            provider_turn_id,
            native_item_ref,
            parent_item_id: None,
            ordinal: block.ordinal,
            status: block.status,
            title: None,
            started_at: Some(block.started_at),
            completed_at: block.completed_at,
            updated_at: block.updated_at,
            text: block.text.to_owned(),
            streaming: block.streaming,
        };
    }
    OrchestrationV2TurnItem::AssistantMessage {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id,
        thread_id: of.thread_id.clone(),
        run_id: None,
        node_id: Some(node_id.clone()),
        provider_thread_id,
        provider_turn_id,
        native_item_ref,
        parent_item_id: None,
        ordinal: block.ordinal,
        status: block.status,
        title: None,
        started_at: Some(block.started_at),
        completed_at: block.completed_at,
        updated_at: block.updated_at,
        message_id: derive::message_from_provider_item(driver, of.native_item_id),
        text: block.text.to_owned(),
        attachments: None,
        streaming: block.streaming,
    }
}

/// `ordinalOf` / `itemOrdinal`: a native item keeps the ordinal it got on first sight.
pub fn next_ordinal(
    ordinals: &mut std::collections::HashMap<String, u64>,
    next: &mut u64,
    native_item_id: &str,
) -> u64 {
    if let Some(ordinal) = ordinals.get(native_item_id) {
        return *ordinal;
    }
    let ordinal = *next;
    *next += 1;
    ordinals.insert(native_item_id.to_owned(), ordinal);
    ordinal
}

/// The node status an item status settles into (`toolStatus`, `emitTool`).
pub fn node_status_of(status: OrchestrationV2TurnItemStatus) -> OrchestrationV2ExecutionNodeStatus {
    match status {
        OrchestrationV2TurnItemStatus::Pending => OrchestrationV2ExecutionNodeStatus::Pending,
        OrchestrationV2TurnItemStatus::Completed => OrchestrationV2ExecutionNodeStatus::Completed,
        OrchestrationV2TurnItemStatus::Failed => OrchestrationV2ExecutionNodeStatus::Failed,
        _ => OrchestrationV2ExecutionNodeStatus::Running,
    }
}
