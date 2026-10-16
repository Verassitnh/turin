//! Port of upstream/apps/web/src/threadSync.ts (+ threadSync.test.ts)
//!
//! Which "loading" or "syncing" notice a thread shows while its detail is
//! being fetched.

use crate::state::thread_state::EnvironmentThreadStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadSyncPhase {
    Loading,
    Syncing,
}

pub struct ThreadSyncInput {
    pub detail_exists: bool,
    pub shell_exists: bool,
    pub status: EnvironmentThreadStatus,
}

pub fn resolve_thread_sync_phase(input: &ThreadSyncInput) -> Option<ThreadSyncPhase> {
    if !input.shell_exists {
        return None;
    }
    match input.status {
        EnvironmentThreadStatus::Empty
        | EnvironmentThreadStatus::Cached
        | EnvironmentThreadStatus::Synchronizing => Some(if input.detail_exists {
            ThreadSyncPhase::Syncing
        } else {
            ThreadSyncPhase::Loading
        }),
        EnvironmentThreadStatus::Deleted | EnvironmentThreadStatus::Live => None,
    }
}

pub fn thread_sync_label(phase: ThreadSyncPhase) -> &'static str {
    match phase {
        ThreadSyncPhase::Loading => "Loading messages...",
        ThreadSyncPhase::Syncing => "Syncing messages...",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_when_only_shell_data_is_available() {
        assert_eq!(
            resolve_thread_sync_phase(&ThreadSyncInput {
                detail_exists: false,
                shell_exists: true,
                status: EnvironmentThreadStatus::Synchronizing,
            }),
            Some(ThreadSyncPhase::Loading)
        );
    }

    #[test]
    fn syncs_when_cached_detail_is_already_visible() {
        assert_eq!(
            resolve_thread_sync_phase(&ThreadSyncInput {
                detail_exists: true,
                shell_exists: true,
                status: EnvironmentThreadStatus::Cached,
            }),
            Some(ThreadSyncPhase::Syncing)
        );
    }

    #[test]
    fn does_not_report_a_sync_phase_without_a_shell_or_after_going_live() {
        assert_eq!(
            resolve_thread_sync_phase(&ThreadSyncInput {
                detail_exists: false,
                shell_exists: false,
                status: EnvironmentThreadStatus::Empty,
            }),
            None
        );
        assert_eq!(
            resolve_thread_sync_phase(&ThreadSyncInput {
                detail_exists: true,
                shell_exists: true,
                status: EnvironmentThreadStatus::Live,
            }),
            None
        );
    }

    #[test]
    fn uses_the_same_loading_and_syncing_language_as_mobile() {
        assert_eq!(
            thread_sync_label(ThreadSyncPhase::Loading),
            "Loading messages..."
        );
        assert_eq!(
            thread_sync_label(ThreadSyncPhase::Syncing),
            "Syncing messages..."
        );
    }
}
