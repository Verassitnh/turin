//! Port of upstream/packages/shared/src/model.ts
//!
//! Option descriptors, option selections, model slugs, custom model entries, and the Claude
//! `ultrathink` prompt prefix. Provider option values are `TrimmedNonEmptyString | bool`, which the
//! contract models as [`ProviderOptionSelectionValue`].

use std::collections::HashSet;
use std::sync::LazyLock;

use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use turin_contracts::base_schemas::{TrimmedNonEmptyString, js_trim};
use turin_contracts::model::{
    CustomModelEntry, CustomModelSetting, ModelCapabilities, ProviderOptionDescriptor,
    ProviderOptionSelection, ProviderOptionSelectionValue, SelectProviderOptionDescriptor,
    model_slug_aliases_by_provider,
};
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::provider_instance::ProviderInstanceId;

use crate::array::copy_sorted;
use crate::js_string::{JS_WS, locale_compare};

const ULTRATHINK_EFFORT: &str = "ultrathink";

/// Choose the command for a model change against the thread's current provider instance.
pub fn model_selection_command_type(
    current_instance_id: &ProviderInstanceId,
    selection: &ModelSelection,
) -> &'static str {
    if current_instance_id == &selection.instance_id {
        "thread.model-selection.set"
    } else {
        "provider.switch"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectableModelOption {
    pub slug: String,
    pub name: String,
    /// Empty when the option has no aliases.
    pub aliases: Vec<String>,
}

pub fn create_model_capabilities(
    option_descriptors: &[ProviderOptionDescriptor],
) -> ModelCapabilities {
    ModelCapabilities {
        option_descriptors: Some(option_descriptors.to_vec()),
    }
}

fn raw_selection_value<'a>(
    selections: Option<&'a [ProviderOptionSelection]>,
    id: &str,
) -> Option<&'a ProviderOptionSelectionValue> {
    selections?
        .iter()
        .find(|selection| selection.id.as_str() == id)
        .map(|selection| &selection.value)
}

fn as_string(value: &ProviderOptionSelectionValue) -> Option<&str> {
    match value {
        ProviderOptionSelectionValue::Str(value) => Some(value.as_str()),
        ProviderOptionSelectionValue::Bool(_) => None,
    }
}

fn as_bool(value: &ProviderOptionSelectionValue) -> Option<bool> {
    match value {
        ProviderOptionSelectionValue::Bool(value) => Some(*value),
        ProviderOptionSelectionValue::Str(_) => None,
    }
}

pub fn get_provider_option_string_selection_value<'a>(
    selections: Option<&'a [ProviderOptionSelection]>,
    id: &str,
) -> Option<&'a str> {
    raw_selection_value(selections, id).and_then(as_string)
}

pub fn get_provider_option_boolean_selection_value(
    selections: Option<&[ProviderOptionSelection]>,
    id: &str,
) -> Option<bool> {
    raw_selection_value(selections, id).and_then(as_bool)
}

pub fn get_model_selection_string_option_value<'a>(
    model_selection: Option<&'a ModelSelection>,
    id: &str,
) -> Option<&'a str> {
    get_provider_option_string_selection_value(
        model_selection.and_then(|selection| selection.options.as_deref()),
        id,
    )
}

pub fn get_model_selection_boolean_option_value(
    model_selection: Option<&ModelSelection>,
    id: &str,
) -> Option<bool> {
    get_provider_option_boolean_selection_value(
        model_selection.and_then(|selection| selection.options.as_deref()),
        id,
    )
}

fn value_as_js_string(value: &ProviderOptionSelectionValue) -> String {
    match value {
        ProviderOptionSelectionValue::Str(value) => value.as_str().to_owned(),
        ProviderOptionSelectionValue::Bool(value) => value.to_string(),
    }
}

fn canonical_model_selection_options(
    selection: &ModelSelection,
) -> Vec<(&str, &ProviderOptionSelectionValue)> {
    let pairs: Vec<(&str, &ProviderOptionSelectionValue)> = selection
        .options
        .iter()
        .flatten()
        .map(|option| (option.id.as_str(), &option.value))
        .collect();
    copy_sorted(&pairs, |(left_id, left_value), (right_id, right_value)| {
        let id_order = locale_compare(left_id, right_id);
        if id_order != std::cmp::Ordering::Equal {
            return id_order;
        }
        locale_compare(
            &value_as_js_string(left_value),
            &value_as_js_string(right_value),
        )
    })
}

/// Compares the complete provider selection while treating option ordering and an omitted empty
/// option list as presentation details.
pub fn model_selections_equal(left: &ModelSelection, right: &ModelSelection) -> bool {
    if left.instance_id != right.instance_id || left.model != right.model {
        return false;
    }
    let left_options = canonical_model_selection_options(left);
    let right_options = canonical_model_selection_options(right);
    left_options.len() == right_options.len()
        && left_options.iter().zip(&right_options).all(
            |((left_id, left_value), (right_id, right_value))| {
                left_id == right_id && left_value == right_value
            },
        )
}

/// Trims a string, returning `None` for empty or missing values.
fn trim_or_none(value: Option<&str>) -> Option<&str> {
    value.map(js_trim).filter(|trimmed| !trimmed.is_empty())
}

