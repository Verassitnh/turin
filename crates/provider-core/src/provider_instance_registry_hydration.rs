//! Port of `deriveProviderInstanceConfigMap` in
//! upstream/apps/server/src/provider/ProviderInstanceRegistryHydration.ts.
//!
//! A scoped watcher subscribes through `ProviderHost` and reconciles the
//! registry when provider settings change.

use std::sync::Arc;

use tracing::error;

use crate::{
    driver::ProviderDriver, provider_host::ProviderHost,
    provider_instance_registry::ProviderInstanceRegistry,
};

use turin_contracts::{
    provider_instance::{
        ProviderDriverKind, ProviderInstanceConfig, ProviderInstanceConfigMap,
        default_instance_id_for_driver,
    },
    settings::{
        ServerSettings, ServerSettingsError, provider_instance_config_enabled_flag,
        resolve_provider_instance_enabled,
    },
};

/// A built-in driver slot. `has_default_instance == false` opts out of the
/// implicit instance at `default_instance_id_for_driver`.
pub struct DefaultInstanceDriver {
    pub driver_kind: ProviderDriverKind,
    pub has_default_instance: bool,
}

/// Owns both the registry and its settings watcher. Dropping this handle
/// interrupts the watcher rather than leaving a detached task behind.
pub struct ProviderInstanceRegistryHydration {
    pub registry: Arc<ProviderInstanceRegistry>,
    watcher: tokio::task::JoinHandle<()>,
}

impl ProviderInstanceRegistryHydration {
    /// Subscribes before the initial read, so a write between the two is
    /// picked up by the first reconcile instead of being lost. A failed
    /// initial read fails construction, like upstream's layer build.
    #[allow(clippy::result_large_err)]
    pub async fn new(
        drivers: Vec<Arc<dyn ProviderDriver>>,
        host: &ProviderHost,
    ) -> Result<Self, ServerSettingsError> {
        let defaults: Vec<_> = drivers
            .iter()
            .map(|driver| DefaultInstanceDriver {
                driver_kind: driver.driver_kind().clone(),
                has_default_instance: driver.metadata().has_default_instance,
            })
            .collect();
        let mut changes = host.subscribe_settings();
        let initial = host.get_settings().await?;
        let registry = ProviderInstanceRegistry::new(
            drivers,
            derive_provider_instance_config_map(&initial, &defaults),
        )
        .await;
        let watched = Arc::clone(&registry);
        let watcher = tokio::spawn(async move {
            while let Some(mut settings) = changes.recv().await {
                // Only the newest settings matter: reconcile is idempotent on
                // the final map, so intermediate emissions are dropped rather
                // than each costing a reconcile.
                while let Ok(next) = changes.try_recv() {
                    settings = next;
                }
                watched
                    .reconcile(derive_provider_instance_config_map(&settings, &defaults))
                    .await;
            }
            error!("ProviderInstanceRegistry settings watcher ended");
        });
        Ok(Self { registry, watcher })
    }
}

