//! Server settings for the provider host, until the settings slice (R3 logic,
//! S1 UI) lands a writer.
//!
//! `<data dir>/settings.json` holds a `ServerSettings` document (upstream's
//! file, same shape) when present; a missing or unreadable file gives the
//! defaults. `TURIN_CLAUDE_BINARY` overrides the Claude binary of the default
//! `claudeAgent` instance; it is the hook the fake-CLI smoke test uses.

use std::path::Path;

use serde_json::{Value, json};
use turin_contracts::settings::ServerSettings;

pub const SETTINGS_FILE: &str = "settings.json";
pub const CLAUDE_BINARY_ENV: &str = "TURIN_CLAUDE_BINARY";

pub fn load_server_settings(data_dir: &Path, claude_binary: Option<&str>) -> ServerSettings {
    let mut document = match std::fs::read_to_string(data_dir.join(SETTINGS_FILE)) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(value) if value.is_object() => value,
            Ok(_) | Err(_) => {
                tracing::warn!("{SETTINGS_FILE} is not a JSON object; using default settings");
                json!({})
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(error) => {
            tracing::warn!(%error, "cannot read {SETTINGS_FILE}; using default settings");
            json!({})
        }
    };
    if let Some(binary) = claude_binary.filter(|binary| !binary.trim().is_empty()) {
        document["providerInstances"]["claudeAgent"] = json!({
            "driver": "claudeAgent",
            "enabled": true,
            "config": { "binaryPath": binary },
        });
    }
    serde_json::from_value(document).unwrap_or_else(|error| {
        tracing::warn!(%error, "invalid {SETTINGS_FILE}; using default settings");
        ServerSettings::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().expect("dir");
        assert_eq!(
            load_server_settings(dir.path(), None),
            ServerSettings::default()
        );
    }

    #[test]
    fn the_claude_binary_override_replaces_the_default_instance() {
        let dir = tempfile::tempdir().expect("dir");
        let settings = load_server_settings(dir.path(), Some("/tmp/fake-claude"));
        let text = serde_json::to_string(&settings).expect("encodes");
        assert!(text.contains("/tmp/fake-claude"), "{text}");
    }

    #[test]
    fn an_invalid_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join(SETTINGS_FILE), "{ not json").expect("write");
        assert_eq!(
            load_server_settings(dir.path(), None),
            ServerSettings::default()
        );
    }
}
