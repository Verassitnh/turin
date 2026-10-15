//! Port of upstream/apps/server/src/usage/UsageService.ts.
//!
//! Reads every driver's usage history and returns priced usage buckets.
//!
//! Each provider with a usage reader contributes its sources, including work
//! driven outside T3 Code. A `Transcripts` reader names JSONL directories that
//! this service streams itself; a `Scan` reader reads its own sources.
//!
//! JSONL transcripts are append-only, so parsed records are memoised per file by
//! `(size, mtime)`. Warm scans only reparse files that changed, and a file that
//! merely grew resumes from its cached parse position so only the appended bytes
//! are read.
//!
//! Deviations (see PORTING.md, N4): the readers arrive as values from the
//! composition root instead of being looked up from built-in drivers, and the
//! caller passes the settings snapshot into `read_summary`. Driver config is
//! handed to readers undecoded.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use tokio::sync::{OnceCell, watch};
use tokio::task::JoinHandle;
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::settings::ServerSettings;
use turin_contracts::usage::{
    USAGE_CONTRACT_VERSION, UsageDay, UsagePricing, UsagePricingStatus, UsageProviderKind,
    UsageReadError, UsageReadErrorReason, UsageResolution, UsageSource, UsageSourceAction,
    UsageSourceFingerprint, UsageSourceStatus, UsageSummary, UsageSummaryInput,
};
use turin_provider_core::host_process::ProcessEnv;
use turin_provider_core::instance_environment::merge_provider_instance_environment;
use turin_provider_core::usage::{
    ProviderUsageInstance, ProviderUsageReader, ProviderUsageScanInput, TranscriptDirectory,
    TranscriptUsageFormat, UsageRecord,
};

use crate::usage_aggregation::{
    AggregateOptions, UsageAggregator, provider_name, resolve_model_aliases,
};
use crate::usage_pricing::{RateTable, create_override_rate_table, parse_rate_table};
use crate::usage_scan_cache::{
    CachedFile, LEGACY_SCAN_CACHE_FILE_NAME, SCAN_CACHE_FILE_NAME, ScanCache, decode_scan_cache,
    dedupe_within_file, prune_scan_cache, write_scan_cache,
};
use crate::usage_transcript_reader::{
    TranscriptFile, TranscriptParsePosition, list_transcript_files, read_directory_volume_id,
    read_transcript_records,
};

pub const LITELLM_RATES_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const RATES_CACHE_FILE_NAME: &str = "usage-model-rates.json";
const RATES_FETCH_TIMEOUT: Duration = Duration::from_secs(10);

const DAY_MS: f64 = 24.0 * 60.0 * 60.0 * 1000.0;
/// Rates move rarely; a day-old table keeps the page working offline.
const RATES_TTL_MS: f64 = DAY_MS;
/// An explicit refresh ignores the TTL, but not a table fetched this recently.
const RATES_REFRESH_FLOOR_MS: f64 = 60.0 * 1000.0;
/// Files are filtered by mtime before opening. The slack covers a session whose
/// last write lands just before local midnight on the window's first day.
const MTIME_SLACK_MS: f64 = 36.0 * 60.0 * 60.0 * 1000.0;
const MAX_HOURLY_WINDOW_MS: f64 = DAY_MS;
/// The longest window the UI offers, 90 days, plus its `MTIME_SLACK_MS`, rounded
/// up. Older entries are pruned.
const CACHE_RETENTION_DAYS: f64 = 92.0;
/// Transcripts parsed at once. More gains little once the disk stays busy.
const TRANSCRIPT_READ_CONCURRENCY: usize = 4;

type SummaryResult = Result<UsageSummary, UsageReadError>;

/// One provider reader and the driver whose instances it reads.
pub struct UsageReader {
    pub driver_kind: String,
    pub reader: ProviderUsageReader,
}

pub struct UsageServiceConfig {
    pub state_dir: PathBuf,
    pub readers: Vec<UsageReader>,
    /// The server's own environment, merged under each instance's overrides.
    pub host_env: ProcessEnv,
    pub home_dir: String,
    /// Where the LiteLLM rate table is fetched from. Tests point this at a dead port.
    pub rates_url: String,
}

/// A transcript directory a reader points at, with its resolved identity.
struct TranscriptSource {
    reader: usize,
    provider: UsageProviderKind,
    dir: String,
    volume_id: String,
    file_name: Option<String>,
}

/// On-disk identity of a transcript directory, so the source keeps its
/// fingerprint across root cleanup.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedSource {
    dir: String,
    volume_id: String,
}

struct State {
    file_cache: ScanCache,
    source_cache: IndexMap<String, CachedSource>,
    cache_dirty: bool,
    rates: RateTable,
    rates_fetched_at_ms: Option<f64>,
    rates_status: UsagePricingStatus,
}

/// One directory's walk and parse, before rates are involved.
struct ScannedDir {
    provider: UsageProviderKind,
    dir: String,
    volume_id: String,
    host_id: Option<String>,
    status: Option<UsageSourceStatus>,
    message: Option<String>,
    action: Option<UsageSourceAction>,
    /// Parsed records per file, or `None` when the directory does not exist.
    files: Option<Vec<(String, Vec<UsageRecord>)>>,
    /// Answered from a cache while a refresh runs.
    refreshing: bool,
}

/// What a read of one transcript yields, and the cache entry it would store.
struct FileRead {
    path: String,
    records: Vec<UsageRecord>,
    failed: bool,
    update: Option<CacheUpdate>,
}

struct CacheUpdate {
    entry: CachedFile,
    /// `(size, mtime)` of the entry this one supersedes, `None` for a new file.
    replaces: Option<(i64, f64)>,
}

struct Inner {
    config: UsageServiceConfig,
    http: reqwest::Client,
    state: Mutex<State>,
    /// One rate fetch at a time; a burst of refreshes waits on the first fetch.
    rates_lock: tokio::sync::Mutex<()>,
    /// Serialises scan-cache writes so an older snapshot cannot land after a newer one.
    persist_lock: tokio::sync::Mutex<()>,
    scan_cache_loaded: OnceCell<()>,
    /// In-flight scans by request key, so identical concurrent reads share one scan.
    inflight: Mutex<HashMap<String, watch::Receiver<Option<SummaryResult>>>>,
    persists: Mutex<Vec<JoinHandle<()>>>,
}

