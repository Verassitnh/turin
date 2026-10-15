//! Port of upstream/packages/shared/src/toolOutput.ts
//!
//! Cut (HTML render and MCP Apps are cut features): `htmlRenderFromToolItem`,
//! `mcpAppFromToolItem`, and the `htmlRender` / MCP-app reference fields of
//! `CompactToolOutput` (with the oversized-output steps that only trim the
//! MCP app reference).
//!
//! `PROVIDER_SEND_TURN_MAX_IMAGE_BYTES` and
//! `isProviderSendTurnSupportedImageMimeType` come from
//! upstream/packages/contracts/src/chatAttachment.ts; `turin-contracts` does
//! not have them yet, so they are mirrored here.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use turin_contracts::base_schemas::js_trim;

/// `PROVIDER_SEND_TURN_MAX_IMAGE_BYTES` (contracts/chatAttachment.ts).
pub const PROVIDER_SEND_TURN_MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// `PROVIDER_SEND_TURN_SUPPORTED_IMAGE_MIME_TYPES` (contracts/chatAttachment.ts).
pub const PROVIDER_SEND_TURN_SUPPORTED_IMAGE_MIME_TYPES: [&str; 4] =
    ["image/gif", "image/jpeg", "image/png", "image/webp"];

/// Whether a pasted or picked image mime type can be sent on a provider turn.
/// (`isProviderSendTurnSupportedImageMimeType`, contracts/chatAttachment.ts.)
pub fn is_provider_send_turn_supported_image_mime_type(mime_type: &str) -> bool {
    PROVIDER_SEND_TURN_SUPPORTED_IMAGE_MIME_TYPES.contains(&mime_type.to_lowercase().as_str())
}

const MAX_PARSED_BYTES: usize = 16_384;
const MAX_METADATA_BYTES: usize = 8_192;
const MAX_ID_LENGTH: usize = 256;
const MAX_THREADS: usize = 100;
const MAX_CONTENT_BLOCKS: usize = 32;
const MAX_ENVELOPE_DEPTH: usize = 4;
const MAX_ENVELOPE_NODES: i64 = 128;

type Record = Map<String, Value>;

struct ResultReadBudget {
    remaining_bytes: usize,
    remaining_nodes: i64,
    exceeded: bool,
}

struct ResultEnvelope<'a> {
    data: Option<Cow<'a, Record>>,
    failed: bool,
}

impl ResultEnvelope<'_> {
    const NOT_FAILED: Self = Self {
        data: None,
        failed: false,
    };
}

/// `CompactToolOutput["status"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CompactToolOutputStatus {
    #[serde(rename = "rolled_back")]
    RolledBack,
}

/// `CompactToolOutput["thread"]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactThreadRef {
    pub thread_id: String,
}

/// `CompactToolOutput["threads"][number]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactThreadEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<CompactToolOutputStatus>,
}

/// `CompactToolOutput`, minus the cut `htmlRender` / MCP app references.
/// Field order is upstream's key insertion order, so the encoded size check
/// measures the same JSON.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactToolOutput {
    /// Upstream `isError?: true`: only ever encoded as `true`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<CompactToolOutputStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<CompactThreadRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threads: Option<Vec<CompactThreadEntry>>,
}

impl CompactToolOutput {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// `JSON.stringify(output)`: the stored, wire-shaped JSON.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    fn encoded_len(&self) -> usize {
        serde_json::to_string(self).map_or(usize::MAX, |json| json.len())
    }
}

/// `Predicate.isObject`: any non-null object, arrays included.
fn is_object(value: &Value) -> bool {
    matches!(value, Value::Object(_) | Value::Array(_))
}

/// `value[key]` on a `Predicate.isObject` value (arrays have no string keys).
fn property<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_object().and_then(|record| record.get(key))
}

/// JS `a ?? b` on property reads: `null` counts as missing for `a`.
fn nullish(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !value.is_null())
}

