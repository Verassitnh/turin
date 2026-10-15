//! Port of upstream/packages/provider-core/src/server/nativeProtocolLogging.ts.

use serde_json::{Value, json};

/// `isStructuralTag` from upstream `shared/observability`: a bounded
/// identifier-like string that is safe to copy into diagnostics.
fn is_structural_tag(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphabetic()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/-".contains(byte))
}

/// Accept only bounded method names as structural diagnostic fields.
pub fn structural_protocol_method(value: &str) -> &str {
    if is_structural_tag(value) {
        value
    } else {
        "unknown"
    }
}

/// `errorTag` for a tagged record: the tag when structural, else `TaggedError`.
fn tagged_error_tag(tag: &str) -> &str {
    if is_structural_tag(tag) {
        tag
    } else {
        "TaggedError"
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
                summary["messageTag"] = Value::String(tagged_error_tag(tag).to_owned());
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
        assert_eq!(
            summarize_native_protocol_payload(&json!({"_tag":"secret token value"})),
            json!({"valueType":"object","fieldCount":1,"messageTag":"TaggedError"})
        );
        assert_eq!(
            summarize_native_protocol_payload(&json!({"_tag":"AcpRequestError"})),
            json!({"valueType":"object","fieldCount":1,"messageTag":"AcpRequestError"})
        );
        assert_eq!(
            summarize_native_protocol_payload(&json!("héllo")),
            json!({"valueType":"string","byteLength":6})
        );
        assert_eq!(
            summarize_native_protocol_payload(&json!([1, 2, 3])),
            json!({"valueType":"array","itemCount":3})
        );
    }
}
