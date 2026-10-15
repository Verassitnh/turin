//! Port of upstream/apps/server/src/orchestration-v2/QueuedRunOrder.ts

use std::collections::HashSet;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ConversationMessage, OrchestrationV2Run, OrchestrationV2RunStatus,
};

pub fn is_automatic_completion_run(
    messages: &[OrchestrationV2ConversationMessage],
    run: &OrchestrationV2Run,
) -> bool {
    messages
        .iter()
        .any(|message| message.id == run.user_message_id && message.delegated_completion.is_some())
}

pub fn queued_runs_in_delivery_order<'a>(
    runs: &'a [OrchestrationV2Run],
    messages: &[OrchestrationV2ConversationMessage],
) -> Vec<&'a OrchestrationV2Run> {
    let automatic: HashSet<_> = messages
        .iter()
        .filter(|message| message.delegated_completion.is_some())
        .map(|message| &message.id)
        .collect();
    let mut queued: Vec<_> = runs
        .iter()
        .filter(|run| run.status == OrchestrationV2RunStatus::Queued)
        .collect();
    queued.sort_by_key(|run| {
        let position = run
            .queue_position
            .flatten()
            .map(|p| p.get())
            .unwrap_or(run.ordinal.get());
        (
            !automatic.contains(&run.user_message_id),
            position,
            run.ordinal.get(),
        )
    });
    queued
}
