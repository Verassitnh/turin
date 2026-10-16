//! Codex model list mapping: `parseCodexModelListResponse`,
//! `mapCodexModelCapabilities`, `applyPreferredCodexDefaultModel`, and
//! `appendCustomCodexModels` from upstream/apps/server/src/provider/CodexProvider.ts,
//! `codexModelFamily` and `formatCodexModelName` (upstream/packages/shared/src/model.ts)
//! live in `turin_shared::model`.
use serde_json::Value;
use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::model::{
    CustomModelSetting, ModelCapabilities, PREFERRED_DEFAULT_CODEX_MODELS, ProviderOptionChoice,
    ProviderOptionDescriptor, SelectOptionType, SelectProviderOptionDescriptor,
};
use turin_provider_core::custom_models::provider_models_from_settings;
use turin_provider_core::server_provider::ServerProviderModel;
use turin_shared::model::{codex_model_family, format_codex_model_name};

/// `DEFAULT_SERVICE_TIER_ID`.
const DEFAULT_SERVICE_TIER_ID: &str = "default";

/// Shorter copy for tiers whose catalog description wraps in the traits menu.
fn service_tier_description(id: &str) -> Option<&'static str> {
    match id {
        "ultrafast" => Some("Even faster, more expensive"),
        _ => None,
    }
}

/// `REASONING_EFFORT_LABELS`; unknown efforts show their raw id.
fn reasoning_effort_label(reasoning_effort: &str) -> &str {
    match reasoning_effort {
        "none" => "None",
        "minimal" => "Minimal",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra High",
        "max" => "Max",
        "ultra" => "Ultra",
        other => other,
    }
}

fn tne(value: &str) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

