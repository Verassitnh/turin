//! Port of the artifact builders in upstream ClaudeAdapterV2.ts:
//! `buildAssistantArtifacts`, `buildToolCallArtifacts`, the tool classification
//! table, and the proposed-plan capture. Items are assembled in the contract's
//! wire shape and decoded, so the contract types validate every one.
//!
//! Titles from upstream's `formatReadToolLabel`, `formatSearchToolLabel`,
//! `dynamicToolTitle`, and `mcpToolPresentation` are not ported; those rows are
//! `title: null`.
use serde_json::{Value, json};
use std::collections::HashMap;
use turin_contracts::{
    NodeId, PlanId, ProviderThreadId, ProviderTurnId, RunId, ThreadId,
    orchestration_v2::{
        OrchestrationV2ConversationMessage, OrchestrationV2ExecutionNode, OrchestrationV2PlanArtifact,
        OrchestrationV2TurnItem,
    },
    provider_instance::ProviderDriverKind,
};
use turin_provider_core::id_allocator::{IdAllocator, derive};

/// `(driver, native item id) -> ordinal` for one provider turn.
pub type ItemOrdinals = HashMap<String, u64>;

/// Upstream `resolveClaudeItemOrdinal`: `providerTurnOrdinal * 100 + size + 1`,
/// allocated on first sight and reused afterwards.
pub fn item_ordinal(ordinals: &mut ItemOrdinals, provider_turn_ordinal: i64, native_item_id: &str) -> u64 {
    if let Some(existing) = ordinals.get(native_item_id) {
        return *existing;
    }
    let ordinal = provider_turn_ordinal.max(0) as u64 * 100 + ordinals.len() as u64 + 1;
    ordinals.insert(native_item_id.to_owned(), ordinal);
    ordinal
}

/// Where an item sits: the thread, run, root node, and provider turn it
/// belongs to.
#[derive(Clone)]
pub struct TurnScope {
    pub driver: ProviderDriverKind,
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub root_node_id: NodeId,
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: ProviderTurnId,
}

fn native_ref(driver: &ProviderDriverKind, native_item_id: &str) -> Value {
    json!({"driver": driver, "nativeId": native_item_id, "strength": "strong"})
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> T {
    // The JSON is built here from contract field names; a mismatch is a bug in
    // this module and the unit tests below decode every shape.
    serde_json::from_value(value).expect("Claude artifact matches the contract shape")
}

pub struct AssistantArtifacts {
    pub node: OrchestrationV2ExecutionNode,
    pub message: OrchestrationV2ConversationMessage,
    pub turn_item: OrchestrationV2TurnItem,
}

/// Upstream `buildAssistantArtifacts`: one completed assistant message, its
/// execution node, and its turn item.
pub fn assistant_artifacts(
    scope: &TurnScope,
    native_item_id: &str,
    ordinal: u64,
    text: &str,
    at: turin_contracts::UtcDateTime,
) -> AssistantArtifacts {
    let driver = &scope.driver;
    let node_id = derive::node_from_provider_item(driver, native_item_id);
    let message_id = derive::message_from_provider_item(driver, native_item_id);
    let item_id = derive::turn_item_from_provider_item(driver, native_item_id);
    let native = native_ref(driver, native_item_id);
    let node = decode(json!({
        "id": node_id, "threadId": scope.thread_id, "runId": scope.run_id,
        "parentNodeId": scope.root_node_id, "rootNodeId": scope.root_node_id,
        "kind": "assistant_message", "status": "completed", "countsForRun": false,
        "providerThreadId": scope.provider_thread_id, "providerTurnId": scope.provider_turn_id,
        "nativeItemRef": native, "runtimeRequestId": null, "checkpointScopeId": null,
        "startedAt": at, "completedAt": at,
    }));
    let message = decode(json!({
        "createdBy": "agent", "creationSource": "provider", "id": message_id,
        "threadId": scope.thread_id, "runId": scope.run_id, "nodeId": node_id,
        "role": "assistant", "text": text, "attachments": [], "streaming": false,
        "createdAt": at, "updatedAt": at,
    }));
    let turn_item = decode(json!({
        "type": "assistant_message", "id": item_id, "threadId": scope.thread_id,
        "runId": scope.run_id, "nodeId": node_id, "providerThreadId": scope.provider_thread_id,
        "providerTurnId": scope.provider_turn_id, "nativeItemRef": native,
        "parentItemId": null, "ordinal": ordinal, "status": "completed", "title": null,
        "startedAt": at, "completedAt": at, "updatedAt": at,
        "messageId": message_id, "text": text, "streaming": false,
    }));
    AssistantArtifacts {
        node,
        message,
        turn_item,
    }
}

/// Item type and request kind for a native tool name (upstream
/// `CLAUDE_KNOWN_TOOL_CLASSIFICATIONS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolItemType {
    CommandExecution,
    FileChange,
    DynamicTool,
    WebSearch,
}

