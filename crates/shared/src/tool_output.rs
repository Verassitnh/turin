//! Port of upstream/packages/shared/src/toolOutput.ts
//!
//! Tool output handling: the compact ID and failure envelope kept on a tool item, command failure
//! phrases, and the inline images a tool returned. The HTML-render and MCP-app references are not
//! ported (both features are cut), so `compact_dynamic_tool_output` keeps no `htmlRender` or
//! `mcpApp` fields.

use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use turin_contracts::chat_attachment::{
    PROVIDER_SEND_TURN_MAX_IMAGE_BYTES, is_provider_send_turn_supported_image_mime_type,
};

use crate::js_string::utf16_len;

const MAX_PARSED_BYTES: i64 = 16_384;
const MAX_METADATA_BYTES: usize = 8_192;
const MAX_ID_LENGTH: usize = 256;
const MAX_THREADS: usize = 100;
const MAX_CONTENT_BLOCKS: usize = 32;
const MAX_ENVELOPE_DEPTH: usize = 4;
const MAX_ENVELOPE_NODES: i64 = 128;

type Record = Map<String, Value>;

static NULL: Value = Value::Null;

/// `value[key]`, or `undefined` (`Value::Null`) when absent or when `value` is not an object.
fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&NULL)
}

struct ResultReadBudget {
    remaining_bytes: i64,
    remaining_nodes: i64,
    exceeded: bool,
}

struct ResultEnvelope {
    data: Option<Record>,
    failed: bool,
}

impl ResultEnvelope {
    fn none() -> Self {
        Self {
            data: None,
            failed: false,
        }
    }
}

fn read_result(value: &Value, budget: &mut ResultReadBudget, depth: usize) -> ResultEnvelope {
    // `budget.remainingNodes-- <= 0`: the counter only moves when the depth check passes.
    if depth > MAX_ENVELOPE_DEPTH {
        budget.exceeded = true;
        return ResultEnvelope::none();
    }
    let exhausted = budget.remaining_nodes <= 0;
    budget.remaining_nodes -= 1;
    if exhausted {
        budget.exceeded = true;
        return ResultEnvelope::none();
    }
    if let Some(text) = value.as_str() {
        // JS compares UTF-16 length first, then UTF-8 bytes.
        if utf16_len(text) as i64 > budget.remaining_bytes {
            budget.exceeded = true;
            return ResultEnvelope::none();
        }
        let bytes = text.len() as i64;
        if bytes > budget.remaining_bytes {
            budget.exceeded = true;
            return ResultEnvelope::none();
        }
        budget.remaining_bytes -= bytes;
        // `JSON.parse` failures are swallowed as "no envelope".
        return match serde_json::from_str::<Value>(text) {
            Ok(parsed) => read_result(&parsed, budget, depth + 1),
            Err(_) => ResultEnvelope::none(),
        };
    }
    if let Some(blocks) = value.as_array() {
        if blocks.len() > MAX_CONTENT_BLOCKS {
            budget.exceeded = true;
            return ResultEnvelope::none();
        }
        let mut data = None;
        let mut failed = false;
        for block in blocks {
            // `Predicate.isObject(text) ? (text.text ?? text) : text`
            let text = field(block, "text");
            let input = match text.get("text") {
                Some(inner) if text.is_object() && !inner.is_null() => inner,
                _ => text,
            };
            let result = read_result(input, budget, depth + 1);
            if data.is_none() {
                data = result.data;
            }
            failed |= result.failed;
            if budget.exceeded {
                break;
            }
        }
        return ResultEnvelope { data, failed };
    }
    let Some(object) = value.as_object() else {
        return ResultEnvelope::none();
    };
    let failed = field(value, "isError") == &Value::Bool(true)
        || field(value, "is_error") == &Value::Bool(true)
        || field(value, "_tag").as_str() == Some("OrchestratorMcpFailure")
        || !field(value, "error").is_null();
    // `value.structuredContent ?? value.content`
    let structured = field(value, "structuredContent");
    let content = if structured.is_null() {
        object.get("content")
    } else {
        Some(structured)
    };
    if let Some(content) = content {
        let nested = read_result(content, budget, depth + 1);
        return ResultEnvelope {
            data: nested.data,
            failed: failed || nested.failed,
        };
    }
    ResultEnvelope {
        data: Some(object.clone()),
        failed,
    }
}

