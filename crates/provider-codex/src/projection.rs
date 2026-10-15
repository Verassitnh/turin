//! Port of the notification projection in upstream/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts
//! (`item/started`, `item/completed`, the `*Delta` streams, `turn/plan/updated`,
//! `thread/tokenUsage/updated`, and the turn terminal path), plus the text-delta
//! coalescer from `ProviderTextDeltaCoalescer.ts`.
//!
//! The caller holds the `RuntimeState` lock across each call. Projection is
//! synchronous, so a turn is only ever borrowed mutably in one place.
//!
//! Not ported yet: subagent, fork, goal, background-task, and MCP app capture.
//! MCP and dynamic-tool calls project as `dynamic_tool` items.
use crate::{
    adapter::CodexEvent, codex_turn_token_usage::CodexTurnTokenUsageState, items,
    items::ItemArtifacts, orchestration_adapter::driver,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};
use tokio::time::Instant;
use turin_contracts::{
    PlanId, PositiveInt, RuntimeRequestId, TrimmedNonEmptyString, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2NativeRefStrength,
        OrchestrationV2PlanArtifact, OrchestrationV2PlanArtifactStatus, OrchestrationV2PlanStep,
        OrchestrationV2PlanStepStatus, OrchestrationV2ProviderRef,
        OrchestrationV2ProviderThreadDisposition, OrchestrationV2ProviderTurn,
        OrchestrationV2ProviderTurnStatus, OrchestrationV2ProviderTurnTokenUsage,
        OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
    },
    provider_runtime::TurnTokenUsage,
};
use turin_provider_core::{
    id_allocator::{IdAllocator, derive},
    provider_adapter::{
        ProviderAdapterV2Event, ProviderAdapterV2TurnInput, ProviderAdapterV2TurnTerminalStatus,
    },
};

/// Upstream `CODEX_ASSISTANT_DELTA_FLUSH_INTERVAL_MS`, also used for reasoning.
const TEXT_FLUSH_INTERVAL: Duration = Duration::from_millis(50);
/// Upstream `CODEX_PLAN_DELTA_FLUSH_INTERVAL_MS`.
const PLAN_FLUSH_INTERVAL: Duration = Duration::from_millis(400);

/// One Codex turn as the projection sees it.
#[derive(Clone)]
pub struct TurnState {
    pub input: ProviderAdapterV2TurnInput,
    pub native_turn: String,
    /// Native item ids in first-seen order; the index sets each item's ordinal.
    pub positions: Vec<(String, UtcDateTime)>,
    pub started_at: Option<UtcDateTime>,
    pub running_commands: Vec<RunningCommand>,
    /// Non-commentary assistant items, in first-seen order.
    pub final_answer_items: Vec<String>,
    /// Final texts already completed as final answers (duplicate guard).
    pub completed_texts: HashSet<String>,
    /// Raw `mcpToolCall` / `dynamicToolCall` items still in progress, in first-seen order.
    pub running_tools: Vec<Value>,
}

impl TurnState {
    pub fn new(native_turn: String, input: ProviderAdapterV2TurnInput) -> Self {
        Self {
            input,
            native_turn,
            positions: vec![],
            started_at: None,
            running_commands: vec![],
            final_answer_items: vec![],
            completed_texts: HashSet::new(),
            running_tools: vec![],
        }
    }
}

/// A `commandExecution` still in progress. Its process id is what
/// `thread/backgroundTerminals/terminate` needs on interrupt.
#[derive(Clone)]
pub struct RunningCommand {
    pub id: String,
    pub process_id: Option<String>,
    pub command: String,
    pub aggregated_output: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    Assistant,
    /// Reasoning streams per part, indexed by `summary` or `content`.
    Reasoning,
    Plan,
}

/// Buffered text for one item (or reasoning part), flushed on a timer.
pub struct TextBuffer {
    pub native_turn: String,
    /// Native item id, or the composite reasoning-part key.
    pub key: String,
    pub native_item: String,
    pub kind: TextKind,
    pub text: String,
    pub dirty: bool,
}

