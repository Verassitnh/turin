//! Cursor provider status and model catalog.
//!
//! Port of `upstream/packages/provider-cursor/src/server/status.ts`. Upstream
//! reads the Cursor SDK catalog; Turin reads the ACP probe in [`crate::probe`]:
//! `session/list` answers whether the agent is signed in, and
//! `cursor/list_available_models` is the model catalog. Built-in models carry
//! their parameters as option descriptors, as upstream's SDK models do.

use std::time::Duration;

use turin_acp::cursor::{AvailableModel, ModelConfigOption};
use turin_contracts::model::{
    BooleanOptionType, BooleanProviderOptionDescriptor, CustomModelSetting, ModelCapabilities,
    ProviderOptionChoice, ProviderOptionDescriptor, SelectOptionType,
    SelectProviderOptionDescriptor,
};
use turin_contracts::provider_instance::ProviderDriverKind;
use turin_contracts::{IsoDateTime, TrimmedNonEmptyString};
use turin_provider_core::server_provider::{
    ServerProviderAuth, ServerProviderAuthStatus, ServerProviderDraft, ServerProviderModel,
    ServerProviderState,
};
use turin_provider_core::snapshot_probe::{
    BuildServerProviderInput, ProviderProbeResult, ServerProviderPresentation,
    build_server_provider,
};

use crate::settings::CursorSettings;

/// Upstream's `CURSOR_SDK_CATALOG_TIMEOUT_MS`. The whole probe runs under it.
pub const CURSOR_PROBE_TIMEOUT: Duration = Duration::from_millis(15_000);

/// Result of one ACP probe. Built by [`crate::probe::probe_status`].
#[derive(Debug, Clone, PartialEq)]
pub struct CursorProbe {
    pub installed: bool,
    pub version: Option<String>,
    pub status: ServerProviderState,
    pub auth: ServerProviderAuth,
    pub message: Option<String>,
    pub models: Vec<AvailableModel>,
}

pub fn presentation(show_interaction_mode_toggle: bool) -> ServerProviderPresentation {
    ServerProviderPresentation {
        display_name: "Cursor".to_owned(),
        badge_label: None,
        show_interaction_mode_toggle: Some(show_interaction_mode_toggle),
        reports_context_window: Some(false),
        supported_runtime_modes: None,
        requires_new_thread_for_model_change: Some(false),
        supports_conversation_rollback: Some(false),
    }
}

/// Snapshot before the first probe. Upstream `buildInitialCursorProviderSnapshot`.
pub fn initial_draft(
    driver: &ProviderDriverKind,
    settings: &CursorSettings,
    enabled: bool,
    checked_at: IsoDateTime,
) -> ServerProviderDraft {
    let models = provider_models_from_settings(&[], &settings.custom_models);
    let (installed, message) = if enabled {
        (true, "Checking Cursor agent availability...")
    } else {
        (false, "Cursor is disabled in T3 Code settings.")
    };
    let mut draft = build_server_provider(BuildServerProviderInput {
        driver: Some(driver),
        presentation: presentation(true),
        enabled,
        checked_at,
        models,
        update_required_models: Vec::new(),
        slash_commands: Vec::new(),
        skills: Vec::new(),
        probe: ProviderProbeResult {
            installed,
            version: None,
            status: ServerProviderState::Warning,
            auth: ServerProviderAuth::with_status(ServerProviderAuthStatus::Unknown),
            message: Some(message.to_owned()),
            usage_limits: None,
        },
        maintenance: None,
    });
    draft.supports_text_generation = Some(false);
    draft
}

/// Snapshot for a probe that ran. Upstream `buildCursorProviderSnapshot`, with
/// its discovery warning folded into the message.
pub fn draft_from_probe(
    driver: &ProviderDriverKind,
    enabled: bool,
    settings: &CursorSettings,
    checked_at: IsoDateTime,
    probe: CursorProbe,
    discovery_warning: Option<String>,
) -> ServerProviderDraft {
    let models =
        provider_models_from_settings(&catalog_models(&probe.models), &settings.custom_models);
    let status = match (&discovery_warning, probe.status) {
        (Some(_), ServerProviderState::Ready) => ServerProviderState::Warning,
        (_, status) => status,
    };
    let message = [probe.message, discovery_warning]
        .into_iter()
        .flatten()
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut draft = build_server_provider(BuildServerProviderInput {
        driver: Some(driver),
        presentation: presentation(true),
        enabled,
        checked_at,
        models,
        update_required_models: Vec::new(),
        slash_commands: Vec::new(),
        skills: Vec::new(),
        probe: ProviderProbeResult {
            installed: probe.installed,
            version: probe.version,
            status,
            auth: probe.auth,
            message: (!message.is_empty()).then_some(message),
            usage_limits: None,
        },
        maintenance: None,
    });
    draft.supports_text_generation = Some(false);
    draft
}

