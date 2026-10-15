//! Port of the child-thread half of upstream/apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts:
//! `claudeSubagentIds` (~4256), `emitSubagentConversationMessage` (~4621), the
//! subagent's node, child root node and turn item in `updateClaudeSubagentNode`
//! (~5040-5420), and `toolCallsFor` (~5570), which sends a subagent's own text
//! and tool calls to its child thread instead of the parent turn.
//!
//! The lifecycle (launch, status, result) stays in `claude_subagents.rs`; this
//! module only builds the events for it.
use std::collections::HashMap;

use serde_json::{Value, json};
use turin_contracts::{
    NodeId, ProviderThreadId, ProviderTurnId, ThreadId, TurnItemId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2ConversationMessageRole, OrchestrationV2ExecutionNode,
        OrchestrationV2ProviderRef, OrchestrationV2Subagent, OrchestrationV2SubagentStatus,
        OrchestrationV2TurnItem,
    },
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
};
use turin_provider_core::{
    id_allocator::derive,
    provider_adapter::ProviderAdapterV2Event,
    subagent_projection::{
        SubagentConversationArtifactsInput, make_subagent_conversation_artifacts,
    },
};

use crate::{
    claude_tool_titles::ToolPresentation,
    claude_turn_items::{ToolCall, TurnScope, tool_call_artifacts},
};

/// The launch prompt is the child thread's first item; the rest follow it.
const CHILD_ITEM_ORDINAL_START: u64 = 100;

/// The ids a subagent is known by (upstream `claudeSubagentIds`).
#[derive(Debug, Clone)]
pub struct SubagentIds {
    /// The subagent's execution node in the parent thread.
    pub node_id: NodeId,
    /// The root node of the child thread.
    pub child_root_node_id: NodeId,
    pub child_thread_id: ThreadId,
    /// The subagent's turn item in the parent thread.
    pub turn_item_id: TurnItemId,
}

pub fn subagent_ids(
    driver: &ProviderDriverKind,
    provider_thread_id: &ProviderThreadId,
    task_id: &str,
) -> SubagentIds {
    SubagentIds {
        node_id: derive::node_from_provider_item(driver, &format!("task:{task_id}")),
        child_root_node_id: derive::node_from_provider_item(
            driver,
            &format!("task:{task_id}:thread-root"),
        ),
        child_thread_id: derive::thread_from_provider_thread(
            driver,
            None,
            &format!("{}:{task_id}", provider_thread_id.as_str()),
        ),
        turn_item_id: derive::turn_item_from_provider_item(
            driver,
            &format!("task:{task_id}:subagent"),
        ),
    }
}

/// A tool call the subagent made, until its tool_result arrives.
pub struct ChildTool {
    pub name: String,
    pub input: Value,
    pub presentation: ToolPresentation,
    pub ordinal: u64,
    pub started_at: UtcDateTime,
    pub finished: bool,
}

/// Upstream `ActiveClaudeSubagent`'s child-thread fields: where the subagent's
/// items land and how they are numbered.
pub struct ChildThread {
    pub thread_id: ThreadId,
    pub root_node_id: NodeId,
    next_ordinal: u64,
    ordinals: HashMap<String, u64>,
    pub tools: HashMap<String, ChildTool>,
    /// The subagent's latest text. Its result repeats that text on completion.
    pub last_assistant_text: Option<String>,
}

impl ChildThread {
    pub fn new(thread_id: ThreadId, root_node_id: NodeId) -> Self {
        Self {
            thread_id,
            root_node_id,
            next_ordinal: CHILD_ITEM_ORDINAL_START,
            ordinals: HashMap::new(),
            tools: HashMap::new(),
            last_assistant_text: None,
        }
    }

    /// Upstream `nextChildItemOrdinal`: a native item takes the next ordinal on
    /// first sight and keeps it.
    pub fn ordinal(&mut self, native_item_id: &str) -> u64 {
        if let Some(ordinal) = self.ordinals.get(native_item_id) {
            return *ordinal;
        }
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        self.ordinals.insert(native_item_id.to_owned(), ordinal);
        ordinal
    }
}

