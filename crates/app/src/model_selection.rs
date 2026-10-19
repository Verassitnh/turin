//! Port of upstream/apps/web/src/modelSelection.ts
//!
//! The one place that decides which models an instance offers: the server's
//! built-in catalogue, the custom models authored in settings (never the
//! server's lagging custom rows), the user's hidden models and ordering, and the
//! selected model kept as an "unavailable" row when a dynamic catalogue
//! (OpenCode, Antigravity) no longer lists it. The composer's model picker and
//! the settings pickers call these functions instead of merging by hand.
//!
//! Settings arrive as the merged [`UnifiedSettings`]
//! (`settings_logic::use_settings::merge_environment_settings`), whose server
//! half already carries the resolved model preferences.

use std::collections::HashSet;

use indexmap::IndexMap;
use turin_contracts::model::{
    ANTIGRAVITY_DEFAULT_MODEL, default_text_generation_model_by_provider,
};
use turin_contracts::settings::UnifiedSettings;
use turin_provider_core::server_provider::ServerProvider;
use turin_shared::js_string::utf16_len;
use turin_shared::model::{
    CustomModelDefinition, SelectableModelOption, normalize_custom_model_slug,
    read_custom_model_entries, resolve_selectable_model,
};

use crate::composer::catalog::ProviderInstanceEntry;
use crate::composer::live_catalog::provider_entries;
use crate::composer::model_ordering::sort_models_for_provider_instance;
use crate::composer::model_ordering::{ModelSlugItem, SortModelsOptions};
use crate::composer::provider_options::{ModelCapabilities, OptionSelection, OptionValue};
use crate::composer::provider_state::{
    ComposerPromptInjectionState, ComposerProviderStateInput, ProviderModel,
    get_composer_provider_state, get_default_server_model, get_provider_models,
    resolve_selectable_provider,
};

const MAX_CUSTOM_MODEL_COUNT: usize = 32;
pub const MAX_CUSTOM_MODEL_LENGTH: usize = 256;
/// `NO_PROVIDER_MODEL_SELECTION.instanceId` (providerInstances.ts).
pub const NO_PROVIDER_INSTANCE_ID: &str = "t3code_no_provider";

/// `AppModelOption`. The optional flags upstream leaves `undefined` are `false`
/// here.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppModelOption {
    pub slug: String,
    pub name: String,
    pub short_name: Option<String>,
    pub sub_provider: Option<String>,
    pub aliases: Option<Vec<String>>,
    /// `badge: "new"`.
    pub is_new: bool,
    pub is_custom: bool,
    pub is_default: bool,
    pub is_legacy: bool,
    pub is_unavailable: bool,
}

impl ModelSlugItem for AppModelOption {
    fn slug(&self) -> &str {
        &self.slug
    }
}

impl ModelSlugItem for &AppModelOption {
    fn slug(&self) -> &str {
        &self.slug
    }
}

impl From<AppModelOption> for ProviderModel {
    fn from(option: AppModelOption) -> Self {
        Self {
            slug: option.slug,
            name: option.name,
            short_name: option.short_name,
            sub_provider: option.sub_provider,
            aliases: option.aliases.unwrap_or_default(),
            is_new: option.is_new,
            is_custom: option.is_custom,
            is_default: option.is_default,
            is_legacy: option.is_legacy,
            is_unavailable: option.is_unavailable,
            capabilities: None,
        }
    }
}

/// `resolveAppModelSelectionState`'s result: a `ModelSelection` that may carry the
/// empty model of `NO_PROVIDER_MODEL_SELECTION`, which the contract type cannot hold.
#[derive(Debug, Clone, PartialEq)]
pub struct AppModelSelection {
    pub instance_id: String,
    pub model: String,
    pub options: Vec<OptionSelection>,
}

impl AppModelSelection {
    /// `createModelSelection`: an empty option list is stored as none.
    fn create(instance_id: &str, model: &str, options: Option<Vec<OptionSelection>>) -> Self {
        Self {
            instance_id: instance_id.to_owned(),
            model: model.to_owned(),
            options: options.unwrap_or_default(),
        }
    }
}

