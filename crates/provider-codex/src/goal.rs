//! Port of the goal half of upstream `CodexAdapterV2.ts`: Codex's native thread goal
//! (`thread/goal/updated`, `thread/goal/cleared`) and the copy of it that each root
//! provider thread carries (`emitGoalUpdate`). A root turn that completes while its
//! goal is active is held: Codex starts the next goal turn itself, so the run stays
//! open for it. The hold settles when that turn starts, when the goal stops being
//! active, or when the continuation grace period ends.
use crate::{
    items,
    orchestration_adapter::driver,
    projection::{RuntimeState, TurnState},
};
use serde_json::Value;
use std::time::Duration;
use tokio::time::Instant;
use turin_contracts::base_schemas::{is_js_whitespace, js_trim};
use turin_contracts::{
    NonNegativeInt, TrimmedNonEmptyString, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2ProviderGoal, OrchestrationV2ProviderGoalStatus,
        OrchestrationV2ProviderThread,
    },
};
use turin_provider_core::provider_adapter::{ProviderAdapterV2Event, ProviderAdapterV2TurnInput};

/// Upstream `CODEX_GOAL_CONTINUATION_GRACE`: how long a held run waits for the
/// next goal turn before it settles, in case Codex declines to start one.
pub const CONTINUATION_GRACE: Duration = Duration::from_secs(5);

/// A root turn held for its goal. Its completion and terminal are delivered when
/// the hold resolves: a continuation turn drops the terminal and keeps the run.
pub struct GoalHold {
    /// The held turn's input. The continuation turn runs under it, one turn later.
    pub input: ProviderAdapterV2TurnInput,
    /// The held turn's completed provider turn update.
    pub completed: ProviderAdapterV2Event,
    /// The held turn's completed terminal.
    pub terminal: ProviderAdapterV2Event,
    pub deadline: Instant,
}

/// Upstream `providerGoalFromCodex`: a goal with no objective is no goal.
pub fn provider_goal_from_codex(goal: &Value) -> Option<OrchestrationV2ProviderGoal> {
    let objective = js_trim(goal.get("objective")?.as_str()?);
    if objective.is_empty() {
        return None;
    }
    let status = match goal.get("status")?.as_str()? {
        "active" => OrchestrationV2ProviderGoalStatus::Active,
        "paused" => OrchestrationV2ProviderGoalStatus::Paused,
        "blocked" => OrchestrationV2ProviderGoalStatus::Blocked,
        "usageLimited" => OrchestrationV2ProviderGoalStatus::UsageLimited,
        "budgetLimited" => OrchestrationV2ProviderGoalStatus::BudgetLimited,
        "complete" => OrchestrationV2ProviderGoalStatus::Complete,
        _ => return None,
    };
    let count = |key: &str| {
        goal.get(key)
            .and_then(Value::as_u64)
            .map(|count| count as NonNegativeInt)
    };
    Some(OrchestrationV2ProviderGoal {
        objective: TrimmedNonEmptyString::new(objective).ok()?,
        status,
        tokens_used: Some(count("tokensUsed").unwrap_or(0)),
        token_budget: Some(count("tokenBudget")),
        time_used_seconds: count("timeUsedSeconds"),
        checks: None,
        last_check: None,
    })
}

/// `thread/goal/updated` and `thread/goal/cleared`.
pub fn goal_event(
    state: &mut RuntimeState,
    method: &str,
    params: &Value,
) -> Vec<ProviderAdapterV2Event> {
    let thread = params
        .get("threadId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (goal, inactive) = if method == "thread/goal/cleared" {
        (None, true)
    } else {
        let goal = params.get("goal");
        let raw_status = goal
            .and_then(|goal| goal.get("status"))
            .and_then(Value::as_str);
        (
            goal.and_then(provider_goal_from_codex),
            raw_status != Some("active"),
        )
    };
    state.goals.insert(thread.to_owned(), goal);
    let mut events = emit_goal_update(state, thread);
    if inactive {
        events.extend(release_hold(state, thread));
    }
    events
}

/// Upstream `emitGoalUpdate`: writes the thread's native goal onto its root provider
/// thread, when the goal changed. Nothing is written before the thread's first turn.
fn emit_goal_update(state: &mut RuntimeState, thread: &str) -> Vec<ProviderAdapterV2Event> {
    let Some(goal) = state.goals.get(thread).cloned() else {
        return vec![];
    };
    let Some(provider_thread) = state.goal_threads.get(thread) else {
        return vec![];
    };
    if provider_thread.goal == goal {
        return vec![];
    }
    let mut updated: OrchestrationV2ProviderThread = provider_thread.clone();
    updated.goal = goal;
    updated.updated_at = turin_contracts::UtcDateTime::now();
    state
        .goal_threads
        .insert(thread.to_owned(), updated.clone());
    vec![ProviderAdapterV2Event::ProviderThreadUpdated {
        driver: driver(),
        provider_thread: updated,
        retained_native_turn_ids: None,
    }]
}