/// The usage page's read model. Cheap to clone; clones share all caches.
#[derive(Clone)]
pub struct UsageService {
    inner: Arc<Inner>,
}

fn now_ms() -> f64 {
    Utc::now().timestamp_millis() as f64
}

/// `Date#toISOString` for an instant in epoch milliseconds.
fn iso_millis(timestamp_ms: f64) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms as i64)
        .map(|instant| instant.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_default()
}

fn parse_instant(text: &str) -> Option<f64> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|parsed| parsed.timestamp_millis() as f64)
}

fn trimmed(value: impl AsRef<str>) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value.as_ref())
        .unwrap_or_else(|_| TrimmedNonEmptyString::new("unknown").expect("fallback is non-empty"))
}

fn invalid_window(detail: impl AsRef<str>) -> UsageReadError {
    UsageReadError::ReadError {
        reason: UsageReadErrorReason::InvalidWindow,
        detail: trimmed(detail),
        cause: None,
    }
}

fn scan_failed(detail: impl AsRef<str>) -> UsageReadError {
    UsageReadError::ReadError {
        reason: UsageReadErrorReason::ScanFailed,
        detail: trimmed(detail),
        cause: None,
    }
}

/// Whether `a` read a later state of its file than `b`. Transcripts only grow.
fn is_later_read(a: &CachedFile, b: &CachedFile) -> bool {
    a.mtime_ms > b.mtime_ms || (a.mtime_ms == b.mtime_ms && a.size > b.size)
}

fn is_within_directory(file_path: &str, dir: &str) -> bool {
    Path::new(file_path).starts_with(dir)
}

/// Sessions of shared-session providers with records in more than one file,
/// such as a rollout that moved after it was read. Only these need cross-file
/// dedupe keys: within one file the occurrence count already keeps every key
/// unique.
fn shared_sessions(
    files: &[&[UsageRecord]],
    shared_providers: &HashSet<UsageProviderKind>,
) -> HashSet<String> {
    let mut first_file: HashMap<&str, usize> = HashMap::new();
    let mut shared = HashSet::new();
    for (index, records) in files.iter().enumerate() {
        let mut previous = "";
        for record in *records {
            let session = record.session_id.as_str();
            if !shared_providers.contains(&record.provider)
                || session == previous
                || session.is_empty()
            {
                continue;
            }
            previous = session;
            match first_file.get(session) {
                None => {
                    first_file.insert(session, index);
                }
                Some(first) if *first != index => {
                    shared.insert(session.to_owned());
                }
                Some(_) => {}
            }
        }
    }
    shared
}

fn scan_key(input: &UsageSummaryInput, settings: &ServerSettings) -> String {
    serde_json::to_string(&json!([
        input.time_zone.as_str(),
        input.since_day.as_str(),
        input.until_day.as_str(),
        input.resolution.unwrap_or(UsageResolution::Day),
        input.since_time.as_ref().map(|value| value.as_str()),
        input.until_time.as_ref().map(|value| value.as_str()),
        settings,
        // A waiting read must never share a scan that answers with `refreshing`.
        input.await_refresh == Some(true),
    ]))
    .unwrap_or_default()
}

/// Writes through a temp file and a rename, so a crash mid-write leaves the old file.
async fn write_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    tokio::fs::write(&temp, contents).await?;
    tokio::fs::rename(&temp, path).await
}

impl UsageService {
    pub fn new(config: UsageServiceConfig) -> Self {
        let state = State {
            file_cache: ScanCache::new(),
            source_cache: IndexMap::new(),
            cache_dirty: false,
            rates: RateTable::new(),
            rates_fetched_at_ms: None,
            rates_status: UsagePricingStatus::Unavailable,
        };
        Self {
            inner: Arc::new(Inner {
                config,
                http: reqwest::Client::new(),
                state: Mutex::new(state),
                rates_lock: tokio::sync::Mutex::new(()),
                persist_lock: tokio::sync::Mutex::new(()),
                scan_cache_loaded: OnceCell::new(),
                inflight: Mutex::new(HashMap::new()),
                persists: Mutex::new(Vec::new()),
            }),
        }
    }

    fn scan_cache_path(&self) -> PathBuf {
        self.inner.config.state_dir.join(SCAN_CACHE_FILE_NAME)
    }

    fn legacy_scan_cache_path(&self) -> PathBuf {
        self.inner
            .config
            .state_dir
            .join(LEGACY_SCAN_CACHE_FILE_NAME)
    }

    fn rates_cache_path(&self) -> PathBuf {
        self.inner.config.state_dir.join(RATES_CACHE_FILE_NAME)
    }

    fn transcript_format(&self, reader: usize) -> &TranscriptUsageFormat {
        match &self.inner.config.readers[reader].reader {
            ProviderUsageReader::Transcripts { format, .. } => format,
            ProviderUsageReader::Scan { .. } => {
                unreachable!("transcript index points at a scan reader")
            }
        }
    }

    fn transcript_format_for(&self, provider: UsageProviderKind) -> Option<&TranscriptUsageFormat> {
        self.inner
            .config
            .readers
            .iter()
            .find_map(|reader| match &reader.reader {
                ProviderUsageReader::Transcripts {
                    provider: candidate,
                    format,
                    ..
                } if *candidate == provider => Some(format),
                _ => None,
            })
    }

    /// Providers whose format lets one session's records appear in several files.
    fn shared_session_providers(&self) -> HashSet<UsageProviderKind> {
        self.inner
            .config
            .readers
            .iter()
            .filter_map(|reader| match &reader.reader {
                ProviderUsageReader::Transcripts {
                    provider, format, ..
                } if format.shared_sessions_across_files => Some(*provider),
                _ => None,
            })
            .collect()
    }

