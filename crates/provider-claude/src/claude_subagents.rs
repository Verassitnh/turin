//! Port of the subagent part of upstream/apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts:
//! `rememberClaudeSubagentLaunch` (~3213), the session subagent registry
//! (`sessionSubagentsByTaskId`, ~3598) and the task_started, task_progress,
//! task_updated and task_notification projection (`updateClaudeSubagentNode`,
//! ~4968).
//!
//! Each lifecycle method returns the events it produces, in upstream's order.
//! A subagent's own text and tool calls go to its child thread (see
//! `claude_subagent_threads.rs`), never to the parent turn.
//!
//! Not ported here: the model canonicalisation through the catalog (a subagent
//! takes its parent's model selection); a resumed subagent's re-open
//! (SendMessage, `reopen`); the workflow coordinator's members.
use crate::{
    claude_subagent_threads::{
        ChildMessage, ChildThread, SubagentIds, SubagentNodeInput, child_tool_call,
        child_turn_scope, conversation_events, normalize_result_text, open_tool_status,
        subagent_ids, subagent_node_events, subagent_turn_item, tool_call_events,
    },
    claude_tool_titles::{ToolPresentation, fallback_presentation, presentations_from_meta},
    claude_workflow_progress::{
        CLAUDE_WORKFLOW_TASK_TYPE, has_claude_workflow_snapshot, merge_claude_workflow_progress,
    },
};
use serde_json::Value;
use std::collections::HashMap;
use turin_contracts::{
    NodeId, ProviderThreadId, ProviderTurnId, RunId, ThreadId, TrimmedNonEmptyString, UtcDateTime,
    js_trim,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2AppThread, OrchestrationV2ConversationMessageRole,
        OrchestrationV2CreationSource, OrchestrationV2NativeRefStrength,
        OrchestrationV2ProviderRef, OrchestrationV2Subagent, OrchestrationV2SubagentOrigin,
        OrchestrationV2SubagentStatus,
    },
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
};
use turin_provider_core::{
    provider_adapter::{
        ProviderAdapterV2Event, ProviderAdapterV2SubagentOutcome,
        ProviderAdapterV2SubagentOutcomeStatus,
    },
    subagent_projection::{
        SubagentChildThreadInput, make_subagent_child_thread, subagent_thread_title,
    },
};

/// The `task_type`s the CLI gives a subagent. A workflow is a coordinator
/// subagent. `local_bash` is shell work, not a subagent, and is ignored here.
const SUBAGENT_TASK_TYPES: [&str; 2] = ["local_agent", CLAUDE_WORKFLOW_TASK_TYPE];

/// Where a new subagent is parented. Set when a turn starts and kept after it,
/// because a background subagent reports its progress after its turn settles.
#[derive(Debug, Clone)]
pub struct SubagentScope {
    pub driver: ProviderDriverKind,
    pub provider_instance_id: ProviderInstanceId,
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub root_node_id: NodeId,
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: ProviderTurnId,
    /// The parent thread, which a child thread copies its shape from.
    pub app_thread: OrchestrationV2AppThread,
    /// The parent's selection. A child thread runs with it until model
    /// canonicalisation lands (T10d).
    pub model_selection: ModelSelection,
}

/// What the launching Agent call said. It waits here until task_started names
/// the task, and is removed then.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Launch {
    description: Option<String>,
    model: Option<String>,
    prompt: Option<String>,
    /// The Agent call that owns this one, for a subagent that starts another.
    owner_tool_use_id: Option<String>,
}

/// One registered subagent: its record, its ids, and where its items go.
struct Task {
    subagent: OrchestrationV2Subagent,
    ids: SubagentIds,
    /// The subagent's turn item ordinal in the parent turn.
    turn_item_ordinal: u64,
    child: ChildThread,
    /// The launching turn's provider ids. Its nodes report against them even
    /// after the turn settles.
    provider_thread_id: ProviderThreadId,
    provider_turn_id: ProviderTurnId,
    root_node_id: NodeId,
    /// Set by a native terminal frame (upstream `confirmedOutcome`). A Stop
    /// is answered from it, and a resume clears it.
    confirmed: Option<ProviderAdapterV2SubagentOutcome>,
}

/// What `stopSubagent` checks before it asks the CLI to stop a task.
pub struct StopState {
    pub thread_id: ThreadId,
    pub started_at: Option<UtcDateTime>,
    pub confirmed: Option<ProviderAdapterV2SubagentOutcome>,
}

