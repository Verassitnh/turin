//! Port of upstream/apps/server/src/pullRequest/PullRequestReadCache.ts
//!
//! A read-through cache for encoded pull request reads. Entries live a minute, are keyed by the
//! SHA-256 of the caller's key, and are written through a [`KeyValueStore`] (a directory of files
//! in production, memory in tests) so a restarted app reuses them. Writes invalidate by *scope*:
//! each scope carries a random revision for a minute, and an entry written under an older
//! revision of any scope it names is refetched.
//!
//! Upstream builds this on Effect's `PersistedCache` and `KeyValueStore`; neither exists here, so
//! both are reimplemented to the behaviour upstream relies on (see the notes on [`Inner`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::FutureExt;
use futures::future::{BoxFuture, Shared, WeakShared};
use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Semaphore, oneshot};
use tokio::task::JoinSet;
use turin_contracts::pull_request::{PullRequestOperationError, PullRequestUnavailableError};

use crate::source_control_core::clock::Clock;

const CONCURRENT_READS: u32 = 512;
/// Persistence prefixes every entry key with the store id, so each entry file name starts with
/// it. The scope `revisions` file does not.
const STORE_ID: &str = "pr-v2";
/// How long a cached read, and a scope's revision, stays valid.
const ENTRY_TTL_MILLIS: i64 = 60_000;
/// An entry file is the store id followed by the SHA-256 hex digest of its key.
static ENTRY_FILE_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("^{STORE_ID}[0-9a-f]{{64}}$")).unwrap());
/// Entries expire a minute after they are written, and a write sets the file's mtime. A file
/// untouched for a day is long expired; the day only leaves slack for clock changes. Pruning a
/// live entry would cost one refetch.
pub const ENTRY_FILE_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// How often the entry directory is swept once the file-backed cache is open.
pub const PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// `ReadError`: what a cached lookup may fail with. Failures are never cached.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PullRequestReadError {
    #[error(transparent)]
    Operation(#[from] PullRequestOperationError),
    #[error(transparent)]
    Unavailable(#[from] PullRequestUnavailableError),
}

pub type PullRequestReadResult = Result<String, PullRequestReadError>;
/// The read to run on a miss. Run at most once per [`PullRequestReadCache::get`] call, however
/// many layers ask for its answer (upstream wraps it in `Effect.cached`).
pub type PullRequestReadLookup = BoxFuture<'static, PullRequestReadResult>;

// ---------------------------------------------------------------------------------------------
// KeyValueStore (effect/persistence/KeyValueStore)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("KeyValueStore {method} failed: {message}")]
pub struct KeyValueStoreError {
    pub method: &'static str,
    pub message: String,
}

/// The string key-value store the cache persists through.
#[async_trait]
pub trait KeyValueStore: Send + Sync + 'static {
    async fn get(&self, key: &str) -> Result<Option<String>, KeyValueStoreError>;
    async fn set(&self, key: &str, value: &str) -> Result<(), KeyValueStoreError>;
    async fn remove(&self, key: &str) -> Result<(), KeyValueStoreError>;
}

/// `KeyValueStore.layerMemory`.
#[derive(Debug, Default)]
pub struct MemoryKeyValueStore {
    entries: Mutex<HashMap<String, String>>,
}

impl MemoryKeyValueStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl KeyValueStore for MemoryKeyValueStore {
    async fn get(&self, key: &str) -> Result<Option<String>, KeyValueStoreError> {
        Ok(self.entries.lock().get(key).cloned())
    }
    async fn set(&self, key: &str, value: &str) -> Result<(), KeyValueStoreError> {
        self.entries.lock().insert(key.to_owned(), value.to_owned());
        Ok(())
    }
    async fn remove(&self, key: &str) -> Result<(), KeyValueStoreError> {
        self.entries.lock().remove(key);
        Ok(())
    }
}

/// `KeyValueStore.layerFileSystem(directory)`: one file per key, named by the key.
#[derive(Debug, Clone)]
pub struct FileSystemKeyValueStore {
    directory: PathBuf,
}