    fn pricing(&self) -> UsagePricing {
        let state = self.inner.state.lock();
        UsagePricing {
            status: state.rates_status,
            source: trimmed(LITELLM_RATES_URL),
            fetched_at: state.rates_fetched_at_ms.map(iso_millis),
            known_models: state.rates.len() as u64,
        }
    }

    /// Loads the LiteLLM rate table, preferring a fresh copy and falling back to
    /// the on-disk snapshot. With neither, every model reports as unpriced rather
    /// than the page failing. `force` refetches inside the TTL so a model that
    /// LiteLLM added since the last fetch gets priced now.
    async fn load_rates(&self, force: bool) {
        let now = now_ms();
        let max_age = if force {
            RATES_REFRESH_FLOOR_MS
        } else {
            RATES_TTL_MS
        };
        let fetched_at = self.inner.state.lock().rates_fetched_at_ms;
        if fetched_at.is_some_and(|fetched_at| now - fetched_at < max_age) {
            return;
        }

        if fetched_at.is_none()
            && let Some((disk_fetched_at, document)) = self.read_rates_cache().await
        {
            let parsed = parse_rate_table(&document);
            if !parsed.is_empty() {
                let mut state = self.inner.state.lock();
                state.rates = parsed;
                state.rates_fetched_at_ms = Some(disk_fetched_at);
                state.rates_status = UsagePricingStatus::Cached;
                drop(state);
                if now - disk_fetched_at < max_age {
                    return;
                }
            }
        }

        let Some(document) = self.fetch_rates().await else {
            // The refresh failed; whatever we are serving is now past its TTL and
            // must not keep claiming to be fresh.
            let mut state = self.inner.state.lock();
            if !state.rates.is_empty() {
                state.rates_status = UsagePricingStatus::Cached;
            }
            return;
        };

        let parsed = parse_rate_table(&document);
        if parsed.is_empty() {
            return;
        }
        {
            let mut state = self.inner.state.lock();
            state.rates = parsed;
            state.rates_fetched_at_ms = Some(now);
            state.rates_status = UsagePricingStatus::Fresh;
        }
        let contents = json!({ "fetchedAtMs": now, "document": document }).to_string();
        // A rates cache we cannot write is a slower next start, not a failed read.
        let _ = write_atomically(&self.rates_cache_path(), &contents).await;
    }

    async fn read_rates_cache(&self) -> Option<(f64, Value)> {
        let raw = tokio::fs::read_to_string(self.rates_cache_path())
            .await
            .ok()?;
        let parsed: Value = serde_json::from_str(&raw).ok()?;
        let fetched_at = parsed.get("fetchedAtMs")?.as_f64()?;
        let document = parsed.get("document")?.clone();
        Some((fetched_at, document))
    }

    async fn fetch_rates(&self) -> Option<Value> {
        let response = self
            .inner
            .http
            .get(&self.inner.config.rates_url)
            .timeout(RATES_FETCH_TIMEOUT)
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.json::<Value>().await.ok()
    }

    async fn ensure_rates(&self, force: bool) {
        let _guard = self.inner.rates_lock.lock().await;
        self.load_rates(force).await;
    }

    /// Refetches the rate table ahead of its TTL. See `ensure_rates`.
    pub async fn refresh_rates(&self) -> UsagePricing {
        self.ensure_rates(true).await;
        self.pricing()
    }

    /// Every instance of the driver `driver_kind`, with its settings overrides
    /// merged over the server environment. Disabled accounts still have history.
    /// An unconfigured default slot runs with default config, as the registry does.
    fn usage_instances(
        &self,
        driver_kind: &str,
        settings: &ServerSettings,
    ) -> Vec<ProviderUsageInstance> {
        let home = self.inner.config.home_dir.as_str();
        let mut instances = Vec::new();
        for (instance_id, instance) in &settings.provider_instances {
            if instance.driver.as_str() != driver_kind {
                continue;
            }
            instances.push(ProviderUsageInstance {
                instance_id: instance_id.clone(),
                config: instance.config.clone(),
                environment: merge_provider_instance_environment(
                    instance.environment.as_ref(),
                    &self.inner.config.host_env,
                    home,
                ),
                configured: true,
            });
        }
        if let Ok(default_id) = ProviderInstanceId::new(driver_kind)
            && !settings.provider_instances.contains_key(&default_id)
        {
            instances.push(ProviderUsageInstance {
                instance_id: default_id,
                config: Some(json!({})),
                environment: merge_provider_instance_environment(
                    None,
                    &self.inner.config.host_env,
                    home,
                ),
                configured: false,
            });
        }
        instances
    }

    /// Resolves every transcript directory the usage readers point at.
    async fn resolve_transcript_dirs(
        &self,
        settings: &ServerSettings,
        retention_cutoff_ms: f64,
    ) -> Vec<TranscriptSource> {
        let mut dirs = Vec::new();
        let mut seen = HashSet::new();
        for (index, reader) in self.inner.config.readers.iter().enumerate() {
            let ProviderUsageReader::Transcripts {
                provider,
                directories,
                ..
            } = &reader.reader
            else {
                continue;
            };
            let provider = *provider;
            let mut found: Vec<TranscriptDirectory> = Vec::new();
            for instance in self.usage_instances(&reader.driver_kind, settings) {
                found.extend(directories(instance).await);
            }
            for TranscriptDirectory {
                dir: directory,
                file_name,
            } in found
            {
                let source_key = format!("{}\0{}", provider_name(provider), directory);
                let previous = self
                    .inner
                    .state
                    .lock()
                    .source_cache
                    .get(&source_key)
                    .cloned();
                // Keep canonical paths and source fingerprints stable after root cleanup,
                // including aliases and clients merging pre-cleanup environment summaries.
                let dir = match tokio::fs::canonicalize(&directory).await {
                    Ok(path) => path.to_string_lossy().into_owned(),
                    Err(_) => previous
                        .as_ref()
                        .map_or(directory.clone(), |p| p.dir.clone()),
                };
                let current_volume_id = read_directory_volume_id(Path::new(&dir)).await;
                let has_retained_history = {
                    let state = self.inner.state.lock();
                    state.file_cache.iter().any(|(file_path, entry)| {
                        entry.provider == provider
                            && entry.mtime_ms >= retention_cutoff_ms
                            && entry.records.len() + entry.tail_records.len() > 0
                            && is_within_directory(file_path, &dir)
                    })
                };
                // A recreated directory still reports the retained history under its old identity.
                let volume_id = match &previous {
                    Some(previous)
                        if previous.dir == dir
                            && (has_retained_history || current_volume_id.is_empty()) =>
                    {
                        if previous.volume_id.is_empty() {
                            current_volume_id.clone()
                        } else {
                            previous.volume_id.clone()
                        }
                    }
                    _ => current_volume_id.clone(),
                };
                let changed = previous
                    .as_ref()
                    .is_none_or(|previous| previous.dir != dir || previous.volume_id != volume_id);
                if changed {
                    let mut state = self.inner.state.lock();
                    state.source_cache.insert(
                        source_key,
                        CachedSource {
                            dir: dir.clone(),
                            volume_id: volume_id.clone(),
                        },
                    );
                    state.cache_dirty = true;
                }
                if !seen.insert(format!("{}\0{}", provider_name(provider), dir)) {
                    continue;
                }
                dirs.push(TranscriptSource {
                    reader: index,
                    provider,
                    dir,
                    volume_id,
                    file_name,
                });
            }
        }
        dirs
    }

