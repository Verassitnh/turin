//! Port of the plan and todo helpers in upstream ClaudeAdapterV2.ts:
//! `emitClaudePlanProjection`, `claudeTodoSteps`, `applyClaudeTaskTool`,
//! `claudeTaskSteps`, and `claudeProposedPlan`.
//!
//! A projection is one execution node, one `plan.updated`, and one
//! `turn_item.updated` for a native item. TodoWrite projects its own list.
//! Claude 5's TaskCreate, TaskUpdate and TaskList keep one list per native
//! thread, and each change re-projects it under `claude-tasks:<turn>`.
use crate::claude_turn_items::{ItemOrdinals, TurnScope, item_ordinal};
use indexmap::IndexMap;
use serde_json::{Value, json};
use std::collections::HashMap;
use turin_contracts::{
    PlanId, UtcDateTime, js_trim,
    orchestration_v2::{
        OrchestrationV2ExecutionNode, OrchestrationV2PlanArtifact,
        OrchestrationV2PlanArtifactStatus, OrchestrationV2PlanStep, OrchestrationV2PlanStepStatus,
        OrchestrationV2TurnItem,
    },
};
use turin_provider_core::{
    id_allocator::{IdAllocator, derive},
    provider_adapter::ProviderAdapterV2Event,
};

/// One Claude task, in creation order. Upstream `Omit<PlanStep, "id">`.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskEntry {
    pub text: String,
    pub status: OrchestrationV2PlanStepStatus,
}

/// Claude's task id to task. `IndexMap` keeps creation order and replaces
/// in place on `set`, as a JS `Map` does.
pub type TaskList = IndexMap<String, TaskEntry>;

/// What a projection carries.
pub enum PlanBody<'a> {
    TodoList(Vec<OrchestrationV2PlanStep>),
    ProposedPlan(&'a str),
}

/// Plan state the session keeps across turns: plan ids per native item, the
/// latest plan per kind, and the task list per native thread.
#[derive(Default)]
pub struct PlanState {
    plan_ids: HashMap<String, PlanId>,
    latest: HashMap<String, OrchestrationV2PlanArtifact>,
    task_lists: HashMap<String, TaskList>,
}

impl PlanState {
    pub fn task_list(&self, native_thread_id: &str) -> TaskList {
        self.task_lists
            .get(native_thread_id)
            .cloned()
            .unwrap_or_default()
    }
    pub fn set_task_list(&mut self, native_thread_id: &str, tasks: TaskList) {
        self.task_lists.insert(native_thread_id.to_owned(), tasks);
    }
}

fn record_value(input: &Value) -> &Value {
    if input.get("type").and_then(Value::as_str) == Some("record") {
        input.get("value").unwrap_or(&Value::Null)
    } else {
        input
    }
}

