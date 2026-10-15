//! Port of the logic-level cases in upstream/packages/shared/src/serverSettings.test.ts.
//! Patches are written as JSON to match the upstream object literals.

use serde_json::{Value, json};
use turin_contracts::base_schemas::ProjectId;
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::provider_instance::{
    ProviderDriverKind, ProviderInstanceConfig, ProviderInstanceId,
};
use turin_contracts::settings::{ServerSettings, default_server_settings};

use super::*;

fn apply(current: &ServerSettings, patch: Value) -> ServerSettings {
    let patch: ServerSettingsPatch = serde_json::from_value(patch).expect("valid patch");
    apply_server_settings_patch(current, &patch).expect("patch applies")
}

fn defaults() -> ServerSettings {
    default_server_settings()
}

fn background(settings: &ServerSettings) -> Value {
    serde_json::to_value(&settings.background_activity).unwrap()
}

fn selection(instance: &str, model: &str, options: Option<Value>) -> Value {
    let mut value = json!({ "instanceId": instance, "model": model });
    if let Some(options) = options {
        value["options"] = options;
    }
    value
}

struct Provider {
    instance_id: ProviderInstanceId,
    enabled: bool,
    available: bool,
    text_capable: bool,
}

impl ProviderStatus for Provider {
    fn instance_id(&self) -> &ProviderInstanceId {
        &self.instance_id
    }
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn is_available(&self) -> bool {
        self.available
    }
    fn is_text_generation_capable(&self) -> bool {
        self.text_capable
    }
}

// fallbackTextGenerationModelSelection

#[test]
fn fallback_ignores_a_configured_non_default_instance_at_machine_scope() {
    let configured = ProviderInstanceId::new("codex_work").unwrap();
    let mut instances = indexmap::IndexMap::new();
    instances.insert(
        configured.clone(),
        ProviderInstanceConfig {
            driver: ProviderDriverKind::new("codex").unwrap(),
            display_name: None,
            accent_color: None,
            environment: None,
            enabled: None,
            config: Some(json!({})),
        },
    );
    let fallback = fallback_text_generation_model_selection(
        &instances,
        |id, _| *id == configured,
        false,
        None,
    );
    assert!(fallback.is_none());
}

#[test]
fn fallback_considers_a_configured_non_default_instance_only_when_asked() {
    let configured = ProviderInstanceId::new("codex_work").unwrap();
    let mut instances = indexmap::IndexMap::new();
    instances.insert(
        configured.clone(),
        ProviderInstanceConfig {
            driver: ProviderDriverKind::new("codex").unwrap(),
            display_name: None,
            accent_color: None,
            environment: None,
            enabled: None,
            config: Some(json!({})),
        },
    );
    let fallback =
        fallback_text_generation_model_selection(&instances, |id, _| *id == configured, true, None)
            .expect("configured instance is a candidate");
    assert_eq!(fallback.instance_id, configured);
}

// serverSettings helpers: legacy project maps

#[test]
fn auto_pull_inherits_while_keeping_legacy_opt_ins_and_overrides() {
    let project = ProjectId::new("project-pull").unwrap();
    let other = ProjectId::new("other-project").unwrap();
    let base = defaults();
    assert!(!resolve_project_auto_pull(&base, &project, false));
    assert!(resolve_project_auto_pull(&base, &project, true));

    let enabled = apply(&base, json!({ "defaultAutoPull": true }));
    assert!(resolve_project_auto_pull(&enabled, &project, false));

    let overridden = apply(
        &enabled,
        json!({ "projectAutoPullOverrides": { project.as_str(): false } }),
    );
    assert!(!resolve_project_auto_pull(&overridden, &project, true));

    let reset = apply(
        &overridden,
        json!({ "projectAutoPullOverrides": { project.as_str(): null } }),
    );
    assert!(resolve_project_auto_pull(&reset, &project, false));

    let disabled = apply(
        &reset,
        json!({
            "defaultAutoPull": false,
            "projectAutoPullOverrides": { project.as_str(): true },
        }),
    );
    assert!(resolve_project_auto_pull(&disabled, &project, false));
    assert!(!resolve_project_auto_pull(&disabled, &other, false));
}