fn default_option_id(descriptor: &SelectProviderOptionDescriptor) -> Option<String> {
    descriptor
        .options
        .iter()
        .find(|option| option.is_default == Some(true))
        .map(|option| option.id.as_str().to_owned())
}

fn current_choice_value(descriptor: &SelectProviderOptionDescriptor) -> Option<String> {
    descriptor
        .current_value
        .as_ref()
        .map(|value| value.as_str().to_owned())
}

fn resolve_descriptor_choice_value(
    descriptor: &SelectProviderOptionDescriptor,
    raw: Option<&str>,
) -> Option<String> {
    let Some(trimmed) = trim_or_none(raw) else {
        return current_choice_value(descriptor).or_else(|| default_option_id(descriptor));
    };
    if descriptor.options.is_empty() {
        return Some(trimmed.to_owned());
    }
    let prompt_injected = descriptor
        .prompt_injected_values
        .as_ref()
        .is_some_and(|values| values.iter().any(|value| value.as_str() == trimmed));
    if prompt_injected
        && descriptor
            .options
            .iter()
            .any(|option| option.id.as_str() == trimmed)
    {
        return default_option_id(descriptor);
    }
    if descriptor
        .options
        .iter()
        .any(|option| option.id.as_str() == trimmed)
    {
        return Some(trimmed.to_owned());
    }
    current_choice_value(descriptor).or_else(|| default_option_id(descriptor))
}

fn with_descriptor_current_value(
    descriptor: ProviderOptionDescriptor,
    raw: Option<&ProviderOptionSelectionValue>,
) -> ProviderOptionDescriptor {
    match descriptor {
        ProviderOptionDescriptor::Boolean(mut boolean) => {
            if let Some(ProviderOptionSelectionValue::Bool(value)) = raw {
                boolean.current_value = Some(*value);
            }
            ProviderOptionDescriptor::Boolean(boolean)
        }
        ProviderOptionDescriptor::Select(mut select) => {
            let resolved = match raw {
                Some(ProviderOptionSelectionValue::Str(value)) => {
                    resolve_descriptor_choice_value(&select, Some(value.as_str()))
                }
                _ => resolve_descriptor_choice_value(
                    &select,
                    current_choice_value(&select).as_deref(),
                ),
            };
            select.current_value =
                resolved.and_then(|value| TrimmedNonEmptyString::new(value).ok());
            ProviderOptionDescriptor::Select(select)
        }
    }
}

fn descriptor_id(descriptor: &ProviderOptionDescriptor) -> &str {
    match descriptor {
        ProviderOptionDescriptor::Select(select) => select.id.as_str(),
        ProviderOptionDescriptor::Boolean(boolean) => boolean.id.as_str(),
    }
}

fn descriptor_current_value(
    descriptor: &ProviderOptionDescriptor,
) -> Option<ProviderOptionSelectionValue> {
    match descriptor {
        ProviderOptionDescriptor::Select(select) => select
            .current_value
            .clone()
            .map(ProviderOptionSelectionValue::Str),
        ProviderOptionDescriptor::Boolean(boolean) => boolean
            .current_value
            .map(ProviderOptionSelectionValue::Bool),
    }
}

pub fn get_provider_option_descriptors(
    caps: &ModelCapabilities,
    selections: Option<&[ProviderOptionSelection]>,
) -> Vec<ProviderOptionDescriptor> {
    caps.option_descriptors
        .iter()
        .flatten()
        .map(|descriptor| {
            let raw = raw_selection_value(selections, descriptor_id(descriptor))
                .cloned()
                .or_else(|| descriptor_current_value(descriptor));
            with_descriptor_current_value(descriptor.clone(), raw.as_ref())
        })
        .collect()
}

fn reported_option_value(
    id: &str,
    selection: Option<&ModelSelection>,
    reported_selection: Option<&ModelSelection>,
) -> Option<ProviderOptionSelectionValue> {
    let (selection, reported_selection) = (selection?, reported_selection?);
    if selection.instance_id != reported_selection.instance_id
        || selection.model != reported_selection.model
        || selection
            .options
            .iter()
            .flatten()
            .any(|option| option.id.as_str() == id)
    {
        return None;
    }
    raw_selection_value(reported_selection.options.as_deref(), id).cloned()
}

/// The value a dispatch or display should use for `descriptor`. Provider-reported values are
/// display-only, so they win only when the selection does not set the option itself.
pub fn get_provider_option_current_value(
    descriptor: Option<&ProviderOptionDescriptor>,
    selection: Option<&ModelSelection>,
    reported_selection: Option<&ModelSelection>,
) -> Option<ProviderOptionSelectionValue> {
    let descriptor = descriptor?;
    let id = descriptor_id(descriptor);
    let has_explicit_option = selection.is_some_and(|selection| {
        selection
            .options
            .iter()
            .flatten()
            .any(|option| option.id.as_str() == id)
    });
    if let Some(reported) = reported_option_value(id, selection, reported_selection) {
        return Some(reported);
    }
    if id == "variant" && selection.is_some() && !has_explicit_option {
        return None;
    }
    match descriptor {
        ProviderOptionDescriptor::Boolean(boolean) => boolean
            .current_value
            .map(ProviderOptionSelectionValue::Bool),
        ProviderOptionDescriptor::Select(select) => {
            if let Some(current) = &select.current_value {
                return Some(ProviderOptionSelectionValue::Str(current.clone()));
            }
            default_option_id(select)
                .and_then(|id| TrimmedNonEmptyString::new(id).ok())
                .map(ProviderOptionSelectionValue::Str)
        }
    }
}

