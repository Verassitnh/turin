//! Port of the interrupt path of `interruptTurn` in
//! upstream/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts
//! (L6730-7200): which turns a Stop reaches, and how a background terminal is stopped.

use std::time::Duration;

use serde_json::{Value, json};
use turin_codex_app_server::{client::Client, errors::AppServerError};
use turin_contracts::{ProviderThreadId, ProviderTurnId};
use turin_provider_core::id_allocator::derive;

use crate::{
    orchestration_adapter::driver,
    projection::{RuntimeState, TurnState, turn_native_thread},
    subagent,
};

/// One turn a Stop reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub native_turn: String,
    pub thread: String,
    /// Registered as an active turn (it can be sent `turn/interrupt`), not only settled.
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The active root(s) the Stop is for.
    pub roots: Vec<String>,
    pub targets: Vec<Target>,
}

/// Whether `turn` is one of `roots` or runs under one (`isDescendantCodexTurn`).
pub fn in_lineage(state: &RuntimeState, turn: &TurnState, roots: &[String]) -> bool {
    subagent::ancestors_of(state, turn)
        .iter()
        .any(|native| roots.contains(native))
}

/// The turns a Stop reaches: the active turn, the active turns in its lineage, and the
/// settled turns in it. After a runtime restart request, the settled turns of the thread too.
/// `None` when nothing is reachable.
pub fn plan(
    state: &RuntimeState,
    provider_turn: &ProviderTurnId,
    provider_thread: &ProviderThreadId,
    restart: bool,
) -> Option<Plan> {
    let active = state
        .turns
        .values()
        .find(|turn| derive::provider_turn(&driver(), &turn.native_turn) == *provider_turn);
    let roots: Vec<String> = active.iter().map(|turn| turn.native_turn.clone()).collect();
    let target = |turn: &TurnState, active: bool| {
        turn_native_thread(turn).map(|thread| Target {
            native_turn: turn.native_turn.clone(),
            thread,
            active,
        })
    };
    let mut targets: Vec<Target> = active
        .into_iter()
        .filter_map(|turn| target(turn, true))
        .collect();
    let mut by_turn: Vec<&TurnState> = state.turns.values().collect();
    by_turn.sort_by(|left, right| left.native_turn.cmp(&right.native_turn));
    for turn in by_turn {
        if !roots.contains(&turn.native_turn)
            && in_lineage(state, turn, &roots)
            && let Some(found) = target(turn, true)
        {
            targets.push(found);
        }
    }
    let mut settled: Vec<&TurnState> = state.settled.values().collect();
    settled.sort_by(|left, right| left.native_turn.cmp(&right.native_turn));
    for turn in settled {
        let on_thread = restart && turn.input.provider_thread.id == *provider_thread;
        if (on_thread || in_lineage(state, turn, &roots))
            && !roots.contains(&turn.native_turn)
            && let Some(found) = target(turn, false)
        {
            targets.push(found);
        }
    }
    (!targets.is_empty()).then_some(Plan { roots, targets })
}

/// The background terminals the targets still run, as (native thread, process id), taken
/// before the turns end, since ending a stopped turn forgets its commands.
pub fn terminals(state: &RuntimeState, targets: &[Target]) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = vec![];
    for target in targets {
        let turn = state
            .turns
            .get(&target.native_turn)
            .or_else(|| state.settled.get(&target.native_turn));
        for command in turn.into_iter().flat_map(|turn| &turn.running_commands) {
            if let Some(process) = &command.process_id {
                let entry = (target.thread.clone(), process.clone());
                if !found.contains(&entry) {
                    found.push(entry);
                }
            }
        }
    }
    found
}

const TERMINATE_TIMEOUT: Duration = Duration::from_secs(10);

/// `terminateBackgroundTerminal`. Called through the raw client: the method is not in the
/// pinned typed surface. An app-server that is gone, or a thread it already unloaded, took
/// the terminal with it. Otherwise a terminal still listed afterwards is an error.
pub async fn terminate_background_terminal(
    client: &Client,
    thread: &str,
    process: &str,
) -> Result<(), String> {
    match tokio::time::timeout(TERMINATE_TIMEOUT, terminate(client, thread, process)).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "Timed out stopping Codex background terminal {process}."
        )),
    }
}

async fn terminate(client: &Client, thread: &str, process: &str) -> Result<(), String> {
    let response = client
        .raw
        .request(
            "thread/backgroundTerminals/terminate",
            Some(json!({"threadId": thread, "processId": process})),
        )
        .await;
    let response = match response {
        Ok(response) => response,
        Err(AppServerError::ProcessExited(_) | AppServerError::InputStreamEnded) => return Ok(()),
        Err(AppServerError::Request { error, .. })
            if error.code == -32600 && error.message.starts_with("thread not found:") =>
        {
            return Ok(());
        }
        Err(error) => return Err(error.to_string()),
    };
    let terminated = response
        .as_ref()
        .and_then(|response| response.get("terminated"))
        .and_then(Value::as_bool)
        .ok_or_else(|| "Codex returned an invalid background terminal response.".to_owned())?;
    if terminated {
        return Ok(());
    }
    let mut cursor: Option<String> = None;
    loop {
        let mut params = json!({"threadId": thread});
        if let Some(cursor) = &cursor {
            params["cursor"] = json!(cursor);
        }
        let page = client
            .raw
            .request("thread/backgroundTerminals/list", Some(params))
            .await
            .map_err(|error| error.to_string())?
            .unwrap_or(Value::Null);
        let listed = page
            .get("data")
            .and_then(Value::as_array)
            .is_some_and(|terminals| {
                terminals.iter().any(|terminal| {
                    terminal.get("processId").and_then(Value::as_str) == Some(process)
                })
            });
        if listed {
            return Err(format!(
                "Codex background terminal {process} remained active after termination."
            ));
        }
        match page.get("nextCursor").and_then(Value::as_str) {
            Some(next) => cursor = Some(next.to_owned()),
            None => return Ok(()),
        }
    }
}
