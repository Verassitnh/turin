//! Port of upstream/apps/server/src/provider/ProviderRegistry.ts.
//!
//! Owns provider install/auth/version/model snapshots and exposes the latest
//! provider state to the app. It aggregates the per-instance snapshot streams
//! into one materialized list.
//!
//! Each configured instance (including multi-instance setups like
//! `codex_personal` + `codex_work`) contributes one [`ProviderSnapshotSource`],
//! keyed by `instance_id`. Instances whose driver is unavailable or whose
//! config failed to decode are merged from `list_unavailable` as shadow
//! snapshots so the UI can render their exact unavailable reason.
//!
//! Cache paths on disk are keyed by `instance_id`. Because
//! `default_instance_id_for_driver(kind) == kind` for built-in kinds, existing
//! `<kind>.json` files remain the on-disk location for that driver's default
//! instance. Identity-less legacy cache contents are ignored and replaced by
//! the first live refresh.
//!
//! Upstream builds this as an Effect `Layer` over `ProviderInstanceRegistry`,
//! `ModelManifest`, and `ServerConfig`. Here [`ProviderRegistry::start`] takes
//! them directly; the instance registry is passed as a [`ProviderInstanceSource`]
//! because upstream's tests substitute their own. Effect defects (`Effect.die`)
//! are panics, caught at the same boundaries upstream catches them: the refresh
//! entry points and the instance-change loop. The fibers upstream forks into
//! the layer scope are tokio tasks aborted when the registry is dropped.

use std::{
    collections::{HashMap, HashSet},
    panic::AssertUnwindSafe,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use futures::{FutureExt, StreamExt, future::join_all};
use indexmap::IndexMap;
use parking_lot::Mutex;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_stream::wrappers::UnboundedReceiverStream;
use turin_contracts::{
    TrimmedNonEmptyString,
    provider_instance::{ProviderDriverKind, ProviderInstanceId, default_instance_id_for_driver},
};
use turin_provider_core::{
    built_in_provider_catalog::ProviderSnapshotSource,
    driver::ProviderInstance,
    errors::ProviderDriverError,
    maintenance::ProviderMaintenanceCapabilities,
    maintenance_resolver::make_manual_only_provider_maintenance_capabilities,
    model_manifest::{ModelManifest, ModelManifestData, bundled_model_manifest},
    provider_compatibility::{
        ProviderCompatibilityPolicy, apply_provider_compatibility,
        decode_provider_compatibility_policies,
    },
    provider_instance_registry::ProviderInstanceRegistry,
    provider_status_cache::{
        hydrate_cached_provider, is_cached_provider_correlated, order_provider_snapshots,
        read_provider_status_cache, resolve_provider_status_cache_path,
        write_provider_status_cache,
    },
    rt::BoxStream,
    server_provider::{
        ProviderWorkspaceSnapshot, ServerProvider, ServerProviderAuthStatus, ServerProviderModel,
        ServerProviderState, ServerProviderUpdateState, ServerProviderUpdateStatus,
        ServerProviderWorkspaceSnapshot, is_provider_workspace_snapshot_current,
    },
};

/// Volatile maintenance actions the registry projects onto a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderMaintenanceActionKind {
    Update,
}

const MAX_WORKSPACE_SNAPSHOTS_PER_PROVIDER: usize = 16;

fn make_manual_provider_maintenance_capabilities(
    provider: ProviderDriverKind,
) -> ProviderMaintenanceCapabilities {
    make_manual_only_provider_maintenance_capabilities(provider)
}

fn has_model_capabilities(model: &ServerProviderModel) -> bool {
    model
        .capabilities
        .as_ref()
        .and_then(|capabilities| capabilities.option_descriptors.as_ref())
        .is_some_and(|descriptors| !descriptors.is_empty())
}

fn workspace_snapshot_of<'a>(
    provider: Option<&'a ServerProvider>,
    cwd: &str,
) -> Option<&'a ServerProviderWorkspaceSnapshot> {
    provider?
        .workspace_snapshots
        .as_ref()?
        .iter()
        .find(|snapshot| snapshot.cwd.as_str() == cwd)
}

fn drop_provider_workspace_snapshot(mut provider: ServerProvider, cwd: &str) -> ServerProvider {
    if let Some(snapshots) = provider.workspace_snapshots.as_mut()
        && snapshots
            .iter()
            .any(|snapshot| snapshot.cwd.as_str() == cwd)
    {
        snapshots.retain(|snapshot| snapshot.cwd.as_str() != cwd);
    }
    provider
}

/// Upstream `upsertProviderWorkspaceSnapshot`. A pending scan keeps the slash
/// commands already stored for the cwd, since its own list is only the
/// placeholder. A `cwd` that is blank cannot name a workspace, so the provider
/// comes back unchanged.
pub fn upsert_provider_workspace_snapshot(
    provider: &ServerProvider,
    cwd: &str,
    scoped_snapshot: &ProviderWorkspaceSnapshot,
) -> ServerProvider {
    let Ok(cwd_name) = TrimmedNonEmptyString::new(cwd) else {
        return provider.clone();
    };
    let pending = scoped_snapshot.slash_commands_pending == Some(true);
    let slash_commands = if pending {
        workspace_snapshot_of(Some(provider), cwd)
            .map(|snapshot| snapshot.slash_commands.clone())
            .unwrap_or_else(|| scoped_snapshot.provider.slash_commands.clone())
    } else {
        scoped_snapshot.provider.slash_commands.clone()
    };
    let workspace_snapshot = ServerProviderWorkspaceSnapshot {
        cwd: cwd_name,
        checked_at: scoped_snapshot.provider.checked_at.clone(),
        slash_commands,
        slash_commands_pending: pending.then_some(true),
        skills: scoped_snapshot.provider.skills.clone(),
    };
    let mut snapshots: Vec<ServerProviderWorkspaceSnapshot> = provider
        .workspace_snapshots
        .iter()
        .flatten()
        .filter(|snapshot| snapshot.cwd.as_str() != cwd)
        .cloned()
        .collect();
    snapshots.push(workspace_snapshot);
    let skip = snapshots
        .len()
        .saturating_sub(MAX_WORKSPACE_SNAPSHOTS_PER_PROVIDER);
    let mut next = provider.clone();
    next.workspace_snapshots = Some(snapshots.split_off(skip));
    next
}

