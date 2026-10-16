//! Port of upstream/apps/web/src/hooks/useSettings.ts (pure settings merge,
//! environment identification, and client-settings hydration / persistence).
//!
//! React hooks and server RPC routing are intentionally omitted.

#![allow(dead_code)]

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use futures::FutureExt;
use futures::future::Shared;
use parking_lot::Mutex;
use serde_json::Value;
use turin_contracts::settings::{
    ClientSettings, ClientSettingsPatch, EnvironmentIdentificationMode, ServerSettings,
    ServerSettingsPatch, UnifiedSettings, default_client_settings,
};
use turin_shared::server_settings::{model_preferences_migration_patch, resolve_model_preferences};

// ── Pure merge / identification ──────────────────────────────────────

pub fn merge_environment_settings(
    server_settings: ServerSettings,
    client_settings: ClientSettings,
) -> UnifiedSettings {
    let server_favorites = serde_json::to_value(&server_settings.favorites).ok();
    let server_preferences = serde_json::to_value(&server_settings.provider_model_preferences).ok();
    let legacy_favorites = serde_json::to_value(&client_settings.favorites).ok();
    let legacy_preferences = serde_json::to_value(&client_settings.provider_model_preferences).ok();
    let (favorites, provider_model_preferences) = resolve_model_preferences(
        server_favorites,
        server_preferences,
        legacy_favorites,
        legacy_preferences,
    );

    let mut server = server_settings;
    server.favorites = serde_json::from_value(favorites).ok();
    server.provider_model_preferences = serde_json::from_value(provider_model_preferences).ok();

    UnifiedSettings {
        server,
        client: client_settings,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolveEnvironmentIdentificationModeInput {
    pub mode: EnvironmentIdentificationMode,
    pub settings_hydrated: bool,
    pub palette_theme_active: bool,
    pub palette_theme_allows_artwork: bool,
}

pub fn resolve_environment_identification_mode(
    input: ResolveEnvironmentIdentificationModeInput,
) -> EnvironmentIdentificationMode {
    if !input.settings_hydrated {
        return EnvironmentIdentificationMode::None;
    }
    if input.palette_theme_active
        && !input.palette_theme_allows_artwork
        && input.mode == EnvironmentIdentificationMode::Artwork
    {
        EnvironmentIdentificationMode::Pill
    } else {
        input.mode
    }
}

fn merge_server_settings_patches(
    migration: Option<ServerSettingsPatch>,
    patch: ServerSettingsPatch,
) -> ServerSettingsPatch {
    let mut merged = serde_json::to_value(migration.unwrap_or_default()).expect("patch json");
    let patch_value = serde_json::to_value(patch).expect("patch json");
    if let (Value::Object(base), Value::Object(overlay)) = (&mut merged, patch_value) {
        // `{ ...migration, ...patch }`: explicit nulls in the patch win.
        for (key, value) in overlay {
            base.insert(key, value);
        }
    }
    serde_json::from_value(merged).expect("merged patch")
}

pub async fn prepare_model_preferences_patch(
    settings: &ServerSettings,
    patch: ServerSettingsPatch,
) -> Result<ServerSettingsPatch, ClientSettingsHydrationError> {
    if settings.favorites.is_none() || settings.provider_model_preferences.is_none() {
        ensure_client_settings_hydrated().await?;
    }
    let client = get_client_settings();
    // `null` (not absent) marks a list the server has not claimed yet.
    let server_favorites = serde_json::to_value(&settings.favorites).ok();
    let server_preferences = serde_json::to_value(&settings.provider_model_preferences).ok();
    let legacy_favorites = serde_json::to_value(&client.favorites).ok();
    let legacy_preferences = serde_json::to_value(&client.provider_model_preferences).ok();
    let migration = model_preferences_migration_patch(
        server_favorites.as_ref(),
        server_preferences.as_ref(),
        legacy_favorites.as_ref(),
        legacy_preferences.as_ref(),
    );
    Ok(merge_server_settings_patches(migration, patch))
}

/// `Struct.keys(ServerSettings.fields)`: the keys that route to the server.
const SERVER_SETTINGS_KEYS: &[&str] = &[
    "favorites",
    "providerModelPreferences",
    "worktreeCleanup",
    "storageCleanup",
    "worktreesDirectory",
    "previousWorktreesDirectories",
    "responseStreamingMode",
    "enableProviderUpdateChecks",
    "continueThreadsAfterServerUpdate",
    "enableAgentBrowserAccess",
    "projectAgentBrowserAccessOverrides",
    "defaultAutoPull",
    "defaultProjectScripts",
    "projectScriptOverrides",
    "projectAutoPullOverrides",
    "defaultModelSelection",
    "defaultRuntimeMode",
    "projectSettingsOverrides",
    "projectSettingsFolded",
    "enableAgentDeviceAccess",
    "enableDeviceSupport",
    "deviceOnboardingCompleted",
    "sidebarAutoSettleAfterDays",
    "snoozeLimitedThreads",
    "autoResumeLimitedThreads",
    "sidebarAutoSettleKeepPullRequests",
    "sidebarAutoSettleOnMerge",
    "backgroundActivity",
    "automaticGitFetchInterval",
    "providerHealthRefreshInterval",
    "backgroundActivityProfile",
    "defaultTheme",
    "defaultThemeSetAt",
    "environmentIcon",
    "defaultThreadEnvMode",
    "newWorktreesStartFromOrigin",
    "worktreeSubmodules",
    "addProjectBaseDirectory",
    "textGenerationModelSelection",
    "branchNamingMode",
    "branchNamePrefix",
    "branchNameInstructions",
    "removeAgentCreditsOnMerge",
    "sourceControlWritingStyle",
    "sourceControlWriterModelSelection",
    "reviewGuidesEnabled",
    "reviewGuideModelSelection",
    "reviewGuideInstructions",
    "pullRequestMergeMethod",
    "providerInstances",
    "observability",
    "sourceControlHosts",
    "usageLimitSources",
    "cursorKeychainUsageEnabled",
    "usagePriceOverrides",
    "usageModelAliases",
];

/// `splitPatch`: routes each key of a unified patch to the server-owned or
/// client-owned settings store. Both halves are JSON objects.
pub fn split_unified_patch(patch: &Value) -> (Value, Value) {
    let mut server_patch = serde_json::Map::new();
    let mut client_patch = serde_json::Map::new();
    if let Value::Object(entries) = patch {
        for (key, value) in entries {
            if SERVER_SETTINGS_KEYS.contains(&key.as_str()) {
                server_patch.insert(key.clone(), value.clone());
            } else {
                client_patch.insert(key.clone(), value.clone());
            }
        }
    }
    (Value::Object(server_patch), Value::Object(client_patch))
}

// ── Client settings store ────────────────────────────────────────────
//
// Upstream keeps module-level state and promise chains. Every entry point
// below does its synchronous part (status flips, optimistic snapshot, queue
// registration) at call time and returns a future for the rest, so ordering
// matches the promise-based original even when the caller polls late. Work is
// spawned on the ambient tokio runtime.

const CLIENT_SETTINGS_PERSISTENCE_ERROR_SCOPE: &str = "[CLIENT_SETTINGS]";

type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type HydrationResult = Result<(), ClientSettingsHydrationError>;

/// Persistence callback used by the `*_with` variants (upstream's optional
/// `persist` parameter).
pub type PersistClientSettings =
    Arc<dyn Fn(ClientSettings) -> BoxFut<HydrationResult> + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientSettingsHydrationStatus {
    Pending,
    Ready,
    Failed,
    Retrying,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientSettingsHydrationError {
    pub message: String,
}

impl ClientSettingsHydrationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ClientSettingsHydrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ClientSettingsHydrationError {}

/// Upstream `ensureLocalApi().persistence.{get,set}ClientSettings`.
#[async_trait]
pub trait ClientSettingsPersistence: Send + Sync {
    async fn get_client_settings(
        &self,
    ) -> Result<Option<ClientSettings>, ClientSettingsHydrationError>;
    async fn set_client_settings(&self, settings: ClientSettings) -> HydrationResult;
}

struct InMemoryClientSettingsPersistence {
    value: Mutex<Option<ClientSettings>>,
}

#[async_trait]
impl ClientSettingsPersistence for InMemoryClientSettingsPersistence {
    async fn get_client_settings(
        &self,
    ) -> Result<Option<ClientSettings>, ClientSettingsHydrationError> {
        Ok(self.value.lock().clone())
    }

    async fn set_client_settings(&self, settings: ClientSettings) -> HydrationResult {
        *self.value.lock() = Some(settings);
        Ok(())
    }
}

struct HydrationInFlight {
    id: u64,
    future: Shared<BoxFut<HydrationResult>>,
}

struct ClientSettingsStore {
    snapshot: Mutex<ClientSettings>,
    hydration_status: Mutex<ClientSettingsHydrationStatus>,
    hydration_generation: AtomicU64,
    hydration_seq: AtomicU64,
    hydration_in_flight: Mutex<Option<HydrationInFlight>>,
    deferred_patch_count: Mutex<u32>,
    persistence: Mutex<Arc<dyn ClientSettingsPersistence>>,
    /// Completion signal of the most recently enqueued persistence job.
    persistence_tail: Mutex<Option<Shared<tokio::sync::oneshot::Receiver<()>>>>,
}

impl ClientSettingsStore {
    fn new(persistence: Arc<dyn ClientSettingsPersistence>) -> Self {
        Self {
            snapshot: Mutex::new(default_client_settings()),
            hydration_status: Mutex::new(ClientSettingsHydrationStatus::Pending),
            hydration_generation: AtomicU64::new(0),
            hydration_seq: AtomicU64::new(0),
            hydration_in_flight: Mutex::new(None),
            deferred_patch_count: Mutex::new(0),
            persistence: Mutex::new(persistence),
            persistence_tail: Mutex::new(None),
        }
    }

    fn get_snapshot(&self) -> ClientSettings {
        self.snapshot.lock().clone()
    }

    fn replace_snapshot(&self, settings: ClientSettings) {
        *self.snapshot.lock() = settings;
    }

    fn hydration_status(&self) -> ClientSettingsHydrationStatus {
        *self.hydration_status.lock()
    }

    fn set_hydration_status(&self, next: ClientSettingsHydrationStatus) {
        *self.hydration_status.lock() = next;
    }

    /// `hydrateClientSettings`: starts (or joins) the single in-flight read.
    fn hydrate(self: &Arc<Self>) -> BoxFut<HydrationResult> {
        if self.hydration_status() == ClientSettingsHydrationStatus::Ready {
            return Box::pin(async { Ok(()) });
        }
        let mut slot = self.hydration_in_flight.lock();
        if let Some(in_flight) = slot.as_ref() {
            return Box::pin(in_flight.future.clone());
        }

        let generation = self.hydration_generation.load(Ordering::SeqCst);
        self.set_hydration_status(match self.hydration_status() {
            ClientSettingsHydrationStatus::Failed | ClientSettingsHydrationStatus::Retrying => {
                ClientSettingsHydrationStatus::Retrying
            }
            _ => ClientSettingsHydrationStatus::Pending,
        });

        let id = self.hydration_seq.fetch_add(1, Ordering::SeqCst);
        let store = Arc::clone(self);
        // The slot lock is held until after the task is registered, so the
        // task's own clean-up below cannot run before the slot is filled.
        let task = tokio::spawn(async move {
            let persistence = store.persistence.lock().clone();
            let result = match persistence.get_client_settings().await {
                Ok(persisted) => {
                    if store.hydration_generation.load(Ordering::SeqCst) == generation {
                        if let Some(persisted) = persisted {
                            store.replace_snapshot(persisted);
                        }
                        store.set_hydration_status(ClientSettingsHydrationStatus::Ready);
                    }
                    Ok(())
                }
                Err(error) => {
                    if store.hydration_generation.load(Ordering::SeqCst) == generation {
                        store.set_hydration_status(ClientSettingsHydrationStatus::Failed);
                    }
                    tracing::error!(
                        scope = CLIENT_SETTINGS_PERSISTENCE_ERROR_SCOPE,
                        operation = "hydrate",
                        error = %error,
                        "hydrate failed"
                    );
                    Err(error)
                }
            };
            let mut slot = store.hydration_in_flight.lock();
            if slot.as_ref().is_some_and(|current| current.id == id) {
                *slot = None;
            }
            result
        });
        let shared: Shared<BoxFut<HydrationResult>> = (Box::pin(async move {
            task.await.unwrap_or_else(|_| {
                Err(ClientSettingsHydrationError::new(
                    "client settings hydration task failed",
                ))
            })
        }) as BoxFut<HydrationResult>)
            .shared();
        *slot = Some(HydrationInFlight {
            id,
            future: shared.clone(),
        });
        Box::pin(shared)
    }

    /// `enqueueClientSettingsPersistence`: runs `work` after every previously
    /// enqueued job has settled, whether it succeeded or not.
    fn enqueue<T>(
        self: &Arc<Self>,
        work: BoxFut<Result<T, ClientSettingsHydrationError>>,
    ) -> BoxFut<Result<T, ClientSettingsHydrationError>>
    where
        T: Send + 'static,
    {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let previous = self.persistence_tail.lock().replace(done_rx.shared());
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let result = work.await;
            let _ = done_tx.send(());
            let _ = result_tx.send(result);
        });
        Box::pin(async move {
            result_rx.await.unwrap_or_else(|_| {
                Err(ClientSettingsHydrationError::new(
                    "client settings persistence job dropped",
                ))
            })
        })
    }

    fn apply_client_patch(current: &ClientSettings, patch: &ClientSettingsPatch) -> ClientSettings {
        let mut base = serde_json::to_value(current).expect("client settings");
        let overlay = serde_json::to_value(patch).expect("client patch");
        if let (Value::Object(base_map), Value::Object(overlay_map)) = (&mut base, overlay) {
            // `{ ...snapshot, ...patch }`: absent keys are skipped on encode,
            // so anything present (including an explicit null) wins.
            for (key, value) in overlay_map {
                base_map.insert(key, value);
            }
        }
        serde_json::from_value(base).expect("merged client settings")
    }

    /// `persistClientSettingsPatch`.
    fn persist_patch(
        self: &Arc<Self>,
        patch: ClientSettingsPatch,
        persist: PersistClientSettings,
    ) -> BoxFut<()> {
        // Patches queued before hydration must publish before newer optimistic patches.
        let defer_patch = self.hydration_status() != ClientSettingsHydrationStatus::Ready
            || *self.deferred_patch_count.lock() > 0;
        if defer_patch {
            *self.deferred_patch_count.lock() += 1;
        } else {
            let next = Self::apply_client_patch(&self.get_snapshot(), &patch);
            self.replace_snapshot(next);
        }
        let store = Arc::clone(self);
        let job = self.enqueue(Box::pin(async move {
            if defer_patch {
                let published = async {
                    if store.hydration_status() != ClientSettingsHydrationStatus::Ready {
                        store.hydrate().await?;
                    }
                    let next = Self::apply_client_patch(&store.get_snapshot(), &patch);
                    store.replace_snapshot(next);
                    Ok::<(), ClientSettingsHydrationError>(())
                }
                .await;
                {
                    let mut count = store.deferred_patch_count.lock();
                    *count = count.saturating_sub(1);
                }
                published?;
            }
            persist(store.get_snapshot()).await
        }));
        Box::pin(async move {
            if let Err(error) = job.await {
                tracing::error!(
                    scope = CLIENT_SETTINGS_PERSISTENCE_ERROR_SCOPE,
                    operation = "persist",
                    error = %error,
                    "persist failed"
                );
            }
        })
    }

    /// `persistClientSettingsUpdate`.
    fn persist_update(
        self: &Arc<Self>,
        update: Arc<dyn Fn(ClientSettings) -> ClientSettings + Send + Sync>,
        persist: PersistClientSettings,
    ) -> BoxFut<Result<ClientSettings, ClientSettingsHydrationError>> {
        let store = Arc::clone(self);
        self.enqueue(Box::pin(async move {
            if store.hydration_status() != ClientSettingsHydrationStatus::Ready {
                store.hydrate().await?;
            }
            loop {
                let current = store.get_snapshot();
                let next = update(current.clone());
                persist(next.clone()).await?;
                if store.get_snapshot() == current {
                    store.replace_snapshot(next.clone());
                    return Ok(next);
                }
            }
        }))
    }
}

fn client_settings_store() -> Arc<ClientSettingsStore> {
    static STORE: OnceLock<Arc<ClientSettingsStore>> = OnceLock::new();
    Arc::clone(STORE.get_or_init(|| {
        Arc::new(ClientSettingsStore::new(Arc::new(
            InMemoryClientSettingsPersistence {
                value: Mutex::new(None),
            },
        )))
    }))
}

/// Replaces the backing store (upstream: `ensureLocalApi().persistence`).
pub fn set_client_settings_persistence(persistence: Arc<dyn ClientSettingsPersistence>) {
    *client_settings_store().persistence.lock() = persistence;
}

fn default_client_settings_persistence() -> PersistClientSettings {
    Arc::new(|settings| {
        let persistence = client_settings_store().persistence.lock().clone();
        Box::pin(async move { persistence.set_client_settings(settings).await })
    })
}

/// Non-hook accessor for the current merged client settings snapshot.
pub fn get_client_settings() -> ClientSettings {
    client_settings_store().get_snapshot()
}

pub fn client_settings_hydration_status() -> ClientSettingsHydrationStatus {
    client_settings_store().hydration_status()
}

/// `useClientSettingsHydrated` without the hook.
pub fn client_settings_hydrated() -> bool {
    client_settings_hydration_status() == ClientSettingsHydrationStatus::Ready
}

/// Resolves after settings load or storage confirms no saved settings exist.
/// Failed reads reject and remain retryable. They must not allow defaults to
/// overwrite saved preferences.
pub fn ensure_client_settings_hydrated() -> impl Future<Output = HydrationResult> + Send {
    client_settings_store().hydrate()
}

/// Queues a client-settings patch. Persistence failures are logged, not returned.
pub fn persist_client_settings_patch(
    patch: ClientSettingsPatch,
) -> impl Future<Output = ()> + Send {
    persist_client_settings_patch_with(patch, default_client_settings_persistence())
}

pub fn persist_client_settings_patch_with(
    patch: ClientSettingsPatch,
    persist: PersistClientSettings,
) -> impl Future<Output = ()> + Send {
    client_settings_store().persist_patch(patch, persist)
}

/// Persists a client-settings update before publishing it to the in-memory
/// snapshot. If another settings write lands while persistence is pending, the
/// updater is reapplied to that newer snapshot and persisted again so neither
/// change is lost.
pub fn persist_client_settings_update<F>(
    update: F,
) -> impl Future<Output = Result<ClientSettings, ClientSettingsHydrationError>> + Send
where
    F: Fn(ClientSettings) -> ClientSettings + Send + Sync + 'static,
{
    persist_client_settings_update_with(update, default_client_settings_persistence())
}

pub fn persist_client_settings_update_with<F>(
    update: F,
    persist: PersistClientSettings,
) -> impl Future<Output = Result<ClientSettings, ClientSettingsHydrationError>> + Send
where
    F: Fn(ClientSettings) -> ClientSettings + Send + Sync + 'static,
{
    client_settings_store().persist_update(Arc::new(update), persist)
}

/// `__resetClientSettingsPersistenceForTests`.
pub fn reset_client_settings_persistence_for_tests() {
    let store = client_settings_store();
    store.hydration_generation.fetch_add(1, Ordering::SeqCst);
    store.replace_snapshot(default_client_settings());
    store.set_hydration_status(ClientSettingsHydrationStatus::Pending);
    *store.hydration_in_flight.lock() = None;
    *store.persistence_tail.lock() = None;
    *store.deferred_patch_count.lock() = 0;
}

/// `__setClientSettingsForTests`.
pub fn set_client_settings_for_tests(settings: ClientSettings) {
    let store = client_settings_store();
    store.hydration_generation.fetch_add(1, Ordering::SeqCst);
    store.replace_snapshot(settings);
    store.set_hydration_status(ClientSettingsHydrationStatus::Ready);
    *store.hydration_in_flight.lock() = None;
}

#[cfg(test)]
mod tests {
    // Port of upstream/apps/web/src/hooks/useSettings.test.ts. The store is a
    // process-wide singleton (as upstream's module state is), so tests take
    // `TEST_LOCK` through `setup()`.
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;

    use tokio::sync::oneshot;
    use turin_contracts::base_schemas::TrimmedNonEmptyString;
    use turin_contracts::provider_instance::{
        ProviderDriverKind, ProviderInstanceConfig, ProviderInstanceId,
    };
    use turin_contracts::settings::{
        ModelFavorite, ProviderModelPreferences, ProviderModelPreferencesEntry, SetModelFavorite,
        SetModelHidden, SetProviderModelOrder, TimestampFormat,
    };

    use serde_json::json;
    use turin_contracts::settings::default_server_settings;
    use turin_shared::server_settings::apply_server_settings_patch;

    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    enum GetStep {
        Fail(&'static str),
        Pending(oneshot::Receiver<ClientSettings>),
    }

    enum SetStep {
        Fail(&'static str),
        /// Signals `started`, then waits for `release` before succeeding.
        Block {
            started: Option<oneshot::Sender<()>>,
            release: oneshot::Receiver<()>,
        },
        /// Succeeds once `release` fires, without signalling.
        Gate(oneshot::Receiver<()>),
    }

    /// Scripted stand-in for `persistenceMocks`.
    struct Mock {
        get_steps: Mutex<VecDeque<GetStep>>,
        get_fallback: Mutex<Result<Option<ClientSettings>, &'static str>>,
        get_calls: AtomicUsize,
        set_steps: Mutex<VecDeque<SetStep>>,
        set_calls: Mutex<Vec<ClientSettings>>,
        durable: Mutex<ClientSettings>,
    }

    impl Mock {
        fn get_count(&self) -> usize {
            self.get_calls.load(Ordering::SeqCst)
        }

        fn set_history(&self) -> Vec<ClientSettings> {
            self.set_calls.lock().clone()
        }

        fn push_get(&self, step: GetStep) {
            self.get_steps.lock().push_back(step);
        }

        fn push_set(&self, step: SetStep) {
            self.set_steps.lock().push_back(step);
        }

        fn persist_fn(self: &Arc<Self>) -> PersistClientSettings {
            let mock = Arc::clone(self);
            Arc::new(move |settings| {
                let mock = Arc::clone(&mock);
                Box::pin(async move { mock.set(settings).await })
            })
        }

        async fn set(&self, settings: ClientSettings) -> HydrationResult {
            self.set_calls.lock().push(settings.clone());
            let step = self.set_steps.lock().pop_front();
            match step {
                None => {}
                Some(SetStep::Fail(message)) => {
                    return Err(ClientSettingsHydrationError::new(message));
                }
                Some(SetStep::Block { started, release }) => {
                    if let Some(started) = started {
                        let _ = started.send(());
                    }
                    let _ = release.await;
                }
                Some(SetStep::Gate(release)) => {
                    let _ = release.await;
                }
            }
            *self.durable.lock() = settings;
            Ok(())
        }
    }

    #[async_trait]
    impl ClientSettingsPersistence for Mock {
        async fn get_client_settings(
            &self,
        ) -> Result<Option<ClientSettings>, ClientSettingsHydrationError> {
            self.get_calls.fetch_add(1, Ordering::SeqCst);
            let step = self.get_steps.lock().pop_front();
            match step {
                Some(GetStep::Fail(message)) => Err(ClientSettingsHydrationError::new(message)),
                Some(GetStep::Pending(rx)) => rx
                    .await
                    .map(Some)
                    .map_err(|_| ClientSettingsHydrationError::new("read dropped")),
                None => self
                    .get_fallback
                    .lock()
                    .clone()
                    .map_err(ClientSettingsHydrationError::new),
            }
        }

        async fn set_client_settings(&self, settings: ClientSettings) -> HydrationResult {
            self.set(settings).await
        }
    }

    struct Setup {
        mock: Arc<Mock>,
        _guard: parking_lot::MutexGuard<'static, ()>,
    }

    impl std::ops::Deref for Setup {
        type Target = Arc<Mock>;
        fn deref(&self) -> &Self::Target {
            &self.mock
        }
    }

    /// `beforeEach`: reads resolve `null`, writes resolve, store reset.
    fn setup() -> Setup {
        let guard = TEST_LOCK.lock();
        reset_client_settings_persistence_for_tests();
        let mock = Arc::new(Mock {
            get_steps: Mutex::new(VecDeque::new()),
            get_fallback: Mutex::new(Ok(None)),
            get_calls: AtomicUsize::new(0),
            set_steps: Mutex::new(VecDeque::new()),
            set_calls: Mutex::new(Vec::new()),
            durable: Mutex::new(default_client_settings()),
        });
        set_client_settings_persistence(mock.clone());
        Setup {
            mock,
            _guard: guard,
        }
    }

    /// Lets spawned store tasks run (stand-in for `await Promise.resolve()`).
    async fn settle() {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    fn codex_work() -> ProviderInstanceId {
        ProviderInstanceId::new("codex_work").expect("id")
    }

    fn model(name: &str) -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(name).expect("model")
    }

    fn saved_settings() -> ClientSettings {
        ClientSettings {
            timestamp_format: TimestampFormat::TwelveHour,
            favorites: vec![ModelFavorite {
                provider: codex_work(),
                model: model("gpt-5.6"),
            }],
            ..default_client_settings()
        }
    }

    const ONBOARDING_COMPLETED_AT: &str = "2026-09-05T12:00:00.000Z";

    fn complete(current: ClientSettings) -> ClientSettings {
        ClientSettings {
            onboarding_completed_at: Some(ONBOARDING_COMPLETED_AT.to_string()),
            ..current
        }
    }

    fn word_wrap(value: bool) -> ClientSettingsPatch {
        ClientSettingsPatch {
            word_wrap: Some(value),
            ..Default::default()
        }
    }

    /// Upstream appends a `browserProfiles` entry; `ClientSettings` has no
    /// such field in Turin (browser preview is cut), so a favorite stands in
    /// as the list-valued setting the awaited update appends to.
    fn imported_profile() -> ModelFavorite {
        ModelFavorite {
            provider: codex_work(),
            model: model("profile-import"),
        }
    }

    // ── client settings hydration ────────────────────────────────────

    #[tokio::test]
    async fn waits_for_saved_model_preferences_before_first_server_edit() {
        let mock = setup();
        let provider = codex_work();
        let (finish_read, read) = oneshot::channel();
        mock.push_get(GetStep::Pending(read));
        let patch = tokio::spawn({
            let provider = provider.clone();
            async move {
                prepare_model_preferences_patch(
                    &default_server_settings(),
                    ServerSettingsPatch {
                        set_model_favorites: Some(vec![SetModelFavorite {
                            provider: provider.clone(),
                            model: model("new"),
                            favorite: true,
                        }]),
                        set_models_hidden: Some(vec![SetModelHidden {
                            provider,
                            model: model("new"),
                            hidden: true,
                        }]),
                        ..Default::default()
                    },
                )
                .await
            }
        });
        settle().await;
        assert_eq!(get_client_settings(), default_client_settings());
        let mut saved = saved_settings();
        saved.provider_model_preferences.insert(
            provider.clone(),
            ProviderModelPreferencesEntry {
                hidden_models: vec!["saved-hidden".to_string()],
                model_order: vec!["saved-order".to_string()],
            },
        );
        finish_read.send(saved.clone()).expect("finish read");

        let prepared = patch.await.expect("join").expect("patch");
        let edited =
            apply_server_settings_patch(&default_server_settings(), &prepared).expect("apply");
        let mut expected_favorites = saved.favorites.clone();
        expected_favorites.push(ModelFavorite {
            provider: provider.clone(),
            model: model("new"),
        });
        assert_eq!(edited.favorites, Some(expected_favorites));
        let preferences = edited.provider_model_preferences.expect("preferences");
        let provider_preferences = preferences.get(&provider).expect("provider");
        assert_eq!(
            provider_preferences.hidden_models,
            vec!["saved-hidden".to_string(), "new".to_string()]
        );
        assert_eq!(
            provider_preferences.model_order,
            vec!["saved-order".to_string()]
        );
    }

    #[tokio::test]
    async fn does_not_prepare_a_model_preference_write_after_a_failed_legacy_read() {
        let mock = setup();
        mock.push_get(GetStep::Fail("storage unavailable"));
        let result = prepare_model_preferences_patch(
            &default_server_settings(),
            ServerSettingsPatch {
                set_provider_model_order: Some(SetProviderModelOrder {
                    provider: codex_work(),
                    model_order: vec![],
                }),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            result.expect_err("failed read"),
            ClientSettingsHydrationError::new("storage unavailable")
        );
    }

    #[tokio::test]
    async fn rejects_completion_after_a_failed_read_and_preserves_saved_preferences_on_retry() {
        let mock = setup();
        mock.push_get(GetStep::Fail("storage unavailable"));
        *mock.get_fallback.lock() = Ok(Some(saved_settings()));

        assert_eq!(
            persist_client_settings_update(complete)
                .await
                .expect_err("failed read"),
            ClientSettingsHydrationError::new("storage unavailable")
        );
        assert!(mock.set_history().is_empty());
        assert_eq!(get_client_settings(), default_client_settings());

        let completed_settings = ClientSettings {
            onboarding_completed_at: Some(ONBOARDING_COMPLETED_AT.to_string()),
            ..saved_settings()
        };
        assert_eq!(
            persist_client_settings_update(complete).await,
            Ok(completed_settings.clone())
        );
        assert_eq!(mock.set_history(), vec![completed_settings]);
        assert_eq!(mock.get_count(), 2);
    }

    #[tokio::test]
    async fn uses_defaults_only_after_storage_confirms_no_saved_settings_exist() {
        let mock = setup();
        let completed_settings = ClientSettings {
            onboarding_completed_at: Some(ONBOARDING_COMPLETED_AT.to_string()),
            ..default_client_settings()
        };
        assert_eq!(
            persist_client_settings_update(complete).await,
            Ok(completed_settings.clone())
        );
        assert_eq!(mock.get_count(), 1);
        assert_eq!(mock.set_history(), vec![completed_settings]);
    }

    #[tokio::test]
    async fn holds_patches_until_a_pending_read_supplies_the_saved_preferences() {
        let mock = setup();
        let (finish_read, read) = oneshot::channel();
        mock.push_get(GetStep::Pending(read));

        let hydration = ensure_client_settings_hydrated();
        let patch = persist_client_settings_patch(word_wrap(false));
        settle().await;
        assert_eq!(get_client_settings(), default_client_settings());
        assert!(mock.set_history().is_empty());

        finish_read.send(saved_settings()).expect("finish read");
        hydration.await.expect("hydrated");
        patch.await;
        assert_eq!(
            mock.set_history(),
            vec![ClientSettings {
                word_wrap: false,
                ..saved_settings()
            }]
        );
        assert_eq!(mock.get_count(), 1);
    }

    #[tokio::test]
    async fn handles_failed_patch_reads_without_writing_and_retries_with_the_saved_preferences() {
        let mock = setup();
        *mock.get_fallback.lock() = Err("storage unavailable");

        let hydration = ensure_client_settings_hydrated();
        let failed_patch = persist_client_settings_patch(word_wrap(false));
        assert_eq!(
            hydration.await,
            Err(ClientSettingsHydrationError::new("storage unavailable"))
        );
        // The failed patch is logged, not returned, and must not write.
        failed_patch.await;
        assert!(mock.set_history().is_empty());

        *mock.get_fallback.lock() = Ok(Some(saved_settings()));
        persist_client_settings_patch(word_wrap(false)).await;
        assert_eq!(
            mock.set_history(),
            vec![ClientSettings {
                word_wrap: false,
                ..saved_settings()
            }]
        );
    }

    #[tokio::test]
    async fn preserves_patch_order_across_hydration_and_a_blocked_completion_write() {
        let mock = setup();
        let (finish_read, read) = oneshot::channel();
        mock.push_get(GetStep::Pending(read));
        *mock.get_fallback.lock() = Ok(Some(saved_settings()));
        let (completion_write_started, started) = oneshot::channel();
        let (finish_completion_write, blocked_write) = oneshot::channel();
        mock.push_set(SetStep::Block {
            started: Some(completion_write_started),
            release: blocked_write,
        });
        *mock.durable.lock() = saved_settings();
        let persist = mock.persist_fn();

        let completion = persist_client_settings_update_with(complete, persist.clone());
        let first_patch = persist_client_settings_patch_with(word_wrap(false), persist.clone());
        finish_read.send(saved_settings()).expect("finish read");
        started.await.expect("completion write started");
        let second_patch = persist_client_settings_patch_with(word_wrap(true), persist.clone());
        let final_write = persist_client_settings_update_with(|current| current, persist);

        finish_completion_write.send(()).expect("unblock");
        completion.await.expect("completion");
        final_write.await.expect("final write");
        first_patch.await;
        second_patch.await;

        let expected = ClientSettings {
            onboarding_completed_at: Some(ONBOARDING_COMPLETED_AT.to_string()),
            word_wrap: true,
            ..saved_settings()
        };
        assert_eq!(get_client_settings(), expected);
        assert_eq!(*mock.durable.lock(), expected);
    }

    // ── persistClientSettingsPatch ───────────────────────────────────

    #[tokio::test]
    async fn waits_for_settings_writes_in_request_order() {
        let mock = setup();
        set_client_settings_for_tests(default_client_settings());
        let (first_started, started) = oneshot::channel();
        let (finish_first, first_release) = oneshot::channel();
        mock.push_set(SetStep::Block {
            started: Some(first_started),
            release: first_release,
        });

        let first_settings = ClientSettings {
            snap_shot_flash: false,
            ..default_client_settings()
        };
        let second_settings = ClientSettings {
            snap_shot_play_sound: false,
            ..first_settings.clone()
        };
        let first = persist_client_settings_patch(ClientSettingsPatch {
            snap_shot_flash: Some(false),
            ..Default::default()
        });
        started.await.expect("first write started");
        let second = persist_client_settings_patch(ClientSettingsPatch {
            snap_shot_play_sound: Some(false),
            ..Default::default()
        });

        assert_eq!(mock.set_history().len(), 1);
        assert_eq!(get_client_settings(), second_settings);
        finish_first.send(()).expect("finish first");
        first.await;
        second.await;

        assert_eq!(mock.set_history(), vec![first_settings, second_settings]);
    }

    // ── persistClientSettingsUpdate ──────────────────────────────────

    #[tokio::test]
    async fn publishes_the_update_only_after_persistence_succeeds() {
        let mock = setup();
        set_client_settings_for_tests(default_client_settings());
        let (finish_persistence, gate) = oneshot::channel();
        mock.push_set(SetStep::Gate(gate));

        let pending = persist_client_settings_update_with(
            |current| ClientSettings {
                timestamp_format: TimestampFormat::TwelveHour,
                ..current
            },
            mock.persist_fn(),
        );
        settle().await;
        assert_eq!(
            get_client_settings().timestamp_format,
            default_client_settings().timestamp_format
        );
        finish_persistence.send(()).expect("finish");
        let result = pending.await.expect("update");
        assert_eq!(result.timestamp_format, TimestampFormat::TwelveHour);
        assert_eq!(
            get_client_settings().timestamp_format,
            TimestampFormat::TwelveHour
        );
    }

    #[tokio::test]
    async fn keeps_the_current_snapshot_and_propagates_persistence_failure() {
        let mock = setup();
        set_client_settings_for_tests(default_client_settings());
        mock.push_set(SetStep::Fail("disk full"));

        let result = persist_client_settings_update_with(
            |current| ClientSettings {
                timestamp_format: TimestampFormat::TwelveHour,
                ..current
            },
            mock.persist_fn(),
        )
        .await;
        assert_eq!(
            result.expect_err("persist failure"),
            ClientSettingsHydrationError::new("disk full")
        );
        assert_eq!(get_client_settings(), default_client_settings());
    }

    #[tokio::test]
    async fn preserves_an_optimistic_write_made_while_an_awaited_update_persists() {
        let mock = setup();
        let (finish_first_persistence, first_persistence) = oneshot::channel();
        mock.push_set(SetStep::Gate(first_persistence));
        set_client_settings_for_tests(default_client_settings());
        let persist = mock.persist_fn();
        let profile = imported_profile();

        let pending = persist_client_settings_update_with(
            {
                let profile = profile.clone();
                move |current| {
                    let mut next = current;
                    next.favorites.push(profile.clone());
                    next
                }
            },
            persist.clone(),
        );
        settle().await;
        let patch = persist_client_settings_patch_with(word_wrap(false), persist);
        finish_first_persistence.send(()).expect("finish first");
        pending.await.expect("update");
        patch.await;

        let calls = mock.set_history();
        assert_eq!(calls.len(), 3);
        assert!(!calls[1].word_wrap);
        assert!(calls[1].favorites.contains(&profile));
        let durable = mock.durable.lock().clone();
        assert!(!durable.word_wrap);
        assert!(durable.favorites.contains(&profile));
        assert!(!get_client_settings().word_wrap);
        assert!(get_client_settings().favorites.contains(&profile));
    }

    #[tokio::test]
    async fn orders_an_awaited_update_after_an_older_optimistic_write() {
        let mock = setup();
        let (finish_old_write, old_write) = oneshot::channel();
        mock.push_set(SetStep::Gate(old_write));
        set_client_settings_for_tests(default_client_settings());
        let persist = mock.persist_fn();
        let profile = imported_profile();

        let older = persist_client_settings_patch_with(word_wrap(false), persist.clone());
        let registration = persist_client_settings_update_with(
            {
                let profile = profile.clone();
                move |current| {
                    let mut next = current;
                    next.favorites.push(profile.clone());
                    next
                }
            },
            persist,
        );
        settle().await;
        assert_eq!(mock.set_history().len(), 1);

        finish_old_write.send(()).expect("finish old write");
        registration.await.expect("registration");
        older.await;

        assert_eq!(mock.set_history().len(), 2);
        let durable = mock.durable.lock().clone();
        assert!(!durable.word_wrap);
        assert!(durable.favorites.contains(&profile));
    }

    #[tokio::test]
    async fn continues_the_queue_after_a_rejected_write() {
        let mock = setup();
        mock.push_set(SetStep::Fail("disk full"));
        set_client_settings_for_tests(default_client_settings());
        let persist = mock.persist_fn();

        let rejected = persist_client_settings_update_with(
            |current| ClientSettings {
                timestamp_format: TimestampFormat::TwelveHour,
                ..current
            },
            persist.clone(),
        )
        .await;
        assert_eq!(
            rejected.expect_err("rejected"),
            ClientSettingsHydrationError::new("disk full")
        );
        let resolved = persist_client_settings_update_with(
            |current| ClientSettings {
                word_wrap: false,
                ..current
            },
            persist,
        )
        .await
        .expect("resolved");
        assert!(!resolved.word_wrap);
    }

    #[test]
    fn split_unified_patch_routes_server_and_client_keys() {
        let (server, client) = split_unified_patch(&json!({
            "favorites": [],
            "defaultTheme": "dark",
            "wordWrap": false,
            "timestampFormat": "12-hour",
        }));
        assert_eq!(server, json!({ "favorites": [], "defaultTheme": "dark" }));
        assert_eq!(
            client,
            json!({ "wordWrap": false, "timestampFormat": "12-hour" })
        );
    }

    #[test]
    fn server_settings_keys_cover_every_default_server_setting() {
        let Value::Object(defaults) =
            serde_json::to_value(default_server_settings()).expect("server settings")
        else {
            panic!("server settings encode as an object");
        };
        for key in defaults.keys() {
            assert!(SERVER_SETTINGS_KEYS.contains(&key.as_str()), "{key}");
        }
    }

    // ── resolveEnvironmentIdentificationMode ─────────────────────────

    fn identification(
        mode: EnvironmentIdentificationMode,
        settings_hydrated: bool,
        palette_theme_active: bool,
        palette_theme_allows_artwork: bool,
    ) -> EnvironmentIdentificationMode {
        resolve_environment_identification_mode(ResolveEnvironmentIdentificationModeInput {
            mode,
            settings_hydrated,
            palette_theme_active,
            palette_theme_allows_artwork,
        })
    }

    #[test]
    fn keeps_identification_hidden_until_client_settings_hydrate() {
        assert_eq!(
            identification(EnvironmentIdentificationMode::Artwork, false, false, false),
            EnvironmentIdentificationMode::None
        );
        assert_eq!(
            identification(EnvironmentIdentificationMode::Pill, true, false, false),
            EnvironmentIdentificationMode::Pill
        );
    }

    #[test]
    fn uses_a_pill_instead_of_artwork_with_a_palette_theme() {
        assert_eq!(
            identification(EnvironmentIdentificationMode::Artwork, true, true, false),
            EnvironmentIdentificationMode::Pill
        );
    }

    #[test]
    fn respects_none_with_a_palette_theme() {
        assert_eq!(
            identification(EnvironmentIdentificationMode::None, true, true, false),
            EnvironmentIdentificationMode::None
        );
    }

    #[test]
    fn keeps_artwork_when_the_palette_theme_opts_into_it() {
        assert_eq!(
            identification(EnvironmentIdentificationMode::Artwork, true, true, true),
            EnvironmentIdentificationMode::Artwork
        );
    }

    // ── mergeEnvironmentSettings ─────────────────────────────────────

    #[test]
    fn combines_the_selected_environments_server_settings_with_client_preferences() {
        let mut server_settings = default_server_settings();
        server_settings.provider_instances.insert(
            ProviderInstanceId::new("codex_remote").expect("id"),
            ProviderInstanceConfig {
                driver: ProviderDriverKind::new("codex").expect("driver"),
                display_name: None,
                accent_color: None,
                environment: None,
                enabled: Some(true),
                config: None,
            },
        );
        let client_settings = ClientSettings {
            favorites: vec![ModelFavorite {
                provider: ProviderInstanceId::new("codex_remote").expect("id"),
                model: model("gpt-5.4"),
            }],
            ..default_client_settings()
        };
        let settings = merge_environment_settings(server_settings.clone(), client_settings.clone());
        assert_eq!(
            settings.server.provider_instances,
            server_settings.provider_instances
        );
        assert_eq!(settings.server.favorites, Some(client_settings.favorites));
    }

    #[test]
    fn keeps_server_settlement_settings_when_legacy_client_data_contains_retired_keys() {
        let mut server_settings = default_server_settings();
        server_settings.sidebar_auto_settle_after_days =
            Some(serde_json::from_value(json!(14.0)).expect("days"));
        server_settings.sidebar_auto_settle_on_merge = false;
        // Retired client keys are dropped on decode; the merged result must
        // still carry the server-owned values.
        let legacy_client: ClientSettings = serde_json::from_value({
            let mut value = serde_json::to_value(default_client_settings()).expect("client");
            value["sidebarAutoSettleAfterDays"] = json!(1);
            value["sidebarAutoSettleOnMerge"] = json!(true);
            value
        })
        .expect("legacy client settings");
        let settings = merge_environment_settings(server_settings.clone(), legacy_client);
        assert_eq!(
            settings.server.sidebar_auto_settle_after_days,
            server_settings.sidebar_auto_settle_after_days
        );
        assert!(!settings.server.sidebar_auto_settle_on_merge);
    }

    // ── onboarding completion persistence ────────────────────────────

    #[tokio::test]
    async fn keeps_onboarding_incomplete_after_a_failed_save_and_preserves_preferences_on_retry() {
        let mock = setup();
        mock.push_set(SetStep::Fail("disk full"));
        let existing_settings = saved_settings();
        set_client_settings_for_tests(existing_settings.clone());
        let persist = mock.persist_fn();

        let failed = persist_client_settings_update_with(complete, persist.clone()).await;
        assert_eq!(
            failed.expect_err("failed save"),
            ClientSettingsHydrationError::new("disk full")
        );
        assert_eq!(get_client_settings(), existing_settings);
        assert_eq!(get_client_settings().onboarding_completed_at, None);

        let completed_settings = complete(existing_settings);
        assert_eq!(
            persist_client_settings_update_with(complete, persist).await,
            Ok(completed_settings.clone())
        );
        assert_eq!(get_client_settings(), completed_settings);
        assert_eq!(mock.set_history().last(), Some(&completed_settings));
    }

    #[test]
    fn uses_the_environments_model_preferences_after_migration_including_empty_lists() {
        let mut client = default_client_settings();
        client.favorites = vec![ModelFavorite {
            provider: codex_work(),
            model: model("old"),
        }];
        client.provider_model_preferences.insert(
            codex_work(),
            ProviderModelPreferencesEntry {
                hidden_models: vec!["old".to_string()],
                model_order: vec![],
            },
        );
        assert_eq!(
            merge_environment_settings(default_server_settings(), client.clone())
                .server
                .favorites,
            Some(client.favorites.clone())
        );
        let merged = merge_environment_settings(
            ServerSettings {
                favorites: Some(vec![]),
                provider_model_preferences: Some(ProviderModelPreferences::default()),
                ..default_server_settings()
            },
            client,
        );
        assert_eq!(merged.server.favorites, Some(vec![]));
        assert_eq!(
            merged.server.provider_model_preferences,
            Some(ProviderModelPreferences::default())
        );
    }
}
