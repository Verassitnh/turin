//! Port of upstream/packages/client-runtime/src/state/threadRelationships.ts

use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;
use serde::Serialize;

use crate::state::contracts::{
    OrchestrationV2ForkSource, OrchestrationV2LineageRelationship, OrchestrationV2ThreadShell,
    ThreadId,
};
use crate::state::thread_projection::ThreadProjection;
use crate::state::time::epoch_millis;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadRelationshipKind {
    Parent,
    Fork,
    Subagent,
    Transfer,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreadRelationshipNode {
    pub thread_id: ThreadId,
    pub thread: Option<Arc<OrchestrationV2ThreadShell>>,
    pub missing: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadRelationshipEdge {
    pub source_thread_id: ThreadId,
    pub target_thread_id: ThreadId,
    pub kind: ThreadRelationshipKind,
    /// The wire spelling of the status (`running`, `rolled_back`, …).
    pub status: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThreadRelationshipGraph {
    pub nodes: IndexMap<ThreadId, ThreadRelationshipNode>,
    pub edges: Vec<ThreadRelationshipEdge>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadRelationshipWalkRow {
    pub thread_id: ThreadId,
    pub from_thread_id: ThreadId,
    pub depth: usize,
    pub edge: ThreadRelationshipEdge,
}

/// The wire spelling of a unit enum.
fn wire_name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => String::new(),
    }
}

pub fn resolve_merge_back_target_thread_id(
    projection: Option<&ThreadProjection>,
) -> Option<ThreadId> {
    let thread = &projection?.thread;
    if thread.lineage.relationship_to_parent != Some(OrchestrationV2LineageRelationship::Fork) {
        return None;
    }
    match &thread.forked_from {
        Some(OrchestrationV2ForkSource::Run { thread_id, .. }) => Some(thread_id.clone()),
        _ => thread.lineage.parent_thread_id.clone(),
    }
}

fn edge_key(edge: &ThreadRelationshipEdge) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{:?}",
        edge.source_thread_id, edge.target_thread_id, edge.kind
    )
}

fn shell_status(thread: &OrchestrationV2ThreadShell) -> String {
    thread
        .activity_run_status
        .flatten()
        .map_or_else(|| wire_name(&thread.status), |status| wire_name(&status))
}

