//! Port of upstream/apps/server/src/provider/providerInstallation.ts
//!
//! Routes instance setup to the environment-owned installer without owning the download. The
//! installer is [`ManagedInstallation`], the surface of upstream's `CodexInstallation` service
//! (`CodexInstallation.ts` is not ported yet; the Codex driver will supply the implementation).
//!
//! Deviation: upstream also routes `antigravity` to `AntigravityInstallation`. Antigravity is
//! cut (PORTING.md), so `requireInstance` accepts Codex only, and the `AntigravitySettings` decode
//! and the `!isCodex` binary-path branch are gone.
//!
//! Deviation: upstream's three service dependencies (`ProviderInstanceRegistry`,
//! `ProviderRegistry`, `ServerSettingsService`) are the narrow ports below so routing is testable
//! without live drivers. The registries implement them. Settings come through [`ProviderHost`],
//! which wraps `ServerSettingsService.getSettings`.

#![allow(clippy::result_large_err)]

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use turin_contracts::{
    js_trim,
    provider_instance::{ProviderDriverKind, ProviderInstanceConfigMap, ProviderInstanceId},
    provider_setup::{
        ProviderInstallCancelInput, ProviderInstallState, ProviderSetupError, ProviderSetupInput,
    },
    settings::{CodexSettings, CodexSetupMode},
};
use turin_provider_core::{
    command_path::resolve_command_path,
    host_process::HostProcess,
    instance_environment::merge_provider_instance_environment,
    provider_host::ProviderHost,
    provider_instance_registry::ProviderInstanceRegistry,
    provider_instance_registry_hydration::{
        DefaultInstanceDriver, derive_provider_instance_config_map,
    },
};

use crate::provider_registry::ProviderRegistry;

/// `CodexInstallationError`: `{ operation, detail }`, reported to the caller as a
/// [`ProviderSetupError`] for the instance being set up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallationError {
    pub operation: String,
    pub detail: String,
}

/// The slice of upstream's `CodexInstallationService` that setup routing uses.
#[async_trait]
pub trait ManagedInstallation: Send + Sync {
    async fn start(&self) -> Result<ProviderInstallState, InstallationError>;
    async fn cancel(&self, operation_id: &str) -> Result<ProviderInstallState, InstallationError>;
    async fn state(&self) -> ProviderInstallState;
    fn changes(&self) -> BoxStream<'static, ProviderInstallState>;
    /// `protectedBinaryPaths`: executables other instances still use; the installer must not
    /// delete them.
    async fn remove(&self, protected_binary_paths: Vec<String>) -> Result<(), InstallationError>;
}

/// The slice of `ProviderInstanceRegistry` this reads: each instance's driver kind.
pub trait ProviderInstallationInstances: Send + Sync {
    fn driver_kind(&self, instance_id: &ProviderInstanceId) -> Option<ProviderDriverKind>;
    fn list_instances(&self) -> Vec<(ProviderInstanceId, ProviderDriverKind)>;
}

impl ProviderInstallationInstances for ProviderInstanceRegistry {
    fn driver_kind(&self, instance_id: &ProviderInstanceId) -> Option<ProviderDriverKind> {
        self.get_instance(instance_id)
            .map(|instance| instance.driver_kind.clone())
    }

    fn list_instances(&self) -> Vec<(ProviderInstanceId, ProviderDriverKind)> {
        ProviderInstanceRegistry::list_instances(self)
            .into_iter()
            .map(|instance| (instance.instance_id.clone(), instance.driver_kind.clone()))
            .collect()
    }
}

/// `ProviderRegistry.refreshInstance`.
#[async_trait]
pub trait ProviderInstanceRefresher: Send + Sync {
    async fn refresh_instance(&self, instance_id: &ProviderInstanceId);
}

#[async_trait]
impl ProviderInstanceRefresher for ProviderRegistry {
    async fn refresh_instance(&self, instance_id: &ProviderInstanceId) {
        ProviderRegistry::refresh_instance(self, instance_id).await;
    }
}

fn codex_driver() -> ProviderDriverKind {
    ProviderDriverKind::new("codex").expect("codex is a valid driver kind")
}

