//! Port of upstream/packages/provider-core/src/server/managedProvider.ts.
//!
//! One managed snapshot per provider instance. It probes on settings change,
//! on the background health interval when something asks for provider status,
//! and on explicit refresh. A slow enrichment (binary resolution, package
//! state) publishes a second snapshot later, and only the newest enrichment is
//! kept.
//!
//! Upstream runs these as Effect fibers scoped to the layer. Here they are tokio
//! tasks, aborted when the [`ManagedServerProvider`] is dropped.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::StreamExt;
use parking_lot::Mutex;
use tokio::{
    sync::{Mutex as AsyncMutex, Notify, mpsc},
    task::JoinHandle,
};
use tokio_stream::wrappers::UnboundedReceiverStream;
use turin_contracts::{
    provider_usage_limits::ProviderUsageLimitsUpdate,
    settings::{DEFAULT_PROVIDER_HEALTH_REFRESH_INTERVAL, ServerSettingsError},
};
use turin_shared::server_settings::resolve_server_background_activity_settings;

use crate::{
    background_scope::BackgroundScope,
    maintenance::ProviderMaintenanceCapabilities,
    provider_host::ProviderHost,
    rt::{BoxFuture, BoxStream},
    server_provider::{ServerProvider, ServerProviderAuth},
    snapshot::ManagedServerProvider as ManagedServerProviderTrait,
    usage_limits::{apply_usage_limits_update, resolve_usage_limits_after_probe},
};

/// Used when the periodic check would otherwise spin: upstream's 60 s floor for a zero interval.
const ZERO_INTERVAL_FLOOR: Duration = Duration::from_secs(60);

type Shared<T> = Arc<dyn Fn() -> BoxFuture<'static, T> + Send + Sync>;

/// What an enrichment callback receives. `publish_snapshot` only lands while
/// this enrichment's generation is still the newest.
pub struct EnrichInput<S> {
    pub settings: S,
    pub snapshot: ServerProvider,
    pub get_snapshot: Shared<ServerProvider>,
    pub publish_snapshot: Arc<dyn Fn(ServerProvider) -> BoxFuture<'static, ()> + Send + Sync>,
}

pub type EnrichSnapshot<S> = Arc<dyn Fn(EnrichInput<S>) -> BoxFuture<'static, ()> + Send + Sync>;

/// Resolves maintenance capabilities; `true` is the click path (no cached resolution).
pub type ResolveMaintenance =
    Arc<dyn Fn(bool) -> BoxFuture<'static, ProviderMaintenanceCapabilities> + Send + Sync>;

/// Compares previous and next settings; `true` means a probe is needed.
pub type SettingsChanged<S> = Arc<dyn Fn(&S, &S) -> bool + Send + Sync>;

/// Reads the instance's settings, which can fail.
pub type GetSettings<S> =
    Arc<dyn Fn() -> BoxFuture<'static, Result<S, ServerSettingsError>> + Send + Sync>;

/// Runs the provider probe, which can fail on settings read.
pub type CheckProvider =
    Arc<dyn Fn() -> BoxFuture<'static, Result<ServerProvider, ServerSettingsError>> + Send + Sync>;

/// Everything a driver supplies to build its managed snapshot (upstream
/// `makeManagedServerProvider` input).
pub struct ManagedServerProviderInput<S> {
    pub resolve_maintenance: ResolveMaintenance,
    pub get_settings: GetSettings<S>,
    pub have_settings_changed: SettingsChanged<S>,
    pub initial_snapshot: Arc<dyn Fn(S) -> BoxFuture<'static, ServerProvider> + Send + Sync>,
    pub check_provider: CheckProvider,
    pub enrich_snapshot: Option<EnrichSnapshot<S>>,
    /// `None` reads `providerHealthRefreshInterval` from the host's server settings.
    pub refresh_interval: Option<Duration>,
    /// Upstream `refreshOnInterval !== false`.
    pub refresh_on_interval: bool,
    pub check_provider_on_settings_change: Option<SettingsChanged<S>>,
}

#[derive(Clone)]
struct ProviderSnapshotState {
    snapshot: ServerProvider,
    enrichment_generation: u64,
}