    /// Loads the persisted scan cache once per process. The `OnceCell` makes
    /// concurrent first readers await the same load rather than cold scanning
    /// against an empty cache.
    async fn ensure_scan_cache_loaded(&self) {
        self.inner
            .scan_cache_loaded
            .get_or_init(|| async { self.load_scan_cache().await })
            .await;
    }

    async fn load_scan_cache(&self) {
        let read_document = |path: PathBuf| async move {
            let raw = tokio::fs::read_to_string(path).await.ok()?;
            serde_json::from_str::<Value>(&raw).ok()
        };
        let mut document = read_document(self.scan_cache_path()).await;
        if document.is_none() {
            document = read_document(self.legacy_scan_cache_path()).await;
            // Write the migrated cache to its own file on the next scan.
            if document.is_some() {
                self.inner.state.lock().cache_dirty = true;
            }
        }
        let Some(document) = document else {
            return;
        };
        let entries = decode_scan_cache(&document, |provider| self.transcript_format_for(provider));
        let sources: Option<IndexMap<String, CachedSource>> = document
            .get("sources")
            .and_then(|sources| serde_json::from_value(sources.clone()).ok());
        let mut state = self.inner.state.lock();
        for (path, entry) in entries {
            state.file_cache.insert(path, entry);
        }
        if let Some(sources) = sources {
            for (key, source) in sources {
                state.source_cache.insert(key, source);
            }
        }
    }

    /// Writes the scan cache when it has changed. The dirty flag is cleared before
    /// encoding, so a change during the write marks it dirty again; a failed write
    /// restores it, so the next persist retries.
    async fn persist_caches(&self) {
        let _guard = self.inner.persist_lock.lock().await;
        let contents = {
            let mut state = self.inner.state.lock();
            if !state.cache_dirty {
                return;
            }
            state.cache_dirty = false;
            let mut extra = Map::new();
            extra.insert("sources".to_owned(), json!(state.source_cache));
            write_scan_cache(&state.file_cache, &extra)
        };
        if write_atomically(&self.scan_cache_path(), &contents)
            .await
            .is_err()
        {
            self.inner.state.lock().cache_dirty = true;
        }
    }

    /// Writes dirty caches in the background, after the summary that dirtied them answers.
    fn schedule_persist(&self) {
        let service = self.clone();
        let handle = tokio::spawn(async move { service.persist_caches().await });
        let mut persists = self.inner.persists.lock();
        persists.retain(|pending| !pending.is_finished());
        persists.push(handle);
    }

    /// Waits for every write scheduled so far, as a restart would need.
    pub async fn await_persisted(&self) {
        let handles = std::mem::take(&mut *self.inner.persists.lock());
        for handle in handles {
            let _ = handle.await;
        }
    }

    /// Parses one transcript, reusing the cached result when it is unchanged.
    ///
    /// A file that only grew re-parses from the cached position, so an actively
    /// written multi-hundred-megabyte rollout costs its appended bytes per scan
    /// rather than a full re-read. The reader verifies the position's guard bytes
    /// and silently restarts from byte 0 when they no longer match.
    async fn read_file_records(
        &self,
        reader: usize,
        provider: UsageProviderKind,
        file: &TranscriptFile,
    ) -> FileRead {
        let path = file.path.to_string_lossy().into_owned();
        let size = file.size as i64;
        let cached = {
            let state = self.inner.state.lock();
            match state.file_cache.get(&path) {
                // Provider is part of the identity: a hit parsed by another parser must not be reused.
                Some(cached)
                    if cached.size == size
                        && cached.mtime_ms == file.mtime_ms
                        && cached.provider == provider =>
                {
                    return FileRead {
                        path,
                        records: cached
                            .records
                            .iter()
                            .chain(&cached.tail_records)
                            .cloned()
                            .collect(),
                        failed: false,
                        update: None,
                    };
                }
                cached => cached.cloned(),
            }
        };

        // Only a strictly grown file may resume. Same size with a new mtime, or
        // a shrunken file, means rewritten content; re-parse it whole.
        let resume_from: Option<TranscriptParsePosition> = cached
            .as_ref()
            .filter(|cached| cached.provider == provider && size > cached.size)
            .map(|cached| cached.position.clone());

        let parsed = read_transcript_records(
            &file.path,
            self.transcript_format(reader),
            resume_from.as_ref(),
        )
        .await;
        let Some(parsed) = parsed else {
            // A read failure is not an empty transcript: caching it under this
            // (size, mtime) would silently drop the file's usage until it changes.
            let records = match &cached {
                Some(cached) if cached.provider == provider => cached
                    .records
                    .iter()
                    .chain(&cached.tail_records)
                    .cloned()
                    .collect(),
                _ => Vec::new(),
            };
            return FileRead {
                path,
                records,
                failed: true,
                update: None,
            };
        };

        // One seen set spans the cached base, the new lines, and the tail, so a
        // resumed parse dedupes exactly like a full one.
        let base = match &cached {
            Some(cached) if parsed.resumed => cached.records.clone(),
            _ => Vec::new(),
        };
        let mut seen = HashSet::new();
        let records = dedupe_within_file(&[base, parsed.records].concat(), &mut seen);
        let tail_records = dedupe_within_file(&parsed.tail_records, &mut seen);
        let replaces = cached.as_ref().map(|cached| (cached.size, cached.mtime_ms));
        let out = if tail_records.is_empty() {
            records.clone()
        } else {
            [records.clone(), tail_records.clone()].concat()
        };
        FileRead {
            path: path.clone(),
            records: out,
            failed: false,
            update: Some(CacheUpdate {
                entry: CachedFile {
                    size,
                    mtime_ms: file.mtime_ms,
                    provider,
                    records,
                    tail_records,
                    position: parsed.position,
                },
                replaces,
            }),
        }
    }