#[derive(Default)]
pub struct RuntimeState {
    pub turns: HashMap<String, TurnState>,
    pub requests: HashMap<RuntimeRequestId, u64>,
    pub texts: Vec<TextBuffer>,
    /// Flush deadlines: index 0 for assistant and reasoning text, 1 for plan text.
    pub flush_at: [Option<Instant>; 2],
    pub plan_ids: HashMap<String, PlanId>,
    /// Token usage keyed by native thread id.
    pub usage: HashMap<String, CodexTurnTokenUsageState>,
}

fn group_of(kind: TextKind) -> usize {
    usize::from(kind == TextKind::Plan)
}

fn time_ms(ms: Option<i64>) -> Option<UtcDateTime> {
    ms.and_then(chrono::DateTime::from_timestamp_millis)
        .map(UtcDateTime::from_datetime)
}

fn time_secs(value: Option<&Value>) -> Option<UtcDateTime> {
    value
        .and_then(Value::as_f64)
        .and_then(|s| chrono::DateTime::from_timestamp_millis((s * 1000.0) as i64))
        .map(UtcDateTime::from_datetime)
}

/// The native turn a notification belongs to, for the notifications the
/// projection handles. Others are never parked.
pub fn turn_scoped<'a>(method: &str, params: &'a Value) -> Option<&'a str> {
    let handled = method.starts_with("item/")
        || method.starts_with("turn/")
        || method == "thread/tokenUsage/updated";
    if handled {
        native_turn_of(params)
    } else {
        None
    }
}

fn native_turn_of(params: &Value) -> Option<&str> {
    params
        .pointer("/turn/id")
        .and_then(Value::as_str)
        .or_else(|| params.get("turnId").and_then(Value::as_str))
}

fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Upstream `resolvePlanId`: one plan id per plan key, allocated on first use.
fn plan_id(plan_ids: &mut HashMap<String, PlanId>, turn: &TurnState, key: &str) -> PlanId {
    plan_ids
        .entry(key.to_owned())
        .or_insert_with(|| {
            IdAllocator.plan(&turn.input.thread_id, Some(&turn.input.run_id), &driver())
        })
        .clone()
}

fn emit_item(artifacts: ItemArtifacts) -> Vec<ProviderAdapterV2Event> {
    let mut events = vec![ProviderAdapterV2Event::NodeUpdated {
        driver: driver(),
        node: artifacts.node,
    }];
    if let Some(message) = artifacts.message {
        events.push(ProviderAdapterV2Event::MessageUpdated {
            driver: driver(),
            message,
        });
    }
    events.push(ProviderAdapterV2Event::TurnItemUpdated {
        driver: driver(),
        turn_item: artifacts.turn_item,
    });
    events
}

/// Emits one buffered text's update, as upstream's coalescer `emit` callbacks do.
fn emit_text(
    turn: &mut TurnState,
    plan_ids: &mut HashMap<String, PlanId>,
    buffer: &TextBuffer,
    text: &str,
    completed: bool,
    interrupted: bool,
    now: UtcDateTime,
) -> Vec<ProviderAdapterV2Event> {
    match buffer.kind {
        TextKind::Assistant => {
            let final_answer = turn.final_answer_items.contains(&buffer.native_item);
            if final_answer {
                // A final answer the turn already completed with this text is not re-emitted.
                let duplicate = completed
                    && !turn.completed_texts.is_empty()
                    && (text.is_empty() || turn.completed_texts.contains(text));
                // Streaming waits while another final answer already completed or
                // a different item leads.
                let deferred = !completed
                    && (!turn.completed_texts.is_empty()
                        || turn.final_answer_items.first() != Some(&buffer.native_item));
                if duplicate || deferred {
                    return vec![];
                }
            }
            let events = emit_item(items::agent_message(
                turn,
                &buffer.native_item,
                text,
                completed,
                now,
            ));
            if final_answer && completed {
                turn.completed_texts.insert(text.to_owned());
            }
            events
        }
        TextKind::Reasoning => emit_item(items::reasoning(
            turn,
            &buffer.key,
            &buffer.native_item,
            text,
            completed,
            now,
        )),
        TextKind::Plan => {
            let id = plan_id(plan_ids, turn, &buffer.native_item);
            // An interrupted turn leaves its partial plan streaming.
            let completed = completed && !interrupted;
            let (node, turn_item) =
                items::proposed_plan(turn, &buffer.native_item, &id, text, completed, now);
            let plan = OrchestrationV2PlanArtifact::ProposedPlan {
                id,
                thread_id: turn.input.thread_id.clone(),
                run_id: Some(turn.input.run_id.clone()),
                node_id: node.id.clone(),
                // A finished proposal stays active until Implement consumes it.
                status: OrchestrationV2PlanArtifactStatus::Active,
                detail_in_turn_item: None,
                markdown: text.to_owned(),
            };
            vec![
                ProviderAdapterV2Event::NodeUpdated {
                    driver: driver(),
                    node,
                },
                ProviderAdapterV2Event::PlanUpdated {
                    driver: driver(),
                    plan,
                },
                ProviderAdapterV2Event::TurnItemUpdated {
                    driver: driver(),
                    turn_item,
                },
            ]
        }
    }
}

