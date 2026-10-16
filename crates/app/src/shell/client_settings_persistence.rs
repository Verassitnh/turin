//! Desktop [`ClientSettingsPersistence`] for [`crate::settings::live_init`].

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::settings::ClientSettings;

use crate::desktop::desktop_client_settings::{read_client_settings, write_client_settings};
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
        read_client_settings(&self.path)
            .map_err(|error| ClientSettingsHydrationError::new(error.to_string()))
    }

    async fn set_client_settings(
        &self,
        settings: ClientSettings,
    ) -> Result<(), ClientSettingsHydrationError> {
        write_client_settings(&self.path, &settings)
            .map_err(|error| ClientSettingsHydrationError::new(error.to_string()))
    }
}