fn read_result<'a>(
    value: Option<&'a Value>,
    budget: &mut ResultReadBudget,
    depth: usize,
) -> ResultEnvelope<'a> {
    if depth > MAX_ENVELOPE_DEPTH || {
        // `budget.remainingNodes-- <= 0`
        let remaining = budget.remaining_nodes;
        budget.remaining_nodes -= 1;
        remaining <= 0
    } {
        budget.exceeded = true;
        return ResultEnvelope::NOT_FAILED;
    }
    let Some(value) = value else {
        return ResultEnvelope::NOT_FAILED;
    };
    if let Value::String(text) = value {
        // Upstream checks the UTF-16 length first as a cheap pre-check, then
        // the UTF-8 byte length. `str::len` is already the byte length.
        let bytes = text.len();
        if bytes > budget.remaining_bytes {
            budget.exceeded = true;
            return ResultEnvelope::NOT_FAILED;
        }
        budget.remaining_bytes -= bytes;
        return match serde_json::from_str::<Value>(text) {
            Ok(parsed) => {
                let result = read_result(Some(&parsed), budget, depth + 1);
                ResultEnvelope {
                    data: result.data.map(|data| Cow::Owned(data.into_owned())),
                    failed: result.failed,
                }
            }
            Err(_) => ResultEnvelope::NOT_FAILED,
        };
    }
    if let Value::Array(blocks) = value {
        if blocks.len() > MAX_CONTENT_BLOCKS {
            budget.exceeded = true;
            return ResultEnvelope::NOT_FAILED;
        }
        let mut data = None;
        let mut failed = false;
        for block in blocks {
            let text = if is_object(block) {
                property(block, "text")
            } else {
                None
            };
            let inner = match text {
                Some(text) if is_object(text) => nullish(property(text, "text")).or(Some(text)),
                text => text,
            };
            let result = read_result(inner, budget, depth + 1);
            if data.is_none() {
                data = result.data;
            }
            failed = failed || result.failed;
            if budget.exceeded {
                break;
            }
        }
        return ResultEnvelope { data, failed };
    }
    let Some(record) = value.as_object() else {
        return ResultEnvelope::NOT_FAILED;
    };
    let failed = record.get("isError") == Some(&Value::Bool(true))
        || record.get("is_error") == Some(&Value::Bool(true))
        || record.get("_tag").and_then(Value::as_str) == Some("OrchestratorMcpFailure")
        || nullish(record.get("error")).is_some();
    let content = nullish(record.get("structuredContent")).or_else(|| record.get("content"));
    if content.is_some() {
        let nested = read_result(content, budget, depth + 1);
        return ResultEnvelope {
            data: nested.data,
            failed: failed || nested.failed,
        };
    }
    ResultEnvelope {
        data: Some(Cow::Borrowed(record)),
        failed,
    }
}

/// Upstream copies with `Array.from(value).join("")` so the compact output
/// does not pin the (possibly huge) parent string in V8; an owned `String`
/// already has that property.
fn bounded_id(value: Option<&Value>) -> Option<String> {
    let value = value?.as_str()?;
    // `value.length` counts UTF-16 code units.
    if value.encode_utf16().count() > MAX_ID_LENGTH || js_trim(value).is_empty() {
        return None;
    }
    Some(value.to_owned())
}

fn is_rolled_back(value: Option<&Value>) -> bool {
    value.and_then(Value::as_str) == Some("rolled_back")
}