/// Buffers a delta. The first delta of a flush group arms that group's timer.
fn append_text(
    state: &mut RuntimeState,
    turn: &mut TurnState,
    key: String,
    native_item: &str,
    kind: TextKind,
    delta: &str,
) {
    if delta.is_empty() {
        return;
    }
    // Reasoning reserves its ordinal at the first delta, ahead of later items.
    if kind == TextKind::Reasoning {
        items::resolve_position(turn, &key, None, UtcDateTime::now());
    }
    let index = match state
        .texts
        .iter()
        .position(|b| b.native_turn == turn.native_turn && b.key == key)
    {
        Some(index) => index,
        None => {
            state.texts.push(TextBuffer {
                native_turn: turn.native_turn.clone(),
                key,
                native_item: native_item.to_owned(),
                kind,
                text: String::new(),
                dirty: false,
            });
            state.texts.len() - 1
        }
    };
    let buffer = &mut state.texts[index];
    buffer.text.push_str(delta);
    buffer.dirty = true;
    let group = group_of(kind);
    if state.flush_at[group].is_none() {
        let interval = if group == 0 {
            TEXT_FLUSH_INTERVAL
        } else {
            PLAN_FLUSH_INTERVAL
        };
        state.flush_at[group] = Some(Instant::now() + interval);
    }
}

/// Removes one buffered text and emits it as completed. `final_text` replaces the
/// buffered text. `emit_empty: false` skips an empty completion.
fn complete_text(
    state: &mut RuntimeState,
    turn: &mut TurnState,
    key: &str,
    final_text: Option<String>,
    emit_empty: bool,
    interrupted: bool,
) -> Vec<ProviderAdapterV2Event> {
    let buffer = state
        .texts
        .iter()
        .position(|b| b.native_turn == turn.native_turn && b.key == key)
        .map(|index| state.texts.remove(index));
    let buffered = buffer.as_ref().map(|b| b.text.clone()).unwrap_or_default();
    let text = final_text.unwrap_or(buffered);
    if !emit_empty && text.is_empty() {
        return vec![];
    }
    let buffer = buffer.unwrap_or_else(|| TextBuffer {
        native_turn: turn.native_turn.clone(),
        key: key.to_owned(),
        native_item: key.to_owned(),
        kind: TextKind::Assistant,
        text: String::new(),
        dirty: false,
    });
    emit_text(
        turn,
        &mut state.plan_ids,
        &buffer,
        &text,
        true,
        interrupted,
        UtcDateTime::now(),
    )
}

