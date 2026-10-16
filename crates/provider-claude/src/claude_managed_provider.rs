//! Per-instance Claude snapshot state, the `ManagedServerProvider` the driver
//! publishes. Upstream builds this with `makeManagedServerProvider` and the
//! driver's `checkProvider`. Here it is a plain struct: one published snapshot,
//! a watch channel for `stream_changes`, a refresh lock, and the probes the
//! status check reads through resolvers.
//!
//! Process-wide services (the remote model manifest, the latest-version cache,
//! and the reset-credit coordinator) live in [`ClaudeSharedServices`]. The
//! driver owns one and every instance it creates shares it.
use crate::{
    claude_capabilities_probe::{ClaudeCapabilitiesProbe, probe_claude_capabilities},
    claude_home::{ProcessEnv, resolve_claude_home_path},
    claude_model_catalog::{ClaudeModelCatalog, resolve_claude_model_catalog},
    claude_model_manifest::resolve_claude_provider_catalog,
    claude_provider::{
        CapabilitiesResolver, ClaudeStatusInput, ResetCreditsResolver,
        check_claude_provider_status, make_pending_claude_provider,
        probe_claude_workspace_snapshot,
    },
    claude_reset_credit_coordinator::ResetCreditCoordinator,
    claude_reset_credits::{
        ClaudeResetCreditError, ClaudeResetCreditsEndpoint, ConsumeClaudeResetCreditInput,
        claude_account_config_path, consume_claude_reset_credit,
        is_settled_claude_reset_credit_failure, read_claude_organization_id,
        read_claude_reset_credits,
    },
    claude_usage_limits::SharedClaudeScopedLimitNames,
};
use chrono::Utc;
use parking_lot::Mutex;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex as AsyncMutex, watch};
use tokio_stream::wrappers::WatchStream;
use turin_contracts::{
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
    provider_usage_limits::{
        ProviderConsumeResetCreditOutcome, ProviderUsageLimitsUpdate,
        ServerProviderUsageUnavailableReason,
    },
    settings::ClaudeSettings,
};
use turin_provider_core::{
    driver::driver_error,
    errors::ProviderDriverError,
    host_process::Platform,
    maintenance::ProviderMaintenanceCapabilities,
    model_manifest::{ModelManifest, UpdateChecks},
    package_managed_maintenance::{
        CachedMaintenanceResolution, NativeUpdate, PackageManagedProviderMaintenanceDefinition,
        enrich_provider_snapshot_with_version_advisory, normalize_command_path,
    },
    provider_host::ProviderHost,
    provider_latest_versions::ProviderLatestVersions,
    rt::{BoxFuture, BoxStream},
    server_provider::{ProviderWorkspaceSnapshot, ServerProvider, ServerProviderDraft},
    usage_limits::apply_usage_limits_update,
};

/// `CAPABILITIES_PROBE_TTL`: five minutes.
pub const CAPABILITIES_PROBE_TTL: Duration = Duration::from_secs(5 * 60);

/// The npm package the CLI is published under, for the latest-version lookup.
pub const CLAUDE_NPM_PACKAGE: &str = "@anthropic-ai/claude-code";

/// `ClaudeDriver.ts` `UPDATE`: the package-managed maintenance definition.
/// `claude update` runs first for paths the native installer owns.
pub fn claude_maintenance_definition() -> PackageManagedProviderMaintenanceDefinition {
    PackageManagedProviderMaintenanceDefinition {
        provider: ProviderDriverKind::new("claudeAgent").expect("valid Claude driver"),
        npm_package_name: CLAUDE_NPM_PACKAGE.to_owned(),
        native_update: Some(NativeUpdate {
            args: vec!["update".to_owned()],
            is_command_path: Some(is_claude_native_command_path),
            env: None,
        }),
    }
}

/// `isClaudeNativeCommandPath`: the native installer's own locations.
fn is_claude_native_command_path(command_path: &str) -> bool {
    let normalized = normalize_command_path(command_path);
    normalized.ends_with("/.local/bin/claude")
        || normalized.ends_with("/.local/bin/claude.exe")
        || normalized.contains("/.local/share/claude/")
}

/// `ProviderHost`-backed answer to `enableProviderUpdateChecks`. A settings
/// read that fails reads as checks off, so no fetch goes out.
pub fn provider_update_checks(host: Arc<ProviderHost>) -> UpdateChecks {
    Arc::new(move || {
        let host = host.clone();
        Box::pin(async move {
            host.get_settings()
                .await
                .map(|settings| settings.enable_provider_update_checks)
                .unwrap_or(false)
        }) as BoxFuture<'static, bool>
    })
}

