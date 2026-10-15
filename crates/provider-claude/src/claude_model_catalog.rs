//! Port of upstream/apps/server/src/provider/ClaudeModelCatalog.ts.
//!
//! The catalog is built from the manifest's Claude entry (see
//! `claude_model_manifest`) and falls back to the bundled entry when the
//! given one is missing or has invalid adapters.
use crate::claude_model_manifest::{
    CLAUDE_DRIVER, ClaudeCodeCompatibility, ClaudeCodeProfile, ProviderCatalog,
    bundled_model_manifest, decode_claude_model_adapter, decode_claude_profile_adapter,
    resolve_provider_catalog,
};
use std::{cmp::Ordering, sync::OnceLock};
use turin_contracts::{
    TrimmedNonEmptyString,
    model::{
        BooleanProviderOptionDescriptor, CustomModelEntry, CustomModelSetting, ModelCapabilities,
        ProviderOptionDescriptor, ProviderOptionSelection, ProviderOptionSelectionValue,
        SelectProviderOptionDescriptor,
    },
    model_selection::ModelSelection,
};
use turin_provider_core::{
    semver::compare_semver_versions,
    server_provider::{ServerProviderModel, ServerProviderUpdateRequiredModel},
};

/// `EMPTY_CAPABILITIES`: a model without capabilities presents no options.
fn empty_capabilities() -> ModelCapabilities {
    ModelCapabilities {
        option_descriptors: Some(Vec::new()),
    }
}

#[derive(Debug, Clone)]
pub struct ClaudeCatalogModel {
    pub model: ServerProviderModel,
    pub runtime: ClaudeCodeProfile,
    pub compatibility: ClaudeCodeCompatibility,
}

#[derive(Debug, Clone, Default)]
pub struct ClaudeModelCatalog {
    pub models: Vec<ClaudeCatalogModel>,
}

fn tne(value: &str) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

/// `catalogServerModel`: a manifest entry as the snapshot model it presents.
fn catalog_server_model(
    catalog: &ProviderCatalog,
    entry: &crate::claude_model_manifest::ProviderCatalogModel,
) -> Option<ServerProviderModel> {
    Some(ServerProviderModel {
        slug: tne(&entry.slug)?,
        name: tne(&entry.name)?,
        short_name: entry.short_name.as_deref().and_then(tne),
        sub_provider: entry.sub_provider.as_deref().and_then(tne),
        aliases: match &entry.aliases {
            Some(aliases) => Some(
                aliases
                    .iter()
                    .map(|alias| tne(alias))
                    .collect::<Option<Vec<_>>>()?,
            ),
            None => None,
        },
        badge: entry.badge,
        is_custom: false,
        is_default: (catalog.default_chat_model.as_deref() == Some(entry.slug.as_str()))
            .then_some(true),
        is_legacy: entry.is_legacy.then_some(true),
        capabilities: entry.capabilities.clone(),
    })
}

fn try_resolve_claude_model_catalog(
    resolved: Option<&ProviderCatalog>,
) -> Option<ClaudeModelCatalog> {
    let resolved = resolved?;
    let mut models = Vec::with_capacity(resolved.models.len());
    for entry in &resolved.models {
        let profile = decode_claude_profile_adapter(entry.profile_adapter.as_ref())?;
        let adapter = decode_claude_model_adapter(entry.adapter.as_ref())?;
        models.push(ClaudeCatalogModel {
            model: catalog_server_model(resolved, entry)?,
            runtime: profile.claude_code.unwrap_or_default(),
            compatibility: adapter.claude_code.unwrap_or_default(),
        });
    }
    Some(ClaudeModelCatalog { models })
}

/// `BUNDLED_CLAUDE_PROVIDER_CATALOG`: the Claude entry of the bundled manifest.
fn bundled_claude_provider_catalog() -> Option<&'static ProviderCatalog> {
    static CATALOG: OnceLock<Option<ProviderCatalog>> = OnceLock::new();
    CATALOG
        .get_or_init(|| resolve_provider_catalog(bundled_model_manifest(), CLAUDE_DRIVER))
        .as_ref()
}

/// Claude's catalog from its manifest entry, falling back to the bundled entry
/// when the current one is missing or has invalid adapters.
pub fn resolve_claude_model_catalog(catalog: Option<&ProviderCatalog>) -> ClaudeModelCatalog {
    try_resolve_claude_model_catalog(catalog)
        .or_else(|| try_resolve_claude_model_catalog(bundled_claude_provider_catalog()))
        .unwrap_or_default()
}

