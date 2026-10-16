#![allow(dead_code)]

//! Port of upstream/apps/web/src/closedViewStore.ts (panel-tab entries; browser
//! preview sessions are cut — see PORTING.md Deviations).

use crate::shell::reopen_closed_view::ClosedViewEntry;
use crate::state::models::scoped_thread_key;
use crate::state::right_panel_store::{ClosedPanelTab, RightPanelSurface};

const CLOSED_VIEW_LIMIT: usize = 20;

/// `ClosedViewStoreState` (panel-tab portion).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClosedViewStore {
    pub entries: Vec<ClosedViewEntry>,
}

impl ClosedViewStore {
    /// `remember`.
    pub fn remember(
        &mut self,
        thread_ref: turin_contracts::environment::ScopedThreadRef,
        surface: RightPanelSurface,
    ) -> String {
        let view = ClosedPanelTab::new(thread_ref, surface);
        let id = view.id.clone();
        self.entries.retain(|entry| !same_target(entry, &view));
        self.entries.insert(
            0,
            ClosedViewEntry {
                id: id.clone(),
                thread_ref: view.thread_ref,
                surface: view.surface,
            },
        );
        self.entries.truncate(CLOSED_VIEW_LIMIT);
        id
    }

    /// `defer`.
    pub fn defer(&mut self, id: &str) {
        let Some(index) = self.entries.iter().position(|entry| entry.id == id) else {
            return;
        };
        let entry = self.entries.remove(index);
        self.entries.push(entry);
    }

    /// `remove`.
    pub fn remove(&mut self, id: &str) {
        self.entries.retain(|entry| entry.id != id);
    }
}

fn same_target(entry: &ClosedViewEntry, view: &ClosedPanelTab) -> bool {
    scoped_thread_key(
        &entry.thread_ref.environment_id,
        &entry.thread_ref.thread_id,
    ) == scoped_thread_key(&view.thread_ref.environment_id, &view.thread_ref.thread_id)
        && entry.surface.id() == view.surface.id()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::right_panel_store::RightPanelSurface;
    use turin_contracts::environment::ScopedThreadRef;
    use turin_contracts::{EnvironmentId, ThreadId};

    fn thread_ref() -> ScopedThreadRef {
        ScopedThreadRef {
            environment_id: EnvironmentId::new("local").unwrap(),
            thread_id: ThreadId::new("thread-1").unwrap(),
        }
    }

    #[test]
    fn remember_replaces_same_target_and_caps_entries() {
        let mut store = ClosedViewStore::default();
        let thread = thread_ref();
        store.remember(thread.clone(), RightPanelSurface::Diff);
        store.remember(thread.clone(), RightPanelSurface::PullRequests);
        assert_eq!(store.entries.len(), 2);
        store.remember(thread, RightPanelSurface::Diff);
        assert_eq!(store.entries.len(), 2);
        assert_eq!(store.entries[0].surface, RightPanelSurface::Diff);
    }
}
