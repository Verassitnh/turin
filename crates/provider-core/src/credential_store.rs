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
    time::Duration,
};

use async_trait::async_trait;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
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
    #[error("{resource} already exists.")]
    AlreadyExists { resource: String },
    #[error("Failed to read {resource} after it was created concurrently.")]
    ConcurrentRead { resource: String },
}

impl SecretStoreError {
    fn tag(&self) -> &'static str {
        match self {
            Self::Secure { .. } => "SecretStoreSecureError",
            Self::Read { .. } => "SecretStoreReadError",
            Self::Persist { .. } => "SecretStorePersistError",
            Self::Remove { .. } => "SecretStoreRemoveError",
            Self::AlreadyExists { .. } => "SecretStoreAlreadyExistsError",
            Self::ConcurrentRead { .. } => "SecretStoreConcurrentReadError",
        }
    }

    fn detail(&self) -> &str {
        match self {
            Self::Secure { detail, .. }
            | Self::Read { detail, .. }
            | Self::Persist { detail, .. }
            | Self::Remove { detail, .. } => detail,
            Self::AlreadyExists { .. } | Self::ConcurrentRead { .. } => "",
        }
    }

    fn into_cause(self) -> ProviderFailureCause {
        let tag = self.tag();
        let detail = self.detail().trim();
        let message = if detail.is_empty() {
            self.to_string()
        } else {
            format!("{self} {detail}")
        };
        ProviderFailureCause::tagged(tag).with_message(message)
    }
}

