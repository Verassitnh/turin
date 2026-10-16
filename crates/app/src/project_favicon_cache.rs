//! Port of upstream/packages/client-runtime/src/projectFaviconCache.ts
//! (`createProjectFaviconCache`, `createProjectFaviconImageLoader`) and its test file.
//!
//! Stores small, self-contained images so startup never needs an old signed URL. The cache keeps
//! its entries in memory and writes each change through a [`ProjectFaviconStorage`]
//! (`project_favicon_storage.rs`: files under the app data dir, where the web client has
//! IndexedDB). Everything that can wait is async: a resolve hydrates once, reads from the loader
//! (an HTTP or file fetch plus a downscale off the UI thread), and persists in the background;
//! [`ProjectFaviconCache::flush`] waits for those writes.
//!
//! `AbortSignal` is a [`CancellationToken`]. The cache must be used inside a tokio runtime, which
//! runs its persistence tasks.

#![allow(dead_code)]

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use base64::Engine as _;
use futures::StreamExt as _;
use futures::stream::BoxStream;
use indexmap::IndexMap;
use parking_lot::Mutex;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::sync::{OnceCell, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use turin_contracts::EnvironmentId;
use turin_shared::file_preview::media_mime_type;
use turin_shared::project_favicon::{
    get_project_favicon_cache_key, get_project_favicon_resource_key,
    is_project_favicon_fallback_url,
};

pub const PROJECT_FAVICON_THUMBNAIL_SIZE: u32 = 96;
pub const PROJECT_FAVICON_MAX_DATA_URL_LENGTH: usize = 32 * 1024;
/// Larger sources are not worth decoding for an icon and are left to the remote URL.
pub const PROJECT_FAVICON_MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
pub const PROJECT_FAVICON_CACHE_MAX_BYTES: usize = 1024 * 1024;
pub const PROJECT_FAVICON_CACHE_MAX_ENTRIES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectFaviconTarget {
    pub environment_id: EnvironmentId,
    pub cwd: String,
    pub favicon_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFaviconEntry {
    pub environment_id: EnvironmentId,
    pub cwd: String,
    pub favicon_path: Option<String>,
    pub revision: String,
    pub data_url: String,
}

/// `ImageDataUrl`: bounded, and one of the image types an icon can be.
static IMAGE_DATA_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^data:image/(?:png|jpeg|gif|webp|avif|svg\+xml|x-icon|vnd\.microsoft\.icon);base64,[A-Za-z0-9+/]+={0,2}$",
    )
    .expect("image data url pattern")
});

pub fn is_image_data_url(value: &str) -> bool {
    value.len() <= PROJECT_FAVICON_MAX_DATA_URL_LENGTH && IMAGE_DATA_URL.is_match(value)
}

/// `decodeEntry`: a record that fails validation is not an entry.
fn decode_entry(record: serde_json::Value) -> Option<ProjectFaviconEntry> {
    let entry: ProjectFaviconEntry = serde_json::from_value(record).ok()?;
    is_image_data_url(&entry.data_url).then_some(entry)
}

fn key_for(target: &ProjectFaviconTarget) -> String {
    get_project_favicon_resource_key(
        target.environment_id.as_str(),
        &target.cwd,
        target.favicon_path.as_deref(),
    )
}

fn key_for_entry(entry: &ProjectFaviconEntry) -> String {
    get_project_favicon_resource_key(
        entry.environment_id.as_str(),
        &entry.cwd,
        entry.favicon_path.as_deref(),
    )
}

#[async_trait]
pub trait ProjectFaviconStorage: Send + Sync {
    /// Every persisted record; entries that fail validation are ignored.
    async fn list(&self) -> Result<Vec<serde_json::Value>, String>;
    async fn put(&self, key: &str, entry: &ProjectFaviconEntry) -> Result<(), String>;
    async fn remove(&self, key: &str, entry: &ProjectFaviconEntry) -> Result<(), String>;
}

/// `load(url, signal)`: the data URL of the image at `url`.
#[async_trait]
pub trait ProjectFaviconLoader: Send + Sync {
    async fn load(&self, url: &str, signal: &CancellationToken) -> Result<String, String>;
}

struct Inner {
    entries: IndexMap<String, ProjectFaviconEntry>,
    environment_revisions: HashMap<String, u64>,
    generation: u64,
}

