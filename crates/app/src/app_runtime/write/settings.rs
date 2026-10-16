//! Settings startup and live bridge to the provider host.
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
/// Default title-generation model used when seeding a new desktop data directory.
pub const DEFAULT_TEXT_GENERATION_MODEL: &str = "claude-haiku-5-5";

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
    // Preserve the desktop seed preference. Existing explicit selections survive;
    // the persistent service owns all settings after startup.
    if document.get("textGenerationModelSelection").is_none() {
        document["textGenerationModelSelection"] =
            json!({"instanceId": "claudeAgent", "model": DEFAULT_TEXT_GENERATION_MODEL});
    }
    serde_json::from_value(document).unwrap_or_else(|error| {
        tracing::warn!(%error, "invalid {SETTINGS_FILE}; using default settings");
        ServerSettings::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_generation_instance(settings: &ServerSettings) -> String {
        settings
            .text_generation_model_selection
            .instance_id
            .to_string()
    }

    #[test]
    fn missing_file_gives_defaults_with_claude_text_generation() {
        let dir = tempfile::tempdir().expect("dir");
        let settings = load_server_settings(dir.path(), None);
        assert_eq!(text_generation_instance(&settings), "claudeAgent");
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
        let settings = load_server_settings(dir.path(), None);
        assert_eq!(text_generation_instance(&settings), "claudeAgent");
    }

    #[test]
    fn an_explicit_text_generation_model_is_kept() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            r#"{"textGenerationModelSelection": {"instanceId": "codex", "model": "gpt-5.4-mini"}}"#,
        )
        .expect("write");
        let settings = load_server_settings(dir.path(), None);
        assert_eq!(text_generation_instance(&settings), "codex");
    }
}

/// The persistent settings owner. Seed a new data directory using the supplied
/// startup configuration; existing documents are loaded by the real service,
/// including secret materialization and legacy migrations.
pub async fn open_settings(
    data_dir: &Path,
    db: turin_persistence::Db,
    initial: &ServerSettings,
) -> anyhow::Result<(
    std::sync::Arc<turin_runtime::server_settings::service::ServerSettingsService>,
    Option<turin_runtime::server_settings::service::SettingsWatcher>,
)> {
    use anyhow::Context as _;
    use std::sync::Arc;
    use turin_provider_core::credential_store::{FileSecretStore, SecretStore};
    use turin_runtime::server_settings::service::ServerSettingsService;
    let path = data_dir.join(SETTINGS_FILE);
    let exists = tokio::fs::try_exists(&path)
        .await
        .context("checking settings file")?;
    let secrets_path = data_dir.join("userdata/secrets");
    let secrets: Arc<dyn SecretStore> = Arc::new(
        tokio::task::spawn_blocking(move || FileSecretStore::open(secrets_path))
            .await
            .context("opening settings secrets task")?
            .context("opening settings secrets")?,
    );
    let service = ServerSettingsService::new(path, secrets, db);
    let watcher = service.start().await.context("starting settings service")?;
    if !exists {
        let patch = serde_json::from_value(serde_json::to_value(initial)?)?;
        service
            .update_settings(&patch)
            .await
            .context("saving initial settings")?;
    }
    Ok((service, watcher))
}

/// Development override is process-local and must survive settings reloads.
pub fn effective_settings(mut settings: ServerSettings) -> ServerSettings {
    if let Ok(binary) = std::env::var(CLAUDE_BINARY_ENV) {
        if !binary.trim().is_empty() {
            let id = turin_contracts::provider_instance::ProviderInstanceId::new("claudeAgent")
                .expect("static instance");
            let entry = settings.provider_instances.entry(id).or_insert_with(|| {
                serde_json::from_value(json!({"driver":"claudeAgent", "enabled":true, "config":{}}))
                    .expect("static config")
            });
            entry.config = {
                let mut config = entry.config.clone().unwrap_or_else(|| json!({}));
                config["binaryPath"] = json!(binary);
                Some(config)
            };
        }
    }
    settings
}