/// `BUNDLED_CLAUDE_MODEL_CATALOG`.
pub fn bundled_claude_model_catalog() -> &'static ClaudeModelCatalog {
    static CATALOG: OnceLock<ClaudeModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| resolve_claude_model_catalog(bundled_claude_provider_catalog()))
}

/// `readCustomModelEntries` from `@t3tools/shared/model`: trimmed, non-empty,
/// first occurrence wins, and a bare slug has no capabilities.
#[derive(Debug, Clone)]
pub(crate) struct CustomModelDefinition {
    pub(crate) slug: String,
    pub(crate) name: String,
    pub(crate) capabilities: Option<ModelCapabilities>,
}

pub(crate) fn read_custom_model_entries(
    values: &[CustomModelSetting],
) -> Vec<CustomModelDefinition> {
    let mut entries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for value in values {
        let (slug, name, capabilities) = match value {
            CustomModelSetting::Slug(slug) => (slug.as_str(), None, None),
            CustomModelSetting::Entry(CustomModelEntry {
                slug,
                name,
                capabilities,
            }) => (
                slug.as_str(),
                name.as_ref().map(|n| n.as_str()),
                capabilities.as_ref(),
            ),
        };
        let slug = slug.trim();
        if slug.is_empty() || !seen.insert(slug.to_owned()) {
            continue;
        }
        let name = name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(slug);
        entries.push(CustomModelDefinition {
            slug: slug.to_owned(),
            name: name.to_owned(),
            capabilities: capabilities.map(|caps| ModelCapabilities {
                option_descriptors: Some(caps.option_descriptors.clone().unwrap_or_default()),
            }),
        });
    }
    entries
}

/// `scopeClaudeModelCatalog`: scope the catalog to one instance's settings.
/// Custom model slugs stay opaque (a built-in alias they shadow is dropped,
/// canonical slugs and capabilities are preserved), and custom entries that
/// declare their own capabilities are appended so the adapter resolves effort /
/// fast mode / thinking against the user's descriptors. Custom entries carry
/// no runtime profile, so option values pass through to Claude Code verbatim.
pub fn scope_claude_model_catalog(
    catalog: &ClaudeModelCatalog,
    custom_models: &[CustomModelSetting],
) -> ClaudeModelCatalog {
    let custom_entries = read_custom_model_entries(custom_models);
    if custom_entries.is_empty() {
        return catalog.clone();
    }
    let custom_aliases: std::collections::HashSet<String> = custom_entries
        .iter()
        .map(|entry| entry.slug.to_lowercase())
        .collect();

    let mut models: Vec<ClaudeCatalogModel> = catalog
        .models
        .iter()
        .map(|entry| {
            let Some(aliases) = &entry.model.aliases else {
                return entry.clone();
            };
            if !aliases
                .iter()
                .any(|alias| custom_aliases.contains(&alias.as_str().to_lowercase()))
            {
                return entry.clone();
            }
            let mut scoped = entry.clone();
            scoped.model.aliases = Some(
                aliases
                    .iter()
                    .filter(|alias| !custom_aliases.contains(&alias.as_str().to_lowercase()))
                    .cloned()
                    .collect(),
            );
            scoped
        })
        .collect();
    let built_in_slugs: std::collections::HashSet<String> = models
        .iter()
        .map(|entry| entry.model.slug.as_str().to_owned())
        .collect();
    for entry in custom_entries {
        let Some(capabilities) = entry.capabilities else {
            continue;
        };
        if built_in_slugs.contains(&entry.slug) {
            continue;
        }
        let (Some(slug), Some(name)) = (tne(&entry.slug), tne(&entry.name)) else {
            continue;
        };
        models.push(ClaudeCatalogModel {
            model: ServerProviderModel {
                slug,
                name,
                short_name: None,
                sub_provider: None,
                aliases: None,
                badge: None,
                is_custom: true,
                is_default: None,
                is_legacy: None,
                capabilities: Some(capabilities),
            },
            runtime: ClaudeCodeProfile::default(),
            compatibility: ClaudeCodeCompatibility::default(),
        });
    }
    ClaudeModelCatalog { models }
}