/// A string ID of at most 256 UTF-16 units that is not blank. Returned untrimmed, as upstream does.
fn bounded_id(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    if utf16_len(text) > MAX_ID_LENGTH || text.trim().is_empty() {
        return None;
    }
    Some(text.to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactThread {
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactThreadRow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// The IDs and failure metadata T3's grouped tool summaries read, with result bodies dropped.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactToolOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<CompactThread>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threads: Option<Vec<CompactThreadRow>>,
}

/// Keeps only IDs and failure metadata used by T3's grouped tool summaries.
pub fn compact_dynamic_tool_output(value: &Value) -> Option<CompactToolOutput> {
    let mut budget = ResultReadBudget {
        remaining_bytes: MAX_PARSED_BYTES,
        remaining_nodes: MAX_ENVELOPE_NODES,
        exceeded: false,
    };
    let result = read_result(value, &mut budget, 0);
    let mut output = CompactToolOutput {
        is_error: result.failed.then_some(true),
        ..CompactToolOutput::default()
    };
    let data = if budget.exceeded { None } else { result.data };
    if let Some(data) = data {
        output.thread_id = bounded_id(data.get("threadId"));
        output.message_id = bounded_id(data.get("messageId"));
        output.task_id = bounded_id(data.get("taskId"));
        output.scheduled_task_id = bounded_id(data.get("scheduledTaskId"));
        if data.get("status").and_then(Value::as_str) == Some("rolled_back") {
            output.status = Some("rolled_back".to_owned());
        }
        let nested_thread_id = data
            .get("thread")
            .filter(|thread| thread.is_object())
            .and_then(|thread| bounded_id(thread.get("threadId")));
        if let Some(thread_id) = nested_thread_id {
            output.thread = Some(CompactThread { thread_id });
        }

        if let Some(entries) = data.get("threads").and_then(Value::as_array) {
            let mut complete = entries.len() <= MAX_THREADS;
            let mut rows = Vec::new();
            if complete {
                for entry in entries {
                    let Some(entry) = entry.as_object() else {
                        complete = false;
                        break;
                    };
                    let thread_id = bounded_id(entry.get("threadId"));
                    let rolled_back =
                        entry.get("status").and_then(Value::as_str) == Some("rolled_back");
                    if thread_id.is_none() && !rolled_back {
                        complete = false;
                        break;
                    }
                    rows.push(CompactThreadRow {
                        thread_id,
                        status: rolled_back.then(|| "rolled_back".to_owned()),
                    });
                }
            }
            if complete {
                output.threads = Some(rows);
            } else {
                // A partial batch or a top-level ID would turn an unknown creation count into a
                // confidently wrong one in the existing summary parser.
                output.thread_id = None;
                output.status = None;
            }
        }
    }
    let oversized = |output: &CompactToolOutput| {
        serde_json::to_vec(output).is_ok_and(|bytes| bytes.len() > MAX_METADATA_BYTES)
    };
    if oversized(&output) {
        output.threads = None;
        output.thread_id = None;
        output.status = None;
    }
    if output == CompactToolOutput::default() {
        None
    } else {
        Some(output)
    }
}

/// Some providers report completion even when command output describes a failure.
pub fn tool_output_indicates_failure(text: &str) -> bool {
    fn patterns() -> &'static [Regex; 6] {
        static PATTERNS: OnceLock<[Regex; 6]> = OnceLock::new();
        PATTERNS.get_or_init(|| {
            [
                Regex::new(r"(?i)file not found|no files found|enoent|no such file|commandnotfoundexception|command not found|is not recognized as the name of a cmdlet|a parameter cannot be found that matches parameter name")
                    .expect("valid regex"),
                Regex::new(r"(?i)cannot find path").expect("valid regex"),
                Regex::new(r"(?i)because it does not exist").expect("valid regex"),
                Regex::new(r"(?i)is not recognized").expect("valid regex"),
                Regex::new(r"(?i)the term '").expect("valid regex"),
                Regex::new(r"(?i)<exited with exit code\s+[1-9][0-9]*\s*>").expect("valid regex"),
            ]
        })
    }
    fn exit_patterns() -> &'static [Regex; 2] {
        static PATTERNS: OnceLock<[Regex; 2]> = OnceLock::new();
        PATTERNS.get_or_init(|| {
            [
                Regex::new(r"(?i)exit(?:ed)? with exit code\s+[1-9][0-9]*").expect("valid regex"),
                Regex::new(r"(?i)exit code\s*[:\s]\s*[1-9][0-9]*(?-u:\b)").expect("valid regex"),
            ]
        })
    }
    let [
        missing,
        cannot_find_path,
        does_not_exist,
        not_recognized,
        the_term,
        exited_marker,
    ] = patterns();
    missing.is_match(text)
        || (cannot_find_path.is_match(text) && does_not_exist.is_match(text))
        || (not_recognized.is_match(text) && the_term.is_match(text))
        || exited_marker.is_match(text)
        || exit_patterns().iter().any(|pattern| pattern.is_match(text))
}

