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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::test_support::*;
    use serde_json::json;

    fn runs(specs: &[(&str, u64, &str, Option<u64>)]) -> Vec<OrchestrationV2Run> {
        specs
            .iter()
            .map(|(id, ordinal, status, position)| {
                let mut value = run("thread:a", id, *ordinal, status);
                if let Some(position) = position {
                    value["queuePosition"] = json!(position);
                }
                serde_json::from_value(value).unwrap()
            })
            .collect()
    }

    fn wake_message(run_id: &str) -> OrchestrationV2ConversationMessage {
        let mut value = message(
            "thread:a",
            &format!("message:{run_id}"),
            None,
            "user",
            "done",
            T0,
        );
        value["delegatedCompletion"] =
            json!({"parentRunId": "run:p", "generation": 1, "taskIds": ["node:t"]});
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn automatic_completions_go_first_then_queue_position_then_ordinal() {
        let all = runs(&[
            ("a", 1, "queued", Some(3)),
            ("b", 2, "queued", None),
            ("c", 3, "queued", Some(1)),
            ("d", 4, "completed", None),
            ("e", 5, "queued", Some(9)),
        ]);
        let messages = [wake_message("e")];
        let ids: Vec<_> = queued_runs_in_delivery_order(&all, &messages)
            .iter()
            .map(|run| run.id.to_string())
            .collect();
        assert_eq!(ids, ["e", "c", "b", "a"]);
        assert!(is_automatic_completion_run(&messages, &all[4]));
        assert!(!is_automatic_completion_run(&messages, &all[0]));
    }
}
