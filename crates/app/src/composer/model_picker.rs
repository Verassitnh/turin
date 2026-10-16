//! Port of the logic in upstream/apps/web/src/components/chat/ModelPickerContent.tsx
//! and `modelOrdering.ts`: which models the picker lists for a sidebar
//! selection or search, the legacy-models section, provider navigation and the
//! setup affordances. The view in `views/model_picker.rs` renders
//! [`ModelPickerList`].
//! Also ports upstream/apps/web/src/modelOrdering.ts.

use std::collections::HashSet;

use turin_contracts::model::ANTIGRAVITY_DEFAULT_MODEL;

use crate::composer::catalog::ProviderInstanceEntry;
use crate::composer::model_picker_keys::{model_picker_legacy_section_key, model_picker_model_key};
use crate::composer::model_picker_search::{
    ModelPickerSearchable, build_model_picker_search_text, score_model_picker_search,
};
use crate::composer::provider_state::{ProviderModel, resolve_selectable_model};

/// `providerModelKey`: `${instanceId}:${slug}`, the favorites key.
pub fn provider_model_key(instance_id: &str, slug: &str) -> String {
    format!("{instance_id}:{slug}")
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelPickerItem {
    pub model: ProviderModel,
    pub instance_id: String,
    pub driver_kind: String,
    pub instance_display_name: String,
    pub instance_accent_color: Option<String>,
    pub continuation_group_key: Option<String>,
}

impl ModelPickerItem {
    pub fn key(&self) -> String {
        model_picker_model_key(&self.instance_id, &self.model.slug)
    }
}

/// The sidebar selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerSelection {
    Favorites,
    Instance(String),
}

/// `resolveModelPickerSelectedModel`.
pub fn resolve_model_picker_selected_model<'a>(
    driver_kind: Option<&str>,
    model: &str,
    options: &'a [ProviderModel],
) -> Option<&'a ProviderModel> {
    if driver_kind == Some("antigravity") && model == ANTIGRAVITY_DEFAULT_MODEL {
        let available: Vec<&ProviderModel> = options
            .iter()
            .filter(|option| option.slug != ANTIGRAVITY_DEFAULT_MODEL && !option.is_unavailable)
            .collect();
        return available
            .iter()
            .find(|option| {
                option
                    .aliases
                    .iter()
                    .any(|alias| alias == ANTIGRAVITY_DEFAULT_MODEL)
            })
            .or_else(|| available.iter().find(|option| option.is_default))
            .copied();
    }
    options.iter().find(|option| option.slug == model)
}

/// `shouldIncludeModelPickerOption`.
pub fn should_include_model_picker_option(
    entry: &ProviderInstanceEntry,
    option: &ProviderModel,
    active_instance_id: &str,
    active_model: &str,
) -> bool {
    if entry.driver_kind == "antigravity" && option.slug == ANTIGRAVITY_DEFAULT_MODEL {
        return false;
    }
    if entry.picker_ready() {
        return true;
    }
    entry.enabled
        && (entry.driver_kind == "opencode" || entry.driver_kind == "antigravity")
        && entry.instance_id == active_instance_id
        && option.slug == active_model
        && option.is_unavailable
}

/// `shouldOfferModelPickerSetup`.
pub fn should_offer_model_picker_setup(
    entry: &ProviderInstanceEntry,
    options: &[ProviderModel],
) -> bool {
    use crate::composer::catalog::{AuthStatus, ProviderStatus};
    entry.enabled
        && entry.status != ProviderStatus::Disabled
        && entry.has_setup()
        && (!entry.picker_ready()
            || !entry.installed
            || entry.auth_status == AuthStatus::Unauthenticated
            || !options.iter().any(|option| !option.is_unavailable))
}

/// `adjacentModelPickerProvider`: wraps through favorites and ready instances.
pub fn adjacent_model_picker_provider(
    entries: &[ProviderInstanceEntry],
    selected: &PickerSelection,
    direction: i32,
    disabled_instance_ids: Option<&HashSet<String>>,
    selectable_unavailable_instance_ids: Option<&HashSet<String>>,
) -> PickerSelection {
    let mut providers = vec![PickerSelection::Favorites];
    providers.extend(
        entries
            .iter()
            .filter(|entry| {
                !disabled_instance_ids.is_some_and(|ids| ids.contains(&entry.instance_id))
                    && (entry.picker_ready()
                        || selectable_unavailable_instance_ids
                            .is_some_and(|ids| ids.contains(&entry.instance_id)))
            })
            .map(|entry| PickerSelection::Instance(entry.instance_id.clone())),
    );
    let len = providers.len() as i32;
    let next = match providers.iter().position(|provider| provider == selected) {
        None => {
            if direction == 1 {
                0
            } else {
                len - 1
            }
        }
        Some(index) => (index as i32 + direction + len) % len,
    };
    providers[next as usize].clone()
}

