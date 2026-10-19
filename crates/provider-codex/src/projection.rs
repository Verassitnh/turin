//! Port of the notification projection in upstream/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts
//! (`item/started`, `item/completed`, the `*Delta` streams, `turn/plan/updated`,
//! `thread/tokenUsage/updated`, and the turn terminal path), plus the text-delta
//! coalescer from `ProviderTextDeltaCoalescer.ts`.
//!
//! The caller holds the `RuntimeState` lock across each call. Projection is
//! synchronous, so a turn is only ever borrowed mutably in one place.
//!
//! Subagent threads and their child turns project through `subagent`.
//! Not ported yet: fork, goal, background-task, and MCP app capture.
//! MCP and dynamic-tool calls project as `dynamic_tool` items.
//!
//! Port of upstream/apps/server/src/orchestration-v2/Adapters/ProviderTextDeltaCoalescer.ts (coalescer (the Claude adapter emits each delta; see Deviations)).
use crate::{
    adapter::CodexEvent,
    codex_turn_token_usage::CodexTurnTokenUsageState,
    goal, items,
    items::ItemArtifacts,
    orchestration_adapter::driver,
    retry::{self, LatestFailure, ProviderRetry},
    subagent::{self, SubagentThread},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};
use tokio::time::Instant;
use turin_contracts::base_schemas::js_trim;
use turin_contracts::{
    PlanId, PositiveInt, RunAttemptId, RunId, RuntimeRequestId, TrimmedNonEmptyString, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2ExecutionNodeStatus, OrchestrationV2NativeRefStrength,
        OrchestrationV2PlanArtifact, OrchestrationV2PlanArtifactStatus, OrchestrationV2PlanStep,
        OrchestrationV2PlanStepStatus, OrchestrationV2ProviderGoal, OrchestrationV2ProviderRef,
        OrchestrationV2ProviderThread, OrchestrationV2ProviderThreadDisposition,
        OrchestrationV2ProviderTurn, OrchestrationV2ProviderTurnStatus,
        OrchestrationV2ProviderTurnTokenUsage, OrchestrationV2SubagentStatus,
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
    /// The native subagent thread this is a child turn of. Its run and attempt are empty.
    pub subagent: Option<String>,
    /// The last provider error that was not retried.
    pub latest_failure: Option<LatestFailure>,
    /// The retry this turn is in, while the provider retries it.
    pub provider_retry: Option<ProviderRetry>,
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
            subagent: None,
            latest_failure: None,
            provider_retry: None,
        }
    }

    /// The run a turn's items belong to. A child turn has none, as upstream's do.
    pub fn run_id(&self) -> Option<RunId> {
        self.subagent.is_none().then(|| self.input.run_id.clone())
    }

    /// The attempt a turn's provider turn reports. A child turn has none.
    pub fn attempt_id(&self) -> Option<RunAttemptId> {
        self.subagent
            .is_none()
            .then(|| self.input.attempt_id.clone())
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
    /// Native subagent threads, keyed by native thread id. A child's `turn/started`
    /// opens a child turn here, so the thread is the lookup from frame to subagent.
    pub subagents: HashMap<String, SubagentThread>,
    /// Turns that completed with background work still running, keyed by native turn
    /// id. Their late item frames project until that work is done.
    pub settled: HashMap<String, TurnState>,
    /// Native turns that ended without completing. A child turn under one is interrupted.
    pub terminalized: HashSet<String>,
    /// Late child turns to interrupt, as (native thread, native turn). The pump sends them.
    pub interrupts: Vec<(String, String)>,
    /// Threads whose `turn/start` is in flight. Its turn registers when the call answers.
    pub starting: HashSet<String>,
    /// The native goal of each thread, `None` when it has none.
    pub goals: HashMap<String, Option<OrchestrationV2ProviderGoal>>,
    /// The root provider thread each native thread's goal is written onto.
    pub goal_threads: HashMap<String, OrchestrationV2ProviderThread>,
    /// Root turns held for their goal, by native thread id.
    pub goal_holds: HashMap<String, goal::GoalHold>,
    /// Threads with a `/goal` activation in flight: a turn that ends before it is active is held.
    pub goal_activations: HashSet<String>,
    /// The model each thread reported, by native thread id.
    pub subagent_selections: HashMap<String, subagent::ReportedSelection>,
    /// A compaction's turn is unknown until Codex reports it: the next `turn/started`
    /// on the thread adopts this input (`pendingRootTurns`).
    pub pending_root_turns: HashMap<String, ProviderAdapterV2TurnInput>,
    /// Requests no turn can show, to answer at once as (broker id, decision). The pump sends them.
    pub unanswered: Vec<(u64, crate::approvals::Decision)>,
    /// Native turns a Stop is interrupting. A clean `turn/completed` for one reads as interrupted.
    pub interrupting: HashSet<String>,
    /// Native turns that have ended and are no longer held. A later frame for one is stale:
    /// it is dropped (a request is rejected) rather than parked for a turn that never returns.
    pub finished: HashSet<String>,
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
        || method == "thread/tokenUsage/updated"
        || method == "error";
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
            IdAllocator.plan(&turn.input.thread_id, turn.run_id().as_ref(), &driver())
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
            interrupted,
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
                run_id: turn.run_id(),
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
    // A held goal run whose continuation grace ended settles as completed.
    for thread in goal::expired(state, now) {
        events.extend(goal::release_hold(state, &thread));
    }
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
    state
        .flush_at
        .iter()
        .flatten()
        .copied()
        .chain(goal::next_deadline(state))
        .min()
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

pub(crate) fn provider_turn(
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
        run_attempt_id: turn.attempt_id(),
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
    // Thread settings are not turn-scoped, so they update the reported selection directly.
    if matches!(
        raw.method.as_str(),
        "thread/settings/updated" | "model/rerouted"
    ) {
        return subagent::settings_updated(state, &raw.method, &raw.params);
    }
    if matches!(
        raw.method.as_str(),
        "thread/goal/updated" | "thread/goal/cleared"
    ) {
        return goal::goal_event(state, &raw.method, &raw.params);
    }
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
    let now = UtcDateTime::now();
    if !state.turns.contains_key(&native_turn) {
        if state.settled.contains_key(&native_turn) {
            return project_settled(&raw, &thread, &native_turn, state, now);
        }
        // A compaction starts a turn nobody asked for by id: it belongs to the run that
        // started the compaction.
        if raw.method == "turn/started"
            && let Some(input) = state.pending_root_turns.remove(&thread)
        {
            state
                .goal_threads
                .insert(thread.clone(), input.provider_thread.clone());
            state.turns.insert(
                native_turn.clone(),
                TurnState::new(native_turn.clone(), input),
            );
            return project_event(raw, state);
        }
        // Codex starts a held goal's next turn itself. That turn runs in the held run.
        if raw.method == "turn/started" && state.goal_holds.contains_key(&thread) {
            let mut events = goal::open_continuation(state, &thread, &native_turn);
            events.extend(project_event(raw, state));
            return events;
        }
        // A subagent's first `turn/started` opens its child turn. Other frames for
        // a turn that is not registered are dropped here.
        if raw.method == "turn/started" && subagent::opens_child_turn(state, &thread, &native_turn)
        {
            let started_at = time_secs(raw.params.pointer("/turn/startedAt")).unwrap_or(now);
            return subagent::start_child_turn(state, &thread, &native_turn, started_at);
        }
        return vec![];
    }
    let Some(mut turn) = state.turns.remove(&native_turn) else {
        return vec![];
    };
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
        "item/started" => item_started(&raw.params, &thread, &mut turn, state, now),
        "item/completed" => item_completed(&raw.params, &thread, &mut turn, state, now),
        "item/agentMessage/delta" => {
            let item = str_of(&raw.params, "itemId").unwrap_or_default().to_owned();
            let delta = str_of(&raw.params, "delta").unwrap_or_default();
            let events = retry::complete(&mut turn, now);
            append_text(
                state,
                &mut turn,
                item.clone(),
                &item,
                TextKind::Assistant,
                delta,
            );
            events
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
            let events = if delta.is_empty() {
                vec![]
            } else {
                retry::complete(&mut turn, now)
            };
            append_text(state, &mut turn, key, &item, TextKind::Reasoning, delta);
            events
        }
        "item/plan/delta" => {
            let item = str_of(&raw.params, "itemId").unwrap_or_default().to_owned();
            let delta = str_of(&raw.params, "delta").unwrap_or_default();
            let events = retry::complete(&mut turn, now);
            append_text(state, &mut turn, item.clone(), &item, TextKind::Plan, delta);
            events
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
        "error" => retry::error(&mut turn, &raw.params, now),
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

/// Whether a settled turn still has work its late item frames complete: a running
/// command, or a persistent dynamic tool. Upstream's `turnHasRetainedBackgroundWork`.
pub fn has_retained_work(turn: &TurnState) -> bool {
    !turn.running_commands.is_empty() || turn.running_tools.iter().any(is_persistent_tool)
}

/// The native thread a turn runs on: the child's thread for a subagent turn, else the
/// root provider thread's native id.
pub fn turn_native_thread(turn: &TurnState) -> Option<String> {
    match &turn.subagent {
        Some(child) => Some(child.clone()),
        None => turn
            .input
            .provider_thread
            .native_thread_ref
            .as_ref()
            .and_then(|native| native.native_id.as_ref())
            .map(|id| id.as_str().to_owned()),
    }
}

/// A Stop reached a settled turn: its retained commands and tools (persistent ones too)
/// end as interrupted, without rewriting the completed turn, and the turn is released.
pub fn stop_settled_work(
    state: &mut RuntimeState,
    native_turn: &str,
    now: UtcDateTime,
) -> Vec<ProviderAdapterV2Event> {
    let Some(mut turn) = state.settled.remove(native_turn) else {
        return vec![];
    };
    let mut events = vec![];
    for command in std::mem::take(&mut turn.running_commands) {
        let item = json!({
            "id": command.id,
            "command": command.command,
            "aggregatedOutput": command.aggregated_output,
            "status": "inProgress",
        });
        let mut artifacts = items::command_execution(&mut turn, &item, None, now);
        artifacts.node.status = OrchestrationV2ExecutionNodeStatus::Interrupted;
        artifacts.node.completed_at = Some(now);
        if let OrchestrationV2TurnItem::CommandExecution {
            status,
            completed_at,
            ..
        } = &mut artifacts.turn_item
        {
            *status = OrchestrationV2TurnItemStatus::Interrupted;
            *completed_at = Some(now);
        }
        events.extend(emit_item(artifacts));
    }
    for tool in std::mem::take(&mut turn.running_tools) {
        let mut artifacts = items::dynamic_tool(&mut turn, &tool, now);
        artifacts.node.status = OrchestrationV2ExecutionNodeStatus::Interrupted;
        artifacts.node.completed_at = Some(now);
        if let OrchestrationV2TurnItem::DynamicTool {
            status,
            completed_at,
            updated_at,
            ..
        } = &mut artifacts.turn_item
        {
            *status = OrchestrationV2TurnItemStatus::Interrupted;
            *completed_at = Some(now);
            *updated_at = now;
        }
        events.extend(emit_item(artifacts));
    }
    events
}

/// `hasPendingBackgroundWork`: any running command or dynamic tool in any turn,
/// or a subagent still running.
pub fn has_pending_background_work(state: &RuntimeState) -> bool {
    state
        .turns
        .values()
        .chain(state.settled.values())
        .any(|turn| !turn.running_commands.is_empty() || !turn.running_tools.is_empty())
        || subagent::has_running(state)
}

/// `hasPendingBackgroundWorkForThread`: retained work in the thread's root turns or
/// their descendants, or a child turn still active under one.
pub fn has_pending_background_work_for_thread(state: &RuntimeState, thread: &str) -> bool {
    let roots: Vec<&str> = state
        .turns
        .values()
        .chain(state.settled.values())
        .filter(|turn| turn.subagent.is_none() && turn.input.provider_thread.id.as_str() == thread)
        .map(|turn| turn.native_turn.as_str())
        .collect();
    if roots.is_empty() {
        return false;
    }
    state
        .turns
        .values()
        .chain(state.settled.values())
        .filter(|turn| {
            let ancestors = subagent::ancestors_of(state, turn);
            ancestors
                .iter()
                .any(|native| roots.contains(&native.as_str()))
        })
        .any(|turn| {
            has_retained_work(turn)
                || (turn.subagent.is_some() && state.turns.contains_key(&turn.native_turn))
        })
}

/// A frame for a turn that completed with background work still running. Upstream
/// resolves such a turn only for `item/completed`. A settled turn's text emits
/// await an active turn, so text completed on it is dropped. Work it still runs
/// (commands, persistent tools) completes. The turn is released once no work is left.
fn project_settled(
    raw: &CodexEvent,
    thread: &str,
    native_turn: &str,
    state: &mut RuntimeState,
    now: UtcDateTime,
) -> Vec<ProviderAdapterV2Event> {
    if raw.method != "item/completed" {
        return vec![];
    }
    let Some(mut turn) = state.settled.remove(native_turn) else {
        return vec![];
    };
    let text_item = matches!(
        raw.params.pointer("/item/type").and_then(Value::as_str),
        Some("agentMessage" | "reasoning" | "plan")
    );
    let events = if text_item {
        vec![]
    } else {
        item_completed(&raw.params, thread, &mut turn, state, now)
    };
    if has_retained_work(&turn) {
        state.settled.insert(native_turn.to_owned(), turn);
    }
    events
}

/// Composite key for one reasoning part, as upstream `reasoningPartKey`.
fn reasoning_key(native_turn: &str, native_item: &str, stream: &str, index: u64) -> String {
    serde_json::to_string(&json!([native_turn, native_item, stream, index])).expect("reasoning key")
}

fn item_started(
    params: &Value,
    thread: &str,
    turn: &mut TurnState,
    state: &mut RuntimeState,
    now: UtcDateTime,
) -> Vec<ProviderAdapterV2Event> {
    let Some(item) = params.get("item") else {
        return vec![];
    };
    let kind = str_of(item, "type").unwrap_or_default();
    let started_at = time_ms(params.get("startedAtMs").and_then(Value::as_i64));
    let native_item = str_of(item, "id").unwrap_or_default();
    // Upstream handles these before it resolves an item position.
    match kind {
        "contextCompaction" => return compaction(turn, native_item, started_at, false, now),
        "userMessage" => return subagent::child_user_message(state, turn, item, started_at),
        "subAgentActivity" => return sub_agent_activity(item, thread, turn, state),
        _ => {}
    }
    let mut events = retry::complete(turn, now);
    if kind != "reasoning" {
        items::resolve_position(turn, native_item, started_at, now);
    }
    events.extend(match kind {
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
    });
    events
}

/// Upstream `subAgentActivity` on `item/started`: a `started` activity registers
/// the subagent under this turn, and a terminal one settles the subagent it names.
fn sub_agent_activity(
    item: &Value,
    thread: &str,
    turn: &mut TurnState,
    state: &mut RuntimeState,
) -> Vec<ProviderAdapterV2Event> {
    let agent_thread = str_of(item, "agentThreadId").unwrap_or_default();
    let call = str_of(item, "id").unwrap_or_default();
    state
        .usage
        .entry(thread.to_owned())
        .or_default()
        .mark_subagent(&turn.native_turn);
    match str_of(item, "kind").unwrap_or_default() {
        "started" => {
            let ordinal = subagent::spawned_under(state, &turn.input.root_node_id) + 1;
            let spawn = subagent::Spawn {
                native_thread: agent_thread,
                native_item: format!("{call}:{agent_thread}"),
                native_tool_call: call,
                prompt: "",
                title: str_of(item, "agentPath"),
                model: None,
                effort: None,
                ordinal,
                emit_initial_prompt: false,
            };
            subagent::register(state, turn, spawn)
        }
        "completed" => subagent::activity_finished(
            state,
            agent_thread,
            OrchestrationV2SubagentStatus::Completed,
        ),
        "interrupted" => subagent::activity_finished(
            state,
            agent_thread,
            OrchestrationV2SubagentStatus::Interrupted,
        ),
        _ => vec![],
    }
}

/// The questions an async agent message asks, when it asks any.
fn async_questions(item: &Value) -> Option<&Vec<Value>> {
    if str_of(item, "delivery") != Some("async") {
        return None;
    }
    item.get("questions")
        .and_then(Value::as_array)
        .filter(|questions| !questions.is_empty())
}

/// Upstream's async-question branch of `item/completed`. The questions take the
/// builder's shape, each titled `Question`, and are answered by a message. No
/// runtime request is registered for a reply on the session.
fn async_question(
    turn: &mut TurnState,
    native_item: &str,
    questions: &[Value],
    now: UtcDateTime,
) -> Vec<ProviderAdapterV2Event> {
    let codex_shape: Vec<Value> = questions
        .iter()
        .enumerate()
        .map(|(index, question)| {
            let options: Vec<Value> = question
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|label| json!({"label": label, "description": ""}))
                .collect();
            json!({
                "id": index.to_string(),
                "header": "Question",
                "question": str_of(question, "title").unwrap_or_default(),
                "options": options,
            })
        })
        .collect();
    let artifacts = items::user_input_request(
        turn,
        native_item,
        &format!("async:{native_item}"),
        items::codex_questions(&Value::Array(codex_shape)),
        None,
        now,
    );
    vec![
        ProviderAdapterV2Event::NodeUpdated {
            driver: driver(),
            node: artifacts.node,
        },
        ProviderAdapterV2Event::TurnItemUpdated {
            driver: driver(),
            turn_item: artifacts.turn_item,
        },
        ProviderAdapterV2Event::RuntimeRequestUpdated {
            driver: driver(),
            thread_id: Some(turn.input.thread_id.clone()),
            runtime_request: artifacts.request,
        },
    ]
}

/// Upstream `emitCompactionItem`: a context compaction is one turn item with no
/// node. It is titled while it runs and again once it completes.
fn compaction(
    turn: &mut TurnState,
    native_item: &str,
    started_at: Option<UtcDateTime>,
    completed: bool,
    now: UtcDateTime,
) -> Vec<ProviderAdapterV2Event> {
    let (ordinal, started) = items::resolve_position(turn, native_item, started_at, now);
    let turn_item = OrchestrationV2TurnItem::Compaction {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: derive::turn_item_from_provider_item(&driver(), native_item),
        thread_id: turn.input.thread_id.clone(),
        run_id: turn.run_id(),
        node_id: Some(turn.input.root_node_id.clone()),
        provider_thread_id: Some(turn.input.provider_thread.id.clone()),
        provider_turn_id: Some(derive::provider_turn(&driver(), &turn.native_turn)),
        native_item_ref: Some(items::native_item_ref(native_item)),
        parent_item_id: None,
        ordinal,
        status: if completed {
            OrchestrationV2TurnItemStatus::Completed
        } else {
            OrchestrationV2TurnItemStatus::Running
        },
        title: Some(
            if completed {
                "Context compacted"
            } else {
                "Compacting context"
            }
            .to_owned(),
        ),
        started_at: Some(started),
        completed_at: completed.then_some(now),
        updated_at: now,
        driver: Some(driver()),
        summary: None,
        before_token_count: None,
        after_token_count: None,
    };
    vec![ProviderAdapterV2Event::TurnItemUpdated {
        driver: driver(),
        turn_item,
    }]
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
    thread: &str,
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
        "contextCompaction" => compaction(turn, &native_item, None, true, now),
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
            // An async message with questions asks them, and its text is not projected.
            if let Some(questions) = async_questions(item) {
                turn.final_answer_items.retain(|id| id != &native_item);
                return async_question(turn, &native_item, questions, now);
            }
            let final_text = str_of(item, "text").unwrap_or_default().to_owned();
            let final_answer = str_of(item, "phase") != Some("commentary");
            let mut events = complete_text(
                state,
                turn,
                &native_item,
                Some(final_text.clone()),
                true,
                false,
            );
            turn.final_answer_items.retain(|id| id != &native_item);
            // A child's final answer is its subagent's result.
            if final_answer
                && !events.is_empty()
                && let Some(thread) = turn.subagent.clone()
            {
                events.extend(subagent::set_result(state, &thread, &final_text));
            }
            events
        }
        "userMessage" => subagent::child_user_message(state, turn, item, None),
        "subAgentActivity" => {
            let status = match str_of(item, "kind") {
                Some("completed") => OrchestrationV2SubagentStatus::Completed,
                Some("interrupted") => OrchestrationV2SubagentStatus::Interrupted,
                _ => return vec![],
            };
            subagent::activity_finished(
                state,
                str_of(item, "agentThreadId").unwrap_or_default(),
                status,
            )
        }
        "collabAgentToolCall" => {
            let mut events = vec![];
            if str_of(item, "tool") == Some("spawnAgent") {
                state
                    .usage
                    .entry(thread.to_owned())
                    .or_default()
                    .mark_subagent(&turn.native_turn);
                events.extend(spawn_agents(item, turn, state, &native_item));
            }
            events.extend(subagent::update_states(
                state,
                item.get("agentsStates").unwrap_or(&Value::Null),
            ));
            events
        }
        _ => vec![],
    }
}