/// Process-wide Claude services, shared by every instance the driver creates.
pub struct ClaudeSharedServices {
    pub host: Arc<ProviderHost>,
    pub model_manifest: Arc<ModelManifest>,
    pub latest_versions: Arc<ProviderLatestVersions>,
    pub reset_credits: Arc<ResetCreditCoordinator>,
}

impl ClaudeSharedServices {
    /// The services with the bundled manifest's remote refresh, cached under
    /// the host's state directory.
    pub fn new(host: Arc<ProviderHost>) -> Self {
        let checks = provider_update_checks(host.clone());
        let manifest = ModelManifest::new(PathBuf::from(&host.paths.state_dir), checks);
        Self::with_model_manifest(host, manifest)
    }

    /// Same, with a caller-built manifest service (tests point it at a local URL).
    pub fn with_model_manifest(host: Arc<ProviderHost>, manifest: ModelManifest) -> Self {
        Self {
            host,
            model_manifest: Arc::new(manifest),
            latest_versions: Arc::new(ProviderLatestVersions::new([])),
            reset_credits: Arc::new(ResetCreditCoordinator::new()),
        }
    }
}

struct CachedCapabilities {
    key: String,
    probed_at: Instant,
    value: Option<ClaudeCapabilitiesProbe>,
}

/// Everything one instance's probes read. Shared between the snapshot, the
/// resolvers, and the reset-credit flow, so every path sees the same login.
pub struct ClaudeInstanceProbes {
    pub instance_id: ProviderInstanceId,
    pub settings: ClaudeSettings,
    pub environment: ProcessEnv,
    pub home: String,
    pub scoped_names: SharedClaudeScopedLimitNames,
    pub endpoint: ClaudeResetCreditsEndpoint,
    pub http: reqwest::Client,
    pub services: Arc<ClaudeSharedServices>,
    /// Per-instance: the binary path decides who owns the install.
    pub maintenance: CachedMaintenanceResolution,
    /// Serializes account probes per instance, so concurrent checks share one result.
    capabilities: AsyncMutex<Option<CachedCapabilities>>,
}

impl ClaudeInstanceProbes {
    pub fn new(
        instance_id: ProviderInstanceId,
        settings: ClaudeSettings,
        environment: ProcessEnv,
        home: String,
        scoped_names: SharedClaudeScopedLimitNames,
        services: Arc<ClaudeSharedServices>,
    ) -> Self {
        let maintenance = CachedMaintenanceResolution::new(
            claude_maintenance_definition(),
            Some(settings.binary_path.as_str().to_owned()),
            Some(environment.clone()),
            Platform::current(),
        );
        Self {
            instance_id,
            settings,
            environment,
            home,
            scoped_names,
            endpoint: ClaudeResetCreditsEndpoint::production(),
            http: reqwest::Client::new(),
            services,
            maintenance,
            capabilities: AsyncMutex::new(None),
        }
    }

    fn config_dir(&self) -> PathBuf {
        resolve_claude_home_path(
            self.settings.home_path.as_str(),
            Some(&self.environment),
            &self.home,
        )
    }

    fn cache_key(&self) -> String {
        crate::claude_home::make_claude_capabilities_cache_key(
            self.settings.binary_path.as_str(),
            self.settings.home_path.as_str(),
            None,
            Some(&self.environment),
            &self.home,
        )
    }

    /// The account probe, cached for [`CAPABILITIES_PROBE_TTL`] per binary, home, and cwd.
    pub async fn capabilities(&self) -> Option<ClaudeCapabilitiesProbe> {
        let key = self.cache_key();
        let mut cache = self.capabilities.lock().await;
        if let Some(cached) = cache.as_ref()
            && cached.key == key
            && cached.probed_at.elapsed() < CAPABILITIES_PROBE_TTL
        {
            return cached.value.clone();
        }
        let value = match probe_claude_capabilities(
            &self.settings,
            &self.environment,
            None,
            true,
            &self.home,
        )
        .await
        {
            Some(mut probe) => {
                probe.workspace_id = read_claude_organization_id(&claude_account_config_path(
                    self.config_dir_option().as_deref(),
                    &self.home,
                ))
                .await;
                Some(probe)
            }
            None => None,
        };
        *cache = Some(CachedCapabilities {
            key,
            probed_at: Instant::now(),
            value: value.clone(),
        });
        value
    }

    /// The config directory only when the instance sets one, matching upstream's
    /// `homePath.trim() || CLAUDE_CONFIG_DIR` choice of account file.
    fn config_dir_option(&self) -> Option<PathBuf> {
        let explicit = !self.settings.home_path.as_str().trim().is_empty()
            || self
                .environment
                .get("CLAUDE_CONFIG_DIR")
                .is_some_and(|value| !value.trim().is_empty());
        explicit.then(|| self.config_dir())
    }

