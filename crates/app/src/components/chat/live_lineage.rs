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
use super::thread_navigation::request_open_thread;
use super::thread_relationships_panel::{LineageActions, ThreadRelationshipsPanel};
use crate::app_runtime::write::bus::CommandBus;
use crate::app_runtime::write::commands::CommandError;
use crate::state::store::ClientStore;
use gpui_kit::{App, AsyncApp, Entity, WeakEntity};
use std::rc::Rc;

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

fn toast_error(title: &str, error: &CommandError, cx: &mut App) {
    crate::ui::toast::toast_manager(cx).update(cx, |store, cx| {
        store.add(
            crate::ui::toast::ToastOptions::new(title.to_owned())
                .type_(crate::ui::toast_logic::ToastType::Error)
                .description(error.message.clone()),
            cx,
        );
    });
}

/// Runs `task`, then clears the panel's busy state and reports a failure.
fn settle(
    panel: WeakEntity<ThreadRelationshipsPanel>,
    task: gpui_kit::Task<Result<(), CommandError>>,
    failure_title: &'static str,
    on_success: impl FnOnce(&mut App) + 'static,
    stop: bool,
    cx: &mut App,
) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        let result = task.await;
        cx.update(|cx| {
            panel
                .update(cx, |panel, cx| {
                    if stop {
                        panel.finish_stop(cx);
                    } else {
                        panel.finish_busy_action(cx);
                    }
                })
                .ok();
            match result {
                Ok(()) => on_success(cx),
                Err(error) => toast_error(failure_title, &error, cx),
            }
        });
    })
    .detach();
}

/// The lineage panel's commands for the routed thread: merge back, disconnect
/// the agent session, stop a subagent, stop a workflow
/// (`ThreadRelationshipsControl.tsx`).
pub fn live_lineage_actions(
    client: Entity<ClientStore>,
    bus: CommandBus,
    thread_id: ThreadId,
) -> LineageActions {
    let open_thread = Rc::new(|id: &ThreadId, _: &mut gpui_kit::Window, cx: &mut App| {
        request_open_thread(id.as_str(), cx);
    });
    let merge = {
        let (client, bus, thread_id) = (client.clone(), bus.clone(), thread_id.clone());
        Rc::new(
            move |panel: WeakEntity<ThreadRelationshipsPanel>,
                  _: &mut gpui_kit::Window,
                  cx: &mut App| {
                let target = client
                    .read(cx)
                    .state()
                    .thread(&thread_id)
                    .and_then(|entry| {
                        let projection = entry.state.data.as_ref()?;
                        let run = resolve_latest_merge_back_run(&projection.runs)?;
                        let target = resolve_merge_back_target_thread_id(Some(projection))?;
                        Some((target, run.id.as_str().to_owned()))
                    });
                let Some((target, run_id)) = target else {
                    panel
                        .update(cx, |panel, cx| panel.finish_busy_action(cx))
                        .ok();
                    return;
                };
                let source = thread_id.clone();
                let target_for_task = target.clone();
                let task = bus.task(cx, move |commands| async move {
                    commands
                        .merge_thread_back(&source, &target_for_task, &run_id)
                        .await
                });
                settle(
                    panel,
                    task,
                    "Failed to merge back",
                    move |cx| request_open_thread(target.as_str(), cx),
                    false,
                    cx,
                );
            },
        )
    };
    let detach = {
        let (bus, thread_id) = (bus.clone(), thread_id.clone());
        Rc::new(
            move |panel: WeakEntity<ThreadRelationshipsPanel>,
                  _: &mut gpui_kit::Window,
                  cx: &mut App| {
                let thread_id = thread_id.clone();
                let task = bus.task(cx, move |commands| async move {
                    commands.detach_thread_sessions(&thread_id).await
                });
                settle(
                    panel,
                    task,
                    "Failed to disconnect agent session",
                    |_| {},
                    false,
                    cx,
                );
            },
        )
    };
    let stop_subagent = {
        let (bus, thread_id) = (bus.clone(), thread_id.clone());
        Rc::new(
            move |panel: WeakEntity<ThreadRelationshipsPanel>,
                  child: &ThreadId,
                  agent: &crate::state::subagent_runtime::RuntimeSubagent,
                  _: &mut gpui_kit::Window,
                  cx: &mut App| {
                let (parent, child, agent_id) =
                    (thread_id.clone(), child.clone(), agent.id.clone());
                let task = bus.task(cx, move |commands| async move {
                    commands.stop_subagent(&parent, &agent_id, &child).await
                });
                settle(panel, task, "Could not stop subagent", |_| {}, true, cx);
            },
        )
    };
    let stop_workflow = {
        Rc::new(
            move |panel: WeakEntity<ThreadRelationshipsPanel>,
                  subagent_id: &str,
                  _: &mut gpui_kit::Window,
                  cx: &mut App| {
                let (thread_id, subagent_id) = (thread_id.clone(), subagent_id.to_owned());
                let task = bus.task(cx, move |commands| async move {
                    commands.stop_workflow(&thread_id, &subagent_id).await
                });
                settle(panel, task, "Could not stop workflow", |_| {}, true, cx);
            },
        )
    };
    LineageActions {
        open_thread: Some(open_thread),
        merge: Some(merge),
        detach: Some(detach),
        stop_subagent: Some(stop_subagent),
        stop_workflow: Some(stop_workflow),
    }
}
