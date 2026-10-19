//! `ProviderDriver` for Cursor.
//!
//! `create` materializes one instance. Each `open_session` then owns its own
//! `cursor-agent acp` process. Text generation is not on this transport.
//!
//! The snapshot is probed on refresh, not at creation: a probe spawns
//! `cursor-agent`, so it waits for the first refresh the host asks for.
//!
//! Port of upstream/packages/provider-cursor/src/server/driver.ts.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream;
use serde_json::Value;
use tokio::sync::watch;
use turin_contracts::UtcDateTime;
use turin_contracts::git::TextGenerationError;
use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};
use turin_contracts::provider_usage_limits::ProviderUsageLimitsUpdate;
use turin_provider_core::driver::{
    ProviderDriver, ProviderDriverCreateInput, ProviderDriverMetadata, ProviderInstance,
    RefreshModels, default_provider_continuation_identity, driver_error,
};
use turin_provider_core::errors::ProviderDriverError;
use turin_provider_core::host_process::{HostProcess, ProcessEnv};
use turin_provider_core::instance_environment::merge_provider_instance_environment;
use turin_provider_core::maintenance::ProviderMaintenanceCapabilities;
use turin_provider_core::maintenance_resolver::make_manual_only_provider_maintenance_capabilities;
use turin_provider_core::provider_host::ProviderHost;
use turin_provider_core::rt::{BoxFuture, BoxStream};
use turin_provider_core::server_provider::{ServerProvider, ServerProviderAuthStatus};
use turin_provider_core::snapshot::ManagedServerProvider;
use turin_provider_core::text_generation::{
    BranchNameGenerationInput, BranchNameGenerationResult, CommitMessageGenerationInput,
    CommitMessageGenerationResult, PrContentGenerationInput, PrContentGenerationResult,
    ProviderTextGeneration, ReviewGuideGenerationInput, ReviewGuideGenerationResult,
    ThreadTitleGenerationInput, ThreadTitleGenerationResult,
};
use turin_provider_core::usage::ProviderUsageReader;

use crate::probe::probe_status;
use crate::session::CursorAdapter;
use crate::settings::CursorSettings;
use crate::sign_in;
use crate::status;
use crate::status::CursorProbe;
use crate::usage_host::{CursorUsageLimitsSource, HostCursorUsageLimits, cursor_usage_reader};

/// Runs one status probe of the agent at `program`. A seam so tests do not spawn
/// `cursor-agent`.
type ProbeStatus = Arc<dyn Fn(String) -> BoxFuture<'static, CursorProbe> + Send + Sync>;

fn spawn_probe() -> ProbeStatus {
    Arc::new(|program| Box::pin(async move { probe_status(&program).await }))
}

pub struct CursorDriver {
    kind: ProviderDriverKind,
    metadata: ProviderDriverMetadata,
    /// The account history the usage page scans. Absent without a host.
    usage: Option<ProviderUsageReader>,
    /// Where the status probe reads plan limit windows. Absent without a host.
    usage_limits: Option<Arc<dyn CursorUsageLimitsSource>>,
    probe: ProbeStatus,
}

impl CursorDriver {
    pub fn new() -> Self {
        Self {
            kind: ProviderDriverKind::new("cursor").expect("cursor driver slug"),
            metadata: ProviderDriverMetadata::new("Cursor"),
            usage: None,
            usage_limits: None,
            probe: spawn_probe(),
        }
    }

    /// The driver the app runs: usage history and limit windows come from the host's
    /// state directory, settings and the process environment. A failure to build the
    /// HTTP clients leaves the usage side off rather than the driver.
    pub fn with_host(host: Arc<ProviderHost>) -> Self {
        let process = HostProcess::current();
        let usage = cursor_usage_reader(Path::new(&host.paths.state_dir), &process)
            .map_err(|error| tracing::warn!(%error, "Cursor account usage is unavailable"))
            .ok();
        let usage_limits = HostCursorUsageLimits::new(host, process)
            .map_err(|error| tracing::warn!(%error, "Cursor usage limits are unavailable"))
            .ok()
            .map(|source| Arc::new(source) as Arc<dyn CursorUsageLimitsSource>);
        Self {
            usage,
            usage_limits,
            ..Self::new()
        }
    }