pub struct ProjectFaviconCache {
    storage: Arc<dyn ProjectFaviconStorage>,
    loader: Arc<dyn ProjectFaviconLoader>,
    inner: Arc<Mutex<Inner>>,
    hydration: OnceCell<()>,
    /// Held for writing while a clear removes entries, and read-locked for an instant by every
    /// resolve: a download that starts during a clear waits for it, so it cannot repopulate
    /// storage.
    clearing: RwLock<()>,
    pending: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl ProjectFaviconCache {
    pub fn new(
        storage: Arc<dyn ProjectFaviconStorage>,
        loader: Arc<dyn ProjectFaviconLoader>,
    ) -> Self {
        Self {
            storage,
            loader,
            inner: Arc::new(Mutex::new(Inner {
                entries: IndexMap::new(),
                environment_revisions: HashMap::new(),
                generation: 0,
            })),
            hydration: OnceCell::new(),
            clearing: RwLock::new(()),
            pending: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// `persist`: keep the in-memory image if local storage is full or unavailable.
    fn persist(&self, operation: impl Future<Output = Result<(), String>> + Send + 'static) {
        let task = tokio::spawn(async move {
            let _ = operation.await;
        });
        let mut pending = self.pending.lock();
        pending.retain(|handle| !handle.is_finished());
        pending.push(task);
    }

    fn remove(&self, key: &str) {
        let Some(entry) = self.inner.lock().entries.shift_remove(key) else {
            return;
        };
        let storage = self.storage.clone();
        let key = key.to_owned();
        self.persist(async move { storage.remove(&key, &entry).await });
    }

    fn trim(&self) {
        loop {
            let oldest = {
                let inner = self.inner.lock();
                let bytes: usize = inner
                    .entries
                    .values()
                    .map(|entry| entry.data_url.len())
                    .sum();
                if inner.entries.len() <= PROJECT_FAVICON_CACHE_MAX_ENTRIES
                    && bytes <= PROJECT_FAVICON_CACHE_MAX_BYTES
                {
                    return;
                }
                match inner.entries.first() {
                    Some((key, _)) => key.clone(),
                    None => return,
                }
            };
            self.remove(&oldest);
        }
    }

    /// Reads the persisted records once; a missing, corrupt, or unavailable cache must not
    /// prevent startup.
    pub async fn hydrate(&self) {
        self.hydration
            .get_or_init(|| async {
                if let Ok(records) = self.storage.list().await {
                    {
                        let mut inner = self.inner.lock();
                        for record in records {
                            if let Some(entry) = decode_entry(record) {
                                inner.entries.insert(key_for_entry(&entry), entry);
                            }
                        }
                    }
                    self.trim();
                }
            })
            .await;
    }

    pub fn peek(&self, target: &ProjectFaviconTarget) -> Option<String> {
        self.inner
            .lock()
            .entries
            .get(&key_for(target))
            .map(|entry| entry.data_url.clone())
    }

    fn environment_revision(&self, environment_id: &EnvironmentId) -> u64 {
        self.inner
            .lock()
            .environment_revisions
            .get(environment_id.as_str())
            .copied()
            .unwrap_or(0)
    }

    pub async fn resolve(
        &self,
        target: &ProjectFaviconTarget,
        url: Option<&str>,
        signal: &CancellationToken,
    ) -> Option<String> {
        drop(self.clearing.read().await);
        let start_generation = self.inner.lock().generation;
        let start_revision = self.environment_revision(&target.environment_id);
        self.hydrate().await;
        let Some(url) = url.filter(|_| !signal.is_cancelled()) else {
            return self.peek(target);
        };
        let key = key_for(target);
        if is_project_favicon_fallback_url(Some(url)) {
            self.remove(&key);
            return None;
        }
        let revision =
            get_project_favicon_cache_key(target.environment_id.as_str(), &target.cwd, url);
        {
            // A use moves the entry to the newest end, which is what `trim` keeps.
            let mut inner = self.inner.lock();
            if let Some(cached) = inner.entries.shift_remove(&key) {
                inner.entries.insert(key.clone(), cached.clone());
                if cached.revision == revision {
                    return Some(cached.data_url);
                }
            }
        }
        // An outage or failed decode leaves the last successful image visible.
        if let Ok(data_url) = self.loader.load(url, signal).await {
            let superseded = signal.is_cancelled()
                || self.inner.lock().generation != start_generation
                || self.environment_revision(&target.environment_id) != start_revision;
            if superseded {
                return self.peek(target);
            }
            if is_image_data_url(&data_url) {
                let entry = ProjectFaviconEntry {
                    environment_id: target.environment_id.clone(),
                    cwd: target.cwd.clone(),
                    favicon_path: target.favicon_path.clone().filter(|path| !path.is_empty()),
                    revision,
                    data_url: data_url.clone(),
                };
                self.inner.lock().entries.insert(key.clone(), entry.clone());
                let storage = self.storage.clone();
                self.persist(async move { storage.put(&key, &entry).await });
                self.trim();
                return Some(data_url);
            }
        }
        self.peek(target).or_else(|| Some(url.to_owned()))
    }

    /// Waits for the writes started so far.
    pub async fn flush(&self) {
        let handles = std::mem::take(&mut *self.pending.lock());
        for handle in handles {
            let _ = handle.await;
        }
    }

    /// A download that started before the clear sees the revision change and is discarded; one
    /// that starts during the clear waits for it.
    async fn clear(&self, environment_id: Option<&EnvironmentId>) {
        {
            let mut inner = self.inner.lock();
            match environment_id {
                None => inner.generation += 1,
                Some(environment_id) => {
                    *inner
                        .environment_revisions
                        .entry(environment_id.as_str().to_owned())
                        .or_insert(0) += 1;
                }
            }
        }
        let _clearing = self.clearing.write().await;
        self.hydrate().await;
        let keys: Vec<String> = {
            let inner = self.inner.lock();
            inner
                .entries
                .iter()
                .filter(|(_, entry)| {
                    environment_id
                        .is_none_or(|environment_id| entry.environment_id == *environment_id)
                })
                .map(|(key, _)| key.clone())
                .collect()
        };
        for key in keys {
            self.remove(&key);
        }
        self.flush().await;
    }

    pub async fn clear_environment(&self, environment_id: &EnvironmentId) {
        self.clear(Some(environment_id)).await;
    }

    pub async fn clear_all(&self) {
        self.clear(None).await;
    }
}

// ── The image loader ────────────────────────────────────────────────────────

/// What a fetch answers with: the status and headers the loader reads, and the body as it
/// arrives.
pub struct FetchResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub content_length: Option<u64>,
    pub body: BoxStream<'static, Result<Vec<u8>, String>>,
}

#[async_trait]
pub trait ProjectFaviconFetch: Send + Sync {
    async fn fetch(&self, url: &str, signal: &CancellationToken) -> Result<FetchResponse, String>;
}

pub struct DownscaleInput {
    pub url: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

#[async_trait]
pub trait ProjectFaviconDownscale: Send + Sync {
    async fn downscale(
        &self,
        image: DownscaleInput,
        signal: &CancellationToken,
    ) -> Result<String, String>;
}

const TOO_LARGE: &str = "Project icon is too large to decode.";

async fn read_bounded(response: FetchResponse, max_bytes: usize) -> Result<Vec<u8>, String> {
    if response
        .content_length
        .is_some_and(|declared| declared > max_bytes as u64)
    {
        return Err(TOO_LARGE.to_owned());
    }
    let mut body = response.body;
    let mut bytes = Vec::new();
    // Dropping the stream stops the download.
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        if bytes.len() + chunk.len() > max_bytes {
            return Err(TOO_LARGE.to_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Fetches an icon and inlines its bytes when they fit the cache limit, so SVGs and small
/// bitmaps are stored exactly as served. Larger bitmaps go through the platform downscaler;
/// larger SVGs stay remote because rasterizing them without intrinsic dimensions is unreliable.
pub struct ProjectFaviconImageLoader {
    fetch: Arc<dyn ProjectFaviconFetch>,
    downscale: Arc<dyn ProjectFaviconDownscale>,
}

impl ProjectFaviconImageLoader {
    pub fn new(
        fetch: Arc<dyn ProjectFaviconFetch>,
        downscale: Arc<dyn ProjectFaviconDownscale>,
    ) -> Self {
        Self { fetch, downscale }
    }
}

#[async_trait]
impl ProjectFaviconLoader for ProjectFaviconImageLoader {
    async fn load(&self, url: &str, signal: &CancellationToken) -> Result<String, String> {
        let response = self.fetch.fetch(url, signal).await?;
        if !(200..300).contains(&response.status) {
            return Err(format!(
                "Project icon request failed with {}.",
                response.status
            ));
        }
        let content_type = response
            .content_type
            .as_deref()
            .and_then(|value| value.split(';').next())
            .map(|value| value.trim().to_lowercase());
        let mime_type = match content_type.filter(|value| value.starts_with("image/")) {
            Some(content_type) => content_type,
            None => {
                media_mime_type(url).ok_or_else(|| "Project icon has no image type.".to_owned())?
            }
        };
        let bytes = read_bounded(response, PROJECT_FAVICON_MAX_SOURCE_BYTES).await?;
        if signal.is_cancelled() {
            return Err("Project icon request was aborted.".to_owned());
        }
        let data_url = format!(
            "data:{mime_type};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        );
        if is_image_data_url(&data_url) {
            return Ok(data_url);
        }
        if mime_type == "image/svg+xml" {
            return Err("Project icon exceeds the cache limit.".to_owned());
        }
        self.downscale
            .downscale(
                DownscaleInput {
                    url: url.to_owned(),
                    mime_type,
                    bytes,
                },
                signal,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::future::BoxFuture;
    use pretty_assertions::assert_eq;
    use tokio::sync::oneshot;

    use super::*;

    const URL: &str = "https://remote.test/api/assets/token-a/vabc-icon.svg";
    const IMAGE: &str = "data:image/png;base64,aWNvbg==";
    const REPLACEMENT: &str = "data:image/png;base64,bmV3";

    fn environment(id: &str) -> EnvironmentId {
        EnvironmentId::new(id).expect("an environment id")
    }

    fn target() -> ProjectFaviconTarget {
        ProjectFaviconTarget {
            environment_id: environment("remote"),
            cwd: "/workspace".to_owned(),
            favicon_path: None,
        }
    }

    fn signal() -> CancellationToken {
        CancellationToken::new()
    }

    #[derive(Default)]
    struct MemoryStorage {
        records: Mutex<IndexMap<String, ProjectFaviconEntry>>,
    }

    #[async_trait]
    impl ProjectFaviconStorage for MemoryStorage {
        async fn list(&self) -> Result<Vec<serde_json::Value>, String> {
            Ok(self
                .records
                .lock()
                .values()
                .map(|entry| serde_json::to_value(entry).expect("entry json"))
                .collect())
        }
        async fn put(&self, key: &str, entry: &ProjectFaviconEntry) -> Result<(), String> {
            self.records.lock().insert(key.to_owned(), entry.clone());
            Ok(())
        }
        async fn remove(&self, key: &str, _: &ProjectFaviconEntry) -> Result<(), String> {
            self.records.lock().shift_remove(key);
            Ok(())
        }
    }

    /// `vi.fn(async () => image)` with one-shot overrides (`mockImplementationOnce`).
    struct TestLoader {
        calls: AtomicUsize,
        default: Mutex<Result<String, String>>,
        once: Mutex<VecDeque<BoxFuture<'static, Result<String, String>>>>,
    }

    impl TestLoader {
        fn new(default: &str) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                default: Mutex::new(Ok(default.to_owned())),
                once: Mutex::new(VecDeque::new()),
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn set_default(&self, value: Result<String, String>) {
            *self.default.lock() = value;
        }

        fn next(&self, future: impl Future<Output = Result<String, String>> + Send + 'static) {
            self.once.lock().push_back(Box::pin(future));
        }
    }

    #[async_trait]
    impl ProjectFaviconLoader for TestLoader {
        async fn load(&self, _: &str, _: &CancellationToken) -> Result<String, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let once = self.once.lock().pop_front();
            match once {
                Some(future) => future.await,
                None => self.default.lock().clone(),
            }
        }
    }

    struct Fixture {
        storage: Arc<MemoryStorage>,
        load: Arc<TestLoader>,
        cache: Arc<ProjectFaviconCache>,
    }

    fn fixture() -> Fixture {
        let storage = Arc::new(MemoryStorage::default());
        let load = TestLoader::new(IMAGE);
        Fixture {
            cache: Arc::new(ProjectFaviconCache::new(storage.clone(), load.clone())),
            storage,
            load,
        }
    }

    fn fresh(fixture: &Fixture) -> ProjectFaviconCache {
        ProjectFaviconCache::new(fixture.storage.clone(), fixture.load.clone())
    }

    #[tokio::test]
    async fn restores_image_bytes_in_a_fresh_client_before_any_remote_response() {
        let fixture = fixture();
        assert_eq!(
            fixture
                .cache
                .resolve(&target(), Some(URL), &signal())
                .await
                .as_deref(),
            Some(IMAGE)
        );
        fixture.cache.flush().await;
        let reloaded = fresh(&fixture);
        reloaded.hydrate().await;
        assert_eq!(reloaded.peek(&target()).as_deref(), Some(IMAGE));
        assert_eq!(
            reloaded
                .resolve(&target(), None, &signal())
                .await
                .as_deref(),
            Some(IMAGE)
        );
        assert_eq!(fixture.load.calls(), 1);
    }

    #[tokio::test]
    async fn reuses_the_image_when_signed_urls_or_connection_origins_change() {
        let fixture = fixture();
        fixture.cache.resolve(&target(), Some(URL), &signal()).await;
        assert_eq!(
            fixture
                .cache
                .resolve(
                    &target(),
                    Some("https://new.test/api/assets/token-b/vabc-icon.svg"),
                    &signal()
                )
                .await
                .as_deref(),
            Some(IMAGE)
        );
        assert_eq!(fixture.load.calls(), 1);
    }

    #[tokio::test]
    async fn keeps_the_old_image_during_refresh_and_failures_then_persists_its_replacement() {
        let fixture = fixture();
        fixture.cache.resolve(&target(), Some(URL), &signal()).await;
        let (send, receive) = oneshot::channel::<String>();
        fixture
            .load
            .next(async move { receive.await.map_err(|error| error.to_string()) });
        let refreshing = {
            let cache = fixture.cache.clone();
            tokio::spawn(async move {
                cache
                    .resolve(
                        &target(),
                        Some(&URL.replace("vabc", "vdef")),
                        &CancellationToken::new(),
                    )
                    .await
            })
        };
        tokio::task::yield_now().await;
        assert_eq!(fixture.cache.peek(&target()).as_deref(), Some(IMAGE));
        send.send(REPLACEMENT.to_owned()).unwrap();
        assert_eq!(refreshing.await.unwrap().as_deref(), Some(REPLACEMENT));
        fixture.load.next(async { Err("offline".to_owned()) });
        assert_eq!(
            fixture
                .cache
                .resolve(&target(), Some(URL), &signal())
                .await
                .as_deref(),
            Some(REPLACEMENT)
        );
        fixture.cache.flush().await;
        assert_eq!(
            fresh(&fixture)
                .resolve(&target(), None, &signal())
                .await
                .as_deref(),
            Some(REPLACEMENT)
        );
    }

    #[tokio::test]
    async fn persists_confirmed_removal_and_ignores_an_aborted_older_download() {
        let fixture = fixture();
        fixture.cache.resolve(&target(), Some(URL), &signal()).await;
        let (send, receive) = oneshot::channel::<String>();
        let (started_send, started) = oneshot::channel::<()>();
        fixture.load.next(async move {
            let _ = started_send.send(());
            receive.await.map_err(|error| error.to_string())
        });
        let controller = CancellationToken::new();
        let pending = {
            let cache = fixture.cache.clone();
            let controller = controller.clone();
            tokio::spawn(async move {
                cache
                    .resolve(&target(), Some(&URL.replace("vabc", "vdef")), &controller)
                    .await
            })
        };
        started.await.unwrap();
        controller.cancel();
        assert_eq!(
            fixture
                .cache
                .resolve(
                    &target(),
                    Some("https://remote.test/api/assets/token/project-favicon-missing"),
                    &signal()
                )
                .await,
            None
        );
        send.send(REPLACEMENT.to_owned()).unwrap();
        pending.await.unwrap();
        fixture.cache.flush().await;
        assert_eq!(
            fresh(&fixture).resolve(&target(), None, &signal()).await,
            None
        );
    }

    #[tokio::test]
    async fn isolates_environments_workspaces_and_icon_selections() {
        let fixture = fixture();
        fixture.cache.resolve(&target(), Some(URL), &signal()).await;
        let base = target();
        assert_eq!(
            fixture
                .cache
                .peek(&ProjectFaviconTarget {
                    favicon_path: None,
                    ..base.clone()
                })
                .as_deref(),
            Some(IMAGE)
        );
        assert_eq!(
            fixture.cache.peek(&ProjectFaviconTarget {
                favicon_path: Some("brand.svg".to_owned()),
                ..base.clone()
            }),
            None
        );
        assert_eq!(
            fixture.cache.peek(&ProjectFaviconTarget {
                cwd: "/other".to_owned(),
                ..base.clone()
            }),
            None
        );
        assert_eq!(
            fixture.cache.peek(&ProjectFaviconTarget {
                environment_id: environment("other"),
                ..base
            }),
            None
        );
    }

    async fn does_not_restore_images_removed_during_a_download(clear_all: bool, remaining: usize) {
        let fixture = fixture();
        let other = ProjectFaviconTarget {
            environment_id: environment("other"),
            ..target()
        };
        fixture.cache.resolve(&other, Some(URL), &signal()).await;
        let (send, receive) = oneshot::channel::<String>();
        let (started_send, started) = oneshot::channel::<()>();
        fixture.load.next(async move {
            let _ = started_send.send(());
            receive.await.map_err(|error| error.to_string())
        });
        let pending = {
            let cache = fixture.cache.clone();
            tokio::spawn(async move {
                cache
                    .resolve(&target(), Some(URL), &CancellationToken::new())
                    .await
            })
        };
        started.await.unwrap();
        if clear_all {
            fixture.cache.clear_all().await;
        } else {
            fixture
                .cache
                .clear_environment(&target().environment_id)
                .await;
        }
        send.send(IMAGE.to_owned()).unwrap();
        pending.await.unwrap();
        fixture.cache.flush().await;
        assert_eq!(fixture.cache.peek(&target()), None);
        assert_eq!(fixture.storage.records.lock().len(), remaining);
    }

    #[tokio::test]
    async fn does_not_restore_images_for_one_environment_removed_during_a_download() {
        does_not_restore_images_removed_during_a_download(false, 1).await;
    }

    #[tokio::test]
    async fn does_not_restore_images_for_every_environment_removed_during_a_download() {
        does_not_restore_images_removed_during_a_download(true, 0).await;
    }

    /// Storage whose removal waits to be released.
    struct GatedRemovalStorage {
        inner: MemoryStorage,
        gate: tokio::sync::Notify,
        released: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl ProjectFaviconStorage for GatedRemovalStorage {
        async fn list(&self) -> Result<Vec<serde_json::Value>, String> {
            self.inner.list().await
        }
        async fn put(&self, key: &str, entry: &ProjectFaviconEntry) -> Result<(), String> {
            self.inner.put(key, entry).await
        }
        async fn remove(&self, key: &str, entry: &ProjectFaviconEntry) -> Result<(), String> {
            while !self.released.load(Ordering::SeqCst) {
                self.gate.notified().await;
            }
            self.inner.remove(key, entry).await
        }
    }

    #[tokio::test]
    async fn discards_a_download_that_starts_while_the_environment_is_being_cleared() {
        let storage = Arc::new(GatedRemovalStorage {
            inner: MemoryStorage::default(),
            gate: tokio::sync::Notify::new(),
            released: std::sync::atomic::AtomicBool::new(false),
        });
        let load = TestLoader::new(IMAGE);
        let cache = Arc::new(ProjectFaviconCache::new(storage.clone(), load.clone()));
        cache.resolve(&target(), Some(URL), &signal()).await;
        cache.flush().await;
        let clearing = {
            let cache = cache.clone();
            tokio::spawn(async move { cache.clear_environment(&target().environment_id).await })
        };
        tokio::task::yield_now().await;
        // The late download's own read waits until the test lets it go, so that what the clear
        // left behind can be looked at before it writes (a JS continuation is ordered by the
        // microtask queue; tokio does not promise which task wakes first).
        let (release, released) = oneshot::channel::<()>();
        load.next(async move {
            let _ = released.await;
            Ok(IMAGE.to_owned())
        });
        let late = {
            let cache = cache.clone();
            tokio::spawn(async move {
                cache
                    .resolve(
                        &target(),
                        Some(&URL.replace("vabc", "vdef")),
                        &CancellationToken::new(),
                    )
                    .await
            })
        };
        tokio::task::yield_now().await;
        // It is parked behind the clear: it has not started its download.
        assert_eq!(load.calls(), 1);
        storage.released.store(true, Ordering::SeqCst);
        storage.gate.notify_waiters();
        clearing.await.unwrap();
        assert_eq!(storage.inner.records.lock().len(), 0);
        assert_eq!(cache.peek(&target()), None);
        release.send(()).unwrap();
        assert_eq!(late.await.unwrap().as_deref(), Some(IMAGE));
        assert_eq!(load.calls(), 2);
    }

    #[tokio::test]
    async fn bounds_individual_images_total_bytes_and_entry_count_in_storage() {
        let fixture = fixture();
        fixture.load.set_default(Ok(format!(
            "data:image/png;base64,{}",
            "a".repeat(PROJECT_FAVICON_MAX_DATA_URL_LENGTH)
        )));
        assert_eq!(
            fixture
                .cache
                .resolve(&target(), Some(URL), &signal())
                .await
                .as_deref(),
            Some(URL)
        );
        assert_eq!(fixture.cache.peek(&target()), None);
        let large = format!(
            "data:image/png;base64,{}",
            "a".repeat(PROJECT_FAVICON_MAX_DATA_URL_LENGTH - 32)
        );
        fixture.load.set_default(Ok(large.clone()));
        let at = |cwd: String| ProjectFaviconTarget { cwd, ..target() };
        for i in 0..40 {
            fixture
                .cache
                .resolve(&at(format!("/large-{i}")), Some(URL), &signal())
                .await;
        }
        fixture.cache.flush().await;
        let stored: usize = fixture
            .storage
            .records
            .lock()
            .values()
            .map(|entry| entry.data_url.len())
            .sum();
        assert!(stored <= PROJECT_FAVICON_CACHE_MAX_BYTES);
        assert_eq!(fixture.cache.peek(&at("/large-0".to_owned())), None);
        assert_eq!(
            fixture.cache.peek(&at("/large-39".to_owned())).as_deref(),
            Some(large.as_str())
        );
        fixture.load.set_default(Ok(IMAGE.to_owned()));
        for i in 0..=PROJECT_FAVICON_CACHE_MAX_ENTRIES {
            fixture
                .cache
                .resolve(&at(format!("/small-{i}")), Some(URL), &signal())
                .await;
        }
        fixture.cache.flush().await;
        assert_eq!(
            fixture.storage.records.lock().len(),
            PROJECT_FAVICON_CACHE_MAX_ENTRIES
        );
        assert_eq!(fixture.cache.peek(&at("/small-0".to_owned())), None);
    }

    struct CorruptStorage;

    #[async_trait]
    impl ProjectFaviconStorage for CorruptStorage {
        async fn list(&self) -> Result<Vec<serde_json::Value>, String> {
            let record = |cwd: &str, data_url: &str| {
                serde_json::json!({
                    "environmentId": "remote",
                    "cwd": cwd,
                    "faviconPath": null,
                    "revision": "r",
                    "dataUrl": data_url,
                })
            };
            Ok(vec![
                record("/workspace", IMAGE),
                record("/broken", "not-an-image"),
                serde_json::json!("garbage"),
            ])
        }
        async fn put(&self, _: &str, _: &ProjectFaviconEntry) -> Result<(), String> {
            Ok(())
        }
        async fn remove(&self, _: &str, _: &ProjectFaviconEntry) -> Result<(), String> {
            Ok(())
        }
    }

    struct UnavailableStorage;

    #[async_trait]
    impl ProjectFaviconStorage for UnavailableStorage {
        async fn list(&self) -> Result<Vec<serde_json::Value>, String> {
            Err("storage unavailable".to_owned())
        }
        async fn put(&self, _: &str, _: &ProjectFaviconEntry) -> Result<(), String> {
            Err("quota exceeded".to_owned())
        }
        async fn remove(&self, _: &str, _: &ProjectFaviconEntry) -> Result<(), String> {
            Err("quota exceeded".to_owned())
        }
    }

    #[tokio::test]
    async fn skips_corrupt_records_and_tolerates_unavailable_storage() {
        let corrupt =
            ProjectFaviconCache::new(Arc::new(CorruptStorage), TestLoader::new(REPLACEMENT));
        corrupt.hydrate().await;
        assert_eq!(corrupt.peek(&target()).as_deref(), Some(IMAGE));
        assert_eq!(
            corrupt.peek(&ProjectFaviconTarget {
                cwd: "/broken".to_owned(),
                ..target()
            }),
            None
        );

        let unavailable =
            ProjectFaviconCache::new(Arc::new(UnavailableStorage), TestLoader::new(IMAGE));
        assert_eq!(
            unavailable
                .resolve(&target(), Some(URL), &signal())
                .await
                .as_deref(),
            Some(IMAGE)
        );
        unavailable.flush().await;
        assert_eq!(unavailable.peek(&target()).as_deref(), Some(IMAGE));
    }

    // ── The image loader ────────────────────────────────────────────────────

    const SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect width="16" height="16"/></svg>"#;

    fn svg_base64() -> String {
        base64::engine::general_purpose::STANDARD.encode(SVG)
    }

    struct OneResponse(Mutex<Option<FetchResponse>>);

    #[async_trait]
    impl ProjectFaviconFetch for OneResponse {
        async fn fetch(&self, _: &str, _: &CancellationToken) -> Result<FetchResponse, String> {
            self.0.lock().take().ok_or_else(|| "no response".to_owned())
        }
    }

    #[derive(Default)]
    struct RecordingDownscale {
        calls: Mutex<Vec<(String, Vec<u8>)>>,
    }

    #[async_trait]
    impl ProjectFaviconDownscale for RecordingDownscale {
        async fn downscale(
            &self,
            image: DownscaleInput,
            _: &CancellationToken,
        ) -> Result<String, String> {
            self.calls.lock().push((image.mime_type, image.bytes));
            Ok(REPLACEMENT.to_owned())
        }
    }

    fn response(status: u16, content_type: Option<&str>, body: Vec<u8>) -> FetchResponse {
        FetchResponse {
            status,
            content_type: content_type.map(str::to_owned),
            content_length: None,
            body: futures::stream::iter(vec![Ok(body)]).boxed(),
        }
    }

    fn loader(response: FetchResponse) -> (ProjectFaviconImageLoader, Arc<RecordingDownscale>) {
        let downscale = Arc::new(RecordingDownscale::default());
        (
            ProjectFaviconImageLoader::new(
                Arc::new(OneResponse(Mutex::new(Some(response)))),
                downscale.clone(),
            ),
            downscale,
        )
    }

    #[tokio::test]
    async fn inlines_small_icons_exactly_as_served_without_rasterizing() {
        let (load, downscale) = loader(response(
            200,
            Some("image/svg+xml; charset=utf-8"),
            SVG.as_bytes().to_vec(),
        ));
        assert_eq!(
            load.load(URL, &signal()).await.unwrap(),
            format!("data:image/svg+xml;base64,{}", svg_base64())
        );
        assert!(downscale.calls.lock().is_empty());
    }

    #[tokio::test]
    async fn falls_back_to_the_file_extension_when_the_response_has_no_image_type() {
        let (load, _) = loader(response(200, Some("text/plain"), SVG.as_bytes().to_vec()));
        assert_eq!(
            load.load(URL, &signal()).await.unwrap(),
            format!("data:image/svg+xml;base64,{}", svg_base64())
        );
    }

    #[tokio::test]
    async fn downscales_large_bitmaps_and_refuses_large_vector_icons() {
        let bytes = vec![0u8; PROJECT_FAVICON_MAX_DATA_URL_LENGTH];
        let (bitmap, downscale) = loader(response(200, Some("image/png"), bytes.clone()));
        assert_eq!(
            bitmap
                .load("https://remote.test/api/assets/t/v1-icon.png", &signal())
                .await
                .unwrap(),
            REPLACEMENT
        );
        assert_eq!(
            downscale.calls.lock().as_slice(),
            &[("image/png".to_owned(), bytes.clone())]
        );
        let (vector, downscale) = loader(response(200, Some("image/svg+xml"), bytes));
        let error = vector.load(URL, &signal()).await.unwrap_err();
        assert!(error.contains("exceeds the cache limit"), "{error}");
        assert!(downscale.calls.lock().is_empty());
    }

    #[tokio::test]
    async fn stops_reading_a_response_that_exceeds_the_source_limit() {
        let pulled = Arc::new(AtomicUsize::new(0));
        let chunk_len = 1024 * 1024;
        let counter = pulled.clone();
        let body = futures::stream::repeat_with(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(vec![0u8; chunk_len])
        })
        .boxed();
        let (load, downscale) = loader(FetchResponse {
            status: 200,
            content_type: Some("image/png".to_owned()),
            content_length: None,
            body,
        });
        let error = load.load(URL, &signal()).await.unwrap_err();
        assert!(error.contains("too large"), "{error}");
        assert!(pulled.load(Ordering::SeqCst) < PROJECT_FAVICON_MAX_SOURCE_BYTES / chunk_len + 3);
        assert!(downscale.calls.lock().is_empty());
        let (declared, _) = loader(FetchResponse {
            status: 200,
            content_type: Some("image/png".to_owned()),
            content_length: Some(1 << 40),
            body: futures::stream::iter(vec![Ok(b"x".to_vec())]).boxed(),
        });
        let error = declared.load(URL, &signal()).await.unwrap_err();
        assert!(error.contains("too large"), "{error}");
    }

    #[tokio::test]
    async fn rejects_failed_responses_and_non_image_payloads() {
        let (failed, _) = loader(response(404, None, b"nope".to_vec()));
        let error = failed.load(URL, &signal()).await.unwrap_err();
        assert!(error.contains("404"), "{error}");
        let (html, _) = loader(response(200, Some("text/html"), b"<html/>".to_vec()));
        let error = html
            .load("https://remote.test/api/assets/t/v1-favicon", &signal())
            .await
            .unwrap_err();
        assert!(error.contains("no image type"), "{error}");
    }
}