    pub async fn invalidate_capabilities(&self) {
        *self.capabilities.lock().await = None;
    }

    /// Banked resets for the login, read with the CLI version as the user agent.
    pub async fn reset_credits(
        &self,
        version: String,
    ) -> Option<turin_contracts::provider_usage_limits::ServerProviderResetCredits> {
        read_claude_reset_credits(
            &self.http,
            &self.endpoint,
            &self.config_dir(),
            &version,
            Platform::current(),
            Utc::now().timestamp_millis(),
        )
        .await
    }

    /// The status check's capability resolver, bound to this instance.
    pub fn capabilities_resolver(self: &Arc<Self>) -> CapabilitiesResolver {
        let probes = self.clone();
        Arc::new(move || {
            let probes = probes.clone();
            Box::pin(async move { probes.capabilities().await })
        })
    }

    pub fn reset_credits_resolver(self: &Arc<Self>) -> ResetCreditsResolver {
        let probes = self.clone();
        Arc::new(move |version: String| {
            let probes = probes.clone();
            Box::pin(async move { probes.reset_credits(version).await })
        })
    }
}

/// The managed snapshot for one Claude instance.
pub struct ClaudeManagedProvider {
    probes: Arc<ClaudeInstanceProbes>,
    driver: ProviderDriverKind,
    current: Mutex<ServerProvider>,
    changes: watch::Sender<ServerProvider>,
    refresh_lock: AsyncMutex<()>,
}

impl ClaudeManagedProvider {
    pub fn new(
        probes: Arc<ClaudeInstanceProbes>,
        driver: ProviderDriverKind,
        catalog: &ClaudeModelCatalog,
    ) -> Self {
        let pending = stamp(
            &probes.instance_id,
            &driver,
            make_pending_claude_provider(&probes.settings, catalog),
        );
        let (changes, _) = watch::channel(pending.clone());
        Self {
            probes,
            driver,
            current: Mutex::new(pending),
            changes,
            refresh_lock: AsyncMutex::new(()),
        }
    }

    fn publish(&self, next: ServerProvider) {
        *self.current.lock() = next.clone();
        let _ = self.changes.send(next);
    }

    pub fn get_snapshot(&self) -> ServerProvider {
        self.current.lock().clone()
    }

    /// `refresh`: the status check, then the version advisory, then publish.
    /// Starts the TTL-gated manifest refresh first, so the check reads whatever
    /// manifest is current. A fetch that lands mid-check shows on the next one.
    pub async fn refresh(&self) -> ServerProvider {
        let _guard = self.refresh_lock.lock().await;
        let probes = &self.probes;
        let services = &probes.services;
        services.model_manifest.refresh_in_background();
        let manifest = services.model_manifest.current().await;
        let catalog =
            resolve_claude_model_catalog(resolve_claude_provider_catalog(&manifest).as_ref());

        let draft = check_claude_provider_status(ClaudeStatusInput {
            settings: &probes.settings,
            environment: &probes.environment,
            home: &probes.home,
            cwd: None,
            catalog: &catalog,
            scoped_limit_names: Some(&probes.scoped_names),
            resolve_capabilities: Some(probes.capabilities_resolver()),
            resolve_reset_credits: Some(probes.reset_credits_resolver()),
        })
        .await;
        let snapshot = stamp(&probes.instance_id, &self.driver, draft);
        let capabilities = probes.maintenance.get(false).await;
        let enable_checks = services
            .host
            .get_settings()
            .await
            .ok()
            .map(|settings| settings.enable_provider_update_checks);
        let next = enrich_provider_snapshot_with_version_advisory(
            snapshot,
            &capabilities,
            enable_checks,
            &services.latest_versions,
            &probes.http,
        )
        .await;
        self.publish(next.clone());
        next
    }

