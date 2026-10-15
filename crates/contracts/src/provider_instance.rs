//! Port of upstream/packages/contracts/src/providerInstance.ts
//!
//! `ProviderDriverKind` is intentionally an open branded slug, not a closed
//! literal union: parsing persisted payloads must always succeed and the
//! runtime marks unknown drivers "unavailable" rather than crashing. The
//! driver-specific config payload stays opaque here; each driver owns its own
//! config schema.

use crate::base_schemas::{ContractError, TrimmedNonEmptyString, js_trim};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

const PROVIDER_SLUG_MAX_CHARS: usize = 64;
/// Slug pattern shared by driver kinds and instance ids — letters, digits,
/// dashes, underscores. The first character must be a letter so slugs remain
/// JS-identifier friendly when used as object keys, log fields, or telemetry
/// attributes. Mixed case is permitted so historical driver kinds (e.g.
/// `claudeAgent`) can be used verbatim during the migration and so external
/// fork authors retain reasonable freedom.
const PROVIDER_SLUG_FIRST: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
const PROVIDER_SLUG_REST: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_";
const ENVIRONMENT_VARIABLE_NAME_MAX_CHARS: usize = 128;

fn is_slug(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if PROVIDER_SLUG_FIRST.contains(first) => {}
        _ => return false,
    }
    // `Schema.isMaxLength` counts UTF-16 code units.
    value.encode_utf16().count() <= PROVIDER_SLUG_MAX_CHARS
        && chars.all(|c| PROVIDER_SLUG_REST.contains(c))
}

macro_rules! provider_slug {
    ($(#[$meta:meta])* $name:ident, $brand:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Validates like upstream's branded slug schema: trimmed,
            /// non-empty, `^[a-zA-Z][a-zA-Z0-9_-]*$`, at most 64 chars.
            pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
                let trimmed = js_trim(value.as_ref());
                if !is_slug(trimmed) {
                    return Err(ContractError {
                        expected: concat!("a slug matching ^[a-zA-Z][a-zA-Z0-9_-]*$ (<= 64 chars) for ", $brand),
                        got: value.as_ref().to_owned(),
                    });
                }
                Ok(Self(trimmed.to_owned()))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl ::std::convert::AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl ::std::borrow::Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.0)
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw = <::std::borrow::Cow<'de, str>>::deserialize(d)?;
                Self::new(raw.as_ref()).map_err(::serde::de::Error::custom)
            }
        }
    };
}

provider_slug! {
    /// `ProviderDriverKind` — open branded slug naming a driver implementation.
    ///
    /// Notably **not** validated: that the driver is one we know how to load.
    /// That check belongs to the runtime registry, which downgrades unknown
    /// drivers gracefully.
    ProviderDriverKind,
    "ProviderDriverKind"
}

/// Whether a raw value is a well-formed driver-kind slug (upstream `isProviderDriverKind`).
pub fn is_provider_driver_kind(value: &str) -> bool {
    is_slug(js_trim(value))
}

provider_slug! {
    /// `ProviderInstanceId` — user-defined routing key for a configured
    /// provider instance. Same slug rules as `ProviderDriverKind`; branded
    /// separately so the type system cannot confuse the two.
    ProviderInstanceId,
    "ProviderInstanceId"
}

/// Lightweight reference identifying which driver implements an instance.
/// Carried alongside `ProviderInstanceId` on wire shapes so consumers can
/// branch on driver behavior (icons, capabilities, presentation) without
/// having to look up the instance in the registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstanceRef {
    pub instance_id: ProviderInstanceId,
    pub driver: ProviderDriverKind,
}

/// Environment variable name: `^[a-zA-Z_][a-zA-Z0-9_]*$`, at most 128 chars.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderInstanceEnvironmentVariableName(String);

