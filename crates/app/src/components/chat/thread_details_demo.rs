//! Demo fixtures for thread details UI (`TURIN_CHAT_DEMO=thread-details`).

use std::sync::Arc;

use crate::state::contracts::{OrchestrationV2ThreadShell, ThreadId};
use crate::state::subagent_runtime::{
    ProjectedSubagent, RuntimeSubagent, SubagentWorkflowGroup, derive_workflow_groups,
    projected_subagents_to_runtime,
};
use crate::state::thread_relationships::{
    ThreadRelationshipEdge, ThreadRelationshipGraph, ThreadRelationshipKind,
    ThreadRelationshipWalkRow, derive_thread_relationship_graph, immediate_thread_relationships,
    order_web_thread_lineage_rows,
};
use serde_json::json;

#[derive(Clone, Debug)]
pub struct LineagePanelSnapshot {
    pub current_thread_id: ThreadId,
    pub graph: ThreadRelationshipGraph,
    pub rows: Vec<ThreadRelationshipWalkRow>,
    pub workflow_groups: Vec<SubagentWorkflowGroup>,
    pub subagents_by_child_thread: std::collections::HashMap<String, RuntimeSubagent>,
}

pub fn parse_thread_details_demo() -> Option<LineagePanelSnapshot> {
    std::env::var("TURIN_CHAT_DEMO")
        .ok()
        .filter(|demo| demo.contains("thread-details"))
        .map(|_| lineage_demo_snapshot())
}

fn id(value: &str) -> ThreadId {
    ThreadId::new(value).unwrap()
}

fn lineage_demo_snapshot() -> LineagePanelSnapshot {
    let current = id("thread-current");
    let shells = demo_shells();
    let graph = derive_thread_relationship_graph(&shells, None);
    let rows = order_web_thread_lineage_rows(
        &graph,
        &immediate_thread_relationships(&graph, &current),
        &current,
        None,
    );
    let runtime = projected_subagents_to_runtime(&demo_subagents());
    let workflow_groups = derive_workflow_groups(&runtime);
    let subagents_by_child_thread = runtime
        .into_iter()
        .filter_map(|agent| {
            agent
                .child_thread_id
                .clone()
                .map(|thread_id| (thread_id, agent))
        })
        .collect();
    LineagePanelSnapshot {
        current_thread_id: current,
        graph,
        rows,
        workflow_groups,
        subagents_by_child_thread,
    }
}

fn demo_shell(thread_id: &str, patch: serde_json::Value) -> Arc<OrchestrationV2ThreadShell> {
    let mut value = json!({
        "id": thread_id,
        "projectId": "project-v2",
        "title": thread_id,
        "providerInstanceId": "codex",
        "modelSelection": { "instanceId": "codex", "model": "gpt-5.4" },
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "branch": null,
        "worktreePath": null,
        "activeProviderThreadId": null,
        "lineage": {
            "rootThreadId": thread_id,
            "parentThreadId": null,
            "relationshipToParent": null
        },
        "forkedFrom": null,
        "createdBy": "user",
        "creationSource": "web",
        "createdAt": "2026-06-01T00:00:00.000Z",
        "updatedAt": "2026-06-01T00:00:00.000Z",
        "archivedAt": null,
        "settledOverride": null,
        "settledAt": null,
        "lastVisitedAt": null,
        "deletedAt": null,
        "latestRunId": null,
        "activeRunId": null,
        "status": "idle",
        "pendingRuntimeRequest": null,
        "latestVisibleMessage": null,
        "latestUserMessageAt": null,
        "hasActionableProposedPlan": false,
        "itemCount": 0,
        "visibleItemCount": 0,
    });
    for (key, field) in patch.as_object().unwrap() {
        value[key] = field.clone();
    }
    Arc::new(serde_json::from_value(value).expect("demo shell decodes"))
}

