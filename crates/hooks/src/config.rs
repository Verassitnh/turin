//! Hook configuration: `~/.turin/hooks.toml` and `<project>/.turin/hooks.toml`.
//!
//! New functionality (PORTING.md "Hooks (new)"); no upstream counterpart.
//! Env-var naming mirrors upstream `packages/shared/src/projectScripts.ts`
//! (`T3CODE_PROJECT_ROOT` / `T3CODE_WORKTREE_PATH` → `TURIN_*`).
//!
//! Loading never panics and never fails wholesale: a broken file or entry is
//! recorded as a [`HookConfigError`] (file + hook index) and skipped, and the
//! remaining hooks still run.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::event::HookEventKind;
use crate::trust::{TrustStore, sha256_hex};

/// `timeout_secs` for a hook that does not set one.
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Working directory a hook command runs in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookCwd {
    /// The project's workspace root.
    #[default]
    #[serde(rename = "project")]
    Project,
    /// The thread's workspace path. Falls back to the project root when the
    /// dispatched event carries no thread cwd.
    #[serde(rename = "thread")]
    Thread,
}

/// Which config file a hook came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookConfigSource {
    Global,
    Project,
}

impl fmt::Display for HookConfigSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Global => "global",
            Self::Project => "project",
        })
    }
}

/// One validated `[[hook]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookSpec {
    pub event: HookEventKind,
    /// Shell snippet executed with `/bin/sh -c`.
    pub command: String,
    pub cwd: HookCwd,
    /// Whether the orchestrator waits for the hook to finish. Only honored
    /// for `thread.created`; other events coerce it to non-blocking.
    pub blocking: bool,
    pub timeout_secs: u64,
    pub source: HookConfigSource,
    /// Index of the entry inside its own file, for error reporting.
    pub index: usize,
}

/// A config problem: which file, and which `[[hook]]` entry when known.
/// Invalid entries are skipped, never fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookConfigError {
    pub file: PathBuf,
    pub hook_index: Option<usize>,
    pub message: String,
}

impl fmt::Display for HookConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.hook_index {
            Some(index) => write!(
                f,
                "{}: hook[{index}]: {}",
                self.file.display(),
                self.message
            ),
            None => write!(f, "{}: {}", self.file.display(), self.message),
        }
    }
}

impl std::error::Error for HookConfigError {}

/// Parsed hooks from every config file plus every config error seen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HooksConfig {
    hooks: Vec<HookSpec>,
    errors: Vec<HookConfigError>,
    project: ProjectHooksStatus,
}

/// Why project hooks are not running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PendingReason {
    /// The user never approved this project's hooks.
    Untrusted,
    /// They were approved, but the file's bytes changed since (git pull,
    /// branch switch, an agent edit): re-approval required.
    Changed,
}

/// A project hooks file that is not trusted. Nothing in it runs. The UI
/// shows `hooks` for review and calls
/// [`crate::HookRunner::trust_project_hooks`] with `hash` on "Approve".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingProjectHooks {
    /// Canonical project root: the key the approval is stored under.
    pub project_root: PathBuf,
    pub file: PathBuf,
    /// SHA-256 (hex) of the exact file bytes `hooks` were parsed from.
    pub hash: String,
    pub reason: PendingReason,
    /// The valid entries in the file, for review. Never executed.
    pub hooks: Vec<HookSpec>,
}

/// Trust state of `<project>/.turin/hooks.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ProjectHooksStatus {
    /// No project hooks file.
    #[default]
    Absent,
    /// The file matches the approved hash; its hooks are loaded.
    Trusted { hash: String },
    /// Not approved or changed since approval; its hooks are inert.
    Pending(Box<PendingProjectHooks>),
}