#[derive(Default)]
pub struct SubagentRegistry {
    scope: Option<SubagentScope>,
    /// Keyed by the Agent tool_use id.
    launches: HashMap<String, Launch>,
    /// Keyed by the CLI task id.
    tasks: HashMap<String, Task>,
    /// A run's Agent call (its launch, or a later SendMessage resume) to the task it started.
    task_by_run_tool_use: HashMap<String, String>,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn native_ref(task_id: &str) -> OrchestrationV2ProviderRef {
    OrchestrationV2ProviderRef {
        driver: ProviderDriverKind::new("claudeAgent").expect("valid Claude driver"),
        native_id: turin_contracts::TrimmedNonEmptyString::new(task_id).ok(),
        strength: OrchestrationV2NativeRefStrength::Strong,
        fingerprint: None,
        ordinal: None,
    }
}

/// Upstream `claudeTaskOutcome`, plus `killed` and `cancelled` which the CLI
/// also reports for a stopped task.
fn terminal_status(status: &str) -> Option<OrchestrationV2SubagentStatus> {
    match status {
        "completed" => Some(OrchestrationV2SubagentStatus::Completed),
        "failed" => Some(OrchestrationV2SubagentStatus::Failed),
        "stopped" | "killed" | "cancelled" => Some(OrchestrationV2SubagentStatus::Cancelled),
        _ => None,
    }
}

/// The subagent's record and the events describing it: the subagent node and
/// child root when `lifecycle` (a status change), then `subagent.updated` and
/// its turn item.
fn task_events(task: &Task, lifecycle: bool) -> Vec<ProviderAdapterV2Event> {
    let driver = &task.subagent.driver;
    let mut events = Vec::new();
    if lifecycle {
        events.extend(subagent_node_events(SubagentNodeInput {
            driver,
            subagent: &task.subagent,
            ids: &task.ids,
            root_node_id: &task.root_node_id,
            provider_thread_id: &task.provider_thread_id,
            provider_turn_id: &task.provider_turn_id,
        }));
    }
    events.push(ProviderAdapterV2Event::SubagentUpdated {
        driver: driver.clone(),
        subagent: task.subagent.clone(),
    });
    events.push(subagent_turn_item(
        driver,
        &task.subagent,
        &task.ids,
        task.turn_item_ordinal,
        &task.provider_thread_id,
        &task.provider_turn_id,
        &task.subagent.provider_instance_id,
    ));
    events
}

impl SubagentRegistry {
    pub fn set_scope(&mut self, scope: SubagentScope) {
        self.scope = Some(scope);
    }

    /// Upstream `rememberClaudeSubagentLaunch`. An Agent call is never a tool
    /// call; its input is kept until task_started names the task.
    pub fn remember_launch(
        &mut self,
        tool_use_id: &str,
        input: &Value,
        owner_tool_use_id: Option<&str>,
    ) {
        if self.task_by_run_tool_use.contains_key(tool_use_id) {
            return;
        }
        // `inherit` is the caller's model. Turin does not resolve it yet, so it
        // is left unset, as an absent model would be.
        let model = text(input, "model").filter(|model| model != "inherit");
        self.launches.insert(
            tool_use_id.to_owned(),
            Launch {
                description: text(input, "description"),
                model,
                prompt: text(input, "prompt"),
                owner_tool_use_id: owner_tool_use_id.map(str::to_owned),
            },
        );
    }