/// Whether a thread's native goal is active. Upstream's `holdsForGoal` condition.
pub fn is_active(state: &RuntimeState, thread: &str) -> bool {
    state
        .goals
        .get(thread)
        .and_then(Option::as_ref)
        .is_some_and(|goal| goal.status == OrchestrationV2ProviderGoalStatus::Active)
}

/// Holds a completed root turn for its goal.
pub fn hold(state: &mut RuntimeState, thread: &str, hold: GoalHold) {
    state.goal_holds.insert(thread.to_owned(), hold);
}

/// Upstream's continuation: the goal's next turn, which Codex started, runs in the
/// held turn's run as the next provider turn. The held completion is delivered, and
/// its terminal is dropped, since the run continues. Returns the events to deliver.
pub fn open_continuation(
    state: &mut RuntimeState,
    thread: &str,
    native_turn: &str,
) -> Vec<ProviderAdapterV2Event> {
    let Some(held) = state.goal_holds.remove(thread) else {
        return vec![];
    };
    let mut input = held.input;
    input.provider_turn_ordinal += 1;
    state.turns.insert(
        native_turn.to_owned(),
        TurnState::new(native_turn.to_owned(), input),
    );
    vec![held.completed]
}

/// Settles a held run with its completed turn, as upstream's `releaseGoalHold`.
pub fn release_hold(state: &mut RuntimeState, thread: &str) -> Vec<ProviderAdapterV2Event> {
    match state.goal_holds.remove(thread) {
        Some(held) => vec![held.completed, held.terminal],
        None => vec![],
    }
}

/// Threads whose hold's continuation grace has ended.
pub fn expired(state: &RuntimeState, now: Instant) -> Vec<String> {
    state
        .goal_holds
        .iter()
        .filter(|(_, held)| held.deadline <= now)
        .map(|(thread, _)| thread.clone())
        .collect()
}

/// The earliest continuation deadline, for the pump's timer.
pub fn next_deadline(state: &RuntimeState) -> Option<Instant> {
    state.goal_holds.values().map(|held| held.deadline).min()
}

/// A `/goal` message, parsed the way the Codex TUI does: `clear`, `pause`, and
/// `resume` control the goal, a bare `/goal` (or `edit`) shows it, and any other
/// text is the new objective. Other messages are not goal commands.
#[derive(Debug, PartialEq, Eq)]
pub enum GoalCommand {
    Show,
    Clear,
    Pause,
    Resume,
    Set(String),
}

pub fn parse_command(text: &str) -> Option<GoalCommand> {
    let trimmed = js_trim(text);
    let rest = trimmed.strip_prefix("/goal")?;
    if !rest.is_empty() && !rest.starts_with(is_js_whitespace) {
        return None;
    }
    let argument = js_trim(rest);
    Some(match argument.to_lowercase().as_str() {
        "" | "edit" => GoalCommand::Show,
        "clear" => GoalCommand::Clear,
        "pause" => GoalCommand::Pause,
        "resume" => GoalCommand::Resume,
        _ => GoalCommand::Set(argument.to_owned()),
    })
}

/// Records a thread's native goal, and writes it onto the root provider thread when
/// it changed. Upstream `rememberGoal` followed by `emitGoalUpdate`.
pub fn set_goal(
    state: &mut RuntimeState,
    thread: &str,
    goal: Option<OrchestrationV2ProviderGoal>,
) -> Vec<ProviderAdapterV2Event> {
    state.goals.insert(thread.to_owned(), goal);
    emit_goal_update(state, thread)
}

/// Whether a root turn that completes now is held for its goal: the goal is active,
/// or a `/goal` activation is still in flight. Upstream's `holdsForGoal`.
pub fn holds_run(state: &RuntimeState, thread: &str) -> bool {
    is_active(state, thread) || state.goal_activations.contains(thread)
}