/// The scope child items are built in: the child thread's root, with no run
/// (upstream `runId: null`), inside the launching provider turn.
pub fn child_turn_scope(
    driver: &ProviderDriverKind,
    child: &ChildThread,
    provider_thread_id: &ProviderThreadId,
    provider_turn_id: &ProviderTurnId,
) -> TurnScope {
    TurnScope {
        driver: driver.clone(),
        thread_id: child.thread_id.clone(),
        run_id: None,
        root_node_id: child.root_node_id.clone(),
        provider_thread_id: provider_thread_id.clone(),
        provider_turn_id: provider_turn_id.clone(),
    }
}

pub fn native_ref(driver: &ProviderDriverKind, native_item_id: &str) -> OrchestrationV2ProviderRef {
    serde_json::from_value(json!({
        "driver": driver, "nativeId": native_item_id, "strength": "strong",
    }))
    .expect("native ref matches the contract shape")
}

/// One message for a child thread: its native item, its sender, and its text.
pub struct ChildMessage<'a> {
    pub native_item_id: &'a str,
    /// The parent thread, for the launch prompt and a resume's message.
    pub sender: Option<&'a ThreadId>,
    pub role: OrchestrationV2ConversationMessageRole,
    pub text: &'a str,
    pub ordinal: u64,
}

/// Upstream `emitSubagentConversationMessage`: a message in the child thread
/// and its turn item.
pub fn conversation_events(
    driver: &ProviderDriverKind,
    child: &ChildThread,
    message: ChildMessage<'_>,
    now: UtcDateTime,
) -> Vec<ProviderAdapterV2Event> {
    let native_item_id = message.native_item_id;
    let artifacts = make_subagent_conversation_artifacts(SubagentConversationArtifactsInput {
        message_id: derive::message_from_provider_item(driver, native_item_id),
        sender_thread_id: message.sender.cloned(),
        turn_item_id: derive::turn_item_from_provider_item(driver, native_item_id),
        thread_id: child.thread_id.clone(),
        root_node_id: child.root_node_id.clone(),
        provider_thread_id: None,
        provider_turn_id: None,
        native_item_ref: Some(native_ref(driver, native_item_id)),
        role: message.role,
        text: message.text.to_owned(),
        ordinal: message.ordinal,
        now,
    });
    vec![
        ProviderAdapterV2Event::MessageUpdated {
            driver: driver.clone(),
            message: artifacts.message,
        },
        ProviderAdapterV2Event::TurnItemUpdated {
            driver: driver.clone(),
            turn_item: artifacts.turn_item,
        },
    ]
}

/// The subagent's execution node in the parent thread, and its child root node.
/// Upstream `node.updated` pair emitted on every lifecycle change.
pub struct SubagentNodeInput<'a> {
    pub driver: &'a ProviderDriverKind,
    pub subagent: &'a OrchestrationV2Subagent,
    pub ids: &'a SubagentIds,
    pub root_node_id: &'a NodeId,
    pub provider_thread_id: &'a ProviderThreadId,
    pub provider_turn_id: &'a ProviderTurnId,
}

pub fn subagent_node_events(input: SubagentNodeInput<'_>) -> Vec<ProviderAdapterV2Event> {
    let task = input.subagent;
    let status = to_value(&task.status);
    let native = &task.native_task_ref;
    let subagent_node: OrchestrationV2ExecutionNode = serde_json::from_value(json!({
        "id": input.ids.node_id, "threadId": task.thread_id, "runId": task.run_id,
        "parentNodeId": task.parent_node_id, "rootNodeId": input.root_node_id,
        "kind": "subagent", "status": status, "countsForRun": false,
        "providerThreadId": input.provider_thread_id, "providerTurnId": input.provider_turn_id,
        "nativeItemRef": native, "runtimeRequestId": null, "checkpointScopeId": null,
        "startedAt": task.started_at, "completedAt": task.completed_at,
    }))
    .expect("subagent node matches the contract shape");
    let child_node: OrchestrationV2ExecutionNode = serde_json::from_value(json!({
        "id": input.ids.child_root_node_id, "threadId": input.ids.child_thread_id,
        "runId": null, "parentNodeId": null, "rootNodeId": input.ids.child_root_node_id,
        "kind": "root_turn", "status": status, "countsForRun": false,
        "providerThreadId": null, "providerTurnId": null,
        "nativeItemRef": task.native_task_ref, "runtimeRequestId": null,
        "checkpointScopeId": null,
        "startedAt": task.started_at, "completedAt": task.completed_at,
    }))
    .expect("child root node matches the contract shape");
    vec![
        ProviderAdapterV2Event::NodeUpdated {
            driver: input.driver.clone(),
            node: subagent_node,
        },
        ProviderAdapterV2Event::NodeUpdated {
            driver: input.driver.clone(),
            node: child_node,
        },
    ]
}