#[test]
fn browser_access_inherits_and_restores_inheritance_when_an_override_is_removed() {
    let project = ProjectId::new("project-browser").unwrap();
    let other = ProjectId::new("other-project").unwrap();
    let overridden = apply(
        &defaults(),
        json!({ "projectAgentBrowserAccessOverrides": { project.as_str(): false } }),
    );
    assert!(!resolve_project_agent_browser_access(&overridden, &project));
    assert!(resolve_project_agent_browser_access(&overridden, &other));

    let reset = apply(
        &overridden,
        json!({ "projectAgentBrowserAccessOverrides": { project.as_str(): null } }),
    );
    assert!(resolve_project_agent_browser_access(&reset, &project));
}

#[test]
fn boolean_overrides_of_other_projects_survive_separate_updates_and_resets() {
    let first = ProjectId::new("first-project").unwrap();
    let second = ProjectId::new("second-project").unwrap();
    let first_update = apply(
        &defaults(),
        json!({
            "defaultAutoPull": true,
            "projectAutoPullOverrides": { first.as_str(): false },
            "projectAgentBrowserAccessOverrides": { first.as_str(): false },
        }),
    );
    let second_update = apply(
        &first_update,
        json!({
            "projectAutoPullOverrides": { second.as_str(): false },
            "projectAgentBrowserAccessOverrides": { second.as_str(): false },
        }),
    );
    for project in [&first, &second] {
        assert!(!resolve_project_auto_pull(&second_update, project, false));
        assert!(!resolve_project_agent_browser_access(
            &second_update,
            project
        ));
    }

    let reset = apply(
        &second_update,
        json!({
            "projectAutoPullOverrides": { first.as_str(): null },
            "projectAgentBrowserAccessOverrides": { first.as_str(): null },
        }),
    );
    assert!(resolve_project_auto_pull(&reset, &first, false));
    assert!(resolve_project_agent_browser_access(&reset, &first));
    assert!(!resolve_project_auto_pull(&reset, &second, false));
    assert!(!resolve_project_agent_browser_access(&reset, &second));
    assert!(!reset.project_auto_pull_overrides.contains_key(&first));
}

#[test]
fn conversation_model_default_is_replaced_and_cleared_without_stale_options() {
    let current = apply(
        &defaults(),
        json!({
            "defaultModelSelection": selection(
                "codex",
                "gpt-5.4",
                Some(json!([{ "id": "reasoningEffort", "value": "high" }])),
            ),
        }),
    );
    let next = apply(
        &current,
        json!({ "defaultModelSelection": selection("claudeAgent", "sonnet", None) }),
    );
    assert_eq!(
        serde_json::to_value(&next.default_model_selection).unwrap(),
        selection("claudeAgent", "sonnet", None)
    );
    let cleared = apply(&next, json!({ "defaultModelSelection": null }));
    assert!(cleared.default_model_selection.is_none());
}

// textGenerationModelSelection

#[test]
fn text_generation_selection_is_replaced_when_provider_and_model_are_given() {
    let current = apply(
        &defaults(),
        json!({
            "textGenerationModelSelection": selection(
                "codex",
                "gpt-5.4-mini",
                Some(json!([
                    { "id": "reasoningEffort", "value": "high" },
                    { "id": "fastMode", "value": true },
                ])),
            ),
        }),
    );
    let next = apply(
        &current,
        json!({
            "textGenerationModelSelection": { "instanceId": "codex", "model": "gpt-5.4-mini" },
        }),
    );
    assert_eq!(
        serde_json::to_value(&next.text_generation_model_selection).unwrap(),
        selection("codex", "gpt-5.4-mini", None)
    );
}

