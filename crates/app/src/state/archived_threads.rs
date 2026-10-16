//! Port of upstream/packages/client-runtime/src/state/archivedThreads.ts
//! Also ports upstream/apps/web/src/lib/archivedThreadsState.ts.
//!
//! The archived-thread list is just another snapshot the event source hands
//! over (there is no per-environment query to fail or wait on), so only the
//! key helpers and the presentation shape remain.

use crate::state::contracts::{EnvironmentId, OrchestrationV2ArchivedShellSnapshot};

#[derive(Clone, Debug, PartialEq)]
pub struct ArchivedSnapshotEntry {
    pub environment_id: EnvironmentId,
    pub snapshot: OrchestrationV2ArchivedShellSnapshot,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArchivedThreadSnapshotsState {
    pub snapshots: Vec<ArchivedSnapshotEntry>,
    pub error: Option<String>,
    pub is_loading: bool,
}

/// Shown for any archived-snapshot failure; the cause can carry credentials.
pub const ARCHIVED_THREADS_ERROR_MESSAGE: &str = "Failed to load archived threads.";

const ARCHIVED_THREADS_ENVIRONMENT_KEY_SEPARATOR: char = '\u{1f}';

pub fn make_archived_threads_environment_key(environment_ids: &[EnvironmentId]) -> String {
    let mut sorted: Vec<&str> = environment_ids.iter().map(EnvironmentId::as_str).collect();
    // `Order.String`: UTF-16 code unit order.
    sorted.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    sorted.join(&ARCHIVED_THREADS_ENVIRONMENT_KEY_SEPARATOR.to_string())
}

pub fn parse_archived_threads_environment_key(key: &str) -> Vec<EnvironmentId> {
    if key.is_empty() {
        return Vec::new();
    }
    key.split(ARCHIVED_THREADS_ENVIRONMENT_KEY_SEPARATOR)
        .filter_map(|environment_id| EnvironmentId::new(environment_id).ok())
        .collect()
}

/// Folds per-environment results into the state the archived list renders.
/// A failure never exposes its message.
pub fn archived_thread_snapshots_state<E>(
    environment_ids: &[EnvironmentId],
    mut result_of: impl FnMut(&EnvironmentId) -> ArchivedSnapshotResult<E>,
) -> ArchivedThreadSnapshotsState {
    let mut state = ArchivedThreadSnapshotsState::default();
    for environment_id in environment_ids {
        let result = result_of(environment_id);
        state.is_loading |= result.waiting;
        if let Some(snapshot) = result.snapshot {
            state.snapshots.push(ArchivedSnapshotEntry {
                environment_id: environment_id.clone(),
                snapshot,
            });
        }
        if state.error.is_none() && result.failure.is_some() {
            state.error = Some(ARCHIVED_THREADS_ERROR_MESSAGE.to_owned());
        }
    }
    state
}

/// `AsyncResult<OrchestrationV2ArchivedShellSnapshot, E>` flattened.
pub struct ArchivedSnapshotResult<E> {
    pub snapshot: Option<OrchestrationV2ArchivedShellSnapshot>,
    pub waiting: bool,
    pub failure: Option<E>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_environment_keys_in_sorted_order() {
        let env_a = EnvironmentId::new("env-a").unwrap();
        let env_b = EnvironmentId::new("env-b").unwrap();
        let key = make_archived_threads_environment_key(&[env_b.clone(), env_a.clone()]);
        assert_eq!(
            parse_archived_threads_environment_key(&key),
            vec![env_a, env_b]
        );
        assert_eq!(
            parse_archived_threads_environment_key(""),
            Vec::<EnvironmentId>::new()
        );
    }

    #[test]
    fn does_not_expose_an_archived_snapshot_failure_message() {
        let environment_id = EnvironmentId::new("env-sensitive").unwrap();
        let state =
            archived_thread_snapshots_state(&[environment_id], |_| ArchivedSnapshotResult {
                snapshot: None,
                waiting: false,
                failure: Some("credential=secret-value".to_owned()),
            });
        assert_eq!(
            state,
            ArchivedThreadSnapshotsState {
                snapshots: vec![],
                error: Some("Failed to load archived threads.".into()),
                is_loading: false,
            }
        );
    }
}
