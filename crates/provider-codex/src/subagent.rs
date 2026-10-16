//! Port of the subagent half of upstream/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts:
//! `registerSubagentThread` (from a `subAgentActivity` or a `spawnAgent` collab call),
//! `emitSubagentProviderTurnStarted`, `emitSubagentTaskUpdate`, `updateSubagentStates`,
//! and `emitSubagentUserMessage`, with `makeSubagentChildThread` and
//! `subagentThreadTitle` from provider-core's `subagentProjection.ts`.
//!
//! A child turn is an ordinary `TurnState` with `subagent` set, so the item and
//! delta handlers project it unchanged. Its `run_id` and `attempt_id` are empty,
//! as upstream's child contexts have no run. Upstream's `CodexSubagentThreadContext`
//! is `SubagentThread`, and its `parentContext` is `ParentLink`.
use crate::{
    items,
    orchestration_adapter::{driver, native_ref},
    projection::{self, RuntimeState, TurnState},
};
use serde_json::{Value, json};
use std::collections::HashSet;
use turin_contracts::base_schemas::js_trim;
use turin_contracts::{
    NodeId, ProviderThreadId, ProviderTurnId, RunId, ThreadId, TrimmedNonEmptyString, TurnItemId,
    UtcDateTime,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2AppThread, OrchestrationV2AppThreadLineage,
        OrchestrationV2ConversationMessage, OrchestrationV2ConversationMessageRole,
        OrchestrationV2CreationSource, OrchestrationV2ExecutionNode,
        OrchestrationV2ExecutionNodeKind, OrchestrationV2ExecutionNodeStatus,
        OrchestrationV2ForkSource, OrchestrationV2LineageRelationship,
        OrchestrationV2ProviderThread, OrchestrationV2ProviderThreadForkedFrom,
        OrchestrationV2ProviderThreadStatus, OrchestrationV2ProviderTurnStatus,
        OrchestrationV2Subagent, OrchestrationV2SubagentOrigin, OrchestrationV2SubagentStatus,
        OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
        OrchestrationV2UserMessageInputIntent,
    },
    provider_instance::ProviderInstanceId,
};
use turin_provider_core::{
    id_allocator::derive,
    provider_adapter::{ProviderAdapterV2Event, ProviderAdapterV2TurnInput},
};

/// The turn a subagent's work hangs off: upstream's `parentContext`. A child
/// turn that starts again re-parents to the parent thread's active turn.
#[derive(Clone)]
pub struct ParentLink {
    thread_id: ThreadId,
    run_id: Option<RunId>,
    root_node: NodeId,
    provider_turn: ProviderTurnId,
    /// The parent's input, which child turn inputs are derived from.
    input: ProviderAdapterV2TurnInput,
    /// The top-level turn that approvals from this subagent are asked on.
    owner_native_turn: String,
    /// The native subagent thread the parent turn runs in, when the parent is a child turn.
    subagent: Option<String>,
    /// The parent turn, then the turns above it, through each subagent's parent.
    ancestors: Vec<String>,
}

/// One native subagent thread: upstream's `CodexSubagentThreadContext`.
pub struct SubagentThread {
    parent_native_thread: String,
    parent: ParentLink,
    /// The item that created the subagent: its node's native reference.
    native_tool_call: String,
    child_thread: ThreadId,
    child_app_thread: OrchestrationV2AppThread,
    child_root_node: NodeId,
    node: NodeId,
    provider_thread: OrchestrationV2ProviderThread,
    turn_item_id: TurnItemId,
    turn_item_ordinal: u64,
    started_at: UtcDateTime,
    /// Child provider turns started so far; the next one takes `turn_count + 1`.
    turn_count: i64,
    native_turns: HashSet<String>,
    task: OrchestrationV2Subagent,
}

/// What a `subAgentActivity` or `spawnAgent` entry supplies to `register`.
pub struct Spawn<'a> {
    pub native_thread: &'a str,
    /// Upstream's `nativeItemId`: `<item id>:<agent thread id>`.
    pub native_item: String,
    pub native_tool_call: &'a str,
    pub prompt: &'a str,
    /// `agentPath` for a `subAgentActivity`; a spawn has no title.
    pub title: Option<&'a str>,
    pub model: Option<&'a str>,
    /// `Some(effort)` when the spawn reported one, null included. `None` when it did not.
    pub effort: Option<Option<&'a str>>,
    pub ordinal: usize,
    /// Whether the spawn's prompt becomes a user message in the child thread.
    pub emit_initial_prompt: bool,
}

/// A task update, as upstream's `emitSubagentTaskUpdate` input.
#[derive(Default)]
pub struct TaskUpdate {
    pub status: Option<OrchestrationV2SubagentStatus>,
    /// `Some(None)` clears the result, as upstream's `result: null` does.
    pub result: Option<Option<String>>,
    pub completed_at: Option<UtcDateTime>,
    pub started_at: Option<UtcDateTime>,
    pub reopen: bool,
}

/// The native turn that owns a turn's approvals: the top-level turn for a child.
pub fn owner_native_turn(state: &RuntimeState, turn: &TurnState) -> String {
    match &turn.subagent {
        Some(thread) => state.subagents.get(thread).map_or_else(
            || turn.native_turn.clone(),
            |sub| sub.parent.owner_native_turn.clone(),
        ),
        None => turn.native_turn.clone(),
    }
}

