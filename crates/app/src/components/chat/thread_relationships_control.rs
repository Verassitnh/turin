//! Port of upstream/apps/web/src/components/chat/ThreadRelationshipsControl.tsx
//!
//! Lineage paging helpers and relationship row copy. The panel view lives in
//! `thread_relationships_panel.rs`.

use crate::state::contracts::{
    OrchestrationV2ActivityRunStatus, OrchestrationV2ShellThreadStatus, OrchestrationV2ThreadShell,
    ThreadId,
};
use crate::state::subagent_display::format_subagent_display_title;
use crate::state::subagent_runtime::RuntimeSubagent;
use crate::state::thread_relationships::{
    ThreadRelationshipEdge, ThreadRelationshipKind, ThreadRelationshipWalkRow,
    is_parent_thread_relationship,
};
use crate::state::time::{epoch_millis, nullable_iso, parse_ms};

/// Lineage paging: show a workable window, keep the rest behind Show more.
pub const THREAD_LINEAGE_INITIAL_COUNT: usize = 6;
pub const THREAD_LINEAGE_PAGE_COUNT: usize = 12;

/// `max-h-[13.5rem]` on the related-threads list.
pub const THREAD_LINEAGE_LIST_MAX_HEIGHT_REM: f32 = 13.5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadLineageWindow<Row> {
    pub visible_rows: Vec<Row>,
    pub hidden_count: usize,
}

pub fn resolve_thread_lineage_window<Row: Clone>(
    rows: &[Row],
    visible_count: usize,
) -> ThreadLineageWindow<Row> {
    let visible_rows: Vec<Row> = rows.iter().take(visible_count).cloned().collect();
    let hidden_count = rows.len().saturating_sub(visible_rows.len());
    ThreadLineageWindow {
        visible_rows,
        hidden_count,
    }
}

pub fn thread_lineage_show_more_label(hidden_count: usize) -> String {
    format!("Show {} more", hidden_count.min(THREAD_LINEAGE_PAGE_COUNT))
}

pub fn relationship_label(edge: &ThreadRelationshipEdge, current_thread_id: &ThreadId) -> String {
    if edge.kind == ThreadRelationshipKind::Transfer {
        return "Context transfer".to_owned();
    }
    if edge.kind == ThreadRelationshipKind::Subagent {
        return if edge.source_thread_id == *current_thread_id {
            "Subagent".to_owned()
        } else {
            "Parent agent".to_owned()
        };
    }
    if edge.source_thread_id == *current_thread_id {
        "Fork".to_owned()
    } else {
        "Parent thread".to_owned()
    }
}