pub fn derive_thread_relationship_graph(
    threads: &[Arc<OrchestrationV2ThreadShell>],
    projection: Option<&ThreadProjection>,
) -> ThreadRelationshipGraph {
    let mut threads_by_id: IndexMap<ThreadId, Arc<OrchestrationV2ThreadShell>> = IndexMap::new();
    for thread in threads {
        // Callers order shells from most to least authoritative. In particular,
        // live shells precede archived snapshots, which may still contain a stale
        // copy during archive refresh.
        threads_by_id
            .entry(thread.id.clone())
            .or_insert_with(|| Arc::clone(thread));
    }
    let mut nodes: IndexMap<ThreadId, ThreadRelationshipNode> = threads_by_id
        .values()
        .map(|thread| {
            (
                thread.id.clone(),
                ThreadRelationshipNode {
                    thread_id: thread.id.clone(),
                    thread: Some(Arc::clone(thread)),
                    missing: false,
                },
            )
        })
        .collect();
    let mut edges_by_key: IndexMap<String, ThreadRelationshipEdge> = IndexMap::new();

    fn ensure_node(nodes: &mut IndexMap<ThreadId, ThreadRelationshipNode>, thread_id: &ThreadId) {
        nodes
            .entry(thread_id.clone())
            .or_insert_with(|| ThreadRelationshipNode {
                thread_id: thread_id.clone(),
                thread: None,
                missing: true,
            });
    }
    let mut add_edge = |nodes: &mut IndexMap<ThreadId, ThreadRelationshipNode>,
                        edge: ThreadRelationshipEdge| {
        ensure_node(nodes, &edge.source_thread_id);
        ensure_node(nodes, &edge.target_thread_id);
        let edge = if edge.kind == ThreadRelationshipKind::Subagent
            && edge.status.as_deref() == Some("rolled_back")
        {
            ThreadRelationshipEdge {
                status: Some("interrupted".to_owned()),
                ..edge
            }
        } else {
            edge
        };
        edges_by_key.insert(edge_key(&edge), edge);
    };

    for thread in threads_by_id.values() {
        let parent_thread_id = match &thread.forked_from {
            Some(OrchestrationV2ForkSource::Run { thread_id, .. }) => Some(thread_id.clone()),
            _ => thread.lineage.parent_thread_id.clone(),
        };
        let Some(parent_thread_id) = parent_thread_id else {
            continue;
        };
        add_edge(
            &mut nodes,
            ThreadRelationshipEdge {
                source_thread_id: parent_thread_id,
                target_thread_id: thread.id.clone(),
                kind: if thread.lineage.relationship_to_parent
                    == Some(OrchestrationV2LineageRelationship::Subagent)
                {
                    ThreadRelationshipKind::Subagent
                } else {
                    ThreadRelationshipKind::Fork
                },
                status: Some(shell_status(thread)),
            },
        );
    }

    if let Some(projection) = projection {
        let owner_thread_id = projection.thread.id.clone();
        for subagent in projection.subagents.iter() {
            let Some(child_thread_id) = &subagent.child_thread_id else {
                continue;
            };
            let child = threads_by_id.get(child_thread_id);
            // The row describes the child conversation, including queued follow-ups
            // and their outcomes. A child without an app run still uses its task.
            let status = match child {
                Some(child) if child.activity_run_status.flatten().is_some() => shell_status(child),
                Some(child) if child.latest_run_id.is_some() => wire_name(&child.status),
                _ => wire_name(&subagent.status),
            };
            add_edge(
                &mut nodes,
                ThreadRelationshipEdge {
                    source_thread_id: owner_thread_id.clone(),
                    target_thread_id: child_thread_id.clone(),
                    kind: ThreadRelationshipKind::Subagent,
                    status: Some(status),
                },
            );
        }
        for transfer in projection.context_transfers.iter() {
            if transfer.source_thread_id == transfer.target_thread_id {
                continue;
            }
            add_edge(
                &mut nodes,
                ThreadRelationshipEdge {
                    source_thread_id: transfer.source_thread_id.clone(),
                    target_thread_id: transfer.target_thread_id.clone(),
                    kind: ThreadRelationshipKind::Transfer,
                    status: Some(wire_name(&transfer.status)),
                },
            );
        }
    }

    ThreadRelationshipGraph {
        nodes,
        edges: edges_by_key.into_values().collect(),
    }
}

fn related_id<'a>(edge: &'a ThreadRelationshipEdge, thread_id: &ThreadId) -> Option<&'a ThreadId> {
    if edge.source_thread_id == *thread_id {
        Some(&edge.target_thread_id)
    } else if edge.target_thread_id == *thread_id {
        Some(&edge.source_thread_id)
    } else {
        None
    }
}

pub fn related_thread_ids(graph: &ThreadRelationshipGraph, thread_id: &ThreadId) -> Vec<ThreadId> {
    let mut ids: IndexMap<ThreadId, ()> = IndexMap::new();
    for edge in &graph.edges {
        if edge.source_thread_id == *thread_id {
            ids.insert(edge.target_thread_id.clone(), ());
        }
        if edge.target_thread_id == *thread_id {
            ids.insert(edge.source_thread_id.clone(), ());
        }
    }
    ids.into_keys().collect()
}

pub fn walk_thread_relationships(
    graph: &ThreadRelationshipGraph,
    thread_id: &ThreadId,
) -> Vec<ThreadRelationshipWalkRow> {
    let mut visited: HashSet<ThreadId> = HashSet::from([thread_id.clone()]);
    let mut pending: Vec<(ThreadId, usize)> = vec![(thread_id.clone(), 0)];
    let mut rows = Vec::new();

    let mut index = 0;
    while index < pending.len() {
        let (current_id, current_depth) = pending[index].clone();
        index += 1;
        for edge in &graph.edges {
            let Some(related) = related_id(edge, &current_id) else {
                continue;
            };
            if visited.contains(related) {
                continue;
            }
            visited.insert(related.clone());
            let depth = current_depth + 1;
            rows.push(ThreadRelationshipWalkRow {
                thread_id: related.clone(),
                from_thread_id: current_id.clone(),
                depth,
                edge: edge.clone(),
            });
            pending.push((related.clone(), depth));
        }
    }
    rows
}