fn setup_error(
    instance_id: &ProviderInstanceId,
    operation: &str,
    detail: impl Into<String>,
) -> ProviderSetupError {
    ProviderSetupError::SetupError {
        instance_id: instance_id.clone(),
        operation: operation.to_owned(),
        detail: detail.into(),
        cause: None,
    }
}

fn failure(instance_id: &ProviderInstanceId, error: InstallationError) -> ProviderSetupError {
    setup_error(instance_id, &error.operation, error.detail)
}

pub struct ProviderInstallation {
    codex_installation: Arc<dyn ManagedInstallation>,
    instances: Arc<dyn ProviderInstallationInstances>,
    providers: Arc<dyn ProviderInstanceRefresher>,
    host: Arc<ProviderHost>,
    drivers: Vec<DefaultInstanceDriver>,
    host_process: HostProcess,
}

impl ProviderInstallation {
    /// `drivers` are the built-in driver slots `deriveProviderInstanceConfigMap` folds in.
    pub fn new(
        codex_installation: Arc<dyn ManagedInstallation>,
        instances: Arc<dyn ProviderInstallationInstances>,
        providers: Arc<dyn ProviderInstanceRefresher>,
        host: Arc<ProviderHost>,
        drivers: Vec<DefaultInstanceDriver>,
    ) -> Self {
        Self {
            codex_installation,
            instances,
            providers,
            host,
            drivers,
            host_process: HostProcess::current(),
        }
    }

    /// Test seam: the process environment the protected-path scan resolves commands against.
    #[cfg(test)]
    fn with_host_process(mut self, host_process: HostProcess) -> Self {
        self.host_process = host_process;
        self
    }

    async fn read_entries(
        &self,
        instance_id: &ProviderInstanceId,
        operation: &str,
    ) -> Result<ProviderInstanceConfigMap, ProviderSetupError> {
        let current = self.host.get_settings().await.map_err(|_| {
            setup_error(
                instance_id,
                operation,
                "Could not read provider installation settings.",
            )
        })?;
        Ok(derive_provider_instance_config_map(&current, &self.drivers))
    }

    async fn require_instance(
        &self,
        instance_id: &ProviderInstanceId,
        operation: &str,
        managed_only: bool,
    ) -> Result<(Arc<dyn ManagedInstallation>, ProviderDriverKind), ProviderSetupError> {
        let driver_kind = self.instances.driver_kind(instance_id);
        let Some(driver_kind) = driver_kind.filter(|kind| *kind == codex_driver()) else {
            return Err(setup_error(
                instance_id,
                operation,
                "Managed installation is not available for this provider instance.",
            ));
        };
        let entries = self.read_entries(instance_id, operation).await?;
        let invalid_config = || {
            setup_error(
                instance_id,
                operation,
                "The provider instance configuration is invalid.",
            )
        };
        let raw_config = entries
            .get(instance_id)
            .and_then(|entry| entry.config.clone())
            .unwrap_or_else(|| serde_json::json!({}));
        let config: CodexSettings =
            serde_json::from_value(raw_config).map_err(|_| invalid_config())?;
        if config.setup_mode != Some(CodexSetupMode::Managed) {
            return Err(setup_error(
                instance_id,
                operation,
                "Choose managed setup to install Codex in T3 Code.",
            ));
        }
        if managed_only
            && !config.binary_path.as_str().is_empty()
            && config.binary_path.as_str() != "codex"
        {
            return Err(setup_error(
                instance_id,
                operation,
                "This instance uses a custom executable. Clear its binary path to manage installation in T3 Code.",
            ));
        }
        Ok((Arc::clone(&self.codex_installation), driver_kind))
    }

    pub async fn start(
        &self,
        input: &ProviderSetupInput,
    ) -> Result<ProviderInstallState, ProviderSetupError> {
        let (installation, _) = self
            .require_instance(&input.instance_id, "install", true)
            .await?;
        installation
            .start()
            .await
            .map_err(|error| failure(&input.instance_id, error))
    }

    pub async fn cancel(
        &self,
        input: &ProviderInstallCancelInput,
    ) -> Result<ProviderInstallState, ProviderSetupError> {
        let (installation, _) = self
            .require_instance(&input.instance_id, "cancel-install", false)
            .await?;
        installation
            .cancel(input.operation_id.as_str())
            .await
            .map_err(|error| failure(&input.instance_id, error))
    }