/// `resolveClaudeCatalogModel`: exact slug first, then a case-insensitive alias.
fn resolve_claude_catalog_model<'a>(
    catalog: &'a ClaudeModelCatalog,
    slug_or_alias: Option<&str>,
) -> Option<&'a ClaudeCatalogModel> {
    let value = slug_or_alias?.trim();
    if value.is_empty() {
        return None;
    }
    let lowered = value.to_lowercase();
    catalog
        .models
        .iter()
        .find(|entry| entry.model.slug.as_str() == value)
        .or_else(|| {
            catalog.models.iter().find(|entry| {
                entry.model.aliases.as_ref().is_some_and(|aliases| {
                    aliases
                        .iter()
                        .any(|alias| alias.as_str().to_lowercase() == lowered)
                })
            })
        })
}

pub fn resolve_claude_model_slug(catalog: &ClaudeModelCatalog, slug_or_alias: &str) -> String {
    resolve_claude_catalog_model(catalog, Some(slug_or_alias))
        .map(|entry| entry.model.slug.as_str().to_owned())
        .unwrap_or_else(|| slug_or_alias.to_owned())
}

/// `getClaudeCatalogModelCapabilities`.
pub fn get_claude_catalog_model_capabilities(
    catalog: &ClaudeModelCatalog,
    slug_or_alias: Option<&str>,
) -> ModelCapabilities {
    resolve_claude_catalog_model(catalog, slug_or_alias)
        .and_then(|entry| entry.model.capabilities.clone())
        .unwrap_or_else(empty_capabilities)
}

fn is_version_supported(compatibility: &ClaudeCodeCompatibility, version: Option<&str>) -> bool {
    if compatibility.min_version.is_none() && compatibility.max_version_exclusive.is_none() {
        return true;
    }
    let Some(version) = version.filter(|version| !version.is_empty()) else {
        return false;
    };
    if let Some(min) = &compatibility.min_version
        && compare_semver_versions(version, min) == Ordering::Less
    {
        return false;
    }
    !matches!(
        &compatibility.max_version_exclusive,
        Some(max) if compare_semver_versions(version, max) != Ordering::Less
    )
}

/// `resolveClaudeModelsForVersion`: models the installed Claude Code can run.
pub fn resolve_claude_models_for_version(
    catalog: &ClaudeModelCatalog,
    version: Option<&str>,
) -> Vec<ServerProviderModel> {
    catalog
        .models
        .iter()
        .filter(|entry| is_version_supported(&entry.compatibility, version))
        .map(|entry| entry.model.clone())
        .collect()
}

/// Current catalog models the installed Claude Code is too old to run, so the
/// picker can show them as "update to use" instead of hiding them.
pub fn resolve_claude_update_required_models(
    catalog: &ClaudeModelCatalog,
    version: Option<&str>,
) -> Vec<ServerProviderUpdateRequiredModel> {
    let Some(version) = version.filter(|version| !version.is_empty()) else {
        return Vec::new();
    };
    catalog
        .models
        .iter()
        .filter_map(|entry| {
            let min_version = entry.compatibility.min_version.as_deref()?;
            if entry.model.is_legacy == Some(true)
                || compare_semver_versions(version, min_version) != Ordering::Less
            {
                return None;
            }
            Some(ServerProviderUpdateRequiredModel {
                slug: entry.model.slug.clone(),
                name: entry.model.name.clone(),
                badge: entry.model.badge,
                min_version: tne(min_version)?,
            })
        })
        .collect()
}

/// `formatClaudeVersionUpgradeMessage`: names the newest-gated model the
/// installed version cannot run, the one with the lowest minimum version.
pub fn format_claude_version_upgrade_message(
    catalog: &ClaudeModelCatalog,
    version: Option<&str>,
) -> Option<String> {
    let version = version.filter(|version| !version.is_empty());
    let unavailable = catalog
        .models
        .iter()
        .filter(|entry| {
            entry
                .compatibility
                .min_version
                .as_deref()
                .is_some_and(|min| {
                    version.is_none_or(|version| {
                        compare_semver_versions(version, min) == Ordering::Less
                    })
                })
        })
        .min_by(|left, right| {
            compare_semver_versions(
                left.compatibility
                    .min_version
                    .as_deref()
                    .unwrap_or_default(),
                right
                    .compatibility
                    .min_version
                    .as_deref()
                    .unwrap_or_default(),
            )
        })?;
    let min_version = unavailable.compatibility.min_version.as_deref()?;
    let version_label = match version {
        Some(version) => format!("v{version}"),
        None => "the installed version".to_owned(),
    };
    Some(format!(
        "Claude Code {version_label} is too old for {}. Upgrade to v{min_version} or newer to access it.",
        unavailable.model.name.as_str()
    ))
}