pub fn get_provider_option_current_label(
    descriptor: Option<&ProviderOptionDescriptor>,
    selection: Option<&ModelSelection>,
    reported_selection: Option<&ModelSelection>,
) -> Option<String> {
    let descriptor = descriptor?;
    let select = match descriptor {
        ProviderOptionDescriptor::Boolean(boolean) => {
            return boolean
                .current_value
                .map(|value| if value { "On" } else { "Off" }.to_owned());
        }
        ProviderOptionDescriptor::Select(select) => select,
    };
    let current_value =
        get_provider_option_current_value(Some(descriptor), selection, reported_selection);
    let current = current_value.as_ref().and_then(as_string);
    if let Some(label) = select
        .options
        .iter()
        .find(|option| Some(option.id.as_str()) == current)
        .map(|option| option.label.as_str())
    {
        return Some(label.to_owned());
    }
    let reported = reported_option_value(descriptor_id(descriptor), selection, reported_selection);
    if reported.as_ref().and_then(as_string) == Some("default") {
        return Some("Default".to_owned());
    }
    if descriptor_id(descriptor) == "variant" {
        return Some("Unknown".to_owned());
    }
    None
}

pub fn build_provider_option_selections_from_descriptors(
    descriptors: Option<&[ProviderOptionDescriptor]>,
) -> Option<Vec<ProviderOptionSelection>> {
    let descriptors = descriptors.filter(|descriptors| !descriptors.is_empty())?;
    let selections: Vec<ProviderOptionSelection> = descriptors
        .iter()
        .filter_map(|descriptor| {
            let value = get_provider_option_current_value(Some(descriptor), None, None)?;
            Some(ProviderOptionSelection {
                id: TrimmedNonEmptyString::new(descriptor_id(descriptor)).ok()?,
                value,
            })
        })
        .collect();
    (!selections.is_empty()).then_some(selections)
}

pub fn build_explicit_provider_option_selections_from_descriptors(
    descriptors: Option<&[ProviderOptionDescriptor]>,
    selections: Option<&[ProviderOptionSelection]>,
) -> Option<Vec<ProviderOptionSelection>> {
    let selections = selections.filter(|selections| !selections.is_empty())?;
    let explicit_ids: HashSet<&str> = selections
        .iter()
        .map(|selection| selection.id.as_str())
        .collect();
    let normalized: Vec<ProviderOptionSelection> =
        build_provider_option_selections_from_descriptors(descriptors)?
            .into_iter()
            .filter(|selection| explicit_ids.contains(selection.id.as_str()))
            .collect();
    (!normalized.is_empty()).then_some(normalized)
}

/// `/\bultrathink\b/i` with ASCII word boundaries and ASCII case folding, as JS evaluates it.
pub fn is_claude_ultrathink_prompt(text: Option<&str>) -> bool {
    const WORD: &str = "ultrathink";
    let Some(text) = text else {
        return false;
    };
    let bytes = text.as_bytes();
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    bytes
        .windows(WORD.len())
        .enumerate()
        .any(|(start, window)| {
            window.eq_ignore_ascii_case(WORD.as_bytes())
                && (start == 0 || !is_word(bytes[start - 1]))
                && bytes
                    .get(start + WORD.len())
                    .is_none_or(|byte| !is_word(*byte))
        })
}

/// Compares Codex model families without changing provider-owned dispatch identifiers.
pub fn codex_model_family(slug: &str) -> &str {
    if slug.starts_with("openai.gpt-") {
        &slug["openai.".len()..]
    } else {
        slug
    }
}

/// `name.replace(/^gpt/i, "GPT").replace(/-([a-z])/g, (_, c) => "-" + c.toUpperCase())`
pub fn format_codex_model_name(name: &str) -> String {
    let head = name
        .get(..3)
        .filter(|head| head.eq_ignore_ascii_case("gpt"))
        .map(|_| format!("GPT{}", &name[3..]))
        .unwrap_or_else(|| name.to_owned());
    let mut out = String::with_capacity(head.len());
    let mut chars = head.chars().peekable();
    while let Some(ch) = chars.next() {
        out.push(ch);
        if ch == '-' && chars.peek().is_some_and(|next| next.is_ascii_lowercase()) {
            let next = chars.next().expect("peeked");
            out.push(next.to_ascii_uppercase());
        }
    }
    out
}

static GPT_MODEL_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^gpt-[0-9]").expect("static regex"));

static CLAUDE_FAMILY_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(claude-(opus|sonnet|haiku|fable)|gemini|grok|composer)-[0-9]")
        .expect("static regex")
});

/// Puts the dot back into `claude-opus-4-6` style versions. Lookahead, so fancy-regex.
static CLAUDE_VERSION_DOT: LazyLock<FancyRegex> = LazyLock::new(|| {
    FancyRegex::new(r"(?i)^(claude-[a-z]+-[0-9]+)-([0-9]{1,2})(?=-|\[|$)").expect("static regex")
});

