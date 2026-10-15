//! Port of upstream/apps/server/src/provider/ClaudeModelManifest.ts, plus the
//! Claude slice of `ModelManifest.ts` (`resolveProviderCatalog`) that
//! `ClaudeModelCatalog` reads.
//!
//! The manifest is static data: `model-manifest.json` is compiled into the
//! binary, the same file upstream imports as its bundle. Upstream also
//! refreshes it over HTTP with a disk cache. Turin ships the bundle only, so
//! there is no remote refresh (see PORTING.md, provider-claude model manifest).
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{cmp::Ordering, collections::HashSet, sync::OnceLock};
use turin_contracts::{TrimmedNonEmptyString, js_trim, model::ModelCapabilities};
use turin_provider_core::{
    semver::{compare_semver_versions, normalize_semver_version},
    server_provider::ServerProviderModelBadge,
};

pub const CLAUDE_DRIVER: &str = "claudeAgent";

/// `model-manifest.json`, the bundle shared with `apps/server`.
const BUNDLED_MODEL_MANIFEST_JSON: &str = include_str!("../../provider-core/model-manifest.json");

/// `ClaudeCodeProfileSchema`: the runtime mapping for one capability profile.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCodeProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_map: Option<IndexMap<TrimmedNonEmptyString, Option<TrimmedNonEmptyString>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_suffixes: Option<
        IndexMap<TrimmedNonEmptyString, IndexMap<TrimmedNonEmptyString, TrimmedNonEmptyString>>,
    >,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<IndexMap<TrimmedNonEmptyString, f64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed_context_window_tokens: Option<f64>,
}

/// `ClaudeProfileAdapterSchema`: the adapter payload on a manifest profile.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeProfileAdapter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_code: Option<ClaudeCodeProfile>,
}

/// `ClaudeCodeCompatibilitySchema`: the Claude Code versions a model needs.
/// Both bounds are semver and `minVersion < maxVersionExclusive` when both exist.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCodeCompatibility {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_version_exclusive: Option<String>,
}

/// `ClaudeModelAdapterSchema`: the adapter payload on a manifest model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeModelAdapter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_code: Option<ClaudeCodeCompatibility>,
}

/// `SEMVER_NUMBER_SEGMENT` plus `parseSemver(version) !== null` for a
/// trimmed, non-empty string.
fn is_semver(value: &str) -> bool {
    let trimmed = js_trim(value);
    if trimmed.is_empty() {
        return false;
    }
    let normalized = normalize_semver_version(trimmed);
    let normalized = normalized.strip_prefix('v').unwrap_or(&normalized);
    let main = normalized.split('-').next().unwrap_or("");
    let segments: Vec<&str> = main.split('.').collect();
    segments.len() == 3
        && segments
            .iter()
            .all(|segment| !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit()))
}

/// An adapter payload is absent when the key is missing or `null`, which
/// upstream reads as `{}`.
fn adapter_payload(adapter: Option<&Value>) -> Value {
    match adapter {
        None | Some(Value::Null) => Value::Object(Default::default()),
        Some(value) => value.clone(),
    }
}

pub fn decode_claude_profile_adapter(adapter: Option<&Value>) -> Option<ClaudeProfileAdapter> {
    serde_json::from_value(adapter_payload(adapter)).ok()
}

/// Decodes a model's adapter payload and checks its compatibility bounds.
pub fn decode_claude_model_adapter(adapter: Option<&Value>) -> Option<ClaudeModelAdapter> {
    let decoded: ClaudeModelAdapter = serde_json::from_value(adapter_payload(adapter)).ok()?;
    if let Some(code) = &decoded.claude_code {
        for version in [&code.min_version, &code.max_version_exclusive]
            .into_iter()
            .flatten()
        {
            if !is_semver(version) {
                return None;
            }
        }
        if let (Some(min), Some(max)) = (&code.min_version, &code.max_version_exclusive)
            && compare_semver_versions(min, max) != Ordering::Less
        {
            return None;
        }
    }
    Some(decoded)
}

/// `hasValidClaudeManifestAdapters`: every Claude profile and model adapter
/// decodes. A manifest with no Claude entry is valid.
pub fn has_valid_claude_manifest_adapters(manifest: &Value) -> bool {
    let Some(provider) = manifest
        .get("providers")
        .and_then(|providers| providers.get(CLAUDE_DRIVER))
    else {
        return true;
    };
    let (Some(profiles), Some(models)) = (
        provider.get("profiles").and_then(Value::as_object),
        provider.get("models").and_then(Value::as_array),
    ) else {
        return false;
    };
    profiles
        .values()
        .all(|profile| decode_claude_profile_adapter(profile.get("adapter")).is_some())
        && models
            .iter()
            .all(|model| decode_claude_model_adapter(model.get("adapter")).is_some())
}

/// `ModelCatalog.ProviderCatalogModel`. Provider-owned metadata stays as JSON
/// (`adapter`, `profile_adapter`); the provider decodes it with the functions above.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderCatalogModel {
    pub slug: String,
    pub name: String,
    pub short_name: Option<String>,
    pub sub_provider: Option<String>,
    pub aliases: Option<Vec<String>>,
    pub badge: Option<ServerProviderModelBadge>,
    pub is_legacy: bool,
    pub capabilities: Option<ModelCapabilities>,
    pub adapter: Option<Value>,
    pub profile_adapter: Option<Value>,
}

/// `ModelCatalog.ProviderCatalog`: one provider's entry in the manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderCatalog {
    pub models: Vec<ProviderCatalogModel>,
    pub default_chat_model: Option<String>,
}

