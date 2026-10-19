//! The persistence half of upstream/apps/web/src/uiStateStore.ts: the UI state
//! (manual project order, expansion, visit markers) read from and written to a
//! [`LocalStorage`] under upstream's `t3code:ui-state:v1` key.
//!
//! The pure model and its sanitizers are in [`super::ui_state_store`]. Upstream
//! debounces its `localStorage` write; changes here are rare (a drag-reorder),
//! so each one writes at once. A missing, unreadable or corrupt value loads as
//! the initial state.

use crate::hooks::use_local_storage::{LocalStorageError, SharedLocalStorage};
use crate::state::ui_state_store::{
    PersistedUiState, UiState, parse_persisted_state, persisted_state_json, reorder_projects,
};

/// `PERSISTED_STATE_KEY`.
pub const UI_STATE_KEY: &str = "t3code:ui-state:v1";

pub struct UiStateFile {
    storage: SharedLocalStorage,
    state: UiState,
}

impl UiStateFile {
    /// `hydrateUiState`.
    pub fn load(storage: SharedLocalStorage) -> Self {
        let state = storage
            .get_item(UI_STATE_KEY)
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<PersistedUiState>(&raw).ok())
            .map(|persisted| parse_persisted_state(&persisted))
            .unwrap_or_default();
        Self { storage, state }
    }

    pub fn state(&self) -> &UiState {
        &self.state
    }

    /// The manual project order the sidebar feeds to `useSidebarProjectGroups`.
    pub fn project_order(&self) -> &[String] {
        &self.state.project_order
    }

    /// `reorderProjects`, then `persistState` when the order changed. Returns
    /// whether it did.
    pub fn reorder_projects(
        &mut self,
        current_project_order: &[String],
        dragged_project_ids: &[String],
        target_project_ids: &[String],
    ) -> Result<bool, LocalStorageError> {
        let changed = reorder_projects(
            &mut self.state,
            current_project_order,
            dragged_project_ids,
            target_project_ids,
        );
        if changed {
            self.persist()?;
        }
        Ok(changed)
    }

    /// `persistState`.
    pub fn persist(&self) -> Result<(), LocalStorageError> {
        self.storage
            .set_item(UI_STATE_KEY, &persisted_state_json(&self.state).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::use_local_storage::{FileLocalStorage, MemoryLocalStorage};

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn nothing_stored_loads_the_initial_state() {
        let file = UiStateFile::load(MemoryLocalStorage::shared());
        assert!(file.project_order().is_empty());
    }

    #[test]
    fn a_reorder_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local-storage.json");
        let mut file = UiStateFile::load(std::sync::Arc::new(FileLocalStorage::open(&path)));
        let current = ids(&["a", "b", "c"]);
        assert!(
            file.reorder_projects(&current, &ids(&["c"]), &ids(&["a"]))
                .unwrap()
        );
        assert_eq!(file.project_order(), ids(&["c", "a", "b"]));

        // A new process: nothing shared with the first but the file.
        let reopened = UiStateFile::load(std::sync::Arc::new(FileLocalStorage::open(&path)));
        assert_eq!(reopened.project_order(), ids(&["c", "a", "b"]));
    }

    #[test]
    fn an_unchanged_order_writes_nothing() {
        let storage = MemoryLocalStorage::shared();
        let mut file = UiStateFile::load(storage.clone());
        let current = ids(&["a", "b"]);
        assert!(
            !file
                .reorder_projects(&current, &ids(&["a"]), &ids(&["a"]))
                .unwrap()
        );
        assert_eq!(storage.get_item(UI_STATE_KEY).unwrap(), None);
    }

    #[test]
    fn a_corrupt_value_loads_as_the_initial_state() {
        let storage = MemoryLocalStorage::shared();
        storage.set_item(UI_STATE_KEY, "{broken").unwrap();
        assert!(UiStateFile::load(storage).project_order().is_empty());
    }
}
