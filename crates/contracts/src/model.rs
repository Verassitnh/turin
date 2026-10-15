//! Port of upstream/packages/contracts/src/model.ts

use crate::base_schemas::TrimmedNonEmptyString;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderOptionDescriptorType {
    Select,
    Boolean,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderOptionChoice {
    pub id: TrimmedNonEmptyString,
    pub label: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
}

string_literal!(
    /// Constant `"select"` type tag of a select option descriptor.
    SelectOptionType,
    "select"
);
string_literal!(
    /// Constant `"boolean"` type tag of a boolean option descriptor.
    BooleanOptionType,
    "boolean"
);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectProviderOptionDescriptor {
    pub id: TrimmedNonEmptyString,
    pub label: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<TrimmedNonEmptyString>,
    #[serde(rename = "type")]
    pub type_: SelectOptionType,
    pub options: Vec<ProviderOptionChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_value: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_injected_values: Option<Vec<TrimmedNonEmptyString>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BooleanProviderOptionDescriptor {
    pub id: TrimmedNonEmptyString,
    pub label: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<TrimmedNonEmptyString>,
    #[serde(rename = "type")]
    pub type_: BooleanOptionType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_value: Option<bool>,
}

/// The descriptor union, untagged like upstream's `Schema.Union`: each member
/// struct carries its own `type` literal, so matching is exact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderOptionDescriptor {
    Select(SelectProviderOptionDescriptor),
    Boolean(BooleanProviderOptionDescriptor),
}

/// `TrimmedNonEmptyString | boolean`; the string is tried first, like upstream's union order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderOptionSelectionValue {
    Str(TrimmedNonEmptyString),
    Bool(bool),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderOptionSelection {
    pub id: TrimmedNonEmptyString,
    pub value: ProviderOptionSelectionValue,
}

/// Serde codec for the `options` field of every `ModelSelection` variant.
///
/// Accepts both the canonical array shape `[{id, value}]` (preferred) and the
/// legacy object shape `{effort: "max", fastMode: true}` from pre-migration
/// data. Always normalizes to the canonical array on decode and re-encodes as
/// the canonical array, so any legacy storage gets cleaned up the next time
/// the containing record is written back.
pub mod provider_option_selections {
    use super::*;

    pub fn serialize<S: serde::Serializer>(
        values: &[ProviderOptionSelection],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        values.serialize(s)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<ProviderOptionSelection>, D::Error> {
        let raw = serde_json::Value::deserialize(d)?;
        if raw.is_array() {
            Vec::<ProviderOptionSelection>::deserialize(raw).map_err(serde::de::Error::custom)
        } else if raw.is_object() {
            let record = serde_json::Map::<String, serde_json::Value>::deserialize(raw)
                .map_err(serde::de::Error::custom)?;
            Ok(coerce_legacy_options_object_to_array(record))
        } else {
            Err(serde::de::Error::custom(format!(
                "expected an array or object of provider option selections, got {raw}"
            )))
        }
    }

    /// Legacy on-disk shape (`{ effort: "max", fastMode: true }`), readable so
    /// we can tolerate stored data written before the v3 array shape:
    /// `settings.json` files from older client builds, SQLite databases that
    /// have not yet run migration 026, and any future regression that
    /// re-introduces the legacy shape.
    fn coerce_legacy_options_object_to_array(
        record: serde_json::Map<String, serde_json::Value>,
    ) -> Vec<ProviderOptionSelection> {
        let mut entries = Vec::new();
        for (raw_key, raw_value) in record {
            let id = raw_key.trim();
            if id.is_empty() {
                continue;
            }
            match raw_value {
                serde_json::Value::String(raw) => {
                    let trimmed = raw.trim();
                    if !trimmed.is_empty() {
                        entries.push(ProviderOptionSelection {
                            id: TrimmedNonEmptyString::new(id).expect("trimmed non-empty checked above"),
                            value: ProviderOptionSelectionValue::Str(
                                TrimmedNonEmptyString::new(trimmed).expect("trimmed non-empty checked above"),
                            ),
                        });
                    }
                }
                serde_json::Value::Bool(value) => {
                    entries.push(ProviderOptionSelection {
                        id: TrimmedNonEmptyString::new(id).expect("trimmed non-empty checked above"),
                        value: ProviderOptionSelectionValue::Bool(value),
                    });
                }
                // Drop anything else (numbers, null, nested objects/arrays) to
                // match the permissive normalization performed by migration 026.
                _ => {}
            }
        }
        entries
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_descriptors: Option<Vec<ProviderOptionDescriptor>>,
}

/// A user-authored custom model. `name` and `capabilities` are optional so a
/// bare slug keeps its driver-default presentation; when `capabilities` is
/// set, its descriptors replace the driver default in the model picker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomModelEntry {
    pub slug: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ModelCapabilities>,
}

/// On-disk custom model setting: the legacy bare slug, or a full entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CustomModelSetting {
    Slug(String),
    Entry(CustomModelEntry),
}

pub const DEFAULT_MODEL: &str = "gpt-6-astra";

/// Codex default-model preference, most preferred first. The provider
/// snapshot marks the first of these present in the live `model/list`
/// response as default; when none are available, Codex's own `isDefault`
/// flag wins.
pub const PREFERRED_DEFAULT_CODEX_MODELS: &[&str] =
    &[DEFAULT_MODEL, "gpt-5.6-sol", "gpt-5.6-terra"];
pub const DEFAULT_TEXT_GENERATION_MODEL: &str = "gpt-6-luna";
/// Keep the official Antigravity session's current model. Never send this ID to ACP.
pub const ANTIGRAVITY_DEFAULT_MODEL: &str = "antigravity-default";
pub const DEFAULT_TEXT_GENERATION_REASONING_EFFORT: &str = "low";

/// Let Muse pick the account's default model. Never send this ID to Muse.
pub const MUSE_DEFAULT_MODEL: &str = "default";

/// Upstream `DEFAULT_MODEL_BY_PROVIDER` (a partial record keyed by driver
/// kind), as a lookup. Turin keeps the cut drivers' entries so upstream
/// settings rows still resolve.
pub fn default_model_by_provider(driver: &str) -> Option<&'static str> {
    match driver {
        "codex" => Some(DEFAULT_MODEL),
        "claudeAgent" => Some("claude-fable-5-1"),
        "cursor" => Some("auto"),
        // Product slug, not an ACP model id. The Grok adapter treats it as
        // "the session's current model".
        "grok" => Some("grok-build"),
        "muse" => Some(MUSE_DEFAULT_MODEL),
        "acpRegistry" => Some("default"),
        // "default" defers to the user's own Pi settings.json model selection.
        "pi" => Some("default"),
        "opencode" => Some("openai/gpt-5"),
        "antigravity" => Some(ANTIGRAVITY_DEFAULT_MODEL),
        _ => None,
    }
}