#[test]
fn text_generation_selection_still_deep_merges_when_only_options_are_given() {
    let current = apply(
        &defaults(),
        json!({
            "textGenerationModelSelection": selection(
                "codex",
                "gpt-5.4-mini",
                Some(json!([
                    { "id": "reasoningEffort", "value": "high" },
                    { "id": "fastMode", "value": true },
                ])),
            ),
        }),
    );
    let next = apply(
        &current,
        json!({
            "textGenerationModelSelection": { "options": [{ "id": "fastMode", "value": false }] },
        }),
    );
    assert_eq!(
        serde_json::to_value(&next.text_generation_model_selection).unwrap(),
        selection(
            "codex",
            "gpt-5.4-mini",
            Some(json!([
                { "id": "reasoningEffort", "value": "high" },
                { "id": "fastMode", "value": false },
            ])),
        )
    );
}

#[test]
fn text_generation_selection_across_providers_drops_stale_options() {
    let current = apply(
        &defaults(),
        json!({
            "textGenerationModelSelection": selection(
                "codex",
                "gpt-5.4-mini",
                Some(json!([{ "id": "fastMode", "value": true }])),
            ),
        }),
    );
    let next = apply(
        &current,
        json!({
            "textGenerationModelSelection": { "instanceId": "opencode", "model": "openai/gpt-5" },
        }),
    );
    assert_eq!(
        serde_json::to_value(&next.text_generation_model_selection).unwrap(),
        selection("opencode", "openai/gpt-5", None)
    );
}

#[test]
fn array_based_text_generation_selection_patches_are_accepted() {
    let next = apply(
        &defaults(),
        json!({
            "textGenerationModelSelection": {
                "instanceId": "opencode",
                "model": "openai/gpt-5",
                "options": [
                    { "id": "variant", "value": "prod" },
                    { "id": "agent", "value": "build" },
                ],
            },
        }),
    );
    assert_eq!(
        serde_json::to_value(&next.text_generation_model_selection).unwrap(),
        selection(
            "opencode",
            "openai/gpt-5",
            Some(json!([
                { "id": "variant", "value": "prod" },
                { "id": "agent", "value": "build" },
            ])),
        )
    );
}

// sourceControlWriterModelSelection

#[test]
fn writer_selection_is_replaced_without_stale_options_and_cleared_with_null() {
    let current = apply(
        &defaults(),
        json!({
            "sourceControlWriterModelSelection": selection(
                "codex",
                "gpt-5.4-mini",
                Some(json!([{ "id": "reasoningEffort", "value": "high" }])),
            ),
        }),
    );
    let replaced = apply(
        &current,
        json!({
            "sourceControlWriterModelSelection": { "instanceId": "opencode", "model": "openai/gpt-5" },
        }),
    );
    assert_eq!(
        serde_json::to_value(&replaced.source_control_writer_model_selection).unwrap(),
        selection("opencode", "openai/gpt-5", None)
    );
    let cleared = apply(
        &replaced,
        json!({ "sourceControlWriterModelSelection": null }),
    );
    assert!(cleared.source_control_writer_model_selection.is_none());
}

#[test]
fn writer_falls_back_from_a_disabled_provider_without_clearing_its_selection() {
    let instance = ProviderInstanceId::new("codex_writer").unwrap();
    let writer = ModelSelection {
        instance_id: instance.clone(),
        model: turin_contracts::base_schemas::TrimmedNonEmptyString::new("gpt-5.4-mini").unwrap(),
        options: None,
    };
    let mut settings = defaults();
    settings.provider_instances.insert(
        instance.clone(),
        ProviderInstanceConfig {
            driver: ProviderDriverKind::new("codex").unwrap(),
            display_name: None,
            accent_color: None,
            environment: None,
            enabled: Some(false),
            config: Some(json!({})),
        },
    );
    settings.source_control_writer_model_selection = Some(writer.clone());

    assert!(!is_model_selection_provider_enabled(&settings, &writer));
    assert_eq!(
        resolve_source_control_writer_model_selection(&settings, None::<&[Provider]>, None),
        settings.text_generation_model_selection
    );
    assert_eq!(settings.source_control_writer_model_selection, Some(writer));
}