/// `getProviderOptionDescriptors` from `@t3tools/shared/model`: the model's
/// descriptors with `currentValue` resolved from the selections.
pub fn get_provider_option_descriptors(
    caps: &ModelCapabilities,
    selections: Option<&[ProviderOptionSelection]>,
) -> Vec<ProviderOptionDescriptor> {
    caps.option_descriptors
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|descriptor| {
            let raw = selections
                .and_then(|selections| {
                    selections
                        .iter()
                        .find(|s| s.id.as_str() == descriptor_id(descriptor))
                })
                .map(|selection| selection.value.clone());
            with_descriptor_current_value(descriptor, raw)
        })
        .collect()
}

fn descriptor_id(descriptor: &ProviderOptionDescriptor) -> &str {
    match descriptor {
        ProviderOptionDescriptor::Select(select) => select.id.as_str(),
        ProviderOptionDescriptor::Boolean(boolean) => boolean.id.as_str(),
    }
}

fn with_descriptor_current_value(
    descriptor: &ProviderOptionDescriptor,
    raw: Option<ProviderOptionSelectionValue>,
) -> ProviderOptionDescriptor {
    match descriptor {
        ProviderOptionDescriptor::Boolean(boolean) => {
            let mut next: BooleanProviderOptionDescriptor = boolean.clone();
            if let Some(ProviderOptionSelectionValue::Bool(value)) = raw {
                next.current_value = Some(value);
            }
            ProviderOptionDescriptor::Boolean(next)
        }
        ProviderOptionDescriptor::Select(select) => {
            // Upstream falls back to the descriptor's own `currentValue` when the
            // selection is absent or not a string.
            let raw = match raw {
                Some(ProviderOptionSelectionValue::Str(value)) => Some(value.as_str().to_owned()),
                _ => select.current_value.as_ref().map(|v| v.as_str().to_owned()),
            };
            let mut next: SelectProviderOptionDescriptor = select.clone();
            next.current_value = resolve_descriptor_choice_value(select, raw.as_deref())
                .and_then(|value| tne(&value));
            ProviderOptionDescriptor::Select(next)
        }
    }
}

/// `resolveDescriptorChoiceValue`.
fn resolve_descriptor_choice_value(
    descriptor: &SelectProviderOptionDescriptor,
    raw: Option<&str>,
) -> Option<String> {
    let default_or_current = || {
        descriptor
            .current_value
            .as_ref()
            .map(|value| value.as_str().to_owned())
            .or_else(|| default_option_id(descriptor))
    };
    let trimmed = raw.map(str::trim).filter(|value| !value.is_empty());
    let Some(trimmed) = trimmed else {
        return default_or_current();
    };
    if descriptor.options.is_empty() {
        return Some(trimmed.to_owned());
    }
    let has_option = descriptor
        .options
        .iter()
        .any(|option| option.id.as_str() == trimmed);
    let prompt_injected = descriptor
        .prompt_injected_values
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|value| value.as_str() == trimmed);
    if prompt_injected && has_option {
        return default_option_id(descriptor);
    }
    if has_option {
        return Some(trimmed.to_owned());
    }
    default_or_current()
}

fn default_option_id(descriptor: &SelectProviderOptionDescriptor) -> Option<String> {
    descriptor
        .options
        .iter()
        .find(|option| option.is_default == Some(true))
        .map(|option| option.id.as_str().to_owned())
}

/// `getProviderOptionCurrentValue` for a select descriptor: the resolved
/// `currentValue`, else the default option. Boolean descriptors have no string value.
fn select_current_value(descriptor: &ProviderOptionDescriptor) -> Option<String> {
    match descriptor {
        ProviderOptionDescriptor::Select(select) => select
            .current_value
            .as_ref()
            .map(|value| value.as_str().to_owned())
            .or_else(|| default_option_id(select)),
        ProviderOptionDescriptor::Boolean(_) => None,
    }
}

fn current_string_option(descriptors: &[ProviderOptionDescriptor], id: &str) -> Option<String> {
    descriptors
        .iter()
        .find(|descriptor| descriptor_id(descriptor) == id)
        .and_then(select_current_value)
}

/// `resolveClaudeCatalogEffort`: the effort a raw value resolves to for a model.
pub fn resolve_claude_catalog_effort(
    catalog: &ClaudeModelCatalog,
    model: Option<&str>,
    raw: Option<&str>,
) -> Option<String> {
    let caps = get_claude_catalog_model_capabilities(catalog, model);
    let selections = selection_for("effort", raw);
    let descriptors = get_provider_option_descriptors(&caps, selections.as_deref());
    current_string_option(&descriptors, "effort")
}

