//! Port of upstream/apps/web/src/threadSelectionStore.ts
//!
//! Sidebar multi-selection: Cmd+Click toggles, Shift+Click selects a range
//! from the anchor, bulk actions read the set. Owned by the sidebar view
//! (upstream: a zustand store).
// A faithful port keeps upstream's whole API (rule 3); callers that are
// not ported yet (T14 reducers, the chat header menu, settings) use the rest.
#![allow(dead_code)]

use std::collections::HashSet;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadSelection {
    /// Selected scoped thread keys, in selection order.
    selected: Vec<String>,
    /// Anchors shift-click range selection.
    anchor: Option<String>,
}

impl ThreadSelection {
    pub fn selected(&self) -> &[String] {
        &self.selected
    }

    pub fn anchor(&self) -> Option<&str> {
        self.anchor.as_deref()
    }

    pub fn contains(&self, key: &str) -> bool {
        self.selected.iter().any(|selected| selected == key)
    }

    pub fn has_selection(&self) -> bool {
        !self.selected.is_empty()
    }

    /// `toggleThread` (Cmd/Ctrl+Click).
    pub fn toggle_thread(&mut self, key: &str) {
        if let Some(index) = self.selected.iter().position(|selected| selected == key) {
            self.selected.remove(index);
        } else {
            self.selected.push(key.to_string());
            self.anchor = Some(key.to_string());
        }
    }

    /// `rangeSelectTo` (Shift+Click): from the anchor through `key` in
    /// `ordered`; without a usable anchor it adds `key` and anchors there.
    pub fn range_select_to(&mut self, key: &str, ordered: &[String]) {
        let add = |selected: &mut Vec<String>, key: &str| {
            if !selected.iter().any(|existing| existing == key) {
                selected.push(key.to_string());
            }
        };
        let Some(anchor) = self.anchor.clone() else {
            add(&mut self.selected, key);
            self.anchor = Some(key.to_string());
            return;
        };
        let (Some(anchor_index), Some(target_index)) = (
            ordered.iter().position(|candidate| *candidate == anchor),
            ordered.iter().position(|candidate| candidate == key),
        ) else {
            add(&mut self.selected, key);
            self.anchor = Some(key.to_string());
            return;
        };
        let (start, end) = (
            anchor_index.min(target_index),
            anchor_index.max(target_index),
        );
        for candidate in &ordered[start..=end] {
            add(&mut self.selected, candidate);
        }
        // The anchor stays so later shift-clicks extend from the same point.
    }

    /// `clearSelection`.
    pub fn clear(&mut self) {
        self.selected.clear();
        self.anchor = None;
    }

    /// `setAnchor` (plain-click navigation).
    pub fn set_anchor(&mut self, key: &str) {
        self.anchor = Some(key.to_string());
    }

    /// `removeFromSelection`.
    pub fn remove(&mut self, keys: &[String]) {
        let remove: HashSet<&String> = keys.iter().collect();
        let before = self.selected.len();
        self.selected.retain(|key| !remove.contains(key));
        if self.selected.len() != before
            && self
                .anchor
                .as_ref()
                .is_some_and(|anchor| remove.contains(anchor))
        {
            self.anchor = None;
        }
    }
}

/// `getThreadKeysToDeselectAfterDelete`: completed deletions and missing
/// threads leave the selection; failed or unprocessed ones stay.
pub fn get_thread_keys_to_deselect_after_delete(
    selected: &[String],
    deleted: &HashSet<String>,
    has_thread: impl Fn(&str) -> bool,
) -> Vec<String> {
    selected
        .iter()
        .filter(|key| deleted.contains(*key) || !has_thread(key))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn toggles_and_anchors() {
        let mut selection = ThreadSelection::default();
        selection.toggle_thread("a");
        assert_eq!(selection.selected(), keys(&["a"]));
        assert_eq!(selection.anchor(), Some("a"));
        selection.toggle_thread("a");
        assert!(!selection.has_selection());
        assert_eq!(selection.anchor(), Some("a"));
    }

    #[test]
    fn range_selects_from_the_anchor_and_keeps_it() {
        let ordered = keys(&["a", "b", "c", "d"]);
        let mut selection = ThreadSelection::default();
        selection.set_anchor("b");
        selection.range_select_to("d", &ordered);
        assert_eq!(selection.selected(), keys(&["b", "c", "d"]));
        assert_eq!(selection.anchor(), Some("b"));
        selection.range_select_to("a", &ordered);
        assert!(selection.contains("a"));
    }

    #[test]
    fn range_without_anchor_selects_the_target() {
        let mut selection = ThreadSelection::default();
        selection.range_select_to("c", &keys(&["a", "c"]));
        assert_eq!(selection.selected(), keys(&["c"]));
        assert_eq!(selection.anchor(), Some("c"));
    }

    #[test]
    fn removal_clears_a_removed_anchor() {
        let mut selection = ThreadSelection::default();
        selection.toggle_thread("a");
        selection.toggle_thread("b");
        selection.remove(&keys(&["b"]));
        assert_eq!(selection.selected(), keys(&["a"]));
        assert_eq!(selection.anchor(), None);
    }

    #[test]
    fn deselects_completed_and_missing_deletions() {
        let deleted: HashSet<String> = ["a".to_string()].into_iter().collect();
        assert_eq!(
            get_thread_keys_to_deselect_after_delete(&keys(&["a", "b", "c"]), &deleted, |key| key
                != "c"),
            keys(&["a", "c"])
        );
    }
}
