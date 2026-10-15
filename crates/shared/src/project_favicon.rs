//! Port of upstream/packages/shared/src/projectFavicon.ts

use crate::file_preview::WORKSPACE_IMAGE_PREVIEW_EXTENSIONS;
use url::Url;

pub const PROJECT_FAVICON_FALLBACK_MARKER: &str = "project-favicon-missing";

pub fn project_favicon_extensions() -> Vec<&'static str> {
    WORKSPACE_IMAGE_PREVIEW_EXTENSIONS
        .iter()
        .copied()
        .chain(std::iter::once(".icns"))
        .collect()
}

pub fn is_project_favicon_path(path: &str) -> bool {
    let literal = path.to_ascii_lowercase();
    project_favicon_extensions()
        .iter()
        .any(|extension| literal.ends_with(extension))
}

fn js_json_string(values: &[Option<&str>]) -> String {
    let json = values
        .iter()
        .map(|value| match value {
            Some(text) => serde_json::Value::String((*text).to_string()),
            None => serde_json::Value::Null,
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&json).unwrap_or_else(|_| "[]".to_string())
}

pub fn get_project_favicon_resource_key(
    environment_id: &str,
    workspace_root: &str,
    favicon_path: Option<&str>,
) -> String {
    let path = favicon_path.filter(|value| !value.is_empty());
    js_json_string(&[Some(environment_id), Some(workspace_root), path])
}

fn pathname_revision(url: &str) -> Option<String> {
    let base = Url::parse("https://t3.invalid").ok()?;
    let parsed = Url::options().base_url(Some(&base)).parse(url).ok()?;
    let pathname = parsed.path();
    let revision = pathname.rsplit('/').next().unwrap_or("");
    Some(revision.to_string())
}

pub fn get_project_favicon_cache_key(
    environment_id: &str,
    workspace_root: &str,
    url: &str,
) -> String {
    let revision = pathname_revision(url).unwrap_or_else(|| url.to_string());
    js_json_string(&[Some(environment_id), Some(workspace_root), Some(&revision)])
}

pub fn is_project_favicon_fallback_url(url: Option<&str>) -> bool {
    let Some(url) = url.filter(|value| !value.is_empty()) else {
        return false;
    };
    let Some(revision) = pathname_revision(url) else {
        return false;
    };
    revision == PROJECT_FAVICON_FALLBACK_MARKER
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_the_project_and_versioned_filename_as_the_cache_identity() {
        let first = "https://environment.example/api/assets/first-signed-token/v1-20-favicon.svg";
        let refreshed =
            "https://environment.example/api/assets/refreshed-signed-token/v1-20-favicon.svg";
        assert_eq!(
            get_project_favicon_cache_key("environment-1", "/workspace", first),
            get_project_favicon_cache_key("environment-1", "/workspace", refreshed)
        );
        assert_ne!(
            get_project_favicon_cache_key("environment-1", "/workspace", first),
            get_project_favicon_cache_key(
                "environment-1",
                "/workspace",
                "https://environment.example/api/assets/refreshed-signed-token/v2-20-favicon.svg"
            )
        );
        assert_ne!(
            get_project_favicon_cache_key("environment-1", "/workspace", first),
            get_project_favicon_cache_key("environment-2", "/workspace", first)
        );
    }

    #[test]
    fn identifies_fallback_asset_urls_by_filename() {
        assert!(is_project_favicon_fallback_url(Some(
            "https://environment.example/api/assets/signed-token/project-favicon-missing"
        )));
        assert!(is_project_favicon_fallback_url(Some(
            "/api/assets/signed-token/project-favicon-missing"
        )));
        assert!(!is_project_favicon_fallback_url(Some(
            "https://environment.example/api/assets/token/favicon.svg"
        )));
        assert!(!is_project_favicon_fallback_url(Some(
            "https://environment.example/api/assets/token/favicon.svg?name=project-favicon-missing"
        )));
        assert!(!is_project_favicon_fallback_url(None));
    }
}
