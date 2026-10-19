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

/// Decodes an artifact built here from contract field names. The unit tests
/// decode every shape, but a native frame can still carry a value the contract
/// rejects (a blank id, say). Such an artifact is logged and dropped, so one
/// malformed frame cannot panic the session actor.
fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Option<T> {
    match serde_json::from_value(value) {
        Ok(artifact) => Some(artifact),
        Err(error) => {
            tracing::warn!(%error, "Claude artifact does not match the contract shape; dropped");
            None
        }
    }
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
) -> Option<AssistantArtifacts> {
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
    }))?;
    let message = decode(json!({
        "createdBy": "agent", "creationSource": "provider", "id": message_id,
        "threadId": scope.thread_id, "runId": scope.run_id, "nodeId": node_id,
        "role": "assistant", "text": text, "attachments": [], "streaming": false,
        "createdAt": at, "updatedAt": at,
    }))?;
    let turn_item = decode(json!({
        "type": "assistant_message", "id": item_id, "threadId": scope.thread_id,
        "runId": scope.run_id, "nodeId": node_id, "providerThreadId": scope.provider_thread_id,
        "providerTurnId": scope.provider_turn_id, "nativeItemRef": native,
        "parentItemId": null, "ordinal": ordinal, "status": "completed", "title": null,
        "startedAt": at, "completedAt": at, "updatedAt": at,
        "messageId": message_id, "text": text, "streaming": false,
    }))?;
    Some(AssistantArtifacts {
        node,
        message,
        turn_item,
    })
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
) -> Option<OrchestrationV2TurnItem> {
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
) -> Option<OrchestrationV2TurnItem> {
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
) -> Option<OrchestrationV2TurnItem> {
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
pub fn tool_call_artifacts(scope: &TurnScope, call: &ToolCall<'_>) -> Option<ToolCallArtifacts> {
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
    }))?;

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
    let title = match read_path.as_deref() {
        Some(path) => Some(read_label(path)),
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
    // Upstream `viewedImagePath`: a Read of a workspace image names the image,
    // so the viewer can show it. Only a single-line path within the length bound.
    let viewed_image_path = read_path
        .as_deref()
        .filter(|path| {
            turin_provider_core::js::utf16_len(path) <= 4096
                && !path.contains(['\r', '\n'])
                && turin_shared::file_preview::is_workspace_image_preview_path(path)
        })
        .map(str::to_owned);
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
        ToolItemType::WebSearch => web_search_fields(call, input, output_text.as_deref()),
        ToolItemType::DynamicTool => {
            json!({
                "type": "dynamic_tool",
                "toolName": call.tool_name,
                "input": input,
                "output": output_value,
                "toolIcon": call.presentation.tool_icon,
                "toolSource": call.presentation.tool_source,
                "viewedImagePath": viewed_image_path,
            })
        }
    };
    for (key, value) in extra.as_object().into_iter().flatten() {
        if !value.is_null() {
            item.insert(key.clone(), value.clone());
        }
    }
    let turn_item = decode(Value::Object(item))?;
    Some(ToolCallArtifacts { node, turn_item })
}

/// Upstream `WEB_FETCH_SNIPPET_MAX_CHARS`: a WebFetch row keeps this much page text.
const WEB_FETCH_SNIPPET_MAX_CHARS: usize = 8_000;

/// Upstream `isClaudeWebSearchOutput`: the structured output of a web search.
fn is_web_search_output(value: &Value) -> bool {
    value.get("query").is_some_and(Value::is_string)
        && value.get("results").is_some_and(Value::is_array)
        && value.get("durationSeconds").is_some_and(Value::is_number)
}

