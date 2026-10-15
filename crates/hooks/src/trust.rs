//! Per-project trust for `<project>/.turin/hooks.toml`.
//!
//! New functionality (PORTING.md "Hooks (new)", Trust). Upstream never runs
//! commands taken from repo files (`t3.json` scripts are import-only), so a
//! project hooks file is inert until the user approves its exact contents.
//! Approvals live outside the repo in `~/.turin/trusted-hooks.json` (or under
//! `$TURIN_HOME`): a flat JSON object mapping the canonical project root to
//! the hex SHA-256 of the approved file's bytes. Any edit changes the hash and
//! revokes the approval.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// File name of the trust store inside the Turin home directory.
pub const TRUST_FILE_NAME: &str = "trusted-hooks.json";

/// `$TURIN_HOME/trusted-hooks.json` when `TURIN_HOME` is set, else
/// `~/.turin/trusted-hooks.json`; `None` when no home directory is known.
pub fn default_trust_store_path() -> Option<PathBuf> {
    crate::config::default_turin_home().map(|home| home.join(TRUST_FILE_NAME))
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Why an approval could not be recorded.
#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    /// There is no project hooks file (any more) to approve.
    #[error("no project hooks file at {0}")]
    NoProjectHooks(PathBuf),
    /// The runner has no project root or no trust store location.
    #[error("trust store unavailable: no project root or home directory")]
    Unavailable,
    /// The file changed after the hash was shown to the user.
    #[error("project hooks changed since review (reviewed {expected}, found {actual})")]
    HashMismatch { expected: String, actual: String },
    #[error("cannot update trust store {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Reads and writes the trust file. Stateless: every call goes to disk, so a
/// reload always sees the latest approvals.
#[derive(Debug, Clone)]
pub struct TrustStore {
    path: PathBuf,
}

impl TrustStore {
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    /// The approved hash for `canonical_root`. A missing file means nothing
    /// is approved; an unreadable or malformed file fails closed the same
    /// way.
    pub fn lookup(&self, canonical_root: &Path) -> Option<String> {
        self.read().remove(&root_key(canonical_root))
    }

    /// Records `hash` as the approved contents for `canonical_root`.
    pub fn approve(&self, canonical_root: &Path, hash: &str) -> Result<(), TrustError> {
        let mut entries = self.read();
        entries.insert(root_key(canonical_root), hash.to_owned());
        self.write(&entries)
    }

    /// Removes any approval for `canonical_root`.
    pub fn revoke(&self, canonical_root: &Path) -> Result<(), TrustError> {
        let mut entries = self.read();
        if entries.remove(&root_key(canonical_root)).is_some() {
            self.write(&entries)?;
        }
        Ok(())
    }

    fn read(&self) -> BTreeMap<String, String> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
            Err(err) => {
                tracing::warn!(%err, path = %self.path.display(), "hooks trust store unreadable; treating every project as untrusted");
                return BTreeMap::new();
            }
        };
        match serde_json::from_slice(&bytes) {
            Ok(entries) => entries,
            Err(err) => {
                tracing::warn!(%err, path = %self.path.display(), "hooks trust store malformed; treating every project as untrusted");
                BTreeMap::new()
            }
        }
    }

    /// Atomic replace (temp file + rename) so a crash never leaves a
    /// half-written trust file.
    fn write(&self, entries: &BTreeMap<String, String>) -> Result<(), TrustError> {
        let io_err = |source| TrustError::Io {
            path: self.path.clone(),
            source,
        };
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(entries).expect("string map serializes");
        std::fs::write(&tmp, body).map_err(io_err)?;
        std::fs::rename(&tmp, &self.path).map_err(io_err)
    }
}

fn root_key(root: &Path) -> String {
    root.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn approve_lookup_revoke_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = TrustStore::new(&dir.path().join("nested").join(TRUST_FILE_NAME));
        let root = Path::new("/projects/a");
        assert_eq!(store.lookup(root), None);
        store.approve(root, "h1").unwrap();
        store.approve(Path::new("/projects/b"), "h2").unwrap();
        assert_eq!(store.lookup(root).as_deref(), Some("h1"));
        store.approve(root, "h3").unwrap();
        assert_eq!(store.lookup(root).as_deref(), Some("h3"));
        store.revoke(root).unwrap();
        assert_eq!(store.lookup(root), None);
        assert_eq!(
            store.lookup(Path::new("/projects/b")).as_deref(),
            Some("h2")
        );
    }

    #[test]
    fn file_is_a_flat_root_to_hash_map() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TRUST_FILE_NAME);
        TrustStore::new(&path)
            .approve(Path::new("/p"), "abc")
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value, serde_json::json!({"/p": "abc"}));
    }

    #[test]
    fn malformed_store_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TRUST_FILE_NAME);
        std::fs::write(&path, "{ not json").unwrap();
        let store = TrustStore::new(&path);
        assert_eq!(store.lookup(Path::new("/p")), None);
        // Approving over a corrupt file repairs it.
        store.approve(Path::new("/p"), "abc").unwrap();
        assert_eq!(store.lookup(Path::new("/p")).as_deref(), Some("abc"));
    }
}