/// Formats a slug for display: `gpt-*` gets Codex casing, Claude/Gemini/Grok/Composer versions get
/// a title-cased name, and anything else is returned as-is.
pub fn format_model_slug_name(slug: &str) -> String {
    let separator = slug.rfind('/').map(|index| index + 1).unwrap_or(0);
    let prefix = &slug[..separator];
    let name = &slug[separator..];
    if GPT_MODEL_NAME.is_match(name) {
        return format!("{prefix}{}", format_codex_model_name(name));
    }
    if !CLAUDE_FAMILY_NAME.is_match(name) {
        return slug.to_owned();
    }
    let dotted = CLAUDE_VERSION_DOT.replace(name, "$1.$2").into_owned();
    let title_cased: Vec<String> = dotted
        .split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect();
    format!("{prefix}{}", title_cased.join(" "))
}

/// Upstream defaults `provider` to `codex`; Rust callers always pass the driver kind.
pub fn normalize_model_slug(model: Option<&str>, provider: &str) -> Option<String> {
    let trimmed = normalize_custom_model_slug(model)?;
    let aliased = model_slug_aliases_by_provider(provider)
        .unwrap_or(&[])
        .iter()
        .find(|(alias, _)| *alias == trimmed)
        .map(|(_, canonical)| (*canonical).to_owned());
    Some(aliased.unwrap_or(trimmed))
}

/// Custom model identifiers are provider-owned, so only trim them; never expand aliases.
pub fn normalize_custom_model_slug(model: Option<&str>) -> Option<String> {
    let trimmed = js_trim(model?);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// A custom model setting with its optional fields resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct CustomModelDefinition {
    pub slug: String,
    pub name: String,
    pub capabilities: Option<ModelCapabilities>,
}

/// Reads a `customModels` setting into resolved definitions. Accepts the typed union as well as
/// the opaque `providerInstances[id].config` blob clients see, so it tolerates bare slugs,
/// malformed rows, and unparseable capabilities (dropped rather than failing the whole list).
/// Slugs are trimmed and deduplicated, first occurrence wins; `name` falls back to the slug.
pub fn read_custom_model_entries(value: &serde_json::Value) -> Vec<CustomModelDefinition> {
    let Some(raw_entries) = value.as_array() else {
        return Vec::new();
    };
    let mut entries: Vec<CustomModelDefinition> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for raw in raw_entries {
        let record = match raw {
            serde_json::Value::String(slug) => serde_json::json!({ "slug": slug }),
            serde_json::Value::Object(_) => raw.clone(),
            _ => continue,
        };
        let Some(slug) =
            normalize_custom_model_slug(record.get("slug").and_then(|slug| slug.as_str()))
        else {
            continue;
        };
        if !seen.insert(slug.clone()) {
            continue;
        }
        let name = record
            .get("name")
            .and_then(|name| name.as_str())
            .and_then(|name| normalize_custom_model_slug(Some(name)))
            .unwrap_or_else(|| slug.clone());
        let capabilities = match record.get("capabilities") {
            None | Some(serde_json::Value::Null) => None,
            Some(raw_capabilities) => {
                serde_json::from_value::<ModelCapabilities>(raw_capabilities.clone())
                    .ok()
                    .map(|decoded| {
                        create_model_capabilities(&decoded.option_descriptors.unwrap_or_default())
                    })
            }
        };
        entries.push(CustomModelDefinition {
            slug,
            name,
            capabilities,
        });
    }
    entries
}

/// Writes a definition back to the compact stored shape: a bare slug when it carries nothing
/// custom, otherwise an entry with only the set fields.
pub fn to_custom_model_setting(entry: &CustomModelDefinition) -> CustomModelSetting {
    let descriptors = entry
        .capabilities
        .as_ref()
        .and_then(|capabilities| capabilities.option_descriptors.as_deref())
        .unwrap_or_default();
    let name = (!entry.name.is_empty() && entry.name != entry.slug).then(|| entry.name.clone());
    if name.is_none() && descriptors.is_empty() {
        return CustomModelSetting::Slug(entry.slug.clone());
    }
    CustomModelSetting::Entry(CustomModelEntry {
        slug: TrimmedNonEmptyString::new(&entry.slug)
            .expect("custom model slugs are trimmed and non-empty"),
        name: name.map(|name| {
            TrimmedNonEmptyString::new(name).expect("custom model names are trimmed and non-empty")
        }),
        capabilities: (!descriptors.is_empty()).then(|| create_model_capabilities(descriptors)),
    })
}

pub fn resolve_selectable_model(
    provider: &str,
    value: Option<&str>,
    options: &[SelectableModelOption],
) -> Option<String> {
    let trimmed = js_trim(value?);
    if trimmed.is_empty() {
        return None;
    }
    let lowered = trimmed.to_lowercase();
    if let Some(option) = options.iter().find(|option| option.slug == trimmed) {
        return Some(option.slug.clone());
    }
    if let Some(option) = options
        .iter()
        .find(|option| option.name.to_lowercase() == lowered)
    {
        return Some(option.slug.clone());
    }
    if let Some(option) = options.iter().find(|option| {
        option
            .aliases
            .iter()
            .any(|alias| alias.to_lowercase() == lowered)
    }) {
        return Some(option.slug.clone());
    }
    let normalized = normalize_model_slug(Some(trimmed), provider)?;
    options
        .iter()
        .find(|option| option.slug == normalized)
        .map(|option| option.slug.clone())
}

