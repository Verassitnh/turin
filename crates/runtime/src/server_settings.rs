//! Port of upstream/apps/server/src/serverSettings.ts: the settings file
//! service. This module holds the pure pipeline (decode, normalise, sparse
//! encode, text-generation fallback). The file, secret and watch side is in
//! [`service`]. Legacy migrations are in [`legacy`].

pub mod legacy;
pub mod service;

pub use service::ServerSettingsService;

use serde_json::{Map, Value, json};
use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceConfig};
use turin_contracts::settings::{
    ServerSettings, default_server_settings, resolve_provider_instance_enabled,
};
use turin_shared::server_settings::{
    derive_legacy_project_overrides, fallback_text_generation_model_selection,
    is_model_selection_provider_enabled,
};

use crate::settings_secrets::{SecretValue, map_settings_secrets};
use crate::support::lenient_json::parse_lenient_json;

type Object = Map<String, Value>;

/// Keys compared as a whole value, never stripped field by field.
const ATOMIC_SETTINGS_KEYS: [&str; 7] = [
    "backgroundActivity",
    "automaticGitFetchInterval",
    "providerHealthRefreshInterval",
    "sourceControlWriterModelSelection",
    "reviewGuideModelSelection",
    "textGenerationModelSelection",
    "pullRequestMergeMethod",
];

/// The `responseStreamingMode` value the retired `token` mode migrates to.
fn migrate_response_streaming_mode(value: &mut Value) {
    if value.as_str() == Some("token") {
        *value = json!("paragraph");
    }
}

/// Reports a decode failure by category and position. serde's own message
/// quotes the rejected value, which can be a credential in a mistyped field.
fn summarize_decode_error(error: serde_json::Error) -> String {
    format!(
        "{:?} at line {} column {}",
        error.classify(),
        error.line(),
        error.column()
    )
}

/// `ServerSettingsJson`: the decode of a settings file. Retired `token`
/// streaming mode decodes as `paragraph` at the top level and per project.
pub fn decode_settings_json(raw: &str) -> Result<ServerSettings, String> {
    let mut value = parse_lenient_json(raw).map_err(summarize_decode_error)?;
    if !value.is_object() {
        return Err("expected a JSON object".to_owned());
    }
    if let Some(mode) = value.get_mut("responseStreamingMode") {
        migrate_response_streaming_mode(mode);
    }
    if let Some(Value::Object(overrides)) = value.get_mut("projectSettingsOverrides") {
        for entry in overrides.values_mut() {
            if let Some(Value::Object(entry)) = Some(entry)
                && let Some(mode) = entry.get_mut("responseStreamingMode")
            {
                migrate_response_streaming_mode(mode);
            }
        }
    }
    serde_json::from_value(value).map_err(summarize_decode_error)
}

/// `LegacyProviderSettingsJson`: the retired `providers`, `bitbucket` and
/// `github` keys, read without their old schemas. Each present key must be an
/// object. Returns the raw object when that holds.
pub fn decode_legacy_file(raw: &str) -> Result<Object, String> {
    let value = parse_lenient_json(raw).map_err(summarize_decode_error)?;
    let Value::Object(object) = value else {
        return Err("expected a JSON object".to_owned());
    };
    for key in ["providers", "bitbucket", "github"] {
        if let Some(value) = object.get(key)
            && !value.is_object()
        {
            return Err(format!("`{key}` must be an object"));
        }
    }
    Ok(object)
}

/// `stripDefaultServerSettings`: the sparse form that is written to disk. Keys
/// equal to their default are omitted. Arrays and atomic keys compare whole.
pub fn strip_default(current: &Value, defaults: &Value) -> Option<Value> {
    if current.is_array() || defaults.is_array() {
        return (current != defaults).then(|| current.clone());
    }
    match (current, defaults) {
        (Value::Object(current), Value::Object(defaults)) => {
            let mut next = Object::new();
            for (key, value) in current {
                if ATOMIC_SETTINGS_KEYS.contains(&key.as_str()) {
                    if defaults.get(key) != Some(value) {
                        next.insert(key.clone(), value.clone());
                    }
                } else if let Some(stripped) =
                    strip_default(value, defaults.get(key).unwrap_or(&Value::Null))
                {
                    next.insert(key.clone(), stripped);
                }
            }
            (!next.is_empty()).then_some(Value::Object(next))
        }
        _ => (current != defaults).then(|| current.clone()),
    }
}

