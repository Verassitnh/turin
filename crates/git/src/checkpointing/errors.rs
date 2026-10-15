//! Port of upstream/apps/server/src/checkpointing/Errors.ts
//!
//! These tagged errors are not contract types. `CheckpointServiceError` in
//! upstream also includes `ProjectionRepositoryError`; persistence is outside
//! this crate, and `getCheckpointContext` failures are mapped to
//! [`CheckpointThreadNotFoundError`] before they surface.

use turin_contracts::base_schemas::{NonNegativeInt, ThreadId};
use turin_contracts::vcs::VcsError;

/// `CheckpointDiffOperation`: the two literal operation names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckpointDiffOperation {
    GetTurnDiff,
    GetFullThreadDiff,
}

impl CheckpointDiffOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GetTurnDiff => "CheckpointDiffQuery.getTurnDiff",
            Self::GetFullThreadDiff => "CheckpointDiffQuery.getFullThreadDiff",
        }
    }

    const fn diff_noun(self) -> &'static str {
        match self {
            Self::GetTurnDiff => "turn diff",
            Self::GetFullThreadDiff => "full thread diff",
        }
    }
}

impl std::fmt::Display for CheckpointDiffOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which side of a turn range is missing. Not part of the Display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckpointEndpoint {
    From,
    To,
}

impl CheckpointEndpoint {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::From => "from",
            Self::To => "to",
        }
    }
}

/// The computed result does not satisfy the checkpoint RPC contract.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "Checkpoint invariant violation in {operation}: Computed {result} result does not satisfy contract schema.",
    result = .operation.diff_noun()
)]
pub struct CheckpointDiffResultInvalidError {
    pub operation: CheckpointDiffOperation,
    pub thread_id: ThreadId,
}

/// Projection state no longer contains the requested checkpoint thread.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Checkpoint invariant violation in {operation}: Thread '{thread_id}' not found.")]
pub struct CheckpointThreadNotFoundError {
    pub operation: CheckpointDiffOperation,
    pub thread_id: ThreadId,
}

/// The checkpoint thread has no workspace path from which to compute a diff.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "Checkpoint invariant violation in {operation}: Workspace path missing for thread '{thread_id}' when computing {diff}.",
    diff = .operation.diff_noun()
)]
pub struct CheckpointWorkspacePathMissingError {
    pub operation: CheckpointDiffOperation,
    pub thread_id: ThreadId,
}

/// The requested turn lies beyond the latest available checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "Checkpoint unavailable for thread {thread_id} turn {requested_turn_count}: Turn diff range exceeds current turn count: requested {requested_turn_count}, current {available_turn_count}."
)]
pub struct CheckpointTurnRangeUnavailableError {
    pub operation: CheckpointDiffOperation,
    pub thread_id: ThreadId,
    pub requested_turn_count: NonNegativeInt,
    pub available_turn_count: NonNegativeInt,
}

/// Expected checkpoint metadata does not contain the requested Git ref.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "Checkpoint unavailable for thread {thread_id} turn {turn_count}: Checkpoint ref is unavailable for turn {turn_count}."
)]
pub struct CheckpointRefUnavailableError {
    pub operation: CheckpointDiffOperation,
    pub thread_id: ThreadId,
    pub turn_count: NonNegativeInt,
    pub checkpoint: CheckpointEndpoint,
}

/// `CheckpointStoreError`.
pub type CheckpointStoreError = VcsError;

/// Store failures plus the checkpoint diff invariant errors.
///
/// `VcsError` stays unboxed so callers can match the contract variant.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[allow(clippy::large_enum_variant)]
pub enum CheckpointServiceError {
    #[error(transparent)]
    Store(#[from] VcsError),
    #[error(transparent)]
    ResultInvalid(#[from] CheckpointDiffResultInvalidError),
    #[error(transparent)]
    ThreadNotFound(#[from] CheckpointThreadNotFoundError),
    #[error(transparent)]
    WorkspacePathMissing(#[from] CheckpointWorkspacePathMissingError),
    #[error(transparent)]
    TurnRangeUnavailable(#[from] CheckpointTurnRangeUnavailableError),
    #[error(transparent)]
    RefUnavailable(#[from] CheckpointRefUnavailableError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread() -> ThreadId {
        ThreadId::new("thread-1").unwrap()
    }

    #[test]
    fn operation_names_are_the_two_literals() {
        assert_eq!(
            CheckpointDiffOperation::GetTurnDiff.as_str(),
            "CheckpointDiffQuery.getTurnDiff"
        );
        assert_eq!(
            CheckpointDiffOperation::GetFullThreadDiff.as_str(),
            "CheckpointDiffQuery.getFullThreadDiff"
        );
        assert_eq!(CheckpointEndpoint::From.as_str(), "from");
        assert_eq!(CheckpointEndpoint::To.as_str(), "to");
    }

    #[test]
    fn display_matches_the_typescript_getters() {
        let thread_id = thread();
        assert_eq!(
            CheckpointDiffResultInvalidError {
                operation: CheckpointDiffOperation::GetTurnDiff,
                thread_id: thread_id.clone(),
            }
            .to_string(),
            "Checkpoint invariant violation in CheckpointDiffQuery.getTurnDiff: Computed turn diff result does not satisfy contract schema."
        );
        assert_eq!(
            CheckpointDiffResultInvalidError {
                operation: CheckpointDiffOperation::GetFullThreadDiff,
                thread_id: thread_id.clone(),
            }
            .to_string(),
            "Checkpoint invariant violation in CheckpointDiffQuery.getFullThreadDiff: Computed full thread diff result does not satisfy contract schema."
        );
        assert_eq!(
            CheckpointThreadNotFoundError {
                operation: CheckpointDiffOperation::GetTurnDiff,
                thread_id: thread_id.clone(),
            }
            .to_string(),
            "Checkpoint invariant violation in CheckpointDiffQuery.getTurnDiff: Thread 'thread-1' not found."
        );
        assert_eq!(
            CheckpointWorkspacePathMissingError {
                operation: CheckpointDiffOperation::GetFullThreadDiff,
                thread_id: thread_id.clone(),
            }
            .to_string(),
            "Checkpoint invariant violation in CheckpointDiffQuery.getFullThreadDiff: Workspace path missing for thread 'thread-1' when computing full thread diff."
        );
        assert_eq!(
            CheckpointTurnRangeUnavailableError {
                operation: CheckpointDiffOperation::GetTurnDiff,
                thread_id: thread_id.clone(),
                requested_turn_count: 4,
                available_turn_count: 2,
            }
            .to_string(),
            "Checkpoint unavailable for thread thread-1 turn 4: Turn diff range exceeds current turn count: requested 4, current 2."
        );
        assert_eq!(
            CheckpointRefUnavailableError {
                operation: CheckpointDiffOperation::GetTurnDiff,
                thread_id,
                turn_count: 1,
                checkpoint: CheckpointEndpoint::From,
            }
            .to_string(),
            "Checkpoint unavailable for thread thread-1 turn 1: Checkpoint ref is unavailable for turn 1."
        );
    }
}