pub fn relationship_thread_title(title: &str, is_subagent: bool) -> String {
    if is_subagent {
        format_subagent_display_title(title)
    } else {
        title.to_owned()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadLineageGroupId {
    Related,
    Active,
    Previous,
}

const PREVIOUS_SUBAGENT_STATUSES: &[&str] = &[
    "completed",
    "failed",
    "error",
    "cancelled",
    "interrupted",
    "idle",
];

pub fn thread_lineage_group_id(
    edge: &ThreadRelationshipEdge,
    current_thread_id: &ThreadId,
) -> ThreadLineageGroupId {
    if edge.kind != ThreadRelationshipKind::Subagent
        || is_parent_thread_relationship(edge, current_thread_id)
    {
        return ThreadLineageGroupId::Related;
    }
    let status = edge.status.as_deref().unwrap_or("");
    if PREVIOUS_SUBAGENT_STATUSES.contains(&status) {
        ThreadLineageGroupId::Previous
    } else {
        ThreadLineageGroupId::Active
    }
}

pub fn group_thread_lineage_rows(
    rows: &[ThreadRelationshipWalkRow],
    current_thread_id: &ThreadId,
) -> (
    Vec<ThreadRelationshipWalkRow>,
    Vec<ThreadRelationshipWalkRow>,
    Vec<ThreadRelationshipWalkRow>,
) {
    let mut related = Vec::new();
    let mut active = Vec::new();
    let mut previous = Vec::new();
    for row in rows {
        match thread_lineage_group_id(&row.edge, current_thread_id) {
            ThreadLineageGroupId::Related => related.push(row.clone()),
            ThreadLineageGroupId::Active => active.push(row.clone()),
            ThreadLineageGroupId::Previous => previous.push(row.clone()),
        }
    }
    (related, active, previous)
}

/// The row's timer and hover card follow current child work, including queued
/// follow-ups whose provider turn has not started yet.
pub fn current_subagent(
    agent: Option<RuntimeSubagent>,
    child_thread: Option<&OrchestrationV2ThreadShell>,
) -> Option<RuntimeSubagent> {
    let mut agent = agent?;
    let child = child_thread?;
    let live_status = child
        .activity_run_status
        .flatten()
        .map(activity_run_status_wire)
        .or_else(|| {
            (child.status == OrchestrationV2ShellThreadStatus::Queued)
                .then_some("queued".to_owned())
        });
    let child_ms = child
        .latest_run_requested_at
        .flatten()
        .as_ref()
        .map(epoch_millis);
    let agent_ms = agent
        .completed_at
        .as_deref()
        .or(agent.started_at.as_deref())
        .or(Some(agent.updated_at.as_str()))
        .and_then(parse_ms);
    let newer_run = matches!((child_ms, agent_ms), (Some(c), Some(a)) if c > a);
    if live_status.is_none() && !newer_run {
        return Some(agent);
    }
    let has_live_status = live_status.is_some();
    let status = live_status.unwrap_or_else(|| thread_status_wire(child.status));
    let started_at = if has_live_status {
        nullable_iso(child.activity_run_started_at.flatten().as_ref())
    } else {
        nullable_iso(child.latest_run_started_at.flatten().as_ref())
    };
    let completed_at = if has_live_status {
        None
    } else {
        nullable_iso(child.latest_run_completed_at.flatten().as_ref())
    };
    agent.status = map_live_subagent_status(&status);
    agent.started_at = started_at;
    agent.completed_at = completed_at;
    agent.progress = None;
    agent.result = None;
    agent.error = None;
    Some(agent)
}

fn activity_run_status_wire(status: OrchestrationV2ActivityRunStatus) -> String {
    match status {
        OrchestrationV2ActivityRunStatus::Preparing => "preparing",
        OrchestrationV2ActivityRunStatus::Starting => "starting",
        OrchestrationV2ActivityRunStatus::Running => "running",
        OrchestrationV2ActivityRunStatus::Waiting => "waiting",
    }
    .to_owned()
}

fn thread_status_wire(status: OrchestrationV2ShellThreadStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn map_live_subagent_status(status: &str) -> crate::state::subagent_runtime::RuntimeSubagentStatus {
    use crate::state::subagent_runtime::RuntimeSubagentStatus as S;
    match status {
        "preparing" | "starting" | "queued" => S::Pending,
        "rolled_back" => S::Interrupted,
        "running" | "in_progress" => S::Running,
        "waiting" | "blocked" => S::Waiting,
        "completed" => S::Completed,
        "failed" | "error" => S::Failed,
        "cancelled" | "interrupted" => S::Cancelled,
        "idle" => S::Idle,
        _ => S::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::thread_relationships::ThreadRelationshipEdge;

    fn id(name: &str) -> ThreadId {
        ThreadId::new(name).unwrap()
    }

    #[test]
    fn shows_six_rows_before_the_first_expansion() {
        let rows: Vec<String> = (0..20).map(|i| format!("row-{i}")).collect();
        let window = resolve_thread_lineage_window(&rows, 6);
        assert_eq!(window.visible_rows, rows[..6]);
        assert_eq!(window.hidden_count, 14);
    }

    #[test]
    fn offers_one_page_at_a_time() {
        assert_eq!(thread_lineage_show_more_label(14), "Show 12 more");
        assert_eq!(thread_lineage_show_more_label(2), "Show 2 more");
    }

    #[test]
    fn omits_the_expansion_affordance_when_everything_fits() {
        let rows: Vec<i32> = (0..6).collect();
        assert_eq!(resolve_thread_lineage_window(&rows, 6).hidden_count, 0);
        assert_eq!(thread_lineage_show_more_label(0), "Show 0 more");
    }

    #[test]
    fn labels_relationship_edges_from_the_current_thread() {
        let current = id("current");
        assert_eq!(
            relationship_label(
                &ThreadRelationshipEdge {
                    source_thread_id: current.clone(),
                    target_thread_id: id("child"),
                    kind: ThreadRelationshipKind::Fork,
                    status: None,
                },
                &current,
            ),
            "Fork"
        );
        assert_eq!(
            relationship_label(
                &ThreadRelationshipEdge {
                    source_thread_id: id("parent"),
                    target_thread_id: current.clone(),
                    kind: ThreadRelationshipKind::Subagent,
                    status: None,
                },
                &current,
            ),
            "Parent agent"
        );
    }

    #[test]
    fn groups_active_subagents_separately_from_previous() {
        let current = id("current");
        let row = |status: &str| ThreadRelationshipWalkRow {
            thread_id: id("child"),
            from_thread_id: current.clone(),
            depth: 1,
            edge: ThreadRelationshipEdge {
                source_thread_id: current.clone(),
                target_thread_id: id("child"),
                kind: ThreadRelationshipKind::Subagent,
                status: Some(status.to_owned()),
            },
        };
        let (_, active, previous) =
            group_thread_lineage_rows(&[row("running"), row("completed")], &current);
        assert_eq!(active.len(), 1);
        assert_eq!(previous.len(), 1);
    }
}
