//! Port of upstream/packages/shared/src/agentAwareness.ts
//!
//! Upstream takes a `Pick` of the V2 thread shell. [`AgentAwarenessThread`] is that pick, so the
//! caller maps the shell into it and this module does not depend on the shell's full shape.
//! `status` is `None` for upstream's `idle`, which is not a run status and never has awareness.

use turin_contracts::base_schemas::{EnvironmentId, ThreadId, UtcDateTime};
use turin_contracts::orchestration_v2::{
    OrchestrationV2LineageRelationship, OrchestrationV2PendingBackgroundTask,
    OrchestrationV2RunStatus, OrchestrationV2RuntimeRequestKind,
};

use crate::js_string::encode_uri_component;
use crate::orchestration_v2_pending_background_work::background_work_holds_completion;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAwarenessPhase {
    Starting,
    Running,
    WaitingForApproval,
    WaitingForInput,
    Completed,
    Failed,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAwarenessState {
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
    pub project_title: String,
    pub thread_title: String,
    pub phase: AgentAwarenessPhase,
    pub headline: &'static str,
    pub detail: Option<&'static str>,
    pub model_title: String,
    pub updated_at: String,
    pub deep_link: String,
}

/// The thread fields awareness reads (upstream's `Pick<OrchestrationV2ThreadShell, ...>`).
#[derive(Debug, Clone, PartialEq)]
pub struct AgentAwarenessThread {
    pub id: ThreadId,
    pub title: String,
    pub model: String,
    pub updated_at: UtcDateTime,
    pub relationship_to_parent: Option<OrchestrationV2LineageRelationship>,
    /// `None` is upstream's `idle`.
    pub status: Option<OrchestrationV2RunStatus>,
    /// Wins over `status` when set, as `activityRunStatus ?? status` does.
    pub activity_run_status: Option<OrchestrationV2RunStatus>,
    pub pending_runtime_request: Option<OrchestrationV2RuntimeRequestKind>,
    pub pending_background_tasks: Vec<OrchestrationV2PendingBackgroundTask>,
}

/// Build relay activity directly from the V2 shell projection. `None` for subagents and for
/// threads with nothing to show.
pub fn project_thread_awareness_v2(
    environment_id: &EnvironmentId,
    project_title: &str,
    thread: &AgentAwarenessThread,
) -> Option<AgentAwarenessState> {
    if thread.relationship_to_parent == Some(OrchestrationV2LineageRelationship::Subagent) {
        return None;
    }
    let phase = resolve_thread_awareness_phase_v2(thread)?;
    let detail = match phase {
        AgentAwarenessPhase::Completed => Some("Review the completed task."),
        AgentAwarenessPhase::Failed => Some("The agent run failed."),
        _ => None,
    };
    Some(AgentAwarenessState {
        environment_id: environment_id.clone(),
        thread_id: thread.id.clone(),
        project_title: project_title.to_owned(),
        thread_title: thread.title.clone(),
        phase,
        headline: headline_for_phase(phase),
        detail,
        model_title: thread.model.clone(),
        updated_at: thread.updated_at.to_string(),
        deep_link: format!(
            "/threads/{}/{}",
            encode_uri_component(environment_id.as_str()),
            encode_uri_component(thread.id.as_str())
        ),
    })
}

fn resolve_thread_awareness_phase_v2(thread: &AgentAwarenessThread) -> Option<AgentAwarenessPhase> {
    match thread.pending_runtime_request {
        Some(OrchestrationV2RuntimeRequestKind::UserInput) => {
            return Some(AgentAwarenessPhase::WaitingForInput);
        }
        // Authentication refreshes are not user approvals.
        Some(kind) if kind != OrchestrationV2RuntimeRequestKind::AuthRefresh => {
            return Some(AgentAwarenessPhase::WaitingForApproval);
        }
        _ => {}
    }
    match thread.activity_run_status.or(thread.status)? {
        OrchestrationV2RunStatus::Preparing | OrchestrationV2RunStatus::Starting => {
            Some(AgentAwarenessPhase::Starting)
        }
        OrchestrationV2RunStatus::Running | OrchestrationV2RunStatus::Waiting => {
            Some(AgentAwarenessPhase::Running)
        }
        // Work that will wake the agent keeps the run going; a dev server does not.
        OrchestrationV2RunStatus::Completed => {
            if background_work_holds_completion(&thread.pending_background_tasks) {
                Some(AgentAwarenessPhase::Running)
            } else {
                Some(AgentAwarenessPhase::Completed)
            }
        }
        OrchestrationV2RunStatus::Failed => Some(AgentAwarenessPhase::Failed),
        OrchestrationV2RunStatus::Queued
        | OrchestrationV2RunStatus::Interrupted
        | OrchestrationV2RunStatus::Cancelled
        | OrchestrationV2RunStatus::RolledBack => None,
    }
}

fn headline_for_phase(phase: AgentAwarenessPhase) -> &'static str {
    match phase {
        AgentAwarenessPhase::Starting => "Starting agent",
        AgentAwarenessPhase::Running => "Agent is working",
        AgentAwarenessPhase::WaitingForApproval => "Approval needed",
        AgentAwarenessPhase::WaitingForInput => "Waiting for input",
        AgentAwarenessPhase::Completed => "Agent finished",
        AgentAwarenessPhase::Failed => "Agent failed",
        AgentAwarenessPhase::Stale => "Update delayed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::orchestration_v2::OrchestrationV2RunStatus as Run;

    const NOW: &str = "2026-05-22T12:00:00.000Z";

    fn environment() -> EnvironmentId {
        EnvironmentId::new("env-1").unwrap()
    }

    fn thread() -> AgentAwarenessThread {
        AgentAwarenessThread {
            id: ThreadId::new("thread-2").unwrap(),
            title: "Integrate orchestration".to_owned(),
            model: "gpt-5.4".to_owned(),
            updated_at: UtcDateTime::from_datetime(
                chrono::DateTime::parse_from_rfc3339(NOW)
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
            relationship_to_parent: None,
            status: Some(Run::Running),
            activity_run_status: None,
            pending_runtime_request: None,
            pending_background_tasks: vec![],
        }
    }

    fn command(task_id: &str) -> OrchestrationV2PendingBackgroundTask {
        OrchestrationV2PendingBackgroundTask::Command {
            task_id: turin_contracts::base_schemas::TrimmedNonEmptyString::new(task_id).unwrap(),
            description: None,
        }
    }

    fn monitor(task_id: &str) -> OrchestrationV2PendingBackgroundTask {
        OrchestrationV2PendingBackgroundTask::Monitor {
            task_id: turin_contracts::base_schemas::TrimmedNonEmptyString::new(task_id).unwrap(),
            description: None,
        }
    }

    fn subagent(task_id: &str) -> OrchestrationV2PendingBackgroundTask {
        OrchestrationV2PendingBackgroundTask::Subagent {
            task_id: turin_contracts::base_schemas::TrimmedNonEmptyString::new(task_id).unwrap(),
            description: None,
            child_thread_id: None,
        }
    }

    fn project(thread: &AgentAwarenessThread) -> Option<AgentAwarenessState> {
        project_thread_awareness_v2(&environment(), "t3code", thread)
    }

    fn phase_of(thread: &AgentAwarenessThread) -> Option<AgentAwarenessPhase> {
        project(thread).map(|state| state.phase)
    }

    #[test]
    fn projects_v2_run_state() {
        let state = project(&thread()).unwrap();
        assert_eq!(state.phase, AgentAwarenessPhase::Running);
        assert_eq!(state.headline, "Agent is working");
        assert_eq!(state.detail, None);
        assert_eq!(state.model_title, "gpt-5.4");
        assert_eq!(state.updated_at, NOW);
        assert_eq!(state.deep_link, "/threads/env-1/thread-2");
        assert_eq!(state.project_title, "t3code");
    }

    #[test]
    fn does_not_publish_subagent_activity() {
        for status in [Run::Running, Run::Completed, Run::Failed] {
            let subagent_thread = AgentAwarenessThread {
                status: Some(status),
                relationship_to_parent: Some(OrchestrationV2LineageRelationship::Subagent),
                ..thread()
            };
            assert_eq!(project(&subagent_thread), None, "{status:?}");
        }
    }

    #[test]
    fn keeps_an_older_activity_run_visible_over_a_newer_cancelled_run() {
        let cancelled = AgentAwarenessThread {
            status: Some(Run::Cancelled),
            activity_run_status: Some(Run::Running),
            ..thread()
        };
        assert_eq!(phase_of(&cancelled), Some(AgentAwarenessPhase::Running));
    }

    #[test]
    fn reports_a_completed_run_waiting_on_background_work_as_the_work_requires() {
        let cases = [
            (
                "only a dev server",
                AgentAwarenessPhase::Completed,
                vec![command("dev")],
            ),
            (
                "a monitor",
                AgentAwarenessPhase::Running,
                vec![monitor("watch")],
            ),
            (
                "a dev server and a subagent",
                AgentAwarenessPhase::Running,
                vec![command("dev"), subagent("review")],
            ),
        ];
        for (case, phase, tasks) in cases {
            let completed = AgentAwarenessThread {
                status: Some(Run::Completed),
                pending_background_tasks: tasks,
                ..thread()
            };
            assert_eq!(phase_of(&completed), Some(phase), "{case}");
        }
    }

    #[test]
    fn completed_and_failed_runs_carry_their_detail() {
        let completed = AgentAwarenessThread {
            status: Some(Run::Completed),
            ..thread()
        };
        let state = project(&completed).unwrap();
        assert_eq!(state.headline, "Agent finished");
        assert_eq!(state.detail, Some("Review the completed task."));
        let failed = AgentAwarenessThread {
            status: Some(Run::Failed),
            ..thread()
        };
        assert_eq!(
            project(&failed).unwrap().detail,
            Some("The agent run failed.")
        );
    }

    #[test]
    fn prioritizes_v2_user_input_requests() {
        let waiting = AgentAwarenessThread {
            pending_runtime_request: Some(OrchestrationV2RuntimeRequestKind::UserInput),
            ..thread()
        };
        let state = project(&waiting).unwrap();
        assert_eq!(state.phase, AgentAwarenessPhase::WaitingForInput);
        assert_eq!(state.headline, "Waiting for input");
    }

    #[test]
    fn does_not_present_authentication_refreshes_as_user_approvals() {
        let refreshing = AgentAwarenessThread {
            pending_runtime_request: Some(OrchestrationV2RuntimeRequestKind::AuthRefresh),
            ..thread()
        };
        let state = project(&refreshing).unwrap();
        assert_eq!(state.phase, AgentAwarenessPhase::Running);
        assert_eq!(state.headline, "Agent is working");
    }

    #[test]
    fn an_idle_or_queued_thread_has_no_awareness() {
        assert_eq!(
            project(&AgentAwarenessThread {
                status: None,
                ..thread()
            }),
            None
        );
        assert_eq!(
            project(&AgentAwarenessThread {
                status: Some(Run::Queued),
                ..thread()
            }),
            None
        );
    }
}