/// Upstream `shouldRetainMissingProviderModels`, minus the ACP registry and
/// Antigravity branches (both providers are cut). Codex and OpenCode replace
/// their inventory after a successful discovery; every other driver keeps what
/// it knew.
fn should_retain_missing_provider_models(provider: &ServerProvider) -> bool {
    let is_codex = provider.driver.as_str() == "codex";
    if !is_codex && provider.driver.as_str() != "opencode" {
        return true;
    }

    if is_codex
        && (!provider.enabled || provider.auth.status == ServerProviderAuthStatus::Unauthenticated)
    {
        return false;
    }

    // Successful discovery replaces these inventories so cached retired models disappear.
    let is_pending_initial_probe =
        provider.enabled && !provider.installed && provider.status == ServerProviderState::Warning;
    let did_installed_provider_probe_fail =
        provider.installed && provider.status == ServerProviderState::Error;
    is_pending_initial_probe || did_installed_provider_probe_fail
}

fn should_retain_missing_open_code_metadata(provider: &ServerProvider) -> bool {
    provider.driver.as_str() == "opencode" && should_retain_missing_provider_models(provider)
}

fn merge_provider_models(
    provider: &ServerProvider,
    previous_models: &[ServerProviderModel],
    next_models: &[ServerProviderModel],
) -> Vec<ServerProviderModel> {
    let should_retain_missing_models = should_retain_missing_provider_models(provider);
    // Custom rows are derived from settings and every snapshot carries the full
    // current list, so a custom model missing from `next_models` was removed by
    // the user and must not be resurrected from the previous snapshot.
    // A model the installed CLI is too old to run was offered by the pending
    // snapshot, before the version was known; retaining it would make it
    // selectable again.
    let update_required_slugs: HashSet<&str> = provider
        .update_required_models
        .iter()
        .flatten()
        .map(|model| model.slug.as_str())
        .collect();
    let retainable_previous_models: Vec<&ServerProviderModel> = previous_models
        .iter()
        .filter(|model| !model.is_custom && !update_required_slugs.contains(model.slug.as_str()))
        .collect();

    if should_retain_missing_models
        && next_models.is_empty()
        && !retainable_previous_models.is_empty()
    {
        return retainable_previous_models.into_iter().cloned().collect();
    }

    let previous_by_slug: HashMap<&str, &ServerProviderModel> = previous_models
        .iter()
        .map(|model| (model.slug.as_str(), model))
        .collect();
    let mut merged_models: Vec<ServerProviderModel> = next_models
        .iter()
        .map(|model| match previous_by_slug.get(model.slug.as_str()) {
            Some(previous_model)
                if !has_model_capabilities(model) && has_model_capabilities(previous_model) =>
            {
                let mut model = model.clone();
                model.capabilities = previous_model.capabilities.clone();
                model
            }
            _ => model.clone(),
        })
        .collect();
    if should_retain_missing_models {
        let next_slugs: HashSet<&str> = next_models
            .iter()
            .map(|model| model.slug.as_str())
            .collect();
        merged_models.extend(
            retainable_previous_models
                .into_iter()
                .filter(|model| !next_slugs.contains(model.slug.as_str()))
                .cloned(),
        );
    }
    merged_models
}

/// Upstream `mergeProviderSnapshot`, minus the Antigravity saved-account
/// carry-over (Antigravity is cut).
pub fn merge_provider_snapshot(
    previous_provider: Option<&ServerProvider>,
    next_provider: &ServerProvider,
) -> ServerProvider {
    let Some(previous_provider) = previous_provider else {
        return next_provider.clone();
    };
    let mut merged = next_provider.clone();
    merged.models = merge_provider_models(
        next_provider,
        &previous_provider.models,
        &next_provider.models,
    );
    if next_provider.workspace_snapshots.is_none()
        && previous_provider.workspace_snapshots.is_some()
    {
        merged.workspace_snapshots = previous_provider.workspace_snapshots.clone();
    }
    if should_retain_missing_open_code_metadata(next_provider) {
        if next_provider.slash_commands.is_empty() {
            merged.slash_commands = previous_provider.slash_commands.clone();
        }
        if next_provider.skills.is_empty() {
            merged.skills = previous_provider.skills.clone();
        }
    }
    merged
}

pub fn merge_provider_snapshots(
    previous_providers: &[ServerProvider],
    next_providers: &[ServerProvider],
) -> Vec<ServerProvider> {
    let mut merged_providers: IndexMap<ProviderInstanceId, ServerProvider> = previous_providers
        .iter()
        .map(|provider| (snapshot_instance_key(provider), provider.clone()))
        .collect();

    for provider in next_providers {
        let key = snapshot_instance_key(provider);
        let merged = merge_provider_snapshot(merged_providers.get(&key), provider);
        merged_providers.insert(key, merged);
    }

    order_provider_snapshots(&merged_providers.into_values().collect::<Vec<_>>())
}

pub fn select_providers_by_kind(
    providers: &[ServerProvider],
    provider_kinds: &HashSet<ProviderDriverKind>,
) -> Vec<ServerProvider> {
    providers
        .iter()
        .filter(|provider| provider_kinds.contains(&provider.driver))
        .cloned()
        .collect()
}

/// Key a snapshot for aggregation and persistence. Snapshot sources must be
/// correlated by instance id before reaching a map; missing identities are
/// defects, not runtime routing fallbacks.
fn snapshot_instance_key(provider: &ServerProvider) -> ProviderInstanceId {
    provider.instance_id.clone()
}

/// A snapshot source emitting another instance's or driver's snapshot is a
/// defect (upstream `Effect.die`).
fn correlate_snapshot_with_source(
    source: &ProviderSnapshotSource,
    snapshot: ServerProvider,
) -> ServerProvider {
    correlate_snapshot(&source.instance_id, &source.driver_kind, snapshot)
}

/// [`correlate_snapshot_with_source`] for a subscription task, which keeps only
/// the ids so it never owns the managed provider.
fn correlate_snapshot(
    instance_id: &ProviderInstanceId,
    driver_kind: &ProviderDriverKind,
    snapshot: ServerProvider,
) -> ServerProvider {
    if &snapshot.instance_id != instance_id {
        panic!(
            "Provider snapshot instance mismatch: source '{}' emitted '{}'.",
            instance_id, snapshot.instance_id
        );
    }
    if &snapshot.driver != driver_kind {
        panic!(
            "Provider snapshot driver mismatch for instance '{}': source '{}' emitted '{}'.",
            instance_id, driver_kind, snapshot.driver
        );
    }
    snapshot
}