    /// The installer's state stream, once the instance is known to be managed.
    pub async fn subscribe(
        &self,
        input: &ProviderSetupInput,
    ) -> Result<BoxStream<'static, ProviderInstallState>, ProviderSetupError> {
        let (installation, _) = self
            .require_instance(&input.instance_id, "observe-install", false)
            .await?;
        Ok(installation.changes())
    }

    pub async fn remove(
        &self,
        input: &ProviderSetupInput,
    ) -> Result<ProviderInstallState, ProviderSetupError> {
        let (installation, driver) = self
            .require_instance(&input.instance_id, "remove-install", true)
            .await?;
        let entries = self
            .read_entries(&input.instance_id, "remove-install")
            .await?;
        let mut protected_paths: Vec<String> = Vec::new();
        for entry in entries.values() {
            let Some(binary_path) = entry
                .config
                .as_ref()
                .and_then(|config| config.get("binaryPath"))
                .and_then(|value| value.as_str())
                .map(js_trim)
                .filter(|binary_path| !binary_path.is_empty())
            else {
                continue;
            };
            let env = merge_provider_instance_environment(
                entry.environment.as_ref(),
                &self.host_process.environment,
                &self.host_process.home_directory,
            );
            let resolved = resolve_command_path(binary_path, &env, self.host_process.platform);
            protected_paths.push(binary_path.to_owned());
            protected_paths.extend(resolved);
        }
        installation
            .remove(protected_paths)
            .await
            .map_err(|error| failure(&input.instance_id, error))?;
        for (instance_id, kind) in self.instances.list_instances() {
            if kind == driver {
                self.providers.refresh_instance(&instance_id).await;
            }
        }
        Ok(installation.state().await)
    }
}

#[cfg(test)]
mod tests {
    //! Port of upstream/apps/server/src/provider/providerInstallation.test.ts. Upstream's cases
    //! drive Antigravity; Antigravity is cut, so the same routing cases run against Codex's
    //! managed setup (upstream's own Codex cases are ported as written).

