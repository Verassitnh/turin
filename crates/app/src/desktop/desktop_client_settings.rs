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

/// Default desktop client settings path (`DesktopEnvironment` + `settings.json`).
pub fn default_client_settings_path(home: &Path) -> PathBuf {
    home.join(".t3code").join("settings.json")
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
    fs::write(path, encoded).map_err(|source| DesktopClientSettingsError::Write {
        path: path.to_path_buf(),
        source,
    })
}