    async fn scan_transcript_dir(
        &self,
        source: &TranscriptSource,
        window_start_ms: f64,
    ) -> ScannedDir {
        let exists = tokio::fs::try_exists(&source.dir).await.unwrap_or(false);
        if !exists {
            return ScannedDir {
                provider: source.provider,
                dir: source.dir.clone(),
                volume_id: source.volume_id.clone(),
                host_id: None,
                status: None,
                message: None,
                action: None,
                files: None,
                refreshing: false,
            };
        }
        let (files, failed_paths) = list_transcript_files(
            Path::new(&source.dir),
            window_start_ms,
            source.file_name.as_deref(),
        )
        .await;
        // A cold parse waits on disk reads, so a few files in flight read close
        // to twice as fast. Results keep walk order.
        let reads_in_order: Vec<_> = files
            .iter()
            .map(|file| self.read_file_records(source.reader, source.provider, file))
            .collect();
        let reads: Vec<FileRead> = futures::stream::iter(reads_in_order)
            .buffered(TRANSCRIPT_READ_CONCURRENCY)
            .collect()
            .await;

        let mut parsed_files = Vec::with_capacity(reads.len());
        let mut unread = failed_paths;
        for read in reads {
            if read.failed {
                unread += 1;
            }
            if let Some(update) = read.update {
                // A scan of another window may have cached its own read of this file
                // meanwhile. Keep whichever read saw the later file, so a slower scan
                // never replaces newer usage with older.
                let mut state = self.inner.state.lock();
                let accept = match state.file_cache.get(&read.path) {
                    None => true,
                    Some(current) => {
                        update.replaces == Some((current.size, current.mtime_ms))
                            || !is_later_read(current, &update.entry)
                    }
                };
                if accept {
                    state.file_cache.insert(read.path.clone(), update.entry);
                    state.cache_dirty = true;
                }
            }
            parsed_files.push((read.path, read.records));
        }
        // Unread files keep their cached usage, but the total may be short.
        let (status, message) = if unread > 0 {
            (
                Some(UsageSourceStatus::Partial),
                Some(format!(
                    "{unread} transcript path(s) could not be read; usage may be incomplete."
                )),
            )
        } else {
            (None, None)
        };
        ScannedDir {
            provider: source.provider,
            dir: source.dir.clone(),
            volume_id: source.volume_id.clone(),
            host_id: None,
            status,
            message,
            action: None,
            files: Some(parsed_files),
            refreshing: false,
        }
    }

    async fn collect_dirs(
        &self,
        window_start_ms: f64,
        settings: &ServerSettings,
        retention_cutoff_ms: f64,
        await_refresh: bool,
    ) -> Result<Vec<ScannedDir>, UsageReadError> {
        let dirs = self
            .resolve_transcript_dirs(settings, retention_cutoff_ms)
            .await;
        let transcripts = async {
            let mut scanned = Vec::with_capacity(dirs.len());
            for dir in &dirs {
                scanned.push(self.scan_transcript_dir(dir, window_start_ms).await);
            }
            scanned
        };
        let scans = futures::future::try_join_all(
            self.inner
                .config
                .readers
                .iter()
                .filter(|reader| matches!(reader.reader, ProviderUsageReader::Scan { .. }))
                .map(|reader| async move {
                    let ProviderUsageReader::Scan { provider, scan } = &reader.reader else {
                        return Ok(Vec::new());
                    };
                    let provider = *provider;
                    let instances = self.usage_instances(&reader.driver_kind, settings);
                    let input = ProviderUsageScanInput {
                        instances,
                        settings: settings.clone(),
                        window_start_ms,
                        retention_cutoff_ms,
                        await_refresh,
                    };
                    let sources = scan(input).await?;
                    let mut scanned = Vec::with_capacity(sources.len());
                    for source in sources {
                        let volume_id = match source.volume_id {
                            Some(volume_id) => volume_id,
                            None => read_directory_volume_id(Path::new(&source.dir)).await,
                        };
                        scanned.push(ScannedDir {
                            provider,
                            dir: source.dir,
                            volume_id,
                            host_id: source.host_id,
                            status: source.status,
                            message: source.message,
                            action: source.action,
                            files: source.files.map(|files| {
                                files
                                    .into_iter()
                                    .map(|file| (file.path, file.records))
                                    .collect()
                            }),
                            refreshing: source.refreshing,
                        });
                    }
                    Ok::<_, UsageReadError>(scanned)
                }),
        );

        // Independent sources scan together. Transcript directories go one at a
        // time, so open files stay at `TRANSCRIPT_READ_CONCURRENCY`. The result
        // keeps this order, since aggregation keeps the first copy of a duplicate.
        let (transcripts, scans) = tokio::join!(transcripts, scans);
        let mut scanned = transcripts;
        for group in scans? {
            scanned.extend(group);
        }
        Ok(scanned)
    }

