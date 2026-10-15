//! Port of upstream/apps/web/src/components/settings/customModelEditor.logic.ts
//! (and its test file).
//!
//! The editor works on a plain mirror of `ProviderOptionDescriptor`; the
//! conversions go through JSON so the contract types keep owning validation.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use turin_contracts::model::{ModelCapabilities, ProviderOptionDescriptor};
use turin_shared::model::{CustomModelDefinition, create_model_capabilities};

/// Editable mirror of a `ProviderOptionChoice`. `key` is only a UI key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorChoice {
    pub key: String,
    pub id: String,
    pub label: String,
    pub is_default: bool,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorDescriptorType {
    Select,
    Boolean,
}

/// Editable mirror of a `ProviderOptionDescriptor`. `key` is only a UI key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorDescriptor {
    pub key: String,
    pub type_: EditorDescriptorType,
    pub id: String,
    pub label: String,
    pub choices: Vec<EditorChoice>,
    pub current_boolean_value: Option<bool>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomModelDraft {
    pub slug: String,
    pub name: String,
    pub descriptors: Vec<EditorDescriptor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetChoice {
    pub id: &'static str,
    pub label: &'static str,
    pub is_default: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DescriptorPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub type_: EditorDescriptorType,
    pub choices: &'static [PresetChoice],
}

const fn choice(id: &'static str, label: &'static str, is_default: bool) -> PresetChoice {
    PresetChoice {
        id,
        label,
        is_default,
    }
}

const EFFORT_CHOICES: &[PresetChoice] = &[
    choice("low", "Low", false),
    choice("medium", "Medium", true),
    choice("high", "High", false),
    choice("xhigh", "Extra High", false),
];

const CODEX_PRESETS: &[DescriptorPreset] = &[
    DescriptorPreset {
        id: "reasoningEffort",
        label: "Reasoning",
        type_: EditorDescriptorType::Select,
        choices: EFFORT_CHOICES,
    },
    DescriptorPreset {
        id: "serviceTier",
        label: "Speed",
        type_: EditorDescriptorType::Select,
        choices: &[choice("default", "Standard", true), choice("fast", "Fast", false)],
    },
];

const CLAUDE_PRESETS: &[DescriptorPreset] = &[
    DescriptorPreset {
        id: "effort",
        label: "Reasoning",
        type_: EditorDescriptorType::Select,
        choices: &[
            choice("low", "Low", false),
            choice("medium", "Medium", false),
            choice("high", "High", true),
            choice("xhigh", "Extra High", false),
            choice("max", "Max", false),
        ],
    },
    DescriptorPreset {
        id: "fastMode",
        label: "Fast Mode",
        type_: EditorDescriptorType::Boolean,
        choices: &[],
    },
    DescriptorPreset {
        id: "thinking",
        label: "Thinking",
        type_: EditorDescriptorType::Boolean,
        choices: &[],
    },
];

const CURSOR_PRESETS: &[DescriptorPreset] = &[
    DescriptorPreset {
        id: "reasoning",
        label: "Reasoning",
        type_: EditorDescriptorType::Select,
        choices: EFFORT_CHOICES,
    },
    DescriptorPreset {
        id: "fastMode",
        label: "Fast Mode",
        type_: EditorDescriptorType::Boolean,
        choices: &[],
    },
    DescriptorPreset {
        id: "thinking",
        label: "Thinking",
        type_: EditorDescriptorType::Boolean,
        choices: &[],
    },
];

const OPENCODE_PRESETS: &[DescriptorPreset] = &[
    DescriptorPreset {
        id: "variant",
        label: "Reasoning",
        type_: EditorDescriptorType::Select,
        choices: EFFORT_CHOICES,
    },
    DescriptorPreset {
        id: "agent",
        label: "Agent",
        type_: EditorDescriptorType::Select,
        choices: &[choice("build", "Build", true), choice("plan", "Plan", false)],
    },
];

/// `DESCRIPTOR_PRESETS_BY_KIND`: option ids each adapter actually reads off a
/// turn's model selection, with the usual choices pre-filled. Anything else the
/// user types is stored verbatim but will be ignored by the driver. The
/// grok and pi entries are omitted: those providers are cut.
pub const DESCRIPTOR_PRESETS_BY_KIND: &[(&str, &[DescriptorPreset])] = &[
    ("codex", CODEX_PRESETS),
    ("claudeAgent", CLAUDE_PRESETS),
    ("cursor", CURSOR_PRESETS),
    ("opencode", OPENCODE_PRESETS),
];

pub fn descriptor_presets_for_kind(kind: &str) -> &'static [DescriptorPreset] {
    DESCRIPTOR_PRESETS_BY_KIND
        .iter()
        .find(|(candidate, _)| *candidate == kind)
        .map(|(_, presets)| *presets)
        .unwrap_or(&[])
}

