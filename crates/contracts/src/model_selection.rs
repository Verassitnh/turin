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
        // `ModelSelectionSource`: every key is `Schema.Unknown`, so presence
        // (not nullness) is what the migration inspects. A present `null` for
        // `instanceId` or `options` therefore reaches the wire schema and
        // fails there, exactly like upstream.
        let mut source = serde_json::Map::<String, serde_json::Value>::deserialize(d)?;
        let instance_id = match source.remove("instanceId") {
            Some(explicit) => Some(explicit),
            None => match source.remove("provider") {
                Some(serde_json::Value::String(provider)) => {
                    Some(serde_json::Value::String(provider))
                }
                _ => None,
            },
        };
        let instance_id =
            instance_id.ok_or_else(|| serde::de::Error::missing_field("instanceId"))?;
        let model = source
            .remove("model")
            .ok_or_else(|| serde::de::Error::missing_field("model"))?;
        let options = match source.remove("options") {
            None => None,
            Some(value) => Some(
                crate::model::provider_option_selections::deserialize(value)
                    .map_err(serde::de::Error::custom)?,
            ),
        };
        Ok(Self {
            instance_id: serde_json::from_value(instance_id).map_err(serde::de::Error::custom)?,
            model: serde_json::from_value(model).map_err(serde::de::Error::custom)?,
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
            serde_json::from_str(r#"{"instanceId":"codex_personal","model":"gpt-5-codex"}"#)
                .unwrap();
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
        assert!(
            serde_json::from_str::<ModelSelection>(r#"{"instanceId":"1invalid","model":"x"}"#)
                .is_err()
        );
    }

    #[test]
    fn legacy_options_object_is_normalized_and_reencoded_canonically() {
        let decoded: ModelSelection = serde_json::from_str(
            r#"{"provider":"claudeAgent","model":"claude-opus-4-6","options":{"effort":"max","fastMode":true,"emptyStr":"   ","nullish":null,"nested":{"foo":1}}}"#,
        )
        .unwrap();
        let encoded = serde_json::to_value(&decoded).unwrap();
        assert_eq!(
            encoded["options"],
            serde_json::json!([
                {"id":"effort","value":"max"},
                {"id":"fastMode","value":true}
            ])
        );
        // Round trip of the canonical form is stable.
        let again: ModelSelection = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(serde_json::to_value(&again).unwrap(), encoded);
    }

    #[test]
    fn options_absent_stays_absent_and_null_is_rejected() {
        let bare: ModelSelection =
            serde_json::from_str(r#"{"instanceId":"codex","model":"m"}"#).unwrap();
        assert_eq!(bare.options, None);
        assert_eq!(
            serde_json::to_string(&bare).unwrap(),
            r#"{"instanceId":"codex","model":"m"}"#
        );
        // `options` is optionalKey: an explicit null reaches the wire schema
        // and fails, it is not folded into "absent".
        assert!(
            serde_json::from_str::<ModelSelection>(
                r#"{"instanceId":"codex","model":"m","options":null}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<ModelSelection>(
                r#"{"instanceId":null,"provider":"codex","model":"m"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn missing_or_blank_routing_fields_fail() {
        for raw in [
            r#"{"model":"m"}"#,
            r#"{"provider":5,"model":"m"}"#,
            r#"{"instanceId":"codex"}"#,
            r#"{"instanceId":"codex","model":"   "}"#,
        ] {
            assert!(
                serde_json::from_str::<ModelSelection>(raw).is_err(),
                "{raw}"
            );
        }
    }
}
