//! Port of upstream/apps/server/src/config.ts
//!
//! ServerConfig - Runtime configuration.
//!
//! Defines process-level configuration and the on-disk layout under the base
//! directory. Turin keeps upstream's layout (`<base>/userdata/settings.json`,
//! `<base>/userdata/keybindings.json`, `<base>/userdata/secrets/…`) under its
//! own base directory, `~/.turin`, so a copied `~/.t3` reads unchanged.
//! `resolve_base_dir` and `expand_home_path` are ported from
//! upstream/apps/server/src/os-jank.ts.
//!
//! Port of upstream/apps/server/src/os-jank.ts (`expandHomePath`, `resolveBaseDir`).

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::support::blocking::blocking;

/// Directory name under the home directory when no base dir is configured.
/// Upstream uses `.t3`.
pub const DEFAULT_BASE_DIR_NAME: &str = ".turin";

/// Environment variable naming the base dir (upstream `T3CODE_HOME`).
pub const BASE_DIR_ENV: &str = "TURIN_HOME";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeMode {
    Web,
    Desktop,
}

/// ServerDerivedPaths - Derived paths from the base directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerDerivedPaths {
    pub state_dir: PathBuf,
    pub db_path: PathBuf,
    pub keybindings_config_path: PathBuf,
    pub settings_path: PathBuf,
    /// Palettes this machine publishes for clients to follow, one file per theme.
    pub environment_themes_dir: PathBuf,
    pub provider_status_cache_dir: PathBuf,
    pub worktrees_dir: PathBuf,
    pub attachments_dir: PathBuf,
    /// Screenshots the agent asks the collaborative browser to keep for the user.
    pub browser_artifacts_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub server_trace_path: PathBuf,
    pub provider_logs_dir: PathBuf,
    pub provider_event_log_path: PathBuf,
    pub terminal_logs_dir: PathBuf,
    pub anonymous_id_path: PathBuf,
    pub environment_id_path: PathBuf,
    pub server_runtime_state_path: PathBuf,
    pub secrets_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeriveServerPathsOptions {
    pub base_dir_is_explicit: bool,
}

/// ServerConfig - runtime configuration shared by every service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub paths: ServerDerivedPaths,
    pub mode: RuntimeMode,
    pub cwd: PathBuf,
    pub base_dir: PathBuf,
    pub dev_url: Option<String>,
    pub auto_bootstrap_project_from_cwd: bool,
}

impl std::ops::Deref for ServerConfig {
    type Target = ServerDerivedPaths;
    fn deref(&self) -> &ServerDerivedPaths {
        &self.paths
    }
}

impl ServerConfig {
    /// Desktop configuration for `base_dir`: derives the layout and creates
    /// its directories.
    pub async fn new_desktop(
        cwd: PathBuf,
        base_dir: PathBuf,
        dev_url: Option<String>,
    ) -> io::Result<Self> {
        let paths = derive_server_paths(
            &base_dir,
            dev_url.as_deref(),
            DeriveServerPathsOptions::default(),
        );
        ensure_server_directories(&paths).await?;
        Ok(Self {
            paths,
            mode: RuntimeMode::Desktop,
            cwd,
            base_dir,
            dev_url,
            auto_bootstrap_project_from_cwd: false,
        })
    }

    /// Port of `layerTest(cwd, baseDir)`.
    pub async fn for_test(
        cwd: impl Into<PathBuf>,
        base_dir: impl Into<PathBuf>,
    ) -> io::Result<Self> {
        let base_dir = base_dir.into();
        let paths = derive_server_paths(&base_dir, None, DeriveServerPathsOptions::default());
        ensure_server_directories(&paths).await?;
        Ok(Self {
            paths,
            mode: RuntimeMode::Web,
            cwd: cwd.into(),
            base_dir,
            dev_url: None,
            auto_bootstrap_project_from_cwd: false,
        })
    }
}

pub fn derive_server_paths(
    base_dir: &Path,
    dev_url: Option<&str>,
    options: DeriveServerPathsOptions,
) -> ServerDerivedPaths {
    let state_dir = base_dir.join(if dev_url.is_some() && !options.base_dir_is_explicit {
        "dev"
    } else {
        "userdata"
    });
    let db_path = state_dir.join("statev2.sqlite");
    let attachments_dir = state_dir.join("attachments");
    let logs_dir = state_dir.join("logs");
    let provider_logs_dir = logs_dir.join("provider");
    let provider_status_cache_dir = base_dir.join("caches");
    ServerDerivedPaths {
        db_path,
        keybindings_config_path: state_dir.join("keybindings.json"),
        settings_path: state_dir.join("settings.json"),
        environment_themes_dir: state_dir.join("themes"),
        provider_status_cache_dir,
        worktrees_dir: base_dir.join("worktrees"),
        attachments_dir,
        browser_artifacts_dir: state_dir.join("browser-artifacts"),
        server_trace_path: logs_dir.join("server.trace.ndjson"),
        provider_event_log_path: provider_logs_dir.join("events.log"),
        provider_logs_dir,
        terminal_logs_dir: logs_dir.join("terminals"),
        logs_dir,
        anonymous_id_path: state_dir.join("anonymous-id"),
        environment_id_path: state_dir.join("environment-id"),
        server_runtime_state_path: state_dir.join("server-runtime.json"),
        secrets_dir: state_dir.join("secrets"),
        state_dir,
    }
}

/// Creates the server directories. Blocking filesystem work runs on the
/// blocking pool.
pub async fn ensure_server_directories(paths: &ServerDerivedPaths) -> io::Result<()> {
    let paths = paths.clone();
    blocking(move || create_server_directories(&paths)).await
}