/// Emits the buffered text whose flush deadline has passed. The stream calls
/// this when its flush timer fires.
pub fn flush_due(state: &mut RuntimeState, now: Instant) -> Vec<ProviderAdapterV2Event> {
    let mut events = vec![];
    for group in 0..2 {
        if state.flush_at[group].is_none_or(|at| at > now) {
            continue;
        }
        state.flush_at[group] = None;
        let dirty: Vec<usize> = state
            .texts
            .iter()
            .enumerate()
            .filter(|(_, b)| b.dirty && group_of(b.kind) == group)
            .map(|(index, _)| index)
            .collect();
        for index in dirty {
            let snapshot = TextBuffer {
                native_turn: state.texts[index].native_turn.clone(),
                key: state.texts[index].key.clone(),
                native_item: state.texts[index].native_item.clone(),
                kind: state.texts[index].kind,
                text: String::new(),
                dirty: false,
            };
            let text = state.texts[index].text.clone();
            state.texts[index].dirty = false;
            let Some(mut turn) = state.turns.remove(&snapshot.native_turn) else {
                continue;
            };
            events.extend(emit_text(
                &mut turn,
                &mut state.plan_ids,
                &snapshot,
                &text,
                false,
                false,
                UtcDateTime::now(),
            ));
            state.turns.insert(snapshot.native_turn, turn);
        }
    }
    events
}

/// The earliest pending flush, for the stream's timer branch.
pub fn next_flush(state: &RuntimeState) -> Option<Instant> {
    state.flush_at.iter().flatten().min().copied()
}

/// Upstream `flushTurn`: completes every buffered text of a turn that is ending.
fn flush_turn(
    state: &mut RuntimeState,
    turn: &mut TurnState,
    interrupted: bool,
) -> Vec<ProviderAdapterV2Event> {
    let keys: Vec<String> = state
        .texts
        .iter()
        .filter(|b| b.native_turn == turn.native_turn)
        .map(|b| b.key.clone())
        .collect();
    keys.iter()
        .flat_map(|key| complete_text(state, turn, key, None, true, interrupted))
        .collect()
}

fn provider_turn(
    turn: &TurnState,
    status: OrchestrationV2ProviderTurnStatus,
    completed_at: Option<UtcDateTime>,
    token_usage: Option<OrchestrationV2ProviderTurnTokenUsage>,
    turn_token_usage: Option<TurnTokenUsage>,
) -> OrchestrationV2ProviderTurn {
    OrchestrationV2ProviderTurn {
        id: derive::provider_turn(&driver(), &turn.native_turn),
        provider_thread_id: turn.input.provider_thread.id.clone(),
        node_id: turn.input.root_node_id.clone(),
        run_attempt_id: Some(turn.input.attempt_id.clone()),
        native_turn_ref: Some(OrchestrationV2ProviderRef {
            driver: driver(),
            native_id: Some(TrimmedNonEmptyString::new(&turn.native_turn).expect("native turn")),
            strength: OrchestrationV2NativeRefStrength::Strong,
            fingerprint: None,
            ordinal: None,
        }),
        ordinal: PositiveInt::new(turn.input.provider_turn_ordinal.max(1) as u64)
            .expect("positive ordinal"),
        status,
        started_at: turn.started_at,
        completed_at,
        token_usage,
        turn_token_usage,
    }
}

/// Upstream `codexProviderTurnTokenUsage`, read from the `last` breakdown.
fn token_usage(usage: &Value) -> OrchestrationV2ProviderTurnTokenUsage {
    let last = &usage["last"];
    let count = |key: &str| last.get(key).and_then(Value::as_u64).unwrap_or(0);
    OrchestrationV2ProviderTurnTokenUsage {
        used_tokens: count("totalTokens"),
        max_tokens: Some(usage.get("modelContextWindow").and_then(Value::as_u64)),
        input_tokens: Some(count("inputTokens")),
        cached_input_tokens: Some(count("cachedInputTokens")),
        output_tokens: Some(count("outputTokens")),
        reasoning_output_tokens: Some(count("reasoningOutputTokens")),
        prompt_cache_ttl_ms: None,
        updated_at: UtcDateTime::now().to_string(),
    }
}

fn plan_step_status(status: &str) -> OrchestrationV2PlanStepStatus {
    match status {
        "completed" => OrchestrationV2PlanStepStatus::Completed,
        "inProgress" => OrchestrationV2PlanStepStatus::Running,
        _ => OrchestrationV2PlanStepStatus::Pending,
    }
}