struct Inner<S> {
    input: ManagedServerProviderInput<S>,
    host: Arc<ProviderHost>,
    /// Upstream `refreshSemaphore` (one permit): probes never overlap.
    refresh_lock: AsyncMutex<()>,
    state: Mutex<ProviderSnapshotState>,
    settings: Mutex<S>,
    enrichment: Mutex<Option<JoinHandle<()>>>,
    subscribers: Mutex<Vec<mpsc::UnboundedSender<ServerProvider>>>,
    /// Coalesces interval changes into one wake-up, like upstream's `Queue.sliding(1)`.
    interval_changed: Notify,
}

impl<S: Clone + Send + Sync + 'static> Inner<S> {
    fn publish(&self, snapshot: ServerProvider) {
        self.subscribers
            .lock()
            .retain(|subscriber| subscriber.send(snapshot.clone()).is_ok());
    }

    async fn publish_enriched(self: &Arc<Self>, generation: u64, next: ServerProvider) {
        let to_publish = {
            let mut state = self.state.lock();
            if state.enrichment_generation != generation {
                None
            } else {
                // Enrichment derives from the snapshot it was handed; a runtime
                // usage update that landed since must not be reverted by it.
                let mut merged = next;
                merged.usage_limits = state.snapshot.usage_limits.clone();
                if merged == state.snapshot {
                    None
                } else {
                    state.snapshot = merged.clone();
                    Some(merged)
                }
            }
        };
        if let Some(snapshot) = to_publish {
            self.publish(snapshot);
        }
    }

    async fn restart_snapshot_enrichment(
        self: &Arc<Self>,
        settings: S,
        snapshot: ServerProvider,
        generation: u64,
    ) {
        let previous = self.enrichment.lock().take();
        if let Some(previous) = previous {
            previous.abort();
        }
        // Enrichment resolves binaries and package-manager state, which a
        // disabled provider has no use for.
        let Some(enrich) = self.input.enrich_snapshot.clone() else {
            return;
        };
        if !snapshot.enabled {
            return;
        }
        let get_inner = Arc::clone(self);
        let publish_inner = Arc::clone(self);
        let input = EnrichInput {
            settings,
            snapshot,
            get_snapshot: Arc::new(move || {
                let inner = Arc::clone(&get_inner);
                Box::pin(async move { inner.state.lock().snapshot.clone() })
            }),
            publish_snapshot: Arc::new(move |next| {
                let inner = Arc::clone(&publish_inner);
                Box::pin(async move { inner.publish_enriched(generation, next).await })
            }),
        };
        let handle = tokio::spawn(async move {
            enrich(input).await;
        });
        *self.enrichment.lock() = Some(handle);
    }

    /// Upstream `applySnapshot`. Callers hold `refresh_lock`.
    // ServerSettingsError is built only on settings-read failure paths; boxing it would thread `Box::new` through every `?`.
    #[allow(clippy::result_large_err)]
    async fn apply_snapshot_base(
        self: &Arc<Self>,
        next_settings: S,
        force_refresh: bool,
    ) -> Result<ServerProvider, ServerSettingsError> {
        let previous_settings = self.settings.lock().clone();
        if !force_refresh && !(self.input.have_settings_changed)(&previous_settings, &next_settings)
        {
            *self.settings.lock() = next_settings;
            return Ok(self.state.lock().snapshot.clone());
        }

        if !force_refresh
            && self
                .input
                .check_provider_on_settings_change
                .as_ref()
                .is_some_and(|check| !check(&previous_settings, &next_settings))
        {
            let (snapshot, generation) = {
                let mut state = self.state.lock();
                state.enrichment_generation += 1;
                (state.snapshot.clone(), state.enrichment_generation)
            };
            *self.settings.lock() = next_settings.clone();
            self.restart_snapshot_enrichment(next_settings, snapshot.clone(), generation)
                .await;
            return Ok(snapshot);
        }

        let probed = (self.input.check_provider)().await?;
        let (next_snapshot, generation) = {
            let mut state = self.state.lock();
            let generation = if self.input.enrich_snapshot.is_some() {
                state.enrichment_generation + 1
            } else {
                state.enrichment_generation
            };
            // A failed read keeps the last good limits only for the same account,
            // and they keep the workspace they were read for.
            let previous = state.snapshot.auth.clone();
            let lowercase_email = |auth: &ServerProviderAuth| {
                auth.email
                    .as_ref()
                    .map(|email| email.as_str().to_lowercase())
            };
            let email_differs = match (lowercase_email(&probed.auth), lowercase_email(&previous)) {
                (Some(a), Some(b)) => a != b,
                _ => false,
            };
            // Limits read without a workspace cannot be attributed to a new one.
            let switched_account = email_differs
                || probed
                    .auth
                    .workspace_id
                    .as_ref()
                    .is_some_and(|workspace| Some(workspace) != previous.workspace_id.as_ref());
            let usage_limits = if switched_account {
                probed.usage_limits.clone()
            } else {
                resolve_usage_limits_after_probe(
                    state.snapshot.usage_limits.as_ref(),
                    probed.usage_limits.as_ref(),
                )
            };
            let carried_workspace = (usage_limits != probed.usage_limits)
                .then(|| state.snapshot.auth.workspace_id.clone())
                .flatten();
            let mut snapshot = probed;
            if let Some(workspace) = carried_workspace
                && snapshot.auth.workspace_id.is_none()
            {
                snapshot.auth.workspace_id = Some(workspace);
            }
            snapshot.usage_limits = usage_limits;
            state.snapshot = snapshot.clone();
            state.enrichment_generation = generation;
            (snapshot, generation)
        };
        *self.settings.lock() = next_settings.clone();
        self.publish(next_snapshot.clone());
        self.restart_snapshot_enrichment(next_settings, next_snapshot.clone(), generation)
            .await;
        Ok(next_snapshot)
    }

    // ServerSettingsError is built only on settings-read failure paths; boxing it would thread `Box::new` through every `?`.
    #[allow(clippy::result_large_err)]
    async fn apply_snapshot(
        self: &Arc<Self>,
        next_settings: S,
        force_refresh: bool,
    ) -> Result<ServerProvider, ServerSettingsError> {
        let _permit = self.refresh_lock.lock().await;
        self.apply_snapshot_base(next_settings, force_refresh).await
    }

    // ServerSettingsError is built only on settings-read failure paths; boxing it would thread `Box::new` through every `?`.
    #[allow(clippy::result_large_err)]
    async fn refresh(self: &Arc<Self>) -> Result<ServerProvider, ServerSettingsError> {
        let next_settings = (self.input.get_settings)().await?;
        self.apply_snapshot(next_settings, true).await
    }

    async fn has_provider_status_demand(&self) -> bool {
        let instance_id = self.state.lock().snapshot.instance_id.clone();
        let generic = self
            .host
            .should_run_background_work(BackgroundScope::ProviderStatus { instance_id: None })
            .await;
        let instance = self
            .host
            .should_run_background_work(BackgroundScope::ProviderStatus {
                instance_id: Some(instance_id),
            })
            .await;
        generic || instance
    }

    async fn refresh_interval(&self) -> Duration {
        if let Some(interval) = self.input.refresh_interval {
            return interval;
        }
        match self.host.get_settings().await {
            Ok(settings) => Duration::from_millis(
                resolve_server_background_activity_settings(&settings)
                    .provider_health_refresh_interval,
            ),
            Err(_) => DEFAULT_PROVIDER_HEALTH_REFRESH_INTERVAL,
        }
    }
}

