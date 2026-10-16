//! Port of the persistence half of upstream/apps/web/src/diffPanelStore.ts
//! (`persist` → `t3code:diff-panel-state:v1`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::diff_panel_store::DiffPanelStore;

pub const DIFF_PANEL_STATE_STORAGE_KEY: &str = "t3code:diff-panel-state:v1";
pub const DIFF_PANEL_STATE_VERSION: u32 = 2;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedDiffPanelState {
    by_thread_key: std::collections::HashMap<String, super::diff_panel_store::DiffPanelSelection>,
    branch_base_ref_by_thread_key: std::collections::HashMap<String, Option<String>>,
}

impl From<&DiffPanelStore> for PersistedDiffPanelState {
    fn from(store: &DiffPanelStore) -> Self {
        Self {
            by_thread_key: store.by_thread_key.clone(),
            branch_base_ref_by_thread_key: store.branch_base_ref_by_thread_key.clone(),
        }
    }
}

impl From<PersistedDiffPanelState> for DiffPanelStore {
    fn from(value: PersistedDiffPanelState) -> Self {
        Self {
            by_thread_key: value.by_thread_key,
            branch_base_ref_by_thread_key: value.branch_base_ref_by_thread_key,
        }
    }
}

pub fn encode_diff_panel_store(store: &DiffPanelStore) -> Value {
    serde_json::to_value(PersistedDiffPanelState::from(store)).expect("diff panel store encodes")
}

pub fn decode_diff_panel_store(value: &Value) -> Option<DiffPanelStore> {
    serde_json::from_value::<PersistedDiffPanelState>(value.clone())
        .ok()
        .map(Into::into)
}

pub fn decode_diff_panel_store_json(json: &str) -> Option<DiffPanelStore> {
    serde_json::from_str::<PersistedDiffPanelState>(json)
        .ok()
        .map(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::diff_panel_store::{DiffPanelSelection, ScopedThreadRef};

    #[test]
    fn round_trips_upstream_storage_shape() {
        let mut store = DiffPanelStore::default();
        let thread = ScopedThreadRef::new("env", "thread");
        store.select_turn(&thread, "run-1", Some("src/a.ts"));
        let encoded = encode_diff_panel_store(&store);
        let decoded = decode_diff_panel_store(&encoded).expect("decode");
        assert_eq!(decoded.by_thread_key, store.by_thread_key);
    }

    #[test]
    fn accepts_turn_selection_json() {
        let value = serde_json::json!({
          "byThreadKey": {
            "env:thread": {
              "kind": "turn",
              "turnId": "run-1",
              "filePath": "src/a.ts",
              "revealRequestId": 1
            }
          },
          "branchBaseRefByThreadKey": {}
        });
        let store = decode_diff_panel_store(&value).expect("decode");
        assert!(matches!(
            store.selection(Some(&ScopedThreadRef::new("env", "thread"))),
            DiffPanelSelection::Turn { .. }
        ));
    }
}