/// `sortProviderModelItems`: favorites first (when grouping), then instance
/// order, then the original order.
pub fn sort_provider_model_items(
    items: Vec<ModelPickerItem>,
    favorite_model_keys: &HashSet<String>,
    group_favorites: bool,
    instance_order: &[String],
) -> Vec<ModelPickerItem> {
    let mut indexed: Vec<(usize, ModelPickerItem)> = items.into_iter().enumerate().collect();
    indexed.sort_by_key(|(original, item)| {
        let favorite = group_favorites
            && favorite_model_keys
                .contains(&provider_model_key(&item.instance_id, &item.model.slug));
        let instance_rank = instance_order
            .iter()
            .position(|id| *id == item.instance_id)
            .unwrap_or(usize::MAX);
        (!favorite, instance_rank, *original)
    });
    indexed.into_iter().map(|(_, item)| item).collect()
}

/// A row of the model list.
#[derive(Debug, Clone, PartialEq)]
pub enum PickerRow {
    Model(Box<ModelPickerItem>),
    /// The collapsed/expanded "Legacy models" header.
    LegacySection {
        key: String,
        count: usize,
        expanded: bool,
    },
}

impl PickerRow {
    pub fn key(&self) -> String {
        match self {
            Self::Model(item) => item.key(),
            Self::LegacySection { key, .. } => key.clone(),
        }
    }
}

pub struct ModelPickerInput<'a> {
    pub instance_entries: &'a [ProviderInstanceEntry],
    pub active_instance_id: &'a str,
    pub model: &'a str,
    pub locked_provider: Option<&'a str>,
    pub locked_continuation_group_key: Option<&'a str>,
    /// Keys from [`provider_model_key`].
    pub favorites: &'a HashSet<String>,
    pub search_query: &'a str,
    pub selected: &'a PickerSelection,
    pub expanded_legacy_instances: &'a HashSet<String>,
    pub on_open_provider_setup: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelPickerList {
    pub rows: Vec<PickerRow>,
    pub sidebar_entries: Vec<ProviderInstanceEntry>,
    pub show_sidebar: bool,
    pub is_searching: bool,
    pub locked_disabled_instance_ids: Option<HashSet<String>>,
    pub selectable_unavailable_instance_ids: Option<HashSet<String>>,
    /// Instances whose setup prompt shows under the list.
    pub provider_setup_entries: Vec<ProviderInstanceEntry>,
    pub update_required_notices: Vec<(String, String)>,
    /// `modelKey` → jump index (the first nine selectable rows).
    pub jump_keys: Vec<String>,
    pub active_model_key: Option<String>,
}

/// The initial sidebar selection (`useState` initializer).
pub fn initial_picker_selection(input: &ModelPickerInput<'_>) -> PickerSelection {
    let active = input
        .instance_entries
        .iter()
        .find(|entry| entry.instance_id == input.active_instance_id);
    let options = active.map(|entry| entry.models.as_slice()).unwrap_or(&[]);
    let active_model = resolve_model_picker_selected_model(
        active.map(|e| e.driver_kind.as_str()),
        input.model,
        options,
    );
    let active_slug = active_model.map(|model| model.slug.as_str()).unwrap_or(
        if input.model == ANTIGRAVITY_DEFAULT_MODEL {
            ""
        } else {
            input.model
        },
    );
    let has_selectable_unavailable = active.is_some_and(|entry| {
        options.iter().any(|option| {
            should_include_model_picker_option(entry, option, input.active_instance_id, active_slug)
        }) && !entry.picker_ready()
    });
    let needs_setup = input.on_open_provider_setup
        && active.is_some_and(|entry| should_offer_model_picker_setup(entry, options));
    if input.locked_provider.is_some()
        || has_selectable_unavailable
        || needs_setup
        || input.favorites.is_empty()
    {
        PickerSelection::Instance(input.active_instance_id.to_owned())
    } else {
        PickerSelection::Favorites
    }
}

/// The legacy section starts expanded when the active model is a legacy one.
pub fn initial_expanded_legacy_instances(input: &ModelPickerInput<'_>) -> HashSet<String> {
    let active = input
        .instance_entries
        .iter()
        .find(|entry| entry.instance_id == input.active_instance_id);
    let active_legacy = active.is_some_and(|entry| {
        entry
            .models
            .iter()
            .any(|model| model.slug == input.model && model.is_legacy)
    });
    if active_legacy {
        HashSet::from([input.active_instance_id.to_owned()])
    } else {
        HashSet::new()
    }
}

