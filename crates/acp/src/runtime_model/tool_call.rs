use serde_json::{Map, Value};
use turin_contracts::provider_runtime::ToolLifecycleItemType;
use turin_shared::tool_activity::{
    ToolActivityPresentationInput, derive_tool_activity_presentation, merge_tool_activity_data,
};

use super::content::{sanitize_acp_tool_call_content, tool_call_content_text};
use super::util::is_record;

const TOOL_CALL_CONTENT_MAX_CHARS: usize = 8_000;
const TOOL_CALL_CONTENT_TRUNCATION_MARKER: &str = "[Earlier output truncated]\n\n";
const RAW_OUTPUT_TEXT_FIELDS: [&str; 4] = ["content", "stdout", "stderr", "output"];
const TOOL_CALL_UPDATE_MIN_DETAIL_GROWTH_CHARS: usize = 256;
const TOOL_CALL_UPDATE_COALESCE_LIMIT: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpToolCallStatus {
    Pending,
    InProgress,
    RequiresAction,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AcpToolCallState {
    pub tool_call_id: String,
    pub kind: Option<String>,
    pub title: Option<String>,
    pub status: Option<AcpToolCallStatus>,
    pub command: Option<String>,
    pub detail: Option<String>,
    pub data: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AcpToolCallEmitDecisionInput<'a> {
    pub previous: Option<&'a AcpToolCallState>,
    pub next: &'a AcpToolCallState,
    pub last_emitted_detail_length: Option<usize>,
    pub skipped_since_emit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpToolCallEmitDecision {
    pub emit: bool,
    pub skipped_since_emit: usize,
}

pub fn canonical_item_type_from_acp_tool_kind(kind: Option<&str>) -> ToolLifecycleItemType {
    match kind {
        Some("read") => ToolLifecycleItemType::DynamicToolCall,
        Some("execute") => ToolLifecycleItemType::CommandExecution,
        Some("edit") | Some("delete") | Some("move") => ToolLifecycleItemType::FileChange,
        Some("search") | Some("fetch") => ToolLifecycleItemType::WebSearch,
        _ => ToolLifecycleItemType::DynamicToolCall,
    }
}

fn lifecycle_item_type_wire(kind: Option<&str>) -> &'static str {
    match canonical_item_type_from_acp_tool_kind(kind) {
        ToolLifecycleItemType::CommandExecution => "command_execution",
        ToolLifecycleItemType::FileChange => "file_change",
        ToolLifecycleItemType::WebSearch => "web_search",
        ToolLifecycleItemType::DynamicToolCall => "dynamic_tool_call",
        ToolLifecycleItemType::McpToolCall => "mcp_tool_call",
        ToolLifecycleItemType::CollabAgentToolCall => "collab_agent_tool_call",
        ToolLifecycleItemType::ImageView => "image_view",
    }
}

fn bound_tool_call_output_text(text: &str) -> String {
    if text.len() <= TOOL_CALL_CONTENT_MAX_CHARS {
        return text.to_owned();
    }
    let tail = &text[text.len().saturating_sub(TOOL_CALL_CONTENT_MAX_CHARS)..];
    format!("{TOOL_CALL_CONTENT_TRUNCATION_MARKER}{tail}")
}

fn bound_tool_call_raw_output(raw_output: &Value) -> Value {
    let Some(record) = raw_output.as_object() else {
        return raw_output.clone();
    };
    let mut changed = false;
    let mut bounded = record.clone();
    for field in RAW_OUTPUT_TEXT_FIELDS {
        if let Some(value) = record.get(field).and_then(Value::as_str)
            && value.len() > TOOL_CALL_CONTENT_MAX_CHARS
        {
            bounded.insert(
                field.to_owned(),
                Value::String(bound_tool_call_output_text(value)),
            );
            changed = true;
        }
    }
    if changed {
        Value::Object(bounded)
    } else {
        raw_output.clone()
    }
}

struct ExtractedToolCallContent {
    text: Option<String>,
    content: Option<Vec<Value>>,
}

fn bound_tool_call_content_entries(content: &[Value]) -> Vec<Value> {
    let mut changed = false;
    let bounded: Vec<Value> = content
        .iter()
        .map(|entry| {
            let Some(text) = tool_call_content_text(entry) else {
                return entry.clone();
            };
            if text.len() <= TOOL_CALL_CONTENT_MAX_CHARS {
                return entry.clone();
            }
            changed = true;
            let trimmed = text.trim();
            let bounded_text =
                bound_tool_call_output_text(if !trimmed.is_empty() { trimmed } else { text });
            serde_json::json!({
                "type": "content",
                "content": { "type": "text", "text": bounded_text },
            })
        })
        .collect();
    if changed { bounded } else { content.to_vec() }
}

fn distribute_retained_tail_across_content(content: &[Value], tail: &str) -> Vec<Value> {
    let mut text_ranges: Vec<Option<(usize, usize, &str)>> = vec![None; content.len()];
    let mut offset = 0usize;
    let mut seen_text = false;
    for (index, entry) in content.iter().enumerate() {
        let Some(text) = tool_call_content_text(entry)
            .map(str::trim)
            .filter(|t| !t.is_empty())
        else {
            continue;
        };
        if seen_text {
            offset += 1;
        }
        seen_text = true;
        let start = offset;
        let end = offset + text.len();
        text_ranges[index] = Some((start, end, text));
        offset = end;
    }
    let tail_start = offset.saturating_sub(tail.len());
    let mut marker_pending = true;
    let mut out = Vec::new();
    for (index, entry) in content.iter().enumerate() {
        if tool_call_content_text(entry).is_none() {
            out.push(entry.clone());
            continue;
        }
        let Some((start, end, text)) = text_ranges[index] else {
            continue;
        };
        let overlap_start = start.max(tail_start);
        let overlap_end = end.min(offset);
        if overlap_end <= overlap_start {
            continue;
        }
        let mut piece = text[overlap_start - start..overlap_end - start].to_owned();
        if marker_pending {
            piece = format!("{TOOL_CALL_CONTENT_TRUNCATION_MARKER}{piece}");
            marker_pending = false;
        }
        out.push(serde_json::json!({
            "type": "content",
            "content": { "type": "text", "text": piece },
        }));
    }
    out
}

fn extract_text_content_from_tool_call_content(
    content: Option<&[Value]>,
) -> ExtractedToolCallContent {
    let Some(content) = content else {
        return ExtractedToolCallContent {
            text: None,
            content: None,
        };
    };
    let chunks: Vec<String> = content
        .iter()
        .filter_map(|entry| {
            tool_call_content_text(entry)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        })
        .collect();
    if chunks.is_empty() {
        return ExtractedToolCallContent {
            text: None,
            content: Some(bound_tool_call_content_entries(content)),
        };
    }
    let joined = chunks.join("\n");
    if joined.len() <= TOOL_CALL_CONTENT_MAX_CHARS {
        return ExtractedToolCallContent {
            text: Some(joined),
            content: Some(bound_tool_call_content_entries(content)),
        };
    }
    let bounded = bound_tool_call_output_text(&joined);
    let tail = &joined[joined.len().saturating_sub(TOOL_CALL_CONTENT_MAX_CHARS)..];
    ExtractedToolCallContent {
        text: Some(bounded),
        content: Some(distribute_retained_tail_across_content(content, tail)),
    }
}

fn normalize_tool_kind(kind: Option<&Value>) -> Option<String> {
    kind.and_then(Value::as_str)
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_owned)
}

