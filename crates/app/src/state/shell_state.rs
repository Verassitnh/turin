//! Port of the state machine of upstream/packages/client-runtime/src/state/shell.ts
//! (`makeEnvironmentShellState`'s `applyItems`).
//!
//! Dropped with the transport: the cached-shell load/save queue, the HTTP
//! snapshot loader, deferred pull-request link fills, resume cursors and the
//! reconnect/wake-up handling. The producer (the orchestrator) hands over
//! `ShellStreamItem`s; this applies them.
//!
//! Port of upstream/packages/client-runtime/src/state/shell.ts (state machine).

use std::sync::Arc;

use crate::state::shell_reducer::{
    MergeShellSnapshotOptions, ShellSnapshot, ShellStreamItem, apply_shell_stream_event,
    merge_shell_snapshot_projects, reuse_unchanged_thread_shells,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvironmentShellStatus {
    Empty,
    Cached,
    Synchronizing,
    Live,
}

/// `EnvironmentShellState`.
#[derive(Clone, Debug, PartialEq)]
pub struct EnvironmentShellState {
    pub snapshot: Option<Arc<ShellSnapshot>>,
    pub status: EnvironmentShellStatus,
    pub error: Option<String>,
    /// The producer will send `Synchronized` when its replay is complete.
    awaiting_completion: bool,
}

pub const SHELL_SYNCHRONIZATION_ERROR_MESSAGE: &str = "Could not synchronize environment data.";

impl Default for EnvironmentShellState {
    fn default() -> Self {
        Self {
            snapshot: None,
            status: EnvironmentShellStatus::Empty,
            error: None,
            awaiting_completion: false,
        }
    }
}

fn shell_status_for_snapshot(snapshot: &Option<Arc<ShellSnapshot>>) -> EnvironmentShellStatus {
    if snapshot.is_some() {
        EnvironmentShellStatus::Cached
    } else {
        EnvironmentShellStatus::Empty
    }
}

impl EnvironmentShellState {
    pub fn set_synchronizing(&mut self) {
        self.status = EnvironmentShellStatus::Synchronizing;
        self.error = None;
    }

    /// Ready, but waiting for the producer's first frames.
    pub fn set_ready(&mut self) {
        if self.status != EnvironmentShellStatus::Live {
            self.status = EnvironmentShellStatus::Synchronizing;
            self.error = None;
        }
    }

    /// The producer announces whether it will send a completion marker.
    pub fn set_awaiting_completion(&mut self, awaiting: bool) {
        self.awaiting_completion = awaiting;
    }

    pub fn set_disconnected(&mut self) {
        self.awaiting_completion = false;
        self.status = shell_status_for_snapshot(&self.snapshot);
    }

    pub fn set_stream_error(&mut self) {
        self.awaiting_completion = false;
        self.status = shell_status_for_snapshot(&self.snapshot);
        self.error = Some(SHELL_SYNCHRONIZATION_ERROR_MESSAGE.to_owned());
    }

    /// Apply each received batch with one state write. Returns whether anything changed.
    pub fn apply_items(&mut self, items: Vec<ShellStreamItem>) -> bool {
        let initial = self.clone();
        let mut waiting = self.awaiting_completion;
        let mut next = self.clone();
        for item in items {
            match item {
                ShellStreamItem::Synchronized => {
                    waiting = false;
                    if next.snapshot.is_some() {
                        next.status = EnvironmentShellStatus::Live;
                        next.error = None;
                    }
                    continue;
                }
                ShellStreamItem::Snapshot {
                    snapshot,
                    resolved_repository_identity_roots,
                } => {
                    let options =
                        resolved_repository_identity_roots.map(|roots| MergeShellSnapshotOptions {
                            resolved_repository_identity_roots: roots,
                        });
                    let previous = next.snapshot.as_deref();
                    // An authoritative snapshot keeps the object of every row it left unchanged.
                    let snapshot = if options.is_none() {
                        reuse_unchanged_thread_shells(previous, snapshot)
                    } else {
                        snapshot
                    };
                    let merged =
                        merge_shell_snapshot_projects(previous, snapshot, options.as_ref());
                    next.snapshot = Some(Arc::new(merged));
                }
                ShellStreamItem::Event(event) => {
                    let Some(snapshot) = next.snapshot.as_deref() else {
                        continue;
                    };
                    if event.sequence() <= snapshot.snapshot_sequence {
                        continue;
                    }
                    let Some(applied) = apply_shell_stream_event(snapshot, &event) else {
                        continue;
                    };
                    next.snapshot = Some(Arc::new(applied));
                }
            }
            next.status = if waiting {
                EnvironmentShellStatus::Synchronizing
            } else {
                EnvironmentShellStatus::Live
            };
            next.error = None;
        }
        next.awaiting_completion = waiting;
        *self = next;
        *self != initial
    }
}

/// `EnvironmentShellSummary`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvironmentShellSummary {
    pub has_snapshot: bool,
    pub has_synchronizing_shell: bool,
    pub has_cached_shell: bool,
    pub has_live_shell: bool,
    pub first_error: Option<String>,
}