// Project a live `ProviderInstance` into the aggregator's consumption shape.
// Each call re-captures the instance's `snapshot`, so after the instance
// registry rebuilds an instance (e.g. because its settings changed), a fresh
// source rides the new stream instead of a closed one.
fn build_snapshot_source(instance: &ProviderInstance) -> ProviderSnapshotSource {
    ProviderSnapshotSource::new(
        instance.instance_id.clone(),
        instance.driver_kind.clone(),
        Arc::clone(&instance.snapshot),
    )
}

/// The slice of upstream's `ProviderInstanceRegistry` service this registry
/// reads. [`ProviderInstanceRegistry`] is the live implementation; tests supply
/// their own, as upstream's do.
pub trait ProviderInstanceSource: Send + Sync {
    fn get_instance(&self, instance_id: &ProviderInstanceId) -> Option<Arc<ProviderInstance>>;
    fn list_instances(&self) -> Vec<Arc<ProviderInstance>>;
    fn list_unavailable(&self) -> Vec<ServerProvider>;
    /// Registers the subscription before returning, so a publish after this
    /// call cannot be missed.
    fn subscribe_changes(&self) -> mpsc::UnboundedReceiver<()>;
}

impl ProviderInstanceSource for ProviderInstanceRegistry {
    fn get_instance(&self, instance_id: &ProviderInstanceId) -> Option<Arc<ProviderInstance>> {
        ProviderInstanceRegistry::get_instance(self, instance_id)
    }

    fn list_instances(&self) -> Vec<Arc<ProviderInstance>> {
        ProviderInstanceRegistry::list_instances(self)
    }

    fn list_unavailable(&self) -> Vec<ServerProvider> {
        ProviderInstanceRegistry::list_unavailable(self)
    }

    fn subscribe_changes(&self) -> mpsc::UnboundedReceiver<()> {
        ProviderInstanceRegistry::subscribe_changes(self)
    }
}

/// Epoch milliseconds. Injected so tests can move time (upstream `TestClock`).
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

fn system_clock() -> Clock {
    Arc::new(|| chrono::Utc::now().timestamp_millis())
}

fn format_iso(epoch_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(epoch_ms)
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

pub struct ProviderRegistryOptions {
    /// Upstream `config.providerStatusCacheDir`.
    pub provider_status_cache_dir: PathBuf,
    /// The running app's version, matched against compatibility policies.
    pub t3_code_version: String,
    /// `None` reads the system clock.
    pub clock: Option<Clock>,
}

/// Tasks the registry owns. Upstream forks them into the layer scope; here
/// dropping the registry aborts them.
struct TaskSet {
    handles: Mutex<Vec<JoinHandle<()>>>,
    closed: AtomicBool,
}

impl TaskSet {
    fn new() -> Self {
        Self {
            handles: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
        }
    }

    fn spawn(&self, task: impl std::future::Future<Output = ()> + Send + 'static) {
        let mut handles = self.handles.lock();
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        handles.retain(|handle| !handle.is_finished());
        handles.push(tokio::spawn(task));
    }

    fn abort_all(&self) {
        let mut handles = self.handles.lock();
        self.closed.store(true, Ordering::SeqCst);
        for handle in handles.drain(..) {
            handle.abort();
        }
    }
}

#[derive(Clone)]
struct UpsertOptions {
    publish: bool,
    persist: bool,
    replace: bool,
    /// The emitting source's liveness flag. A snapshot from a source that has
    /// been retired (its instance was rebuilt or removed) is dropped, checked
    /// under the same lock as the merge so a late emission cannot overwrite the
    /// rebuilt instance or bring back a removed one.
    only_if_active: Option<Arc<AtomicBool>>,
}

impl Default for UpsertOptions {
    fn default() -> Self {
        Self {
            publish: true,
            persist: true,
            replace: false,
            only_if_active: None,
        }
    }
}

impl UpsertOptions {
    fn from_source(active: &Arc<AtomicBool>) -> Self {
        Self {
            only_if_active: Some(Arc::clone(active)),
            ..Self::default()
        }
    }

    fn source_retired(&self) -> bool {
        self.only_if_active
            .as_ref()
            .is_some_and(|active| !active.load(Ordering::SeqCst))
    }
}

/// Aborts its task when dropped.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// One live instance and the task forwarding its change stream. The task owns
/// only the stream receiver and the instance's ids, never the managed provider,
/// so the instance registry dropping the instance is what ends the provider.
/// `retire` (also run on drop) stops the task and marks late snapshots stale;
/// upstream gets the same from the instance scope closing.
struct LiveSub {
    instance: Arc<ProviderInstance>,
    active: Arc<AtomicBool>,
    task: Option<AbortOnDrop>,
}

impl LiveSub {
    fn retire(&mut self) {
        self.active.store(false, Ordering::SeqCst);
        self.task = None;
    }
}

impl Drop for LiveSub {
    fn drop(&mut self) {
        self.retire();
    }
}

type RefreshAllResult = Option<Result<Vec<ServerProvider>, ()>>;

struct Inner {
    source: Arc<dyn ProviderInstanceSource>,
    manifest: Arc<ModelManifest>,
    cache_dir: PathBuf,
    t3_code_version: String,
    clock: Clock,
    tasks: Arc<TaskSet>,
    bundled_compatibility: Option<Vec<ProviderCompatibilityPolicy>>,
    providers: Mutex<Vec<ServerProvider>>,
    /// Instances (by identity) with a workspace scan running, and the cwds.
    workspace_refreshes: Mutex<Vec<(Arc<ProviderInstance>, HashSet<String>)>>,
    /// Volatile; never persisted. Only update actions exist today.
    maintenance_update_states: Mutex<HashMap<ProviderInstanceId, ServerProviderUpdateState>>,
    /// Live-source registry: the dynamic counterpart to the boot-time sources.
    /// Keyed by `instance_id`; the stored instance is compared by identity so
    /// "no-op" reconciles (settings unchanged) skip re-subscribing and re-probing.
    live_subs: Mutex<IndexMap<ProviderInstanceId, LiveSub>>,
    /// Serializes `sync_live_sources` so a rapid burst of reconciles doesn't
    /// interleave two passes clobbering each other's bookkeeping.
    sync_lock: tokio::sync::Mutex<()>,
    compatibility_refresh_running: AtomicBool,
    refresh_all_in_flight: Mutex<Option<watch::Receiver<RefreshAllResult>>>,
    /// Aggregator fan-out: each subscriber gets every coalesced update.
    subscribers: Mutex<Vec<mpsc::UnboundedSender<Vec<ServerProvider>>>>,
}

/// A live source plus its liveness flag, for a refresh in flight when the
/// instance is retired.
struct LiveSource {
    source: ProviderSnapshotSource,
    active: Arc<AtomicBool>,
}

struct ResetFlagOnDrop(Arc<Inner>);

impl Drop for ResetFlagOnDrop {
    fn drop(&mut self) {
        self.0
            .compatibility_refresh_running
            .store(false, Ordering::SeqCst);
    }
}

struct ReleaseWorkspaceClaim<'a> {
    inner: &'a Inner,
    instance: Arc<ProviderInstance>,
    cwd: String,
}

