//! Port of upstream/apps/server/src/provider/ClaudeModelManifest.ts: the
//! Claude profile and model adapter schemas, decoded from the manifest's JSON
//! `adapter` fields. The catalog itself comes from provider-core's
//! `model_manifest` (typed manifest, `resolveProviderCatalog`). Remote refresh
//! and the disk cache live in that service, which the driver owns.
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Ordering;
use turin_contracts::provider_instance::ProviderDriverKind;
use turin_contracts::{TrimmedNonEmptyString, js_trim};
use turin_provider_core::{
    model_manifest::{ModelManifestData, resolve_provider_catalog},
    semver::{compare_semver_versions, normalize_semver_version},
};

pub const CLAUDE_DRIVER: &str = "claudeAgent";

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

pub use turin_provider_core::model_manifest::{
    ProviderCatalog, ProviderCatalogModel, bundled_model_manifest,
};

/// The Claude entry of a typed manifest, through provider-core's
/// `resolveProviderCatalog`. `None` when the manifest has no Claude entry or
/// its references are invalid.
pub fn resolve_claude_provider_catalog(manifest: &ModelManifestData) -> Option<ProviderCatalog> {
    resolve_provider_catalog(manifest, &ProviderDriverKind::new(CLAUDE_DRIVER).ok()?)
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
        let value = serde_json::to_value(&manifest).expect("manifest serializes");
        assert!(has_valid_claude_manifest_adapters(&value));
        let catalog =
            resolve_claude_provider_catalog(&manifest).expect("bundled Claude catalog resolves");
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
}
