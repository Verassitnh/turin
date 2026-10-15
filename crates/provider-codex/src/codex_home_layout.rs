//! Port of upstream/apps/server/src/provider/Drivers/CodexHomeLayout.ts and
//! upstream/apps/server/src/provider/CodexManagedHome.ts.
//!
//! Resolves the Codex home a session runs under. An auth overlay runs in a
//! shadow home that symlinks shared state from the user's real home and keeps
//! auth private. The filesystem work is sync `std::fs` and expects a blocking
//! context. Windows symlinks are not implemented: materializing an overlay
//! there returns an error.

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

use indexmap::IndexSet;
use turin_provider_core::path_expansion::expand_home_path;

const KNOWN_SHARED_DIRECTORIES: &[&str] = &[
    "sessions",
    "archived_sessions",
    "sqlite",
    "shell_snapshots",
    "worktrees",
    "skills",
    "plugins",
    "cache",
    "logs",
    "mcp-oauth-locks",
];

const PRIVATE_ENTRY_NAMES: &[&str] = &["auth.json", "models_cache.json"];
const SHADOW_LOCAL_ENTRY_NAMES: &[&str] = &["log", "memories", "tmp"];
// Codex creates these runtime locks itself when it starts in a shadow home before the shared
// home has one. They guard shared state, so a local copy is replaced with the shared link.
const REPLACEABLE_SHARED_RUNTIME_ENTRIES: &[&str] =
    &["mcp-oauth-locks", ".sqlite-maintenance.lock"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexHomeMode {
    Direct,
    AuthOverlay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexHomeLayout {
    pub mode: CodexHomeMode,
    pub shared_home_path: PathBuf,
    pub effective_home_path: Option<PathBuf>,
    pub continuation_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodexShadowHomeError {
    #[error(
        "Codex shadow home path '{effective}' must be different from the shared home path '{shared}'."
    )]
    PathConflict { shared: String, effective: String },
    #[error(
        "Cannot create Codex shadow home entry '{entry}' because '{link}' already exists and is not a symlink."
    )]
    EntryConflict { entry: String, link: String },
    #[error(
        "Codex shadow home private entry '{entry}' at '{path}' must be a real file, not a symlink."
    )]
    PrivateEntrySymlink { entry: String, path: String },
    #[error("Codex shadow home filesystem operation '{operation}' failed for '{path}'.")]
    FileSystem {
        operation: &'static str,
        path: String,
        detail: String,
    },
}

/// `resolveCodexHomeLayout`. `home` is the host home directory. An empty
/// `home_path` means the default `~/.codex`.
pub fn resolve_codex_home_layout(
    home_path: &str,
    shadow_home_path: &str,
    home: &str,
) -> CodexHomeLayout {
    let shared_home_path = resolve_home_path(home, home_path);
    let continuation_key = format!("codex:home:{}", shared_home_path.display());
    let shadow = shadow_home_path.trim();
    if shadow.is_empty() {
        let effective_home_path = (!home_path.trim().is_empty()).then(|| shared_home_path.clone());
        return CodexHomeLayout {
            mode: CodexHomeMode::Direct,
            shared_home_path,
            effective_home_path,
            continuation_key,
        };
    }
    CodexHomeLayout {
        mode: CodexHomeMode::AuthOverlay,
        shared_home_path,
        effective_home_path: Some(resolve_absolute(&expand_home_path(shadow, home))),
        continuation_key,
    }
}

/// `resolveManagedCodexHomeLayout`. Non-default instances without an explicit
/// shadow home get `<state>/providers/codex/<instance>/shadow`.
pub fn resolve_managed_codex_home_layout(
    state_dir: &Path,
    instance_id: &str,
    home_path: &str,
    shadow_home_path: &str,
    home: &str,
) -> CodexHomeLayout {
    let shadow = if !shadow_home_path.trim().is_empty() {
        shadow_home_path.to_owned()
    } else if instance_id == "codex" {
        String::new()
    } else {
        state_dir
            .join("providers")
            .join("codex")
            .join(instance_id)
            .join("shadow")
            .to_string_lossy()
            .into_owned()
    };
    resolve_codex_home_layout(home_path, &shadow, home)
}

