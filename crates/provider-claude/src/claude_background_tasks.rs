//! Port of the background-task roster in upstream/apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts:
//! `CLAUDE_OPAQUE_BACKGROUND_TASK_KINDS` (~2052), `parseClaudeBackgroundTaskEntry`
//! (~2115), `claudePendingBackgroundTask` (~2061), the `background_tasks_changed`
//! snapshot, and the task_started and task_notification edges that keep the
//! roster (`replacePendingBackgroundTasks`, `upsertPendingBackgroundTask`,
//! `clearPendingBackgroundTask`).
//!
//! The roster lists the non-subagent work a thread has running in the
//! background (today a `local_bash` task). Subagents are not on it; they are
//! subagent records. The roster does not drive wake turns here (item 5).
use serde_json::Value;
use turin_contracts::{
    TrimmedNonEmptyString, orchestration_v2::OrchestrationV2PendingBackgroundTask,
};

/// The CLI task types that live on the roster, and their kinds. A Monitor is a
/// `local_bash` task too; only the Monitor tool call that started it tells them apart.
const OPAQUE_TASK_TYPES: [&str; 1] = ["local_bash"];

pub fn is_opaque_task_type(task_type: Option<&str>) -> bool {
    task_type.is_some_and(|task_type| OPAQUE_TASK_TYPES.contains(&task_type))
}

/// Upstream `claudePendingBackgroundTask`. A blank description is dropped.
pub fn pending_task(
    task_id: &str,
    task_type: Option<&str>,
    started_by_monitor: bool,
    description: Option<&str>,
) -> Option<OrchestrationV2PendingBackgroundTask> {
    let task_id = TrimmedNonEmptyString::new(task_id).ok()?;
    let description = description.and_then(|text| TrimmedNonEmptyString::new(text).ok());
    Some(if started_by_monitor {
        OrchestrationV2PendingBackgroundTask::Monitor {
            task_id,
            description,
        }
    } else if task_type == Some("local_bash") {
        OrchestrationV2PendingBackgroundTask::Command {
            task_id,
            description,
        }
    } else {
        OrchestrationV2PendingBackgroundTask::BackgroundTask {
            task_id,
            description,
        }
    })
}

/// Upstream `parseClaudeBackgroundTaskEntry`: one entry of a roster snapshot.
/// Subagents and other types stay off the roster.
pub fn parse_roster_entry(
    entry: &Value,
    started_by_monitor: impl Fn(&str) -> bool,
) -> Option<OrchestrationV2PendingBackgroundTask> {
    let task_id = entry.get("task_id").and_then(Value::as_str)?;
    if task_id.is_empty() {
        return None;
    }
    let task_type = entry.get("task_type").and_then(Value::as_str);
    if !is_opaque_task_type(task_type) {
        return None;
    }
    let description = entry.get("description").and_then(Value::as_str);
    pending_task(task_id, task_type, started_by_monitor(task_id), description)
}

pub fn task_id(task: &OrchestrationV2PendingBackgroundTask) -> &str {
    task.task_id().as_str()
}

/// The roster of one native thread, in first-seen order (upstream's `Map`).
#[derive(Default)]
pub struct Roster {
    tasks: Vec<OrchestrationV2PendingBackgroundTask>,
}

impl Roster {
    pub fn tasks(&self) -> &[OrchestrationV2PendingBackgroundTask] {
        &self.tasks
    }

    /// Upstream `replacePendingBackgroundTasks`: a snapshot replaces the roster.
    pub fn replace(&mut self, tasks: Vec<OrchestrationV2PendingBackgroundTask>) {
        self.tasks = tasks;
    }

    /// Upstream `upsertPendingBackgroundTask`: a known task keeps its place.
    pub fn upsert(&mut self, task: OrchestrationV2PendingBackgroundTask) {
        match self.tasks.iter().position(|t| task_id(t) == task_id(&task)) {
            Some(index) => self.tasks[index] = task,
            None => self.tasks.push(task),
        }
    }

    /// Upstream `clearPendingBackgroundTask`. Returns whether it was listed.
    pub fn remove(&mut self, task_id: &str) -> bool {
        let before = self.tasks.len();
        self.tasks.retain(|t| self::task_id(t) != task_id);
        self.tasks.len() != before
    }

    pub fn clear(&mut self) -> bool {
        let changed = !self.tasks.is_empty();
        self.tasks.clear();
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn local_bash_is_a_command_and_other_types_stay_off_the_roster() {
        let command = parse_roster_entry(
            &json!({"task_id": "bg-1", "task_type": "local_bash", "description": "npm test"}),
            |_| false,
        )
        .expect("a local_bash task is on the roster");
        assert!(matches!(
            command,
            OrchestrationV2PendingBackgroundTask::Command { .. }
        ));
        assert_eq!(task_id(&command), "bg-1");
        assert_eq!(command.description().map(|d| d.as_str()), Some("npm test"));

        assert!(
            parse_roster_entry(
                &json!({"task_id": "agent-1", "task_type": "local_agent"}),
                |_| false
            )
            .is_none(),
            "a subagent is a subagent record, not roster work"
        );
        assert!(parse_roster_entry(&json!({"task_type": "local_bash"}), |_| false).is_none());
    }

    #[test]
    fn a_monitor_is_told_apart_by_the_tool_call_that_started_it() {
        let monitor = parse_roster_entry(
            &json!({"task_id": "mon-1", "task_type": "local_bash", "description": "  "}),
            |id| id == "mon-1",
        )
        .unwrap();
        assert!(matches!(
            monitor,
            OrchestrationV2PendingBackgroundTask::Monitor { .. }
        ));
        // A blank description is no description.
        assert!(monitor.description().is_none());
    }

    #[test]
    fn roster_keeps_first_seen_order_and_reports_changes() {
        let mut roster = Roster::default();
        let task = |id: &str| pending_task(id, Some("local_bash"), false, None).unwrap();
        roster.upsert(task("a"));
        roster.upsert(task("b"));
        roster.upsert(task("a"));
        let ids: Vec<&str> = roster.tasks().iter().map(task_id).collect();
        assert_eq!(ids, vec!["a", "b"]);
        assert!(roster.remove("a"));
        assert!(
            !roster.remove("a"),
            "clearing an unlisted task is no change"
        );
        assert!(roster.clear());
        assert!(!roster.clear());
    }
}
