//! Port of upstream/apps/server/src/provider/ClaudeModelCatalog.ts and ClaudeModelManifest.ts.
use serde_json::{Value, json};
use std::cmp::Ordering;
use turin_provider_core::semver::compare_semver_versions;

#[derive(Debug, Clone)]
pub struct ClaudeCatalogModel {
    pub model: Value,
    pub runtime: Value,
    pub compatibility: Value,
}
#[derive(Debug, Clone, Default)]
pub struct ClaudeModelCatalog {
    pub models: Vec<ClaudeCatalogModel>,
}
fn field<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    value.get(name).and_then(Value::as_str)
}
fn slug(entry: &ClaudeCatalogModel) -> Option<&str> {
    field(&entry.model, "slug")
}

pub fn resolve_claude_catalog_model<'a>(
    catalog: &'a ClaudeModelCatalog,
    slug_or_alias: Option<&str>,
) -> Option<&'a ClaudeCatalogModel> {
    let value = slug_or_alias?.trim();
    if value.is_empty() {
        return None;
    }
    catalog
        .models
        .iter()
        .find(|entry| slug(entry) == Some(value))
        .or_else(|| {
            catalog.models.iter().find(|entry| {
                entry
                    .model
                    .get("aliases")
                    .and_then(Value::as_array)
                    .is_some_and(|aliases| {
                        aliases.iter().any(|alias| {
                            alias
                                .as_str()
                                .is_some_and(|s| s.eq_ignore_ascii_case(value))
                        })
                    })
            })
        })
}
pub fn resolve_claude_model_slug(catalog: &ClaudeModelCatalog, slug_or_alias: &str) -> String {
    resolve_claude_catalog_model(catalog, Some(slug_or_alias))
        .and_then(slug)
        .unwrap_or(slug_or_alias)
        .to_owned()
}
pub fn get_claude_catalog_model_capabilities(
    catalog: &ClaudeModelCatalog,
    slug_or_alias: Option<&str>,
) -> Value {
    resolve_claude_catalog_model(catalog, slug_or_alias)
        .and_then(|e| e.model.get("capabilities"))
        .cloned()
        .unwrap_or_else(|| json!({"optionDescriptors":[]}))
}
fn supported(entry: &ClaudeCatalogModel, version: Option<&str>) -> bool {
    let min = field(&entry.compatibility, "minVersion");
    let max = field(&entry.compatibility, "maxVersionExclusive");
    if min.is_none() && max.is_none() {
        return true;
    }
    let Some(version) = version else {
        return false;
    };
    if min.is_some_and(|min| compare_semver_versions(version, min) == Ordering::Less) {
        return false;
    }
    !max.is_some_and(|max| compare_semver_versions(version, max) != Ordering::Less)
}
pub fn resolve_claude_models_for_version(
    catalog: &ClaudeModelCatalog,
    version: Option<&str>,
) -> Vec<Value> {
    catalog
        .models
        .iter()
        .filter(|e| supported(e, version))
        .map(|e| e.model.clone())
        .collect()
}
pub fn resolve_claude_update_required_models(
    catalog: &ClaudeModelCatalog,
    version: Option<&str>,
) -> Vec<Value> {
    let Some(version) = version else {
        return vec![];
    };
    catalog.models.iter().filter_map(|entry| {
        let min = field(&entry.compatibility,"minVersion")?;
        if entry.model.get("isLegacy").and_then(Value::as_bool) == Some(true) || compare_semver_versions(version,min) != Ordering::Less { return None; }
        let mut value = json!({"slug":entry.model.get("slug"),"name":entry.model.get("name"),"minVersion":min});
        if let Some(badge) = entry.model.get("badge") { value["badge"] = badge.clone(); }
        Some(value)
    }).collect()
}
pub fn format_claude_version_upgrade_message(
    catalog: &ClaudeModelCatalog,
    version: Option<&str>,
) -> Option<String> {
    let unavailable = catalog
        .models
        .iter()
        .filter(|e| {
            field(&e.compatibility, "minVersion").is_some_and(|min| {
                version.is_none_or(|v| compare_semver_versions(v, min) == Ordering::Less)
            })
        })
        .min_by(|a, b| {
            compare_semver_versions(
                field(&a.compatibility, "minVersion").unwrap(),
                field(&b.compatibility, "minVersion").unwrap(),
            )
        })?;
    let min = field(&unavailable.compatibility, "minVersion")?;
    let name = field(&unavailable.model, "name")?;
    let version_label = version
        .map(|v| format!("v{v}"))
        .unwrap_or_else(|| "the installed version".into());
    Some(format!(
        "Claude Code {version_label} is too old for {name}. Upgrade to v{min} or newer to access it."
    ))
}
pub fn normalize_claude_catalog_effort(
    catalog: &ClaudeModelCatalog,
    effort: Option<&str>,
    model: Option<&str>,
) -> Option<String> {
    let effort = effort.filter(|s| !s.is_empty())?;
    let mapping =
        resolve_claude_catalog_model(catalog, model).and_then(|e| e.runtime.get("effortMap"));
    match mapping.and_then(|m| m.get(effort)) {
        Some(Value::String(value)) => Some(value.clone()),
        Some(Value::Null) => None,
        _ => Some(effort.to_owned()),
    }
}
pub fn resolve_claude_catalog_api_model_id(
    catalog: &ClaudeModelCatalog,
    model: &str,
    options: &Value,
) -> String {
    let Some(entry) = resolve_claude_catalog_model(catalog, Some(model)) else {
        return model.into();
    };
    let slug = slug(entry).unwrap_or(model);
    if let Some(suffixes) = entry
        .runtime
        .get("modelSuffixes")
        .and_then(Value::as_object)
    {
        for (option_id, values) in suffixes {
            let selected = options.get(option_id).and_then(Value::as_str);
            if let Some(suffix) = selected
                .and_then(|selected| values.get(selected))
                .and_then(Value::as_str)
            {
                return format!("{slug}{suffix}");
            }
        }
    }
    slug.to_owned()
}
pub fn resolve_claude_catalog_context_window_tokens(
    catalog: &ClaudeModelCatalog,
    model: &str,
    context_window: Option<&str>,
) -> Option<f64> {
    let entry = resolve_claude_catalog_model(catalog, Some(model))?;
    entry
        .runtime
        .get("fixedContextWindowTokens")
        .and_then(Value::as_f64)
        .filter(|n| *n != 0.0)
        .or_else(|| {
            context_window.and_then(|w| entry.runtime.get("contextWindowTokens")?.get(w)?.as_f64())
        })
}
/// Validate Claude-specific manifest adapters before accepting an updated
/// manifest. Unknown fields remain legal; known fields keep upstream types.
pub fn has_valid_claude_manifest_adapters(manifest: &Value) -> bool {
    let Some(provider) = manifest.get("providers").and_then(|p| p.get("claudeAgent")) else {
        return true;
    };
    let profiles = provider.get("profiles").and_then(Value::as_object);
    let models = provider.get("models").and_then(Value::as_array);
    profiles.is_some_and(|p| p.values().all(|e| valid_profile_adapter(e.get("adapter"))))
        && models.is_some_and(|m| m.iter().all(|e| valid_model_adapter(e.get("adapter"))))
}
fn valid_profile_adapter(adapter: Option<&Value>) -> bool {
    let Some(adapter) = adapter else {
        return true;
    };
    let Some(map) = adapter.as_object() else {
        return false;
    };
    let Some(code) = map.get("claudeCode") else {
        return true;
    };
    let Some(code) = code.as_object() else {
        return false;
    };
    ["fixedContextWindowTokens"]
        .iter()
        .all(|k| code.get(*k).is_none_or(Value::is_number))
        && ["effortMap", "modelSuffixes", "contextWindowTokens"]
            .iter()
            .all(|k| code.get(*k).is_none_or(Value::is_object))
}
fn valid_model_adapter(adapter: Option<&Value>) -> bool {
    let Some(adapter) = adapter else {
        return true;
    };
    let Some(map) = adapter.as_object() else {
        return false;
    };
    let Some(code) = map.get("claudeCode") else {
        return true;
    };
    let Some(code) = code.as_object() else {
        return false;
    };
    let min = code.get("minVersion").and_then(Value::as_str);
    let max = code.get("maxVersionExclusive").and_then(Value::as_str);
    if code.get("minVersion").is_some() && min.is_none()
        || code.get("maxVersionExclusive").is_some() && max.is_none()
    {
        return false;
    }
    !min.zip(max)
        .is_some_and(|(min, max)| compare_semver_versions(min, max) != Ordering::Less)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_versions_and_effort() {
        let catalog = ClaudeModelCatalog {
            models: vec![ClaudeCatalogModel {
                model: json!({"slug":"claude-sonnet","name":"Sonnet","aliases":["sonnet"]}),
                runtime: json!({"effortMap":{"x":"high"},"modelSuffixes":{"contextWindow":{"long":"[1m]"}}}),
                compatibility: json!({"minVersion":"2.1.0"}),
            }],
        };
        assert_eq!(
            resolve_claude_model_slug(&catalog, "SONNET"),
            "claude-sonnet"
        );
        assert!(resolve_claude_models_for_version(&catalog, Some("2.0.0")).is_empty());
        assert_eq!(
            normalize_claude_catalog_effort(&catalog, Some("x"), Some("sonnet")),
            Some("high".into())
        );
        assert_eq!(
            resolve_claude_catalog_api_model_id(
                &catalog,
                "sonnet",
                &json!({"contextWindow":"long"})
            ),
            "claude-sonnet[1m]"
        );
        assert!(
            format_claude_version_upgrade_message(&catalog, Some("2.0.0"))
                .unwrap()
                .contains("v2.1.0")
        );
    }
}