/// Upstream `registerSubagentThreads`: each thread a `spawnAgent` call received
/// becomes a subagent, numbered in the order the call lists them.
fn spawn_agents(
    item: &Value,
    turn: &mut TurnState,
    state: &mut RuntimeState,
    call: &str,
) -> Vec<ProviderAdapterV2Event> {
    let prompt = str_of(item, "prompt").unwrap_or_default();
    let model = str_of(item, "model").filter(|model| !model.is_empty());
    let mut events = vec![];
    for (index, agent_thread) in item
        .get("receiverThreadIds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .enumerate()
    {
        events.extend(subagent::register(
            state,
            turn,
            subagent::Spawn {
                native_thread: agent_thread,
                native_item: format!("{call}:{agent_thread}"),
                native_tool_call: call,
                prompt,
                title: None,
                model,
                effort: Some(str_of(item, "reasoningEffort")),
                ordinal: index + 1,
                emit_initial_prompt: true,
            },
        ));
    }
    events
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
                .map(js_trim)
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
        .map(js_trim)
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
        run_id: turn.run_id(),
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
    let native_status = params.pointer("/turn/status").and_then(Value::as_str);
    // A turn Stop is interrupting ends as interrupted, even when Codex reports it completed.
    let native_status = match native_status {
        Some("completed") if state.interrupting.contains(&turn.native_turn) => Some("interrupted"),
        other => other,
    };
    let (status, terminal) = match native_status {
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
    if status != OrchestrationV2ProviderTurnStatus::Completed {
        state.terminalized.insert(turn.native_turn.clone());
    }
    state.finished.insert(turn.native_turn.clone());
    let retry = turn.provider_retry.take();
    let mut events = retry::finish(turn, retry.clone(), status, now);
    events.extend(flush_turn(state, turn, stopped_early));
    // Commands still running when an interrupted or failed turn ends were stopped with
    // it. A completed turn keeps the commands it still runs, so their completion lands.
    let stopped = if stopped_early {
        std::mem::take(&mut turn.running_commands)
    } else {
        vec![]
    };
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
    // when the turn ended cleanly. The turn is then retained, so the tool completes later.
    let (tool_node_status, tool_item_status) = stopped_item_statuses(status);
    let (kept, stopped_tools): (Vec<Value>, Vec<Value>) = std::mem::take(&mut turn.running_tools)
        .into_iter()
        .partition(|tool| !stopped_early && is_persistent_tool(tool));
    turn.running_tools = kept;
    for tool in stopped_tools {
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
    let (failure, failure_item_ordinal) = match status {
        OrchestrationV2ProviderTurnStatus::Failed => {
            let message = params
                .pointer("/turn/error/message")
                .and_then(Value::as_str);
            let code = message
                .and_then(|_| retry::error_info_code(params.pointer("/turn/error/codexErrorInfo")));
            let (failure, ordinal) =
                retry::terminal_failure(turn, retry.as_ref(), message, code.as_deref());
            (Some(failure), Some(ordinal))
        }
        _ => (None, None),
    };
    let failed = failure.is_some();
    let completed_at = time_secs(params.pointer("/turn/completedAt")).unwrap_or(now);
    let provider = provider_turn(turn, status, Some(completed_at), None, token);
    let provider_event = ProviderAdapterV2Event::ProviderTurnUpdated {
        driver: driver(),
        thread_id: Some(turn.input.thread_id.clone()),
        provider_turn: provider,
    };
    // A clean end with background work keeps the turn, so its late item frames land.
    if status == OrchestrationV2ProviderTurnStatus::Completed && has_retained_work(turn) {
        state.settled.insert(turn.native_turn.clone(), turn.clone());
    }
    // A child turn settles its subagent. Only a top-level turn ends a run.
    if turn.subagent.is_some() {
        events.push(provider_event);
        events.extend(subagent::finish_child_turn(
            state,
            turn,
            status,
            completed_at,
        ));
        return events;
    }
    let terminal_event = ProviderAdapterV2Event::TurnTerminal {
        driver: driver(),
        provider_thread_id: turn.input.provider_thread.id.clone(),
        provider_turn_id: derive::provider_turn(&driver(), &turn.native_turn),
        run_ordinal: PositiveInt::new(turn.input.run_ordinal.max(1) as u64)
            .expect("positive ordinal"),
        status: terminal,
        failure,
        failure_item_ordinal,
        retry: failed
            .then(|| retry.as_ref().map(|retry| retry.retry.clone()))
            .flatten(),
        retry_started_at: failed
            .then(|| retry.as_ref().map(|retry| retry.started_at))
            .flatten(),
        thread_disposition: OrchestrationV2ProviderThreadDisposition::Reusable,
    };
    // A clean end with an active goal is held: Codex starts the goal's next turn itself.
    let thread = str_of(params, "threadId").unwrap_or_default().to_owned();
    if status == OrchestrationV2ProviderTurnStatus::Completed && goal::holds_run(state, &thread) {
        goal::hold(
            state,
            &thread,
            goal::GoalHold {
                input: turn.input.clone(),
                completed: provider_event,
                terminal: terminal_event,
                deadline: Instant::now() + goal::CONTINUATION_GRACE,
            },
        );
        return events;
    }
    events.push(provider_event);
    events.push(terminal_event);
    events
}