/// The top-level subagent node a child turn's work runs under: the one on the
/// owner's thread that leads to the turn. Approvals from the turn parent under it.
/// Upstream's `approvalOwnerCodexTurn` walks up to the last subagent on the way.
pub fn subagent_node(state: &RuntimeState, turn: &TurnState) -> Option<NodeId> {
    let mut node = None;
    let mut thread = turn.subagent.clone();
    while let Some(current) = thread {
        let sub = state.subagents.get(&current)?;
        node = Some(sub.node.clone());
        thread = sub.parent.subagent.clone();
    }
    node
}

/// Whether a subagent's task is running (`subagent.task.status === "running"`).
pub fn has_running(state: &RuntimeState) -> bool {
    state
        .subagents
        .values()
        .any(|sub| sub.task.status == OrchestrationV2SubagentStatus::Running)
}

/// The turn and every turn above it, through the subagents it runs under.
pub(crate) fn ancestors_of(state: &RuntimeState, turn: &TurnState) -> Vec<String> {
    let mut ancestors = vec![turn.native_turn.clone()];
    if let Some(sub) = turn
        .subagent
        .as_ref()
        .and_then(|thread| state.subagents.get(thread))
    {
        ancestors.extend(sub.parent.ancestors.iter().cloned());
    }
    ancestors
}

fn link_of(state: &RuntimeState, turn: &TurnState) -> ParentLink {
    ParentLink {
        thread_id: turn.input.thread_id.clone(),
        run_id: turn.run_id(),
        root_node: turn.input.root_node_id.clone(),
        provider_turn: derive::provider_turn(&driver(), &turn.native_turn),
        input: turn.input.clone(),
        owner_native_turn: owner_native_turn(state, turn),
        subagent: turn.subagent.clone(),
        ancestors: ancestors_of(state, turn),
    }
}

/// The native thread id of a provider thread.
fn native_thread_of(thread: &OrchestrationV2ProviderThread) -> Option<String> {
    thread
        .native_thread_ref
        .as_ref()?
        .native_id
        .as_ref()
        .map(|id| id.as_str().to_owned())
}

/// The model a child thread reported, as upstream's `subagentSelections` entry: the
/// model and its options, without the instance.
#[derive(Clone)]
pub struct ReportedSelection {
    model: String,
    options: Option<Vec<Value>>,
}

/// The reported selection as a model selection of the subagent's instance.
fn model_selection_of(
    instance: &ProviderInstanceId,
    selection: &ReportedSelection,
) -> ModelSelection {
    let mut value = json!({"instanceId": instance, "model": selection.model});
    if let Some(options) = &selection.options {
        value["options"] = Value::Array(options.clone());
    }
    serde_json::from_value(value).expect("model selection")
}

/// Upstream `updateSubagentSelection`. It records the model a child thread reported,
/// and updates its subagent's task when the selection changes. `effort` and `tier`
/// are `None` when the event did not carry them, and `Some(None)` when it carried null.
pub fn update_selection(
    state: &mut RuntimeState,
    thread: &str,
    model: Option<&str>,
    effort: Option<Option<&str>>,
    tier: Option<Option<&str>>,
) -> Vec<ProviderAdapterV2Event> {
    let Some(model) = model.map(js_trim).filter(|model| !model.is_empty()) else {
        return vec![];
    };
    let previous_options = state
        .subagent_selections
        .get(thread)
        .and_then(|selection| selection.options.clone());
    let options = if effort.is_none() && tier.is_none() {
        previous_options
    } else {
        let mut kept: Vec<Value> = previous_options
            .unwrap_or_default()
            .into_iter()
            .filter(|option| match option["id"].as_str() {
                Some("reasoningEffort") => effort.is_none(),
                Some("serviceTier") => tier.is_none(),
                _ => false,
            })
            .collect();
        for (id, value) in [
            ("reasoningEffort", effort.flatten()),
            ("serviceTier", tier.flatten()),
        ] {
            if let Some(value) = value.map(js_trim).filter(|value| !value.is_empty()) {
                kept.push(json!({"id": id, "value": value}));
            }
        }
        Some(kept)
    };
    let selection = ReportedSelection {
        model: model.to_owned(),
        options,
    };
    state
        .subagent_selections
        .insert(thread.to_owned(), selection.clone());
    let Some(sub) = state.subagents.get_mut(thread) else {
        return vec![];
    };
    let model_selection = model_selection_of(&sub.task.provider_instance_id, &selection);
    if sub.task.model_selection.as_ref().is_some_and(|known| {
        serde_json::to_value(known).ok() == serde_json::to_value(&model_selection).ok()
    }) {
        return vec![];
    }
    sub.task.model = Some(selection.model);
    sub.task.model_selection = Some(model_selection);
    sub.task.updated_at = UtcDateTime::now();
    vec![ProviderAdapterV2Event::SubagentUpdated {
        driver: driver(),
        subagent: sub.task.clone(),
    }]
}