    async fn scan_summary(
        &self,
        input: &UsageSummaryInput,
        settings: &ServerSettings,
    ) -> SummaryResult {
        let since_day = input.since_day.as_str();
        let until_day = input.until_day.as_str();
        if since_day > until_day {
            return Err(invalid_window(format!(
                "sinceDay '{since_day}' is after untilDay '{until_day}'"
            )));
        }

        let hourly_window = if input.resolution == Some(UsageResolution::Hour) {
            let since = input
                .since_time
                .as_ref()
                .and_then(|value| parse_instant(value.as_str()));
            let until = input
                .until_time
                .as_ref()
                .and_then(|value| parse_instant(value.as_str()));
            let (Some(since), Some(until)) = (since, until) else {
                return Err(invalid_window(
                    "Hourly usage requires valid sinceTime and untilTime instants",
                ));
            };
            let duration = until - since;
            if duration <= 0.0 || duration > MAX_HOURLY_WINDOW_MS {
                return Err(invalid_window(
                    "Hourly usage window must be greater than zero and at most 24 hours",
                ));
            }
            Some((since, until))
        } else {
            None
        };

        let started_at_ms = now_ms();
        self.ensure_scan_cache_loaded().await;

        let host_id = gethostname::gethostname().to_string_lossy().into_owned();
        let Some(window_start) = parse_instant(&format!("{since_day}T00:00:00Z")) else {
            return Err(invalid_window(format!(
                "sinceDay '{since_day}' is not a valid date"
            )));
        };
        let window_start_ms =
            hourly_window.map_or(window_start, |(since, _)| since) - MTIME_SLACK_MS;
        let retention_cutoff_ms = started_at_ms - CACHE_RETENTION_DAYS * DAY_MS;

        // Pricing only matters once records are aggregated, so the rate table
        // loads while transcripts stream instead of gating them.
        let (_, scanned_dirs) = tokio::join!(
            self.ensure_rates(false),
            self.collect_dirs(
                window_start_ms,
                settings,
                retention_cutoff_ms,
                input.await_refresh == Some(true),
            ),
        );
        let scanned_dirs = scanned_dirs?;

        let aliases: IndexMap<String, String> = settings
            .usage_model_aliases
            .iter()
            .map(|(model, target)| (model.as_str().to_owned(), target.as_str().to_owned()))
            .collect();
        let mut aggregator = UsageAggregator::new(AggregateOptions {
            time_zone: input.time_zone.as_str().to_owned(),
            since_day: since_day.to_owned(),
            until_day: until_day.to_owned(),
            rates: self.inner.state.lock().rates.clone(),
            price_overrides: Some(create_override_rate_table(&settings.usage_price_overrides)),
            model_aliases: resolve_model_aliases(&aliases),
            resolution: input.resolution.unwrap_or(UsageResolution::Day),
            since_time_ms: hourly_window.map(|(since, _)| since as i64),
            until_time_ms: hourly_window.map(|(_, until)| until as i64),
        })
        .map_err(|error| scan_failed(error.to_string()))?;

        // Cleanup may remove transcripts, but the usage we already saved still
        // contributes to its source through the normal aggregation and dedupe
        // path. Like the walk, skip files last written before the window.
        let retained_since_ms = window_start_ms.max(retention_cutoff_ms);
        let files_by_dir: Vec<Vec<(String, Vec<UsageRecord>)>> = {
            let state = self.inner.state.lock();
            scanned_dirs
                .iter()
                .map(|scanned| {
                    let mut retained = scanned.files.clone().unwrap_or_default();
                    let live: HashSet<String> =
                        retained.iter().map(|(path, _)| path.clone()).collect();
                    for (path, entry) in &state.file_cache {
                        if entry.provider != scanned.provider
                            || entry.mtime_ms < retained_since_ms
                            || live.contains(path)
                            || !is_within_directory(path, &scanned.dir)
                        {
                            continue;
                        }
                        retained.push((
                            path.clone(),
                            entry
                                .records
                                .iter()
                                .chain(&entry.tail_records)
                                .cloned()
                                .collect(),
                        ));
                    }
                    retained
                })
                .collect()
        };
        let shared_providers = self.shared_session_providers();
        let all_files: Vec<&[UsageRecord]> = files_by_dir
            .iter()
            .flatten()
            .map(|(_, records)| records.as_slice())
            .collect();
        let cross_file_sessions = shared_sessions(&all_files, &shared_providers);

        let mut sources: Vec<UsageSource> = Vec::new();
        for (index, scanned) in scanned_dirs.iter().enumerate() {
            let mut scanned_files: u64 = 0;
            let mut skipped_files: u64 = 0;
            // Distinct per directory. A session spans days and models, so clients total
            // this figure instead of summing buckets.
            let mut session_ids: HashSet<String> = HashSet::new();

            for (_, records) in &files_by_dir[index] {
                if records.is_empty() {
                    skipped_files += 1;
                    continue;
                }
                scanned_files += 1;
                let mut event_occurrences: HashMap<String, u64> = HashMap::new();
                for record in records {
                    let mut usage_record = record.clone();
                    if shared_providers.contains(&record.provider)
                        && cross_file_sessions.contains(&record.session_id)
                    {
                        // Match moved copies without collapsing repeated equal events
                        // within one file (timestamps can have only second precision).
                        let key = serde_json::to_string(&(
                            provider_name(record.provider),
                            &record.session_id,
                            record.timestamp_ms,
                            &record.model,
                            record.totals,
                        ))
                        .unwrap_or_default();
                        let occurrence = event_occurrences.entry(key.clone()).or_insert(0);
                        *occurrence += 1;
                        usage_record.dedupe_key = Some(format!("{key}:{occurrence}"));
                    }
                    // Only sessions contributing in-window count; the mtime slack can
                    // admit boundary files whose records fall outside the range.
                    if aggregator.add(&usage_record, &scanned.dir) && !record.session_id.is_empty()
                    {
                        session_ids.insert(record.session_id.clone());
                    }
                }
            }

            let status = if scanned.files.is_none() && scanned_files == 0 {
                // Clients exclude missing sources, so saved records remain an available source.
                UsageSourceStatus::Missing
            } else {
                scanned.status.unwrap_or(UsageSourceStatus::Ok)
            };
            let message = scanned.message.clone().or_else(|| {
                scanned
                    .files
                    .is_none()
                    .then(|| "No transcript directory on this environment.".to_owned())
            });
            sources.push(UsageSource {
                fingerprint: UsageSourceFingerprint {
                    host_id: trimmed(scanned.host_id.as_deref().unwrap_or(&host_id)),
                    provider: scanned.provider,
                    resolved_home_path: trimmed(&scanned.dir),
                    volume_id: scanned.volume_id.clone(),
                },
                status,
                scanned_files,
                skipped_files,
                malformed_records: 0,
                distinct_sessions: session_ids.len() as u64,
                message: message.as_deref().map(trimmed),
                action: scanned.action,
                refreshing: scanned.refreshing.then_some(true),
            });
        }

        {
            let mut state = self.inner.state.lock();
            if prune_scan_cache(&mut state.file_cache, retention_cutoff_ms) > 0 {
                state.cache_dirty = true;
            }
        }

        let aggregated = aggregator.finish();
        let finished_at_ms = now_ms();
        Ok(UsageSummary {
            contract_version: USAGE_CONTRACT_VERSION,
            read_at: iso_millis(finished_at_ms),
            time_zone: trimmed(input.time_zone.as_str()),
            since_day: UsageDay::new(since_day).expect("validated by the caller"),
            until_day: UsageDay::new(until_day).expect("validated by the caller"),
            buckets: aggregated.buckets,
            sources,
            pricing: self.pricing(),
            scan_duration_ms: (finished_at_ms - started_at_ms).max(0.0) as u64,
        })
    }

