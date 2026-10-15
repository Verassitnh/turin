//! Port of upstream/apps/server/src/keybindings.ts
//!
//! Owns parsing, validation, merge, and persistence of the user keybindings
//! file (`<userdata>/keybindings.json`). Upstream is an Effect service with a
//! cache, a PubSub, a semaphore and a file watcher; here it is a plain struct
//! behind `Arc`. Writes serialize on `write_lock`, the cache is a `Mutex`, and
//! change subscribers each get an unbounded channel.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use indexmap::IndexSet;
use notify::RecommendedWatcher;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use turin_contracts::keybindings::{
    KeybindingCommand, KeybindingRule, KeybindingShortcut, KeybindingValue, KeybindingWhen,
    KeybindingsConfigError, MAX_KEYBINDINGS_COUNT, ResolvedKeybindingRule,
};

use crate::shared_keybindings::{
    DEFAULT_KEYBINDINGS, compile_resolved_keybinding_rule, compile_resolved_keybindings_config,
    merge_with_default_keybindings, parse_keybinding_shortcut,
};
use crate::support::atomic_write::write_file_string_atomically;
use crate::support::file_watch::{WATCH_DEBOUNCE, next_debounced, watch_file};
use crate::support::lenient_json::{parse_lenient_json, to_json_string_pretty};

/// A problem with the file that does not stop the rest of it from loading.
/// Mirrors upstream `ServerConfigIssue`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind")]
pub enum ServerConfigIssue {
    #[serde(rename = "keybindings.malformed-config")]
    MalformedConfig { message: String },
    #[serde(rename = "keybindings.invalid-entry")]
    InvalidEntry { index: usize, message: String },
}

/// `KeybindingsConfigState` and `KeybindingsChangeEvent` share one shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingsConfigState {
    pub keybindings: Vec<ResolvedKeybindingRule>,
    pub issues: Vec<ServerConfigIssue>,
}

/// `ServerUpsertKeybindingInput`. `replace` names the existing rule to drop
/// (exact key/command/when match) before `rule` is added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpsertKeybindingInput {
    pub rule: KeybindingRule,
    pub replace: Option<KeybindingRule>,
}

/// Default rules added after the startup sync already ran. The backfill only
/// adds a rule next to its untouched `alongside` rule, and records the id so
/// it runs once per config file.
struct LateDefaultKeybinding {
    id: &'static str,
    alongside: KeybindingRule,
    rule: KeybindingRule,
}

fn static_rule(key: &str, command: &str, when: Option<&str>) -> KeybindingRule {
    KeybindingRule {
        key: KeybindingValue::new(key).expect("static late default key"),
        command: KeybindingCommand::new(command).expect("static late default command"),
        when: when.map(|when| KeybindingWhen::new(when).expect("static late default when")),
    }
}

static LATE_DEFAULT_KEYBINDINGS: LazyLock<Vec<LateDefaultKeybinding>> = LazyLock::new(|| {
    vec![LateDefaultKeybinding {
        id: "composer.sendBackground:mod+enter",
        alongside: static_rule(
            "mod+alt+enter",
            "composer.sendBackground",
            Some("composerFocus && draftThreadRoute"),
        ),
        rule: static_rule(
            "mod+enter",
            "composer.sendBackground",
            Some("composerFocus && draftThreadRoute"),
        ),
    }]
});

/// `isSameKeybindingRule`: `None` and absent `when` compare equal, which the
/// derived `PartialEq` already does.
fn is_same_rule(left: &KeybindingRule, right: &KeybindingRule) -> bool {
    left == right
}

/// `keybindingShortcutContext`: the encoded shortcut plus `when`. Two rules
/// with the same context fight for the same key press.
fn shortcut_context(rule: &KeybindingRule) -> Option<String> {
    let parsed = parse_keybinding_shortcut(rule.key.as_str())?;
    let encoded = encode_shortcut(&parsed)?;
    Some(format!(
        "{encoded}\u{0}{}",
        rule.when.as_ref().map_or("", KeybindingWhen::as_str)
    ))
}

