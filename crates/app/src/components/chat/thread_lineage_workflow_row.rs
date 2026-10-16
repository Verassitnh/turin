//! Port of upstream/apps/web/src/components/chat/ThreadLineageWorkflowRow.tsx
//!
//! Workflow phase timing and status labels. The expandable row view is
//! [`ThreadLineageWorkflowRow`].

use crate::state::subagent_runtime::{
    PhaseState, RuntimeSubagent, RuntimeSubagentStatus, SubagentWorkflowGroupPhase,
};

/// Wall clock for the phase: members run in parallel, so the span from the
/// first start to the last finish is the time the phase actually took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhaseElapsed {
    pub status: RuntimeSubagentStatus,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

pub fn phase_elapsed(phase: &SubagentWorkflowGroupPhase) -> PhaseElapsed {
    let instants = |key: fn(&RuntimeSubagent) -> &Option<String>| {
        let mut values: Vec<&str> = phase
            .members
            .iter()
            .filter_map(|member| key(member).as_deref())
            .collect();
        values.sort_unstable();
        values
    };
    let running = phase.state == PhaseState::Running;
    let started_at = instants(|m| &m.started_at).first().map(|s| (*s).to_owned());
    let completed_at = if running {
        None
    } else {
        instants(|m| &m.completed_at)
            .last()
            .map(|s| (*s).to_owned())
    };
    PhaseElapsed {
        status: if running {
            RuntimeSubagentStatus::Running
        } else {
            RuntimeSubagentStatus::Completed
        },
        started_at: if running || completed_at.is_some() {
            started_at
        } else {
            None
        },
        completed_at,
    }
}

/// Phase state for the header's label; tone and member badges show it visually.
pub fn phase_status_label(phase: &SubagentWorkflowGroupPhase) -> &'static str {
    if phase.state == PhaseState::Running {
        return "running";
    }
    if phase
        .members
        .iter()
        .any(|member| member.status == RuntimeSubagentStatus::Failed)
    {
        return "failed";
    }
    if phase.members.iter().any(|member| {
        matches!(
            member.status,
            RuntimeSubagentStatus::Cancelled | RuntimeSubagentStatus::Interrupted
        )
    }) {
        return "stopped";
    }
    if phase.state == PhaseState::Done {
        "done"
    } else {
        "not started"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::subagent_runtime::PhaseState;

    fn member(
        started: &str,
        completed: Option<&str>,
        status: RuntimeSubagentStatus,
    ) -> RuntimeSubagent {
        use crate::state::subagent_runtime::RuntimeSubagentKind;
        RuntimeSubagent {
            id: "m".into(),
            kind: RuntimeSubagentKind::WorkflowAgent,
            title: "Member".into(),
            role: None,
            model: None,
            effort: None,
            status,
            activation_count: 0,
            usage: None,
            progress: None,
            last_tool_name: None,
            result: None,
            error: None,
            output_file: None,
            parent_agent_id: None,
            agent_index: None,
            phase_index: None,
            phase_title: None,
            attempt: None,
            workflow_name: None,
            phases: Vec::new(),
            run_handles: None,
            child_thread_id: None,
            recent_activity: Vec::new(),
            first_seen_at: started.into(),
            started_at: Some(started.into()),
            completed_at: completed.map(str::to_owned),
            updated_at: started.into(),
        }
    }

    #[test]
    fn phase_elapsed_uses_first_start_and_last_finish() {
        let phase = SubagentWorkflowGroupPhase {
            index: 0,
            title: "Inspect".into(),
            members: vec![
                member(
                    "2026-01-01T12:00:00Z",
                    Some("2026-01-01T12:05:00Z"),
                    RuntimeSubagentStatus::Completed,
                ),
                member(
                    "2026-01-01T12:01:00Z",
                    Some("2026-01-01T12:10:00Z"),
                    RuntimeSubagentStatus::Completed,
                ),
            ],
            state: PhaseState::Done,
            settled_count: 2,
        };
        let elapsed = phase_elapsed(&phase);
        assert_eq!(elapsed.started_at.as_deref(), Some("2026-01-01T12:00:00Z"));
        assert_eq!(
            elapsed.completed_at.as_deref(),
            Some("2026-01-01T12:10:00Z")
        );
    }

    #[test]
    fn running_phase_has_no_completed_at() {
        let phase = SubagentWorkflowGroupPhase {
            index: 0,
            title: "Run".into(),
            members: vec![member(
                "2026-01-01T12:00:00Z",
                None,
                RuntimeSubagentStatus::Running,
            )],
            state: PhaseState::Running,
            settled_count: 0,
        };
        let elapsed = phase_elapsed(&phase);
        assert_eq!(elapsed.status, RuntimeSubagentStatus::Running);
        assert!(elapsed.completed_at.is_none());
    }

    #[test]
    fn phase_status_label_prefers_failed_members() {
        let phase = SubagentWorkflowGroupPhase {
            index: 0,
            title: "Run".into(),
            members: vec![member(
                "2026-01-01T12:00:00Z",
                Some("2026-01-01T12:01:00Z"),
                RuntimeSubagentStatus::Failed,
            )],
            state: PhaseState::Done,
            settled_count: 1,
        };
        assert_eq!(phase_status_label(&phase), "failed");
    }
}