/// Upstream's `thread/settings/updated` and `model/rerouted`: a thread's reported
/// model, and its effort and service tier when the event carries them.
pub fn settings_updated(
    state: &mut RuntimeState,
    method: &str,
    params: &Value,
) -> Vec<ProviderAdapterV2Event> {
    let thread = params
        .get("threadId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method == "model/rerouted" {
        return update_selection(
            state,
            thread,
            params.get("toModel").and_then(Value::as_str),
            None,
            None,
        );
    }
    let Some(settings) = params.get("threadSettings") else {
        return vec![];
    };
    let carried = |key: &str| settings.get(key).map(Value::as_str);
    update_selection(
        state,
        thread,
        settings.get("model").and_then(Value::as_str),
        carried("effort"),
        carried("serviceTier"),
    )
}

/// Upstream `subagentThreadTitle`.
fn thread_title(parent_title: &str, title: Option<&str>, prompt: &str, ordinal: usize) -> String {
    let trimmed = |text: &str| {
        let text = js_trim(text);
        (!text.is_empty()).then(|| text.to_owned())
    };
    match title.and_then(trimmed).or_else(|| trimmed(prompt)) {
        None => format!("{parent_title} subagent {ordinal}"),
        Some(detail) if detail.chars().count() > 72 => {
            format!("{}...", detail.chars().take(69).collect::<String>())
        }
        Some(detail) => detail,
    }
}

/// Upstream `isOrchestrationV2WorkActive`, for subagent statuses.
fn is_active(status: OrchestrationV2SubagentStatus) -> bool {
    matches!(
        status,
        OrchestrationV2SubagentStatus::Pending
            | OrchestrationV2SubagentStatus::Running
            | OrchestrationV2SubagentStatus::Waiting
    )
}

/// The node and task status a child's provider turn ends with.
fn settled_status(status: OrchestrationV2ProviderTurnStatus) -> OrchestrationV2SubagentStatus {
    match status {
        OrchestrationV2ProviderTurnStatus::Completed => OrchestrationV2SubagentStatus::Completed,
        OrchestrationV2ProviderTurnStatus::Interrupted => {
            OrchestrationV2SubagentStatus::Interrupted
        }
        OrchestrationV2ProviderTurnStatus::Failed => OrchestrationV2SubagentStatus::Failed,
        OrchestrationV2ProviderTurnStatus::Cancelled => OrchestrationV2SubagentStatus::Cancelled,
        OrchestrationV2ProviderTurnStatus::Pending | OrchestrationV2ProviderTurnStatus::Running => {
            OrchestrationV2SubagentStatus::Running
        }
    }
}

fn node_status(status: OrchestrationV2SubagentStatus) -> OrchestrationV2ExecutionNodeStatus {
    match status {
        OrchestrationV2SubagentStatus::Idle => OrchestrationV2ExecutionNodeStatus::Idle,
        OrchestrationV2SubagentStatus::Pending => OrchestrationV2ExecutionNodeStatus::Pending,
        OrchestrationV2SubagentStatus::Running => OrchestrationV2ExecutionNodeStatus::Running,
        OrchestrationV2SubagentStatus::Waiting => OrchestrationV2ExecutionNodeStatus::Waiting,
        OrchestrationV2SubagentStatus::Completed => OrchestrationV2ExecutionNodeStatus::Completed,
        OrchestrationV2SubagentStatus::Failed => OrchestrationV2ExecutionNodeStatus::Failed,
        OrchestrationV2SubagentStatus::Cancelled => OrchestrationV2ExecutionNodeStatus::Cancelled,
        OrchestrationV2SubagentStatus::Interrupted => {
            OrchestrationV2ExecutionNodeStatus::Interrupted
        }
    }
}

fn item_status(status: OrchestrationV2SubagentStatus) -> OrchestrationV2TurnItemStatus {
    match status {
        OrchestrationV2SubagentStatus::Idle => OrchestrationV2TurnItemStatus::Idle,
        OrchestrationV2SubagentStatus::Pending => OrchestrationV2TurnItemStatus::Pending,
        OrchestrationV2SubagentStatus::Running => OrchestrationV2TurnItemStatus::Running,
        OrchestrationV2SubagentStatus::Waiting => OrchestrationV2TurnItemStatus::Waiting,
        OrchestrationV2SubagentStatus::Completed => OrchestrationV2TurnItemStatus::Completed,
        OrchestrationV2SubagentStatus::Failed => OrchestrationV2TurnItemStatus::Failed,
        OrchestrationV2SubagentStatus::Cancelled => OrchestrationV2TurnItemStatus::Cancelled,
        OrchestrationV2SubagentStatus::Interrupted => OrchestrationV2TurnItemStatus::Interrupted,
    }
}

/// Upstream's `collabAgentToolCall` `agentsStates` status names.
fn collab_status(status: &str) -> Option<OrchestrationV2SubagentStatus> {
    Some(match status {
        "pendingInit" => OrchestrationV2SubagentStatus::Pending,
        "running" => OrchestrationV2SubagentStatus::Running,
        "interrupted" => OrchestrationV2SubagentStatus::Interrupted,
        "completed" => OrchestrationV2SubagentStatus::Completed,
        "errored" | "notFound" => OrchestrationV2SubagentStatus::Failed,
        "shutdown" => OrchestrationV2SubagentStatus::Cancelled,
        _ => return None,
    })
}

