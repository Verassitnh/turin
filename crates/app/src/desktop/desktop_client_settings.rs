#![allow(dead_code)]

//! Port of upstream/apps/desktop/src/settings/DesktopClientSettings.ts (local
//! JSON read/write for `ClientSettings`; Effect services are inlined).

use std::fs;
use std::path::{Path, PathBuf};

use turin_contracts::settings::ClientSettings;

#[derive(Debug, thiserror::Error)]
pub enum DesktopClientSettingsError {
    #[error("read failed at {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("decode failed at {path}: {source}")]
    Decode {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("encode failed at {path}: {source}")]
    Encode {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("write failed at {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// `~/.turin/userdata/client-settings.json` for a caller-supplied home.
/// Tests pass a temp directory and must not resolve the real home directory.
pub fn client_settings_path_for_home(home: &Path) -> PathBuf {
    let base = turin_runtime::config::resolve_base_dir(None, home)
        .unwrap_or_else(|_| home.join(turin_runtime::config::DEFAULT_BASE_DIR_NAME));
    turin_runtime::config::derive_server_paths(
        &base,
        None,
        turin_runtime::config::DeriveServerPathsOptions::default(),
    )
    .state_dir
    .join("client-settings.json")
}

/// Live preferences share the database's data directory, including dev overrides.
pub fn client_settings_path() -> PathBuf {
    crate::app_runtime::data_dir::resolve_data_dir()
        .map(|dir| dir.join("client-settings.json"))
        .unwrap_or_else(|| client_settings_path_for_home(Path::new("")))
}

/// `DesktopEnvironment` client settings (`<state>/client-settings.json`).
pub fn default_client_settings_path(home: &Path) -> PathBuf {
    client_settings_path_for_home(home)
}

/// Reads settings, accepting either `{ "settings": … }` or a bare document.
pub fn read_client_settings(
    path: &Path,
) -> Result<Option<ClientSettings>, DesktopClientSettingsError> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(DesktopClientSettingsError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|source| DesktopClientSettingsError::Decode {
            path: path.to_path_buf(),
            source,
        })?;
    let settings_value = value.get("settings").cloned().unwrap_or(value);
    let settings: ClientSettings = serde_json::from_value(settings_value).map_err(|source| {
        DesktopClientSettingsError::Decode {
            path: path.to_path_buf(),
            source,
        }
    })?;
    Ok(Some(settings))
}

/// Moves a settings file that cannot be decoded aside, as
/// `client-settings.json.corrupt-<unix ms>`, so the defaults can be used and
/// saved over it without losing what the user had. Returns where it went.
pub fn quarantine_unreadable_client_settings(path: &Path) -> std::io::Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".corrupt-{stamp}"));
    let backup = path.with_file_name(name);
    fs::rename(path, &backup)?;
    Ok(backup)
}

pub fn write_client_settings(
    path: &Path,
    settings: &ClientSettings,
) -> Result<(), DesktopClientSettingsError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| DesktopClientSettingsError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    }
    let document = serde_json::json!({ "settings": settings });
    let encoded = serde_json::to_string_pretty(&document).map_err(|source| {
        DesktopClientSettingsError::Encode {
            path: path.to_path_buf(),
            source,
        }
    })?;
    // Temp file + rename (through symlinks), so a crash mid-write cannot leave truncated JSON.
    turin_shared::atomic_write::write_file_string_atomically(path, &encoded).map_err(|source| {
        DesktopClientSettingsError::Write {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::settings::QuitConfirmationMode;

    #[test]
    fn confirm_quit_reads_turin_client_settings() {
        let home =
            std::env::temp_dir().join(format!("turin-client-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let path = client_settings_path_for_home(&home);
        assert!(path.ends_with(".turin/userdata/client-settings.json"));
        write_client_settings(
            &path,
            &ClientSettings {
                confirm_quit: QuitConfirmationMode::Direct,
                ..ClientSettings::default()
            },
        )
        .unwrap();
        let read = read_client_settings(&path).unwrap().unwrap();
        assert_eq!(read.confirm_quit, QuitConfirmationMode::Direct);
        let changed = r#"{"settings":{"confirmQuit":"double-click"}}"#;
        std::fs::write(&path, changed).unwrap();
        let read = read_client_settings(&path).unwrap().unwrap();
        assert_eq!(read.confirm_quit, QuitConfirmationMode::DoubleClick);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn an_unreadable_file_is_moved_aside_whole() {
        let dir =
            std::env::temp_dir().join(format!("turin-client-quarantine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("client-settings.json");
        std::fs::write(&path, "{ not json").unwrap();
        let backup = quarantine_unreadable_client_settings(&path).unwrap();
        assert!(!path.exists());
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "{ not json");
        assert!(
            backup
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("client-settings.json.corrupt-")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
