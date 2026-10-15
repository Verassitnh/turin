//! Port of upstream/apps/server/src/usage/UsageLimitSources.ts.
//!
//! Quota from places this environment cannot run turns on, today a CLIProxyAPI
//! hub pooling several subscription accounts.
//!
//! Each enabled `usageLimitSources` entry is polled on the provider health-check
//! interval and on every settings change, then published as one snapshot per
//! source. A source that fails keeps its row with `error` set so the user can see
//! it is configured but unreachable. Nothing is persisted: like provider status,
//! this is live state that re-derives on boot.
//!
//! Deviations (see PORTING.md, N4): settings arrive as a `watch` receiver from the
//! composition root instead of `ServerSettingsService`. The background-policy gate
//! (`BackgroundPolicy`, cut with background leases) is not consulted, and the
//! interval is the fixed provider-health default rather than the resolved
//! background-activity setting.

use chrono::Utc;
use futures::StreamExt;
use indexmap::IndexMap;
use std::sync::Arc;
use tokio::sync::{Mutex as AsyncMutex, watch};
use tokio::task::JoinHandle;
use turin_contracts::base_schemas::{TrimmedNonEmptyString, js_trim};
use turin_contracts::provider_usage_limits::{
    CliproxySourceKind, ProviderConsumeResetCreditResult, UsageLimitSourceConsumeResetCreditInput,
    UsageLimitSourceError, UsageLimitSourceSnapshot,
};
use turin_contracts::settings::{
    DEFAULT_PROVIDER_HEALTH_REFRESH_INTERVAL, ServerSettings, UsageLimitSourceConfig,
};
use turin_contracts::usage_limit_source_id::UsageLimitSourceId;

use crate::cliproxy_api::{CliproxyApi, UsageLimitMappers};

const SOURCE_READ_CONCURRENCY: usize = 4;

/// Aborts its task when the last service clone drops, as `forkScoped` does with its layer.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// State shared by the service handles and its background task.
struct Shared {
    api: CliproxyApi,
    settings: watch::Receiver<ServerSettings>,
    /// The current snapshots; subscribers see the current set, then every change.
    snapshots: watch::Sender<Vec<UsageLimitSourceSnapshot>>,
    /// One refresh at a time: a slow hub read started before a settings change must
    /// not publish after the change's own refresh and resurrect a removed source.
    refresh_lock: AsyncMutex<()>,
    /// Completed refresh calls, for tests that count ticks and startup reads.
    #[cfg(test)]
    refreshes: std::sync::atomic::AtomicUsize,
}

/// Cheap to clone; the background poll stops when the last clone drops.
#[derive(Clone)]
pub struct UsageLimitSources {
    shared: Arc<Shared>,
    _background: Arc<AbortOnDrop>,
}

fn now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// The display label: the configured label, else the hub's host, else a fallback.
fn source_label(id: &UsageLimitSourceId, config: &UsageLimitSourceConfig) -> TrimmedNonEmptyString {
    let label = match &config.label {
        Some(label) => label.as_str().to_owned(),
        None => {
            let trimmed_id = js_trim(id.as_str());
            let fallback = if trimmed_id.is_empty() {
                "Usage limit source"
            } else {
                trimmed_id
            };
            host_of(config.url.as_str()).unwrap_or_else(|| fallback.to_owned())
        }
    };
    TrimmedNonEmptyString::new(&label).unwrap_or_else(|_| {
        TrimmedNonEmptyString::new("Usage limit source").expect("fallback is non-empty")
    })
}