/// `normalizeClaudeCatalogEffort`: maps an effort through the model's
/// `effortMap`. Unmapped efforts pass through, and a mapping to `null` drops it.
pub fn normalize_claude_catalog_effort(
    catalog: &ClaudeModelCatalog,
    effort: Option<&str>,
    model: Option<&str>,
) -> Option<String> {
    let effort = effort.filter(|effort| !effort.is_empty())?;
    let Some(effort_map) = resolve_claude_catalog_model(catalog, model)
        .and_then(|entry| entry.runtime.effort_map.as_ref())
    else {
        return Some(effort.to_owned());
    };
    match effort_map.iter().find(|(key, _)| key.as_str() == effort) {
        None => Some(effort.to_owned()),
        Some((_, mapped)) => mapped.as_ref().map(|value| value.as_str().to_owned()),
    }
}

/// `isClaudeCatalogUltracodeEffort`.
pub fn is_claude_catalog_ultracode_effort(effort: Option<&str>) -> bool {
    effort == Some("ultracode")
}

/// `resolveClaudeCatalogApiModelId`: the slug plus the first model suffix the
/// selection's options select, in the runtime's suffix order.
pub fn resolve_claude_catalog_api_model_id(
    catalog: &ClaudeModelCatalog,
    model_selection: &ModelSelection,
) -> String {
    let entry = resolve_claude_catalog_model(catalog, Some(model_selection.model.as_str()));
    let slug = entry
        .map(|entry| entry.model.slug.as_str().to_owned())
        .unwrap_or_else(|| model_selection.model.as_str().to_owned());
    let caps = entry
        .and_then(|entry| entry.model.capabilities.clone())
        .unwrap_or_else(empty_capabilities);
    let descriptors = get_provider_option_descriptors(&caps, model_selection.options.as_deref());
    if let Some(suffixes) = entry.and_then(|entry| entry.runtime.model_suffixes.as_ref()) {
        for (option_id, values) in suffixes {
            let Some(value) = current_string_option(&descriptors, option_id.as_str()) else {
                continue;
            };
            if let Some(suffix) = values
                .iter()
                .find(|(key, _)| key.as_str() == value)
                .map(|(_, suffix)| suffix.as_str())
            {
                return format!("{slug}{suffix}");
            }
        }
    }
    slug
}

fn selection_for(id: &str, raw: Option<&str>) -> Option<Vec<ProviderOptionSelection>> {
    let value = raw.filter(|raw| !raw.is_empty())?;
    Some(vec![ProviderOptionSelection {
        id: tne(id)?,
        value: ProviderOptionSelectionValue::Str(tne(value)?),
    }])
}

/// `getModelSelectionStringOptionValue`.
fn model_selection_string_option(selection: &ModelSelection, id: &str) -> Option<String> {
    selection
        .options
        .as_deref()?
        .iter()
        .find(|option| option.id.as_str() == id)
        .and_then(|option| match &option.value {
            ProviderOptionSelectionValue::Str(value) => Some(value.as_str().to_owned()),
            ProviderOptionSelectionValue::Bool(_) => None,
        })
}

/// `resolveClaudeCatalogContextWindowTokens`: a fixed window wins; otherwise
/// the selected `contextWindow` option maps to a token count.
pub fn resolve_claude_catalog_context_window_tokens(
    catalog: &ClaudeModelCatalog,
    model_selection: Option<&ModelSelection>,
) -> Option<f64> {
    let entry = resolve_claude_catalog_model(
        catalog,
        model_selection.map(|selection| selection.model.as_str()),
    )?;
    if let Some(fixed) = entry
        .runtime
        .fixed_context_window_tokens
        .filter(|tokens| *tokens != 0.0)
    {
        return Some(fixed);
    }
    let raw = model_selection
        .and_then(|selection| model_selection_string_option(selection, "contextWindow"));
    let caps =
        get_claude_catalog_model_capabilities(catalog, model_selection.map(|s| s.model.as_str()));
    let selections = selection_for("contextWindow", raw.as_deref());
    let descriptors = get_provider_option_descriptors(&caps, selections.as_deref());
    let context_window = current_string_option(&descriptors, "contextWindow")?;
    if context_window.is_empty() {
        return None;
    }
    entry
        .runtime
        .context_window_tokens
        .as_ref()?
        .iter()
        .find(|(key, _)| key.as_str() == context_window)
        .map(|(_, tokens)| *tokens)
}

#[cfg(test)]
mod tests;
