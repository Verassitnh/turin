//! Port of upstream/packages/client-runtime/src/state/turnItemPresentation.ts
//! (`turnItemIsWorkspacePreparation` only; `workspacePreparationRetryRunIds`
//! serves the retry action, which lands with the composer).

use super::turn_item::{TurnItem, TurnItemPayload, TurnItemStatus};

/// `ORCHESTRATION_V2_WORKSPACE_PREPARATION_FAILURE_CODE`.
pub const ORCHESTRATION_V2_WORKSPACE_PREPARATION_FAILURE_CODE: &str =
    "workspace_preparation_failed";

const WORKSPACE_PREPARATION_INPUT: &str = "Preparing workspace";

/// Workspace setup is client bookkeeping; preparation failures have their own
/// error item. A retry cancels that item, which then has nothing left to say.
pub fn turn_item_is_workspace_preparation(item: &TurnItem) -> bool {
    match &item.payload {
        TurnItemPayload::CommandExecution { input, .. } => input == WORKSPACE_PREPARATION_INPUT,
        TurnItemPayload::Error { failure, .. } => {
            item.status == TurnItemStatus::Cancelled
                && failure.code.as_deref()
                    == Some(ORCHESTRATION_V2_WORKSPACE_PREPARATION_FAILURE_CODE)
        }
        _ => false,
    }
}