/// Resolve the picker's rows and chrome for the current state.
pub fn compute_model_picker_list(input: &ModelPickerInput<'_>) -> ModelPickerList {
    let entries = input.instance_entries;
    let active_entry = entries
        .iter()
        .find(|entry| entry.instance_id == input.active_instance_id);
    let active_options = active_entry
        .map(|entry| entry.models.as_slice())
        .unwrap_or(&[]);
    let active_model = resolve_model_picker_selected_model(
        active_entry.map(|e| e.driver_kind.as_str()),
        input.model,
        active_options,
    );
    let active_model_slug = active_model
        .map(|model| model.slug.clone())
        .unwrap_or_else(|| {
            if input.model == ANTIGRAVITY_DEFAULT_MODEL {
                String::new()
            } else {
                input.model.to_owned()
            }
        });
    let active_model_key = (!active_model_slug.is_empty())
        .then(|| model_picker_model_key(input.active_instance_id, &active_model_slug));

    let matches_locked = |entry_driver: &str, entry_group: &Option<String>| -> bool {
        let Some(locked) = input.locked_provider else {
            return true;
        };
        if entry_driver != locked {
            return false;
        }
        match input.locked_continuation_group_key {
            Some(group) if !group.is_empty() => entry_group.as_deref() == Some(group),
            _ => true,
        }
    };

    let active_has_selectable_unavailable = active_entry.is_some_and(|entry| {
        active_options.iter().any(|option| {
            should_include_model_picker_option(
                entry,
                option,
                input.active_instance_id,
                &active_model_slug,
            )
        }) && !entry.picker_ready()
    });
    let mut selectable_unavailable: HashSet<String> = HashSet::new();
    if active_has_selectable_unavailable {
        selectable_unavailable.insert(input.active_instance_id.to_owned());
    }
    if input.on_open_provider_setup {
        for entry in entries {
            if should_offer_model_picker_setup(entry, &entry.models) {
                selectable_unavailable.insert(entry.instance_id.clone());
            }
        }
    }

    // Flatten models into a searchable array.
    let mut flat: Vec<ModelPickerItem> = Vec::new();
    for entry in entries {
        for model in &entry.models {
            if !should_include_model_picker_option(
                entry,
                model,
                input.active_instance_id,
                &active_model_slug,
            ) {
                continue;
            }
            flat.push(ModelPickerItem {
                model: model.clone(),
                instance_id: entry.instance_id.clone(),
                driver_kind: entry.driver_kind.clone(),
                instance_display_name: entry.display_name.clone(),
                instance_accent_color: entry.accent_color.clone(),
                continuation_group_key: entry.continuation_group_key.clone(),
            });
        }
    }

    let is_locked = input.locked_provider.is_some();
    let query = turin_contracts::base_schemas::js_trim(input.search_query);
    let is_searching = !query.is_empty();
    let locked_disabled: Option<HashSet<String>> = is_locked.then(|| {
        entries
            .iter()
            .filter(|entry| !matches_locked(&entry.driver_kind, &entry.continuation_group_key))
            .map(|entry| entry.instance_id.clone())
            .collect()
    });
    let mut sidebar_entries: Vec<ProviderInstanceEntry> = entries
        .iter()
        .filter(|e| e.picker_visible())
        .cloned()
        .collect();
    if is_locked {
        let (available, disabled): (Vec<_>, Vec<_>) = sidebar_entries
            .into_iter()
            .partition(|entry| matches_locked(&entry.driver_kind, &entry.continuation_group_key));
        sidebar_entries = available.into_iter().chain(disabled).collect();
    }
    let show_sidebar = !is_searching && !sidebar_entries.is_empty();
    let instance_order: Vec<String> = entries
        .iter()
        .map(|entry| entry.instance_id.clone())
        .collect();
    let is_favorite = |item: &ModelPickerItem| {
        input
            .favorites
            .contains(&provider_model_key(&item.instance_id, &item.model.slug))
    };

    fn searchable(item: &ModelPickerItem, favorite: bool) -> ModelPickerSearchable<'_> {
        ModelPickerSearchable {
            driver_kind: &item.driver_kind,
            provider_display_name: &item.instance_display_name,
            name: &item.model.name,
            short_name: item.model.short_name.as_deref(),
            sub_provider: item.model.sub_provider.as_deref(),
            is_favorite: favorite,
        }
    }

    let filtered: Vec<ModelPickerItem> = if is_searching {
        let mut ranked: Vec<(i64, bool, String, ModelPickerItem)> = flat
            .into_iter()
            .filter_map(|item| {
                let favorite = is_favorite(&item);
                let score = score_model_picker_search(&searchable(&item, favorite), query)?;
                let tie_breaker = build_model_picker_search_text(&searchable(&item, false));
                Some((score, favorite, tie_breaker, item))
            })
            .collect();
        // When searching, only the locked provider constrains the results.
        ranked.retain(|(_, _, _, item)| {
            matches_locked(&item.driver_kind, &item.continuation_group_key)
        });
        ranked.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| crate::composer::js_string::locale_compare(&a.2, &b.2))
        });
        ranked.into_iter().map(|(_, _, _, item)| item).collect()
    } else {
        let mut result = flat;
        if is_locked {
            result.retain(|item| matches_locked(&item.driver_kind, &item.continuation_group_key));
        }
        match input.selected {
            PickerSelection::Favorites => result.retain(|item| is_favorite(item)),
            PickerSelection::Instance(id) => result.retain(|item| &item.instance_id == id),
        }
        sort_provider_model_items(
            result,
            input.favorites,
            *input.selected != PickerSelection::Favorites,
            if *input.selected == PickerSelection::Favorites {
                instance_order.as_slice()
            } else {
                &[]
            },
        )
    };

    // Legacy section.
    let mut rows: Vec<PickerRow> = Vec::new();
    let legacy_instance = match input.selected {
        PickerSelection::Instance(id) if !is_searching => Some(id.clone()),
        _ => None,
    };
    let legacy_count = filtered.iter().filter(|item| item.model.is_legacy).count();
    if let Some(instance) = legacy_instance.filter(|_| legacy_count > 0) {
        let expanded = input.expanded_legacy_instances.contains(&instance);
        let (legacy, current): (Vec<_>, Vec<_>) =
            filtered.into_iter().partition(|item| item.model.is_legacy);
        rows.extend(
            current
                .into_iter()
                .map(|item| PickerRow::Model(Box::new(item))),
        );
        rows.push(PickerRow::LegacySection {
            key: model_picker_legacy_section_key(&instance),
            count: legacy.len(),
            expanded,
        });
        if expanded {
            rows.extend(
                legacy
                    .into_iter()
                    .map(|item| PickerRow::Model(Box::new(item))),
            );
        }
    } else {
        rows.extend(
            filtered
                .into_iter()
                .map(|item| PickerRow::Model(Box::new(item))),
        );
    }

    let selected_entry = match input.selected {
        PickerSelection::Favorites => None,
        PickerSelection::Instance(id) => entries.iter().find(|entry| &entry.instance_id == id),
    };
    let provider_setup_entries: Vec<ProviderInstanceEntry> =
        if !is_searching && input.on_open_provider_setup {
            entries
                .iter()
                .filter(|entry| {
                    matches_locked(&entry.driver_kind, &entry.continuation_group_key)
                        && should_offer_model_picker_setup(entry, &entry.models)
                        && match selected_entry {
                            Some(selected) => entry.instance_id == selected.instance_id,
                            None => rows.is_empty(),
                        }
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
    let notice_entries: Vec<&ProviderInstanceEntry> = if is_searching {
        entries
            .iter()
            .filter(|entry| matches_locked(&entry.driver_kind, &entry.continuation_group_key))
            .collect()
    } else {
        selected_entry.into_iter().collect()
    };
    let update_required_notices = notice_entries
        .into_iter()
        .filter_map(|entry| {
            entry
                .update_required_notice
                .clone()
                .map(|notice| (entry.instance_id.clone(), notice))
        })
        .collect();

    ModelPickerList {
        jump_keys: rows
            .iter()
            .filter_map(|row| match row {
                PickerRow::Model(item) => Some(item.key()),
                PickerRow::LegacySection { .. } => None,
            })
            .take(9)
            .collect(),
        rows,
        sidebar_entries,
        show_sidebar,
        is_searching,
        locked_disabled_instance_ids: locked_disabled,
        selectable_unavailable_instance_ids: (!selectable_unavailable.is_empty())
            .then_some(selectable_unavailable),
        provider_setup_entries,
        update_required_notices,
        active_model_key,
    }
}

/// `handleModelSelect`'s resolution: the provider's canonical slug for a pick.
pub fn resolve_model_pick(
    entries: &[ProviderInstanceEntry],
    instance_id: &str,
    model_slug: &str,
) -> Option<String> {
    let entry = entries
        .iter()
        .find(|entry| entry.instance_id == instance_id)?;
    resolve_selectable_model(&entry.driver_kind, Some(model_slug), &entry.models)
}

#[cfg(test)]
mod tests;
