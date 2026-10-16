//! Port of upstream/apps/desktop/src/settings/DesktopAppSettings.ts (window
//! geometry fields used by the GPUI shell).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::desktop_window::DesktopWindowBounds;

#[derive(Debug, thiserror::Error)]
pub enum DesktopAppSettingsError {
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

/// `DesktopEnvironment.desktopSettingsPath` (`<state>/desktop-settings.json`).
pub fn default_desktop_app_settings_path(home: &Path) -> PathBuf {
    home.join(".t3code").join("desktop-settings.json")
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopAppSettings {
    #[serde(default)]
    pub main_window_bounds: Option<DesktopWindowBounds>,
    #[serde(default)]
    pub main_window_maximized: bool,
}

pub fn read_desktop_app_settings(
    path: &Path,
) -> Result<DesktopAppSettings, DesktopAppSettingsError> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DesktopAppSettings::default());
        }
        Err(source) => {
            return Err(DesktopAppSettingsError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    serde_json::from_str(&raw).map_err(|source| DesktopAppSettingsError::Decode {
        path: path.to_path_buf(),
        source,
    })
}

pub fn write_desktop_app_settings(
    path: &Path,
    settings: &DesktopAppSettings,
) -> Result<(), DesktopAppSettingsError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| DesktopAppSettingsError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    }
    let encoded = serde_json::to_string_pretty(settings).map_err(|source| {
        DesktopAppSettingsError::Encode {
            path: path.to_path_buf(),
            source,
        }
    })?;
    fs::write(path, encoded).map_err(|source| DesktopAppSettingsError::Write {
        path: path.to_path_buf(),
        source,
    })
}