    /// task_started registers the subagent, creates its child thread, and opens
    /// its run. A task_started for a finished task is a resume (SendMessage):
    /// it reopens the task under the resuming tool call, and the message is the
    /// child's next prompt. `turn_item_ordinal` numbers the turn item in the
    /// parent turn (upstream `resolveItemOrdinal`). Empty for shell tasks, a
    /// duplicate start of a running task, and when no scope is set yet.
    pub fn started(
        &mut self,
        raw: &Value,
        now: UtcDateTime,
        turn_item_ordinal: &mut dyn FnMut(&str) -> u64,
    ) -> Vec<ProviderAdapterV2Event> {
        let Some(task_type) = raw.get("task_type").and_then(Value::as_str) else {
            return Vec::new();
        };
        if !SUBAGENT_TASK_TYPES.contains(&task_type) {
            return Vec::new();
        }
        let Some(scope) = self.scope.clone() else {
            return Vec::new();
        };
        let Some(task_id) = text(raw, "task_id") else {
            return Vec::new();
        };
        if self
            .tasks
            .get(&task_id)
            .is_some_and(|task| task.subagent.status == OrchestrationV2SubagentStatus::Running)
        {
            return Vec::new();
        }
        let run_tool_use_id = text(raw, "tool_use_id");
        let launch = run_tool_use_id
            .as_deref()
            .and_then(|id| self.launches.remove(id))
            .unwrap_or_default();
        // A subagent started by another subagent is parented to that owner's node.
        let parent_node_id = launch
            .owner_tool_use_id
            .as_deref()
            .and_then(|owner| self.task_by_run_tool_use.get(owner))
            .and_then(|owner_task| self.tasks.get(owner_task))
            .map(|owner| owner.ids.node_id.clone())
            .unwrap_or_else(|| scope.root_node_id.clone());
        // Workflow telemetry is cumulative. The coordinator's first frame seeds it.
        let workflow = (task_type == CLAUDE_WORKFLOW_TASK_TYPE)
            .then(|| {
                merge_claude_workflow_progress(
                    None,
                    text(raw, "workflow_name").as_deref(),
                    None,
                    raw,
                )
            })
            .flatten();
        let prompt = launch.prompt.clone().or_else(|| text(raw, "prompt"));
        let title = launch
            .description
            .clone()
            .or_else(|| text(raw, "description"));
        let previous = self.tasks.remove(&task_id);
        let is_new = previous.is_none();
        let (subagent, ids, turn_item_ordinal, child) = match previous {
            Some(previous) => {
                // A resume: the record reopens. Its previous answer and progress no
                // longer describe the run, and its run is the resuming one.
                let mut subagent = previous.subagent;
                subagent.run_id = Some(scope.run_id.clone());
                subagent.status = OrchestrationV2SubagentStatus::Running;
                subagent.progress = None;
                subagent.result = None;
                subagent.started_at = Some(now);
                subagent.completed_at = None;
                subagent.updated_at = now;
                if let Some(prompt) = prompt {
                    subagent.prompt = prompt;
                }
                if let Some(title) = title {
                    subagent.title = Some(title);
                }
                let mut child = previous.child;
                child.last_assistant_text = None;
                (subagent, previous.ids, previous.turn_item_ordinal, child)
            }
            None => {
                let ids = subagent_ids(&scope.driver, &scope.provider_thread_id, &task_id);
                let child =
                    ChildThread::new(ids.child_thread_id.clone(), ids.child_root_node_id.clone());
                let ordinal = turn_item_ordinal(&format!("task:{task_id}:subagent"));
                let subagent = OrchestrationV2Subagent {
                    id: ids.node_id.clone(),
                    thread_id: scope.thread_id.clone(),
                    run_id: Some(scope.run_id.clone()),
                    parent_node_id,
                    origin: OrchestrationV2SubagentOrigin::ProviderNative,
                    created_by: OrchestrationV2Actor::Agent,
                    driver: scope.driver.clone(),
                    provider_instance_id: scope.provider_instance_id.clone(),
                    provider_thread_id: None,
                    child_thread_id: Some(ids.child_thread_id.clone()),
                    native_task_ref: Some(native_ref(&task_id)),
                    prompt: prompt.unwrap_or_default(),
                    title,
                    model: launch.model,
                    model_selection: None,
                    completion_wake: None,
                    completion_delivery: None,
                    status: OrchestrationV2SubagentStatus::Running,
                    progress: None,
                    workflow,
                    result: None,
                    started_at: Some(now),
                    completed_at: None,
                    updated_at: now,
                };
                (subagent, ids, ordinal, child)
            }
        };
        if let Some(run_tool_use_id) = run_tool_use_id.clone() {
            self.task_by_run_tool_use
                .insert(run_tool_use_id, task_id.clone());
        }
        let mut task = Task {
            subagent,
            ids,
            turn_item_ordinal,
            child,
            provider_thread_id: scope.provider_thread_id.clone(),
            provider_turn_id: scope.provider_turn_id.clone(),
            root_node_id: scope.root_node_id.clone(),
            confirmed: None,
        };
        let mut events = Vec::new();
        if is_new {
            // Upstream titles the child with the subagent count, this one included.
            let ordinal = self.tasks.len() as u64 + 1;
            let title = match task.subagent.workflow.as_ref() {
                Some(workflow) => Some(format!(
                    "Workflow · {}",
                    workflow
                        .name
                        .as_ref()
                        .map(|name| name.as_str())
                        .or(task.subagent.title.as_deref())
                        .unwrap_or("Untitled")
                )),
                None => task.subagent.title.clone(),
            };
            let child_title = subagent_thread_title(
                scope.app_thread.title.as_str(),
                title.as_deref(),
                &task.subagent.prompt,
                ordinal,
            );
            let child_thread = make_subagent_child_thread(SubagentChildThreadInput {
                parent_thread: &scope.app_thread,
                child_thread_id: task.ids.child_thread_id.clone(),
                parent_node_id: task.ids.node_id.clone(),
                active_provider_thread_id: None,
                provider_instance_id: scope.provider_instance_id.clone(),
                model_selection: scope.model_selection.clone(),
                title: TrimmedNonEmptyString::new(child_title).unwrap_or_else(|_| {
                    TrimmedNonEmptyString::new("Subagent").expect("non-empty title")
                }),
                now,
                created_by: OrchestrationV2Actor::Agent,
                creation_source: OrchestrationV2CreationSource::Provider,
            });
            events.push(ProviderAdapterV2Event::AppThreadCreated {
                driver: scope.driver.clone(),
                app_thread: child_thread,
            });
        }
        events.extend(task_events(&task, true));
        // The launch prompt opens the child thread. A resume's message opens the
        // next run, under the resuming tool call.
        let prompt_native = match (is_new, run_tool_use_id) {
            (true, _) => Some(format!("task:{task_id}:prompt")),
            (false, Some(tool_use_id)) if text(raw, "prompt").is_some() => {
                Some(format!("task:{task_id}:prompt:{tool_use_id}"))
            }
            (false, _) => None,
        };
        if let Some(native) = prompt_native {
            let prompt = task.subagent.prompt.clone();
            let ordinal = task.child.ordinal(&native);
            events.extend(conversation_events(
                &scope.driver,
                &task.child,
                ChildMessage {
                    native_item_id: &native,
                    sender: Some(&scope.thread_id),
                    role: OrchestrationV2ConversationMessageRole::User,
                    text: &prompt,
                    ordinal,
                },
                now,
            ));
        }
        self.tasks.insert(task_id, task);
        events
    }

    /// What `stopSubagent` needs to know about a task. `None` for an unknown task.
    pub fn stop_state(&self, task_id: &str) -> Option<StopState> {
        let task = self.tasks.get(task_id)?;
        Some(StopState {
            thread_id: task.subagent.thread_id.clone(),
            started_at: task.subagent.started_at,
            confirmed: task.confirmed.clone(),
        })
    }

