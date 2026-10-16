//! Port of upstream/packages/client-runtime/src/state/assets.ts (`createProjectFaviconUrlAtomFamily`)
//! and the image half of upstream/apps/web/src/components/ProjectFavicon.tsx.
//!
//! A project row asks [`ProjectFaviconImages::image`] for its icon image. The first ask reads
//! what the cache already holds (so a restart shows last session's icon at once) and starts one
//! resolve on the tokio runtime: the resolver finds the icon file (`ProjectFaviconResolver`), the
//! cache loads and persists it, and the result is decoded off the UI thread into a GPUI image;
//! the windows redraw when it lands. Until then, and for a project with no icon file, the row
//! shows its monogram, as upstream's fallback does.
//!
//! Upstream's asset URL is a signed HTTP URL the server mints; here it is a
//! `turin-asset://project-favicon/<encoded path>/v<mtime>-<size>-<file name>` URL naming the file
//! on this machine. Its last segment is the cache's revision, so a replaced icon refreshes. Not
//! ported: `.icns` icons (the server serves their embedded PNG) and the refresh on a project
//! clone's phase change (project clones are cut).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use base64::Engine as _;
use gpui_kit::{App, AppContext as _, Context, Entity, Global, Image, ImageFormat, RenderImage};
use tokio_util::sync::CancellationToken;
use turin_projects::project_favicon_resolver::ProjectFaviconResolver;
use turin_shared::project_favicon::{
    PROJECT_FAVICON_FALLBACK_MARKER, get_project_favicon_resource_key,
    is_project_favicon_fallback_url,
};

use crate::project_favicon_cache::{ProjectFaviconCache, ProjectFaviconTarget};
use crate::state::models::EnvironmentProject;

pub const PROJECT_FAVICON_ASSET_SCHEME: &str = "turin-asset";
const ASSET_HOST: &str = "project-favicon";

/// The URL the cache is asked to resolve for an icon file: the file's path, then
/// `v<mtime>-<size>-<name>`, which is what the cache keys its revision on.
pub fn project_favicon_asset_url(path: &str, modified_ms: u128, size: u64) -> Option<String> {
    let name = Path::new(path).file_name()?.to_str()?;
    let encoded = percent_encoding::utf8_percent_encode(path, percent_encoding::NON_ALPHANUMERIC);
    Some(format!(
        "{PROJECT_FAVICON_ASSET_SCHEME}://{ASSET_HOST}/{encoded}/v{modified_ms:x}-{size:x}-{name}"
    ))
}

/// What the resolver answers when a project has no icon file: the marker the cache treats as
/// "no image" (`project-favicon-missing`).
pub fn project_favicon_fallback_url() -> String {
    format!("{PROJECT_FAVICON_ASSET_SCHEME}://{ASSET_HOST}/{PROJECT_FAVICON_FALLBACK_MARKER}")
}

/// The file an asset URL names (the inverse of [`project_favicon_asset_url`]).
pub fn project_favicon_asset_path(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    if parsed.scheme() != PROJECT_FAVICON_ASSET_SCHEME || parsed.host_str() != Some(ASSET_HOST) {
        return None;
    }
    let encoded = parsed.path_segments()?.next()?;
    percent_encoding::percent_decode_str(encoded)
        .decode_utf8()
        .ok()
        .map(|path| path.into_owned())
        .filter(|path| !path.is_empty() && path != PROJECT_FAVICON_FALLBACK_MARKER)
}

fn modified_millis(metadata: &std::fs::Metadata) -> u128 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_millis())
}

/// `createUrl` for a `project-favicon` resource: the asset URL of the icon file, or the fallback
/// URL when the project has none (or it cannot be read).
pub async fn resolve_project_favicon_url(
    resolver: &ProjectFaviconResolver,
    cwd: &str,
    favicon_path: Option<&str>,
) -> String {
    let Ok(Some(path)) = resolver.resolve_path(cwd, favicon_path).await else {
        return project_favicon_fallback_url();
    };
    let Ok(metadata) = tokio::fs::metadata(&path).await else {
        return project_favicon_fallback_url();
    };
    project_favicon_asset_url(&path, modified_millis(&metadata), metadata.len())
        .unwrap_or_else(project_favicon_fallback_url)
}

fn image_format(mime_type: &str) -> Option<ImageFormat> {
    Some(match mime_type {
        "image/png" => ImageFormat::Png,
        "image/jpeg" => ImageFormat::Jpeg,
        "image/webp" => ImageFormat::Webp,
        "image/gif" => ImageFormat::Gif,
        "image/svg+xml" => ImageFormat::Svg,
        _ => return None,
    })
}

/// A cached data URL as image bytes; `None` for a format GPUI cannot draw.
pub fn decode_data_url(data_url: &str) -> Option<(ImageFormat, Vec<u8>)> {
    let rest = data_url.strip_prefix("data:")?;
    let (mime_type, payload) = rest.split_once(";base64,")?;
    let format = image_format(mime_type)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()?;
    Some((format, bytes))
}

/// The drawable image of a cached data URL. Decoded here, once, rather than by the image
/// element's asset loader: that loads over a few frames and tells the *view* it was drawn in,
/// which a virtualized row list does not redraw for.
fn image_of(data_url: &str, cx: &App) -> Option<Arc<RenderImage>> {
    let (format, bytes) = decode_data_url(data_url)?;
    Image::from_bytes(format, bytes)
        .to_image_data(cx.svg_renderer())
        .ok()
}