impl Drop for ProviderInstanceRegistryHydration {
    fn drop(&mut self) {
        self.watcher.abort();
    }
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
            if provider_instance_config_enabled_flag(next.config.as_ref()) == Some(false)
                && let Some(serde_json::Value::Object(config)) = next.config.as_mut()
            {
                config.insert("enabled".to_owned(), serde_json::Value::Bool(true));
            }
            merged.insert(instance_id.clone(), next);
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    type ProjectInstanceEnables = Vec<(ProviderInstanceId, Option<bool>)>;
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
        overrides: Vec<(ProjectId, ProjectInstanceEnables)>,
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
            vec![(
                project("project-a"),
                vec![(instance_id.clone(), Some(true))],
            )],
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
            vec![(
                project("project-a"),
                vec![(instance_id.clone(), Some(true))],
            )],
        );
        let merged = derive_provider_instance_config_map(&settings, &[]);
        assert_eq!(merged[&instance_id].enabled, Some(true));
        assert_eq!(
            merged[&instance_id].config,
            Some(serde_json::json!({"enabled": true}))
        );
    }

    #[test]
    fn leaves_an_already_enabled_instance_untouched() {
        let instance_id = id("codex_work");
        let original = codex(Some(true), Some(serde_json::json!({"binaryPath": "codex"})));
        let settings = settings_with(
            indexmap::IndexMap::from([(instance_id.clone(), original.clone())]),
            vec![(
                project("project-a"),
                vec![(instance_id.clone(), Some(true))],
            )],
        );
        let merged = derive_provider_instance_config_map(&settings, &[]);
        assert_eq!(merged[&instance_id], original);
    }

    #[test]
    fn does_not_revive_an_instance_settings_no_longer_configure() {
        let instance_id = id("codex_work");
        let settings = settings_with(
            indexmap::IndexMap::new(),
            vec![(
                project("project-a"),
                vec![(instance_id.clone(), Some(true))],
            )],
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
            vec![(
                project("project-a"),
                vec![(instance_id.clone(), Some(false))],
            )],
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

#[cfg(test)]
mod watcher_tests {
    use std::{any::Any, sync::Mutex};

    use tokio::sync::mpsc;
    use turin_contracts::{
        chat_attachment::ChatAttachment, provider_instance::ProviderInstanceId,
        settings::default_server_settings,
    };

    use super::*;
    use crate::{
        background_scope::BackgroundScope,
        credential_store::{MemorySecretStore, ProviderCredentials},
        provider_host::{ProviderHost, ProviderHostPaths, SettingsSnapshotJob},
        rt::BoxFuture,
    };

    fn host(
        settings: mpsc::UnboundedReceiver<ServerSettings>,
        initial: ServerSettings,
    ) -> ProviderHost {
        // `subscribe_settings` hands out its receiver once; the test keeps the sender.
        let receiver = Mutex::new(Some(settings));
        ProviderHost::new(
            ProviderHostPaths {
                cwd: "/tmp".to_owned(),
                base_dir: "/tmp".to_owned(),
                state_dir: "/tmp".to_owned(),
                provider_status_cache_dir: "/tmp".to_owned(),
                attachments_dir: "/tmp".to_owned(),
            },
            Arc::new(move || -> BoxFuture<'static, _> {
                let settings = initial.clone();
                Box::pin(async move { Ok(settings) })
            }),
            Arc::new(|_job: SettingsSnapshotJob| -> BoxFuture<'static, _> {
                Box::pin(async { Ok(Box::new(()) as Box<dyn Any + Send>) })
            }),
            Arc::new(move || receiver.lock().unwrap().take().expect("subscribed once")),
            Arc::new(|_scope: BackgroundScope| -> BoxFuture<'static, bool> {
                Box::pin(async { true })
            }),
            Arc::new(|_attachment: &ChatAttachment| -> Option<String> { None }),
            Arc::new(|namespace: String, binding: String| {
                ProviderCredentials::open(Arc::new(MemorySecretStore::new()), &namespace, &binding)
            }),
        )
    }

    #[tokio::test]
    async fn a_settings_change_reconciles_the_registry_once_it_is_published() {
        let (sender, receiver) = mpsc::unbounded_channel();
        let host = host(receiver, default_server_settings());
        let hydration = ProviderInstanceRegistryHydration::new(Vec::new(), &host)
            .await
            .unwrap();
        assert!(hydration.registry.list_unavailable().is_empty());

        let mut changes = hydration.registry.subscribe_changes();
        let mut next = default_server_settings();
        next.provider_instances.insert(
            ProviderInstanceId::new("codex").unwrap(),
            ProviderInstanceConfig {
                driver: ProviderDriverKind::new("codex").unwrap(),
                display_name: None,
                accent_color: None,
                environment: None,
                enabled: None,
                config: None,
            },
        );
        sender.send(next).unwrap();

        changes
            .recv()
            .await
            .expect("reconcile publishes its change");
        let unavailable = hydration.registry.list_unavailable();
        assert_eq!(unavailable.len(), 1);
        assert_eq!(unavailable[0].instance_id.as_str(), "codex");
    }
}
