//! Port of upstream/apps/desktop/src/settings/DesktopAppSettings.ts (window
//! geometry fields used by the GPUI shell).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::desktop_window::{DesktopWindowBounds, normalize_main_window_bounds};

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

/// `~/.turin/userdata/desktop-settings.json` for a caller-supplied home.
/// Tests pass a temp directory and must not call [`desktop_settings_path`].
pub fn desktop_settings_path_for_home(home: &Path) -> PathBuf {
    let base = turin_runtime::config::resolve_base_dir(None, home)
        .unwrap_or_else(|_| home.join(turin_runtime::config::DEFAULT_BASE_DIR_NAME));
    turin_runtime::config::derive_server_paths(
        &base,
        None,
        turin_runtime::config::DeriveServerPathsOptions::default(),
    )
    .state_dir
    .join("desktop-settings.json")
}

/// `DesktopEnvironment.desktopSettingsPath` (`<state>/desktop-settings.json`).
#[allow(dead_code)]
pub fn default_desktop_app_settings_path(home: &Path) -> PathBuf {
    desktop_settings_path_for_home(home)
}

pub fn desktop_settings_path() -> PathBuf {
    match turin_runtime::config::home_directory() {
        Ok(home) => desktop_settings_path_for_home(&home),
        Err(_) => PathBuf::from(".turin/userdata/desktop-settings.json"),
    }
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
    let mut settings: DesktopAppSettings =
        serde_json::from_str(&raw).map_err(|source| DesktopAppSettingsError::Decode {
            path: path.to_path_buf(),
            source,
        })?;
    settings.main_window_bounds = settings
        .main_window_bounds
        .and_then(normalize_main_window_bounds);
    Ok(settings)
}

/// Merges geometry into the existing JSON document and replaces the file
/// with a temp write plus rename. Unknown keys are kept. A false maximized
/// flag is omitted, matching the upstream schema.
pub fn write_desktop_app_settings(
    path: &Path,
    settings: &DesktopAppSettings,
) -> Result<(), DesktopAppSettingsError> {
    let mut document = match fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str::<serde_json::Value>(&raw)
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new())),
        Err(_) => serde_json::Value::Object(serde_json::Map::new()),
    };
    if !document.is_object() {
        document = serde_json::Value::Object(serde_json::Map::new());
    }
    let object = document
        .as_object_mut()
        .expect("document was just forced to an object");
    match settings
        .main_window_bounds
        .and_then(normalize_main_window_bounds)
    {
        Some(bounds) => {
            let value =
                serde_json::to_value(bounds).map_err(|source| DesktopAppSettingsError::Encode {
                    path: path.to_path_buf(),
                    source,
                })?;
            object.insert("mainWindowBounds".to_owned(), value);
        }
        None => {
            object.remove("mainWindowBounds");
        }
    }
    if settings.main_window_maximized {
        object.insert(
            "mainWindowMaximized".to_owned(),
            serde_json::Value::Bool(true),
        );
    } else {
        object.remove("mainWindowMaximized");
    }
    let encoded = serde_json::to_string_pretty(&document).map_err(|source| {
        DesktopAppSettingsError::Encode {
            path: path.to_path_buf(),
            source,
        }
    })?;
    turin_shared::atomic_write::write_file_string_atomically(path, &encoded).map_err(|source| {
        DesktopAppSettingsError::Write {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(width: i32, height: i32) -> DesktopWindowBounds {
        DesktopWindowBounds {
            x: 40,
            y: 50,
            width,
            height,
        }
    }

    #[test]
    fn path_is_under_turin_userdata() {
        let home = std::env::temp_dir().join(format!(
            "turin-desktop-settings-path-{}",
            std::process::id()
        ));
        let path = desktop_settings_path_for_home(&home);
        assert_eq!(
            path,
            home.join(".turin")
                .join("userdata")
                .join("desktop-settings.json")
        );
        assert!(!path.components().any(|component| {
            component.as_os_str() == ".t3" || component.as_os_str() == ".t3code"
        }));
    }

    #[test]
    fn write_merges_bounds_and_rejects_sub_minimum_sizes() {
        let dir =
            std::env::temp_dir().join(format!("turin-desktop-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("desktop-settings.json");
        std::fs::write(&path, r#"{"theme":"kept","mainWindowMaximized":true}"#).unwrap();

        write_desktop_app_settings(
            &path,
            &DesktopAppSettings {
                main_window_bounds: Some(bounds(1100, 780)),
                main_window_maximized: true,
            },
        )
        .unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["theme"], "kept");
        assert_eq!(raw["mainWindowMaximized"], true);
        assert_eq!(raw["mainWindowBounds"]["width"], 1100);

        write_desktop_app_settings(
            &path,
            &DesktopAppSettings {
                main_window_bounds: Some(bounds(100, 100)),
                main_window_maximized: false,
            },
        )
        .unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(raw.get("mainWindowBounds").is_none());
        assert!(raw.get("mainWindowMaximized").is_none());
        assert_eq!(raw["theme"], "kept");

        let read = read_desktop_app_settings(&path).unwrap();
        assert_eq!(read.main_window_bounds, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
