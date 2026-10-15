//! Port of upstream/apps/server/src/sourceControl/sourceControlHostSecrets.ts
//!
//! Which source control host settings fields hold secrets, read from each built-in host's client
//! definition. The app's settings store keeps those values in the secret store and redacts them
//! for clients, the same way for every host.
//!
//! A secret field holds a string, or a string per server host such as GitHub's `tokens`.

use std::collections::HashMap;
use std::sync::LazyLock;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Map, Value};

use crate::github::definition as github;

/// Secret field names per host kind; a host with none is absent.
pub static SOURCE_CONTROL_HOST_SECRET_FIELDS: LazyLock<
    HashMap<&'static str, &'static [&'static str]>,
> = LazyLock::new(|| {
    let mut fields = HashMap::new();
    if !github::SECRET_SETTINGS_FIELDS.is_empty() {
        fields.insert(github::KIND, github::SECRET_SETTINGS_FIELDS);
    }
    fields
});

/// One stored secret: a host's field, and the server host it is for when the field is per host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceControlHostSecretSlot {
    pub kind: String,
    pub field: String,
    pub server_host: Option<String>,
}

fn encode(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(value.as_bytes())
}

/// The secret store entry for one slot; names stay stable across releases.
pub fn source_control_host_secret_name(slot: &SourceControlHostSecretSlot) -> String {
    match &slot.server_host {
        None => format!(
            "source-control-{}-{}",
            encode(&slot.kind),
            encode(&slot.field)
        ),
        Some(host) => format!(
            "source-control-{}-{}-{}",
            encode(&slot.kind),
            encode(&slot.field),
            encode(&host.trim().to_lowercase())
        ),
    }
}

/// The string values a secret field holds, keyed by server host, or `None` for a plain string.
pub fn read_secret_field_values(value: &Value) -> Vec<(Option<String>, String)> {
    match value {
        Value::String(s) => vec![(None, s.clone())],
        Value::Object(map) => map
            .iter()
            .filter_map(|(host, entry)| {
                entry
                    .as_str()
                    .map(|s| (Some(host.trim().to_lowercase()), s.to_owned()))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A secret field's value rebuilt from per-slot values, in the shape it was read from.
pub fn write_secret_field_values(original: &Value, values: &[(Option<String>, String)]) -> Value {
    if original.is_string() {
        return Value::String(values.first().map(|(_, v)| v.clone()).unwrap_or_default());
    }
    let mut map = Map::new();
    for (host, value) in values {
        map.insert(
            host.clone().unwrap_or_default(),
            Value::String(value.clone()),
        );
    }
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn github_declares_tokens_as_secret() {
        assert_eq!(
            SOURCE_CONTROL_HOST_SECRET_FIELDS.get("github"),
            Some(&["tokens"].as_slice())
        );
    }

    #[test]
    fn secret_names_are_stable_base64url() {
        let slot = SourceControlHostSecretSlot {
            kind: "github".into(),
            field: "tokens".into(),
            server_host: Some(" GitHub.com ".into()),
        };
        assert_eq!(
            source_control_host_secret_name(&slot),
            "source-control-Z2l0aHVi-dG9rZW5z-Z2l0aHViLmNvbQ"
        );
        let plain = SourceControlHostSecretSlot {
            server_host: None,
            ..slot
        };
        assert_eq!(
            source_control_host_secret_name(&plain),
            "source-control-Z2l0aHVi-dG9rZW5z"
        );
    }

    #[test]
    fn reads_and_writes_per_host_values() {
        let original = json!({ " GitHub.com ": "abc", "ghe.example.com": 3, "x": "y" });
        let values = read_secret_field_values(&original);
        assert_eq!(
            values,
            vec![
                (Some("github.com".into()), "abc".into()),
                (Some("x".into()), "y".into())
            ]
        );
        assert_eq!(
            write_secret_field_values(&original, &values),
            json!({ "github.com": "abc", "x": "y" })
        );
        assert_eq!(
            read_secret_field_values(&json!("t")),
            vec![(None, "t".into())]
        );
        assert_eq!(
            write_secret_field_values(&json!("t"), &[(None, "new".into())]),
            json!("new")
        );
        assert_eq!(write_secret_field_values(&json!("t"), &[]), json!(""));
        assert!(read_secret_field_values(&json!(5)).is_empty());
    }
}