fn subagent_turn_item(sub: &SubagentThread) -> OrchestrationV2TurnItem {
    let task = &sub.task;
    OrchestrationV2TurnItem::Subagent {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: sub.turn_item_id.clone(),
        thread_id: task.thread_id.clone(),
        run_id: task.run_id.clone(),
        node_id: Some(task.id.clone()),
        provider_thread_id: task.provider_thread_id.clone(),
        provider_turn_id: Some(sub.parent.provider_turn.clone()),
        native_item_ref: task.native_task_ref.clone(),
        parent_item_id: None,
        ordinal: sub.turn_item_ordinal,
        status: item_status(task.status),
        title: task.title.clone(),
        started_at: task.started_at,
        completed_at: task.completed_at,
        updated_at: task.updated_at,
        subagent_id: task.id.clone(),
        origin: task.origin,
        driver: task.driver.clone(),
        provider_instance_id: task.provider_instance_id.clone(),
        child_thread_id: task.child_thread_id.clone(),
        prompt: task.prompt.clone(),
        progress: task.progress.clone(),
        result: task.result.clone(),
    }
}

/// Upstream `emitSubagentTaskUpdate`. A snapshot that arrives after the task
/// settled cannot reopen it, unless it is a new child turn (`reopen`).
fn emit_task(sub: &mut SubagentThread, update: TaskUpdate) -> Vec<ProviderAdapterV2Event> {
    let now = UtcDateTime::now();
    let prior = sub.task.status;
    let requested = update.status.unwrap_or(prior);
    let settled = !is_active(prior);
    if settled && is_active(requested) && !update.reopen {
        return vec![];
    }
    let status = if settled && !update.reopen {
        prior
    } else {
        requested
    };
    if update.reopen {
        sub.task.run_id = sub.parent.run_id.clone();
        sub.task.parent_node_id = sub.parent.root_node.clone();
    }
    sub.task.status = status;
    sub.task.completed_at = if is_active(status) {
        None
    } else {
        sub.task.completed_at.or(update.completed_at).or(Some(now))
    };
    if let Some(started_at) = update.started_at {
        sub.started_at = started_at;
        sub.task.started_at = Some(started_at);
    }
    if let Some(result) = update.result {
        sub.task.result = result;
    }
    sub.task.updated_at = now;
    vec![
        ProviderAdapterV2Event::SubagentUpdated {
            driver: driver(),
            subagent: sub.task.clone(),
        },
        ProviderAdapterV2Event::TurnItemUpdated {
            driver: driver(),
            turn_item: subagent_turn_item(sub),
        },
    ]
}

/// Where a user message lands: the thread, its root node, and the provider turn.
struct MessageSite<'a> {
    thread_id: &'a ThreadId,
    root_node: &'a NodeId,
    provider_thread: &'a ProviderThreadId,
    provider_turn: Option<ProviderTurnId>,
}

/// The user message a subagent's prompt or a child turn's input becomes, as
/// upstream's `makeSubagentConversationArtifacts` builds it for the user role.
fn user_message(
    sender: &ThreadId,
    site: MessageSite,
    native_item: &str,
    text: &str,
    ordinal: u64,
    now: UtcDateTime,
) -> Vec<ProviderAdapterV2Event> {
    let MessageSite {
        thread_id,
        root_node,
        provider_thread,
        provider_turn,
    } = site;
    let message_id = derive::message_from_provider_item(&driver(), native_item);
    let turn_item_id = derive::turn_item_from_provider_item(&driver(), native_item);
    let message = OrchestrationV2ConversationMessage {
        notification: None,
        delegated_completion: None,
        created_by: OrchestrationV2Actor::Agent,
        creation_source: OrchestrationV2CreationSource::Provider,
        scheduled_task_id: None,
        sender_thread_id: Some(sender.clone()),
        id: message_id.clone(),
        thread_id: thread_id.clone(),
        run_id: None,
        node_id: Some(root_node.clone()),
        role: OrchestrationV2ConversationMessageRole::User,
        text: text.to_owned(),
        context: None,
        attachments: vec![],
        streaming: false,
        created_at: now,
        updated_at: now,
    };
    let turn_item = OrchestrationV2TurnItem::UserMessage {
        tool_non_execution_kind: None,
        tool_surface: None,
        tool_icon: None,
        tool_source: None,
        id: turn_item_id,
        thread_id: thread_id.clone(),
        run_id: None,
        node_id: Some(root_node.clone()),
        provider_thread_id: Some(provider_thread.clone()),
        provider_turn_id: provider_turn,
        native_item_ref: Some(items::native_item_ref(native_item)),
        parent_item_id: None,
        ordinal,
        status: OrchestrationV2TurnItemStatus::Completed,
        title: None,
        started_at: Some(now),
        completed_at: Some(now),
        updated_at: now,
        created_by: OrchestrationV2Actor::Agent,
        creation_source: OrchestrationV2CreationSource::Provider,
        message_id,
        scheduled_task_id: None,
        sender_thread_id: Some(sender.clone()),
        input_intent: OrchestrationV2UserMessageInputIntent::TurnStart,
        text: text.to_owned(),
        context: None,
        attachments: vec![],
    };
    vec![
        ProviderAdapterV2Event::MessageUpdated {
            driver: driver(),
            message,
        },
        ProviderAdapterV2Event::TurnItemUpdated {
            driver: driver(),
            turn_item,
        },
    ]
}