/// Per-provider text generation model defaults.
pub fn default_text_generation_model_by_provider(driver: &str) -> Option<&'static str> {
    match driver {
        "codex" => Some(DEFAULT_TEXT_GENERATION_MODEL),
        "antigravity" => Some(ANTIGRAVITY_DEFAULT_MODEL),
        "claudeAgent" => Some("claude-haiku-5-5"),
        "cursor" => Some("composer-2"),
        "opencode" => Some("openai/gpt-5"),
        _ => None,
    }
}

/// Upstream `MODEL_SLUG_ALIASES_BY_PROVIDER`: (alias, canonical) pairs per driver.
pub fn model_slug_aliases_by_provider(driver: &str) -> Option<&'static [(&'static str, &'static str)]> {
    match driver {
        "codex" => Some(&[
            ("gpt-5-codex", "gpt-5.4"),
            ("5.4", "gpt-5.4"),
            ("5.3", "gpt-5.3-codex"),
            ("gpt-5.3", "gpt-5.3-codex"),
            ("5.3-spark", "gpt-5.3-codex-spark"),
            ("gpt-5.3-spark", "gpt-5.3-codex-spark"),
        ]),
        "claudeAgent" => Some(&[]),
        "cursor" => Some(&[
            ("composer", "composer-2"),
            ("composer-1.5", "composer-1.5"),
            ("composer-1", "composer-1.5"),
            ("opus-4.6-thinking", "claude-opus-4-6"),
            ("opus-4.6", "claude-opus-4-6"),
            ("sonnet-4.6-thinking", "claude-sonnet-4-6"),
            ("sonnet-4.6", "claude-sonnet-4-6"),
            ("opus-4.5-thinking", "claude-opus-4-5"),
            ("opus-4.5", "claude-opus-4-5"),
        ]),
        "opencode" => Some(&[]),
        _ => None,
    }
}

