//! Port of upstream/packages/client-runtime/src/state/fileMetadata.ts
//!
//! The metadata cache behind file chips, search results and the file tree.
//! Upstream runs it as Effect atoms over an RPC session. Turin is one process,
//! so the RPC call becomes [`FilesystemMetadataSource`] and the atom families
//! become [`FileMetadataCache`]. The cache rules stay: 60 s freshness, a
//! 1024-entry cache, per-thread retention that outlives expiry, invalidation
//! that re-requests only the paths written while their response was in
//! flight, listing kinds that seed without a request, and a reconnect that
//! drops everything.
//!
//! Deviations: one environment, so the environment key is gone. Atom refreshes
//! (`refreshPath`, `refreshTrigger`) and the same-tick batch window are not
//! ported. Callers hand every path they need to [`FileMetadataCache::metadata_many`],
//! which dedupes and splits the misses into `FILESYSTEM_METADATA_BATCH_LIMIT`
//! chunks itself. `seedAfterRead` has no caller in Turin yet and is not ported.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use indexmap::IndexMap;
use parking_lot::Mutex;
use turin_contracts::filesystem::{
    FILESYSTEM_METADATA_BATCH_LIMIT, FilesystemEntryKind, FilesystemEntryMetadata,
};

use turin_shared::path::{normalize_project_path_for_comparison, resolve_workspace_file_path};

pub const METADATA_STALE_TIME_MS: u64 = 60_000;
pub const METADATA_CACHE_LIMIT: usize = 1024;

pub type FileMetadata = FilesystemEntryMetadata;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("filesystem metadata request failed: {0}")]
pub struct FilesystemMetadataError(pub String);

/// The `filesystem.getMetadata` RPC. Results follow request order; `None`
/// means the path is missing, unreadable, or unsupported.
#[async_trait]
pub trait FilesystemMetadataSource: Send + Sync {
    async fn get_metadata(
        &self,
        paths: &[String],
    ) -> Result<Vec<Option<FileMetadata>>, FilesystemMetadataError>;
}

type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

#[derive(Clone)]
struct CachedMetadata {
    value: Option<FileMetadata>,
    expires: u64,
}

#[derive(Default)]
struct RetainedThread {
    readers: usize,
    entries: HashMap<String, CachedMetadata>,
}

struct InFlight {
    path: String,
    valid: bool,
}

#[derive(Default)]
struct State {
    cache: IndexMap<String, CachedMetadata>,
    retained: HashMap<String, RetainedThread>,
    in_flight: HashMap<u64, InFlight>,
    next_token: u64,
}

/// Keeps a thread's visited paths readable after they expire. Dropping the
/// last guard for a thread releases its paths.
pub struct RetainedFileMetadata {
    state: Arc<Mutex<State>>,
    thread_id: String,
}

impl Drop for RetainedFileMetadata {
    fn drop(&mut self) {
        let mut state = self.state.lock();
        if let Some(thread) = state.retained.get_mut(&self.thread_id) {
            thread.readers -= 1;
            if thread.readers == 0 {
                state.retained.remove(&self.thread_id);
            }
        }
    }
}

#[derive(Clone)]
pub struct FileMetadataCache {
    state: Arc<Mutex<State>>,
    source: Arc<dyn FilesystemMetadataSource>,
    clock: Clock,
}

impl FileMetadataCache {
    pub fn new(source: Arc<dyn FilesystemMetadataSource>) -> Self {
        Self::with_clock(source, Arc::new(wall_clock_millis))
    }