pub fn summarize_shell_states<'a>(
    states: impl IntoIterator<Item = &'a EnvironmentShellState>,
) -> EnvironmentShellSummary {
    let mut summary = EnvironmentShellSummary::default();
    for state in states {
        summary.has_synchronizing_shell |= state.status == EnvironmentShellStatus::Synchronizing;
        summary.has_cached_shell |= state.status == EnvironmentShellStatus::Cached;
        summary.has_live_shell |= state.status == EnvironmentShellStatus::Live;
        if summary.first_error.is_none() {
            summary.first_error.clone_from(&state.error);
        }
        if state.snapshot.is_some() {
            summary.has_snapshot = true;
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::OrchestrationV2ShellLocation;
    use crate::state::shell_reducer::ShellStreamEvent;
    use crate::state::test_fixtures::{v2_project, v2_thread_shell};

    fn snapshot(sequence: u64) -> ShellSnapshot {
        ShellSnapshot {
            schema_version: crate::state::contracts::PositiveInt::new(1).unwrap(),
            snapshot_sequence: sequence,
            threads: Arc::new(vec![Arc::new(v2_thread_shell())]),
            archived_threads: Arc::new(Vec::new()),
            projects: Arc::new(vec![Arc::new(v2_project())]),
        }
    }

    fn snapshot_item(sequence: u64) -> ShellStreamItem {
        ShellStreamItem::Snapshot {
            snapshot: snapshot(sequence),
            resolved_repository_identity_roots: None,
        }
    }

    fn rename(sequence: u64, title: &str) -> ShellStreamItem {
        ShellStreamItem::Event(ShellStreamEvent::ThreadUpdated {
            sequence,
            location: OrchestrationV2ShellLocation::Active,
            thread: Arc::new(crate::state::contracts::OrchestrationV2ThreadShell {
                title: title.into(),
                ..v2_thread_shell()
            }),
        })
    }

    fn titles(state: &EnvironmentShellState) -> Vec<String> {
        state
            .snapshot
            .as_ref()
            .unwrap()
            .threads
            .iter()
            .map(|thread| thread.title.clone())
            .collect()
    }

    #[test]
    fn a_snapshot_goes_live_and_later_events_apply() {
        let mut state = EnvironmentShellState::default();
        assert!(state.apply_items(vec![snapshot_item(1), rename(2, "Renamed")]));
        assert_eq!(state.status, EnvironmentShellStatus::Live);
        assert_eq!(titles(&state), vec!["Renamed"]);
    }

    #[test]
    fn events_before_any_snapshot_are_dropped() {
        let mut state = EnvironmentShellState::default();
        assert!(!state.apply_items(vec![rename(2, "Renamed")]));
        assert_eq!(state, EnvironmentShellState::default());
    }

    #[test]
    fn stale_events_are_ignored() {
        let mut state = EnvironmentShellState::default();
        state.apply_items(vec![snapshot_item(5)]);
        assert!(!state.apply_items(vec![rename(5, "Stale"), rename(3, "Older")]));
        assert_eq!(titles(&state), vec!["Thread"]);
    }

    #[test]
    fn waits_for_the_completion_marker_when_one_is_expected() {
        let mut state = EnvironmentShellState::default();
        state.set_awaiting_completion(true);
        state.apply_items(vec![snapshot_item(1)]);
        assert_eq!(state.status, EnvironmentShellStatus::Synchronizing);
        state.apply_items(vec![ShellStreamItem::Synchronized]);
        assert_eq!(state.status, EnvironmentShellStatus::Live);
    }

    #[test]
    fn disconnects_and_errors_fall_back_to_cached() {
        let mut state = EnvironmentShellState::default();
        state.apply_items(vec![snapshot_item(1)]);
        state.set_stream_error();
        assert_eq!(state.status, EnvironmentShellStatus::Cached);
        assert_eq!(
            state.error.as_deref(),
            Some("Could not synchronize environment data.")
        );
        // The next frames clear the error.
        state.apply_items(vec![rename(2, "Back")]);
        assert_eq!(
            (state.status, state.error.clone()),
            (EnvironmentShellStatus::Live, None)
        );
        state.set_disconnected();
        assert_eq!(state.status, EnvironmentShellStatus::Cached);
        let mut empty = EnvironmentShellState::default();
        empty.set_disconnected();
        assert_eq!(empty.status, EnvironmentShellStatus::Empty);
    }

    #[test]
    fn a_new_authoritative_snapshot_keeps_unchanged_rows() {
        let mut state = EnvironmentShellState::default();
        state.apply_items(vec![snapshot_item(1)]);
        let before = Arc::clone(&state.snapshot.as_ref().unwrap().threads[0]);
        state.apply_items(vec![snapshot_item(2)]);
        assert!(Arc::ptr_eq(
            &state.snapshot.as_ref().unwrap().threads[0],
            &before
        ));
        assert_eq!(state.snapshot.as_ref().unwrap().snapshot_sequence, 2);
    }

    #[test]
    fn summarizes_states() {
        let mut live = EnvironmentShellState::default();
        live.apply_items(vec![snapshot_item(1)]);
        let mut errored = EnvironmentShellState::default();
        errored.set_stream_error();
        let summary = summarize_shell_states([&live, &errored]);
        assert_eq!(
            summary,
            EnvironmentShellSummary {
                has_snapshot: true,
                has_synchronizing_shell: false,
                has_cached_shell: false,
                has_live_shell: true,
                first_error: Some("Could not synchronize environment data.".into()),
            }
        );
    }
}