#[test]
fn writer_falls_back_from_an_unavailable_or_text_incapable_provider() {
    let settings = apply(
        &defaults(),
        json!({
            "providerInstances": { "missing_writer": { "driver": "codex", "enabled": true, "config": {} } },
            "sourceControlWriterModelSelection": selection("missing_writer", "missing-model", None),
        }),
    );
    let unavailable = [Provider {
        instance_id: ProviderInstanceId::new("missing_writer").unwrap(),
        enabled: true,
        available: false,
        text_capable: true,
    }];
    assert_eq!(
        resolve_source_control_writer_model_selection(&settings, Some(&unavailable), None),
        settings.text_generation_model_selection
    );
    let incapable = [Provider {
        instance_id: ProviderInstanceId::new("missing_writer").unwrap(),
        enabled: true,
        available: true,
        text_capable: false,
    }];
    assert_eq!(
        resolve_source_control_writer_model_selection(&settings, Some(&incapable), None),
        settings.text_generation_model_selection
    );
    let ready = [Provider {
        instance_id: ProviderInstanceId::new("missing_writer").unwrap(),
        enabled: true,
        available: true,
        text_capable: true,
    }];
    assert_eq!(
        resolve_source_control_writer_model_selection(&settings, Some(&ready), None)
            .instance_id
            .as_str(),
        "missing_writer"
    );
}

// providerInstances and usage sources

#[test]
fn provider_instances_are_replaced_so_omitted_fields_are_cleared() {
    let with_fields = apply(
        &defaults(),
        json!({
            "providerInstances": {
                "codex_work": { "driver": "codex", "displayName": "Work", "enabled": true, "config": {} },
            },
        }),
    );
    let replaced = apply(
        &with_fields,
        json!({
            "providerInstances": {
                "codex_work": { "driver": "codex", "config": {} },
            },
        }),
    );
    let entry = replaced
        .provider_instances
        .get(&ProviderInstanceId::new("codex_work").unwrap())
        .unwrap();
    assert!(entry.display_name.is_none());
    assert!(entry.enabled.is_none());
}

#[test]
fn project_provider_instances_survive_a_patch_entry_that_omits_the_key() {
    let project = ProjectId::new("project-1").unwrap();
    let stored = apply(
        &defaults(),
        json!({
            "projectSettingsOverrides": {
                project.as_str(): {
                    "providerInstances": { "codex_work": { "enabled": false } },
                },
            },
        }),
    );
    let next = apply(
        &stored,
        json!({
            "projectSettingsOverrides": { project.as_str(): { "defaultAutoPull": true } },
        }),
    );
    let entry = &next.project_settings_overrides[&project];
    assert_eq!(entry.default_auto_pull, Some(true));
    assert!(entry.provider_instances.is_some());
}

#[test]
fn usage_limit_sources_upsert_and_remove_per_entry() {
    let first = apply(
        &defaults(),
        json!({ "usageLimitSources": { "alpha": { "kind": "cliproxy", "url": "https://a.example", "managementKey": "" }, "beta": { "kind": "cliproxy", "url": "https://a.example", "managementKey": "" } } }),
    );
    let next = apply(
        &first,
        json!({ "usageLimitSources": { "alpha": null, "gamma": { "kind": "cliproxy", "url": "https://a.example", "managementKey": "" } } }),
    );
    let value = serde_json::to_value(&next.usage_limit_sources).unwrap();
    assert!(value.get("alpha").is_none());
    assert!(value.get("beta").is_some());
    assert!(value.get("gamma").is_some());
}

// worktreesDirectory

#[test]
fn a_previous_worktrees_directory_is_remembered() {
    let next = apply(&defaults(), json!({ "worktreesDirectory": "/custom/a" }));
    let moved = apply(&next, json!({ "worktreesDirectory": "/custom/b" }));
    let previous = serde_json::to_value(&moved.previous_worktrees_directories).unwrap();
    assert!(
        previous
            .as_array()
            .unwrap()
            .contains(&json!(next.worktrees_directory))
    );
}

// background activity

