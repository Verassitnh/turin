//! Port of `readCustomModelEntries` (upstream/packages/shared/src/model.ts) and
//! `providerModelsFromSettings` (upstream/packages/provider-core/src/server/snapshotProbe.ts).
//!
//! Serde already decoded each setting's shape (bare slug or entry), so the
//! slug and name normalization is the only work left here.
use std::collections::{HashMap, HashSet};

use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::model::{CustomModelSetting, ModelCapabilities};

use crate::server_provider::ServerProviderModel;

/// `CustomModelDefinition`: a deduplicated, normalized custom model setting.
#[derive(Debug, Clone, PartialEq)]
pub struct CustomModelDefinition {
    pub slug: String,
    pub name: String,
    pub capabilities: Option<ModelCapabilities>,
}

/// `normalizeCustomModelSlug`: trimmed, or `None` when blank.
fn normalize_custom_model_slug(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// `readCustomModelEntries`: first occurrence of each slug wins, the name
/// falls back to the slug, and declared capabilities always carry an
/// `optionDescriptors` array (possibly empty) as `createModelCapabilities` does.
pub fn read_custom_model_entries(settings: &[CustomModelSetting]) -> Vec<CustomModelDefinition> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for setting in settings {
        let (slug, name, capabilities) = match setting {
            CustomModelSetting::Slug(slug) => (slug.as_str(), None, None),
            CustomModelSetting::Entry(entry) => (
                entry.slug.as_str(),
                entry.name.as_ref().map(TrimmedNonEmptyString::as_str),
                entry.capabilities.as_ref(),
            ),
        };
        let Some(slug) = normalize_custom_model_slug(slug) else {
            continue;
        };
        if !seen.insert(slug.clone()) {
            continue;
        }
        let name = name
            .and_then(normalize_custom_model_slug)
            .unwrap_or_else(|| slug.clone());
        entries.push(CustomModelDefinition {
            slug,
            name,
            capabilities: capabilities.map(|capabilities| ModelCapabilities {
                option_descriptors: Some(
                    capabilities.option_descriptors.clone().unwrap_or_default(),
                ),
            }),
        });
    }
    entries
}

/// `providerModelsFromSettings`: appends the user's custom models after the
/// built-ins. A named entry whose slug matches a built-in renames that
/// built-in (and drops its short name) instead of duplicating it.
pub fn provider_models_from_settings(
    built_in_models: &[ServerProviderModel],
    custom_models: &[CustomModelSetting],
    custom_model_capabilities: Option<&ModelCapabilities>,
) -> Vec<ServerProviderModel> {
    let entries = read_custom_model_entries(custom_models);
    if entries.is_empty() {
        return built_in_models.to_vec();
    }

    let name_overrides: HashMap<&str, &str> = entries
        .iter()
        .filter(|entry| entry.name != entry.slug)
        .map(|entry| (entry.slug.as_str(), entry.name.as_str()))
        .collect();
    let mut models: Vec<ServerProviderModel> = built_in_models
        .iter()
        .map(|model| match name_overrides.get(model.slug.as_str()) {
            Some(name) => ServerProviderModel {
                // The built-in short name would undo the override in compact UI.
                short_name: None,
                name: TrimmedNonEmptyString::new(name).unwrap_or_else(|_| model.name.clone()),
                ..model.clone()
            },
            None => model.clone(),
        })
        .collect();

    let mut seen: HashSet<String> = models
        .iter()
        .map(|model| model.slug.as_str().to_owned())
        .collect();
    for entry in entries {
        if !seen.insert(entry.slug.clone()) {
            continue;
        }
        let (Ok(slug), Ok(name)) = (
            TrimmedNonEmptyString::new(&entry.slug),
            TrimmedNonEmptyString::new(&entry.name),
        ) else {
            continue;
        };
        models.push(ServerProviderModel {
            slug,
            name,
            short_name: None,
            sub_provider: None,
            aliases: None,
            badge: None,
            is_custom: true,
            is_default: None,
            is_legacy: None,
            capabilities: entry
                .capabilities
                .or_else(|| custom_model_capabilities.cloned()),
        });
    }
    models
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::model::{CustomModelEntry, ProviderOptionDescriptor};

    fn built_in(slug: &str, name: &str) -> ServerProviderModel {
        ServerProviderModel {
            slug: TrimmedNonEmptyString::new(slug).unwrap(),
            name: TrimmedNonEmptyString::new(name).unwrap(),
            short_name: TrimmedNonEmptyString::new("short").ok(),
            sub_provider: None,
            aliases: None,
            badge: None,
            is_custom: false,
            is_default: None,
            is_legacy: None,
            capabilities: None,
        }
    }

    fn entry(slug: &str, name: Option<&str>) -> CustomModelSetting {
        CustomModelSetting::Entry(CustomModelEntry {
            slug: TrimmedNonEmptyString::new(slug).unwrap(),
            name: name.and_then(|name| TrimmedNonEmptyString::new(name).ok()),
            capabilities: None,
        })
    }

    #[test]
    fn reads_slugs_trims_and_deduplicates() {
        let entries = read_custom_model_entries(&[
            CustomModelSetting::Slug("  gpt-a  ".to_owned()),
            CustomModelSetting::Slug("gpt-a".to_owned()),
            CustomModelSetting::Slug("   ".to_owned()),
            entry("gpt-b", Some("Bee")),
        ]);
        assert_eq!(
            entries,
            vec![
                CustomModelDefinition {
                    slug: "gpt-a".to_owned(),
                    name: "gpt-a".to_owned(),
                    capabilities: None,
                },
                CustomModelDefinition {
                    slug: "gpt-b".to_owned(),
                    name: "Bee".to_owned(),
                    capabilities: None,
                },
            ]
        );
    }

    #[test]
    fn declared_capabilities_always_carry_option_descriptors() {
        let entries = read_custom_model_entries(&[CustomModelSetting::Entry(CustomModelEntry {
            slug: TrimmedNonEmptyString::new("gpt-c").unwrap(),
            name: None,
            capabilities: Some(ModelCapabilities {
                option_descriptors: None,
            }),
        })]);
        assert_eq!(
            entries[0].capabilities,
            Some(ModelCapabilities {
                option_descriptors: Some(Vec::new()),
            })
        );
    }

    #[test]
    fn named_entry_renames_matching_built_in_and_drops_short_name() {
        let models = provider_models_from_settings(
            &[built_in("gpt-x", "GPT X")],
            &[entry("gpt-x", Some("Renamed"))],
            None,
        );
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name.as_str(), "Renamed");
        assert!(models[0].short_name.is_none());
        assert!(!models[0].is_custom);
    }

    #[test]
    fn bare_custom_slug_borrows_fallback_capabilities() {
        let fallback = ModelCapabilities {
            option_descriptors: Some(Vec::<ProviderOptionDescriptor>::new()),
        };
        let models = provider_models_from_settings(
            &[built_in("gpt-x", "GPT X")],
            &[
                CustomModelSetting::Slug("gpt-y".to_owned()),
                CustomModelSetting::Slug("gpt-x".to_owned()),
            ],
            Some(&fallback),
        );
        let slugs: Vec<&str> = models.iter().map(|model| model.slug.as_str()).collect();
        assert_eq!(slugs, vec!["gpt-x", "gpt-y"]);
        assert!(models[1].is_custom);
        assert_eq!(models[1].capabilities, Some(fallback));
    }

    #[test]
    fn no_custom_models_returns_built_ins_unchanged() {
        let built = vec![built_in("gpt-x", "GPT X")];
        assert_eq!(provider_models_from_settings(&built, &[], None), built);
    }
}