/// Registers a native subagent under `parent`, the turn that spawned it. Upstream's
/// `registerSubagentThread`: the child thread, its root node, its provider thread,
/// the subagent node, and the task, then the initial prompt. Idempotent per thread.
pub fn register(
    state: &mut RuntimeState,
    parent: &mut TurnState,
    spawn: Spawn,
) -> Vec<ProviderAdapterV2Event> {
    if state.subagents.contains_key(spawn.native_thread) {
        return vec![];
    }
    let now = UtcDateTime::now();
    let instance = parent.input.model_selection.instance_id.clone();
    let link = link_of(state, parent);
    let parent_native_thread = native_thread_of(&parent.input.provider_thread).unwrap_or_default();
    let (turn_item_ordinal, _) = items::resolve_position(parent, &spawn.native_item, None, now);

    let node = derive::node_from_provider_item(&driver(), &spawn.native_item);
    let child_root =
        derive::node_from_provider_item(&driver(), &format!("{}:thread-root", spawn.native_item));
    let child_thread = derive::thread_from_provider_thread(&driver(), None, spawn.native_thread);
    let provider_thread_id = derive::provider_thread(&driver(), None, spawn.native_thread);
    let turn_item_id = derive::turn_item_from_provider_item(&driver(), &spawn.native_item);
    // Upstream `registerSubagentThread`: a spawn's model and effort are the child's
    // selection, unless the child has already reported one. The subagent is not
    // registered yet, so the update only records the selection.
    if let Some(model) = spawn.model
        && !state.subagent_selections.contains_key(spawn.native_thread)
    {
        update_selection(state, spawn.native_thread, Some(model), spawn.effort, None);
    }
    let reported = state.subagent_selections.get(spawn.native_thread).cloned();
    let model = reported
        .as_ref()
        .map(|selection| selection.model.clone())
        .or_else(|| spawn.model.map(str::to_owned));
    let task_selection = reported
        .as_ref()
        .map(|selection| model_selection_of(&instance, selection));

    let mut child_selection: ModelSelection = parent.input.model_selection.clone();
    if let Some(model) = &model {
        child_selection.model = TrimmedNonEmptyString::new(model).expect("nonempty model");
    }
    let mut child_app = parent.input.app_thread.clone();
    child_app.created_by = OrchestrationV2Actor::Agent;
    child_app.creation_source = OrchestrationV2CreationSource::Provider;
    child_app.id = child_thread.clone();
    child_app.title = TrimmedNonEmptyString::new(thread_title(
        parent.input.app_thread.title.as_str(),
        spawn.title,
        spawn.prompt,
        spawn.ordinal,
    ))
    .expect("nonempty title");
    child_app.linked_pull_request = Some(None);
    child_app.pull_requests = Some(vec![]);
    child_app.history_origin = None;
    child_app.provider_instance_id = instance.clone();
    child_app.model_selection = child_selection.clone();
    child_app.active_provider_thread_id = Some(provider_thread_id.clone());
    child_app.lineage = OrchestrationV2AppThreadLineage {
        parent_thread_id: Some(link.thread_id.clone()),
        relationship_to_parent: Some(OrchestrationV2LineageRelationship::Subagent),
        root_thread_id: parent.input.app_thread.lineage.root_thread_id.clone(),
    };
    child_app.forked_from = Some(OrchestrationV2ForkSource::Node {
        node_id: node.clone(),
    });
    child_app.created_at = now;
    child_app.updated_at = now;
    child_app.archived_at = None;

    let provider_thread = OrchestrationV2ProviderThread {
        id: provider_thread_id.clone(),
        driver: driver(),
        provider_instance_id: instance.clone(),
        provider_session_id: parent.input.provider_thread.provider_session_id.clone(),
        app_thread_id: Some(child_thread.clone()),
        owner_node_id: None,
        native_thread_ref: Some(native_ref(spawn.native_thread)),
        native_conversation_head_ref: None,
        status: OrchestrationV2ProviderThreadStatus::Idle,
        first_run_ordinal: None,
        last_run_ordinal: None,
        handoff_ids: vec![],
        forked_from: Some(OrchestrationV2ProviderThreadForkedFrom {
            provider_thread_id: parent.input.provider_thread.id.clone(),
            provider_turn_id: Some(link.provider_turn.clone()),
            checkpoint_id: None,
        }),
        pending_background_tasks: vec![],
        context_usage: None,
        native_metadata: None,
        goal: None,
        created_at: now,
        updated_at: now,
    };
    let task = OrchestrationV2Subagent {
        id: node.clone(),
        thread_id: link.thread_id.clone(),
        run_id: link.run_id.clone(),
        parent_node_id: link.root_node.clone(),
        origin: OrchestrationV2SubagentOrigin::ProviderNative,
        created_by: OrchestrationV2Actor::Agent,
        driver: driver(),
        provider_instance_id: instance.clone(),
        provider_thread_id: Some(provider_thread_id.clone()),
        child_thread_id: Some(child_thread.clone()),
        native_task_ref: Some(items::native_item_ref(&spawn.native_item)),
        prompt: spawn.prompt.to_owned(),
        title: spawn.title.map(str::to_owned),
        model,
        model_selection: task_selection,
        completion_wake: None,
        completion_delivery: None,
        status: OrchestrationV2SubagentStatus::Running,
        progress: None,
        workflow: None,
        result: None,
        started_at: Some(now),
        completed_at: None,
        updated_at: now,
    };
    let mut sub = SubagentThread {
        parent_native_thread,
        parent: link.clone(),
        native_tool_call: spawn.native_tool_call.to_owned(),
        child_thread: child_thread.clone(),
        child_app_thread: child_app.clone(),
        child_root_node: child_root.clone(),
        node: node.clone(),
        provider_thread: provider_thread.clone(),
        turn_item_id,
        turn_item_ordinal,
        started_at: now,
        turn_count: 0,
        native_turns: HashSet::new(),
        task,
    };

    let mut events = vec![
        ProviderAdapterV2Event::AppThreadCreated {
            driver: driver(),
            app_thread: child_app,
        },
        ProviderAdapterV2Event::NodeUpdated {
            driver: driver(),
            node: OrchestrationV2ExecutionNode {
                id: node.clone(),
                thread_id: link.thread_id.clone(),
                run_id: link.run_id.clone(),
                parent_node_id: Some(link.root_node.clone()),
                root_node_id: link.root_node.clone(),
                kind: OrchestrationV2ExecutionNodeKind::Subagent,
                status: OrchestrationV2ExecutionNodeStatus::Running,
                counts_for_run: false,
                provider_thread_id: Some(provider_thread_id.clone()),
                provider_turn_id: Some(link.provider_turn.clone()),
                native_item_ref: Some(native_ref(spawn.native_tool_call)),
                runtime_request_id: None,
                checkpoint_scope_id: None,
                started_at: Some(now),
                completed_at: None,
            },
        },
        ProviderAdapterV2Event::NodeUpdated {
            driver: driver(),
            node: OrchestrationV2ExecutionNode {
                id: child_root.clone(),
                thread_id: child_thread.clone(),
                run_id: None,
                parent_node_id: None,
                root_node_id: child_root.clone(),
                kind: OrchestrationV2ExecutionNodeKind::RootTurn,
                status: OrchestrationV2ExecutionNodeStatus::Running,
                counts_for_run: false,
                provider_thread_id: Some(provider_thread_id.clone()),
                provider_turn_id: None,
                native_item_ref: Some(items::native_item_ref(&spawn.native_item)),
                runtime_request_id: None,
                checkpoint_scope_id: None,
                started_at: Some(now),
                completed_at: None,
            },
        },
        ProviderAdapterV2Event::ProviderThreadUpdated {
            driver: driver(),
            provider_thread,
            retained_native_turn_ids: None,
        },
    ];
    if spawn.emit_initial_prompt && !spawn.prompt.is_empty() {
        let prompt_item = format!("{}:prompt", spawn.native_item);
        events.extend(user_message(
            &link.thread_id,
            MessageSite {
                thread_id: &child_thread,
                root_node: &child_root,
                provider_thread: &provider_thread_id,
                provider_turn: None,
            },
            &prompt_item,
            spawn.prompt,
            100,
            now,
        ));
    }
    events.extend(emit_task(
        &mut sub,
        TaskUpdate {
            status: Some(OrchestrationV2SubagentStatus::Running),
            ..TaskUpdate::default()
        },
    ));
    state.subagents.insert(spawn.native_thread.to_owned(), sub);
    events
}