fn create_server_directories(paths: &ServerDerivedPaths) -> io::Result<()> {
    let parent = |path: &Path| path.parent().map(Path::to_path_buf).unwrap_or_default();
    for directory in [
        paths.state_dir.clone(),
        paths.logs_dir.clone(),
        paths.provider_logs_dir.clone(),
        paths.terminal_logs_dir.clone(),
        paths.attachments_dir.clone(),
        paths.worktrees_dir.clone(),
        parent(&paths.keybindings_config_path),
        parent(&paths.settings_path),
        paths.provider_status_cache_dir.clone(),
        parent(&paths.anonymous_id_path),
        parent(&paths.server_runtime_state_path),
    ] {
        std::fs::create_dir_all(directory)?;
    }
    Ok(())
}

/// Port of os-jank.ts `expandHomePath`.
pub fn expand_home_path(input: &str, home: &Path) -> PathBuf {
    if input == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = input
        .strip_prefix("~/")
        .or_else(|| input.strip_prefix("~\\"))
    {
        return home.join(rest);
    }
    PathBuf::from(input)
}

/// Port of os-jank.ts `resolveBaseDir`: an empty value means
/// `<home>/.turin`; otherwise `~` expands and the path is made absolute.
pub fn resolve_base_dir(raw: Option<&str>, home: &Path) -> io::Result<PathBuf> {
    match raw.map(str::trim) {
        None | Some("") => Ok(home.join(DEFAULT_BASE_DIR_NAME)),
        Some(raw) => crate::support::symlink::lexical_absolute(&expand_home_path(raw, home)),
    }
}

/// The base dir from `TURIN_HOME`, falling back to `~/.turin`.
pub fn resolve_base_dir_from_env() -> io::Result<PathBuf> {
    let home = home_directory()?;
    resolve_base_dir(std::env::var(BASE_DIR_ENV).ok().as_deref(), &home)
}

/// `HostProcess.HomeDirectory`.
pub fn home_directory() -> io::Result<PathBuf> {
    dirs::home_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "home directory is unavailable"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_the_upstream_layout_under_userdata() {
        let paths = derive_server_paths(
            Path::new("/base"),
            None,
            DeriveServerPathsOptions::default(),
        );
        assert_eq!(paths.state_dir, Path::new("/base/userdata"));
        assert_eq!(
            paths.settings_path,
            Path::new("/base/userdata/settings.json")
        );
        assert_eq!(
            paths.keybindings_config_path,
            Path::new("/base/userdata/keybindings.json")
        );
        assert_eq!(paths.db_path, Path::new("/base/userdata/statev2.sqlite"));
        assert_eq!(paths.secrets_dir, Path::new("/base/userdata/secrets"));
        assert_eq!(paths.worktrees_dir, Path::new("/base/worktrees"));
        assert_eq!(paths.provider_status_cache_dir, Path::new("/base/caches"));
        assert_eq!(
            paths.server_trace_path,
            Path::new("/base/userdata/logs/server.trace.ndjson")
        );
        assert_eq!(
            paths.provider_event_log_path,
            Path::new("/base/userdata/logs/provider/events.log")
        );
        assert_eq!(
            paths.environment_id_path,
            Path::new("/base/userdata/environment-id")
        );
    }

    #[test]
    fn uses_the_dev_state_dir_only_for_implicit_dev_runs() {
        let dev = derive_server_paths(
            Path::new("/b"),
            Some("http://localhost:5173"),
            DeriveServerPathsOptions::default(),
        );
        assert_eq!(dev.state_dir, Path::new("/b/dev"));
        let explicit = derive_server_paths(
            Path::new("/b"),
            Some("http://localhost:5173"),
            DeriveServerPathsOptions {
                base_dir_is_explicit: true,
            },
        );
        assert_eq!(explicit.state_dir, Path::new("/b/userdata"));
    }

    #[test]
    fn resolves_the_base_dir_like_upstream() {
        let home = Path::new("/home/me");
        assert_eq!(
            resolve_base_dir(None, home).unwrap(),
            Path::new("/home/me/.turin")
        );
        assert_eq!(
            resolve_base_dir(Some("  "), home).unwrap(),
            Path::new("/home/me/.turin")
        );
        assert_eq!(
            resolve_base_dir(Some("~/data"), home).unwrap(),
            Path::new("/home/me/data")
        );
        assert_eq!(
            resolve_base_dir(Some(" /x/../y "), home).unwrap(),
            Path::new("/y")
        );
        assert_eq!(expand_home_path("~", home), Path::new("/home/me"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn creates_the_server_directories() {
        let root = tempfile::tempdir().unwrap();
        let config = ServerConfig::for_test(root.path(), root.path())
            .await
            .unwrap();
        for directory in [
            &config.state_dir,
            &config.logs_dir,
            &config.provider_logs_dir,
            &config.attachments_dir,
            &config.worktrees_dir,
            &config.provider_status_cache_dir,
        ] {
            assert!(directory.is_dir(), "{}", directory.display());
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_desktop_config_creates_its_directories_on_the_blocking_pool() {
        let root = tempfile::tempdir().unwrap();
        let base_dir = root.path().join("base");
        let config = ServerConfig::new_desktop(root.path().to_path_buf(), base_dir, None)
            .await
            .unwrap();
        assert!(config.state_dir.is_dir());
        assert!(config.keybindings_config_path.parent().unwrap().is_dir());
        assert!(config.server_runtime_state_path.parent().unwrap().is_dir());
    }
}
