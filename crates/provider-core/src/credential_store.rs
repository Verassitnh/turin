//! Port of upstream/apps/server/src/provider/ProviderCredentialStore.ts
//! and the file-backed half of auth/ServerSecretStore.ts.
//!
//! The storage key is `provider-auth-` plus the SHA-256 hex of
//! `${namespace.length}:${namespace}${bindingId}`. That string is the
//! on-disk filename and the macOS keychain account, so a T3 secrets
//! directory still opens.
//!
//! The file store (`{secretsDir}/{key}.bin`, directory 0700, files 0600,
//! atomic rename) is the compatible backend. [`KeychainSecretStore`] is an
//! additional macOS backend that uses the same account string; it is not the
//! default, because upstream persists these bytes as files.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use thiserror::Error;
use turin_contracts::provider_setup::ProviderCredentialOwner;

use crate::{
    auth::ProviderCredentialBinding,
    errors::{ProviderCredentialError, ProviderCredentialOperation},
    failure::ProviderFailureCause,
};

/// Stable secret name for one provider binding. Must not change between releases.
pub fn provider_credential_key(namespace: &str, binding_id: &str) -> String {
    let material = format!("{}:{namespace}{binding_id}", namespace.len());
    let digest = Sha256::digest(material.as_bytes());
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    format!("provider-auth-{encoded}")
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SecretStoreError {
    #[error("Failed to secure {resource}.")]
    Secure { resource: String, detail: String },
    #[error("Failed to read {resource}.")]
    Read { resource: String, detail: String },
    #[error("Failed to persist {resource}.")]
    Persist { resource: String, detail: String },
    #[error("Failed to remove {resource}.")]
    Remove { resource: String, detail: String },
}

impl SecretStoreError {
    fn tag(&self) -> &'static str {
        match self {
            Self::Secure { .. } => "SecretStoreSecureError",
            Self::Read { .. } => "SecretStoreReadError",
            Self::Persist { .. } => "SecretStorePersistError",
            Self::Remove { .. } => "SecretStoreRemoveError",
        }
    }

    fn into_cause(self) -> ProviderFailureCause {
        let tag = self.tag();
        ProviderFailureCause::tagged(tag).with_message(self.to_string())
    }
}

#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, SecretStoreError>;
    async fn set(&self, name: &str, value: &[u8]) -> Result<(), SecretStoreError>;
    async fn remove(&self, name: &str) -> Result<(), SecretStoreError>;
}

#[derive(Default)]
pub struct MemorySecretStore {
    values: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SecretStore for MemorySecretStore {
    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        Ok(self.values.lock().get(name).cloned())
    }

    async fn set(&self, name: &str, value: &[u8]) -> Result<(), SecretStoreError> {
        self.values.lock().insert(name.to_owned(), value.to_vec());
        Ok(())
    }

    async fn remove(&self, name: &str) -> Result<(), SecretStoreError> {
        self.values.lock().remove(name);
        Ok(())
    }
}

/// `{secrets_dir}/{name}.bin`, mode 0600, written via a sibling temp file.
pub struct FileSecretStore {
    secrets_dir: PathBuf,
}

impl FileSecretStore {
    pub fn open(secrets_dir: impl Into<PathBuf>) -> Result<Self, SecretStoreError> {
        let secrets_dir = secrets_dir.into();
        std::fs::create_dir_all(&secrets_dir).map_err(|error| SecretStoreError::Secure {
            resource: format!("secrets directory {}", secrets_dir.display()),
            detail: error.to_string(),
        })?;
        set_mode(&secrets_dir, 0o700).map_err(|error| SecretStoreError::Secure {
            resource: format!("secrets directory {}", secrets_dir.display()),
            detail: error.to_string(),
        })?;
        Ok(Self { secrets_dir })
    }

    fn path_for(&self, name: &str) -> PathBuf {
        self.secrets_dir.join(format!("{name}.bin"))
    }
}

