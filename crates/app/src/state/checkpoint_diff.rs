//! Port of upstream/packages/client-runtime/src/state/checkpointDiff.ts
//!
//! The state shape the diff panel reads for a turn or full-thread checkpoint
//! diff. Both upstream result types are `ThreadTurnDiff`.

use turin_contracts::checkpoint_diff::ThreadTurnDiff;

use crate::state::contracts::{EnvironmentId, ThreadId};

/// `OrchestrationGetTurnDiffResult | OrchestrationGetFullThreadDiffResult`.
pub type CheckpointDiffResult = ThreadTurnDiff;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CheckpointDiffState {
    pub data: Option<CheckpointDiffResult>,
    pub error: Option<String>,
    pub is_pending: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CheckpointDiffTarget {
    pub environment_id: Option<EnvironmentId>,
    pub thread_id: Option<ThreadId>,
    pub from_turn_count: Option<u64>,
    pub to_turn_count: Option<u64>,
    pub ignore_whitespace: bool,
    pub cache_scope: Option<String>,
}