/// The sparse JSON text written to the settings file, with a trailing newline.
pub fn sparse_settings_json(settings: &ServerSettings) -> serde_json::Result<String> {
    sparse_settings_json_retaining(settings, &Map::new())
}

/// [`sparse_settings_json`] with `retained` keys written beside the settings
/// unchanged. The load path uses it to keep retired keys whose copy failed.
pub fn sparse_settings_json_retaining(
    settings: &ServerSettings,
    retained: &Object,
) -> serde_json::Result<String> {
    let current = serde_json::to_value(settings)?;
    let defaults = serde_json::to_value(default_server_settings())?;
    let mut sparse = strip_default(&current, &defaults).unwrap_or_else(|| json!({}));
    if let Value::Object(object) = &mut sparse {
        object.extend(
            retained
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    Ok(format!("{}\n", serde_json::to_string_pretty(&sparse)?))
}

/// `redactServerSettingsForClient`: clients see that a credential is set, never
/// its value. A sensitive value becomes empty and marked redacted when one is
/// stored. Other values pass through unchanged.
pub fn redact_server_settings_for_client(
    settings: &ServerSettings,
) -> serde_json::Result<ServerSettings> {
    map_settings_secrets(settings, |secret| {
        if secret.sensitive {
            SecretValue {
                value: String::new(),
                redacted: secret.redacted || !secret.value.is_empty(),
            }
        } else {
            SecretValue {
                value: secret.value.clone(),
                redacted: false,
            }
        }
    })
}

/// `normalizeServerSettings`: a decode round trip, the enabled-flag fold, and
/// the derived legacy project maps recomputed.
pub fn normalize_server_settings(settings: ServerSettings) -> serde_json::Result<ServerSettings> {
    let mut value = serde_json::to_value(&settings)?;
    legacy::fold_provider_instance_enabled_flags(&mut value);
    let overrides = value
        .get("projectSettingsOverrides")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (key, derived) in derive_legacy_project_overrides(&overrides) {
        value[key] = derived;
    }
    serde_json::from_value(value)
}

/// `resolveTextGenerationProvider`: keeps the text-generation selection when
/// its instance is enabled and not ACP registry, else falls back.
pub fn resolve_text_generation_provider(settings: ServerSettings) -> ServerSettings {
    let selection_ok = settings
        .provider_instances
        .get(&settings.text_generation_model_selection.instance_id)
        .map(|instance| instance.driver.as_str() != "acpRegistry")
        .unwrap_or(true);
    if is_model_selection_provider_enabled(&settings, &settings.text_generation_model_selection)
        && selection_ok
    {
        return settings;
    }
    fallback_text_generation_provider(settings)
}

/// `fallbackTextGenerationProvider`. A slot without an explicit instance uses
/// its driver's default enabled state.
fn fallback_text_generation_provider(mut settings: ServerSettings) -> ServerSettings {
    let instances = settings.provider_instances.clone();
    let fallback = fallback_text_generation_model_selection(
        &instances,
        |instance_id, driver| {
            let config =
                instances
                    .get(instance_id)
                    .cloned()
                    .unwrap_or_else(|| ProviderInstanceConfig {
                        driver: ProviderDriverKind::new(driver).expect("driver kind"),
                        display_name: None,
                        accent_color: None,
                        environment: None,
                        enabled: None,
                        config: None,
                    });
            resolve_provider_instance_enabled(&config)
        },
        false,
        settings.default_model_selection.as_ref(),
    );
    if let Some(selection) = fallback {
        settings.text_generation_model_selection = selection;
    }
    settings
}

#[cfg(test)]
mod tests;
