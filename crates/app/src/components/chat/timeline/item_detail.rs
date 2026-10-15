//! Port of upstream/packages/client-runtime/src/work-log/itemDetail.ts
//!
//! The mirror's file-search results carry only file names and web-search
//! results only `(title, url)`, so `turn_item_output_text` omits line numbers,
//! previews and snippets until the mirror grows them.

use serde_json::{Map, Value};

use super::js_text::js_trim;
use super::tool_output::{read_tool_output_image, tool_output_images};
use super::turn_item::{TurnItem, TurnItemPayload, TurnItemStatus};

const MAX_TEXT_BLOCK_DEPTH: usize = 4;

fn as_record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

/// Wire projection replaces a large dynamic input with `{ summary, truncated: true }`.
fn is_summarized_value(value: &Value) -> bool {
    as_record(value).is_some_and(|record| {
        record.get("truncated") == Some(&Value::Bool(true))
            && record.get("summary").is_some_and(Value::is_string)
    })
}

fn text_from_blocks(value: &Value, depth: usize) -> Option<String> {
    if depth > MAX_TEXT_BLOCK_DEPTH {
        return None;
    }
    match value {
        Value::String(text) => return Some(text.clone()),
        Value::Array(blocks) => {
            let parts: Option<Vec<String>> = blocks
                .iter()
                .map(|block| text_from_blocks(block, depth + 1))
                .collect();
            return parts.map(|parts| {
                parts
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        }
        _ => {}
    }
    let record = as_record(value)?;
    let block_type = record.get("type").and_then(Value::as_str);
    if block_type == Some("text")
        && let Some(text) = record.get("text").and_then(Value::as_str)
    {
        return Some(text.to_string());
    }
    // Images clients can show render on their own (see turn_item_output_images).
    if block_type == Some("image") {
        return Some(if read_tool_output_image(value).is_some() {
            String::new()
        } else {
            "[image]".to_string()
        });
    }
    if block_type == Some("resource_link")
        && let Some(uri) = record.get("uri").and_then(Value::as_str)
    {
        return Some(uri.to_string());
    }
    if block_type == Some("resource")
        && let Some(resource) = record.get("resource").and_then(Value::as_object)
    {
        if let Some(text) = resource.get("text").and_then(Value::as_str) {
            return Some(text.to_string());
        }
        if let Some(uri) = resource.get("uri").and_then(Value::as_str) {
            return Some(uri.to_string());
        }
    }
    let keys: Vec<&str> = record
        .keys()
        .map(String::as_str)
        .filter(|key| *key != "isError" && *key != "is_error")
        .collect();
    // MCP results and provider tool results wrap their text in `content`.
    // `structuredContent` usually repeats it as data, so it only shows when the
    // text is empty.
    let content = record.get("content");
    if keys == ["content"] {
        return content.and_then(|content| text_from_blocks(content, depth + 1));
    }
    if keys.len() == 2 && keys.contains(&"content") && keys.contains(&"structuredContent") {
        let text = content.and_then(|content| text_from_blocks(content, depth + 1));
        return text.filter(|text| !js_trim(text).is_empty());
    }
    None
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// MCP tools often return JSON as minified text, one document per line. Indent each.
fn pretty_json_text(text: &str) -> String {
    let trimmed = js_trim(text);
    if !trimmed.starts_with(['[', '{']) {
        return text.to_string();
    }
    if let Ok(whole) = serde_json::from_str::<Value>(trimmed) {
        return pretty_json(&whole);
    }
    let documents: Option<Vec<Value>> = trimmed
        .split('\n')
        .filter(|line| !js_trim(line).is_empty())
        .map(|line| serde_json::from_str::<Value>(js_trim(line)).ok())
        .collect();
    match documents {
        Some(documents) => documents
            .iter()
            .map(pretty_json)
            .collect::<Vec<_>>()
            .join("\n\n"),
        None => text.to_string(),
    }
}

/// Formats a tool input or output for display: text blocks as text, the rest as JSON.
fn format_tool_value(value: Option<&Value>, has_image_preview: bool) -> Option<String> {
    let value = value.filter(|value| !value.is_null())?;
    // Claude Read returns one structured image; lists may contain other images.
    if has_image_preview
        && as_record(value)
            .is_some_and(|record| record.get("type").and_then(Value::as_str) == Some("image"))
    {
        return None;
    }
    if let Some(text) = text_from_blocks(value, 0) {
        return (!js_trim(&text).is_empty()).then(|| pretty_json_text(&text));
    }
    let json = pretty_json(value);
    if json.is_empty() || json == "{}" || json == "[]" {
        return None;
    }
    Some(json)
}

/// Input of [`tool_call_lines`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ToolCallLinesInput<'a> {
    pub command: Option<&'a str>,
    pub args: Option<&'a Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolCallLines {
    pub command: Option<String>,
    pub args: Option<Vec<(String, String)>>,
    pub args_text: Option<String>,
}

/// The call a tool row's body shows above its result: the full command, the
/// arguments as `key value` pairs, or formatted text when they are not flat.
pub fn tool_call_lines(input: ToolCallLinesInput<'_>) -> ToolCallLines {
    if let Some(command) = input.command {
        // The row title truncates to its width, so the body always has the full command.
        let command = js_trim(command);
        return ToolCallLines {
            command: (!command.is_empty()).then(|| command.to_string()),
            ..Default::default()
        };
    }
    if let Some(args) = input.args.and_then(as_record)
        && !input.args.is_some_and(is_summarized_value)
    {
        let entries: Vec<(String, String)> = args
            .iter()
            .map(|(key, value)| {
                // An empty string or null can be the point of a call (a clear or reset), so show it.
                let text = match value {
                    Value::String(text) if !text.is_empty() => text.clone(),
                    value => serde_json::to_string(value).unwrap_or_default(),
                };
                (key.clone(), text)
            })
            .collect();
        return ToolCallLines {
            args: (!entries.is_empty()).then_some(entries),
            ..Default::default()
        };
    }
    ToolCallLines {
        args_text: format_tool_value(input.args, false),
        ..Default::default()
    }
}

fn tool_call_has_lines(lines: &ToolCallLines) -> bool {
    lines.command.is_some() || lines.args.is_some() || lines.args_text.is_some()
}

/// Cache key for a fetched item. A running item keeps one key, so an open row
/// fetches once while it streams and again when it finishes, not on every update.
pub fn turn_item_detail_revision(item: &TurnItem) -> String {
    match item.status {
        TurnItemStatus::Idle
        | TurnItemStatus::Pending
        | TurnItemStatus::Running
        | TurnItemStatus::Waiting => "live".to_string(),
        _ => item.updated_at.clone(),
    }
}

/// True when the timeline item withholds content that `getTurnItem` returns.
pub fn turn_item_needs_detail_fetch(item: &TurnItem) -> bool {
    match &item.payload {
        TurnItemPayload::CommandExecution { output_omitted, .. } => *output_omitted,
        TurnItemPayload::DynamicTool {
            output_omitted,
            input,
            ..
        } => *output_omitted || is_summarized_value(input),
        _ => false,
    }
}

/// Older Claude bash rows stored the raw `{ stdout, stderr, interrupted, ... }` result.
fn command_output_text(output: &str) -> String {
    if !output
        .trim_start_matches(super::js_text::is_js_whitespace)
        .starts_with("{\"stdout\"")
    {
        return output.to_string();
    }
    let Ok(parsed) = serde_json::from_str::<Value>(js_trim(output)) else {
        return output.to_string();
    };
    let (Some(stdout), Some(stderr), Some(_)) = (
        parsed.get("stdout").and_then(Value::as_str),
        parsed.get("stderr").and_then(Value::as_str),
        parsed.get("interrupted").and_then(Value::as_bool),
    ) else {
        return output.to_string();
    };
    [stdout, stderr]
        .into_iter()
        .filter(|part| !js_trim(part).is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The tool output carried by a fetched item, formatted for display.
pub fn turn_item_output_text(item: &TurnItem) -> Option<String> {
    match &item.payload {
        TurnItemPayload::CommandExecution { output, .. } => output
            .as_deref()
            .filter(|output| !js_trim(output).is_empty())
            .map(command_output_text)
            .filter(|text| !text.is_empty()),
        TurnItemPayload::DynamicTool {
            output_omitted: true,
            ..
        } => None,
        TurnItemPayload::DynamicTool {
            output,
            viewed_image_path,
            ..
        } => format_tool_value(output.as_ref(), viewed_image_path.is_some()),
        TurnItemPayload::FileSearch { results, .. } => {
            (!results.is_empty()).then(|| results.join("\n"))
        }
        TurnItemPayload::WebSearch { results, .. } => (!results.is_empty()).then(|| {
            results
                .iter()
                .map(|(title, url)| {
                    let heading = title
                        .as_deref()
                        .map(js_trim)
                        .filter(|title| !title.is_empty())
                        .or(url.as_deref());
                    let link = title.as_ref().and(url.as_deref());
                    [heading, link]
                        .into_iter()
                        .flatten()
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        }),
        _ => None,
    }
}

/// `Extract<AssetResource, { _tag: "tool-output-image" }>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutputImageResource {
    pub thread_id: String,
    pub item_id: String,
    pub index: usize,
}

/// Images in a fetched item's tool output, as assets. The detail read leaves
/// the bytes out, so each loads by its index.
pub fn turn_item_output_images(item: &TurnItem) -> Vec<ToolOutputImageResource> {
    let TurnItemPayload::DynamicTool {
        output: Some(output),
        output_omitted: false,
        ..
    } = &item.payload
    else {
        return Vec::new();
    };
    (0..tool_output_images(output).len())
        .map(|index| ToolOutputImageResource {
            thread_id: item.thread_id.clone(),
            item_id: item.id.clone(),
            index,
        })
        .collect()
}

/// Whether expanding the item shows anything. Rows without content must not
/// offer a disclosure, otherwise they open to an empty panel.
pub fn turn_item_has_detail(item: &TurnItem) -> bool {
    let non_blank = |text: &str| !js_trim(text).is_empty();
    match &item.payload {
        TurnItemPayload::Reasoning { text, .. } => non_blank(text),
        TurnItemPayload::CommandExecution {
            input,
            output,
            output_omitted,
            exit_code,
            ..
        } => {
            tool_call_lines(ToolCallLinesInput {
                command: Some(input),
                args: None,
            })
            .command
            .is_some()
                || *output_omitted
                || output.as_deref().is_some_and(non_blank)
                || exit_code.is_some_and(|code| code != 0)
        }
        TurnItemPayload::FileChange { .. }
        | TurnItemPayload::Checkpoint { .. }
        | TurnItemPayload::Fork { .. }
        | TurnItemPayload::Handoff { .. } => true,
        TurnItemPayload::FileSearch { pattern, results } => {
            !results.is_empty() || pattern.as_deref().is_some_and(non_blank)
        }
        TurnItemPayload::WebSearch { patterns, results } => {
            !results.is_empty() || !patterns.is_empty()
        }
        TurnItemPayload::DynamicTool {
            output_omitted,
            input,
            ..
        } => {
            *output_omitted
                || tool_call_has_lines(&tool_call_lines(ToolCallLinesInput {
                    command: None,
                    args: Some(input),
                }))
        }
        TurnItemPayload::ApprovalRequest { prompt, .. } => prompt.as_deref().is_some_and(non_blank),
        TurnItemPayload::UserInputRequest { questions, .. } => !questions.is_empty(),
        TurnItemPayload::Notification { detail, .. } => detail.as_deref().is_some_and(non_blank),
        TurnItemPayload::SystemNotice { message } => non_blank(message),
        TurnItemPayload::Error { failure, .. } => non_blank(&failure.message),
        TurnItemPayload::ProposedPlan { markdown, .. } => non_blank(markdown),
        TurnItemPayload::TodoList { steps, .. } => !steps.is_empty(),
        TurnItemPayload::Subagent {
            child_thread_id, ..
        } => child_thread_id.is_some(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn screenshot(output: Value) -> TurnItem {
        TurnItem {
            started_at: Some("2026-10-05T00:00:00.000Z".into()),
            completed_at: Some("2026-10-05T00:00:01.000Z".into()),
            ordinal: 1,
            ..TurnItem::new(
                "tool-screenshot",
                "thread-1",
                TurnItemStatus::Completed,
                "2026-10-05T00:00:01.000Z",
                TurnItemPayload::DynamicTool {
                    tool_name: Some("mcp__t3-code__device_screenshot".into()),
                    viewed_image_path: None,
                    input: json!({ "deviceId": "phone" }),
                    output: Some(output),
                    output_omitted: false,
                },
            )
        }
    }

    fn png_block() -> Value {
        json!({ "type": "image", "mimeType": "image/png" })
    }

    fn with_preview(mut item: TurnItem, path: Option<&str>, tool_name: Option<&str>) -> TurnItem {
        if let TurnItemPayload::DynamicTool {
            viewed_image_path,
            tool_name: name,
            ..
        } = &mut item.payload
        {
            *viewed_image_path = path.map(str::to_string);
            if let Some(tool_name) = tool_name {
                *name = Some(tool_name.to_string());
            }
        }
        item
    }

    #[test]
    fn shows_a_screenshot_as_an_image_asset_not_as_text() {
        let item = screenshot(json!({
            "content": [{ "type": "text", "text": "Captured the home screen." }, png_block()],
        }));
        assert_eq!(
            turn_item_output_images(&item),
            vec![ToolOutputImageResource {
                thread_id: "thread-1".into(),
                item_id: "tool-screenshot".into(),
                index: 0,
            }]
        );
        assert_eq!(
            turn_item_output_text(&item).as_deref(),
            Some("Captured the home screen.")
        );
        assert_eq!(
            turn_item_output_text(&screenshot(json!({ "content": [png_block()] }))),
            None
        );
    }

    #[test]
    fn keeps_a_placeholder_for_images_it_cannot_show() {
        let item =
            screenshot(json!({ "content": [{ "type": "image", "mimeType": "image/svg+xml" }] }));
        assert_eq!(turn_item_output_images(&item), vec![]);
        assert_eq!(turn_item_output_text(&item).as_deref(), Some("[image]"));
    }

    #[test]
    fn omits_claude_reads_image_placeholder_when_the_file_has_a_preview() {
        let output = json!({
            "type": "image",
            "file": { "filePath": "/tmp/blk.png", "type": "image/png", "sizeBytes": 1024 },
        });
        let item = with_preview(screenshot(output), Some("/tmp/blk.png"), Some("Read"));
        assert_eq!(turn_item_output_images(&item), vec![]);
        assert_eq!(turn_item_output_text(&item), None);
        assert_eq!(
            turn_item_output_text(&with_preview(item, None, None)).as_deref(),
            Some("[image]")
        );
    }

    #[test]
    fn keeps_accompanying_text_and_literal_image_markers_when_the_file_has_a_preview() {
        let output = json!({
            "content": [
                { "type": "text", "text": "Read /tmp/blk.png. Literal marker: [image]" },
                png_block(),
            ],
        });
        let item = with_preview(screenshot(output), Some("/tmp/blk.png"), None);
        assert_eq!(
            turn_item_output_text(&item).as_deref(),
            Some("Read /tmp/blk.png. Literal marker: [image]")
        );
    }

    #[test]
    fn keeps_placeholders_for_unserved_images_in_a_list_despite_a_file_preview() {
        let blocks = json!([
            { "type": "text", "text": "Read /tmp/blk.png." },
            { "type": "image", "file": { "type": "image/png", "sizeBytes": 1024 } },
            png_block(),
            { "type": "image", "mimeType": "image/svg+xml" },
        ]);
        for output in [
            blocks.clone(),
            json!({ "content": blocks }),
            json!({ "content": blocks, "isError": false }),
        ] {
            let item = with_preview(screenshot(output), Some("/tmp/blk.png"), None);
            assert_eq!(
                turn_item_output_text(&item).as_deref(),
                Some("Read /tmp/blk.png.\n[image]\n[image]")
            );
            assert_eq!(turn_item_output_images(&item).len(), 1);
        }
    }
}