impl FileSystemKeyValueStore {
    /// Creates `directory` (and parents) if needed.
    pub async fn open(directory: impl Into<PathBuf>) -> Result<Self, KeyValueStoreError> {
        let directory = directory.into();
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(|error| KeyValueStoreError {
                method: "open",
                message: error.to_string(),
            })?;
        Ok(Self { directory })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn path_of(&self, key: &str) -> PathBuf {
        // Keys here are `revisions` and `pr-v2<hex>`; encoding keeps any other key a file name.
        self.directory
            .join(crate::shared::js::encode_uri_component(key))
    }
}

#[async_trait]
impl KeyValueStore for FileSystemKeyValueStore {
    async fn get(&self, key: &str) -> Result<Option<String>, KeyValueStoreError> {
        match tokio::fs::read_to_string(self.path_of(key)).await {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(KeyValueStoreError {
                method: "get",
                message: error.to_string(),
            }),
        }
    }
    async fn set(&self, key: &str, value: &str) -> Result<(), KeyValueStoreError> {
        tokio::fs::write(self.path_of(key), value)
            .await
            .map_err(|error| KeyValueStoreError {
                method: "set",
                message: error.to_string(),
            })
    }
    async fn remove(&self, key: &str) -> Result<(), KeyValueStoreError> {
        match tokio::fs::remove_file(self.path_of(key)).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(KeyValueStoreError {
                method: "remove",
                message: error.to_string(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Persisted shapes
// ---------------------------------------------------------------------------------------------

/// `Schema.Finite` decodes any JSON number; ours are whole milliseconds.
fn finite_millis<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    let value = f64::deserialize(deserializer)?;
    if value.is_finite() {
        Ok(value.floor() as i64)
    } else {
        Err(serde::de::Error::custom("expected a finite number"))
    }
}

/// A scope's current revision and when it stops counting.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevisionRecord {
    revision: String,
    #[serde(deserialize_with = "finite_millis")]
    expires_at: i64,
}

type Revisions = IndexMap<String, RevisionRecord>;

/// A stored read: the encoded payload, when it expires, and the scope revisions it was read under.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    payload: String,
    #[serde(deserialize_with = "finite_millis")]
    expires_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
}

/// Why a cache layer failed. Store faults fall back to the plain lookup; read errors are the
/// caller's own and pass through.
#[derive(Debug, Clone)]
enum GetFailure {
    Read(PullRequestReadError),
    Store(#[allow(dead_code)] String),
}

impl From<KeyValueStoreError> for GetFailure {
    fn from(error: KeyValueStoreError) -> Self {
        Self::Store(error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ReadKey {
    key: String,
    revision: String,
}

/// `Read`: one request to the persisted cache.
#[derive(Clone)]
struct ReadRequest {
    /// SHA-256 hex of the caller's key.
    key: String,
    /// Scope revisions joined by `:`; an empty part is a scope nothing has invalidated.
    revision: String,
    lookup: Shared<PullRequestReadLookup>,
}

impl ReadRequest {
    fn read_key(&self) -> ReadKey {
        ReadKey {
            key: self.key.clone(),
            revision: self.revision.clone(),
        }
    }

    fn store_key(&self) -> String {
        format!("{STORE_ID}{}", self.key)
    }

    fn matches_revision(&self, revision: Option<&str>) -> bool {
        let stored: Vec<&str> = revision.map(|r| r.split(':').collect()).unwrap_or_default();
        self.revision
            .split(':')
            .enumerate()
            .all(|(index, value)| value.is_empty() || Some(&value) == stored.get(index))
    }
}

/// Shares one in-flight computation per key between concurrent callers (Effect's `Cache.get`
/// coalescing). Nothing is retained once the computation is done or every waiter has gone, so
/// a read abandoned by all its callers is cancelled rather than left running.
struct Coalescer<K, V: Clone> {
    inflight: Mutex<HashMap<K, WeakShared<BoxFuture<'static, V>>>>,
}

impl<K: Eq + std::hash::Hash, V: Clone + Send + 'static> Coalescer<K, V> {
    fn new() -> Self {
        Self {
            inflight: Mutex::new(HashMap::new()),
        }
    }

    async fn run(&self, key: K, make: impl FnOnce() -> BoxFuture<'static, V>) -> V {
        let shared = {
            let mut inflight = self.inflight.lock();
            inflight.retain(|_, weak| weak.upgrade().is_some_and(|s| s.peek().is_none()));
            match inflight.get(&key).and_then(WeakShared::upgrade) {
                Some(shared) => shared,
                None => {
                    let shared = make().shared();
                    if let Some(weak) = shared.downgrade() {
                        inflight.insert(key, weak);
                    }
                    shared
                }
            }
        };
        shared.await
    }
}

/// The cache's state. `get` holds one permit of `lock` and `invalidate` all of them, so a
/// scope write waits for every in-flight read and blocks new ones until it is done.
struct Inner {
    store: Arc<dyn KeyValueStore>,
    clock: Arc<dyn Clock>,
    /// Cleared when a scope write fails: from then on every read goes straight to its lookup.
    enabled: AtomicBool,
    lock: Arc<Semaphore>,
    /// The scope revisions, loaded once and kept until the next write (`Cache` of capacity 1
    /// that keeps successes forever and failures not at all).
    revisions: tokio::sync::Mutex<Option<Arc<Revisions>>>,
    /// `PersistedCache`'s in-memory layer: capacity [`CONCURRENT_READS`], each entry living until
    /// its own `expiresAt`, oldest dropped first.
    memory: Mutex<IndexMap<ReadKey, Entry>>,
    gets: Coalescer<ReadKey, Result<Entry, GetFailure>>,
    refreshes: Coalescer<ReadKey, Result<Entry, GetFailure>>,
    /// Scope writes run to completion even when their caller goes away (`Effect.uninterruptible`).
    tasks: Mutex<JoinSet<()>>,
}

impl Inner {
    fn now(&self) -> i64 {
        self.clock.now_millis()
    }

    async fn revisions(&self) -> Result<Arc<Revisions>, GetFailure> {
        let mut cached = self.revisions.lock().await;
        if let Some(revisions) = cached.as_ref() {
            return Ok(revisions.clone());
        }
        let raw = self.store.get("revisions").await?;
        let revisions: Revisions = serde_json::from_str(raw.as_deref().unwrap_or("{}"))
            .map_err(|error| GetFailure::Store(error.to_string()))?;
        let revisions = Arc::new(revisions);
        *cached = Some(revisions.clone());
        Ok(revisions)
    }

    fn memory_get(&self, key: &ReadKey) -> Option<Entry> {
        let now = self.now();
        let mut memory = self.memory.lock();
        match memory.get(key) {
            Some(entry) if entry.expires_at > now => Some(entry.clone()),
            Some(_) => {
                memory.shift_remove(key);
                None
            }
            None => None,
        }
    }

    fn memory_insert(&self, key: ReadKey, entry: Entry) {
        let mut memory = self.memory.lock();
        memory.shift_remove(&key);
        while memory.len() >= CONCURRENT_READS as usize {
            memory.shift_remove_index(0);
        }
        memory.insert(key, entry);
    }

    /// `PersistedCache.get`: memory, then the store, then the lookup.
    async fn cache_get(self: &Arc<Self>, request: ReadRequest) -> Result<Entry, GetFailure> {
        let key = request.read_key();
        if let Some(entry) = self.memory_get(&key) {
            return Ok(entry);
        }
        let inner = self.clone();
        self.gets
            .run(key, move || {
                async move { inner.load_or_lookup(request).await }.boxed()
            })
            .await
    }

    async fn load_or_lookup(&self, request: ReadRequest) -> Result<Entry, GetFailure> {
        let store_key = request.store_key();
        if let Some(raw) = self.store.get(&store_key).await? {
            // An entry that no longer decodes is a miss and is overwritten below.
            if let Ok(entry) = serde_json::from_str::<Entry>(&raw) {
                if entry.expires_at > self.now() {
                    self.remember(&request, entry.clone());
                    return Ok(entry);
                }
                // The persisted cache drops an expired entry only when it is read again.
                self.store.remove(&store_key).await?;
            }
        }
        self.fetch_and_store(request).await
    }

    /// Keeps a loaded entry in memory only while its revision still matches the request. A read
    /// that loaded the store before an invalidation holds a revision the scope has moved off, and
    /// must not be found under the new key.
    fn remember(&self, request: &ReadRequest, entry: Entry) {
        if request.matches_revision(entry.revision.as_deref()) {
            self.memory_insert(request.read_key(), entry);
        }
    }

    /// Runs the lookup and stores what it returns under the request's revision.
    async fn fetch_and_store(&self, request: ReadRequest) -> Result<Entry, GetFailure> {
        let store_key = request.store_key();
        let payload = request.lookup.clone().await.map_err(GetFailure::Read)?;
        let entry = Entry {
            payload,
            expires_at: self.now() + ENTRY_TTL_MILLIS,
            revision: Some(request.revision.clone()),
        };
        let encoded =
            serde_json::to_string(&entry).map_err(|e| GetFailure::Store(e.to_string()))?;
        self.store.set(&store_key, &encoded).await?;
        self.memory_insert(request.read_key(), entry.clone());
        Ok(entry)
    }

    /// `PersistedCache.invalidate`: the store first, then memory. A read that misses the store
    /// after the removal looks up again rather than finding an entry the store no longer holds.
    async fn cache_invalidate(&self, request: &ReadRequest) -> Result<(), GetFailure> {
        let removed = self.store.remove(&request.store_key()).await;
        self.memory.lock().shift_remove(&request.read_key());
        removed?;
        Ok(())
    }

    /// `PullRequestReadCache.refresh`, shared between concurrent callers for the same request.
    async fn refresh(self: &Arc<Self>, request: ReadRequest) -> Result<Entry, GetFailure> {
        let inner = self.clone();
        let key = request.read_key();
        self.refreshes
            .run(key, move || {
                async move {
                    let stored = inner.cache_get(request.clone()).await?;
                    if request.matches_revision(stored.revision.as_deref()) {
                        return Ok(stored);
                    }
                    inner.cache_invalidate(&request).await?;
                    let fresh = inner.cache_get(request.clone()).await?;
                    if request.matches_revision(fresh.revision.as_deref()) {
                        return Ok(fresh);
                    }
                    // A read that loaded the store before the removal wrote its stale entry back.
                    // Take a new lookup rather than serve that entry.
                    inner.fetch_and_store(request).await
                }
                .boxed()
            })
            .await
    }

    async fn get_cached(
        self: &Arc<Self>,
        key: &str,
        read: Shared<PullRequestReadLookup>,
        scopes: &[String],
    ) -> Result<String, GetFailure> {
        let current = self.revisions().await?;
        let now = self.now();
        let revision = scopes
            .iter()
            .map(|scope| match current.get(scope) {
                Some(value) if value.expires_at > now => value.revision.as_str(),
                _ => "",
            })
            .collect::<Vec<_>>()
            .join(":");
        let request = ReadRequest {
            key: digest(key),
            revision,
            lookup: read,
        };
        let stored = self.cache_get(request.clone()).await?;
        if request.matches_revision(stored.revision.as_deref()) {
            Ok(stored.payload)
        } else {
            Ok(self.refresh(request).await?.payload)
        }
    }

    async fn write_revision(&self, scope: &str) -> Result<(), GetFailure> {
        let now = self.now();
        let current = self.revisions().await?;
        let mut next: Revisions = current
            .iter()
            .filter(|(_, value)| value.expires_at > now)
            .map(|(scope, value)| (scope.clone(), value.clone()))
            .collect();
        next.insert(
            scope.to_owned(),
            RevisionRecord {
                revision: uuid::Uuid::new_v4().to_string(),
                expires_at: now + ENTRY_TTL_MILLIS,
            },
        );
        let encoded = serde_json::to_string(&next).map_err(|e| GetFailure::Store(e.to_string()))?;
        self.store.set("revisions", &encoded).await?;
        *self.revisions.lock().await = Some(Arc::new(next));
        Ok(())
    }
}

fn digest(key: &str) -> String {
    Sha256::digest(key.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Aborts the periodic sweep when the owner goes away (upstream scopes it to the layer).
struct PruneTask(tokio::task::JoinHandle<()>);

impl Drop for PruneTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub struct PullRequestReadCache {
    inner: Arc<Inner>,
    _pruner: Option<PruneTask>,
}

impl PullRequestReadCache {
    /// `PullRequestReadCache.make`: a cache over `store`, reading time from `clock`.
    pub fn new(store: Arc<dyn KeyValueStore>, clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                clock,
                enabled: AtomicBool::new(true),
                lock: Arc::new(Semaphore::new(CONCURRENT_READS as usize)),
                revisions: tokio::sync::Mutex::new(None),
                memory: Mutex::new(IndexMap::new()),
                gets: Coalescer::new(),
                refreshes: Coalescer::new(),
                tasks: Mutex::new(JoinSet::new()),
            }),
            _pruner: None,
        }
    }

    /// A cache that forgets everything on restart.
    pub fn memory(clock: Arc<dyn Clock>) -> Self {
        Self::new(Arc::new(MemoryKeyValueStore::new()), clock)
    }

    /// `PullRequestReadCache.layer`: files under `<provider_status_cache_dir>/pull-requests`,
    /// swept now and then every [`PRUNE_INTERVAL`] for as long as the cache lives. An unusable
    /// directory falls back to memory. Upstream starts the sweep once the server is active; build
    /// the cache then if that matters.
    pub async fn file_backed(provider_status_cache_dir: &Path, clock: Arc<dyn Clock>) -> Self {
        let directory = provider_status_cache_dir.join("pull-requests");
        match FileSystemKeyValueStore::open(&directory).await {
            Ok(store) => {
                let mut cache = Self::new(Arc::new(store), clock.clone());
                cache._pruner = Some(spawn_prune_loop(directory, clock, PRUNE_INTERVAL));
                cache
            }
            Err(_) => {
                tracing::warn!("PR cache directory unavailable; using memory cache");
                Self::memory(clock)
            }
        }
    }

    /// The cached answer for `key`, or the result of `lookup` (cached for a minute unless it
    /// fails). `scopes` name what the read depends on: an entry written before the newest
    /// [`invalidate`](Self::invalidate) of any of them is refetched, and concurrent refetches
    /// share one lookup. Cache faults degrade to running `lookup` directly.
    pub async fn get(
        &self,
        key: &str,
        lookup: PullRequestReadLookup,
        scopes: &[String],
    ) -> PullRequestReadResult {
        if !self.inner.enabled.load(Ordering::SeqCst) {
            return lookup.await;
        }
        let read = lookup.shared();
        let Ok(_permit) = self.inner.lock.clone().acquire_owned().await else {
            return read.await;
        };
        match self.inner.get_cached(key, read.clone(), scopes).await {
            Ok(payload) => Ok(payload),
            Err(GetFailure::Read(error)) => Err(error),
            Err(GetFailure::Store(_)) => read.await,
        }
    }

    /// Marks everything read under `scope` stale. Waits for in-flight reads first; once started
    /// it finishes even if this future is dropped. A failed write turns the cache off.
    pub async fn invalidate(&self, scope: &str) {
        let Ok(permit) = self
            .inner
            .lock
            .clone()
            .acquire_many_owned(CONCURRENT_READS)
            .await
        else {
            return;
        };
        let inner = self.inner.clone();
        let scope = scope.to_owned();
        let (done, finished) = oneshot::channel();
        {
            let mut tasks = self.inner.tasks.lock();
            while tasks.try_join_next().is_some() {}
            tasks.spawn(async move {
                let _permit = permit;
                if inner.write_revision(&scope).await.is_err() {
                    inner.enabled.store(false, Ordering::SeqCst);
                    tracing::warn!("PR cache disabled after clearing failed");
                }
                let _ = done.send(());
            });
        }
        let _ = finished.await;
    }
}

/// Deletes entry files in `directory` not written within [`ENTRY_FILE_MAX_AGE`]. The persisted
/// cache drops an expired entry only when it is read again, so files for PRs nobody reopens would
/// otherwise stay forever.
pub async fn prune_expired_entry_files(directory: &Path, clock: &dyn Clock) -> std::io::Result<()> {
    let cutoff = clock.now_millis() - ENTRY_FILE_MAX_AGE.as_millis() as i64;
    let mut entries = Vec::new();
    let mut read_dir = tokio::fs::read_dir(directory).await?;
    while let Some(entry) = read_dir.next_entry().await? {
        if let Some(name) = entry.file_name().to_str()
            && ENTRY_FILE_NAME.is_match(name)
        {
            entries.push(entry.path());
        }
    }
    // One file at a time, visiting every file, so one locked file does not stop the sweep.
    let mut failures: Vec<std::io::Error> = Vec::new();
    for path in entries {
        let result = async {
            let modified = tokio::fs::metadata(&path).await?.modified()?;
            let modified_millis = match modified.duration_since(std::time::UNIX_EPOCH) {
                Ok(after) => after.as_millis() as i64,
                Err(before) => -(before.duration().as_millis() as i64),
            };
            if modified_millis < cutoff {
                tokio::fs::remove_file(&path).await?;
            }
            Ok::<(), std::io::Error>(())
        }
        .await;
        match result {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => failures.push(error),
            _ => {}
        }
    }
    if let Some(first) = failures.first() {
        tracing::warn!(failed = failures.len(), cause = %first, "Failed to prune some PR cache files");
    }
    Ok(())
}

/// Prunes once now, then `interval` after each sweep finishes.
fn spawn_prune_loop(directory: PathBuf, clock: Arc<dyn Clock>, interval: Duration) -> PruneTask {
    PruneTask(tokio::spawn(async move {
        loop {
            if let Err(cause) = prune_expired_entry_files(&directory, clock.as_ref()).await {
                tracing::warn!(%cause, "Failed to prune PR cache files");
            }
            tokio::time::sleep(interval).await;
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_control_core::clock::ManualClock;
    use async_trait::async_trait;
    use pretty_assertions::assert_eq;
    use std::sync::atomic::AtomicUsize;
    use std::time::SystemTime;
    use tokio::sync::Notify;

    /// 2026-10-01T00:00:00Z
    const START: i64 = 1_790_812_800_000;

    fn lookup_ok(value: &'static str) -> PullRequestReadLookup {
        async move { Ok(value.to_owned()) }.boxed()
    }

    /// Counts how often a lookup actually ran and answers with that count.
    fn counting(reads: &Arc<AtomicUsize>) -> impl Fn() -> PullRequestReadLookup + use<> {
        let reads = reads.clone();
        move || {
            let reads = reads.clone();
            async move { Ok((reads.fetch_add(1, Ordering::SeqCst) + 1).to_string()) }.boxed()
        }
    }

    fn scopes(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    async fn open(directory: &Path, clock: &Arc<ManualClock>) -> PullRequestReadCache {
        let store = FileSystemKeyValueStore::open(directory).await.unwrap();
        PullRequestReadCache::new(Arc::new(store), clock.clone())
    }

    fn file_names(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// Sets a file's mtime to `age` before the current (manual) clock time.
    fn age_files(directory: &Path, clock: &ManualClock, names: &[&str], age: Duration) {
        let mtime = SystemTime::UNIX_EPOCH
            + Duration::from_millis((clock.now_millis() - age.as_millis() as i64) as u64);
        for name in names {
            std::fs::File::options()
                .write(true)
                .open(directory.join(name))
                .unwrap()
                .set_modified(mtime)
                .unwrap();
        }
    }

    /// A store whose first `remove` of one key stops, before or after the removal, until the test
    /// releases it. Lets a test run other reads while an invalidation is half done.
    struct PausingStore {
        inner: MemoryKeyValueStore,
        key: String,
        pause_after: bool,
        armed: AtomicBool,
        reached: Notify,
        release: Notify,
    }

    impl PausingStore {
        fn new(key: String, pause_after: bool) -> Self {
            Self {
                inner: MemoryKeyValueStore::new(),
                key,
                pause_after,
                armed: AtomicBool::new(true),
                reached: Notify::new(),
                release: Notify::new(),
            }
        }
    }

    #[async_trait]
    impl KeyValueStore for PausingStore {
        async fn get(&self, key: &str) -> Result<Option<String>, KeyValueStoreError> {
            self.inner.get(key).await
        }
        async fn set(&self, key: &str, value: &str) -> Result<(), KeyValueStoreError> {
            self.inner.set(key, value).await
        }
        async fn remove(&self, key: &str) -> Result<(), KeyValueStoreError> {
            if key != self.key || !self.armed.swap(false, Ordering::SeqCst) {
                return self.inner.remove(key).await;
            }
            if self.pause_after {
                let removed = self.inner.remove(key).await;
                self.reached.notify_one();
                self.release.notified().await;
                removed
            } else {
                self.reached.notify_one();
                self.release.notified().await;
                self.inner.remove(key).await
            }
        }
    }

    /// The store key the cache writes a request's entry under.
    fn store_key_of(key: &str) -> String {
        format!("{STORE_ID}{}", digest(key))
    }

    /// A stored entry at the scope's empty revision, then a scope write that moves the scope on:
    /// the state every stale-read race starts from. Returns the cache and the scope's new revision.
    async fn stale_entry_behind_a_scope_write(
        store: Arc<PausingStore>,
        clock: &Arc<ManualClock>,
    ) -> (Arc<PullRequestReadCache>, String) {
        let cache = Arc::new(PullRequestReadCache::new(store, clock.clone()));
        assert_eq!(
            cache
                .get("first", lookup_ok("1"), &scopes(&["pr"]))
                .await
                .unwrap(),
            "1"
        );
        cache.invalidate("pr").await;
        let revision = cache.inner.revisions().await.unwrap()["pr"]
            .revision
            .clone();
        (cache, revision)
    }

    #[tokio::test]
    async fn a_read_that_loaded_the_store_before_an_invalidation_is_not_kept_under_the_new_revision()
     {
        // Deterministic interleaving: the refresh stops before it removes the stale entry, and a
        // load runs in that window. The load sees the stale entry; memory must not keep it under the
        // scope's new revision, where the refresh's final read would find it.
        let clock = ManualClock::new(START);
        let store = Arc::new(PausingStore::new(store_key_of("first"), false));
        let (cache, revision) = stale_entry_behind_a_scope_write(store.clone(), &clock).await;
        let refresh = tokio::spawn({
            let cache = cache.clone();
            async move { cache.get("first", lookup_ok("4"), &scopes(&["pr"])).await }
        });
        store.reached.notified().await;

        let request = ReadRequest {
            key: digest("first"),
            revision,
            lookup: lookup_ok("4").shared(),
        };
        let loaded = cache.inner.load_or_lookup(request.clone()).await.unwrap();
        assert_eq!(loaded.payload, "1");
        assert!(cache.inner.memory_get(&request.read_key()).is_none());

        store.release.notify_one();
        assert_eq!(refresh.await.unwrap().unwrap(), "4");
    }

    #[tokio::test]
    async fn a_stale_entry_written_back_during_an_invalidation_is_not_served() {
        // Deterministic interleaving: the removal completes, then a late reader writes the stale
        // entry back to the store before the refresh's final read. The final read must not serve it.
        let clock = ManualClock::new(START);
        let store = Arc::new(PausingStore::new(store_key_of("first"), true));
        let (cache, _) = stale_entry_behind_a_scope_write(store.clone(), &clock).await;
        let refresh = tokio::spawn({
            let cache = cache.clone();
            async move { cache.get("first", lookup_ok("4"), &scopes(&["pr"])).await }
        });
        store.reached.notified().await;

        let stale = serde_json::to_string(&Entry {
            payload: "1".to_owned(),
            expires_at: START + ENTRY_TTL_MILLIS,
            revision: Some(String::new()),
        })
        .unwrap();
        store.set(&store_key_of("first"), &stale).await.unwrap();

        store.release.notify_one();
        assert_eq!(refresh.await.unwrap().unwrap(), "4");
    }

    #[tokio::test]
    async fn prunes_entry_files_written_before_the_max_age_and_keeps_fresh_ones() {
        let directory = tempfile::tempdir().unwrap();
        let directory = directory.path();
        let clock = ManualClock::new(START);
        let cache = open(directory, &clock).await;
        cache
            .get("old", lookup_ok("old"), &scopes(&["pr"]))
            .await
            .unwrap();
        cache.invalidate("pr").await;
        let names = file_names(directory);
        let [stale, revisions] = names.as_slice() else {
            panic!("{names:?}")
        };
        let (stale, revisions) = (stale.clone(), revisions.clone());
        assert_eq!(revisions, "revisions");
        cache.get("fresh", lookup_ok("fresh"), &[]).await.unwrap();
        let fresh = file_names(directory)
            .into_iter()
            .find(|name| *name != stale && *name != revisions)
            .unwrap();
        let unrelated = "unrelated.json";
        std::fs::write(directory.join(unrelated), "{}").unwrap();
        // Starts with the store id but is not an entry file.
        let prefixed = "pr-v2-backup.json";
        std::fs::write(directory.join(prefixed), "{}").unwrap();
        let just_expired = ENTRY_FILE_MAX_AGE + Duration::from_secs(1);
        age_files(
            directory,
            &clock,
            &[&stale, &revisions, unrelated, prefixed],
            just_expired,
        );
        age_files(directory, &clock, &[&fresh], ENTRY_FILE_MAX_AGE);

        prune_expired_entry_files(directory, clock.as_ref())
            .await
            .unwrap();

        let mut expected = vec![fresh, revisions, unrelated.to_owned(), prefixed.to_owned()];
        expected.sort();
        assert_eq!(file_names(directory), expected);
    }

    /// Upstream drives the hourly sweep with `TestClock` and observes file operations. Here the
    /// sweep interval is a real (short) duration and the manual clock decides what is expired.
    #[tokio::test]
    async fn sweeps_the_cache_directory_on_an_interval_once_started() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("pull-requests");
        let clock = ManualClock::new(START);
        let cache = {
            let store = FileSystemKeyValueStore::open(&directory).await.unwrap();
            PullRequestReadCache::new(Arc::new(store), clock.clone())
        };
        cache
            .get("summary", lookup_ok("cached"), &[])
            .await
            .unwrap();
        let entry = file_names(&directory).remove(0);
        // Within the max age at the first two sweeps, past it at the third.
        age_files(
            &directory,
            &clock,
            &[&entry],
            ENTRY_FILE_MAX_AGE - Duration::from_secs(90 * 60),
        );
        let _sweep = spawn_prune_loop(directory.clone(), clock.clone(), Duration::from_millis(10));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(file_names(&directory), vec![entry.clone()]);
        clock.advance(60 * 60 * 1000);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(file_names(&directory), vec![entry.clone()]);
        clock.advance(60 * 60 * 1000);
        let mut removed = false;
        for _ in 0..100 {
            if file_names(&directory).is_empty() {
                removed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(removed, "the entry file was not swept");
    }

    #[tokio::test]
    async fn file_backed_opens_under_pull_requests_and_prunes_at_start() {
        let root = tempfile::tempdir().unwrap();
        let clock = ManualClock::new(START);
        let cache = PullRequestReadCache::file_backed(root.path(), clock.clone()).await;
        cache
            .get("summary", lookup_ok("cached"), &[])
            .await
            .unwrap();
        let directory = root.path().join("pull-requests");
        let entry = file_names(&directory).remove(0);
        assert!(ENTRY_FILE_NAME.is_match(&entry), "{entry}");
    }

    #[tokio::test]
    async fn reuses_files_after_restart_and_respects_the_original_expiry() {
        let directory = tempfile::tempdir().unwrap();
        let directory = directory.path();
        let clock = ManualClock::new(START);
        let reads = Arc::new(AtomicUsize::new(0));
        let lookup = counting(&reads);
        let first = open(directory, &clock).await;
        let key = "long/repository/key".repeat(100);
        assert_eq!(first.get(&key, lookup(), &[]).await.unwrap(), "1");
        clock.advance(59_000);
        let restarted = open(directory, &clock).await;
        assert_eq!(restarted.get(&key, lookup(), &[]).await.unwrap(), "1");
        clock.advance(1_000);
        assert_eq!(restarted.get(&key, lookup(), &[]).await.unwrap(), "2");
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        assert_eq!(file_names(directory).len(), 1);
    }

    #[tokio::test]
    async fn clears_in_flight_reads_before_a_new_service_can_reuse_them() {
        let directory = tempfile::tempdir().unwrap();
        let directory = directory.path();
        let clock = ManualClock::new(START);
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let cache = Arc::new(open(directory, &clock).await);
        let read = tokio::spawn({
            let (cache, started, release) = (cache.clone(), started.clone(), release.clone());
            async move {
                let lookup = async move {
                    started.notify_one();
                    release.notified().await;
                    Ok("old".to_owned())
                }
                .boxed();
                cache.get("summary", lookup, &scopes(&["pr"])).await
            }
        });
        started.notified().await;
        let invalidate = tokio::spawn({
            let cache = cache.clone();
            async move { cache.invalidate("pr").await }
        });
        tokio::task::yield_now().await;
        release.notify_one();
        assert_eq!(read.await.unwrap().unwrap(), "old");
        invalidate.await.unwrap();
        let restarted = open(directory, &clock).await;
        assert_eq!(
            restarted
                .get("summary", lookup_ok("new"), &scopes(&["pr"]))
                .await
                .unwrap(),
            "new"
        );
    }

    #[tokio::test]
    async fn invalidates_only_the_changed_scope_across_restarts_and_coalesces_its_next_reads() {
        let directory = tempfile::tempdir().unwrap();
        let directory = directory.path();
        let clock = ManualClock::new(START);
        let reads = Arc::new(AtomicUsize::new(0));
        let lookup = counting(&reads);
        let cache = open(directory, &clock).await;
        cache
            .get("first", lookup(), &scopes(&["project", "pr-1"]))
            .await
            .unwrap();
        cache
            .get("second", lookup(), &scopes(&["project", "pr-2"]))
            .await
            .unwrap();
        cache
            .get("third", lookup(), &scopes(&["other-project", "pr-3"]))
            .await
            .unwrap();
        cache.invalidate("pr-1").await;
        let restarted = open(directory, &clock).await;
        let first_scopes = scopes(&["project", "pr-1"]);
        let answers = futures::future::join_all(
            (0..10).map(|_| restarted.get("first", lookup(), &first_scopes)),
        )
        .await;
        assert_eq!(
            answers.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
            vec!["4".to_owned(); 10]
        );
        assert_eq!(
            restarted
                .get("second", lookup(), &scopes(&["project", "pr-2"]))
                .await
                .unwrap(),
            "2"
        );
        restarted.invalidate("project").await;
        let again = open(directory, &clock).await;
        assert_eq!(
            again
                .get("second", lookup(), &scopes(&["project", "pr-2"]))
                .await
                .unwrap(),
            "5"
        );
        assert_eq!(
            again
                .get("third", lookup(), &scopes(&["other-project", "pr-3"]))
                .await
                .unwrap(),
            "3"
        );
        assert_eq!(reads.load(Ordering::SeqCst), 5);
        let files = file_names(directory).len();
        for _ in 0..3 {
            again.invalidate("pr-1").await;
            again.get("first", lookup(), &first_scopes).await.unwrap();
        }
        assert_eq!(file_names(directory).len(), files);
    }

    #[tokio::test]
    async fn shares_a_pending_refresh_without_blocking_an_unrelated_cached_pr() {
        let clock = ManualClock::new(START);
        let cache = Arc::new(PullRequestReadCache::memory(clock));
        cache
            .get("first", lookup_ok("old"), &scopes(&["pr-1"]))
            .await
            .unwrap();
        cache
            .get("second", lookup_ok("warm"), &scopes(&["pr-2"]))
            .await
            .unwrap();
        cache.invalidate("pr-1").await;
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let reads = Arc::new(AtomicUsize::new(0));
        let pr_1 = scopes(&["pr-1"]);
        let refresh = {
            let (started, release, reads) = (started.clone(), release.clone(), reads.clone());
            move || {
                let (started, release, reads) = (started.clone(), release.clone(), reads.clone());
                let lookup: PullRequestReadLookup = async move {
                    reads.fetch_add(1, Ordering::SeqCst);
                    started.notify_one();
                    release.notified().await;
                    Ok("fresh".to_owned())
                }
                .boxed();
                lookup
            }
        };
        let pending = tokio::spawn({
            let cache = cache.clone();
            async move {
                futures::future::join_all((0..10).map(|_| cache.get("first", refresh(), &pr_1)))
                    .await
            }
        });
        started.notified().await;
        let miss: PullRequestReadLookup = async { panic!("cache miss") }.boxed();
        assert_eq!(
            cache.get("second", miss, &scopes(&["pr-2"])).await.unwrap(),
            "warm"
        );
        release.notify_one();
        let answers = pending.await.unwrap();
        assert_eq!(
            answers.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
            vec!["fresh".to_owned(); 10]
        );
        assert_eq!(reads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn compacts_expired_scope_records_without_discarding_fresh_pr_data() {
        let directory = tempfile::tempdir().unwrap();
        let directory = directory.path();
        let clock = ManualClock::new(START);
        let cache = open(directory, &clock).await;
        cache
            .get("summary", lookup_ok("old"), &scopes(&["pr"]))
            .await
            .unwrap();
        cache.invalidate("pr").await;
        for index in 0..100 {
            cache.invalidate(&format!("pr-{index}")).await;
        }
        assert_eq!(file_names(directory).len(), 2);
        let size = |directory: &Path| {
            std::fs::metadata(directory.join("revisions"))
                .unwrap()
                .len()
        };
        let before = size(directory);
        clock.advance(59_000);
        assert_eq!(
            cache
                .get("summary", lookup_ok("fresh"), &scopes(&["pr"]))
                .await
                .unwrap(),
            "fresh"
        );
        clock.advance(1_000);
        cache.invalidate("other-pr").await;
        assert!(size(directory) < before);
        let restarted = open(directory, &clock).await;
        let miss: PullRequestReadLookup = async { panic!("cache miss") }.boxed();
        assert_eq!(
            restarted
                .get("summary", miss, &scopes(&["pr"]))
                .await
                .unwrap(),
            "fresh"
        );
    }

    #[tokio::test]
    async fn does_not_persist_failed_github_reads() {
        let directory = tempfile::tempdir().unwrap();
        let directory = directory.path();
        let clock = ManualClock::new(START);
        let cache = open(directory, &clock).await;
        let error = PullRequestOperationError::Operation {
            operation: "summary".to_owned(),
            detail: turin_contracts::TrimmedNonEmptyString::new("unavailable").unwrap(),
            reason: None,
            cause: None,
        };
        let failing: PullRequestReadLookup = {
            let error = error.clone();
            async move { Err(error.into()) }.boxed()
        };
        let failure = cache.get("summary", failing, &[]).await.unwrap_err();
        assert_eq!(failure, PullRequestReadError::Operation(error));
        let restarted = open(directory, &clock).await;
        assert_eq!(
            restarted
                .get("summary", lookup_ok("recovered"), &[])
                .await
                .unwrap(),
            "recovered"
        );
    }

    /// A store whose first `get` of `revisions` fails.
    struct FailFirstGet {
        inner: MemoryKeyValueStore,
        fail: AtomicBool,
    }

    #[async_trait]
    impl KeyValueStore for FailFirstGet {
        async fn get(&self, key: &str) -> Result<Option<String>, KeyValueStoreError> {
            if self.fail.swap(false, Ordering::SeqCst) {
                return Err(KeyValueStoreError {
                    method: "get",
                    message: "unavailable".into(),
                });
            }
            self.inner.get(key).await
        }
        async fn set(&self, key: &str, value: &str) -> Result<(), KeyValueStoreError> {
            self.inner.set(key, value).await
        }
        async fn remove(&self, key: &str) -> Result<(), KeyValueStoreError> {
            self.inner.remove(key).await
        }
    }

    #[tokio::test]
    async fn resumes_caching_after_a_failed_scope_read() {
        let clock = ManualClock::new(START);
        let store = FailFirstGet {
            inner: MemoryKeyValueStore::new(),
            fail: AtomicBool::new(true),
        };
        let cache = PullRequestReadCache::new(Arc::new(store), clock);
        let reads = Arc::new(AtomicUsize::new(0));
        let lookup = counting(&reads);
        let pr = scopes(&["pr"]);
        assert_eq!(cache.get("summary", lookup(), &pr).await.unwrap(), "1");
        assert_eq!(cache.get("summary", lookup(), &pr).await.unwrap(), "2");
        assert_eq!(cache.get("summary", lookup(), &pr).await.unwrap(), "2");
    }

    #[tokio::test]
    async fn cancels_abandoned_reads_without_blocking_invalidation() {
        let clock = ManualClock::new(START);
        let cache = Arc::new(PullRequestReadCache::memory(clock));
        let started = Arc::new(Notify::new());
        let read = tokio::spawn({
            let (cache, started) = (cache.clone(), started.clone());
            async move {
                let lookup: PullRequestReadLookup = async move {
                    started.notify_one();
                    futures::future::pending::<PullRequestReadResult>().await
                }
                .boxed();
                cache.get("summary", lookup, &scopes(&["pr"])).await
            }
        });
        started.notified().await;
        read.abort();
        let _ = read.await;
        cache.invalidate("pr").await;
        assert_eq!(
            cache
                .get("summary", lookup_ok("fresh"), &scopes(&["pr"]))
                .await
                .unwrap(),
            "fresh"
        );
    }

    /// A store that parks after writing `revisions`, until released.
    struct ParkAfterRevisionWrite {
        inner: MemoryKeyValueStore,
        written: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait]
    impl KeyValueStore for ParkAfterRevisionWrite {
        async fn get(&self, key: &str) -> Result<Option<String>, KeyValueStoreError> {
            self.inner.get(key).await
        }
        async fn set(&self, key: &str, value: &str) -> Result<(), KeyValueStoreError> {
            self.inner.set(key, value).await?;
            if key == "revisions" {
                self.written.notify_one();
                self.release.notified().await;
            }
            Ok(())
        }
        async fn remove(&self, key: &str) -> Result<(), KeyValueStoreError> {
            self.inner.remove(key).await
        }
    }

    #[tokio::test]
    async fn finishes_the_in_memory_revision_update_when_invalidation_is_canceled_after_writing() {
        let clock = ManualClock::new(START);
        let written = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let store = ParkAfterRevisionWrite {
            inner: MemoryKeyValueStore::new(),
            written: written.clone(),
            release: release.clone(),
        };
        let cache = Arc::new(PullRequestReadCache::new(Arc::new(store), clock));
        cache
            .get("summary", lookup_ok("old"), &scopes(&["pr"]))
            .await
            .unwrap();
        let invalidation = tokio::spawn({
            let cache = cache.clone();
            async move { cache.invalidate("pr").await }
        });
        written.notified().await;
        invalidation.abort();
        let _ = invalidation.await;
        release.notify_one();
        assert_eq!(
            cache
                .get("summary", lookup_ok("fresh"), &scopes(&["pr"]))
                .await
                .unwrap(),
            "fresh"
        );
    }
}