/// One provider instance's managed snapshot. Dropping it stops its tasks.
pub struct ManagedServerProvider<S> {
    inner: Arc<Inner<S>>,
    tasks: Vec<JoinHandle<()>>,
}

impl<S: Clone + Send + Sync + 'static> ManagedServerProvider<S> {
    /// Upstream `makeManagedServerProvider`: reads the initial settings, builds
    /// the initial snapshot, and starts the settings and interval loops. The
    /// first probe is forced and runs in the background.
    // ServerSettingsError is built only on settings-read failure paths; boxing it would thread `Box::new` through every `?`.
    #[allow(clippy::result_large_err)]
    pub async fn start(
        input: ManagedServerProviderInput<S>,
        settings_stream: BoxStream<'static, S>,
        host: Arc<ProviderHost>,
    ) -> Result<Self, ServerSettingsError> {
        let initial_settings = (input.get_settings)().await?;
        let initial_snapshot = (input.initial_snapshot)(initial_settings.clone()).await;
        let interval_from_settings = input.refresh_interval.is_none();
        let inner = Arc::new(Inner {
            host: Arc::clone(&host),
            refresh_lock: AsyncMutex::new(()),
            state: Mutex::new(ProviderSnapshotState {
                snapshot: initial_snapshot,
                enrichment_generation: 0,
            }),
            settings: Mutex::new(initial_settings.clone()),
            enrichment: Mutex::new(None),
            subscribers: Mutex::new(Vec::new()),
            interval_changed: Notify::new(),
            input,
        });
        let mut tasks = Vec::new();

        if interval_from_settings {
            let inner = Arc::clone(&inner);
            let mut changes = host.subscribe_settings();
            tasks.push(tokio::spawn(async move {
                let mut last: Option<u64> = None;
                while let Some(settings) = changes.recv().await {
                    let millis = resolve_server_background_activity_settings(&settings)
                        .provider_health_refresh_interval;
                    if last != Some(millis) {
                        last = Some(millis);
                        inner.interval_changed.notify_one();
                    }
                }
            }));
        }

        {
            let inner = Arc::clone(&inner);
            tasks.push(tokio::spawn(async move {
                let mut settings_stream = settings_stream;
                while let Some(next) = settings_stream.next().await {
                    if let Err(error) = inner.apply_snapshot(next, false).await {
                        tracing::error!(?error, "provider settings check failed");
                    }
                }
            }));
        }

        tasks.push(tokio::spawn(interval_loop(Arc::clone(&inner))));

        let initial_inner = Arc::clone(&inner);
        tasks.push(tokio::spawn(async move {
            if let Err(error) = initial_inner.apply_snapshot(initial_settings, true).await {
                tracing::error!(?error, "initial provider check failed");
            }
        }));

        Ok(Self { inner, tasks })
    }
}