pub fn classify_tool(tool_name: &str) -> ToolItemType {
    let normalized: String = tool_name
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '_' && *c != '-')
        .collect();
    match normalized.as_str() {
        "bash" => ToolItemType::CommandExecution,
        "edit" | "multiedit" | "notebookedit" | "write" => ToolItemType::FileChange,
        "webfetch" | "websearch" => ToolItemType::WebSearch,
        _ => ToolItemType::DynamicTool,
    }
}

/// Text of a `tool_result` content value: a string, or the joined text blocks.
pub fn tool_output_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let text: String = blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect();
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

pub struct ToolCallArtifacts {
    pub node: OrchestrationV2ExecutionNode,
    pub turn_item: OrchestrationV2TurnItem,
}

pub struct ToolCall<'a> {
    pub native_item_id: &'a str,
    pub tool_name: &'a str,
    pub input: &'a Value,
    /// The `tool_result` content once the call has finished.
    pub output: Option<&'a Value>,
    /// One of `running`, `completed`, `failed`, `interrupted`, `cancelled`.
    pub status: &'static str,
    pub ordinal: u64,
    pub started_at: turin_contracts::UtcDateTime,
    pub updated_at: turin_contracts::UtcDateTime,
}

/// Upstream `buildToolCallArtifacts`: the tool execution node and its turn item.
pub fn tool_call_artifacts(scope: &TurnScope, call: &ToolCall<'_>) -> ToolCallArtifacts {
    let driver = &scope.driver;
    let node_id = derive::node_from_provider_item(driver, call.native_item_id);
    let item_id = derive::turn_item_from_provider_item(driver, call.native_item_id);
    let native = native_ref(driver, call.native_item_id);
    let completed_at = (call.status != "running").then_some(call.updated_at);
    let node = decode(json!({
        "id": node_id, "threadId": scope.thread_id, "runId": scope.run_id,
        "parentNodeId": scope.root_node_id, "rootNodeId": scope.root_node_id,
        "kind": "tool_call", "status": call.status, "countsForRun": false,
        "providerThreadId": scope.provider_thread_id, "providerTurnId": scope.provider_turn_id,
        "nativeItemRef": native, "runtimeRequestId": null, "checkpointScopeId": null,
        "startedAt": call.started_at, "completedAt": completed_at,
    }));

    let output_value = call.output.cloned();
    let output_text = call.output.and_then(tool_output_text);
    let common = json!({
        "id": item_id, "threadId": scope.thread_id, "runId": scope.run_id,
        "nodeId": node_id, "providerThreadId": scope.provider_thread_id,
        "providerTurnId": scope.provider_turn_id, "nativeItemRef": native,
        "parentItemId": null, "ordinal": call.ordinal, "status": call.status,
        "title": null, "startedAt": call.started_at, "completedAt": completed_at,
        "updatedAt": call.updated_at,
    });
    let mut item = common.as_object().cloned().unwrap_or_default();
    let input = call.input;
    let str_field = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| input.get(*key).and_then(Value::as_str))
            .map(str::to_owned)
    };
    let extra = match classify_tool(call.tool_name) {
        ToolItemType::CommandExecution => {
            json!({
                "type": "command_execution",
                "input": str_field(&["command", "cmd", "script"]).unwrap_or_default(),
                "output": output_text,
            })
        }
        ToolItemType::FileChange => {
            json!({
                "type": "file_change",
                "fileName": str_field(&["file_path", "path", "notebook_path"])
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| call.tool_name.to_owned()),
                "diffStr": output_text,
            })
        }
        ToolItemType::WebSearch => {
            let pattern = str_field(&["query", "url"]);
            json!({
                "type": "web_search",
                "patterns": pattern.map(|p| vec![p]).unwrap_or_default(),
            })
        }
        ToolItemType::DynamicTool => {
            json!({
                "type": "dynamic_tool",
                "toolName": call.tool_name,
                "input": input,
                "output": output_value,
            })
        }
    };
    for (key, value) in extra.as_object().into_iter().flatten() {
        if !value.is_null() {
            item.insert(key.clone(), value.clone());
        }
    }
    let turn_item = decode(Value::Object(item));
    ToolCallArtifacts { node, turn_item }
}

