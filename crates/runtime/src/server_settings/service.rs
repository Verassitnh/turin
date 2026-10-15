//! The file, secret and watch side of upstream `ServerSettingsService`
//! (apps/server/src/serverSettings.ts). Pure pipeline steps live in the parent
//! module and [`legacy`]. This module owns the settings cache, the write
//! lock, secret persist and rollback, the sparse file write, and the change
//! stream.

use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use indexmap::IndexMap;
use notify::RecommendedWatcher;
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio::task::JoinHandle;
use turin_contracts::provider_instance::ProviderInstanceMutation;
use turin_contracts::settings::{
    ServerSettings, ServerSettingsError, ServerSettingsOperation, ServerSettingsPatch,
    default_server_settings,
};
use turin_persistence::Db;
use turin_provider_core::credential_store::SecretStore;
use turin_shared::server_settings::apply_server_settings_patch;

use super::legacy::{
    LegacySourceControl, SecretMove, fold_legacy_project_settings,
    fold_provider_instance_enabled_flags, legacy_source_control, migrate_legacy_provider_settings,
    migrate_legacy_source_control_settings,
};
use super::{
    decode_legacy_file, decode_settings_json, normalize_server_settings,
    resolve_text_generation_provider, sparse_settings_json,
};
use crate::persistence_reader::{
    LegacyProjectSettingsRow, ProviderHistoryRow, legacy_project_settings_rows, provider_history,
};
use crate::settings_secrets::{
    SecretValue, SettingsSecret, SettingsSecretKind, map_settings_secrets,
};
use crate::support::atomic_write::write_file_string_atomically;
use crate::support::file_watch::{WATCH_DEBOUNCE, next_debounced, watch_file};
use crate::support::symlink::resolve_symlink_target;

type Object = serde_json::Map<String, Value>;

fn settings_error(
    path: &Path,
    operation: ServerSettingsOperation,
    provider_instance_id: Option<&str>,
    environment_variable: Option<&str>,
    cause: impl Display,
) -> ServerSettingsError {
    ServerSettingsError::ServerSettingsError {
        settings_path: path.display().to_string(),
        operation,
        provider_instance_id: provider_instance_id.map(str::to_owned),
        environment_variable: environment_variable.map(str::to_owned),
        cause: Some(Value::String(cause.to_string())),
    }
}

/// A secret write made while applying a change, with what it replaced so a
/// later failure can restore it.
struct AppliedWrite {
    name: String,
    previous: Option<Vec<u8>>,
}

/// One secret store operation a save needs.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SecretChange {
    Write {
        name: String,
        provider_instance_id: Option<String>,
        environment_variable: Option<String>,
        value: Vec<u8>,
    },
    Remove {
        name: String,
        provider_instance_id: Option<String>,
        environment_variable: Option<String>,
        operation: ServerSettingsOperation,
    },
}

impl SecretChange {
    fn name(&self) -> &str {
        match self {
            Self::Write { name, .. } | Self::Remove { name, .. } => name,
        }
    }
}