    pub fn with_clock(source: Arc<dyn FilesystemMetadataSource>, clock: Clock) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            source,
            clock,
        }
    }

    /// `retainThread`: the thread's visited paths stay readable until the guard drops.
    pub fn retain_thread(&self, thread_id: &str) -> RetainedFileMetadata {
        let mut state = self.state.lock();
        state
            .retained
            .entry(thread_id.to_owned())
            .or_default()
            .readers += 1;
        RetainedFileMetadata {
            state: self.state.clone(),
            thread_id: thread_id.to_owned(),
        }
    }

    /// One path. Batches of one; see [`Self::metadata_many`].
    pub async fn metadata(
        &self,
        path: &str,
        thread_id: Option<&str>,
    ) -> Result<Option<FileMetadata>, FilesystemMetadataError> {
        let mut values = self
            .metadata_many(std::slice::from_ref(&path.to_owned()), thread_id)
            .await?;
        Ok(values.pop().flatten())
    }

    /// Metadata for each path, in request order. Cache and retained hits are
    /// answered without a request. Misses are deduped and sent in chunks of
    /// [`FILESYSTEM_METADATA_BATCH_LIMIT`]. A path invalidated while its chunk
    /// was in flight is requested again, never completed with the old value.
    pub async fn metadata_many(
        &self,
        paths: &[String],
        thread_id: Option<&str>,
    ) -> Result<Vec<Option<FileMetadata>>, FilesystemMetadataError> {
        let keys: Vec<String> = paths
            .iter()
            .map(|path| normalize_project_path_for_comparison(path))
            .collect();
        let mut resolved: HashMap<String, Option<FileMetadata>> = HashMap::new();
        let mut seen = HashSet::new();
        let mut pending: Vec<String> = keys
            .iter()
            .filter(|key| seen.insert(key.as_str()))
            .cloned()
            .collect();
        while !pending.is_empty() {
            let misses = self.resolve_cached(&pending, thread_id, &mut resolved);
            let mut retry = Vec::new();
            for chunk in misses.chunks(FILESYSTEM_METADATA_BATCH_LIMIT) {
                let tokens = self.begin_request(chunk);
                let response = match self.source.get_metadata(chunk).await {
                    Ok(response) => response,
                    Err(error) => {
                        self.end_request(&tokens);
                        return Err(error);
                    }
                };
                let now = (self.clock)();
                let mut state = self.state.lock();
                for (index, (path, token)) in chunk.iter().zip(&tokens).enumerate() {
                    let valid = state
                        .in_flight
                        .remove(token)
                        .is_some_and(|flight| flight.valid);
                    if !valid {
                        retry.push(path.clone());
                        continue;
                    }
                    let value = response.get(index).cloned().flatten();
                    let entry = CachedMetadata {
                        value: value.clone(),
                        expires: now + METADATA_STALE_TIME_MS,
                    };
                    remember(&mut state, path, value.clone(), now, false);
                    if let Some(thread) = thread_id.and_then(|id| state.retained.get_mut(id)) {
                        thread.entries.insert(path.clone(), entry);
                    }
                    resolved.insert(path.clone(), value);
                }
            }
            pending = retry;
        }
        Ok(keys
            .iter()
            .map(|key| resolved.get(key).cloned().flatten())
            .collect())
    }

    /// `rememberEntries`: a directory listing seeds kinds without a request.
    /// Size and MIME hints are optional and come from a later read.
    pub fn remember_entries(&self, cwd: &str, entries: &[(String, FilesystemEntryKind)]) {
        let now = (self.clock)();
        let mut state = self.state.lock();
        for (path, kind) in entries {
            let key =
                normalize_project_path_for_comparison(&resolve_workspace_file_path(path, cwd));
            if known_entry(&state, &key, now)
                .and_then(|entry| entry.value.as_ref())
                .map(|value| value.kind)
                == Some(*kind)
            {
                continue;
            }
            let value = FileMetadata {
                kind: *kind,
                byte_length: None,
                mime_type: None,
            };
            remember(&mut state, &key, Some(value), now, true);
        }
    }

    /// `rememberFile`: a file read sets its size, keeping a known MIME hint.
    pub fn remember_file(&self, cwd: &str, relative_path: &str, byte_length: u64) {
        let key =
            normalize_project_path_for_comparison(&resolve_workspace_file_path(relative_path, cwd));
        let now = (self.clock)();
        let mut state = self.state.lock();
        let previous = known_entry(&state, &key, now).and_then(|entry| entry.value.clone());
        let value = FileMetadata {
            kind: FilesystemEntryKind::File,
            byte_length: Some(byte_length),
            mime_type: previous.and_then(|value| value.mime_type),
        };
        remember(&mut state, &key, Some(value), now, true);
    }

    /// `invalidate`: drops the path everywhere. Responses still in flight for
    /// it are re-requested, and retained threads re-read it on their next visit.
    pub fn invalidate(&self, path: &str) {
        let key = normalize_project_path_for_comparison(path);
        let mut state = self.state.lock();
        for flight in state.in_flight.values_mut() {
            if flight.path == key {
                flight.valid = false;
            }
        }
        state.cache.shift_remove(&key);
        for thread in state.retained.values_mut() {
            thread.entries.remove(&key);
        }
    }

    /// A reconnect or grant change: nothing cached or retained is trusted, and
    /// in-flight responses are discarded.
    pub fn reset(&self) {
        let mut state = self.state.lock();
        state.cache.clear();
        for thread in state.retained.values_mut() {
            thread.entries.clear();
        }
        for flight in state.in_flight.values_mut() {
            flight.valid = false;
        }
    }

    /// Answers what the cache can, removes answered paths from `pending`'s
    /// results, and returns the paths that still need a request.
    fn resolve_cached(
        &self,
        pending: &[String],
        thread_id: Option<&str>,
        resolved: &mut HashMap<String, Option<FileMetadata>>,
    ) -> Vec<String> {
        let now = (self.clock)();
        let mut state = self.state.lock();
        let mut misses = Vec::new();
        for key in pending {
            if let Some(value) = lookup(&mut state, key, thread_id, now) {
                resolved.insert(key.clone(), value);
            } else {
                misses.push(key.clone());
            }
        }
        misses
    }

    fn begin_request(&self, paths: &[String]) -> Vec<u64> {
        let mut state = self.state.lock();
        paths
            .iter()
            .map(|path| {
                let token = state.next_token;
                state.next_token += 1;
                state.in_flight.insert(
                    token,
                    InFlight {
                        path: path.clone(),
                        valid: true,
                    },
                );
                token
            })
            .collect()
    }

    fn end_request(&self, tokens: &[u64]) {
        let mut state = self.state.lock();
        for token in tokens {
            state.in_flight.remove(token);
        }
    }
}

