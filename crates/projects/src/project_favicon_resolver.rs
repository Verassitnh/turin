//! Port of upstream/apps/server/src/project/ProjectFaviconResolver.ts
//!
//! Resolves a representative favicon or app icon file for a workspace by
//! checking common file locations and project source metadata.

use std::fmt;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{FuturesOrdered, StreamExt};
use regex::Regex;
use turin_git::workspace::workspace_paths::{
    NormalizeWorkspaceRootError, WorkspacePathOutsideRootError, WorkspacePaths,
};

use crate::ttl_cache::{Clock, TtlCache, system_clock};

// Resolution probes 23 well-known paths plus 7 source files, so a miss
// costs ~30 filesystem probes. AssetAccess resolves on every project-favicon
// asset URL, and a project's icon does not move, so the answer is cached.
const FAVICON_CACHE_CAPACITY: usize = 512;
const FAVICON_POSITIVE_CACHE_TTL: Duration = Duration::from_secs(10 * 60);
const FAVICON_NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(60);

fn favicon_cache_key(cwd: &str, favicon_path: Option<&str>) -> String {
    format!("{}\0{}", favicon_path.unwrap_or(""), cwd)
}

fn parse_favicon_cache_key(key: &str) -> (String, Option<String>) {
    match key.split_once('\0') {
        None => (key.to_owned(), None),
        Some(("", cwd)) => (cwd.to_owned(), None),
        Some((favicon_path, cwd)) => (cwd.to_owned(), Some(favicon_path.to_owned())),
    }
}

// Well-known favicon paths checked in order.
const FAVICON_CANDIDATES: [&str; 23] = [
    "favicon.svg",
    "favicon.ico",
    "favicon.png",
    "public/favicon.svg",
    "public/favicon.ico",
    "public/favicon.png",
    "app/favicon.ico",
    "app/favicon.png",
    "app/icon.svg",
    "app/icon.png",
    "app/icon.ico",
    "src/favicon.ico",
    "src/favicon.svg",
    "src/app/favicon.ico",
    "src/app/icon.svg",
    "src/app/icon.png",
    "assets/icon.svg",
    "assets/icon.png",
    "assets/logo.svg",
    "assets/logo.png",
    ".idea/icon.svg",
    "Resources/AppIcon.icns",
    "AppIcon.icns",
];

// Files that may contain a <link rel="icon"> or icon metadata declaration.
const ICON_SOURCE_FILES: [&str; 7] = [
    "index.html",
    "public/index.html",
    "app/routes/__root.tsx",
    "src/routes/__root.tsx",
    "app/root.tsx",
    "src/root.tsx",
    "src/index.html",
];

// Matches a whole `<link ...>` tag. Candidate tags are then checked for `rel`
// and `href` separately: the JS original used lookaheads, which the regex crate
// lacks. Anchoring on `<link` keeps the scan linear on large sources.
static LINK_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<link\b[^>]*>").expect("valid link tag regex"));
static LINK_ICON_REL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\brel=["'](?:icon|shortcut icon)["']"#).expect("valid rel regex")
});
static LINK_ICON_HREF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\bhref=["']([^"'?]+)"#).expect("valid href regex"));
// Icon metadata counts when `rel` and `href` share a brace-free run, so a run holding `rel`
// but no href falls through to the next one rather than ending the search.
static ICON_REL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\brel\s*:\s*["'](?:icon|shortcut icon)["']"#).expect("valid icon rel regex")
});
static ICON_HREF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\bhref\s*:\s*["']([^"'?]+)"#).expect("valid icon href regex")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectFaviconOperation {
    NormalizeWorkspace,
    ResolvePath,
    StatCandidate,
    ReadSource,
}

impl ProjectFaviconOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NormalizeWorkspace => "normalize-workspace",
            Self::ResolvePath => "resolve-path",
            Self::StatCandidate => "stat-candidate",
            Self::ReadSource => "read-source",
        }
    }
}