    /// The outcome a native terminal frame confirmed for the task, if any.
    pub fn confirmed_outcome(&self, task_id: &str) -> Option<ProviderAdapterV2SubagentOutcome> {
        self.tasks.get(task_id)?.confirmed.clone()
    }

    /// task_progress updates a running subagent's progress line, and a
    /// workflow coordinator's phase and member snapshot.
    pub fn progress(&mut self, raw: &Value, now: UtcDateTime) -> Vec<ProviderAdapterV2Event> {
        let Some(task_id) = text(raw, "task_id") else {
            return Vec::new();
        };
        let Some(task) = self.tasks.get_mut(&task_id) else {
            return Vec::new();
        };
        if task.subagent.status != OrchestrationV2SubagentStatus::Running {
            return Vec::new();
        }
        let mut changed = false;
        if let Some(progress) = text(raw, "summary").or_else(|| text(raw, "description")) {
            task.subagent.progress = Some(progress);
            changed = true;
        }
        if task.subagent.workflow.is_some()
            && has_claude_workflow_snapshot(raw)
            && let Some(workflow) =
                merge_claude_workflow_progress(task.subagent.workflow.as_ref(), None, None, raw)
        {
            task.subagent.workflow = Some(workflow);
            changed = true;
        }
        if !changed {
            return Vec::new();
        }
        task.subagent.updated_at = now;
        task_events(task, false)
    }

    /// The Workflow tool's launch acknowledgement carries the run's handles.
    /// It is the only carrier of them, so they land on the coordinator it started.
    pub fn run_handles(
        &mut self,
        tool_use_id: &str,
        handles: &Value,
        now: UtcDateTime,
    ) -> Vec<ProviderAdapterV2Event> {
        let Some(task_id) = self.task_by_run_tool_use.get(tool_use_id).cloned() else {
            return Vec::new();
        };
        let Some(task) = self.tasks.get_mut(&task_id) else {
            return Vec::new();
        };
        if task.subagent.status != OrchestrationV2SubagentStatus::Running {
            return Vec::new();
        }
        let Some(workflow) = merge_claude_workflow_progress(
            task.subagent.workflow.as_ref(),
            None,
            Some(handles),
            &Value::Null,
        ) else {
            return Vec::new();
        };
        task.subagent.workflow = Some(workflow);
        task.subagent.updated_at = now;
        task_events(task, false)
    }

    /// task_updated carries a status patch. Only its terminal statuses matter here.
    pub fn updated(&mut self, raw: &Value, now: UtcDateTime) -> Vec<ProviderAdapterV2Event> {
        let Some(status) = raw.pointer("/patch/status").and_then(Value::as_str) else {
            return Vec::new();
        };
        let Some(task_id) = text(raw, "task_id") else {
            return Vec::new();
        };
        self.settle(&task_id, status, None, now)
    }

    /// task_notification ends the task and carries its result text.
    pub fn notified(&mut self, raw: &Value, now: UtcDateTime) -> Vec<ProviderAdapterV2Event> {
        let (Some(status), Some(task_id)) = (text(raw, "status"), text(raw, "task_id")) else {
            return Vec::new();
        };
        self.settle(&task_id, &status, text(raw, "summary"), now)
    }

    /// A subagent's own assistant frame: its text becomes a message in the
    /// child thread, and its tool calls become tool items there. Nested Agent
    /// calls are not tool calls; `remember_launch` already took them.
    pub fn child_assistant(
        &mut self,
        owner_tool_use_id: &str,
        raw: &Value,
        now: UtcDateTime,
    ) -> Vec<ProviderAdapterV2Event> {
        let Some(task_id) = self.task_by_run_tool_use.get(owner_tool_use_id).cloned() else {
            return Vec::new();
        };
        let Some(task) = self.tasks.get_mut(&task_id) else {
            return Vec::new();
        };
        let driver = task.subagent.driver.clone();
        let message_id = raw
            .pointer("/message/id")
            .and_then(Value::as_str)
            .or_else(|| raw.get("uuid").and_then(Value::as_str))
            .unwrap_or("assistant")
            .to_owned();
        let blocks: Vec<Value> = raw
            .pointer("/message/content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let tool_names: HashMap<String, String> = blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
            .filter_map(|b| {
                let id = b.get("id")?.as_str()?;
                let name = b.get("name")?.as_str()?;
                Some((id.to_owned(), name.to_owned()))
            })
            .collect();
        let presentations = presentations_from_meta(raw, &tool_names);
        let scope = child_turn_scope(
            &driver,
            &task.child,
            &task.provider_thread_id,
            &task.provider_turn_id,
        );
        let mut events = Vec::new();
        for (index, block) in blocks.iter().enumerate() {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let text = block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if text.is_empty() {
                        continue;
                    }
                    let native = format!("{message_id}:text:{index}");
                    let ordinal = task.child.ordinal(&native);
                    task.child.last_assistant_text = Some(text.to_owned());
                    events.extend(conversation_events(
                        &driver,
                        &task.child,
                        ChildMessage {
                            native_item_id: &native,
                            sender: None,
                            role: OrchestrationV2ConversationMessageRole::Assistant,
                            text,
                            ordinal,
                        },
                        now,
                    ));
                }
                Some("tool_use") => {
                    let Some(native) = block.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_owned();
                    if name == "Agent" {
                        continue;
                    }
                    let presentation: ToolPresentation = presentations
                        .get(native)
                        .cloned()
                        .unwrap_or_else(|| fallback_presentation(&name));
                    let tool = crate::claude_subagent_threads::ChildTool {
                        name,
                        input: block.get("input").cloned().unwrap_or(Value::Null),
                        presentation,
                        ordinal: task.child.ordinal(native),
                        started_at: now,
                        finished: false,
                    };
                    let call = child_tool_call(native, &tool, None, None, "running", now);
                    events.extend(tool_call_events(&scope, &call));
                    task.child.tools.insert(native.to_owned(), tool);
                }
                _ => {}
            }
        }
        events
    }

