//! Port of upstream/apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts:
//! `nextClaudeGoal` (~1299) and its prefixes (~1292). In SDK mode Claude reports
//! `/goal` only through the transcript: synthetic command output names the goal,
//! and each unmet evaluator check returns as Stop hook feedback.
//!
//! The goal is the thread's `goal`, a mirror of the latest native report. A
//! completed goal has no frame of its own; the adapter settles it at the end of
//! a turn (see `claude_provider_adapter.rs`).
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};
use turin_contracts::{
    js_trim,
    orchestration_v2::{OrchestrationV2ProviderGoal, OrchestrationV2ProviderGoalStatus},
};

const GOAL_SET_PREFIX: &str = "Goal set: ";
const GOAL_CLEARED_PREFIX: &str = "Goal cleared: ";
const NO_GOAL_PREFIX: &str = "No goal set";

static GOAL_ACTIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Goal active: ([\s\S]+?) \((?:not yet evaluated|(\d+) turns?)\)")
        .expect("valid goal pattern")
});

/// Upstream `textFromClaudeContent`: the text parts of a message, joined.
fn text_of(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

fn goal(
    objective: &str,
    status: OrchestrationV2ProviderGoalStatus,
    checks: u64,
) -> OrchestrationV2ProviderGoal {
    serde_json::from_value(json!({
        "objective": objective,
        "status": status,
        "checks": checks,
    }))
    .expect("goal matches the contract shape")
}

/// The goal after one root frame, or `None` when the frame says nothing about
/// it. `Some(None)` clears the goal. The frame is a root frame: subagent frames
/// are the caller's to skip.
pub fn next_claude_goal(
    current: Option<&OrchestrationV2ProviderGoal>,
    frame: &Value,
) -> Option<Option<OrchestrationV2ProviderGoal>> {
    match frame.get("type").and_then(Value::as_str)? {
        "assistant" => {
            let message = frame.get("message")?;
            if message.get("model").and_then(Value::as_str) == Some("<synthetic>") {
                return assistant_goal(current, js_trim(&text_of(message.get("content")?)));
            }
            None
        }
        "user" => {
            if frame.get("isSynthetic").and_then(Value::as_bool) != Some(true) {
                return None;
            }
            let current = current?;
            let prefix = format!("Stop hook feedback:\n[{}]: ", current.objective.as_str());
            let text = text_of(frame.get("message")?.get("content")?);
            let last_check = text.strip_prefix(&prefix)?;
            let mut next = current.clone();
            next.status = OrchestrationV2ProviderGoalStatus::Active;
            next.checks = Some(current.checks.unwrap_or(0) + 1);
            next.last_check = Some(js_trim(last_check).to_owned());
            Some(Some(next))
        }
        _ => None,
    }
}

/// The synthetic assistant text that names the goal.
fn assistant_goal(
    current: Option<&OrchestrationV2ProviderGoal>,
    text: &str,
) -> Option<Option<OrchestrationV2ProviderGoal>> {
    if let Some(objective) = text.strip_prefix(GOAL_SET_PREFIX) {
        let objective = js_trim(objective);
        return (!objective.is_empty()).then(|| {
            Some(goal(
                objective,
                OrchestrationV2ProviderGoalStatus::Active,
                0,
            ))
        });
    }
    if text.starts_with(GOAL_CLEARED_PREFIX) || text.starts_with(NO_GOAL_PREFIX) {
        return Some(None);
    }
    let active = GOAL_ACTIVE.captures(text)?;
    let objective = active.get(1)?.as_str();
    let checks: u64 = active
        .get(2)
        .and_then(|turns| turns.as_str().parse().ok())
        .unwrap_or(0);
    // The same objective keeps the record it has, its last check included.
    let mut next = match current.filter(|current| current.objective.as_str() == objective) {
        Some(current) => current.clone(),
        None => goal(objective, OrchestrationV2ProviderGoalStatus::Active, checks),
    };
    next.status = OrchestrationV2ProviderGoalStatus::Active;
    next.checks = Some(checks);
    Some(Some(next))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(text: &str) -> Value {
        json!({"type": "assistant", "message": {"model": "<synthetic>", "content": [{"type": "text", "text": text}]}})
    }

    #[test]
    fn a_goal_set_or_cleared_by_command_output_is_read_from_the_text() {
        let set = next_claude_goal(None, &assistant("Goal set: ship the parser  "))
            .unwrap()
            .unwrap();
        assert_eq!(set.objective.as_str(), "ship the parser");
        assert_eq!(set.status, OrchestrationV2ProviderGoalStatus::Active);
        assert_eq!(set.checks, Some(0));
        assert!(next_claude_goal(None, &assistant("Goal set:   ")).is_none());
        assert_eq!(
            next_claude_goal(Some(&set), &assistant("Goal cleared: done")),
            Some(None)
        );
        assert_eq!(
            next_claude_goal(Some(&set), &assistant("No goal set.")),
            Some(None)
        );
    }

    #[test]
    fn goal_active_counts_turns_and_keeps_the_same_objectives_record() {
        let current = goal("ship it", OrchestrationV2ProviderGoalStatus::Active, 1);
        let evaluated =
            next_claude_goal(Some(&current), &assistant("Goal active: ship it (3 turns)"))
                .unwrap()
                .unwrap();
        assert_eq!(evaluated.checks, Some(3));
        let fresh = next_claude_goal(
            Some(&current),
            &assistant("Goal active: other (not yet evaluated)"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(fresh.objective.as_str(), "other");
        assert_eq!(fresh.checks, Some(0));
        assert!(next_claude_goal(None, &assistant("Just prose.")).is_none());
    }

    #[test]
    fn an_unmet_check_is_stop_hook_feedback_for_the_current_goal() {
        let current = goal("ship it", OrchestrationV2ProviderGoalStatus::Active, 2);
        let feedback = json!({
            "type": "user", "isSynthetic": true,
            "message": {"content": "Stop hook feedback:\n[ship it]: tests are red\n"},
        });
        let next = next_claude_goal(Some(&current), &feedback)
            .unwrap()
            .unwrap();
        assert_eq!(next.checks, Some(3));
        assert_eq!(next.last_check.as_deref(), Some("tests are red"));
        // Not synthetic, or for another goal, says nothing.
        let real =
            json!({"type": "user", "message": {"content": "Stop hook feedback:\n[ship it]: x"}});
        assert!(next_claude_goal(Some(&current), &real).is_none());
        assert!(next_claude_goal(None, &feedback).is_none());
    }
}
