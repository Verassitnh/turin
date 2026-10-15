//! Ports of upstream/apps/web/src/components/chat/composerProviderState.tsx,
//! the logic half of TraitsPicker.tsx and `providerModels.ts`
//! (`getProviderModelCapabilities`, `resolveSelectableModel`).
//!
//! Everything is data-driven by the model's option descriptors; the views in
//! `views/` render what [`traits_section`] and [`build_traits_trigger_display`]
//! decide.

use crate::composer::provider_options::{
    ModelCapabilities, OptionDescriptor, OptionSelection, OptionValue, SelectionContext,
    build_explicit_provider_option_selections_from_descriptors, build_provider_option_selections_from_descriptors,
    get_provider_option_current_label, get_provider_option_current_value, get_provider_option_descriptors,
    is_claude_ultrathink_prompt, normalize_model_slug,
};

/// `ServerProviderModel`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProviderModel {
    pub slug: String,
    pub name: String,
    pub short_name: Option<String>,
    pub sub_provider: Option<String>,
    pub aliases: Vec<String>,
    /// `badge: "new"`.
    pub is_new: bool,
    pub is_custom: bool,
    pub is_default: bool,
    pub is_legacy: bool,
    pub capabilities: Option<ModelCapabilities>,
}

impl ProviderModel {
    pub fn simple(slug: &str, name: &str) -> Self {
        Self {
            slug: slug.to_owned(),
            name: name.to_owned(),
            ..Default::default()
        }
    }
}

/// `resolveSelectableModel`.
pub fn resolve_selectable_model(provider: &str, value: Option<&str>, options: &[ProviderModel]) -> Option<String> {
    let trimmed = turin_contracts::base_schemas::js_trim(value?);
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_lowercase();
    if let Some(direct) = options.iter().find(|option| option.slug == trimmed) {
        return Some(direct.slug.clone());
    }
    if let Some(by_name) = options.iter().find(|option| option.name.to_lowercase() == lower) {
        return Some(by_name.slug.clone());
    }
    if let Some(by_alias) = options
        .iter()
        .find(|option| option.aliases.iter().any(|alias| alias.to_lowercase() == lower))
    {
        return Some(by_alias.slug.clone());
    }
    let normalized = normalize_model_slug(Some(trimmed), provider)?;
    options.iter().find(|option| option.slug == normalized).map(|option| option.slug.clone())
}

/// The opencode "plan" agent is only reachable while legacy plan mode is on.
/// With it off, drop the option so it cannot be selected or dispatched, and
/// drop the descriptor entirely when nothing remains selectable.
fn without_plan_agent_option(caps: ModelCapabilities) -> ModelCapabilities {
    ModelCapabilities {
        option_descriptors: caps
            .option_descriptors
            .into_iter()
            .filter_map(|descriptor| match descriptor {
                OptionDescriptor::Select {
                    id,
                    label,
                    description,
                    options,
                    current_value,
                    prompt_injected_values,
                } if id == "agent" => {
                    let options: Vec<_> = options.into_iter().filter(|option| option.id != "plan").collect();
                    if options.is_empty() {
                        return None;
                    }
                    let current_value = current_value
                        .filter(|value| options.iter().any(|option| &option.id == value))
                        .or_else(|| {
                            options
                                .iter()
                                .find(|option| option.is_default)
                                .or_else(|| options.first())
                                .map(|option| option.id.clone())
                        });
                    Some(OptionDescriptor::Select {
                        id,
                        label,
                        description,
                        options,
                        current_value,
                        prompt_injected_values,
                    })
                }
                other => Some(other),
            })
            .collect(),
    }
}

