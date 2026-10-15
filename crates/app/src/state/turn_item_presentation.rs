//! Port of upstream/packages/client-runtime/src/state/turnItemPresentation.ts

use std::collections::HashSet;

use crate::state::contracts::{
    ORCHESTRATION_V2_WORKSPACE_PREPARATION_FAILURE_CODE, OrchestrationV2Run,
    OrchestrationV2RunStatus, OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus, RunId,
};

const WORKSPACE_PREPARATION_INPUT: &str = "Preparing workspace";

fn is_preparation_failure(failure_code: Option<&str>) -> bool {
    failure_code == Some(ORCHESTRATION_V2_WORKSPACE_PREPARATION_FAILURE_CODE)
}

/// Workspace setup is client bookkeeping; preparation failures have their own
/// error item. A retry cancels that item, which then has nothing left to say.
pub fn turn_item_is_workspace_preparation(item: &OrchestrationV2TurnItem) -> bool {
    match item {
        OrchestrationV2TurnItem::CommandExecution { input, .. } => {
            input == WORKSPACE_PREPARATION_INPUT
        }
        OrchestrationV2TurnItem::Error {
            status, failure, ..
        } => {
            *status == OrchestrationV2TurnItemStatus::Cancelled
                && is_preparation_failure(failure.code.as_ref().map(|code| code.as_str()))
        }
        _ => false,
    }
}

/// Runs a Retry can prepare again: their workspace preparation failed and the
/// run still ended there. Older servers record no preparation on the run, so
/// they never offer it.
pub fn workspace_preparation_retry_run_ids<'a>(
    runs: impl IntoIterator<Item = &'a OrchestrationV2Run>,
    items: impl IntoIterator<Item = &'a OrchestrationV2TurnItem>,
) -> HashSet<RunId> {
    let failed_runs: HashSet<&RunId> = runs
        .into_iter()
        .filter(|run| {
            run.status == OrchestrationV2RunStatus::Failed && run.workspace_preparation.is_some()
        })
        .map(|run| &run.id)
        .collect();
    let mut retryable = HashSet::new();
    if failed_runs.is_empty() {
        return retryable;
    }
    for item in items {
        if let OrchestrationV2TurnItem::Error {
            status,
            failure,
            run_id: Some(run_id),
            ..
        } = item
            && *status == OrchestrationV2TurnItemStatus::Failed
            && is_preparation_failure(failure.code.as_ref().map(|code| code.as_str()))
            && failed_runs.contains(run_id)
        {
            retryable.insert(run_id.clone());
        }
    }
    retryable
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::state::test_fixtures::{from_json, v2_command_item, v2_run};

    fn command(input: &str) -> OrchestrationV2TurnItem {
        let mut item =
            serde_json::to_value(v2_command_item("item-command", "run-1", "", 1)).unwrap();
        item["input"] = json!(input);
        from_json(item)
    }

    fn preparation_failure(status: &str, code: Option<&str>) -> OrchestrationV2TurnItem {
        let mut item = serde_json::to_value(v2_command_item("item-error", "run-1", "", 1)).unwrap();
        let object = item.as_object_mut().unwrap();
        for key in ["input", "output", "exitCode"] {
            object.remove(key);
        }
        object.insert("type".into(), json!("error"));
        object.insert("status".into(), json!(status));
        object.insert(
            "failure".into(),
            json!({ "class": "validation_error", "message": "fetch failed", "code": code, "retryable": false }),
        );
        from_json(item)
    }

    fn run(status: &str, prepared: bool) -> OrchestrationV2Run {
        let mut value = serde_json::to_value(v2_run("run-1", 1, status)).unwrap();
        if prepared {
            value["workspacePreparation"] = json!({ "type": "worktree", "baseRef": "main" });
        }
        from_json(value)
    }

    #[test]
    fn identifies_the_synthetic_workspace_preparation_command() {
        assert!(turn_item_is_workspace_preparation(&command(
            "Preparing workspace"
        )));
        assert!(!turn_item_is_workspace_preparation(&command(
            "prepare workspace"
        )));
    }

    #[test]
    fn hides_a_preparation_failure_only_once_a_retry_cancelled_it() {
        let code = Some(ORCHESTRATION_V2_WORKSPACE_PREPARATION_FAILURE_CODE);
        assert!(!turn_item_is_workspace_preparation(&preparation_failure(
            "failed", code
        )));
        assert!(turn_item_is_workspace_preparation(&preparation_failure(
            "cancelled",
            code
        )));
        assert!(!turn_item_is_workspace_preparation(&preparation_failure(
            "cancelled",
            None
        )));
    }

    #[test]
    fn offers_a_retry_while_the_run_still_ends_in_its_failed_preparation() {
        let code = Some(ORCHESTRATION_V2_WORKSPACE_PREPARATION_FAILURE_CODE);
        let failure = [preparation_failure("failed", code)];
        let retry = |runs: &[OrchestrationV2Run], items: &[OrchestrationV2TurnItem]| {
            workspace_preparation_retry_run_ids(runs, items)
        };
        assert_eq!(
            retry(&[run("failed", true)], &failure),
            HashSet::from([RunId::new("run-1").unwrap()])
        );
        // Retried: the run is preparing again and the old failure is cancelled.
        assert!(retry(&[run("preparing", true)], &failure).is_empty());
        // An older server records no preparation, so there is nothing to repeat.
        assert!(retry(&[run("failed", false)], &failure).is_empty());
        // A provider error on a run that did reach the provider is not a preparation failure.
        assert!(
            retry(
                &[run("failed", true)],
                &[preparation_failure("failed", Some("provider_crashed"))]
            )
            .is_empty()
        );
    }
}
