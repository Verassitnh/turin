//! Port of upstream/apps/server/src/provider/ProviderInstanceRegistry.ts
//! and ProviderInstanceRegistryMutator.ts.
//! Also: Port of upstream/apps/server/src/provider/ProviderInstanceRegistryMutator.ts.
//!
//! The registry is the only owner of live `ProviderInstance`s. Unknown
//! drivers and decode/create failures become unavailable shadow snapshots
//! instead of failing the process. `reconcile` closes removed scopes before
//! it builds replacements, and a separate async lock serializes reconciles
//! so a read never waits on `create`.

use std::{collections::HashMap, sync::Arc};

use indexmap::IndexMap;
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::error;
use turin_contracts::{
    provider_instance::{
        ProviderDriverKind, ProviderInstanceConfig, ProviderInstanceConfigMap, ProviderInstanceId,
    },
    settings::provider_instance_config_enabled_flag,
};

use crate::{
    driver::{ProviderDriver, ProviderDriverCreateInput, ProviderInstance, ProviderInstanceScope},
    rt::BoxStream,
    server_provider::ServerProvider,
    unavailable_provider_snapshot::{
        UnavailableProviderSnapshotInput, build_unavailable_provider_snapshot,
    },
};

struct LiveEntry {
    instance: Arc<ProviderInstance>,
    scope: Arc<ProviderInstanceScope>,
    entry: ProviderInstanceConfig,
}

struct ChangeBus {
    subscribers: parking_lot::Mutex<Vec<mpsc::UnboundedSender<()>>>,
}

impl ChangeBus {
    fn new() -> Self {
        Self {
            subscribers: parking_lot::Mutex::new(Vec::new()),
        }
    }

    fn subscribe(&self) -> mpsc::UnboundedReceiver<()> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.subscribers.lock().push(sender);
        receiver
    }

    fn publish(&self) {
        self.subscribers
            .lock()
            .retain(|sender| sender.send(()).is_ok());
    }
}

pub struct ProviderInstanceRegistry {
    drivers: HashMap<ProviderDriverKind, Arc<dyn ProviderDriver>>,
    entries: parking_lot::Mutex<IndexMap<ProviderInstanceId, LiveEntry>>,
    unavailable: parking_lot::Mutex<IndexMap<ProviderInstanceId, ServerProvider>>,
    changes: ChangeBus,
    reconcile_lock: Mutex<()>,
}

impl ProviderInstanceRegistry {
    pub async fn new(
        drivers: Vec<Arc<dyn ProviderDriver>>,
        config_map: ProviderInstanceConfigMap,
    ) -> Arc<Self> {
        let mut by_id = HashMap::new();
        for driver in drivers {
            by_id.insert(driver.driver_kind().clone(), driver);
        }
        let registry = Arc::new(Self {
            drivers: by_id,
            entries: parking_lot::Mutex::new(IndexMap::new()),
            unavailable: parking_lot::Mutex::new(IndexMap::new()),
            changes: ChangeBus::new(),
            reconcile_lock: Mutex::new(()),
        });
        registry.reconcile(config_map).await;
        registry
    }

    pub fn get_instance(&self, instance_id: &ProviderInstanceId) -> Option<Arc<ProviderInstance>> {
        self.entries
            .lock()
            .get(instance_id)
            .map(|entry| Arc::clone(&entry.instance))
    }

    pub fn list_instances(&self) -> Vec<Arc<ProviderInstance>> {
        self.entries
            .lock()
            .values()
            .map(|entry| Arc::clone(&entry.instance))
            .collect()
    }

    pub fn list_unavailable(&self) -> Vec<ServerProvider> {
        self.unavailable.lock().values().cloned().collect()
    }

    /// Registers the subscription before returning, so a publish that happens
    /// after this call cannot land in the gap upstream's deferred
    /// `Stream.fromPubSub` subscribe had.
    pub fn subscribe_changes(&self) -> mpsc::UnboundedReceiver<()> {
        self.changes.subscribe()
    }