/// The active turn of the parent thread, which a child turn runs under.
fn active_parent(state: &RuntimeState, parent_native_thread: &str) -> Option<ParentLink> {
    state
        .turns
        .values()
        .filter(|turn| {
            turn.input.provider_thread.native_thread_ref.is_some()
                && native_thread_of(&turn.input.provider_thread).as_deref()
                    == Some(parent_native_thread)
        })
        .max_by_key(|turn| turn.input.provider_turn_ordinal)
        .map(|turn| link_of(state, turn))
}

/// Whether a `turn/started` opens a child turn of a registered subagent. It does
/// unless its parent thread is starting a turn, which the child's frame waits for,
/// as upstream's `pendingSubagentTurns` does. A parent that has finished is not
/// waited for: upstream runs the late child under the parent's last context, and
/// interrupts it if an ancestor ended without completing.
pub fn opens_child_turn(state: &RuntimeState, thread: &str, native_turn: &str) -> bool {
    state.subagents.get(thread).is_some_and(|sub| {
        !sub.native_turns.contains(native_turn)
            && (active_parent(state, &sub.parent_native_thread).is_some()
                || !state.starting.contains(&sub.parent_native_thread)
                || sub
                    .parent
                    .ancestors
                    .iter()
                    .any(|turn| state.terminalized.contains(turn)))
    })
}