/// Projects one Codex notification into orchestration events.
pub fn project_event(raw: CodexEvent, state: &mut RuntimeState) -> Vec<ProviderAdapterV2Event> {
    let Some(native_turn) = native_turn_of(&raw.params).map(str::to_owned) else {
        return vec![];
    };
    let thread = str_of(&raw.params, "threadId")
        .unwrap_or_default()
        .to_owned();
    if raw.method == "thread/tokenUsage/updated"
        && let Some(usage) = raw.params.get("tokenUsage")
    {
        state
            .usage
            .entry(thread.clone())
            .or_default()
            .accumulate(&native_turn, usage);
    }
    let Some(mut turn) = state.turns.remove(&native_turn) else {
        return vec![];
    };
    let now = UtcDateTime::now();
    let events = match raw.method.as_str() {
        "turn/started" => {
            turn.started_at = time_secs(raw.params.pointer("/turn/startedAt")).or(Some(now));
            state.usage.entry(thread).or_default().begin(&native_turn);
            vec![ProviderAdapterV2Event::ProviderTurnUpdated {
                driver: driver(),
                thread_id: Some(turn.input.thread_id.clone()),
                provider_turn: provider_turn(
                    &turn,
                    OrchestrationV2ProviderTurnStatus::Running,
                    None,
                    None,
                    None,
                ),
            }]
        }
        "item/started" => item_started(&raw.params, &mut turn, now),
        "item/completed" => item_completed(&raw.params, &mut turn, state, now),
        "item/agentMessage/delta" => {
            let item = str_of(&raw.params, "itemId").unwrap_or_default().to_owned();
            let delta = str_of(&raw.params, "delta").unwrap_or_default();
            append_text(
                state,
                &mut turn,
                item.clone(),
                &item,
                TextKind::Assistant,
                delta,
            );
            vec![]
        }
        "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
            let (stream, index_field) = if raw.method == "item/reasoning/summaryTextDelta" {
                ("summary", "summaryIndex")
            } else {
                ("content", "contentIndex")
            };
            let index = raw
                .params
                .get(index_field)
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let item = str_of(&raw.params, "itemId").unwrap_or_default().to_owned();
            let delta = str_of(&raw.params, "delta").unwrap_or_default();
            let key = reasoning_key(&native_turn, &item, stream, index);
            append_text(state, &mut turn, key, &item, TextKind::Reasoning, delta);
            vec![]
        }
        "item/plan/delta" => {
            let item = str_of(&raw.params, "itemId").unwrap_or_default().to_owned();
            let delta = str_of(&raw.params, "delta").unwrap_or_default();
            append_text(state, &mut turn, item.clone(), &item, TextKind::Plan, delta);
            vec![]
        }
        "turn/plan/updated" => todo_updated(&raw.params, &mut turn, state, now),
        "thread/tokenUsage/updated" => match raw.params.get("tokenUsage") {
            Some(usage) => vec![ProviderAdapterV2Event::ProviderTurnUpdated {
                driver: driver(),
                thread_id: Some(turn.input.thread_id.clone()),
                provider_turn: provider_turn(
                    &turn,
                    OrchestrationV2ProviderTurnStatus::Running,
                    None,
                    Some(token_usage(usage)),
                    None,
                ),
            }],
            None => vec![],
        },
        "turn/completed" => {
            let events = turn_completed(&raw.params, &mut turn, state, now);
            // The turn is finished: it is not reinserted.
            return events;
        }
        _ => vec![],
    };
    state.turns.insert(native_turn, turn);
    events
}

/// Composite key for one reasoning part, as upstream `reasoningPartKey`.
fn reasoning_key(native_turn: &str, native_item: &str, stream: &str, index: u64) -> String {
    serde_json::to_string(&json!([native_turn, native_item, stream, index])).expect("reasoning key")
}