/// An image a tool returned inline. `data` is base64; a detail read omits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutputImage {
    pub mime_type: String,
    pub data: Option<String>,
}

/// Reads one image block in the MCP `{ data, mimeType }` shape or the Anthropic
/// `{ source: { type: "base64", media_type, data } }` shape Claude stores. Only raster types are
/// recognized, so the server never serves an agent's SVG or HTML inline.
pub fn read_tool_output_image(block: &Value) -> Option<ToolOutputImage> {
    if block.get("type").and_then(Value::as_str) != Some("image") {
        return None;
    }
    // `Predicate.isObject` covers arrays too, so an array `source` is rejected as not base64.
    let source = field(block, "source");
    let source = (source.is_object() || source.is_array()).then_some(source);
    if let Some(source) = source
        && source.get("type").and_then(Value::as_str) != Some("base64")
    {
        return None;
    }
    let (mime_type, data) = match source {
        Some(source) => (field(source, "media_type"), field(source, "data")),
        None => (field(block, "mimeType"), field(block, "data")),
    };
    let mime_type = mime_type.as_str()?;
    if !is_provider_send_turn_supported_image_mime_type(mime_type) {
        return None;
    }
    Some(ToolOutputImage {
        mime_type: mime_type.to_lowercase(),
        data: data.as_str().map(str::to_owned),
    })
}

/// Tools return a block, a list of blocks, or an MCP result with a `content` list.
fn output_blocks(value: &Value) -> Vec<&Value> {
    if let Some(blocks) = value.as_array() {
        return blocks.iter().collect();
    }
    if let Some(blocks) = value.get("content").and_then(Value::as_array) {
        return blocks.iter().collect();
    }
    vec![value]
}

/// A tool returns one screenshot or a few frames; more would only flood the timeline.
pub const MAX_TOOL_OUTPUT_IMAGES: usize = 8;

/// The largest image a `tool-output-image` asset serves: a provider turn's limit, as base64.
pub const MAX_TOOL_OUTPUT_IMAGE_BASE64_LENGTH: u64 =
    PROVIDER_SEND_TURN_MAX_IMAGE_BYTES.div_ceil(3) * 4;

/// The blocks [`tool_output_images`] reads, by reference. These hold the only image bytes in a tool
/// output that a `tool-output-image` asset can serve, up to [`MAX_TOOL_OUTPUT_IMAGE_BASE64_LENGTH`].
pub fn tool_output_image_blocks(value: &Value) -> Vec<&Value> {
    output_blocks(value)
        .into_iter()
        .filter(|block| read_tool_output_image(block).is_some())
        .take(MAX_TOOL_OUTPUT_IMAGES)
        .collect()
}

/// The first images in a tool output, in order. The order is the `tool-output-image` asset index,
/// so servers and clients agree on it.
pub fn tool_output_images(value: &Value) -> Vec<ToolOutputImage> {
    tool_output_image_blocks(value)
        .into_iter()
        .filter_map(read_tool_output_image)
        .collect()
}

