//! Port of upstream/packages/shared/src/serverSettings.ts, with the parts of
//! `backgroundActivitySettings.ts` and `model.ts` it uses (background activity
//! presets and normalisation, `createModelSelection`, `readCustomModelEntries`).
//!
//! Patch application runs on `serde_json::Value` objects so it keeps upstream's
//! generic `deepMerge` and `null`-clears semantics. Typed contract structs are
//! converted at the edges.

use std::collections::HashSet;

use indexmap::IndexMap;
use serde_json::{Map, Value, json};
use turin_contracts::base_schemas::ProjectId;
use turin_contracts::model::DEFAULT_TEXT_GENERATION_MODEL;
use turin_contracts::model::{
    default_model_by_provider, default_text_generation_model_by_provider,
};
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::provider_instance::{
    ProviderDriverKind, ProviderInstanceConfig, ProviderInstanceId, default_instance_id_for_driver,
};
use turin_contracts::settings::{
    BackgroundActivityOverrides, BackgroundActivityProfile, BackgroundActivityProfileSelection,
    BackgroundActivitySchemaVersion, BackgroundActivitySettings,
    DEFAULT_AUTOMATIC_GIT_FETCH_INTERVAL, DEFAULT_BACKGROUND_ACTIVITY_PROFILE,
    DEFAULT_PROVIDER_HEALTH_REFRESH_INTERVAL, ServerSettings, ServerSettingsPatch,
    is_unconfigured_default_instance_enabled, resolve_project_provider_instance_enabled,
    resolve_provider_instance_enabled,
};

type Object = Map<String, Value>;

// ---------------------------------------------------------------------------
// Background activity (backgroundActivitySettings.ts)
// ---------------------------------------------------------------------------

/// `ResolvedBackgroundActivitySettings`. Durations are milliseconds, the
/// contracts encoding of `DurationFromMillis`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedBackgroundActivity {
    pub profile: BackgroundActivityProfile,
    pub automatic_git_fetch_interval: u64,
    pub provider_health_refresh_interval: u64,
    pub host_power_monitor_active_interval: u64,
    pub host_power_monitor_idle_interval: u64,
    pub idle_client_ttl: u64,
    pub pause_when_host_locked: bool,
    pub pause_when_host_low_power: bool,
    pub pause_when_client_low_power: bool,
    pub pause_when_on_battery: bool,
}

const fn seconds(value: u64) -> u64 {
    value * 1_000
}

const fn minutes(value: u64) -> u64 {
    value * 60_000
}

/// `PRESET_SETTINGS`.
fn preset_settings(profile: BackgroundActivityProfile) -> ResolvedBackgroundActivity {
    match profile {
        BackgroundActivityProfile::Performance => ResolvedBackgroundActivity {
            profile,
            automatic_git_fetch_interval: seconds(15),
            provider_health_refresh_interval: minutes(1),
            host_power_monitor_active_interval: seconds(30),
            host_power_monitor_idle_interval: minutes(2),
            idle_client_ttl: seconds(45),
            pause_when_host_locked: true,
            pause_when_host_low_power: false,
            pause_when_client_low_power: false,
            pause_when_on_battery: false,
        },
        BackgroundActivityProfile::Balanced => ResolvedBackgroundActivity {
            profile,
            automatic_git_fetch_interval: DEFAULT_AUTOMATIC_GIT_FETCH_INTERVAL.as_millis() as u64,
            provider_health_refresh_interval: DEFAULT_PROVIDER_HEALTH_REFRESH_INTERVAL.as_millis()
                as u64,
            host_power_monitor_active_interval: seconds(30),
            host_power_monitor_idle_interval: minutes(5),
            idle_client_ttl: seconds(45),
            pause_when_host_locked: true,
            pause_when_host_low_power: true,
            pause_when_client_low_power: true,
            pause_when_on_battery: false,
        },
        BackgroundActivityProfile::BatterySaver => ResolvedBackgroundActivity {
            profile,
            automatic_git_fetch_interval: 0,
            provider_health_refresh_interval: minutes(15),
            host_power_monitor_active_interval: minutes(1),
            host_power_monitor_idle_interval: minutes(10),
            idle_client_ttl: seconds(45),
            pause_when_host_locked: true,
            pause_when_host_low_power: true,
            pause_when_client_low_power: true,
            pause_when_on_battery: true,
        },
    }
}

/// `getBackgroundActivityPresetSettings`.
pub fn get_background_activity_preset_settings(
    profile: BackgroundActivityProfile,
) -> ResolvedBackgroundActivity {
    preset_settings(profile)
}

fn selection_to_profile(
    selection: BackgroundActivityProfileSelection,
) -> Option<BackgroundActivityProfile> {
    match selection {
        BackgroundActivityProfileSelection::Balanced => Some(BackgroundActivityProfile::Balanced),
        BackgroundActivityProfileSelection::Performance => {
            Some(BackgroundActivityProfile::Performance)
        }
        BackgroundActivityProfileSelection::BatterySaver => {
            Some(BackgroundActivityProfile::BatterySaver)
        }
        BackgroundActivityProfileSelection::Custom => None,
    }
}

fn profile_to_selection(profile: BackgroundActivityProfile) -> BackgroundActivityProfileSelection {
    match profile {
        BackgroundActivityProfile::Balanced => BackgroundActivityProfileSelection::Balanced,
        BackgroundActivityProfile::Performance => BackgroundActivityProfileSelection::Performance,
        BackgroundActivityProfile::BatterySaver => BackgroundActivityProfileSelection::BatterySaver,
    }
}

/// `getBackgroundActivityBaseProfile`.
pub fn get_background_activity_base_profile(
    background_activity: &BackgroundActivitySettings,
) -> BackgroundActivityProfile {
    match selection_to_profile(background_activity.profile) {
        Some(profile) => profile,
        None => background_activity
            .base_profile
            .unwrap_or(DEFAULT_BACKGROUND_ACTIVITY_PROFILE),
    }
}