    pub fn stream_changes(&self) -> BoxStream<'static, ()> {
        Box::pin(UnboundedReceiverStream::new(self.subscribe_changes()))
    }

    pub async fn reconcile(self: &Arc<Self>, config_map: ProviderInstanceConfigMap) {
        let _guard = self.reconcile_lock.lock().await;
        let previous_entries = {
            let entries = self.entries.lock();
            entries
                .iter()
                .map(|(id, entry)| {
                    (
                        id.clone(),
                        entry.entry.clone(),
                        Arc::clone(&entry.scope),
                        Arc::clone(&entry.instance),
                    )
                })
                .collect::<Vec<_>>()
        };
        let previous_unavailable = self.unavailable.lock().clone();
        let next_keys: std::collections::HashSet<&ProviderInstanceId> = config_map.keys().collect();

        let mut removed_ids = Vec::new();
        let mut replaced_ids = Vec::new();
        for (instance_id, entry, _, _) in &previous_entries {
            if !next_keys.contains(instance_id) {
                removed_ids.push(instance_id.clone());
                continue;
            }
            if let Some(next) = config_map.get(instance_id)
                && next != entry
            {
                replaced_ids.push(instance_id.clone());
            }
        }

        for instance_id in removed_ids.iter().chain(replaced_ids.iter()) {
            if let Some((_, _, scope, _)) = previous_entries
                .iter()
                .find(|(id, _, _, _)| id == instance_id)
            {
                scope.close().await;
            }
        }

        let previous_by_id: HashMap<_, _> = previous_entries
            .iter()
            .map(|(id, _, scope, instance)| (id.clone(), (Arc::clone(scope), Arc::clone(instance))))
            .collect();
        let previous_entry_by_id: HashMap<_, _> = previous_entries
            .iter()
            .map(|(id, entry, _, _)| (id.clone(), entry.clone()))
            .collect();

        let mut built_entries = IndexMap::new();
        let mut built_unavailable = IndexMap::new();
        let mut next_order = Vec::new();

        for (instance_id, entry) in &config_map {
            next_order.push(instance_id.clone());
            let replaced = replaced_ids.iter().any(|id| id == instance_id);
            if let Some((scope, instance)) = previous_by_id.get(instance_id)
                && !replaced
                && let Some(previous_entry) = previous_entry_by_id.get(instance_id)
            {
                built_entries.insert(
                    instance_id.clone(),
                    LiveEntry {
                        instance: Arc::clone(instance),
                        scope: Arc::clone(scope),
                        entry: previous_entry.clone(),
                    },
                );
                continue;
            }
            match self.build_entry(instance_id, entry).await {
                BuiltEntry::Live(live) => {
                    built_entries.insert(instance_id.clone(), live);
                }
                BuiltEntry::Unavailable(snapshot) => {
                    built_unavailable.insert(instance_id.clone(), snapshot);
                }
            }
        }

        let previous_order: Vec<_> = previous_entries
            .iter()
            .map(|(id, _, _, _)| id.clone())
            .collect();
        let order_changed = previous_order != next_order;
        let entries_changed = order_changed
            || !removed_ids.is_empty()
            || !replaced_ids.is_empty()
            || built_entries.len() != previous_entries.len();
        let unavailable_changed = built_unavailable.len() != previous_unavailable.len()
            || built_unavailable.iter().any(|(id, snapshot)| {
                previous_unavailable
                    .get(id)
                    .is_none_or(|previous| previous != snapshot)
            })
            || previous_unavailable
                .keys()
                .any(|id| !built_unavailable.contains_key(id));

        *self.entries.lock() = built_entries;
        *self.unavailable.lock() = built_unavailable;
        if entries_changed || unavailable_changed {
            self.changes.publish();
        }
    }

    async fn build_entry(
        &self,
        instance_id: &ProviderInstanceId,
        entry: &ProviderInstanceConfig,
    ) -> BuiltEntry {
        let Some(driver) = self.drivers.get(&entry.driver) else {
            return BuiltEntry::Unavailable(self.shadow(
                instance_id,
                entry,
                format!("Driver '{}' is not registered in this build.", entry.driver),
            ));
        };

        let raw = match &entry.config {
            None | Some(Value::Null) => driver.default_config(),
            Some(value) => value.clone(),
        };
        let typed = match driver.decode_config(&raw) {
            Ok(typed) => typed,
            Err(detail) => {
                error!(
                    instance_id = instance_id.as_str(),
                    driver = entry.driver.as_str(),
                    detail,
                    "Failed to decode provider instance config"
                );
                return BuiltEntry::Unavailable(self.shadow(
                    instance_id,
                    entry,
                    format!("Invalid config for instance '{instance_id}': {detail}"),
                ));
            }
        };

        let scope = ProviderInstanceScope::new();
        let enabled = resolve_entry_enabled(entry, &typed);
        match driver
            .create(ProviderDriverCreateInput {
                instance_id: instance_id.clone(),
                display_name: entry.display_name.clone(),
                accent_color: entry.accent_color.clone(),
                environment: entry.environment.clone().unwrap_or_default(),
                enabled,
                config: typed,
                scope: Arc::clone(&scope),
            })
            .await
        {
            Ok(instance) => BuiltEntry::Live(LiveEntry {
                instance: Arc::new(instance),
                scope,
                entry: entry.clone(),
            }),
            Err(error) => {
                error!(
                    instance_id = instance_id.as_str(),
                    driver = entry.driver.as_str(),
                    detail = error.detail,
                    "Failed to create provider instance"
                );
                scope.close().await;
                BuiltEntry::Unavailable(self.shadow(
                    instance_id,
                    entry,
                    format!(
                        "Driver '{}' failed to create instance: {}",
                        entry.driver, error.detail
                    ),
                ))
            }
        }
    }

    fn shadow(
        &self,
        instance_id: &ProviderInstanceId,
        entry: &ProviderInstanceConfig,
        reason: String,
    ) -> ServerProvider {
        build_unavailable_provider_snapshot(UnavailableProviderSnapshotInput {
            driver_kind: entry.driver.clone(),
            instance_id: instance_id.clone(),
            display_name: entry
                .display_name
                .as_ref()
                .map(|name| name.as_str().to_owned()),
            accent_color: entry
                .accent_color
                .as_ref()
                .map(|color| color.as_str().to_owned()),
            reason,
            checked_at: None,
        })
    }
}