/// Replaces each image's bytes with `{ type: "image", mimeType }` and keeps its position, so detail
/// reads stay small and clients load the bytes as assets.
pub fn omit_tool_output_image_data(value: &Value) -> Value {
    fn omit(block: &Value) -> Value {
        match read_tool_output_image(block) {
            Some(ToolOutputImage {
                mime_type,
                data: Some(_),
            }) => serde_json::json!({"type": "image", "mimeType": mime_type}),
            _ => block.clone(),
        }
    }
    if let Some(blocks) = value.as_array() {
        return Value::Array(blocks.iter().map(omit).collect());
    }
    if let Some(blocks) = value.get("content").and_then(Value::as_array) {
        let mut object = value.as_object().cloned().unwrap_or_default();
        object.insert(
            "content".to_owned(),
            Value::Array(blocks.iter().map(omit).collect()),
        );
        return Value::Object(object);
    }
    omit(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_ids_through_the_mcp_result_envelopes_used_by_t3_summaries() {
        let metadata = CompactToolOutput {
            thread_id: Some("thread-1".to_owned()),
            message_id: Some("message-1".to_owned()),
            ..CompactToolOutput::default()
        };
        let body = json!({"threadId": "thread-1", "messageId": "message-1", "response": "Private response body"});
        let json_text = body.to_string();
        let values = [
            body.clone(),
            json!({"structuredContent": serde_json::from_str::<Value>(&json_text).unwrap(), "content": "Ignored alternative body"}),
            json!([{"type": "text", "text": json_text}]),
            json!({"content": [{"text": {"text": json_text}}], "isError": false}),
            Value::String(json_text.clone()),
        ];
        for value in values {
            assert_eq!(compact_dynamic_tool_output(&value), Some(metadata.clone()));
        }
    }

    #[test]
    fn keeps_nested_thread_identity_and_scalar_task_ids_without_result_bodies() {
        let output = json!({
            "thread": {"threadId": "thread-1", "messages": ["Private message"]},
            "taskId": "task-1",
            "scheduledTaskId": "scheduled-1",
            "status": "failed",
            "summary": "A child failed; the tool itself succeeded.",
        });
        assert_eq!(
            serde_json::to_value(compact_dynamic_tool_output(&output)).unwrap(),
            json!({
                "thread": {"threadId": "thread-1"},
                "taskId": "task-1",
                "scheduledTaskId": "scheduled-1",
            })
        );
        assert_eq!(output["thread"]["messages"], json!(["Private message"]));
    }

    #[test]
    fn keeps_explicit_envelope_failure_while_dropping_error_text_and_arbitrary_output() {
        for failure in [
            json!({"isError": true}),
            json!({"is_error": true}),
            json!({"_tag": "OrchestratorMcpFailure"}),
            json!({"error": {"message": "Private error"}}),
        ] {
            let mut value = failure.as_object().unwrap().clone();
            value.insert(
                "structuredContent".to_owned(),
                json!({"threadId": "thread-1", "error": "Private details"}),
            );
            assert_eq!(
                serde_json::to_value(compact_dynamic_tool_output(&Value::Object(value))).unwrap(),
                json!({"isError": true, "threadId": "thread-1"})
            );
        }
        assert_eq!(
            compact_dynamic_tool_output(&json!("Private plain text")),
            None
        );
        assert_eq!(
            compact_dynamic_tool_output(&json!({"result": {"body": "Private result"}})),
            None
        );
    }

    #[test]
    fn uses_the_first_result_data_but_combines_explicit_failure_flags_across_content_blocks() {
        assert_eq!(
            serde_json::to_value(compact_dynamic_tool_output(&json!([
                {"text": json!({"threadId": "first"}).to_string()},
                {"text": json!({"threadId": "second", "isError": true, "error": "Private error"}).to_string()},
            ])))
            .unwrap(),
            json!({"threadId": "first", "isError": true})
        );
        assert_eq!(
            compact_dynamic_tool_output(&json!([
                {"text": "{}"},
                {"text": json!({"threadId": "second"}).to_string()},
            ])),
            None
        );
    }

    #[test]
    fn keeps_complete_thread_creation_batches_and_rollback_markers_without_row_bodies() {
        assert_eq!(
            serde_json::to_value(compact_dynamic_tool_output(&json!({
                "threads": [
                    {"threadId": "created", "status": "running", "messages": ["Private message"]},
                    {"threadId": "reverted", "status": "rolled_back", "error": "Private error"},
                    {"status": "rolled_back"},
                ],
            })))
            .unwrap(),
            json!({
                "threads": [
                    {"threadId": "created"},
                    {"threadId": "reverted", "status": "rolled_back"},
                    {"status": "rolled_back"},
                ],
            })
        );
        assert_eq!(
            serde_json::to_value(compact_dynamic_tool_output(
                &json!({"threadId": "reverted", "status": "rolled_back"})
            ))
            .unwrap(),
            json!({"threadId": "reverted", "status": "rolled_back"})
        );
        assert_eq!(
            serde_json::to_value(compact_dynamic_tool_output(&json!({"threads": []}))).unwrap(),
            json!({"threads": []})
        );
    }

    #[test]
    fn omits_incomplete_or_oversized_creation_evidence_instead_of_returning_a_partial_count() {
        let batches = [
            json!([{"threadId": "known"}, {"title": "No confirmed ID"}]),
            Value::Array((0..101).map(|index| json!({"threadId": format!("thread-{index}")})).collect()),
            Value::Array(
                (0..100)
                    .map(|index| json!({"threadId": format!("{index}").to_string() + &"x".repeat(256 - format!("{index}").len())}))
                    .collect(),
            ),
            json!([{"threadId": "x".repeat(257)}]),
        ];
        for threads in batches {
            let value = json!({
                "threadId": "must-not-be-counted-as-one",
                "status": "rolled_back",
                "taskId": "task-1",
                "threads": threads,
            });
            assert_eq!(
                serde_json::to_value(compact_dynamic_tool_output(&value)).unwrap(),
                json!({"taskId": "task-1"})
            );
        }
        let normal_batch = json!({
            "threads": (0..100)
                .map(|index| json!({"threadId": format!("00000000-0000-4000-8000-{index:012}")}))
                .collect::<Vec<_>>(),
        });
        let compact = compact_dynamic_tool_output(&normal_batch).unwrap();
        assert_eq!(serde_json::to_value(&compact).unwrap(), normal_batch);
        assert!(serde_json::to_vec(&compact).unwrap().len() <= 8_192);
    }

    #[test]
    fn bounds_parsing_and_envelope_scanning_while_preserving_an_outer_failure_marker() {
        let oversized_json = json!({"threadId": "hidden", "body": "😄".repeat(5_000)}).to_string();
        let contents = [
            Value::String("x".repeat(1_000_000)),
            Value::String(oversized_json),
            Value::Array(
                (0..33)
                    .map(|_| json!({"text": json!({"threadId": "hidden"}).to_string()}))
                    .collect(),
            ),
            json!({"content": {"content": {"content": {"content": {"threadId": "too-deep"}}}}}),
        ];
        for content in contents {
            let value = json!({"isError": true, "content": content});
            assert_eq!(
                serde_json::to_value(compact_dynamic_tool_output(&value)).unwrap(),
                json!({"isError": true})
            );
        }
        assert_eq!(
            serde_json::to_value(compact_dynamic_tool_output(
                &json!({"threadId": "known", "body": "x".repeat(1_000_000)})
            ))
            .unwrap(),
            json!({"threadId": "known"})
        );
    }

    #[test]
    fn is_idempotent_for_compact_metadata() {
        let compact = compact_dynamic_tool_output(&json!({
            "isError": true,
            "structuredContent": {
                "threadId": "thread-1",
                "threads": [{"threadId": "thread-1"}, {"status": "rolled_back"}],
                "output": "Private output",
            },
        }))
        .unwrap();
        assert_eq!(
            compact_dynamic_tool_output(&serde_json::to_value(&compact).unwrap()),
            Some(compact)
        );
    }

    #[test]
    fn preserves_the_command_failure_phrases_across_shells_without_copying_the_output() {
        for text in [
            "FILE NOT FOUND",
            "No files found",
            "ENOENT",
            "No such file or directory",
            "CommandNotFoundException",
            "command not found",
            "Cannot find path 'a' because it does not exist",
            "The term 'example' is not recognized",
            "is not recognized as the name of a cmdlet",
            "A parameter cannot be found that matches parameter name",
            "<exited with exit code 2>",
            "Exited with exit code 1",
            "exit code: 127",
        ] {
            assert!(tool_output_indicates_failure(text), "{text}");
        }
    }

    #[test]
    fn does_not_mark_successful_exit_codes_or_incomplete_failure_phrases() {
        for text in [
            "Done",
            "exit code: 0",
            "Exited with exit code 0",
            "cannot find path",
            "is not recognized",
        ] {
            assert!(!tool_output_indicates_failure(text), "{text}");
        }
    }

    #[test]
    fn finds_images_in_each_place_providers_store_them_in_order() {
        let mcp_image = json!({"type": "image", "data": "AAAA", "mimeType": "image/png"});
        let claude_image = json!({"type": "image", "source": {"type": "base64", "media_type": "image/JPEG", "data": "BBBB"}});
        let text = json!({"type": "text", "text": "{}"});
        assert_eq!(
            tool_output_images(&json!({"content": [text, claude_image, mcp_image]})),
            vec![
                ToolOutputImage {
                    mime_type: "image/jpeg".to_owned(),
                    data: Some("BBBB".to_owned())
                },
                ToolOutputImage {
                    mime_type: "image/png".to_owned(),
                    data: Some("AAAA".to_owned())
                },
            ]
        );
        let png = ToolOutputImage {
            mime_type: "image/png".to_owned(),
            data: Some("AAAA".to_owned()),
        };
        assert_eq!(
            tool_output_images(&json!([mcp_image.clone()])),
            vec![png.clone()]
        );
        assert_eq!(tool_output_images(&mcp_image), vec![png]);
    }

    #[test]
    fn skips_types_the_server_must_not_serve_inline() {
        let mcp_image = json!({"type": "image", "data": "AAAA", "mimeType": "image/png"});
        assert!(
            tool_output_images(&json!([
                {"type": "image", "data": "AAAA", "mimeType": "image/svg+xml"},
                {"type": "image", "data": "AAAA", "mimeType": "text/html"},
                {"type": "image", "source": {"type": "url", "url": "https://example.com/a.png"}},
                mcp_image.clone(),
            ]))
            .iter()
            .all(|image| image.mime_type == "image/png")
        );
        assert_eq!(
            tool_output_images(&json!([
                {"type": "image", "data": "AAAA", "mimeType": "image/svg+xml"},
                {"type": "image", "data": "AAAA", "mimeType": "text/html"},
                {"type": "image", "source": {"type": "url", "url": "https://example.com/a.png"}},
            ])),
            vec![]
        );
    }

    #[test]
    fn stops_after_the_first_few_images() {
        let mcp_image = json!({"type": "image", "data": "AAAA", "mimeType": "image/png"});
        let images = tool_output_images(&Value::Array(vec![mcp_image; 10_000]));
        assert_eq!(images.len(), MAX_TOOL_OUTPUT_IMAGES);
    }

    #[test]
    fn drops_the_bytes_but_keeps_each_images_position() {
        let mcp_image = json!({"type": "image", "data": "AAAA", "mimeType": "image/png"});
        let claude_image = json!({"type": "image", "source": {"type": "base64", "media_type": "image/JPEG", "data": "BBBB"}});
        let text = json!({"type": "text", "text": "{}"});
        let omitted = omit_tool_output_image_data(
            &json!({"content": [text.clone(), claude_image, mcp_image]}),
        );
        assert_eq!(
            omitted,
            json!({"content": [
                text,
                {"type": "image", "mimeType": "image/jpeg"},
                {"type": "image", "mimeType": "image/png"},
            ]})
        );
        assert_eq!(
            tool_output_images(&omitted),
            vec![
                ToolOutputImage {
                    mime_type: "image/jpeg".to_owned(),
                    data: None
                },
                ToolOutputImage {
                    mime_type: "image/png".to_owned(),
                    data: None
                },
            ]
        );
    }
}
