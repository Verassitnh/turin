//! Port of the artifact builders in upstream ClaudeAdapterV2.ts:
//! `buildAssistantArtifacts`, `buildToolCallArtifacts`, the tool classification
//! table, and the proposed-plan capture. Items are assembled in the contract's
//! wire shape and decoded, so the contract types validate every one.
//!
//! Plan and todo projections live in `claude_plan_items`. Titles and MCP
//! presentation live in `claude_tool_titles`.
use crate::claude_tool_titles::{ToolPresentation, read_label, search_label, skill_title};
use serde_json::{Value, json};
use std::collections::HashMap;
use turin_contracts::{
    NodeId, ProviderThreadId, ProviderTurnId, RunId, ThreadId,
    orchestration_v2::{
        OrchestrationV2ConversationMessage, OrchestrationV2ExecutionNode, OrchestrationV2TurnItem,
    },
    provider_instance::ProviderDriverKind,
};
use turin_provider_core::id_allocator::derive;

/// `(driver, native item id) -> ordinal` for one provider turn.
pub type ItemOrdinals = HashMap<String, u64>;

/// Upstream `resolveClaudeItemOrdinal`: `providerTurnOrdinal * 100 + size + 1`,
/// allocated on first sight and reused afterwards.
pub fn item_ordinal(
    ordinals: &mut ItemOrdinals,
    provider_turn_ordinal: i64,
    native_item_id: &str,
) -> u64 {
    if let Some(existing) = ordinals.get(native_item_id) {
        return *existing;
    }
    let ordinal = provider_turn_ordinal.max(0) as u64 * 100 + ordinals.len() as u64 + 1;
    ordinals.insert(native_item_id.to_owned(), ordinal);
    ordinal
}

/// Where an item sits: the thread, run, root node, and provider turn it
/// belongs to. A subagent's child thread has no run (`run_id` is `None`), as
/// upstream's `runId: null` on its items.
#[derive(Clone)]
pub struct TurnScope {
    pub driver: ProviderDriverKind,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
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

/// Upstream `normalizedClaudeToolName`: lowercase, with whitespace, `_` and `-` removed.
pub fn normalized_tool_name(tool_name: &str) -> String {
    tool_name
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '_' && *c != '-')
        .collect()
}

pub fn classify_tool(tool_name: &str) -> ToolItemType {
    match normalized_tool_name(tool_name).as_str() {
        "bash" => ToolItemType::CommandExecution,
        "edit" | "multiedit" | "notebookedit" | "write" => ToolItemType::FileChange,
        "webfetch" | "websearch" => ToolItemType::WebSearch,
        _ => ToolItemType::DynamicTool,
    }
}

/// Upstream `claudeNativeToolOutputText`: a string as it is, anything else as JSON.
pub fn native_output_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Upstream `claudeSubagentResultText` for a `tool_result` content value: its
/// text blocks, one per line. Anything else falls back to `native_output_text`.
fn content_text(content: &Value) -> String {
    if let Value::Array(parts) = content {
        let text = parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            return text;
        }
    }
    native_output_text(content)
}

/// Upstream `claudeCommandOutputText`. A Bash result's structured `stdout` and
/// `stderr` are its text. A background run has empty streams, so its content
/// acknowledgement is kept instead.
pub fn command_output_text(value: &Value, fallback: Option<&Value>) -> String {
    let stdout = value.get("stdout").and_then(Value::as_str);
    let stderr = value.get("stderr").and_then(Value::as_str);
    if stdout.is_none() && stderr.is_none() {
        return native_output_text(value);
    }
    let text = [stdout, stderr]
        .into_iter()
        .flatten()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if !text.is_empty() {
        return text;
    }
    fallback.map(content_text).unwrap_or_default()
}

/// Upstream `reasoning` turn item for one completed thinking block, titled
/// "Thinking".
pub fn reasoning_item(
    scope: &TurnScope,
    native_item_id: &str,
    ordinal: u64,
    text: &str,
    at: turin_contracts::UtcDateTime,
) -> OrchestrationV2TurnItem {
    streaming_reasoning_item(scope, native_item_id, ordinal, text, at, at, true)
}

/// Upstream reasoning item as its thinking block streams: `running` and
/// `streaming` until the block stops, then `completed`. The item keeps its
/// native id, so the completed block replaces the streamed text in place.
pub fn streaming_reasoning_item(
    scope: &TurnScope,
    native_item_id: &str,
    ordinal: u64,
    text: &str,
    started_at: turin_contracts::UtcDateTime,
    updated_at: turin_contracts::UtcDateTime,
    completed: bool,
) -> OrchestrationV2TurnItem {
    let driver = &scope.driver;
    decode(json!({
        "type": "reasoning",
        "id": derive::turn_item_from_provider_item(driver, native_item_id),
        "threadId": scope.thread_id, "runId": scope.run_id,
        "nodeId": scope.root_node_id, "providerThreadId": scope.provider_thread_id,
        "providerTurnId": scope.provider_turn_id,
        "nativeItemRef": native_ref(driver, native_item_id),
        "parentItemId": null, "ordinal": ordinal,
        "status": if completed { "completed" } else { "running" },
        "title": "Thinking", "startedAt": started_at,
        "completedAt": if completed { json!(updated_at) } else { Value::Null },
        "updatedAt": updated_at,
        "text": text, "streaming": !completed,
    }))
}

