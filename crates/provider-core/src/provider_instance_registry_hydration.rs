//! Port of `deriveProviderInstanceConfigMap` in
//! upstream/apps/server/src/provider/ProviderInstanceRegistryHydration.ts.
//!
//! The settings watcher that calls `reconcile` lives with the app: this crate
//! does not own `ServerSettingsService`. The pure derivation is what that
//! watcher must pass in.

use turin_contracts::{
    provider_instance::{
        ProviderDriverKind, ProviderInstanceConfig, ProviderInstanceConfigMap,
        default_instance_id_for_driver,
    },
    settings::{
        ServerSettings, provider_instance_config_enabled_flag, resolve_provider_instance_enabled,
    },
};

/// A built-in driver slot. `has_default_instance == false` opts out of the
/// implicit instance at `default_instance_id_for_driver`.
pub struct DefaultInstanceDriver {
    pub driver_kind: ProviderDriverKind,
    pub has_default_instance: bool,
}

/// Explicit `providerInstances` entries plus an implicit default instance for
/// each built-in driver whose default slot is empty. A project that turns an
/// instance on folds `enabled: true` into the machine map so the instance is
/// actually probed. An instance absent from settings stays absent.
pub fn derive_provider_instance_config_map(
    settings: &ServerSettings,
    drivers: &[DefaultInstanceDriver],
) -> ProviderInstanceConfigMap {
    let mut merged = settings.provider_instances.clone();
    for driver in drivers {
        if !driver.has_default_instance {
            continue;
        }
        let instance_id = default_instance_id_for_driver(&driver.driver_kind);
        if merged.contains_key(&instance_id) {
            continue;
        }
        merged.insert(
            instance_id,
            ProviderInstanceConfig {
                driver: driver.driver_kind.clone(),
                display_name: None,
                accent_color: None,
                environment: None,
                enabled: None,
                config: None,
            },
        );
    }

    for overrides in settings.project_settings_overrides.values() {
        let Some(project_instances) = overrides.provider_instances.as_ref() else {
            continue;
        };
        for (instance_id, project_override) in project_instances {
            if project_override.enabled != Some(true) {
                continue;
            }
            let Some(existing) = merged.get(instance_id).cloned() else {
                continue;
            };
            if resolve_provider_instance_enabled(&existing) {
                continue;
            }
            let mut next = existing;
            next.enabled = Some(true);
            if provider_instance_config_enabled_flag(next.config.as_ref()) == Some(false) {
                if let Some(serde_json::Value::Object(config)) = next.config.as_mut() {
                    config.insert("enabled".to_owned(), serde_json::Value::Bool(true));
                }
            }
            merged.insert(instance_id.clone(), next);
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::{
        ProjectId,
        provider_instance::ProviderInstanceId,
        settings::{
            ProjectProviderInstanceOverride, ProjectSettingsOverrides, default_server_settings,
        },
    };

    fn id(value: &str) -> ProviderInstanceId {
        ProviderInstanceId::new(value).unwrap()
    }

    fn project(value: &str) -> ProjectId {
        ProjectId::new(value).unwrap()
    }

    fn codex(enabled: Option<bool>, config: Option<serde_json::Value>) -> ProviderInstanceConfig {
        ProviderInstanceConfig {
            driver: ProviderDriverKind::new("codex").unwrap(),
            display_name: None,
            accent_color: None,
            environment: None,
            enabled,
            config,
        }
    }

    fn settings_with(
        instances: ProviderInstanceConfigMap,
        overrides: Vec<(ProjectId, Vec<(ProviderInstanceId, Option<bool>)>)>,
    ) -> ServerSettings {
        let mut settings = default_server_settings();
        settings.provider_instances = instances;
        for (project_id, instances) in overrides {
            let mut map = indexmap::IndexMap::new();
            for (instance_id, enabled) in instances {
                map.insert(instance_id, ProjectProviderInstanceOverride { enabled });
            }
            settings.project_settings_overrides.insert(
                project_id,
                ProjectSettingsOverrides {
                    provider_instances: Some(map),
                    ..ProjectSettingsOverrides::default()
                },
            );
        }
        settings
    }

    #[test]
    fn turns_a_machine_disabled_instance_on_when_a_project_enables_it() {
        let instance_id = id("codex_work");
        let settings = settings_with(
            indexmap::IndexMap::from([(instance_id.clone(), codex(Some(false), None))]),
            vec![(project("project-a"), vec![(instance_id.clone(), Some(true))])],
        );
        let merged = derive_provider_instance_config_map(&settings, &[]);
        assert_eq!(merged[&instance_id].enabled, Some(true));
        assert!(merged[&instance_id].config.is_none());
    }

    #[test]
    fn clears_a_nested_config_enabled_false_when_a_project_enables_the_instance() {
        let instance_id = id("codex_work");
        let settings = settings_with(
            indexmap::IndexMap::from([(
                instance_id.clone(),
                codex(None, Some(serde_json::json!({"enabled": false}))),
            )]),
            vec![(project("project-a"), vec![(instance_id.clone(), Some(true))])],
        );
        let merged = derive_provider_instance_config_map(&settings, &[]);
        assert_eq!(merged[&instance_id].enabled, Some(true));
        assert_eq!(merged[&instance_id].config, Some(serde_json::json!({"enabled": true})));
    }

    #[test]
    fn leaves_an_already_enabled_instance_untouched() {
        let instance_id = id("codex_work");
        let original = codex(Some(true), Some(serde_json::json!({"binaryPath": "codex"})));
        let settings = settings_with(
            indexmap::IndexMap::from([(instance_id.clone(), original.clone())]),
            vec![(project("project-a"), vec![(instance_id.clone(), Some(true))])],
        );
        let merged = derive_provider_instance_config_map(&settings, &[]);
        assert_eq!(merged[&instance_id], original);
    }

    #[test]
    fn does_not_revive_an_instance_settings_no_longer_configure() {
        let instance_id = id("codex_work");
        let settings = settings_with(
            indexmap::IndexMap::new(),
            vec![(project("project-a"), vec![(instance_id.clone(), Some(true))])],
        );
        let merged = derive_provider_instance_config_map(&settings, &[]);
        assert!(!merged.contains_key(&instance_id));
    }

    #[test]
    fn ignores_a_project_override_that_disables_an_instance() {
        let instance_id = id("codex_work");
        let original = codex(Some(true), None);
        let settings = settings_with(
            indexmap::IndexMap::from([(instance_id.clone(), original.clone())]),
            vec![(project("project-a"), vec![(instance_id.clone(), Some(false))])],
        );
        let merged = derive_provider_instance_config_map(&settings, &[]);
        assert_eq!(merged[&instance_id], original);
    }

    #[test]
    fn adds_a_default_instance_when_the_slot_is_empty() {
        let settings = default_server_settings();
        let driver = ProviderDriverKind::new("codex").unwrap();
        let merged = derive_provider_instance_config_map(
            &settings,
            &[
                DefaultInstanceDriver {
                    driver_kind: driver.clone(),
                    has_default_instance: true,
                },
                DefaultInstanceDriver {
                    driver_kind: ProviderDriverKind::new("cursor").unwrap(),
                    has_default_instance: false,
                },
            ],
        );
        let instance_id = default_instance_id_for_driver(&driver);
        assert_eq!(merged[&instance_id].driver, driver);
        assert!(merged[&instance_id].enabled.is_none());
        assert!(!merged.contains_key(&ProviderInstanceId::new("cursor").unwrap()));
    }
}
