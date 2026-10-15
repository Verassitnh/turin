//! Port of the subagent part of upstream/apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts:
//! `rememberClaudeSubagentLaunch` (~3213), the session subagent registry
//! (`sessionSubagentsByTaskId`, ~3598) and the task_started, task_progress,
//! task_updated and task_notification projection (`updateClaudeSubagentNode`,
//! ~4968).
//!
//! Not ported here: child threads (`childThreadId`, `childRootNodeId`), which
//! wait on provider-core's `subagentProjection` (T08b); the subagent's own
//! transcript, which is routed to that child thread; the model canonicalisation
//! through the catalog; and the workflow coordinator's members. A subagent here
//! therefore has no child thread, and its frames are dropped, not routed.
use serde_json::Value;
use std::collections::HashMap;
use turin_contracts::{
    NodeId, ProviderThreadId, RunId, ThreadId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2NativeRefStrength, OrchestrationV2ProviderRef,
        OrchestrationV2Subagent, OrchestrationV2SubagentOrigin, OrchestrationV2SubagentStatus,
    },
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
};
use turin_provider_core::id_allocator::derive;

/// The `task_type` the CLI gives a subagent. `local_bash` is shell work, not
/// a subagent, and is ignored here.
const SUBAGENT_TASK_TYPE: &str = "local_agent";

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

#[derive(Default)]
pub struct SubagentRegistry {
    scope: Option<SubagentScope>,
    /// Keyed by the Agent tool_use id.
    launches: HashMap<String, Launch>,
    /// Keyed by the CLI task id.
    tasks: HashMap<String, OrchestrationV2Subagent>,
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