impl HooksConfig {
    /// Loads both files. Missing files are fine (both are optional); broken
    /// files or entries are skipped and reported through [`Self::errors`].
    /// Global hooks run before project hooks, each file in `[[hook]]` order.
    ///
    /// The global file is trusted. The project file is **inert** until its
    /// exact bytes match the hash the user approved (see [`crate::trust`]):
    /// otherwise its hooks are parsed only to be shown for review, and the
    /// result is reported through [`Self::project_status`].
    pub fn load(paths: &HookPaths) -> Self {
        let mut hooks = Vec::new();
        let mut errors = Vec::new();
        if let Some(path) = paths.global.as_deref()
            && path.is_file()
        {
            match std::fs::read_to_string(path) {
                Ok(text) => {
                    let (file_hooks, file_errors) =
                        parse_text(path, &text, HookConfigSource::Global);
                    hooks.extend(file_hooks);
                    errors.extend(file_errors);
                }
                Err(err) => errors.push(HookConfigError {
                    file: path.to_path_buf(),
                    hook_index: None,
                    message: format!("cannot read hooks config: {err}"),
                }),
            }
        }

        let mut project = ProjectHooksStatus::Absent;
        if let Some(path) = paths.project.as_deref()
            && path.is_file()
        {
            // Hash and parse the same bytes: the hash that gates execution
            // must describe exactly what runs.
            match std::fs::read(path) {
                Ok(bytes) => {
                    let hash = sha256_hex(&bytes);
                    let (file_hooks, file_errors) = match String::from_utf8(bytes) {
                        Ok(text) => parse_text(path, &text, HookConfigSource::Project),
                        Err(_) => (
                            Vec::new(),
                            vec![HookConfigError {
                                file: path.to_path_buf(),
                                hook_index: None,
                                message: "hooks config is not valid UTF-8".to_owned(),
                            }],
                        ),
                    };
                    errors.extend(file_errors);
                    let root = paths.canonical_project_root();
                    let approved = root.as_deref().and_then(|root| {
                        paths
                            .trust_store
                            .as_deref()
                            .and_then(|store| TrustStore::new(store).lookup(root))
                    });
                    match approved {
                        Some(approved) if approved == hash => {
                            hooks.extend(file_hooks);
                            project = ProjectHooksStatus::Trusted { hash };
                        }
                        other => {
                            project = ProjectHooksStatus::Pending(Box::new(PendingProjectHooks {
                                project_root: root.unwrap_or_default(),
                                file: path.to_path_buf(),
                                hash,
                                reason: if other.is_some() {
                                    PendingReason::Changed
                                } else {
                                    PendingReason::Untrusted
                                },
                                hooks: file_hooks,
                            }));
                        }
                    }
                }
                Err(err) => errors.push(HookConfigError {
                    file: path.to_path_buf(),
                    hook_index: None,
                    message: format!("cannot read hooks config: {err}"),
                }),
            }
        }
        Self {
            hooks,
            errors,
            project,
        }
    }

    /// Whether the project's hooks file is absent, trusted, or waiting for
    /// the user's approval.
    pub fn project_status(&self) -> &ProjectHooksStatus {
        &self.project
    }

    /// The project hooks that are inert until approved, when there are any.
    pub fn pending_project_hooks(&self) -> Option<&PendingProjectHooks> {
        match &self.project {
            ProjectHooksStatus::Pending(pending) => Some(pending),
            _ => None,
        }
    }

    /// The hooks subscribed to `event`, global ones first, in file order.
    pub fn hooks_for(&self, event: HookEventKind) -> Vec<&HookSpec> {
        self.hooks
            .iter()
            .filter(|hook| hook.event == event)
            .collect()
    }

    /// Every loaded hook, in execution order.
    pub fn all_hooks(&self) -> &[HookSpec] {
        &self.hooks
    }

    /// Every config error seen while loading.
    pub fn errors(&self) -> &[HookConfigError] {
        &self.errors
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}

/// Raw `[[hook]]` table as written by the user. `deny_unknown_fields` turns
/// field typos into per-hook errors instead of silently ignored options.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookToml {
    event: String,
    command: String,
    cwd: Option<HookCwd>,
    blocking: Option<bool>,
    timeout_secs: Option<u64>,
}

