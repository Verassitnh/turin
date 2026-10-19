//! Port of upstream/packages/provider-cursor/src/server/credentialStore.ts.
//!
//! The SDK owns the credential format; persistence uses the environment's
//! secret store. A legacy auth file is migrated once, then deleted.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::warn;
use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::provider_setup::ProviderSetupError;
use turin_provider_core::credential_store::ProviderCredentials;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorSdkCredentials {
    pub version: i32,
    pub backend_url: String,
    pub api_key: String,
    pub created_at_ms: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_expires_at_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

pub struct CursorSdkCredentialStore {
    credentials: ProviderCredentials,
    instance_id: ProviderInstanceId,
}

impl CursorSdkCredentialStore {
    pub async fn load(&self) -> Option<CursorSdkCredentials> {
        let bytes = self.credentials.get().await.ok().flatten()?;
        decode_credentials(std::str::from_utf8(&bytes).ok()?)
    }

    #[allow(clippy::result_large_err)]
    pub async fn save(&self, value: CursorSdkCredentials) -> Result<(), ProviderSetupError> {
        let encoded = encode_credentials(&value).map_err(|_| ProviderSetupError::SetupError {
            instance_id: self.instance_id.clone(),
            operation: "credentials".to_owned(),
            detail: "Cursor returned an unsupported credential format.".to_owned(),
            cause: None,
        })?;
        self.credentials
            .set(encoded.as_bytes())
            .await
            .map_err(|error| setup_error(&self.instance_id, error.to_string()))?;
        Ok(())
    }

    #[allow(clippy::result_large_err)]
    pub async fn clear(&self) -> Result<(), ProviderSetupError> {
        self.credentials
            .remove()
            .await
            .map_err(|error| setup_error(&self.instance_id, error.to_string()))
    }
}

pub struct CursorCredentialStoreBundle {
    pub store: CursorSdkCredentialStore,
    pub binding: turin_provider_core::auth::ProviderCredentialBinding,
}

pub async fn make_cursor_credential_store(
    credentials: ProviderCredentials,
    instance_id: ProviderInstanceId,
    legacy_file: Option<PathBuf>,
) -> CursorCredentialStoreBundle {
    if let Some(legacy_file) = legacy_file {
        migrate_legacy_file(&credentials, &legacy_file).await;
    }
    let binding = credentials.binding.clone();
    CursorCredentialStoreBundle {
        store: CursorSdkCredentialStore {
            credentials,
            instance_id,
        },
        binding,
    }
}

async fn migrate_legacy_file(credentials: &ProviderCredentials, legacy_file: &Path) {
    if let Err(error) = migrate_legacy_file_inner(credentials, legacy_file).await {
        warn!(?error, "Could not migrate legacy Cursor credentials");
    }
}

async fn migrate_legacy_file_inner(
    credentials: &ProviderCredentials,
    legacy_file: &Path,
) -> Result<(), std::io::Error> {
    let legacy = match tokio::fs::read_to_string(legacy_file).await {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let stored = credentials.get().await.ok().flatten();
    let stored_is_valid = stored
        .as_deref()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .and_then(decode_credentials)
        .is_some();
    if !stored_is_valid
        && decode_credentials(&legacy).is_some()
        && let Err(error) = credentials.set(legacy.as_bytes()).await
    {
        return Err(std::io::Error::other(error.to_string()));
    }
    tokio::fs::remove_file(legacy_file).await
}

fn decode_credentials(text: &str) -> Option<CursorSdkCredentials> {
    let parsed: CursorSdkCredentials = serde_json::from_str(text).ok()?;
    if parsed.version != 1 {
        return None;
    }
    Some(parsed)
}

fn encode_credentials(value: &CursorSdkCredentials) -> Result<String, serde_json::Error> {
    if value.version != 1 {
        return Err(serde_json::Error::io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "unsupported credential version",
        )));
    }
    serde_json::to_string(value)
}

