//! Port of upstream/packages/provider-core/src/server/nativeProtocolLogging.ts.

use serde_json::{Value, json};

/// Accept only bounded method names as structural diagnostic fields.
pub fn structural_protocol_method(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 128
        || !bytes[0].is_ascii_alphabetic()
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/-".contains(byte))
    {
        "unknown"
    } else {
        value
    }
}

/// Summarize shape without copying prompt or tool-output values to diagnostics.
pub fn summarize_native_protocol_payload(payload: &Value) -> Value {
    match payload {
        Value::Null => json!({"valueType": "null"}),
        Value::String(value) => json!({"valueType": "string", "byteLength": value.len()}),
        Value::Array(items) => json!({"valueType": "array", "itemCount": items.len()}),
        Value::Object(fields) => {
            let mut summary = json!({"valueType": "object", "fieldCount": fields.len()});
            if let Some(tag) = fields.get("_tag").and_then(Value::as_str) {
                summary["messageTag"] = Value::String(tag.to_owned());
            }
            if let Some(tag) = fields.get("tag").and_then(Value::as_str) {
                summary["method"] = Value::String(structural_protocol_method(tag).to_owned());
            }
            summary
        }
        Value::Bool(_) => json!({"valueType": "boolean"}),
        Value::Number(_) => json!({"valueType": "number"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_methods_and_payload_content() {
        assert_eq!(structural_protocol_method("thread/start"), "thread/start");
        assert_eq!(structural_protocol_method("bad method"), "unknown");
        assert_eq!(structural_protocol_method(&"a".repeat(129)), "unknown");
        assert_eq!(
            summarize_native_protocol_payload(&json!({"tag":"thread/start","secret":"do not log"})),
            json!({"valueType":"object","fieldCount":2,"method":"thread/start"})
        );
    }
}
