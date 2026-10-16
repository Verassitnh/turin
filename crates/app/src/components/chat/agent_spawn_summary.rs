//! Port of upstream/apps/web/src/components/chat/agentSpawnSummary.ts

use crate::state::subagent_runtime::{
    RuntimeSubagentKind, RuntimeSubagentStatus, is_active_subagent_status,
    is_terminal_subagent_status,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSpawnSummary {
    pub live: bool,
    pub lead: String,
    pub status: String,
    pub tone: AgentSpawnSummaryTone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSpawnSummaryTone {
    Working,
    Failed,
    Completed,
    Inactive,
}

impl AgentSpawnSummaryTone {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Failed => "failed",
            Self::Completed => "completed",
            Self::Inactive => "inactive",
        }
    }
}

pub struct AgentSpawnAgent {
    pub kind: RuntimeSubagentKind,
    pub status: RuntimeSubagentStatus,
}

/// Summarize observed states without treating idle or missing agents as completed.
pub fn derive_agent_spawn_summary(
    agents: &[AgentSpawnAgent],
    agent_count: u64,
    coordinator_status: Option<RuntimeSubagentStatus>,
) -> AgentSpawnSummary {
    let working = agents
        .iter()
        .filter(|agent| is_active_subagent_status(agent.status))
        .count();
    let failed = agents
        .iter()
        .filter(|agent| agent.status == RuntimeSubagentStatus::Failed)
        .count();
    let idle = agents
        .iter()
        .filter(|agent| agent.status == RuntimeSubagentStatus::Idle)
        .count();
    let stopped = agents
        .iter()
        .filter(|agent| {
            matches!(
                agent.status,
                RuntimeSubagentStatus::Cancelled | RuntimeSubagentStatus::Interrupted
            )
        })
        .count();
    let batches = agents
        .iter()
        .filter(|agent| agent.kind == RuntimeSubagentKind::SubagentBatch)
        .count();
    let individuals = agent_count.saturating_sub(batches as u64);
    // Workflow coordinators can keep running between dynamic member launches.
    let live = coordinator_status
        .map(|status| !is_terminal_subagent_status(status))
        .unwrap_or(working > 0);
    let mut subjects = Vec::new();
    if individuals > 0 {
        subjects.push(format!(
            "{individuals} subagent{}",
            if individuals == 1 { "" } else { "s" }
        ));
    }
    if batches > 0 {
        let prefix = if individuals > 0 { "" } else { "subagent " };
        subjects.push(format!(
            "{batches} {prefix}batch{}",
            if batches == 1 { "" } else { "es" }
        ));
    }
    let subjects = subjects.join(" and ");
    let verb = if batches > 0 {
        "Launched"
    } else if live {
        "Kicked off"
    } else {
        "Ran"
    };
    let lead = format!(
        "{verb} {}",
        if subjects.is_empty() {
            "subagents"
        } else {
            &subjects
        }
    );

    let status = if live {
        if working > 0 {
            format!("{working} working")
        } else {
            "working".to_owned()
        }
    } else if coordinator_status == Some(RuntimeSubagentStatus::Failed) {
        "Workflow failed".to_owned()
    } else if matches!(
        coordinator_status,
        Some(RuntimeSubagentStatus::Cancelled) | Some(RuntimeSubagentStatus::Interrupted)
    ) {
        "Workflow stopped".to_owned()
    } else if failed > 0 {
        format!("{failed} failed")
    } else if stopped > 0 {
        format!("{stopped} stopped")
    } else if idle > 0 {
        format!("{idle} idle")
    } else if coordinator_status != Some(RuntimeSubagentStatus::Completed)
        && (agents.is_empty() || agents.len() < agent_count as usize)
    {
        "Status unavailable".to_owned()
    } else {
        "✓ completed".to_owned()
    };

    let tone = if live {
        AgentSpawnSummaryTone::Working
    } else if failed > 0 || coordinator_status == Some(RuntimeSubagentStatus::Failed) {
        AgentSpawnSummaryTone::Failed
    } else if status == "✓ completed" {
        AgentSpawnSummaryTone::Completed
    } else {
        AgentSpawnSummaryTone::Inactive
    };

    AgentSpawnSummary {
        live,
        lead,
        status,
        tone,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch(status: RuntimeSubagentStatus) -> AgentSpawnAgent {
        AgentSpawnAgent {
            kind: RuntimeSubagentKind::SubagentBatch,
            status,
        }
    }

    fn agent(status: RuntimeSubagentStatus) -> AgentSpawnAgent {
        AgentSpawnAgent {
            kind: RuntimeSubagentKind::Subagent,
            status,
        }
    }

    #[test]
    fn counts_native_batch_without_claiming_child_count() {
        assert_eq!(
            derive_agent_spawn_summary(&[batch(RuntimeSubagentStatus::Running)], 1, None),
            AgentSpawnSummary {
                live: true,
                lead: "Launched 1 subagent batch".into(),
                status: "1 working".into(),
                tone: AgentSpawnSummaryTone::Working,
            }
        );
        assert_eq!(
            derive_agent_spawn_summary(&[batch(RuntimeSubagentStatus::Idle)], 1, None),
            AgentSpawnSummary {
                live: false,
                lead: "Launched 1 subagent batch".into(),
                status: "1 idle".into(),
                tone: AgentSpawnSummaryTone::Inactive,
            }
        );
    }

    #[test]
    fn keeps_individuals_and_batches_separate_in_mixed_group() {
        let summary = derive_agent_spawn_summary(
            &[
                agent(RuntimeSubagentStatus::Running),
                batch(RuntimeSubagentStatus::Running),
                batch(RuntimeSubagentStatus::Idle),
            ],
            3,
            None,
        );
        assert_eq!(summary.lead, "Launched 1 subagent and 2 batches");
    }

    #[test]
    fn reports_terminal_states_accurately() {
        for (state, status, tone) in [
            (
                RuntimeSubagentStatus::Idle,
                "1 idle",
                AgentSpawnSummaryTone::Inactive,
            ),
            (
                RuntimeSubagentStatus::Cancelled,
                "1 stopped",
                AgentSpawnSummaryTone::Inactive,
            ),
            (
                RuntimeSubagentStatus::Interrupted,
                "1 stopped",
                AgentSpawnSummaryTone::Inactive,
            ),
            (
                RuntimeSubagentStatus::Failed,
                "1 failed",
                AgentSpawnSummaryTone::Failed,
            ),
            (
                RuntimeSubagentStatus::Completed,
                "✓ completed",
                AgentSpawnSummaryTone::Completed,
            ),
        ] {
            let summary = derive_agent_spawn_summary(
                &[agent(RuntimeSubagentStatus::Completed), agent(state)],
                2,
                None,
            );
            assert!(!summary.live);
            assert_eq!(summary.status, status);
            assert_eq!(summary.tone, tone);
        }
    }

    #[test]
    fn does_not_claim_completion_when_roster_is_incomplete() {
        let summary =
            derive_agent_spawn_summary(&[agent(RuntimeSubagentStatus::Completed)], 2, None);
        assert_eq!(summary.status, "Status unavailable");
        assert_eq!(summary.tone, AgentSpawnSummaryTone::Inactive);
    }

    #[test]
    fn keeps_workflow_active_between_child_launches() {
        let summary = derive_agent_spawn_summary(
            &[agent(RuntimeSubagentStatus::Completed)],
            1,
            Some(RuntimeSubagentStatus::Running),
        );
        assert!(summary.live);
        assert_eq!(summary.status, "working");
        assert_eq!(summary.tone, AgentSpawnSummaryTone::Working);
    }

    #[test]
    fn preserves_workflow_outcome_when_children_completed() {
        for (coordinator, status, tone) in [
            (
                RuntimeSubagentStatus::Failed,
                "Workflow failed",
                AgentSpawnSummaryTone::Failed,
            ),
            (
                RuntimeSubagentStatus::Cancelled,
                "Workflow stopped",
                AgentSpawnSummaryTone::Inactive,
            ),
        ] {
            let summary = derive_agent_spawn_summary(
                &[agent(RuntimeSubagentStatus::Completed)],
                1,
                Some(coordinator),
            );
            assert!(!summary.live);
            assert_eq!(summary.status, status);
            assert_eq!(summary.tone, tone);
        }
    }
}