fn setup_error(instance_id: &ProviderInstanceId, detail: String) -> ProviderSetupError {
    ProviderSetupError::SetupError {
        instance_id: instance_id.clone(),
        operation: "credentials".to_owned(),
        detail,
        cause: None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use turin_provider_core::credential_store::{MemorySecretStore, SecretStore};

    const LEGACY: &str = r#"{
  "version": 1,
  "backendUrl": "https://api.cursor.com",
  "apiKey": "test-only-legacy-key",
  "createdAtMs": 100
}"#;

    async fn bundle(
        store: Arc<dyn SecretStore>,
        instance: &str,
        legacy: Option<&str>,
        stored: Option<&str>,
    ) -> (CursorCredentialStoreBundle, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let legacy_path = legacy.map(|contents| {
            let path = dir.path().join("cursor.json");
            std::fs::write(&path, contents).unwrap();
            path
        });
        let credentials = ProviderCredentials::open(store, "cursor", instance);
        if let Some(stored) = stored {
            credentials.set(stored.as_bytes()).await.unwrap();
        }
        let bundle = make_cursor_credential_store(
            credentials,
            ProviderInstanceId::new(instance).expect("valid id"),
            legacy_path,
        )
        .await;
        (bundle, dir)
    }

    #[tokio::test]
    async fn imports_a_legacy_sign_in_and_deletes_the_legacy_file() {
        let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let (bundle, dir) = bundle(store, "personal", Some(LEGACY), None).await;
        assert_eq!(
            bundle
                .store
                .load()
                .await
                .as_ref()
                .map(|c| c.api_key.as_str()),
            Some("test-only-legacy-key")
        );
        assert!(!dir.path().join("cursor.json").exists());
    }

    #[tokio::test]
    async fn keeps_a_newer_stored_sign_in() {
        let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let stored = LEGACY.replace("test-only-legacy-key", "test-only-current-key");
        let (bundle, dir) = bundle(store, "personal", Some(LEGACY), Some(&stored)).await;
        assert_eq!(
            bundle
                .store
                .load()
                .await
                .as_ref()
                .map(|c| c.api_key.as_str()),
            Some("test-only-current-key")
        );
        assert!(!dir.path().join("cursor.json").exists());
    }

    #[tokio::test]
    async fn replaces_a_damaged_stored_sign_in() {
        let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let (bundle, dir) = bundle(store, "personal", Some(LEGACY), Some("damaged")).await;
        assert_eq!(
            bundle
                .store
                .load()
                .await
                .as_ref()
                .map(|c| c.api_key.as_str()),
            Some("test-only-legacy-key")
        );
        assert!(!dir.path().join("cursor.json").exists());
    }

    #[tokio::test]
    async fn discards_a_damaged_legacy_file() {
        let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let (bundle, dir) = bundle(store, "personal", Some("damaged"), None).await;
        assert!(bundle.store.load().await.is_none());
        assert!(!dir.path().join("cursor.json").exists());
    }

    #[tokio::test]
    async fn restores_sdk_credentials_in_a_new_controller_and_keeps_another_account_when_signing_out()
     {
        let store: Arc<dyn SecretStore> = Arc::new(MemorySecretStore::new());
        let credentials = CursorSdkCredentials {
            version: 1,
            backend_url: "https://api.cursor.com".to_owned(),
            api_key: "test-only-key".to_owned(),
            created_at_ms: 100.0,
            api_key_expires_at_ms: Some(1000.0),
            email: Some("test@example.com".to_owned()),
        };
        let personal = make_cursor_credential_store(
            ProviderCredentials::open(Arc::clone(&store), "cursor", "personal"),
            ProviderInstanceId::new("personal").unwrap(),
            None,
        )
        .await;
        let work = make_cursor_credential_store(
            ProviderCredentials::open(Arc::clone(&store), "cursor", "work"),
            ProviderInstanceId::new("work").unwrap(),
            None,
        )
        .await;
        personal.store.save(credentials.clone()).await.unwrap();
        work.store
            .save(CursorSdkCredentials {
                api_key: "test-only-work-key".to_owned(),
                ..credentials.clone()
            })
            .await
            .unwrap();
        let restored = make_cursor_credential_store(
            ProviderCredentials::open(Arc::clone(&store), "cursor", "personal"),
            ProviderInstanceId::new("personal").unwrap(),
            None,
        )
        .await;
        assert_eq!(restored.store.load().await, Some(credentials.clone()));
        ProviderCredentials::open(Arc::clone(&store), "cursor", "personal")
            .set(b"damaged credential")
            .await
            .unwrap();
        assert!(restored.store.load().await.is_none());
        restored.store.save(credentials.clone()).await.unwrap();
        restored.store.clear().await.unwrap();
        assert!(personal.store.load().await.is_none());
        assert_eq!(
            work.store.load().await.as_ref().map(|c| c.api_key.as_str()),
            Some("test-only-work-key")
        );
    }
}
