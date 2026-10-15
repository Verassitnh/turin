//! Port of upstream/packages/client-runtime/src/state/threadCheckpoints.ts

use crate::state::contracts::{
    CheckpointId, CheckpointRef, CheckpointScopeId, MessageId,
    OrchestrationV2CheckpointFileSummary, OrchestrationV2CheckpointStatus,
    OrchestrationV2ConversationMessageRole, RunId,
};
use crate::state::thread_projection::ThreadProjection;
use crate::state::time::iso;

#[derive(Clone, Debug, PartialEq)]
pub struct ThreadCheckpointSummary {
    pub checkpoint_id: CheckpointId,
    pub scope_id: CheckpointScopeId,
    pub run_id: RunId,
    pub checkpoint_turn_count: u64,
    pub checkpoint_ref: CheckpointRef,
    pub status: OrchestrationV2CheckpointStatus,
    pub files: Vec<OrchestrationV2CheckpointFileSummary>,
    pub assistant_message_id: Option<MessageId>,
    pub completed_at: String,
}

/// Derives the checkpoint/diff rows needed by review UIs from native V2 entities.
pub fn derive_thread_checkpoint_summaries(
    projection: &ThreadProjection,
) -> Vec<ThreadCheckpointSummary> {
    projection
        .checkpoints
        .iter()
        .filter_map(|checkpoint| {
            let (Some(app_run_ordinal), Some(run_id)) =
                (checkpoint.app_run_ordinal, checkpoint.run_id.as_ref())
            else {
                return None;
            };
            let assistant_message_id = projection
                .messages
                .iter()
                .rfind(|message| {
                    message.run_id.as_ref() == Some(run_id)
                        && message.role == OrchestrationV2ConversationMessageRole::Assistant
                })
                .map(|message| message.id.clone());
            Some(ThreadCheckpointSummary {
                checkpoint_id: checkpoint.id.clone(),
                scope_id: checkpoint.scope_id.clone(),
                run_id: run_id.clone(),
                checkpoint_turn_count: app_run_ordinal.get(),
                checkpoint_ref: checkpoint.ref_.clone(),
                status: checkpoint.status,
                files: checkpoint.files.clone(),
                assistant_message_id,
                completed_at: iso(&checkpoint.captured_at),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fixtures::{from_json, v2_message, v2_projection};
    use serde_json::json;

    const NOW: &str = "2026-06-20T00:00:00.000Z";

    fn checkpoint(
        id: &str,
        run_id: Option<&str>,
        app_run_ordinal: Option<u64>,
    ) -> serde_json::Value {
        json!({
            "id": id, "threadId": "thread-v2", "scopeId": "scope-1", "runId": run_id, "nodeId": "node-1",
            "parentCheckpointId": null, "ordinalWithinScope": 1, "appRunOrdinal": app_run_ordinal,
            "ref": "refs/t3/checkpoints/x", "status": "ready", "files": [], "capturedAt": NOW,
        })
    }

    #[test]
    fn derives_rows_only_for_checkpoints_that_belong_to_a_run() {
        let mut contract = v2_projection();
        contract.checkpoints = vec![
            from_json(checkpoint("c0", None, None)),
            from_json(checkpoint("c1", Some("run-1"), Some(1))),
        ];
        let mut assistant = serde_json::to_value(v2_message("m1", "answer")).unwrap();
        assistant["role"] = json!("assistant");
        assistant["runId"] = json!("run-1");
        contract.messages = vec![from_json(assistant)];
        let rows = derive_thread_checkpoint_summaries(&contract.into());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].checkpoint_id.as_str(), "c1");
        assert_eq!(rows[0].checkpoint_turn_count, 1);
        assert_eq!(
            rows[0].assistant_message_id.as_ref().map(|id| id.as_str()),
            Some("m1")
        );
        assert_eq!(rows[0].completed_at, NOW);
    }
}