fn item_started(
    params: &Value,
    turn: &mut TurnState,
    now: UtcDateTime,
) -> Vec<ProviderAdapterV2Event> {
    let Some(item) = params.get("item") else {
        return vec![];
    };
    let kind = str_of(item, "type").unwrap_or_default();
    // Compaction, user messages, and subagent activity are not ported yet.
    if matches!(
        kind,
        "contextCompaction" | "userMessage" | "subAgentActivity"
    ) {
        return vec![];
    }
    let native_item = str_of(item, "id").unwrap_or_default();
    let started_at = time_ms(params.get("startedAtMs").and_then(Value::as_i64));
    if kind != "reasoning" {
        items::resolve_position(turn, native_item, started_at, now);
    }
    match kind {
        "agentMessage" => {
            if str_of(item, "phase") != Some("commentary")
                && !turn.final_answer_items.iter().any(|id| id == native_item)
            {
                turn.final_answer_items.push(native_item.to_owned());
            }
            vec![]
        }
        "commandExecution" => {
            let status = items::codex_item_status(str_of(item, "status").unwrap_or("inProgress"));
            if !status.completed {
                track_command(turn, item);
            }
            emit_item(items::command_execution(turn, item, started_at, now))
        }
        "mcpToolCall" | "dynamicToolCall" => {
            let status = items::codex_item_status(str_of(item, "status").unwrap_or("inProgress"));
            if !status.completed {
                track_tool(turn, item);
            }
            emit_item(items::dynamic_tool(turn, item, now))
        }
        "webSearch" => emit_item(items::web_search(turn, item, false, now)),
        _ => vec![],
    }
}

fn track_tool(turn: &mut TurnState, item: &Value) {
    let id = str_of(item, "id");
    turn.running_tools.retain(|tool| str_of(tool, "id") != id);
    turn.running_tools.push(item.clone());
}

/// The node and turn-item status a still-running item takes when its turn ends.
/// Only an interrupt or a failure stops a tool, so a clean end cancels it.
fn stopped_item_statuses(
    status: OrchestrationV2ProviderTurnStatus,
) -> (
    OrchestrationV2ExecutionNodeStatus,
    OrchestrationV2TurnItemStatus,
) {
    match status {
        OrchestrationV2ProviderTurnStatus::Interrupted => (
            OrchestrationV2ExecutionNodeStatus::Interrupted,
            OrchestrationV2TurnItemStatus::Interrupted,
        ),
        OrchestrationV2ProviderTurnStatus::Failed => (
            OrchestrationV2ExecutionNodeStatus::Failed,
            OrchestrationV2TurnItemStatus::Failed,
        ),
        _ => (
            OrchestrationV2ExecutionNodeStatus::Cancelled,
            OrchestrationV2TurnItemStatus::Cancelled,
        ),
    }
}

/// Upstream `isPersistentCodexDynamicTool`: a tool that asks to outlive its turn.
fn is_persistent_tool(item: &Value) -> bool {
    item.get("arguments")
        .and_then(|input| input.get("persistent"))
        .and_then(Value::as_bool)
        == Some(true)
}

fn track_command(turn: &mut TurnState, item: &Value) {
    let id = str_of(item, "id").unwrap_or_default().to_owned();
    turn.running_commands.retain(|c| c.id != id);
    turn.running_commands.push(RunningCommand {
        id,
        process_id: str_of(item, "processId").map(str::to_owned),
        command: str_of(item, "command").unwrap_or_default().to_owned(),
        aggregated_output: str_of(item, "aggregatedOutput").map(str::to_owned),
    });
}

fn item_completed(
    params: &Value,
    turn: &mut TurnState,
    state: &mut RuntimeState,
    now: UtcDateTime,
) -> Vec<ProviderAdapterV2Event> {
    let Some(item) = params.get("item") else {
        return vec![];
    };
    let kind = str_of(item, "type").unwrap_or_default();
    let native_item = str_of(item, "id").unwrap_or_default().to_owned();
    match kind {
        "reasoning" => reasoning_completed(item, turn, state, &native_item),
        "commandExecution" => {
            turn.running_commands.retain(|c| c.id != native_item);
            emit_item(items::command_execution(turn, item, None, now))
        }
        "mcpToolCall" | "dynamicToolCall" => {
            turn.running_tools
                .retain(|tool| str_of(tool, "id") != Some(native_item.as_str()));
            emit_item(items::dynamic_tool(turn, item, now))
        }
        "webSearch" => emit_item(items::web_search(turn, item, true, now)),
        "fileChange" => items::file_change(turn, item, now)
            .map(emit_item)
            .unwrap_or_default(),
        "plan" => {
            let final_text = str_of(item, "text")
                .filter(|text| !text.is_empty())
                .map(str::to_owned);
            complete_text(state, turn, &native_item, final_text, true, false)
        }
        "agentMessage" => {
            let final_text = str_of(item, "text").unwrap_or_default().to_owned();
            let events = complete_text(state, turn, &native_item, Some(final_text), true, false);
            turn.final_answer_items.retain(|id| id != &native_item);
            events
        }
        _ => vec![],
    }
}

