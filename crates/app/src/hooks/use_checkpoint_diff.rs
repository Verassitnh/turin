//! Port of upstream/apps/web/src/lib/checkpointDiffState.ts

#![allow(dead_code)]

use crate::state::checkpoint_diff::{CheckpointDiffState, CheckpointDiffTarget};

pub type UseCheckpointDiffQuery =
    fn(CheckpointDiffTarget, Option<UseCheckpointDiffOptions>) -> CheckpointDiffState;

#[derive(Clone, Copy, Debug, Default)]
pub struct UseCheckpointDiffOptions {
    pub enabled: bool,
}

impl UseCheckpointDiffOptions {
    pub fn enabled(enabled: bool) -> Self {
        Self { enabled }
    }
}

/// Thin wrapper over the checkpoint diff query seam (upstream `useCheckpointDiffQuery`).
pub fn use_checkpoint_diff(
    query: UseCheckpointDiffQuery,
    target: CheckpointDiffTarget,
    options: Option<UseCheckpointDiffOptions>,
) -> CheckpointDiffState {
    let state = query(
        target,
        Some(options.unwrap_or(UseCheckpointDiffOptions { enabled: true })),
    );
    CheckpointDiffState {
        data: state.data,
        error: state.error,
        is_pending: state.is_pending,
    }
}
