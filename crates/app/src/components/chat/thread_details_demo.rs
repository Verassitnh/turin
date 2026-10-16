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
    /// The provider driver kind behind subagent glyphs and tooltips.
    pub driver: Option<String>,
    /// `resolveMergeBackTargetThreadId(projection)`.
    pub merge_target_thread_id: Option<ThreadId>,
    /// `latestMergeBackRun !== null`.
    pub has_merge_back_run: bool,
    /// `canDetachThreadProviderSession(projection)`.
    pub can_detach: bool,
    /// `orchestrationEnvironment.stopWorkflow.permissionAtom`.
    pub can_operate_workflow: bool,
    /// Agents `canStop` holds for (app-owned, or a strong provider-native task).
    pub stoppable_agent_ids: std::collections::HashSet<String>,
    /// Subagents without a child thread yet have no row, so they count apart.
    pub running_without_thread: usize,
}

/// `TURIN_CHAT_DEMO=thread-details,lineage-expanded` opens the workflow tree
/// and the previous-agents group, for screenshots.
pub fn lineage_demo_expanded() -> bool {
    std::env::var("TURIN_CHAT_DEMO").is_ok_and(|demo| demo.contains("lineage-expanded"))
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

pub(crate) fn lineage_demo_snapshot() -> LineagePanelSnapshot {
    let current = id("thread-current");
    let merge_target = id("thread-parent");
    let shells = demo_shells();
    let graph = derive_thread_relationship_graph(&shells, None);
    let rows = order_web_thread_lineage_rows(
        &graph,
        &immediate_thread_relationships(&graph, &current),
        &current,
        Some(&merge_target),
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
        driver: Some("codex".to_owned()),
        merge_target_thread_id: Some(merge_target),
        has_merge_back_run: true,
        can_detach: true,
        can_operate_workflow: true,
        stoppable_agent_ids: [
            "plain-subagent".to_owned(),
            "workflow-coordinator".to_owned(),
        ]
        .into_iter()
        .collect(),
        running_without_thread: 0,
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
            "thread-parent",
            json!({ "title": "Checkout redesign", "createdAt": "2026-05-30T00:00:00.000Z" }),
        ),
        demo_shell(
            "thread-current",
            json!({
                "title": "Checkout review",
                "createdAt": "2026-06-01T00:00:00.000Z",
                "forkedFrom": { "type": "run", "threadId": "thread-parent", "runId": "run-p" },
                "lineage": {
                    "rootThreadId": "thread-parent",
                    "parentThreadId": "thread-parent",
                    "relationshipToParent": "fork"
                }
            }),
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
            "thread-prev-a",
            json!({
                "title": "Subagent: /root/lint_agent",
                "status": "completed",
                "createdAt": "2026-05-31T00:00:00.000Z",
                "lineage": {
                    "rootThreadId": "thread-current",
                    "parentThreadId": "thread-current",
                    "relationshipToParent": "subagent"
                }
            }),
        ),
        demo_shell(
            "thread-prev-b",
            json!({
                "title": "Subagent: /root/test_agent",
                "status": "failed",
                "createdAt": "2026-05-31T01:00:00.000Z",
                "lineage": {
                    "rootThreadId": "thread-current",
                    "parentThreadId": "thread-current",
                    "relationshipToParent": "subagent"
                }
            }),
        ),
        demo_shell(
            "member-a",
            json!({ "title": "Checker", "status": "running",
                "lineage": { "rootThreadId": "thread-current", "parentThreadId": "workflow-chat",
                    "relationshipToParent": "subagent" } }),
        ),
        demo_shell(
            "member-b",
            json!({ "title": "Writer", "status": "completed",
                "lineage": { "rootThreadId": "thread-current", "parentThreadId": "workflow-chat",
                    "relationshipToParent": "subagent" } }),
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
    // Relative to now, so the elapsed labels read like a live thread.
    let at = |minutes_ago: i64| -> UtcDateTime {
        (chrono::Utc::now() - chrono::Duration::minutes(minutes_ago))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            .parse()
            .unwrap()
    };
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
                started_at: Some((chrono::Utc::now().timestamp_millis() - 150_000) as u64),
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
                duration_ms: Some(95_000),
                queued_at: None,
                started_at: Some((chrono::Utc::now().timestamp_millis() - 200_000) as u64),
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
            started_at: Some(at(4)),
            completed_at: None,
            updated_at: at(0),
        },
        ProjectedSubagent {
            id: "prev-a".into(),
            title: Some("Lint".into()),
            prompt: "Run lint".into(),
            model: Some("composer".into()),
            status: OrchestrationV2SubagentStatus::Completed,
            progress: None,
            child_thread_id: Some("thread-prev-a".into()),
            workflow: None,
            result: Some("No lint errors".into()),
            started_at: Some(at(95)),
            completed_at: Some(at(92)),
            updated_at: at(92),
        },
        ProjectedSubagent {
            id: "prev-b".into(),
            title: Some("Tests".into()),
            prompt: "Run tests".into(),
            model: Some("composer".into()),
            status: OrchestrationV2SubagentStatus::Failed,
            progress: None,
            child_thread_id: Some("thread-prev-b".into()),
            workflow: None,
            result: None,
            started_at: Some(at(70)),
            completed_at: Some(at(61)),
            updated_at: at(61),
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
            started_at: Some(at(12)),
            completed_at: None,
            updated_at: at(0),
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

/// `TURIN_CHAT_DEMO=thread-details,pr-rows`: the panel's pull request rows with a host detail in
/// each state the action slot ranks.
pub fn pr_rows_demo(
    window: &mut gpui_kit::Window,
    cx: &mut gpui_kit::App,
) -> Option<gpui_kit::AnyElement> {
    use std::rc::Rc;

    use gpui_kit::{IntoElement as _, ParentElement as _, Styled as _, div};
    use turin_contracts::thread_pull_request::ThreadPullRequestLink;

    use super::thread_details_pr_row::{
        PrRowActions, ThreadDetailsPrRow, demo_pull_request_detail,
    };
    use super::thread_details_pr_rows::{ThreadDetailsPrRows, thread_details_pr_link_row};
    use super::thread_details_section::ThreadDetailsSection;

    if !std::env::var("TURIN_CHAT_DEMO").is_ok_and(|demo| demo.contains("pr-rows")) {
        return None;
    }
    let link = |number: u64, head: &str, base: &str, updated_at: &str, watched: bool| {
        let mut link: ThreadPullRequestLink = serde_json::from_value(json!({
            "host": "github.com",
            "repository": "acme/web",
            "number": number,
            "url": format!("https://github.com/acme/web/pull/{number}"),
            "source": "manual",
            "linkedAt": updated_at,
            "snapshot": {
                "state": "open",
                "title": format!("Change {number}"),
                "headBranch": head,
                "baseBranch": base,
                "isDraft": false,
                "updatedAt": updated_at,
                "syncedAt": updated_at,
            },
            "stack": null,
        }))
        .expect("demo link decodes");
        if watched {
            link.watch = serde_json::from_value(json!({
                "startedAt": updated_at, "headSha": null, "failedChecks": [], "passed": false,
                "passedChecks": [], "remarksThrough": updated_at, "remarkIds": [],
                "conflicting": false, "wakes": 0,
            }))
            .ok();
        }
        link
    };
    let check = |name: &str, status: &str| json!({ "name": name, "status": status, "description": null, "url": null });
    let noop: super::thread_details_pr_row::OpenHandler = Rc::new(|_, _, _| {});
    let row = |id: &str, number: u64, detail: serde_json::Value| {
        let detail = demo_pull_request_detail(serde_json::Value::as_object(&detail).map_or(
            json!({ "number": number }),
            |_| {
                let mut detail = detail.clone();
                detail["number"] = json!(number);
                detail
            },
        ));
        ThreadDetailsPrRow::new(id.to_owned(), number, format!("#{number}"), noop.clone())
            .detail(Some(detail))
            .actions(PrRowActions::default())
            .into_any_element()
    };
    let current = link(
        12,
        "layer-two",
        "layer-one",
        "2026-10-15T10:00:20.000Z",
        false,
    );
    let links = vec![
        link(11, "layer-one", "main", "2026-10-15T10:00:10.000Z", true),
        current.clone(),
        link(3, "unrelated", "main", "2026-10-15T10:00:05.000Z", false),
    ];
    let merge_ready = demo_pull_request_detail(json!({
        "number": 12,
        "title": "Cache the checkout title",
        "checks": [check("ci", "success"), check("lint", "success")],
    }));
    let current_row =
        thread_details_pr_link_row("demo-current", &current, noop.clone(), Some(merge_ready))
            .actions(PrRowActions::default());
    let rows = ThreadDetailsPrRows {
        id: "demo-pr-rows".into(),
        links,
        current_link: Some(current),
        current_row,
        on_open_link: Rc::new(|_, _, _, _| {}),
        stop_watching: Some(Rc::new(|_, _, _| {})),
        link_detail: None,
    };
    let _ = (window, cx);
    Some(
        ThreadDetailsSection::new("Pull requests")
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(rows)
                    .child(row(
                        "demo-fix",
                        21,
                        json!({
                            "title": "Fix the flaky cart test",
                            "checks": [check("ci", "failure"), check("lint", "success")],
                        }),
                    ))
                    .child(row(
                        "demo-running",
                        22,
                        json!({
                            "title": "Bump the checkout SDK",
                            "checks": [check("ci", "pending"), check("lint", "success")],
                        }),
                    ))
                    .child(row(
                        "demo-resolve",
                        23,
                        json!({
                            "title": "Rework the tax rules",
                            "mergeability": "conflicting",
                            "checks": [check("ci", "success")],
                        }),
                    ))
                    .child(row(
                        "demo-draft",
                        24,
                        json!({ "title": "Spike: order history", "isDraft": true }),
                    )),
            )
            .into_any_element(),
    )
}