#[test]
fn profiles_are_stored_as_a_versioned_object_and_legacy_aliases_follow() {
    let next = apply(
        &defaults(),
        json!({
            "backgroundActivity": { "schemaVersion": 1, "profile": "battery-saver", "overrides": {} },
        }),
    );
    assert_eq!(
        background(&next),
        json!({ "schemaVersion": 1, "profile": "battery-saver", "overrides": {} })
    );
    assert_eq!(
        next.background_activity_profile,
        BackgroundActivityProfile::BatterySaver
    );
    assert_eq!(next.automatic_git_fetch_interval, 0);
    assert_eq!(next.provider_health_refresh_interval, minutes(15));
}

#[test]
fn legacy_interval_patches_become_custom_background_overrides() {
    let next = apply(&defaults(), json!({ "automaticGitFetchInterval": 15_000 }));
    assert_eq!(
        background(&next),
        json!({
            "schemaVersion": 1,
            "profile": "custom",
            "baseProfile": "balanced",
            "overrides": { "automaticGitFetchInterval": 15_000 },
        })
    );
    let resolved = resolve_server_background_activity_settings(&next);
    assert_eq!(resolved.profile, BackgroundActivityProfile::Balanced);
    assert_eq!(resolved.automatic_git_fetch_interval, 15_000);
}

#[test]
fn unrelated_patches_preserve_legacy_background_settings() {
    let mut current = defaults();
    current.background_activity_profile = BackgroundActivityProfile::Performance;
    current.automatic_git_fetch_interval = 7_000;
    current.provider_health_refresh_interval = 240_000;
    let next = apply(
        &current,
        json!({
            "sourceControlWriterModelSelection": selection("codex", "gpt-5.4-mini", None),
        }),
    );
    assert_eq!(
        background(&next),
        json!({
            "schemaVersion": 1,
            "profile": "custom",
            "baseProfile": "performance",
            "overrides": {
                "automaticGitFetchInterval": 7_000,
                "providerHealthRefreshInterval": 240_000,
            },
        })
    );
    assert_eq!(
        next.background_activity_profile,
        BackgroundActivityProfile::Performance
    );
    assert_eq!(next.automatic_git_fetch_interval, 7_000);
    assert_eq!(next.provider_health_refresh_interval, 240_000);
}

#[test]
fn a_concrete_profile_does_not_reactivate_dormant_overrides() {
    let current = apply(
        &defaults(),
        json!({
            "backgroundActivity": {
                "schemaVersion": 1,
                "profile": "battery-saver",
                "overrides": { "providerHealthRefreshInterval": 5_000 },
            },
        }),
    );
    let next = apply(&current, json!({ "automaticGitFetchInterval": 15_000 }));
    assert_eq!(
        background(&next),
        json!({
            "schemaVersion": 1,
            "profile": "custom",
            "baseProfile": "battery-saver",
            "overrides": { "automaticGitFetchInterval": 15_000 },
        })
    );
}

#[test]
fn structured_background_settings_win_over_legacy_aliases() {
    let next = apply(
        &defaults(),
        json!({
            "backgroundActivity": { "schemaVersion": 1, "profile": "battery-saver", "overrides": {} },
            "automaticGitFetchInterval": 5_000,
            "backgroundActivityProfile": "performance",
        }),
    );
    assert_eq!(
        background(&next),
        json!({ "schemaVersion": 1, "profile": "battery-saver", "overrides": {} })
    );
    assert_eq!(
        next.background_activity_profile,
        BackgroundActivityProfile::BatterySaver
    );
    assert_eq!(next.automatic_git_fetch_interval, 0);
}

#[test]
fn custom_background_settings_reconcile_back_to_a_preset() {
    let custom = apply(&defaults(), json!({ "automaticGitFetchInterval": 15_000 }));
    let next = apply(&custom, json!({ "automaticGitFetchInterval": 30_000 }));
    assert_eq!(
        background(&next),
        json!({ "schemaVersion": 1, "profile": "balanced", "overrides": {} })
    );
    assert_eq!(
        next.background_activity_profile,
        BackgroundActivityProfile::Balanced
    );
    assert_eq!(next.automatic_git_fetch_interval, 30_000);
}