/// Upstream `emitSubagentProviderTurnStarted`: registers a child turn under the
/// active parent turn, and emits the child's running turn and node.
pub fn start_child_turn(
    state: &mut RuntimeState,
    thread: &str,
    native_turn: &str,
    started_at: UtcDateTime,
) -> Vec<ProviderAdapterV2Event> {
    let Some(known) = state.subagents.get(thread) else {
        return vec![];
    };
    if known.native_turns.contains(native_turn) {
        return vec![];
    }
    let parent = active_parent(state, &known.parent_native_thread);
    let Some(sub) = state.subagents.get_mut(thread) else {
        return vec![];
    };
    sub.native_turns.insert(native_turn.to_owned());
    if let Some(parent) = parent {
        sub.parent = parent;
    }
    // Upstream: a child turn under a terminalized ancestor is interrupted, not registered.
    if sub
        .parent
        .ancestors
        .iter()
        .any(|turn| state.terminalized.contains(turn))
    {
        state
            .interrupts
            .push((thread.to_owned(), native_turn.to_owned()));
        return vec![];
    }
    sub.turn_count += 1;
    let ordinal = sub.turn_count;
    let node = if ordinal == 1 {
        sub.child_root_node.clone()
    } else {
        derive::node_from_provider_item(&driver(), &format!("{native_turn}:thread-root"))
    };
    let mut input = sub.parent.input.clone();
    input.app_thread = sub.child_app_thread.clone();
    input.thread_id = sub.child_thread.clone();
    input.provider_thread = sub.provider_thread.clone();
    input.root_node_id = node.clone();
    input.provider_turn_ordinal = ordinal;
    let mut turn = TurnState::new(native_turn.to_owned(), input);
    turn.subagent = Some(thread.to_owned());
    turn.started_at = Some(started_at);

    let now = UtcDateTime::now();
    let mut events = vec![ProviderAdapterV2Event::ProviderTurnUpdated {
        driver: driver(),
        thread_id: Some(sub.child_thread.clone()),
        provider_turn: projection::provider_turn(
            &turn,
            OrchestrationV2ProviderTurnStatus::Running,
            None,
            None,
            None,
        ),
    }];
    sub.provider_thread.status = OrchestrationV2ProviderThreadStatus::Active;
    sub.provider_thread.updated_at = now;
    events.push(ProviderAdapterV2Event::ProviderThreadUpdated {
        driver: driver(),
        provider_thread: sub.provider_thread.clone(),
        retained_native_turn_ids: None,
    });
    events.push(ProviderAdapterV2Event::NodeUpdated {
        driver: driver(),
        node: OrchestrationV2ExecutionNode {
            id: node.clone(),
            thread_id: sub.child_thread.clone(),
            run_id: None,
            parent_node_id: None,
            root_node_id: node.clone(),
            kind: OrchestrationV2ExecutionNodeKind::RootTurn,
            status: OrchestrationV2ExecutionNodeStatus::Running,
            counts_for_run: false,
            provider_thread_id: Some(sub.provider_thread.id.clone()),
            provider_turn_id: Some(derive::provider_turn(&driver(), native_turn)),
            native_item_ref: sub.task.native_task_ref.clone(),
            runtime_request_id: None,
            checkpoint_scope_id: None,
            started_at: Some(started_at),
            completed_at: None,
        },
    });
    events.extend(emit_task(
        sub,
        TaskUpdate {
            status: Some(OrchestrationV2SubagentStatus::Running),
            started_at: Some(started_at),
            reopen: ordinal > 1,
            result: (ordinal > 1).then_some(None),
            ..TaskUpdate::default()
        },
    ));
    state
        .usage
        .entry(thread.to_owned())
        .or_default()
        .begin(native_turn);
    state.turns.insert(native_turn.to_owned(), turn);
    events
}

