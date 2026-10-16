//! Port of `upstream/packages/provider-cursor/src/server/CursorUsageAccounts.ts`.
//!
//! Cursor's account API is slow, so reads answer from cache and mark themselves
//! `refreshing` while a background refresh runs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use turin_contracts::usage::{UsageSourceAction, UsageSourceStatus};
use turin_provider_core::usage::{ProviderUsageFile, ProviderUsageScan};
use turin_shared::atomic_write::write_file_string_atomically;

use crate::account_cache::{
    CURSOR_ACCOUNT_CACHE_FILE_NAME, CURSOR_ACCOUNT_TTL_MS, CursorAccountCache,
    CursorCredentialSource, cursor_fetch_range, decode_cursor_account_caches,
    encode_cursor_account_caches, is_cursor_cache_fresh, merge_cursor_fetch,
};
use crate::cursor_account_reader::CursorAccountReader;

const CURSOR_ACCOUNT_READ_ERROR: &str = "Cursor account usage could not be read.";

#[derive(Debug, Clone)]
pub struct CursorUsageScanInput {
    pub keychain_usage_enabled: bool,
    pub window_start_ms: f64,
    pub retention_cutoff_ms: f64,
    pub await_refresh: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CursorUsageEnvironment {
    pub home: Option<String>,
    pub userprofile: Option<String>,
    pub xdg_config_home: Option<String>,
    pub appdata: Option<String>,
    pub agent_cli_credential_store: Option<String>,
    pub cursor_auth_token: Option<String>,
    pub cursor_api_key: Option<String>,
    pub platform: CursorPlatform,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CursorPlatform {
    #[default]
    Linux,
    Darwin,
    Win32,
}

#[derive(Clone)]
struct FailureRecord {
    at_ms: f64,
    /// `None` is the upstream `null` error (no login to report).
    error: Option<Option<String>>,
}

struct Inner {
    caches: HashMap<String, CursorAccountCache>,
    cache_dirty: bool,
    failures: HashMap<String, FailureRecord>,
    refreshes: HashMap<String, Arc<Notify>>,
    loaded: bool,
    pending_persists: Vec<JoinHandle<()>>,
}

pub struct CursorUsageAccounts {
    cache_path: PathBuf,
    auth_path: PathBuf,
    environment: CursorUsageEnvironment,
    reader: Arc<CursorAccountReader>,
    inner: Arc<Mutex<Inner>>,
    persist_lock: Arc<Mutex<()>>,
}

impl CursorUsageAccounts {
    pub fn new(
        state_dir: impl AsRef<Path>,
        auth_path: PathBuf,
        environment: CursorUsageEnvironment,
        reader: Arc<CursorAccountReader>,
    ) -> Self {
        Self {
            cache_path: state_dir.as_ref().join(CURSOR_ACCOUNT_CACHE_FILE_NAME),
            auth_path,
            environment,
            reader,
            inner: Arc::new(Mutex::new(Inner {
                caches: HashMap::new(),
                cache_dirty: false,
                failures: HashMap::new(),
                refreshes: HashMap::new(),
                loaded: false,
                pending_persists: Vec::new(),
            })),
            persist_lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn scan(&self, input: CursorUsageScanInput) -> Vec<ProviderUsageScan> {
        let login_unavailable = self.login_unavailable();
        let use_keychain = self.use_keychain();
        if use_keychain && !login_unavailable && !input.keychain_usage_enabled {
            return vec![ProviderUsageScan {
                dir: self.auth_path.to_string_lossy().into_owned(),
                volume_id: Some(String::new()),
                host_id: None,
                status: None,
                message: Some("Cursor account usage is off on this environment.".to_owned()),
                action: Some(UsageSourceAction::EnableCursorKeychain),
                files: None,
                refreshing: false,
            }];
        }
        if login_unavailable {
            return vec![ProviderUsageScan {
                dir: self.auth_path.to_string_lossy().into_owned(),
                volume_id: None,
                host_id: None,
                status: None,
                message: Some(
                    "Cursor account history needs a Cursor CLI login on this server.".to_owned(),
                ),
                action: None,
                files: None,
                refreshing: false,
            }];
        }
        let credential = if use_keychain {
            CursorCredentialSource::Keychain
        } else {
            CursorCredentialSource::File(self.auth_path.clone())
        };
        match self
            .account_source(
                credential,
                input.window_start_ms,
                input.retention_cutoff_ms,
                input.await_refresh,
            )
            .await
        {
            Some(source) => vec![source],
            None => Vec::new(),
        }
    }

    pub async fn await_persisted(&self) {
        let handles = {
            let mut inner = self.inner.lock().await;
            std::mem::take(&mut inner.pending_persists)
        };
        for handle in handles {
            let _ = handle.await;
        }
    }

    fn login_unavailable(&self) -> bool {
        self.environment
            .cursor_auth_token
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            || self
                .environment
                .cursor_api_key
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            || self.environment.agent_cli_credential_store.as_deref() == Some("memory")
    }

    fn use_keychain(&self) -> bool {
        self.environment.platform == CursorPlatform::Darwin
            && self.environment.agent_cli_credential_store.as_deref() != Some("file")
    }

    async fn ensure_loaded(&self) {
        let mut inner = self.inner.lock().await;
        if inner.loaded {
            return;
        }
        inner.loaded = true;
        let document = tokio::fs::read_to_string(&self.cache_path)
            .await
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok());
        if let Some(document) = document {
            for (key, cache) in decode_cursor_account_caches(&document) {
                inner.caches.insert(key, cache);
            }
        }
    }

    async fn schedule_persist(&self) {
        let cache_path = self.cache_path.clone();
        let inner = Arc::clone(&self.inner);
        let persist_lock = Arc::clone(&self.persist_lock);
        let handle = tokio::spawn(async move {
            let _guard = persist_lock.lock().await;
            let contents = {
                let mut state = inner.lock().await;
                if !state.cache_dirty {
                    return;
                }
                state.cache_dirty = false;
                encode_cursor_account_caches(&state.caches)
            };
            let result = tokio::task::spawn_blocking(move || {
                write_file_string_atomically(&cache_path, &contents)
            })
            .await;
            if result.is_err() || result.ok().and_then(Result::ok).is_none() {
                let mut state = inner.lock().await;
                state.cache_dirty = true;
            }
        });
        self.inner.lock().await.pending_persists.push(handle);
    }

    async fn refresh_account(
        &self,
        credential: CursorCredentialSource,
        credential_key: &str,
        retention_start_ms: f64,
    ) {
        let now_ms = js_now_ms();
        let mut base = self.inner.lock().await.caches.get(credential_key).cloned();
        let mut fetched = self
            .fetch_missing(&credential, base.as_ref(), retention_start_ms, now_ms)
            .await;
        if let Some(cache) = &base
            && fetched.account_key.as_deref().is_some()
            && fetched.account_key.as_deref() != Some(cache.account_key.as_str())
        {
            self.inner.lock().await.caches.remove(credential_key);
            self.inner.lock().await.cache_dirty = true;
            base = None;
            fetched = self
                .fetch_missing(&credential, None, retention_start_ms, now_ms)
                .await;
        }
        if fetched.missing || fetched.error.is_some() || fetched.account_key.is_none() {
            let error = if fetched.missing || fetched.error.is_some() {
                Some(fetched.error)
            } else {
                Some(Some(CURSOR_ACCOUNT_READ_ERROR.to_owned()))
            };
            self.inner.lock().await.failures.insert(
                credential_key.to_owned(),
                FailureRecord {
                    at_ms: now_ms,
                    error,
                },
            );
            self.schedule_persist().await;
            return;
        }
        let account_key = fetched.account_key.clone().expect("checked");
        let range = cursor_fetch_range(base.as_ref(), retention_start_ms, now_ms);
        let merged = merge_cursor_fetch(
            base.as_ref(),
            &account_key,
            range,
            &fetched.records,
            now_ms,
            retention_start_ms,
        );
        {
            let mut inner = self.inner.lock().await;
            inner.caches.insert(credential_key.to_owned(), merged.cache);
            inner.failures.remove(credential_key);
            if merged.changed {
                inner.cache_dirty = true;
            }
        }
        self.schedule_persist().await;
    }

    async fn fetch_missing(
        &self,
        credential: &CursorCredentialSource,
        cache: Option<&CursorAccountCache>,
        retention_start_ms: f64,
        now_ms: f64,
    ) -> crate::account_usage::AccountUsageRead {
        let range = cursor_fetch_range(cache, retention_start_ms, now_ms);
        self.reader
            .read(credential.clone(), range.since_ms, range.until_ms)
            .await
    }

    async fn start_refresh(
        &self,
        credential: CursorCredentialSource,
        credential_key: String,
        retention_start_ms: f64,
    ) -> Arc<Notify> {
        let mut inner = self.inner.lock().await;
        if let Some(notify) = inner.refreshes.get(&credential_key) {
            return Arc::clone(notify);
        }
        let done = Arc::new(Notify::new());
        inner
            .refreshes
            .insert(credential_key.clone(), Arc::clone(&done));
        let this = self.clone_for_task();
        let notify = Arc::clone(&done);
        tokio::spawn(async move {
            this.refresh_account(credential, &credential_key, retention_start_ms)
                .await;
            this.inner.lock().await.refreshes.remove(&credential_key);
            notify.notify_waiters();
        });
        done
    }

    fn clone_for_task(&self) -> Self {
        Self {
            cache_path: self.cache_path.clone(),
            auth_path: self.auth_path.clone(),
            environment: self.environment.clone(),
            reader: Arc::clone(&self.reader),
            inner: Arc::clone(&self.inner),
            persist_lock: Arc::clone(&self.persist_lock),
        }
    }

    async fn account_source(
        &self,
        credential: CursorCredentialSource,
        window_start_ms: f64,
        retention_start_ms: f64,
        await_refresh: bool,
    ) -> Option<ProviderUsageScan> {
        if let CursorCredentialSource::File(path) = &credential
            && tokio::fs::metadata(path).await.is_err()
        {
            return None;
        }
        self.ensure_loaded().await;
        let credential_key = match &credential {
            CursorCredentialSource::File(path) => path.to_string_lossy().into_owned(),
            CursorCredentialSource::Keychain => "keychain".to_owned(),
        };
        let now_ms = js_now_ms();
        let recent_failure = self
            .inner
            .lock()
            .await
            .failures
            .get(&credential_key)
            .cloned();
        let mut refreshing = false;
        if recent_failure
            .as_ref()
            .is_none_or(|failure| now_ms - failure.at_ms >= CURSOR_ACCOUNT_TTL_MS)
            && !is_cursor_cache_fresh(self.inner.lock().await.caches.get(&credential_key), now_ms)
        {
            let refresh = self
                .start_refresh(credential, credential_key.clone(), retention_start_ms)
                .await;
            if await_refresh {
                refresh.notified().await;
            } else {
                refreshing = true;
            }
        }
        let inner = self.inner.lock().await;
        let cache = inner.caches.get(&credential_key);
        let failure = if refreshing {
            None
        } else {
            inner.failures.get(&credential_key)
        };
        if matches!(
            failure,
            Some(FailureRecord {
                error: Some(None),
                ..
            })
        ) {
            return None;
        }
        let failure_message = failure.and_then(|failure| failure.error.clone()).flatten();
        if cache.is_none() {
            return Some(ProviderUsageScan {
                dir: self.auth_path.to_string_lossy().into_owned(),
                volume_id: None,
                host_id: None,
                status: None,
                message: if refreshing {
                    None
                } else {
                    Some(failure_message.unwrap_or_else(|| CURSOR_ACCOUNT_READ_ERROR.to_owned()))
                },
                action: None,
                files: if refreshing { Some(Vec::new()) } else { None },
                refreshing,
            });
        }
        let cache = cache.expect("checked");
        let source = format!("cursor-account:{}", cache.account_key);
        Some(ProviderUsageScan {
            dir: source.clone(),
            host_id: Some("cursor.com".to_owned()),
            volume_id: Some(cache.account_key.clone()),
            files: Some(vec![ProviderUsageFile {
                path: source,
                records: cache
                    .records
                    .iter()
                    .filter(|record| record.timestamp_ms >= window_start_ms)
                    .cloned()
                    .map(provider_usage_record)
                    .collect(),
            }]),
            status: if failure_message.is_none() {
                Some(UsageSourceStatus::Ok)
            } else {
                Some(UsageSourceStatus::Partial)
            },
            message: failure_message,
            action: None,
            refreshing,
        })
    }
}

fn provider_usage_record(
    record: crate::account_usage::UsageRecord,
) -> turin_provider_core::usage::UsageRecord {
    turin_provider_core::usage::UsageRecord {
        provider: record.provider,
        timestamp_ms: record.timestamp_ms,
        model: record.model,
        rate_model: Some(record.rate_model),
        session_id: record.session_id,
        totals: record.totals,
        reported_cost_usd: record.reported_cost_usd,
        speed: match record.speed {
            crate::account_usage::UsageSpeed::Standard => {
                turin_provider_core::usage::UsageSpeed::Standard
            }
            crate::account_usage::UsageSpeed::Fast => turin_provider_core::usage::UsageSpeed::Fast,
            crate::account_usage::UsageSpeed::Ultrafast => {
                turin_provider_core::usage::UsageSpeed::Ultrafast
            }
        },
        dedupe_key: Some(record.dedupe_key),
    }
}

pub fn cursor_auth_path(environment: &CursorUsageEnvironment) -> PathBuf {
    let user_home = environment
        .home
        .as_deref()
        .or(environment.userprofile.as_deref())
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/"));
    let cursor_home = match environment.platform {
        CursorPlatform::Darwin => user_home.join("Library/Application Support"),
        CursorPlatform::Win32 => environment
            .appdata
            .as_deref()
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| user_home.join("AppData/Roaming")),
        CursorPlatform::Linux => environment
            .xdg_config_home
            .as_deref()
            .filter(|path| !path.is_empty() && Path::new(path).is_absolute())
            .map(PathBuf::from)
            .unwrap_or_else(|| user_home.join(".config")),
    };
    match environment.platform {
        CursorPlatform::Darwin => user_home.join(".cursor/auth.json"),
        CursorPlatform::Win32 => cursor_home.join("Cursor/auth.json"),
        CursorPlatform::Linux => cursor_home.join("cursor/auth.json"),
    }
}

fn js_now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0)
}
