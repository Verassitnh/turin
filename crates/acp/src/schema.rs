//! Pinned ACP JSON Schema from the releases `effect-acp` generates against.
//!
//! Upstream `scripts/generate.ts` downloads these files and emits Effect Schema.
//! Turin vendors the same bytes. Wire messages stay JSON. Forward-compatible
//! unions follow the generator's `not` rewrite: a known discriminator that fails
//! its variant is rejected, and any other discriminator is kept.
//!
//! Port of upstream/packages/effect-acp/src/schema.ts.

use serde_json::Value;

use crate::errors::SchemaIssue;

pub const V2_SCHEMA_JSON: &str = include_str!("../schema/schema-v2.0.0-alpha.3.json");
pub const V2_META_JSON: &str = include_str!("../schema/meta-v2.0.0-alpha.3.json");
pub const V1_SCHEMA_JSON: &str = include_str!("../schema/schema-v1.21.0.json");
pub const V1_META_JSON: &str = include_str!("../schema/meta-v1.21.0.json");

const KNOWN_CONTENT: &[&str] = &["text", "image", "audio", "resource_link", "resource"];

pub fn is_content_block(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let Some(kind) = object.get("type").and_then(Value::as_str) else {
        return false;
    };
    match kind {
        "text" => string_field(object, "text"),
        "image" | "audio" => string_field(object, "data") && string_field(object, "mimeType"),
        "resource_link" => string_field(object, "name") && string_field(object, "uri"),
        "resource" => object.get("resource").is_some_and(is_embedded_resource),
        other if !KNOWN_CONTENT.contains(&other) => true,
        _ => false,
    }
}

pub fn is_create_elicitation_request(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if !string_field(object, "message") {
        return false;
    }
    let Some(mode) = object.get("mode").and_then(Value::as_str) else {
        return false;
    };
    let scoped = string_field(object, "sessionId") || request_id_field(object);
    match mode {
        "form" => scoped && object.get("requestedSchema").is_some_and(Value::is_object),
        "url" => scoped && string_field(object, "elicitationId") && string_field(object, "url"),
        other if other != "form" && other != "url" => scoped,
        _ => false,
    }
}

pub fn is_v1_available_command(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if !string_field(object, "name") || !string_field(object, "description") {
        return false;
    }
    match object.get("input") {
        None | Some(Value::Null) => true,
        Some(input) => input
            .as_object()
            .is_some_and(|input| string_field(input, "hint")),
    }
}

pub fn is_v2_available_command(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if !string_field(object, "name") || !string_field(object, "description") {
        return false;
    }
    match object.get("input") {
        None | Some(Value::Null) => true,
        Some(input) => is_v2_command_input(input),
    }
}

pub fn decode_session_update(value: &Value) -> Result<(), SchemaIssue> {
    let Some(object) = value.as_object() else {
        return Err(SchemaIssue::invalid_type(0));
    };
    match object.get("sessionId") {
        Some(Value::String(_)) => {}
        Some(_) => return Err(SchemaIssue::invalid_type(1)),
        None => {
            return Err(SchemaIssue {
                issue_count: 1,
                issue_kinds: vec!["MissingKey".to_owned()],
                maximum_path_depth: 1,
            });
        }
    }
    let Some(update) = object.get("update").and_then(Value::as_object) else {
        return Err(SchemaIssue::invalid_type(1));
    };
    let Some(tag) = update.get("sessionUpdate").and_then(Value::as_str) else {
        return Err(SchemaIssue::invalid_type(2));
    };
    if matches!(
        tag,
        "user_message_chunk"
            | "agent_message_chunk"
            | "agent_thought_chunk"
            | "user_message"
            | "agent_message"
            | "agent_thought"
            | "tool_call"
            | "tool_call_update"
            | "tool_call_content_chunk"
    ) && let Some(content) = update.get("content")
    {
        validate_content_field(content)?;
    }
    Ok(())
}

pub fn decode_elicitation_complete(value: &Value) -> Result<(), SchemaIssue> {
    let Some(object) = value.as_object() else {
        return Err(SchemaIssue::invalid_type(0));
    };
    if string_field(object, "elicitationId") {
        Ok(())
    } else {
        Err(SchemaIssue::invalid_type(1))
    }
}

fn validate_content_field(content: &Value) -> Result<(), SchemaIssue> {
    match content {
        Value::Null => Ok(()),
        Value::Array(items) => {
            for item in items {
                validate_contentish(item)?;
            }
            Ok(())
        }
        other => validate_contentish(other),
    }
}