/// `NO_PROVIDER_MODEL_SELECTION`: the placeholder shown when no provider can take
/// the work. It must never be persisted or dispatched.
pub fn no_provider_model_selection() -> AppModelSelection {
    AppModelSelection {
        instance_id: NO_PROVIDER_INSTANCE_ID.to_owned(),
        model: String::new(),
        options: Vec::new(),
    }
}

/// `readInstanceCustomModels`: the instance's own
/// `providerInstances[id].config.customModels`. Custom instances never read the
/// legacy per-driver bucket, or a model added to `claude_openrouter` would show up
/// on the stock `claudeAgent` instance.
pub fn read_instance_custom_models(
    settings: &UnifiedSettings,
    instance_id: &str,
    driver_kind: &str,
) -> Vec<CustomModelDefinition> {
    if driver_kind == "antigravity" {
        return Vec::new();
    }
    let Some(config) = settings
        .server
        .provider_instances
        .iter()
        .find(|(id, _)| id.as_str() == instance_id)
        .and_then(|(_, instance)| instance.config.as_ref())
    else {
        return Vec::new();
    };
    match config.get("customModels") {
        Some(value) if value.is_array() => read_custom_model_entries(value),
        _ => Vec::new(),
    }
}

/// `toAppModelOption`.
fn to_app_model_option(model: &ProviderModel) -> AppModelOption {
    AppModelOption {
        slug: model.slug.clone(),
        name: model.name.clone(),
        short_name: model.short_name.clone().filter(|name| !name.is_empty()),
        sub_provider: model.sub_provider.clone().filter(|name| !name.is_empty()),
        aliases: (!model.aliases.is_empty()).then(|| model.aliases.clone()),
        is_new: model.is_new,
        is_custom: model.is_custom,
        is_default: model.is_default,
        is_legacy: model.is_legacy,
        is_unavailable: false,
    }
}

/// `readInstanceModelPreferences`: `(hiddenModels, modelOrder)`.
pub fn read_instance_model_preferences(
    settings: &UnifiedSettings,
    instance_id: &str,
) -> (Vec<String>, Vec<String>) {
    settings
        .server
        .provider_model_preferences
        .iter()
        .flatten()
        .find(|(id, _)| id.as_str() == instance_id)
        .map(|(_, preferences)| {
            (
                preferences.hidden_models.clone(),
                preferences.model_order.clone(),
            )
        })
        .unwrap_or_default()
}

/// `applyInstanceModelPreferences`: hidden built-ins drop out (custom models are
/// never hidden), then the user's order applies.
fn apply_instance_model_preferences(
    options: Vec<AppModelOption>,
    hidden_models: &[String],
    model_order: &[String],
) -> Vec<AppModelOption> {
    let hidden: HashSet<&str> = hidden_models.iter().map(String::as_str).collect();
    sort_models_for_provider_instance(
        options
            .into_iter()
            .filter(|option| option.is_custom || !hidden.contains(option.slug.as_str()))
            .collect(),
        SortModelsOptions {
            model_order,
            ..Default::default()
        },
    )
}

/// `normalizeCustomModelEntries`: drops entries over the length cap, entries that
/// repeat a built-in slug or an earlier entry, and everything past the count cap.
pub fn normalize_custom_model_entries(
    models: &[CustomModelDefinition],
    built_in_model_slugs: &HashSet<&str>,
) -> Vec<CustomModelDefinition> {
    let mut normalized: Vec<CustomModelDefinition> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for candidate in models {
        if utf16_len(&candidate.slug) > MAX_CUSTOM_MODEL_LENGTH
            || built_in_model_slugs.contains(candidate.slug.as_str())
            || seen.contains(candidate.slug.as_str())
        {
            continue;
        }
        seen.insert(candidate.slug.as_str());
        normalized.push(candidate.clone());
        if normalized.len() >= MAX_CUSTOM_MODEL_COUNT {
            break;
        }
    }
    normalized
}