/// Upstream `finalizeCodexTurn` for a child: its node, provider thread, subagent
/// node, and task settle with the turn. A root turn settles through the parent.
pub fn finish_child_turn(
    state: &mut RuntimeState,
    turn: &TurnState,
    status: OrchestrationV2ProviderTurnStatus,
    completed_at: UtcDateTime,
) -> Vec<ProviderAdapterV2Event> {
    let Some(thread) = turn.subagent.as_ref() else {
        return vec![];
    };
    let Some(sub) = state.subagents.get_mut(thread) else {
        return vec![];
    };
    let task_status = settled_status(status);
    let node_status = node_status(task_status);
    let mut events = vec![ProviderAdapterV2Event::NodeUpdated {
        driver: driver(),
        node: OrchestrationV2ExecutionNode {
            id: turn.input.root_node_id.clone(),
            thread_id: sub.child_thread.clone(),
            run_id: None,
            parent_node_id: None,
            root_node_id: turn.input.root_node_id.clone(),
            kind: OrchestrationV2ExecutionNodeKind::RootTurn,
            status: node_status,
            counts_for_run: false,
            provider_thread_id: Some(sub.provider_thread.id.clone()),
            provider_turn_id: Some(derive::provider_turn(&driver(), &turn.native_turn)),
            native_item_ref: sub.task.native_task_ref.clone(),
            runtime_request_id: None,
            checkpoint_scope_id: None,
            started_at: turn.started_at,
            completed_at: Some(completed_at),
        },
    }];
    sub.provider_thread.status = OrchestrationV2ProviderThreadStatus::Idle;
    sub.provider_thread.updated_at = completed_at;
    events.push(ProviderAdapterV2Event::ProviderThreadUpdated {
        driver: driver(),
        provider_thread: sub.provider_thread.clone(),
        retained_native_turn_ids: None,
    });
    events.push(ProviderAdapterV2Event::NodeUpdated {
        driver: driver(),
        node: OrchestrationV2ExecutionNode {
            id: sub.node.clone(),
            thread_id: sub.parent.thread_id.clone(),
            run_id: sub.parent.run_id.clone(),
            parent_node_id: Some(sub.parent.root_node.clone()),
            root_node_id: sub.parent.root_node.clone(),
            kind: OrchestrationV2ExecutionNodeKind::Subagent,
            status: node_status,
            counts_for_run: false,
            provider_thread_id: Some(sub.provider_thread.id.clone()),
            provider_turn_id: Some(sub.parent.provider_turn.clone()),
            native_item_ref: Some(native_ref(&sub.native_tool_call)),
            runtime_request_id: None,
            checkpoint_scope_id: None,
            started_at: Some(sub.started_at),
            completed_at: Some(completed_at),
        },
    });
    events.extend(emit_task(
        sub,
        TaskUpdate {
            status: Some(task_status),
            completed_at: Some(completed_at),
            ..TaskUpdate::default()
        },
    ));
    events
}

/// Upstream's final-answer branch of `item/completed`: a child's final text is
/// the subagent's result.
pub fn set_result(
    state: &mut RuntimeState,
    thread: &str,
    text: &str,
) -> Vec<ProviderAdapterV2Event> {
    state
        .subagents
        .get_mut(thread)
        .map(|sub| {
            emit_task(
                sub,
                TaskUpdate {
                    result: Some(Some(text.to_owned())),
                    ..TaskUpdate::default()
                },
            )
        })
        .unwrap_or_default()
}

/// Upstream `updateSubagentStates`: every `agentsStates` entry of a collab call
/// updates the subagent it names.
pub fn update_states(
    state: &mut RuntimeState,
    agents_states: &Value,
) -> Vec<ProviderAdapterV2Event> {
    let mut events = vec![];
    for (thread, entry) in agents_states.as_object().into_iter().flatten() {
        let Some(sub) = state.subagents.get_mut(thread) else {
            continue;
        };
        let Some(status) = entry
            .get("status")
            .and_then(Value::as_str)
            .and_then(collab_status)
        else {
            continue;
        };
        let result = entry
            .get("message")
            .and_then(Value::as_str)
            .map(|message| Some(message.to_owned()));
        events.extend(emit_task(
            sub,
            TaskUpdate {
                status: Some(status),
                result,
                ..TaskUpdate::default()
            },
        ));
    }
    events
}

/// Upstream `emitSubagentUserMessage`: a child's later user input (a `sendInput`)
/// becomes a user message in the child thread. The first child turn's prompt
/// comes from the spawn instead, so it is skipped.
pub fn child_user_message(
    state: &mut RuntimeState,
    turn: &mut TurnState,
    item: &Value,
    started_at: Option<UtcDateTime>,
) -> Vec<ProviderAdapterV2Event> {
    let Some(thread) = turn.subagent.clone() else {
        return vec![];
    };
    if turn.input.provider_turn_ordinal == 1 {
        return vec![];
    }
    let text = item
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    let text = js_trim(&text);
    if text.is_empty() {
        return vec![];
    }
    let Some(sender) = state
        .subagents
        .get(&thread)
        .map(|sub| sub.parent.thread_id.clone())
    else {
        return vec![];
    };
    let native_item = item.get("id").and_then(Value::as_str).unwrap_or_default();
    let now = UtcDateTime::now();
    let (ordinal, started) = items::resolve_position(turn, native_item, started_at, now);
    let site = MessageSite {
        thread_id: &turn.input.thread_id,
        root_node: &turn.input.root_node_id,
        provider_thread: &turn.input.provider_thread.id,
        provider_turn: Some(derive::provider_turn(&driver(), &turn.native_turn)),
    };
    user_message(&sender, site, native_item, text, ordinal, started)
}

/// Upstream `registerSubagentActivity` for a non-start `subAgentActivity`: the
/// subagent it names takes the activity's terminal status.
pub fn activity_finished(
    state: &mut RuntimeState,
    agent_thread: &str,
    status: OrchestrationV2SubagentStatus,
) -> Vec<ProviderAdapterV2Event> {
    state
        .subagents
        .get_mut(agent_thread)
        .map(|sub| {
            emit_task(
                sub,
                TaskUpdate {
                    status: Some(status),
                    ..TaskUpdate::default()
                },
            )
        })
        .unwrap_or_default()
}

/// Subagents already spawned under a top-level root node: the ordinal of the next
/// `subAgentActivity` under it.
pub fn spawned_under(state: &RuntimeState, root: &NodeId) -> usize {
    state
        .subagents
        .values()
        .filter(|sub| &sub.parent.root_node == root)
        .count()
}