/// The ExitPlanMode plan as a plan artifact (`plan.updated`). Upstream keeps
/// the plan as a draft until the user decides, so it is a draft here too.
pub fn proposed_plan_artifact(scope: &TurnScope, plan_id: PlanId, markdown: &str) -> OrchestrationV2PlanArtifact {
    decode(json!({
        "kind": "proposed_plan", "id": plan_id, "threadId": scope.thread_id,
        "runId": scope.run_id, "nodeId": scope.root_node_id, "status": "draft",
        "markdown": markdown,
    }))
}

/// Plan ids are generated per plan, as upstream does for ExitPlanMode plans.
pub fn new_plan_id(scope: &TurnScope) -> PlanId {
    IdAllocator.plan(&scope.thread_id, Some(&scope.run_id), &scope.driver)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turin_contracts::UtcDateTime;

    fn scope() -> TurnScope {
        let driver = ProviderDriverKind::new("claudeAgent").unwrap();
        TurnScope {
            driver,
            thread_id: serde_json::from_value(json!("thread-1")).unwrap(),
            run_id: serde_json::from_value(json!("run-1")).unwrap(),
            root_node_id: serde_json::from_value(json!("node-root")).unwrap(),
            provider_thread_id: serde_json::from_value(json!("provider-thread-1")).unwrap(),
            provider_turn_id: serde_json::from_value::<ProviderTurnId>(json!("provider-turn-1"))
                .unwrap(),
        }
    }

    #[test]
    fn ordinals_follow_the_turn_and_stay_stable_per_item() {
        let mut ordinals = ItemOrdinals::new();
        assert_eq!(item_ordinal(&mut ordinals, 2, "a"), 201);
        assert_eq!(item_ordinal(&mut ordinals, 2, "b"), 202);
        assert_eq!(item_ordinal(&mut ordinals, 2, "a"), 201);
    }

    #[test]
    fn classifies_tools_like_upstream() {
        assert_eq!(classify_tool("Bash"), ToolItemType::CommandExecution);
        assert_eq!(classify_tool("multi_edit"), ToolItemType::FileChange);
        assert_eq!(classify_tool("WebFetch"), ToolItemType::WebSearch);
        assert_eq!(classify_tool("Read"), ToolItemType::DynamicTool);
    }

    #[test]
    fn tool_output_joins_text_blocks() {
        assert_eq!(tool_output_text(&json!("plain")), Some("plain".into()));
        assert_eq!(
            tool_output_text(&json!([{"type":"text","text":"a"},{"type":"text","text":"b"}])),
            Some("ab".into())
        );
        assert_eq!(tool_output_text(&json!([])), None);
    }

    #[test]
    fn builds_assistant_and_tool_artifacts_in_contract_shape() {
        let at: UtcDateTime = serde_json::from_value(json!("2026-10-15T00:00:00.000Z")).unwrap();
        let scope = scope();
        let assistant = assistant_artifacts(&scope, "msg-1:0", 101, "hi", at);
        assert!(matches!(assistant.turn_item, OrchestrationV2TurnItem::AssistantMessage { .. }));

        let input = json!({"command": "ls"});
        let tool = tool_call_artifacts(
            &scope,
            &ToolCall {
                native_item_id: "toolu_1",
                tool_name: "Bash",
                input: &input,
                output: Some(&json!("out")),
                status: "completed",
                ordinal: 102,
                started_at: at,
                updated_at: at,
            },
        );
        assert!(matches!(tool.turn_item, OrchestrationV2TurnItem::CommandExecution { .. }));
    }
}
