//! Logic of ProviderSettingsForm.tsx, ProviderInstanceCard.tsx and
//! ProviderModelsSection.tsx (their exported pure helpers), with the upstream
//! tests that apply to the kept drivers.
//! Also: Port of upstream/apps/web/src/modelOrdering.ts.
//!
//! The per-driver settings schemas come from `turin_contracts::settings`
//! (Codex, Claude); Cursor and OpenCode forms are defined here from their
//! client packages (`provider-cursor/src/settings.ts`,
//! `provider-opencode/src/settings.ts`).
//!
//! Port of upstream/apps/web/src/components/settings/ProviderModelsSection.tsx (logic).
//! Port of upstream/apps/web/src/components/settings/ProviderSettingsForm.tsx (logic).

use std::collections::HashSet;

use serde_json::{Map, Value};
use turin_contracts::settings::{
    CLAUDE_SETTINGS_FORM_ORDER, CODEX_SETTINGS_FORM_ORDER, ClearWhenEmpty,
    ProviderSettingsFormControl, ProviderSettingsFormOption, claude_settings_form_fields,
    codex_settings_form_fields,
};
use turin_provider_core::server_provider::ServerProviderModel;
use turin_shared::model::{CustomModelDefinition, read_custom_model_entries};

use crate::composer::model_ordering::{
    ModelSlugItem, SortModelsOptions, sort_models_for_provider_instance,
};

impl ModelSlugItem for &ServerProviderModel {
    fn slug(&self) -> &str {
        self.slug.as_str()
    }
}

/// `ProviderSettingsFieldModel`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderSettingsFieldModel {
    pub key: String,
    pub control: ProviderSettingsFormControl,
    pub label: String,
    pub description: Option<String>,
    pub placeholder: Option<String>,
    pub clear_when_empty: ClearWhenEmpty,
    /// Kept in the server's secret store: a saved value arrives redacted.
    pub secret: bool,
    pub default_boolean_value: Option<bool>,
    /// Choices for a `select` control. The first entry is the default.
    pub options: Option<Vec<ProviderSettingsFormOption>>,
}

/// A named environment field a driver asks for (`ProviderEnvironmentField`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderEnvironmentField {
    pub name: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub placeholder: &'static str,
    /// `undefined` means secure: `sensitive ?? true`.
    pub sensitive: Option<bool>,
}

impl ProviderEnvironmentField {
    pub fn is_sensitive(&self) -> bool {
        self.sensitive.unwrap_or(true)
    }
}

/// `cursorClient.environmentFields`.
pub const CURSOR_ENVIRONMENT_FIELDS: &[ProviderEnvironmentField] = &[ProviderEnvironmentField {
    name: "CURSOR_API_KEY",
    label: "Cursor API key",
    description: "Optional. Overrides browser sign-in for this provider.",
    placeholder: "Paste API key",
    sensitive: Some(true),
}];

pub fn environment_fields_for_driver(driver: &str) -> &'static [ProviderEnvironmentField] {
    if driver == "cursor" {
        CURSOR_ENVIRONMENT_FIELDS
    } else {
        &[]
    }
}