fn parse_text(
    path: &Path,
    text: &str,
    source: HookConfigSource,
) -> (Vec<HookSpec>, Vec<HookConfigError>) {
    let mut hooks = Vec::new();
    let mut errors = Vec::new();
    let file_error = |message: String| HookConfigError {
        file: path.to_path_buf(),
        hook_index: None,
        message,
    };

    let value = match toml::from_str::<toml::Value>(text) {
        Ok(value) => value,
        Err(err) => {
            errors.push(file_error(format!("invalid TOML: {err}")));
            return (hooks, errors);
        }
    };
    let Some(table) = value.as_table() else {
        errors.push(file_error("hooks config must be a TOML table".to_owned()));
        return (hooks, errors);
    };
    for key in table.keys() {
        if key != "hook" {
            errors.push(file_error(format!(
                "unknown top-level key '{key}' (only [[hook]] entries are supported)"
            )));
        }
    }
    let Some(hook_value) = table.get("hook") else {
        return (hooks, errors);
    };
    let Some(entries) = hook_value.as_array() else {
        errors.push(file_error(
            "top-level 'hook' must be an array of tables ([[hook]])".to_owned(),
        ));
        return (hooks, errors);
    };
    for (index, entry) in entries.iter().enumerate() {
        match parse_entry(entry) {
            Ok(mut spec) => {
                spec.source = source;
                spec.index = index;
                hooks.push(spec);
            }
            Err(message) => errors.push(HookConfigError {
                file: path.to_path_buf(),
                hook_index: Some(index),
                message,
            }),
        }
    }
    (hooks, errors)
}

fn parse_entry(entry: &toml::Value) -> Result<HookSpec, String> {
    let raw =
        HookToml::deserialize(entry.clone()).map_err(|err| format!("invalid hook entry: {err}"))?;
    let event = HookEventKind::from_str(&raw.event).map_err(|err| err.to_string())?;
    if raw.command.trim().is_empty() {
        return Err("command must not be empty".to_owned());
    }
    if raw.timeout_secs == Some(0) {
        return Err("timeout_secs must be greater than zero".to_owned());
    }
    Ok(HookSpec {
        event,
        command: raw.command,
        cwd: raw.cwd.unwrap_or_default(),
        blocking: raw.blocking.unwrap_or(false),
        timeout_secs: raw.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
        // Replaced by the caller with the real source and file index.
        source: HookConfigSource::Global,
        index: 0,
    })
}

/// Locations of the two hooks config files and the trust store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookPaths {
    /// `~/.turin/hooks.toml`, or `$TURIN_HOME/hooks.toml` when set. Trusted.
    pub global: Option<PathBuf>,
    /// `<project>/.turin/hooks.toml`. Inert until approved.
    pub project: Option<PathBuf>,
    /// The project root the approval is keyed on. Without it the project
    /// file can never be approved.
    pub project_root: Option<PathBuf>,
    /// `~/.turin/trusted-hooks.json` (or under `$TURIN_HOME`): canonical
    /// project root → SHA-256 of the approved `hooks.toml`.
    pub trust_store: Option<PathBuf>,
}

impl HookPaths {
    pub fn for_project(project_root: &Path) -> Self {
        Self {
            global: default_global_path(),
            project: Some(project_root.join(".turin").join("hooks.toml")),
            project_root: Some(project_root.to_path_buf()),
            trust_store: crate::trust::default_trust_store_path(),
        }
    }

    /// The project root as the trust store keys it (symlinks resolved).
    pub(crate) fn canonical_project_root(&self) -> Option<PathBuf> {
        let root = self.project_root.as_deref()?;
        Some(std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()))
    }

    /// Whether a filesystem event at `path` can change what the config
    /// files say: the files themselves or any directory above them (a
    /// `.turin/` that appears or disappears). Paths are compared both as
    /// configured and with symlinks resolved, because FSEvents reports
    /// canonical paths even for a non-canonical watch.
    pub(crate) fn is_relevant_event_path(&self, path: &Path) -> bool {
        [self.global.as_deref(), self.project.as_deref()]
            .into_iter()
            .flatten()
            .any(|target| target.starts_with(path) || resolve_canonical(target).starts_with(path))
    }
}

