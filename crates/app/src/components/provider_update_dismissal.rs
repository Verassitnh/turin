//! Port of upstream/apps/web/src/providerUpdateDismissal.ts
#![allow(dead_code)]
//!
//! Persisted dismissal keys for provider-update toasts. File I/O is a T20 seam;
//! callers pass a [`ProviderUpdateDismissalsStore`].

use indexmap::IndexSet;
use serde::{Deserialize, Serialize};
use turin_contracts::base_schemas::js_trim;

pub const PROVIDER_UPDATE_DISMISSALS_STORAGE_KEY: &str = "t3code:provider-update-dismissals:v1";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUpdateDismissals {
    pub keys: Vec<String>,
}

impl ProviderUpdateDismissals {
    pub fn key_set(&self) -> IndexSet<String> {
        self.keys.iter().cloned().collect()
    }
}

/// Read/write seam for persisted dismissals (upstream `useLocalStorage`).
pub trait ProviderUpdateDismissalsStore {
    fn read(&self) -> ProviderUpdateDismissals;
    fn write(&mut self, document: ProviderUpdateDismissals);
}

pub fn is_provider_update_notification_dismissed(
    store: &dyn ProviderUpdateDismissalsStore,
    dismissal_key: Option<&str>,
) -> bool {
    let Some(dismissal_key) = dismissal_key.filter(|key| !key.is_empty()) else {
        return false;
    };
    store.read().keys.iter().any(|key| key == dismissal_key)
}

pub fn dismiss_provider_update_notification(
    store: &mut dyn ProviderUpdateDismissalsStore,
    dismissal_key: Option<&str>,
) {
    let Some(trimmed_key) = dismissal_key.map(js_trim).filter(|key| !key.is_empty()) else {
        return;
    };
    let mut document = store.read();
    if document.keys.iter().any(|key| key == trimmed_key) {
        return;
    }
    document.keys.push(trimmed_key.to_owned());
    store.write(document);
}

/// In-memory store for tests and unwired UI.
#[derive(Clone, Debug, Default)]
pub struct InMemoryProviderUpdateDismissalsStore {
    document: ProviderUpdateDismissals,
}

impl ProviderUpdateDismissalsStore for InMemoryProviderUpdateDismissalsStore {
    fn read(&self) -> ProviderUpdateDismissals {
        self.document.clone()
    }

    fn write(&mut self, document: ProviderUpdateDismissals) {
        self.document = document;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_dismissed_keys_without_duplicates() {
        let mut store = InMemoryProviderUpdateDismissalsStore::default();
        assert!(!is_provider_update_notification_dismissed(
            &store,
            Some("a")
        ));
        dismiss_provider_update_notification(&mut store, Some(" a "));
        assert!(is_provider_update_notification_dismissed(&store, Some("a")));
        dismiss_provider_update_notification(&mut store, Some("a"));
        assert_eq!(store.read().keys, vec!["a".to_owned()]);
    }

    #[test]
    fn ignores_empty_keys() {
        let mut store = InMemoryProviderUpdateDismissalsStore::default();
        dismiss_provider_update_notification(&mut store, None);
        dismiss_provider_update_notification(&mut store, Some("   "));
        assert!(store.read().keys.is_empty());
    }
}
