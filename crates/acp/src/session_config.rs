//! Port of upstream/packages/provider-acp/src/server/sessionConfig.ts
//!
//! Maps ACP session configuration and session modes onto T3's provider option
//! descriptors so the existing model-options UI can drive them.
//!
//! Model selection stays on the dedicated model picker: `category: "model"`
//! options are excluded here because they already surface as models. Session
//! modes advertised through the modes API (rather than a config option) are
//! folded into one synthetic descriptor that the ACP adapter routes to
//! `session/set_mode`.

use std::collections::HashSet;

use serde::Deserialize;
use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::base_schemas::{js_len, js_trim};
use turin_contracts::model::{
    BooleanOptionType, BooleanProviderOptionDescriptor, ProviderOptionChoice,
    ProviderOptionDescriptor, SelectOptionType, SelectProviderOptionDescriptor,
};

use crate::runtime_model::AcpSessionModeState;

/// Synthetic descriptor ID for agents that expose modes outside config options.
pub const ACP_SESSION_MODE_OPTION_ID: &str = "_t3/session-mode";

/// Choice ID for a select value that is exactly `""`. Agents use it for a real
/// choice (GitHub Copilot's default agent), but T3 option IDs must be
/// non-empty. The ACP adapter maps it back to `""` before applying a selection.
pub const ACP_EMPTY_OPTION_VALUE: &str = "_t3/empty";

const MAX_OPTION_DESCRIPTORS: usize = 16;
const MAX_OPTION_CHOICES: usize = 64;
const MAX_TEXT_LENGTH: usize = 256;
const MAX_DESCRIPTION_LENGTH: usize = 1_024;

/// One value of an ACP select option (`SessionConfigSelectOption`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigSelectOption {
    pub value: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// A named group of select values (`SessionConfigSelectGroup`). Only the
/// options are read; the group id never reaches a descriptor.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SessionConfigSelectGroup {
    pub options: Vec<SessionConfigSelectOption>,
}

/// Flat or grouped select values. Untagged on purpose: a group has no `value`
/// key, so an entry is a group exactly when it is not an option.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum SessionConfigSelectEntry {
    Flat(SessionConfigSelectOption),
    Group(SessionConfigSelectGroup),
}

/// `SessionConfigOption`, discriminated by `type`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SessionConfigOption {
    Select {
        id: String,
        name: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        category: Option<String>,
        current_value: String,
        options: Vec<SessionConfigSelectEntry>,
    },
    Boolean {
        id: String,
        name: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        category: Option<String>,
        current_value: bool,
    },
}

impl SessionConfigOption {
    pub fn id(&self) -> &str {
        match self {
            Self::Select { id, .. } | Self::Boolean { id, .. } => id,
        }
    }

    pub fn category(&self) -> Option<&str> {
        match self {
            Self::Select { category, .. } | Self::Boolean { category, .. } => category.as_deref(),
        }
    }
}

/// `String(value).trim().slice(0, max)` in UTF-16 units.
fn bounded_text(value: Option<&str>, maximum_length: usize) -> String {
    js_prefix(js_trim(value.unwrap_or("")), maximum_length).to_owned()
}

/// First `maximum_units` UTF-16 code units of `value`, cut on a char boundary.
fn js_prefix(value: &str, maximum_units: usize) -> &str {
    let mut units = 0;
    for (index, c) in value.char_indices() {
        units += c.len_utf16();
        if units > maximum_units {
            return &value[..index];
        }
    }
    value
}

/// A value the wire would not mutate: non-empty, already trimmed, and within the length cap.
fn bounded_opaque_value(value: &str, maximum_length: usize) -> Option<&str> {
    (!value.is_empty() && value == js_trim(value) && js_len(value) <= maximum_length)
        .then_some(value)
}

fn choice_id(value: &str) -> Option<String> {
    if value.is_empty() {
        Some(ACP_EMPTY_OPTION_VALUE.to_owned())
    } else {
        bounded_opaque_value(value, MAX_TEXT_LENGTH).map(str::to_owned)
    }
}

/// Only the value/name/description triple of a select value matters here.
struct Candidate<'a> {
    value: &'a str,
    name: &'a str,
    description: Option<&'a str>,
}

fn flatten_select_choices(options: &[SessionConfigSelectEntry]) -> Vec<Candidate<'_>> {
    options
        .iter()
        .flat_map(|entry| match entry {
            SessionConfigSelectEntry::Flat(option) => vec![option],
            SessionConfigSelectEntry::Group(group) => group.options.iter().collect(),
        })
        .map(|option| Candidate {
            value: &option.value,
            name: &option.name,
            description: option.description.as_deref(),
        })
        .collect()
}

