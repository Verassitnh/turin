//! Snapshot and restore of the Cursor CLI files a parameterized session rewrites.
//!
//! `session/new`, `session/load`, and `session/set_config_option` can change
//! `~/.cursor/acp-config.json` (`selectedModelVariantId`) and the model keys
//! in `~/.cursor/cli-config.json`. The snapshot keeps only those keys. Other
//! keys, including `permissions.allow`, are left as the CLI wrote them.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::{CursorError, CursorOperation};

const ACP_CONFIG: &str = "acp-config.json";
const CLI_CONFIG: &str = "cli-config.json";
const VARIANT_KEY: &str = "selectedModelVariantId";
const CLI_KEYS: &[&str] = &["model", "selectedModel", "modelSelectionHistory"];

#[derive(Debug, Clone, PartialEq)]
enum FileSnap {
    Absent,
    /// Not JSON. Restoring writes these bytes back.
    Raw(Vec<u8>),
    Json(Map<String, Value>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CursorUserConfigSnapshot {
    acp: FileSnap,
    cli: FileSnap,
}

pub fn default_cursor_home() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".cursor"))
}

pub fn snapshot_user_config(cursor_home: &Path) -> Result<CursorUserConfigSnapshot, CursorError> {
    Ok(CursorUserConfigSnapshot {
        acp: read_snap(&cursor_home.join(ACP_CONFIG))?,
        cli: read_snap(&cursor_home.join(CLI_CONFIG))?,
    })
}

pub fn restore_user_config(
    cursor_home: &Path,
    snapshot: &CursorUserConfigSnapshot,
) -> Result<(), CursorError> {
    restore_file(&cursor_home.join(ACP_CONFIG), &snapshot.acp, &[VARIANT_KEY])?;
    restore_file(&cursor_home.join(CLI_CONFIG), &snapshot.cli, CLI_KEYS)?;
    Ok(())
}

/// Runs `operation` with the Cursor user files restored afterwards.
///
/// File IO runs on the blocking pool. The restore also runs if `operation` is
/// dropped mid-await: [`RestoreOnDrop`] restores synchronously, so a cancelled
/// turn does not leave model edits in `~/.cursor`.
pub async fn with_user_config<T, F, Fut>(
    cursor_home: Option<&Path>,
    operation: F,
) -> Result<T, CursorError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, CursorError>>,
{
    let Some(cursor_home) = cursor_home else {
        return operation().await;
    };
    let home = cursor_home.to_path_buf();
    let snapshot = blocking({
        let home = home.clone();
        move || snapshot_user_config(&home)
    })
    .await?;
    let mut guard = RestoreOnDrop {
        home: home.clone(),
        snapshot: Some(snapshot),
    };
    let result = operation().await;
    let snapshot = guard
        .snapshot
        .take()
        .expect("snapshot is armed until restore");
    let restored = blocking(move || restore_user_config(&home, &snapshot)).await;
    match (result, restored) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

/// Restores the snapshot when dropped while still armed.
struct RestoreOnDrop {
    home: PathBuf,
    snapshot: Option<CursorUserConfigSnapshot>,
}

impl Drop for RestoreOnDrop {
    fn drop(&mut self) {
        if let Some(snapshot) = self.snapshot.take() {
            let _ = restore_user_config(&self.home, &snapshot);
        }
    }
}

async fn blocking<T, F>(work: F) -> Result<T, CursorError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, CursorError> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| CursorError::new(CursorOperation::Config, "Cursor config task stopped."))?
}

fn read_snap(path: &Path) -> Result<FileSnap, CursorError> {
    match fs::read(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(FileSnap::Absent),
        Err(error) => Err(io_error(error)),
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(object)) => Ok(FileSnap::Json(object)),
            _ => Ok(FileSnap::Raw(bytes)),
        },
    }
}

fn restore_file(path: &Path, snapshot: &FileSnap, keys: &[&str]) -> Result<(), CursorError> {
    match snapshot {
        FileSnap::Absent => remove_keys_or_file(path, keys),
        FileSnap::Raw(bytes) => write_bytes(path, bytes),
        FileSnap::Json(original) => merge_keys(path, original, keys),
    }
}

fn remove_keys_or_file(path: &Path, keys: &[&str]) -> Result<(), CursorError> {
    let Ok(bytes) = fs::read(path) else {
        return Ok(());
    };
    let Ok(Value::Object(mut object)) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(());
    };
    for key in keys {
        object.remove(*key);
    }
    if object.is_empty() {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error(error)),
        }
    } else {
        write_json(path, &Value::Object(object))
    }
}

