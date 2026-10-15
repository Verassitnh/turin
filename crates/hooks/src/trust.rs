//! Per-project trust for `<project>/.turin/` (`hooks.toml` and the scripts
//! next to it).
//!
//! New functionality (PORTING.md "Hooks (new)", Trust). Upstream never runs
//! commands taken from repo files (`t3.json` scripts are import-only), so a
//! project hooks config is inert until the user approves its exact contents.
//! Approvals live outside the repo in `~/.turin/trusted-hooks.json` (or under
//! `$TURIN_HOME`): a flat JSON object mapping the canonical project root to
//! the hex SHA-256 of the approved config tree: `hooks.toml` plus every other
//! file under `<project>/.turin/` (see [`hash_config_tree`]). Any edit to any
//! of them changes the hash and revokes the approval.

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
    /// The config tree cannot be approved as it stands (e.g. a symlink
    /// leaves `.turin/`); fix it first.
    #[error("project hooks cannot be trusted: {0}")]
    Untrustable(TreeError),
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

/// Most files / bytes a `.turin/` tree may hold to be approvable. Approval
/// hashes everything, so an accidental `node_modules` there is refused
/// instead of hashed on every dispatch.
pub const MAX_TREE_FILES: usize = 4096;
pub const MAX_TREE_BYTES: u64 = 32 * 1024 * 1024;

/// Why a `.turin/` tree cannot be hashed for approval.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TreeError {
    /// A symlink under `.turin/` resolves outside it (or does not resolve at
    /// all): its target is not covered by the approval.
    #[error("{} is a symlink that resolves outside .turin/ ({detail})", link.display())]
    SymlinkEscapes { link: PathBuf, detail: String },
    /// `.turin/` itself resolves outside the project root.
    #[error(".turin/ resolves to {}, outside the project", resolved.display())]
    ConfigDirOutsideProject { resolved: PathBuf },
    #[error("cannot read {}: {message}", path.display())]
    Unreadable { path: PathBuf, message: String },
    #[error(".turin/ has more than {MAX_TREE_FILES} files or {MAX_TREE_BYTES} bytes")]
    TooLarge,
}

/// The approvable fingerprint of a config tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeSnapshot {
    /// Hex SHA-256 over every file under the config dir.
    pub hash: String,
    /// The exact bytes of the config file that were hashed, so parsing can
    /// use what the hash describes.
    pub config_bytes: Vec<u8>,
}

