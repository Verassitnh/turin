//! Port of upstream/apps/web/src/worktreeCleanup.ts: `getOrphanedWorktreePathForThread`.
//!
//! `formatWorktreePathForDisplay` lives in the sidebar's status indicators
//! (`components/sidebar/thread_status_indicators.rs`), which is its only
//! caller and keeps its tests.
// Consumers are wired by the sidebar and T22.
#![allow(dead_code)]

use turin_contracts::ThreadId;

use crate::state::models::EnvironmentThreadShell;

/// `normalizeWorktreePath`: trimmed, and `None` when empty.
fn normalize_worktree_path(path: Option<&str>) -> Option<&str> {
    let trimmed = path?.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// `getOrphanedWorktreePathForThread`: the target thread's worktree path when
/// no other listed thread links to it, so deleting the target can remove the
/// worktree. `None` when the thread is missing, has no worktree, or shares it.
pub fn get_orphaned_worktree_path_for_thread(
    threads: &[EnvironmentThreadShell],
    thread_id: &ThreadId,
) -> Option<String> {
    let target = threads.iter().find(|thread| &thread.id == thread_id)?;
    let target_path = normalize_worktree_path(target.worktree_path.as_deref())?;
    let is_shared = threads.iter().any(|thread| {
        &thread.id != thread_id
            && normalize_worktree_path(thread.worktree_path.as_deref()) == Some(target_path)
    });
    (!is_shared).then(|| target_path.to_owned())
}

#[cfg(test)]
mod tests {
    use turin_contracts::{EnvironmentId, ProjectId, ThreadId};

    use super::get_orphaned_worktree_path_for_thread;
    use crate::state::models::EnvironmentThreadShell;

    fn thread(id: &str, worktree_path: Option<&str>) -> EnvironmentThreadShell {
        let mut shell = EnvironmentThreadShell::new(
            EnvironmentId::new("environment-local").unwrap(),
            ThreadId::new(id).unwrap(),
            ProjectId::new("project-1").unwrap(),
            "Thread",
            "2026-02-13T00:00:00.000Z",
        );
        shell.worktree_path = worktree_path.map(str::to_owned);
        shell
    }

    fn target() -> ThreadId {
        ThreadId::new("thread-1").unwrap()
    }

    #[test]
    fn returns_none_when_the_target_thread_does_not_exist() {
        assert_eq!(
            get_orphaned_worktree_path_for_thread(&[], &ThreadId::new("missing-thread").unwrap()),
            None
        );
    }

    #[test]
    fn returns_none_when_the_target_thread_has_no_worktree() {
        let threads = [thread("thread-1", None)];
        assert_eq!(
            get_orphaned_worktree_path_for_thread(&threads, &target()),
            None
        );
    }

    #[test]
    fn returns_the_path_when_no_other_thread_links_to_that_worktree() {
        let threads = [thread("thread-1", Some("/tmp/repo/worktrees/feature-a"))];
        assert_eq!(
            get_orphaned_worktree_path_for_thread(&threads, &target()).as_deref(),
            Some("/tmp/repo/worktrees/feature-a")
        );
    }

    #[test]
    fn returns_none_when_another_thread_links_to_the_same_worktree() {
        let threads = [
            thread("thread-1", Some("/tmp/repo/worktrees/feature-a")),
            thread("thread-2", Some("/tmp/repo/worktrees/feature-a")),
        ];
        assert_eq!(
            get_orphaned_worktree_path_for_thread(&threads, &target()),
            None
        );
    }

    #[test]
    fn ignores_threads_linked_to_different_worktrees() {
        let threads = [
            thread("thread-1", Some("/tmp/repo/worktrees/feature-a")),
            thread("thread-2", Some("/tmp/repo/worktrees/feature-b")),
        ];
        assert_eq!(
            get_orphaned_worktree_path_for_thread(&threads, &target()).as_deref(),
            Some("/tmp/repo/worktrees/feature-a")
        );
    }

    #[test]
    fn treats_a_blank_worktree_path_as_no_worktree() {
        let threads = [
            thread("thread-1", Some("   ")),
            thread("thread-2", Some("   ")),
        ];
        assert_eq!(
            get_orphaned_worktree_path_for_thread(&threads, &target()),
            None
        );
    }
}