static NEXT_KEY: AtomicU64 = AtomicU64::new(0);

fn new_editor_key() -> String {
    format!("k{}", NEXT_KEY.fetch_add(1, Ordering::Relaxed) + 1)
}

pub fn choice_from_preset(choice: &PresetChoice) -> EditorChoice {
    EditorChoice {
        key: new_editor_key(),
        id: choice.id.to_owned(),
        label: choice.label.to_owned(),
        is_default: choice.is_default,
        description: None,
    }
}

pub fn descriptor_from_preset(preset: &DescriptorPreset) -> EditorDescriptor {
    EditorDescriptor {
        key: new_editor_key(),
        type_: preset.type_,
        id: preset.id.to_owned(),
        label: preset.label.to_owned(),
        choices: preset.choices.iter().map(choice_from_preset).collect(),
        current_boolean_value: None,
        description: None,
    }
}

pub fn empty_editor_descriptor() -> EditorDescriptor {
    EditorDescriptor {
        key: new_editor_key(),
        type_: EditorDescriptorType::Select,
        id: String::new(),
        label: String::new(),
        choices: Vec::new(),
        current_boolean_value: None,
        description: None,
    }
}

pub fn empty_editor_choice() -> EditorChoice {
    EditorChoice {
        key: new_editor_key(),
        id: String::new(),
        label: String::new(),
        is_default: false,
        description: None,
    }
}

/// Prompt-injected choices (Claude's `ultrathink`) are delivered as prompt text
/// by built-in runtime profiles a custom entry does not have, so they are
/// dropped rather than stored as a plain option value.
fn descriptor_to_editor(descriptor: &ProviderOptionDescriptor) -> EditorDescriptor {
    match descriptor {
        ProviderOptionDescriptor::Select(select) => {
            let prompt_injected: Vec<&str> = select
                .prompt_injected_values
                .iter()
                .flatten()
                .map(|value| value.as_str())
                .collect();
            let choices: Vec<_> = select
                .options
                .iter()
                .filter(|option| !prompt_injected.contains(&option.id.as_str()))
                .collect();
            let default_index = choices
                .iter()
                .position(|option| {
                    select.current_value.as_ref().map(|v| v.as_str()) == Some(option.id.as_str())
                })
                .or_else(|| choices.iter().position(|option| option.is_default == Some(true)));
            EditorDescriptor {
                key: new_editor_key(),
                type_: EditorDescriptorType::Select,
                id: select.id.as_str().to_owned(),
                label: select.label.as_str().to_owned(),
                description: select.description.as_ref().map(|d| d.as_str().to_owned()),
                current_boolean_value: None,
                choices: choices
                    .iter()
                    .enumerate()
                    .map(|(index, option)| EditorChoice {
                        key: new_editor_key(),
                        id: option.id.as_str().to_owned(),
                        label: option.label.as_str().to_owned(),
                        description: option.description.as_ref().map(|d| d.as_str().to_owned()),
                        is_default: Some(index) == default_index,
                    })
                    .collect(),
            }
        }
        ProviderOptionDescriptor::Boolean(boolean) => EditorDescriptor {
            key: new_editor_key(),
            type_: EditorDescriptorType::Boolean,
            id: boolean.id.as_str().to_owned(),
            label: boolean.label.as_str().to_owned(),
            description: boolean.description.as_ref().map(|d| d.as_str().to_owned()),
            current_boolean_value: boolean.current_value,
            choices: Vec::new(),
        },
    }
}