/// Hashes everything under `config_dir` (`<project>/.turin`): sorted relative
/// paths plus file bytes, skipping nothing (scripts, `.DS_Store`, dotfiles).
/// Each entry is length-prefixed, so no two different trees collide on
/// concatenation. A symlink is hashed as its link text and the bytes it
/// resolves to, and must resolve inside `config_dir`; otherwise the project
/// hooks cannot be approved. Symlinked directories are not descended (their
/// real location is walked on its own).
///
/// `config_file` is the hooks file inside `config_dir`; its bytes come back
/// in the snapshot. `project_root`, when known, must contain `config_dir`.
pub fn hash_config_tree(
    config_dir: &Path,
    config_file: &Path,
    project_root: Option<&Path>,
) -> Result<TreeSnapshot, TreeError> {
    let unreadable = |path: &Path, err: std::io::Error| TreeError::Unreadable {
        path: path.to_path_buf(),
        message: err.to_string(),
    };
    let canonical_dir =
        std::fs::canonicalize(config_dir).map_err(|err| unreadable(config_dir, err))?;
    if let Some(root) = project_root
        && !canonical_dir.starts_with(root)
    {
        return Err(TreeError::ConfigDirOutsideProject {
            resolved: canonical_dir,
        });
    }

    // Collect (relative path, kind) first, then hash in sorted order.
    let mut entries: Vec<PathBuf> = Vec::new();
    let mut pending = vec![config_dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let read = std::fs::read_dir(&dir).map_err(|err| unreadable(&dir, err))?;
        for entry in read {
            let entry = entry.map_err(|err| unreadable(&dir, err))?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|err| unreadable(&path, err))?;
            if kind.is_dir() {
                pending.push(path);
            } else {
                entries.push(
                    path.strip_prefix(config_dir)
                        .expect("read_dir yields children")
                        .to_path_buf(),
                );
            }
            if entries.len() > MAX_TREE_FILES {
                return Err(TreeError::TooLarge);
            }
        }
    }
    entries.sort();

    let mut hasher = Sha256::new();
    hasher.update(b"turin-hooks-tree-v1\0");
    let config_rel = config_file
        .strip_prefix(config_dir)
        .unwrap_or(config_file)
        .to_path_buf();
    let mut config_bytes = None;
    let mut total: u64 = 0;
    for rel in &entries {
        let path = config_dir.join(rel);
        let meta = std::fs::symlink_metadata(&path).map_err(|err| unreadable(&path, err))?;
        let rel_bytes = rel.as_os_str().as_encoded_bytes();
        if meta.file_type().is_symlink() {
            let escapes = |detail: String| TreeError::SymlinkEscapes {
                link: rel.clone(),
                detail,
            };
            let resolved = std::fs::canonicalize(&path)
                .map_err(|err| escapes(format!("does not resolve: {err}")))?;
            if !resolved.starts_with(&canonical_dir) {
                return Err(escapes(format!("points to {}", resolved.display())));
            }
            let target = std::fs::read_link(&path).map_err(|err| unreadable(&path, err))?;
            let bytes = if resolved.is_file() {
                std::fs::read(&resolved).map_err(|err| unreadable(&path, err))?
            } else {
                Vec::new()
            };
            total += bytes.len() as u64;
            hash_entry(
                &mut hasher,
                b'L',
                rel_bytes,
                &[target.as_os_str().as_encoded_bytes(), &bytes],
            );
            if *rel == config_rel {
                config_bytes = Some(bytes);
            }
        } else if meta.file_type().is_file() {
            let bytes = std::fs::read(&path).map_err(|err| unreadable(&path, err))?;
            total += bytes.len() as u64;
            hash_entry(&mut hasher, b'F', rel_bytes, &[&bytes]);
            if *rel == config_rel {
                config_bytes = Some(bytes);
            }
        } else {
            // Sockets, fifos: not content we can approve.
            return Err(TreeError::Unreadable {
                path,
                message: "not a regular file".to_owned(),
            });
        }
        if total > MAX_TREE_BYTES {
            return Err(TreeError::TooLarge);
        }
    }
    let config_bytes = config_bytes.ok_or_else(|| TreeError::Unreadable {
        path: config_file.to_path_buf(),
        message: "hooks file is not inside the config directory".to_owned(),
    })?;
    Ok(TreeSnapshot {
        hash: format!("{:x}", hasher.finalize()),
        config_bytes,
    })
}