/// `resolveBackgroundActivitySettings`: the base preset with the custom
/// overrides applied on top.
pub fn resolve_background_activity_settings(
    background_activity: &BackgroundActivitySettings,
) -> ResolvedBackgroundActivity {
    let base_profile = get_background_activity_base_profile(background_activity);
    let preset = preset_settings(base_profile);
    let empty = BackgroundActivityOverrides::default();
    let overrides = if background_activity.profile == BackgroundActivityProfileSelection::Custom {
        &background_activity.overrides
    } else {
        &empty
    };
    ResolvedBackgroundActivity {
        profile: base_profile,
        automatic_git_fetch_interval: overrides
            .automatic_git_fetch_interval
            .unwrap_or(preset.automatic_git_fetch_interval),
        provider_health_refresh_interval: overrides
            .provider_health_refresh_interval
            .unwrap_or(preset.provider_health_refresh_interval),
        host_power_monitor_active_interval: overrides
            .host_power_monitor_active_interval
            .unwrap_or(preset.host_power_monitor_active_interval),
        host_power_monitor_idle_interval: overrides
            .host_power_monitor_idle_interval
            .unwrap_or(preset.host_power_monitor_idle_interval),
        idle_client_ttl: overrides.idle_client_ttl.unwrap_or(preset.idle_client_ttl),
        pause_when_host_locked: overrides
            .pause_when_host_locked
            .unwrap_or(preset.pause_when_host_locked),
        pause_when_host_low_power: overrides
            .pause_when_host_low_power
            .unwrap_or(preset.pause_when_host_low_power),
        pause_when_client_low_power: overrides
            .pause_when_client_low_power
            .unwrap_or(preset.pause_when_client_low_power),
        pause_when_on_battery: overrides
            .pause_when_on_battery
            .unwrap_or(preset.pause_when_on_battery),
    }
}

fn resolved_settings_equal(a: &ResolvedBackgroundActivity, b: &ResolvedBackgroundActivity) -> bool {
    a == b
}

/// `normalizeBackgroundActivitySettings`: a custom profile that matches a preset
/// collapses to that preset; otherwise only the fields that differ from the base
/// preset are kept as overrides.
pub fn normalize_background_activity_settings(
    background_activity: &BackgroundActivitySettings,
) -> BackgroundActivitySettings {
    if background_activity.profile != BackgroundActivityProfileSelection::Custom {
        return BackgroundActivitySettings {
            schema_version: schema_version(),
            profile: background_activity.profile,
            base_profile: None,
            overrides: BackgroundActivityOverrides::default(),
        };
    }

    let resolved = resolve_background_activity_settings(background_activity);
    let base_profile = get_background_activity_base_profile(background_activity);
    for profile in [
        base_profile,
        BackgroundActivityProfile::Balanced,
        BackgroundActivityProfile::Performance,
        BackgroundActivityProfile::BatterySaver,
    ] {
        if resolved_settings_equal(&resolved, &preset_settings(profile)) {
            return BackgroundActivitySettings {
                schema_version: schema_version(),
                profile: profile_to_selection(profile),
                base_profile: None,
                overrides: BackgroundActivityOverrides::default(),
            };
        }
    }

    let preset = preset_settings(base_profile);
    let differs = |value: u64, base: u64| (value != base).then_some(value);
    let overrides = BackgroundActivityOverrides {
        automatic_git_fetch_interval: differs(
            resolved.automatic_git_fetch_interval,
            preset.automatic_git_fetch_interval,
        ),
        provider_health_refresh_interval: differs(
            resolved.provider_health_refresh_interval,
            preset.provider_health_refresh_interval,
        ),
        host_power_monitor_active_interval: differs(
            resolved.host_power_monitor_active_interval,
            preset.host_power_monitor_active_interval,
        ),
        host_power_monitor_idle_interval: differs(
            resolved.host_power_monitor_idle_interval,
            preset.host_power_monitor_idle_interval,
        ),
        idle_client_ttl: differs(resolved.idle_client_ttl, preset.idle_client_ttl),
        pause_when_host_locked: (resolved.pause_when_host_locked != preset.pause_when_host_locked)
            .then_some(resolved.pause_when_host_locked),
        pause_when_host_low_power: (resolved.pause_when_host_low_power
            != preset.pause_when_host_low_power)
            .then_some(resolved.pause_when_host_low_power),
        pause_when_client_low_power: (resolved.pause_when_client_low_power
            != preset.pause_when_client_low_power)
            .then_some(resolved.pause_when_client_low_power),
        pause_when_on_battery: (resolved.pause_when_on_battery != preset.pause_when_on_battery)
            .then_some(resolved.pause_when_on_battery),
    };

    BackgroundActivitySettings {
        schema_version: schema_version(),
        profile: BackgroundActivityProfileSelection::Custom,
        base_profile: Some(base_profile),
        overrides,
    }
}