    /// Reads the usage summary for `input` under `settings`.
    ///
    /// Concurrent identical requests share one scan. The scan runs detached, so a
    /// caller that goes away cannot tear it out from under the others, and a
    /// finished scan still warms the cache.
    pub async fn read_summary(
        &self,
        input: &UsageSummaryInput,
        settings: &ServerSettings,
    ) -> SummaryResult {
        let key = scan_key(input, settings);
        let mut receiver = {
            let mut inflight = self.inner.inflight.lock();
            if let Some(existing) = inflight.get(&key) {
                existing.clone()
            } else {
                let (sender, receiver) = watch::channel(None);
                inflight.insert(key.clone(), receiver.clone());
                let service = self.clone();
                let input = input.clone();
                let settings = settings.clone();
                // Detached on purpose, matching `Effect.forkDetach`: the handle is
                // not kept because the scan outlives any one caller.
                drop(tokio::spawn(async move {
                    let result = service.scan_summary(&input, &settings).await;
                    service.inner.inflight.lock().remove(&key);
                    if result.is_ok() {
                        service.schedule_persist();
                    }
                    let _ = sender.send(Some(result));
                }));
                receiver
            }
        };
        let ready = receiver
            .wait_for(Option::is_some)
            .await
            .map_err(|_| scan_failed("The usage scan ended without a result."))?;
        match ready.clone() {
            Some(result) => result,
            None => Err(scan_failed("The usage scan ended without a result.")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_formats::claude_format;
    use serde_json::json;
    use turin_contracts::usage::{UsageCostSource, UsageResolution};
    use turin_provider_core::rt::boxed;

    fn claude_line(id: u32, output_tokens: u64) -> String {
        format!(
            "{}\n",
            json!({
                "type": "assistant",
                "timestamp": "2026-08-01T10:00:00Z",
                "requestId": format!("req_{id}"),
                "sessionId": "session-1",
                "message": {
                    "id": format!("msg_{id}"),
                    "model": "claude-fable-5",
                    "usage": { "input_tokens": 10, "output_tokens": output_tokens },
                },
            })
        )
    }

    fn settings() -> ServerSettings {
        serde_json::from_value(json!({})).expect("empty settings decode to defaults")
    }

    fn input(since: &str, until: &str) -> UsageSummaryInput {
        UsageSummaryInput {
            since_day: UsageDay::new(since).unwrap(),
            until_day: UsageDay::new(until).unwrap(),
            time_zone: TrimmedNonEmptyString::new("UTC").unwrap(),
            resolution: None,
            since_time: None,
            until_time: None,
            await_refresh: None,
        }
    }

    /// A rate table written to disk with a fresh timestamp, so no fetch happens.
    fn seed_rates(state_dir: &Path) {
        std::fs::create_dir_all(state_dir).unwrap();
        let document = json!({
            "fetchedAtMs": now_ms(),
            "document": {
                "claude-fable-5": { "input_cost_per_token": 1e-5, "output_cost_per_token": 5e-5 },
            },
        });
        std::fs::write(state_dir.join(RATES_CACHE_FILE_NAME), document.to_string()).unwrap();
    }

    fn service(state_dir: &Path, transcripts: &Path) -> UsageService {
        let dir = transcripts.to_path_buf();
        let reader = UsageReader {
            driver_kind: "claudeAgent".to_owned(),
            reader: ProviderUsageReader::Transcripts {
                provider: UsageProviderKind::Claude,
                format: claude_format(),
                directories: Arc::new(move |_instance| {
                    let dir = dir.clone();
                    boxed(async move {
                        vec![TranscriptDirectory {
                            dir: dir.to_string_lossy().into_owned(),
                            file_name: None,
                        }]
                    })
                }),
            },
        };
        UsageService::new(UsageServiceConfig {
            state_dir: state_dir.to_path_buf(),
            readers: vec![reader],
            host_env: ProcessEnv::new(),
            home_dir: "/home/test".to_owned(),
            // Never reached: the seeded rate table is fresh. A dead port keeps a stray fetch cheap.
            rates_url: "http://127.0.0.1:9/rates.json".to_owned(),
        })
    }

    fn reason(error: &UsageReadError) -> UsageReadErrorReason {
        match error {
            UsageReadError::ReadError { reason, .. } => *reason,
        }
    }

    fn records(summary: &UsageSummary) -> u64 {
        summary.buckets.iter().map(|bucket| bucket.records).sum()
    }

    #[tokio::test]
    async fn reads_and_prices_a_transcript_directory() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let transcripts = root.path().join("projects");
        std::fs::create_dir_all(&transcripts).unwrap();
        std::fs::write(
            transcripts.join("s1.jsonl"),
            claude_line(1, 5) + &claude_line(2, 7),
        )
        .unwrap();
        seed_rates(&state);

        let summary = service(&state, &transcripts)
            .read_summary(&input("2026-08-01", "2026-08-31"), &settings())
            .await
            .unwrap();

        assert_eq!(summary.buckets.len(), 1);
        let bucket = &summary.buckets[0];
        assert_eq!(bucket.records, 2);
        assert_eq!(bucket.totals.output_tokens, 12);
        // Two records of 10 input and 5 or 7 output tokens: 2 * 10 * 1e-5 + 12 * 5e-5.
        assert_eq!(bucket.cost_source, UsageCostSource::ModelPriced);
        assert!(
            (bucket.cost_usd - 8e-4).abs() < 1e-12,
            "{}",
            bucket.cost_usd
        );
        assert_eq!(summary.sources.len(), 1);
        assert_eq!(summary.sources[0].status, UsageSourceStatus::Ok);
        assert_eq!(summary.sources[0].scanned_files, 1);
        assert_eq!(summary.sources[0].distinct_sessions, 1);
        assert_eq!(summary.pricing.status, UsagePricingStatus::Cached);
        assert_eq!(summary.pricing.known_models, 1);
    }

    #[tokio::test]
    async fn a_warm_scan_sees_only_appended_lines_of_a_grown_file() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let transcripts = root.path().join("projects");
        std::fs::create_dir_all(&transcripts).unwrap();
        let file = transcripts.join("s1.jsonl");
        std::fs::write(&file, claude_line(1, 5)).unwrap();
        seed_rates(&state);
        let service = service(&state, &transcripts);
        let cold = service
            .read_summary(&input("2026-08-01", "2026-08-31"), &settings())
            .await
            .unwrap();
        assert_eq!(records(&cold), 1);

        let mut appended = std::fs::read_to_string(&file).unwrap();
        appended += &claude_line(2, 9);
        std::fs::write(&file, appended).unwrap();
        let warm = service
            .read_summary(&input("2026-08-01", "2026-08-31"), &settings())
            .await
            .unwrap();
        assert_eq!(records(&warm), 2);
        assert_eq!(warm.buckets[0].totals.output_tokens, 14);
    }

    #[tokio::test]
    async fn drops_a_message_copied_into_another_transcript() {
        // Resumed and forked sessions copy a message's records forward; the
        // global dedupe pass keeps one.
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let transcripts = root.path().join("projects");
        std::fs::create_dir_all(&transcripts).unwrap();
        std::fs::write(transcripts.join("a.jsonl"), claude_line(1, 5)).unwrap();
        std::fs::write(transcripts.join("b.jsonl"), claude_line(1, 5)).unwrap();
        seed_rates(&state);

        let summary = service(&state, &transcripts)
            .read_summary(&input("2026-08-01", "2026-08-31"), &settings())
            .await
            .unwrap();
        assert_eq!(records(&summary), 1);
        assert_eq!(summary.buckets[0].totals.output_tokens, 5);
    }

    #[tokio::test]
    async fn persists_the_scan_cache_for_the_next_start() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let transcripts = root.path().join("projects");
        std::fs::create_dir_all(&transcripts).unwrap();
        std::fs::write(transcripts.join("s1.jsonl"), claude_line(1, 5)).unwrap();
        seed_rates(&state);

        let first = service(&state, &transcripts);
        let cold = first
            .read_summary(&input("2026-08-01", "2026-08-31"), &settings())
            .await
            .unwrap();
        first.await_persisted().await;
        assert!(state.join(SCAN_CACHE_FILE_NAME).exists());

        let restarted = service(&state, &transcripts)
            .read_summary(&input("2026-08-01", "2026-08-31"), &settings())
            .await
            .unwrap();
        assert_eq!(restarted.buckets, cold.buckets);
        assert_eq!(restarted.sources[0].scanned_files, 1);
    }

