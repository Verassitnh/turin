//! The image helpers of upstream/packages/shared/src/toolOutput.ts that the
//! projection needs (`readToolOutputImage`, `toolOutputImageBlocks`). The rest
//! of that file belongs to `turin-shared` (T05b).

use serde_json::Value;
use turin_contracts::chat_attachment::{
    PROVIDER_SEND_TURN_MAX_IMAGE_BYTES, is_provider_send_turn_supported_image_mime_type,
};

/// A tool returns one screenshot or a few frames; more would only flood the timeline.
pub const MAX_TOOL_OUTPUT_IMAGES: usize = 8;

/// The largest image a `tool-output-image` asset serves: a provider turn's limit, as base64.
pub const MAX_TOOL_OUTPUT_IMAGE_BASE64_LENGTH: usize =
    (PROVIDER_SEND_TURN_MAX_IMAGE_BYTES as usize).div_ceil(3) * 4;

/// An image a tool returned inline. `data` is base64; a detail read omits it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutputImage<'a> {
    pub mime_type: String,
    pub data: Option<&'a str>,
}

/// Reads one image block in the MCP `{ data, mimeType }` shape or the Anthropic
/// `{ source: { type: "base64", media_type, data } }` shape Claude stores.
/// Only raster types are recognized, so the server never serves an agent's SVG
/// or HTML inline.
pub fn read_tool_output_image(block: &Value) -> Option<ToolOutputImage<'_>> {
    let block = block.as_object()?;
    if block.get("type")? != "image" {
        return None;
    }
    let source = block.get("source").and_then(Value::as_object);
    if let Some(source) = source
        && source.get("type") != Some(&Value::String("base64".into()))
    {
        return None;
    }
    let (mime_type, data) = match source {
        Some(source) => (source.get("media_type"), source.get("data")),
        None => (block.get("mimeType"), block.get("data")),
    };
    let mime_type = mime_type?.as_str()?;
    if !is_provider_send_turn_supported_image_mime_type(mime_type) {
        return None;
    }
    Some(ToolOutputImage {
        mime_type: mime_type.to_lowercase(),
        data: data.and_then(Value::as_str),
    })
}

/// Tools return a block, a list of blocks, or an MCP result with a `content` list.
fn output_blocks(value: &Value) -> Vec<&Value> {
    match value {
        Value::Array(blocks) => blocks.iter().collect(),
        Value::Object(map) => match map.get("content") {
            Some(Value::Array(blocks)) => blocks.iter().collect(),
            _ => vec![value],
        },
        _ => vec![value],
    }
}

/// The blocks `tool_output_images` reads, by reference. These hold the only
/// image bytes in a tool output that a `tool-output-image` asset can serve, up
/// to [`MAX_TOOL_OUTPUT_IMAGE_BASE64_LENGTH`].
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
