//! Port of upstream/apps/web/src/lib/checkpointDiffState.ts (`useCheckpointDiff`), and the
//! checkpoint query the diff panel makes through it (`DiffPanel.tsx`'s `activeCheckpointDiff`).
//!
//! The query is a seam: [`use_checkpoint_diff`] only narrows what a query answers to the state the
//! panel reads. [`checkpoint_diff_query`] is the query the panel's `DiffSource` answers it with.

#![allow(dead_code)]

use crate::diff::view_model::Loadable;
use crate::state::checkpoint_diff::{CheckpointDiffState, CheckpointDiffTarget};
use turin_contracts::checkpoint_diff::ThreadTurnDiff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UseCheckpointDiffOptions {
    pub enabled: bool,
}

impl Default for UseCheckpointDiffOptions {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl UseCheckpointDiffOptions {
    pub fn enabled(enabled: bool) -> Self {
        Self { enabled }
    }
}

/// `useCheckpointDiffQuery`: the read behind the hook.
pub type UseCheckpointDiffQuery<'a> =
    &'a dyn Fn(&CheckpointDiffTarget, UseCheckpointDiffOptions) -> CheckpointDiffState;

/// `useCheckpointDiff`: the query's data, error and pending flag, nothing else.
pub fn use_checkpoint_diff(
    query: UseCheckpointDiffQuery<'_>,
    target: &CheckpointDiffTarget,
    options: Option<UseCheckpointDiffOptions>,
) -> CheckpointDiffState {
    let state = query(target, options.unwrap_or_default());
    CheckpointDiffState {
        data: state.data,
        error: state.error,
        is_pending: state.is_pending,
    }
}

/// What the checkpoint query answers from a source that reads a turn range's patch: a disabled
/// query, or one with no thread or range to ask about, has nothing and is not pending.
pub fn checkpoint_diff_query(
    target: &CheckpointDiffTarget,
    options: UseCheckpointDiffOptions,
    read_patch: impl FnOnce(u64, u64, bool) -> Loadable<String>,
) -> CheckpointDiffState {
    let (true, Some(thread_id), Some(from), Some(to)) = (
        options.enabled,
        target.thread_id.as_ref(),
        target.from_turn_count,
        target.to_turn_count,
    ) else {
        return CheckpointDiffState::default();
    };
    match read_patch(from, to, target.ignore_whitespace) {
        Loadable::Pending => CheckpointDiffState {
            is_pending: true,
            ..CheckpointDiffState::default()
        },
        Loadable::Failed(message) => CheckpointDiffState {
            error: Some(message),
            ..CheckpointDiffState::default()
        },
        Loadable::Ready(diff) => CheckpointDiffState {
            data: Some(ThreadTurnDiff {
                from_turn_count: from,
                to_turn_count: to,
                thread_id: thread_id.clone(),
                diff,
            }),
            ..CheckpointDiffState::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::models::fixtures::{env, thread_id};

    fn target(from: Option<u64>, to: Option<u64>) -> CheckpointDiffTarget {
        CheckpointDiffTarget {
            environment_id: Some(env("local")),
            thread_id: Some(thread_id("thread-1")),
            from_turn_count: from,
            to_turn_count: to,
            ignore_whitespace: true,
            cache_scope: Some("turn:run-1".to_owned()),
        }
    }

    #[test]
    fn a_ready_patch_becomes_the_turn_diff_of_the_range() {
        let state = checkpoint_diff_query(
            &target(Some(1), Some(2)),
            UseCheckpointDiffOptions::default(),
            |from, to, ignore_whitespace| {
                assert_eq!((from, to, ignore_whitespace), (1, 2, true));
                Loadable::Ready("diff --git a/x b/x\n".to_owned())
            },
        );
        let data = state.data.expect("data");
        assert_eq!((data.from_turn_count, data.to_turn_count), (1, 2));
        assert_eq!(data.diff, "diff --git a/x b/x\n");
        assert!(!state.is_pending);
        assert_eq!(state.error, None);
    }

    #[test]
    fn a_pending_or_failed_read_says_so() {
        let options = UseCheckpointDiffOptions::default();
        let pending = checkpoint_diff_query(&target(Some(0), Some(1)), options, |_, _, _| {
            Loadable::Pending
        });
        assert!(pending.is_pending && pending.data.is_none());
        let failed = checkpoint_diff_query(&target(Some(0), Some(1)), options, |_, _, _| {
            Loadable::Failed("no checkpoint".to_owned())
        });
        assert_eq!(failed.error.as_deref(), Some("no checkpoint"));
        assert!(!failed.is_pending);
    }

    #[test]
    fn a_disabled_query_or_one_without_a_range_reads_nothing() {
        let never = |_: u64, _: u64, _: bool| -> Loadable<String> { panic!("must not read") };
        assert_eq!(
            checkpoint_diff_query(
                &target(Some(0), Some(1)),
                UseCheckpointDiffOptions::enabled(false),
                never
            ),
            CheckpointDiffState::default()
        );
        assert_eq!(
            checkpoint_diff_query(
                &target(None, None),
                UseCheckpointDiffOptions::default(),
                never
            ),
            CheckpointDiffState::default()
        );
    }

    #[test]
    fn the_hook_keeps_only_data_error_and_pending() {
        let state = use_checkpoint_diff(
            &|_, options| {
                assert!(options.enabled);
                CheckpointDiffState {
                    is_pending: true,
                    ..CheckpointDiffState::default()
                }
            },
            &target(Some(0), Some(1)),
            None,
        );
        assert!(state.is_pending);
    }
}