/// `resolveServerBackgroundActivitySettings`. Settings written before
/// `backgroundActivity` existed keep their legacy interval fields and profile.
pub fn resolve_server_background_activity_settings(
    settings: &ServerSettings,
) -> ResolvedBackgroundActivity {
    let background_activity_is_default = settings.background_activity.profile
        == profile_to_selection_for_default()
        && settings.background_activity.base_profile.is_none()
        && settings.background_activity.overrides == BackgroundActivityOverrides::default();
    let legacy_profile = settings.background_activity_profile;
    let legacy_preset = preset_settings(legacy_profile);
    let has_legacy_overrides = legacy_profile != DEFAULT_BACKGROUND_ACTIVITY_PROFILE
        || settings.automatic_git_fetch_interval
            != DEFAULT_AUTOMATIC_GIT_FETCH_INTERVAL.as_millis() as u64
        || settings.provider_health_refresh_interval
            != DEFAULT_PROVIDER_HEALTH_REFRESH_INTERVAL.as_millis() as u64;

    if background_activity_is_default && has_legacy_overrides {
        let fetch_matches =
            settings.automatic_git_fetch_interval == legacy_preset.automatic_git_fetch_interval;
        let refresh_matches = settings.provider_health_refresh_interval
            == legacy_preset.provider_health_refresh_interval;
        let overrides = BackgroundActivityOverrides {
            automatic_git_fetch_interval: (!fetch_matches)
                .then_some(settings.automatic_git_fetch_interval),
            provider_health_refresh_interval: (!refresh_matches)
                .then_some(settings.provider_health_refresh_interval),
            ..Default::default()
        };
        return resolve_background_activity_settings(&BackgroundActivitySettings {
            schema_version: schema_version(),
            profile: if fetch_matches && refresh_matches {
                profile_to_selection(legacy_profile)
            } else {
                BackgroundActivityProfileSelection::Custom
            },
            base_profile: Some(legacy_profile),
            overrides,
        });
    }
    resolve_background_activity_settings(&settings.background_activity)
}

fn profile_to_selection_for_default() -> BackgroundActivityProfileSelection {
    profile_to_selection(DEFAULT_BACKGROUND_ACTIVITY_PROFILE)
}

/// `normalizeServerBackgroundActivitySettings`.
pub fn normalize_server_background_activity_settings(
    settings: &ServerSettings,
) -> BackgroundActivitySettings {
    let resolved = resolve_server_background_activity_settings(settings);
    normalize_background_activity_settings(&BackgroundActivitySettings {
        schema_version: schema_version(),
        profile: BackgroundActivityProfileSelection::Custom,
        base_profile: Some(resolved.profile),
        overrides: BackgroundActivityOverrides {
            automatic_git_fetch_interval: Some(resolved.automatic_git_fetch_interval),
            provider_health_refresh_interval: Some(resolved.provider_health_refresh_interval),
            host_power_monitor_active_interval: Some(resolved.host_power_monitor_active_interval),
            host_power_monitor_idle_interval: Some(resolved.host_power_monitor_idle_interval),
            idle_client_ttl: Some(resolved.idle_client_ttl),
            pause_when_host_locked: Some(resolved.pause_when_host_locked),
            pause_when_host_low_power: Some(resolved.pause_when_host_low_power),
            pause_when_client_low_power: Some(resolved.pause_when_client_low_power),
            pause_when_on_battery: Some(resolved.pause_when_on_battery),
        },
    })
}

// ---------------------------------------------------------------------------
// Model helpers (model.ts)
// ---------------------------------------------------------------------------

/// `readCustomModelEntries`, reduced to the slugs. Only `fallbackTextGeneration
/// ModelSelection` reads them, so the name and capability decoding is omitted.
fn read_custom_model_slugs(value: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(entries)) = value else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut slugs = Vec::new();
    for raw in entries {
        let slug = match raw {
            Value::String(slug) => Some(slug.as_str()),
            Value::Object(record) => record.get("slug").and_then(Value::as_str),
            _ => None,
        };
        let Some(slug) = slug.map(str::trim).filter(|slug| !slug.is_empty()) else {
            continue;
        };
        if seen.insert(slug.to_owned()) {
            slugs.push(slug.to_owned());
        }
    }
    slugs
}

// ---------------------------------------------------------------------------
// Legacy project overrides
// ---------------------------------------------------------------------------

/// `deriveLegacyProjectOverrides`: the per-key maps older clients still read,
/// rebuilt from `projectSettingsOverrides` so they cannot drift from it.
pub fn derive_legacy_project_overrides(project_settings_overrides: &Object) -> Object {
    let mut browser_access = Object::new();
    let mut auto_pull = Object::new();
    let mut scripts = Object::new();
    for (project_id, entry) in project_settings_overrides {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        if let Some(value) = entry.get("enableAgentBrowserAccess") {
            browser_access.insert(project_id.clone(), value.clone());
        }
        if let Some(value) = entry.get("defaultAutoPull") {
            auto_pull.insert(project_id.clone(), value.clone());
        }
        if let Some(value) = entry.get("defaultProjectScripts") {
            scripts.insert(project_id.clone(), value.clone());
        }
    }
    let mut derived = Object::new();
    derived.insert(
        "projectAgentBrowserAccessOverrides".into(),
        Value::Object(browser_access),
    );
    derived.insert("projectAutoPullOverrides".into(), Value::Object(auto_pull));
    derived.insert("projectScriptOverrides".into(), Value::Object(scripts));
    derived
}

/// `mergeProjectSettingsOverridesPatch`: an incoming entry replaces the stored
/// one, except that a `providerInstances` key the client never mentioned keeps
/// the stored value. Older clients do not know that key.
fn merge_project_settings_overrides_patch(current: &Object, patch: &Object) -> Object {
    let mut next = current.clone();
    for (project_id, entry) in patch {
        if entry.is_null() {
            next.shift_remove(project_id);
            continue;
        }
        let stored_provider_instances = current
            .get(project_id)
            .and_then(|stored| stored.get("providerInstances"))
            .cloned();
        let mut merged = entry.as_object().cloned().unwrap_or_default();
        if !merged.contains_key("providerInstances")
            && let Some(stored) = stored_provider_instances
        {
            merged.insert("providerInstances".into(), stored);
        }
        next.insert(project_id.clone(), Value::Object(merged));
    }
    next
}

/// `mergeSettingsEntries`: upsert each patched entry; `null` removes it.
fn merge_settings_entries(current: &Object, patch: &Object) -> Object {
    let mut next = current.clone();
    for (id, config) in patch {
        if config.is_null() {
            next.shift_remove(id);
        } else {
            next.insert(id.clone(), config.clone());
        }
    }
    next
}