    /// Replaces the limit source and the probe; for tests.
    #[cfg(test)]
    fn with_test_seams(usage_limits: Arc<dyn CursorUsageLimitsSource>, probe: ProbeStatus) -> Self {
        Self {
            usage_limits: Some(usage_limits),
            probe,
            ..Self::new()
        }
    }
}

impl Default for CursorDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProviderDriver for CursorDriver {
    fn driver_kind(&self) -> &ProviderDriverKind {
        &self.kind
    }

    fn metadata(&self) -> &ProviderDriverMetadata {
        &self.metadata
    }

    fn default_config(&self) -> Value {
        serde_json::to_value(CursorSettings::default())
            .unwrap_or_else(|_| Value::Object(Default::default()))
    }

    fn usage(&self) -> Option<&ProviderUsageReader> {
        self.usage.as_ref()
    }

    fn decode_config(&self, raw: &Value) -> Result<Value, String> {
        let settings = if raw.is_null() {
            CursorSettings::default()
        } else {
            CursorSettings::from_value(raw.clone()).map_err(|error| error.to_string())?
        };
        serde_json::to_value(settings).map_err(|error| error.to_string())
    }

    async fn create(
        &self,
        input: ProviderDriverCreateInput,
    ) -> Result<ProviderInstance, ProviderDriverError> {
        let settings = CursorSettings::from_value(input.config.clone())
            .map_err(|error| driver_error(&self.kind, &input.instance_id, error.to_string()))?;
        let adapter = CursorAdapter::from_settings(input.instance_id.clone(), &settings);
        let enabled = settings.enabled && input.enabled;
        let environment = merge_provider_instance_environment(
            Some(&input.environment),
            &HostProcess::current().environment,
            &HostProcess::current().home_directory,
        );
        let mut snapshot = CursorSnapshot::new(
            self.kind.clone(),
            input.instance_id.clone(),
            settings.clone(),
            enabled,
        );
        snapshot.environment = environment;
        snapshot.usage_limits = self.usage_limits.clone();
        snapshot.prober = Arc::clone(&self.probe);
        let snapshot = Arc::new(snapshot);

        let refresh_snapshot = Arc::clone(&snapshot);
        let refresh_models: RefreshModels = Arc::new(move || {
            let snapshot = Arc::clone(&refresh_snapshot);
            Box::pin(async move {
                snapshot.refresh().await;
                Ok(())
            }) as BoxFuture<'static, Result<(), ProviderDriverError>>
        });
        let changed_snapshot = Arc::clone(&snapshot);
        let auth = sign_in::controller(
            input.instance_id.clone(),
            settings.agent_program().to_owned(),
            enabled,
            Arc::new(move || {
                let snapshot = Arc::clone(&changed_snapshot);
                Box::pin(async move {
                    snapshot.refresh().await;
                }) as BoxFuture<'static, ()>
            }),
        );
        Ok(ProviderInstance {
            instance_id: input.instance_id.clone(),
            driver_kind: self.kind.clone(),
            continuation_identity: default_provider_continuation_identity(
                &self.kind,
                &input.instance_id,
            ),
            display_name: input
                .display_name
                .as_ref()
                .map(|name| name.as_str().to_owned()),
            accent_color: input
                .accent_color
                .as_ref()
                .map(|color| color.as_str().to_owned()),
            enabled: input.enabled,
            snapshot,
            snapshot_for_cwd: None,
            refresh_models: Some(refresh_models),
            invalidate_caches: None,
            consume_reset_credit: None,
            orchestration_adapter: Arc::new(adapter),
            text_generation: Arc::new(CursorText),
            auth: Some(Arc::new(auth)),
        })
    }
}

/// The published Cursor snapshot for one instance. Refresh probes the agent
/// and publishes the result to `get_snapshot` and `stream_changes`.
struct CursorSnapshot {
    driver: ProviderDriverKind,
    instance_id: ProviderInstanceId,
    settings: CursorSettings,
    enabled: bool,
    program: String,
    maintenance: ProviderMaintenanceCapabilities,
    current: watch::Sender<ServerProvider>,
    /// The instance's environment, the limit read's credentials (`CURSOR_AUTH_TOKEN`).
    environment: ProcessEnv,
    usage_limits: Option<Arc<dyn CursorUsageLimitsSource>>,
    prober: ProbeStatus,
}