impl fmt::Display for ProjectFaviconOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Upstream `cause: Schema.Defect()`, kept as a closed set so callers can match.
#[derive(Debug, thiserror::Error)]
pub enum ProjectFaviconCause {
    #[error(transparent)]
    Workspace(#[from] NormalizeWorkspaceRootError),
    #[error(transparent)]
    OutsideRoot(#[from] WorkspacePathOutsideRootError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
#[error("Failed to resolve project favicon during {operation} for workspace {workspace_root}.")]
pub struct ProjectFaviconResolutionError {
    pub operation: ProjectFaviconOperation,
    pub workspace_root: String,
    pub relative_path: Option<String>,
    pub absolute_path: Option<String>,
    // Boxed: keeps `Result<_, ProjectFaviconResolutionError>` under clippy's result_large_err limit.
    #[source]
    pub cause: Box<ProjectFaviconCause>,
}

impl ProjectFaviconResolutionError {
    /// True when the workspace root itself is gone, e.g. a moved or deleted checkout.
    pub fn is_missing_workspace_root(&self) -> bool {
        self.operation == ProjectFaviconOperation::NormalizeWorkspace
            && matches!(
                *self.cause,
                ProjectFaviconCause::Workspace(NormalizeWorkspaceRootError::NotExists(_))
            )
    }

    fn new(
        operation: ProjectFaviconOperation,
        workspace_root: &str,
        relative_path: Option<&str>,
        absolute_path: Option<&str>,
        cause: ProjectFaviconCause,
    ) -> Self {
        Self {
            operation,
            workspace_root: workspace_root.to_owned(),
            relative_path: relative_path.map(str::to_owned),
            absolute_path: absolute_path.map(str::to_owned),
            cause: Box::new(cause),
        }
    }
}

/// Upstream `T3ProjectFileLoader` dependency: the `iconPath` from a project's
/// `t3.json`. The file feature is outside the N2 port, so the app injects a
/// source or none.
#[async_trait]
pub trait ProjectIconPathSource: Send + Sync {
    async fn icon_path(&self, workspace_root: &str) -> Option<String>;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CandidateScope {
    Workspace,
    Filesystem,
}

/// Upstream `ProjectFaviconResolver`.
pub struct ProjectFaviconResolver {
    workspace_paths: Arc<WorkspacePaths>,
    icon_source: Option<Arc<dyn ProjectIconPathSource>>,
    cache: TtlCache<String, Option<String>>,
}

impl ProjectFaviconResolver {
    pub fn new(
        workspace_paths: Arc<WorkspacePaths>,
        icon_source: Option<Arc<dyn ProjectIconPathSource>>,
    ) -> Self {
        Self::with_clock(workspace_paths, icon_source, system_clock())
    }

    pub fn with_clock(
        workspace_paths: Arc<WorkspacePaths>,
        icon_source: Option<Arc<dyn ProjectIconPathSource>>,
        clock: Clock,
    ) -> Self {
        Self {
            workspace_paths,
            icon_source,
            cache: TtlCache::new(FAVICON_CACHE_CAPACITY, clock),
        }
    }

    /// Resolve a favicon or icon file path for the workspace root. `None` when
    /// no candidate icon file exists.
    pub async fn resolve_path(
        &self,
        cwd: &str,
        favicon_path: Option<&str>,
    ) -> Result<Option<String>, ProjectFaviconResolutionError> {
        let key = favicon_cache_key(cwd, favicon_path);
        let cached = self.load_cached(&key).await?;
        let Some(cached) = cached else {
            return Ok(None);
        };

        // A hit still confirms the file with one stat rather than the ~20 probes a
        // full walk costs, so a deleted icon falls back at once instead of after
        // the TTL.
        let stats = stat_optional(&cached).await.map_err(|cause| {
            ProjectFaviconResolutionError::new(
                ProjectFaviconOperation::StatCandidate,
                cwd,
                None,
                Some(&cached),
                cause.into(),
            )
        })?;
        if stats.is_some_and(|metadata| metadata.is_file()) {
            return Ok(Some(cached));
        }

        self.cache.invalidate(&key);
        self.load_cached(&key).await
    }

    async fn load_cached(
        &self,
        key: &str,
    ) -> Result<Option<String>, ProjectFaviconResolutionError> {
        if let Some(value) = self.cache.get(&key.to_owned()) {
            return Ok(value);
        }
        let (cwd, favicon_path) = parse_favicon_cache_key(key);
        let value = self
            .resolve_path_uncached(&cwd, favicon_path.as_deref())
            .await?;
        let ttl = if value.is_some() {
            FAVICON_POSITIVE_CACHE_TTL
        } else {
            FAVICON_NEGATIVE_CACHE_TTL
        };
        self.cache.insert(key.to_owned(), value.clone(), ttl);
        Ok(value)
    }

    async fn resolve_path_uncached(
        &self,
        cwd: &str,
        favicon_path: Option<&str>,
    ) -> Result<Option<String>, ProjectFaviconResolutionError> {
        let project_cwd = self
            .workspace_paths
            .normalize_workspace_root(cwd, false)
            .await
            .map_err(|cause| {
                ProjectFaviconResolutionError::new(
                    ProjectFaviconOperation::NormalizeWorkspace,
                    cwd,
                    None,
                    None,
                    cause.into(),
                )
            })?;

        // A grouped project's saved path can be absent from one checkout. Use it
        // where it exists and retain automatic discovery for the other checkouts.
        if let Some(favicon_path) = favicon_path {
            let existing = self
                .find_existing_file(
                    &project_cwd,
                    &[favicon_path.to_owned()],
                    CandidateScope::Filesystem,
                )
                .await?;
            if existing.is_some() {
                return Ok(existing);
            }
        }

        // A t3.json iconPath takes precedence over the well-known locations.
        if let Some(source) = &self.icon_source
            && let Some(icon_path) = source.icon_path(&project_cwd).await
        {
            let existing = self
                .find_existing_file(&project_cwd, &[icon_path], CandidateScope::Workspace)
                .await?;
            if existing.is_some() {
                return Ok(existing);
            }
        }

        let well_known = self
            .first_in_order(
                FAVICON_CANDIDATES
                    .iter()
                    .map(|candidate| (*candidate).to_owned())
                    .collect(),
                |candidate| {
                    let project_cwd = project_cwd.clone();
                    async move {
                        self.find_existing_file(
                            &project_cwd,
                            &[candidate],
                            CandidateScope::Workspace,
                        )
                        .await
                    }
                },
            )
            .await?;
        if well_known.is_some() {
            return Ok(well_known);
        }

        self.first_in_order(
            ICON_SOURCE_FILES
                .iter()
                .map(|file| (*file).to_owned())
                .collect(),
            |source_file| {
                let project_cwd = project_cwd.clone();
                async move { self.find_icon_from_source(&project_cwd, &source_file).await }
            },
        )
        .await
    }

    /// Probe items concurrently and return the first hit in list order. Probes
    /// after a hit are dropped, which cancels them. A failure surfaces only when
    /// no earlier probe found a file.
    async fn first_in_order<T, F, Fut>(
        &self,
        items: Vec<T>,
        probe: F,
    ) -> Result<Option<String>, ProjectFaviconResolutionError>
    where
        F: Fn(T) -> Fut,
        Fut: Future<Output = Result<Option<String>, ProjectFaviconResolutionError>>,
    {
        let mut pending: FuturesOrdered<Fut> = items.into_iter().map(probe).collect();
        while let Some(found) = pending.next().await {
            if let Some(path) = found? {
                return Ok(Some(path));
            }
        }
        Ok(None)
    }

    async fn find_existing_file(
        &self,
        project_cwd: &str,
        relative_candidates: &[String],
        scope: CandidateScope,
    ) -> Result<Option<String>, ProjectFaviconResolutionError> {
        for relative_path in relative_candidates {
            let absolute_path =
                if scope == CandidateScope::Filesystem && Path::new(relative_path).is_absolute() {
                    relative_path.clone()
                } else {
                    match self
                        .workspace_paths
                        .resolve_relative_path_within_root(project_cwd, relative_path)
                        .await
                    {
                        Ok(resolved) => resolved.absolute_path,
                        // Upstream `catchTags(WorkspacePathOutsideRootError)`: skip the candidate.
                        Err(_) => continue,
                    }
                };
            let stats = stat_optional(&absolute_path).await.map_err(|cause| {
                ProjectFaviconResolutionError::new(
                    ProjectFaviconOperation::StatCandidate,
                    project_cwd,
                    Some(relative_path),
                    Some(&absolute_path),
                    cause.into(),
                )
            })?;
            if stats.is_some_and(|metadata| metadata.is_file()) {
                return Ok(Some(absolute_path));
            }
        }
        Ok(None)
    }

    /// Read one source file and resolve the icon it declares, if any.
    async fn find_icon_from_source(
        &self,
        project_cwd: &str,
        source_file: &str,
    ) -> Result<Option<String>, ProjectFaviconResolutionError> {
        let source_path = self
            .workspace_paths
            .resolve_relative_path_within_root(project_cwd, source_file)
            .await
            .map_err(|cause| {
                ProjectFaviconResolutionError::new(
                    ProjectFaviconOperation::ResolvePath,
                    project_cwd,
                    Some(source_file),
                    None,
                    cause.into(),
                )
            })?;
        // Node decodes UTF-8 with replacement, so invalid bytes do not fail the read.
        let source = match tokio::fs::read(&source_path.absolute_path).await {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(cause) => {
                return Err(ProjectFaviconResolutionError::new(
                    ProjectFaviconOperation::ReadSource,
                    project_cwd,
                    Some(source_file),
                    Some(&source_path.absolute_path),
                    cause.into(),
                ));
            }
        };
        let Some(href) = extract_icon_href(&source) else {
            return Ok(None);
        };
        self.find_existing_file(
            project_cwd,
            &resolve_icon_href(&href),
            CandidateScope::Workspace,
        )
        .await
    }
}

/// `stat` that treats a missing path as `None` (upstream `optionOnNotFound`).
async fn stat_optional(path: &str) -> Result<Option<std::fs::Metadata>, std::io::Error> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => Ok(Some(metadata)),
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(cause) => Err(cause),
    }
}

fn resolve_icon_href(href: &str) -> Vec<String> {
    let clean = href.strip_prefix('/').unwrap_or(href);
    vec![
        Path::new("public")
            .join(clean)
            .to_string_lossy()
            .into_owned(),
        clean.to_owned(),
    ]
}

fn extract_icon_href(source: &str) -> Option<String> {
    for tag in LINK_TAG_RE.find_iter(source) {
        let tag = tag.as_str();
        if !LINK_ICON_REL_RE.is_match(tag) {
            continue;
        }
        if let Some(captures) = LINK_ICON_HREF_RE.captures(tag) {
            return Some(captures[1].to_owned());
        }
    }
    for run in source.split('}') {
        if !ICON_REL_RE.is_match(run) {
            continue;
        }
        if let Some(captures) = ICON_HREF_RE.captures(run) {
            return Some(captures[1].to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use pretty_assertions::assert_eq;
    use turin_git::workspace::workspace_paths::WorkspacePaths;

    use super::{
        ProjectFaviconCause, ProjectFaviconOperation, ProjectFaviconResolver, extract_icon_href,
    };
    use crate::ttl_cache::test_support::TestClock;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn path_string(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn write_text(cwd: &Path, relative_path: &str, contents: &str) {
        let absolute = cwd.join(relative_path);
        std::fs::create_dir_all(absolute.parent().expect("parent")).expect("mkdir");
        std::fs::write(absolute, contents).expect("write");
    }

    fn resolver(clock: &Arc<TestClock>) -> ProjectFaviconResolver {
        ProjectFaviconResolver::with_clock(
            Arc::new(WorkspacePaths::new(String::new())),
            None,
            clock.clock(),
        )
    }

    fn real_resolver() -> ProjectFaviconResolver {
        ProjectFaviconResolver::new(Arc::new(WorkspacePaths::new(String::new())), None)
    }

    #[tokio::test]
    async fn serves_repeated_resolves_from_cache_instead_of_re_walking_candidates() {
        let temp = temp_dir();
        let cwd = path_string(temp.path());
        let clock = TestClock::new();
        let resolver = resolver(&clock);
        write_text(temp.path(), "public/favicon.svg", "<svg>public</svg>");

        let resolved = resolver.resolve_path(&cwd, None).await.expect("ok");
        assert!(
            resolved
                .as_deref()
                .is_some_and(|path| path.ends_with("public/favicon.svg"))
        );

        // `favicon.svg` outranks `public/favicon.svg`, so a resolver that walked
        // the candidate list again would switch to it.
        write_text(temp.path(), "favicon.svg", "<svg>root</svg>");
        for _ in 0..3 {
            assert_eq!(
                resolver.resolve_path(&cwd, None).await.expect("ok"),
                resolved
            );
        }

        clock.adjust(Duration::from_secs(11 * 60));
        assert_eq!(
            resolver.resolve_path(&cwd, None).await.expect("ok"),
            Some(path_string(&temp.path().join("favicon.svg")))
        );
        assert_ne!(
            resolver.resolve_path(&cwd, None).await.expect("ok"),
            resolved
        );
    }

    #[tokio::test]
    async fn falls_back_at_once_when_a_cached_favicon_is_deleted() {
        let temp = temp_dir();
        let cwd = path_string(temp.path());
        let clock = TestClock::new();
        let resolver = resolver(&clock);
        write_text(temp.path(), "favicon.svg", "<svg>favicon</svg>");

        assert!(
            resolver
                .resolve_path(&cwd, None)
                .await
                .expect("ok")
                .is_some()
        );
        std::fs::remove_file(temp.path().join("favicon.svg")).expect("remove");

        // Still inside the positive TTL: the cached path must not be served.
        assert_eq!(resolver.resolve_path(&cwd, None).await.expect("ok"), None);
    }

    #[tokio::test]
    async fn re_probes_for_a_favicon_added_after_a_miss_once_the_negative_ttl_expires() {
        let temp = temp_dir();
        let cwd = path_string(temp.path());
        let clock = TestClock::new();
        let resolver = resolver(&clock);

        assert_eq!(resolver.resolve_path(&cwd, None).await.expect("ok"), None);
        write_text(temp.path(), "favicon.svg", "<svg>favicon</svg>");
        assert_eq!(resolver.resolve_path(&cwd, None).await.expect("ok"), None);

        clock.adjust(Duration::from_secs(2 * 60));
        assert!(
            resolver
                .resolve_path(&cwd, None)
                .await
                .expect("ok")
                .is_some()
        );
    }

    #[tokio::test]
    async fn prefers_well_known_favicon_files() {
        let temp = temp_dir();
        write_text(temp.path(), "favicon.svg", "<svg>favicon</svg>");

        let resolved = real_resolver()
            .resolve_path(&path_string(temp.path()), None)
            .await
            .expect("ok");
        assert!(
            resolved
                .as_deref()
                .is_some_and(|path| path.contains("favicon.svg"))
        );
    }

    #[tokio::test]
    async fn uses_a_saved_project_favicon_override() {
        let temp = temp_dir();
        write_text(temp.path(), "brand/custom.svg", "<svg>custom</svg>");
        write_text(temp.path(), "favicon.svg", "<svg>automatic</svg>");

        let resolved = real_resolver()
            .resolve_path(&path_string(temp.path()), Some("brand/custom.svg"))
            .await
            .expect("ok");
        assert_eq!(
            resolved,
            Some(path_string(&temp.path().join("brand").join("custom.svg")))
        );
    }

    #[tokio::test]
    async fn uses_a_saved_project_favicon_outside_the_workspace() {
        let cwd = temp_dir();
        let pictures = temp_dir();
        write_text(pictures.path(), "custom.png", "image");
        let external = path_string(&pictures.path().join("custom.png"));

        let resolved = real_resolver()
            .resolve_path(&path_string(cwd.path()), Some(&external))
            .await
            .expect("ok");
        assert_eq!(resolved, Some(external));
    }

    #[tokio::test]
    async fn falls_back_when_a_saved_override_is_missing_from_a_checkout() {
        let temp = temp_dir();
        write_text(temp.path(), "favicon.svg", "<svg>automatic</svg>");

        let resolved = real_resolver()
            .resolve_path(&path_string(temp.path()), Some("brand/missing.svg"))
            .await
            .expect("ok");
        assert!(
            resolved
                .as_deref()
                .is_some_and(|path| path.contains("favicon.svg"))
        );
    }

    #[tokio::test]
    async fn resolves_icon_hrefs_from_project_source_files() {
        let temp = temp_dir();
        write_text(
            temp.path(),
            "index.html",
            r#"<link rel="icon" href="/brand/logo.svg">"#,
        );
        write_text(temp.path(), "public/brand/logo.svg", "<svg>brand</svg>");

        let resolved = real_resolver()
            .resolve_path(&path_string(temp.path()), None)
            .await
            .expect("ok");
        assert_eq!(
            resolved,
            Some(path_string(
                &temp.path().join("public").join("brand").join("logo.svg")
            ))
        );
    }

    #[tokio::test]
    async fn resolves_icon_hrefs_from_object_literal_route_metadata() {
        let temp = temp_dir();
        write_text(
            temp.path(),
            "src/routes/__root.tsx",
            r#"export const Route = createRootRoute({
  head: () => ({
    links: [
      { rel: "stylesheet", href: "/app.css" },
      { rel: "icon", href: "/brand/logo.svg" },
    ],
  }),
});"#,
        );
        write_text(temp.path(), "public/brand/logo.svg", "<svg>brand</svg>");

        let resolved = real_resolver()
            .resolve_path(&path_string(temp.path()), None)
            .await
            .expect("ok");
        assert_eq!(
            resolved,
            Some(path_string(
                &temp.path().join("public").join("brand").join("logo.svg")
            ))
        );
    }

    #[tokio::test]
    async fn resolves_object_literal_icon_metadata_when_href_precedes_rel() {
        let temp = temp_dir();
        write_text(
            temp.path(),
            "src/root.tsx",
            r#"const links = [{ href: "/brand/logo.svg", rel: "shortcut icon" }];"#,
        );
        write_text(temp.path(), "public/brand/logo.svg", "<svg>brand</svg>");

        let resolved = real_resolver()
            .resolve_path(&path_string(temp.path()), None)
            .await
            .expect("ok");
        assert_eq!(
            resolved,
            Some(path_string(
                &temp.path().join("public").join("brand").join("logo.svg")
            ))
        );
    }

    #[tokio::test]
    async fn resolves_object_literal_icon_metadata_alongside_nested_objects() {
        let temp = temp_dir();
        write_text(
            temp.path(),
            "src/root.tsx",
            r#"const links = [{ attributes: {}, rel: "icon", href: "/brand/logo.svg" }];"#,
        );
        write_text(temp.path(), "public/brand/logo.svg", "<svg>brand</svg>");

        let resolved = real_resolver()
            .resolve_path(&path_string(temp.path()), None)
            .await
            .expect("ok");
        assert_eq!(
            resolved,
            Some(path_string(
                &temp.path().join("public").join("brand").join("logo.svg")
            ))
        );
    }

    #[tokio::test]
    async fn skips_icon_metadata_without_an_href_and_keeps_scanning() {
        let temp = temp_dir();
        write_text(
            temp.path(),
            "src/root.tsx",
            r#"const links = [{ rel: "icon" }, { rel: "icon", href: "/brand/logo.svg" }];"#,
        );
        write_text(temp.path(), "public/brand/logo.svg", "<svg>brand</svg>");

        let resolved = real_resolver()
            .resolve_path(&path_string(temp.path()), None)
            .await
            .expect("ok");
        assert_eq!(
            resolved,
            Some(path_string(
                &temp.path().join("public").join("brand").join("logo.svg")
            ))
        );
    }

    #[tokio::test]
    async fn scans_large_icon_sources_without_an_icon_in_reasonable_time() {
        let temp = temp_dir();
        // Mirrors a generated single-file build: large, brace-sparse, and no icon metadata.
        let filler = format!("<p>{}</p>\n", "pokopia companion guide ".repeat(24));
        write_text(
            temp.path(),
            "index.html",
            &format!(
                "<!doctype html><html><head><title>guide</title></head><body>\n{}</body></html>",
                filler.repeat(1200)
            ),
        );

        let started = std::time::Instant::now();
        let resolved = real_resolver()
            .resolve_path(&path_string(temp.path()), None)
            .await
            .expect("ok");
        assert_eq!(resolved, None);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn returns_null_when_no_icon_is_present() {
        let temp = temp_dir();
        assert_eq!(
            real_resolver()
                .resolve_path(&path_string(temp.path()), None)
                .await
                .expect("ok"),
            None
        );
    }

    #[tokio::test]
    async fn preserves_workspace_normalization_context() {
        let temp = temp_dir();
        let missing = path_string(&temp.path().join("missing"));

        let error = real_resolver()
            .resolve_path(&missing, None)
            .await
            .expect_err("missing root fails");
        assert_eq!(error.operation, ProjectFaviconOperation::NormalizeWorkspace);
        assert_eq!(error.workspace_root, missing);
        assert!(error.is_missing_workspace_root());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn preserves_non_missing_candidate_stat_failures() {
        let temp = temp_dir();
        let cwd = path_string(temp.path());
        // A self-referencing symlink makes stat fail with ELOOP, not NotFound.
        std::os::unix::fs::symlink("favicon.svg", temp.path().join("favicon.svg"))
            .expect("symlink");

        let error = real_resolver()
            .resolve_path(&cwd, None)
            .await
            .expect_err("stat failure surfaces");
        assert_eq!(error.operation, ProjectFaviconOperation::StatCandidate);
        assert_eq!(error.workspace_root, cwd);
        assert_eq!(error.relative_path.as_deref(), Some("favicon.svg"));
        assert_eq!(
            error.absolute_path,
            Some(path_string(&temp.path().join("favicon.svg")))
        );
        assert!(matches!(*error.cause, ProjectFaviconCause::Io(_)));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn ignores_a_failing_candidate_that_ranks_below_a_found_favicon() {
        let temp = temp_dir();
        write_text(temp.path(), "favicon.svg", "<svg>root</svg>");
        std::os::unix::fs::symlink("favicon.ico", temp.path().join("favicon.ico"))
            .expect("symlink");

        let resolved = real_resolver()
            .resolve_path(&path_string(temp.path()), None)
            .await
            .expect("lower-ranked failure is ignored");
        assert_eq!(
            resolved,
            Some(path_string(&temp.path().join("favicon.svg")))
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn preserves_icon_source_read_failures() {
        let temp = temp_dir();
        let cwd = path_string(temp.path());
        std::os::unix::fs::symlink("index.html", temp.path().join("index.html")).expect("symlink");

        let error = real_resolver()
            .resolve_path(&cwd, None)
            .await
            .expect_err("read failure surfaces");
        assert_eq!(error.operation, ProjectFaviconOperation::ReadSource);
        assert_eq!(error.relative_path.as_deref(), Some("index.html"));
        assert_eq!(
            error.absolute_path,
            Some(path_string(&temp.path().join("index.html")))
        );
    }

    #[tokio::test]
    async fn skips_icon_metadata_paths_outside_the_workspace() {
        let temp = temp_dir();
        write_text(
            temp.path(),
            "index.html",
            r#"<link rel="icon" href="../../secret.svg">"#,
        );

        assert_eq!(
            real_resolver()
                .resolve_path(&path_string(temp.path()), None)
                .await
                .expect("ok"),
            None
        );
    }

    #[tokio::test]
    async fn continues_to_later_sources_after_an_outside_root_icon_href() {
        let temp = temp_dir();
        write_text(
            temp.path(),
            "index.html",
            r#"<link rel="icon" href="../../secret.svg">"#,
        );
        write_text(
            temp.path(),
            "public/index.html",
            r#"<link rel="icon" href="/brand/logo.svg">"#,
        );
        write_text(temp.path(), "public/brand/logo.svg", "<svg>brand</svg>");

        let resolved = real_resolver()
            .resolve_path(&path_string(temp.path()), None)
            .await
            .expect("ok");
        assert_eq!(
            resolved,
            Some(path_string(
                &temp.path().join("public").join("brand").join("logo.svg")
            ))
        );
    }

    #[test]
    fn extracts_link_tags_in_any_attribute_order() {
        assert_eq!(
            extract_icon_href(r#"<LINK HREF="/a.svg" REL="shortcut icon">"#),
            Some("/a.svg".to_owned())
        );
        // A `<link>` whose href is empty or carries a query falls through.
        assert_eq!(extract_icon_href(r#"<link rel="icon" href="">"#), None);
        // The href capture stops at a query string.
        assert_eq!(
            extract_icon_href(r#"<link rel="icon" href="/a.svg?v=2">"#),
            Some("/a.svg".to_owned())
        );
    }

    #[test]
    fn ignores_non_icon_link_tags_and_links_without_a_closing_bracket() {
        assert_eq!(
            extract_icon_href(r#"<link rel="stylesheet" href="/a.css">"#),
            None
        );
        assert_eq!(extract_icon_href(r#"<linkrel="icon" href="/a.svg">"#), None);
        assert_eq!(extract_icon_href(r#"<link rel="icon" href="/a.svg""#), None);
    }

    #[test]
    fn cache_key_round_trips_like_upstream() {
        use super::{favicon_cache_key, parse_favicon_cache_key};
        assert_eq!(
            parse_favicon_cache_key(&favicon_cache_key("/w", Some("brand/x.svg"))),
            ("/w".to_owned(), Some("brand/x.svg".to_owned()))
        );
        assert_eq!(
            parse_favicon_cache_key(&favicon_cache_key("/w", None)),
            ("/w".to_owned(), None)
        );
        assert_eq!(
            parse_favicon_cache_key(&favicon_cache_key("/w", Some(""))),
            ("/w".to_owned(), None)
        );
    }
}