impl Drop for ReleaseWorkspaceClaim<'_> {
    fn drop(&mut self) {
        let mut refreshes = self.inner.workspace_refreshes.lock();
        if let Some(position) = refreshes
            .iter()
            .position(|(candidate, _)| Arc::ptr_eq(candidate, &self.instance))
        {
            refreshes[position].1.remove(&self.cwd);
            if refreshes[position].1.is_empty() {
                refreshes.remove(position);
            }
        }
    }
}

impl Inner {
    fn publish(&self, providers: &[ServerProvider]) {
        self.subscribers
            .lock()
            .retain(|subscriber| subscriber.send(providers.to_vec()).is_ok());
    }

    fn classify_compatibility(
        &self,
        provider: ServerProvider,
        manifest_policies: Option<&[ProviderCompatibilityPolicy]>,
    ) -> ServerProvider {
        apply_provider_compatibility(
            provider,
            manifest_policies,
            self.bundled_compatibility.as_deref(),
            &self.t3_code_version,
        )
    }

    async fn manifest_policies(&self) -> Option<Vec<ProviderCompatibilityPolicy>> {
        let manifest: ModelManifestData = self.manifest.current().await;
        manifest
            .compatibility
            .as_deref()
            .map(decode_provider_compatibility_policies)
    }

    fn have_providers_changed(previous: &[ServerProvider], next: &[ServerProvider]) -> bool {
        previous != next
    }

    async fn persist_provider(&self, provider: &ServerProvider) {
        // Persist every instance — the file name is the instance id, so
        // multi-instance setups (e.g. `codex_personal`, `codex_work`) each get
        // their own cache. The path is resolved fresh so snapshots produced by
        // newly-added instances post-boot still land on disk.
        let file_path =
            resolve_provider_status_cache_path(&self.cache_dir, &snapshot_instance_key(provider));
        let mut machine_provider = provider.clone();
        machine_provider.workspace_snapshots = None;
        if let Err(error) = write_provider_status_cache(&file_path, &machine_provider).await {
            tracing::error!(?error, "failed to write provider status cache");
        }
    }

    fn apply_provider_update_state(&self, mut provider: ServerProvider) -> ServerProvider {
        provider.update_state = self
            .maintenance_update_states
            .lock()
            .get(&provider.instance_id)
            .cloned();
        provider
    }

    async fn upsert_providers(
        &self,
        next_providers: Vec<ServerProvider>,
        options: UpsertOptions,
    ) -> Vec<ServerProvider> {
        if options.source_retired() {
            return self.current_providers();
        }
        let manifest_policies = self.manifest_policies().await;
        let next_providers_with_update_state: Vec<ServerProvider> = next_providers
            .into_iter()
            .map(|provider| self.apply_provider_update_state(provider))
            .collect();
        let (previous_providers, providers, providers_to_persist) = {
            let mut current = self.providers.lock();
            if options.source_retired() {
                return current.clone();
            }
            let previous_providers = current.clone();
            let mut merged_providers: IndexMap<ProviderInstanceId, ServerProvider> =
                previous_providers
                    .iter()
                    .map(|provider| (snapshot_instance_key(provider), provider.clone()))
                    .collect();
            let mut updated_keys: HashSet<ProviderInstanceId> = HashSet::new();

            for provider in next_providers_with_update_state {
                let key = snapshot_instance_key(&provider);
                updated_keys.insert(key.clone());
                let value = if options.replace {
                    provider
                } else {
                    merge_provider_snapshot(merged_providers.get(&key), &provider)
                };
                merged_providers.insert(key, value);
            }

            let classified: Vec<ServerProvider> = merged_providers
                .into_values()
                .map(|provider| self.classify_compatibility(provider, manifest_policies.as_deref()))
                .collect();
            let providers = order_provider_snapshots(&classified);
            let providers_to_persist: Vec<ServerProvider> = providers
                .iter()
                .filter(|provider| updated_keys.contains(&snapshot_instance_key(provider)))
                .cloned()
                .collect();
            *current = providers.clone();
            (previous_providers, providers, providers_to_persist)
        };

        if Self::have_providers_changed(&previous_providers, &providers) {
            if options.persist {
                join_all(
                    providers_to_persist
                        .iter()
                        .map(|provider| self.persist_provider(provider)),
                )
                .await;
            }
            if options.publish {
                self.publish(&providers);
            }
        }

        providers
    }

    /// Replace the list under the lock and publish when it changed.
    fn update_providers(
        &self,
        update: impl FnOnce(&[ServerProvider]) -> Vec<ServerProvider>,
    ) -> Vec<ServerProvider> {
        let (previous_providers, next_providers) = {
            let mut current = self.providers.lock();
            let previous_providers = current.clone();
            let next_providers = update(&previous_providers);
            *current = next_providers.clone();
            (previous_providers, next_providers)
        };
        if Self::have_providers_changed(&previous_providers, &next_providers) {
            self.publish(&next_providers);
        }
        next_providers
    }

    fn current_providers(&self) -> Vec<ServerProvider> {
        self.providers.lock().clone()
    }

    async fn load_providers(sources: &[ProviderSnapshotSource]) -> Vec<ServerProvider> {
        join_all(sources.iter().map(|source| async move {
            let snapshot = source.get_snapshot().await;
            correlate_snapshot_with_source(source, snapshot)
        }))
        .await
    }