pub(crate) fn normalize_tool_call_status(
    raw: Option<&Value>,
    fallback: Option<AcpToolCallStatus>,
) -> Option<AcpToolCallStatus> {
    match raw.and_then(Value::as_str) {
        Some("pending") => Some(AcpToolCallStatus::Pending),
        Some("in_progress") | Some("inProgress") => Some(AcpToolCallStatus::InProgress),
        Some("completed") => Some(AcpToolCallStatus::Completed),
        Some("failed") => Some(AcpToolCallStatus::Failed),
        _ => fallback,
    }
}

fn normalize_command_value(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        let trimmed = text.trim();
        return (!trimmed.is_empty()).then(|| trimmed.to_owned());
    }
    let array = value.as_array()?;
    let parts: Vec<String> = array
        .iter()
        .filter_map(|entry| {
            entry
                .as_str()
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn extract_command_from_title(title: Option<&str>) -> Option<String> {
    let title = title?;
    let re = regex::Regex::new(r"`([^`]+)`").expect("valid regex");
    re.captures(title)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().trim())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn extract_tool_call_command(
    raw_input: Option<&Value>,
    title: Option<&str>,
    kind: Option<&str>,
) -> Option<String> {
    if let Some(record) = raw_input.and_then(is_record) {
        if let Some(command) = record.get("command").and_then(normalize_command_value) {
            return Some(command);
        }
        let executable = record
            .get("executable")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        let args = record.get("args").and_then(normalize_command_value);
        if !executable.is_empty() {
            return args
                .map(|a| format!("{executable} {a}"))
                .or_else(|| Some(executable.to_owned()));
        }
    }
    if kind != Some("execute") {
        return None;
    }
    extract_command_from_title(title)
}

fn file_path_from_tool_value(value: &Value) -> Option<String> {
    let trimmed = value.as_str()?.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn locations_from_tool_call_input(
    locations: Option<&[Value]>,
    content: Option<&[Value]>,
    raw_input: Option<&Value>,
    raw_output: Option<&Value>,
) -> Option<Vec<Value>> {
    if locations == Some(&[] as &[Value]) {
        return Some(Vec::new());
    }
    let mut out: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let push_location =
        |location: &Value, out: &mut Vec<Value>, seen: &mut std::collections::HashSet<String>| {
            let Some(path) = location.get("path").and_then(file_path_from_tool_value) else {
                return;
            };
            if seen.insert(path.clone()) {
                let mut loc = location.as_object().cloned().unwrap_or_default();
                loc.insert("path".to_owned(), Value::String(path));
                out.push(Value::Object(loc));
            }
        };
    let push_path =
        |value: &Value, out: &mut Vec<Value>, seen: &mut std::collections::HashSet<String>| {
            let Some(path) = file_path_from_tool_value(value) else {
                return;
            };
            if seen.insert(path.clone()) {
                out.push(serde_json::json!({ "path": path }));
            }
        };
    if let Some(locations) = locations {
        for location in locations {
            push_location(location, &mut out, &mut seen);
        }
    }
    if let Some(content) = content {
        for entry in content {
            if entry.get("type").and_then(Value::as_str) == Some("diff")
                && entry.get("path").is_some()
            {
                push_path(
                    entry.get("path").unwrap_or(&Value::Null),
                    &mut out,
                    &mut seen,
                );
            }
        }
    }
    if let Some(record) = raw_input.and_then(is_record) {
        for key in ["path", "filePath", "file_path"] {
            if let Some(value) = record.get(key) {
                push_path(value, &mut out, &mut seen);
            }
        }
    }
    if let Some(record) = raw_output.and_then(is_record) {
        for key in ["path", "filePath", "file_path"] {
            if let Some(value) = record.get(key) {
                push_path(value, &mut out, &mut seen);
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

pub(crate) struct MakeToolCallStateInput<'a> {
    pub tool_call_id: &'a str,
    pub title: Option<&'a Value>,
    pub kind: Option<&'a Value>,
    pub status: Option<&'a Value>,
    pub raw_input: Option<&'a Value>,
    pub raw_output: Option<&'a Value>,
    pub content: Option<&'a [Value]>,
    pub locations: Option<&'a [Value]>,
    pub meta: Option<&'a Value>,
}

pub(crate) fn make_tool_call_state(
    input: MakeToolCallStateInput<'_>,
    fallback_status: Option<AcpToolCallStatus>,
) -> Option<AcpToolCallState> {
    let tool_call_id = input.tool_call_id.trim();
    if tool_call_id.is_empty() {
        return None;
    }
    let title = input
        .title
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    let kind = normalize_tool_kind(input.kind);
    let command = extract_tool_call_command(input.raw_input, title.as_deref(), kind.as_deref());
    let extracted = extract_text_content_from_tool_call_content(input.content);
    let text_content = extracted.text.as_deref();
    let normalized_title = title.as_ref().filter(|t| {
        let lower = t.to_lowercase();
        lower != "terminal" && lower != "tool call"
    });
    let mut data: Map<String, Value> = Map::new();
    data.insert(
        "toolCallId".to_owned(),
        Value::String(tool_call_id.to_owned()),
    );
    if let Some(ref kind) = kind {
        data.insert("kind".to_owned(), Value::String(kind.clone()));
    }
    if let Some(ref title) = title {
        data.insert("title".to_owned(), Value::String(title.clone()));
    }
    if let Some(ref command) = command {
        data.insert("command".to_owned(), Value::String(command.clone()));
    }
    if let Some(raw_input) = input.raw_input
        && !raw_input.is_null()
    {
        data.insert("rawInput".to_owned(), raw_input.clone());
    }
    if let Some(meta) = input.meta.filter(|m| m.is_object()) {
        data.insert("meta".to_owned(), meta.clone());
    }
    if let Some(raw_output) = input.raw_output
        && !raw_output.is_null()
    {
        data.insert(
            "rawOutput".to_owned(),
            bound_tool_call_raw_output(raw_output),
        );
    }
    if let Some(content) = input.content {
        let sanitized =
            sanitize_acp_tool_call_content(extracted.content.as_deref().unwrap_or(content));
        data.insert("content".to_owned(), Value::Array(sanitized));
    }
    if let Some(locations) = locations_from_tool_call_input(
        input.locations,
        input.content,
        input.raw_input,
        input.raw_output,
    ) {
        data.insert("locations".to_owned(), Value::Array(locations));
    }
    let fallback_detail = command
        .as_deref()
        .or(normalized_title.map(String::as_str))
        .or(text_content);
    let has_presentation_seed = title.is_some()
        || kind.is_some()
        || command.is_some()
        || normalized_title.is_some()
        || text_content.is_some();
    let presentation = has_presentation_seed.then(|| {
        derive_tool_activity_presentation(&ToolActivityPresentationInput {
            item_type: Some(lifecycle_item_type_wire(kind.as_deref())),
            title: title.as_deref(),
            detail: fallback_detail,
            data: &Value::Object(data.clone()),
            fallback_summary: title.as_deref().or(Some("Tool")),
        })
    });
    let status = normalize_tool_call_status(input.status, fallback_status);
    Some(AcpToolCallState {
        tool_call_id: tool_call_id.to_owned(),
        kind,
        title: presentation
            .as_ref()
            .filter(|p| !p.summary.is_empty())
            .map(|p| p.summary.clone()),
        status,
        command,
        detail: presentation.and_then(|p| p.detail),
        data,
    })
}

fn tool_call_locations(update: &Value) -> Option<&[Value]> {
    match update.get("locations") {
        None => None,
        Some(Value::Null) => Some(&[]),
        Some(value) => value.as_array().map(|array| array.as_slice()),
    }
}

pub(crate) fn parse_typed_tool_call_state(
    update: &Value,
    fallback_status: Option<AcpToolCallStatus>,
) -> Option<AcpToolCallState> {
    make_tool_call_state(
        MakeToolCallStateInput {
            tool_call_id: update
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or(""),
            title: update.get("title"),
            kind: update.get("kind"),
            status: update.get("status"),
            raw_input: update.get("rawInput"),
            raw_output: update.get("rawOutput"),
            content: update
                .get("content")
                .and_then(Value::as_array)
                .map(|a| a.as_slice()),
            locations: tool_call_locations(update),
            meta: update.get("_meta"),
        },
        fallback_status,
    )
}

pub fn merge_tool_call_state(
    previous: Option<&AcpToolCallState>,
    next: AcpToolCallState,
) -> AcpToolCallState {
    let next_kind = next
        .kind
        .as_deref()
        .or_else(|| next.data.get("kind").and_then(Value::as_str));
    let kind = next_kind
        .map(str::to_owned)
        .or_else(|| previous.and_then(|p| p.kind.clone()));
    let title = next
        .title
        .or_else(|| previous.and_then(|p| p.title.clone()));
    let status = next.status.or(previous.and_then(|p| p.status));
    let command = next
        .command
        .or_else(|| previous.and_then(|p| p.command.clone()));
    let detail = next
        .detail
        .or_else(|| previous.and_then(|p| p.detail.clone()));
    let data = merge_tool_activity_data(
        &Value::Object(previous.map(|p| p.data.clone()).unwrap_or_default()),
        &Value::Object(next.data.clone()),
    )
    .unwrap_or(next.data);
    AcpToolCallState {
        tool_call_id: next.tool_call_id,
        kind,
        title,
        status,
        command,
        detail,
        data,
    }
}

fn tool_call_output_unchanged(previous: &AcpToolCallState, next: &AcpToolCallState) -> bool {
    previous.data.get("content") == next.data.get("content")
        && previous.data.get("rawOutput") == next.data.get("rawOutput")
}

pub fn tool_call_progress_length(state: &AcpToolCallState) -> usize {
    let mut content_chars = 0usize;
    if let Some(content) = state.data.get("content").and_then(Value::as_array) {
        for entry in content {
            if let Some(text) = tool_call_content_text(entry) {
                content_chars += text.len();
            }
        }
    }
    let mut raw_output_chars = 0usize;
    if let Some(record) = state.data.get("rawOutput").and_then(is_record) {
        for field in RAW_OUTPUT_TEXT_FIELDS {
            if let Some(value) = record.get(field).and_then(Value::as_str) {
                raw_output_chars += value.len();
            }
        }
    }
    state
        .detail
        .as_ref()
        .map(String::len)
        .unwrap_or(0)
        .max(content_chars)
        .max(raw_output_chars)
}

fn tool_call_content_texts(state: &AcpToolCallState) -> String {
    let Some(content) = state.data.get("content").and_then(Value::as_array) else {
        return String::new();
    };
    content
        .iter()
        .map(|entry| tool_call_content_text(entry).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\u{0000}")
}

pub fn tool_call_visible_output_changed(
    previous: &AcpToolCallState,
    next: &AcpToolCallState,
) -> bool {
    previous.detail != next.detail
        || tool_call_content_texts(previous) != tool_call_content_texts(next)
        || previous.data.get("rawOutput") != next.data.get("rawOutput")
}

pub fn decide_tool_call_update_emission(
    input: &AcpToolCallEmitDecisionInput<'_>,
) -> AcpToolCallEmitDecision {
    let AcpToolCallEmitDecisionInput {
        previous,
        next,
        last_emitted_detail_length,
        skipped_since_emit,
    } = input;
    if matches!(
        next.status,
        Some(AcpToolCallStatus::Completed) | Some(AcpToolCallStatus::Failed)
    ) {
        return AcpToolCallEmitDecision {
            emit: true,
            skipped_since_emit: 0,
        };
    }
    if previous.is_none()
        || previous.map(|p| p.title.as_deref()) != Some(next.title.as_deref())
        || previous.and_then(|p| p.status) != next.status
    {
        return AcpToolCallEmitDecision {
            emit: true,
            skipped_since_emit: 0,
        };
    }
    let previous = previous.expect("checked");
    if previous.detail == next.detail && tool_call_output_unchanged(previous, next) {
        return AcpToolCallEmitDecision {
            emit: false,
            skipped_since_emit: *skipped_since_emit,
        };
    }
    let progress_length = tool_call_progress_length(next);
    let grew_meaningfully = match last_emitted_detail_length {
        None => true,
        Some(last) => progress_length.abs_diff(*last) >= TOOL_CALL_UPDATE_MIN_DETAIL_GROWTH_CHARS,
    };
    if grew_meaningfully || *skipped_since_emit + 1 >= TOOL_CALL_UPDATE_COALESCE_LIMIT {
        AcpToolCallEmitDecision {
            emit: true,
            skipped_since_emit: 0,
        }
    } else {
        AcpToolCallEmitDecision {
            emit: false,
            skipped_since_emit: skipped_since_emit + 1,
        }
    }
}

pub(crate) fn bound_tool_call_raw_payload(
    params: &Value,
    update: &Value,
    tool_call: &AcpToolCallState,
) -> Value {
    let bounded_content = tool_call.data.get("content");
    let bounded_raw_output = tool_call.data.get("rawOutput");
    let content_bounded =
        update.get("content").is_some() && bounded_content != update.get("content");
    let raw_output_bounded =
        update.get("rawOutput").is_some() && bounded_raw_output != update.get("rawOutput");
    if !content_bounded && !raw_output_bounded {
        return params.clone();
    }
    let Some(mut update_obj) = update.as_object().cloned() else {
        return params.clone();
    };
    if content_bounded {
        update_obj.insert(
            "content".to_owned(),
            bounded_content.cloned().unwrap_or(Value::Null),
        );
    }
    if raw_output_bounded {
        update_obj.insert(
            "rawOutput".to_owned(),
            bounded_raw_output.cloned().unwrap_or(Value::Null),
        );
    }
    let mut params_obj = params.as_object().cloned().unwrap_or_default();
    params_obj.insert("update".to_owned(), Value::Object(update_obj));
    Value::Object(params_obj)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_call(detail: Option<&str>, status: Option<AcpToolCallStatus>) -> AcpToolCallState {
        AcpToolCallState {
            tool_call_id: "tool-1".to_owned(),
            title: Some("Grok Tool".to_owned()),
            kind: None,
            status,
            command: None,
            detail: detail.map(str::to_owned),
            data: Map::new(),
        }
    }

    mod decide_tool_call_update_emission {
        use super::*;

        #[test]
        fn emits_the_first_in_progress_tool_call_even_when_it_has_no_detail() {
            let next = AcpToolCallState {
                tool_call_id: "tool-1".to_owned(),
                title: Some("Grok Tool".to_owned()),
                kind: None,
                status: Some(AcpToolCallStatus::Pending),
                command: None,
                detail: None,
                data: Map::new(),
            };
            assert_eq!(
                decide_tool_call_update_emission(&AcpToolCallEmitDecisionInput {
                    previous: None,
                    next: &next,
                    last_emitted_detail_length: None,
                    skipped_since_emit: 0,
                }),
                AcpToolCallEmitDecision {
                    emit: true,
                    skipped_since_emit: 0,
                }
            );
        }

        #[test]
        fn always_emits_terminal_status_updates_regardless_of_growth() {
            let previous = tool_call(Some("same"), Some(AcpToolCallStatus::InProgress));
            assert_eq!(
                decide_tool_call_update_emission(&AcpToolCallEmitDecisionInput {
                    previous: Some(&previous),
                    next: &tool_call(Some("same"), Some(AcpToolCallStatus::Completed)),
                    last_emitted_detail_length: Some(4),
                    skipped_since_emit: 0,
                }),
                AcpToolCallEmitDecision {
                    emit: true,
                    skipped_since_emit: 0,
                }
            );
            assert_eq!(
                decide_tool_call_update_emission(&AcpToolCallEmitDecisionInput {
                    previous: Some(&previous),
                    next: &tool_call(Some("same"), Some(AcpToolCallStatus::Failed)),
                    last_emitted_detail_length: Some(4),
                    skipped_since_emit: 3,
                }),
                AcpToolCallEmitDecision {
                    emit: true,
                    skipped_since_emit: 0,
                }
            );
        }

        #[test]
        fn skips_updates_whose_bounded_detail_did_not_change() {
            let previous = tool_call(Some("frame 1"), Some(AcpToolCallStatus::InProgress));
            assert_eq!(
                decide_tool_call_update_emission(&AcpToolCallEmitDecisionInput {
                    previous: Some(&previous),
                    next: &previous,
                    last_emitted_detail_length: Some(7),
                    skipped_since_emit: 0,
                }),
                AcpToolCallEmitDecision {
                    emit: false,
                    skipped_since_emit: 0,
                }
            );
        }

        #[test]
        fn coalesces_command_tool_updates_whose_content_grew_while_detail_stayed_the_command() {
            fn command_call(stdout: &str) -> AcpToolCallState {
                AcpToolCallState {
                    tool_call_id: "tool-1".to_owned(),
                    title: Some("Ran command".to_owned()),
                    kind: None,
                    status: Some(AcpToolCallStatus::InProgress),
                    command: Some("ls".to_owned()),
                    detail: Some("ls".to_owned()),
                    data: Map::from_iter([
                        ("command".to_owned(), Value::String("ls".to_owned())),
                        (
                            "content".to_owned(),
                            Value::Array(vec![serde_json::json!({
                                "type": "content",
                                "content": { "type": "text", "text": stdout },
                            })]),
                        ),
                    ]),
                }
            }
            let mut previous: Option<AcpToolCallState> = None;
            let mut last_emitted_detail_length = None;
            let mut skipped_since_emit = 0;
            let mut emissions = Vec::new();
            for i in 1..=12 {
                let next = command_call(&"x".repeat(i));
                let decision = decide_tool_call_update_emission(&AcpToolCallEmitDecisionInput {
                    previous: previous.as_ref(),
                    next: &next,
                    last_emitted_detail_length,
                    skipped_since_emit,
                });
                emissions.push(decision.emit);
                skipped_since_emit = decision.skipped_since_emit;
                if decision.emit {
                    last_emitted_detail_length = Some(tool_call_progress_length(&next));
                }
                previous = Some(next);
            }
            let emitted_indices: Vec<usize> = emissions
                .iter()
                .enumerate()
                .filter_map(|(index, emitted)| emitted.then_some(index + 1))
                .collect();
            assert_eq!(emitted_indices, vec![1, 11]);
        }

        #[test]
        fn emits_pending_to_in_progress_status_changes_even_when_detail_and_output_are_unchanged() {
            assert_eq!(
                decide_tool_call_update_emission(&AcpToolCallEmitDecisionInput {
                    previous: Some(&tool_call(Some("same"), Some(AcpToolCallStatus::Pending))),
                    next: &tool_call(Some("same"), Some(AcpToolCallStatus::InProgress)),
                    last_emitted_detail_length: Some(4),
                    skipped_since_emit: 0,
                }),
                AcpToolCallEmitDecision {
                    emit: true,
                    skipped_since_emit: 0,
                }
            );
        }

        #[test]
        fn emits_immediately_when_the_title_changes_even_with_no_growth() {
            let previous = AcpToolCallState {
                tool_call_id: "tool-1".to_owned(),
                title: Some("Reading file".to_owned()),
                kind: None,
                status: None,
                command: None,
                detail: Some("x".to_owned()),
                data: Map::new(),
            };
            let next = AcpToolCallState {
                tool_call_id: "tool-1".to_owned(),
                title: Some("Ran command".to_owned()),
                kind: None,
                status: None,
                command: None,
                detail: Some("x".to_owned()),
                data: Map::new(),
            };
            assert_eq!(
                decide_tool_call_update_emission(&AcpToolCallEmitDecisionInput {
                    previous: Some(&previous),
                    next: &next,
                    last_emitted_detail_length: Some(1),
                    skipped_since_emit: 0,
                }),
                AcpToolCallEmitDecision {
                    emit: true,
                    skipped_since_emit: 0,
                }
            );
        }

        #[test]
        fn coalesces_small_deltas_but_forces_an_emission_after_the_coalesce_limit() {
            let mut last_emitted_detail_length = Some(0usize);
            let mut skipped_since_emit = 0;
            let mut emissions = Vec::new();
            let mut previous: Option<AcpToolCallState> = None;
            for i in 1..=12 {
                let next = tool_call(Some(&"x".repeat(i)), Some(AcpToolCallStatus::InProgress));
                let decision = decide_tool_call_update_emission(&AcpToolCallEmitDecisionInput {
                    previous: previous.as_ref(),
                    next: &next,
                    last_emitted_detail_length,
                    skipped_since_emit,
                });
                emissions.push(decision.emit);
                skipped_since_emit = decision.skipped_since_emit;
                if decision.emit {
                    last_emitted_detail_length = next.detail.as_ref().map(String::len);
                }
                previous = Some(next);
            }
            let emitted_indices: Vec<usize> = emissions
                .iter()
                .enumerate()
                .filter_map(|(index, emitted)| emitted.then_some(index + 1))
                .collect();
            assert_eq!(emitted_indices, vec![1, 11]);
        }

        #[test]
        fn retains_the_latest_replacement_snapshot_when_equal_length_updates_are_coalesced() {
            let mut previous = tool_call(Some("frame-a"), Some(AcpToolCallStatus::InProgress));
            let last_emitted_detail_length = previous.detail.as_ref().map(String::len);
            let mut skipped_since_emit = 0;
            for detail in ["frame-b", "frame-c"] {
                let next = merge_tool_call_state(
                    Some(&previous),
                    tool_call(Some(detail), Some(AcpToolCallStatus::InProgress)),
                );
                let decision = decide_tool_call_update_emission(&AcpToolCallEmitDecisionInput {
                    previous: Some(&previous),
                    next: &next,
                    last_emitted_detail_length,
                    skipped_since_emit,
                });
                assert!(!decision.emit);
                skipped_since_emit = decision.skipped_since_emit;
                previous = next;
            }
            let completed = merge_tool_call_state(
                Some(&previous),
                tool_call(None, Some(AcpToolCallStatus::Completed)),
            );
            assert_eq!(completed.detail.as_deref(), Some("frame-c"));
            assert_eq!(
                decide_tool_call_update_emission(&AcpToolCallEmitDecisionInput {
                    previous: Some(&previous),
                    next: &completed,
                    last_emitted_detail_length,
                    skipped_since_emit,
                }),
                AcpToolCallEmitDecision {
                    emit: true,
                    skipped_since_emit: 0,
                }
            );
        }
    }
}
