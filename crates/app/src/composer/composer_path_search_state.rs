//! Port of upstream/apps/web/src/lib/composerPathSearchState.ts
//!
//! `useComposerPathSearch` wraps the search query and hands the composer menu
//! a stable entry list (`useMemo` on `state.entries`). Here the query result
//! comes in as [`ComposerPathSearchState`] and [`ComposerPathSearchProjection`]
//! is the memo: it rebuilds the list only when the source entries change.
//! Entries keep `path` and `kind`; `parentPath` is dropped, as upstream does.

use crate::state::composer_path_search::{ComposerPathSearchEntry, ComposerPathSearchState};

#[derive(Debug, Default)]
pub struct ComposerPathSearchProjection {
    source: Option<Vec<ComposerPathSearchEntry>>,
    entries: Vec<ComposerPathSearchEntry>,
    rebuilds: usize,
}

impl ComposerPathSearchProjection {
    pub fn project(&mut self, state: &ComposerPathSearchState) -> ComposerPathSearchState {
        if self.source.as_ref() != Some(&state.entries) {
            self.entries = state
                .entries
                .iter()
                .map(|entry| ComposerPathSearchEntry {
                    path: entry.path.clone(),
                    kind: entry.kind,
                    parent_path: None,
                })
                .collect();
            self.source = Some(state.entries.clone());
            self.rebuilds += 1;
        }
        ComposerPathSearchState {
            entries: self.entries.clone(),
            error: state.error.clone(),
            is_pending: state.is_pending,
        }
    }

    /// How many times the entry list was rebuilt (the memo's misses).
    pub fn rebuilds(&self) -> usize {
        self.rebuilds
    }
}

#[cfg(test)]
mod tests {
    //! Upstream ships no test file; these cover the projection.

    use super::*;
    use crate::state::composer_path_search::ComposerPathSearchKind;

    fn entry(path: &str, kind: ComposerPathSearchKind) -> ComposerPathSearchEntry {
        ComposerPathSearchEntry {
            path: path.into(),
            kind,
            parent_path: Some("src".into()),
        }
    }

    fn state(entries: Vec<ComposerPathSearchEntry>) -> ComposerPathSearchState {
        ComposerPathSearchState {
            entries,
            is_pending: true,
            error: Some("boom".into()),
        }
    }

    #[test]
    fn keeps_path_and_kind_and_passes_status_through() {
        let mut projection = ComposerPathSearchProjection::default();
        let projected = projection.project(&state(vec![
            entry("src/a.ts", ComposerPathSearchKind::File),
            entry("src", ComposerPathSearchKind::Directory),
        ]));
        assert_eq!(
            projected.entries,
            [
                ComposerPathSearchEntry {
                    path: "src/a.ts".into(),
                    kind: ComposerPathSearchKind::File,
                    parent_path: None,
                },
                ComposerPathSearchEntry {
                    path: "src".into(),
                    kind: ComposerPathSearchKind::Directory,
                    parent_path: None,
                },
            ]
        );
        assert!(projected.is_pending);
        assert_eq!(projected.error.as_deref(), Some("boom"));
    }

    #[test]
    fn rebuilds_the_list_only_when_the_source_entries_change() {
        let mut projection = ComposerPathSearchProjection::default();
        let entries = vec![entry("a", ComposerPathSearchKind::File)];
        projection.project(&state(entries.clone()));
        let mut pending_cleared = state(entries.clone());
        pending_cleared.is_pending = false;
        let projected = projection.project(&pending_cleared);
        assert!(!projected.is_pending);
        assert_eq!(projection.rebuilds(), 1);

        projection.project(&state(vec![entry("b", ComposerPathSearchKind::File)]));
        assert_eq!(projection.rebuilds(), 2);
    }
}
