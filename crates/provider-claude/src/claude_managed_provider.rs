//! Per-instance Claude snapshot state, the `ManagedServerProvider` the driver
//! publishes. Upstream builds this with `makeManagedServerProvider` and the
//! driver's `checkProvider`. Here it is a plain struct: one published snapshot,
//! a watch channel for `stream_changes`, a refresh lock, and the capability and
//! reset-credit probes the status check reads through resolvers.
use crate::{
    claude_capabilities_probe::{ClaudeCapabilitiesProbe, probe_claude_capabilities},
    claude_home::{ProcessEnv, resolve_claude_home_path},
    claude_model_catalog::ClaudeModelCatalog,
    claude_provider::{
        CapabilitiesResolver, ClaudeStatusInput, ResetCreditsResolver,
        check_claude_provider_status, make_pending_claude_provider,
        probe_claude_workspace_snapshot,
    },
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
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex as AsyncMutex, watch};
use tokio_stream::wrappers::WatchStream;
use turin_contracts::{
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
    provider_usage_limits::{ProviderConsumeResetCreditOutcome, ProviderUsageLimitsUpdate},
    settings::ClaudeSettings,
};
use turin_provider_core::{
    driver::driver_error,
    errors::ProviderDriverError,
    host_process::Platform,
    maintenance::ProviderMaintenanceCapabilities,
    maintenance_resolver::make_manual_only_provider_maintenance_capabilities,
    rt::BoxStream,
    server_provider::{ProviderWorkspaceSnapshot, ServerProvider, ServerProviderDraft},
    usage_limits::apply_usage_limits_update,
};

/// `CAPABILITIES_PROBE_TTL`: five minutes.
pub const CAPABILITIES_PROBE_TTL: Duration = Duration::from_secs(5 * 60);

struct CachedCapabilities {
    key: String,
    probed_at: Instant,
    value: Option<ClaudeCapabilitiesProbe>,
}

/// Everything the instance's probes read. Shared between the snapshot, the
/// resolvers, and the reset-credit flow, so every path sees the same login.
pub struct ClaudeInstanceProbes {
    pub instance_id: ProviderInstanceId,
    pub settings: ClaudeSettings,
    pub environment: ProcessEnv,
    pub home: String,
    pub catalog: Arc<ClaudeModelCatalog>,
    pub scoped_names: SharedClaudeScopedLimitNames,
    pub endpoint: ClaudeResetCreditsEndpoint,
    pub http: reqwest::Client,
    /// Serializes account probes per instance, so concurrent checks share one result.
    capabilities: AsyncMutex<Option<CachedCapabilities>>,
    /// Serializes reset claims, as upstream's `ResetCreditCoordinator` does per config dir.
    reset_claims: AsyncMutex<()>,
    /// The request id of an unanswered claim, kept so a retry asks about the same claim.
    pending_claim: Mutex<Option<String>>,
}

impl ClaudeInstanceProbes {
    pub fn new(
        instance_id: ProviderInstanceId,
        settings: ClaudeSettings,
        environment: ProcessEnv,
        home: String,
        catalog: Arc<ClaudeModelCatalog>,
        scoped_names: SharedClaudeScopedLimitNames,
    ) -> Self {
        Self {
            instance_id,
            settings,
            environment,
            home,
            catalog,
            scoped_names,
            endpoint: ClaudeResetCreditsEndpoint::production(),
            http: reqwest::Client::new(),
            capabilities: AsyncMutex::new(None),
            reset_claims: AsyncMutex::new(()),
            pending_claim: Mutex::new(None),
        }
    }

    fn config_dir(&self) -> std::path::PathBuf {
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
    fn config_dir_option(&self) -> Option<std::path::PathBuf> {
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
    pub fn new(probes: Arc<ClaudeInstanceProbes>, driver: ProviderDriverKind) -> Self {
        let pending = stamp(
            &probes.instance_id,
            &driver,
            make_pending_claude_provider(&probes.settings, &probes.catalog),
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

    /// `refresh`: runs the status check and publishes the result.
    pub async fn refresh(&self) -> ServerProvider {
        let _guard = self.refresh_lock.lock().await;
        let probes = &self.probes;
        let draft = check_claude_provider_status(ClaudeStatusInput {
            settings: &probes.settings,
            environment: &probes.environment,
            home: &probes.home,
            cwd: None,
            catalog: &probes.catalog,
            scoped_limit_names: Some(&probes.scoped_names),
            resolve_capabilities: Some(probes.capabilities_resolver()),
            resolve_reset_credits: Some(probes.reset_credits_resolver()),
        })
        .await;
        let next = stamp(&probes.instance_id, &self.driver, draft);
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

    pub async fn resolve_maintenance(&self, _fresh: bool) -> ProviderMaintenanceCapabilities {
        // Update commands are not wired for Claude yet (see PORTING.md).
        make_manual_only_provider_maintenance_capabilities(self.driver.clone())
    }

    /// `consumeResetCredit`: claims the banked reset the snapshot names, then
    /// re-probes. Claims are serialized per instance. The request id survives
    /// an unanswered claim so the retry asks about the same claim.
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
        let _claim = probes.reset_claims.lock().await;
        let request_id = probes
            .pending_claim
            .lock()
            .get_or_insert_with(|| uuid::Uuid::new_v4().simple().to_string())
            .clone();

        let claim = consume_claude_reset_credit(
            &probes.http,
            &probes.endpoint,
            Platform::current(),
            ConsumeClaudeResetCreditInput {
                config_dir: &config_dir,
                account_config_path: &account_path,
                version: version.as_str(),
                grant_id: grant.as_str(),
                request_id: &request_id,
            },
        )
        .await;
        match &claim {
            Ok(_) => *probes.pending_claim.lock() = None,
            Err(error) if is_settled_claude_reset_credit_failure(error) => {
                *probes.pending_claim.lock() = None;
            }
            Err(_) => {}
        }
        let outcome =
            claim.map_err(|error| claim_error(&self.probes.instance_id, &self.driver, error))?;

        // Re-probe after any answer. Only a reset claims the limits changed, so
        // only a reset reports an unconfirmed refresh.
        let before = current
            .draft
            .usage_limits
            .as_ref()
            .map(|l| l.checked_at.clone());
        probes.invalidate_capabilities().await;
        let refreshed = self.refresh().await;
        let after = refreshed.draft.usage_limits.as_ref();
        if matches!(outcome, ProviderConsumeResetCreditOutcome::Reset)
            && (after.is_none()
                || after.map(|limits| &limits.checked_at) == before.as_ref()
                || after.is_some_and(|limits| limits.unavailable.as_ref().is_some_and(|u| {
                    u.reason == turin_contracts::provider_usage_limits::ServerProviderUsageUnavailableReason::ProbeFailed
                })))
        {
            return Err(driver_error(
                &self.driver,
                &self.probes.instance_id,
                "The reset was applied, but Claude could not confirm the new limits. Refresh to check.",
            ));
        }
        Ok(outcome)
    }

    /// `snapshotForCwd`: the machine snapshot with this workspace's commands.
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
