//! Port of upstream/packages/client-runtime/src/state/subagentRuntime.ts
//!
//! Subagent status helpers shared by web and mobile, and the runtime shape the
//! web agent rows render.

use std::collections::HashMap;

use crate::state::contracts::{
    OrchestrationV2Subagent, OrchestrationV2SubagentStatus, OrchestrationV2SubagentWorkflow,
    OrchestrationV2TurnItem, OrchestrationV2WorkflowAgentState, UtcDateTime,
};
use crate::state::support::turn_item::{
    TurnItemExt, TurnItemType, is_subagent_status_active, is_turn_item_status_active,
};
use crate::state::time::{iso, to_iso_string};

/// `RuntimeSubagentStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeSubagentStatus {
    Pending,
    Running,
    Waiting,
    Idle,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl From<OrchestrationV2SubagentStatus> for RuntimeSubagentStatus {
    fn from(status: OrchestrationV2SubagentStatus) -> Self {
        match status {
            OrchestrationV2SubagentStatus::Idle => Self::Idle,
            OrchestrationV2SubagentStatus::Pending => Self::Pending,
            OrchestrationV2SubagentStatus::Running => Self::Running,
            OrchestrationV2SubagentStatus::Waiting => Self::Waiting,
            OrchestrationV2SubagentStatus::Completed => Self::Completed,
            OrchestrationV2SubagentStatus::Failed => Self::Failed,
            OrchestrationV2SubagentStatus::Cancelled => Self::Cancelled,
            OrchestrationV2SubagentStatus::Interrupted => Self::Interrupted,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubagentUsage {
    pub total_tokens: u64,
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    pub tool_uses: Option<u64>,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentActivityEntry {
    pub at: String,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentWorkflowPhase {
    pub index: u64,
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubagentRunHandles {
    pub run_id: Option<String>,
    pub script_path: Option<String>,
    pub transcript_dir: Option<String>,
    pub session_url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeSubagentKind {
    Subagent,
    SubagentBatch,
    Workflow,
    WorkflowAgent,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeSubagent {
    pub id: String,
    pub kind: RuntimeSubagentKind,
    pub title: String,
    pub role: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub status: RuntimeSubagentStatus,
    pub activation_count: u64,
    pub usage: Option<SubagentUsage>,
    pub progress: Option<String>,
    pub last_tool_name: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub output_file: Option<String>,
    pub parent_agent_id: Option<String>,
    pub agent_index: Option<u64>,
    pub phase_index: Option<u64>,
    pub phase_title: Option<String>,
    pub attempt: Option<u64>,
    pub workflow_name: Option<String>,
    pub phases: Vec<SubagentWorkflowPhase>,
    pub run_handles: Option<SubagentRunHandles>,
    /// The thread this agent owns, when it has one.
    pub child_thread_id: Option<String>,
    pub recent_activity: Vec<SubagentActivityEntry>,
    /// First retained observation, used as the roster's stable display order.
    pub first_seen_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub updated_at: String,
}

pub fn is_terminal_subagent_status(status: RuntimeSubagentStatus) -> bool {
    matches!(
        status,
        RuntimeSubagentStatus::Completed
            | RuntimeSubagentStatus::Failed
            | RuntimeSubagentStatus::Cancelled
            | RuntimeSubagentStatus::Interrupted
    )
}

/// Active = the user may still need to care while it runs. Idle is settled-ish
/// but resumable; waiting counts as active because it needs the user.
pub fn is_active_subagent_status(status: RuntimeSubagentStatus) -> bool {
    matches!(
        status,
        RuntimeSubagentStatus::Pending
            | RuntimeSubagentStatus::Running
            | RuntimeSubagentStatus::Waiting
    )
}

pub fn is_live_subagent_turn_item(item: &OrchestrationV2TurnItem) -> bool {
    item.item_type() == TurnItemType::Subagent && is_turn_item_status_active(item.status())
}

/// A workflow coordinator with its members grouped by phase, as Lineage renders it.
#[derive(Clone, Debug, PartialEq)]
pub struct SubagentWorkflowGroup {
    pub workflow: RuntimeSubagent,
    pub phases: Vec<SubagentWorkflowGroupPhase>,
    /// Members with no resolvable phase (orphans render under the workflow).
    pub unphased_members: Vec<RuntimeSubagent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseState {
    Pending,
    Running,
    /// Every member settled (success or error).
    Done,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubagentWorkflowGroupPhase {
    pub index: u64,
    pub title: String,
    pub members: Vec<RuntimeSubagent>,
    pub state: PhaseState,
    pub settled_count: usize,
}

/// The subset of `OrchestrationV2Subagent` the runtime projection reads
/// (upstream types the parameter structurally).
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectedSubagent {
    pub id: String,
    pub title: Option<String>,
    pub prompt: String,
    pub model: Option<String>,
    pub status: OrchestrationV2SubagentStatus,
    pub progress: Option<String>,
    pub child_thread_id: Option<String>,
    pub workflow: Option<OrchestrationV2SubagentWorkflow>,
    pub result: Option<String>,
    pub started_at: Option<UtcDateTime>,
    pub completed_at: Option<UtcDateTime>,
    pub updated_at: UtcDateTime,
}

impl From<&OrchestrationV2Subagent> for ProjectedSubagent {
    fn from(subagent: &OrchestrationV2Subagent) -> Self {
        Self {
            id: subagent.id.as_str().to_owned(),
            title: subagent.title.clone(),
            prompt: subagent.prompt.clone(),
            model: subagent.model.clone(),
            status: subagent.status,
            progress: subagent.progress.clone(),
            child_thread_id: subagent
                .child_thread_id
                .as_ref()
                .map(|id| id.as_str().to_owned()),
            workflow: subagent.workflow.clone(),
            result: subagent.result.clone(),
            started_at: subagent.started_at,
            completed_at: subagent.completed_at,
            updated_at: subagent.updated_at,
        }
    }
}

struct UsageSource {
    total_tokens: Option<u64>,
    tool_calls: Option<u64>,
    duration_ms: Option<u64>,
}

fn workflow_usage(source: &UsageSource) -> Option<SubagentUsage> {
    let total_tokens = source.total_tokens?;
    Some(SubagentUsage {
        total_tokens,
        tool_uses: source.tool_calls,
        duration_ms: source.duration_ms,
        ..SubagentUsage::default()
    })
}

fn iso_from_epoch_millis(value: Option<u64>) -> Option<String> {
    value.map(|value| to_iso_string(value as i64))
}

/// Expands a coordinator's workflow roster into member rows. Members are
/// synthesized from the coordinator's snapshot rather than read from their own
/// subagent entities, so a parent thread's Lineage can show them without
/// loading the coordinator's child thread; their identity is the coordinator id
/// plus the spawn ordinal.
fn workflow_members_to_runtime(
    coordinator: &RuntimeSubagent,
    workflow: &OrchestrationV2SubagentWorkflow,
) -> Vec<RuntimeSubagent> {
    workflow
        .agents
        .iter()
        .map(|agent| {
            // Every member state but queued is already a runtime status.
            let status = if matches!(
                agent.state,
                OrchestrationV2WorkflowAgentState::Queued
                    | OrchestrationV2WorkflowAgentState::Running
            ) && is_terminal_subagent_status(coordinator.status)
            {
                coordinator.status
            } else {
                match agent.state {
                    OrchestrationV2WorkflowAgentState::Queued => RuntimeSubagentStatus::Pending,
                    OrchestrationV2WorkflowAgentState::Running => RuntimeSubagentStatus::Running,
                    OrchestrationV2WorkflowAgentState::Completed => {
                        RuntimeSubagentStatus::Completed
                    }
                    OrchestrationV2WorkflowAgentState::Failed => RuntimeSubagentStatus::Failed,
                }
            };
            let failed = status == RuntimeSubagentStatus::Failed;
            let started_at = iso_from_epoch_millis(agent.started_at);
            // The provider reports a settled member's duration, not its end instant.
            let completed_at = if is_terminal_subagent_status(status) {
                match (agent.started_at, agent.duration_ms) {
                    (Some(started), Some(duration)) => {
                        iso_from_epoch_millis(Some(started + duration))
                    }
                    _ => coordinator.completed_at.clone(),
                }
            } else {
                None
            };
            let result = agent
                .result
                .as_ref()
                .map(|result| result.as_str().to_owned());
            RuntimeSubagent {
                // Members share the run's handles: the transcript directory is what
                // makes their conversation readable, and only the run knows it.
                id: format!("{}:agent:{}", coordinator.id, agent.index),
                kind: RuntimeSubagentKind::WorkflowAgent,
                title: agent.label.as_str().to_owned(),
                model: agent.model.as_ref().map(|model| model.as_str().to_owned()),
                status,
                // A member the workflow retried counts each attempt as a run.
                activation_count: agent.attempt.unwrap_or(1),
                usage: workflow_usage(&UsageSource {
                    total_tokens: agent.total_tokens,
                    tool_calls: agent.tool_calls,
                    duration_ms: agent.duration_ms,
                }),
                // A member keeps its prompt on `progress` even once settled: that is the
                // question half of its conversation, and the detail view shows both halves.
                progress: agent
                    .prompt
                    .as_ref()
                    .map(|prompt| prompt.as_str().to_owned()),
                result: if failed { None } else { result.clone() },
                error: if failed { result } else { None },
                parent_agent_id: Some(coordinator.id.clone()),
                agent_index: Some(agent.index),
                phase_index: agent.phase_index,
                phase_title: agent
                    .phase_title
                    .as_ref()
                    .map(|title| title.as_str().to_owned()),
                attempt: agent.attempt,
                phases: Vec::new(),
                child_thread_id: agent
                    .child_thread_id
                    .as_ref()
                    .map(|id| id.as_str().to_owned()),
                recent_activity: Vec::new(),
                first_seen_at: iso_from_epoch_millis(agent.queued_at)
                    .or_else(|| started_at.clone())
                    .unwrap_or_else(|| coordinator.first_seen_at.clone()),
                started_at: started_at.clone(),
                completed_at: completed_at.clone(),
                updated_at: completed_at
                    .clone()
                    .or(started_at)
                    .unwrap_or_else(|| coordinator.first_seen_at.clone()),
                ..coordinator.clone()
            }
        })
        .collect()
}

/// Projects subagents and workflow members into the runtime roster.
pub fn projected_subagents_to_runtime(subagents: &[ProjectedSubagent]) -> Vec<RuntimeSubagent> {
    subagents
        .iter()
        .flat_map(|subagent| {
            let updated_at = iso(&subagent.updated_at);
            let started_at = subagent.started_at.as_ref().map(iso);
            let first_seen_at = started_at.clone().unwrap_or_else(|| updated_at.clone());
            let workflow = subagent.workflow.as_ref();
            let run_handles = workflow
                .and_then(|workflow| workflow.run_handles.as_ref())
                .map(|handles| SubagentRunHandles {
                    run_id: handles.run_id.as_ref().map(|v| v.as_str().to_owned()),
                    script_path: handles.script_path.as_ref().map(|v| v.as_str().to_owned()),
                    transcript_dir: handles
                        .transcript_dir
                        .as_ref()
                        .map(|v| v.as_str().to_owned()),
                    session_url: None,
                });
            let coordinator = RuntimeSubagent {
                id: subagent.id.clone(),
                kind: if workflow.is_none() {
                    RuntimeSubagentKind::Subagent
                } else {
                    RuntimeSubagentKind::Workflow
                },
                title: subagent.title.clone().unwrap_or_else(|| {
                    // `prompt.length > 80 ? prompt.slice(0, 77) + "..." : prompt` counts UTF-16 units.
                    let units: Vec<u16> = subagent.prompt.encode_utf16().collect();
                    if units.len() > 80 {
                        format!("{}...", String::from_utf16_lossy(&units[..77]))
                    } else {
                        subagent.prompt.clone()
                    }
                }),
                role: None,
                model: subagent.model.clone(),
                effort: None,
                status: subagent.status.into(),
                activation_count: 1,
                usage: workflow.and_then(|workflow| {
                    workflow_usage(&UsageSource {
                        total_tokens: workflow.total_tokens,
                        tool_calls: workflow.tool_calls,
                        duration_ms: workflow.duration_ms,
                    })
                }),
                progress: subagent.progress.clone(),
                last_tool_name: None,
                result: subagent.result.clone(),
                error: if subagent.status == OrchestrationV2SubagentStatus::Failed {
                    subagent.result.clone()
                } else {
                    None
                },
                output_file: None,
                parent_agent_id: None,
                agent_index: None,
                phase_index: None,
                phase_title: None,
                attempt: None,
                workflow_name: workflow
                    .and_then(|workflow| workflow.name.as_ref())
                    .map(|name| name.as_str().to_owned()),
                phases: workflow
                    .map(|workflow| {
                        workflow
                            .phases
                            .iter()
                            .map(|phase| SubagentWorkflowPhase {
                                index: phase.index,
                                title: phase.title.as_str().to_owned(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                run_handles,
                child_thread_id: subagent.child_thread_id.clone(),
                recent_activity: Vec::new(),
                first_seen_at,
                started_at,
                completed_at: subagent.completed_at.as_ref().map(iso),
                updated_at,
            };
            match workflow {
                None => vec![coordinator],
                Some(workflow) => {
                    let members = workflow_members_to_runtime(&coordinator, workflow);
                    let mut rows = vec![coordinator];
                    rows.extend(members);
                    rows
                }
            }
        })
        .collect()
}

pub fn derive_workflow_groups(agents: &[RuntimeSubagent]) -> Vec<SubagentWorkflowGroup> {
    let mut workflows: Vec<&RuntimeSubagent> = agents
        .iter()
        .filter(|agent| agent.kind == RuntimeSubagentKind::Workflow)
        .collect();
    workflows.sort_by(|a, b| {
        a.first_seen_at
            .cmp(&b.first_seen_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut members: HashMap<&str, Vec<&RuntimeSubagent>> = HashMap::new();
    for agent in agents {
        if agent.kind != RuntimeSubagentKind::Workflow
            && let Some(parent) = agent.parent_agent_id.as_deref()
        {
            members.entry(parent).or_default().push(agent);
        }
    }

    workflows
        .into_iter()
        .map(|workflow| {
            let workflow_members: &[&RuntimeSubagent] = members
                .get(workflow.id.as_str())
                .map_or(&[], |members| members.as_slice());
            // Union, not either/or: the declared plan lags the members, because the
            // provider only admits a phase once something in it starts. Members seed
            // the map; the declared titles then overwrite whatever they guessed.
            let mut phase_titles: HashMap<u64, String> = HashMap::new();
            for member in workflow_members {
                let Some(phase_index) = member.phase_index else {
                    continue;
                };
                let title = member
                    .phase_title
                    .clone()
                    .or_else(|| phase_titles.get(&phase_index).cloned())
                    .unwrap_or_else(|| format!("Phase {}", phase_index + 1));
                phase_titles.insert(phase_index, title);
            }
            for phase in &workflow.phases {
                phase_titles.insert(phase.index, phase.title.clone());
            }
            let mut known_phases: Vec<(u64, String)> = phase_titles.into_iter().collect();
            known_phases.sort_by_key(|(index, _)| *index);

            let phases = known_phases
                .into_iter()
                .map(|(index, title)| {
                    let mut phase_members: Vec<RuntimeSubagent> = workflow_members
                        .iter()
                        .filter(|member| member.phase_index == Some(index))
                        .map(|member| (*member).clone())
                        .collect();
                    phase_members.sort_by_key(|member| member.agent_index.unwrap_or(0));
                    // Idle members count as active for phase-liveness: a resumable Codex
                    // member has not finished the phase.
                    let has_active_member = phase_members.iter().any(|member| {
                        is_active_subagent_status(member.status)
                            || member.status == RuntimeSubagentStatus::Idle
                    });
                    let settled_count = phase_members
                        .iter()
                        .filter(|member| is_terminal_subagent_status(member.status))
                        .count();
                    let state = if phase_members.is_empty() {
                        PhaseState::Pending
                    } else if has_active_member {
                        PhaseState::Running
                    } else {
                        PhaseState::Done
                    };
                    SubagentWorkflowGroupPhase {
                        index,
                        title,
                        members: phase_members,
                        state,
                        settled_count,
                    }
                })
                .collect();

            let mut unphased_members: Vec<RuntimeSubagent> = workflow_members
                .iter()
                .filter(|member| member.phase_index.is_none())
                .map(|member| (*member).clone())
                .collect();
            unphased_members.sort_by_key(|member| member.agent_index.unwrap_or(0));

            SubagentWorkflowGroup {
                workflow: workflow.clone(),
                phases,
                unphased_members,
            }
        })
        .collect()
}

#[allow(dead_code)]
fn _status_active(status: OrchestrationV2SubagentStatus) -> bool {
    is_subagent_status_active(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::{
        NonEmptyString, OrchestrationV2WorkflowAgent, OrchestrationV2WorkflowPhase,
        TrimmedNonEmptyString,
    };
    use pretty_assertions::assert_eq;

    fn at(value: &str) -> UtcDateTime {
        value.parse().unwrap()
    }

    fn subagent() -> ProjectedSubagent {
        ProjectedSubagent {
            id: "coordinator".into(),
            title: Some("probe".into()),
            prompt: "export const meta = …".into(),
            model: Some("claude-opus-5[1m]".into()),
            status: OrchestrationV2SubagentStatus::Running,
            progress: None,
            child_thread_id: None,
            workflow: None,
            result: None,
            started_at: Some(at("2026-08-01T10:00:00.000Z")),
            completed_at: None,
            updated_at: at("2026-08-01T10:00:05.000Z"),
        }
    }

    fn text(value: &str) -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(value).unwrap()
    }

    fn member(
        index: u64,
        label: &str,
        state: OrchestrationV2WorkflowAgentState,
    ) -> OrchestrationV2WorkflowAgent {
        OrchestrationV2WorkflowAgent {
            index,
            label: text(label),
            agent_id: None,
            child_thread_id: None,
            state,
            phase_index: Some(1),
            phase_title: Some(text("Alpha")),
            model: None,
            attempt: None,
            total_tokens: None,
            tool_calls: None,
            duration_ms: None,
            queued_at: None,
            started_at: None,
            prompt: None,
            result: None,
        }
    }

    fn running_member() -> OrchestrationV2WorkflowAgent {
        member(1, "alpha:one", OrchestrationV2WorkflowAgentState::Running)
    }

    fn workflow(
        phases: Vec<(u64, &str)>,
        agents: Vec<OrchestrationV2WorkflowAgent>,
    ) -> OrchestrationV2SubagentWorkflow {
        OrchestrationV2SubagentWorkflow {
            name: None,
            run_handles: None,
            phases: phases
                .into_iter()
                .map(|(index, title)| OrchestrationV2WorkflowPhase {
                    index,
                    title: text(title),
                })
                .collect(),
            agents,
            total_tokens: None,
            tool_calls: None,
            duration_ms: None,
        }
    }

    fn groups_of(subagents: &[ProjectedSubagent]) -> Vec<SubagentWorkflowGroup> {
        derive_workflow_groups(&projected_subagents_to_runtime(subagents))
    }

    #[test]
    fn leaves_a_plain_subagent_flat() {
        let rows = projected_subagents_to_runtime(&[subagent()]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, RuntimeSubagentKind::Subagent);
        assert_eq!(rows[0].phases, Vec::new());
    }

    #[test]
    fn projects_a_coordinator_plus_one_row_per_member() {
        let mut queued = member(2, "alpha:two", OrchestrationV2WorkflowAgentState::Queued);
        queued.phase_title = Some(text("Alpha"));
        let mut wf = workflow(
            vec![(1, "Alpha"), (2, "Beta")],
            vec![running_member(), queued],
        );
        wf.name = Some(text("probe-wf"));
        wf.total_tokens = Some(1000);
        let rows = projected_subagents_to_runtime(&[ProjectedSubagent {
            workflow: Some(wf),
            ..subagent()
        }]);
        assert_eq!(
            rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
            vec![
                RuntimeSubagentKind::Workflow,
                RuntimeSubagentKind::WorkflowAgent,
                RuntimeSubagentKind::WorkflowAgent
            ]
        );
        assert_eq!(rows[0].workflow_name.as_deref(), Some("probe-wf"));
        assert_eq!(rows[0].usage.as_ref().unwrap().total_tokens, 1000);
        assert_eq!(rows[1].id, "coordinator:agent:1");
        assert_eq!(rows[1].title, "alpha:one");
        assert_eq!(rows[1].parent_agent_id.as_deref(), Some("coordinator"));
        assert_eq!(rows[1].phase_index, Some(1));
        assert_eq!(rows[1].phase_title.as_deref(), Some("Alpha"));
        assert_eq!(rows[1].status, RuntimeSubagentStatus::Running);
        // A queued member is not started work: it must not report an elapsed clock.
        assert_eq!(rows[2].status, RuntimeSubagentStatus::Pending);
        assert_eq!(rows[2].started_at, None);
    }

    #[test]
    fn derives_a_settled_members_end_from_its_reported_duration() {
        let mut settled = member(1, "alpha:one", OrchestrationV2WorkflowAgentState::Completed);
        settled.started_at = Some(1_785_578_400_000); // 2026-08-01T10:00:00Z
        settled.duration_ms = Some(1500);
        settled.total_tokens = Some(71_141);
        settled.tool_calls = Some(3);
        settled.prompt = Some(NonEmptyString::new("Reply with exactly: A1").unwrap());
        settled.result = Some(NonEmptyString::new("A1").unwrap());
        let rows = projected_subagents_to_runtime(&[ProjectedSubagent {
            workflow: Some(workflow(vec![], vec![settled])),
            ..subagent()
        }]);
        let settled = &rows[1];
        assert_eq!(settled.status, RuntimeSubagentStatus::Completed);
        assert_eq!(
            settled.completed_at.as_deref(),
            Some("2026-08-01T10:00:01.500Z")
        );
        assert_eq!(settled.result.as_deref(), Some("A1"));
        // The prompt stays on progress so the detail view can show both halves.
        assert_eq!(settled.progress.as_deref(), Some("Reply with exactly: A1"));
        assert_eq!(
            settled.usage,
            Some(SubagentUsage {
                total_tokens: 71_141,
                tool_uses: Some(3),
                duration_ms: Some(1500),
                ..SubagentUsage::default()
            })
        );
    }

    #[test]
    fn settles_unfinished_members_when_the_coordinator_settles() {
        for status in [
            OrchestrationV2SubagentStatus::Completed,
            OrchestrationV2SubagentStatus::Cancelled,
            OrchestrationV2SubagentStatus::Failed,
        ] {
            let groups = groups_of(&[ProjectedSubagent {
                status,
                completed_at: Some(at("2026-08-01T10:00:09.000Z")),
                workflow: Some(workflow(
                    vec![],
                    vec![
                        running_member(),
                        member(2, "alpha:two", OrchestrationV2WorkflowAgentState::Queued),
                        member(
                            3,
                            "alpha:three",
                            OrchestrationV2WorkflowAgentState::Completed,
                        ),
                    ],
                )),
                ..subagent()
            }]);
            // The settled run keeps its members reachable under the coordinator.
            let expected: RuntimeSubagentStatus = status.into();
            assert_eq!(groups[0].workflow.status, expected);
            let members: Vec<&RuntimeSubagent> = groups[0]
                .phases
                .iter()
                .flat_map(|phase| &phase.members)
                .collect();
            assert_eq!(
                members.iter().map(|agent| agent.status).collect::<Vec<_>>(),
                vec![expected, expected, RuntimeSubagentStatus::Completed]
            );
            assert_eq!(
                members[0].completed_at.as_deref(),
                Some("2026-08-01T10:00:09.000Z")
            );
        }
    }

    #[test]
    fn surfaces_a_retried_member_through_the_activation_count() {
        let mut retried = running_member();
        retried.attempt = Some(3);
        let rows = projected_subagents_to_runtime(&[ProjectedSubagent {
            workflow: Some(workflow(vec![], vec![retried])),
            ..subagent()
        }]);
        assert_eq!(rows[1].activation_count, 3);
        assert_eq!(rows[1].attempt, Some(3));
    }

    #[test]
    fn groups_members_under_their_phases_and_counts_the_run() {
        let mut beta_one = member(3, "beta:one", OrchestrationV2WorkflowAgentState::Running);
        beta_one.phase_index = Some(2);
        beta_one.phase_title = Some(text("Beta"));
        let mut beta_two = member(4, "beta:two", OrchestrationV2WorkflowAgentState::Queued);
        beta_two.phase_index = Some(2);
        beta_two.phase_title = Some(text("Beta"));
        let groups = groups_of(&[ProjectedSubagent {
            workflow: Some(workflow(
                vec![(1, "Alpha"), (2, "Beta")],
                vec![
                    member(1, "alpha:one", OrchestrationV2WorkflowAgentState::Completed),
                    member(2, "alpha:two", OrchestrationV2WorkflowAgentState::Failed),
                    beta_one,
                    beta_two,
                ],
            )),
            ..subagent()
        }]);
        let group = &groups[0];
        assert_eq!(
            group
                .phases
                .iter()
                .map(|phase| (phase.title.as_str(), phase.state))
                .collect::<Vec<_>>(),
            vec![("Alpha", PhaseState::Done), ("Beta", PhaseState::Running)]
        );
        assert_eq!(
            group
                .phases
                .iter()
                .map(|phase| phase.settled_count)
                .collect::<Vec<_>>(),
            vec![2, 0]
        );
        assert_eq!(
            group
                .phases
                .iter()
                .map(|phase| phase.members.len())
                .sum::<usize>(),
            4
        );
        assert_eq!(group.unphased_members, Vec::new());
    }

    #[test]
    fn shows_a_phase_the_script_only_reached_at_runtime() {
        let mut late = member(
            2,
            "dynamic:late",
            OrchestrationV2WorkflowAgentState::Running,
        );
        late.phase_index = Some(3);
        late.phase_title = Some(text("Gamma"));
        let groups = groups_of(&[ProjectedSubagent {
            workflow: Some(workflow(
                // The declared plan has not caught up with the member's phase yet.
                vec![(1, "Alpha")],
                vec![
                    member(1, "alpha:one", OrchestrationV2WorkflowAgentState::Completed),
                    late,
                ],
            )),
            ..subagent()
        }]);
        assert_eq!(
            groups[0]
                .phases
                .iter()
                .map(|phase| phase.title.as_str())
                .collect::<Vec<_>>(),
            vec!["Alpha", "Gamma"]
        );
        assert_eq!(groups[0].unphased_members, Vec::new());
    }

    #[test]
    fn keeps_a_reported_phase_title_whichever_member_comes_first() {
        for missing_first in [false, true] {
            let mut agents = vec![running_member(), running_member()];
            agents[0].phase_title = Some(text("Review"));
            agents[1].index = 2;
            agents[1].phase_title = None;
            if missing_first {
                agents.reverse();
            }
            let groups = groups_of(&[ProjectedSubagent {
                workflow: Some(workflow(vec![], agents)),
                ..subagent()
            }]);
            assert_eq!(groups[0].phases[0].title, "Review");
        }
    }

    #[test]
    fn keeps_unphased_members_with_their_workflow_and_excludes_unrelated_agents() {
        let mut unphased = running_member();
        unphased.phase_index = None;
        unphased.phase_title = None;
        let runtime = projected_subagents_to_runtime(&[
            ProjectedSubagent {
                id: "ordinary".into(),
                ..subagent()
            },
            ProjectedSubagent {
                id: "__proto__".into(),
                workflow: Some(workflow(vec![], vec![unphased])),
                ..subagent()
            },
            ProjectedSubagent {
                id: "constructor".into(),
                workflow: Some(workflow(vec![], vec![])),
                ..subagent()
            },
            ProjectedSubagent {
                id: "orphan".into(),
                workflow: Some(workflow(vec![], vec![running_member()])),
                ..subagent()
            },
        ]);
        let filtered: Vec<RuntimeSubagent> = runtime
            .into_iter()
            .filter(|agent| agent.id != "orphan")
            .collect();
        let groups = derive_workflow_groups(&filtered);
        assert_eq!(
            groups
                .iter()
                .map(|group| group.workflow.id.as_str())
                .collect::<Vec<_>>(),
            vec!["__proto__", "constructor"]
        );
        assert_eq!(groups[0].phases, Vec::new());
        assert_eq!(
            groups[0]
                .unphased_members
                .iter()
                .map(|agent| agent.title.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha:one"]
        );
        assert_eq!(groups[1].phases, Vec::new());
        assert_eq!(groups[1].unphased_members, Vec::new());
    }
}