/// `optionSelections` is stored only when it has entries; an empty list is the same as none.
pub fn create_model_selection(
    instance_id: ProviderInstanceId,
    model: TrimmedNonEmptyString,
    options: Option<&[ProviderOptionSelection]>,
) -> ModelSelection {
    let selections = options
        .map(<[ProviderOptionSelection]>::to_vec)
        .unwrap_or_default();
    ModelSelection {
        instance_id,
        model,
        options: (!selections.is_empty()).then_some(selections),
    }
}

/// The effort value when any select descriptor lists it as prompt-injected, or `None`.
pub fn resolve_prompt_injected_effort(
    caps: &ModelCapabilities,
    raw_effort: Option<&str>,
) -> Option<String> {
    let trimmed = trim_or_none(raw_effort)?;
    get_provider_option_descriptors(caps, None)
        .iter()
        .any(|descriptor| match descriptor {
            ProviderOptionDescriptor::Select(select) => select
                .prompt_injected_values
                .as_ref()
                .is_some_and(|values| values.iter().any(|value| value.as_str() == trimmed)),
            ProviderOptionDescriptor::Boolean(_) => false,
        })
        .then(|| trimmed.to_owned())
}

/// `/^\/[^\s/]+(?:\s|$)/u`: a slash command, which the prefix would turn into prose.
static SLASH_COMMAND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^/[^{JS_WS}/]+(?:[{JS_WS}]|$)")).expect("static regex"));