/// Upstream `buildCursorDiscoveredModelsFromSdk`: one model per slug, with
/// capabilities from the model's parameters.
pub fn catalog_models(models: &[AvailableModel]) -> Vec<ServerProviderModel> {
    let mut seen = std::collections::HashSet::new();
    models
        .iter()
        .filter_map(|model| {
            let slug = model.value.trim();
            let name = model.name.trim();
            if slug.is_empty() || name.is_empty() || !seen.insert(slug.to_owned()) {
                return None;
            }
            Some(ServerProviderModel {
                slug: tne(slug)?,
                name: tne(name)?,
                short_name: None,
                sub_provider: None,
                aliases: None,
                badge: None,
                is_custom: false,
                is_default: None,
                is_legacy: None,
                capabilities: Some(capabilities_from_parameters(&model.config_options)),
            })
        })
        .collect()
}

/// Option descriptors from a model's ACP config options. The `model` selector
/// itself is not a parameter; it is chosen from the list, not through options.
pub fn capabilities_from_parameters(options: &[ModelConfigOption]) -> ModelCapabilities {
    let mut seen = std::collections::HashSet::new();
    let descriptors = options
        .iter()
        .filter(|option| option.id.trim() != "model")
        .filter(|option| seen.insert(option.id.trim().to_owned()))
        .filter_map(descriptor_from_parameter)
        .collect();
    ModelCapabilities {
        option_descriptors: Some(descriptors),
    }
}

fn descriptor_from_parameter(option: &ModelConfigOption) -> Option<ProviderOptionDescriptor> {
    let id = tne(option.id.trim())?;
    let label = tne(option.name.trim()).unwrap_or_else(|| id.clone());
    let current = option.current_value.trim();
    let is_boolean = option.type_name == "boolean"
        || (option.options.len() == 2
            && option
                .options
                .iter()
                .all(|choice| matches!(choice.value.as_str(), "true" | "false")));
    if is_boolean {
        return Some(ProviderOptionDescriptor::Boolean(
            BooleanProviderOptionDescriptor {
                id,
                label,
                description: None,
                type_: BooleanOptionType,
                current_value: match current {
                    "true" => Some(true),
                    "false" => Some(false),
                    _ => None,
                },
            },
        ));
    }
    let options: Vec<ProviderOptionChoice> = option
        .options
        .iter()
        .filter_map(|choice| {
            let value = choice.value.trim();
            Some(ProviderOptionChoice {
                id: tne(value)?,
                label: tne(choice.name.trim()).or_else(|| tne(value))?,
                description: None,
                is_default: (value == current).then_some(true),
            })
        })
        .collect();
    if options.is_empty() {
        return None;
    }
    Some(ProviderOptionDescriptor::Select(
        SelectProviderOptionDescriptor {
            id,
            label,
            description: None,
            type_: SelectOptionType,
            options,
            current_value: tne(current),
            prompt_injected_values: None,
        },
    ))
}

/// Upstream `providerModelsFromSettings`: built-in models first, with custom
/// names applied, then the custom models the built-ins do not already cover.
pub fn provider_models_from_settings(
    built_in: &[ServerProviderModel],
    custom_models: &[CustomModelSetting],
) -> Vec<ServerProviderModel> {
    let entries = custom_entries(custom_models);
    if entries.is_empty() {
        return built_in.to_vec();
    }
    let mut models: Vec<ServerProviderModel> = built_in
        .iter()
        .map(|model| {
            match entries
                .iter()
                .find(|entry| entry.slug == model.slug.as_str() && entry.name != entry.slug)
            {
                Some(entry) => ServerProviderModel {
                    name: tne(&entry.name).unwrap_or_else(|| model.name.clone()),
                    // The built-in short name would undo the override in compact UI.
                    short_name: None,
                    ..model.clone()
                },
                None => model.clone(),
            }
        })
        .collect();
    let mut seen: std::collections::HashSet<String> = models
        .iter()
        .map(|model| model.slug.as_str().to_owned())
        .collect();
    for entry in entries {
        if !seen.insert(entry.slug.clone()) {
            continue;
        }
        let (Some(slug), Some(name)) = (tne(&entry.slug), tne(&entry.name)) else {
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
            capabilities: Some(entry.capabilities.unwrap_or(ModelCapabilities {
                option_descriptors: Some(Vec::new()),
            })),
        });
    }
    models
}

struct CustomEntry {
    slug: String,
    name: String,
    capabilities: Option<ModelCapabilities>,
}

fn custom_entries(custom_models: &[CustomModelSetting]) -> Vec<CustomEntry> {
    custom_models
        .iter()
        .filter_map(|setting| {
            let (slug, name, capabilities) = match setting {
                CustomModelSetting::Slug(slug) => (slug.trim().to_owned(), None, None),
                CustomModelSetting::Entry(entry) => (
                    entry.slug.as_str().to_owned(),
                    entry.name.as_ref().map(|name| name.as_str().to_owned()),
                    entry.capabilities.clone(),
                ),
            };
            if slug.is_empty() {
                return None;
            }
            let name = name
                .map(|name| name.trim().to_owned())
                .unwrap_or_else(|| slug.clone());
            Some(CustomEntry {
                slug,
                name,
                capabilities,
            })
        })
        .collect()
}

fn tne(value: &str) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