fn target_of(project: &EnvironmentProject) -> ProjectFaviconTarget {
    ProjectFaviconTarget {
        environment_id: project.environment_id.clone(),
        cwd: project.workspace_root.clone(),
        favicon_path: project.favicon_path.clone(),
    }
}

struct ImagesGlobal(Entity<ProjectFaviconImages>);

impl Global for ImagesGlobal {}

pub struct ProjectFaviconImages {
    cache: Arc<ProjectFaviconCache>,
    resolver: Arc<ProjectFaviconResolver>,
    runtime: tokio::runtime::Handle,
    /// The drawn image by resource key; `None` is settled to "no image".
    images: HashMap<String, Option<Arc<RenderImage>>>,
    requested: HashSet<String>,
}

impl ProjectFaviconImages {
    /// Makes the service the app's: rows ask it through [`Self::image`].
    pub fn install(
        cache: Arc<ProjectFaviconCache>,
        resolver: Arc<ProjectFaviconResolver>,
        runtime: tokio::runtime::Handle,
        cx: &mut App,
    ) {
        let images = cx.new(|_| Self {
            cache,
            resolver,
            runtime,
            images: HashMap::new(),
            requested: HashSet::new(),
        });
        cx.set_global(ImagesGlobal(images));
    }

    /// The project's icon image, if it has one yet. Cheap to call on every render.
    pub fn image(cx: &mut App, project: &EnvironmentProject) -> Option<Arc<RenderImage>> {
        let service = cx.try_global::<ImagesGlobal>()?.0.clone();
        let target = target_of(project);
        service.update(cx, |this, cx| this.image_for(target, cx))
    }

    fn image_for(
        &mut self,
        target: ProjectFaviconTarget,
        cx: &mut Context<Self>,
    ) -> Option<Arc<RenderImage>> {
        let key = get_project_favicon_resource_key(
            target.environment_id.as_str(),
            &target.cwd,
            target.favicon_path.as_deref(),
        );
        if let Some(image) = self.images.get(&key) {
            return image.clone();
        }
        // Last session's icon, before anything is read.
        let held = self
            .cache
            .peek(&target)
            .and_then(|data_url| image_of(&data_url, cx));
        if self.requested.insert(key.clone()) {
            self.resolve(key.clone(), target, cx);
        }
        if held.is_some() {
            self.images.insert(key, held.clone());
        }
        held
    }

    fn resolve(&self, key: String, target: ProjectFaviconTarget, cx: &mut Context<Self>) {
        let (send, receive) = futures::channel::oneshot::channel::<Option<String>>();
        let cache = self.cache.clone();
        let resolver = self.resolver.clone();
        self.runtime.spawn(async move {
            let url =
                resolve_project_favicon_url(&resolver, &target.cwd, target.favicon_path.as_deref())
                    .await;
            let signal = CancellationToken::new();
            let data_url = cache.resolve(&target, Some(&url), &signal).await;
            // A fallback URL, or the URL itself when the load failed, is not an image.
            let data_url = data_url
                .filter(|value| !is_project_favicon_fallback_url(Some(value)))
                .filter(|value| value.starts_with("data:image/"));
            let _ = send.send(data_url);
        });
        cx.spawn(async move |this, cx| {
            let data_url = receive.await.ok().flatten();
            this.update(cx, |this, cx| {
                let image = data_url.and_then(|data_url| image_of(&data_url, cx));
                this.images.insert(key, image);
                cx.notify();
                // The rows that asked are plain elements, not observers: redraw them.
                cx.refresh_windows();
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_asset_url_round_trips_the_path_and_changes_its_revision_with_the_file() {
        let path = "/Users/dev/My Code/app/public/favicon.svg";
        let first = project_favicon_asset_url(path, 0x1a2b, 0x40).unwrap();
        assert_eq!(project_favicon_asset_path(&first).as_deref(), Some(path));
        assert!(first.ends_with("/v1a2b-40-favicon.svg"), "{first}");
        let edited = project_favicon_asset_url(path, 0x1a2c, 0x40).unwrap();
        assert_ne!(
            turin_shared::project_favicon::get_project_favicon_cache_key("e", path, &first),
            turin_shared::project_favicon::get_project_favicon_cache_key("e", path, &edited)
        );
        // The image type is read from the file name at the end.
        assert_eq!(
            turin_shared::file_preview::media_mime_type(&first).as_deref(),
            Some("image/svg+xml")
        );
    }

    #[test]
    fn the_fallback_url_is_the_marker_the_cache_clears_on() {
        let url = project_favicon_fallback_url();
        assert!(is_project_favicon_fallback_url(Some(&url)));
        assert_eq!(project_favicon_asset_path(&url), None);
    }

    #[test]
    fn urls_of_other_schemes_name_no_file() {
        assert_eq!(project_favicon_asset_path("https://remote.test/a/b"), None);
        assert_eq!(project_favicon_asset_path("not a url"), None);
    }

    #[test]
    fn data_urls_decode_for_the_formats_gpui_draws() {
        let (format, bytes) = decode_data_url("data:image/png;base64,aWNvbg==").unwrap();
        assert_eq!(format, ImageFormat::Png);
        assert_eq!(bytes, b"icon");
        assert!(decode_data_url("data:image/x-icon;base64,aWNvbg==").is_none());
        assert!(decode_data_url("data:image/png;base64,***").is_none());
        assert!(decode_data_url("https://example.test/a.png").is_none());
    }
}
