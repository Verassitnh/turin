//! Port of upstream/packages/client-runtime/src/state/assets.ts
//!
//! Asset URLs are signed HTTP URLs from the server's `assets.createUrl` RPC.
//! The `assets` contract is on the cut list (PORTING.md): files are local, so
//! the contract crate has no `AssetResource` and the query atoms, their cache
//! timing and the favicon URL family (which needs `projectFaviconCache`) are
//! not ported. What is ported is the logic around the call:
//!
//! - [`blocked_asset_url_state`] and [`asset_url_state_from_result`]: the
//!   state a view renders for a host file.
//! - [`create_asset_url`]: `createUrl`'s `execute`, including the fallback that
//!   asks the local environment for a media file the remote one cannot find.
//! - [`resolve_asset_url`] lives with the upload queue (`composer/
//!   attachment_upload_queue.rs`).
//!
//! `AssetResource` here is the subset the logic reads: the three file
//! resources by field, the rest by tag. `parseAssetCollectionKey` and
//! `InvalidAssetCollectionKeyError` decode the full contract union and wait
//! with it.

use async_trait::async_trait;
use turin_shared::file_preview::media_mime_type_from_extension;
use turin_shared::path::is_windows_absolute_path;

use crate::composer::attachment_upload_queue::resolve_asset_url;

pub const ASSET_URL_REFRESH_INTERVAL_MS: u64 = 30 * 60_000;
pub const ASSET_URL_STALE_TIME_MS: u64 = 5 * 60_000;
pub const ASSET_URL_IDLE_TTL_MS: u64 = 60 * 60_000;

/// Shown in place of a host file the session lacks `filesystem:read` for.
pub const MISSING_FILE_ACCESS_MESSAGE: &str =
    "This connection cannot read host files. Pair again with a new link to view it.";

