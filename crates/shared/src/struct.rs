//! Port of upstream/packages/shared/src/Struct.ts

use serde_json::{Map, Value};

/// Deep-merges `patch` into `current` like upstream `deepMerge`.
///
/// Upstream merges plain objects recursively (`effect/Predicate.isObject`:
/// `typeof === "object" && !== null && !Array.isArray`) and replaces every
/// other value wholesale; `undefined` values in the patch are skipped (JSON
/// has no `undefined`, so nothing is skipped). Arrays replace,
/// they do not concatenate. A non-object `patch` replaces `current` outright.
pub fn deep_merge(current: &Value, patch: &Value) -> Value {
    if !is_object(current) || !is_object(patch) {
        return patch.clone();
    }
    let mut next = current.as_object().expect("checked is_object").clone();
    for (key, value) in patch.as_object().expect("checked is_object") {
        let existing = next.get(key).cloned();
        next.insert(
            key.clone(),
            match existing {
                Some(existing) if is_object(&existing) && is_object(value) => {
                    deep_merge(&existing, value)
                }
                _ => value.clone(),
            },
        );
    }
    Value::Object(next)
}

/// `effect/Predicate.isObject`: a non-null, non-array object.
fn is_object(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}

/// Object-map variant used by callers that hold `Map<String, Value>`s.
pub fn deep_merge_map(
    current: &Map<String, Value>,
    patch: &Map<String, Value>,
) -> Map<String, Value> {
    let merged = deep_merge(
        &Value::Object(current.clone()),
        &Value::Object(patch.clone()),
    );
    match merged {
        Value::Object(map) => map,
        _ => unreachable!("object merge yields an object"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merges_nested_objects() {
        let current = json!({"a": 1, "nested": {"x": 1, "y": 2}, "keep": true});
        let patch = json!({"nested": {"y": 3, "z": 4}});
        assert_eq!(
            deep_merge(&current, &patch),
            json!({"a": 1, "nested": {"x": 1, "y": 3, "z": 4}, "keep": true})
        );
    }

    #[test]
    fn replaces_non_object_values_wholesale() {
        let current = json!({"a": [1, 2], "b": "x"});
        let patch = json!({"a": [3], "b": {"c": 1}});
        assert_eq!(
            deep_merge(&current, &patch),
            json!({"a": [3], "b": {"c": 1}})
        );
    }

    #[test]
    fn non_object_patch_replaces_current() {
        assert_eq!(deep_merge(&json!({"a": 1}), &json!(5)), json!(5));
        assert_eq!(deep_merge(&json!({"a": 1}), &json!("s")), json!("s"));
    }
}
