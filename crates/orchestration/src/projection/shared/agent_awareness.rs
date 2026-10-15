//! Port of `packages/shared/src/agentAwareness.ts`: the sidebar and relay view of
//! a thread's agent activity, projected from the V2 shell row.
//!
//! The encoder for the deep link is a local copy of `encodeURIComponent`; the
//! shared helpers move to `turin-shared` with T05b (see `shared/mod.rs`), and
//! this copy goes then.

use turin_contracts::orchestration_v2::{
    OrchestrationV2ActivityRunStatus, OrchestrationV2LineageRelationship,
    OrchestrationV2RuntimeRequestKind, OrchestrationV2ShellThreadStatus,
    OrchestrationV2ThreadShell,
};
use turin_contracts::{EnvironmentId, ThreadId};

use crate::projection::shared::pending_background_work::background_work_holds_completion;

/// `AgentAwarenessPhase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAwarenessPhase {
    Starting,
    Running,
    WaitingForApproval,
    WaitingForInput,
    Completed,
    Failed,
    /// Never produced by the V2 projection; kept for the relay's stale check.
    Stale,
}

impl AgentAwarenessPhase {
    /// `headlineForPhase`.
    pub fn headline(self) -> &'static str {
        match self {
            Self::Starting => "Starting agent",
            Self::Running => "Agent is working",
            Self::WaitingForApproval => "Approval needed",
            Self::WaitingForInput => "Waiting for input",
            Self::Completed => "Agent finished",
            Self::Failed => "Agent failed",
            Self::Stale => "Update delayed",
        }
    }
}

/// `AgentAwarenessState`.
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
    /// `DateTime.formatIso(thread.updatedAt)`.
    pub updated_at: String,
    pub deep_link: String,
}

/// `ProjectThreadAwarenessV2Input`, with the project reduced to its title.
pub struct ProjectThreadAwarenessV2Input<'a> {
    pub environment_id: &'a EnvironmentId,
    pub project_title: &'a str,
    pub thread: &'a OrchestrationV2ThreadShell,
}

/// `projectThreadAwarenessV2`. `None` for subagent threads and for threads with
/// no active or completed-with-work state.
pub fn project_thread_awareness_v2(
    input: ProjectThreadAwarenessV2Input<'_>,
) -> Option<AgentAwarenessState> {
    let ProjectThreadAwarenessV2Input {
        environment_id,
        project_title,
        thread,
    } = input;
    if thread.lineage.relationship_to_parent == Some(OrchestrationV2LineageRelationship::Subagent) {
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
        headline: phase.headline(),
        detail,
        model_title: thread.model_selection.model.as_str().to_owned(),
        updated_at: thread.updated_at.to_string(),
        deep_link: deep_link(environment_id, &thread.id),
    })
}

fn resolve_thread_awareness_phase_v2(
    thread: &OrchestrationV2ThreadShell,
) -> Option<AgentAwarenessPhase> {
    // `activityRunStatus ?? status`: an absent or null activity status falls back to the shell status.
    let activity: Option<OrchestrationV2ActivityRunStatus> = thread.activity_run_status.flatten();
    if let Some(request) = &thread.pending_runtime_request {
        if request.kind == OrchestrationV2RuntimeRequestKind::UserInput {
            return Some(AgentAwarenessPhase::WaitingForInput);
        }
        if request.kind != OrchestrationV2RuntimeRequestKind::AuthRefresh {
            return Some(AgentAwarenessPhase::WaitingForApproval);
        }
    }
    match activity {
        Some(
            OrchestrationV2ActivityRunStatus::Preparing
            | OrchestrationV2ActivityRunStatus::Starting,
        ) => {
            return Some(AgentAwarenessPhase::Starting);
        }
        Some(
            OrchestrationV2ActivityRunStatus::Running | OrchestrationV2ActivityRunStatus::Waiting,
        ) => {
            return Some(AgentAwarenessPhase::Running);
        }
        None => {}
    }
    match thread.status {
        OrchestrationV2ShellThreadStatus::Preparing
        | OrchestrationV2ShellThreadStatus::Starting => Some(AgentAwarenessPhase::Starting),
        OrchestrationV2ShellThreadStatus::Running | OrchestrationV2ShellThreadStatus::Waiting => {
            Some(AgentAwarenessPhase::Running)
        }
        // Work that will wake the agent keeps the run going; a dev server does not.
        OrchestrationV2ShellThreadStatus::Completed => {
            if background_work_holds_completion(&thread.pending_background_tasks) {
                Some(AgentAwarenessPhase::Running)
            } else {
                Some(AgentAwarenessPhase::Completed)
            }
        }
        OrchestrationV2ShellThreadStatus::Failed => Some(AgentAwarenessPhase::Failed),
        OrchestrationV2ShellThreadStatus::Idle
        | OrchestrationV2ShellThreadStatus::Queued
        | OrchestrationV2ShellThreadStatus::Interrupted
        | OrchestrationV2ShellThreadStatus::Cancelled
        | OrchestrationV2ShellThreadStatus::RolledBack => None,
    }
}

/// `/threads/${encodeURIComponent(env)}/${encodeURIComponent(thread)}`.
fn deep_link(environment_id: &EnvironmentId, thread_id: &ThreadId) -> String {
    format!(
        "/threads/{}/{}",
        encode_uri_component(environment_id.as_str()),
        encode_uri_component(thread_id.as_str())
    )
}