/// The `AssetResource` variants the logic reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetResource {
    WorkspaceFile {
        thread_id: String,
        path: String,
    },
    MediaFile {
        thread_id: String,
        path: String,
    },
    DraftWorkspaceFile {
        cwd: String,
        path: String,
    },
    /// Any other resource, by its `_tag` (`attachment`, `project-favicon`, ...).
    Other {
        tag: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssetImageDimensions {
    pub width: u32,
    pub height: u32,
}

/// `AssetCreateUrlResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetCreateUrlResult {
    pub relative_url: String,
    /// When the signed URL stops working, in epoch milliseconds.
    pub expires_at: f64,
    pub source_path: Option<String>,
    pub image_dimensions: Option<AssetImageDimensions>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AssetUrlState {
    Loading,
    Failure {
        /// Set when the session lacks `filesystem:read`; the file itself may be fine.
        missing_file_access: bool,
    },
    Success {
        url: String,
        expires_at: f64,
        /// The host path the server chose to serve, when it differs from what was asked for.
        source_path: Option<String>,
        /// Pixel size from the image header, when the server could read one.
        image_dimensions: Option<AssetImageDimensions>,
    },
}

impl AssetUrlState {
    fn failure() -> Self {
        Self::Failure {
            missing_file_access: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileAccess<'a> {
    pub can_read_files: bool,
    pub is_pending: bool,
    pub error: Option<&'a str>,
}

/// The state for a host file this session cannot read yet, or `None` when the
/// client should ask the host. The host requires `filesystem:read` for these
/// resources, so clients skip the request. A connected session without the
/// scope gets its own reason, so the UI can say to pair again instead of
/// implying the file is gone.
pub fn blocked_asset_url_state(
    resource: Option<&AssetResource>,
    file_access: FileAccess<'_>,
) -> Option<AssetUrlState> {
    let resource = resource?;
    if file_access.can_read_files
        || !matches!(
            resource,
            AssetResource::WorkspaceFile { .. }
                | AssetResource::MediaFile { .. }
                | AssetResource::DraftWorkspaceFile { .. }
        )
    {
        return None;
    }
    if file_access.is_pending {
        return Some(AssetUrlState::Loading);
    }
    // An error means the session check itself failed, such as when offline.
    Some(if file_access.error.is_none() {
        AssetUrlState::Failure {
            missing_file_access: true,
        }
    } else {
        AssetUrlState::failure()
    })
}

/// `AsyncResult<AssetCreateUrlResult, unknown>`, settled or not.
#[derive(Debug, Clone, PartialEq)]
pub enum AssetUrlResult {
    Initial,
    Success(AssetCreateUrlResult),
    Failure,
}

pub fn asset_url_state_from_result(
    result: &AssetUrlResult,
    http_base_url: Option<&str>,
) -> AssetUrlState {
    let value = match result {
        AssetUrlResult::Failure => return AssetUrlState::failure(),
        AssetUrlResult::Success(value) => value,
        AssetUrlResult::Initial => return AssetUrlState::Loading,
    };
    let Some(http_base_url) = http_base_url else {
        return AssetUrlState::Loading;
    };
    let Some(url) = resolve_asset_url(http_base_url, &value.relative_url) else {
        return AssetUrlState::failure();
    };
    AssetUrlState::Success {
        url,
        expires_at: value.expires_at,
        source_path: value.source_path.clone(),
        image_dimensions: value.image_dimensions,
    }
}

/// An `assets.createUrl` failure, by its `_tag` (`AssetWorkspaceAssetNotFoundError`,
/// `EnvironmentAuthorizationError`, ...).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{tag}: {message}")]
pub struct AssetUrlError {
    pub tag: String,
    pub message: String,
}

/// The `assets.createUrl` RPC against one environment.
#[async_trait]
pub trait AssetUrlRequests: Send + Sync {
    async fn create_url(
        &self,
        environment_id: &str,
        resource: &AssetResource,
    ) -> Result<AssetCreateUrlResult, AssetUrlError>;
}

/// The local environment's media server (`localMediaEnvironment`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalMediaEnvironment {
    pub environment_id: String,
    pub http_base_url: String,
}

/// Errors that mean the remote environment cannot see the file on its host.
const MISSING_ON_REMOTE_TAGS: [&str; 3] = [
    "AssetWorkspaceAssetNotFoundError",
    "AssetWorkspaceAssetInspectionError",
    "AssetWorkspaceContextNotFoundError",
];

/// `path.slice(path.lastIndexOf("."))` fed to `mediaMimeTypeFromExtension`.
fn has_media_extension(path: &str) -> bool {
    path.rfind('.')
        .is_some_and(|dot| media_mime_type_from_extension(&path[dot..]).is_some())
}

/// `createUrl`'s `execute`: asks `environment_id`; when it cannot find an
/// absolute media file that the local environment could serve, asks the local
/// environment and makes the URL absolute against the local server's origin.
pub async fn create_asset_url(
    requests: &dyn AssetUrlRequests,
    environment_id: &str,
    resource: &AssetResource,
    local_media_environment: Option<&LocalMediaEnvironment>,
) -> Result<AssetCreateUrlResult, AssetUrlError> {
    let error = match requests.create_url(environment_id, resource).await {
        Ok(result) => return Ok(result),
        Err(error) => error,
    };
    let Some(local) = local_media_environment else {
        return Err(error);
    };
    let AssetResource::MediaFile { path, .. } = resource else {
        return Err(error);
    };
    if local.environment_id == environment_id
        || !MISSING_ON_REMOTE_TAGS.contains(&error.tag.as_str())
        || !(path.starts_with('/') || is_windows_absolute_path(path))
        || !has_media_extension(path)
    {
        return Err(error);
    }
    let mut asset = requests.create_url(&local.environment_id, resource).await?;
    // Callers resolve against the thread's server, so preserve the local server's origin.
    asset.relative_url =
        resolve_asset_url(&local.http_base_url, &asset.relative_url).ok_or_else(|| {
            AssetUrlError {
                tag: "InvalidAssetUrl".to_owned(),
                message: format!(
                    "Cannot resolve {:?} against {:?}.",
                    asset.relative_url, local.http_base_url
                ),
            }
        })?;
    Ok(asset)
}

#[cfg(test)]
#[path = "assets_tests.rs"]
mod tests;