pub fn draft_from_definition(entry: &CustomModelDefinition) -> CustomModelDraft {
    CustomModelDraft {
        slug: entry.slug.clone(),
        name: if entry.name == entry.slug {
            String::new()
        } else {
            entry.name.clone()
        },
        descriptors: entry
            .capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.option_descriptors.as_ref())
            .map(|descriptors| descriptors.iter().map(descriptor_to_editor).collect())
            .unwrap_or_default(),
    }
}

/// Claude context choices require runtime suffix mappings that custom entries do not carry.
pub fn descriptors_from_capabilities(
    capabilities: Option<&ModelCapabilities>,
    driver_kind: Option<&str>,
) -> Vec<EditorDescriptor> {
    capabilities
        .and_then(|capabilities| capabilities.option_descriptors.as_ref())
        .map(|descriptors| {
            descriptors
                .iter()
                .filter(|descriptor| {
                    let id = match descriptor {
                        ProviderOptionDescriptor::Select(select) => select.id.as_str(),
                        ProviderOptionDescriptor::Boolean(boolean) => boolean.id.as_str(),
                    };
                    driver_kind != Some("claudeAgent") || id != "contextWindow"
                })
                .map(descriptor_to_editor)
                .collect()
        })
        .unwrap_or_default()
}

/// Validate the draft before saving. Returns the first problem in reading
/// order so the message is actionable, or `None` when the draft is sound.
pub fn validate_draft(draft: &CustomModelDraft) -> Option<String> {
    let mut seen_ids = std::collections::HashSet::new();
    for (index, descriptor) in draft.descriptors.iter().enumerate() {
        let position = format!("Option {}", index + 1);
        let id = descriptor.id.trim();
        if id.is_empty() {
            return Some(format!("{position} needs an id."));
        }
        if !seen_ids.insert(id.to_owned()) {
            return Some(format!("{position}: id \"{id}\" is used twice."));
        }
        if descriptor.label.trim().is_empty() {
            return Some(format!("{position} needs a label."));
        }
        if descriptor.type_ != EditorDescriptorType::Select {
            continue;
        }
        if descriptor.choices.is_empty() {
            return Some(format!("{position} needs at least one choice."));
        }
        let mut seen_choices = std::collections::HashSet::new();
        for choice in &descriptor.choices {
            let choice_id = choice.id.trim();
            if choice_id.is_empty() {
                return Some(format!("{position} has a choice without a value."));
            }
            if !seen_choices.insert(choice_id.to_owned()) {
                return Some(format!("{position}: choice \"{choice_id}\" is used twice."));
            }
        }
    }
    None
}

