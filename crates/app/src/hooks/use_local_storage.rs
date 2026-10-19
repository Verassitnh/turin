//! Port of the storage seam in upstream/apps/web/src/hooks/useLocalStorage.ts.
//!
//! The browser's `localStorage` becomes a [`LocalStorage`] trait so views
//! take the store as a dependency. [`MemoryLocalStorage`] is the in-memory
//! fallback; [`FileLocalStorage`] keeps the same key/value pairs in a JSON file
//! under the data directory (upstream keys are kept verbatim so an import can
//! find them).

// Ported whole (rule 3); the file-backed store (settings task) uses the rest.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;

/// `LocalStorageOperationError`: any failed read or write (private mode,
/// quota, blocked storage access).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Failed to {operation} local storage item {key}.")]
pub struct LocalStorageError {
    pub operation: &'static str,
    pub key: String,
}

pub trait LocalStorage: Send + Sync {
    fn get_item(&self, key: &str) -> Result<Option<String>, LocalStorageError>;
    fn set_item(&self, key: &str, value: &str) -> Result<(), LocalStorageError>;
    fn remove_item(&self, key: &str) -> Result<(), LocalStorageError>;
}

pub type SharedLocalStorage = Arc<dyn LocalStorage>;

/// upstream's `fallbackStorage`: a plain in-memory map.
#[derive(Default)]
pub struct MemoryLocalStorage {
    values: Mutex<HashMap<String, String>>,
}

impl MemoryLocalStorage {
    pub fn shared() -> SharedLocalStorage {
        Arc::new(Self::default())
    }
}

impl LocalStorage for MemoryLocalStorage {
    fn get_item(&self, key: &str) -> Result<Option<String>, LocalStorageError> {
        Ok(self.values.lock().get(key).cloned())
    }

    fn set_item(&self, key: &str, value: &str) -> Result<(), LocalStorageError> {
        self.values.lock().insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn remove_item(&self, key: &str) -> Result<(), LocalStorageError> {
        self.values.lock().remove(key);
        Ok(())
    }
}

/// A file-backed [`LocalStorage`]: one JSON object of string values, rewritten
/// atomically (temp file, then rename) on every change. An unreadable or
/// corrupt file reads as empty and is replaced by the next write, as a browser
/// treats cleared storage.
pub struct FileLocalStorage {
    path: PathBuf,
    values: Mutex<BTreeMap<String, String>>,
}

static OPEN_FILES: LazyLock<Mutex<HashMap<PathBuf, Arc<FileLocalStorage>>>> =
    LazyLock::new(Mutex::default);

impl FileLocalStorage {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let values = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<BTreeMap<String, String>>(&raw).ok())
            .unwrap_or_default();
        Self {
            path,
            values: Mutex::new(values),
        }
    }

    /// One shared instance per path, so every owner sees the same values.
    pub fn shared_at(path: &Path) -> SharedLocalStorage {
        let mut open = OPEN_FILES.lock();
        open.entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(Self::open(path)))
            .clone()
    }

    /// `<data dir>/<file_name>`; in-memory when there is no data directory.
    pub fn shared_in_data_dir(file_name: &str) -> SharedLocalStorage {
        match crate::app_runtime::data_dir::resolve_data_dir() {
            Some(dir) => Self::shared_at(&dir.join(file_name)),
            None => MemoryLocalStorage::shared(),
        }
    }

    fn write(
        &self,
        values: &BTreeMap<String, String>,
        operation: &'static str,
        key: &str,
    ) -> Result<(), LocalStorageError> {
        let fail = || LocalStorageError {
            operation,
            key: key.to_owned(),
        };
        let encoded = serde_json::to_vec(values).map_err(|_| fail())?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| fail())?;
        }
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, encoded).map_err(|_| fail())?;
        std::fs::rename(&temp, &self.path).map_err(|_| fail())
    }
}

impl LocalStorage for FileLocalStorage {
    fn get_item(&self, key: &str) -> Result<Option<String>, LocalStorageError> {
        Ok(self.values.lock().get(key).cloned())
    }

    fn set_item(&self, key: &str, value: &str) -> Result<(), LocalStorageError> {
        let mut values = self.values.lock();
        let previous = values.insert(key.to_owned(), value.to_owned());
        self.write(&values, "set", key)
            .inspect_err(|_| match previous {
                Some(previous) => {
                    values.insert(key.to_owned(), previous);
                }
                None => {
                    values.remove(key);
                }
            })
    }

    fn remove_item(&self, key: &str) -> Result<(), LocalStorageError> {
        let mut values = self.values.lock();
        if values.remove(key).is_none() {
            return Ok(());
        }
        self.write(&values, "remove", key)
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;

    #[test]
    fn values_survive_reopening_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("local-storage.json");
        let storage = FileLocalStorage::open(&path);
        storage.set_item("t3code:last-editor", "\"zed\"").unwrap();
        storage.set_item("other", "1").unwrap();
        storage.remove_item("other").unwrap();
        let reopened = FileLocalStorage::open(&path);
        assert_eq!(
            reopened.get_item("t3code:last-editor").unwrap().as_deref(),
            Some("\"zed\"")
        );
        assert_eq!(reopened.get_item("other").unwrap(), None);
    }

    #[test]
    fn a_corrupt_file_reads_as_empty_and_the_next_write_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local-storage.json");
        std::fs::write(&path, "{not json").unwrap();
        let storage = FileLocalStorage::open(&path);
        assert_eq!(storage.get_item("k").unwrap(), None);
        storage.set_item("k", "v").unwrap();
        assert_eq!(
            FileLocalStorage::open(&path)
                .get_item("k")
                .unwrap()
                .as_deref(),
            Some("v")
        );
    }

    #[test]
    fn a_failed_write_leaves_the_stored_value_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        // The parent "directory" is a file, so the write cannot succeed.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        let storage = FileLocalStorage::open(blocker.join("local-storage.json"));
        assert!(storage.set_item("k", "v").is_err());
        assert_eq!(storage.get_item("k").unwrap(), None);
    }

    #[test]
    fn shared_at_hands_every_caller_the_same_instance() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local-storage.json");
        let first = FileLocalStorage::shared_at(&path);
        let second = FileLocalStorage::shared_at(&path);
        first.set_item("k", "v").unwrap();
        assert_eq!(second.get_item("k").unwrap().as_deref(), Some("v"));
    }
}
