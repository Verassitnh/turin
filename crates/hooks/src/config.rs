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
}

impl HooksConfig {
    /// Loads both files. Missing files are fine (both are optional); broken
    /// files or entries are skipped and reported through [`Self::errors`].
    /// Global hooks run before project hooks, each file in `[[hook]]` order.
    pub fn load(paths: &HookPaths) -> Self {
        let mut hooks = Vec::new();
        let mut errors = Vec::new();
        for (path, source) in [
            (paths.global.as_deref(), HookConfigSource::Global),
            (paths.project.as_deref(), HookConfigSource::Project),
        ] {
            let Some(path) = path else { continue };
            if !path.is_file() {
                continue;
            }
            let (file_hooks, file_errors) = parse_file(path, source);
            hooks.extend(file_hooks);
            errors.extend(file_errors);
        }
        Self { hooks, errors }
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

fn parse_file(path: &Path, source: HookConfigSource) -> (Vec<HookSpec>, Vec<HookConfigError>) {
    let mut hooks = Vec::new();
    let mut errors = Vec::new();
    let file_error = |message: String| HookConfigError {
        file: path.to_path_buf(),
        hook_index: None,
        message,
    };

    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            errors.push(file_error(format!("cannot read hooks config: {err}")));
            return (hooks, errors);
        }
    };
    let value = match toml::from_str::<toml::Value>(&text) {
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

/// Locations of the two hooks config files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookPaths {
    /// `~/.turin/hooks.toml`, or `$TURIN_HOME/hooks.toml` when set.
    pub global: Option<PathBuf>,
    /// `<project>/.turin/hooks.toml`.
    pub project: Option<PathBuf>,
}

impl HookPaths {
    pub fn for_project(project_root: &Path) -> Self {
        Self {
            global: default_global_path(),
            project: Some(project_root.join(".turin").join("hooks.toml")),
        }
    }

    /// Whether `path` names one of the config files (direct or canonicalized
    /// comparison, so events fired by an atomic-save rename still match).
    pub(crate) fn contains_file(&self, path: &Path) -> bool {
        [self.global.as_deref(), self.project.as_deref()]
            .iter()
            .flatten()
            .any(|target| paths_match(path, target))
    }
}

/// Global config path: `$TURIN_HOME/hooks.toml` when `TURIN_HOME` is set,
/// else `~/.turin/hooks.toml`; `None` when no home directory can be found.
pub fn default_global_path() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("TURIN_HOME")
        && !home.is_empty()
    {
        return Some(PathBuf::from(home).join("hooks.toml"));
    }
    dirs::home_dir().map(|home| home.join(".turin").join("hooks.toml"))
}

fn paths_match(a: &Path, b: &Path) -> bool {
    a == b
        || match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn write_config(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn load(global: Option<&Path>, project: Option<&Path>) -> HooksConfig {
        HooksConfig::load(&HookPaths {
            global: global.map(Path::to_path_buf),
            project: project.map(Path::to_path_buf),
        })
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