/// Convert a validated draft back into a definition. Blank name → slug.
pub fn definition_from_draft(draft: &CustomModelDraft) -> CustomModelDefinition {
    let descriptors: Vec<ProviderOptionDescriptor> = draft
        .descriptors
        .iter()
        .map(|descriptor| {
            let id = descriptor.id.trim();
            let label = descriptor.label.trim();
            let mut value = json!({ "id": id, "label": label });
            let object = value.as_object_mut().expect("object");
            if descriptor.type_ == EditorDescriptorType::Boolean {
                object.insert("type".into(), json!("boolean"));
                if let Some(description) = &descriptor.description {
                    object.insert("description".into(), json!(description));
                }
                if let Some(current) = descriptor.current_boolean_value {
                    object.insert("currentValue".into(), json!(current));
                }
            } else {
                object.insert("type".into(), json!("select"));
                if let Some(description) = &descriptor.description {
                    object.insert("description".into(), json!(description));
                }
                let options: Vec<Value> = descriptor
                    .choices
                    .iter()
                    .map(|choice| {
                        let choice_id = choice.id.trim();
                        let choice_label = choice.label.trim();
                        let mut option = json!({
                            "id": choice_id,
                            "label": if choice_label.is_empty() { choice_id } else { choice_label },
                        });
                        let option_object = option.as_object_mut().expect("object");
                        if let Some(description) = &choice.description {
                            option_object.insert("description".into(), json!(description));
                        }
                        if choice.is_default {
                            option_object.insert("isDefault".into(), json!(true));
                        }
                        option
                    })
                    .collect();
                let current_value = descriptor
                    .choices
                    .iter()
                    .find(|choice| choice.is_default)
                    .map(|choice| choice.id.trim().to_owned());
                object.insert("options".into(), Value::Array(options));
                if let Some(current_value) = current_value.filter(|value| !value.is_empty()) {
                    object.insert("currentValue".into(), json!(current_value));
                }
            }
            serde_json::from_value(value).expect("validated descriptor decodes")
        })
        .collect();
    let name = draft.name.trim();
    CustomModelDefinition {
        slug: draft.slug.clone(),
        name: if name.is_empty() {
            draft.slug.clone()
        } else {
            name.to_owned()
        },
        capabilities: (!descriptors.is_empty()).then(|| create_model_capabilities(&descriptors)),
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    fn draft(overrides: impl FnOnce(&mut CustomModelDraft)) -> CustomModelDraft {
        let mut draft = CustomModelDraft {
            slug: "my-model".into(),
            name: String::new(),
            descriptors: Vec::new(),
        };
        overrides(&mut draft);
        draft
    }

    fn caps(value: Value) -> ModelCapabilities {
        serde_json::from_value(value).unwrap()
    }

    fn caps_json(definition: &CustomModelDefinition) -> Value {
        serde_json::to_value(&definition.capabilities).unwrap()
    }

    fn select(id: &str, choices: &[&str]) -> EditorDescriptor {
        EditorDescriptor {
            key: id.into(),
            type_: EditorDescriptorType::Select,
            id: id.into(),
            label: "Label".into(),
            choices: choices
                .iter()
                .map(|choice| EditorChoice {
                    key: (*choice).into(),
                    id: (*choice).into(),
                    label: String::new(),
                    is_default: false,
                    description: None,
                })
                .collect(),
            current_boolean_value: None,
            description: None,
        }
    }

    #[test]
    fn round_trips_a_definition_through_the_draft_marking_the_current_value_as_default() {
        let definition = definition_from_draft(&draft(|d| {
            d.name = " My Model ".into();
            d.descriptors = vec![
                EditorDescriptor {
                    key: "a".into(),
                    type_: EditorDescriptorType::Select,
                    id: "reasoningEffort".into(),
                    label: "Reasoning".into(),
                    choices: vec![
                        EditorChoice {
                            key: "a1".into(),
                            id: "low".into(),
                            label: "Low".into(),
                            is_default: false,
                            description: None,
                        },
                        EditorChoice {
                            key: "a2".into(),
                            id: "high".into(),
                            label: String::new(),
                            is_default: true,
                            description: None,
                        },
                    ],
                    current_boolean_value: None,
                    description: None,
                },
                EditorDescriptor {
                    key: "b".into(),
                    type_: EditorDescriptorType::Boolean,
                    id: "fastMode".into(),
                    label: "Fast Mode".into(),
                    choices: Vec::new(),
                    current_boolean_value: None,
                    description: None,
                },
            ];
        }));

        assert_eq!(definition.slug, "my-model");
        assert_eq!(definition.name, "My Model");
        assert_eq!(
            caps_json(&definition),
            json!({
                "optionDescriptors": [
                    {
                        "id": "reasoningEffort",
                        "label": "Reasoning",
                        "type": "select",
                        "options": [
                            { "id": "low", "label": "Low" },
                            { "id": "high", "label": "high", "isDefault": true }
                        ],
                        "currentValue": "high"
                    },
                    { "id": "fastMode", "label": "Fast Mode", "type": "boolean" }
                ]
            })
        );

        let reopened = draft_from_definition(&definition);
        assert_eq!(reopened.name, "My Model");
        assert_eq!(
            reopened.descriptors.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
            vec!["reasoningEffort", "fastMode"]
        );
        assert_eq!(
            reopened.descriptors[0]
                .choices
                .iter()
                .map(|choice| choice.is_default)
                .collect::<Vec<_>>(),
            vec![false, true]
        );
    }

    #[test]
    fn preserves_the_current_choice_when_it_differs_from_the_built_in_default() {
        let descriptors = descriptors_from_capabilities(
            Some(&caps(json!({
                "optionDescriptors": [{
                    "id": "effort", "label": "Reasoning", "type": "select", "currentValue": "high",
                    "options": [
                        { "id": "low", "label": "Low", "isDefault": true },
                        { "id": "high", "label": "High" }
                    ]
                }]
            }))),
            Some("claudeAgent"),
        );
        assert_eq!(
            descriptors[0].choices.iter().map(|c| c.is_default).collect::<Vec<_>>(),
            vec![false, true]
        );
        let definition = definition_from_draft(&draft(|d| d.descriptors = descriptors));
        assert_eq!(
            caps_json(&definition)["optionDescriptors"][0]["currentValue"],
            json!("high")
        );
    }

    #[test]
    fn drops_prompt_injected_choices_when_copying_a_built_ins_descriptors() {
        let copied = descriptors_from_capabilities(
            Some(&caps(json!({
                "optionDescriptors": [{
                    "id": "effort", "label": "Reasoning", "type": "select",
                    "options": [
                        { "id": "high", "label": "High", "isDefault": true },
                        { "id": "ultrathink", "label": "Ultrathink" }
                    ],
                    "promptInjectedValues": ["ultrathink"]
                }]
            }))),
            Some("claudeAgent"),
        );
        assert_eq!(
            copied[0].choices.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["high"]
        );
    }

    #[test]
    fn preserves_boolean_values_through_copy_and_edit() {
        for current_value in [Some(true), Some(false), None] {
            let mut descriptor = json!({ "id": "thinking", "label": "Thinking", "type": "boolean" });
            if let Some(value) = current_value {
                descriptor["currentValue"] = json!(value);
            }
            let capabilities = caps(json!({ "optionDescriptors": [descriptor] }));
            let copied = definition_from_draft(&draft(|d| {
                d.descriptors = descriptors_from_capabilities(Some(&capabilities), Some("cursor"))
            }));
            assert_eq!(copied.capabilities.as_ref(), Some(&capabilities));
            let mut reopened = draft_from_definition(&copied);
            reopened.name = "Renamed".into();
            assert_eq!(
                definition_from_draft(&reopened).capabilities.as_ref(),
                Some(&capabilities)
            );
        }
    }

    #[test]
    fn excludes_claude_context_choices_from_presets_and_copies_without_changing_other_providers_or_authored_entries(
    ) {
        let capabilities = caps(json!({
            "optionDescriptors": [
                { "id": "contextWindow", "label": "Context", "type": "select",
                  "options": [{ "id": "1m", "label": "1M", "isDefault": true }] },
                { "id": "thinking", "label": "Thinking", "type": "boolean", "currentValue": true }
            ]
        }));
        let copied = definition_from_draft(&draft(|d| {
            d.descriptors = descriptors_from_capabilities(Some(&capabilities), Some("claudeAgent"))
        }));
        assert_eq!(
            caps_json(&copied)["optionDescriptors"],
            json!([serde_json::to_value(&capabilities).unwrap()["optionDescriptors"][1]])
        );
        let presets = definition_from_draft(&draft(|d| {
            d.descriptors = descriptor_presets_for_kind("claudeAgent")
                .iter()
                .map(descriptor_from_preset)
                .collect()
        }));
        assert!(
            !caps_json(&presets)["optionDescriptors"]
                .as_array()
                .unwrap()
                .iter()
                .any(|option| option["id"] == "contextWindow")
        );
        let cursor_copy = descriptors_from_capabilities(Some(&capabilities), Some("cursor"));
        assert_eq!(
            cursor_copy.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
            vec!["contextWindow", "thinking"]
        );
        let authored = CustomModelDefinition {
            slug: "custom".into(),
            name: "Custom".into(),
            capabilities: Some(capabilities.clone()),
        };
        let round_trip = definition_from_draft(&draft_from_definition(&authored));
        // `toMatchObject`: the saved descriptor may carry more keys (currentValue).
        let expected = &serde_json::to_value(&capabilities).unwrap()["optionDescriptors"][0];
        let actual = &caps_json(&round_trip)["optionDescriptors"][0];
        for key in ["id", "label", "type", "options"] {
            assert_eq!(&actual[key], &expected[key], "{key}");
        }
    }

    #[test]
    fn preserves_choice_descriptions_when_copying_renaming_and_saving() {
        let capabilities = caps(json!({
            "optionDescriptors": [{
                "id": "effort", "label": "Reasoning", "description": "Choose a reasoning level.",
                "type": "select",
                "options": [
                    { "id": "high", "label": "High", "isDefault": true },
                    { "id": "ultracode", "label": "Ultracode", "description": "Uses additional reasoning." }
                ]
            }]
        }));
        let copied = definition_from_draft(&draft(|d| {
            d.descriptors = descriptors_from_capabilities(Some(&capabilities), Some("claudeAgent"))
        }));
        let reopened = draft_from_definition(&copied);
        let mut renamed = reopened;
        renamed.name = "Renamed".into();
        let saved = definition_from_draft(&renamed);
        let expected = &serde_json::to_value(&capabilities).unwrap()["optionDescriptors"][0];
        for definition in [&saved, &copied] {
            let actual = &caps_json(definition)["optionDescriptors"][0];
            for key in ["id", "label", "description", "type", "options"] {
                assert_eq!(&actual[key], &expected[key], "{key}");
            }
        }
    }

    #[test]
    fn offers_saveable_presets_for_each_provider() {
        for (driver, presets) in DESCRIPTOR_PRESETS_BY_KIND {
            assert_eq!(
                validate_draft(&draft(|d| {
                    d.descriptors = presets.iter().map(descriptor_from_preset).collect()
                })),
                None,
                "{driver}"
            );
        }
    }

    #[test]
    fn collapses_a_blank_name_and_no_options_back_to_a_bare_definition() {
        let definition = definition_from_draft(&draft(|d| d.name = "  ".into()));
        assert_eq!(definition.slug, "my-model");
        assert_eq!(definition.name, "my-model");
        assert_eq!(definition.capabilities, None);
        assert_eq!(
            draft_from_definition(&CustomModelDefinition {
                slug: "x".into(),
                name: "x".into(),
                capabilities: None,
            })
            .name,
            ""
        );
    }

    #[test]
    fn rejects_duplicate_ids_blank_ids_and_selects_without_choices() {
        assert_eq!(
            validate_draft(&draft(|d| d.descriptors = vec![select("", &["a"])])),
            Some("Option 1 needs an id.".into())
        );
        assert_eq!(
            validate_draft(&draft(|d| {
                d.descriptors = vec![select("effort", &["a"]), select("effort", &["b"])]
            })),
            Some("Option 2: id \"effort\" is used twice.".into())
        );
        assert_eq!(
            validate_draft(&draft(|d| d.descriptors = vec![select("effort", &[])])),
            Some("Option 1 needs at least one choice.".into())
        );
        assert_eq!(
            validate_draft(&draft(|d| d.descriptors = vec![select("effort", &["a", "a"])])),
            Some("Option 1: choice \"a\" is used twice.".into())
        );
        assert_eq!(
            validate_draft(&draft(|d| d.descriptors = vec![select("effort", &["a"])])),
            None
        );
    }
}