    /// task_started registers the subagent and opens its run. Returns the
    /// subagent to emit, or `None` for shell tasks and when no scope is set yet.
    pub fn started(&mut self, raw: &Value, now: UtcDateTime) -> Option<OrchestrationV2Subagent> {
        if raw.get("task_type").and_then(Value::as_str) != Some(SUBAGENT_TASK_TYPE) {
            return None;
        }
        let scope = self.scope.clone()?;
        let task_id = text(raw, "task_id")?;
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
            .map(|owner| owner.id.clone())
            .unwrap_or_else(|| scope.root_node_id.clone());
        let subagent = OrchestrationV2Subagent {
            id: derive::node_from_provider_item(&scope.driver, &format!("task:{task_id}")),
            thread_id: scope.thread_id.clone(),
            run_id: Some(scope.run_id.clone()),
            parent_node_id,
            origin: OrchestrationV2SubagentOrigin::ProviderNative,
            created_by: OrchestrationV2Actor::Agent,
            driver: scope.driver.clone(),
            provider_instance_id: scope.provider_instance_id.clone(),
            provider_thread_id: Some(scope.provider_thread_id.clone()),
            child_thread_id: None,
            native_task_ref: Some(native_ref(&task_id)),
            prompt: launch
                .prompt
                .or_else(|| text(raw, "prompt"))
                .unwrap_or_default(),
            title: launch.description.or_else(|| text(raw, "description")),
            model: launch.model,
            model_selection: None,
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
        if let Some(run_tool_use_id) = run_tool_use_id {
            self.task_by_run_tool_use
                .insert(run_tool_use_id, task_id.clone());
        }
        self.tasks.insert(task_id, subagent.clone());
        Some(subagent)
    }

    /// task_progress updates a running subagent's progress line.
    pub fn progress(&mut self, raw: &Value, now: UtcDateTime) -> Option<OrchestrationV2Subagent> {
        let task = self.tasks.get_mut(&text(raw, "task_id")?)?;
        if task.status != OrchestrationV2SubagentStatus::Running {
            return None;
        }
        let progress = text(raw, "summary").or_else(|| text(raw, "description"))?;
        task.progress = Some(progress);
        task.updated_at = now;
        Some(task.clone())
    }

    /// task_updated carries a status patch. Only its terminal statuses matter here.
    pub fn updated(&mut self, raw: &Value, now: UtcDateTime) -> Option<OrchestrationV2Subagent> {
        let status = raw.pointer("/patch/status").and_then(Value::as_str)?;
        self.settle(&text(raw, "task_id")?, status, None, now)
    }

    /// task_notification ends the task and carries its result text.
    pub fn notified(&mut self, raw: &Value, now: UtcDateTime) -> Option<OrchestrationV2Subagent> {
        let status = text(raw, "status")?;
        self.settle(&text(raw, "task_id")?, &status, text(raw, "summary"), now)
    }

    /// Status is monotone: a terminal subagent keeps its status. A result
    /// that arrives after the status (task_updated comes before
    /// task_notification) is still recorded.
    fn settle(
        &mut self,
        task_id: &str,
        status: &str,
        result: Option<String>,
        now: UtcDateTime,
    ) -> Option<OrchestrationV2Subagent> {
        let next = terminal_status(status);
        let task = self.tasks.get_mut(task_id)?;
        let mut changed = false;
        if let Some(next) = next {
            if task.status == OrchestrationV2SubagentStatus::Running {
                task.status = next;
                task.completed_at = Some(now);
                changed = true;
            }
        }
        if task.result.is_none() && result.is_some() {
            task.result = result;
            changed = true;
        }
        if !changed {
            return None;
        }
        task.updated_at = now;
        Some(task.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scope() -> SubagentScope {
        SubagentScope {
            driver: ProviderDriverKind::new("claudeAgent").unwrap(),
            provider_instance_id: serde_json::from_value(json!("claudeAgent")).unwrap(),
            thread_id: serde_json::from_value(json!("thread-1")).unwrap(),
            run_id: serde_json::from_value(json!("run-1")).unwrap(),
            root_node_id: serde_json::from_value(json!("node-root")).unwrap(),
            provider_thread_id: serde_json::from_value(json!("provider-thread-1")).unwrap(),
        }
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
        let started = registry
            .started(
                &json!({"subtype": "task_started", "task_id": "task_a", "tool_use_id": "toolu_a", "task_type": "local_agent"}),
                now,
            )
            .expect("subagent starts");
        assert_eq!(started.status, OrchestrationV2SubagentStatus::Running);
        assert_eq!(started.prompt, "Reply A");
        assert_eq!(started.title.as_deref(), Some("Agent A"));
        assert_eq!(started.model.as_deref(), Some("haiku"));
        assert_eq!(started.parent_node_id, scope().root_node_id);
        assert!(started.child_thread_id.is_none());

        let done = registry
            .notified(
                &json!({"task_id": "task_a", "status": "completed", "summary": "A_FIRST"}),
                now,
            )
            .expect("notification ends it");
        assert_eq!(done.status, OrchestrationV2SubagentStatus::Completed);
        assert_eq!(done.result.as_deref(), Some("A_FIRST"));
        // A later duplicate does not change a terminal subagent.
        assert!(
            registry
                .notified(&json!({"task_id": "task_a", "status": "failed"}), now)
                .is_none()
        );
    }

    #[test]
    fn task_updated_before_notification_keeps_the_result() {
        let mut registry = SubagentRegistry::default();
        registry.set_scope(scope());
        let now = UtcDateTime::now();
        registry.started(
            &json!({"task_id": "task_b", "task_type": "local_agent"}),
            now,
        );
        let updated = registry
            .updated(
                &json!({"task_id": "task_b", "patch": {"status": "completed"}}),
                now,
            )
            .expect("status lands");
        assert_eq!(updated.status, OrchestrationV2SubagentStatus::Completed);
        let noted = registry
            .notified(
                &json!({"task_id": "task_b", "status": "completed", "summary": "B_DONE"}),
                now,
            )
            .expect("result lands after the status");
        assert_eq!(noted.result.as_deref(), Some("B_DONE"));
    }

    #[test]
    fn shell_tasks_are_not_subagents() {
        let mut registry = SubagentRegistry::default();
        registry.set_scope(scope());
        assert!(
            registry
                .started(
                    &json!({"task_id": "bash_1", "task_type": "local_bash"}),
                    UtcDateTime::now()
                )
                .is_none()
        );
    }

    #[test]
    fn nested_launch_is_parented_to_its_owner_subagent() {
        let mut registry = SubagentRegistry::default();
        registry.set_scope(scope());
        let now = UtcDateTime::now();
        registry.remember_launch("toolu_outer", &json!({"description": "outer"}), None);
        registry.started(
            &json!({"task_id": "task_outer", "tool_use_id": "toolu_outer", "task_type": "local_agent"}),
            now,
        );
        registry.remember_launch(
            "toolu_inner",
            &json!({"description": "inner"}),
            Some("toolu_outer"),
        );
        let inner = registry
            .started(
                &json!({"task_id": "task_inner", "tool_use_id": "toolu_inner", "task_type": "local_agent"}),
                now,
            )
            .unwrap();
        assert_eq!(
            inner.parent_node_id,
            derive::node_from_provider_item(&scope().driver, "task:task_outer")
        );
    }
}