/// Keeps only IDs and failure metadata used by T3's grouped tool summaries.
pub fn compact_dynamic_tool_output(value: Option<&Value>) -> Option<CompactToolOutput> {
    let mut budget = ResultReadBudget {
        remaining_bytes: MAX_PARSED_BYTES,
        remaining_nodes: MAX_ENVELOPE_NODES,
        exceeded: false,
    };
    let result = read_result(value, &mut budget, 0);
    let mut output = CompactToolOutput {
        is_error: result.failed,
        ..Default::default()
    };
    let data = if budget.exceeded { None } else { result.data };
    if let Some(data) = data.as_deref() {
        output.thread_id = bounded_id(data.get("threadId"));
        output.message_id = bounded_id(data.get("messageId"));
        output.task_id = bounded_id(data.get("taskId"));
        output.scheduled_task_id = bounded_id(data.get("scheduledTaskId"));
        if is_rolled_back(data.get("status")) {
            output.status = Some(CompactToolOutputStatus::RolledBack);
        }
        let nested_thread_id = data
            .get("thread")
            .filter(|thread| is_object(thread))
            .and_then(|thread| bounded_id(property(thread, "threadId")));
        if let Some(thread_id) = nested_thread_id {
            output.thread = Some(CompactThreadRef { thread_id });
        }

        if let Some(Value::Array(entries)) = data.get("threads") {
            let mut complete = entries.len() <= MAX_THREADS;
            let mut threads = Vec::new();
            if complete {
                for entry in entries {
                    if !is_object(entry) {
                        complete = false;
                        break;
                    }
                    let thread_id = bounded_id(property(entry, "threadId"));
                    let rolled_back = is_rolled_back(property(entry, "status"));
                    if thread_id.is_none() && !rolled_back {
                        complete = false;
                        break;
                    }
                    threads.push(CompactThreadEntry {
                        thread_id,
                        status: rolled_back.then_some(CompactToolOutputStatus::RolledBack),
                    });
                }
            }
            if complete {
                output.threads = Some(threads);
            } else {
                // A partial batch or a top-level ID would turn an unknown creation
                // count into a confidently wrong one in the existing summary parser.
                output.thread_id = None;
                output.status = None;
            }
        }
    }
    if output.encoded_len() > MAX_METADATA_BYTES {
        output.threads = None;
        output.thread_id = None;
        output.status = None;
    }
    (!output.is_empty()).then_some(output)
}

static FAILURE_PHRASES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)file not found|no files found|enoent|no such file|commandnotfoundexception|command not found|is not recognized as the name of a cmdlet|a parameter cannot be found that matches parameter name",
    )
    .expect("valid failure phrase regex")
});
static CANNOT_FIND_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)cannot find path").expect("valid regex"));
static DOES_NOT_EXIST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)because it does not exist").expect("valid regex"));
static IS_NOT_RECOGNIZED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)is not recognized").expect("valid regex"));
static THE_TERM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)the term '").expect("valid regex"));
// JS `\d` and `\b` (no `u` flag) are ASCII-only.
static EXITED_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<exited with exit code\s+[1-9][0-9]*\s*>").expect("valid regex")
});
static EXITED_WITH_CODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)exit(?:ed)? with exit code\s+[1-9][0-9]*").expect("valid regex")
});
static EXIT_CODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)exit code\s*[:\s]\s*[1-9][0-9]*(?-u:\b)").expect("valid regex")
});

/// Some providers report completion even when command output describes a failure.
pub fn tool_output_indicates_failure(text: &str) -> bool {
    FAILURE_PHRASES.is_match(text)
        || (CANNOT_FIND_PATH.is_match(text) && DOES_NOT_EXIST.is_match(text))
        || (IS_NOT_RECOGNIZED.is_match(text) && THE_TERM.is_match(text))
        || EXITED_TAG.is_match(text)
        || EXITED_WITH_CODE.is_match(text)
        || EXIT_CODE.is_match(text)
}

/// An image a tool returned inline. `data` is base64; a detail read omits it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolOutputImage<'a> {
    pub mime_type: &'a str,
    pub data: Option<&'a str>,
}

/// Reads one image block in the MCP `{ data, mimeType }` shape or the Anthropic
/// `{ source: { type: "base64", media_type, data } }` shape Claude stores.
/// Only raster types are recognized, so the server never serves an agent's SVG
/// or HTML inline.
///
/// Upstream returns `mimeType.toLowerCase()`; the supported types are ASCII,
/// so the lowercase form is the matching `'static` table entry.
pub fn read_tool_output_image(block: &Value) -> Option<ToolOutputImage<'_>> {
    if !is_object(block) || property(block, "type").and_then(Value::as_str) != Some("image") {
        return None;
    }
    let source = property(block, "source").filter(|source| is_object(source));
    if let Some(source) = source
        && property(source, "type").and_then(Value::as_str) != Some("base64")
    {
        return None;
    }
    let (mime_type, data) = match source {
        None => (property(block, "mimeType"), property(block, "data")),
        Some(source) => (property(source, "media_type"), property(source, "data")),
    };
    let mime_type = mime_type?.as_str()?;
    let lower = mime_type.to_lowercase();
    let mime_type = PROVIDER_SEND_TURN_SUPPORTED_IMAGE_MIME_TYPES
        .into_iter()
        .find(|supported| *supported == lower)?;
    Some(ToolOutputImage {
        mime_type,
        data: data.and_then(Value::as_str),
    })
}