fn has_same_shortcut_context(left: &KeybindingRule, right: &KeybindingRule) -> bool {
    match (shortcut_context(left), shortcut_context(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// `encodeShortcut`: canonical `mod+meta+ctrl+alt+shift+key` string.
fn encode_shortcut(shortcut: &KeybindingShortcut) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    if shortcut.mod_key {
        parts.push("mod");
    }
    if shortcut.meta_key {
        parts.push("meta");
    }
    if shortcut.ctrl_key {
        parts.push("ctrl");
    }
    if shortcut.alt_key {
        parts.push("alt");
    }
    if shortcut.shift_key {
        parts.push("shift");
    }
    let key = shortcut.key.as_str();
    if key.is_empty() {
        return None;
    }
    if key != "+" && key.contains('+') {
        return None;
    }
    let key = if key == " " { "space" } else { key };
    parts.push(key);
    Some(parts.join("+"))
}

fn trim_issue_message(message: &str) -> String {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        "Invalid keybindings configuration.".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn malformed_config_issue(detail: &str) -> ServerConfigIssue {
    ServerConfigIssue::MalformedConfig {
        message: trim_issue_message(detail),
    }
}

fn invalid_entry_issue(index: usize, detail: &str) -> ServerConfigIssue {
    ServerConfigIssue::InvalidEntry {
        index,
        message: trim_issue_message(detail),
    }
}

fn config_error(
    config_path: &Path,
    detail: &str,
    cause: impl std::fmt::Display,
) -> KeybindingsConfigError {
    KeybindingsConfigError::ParseError {
        config_path: config_path.display().to_string(),
        detail: detail.to_owned(),
        cause: Some(Value::String(cause.to_string())),
    }
}

/// `RawKeybindingsEntries`: lenient JSON whose top level must be an array.
fn parse_raw_entries(raw: &str) -> Result<Vec<Value>, String> {
    match parse_lenient_json(raw) {
        Ok(Value::Array(entries)) => Ok(entries),
        Ok(_) => Err("expected a JSON array".to_owned()),
        Err(error) => Err(error.to_string()),
    }
}

/// Decodes one entry as a `KeybindingRule` and checks that it also compiles
/// (`ResolvedKeybindingFromConfig`). The error is the issue detail.
fn decode_rule_entry(entry: &Value) -> Result<KeybindingRule, String> {
    let rule: KeybindingRule =
        serde_json::from_value(entry.clone()).map_err(|error| error.to_string())?;
    if compile_resolved_keybinding_rule(&rule).is_none() {
        return Err("Invalid keybinding rule".to_owned());
    }
    Ok(rule)
}

/// `Keybindings` service. Create with [`Keybindings::new`], then call
/// [`Keybindings::start`] once to sync defaults and attach the file watcher.
pub struct Keybindings {
    config_path: PathBuf,
    write_lock: Mutex<()>,
    cache: Mutex<Option<KeybindingsConfigState>>,
    subscribers: Mutex<Vec<mpsc::UnboundedSender<KeybindingsConfigState>>>,
}

/// Owns the file watcher and the task that reloads on change. Dropping it
/// stops watching.
pub struct KeybindingsWatcher {
    _watcher: RecommendedWatcher,
    task: JoinHandle<()>,
}

impl Drop for KeybindingsWatcher {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Keybindings {
    pub fn new(config_path: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            config_path: config_path.into(),
            write_lock: Mutex::new(()),
            cache: Mutex::new(None),
            subscribers: Mutex::new(Vec::new()),
        })
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Port of `start`: ensure the config directory, attach the watcher, sync
    /// defaults, then load the snapshot. Must run inside a tokio runtime (the
    /// reload task is spawned here). The caller keeps the returned watcher
    /// alive for as long as it wants changes.
    pub fn start(self: &Arc<Self>) -> Result<KeybindingsWatcher, KeybindingsConfigError> {
        let directory = self
            .config_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        std::fs::create_dir_all(&directory).map_err(|error| {
            config_error(
                &self.config_path,
                "failed to prepare keybindings config directory",
                error,
            )
        })?;

        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let watcher = watch_file(&self.config_path, events_tx).map_err(|error| {
            config_error(
                &self.config_path,
                "failed to watch keybindings config",
                error,
            )
        })?;

        let this = Arc::clone(self);
        let task = tokio::spawn(async move {
            while next_debounced(&mut events_rx, WATCH_DEBOUNCE)
                .await
                .is_some()
            {
                // Upstream logs and keeps watching when a reload fails.
                if let Err(error) = this.revalidate_and_emit() {
                    tracing::warn!(error = %error, "keybindings reload failed");
                }
            }
        });

        self.sync_default_keybindings_on_startup()?;
        self.invalidate_cache();
        self.snapshot()?;
        Ok(KeybindingsWatcher {
            _watcher: watcher,
            task,
        })
    }

    /// Port of `streamChanges` (PubSub.unbounded): each subscriber gets its
    /// own channel and never blocks a publisher.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<KeybindingsConfigState> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subscribers.lock().push(tx);
        rx
    }

    /// Port of `getSnapshot` / `loadConfigState`. A failed read is not cached,
    /// so the next call retries.
    pub fn snapshot(&self) -> Result<KeybindingsConfigState, KeybindingsConfigError> {
        if let Some(state) = self.cache.lock().clone() {
            return Ok(state);
        }
        let state = self.load_state_from_disk()?;
        *self.cache.lock() = Some(state.clone());
        Ok(state)
    }

    /// Port of `upsertKeybindingRule`. Writes atomically, caps the file at
    /// [`MAX_KEYBINDINGS_COUNT`] (oldest first), and returns the merged rules.
    pub fn upsert_keybinding_rule(
        &self,
        input: UpsertKeybindingInput,
    ) -> Result<Vec<ResolvedKeybindingRule>, KeybindingsConfigError> {
        let _write = self.write_lock.lock();
        let custom = self.load_writable_custom()?;
        let rule = input.rule;
        let mut next: Vec<KeybindingRule> = custom
            .into_iter()
            .filter(|entry| match &input.replace {
                Some(target) => !is_same_rule(entry, target) && !is_same_rule(entry, &rule),
                None => !is_same_rule(entry, &rule),
            })
            .collect();
        next.push(rule);
        if next.len() > MAX_KEYBINDINGS_COUNT {
            tracing::warn!(
                path = %self.config_path.display(),
                max_entries = MAX_KEYBINDINGS_COUNT,
                "truncating keybindings config to max entries"
            );
            let excess = next.len() - MAX_KEYBINDINGS_COUNT;
            next.drain(..excess);
        }
        self.write_config(&next)?;
        self.publish_rules(&next)
    }

    /// Port of `removeKeybindingRule`: drops the exact key/command/when match.
    pub fn remove_keybinding_rule(
        &self,
        target: &KeybindingRule,
    ) -> Result<Vec<ResolvedKeybindingRule>, KeybindingsConfigError> {
        let _write = self.write_lock.lock();
        let custom = self.load_writable_custom()?;
        let next: Vec<KeybindingRule> = custom
            .into_iter()
            .filter(|entry| !is_same_rule(entry, target))
            .collect();
        self.write_config(&next)?;
        self.publish_rules(&next)
    }

    /// Port of `syncDefaultKeybindingsOnStartup`: writes the defaults when the
    /// file is missing; otherwise appends defaults whose command and shortcut
    /// context are both free, keeping existing custom rules first in priority.
    pub fn sync_default_keybindings_on_startup(&self) -> Result<(), KeybindingsConfigError> {
        let _write = self.write_lock.lock();
        let applied = self.read_applied_migration_ids();
        let pending_late: Vec<&LateDefaultKeybinding> = LATE_DEFAULT_KEYBINDINGS
            .iter()
            .filter(|late| !applied.contains(late.id))
            .collect();

        if !self.config_exists()? {
            self.write_config(&DEFAULT_KEYBINDINGS)?;
            if !pending_late.is_empty() {
                self.record_applied_migrations(
                    applied
                        .iter()
                        .map(String::as_str)
                        .chain(LATE_DEFAULT_KEYBINDINGS.iter().map(|late| late.id)),
                )?;
            }
            self.invalidate_cache();
            return Ok(());
        }

        let (custom, issues) = self.load_runtime_custom()?;
        if !issues.is_empty() {
            tracing::warn!(
                path = %self.config_path.display(),
                ?issues,
                "skipping startup keybindings default sync because config has issues"
            );
            self.invalidate_cache();
            return Ok(());
        }

        let existing_commands: HashSet<&str> =
            custom.iter().map(|entry| entry.command.as_str()).collect();
        let mut missing_defaults: Vec<KeybindingRule> = Vec::new();
        let mut shortcut_conflicts: Vec<(&KeybindingRule, &KeybindingRule)> = Vec::new();
        for default_rule in DEFAULT_KEYBINDINGS.iter() {
            if existing_commands.contains(default_rule.command.as_str()) {
                continue;
            }
            if let Some(conflict) = custom
                .iter()
                .find(|entry| has_same_shortcut_context(entry, default_rule))
            {
                shortcut_conflicts.push((default_rule, conflict));
                continue;
            }
            missing_defaults.push(default_rule.clone());
        }
        // Late defaults join an untouched earlier default when their shortcut
        // is still free.
        for late in &pending_late {
            if custom
                .iter()
                .any(|entry| is_same_rule(entry, &late.alongside))
                && !custom
                    .iter()
                    .any(|entry| has_same_shortcut_context(entry, &late.rule))
            {
                missing_defaults.push(late.rule.clone());
            }
        }
        for (default_rule, conflict) in &shortcut_conflicts {
            tracing::warn!(
                path = %self.config_path.display(),
                default_command = %default_rule.command.as_str(),
                conflicting_command = %conflict.command.as_str(),
                key = %default_rule.key.as_str(),
                reason = "shortcut context already used by existing rule",
                "skipping default keybinding due to shortcut conflict"
            );
        }
        if !missing_defaults.is_empty() {
            let matching: Vec<&str> = DEFAULT_KEYBINDINGS
                .iter()
                .filter(|default_rule| custom.iter().any(|entry| is_same_rule(entry, default_rule)))
                .map(|default_rule| default_rule.command.as_str())
                .collect();
            if !matching.is_empty() {
                tracing::warn!(
                    path = %self.config_path.display(),
                    commands = ?matching,
                    "default keybinding rule already defined in user config"
                );
            }
        }

        // Only defaults that fit are appended, placed before the persisted
        // rules so existing custom shortcuts keep priority.
        let available_slots = MAX_KEYBINDINGS_COUNT.saturating_sub(custom.len());
        let split_at = available_slots.min(missing_defaults.len());
        let skipped_defaults = missing_defaults.split_off(split_at);
        let defaults_to_append = missing_defaults;
        if !skipped_defaults.is_empty() {
            let commands: Vec<&str> = skipped_defaults
                .iter()
                .map(|rule| rule.command.as_str())
                .collect();
            tracing::warn!(
                path = %self.config_path.display(),
                max_entries = MAX_KEYBINDINGS_COUNT,
                ?commands,
                "skipping default keybinding backfill at max entries"
            );
        }
        if !defaults_to_append.is_empty() {
            let mut combined = defaults_to_append;
            combined.extend(custom.iter().cloned());
            self.write_config(&combined)?;
        }

        // A late default skipped at max entries stays pending for a later start.
        let settled: Vec<&LateDefaultKeybinding> = pending_late
            .iter()
            .copied()
            .filter(|late| !skipped_defaults.contains(&late.rule))
            .collect();
        if !settled.is_empty() {
            self.record_applied_migrations(
                applied
                    .iter()
                    .map(String::as_str)
                    .chain(settled.iter().map(|late| late.id)),
            )?;
        }
        self.invalidate_cache();
        Ok(())
    }

    /// Port of `revalidateAndEmit`, run by the watcher after each settled burst.
    fn revalidate_and_emit(&self) -> Result<(), KeybindingsConfigError> {
        let _write = self.write_lock.lock();
        self.invalidate_cache();
        let state = self.snapshot()?;
        self.emit(state);
        Ok(())
    }

    fn publish_rules(
        &self,
        rules: &[KeybindingRule],
    ) -> Result<Vec<ResolvedKeybindingRule>, KeybindingsConfigError> {
        let merged = merge_with_default_keybindings(compile_resolved_keybindings_config(rules));
        let state = KeybindingsConfigState {
            keybindings: merged.clone(),
            issues: Vec::new(),
        };
        *self.cache.lock() = Some(state.clone());
        self.emit(state);
        Ok(merged)
    }

    fn emit(&self, state: KeybindingsConfigState) {
        self.subscribers
            .lock()
            .retain(|tx| tx.send(state.clone()).is_ok());
    }

    fn invalidate_cache(&self) {
        *self.cache.lock() = None;
    }

    fn load_state_from_disk(&self) -> Result<KeybindingsConfigState, KeybindingsConfigError> {
        let (custom, issues) = self.load_runtime_custom()?;
        Ok(KeybindingsConfigState {
            keybindings: merge_with_default_keybindings(compile_resolved_keybindings_config(
                &custom,
            )),
            issues,
        })
    }

    fn config_exists(&self) -> Result<bool, KeybindingsConfigError> {
        self.config_path.try_exists().map_err(|error| {
            config_error(
                &self.config_path,
                "failed to access keybindings config",
                error,
            )
        })
    }

    fn read_raw_config(&self) -> Result<String, KeybindingsConfigError> {
        std::fs::read_to_string(&self.config_path).map_err(|error| {
            config_error(
                &self.config_path,
                "failed to read keybindings config",
                error,
            )
        })
    }

    /// `loadWritableCustomKeybindingsConfig`: silently drops invalid entries,
    /// and any read or parse failure is reported as "expected JSON array".
    fn load_writable_custom(&self) -> Result<Vec<KeybindingRule>, KeybindingsConfigError> {
        if !self.config_exists()? {
            return Ok(Vec::new());
        }
        // Upstream maps read failures to the same "expected JSON array" detail.
        let raw = std::fs::read_to_string(&self.config_path)
            .map_err(|error| config_error(&self.config_path, "expected JSON array", error))?;
        let entries = parse_raw_entries(&raw)
            .map_err(|cause| config_error(&self.config_path, "expected JSON array", cause))?;
        let mut rules = Vec::new();
        for entry in &entries {
            match decode_rule_entry(entry) {
                Ok(rule) => rules.push(rule),
                Err(detail) => {
                    tracing::warn!(
                        path = %self.config_path.display(),
                        entry = %entry,
                        error = %detail,
                        "ignoring invalid keybinding entry"
                    );
                }
            }
        }
        Ok(rules)
    }

    /// `loadRuntimeCustomKeybindingsConfig`: like the writable loader, but
    /// reports what it skipped as issues instead of hiding it.
    fn load_runtime_custom(
        &self,
    ) -> Result<(Vec<KeybindingRule>, Vec<ServerConfigIssue>), KeybindingsConfigError> {
        if !self.config_exists()? {
            return Ok((Vec::new(), Vec::new()));
        }
        let raw = self.read_raw_config()?;
        let entries = match parse_raw_entries(&raw) {
            Ok(entries) => entries,
            Err(cause) => {
                let detail = format!("expected JSON array ({cause})");
                return Ok((Vec::new(), vec![malformed_config_issue(&detail)]));
            }
        };
        let mut rules = Vec::new();
        let mut issues = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            match decode_rule_entry(entry) {
                Ok(rule) => rules.push(rule),
                Err(detail) => {
                    issues.push(invalid_entry_issue(index, &detail));
                    tracing::warn!(
                        path = %self.config_path.display(),
                        index,
                        entry = %entry,
                        error = %detail,
                        "ignoring invalid keybinding entry"
                    );
                }
            }
        }
        Ok((rules, issues))
    }

    /// Migration ids live in `keybindings-migrations`, one per line, beside
    /// the config. A missing or unreadable file means none are applied.
    fn applied_migrations_path(&self) -> PathBuf {
        self.config_path
            .parent()
            .unwrap_or(Path::new(""))
            .join("keybindings-migrations")
    }

    fn read_applied_migration_ids(&self) -> IndexSet<String> {
        std::fs::read_to_string(self.applied_migrations_path())
            .map(|contents| {
                contents
                    .split('\n')
                    .filter(|line| !line.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn record_applied_migrations<'a>(
        &self,
        ids: impl Iterator<Item = &'a str>,
    ) -> Result<(), KeybindingsConfigError> {
        let unique: IndexSet<&str> = ids.collect();
        let contents = format!("{}\n", unique.into_iter().collect::<Vec<_>>().join("\n"));
        write_file_string_atomically(&self.applied_migrations_path(), &contents).map_err(|error| {
            config_error(
                &self.config_path,
                "failed to record keybinding migrations",
                error,
            )
        })
    }

    /// `writeConfigAtomically`: pretty JSON with a trailing newline, the same
    /// bytes `JSON.stringify(rules, null, 2)` plus `\n` produce.
    fn write_config(&self, rules: &[KeybindingRule]) -> Result<(), KeybindingsConfigError> {
        let encoded = to_json_string_pretty(rules).map_err(|error| {
            config_error(
                &self.config_path,
                "failed to write keybindings config",
                error,
            )
        })?;
        write_file_string_atomically(&self.config_path, &format!("{encoded}\n")).map_err(|error| {
            config_error(
                &self.config_path,
                "failed to write keybindings config",
                error,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn rule(key: &str, command: &str, when: Option<&str>) -> KeybindingRule {
        static_rule(key, command, when)
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keybindings.json");
        (dir, path)
    }

    #[test]
    fn missing_file_is_created_with_the_defaults() {
        let (_dir, path) = fixture();
        let service = Keybindings::new(&path);
        service.sync_default_keybindings_on_startup().unwrap();
        let written: Vec<KeybindingRule> = serde_json::from_value(read_json(&path)).unwrap();
        assert_eq!(written, *DEFAULT_KEYBINDINGS);
        assert!(std::fs::read_to_string(&path).unwrap().ends_with("]\n"));
    }

    #[test]
    fn startup_backfills_a_missing_command_before_custom_rules() {
        let (_dir, path) = fixture();
        std::fs::write(&path, r#"[{"key":"mod+alt+x","command":"sidebar.toggle"}]"#).unwrap();
        let service = Keybindings::new(&path);
        service.sync_default_keybindings_on_startup().unwrap();
        let written: Vec<KeybindingRule> = serde_json::from_value(read_json(&path)).unwrap();
        // `sidebar.toggle` is already customised, so it is not backfilled.
        assert_eq!(written.len(), DEFAULT_KEYBINDINGS.len());
        // Defaults go first; the custom rule keeps priority as the last entry.
        assert_eq!(
            written.last().unwrap(),
            &rule("mod+alt+x", "sidebar.toggle", None)
        );
    }

    #[test]
    fn startup_skips_defaults_whose_shortcut_is_taken() {
        let (_dir, path) = fixture();
        std::fs::write(
            &path,
            r#"[{"key":"mod+b","command":"thread.copyReference"}]"#,
        )
        .unwrap();
        let service = Keybindings::new(&path);
        service.sync_default_keybindings_on_startup().unwrap();
        let written: Vec<KeybindingRule> = serde_json::from_value(read_json(&path)).unwrap();
        assert!(
            !written
                .iter()
                .any(|entry| entry.command.as_str() == "sidebar.toggle")
        );
        assert_eq!(
            written.last().unwrap().command.as_str(),
            "thread.copyReference"
        );
    }

    #[test]
    fn startup_leaves_the_file_alone_when_it_has_issues() {
        let (_dir, path) = fixture();
        let original = r#"[{"key":"mod+a"}]"#;
        std::fs::write(&path, original).unwrap();
        let service = Keybindings::new(&path);
        service.sync_default_keybindings_on_startup().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn snapshot_reports_malformed_and_invalid_entries_without_failing() {
        let (_dir, path) = fixture();
        std::fs::write(&path, "{}").unwrap();
        let service = Keybindings::new(&path);
        let state = service.snapshot().unwrap();
        assert!(matches!(
            state.issues.as_slice(),
            [ServerConfigIssue::MalformedConfig { .. }]
        ));

        std::fs::write(
            &path,
            r#"[{"key":"mod+a","command":"chat.new"},{"key":"mod+b++","command":"thread.settle"}]"#,
        )
        .unwrap();
        let service = Keybindings::new(&path);
        let state = service.snapshot().unwrap();
        assert!(matches!(
            state.issues.as_slice(),
            [ServerConfigIssue::InvalidEntry { index: 1, .. }]
        ));
        assert!(
            state
                .keybindings
                .iter()
                .any(|binding| binding.command.as_str() == "thread.pin")
        );
    }

    #[test]
    fn upsert_replaces_the_target_and_puts_the_new_rule_last() {
        let (_dir, path) = fixture();
        std::fs::write(
            &path,
            r#"[{"key":"mod+x","command":"thread.pin"},{"key":"mod+y","command":"thread.settle"}]"#,
        )
        .unwrap();
        let service = Keybindings::new(&path);
        let merged = service
            .upsert_keybinding_rule(UpsertKeybindingInput {
                rule: rule("mod+z", "thread.pin", None),
                replace: Some(rule("mod+x", "thread.pin", None)),
            })
            .unwrap();
        assert_eq!(merged.last().unwrap().command.as_str(), "thread.pin");
        let written: Vec<KeybindingRule> = serde_json::from_value(read_json(&path)).unwrap();
        assert_eq!(
            written,
            vec![
                rule("mod+y", "thread.settle", None),
                rule("mod+z", "thread.pin", None)
            ]
        );
    }

    #[test]
    fn upsert_caps_the_file_at_the_max_count_keeping_the_newest() {
        let (_dir, path) = fixture();
        let full: Vec<KeybindingRule> = (0..MAX_KEYBINDINGS_COUNT)
            .map(|index| {
                rule(
                    &format!("mod+f{}", index % 9),
                    "chat.find",
                    Some(&format!("ctx{index}")),
                )
            })
            .collect();
        std::fs::write(&path, to_json_string_pretty(&full).unwrap()).unwrap();
        let service = Keybindings::new(&path);
        service
            .upsert_keybinding_rule(UpsertKeybindingInput {
                rule: rule("mod+q", "thread.pin", None),
                replace: None,
            })
            .unwrap();
        let written: Vec<KeybindingRule> = serde_json::from_value(read_json(&path)).unwrap();
        assert_eq!(written.len(), MAX_KEYBINDINGS_COUNT);
        assert_eq!(written[0], full[1]);
        assert_eq!(written.last().unwrap().command.as_str(), "thread.pin");
    }

    #[test]
    fn remove_matches_key_command_and_when_exactly() {
        let (_dir, path) = fixture();
        std::fs::write(
            &path,
            r#"[{"key":"mod+x","command":"thread.pin","when":"terminalFocus"},{"key":"mod+x","command":"thread.pin"}]"#,
        )
        .unwrap();
        let service = Keybindings::new(&path);
        service
            .remove_keybinding_rule(&rule("mod+x", "thread.pin", None))
            .unwrap();
        let written: Vec<KeybindingRule> = serde_json::from_value(read_json(&path)).unwrap();
        assert_eq!(
            written,
            vec![rule("mod+x", "thread.pin", Some("terminalFocus"))]
        );
    }

    #[test]
    fn encodes_shortcuts_canonically() {
        let shortcut = parse_keybinding_shortcut("shift+mod+k").unwrap();
        assert_eq!(encode_shortcut(&shortcut).unwrap(), "mod+shift+k");
    }

    #[tokio::test]
    async fn watcher_reloads_and_emits_after_an_external_edit() {
        let (_dir, path) = fixture();
        let service = Keybindings::new(&path);
        let mut changes = service.subscribe();
        let _watcher = service.start().unwrap();

        std::fs::write(&path, r#"[{"key":"mod+x","command":"thread.pin"}]"#).unwrap();
        // Startup's own write also reports events; wait for the external edit.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let state = changes.recv().await.expect("channel open");
                if state.keybindings.iter().any(|binding| {
                    binding.command.as_str() == "thread.pin" && binding.shortcut.key.as_str() == "x"
                }) {
                    return;
                }
            }
        })
        .await
        .expect("a change event naming the external edit");
    }
}