pub fn apply_claude_prompt_effort_prefix(text: &str, effort: Option<&str>) -> String {
    let trimmed = js_trim(text);
    if trimmed.is_empty() {
        return trimmed.to_owned();
    }
    // Prefixing a slash command turns it into plain prose, so Claude never runs it. Command names
    // come from arbitrary file names ("/deploy.prod", "/plugin:skill"), so accept any first token
    // without a second slash; absolute paths like "/home/theo/app.ts" keep the prefix.
    if effort != Some(ULTRATHINK_EFFORT) || SLASH_COMMAND.is_match(trimmed) {
        return trimmed.to_owned();
    }
    if trimmed.starts_with("Ultrathink:") {
        return trimmed.to_owned();
    }
    format!("Ultrathink:\n{trimmed}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn descriptors(value: serde_json::Value) -> Vec<ProviderOptionDescriptor> {
        serde_json::from_value(value).expect("valid descriptors")
    }

    fn selections(value: serde_json::Value) -> Vec<ProviderOptionSelection> {
        serde_json::from_value(value).expect("valid selections")
    }

    fn codex_caps() -> ModelCapabilities {
        create_model_capabilities(&descriptors(json!([
            {
                "id": "reasoningEffort",
                "label": "Reasoning",
                "type": "select",
                "options": [
                    { "id": "xhigh", "label": "Extra High" },
                    { "id": "high", "label": "High", "isDefault": true },
                ],
                "currentValue": "high",
            },
            { "id": "fastMode", "label": "Fast Mode", "type": "boolean" },
        ])))
    }

    fn claude_caps() -> ModelCapabilities {
        create_model_capabilities(&descriptors(json!([
            {
                "id": "effort",
                "label": "Reasoning",
                "type": "select",
                "options": [
                    { "id": "medium", "label": "Medium" },
                    { "id": "high", "label": "High", "isDefault": true },
                    { "id": "ultrathink", "label": "Ultrathink" },
                ],
                "currentValue": "high",
                "promptInjectedValues": ["ultrathink"],
            },
            {
                "id": "contextWindow",
                "label": "Context Window",
                "type": "select",
                "options": [
                    { "id": "200k", "label": "200k" },
                    { "id": "1m", "label": "1M", "isDefault": true },
                ],
                "currentValue": "1m",
            },
        ])))
    }

    fn model_selection(
        instance: &str,
        model: &str,
        options: Option<serde_json::Value>,
    ) -> ModelSelection {
        let mut value = json!({ "instanceId": instance, "model": model });
        if let Some(options) = options {
            value["options"] = options;
        }
        serde_json::from_value(value).expect("valid selection")
    }

    #[test]
    fn codex_family_strips_only_the_openai_gpt_prefix() {
        assert_eq!(codex_model_family("openai.gpt-5.5"), "gpt-5.5");
        assert_eq!(codex_model_family("openai.other"), "openai.other");
        assert_eq!(codex_model_family("gpt-5.5"), "gpt-5.5");
    }

    #[test]
    fn codex_names_capitalize_gpt_and_dash_letters() {
        assert_eq!(format_codex_model_name("gpt-5.4-mini"), "GPT-5.4-Mini");
        assert_eq!(format_codex_model_name("GPT 5"), "GPT 5");
        assert_eq!(format_codex_model_name("Model-x"), "Model-X");
        assert_eq!(format_codex_model_name("gp"), "gp");
    }

    #[test]
    fn keeps_the_codex_catalog_display_formatting() {
        assert_eq!(
            format_codex_model_name("gpt-5.3-codex-spark"),
            "GPT-5.3-Codex-Spark"
        );
        assert_eq!(format_codex_model_name("GPT Test"), "GPT Test");
    }

    #[test]
    fn formats_known_model_ids_without_losing_their_qualifiers() {
        for (slug, expected) in [
            ("gpt-5.4", "GPT-5.4"),
            ("claude-opus-4-6", "Claude Opus 4.6"),
            ("claude-sonnet-4-20250514", "Claude Sonnet 4 20250514"),
            ("claude-opus-4-6[1m]", "Claude Opus 4.6[1m]"),
            ("openai/gpt-5.4-mini", "openai/GPT-5.4-Mini"),
            (
                "gemini-2.5-pro-preview-06-05",
                "Gemini 2.5 Pro Preview 06 05",
            ),
            ("custom/model-v2", "custom/model-v2"),
            ("gpt-proxy", "gpt-proxy"),
            ("My Custom Model", "My Custom Model"),
        ] {
            assert_eq!(format_model_slug_name(slug), expected, "{slug}");
        }
    }

    #[test]
    fn preserves_exact_custom_slugs_instead_of_expanding_provider_aliases() {
        // Claude aliases resolve through the model catalog, so the alias table passes unknown
        // slugs through unchanged.
        assert_eq!(
            normalize_model_slug(Some("opus"), "claudeAgent").as_deref(),
            Some("opus")
        );
        assert_eq!(
            normalize_custom_model_slug(Some(" opus ")).as_deref(),
            Some("opus")
        );
        // Codex aliases do canonicalize.
        assert_eq!(
            normalize_model_slug(Some("5.4"), "codex").as_deref(),
            Some("gpt-5.4")
        );
        assert_eq!(normalize_model_slug(Some("  "), "codex"), None);
    }

    #[test]
    fn applies_selection_values_to_capability_descriptors() {
        let resolved = get_provider_option_descriptors(
            &claude_caps(),
            Some(&selections(json!([
                { "id": "effort", "value": "medium" },
                { "id": "contextWindow", "value": "200k" },
            ]))),
        );
        assert_eq!(
            serde_json::to_value(&resolved).unwrap(),
            json!([
                {
                    "id": "effort",
                    "label": "Reasoning",
                    "type": "select",
                    "options": [
                        { "id": "medium", "label": "Medium" },
                        { "id": "high", "label": "High", "isDefault": true },
                        { "id": "ultrathink", "label": "Ultrathink" },
                    ],
                    "currentValue": "medium",
                    "promptInjectedValues": ["ultrathink"],
                },
                {
                    "id": "contextWindow",
                    "label": "Context Window",
                    "type": "select",
                    "options": [
                        { "id": "200k", "label": "200k" },
                        { "id": "1m", "label": "1M", "isDefault": true },
                    ],
                    "currentValue": "200k",
                },
            ])
        );
    }

    #[test]
    fn builds_wire_format_option_selections_from_descriptors() {
        let resolved = get_provider_option_descriptors(
            &codex_caps(),
            Some(&selections(json!([
                { "id": "reasoningEffort", "value": "high" },
                { "id": "fastMode", "value": true },
            ]))),
        );
        assert_eq!(
            serde_json::to_value(build_provider_option_selections_from_descriptors(Some(
                &resolved
            )))
            .unwrap(),
            json!([
                { "id": "reasoningEffort", "value": "high" },
                { "id": "fastMode", "value": true },
            ])
        );
    }

    #[test]
    fn builds_dispatch_options_only_from_explicit_selections() {
        let resolved = get_provider_option_descriptors(
            &codex_caps(),
            Some(&selections(json!([{ "id": "fastMode", "value": true }]))),
        );
        assert_eq!(
            build_explicit_provider_option_selections_from_descriptors(Some(&resolved), None),
            None
        );
        assert_eq!(
            serde_json::to_value(build_explicit_provider_option_selections_from_descriptors(
                Some(&resolved),
                Some(&selections(json!([{ "id": "fastMode", "value": true }]))),
            ))
            .unwrap(),
            json!([{ "id": "fastMode", "value": true }])
        );
    }

    #[test]
    fn stores_option_selection_arrays_in_model_selections() {
        let created = create_model_selection(
            ProviderInstanceId::new("codex").unwrap(),
            TrimmedNonEmptyString::new("gpt-5.4").unwrap(),
            Some(&selections(json!([
                { "id": "reasoningEffort", "value": "high" },
                { "id": "fastMode", "value": true },
            ]))),
        );
        assert_eq!(
            serde_json::to_value(&created).unwrap(),
            json!({
                "instanceId": "codex",
                "model": "gpt-5.4",
                "options": [
                    { "id": "reasoningEffort", "value": "high" },
                    { "id": "fastMode", "value": true },
                ],
            })
        );
        let empty = create_model_selection(
            ProviderInstanceId::new("codex").unwrap(),
            TrimmedNonEmptyString::new("gpt-5.4").unwrap(),
            Some(&[]),
        );
        assert_eq!(empty.options, None);
    }

    #[test]
    fn reads_typed_option_selection_values() {
        let selection = model_selection(
            "codex",
            "gpt-5.4",
            Some(json!([
                { "id": "reasoningEffort", "value": "high" },
                { "id": "fastMode", "value": true },
            ])),
        );
        let options = selection.options.as_deref();
        assert_eq!(
            get_provider_option_string_selection_value(options, "reasoningEffort"),
            Some("high")
        );
        assert_eq!(
            get_provider_option_string_selection_value(options, "fastMode"),
            None
        );
        assert_eq!(
            get_provider_option_boolean_selection_value(options, "fastMode"),
            Some(true)
        );
        assert_eq!(
            get_provider_option_boolean_selection_value(options, "reasoningEffort"),
            None
        );
        assert_eq!(
            get_model_selection_string_option_value(Some(&selection), "reasoningEffort"),
            Some("high")
        );
        assert_eq!(
            get_model_selection_boolean_option_value(Some(&selection), "fastMode"),
            Some(true)
        );
    }

    #[test]
    fn compares_complete_model_selections_independent_of_option_ordering() {
        let left = model_selection(
            "codex",
            "gpt-5.4",
            Some(json!([
                { "id": "reasoningEffort", "value": "high" },
                { "id": "fastMode", "value": true },
            ])),
        );
        let reordered = model_selection(
            "codex",
            "gpt-5.4",
            Some(json!([
                { "id": "fastMode", "value": true },
                { "id": "reasoningEffort", "value": "high" },
            ])),
        );
        assert!(model_selections_equal(&left, &reordered));
        let different_value = model_selection(
            "codex",
            "gpt-5.4",
            Some(json!([
                { "id": "fastMode", "value": true },
                { "id": "reasoningEffort", "value": "medium" },
            ])),
        );
        assert!(!model_selections_equal(&left, &different_value));
        let different_model = model_selection(
            "codex",
            "gpt-5.5",
            Some(json!([
                { "id": "fastMode", "value": true },
                { "id": "reasoningEffort", "value": "high" },
            ])),
        );
        assert!(!model_selections_equal(&left, &different_model));
    }

    #[test]
    fn keeps_slash_commands_intact_when_ultrathink_is_selected() {
        for text in [
            "/compact",
            " /compact keep recent errors ",
            " /review src/model.ts ",
            "/security-review",
            "/plugin:skill run",
            "/deploy.prod to staging",
        ] {
            let expected = js_trim(text);
            assert_eq!(
                apply_claude_prompt_effort_prefix(text, Some("ultrathink")),
                expected,
                "{text}"
            );
        }
    }

    #[test]
    fn still_adds_the_ultrathink_prefix_to_ordinary_prompts() {
        assert_eq!(
            apply_claude_prompt_effort_prefix("Investigate this failure", Some("ultrathink")),
            "Ultrathink:\nInvestigate this failure"
        );
        assert_eq!(
            apply_claude_prompt_effort_prefix(
                "/home/theo/app.ts crashed on load",
                Some("ultrathink")
            ),
            "Ultrathink:\n/home/theo/app.ts crashed on load"
        );
        assert_eq!(
            apply_claude_prompt_effort_prefix("Ultrathink:\nalready", Some("ultrathink")),
            "Ultrathink:\nalready"
        );
        assert_eq!(
            apply_claude_prompt_effort_prefix("  ", Some("ultrathink")),
            ""
        );
    }

    #[test]
    fn detects_ultrathink_prompts_on_word_boundaries() {
        assert!(is_claude_ultrathink_prompt(Some(
            "please ULTRATHINK about this"
        )));
        assert!(is_claude_ultrathink_prompt(Some("ultrathink")));
        assert!(!is_claude_ultrathink_prompt(Some("ultrathinking")));
        assert!(!is_claude_ultrathink_prompt(Some("my_ultrathink")));
        assert!(!is_claude_ultrathink_prompt(None));
    }

    #[test]
    fn resolves_custom_model_entries_trimming_and_deduplicating_on_slug() {
        let capabilities = json!({
            "optionDescriptors": [{
                "id": "effort",
                "label": "Reasoning",
                "type": "select",
                "options": [{ "id": "high", "label": "High", "isDefault": true }],
                "currentValue": "high",
            }],
        });
        let entries = read_custom_model_entries(&json!([
            " bare ",
            { "slug": "named", "name": " Named ", "capabilities": capabilities.clone() },
            "bare",
            { "slug": "named", "name": "Second" },
            "",
            { "name": "no slug" },
            42,
        ]));
        assert_eq!(
            entries,
            vec![
                CustomModelDefinition {
                    slug: "bare".to_owned(),
                    name: "bare".to_owned(),
                    capabilities: None,
                },
                CustomModelDefinition {
                    slug: "named".to_owned(),
                    name: "Named".to_owned(),
                    capabilities: Some(serde_json::from_value(capabilities).unwrap()),
                },
            ]
        );
    }

    #[test]
    fn drops_unparseable_capabilities_but_keeps_the_entry() {
        assert_eq!(
            read_custom_model_entries(
                &json!([{ "slug": "x", "capabilities": { "optionDescriptors": "nope" } }])
            ),
            vec![CustomModelDefinition {
                slug: "x".to_owned(),
                name: "x".to_owned(),
                capabilities: None,
            }]
        );
        assert!(read_custom_model_entries(&json!("not a list")).is_empty());
    }

    #[test]
    fn writes_the_compact_stored_shape_back() {
        let bare = CustomModelDefinition {
            slug: "x".to_owned(),
            name: "x".to_owned(),
            capabilities: None,
        };
        assert_eq!(
            to_custom_model_setting(&bare),
            CustomModelSetting::Slug("x".to_owned())
        );
        let empty_capabilities = CustomModelDefinition {
            capabilities: Some(create_model_capabilities(&[])),
            ..bare.clone()
        };
        assert_eq!(
            to_custom_model_setting(&empty_capabilities),
            CustomModelSetting::Slug("x".to_owned())
        );
        let named = CustomModelDefinition {
            name: "X".to_owned(),
            ..bare
        };
        assert_eq!(
            serde_json::to_value(to_custom_model_setting(&named)).unwrap(),
            json!({ "slug": "x", "name": "X" })
        );
    }

    #[test]
    fn resolves_selectable_models_by_slug_name_alias_then_normalized_slug() {
        let options = [SelectableModelOption {
            slug: "gpt-5.4".to_owned(),
            name: "GPT Five".to_owned(),
            aliases: vec!["flagship".to_owned()],
        }];
        assert_eq!(
            resolve_selectable_model("codex", Some(" gpt-5.4 "), &options).as_deref(),
            Some("gpt-5.4")
        );
        assert_eq!(
            resolve_selectable_model("codex", Some("gpt five"), &options).as_deref(),
            Some("gpt-5.4")
        );
        assert_eq!(
            resolve_selectable_model("codex", Some("Flagship"), &options).as_deref(),
            Some("gpt-5.4")
        );
        assert_eq!(
            resolve_selectable_model("codex", Some("5.4"), &options).as_deref(),
            Some("gpt-5.4")
        );
        assert_eq!(
            resolve_selectable_model("codex", Some("missing"), &options),
            None
        );
        assert_eq!(
            resolve_selectable_model("codex", Some("  "), &options),
            None
        );
    }

    #[test]
    fn resolves_prompt_injected_effort_from_any_select_descriptor() {
        assert_eq!(
            resolve_prompt_injected_effort(&claude_caps(), Some("ultrathink")).as_deref(),
            Some("ultrathink")
        );
        assert_eq!(
            resolve_prompt_injected_effort(&claude_caps(), Some("medium")),
            None
        );
        assert_eq!(
            resolve_prompt_injected_effort(&codex_caps(), Some("ultrathink")),
            None
        );
    }

    #[test]
    fn provider_reported_option_display() {
        let selection = model_selection("opencode", "ling", None);
        let reported = model_selection(
            "opencode",
            "ling",
            Some(json!([{ "id": "variant", "value": "default" }])),
        );
        let descriptor = ProviderOptionDescriptor::Select(
            serde_json::from_value(json!({
                "id": "variant",
                "label": "Reasoning",
                "type": "select",
                "options": [{ "id": "none", "label": "None" }, { "id": "thinking", "label": "Thinking" }],
            }))
            .unwrap(),
        );

        // Explicit reports without adding a choice or a dispatch option.
        assert_eq!(
            get_provider_option_current_label(Some(&descriptor), Some(&selection), Some(&reported))
                .as_deref(),
            Some("Default")
        );
        let thinking = model_selection(
            "opencode",
            "ling",
            Some(json!([{ "id": "variant", "value": "thinking" }])),
        );
        assert_eq!(
            get_provider_option_current_label(Some(&descriptor), Some(&selection), Some(&thinking))
                .as_deref(),
            Some("Thinking")
        );
        // A descriptor's own current value applies when the selection sets the option.
        let none_current = ProviderOptionDescriptor::Select(
            serde_json::from_value(json!({
                "id": "variant",
                "label": "Reasoning",
                "type": "select",
                "options": [{ "id": "none", "label": "None" }, { "id": "thinking", "label": "Thinking" }],
                "currentValue": "none",
            }))
            .unwrap(),
        );
        assert_eq!(
            get_provider_option_current_label(
                Some(&none_current),
                Some(&model_selection(
                    "opencode",
                    "ling",
                    Some(json!([{ "id": "variant", "value": "none" }]))
                )),
                Some(&reported),
            )
            .as_deref(),
            Some("None")
        );
        assert_eq!(
            get_provider_option_current_label(Some(&none_current), Some(&selection), None)
                .as_deref(),
            Some("Unknown")
        );
        assert_eq!(
            get_provider_option_current_label(Some(&descriptor), Some(&selection), None).as_deref(),
            Some("Unknown")
        );
        assert_eq!(
            build_provider_option_selections_from_descriptors(Some(std::slice::from_ref(
                &descriptor
            ))),
            None
        );
    }

    #[test]
    fn ignores_reports_after_changing_the_model_instance_or_option() {
        let descriptor = ProviderOptionDescriptor::Select(
            serde_json::from_value(json!({
                "id": "variant",
                "label": "Reasoning",
                "type": "select",
                "options": [{ "id": "none", "label": "None" }, { "id": "thinking", "label": "Thinking" }],
            }))
            .unwrap(),
        );
        let reported = model_selection(
            "opencode",
            "ling",
            Some(json!([{ "id": "variant", "value": "default" }])),
        );
        for changed in [
            model_selection("opencode", "other", None),
            model_selection("other", "ling", None),
            model_selection(
                "opencode",
                "ling",
                Some(json!([{ "id": "variant", "value": "none" }])),
            ),
        ] {
            assert_eq!(
                get_provider_option_current_label(
                    Some(&descriptor),
                    Some(&changed),
                    Some(&reported)
                )
                .as_deref(),
                Some("Unknown"),
            );
        }
    }
}
