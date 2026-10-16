use serde_json::Value;

use super::util::bounded_content_metadata;

const MAX_ACP_CONTENT_TEXT_LENGTH: usize = 65_536;

fn bounded_content_text(value: &str) -> String {
    if value.len() <= MAX_ACP_CONTENT_TEXT_LENGTH {
        value.to_owned()
    } else {
        value[..MAX_ACP_CONTENT_TEXT_LENGTH].to_owned()
    }
}

fn bounded_content_uri(value: Option<&str>) -> String {
    let uri = bounded_content_metadata(value, 4096);
    if uri.to_ascii_lowercase().starts_with("data:") {
        String::new()
    } else {
        uri
    }
}

/// Convert every ACP output block into bounded, displayable text without retaining base64 data.
pub fn acp_content_block_display_text(content: &Value) -> Option<String> {
    let content_type = content.get("type")?.as_str()?;
    match content_type {
        "text" => {
            let text = content.get("text")?.as_str()?;
            Some(bounded_content_text(text))
        }
        "resource_link" => {
            let label = {
                let from_title =
                    bounded_content_metadata(content.get("title").and_then(Value::as_str), 512);
                if from_title.is_empty() {
                    bounded_content_metadata(content.get("name").and_then(Value::as_str), 512)
                } else {
                    from_title
                }
            };
            let label = if label.is_empty() {
                "resource".to_owned()
            } else {
                label
            };
            let description =
                bounded_content_metadata(content.get("description").and_then(Value::as_str), 2048);
            let uri = bounded_content_uri(content.get("uri").and_then(Value::as_str));
            let mut out = if description.is_empty() {
                label
            } else {
                format!("{label}: {description}")
            };
            if !uri.is_empty() {
                out.push('\n');
                out.push_str(&uri);
            }
            Some(out)
        }
        "resource" => {
            let resource = content.get("resource")?;
            if resource.get("text").and_then(Value::as_str).is_some() {
                return Some(bounded_content_text(
                    resource.get("text").and_then(Value::as_str).unwrap_or(""),
                ));
            }
            let mime_type =
                bounded_content_metadata(resource.get("mimeType").and_then(Value::as_str), 256);
            let uri = bounded_content_uri(resource.get("uri").and_then(Value::as_str));
            let mime_part = if mime_type.is_empty() {
                String::new()
            } else {
                format!(" ({mime_type})")
            };
            let uri_part = if uri.is_empty() {
                String::new()
            } else {
                format!(": {uri}")
            };
            Some(format!("[ACP binary resource{mime_part}{uri_part}]"))
        }
        "image" => {
            let mime_type =
                bounded_content_metadata(content.get("mimeType").and_then(Value::as_str), 256);
            let mime = if mime_type.is_empty() {
                "unknown type".to_owned()
            } else {
                mime_type
            };
            let uri = bounded_content_uri(content.get("uri").and_then(Value::as_str));
            let uri_part = if uri.is_empty() {
                String::new()
            } else {
                format!(": {uri}")
            };
            Some(format!("[ACP image ({mime}){uri_part}]"))
        }
        "audio" => {
            let mime_type =
                bounded_content_metadata(content.get("mimeType").and_then(Value::as_str), 256);
            let mime = if mime_type.is_empty() {
                "unknown type".to_owned()
            } else {
                mime_type
            };
            Some(format!("[ACP audio ({mime})]"))
        }
        "_t3_unknown" => {
            let original =
                bounded_content_metadata(content.get("originalType").and_then(Value::as_str), 128);
            let label = if original.is_empty() {
                "unknown"
            } else {
                original.as_str()
            };
            Some(format!("[Unsupported ACP content: {label}]"))
        }
        _ => None,
    }
}

pub(crate) fn tool_call_content_text(entry: &Value) -> Option<&str> {
    if entry.get("type").and_then(Value::as_str) != Some("content") {
        return None;
    }
    let inner = entry.get("content")?;
    if inner.get("type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    inner.get("text").and_then(Value::as_str)
}

pub(crate) fn sanitize_acp_tool_call_content(content: &[Value]) -> Vec<Value> {
    content
        .iter()
        .map(|entry| {
            let Some(entry_type) = entry.get("type").and_then(Value::as_str) else {
                return entry.clone();
            };
            match entry_type {
                "content" => {
                    let inner = entry.get("content").cloned().unwrap_or(Value::Null);
                    Value::Object(
                        [
                            ("type".to_owned(), Value::String("content".to_owned())),
                            (
                                "content".to_owned(),
                                Value::Object(
                                    [
                                        ("type".to_owned(), Value::String("text".to_owned())),
                                        (
                                            "text".to_owned(),
                                            Value::String(
                                                acp_content_block_display_text(&inner)
                                                    .unwrap_or_default(),
                                            ),
                                        ),
                                    ]
                                    .into_iter()
                                    .collect(),
                                ),
                            ),
                        ]
                        .into_iter()
                        .collect(),
                    )
                }
                "diff" => {
                    if entry.get("changes").is_some() {
                        let mut out = entry.clone();
                        if let Some(obj) = out.as_object_mut() {
                            obj.remove("patch");
                            if let Some(patch) = entry.get("patch")
                                && !patch.is_null()
                            {
                                obj.insert("patch".to_owned(), patch.clone());
                            }
                        }
                        out
                    } else {
                        entry.clone()
                    }
                }
                "terminal" => entry.clone(),
                "_t3_unknown" => {
                    let original = bounded_content_metadata(
                        entry.get("originalType").and_then(Value::as_str),
                        128,
                    );
                    let label = if original.is_empty() {
                        "unknown".to_owned()
                    } else {
                        original
                    };
                    serde_json::json!({
                        "type": "_t3_unknown",
                        "originalType": label,
                        "raw": null
                    })
                }
                _ => entry.clone(),
            }
        })
        .collect()
}

pub(crate) fn content_has_text_blocks(content: &[Value]) -> bool {
    content.iter().any(|block| {
        acp_content_block_display_text(block)
            .map(|text| !text.is_empty())
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bounds_text_without_changing_streamed_whitespace() {
        let text = format!("  {}  ", "x".repeat(70_000));
        let projected_text =
            acp_content_block_display_text(&json!({ "type": "text", "text": text }));
        let projected_resource = acp_content_block_display_text(&json!({
            "type": "resource",
            "resource": { "uri": "file:///workspace/large.txt", "text": text }
        }));
        assert_eq!(projected_text.as_ref().map(String::len), Some(65_536));
        assert!(projected_text.as_ref().unwrap().starts_with("  "));
        assert_eq!(projected_resource, projected_text);
    }
}
