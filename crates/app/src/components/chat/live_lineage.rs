//! Live [`LineagePanelSnapshot`] from client store projection + shell rows.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use turin_contracts::ThreadId;

use crate::state::client_state::ClientState;
use crate::state::contracts::OrchestrationV2ThreadShell;
use crate::state::subagent_runtime::{
    ProjectedSubagent, RuntimeSubagentStatus, derive_workflow_groups,
    projected_subagents_to_runtime,
};
use crate::state::support::turn_item::is_subagent_status_active;
use crate::state::thread_relationships::{
    derive_thread_relationship_graph, immediate_thread_relationships,
    order_web_thread_lineage_rows, resolve_merge_back_target_thread_id,
};
use crate::state::thread_workflows::{
    can_detach_thread_provider_session, resolve_latest_merge_back_run,
};

use super::thread_details_demo::LineagePanelSnapshot;

pub fn lineage_panel_snapshot(
    state: &ClientState,
    thread_id: &ThreadId,
    driver: Option<String>,
) -> Option<LineagePanelSnapshot> {
    let entry = state.thread(thread_id)?;
    let projection = entry.state.data.as_ref()?;
    let shell_view = state.shell_view.as_ref()?;
    let shells: Vec<Arc<OrchestrationV2ThreadShell>> =
        shell_view.snapshot.threads.iter().cloned().collect();
    let merge_target = resolve_merge_back_target_thread_id(Some(projection));
    let graph = derive_thread_relationship_graph(&shells, Some(projection));
    let rows = order_web_thread_lineage_rows(
        &graph,
        &immediate_thread_relationships(&graph, thread_id),
        thread_id,
        merge_target.as_ref(),
    );
    let projected: Vec<ProjectedSubagent> = projection
        .subagents
        .iter()
        .map(|subagent| ProjectedSubagent::from(subagent.as_ref()))
        .collect();
    let runtime = projected_subagents_to_runtime(&projected);
    let workflow_groups = derive_workflow_groups(&runtime);
    let subagents_by_child_thread: HashMap<String, _> = runtime
        .into_iter()
        .filter_map(|agent| {
            agent
                .child_thread_id
                .clone()
                .map(|thread_id| (thread_id, agent))
        })
        .collect();
    let running_without_thread = projected
        .iter()
        .filter(|agent| agent.child_thread_id.is_none() && is_subagent_status_active(agent.status))
        .count();
    let stoppable_agent_ids: HashSet<String> = projected
        .iter()
        .filter(|agent| is_subagent_status_active(agent.status))
        .map(|agent| agent.id.clone())
        .collect();
    let has_merge_back_run = resolve_latest_merge_back_run(&projection.runs).is_some();
    let can_operate_workflow = workflow_groups.iter().any(|group| {
        matches!(
            group.workflow.status,
            RuntimeSubagentStatus::Running | RuntimeSubagentStatus::Waiting
        )
    });
    Some(LineagePanelSnapshot {
        current_thread_id: thread_id.clone(),
        graph,
        rows,
        workflow_groups,
        subagents_by_child_thread,
        driver,
        merge_target_thread_id: merge_target,
        has_merge_back_run,
        can_detach: can_detach_thread_provider_session(projection),
        can_operate_workflow,
        stoppable_agent_ids,
        running_without_thread,
    })
}