/// Completes every reasoning part of an item. Parts without a streamed buffer
/// complete from the item's own text, and empty parts emit nothing.
fn reasoning_completed(
    item: &Value,
    turn: &mut TurnState,
    state: &mut RuntimeState,
    native_item: &str,
) -> Vec<ProviderAdapterV2Event> {
    let mut events = vec![];
    for stream in ["summary", "content"] {
        let parts = item
            .get(stream)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (index, part) in parts.iter().enumerate() {
            let key = reasoning_key(&turn.native_turn, native_item, stream, index as u64);
            let final_text = part
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_owned);
            events.extend(complete_text(state, turn, &key, final_text, false, false));
        }
    }
    // Streamed parts beyond the item's final list still complete.
    let leftovers: Vec<String> = state
        .texts
        .iter()
        .filter(|b| b.native_turn == turn.native_turn && b.native_item == native_item)
        .map(|b| b.key.clone())
        .collect();
    for key in leftovers {
        events.extend(complete_text(state, turn, &key, None, false, false));
    }
    events
}

fn todo_updated(
    params: &Value,
    turn: &mut TurnState,
    state: &mut RuntimeState,
    now: UtcDateTime,
) -> Vec<ProviderAdapterV2Event> {
    let steps: Vec<OrchestrationV2PlanStep> = params
        .get("plan")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, step)| {
            let text = str_of(step, "step")
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map_or_else(|| format!("Step {}", index + 1), str::to_owned);
            OrchestrationV2PlanStep {
                id: TrimmedNonEmptyString::new(format!("step-{}", index + 1)).expect("step id"),
                text: TrimmedNonEmptyString::new(text).expect("step text"),
                status: plan_step_status(str_of(step, "status").unwrap_or_default()),
                duration_anchor_at: None,
                duration_ms: None,
            }
        })
        .collect();
    let explanation = str_of(params, "explanation")
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    let native_item = format!("turn-plan:{}", turn.native_turn);
    let plan = plan_id(&mut state.plan_ids, turn, &native_item);
    let (node, turn_item) = items::todo_list(
        turn,
        &native_item,
        &plan,
        &steps,
        explanation.as_deref(),
        now,
    );
    let plan_artifact = OrchestrationV2PlanArtifact::TodoList {
        id: plan,
        thread_id: turn.input.thread_id.clone(),
        run_id: Some(turn.input.run_id.clone()),
        node_id: node.id.clone(),
        status: OrchestrationV2PlanArtifactStatus::Active,
        detail_in_turn_item: None,
        steps,
        explanation,
    };
    vec![
        ProviderAdapterV2Event::NodeUpdated {
            driver: driver(),
            node,
        },
        ProviderAdapterV2Event::PlanUpdated {
            driver: driver(),
            plan: plan_artifact,
        },
        ProviderAdapterV2Event::TurnItemUpdated {
            driver: driver(),
            turn_item,
        },
    ]
}