fn select_choices<'a>(
    candidates: impl IntoIterator<Item = Candidate<'a>>,
) -> Vec<ProviderOptionChoice> {
    let mut seen = HashSet::new();
    let mut choices = Vec::new();
    for candidate in candidates {
        let Some(id) = choice_id(candidate.value) else {
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        let description = bounded_text(candidate.description, MAX_DESCRIPTION_LENGTH);
        let label = bounded_text(Some(candidate.name), MAX_TEXT_LENGTH);
        choices.push(ProviderOptionChoice {
            id: trimmed(&id),
            label: trimmed(if label.is_empty() { &id } else { &label }),
            description: (!description.is_empty()).then(|| trimmed(&description)),
            is_default: None,
        });
        if choices.len() == MAX_OPTION_CHOICES {
            break;
        }
    }
    choices
}

/// Values reaching this point are trimmed and non-empty by construction.
fn trimmed(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).expect("bounded option text is non-empty")
}

fn current_choice(
    current: &str,
    choices: &[ProviderOptionChoice],
) -> Option<TrimmedNonEmptyString> {
    let current = choice_id(current)?;
    choices
        .iter()
        .any(|choice| choice.id.as_str() == current)
        .then(|| trimmed(&current))
}

pub fn acp_provider_option_descriptors(
    config_options: Option<&[SessionConfigOption]>,
    mode_state: Option<&AcpSessionModeState>,
) -> Vec<ProviderOptionDescriptor> {
    let mut descriptors: Vec<ProviderOptionDescriptor> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut mirrored_mode_choice_sets: Vec<HashSet<String>> = Vec::new();
    let mut has_mode_category = false;

    for option in config_options.unwrap_or(&[]) {
        // "model" options surface as the model list; "collaboration_mode" options
        // are driven by T3's own plan/build interaction mode in the ACP adapter.
        if matches!(option.category(), Some("model" | "collaboration_mode")) {
            continue;
        }
        let Some(id) = bounded_opaque_value(option.id(), MAX_TEXT_LENGTH).map(str::to_owned) else {
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        let (name, description, category) = match option {
            SessionConfigOption::Select {
                name,
                description,
                category,
                ..
            }
            | SessionConfigOption::Boolean {
                name,
                description,
                category,
                ..
            } => (name, description, category),
        };
        let description = bounded_text(description.as_deref(), MAX_DESCRIPTION_LENGTH);
        let label = bounded_text(Some(name), MAX_TEXT_LENGTH);
        let label = trimmed(if label.is_empty() { &id } else { &label });
        let description = (!description.is_empty()).then(|| trimmed(&description));
        let is_mode = category.as_deref() == Some("mode");

        match option {
            SessionConfigOption::Boolean { current_value, .. } => {
                if is_mode {
                    has_mode_category = true;
                }
                descriptors.push(ProviderOptionDescriptor::Boolean(
                    BooleanProviderOptionDescriptor {
                        id: trimmed(&id),
                        label,
                        description,
                        type_: BooleanOptionType,
                        current_value: Some(*current_value),
                    },
                ));
                if descriptors.len() == MAX_OPTION_DESCRIPTORS {
                    return descriptors;
                }
            }
            SessionConfigOption::Select {
                current_value,
                options,
                ..
            } => {
                let choices = select_choices(flatten_select_choices(options));
                if choices.is_empty() {
                    seen.remove(&id);
                    continue;
                }
                if is_mode {
                    has_mode_category = true;
                }
                if category.as_deref() == Some("thought_level") {
                    mirrored_mode_choice_sets.push(
                        choices
                            .iter()
                            .map(|choice| choice.id.as_str().to_owned())
                            .collect(),
                    );
                }
                let current = current_choice(current_value, &choices);
                descriptors.push(ProviderOptionDescriptor::Select(
                    SelectProviderOptionDescriptor {
                        id: trimmed(&id),
                        label,
                        description,
                        type_: SelectOptionType,
                        options: choices,
                        current_value: current,
                        prompt_injected_values: None,
                    },
                ));
                if descriptors.len() == MAX_OPTION_DESCRIPTORS {
                    return descriptors;
                }
            }
        }
    }

    if let Some(mode_state) = mode_state.filter(|_| !has_mode_category) {
        let choices = select_choices(mode_state.available_modes.iter().map(|mode| Candidate {
            value: &mode.id,
            name: &mode.name,
            description: mode.description.as_deref(),
        }));
        let current = current_choice(&mode_state.current_mode_id, &choices);
        // Some agents mirror one knob through both the modes API and a config
        // option (codex-acp advertises its thinking levels as modes too). Skip
        // the synthetic descriptor when an existing descriptor already exposes
        // the same choice set, so the composer shows the knob once.
        let duplicates_known_mode_descriptor = mirrored_mode_choice_sets.iter().any(|choice_ids| {
            choice_ids.len() == choices.len()
                && choices
                    .iter()
                    .all(|choice| choice_ids.contains(choice.id.as_str()))
        });
        if choices.len() > 1
            && !duplicates_known_mode_descriptor
            && !seen.contains(ACP_SESSION_MODE_OPTION_ID)
        {
            descriptors.push(ProviderOptionDescriptor::Select(
                SelectProviderOptionDescriptor {
                    id: trimmed(ACP_SESSION_MODE_OPTION_ID),
                    label: trimmed("Mode"),
                    description: Some(trimmed("Session mode advertised by the ACP agent.")),
                    type_: SelectOptionType,
                    options: choices,
                    current_value: current,
                    prompt_injected_values: None,
                },
            ));
        }
    }

    descriptors.truncate(MAX_OPTION_DESCRIPTORS);
    descriptors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_model::{AcpSessionMode, AcpSessionModeState};
    use serde_json::{Value, json};

    fn options(value: Value) -> Vec<SessionConfigOption> {
        serde_json::from_value(value).expect("config options")
    }

    fn descriptors(
        config_options: Option<Vec<SessionConfigOption>>,
        mode_state: Option<AcpSessionModeState>,
    ) -> Value {
        serde_json::to_value(acp_provider_option_descriptors(
            config_options.as_deref(),
            mode_state.as_ref(),
        ))
        .expect("descriptors serialize")
    }

    fn mode(id: &str, name: &str, description: Option<&str>) -> AcpSessionMode {
        AcpSessionMode {
            id: id.to_owned(),
            name: name.to_owned(),
            description: description.map(str::to_owned),
        }
    }

    #[test]
    fn maps_non_model_select_options_and_excludes_model_and_collaboration_categories() {
        let config = options(json!([
            {
                "id": "model",
                "name": "Model",
                "category": "model",
                "type": "select",
                "currentValue": "gpt-5.6-sol",
                "options": [{ "value": "gpt-5.6-sol", "name": "GPT-5.6-Sol" }],
            },
            {
                "id": "collaboration_mode",
                "name": "Collaboration mode",
                "category": "collaboration_mode",
                "type": "select",
                "currentValue": "default",
                "options": [
                    { "value": "default", "name": "Default" },
                    { "value": "plan", "name": "Plan" },
                ],
            },
            {
                "id": "reasoning_effort",
                "name": "Reasoning effort",
                "category": "thought_level",
                "type": "select",
                "currentValue": "medium",
                "options": [
                    { "value": "low", "name": "Low" },
                    { "value": "medium", "name": "Medium", "description": "Balanced" },
                ],
            },
        ]));

        assert_eq!(
            descriptors(Some(config), None),
            json!([{
                "id": "reasoning_effort",
                "label": "Reasoning effort",
                "type": "select",
                "options": [
                    { "id": "low", "label": "Low" },
                    { "id": "medium", "label": "Medium", "description": "Balanced" },
                ],
                "currentValue": "medium",
            }])
        );
    }

    #[test]
    fn maps_boolean_session_options() {
        let config = options(json!([{
            "id": "fast_mode",
            "name": "Fast mode",
            "description": "Use lower-latency inference.",
            "type": "boolean",
            "currentValue": true,
        }]));

        assert_eq!(
            descriptors(Some(config), None),
            json!([{
                "id": "fast_mode",
                "label": "Fast mode",
                "description": "Use lower-latency inference.",
                "type": "boolean",
                "currentValue": true,
            }])
        );
    }

    #[test]
    fn flattens_grouped_select_choices_and_drops_empty_or_duplicate_values() {
        let config = options(json!([{
            "id": "sandbox",
            "name": "Sandbox",
            "type": "select",
            "currentValue": "workspace",
            "options": [
                {
                    "groupId": "standard",
                    "name": "Standard",
                    "options": [
                        { "value": "workspace", "name": "Workspace" },
                        { "value": " ", "name": "Blank" },
                    ],
                },
                {
                    "groupId": "extra",
                    "name": "Extra",
                    "options": [
                        { "value": "workspace", "name": "Duplicate" },
                        { "value": "full", "name": "Full" },
                    ],
                },
            ],
        }]));

        assert_eq!(
            descriptors(Some(config), None),
            json!([{
                "id": "sandbox",
                "label": "Sandbox",
                "type": "select",
                "options": [
                    { "id": "workspace", "label": "Workspace" },
                    { "id": "full", "label": "Full" },
                ],
                "currentValue": "workspace",
            }])
        );
    }

    #[test]
    fn synthesizes_a_mode_descriptor_for_agents_without_a_mode_config_option() {
        let descriptors = descriptors(
            Some(Vec::new()),
            Some(AcpSessionModeState {
                current_mode_id: "safe".to_owned(),
                available_modes: vec![
                    mode("safe", "Safe", None),
                    mode("yolo", "YOLO", Some("No approvals")),
                ],
            }),
        );

        assert_eq!(
            descriptors,
            json!([{
                "id": ACP_SESSION_MODE_OPTION_ID,
                "label": "Mode",
                "description": "Session mode advertised by the ACP agent.",
                "type": "select",
                "options": [
                    { "id": "safe", "label": "Safe" },
                    { "id": "yolo", "label": "YOLO", "description": "No approvals" },
                ],
                "currentValue": "safe",
            }])
        );
    }

    #[test]
    fn suppresses_the_synthetic_mode_descriptor_when_modes_duplicate_a_config_option() {
        let config = options(json!([{
            "id": "thinking",
            "name": "Thinking",
            "category": "thought_level",
            "type": "select",
            "currentValue": "medium",
            "options": [
                { "value": "off", "name": "Thinking: off" },
                { "value": "medium", "name": "Thinking: medium" },
            ],
        }]));
        let descriptors = acp_provider_option_descriptors(
            Some(&config),
            Some(&AcpSessionModeState {
                current_mode_id: "medium".to_owned(),
                available_modes: vec![
                    mode("off", "Thinking: off", None),
                    mode("medium", "Thinking: medium", None),
                ],
            }),
        );

        let ids: Vec<&str> = descriptors
            .iter()
            .map(|descriptor| match descriptor {
                ProviderOptionDescriptor::Select(select) => select.id.as_str(),
                ProviderOptionDescriptor::Boolean(boolean) => boolean.id.as_str(),
            })
            .collect();
        assert_eq!(ids, vec!["thinking"]);
    }

    #[test]
    fn does_not_let_an_unrelated_select_option_hide_the_session_modes_api() {
        let config = options(json!([{
            "id": "unrelated",
            "name": "Unrelated",
            "type": "select",
            "currentValue": "safe",
            "options": [
                { "value": "safe", "name": "Safe" },
                { "value": "yolo", "name": "YOLO" },
            ],
        }]));
        let descriptors = acp_provider_option_descriptors(
            Some(&config),
            Some(&AcpSessionModeState {
                current_mode_id: "safe".to_owned(),
                available_modes: vec![mode("safe", "Safe", None), mode("yolo", "YOLO", None)],
            }),
        );

        let ids: Vec<String> = descriptors
            .iter()
            .map(|descriptor| match descriptor {
                ProviderOptionDescriptor::Select(select) => select.id.as_str().to_owned(),
                ProviderOptionDescriptor::Boolean(boolean) => boolean.id.as_str().to_owned(),
            })
            .collect();
        assert_eq!(ids, vec!["unrelated", ACP_SESSION_MODE_OPTION_ID]);
    }

    #[test]
    fn preserves_canonical_opaque_option_values_and_omits_values_the_wire_would_mutate() {
        let exact_value = "value:with:opaque-markers";
        let config = options(json!([{
            "id": "opaque",
            "name": "Opaque",
            "type": "select",
            "currentValue": exact_value,
            "options": [
                { "value": exact_value, "name": "Exact" },
                { "value": " value with spaces ", "name": "Would be trimmed" },
                { "value": "x".repeat(257), "name": "Too long" },
            ],
        }]));

        assert_eq!(
            descriptors(Some(config), None)[0],
            json!({
                "id": "opaque",
                "label": "Opaque",
                "type": "select",
                "options": [{ "id": exact_value, "label": "Exact" }],
                "currentValue": exact_value,
            })
        );
    }

    #[test]
    fn suppresses_the_synthetic_mode_descriptor_when_a_mode_category_option_exists() {
        let config = options(json!([{
            "id": "mode",
            "name": "Mode",
            "category": "mode",
            "type": "select",
            "currentValue": "agent",
            "options": [
                { "value": "read-only", "name": "Read-only" },
                { "value": "agent", "name": "Agent" },
            ],
        }]));
        let descriptors = acp_provider_option_descriptors(
            Some(&config),
            Some(&AcpSessionModeState {
                current_mode_id: "agent".to_owned(),
                available_modes: vec![
                    mode("read-only", "Read-only", None),
                    mode("agent", "Agent", None),
                    mode("agent-full-access", "Agent (full access)", None),
                ],
            }),
        );

        let ids: Vec<String> = descriptors
            .iter()
            .map(|descriptor| match descriptor {
                ProviderOptionDescriptor::Select(select) => select.id.as_str().to_owned(),
                ProviderOptionDescriptor::Boolean(boolean) => boolean.id.as_str().to_owned(),
            })
            .collect();
        assert_eq!(ids, vec!["mode"]);
    }
}
