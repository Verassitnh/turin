//! Port of upstream/apps/web/src/assets/projectFaviconCache.ts.
//!
//! The pieces the web client builds the favicon cache from: a persistent store (IndexedDB
//! upstream; one JSON file per entry under `<app data dir>/project-favicons` here), the fetch
//! (the browser's `fetch`; reqwest for http(s) and a file read for `file://` here), and
//! `downscaleProjectFavicon` (canvas there; the `image` crate on a blocking thread here).
//!
//! Every operation is async and keeps its blocking work off the caller: file IO runs on tokio's
//! file pool, decoding and resizing on a blocking thread.

#![allow(dead_code)]

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use futures::StreamExt as _;
use image::ImageEncoder as _;
use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPEncoder;
use image::imageops::FilterType;
use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;

use crate::project_favicon_cache::{
    DownscaleInput, FetchResponse, PROJECT_FAVICON_MAX_DATA_URL_LENGTH,
    PROJECT_FAVICON_MAX_SOURCE_BYTES, PROJECT_FAVICON_THUMBNAIL_SIZE, ProjectFaviconCache,
    ProjectFaviconDownscale, ProjectFaviconEntry, ProjectFaviconFetch, ProjectFaviconImageLoader,
    ProjectFaviconStorage,
};
use crate::project_favicon_images::project_favicon_asset_path;

/// The folder under the app data dir (`DATABASE_NAME` / `STORE_NAME` upstream).
pub const PROJECT_FAVICON_DIRECTORY: &str = "project-favicons";

// ── Storage ─────────────────────────────────────────────────────────────────

/// One `<sha256 of the key>.json` file per entry. A record that cannot be read or parsed is left
/// out of [`ProjectFaviconStorage::list`]; the cache validates what it gets.
pub struct DiskProjectFaviconStorage {
    directory: PathBuf,
}

impl DiskProjectFaviconStorage {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    pub fn under(data_dir: &Path) -> Self {
        Self::new(data_dir.join(PROJECT_FAVICON_DIRECTORY))
    }

    fn path_for(&self, key: &str) -> PathBuf {
        let digest = Sha256::digest(key.as_bytes());
        self.directory.join(format!("{digest:x}.json"))
    }
}

#[async_trait]
impl ProjectFaviconStorage for DiskProjectFaviconStorage {
    async fn list(&self) -> Result<Vec<serde_json::Value>, String> {
        let mut records = Vec::new();
        let mut entries = match tokio::fs::read_dir(&self.directory).await {
            Ok(entries) => entries,
            // Nothing stored yet.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(records),
            Err(error) => return Err(error.to_string()),
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let Ok(raw) = tokio::fs::read_to_string(&path).await else {
                continue;
            };
            if let Ok(record) = serde_json::from_str(&raw) {
                records.push(record);
            }
        }
        Ok(records)
    }