/// `appendUnavailableDynamicModelSelection`: a dynamic catalogue can lose the
/// selected model between refreshes; the picker keeps showing it, marked
/// unavailable, unless the user hid it.
fn append_unavailable_dynamic_model_selection(
    mut options: Vec<AppModelOption>,
    raw_models: &[ProviderModel],
    provider: &str,
    selected_model: Option<&str>,
    hidden_models: &[String],
) -> Vec<AppModelOption> {
    if provider != "opencode" && provider != "antigravity" {
        return options;
    }
    let Some(slug) = normalize_custom_model_slug(selected_model) else {
        return options;
    };
    if provider == "antigravity" && slug == ANTIGRAVITY_DEFAULT_MODEL {
        return options;
    }
    // A model that exists in the raw catalog can be absent from `options`
    // because the user hid it. Keep that preference authoritative.
    if resolve_catalog_model(provider, Some(&slug), raw_models).is_some() {
        return options;
    }
    if hidden_models.contains(&slug) {
        return options;
    }
    if options.iter().any(|option| option.slug == slug) {
        return options;
    }
    options.push(AppModelOption {
        name: slug.clone(),
        slug,
        is_custom: false,
        is_unavailable: true,
        ..Default::default()
    });
    options
}

/// `resolveSelectableModel` over catalogue models.
fn resolve_catalog_model(
    provider: &str,
    value: Option<&str>,
    models: &[ProviderModel],
) -> Option<String> {
    let options: Vec<SelectableModelOption> = models
        .iter()
        .map(|model| SelectableModelOption {
            slug: model.slug.clone(),
            name: model.name.clone(),
            aliases: model.aliases.clone(),
        })
        .collect();
    resolve_selectable_model(provider, value, &options)
}

/// `resolveSelectableModel` over app options.
fn resolve_app_option(
    provider: &str,
    value: Option<&str>,
    options: &[AppModelOption],
) -> Option<String> {
    let options: Vec<SelectableModelOption> = options
        .iter()
        .map(|option| SelectableModelOption {
            slug: option.slug.clone(),
            name: option.name.clone(),
            aliases: option.aliases.clone().unwrap_or_default(),
        })
        .collect();
    resolve_selectable_model(provider, value, &options)
}

/// The shared tail of `getAppModelOptions` and `getAppModelOptionsForInstance`:
/// built-ins from `raw_models` (server custom rows are ignored), then the
/// settings' custom models, then preferences and the unavailable selection.
fn app_model_options(
    settings: &UnifiedSettings,
    raw_models: &[ProviderModel],
    instance_id: &str,
    driver_kind: &str,
    selected_model: Option<&str>,
) -> Vec<AppModelOption> {
    // Server-reported custom rows mirror settings and can lag a removal, so only
    // built-ins are taken from the snapshot; custom rows are rebuilt from settings.
    let mut options: Vec<AppModelOption> = raw_models
        .iter()
        .filter(|model| !model.is_custom)
        .map(to_app_model_option)
        .collect();
    let mut seen: HashSet<String> = options.iter().map(|option| option.slug.clone()).collect();
    let built_in: HashSet<&str> = raw_models
        .iter()
        .filter(|model| !model.is_custom)
        .map(|model| model.slug.as_str())
        .collect();

    let custom_models = read_instance_custom_models(settings, instance_id, driver_kind);
    for entry in normalize_custom_model_entries(&custom_models, &built_in) {
        if seen.contains(&entry.slug) {
            continue;
        }
        seen.insert(entry.slug.clone());
        options.push(AppModelOption {
            slug: entry.slug,
            name: entry.name,
            is_custom: true,
            ..Default::default()
        });
    }

    let (hidden_models, model_order) = read_instance_model_preferences(settings, instance_id);
    append_unavailable_dynamic_model_selection(
        apply_instance_model_preferences(options, &hidden_models, &model_order),
        raw_models,
        driver_kind,
        selected_model,
        &hidden_models,
    )
}

/// `getAppModelOptions` (module-private upstream): the options of a driver's
/// default instance.
fn get_app_model_options(
    settings: &UnifiedSettings,
    providers: &[ProviderInstanceEntry],
    provider: &str,
    selected_model: Option<&str>,
) -> Vec<AppModelOption> {
    // The default instance id is the driver kind itself.
    app_model_options(
        settings,
        get_provider_models(providers, provider),
        provider,
        provider,
        selected_model,
    )
}