#[test]
fn custom_overrides_that_duplicate_the_base_profile_are_dropped() {
    let next = apply(
        &defaults(),
        json!({
            "backgroundActivity": {
                "schemaVersion": 1,
                "profile": "custom",
                "baseProfile": "balanced",
                "overrides": { "automaticGitFetchInterval": 30_000 },
            },
        }),
    );
    assert_eq!(
        background(&next),
        json!({ "schemaVersion": 1, "profile": "balanced", "overrides": {} })
    );
}

#[test]
fn a_background_patch_replaces_the_complete_override_record() {
    let current = apply(
        &defaults(),
        json!({
            "backgroundActivity": {
                "schemaVersion": 1,
                "profile": "custom",
                "baseProfile": "balanced",
                "overrides": {
                    "automaticGitFetchInterval": 15_000,
                    "providerHealthRefreshInterval": 180_000,
                },
            },
        }),
    );
    let next = apply(
        &current,
        json!({ "backgroundActivity": { "overrides": { "automaticGitFetchInterval": 10_000 } } }),
    );
    assert_eq!(
        background(&next),
        json!({
            "schemaVersion": 1,
            "profile": "custom",
            "baseProfile": "balanced",
            "overrides": { "automaticGitFetchInterval": 10_000 },
        })
    );
}

#[test]
fn interval_overrides_supplied_with_a_profile_patch_are_kept() {
    let next = apply(
        &defaults(),
        json!({
            "backgroundActivityProfile": "performance",
            "automaticGitFetchInterval": 0,
            "providerHealthRefreshInterval": 240_000,
        }),
    );
    assert_eq!(
        background(&next),
        json!({
            "schemaVersion": 1,
            "profile": "custom",
            "baseProfile": "performance",
            "overrides": {
                "automaticGitFetchInterval": 0,
                "providerHealthRefreshInterval": 240_000,
            },
        })
    );
}

#[test]
fn overrides_attached_to_a_concrete_background_profile_are_ignored() {
    let mut settings = defaults();
    settings.background_activity = serde_json::from_value(json!({
        "schemaVersion": 1,
        "profile": "balanced",
        "overrides": { "pauseWhenOnBattery": true },
    }))
    .unwrap();
    let resolved = resolve_server_background_activity_settings(&settings);
    assert!(!resolved.pause_when_on_battery);
}

fn minutes(value: u64) -> u64 {
    value * 60_000
}

// synchronized model preferences

fn legacy_favorites() -> Value {
    json!([{ "provider": "codex_work", "model": "sol" }])
}

fn legacy_preferences() -> Value {
    json!({ "codex_work": { "hiddenModels": ["astra"], "modelOrder": ["sol"] } })
}

#[test]
fn concurrent_adds_and_removes_of_the_same_favorite_are_idempotent() {
    let add = json!({ "setModelFavorites": [{ "provider": "codex_work", "model": "sol", "favorite": true }] });
    let first = apply(&defaults(), add.clone());
    let again = apply(&first, add);
    assert_eq!(
        serde_json::to_value(&again.favorites).unwrap(),
        json!([{ "provider": "codex_work", "model": "sol" }])
    );
    let remove = json!({ "setModelFavorites": [{ "provider": "codex_work", "model": "sol", "favorite": false }] });
    let removed = apply(&first, remove.clone());
    assert_eq!(
        serde_json::to_value(&apply(&removed, remove).favorites).unwrap(),
        json!([])
    );
}

#[test]
fn migration_patch_is_empty_for_empty_legacy_lists() {
    assert!(
        model_preferences_migration_patch(
            Some(&Value::Null),
            Some(&Value::Null),
            Some(&json!([])),
            Some(&json!({})),
        )
        .is_none()
    );
}