/// The `web_search` item of a WebSearch or WebFetch call (upstream
/// `webSearchPatternsFromClaudeTool`, `webSearchResultsFromClaudeOutput`, and
/// the WebFetch snippet). A WebFetch returns page text, not hits, so its
/// row carries a bounded preview of that text.
fn web_search_fields(call: &ToolCall<'_>, input: &Value, output_text: Option<&str>) -> Value {
    let output = call.output.filter(|value| is_web_search_output(value));
    let pattern = first_trimmed_string(input, &["query", "url", "pattern"]).or_else(|| {
        output
            .and_then(|value| value.get("query"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .map(str::to_owned)
    });
    // Each hit is one result: a title and URL, from the search's result blocks.
    let mut results: Vec<Value> = output
        .and_then(|value| value.get("results"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|result| result.get("content").and_then(Value::as_array))
        .flatten()
        .map(|content| {
            let mut hit = serde_json::Map::new();
            if let Some(title) = content.get("title").and_then(Value::as_str) {
                hit.insert("title".into(), json!(title));
            }
            if let Some(url) = content
                .get("url")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|url| !url.is_empty())
            {
                hit.insert("url".into(), json!(url));
            }
            Value::Object(hit)
        })
        .collect();
    let fetched = normalized_tool_name(call.tool_name) == "webfetch";
    let text = output_text.unwrap_or_default();
    if results.is_empty() && fetched && !text.trim().is_empty() {
        let mut page = serde_json::Map::new();
        if let Some(url) = first_trimmed_string(input, &["url"]) {
            page.insert("url".into(), json!(url));
        }
        let snippet = turin_provider_core::js::take_utf16(text, WEB_FETCH_SNIPPET_MAX_CHARS);
        page.insert("snippet".into(), json!(snippet));
        results.push(Value::Object(page));
    }
    let mut fields = serde_json::Map::new();
    fields.insert("type".into(), json!("web_search"));
    if let Some(pattern) = pattern {
        fields.insert("patterns".into(), json!([pattern]));
    }
    if !results.is_empty() {
        fields.insert("results".into(), json!(results));
    }
    Value::Object(fields)
}

/// Upstream `CLAUDE_KNOWN_TOOL_CLASSIFICATIONS` `requestKind`: what kind of
/// approval a native tool asks for. Unknown tools ask as a command.
pub fn request_kind(tool_name: &str) -> &'static str {
    match normalized_tool_name(tool_name).as_str() {
        "edit" | "multiedit" | "notebookedit" | "write" => "file-change",
        "glob" | "grep" | "ls" | "read" => "file-read",
        _ => "command",
    }
}

/// Upstream `summarizeClaudeToolRequest`: the command or file a tool asks to
/// use, else its input as JSON, capped at 400 characters.
pub fn summarize_tool_request(tool_name: &str, input: &Value) -> String {
    let value = if input.get("type").and_then(Value::as_str) == Some("record") {
        input.get("value").unwrap_or(input)
    } else {
        input
    };
    let cap = |text: &str| text.chars().take(400).collect::<String>();
    if let Some(command) = first_trimmed_string(value, &["command", "cmd", "script"]) {
        return format!("{tool_name}: {}", cap(&command));
    }
    if let Some(path) = first_trimmed_string(value, &["file_path", "path", "filename", "fileName"])
    {
        return format!("{tool_name}: {}", cap(&path));
    }
    let serialized = serde_json::to_string(value).unwrap_or_default();
    if serialized.chars().count() <= 400 {
        format!("{tool_name}: {serialized}")
    } else {
        format!(
            "{tool_name}: {}...",
            serialized.chars().take(397).collect::<String>()
        )
    }
}

/// Upstream `claudeUserInputQuestions`: the `AskUserQuestion` input's
/// questions in the contract's shape. A question is keyed by its own text.
pub fn user_input_questions(input: &Value) -> Vec<Value> {
    let value = if input.get("type").and_then(Value::as_str) == Some("record") {
        input.get("value").unwrap_or(input)
    } else {
        input
    };
    let Some(questions) = value.get("questions").and_then(Value::as_array) else {
        return vec![];
    };
    let text = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_owned()
    };
    questions
        .iter()
        .enumerate()
        .filter_map(|(index, record)| {
            record.as_object()?;
            let question = text(record, "question");
            if question.is_empty() {
                return None;
            }
            let header = match text(record, "header") {
                header if header.is_empty() => format!("Question {}", index + 1),
                header => header,
            };
            let options: Vec<Value> = record
                .get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .filter_map(|option| {
                            option.as_object()?;
                            let label = text(option, "label");
                            (!label.is_empty())
                                .then(|| json!({"label": label, "description": text(option, "description")}))
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(json!({
                "id": question, "header": header, "question": question,
                "options": options,
                "multiSelect": record.get("multiSelect").and_then(Value::as_bool) == Some(true),
            }))
        })
        .collect()
}

/// What a raised request asks the user.
pub enum RequestBody<'a> {
    /// `request_kind` is a `request_kind(..)` value.
    Approval { request_kind: &'a str, prompt: &'a str },
    UserInput { questions: &'a [Value] },
}

pub struct RequestArtifacts {
    pub node: OrchestrationV2ExecutionNode,
    pub turn_item: OrchestrationV2TurnItem,
}

/// Upstream `buildApprovalRequestArtifacts` (node and turn item): the waiting
/// node a runtime request hangs from, and the item that carries what the user
/// is asked. The item's prompt is what the approval drawer shows.
pub fn request_artifacts(
    scope: &TurnScope,
    native_item_id: &str,
    request_id: &turin_contracts::RuntimeRequestId,
    ordinal: u64,
    body: &RequestBody<'_>,
    at: turin_contracts::UtcDateTime,
) -> Option<RequestArtifacts> {
    let driver = &scope.driver;
    let node_id = derive::approval_node(request_id);
    let native = native_ref(driver, native_item_id);
    let node = decode(json!({
        "id": node_id, "threadId": scope.thread_id, "runId": scope.run_id,
        "parentNodeId": derive::node_from_provider_item(driver, native_item_id),
        "rootNodeId": scope.root_node_id,
        "kind": match body {
            RequestBody::Approval { .. } => "approval_request",
            RequestBody::UserInput { .. } => "user_input_request",
        },
        "status": "waiting", "countsForRun": false,
        "providerThreadId": scope.provider_thread_id, "providerTurnId": scope.provider_turn_id,
        "nativeItemRef": native, "runtimeRequestId": request_id, "checkpointScopeId": null,
        "startedAt": at, "completedAt": null,
    }))?;
    let mut item = json!({
        "id": derive::approval_turn_item(request_id), "threadId": scope.thread_id,
        "runId": scope.run_id, "nodeId": node_id,
        "providerThreadId": scope.provider_thread_id, "providerTurnId": scope.provider_turn_id,
        "nativeItemRef": native, "parentItemId": null, "ordinal": ordinal,
        "status": "waiting", "title": null, "startedAt": at, "completedAt": null,
        "updatedAt": at, "requestId": request_id,
    });
    if let Some(map) = item.as_object_mut() {
        match body {
            RequestBody::Approval { request_kind, prompt } => {
                map.insert("type".into(), json!("approval_request"));
                map.insert("requestKind".into(), json!(request_kind));
                map.insert("prompt".into(), json!(prompt));
            }
            RequestBody::UserInput { questions } => {
                map.insert("type".into(), json!("user_input_request"));
                map.insert("questions".into(), json!(questions));
            }
        }
    }
    Some(RequestArtifacts {
        node,
        turn_item: decode(item)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_approval_prompt_names_the_command_or_file() {
        assert_eq!(
            summarize_tool_request("Bash", &json!({"command": " pwd "})),
            "Bash: pwd"
        );
        assert_eq!(
            summarize_tool_request("Write", &json!({"file_path": "/w/a.txt", "content": "x"})),
            "Write: /w/a.txt"
        );
        assert_eq!(
            summarize_tool_request("Mcp", &json!({"a": 1})),
            "Mcp: {\"a\":1}"
        );
        assert_eq!(request_kind("Write"), "file-change");
        assert_eq!(request_kind("Grep"), "file-read");
        assert_eq!(request_kind("Bash"), "command");
    }

    #[test]
    fn user_input_questions_are_keyed_by_their_text() {
        let questions = user_input_questions(&json!({"questions": [
            {"question": "Which color?", "options": [{"label": "Red", "description": "Warm"}, {"label": " "}], "multiSelect": true},
            {"question": " "},
        ]}));
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0]["id"], "Which color?");
        assert_eq!(questions[0]["header"], "Question 1");
        assert_eq!(questions[0]["multiSelect"], true);
        assert_eq!(questions[0]["options"].as_array().map(Vec::len), Some(1));
    }

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
        let assistant = assistant_artifacts(&scope, "msg-1:0", 101, "hi", at).unwrap();
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
        )
        .unwrap();
        assert!(matches!(
            tool.turn_item,
            OrchestrationV2TurnItem::CommandExecution { .. }
        ));
    }

    fn artifact(tool_name: &str, input: &Value, output: Option<&Value>) -> Value {
        let at = UtcDateTime::now();
        let presentation = crate::claude_tool_titles::fallback_presentation(tool_name);
        let call = ToolCall {
            native_item_id: "toolu_1",
            tool_name,
            input,
            presentation: &presentation,
            output,
            output_fallback: None,
            status: "completed",
            ordinal: 1,
            started_at: at,
            updated_at: at,
        };
        serde_json::to_value(tool_call_artifacts(&scope(), &call).unwrap().turn_item).unwrap()
    }

    #[test]
    fn an_artifact_the_contract_rejects_is_dropped_not_panicked() {
        // A blank native id is not a TrimmedNonEmptyString: the builder returns None.
        let at = UtcDateTime::now();
        let input = json!({"command": "ls"});
        let blank = ToolCall {
            native_item_id: "",
            tool_name: "Bash",
            input: &input,
            presentation: &ToolPresentation::default(),
            output: None,
            output_fallback: None,
            status: "running",
            ordinal: 1,
            started_at: at,
            updated_at: at,
        };
        assert!(tool_call_artifacts(&scope(), &blank).is_none());
        assert!(assistant_artifacts(&scope(), "", 1, "hi", at).is_none());
    }

    #[test]
    fn a_read_of_a_workspace_image_names_the_viewed_path() {
        let image = artifact("Read", &json!({"file_path": "/work/app/logo.png"}), None);
        assert_eq!(image["type"], "dynamic_tool");
        assert_eq!(image["viewedImagePath"], "/work/app/logo.png");

        let text = artifact("Read", &json!({"file_path": "/work/app/notes.md"}), None);
        assert!(text.get("viewedImagePath").is_none());
    }

    #[test]
    fn web_search_carries_its_query_and_hits_and_web_fetch_a_snippet() {
        let search_output = json!({
            "query": "rust gpui",
            "results": [
                {"tool_use_id": "t1", "content": [{"title": "GPUI", "url": "https://gpui.rs"}]},
                "text only",
            ],
            "durationSeconds": 1.5,
        });
        let search = artifact("WebSearch", &json!({}), Some(&search_output));
        assert_eq!(search["type"], "web_search");
        assert_eq!(search["patterns"], json!(["rust gpui"]));
        assert_eq!(
            search["results"],
            json!([{"title": "GPUI", "url": "https://gpui.rs"}])
        );

        let page = json!("Page body");
        let fetch = artifact(
            "WebFetch",
            &json!({"url": "https://example.com"}),
            Some(&page),
        );
        assert_eq!(fetch["patterns"], json!(["https://example.com"]));
        assert_eq!(
            fetch["results"],
            json!([{"url": "https://example.com", "snippet": "Page body"}])
        );
    }
}