    async fn sync_provider(
        self: &Arc<Self>,
        provider: ServerProvider,
        options: UpsertOptions,
    ) -> Vec<ServerProvider> {
        if options.source_retired() {
            return self.current_providers();
        }
        let providers = self.upsert_providers(vec![provider], options).await;
        // Reclassify the current read model after fetching. Never republish the
        // probe captured before the fetch: a newer health result may have landed.
        if !self
            .compatibility_refresh_running
            .swap(true, Ordering::SeqCst)
        {
            let inner = Arc::clone(self);
            self.tasks.spawn(async move {
                let inner = ResetFlagOnDrop(inner);
                inner.0.manifest.refresh().await;
                inner
                    .0
                    .upsert_providers(
                        Vec::new(),
                        UpsertOptions {
                            persist: false,
                            ..UpsertOptions::default()
                        },
                    )
                    .await;
            });
        }
        providers
    }

    async fn set_provider_maintenance_action_state(
        self: &Arc<Self>,
        instance_id: &ProviderInstanceId,
        action: ProviderMaintenanceActionKind,
        state: Option<ServerProviderUpdateState>,
    ) -> Vec<ServerProvider> {
        let ProviderMaintenanceActionKind::Update = action;
        {
            let mut states = self.maintenance_update_states.lock();
            match state {
                Some(state) if state.status != ServerProviderUpdateStatus::Idle => {
                    states.insert(instance_id.clone(), state);
                }
                _ => {
                    states.remove(instance_id);
                }
            }
        }

        let existing_providers = self.current_providers();
        let Some(matching_provider) = existing_providers
            .iter()
            .find(|candidate| &candidate.instance_id == instance_id)
        else {
            return existing_providers;
        };

        let next_provider = self.apply_provider_update_state(matching_provider.clone());
        self.upsert_providers(
            vec![next_provider],
            UpsertOptions {
                persist: false,
                ..UpsertOptions::default()
            },
        )
        .await
    }

    async fn refresh_one_source(self: &Arc<Self>, source: &LiveSource) -> Vec<ServerProvider> {
        let next_provider = source.source.refresh().await;
        let next_provider = correlate_snapshot_with_source(&source.source, next_provider);
        self.sync_provider(next_provider, UpsertOptions::from_source(&source.active))
            .await
    }

    fn live_sources(&self) -> Vec<LiveSource> {
        self.live_subs
            .lock()
            .values()
            .map(|sub| LiveSource {
                source: build_snapshot_source(&sub.instance),
                active: Arc::clone(&sub.active),
            })
            .collect()
    }

    /// Untargeted refreshes probe every live source, so any caller can request
    /// them. Concurrent callers share one in-flight pass instead of each
    /// starting their own set of provider processes.
    async fn refresh_all(self: &Arc<Self>) -> Result<Vec<ServerProvider>, ()> {
        let mut completion = {
            let mut in_flight = self.refresh_all_in_flight.lock();
            if let Some(receiver) = in_flight.as_ref() {
                receiver.clone()
            } else {
                let (sender, receiver) = watch::channel::<RefreshAllResult>(None);
                *in_flight = Some(receiver.clone());
                // Detached from the caller (tracked by the registry) so an
                // interrupted first caller cannot cancel the pass that other
                // callers are already awaiting.
                let inner = Arc::clone(self);
                self.tasks.spawn(async move {
                    let pass = AssertUnwindSafe(async {
                        let sources = inner.live_sources();
                        join_all(
                            sources
                                .iter()
                                .map(|source| inner.refresh_one_source(source)),
                        )
                        .await;
                        inner.current_providers()
                    })
                    .catch_unwind()
                    .await;
                    let result = pass.map_err(|_| ());
                    *inner.refresh_all_in_flight.lock() = None;
                    let _ = sender.send(Some(result));
                });
                receiver
            }
        };
        loop {
            if let Some(result) = completion.borrow_and_update().clone() {
                return result;
            }
            if completion.changed().await.is_err() {
                // The pass was aborted with the registry.
                return Err(());
            }
        }
    }

    async fn refresh(
        self: &Arc<Self>,
        provider: Option<&ProviderDriverKind>,
    ) -> Result<Vec<ServerProvider>, ()> {
        let Some(provider) = provider else {
            return self.refresh_all().await;
        };
        // Kind-scoped refreshes target the default instance for that driver.
        let default_instance_id = default_instance_id_for_driver(provider);
        let sources = self.live_sources();
        let Some(provider_source) = sources
            .iter()
            .find(|candidate| candidate.source.instance_id == default_instance_id)
        else {
            return Ok(self.current_providers());
        };
        Ok(self.refresh_one_source(provider_source).await)
    }

    async fn refresh_instance(
        self: &Arc<Self>,
        instance_id: &ProviderInstanceId,
    ) -> Vec<ServerProvider> {
        let sources = self.live_sources();
        let Some(provider_source) = sources
            .iter()
            .find(|candidate| &candidate.source.instance_id == instance_id)
        else {
            return self.current_providers();
        };
        self.refresh_one_source(provider_source).await
    }