/// Upstream `compaction` turn item for a `compact_boundary` message. Its
/// native id is the message uuid.
pub fn compaction_item(
    scope: &TurnScope,
    native_item_id: &str,
    ordinal: u64,
    before_tokens: Option<u64>,
    after_tokens: Option<u64>,
    at: turin_contracts::UtcDateTime,
) -> OrchestrationV2TurnItem {
    let driver = &scope.driver;
    let mut item = json!({
        "type": "compaction",
        "id": derive::turn_item_from_provider_item(driver, native_item_id),
        "threadId": scope.thread_id, "runId": scope.run_id,
        "nodeId": scope.root_node_id, "providerThreadId": scope.provider_thread_id,
        "providerTurnId": scope.provider_turn_id,
        "nativeItemRef": native_ref(driver, native_item_id),
        "parentItemId": null, "ordinal": ordinal, "status": "completed",
        "title": "Context compacted", "startedAt": at, "completedAt": at, "updatedAt": at,
        "driver": driver,
    });
    if let Some(map) = item.as_object_mut() {
        if let Some(before) = before_tokens {
            map.insert("beforeTokenCount".into(), json!(before));
        }
        if let Some(after) = after_tokens {
            map.insert("afterTokenCount".into(), json!(after));
        }
    }
    decode(item)
}

/// Upstream `firstStringInputField`: the first key holding a non-blank string,
/// trimmed.
fn first_trimmed_string(input: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        let value = input.get(*key)?.as_str()?.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

pub struct ToolCallArtifacts {
    pub node: OrchestrationV2ExecutionNode,
    pub turn_item: OrchestrationV2TurnItem,
}

pub struct ToolCall<'a> {
    pub native_item_id: &'a str,
    pub tool_name: &'a str,
    pub input: &'a Value,
    pub presentation: &'a ToolPresentation,
    /// The call's native output once it has finished: the structured
    /// `tool_use_result` when the frame carries one, else the `tool_result` content.
    pub output: Option<&'a Value>,
    /// The `tool_result` content behind a structured `output`, the text a
    /// Bash result falls back to.
    pub output_fallback: Option<&'a Value>,
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
    let non_empty = |text: String| (!text.is_empty()).then_some(text);
    let output_text = call.output.map(native_output_text).and_then(non_empty);
    let command_output = call
        .output
        .map(|value| command_output_text(value, call.output_fallback))
        .and_then(non_empty);
    let input = call.input;
    // Upstream order: a Read names its file, a search names its query, then a
    // Skill, then the MCP presentation.
    let normalized = normalized_tool_name(call.tool_name);
    let read_path = matches!(normalized.as_str(), "read" | "readfile")
        .then(|| first_trimmed_string(input, &["file_path", "path"]))
        .flatten();
    let title = match read_path {
        Some(path) => Some(read_label(&path)),
        None if matches!(normalized.as_str(), "grep" | "glob" | "ls") => search_label(input),
        None => None,
    }
    .or_else(|| skill_title(call.tool_name, input))
    .or_else(|| call.presentation.title.clone());
    let common = json!({
        "id": item_id, "threadId": scope.thread_id, "runId": scope.run_id,
        "nodeId": node_id, "providerThreadId": scope.provider_thread_id,
        "providerTurnId": scope.provider_turn_id, "nativeItemRef": native,
        "parentItemId": null, "ordinal": call.ordinal, "status": call.status,
        "title": title, "startedAt": call.started_at, "completedAt": completed_at,
        "updatedAt": call.updated_at,
    });
    let mut item = common.as_object().cloned().unwrap_or_default();
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
                "output": command_output,
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
                "toolIcon": call.presentation.tool_icon,
                "toolSource": call.presentation.tool_source,
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
            run_id: Some(serde_json::from_value(json!("run-1")).unwrap()),
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
    fn native_output_is_text_or_json_like_upstream() {
        assert_eq!(native_output_text(&json!("plain")), "plain");
        assert_eq!(
            native_output_text(&json!([{"type":"text","text":"a"}])),
            r#"[{"type":"text","text":"a"}]"#
        );
    }

    #[test]
    fn command_output_prefers_structured_streams_then_the_content() {
        let content = json!([{"type":"text","text":"ack"}]);
        assert_eq!(
            command_output_text(&json!({"stdout": "out", "stderr": "warn"}), None),
            "out\nwarn"
        );
        assert_eq!(
            command_output_text(&json!({"stdout": " ", "stderr": ""}), Some(&content)),
            "ack"
        );
        assert_eq!(command_output_text(&json!({"stdout": ""}), None), "");
        assert_eq!(command_output_text(&json!("plain"), None), "plain");
    }

    #[test]
    fn builds_assistant_and_tool_artifacts_in_contract_shape() {
        let at: UtcDateTime = serde_json::from_value(json!("2026-10-15T00:00:00.000Z")).unwrap();
        let scope = scope();
        let assistant = assistant_artifacts(&scope, "msg-1:0", 101, "hi", at);
        assert!(matches!(
            assistant.turn_item,
            OrchestrationV2TurnItem::AssistantMessage { .. }
        ));

        let input = json!({"command": "ls"});
        let tool = tool_call_artifacts(
            &scope,
            &ToolCall {
                native_item_id: "toolu_1",
                tool_name: "Bash",
                input: &input,
                presentation: &ToolPresentation::default(),
                output: Some(&json!("out")),
                output_fallback: None,
                status: "completed",
                ordinal: 102,
                started_at: at,
                updated_at: at,
            },
        );
        assert!(matches!(
            tool.turn_item,
            OrchestrationV2TurnItem::CommandExecution { .. }
        ));
    }
}