/// `translateLegacyProjectOverridePatch`: rewrites the legacy per-key project
/// maps in a patch into entries of `projectSettingsOverrides`. The canonical
/// entry in the same patch wins over the legacy map for the same project.
fn translate_legacy_project_override_patch(current_overrides: &Object, patch: Object) -> Object {
    let mut patch = patch;
    let browser = patch.remove("projectAgentBrowserAccessOverrides");
    let auto_pull = patch.remove("projectAutoPullOverrides");
    let scripts = patch.remove("projectScriptOverrides");
    if browser.is_none() && auto_pull.is_none() && scripts.is_none() {
        return patch;
    }

    let canonical: Object = patch
        .get("projectSettingsOverrides")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let canonical_ids: HashSet<String> = canonical.keys().cloned().collect();
    // `null` entries stay in the map: they mean "remove this project".
    let mut entries: IndexMap<String, Option<Object>> = canonical
        .iter()
        .map(|(id, entry)| (id.clone(), entry.as_object().cloned()))
        .collect();

    let mut apply_key = |map: Option<Value>, key: &str| {
        let Some(Value::Object(map)) = map else {
            return;
        };
        for (project_id, value) in map {
            if canonical_ids.contains(&project_id) {
                continue;
            }
            let mut entry = entries
                .get(&project_id)
                .cloned()
                .flatten()
                .or_else(|| {
                    current_overrides
                        .get(&project_id)
                        .and_then(Value::as_object)
                        .cloned()
                })
                .unwrap_or_default();
            if value.is_null() {
                entry.shift_remove(key);
            } else {
                entry.insert(key.to_owned(), value);
            }
            entries.insert(project_id, (!entry.is_empty()).then_some(entry));
        }
    };
    apply_key(browser, "enableAgentBrowserAccess");
    apply_key(auto_pull, "defaultAutoPull");
    apply_key(scripts, "defaultProjectScripts");

    let mut translated: Object = Object::new();
    for (id, entry) in entries {
        translated.insert(id, entry.map(Value::Object).unwrap_or(Value::Null));
    }
    patch.insert("projectSettingsOverrides".into(), Value::Object(translated));
    patch
}

// ---------------------------------------------------------------------------
// Deep merge (Struct.ts)
// ---------------------------------------------------------------------------

/// `deepMerge`: plain objects merge key by key; anything else is replaced. A
/// `null` in the patch overwrites, so it clears. Arrays are replaced, not merged
/// (see PORTING Deviations: Effect's `isObject` would turn them into objects).
fn deep_merge(current: &Value, patch: &Value) -> Value {
    let (Value::Object(current), Value::Object(patch)) = (current, patch) else {
        return patch.clone();
    };
    let mut next = current.clone();
    for (key, value) in patch {
        let merged = match (next.get(key), value) {
            (Some(existing @ Value::Object(_)), Value::Object(_)) => deep_merge(existing, value),
            _ => value.clone(),
        };
        next.insert(key.clone(), merged);
    }
    Value::Object(next)
}

// ---------------------------------------------------------------------------
// applyServerSettingsPatch
// ---------------------------------------------------------------------------

/// `createModelSelection`: options are cloned, and an empty list is dropped.
fn create_model_selection_value(
    instance_id: &Value,
    model: &Value,
    options: Option<&Value>,
) -> Value {
    let mut selection = json!({ "instanceId": instance_id, "model": model });
    if let Some(Value::Array(options)) = options
        && !options.is_empty()
    {
        selection["options"] = Value::Array(options.clone());
    }
    selection
}

/// `mergeModelSelectionOptionsById`: a patched option upserts by id. An empty
/// patch list clears all options; a missing patch list keeps the current ones.
fn merge_model_selection_options_by_id(
    current: Option<&Value>,
    patch: Option<&Value>,
) -> Option<Value> {
    let Some(patch) = patch else {
        return current.cloned().filter(|value| value.is_array());
    };
    let patch = patch.as_array()?;
    if patch.is_empty() {
        return None;
    }
    let mut merged: IndexMap<String, Value> = IndexMap::new();
    for selection in current.and_then(Value::as_array).into_iter().flatten() {
        if let Some(id) = selection.get("id").and_then(Value::as_str) {
            merged.insert(
                id.to_owned(),
                selection.get("value").cloned().unwrap_or(Value::Null),
            );
        }
    }
    for selection in patch {
        if let Some(id) = selection.get("id").and_then(Value::as_str) {
            merged.insert(
                id.to_owned(),
                selection.get("value").cloned().unwrap_or(Value::Null),
            );
        }
    }
    Some(Value::Array(
        merged
            .into_iter()
            .map(|(id, value)| json!({ "id": id, "value": value }))
            .collect(),
    ))
}

/// JS `a ?? b` on optional JSON values: `null` counts as missing.
fn nullish_owned(primary: Option<Value>, fallback: Option<Value>) -> Option<Value> {
    primary
        .filter(|value| !value.is_null())
        .or(fallback.filter(|value| !value.is_null()))
}

fn nullish<'a>(primary: Option<&'a Value>, fallback: Option<&'a Value>) -> Option<&'a Value> {
    match primary {
        Some(value) if !value.is_null() => Some(value),
        _ => fallback.filter(|value| !value.is_null()),
    }
}