/// `knownEntry`: an unexpired cache entry.
fn known_entry<'a>(state: &'a State, key: &str, now: u64) -> Option<&'a CachedMetadata> {
    state.cache.get(key).filter(|entry| entry.expires > now)
}

/// `execute`'s lookup order: the thread's retained entry (even if expired),
/// then an unexpired cache entry, which is copied into the retained map.
fn lookup(
    state: &mut State,
    key: &str,
    thread_id: Option<&str>,
    now: u64,
) -> Option<Option<FileMetadata>> {
    if let Some(entry) = thread_id
        .and_then(|id| state.retained.get(id))
        .and_then(|thread| thread.entries.get(key))
    {
        return Some(entry.value.clone());
    }
    let cached = known_entry(state, key, now).cloned()?;
    if let Some(thread) = thread_id.and_then(|id| state.retained.get_mut(id)) {
        thread.entries.insert(key.to_owned(), cached.clone());
    }
    Some(cached.value)
}

/// `remember`: stores a value and refreshes it in every retained thread that
/// already holds the path. A partial value of the same kind merges into what
/// the thread saw, so a listing refresh keeps MIME hints.
fn remember(state: &mut State, key: &str, value: Option<FileMetadata>, now: u64, partial: bool) {
    let expires = now + METADATA_STALE_TIME_MS;
    state.cache.shift_remove(key);
    state.cache.insert(
        key.to_owned(),
        CachedMetadata {
            value: value.clone(),
            expires,
        },
    );
    for thread in state.retained.values_mut() {
        let Some(previous) = thread
            .entries
            .get(key)
            .and_then(|entry| entry.value.clone())
        else {
            continue;
        };
        let merged = match &value {
            Some(next) if partial && previous.kind == next.kind => Some(FileMetadata {
                kind: next.kind,
                byte_length: next.byte_length.or(previous.byte_length),
                mime_type: next.mime_type.clone().or(previous.mime_type),
            }),
            _ => value.clone(),
        };
        thread.entries.insert(
            key.to_owned(),
            CachedMetadata {
                value: merged,
                expires,
            },
        );
    }
    if state.cache.len() > METADATA_CACHE_LIMIT {
        state.cache.shift_remove_index(0);
    }
}