#[async_trait]
impl SecretStore for FileSecretStore {
    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        let path = self.path_for(name);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(SecretStoreError::Read {
                resource: format!("secret {name}"),
                detail: error.to_string(),
            }),
        }
    }

    async fn set(&self, name: &str, value: &[u8]) -> Result<(), SecretStoreError> {
        let path = self.path_for(name);
        let temp = PathBuf::from(format!("{}.{}.tmp", path.display(), uuid::Uuid::new_v4()));
        let write = (|| -> std::io::Result<()> {
            std::fs::write(&temp, value)?;
            set_mode(&temp, 0o600)?;
            std::fs::rename(&temp, &path)?;
            set_mode(&path, 0o600)?;
            Ok(())
        })();
        if let Err(error) = write {
            let _ = std::fs::remove_file(&temp);
            return Err(SecretStoreError::Persist {
                resource: format!("secret {name}"),
                detail: error.to_string(),
            });
        }
        Ok(())
    }

    async fn remove(&self, name: &str) -> Result<(), SecretStoreError> {
        let path = self.path_for(name);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(SecretStoreError::Remove {
                resource: format!("secret {name}"),
                detail: error.to_string(),
            }),
        }
    }
}

/// macOS keychain entries addressed by the same `provider-auth-…` account.
///
/// `security` may prompt for keychain access. The file store is the one
/// ProviderHost should use unless a host explicitly selects this backend.
pub struct KeychainSecretStore {
    service: String,
}

impl KeychainSecretStore {
    pub fn new() -> Self {
        Self {
            service: "turin-provider-auth".to_owned(),
        }
    }
}

impl Default for KeychainSecretStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SecretStore for KeychainSecretStore {
    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
        let output = security(&[
            "find-generic-password",
            "-s",
            &self.service,
            "-a",
            name,
            "-w",
        ])
        .await?;
        let Some(password) = output else {
            return Ok(None);
        };
        decode_hex(password.trim())
            .map(Some)
            .map_err(|detail| SecretStoreError::Read {
                resource: format!("secret {name}"),
                detail,
            })
    }

    async fn set(&self, name: &str, value: &[u8]) -> Result<(), SecretStoreError> {
        let hex = encode_hex(value);
        let status = security_status(&[
            "add-generic-password",
            "-U",
            "-s",
            &self.service,
            "-a",
            name,
            "-X",
            &hex,
        ])
        .await?;
        if !status.success() {
            return Err(SecretStoreError::Persist {
                resource: format!("secret {name}"),
                detail: status_detail(&status),
            });
        }
        Ok(())
    }

    async fn remove(&self, name: &str) -> Result<(), SecretStoreError> {
        let status =
            security_status(&["delete-generic-password", "-s", &self.service, "-a", name]).await?;
        if status.success() || status.code == Some(44) {
            return Ok(());
        }
        Err(SecretStoreError::Remove {
            resource: format!("secret {name}"),
            detail: status_detail(&status),
        })
    }
}

struct CommandStatus {
    code: Option<i32>,
    stderr: String,
}

impl CommandStatus {
    fn success(&self) -> bool {
        self.code == Some(0)
    }
}

fn status_detail(status: &CommandStatus) -> String {
    if status.stderr.is_empty() {
        format!("security exited with {:?}", status.code)
    } else {
        status.stderr.clone()
    }
}

async fn security(args: &[&str]) -> Result<Option<String>, SecretStoreError> {
    let status = security_status(args).await?;
    if status.code == Some(44) {
        return Ok(None);
    }
    if !status.success() {
        return Err(SecretStoreError::Read {
            resource: format!("secret {}", args.last().unwrap_or(&"")),
            detail: status_detail(&status),
        });
    }
    Ok(Some(status.stderr))
}