/// `applyServerSettingsPatch`. Applies `raw_patch` to `current` and returns the
/// next settings. Derived fields (legacy project maps, normalised background
/// activity, legacy interval fields) are recomputed.
pub fn apply_server_settings_patch(
    current: &ServerSettings,
    raw_patch: &ServerSettingsPatch,
) -> serde_json::Result<ServerSettings> {
    let current_value = serde_json::to_value(current)?;
    let current_obj = current_value.as_object().cloned().unwrap_or_default();
    let current_overrides = current_obj
        .get("projectSettingsOverrides")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let patch = translate_legacy_project_override_patch(
        &current_overrides,
        serde_json::to_value(raw_patch)?
            .as_object()
            .cloned()
            .unwrap_or_default(),
    );

    let selection_patch = patch.get("textGenerationModelSelection").cloned();
    let take = |patch: &mut Object, key: &str| patch.remove(key);
    let mut patch_for_merge = patch.clone();
    let migrate = take(&mut patch_for_merge, "migrateModelPreferences");
    let set_favorites = take(&mut patch_for_merge, "setModelFavorites");
    let set_models_hidden = take(&mut patch_for_merge, "setModelsHidden");
    let set_provider_model_order = take(&mut patch_for_merge, "setProviderModelOrder");
    let provider_model_preferences_patch = take(&mut patch_for_merge, "providerModelPreferences");
    let automatic_git_fetch_patch = take(&mut patch_for_merge, "automaticGitFetchInterval");
    let provider_health_patch = take(&mut patch_for_merge, "providerHealthRefreshInterval");
    let background_profile_patch = take(&mut patch_for_merge, "backgroundActivityProfile");
    let background_activity_patch = take(&mut patch_for_merge, "backgroundActivity");
    let worktree_cleanup_patch = take(&mut patch_for_merge, "worktreeCleanup");
    let usage_limit_sources_patch = take(&mut patch_for_merge, "usageLimitSources");
    let source_control_hosts_patch = take(&mut patch_for_merge, "sourceControlHosts");
    let usage_price_overrides_patch = take(&mut patch_for_merge, "usagePriceOverrides");
    let usage_model_aliases_patch = take(&mut patch_for_merge, "usageModelAliases");
    let project_settings_overrides_patch = take(&mut patch_for_merge, "projectSettingsOverrides");

    let current_background = normalize_server_background_activity_settings(current);
    let current_background_value = serde_json::to_value(&current_background)?;
    let background_patch: Option<Value> = if background_profile_patch.is_some() {
        let profile = background_profile_patch.clone().unwrap_or(Value::Null);
        let custom = automatic_git_fetch_patch.is_some() || provider_health_patch.is_some();
        let mut overrides = Object::new();
        if let Some(value) = &automatic_git_fetch_patch {
            overrides.insert("automaticGitFetchInterval".into(), value.clone());
        }
        if let Some(value) = &provider_health_patch {
            overrides.insert("providerHealthRefreshInterval".into(), value.clone());
        }
        let mut next = json!({
            "schemaVersion": 1,
            "profile": if custom { json!("custom") } else { profile.clone() },
            "overrides": Value::Object(overrides),
        });
        if custom {
            next["baseProfile"] = profile;
        }
        Some(next)
    } else if automatic_git_fetch_patch.is_some() || provider_health_patch.is_some() {
        let mut overrides = overrides_object(&current_background);
        if current_background.profile != BackgroundActivityProfileSelection::Custom {
            overrides = Object::new();
        }
        if let Some(value) = &automatic_git_fetch_patch {
            overrides.insert("automaticGitFetchInterval".into(), value.clone());
        }
        if let Some(value) = &provider_health_patch {
            overrides.insert("providerHealthRefreshInterval".into(), value.clone());
        }
        Some(json!({
            "schemaVersion": 1,
            "profile": "custom",
            "baseProfile": get_background_activity_base_profile(&current_background),
            "overrides": Value::Object(overrides),
        }))
    } else {
        None
    };

    // Favorites: a patch list replaces; `setModelFavorites` edits in place.
    let initial_favorites = nullish(patch.get("favorites"), current_obj.get("favorites"))
        .or_else(|| migrate.as_ref().and_then(|m| m.get("favorites")))
        .cloned()
        .unwrap_or(Value::Null);
    let favorites = match &set_favorites {
        Some(changes) => {
            let mut list: Vec<Value> = initial_favorites.as_array().cloned().unwrap_or_default();
            for change in changes.as_array().into_iter().flatten() {
                let provider = change.get("provider");
                let model = change.get("model");
                let is_match = |favorite: &Value| {
                    favorite.get("provider") == provider && favorite.get("model") == model
                };
                let exists = list.iter().any(is_match);
                let favorite = change
                    .get("favorite")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if favorite {
                    if !exists {
                        list.push(json!({ "provider": provider, "model": model }));
                    }
                } else {
                    list.retain(|favorite| !is_match(favorite));
                }
            }
            Value::Array(list)
        }
        None => initial_favorites,
    };

    // Model preferences: hidden models and per-provider order.
    let initial_preferences = nullish(
        provider_model_preferences_patch.as_ref(),
        current_obj.get("providerModelPreferences"),
    )
    .or_else(|| {
        migrate
            .as_ref()
            .and_then(|m| m.get("providerModelPreferences"))
    })
    .cloned()
    .unwrap_or(Value::Null);
    let mut next_preferences = initial_preferences;
    let empty_preferences = || json!({ "hiddenModels": [], "modelOrder": [] });
    for change in set_models_hidden
        .as_ref()
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let provider = change
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let model = change.get("model").cloned().unwrap_or(Value::Null);
        let mut preferences = next_preferences
            .get(provider)
            .cloned()
            .filter(|value| !value.is_null())
            .unwrap_or_else(empty_preferences);
        let mut hidden: Vec<Value> = preferences
            .get("hiddenModels")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        hidden.retain(|item| *item != model);
        if change
            .get("hidden")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            hidden.push(model);
        }
        preferences["hiddenModels"] = Value::Array(hidden);
        let mut object = next_preferences.as_object().cloned().unwrap_or_default();
        object.insert(provider.to_owned(), preferences);
        next_preferences = Value::Object(object);
    }
    if let Some(order) = &set_provider_model_order {
        let provider = order
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut preferences = next_preferences
            .get(provider)
            .cloned()
            .filter(|value| !value.is_null())
            .unwrap_or_else(empty_preferences);
        preferences["modelOrder"] = order.get("modelOrder").cloned().unwrap_or(json!([]));
        let mut object = next_preferences.as_object().cloned().unwrap_or_default();
        object.insert(provider.to_owned(), preferences);
        next_preferences = Value::Object(object);
    }

    let mut next = deep_merge(&current_value, &Value::Object(patch_for_merge.clone()));
    let next_obj = next.as_object().cloned().unwrap_or_default();
    let mut base = next_obj.clone();
    base.insert("favorites".into(), favorites);
    base.insert("providerModelPreferences".into(), next_preferences);

    if let Some(patch_cleanup) = worktree_cleanup_patch {
        let value = if patch_cleanup.get("mode").and_then(Value::as_str) == Some("custom") {
            let storage = next_obj.get("storageCleanup").and_then(Value::as_object);
            let mut rules = Object::new();
            for key in [
                "worktreeKeepWhen",
                "worktreeAfterDays",
                "worktreeOnMerge",
                "worktreeOnDelete",
                "worktreeUnchanged",
            ] {
                rules.insert(
                    key.into(),
                    storage
                        .and_then(|s| s.get(key))
                        .cloned()
                        .unwrap_or(Value::Null),
                );
            }
            if current_obj
                .get("worktreeCleanup")
                .and_then(|value| value.get("mode"))
                .and_then(Value::as_str)
                == Some("custom")
                && let Some(existing) = current_obj
                    .get("worktreeCleanup")
                    .and_then(|value| value.get("rules"))
                    .and_then(Value::as_object)
            {
                rules.extend(existing.clone());
            }
            if let Some(patch_rules) = patch_cleanup.get("rules").and_then(Value::as_object) {
                rules.extend(patch_rules.clone());
            }
            json!({ "mode": "custom", "rules": Value::Object(rules) })
        } else {
            patch_cleanup
        };
        base.insert("worktreeCleanup".into(), value);
    }

    match &background_activity_patch {
        Some(patch_bg) => {
            let mut merged = deep_merge(&current_background_value, patch_bg);
            if let Some(overrides) = patch_bg.get("overrides") {
                merged["overrides"] = overrides.clone();
            }
            base.insert("backgroundActivity".into(), merged);
        }
        None => {
            base.insert(
                "backgroundActivity".into(),
                current_background_value.clone(),
            );
        }
    }
    if background_activity_patch.is_none()
        && let Some(value) = background_patch
    {
        base.insert("backgroundActivity".into(), value);
    }

    if let Some(providers) = patch.get("providerInstances") {
        base.insert("providerInstances".into(), providers.clone());
    }

    if let Some(worktrees_directory) = patch.get("worktreesDirectory")
        && Some(worktrees_directory) != current_obj.get("worktreesDirectory")
    {
        let previous: Vec<Value> = current_obj
            .get("previousWorktreesDirectories")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let current_dir = current_obj
            .get("worktreesDirectory")
            .and_then(Value::as_str);
        let mut list: Vec<Value> = previous
            .iter()
            .filter(|directory| *directory != worktrees_directory)
            .cloned()
            .collect();
        if let Some(current_dir) = current_dir.filter(|dir| !dir.is_empty())
            && !previous.iter().any(|d| d.as_str() == Some(current_dir))
        {
            list.push(Value::String(current_dir.to_owned()));
        }
        base.insert("previousWorktreesDirectories".into(), Value::Array(list));
    }

    // Per host, a patched field replaces the saved one: deepMerge would keep a cleared account pin.
    if let Some(Value::Object(hosts_patch)) = &source_control_hosts_patch {
        let mut hosts = current_obj
            .get("sourceControlHosts")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (kind, fields) in hosts_patch {
            let mut merged = hosts
                .get(kind)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if let Some(fields) = fields.as_object() {
                merged.extend(fields.clone());
            }
            hosts.insert(kind.clone(), Value::Object(merged));
        }
        base.insert("sourceControlHosts".into(), Value::Object(hosts));
    }

    if let Some(Value::Object(overrides_patch)) = &project_settings_overrides_patch {
        let stored = current_obj
            .get("projectSettingsOverrides")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let merged = merge_project_settings_overrides_patch(&stored, overrides_patch);
        let kept: Object = merged
            .into_iter()
            .filter(|(_, entry)| entry.as_object().is_some_and(|entry| !entry.is_empty()))
            .collect();
        base.insert("projectSettingsOverrides".into(), Value::Object(kept));
    }

    for key in ["defaultModelSelection", "defaultProjectScripts"] {
        if let Some(value) = patch.get(key) {
            base.insert(key.into(), value.clone());
        }
    }

    let merge_entries = |key: &str, patch_value: &Option<Value>, base: &mut Object| {
        if let Some(Value::Object(entries_patch)) = patch_value {
            let stored = current_obj
                .get(key)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            base.insert(
                key.into(),
                Value::Object(merge_settings_entries(&stored, entries_patch)),
            );
        }
    };
    merge_entries("usageLimitSources", &usage_limit_sources_patch, &mut base);
    merge_entries(
        "usagePriceOverrides",
        &usage_price_overrides_patch,
        &mut base,
    );
    merge_entries("usageModelAliases", &usage_model_aliases_patch, &mut base);

    for key in [
        "sourceControlWriterModelSelection",
        "reviewGuideModelSelection",
    ] {
        if let Some(value) = patch.get(key) {
            base.insert(key.into(), value.clone());
        }
    }
    if let Some(value) = &automatic_git_fetch_patch {
        base.insert("automaticGitFetchInterval".into(), value.clone());
    }
    if let Some(value) = &provider_health_patch {
        base.insert("providerHealthRefreshInterval".into(), value.clone());
    }

    // Normalise the background activity, then mirror the resolved values into the legacy fields.
    let background_typed: BackgroundActivitySettings = serde_json::from_value(
        base.get("backgroundActivity")
            .cloned()
            .unwrap_or(Value::Null),
    )?;
    let normalized = normalize_background_activity_settings(&background_typed);
    let resolved = resolve_background_activity_settings(&normalized);
    let legacy = derive_legacy_project_overrides(
        base.get("projectSettingsOverrides")
            .and_then(Value::as_object)
            .unwrap_or(&Object::new()),
    );
    for (key, value) in legacy {
        base.insert(key, value);
    }
    base.insert(
        "backgroundActivity".into(),
        serde_json::to_value(&normalized)?,
    );
    base.insert(
        "automaticGitFetchInterval".into(),
        json!(resolved.automatic_git_fetch_interval),
    );
    base.insert(
        "providerHealthRefreshInterval".into(),
        json!(resolved.provider_health_refresh_interval),
    );
    base.insert(
        "backgroundActivityProfile".into(),
        serde_json::to_value(resolved.profile)?,
    );
    next = Value::Object(base);

    let Some(selection_patch) = selection_patch else {
        return serde_json::from_value(next);
    };

    let current_selection = current_obj
        .get("textGenerationModelSelection")
        .cloned()
        .unwrap_or(Value::Null);
    let instance_id = selection_patch
        .get("instanceId")
        .filter(|value| !value.is_null())
        .unwrap_or(&current_selection["instanceId"])
        .clone();
    let model = selection_patch
        .get("model")
        .filter(|value| !value.is_null())
        .unwrap_or(&current_selection["model"])
        .clone();
    let options =
        if selection_patch.get("instanceId").is_some() || selection_patch.get("model").is_some() {
            selection_patch.get("options").cloned()
        } else {
            merge_model_selection_options_by_id(
                current_selection.get("options"),
                selection_patch.get("options"),
            )
        };
    let selection = create_model_selection_value(&instance_id, &model, options.as_ref());
    next["textGenerationModelSelection"] = selection;
    serde_json::from_value(next)
}