fn wall_clock_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use tokio::sync::Notify;
    use turin_contracts::base_schemas::TrimmedNonEmptyString;

    use super::*;

    /// Upstream's `values` map plus the batch log `h.batches` reads.
    #[derive(Default)]
    struct FakeSource {
        values: Mutex<HashMap<String, FileMetadata>>,
        batches: Mutex<Vec<Vec<String>>>,
        denied: AtomicBool,
        gate: Mutex<Option<Arc<Gate>>>,
    }

    /// Holds the first request open so a test can change state mid-flight.
    struct Gate {
        started: Notify,
        release: Notify,
    }

    #[async_trait]
    impl FilesystemMetadataSource for FakeSource {
        async fn get_metadata(
            &self,
            paths: &[String],
        ) -> Result<Vec<Option<FileMetadata>>, FilesystemMetadataError> {
            self.batches.lock().push(paths.to_vec());
            if self.denied.load(Ordering::SeqCst) {
                return Err(FilesystemMetadataError(
                    "File access is not granted.".into(),
                ));
            }
            let gate = self.gate.lock().take();
            if let Some(gate) = gate {
                gate.started.notify_one();
                gate.release.notified().await;
            }
            let values = self.values.lock();
            Ok(paths.iter().map(|path| values.get(path).cloned()).collect())
        }
    }

    struct Harness {
        source: Arc<FakeSource>,
        cache: FileMetadataCache,
        now: Arc<AtomicU64>,
    }

    const HOUR_MS: u64 = 60 * 60 * 1000;

    fn harness() -> Harness {
        let source = Arc::new(FakeSource::default());
        let now = Arc::new(AtomicU64::new(1_000_000));
        let clock = {
            let now = now.clone();
            Arc::new(move || now.load(Ordering::SeqCst)) as Clock
        };
        let cache = FileMetadataCache::with_clock(source.clone(), clock);
        Harness { source, cache, now }
    }

    impl Harness {
        fn set(&self, path: &str, value: FileMetadata) {
            self.source.values.lock().insert(path.to_owned(), value);
        }

        fn batches(&self) -> Vec<Vec<String>> {
            self.source.batches.lock().clone()
        }

        fn advance(&self, ms: u64) {
            self.now.fetch_add(ms, Ordering::SeqCst);
        }

        async fn read(&self, path: &str, thread_id: Option<&str>) -> Option<FileMetadata> {
            self.cache.metadata(path, thread_id).await.unwrap()
        }
    }

    fn file(byte_length: u64) -> FileMetadata {
        FileMetadata {
            kind: FilesystemEntryKind::File,
            byte_length: Some(byte_length),
            mime_type: None,
        }
    }

    fn directory() -> FileMetadata {
        FileMetadata {
            kind: FilesystemEntryKind::Directory,
            byte_length: None,
            mime_type: None,
        }
    }

    fn mime(value: &str) -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(value).unwrap()
    }

    #[tokio::test]
    async fn does_not_seed_a_different_path_from_a_literal_name_with_a_numeric_suffix() {
        let h = harness();
        h.cache.remember_entries(
            "/workspace",
            &[("report:123".into(), FilesystemEntryKind::Directory)],
        );
        h.set("/workspace/report", file(5));
        assert_eq!(h.read("/workspace/report", None).await, Some(file(5)));
        assert_eq!(
            h.read("/workspace/report:123", None).await,
            Some(directory())
        );
        h.cache.remember_file("/workspace", "read:12:3", 100);
        assert_eq!(h.read("/workspace/read:12:3", None).await, Some(file(100)));
        assert_eq!(h.read("/workspace/read", None).await, None);
        assert_eq!(
            h.batches(),
            vec![
                vec!["/workspace/report".to_owned()],
                vec!["/workspace/read".to_owned()]
            ]
        );
    }

    #[tokio::test]
    async fn retries_only_paths_invalidated_while_their_response_is_in_flight() {
        let h = harness();
        let gate = Arc::new(Gate {
            started: Notify::new(),
            release: Notify::new(),
        });
        *h.source.gate.lock() = Some(gate.clone());
        h.set("/workspace/file", file(5));
        h.set("/workspace/unchanged", directory());
        let thread = "in-flight-thread";
        let _retained = h.cache.retain_thread(thread);
        let cache = h.cache.clone();
        let read = tokio::spawn(async move {
            cache
                .metadata_many(
                    &[
                        "/workspace/file".to_owned(),
                        "/workspace/unchanged".to_owned(),
                    ],
                    Some(thread),
                )
                .await
                .unwrap()
        });
        gate.started.notified().await;
        h.cache.invalidate("/workspace/file");
        h.set("/workspace/file", file(10));
        gate.release.notify_one();
        assert_eq!(read.await.unwrap(), vec![Some(file(10)), Some(directory())]);
        assert_eq!(
            h.batches(),
            vec![
                vec![
                    "/workspace/file".to_owned(),
                    "/workspace/unchanged".to_owned()
                ],
                vec!["/workspace/file".to_owned()],
            ]
        );
        assert_eq!(
            h.read("/workspace/file", Some(thread)).await,
            Some(file(10))
        );
        assert_eq!(h.batches().len(), 2);
    }

    #[tokio::test]
    async fn does_not_reuse_expired_metadata_retained_by_an_unrelated_thread() {
        let h = harness();
        let _retained = h.cache.retain_thread("unrelated-thread");
        h.set("/workspace/file", file(5));
        h.read("/workspace/file", Some("unrelated-thread")).await;
        h.advance(HOUR_MS);
        h.set("/workspace/file", directory());
        assert_eq!(h.read("/workspace/file", None).await, Some(directory()));
        assert_eq!(h.batches().len(), 2);
    }

    #[tokio::test]
    async fn uses_a_file_listing_kind_without_requesting_optional_metadata() {
        let h = harness();
        h.cache
            .remember_entries("/workspace", &[("run".into(), FilesystemEntryKind::File)]);
        assert_eq!(
            h.read("/workspace/run", None).await,
            Some(FileMetadata {
                kind: FilesystemEntryKind::File,
                byte_length: None,
                mime_type: None,
            })
        );
        assert!(h.batches().is_empty());
    }

    #[tokio::test]
    async fn tree_refreshes_preserve_mime_hints_retained_by_an_open_thread() {
        let h = harness();
        let thread = "rich-metadata-thread";
        let _retained = h.cache.retain_thread(thread);
        let value = FileMetadata {
            kind: FilesystemEntryKind::File,
            byte_length: Some(5),
            mime_type: Some(mime("text/x-python")),
        };
        h.set("/workspace/run", value.clone());
        h.read("/workspace/run", Some(thread)).await;
        h.advance(HOUR_MS);
        h.cache
            .remember_entries("/workspace", &[("run".into(), FilesystemEntryKind::File)]);
        assert_eq!(h.read("/workspace/run", Some(thread)).await, Some(value));
        assert_eq!(h.batches().len(), 1);
    }

    #[tokio::test]
    async fn an_offscreen_invalidation_waits_until_that_path_is_read_again() {
        let h = harness();
        let thread = "offscreen-thread";
        let _retained = h.cache.retain_thread(thread);
        h.set("/workspace/file", file(5));
        h.read("/workspace/file", Some(thread)).await;
        h.cache.invalidate("/workspace/file");
        assert_eq!(h.batches().len(), 1);
        h.set("/workspace/file", directory());
        assert_eq!(
            h.read("/workspace/file", Some(thread)).await,
            Some(directory())
        );
        assert_eq!(h.batches().len(), 2);
    }

    #[tokio::test]
    async fn retains_more_than_1024_visited_paths_until_the_thread_closes() {
        let h = harness();
        let thread = "large-thread";
        let retained = h.cache.retain_thread(thread);
        let paths: Vec<String> = (0..1300)
            .map(|i| format!("/workspace/visited-{i}"))
            .collect();
        h.cache.metadata_many(&paths, Some(thread)).await.unwrap();
        assert_eq!(h.batches().len(), 21);
        h.advance(HOUR_MS);
        h.cache.metadata_many(&paths, Some(thread)).await.unwrap();
        assert_eq!(h.batches().len(), 21);
        drop(retained);
        assert_eq!(h.read(&paths[0], Some(thread)).await, None);
        assert_eq!(h.batches().len(), 22);
    }

    #[tokio::test]
    async fn closing_one_thread_does_not_release_another_threads_retained_paths() {
        let h = harness();
        let a = h.cache.retain_thread("thread-a");
        let _b = h.cache.retain_thread("thread-b");
        h.set("/workspace/shared", file(5));
        h.read("/workspace/shared", Some("thread-a")).await;
        h.read("/workspace/shared", Some("thread-b")).await;
        assert_eq!(h.batches().len(), 1);
        drop(a);
        h.advance(HOUR_MS);
        h.read("/workspace/shared", Some("thread-b")).await;
        assert_eq!(h.batches().len(), 1);
    }

    #[tokio::test]
    async fn invalidates_a_changed_retained_path_without_refetching_other_paths() {
        let h = harness();
        let thread = "changing-thread";
        let _retained = h.cache.retain_thread(thread);
        h.set("/workspace/file", file(5));
        h.read("/workspace/file", Some(thread)).await;
        h.read("/workspace/unchanged", Some(thread)).await;
        let before = h.batches().len();
        h.set("/workspace/file", file(10));
        h.cache.invalidate("/workspace/file");
        assert_eq!(
            h.read("/workspace/file", Some(thread)).await,
            Some(file(10))
        );
        h.read("/workspace/unchanged", Some(thread)).await;
        assert_eq!(h.batches()[before..], [vec!["/workspace/file".to_owned()]]);
    }

    #[tokio::test]
    async fn a_reconnect_discards_retained_metadata_from_the_previous_session() {
        let h = harness();
        let thread = "reconnected-thread";
        let _retained = h.cache.retain_thread(thread);
        h.set("/workspace/file", file(5));
        h.read("/workspace/file", Some(thread)).await;
        h.set("/workspace/file", directory());
        h.cache.reset();
        assert_eq!(
            h.read("/workspace/file", Some(thread)).await,
            Some(directory())
        );
        assert_eq!(h.batches().len(), 2);
    }

    #[tokio::test]
    async fn settles_a_failed_batch_and_can_retry_after_access_is_granted() {
        let h = harness();
        h.source.denied.store(true, Ordering::SeqCst);
        let failed = h
            .cache
            .metadata_many(
                &["/workspace/a".to_owned(), "/workspace/b".to_owned()],
                None,
            )
            .await;
        assert!(failed.is_err());
        h.source.denied.store(false, Ordering::SeqCst);
        h.set("/workspace/a", directory());
        assert_eq!(h.read("/workspace/a", None).await, Some(directory()));
    }

    #[tokio::test]
    async fn expires_cached_metadata_and_refreshes_a_changed_kind() {
        let h = harness();
        h.set("/workspace/path", file(5));
        h.read("/workspace/path", None).await;
        h.set("/workspace/path", directory());
        h.advance(61_000);
        assert_eq!(h.read("/workspace/path", None).await, Some(directory()));
        assert_eq!(h.batches().len(), 2);
    }

    #[tokio::test]
    async fn reuses_file_read_sizes_and_normalizes_windows_cache_keys() {
        let h = harness();
        h.cache.remember_file("C:\\Workspace", "file", 100);
        assert_eq!(h.read("c:/workspace/file/", None).await, Some(file(100)));
        assert!(h.batches().is_empty());
    }

    #[tokio::test]
    async fn batches_mounted_paths_and_deduplicates_repeated_chips() {
        let h = harness();
        h.set("/workspace/file", file(5));
        h.set("/workspace/folder.ts", directory());
        let results = h
            .cache
            .metadata_many(
                &[
                    "/workspace/file".to_owned(),
                    "/workspace/folder.ts".to_owned(),
                    "/workspace/file".to_owned(),
                ],
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            h.batches(),
            vec![vec![
                "/workspace/file".to_owned(),
                "/workspace/folder.ts".to_owned()
            ]]
        );
        assert_eq!(
            results,
            vec![Some(file(5)), Some(directory()), Some(file(5))]
        );
        h.read("/workspace/file", None).await;
        assert_eq!(h.batches().len(), 1);
    }

    #[tokio::test]
    async fn uses_search_and_tree_kinds_without_another_request() {
        let h = harness();
        h.cache.remember_entries(
            "/workspace",
            &[("folder.ts".into(), FilesystemEntryKind::Directory)],
        );
        assert_eq!(
            h.read("/workspace/folder.ts", None).await,
            Some(directory())
        );
        assert!(h.batches().is_empty());
    }

    #[tokio::test]
    async fn splits_large_mounts_into_bounded_batches_and_caches_misses() {
        let h = harness();
        let paths: Vec<String> = (0..130).map(|i| format!("/workspace/{i}")).collect();
        h.cache.metadata_many(&paths, None).await.unwrap();
        let sizes: Vec<usize> = h.batches().iter().map(Vec::len).collect();
        assert_eq!(sizes, vec![64, 64, 2]);
        assert_eq!(h.read("/workspace/0", None).await, None);
        assert_eq!(h.batches().len(), 3);
    }

    #[tokio::test]
    async fn rechecks_metadata_after_the_environment_session_changes() {
        let h = harness();
        h.set("/workspace/path", file(5));
        assert_eq!(h.read("/workspace/path", None).await, Some(file(5)));
        h.set("/workspace/path", directory());
        h.cache.reset();
        assert_eq!(h.read("/workspace/path", None).await, Some(directory()));
        assert_eq!(h.batches().len(), 2);
    }

    #[test]
    fn remember_respects_the_cache_limit_by_dropping_the_oldest_entry() {
        let h = harness();
        let entries: Vec<(String, FilesystemEntryKind)> = (0..=METADATA_CACHE_LIMIT)
            .map(|i| (format!("/workspace/{i}"), FilesystemEntryKind::File))
            .collect();
        h.cache.remember_entries("/", &entries);
        let state = h.cache.state.lock();
        assert_eq!(state.cache.len(), METADATA_CACHE_LIMIT);
        assert!(!state.cache.contains_key("/workspace/0"));
    }
}