fn merge_keys(
    path: &Path,
    original: &Map<String, Value>,
    keys: &[&str],
) -> Result<(), CursorError> {
    // Start from the snapshot so a full rewrite cannot drop theme or other
    // unrelated keys. Non-model keys the CLI still has, including edits, win.
    let current = match fs::read(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(error) => return Err(io_error(error)),
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(object)) => object,
            _ => Map::new(),
        },
    };
    let mut merged = original.clone();
    for (key, value) in current {
        if !keys.contains(&key.as_str()) {
            merged.insert(key, value);
        }
    }
    for key in keys {
        match original.get(*key) {
            Some(value) => {
                merged.insert((*key).to_owned(), value.clone());
            }
            None => {
                merged.remove(*key);
            }
        }
    }
    write_json(path, &Value::Object(merged))
}

fn write_json(path: &Path, value: &Value) -> Result<(), CursorError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        CursorError::new(
            CursorOperation::Config,
            format!("Could not encode Cursor config: {error}"),
        )
    })?;
    write_bytes(path, &bytes)
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), CursorError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    fs::write(path, bytes).map_err(io_error)
}

fn io_error(error: std::io::Error) -> CursorError {
    CursorError::new(
        CursorOperation::Config,
        "Could not read or restore Cursor config.",
    )
    .with_hidden(error)
}

impl CursorError {
    fn with_hidden(self, _error: impl std::fmt::Display) -> Self {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn restores_only_the_model_keys_the_cli_rewrites() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        fs::write(
            home.join(ACP_CONFIG),
            serde_json::to_vec_pretty(&json!({
                "selectedModelVariantId": "claude-opus-5-5[context=300k,effort=medium,fast=false]",
                "theme": "dark"
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            home.join(CLI_CONFIG),
            serde_json::to_vec_pretty(&json!({
                "model": "composer-2",
                "selectedModel": "composer-2",
                "modelSelectionHistory": ["composer-2"],
                "permissions": {"allow": ["Shell(ls)"]}
            }))
            .unwrap(),
        )
        .unwrap();
        let observed = with_user_config(Some(home), || async {
            fs::write(home.join(ACP_CONFIG), b"{}").unwrap();
            fs::write(
                home.join(CLI_CONFIG),
                serde_json::to_vec(&json!({
                    "model": "gpt-5.5",
                    "selectedModel": "gpt-5.5",
                    "modelSelectionHistory": ["gpt-5.5"],
                    "permissions": {"allow": ["Shell(ls)", "Write(/tmp)"]}
                }))
                .unwrap(),
            )
            .unwrap();
            Ok("opened".to_owned())
        })
        .await
        .unwrap();
        assert_eq!(observed, "opened");
        let acp: Value = serde_json::from_slice(&fs::read(home.join(ACP_CONFIG)).unwrap()).unwrap();
        assert_eq!(
            acp["selectedModelVariantId"],
            "claude-opus-5-5[context=300k,effort=medium,fast=false]"
        );
        assert_eq!(acp["theme"], "dark");
        let cli: Value = serde_json::from_slice(&fs::read(home.join(CLI_CONFIG)).unwrap()).unwrap();
        assert_eq!(cli["model"], "composer-2");
        assert_eq!(cli["selectedModel"], "composer-2");
        assert_eq!(cli["permissions"]["allow"][1], "Write(/tmp)");
    }

    #[tokio::test]
    async fn removes_keys_a_session_created_in_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        with_user_config(Some(home), || async {
            fs::create_dir_all(home).unwrap();
            fs::write(
                home.join(ACP_CONFIG),
                serde_json::to_vec(&json!({"selectedModelVariantId": "variant"})).unwrap(),
            )
            .unwrap();
            fs::write(
                home.join(CLI_CONFIG),
                serde_json::to_vec(&json!({"model": "gpt-5.5", "permissions": {"allow": []}}))
                    .unwrap(),
            )
            .unwrap();
            Ok(())
        })
        .await
        .unwrap();
        assert!(!home.join(ACP_CONFIG).exists());
        let cli: Value = serde_json::from_slice(&fs::read(home.join(CLI_CONFIG)).unwrap()).unwrap();
        assert!(cli.get("model").is_none());
        assert!(cli.get("permissions").is_some());
    }

    #[tokio::test]
    async fn restores_when_the_operation_is_dropped_mid_await() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        fs::write(
            home.join(CLI_CONFIG),
            json!({"model": "composer-2"}).to_string(),
        )
        .unwrap();
        let (written_tx, written_rx) = tokio::sync::oneshot::channel();
        let op_home = home.clone();
        let turn = with_user_config(Some(&home), move || async move {
            fs::write(
                op_home.join(CLI_CONFIG),
                json!({"model": "gpt-5.5"}).to_string(),
            )
            .unwrap();
            let _ = written_tx.send(());
            std::future::pending::<Result<(), CursorError>>().await
        });
        // The turn is cancelled after the CLI rewrote its config and before it returned.
        tokio::select! {
            _ = turn => unreachable!("the operation never completes"),
            _ = written_rx => {}
        }
        let cli: Value = serde_json::from_slice(&fs::read(home.join(CLI_CONFIG)).unwrap()).unwrap();
        assert_eq!(cli["model"], "composer-2");
    }
}
