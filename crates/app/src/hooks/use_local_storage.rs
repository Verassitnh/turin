//! Port of the storage seam in upstream/apps/web/src/hooks/useLocalStorage.ts.
//!
//! The browser's `localStorage` becomes a [`LocalStorage`] trait so views
//! take the store as a dependency. Turin has no persisted web storage yet:
//! [`MemoryLocalStorage`] stands in until the settings task wires a file-backed
//! store (upstream keys are kept verbatim so a later import can find them).

// Ported whole (rule 3); the file-backed store (settings task) uses the rest.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

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