// ── Provider display names ────────────────────────────────────────────

pub fn provider_display_name(driver: &str) -> Option<&'static str> {
    match driver {
        "antigravity" => Some("Antigravity"),
        "codex" => Some("Codex"),
        "claudeAgent" => Some("Claude"),
        "cursor" => Some("Cursor"),
        "grok" => Some("Grok"),
        "muse" => Some("Muse Code"),
        "acpRegistry" => Some("ACP Registry"),
        "pi" => Some("Pi"),
        "opencode" => Some("OpenCode"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_union_decodes_by_type_tag() {
        let select: ProviderOptionDescriptor = serde_json::from_str(
            r#"{"type":"select","id":"effort","label":"Effort","options":[{"id":"low","label":"Low"}],"currentValue":"low"}"#,
        )
        .unwrap();
        assert!(matches!(select, ProviderOptionDescriptor::Select(_)));

        let boolean: ProviderOptionDescriptor = serde_json::from_str(
            r#"{"type":"boolean","id":"fastMode","label":"Fast mode","currentValue":true}"#,
        )
        .unwrap();
        assert!(matches!(boolean, ProviderOptionDescriptor::Boolean(_)));
    }

    #[test]
    fn selections_normalize_legacy_object_shape() {
        #[derive(Deserialize)]
        struct Holder {
            #[serde(with = "provider_option_selections")]
            options: Vec<ProviderOptionSelection>,
        }
        let holder: Holder = serde_json::from_str(
            r#"{"options":{"effort":"max","fastMode":true,"emptyStr":"   ","nullish":null,"nested":{"foo":1}}}"#,
        )
        .unwrap();
        assert_eq!(holder.options.len(), 2);
        assert_eq!(holder.options[0].id.as_str(), "effort");
        assert_eq!(holder.options[0].value, ProviderOptionSelectionValue::Str(TrimmedNonEmptyString::new("max").unwrap()));
        assert_eq!(holder.options[1].value, ProviderOptionSelectionValue::Bool(true));

        // Re-encode is the canonical array.
        let encoded = serde_json::to_string(&holder.options).unwrap();
        assert_eq!(
            encoded,
            r#"[{"id":"effort","value":"max"},{"id":"fastMode","value":true}]"#
        );
    }

    #[test]
    fn selections_accept_canonical_array() {
        #[derive(Deserialize)]
        struct Holder {
            #[serde(with = "provider_option_selections")]
            options: Vec<ProviderOptionSelection>,
        }
        let holder: Holder =
            serde_json::from_str(r#"{"options":[{"id":"effort","value":"high"}]}"#).unwrap();
        assert_eq!(holder.options.len(), 1);
        assert_eq!(holder.options[0].id.as_str(), "effort");
    }

    #[test]
    fn custom_model_setting_accepts_both_shapes() {
        let slug: CustomModelSetting = serde_json::from_str(r#""gpt-5.4""#).unwrap();
        assert_eq!(slug, CustomModelSetting::Slug("gpt-5.4".to_owned()));
        let entry: CustomModelSetting =
            serde_json::from_str(r#"{"slug":"my-model","name":"My Model"}"#).unwrap();
        assert!(matches!(entry, CustomModelSetting::Entry(_)));
    }
}