async fn security_status(args: &[&str]) -> Result<CommandStatus, SecretStoreError> {
    let output = tokio::process::Command::new("security")
        .args(args)
        .output()
        .await
        .map_err(|error| SecretStoreError::Secure {
            resource: "keychain".to_owned(),
            detail: error.to_string(),
        })?;
    // `find-generic-password -w` writes the secret to stdout.
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let body = if args.first() == Some(&"find-generic-password") {
        stdout
    } else {
        stderr
    };
    Ok(CommandStatus {
        code: output.status.code(),
        stderr: body,
    })
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn decode_hex(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err("keychain password is not hex".to_owned());
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    let chars: Vec<u8> = text.bytes().collect();
    for pair in chars.chunks(2) {
        let hi = hex_value(pair[0]).ok_or_else(|| "keychain password is not hex".to_owned())?;
        let lo = hex_value(pair[1]).ok_or_else(|| "keychain password is not hex".to_owned())?;
        bytes.push((hi << 4) | lo);
    }
    Ok(bytes)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// Opaque credentials for one binding. Only the provider decodes the bytes.
pub struct ProviderCredentials {
    pub binding: ProviderCredentialBinding,
    store: Arc<dyn SecretStore>,
}

impl ProviderCredentials {
    pub fn open(store: Arc<dyn SecretStore>, namespace: &str, binding_id: &str) -> Self {
        let key = provider_credential_key(namespace, binding_id);
        Self {
            binding: ProviderCredentialBinding {
                owner: ProviderCredentialOwner::T3,
                key,
            },
            store,
        }
    }

    pub async fn get(&self) -> Result<Option<Vec<u8>>, ProviderCredentialError> {
        self.store
            .get(&self.binding.key)
            .await
            .map_err(|error| credential_error(ProviderCredentialOperation::Get, error))
    }

    pub async fn set(&self, credentials: &[u8]) -> Result<(), ProviderCredentialError> {
        self.store
            .set(&self.binding.key, credentials)
            .await
            .map_err(|error| credential_error(ProviderCredentialOperation::Set, error))
    }

    pub async fn remove(&self) -> Result<(), ProviderCredentialError> {
        self.store
            .remove(&self.binding.key)
            .await
            .map_err(|error| credential_error(ProviderCredentialOperation::Remove, error))
    }
}

fn credential_error(
    operation: ProviderCredentialOperation,
    error: SecretStoreError,
) -> ProviderCredentialError {
    ProviderCredentialError {
        operation,
        cause: error.into_cause(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn isolates_bindings_and_keeps_the_upstream_key() {
        let store = Arc::new(MemorySecretStore::new());
        let a = ProviderCredentials::open(store.clone(), "cursor", "../../personal");
        let b = ProviderCredentials::open(store.clone(), "cursor", "work");
        let c = ProviderCredentials::open(store, "other", "../../personal");
        let bytes = vec![0, 255, 128, 1];
        a.set(&bytes).await.unwrap();
        assert_eq!(a.get().await.unwrap().as_deref(), Some(bytes.as_slice()));
        assert!(b.get().await.unwrap().is_none());
        assert!(c.get().await.unwrap().is_none());
        assert!(!a.binding.key.contains('/'));
        assert_eq!(
            a.binding.key,
            "provider-auth-db3cc9436c8272847fe0c152ba8b0d4695af90acd916f555649e17ba6efe6094"
        );
        let long = provider_credential_key(&"a".repeat(64), &"b".repeat(64));
        assert!(long.len() < 255);
        let delimiter = provider_credential_key("cur", "sor../../personal");
        assert_ne!(delimiter, a.binding.key);
        a.remove().await.unwrap();
        assert!(a.get().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn file_store_round_trips_bytes_and_ignores_a_missing_secret() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::open(dir.path()).unwrap();
        let name = provider_credential_key("cursor", "personal");
        assert!(store.get(&name).await.unwrap().is_none());
        store.set(&name, &[1, 2, 3]).await.unwrap();
        assert_eq!(
            store.get(&name).await.unwrap().as_deref(),
            Some(&[1, 2, 3][..])
        );
        store.remove(&name).await.unwrap();
        assert!(store.get(&name).await.unwrap().is_none());
        store.remove(&name).await.unwrap();
    }
}
