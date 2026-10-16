//! Port of upstream/apps/web/src/hooks/useTurnDiffSummaries.ts

#![allow(dead_code)]

use std::collections::HashMap;

use crate::components::chat::timeline::session_logic::infer_checkpoint_turn_count_by_run_id;
use crate::components::chat::timeline::turn_item::CheckpointFileSummary;
use crate::components::chat::timeline::types::{CheckpointStatus, TurnDiffSummary};
use crate::state::contracts::OrchestrationV2CheckpointStatus;
use crate::state::thread_checkpoints::{
    ThreadCheckpointSummary, derive_thread_checkpoint_summaries,
};
use crate::state::thread_projection::ThreadProjection;

fn checkpoint_status(status: OrchestrationV2CheckpointStatus) -> CheckpointStatus {
    match status {
        OrchestrationV2CheckpointStatus::Ready => CheckpointStatus::Ready,
        OrchestrationV2CheckpointStatus::Missing => CheckpointStatus::Missing,
        OrchestrationV2CheckpointStatus::Error => CheckpointStatus::Error,
        OrchestrationV2CheckpointStatus::Stale => CheckpointStatus::Stale,
    }
}

pub struct TurnDiffSummariesView {
    pub turn_diff_summaries: Vec<TurnDiffSummary>,
    pub inferred_checkpoint_turn_count_by_run_id: HashMap<String, u64>,
}

fn to_turn_diff_summary(summary: ThreadCheckpointSummary) -> TurnDiffSummary {
    TurnDiffSummary {
        checkpoint_id: Some(summary.checkpoint_id.as_str().to_owned()),
        scope_id: Some(summary.scope_id.as_str().to_owned()),
        run_id: summary.run_id.as_str().to_owned(),
        checkpoint_turn_count: summary.checkpoint_turn_count,
        checkpoint_ref: summary.checkpoint_ref.as_str().to_owned(),
        status: checkpoint_status(summary.status),
        files: summary
            .files
            .into_iter()
            .map(|file| CheckpointFileSummary {
                path: file.path.as_str().to_owned(),
                kind: file.kind.as_str().to_owned(),
                additions: file.additions,
                deletions: file.deletions,
            })
            .collect(),
        assistant_message_id: summary
            .assistant_message_id
            .map(|id| id.as_str().to_owned()),
        completed_at: summary.completed_at,
    }
}

pub fn use_turn_diff_summaries(projection: Option<&ThreadProjection>) -> TurnDiffSummariesView {
    let turn_diff_summaries: Vec<TurnDiffSummary> = projection
        .map(|projection| {
            derive_thread_checkpoint_summaries(projection)
                .into_iter()
                .map(to_turn_diff_summary)
                .collect()
        })
        .unwrap_or_default();
    let inferred_checkpoint_turn_count_by_run_id =
        infer_checkpoint_turn_count_by_run_id(&turn_diff_summaries);
    TurnDiffSummariesView {
        turn_diff_summaries,
        inferred_checkpoint_turn_count_by_run_id,
    }
}