impl ProviderInstanceEnvironmentVariableName {
    pub fn new(value: impl AsRef<str>) -> Result<Self, ContractError> {
        let trimmed = js_trim(value.as_ref());
        let valid = !trimmed.is_empty()
            && trimmed.encode_utf16().count() <= ENVIRONMENT_VARIABLE_NAME_MAX_CHARS
            && trimmed
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            && trimmed
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if !valid {
            return Err(ContractError {
                expected: "an environment variable name matching ^[a-zA-Z_][a-zA-Z0-9_]*$ (<= 128 chars)",
                got: value.as_ref().to_owned(),
            });
        }
        Ok(Self(trimmed.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderInstanceEnvironmentVariableName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for ProviderInstanceEnvironmentVariableName {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProviderInstanceEnvironmentVariableName {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        Self::new(raw.as_ref()).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstanceEnvironmentVariable {
    pub name: ProviderInstanceEnvironmentVariableName,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub sensitive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_redacted: Option<bool>,
}

/// `ProviderInstanceEnvironment`: an array of environment variables.
pub type ProviderInstanceEnvironment = Vec<ProviderInstanceEnvironmentVariable>;

/// Envelope shape for a provider instance configuration in `ServerSettings`.
///
/// `driver` is intentionally accepted as any well-formed slug (see module
/// docs). The driver-specific config payload is left opaque; each driver
/// registers its own decoder with the runtime registry, and envelopes for
/// unknown drivers are preserved verbatim so they round-trip across version
/// changes without data loss.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstanceConfig {
    pub driver: ProviderDriverKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_color: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<ProviderInstanceEnvironment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// `Schema.optionalKey(Schema.Unknown)`: absent is `None`, an explicit
    /// `null` is preserved as `Some(Value::Null)`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_unknown"
    )]
    pub config: Option<serde_json::Value>,
}

/// `Schema.optional(Schema.Unknown)` / `optionalKey(Schema.Unknown)`: plain
/// `Option<Value>` would fold an explicit `null` into absent; this keeps it as
/// `Some(Value::Null)`.
pub mod optional_unknown {
    use super::*;

    pub fn serialize<S: Serializer>(
        value: &Option<serde_json::Value>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        value.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<serde_json::Value>, D::Error> {
        serde_json::Value::deserialize(d).map(Some)
    }
}

/// Atomic mutation for one provider-instance map entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ProviderInstanceMutation {
    #[serde(rename = "create")]
    Create {
        instance_id: ProviderInstanceId,
        instance: ProviderInstanceConfig,
    },
    #[serde(rename = "upsert")]
    Upsert {
        instance_id: ProviderInstanceId,
        instance: ProviderInstanceConfig,
    },
    #[serde(rename = "remove")]
    Remove { instance_id: ProviderInstanceId },
}

/// Map shape for `ServerSettings.providerInstances`. Keyed by
/// `ProviderInstanceId`, values are envelopes the registry feeds to drivers.
/// `Schema.Record` upstream: insertion order preserved.
pub type ProviderInstanceConfigMap = indexmap::IndexMap<ProviderInstanceId, ProviderInstanceConfig>;

/// Construct the canonical `ProviderInstanceId` used as a back-compat default
/// for a built-in driver. The legacy single-instance-per-driver world used
/// the driver kind itself as the instance id; preserving that mapping keeps
/// existing persisted threads, bindings, and cache files routable across the
/// migration without rewriting their stored selection payloads.
pub fn default_instance_id_for_driver(driver: &ProviderDriverKind) -> ProviderInstanceId {
    // A driver kind is itself a valid instance id, so this cannot fail.
    ProviderInstanceId::new(driver.as_str()).expect("driver kind is a valid instance id")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_driver(value: &str) -> Result<ProviderDriverKind, ContractError> {
        ProviderDriverKind::new(value)
    }

    #[test]
    fn slugs_accept_valid_values() {
        for id in [
            "codex",
            "codex_personal",
            "codex-work",
            "claudeAgent",
            "x",
            "abc123",
            "ollama",
        ] {
            assert_eq!(decode_driver(id).unwrap().as_str(), id);
            assert_eq!(ProviderInstanceId::new(id).unwrap().as_str(), id);
        }
    }

    #[test]
    fn slugs_reject_invalid_values() {
        for value in [
            "",
            "1codex",
            "-codex",
            "_codex",
            "codex personal",
            "codex.personal",
            "codex/personal",
        ] {
            assert!(
                decode_driver(value).is_err(),
                "{value:?} should be rejected"
            );
        }
        assert!(ProviderInstanceId::new("1codex").is_err());
    }

    #[test]
    fn slugs_trim_surrounding_whitespace() {
        assert_eq!(
            decode_driver("  codex_work  ").unwrap().as_str(),
            "codex_work"
        );
    }

    #[test]
    fn slugs_enforce_max_length() {
        assert!(decode_driver(&"a".repeat(65)).is_err());
        let just_right = decode_driver(&"a".repeat(64)).unwrap();
        assert_eq!(just_right.as_str(), "a".repeat(64));
    }

    #[test]
    fn ref_decodes() {
        let r#ref: ProviderInstanceRef =
            serde_json::from_str(r#"{"instanceId":"codex_work","driver":"codex"}"#).unwrap();
        assert_eq!(r#ref.instance_id.as_str(), "codex_work");
        assert_eq!(r#ref.driver.as_str(), "codex");

        // Fork-defined drivers decode without complaint.
        let fork: ProviderInstanceRef =
            serde_json::from_str(r#"{"instanceId":"ollama_local","driver":"ollama"}"#).unwrap();
        assert_eq!(fork.driver.as_str(), "ollama");

        // A driver that is not a valid slug is rejected.
        assert!(
            serde_json::from_str::<ProviderInstanceRef>(
                r#"{"instanceId":"codex","driver":"1nope"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn config_decodes_minimal_envelope() {
        let decoded: ProviderInstanceConfig =
            serde_json::from_str(r#"{"driver":"codex"}"#).unwrap();
        assert_eq!(decoded.driver.as_str(), "codex");
        assert_eq!(decoded.display_name, None);
        assert_eq!(decoded.enabled, None);
        assert_eq!(decoded.config, None);
    }

    #[test]
    fn config_preserves_opaque_payloads() {
        let decoded: ProviderInstanceConfig = serde_json::from_str(
            r#"{"driver":"ollama","displayName":"Ollama","enabled":true,"config":{"someUnknownKnob":42,"model":"llama3"}}"#,
        )
        .unwrap();
        assert_eq!(decoded.driver.as_str(), "ollama");
        assert_eq!(
            decoded.config,
            Some(serde_json::json!({"someUnknownKnob":42,"model":"llama3"}))
        );
    }

    #[test]
    fn config_trims_envelope_fields() {
        let decoded: ProviderInstanceConfig = serde_json::from_str(
            r#"{"driver":"  codex  ","displayName":"  Codex Personal  ","accentColor":"  #dc2626  ","environment":[{"name":"  OPENROUTER_API_KEY  ","value":"  sk-or-test  "}]}"#,
        )
        .unwrap();
        assert_eq!(decoded.driver.as_str(), "codex");
        assert_eq!(
            decoded.display_name.as_ref().unwrap().as_str(),
            "Codex Personal"
        );
        assert_eq!(decoded.accent_color.as_ref().unwrap().as_str(), "#dc2626");
        // `value` is a plain Schema.String: not trimmed.
        assert_eq!(
            decoded.environment.as_ref().unwrap()[0].value,
            "  sk-or-test  "
        );
    }

    #[test]
    fn config_decodes_environment_variables_with_defaults() {
        let decoded: ProviderInstanceConfig = serde_json::from_str(
            r#"{"driver":"claudeAgent","environment":[
                {"name":"ANTHROPIC_BASE_URL","value":"https://openrouter.ai/api","sensitive":false},
                {"name":"OPENROUTER_API_KEY","value":"sk-or-test","sensitive":true},
                {"name":"ANTHROPIC_API_KEY"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            decoded.environment,
            Some(vec![
                ProviderInstanceEnvironmentVariable {
                    name: ProviderInstanceEnvironmentVariableName::new("ANTHROPIC_BASE_URL")
                        .unwrap(),
                    value: "https://openrouter.ai/api".to_owned(),
                    sensitive: false,
                    value_redacted: None,
                },
                ProviderInstanceEnvironmentVariable {
                    name: ProviderInstanceEnvironmentVariableName::new("OPENROUTER_API_KEY")
                        .unwrap(),
                    value: "sk-or-test".to_owned(),
                    sensitive: true,
                    value_redacted: None,
                },
                // `value` defaults to "" and `sensitive` to false on decode.
                ProviderInstanceEnvironmentVariable {
                    name: ProviderInstanceEnvironmentVariableName::new("ANTHROPIC_API_KEY")
                        .unwrap(),
                    value: String::new(),
                    sensitive: false,
                    value_redacted: None,
                },
            ])
        );
    }

    #[test]
    fn config_rejects_invalid_environment_variable_names() {
        assert!(
            serde_json::from_str::<ProviderInstanceConfig>(
                r#"{"driver":"codex","environment":[{"name":"HAS-DASH","value":"x"}]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn config_rejects_blank_display_name() {
        assert!(
            serde_json::from_str::<ProviderInstanceConfig>(
                r#"{"driver":"codex","displayName":"   "}"#
            )
            .is_err()
        );
    }

    #[test]
    fn config_map_decodes_multi_instance() {
        let decoded: ProviderInstanceConfigMap = serde_json::from_str(
            r#"{"codex_personal":{"driver":"codex","displayName":"Codex (personal)","config":{"homePath":"~/.codex_personal"}},
                "codex_work":{"driver":"codex","config":{"homePath":"~/.codex_work"}},
                "claudeAgent":{"driver":"claudeAgent"},
                "ollama_local":{"driver":"ollama","config":{"endpoint":"http://localhost:11434"}}}"#,
        )
        .unwrap();
        // `Schema.Record` preserves document order (IndexMap), unlike a
        // sorted map; assert the order the JSON arrived in.
        let keys: Vec<&str> = decoded.keys().map(|k| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "codex_personal",
                "codex_work",
                "claudeAgent",
                "ollama_local"
            ]
        );
        assert_eq!(
            decoded
                .get(&ProviderInstanceId::new("codex_work").unwrap())
                .unwrap()
                .config,
            Some(serde_json::json!({"homePath":"~/.codex_work"}))
        );

        assert!(
            serde_json::from_str::<ProviderInstanceConfigMap>(r#"{"1codex":{"driver":"codex"}}"#)
                .is_err()
        );
    }

    #[test]
    fn mutation_round_trips() {
        let mutation = ProviderInstanceMutation::Remove {
            instance_id: ProviderInstanceId::new("codex").unwrap(),
        };
        let json = serde_json::to_string(&mutation).unwrap();
        assert_eq!(json, r#"{"operation":"remove","instanceId":"codex"}"#);
        let back: ProviderInstanceMutation = serde_json::from_str(&json).unwrap();
        assert_eq!(back, mutation);
    }

    #[test]
    fn slug_trim_uses_js_semantics() {
        // U+FEFF is JS whitespace, U+0085 is not.
        assert_eq!(decode_driver("\u{feff}codex").unwrap().as_str(), "codex");
        assert!(decode_driver("codex\u{85}").is_err());
    }

    #[test]
    fn config_keeps_null_config_distinct_from_absent() {
        let null: ProviderInstanceConfig =
            serde_json::from_str(r#"{"driver":"codex","config":null}"#).unwrap();
        assert_eq!(null.config, Some(serde_json::Value::Null));
        assert_eq!(
            serde_json::to_string(&null).unwrap(),
            r#"{"driver":"codex","config":null}"#
        );
        let absent: ProviderInstanceConfig = serde_json::from_str(r#"{"driver":"codex"}"#).unwrap();
        assert_eq!(absent.config, None);
        assert_eq!(
            serde_json::to_string(&absent).unwrap(),
            r#"{"driver":"codex"}"#
        );
    }

    #[test]
    fn environment_variable_always_encodes_defaults() {
        let var: ProviderInstanceEnvironmentVariable =
            serde_json::from_str(r#"{"name":"A"}"#).unwrap();
        assert_eq!(
            serde_json::to_string(&var).unwrap(),
            r#"{"name":"A","value":"","sensitive":false}"#
        );
        let redacted: ProviderInstanceEnvironmentVariable =
            serde_json::from_str(r#"{"name":"A","valueRedacted":true}"#).unwrap();
        assert_eq!(redacted.value_redacted, Some(true));
    }

    #[test]
    fn mutation_decodes_all_operations_and_rejects_unknown() {
        let create: ProviderInstanceMutation = serde_json::from_str(
            r#"{"operation":"create","instanceId":"codex","instance":{"driver":"codex"}}"#,
        )
        .unwrap();
        assert!(matches!(create, ProviderInstanceMutation::Create { .. }));
        let upsert: ProviderInstanceMutation = serde_json::from_str(
            r#"{"operation":"upsert","instanceId":"codex","instance":{"driver":"codex"}}"#,
        )
        .unwrap();
        assert!(matches!(upsert, ProviderInstanceMutation::Upsert { .. }));
        // Plain (non-forward-compatible) upstream union: unknown operations fail.
        assert!(
            serde_json::from_str::<ProviderInstanceMutation>(
                r#"{"operation":"rename","instanceId":"codex"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn default_instance_id_is_driver_slug() {
        let driver = ProviderDriverKind::new("claudeAgent").unwrap();
        assert_eq!(
            default_instance_id_for_driver(&driver).as_str(),
            "claudeAgent"
        );
        assert!(is_provider_driver_kind("claudeAgent"));
        assert!(!is_provider_driver_kind("1x"));
    }
}
