//! Port of upstream/apps/web/src/sidebarPendingFileDropStore.ts (+ test).

#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};

use turin_contracts::environment::ScopedThreadRef;

/// Field-wise ref equality. `scopedThreadKey` joins with `:`, so two distinct
/// refs can collide when an id itself contains one.
pub fn is_same_sidebar_thread_ref(a: &ScopedThreadRef, b: &ScopedThreadRef) -> bool {
    a.environment_id == b.environment_id && a.thread_id == b.thread_id
}

/// One sidebar row drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarPendingFileDrop {
    pub id: String,
    pub thread_ref: ScopedThreadRef,
    pub files: Vec<String>,
}

#[derive(Debug, Default)]
pub struct SidebarPendingFileDropStore {
    pending: Vec<SidebarPendingFileDrop>,
    next_id: AtomicU64,
}

impl SidebarPendingFileDropStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pending(&self) -> &[SidebarPendingFileDrop] {
        &self.pending
    }

    pub fn queue_pending_file_drop(
        &mut self,
        thread_ref: ScopedThreadRef,
        files: Vec<String>,
    ) -> String {
        let id = format!(
            "sidebar-file-drop-{}",
            self.next_id.fetch_add(1, Ordering::Relaxed) + 1
        );
        self.pending.push(SidebarPendingFileDrop {
            id: id.clone(),
            thread_ref,
            files,
        });
        id
    }

    pub fn clear_pending_file_drop(&mut self, id: &str) {
        self.pending.retain(|drop| drop.id != id);
    }

    pub fn clear_pending_file_drops_for_thread(&mut self, thread_ref: &ScopedThreadRef) {
        self.pending
            .retain(|drop| !is_same_sidebar_thread_ref(&drop.thread_ref, thread_ref));
    }

    pub fn consume_pending_file_drop(
        &mut self,
        thread_ref: &ScopedThreadRef,
    ) -> Option<Vec<String>> {
        let matches: Vec<_> = self
            .pending
            .iter()
            .filter(|drop| is_same_sidebar_thread_ref(&drop.thread_ref, thread_ref))
            .cloned()
            .collect();
        if matches.is_empty() {
            return None;
        }
        let matched_ids: std::collections::HashSet<_> =
            matches.iter().map(|drop| &drop.id).collect();
        self.pending.retain(|drop| !matched_ids.contains(&drop.id));
        Some(matches.into_iter().flat_map(|drop| drop.files).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::{EnvironmentId, ThreadId};

    fn ref_of(environment_id: &str, thread_id: &str) -> ScopedThreadRef {
        ScopedThreadRef {
            environment_id: EnvironmentId::new(environment_id).unwrap(),
            thread_id: ThreadId::new(thread_id).unwrap(),
        }
    }

    #[test]
    fn starts_empty() {
        assert!(SidebarPendingFileDropStore::new().pending().is_empty());
    }

    #[test]
    fn stashes_and_consumes_matching_thread() {
        let mut store = SidebarPendingFileDropStore::new();
        store.queue_pending_file_drop(
            ref_of("env-1", "thread-1"),
            vec!["a.png".into(), "b.png".into()],
        );
        assert_eq!(
            store.consume_pending_file_drop(&ref_of("env-1", "thread-1")),
            Some(vec!["a.png".into(), "b.png".into()])
        );
        assert!(store.pending().is_empty());
    }

    #[test]
    fn accumulates_repeat_drops() {
        let mut store = SidebarPendingFileDropStore::new();
        store.queue_pending_file_drop(ref_of("env-1", "thread-1"), vec!["a.png".into()]);
        store.queue_pending_file_drop(ref_of("env-1", "thread-1"), vec!["b.png".into()]);
        assert_eq!(
            store.consume_pending_file_drop(&ref_of("env-1", "thread-1")),
            Some(vec!["a.png".into(), "b.png".into()])
        );
    }

    #[test]
    fn keeps_other_threads_when_consuming_one() {
        let mut store = SidebarPendingFileDropStore::new();
        store.queue_pending_file_drop(ref_of("env-1", "thread-1"), vec!["a.png".into()]);
        store.queue_pending_file_drop(ref_of("env-1", "thread-2"), vec!["b.png".into()]);
        assert_eq!(
            store.consume_pending_file_drop(&ref_of("env-1", "thread-2")),
            Some(vec!["b.png".into()])
        );
        assert_eq!(store.pending().len(), 1);
    }

    #[test]
    fn clears_only_matching_stale_navigation_id() {
        let mut store = SidebarPendingFileDropStore::new();
        let first =
            store.queue_pending_file_drop(ref_of("env-1", "thread-1"), vec!["a.png".into()]);
        store.queue_pending_file_drop(ref_of("env-1", "thread-1"), vec!["b.png".into()]);
        store.clear_pending_file_drop(&first);
        assert_eq!(
            store.consume_pending_file_drop(&ref_of("env-1", "thread-1")),
            Some(vec!["b.png".into()])
        );
    }

    #[test]
    fn newer_drop_survives_failed_first_navigation() {
        let mut store = SidebarPendingFileDropStore::new();
        let first =
            store.queue_pending_file_drop(ref_of("env-1", "thread-1"), vec!["a.png".into()]);
        let second =
            store.queue_pending_file_drop(ref_of("env-1", "thread-1"), vec!["b.png".into()]);
        store.clear_pending_file_drop(&first);
        let remaining = store.pending();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, second);
        assert_eq!(remaining[0].files, vec!["b.png"]);
        assert_eq!(
            store.consume_pending_file_drop(&ref_of("env-1", "thread-1")),
            Some(vec!["b.png".into()])
        );
    }

    #[test]
    fn clears_every_drop_for_missing_thread() {
        let mut store = SidebarPendingFileDropStore::new();
        store.queue_pending_file_drop(ref_of("env-1", "thread-1"), vec!["a.png".into()]);
        store.queue_pending_file_drop(ref_of("env-1", "thread-1"), vec!["b.png".into()]);
        store.queue_pending_file_drop(ref_of("env-1", "thread-2"), vec!["c.png".into()]);
        store.clear_pending_file_drops_for_thread(&ref_of("env-1", "thread-1"));
        assert_eq!(
            store.consume_pending_file_drop(&ref_of("env-1", "thread-2")),
            Some(vec!["c.png".into()])
        );
        assert!(store.pending().is_empty());
    }

    #[test]
    fn does_not_confuse_refs_whose_joined_keys_collide() {
        let mut store = SidebarPendingFileDropStore::new();
        store.queue_pending_file_drop(ref_of("a", "b:c"), vec!["a.png".into()]);
        assert!(
            store
                .consume_pending_file_drop(&ref_of("a:b", "c"))
                .is_none()
        );
        assert_eq!(store.pending().len(), 1);
    }

    #[test]
    fn compares_fields_not_joined_keys() {
        assert!(is_same_sidebar_thread_ref(
            &ref_of("a", "b:c"),
            &ref_of("a", "b:c")
        ));
        assert!(!is_same_sidebar_thread_ref(
            &ref_of("a", "b:c"),
            &ref_of("a:b", "c")
        ));
        assert!(!is_same_sidebar_thread_ref(
            &ref_of("a", "b"),
            &ref_of("a", "c")
        ));
    }
}