/// `persistSettingsSecrets`. Plans the store writes and removals for a save,
/// and returns the settings to persist, with each credential replaced by an
/// empty redacted placeholder.
fn persist_settings_secrets(
    current: &ServerSettings,
    next: &ServerSettings,
) -> serde_json::Result<(ServerSettings, Vec<SecretChange>)> {
    let mut previous: IndexMap<String, SettingsSecret> = IndexMap::new();
    map_settings_secrets(current, |secret| {
        // Duplicate environment names follow the provider's last-value-wins behavior.
        previous.insert(secret.secret_name.clone(), secret.clone());
        SecretValue {
            value: secret.value.clone(),
            redacted: secret.redacted,
        }
    })?;

    let mut changes: Vec<SecretChange> = Vec::new();
    let mut next_names: HashSet<String> = HashSet::new();
    let settings = map_settings_secrets(next, |secret| {
        let name = secret.secret_name.clone();
        let provider_instance_id = secret.provider_instance_id.clone();
        let environment_variable = secret.environment_variable.clone();
        if !secret.sensitive {
            changes.push(SecretChange::Remove {
                name,
                provider_instance_id,
                environment_variable,
                operation: ServerSettingsOperation::RemoveSecret,
            });
            return SecretValue {
                value: secret.value.clone(),
                redacted: false,
            };
        }
        let inline = previous
            .get(&name)
            .filter(|old| old.sensitive && !old.redacted && !old.value.is_empty())
            .map(|old| old.value.clone());
        let value = if secret.redacted {
            inline
        } else {
            Some(secret.value.clone())
        };
        match value {
            None => {
                next_names.insert(name);
                SecretValue {
                    value: String::new(),
                    redacted: true,
                }
            }
            Some(value) if value.is_empty() => {
                changes.push(SecretChange::Remove {
                    name,
                    provider_instance_id,
                    environment_variable,
                    operation: ServerSettingsOperation::RemoveSecret,
                });
                SecretValue {
                    value: String::new(),
                    redacted: false,
                }
            }
            Some(value) => {
                next_names.insert(name.clone());
                changes.push(SecretChange::Write {
                    name,
                    provider_instance_id,
                    environment_variable,
                    value: value.into_bytes(),
                });
                SecretValue {
                    value: String::new(),
                    redacted: true,
                }
            }
        }
    })?;

    // Credentials that were stored before and are gone now are removed too.
    let stale: Vec<SecretChange> = previous
        .values()
        .filter(|secret| secret.sensitive)
        .filter(|secret| !next_names.contains(&secret.secret_name))
        .filter(|secret| {
            !changes
                .iter()
                .any(|change| change.name() == secret.secret_name)
        })
        .map(|secret| SecretChange::Remove {
            name: secret.secret_name.clone(),
            provider_instance_id: secret.provider_instance_id.clone(),
            environment_variable: secret.environment_variable.clone(),
            operation: ServerSettingsOperation::RemoveStaleSecret,
        })
        .collect();
    changes.extend(stale);
    Ok((settings, changes))
}

/// `applyProviderInstanceMutation`.
fn apply_provider_instance_mutation(
    mut settings: ServerSettings,
    mutation: &ProviderInstanceMutation,
) -> ServerSettings {
    match mutation {
        ProviderInstanceMutation::Create {
            instance_id,
            instance,
        }
        | ProviderInstanceMutation::Upsert {
            instance_id,
            instance,
        } => {
            settings
                .provider_instances
                .insert(instance_id.clone(), instance.clone());
        }
        ProviderInstanceMutation::Remove { instance_id } => {
            settings.provider_instances.shift_remove(instance_id);
        }
    }
    settings
}

/// The settings service. Create with [`ServerSettingsService::new`], then call
/// [`ServerSettingsService::start`] once to attach the file watcher.
pub struct ServerSettingsService {
    settings_path: PathBuf,
    secrets: Arc<dyn SecretStore>,
    db: Db,
    write_lock: AsyncMutex<()>,
    cache: Mutex<Option<ServerSettings>>,
    subscribers: Mutex<Vec<mpsc::UnboundedSender<ServerSettings>>>,
}

/// Owns the settings file watcher and the task that reloads on change. Dropping
/// it stops watching.
pub struct SettingsWatcher {
    _watchers: Vec<RecommendedWatcher>,
    task: JoinHandle<()>,
}

impl Drop for SettingsWatcher {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A change subscription. [`SettingsChanges::recv`] yields settings with their
/// secrets materialized, as upstream `streamChanges` does.
pub struct SettingsChanges {
    service: Arc<ServerSettingsService>,
    receiver: mpsc::UnboundedReceiver<ServerSettings>,
}

impl SettingsChanges {
    pub async fn recv(&mut self) -> Option<ServerSettings> {
        let settings = self.receiver.recv().await?;
        match self.service.materialize(&settings).await {
            Ok(materialized) => Some(resolve_text_generation_provider(materialized)),
            Err(error) => {
                tracing::warn!(error = %error, "failed to materialize settings secrets");
                Some(settings)
            }
        }
    }
}

impl ServerSettingsService {
    pub fn new(
        settings_path: impl Into<PathBuf>,
        secrets: Arc<dyn SecretStore>,
        db: Db,
    ) -> Arc<Self> {
        Arc::new(Self {
            settings_path: settings_path.into(),
            secrets,
            db,
            write_lock: AsyncMutex::new(()),
            cache: Mutex::new(None),
            subscribers: Mutex::new(Vec::new()),
        })
    }

    pub fn settings_path(&self) -> &Path {
        &self.settings_path
    }

