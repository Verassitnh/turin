//! Port of upstream/apps/server/src/orchestration-v2/unknownField.ts
//!
//! Readers for decoding undeclared provider payloads without trusting shapes.

use serde_json::Value;
use turin_contracts::base_schemas::js_trim;

/// `field(source, key)`: the value under `key` when `source` is an object,
/// otherwise `undefined` (here `None`). JSON `null` is a present value.
pub fn field<'a>(source: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    source?.get(key)
}

/// `text(value)`: the trimmed string, or `None` when the value is not a string
/// or is blank after JS `trim()`.
pub fn text(value: Option<&Value>) -> Option<&str> {
    let trimmed = js_trim(value?.as_str()?);
    (!trimmed.is_empty()).then_some(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn field_reads_object_keys_and_is_undefined_for_anything_else() {
        let source = json!({ "type": "assistant", "nothing": null });
        assert_eq!(field(Some(&source), "type"), Some(&json!("assistant")));
        assert_eq!(field(Some(&source), "nothing"), Some(&Value::Null));
        assert_eq!(field(Some(&source), "missing"), None);
        let scalar = json!("text");
        assert_eq!(field(Some(&scalar), "length"), None);
        assert_eq!(field(None, "type"), None);
    }

    #[test]
    fn text_trims_strings_and_rejects_blank_or_non_strings() {
        assert_eq!(text(Some(&json!("  assistant \n"))), Some("assistant"));
        assert_eq!(text(Some(&json!("   "))), None);
        assert_eq!(text(Some(&json!(7))), None);
        assert_eq!(text(Some(&Value::Null)), None);
        assert_eq!(text(None), None);
    }
}
