//! Port of upstream/packages/contracts/src/modelSelection.ts

use crate::base_schemas::TrimmedNonEmptyString;
use crate::model::ProviderOptionSelection;
use crate::provider_instance::ProviderInstanceId;
use serde::{Deserialize, Serialize};

/// Selection of a model on a configured provider instance.
///
/// `instanceId` is the routing key. The provider driver, credentials,
/// environment, and continuation identity are resolved by the provider
/// instance registry rather than encoded into the selection itself.
///
/// The decoder still accepts the historical `{ provider, model }` shape while
/// V1 persistence remains readable: a legacy `provider` driver kind is
/// promoted to `instanceId` (built-in drivers use the driver kind slug as
/// their default instance id, so the migration is a 1:1 rename). Runtime code
/// only receives the canonical instance-based representation.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelection {
    pub instance_id: ProviderInstanceId,
    pub model: TrimmedNonEmptyString,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<ProviderOptionSelection>>,
}

impl<'de> Deserialize<'de> for ModelSelection {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Source {
            #[serde(default)]
            provider: Option<serde_json::Value>,
            #[serde(default)]
            instance_id: Option<serde_json::Value>,
            model: serde_json::Value,
            #[serde(default)]
            options: Option<serde_json::Value>,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            instance_id: ProviderInstanceId,
            model: TrimmedNonEmptyString,
            #[serde(default)]
            options: Option<serde_json::Value>,
        }

        let raw = Source::deserialize(d)?;
        // Prefer an explicit instanceId over the legacy provider field.
        let instance_id = raw.instance_id.or(match raw.provider {
            Some(serde_json::Value::String(provider)) => Some(serde_json::Value::String(provider)),
            _ => None,
        });
        let mut wire = serde_json::Map::new();
        wire.insert(
            "instanceId".to_owned(),
            instance_id.unwrap_or(serde_json::Value::Null),
        );
        wire.insert("model".to_owned(), raw.model);
        if let Some(options) = raw.options {
            wire.insert("options".to_owned(), options);
        }
        let wire: Wire = serde_json::from_value(serde_json::Value::Object(wire)).map_err(serde::de::Error::custom)?;
        let options = match wire.options {
            None => None,
            Some(value) => Some(
                crate::model::provider_option_selections::deserialize(value)
                    .map_err(serde::de::Error::custom)?,
            ),
        };
        Ok(Self {
            instance_id: wire.instance_id,
            model: wire.model,
            options,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_legacy_provider_field_to_instance_id() {
        let parsed: ModelSelection = serde_json::from_str(
            r#"{"provider":"codex","model":"gpt-5-codex","options":[{"id":"reasoningEffort","value":"high"}]}"#,
        )
        .unwrap();
        assert_eq!(parsed.instance_id.as_str(), "codex");
        assert_eq!(parsed.model.as_str(), "gpt-5-codex");
        assert_eq!(
            parsed.options,
            Some(vec![ProviderOptionSelection {
                id: TrimmedNonEmptyString::new("reasoningEffort").unwrap(),
                value: crate::model::ProviderOptionSelectionValue::Str(
                    TrimmedNonEmptyString::new("high").unwrap()
                ),
            }])
        );
    }

    #[test]
    fn accepts_explicit_instance_id() {
        let parsed: ModelSelection =
            serde_json::from_str(r#"{"instanceId":"codex_personal","model":"gpt-5-codex"}"#).unwrap();
        assert_eq!(parsed.instance_id.as_str(), "codex_personal");
    }

    #[test]
    fn prefers_explicit_instance_id_over_legacy_provider() {
        let parsed: ModelSelection = serde_json::from_str(
            r#"{"provider":"codex","instanceId":"codex_personal","model":"gpt-5-codex"}"#,
        )
        .unwrap();
        assert_eq!(parsed.instance_id.as_str(), "codex_personal");
    }

    #[test]
    fn decodes_unknown_driver_kinds_via_legacy_provider() {
        // Rollback / fork invariant: a fork's driver still routes.
        let parsed: ModelSelection =
            serde_json::from_str(r#"{"provider":"ollama","model":"llama3:70b"}"#).unwrap();
        assert_eq!(parsed.instance_id.as_str(), "ollama");
        assert_eq!(parsed.model.as_str(), "llama3:70b");
    }

    #[test]
    fn encodes_canonical_instance_id_form() {
        let parsed: ModelSelection = serde_json::from_str(
            r#"{"provider":"ollama","model":"llama3:70b","options":{"temperature":"0.4"}}"#,
        )
        .unwrap();
        let encoded = serde_json::to_value(&parsed).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!({
                "instanceId": "ollama",
                "model": "llama3:70b",
                "options": [{"id":"temperature","value":"0.4"}],
            })
        );
    }

    #[test]
    fn rejects_malformed_instance_ids() {
        assert!(serde_json::from_str::<ModelSelection>(
            r#"{"instanceId":"1invalid","model":"x"}"#
        )
        .is_err());
    }
}
