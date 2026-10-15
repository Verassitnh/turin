//! Port of upstream/apps/web/src/diffPanelStore.ts (+ test).
//!
//! The zustand store is a plain struct; persistence (`persist` to
//! localStorage under `t3code:diff-panel-state:v1`) belongs to the settings
//! task, so the struct derives serde with the upstream field names and the
//! owner can read/write it in one call.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// `ScopedThreadRef`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ScopedThreadRef {
    pub environment_id: String,
    pub thread_id: String,
}

impl ScopedThreadRef {
    pub fn new(environment_id: impl Into<String>, thread_id: impl Into<String>) -> Self {
        Self {
            environment_id: environment_id.into(),
            thread_id: thread_id.into(),
        }
    }

    /// `scopedThreadKey`.
    pub fn key(&self) -> String {
        format!("{}:{}", self.environment_id, self.thread_id)
    }
}

/// `DiffPanelSelection`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DiffPanelSelection {
    /// The Changes view: everything this checkout changed since its base.
    #[serde(rename = "branch", rename_all = "camelCase")]
    Branch { base_ref: Option<String> },
    #[serde(rename = "unstaged")]
    Unstaged,
    #[serde(rename = "turn", rename_all = "camelCase")]
    Turn {
        turn_id: String,
        file_path: Option<String>,
        reveal_request_id: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitScope {
    Branch,
    Unstaged,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffPanelStore {
    pub by_thread_key: HashMap<String, DiffPanelSelection>,
    pub branch_base_ref_by_thread_key: HashMap<String, Option<String>>,
}

fn normalize_base_ref(base_ref: Option<&str>) -> Option<String> {
    base_ref
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

impl DiffPanelStore {
    pub fn select_git_scope(&mut self, thread: &ScopedThreadRef, scope: GitScope) {
        let key = thread.key();
        let previous = self.by_thread_key.get(&key);
        let previous_base_ref = match previous {
            Some(DiffPanelSelection::Branch { base_ref }) => base_ref.clone(),
            _ => self
                .branch_base_ref_by_thread_key
                .get(&key)
                .cloned()
                .flatten(),
        };
        if let Some(DiffPanelSelection::Branch { base_ref }) = previous {
            self.branch_base_ref_by_thread_key
                .insert(key.clone(), base_ref.clone());
        }
        self.by_thread_key.insert(
            key,
            match scope {
                GitScope::Branch => DiffPanelSelection::Branch {
                    base_ref: previous_base_ref,
                },
                GitScope::Unstaged => DiffPanelSelection::Unstaged,
            },
        );
    }

    pub fn select_branch_base_ref(&mut self, thread: &ScopedThreadRef, base_ref: Option<&str>) {
        let key = thread.key();
        let normalized = normalize_base_ref(base_ref);
        self.by_thread_key.insert(
            key.clone(),
            DiffPanelSelection::Branch {
                base_ref: normalized.clone(),
            },
        );
        self.branch_base_ref_by_thread_key.insert(key, normalized);
    }

    pub fn select_turn(&mut self, thread: &ScopedThreadRef, turn_id: &str, file_path: Option<&str>) {
        let key = thread.key();
        let reveal_request_id = match self.by_thread_key.get(&key) {
            Some(DiffPanelSelection::Turn {
                reveal_request_id, ..
            }) => reveal_request_id + 1,
            _ => 1,
        };
        self.by_thread_key.insert(
            key,
            DiffPanelSelection::Turn {
                turn_id: turn_id.to_string(),
                file_path: file_path
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(str::to_string),
                reveal_request_id,
            },
        );
    }

    pub fn reconcile_turn_selection(&mut self, thread: &ScopedThreadRef, available_turn_ids: &[String]) {
        let key = thread.key();
        let Some(DiffPanelSelection::Turn {
            turn_id,
            file_path,
            reveal_request_id,
        }) = self.by_thread_key.get(&key)
        else {
            return;
        };
        let Some(latest) = available_turn_ids.first() else {
            return;
        };
        if available_turn_ids.contains(turn_id) {
            return;
        }
        let next = DiffPanelSelection::Turn {
            turn_id: latest.clone(),
            file_path: file_path.clone(),
            reveal_request_id: *reveal_request_id,
        };
        self.by_thread_key.insert(key, next);
    }

    pub fn remove_thread(&mut self, thread: &ScopedThreadRef) {
        let key = thread.key();
        self.by_thread_key.remove(&key);
        self.branch_base_ref_by_thread_key.remove(&key);
    }

    /// `selectThreadDiffPanelSelection`.
    pub fn selection(&self, thread: Option<&ScopedThreadRef>) -> DiffPanelSelection {
        thread
            .and_then(|thread| self.by_thread_key.get(&thread.key()))
            .cloned()
            .unwrap_or(DiffPanelSelection::Branch { base_ref: None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread() -> ScopedThreadRef {
        ScopedThreadRef::new("environment-1", "thread-1")
    }

    fn branch(base_ref: Option<&str>) -> DiffPanelSelection {
        DiffPanelSelection::Branch {
            base_ref: base_ref.map(Into::into),
        }
    }

    #[test]
    fn defaults_each_thread_to_changes_without_requiring_git_status() {
        let store = DiffPanelStore::default();
        assert_eq!(store.selection(Some(&thread())), branch(None));
    }

    #[test]
    fn defaults_to_changes_before_a_thread_is_selected() {
        assert_eq!(DiffPanelStore::default().selection(None), branch(None));
    }

    #[test]
    fn keeps_a_custom_base_when_a_generic_open_selects_changes_again() {
        let mut store = DiffPanelStore::default();
        store.select_branch_base_ref(&thread(), Some("origin/release"));
        store.select_git_scope(&thread(), GitScope::Branch);
        store.select_turn(&thread(), "turn-1", None);
        store.select_git_scope(&thread(), GitScope::Branch);
        assert_eq!(store.selection(Some(&thread())), branch(Some("origin/release")));
    }

    #[test]
    fn preserves_an_explicit_branch_selection() {
        let mut store = DiffPanelStore::default();
        store.select_git_scope(&thread(), GitScope::Branch);
        assert_eq!(store.selection(Some(&thread())), branch(None));
    }

    #[test]
    fn clears_incompatible_selection_fields_when_changing_scopes() {
        let mut store = DiffPanelStore::default();
        store.select_turn(&thread(), "turn-1", Some("src/app.ts"));
        store.select_git_scope(&thread(), GitScope::Unstaged);
        assert_eq!(store.selection(Some(&thread())), DiffPanelSelection::Unstaged);
        store.select_branch_base_ref(&thread(), Some(" origin/main "));
        assert_eq!(store.selection(Some(&thread())), branch(Some("origin/main")));
    }

    #[test]
    fn clears_a_threads_turn_and_file_when_selecting_working_tree_without_changing_another_threads_branch_base() {
        let other = ScopedThreadRef::new("environment-1", "thread-2");
        let mut store = DiffPanelStore::default();
        store.select_branch_base_ref(&thread(), Some("origin/release"));
        store.select_turn(&thread(), "turn-1", Some("src/app.ts"));
        store.select_branch_base_ref(&other, Some("origin/main"));

        store.select_git_scope(&thread(), GitScope::Unstaged);

        assert_eq!(store.selection(Some(&thread())), DiffPanelSelection::Unstaged);
        assert_eq!(store.selection(Some(&other)), branch(Some("origin/main")));

        store.select_git_scope(&thread(), GitScope::Branch);
        assert_eq!(store.selection(Some(&thread())), branch(Some("origin/release")));
    }

    #[test]
    fn increments_the_reveal_request_when_opening_the_same_turn_file_again() {
        let mut store = DiffPanelStore::default();
        store.select_turn(&thread(), "turn-1", Some("src/app.ts"));
        store.select_turn(&thread(), "turn-1", Some("src/app.ts"));
        assert_eq!(
            store.selection(Some(&thread())),
            DiffPanelSelection::Turn {
                turn_id: "turn-1".into(),
                file_path: Some("src/app.ts".into()),
                reveal_request_id: 2
            }
        );
    }

    #[test]
    fn restores_the_selected_branch_base_after_visiting_another_scope() {
        let mut store = DiffPanelStore::default();
        store.select_branch_base_ref(&thread(), Some("origin/main"));
        store.select_git_scope(&thread(), GitScope::Unstaged);
        store.select_git_scope(&thread(), GitScope::Branch);
        assert_eq!(store.selection(Some(&thread())), branch(Some("origin/main")));
    }

    #[test]
    fn reconciles_a_missing_turn_selection_to_the_latest_available_turn() {
        let mut store = DiffPanelStore::default();
        store.select_turn(&thread(), "turn-missing", Some("src/app.ts"));
        store.reconcile_turn_selection(&thread(), &["turn-latest".to_string()]);
        assert_eq!(
            store.selection(Some(&thread())),
            DiffPanelSelection::Turn {
                turn_id: "turn-latest".into(),
                file_path: Some("src/app.ts".into()),
                reveal_request_id: 1
            }
        );
    }

    #[test]
    fn persisted_shape_uses_the_upstream_keys() {
        let mut store = DiffPanelStore::default();
        store.select_turn(&thread(), "turn-1", None);
        let json = serde_json::to_value(&store).unwrap();
        assert_eq!(
            json["byThreadKey"]["environment-1:thread-1"],
            serde_json::json!({"kind": "turn", "turnId": "turn-1", "filePath": null, "revealRequestId": 1})
        );
    }
}