pub fn immediate_thread_relationships(
    graph: &ThreadRelationshipGraph,
    thread_id: &ThreadId,
) -> Vec<ThreadRelationshipWalkRow> {
    let mut visited: HashSet<ThreadId> = HashSet::new();
    let mut rows = Vec::new();
    for edge in &graph.edges {
        let Some(related) = related_id(edge, thread_id) else {
            continue;
        };
        if !visited.insert(related.clone()) {
            continue;
        }
        rows.push(ThreadRelationshipWalkRow {
            thread_id: related.clone(),
            from_thread_id: thread_id.clone(),
            depth: 1,
            edge: edge.clone(),
        });
    }
    rows
}

/// True when `edge` reaches `current_thread_id` from its parent or owning agent.
pub fn is_parent_thread_relationship(
    edge: &ThreadRelationshipEdge,
    current_thread_id: &ThreadId,
) -> bool {
    edge.kind != ThreadRelationshipKind::Transfer && edge.target_thread_id == *current_thread_id
}

/// An incoming parent row shows its own activity, not the child's edge status.
pub fn thread_relationship_row_status(
    graph: &ThreadRelationshipGraph,
    row: &ThreadRelationshipWalkRow,
) -> Option<String> {
    if row.edge.kind == ThreadRelationshipKind::Transfer
        || row.thread_id == row.edge.target_thread_id
    {
        return row.edge.status.clone();
    }
    let thread = graph.nodes.get(&row.thread_id)?.thread.as_ref()?;
    Some(shell_status(thread))
}

fn thread_created_at_millis(node: Option<&ThreadRelationshipNode>) -> Option<i64> {
    // A related thread we have no shell for has no creation time.
    node.and_then(|node| node.thread.as_ref())
        .map(|thread| epoch_millis(&thread.created_at))
}