impl<S> Drop for ManagedServerProvider<S> {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        if let Some(enrichment) = self.inner.enrichment.lock().take() {
            enrichment.abort();
        }
    }
}

async fn interval_loop<S: Clone + Send + Sync + 'static>(inner: Arc<Inner<S>>) {
    loop {
        let interval = inner.refresh_interval().await;
        let zero = interval.is_zero();
        let sleep_for = if zero { ZERO_INTERVAL_FLOOR } else { interval };
        let interval_elapsed = tokio::select! {
            () = tokio::time::sleep(sleep_for) => true,
            () = inner.interval_changed.notified() => false,
        };
        if inner.input.refresh_on_interval
            && interval_elapsed
            && !zero
            && inner.has_provider_status_demand().await
            && let Err(error) = inner.refresh().await
        {
            tracing::error!(?error, "periodic provider refresh failed");
        }
    }
}

#[async_trait]
impl<S: Clone + Send + Sync + 'static> ManagedServerProviderTrait for ManagedServerProvider<S> {
    async fn resolve_maintenance(&self, fresh: bool) -> ProviderMaintenanceCapabilities {
        (self.inner.input.resolve_maintenance)(fresh).await
    }

    async fn get_snapshot(&self) -> ServerProvider {
        self.inner.state.lock().snapshot.clone()
    }

    /// Upstream `refresh`: a failed probe logs and keeps the last snapshot
    /// (upstream dies here; a panic would take the caller's task down).
    async fn refresh(&self) -> ServerProvider {
        match self.inner.refresh().await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::error!(?error, "provider refresh failed");
                self.inner.state.lock().snapshot.clone()
            }
        }
    }

    fn stream_changes(&self) -> BoxStream<'static, ServerProvider> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.inner.subscribers.lock().push(sender);
        Box::pin(UnboundedReceiverStream::new(receiver))
    }

    /// Runtime usage updates patch only `usageLimits` on the published snapshot
    /// and leave the enrichment generation alone, so an in-flight enrichment
    /// still lands.
    async fn apply_usage_limits(&self, update: ProviderUsageLimitsUpdate, checked_at: &str) {
        let to_publish = {
            let mut state = self.inner.state.lock();
            let usage_limits = apply_usage_limits_update(
                state.snapshot.usage_limits.as_ref(),
                &update,
                checked_at,
            );
            if usage_limits == state.snapshot.usage_limits {
                None
            } else {
                state.snapshot.usage_limits = usage_limits;
                Some(state.snapshot.clone())
            }
        };
        if let Some(snapshot) = to_publish {
            self.inner.publish(snapshot);
        }
    }
}