#[test]
fn migration_fills_only_the_lists_the_store_has_not_saved() {
    let patch = model_preferences_migration_patch(
        Some(&Value::Null),
        Some(&Value::Null),
        Some(&legacy_favorites()),
        Some(&legacy_preferences()),
    )
    .expect("migration patch");
    let migrated = apply(&defaults(), serde_json::to_value(&patch).unwrap());
    assert_eq!(
        serde_json::to_value(&migrated.favorites).unwrap(),
        legacy_favorites()
    );
    assert_eq!(
        serde_json::to_value(&migrated.provider_model_preferences).unwrap(),
        legacy_preferences()
    );
}

#[test]
fn a_delayed_migration_does_not_overwrite_explicit_clears() {
    let stale = model_preferences_migration_patch(
        Some(&Value::Null),
        Some(&Value::Null),
        Some(&legacy_favorites()),
        Some(&legacy_preferences()),
    )
    .expect("migration patch");
    let cleared = apply(
        &defaults(),
        json!({ "favorites": [], "providerModelPreferences": {} }),
    );
    let merged = apply(&cleared, serde_json::to_value(&stale).unwrap());
    let (favorites, preferences) = resolve_model_preferences(
        serde_json::to_value(&merged.favorites).ok(),
        serde_json::to_value(&merged.provider_model_preferences).ok(),
        Some(legacy_favorites()),
        Some(legacy_preferences()),
    );
    assert_eq!(favorites, json!([]));
    assert_eq!(preferences, json!({}));
}

#[test]
fn favorites_are_set_without_losing_other_clients_changes() {
    let first = apply(
        &defaults(),
        json!({ "setModelFavorites": [{ "provider": "codex_work", "model": "sol", "favorite": true }] }),
    );
    let second = apply(
        &first,
        json!({
            "setModelFavorites": [{ "provider": "codex_work", "model": "astra", "favorite": true }],
            "migrateModelPreferences": { "favorites": [] },
        }),
    );
    assert_eq!(
        serde_json::to_value(&second.favorites).unwrap(),
        json!([
            { "provider": "codex_work", "model": "sol" },
            { "provider": "codex_work", "model": "astra" },
        ])
    );
    let removed = apply(
        &second,
        json!({ "setModelFavorites": [{ "provider": "codex_work", "model": "sol", "favorite": false }] }),
    );
    assert_eq!(
        serde_json::to_value(&removed.favorites).unwrap(),
        json!([{ "provider": "codex_work", "model": "astra" }])
    );
}

#[test]
fn visibility_is_set_against_the_latest_state_and_keeps_order() {
    let initial = apply(
        &defaults(),
        json!({ "providerModelPreferences": legacy_preferences() }),
    );
    let first = apply(
        &initial,
        json!({ "setModelsHidden": [{ "provider": "codex_work", "model": "sol", "hidden": true }] }),
    );
    let second = apply(
        &first,
        json!({ "setModelsHidden": [{ "provider": "codex_work", "model": "luna", "hidden": true }] }),
    );
    let preferences = serde_json::to_value(&second.provider_model_preferences).unwrap();
    assert_eq!(
        preferences["codex_work"],
        json!({ "hiddenModels": ["astra", "sol", "luna"], "modelOrder": ["sol"] })
    );
    let restored = apply(
        &second,
        json!({ "setModelsHidden": [{ "provider": "codex_work", "model": "sol", "hidden": false }] }),
    );
    let preferences = serde_json::to_value(&restored.provider_model_preferences).unwrap();
    assert_eq!(
        preferences["codex_work"],
        json!({ "hiddenModels": ["astra", "luna"], "modelOrder": ["sol"] })
    );
}

#[test]
fn the_same_model_on_different_instances_is_a_separate_favorite() {
    let first = apply(
        &defaults(),
        json!({ "setModelFavorites": [{ "provider": "codex_work", "model": "sol", "favorite": true }] }),
    );
    let second = apply(
        &first,
        json!({ "setModelFavorites": [{ "provider": "codex_personal", "model": "sol", "favorite": true }] }),
    );
    assert_eq!(
        serde_json::to_value(&second.favorites).unwrap(),
        json!([
            { "provider": "codex_work", "model": "sol" },
            { "provider": "codex_personal", "model": "sol" },
        ])
    );
}