/// `getAppModelOptionsForInstance`. Built-in models come from the instance's own
/// snapshot, custom slugs from its own `config.customModels`, so two instances of
/// one driver never leak custom slugs into each other.
pub fn get_app_model_options_for_instance(
    settings: &UnifiedSettings,
    entry: &ProviderInstanceEntry,
    selected_model: Option<&str>,
) -> Vec<AppModelOption> {
    app_model_options(
        settings,
        &entry.models,
        &entry.instance_id,
        &entry.driver_kind,
        selected_model,
    )
}

/// Rebuilds `entry.models` from [`get_app_model_options_for_instance`], keeping each
/// model's capabilities: a built-in's come from the snapshot, a custom model's from
/// its settings entry. This is how the composer's catalogue picks up hidden models,
/// the user's order, settings-authored custom models and the unavailable selection.
pub fn apply_app_model_options(
    settings: &UnifiedSettings,
    entry: &mut ProviderInstanceEntry,
    selected_model: Option<&str>,
) {
    let options = get_app_model_options_for_instance(settings, entry, selected_model);
    let custom = read_instance_custom_models(settings, &entry.instance_id, &entry.driver_kind);
    let built_in = std::mem::take(&mut entry.models);
    entry.models = options
        .into_iter()
        .map(|option| {
            let capabilities = built_in
                .iter()
                .find(|model| !model.is_custom && model.slug == option.slug)
                .and_then(|model| model.capabilities.clone())
                .or_else(|| {
                    custom
                        .iter()
                        .find(|definition| definition.slug == option.slug)
                        .and_then(|definition| definition.capabilities.as_ref())
                        .map(ModelCapabilities::from)
                })
                // A custom row the server still reports keeps what it probed.
                .or_else(|| {
                    built_in
                        .iter()
                        .find(|model| model.slug == option.slug)
                        .and_then(|model| model.capabilities.clone())
                });
            let mut model = ProviderModel::from(option);
            model.capabilities = capabilities;
            model
        })
        .collect();
}

/// `resolveAppModelSelection`.
pub fn resolve_app_model_selection(
    provider: &str,
    settings: &UnifiedSettings,
    providers: &[ProviderInstanceEntry],
    selected_model: Option<&str>,
) -> String {
    let resolved_provider = resolve_selectable_provider(providers, Some(provider));
    let options = get_app_model_options(settings, providers, &resolved_provider, selected_model);
    resolve_app_option(&resolved_provider, selected_model, &options)
        .unwrap_or_else(|| get_default_server_model(providers, &resolved_provider))
}

/// `resolutionOptions` of `resolveAppModelSelectionForInstance`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModelResolutionOptions {
    pub preserve_unavailable_selection: bool,
}

/// `resolveAppModelSelectionForInstance`. `None` when the instance is unknown or
/// offers no model.
pub fn resolve_app_model_selection_for_instance(
    instance_id: &str,
    settings: &UnifiedSettings,
    providers: &[ServerProvider],
    selected_model: Option<&str>,
    resolution_options: ModelResolutionOptions,
) -> Option<String> {
    let entries = provider_entries(providers);
    let entry = entries
        .iter()
        .find(|candidate| candidate.instance_id == instance_id)?;
    resolve_for_entry(settings, entry, selected_model, resolution_options)
}

fn resolve_for_entry(
    settings: &UnifiedSettings,
    entry: &ProviderInstanceEntry,
    selected_model: Option<&str>,
    resolution_options: ModelResolutionOptions,
) -> Option<String> {
    let options = get_app_model_options_for_instance(
        settings,
        entry,
        resolution_options
            .preserve_unavailable_selection
            .then_some(selected_model)
            .flatten(),
    );
    if let Some(resolved) = resolve_app_option(&entry.driver_kind, selected_model, &options) {
        return Some(resolved);
    }
    if resolution_options.preserve_unavailable_selection
        && (entry.driver_kind == "opencode" || entry.driver_kind == "antigravity")
    {
        let unavailable = normalize_custom_model_slug(selected_model);
        let (hidden_models, _) = read_instance_model_preferences(settings, &entry.instance_id);
        if let Some(unavailable) = unavailable
            && !hidden_models.contains(&unavailable)
            && resolve_catalog_model(&entry.driver_kind, selected_model, &entry.models).is_none()
            && (entry.driver_kind != "antigravity" || unavailable != ANTIGRAVITY_DEFAULT_MODEL)
        {
            return Some(unavailable);
        }
    }
    options
        .iter()
        .find(|option| option.is_default)
        .map(|option| option.slug.clone())
        .or_else(|| options.first().map(|option| option.slug.clone()))
}