fn turn_completed(
    params: &Value,
    turn: &mut TurnState,
    state: &mut RuntimeState,
    now: UtcDateTime,
) -> Vec<ProviderAdapterV2Event> {
    let (status, terminal) = match params.pointer("/turn/status").and_then(Value::as_str) {
        Some("completed") => (
            OrchestrationV2ProviderTurnStatus::Completed,
            ProviderAdapterV2TurnTerminalStatus::Completed,
        ),
        Some("interrupted") => (
            OrchestrationV2ProviderTurnStatus::Interrupted,
            ProviderAdapterV2TurnTerminalStatus::Interrupted,
        ),
        Some("cancelled") => (
            OrchestrationV2ProviderTurnStatus::Cancelled,
            ProviderAdapterV2TurnTerminalStatus::Cancelled,
        ),
        _ => (
            OrchestrationV2ProviderTurnStatus::Failed,
            ProviderAdapterV2TurnTerminalStatus::Failed,
        ),
    };
    let stopped_early = matches!(
        status,
        OrchestrationV2ProviderTurnStatus::Interrupted | OrchestrationV2ProviderTurnStatus::Failed
    );
    let mut events = flush_turn(state, turn, stopped_early);
    // Commands still running when an interrupted or failed turn ends were stopped with it.
    let stopped = std::mem::take(&mut turn.running_commands);
    if stopped_early {
        let (node_status, item_status) = match status {
            OrchestrationV2ProviderTurnStatus::Interrupted => (
                OrchestrationV2ExecutionNodeStatus::Interrupted,
                OrchestrationV2TurnItemStatus::Interrupted,
            ),
            _ => (
                OrchestrationV2ExecutionNodeStatus::Failed,
                OrchestrationV2TurnItemStatus::Failed,
            ),
        };
        for command in stopped {
            let item = json!({
                "id": command.id,
                "command": command.command,
                "aggregatedOutput": command.aggregated_output,
                "status": "inProgress",
            });
            let mut artifacts = items::command_execution(turn, &item, None, now);
            artifacts.node.status = node_status;
            artifacts.node.completed_at = Some(now);
            if let OrchestrationV2TurnItem::CommandExecution {
                status,
                completed_at,
                ..
            } = &mut artifacts.turn_item
            {
                *status = item_status;
                *completed_at = Some(now);
            }
            events.extend(emit_item(artifacts));
        }
    }
    // Upstream `terminalizeRunningDynamicTools`: a persistent tool keeps running only
    // when the turn ended cleanly. Turin drops the turn state here, so a persistent
    // tool left running stays running (see the Deviations table).
    let (tool_node_status, tool_item_status) = stopped_item_statuses(status);
    for tool in std::mem::take(&mut turn.running_tools) {
        if !stopped_early && is_persistent_tool(&tool) {
            continue;
        }
        let mut artifacts = items::dynamic_tool(turn, &tool, now);
        artifacts.node.status = tool_node_status;
        artifacts.node.completed_at = Some(now);
        if let OrchestrationV2TurnItem::DynamicTool {
            status,
            completed_at,
            updated_at,
            ..
        } = &mut artifacts.turn_item
        {
            *status = tool_item_status;
            *completed_at = Some(now);
            *updated_at = now;
        }
        events.extend(emit_item(artifacts));
    }
    let token = state
        .usage
        .get_mut(str_of(params, "threadId").unwrap_or_default())
        .map(|usage| {
            usage.complete(
                &turn.native_turn,
                status == OrchestrationV2ProviderTurnStatus::Completed,
            )
        });
    let provider = provider_turn(
        turn,
        status,
        time_secs(params.pointer("/turn/completedAt")).or(Some(now)),
        None,
        token,
    );
    events.push(ProviderAdapterV2Event::ProviderTurnUpdated {
        driver: driver(),
        thread_id: Some(turn.input.thread_id.clone()),
        provider_turn: provider,
    });
    events.push(ProviderAdapterV2Event::TurnTerminal {
        driver: driver(),
        provider_thread_id: turn.input.provider_thread.id.clone(),
        provider_turn_id: derive::provider_turn(&driver(), &turn.native_turn),
        run_ordinal: PositiveInt::new(turn.input.run_ordinal.max(1) as u64)
            .expect("positive ordinal"),
        status: terminal,
        failure: None,
        failure_item_ordinal: None,
        retry: None,
        retry_started_at: None,
        thread_disposition: OrchestrationV2ProviderThreadDisposition::Reusable,
    });
    events
}