/// JavaScript `encodeURIComponent`: unreserved characters pass, everything else
/// is percent-encoded per UTF-8 byte.
fn encode_uri_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => {
                out.push(char::from(byte));
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::test_support::at;
    use serde_json::{Value, json};

    const PROJECT_TITLE: &str = "t3code";

    fn shell(overrides: Value) -> OrchestrationV2ThreadShell {
        let mut base = json!({
            "createdBy": "user",
            "creationSource": "web",
            "id": "thread-2",
            "projectId": "project-1",
            "title": "Integrate orchestration",
            "providerInstanceId": "codex",
            "modelSelection": { "instanceId": "codex", "model": "gpt-5.4" },
            "runtimeMode": "full-access",
            "interactionMode": "default",
            "branch": null,
            "worktreePath": null,
            "lineage": {
                "rootThreadId": "thread-2",
                "parentThreadId": null,
                "relationshipToParent": null
            },
            "forkedFrom": null,
            "activeProviderThreadId": null,
            "historyOrigin": null,
            "latestRunId": null,
            "activeRunId": null,
            "activityRunStatus": null,
            "status": "running",
            "pendingRuntimeRequest": null,
            "pendingBackgroundTasks": [],
            "updatedAt": at(5),
            "createdAt": at(1),
            "latestUserMessageAt": at(1),
            "hasActionableProposedPlan": false,
            "providerInstanceHistory": [],
            "itemCount": 0,
            "visibleItemCount": 0,
        });
        let overrides = overrides.as_object().cloned().unwrap_or_default();
        base.as_object_mut().expect("object").extend(overrides);
        // Fields the awareness projection never reads still need values to decode.
        serde_json::from_value(base).expect("shell fixture decodes")
    }

    fn project(thread: &OrchestrationV2ThreadShell) -> Option<AgentAwarenessState> {
        let environment = EnvironmentId::new("env-1").expect("environment");
        project_thread_awareness_v2(ProjectThreadAwarenessV2Input {
            environment_id: &environment,
            project_title: PROJECT_TITLE,
            thread,
        })
    }

    #[test]
    fn projects_v2_run_state() {
        let state = project(&shell(json!({}))).expect("visible");

        assert_eq!(state.phase, AgentAwarenessPhase::Running);
        assert_eq!(state.headline, "Agent is working");
        assert_eq!(state.model_title, "gpt-5.4");
    }

    #[test]
    fn does_not_publish_subagent_activity() {
        for status in ["running", "completed", "failed"] {
            let thread = shell(json!({
                "status": status,
                "lineage": {
                    "rootThreadId": "parent",
                    "parentThreadId": "parent",
                    "relationshipToParent": "subagent"
                }
            }));
            assert!(project(&thread).is_none(), "{status}");
        }
    }

    #[test]
    fn keeps_an_older_activity_run_visible_over_a_newer_cancelled_run() {
        let state = project(&shell(json!({
            "status": "cancelled",
            "activityRunStatus": "running"
        })))
        .expect("visible");

        assert_eq!(state.phase, AgentAwarenessPhase::Running);
        assert_eq!(state.headline, "Agent is working");
    }

    #[test]
    fn reports_a_completed_run_waiting_on_background_work() {
        let cases = [
            (
                "only a dev server",
                "completed",
                json!([{ "taskId": "dev", "kind": "command" }]),
            ),
            (
                "a monitor",
                "running",
                json!([{ "taskId": "watch", "kind": "monitor" }]),
            ),
            (
                "a dev server and a subagent",
                "running",
                json!([
                    { "taskId": "dev", "kind": "command" },
                    { "taskId": "review", "kind": "subagent" }
                ]),
            ),
        ];
        for (case, phase, tasks) in cases {
            let state = project(&shell(json!({
                "status": "completed",
                "pendingBackgroundTasks": tasks
            })))
            .expect("visible");
            let expected = if phase == "running" {
                AgentAwarenessPhase::Running
            } else {
                AgentAwarenessPhase::Completed
            };
            assert_eq!(state.phase, expected, "{case}");
        }
    }

    #[test]
    fn prioritizes_v2_user_input_requests() {
        let state = project(&shell(json!({
            "pendingRuntimeRequest": {
                "id": "request-1",
                "kind": "user_input",
                "createdAt": at(5)
            }
        })))
        .expect("visible");

        assert_eq!(state.phase, AgentAwarenessPhase::WaitingForInput);
        assert_eq!(state.headline, "Waiting for input");
    }

    #[test]
    fn does_not_present_authentication_refreshes_as_user_approvals() {
        let state = project(&shell(json!({
            "pendingRuntimeRequest": {
                "id": "request-auth-refresh",
                "kind": "auth_refresh",
                "createdAt": at(5)
            }
        })))
        .expect("visible");

        assert_eq!(state.phase, AgentAwarenessPhase::Running);
        assert_eq!(state.headline, "Agent is working");
    }

    #[test]
    fn encodes_the_deep_link_like_encode_uri_component() {
        assert_eq!(encode_uri_component("env 1/a"), "env%201%2Fa");
        assert_eq!(
            encode_uri_component("thread-2_x.y!~*'()"),
            "thread-2_x.y!~*'()"
        );
    }
}
