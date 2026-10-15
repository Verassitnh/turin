//! Port of upstream/apps/server/src/claudeModelOptions.ts.
use crate::claude_model_catalog::{
    ClaudeModelCatalog, get_claude_catalog_model_capabilities, normalize_claude_catalog_effort,
    resolve_claude_catalog_api_model_id,
};
use serde_json::{Map, Value, json};
use turin_contracts::model_selection::ModelSelection;

#[derive(Debug, Clone, PartialEq)]
pub struct CompiledClaudeModelSelection {
    pub api_model_id: String,
    pub effort: Option<String>,
    pub prompt_effort: Option<String>,
    pub settings: Value,
    pub query_identity: String,
}

fn option(selection: &ModelSelection, id: &str) -> Option<Value> {
    selection
        .options
        .as_ref()?
        .iter()
        .find(|entry| entry.id.as_str() == id)
        .and_then(|entry| serde_json::to_value(&entry.value).ok())
}
fn descriptor<'a>(caps: &'a Value, id: &str) -> Option<&'a Value> {
    caps.get("optionDescriptors")?
        .as_array()?
        .iter()
        .find(|d| d.get("id").and_then(Value::as_str) == Some(id))
}
fn select_value(caps: &Value, id: &str, raw: Option<&str>) -> Option<String> {
    let descriptor = descriptor(caps, id)?;
    let options = descriptor.get("options").and_then(Value::as_array)?;
    let default = || {
        options
            .iter()
            .find(|v| v.get("isDefault").and_then(Value::as_bool) == Some(true))
            .and_then(|v| v.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let current = descriptor.get("currentValue").and_then(Value::as_str);
    let raw = raw.or(current).map(str::trim).filter(|s| !s.is_empty());
    let Some(raw) = raw else {
        return default();
    };
    if options.is_empty() {
        return Some(raw.into());
    }
    let valid = options
        .iter()
        .any(|v| v.get("id").and_then(Value::as_str) == Some(raw));
    if !valid {
        return current.map(str::to_owned).or_else(default);
    }
    if descriptor
        .get("promptInjectedValues")
        .and_then(Value::as_array)
        .is_some_and(|values| values.iter().any(|v| v.as_str() == Some(raw)))
    {
        return default();
    }
    Some(raw.into())
}

pub fn compile_claude_model_selection(
    selection: &ModelSelection,
    catalog: &ClaudeModelCatalog,
) -> CompiledClaudeModelSelection {
    let model = selection.model.as_str();
    let caps = serde_json::to_value(get_claude_catalog_model_capabilities(catalog, Some(model)))
        .unwrap_or_else(|_| json!({}));
    let raw_effort = option(selection, "effort").and_then(|v| v.as_str().map(str::to_owned));
    let resolved_effort = select_value(&caps, "effort", raw_effort.as_deref());
    let effort = normalize_claude_catalog_effort(catalog, resolved_effort.as_deref(), Some(model));
    let prompt_effort = raw_effort
        .as_deref()
        .filter(|raw| {
            caps.get("optionDescriptors")
                .and_then(Value::as_array)
                .is_some_and(|descriptors| {
                    descriptors.iter().any(|descriptor| {
                        descriptor.get("type").and_then(Value::as_str) == Some("select")
                            && descriptor
                                .get("promptInjectedValues")
                                .and_then(Value::as_array)
                                .is_some_and(|values| {
                                    values.iter().any(|v| v.as_str() == Some(raw))
                                })
                    })
                })
        })
        .map(str::to_owned);
    let mut settings = Map::new();
    for (option_id, setting_id, default_false) in [
        ("thinking", "alwaysThinkingEnabled", false),
        ("fastMode", "fastMode", true),
    ] {
        if descriptor(&caps, option_id)
            .and_then(|d| d.get("type"))
            .and_then(Value::as_str)
            == Some("boolean")
        {
            let selected = option(selection, option_id).and_then(|v| v.as_bool());
            let selected = selected.or_else(|| {
                descriptor(&caps, option_id)
                    .and_then(|d| d.get("currentValue"))
                    .and_then(Value::as_bool)
            });
            if let Some(selected) = selected.or(default_false.then_some(false)) {
                settings.insert(setting_id.into(), json!(selected));
            }
        }
    }
    if resolved_effort.as_deref() == Some("ultracode") {
        settings.insert("ultracode".into(), json!(true));
    }
    let api_model_id = resolve_claude_catalog_api_model_id(catalog, selection);
    let settings = Value::Object(settings);
    let query_identity = format!(
        "{{\"apiModelId\":{},\"effort\":{},\"settings\":{}}}",
        json!(api_model_id),
        json!(effort),
        settings
    );
    CompiledClaudeModelSelection {
        api_model_id,
        effort,
        prompt_effort,
        settings,
        query_identity,
    }
}

/// Keep Claude slash commands intact. Prefix only prose when the selected
/// prompt-injected effort is `ultrathink`.
pub fn apply_claude_prompt_effort_prefix(text: &str, effort: Option<&str>) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() || effort != Some("ultrathink") || trimmed.starts_with("Ultrathink:") {
        return trimmed.into();
    }
    let first = trimmed.split_whitespace().next().unwrap_or("");
    if first.starts_with('/') && first.len() > 1 && !first[1..].contains('/') {
        return trimmed.into();
    }
    format!("Ultrathink:\n{trimmed}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_model_catalog::ClaudeCatalogModel;
    use crate::claude_model_manifest::ClaudeCodeCompatibility;
    #[test]
    fn compiles_selection_defaults_and_prompt_effort() {
        let catalog = ClaudeModelCatalog {
            models: vec![ClaudeCatalogModel {
                model: serde_json::from_value(json!({
                    "slug": "sonnet",
                    "name": "Sonnet",
                    "isCustom": false,
                    "capabilities": {"optionDescriptors": [
                        {"id": "effort", "label": "Reasoning", "type": "select",
                         "options": [
                            {"id": "default", "label": "Default", "isDefault": true},
                            {"id": "ultrathink", "label": "Ultrathink"}
                         ],
                         "promptInjectedValues": ["ultrathink"]},
                        {"id": "fastMode", "label": "Fast Mode", "type": "boolean"}
                    ]}
                }))
                .unwrap(),
                runtime: Default::default(),
                compatibility: ClaudeCodeCompatibility::default(),
            }],
        };
        let selection: ModelSelection = serde_json::from_value(json!({"instanceId":"claudeAgent","model":"sonnet","options":[{"id":"effort","value":"ultrathink"}]})).unwrap();
        let compiled = compile_claude_model_selection(&selection, &catalog);
        assert_eq!(compiled.effort.as_deref(), Some("default"));
        assert_eq!(compiled.prompt_effort.as_deref(), Some("ultrathink"));
        assert_eq!(compiled.settings["fastMode"], false);
        assert_eq!(
            apply_claude_prompt_effort_prefix("hello", compiled.prompt_effort.as_deref()),
            "Ultrathink:\nhello"
        );
        assert_eq!(
            apply_claude_prompt_effort_prefix(
                "/deploy.prod now",
                compiled.prompt_effort.as_deref()
            ),
            "/deploy.prod now"
        );
    }
}
