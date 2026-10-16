//! Port of upstream/apps/web/src/components/ChatView.logic.ts (composer provider and interaction mode).

use turin_contracts::model::ANTIGRAVITY_DEFAULT_MODEL;
use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};
use turin_contracts::provider_policy::ProviderInteractionMode;
use turin_provider_core::server_provider::{
    ServerProvider, ServerProviderAuth, ServerProviderAuthStatus, ServerProviderState,
};

use super::provider_instances::{
    NO_PROVIDER_INSTANCE_ID, ProviderInstanceEntry, resolve_selectable_provider_instance_entry,
};

#[derive(Debug, Clone, PartialEq)]
pub struct ComposerProviderSelection {
    pub selected_provider_entry: Option<ProviderInstanceEntry>,
    pub requested_driver_kind: ProviderDriverKind,
    pub locked_continuation_group_key: Option<String>,
    pub unavailable_provider_instance_id: Option<ProviderInstanceId>,
}

pub fn resolve_composer_provider_selection(
    entries: &[ProviderInstanceEntry],
    candidate_instance_ids: &[Option<ProviderInstanceId>],
    locked_provider: Option<&ProviderDriverKind>,
    locked_instance_id: Option<&ProviderInstanceId>,
) -> ComposerProviderSelection {
    let no_provider = ProviderInstanceId::new(NO_PROVIDER_INSTANCE_ID).ok();
    let requested_instance_id = candidate_instance_ids.iter().find_map(|candidate| {
        candidate
            .as_ref()
            .filter(|candidate| no_provider.as_ref() != Some(*candidate))
    });
    let requested_driver_kind = locked_provider
        .cloned()
        .or_else(|| {
            requested_instance_id
                .and_then(|instance_id| {
                    entries
                        .iter()
                        .find(|entry| entry.instance_id == *instance_id)
                        .map(|entry| entry.driver_kind.clone())
                })
                .or_else(|| entries.first().map(|entry| entry.driver_kind.clone()))
        })
        .unwrap_or_else(|| {
            ProviderDriverKind::new("unconfigured")
                .unwrap_or_else(|_| ProviderDriverKind::new("codex").expect("codex driver"))
        });
    let locked_continuation_group_key = locked_provider.and_then(|_| {
        locked_instance_id.and_then(|instance_id| {
            entries
                .iter()
                .find(|entry| entry.instance_id == *instance_id)
                .and_then(|entry| entry.continuation_group_key.clone())
        })
    });
    let requires_exact_instance = locked_provider
        .is_some_and(|driver| driver.as_str() == "antigravity")
        && locked_instance_id.is_some()
        && locked_continuation_group_key.is_none();
    let compatible_entries: Vec<_> = entries
        .iter()
        .filter(|entry| {
            locked_provider.is_none_or(|driver| entry.driver_kind == *driver)
                && locked_continuation_group_key
                    .as_ref()
                    .is_none_or(|key| entry.continuation_group_key.as_deref() == Some(key.as_str()))
                && (!requires_exact_instance
                    || locked_instance_id.is_some_and(|id| entry.instance_id == *id))
        })
        .collect();
    let compatible_owned: Vec<ProviderInstanceEntry> =
        compatible_entries.into_iter().cloned().collect();
    let driver_filtered: Vec<ProviderInstanceEntry> = compatible_owned
        .iter()
        .filter(|entry| entry.driver_kind == requested_driver_kind)
        .cloned()
        .collect();
    let selected_provider_entry = candidate_instance_ids
        .iter()
        .filter_map(|candidate| candidate.as_ref())
        .filter_map(|candidate| {
            compatible_owned.iter().find(|entry| {
                entry.instance_id == *candidate && entry.enabled && entry.is_available
            })
        })
        .next()
        .cloned()
        .or_else(|| resolve_selectable_provider_instance_entry(&driver_filtered, None).cloned())
        .or_else(|| resolve_selectable_provider_instance_entry(&compatible_owned, None).cloned());
    let unavailable_provider_instance_id = if selected_provider_entry.is_some() {
        None
    } else if locked_provider.is_some() {
        locked_instance_id
            .cloned()
            .or_else(|| requested_instance_id.cloned())
    } else {
        requested_instance_id.cloned()
    };
    ComposerProviderSelection {
        selected_provider_entry,
        requested_driver_kind,
        locked_continuation_group_key,
        unavailable_provider_instance_id,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComposerInteractionMode {
    pub enabled: bool,
    pub interaction_mode: ProviderInteractionMode,
}

pub fn resolve_composer_interaction_mode(
    plan_mode_enabled: bool,
    show_interaction_mode_toggle: Option<bool>,
    interaction_mode: ProviderInteractionMode,
) -> ComposerInteractionMode {
    let enabled = plan_mode_enabled && show_interaction_mode_toggle.is_none_or(|toggle| toggle);
    ComposerInteractionMode {
        enabled,
        interaction_mode: if enabled {
            interaction_mode
        } else {
            ProviderInteractionMode::Default
        },
    }
}

pub fn get_antigravity_send_block_reason(
    provider: Option<&ServerProvider>,
    model: &str,
) -> Option<String> {
    let provider = provider?;
    if provider.driver.as_str() != "antigravity" {
        return None;
    }
    if !provider.installed {
        return Some("Install Antigravity in provider settings before sending.".to_owned());
    }
    if provider.auth.status == ServerProviderAuthStatus::Unauthenticated {
        return Some("Sign in to Antigravity in provider settings before sending.".to_owned());
    }
    let slug = model.trim();
    if slug.is_empty() {
        return Some("Choose an Antigravity model before sending.".to_owned());
    }
    if provider.auth.status == ServerProviderAuthStatus::Unknown {
        return None;
    }
    if provider.models.is_empty() {
        return Some("Refresh Antigravity models in provider settings before sending.".to_owned());
    }
    if provider.status == ServerProviderState::Ready
        && slug != ANTIGRAVITY_DEFAULT_MODEL
        && !provider.models.iter().any(|entry| {
            entry.slug.as_str() == slug
                || entry
                    .aliases
                    .as_ref()
                    .is_some_and(|aliases| aliases.iter().any(|alias| alias.as_str() == slug))
        })
    {
        return Some(
            "That Antigravity model is no longer available. Choose another model.".to_owned(),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::TrimmedNonEmptyString;
    use turin_provider_core::server_provider::ServerProviderModel;

    fn tne(value: &str) -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(value).unwrap()
    }

    fn entry(
        driver: &str,
        instance_id: &str,
        overrides: ProviderEntryOverrides,
    ) -> ProviderInstanceEntry {
        let driver_kind = ProviderDriverKind::new(driver).unwrap();
        let instance_id = ProviderInstanceId::new(instance_id).unwrap();
        let mut snapshot: ServerProvider = serde_json::from_value(serde_json::json!({
            "instanceId": instance_id.as_str(),
            "driver": driver,
            "enabled": overrides.enabled,
            "installed": overrides.installed,
            "status": match overrides.status {
                ServerProviderState::Ready => "ready",
                ServerProviderState::Warning => "warning",
                ServerProviderState::Error => "error",
                ServerProviderState::Disabled => "disabled",
            },
            "auth": { "status": match overrides.auth.status {
                ServerProviderAuthStatus::Authenticated => "authenticated",
                ServerProviderAuthStatus::Unauthenticated => "unauthenticated",
                ServerProviderAuthStatus::Unknown => "unknown",
            }},
            "models": overrides.models,
            "showInteractionModeToggle": overrides.show_interaction_mode_toggle,
            "continuation": overrides.continuation_group_key.map(|group_key| serde_json::json!({ "groupKey": group_key })),
            "checkedAt": "2026-03-29T00:00:00.000Z",
            "version": null,
            "slashCommands": [],
            "skills": [],
        }))
        .expect("provider snapshot json");
        snapshot.models = overrides.models;
        ProviderInstanceEntry {
            instance_id: instance_id.clone(),
            driver_kind,
            continuation_group_key: overrides.continuation_group_key.map(str::to_owned),
            enabled: overrides.enabled,
            is_available: true,
            status: overrides.status,
            snapshot,
        }
    }

    struct ProviderEntryOverrides {
        enabled: bool,
        installed: bool,
        status: ServerProviderState,
        auth: ServerProviderAuth,
        models: Vec<ServerProviderModel>,
        show_interaction_mode_toggle: Option<bool>,
        continuation_group_key: Option<&'static str>,
    }

    impl Default for ProviderEntryOverrides {
        fn default() -> Self {
            Self {
                enabled: true,
                installed: true,
                status: ServerProviderState::Ready,
                auth: ServerProviderAuth::with_status(ServerProviderAuthStatus::Authenticated),
                models: Vec::new(),
                show_interaction_mode_toggle: None,
                continuation_group_key: None,
            }
        }
    }

    #[test]
    fn interaction_mode_resets_when_plan_is_unsupported() {
        assert_eq!(
            resolve_composer_interaction_mode(true, Some(false), ProviderInteractionMode::Plan),
            ComposerInteractionMode {
                enabled: false,
                interaction_mode: ProviderInteractionMode::Default,
            }
        );
        assert_eq!(
            resolve_composer_interaction_mode(true, None, ProviderInteractionMode::Plan),
            ComposerInteractionMode {
                enabled: true,
                interaction_mode: ProviderInteractionMode::Plan,
            }
        );
    }

    #[test]
    fn antigravity_blocks_unauthenticated_and_missing_model_catalog() {
        let signed_out = entry(
            "antigravity",
            "google_work",
            ProviderEntryOverrides {
                status: ServerProviderState::Error,
                auth: ServerProviderAuth::with_status(ServerProviderAuthStatus::Unauthenticated),
                models: vec![ServerProviderModel {
                    slug: tne("gemini-pro"),
                    name: tne("Gemini Pro"),
                    short_name: None,
                    sub_provider: None,
                    aliases: None,
                    badge: None,
                    is_custom: false,
                    is_default: None,
                    is_legacy: None,
                    capabilities: None,
                }],
                ..Default::default()
            },
        );
        assert_eq!(
            get_antigravity_send_block_reason(Some(&signed_out.snapshot), "gemini-pro").as_deref(),
            Some("Sign in to Antigravity in provider settings before sending.")
        );
        assert_eq!(
            get_antigravity_send_block_reason(
                Some(&entry("antigravity", "antigravity", Default::default()).snapshot),
                "gemini-pro"
            )
            .as_deref(),
            Some("Refresh Antigravity models in provider settings before sending.")
        );
    }
}