fn trimmed_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(js_trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn step(
    id: String,
    text: String,
    status: OrchestrationV2PlanStepStatus,
) -> OrchestrationV2PlanStep {
    serde_json::from_value(json!({"id": id, "text": text, "status": status}))
        .expect("plan step matches the contract shape")
}

/// Upstream `claudeTodoSteps`: TodoWrite's `todos`. Entries without content
/// are skipped, but their index still numbers the ids.
pub fn todo_steps(input: &Value) -> Vec<OrchestrationV2PlanStep> {
    let Some(todos) = record_value(input).get("todos").and_then(Value::as_array) else {
        return vec![];
    };
    todos
        .iter()
        .enumerate()
        .filter_map(|(index, todo)| {
            let text = trimmed_string(todo.get("content"))?;
            let status = match todo.get("status").and_then(Value::as_str) {
                Some("completed") => OrchestrationV2PlanStepStatus::Completed,
                Some("in_progress") => OrchestrationV2PlanStepStatus::Running,
                _ => OrchestrationV2PlanStepStatus::Pending,
            };
            Some(step(format!("todo-{index}"), text, status))
        })
        .collect()
}

fn task_status(value: Option<&Value>) -> Option<OrchestrationV2PlanStepStatus> {
    match value.and_then(Value::as_str)? {
        "pending" => Some(OrchestrationV2PlanStepStatus::Pending),
        "in_progress" => Some(OrchestrationV2PlanStepStatus::Running),
        "completed" => Some(OrchestrationV2PlanStepStatus::Completed),
        _ => None,
    }
}

/// Upstream `claudeTaskSteps`.
pub fn task_steps(tasks: &TaskList) -> Vec<OrchestrationV2PlanStep> {
    tasks
        .iter()
        .map(|(id, task)| step(format!("task-{id}"), task.text.clone(), task.status))
        .collect()
}

/// Upstream `applyClaudeTaskTool`. Applies a finished TaskCreate, TaskUpdate
/// or TaskList call. Ids come from Claude, a `deleted` status removes the
/// task, and TaskList replaces the whole list. Returns `None` when the call is
/// not a task tool, fails, or leaves the list unchanged.
pub fn apply_task_tool(
    tasks: &TaskList,
    tool_name: &str,
    input: &Value,
    result: Option<&Value>,
) -> Option<TaskList> {
    let field = |value: Option<&Value>, key: &str| value.and_then(|v| v.get(key)).cloned();
    if tool_name == "TaskList" {
        let listed = result?.get("tasks")?.as_array()?;
        let mut next = TaskList::new();
        for task in listed {
            let id = trimmed_string(task.get("id"));
            let text = trimmed_string(task.get("subject"));
            let status = task_status(task.get("status"));
            if let (Some(id), Some(text), Some(status)) = (id, text, status) {
                next.insert(id, TaskEntry { text, status });
            }
        }
        return (!next.is_empty() || !tasks.is_empty()).then_some(next);
    }
    if tool_name == "TaskCreate" {
        let created = result?.get("task");
        let id = trimmed_string(created.and_then(|c| c.get("id")))?;
        let text = trimmed_string(created.and_then(|c| c.get("subject")))
            .or_else(|| trimmed_string(input.get("subject")))?;
        let mut next = tasks.clone();
        next.insert(
            id,
            TaskEntry {
                text,
                status: OrchestrationV2PlanStepStatus::Pending,
            },
        );
        return Some(next);
    }
    if tool_name != "TaskUpdate"
        || result.and_then(|r| r.get("success")) == Some(&Value::Bool(false))
    {
        return None;
    }
    let id = trimmed_string(input.get("taskId"))
        .or_else(|| result.and_then(|r| trimmed_string(r.get("taskId"))))?;
    let task = tasks.get(&id)?;
    let native_status = field(Some(input), "status");
    if native_status.as_ref().and_then(Value::as_str) == Some("deleted") {
        let mut next = tasks.clone();
        next.shift_remove(&id);
        return Some(next);
    }
    let text = trimmed_string(input.get("subject")).unwrap_or_else(|| task.text.clone());
    let status = task_status(native_status.as_ref()).unwrap_or(task.status);
    if text == task.text && status == task.status {
        return None;
    }
    let mut next = tasks.clone();
    next.insert(id, TaskEntry { text, status });
    Some(next)
}

/// Upstream `claudeProposedPlan`: the trimmed `plan` from ExitPlanMode input.
pub fn proposed_plan_markdown(input: &Value) -> Option<String> {
    trimmed_string(record_value(input).get("plan"))
}

fn set_status(plan: &mut OrchestrationV2PlanArtifact, next: OrchestrationV2PlanArtifactStatus) {
    match plan {
        OrchestrationV2PlanArtifact::ProposedPlan { status, .. }
        | OrchestrationV2PlanArtifact::TodoList { status, .. } => *status = next,
    }
}

fn plan_id(plan: &OrchestrationV2PlanArtifact) -> &PlanId {
    match plan {
        OrchestrationV2PlanArtifact::ProposedPlan { id, .. }
        | OrchestrationV2PlanArtifact::TodoList { id, .. } => id,
    }
}

fn plan_status(plan: &OrchestrationV2PlanArtifact) -> OrchestrationV2PlanArtifactStatus {
    match plan {
        OrchestrationV2PlanArtifact::ProposedPlan { status, .. }
        | OrchestrationV2PlanArtifact::TodoList { status, .. } => *status,
    }
}

/// Upstream `emitClaudePlanProjection`. The plan id is kept per native item.
/// A new plan supersedes the previous plan of the same kind unless that one
/// is completed. The events are the superseded plan (if any), the node, the
/// plan, and the turn item.
pub fn project_plan(
    state: &mut PlanState,
    scope: &TurnScope,
    provider_turn_ordinal: i64,
    ordinals: &mut ItemOrdinals,
    native_item_id: &str,
    body: PlanBody<'_>,
) -> Vec<ProviderAdapterV2Event> {
    let driver = &scope.driver;
    let kind = match body {
        PlanBody::TodoList(_) => "todo_list",
        PlanBody::ProposedPlan(_) => "proposed_plan",
    };
    let projection_id = format!("{kind}:{native_item_id}");
    let plan_key = format!("{}:{projection_id}", scope.thread_id.as_str());
    let plan_id = state
        .plan_ids
        .entry(plan_key)
        .or_insert_with(|| IdAllocator.plan(&scope.thread_id, Some(&scope.run_id), driver))
        .clone();
    let node_id = derive::node_from_provider_item(driver, &projection_id);
    let item_id = derive::turn_item_from_provider_item(driver, &projection_id);
    let ordinal = item_ordinal(ordinals, provider_turn_ordinal, &projection_id);
    let native_ref = json!({"driver": driver, "nativeId": native_item_id, "strength": "strong"});
    let now = UtcDateTime::now();

    let (plan, item_fields, node_kind): (OrchestrationV2PlanArtifact, Value, &str) = match body {
        PlanBody::TodoList(steps) => {
            let status = if steps
                .iter()
                .all(|s| s.status == OrchestrationV2PlanStepStatus::Completed)
            {
                OrchestrationV2PlanArtifactStatus::Completed
            } else {
                OrchestrationV2PlanArtifactStatus::Active
            };
            let plan = json!({
                "kind": "todo_list", "id": plan_id, "threadId": scope.thread_id,
                "runId": scope.run_id, "nodeId": node_id, "status": status, "steps": steps,
            });
            let item = json!({
                "type": "todo_list", "planId": plan_id, "steps": steps,
            });
            (decode_plan(plan), item, "todo_list")
        }
        PlanBody::ProposedPlan(markdown) => {
            let plan = json!({
                "kind": "proposed_plan", "id": plan_id, "threadId": scope.thread_id,
                "runId": scope.run_id, "nodeId": node_id,
                "status": OrchestrationV2PlanArtifactStatus::Active, "markdown": markdown,
            });
            let item = json!({
                "type": "proposed_plan", "planId": plan_id, "markdown": markdown,
                "streaming": false,
            });
            (decode_plan(plan), item, "plan")
        }
    };

    let node: OrchestrationV2ExecutionNode = serde_json::from_value(json!({
        "id": node_id, "threadId": scope.thread_id, "runId": scope.run_id,
        "parentNodeId": scope.root_node_id, "rootNodeId": scope.root_node_id,
        "kind": node_kind, "status": "completed", "countsForRun": false,
        "providerThreadId": scope.provider_thread_id, "providerTurnId": scope.provider_turn_id,
        "nativeItemRef": native_ref, "runtimeRequestId": null, "checkpointScopeId": null,
        "startedAt": now, "completedAt": now,
    }))
    .expect("plan node matches the contract shape");
    let mut item = json!({
        "id": item_id, "threadId": scope.thread_id, "runId": scope.run_id,
        "nodeId": node_id, "providerThreadId": scope.provider_thread_id,
        "providerTurnId": scope.provider_turn_id, "nativeItemRef": native_ref,
        "parentItemId": null, "ordinal": ordinal, "status": "completed", "title": null,
        "startedAt": now, "completedAt": now, "updatedAt": now,
    });
    if let (Some(map), Value::Object(fields)) = (item.as_object_mut(), item_fields) {
        map.extend(fields);
    }
    let turn_item: OrchestrationV2TurnItem =
        serde_json::from_value(item).expect("plan turn item matches the contract shape");

    let latest_key = format!("{}:{kind}", scope.thread_id.as_str());
    let mut events = vec![];
    if let Some(previous) = state.latest.get(&latest_key).filter(|previous| {
        is_other_plan(previous, &plan)
            && plan_status(previous) != OrchestrationV2PlanArtifactStatus::Completed
    }) {
        let mut superseded = previous.clone();
        set_status(
            &mut superseded,
            OrchestrationV2PlanArtifactStatus::Superseded,
        );
        events.push(ProviderAdapterV2Event::PlanUpdated {
            driver: driver.clone(),
            plan: superseded,
        });
    }
    events.push(ProviderAdapterV2Event::NodeUpdated {
        driver: driver.clone(),
        node,
    });
    events.push(ProviderAdapterV2Event::PlanUpdated {
        driver: driver.clone(),
        plan: plan.clone(),
    });
    events.push(ProviderAdapterV2Event::TurnItemUpdated {
        driver: driver.clone(),
        turn_item,
    });
    state.latest.insert(latest_key, plan);
    events
}

fn decode_plan(value: Value) -> OrchestrationV2PlanArtifact {
    serde_json::from_value(value).expect("plan artifact matches the contract shape")
}

/// Upstream compares plan ids, so a re-projection of the same plan does not
/// supersede itself.
fn is_other_plan(
    previous: &OrchestrationV2PlanArtifact,
    next: &OrchestrationV2PlanArtifact,
) -> bool {
    plan_id(previous) != plan_id(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_steps_skip_empty_entries_and_keep_index_ids() {
        let input = json!({"todos": [
            {"content": "  Read  ", "status": "completed"},
            {"content": "   ", "status": "pending"},
            {"content": "Edit", "status": "in_progress"},
            {"content": "Ship"},
        ]});
        let steps = todo_steps(&input);
        let shape: Vec<_> = steps
            .iter()
            .map(|s| (s.id.as_str(), s.text.as_str(), s.status))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("todo-0", "Read", OrchestrationV2PlanStepStatus::Completed),
                ("todo-2", "Edit", OrchestrationV2PlanStepStatus::Running),
                ("todo-3", "Ship", OrchestrationV2PlanStepStatus::Pending),
            ]
        );
        assert_eq!(
            todo_steps(&json!({"type":"record","value":{"todos":[{"content":"A"}]}})).len(),
            1
        );
    }

    #[test]
    fn task_create_update_delete_follow_upstream_rules() {
        let empty = TaskList::new();
        let created = apply_task_tool(
            &empty,
            "TaskCreate",
            &json!({"subject": "Write tests"}),
            Some(&json!({"task": {"id": "1", "subject": "Write tests"}})),
        )
        .unwrap();
        assert_eq!(created["1"].status, OrchestrationV2PlanStepStatus::Pending);

        let running = apply_task_tool(
            &created,
            "TaskUpdate",
            &json!({"taskId": "1", "status": "in_progress"}),
            Some(&json!({"success": true})),
        )
        .unwrap();
        assert_eq!(running["1"].status, OrchestrationV2PlanStepStatus::Running);

        assert!(
            apply_task_tool(
                &running,
                "TaskUpdate",
                &json!({"taskId": "1", "status": "in_progress"}),
                Some(&json!({"success": true})),
            )
            .is_none(),
            "an unchanged task is not re-projected"
        );
        assert!(
            apply_task_tool(
                &running,
                "TaskUpdate",
                &json!({"taskId": "1", "status": "completed"}),
                Some(&json!({"success": false})),
            )
            .is_none(),
            "a failed update changes nothing"
        );
        let deleted = apply_task_tool(
            &running,
            "TaskUpdate",
            &json!({"taskId": "1", "status": "deleted"}),
            Some(&json!({"success": true})),
        )
        .unwrap();
        assert!(deleted.is_empty());
    }

    #[test]
    fn task_list_replaces_the_list_and_an_empty_list_clears_it() {
        let mut before = TaskList::new();
        before.insert(
            "9".into(),
            TaskEntry {
                text: "Old".into(),
                status: OrchestrationV2PlanStepStatus::Pending,
            },
        );
        let listed = apply_task_tool(
            &before,
            "TaskList",
            &json!({}),
            Some(&json!({"tasks": [{"id": "2", "subject": "New", "status": "completed"}]})),
        )
        .unwrap();
        assert_eq!(listed.keys().collect::<Vec<_>>(), vec!["2"]);
        let cleared = apply_task_tool(&listed, "TaskList", &json!({}), Some(&json!({"tasks": []})));
        assert!(cleared.unwrap().is_empty());
        assert!(
            apply_task_tool(
                &TaskList::new(),
                "TaskList",
                &json!({}),
                Some(&json!({"tasks": []}))
            )
            .is_none()
        );
    }
}