fn demo_shells() -> Vec<Arc<OrchestrationV2ThreadShell>> {
    vec![
        demo_shell(
            "thread-current",
            json!({ "title": "Checkout review", "createdAt": "2026-06-01T00:00:00.000Z" }),
        ),
        demo_shell(
            "thread-fork-a",
            json!({
                "title": "Try alt approach",
                "createdAt": "2026-06-02T00:00:00.000Z",
                "forkedFrom": { "type": "run", "threadId": "thread-current", "runId": "run-a" },
                "lineage": {
                    "rootThreadId": "thread-current",
                    "parentThreadId": "thread-current",
                    "relationshipToParent": "fork"
                }
            }),
        ),
        demo_shell(
            "thread-subagent",
            json!({
                "title": "Subagent: /root/review_agent",
                "status": "running",
                "createdAt": "2026-06-03T00:00:00.000Z",
                "lineage": {
                    "rootThreadId": "thread-current",
                    "parentThreadId": "thread-current",
                    "relationshipToParent": "subagent"
                }
            }),
        ),
        demo_shell(
            "workflow-chat",
            json!({
                "title": "Workflow coordinator",
                "status": "running",
                "createdAt": "2026-06-04T00:00:00.000Z",
                "lineage": {
                    "rootThreadId": "thread-current",
                    "parentThreadId": "thread-current",
                    "relationshipToParent": "subagent"
                }
            }),
        ),
    ]
}

fn demo_subagents() -> Vec<ProjectedSubagent> {
    use crate::state::contracts::{
        OrchestrationV2SubagentStatus, OrchestrationV2SubagentWorkflow,
        OrchestrationV2WorkflowAgent, OrchestrationV2WorkflowAgentState,
        OrchestrationV2WorkflowPhase, TrimmedNonEmptyString, UtcDateTime,
    };
    let text = |s: &str| TrimmedNonEmptyString::new(s).unwrap();
    let at = |s: &str| -> UtcDateTime { s.parse().unwrap() };
    let workflow = OrchestrationV2SubagentWorkflow {
        name: Some(text("Checkout review")),
        run_handles: None,
        phases: vec![
            OrchestrationV2WorkflowPhase {
                index: 0,
                title: text("Inspect"),
            },
            OrchestrationV2WorkflowPhase {
                index: 1,
                title: text("Fix"),
            },
        ],
        agents: vec![
            OrchestrationV2WorkflowAgent {
                index: 0,
                label: text("Checker"),
                agent_id: None,
                child_thread_id: Some(ThreadId::new("member-a").unwrap()),
                state: OrchestrationV2WorkflowAgentState::Running,
                phase_index: Some(0),
                phase_title: Some(text("Inspect")),
                model: None,
                attempt: None,
                total_tokens: None,
                tool_calls: None,
                duration_ms: None,
                queued_at: None,
                started_at: None,
                prompt: None,
                result: None,
            },
            OrchestrationV2WorkflowAgent {
                index: 1,
                label: text("Writer"),
                agent_id: None,
                child_thread_id: Some(ThreadId::new("member-b").unwrap()),
                state: OrchestrationV2WorkflowAgentState::Completed,
                phase_index: Some(0),
                phase_title: Some(text("Inspect")),
                model: None,
                attempt: None,
                total_tokens: None,
                tool_calls: None,
                duration_ms: None,
                queued_at: None,
                started_at: None,
                prompt: None,
                result: None,
            },
        ],
        total_tokens: None,
        tool_calls: None,
        duration_ms: None,
    };
    vec![
        ProjectedSubagent {
            id: "workflow-coordinator".into(),
            title: Some("Checkout review".into()),
            prompt: "Review checkout flow".into(),
            model: Some("composer".into()),
            status: OrchestrationV2SubagentStatus::Running,
            progress: None,
            child_thread_id: Some("workflow-chat".into()),
            workflow: Some(workflow),
            result: None,
            started_at: Some(at("2026-06-04T10:00:00.000Z")),
            completed_at: None,
            updated_at: at("2026-06-04T10:00:05.000Z"),
        },
        ProjectedSubagent {
            id: "plain-subagent".into(),
            title: Some("Research docs".into()),
            prompt: "Find API docs".into(),
            model: Some("composer".into()),
            status: OrchestrationV2SubagentStatus::Running,
            progress: Some("Searching…".into()),
            child_thread_id: Some("thread-subagent".into()),
            workflow: None,
            result: None,
            started_at: Some(at("2026-06-03T10:00:00.000Z")),
            completed_at: None,
            updated_at: at("2026-06-03T10:00:05.000Z"),
        },
    ]
}

// Re-export graph edge kind for panel rendering tests.
#[allow(dead_code)]
pub fn example_edge() -> ThreadRelationshipEdge {
    ThreadRelationshipEdge {
        source_thread_id: id("thread-current"),
        target_thread_id: id("thread-fork-a"),
        kind: ThreadRelationshipKind::Fork,
        status: Some("completed".into()),
    }
}