impl CursorSnapshot {
    fn new(
        driver: ProviderDriverKind,
        instance_id: ProviderInstanceId,
        settings: CursorSettings,
        enabled: bool,
    ) -> Self {
        let initial = ServerProvider {
            instance_id: instance_id.clone(),
            driver: driver.clone(),
            draft: status::initial_draft(
                &driver,
                &settings,
                enabled,
                UtcDateTime::now().to_string(),
            ),
        };
        let (current, _) = watch::channel(initial);
        Self {
            program: settings.agent_program().to_owned(),
            maintenance: make_manual_only_provider_maintenance_capabilities(driver.clone()),
            driver,
            instance_id,
            settings,
            enabled,
            current,
            environment: ProcessEnv::new(),
            usage_limits: None,
            prober: spawn_probe(),
        }
    }

    async fn probe(&self) -> ServerProvider {
        let checked_at = UtcDateTime::now().to_string();
        let mut draft = if self.enabled {
            let probe = (self.prober)(self.program.clone()).await;
            // Upstream warns when the catalog comes back empty for a signed-in account.
            let warning = (probe.models.is_empty()
                && probe.message.is_none()
                && probe.auth.status == ServerProviderAuthStatus::Authenticated)
                .then(|| "Cursor model discovery returned no built-in models.".to_owned());
            status::draft_from_probe(
                &self.driver,
                self.enabled,
                &self.settings,
                checked_at,
                probe,
                warning,
            )
        } else {
            status::initial_draft(&self.driver, &self.settings, false, checked_at)
        };
        // Upstream `checkProvider`: plan limits only for an enabled, installed,
        // signed-in instance. A failed read is reported inside the limits, never
        // as a failed refresh.
        if let Some(source) = &self.usage_limits
            && self.enabled
            && draft.installed
            && draft.auth.status == ServerProviderAuthStatus::Authenticated
        {
            draft.usage_limits = Some(
                source
                    .read(self.settings.api_endpoint.as_deref(), &self.environment)
                    .await,
            );
        }
        let provider = ServerProvider {
            instance_id: self.instance_id.clone(),
            driver: self.driver.clone(),
            draft,
        };
        self.current.send_replace(provider.clone());
        provider
    }
}

#[async_trait]
impl ManagedServerProvider for CursorSnapshot {
    async fn resolve_maintenance(&self, _fresh: bool) -> ProviderMaintenanceCapabilities {
        self.maintenance.clone()
    }

    async fn get_snapshot(&self) -> ServerProvider {
        self.current.borrow().clone()
    }

    async fn refresh(&self) -> ServerProvider {
        self.probe().await
    }

    fn stream_changes(&self) -> BoxStream<'static, ServerProvider> {
        let receiver = self.current.subscribe();
        Box::pin(stream::unfold(receiver, |mut receiver| async move {
            receiver.changed().await.ok()?;
            let provider = receiver.borrow_and_update().clone();
            Some((provider, receiver))
        }))
    }

    /// Cursor's plan limit windows come from its dashboard API during the status probe
    /// ([`CursorSnapshot::probe`]); the provider runtime reports none of its own.
    async fn apply_usage_limits(&self, _update: ProviderUsageLimitsUpdate, _checked_at: &str) {}
}

struct CursorText;

fn text_error() -> TextGenerationError {
    TextGenerationError::TextGenerationError {
        operation: "cursor".to_owned(),
        detail: "Cursor ACP does not generate commit messages or titles.".to_owned(),
        cause: None,
    }
}

#[async_trait]
impl ProviderTextGeneration for CursorText {
    async fn generate_commit_message(
        &self,
        _input: CommitMessageGenerationInput,
    ) -> Result<CommitMessageGenerationResult, TextGenerationError> {
        Err(text_error())
    }

    async fn generate_pr_content(
        &self,
        _input: PrContentGenerationInput,
    ) -> Result<PrContentGenerationResult, TextGenerationError> {
        Err(text_error())
    }

    async fn generate_branch_name(
        &self,
        _input: BranchNameGenerationInput,
    ) -> Result<BranchNameGenerationResult, TextGenerationError> {
        Err(text_error())
    }

    async fn generate_thread_title(
        &self,
        _input: ThreadTitleGenerationInput,
    ) -> Result<ThreadTitleGenerationResult, TextGenerationError> {
        Err(text_error())
    }