fn resolve_home_path(home: &str, value: &str) -> PathBuf {
    let expanded = if value.trim().is_empty() {
        PathBuf::from(home).join(".codex")
    } else {
        PathBuf::from(expand_home_path(value, home))
    };
    resolve_absolute(&expanded.to_string_lossy())
}

/// Node's `path.resolve` for a single path: absolute and lexically normalized.
fn resolve_absolute(value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    let absolute = std::path::absolute(&path).unwrap_or(path);
    normalize(&absolute)
}

/// Lexical normalization, so `..` and `.` resolve without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn fs_error(operation: &'static str, path: &Path, error: io::Error) -> CodexShadowHomeError {
    CodexShadowHomeError::FileSystem {
        operation,
        path: path.to_string_lossy().into_owned(),
        detail: error.to_string(),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum LinkState {
    Missing,
    NotSymlink,
    Symlink(PathBuf),
}

fn read_link_state(link: &Path) -> Result<LinkState, CodexShadowHomeError> {
    match fs::symlink_metadata(link) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(LinkState::Missing),
        Err(error) => Err(fs_error("readLink", link, error)),
        Ok(metadata) if metadata.file_type().is_symlink() => fs::read_link(link)
            .map(LinkState::Symlink)
            .map_err(|error| fs_error("readLink", link, error)),
        Ok(_) => Ok(LinkState::NotSymlink),
    }
}

fn remove_path(path: &Path) -> Result<(), CodexShadowHomeError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| fs_error("remove", path, error))?;
    let removed = if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    removed.map_err(|error| fs_error("remove", path, error))
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> Result<(), CodexShadowHomeError> {
    std::os::unix::fs::symlink(target, link).map_err(|error| fs_error("symlink", link, error))
}

#[cfg(not(unix))]
fn create_symlink(_target: &Path, link: &Path) -> Result<(), CodexShadowHomeError> {
    Err(CodexShadowHomeError::FileSystem {
        operation: "symlink",
        path: link.to_string_lossy().into_owned(),
        detail: "shadow home symlinks are only implemented on Unix".to_owned(),
    })
}

/// `ensureSymlink`: `effective/<entry>` links to `shared/<entry>`.
fn ensure_symlink(
    shared: &Path,
    effective: &Path,
    entry: &str,
) -> Result<(), CodexShadowHomeError> {
    let target = shared.join(entry);
    let link = effective.join(entry);
    let state = read_link_state(&link)?;
    match state {
        LinkState::NotSymlink => {
            if !REPLACEABLE_SHARED_RUNTIME_ENTRIES.contains(&entry) {
                return Err(CodexShadowHomeError::EntryConflict {
                    entry: entry.to_owned(),
                    link: link.to_string_lossy().into_owned(),
                });
            }
            remove_path(&link)?;
            create_symlink(&target, &link)
        }
        LinkState::Missing => create_symlink(&target, &link),
        LinkState::Symlink(existing) => {
            let resolved = normalize(&link.parent().unwrap_or(effective).join(existing));
            if resolved != normalize(&target) {
                fs::remove_file(&link).map_err(|error| fs_error("remove", &link, error))?;
                create_symlink(&target, &link)?;
            }
            Ok(())
        }
    }
}

/// `removePrivateSymlink`: a private entry must not stay linked to the shared home.
fn remove_private_symlink(effective: &Path, entry: &str) -> Result<(), CodexShadowHomeError> {
    let path = effective.join(entry);
    if let LinkState::Symlink(_) = read_link_state(&path)? {
        fs::remove_file(&path).map_err(|error| fs_error("remove", &path, error))?;
    }
    Ok(())
}