    use futures::StreamExt;
    use parking_lot::Mutex;
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};
    use turin_contracts::settings::ServerSettings;

    use super::*;

    fn codex_id() -> ProviderInstanceId {
        ProviderInstanceId::new("codex").unwrap()
    }

    fn install_state(driver: &str) -> ProviderInstallState {
        serde_json::from_value(json!({
            "driver": driver,
            "operationId": null,
            "phase": "idle",
            "downloadedBytes": 0,
            "totalBytes": null,
            "version": null,
            "installedVersion": null,
            "canRemove": false,
            "message": null,
        }))
        .unwrap()
    }

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<String>>,
        protected_paths: Mutex<Vec<String>>,
    }

    struct FakeInstallation(Arc<Recorder>);

    #[async_trait]
    impl ManagedInstallation for FakeInstallation {
        async fn start(&self) -> Result<ProviderInstallState, InstallationError> {
            self.0.calls.lock().push("codex-start".to_owned());
            Ok(install_state("codex"))
        }

        async fn cancel(
            &self,
            _operation_id: &str,
        ) -> Result<ProviderInstallState, InstallationError> {
            self.0.calls.lock().push("codex-cancel".to_owned());
            Ok(install_state("codex"))
        }

        async fn state(&self) -> ProviderInstallState {
            install_state("codex")
        }

        fn changes(&self) -> BoxStream<'static, ProviderInstallState> {
            futures::stream::iter([install_state("codex")]).boxed()
        }

        async fn remove(&self, protected: Vec<String>) -> Result<(), InstallationError> {
            *self.0.protected_paths.lock() = protected;
            self.0.calls.lock().push("codex-remove".to_owned());
            Ok(())
        }
    }

    struct FakeInstances(Vec<(ProviderInstanceId, ProviderDriverKind)>);

    impl ProviderInstallationInstances for FakeInstances {
        fn driver_kind(&self, instance_id: &ProviderInstanceId) -> Option<ProviderDriverKind> {
            self.0
                .iter()
                .find(|(id, _)| id == instance_id)
                .map(|(_, kind)| kind.clone())
        }

        fn list_instances(&self) -> Vec<(ProviderInstanceId, ProviderDriverKind)> {
            self.0.clone()
        }
    }

    struct FakeRefresher(Arc<Recorder>);

    #[async_trait]
    impl ProviderInstanceRefresher for FakeRefresher {
        async fn refresh_instance(&self, _instance_id: &ProviderInstanceId) {
            self.0.calls.lock().push("refresh".to_owned());
        }
    }

    fn host_with(settings: Value) -> Arc<ProviderHost> {
        let settings: ServerSettings = serde_json::from_value(settings).unwrap();
        let get_settings = settings.clone();
        Arc::new(ProviderHost::new(
            turin_provider_core::provider_host::ProviderHostPaths {
                cwd: "/".to_owned(),
                base_dir: "/".to_owned(),
                state_dir: "/".to_owned(),
                provider_status_cache_dir: "/".to_owned(),
                attachments_dir: "/".to_owned(),
            },
            Arc::new(move || {
                let settings = get_settings.clone();
                Box::pin(async move { Ok(settings) })
            }),
            Arc::new(|_| unreachable!("installation routing does not snapshot settings")),
            Arc::new(|| unreachable!("installation routing does not watch settings")),
            Arc::new(|_| Box::pin(async { true })),
            Arc::new(|_| None),
            Arc::new(|_, _| unreachable!("installation routing does not open credentials")),
        ))
    }

    struct Harness {
        router: ProviderInstallation,
        recorder: Arc<Recorder>,
    }

    fn harness(kind: &str, id: &ProviderInstanceId, provider_instances: Value) -> Harness {
        harness_with_env(kind, id, provider_instances, HostProcess::current())
    }

    fn harness_with_env(
        kind: &str,
        id: &ProviderInstanceId,
        provider_instances: Value,
        host_process: HostProcess,
    ) -> Harness {
        let recorder = Arc::new(Recorder::default());
        let router = ProviderInstallation::new(
            Arc::new(FakeInstallation(Arc::clone(&recorder))),
            Arc::new(FakeInstances(vec![(
                id.clone(),
                ProviderDriverKind::new(kind).unwrap(),
            )])),
            Arc::new(FakeRefresher(Arc::clone(&recorder))),
            host_with(json!({ "providerInstances": provider_instances })),
            // No built-in default instances: the settings under test are the whole map.
            Vec::new(),
        )
        .with_host_process(host_process);
        Harness { router, recorder }
    }

    fn managed_codex() -> Value {
        json!({ "codex": { "driver": "codex", "config": { "setupMode": "managed" } } })
    }

    fn input(id: &ProviderInstanceId) -> ProviderSetupInput {
        ProviderSetupInput {
            instance_id: id.clone(),
        }
    }

    fn calls(harness: &Harness) -> Vec<String> {
        harness.recorder.calls.lock().clone()
    }

    fn detail(error: &ProviderSetupError) -> &str {
        let ProviderSetupError::SetupError { detail, .. } = error;
        detail
    }

    // "allows explicit installation while the provider is disabled": routing reads the driver and
    // the setup mode, never `enabled`.
    #[tokio::test]
    async fn allows_explicit_installation_while_the_provider_is_disabled() {
        let harness = harness(
            "codex",
            &codex_id(),
            json!({ "codex": {
                "driver": "codex",
                "enabled": false,
                "config": { "setupMode": "managed" },
            } }),
        );
        harness.router.start(&input(&codex_id())).await.unwrap();
        assert_eq!(calls(&harness), vec!["codex-start".to_owned()]);
    }

    #[tokio::test]
    async fn rejects_another_driver_and_unknown_instances_before_installation() {
        let harness = harness("claudeAgent", &codex_id(), managed_codex());
        let wrong_driver = harness.router.start(&input(&codex_id())).await.unwrap_err();
        let missing = harness
            .router
            .start(&input(&ProviderInstanceId::new("missing").unwrap()))
            .await
            .unwrap_err();
        assert!(detail(&wrong_driver).contains("not available"));
        assert!(detail(&missing).contains("not available"));
        assert_eq!(calls(&harness), Vec::<String>::new());
    }

    #[tokio::test]
    async fn keeps_external_installs_manual_without_hiding_shared_install_status() {
        let harness = harness(
            "codex",
            &codex_id(),
            json!({ "codex": { "driver": "codex", "config": { "setupMode": "managed", "binaryPath": "/external/codex" } } }),
        );
        let start = harness.router.start(&input(&codex_id())).await.unwrap_err();
        let remove = harness
            .router
            .remove(&input(&codex_id()))
            .await
            .unwrap_err();
        assert!(detail(&start).contains("custom executable"));
        assert!(detail(&remove).contains("custom executable"));
        let observed: Vec<_> = harness
            .router
            .subscribe(&input(&codex_id()))
            .await
            .unwrap()
            .collect()
            .await;
        assert_eq!(observed, vec![install_state("codex")]);
        assert_eq!(calls(&harness), Vec::<String>::new());
    }

    #[tokio::test]
    async fn routes_a_managed_codex_instance_through_its_own_installer_and_refreshes_removal() {
        let harness = harness("codex", &codex_id(), managed_codex());
        let started = harness.router.start(&input(&codex_id())).await.unwrap();
        assert_eq!(started.driver.as_str(), "codex");
        harness
            .router
            .cancel(&ProviderInstallCancelInput {
                instance_id: codex_id(),
                operation_id: turin_contracts::provider_setup::SetupOperationId::new("operation")
                    .unwrap(),
            })
            .await
            .unwrap();
        let observed: Vec<_> = harness
            .router
            .subscribe(&input(&codex_id()))
            .await
            .unwrap()
            .collect()
            .await;
        assert_eq!(observed[0].driver.as_str(), "codex");
        harness.router.remove(&input(&codex_id())).await.unwrap();
        assert_eq!(
            calls(&harness),
            vec!["codex-start", "codex-cancel", "codex-remove", "refresh"]
        );
    }

    #[tokio::test]
    async fn keeps_native_codex_installation_outside_managed_setup() {
        let harness = harness(
            "codex",
            &codex_id(),
            json!({ "codex": { "driver": "codex", "config": { "setupMode": "existing" } } }),
        );
        let error = harness.router.start(&input(&codex_id())).await.unwrap_err();
        assert!(detail(&error).contains("Choose managed setup"));
        assert_eq!(calls(&harness), Vec::<String>::new());
    }

    #[tokio::test]
    async fn reports_an_invalid_instance_configuration() {
        let harness = harness(
            "codex",
            &codex_id(),
            json!({ "codex": { "driver": "codex", "config": { "setupMode": 7 } } }),
        );
        let error = harness.router.start(&input(&codex_id())).await.unwrap_err();
        assert_eq!(
            detail(&error),
            "The provider instance configuration is invalid."
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn protects_another_instances_binary_found_through_its_own_path() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("codex-test");
        std::fs::write(&executable, "test").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let harness = harness(
            "codex",
            &codex_id(),
            json!({
                "codex": { "driver": "codex", "config": { "setupMode": "managed" } },
                "codex-work": {
                    "driver": "codex",
                    "config": { "binaryPath": "codex-test" },
                    "environment": [
                        { "name": "PATH", "value": directory.path().display().to_string(), "sensitive": false }
                    ],
                },
            }),
        );
        harness.router.remove(&input(&codex_id())).await.unwrap();
        assert!(
            harness
                .recorder
                .protected_paths
                .lock()
                .contains(&executable.display().to_string())
        );
        assert_eq!(calls(&harness), vec!["codex-remove", "refresh"]);
    }

    #[tokio::test]
    async fn keeps_only_the_configured_path_when_another_instances_binary_is_missing() {
        let directory = tempfile::tempdir().unwrap();
        let binary_path = directory.path().join("missing").display().to_string();
        let harness = harness(
            "codex",
            &codex_id(),
            json!({
                "codex": { "driver": "codex", "config": { "setupMode": "managed" } },
                "codex-work": { "driver": "codex", "config": { "binaryPath": binary_path } },
            }),
        );
        harness.router.remove(&input(&codex_id())).await.unwrap();
        assert_eq!(*harness.recorder.protected_paths.lock(), vec![binary_path]);
    }
}