/// Tools return a block, a list of blocks, or an MCP result with a `content` list.
fn output_blocks(value: &Value) -> &[Value] {
    match value {
        Value::Array(blocks) => blocks,
        Value::Object(record) => match record.get("content") {
            Some(Value::Array(blocks)) => blocks,
            _ => std::slice::from_ref(value),
        },
        _ => std::slice::from_ref(value),
    }
}

/// A tool returns one screenshot or a few frames; more would only flood the timeline.
pub const MAX_TOOL_OUTPUT_IMAGES: usize = 8;

/// The largest image a `tool-output-image` asset serves: a provider turn's limit, as base64.
pub const MAX_TOOL_OUTPUT_IMAGE_BASE64_LENGTH: usize =
    PROVIDER_SEND_TURN_MAX_IMAGE_BYTES.div_ceil(3) * 4;

/// The blocks `tool_output_images` reads, by reference. These hold the only image
/// bytes in a tool output that a `tool-output-image` asset can serve, up to
/// `MAX_TOOL_OUTPUT_IMAGE_BASE64_LENGTH`.
pub fn tool_output_image_blocks(value: &Value) -> Vec<&Value> {
    let mut blocks = Vec::new();
    for block in output_blocks(value) {
        if read_tool_output_image(block).is_none() {
            continue;
        }
        blocks.push(block);
        if blocks.len() == MAX_TOOL_OUTPUT_IMAGES {
            break;
        }
    }
    blocks
}

/// The first images in a tool output, in order. The order is the
/// `tool-output-image` asset index, so servers and clients agree on it.
pub fn tool_output_images(value: &Value) -> Vec<ToolOutputImage<'_>> {
    tool_output_image_blocks(value)
        .into_iter()
        .filter_map(read_tool_output_image)
        .collect()
}