    /// A subagent's tool_result finishes its child tool call.
    pub fn child_user(
        &mut self,
        owner_tool_use_id: &str,
        raw: &Value,
        now: UtcDateTime,
    ) -> Vec<ProviderAdapterV2Event> {
        let Some(task_id) = self.task_by_run_tool_use.get(owner_tool_use_id).cloned() else {
            return Vec::new();
        };
        let Some(task) = self.tasks.get_mut(&task_id) else {
            return Vec::new();
        };
        let blocks = raw
            .pointer("/message/content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let results: Vec<&Value> = blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .collect();
        // Upstream prefers the structured `tool_use_result` when the message
        // carries exactly one tool result.
        let structured = (results.len() == 1)
            .then(|| raw.get("tool_use_result"))
            .flatten();
        let scope = child_turn_scope(
            &task.subagent.driver,
            &task.child,
            &task.provider_thread_id,
            &task.provider_turn_id,
        );
        let mut events = Vec::new();
        for block in results {
            let Some(native) = block.get("tool_use_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(tool) = task.child.tools.get_mut(native) else {
                continue;
            };
            if tool.finished {
                continue;
            }
            tool.finished = true;
            let failed = block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let content = block.get("content").cloned().unwrap_or(Value::Null);
            let output = structured.cloned().unwrap_or_else(|| content.clone());
            let call = child_tool_call(
                native,
                tool,
                Some(&output),
                structured.map(|_| &content),
                if failed { "failed" } else { "completed" },
                now,
            );
            events.extend(tool_call_events(&scope, &call));
        }
        events
    }

    /// Status is monotone: a terminal subagent keeps its status. A result
    /// that arrives after the status (task_updated comes before
    /// task_notification) is still recorded, as its own child message.
    fn settle(
        &mut self,
        task_id: &str,
        status: &str,
        result: Option<String>,
        now: UtcDateTime,
    ) -> Vec<ProviderAdapterV2Event> {
        let next = terminal_status(status);
        let Some(task) = self.tasks.get_mut(task_id) else {
            return Vec::new();
        };
        let mut lifecycle = false;
        let mut changed = false;
        if let Some(next) = next
            && task.subagent.status == OrchestrationV2SubagentStatus::Running
        {
            task.subagent.status = next;
            task.subagent.completed_at = Some(now);
            lifecycle = true;
            changed = true;
        }
        let new_result = match (&task.subagent.result, result) {
            (None, Some(result)) => {
                task.subagent.result = Some(result.clone());
                changed = true;
                Some(result)
            }
            _ => None,
        };
        if !changed {
            return Vec::new();
        }
        task.subagent.updated_at = now;
        if let Some(outcome) = confirmed_outcome(&task.subagent) {
            task.confirmed = Some(outcome);
        }
        let mut events = Vec::new();
        if lifecycle {
            // A call the subagent left open never returns once it ends.
            events.extend(finish_open_tools(task, now));
        }
        events.extend(task_events(task, lifecycle));
        if let Some(result) = new_result.filter(|result| {
            task.subagent.status != OrchestrationV2SubagentStatus::Running
                && !js_trim(result).is_empty()
        }) {
            // A completed subagent's result is normally its final text, already
            // in the child thread. Failures and cancellations always get it.
            let shown = task.subagent.status == OrchestrationV2SubagentStatus::Completed
                && task
                    .child
                    .last_assistant_text
                    .as_deref()
                    .map(normalize_result_text)
                    == Some(normalize_result_text(&result));
            if !shown {
                let ordinal = task.child.ordinal(&format!("task:{task_id}:result"));
                events.extend(conversation_events(
                    &task.subagent.driver,
                    &task.child,
                    ChildMessage {
                        native_item_id: &format!("task:{task_id}:result"),
                        sender: None,
                        role: OrchestrationV2ConversationMessageRole::Assistant,
                        text: &result,
                        ordinal,
                    },
                    now,
                ));
            }
        }
        events
    }
}

/// Upstream `confirmedSubagentOutcome`: a finished subagent with its end time.
fn confirmed_outcome(
    subagent: &OrchestrationV2Subagent,
) -> Option<ProviderAdapterV2SubagentOutcome> {
    let status = match subagent.status {
        OrchestrationV2SubagentStatus::Completed => {
            ProviderAdapterV2SubagentOutcomeStatus::Completed
        }
        OrchestrationV2SubagentStatus::Failed => ProviderAdapterV2SubagentOutcomeStatus::Failed,
        OrchestrationV2SubagentStatus::Cancelled => {
            ProviderAdapterV2SubagentOutcomeStatus::Cancelled
        }
        _ => return None,
    };
    Some(ProviderAdapterV2SubagentOutcome {
        status,
        completed_at: subagent.completed_at?,
        result: subagent.result.clone(),
    })
}

/// Ends the child tool calls still open when their subagent ends.
fn finish_open_tools(task: &mut Task, now: UtcDateTime) -> Vec<ProviderAdapterV2Event> {
    let status = open_tool_status(task.subagent.status);
    let scope = child_turn_scope(
        &task.subagent.driver,
        &task.child,
        &task.provider_thread_id,
        &task.provider_turn_id,
    );
    let mut open: Vec<(String, u64)> = task
        .child
        .tools
        .iter()
        .filter(|(_, tool)| !tool.finished)
        .map(|(id, tool)| (id.clone(), tool.ordinal))
        .collect();
    open.sort_by_key(|(_, ordinal)| *ordinal);
    let mut events = Vec::new();
    for (native, _) in open {
        let Some(tool) = task.child.tools.get_mut(&native) else {
            continue;
        };
        tool.finished = true;
        let call = child_tool_call(&native, tool, None, None, status, now);
        events.extend(tool_call_events(&scope, &call));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turin_provider_core::id_allocator::derive;

    fn scope() -> SubagentScope {
        SubagentScope {
            driver: ProviderDriverKind::new("claudeAgent").unwrap(),
            provider_instance_id: serde_json::from_value(json!("claudeAgent")).unwrap(),
            thread_id: serde_json::from_value(json!("thread-1")).unwrap(),
            run_id: serde_json::from_value(json!("run-1")).unwrap(),
            root_node_id: serde_json::from_value(json!("node-root")).unwrap(),
            provider_thread_id: serde_json::from_value(json!("provider-thread-1")).unwrap(),
            provider_turn_id: serde_json::from_value(json!("provider-turn-1")).unwrap(),
            app_thread: serde_json::from_value(json!({
                "createdBy": "user", "creationSource": "web", "id": "thread-1",
                "projectId": "project-1", "title": "Parent",
                "providerInstanceId": "claudeAgent",
                "modelSelection": {"instanceId": "claudeAgent", "model": "claude-haiku-4-5"},
                "runtimeMode": "full-access", "interactionMode": "default",
                "branch": null, "worktreePath": null, "activeProviderThreadId": null,
                "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": "thread-1"},
                "forkedFrom": null, "createdAt": "2026-01-01T00:00:00.000Z",
                "updatedAt": "2026-01-01T00:00:00.000Z", "archivedAt": null, "deletedAt": null,
            }))
            .unwrap(),
            model_selection: serde_json::from_value(json!({
                "instanceId": "claudeAgent", "model": "claude-haiku-4-5",
            }))
            .unwrap(),
        }
    }

    fn no_ordinal(_: &str) -> u64 {
        7
    }

    /// The latest subagent record in `events`, if any.
    fn subagent_of(events: &[ProviderAdapterV2Event]) -> Option<OrchestrationV2Subagent> {
        events.iter().rev().find_map(|event| match event {
            ProviderAdapterV2Event::SubagentUpdated { subagent, .. } => Some(subagent.clone()),
            _ => None,
        })
    }

    fn started_task(registry: &mut SubagentRegistry, raw: Value) -> Vec<ProviderAdapterV2Event> {
        registry.started(&raw, UtcDateTime::now(), &mut no_ordinal)
    }

    #[test]
    fn launch_then_started_parents_to_the_root_and_completes_once() {
        let mut registry = SubagentRegistry::default();
        registry.set_scope(scope());
        let now = UtcDateTime::now();
        registry.remember_launch(
            "toolu_a",
            &json!({"description": "Agent A", "prompt": "Reply A", "model": "haiku"}),
            None,
        );
        let events = started_task(
            &mut registry,
            json!({"subtype": "task_started", "task_id": "task_a", "tool_use_id": "toolu_a", "task_type": "local_agent"}),
        );
        let started = subagent_of(&events).expect("subagent starts");
        assert_eq!(started.status, OrchestrationV2SubagentStatus::Running);
        assert_eq!(started.prompt, "Reply A");
        assert_eq!(started.title.as_deref(), Some("Agent A"));
        assert_eq!(started.model.as_deref(), Some("haiku"));
        assert_eq!(started.parent_node_id, scope().root_node_id);
        assert!(started.child_thread_id.is_some());

        let done = registry.notified(
            &json!({"task_id": "task_a", "status": "completed", "summary": "A_FIRST"}),
            now,
        );
        let done = subagent_of(&done).expect("notification ends it");
        assert_eq!(done.status, OrchestrationV2SubagentStatus::Completed);
        assert_eq!(done.result.as_deref(), Some("A_FIRST"));
        // A later duplicate does not change a terminal subagent.
        assert!(
            registry
                .notified(&json!({"task_id": "task_a", "status": "failed"}), now)
                .is_empty()
        );
    }

    #[test]
    fn task_updated_before_notification_keeps_the_result() {
        let mut registry = SubagentRegistry::default();
        registry.set_scope(scope());
        let now = UtcDateTime::now();
        started_task(
            &mut registry,
            json!({"task_id": "task_b", "task_type": "local_agent"}),
        );
        let updated = registry.updated(
            &json!({"task_id": "task_b", "patch": {"status": "completed"}}),
            now,
        );
        let updated = subagent_of(&updated).expect("status lands");
        assert_eq!(updated.status, OrchestrationV2SubagentStatus::Completed);
        let noted = registry.notified(
            &json!({"task_id": "task_b", "status": "completed", "summary": "B_DONE"}),
            now,
        );
        let noted = subagent_of(&noted).expect("result lands after the status");
        assert_eq!(noted.result.as_deref(), Some("B_DONE"));
    }

    #[test]
    fn workflow_coordinator_takes_its_name_and_run_handles() {
        let mut registry = SubagentRegistry::default();
        registry.set_scope(scope());
        let now = UtcDateTime::now();
        let events = started_task(
            &mut registry,
            json!({"task_id": "task_w", "tool_use_id": "toolu_wf", "task_type": "local_workflow", "workflow_name": "review"}),
        );
        let started = subagent_of(&events).expect("workflow coordinator starts");
        assert_eq!(
            started
                .workflow
                .as_ref()
                .and_then(|w| w.name.as_ref())
                .map(|n| n.as_str()),
            Some("review")
        );
        let handled = registry.run_handles(
            "toolu_wf",
            &json!({"runId": "run-7", "transcriptDir": "/tmp/t", "scriptPath": "/tmp/s.js"}),
            now,
        );
        let handled = subagent_of(&handled).expect("launch ack lands on the coordinator");
        let handles = handled.workflow.unwrap().run_handles.unwrap();
        assert_eq!(handles.run_id.as_ref().map(|v| v.as_str()), Some("run-7"));
        assert_eq!(
            handles.script_path.as_ref().map(|v| v.as_str()),
            Some("/tmp/s.js")
        );
    }

    #[test]
    fn shell_tasks_are_not_subagents() {
        let mut registry = SubagentRegistry::default();
        registry.set_scope(scope());
        assert!(
            started_task(
                &mut registry,
                json!({"task_id": "bash_1", "task_type": "local_bash"}),
            )
            .is_empty()
        );
    }

    #[test]
    fn nested_launch_is_parented_to_its_owner_subagent() {
        let mut registry = SubagentRegistry::default();
        registry.set_scope(scope());
        registry.remember_launch("toolu_outer", &json!({"description": "outer"}), None);
        started_task(
            &mut registry,
            json!({"task_id": "task_outer", "tool_use_id": "toolu_outer", "task_type": "local_agent"}),
        );
        registry.remember_launch(
            "toolu_inner",
            &json!({"description": "inner"}),
            Some("toolu_outer"),
        );
        let events = started_task(
            &mut registry,
            json!({"task_id": "task_inner", "tool_use_id": "toolu_inner", "task_type": "local_agent"}),
        );
        let inner = subagent_of(&events).unwrap();
        assert_eq!(
            inner.parent_node_id,
            derive::node_from_provider_item(&scope().driver, "task:task_outer")
        );
    }

    #[test]
    fn start_creates_a_child_thread_with_its_prompt() {
        let mut registry = SubagentRegistry::default();
        registry.set_scope(scope());
        registry.remember_launch("toolu_a", &json!({"prompt": "Read it"}), None);
        let events = started_task(
            &mut registry,
            json!({"task_id": "task_a", "tool_use_id": "toolu_a", "task_type": "local_agent"}),
        );
        let child_id = subagent_of(&events).unwrap().child_thread_id.unwrap();
        let created = events.iter().find_map(|event| match event {
            ProviderAdapterV2Event::AppThreadCreated { app_thread, .. } => Some(app_thread),
            _ => None,
        });
        let created = created.expect("child thread is created on start");
        assert_eq!(created.id, child_id);
        assert_eq!(
            created
                .lineage
                .parent_thread_id
                .as_ref()
                .map(|id| id.as_str()),
            Some("thread-1")
        );
        let prompt = events.iter().find_map(|event| match event {
            ProviderAdapterV2Event::MessageUpdated { message, .. } => Some(message),
            _ => None,
        });
        let prompt = prompt.expect("the launch prompt is a child message");
        assert_eq!(prompt.thread_id, child_id);
        assert_eq!(prompt.text, "Read it");
    }

    #[test]
    fn child_frames_stay_in_the_child_thread_and_finish_with_it() {
        let mut registry = SubagentRegistry::default();
        registry.set_scope(scope());
        let now = UtcDateTime::now();
        registry.remember_launch("toolu_a", &json!({"prompt": "Run"}), None);
        let started = started_task(
            &mut registry,
            json!({"task_id": "task_a", "tool_use_id": "toolu_a", "task_type": "local_agent"}),
        );
        let child_id = subagent_of(&started).unwrap().child_thread_id.unwrap();
        let frame = json!({
            "message": {"id": "msg_c", "content": [
                {"type": "text", "text": "narrating"},
                {"type": "tool_use", "id": "toolu_bash", "name": "Bash", "input": {"command": "ls"}},
            ]},
            "uuid": "u1", "parent_tool_use_id": "toolu_a",
        });
        let events = registry.child_assistant("toolu_a", &frame, now);
        let messages: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                ProviderAdapterV2Event::MessageUpdated { message, .. } => Some(message),
                _ => None,
            })
            .collect();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].thread_id, child_id);
        assert_eq!(messages[0].text, "narrating");
        let tool_items: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                ProviderAdapterV2Event::TurnItemUpdated { turn_item, .. } => {
                    serde_json::to_value(turn_item).ok()
                }
                _ => None,
            })
            .collect();
        assert!(
            tool_items
                .iter()
                .any(|item| item["threadId"] == json!(child_id) && item["status"] == "running")
        );

        // The subagent ends with its tool call still open: the call is closed with it.
        let ended = registry.notified(
            &json!({"task_id": "task_a", "status": "stopped", "summary": "stopped early"}),
            now,
        );
        let closed: Vec<_> = ended
            .iter()
            .filter_map(|event| match event {
                ProviderAdapterV2Event::TurnItemUpdated { turn_item, .. } => {
                    serde_json::to_value(turn_item).ok()
                }
                _ => None,
            })
            .collect();
        assert!(
            closed
                .iter()
                .any(|item| item["threadId"] == json!(child_id) && item["status"] == "interrupted"),
            "{closed:?}"
        );
        // The result is a child message, since the last text differs from it.
        assert!(ended.iter().any(|event| matches!(
            event,
            ProviderAdapterV2Event::MessageUpdated { message, .. }
                if message.text == "stopped early" && message.thread_id == child_id
        )));
    }

    #[test]
    fn completed_result_is_not_repeated_when_it_was_the_last_text() {
        let mut registry = SubagentRegistry::default();
        registry.set_scope(scope());
        let now = UtcDateTime::now();
        registry.remember_launch("toolu_a", &json!({"prompt": "Run"}), None);
        started_task(
            &mut registry,
            json!({"task_id": "task_a", "tool_use_id": "toolu_a", "task_type": "local_agent"}),
        );
        registry.child_assistant(
            "toolu_a",
            &json!({"message": {"id": "m", "content": [{"type": "text", "text": "A  done"}]}}),
            now,
        );
        let ended = registry.notified(
            &json!({"task_id": "task_a", "status": "completed", "summary": "A done"}),
            now,
        );
        assert!(
            !ended
                .iter()
                .any(|event| matches!(event, ProviderAdapterV2Event::MessageUpdated { message, .. } if message.text == "A done")),
            "a result equal to the last text is already shown"
        );
    }

    #[test]
    fn send_message_resumes_a_finished_subagent_under_its_own_call() {
        let mut registry = SubagentRegistry::default();
        registry.set_scope(scope());
        let now = UtcDateTime::now();
        registry.remember_launch("toolu_a", &json!({"prompt": "First"}), None);
        let started = started_task(
            &mut registry,
            json!({"task_id": "task_a", "tool_use_id": "toolu_a", "task_type": "local_agent"}),
        );
        let child_id = subagent_of(&started).unwrap().child_thread_id.unwrap();
        registry.notified(
            &json!({"task_id": "task_a", "status": "completed", "summary": "FIRST_DONE"}),
            now,
        );

        // SendMessage re-emits task_started for the same task under its own call.
        let resumed = started_task(
            &mut registry,
            json!({"task_id": "task_a", "tool_use_id": "toolu_send", "task_type": "local_agent", "prompt": "Second"}),
        );
        let reopened = subagent_of(&resumed).expect("the resume reopens the task");
        assert_eq!(reopened.status, OrchestrationV2SubagentStatus::Running);
        assert_eq!(reopened.prompt, "Second");
        assert!(reopened.result.is_none());
        assert_eq!(reopened.child_thread_id.as_ref(), Some(&child_id));
        assert!(
            !resumed
                .iter()
                .any(|event| matches!(event, ProviderAdapterV2Event::AppThreadCreated { .. })),
            "a resume reuses the child thread"
        );
        assert!(resumed.iter().any(|event| matches!(
            event,
            ProviderAdapterV2Event::MessageUpdated { message, .. }
                if message.text == "Second" && message.thread_id == child_id
        )));

        // The resumed run's text goes to the same child thread.
        let text = registry.child_assistant(
            "toolu_send",
            &json!({"message": {"id": "m2", "content": [{"type": "text", "text": "SECOND_TEXT"}]}}),
            now,
        );
        assert!(text.iter().any(|event| matches!(
            event,
            ProviderAdapterV2Event::MessageUpdated { message, .. }
                if message.text == "SECOND_TEXT" && message.thread_id == child_id
        )));

        let done = registry.notified(
            &json!({"task_id": "task_a", "status": "completed", "summary": "SECOND_DONE"}),
            now,
        );
        assert_eq!(
            subagent_of(&done).unwrap().result.as_deref(),
            Some("SECOND_DONE")
        );
        assert_eq!(
            registry
                .confirmed_outcome("task_a")
                .and_then(|outcome| outcome.result),
            Some("SECOND_DONE".to_owned())
        );
    }
}
