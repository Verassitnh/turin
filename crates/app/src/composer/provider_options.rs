//! The provider-option helpers of upstream/packages/shared/src/model.ts that
//! the composer uses, over plain-string option types (the contract types wrap
//! every id in a trimmed-string newtype; `From` impls convert at the seam).

use turin_contracts::model as contract;

#[derive(Debug, Clone, PartialEq)]
pub struct OptionChoice {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OptionDescriptor {
    Select {
        id: String,
        label: String,
        description: Option<String>,
        options: Vec<OptionChoice>,
        current_value: Option<String>,
        prompt_injected_values: Vec<String>,
    },
    Boolean {
        id: String,
        label: String,
        description: Option<String>,
        current_value: Option<bool>,
    },
}

impl OptionDescriptor {
    pub fn id(&self) -> &str {
        match self {
            Self::Select { id, .. } | Self::Boolean { id, .. } => id,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Select { label, .. } | Self::Boolean { label, .. } => label,
        }
    }

    pub fn is_select(&self) -> bool {
        matches!(self, Self::Select { .. })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OptionValue {
    Str(String),
    Bool(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub struct OptionSelection {
    pub id: String,
    pub value: OptionValue,
}

impl OptionSelection {
    pub fn string(id: &str, value: &str) -> Self {
        Self {
            id: id.to_owned(),
            value: OptionValue::Str(value.to_owned()),
        }
    }

    pub fn boolean(id: &str, value: bool) -> Self {
        Self {
            id: id.to_owned(),
            value: OptionValue::Bool(value),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelCapabilities {
    pub option_descriptors: Vec<OptionDescriptor>,
}

impl From<&contract::ProviderOptionDescriptor> for OptionDescriptor {
    fn from(descriptor: &contract::ProviderOptionDescriptor) -> Self {
        match descriptor {
            contract::ProviderOptionDescriptor::Select(select) => Self::Select {
                id: select.id.as_str().to_owned(),
                label: select.label.as_str().to_owned(),
                description: select.description.as_ref().map(|d| d.as_str().to_owned()),
                options: select
                    .options
                    .iter()
                    .map(|choice| OptionChoice {
                        id: choice.id.as_str().to_owned(),
                        label: choice.label.as_str().to_owned(),
                        description: choice.description.as_ref().map(|d| d.as_str().to_owned()),
                        is_default: choice.is_default.unwrap_or(false),
                    })
                    .collect(),
                current_value: select.current_value.as_ref().map(|v| v.as_str().to_owned()),
                prompt_injected_values: select
                    .prompt_injected_values
                    .iter()
                    .flatten()
                    .map(|v| v.as_str().to_owned())
                    .collect(),
            },
            contract::ProviderOptionDescriptor::Boolean(boolean) => Self::Boolean {
                id: boolean.id.as_str().to_owned(),
                label: boolean.label.as_str().to_owned(),
                description: boolean.description.as_ref().map(|d| d.as_str().to_owned()),
                current_value: boolean.current_value,
            },
        }
    }
}

impl From<&contract::ModelCapabilities> for ModelCapabilities {
    fn from(caps: &contract::ModelCapabilities) -> Self {
        Self {
            option_descriptors: caps
                .option_descriptors
                .iter()
                .flatten()
                .map(OptionDescriptor::from)
                .collect(),
        }
    }
}

impl From<&contract::ProviderOptionSelection> for OptionSelection {
    fn from(selection: &contract::ProviderOptionSelection) -> Self {
        Self {
            id: selection.id.as_str().to_owned(),
            value: match &selection.value {
                contract::ProviderOptionSelectionValue::Str(value) => {
                    OptionValue::Str(value.as_str().to_owned())
                }
                contract::ProviderOptionSelectionValue::Bool(value) => OptionValue::Bool(*value),
            },
        }
    }
}

fn raw_selection_value<'a>(selections: &'a [OptionSelection], id: &str) -> Option<&'a OptionValue> {
    selections
        .iter()
        .find(|selection| selection.id == id)
        .map(|selection| &selection.value)
}

fn trim_or_none(value: Option<&str>) -> Option<String> {
    let trimmed = turin_contracts::base_schemas::js_trim(value?);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// `resolveDescriptorChoiceValue`.
fn resolve_descriptor_choice_value(
    options: &[OptionChoice],
    current_value: &Option<String>,
    prompt_injected_values: &[String],
    raw: Option<&str>,
) -> Option<String> {
    let default_choice = || {
        options
            .iter()
            .find(|option| option.is_default)
            .map(|option| option.id.clone())
    };
    let Some(trimmed) = trim_or_none(raw) else {
        return current_value.clone().or_else(default_choice);
    };
    if options.is_empty() {
        return Some(trimmed);
    }
    if prompt_injected_values.contains(&trimmed)
        && options.iter().any(|option| option.id == trimmed)
    {
        return default_choice();
    }
    if options.iter().any(|option| option.id == trimmed) {
        return Some(trimmed);
    }
    current_value.clone().or_else(default_choice)
}

/// `getProviderOptionDescriptors`: the model's descriptors with selections applied.
pub fn get_provider_option_descriptors(
    caps: &ModelCapabilities,
    selections: &[OptionSelection],
) -> Vec<OptionDescriptor> {
    caps.option_descriptors
        .iter()
        .cloned()
        .map(|descriptor| {
            let raw = raw_selection_value(selections, descriptor.id());
            match descriptor {
                OptionDescriptor::Boolean {
                    id,
                    label,
                    description,
                    current_value,
                } => OptionDescriptor::Boolean {
                    current_value: match raw {
                        Some(OptionValue::Bool(value)) => Some(*value),
                        _ => current_value,
                    },
                    id,
                    label,
                    description,
                },
                OptionDescriptor::Select {
                    id,
                    label,
                    description,
                    options,
                    current_value,
                    prompt_injected_values,
                } => {
                    // A selection that is not a string falls back to the descriptor value.
                    let raw_string = match raw {
                        Some(OptionValue::Str(value)) => Some(value.as_str()),
                        _ => current_value.as_deref(),
                    };
                    let resolved = resolve_descriptor_choice_value(
                        &options,
                        &current_value,
                        &prompt_injected_values,
                        raw_string,
                    );
                    OptionDescriptor::Select {
                        id,
                        label,
                        description,
                        options,
                        current_value: resolved,
                        prompt_injected_values,
                    }
                }
            }
        })
        .collect()
}

/// A reported (display-only) selection from a running session.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectionContext<'a> {
    pub instance_id: &'a str,
    pub model: &'a str,
    pub options: &'a [OptionSelection],
}

fn reported_option_value<'a>(
    id: &str,
    selection: Option<&SelectionContext<'_>>,
    reported: Option<&'a SelectionContext<'a>>,
) -> Option<&'a OptionValue> {
    let (selection, reported) = (selection?, reported?);
    if selection.instance_id != reported.instance_id
        || selection.model != reported.model
        || selection.options.iter().any(|option| option.id == id)
    {
        return None;
    }
    raw_selection_value(reported.options, id)
}

/// `getProviderOptionCurrentValue`.
pub fn get_provider_option_current_value(
    descriptor: Option<&OptionDescriptor>,
    selection: Option<&SelectionContext<'_>>,
    reported: Option<&SelectionContext<'_>>,
) -> Option<OptionValue> {
    let descriptor = descriptor?;
    let has_explicit = selection.is_some_and(|s| s.options.iter().any(|o| o.id == descriptor.id()));
    if let Some(value) = reported_option_value(descriptor.id(), selection, reported) {
        return Some(value.clone());
    }
    if descriptor.id() == "variant" && selection.is_some() && !has_explicit {
        return None;
    }
    match descriptor {
        OptionDescriptor::Boolean { current_value, .. } => current_value.map(OptionValue::Bool),
        OptionDescriptor::Select {
            options,
            current_value,
            ..
        } => {
            if let Some(value) = current_value.as_ref().filter(|v| !v.is_empty()) {
                return Some(OptionValue::Str(value.clone()));
            }
            options
                .iter()
                .find(|option| option.is_default)
                .map(|option| OptionValue::Str(option.id.clone()))
        }
    }
}

/// `getProviderOptionCurrentLabel`.
pub fn get_provider_option_current_label(
    descriptor: Option<&OptionDescriptor>,
    selection: Option<&SelectionContext<'_>>,
    reported: Option<&SelectionContext<'_>>,
) -> Option<String> {
    let descriptor = descriptor?;
    match descriptor {
        OptionDescriptor::Boolean { current_value, .. } => {
            current_value.map(|value| if value { "On" } else { "Off" }.to_owned())
        }
        OptionDescriptor::Select { options, id, .. } => {
            let current = get_provider_option_current_value(Some(descriptor), selection, reported);
            let label = match &current {
                Some(OptionValue::Str(value)) => options
                    .iter()
                    .find(|o| &o.id == value)
                    .map(|o| o.label.clone()),
                _ => None,
            };
            label.or_else(|| {
                if matches!(reported_option_value(id, selection, reported), Some(OptionValue::Str(v)) if v == "default") {
                    Some("Default".to_owned())
                } else if id == "variant" {
                    Some("Unknown".to_owned())
                } else {
                    None
                }
            })
        }
    }
}

/// `buildProviderOptionSelectionsFromDescriptors`.
pub fn build_provider_option_selections_from_descriptors(
    descriptors: &[OptionDescriptor],
) -> Option<Vec<OptionSelection>> {
    let selections: Vec<OptionSelection> = descriptors
        .iter()
        .filter_map(|descriptor| {
            get_provider_option_current_value(Some(descriptor), None, None).map(|value| {
                OptionSelection {
                    id: descriptor.id().to_owned(),
                    value,
                }
            })
        })
        .collect();
    (!selections.is_empty()).then_some(selections)
}

/// `buildExplicitProviderOptionSelectionsFromDescriptors`.
pub fn build_explicit_provider_option_selections_from_descriptors(
    descriptors: &[OptionDescriptor],
    selections: &[OptionSelection],
) -> Option<Vec<OptionSelection>> {
    if selections.is_empty() {
        return None;
    }
    let normalized: Vec<OptionSelection> =
        build_provider_option_selections_from_descriptors(descriptors)?
            .into_iter()
            .filter(|selection| {
                selections
                    .iter()
                    .any(|explicit| explicit.id == selection.id)
            })
            .collect();
    (!normalized.is_empty()).then_some(normalized)
}

/// `isClaudeUltrathinkPrompt`: `/\bultrathink\b/i`.
pub fn is_claude_ultrathink_prompt(text: Option<&str>) -> bool {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    text.is_some_and(|text| {
        RE.get_or_init(|| regex::Regex::new(r"(?i)\bultrathink\b").unwrap())
            .is_match(text)
    })
}

/// `applyClaudePromptEffortPrefix`.
pub fn apply_claude_prompt_effort_prefix(text: &str, effort: Option<&str>) -> String {
    let trimmed = turin_contracts::base_schemas::js_trim(text);
    if trimmed.is_empty() {
        return String::new();
    }
    // Prefixing a slash command turns it into plain prose, so Claude never
    // runs it: accept any first token without a second slash.
    let is_slash_command = trimmed.strip_prefix('/').is_some_and(|rest| {
        let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        token_end > 0 && !rest[..token_end].contains('/')
    });
    if effort != Some("ultrathink") || is_slash_command {
        return trimmed.to_owned();
    }
    if trimmed.starts_with("Ultrathink:") {
        return trimmed.to_owned();
    }
    format!("Ultrathink:\n{trimmed}")
}

/// `normalizeModelSlug`: trims, then expands the driver's aliases.
pub fn normalize_model_slug(model: Option<&str>, driver: &str) -> Option<String> {
    let trimmed = trim_or_none(model)?;
    let aliased = contract::model_slug_aliases_by_provider(driver)
        .and_then(|aliases| aliases.iter().find(|(alias, _)| *alias == trimmed))
        .map(|(_, canonical)| (*canonical).to_owned());
    Some(aliased.unwrap_or(trimmed))
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub fn select(
        id: &str,
        options: &[(&str, &str, bool)],
        prompt_injected: &[&str],
    ) -> OptionDescriptor {
        let default = options
            .iter()
            .find(|(_, _, is_default)| *is_default)
            .map(|(id, _, _)| (*id).to_owned());
        OptionDescriptor::Select {
            id: id.to_owned(),
            label: id.to_owned(),
            description: None,
            options: options
                .iter()
                .map(|(id, label, is_default)| OptionChoice {
                    id: (*id).to_owned(),
                    label: (*label).to_owned(),
                    description: None,
                    is_default: *is_default,
                })
                .collect(),
            current_value: default,
            prompt_injected_values: prompt_injected.iter().map(|v| (*v).to_owned()).collect(),
        }
    }

    pub fn boolean(id: &str, current: Option<bool>) -> OptionDescriptor {
        OptionDescriptor::Boolean {
            id: id.to_owned(),
            label: id.to_owned(),
            description: None,
            current_value: current,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[test]
    fn selections_override_descriptor_current_values() {
        let caps = ModelCapabilities {
            option_descriptors: vec![
                select(
                    "effort",
                    &[("low", "Low", false), ("high", "High", true)],
                    &[],
                ),
                boolean("fastMode", None),
            ],
        };
        let descriptors = get_provider_option_descriptors(
            &caps,
            &[
                OptionSelection::string("effort", "low"),
                OptionSelection::boolean("fastMode", true),
            ],
        );
        assert_eq!(
            get_provider_option_current_value(Some(&descriptors[0]), None, None),
            Some(OptionValue::Str("low".into()))
        );
        assert_eq!(
            get_provider_option_current_value(Some(&descriptors[1]), None, None),
            Some(OptionValue::Bool(true))
        );
        // An unknown choice falls back to the descriptor default.
        let descriptors =
            get_provider_option_descriptors(&caps, &[OptionSelection::string("effort", "bogus")]);
        assert_eq!(
            get_provider_option_current_value(Some(&descriptors[0]), None, None),
            Some(OptionValue::Str("high".into()))
        );
    }

    #[test]
    fn labels_reflect_the_current_value() {
        let descriptor = select(
            "effort",
            &[("low", "Low", false), ("high", "High", true)],
            &[],
        );
        assert_eq!(
            get_provider_option_current_label(Some(&descriptor), None, None).as_deref(),
            Some("High")
        );
        assert_eq!(
            get_provider_option_current_label(Some(&boolean("thinking", Some(true))), None, None)
                .as_deref(),
            Some("On")
        );
        assert_eq!(
            get_provider_option_current_label(Some(&boolean("thinking", None)), None, None),
            None
        );
    }

    #[test]
    fn reported_values_fill_in_for_unselected_options() {
        let descriptor = select("variant", &[("max", "Max", false)], &[]);
        let selection = SelectionContext {
            instance_id: "opencode",
            model: "m",
            options: &[],
        };
        let reported_options = [OptionSelection::string("variant", "max")];
        let reported = SelectionContext {
            instance_id: "opencode",
            model: "m",
            options: &reported_options,
        };
        assert_eq!(
            get_provider_option_current_value(Some(&descriptor), Some(&selection), Some(&reported)),
            Some(OptionValue::Str("max".into()))
        );
        // Without a reported value, a variant the user did not pick is unknown.
        assert_eq!(
            get_provider_option_current_value(Some(&descriptor), Some(&selection), None),
            None
        );
        assert_eq!(
            get_provider_option_current_label(Some(&descriptor), Some(&selection), None).as_deref(),
            Some("Unknown")
        );
    }

    #[test]
    fn detects_ultrathink_prompts_and_prefixes_them() {
        assert!(is_claude_ultrathink_prompt(Some(
            "Ultrathink:\nInvestigate"
        )));
        assert!(is_claude_ultrathink_prompt(Some("please ULTRATHINK this")));
        assert!(!is_claude_ultrathink_prompt(Some("ultrathinking")));
        assert!(!is_claude_ultrathink_prompt(None));
        assert_eq!(
            apply_claude_prompt_effort_prefix("  hi ", Some("ultrathink")),
            "Ultrathink:\nhi"
        );
        assert_eq!(apply_claude_prompt_effort_prefix("hi", Some("high")), "hi");
        assert_eq!(
            apply_claude_prompt_effort_prefix("/deploy.prod now", Some("ultrathink")),
            "/deploy.prod now"
        );
        assert_eq!(
            apply_claude_prompt_effort_prefix("/home/me/app.ts", Some("ultrathink")),
            "Ultrathink:\n/home/me/app.ts"
        );
        assert_eq!(
            apply_claude_prompt_effort_prefix("Ultrathink:\nx", Some("ultrathink")),
            "Ultrathink:\nx"
        );
    }

    #[test]
    fn normalizes_model_slugs_through_driver_aliases() {
        assert_eq!(
            normalize_model_slug(Some(" 5.4 "), "codex").as_deref(),
            Some("gpt-5.4")
        );
        assert_eq!(
            normalize_model_slug(Some("custom/x"), "codex").as_deref(),
            Some("custom/x")
        );
        assert_eq!(normalize_model_slug(Some("  "), "codex"), None);
        assert_eq!(normalize_model_slug(None, "codex"), None);
    }
}