/// `getProviderModelCapabilities`.
pub fn get_provider_model_capabilities(
    models: &[ProviderModel],
    model: Option<&str>,
    provider: &str,
    plan_mode_enabled: bool,
) -> ModelCapabilities {
    let slug = resolve_selectable_model(provider, model, models);
    let caps = slug
        .and_then(|slug| models.iter().find(|candidate| candidate.slug == slug))
        .and_then(|model| model.capabilities.clone())
        .unwrap_or_default();
    if plan_mode_enabled { caps } else { without_plan_agent_option(caps) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ComposerPromptInjectionState {
    #[default]
    None,
    Ultrathink,
}

/// `getComposerPromptInjectionState`.
pub fn get_composer_prompt_injection_state(prompt: &str) -> ComposerPromptInjectionState {
    if is_claude_ultrathink_prompt(Some(prompt)) {
        ComposerPromptInjectionState::Ultrathink
    } else {
        ComposerPromptInjectionState::None
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ComposerProviderState {
    pub provider: String,
    pub prompt_effort: Option<String>,
    pub model_options_for_dispatch: Option<Vec<OptionSelection>>,
    /// The ultrathink glow on the composer frame and model picker icon.
    pub ultrathink_active: bool,
}

/// `withImplicitFastModeDefault`: Cursor ACP can report `fastMode: true` as the
/// provider default. Fast is only selected when the user chose it, so an
/// explicit `false` is injected otherwise.
pub fn with_implicit_fast_mode_default(
    caps: &ModelCapabilities,
    model_options: &[OptionSelection],
) -> Option<Vec<OptionSelection>> {
    let has_explicit = model_options.iter().any(|selection| selection.id == "fastMode");
    let has_descriptor = caps
        .option_descriptors
        .iter()
        .any(|descriptor| matches!(descriptor, OptionDescriptor::Boolean { id, .. } if id == "fastMode"));
    if has_explicit || !has_descriptor {
        return (!model_options.is_empty()).then(|| model_options.to_vec());
    }
    let mut selections = model_options.to_vec();
    selections.push(OptionSelection::boolean("fastMode", false));
    Some(selections)
}

fn resolve_composer_option_selections(
    models: &[ProviderModel],
    model: &str,
    provider: &str,
    model_options: &[OptionSelection],
    plan_mode_enabled: bool,
) -> (ModelCapabilities, Vec<OptionSelection>) {
    let caps = get_provider_model_capabilities(models, Some(model), provider, plan_mode_enabled);
    let selections = with_implicit_fast_mode_default(&caps, model_options).unwrap_or_default();
    (caps, selections)
}

pub struct ComposerProviderStateInput<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub models: &'a [ProviderModel],
    pub prompt_injection_state: ComposerPromptInjectionState,
    pub model_options: &'a [OptionSelection],
    pub plan_mode_enabled: bool,
}

/// `getComposerProviderState`.
pub fn get_composer_provider_state(input: &ComposerProviderStateInput<'_>) -> ComposerProviderState {
    let provider = input.provider.to_owned();
    if input.provider == "opencode" {
        let normalized = normalize_model_slug(Some(input.model), input.provider);
        let in_catalog = input.models.iter().any(|candidate| Some(&candidate.slug) == normalized.as_ref());
        if !in_catalog {
            let preserved: Vec<OptionSelection> = input
                .model_options
                .iter()
                .filter(|option| {
                    input.plan_mode_enabled
                        || option.id != "agent"
                        || !matches!(&option.value, OptionValue::Str(value) if value == "plan")
                })
                .cloned()
                .collect();
            return ComposerProviderState {
                provider,
                prompt_effort: None,
                model_options_for_dispatch: (!preserved.is_empty()).then_some(preserved),
                ultrathink_active: false,
            };
        }
    }
    let (caps, selections) = resolve_composer_option_selections(
        input.models,
        input.model,
        input.provider,
        input.model_options,
        input.plan_mode_enabled,
    );
    let descriptors = get_provider_option_descriptors(&caps, &selections);
    let primary_select = descriptors.iter().find(|descriptor| descriptor.is_select());
    let primary_value = get_provider_option_current_value(primary_select, None, None);
    let prompt_effort = match primary_value {
        Some(OptionValue::Str(value)) => Some(value),
        _ => None,
    };
    let ultrathink_active = matches!(
        primary_select,
        Some(OptionDescriptor::Select { prompt_injected_values, .. }) if !prompt_injected_values.is_empty()
    ) && input.prompt_injection_state == ComposerPromptInjectionState::Ultrathink;
    ComposerProviderState {
        provider,
        prompt_effort,
        model_options_for_dispatch: build_explicit_provider_option_selections_from_descriptors(&descriptors, &selections),
        ultrathink_active,
    }
}

// ── Traits ─────────────────────────────────────────────────────────────────

fn saved_option_label(id: &str) -> String {
    match id {
        "agent" => return "Agent".to_owned(),
        "effort" => return "Effort".to_owned(),
        "reasoningEffort" => return "Reasoning effort".to_owned(),
        "variant" => return "Reasoning".to_owned(),
        _ => {}
    }
    let mut spaced = String::new();
    let mut previous: Option<char> = None;
    for ch in id.chars() {
        if let Some(prev) = previous
            && (prev.is_ascii_lowercase() || prev.is_ascii_digit())
            && ch.is_ascii_uppercase()
        {
            spaced.push(' ');
        }
        spaced.push(ch);
        previous = Some(ch);
    }
    let mut chars = spaced.chars();
    chars.next().map_or(String::new(), |first| first.to_uppercase().chain(chars).collect())
}

/// `buildUnavailableModelOptionDescriptors`: read-only descriptors for saved
/// values whose model metadata is unavailable.
pub fn build_unavailable_model_option_descriptors(selections: &[OptionSelection]) -> Vec<OptionDescriptor> {
    selections
        .iter()
        .map(|selection| match &selection.value {
            OptionValue::Bool(value) => OptionDescriptor::Boolean {
                id: selection.id.clone(),
                label: saved_option_label(&selection.id),
                description: None,
                current_value: Some(*value),
            },
            OptionValue::Str(value) => OptionDescriptor::Select {
                id: selection.id.clone(),
                label: saved_option_label(&selection.id),
                description: None,
                options: vec![crate::composer::provider_options::OptionChoice {
                    id: value.clone(),
                    label: value.clone(),
                    description: None,
                    is_default: false,
                }],
                current_value: Some(value.clone()),
                prompt_injected_values: Vec::new(),
            },
        })
        .collect()
}

/// What the traits menu shows (`getTraitsSectionVisibility`).
#[derive(Debug, Clone, PartialEq)]
pub struct TraitsSection {
    pub descriptors: Vec<OptionDescriptor>,
    pub primary_select_id: Option<String>,
    pub effort: Option<String>,
    pub ultrathink_prompt_controlled: bool,
    pub ultrathink_in_body_text: bool,
    pub model_is_unavailable: bool,
    pub has_any_controls: bool,
}

pub struct TraitsInput<'a> {
    pub provider: &'a str,
    pub models: &'a [ProviderModel],
    pub model: Option<&'a str>,
    pub prompt: &'a str,
    pub model_options: &'a [OptionSelection],
    pub allow_prompt_injected_effort: bool,
    pub plan_mode_enabled: bool,
}

fn strip_ultrathink_prefix(prompt: &str) -> &str {
    let lower = prompt.to_ascii_lowercase();
    match lower.strip_prefix("ultrathink:") {
        Some(rest) => {
            let consumed = prompt.len() - rest.len();
            prompt[consumed..].trim_start()
        }
        None => prompt,
    }
}

/// `getTraitsSectionVisibility`.
pub fn traits_section(input: &TraitsInput<'_>) -> TraitsSection {
    let caps = get_provider_model_capabilities(input.models, input.model, input.provider, input.plan_mode_enabled);
    let model_is_unavailable = input.provider == "opencode" && {
        let normalized = normalize_model_slug(input.model, input.provider);
        !input.models.iter().any(|candidate| Some(&candidate.slug) == normalized.as_ref())
    };
    let descriptors = if model_is_unavailable {
        let selections: Vec<OptionSelection> = input
            .model_options
            .iter()
            .filter(|option| {
                input.plan_mode_enabled
                    || option.id != "agent"
                    || !matches!(&option.value, OptionValue::Str(value) if value == "plan")
            })
            .cloned()
            .collect();
        build_unavailable_model_option_descriptors(&selections)
    } else {
        get_provider_option_descriptors(&caps, input.model_options)
    };
    let primary_select = descriptors.iter().find(|descriptor| descriptor.is_select());
    let prompt_injection_capable = matches!(
        primary_select,
        Some(OptionDescriptor::Select { prompt_injected_values, .. }) if !prompt_injected_values.is_empty()
    );
    let ultrathink_prompt_controlled =
        input.allow_prompt_injected_effort && prompt_injection_capable && is_claude_ultrathink_prompt(Some(input.prompt));
    let ultrathink_in_body_text =
        ultrathink_prompt_controlled && is_claude_ultrathink_prompt(Some(strip_ultrathink_prefix(input.prompt)));
    let effort = if ultrathink_prompt_controlled {
        Some("ultrathink".to_owned())
    } else {
        match get_provider_option_current_value(primary_select, None, None) {
            Some(OptionValue::Str(value)) => Some(value),
            _ => None,
        }
    };
    let has = |id: &str| descriptors.iter().any(|descriptor| descriptor.id() == id);
    let has_any_controls = primary_select.is_some()
        || descriptors.iter().any(|d| matches!(d, OptionDescriptor::Boolean { id, .. } if id == "thinking" || id == "fastMode"))
        || has("contextWindow")
        || has("agent")
        || (model_is_unavailable && !descriptors.is_empty());
    TraitsSection {
        primary_select_id: primary_select.map(|descriptor| descriptor.id().to_owned()),
        descriptors,
        effort,
        ultrathink_prompt_controlled,
        ultrathink_in_body_text,
        model_is_unavailable,
        has_any_controls,
    }
}

/// `shouldRenderTraitsControls`.
pub fn should_render_traits_controls(input: &TraitsInput<'_>) -> bool {
    traits_section(input).has_any_controls
}

pub struct TraitsTriggerInput<'a> {
    pub provider: &'a str,
    pub descriptors: &'a [OptionDescriptor],
    pub primary_select_descriptor_id: Option<&'a str>,
    pub ultrathink_prompt_controlled: bool,
    pub model_selection: Option<&'a SelectionContext<'a>>,
    pub reported_model_selection: Option<&'a SelectionContext<'a>>,
}

/// `buildTraitsTriggerDisplay`: pair speed with reasoning while keeping other
/// traits separated.
pub fn build_traits_trigger_display(input: &TraitsTriggerInput<'_>) -> String {
    let mut fast_mode_fallback_label: Option<String> = None;
    let mut speed_label: Option<String> = None;
    let mut reasoning_label_index: Option<usize> = None;
    let mut labels: Vec<String> = Vec::new();
    for descriptor in input.descriptors {
        if let OptionDescriptor::Boolean { id, current_value, .. } = descriptor
            && id == "fastMode"
        {
            speed_label = (*current_value == Some(true)).then(|| "Fast".to_owned());
            fast_mode_fallback_label = Some(speed_label.clone().unwrap_or_else(|| "Normal".to_owned()));
            continue;
        }
        if input.provider == "codex"
            && let OptionDescriptor::Select { id, options, .. } = descriptor
            && id == "serviceTier"
        {
            let current = match get_provider_option_current_value(Some(descriptor), None, None) {
                Some(OptionValue::Str(value)) => Some(value),
                _ => None,
            };
            let fast_tier = options.iter().find(|option| option.label == "Fast");
            let ultrafast_tier = options.iter().find(|option| option.label == "Ultrafast");
            let is_current = |tier: Option<&crate::composer::provider_options::OptionChoice>| {
                tier.is_some_and(|tier| current.as_deref() == Some(tier.id.as_str()))
            };
            if ((fast_tier.is_some() || ultrafast_tier.is_some()) && current.as_deref() == Some("default"))
                || is_current(fast_tier)
                || is_current(ultrafast_tier)
            {
                speed_label = if is_current(ultrafast_tier) {
                    Some("Ultrafast".to_owned())
                } else if is_current(fast_tier) {
                    Some("Fast".to_owned())
                } else {
                    None
                };
                fast_mode_fallback_label = Some(
                    options
                        .iter()
                        .find(|option| Some(&option.id) == current.as_ref())
                        .map_or_else(|| "Normal".to_owned(), |option| option.label.clone()),
                );
                continue;
            }
        }
        let label = if input.ultrathink_prompt_controlled && Some(descriptor.id()) == input.primary_select_descriptor_id {
            Some("Ultrathink".to_owned())
        } else if let OptionDescriptor::Boolean { label, current_value, .. } = descriptor {
            Some(format!("{label} {}", if *current_value == Some(true) { "On" } else { "Off" }))
        } else {
            get_provider_option_current_label(Some(descriptor), input.model_selection, input.reported_model_selection)
        };
        if let Some(label) = label.filter(|label| !label.is_empty()) {
            // Custom models retain descriptor order, so the primary select can be context.
            if reasoning_label_index.is_none()
                && descriptor.is_select()
                && ["reasoningEffort", "reasoning", "effort", "variant", "thinking"].contains(&descriptor.id())
            {
                reasoning_label_index = Some(labels.len());
            }
            labels.push(label);
        }
    }
    // Only fall back to text when fast mode is genuinely the sole trait.
    if labels.is_empty()
        && let Some(fallback) = fast_mode_fallback_label
    {
        return fallback;
    }
    if let Some(speed) = speed_label {
        match reasoning_label_index {
            Some(index) => labels[index] = format!("{} {speed}", labels[index]),
            None => labels.push(speed),
        }
    }
    labels.join(" · ")
}

/// Replace one descriptor's current value (`replaceDescriptorCurrentValue`).
pub fn replace_descriptor_current_value(
    descriptors: &[OptionDescriptor],
    descriptor_id: &str,
    value: &OptionValue,
) -> Vec<OptionDescriptor> {
    descriptors
        .iter()
        .cloned()
        .map(|descriptor| {
            if descriptor.id() != descriptor_id {
                return descriptor;
            }
            match (descriptor, value) {
                (
                    OptionDescriptor::Boolean { id, label, description, .. },
                    OptionValue::Bool(value),
                ) => OptionDescriptor::Boolean {
                    id,
                    label,
                    description,
                    current_value: Some(*value),
                },
                (
                    OptionDescriptor::Select {
                        id,
                        label,
                        description,
                        options,
                        prompt_injected_values,
                        ..
                    },
                    OptionValue::Str(value),
                ) => OptionDescriptor::Select {
                    id,
                    label,
                    description,
                    options,
                    current_value: Some(value.clone()),
                    prompt_injected_values,
                },
                (other, _) => other,
            }
        })
        .collect()
}

/// The new selections after a traits menu pick (`updateDescriptors`).
pub fn selections_after_pick(
    descriptors: &[OptionDescriptor],
    descriptor_id: &str,
    value: &OptionValue,
) -> Option<Vec<OptionSelection>> {
    build_provider_option_selections_from_descriptors(&replace_descriptor_current_value(descriptors, descriptor_id, value))
}

/// What picking `value` on a select descriptor does (`handleSelectChange`).
#[derive(Debug, Clone, PartialEq)]
pub enum TraitsSelectOutcome {
    /// Nothing changes.
    None,
    /// A prompt-injected value rewrites the prompt instead of the options.
    PromptChange(String),
    /// New selections, optionally with a prompt stripped of its ultrathink prefix.
    OptionsChange {
        selections: Option<Vec<OptionSelection>>,
        prompt: Option<String>,
    },
}

pub fn handle_traits_select_change(
    section: &TraitsSection,
    descriptor_id: &str,
    value: &str,
    prompt: &str,
) -> TraitsSelectOutcome {
    if value.is_empty() {
        return TraitsSelectOutcome::None;
    }
    let descriptor = section.descriptors.iter().find(|descriptor| descriptor.id() == descriptor_id);
    if let Some(OptionDescriptor::Select { prompt_injected_values, .. }) = descriptor
        && prompt_injected_values.iter().any(|injected| injected == value)
    {
        let next = if turin_contracts::base_schemas::js_trim(prompt).is_empty() {
            "Ultrathink:\n".to_owned()
        } else {
            crate::composer::provider_options::apply_claude_prompt_effort_prefix(prompt, Some("ultrathink"))
        };
        return TraitsSelectOutcome::PromptChange(next);
    }
    let is_primary = section.primary_select_id.as_deref() == Some(descriptor_id);
    if section.ultrathink_in_body_text && is_primary {
        return TraitsSelectOutcome::None;
    }
    let prompt = (section.ultrathink_prompt_controlled && is_primary).then(|| strip_ultrathink_prefix(prompt).to_owned());
    TraitsSelectOutcome::OptionsChange {
        selections: selections_after_pick(&section.descriptors, descriptor_id, &OptionValue::Str(value.to_owned())),
        prompt,
    }
}

#[cfg(test)]
mod tests;
