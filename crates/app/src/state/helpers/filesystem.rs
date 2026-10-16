//! Port of upstream/packages/client-runtime/src/state/filesystem.ts
//! Also ports upstream/apps/web/src/lib/projectPaths.ts.
//!
//! The browse model behind the add-project path picker. Not ported:
//! `resolveFilesystemReadAccess` (auth session scopes) and
//! `canPreloadBrowsePath` (remote connection phase): one process has no
//! sessions and is always connected. `createFilesystemEnvironmentAtoms` is
//! RPC plumbing.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use turin_contracts::filesystem::FilesystemBrowseEntry;

use crate::state::projects::{
    can_navigate_up, get_browse_directory_path, get_browse_leaf_path_segment,
    get_browse_parent_path, has_trailing_path_separator, is_filesystem_browse_query,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilesystemBrowsePath {
    pub is_browsing: bool,
    pub directory_path: String,
    pub filter_query: String,
    pub parent_path: Option<String>,
    pub can_browse_up: bool,
}

/// `getFilesystemBrowsePath(query, platform = "", enabled = true)`.
pub fn get_filesystem_browse_path(
    query: &str,
    platform: &str,
    enabled: bool,
) -> FilesystemBrowsePath {
    let is_browsing = enabled && is_filesystem_browse_query(query, platform);
    let directory_path = if is_browsing {
        get_browse_directory_path(query).to_owned()
    } else {
        String::new()
    };
    let filter_query = if is_browsing && !has_trailing_path_separator(query) {
        get_browse_leaf_path_segment(query).to_owned()
    } else {
        String::new()
    };
    let parent_path = if is_browsing {
        get_browse_parent_path(&directory_path)
    } else {
        None
    };
    FilesystemBrowsePath {
        can_browse_up: is_browsing && can_navigate_up(&directory_path),
        is_browsing,
        directory_path,
        filter_query,
        parent_path,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilteredBrowseEntries<'a> {
    pub visible_entries: Vec<&'a FilesystemBrowseEntry>,
    pub exact_entry: Option<&'a FilesystemBrowseEntry>,
}

pub fn filter_filesystem_browse_entries<'a>(
    entries: &'a [FilesystemBrowseEntry],
    query: &str,
) -> FilteredBrowseEntries<'a> {
    let lower_query = query.to_lowercase();
    let show_hidden = query.starts_with('.');
    let visible_entries: Vec<&FilesystemBrowseEntry> = entries
        .iter()
        .filter(|entry| {
            let name = entry.name.as_str();
            name.to_lowercase().starts_with(&lower_query) && (show_hidden || !name.starts_with('.'))
        })
        .collect();
    let exact_entry = if query.is_empty() {
        None
    } else {
        visible_entries
            .iter()
            .copied()
            .find(|entry| entry.name.as_str() == query)
    };
    FilteredBrowseEntries {
        visible_entries,
        exact_entry,
    }
}

/// `createBrowseNavigationCoordinator`: only the latest navigation commits.
#[derive(Clone, Default)]
pub struct BrowseNavigationCoordinator {
    generation: Arc<AtomicU64>,
}

impl BrowseNavigationCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Awaits `load`, then calls `commit` unless a newer navigation started
    /// (or [`Self::invalidate`] ran) meanwhile. Returns whether it committed.
    pub async fn run<L, C>(&self, load: L, commit: C) -> bool
    where
        L: Future<Output = ()>,
        C: FnOnce(),
    {
        let navigation_generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        load.await;
        if navigation_generation != self.generation.load(Ordering::SeqCst) {
            return false;
        }
        commit();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::TrimmedNonEmptyString;
    use std::sync::Mutex;

    fn entry(name: &str, full_path: &str) -> FilesystemBrowseEntry {
        FilesystemBrowseEntry {
            name: TrimmedNonEmptyString::new(name).unwrap(),
            full_path: TrimmedNonEmptyString::new(full_path).unwrap(),
        }
    }

    #[test]
    fn derives_the_browse_target_and_navigation_state() {
        assert_eq!(
            get_filesystem_browse_path("~/projects/t3", "", true),
            FilesystemBrowsePath {
                is_browsing: true,
                directory_path: "~/projects/".into(),
                filter_query: "t3".into(),
                parent_path: Some("~/".into()),
                can_browse_up: true,
            }
        );
        assert!(!get_filesystem_browse_path("C:\\Users\\test", "MacIntel", true).is_browsing);
        assert!(!get_filesystem_browse_path("~/projects/", "", false).is_browsing);
    }

    #[test]
    fn filters_names_hidden_directories_and_exact_matches_consistently() {
        let entries = [
            entry(".config", "/Users/test/.config"),
            entry("Code", "/Users/test/Code"),
            entry("codething", "/Users/test/codething"),
        ];
        let filtered = filter_filesystem_browse_entries(&entries, "co");
        assert_eq!(filtered.visible_entries, vec![&entries[1], &entries[2]]);
        assert_eq!(filtered.exact_entry, None);
        assert_eq!(
            filter_filesystem_browse_entries(&entries, "").visible_entries,
            vec![&entries[1], &entries[2]]
        );
        assert_eq!(
            filter_filesystem_browse_entries(&entries, ".").visible_entries,
            vec![&entries[0]]
        );
        assert_eq!(
            filter_filesystem_browse_entries(&entries, "Code").exact_entry,
            Some(&entries[1])
        );
    }

    #[tokio::test]
    async fn only_commits_the_latest_valid_navigation() {
        let navigation = BrowseNavigationCoordinator::new();
        let commits = Arc::new(Mutex::new(Vec::<&str>::new()));
        let commit = |name: &'static str| {
            let commits = Arc::clone(&commits);
            move || commits.lock().unwrap().push(name)
        };
        let (first_tx, first_rx) = tokio::sync::oneshot::channel::<()>();
        let (second_tx, second_rx) = tokio::sync::oneshot::channel::<()>();
        let first_run = tokio::spawn({
            let navigation = navigation.clone();
            let commit = commit("first");
            async move {
                navigation
                    .run(
                        async {
                            let _ = first_rx.await;
                        },
                        commit,
                    )
                    .await
            }
        });
        tokio::task::yield_now().await;
        let second_run = tokio::spawn({
            let navigation = navigation.clone();
            let commit = commit("second");
            async move {
                navigation
                    .run(
                        async {
                            let _ = second_rx.await;
                        },
                        commit,
                    )
                    .await
            }
        });
        tokio::task::yield_now().await;
        second_tx.send(()).unwrap();
        assert!(second_run.await.unwrap());
        first_tx.send(()).unwrap();
        assert!(!first_run.await.unwrap());

        let (stale_tx, stale_rx) = tokio::sync::oneshot::channel::<()>();
        let stale_run = tokio::spawn({
            let navigation = navigation.clone();
            let commit = commit("stale");
            async move {
                navigation
                    .run(
                        async {
                            let _ = stale_rx.await;
                        },
                        commit,
                    )
                    .await
            }
        });
        tokio::task::yield_now().await;
        navigation.invalidate();
        stale_tx.send(()).unwrap();
        assert!(!stale_run.await.unwrap());
        assert_eq!(*commits.lock().unwrap(), vec!["second"]);
    }
}