    async fn generate_review_guide(
        &self,
        _input: ReviewGuideGenerationInput,
    ) -> Result<ReviewGuideGenerationResult, TextGenerationError> {
        Err(text_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use serde_json::json;
    use turin_contracts::provider_instance::{
        ProviderInstanceConfig, ProviderInstanceConfigMap, ProviderInstanceId,
    };
    use turin_provider_core::provider_instance_registry::ProviderInstanceRegistry;
    use turin_provider_core::server_provider::ServerProviderState;

    fn entry(config: Value) -> ProviderInstanceConfig {
        ProviderInstanceConfig {
            driver: ProviderDriverKind::new("cursor").unwrap(),
            display_name: None,
            accent_color: None,
            environment: None,
            enabled: Some(true),
            config: Some(config),
        }
    }

    /// Records what the probe asks it to read, and answers one window.
    struct RecordingLimits {
        reads: std::sync::Mutex<Vec<(Option<String>, ProcessEnv)>>,
    }

    #[async_trait]
    impl CursorUsageLimitsSource for RecordingLimits {
        async fn read(
            &self,
            api_endpoint: Option<&str>,
            environment: &ProcessEnv,
        ) -> turin_contracts::provider_usage_limits::ServerProviderUsageLimits {
            self.reads
                .lock()
                .expect("reads")
                .push((api_endpoint.map(str::to_owned), environment.clone()));
            turin_provider_core::usage_limits::make_usage_limits(
                "2026-01-01T00:00:00.000Z".to_owned(),
                vec![turin_contracts::provider_usage_limits::ServerProviderUsageWindow {
                    id: turin_contracts::TrimmedNonEmptyString::new("totalPercentUsed").unwrap(),
                    kind: turin_contracts::provider_usage_limits::ServerProviderUsageWindowKind::Monthly,
                    label: turin_contracts::TrimmedNonEmptyString::new("Total").unwrap(),
                    used_percent: 42.0,
                    resets_at: None,
                    window_duration_mins: None,
                }],
            )
        }
    }

    fn fake_probe(authenticated: bool, installed: bool) -> ProbeStatus {
        Arc::new(move |_program| {
            Box::pin(async move {
                CursorProbe {
                    installed,
                    version: Some("2026.01.01".to_owned()),
                    status: ServerProviderState::Ready,
                    auth: turin_provider_core::server_provider::ServerProviderAuth::with_status(
                        if authenticated {
                            ServerProviderAuthStatus::Authenticated
                        } else {
                            ServerProviderAuthStatus::Unauthenticated
                        },
                    ),
                    message: None,
                    models: Vec::new(),
                }
            })
        })
    }

    async fn probed_instance(
        driver: CursorDriver,
        config: Value,
    ) -> Arc<turin_provider_core::driver::ProviderInstance> {
        let id = ProviderInstanceId::new("cursor").unwrap();
        let mut map = ProviderInstanceConfigMap::default();
        map.insert(id.clone(), entry(config));
        let registry =
            ProviderInstanceRegistry::new(vec![Arc::new(driver) as Arc<dyn ProviderDriver>], map)
                .await;
        registry.get_instance(&id).unwrap()
    }

    #[tokio::test]
    async fn a_signed_in_probe_publishes_the_plan_limit_windows() {
        let limits = Arc::new(RecordingLimits {
            reads: Default::default(),
        });
        let instance = probed_instance(
            CursorDriver::with_test_seams(limits.clone(), fake_probe(true, true)),
            json!({"enabled": true, "apiEndpoint": "https://api.example.test"}),
        )
        .await;

        let refreshed = instance.snapshot.refresh().await;

        let published = refreshed.draft.usage_limits.expect("limits are published");
        assert_eq!(format!("{published:?}").contains("totalPercentUsed"), true);
        let reads = limits.reads.lock().unwrap();
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].0.as_deref(), Some("https://api.example.test"));
        assert_eq!(
            instance.snapshot.get_snapshot().await.draft.usage_limits,
            Some(published)
        );
    }