fn hash_entry(hasher: &mut Sha256, tag: u8, path: &[u8], parts: &[&[u8]]) {
    hasher.update([tag]);
    hasher.update((path.len() as u64).to_le_bytes());
    hasher.update(path);
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
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

    struct Tree {
        _dir: tempfile::TempDir,
        root: PathBuf,
        config_dir: PathBuf,
    }

    fn tree() -> Tree {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let config_dir = root.join(".turin");
        std::fs::create_dir_all(config_dir.join("hooks")).unwrap();
        std::fs::write(config_dir.join("hooks.toml"), "# config\n").unwrap();
        std::fs::write(config_dir.join("hooks/setup.sh"), "echo one\n").unwrap();
        Tree {
            _dir: dir,
            root,
            config_dir,
        }
    }

    fn tree_hash(tree: &Tree) -> Result<TreeSnapshot, TreeError> {
        hash_config_tree(
            &tree.config_dir,
            &tree.config_dir.join("hooks.toml"),
            Some(&tree.root),
        )
    }

    #[test]
    fn tree_hash_is_stable_and_returns_the_hashed_config_bytes() {
        let t = tree();
        let a = tree_hash(&t).unwrap();
        let b = tree_hash(&t).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.config_bytes, b"# config\n");
        assert_eq!(a.hash.len(), 64);
    }

    #[test]
    fn tree_hash_covers_every_file_under_turin() {
        let t = tree();
        let base = tree_hash(&t).unwrap().hash;
        let script = t.config_dir.join("hooks/setup.sh");

        std::fs::write(&script, "echo two\n").unwrap();
        let edited = tree_hash(&t).unwrap().hash;
        assert_ne!(edited, base, "script edit");

        std::fs::write(t.config_dir.join(".DS_Store"), "x").unwrap();
        let added = tree_hash(&t).unwrap().hash;
        assert_ne!(added, edited, "new file, even a dotfile");

        std::fs::rename(&script, t.config_dir.join("hooks/renamed.sh")).unwrap();
        let renamed = tree_hash(&t).unwrap().hash;
        assert_ne!(renamed, added, "rename");

        std::fs::remove_file(t.config_dir.join(".DS_Store")).unwrap();
        assert_ne!(tree_hash(&t).unwrap().hash, renamed, "removal");
    }

    #[test]
    fn tree_hash_framing_prevents_boundary_collisions() {
        let a = tree();
        let b = tree();
        for t in [&a, &b] {
            std::fs::remove_file(t.config_dir.join("hooks/setup.sh")).unwrap();
        }
        std::fs::write(a.config_dir.join("hooks/x"), "yz").unwrap();
        std::fs::write(b.config_dir.join("hooks/xy"), "z").unwrap();
        assert_ne!(tree_hash(&a).unwrap().hash, tree_hash(&b).unwrap().hash);
    }

    #[test]
    fn symlink_inside_turin_is_hashed_with_its_target() {
        let t = tree();
        std::os::unix::fs::symlink("hooks/setup.sh", t.config_dir.join("link.sh")).unwrap();
        let before = tree_hash(&t).unwrap().hash;
        std::fs::write(t.config_dir.join("hooks/setup.sh"), "changed").unwrap();
        assert_ne!(tree_hash(&t).unwrap().hash, before);
    }

    #[test]
    fn symlink_escaping_turin_is_rejected() {
        let t = tree();
        let outside = t.root.join("scripts");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("x.sh"), "echo x").unwrap();

        // File symlink inside the project but outside .turin/.
        let link = t.config_dir.join("hooks/x.sh");
        std::os::unix::fs::symlink(outside.join("x.sh"), &link).unwrap();
        match tree_hash(&t) {
            Err(TreeError::SymlinkEscapes { link, detail }) => {
                assert_eq!(link, Path::new("hooks/x.sh"));
                assert!(detail.contains("scripts"), "{detail}");
            }
            other => panic!("expected escape, got {other:?}"),
        }
        std::fs::remove_file(&link).unwrap();

        // Directory symlink (never descended, still rejected).
        std::os::unix::fs::symlink(&outside, t.config_dir.join("dir")).unwrap();
        assert!(matches!(
            tree_hash(&t),
            Err(TreeError::SymlinkEscapes { .. })
        ));
        std::fs::remove_file(t.config_dir.join("dir")).unwrap();

        // Dangling.
        std::os::unix::fs::symlink("nope", t.config_dir.join("dangling")).unwrap();
        assert!(matches!(
            tree_hash(&t),
            Err(TreeError::SymlinkEscapes { .. })
        ));
    }

    #[test]
    fn turin_dir_resolving_outside_the_project_is_rejected() {
        let t = tree();
        let other = tempfile::tempdir().unwrap();
        let other_root = std::fs::canonicalize(other.path()).unwrap();
        assert!(matches!(
            hash_config_tree(
                &t.config_dir,
                &t.config_dir.join("hooks.toml"),
                Some(&other_root)
            ),
            Err(TreeError::ConfigDirOutsideProject { .. })
        ));
    }
}