/// The subagent's turn item in the parent thread (upstream `type: "subagent"`).
pub fn subagent_turn_item(
    driver: &ProviderDriverKind,
    subagent: &OrchestrationV2Subagent,
    ids: &SubagentIds,
    ordinal: u64,
    provider_thread_id: &ProviderThreadId,
    provider_turn_id: &ProviderTurnId,
    provider_instance_id: &ProviderInstanceId,
) -> ProviderAdapterV2Event {
    let turn_item: OrchestrationV2TurnItem = serde_json::from_value(json!({
        "type": "subagent", "id": ids.turn_item_id, "threadId": subagent.thread_id,
        "runId": subagent.run_id, "nodeId": ids.node_id,
        "providerThreadId": provider_thread_id, "providerTurnId": provider_turn_id,
        "nativeItemRef": subagent.native_task_ref, "parentItemId": null,
        "ordinal": ordinal, "status": to_value(&subagent.status),
        "title": subagent.title, "startedAt": subagent.started_at,
        "completedAt": subagent.completed_at, "updatedAt": subagent.updated_at,
        "subagentId": ids.node_id, "origin": to_value(&subagent.origin),
        "driver": driver, "providerInstanceId": provider_instance_id,
        "childThreadId": ids.child_thread_id, "prompt": subagent.prompt,
        "progress": subagent.progress, "result": subagent.result,
    }))
    .expect("subagent turn item matches the contract shape");
    ProviderAdapterV2Event::TurnItemUpdated {
        driver: driver.clone(),
        turn_item,
    }
}

/// The ToolCall view of a tracked child tool, finished with `output`.
pub fn child_tool_call<'a>(
    native_item_id: &'a str,
    tool: &'a ChildTool,
    output: Option<&'a Value>,
    output_fallback: Option<&'a Value>,
    status: &'static str,
    now: UtcDateTime,
) -> ToolCall<'a> {
    ToolCall {
        native_item_id,
        tool_name: &tool.name,
        input: &tool.input,
        presentation: &tool.presentation,
        output,
        output_fallback,
        status,
        ordinal: tool.ordinal,
        started_at: tool.started_at,
        updated_at: now,
    }
}

/// The node and turn item of a child tool call, as events.
pub fn tool_call_events(scope: &TurnScope, call: &ToolCall<'_>) -> Vec<ProviderAdapterV2Event> {
    let artifacts = tool_call_artifacts(scope, call);
    vec![
        ProviderAdapterV2Event::NodeUpdated {
            driver: scope.driver.clone(),
            node: artifacts.node,
        },
        ProviderAdapterV2Event::TurnItemUpdated {
            driver: scope.driver.clone(),
            turn_item: artifacts.turn_item,
        },
    ]
}

/// Upstream `normalizeClaudeResultText`: whitespace runs collapse to one space.
pub fn normalize_result_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn to_value(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).expect("contract enum serializes")
}

/// Terminal status for an open child tool: a cancelled subagent interrupts
/// its calls, anything else fails them (upstream `endToolCalls`).
pub fn open_tool_status(status: OrchestrationV2SubagentStatus) -> &'static str {
    match status {
        OrchestrationV2SubagentStatus::Cancelled => "interrupted",
        _ => "failed",
    }
}