    #[tokio::test]
    async fn no_limits_are_read_without_a_signed_in_installed_agent() {
        for (authenticated, installed) in [(false, true), (true, false), (false, false)] {
            let limits = Arc::new(RecordingLimits {
                reads: Default::default(),
            });
            let instance = probed_instance(
                CursorDriver::with_test_seams(limits.clone(), fake_probe(authenticated, installed)),
                json!({"enabled": true}),
            )
            .await;

            let refreshed = instance.snapshot.refresh().await;

            assert!(
                refreshed.draft.usage_limits.is_none(),
                "authenticated={authenticated} installed={installed}"
            );
            assert!(limits.reads.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn a_disabled_instance_reads_no_limits() {
        let limits = Arc::new(RecordingLimits {
            reads: Default::default(),
        });
        let instance = probed_instance(
            CursorDriver::with_test_seams(limits.clone(), fake_probe(true, true)),
            json!({"enabled": false}),
        )
        .await;

        let refreshed = instance.snapshot.refresh().await;

        assert!(refreshed.draft.usage_limits.is_none());
        assert!(limits.reads.lock().unwrap().is_empty());
    }

    #[test]
    fn a_driver_without_a_host_keeps_no_usage_history() {
        assert!(CursorDriver::new().usage().is_none());
    }

    #[tokio::test]
    async fn registry_builds_cursor_instances_and_shadows_bad_config() {
        let good = ProviderInstanceId::new("cursor").unwrap();
        let bad = ProviderInstanceId::new("cursor-bad").unwrap();
        let mut map = ProviderInstanceConfigMap::default();
        map.insert(good.clone(), entry(json!({"enabled": true})));
        map.insert(bad.clone(), entry(json!({"binaryPath": null})));
        let registry = ProviderInstanceRegistry::new(
            vec![Arc::new(CursorDriver::new()) as Arc<dyn ProviderDriver>],
            map,
        )
        .await;

        let instance = registry.get_instance(&good).unwrap();
        assert_eq!(instance.driver_kind.as_str(), "cursor");
        let adapter = &instance.orchestration_adapter;
        assert_eq!(adapter.instance_id(), &good);
        let capabilities = adapter.get_capabilities().await.unwrap();
        assert!(!capabilities.threads.can_fork_thread);
        assert!(!capabilities.turns.supports_active_steering);
        assert_eq!(
            instance
                .snapshot
                .get_snapshot()
                .await
                .draft
                .supports_text_generation,
            Some(false)
        );

        assert!(registry.get_instance(&bad).is_none());
        let shadows = registry.list_unavailable();
        assert_eq!(shadows.len(), 1);
        assert_eq!(shadows[0].instance_id, bad);
    }

    #[tokio::test]
    async fn refresh_probes_a_missing_agent_and_publishes_it() {
        let id = ProviderInstanceId::new("cursor").unwrap();
        let mut map = ProviderInstanceConfigMap::default();
        map.insert(
            id.clone(),
            entry(json!({
                "enabled": true,
                "binaryPath": "/nonexistent/turin-cursor-agent-for-tests"
            })),
        );
        let registry = ProviderInstanceRegistry::new(
            vec![Arc::new(CursorDriver::new()) as Arc<dyn ProviderDriver>],
            map,
        )
        .await;
        let instance = registry.get_instance(&id).unwrap();
        let before = instance.snapshot.get_snapshot().await;
        assert!(before.draft.installed);
        assert_eq!(before.draft.status, ServerProviderState::Warning);

        let mut changes = instance.snapshot.stream_changes();
        let refreshed = instance.snapshot.refresh().await;
        assert!(!refreshed.draft.installed);
        assert_eq!(refreshed.draft.status, ServerProviderState::Error);
        assert!(
            refreshed
                .draft
                .message
                .as_ref()
                .is_some_and(|message| message
                    .as_str()
                    .starts_with("Cursor agent could not start"))
        );
        let published = changes.next().await.expect("refresh is published");
        assert_eq!(published.draft.status, ServerProviderState::Error);
        assert_eq!(
            instance.snapshot.get_snapshot().await.draft.status,
            ServerProviderState::Error
        );
    }

    #[tokio::test]
    async fn disabled_instances_never_probe_and_cannot_sign_in() {
        let id = ProviderInstanceId::new("cursor").unwrap();
        let mut map = ProviderInstanceConfigMap::default();
        map.insert(id.clone(), entry(json!({"enabled": false})));
        let registry = ProviderInstanceRegistry::new(
            vec![Arc::new(CursorDriver::new()) as Arc<dyn ProviderDriver>],
            map,
        )
        .await;
        let instance = registry.get_instance(&id).unwrap();
        let refreshed = instance.snapshot.refresh().await;
        assert!(!refreshed.draft.installed);
        assert_eq!(refreshed.draft.status, ServerProviderState::Disabled);
        let auth = instance
            .auth
            .as_ref()
            .expect("cursor has a sign-in controller");
        let error = (auth.start)("t3".to_owned(), None, None, None, None)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "Enable Cursor before signing in.");
    }
}