    /// `stream_changes`: the current snapshot, then every published change.
    pub fn stream_changes(&self) -> BoxStream<'static, ServerProvider> {
        Box::pin(WatchStream::new(self.changes.subscribe()))
    }

    /// Folds a runtime rate-limit update into the published snapshot. Synchronous
    /// so the adapter's event path can call it without spawning.
    pub fn apply_usage_limits_sync(&self, update: &ProviderUsageLimitsUpdate, checked_at: &str) {
        let mut next = self.get_snapshot();
        let Some(limits) =
            apply_usage_limits_update(next.draft.usage_limits.as_ref(), update, checked_at)
        else {
            return;
        };
        next.draft.usage_limits = Some(limits);
        self.publish(next);
    }

    /// `ManagedServerProvider.applyUsageLimits`.
    pub async fn apply_usage_limits(&self, update: ProviderUsageLimitsUpdate, checked_at: &str) {
        self.apply_usage_limits_sync(&update, checked_at);
    }

    pub async fn resolve_maintenance(&self, fresh: bool) -> ProviderMaintenanceCapabilities {
        self.probes.maintenance.get(fresh).await
    }

    /// `consumeResetCredit`: claims the banked reset the snapshot names through
    /// the driver's shared coordinator, then re-probes. Claims on one login
    /// queue across instances, and an unanswered claim keeps its request id.
    // `ProviderDriverError` is the shared driver error and the `ConsumeResetCredit`
    // closure returns it unboxed, so the lint is allowed here.
    #[allow(clippy::result_large_err)]
    pub async fn consume_reset_credit(
        &self,
    ) -> Result<ProviderConsumeResetCreditOutcome, ProviderDriverError> {
        let current = self.get_snapshot();
        let grant = current
            .draft
            .usage_limits
            .as_ref()
            .and_then(|limits| limits.reset_credits.as_ref())
            .and_then(|credits| credits.next_credit_id.clone());
        let Some(grant) = grant else {
            return Ok(ProviderConsumeResetCreditOutcome::NoCredit);
        };
        let Some(version) = current.draft.version.clone() else {
            return Ok(ProviderConsumeResetCreditOutcome::NoCredit);
        };
        let probes = &self.probes;
        let config_dir = probes.config_dir();
        let account_path =
            claude_account_config_path(probes.config_dir_option().as_deref(), &probes.home);
        let account_key = config_dir.to_string_lossy().into_owned();

        let outcome = probes
            .services
            .reset_credits
            .redeem(
                &account_key,
                |request_id| {
                    let http = probes.http.clone();
                    let endpoint = probes.endpoint.clone();
                    let version = version.as_str().to_owned();
                    let grant = grant.as_str().to_owned();
                    async move {
                        consume_claude_reset_credit(
                            &http,
                            &endpoint,
                            Platform::current(),
                            ConsumeClaudeResetCreditInput {
                                config_dir: &config_dir,
                                account_config_path: &account_path,
                                version: &version,
                                grant_id: &grant,
                                request_id: &request_id,
                            },
                        )
                        .await
                    }
                },
                is_settled_claude_reset_credit_failure,
            )
            .await
            .map_err(|error| claim_error(&self.probes.instance_id, &self.driver, error))?;

        // Re-probe after any answer. Only a reset claims the limits changed, so
        // only a reset reports an unconfirmed refresh.
        let before = current
            .draft
            .usage_limits
            .as_ref()
            .map(|limits| limits.checked_at.clone());
        probes.invalidate_capabilities().await;
        let refreshed = self.refresh().await;
        let after = refreshed
            .draft
            .usage_limits
            .as_ref()
            .map(|limits| limits.checked_at.clone());
        let probe_failed = refreshed
            .draft
            .usage_limits
            .as_ref()
            .and_then(|limits| limits.unavailable.as_ref())
            .is_some_and(|unavailable| {
                unavailable.reason == ServerProviderUsageUnavailableReason::ProbeFailed
            });
        if matches!(outcome, ProviderConsumeResetCreditOutcome::Reset)
            && (after.is_none() || after == before || probe_failed)
        {
            return Err(driver_error(
                &self.driver,
                &self.probes.instance_id,
                "The reset was applied, but Claude could not confirm the new limits. Refresh to check.",
            ));
        }
        Ok(outcome)
    }

    /// `snapshotForCwd`: the machine snapshot with this workspace's commands and skills.
    pub async fn snapshot_for_cwd(&self, cwd: &str) -> ProviderWorkspaceSnapshot {
        let probes = &self.probes;
        probe_claude_workspace_snapshot(
            &probes.settings,
            self.get_snapshot(),
            Path::new(cwd),
            &probes.environment,
            &probes.home,
        )
        .await
    }
}

fn stamp(
    instance_id: &ProviderInstanceId,
    driver: &ProviderDriverKind,
    draft: ServerProviderDraft,
) -> ServerProvider {
    ServerProvider {
        instance_id: instance_id.clone(),
        driver: driver.clone(),
        draft,
    }
}

fn claim_error(
    instance_id: &ProviderInstanceId,
    driver: &ProviderDriverKind,
    error: ClaudeResetCreditError,
) -> ProviderDriverError {
    driver_error(driver, instance_id, error.to_string())
}

#[cfg(test)]
mod tests;