    /// Diff the aggregator's live-source set against the current instance
    /// registry and:
    ///   - subscribe to each newly-added or rebuilt instance's change stream
    ///     (so periodic + enrichment refreshes land in `providers`);
    ///   - read each newly-added/rebuilt instance's current snapshot after
    ///     subscribing, closing the race with its independently-running
    ///     background startup probe;
    ///   - prune `providers` of instances that no longer exist.
    ///
    /// Provider refreshes are owned by each managed provider and never run on
    /// the registry's construction path. Consumers see cached or pending
    /// snapshots immediately, then receive live probe results through the
    /// already-attached change stream.
    ///
    /// Each instance's subscription task is owned by its [`LiveSub`]. A rebuilt
    /// or removed instance is retired at the start of the pass: its task is
    /// aborted and snapshots it still emits are dropped. The task holds only the
    /// stream receiver and the instance ids, so the managed provider is released
    /// as soon as the instance registry drops the instance.
    async fn sync_live_sources(self: &Arc<Self>) {
        let _sync_guard = self.sync_lock.lock().await;
        let instances = self.source.list_instances();
        let unavailable_providers = self.source.list_unavailable();
        let next_by_instance: IndexMap<ProviderInstanceId, Arc<ProviderInstance>> = instances
            .into_iter()
            .map(|instance| (instance.instance_id.clone(), instance))
            .collect();
        let mut known_instance_ids: HashSet<ProviderInstanceId> =
            next_by_instance.keys().cloned().collect();
        for provider in &unavailable_providers {
            known_instance_ids.insert(snapshot_instance_key(provider));
        }
        // Carry over subscriptions for instances whose identity is unchanged
        // (reconcile treated them as no-op). Instances that disappeared, or
        // were rebuilt with a different reference, are retired now and fall
        // through to the "newly-added" branch below.
        let mut previously_subscribed: HashSet<ProviderInstanceId> = HashSet::new();
        let mut carried_over: HashSet<ProviderInstanceId> = HashSet::new();
        {
            let mut live_subs = self.live_subs.lock();
            for (instance_id, sub) in live_subs.iter_mut() {
                previously_subscribed.insert(instance_id.clone());
                if next_by_instance
                    .get(instance_id)
                    .is_some_and(|next_instance| Arc::ptr_eq(next_instance, &sub.instance))
                {
                    carried_over.insert(instance_id.clone());
                } else {
                    sub.retire();
                }
            }
        }

        // Collect new/rebuilt instances in `next_by_instance` insertion order
        // (which preserves settings-author order).
        let newly_added: Vec<(ProviderInstanceId, Arc<ProviderInstance>)> = next_by_instance
            .iter()
            .filter(|(instance_id, _)| !carried_over.contains(*instance_id))
            .map(|(instance_id, instance)| (instance_id.clone(), Arc::clone(instance)))
            .collect();

        let rebuilt_instance_ids: HashSet<&ProviderInstanceId> = newly_added
            .iter()
            .map(|(instance_id, _)| instance_id)
            .filter(|instance_id| previously_subscribed.contains(*instance_id))
            .collect();
        if !rebuilt_instance_ids.is_empty() {
            self.update_providers(|previous| {
                previous
                    .iter()
                    .map(|provider| {
                        if !rebuilt_instance_ids.contains(&provider.instance_id) {
                            return provider.clone();
                        }
                        let mut machine_snapshot = provider.clone();
                        machine_snapshot.workspace_snapshots = None;
                        machine_snapshot
                    })
                    .collect()
            });
        }

        // Start long-lived subscriptions to each new/rebuilt instance's change
        // stream before reading its current snapshot. If the driver's own
        // initial probe finishes during this sync, either the current read or
        // the active subscriber observes the result. The stream is opened here,
        // not inside the task, so the subscription exists when this returns.
        let new_subs: Vec<(ProviderInstanceId, LiveSub)> = newly_added
            .iter()
            .map(|(instance_id, instance)| {
                let driver_kind = instance.driver_kind.clone();
                let task_instance_id = instance_id.clone();
                let mut changes = build_snapshot_source(instance).stream_changes();
                let active = Arc::new(AtomicBool::new(true));
                let task = AbortOnDrop(tokio::spawn({
                    let inner = Arc::downgrade(self);
                    let active = Arc::clone(&active);
                    async move {
                        while let Some(provider) = changes.next().await {
                            if !active.load(Ordering::SeqCst) {
                                break;
                            }
                            let Some(inner) = inner.upgrade() else {
                                break;
                            };
                            let provider =
                                correlate_snapshot(&task_instance_id, &driver_kind, provider);
                            inner
                                .sync_provider(provider, UpsertOptions::from_source(&active))
                                .await;
                        }
                    }
                }));
                (
                    instance_id.clone(),
                    LiveSub {
                        instance: Arc::clone(instance),
                        active,
                        task: Some(task),
                    },
                )
            })
            .collect();
        tokio::task::yield_now().await;

        // Snapshot current state without starting a probe. Managed providers
        // launch their startup refresh independently, so this closes the
        // subscription race without putting external work on the registry or
        // app construction path.
        join_all(new_subs.iter().map(|(instance_id, sub)| async move {
            let pass = AssertUnwindSafe(async {
                let source = build_snapshot_source(&sub.instance);
                let provider = source.get_snapshot().await;
                let provider = correlate_snapshot_with_source(&source, provider);
                self.sync_provider(provider, UpsertOptions::from_source(&sub.active))
                    .await;
            })
            .catch_unwind()
            .await;
            if pass.is_err() {
                tracing::error!(
                    %instance_id,
                    "provider registry could not read a new instance's snapshot"
                );
            }
        }))
        .await;
        self.upsert_providers(
            unavailable_providers,
            UpsertOptions {
                persist: false,
                replace: true,
                ..UpsertOptions::default()
            },
        )
        .await;

        {
            let mut live_subs = self.live_subs.lock();
            let mut previous = std::mem::take(&mut *live_subs);
            for instance_id in next_by_instance.keys() {
                if carried_over.contains(instance_id)
                    && let Some(sub) = previous.shift_remove(instance_id)
                {
                    live_subs.insert(instance_id.clone(), sub);
                }
            }
            for (instance_id, sub) in new_subs {
                live_subs.insert(instance_id, sub);
            }
            // Whatever is left in `previous` was retired above; dropping it ends its tasks.
        }

        // Drop aggregator state for instances that have disappeared — otherwise
        // the UI would keep rendering ghosts.
        self.update_providers(|previous| {
            order_provider_snapshots(
                &previous
                    .iter()
                    .filter(|provider| {
                        known_instance_ids.contains(&snapshot_instance_key(provider))
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        });
        self.maintenance_update_states
            .lock()
            .retain(|instance_id, _| known_instance_ids.contains(instance_id));
    }

    async fn sync_live_sources_and_continue(self: &Arc<Self>) {
        if AssertUnwindSafe(self.sync_live_sources())
            .catch_unwind()
            .await
            .is_err()
        {
            tracing::error!("provider registry instance sync failed; keeping subscription alive");
        }
    }

    // The driver's own error type, logged by the caller; boxing it would only add a layer.
    #[allow(clippy::result_large_err)]
    async fn refresh_workspace_snapshot(
        self: &Arc<Self>,
        instance_id: &ProviderInstanceId,
        cwd: &str,
        fresh: bool,
    ) -> Result<Vec<ServerProvider>, ProviderDriverError> {
        // Fresh scans drop other instances' snapshots for this cwd first, so a
        // composer on one of them scans again on next use, even when this
        // instance is gone or cannot be scanned.
        if fresh {
            self.update_providers(|providers| {
                providers
                    .iter()
                    .map(|candidate| {
                        if &candidate.instance_id == instance_id {
                            candidate.clone()
                        } else {
                            drop_provider_workspace_snapshot(candidate.clone(), cwd)
                        }
                    })
                    .collect()
            });
        }
        let providers = self.current_providers();
        let provider = providers
            .iter()
            .find(|candidate| &candidate.instance_id == instance_id);
        let scanned_from = workspace_snapshot_of(provider, cwd).cloned();
        let now = (self.clock)();
        let Some(provider) = provider else {
            return Ok(providers);
        };
        if !provider.enabled
            || (!fresh
                && scanned_from.as_ref().is_some_and(|scanned| {
                    scanned.slash_commands_pending != Some(true)
                        && is_provider_workspace_snapshot_current(&scanned.checked_at, now)
                }))
        {
            return Ok(providers);
        }
        // Drivers spread their machine snapshot, whose `checkedAt` is the last
        // health check. The TTL needs the time this scan started reading files.
        let scanned_at = format_iso(now);
        let Some(instance) = self.source.get_instance(instance_id) else {
            return Ok(providers);
        };
        let Some(snapshot_for_cwd) = instance.snapshot_for_cwd.clone() else {
            return Ok(providers);
        };
        let claimed = {
            let mut refreshes = self.workspace_refreshes.lock();
            match refreshes
                .iter_mut()
                .find(|(candidate, _)| Arc::ptr_eq(candidate, &instance))
            {
                Some((_, cwds)) => !cwds.contains(cwd) && cwds.insert(cwd.to_owned()),
                None => {
                    refreshes.push((Arc::clone(&instance), HashSet::from([cwd.to_owned()])));
                    true
                }
            }
        };
        // A fresh scan never joins a running one, which may predate the change.
        if !claimed && !fresh {
            return Ok(self.current_providers());
        }
        let _release = claimed.then(|| ReleaseWorkspaceClaim {
            inner: self,
            instance: Arc::clone(&instance),
            cwd: cwd.to_owned(),
        });
        // Fresh scans also re-read the machine snapshot after invalidating caches.
        if fresh {
            if let Some(invalidate_caches) = &instance.invalidate_caches {
                invalidate_caches().await;
            }
            self.refresh_instance(instance_id).await;
        }
        let scoped_snapshot = snapshot_for_cwd(cwd.to_owned()).await?;
        if scoped_snapshot.provider.status == ServerProviderState::Error
            && scoped_snapshot.slash_commands_pending.is_none()
        {
            return Ok(self.current_providers());
        }
        let still_current = self
            .source
            .get_instance(instance_id)
            .is_some_and(|current| Arc::ptr_eq(&current, &instance));
        if !still_current {
            return Ok(self.current_providers());
        }
        // Write only if the cwd's snapshot did not change during the scan. A
        // session event or another scan that landed first is newer.
        let mut scoped = scoped_snapshot;
        scoped.provider.checked_at = scanned_at;
        Ok(self.update_providers(|current_providers| {
            current_providers
                .iter()
                .map(|candidate| {
                    if &candidate.instance_id == instance_id
                        && workspace_snapshot_of(Some(candidate), cwd) == scanned_from.as_ref()
                    {
                        upsert_provider_workspace_snapshot(candidate, cwd, &scoped)
                    } else {
                        candidate.clone()
                    }
                })
                .collect()
        }))
    }

    /// Upstream `recoverRefreshFailure`: a failed refresh logs and answers with
    /// the cached list.
    fn recover_refresh_failure(&self) -> Vec<ServerProvider> {
        tracing::error!("provider registry refresh failed; preserving cached providers");
        self.current_providers()
    }
}

pub struct RefreshWorkspaceSnapshotInput {
    pub instance_id: ProviderInstanceId,
    pub cwd: String,
    pub fresh: bool,
}

/// The aggregated provider list. Dropping it stops its tasks.
pub struct ProviderRegistry {
    inner: Arc<Inner>,
    tasks: Arc<TaskSet>,
}

impl Drop for ProviderRegistry {
    fn drop(&mut self) {
        self.tasks.abort_all();
        // Each subscription task is owned by its `LiveSub`; dropping them ends the tasks.
        self.inner.live_subs.lock().clear();
    }
}

impl ProviderRegistry {
    /// Upstream `ProviderRegistry.layer`. Hydrates from the per-instance disk
    /// cache so the UI has something to render during the first refresh, syncs
    /// the live instances, and keeps following instance changes. No provider
    /// probe runs on this path.
    pub async fn start(
        source: Arc<dyn ProviderInstanceSource>,
        manifest: Arc<ModelManifest>,
        options: ProviderRegistryOptions,
    ) -> Self {
        let tasks = Arc::new(TaskSet::new());
        let bundled_compatibility = bundled_model_manifest()
            .compatibility
            .as_deref()
            .map(decode_provider_compatibility_policies);
        let inner = Arc::new(Inner {
            source: Arc::clone(&source),
            manifest,
            cache_dir: options.provider_status_cache_dir,
            t3_code_version: options.t3_code_version,
            clock: options.clock.unwrap_or_else(system_clock),
            tasks: Arc::clone(&tasks),
            bundled_compatibility,
            providers: Mutex::new(Vec::new()),
            workspace_refreshes: Mutex::new(Vec::new()),
            maintenance_update_states: Mutex::new(HashMap::new()),
            live_subs: Mutex::new(IndexMap::new()),
            sync_lock: tokio::sync::Mutex::new(()),
            compatibility_refresh_running: AtomicBool::new(false),
            refresh_all_in_flight: Mutex::new(None),
            subscribers: Mutex::new(Vec::new()),
        });
        // Dropped with `registry` if anything below panics.
        let registry = Self {
            inner: Arc::clone(&inner),
            tasks,
        };

        // Boot-only: hydrate `providers` from the on-disk per-instance cache so
        // the UI has something to render during the first refresh. Instances
        // added post-boot skip this path; their first entry comes from the
        // reactive `sync_live_sources` pass below.
        let boot_instances = source.list_instances();
        let boot_sources: Vec<ProviderSnapshotSource> = boot_instances
            .iter()
            .map(|instance| build_snapshot_source(instance))
            .collect();
        let fallback_providers = Inner::load_providers(&boot_sources).await;
        let fallback_by_instance: HashMap<ProviderInstanceId, ServerProvider> = boot_sources
            .iter()
            .zip(fallback_providers.iter())
            .map(|(source, provider)| (source.instance_id.clone(), provider.clone()))
            .collect();

        let cache_dir = inner.cache_dir.clone();
        let cached_providers: Vec<ServerProvider> = join_all(boot_sources.iter().map(|source| {
            let fallback_by_instance = &fallback_by_instance;
            let cache_dir = &cache_dir;
            async move {
                // One cache file per configured instance. For the default
                // instance of a built-in kind the path equals `<kind>.json` —
                // identical to the legacy filename. We still require the cache
                // payload to carry matching instance id + driver kind; old
                // identity-less payloads are discarded and the first refresh
                // repopulates the cache.
                let file_path = resolve_provider_status_cache_path(cache_dir, &source.instance_id);
                let fallback_provider = fallback_by_instance.get(&source.instance_id)?;
                let cached_provider = read_provider_status_cache(&file_path).await?;
                if !is_cached_provider_correlated(&cached_provider, fallback_provider) {
                    tracing::warn!(
                        path = %file_path.display(),
                        instance_id = %source.instance_id,
                        cached_instance_id = %cached_provider.instance_id,
                        driver = %source.driver_kind,
                        cached_driver = %cached_provider.driver,
                        "provider status cache identity mismatch, ignoring"
                    );
                    return None;
                }
                Some(hydrate_cached_provider(&cached_provider, fallback_provider))
            }
        }))
        .await
        .into_iter()
        .flatten()
        .collect();
        let cached_providers = order_provider_snapshots(&cached_providers);

        let initial_policies = inner.manifest_policies().await;
        *inner.providers.lock() = cached_providers
            .into_iter()
            .map(|provider| inner.classify_compatibility(provider, initial_policies.as_deref()))
            .collect();

        // Seed `providers` with the boot-time fallback snapshots so consumers
        // calling `get_providers` immediately after start see a populated list —
        // even before the first `sync_live_sources` resolves. Cached snapshots
        // (already in `providers`) merge with these via `upsert_providers` so
        // on-disk state wins where present and pending fallbacks fill the gaps.
        inner
            .upsert_providers(
                fallback_providers,
                UpsertOptions {
                    publish: false,
                    ..UpsertOptions::default()
                },
            )
            .await;

        // Subscribe to registry mutations BEFORE running the initial sync, so a
        // reconcile that publishes between here and the loop starting cannot be
        // dropped (which would leave a settings change that replaces an instance
        // out of the aggregated list).
        let mut instance_changes = source.subscribe_changes();
        // Initial sync attaches subscriptions and snapshots current state for
        // every instance present at boot. Provider probes are already running in
        // their managed background tasks and never block this call.
        inner.sync_live_sources().await;
        // React to registry mutations — instance added / removed / rebuilt.
        let loop_inner = Arc::clone(&inner);
        registry.tasks.spawn(async move {
            while instance_changes.recv().await.is_some() {
                loop_inner.sync_live_sources_and_continue().await;
            }
        });

        registry
    }

    /// Read the latest provider snapshots for every configured instance.
    /// Multiple snapshots may share the same `driver` kind (multiple instances
    /// of the same driver) and disambiguate via `instance_id`.
    pub fn get_providers(&self) -> Vec<ServerProvider> {
        self.inner.current_providers()
    }

    /// Refresh all providers, or the default instance of the specified kind
    /// when supplied. Retained for legacy call sites; new code should prefer
    /// [`Self::refresh_instance`].
    pub async fn refresh(&self, provider: Option<&ProviderDriverKind>) -> Vec<ServerProvider> {
        match AssertUnwindSafe(self.inner.refresh(provider))
            .catch_unwind()
            .await
        {
            Ok(Ok(providers)) => providers,
            _ => self.inner.recover_refresh_failure(),
        }
    }

    /// Refresh the specific configured instance. Returns the updated snapshot
    /// list. When the instance id is unknown the call resolves with the
    /// currently cached list (no error), so callers don't have to special-case
    /// unknowns.
    pub async fn refresh_instance(&self, instance_id: &ProviderInstanceId) -> Vec<ServerProvider> {
        match AssertUnwindSafe(self.inner.refresh_instance(instance_id))
            .catch_unwind()
            .await
        {
            Ok(providers) => providers,
            Err(_) => self.inner.recover_refresh_failure(),
        }
    }

    /// Fill the skills and slash commands snapshot for one cwd. A cwd that
    /// already has a snapshot is left alone unless `fresh` is set. A fresh scan
    /// also refreshes the instance's machine snapshot and drops other
    /// instances' snapshots for the cwd.
    pub async fn refresh_workspace_snapshot(
        &self,
        input: RefreshWorkspaceSnapshotInput,
    ) -> Vec<ServerProvider> {
        match AssertUnwindSafe(self.inner.refresh_workspace_snapshot(
            &input.instance_id,
            &input.cwd,
            input.fresh,
        ))
        .catch_unwind()
        .await
        {
            Ok(Ok(providers)) => providers,
            Ok(Err(error)) => {
                tracing::error!(?error, "workspace snapshot scan failed");
                self.inner.recover_refresh_failure()
            }
            Err(_) => self.inner.recover_refresh_failure(),
        }
    }

    /// Resolve the maintenance capabilities owned by one live provider
    /// instance. Falls back to manual-only capabilities when the instance is
    /// not live. `fresh` re-derives ownership from the executable instead of
    /// the cache.
    pub async fn get_provider_maintenance_capabilities_for_instance(
        &self,
        instance_id: &ProviderInstanceId,
        provider: &ProviderDriverKind,
        fresh: bool,
    ) -> ProviderMaintenanceCapabilities {
        // Read the instance registry, not `live_subs`: the latter trails
        // reconciliation, and an update must never run a retired instance's
        // command against a freshly configured executable.
        match self.inner.source.get_instance(instance_id) {
            Some(instance) if &instance.driver_kind == provider => {
                instance.snapshot.resolve_maintenance(fresh).await
            }
            _ => make_manual_provider_maintenance_capabilities(provider.clone()),
        }
    }

    /// Apply volatile maintenance-action state to one configured instance.
    /// This state is never persisted to disk. Today only update actions are
    /// projected onto `ServerProvider.update_state`; install/auth actions can
    /// extend this action map without adding driver-scoped APIs.
    pub async fn set_provider_maintenance_action_state(
        &self,
        instance_id: &ProviderInstanceId,
        action: ProviderMaintenanceActionKind,
        state: Option<ServerProviderUpdateState>,
    ) -> Vec<ServerProvider> {
        self.inner
            .set_provider_maintenance_action_state(instance_id, action, state)
            .await
    }

    /// Stream of provider snapshot updates — one emission per aggregated
    /// change. The array contains the full current state. The subscription
    /// starts when this is called.
    pub fn stream_changes(&self) -> BoxStream<'static, Vec<ServerProvider>> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.inner.subscribers.lock().push(sender);
        Box::pin(UnboundedReceiverStream::new(receiver))
    }
}

#[cfg(test)]
mod tests;
