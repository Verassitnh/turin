//! Cursor instance settings.
//!
//! Port of `upstream/packages/provider-cursor/src/settings.ts`. The fields are
//! the browser-safe settings schema. The V2 SDK ignores `binaryPath` and
//! `apiEndpoint`; the ACP adapter uses `binaryPath` as the `cursor-agent`
//! program when it is set.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use turin_contracts::model::CustomModelSetting;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(
        default,
        deserialize_with = "optional_trimmed_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub binary_path: Option<String>,
    #[serde(
        default,
        deserialize_with = "optional_trimmed_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub api_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_models: Vec<CustomModelSetting>,
}

impl Default for CursorSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            binary_path: None,
            api_endpoint: None,
            custom_models: Vec::new(),
        }
    }
}

impl CursorSettings {
    pub fn from_value(value: Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value)
    }

    /// Program used to spawn `cursor-agent acp`. A configured binary wins.
    pub fn agent_program(&self) -> &str {
        self.binary_path.as_deref().unwrap_or("cursor-agent")
    }
}

fn optional_trimmed_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::Null => Err(D::Error::custom("expected a string")),
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_owned()))
            }
        }
        _ => Err(D::Error::custom("expected a string")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn disabled_by_default_and_keeps_hidden_cli_fields() {
        let settings = CursorSettings::from_value(json!({})).unwrap();
        assert!(!settings.enabled);
        assert_eq!(settings.agent_program(), "cursor-agent");
        assert!(settings.custom_models.is_empty());

        let settings = CursorSettings::from_value(json!({
            "enabled": true,
            "binaryPath": "  /opt/cursor-agent  ",
            "apiEndpoint": " https://example.test ",
            "customModels": ["gpt-custom"]
        }))
        .unwrap();
        assert!(settings.enabled);
        assert_eq!(settings.binary_path.as_deref(), Some("/opt/cursor-agent"));
        assert_eq!(
            settings.api_endpoint.as_deref(),
            Some("https://example.test")
        );
        assert_eq!(settings.agent_program(), "/opt/cursor-agent");
        assert_eq!(settings.custom_models.len(), 1);
    }

    #[test]
    fn null_optional_strings_are_rejected() {
        assert!(CursorSettings::from_value(json!({"binaryPath": null})).is_err());
        assert!(CursorSettings::from_value(json!({"apiEndpoint": null})).is_err());
    }
}