/// The `overrides` of a background activity as a JSON object.
fn overrides_object(settings: &BackgroundActivitySettings) -> Object {
    serde_json::to_value(&settings.overrides)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

/// `schemaVersion: 1`, the only version the contracts accept.
fn schema_version() -> BackgroundActivitySchemaVersion {
    serde_json::from_value(json!(1)).expect("schema version 1 is valid")
}

/// `resolveProjectAgentBrowserAccess` (deprecated upstream): the project's
/// setting, else its legacy map entry, else the machine default.
pub fn resolve_project_agent_browser_access(
    settings: &ServerSettings,
    project_id: &ProjectId,
) -> bool {
    settings
        .project_settings_overrides
        .get(project_id)
        .and_then(|overrides| overrides.enable_agent_browser_access)
        .or_else(|| {
            settings
                .project_agent_browser_access_overrides
                .get(project_id)
                .copied()
        })
        .unwrap_or(settings.enable_agent_browser_access)
}

/// `resolveProjectAutoPull` (deprecated upstream). Existing opt-ins stay on
/// until explicitly overridden or reset: `legacy_auto_pull` ORs in.
pub fn resolve_project_auto_pull(
    settings: &ServerSettings,
    project_id: &ProjectId,
    legacy_auto_pull: bool,
) -> bool {
    settings
        .project_settings_overrides
        .get(project_id)
        .and_then(|overrides| overrides.default_auto_pull)
        .or_else(|| {
            settings
                .project_auto_pull_overrides
                .get(project_id)
                .copied()
        })
        .unwrap_or(legacy_auto_pull || settings.default_auto_pull)
}

// ---------------------------------------------------------------------------
// Model preference helpers
// ---------------------------------------------------------------------------

/// `resolveModelPreferences`: the saved value wins; legacy local values fill in
/// only while the environment has none.
pub fn resolve_model_preferences(
    favorites: Option<Value>,
    provider_model_preferences: Option<Value>,
    legacy_favorites: Option<Value>,
    legacy_provider_model_preferences: Option<Value>,
) -> (Value, Value) {
    (
        nullish_owned(favorites, legacy_favorites).unwrap_or_else(|| json!([])),
        nullish_owned(
            provider_model_preferences,
            legacy_provider_model_preferences,
        )
        .unwrap_or_else(|| json!({})),
    )
}

/// `modelPreferencesMigrationPatch`. Empty legacy values do not claim ownership,
/// so another client can still migrate its saved list.
pub fn model_preferences_migration_patch(
    favorites: Option<&Value>,
    provider_model_preferences: Option<&Value>,
    legacy_favorites: Option<&Value>,
    legacy_provider_model_preferences: Option<&Value>,
) -> Option<ServerSettingsPatch> {
    // `=== null` in upstream: an absent key is not null here.
    let favorites_null = matches!(favorites, Some(Value::Null));
    let legacy_favorites =
        legacy_favorites.filter(|value| value.as_array().is_some_and(|a| !a.is_empty()));
    let preferences_null = matches!(provider_model_preferences, Some(Value::Null));
    let legacy_preferences = legacy_provider_model_preferences
        .filter(|value| value.as_object().is_some_and(|o| !o.is_empty()));
    let migrate_favorites = favorites_null.then_some(legacy_favorites).flatten();
    let migrate_preferences = preferences_null.then_some(legacy_preferences).flatten();
    if migrate_favorites.is_none() && migrate_preferences.is_none() {
        return None;
    }
    let mut migrate = Object::new();
    if let Some(value) = migrate_favorites {
        migrate.insert("favorites".into(), value.clone());
    }
    if let Some(value) = migrate_preferences {
        migrate.insert("providerModelPreferences".into(), value.clone());
    }
    serde_json::from_value(json!({ "migrateModelPreferences": Value::Object(migrate) })).ok()
}

// ---------------------------------------------------------------------------
// Model selection resolution
// ---------------------------------------------------------------------------

/// `isModelSelectionProviderEnabled`.
pub fn is_model_selection_provider_enabled(
    settings: &ServerSettings,
    selection: &ModelSelection,
) -> bool {
    match settings.provider_instances.get(&selection.instance_id) {
        Some(instance) => resolve_provider_instance_enabled(instance),
        None => is_unconfigured_default_instance_enabled(&selection.instance_id),
    }
}

/// Drivers the text-generation fallback tries, in preference order.
const TEXT_GENERATION_FALLBACK_DRIVERS: [&str; 8] = [
    "codex",
    "claudeAgent",
    "cursor",
    "grok",
    "muse",
    "pi",
    "opencode",
    "antigravity",
];

/// `fallbackTextGenerationModelSelection`. The first enabled instance wins.
/// When `default_model_selection` names a custom model of that instance, it is
/// kept as-is, since the built-in default may be refused there.
pub fn fallback_text_generation_model_selection(
    provider_instances: &IndexMap<ProviderInstanceId, ProviderInstanceConfig>,
    is_enabled: impl Fn(&ProviderInstanceId, &str) -> bool,
    include_configured_instances: bool,
    default_model_selection: Option<&ModelSelection>,
) -> Option<ModelSelection> {
    let mut candidates: Vec<(ProviderInstanceId, String)> = TEXT_GENERATION_FALLBACK_DRIVERS
        .iter()
        .map(|driver| {
            let kind = ProviderDriverKind::new(driver).expect("static driver kind");
            (default_instance_id_for_driver(&kind), (*driver).to_owned())
        })
        .collect();
    if include_configured_instances {
        for (instance_id, instance) in provider_instances {
            let driver = instance.driver.as_str();
            if TEXT_GENERATION_FALLBACK_DRIVERS.contains(&driver) {
                candidates.push((instance_id.clone(), driver.to_owned()));
            }
        }
    }
    let (instance_id, driver) = candidates
        .into_iter()
        .find(|(instance_id, driver)| is_enabled(instance_id, driver))?;

    if let Some(thread_default) = default_model_selection {
        let custom_models = provider_instances
            .get(&instance_id)
            .and_then(|instance| instance.config.as_ref())
            .and_then(|config| config.get("customModels"));
        let slugs = read_custom_model_slugs(custom_models);
        if thread_default.instance_id == instance_id
            && slugs
                .iter()
                .any(|slug| slug == thread_default.model.as_str())
        {
            return Some(thread_default.clone());
        }
    }

    let model = default_text_generation_model_by_provider(&driver)
        .or_else(|| default_model_by_provider(&driver))
        .unwrap_or(DEFAULT_TEXT_GENERATION_MODEL);
    Some(ModelSelection {
        instance_id,
        model: turin_contracts::base_schemas::TrimmedNonEmptyString::new(model)
            .expect("default model slug is non-empty"),
        options: None,
    })
}

/// The provider status fields the writer selection checks. Implemented by the
/// provider-core server provider snapshot.
pub trait ProviderStatus {
    fn instance_id(&self) -> &ProviderInstanceId;
    fn enabled(&self) -> bool;
    /// `isProviderAvailable`.
    fn is_available(&self) -> bool;
    /// `isProviderTextGenerationCapable`.
    fn is_text_generation_capable(&self) -> bool;
}

/// `resolveSourceControlWriterModelSelection`. `project_id` is required on
/// purpose: `None` means "outside any project", and must be passed explicitly.
pub fn resolve_source_control_writer_model_selection<P: ProviderStatus>(
    settings: &ServerSettings,
    providers: Option<&[P]>,
    project_id: Option<&turin_contracts::base_schemas::ProjectId>,
) -> ModelSelection {
    let Some(selection) = settings.source_control_writer_model_selection.as_ref() else {
        return settings.text_generation_model_selection.clone();
    };
    if !resolve_project_provider_instance_enabled(
        &settings.provider_instances,
        &settings.project_settings_overrides,
        project_id,
        &selection.instance_id,
    ) {
        return settings.text_generation_model_selection.clone();
    }
    let Some(providers) = providers else {
        return selection.clone();
    };
    let provider = providers
        .iter()
        .find(|candidate| candidate.instance_id() == &selection.instance_id);
    match provider {
        Some(provider)
            if provider.enabled()
                && provider.is_available()
                && provider.is_text_generation_capable() =>
        {
            selection.clone()
        }
        _ => settings.text_generation_model_selection.clone(),
    }
}

/// `resolveReviewGuideModelSelection`: the review guide's own model when that
/// provider can still run it; otherwise the source-control writer's model.
pub fn resolve_review_guide_model_selection<P: ProviderStatus>(
    settings: &ServerSettings,
    providers: Option<&[P]>,
    project_id: Option<&turin_contracts::base_schemas::ProjectId>,
) -> ModelSelection {
    if let Some(selection) = settings.review_guide_model_selection.as_ref()
        && resolve_project_provider_instance_enabled(
            &settings.provider_instances,
            &settings.project_settings_overrides,
            project_id,
            &selection.instance_id,
        )
    {
        let provider = providers.and_then(|providers| {
            providers
                .iter()
                .find(|candidate| candidate.instance_id() == &selection.instance_id)
        });
        let usable = match providers {
            None => true,
            Some(_) => provider.is_some_and(|provider| {
                provider.enabled()
                    && provider.is_available()
                    && provider.is_text_generation_capable()
            }),
        };
        if usable {
            return selection.clone();
        }
    }
    resolve_source_control_writer_model_selection(settings, providers, project_id)
}

#[cfg(test)]
mod tests;