/// A label that falls back to the id when it is blank.
fn label_or(label: &str, id: &TrimmedNonEmptyString) -> TrimmedNonEmptyString {
    tne(label).unwrap_or_else(|| id.clone())
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// `mapCodexModelCapabilities`: reasoning-effort and service-tier selects.
/// Astra models always default to `medium` reasoning.
pub fn map_codex_model_capabilities(model: &Value) -> ModelCapabilities {
    let model_id = str_field(model, "model").unwrap_or_default();
    let default_reasoning_effort = if codex_model_family(model_id) == "gpt-6-astra" {
        Some("medium")
    } else {
        str_field(model, "defaultReasoningEffort")
    };

    let reasoning_options: Vec<ProviderOptionChoice> = model
        .get("supportedReasoningEfforts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let effort = str_field(entry, "reasoningEffort")?;
            let id = tne(effort)?;
            Some(ProviderOptionChoice {
                label: label_or(reasoning_effort_label(effort), &id),
                id,
                description: None,
                is_default: (Some(effort) == default_reasoning_effort).then_some(true),
            })
        })
        .collect();
    let default_reasoning = reasoning_options
        .iter()
        .find(|option| option.is_default == Some(true))
        .map(|option| option.id.clone());

    // (id, display name, description); the listed tiers win over the legacy
    // `additionalSpeedTiers` slugs.
    let service_tiers: Vec<(TrimmedNonEmptyString, String, Option<String>)> = {
        let listed = model
            .get("serviceTiers")
            .and_then(Value::as_array)
            .filter(|tiers| !tiers.is_empty());
        let tiers: Vec<(&str, &str, Option<&str>)> = match listed {
            Some(tiers) => tiers
                .iter()
                .filter_map(|tier| {
                    let id = str_field(tier, "id")?;
                    let name = str_field(tier, "name").unwrap_or(id);
                    Some((id, name, str_field(tier, "description")))
                })
                .collect(),
            None => model
                .get("additionalSpeedTiers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|id| (id, if id == "fast" { "Fast" } else { id }, Some("")))
                .collect(),
        };
        tiers
            .into_iter()
            .filter_map(|(id, name, description)| {
                Some((tne(id)?, name.to_owned(), description.map(str::to_owned)))
            })
            .collect()
    };
    let default_service_tier = str_field(model, "defaultServiceTier")
        .filter(|default| {
            service_tiers
                .iter()
                .any(|(id, _, _)| id.as_str() == *default)
        })
        .unwrap_or(DEFAULT_SERVICE_TIER_ID);

    let mut option_descriptors = Vec::new();
    if !reasoning_options.is_empty() {
        option_descriptors.push(ProviderOptionDescriptor::Select(
            SelectProviderOptionDescriptor {
                id: tne("reasoningEffort").expect("static id"),
                label: tne("Reasoning").expect("static label"),
                description: None,
                type_: SelectOptionType,
                options: reasoning_options,
                current_value: default_reasoning,
                prompt_injected_values: None,
            },
        ));
    }
    if !service_tiers.is_empty() {
        let mut options = vec![ProviderOptionChoice {
            id: tne(DEFAULT_SERVICE_TIER_ID).expect("static id"),
            label: tne("Standard").expect("static label"),
            description: None,
            is_default: (default_service_tier == DEFAULT_SERVICE_TIER_ID).then_some(true),
        }];
        options.extend(service_tiers.iter().map(|(id, name, description)| {
            let description = service_tier_description(id.as_str())
                .map(str::to_owned)
                .or_else(|| description.clone())
                .filter(|description| !description.is_empty())
                .and_then(|description| tne(&description));
            ProviderOptionChoice {
                label: label_or(name, id),
                id: id.clone(),
                description,
                is_default: (default_service_tier == id.as_str()).then_some(true),
            }
        }));
        option_descriptors.push(ProviderOptionDescriptor::Select(
            SelectProviderOptionDescriptor {
                id: tne("serviceTier").expect("static id"),
                label: tne("Service Tier").expect("static label"),
                description: None,
                type_: SelectOptionType,
                options,
                current_value: tne(default_service_tier),
                prompt_injected_values: None,
            },
        ));
    }

    ModelCapabilities {
        option_descriptors: Some(option_descriptors),
    }
}

/// One `model/list` entry as a built-in provider model. Entries without a
/// `model` slug are skipped; upstream's schema would reject them.
pub fn parse_codex_model(model: &Value) -> Option<ServerProviderModel> {
    let slug = str_field(model, "model")?;
    let name = format_codex_model_name(str_field(model, "displayName").unwrap_or(slug));
    Some(ServerProviderModel {
        slug: tne(slug)?,
        name: tne(&name)?,
        short_name: None,
        sub_provider: None,
        aliases: None,
        badge: None,
        is_custom: false,
        is_default: (model.get("isDefault").and_then(Value::as_bool) == Some(true)).then_some(true),
        is_legacy: None,
        capabilities: Some(map_codex_model_capabilities(model)),
    })
}

/// `applyPreferredCodexDefaultModel`: the first preferred family present in
/// the live catalog becomes the default; otherwise Codex's own flag stands.
pub fn apply_preferred_codex_default_model(
    models: Vec<ServerProviderModel>,
) -> Vec<ServerProviderModel> {
    let preferred_slug = PREFERRED_DEFAULT_CODEX_MODELS.iter().find_map(|family| {
        models
            .iter()
            .find(|model| !model.is_custom && codex_model_family(model.slug.as_str()) == *family)
            .map(|model| model.slug.clone())
    });
    let Some(preferred_slug) = preferred_slug else {
        return models;
    };
    models
        .into_iter()
        .map(|model| {
            if model.slug == preferred_slug {
                ServerProviderModel {
                    is_default: Some(true),
                    ..model
                }
            } else if model.is_default.is_some() {
                ServerProviderModel {
                    is_default: None,
                    ..model
                }
            } else {
                model
            }
        })
        .collect()
}

/// `appendCustomCodexModels`: a bare custom slug borrows the first built-in's
/// capabilities; an entry with its own capabilities keeps them.
pub fn append_custom_codex_models(
    models: &[ServerProviderModel],
    custom_models: &[CustomModelSetting],
) -> Vec<ServerProviderModel> {
    let fallback_capabilities = models.iter().find_map(|model| model.capabilities.clone());
    provider_models_from_settings(models, custom_models, fallback_capabilities.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn listed(slug: &str, is_default: bool) -> ServerProviderModel {
        ServerProviderModel {
            slug: tne(slug).unwrap(),
            name: tne(slug).unwrap(),
            short_name: None,
            sub_provider: None,
            aliases: None,
            badge: None,
            is_custom: false,
            is_default: is_default.then_some(true),
            is_legacy: None,
            capabilities: None,
        }
    }

    #[test]
    fn reasoning_defaults_come_from_the_model_except_astra() {
        let model = json!({
            "model": "gpt-x",
            "defaultReasoningEffort": "high",
            "supportedReasoningEfforts": [
                {"reasoningEffort": "low"},
                {"reasoningEffort": "high"},
                {"reasoningEffort": "xhigh"}
            ]
        });
        let caps = map_codex_model_capabilities(&model);
        let descriptors = caps.option_descriptors.unwrap();
        let ProviderOptionDescriptor::Select(reasoning) = &descriptors[0] else {
            panic!("reasoning select first");
        };
        assert_eq!(
            reasoning.current_value.as_ref().map(|v| v.as_str()),
            Some("high")
        );
        assert_eq!(reasoning.options[2].label.as_str(), "Extra High");

        let astra = json!({
            "model": "openai.gpt-6-astra",
            "defaultReasoningEffort": "high",
            "supportedReasoningEfforts": [
                {"reasoningEffort": "medium"},
                {"reasoningEffort": "high"}
            ]
        });
        let caps = map_codex_model_capabilities(&astra);
        let ProviderOptionDescriptor::Select(reasoning) = &caps.option_descriptors.unwrap()[0]
        else {
            panic!("reasoning select first");
        };
        assert_eq!(
            reasoning.current_value.as_ref().map(|v| v.as_str()),
            Some("medium")
        );
    }

    #[test]
    fn service_tiers_add_standard_and_describe_known_tiers() {
        let model = json!({
            "model": "gpt-x",
            "defaultServiceTier": "ultrafast",
            "serviceTiers": [
                {"id": "ultrafast", "name": "Ultra Fast", "description": "long copy"},
                {"id": "flex", "name": "Flex", "description": ""}
            ]
        });
        let caps = map_codex_model_capabilities(&model);
        let descriptors = caps.option_descriptors.unwrap();
        assert_eq!(descriptors.len(), 1, "no reasoning select without efforts");
        let ProviderOptionDescriptor::Select(tiers) = &descriptors[0] else {
            panic!("service tier select");
        };
        assert_eq!(tiers.id.as_str(), "serviceTier");
        assert_eq!(
            tiers.current_value.as_ref().map(|v| v.as_str()),
            Some("ultrafast")
        );
        assert_eq!(tiers.options[0].id.as_str(), "default");
        assert_eq!(tiers.options[0].label.as_str(), "Standard");
        assert_eq!(
            tiers.options[1].description.as_ref().map(|v| v.as_str()),
            Some("Even faster, more expensive")
        );
        assert_eq!(tiers.options[1].is_default, Some(true));
        assert!(tiers.options[2].description.is_none());
    }

    #[test]
    fn additional_speed_tiers_name_fast_and_fall_back_to_default() {
        let model = json!({
            "model": "gpt-x",
            "defaultServiceTier": "missing",
            "additionalSpeedTiers": ["fast"]
        });
        let caps = map_codex_model_capabilities(&model);
        let ProviderOptionDescriptor::Select(tiers) = &caps.option_descriptors.unwrap()[0] else {
            panic!("service tier select");
        };
        assert_eq!(
            tiers.current_value.as_ref().map(|v| v.as_str()),
            Some("default")
        );
        assert_eq!(tiers.options[0].is_default, Some(true));
        assert_eq!(tiers.options[1].label.as_str(), "Fast");
    }

    #[test]
    fn parse_skips_missing_slug_and_formats_name() {
        let parsed = parse_codex_model(&json!({
            "model": "gpt-5.4", "displayName": "gpt-5.4-mini", "isDefault": true
        }))
        .unwrap();
        assert_eq!(parsed.name.as_str(), "GPT-5.4-Mini");
        assert_eq!(parsed.is_default, Some(true));
        assert!(parsed.capabilities.is_some());
        assert!(parse_codex_model(&json!({"displayName": "no slug"})).is_none());
    }

    #[test]
    fn preferred_default_wins_over_codex_flag() {
        let models = apply_preferred_codex_default_model(vec![
            listed("gpt-old", true),
            listed("openai.gpt-6-astra", false),
            listed("gpt-5.6-sol", false),
        ]);
        let defaults: Vec<(&str, Option<bool>)> = models
            .iter()
            .map(|model| (model.slug.as_str(), model.is_default))
            .collect();
        assert_eq!(
            defaults,
            vec![
                ("gpt-old", None),
                ("openai.gpt-6-astra", Some(true)),
                ("gpt-5.6-sol", None),
            ]
        );
    }

    #[test]
    fn codex_flag_stands_without_a_preferred_model() {
        let models = apply_preferred_codex_default_model(vec![listed("gpt-old", true)]);
        assert_eq!(models[0].is_default, Some(true));
    }

    #[test]
    fn custom_models_append_with_first_built_in_capabilities() {
        let mut built_in = listed("gpt-a", false);
        built_in.capabilities = Some(ModelCapabilities {
            option_descriptors: Some(Vec::new()),
        });
        let models = append_custom_codex_models(
            &[built_in],
            &[CustomModelSetting::Slug("gpt-custom".to_owned())],
        );
        assert_eq!(models.len(), 2);
        assert!(models[1].is_custom);
        assert_eq!(models[1].capabilities, models[0].capabilities);
    }
}