/// Orders the web thread-details Lineage rows for display.
///
/// Web-specific by design: the panel pins the parent row first and a distinct
/// merge-back target second so their actions stay where the user expects, and
/// only then falls back to newest-created-first. Mobile does not share that
/// exception, so this is not the canonical relationship order and should not be
/// reused as one.
///
/// Ordering below the pins is `createdAt` descending, which is immutable, so
/// rows never move when messages or status changes arrive on a related thread.
/// Threads whose shell is missing sink to the bottom. Ties break by thread id
/// ascending so the order is total.
pub fn order_web_thread_lineage_rows(
    graph: &ThreadRelationshipGraph,
    rows: &[ThreadRelationshipWalkRow],
    current_thread_id: &ThreadId,
    merge_target_thread_id: Option<&ThreadId>,
) -> Vec<ThreadRelationshipWalkRow> {
    let pin_rank = |row: &ThreadRelationshipWalkRow| -> u8 {
        if is_parent_thread_relationship(&row.edge, current_thread_id) {
            0
        } else if Some(&row.thread_id) == merge_target_thread_id {
            1
        } else {
            2
        }
    };
    let mut ordered = rows.to_vec();
    ordered.sort_by(|left, right| {
        pin_rank(left).cmp(&pin_rank(right)).then_with(|| {
            let left_created = thread_created_at_millis(graph.nodes.get(&left.thread_id));
            let right_created = thread_created_at_millis(graph.nodes.get(&right.thread_id));
            match (left_created, right_created) {
                (Some(left), Some(right)) if left != right => right.cmp(&left),
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                _ => left.thread_id.as_str().cmp(right.thread_id.as_str()),
            }
        })
    });
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::OrchestrationV2ContextTransfer;
    use crate::state::test_fixtures::{from_json, v2_projection, v2_thread_shell};
    use serde_json::{Value, json};

    fn id(value: &str) -> ThreadId {
        ThreadId::new(value).unwrap()
    }

    fn shell(thread_id: &str, patch: Value) -> Arc<OrchestrationV2ThreadShell> {
        let mut value = serde_json::to_value(v2_thread_shell()).unwrap();
        value["id"] = json!(thread_id);
        value["title"] = json!(thread_id);
        value["lineage"] = json!({ "rootThreadId": thread_id, "parentThreadId": null, "relationshipToParent": null });
        for (key, field) in patch.as_object().unwrap() {
            value[key] = field.clone();
        }
        Arc::new(from_json(value))
    }

    fn lineage(root: &str, parent: &str, relationship: &str) -> Value {
        json!({ "rootThreadId": root, "parentThreadId": parent, "relationshipToParent": relationship })
    }

    fn forked_from(parent: &str, run: &str) -> Value {
        json!({ "type": "run", "threadId": parent, "runId": run })
    }

    fn projection_with(
        owner: &str,
        subagents: Vec<Value>,
        transfers: Vec<Value>,
    ) -> ThreadProjection {
        let mut contract = v2_projection();
        contract.thread.id = id(owner);
        contract.subagents = subagents.into_iter().map(from_json).collect();
        contract.context_transfers = transfers
            .into_iter()
            .map(from_json::<OrchestrationV2ContextTransfer>)
            .collect();
        contract.into()
    }

    fn subagent(child: &str, status: &str) -> Value {
        json!({
            "id": format!("sub-{child}"), "threadId": "thread-v2", "runId": null, "parentNodeId": "n",
            "origin": "app_owned", "createdBy": "agent", "driver": "codex", "providerInstanceId": "codex",
            "providerThreadId": null, "childThreadId": child, "nativeTaskRef": null, "prompt": "p",
            "title": null, "model": null, "status": status, "result": null, "startedAt": null,
            "completedAt": null, "updatedAt": "2026-06-20T00:00:00.000Z",
        })
    }

    fn transfer(source: &str, target: &str, status: &str) -> Value {
        json!({
            "id": format!("transfer-{source}-{target}"), "type": "fork", "sourceThreadId": source,
            "targetThreadId": target,
            "sourcePoint": { "threadId": source },
            "basePoint": null, "sourceProviderInstanceId": null, "targetProviderInstanceId": null,
            "targetRunId": null, "status": status, "resolution": null, "createdBy": "user",
            "error": null, "createdAt": "2026-06-20T00:00:00.000Z", "updatedAt": "2026-06-20T00:00:00.000Z",
            "consumedAt": null,
        })
    }

    #[test]
    fn keeps_parent_independent_of_child_status() {
        // (activityRunStatus, status): upstream's table also uses terminal values as
        // activity statuses, which the typed shell cannot hold; the terminal ones
        // here come from `status` instead.
        let effective =
            |activity: Option<&str>, status: &str| activity.unwrap_or(status).to_owned();
        for ((parent_activity, parent_status), (child_activity, child_status)) in [
            ((Some("running"), "cancelled"), (None, "completed")),
            ((None, "completed"), (Some("running"), "cancelled")),
            (
                (Some("running"), "cancelled"),
                (Some("running"), "cancelled"),
            ),
            ((None, "completed"), (None, "completed")),
            ((Some("waiting"), "cancelled"), (None, "failed")),
            ((None, "interrupted"), (Some("starting"), "cancelled")),
        ] {
            let graph = derive_thread_relationship_graph(
                &[
                    shell(
                        "parent",
                        json!({ "activityRunStatus": parent_activity, "status": parent_status }),
                    ),
                    shell(
                        "child",
                        json!({
                            "activityRunStatus": child_activity, "status": child_status,
                            "lineage": lineage("parent", "parent", "fork"),
                        }),
                    ),
                ],
                None,
            );
            let child_row = &immediate_thread_relationships(&graph, &id("child"))[0];
            let parent_row = &immediate_thread_relationships(&graph, &id("parent"))[0];
            assert_eq!(
                thread_relationship_row_status(&graph, child_row),
                Some(effective(parent_activity, parent_status))
            );
            assert_eq!(
                thread_relationship_row_status(&graph, parent_row),
                Some(effective(child_activity, child_status))
            );
        }
    }

    #[test]
    fn does_not_label_a_missing_parent_with_its_childs_running_status() {
        let graph = derive_thread_relationship_graph(
            &[shell(
                "child",
                json!({ "status": "running", "lineage": lineage("missing-parent", "missing-parent", "subagent") }),
            )],
            None,
        );
        let row = &immediate_thread_relationships(&graph, &id("child"))[0];
        assert_eq!(thread_relationship_row_status(&graph, row), None);
    }

    #[test]
    fn keeps_an_older_activity_run_visible_over_a_newer_cancelled_run() {
        let graph = derive_thread_relationship_graph(
            &[shell(
                "thread-child",
                json!({
                    "activityRunStatus": "running", "status": "cancelled",
                    "forkedFrom": forked_from("thread-parent", "run-parent"),
                    "lineage": lineage("thread-parent", "thread-parent", "subagent"),
                }),
            )],
            None,
        );
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].target_thread_id, id("thread-child"));
        assert_eq!(graph.edges[0].status.as_deref(), Some("running"));
    }

    #[test]
    fn keeps_missing_parents_and_cycles_navigable_without_recursive_traversal() {
        let (root, child, missing) = (id("thread-root"), id("thread-child"), id("thread-missing"));
        let graph = derive_thread_relationship_graph(
            &[
                shell(
                    "thread-root",
                    json!({
                        "status": "completed", "forkedFrom": forked_from("thread-child", "run-cycle"),
                        "lineage": lineage("thread-root", "thread-child", "fork"),
                    }),
                ),
                shell(
                    "thread-child",
                    json!({
                        "status": "completed", "forkedFrom": forked_from("thread-missing", "run-missing"),
                        "lineage": lineage("thread-root", "thread-missing", "fork"),
                    }),
                ),
            ],
            None,
        );
        assert!(graph.nodes[&missing].missing);
        assert_eq!(related_thread_ids(&graph, &root), vec![child.clone()]);
        assert_eq!(
            related_thread_ids(&graph, &child),
            vec![root.clone(), missing.clone()]
        );
        assert_eq!(
            walk_thread_relationships(&graph, &root)
                .into_iter()
                .map(|row| (row.thread_id, row.depth))
                .collect::<Vec<_>>(),
            vec![(child.clone(), 1), (missing, 2)]
        );
        assert_eq!(
            immediate_thread_relationships(&graph, &root)
                .into_iter()
                .map(|row| row.thread_id)
                .collect::<Vec<_>>(),
            vec![child]
        );
    }

    #[test]
    fn combines_subagent_and_transfer_edges_with_archived_shell_state() {
        let (parent, child, transfer_target) = (
            id("thread-parent"),
            id("thread-child"),
            id("thread-transfer"),
        );
        let graph = derive_thread_relationship_graph(
            &[
                shell("thread-parent", json!({ "status": "completed" })),
                shell(
                    "thread-child",
                    json!({
                        "status": "completed", "archivedAt": "2026-06-24T00:00:00.000Z",
                        "lineage": lineage("thread-parent", "thread-parent", "subagent"),
                    }),
                ),
            ],
            Some(&projection_with(
                "thread-parent",
                vec![subagent("thread-child", "completed")],
                vec![transfer("thread-child", "thread-transfer", "consumed")],
            )),
        );
        assert!(
            graph.nodes[&child]
                .thread
                .as_ref()
                .unwrap()
                .archived_at
                .is_some()
        );
        assert!(graph.nodes[&transfer_target].missing);
        assert!(
            graph
                .edges
                .iter()
                .any(|edge| edge.source_thread_id == parent
                    && edge.target_thread_id == child
                    && edge.kind == ThreadRelationshipKind::Subagent)
        );
        assert!(graph.edges.iter().any(|edge| edge.source_thread_id == child
            && edge.target_thread_id == transfer_target
            && edge.kind == ThreadRelationshipKind::Transfer));
    }

    #[test]
    fn shows_child_activity_and_latest_status_after_its_delegated_task_settled() {
        for (activity, status, expected) in [
            (Some("running"), "running", "running"),
            (Some("waiting"), "completed", "waiting"),
            (None, "queued", "queued"),
            (None, "failed", "failed"),
            (None, "interrupted", "interrupted"),
            (None, "cancelled", "cancelled"),
            (None, "completed", "completed"),
            (None, "rolled_back", "interrupted"),
        ] {
            let graph = derive_thread_relationship_graph(
                &[
                    shell("thread-parent", json!({ "status": "completed" })),
                    shell(
                        "thread-child",
                        json!({
                            "status": status, "activityRunStatus": activity,
                            "latestRunId": "run-child-followup",
                            "lineage": lineage("thread-parent", "thread-parent", "subagent"),
                        }),
                    ),
                ],
                Some(&projection_with(
                    "thread-parent",
                    vec![subagent("thread-child", "completed")],
                    vec![],
                )),
            );
            let row = &immediate_thread_relationships(&graph, &id("thread-parent"))[0];
            assert_eq!(
                thread_relationship_row_status(&graph, row).as_deref(),
                Some(expected),
                "{activity:?} {status}"
            );
        }
    }

    #[test]
    fn keeps_the_live_shell_when_an_archived_snapshot_contains_the_same_thread_id() {
        let live = shell(
            "thread-child",
            json!({
                "title": "Live child", "status": "running",
                "forkedFrom": forked_from("thread-parent", "run-live"),
                "lineage": lineage("thread-parent", "thread-parent", "fork"),
            }),
        );
        let stale = shell(
            "thread-child",
            json!({
                "title": "Stale archived child", "status": "completed", "archivedAt": "2026-06-24T00:00:00.000Z",
                "forkedFrom": forked_from("thread-stale-parent", "run-stale"),
                "lineage": lineage("thread-stale-parent", "thread-stale-parent", "fork"),
            }),
        );
        let graph = derive_thread_relationship_graph(&[live, stale], None);
        let node = graph.nodes[&id("thread-child")].thread.as_ref().unwrap();
        assert_eq!(
            (node.title.as_str(), node.archived_at),
            ("Live child", None)
        );
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].source_thread_id, id("thread-parent"));
        assert!(!graph.nodes.contains_key(&id("thread-stale-parent")));
    }

    #[test]
    fn resolves_merge_back_only_for_forks_and_prefers_the_recorded_fork_source() {
        let projection = |forked: Value, relationship: &str| {
            let mut contract = v2_projection();
            contract.thread.forked_from = from_json(forked);
            contract.thread.lineage =
                from_json(lineage("thread-source", "thread-parent", relationship));
            ThreadProjection::from(contract)
        };
        assert_eq!(
            resolve_merge_back_target_thread_id(Some(&projection(
                forked_from("thread-source", "run-source"),
                "fork"
            ))),
            Some(id("thread-source"))
        );
        assert_eq!(
            resolve_merge_back_target_thread_id(Some(&projection(json!(null), "fork"))),
            Some(id("thread-parent"))
        );
        assert_eq!(
            resolve_merge_back_target_thread_id(Some(&projection(json!(null), "subagent"))),
            None
        );
        assert_eq!(resolve_merge_back_target_thread_id(None), None);
    }

    fn fork_shell(
        thread_id: &str,
        parent: Option<&str>,
        created_at: &str,
        extra: Value,
    ) -> Arc<OrchestrationV2ThreadShell> {
        let mut patch = json!({ "createdAt": created_at });
        if let Some(parent) = parent {
            patch["forkedFrom"] = forked_from(parent, &format!("run-{thread_id}"));
            patch["lineage"] = lineage(parent, parent, "fork");
        }
        for (key, field) in extra.as_object().unwrap() {
            patch[key] = field.clone();
        }
        shell(thread_id, patch)
    }

    fn ordered_lineage_ids(
        threads: Vec<Arc<OrchestrationV2ThreadShell>>,
        merge_target: Option<&str>,
        projection: Option<ThreadProjection>,
    ) -> Vec<String> {
        let current = id("thread-current");
        let graph = derive_thread_relationship_graph(&threads, projection.as_ref());
        order_web_thread_lineage_rows(
            &graph,
            &immediate_thread_relationships(&graph, &current),
            &current,
            merge_target.map(id).as_ref(),
        )
        .into_iter()
        .map(|row| row.thread_id.to_string())
        .collect()
    }

    #[test]
    fn orders_sibling_forks_newest_created_first() {
        assert_eq!(
            ordered_lineage_ids(
                vec![
                    fork_shell(
                        "thread-current",
                        None,
                        "2026-06-01T00:00:00.000Z",
                        json!({})
                    ),
                    fork_shell(
                        "thread-fork-oldest",
                        Some("thread-current"),
                        "2026-06-02T00:00:00.000Z",
                        json!({})
                    ),
                    fork_shell(
                        "thread-fork-middle",
                        Some("thread-current"),
                        "2026-06-03T00:00:00.000Z",
                        json!({})
                    ),
                    fork_shell(
                        "thread-fork-newest",
                        Some("thread-current"),
                        "2026-06-04T00:00:00.000Z",
                        json!({})
                    ),
                ],
                None,
                None
            ),
            vec![
                "thread-fork-newest",
                "thread-fork-middle",
                "thread-fork-oldest"
            ]
        );
    }

    #[test]
    fn does_not_reorder_when_related_thread_activity_arrives() {
        let before = ordered_lineage_ids(
            vec![
                fork_shell(
                    "thread-current",
                    None,
                    "2026-06-01T00:00:00.000Z",
                    json!({}),
                ),
                fork_shell(
                    "thread-fork-a",
                    Some("thread-current"),
                    "2026-06-02T00:00:00.000Z",
                    json!({}),
                ),
                fork_shell(
                    "thread-fork-b",
                    Some("thread-current"),
                    "2026-06-03T00:00:00.000Z",
                    json!({}),
                ),
                fork_shell(
                    "thread-fork-c",
                    Some("thread-current"),
                    "2026-06-04T00:00:00.000Z",
                    json!({}),
                ),
            ],
            None,
            None,
        );
        // The shell snapshot arrives ordered by `updated_at`, and a client-side
        // `thread.updated` re-appends the shell it replaces, so both the input
        // order and the mutable timestamps move under us while the panel is open.
        let after = ordered_lineage_ids(
            vec![
                fork_shell(
                    "thread-fork-b",
                    Some("thread-current"),
                    "2026-06-03T00:00:00.000Z",
                    json!({ "updatedAt": "2026-07-30T00:00:00.000Z", "status": "running" }),
                ),
                fork_shell(
                    "thread-fork-c",
                    Some("thread-current"),
                    "2026-06-04T00:00:00.000Z",
                    json!({}),
                ),
                fork_shell(
                    "thread-current",
                    None,
                    "2026-06-01T00:00:00.000Z",
                    json!({}),
                ),
                fork_shell(
                    "thread-fork-a",
                    Some("thread-current"),
                    "2026-06-02T00:00:00.000Z",
                    json!({ "updatedAt": "2026-07-31T00:00:00.000Z", "status": "failed" }),
                ),
            ],
            None,
            None,
        );
        assert_eq!(
            before,
            vec!["thread-fork-c", "thread-fork-b", "thread-fork-a"]
        );
        assert_eq!(after, before);
    }

    #[test]
    fn pins_the_parent_first_and_a_distinct_merge_back_target_second() {
        assert_eq!(
            ordered_lineage_ids(
                vec![
                    fork_shell("thread-parent", None, "2026-06-01T00:00:00.000Z", json!({})),
                    fork_shell(
                        "thread-current",
                        Some("thread-parent"),
                        "2026-06-02T00:00:00.000Z",
                        json!({})
                    ),
                    fork_shell(
                        "thread-merge-target",
                        Some("thread-current"),
                        "2026-06-03T00:00:00.000Z",
                        json!({})
                    ),
                    fork_shell(
                        "thread-newest-fork",
                        Some("thread-current"),
                        "2026-06-09T00:00:00.000Z",
                        json!({})
                    ),
                ],
                Some("thread-merge-target"),
                None
            ),
            vec!["thread-parent", "thread-merge-target", "thread-newest-fork"]
        );
    }

    #[test]
    fn sinks_missing_nodes() {
        assert_eq!(
            ordered_lineage_ids(
                vec![
                    fork_shell(
                        "thread-current",
                        None,
                        "2026-06-01T00:00:00.000Z",
                        json!({})
                    ),
                    fork_shell(
                        "thread-dated-fork",
                        Some("thread-current"),
                        "2026-06-02T00:00:00.000Z",
                        json!({})
                    ),
                ],
                None,
                Some(projection_with(
                    "thread-current",
                    vec![],
                    vec![transfer(
                        "thread-current",
                        "thread-missing-transfer",
                        "consumed"
                    )]
                ))
            ),
            vec!["thread-dated-fork", "thread-missing-transfer"]
        );
    }

    #[test]
    fn breaks_equal_creation_times_by_thread_id() {
        assert_eq!(
            ordered_lineage_ids(
                vec![
                    fork_shell(
                        "thread-current",
                        None,
                        "2026-06-01T00:00:00.000Z",
                        json!({})
                    ),
                    fork_shell(
                        "thread-fork-c",
                        Some("thread-current"),
                        "2026-06-02T00:00:00.000Z",
                        json!({})
                    ),
                    fork_shell(
                        "thread-fork-a",
                        Some("thread-current"),
                        "2026-06-02T00:00:00.000Z",
                        json!({})
                    ),
                    fork_shell(
                        "thread-fork-b",
                        Some("thread-current"),
                        "2026-06-02T00:00:00.000Z",
                        json!({})
                    ),
                ],
                None,
                None
            ),
            vec!["thread-fork-a", "thread-fork-b", "thread-fork-c"]
        );
    }
}