/// `ensureShadowAuthIsPrivate`: `auth.json` in the shadow home must be a real file.
fn ensure_shadow_auth_is_private(effective: &Path) -> Result<(), CodexShadowHomeError> {
    let auth = effective.join("auth.json");
    if let LinkState::Symlink(_) = read_link_state(&auth)? {
        return Err(CodexShadowHomeError::PrivateEntrySymlink {
            entry: "auth.json".to_owned(),
            path: auth.to_string_lossy().into_owned(),
        });
    }
    Ok(())
}

/// `materializeCodexShadowHome`. Does nothing for a direct layout. Safe to
/// rerun: existing correct links are left alone.
pub fn materialize_codex_shadow_home(layout: &CodexHomeLayout) -> Result<(), CodexShadowHomeError> {
    if layout.mode != CodexHomeMode::AuthOverlay {
        return Ok(());
    }
    let Some(effective) = layout.effective_home_path.as_deref() else {
        return Ok(());
    };
    let shared = layout.shared_home_path.as_path();
    if shared == effective {
        return Err(CodexShadowHomeError::PathConflict {
            shared: shared.to_string_lossy().into_owned(),
            effective: effective.to_string_lossy().into_owned(),
        });
    }

    for directory in [shared, effective] {
        fs::create_dir_all(directory)
            .map_err(|error| fs_error("makeDirectory", directory, error))?;
    }
    for directory in KNOWN_SHARED_DIRECTORIES {
        let path = shared.join(directory);
        fs::create_dir_all(&path).map_err(|error| fs_error("makeDirectory", &path, error))?;
    }

    let shared_names = fs::read_dir(shared)
        .map_err(|error| fs_error("readDirectory", shared, error))?
        .map(|entry| {
            entry
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .map_err(|error| fs_error("readDirectory", shared, error))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut entries: IndexSet<String> = KNOWN_SHARED_DIRECTORIES
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    for name in shared_names {
        if !PRIVATE_ENTRY_NAMES.contains(&name.as_str())
            && !SHADOW_LOCAL_ENTRY_NAMES.contains(&name.as_str())
        {
            entries.insert(name);
        }
    }

    for entry in PRIVATE_ENTRY_NAMES {
        if *entry != "auth.json" {
            remove_private_symlink(effective, entry)?;
        }
    }
    for entry in &entries {
        if PRIVATE_ENTRY_NAMES.contains(&entry.as_str()) {
            continue;
        }
        ensure_symlink(shared, effective, entry)?;
    }
    ensure_shadow_auth_is_private(effective)
}

/// The managed launch args. Each `-c` override is single-quoted, as upstream
/// joins them into one string.
pub fn managed_codex_launch_args() -> String {
    const OVERRIDES: &[&str] = &[
        r#"model_provider="openai_token_sharing""#,
        r#"model_providers.openai_token_sharing.name="OpenAI Token Sharing""#,
        r#"model_providers.openai_token_sharing.base_url="https://api.openai.com/v1""#,
        r#"model_providers.openai_token_sharing.model_catalog_url="https://api.openai.com/v1/models""#,
        "features.api_key_model_discovery=true",
        "features.multi_agent=false",
        "features.multi_agent_v2=false",
        r#"model_providers.openai_token_sharing.env_key="ACCESS_TOKEN""#,
        r#"model_providers.openai_token_sharing.wire_api="responses""#,
        "model_providers.openai_token_sharing.requires_openai_auth=false",
        "model_providers.openai_token_sharing.supports_websockets=false",
    ];
    OVERRIDES
        .iter()
        .map(|value| format!("-c '{value}'"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The environment for a managed launch. T3 owns the token, so ambient
/// overrides that could redirect it to another provider are removed.
pub fn managed_runtime_environment(
    base: &turin_provider_core::host_process::ProcessEnv,
    access_token: &str,
    home_path: &str,
) -> turin_provider_core::host_process::ProcessEnv {
    let mut environment = base.clone();
    environment.insert("ACCESS_TOKEN".to_owned(), access_token.to_owned());
    environment.insert("CODEX_HOME".to_owned(), home_path.to_owned());
    for key in [
        "T3CODE_CODEX_LAUNCH_ARGS",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
    ] {
        environment.remove(key);
    }
    environment
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> (tempfile::TempDir, String) {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let home = home.to_string_lossy().into_owned();
        (temp, home)
    }

    #[test]
    fn direct_layout_without_a_home_path_uses_the_default_and_no_effective_path() {
        let (_temp, home) = tree();
        let layout = resolve_codex_home_layout("", "", &home);
        assert_eq!(layout.mode, CodexHomeMode::Direct);
        assert_eq!(layout.shared_home_path, PathBuf::from(&home).join(".codex"));
        assert_eq!(layout.effective_home_path, None);
    }

    #[test]
    fn explicit_home_path_is_its_own_effective_home() {
        let (temp, home) = tree();
        let custom = temp.path().join("custom-codex");
        let layout = resolve_codex_home_layout(&custom.to_string_lossy(), "", &home);
        assert_eq!(layout.effective_home_path, Some(custom.clone()));
        assert_eq!(layout.shared_home_path, custom);
    }

    #[test]
    fn shadow_home_selects_the_auth_overlay() {
        let (temp, home) = tree();
        let shadow = temp.path().join("shadow");
        let layout = resolve_codex_home_layout("", &shadow.to_string_lossy(), &home);
        assert_eq!(layout.mode, CodexHomeMode::AuthOverlay);
        assert_eq!(layout.effective_home_path, Some(shadow));
    }

    #[test]
    fn managed_default_shadow_is_per_instance_except_the_builtin_codex() {
        let (temp, home) = tree();
        let state = temp.path().join("state");
        let builtin = resolve_managed_codex_home_layout(&state, "codex", "", "", &home);
        assert_eq!(builtin.mode, CodexHomeMode::Direct);
        let work = resolve_managed_codex_home_layout(&state, "codex-work", "", "", &home);
        assert_eq!(work.mode, CodexHomeMode::AuthOverlay);
        assert_eq!(
            work.effective_home_path,
            Some(state.join("providers/codex/codex-work/shadow"))
        );
    }

    fn overlay(temp: &Path) -> (CodexHomeLayout, PathBuf, PathBuf) {
        let shared = temp.join("shared");
        let effective = temp.join("shadow");
        let layout = CodexHomeLayout {
            mode: CodexHomeMode::AuthOverlay,
            shared_home_path: shared.clone(),
            effective_home_path: Some(effective.clone()),
            continuation_key: "test".to_owned(),
        };
        (layout, shared, effective)
    }

    #[test]
    fn materialize_links_shared_state_and_keeps_auth_and_local_entries_private() {
        let temp = tempfile::tempdir().unwrap();
        let (layout, shared, effective) = overlay(temp.path());
        fs::create_dir_all(&shared).unwrap();
        fs::write(shared.join("config.toml"), "model = \"x\"").unwrap();
        fs::write(shared.join("auth.json"), "{}").unwrap();
        fs::create_dir_all(shared.join("memories")).unwrap();
        fs::create_dir_all(effective.join("log")).unwrap();

        materialize_codex_shadow_home(&layout).unwrap();

        assert!(
            fs::symlink_metadata(effective.join("sessions"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_link(effective.join("config.toml")).unwrap(),
            shared.join("config.toml")
        );
        assert!(!effective.join("auth.json").exists(), "auth stays private");
        assert!(
            !fs::symlink_metadata(effective.join("log"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            !effective.join("memories").exists(),
            "shadow-local entries are not linked"
        );
    }

    #[test]
    fn materialize_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let (layout, shared, _effective) = overlay(temp.path());
        fs::create_dir_all(&shared).unwrap();
        fs::write(shared.join("config.toml"), "").unwrap();
        materialize_codex_shadow_home(&layout).unwrap();
        materialize_codex_shadow_home(&layout).unwrap();
    }

    #[test]
    fn a_real_file_where_a_shared_link_belongs_is_a_conflict() {
        let temp = tempfile::tempdir().unwrap();
        let (layout, shared, effective) = overlay(temp.path());
        fs::create_dir_all(&shared).unwrap();
        fs::write(shared.join("config.toml"), "").unwrap();
        fs::create_dir_all(&effective).unwrap();
        fs::write(effective.join("config.toml"), "local").unwrap();

        let error = materialize_codex_shadow_home(&layout).unwrap_err();
        assert!(
            matches!(error, CodexShadowHomeError::EntryConflict { ref entry, .. } if entry == "config.toml")
        );
    }

    #[test]
    fn a_local_runtime_lock_is_replaced_by_the_shared_link() {
        let temp = tempfile::tempdir().unwrap();
        let (layout, shared, effective) = overlay(temp.path());
        fs::create_dir_all(shared.join("mcp-oauth-locks")).unwrap();
        fs::create_dir_all(effective.join("mcp-oauth-locks")).unwrap();

        materialize_codex_shadow_home(&layout).unwrap();

        assert_eq!(
            fs::read_link(effective.join("mcp-oauth-locks")).unwrap(),
            shared.join("mcp-oauth-locks")
        );
    }

    #[test]
    fn a_symlinked_auth_file_in_the_shadow_home_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let (layout, shared, effective) = overlay(temp.path());
        fs::create_dir_all(&shared).unwrap();
        fs::create_dir_all(&effective).unwrap();
        std::os::unix::fs::symlink(shared.join("auth.json"), effective.join("auth.json")).unwrap();

        let error = materialize_codex_shadow_home(&layout).unwrap_err();
        assert!(matches!(
            error,
            CodexShadowHomeError::PrivateEntrySymlink { .. }
        ));
    }

    #[test]
    fn shared_and_shadow_paths_must_differ() {
        let temp = tempfile::tempdir().unwrap();
        let shared = temp.path().join("same");
        let layout = CodexHomeLayout {
            mode: CodexHomeMode::AuthOverlay,
            shared_home_path: shared.clone(),
            effective_home_path: Some(shared),
            continuation_key: "test".to_owned(),
        };
        assert!(matches!(
            materialize_codex_shadow_home(&layout),
            Err(CodexShadowHomeError::PathConflict { .. })
        ));
    }

    #[test]
    fn launch_args_quote_each_override_and_join_with_spaces() {
        let args = managed_codex_launch_args();
        assert!(args.starts_with("-c 'model_provider=\"openai_token_sharing\"' -c '"));
        assert!(args.ends_with("model_providers.openai_token_sharing.supports_websockets=false'"));
        assert_eq!(args.matches("-c '").count(), 11);
    }

    #[test]
    fn managed_environment_sets_token_and_drops_ambient_overrides() {
        let base = turin_provider_core::host_process::ProcessEnv::from([
            ("OPENAI_API_KEY".to_owned(), "ambient".to_owned()),
            ("OPENAI_BASE_URL".to_owned(), "https://elsewhere".to_owned()),
            ("T3CODE_CODEX_LAUNCH_ARGS".to_owned(), "--x".to_owned()),
            ("PATH".to_owned(), "/usr/bin".to_owned()),
        ]);
        let env = managed_runtime_environment(&base, "token-1", "/home/x/.codex");
        assert_eq!(env.get("ACCESS_TOKEN").map(String::as_str), Some("token-1"));
        assert_eq!(
            env.get("CODEX_HOME").map(String::as_str),
            Some("/home/x/.codex")
        );
        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
        for key in [
            "OPENAI_API_KEY",
            "OPENAI_BASE_URL",
            "T3CODE_CODEX_LAUNCH_ARGS",
        ] {
            assert!(!env.contains_key(key), "{key}");
        }
    }

    #[test]
    fn normalize_collapses_parent_and_current_components() {
        assert_eq!(
            normalize(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
    }
}