    async fn put(&self, key: &str, entry: &ProjectFaviconEntry) -> Result<(), String> {
        let path = self.path_for(key);
        let json = serde_json::to_string(entry).map_err(|error| error.to_string())?;
        let directory = self.directory.clone();
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&directory)?;
            turin_shared::atomic_write::write_file_string_atomically(&path, &json)
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
    }

    async fn remove(&self, key: &str, _: &ProjectFaviconEntry) -> Result<(), String> {
        match tokio::fs::remove_file(self.path_for(key)).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// Used when there is no data dir to write to: every record is dropped and the cache keeps its
/// images in memory for the session.
pub struct NoProjectFaviconStorage;

#[async_trait]
impl ProjectFaviconStorage for NoProjectFaviconStorage {
    async fn list(&self) -> Result<Vec<serde_json::Value>, String> {
        Ok(Vec::new())
    }
    async fn put(&self, _: &str, _: &ProjectFaviconEntry) -> Result<(), String> {
        Ok(())
    }
    async fn remove(&self, _: &str, _: &ProjectFaviconEntry) -> Result<(), String> {
        Ok(())
    }
}

// ── Fetch ───────────────────────────────────────────────────────────────────

/// http(s) through reqwest, `file://` off disk, and the `turin-asset://` URLs
/// (`project_favicon_images`) that name an icon file the resolver found.
pub struct HttpProjectFaviconFetch {
    client: reqwest::Client,
}

impl HttpProjectFaviconFetch {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for HttpProjectFaviconFetch {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProjectFaviconFetch for HttpProjectFaviconFetch {
    async fn fetch(&self, url: &str, signal: &CancellationToken) -> Result<FetchResponse, String> {
        let parsed = url::Url::parse(url).map_err(|error| error.to_string())?;
        if let Some(path) = project_favicon_asset_path(url) {
            // The icon file the resolver found; the type comes from its name.
            let metadata = tokio::fs::metadata(&path)
                .await
                .map_err(|error| error.to_string())?;
            let content_length = Some(metadata.len());
            let content_type = turin_shared::file_preview::media_mime_type(&path);
            if metadata.len() > PROJECT_FAVICON_MAX_SOURCE_BYTES as u64 {
                return Ok(FetchResponse {
                    status: 200,
                    content_type,
                    content_length,
                    body: futures::stream::empty().boxed(),
                });
            }
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|error| error.to_string())?;
            return Ok(FetchResponse {
                status: 200,
                content_type,
                content_length,
                body: futures::stream::iter(vec![Ok(bytes)]).boxed(),
            });
        }
        if parsed.scheme() == "file" {
            let path = parsed
                .to_file_path()
                .map_err(|()| "Project icon path is not a file path.".to_owned())?;
            let metadata = tokio::fs::metadata(&path)
                .await
                .map_err(|error| error.to_string())?;
            let content_length = Some(metadata.len());
            if metadata.len() > PROJECT_FAVICON_MAX_SOURCE_BYTES as u64 {
                // The loader refuses on the declared length before it reads a byte.
                return Ok(FetchResponse {
                    status: 200,
                    content_type: None,
                    content_length,
                    body: futures::stream::empty().boxed(),
                });
            }
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|error| error.to_string())?;
            return Ok(FetchResponse {
                status: 200,
                content_type: None,
                content_length,
                body: futures::stream::iter(vec![Ok(bytes)]).boxed(),
            });
        }
        let request = self.client.get(parsed).send();
        let response = tokio::select! {
            () = signal.cancelled() => return Err("Project icon request was aborted.".to_owned()),
            response = request => response.map_err(|error| error.to_string())?,
        };
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        Ok(FetchResponse {
            status: response.status().as_u16(),
            content_type,
            content_length: response.content_length(),
            body: response
                .bytes_stream()
                .map(|chunk| {
                    chunk
                        .map(|bytes| bytes.to_vec())
                        .map_err(|error| error.to_string())
                })
                .boxed(),
        })
    }
}

// ── Downscale ───────────────────────────────────────────────────────────────

/// The side a thumbnail is fitted to at each attempt: the thumbnail size, then half of it.
fn thumbnail_sizes() -> [u32; 2] {
    [
        PROJECT_FAVICON_THUMBNAIL_SIZE,
        PROJECT_FAVICON_THUMBNAIL_SIZE / 2,
    ]
}

/// `Math.min(1, size / width, size / height)` applied to each side, at least one pixel.
pub fn thumbnail_dimensions(width: u32, height: u32, size: u32) -> (u32, u32) {
    let scale = (size as f64 / width as f64)
        .min(size as f64 / height as f64)
        .min(1.);
    (
        ((width as f64 * scale).round() as u32).max(1),
        ((height as f64 * scale).round() as u32).max(1),
    )
}

fn data_url(mime_type: &str, bytes: &[u8]) -> String {
    use base64::Engine as _;
    format!(
        "data:{mime_type};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// The smaller of a lossless WebP and a PNG of the same pixels. (Upstream asks the canvas for a
/// lossy WebP at quality 0.85; the encoders here are lossless.)
fn encode_thumbnail(image: &image::DynamicImage) -> Result<String, String> {
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut webp = Vec::new();
    WebPEncoder::new_lossless(Cursor::new(&mut webp))
        .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|error| error.to_string())?;
    let mut png = Vec::new();
    PngEncoder::new(Cursor::new(&mut png))
        .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|error| error.to_string())?;
    Ok(if webp.len() <= png.len() {
        data_url("image/webp", &webp)
    } else {
        data_url("image/png", &png)
    })
}

/// `downscaleProjectFavicon`: rasterizes a bitmap that is too large to inline, retrying at half
/// size.
pub fn downscale_blocking(bytes: &[u8]) -> Result<String, String> {
    let bitmap = image::load_from_memory(bytes).map_err(|error| error.to_string())?;
    for size in thumbnail_sizes() {
        let (width, height) = thumbnail_dimensions(bitmap.width(), bitmap.height(), size);
        let scaled = bitmap.resize_exact(width, height, FilterType::Lanczos3);
        let encoded = encode_thumbnail(&scaled)?;
        if encoded.len() <= PROJECT_FAVICON_MAX_DATA_URL_LENGTH {
            return Ok(encoded);
        }
    }
    Err("Project icon thumbnail exceeds the cache limit.".to_owned())
}

pub struct ImageProjectFaviconDownscale;

#[async_trait]
impl ProjectFaviconDownscale for ImageProjectFaviconDownscale {
    async fn downscale(
        &self,
        image: DownscaleInput,
        signal: &CancellationToken,
    ) -> Result<String, String> {
        let work = tokio::task::spawn_blocking(move || downscale_blocking(&image.bytes));
        tokio::select! {
            () = signal.cancelled() => Err("Project icon request was aborted.".to_owned()),
            result = work => result.map_err(|error| error.to_string())?,
        }
    }
}

// ── The cache the app uses ──────────────────────────────────────────────────

/// `projectFaviconCache`: persisted under the app data dir, fetched over http(s) or from disk,
/// downscaled with the `image` crate. Built once, inside the tokio runtime that will run it.
pub fn project_favicon_cache(data_dir: Option<&Path>) -> Arc<ProjectFaviconCache> {
    static CACHE: OnceLock<Arc<ProjectFaviconCache>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let storage: Arc<dyn ProjectFaviconStorage> = match data_dir {
                Some(data_dir) => Arc::new(DiskProjectFaviconStorage::under(data_dir)),
                None => Arc::new(NoProjectFaviconStorage),
            };
            Arc::new(ProjectFaviconCache::new(
                storage,
                Arc::new(ProjectFaviconImageLoader::new(
                    Arc::new(HttpProjectFaviconFetch::new()),
                    Arc::new(ImageProjectFaviconDownscale),
                )),
            ))
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::EnvironmentId;

    fn entry(cwd: &str) -> ProjectFaviconEntry {
        ProjectFaviconEntry {
            environment_id: EnvironmentId::new("local").expect("an environment id"),
            cwd: cwd.to_owned(),
            favicon_path: None,
            revision: "rev".to_owned(),
            data_url: "data:image/png;base64,aWNvbg==".to_owned(),
        }
    }

    #[tokio::test]
    async fn disk_storage_round_trips_and_removes_entries() {
        let dir = tempfile::tempdir().unwrap();
        let storage = DiskProjectFaviconStorage::under(dir.path());
        assert_eq!(storage.list().await.unwrap().len(), 0);
        storage.put("a", &entry("/a")).await.unwrap();
        storage.put("b", &entry("/b")).await.unwrap();
        // The same key again replaces its record.
        storage.put("a", &entry("/a2")).await.unwrap();
        let mut cwds: Vec<String> = storage
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|record| record["cwd"].as_str().unwrap().to_owned())
            .collect();
        cwds.sort();
        assert_eq!(cwds, ["/a2", "/b"]);
        storage.remove("a", &entry("/a2")).await.unwrap();
        // Removing what is not there is fine.
        storage.remove("a", &entry("/a2")).await.unwrap();
        assert_eq!(storage.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn disk_storage_skips_files_that_are_not_records() {
        let dir = tempfile::tempdir().unwrap();
        let storage = DiskProjectFaviconStorage::under(dir.path());
        storage.put("a", &entry("/a")).await.unwrap();
        let folder = dir.path().join(PROJECT_FAVICON_DIRECTORY);
        std::fs::write(folder.join("broken.json"), "{not json").unwrap();
        std::fs::write(folder.join("notes.txt"), "{}").unwrap();
        assert_eq!(storage.list().await.unwrap().len(), 1);
    }

    #[test]
    fn thumbnails_fit_the_size_without_enlarging() {
        assert_eq!(thumbnail_dimensions(192, 96, 96), (96, 48));
        assert_eq!(thumbnail_dimensions(50, 20, 96), (50, 20));
        assert_eq!(thumbnail_dimensions(1000, 1, 96), (96, 1));
        assert_eq!(thumbnail_dimensions(1, 1000, 96), (1, 96));
    }

    #[test]
    fn a_large_bitmap_is_downscaled_into_the_cache_limit() {
        let mut source = image::RgbaImage::new(512, 512);
        for (x, y, pixel) in source.enumerate_pixels_mut() {
            *pixel = image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255]);
        }
        let mut png = Vec::new();
        PngEncoder::new(Cursor::new(&mut png))
            .write_image(&source, 512, 512, image::ExtendedColorType::Rgba8)
            .unwrap();
        let data_url = downscale_blocking(&png).unwrap();
        assert!(data_url.len() <= PROJECT_FAVICON_MAX_DATA_URL_LENGTH);
        assert!(crate::project_favicon_cache::is_image_data_url(&data_url));
    }

    #[test]
    fn bytes_that_are_not_an_image_do_not_downscale() {
        assert!(downscale_blocking(b"not an image").is_err());
    }
}