/// Auth from `session/list`. `None` means the session list worked.
pub fn auth_status_from_error(not_signed_in: bool) -> ServerProviderAuth {
    if not_signed_in {
        ServerProviderAuth::with_status(ServerProviderAuthStatus::Unauthenticated)
    } else {
        ServerProviderAuth::with_status(ServerProviderAuthStatus::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turin_acp::cursor::ListAvailableModelsResult;

    fn catalog(value: serde_json::Value) -> Vec<AvailableModel> {
        serde_json::from_value::<ListAvailableModelsResult>(value)
            .unwrap()
            .models
    }

    #[test]
    fn catalog_becomes_models_with_parameter_descriptors() {
        let models = catalog(json!({"models": [{
            "value": "gpt-5.5",
            "name": "GPT-5.5",
            "configOptions": [
                {"id": "model", "name": "Model", "type": "select", "currentValue": "gpt-5.5", "options": []},
                {"id": "reasoning", "name": "Reasoning", "type": "select", "currentValue": "high",
                 "options": [{"value": "low", "name": "Low"}, {"value": "high", "name": "High"}]},
                {"id": "fast", "name": "Fast", "type": "boolean", "currentValue": "true", "options": []},
                {"id": "empty", "name": "Empty", "type": "select", "currentValue": "x", "options": []}
            ]
        }, {"value": "gpt-5.5", "name": "Duplicate", "configOptions": []}]}));
        let models = catalog_models(&models);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].slug.as_str(), "gpt-5.5");
        assert!(!models[0].is_custom);
        let descriptors = models[0]
            .capabilities
            .as_ref()
            .and_then(|caps| caps.option_descriptors.clone())
            .unwrap();
        assert_eq!(descriptors.len(), 2);
        match &descriptors[0] {
            ProviderOptionDescriptor::Select(select) => {
                assert_eq!(select.id.as_str(), "reasoning");
                assert_eq!(
                    select.current_value.as_ref().map(|v| v.as_str()),
                    Some("high")
                );
                assert_eq!(select.options[1].is_default, Some(true));
                assert_eq!(select.options[0].is_default, None);
            }
            other => panic!("expected select, got {other:?}"),
        }
        match &descriptors[1] {
            ProviderOptionDescriptor::Boolean(flag) => {
                assert_eq!(flag.id.as_str(), "fast");
                assert_eq!(flag.current_value, Some(true));
            }
            other => panic!("expected boolean, got {other:?}"),
        }
    }

    #[test]
    fn custom_models_override_names_and_append_after_built_ins() {
        let built_in = catalog_models(&catalog(json!({"models": [
            {"value": "gpt-5.5", "name": "GPT-5.5", "configOptions": []}
        ]})));
        let custom: Vec<CustomModelSetting> = serde_json::from_value(json!([
            "gpt-custom",
            {"slug": "gpt-5.5", "name": "Renamed"},
            {"slug": "gpt-custom", "name": "Ignored duplicate"}
        ]))
        .unwrap();
        let models = provider_models_from_settings(&built_in, &custom);
        let names: Vec<(&str, bool)> = models
            .iter()
            .map(|model| (model.name.as_str(), model.is_custom))
            .collect();
        assert_eq!(names, [("Renamed", false), ("gpt-custom", true)]);
        assert_eq!(
            models[1].capabilities,
            Some(ModelCapabilities {
                option_descriptors: Some(Vec::new())
            })
        );
    }

    #[test]
    fn discovery_warning_downgrades_ready_and_joins_messages() {
        let driver = ProviderDriverKind::new("cursor").unwrap();
        let settings = CursorSettings {
            enabled: true,
            ..CursorSettings::default()
        };
        let probe = CursorProbe {
            installed: true,
            version: Some("2026.09.15".to_owned()),
            status: ServerProviderState::Ready,
            auth: ServerProviderAuth::with_status(ServerProviderAuthStatus::Authenticated),
            message: None,
            models: Vec::new(),
        };
        let draft = draft_from_probe(
            &driver,
            true,
            &settings,
            "2026-10-15T00:00:00.000Z".to_owned(),
            probe,
            Some("Cursor model discovery returned no models.".to_owned()),
        );
        assert_eq!(draft.status, ServerProviderState::Warning);
        assert_eq!(
            draft.message.as_ref().map(|message| message.as_str()),
            Some("Cursor model discovery returned no models.")
        );
        assert!(draft.installed);
        assert_eq!(draft.show_interaction_mode_toggle, Some(true));
    }

    #[test]
    fn initial_draft_reports_disabled_and_checking_states() {
        let driver = ProviderDriverKind::new("cursor").unwrap();
        let settings = CursorSettings::default();
        let disabled = initial_draft(&driver, &settings, false, "t".to_owned());
        assert!(!disabled.installed);
        assert_eq!(disabled.status, ServerProviderState::Disabled);
        let checking = initial_draft(
            &driver,
            &CursorSettings {
                enabled: true,
                ..settings
            },
            true,
            "t".to_owned(),
        );
        assert!(checking.installed);
        assert_eq!(checking.status, ServerProviderState::Warning);
    }
}