    #[tokio::test]
    async fn reports_a_missing_directory_as_a_missing_source() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        seed_rates(&state);
        let summary = service(&state, &root.path().join("absent"))
            .read_summary(&input("2026-08-01", "2026-08-31"), &settings())
            .await
            .unwrap();

        assert!(summary.buckets.is_empty());
        assert_eq!(summary.sources[0].status, UsageSourceStatus::Missing);
        assert_eq!(
            summary.sources[0]
                .message
                .as_ref()
                .map(|message| message.as_str()),
            Some("No transcript directory on this environment.")
        );
    }

    #[tokio::test]
    async fn rejects_an_inverted_window() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        seed_rates(&state);
        let error = service(&state, root.path())
            .read_summary(&input("2026-08-31", "2026-08-01"), &settings())
            .await
            .unwrap_err();
        assert_eq!(reason(&error), UsageReadErrorReason::InvalidWindow);
    }

    #[tokio::test]
    async fn rejects_an_hourly_window_without_instants_or_longer_than_a_day() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        seed_rates(&state);
        let service = service(&state, root.path());

        let mut hourly = input("2026-08-01", "2026-08-01");
        hourly.resolution = Some(UsageResolution::Hour);
        let error = service
            .read_summary(&hourly, &settings())
            .await
            .unwrap_err();
        assert_eq!(reason(&error), UsageReadErrorReason::InvalidWindow);

        hourly.since_time = Some(TrimmedNonEmptyString::new("2026-08-01T00:00:00Z").unwrap());
        hourly.until_time = Some(TrimmedNonEmptyString::new("2026-08-03T00:00:00Z").unwrap());
        let error = service
            .read_summary(&hourly, &settings())
            .await
            .unwrap_err();
        assert_eq!(reason(&error), UsageReadErrorReason::InvalidWindow);
    }
}