#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, SecretStoreError>;
    async fn set(&self, name: &str, value: &[u8]) -> Result<(), SecretStoreError>;
    async fn remove(&self, name: &str) -> Result<(), SecretStoreError>;

    /// Upstream `ServerSecretStore.create`: writes `value` only when `name` is
    /// absent. The default is check-then-set and is not atomic. Backends that
    /// can create exclusively override it.
    async fn exclusive_create(&self, name: &str, value: &[u8]) -> Result<(), SecretStoreError> {
        if self.get(name).await?.is_some() {
            return Err(SecretStoreError::AlreadyExists {
                resource: format!("secret {name}"),
            });
        }
        self.set(name, value).await
    }

    /// Upstream `getOrCreateRandom`: the stored bytes, or `bytes` fresh random
    /// bytes created exclusively. When another writer wins the race, that
    /// writer's value is returned.
    async fn get_or_create_random(
        &self,
        name: &str,
        bytes: usize,
    ) -> Result<Vec<u8>, SecretStoreError> {
        if let Some(existing) = self.get(name).await? {
            return Ok(existing);
        }
        let mut generated = vec![0u8; bytes];
        getrandom::fill(&mut generated).map_err(|error| SecretStoreError::Secure {
            resource: format!("secret {name}"),
            detail: error.to_string(),
        })?;
        match self.exclusive_create(name, &generated).await {
            Ok(()) => Ok(generated),
            Err(SecretStoreError::AlreadyExists { .. }) => match self.get(name).await? {
                Some(existing) => Ok(existing),
                None => Err(SecretStoreError::ConcurrentRead {
                    resource: format!("secret {name}"),
                }),
            },
            Err(error) => Err(error),
        }
    }
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

    async fn exclusive_create(&self, name: &str, value: &[u8]) -> Result<(), SecretStoreError> {
        let mut values = self.values.lock();
        if values.contains_key(name) {
            return Err(SecretStoreError::AlreadyExists {
                resource: format!("secret {name}"),
            });
        }
        values.insert(name.to_owned(), value.to_vec());
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
        match tokio::fs::read(&path).await {
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
        let write = async {
            // Flushed before the rename: the legacy migration removes its source
            // after this write, so a power loss must not leave an empty secret.
            let mut file = tokio::fs::File::create(&temp).await?;
            tokio::io::AsyncWriteExt::write_all(&mut file, value).await?;
            file.sync_all().await?;
            drop(file);
            set_mode_async(&temp, 0o600).await?;
            tokio::fs::rename(&temp, &path).await?;
            sync_parent_directory(&path).await;
            set_mode_async(&path, 0o600).await
        }
        .await;
        if let Err(error) = write {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(SecretStoreError::Persist {
                resource: format!("secret {name}"),
                detail: error.to_string(),
            });
        }
        Ok(())
    }

    async fn exclusive_create(&self, name: &str, value: &[u8]) -> Result<(), SecretStoreError> {
        let path = self.path_for(name);
        match create_new_file(&path, value) {
            Ok(()) => set_mode(&path, 0o600).map_err(|error| SecretStoreError::Persist {
                resource: format!("secret {name}"),
                detail: error.to_string(),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(SecretStoreError::AlreadyExists {
                    resource: format!("secret {name}"),
                })
            }
            Err(error) => Err(SecretStoreError::Persist {
                resource: format!("secret {name}"),
                detail: error.to_string(),
            }),
        }
    }

    async fn remove(&self, name: &str) -> Result<(), SecretStoreError> {
        let path = self.path_for(name);
        match tokio::fs::remove_file(&path).await {
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
        let status = security_status(
            &[
                "find-generic-password",
                "-s",
                &self.service,
                "-a",
                name,
                "-w",
            ],
            None,
        )
        .await?;
        if status.code == Some(SECURITY_ITEM_NOT_FOUND) {
            return Ok(None);
        }
        if !status.success() {
            return Err(SecretStoreError::Read {
                resource: format!("secret {name}"),
                detail: status.detail(),
            });
        }
        // `find-generic-password -w` writes the secret to stdout.
        decode_hex(status.stdout.trim())
            .map(Some)
            .map_err(|detail| SecretStoreError::Read {
                resource: format!("secret {name}"),
                detail,
            })
    }

    async fn set(&self, name: &str, value: &[u8]) -> Result<(), SecretStoreError> {
        // The hex value goes down stdin, never argv, so `ps` cannot show it.
        let command = keychain_add_command(&self.service, name, &encode_hex(value));
        let status = security_status(SECURITY_INTERACTIVE_ARGS, Some(command.as_bytes())).await?;
        if !status.success() {
            return Err(SecretStoreError::Persist {
                resource: format!("secret {name}"),
                detail: status.detail(),
            });
        }
        Ok(())
    }

    async fn remove(&self, name: &str) -> Result<(), SecretStoreError> {
        let status = security_status(
            &["delete-generic-password", "-s", &self.service, "-a", name],
            None,
        )
        .await?;
        if status.success() || status.code == Some(SECURITY_ITEM_NOT_FOUND) {
            return Ok(());
        }
        Err(SecretStoreError::Remove {
            resource: format!("secret {name}"),
            detail: status.detail(),
        })
    }
}

/// `errSecItemNotFound` as reported by the `security` CLI's exit status.
const SECURITY_ITEM_NOT_FOUND: i32 = 44;

/// `security -i` reads commands from stdin until EOF.
const SECURITY_INTERACTIVE_ARGS: &[&str] = &["-i"];

/// Bounds a `security` call. It may block on a keychain access prompt.
const SECURITY_TIMEOUT: Duration = Duration::from_secs(30);

/// The `add-generic-password` line `security -i` runs. Service and account
/// names are restricted to `[A-Za-z0-9_-]`, so they need no quoting.
fn keychain_add_command(service: &str, name: &str, hex: &str) -> String {
    format!("add-generic-password -U -s {service} -a {name} -X {hex}\n")
}

struct CommandStatus {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl CommandStatus {
    fn success(&self) -> bool {
        self.code == Some(0)
    }

    fn detail(&self) -> String {
        let stderr = self.stderr.trim();
        if stderr.is_empty() {
            format!("security exited with {:?}", self.code)
        } else {
            stderr.to_owned()
        }
    }
}

async fn security_status(
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<CommandStatus, SecretStoreError> {
    run_command("security", args, stdin, SECURITY_TIMEOUT).await
}

/// Runs `program` and collects its output. The child is `kill_on_drop`, so
/// when the timeout drops the call the process is killed rather than left
/// behind.
async fn run_command(
    program: &str,
    args: &[&str],
    stdin: Option<&[u8]>,
    timeout: Duration,
) -> Result<CommandStatus, SecretStoreError> {
    let secure = |error: std::io::Error| SecretStoreError::Secure {
        resource: "keychain".to_owned(),
        detail: error.to_string(),
    };
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .stdin(if stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(secure)?;
    let run = async {
        if let (Some(mut pipe), Some(input)) = (child.stdin.take(), stdin) {
            // Dropping `pipe` at the end of this block sends EOF.
            pipe.write_all(input).await?;
        }
        child.wait_with_output().await
    };
    let output = tokio::time::timeout(timeout, run)
        .await
        .map_err(|_| SecretStoreError::Secure {
            resource: "keychain".to_owned(),
            detail: format!("{program} did not finish within {timeout:?}"),
        })?
        .map_err(secure)?;
    Ok(CommandStatus {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
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

/// Creates `path` only if it does not exist, with mode 0600 on unix. A failed
/// write removes the partial file, so a retry is not blocked by it.
fn create_new_file(path: &Path, value: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let written = file.write_all(value).and_then(|()| file.sync_all());
    if let Err(error) = written {
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}

/// Sync because `open` is a constructor, not an async method.
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

async fn set_mode_async(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// Flushes the directory that holds `path`, so the rename that just put the
/// file there survives a power loss. Unix only, best effort: a failure leaves
/// the secret written and readable, so it does not fail the set.
async fn sync_parent_directory(path: &Path) {
    #[cfg(unix)]
    {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if let Ok(directory) = tokio::fs::File::open(parent).await {
            let _ = directory.sync_all().await;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
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

    /// A store that implements only the required methods, so the trait defaults run.
    struct PlainStore(MemorySecretStore);

    #[async_trait]
    impl SecretStore for PlainStore {
        async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, SecretStoreError> {
            self.0.get(name).await
        }
        async fn set(&self, name: &str, value: &[u8]) -> Result<(), SecretStoreError> {
            self.0.set(name, value).await
        }
        async fn remove(&self, name: &str) -> Result<(), SecretStoreError> {
            self.0.remove(name).await
        }
    }

    #[tokio::test]
    async fn exclusive_create_refuses_to_overwrite_on_every_backend() {
        let dir = tempfile::tempdir().unwrap();
        let file = FileSecretStore::open(dir.path()).unwrap();
        let memory = MemorySecretStore::new();
        let plain = PlainStore(MemorySecretStore::new());
        for store in [&file as &dyn SecretStore, &memory, &plain] {
            store.exclusive_create("n", b"first").await.unwrap();
            let error = store.exclusive_create("n", b"second").await.unwrap_err();
            assert!(matches!(error, SecretStoreError::AlreadyExists { .. }));
            assert_eq!(
                store.get("n").await.unwrap().as_deref(),
                Some(&b"first"[..])
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exclusive_create_writes_owner_only_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::open(dir.path()).unwrap();
        store.exclusive_create("n", b"x").await.unwrap();
        let mode = std::fs::metadata(store.path_for("n"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[tokio::test]
    async fn get_or_create_random_creates_once_and_then_returns_the_stored_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::open(dir.path()).unwrap();
        let first = store.get_or_create_random("key", 32).await.unwrap();
        assert_eq!(first.len(), 32);
        assert_eq!(store.get("key").await.unwrap(), Some(first.clone()));
        let second = store.get_or_create_random("key", 32).await.unwrap();
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn get_or_create_random_uses_the_default_path_for_plain_stores() {
        let store = PlainStore(MemorySecretStore::new());
        let first = store.get_or_create_random("key", 16).await.unwrap();
        assert_eq!(first.len(), 16);
        assert_eq!(store.get_or_create_random("key", 16).await.unwrap(), first);
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

    #[cfg(unix)]
    #[tokio::test]
    async fn file_store_keeps_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join("secrets");
        let store = FileSecretStore::open(&secrets).unwrap();
        store.set("entry", &[1, 2]).await.unwrap();

        let mode = |path: PathBuf| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(secrets.join("entry.bin")), 0o600);
        assert_eq!(mode(secrets), 0o700);
    }

    #[test]
    fn failure_cause_carries_the_store_detail() {
        let cause = SecretStoreError::Read {
            resource: "secret provider-auth-x".to_owned(),
            detail: "User canceled.\n".to_owned(),
        }
        .into_cause();
        assert_eq!(
            cause,
            ProviderFailureCause::tagged("SecretStoreReadError")
                .with_message("Failed to read secret provider-auth-x. User canceled.")
        );
    }

    #[test]
    fn keychain_failure_detail_uses_stderr() {
        let status = CommandStatus {
            code: Some(1),
            stdout: "not the secret".to_owned(),
            stderr: "security: item not found\n".to_owned(),
        };
        assert_eq!(status.detail(), "security: item not found");
    }

    #[test]
    fn keychain_set_puts_the_secret_on_stdin_not_argv() {
        let hex = encode_hex(&[0xde, 0xad, 0xbe, 0xef]);
        let command = keychain_add_command("turin-provider-auth", "provider-auth-abc", &hex);
        assert_eq!(
            command,
            "add-generic-password -U -s turin-provider-auth -a provider-auth-abc -X deadbeef\n"
        );
        // The only argv `set` uses is the interactive flag.
        assert_eq!(SECURITY_INTERACTIVE_ARGS, &["-i"]);
    }

    #[tokio::test]
    async fn stdin_reaches_the_child_and_closes_at_the_end() {
        // `cat` exits only once it sees EOF, so this also proves the pipe closes.
        let status = run_command(
            "/bin/cat",
            &[],
            Some(&b"payload"[..]),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert!(status.success());
        assert_eq!(status.stdout, "payload");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_timed_out_security_call_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let pid_arg = pid_file.to_str().unwrap();
        let result = run_command(
            "/bin/sh",
            &["-c", "echo $$ > \"$0\"; exec sleep 600", pid_arg],
            None,
            Duration::from_secs(1),
        )
        .await;
        let Err(SecretStoreError::Secure { detail, .. }) = result else {
            panic!("expected a timeout error");
        };
        assert!(detail.contains("did not finish"), "{detail}");

        let pid: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        crate::process_group::assert_process_exits(pid, "timed-out security child").await;
    }
}