/// `new URL(url).host`: hostname, plus the port when it is not the scheme default.
fn host_of(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    if host.is_empty() {
        return None;
    }
    Some(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

fn source_error(detail: impl Into<String>) -> UsageLimitSourceError {
    UsageLimitSourceError::SourceError {
        detail: detail.into(),
    }
}

impl Shared {
    fn current(&self) -> Vec<UsageLimitSourceSnapshot> {
        self.snapshots.borrow().clone()
    }

    /// Re-reads every enabled source now. Never fails; failures land on the snapshot.
    pub async fn refresh(&self) {
        #[cfg(test)]
        self.refreshes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _guard = self.refresh_lock.lock().await;
        let settings = self.settings.borrow().clone();
        let entries: Vec<(UsageLimitSourceId, UsageLimitSourceConfig)> = settings
            .usage_limit_sources
            .iter()
            .filter(|(_, config)| config.enabled)
            .map(|(id, config)| (id.clone(), config.clone()))
            .collect();
        let reads: Vec<_> = entries
            .iter()
            .map(|(id, config)| self.read_source(id, config))
            .collect();
        // Sources read four at a time; results keep the settings order.
        let snapshots: Vec<UsageLimitSourceSnapshot> = futures::stream::iter(reads)
            .buffered(SOURCE_READ_CONCURRENCY)
            .collect()
            .await;
        self.publish(snapshots);
    }

    /// Redeems a reset credit on one source, then re-reads that source. Shares the
    /// refresh lock so a stale in-flight read cannot overwrite the redemption.
    pub async fn consume_reset_credit(
        &self,
        input: &UsageLimitSourceConsumeResetCreditInput,
    ) -> Result<ProviderConsumeResetCreditResult, UsageLimitSourceError> {
        let settings = self.settings.borrow().clone();
        let config = match settings.usage_limit_sources.get(&input.source_id) {
            Some(config) if config.enabled && !config.management_key.as_str().is_empty() => {
                config.clone()
            }
            _ => {
                return Err(source_error(
                    "The usage limit source is missing or disabled.",
                ));
            }
        };
        let _guard = self.refresh_lock.lock().await;
        let result = self
            .api
            .consume(&config, input.account_id.as_str(), input.credit_id.as_str())
            .await?;
        let snapshot = self.read_source(&input.source_id, &config).await;
        let next = self
            .current()
            .into_iter()
            .map(|source| {
                if source.id == input.source_id {
                    snapshot.clone()
                } else {
                    source
                }
            })
            .collect();
        self.publish(next);
        Ok(result)
    }

    async fn read_source(
        &self,
        id: &UsageLimitSourceId,
        config: &UsageLimitSourceConfig,
    ) -> UsageLimitSourceSnapshot {
        let checked_at = now_iso();
        let base = |accounts, error: Option<&str>| UsageLimitSourceSnapshot {
            id: id.clone(),
            kind: CliproxySourceKind,
            label: source_label(id, config),
            checked_at: checked_at.clone(),
            accounts,
            error: error.map(|error| {
                TrimmedNonEmptyString::new(error).expect("error details are non-empty")
            }),
        };
        if config.management_key.as_str().is_empty() {
            return base(Vec::new(), Some("No management key configured."));
        }
        match self.api.read_accounts(config).await {
            Ok(accounts) => base(accounts, None),
            Err(UsageLimitSourceError::SourceError { detail }) => {
                tracing::debug!(source = id.as_str(), %detail, "usage limit source read failed");
                base(Vec::new(), Some(&detail))
            }
        }
    }

    fn publish(&self, next: Vec<UsageLimitSourceSnapshot>) {
        self.snapshots.send_if_modified(|current| {
            if *current == next {
                false
            } else {
                *current = next;
                true
            }
        });
    }
}

impl UsageLimitSources {
    /// Starts the service. An initial refresh runs straight away; later refreshes
    /// follow the settings and the health-check interval.
    pub fn start(
        settings: watch::Receiver<ServerSettings>,
        mappers: Arc<dyn UsageLimitMappers>,
    ) -> Self {
        let (snapshots, _) = watch::channel(Vec::new());
        let shared = Arc::new(Shared {
            api: CliproxyApi::new(mappers),
            settings: settings.clone(),
            snapshots,
            refresh_lock: AsyncMutex::new(()),
            #[cfg(test)]
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let background = tokio::spawn(run_background(shared.clone(), settings));
        Self {
            shared,
            _background: Arc::new(AbortOnDrop(background)),
        }
    }

    /// The current snapshots, one per enabled source.
    pub fn current(&self) -> Vec<UsageLimitSourceSnapshot> {
        self.shared.current()
    }

    /// Subscribes to snapshot changes. Read `borrow()` for the current set, then
    /// `changed()` for each later one.
    pub fn subscribe(&self) -> watch::Receiver<Vec<UsageLimitSourceSnapshot>> {
        self.shared.snapshots.subscribe()
    }

    /// Re-reads every enabled source now. Never fails; failures land on the snapshot.
    pub async fn refresh(&self) {
        self.shared.refresh().await;
    }

    /// Redeems a reset credit on one source, then re-reads that source.
    pub async fn consume_reset_credit(
        &self,
        input: &UsageLimitSourceConsumeResetCreditInput,
    ) -> Result<ProviderConsumeResetCreditResult, UsageLimitSourceError> {
        self.shared.consume_reset_credit(input).await
    }
}

/// Refreshes once at startup, on each settings change to the sources, and on each
/// health-check tick. The tick is its own timer, as upstream runs the interval in a
/// fiber of its own: a settings edit neither restarts it, nor stops it when the
/// settings stream ends.
async fn run_background(service: Arc<Shared>, mut settings: watch::Receiver<ServerSettings>) {
    service.refresh().await;
    let mut last_sources: IndexMap<UsageLimitSourceId, UsageLimitSourceConfig> =
        settings.borrow().usage_limit_sources.clone();
    let tick = tokio::time::sleep(DEFAULT_PROVIDER_HEALTH_REFRESH_INTERVAL);
    tokio::pin!(tick);
    let mut settings_open = true;
    loop {
        tokio::select! {
            () = &mut tick => {
                service.refresh().await;
                // Re-armed only after the refresh, as upstream sleeps, then refreshes.
                tick.as_mut().reset(tokio::time::Instant::now() + DEFAULT_PROVIDER_HEALTH_REFRESH_INTERVAL);
            }
            changed = settings.changed(), if settings_open => {
                if changed.is_err() {
                    // The sender is gone: stop listening, keep the health timer.
                    settings_open = false;
                    continue;
                }
                let next = settings.borrow().usage_limit_sources.clone();
                // Settings edits re-read straight away so a new hub shows up without
                // waiting for the interval, and a removed one leaves the list.
                if next != last_sources {
                    last_sources = next;
                    service.refresh().await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cliproxy_api::{ClaudeLimitsInput, CodexLimitsInput};
    use serde_json::json;
    use std::time::Duration;
    use turin_contracts::provider_usage_limits::ServerProviderUsageLimits;
    use turin_provider_core::usage_limits::make_usage_limits;

    struct StubMappers;

    impl UsageLimitMappers for StubMappers {
        fn codex_plan_label(&self, plan_type: Option<&str>) -> String {
            plan_type.unwrap_or("Codex").to_owned()
        }

        fn codex_limits(&self, input: CodexLimitsInput) -> ServerProviderUsageLimits {
            make_usage_limits(input.checked_at, Vec::new())
        }

        fn claude_limits(&self, input: ClaudeLimitsInput) -> ServerProviderUsageLimits {
            make_usage_limits(input.checked_at, Vec::new())
        }
    }

    fn settings(sources: serde_json::Value) -> ServerSettings {
        serde_json::from_value(json!({ "usageLimitSources": sources })).expect("settings decode")
    }

    fn cliproxy(url: &str, label: Option<&str>) -> serde_json::Value {
        let mut source =
            json!({ "kind": "cliproxy", "url": url, "managementKey": "", "enabled": true });
        if let Some(label) = label {
            source["label"] = json!(label);
        }
        source
    }

    #[tokio::test]
    async fn keeps_malformed_source_urls_encodable_for_config_subscribers() {
        let (_sender, receiver) = watch::channel(settings(json!({
            "local-hub": cliproxy("localhost:8317", None),
            "opaque-hub": cliproxy("file:///tmp/hub", None),
            "named-hub": cliproxy("localhost:8317", Some("Named hub")),
            "valid-hub": cliproxy("https://hub.test:8317", None),
            " ": cliproxy("localhost:8317", None),
        })));
        let sources = UsageLimitSources::start(receiver, Arc::new(StubMappers));
        sources.refresh().await;
        let snapshots = sources.current();

        let labels: Vec<&str> = snapshots
            .iter()
            .map(|source| source.label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "local-hub",
                "opaque-hub",
                "Named hub",
                "hub.test:8317",
                "Usage limit source"
            ]
        );
        assert!(serde_json::to_string(&snapshots).is_ok());
        assert!(snapshots.iter().all(|source| {
            source.error.as_ref().map(|error| error.as_str())
                == Some("No management key configured.")
        }));
    }

    #[tokio::test]
    async fn refuses_a_reset_credit_for_a_missing_or_disabled_source() {
        let (_sender, receiver) = watch::channel(settings(json!({
            "off": { "kind": "cliproxy", "url": "https://hub.test:8317", "managementKey": "k", "enabled": false },
        })));
        let sources = UsageLimitSources::start(receiver, Arc::new(StubMappers));
        for source_id in ["off", "absent"] {
            let input = UsageLimitSourceConsumeResetCreditInput {
                source_id: UsageLimitSourceId::new(source_id),
                account_id: TrimmedNonEmptyString::new("acct").unwrap(),
                credit_id: TrimmedNonEmptyString::new("credit").unwrap(),
            };
            let error = sources.consume_reset_credit(&input).await.unwrap_err();
            assert_eq!(
                error,
                UsageLimitSourceError::SourceError {
                    detail: "The usage limit source is missing or disabled.".to_owned()
                }
            );
        }
    }

    /// Settings with one enabled source and no management key, so refreshes read nothing.
    fn keyless_settings(unrelated_price: f64) -> ServerSettings {
        serde_json::from_value(json!({
            "usageLimitSources": {
                "hub": { "kind": "cliproxy", "url": "https://hub.test:8317", "managementKey": "", "enabled": true },
            },
            "usagePriceOverrides": {
                "example-model": { "inputCostPerMillionTokens": unrelated_price, "outputCostPerMillionTokens": 1 },
            },
        }))
        .expect("settings decode")
    }

    /// Lets the background task run until it waits on a timer or a channel.
    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    fn refreshes(sources: &UsageLimitSources) -> usize {
        sources
            .shared
            .refreshes
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    #[tokio::test(start_paused = true)]
    async fn refreshes_once_at_startup() {
        let (_sender, receiver) = watch::channel(keyless_settings(1.0));
        let sources = UsageLimitSources::start(receiver, Arc::new(StubMappers));
        settle().await;
        assert_eq!(refreshes(&sources), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_settings_edit_that_leaves_the_sources_alone_does_not_restart_the_health_timer() {
        let (sender, receiver) = watch::channel(keyless_settings(1.0));
        let sources = UsageLimitSources::start(receiver, Arc::new(StubMappers));
        settle().await;
        assert_eq!(refreshes(&sources), 1);

        tokio::time::advance(Duration::from_secs(4 * 60)).await;
        sender.send_replace(keyless_settings(2.0));
        settle().await;
        // The edit did not touch the sources, so it does not refresh.
        assert_eq!(refreshes(&sources), 1);

        // The tick still lands at five minutes from startup, not from the edit.
        tokio::time::advance(Duration::from_secs(60) + Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(refreshes(&sources), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_health_timer_keeps_running_after_the_settings_sender_closes() {
        let (sender, receiver) = watch::channel(keyless_settings(1.0));
        let sources = UsageLimitSources::start(receiver, Arc::new(StubMappers));
        settle().await;
        drop(sender);
        settle().await;
        assert_eq!(refreshes(&sources), 1);

        tokio::time::advance(DEFAULT_PROVIDER_HEALTH_REFRESH_INTERVAL + Duration::from_millis(1))
            .await;
        settle().await;
        assert_eq!(refreshes(&sources), 2);
    }
}
