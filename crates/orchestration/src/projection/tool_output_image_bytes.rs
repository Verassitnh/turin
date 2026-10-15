//! Port of upstream/apps/server/src/orchestration-v2/toolOutputImageBytes.ts

use std::borrow::Cow;

use serde_json::{Map, Value, json};
use turin_contracts::orchestration_v2::OrchestrationV2TurnItem;

use crate::projection::shared::tool_output::{
    MAX_TOOL_OUTPUT_IMAGE_BASE64_LENGTH, read_tool_output_image, tool_output_image_blocks,
};

const IMAGE_MIME_KEYS: [&str; 4] = ["mimeType", "mime_type", "media_type", "type"];
const IMAGE_BODY_KEYS: [&str; 3] = ["data", "blob", "base64"];
const MAX_DEPTH: usize = 32;
/// Shorter strings are identifiers or tiny fixtures, not pixels worth removing.
const MIN_BODY_LENGTH: usize = 64;

fn has_image_mime(record: &Map<String, Value>) -> bool {
    IMAGE_MIME_KEYS.iter().any(|key| {
        record
            .get(*key)
            .and_then(Value::as_str)
            .is_some_and(|mime| mime.to_lowercase().starts_with("image/"))
    })
}

/// The record that carries an image's bytes: an MCP or ACP `type: "image"`
/// block, Cursor's `image` part (its mime type is optional), or a record with
/// an `image/*` mime field such as Claude's `source`, the `file` of Claude's
/// structured Read result, and Grok's `ImageContent`.
fn is_image_body(record: &Map<String, Value>, key: Option<&str>) -> bool {
    if record.get("type") == Some(&json!("image")) || key == Some("image") {
        return true;
    }
    if !has_image_mime(record) {
        return false;
    }
    match record.get("type") {
        None => true,
        Some(Value::String(kind)) => kind == "base64" || kind.to_lowercase().starts_with("image/"),
        Some(_) => false,
    }
}

/// `^[A-Za-z0-9+/_-]+={0,2}$` at least [`MIN_BODY_LENGTH`] long. Length is in
/// UTF-16 units upstream, which equals bytes for this ASCII-only alphabet.
fn is_base64_body(value: &Value) -> Option<&str> {
    let text = value.as_str()?;
    if text.len() < MIN_BODY_LENGTH {
        return None;
    }
    let body = text.trim_end_matches('=');
    let padding = text.len() - body.len();
    (padding <= 2
        && !body.is_empty()
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'_' | b'-')))
    .then_some(text)
}

fn base64_decoded_length(encoded: &str) -> u64 {
    let length = encoded.trim_end_matches('=').len() as u64;
    length * 3 / 4
}

struct StripContext<'a> {
    output: &'a Value,
    served: Option<Vec<&'a Value>>,
}

/// A block the asset route serves: in a served position and within its size limit.
fn is_served(context: &mut StripContext<'_>, record: &Value) -> bool {
    let output = context.output;
    let served = context
        .served
        .get_or_insert_with(|| tool_output_image_blocks(output));
    if !served.iter().any(|block| std::ptr::eq(*block, record)) {
        return false;
    }
    read_tool_output_image(record)
        .and_then(|image| image.data)
        .is_none_or(|data| data.len() <= MAX_TOOL_OUTPUT_IMAGE_BASE64_LENGTH)
}

/// The stripped copy of `value`, or `None` when nothing under it changed.
/// Containers are copied only when a descendant changes.
fn strip(
    value: &Value,
    key: Option<&str>,
    context: &mut StripContext<'_>,
    depth: usize,
) -> Option<Value> {
    if depth > MAX_DEPTH {
        return None;
    }
    match value {
        Value::Array(entries) => {
            let mut next: Option<Vec<Value>> = None;
            for (index, entry) in entries.iter().enumerate() {
                if let Some(stripped) = strip(entry, None, context, depth + 1) {
                    next.get_or_insert_with(|| entries.clone())[index] = stripped;
                }
            }
            next.map(Value::Array)
        }
        Value::Object(record) => {
            // Every served block is a `type: "image"` block; keep it whole.
            if record.get("type") == Some(&json!("image")) && is_served(context, value) {
                return None;
            }
            let image_body = is_image_body(record, key);
            let mut next: Option<Map<String, Value>> = None;
            let mut removed_body: Option<&str> = None;
            for (entry_key, entry) in record {
                if image_body
                    && IMAGE_BODY_KEYS.contains(&entry_key.as_str())
                    && let Some(body) = is_base64_body(entry)
                {
                    next.get_or_insert_with(|| record.clone())
                        .shift_remove(entry_key);
                    removed_body = Some(body);
                    continue;
                }
                if let Some(stripped) = strip(entry, Some(entry_key), context, depth + 1) {
                    next.get_or_insert_with(|| record.clone())
                        .insert(entry_key.clone(), stripped);
                }
            }
            if let (Some(next), Some(body)) = (next.as_mut(), removed_body) {
                // Measured from the removed body, never taken from a provider's size field.
                next.insert("sizeBytes".into(), json!(base64_decoded_length(body)));
            }
            next.map(Value::Object)
        }
        _ => None,
    }
}