    /// Port of `start`: prepare the directory, attach the watcher, and load the
    /// settings once. Must run inside a tokio runtime.
    pub async fn start(self: &Arc<Self>) -> Result<SettingsWatcher, ServerSettingsError> {
        let directory = self
            .settings_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        std::fs::create_dir_all(&directory).map_err(|error| {
            settings_error(
                &self.settings_path,
                ServerSettingsOperation::PrepareDirectory,
                None,
                None,
                error,
            )
        })?;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut watchers = Vec::new();
        watchers.push(
            watch_file(&self.settings_path, tx.clone()).map_err(|error| {
                settings_error(
                    &self.settings_path,
                    ServerSettingsOperation::PrepareDirectory,
                    None,
                    None,
                    error,
                )
            })?,
        );
        // A symlinked settings file is written in its destination's directory,
        // which a watch on the link's directory never sees. Watch the target too.
        if let Ok(target) = resolve_symlink_target(&self.settings_path) {
            if target
                != std::fs::canonicalize(&self.settings_path)
                    .unwrap_or_else(|_| self.settings_path.clone())
                && target != self.settings_path
            {
                if let Some(parent) = target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Ok(watcher) = watch_file(&target, tx.clone()) {
                    watchers.push(watcher);
                }
            }
        }

        let this = Arc::clone(self);
        let task = tokio::spawn(async move {
            while next_debounced(&mut rx, WATCH_DEBOUNCE).await.is_some() {
                // Upstream logs and keeps watching when a reload fails.
                if let Err(error) = this.revalidate_and_emit().await {
                    tracing::warn!(error = %error, "settings reload failed");
                }
            }
        });

        *self.cache.lock() = None;
        self.cached_settings().await?;
        Ok(SettingsWatcher {
            _watchers: watchers,
            task,
        })
    }

    /// Port of `getSettings`: the cached settings with secrets materialized.
    pub async fn get_settings(&self) -> Result<ServerSettings, ServerSettingsError> {
        let settings = self.cached_settings().await?;
        let materialized = self.materialize(&settings).await?;
        Ok(resolve_text_generation_provider(materialized))
    }

    /// Port of `updateSettings`.
    pub async fn update_settings(
        &self,
        patch: &ServerSettingsPatch,
    ) -> Result<ServerSettings, ServerSettingsError> {
        self.update_and_persist(|current| {
            apply_server_settings_patch(&current, patch).map_err(|error| {
                settings_error(
                    &self.settings_path,
                    ServerSettingsOperation::Normalize,
                    None,
                    None,
                    error,
                )
            })
        })
        .await
    }

    /// Port of `updateProviderInstance`: applies `patch` and one instance
    /// mutation against the same latest snapshot.
    pub async fn update_provider_instance(
        &self,
        mutation: &ProviderInstanceMutation,
        patch: Option<&ServerSettingsPatch>,
    ) -> Result<ServerSettings, ServerSettingsError> {
        self.update_and_persist(|current| {
            if let ProviderInstanceMutation::Create { instance_id, .. } = mutation {
                if current.provider_instances.contains_key(instance_id) {
                    return Err(settings_error(
                        &self.settings_path,
                        ServerSettingsOperation::CreateProviderInstance,
                        Some(instance_id.as_str()),
                        None,
                        "provider instance already exists",
                    ));
                }
            }
            let patched = match patch {
                Some(patch) => apply_server_settings_patch(&current, patch).map_err(|error| {
                    settings_error(
                        &self.settings_path,
                        ServerSettingsOperation::Normalize,
                        None,
                        None,
                        error,
                    )
                })?,
                None => current,
            };
            Ok(apply_provider_instance_mutation(patched, mutation))
        })
        .await
    }

    /// Port of `withSettingsSnapshot`: runs `use_fn` on the materialized settings
    /// while writes are paused.
    pub async fn with_settings_snapshot<T>(
        &self,
        use_fn: impl FnOnce(&ServerSettings) -> T,
    ) -> Result<T, ServerSettingsError> {
        let _write = self.write_lock.lock().await;
        let settings = self.cached_settings().await?;
        let materialized = self.materialize(&settings).await?;
        Ok(use_fn(&resolve_text_generation_provider(materialized)))
    }

    /// Port of `streamChanges` (PubSub.unbounded): a subscriber gets its own channel.
    pub fn subscribe(self: &Arc<Self>) -> SettingsChanges {
        let (tx, receiver) = mpsc::unbounded_channel();
        self.subscribers.lock().push(tx);
        SettingsChanges {
            service: Arc::clone(self),
            receiver,
        }
    }

    fn emit(&self, settings: &ServerSettings) {
        self.subscribers
            .lock()
            .retain(|tx| tx.send(settings.clone()).is_ok());
    }