#[allow(clippy::large_enum_variant)]
enum BuiltEntry {
    Live(LiveEntry),
    Unavailable(ServerProvider),
}

/// An explicit false on either the envelope or the raw config wins. Otherwise
/// the envelope flag, then the decoded config's flag, then enabled.
fn resolve_entry_enabled(entry: &ProviderInstanceConfig, typed_config: &Value) -> bool {
    let raw_enabled = provider_instance_config_enabled_flag(entry.config.as_ref());
    if entry.enabled == Some(false) || raw_enabled == Some(false) {
        return false;
    }
    entry
        .enabled
        .or_else(|| provider_instance_config_enabled_flag(Some(typed_config)))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        driver::{
            ProviderDriverMetadata, default_provider_continuation_identity, driver_error,
            test_support::{StubAdapter, StubSnapshot, StubText, stub_provider},
        },
        provider_adapter::ProviderAdapterV2,
        snapshot::ManagedServerProvider,
        text_generation::ProviderTextGeneration,
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use turin_contracts::TrimmedNonEmptyString;

    struct FakeDriver {
        kind: ProviderDriverKind,
        fail: AtomicBool,
        created: Arc<AtomicUsize>,
        closed: Arc<AtomicUsize>,
        metadata: ProviderDriverMetadata,
    }

    #[async_trait::async_trait]
    impl ProviderDriver for FakeDriver {
        fn driver_kind(&self) -> &ProviderDriverKind {
            &self.kind
        }
        fn metadata(&self) -> &ProviderDriverMetadata {
            &self.metadata
        }
        fn default_config(&self) -> Value {
            serde_json::json!({})
        }
        fn decode_config(&self, raw: &Value) -> Result<Value, String> {
            if raw.get("bad").and_then(Value::as_bool) == Some(true) {
                return Err("bad config".to_owned());
            }
            Ok(raw.clone())
        }
        async fn create(
            &self,
            input: ProviderDriverCreateInput,
        ) -> Result<ProviderInstance, crate::errors::ProviderDriverError> {
            self.created.fetch_add(1, Ordering::SeqCst);
            let closed = Arc::clone(&self.closed);
            input.scope.on_close(async move {
                closed.fetch_add(1, Ordering::SeqCst);
            });
            if self.fail.load(Ordering::SeqCst) {
                return Err(driver_error(&self.kind, &input.instance_id, "boom"));
            }
            let provider = stub_provider(&input.instance_id, &self.kind);
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
                snapshot: Arc::new(StubSnapshot { provider }) as Arc<dyn ManagedServerProvider>,
                snapshot_for_cwd: None,
                refresh_models: None,
                invalidate_caches: None,
                consume_reset_credit: None,
                orchestration_adapter: Arc::new(StubAdapter {
                    instance_id: input.instance_id,
                    driver: self.kind.clone(),
                }) as Arc<dyn ProviderAdapterV2>,
                text_generation: Arc::new(StubText) as Arc<dyn ProviderTextGeneration>,
                auth: None,
            })
        }
    }

    fn kind() -> ProviderDriverKind {
        ProviderDriverKind::new("codex").unwrap()
    }

    fn id(value: &str) -> ProviderInstanceId {
        ProviderInstanceId::new(value).unwrap()
    }

    fn entry(enabled: Option<bool>, config: Option<Value>) -> ProviderInstanceConfig {
        ProviderInstanceConfig {
            driver: kind(),
            display_name: Some(TrimmedNonEmptyString::new("Codex").unwrap()),
            accent_color: None,
            environment: None,
            enabled,
            config,
        }
    }

    fn driver(fail: bool) -> (Arc<FakeDriver>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let created = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicUsize::new(0));
        let driver = Arc::new(FakeDriver {
            kind: kind(),
            fail: AtomicBool::new(fail),
            created: Arc::clone(&created),
            closed: Arc::clone(&closed),
            metadata: ProviderDriverMetadata::new("Codex"),
        });
        (driver, created, closed)
    }

    #[tokio::test]
    async fn unknown_driver_is_an_unavailable_shadow() {
        let registry = ProviderInstanceRegistry::new(
            Vec::new(),
            IndexMap::from([(id("codex"), entry(None, None))]),
        )
        .await;
        assert!(registry.list_instances().is_empty());
        let unavailable = registry.list_unavailable();
        assert_eq!(unavailable.len(), 1);
        assert_eq!(unavailable[0].instance_id.as_str(), "codex");
        assert!(
            unavailable[0]
                .unavailable_reason
                .as_ref()
                .unwrap()
                .as_str()
                .contains("not registered")
        );
    }

    #[tokio::test]
    async fn reconcile_keeps_unchanged_instances_and_closes_removed_ones() {
        let (driver, created, closed) = driver(false);
        let mut map = IndexMap::new();
        map.insert(id("codex"), entry(Some(true), None));
        let registry = ProviderInstanceRegistry::new(vec![driver], map.clone()).await;
        assert_eq!(created.load(Ordering::SeqCst), 1);
        assert!(registry.get_instance(&id("codex")).unwrap().enabled);

        let mut changes = registry.subscribe_changes();
        registry.reconcile(map).await;
        assert!(changes.try_recv().is_err());
        assert_eq!(created.load(Ordering::SeqCst), 1);

        registry.reconcile(IndexMap::new()).await;
        assert!(changes.try_recv().is_ok());
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert!(registry.list_instances().is_empty());
    }

    #[tokio::test]
    async fn decode_and_create_failures_become_shadows_and_close_the_scope() {
        let (driver, created, closed) = driver(false);
        let mut map = IndexMap::new();
        map.insert(
            id("codex"),
            entry(None, Some(serde_json::json!({"bad": true}))),
        );
        let registry =
            ProviderInstanceRegistry::new(vec![driver.clone() as Arc<dyn ProviderDriver>], map)
                .await;
        assert_eq!(created.load(Ordering::SeqCst), 0);
        assert!(
            registry.list_unavailable()[0]
                .unavailable_reason
                .as_ref()
                .unwrap()
                .as_str()
                .contains("Invalid config")
        );

        driver.fail.store(true, Ordering::SeqCst);
        let mut map = IndexMap::new();
        map.insert(
            id("codex"),
            entry(None, Some(serde_json::json!({"ok": true}))),
        );
        registry.reconcile(map).await;
        assert_eq!(created.load(Ordering::SeqCst), 1);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert!(registry.list_instances().is_empty());
        assert!(
            registry.list_unavailable()[0]
                .unavailable_reason
                .as_ref()
                .unwrap()
                .as_str()
                .contains("boom")
        );
    }

    #[tokio::test]
    async fn an_explicit_false_wins_over_a_decoded_true() {
        let (driver, _, _) = driver(false);
        let mut map = IndexMap::new();
        map.insert(
            id("codex"),
            entry(Some(false), Some(serde_json::json!({"enabled": true}))),
        );
        let registry = ProviderInstanceRegistry::new(vec![driver], map).await;
        assert!(!registry.get_instance(&id("codex")).unwrap().enabled);
    }

    #[test]
    fn resolve_entry_enabled_is_most_restrictive() {
        let mut config = entry(None, Some(serde_json::json!({"enabled": false})));
        assert!(!resolve_entry_enabled(
            &config,
            &serde_json::json!({"enabled": true})
        ));
        config.enabled = Some(true);
        config.config = None;
        assert!(resolve_entry_enabled(&config, &serde_json::json!({})));
        config.enabled = None;
        assert!(resolve_entry_enabled(
            &config,
            &serde_json::json!({"enabled": true})
        ));
    }
}