/// `getCustomModelOptionsByInstance`: each configured instance gets its own option
/// list, so the picker can show a driver's built-in and custom instances side by
/// side. Instance order follows the provider list.
pub fn get_custom_model_options_by_instance(
    settings: &UnifiedSettings,
    providers: &[ServerProvider],
    selected_instance_id: Option<&str>,
    selected_model: Option<&str>,
) -> IndexMap<String, Vec<AppModelOption>> {
    provider_entries(providers)
        .iter()
        .map(|entry| {
            let selected = (Some(entry.instance_id.as_str()) == selected_instance_id)
                .then_some(selected_model)
                .flatten();
            (
                entry.instance_id.clone(),
                get_app_model_options_for_instance(settings, entry, selected),
            )
        })
        .collect()
}

/// `selectsPlanAgent`: whether stored model options pick the opencode "plan" agent.
/// Shared settings pickers keep and show such a value even while this device's
/// legacy plan mode is off: another device may have chosen it, and this device's
/// filter only applies to picks made here.
pub fn selects_plan_agent(options: Option<&[OptionSelection]>) -> bool {
    options.is_some_and(|options| {
        options.iter().any(|option| {
            option.id == "agent"
                && matches!(&option.value, OptionValue::Str(value) if value == "plan")
        })
    })
}

/// `resolveAppModelSelectionState`: the text-generation selection settings imply,
/// healed to a provider that is enabled, available and can generate text.
pub fn resolve_app_model_selection_state(
    settings: &UnifiedSettings,
    providers: &[ServerProvider],
) -> AppModelSelection {
    let selection = &settings.server.text_generation_model_selection;
    let selection_instance_id = selection.instance_id.as_str();
    let supported_providers: Vec<ServerProvider> = providers
        .iter()
        .filter(|provider| provider.is_text_generation_capable())
        .cloned()
        .collect();
    let entries = provider_entries(&supported_providers);
    let selected_entry = entries.iter().find(|entry| {
        entry.instance_id == selection_instance_id && entry.enabled && entry.is_available
    });
    let entry = selected_entry.or_else(|| {
        entries
            .iter()
            .find(|candidate| candidate.enabled && candidate.is_available)
    });
    let Some(entry) = entry else {
        return no_provider_model_selection();
    };
    // When the instance changed due to fallback (e.g. selected instance was disabled),
    // don't carry over the old instance's model — use the fallback instance's default.
    let selected_model = selected_entry.map(|_| selection.model.as_str());
    let selected_options: Option<Vec<OptionSelection>> = selected_entry.and_then(|_| {
        selection
            .options
            .as_ref()
            .map(|options| options.iter().map(OptionSelection::from).collect())
    });
    let model = resolve_for_entry(
        settings,
        entry,
        selected_model,
        ModelResolutionOptions::default(),
    )
    .or_else(|| entry.models.first().map(|model| model.slug.clone()))
    .or_else(|| default_text_generation_model_by_provider(&entry.driver_kind).map(str::to_owned));
    let Some(model) = model.filter(|model| !model.is_empty()) else {
        return AppModelSelection::create(&entry.instance_id, "", None);
    };
    let plan_mode =
        settings.client.plan_mode_enabled || selects_plan_agent(selected_options.as_deref());
    let state = get_composer_provider_state(&ComposerProviderStateInput {
        provider: &entry.driver_kind,
        model: &model,
        models: &entry.models,
        prompt_injection_state: ComposerPromptInjectionState::None,
        model_options: selected_options.as_deref().unwrap_or_default(),
        plan_mode_enabled: plan_mode,
    });
    AppModelSelection::create(&entry.instance_id, &model, state.model_options_for_dispatch)
}

#[cfg(test)]
mod tests;