    async fn revalidate_and_emit(&self) -> Result<(), ServerSettingsError> {
        let _write = self.write_lock.lock().await;
        *self.cache.lock() = None;
        let settings = self.cached_settings().await?;
        self.emit(&settings);
        Ok(())
    }

    /// `getSettingsFromCache`. A failed load is not kept, so the next read retries.
    async fn cached_settings(&self) -> Result<ServerSettings, ServerSettingsError> {
        if let Some(settings) = self.cache.lock().clone() {
            return Ok(settings);
        }
        let settings = self.load_settings_from_disk().await?;
        *self.cache.lock() = Some(settings.clone());
        Ok(settings)
    }

    /// `loadSettingsFromDisk`: decode, run the legacy migrations, persist their
    /// result once, and return the settings.
    async fn load_settings_from_disk(&self) -> Result<ServerSettings, ServerSettingsError> {
        let path = self.settings_path.clone();
        let mut settings = default_server_settings();
        let mut legacy_providers: Option<Object> = None;
        let mut legacy_sc = LegacySourceControl::default();
        // A file that failed to decode stays on disk for the user to repair.
        let mut trusted = true;

        if path.try_exists().map_err(|error| {
            settings_error(
                &path,
                ServerSettingsOperation::CheckExists,
                None,
                None,
                error,
            )
        })? {
            let raw = std::fs::read_to_string(&path).map_err(|error| {
                settings_error(&path, ServerSettingsOperation::ReadFile, None, None, error)
            })?;
            let decoded = decode_settings_json(&raw);
            let legacy = decode_legacy_file(&raw);
            if let Ok(legacy_object) = &legacy {
                legacy_providers = super::legacy::legacy_providers(legacy_object);
                legacy_sc = legacy_source_control(legacy_object);
            }
            match (decoded, legacy) {
                (Ok(decoded), Ok(_)) => settings = decoded,
                (decoded, legacy) => {
                    trusted = false;
                    // The file may hold an opt-out we can no longer read: never resume threads on a guess.
                    settings = default_server_settings();
                    settings.continue_threads_after_server_update = false;
                    tracing::warn!(
                        path = %path.display(),
                        decode_error = ?decoded.err(),
                        legacy_error = ?legacy.err(),
                        "failed to parse settings.json, using defaults"
                    );
                }
            }
        }

        let history: Vec<ProviderHistoryRow> =
            provider_history(&self.db).await.map_err(|error| {
                settings_error(
                    &path,
                    ServerSettingsOperation::ReadProviderHistory,
                    None,
                    None,
                    error,
                )
            })?;
        let rows: Vec<LegacyProjectSettingsRow> = if settings.project_settings_folded || !trusted {
            Vec::new()
        } else {
            legacy_project_settings_rows(&self.db)
                .await
                .map_err(|error| {
                    settings_error(
                        &path,
                        ServerSettingsOperation::ReadProjectSettings,
                        None,
                        None,
                        error,
                    )
                })?
        };

        let mut value = serde_json::to_value(&settings).map_err(|error| {
            settings_error(&path, ServerSettingsOperation::Normalize, None, None, error)
        })?;
        if let Some(providers) = &legacy_providers {
            migrate_legacy_provider_settings(&mut value, providers, &history);
        }
        fold_provider_instance_enabled_flags(&mut value);
        let loaded = value.clone();
        if trusted {
            fold_legacy_project_settings(&mut value, &rows);
        }

        // Copy the retired source-control secrets first. The old entries are
        // removed only after the new file is written, so a failed copy retries on the next load.
        let mut secret_moves: Vec<SecretMove> = Vec::new();
        let mut copy_ok = true;
        if trusted {
            let mut candidate = value.clone();
            secret_moves = migrate_legacy_source_control_settings(&mut candidate, &legacy_sc);
        }
        for SecretMove { from, to } in &secret_moves {
            match self.secrets.get(from).await {
                Ok(Some(bytes)) => {
                    if self.secrets.set(to, &bytes).await.is_err() {
                        copy_ok = false;
                        tracing::warn!(secret = %to, "failed to copy a source control secret to its new name");
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    copy_ok = false;
                    tracing::warn!(secret = %to, "failed to copy a source control secret to its new name");
                }
            }
        }
        let has_legacy_source_control =
            trusted && copy_ok && (legacy_sc.bitbucket.is_some() || legacy_sc.github.is_some());
        if has_legacy_source_control {
            migrate_legacy_source_control_settings(&mut value, &legacy_sc);
        } else {
            secret_moves.clear();
        }

        let typed: ServerSettings = serde_json::from_value(value.clone()).map_err(|error| {
            settings_error(&path, ServerSettingsOperation::Normalize, None, None, error)
        })?;
        let migrated = if trusted {
            self.move_inline_settings_secrets(typed).await?
        } else {
            typed
        };
        let migrated_value = serde_json::to_value(&migrated).map_err(|error| {
            settings_error(&path, ServerSettingsOperation::Normalize, None, None, error)
        })?;

        if migrated_value != loaded
            || (trusted && legacy_providers.is_some())
            || has_legacy_source_control
        {
            self.write_settings(&migrated)?;
            for SecretMove { from, .. } in &secret_moves {
                let _ = self.secrets.remove(from).await;
            }
        }
        Ok(migrated)
    }

    /// `moveInlineSettingsSecrets`: moves plaintext field credentials into the
    /// store. A failed move stays usable from the file and is retried next load.
    async fn move_inline_settings_secrets(
        &self,
        settings: ServerSettings,
    ) -> Result<ServerSettings, ServerSettingsError> {
        let mut pending: Vec<SettingsSecret> = Vec::new();
        map_settings_secrets(&settings, |secret| {
            if secret.kind == SettingsSecretKind::Field
                && !secret.redacted
                && !secret.value.is_empty()
            {
                pending.push(secret.clone());
            }
            SecretValue {
                value: secret.value.clone(),
                redacted: secret.redacted,
            }
        })
        .map_err(|error| {
            settings_error(
                &self.settings_path,
                ServerSettingsOperation::Normalize,
                None,
                None,
                error,
            )
        })?;

        let mut moved: HashSet<String> = HashSet::new();
        for secret in pending {
            match self
                .secrets
                .set(&secret.secret_name, secret.value.as_bytes())
                .await
            {
                Ok(()) => {
                    moved.insert(secret.secret_name);
                }
                Err(error) => {
                    tracing::warn!(error = %error, "failed to move a settings secret into the secret store")
                }
            }
        }
        if moved.is_empty() {
            return Ok(settings);
        }
        map_settings_secrets(&settings, |secret| {
            if moved.contains(&secret.secret_name) {
                SecretValue {
                    value: String::new(),
                    redacted: true,
                }
            } else {
                SecretValue {
                    value: secret.value.clone(),
                    redacted: secret.redacted,
                }
            }
        })
        .map_err(|error| {
            settings_error(
                &self.settings_path,
                ServerSettingsOperation::Normalize,
                None,
                None,
                error,
            )
        })
    }

    /// `materializeSettingsSecrets`: fills redacted credentials back in from the
    /// store, so the service hands out full values. Environment variables are
    /// marked redacted for clients.
    async fn materialize(
        &self,
        settings: &ServerSettings,
    ) -> Result<ServerSettings, ServerSettingsError> {
        let mut pending: IndexMap<String, SettingsSecret> = IndexMap::new();
        map_settings_secrets(settings, |secret| {
            if secret.sensitive && secret.redacted {
                pending.insert(secret.secret_name.clone(), secret.clone());
            }
            SecretValue {
                value: secret.value.clone(),
                redacted: secret.redacted,
            }
        })
        .map_err(|error| {
            settings_error(
                &self.settings_path,
                ServerSettingsOperation::Normalize,
                None,
                None,
                error,
            )
        })?;

        let mut stored: HashMap<String, String> = HashMap::new();
        for secret in pending.values() {
            let value = self
                .secrets
                .get(&secret.secret_name)
                .await
                .map_err(|error| {
                    settings_error(
                        &self.settings_path,
                        ServerSettingsOperation::ReadSecret,
                        secret.provider_instance_id.as_deref(),
                        secret.environment_variable.as_deref(),
                        error,
                    )
                })?;
            stored.insert(
                secret.secret_name.clone(),
                value
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .unwrap_or_default(),
            );
        }

        map_settings_secrets(settings, |secret| {
            let value = if secret.sensitive && secret.redacted {
                stored.get(&secret.secret_name).cloned().unwrap_or_default()
            } else {
                secret.value.clone()
            };
            SecretValue {
                value,
                redacted: secret.sensitive
                    && secret.kind == SettingsSecretKind::Environment
                    && secret.redacted,
            }
        })
        .map_err(|error| {
            settings_error(
                &self.settings_path,
                ServerSettingsOperation::Normalize,
                None,
                None,
                error,
            )
        })
    }

    /// Writes the sparse settings file atomically.
    fn write_settings(&self, settings: &ServerSettings) -> Result<(), ServerSettingsError> {
        let text = sparse_settings_json(settings).map_err(|error| {
            settings_error(
                &self.settings_path,
                ServerSettingsOperation::WriteFile,
                None,
                None,
                error,
            )
        })?;
        write_file_string_atomically(&self.settings_path, &text).map_err(|error| {
            settings_error(
                &self.settings_path,
                ServerSettingsOperation::WriteFile,
                None,
                None,
                error,
            )
        })
    }

    /// Runs a store change, recording what each write replaced. A later failure
    /// restores them in reverse.
    async fn apply_secret_changes(
        &self,
        changes: &[SecretChange],
    ) -> Result<Vec<AppliedWrite>, ServerSettingsError> {
        let mut applied: Vec<AppliedWrite> = Vec::new();
        for change in changes {
            let name = change.name().to_owned();
            let (provider, env) = match change {
                SecretChange::Write {
                    provider_instance_id,
                    environment_variable,
                    ..
                }
                | SecretChange::Remove {
                    provider_instance_id,
                    environment_variable,
                    ..
                } => (
                    provider_instance_id.as_deref(),
                    environment_variable.as_deref(),
                ),
            };
            let previous = match self.secrets.get(&name).await {
                Ok(previous) => previous,
                Err(error) => {
                    self.rollback_secret_writes(applied).await;
                    return Err(settings_error(
                        &self.settings_path,
                        ServerSettingsOperation::ReadSecret,
                        provider,
                        env,
                        error,
                    ));
                }
            };
            applied.push(AppliedWrite {
                name: name.clone(),
                previous,
            });
            let result = match change {
                SecretChange::Write { value, .. } => self.secrets.set(&name, value).await,
                SecretChange::Remove { .. } => self.secrets.remove(&name).await,
            };
            if let Err(error) = result {
                let operation = match change {
                    SecretChange::Write { .. } => ServerSettingsOperation::WriteSecret,
                    SecretChange::Remove { operation, .. } => *operation,
                };
                self.rollback_secret_writes(applied).await;
                return Err(settings_error(
                    &self.settings_path,
                    operation,
                    provider,
                    env,
                    error,
                ));
            }
        }
        Ok(applied)
    }

    async fn rollback_secret_writes(&self, applied: Vec<AppliedWrite>) {
        for write in applied.into_iter().rev() {
            let result = match &write.previous {
                Some(bytes) => self.secrets.set(&write.name, bytes).await,
                None => self.secrets.remove(&write.name).await,
            };
            if let Err(error) = result {
                tracing::warn!(secret = %write.name, error = %error, "failed to roll back settings secret");
            }
        }
    }

    /// `updateAndPersistSettings`. Under the write lock: plans the secret
    /// changes, normalizes, applies the changes, materializes, writes the file,
    /// and publishes the change. Any failure after the first secret write rolls
    /// the store back.
    async fn update_and_persist(
        &self,
        update: impl FnOnce(ServerSettings) -> Result<ServerSettings, ServerSettingsError>,
    ) -> Result<ServerSettings, ServerSettingsError> {
        let _write = self.write_lock.lock().await;
        let current = self.cached_settings().await?;
        let updated = update(current.clone())?;
        let (persisted, changes) =
            persist_settings_secrets(&current, &updated).map_err(|error| {
                settings_error(
                    &self.settings_path,
                    ServerSettingsOperation::Normalize,
                    None,
                    None,
                    error,
                )
            })?;
        let next = normalize_server_settings(persisted).map_err(|error| {
            settings_error(
                &self.settings_path,
                ServerSettingsOperation::Normalize,
                None,
                None,
                error,
            )
        })?;

        let applied = self.apply_secret_changes(&changes).await?;
        let materialized = match self.materialize(&next).await {
            Ok(materialized) => materialized,
            Err(error) => {
                self.rollback_secret_writes(applied).await;
                return Err(error);
            }
        };
        if let Err(error) = self.write_settings(&next) {
            self.rollback_secret_writes(applied).await;
            return Err(error);
        }
        *self.cache.lock() = Some(next.clone());
        self.emit(&next);
        Ok(resolve_text_generation_provider(materialized))
    }
}