/// Canonical form of a path that may not exist yet: the nearest existing
/// ancestor is canonicalized and the missing tail is appended unchanged.
pub(crate) fn resolve_canonical(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut tail = Vec::new();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(existing) {
            return tail
                .iter()
                .rev()
                .fold(canonical, |acc, part| acc.join(part));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name);
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Turin's home directory: `$TURIN_HOME` when set, else `~/.turin`; `None`
/// when no home directory can be found.
pub(crate) fn default_turin_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("TURIN_HOME")
        && !home.is_empty()
    {
        return Some(PathBuf::from(home));
    }
    dirs::home_dir().map(|home| home.join(".turin"))
}

/// Global config path: `$TURIN_HOME/hooks.toml` when `TURIN_HOME` is set,
/// else `~/.turin/hooks.toml`; `None` when no home directory can be found.
pub fn default_global_path() -> Option<PathBuf> {
    default_turin_home().map(|home| home.join("hooks.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::TrustStore;
    use pretty_assertions::assert_eq;

    fn write_config(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// Loads with the project file (if any) already approved, so these
    /// tests exercise parsing, not trust.
    fn load(global: Option<&Path>, project: Option<&Path>) -> HooksConfig {
        let paths = trusted_paths(global, project);
        HooksConfig::load(&paths)
    }

    fn trusted_paths(global: Option<&Path>, project: Option<&Path>) -> HookPaths {
        let root = project.map(|file| file.parent().unwrap().to_path_buf());
        let store = root.as_ref().map(|root| root.join("trusted-hooks.json"));
        if let (Some(file), Some(root), Some(store)) = (project, &root, &store)
            && file.is_file()
        {
            let bytes = std::fs::read(file).unwrap();
            TrustStore::new(store)
                .approve(&std::fs::canonicalize(root).unwrap(), &sha256_hex(&bytes))
                .unwrap();
        }
        HookPaths {
            global: global.map(Path::to_path_buf),
            project: project.map(Path::to_path_buf),
            project_root: root,
            trust_store: store,
        }
    }

    fn pending_reason(config: &HooksConfig) -> Option<PendingReason> {
        config.pending_project_hooks().map(|pending| pending.reason)
    }

    const PROJECT_HOOK: &str = "[[hook]]\nevent = \"turn.started\"\ncommand = \"project-cmd\"\n";

    fn project_fixture() -> (tempfile::TempDir, HookPaths) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let file = root.join(".turin").join("hooks.toml");
        write_config(&file, PROJECT_HOOK);
        let paths = HookPaths {
            global: None,
            project: Some(file),
            project_root: Some(root.clone()),
            trust_store: Some(root.join("home").join("trusted-hooks.json")),
        };
        (dir, paths)
    }

    fn approve(paths: &HookPaths) {
        let bytes = std::fs::read(paths.project.as_ref().unwrap()).unwrap();
        TrustStore::new(paths.trust_store.as_ref().unwrap())
            .approve(
                &paths.canonical_project_root().unwrap(),
                &sha256_hex(&bytes),
            )
            .unwrap();
    }

    #[test]
    fn untrusted_project_hooks_are_inert_and_pending() {
        let (_dir, paths) = project_fixture();
        let config = HooksConfig::load(&paths);
        assert!(config.all_hooks().is_empty(), "nothing may run");
        assert!(config.hooks_for(HookEventKind::TurnStarted).is_empty());
        assert_eq!(pending_reason(&config), Some(PendingReason::Untrusted));
        let pending = config.pending_project_hooks().unwrap();
        assert_eq!(pending.hooks.len(), 1, "parsed for review only");
        assert_eq!(pending.hooks[0].command, "project-cmd");
        assert_eq!(pending.hash, sha256_hex(PROJECT_HOOK.as_bytes()));
        assert_eq!(pending.file, *paths.project.as_ref().unwrap());
        assert_eq!(
            pending.project_root,
            paths.canonical_project_root().unwrap()
        );
    }

    #[test]
    fn trusted_project_hooks_load() {
        let (_dir, paths) = project_fixture();
        approve(&paths);
        let config = HooksConfig::load(&paths);
        assert_eq!(config.hooks_for(HookEventKind::TurnStarted).len(), 1);
        assert!(config.pending_project_hooks().is_none());
        assert_eq!(
            config.project_status(),
            &ProjectHooksStatus::Trusted {
                hash: sha256_hex(PROJECT_HOOK.as_bytes())
            }
        );
    }

    #[test]
    fn project_hooks_edited_after_trust_go_inert_as_changed() {
        let (_dir, paths) = project_fixture();
        approve(&paths);
        assert_eq!(HooksConfig::load(&paths).all_hooks().len(), 1);
        // A single appended byte is enough: the hash covers the exact bytes.
        let file = paths.project.as_ref().unwrap();
        let mut edited = std::fs::read_to_string(file).unwrap();
        edited.push('\n');
        std::fs::write(file, edited).unwrap();
        let config = HooksConfig::load(&paths);
        assert!(config.all_hooks().is_empty());
        assert_eq!(pending_reason(&config), Some(PendingReason::Changed));
        // Re-approving the new bytes restores it.
        approve(&paths);
        assert_eq!(HooksConfig::load(&paths).all_hooks().len(), 1);
    }

    #[test]
    fn global_hooks_stay_active_while_project_hooks_are_pending() {
        let (dir, paths) = project_fixture();
        let global = dir.path().join("global.toml");
        write_config(
            &global,
            "[[hook]]\nevent = \"turn.started\"\ncommand = \"global-cmd\"\n",
        );
        let config = HooksConfig::load(&HookPaths {
            global: Some(global),
            ..paths
        });
        let commands: Vec<&str> = config
            .all_hooks()
            .iter()
            .map(|hook| hook.command.as_str())
            .collect();
        assert_eq!(commands, ["global-cmd"]);
        assert!(config.pending_project_hooks().is_some());
    }

    #[test]
    fn approval_is_per_project_root() {
        let (_dir, paths) = project_fixture();
        approve(&paths);
        // Same bytes, different root: still untrusted.
        let other = tempfile::tempdir().unwrap();
        let other_file = other.path().join(".turin").join("hooks.toml");
        write_config(&other_file, PROJECT_HOOK);
        let config = HooksConfig::load(&HookPaths {
            global: None,
            project: Some(other_file),
            project_root: Some(other.path().to_path_buf()),
            trust_store: paths.trust_store.clone(),
        });
        assert_eq!(pending_reason(&config), Some(PendingReason::Untrusted));
    }

    #[test]
    fn project_hooks_without_a_trust_store_stay_pending() {
        let (_dir, mut paths) = project_fixture();
        paths.trust_store = None;
        let config = HooksConfig::load(&paths);
        assert!(config.all_hooks().is_empty());
        assert_eq!(pending_reason(&config), Some(PendingReason::Untrusted));
    }

    #[test]
    fn symlinked_project_root_shares_the_canonical_approval() {
        let (dir, paths) = project_fixture();
        approve(&paths);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        let config = HooksConfig::load(&HookPaths {
            global: None,
            project: Some(link.join(".turin").join("hooks.toml")),
            project_root: Some(link),
            trust_store: paths.trust_store.clone(),
        });
        assert_eq!(config.all_hooks().len(), 1);
    }

    #[test]
    fn relevant_event_paths_cover_files_and_their_directories() {
        let dir = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(dir.path()).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let paths = HookPaths {
            global: None,
            project: Some(link.join(".turin").join("hooks.toml")),
            project_root: None,
            trust_store: None,
        };
        // The file as configured, as canonical FSEvents reports it, and the
        // not-yet-existing `.turin` directory above it.
        assert!(paths.is_relevant_event_path(&link.join(".turin").join("hooks.toml")));
        assert!(paths.is_relevant_event_path(&real.join(".turin").join("hooks.toml")));
        assert!(paths.is_relevant_event_path(&real.join(".turin")));
        assert!(!paths.is_relevant_event_path(&real.join("other.txt")));
        assert!(!paths.is_relevant_event_path(&real.join(".turin").join("other.toml")));
    }

    #[test]
    fn parses_valid_hook_with_explicit_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.toml");
        write_config(
            &path,
            r#"
[[hook]]
event = "thread.created"
command = "./scripts/setup.sh"
cwd = "thread"
blocking = true
timeout_secs = 60
"#,
        );
        let config = load(Some(&path), None);
        assert!(config.errors().is_empty(), "{:?}", config.errors());
        assert_eq!(config.all_hooks().len(), 1);
        let hook = &config.all_hooks()[0];
        assert_eq!(hook.event, HookEventKind::ThreadCreated);
        assert_eq!(hook.command, "./scripts/setup.sh");
        assert_eq!(hook.cwd, HookCwd::Thread);
        assert!(hook.blocking);
        assert_eq!(hook.timeout_secs, 60);
        assert_eq!(hook.source, HookConfigSource::Global);
        assert_eq!(hook.index, 0);
    }

    #[test]
    fn applies_documented_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.toml");
        write_config(
            &path,
            r#"
[[hook]]
event = "turn.completed"
command = "notify-send done"
"#,
        );
        let config = load(Some(&path), None);
        assert!(config.errors().is_empty(), "{:?}", config.errors());
        let hook = &config.all_hooks()[0];
        assert_eq!(hook.cwd, HookCwd::Project);
        assert!(!hook.blocking);
        assert_eq!(hook.timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert_eq!(DEFAULT_TIMEOUT_SECS, 300);
    }

    #[test]
    fn rejects_unknown_event_with_file_and_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.toml");
        write_config(
            &path,
            r#"
[[hook]]
event = "turn.completed"
command = "ok"

[[hook]]
event = "thread.create"
command = "typo"
"#,
        );
        let config = load(Some(&path), None);
        assert_eq!(config.all_hooks().len(), 1, "valid hook is kept");
        let error = &config.errors()[0];
        assert_eq!(error.hook_index, Some(1));
        assert_eq!(error.file, path);
        let message = error.to_string();
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains("hook[1]"), "{message}");
        assert!(
            message.contains("unknown hook event 'thread.create'"),
            "{message}"
        );
    }

    #[test]
    fn rejects_unknown_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.toml");
        write_config(
            &path,
            r#"
[[hook]]
event = "turn.completed"
command = "true"
timeouts_secs = 300
"#,
        );
        let config = load(Some(&path), None);
        assert!(config.all_hooks().is_empty());
        let message = config.errors()[0].to_string();
        assert!(message.contains("hook[0]"), "{message}");
        assert!(message.contains("timeouts_secs"), "{message}");
    }

    #[test]
    fn rejects_zero_timeout_and_empty_command() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.toml");
        write_config(
            &path,
            r#"
[[hook]]
event = "turn.completed"
command = "true"
timeout_secs = 0

[[hook]]
event = "turn.completed"
command = "   "
"#,
        );
        let config = load(Some(&path), None);
        assert!(config.all_hooks().is_empty());
        assert_eq!(config.errors().len(), 2);
        assert!(config.errors()[0].message.contains("timeout_secs"));
        assert!(config.errors()[1].message.contains("command"));
    }

    #[test]
    fn rejects_unknown_cwd_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.toml");
        write_config(
            &path,
            r#"
[[hook]]
event = "turn.completed"
command = "true"
cwd = "worktree"
"#,
        );
        let config = load(Some(&path), None);
        assert!(config.all_hooks().is_empty());
        let message = config.errors()[0].to_string();
        assert!(message.contains("hook[0]"), "{message}");
        assert!(
            message.contains("expected `project` or `thread`"),
            "{message}"
        );
    }

    #[test]
    fn broken_toml_is_a_file_level_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.toml");
        write_config(&path, "event = ");
        let config = load(Some(&path), None);
        assert!(config.all_hooks().is_empty());
        let error = &config.errors()[0];
        assert_eq!(error.hook_index, None);
        let message = error.to_string();
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains("invalid TOML"), "{message}");
    }

    #[test]
    fn non_array_hook_key_is_a_file_level_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.toml");
        write_config(&path, "hook = \"setup.sh\"");
        let config = load(Some(&path), None);
        assert!(config.all_hooks().is_empty());
        assert!(config.errors()[0].message.contains("array of tables"));
    }

    #[test]
    fn unknown_top_level_key_is_a_file_level_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.toml");
        write_config(&path, "hooks = []\n");
        let config = load(Some(&path), None);
        assert!(config.all_hooks().is_empty());
        assert!(
            config.errors()[0]
                .message
                .contains("unknown top-level key 'hooks'")
        );
    }

    #[test]
    fn missing_files_are_silent() {
        let config = load(None, None);
        assert!(config.is_empty());
        assert!(config.errors().is_empty());

        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope").join("hooks.toml");
        let config = load(Some(&missing), None);
        assert!(config.is_empty());
        assert!(config.errors().is_empty());
    }

    #[test]
    fn project_hooks_come_after_global_hooks() {
        let global_dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let global_path = global_dir.path().join("hooks.toml");
        let project_path = project_dir.path().join(".turin").join("hooks.toml");
        write_config(
            &global_path,
            r#"
[[hook]]
event = "thread.created"
command = "global-one"

[[hook]]
event = "thread.created"
command = "global-two"
"#,
        );
        write_config(
            &project_path,
            r#"
[[hook]]
event = "thread.created"
command = "project-one"
"#,
        );
        let config = load(Some(&global_path), Some(&project_path));
        assert!(config.errors().is_empty(), "{:?}", config.errors());
        let commands: Vec<&str> = config
            .hooks_for(HookEventKind::ThreadCreated)
            .iter()
            .map(|hook| hook.command.as_str())
            .collect();
        assert_eq!(commands, ["global-one", "global-two", "project-one"]);
        assert_eq!(config.hooks_for(HookEventKind::TurnFailed).len(), 0);
    }

    #[test]
    fn indices_are_per_file() {
        let global_dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let global_path = global_dir.path().join("hooks.toml");
        let project_path = project_dir.path().join("hooks.toml");
        write_config(
            &global_path,
            "[[hook]]\nevent = \"pr.linked\"\ncommand = \"a\"\n",
        );
        write_config(
            &project_path,
            "[[hook]]\nevent = \"pr.linked\"\ncommand = \"b\"\n\n[[hook]]\nevent = \"pr.linked\"\ncommand = \"c\"\n",
        );
        let config = load(Some(&global_path), Some(&project_path));
        let hooks = config.hooks_for(HookEventKind::PrLinked);
        assert_eq!(hooks[0].source, HookConfigSource::Global);
        assert_eq!(hooks[0].index, 0);
        assert_eq!(hooks[1].source, HookConfigSource::Project);
        assert_eq!(hooks[1].index, 0);
        assert_eq!(hooks[2].source, HookConfigSource::Project);
        assert_eq!(hooks[2].index, 1);
    }

    #[test]
    fn one_bad_hook_does_not_drop_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hooks.toml");
        write_config(
            &path,
            r#"
[[hook]]
event = "git.committed"
command = "first"

[[hook]]
command = "no event"

[[hook]]
event = "git.committed"
command = "third"
"#,
        );
        let config = load(Some(&path), None);
        assert_eq!(config.all_hooks().len(), 2);
        assert_eq!(config.errors().len(), 1);
        assert_eq!(config.errors()[0].hook_index, Some(1));
        assert_eq!(config.all_hooks()[1].index, 2);
    }
}