fn titleize_field_key(key: &str) -> String {
    let mut spaced = String::new();
    let mut previous: Option<char> = None;
    for c in key.chars() {
        if let Some(p) = previous
            && (p.is_ascii_lowercase() || p.is_ascii_digit())
            && c.is_ascii_uppercase()
        {
            spaced.push(' ');
        }
        spaced.push(if c == '-' || c == '_' { ' ' } else { c });
        previous = Some(c);
    }
    let mut chars = spaced.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn field_from_contract(
    contract: &turin_contracts::settings::ProviderSettingsFormField,
    secret: bool,
) -> Option<ProviderSettingsFieldModel> {
    let annotation = &contract.annotation;
    if annotation.hidden == Some(true) {
        return None;
    }
    Some(ProviderSettingsFieldModel {
        key: contract.field.to_owned(),
        control: annotation
            .control
            .unwrap_or(ProviderSettingsFormControl::Text),
        label: contract
            .title
            .map(str::to_owned)
            .unwrap_or_else(|| titleize_field_key(contract.field)),
        description: contract.description.map(str::to_owned),
        placeholder: annotation.placeholder.clone(),
        clear_when_empty: annotation.clear_when_empty.unwrap_or(ClearWhenEmpty::Omit),
        secret,
        default_boolean_value: None,
        options: annotation.options.clone(),
    })
}

fn ordered(
    fields: Vec<turin_contracts::settings::ProviderSettingsFormField>,
    order: &[&str],
) -> Vec<ProviderSettingsFieldModel> {
    let mut indexed: Vec<(usize, &turin_contracts::settings::ProviderSettingsFormField)> =
        fields.iter().enumerate().collect();
    indexed.sort_by_key(|(index, field)| {
        order
            .iter()
            .position(|key| *key == field.field)
            .unwrap_or(order.len() + index)
    });
    indexed
        .into_iter()
        .filter_map(|(_, field)| field_from_contract(field, false))
        .collect()
}

fn opencode_fields() -> Vec<ProviderSettingsFieldModel> {
    let text = |key: &str, label: &str, description: &str, placeholder: &str, control| {
        ProviderSettingsFieldModel {
            key: key.into(),
            control,
            label: label.into(),
            description: Some(description.into()),
            placeholder: Some(placeholder.into()),
            clear_when_empty: ClearWhenEmpty::Omit,
            secret: false,
            default_boolean_value: None,
            options: None,
        }
    };
    vec![
        text(
            "binaryPath",
            "Binary path",
            "Path to the OpenCode binary.",
            "opencode",
            ProviderSettingsFormControl::Text,
        ),
        text(
            "serverUrl",
            "Server URL",
            "Leave blank to let T3 Code spawn the server when needed.",
            "http://127.0.0.1:4096",
            ProviderSettingsFormControl::Text,
        ),
        text(
            "serverPassword",
            "Server password",
            "Stored in plain text on disk.",
            "Optional",
            ProviderSettingsFormControl::Password,
        ),
    ]
}

/// `deriveProviderSettingsFields` for a driver kind.
pub fn derive_provider_settings_fields(driver: &str) -> Vec<ProviderSettingsFieldModel> {
    match driver {
        "codex" => ordered(codex_settings_form_fields(), CODEX_SETTINGS_FORM_ORDER),
        "claudeAgent" => ordered(claude_settings_form_fields(), CLAUDE_SETTINGS_FORM_ORDER),
        "opencode" => opencode_fields(),
        // Cursor uses a dedicated environment field instead of its legacy CLI settings.
        _ => Vec::new(),
    }
}

/// `readProviderConfigString`.
pub fn read_provider_config_string(config: Option<&Value>, key: &str) -> String {
    config
        .and_then(|config| config.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

pub fn read_provider_config_boolean(config: Option<&Value>, key: &str, default: bool) -> bool {
    config
        .and_then(|config| config.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

/// `nextProviderConfigWithFieldValue`: `None` when the config ends up empty.
pub fn next_provider_config_with_field_value(
    config: Option<&Value>,
    field: &ProviderSettingsFieldModel,
    value: FieldValue<'_>,
) -> Option<Value> {
    let mut base: Map<String, Value> = config
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    match value {
        FieldValue::Bool(value) => {
            let empty = field.default_boolean_value.unwrap_or(false);
            if field.clear_when_empty == ClearWhenEmpty::Omit && value == empty {
                base.remove(&field.key);
            } else {
                base.insert(field.key.clone(), Value::Bool(value));
            }
        }
        FieldValue::Text(value) => {
            let trimmed = value.trim();
            if field.clear_when_empty == ClearWhenEmpty::Omit && trimmed.is_empty() {
                base.remove(&field.key);
            } else {
                base.insert(field.key.clone(), Value::String(value.to_owned()));
            }
        }
    }
    (!base.is_empty()).then_some(Value::Object(base))
}

#[derive(Debug, Clone, Copy)]
pub enum FieldValue<'a> {
    Bool(bool),
    Text(&'a str),
}

/// `nextConfigBlobWithValue`: sets one key without dropping empty values.
pub fn next_config_blob_with_value(config: Option<&Value>, key: &str, value: Value) -> Value {
    let mut base: Map<String, Value> = config
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    base.insert(key.to_owned(), value);
    Value::Object(base)
}

pub fn read_config_custom_models(config: Option<&Value>) -> Vec<CustomModelDefinition> {
    config
        .and_then(|config| config.get("customModels"))
        .map(read_custom_model_entries)
        .unwrap_or_default()
}

pub fn read_config_string(config: Option<&Value>, key: &str) -> Option<String> {
    let value = config?.get(key)?.as_str()?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

// ── environment variables ────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentVariable {
    pub name: String,
    pub value: String,
    pub sensitive: bool,
    pub value_redacted: Option<bool>,
}

pub fn environment_from_value(value: Option<&Value>) -> Vec<EnvironmentVariable> {
    value
        .and_then(Value::as_array)
        .map(|variables| {
            variables
                .iter()
                .filter_map(|variable| {
                    Some(EnvironmentVariable {
                        name: variable.get("name")?.as_str()?.to_owned(),
                        value: variable
                            .get("value")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                        sensitive: variable
                            .get("sensitive")
                            .and_then(Value::as_bool)
                            .unwrap_or(true),
                        value_redacted: variable.get("valueRedacted").and_then(Value::as_bool),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn environment_to_value(environment: &[EnvironmentVariable]) -> Value {
    Value::Array(
        environment
            .iter()
            .map(|variable| {
                let mut object = Map::new();
                object.insert("name".into(), Value::String(variable.name.clone()));
                object.insert("value".into(), Value::String(variable.value.clone()));
                object.insert("sensitive".into(), Value::Bool(variable.sensitive));
                if let Some(redacted) = variable.value_redacted {
                    object.insert("valueRedacted".into(), Value::Bool(redacted));
                }
                Value::Object(object)
            })
            .collect(),
    )
}

pub fn read_provider_environment_variable<'a>(
    environment: &'a [EnvironmentVariable],
    name: &str,
) -> Option<&'a EnvironmentVariable> {
    environment.iter().find(|variable| variable.name == name)
}

pub fn provider_environment_without_names(
    environment: &[EnvironmentVariable],
    names: &HashSet<&str>,
) -> Vec<EnvironmentVariable> {
    environment
        .iter()
        .filter(|variable| !names.contains(variable.name.as_str()))
        .cloned()
        .collect()
}

/// The draft a published environment leaves behind. The host writes each
/// sensitive value aside and echoes back an empty value with `valueRedacted`
/// (`settingsSecrets.ts`); the app keeps the same shape, so no plaintext secret
/// stays in the view model once it is saved.
pub fn redact_saved_environment(environment: &[EnvironmentVariable]) -> Vec<EnvironmentVariable> {
    environment
        .iter()
        .map(|variable| {
            if variable.sensitive && !variable.value.is_empty() {
                EnvironmentVariable {
                    value: String::new(),
                    value_redacted: Some(true),
                    ..variable.clone()
                }
            } else {
                variable.clone()
            }
        })
        .collect()
}

/// `nextProviderEnvironmentWithFieldValue`.
pub fn next_provider_environment_with_field_value(
    environment: &[EnvironmentVariable],
    field: &ProviderEnvironmentField,
    value: &str,
) -> Vec<EnvironmentVariable> {
    let trimmed = value.trim();
    let mut next = Vec::new();
    let mut found = false;
    for variable in environment {
        if variable.name != field.name {
            next.push(variable.clone());
            continue;
        }
        found = true;
        if !trimmed.is_empty() {
            next.push(EnvironmentVariable {
                name: variable.name.clone(),
                value: trimmed.to_owned(),
                sensitive: field.is_sensitive(),
                value_redacted: None,
            });
        }
    }
    if !found && !trimmed.is_empty() {
        next.push(EnvironmentVariable {
            name: field.name.to_owned(),
            value: trimmed.to_owned(),
            sensitive: field.is_sensitive(),
            value_redacted: None,
        });
    }
    next
}

/// `ENVIRONMENT_VARIABLE_NAME_PATTERN`: `/^[a-zA-Z_][a-zA-Z0-9_]*$/`.
pub fn is_valid_environment_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ── models ───────────────────────────────────────────────────────────

/// `deriveProviderModelsForDisplay`: built-ins from the live probe, custom
/// rows from current settings so edits show instantly.
pub fn derive_provider_models_for_display(
    live_models: Option<&[ServerProviderModel]>,
    custom_models: &[CustomModelDefinition],
) -> Vec<ServerProviderModel> {
    let live = live_models.unwrap_or_default();
    let server_models: Vec<ServerProviderModel> = live
        .iter()
        .filter(|model| !model.is_custom)
        .cloned()
        .collect();
    let server_slugs: HashSet<&str> = server_models.iter().map(|m| m.slug.as_str()).collect();
    let mut models = server_models.clone();
    for entry in custom_models {
        // A custom entry with a built-in slug only renames that built-in.
        if server_slugs.contains(entry.slug.as_str()) {
            continue;
        }
        let capabilities = entry.capabilities.clone().or_else(|| {
            live.iter()
                .find(|model| model.is_custom && model.slug.as_str() == entry.slug)
                .and_then(|model| model.capabilities.clone())
        });
        models.push(ServerProviderModel {
            slug: turin_contracts::TrimmedNonEmptyString::new(&entry.slug)
                .expect("custom slugs are trimmed and non-empty"),
            name: turin_contracts::TrimmedNonEmptyString::new(&entry.name)
                .expect("custom names are trimmed and non-empty"),
            short_name: None,
            sub_provider: None,
            aliases: None,
            badge: None,
            is_custom: true,
            is_default: None,
            is_legacy: None,
            capabilities,
        });
    }
    models
}

/// `groupModelsForDisplay`: favorites first (in user order), then visible
/// models, then hidden ones.
pub fn group_models_for_display(
    models: &[ServerProviderModel],
    favorite_models: &HashSet<String>,
    hidden_models: &HashSet<String>,
    model_order: &[String],
) -> Vec<ServerProviderModel> {
    let ordered = sort_models_for_provider_instance(
        models.iter().collect::<Vec<_>>(),
        SortModelsOptions {
            model_order,
            favorite_models: Some(favorite_models),
            group_favorites: true,
        },
    );
    let is_hidden = |model: &ServerProviderModel| {
        !model.is_custom && hidden_models.contains(model.slug.as_str())
    };
    let is_favorite = |model: &ServerProviderModel| favorite_models.contains(model.slug.as_str());
    ordered
        .iter()
        .filter(|model| is_favorite(model))
        .chain(
            ordered
                .iter()
                .filter(|model| !is_favorite(model) && !is_hidden(model)),
        )
        .chain(
            ordered
                .iter()
                .filter(|model| !is_favorite(model) && is_hidden(model)),
        )
        .map(|model| (*model).clone())
        .collect()
}

/// `nextHiddenModelsForBulkToggle`.
pub fn next_hidden_models_for_bulk_toggle(
    models: &[ServerProviderModel],
    hidden_models: &[String],
) -> Vec<String> {
    let built_in: Vec<&str> = models
        .iter()
        .filter(|model| !model.is_custom)
        .map(|model| model.slug.as_str())
        .collect();
    let all_hidden = built_in
        .iter()
        .all(|slug| hidden_models.iter().any(|hidden| hidden == slug));
    if all_hidden {
        return hidden_models
            .iter()
            .filter(|slug| !built_in.contains(&slug.as_str()))
            .cloned()
            .collect();
    }
    let mut next: Vec<String> = hidden_models.to_vec();
    for slug in built_in {
        if !next.iter().any(|existing| existing == slug) {
            next.push(slug.to_owned());
        }
    }
    next
}

/// `describeModelCapabilities`: short capability words after a model's slug.
pub fn describe_model_capabilities(model: &ServerProviderModel) -> Vec<&'static str> {
    use turin_contracts::model::ProviderOptionDescriptor as D;
    let descriptors = model
        .capabilities
        .as_ref()
        .and_then(|capabilities| capabilities.option_descriptors.as_deref())
        .unwrap_or_default();
    let mut labels = Vec::new();
    let id_of = |descriptor: &D| match descriptor {
        D::Select(select) => select.id.as_str().to_owned(),
        D::Boolean(boolean) => boolean.id.as_str().to_owned(),
    };
    let has_fast = descriptors.iter().any(|descriptor| {
        id_of(descriptor) == "fastMode"
            || matches!(descriptor, D::Select(select)
                if select.id.as_str() == "serviceTier"
                    && select.options.iter().any(|option| option.id.as_str() == "fast" || option.label.as_str() == "Fast"))
    });
    if has_fast {
        labels.push("Fast mode");
    }
    if descriptors
        .iter()
        .any(|descriptor| id_of(descriptor) == "thinking")
    {
        labels.push("Thinking");
    }
    if descriptors.iter().any(|descriptor| {
        matches!(descriptor, D::Select(select)
            if matches!(select.id.as_str(), "reasoningEffort" | "effort" | "reasoning" | "variant"))
    }) {
        labels.push("Reasoning");
    }
    labels
}

/// `modelListChanges`: which models were added to / removed from a list.
pub fn model_list_changes(current: &[String], next: &[String]) -> Vec<(String, bool)> {
    let current_set: HashSet<&String> = current.iter().collect();
    let next_set: HashSet<&String> = next.iter().collect();
    let mut changes = Vec::new();
    for model in next {
        if !current_set.contains(model) {
            changes.push((model.clone(), true));
        }
    }
    for model in current {
        if !next_set.contains(model) {
            changes.push((model.clone(), false));
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    fn model(slug: &str, is_custom: bool) -> ServerProviderModel {
        serde_json::from_value(json!({
            "slug": slug, "name": slug, "isCustom": is_custom, "capabilities": null
        }))
        .unwrap()
    }

    fn slugs(models: &[ServerProviderModel]) -> Vec<&str> {
        models.iter().map(|model| model.slug.as_str()).collect()
    }

    fn keys(driver: &str) -> Vec<String> {
        derive_provider_settings_fields(driver)
            .into_iter()
            .map(|field| field.key)
            .collect()
    }

    #[test]
    fn derives_visible_provider_config_fields_from_the_definition() {
        assert_eq!(
            keys("codex"),
            vec!["binaryPath", "homePath", "shadowHomePath", "launchArgs"]
        );
    }

    #[test]
    fn sources_labels_and_descriptions_from_schema_annotations() {
        let field = derive_provider_settings_fields("opencode")
            .into_iter()
            .find(|field| field.key == "serverPassword")
            .unwrap();
        assert_eq!(field.label, "Server password");
        assert_eq!(
            field.description.as_deref(),
            Some("Stored in plain text on disk.")
        );
        assert_eq!(field.control, ProviderSettingsFormControl::Password);
    }

    #[test]
    fn uses_a_dedicated_environment_field_instead_of_legacy_cursor_cli_settings() {
        assert!(keys("cursor").is_empty());
        assert_eq!(
            environment_fields_for_driver("cursor"),
            &[ProviderEnvironmentField {
                name: "CURSOR_API_KEY",
                label: "Cursor API key",
                description: "Optional. Overrides browser sign-in for this provider.",
                placeholder: "Paste API key",
                sensitive: Some(true),
            }]
        );
    }

    #[test]
    fn shows_the_auto_compaction_threshold_for_claude_providers() {
        assert_eq!(
            keys("claudeAgent"),
            vec!["binaryPath", "homePath", "autoCompactWindow", "launchArgs"]
        );
    }

    fn switch_field(clear: ClearWhenEmpty, default: Option<bool>) -> ProviderSettingsFieldModel {
        ProviderSettingsFieldModel {
            key: "experimental".into(),
            control: ProviderSettingsFormControl::Switch,
            label: "Experimental".into(),
            description: None,
            placeholder: None,
            clear_when_empty: clear,
            secret: false,
            default_boolean_value: default,
            options: None,
        }
    }

    #[test]
    fn preserves_unknown_config_keys_while_omitting_empty_configurable_fields() {
        let field = derive_provider_settings_fields("opencode")
            .into_iter()
            .find(|field| field.key == "serverUrl")
            .unwrap();
        let next = next_provider_config_with_field_value(
            Some(&json!({ "forkOwned": 1, "serverUrl": "http://127.0.0.1:4096" })),
            &field,
            FieldValue::Text(""),
        );
        assert_eq!(next, Some(json!({ "forkOwned": 1 })));
    }

    #[test]
    fn omits_false_boolean_fields_when_clear_when_empty_is_omit() {
        let next = next_provider_config_with_field_value(
            Some(&json!({ "forkOwned": 1, "experimental": true })),
            &switch_field(ClearWhenEmpty::Omit, Some(false)),
            FieldValue::Bool(false),
        );
        assert_eq!(next, Some(json!({ "forkOwned": 1 })));
    }

    #[test]
    fn omits_true_boolean_fields_when_true_is_the_default() {
        let next = next_provider_config_with_field_value(
            Some(&json!({ "forkOwned": 1, "experimental": false })),
            &switch_field(ClearWhenEmpty::Omit, Some(true)),
            FieldValue::Bool(true),
        );
        assert_eq!(next, Some(json!({ "forkOwned": 1 })));
    }

    #[test]
    fn stores_false_boolean_fields_when_true_is_the_default() {
        let next = next_provider_config_with_field_value(
            None,
            &switch_field(ClearWhenEmpty::Omit, Some(true)),
            FieldValue::Bool(false),
        );
        assert_eq!(next, Some(json!({ "experimental": false })));
    }

    #[test]
    fn preserves_false_boolean_fields_when_clear_when_empty_is_persist() {
        let next = next_provider_config_with_field_value(
            None,
            &switch_field(ClearWhenEmpty::Persist, None),
            FieldValue::Bool(false),
        );
        assert_eq!(next, Some(json!({ "experimental": false })));
    }

    #[test]
    fn lists_favorites_first_then_visible_models_in_user_order_then_hidden_ones() {
        let models = vec![
            model("a", false),
            model("b", false),
            model("c", false),
            model("d", false),
            model("custom", true),
        ];
        let display = group_models_for_display(
            &models,
            &HashSet::from(["c".to_owned()]),
            &HashSet::from(["a".to_owned(), "custom".to_owned()]),
            &["d".to_owned(), "b".to_owned()],
        );
        // A custom model is never hidden, even if its slug is in the hidden set.
        assert_eq!(slugs(&display), vec!["c", "d", "b", "custom", "a"]);
    }

    #[test]
    fn hides_every_built_in_model_without_hiding_custom_models() {
        let models = vec![model("a", false), model("b", false), model("custom", true)];
        assert_eq!(
            next_hidden_models_for_bulk_toggle(&models, &["a".to_owned()]),
            vec!["a", "b"]
        );
    }

    #[test]
    fn shows_every_built_in_model_while_preserving_unrelated_hidden_entries() {
        let models = vec![model("a", false), model("b", false), model("custom", true)];
        let hidden: Vec<String> = ["a", "b", "legacy", "custom"].map(str::to_owned).to_vec();
        assert_eq!(
            next_hidden_models_for_bulk_toggle(&models, &hidden),
            vec!["legacy", "custom"]
        );
    }

    #[test]
    fn custom_rows_come_from_settings_and_skip_built_in_slugs() {
        let live = vec![model("a", false), model("live-custom", true)];
        let custom = vec![
            CustomModelDefinition {
                slug: "a".into(),
                name: "Renamed".into(),
                capabilities: None,
            },
            CustomModelDefinition {
                slug: "mine".into(),
                name: "mine".into(),
                capabilities: None,
            },
        ];
        let display = derive_provider_models_for_display(Some(&live), &custom);
        assert_eq!(slugs(&display), vec!["a", "mine"]);
        assert!(display[1].is_custom);
    }

    #[test]
    fn environment_fields_set_replace_and_clear() {
        let field = &CURSOR_ENVIRONMENT_FIELDS[0];
        let env = vec![EnvironmentVariable {
            name: "OTHER".into(),
            value: "1".into(),
            sensitive: false,
            value_redacted: None,
        }];
        let with_key = next_provider_environment_with_field_value(&env, field, "  secret ");
        assert_eq!(with_key.len(), 2);
        assert_eq!(with_key[1].value, "secret");
        assert!(with_key[1].sensitive);
        let cleared = next_provider_environment_with_field_value(&with_key, field, " ");
        assert_eq!(cleared, env);
        assert!(is_valid_environment_variable_name("FOO_1"));
        assert!(!is_valid_environment_variable_name("1FOO"));
    }

    #[test]
    fn saved_environment_keeps_no_sensitive_plaintext() {
        let env = vec![
            EnvironmentVariable {
                name: "KEY".into(),
                value: "secret".into(),
                sensitive: true,
                value_redacted: Some(false),
            },
            EnvironmentVariable {
                name: "PLAIN".into(),
                value: "1".into(),
                sensitive: false,
                value_redacted: None,
            },
            EnvironmentVariable {
                name: "SAVED".into(),
                value: String::new(),
                sensitive: true,
                value_redacted: Some(true),
            },
        ];
        assert_eq!(
            redact_saved_environment(&env),
            vec![
                EnvironmentVariable {
                    name: "KEY".into(),
                    value: String::new(),
                    sensitive: true,
                    value_redacted: Some(true),
                },
                env[1].clone(),
                env[2].clone(),
            ]
        );
    }

    #[test]
    fn dedicated_secret_is_written_as_a_sensitive_env_var() {
        // `writes dedicated provider secrets as sensitive environment variables`
        let env = vec![EnvironmentVariable {
            name: "EXTRA_FLAG".into(),
            value: "1".into(),
            sensitive: false,
            value_redacted: None,
        }];
        assert_eq!(
            next_provider_environment_with_field_value(
                &env,
                &CURSOR_ENVIRONMENT_FIELDS[0],
                "  cursor-key  "
            ),
            vec![
                env[0].clone(),
                EnvironmentVariable {
                    name: "CURSOR_API_KEY".into(),
                    value: "cursor-key".into(),
                    sensitive: true,
                    value_redacted: None,
                },
            ]
        );
    }

    #[test]
    fn replacing_a_redacted_secret_drops_the_marker() {
        // `replaces redacted provider secrets without preserving redaction markers`
        let env = vec![EnvironmentVariable {
            name: "CURSOR_API_KEY".into(),
            value: String::new(),
            sensitive: true,
            value_redacted: Some(true),
        }];
        assert_eq!(
            next_provider_environment_with_field_value(
                &env,
                &CURSOR_ENVIRONMENT_FIELDS[0],
                "new-key"
            ),
            vec![EnvironmentVariable {
                name: "CURSOR_API_KEY".into(),
                value: "new-key".into(),
                sensitive: true,
                value_redacted: None,
            }]
        );
    }

    #[test]
    fn unset_sensitive_defaults_to_secure() {
        // `applies the secure field default when replacing an existing non-sensitive value`
        let field = ProviderEnvironmentField {
            name: "OPENAI_API_KEY",
            label: "OpenAI API key",
            description: "",
            placeholder: "",
            sensitive: None,
        };
        let env = vec![EnvironmentVariable {
            name: "OPENAI_API_KEY".into(),
            value: "old-key".into(),
            sensitive: false,
            value_redacted: None,
        }];
        assert_eq!(
            next_provider_environment_with_field_value(&env, &field, "new-key"),
            vec![EnvironmentVariable {
                name: "OPENAI_API_KEY".into(),
                value: "new-key".into(),
                sensitive: true,
                value_redacted: None,
            }]
        );
    }

    #[test]
    fn dedicated_secrets_are_separated_from_the_generic_table() {
        // `separates dedicated provider secrets from the generic environment table`
        let env = vec![
            EnvironmentVariable {
                name: "CURSOR_API_KEY".into(),
                value: "cursor-key".into(),
                sensitive: true,
                value_redacted: None,
            },
            EnvironmentVariable {
                name: "EXTRA_FLAG".into(),
                value: "1".into(),
                sensitive: false,
                value_redacted: None,
            },
        ];
        assert_eq!(
            read_provider_environment_variable(&env, "CURSOR_API_KEY")
                .map(|variable| variable.value.as_str()),
            Some("cursor-key")
        );
        assert_eq!(
            provider_environment_without_names(&env, &HashSet::from(["CURSOR_API_KEY"])),
            vec![env[1].clone()]
        );
    }

    #[test]
    fn describes_capabilities_and_model_list_changes() {
        let mut with_caps = model("m", false);
        with_caps.capabilities = Some(
            serde_json::from_value(json!({ "optionDescriptors": [
                { "id": "effort", "label": "Reasoning", "type": "select", "options": [{ "id": "low", "label": "Low" }] },
                { "id": "fastMode", "label": "Fast", "type": "boolean" },
                { "id": "thinking", "label": "Thinking", "type": "boolean" }
            ]}))
            .unwrap(),
        );
        assert_eq!(
            describe_model_capabilities(&with_caps),
            vec!["Fast mode", "Thinking", "Reasoning"]
        );
        assert_eq!(
            model_list_changes(&["a".into(), "b".into()], &["b".into(), "c".into()]),
            vec![("c".to_owned(), true), ("a".to_owned(), false)]
        );
    }
}