/// `resolveProviderCatalog(manifest, driverKind)`: the driver's catalog entry,
/// or `None` when the manifest has no entry or its references are invalid
/// (duplicate slugs, a missing profile, a missing chat default, or
/// capabilities that do not decode).
pub fn resolve_provider_catalog(manifest: &Value, driver: &str) -> Option<ProviderCatalog> {
    let catalog = manifest.get("providers")?.get(driver)?;
    let profiles = catalog.get("profiles")?.as_object()?;
    let entries = catalog.get("models")?.as_array()?;

    let mut seen = HashSet::new();
    let mut models = Vec::with_capacity(entries.len());
    for entry in entries {
        let slug = entry.get("slug")?.as_str()?;
        if !seen.insert(slug.to_owned()) {
            return None;
        }
        let profile = match entry.get("profile").and_then(Value::as_str) {
            Some(profile_id) => Some(profiles.get(profile_id)?),
            None => None,
        };
        let capabilities = match profile.and_then(|profile| profile.get("capabilities")) {
            Some(value) => Some(serde_json::from_value(value.clone()).ok()?),
            None => None,
        };
        models.push(ProviderCatalogModel {
            slug: slug.to_owned(),
            name: entry.get("name")?.as_str()?.to_owned(),
            short_name: entry
                .get("shortName")
                .and_then(Value::as_str)
                .map(str::to_owned),
            sub_provider: entry
                .get("subProvider")
                .and_then(Value::as_str)
                .map(str::to_owned),
            aliases: match entry.get("aliases") {
                Some(aliases) => Some(
                    aliases
                        .as_array()?
                        .iter()
                        .map(|alias| alias.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()?,
                ),
                None => None,
            },
            badge: match entry.get("badge").and_then(Value::as_str) {
                Some("new") => Some(ServerProviderModelBadge::New),
                Some(_) => return None,
                None => None,
            },
            is_legacy: match entry.get("status").and_then(Value::as_str)? {
                "current" => false,
                "legacy" => true,
                _ => return None,
            },
            capabilities,
            adapter: entry.get("adapter").cloned(),
            profile_adapter: profile.and_then(|profile| profile.get("adapter")).cloned(),
        });
    }

    let default_chat_model = match catalog.get("defaults").and_then(|d| d.get("chat")) {
        Some(chat) => {
            let chat = chat.as_str()?;
            if !seen.contains(chat) {
                return None;
            }
            Some(chat.to_owned())
        }
        None => None,
    };
    Some(ProviderCatalog {
        models,
        default_chat_model,
    })
}

/// The bundled `model-manifest.json`, parsed once.
pub fn bundled_model_manifest() -> &'static Value {
    static MANIFEST: OnceLock<Value> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        serde_json::from_str(BUNDLED_MODEL_MANIFEST_JSON).expect("bundled model manifest is JSON")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest_with_claude(profiles: Value, models: Value) -> Value {
        json!({
            "version": 1,
            "currentModels": {},
            "providers": { "claudeAgent": { "profiles": profiles, "models": models } }
        })
    }

    #[test]
    fn bundled_manifest_has_valid_claude_adapters_and_chat_default() {
        let manifest = bundled_model_manifest();
        assert_eq!(manifest["version"], 1);
        assert!(has_valid_claude_manifest_adapters(manifest));
        let catalog = resolve_provider_catalog(manifest, CLAUDE_DRIVER)
            .expect("bundled Claude catalog resolves");
        assert!(!catalog.models.is_empty());
        assert_eq!(
            catalog.default_chat_model.as_deref(),
            Some("claude-fable-5-1")
        );
    }

    #[test]
    fn rejects_malformed_effort_map_values() {
        let manifest = manifest_with_claude(
            json!({"synthetic": {"adapter": {"claudeCode": {"effortMap": {"extreme": 123}}}}}),
            json!([]),
        );
        assert!(!has_valid_claude_manifest_adapters(&manifest));
    }

    #[test]
    fn rejects_model_compatibility_that_is_not_a_semver_range() {
        let manifest = manifest_with_claude(
            json!({}),
            json!([{"slug": "m", "name": "M", "status": "current",
                    "adapter": {"claudeCode": {"minVersion": "soon"}}}]),
        );
        assert!(!has_valid_claude_manifest_adapters(&manifest));

        let inverted = manifest_with_claude(
            json!({}),
            json!([{"slug": "m", "name": "M", "status": "current",
                    "adapter": {"claudeCode": {"minVersion": "3.2.0", "maxVersionExclusive": "3.2.0"}}}]),
        );
        assert!(!has_valid_claude_manifest_adapters(&inverted));
    }

    #[test]
    fn accepts_shorthand_versions_and_null_adapters() {
        let manifest = manifest_with_claude(
            json!({"plain": {"adapter": null}}),
            json!([{"slug": "m", "name": "M", "status": "legacy",
                    "adapter": {"claudeCode": {"minVersion": "3.2", "maxVersionExclusive": "4"}}}]),
        );
        assert!(has_valid_claude_manifest_adapters(&manifest));
    }

    #[test]
    fn a_manifest_without_a_claude_entry_is_valid() {
        assert!(has_valid_claude_manifest_adapters(
            &json!({"providers": {}})
        ));
    }

    #[test]
    fn resolve_rejects_missing_profiles_and_bad_chat_defaults() {
        let dangling = manifest_with_claude(
            json!({}),
            json!([{"slug": "m", "name": "M", "status": "current", "profile": "nope"}]),
        );
        assert!(resolve_provider_catalog(&dangling, CLAUDE_DRIVER).is_none());

        let mut unknown_default = manifest_with_claude(
            json!({}),
            json!([{"slug": "m", "name": "M", "status": "current"}]),
        );
        unknown_default["providers"]["claudeAgent"]["defaults"] = json!({"chat": "other"});
        assert!(resolve_provider_catalog(&unknown_default, CLAUDE_DRIVER).is_none());
    }
}