/// The goal's status as Codex's TUI words it: the first underscore becomes a space.
pub fn describe(goal: &OrchestrationV2ProviderGoal) -> String {
    let status = serde_json::to_value(goal.status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    format!(
        "Goal {}: {}",
        status.replacen('_', " ", 1),
        goal.objective.as_str()
    )
}

/// Upstream `completeGoalCommandTurn`: a `/goal` command that starts no Codex turn
/// settles with a reply, as a completed turn with no native ref.
pub fn reply_events(
    input: &ProviderAdapterV2TurnInput,
    reply: &str,
) -> Vec<ProviderAdapterV2Event> {
    use turin_contracts::orchestration_v2::{
        OrchestrationV2ProviderThreadDisposition, OrchestrationV2ProviderTurn,
        OrchestrationV2ProviderTurnStatus,
    };
    use turin_provider_core::{
        id_allocator::derive,
        provider_adapter::{ProviderAdapterV2Event as Event, ProviderAdapterV2TurnTerminalStatus},
    };
    let now = UtcDateTime::now();
    let native_turn = format!("goal-command:{}", input.attempt_id.as_str());
    let provider_turn_id = derive::provider_turn(&driver(), &native_turn);
    let turn_for = |status, completed_at| OrchestrationV2ProviderTurn {
        id: provider_turn_id.clone(),
        provider_thread_id: input.provider_thread.id.clone(),
        node_id: input.root_node_id.clone(),
        run_attempt_id: Some(input.attempt_id.clone()),
        native_turn_ref: None,
        ordinal: turin_contracts::PositiveInt::new(input.provider_turn_ordinal.max(1) as u64)
            .expect("positive ordinal"),
        status,
        started_at: Some(now),
        completed_at,
        token_usage: None,
        turn_token_usage: None,
    };
    let mut turn = TurnState::new(native_turn.clone(), input.clone());
    let artifacts = items::agent_message(&mut turn, &native_turn, reply, true, now);
    let mut events = vec![Event::ProviderTurnUpdated {
        driver: driver(),
        thread_id: Some(input.thread_id.clone()),
        provider_turn: turn_for(OrchestrationV2ProviderTurnStatus::Running, None),
    }];
    events.push(Event::NodeUpdated {
        driver: driver(),
        node: artifacts.node,
    });
    if let Some(message) = artifacts.message {
        events.push(Event::MessageUpdated {
            driver: driver(),
            message,
        });
    }
    events.push(Event::TurnItemUpdated {
        driver: driver(),
        turn_item: artifacts.turn_item,
    });
    events.push(Event::ProviderTurnUpdated {
        driver: driver(),
        thread_id: Some(input.thread_id.clone()),
        provider_turn: turn_for(OrchestrationV2ProviderTurnStatus::Completed, Some(now)),
    });
    events.push(Event::TurnTerminal {
        driver: driver(),
        provider_thread_id: input.provider_thread.id.clone(),
        provider_turn_id,
        run_ordinal: turin_contracts::PositiveInt::new(input.run_ordinal.max(1) as u64)
            .expect("positive ordinal"),
        status: ProviderAdapterV2TurnTerminalStatus::Completed,
        failure: None,
        failure_item_ordinal: None,
        retry: None,
        retry_started_at: None,
        thread_disposition: OrchestrationV2ProviderThreadDisposition::Reusable,
    });
    events
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_the_goal_subcommands_and_an_objective() {
        assert!(matches!(parse_command("/goal"), Some(GoalCommand::Show)));
        assert!(matches!(
            parse_command("  /goal edit "),
            Some(GoalCommand::Show)
        ));
        assert!(matches!(
            parse_command("/goal CLEAR"),
            Some(GoalCommand::Clear)
        ));
        assert!(matches!(
            parse_command("/goal pause"),
            Some(GoalCommand::Pause)
        ));
        assert!(matches!(
            parse_command("/goal resume"),
            Some(GoalCommand::Resume)
        ));
        assert!(
            matches!(parse_command("/goal ship it"), Some(GoalCommand::Set(objective)) if objective == "ship it")
        );
        // Not a goal command: another word, or no separator after the name.
        assert!(parse_command("/goals").is_none());
        assert!(parse_command("hello /goal").is_none());
    }

    #[test]
    fn a_goal_with_a_blank_objective_is_no_goal() {
        let goal = |objective: &str| {
            provider_goal_from_codex(&json!({"objective": objective, "status": "active"}))
        };
        assert!(goal("   ").is_none());
        // JS `trim()` also strips U+FEFF; a goal made of nothing else must not panic.
        assert!(goal("\u{feff}").is_none());
        assert_eq!(
            goal("\u{feff} ship \u{feff}").unwrap().objective.as_str(),
            "ship"
        );
    }
}
