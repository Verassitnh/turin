//! Desktop [`ClientSettingsPersistence`] for [`crate::settings::live_init`].

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::settings::ClientSettings;

use crate::desktop::desktop_client_settings::{
    DesktopClientSettingsError, quarantine_unreadable_client_settings, read_client_settings,
    write_client_settings,
};
use crate::settings_logic::use_settings::{
    ClientSettingsHydrationError, ClientSettingsPersistence,
};

#[derive(Clone)]
pub struct DesktopFileClientSettingsPersistence {
    path: PathBuf,
}

impl DesktopFileClientSettingsPersistence {
    pub fn new(path: PathBuf) -> Arc<Self> {
        Arc::new(Self { path })
    }
}

#[async_trait]
impl ClientSettingsPersistence for DesktopFileClientSettingsPersistence {
    async fn get_client_settings(
        &self,
    ) -> Result<Option<ClientSettings>, ClientSettingsHydrationError> {
        match read_client_settings(&self.path) {
            Ok(settings) => Ok(settings),
            // A file that is not valid settings would fail every read, and with
            // it every change the user makes. Keep it under another name and
            // start from the defaults, which the next change saves.
            Err(error @ DesktopClientSettingsError::Decode { .. }) => {
                match quarantine_unreadable_client_settings(&self.path) {
                    Ok(backup) => {
                        tracing::warn!(
                            error = %error,
                            backup = %backup.display(),
                            "client settings were unreadable; using the defaults"
                        );
                        Ok(None)
                    }
                    Err(rename_error) => Err(ClientSettingsHydrationError::new(format!(
                        "{error}; and it could not be moved aside: {rename_error}"
                    ))),
                }
            }
            Err(error) => Err(ClientSettingsHydrationError::new(error.to_string())),
        }
    }

    async fn set_client_settings(
        &self,
        settings: ClientSettings,
    ) -> Result<(), ClientSettingsHydrationError> {
        write_client_settings(&self.path, &settings)
            .map_err(|error| ClientSettingsHydrationError::new(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("turin-cs-persist-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir.join("client-settings.json")
    }

    #[tokio::test]
    async fn a_corrupt_file_is_set_aside_and_the_defaults_are_used() {
        for (name, content) in [
            ("json", "{ not json"),
            ("enum", r#"{"settings":{"confirmQuit":"bogus"}}"#),
        ] {
            let path = temp_path(name);
            std::fs::write(&path, content).expect("write");
            let persistence = DesktopFileClientSettingsPersistence::new(path.clone());
            assert_eq!(persistence.get_client_settings().await.expect("read"), None);
            assert!(!path.exists(), "{name}: the bad file is moved");
            let backups: Vec<_> = std::fs::read_dir(path.parent().expect("dir"))
                .expect("list")
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().contains(".corrupt-"))
                .collect();
            assert_eq!(backups.len(), 1, "{name}");
            // The next change saves over it and reads back.
            let settings = ClientSettings {
                confirm_quit: turin_contracts::settings::QuitConfirmationMode::Direct,
                ..ClientSettings::default()
            };
            persistence
                .set_client_settings(settings.clone())
                .await
                .expect("write");
            assert_eq!(
                persistence.get_client_settings().await.expect("read"),
                Some(settings)
            );
            let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
        }
    }
}