/// Replaces the base64 image bodies in a dynamic tool's output with their
/// decoded byte size, except the blocks a `tool-output-image` asset serves.
///
/// Tools that read or capture an image return the whole file as base64, and a
/// turn item is stored twice: in the event log and in the projection. Only the
/// blocks `tool_output_image_blocks` finds, within the asset size limit, are
/// ever read back; previews of a read file load from `viewedImagePath`, and
/// context handoff skips tool items.
/// Strings that are not a base64 body are kept, so identifiers and inline SVG
/// under the same keys survive. An item without such bytes is returned
/// borrowed.
pub fn strip_unserved_tool_output_image_bytes(
    item: &OrchestrationV2TurnItem,
) -> Cow<'_, OrchestrationV2TurnItem> {
    let OrchestrationV2TurnItem::DynamicTool {
        output: Some(output),
        ..
    } = item
    else {
        return Cow::Borrowed(item);
    };
    if !output.is_object() && !output.is_array() {
        return Cow::Borrowed(item);
    }
    let mut context = StripContext {
        output,
        served: None,
    };
    match strip(output, None, &mut context, 0) {
        None => Cow::Borrowed(item),
        Some(stripped) => {
            let mut next = item.clone();
            if let OrchestrationV2TurnItem::DynamicTool { output, .. } = &mut next {
                *output = Some(stripped);
            }
            Cow::Owned(next)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::shared::tool_output::{MAX_TOOL_OUTPUT_IMAGES, tool_output_images};
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;

    const PNG_BYTES: usize = 3_000;

    fn png_base64() -> String {
        STANDARD.encode(vec![7u8; PNG_BYTES])
    }
    /// 3,001 bytes encode with `=` padding, which must not count as data.
    fn padded_base64() -> String {
        STANDARD.encode(vec![9u8; PNG_BYTES + 1])
    }

    fn tool_item(output: Value) -> OrchestrationV2TurnItem {
        serde_json::from_value(json!({
            "id": "turn-item:tool", "threadId": "thread:tool", "runId": null, "nodeId": "node:tool",
            "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
            "parentItemId": null, "ordinal": 1, "status": "completed", "title": null,
            "startedAt": "2026-10-06T00:00:00.000Z", "completedAt": "2026-10-06T00:00:00.000Z",
            "updatedAt": "2026-10-06T00:00:00.000Z", "type": "dynamic_tool", "toolName": "Read",
            "input": {"file_path": "/tmp/shot.png"}, "output": output
        }))
        .unwrap()
    }

    fn output_of(item: &OrchestrationV2TurnItem) -> Value {
        match item {
            OrchestrationV2TurnItem::DynamicTool { output, .. } => {
                output.clone().unwrap_or(Value::Null)
            }
            _ => Value::Null,
        }
    }

    fn stripped(output: Value) -> Value {
        output_of(&strip_unserved_tool_output_image_bytes(&tool_item(output)))
    }

    #[test]
    fn replaces_the_base64_of_claudes_structured_read_result_with_its_measured_size() {
        let got = stripped(json!({"type": "image", "file": {
            "base64": png_base64(), "type": "image/png", "originalSize": PNG_BYTES,
            "dimensions": {"originalWidth": 180, "originalHeight": 180}
        }}));
        assert_eq!(
            got,
            json!({"type": "image", "file": {
                "type": "image/png", "originalSize": PNG_BYTES,
                "dimensions": {"originalWidth": 180, "originalHeight": 180}, "sizeBytes": PNG_BYTES
            }})
        );
    }

    #[test]
    fn strips_acp_image_shapes() {
        let got = stripped(json!({
            "rawOutput": {"type": "ReadFile", "ImageContent": {"data": png_base64(), "mime_type": "image/png"}},
            "failed": {"error": "partial result",
                "result": [{"type": "image", "data": padded_base64(), "mimeType": "image/png"}]}
        }));
        assert_eq!(
            got,
            json!({
                "rawOutput": {"type": "ReadFile", "ImageContent": {"mime_type": "image/png", "sizeBytes": PNG_BYTES}},
                "failed": {"error": "partial result",
                    "result": [{"type": "image", "mimeType": "image/png", "sizeBytes": PNG_BYTES + 1}]}
            })
        );
    }

    #[test]
    fn strips_a_cursor_image_part_that_names_no_mime_type() {
        let got = stripped(
            json!({"content": [{"text": {"text": "captured"}}, {"image": {"data": png_base64()}}]}),
        );
        assert_eq!(
            got,
            json!({"content": [{"text": {"text": "captured"}}, {"image": {"sizeBytes": PNG_BYTES}}]})
        );
    }

    #[test]
    fn keeps_the_screenshots_a_tool_output_image_asset_serves() {
        let claude = json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": png_base64()}});
        let mcp = json!({"type": "image", "data": padded_base64(), "mimeType": "image/jpeg"});
        let item =
            tool_item(json!({"content": [{"type": "text", "text": "captured"}, claude, mcp]}));
        let result = strip_unserved_tool_output_image_bytes(&item);
        assert!(matches!(result, Cow::Borrowed(_)));
        let output = output_of(&result);
        let images = tool_output_images(&output);
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].mime_type, "image/png");
        assert_eq!(images[0].data, Some(png_base64().as_str()));
        assert_eq!(images[1].mime_type, "image/jpeg");
        assert_eq!(images[1].data, Some(padded_base64().as_str()));
    }

    #[test]
    fn strips_only_the_images_past_the_last_one_an_asset_can_serve() {
        let image = json!({"type": "image", "data": png_base64(), "mimeType": "image/png"});
        let images: Vec<_> = (0..MAX_TOOL_OUTPUT_IMAGES + 2)
            .map(|_| image.clone())
            .collect();
        let got = stripped(Value::Array(images.clone()));
        let got = got.as_array().unwrap();
        assert_eq!(
            &got[..MAX_TOOL_OUTPUT_IMAGES],
            &images[..MAX_TOOL_OUTPUT_IMAGES]
        );
        for tail in &got[MAX_TOOL_OUTPUT_IMAGES..] {
            assert_eq!(
                tail,
                &json!({"type": "image", "mimeType": "image/png", "sizeBytes": PNG_BYTES})
            );
        }
        assert_eq!(got.len(), MAX_TOOL_OUTPUT_IMAGES + 2);
    }

    #[test]
    fn strips_a_served_position_image_larger_than_an_asset_may_serve() {
        let oversized = "A".repeat(MAX_TOOL_OUTPUT_IMAGE_BASE64_LENGTH + 4);
        let servable = json!({"type": "image", "data": png_base64(), "mimeType": "image/png"});
        let got = stripped(json!({"content": [
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": oversized}},
            servable.clone()
        ]}));
        assert_eq!(
            got["content"],
            json!([
                {"type": "image", "source": {"type": "base64", "media_type": "image/png",
                    "sizeBytes": (MAX_TOOL_OUTPUT_IMAGE_BASE64_LENGTH + 4) / 4 * 3}},
                servable
            ])
        );
    }

    #[test]
    fn keeps_image_fields_whose_value_is_not_a_base64_body() {
        let svg = format!(
            "<svg xmlns='http://www.w3.org/2000/svg'>{}</svg>",
            "<circle r='4'/>".repeat(8)
        );
        let item = tool_item(json!({"nested": {
            "byId": {"type": "image", "data": "asset-id-42"},
            "svg": {"mimeType": "image/svg+xml", "data": svg},
            "dataUrl": {"type": "image", "data": format!("data:image/png;base64,{}", png_base64())},
            "remote": {"type": "image", "source": {"type": "url", "url": "https://example.test/shot.png"}},
            "tiny": {"type": "image", "data": "AA==", "mimeType": "image/png"},
            "notImage": {"mimeType": "application/pdf", "data": png_base64()}
        }}));
        assert!(matches!(
            strip_unserved_tool_output_image_bytes(&item),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn measures_the_size_instead_of_trusting_one_the_provider_supplied() {
        let got = stripped(
            json!({"type": "image", "file": {"base64": png_base64(), "type": "image/png", "sizeBytes": 0}}),
        );
        assert_eq!(
            got,
            json!({"type": "image", "file": {"type": "image/png", "sizeBytes": PNG_BYTES}})
        );
    }

    #[test]
    fn returns_items_without_image_bytes_borrowed() {
        let item = tool_item(
            json!({"stdout": "data: 42", "data": png_base64(), "nested": {"files": [{"path": "src/app.ts"}]}}),
        );
        assert!(matches!(
            strip_unserved_tool_output_image_bytes(&item),
            Cow::Borrowed(_)
        ));
        let command: OrchestrationV2TurnItem = serde_json::from_value(json!({
            "id": "turn-item:tool", "threadId": "thread:tool", "runId": null, "nodeId": "node:tool",
            "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
            "parentItemId": null, "ordinal": 1, "status": "completed", "title": null,
            "startedAt": null, "completedAt": null, "updatedAt": "2026-10-06T00:00:00.000Z",
            "type": "command_execution", "input": "cat shot.png | base64", "output": png_base64()
        }))
        .unwrap();
        assert!(matches!(
            strip_unserved_tool_output_image_bytes(&command),
            Cow::Borrowed(_)
        ));
        let plain = tool_item(json!("plain text output"));
        assert!(matches!(
            strip_unserved_tool_output_image_bytes(&plain),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn does_not_mutate_the_providers_item_and_keeps_untouched_siblings() {
        let output = json!({"error": "partial result",
            "result": [{"type": "text", "text": "captured"}, {"type": "image", "data": png_base64(), "mimeType": "image/png"}]});
        let item = tool_item(output.clone());
        let got = output_of(&strip_unserved_tool_output_image_bytes(&item));
        assert_eq!(output_of(&item), output);
        assert_eq!(got["result"][0], output["result"][0]);
        assert_eq!(got["result"][1]["sizeBytes"], json!(PNG_BYTES));
    }
}