/// Replaces each image's bytes with `{ type: "image", mimeType }` and keeps its
/// position, so detail reads stay small and clients load the bytes as assets.
pub fn omit_tool_output_image_data(value: &Value) -> Value {
    let omit = |block: &Value| match read_tool_output_image(block) {
        Some(ToolOutputImage {
            mime_type,
            data: Some(_),
        }) => {
            let mut image = Map::new();
            image.insert("type".to_owned(), Value::from("image"));
            image.insert("mimeType".to_owned(), Value::from(mime_type));
            Value::Object(image)
        }
        _ => block.clone(),
    };
    match value {
        Value::Array(blocks) => Value::Array(blocks.iter().map(omit).collect()),
        Value::Object(record) => match record.get("content") {
            Some(Value::Array(blocks)) => {
                let mut record = record.clone();
                record.insert(
                    "content".to_owned(),
                    Value::Array(blocks.iter().map(omit).collect()),
                );
                Value::Object(record)
            }
            _ => omit(value),
        },
        _ => omit(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn compact(value: &Value) -> Option<Value> {
        compact_dynamic_tool_output(Some(value)).map(|output| output.to_value())
    }

    #[test]
    fn extracts_ids_through_the_mcp_result_envelopes_used_by_t3_summaries() {
        let metadata = json!({ "threadId": "thread-1", "messageId": "message-1" });
        let body = json!({
            "threadId": "thread-1",
            "messageId": "message-1",
            "response": "Private response body",
        });
        let text = body.to_string();
        for value in [
            body.clone(),
            json!({ "structuredContent": body, "content": "Ignored alternative body" }),
            json!([{ "type": "text", "text": text }]),
            json!({ "content": [{ "text": { "text": text } }], "isError": false }),
            Value::String(text.clone()),
        ] {
            assert_eq!(compact(&value), Some(metadata.clone()), "{value}");
        }
    }

    #[test]
    fn keeps_nested_thread_identity_and_scalar_task_ids_without_result_bodies() {
        let output = json!({
            "thread": { "threadId": "thread-1", "messages": ["Private message"] },
            "taskId": "task-1",
            "scheduledTaskId": "scheduled-1",
            "status": "failed",
            "summary": "A child failed; the tool itself succeeded.",
        });
        assert_eq!(
            compact(&output),
            Some(json!({
                "thread": { "threadId": "thread-1" },
                "taskId": "task-1",
                "scheduledTaskId": "scheduled-1",
            }))
        );
        assert_eq!(output["thread"]["messages"], json!(["Private message"]));
    }

    #[test]
    fn keeps_explicit_envelope_failure_while_dropping_error_text_and_arbitrary_output() {
        for failure in [
            json!({ "isError": true }),
            json!({ "is_error": true }),
            json!({ "_tag": "OrchestratorMcpFailure" }),
            json!({ "error": { "message": "Private error" } }),
        ] {
            let mut value = failure.as_object().cloned().unwrap_or_default();
            value.insert(
                "structuredContent".to_owned(),
                json!({ "threadId": "thread-1", "error": "Private details" }),
            );
            assert_eq!(
                compact(&Value::Object(value)),
                Some(json!({ "isError": true, "threadId": "thread-1" })),
                "{failure}"
            );
        }
        assert_eq!(compact(&json!("Private plain text")), None);
        assert_eq!(
            compact(&json!({ "result": { "body": "Private result" } })),
            None
        );
    }

    #[test]
    fn uses_the_first_result_data_but_combines_explicit_failure_flags_across_content_blocks() {
        assert_eq!(
            compact(&json!([
                { "text": json!({ "threadId": "first" }).to_string() },
                {
                    "text": json!({ "threadId": "second", "isError": true, "error": "Private error" })
                        .to_string(),
                },
            ])),
            Some(json!({ "threadId": "first", "isError": true }))
        );
        assert_eq!(
            compact(&json!([
                { "text": "{}" },
                { "text": json!({ "threadId": "second" }).to_string() },
            ])),
            None
        );
    }

    #[test]
    fn keeps_complete_thread_creation_batches_and_rollback_markers_without_row_bodies() {
        assert_eq!(
            compact(&json!({
                "threads": [
                    { "threadId": "created", "status": "running", "messages": ["Private message"] },
                    { "threadId": "reverted", "status": "rolled_back", "error": "Private error" },
                    { "status": "rolled_back" },
                ],
            })),
            Some(json!({
                "threads": [
                    { "threadId": "created" },
                    { "threadId": "reverted", "status": "rolled_back" },
                    { "status": "rolled_back" },
                ],
            }))
        );
        assert_eq!(
            compact(&json!({ "threadId": "reverted", "status": "rolled_back" })),
            Some(json!({ "threadId": "reverted", "status": "rolled_back" }))
        );
        assert_eq!(
            compact(&json!({ "threads": [] })),
            Some(json!({ "threads": [] }))
        );
    }

    #[test]
    fn omits_incomplete_or_oversized_creation_evidence_instead_of_returning_a_partial_count() {
        let too_many: Vec<Value> = (0..101)
            .map(|index| json!({ "threadId": format!("thread-{index}") }))
            .collect();
        let too_large: Vec<Value> = (0..100)
            .map(|index| json!({ "threadId": format!("{index:x<256}") }))
            .collect();
        for threads in [
            json!([{ "threadId": "known" }, { "title": "No confirmed ID" }]),
            Value::Array(too_many),
            Value::Array(too_large),
            json!([{ "threadId": "x".repeat(257) }]),
        ] {
            assert_eq!(
                compact(&json!({
                    "threadId": "must-not-be-counted-as-one",
                    "status": "rolled_back",
                    "taskId": "task-1",
                    "threads": threads,
                })),
                Some(json!({ "taskId": "task-1" }))
            );
        }
        let normal_batch = json!({
            "threads": (0..100)
                .map(|index| json!({ "threadId": format!("00000000-0000-4000-8000-{index:012}") }))
                .collect::<Vec<_>>(),
        });
        assert_eq!(compact(&normal_batch), Some(normal_batch.clone()));
        let encoded = compact(&normal_batch).map(|value| value.to_string());
        assert!(encoded.is_some_and(|json| json.len() <= 8_192));
    }

    #[test]
    fn bounds_parsing_and_envelope_scanning_while_preserving_an_outer_failure_marker() {
        let oversized_json =
            json!({ "threadId": "hidden", "body": "😄".repeat(5_000) }).to_string();
        let many_blocks: Vec<Value> = (0..33)
            .map(|_| json!({ "text": json!({ "threadId": "hidden" }).to_string() }))
            .collect();
        for content in [
            Value::String("x".repeat(1_000_000)),
            Value::String(oversized_json),
            Value::Array(many_blocks),
            json!({ "content": { "content": { "content": { "content": { "threadId": "too-deep" } } } } }),
        ] {
            assert_eq!(
                compact(&json!({ "isError": true, "content": content })),
                Some(json!({ "isError": true }))
            );
        }
        assert_eq!(
            compact(&json!({ "threadId": "known", "body": "x".repeat(1_000_000) })),
            Some(json!({ "threadId": "known" }))
        );
    }

    #[test]
    fn is_idempotent_for_compact_metadata() {
        let first = compact(&json!({
            "isError": true,
            "structuredContent": {
                "threadId": "thread-1",
                "threads": [{ "threadId": "thread-1" }, { "status": "rolled_back" }],
                "output": "Private output",
            },
        }));
        let second = first.as_ref().and_then(compact);
        assert_eq!(second, first);
        assert!(first.is_some());
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

    fn mcp_image() -> Value {
        json!({ "type": "image", "data": "AAAA", "mimeType": "image/png" })
    }

    fn claude_image() -> Value {
        json!({
            "type": "image",
            "source": { "type": "base64", "media_type": "image/JPEG", "data": "BBBB" },
        })
    }

    fn text_block() -> Value {
        json!({ "type": "text", "text": "{}" })
    }

    fn image<'a>(mime_type: &'a str, data: Option<&'a str>) -> ToolOutputImage<'a> {
        ToolOutputImage { mime_type, data }
    }

    #[test]
    fn finds_images_in_each_place_providers_store_them_in_order() {
        let output = json!({ "content": [text_block(), claude_image(), mcp_image()] });
        assert_eq!(
            tool_output_images(&output),
            vec![
                image("image/jpeg", Some("BBBB")),
                image("image/png", Some("AAAA"))
            ]
        );
        let list = json!([mcp_image()]);
        assert_eq!(
            tool_output_images(&list),
            vec![image("image/png", Some("AAAA"))]
        );
        let single = mcp_image();
        assert_eq!(
            tool_output_images(&single),
            vec![image("image/png", Some("AAAA"))]
        );
    }

    #[test]
    fn skips_types_the_server_must_not_serve_inline() {
        let blocks = json!([
            { "type": "image", "data": "AAAA", "mimeType": "image/svg+xml" },
            { "type": "image", "data": "AAAA", "mimeType": "text/html" },
            { "type": "image", "source": { "type": "url", "url": "https://example.com/a.png" } },
        ]);
        assert_eq!(tool_output_images(&blocks), vec![]);
    }

    #[test]
    fn stops_after_the_first_few_images() {
        let blocks = Value::Array((0..10_000).map(|_| mcp_image()).collect());
        assert_eq!(tool_output_images(&blocks).len(), MAX_TOOL_OUTPUT_IMAGES);
    }

    #[test]
    fn drops_the_bytes_but_keeps_each_images_position() {
        let omitted = omit_tool_output_image_data(
            &json!({ "content": [text_block(), claude_image(), mcp_image()] }),
        );
        assert_eq!(
            omitted,
            json!({
                "content": [
                    text_block(),
                    { "type": "image", "mimeType": "image/jpeg" },
                    { "type": "image", "mimeType": "image/png" },
                ],
            })
        );
        assert_eq!(
            tool_output_images(&omitted),
            vec![image("image/jpeg", None), image("image/png", None)]
        );
    }

    #[test]
    fn base64_limit_matches_the_provider_image_limit() {
        assert_eq!(MAX_TOOL_OUTPUT_IMAGE_BASE64_LENGTH, 13_981_016);
    }
}