fn validate_contentish(value: &Value) -> Result<(), SchemaIssue> {
    let Some(object) = value.as_object() else {
        return Err(SchemaIssue::invalid_type(2));
    };
    let Some(kind) = object.get("type").and_then(Value::as_str) else {
        return Err(SchemaIssue::invalid_type(2));
    };
    let known = matches!(
        kind,
        "text" | "image" | "audio" | "resource_link" | "resource" | "content" | "diff" | "terminal"
    );
    if known && !is_content_block(value) && !is_tool_content(value) {
        return Err(SchemaIssue::invalid_type(3));
    }
    Ok(())
}

fn is_tool_content(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("content") => object.get("content").is_some_and(is_content_block),
        Some("diff") => {
            string_field(object, "path") && string_field(object, "newText")
                || object.get("changes").is_some_and(Value::is_array)
        }
        Some("terminal") => string_field(object, "terminalId"),
        Some(other) if !matches!(other, "content" | "diff" | "terminal") => true,
        _ => false,
    }
}

fn is_v2_command_input(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    match object.get("type").and_then(Value::as_str) {
        Some("text") => string_field(object, "hint"),
        Some(_) => true,
        None => false,
    }
}

fn is_embedded_resource(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    string_field(object, "uri")
        && (object.get("text").is_some_and(Value::is_string)
            || object.get("blob").is_some_and(Value::is_string))
}

fn string_field(object: &serde_json::Map<String, Value>, key: &str) -> bool {
    object.get(key).is_some_and(Value::is_string)
}

fn request_id_field(object: &serde_json::Map<String, Value>) -> bool {
    matches!(
        object.get("requestId"),
        Some(Value::Null | Value::Number(_) | Value::String(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pinned_releases_match_upstream_generator() {
        let v2_meta: Value = serde_json::from_str(V2_META_JSON).unwrap();
        let v1_meta: Value = serde_json::from_str(V1_META_JSON).unwrap();
        assert_eq!(v2_meta["version"], 2);
        assert_eq!(v1_meta["version"], 1);
        assert_eq!(v2_meta["agentMethods"]["initialize"], "initialize");
        assert_eq!(
            v2_meta["clientMethods"]["session_request_permission"],
            "session/request_permission"
        );
        let v2_schema: Value = serde_json::from_str(V2_SCHEMA_JSON).unwrap();
        let v1_schema: Value = serde_json::from_str(V1_SCHEMA_JSON).unwrap();
        assert!(v2_schema["$defs"]["ContentBlock"].is_object());
        assert!(v1_schema["$defs"]["AvailableCommand"].is_object());
    }

    #[test]
    fn content_blocks_reject_broken_known_variants_and_keep_future_ones() {
        assert!(is_content_block(&json!({"type": "text", "text": "hello"})));
        assert!(!is_content_block(&json!({"type": "text"})));
        assert!(!is_content_block(
            &json!({"type": "image", "data": "aGVsbG8="})
        ));
        assert!(is_content_block(
            &json!({"type": "future_content", "payload": {"value": 1}})
        ));
    }

    #[test]
    fn elicitation_requires_scope_and_mode_fields() {
        let form = json!({
            "sessionId": "session-1",
            "mode": "form",
            "message": "Choose a branch",
            "requestedSchema": {"type": "object", "properties": {}}
        });
        assert!(is_create_elicitation_request(&form));
        let mut missing_schema = form.clone();
        missing_schema
            .as_object_mut()
            .unwrap()
            .remove("requestedSchema");
        assert!(!is_create_elicitation_request(&missing_schema));
        let mut missing_session = form.clone();
        missing_session.as_object_mut().unwrap().remove("sessionId");
        assert!(!is_create_elicitation_request(&missing_session));
        assert!(!is_create_elicitation_request(&json!({
            "sessionId": "session-1",
            "mode": "url",
            "message": "Sign in"
        })));
    }

    #[test]
    fn v1_command_input_does_not_require_a_type_discriminator() {
        let command =
            json!({"name": "plan", "description": "Make a plan", "input": {"hint": "Task"}});
        assert!(is_v1_available_command(&command));
        assert!(!is_v2_available_command(&command));
        assert!(is_v2_available_command(&json!({
            "name": "plan",
            "description": "Make a plan",
            "input": {"type": "text", "hint": "Task"}
        })));
    }
}
