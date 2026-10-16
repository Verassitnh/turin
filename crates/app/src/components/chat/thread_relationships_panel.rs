//! Port of upstream/apps/web/src/components/chat/ThreadRelationshipsControl.tsx
//! (`ThreadRelationshipsPanel`, `ThreadLineageGroup`, `ThreadLineageRowList`).
//!
//! The Lineage section of the thread details panel: related threads, the
//! running subagents, and a collapsed "Previous agents" group, each row a
//! link that opens its thread. Reads a [`LineagePanelSnapshot`] (the projected
//! graph) and reports actions through [`LineageActions`]; the caller owns the
//! commands, as upstream's panel owns its atom commands.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div, px, rems,
};

use super::agent_elapsed::AgentElapsed;
use super::subagent_tooltip_content::{
    SubagentTooltipContent, runtime_status_to_turn_item_status, with_subagent_tooltip,
};
use super::thread_details_control::{
    ThreadDetailsControl, ThreadDetailsPart, ThreadDetailsTone, panel_split_group,
    panel_split_separator,
};
use super::thread_details_demo::{LineagePanelSnapshot, lineage_demo_expanded};
use super::thread_details_section::ThreadDetailsSection;
use super::thread_lineage_workflow_row::{
    ThreadLineageWorkflowRow, WorkflowRowState, with_text_tooltip,
};
use super::thread_relationship_icon::{ThreadRelationshipIcon, thread_relationship_status_label};
use super::thread_relationships_control::{
    THREAD_LINEAGE_INITIAL_COUNT, THREAD_LINEAGE_PAGE_COUNT, ThreadLineageGroupId,
    current_subagent, group_thread_lineage_rows, relationship_label, relationship_thread_title,
    resolve_thread_lineage_window, thread_lineage_show_more_label,
};
use crate::pull_request::dropdown::dropdown;
use crate::pull_request::presentation_view::Spinner;
use crate::state::contracts::ThreadId;
use crate::state::subagent_display::{
    ResolveSubagentMetadataInput, SubagentChildThread, SubagentParentThread,
    resolve_subagent_metadata, subagent_detail_preview,
};
use crate::state::subagent_runtime::{RuntimeSubagent, RuntimeSubagentStatus};
use crate::state::thread_relationships::{
    ThreadRelationshipKind, ThreadRelationshipWalkRow, is_parent_thread_relationship,
    thread_relationship_row_status,
};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, TEXT_2XS, TEXT_SM, tw};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::collapsible_section_header::{
    CollapsibleSectionHeader, CollapsibleSectionVariant, SectionHeaderStatus,
};
use crate::ui::tooltip::Tooltip;
use crate::ui::{Icon, IconName};

type ThreadHandler = Rc<dyn Fn(&ThreadId, &mut Window, &mut App)>;
type ActionHandler = Rc<dyn Fn(&mut Window, &mut App)>;
type StopSubagentHandler = Rc<dyn Fn(&ThreadId, &RuntimeSubagent, &mut Window, &mut App)>;
type StopWorkflowHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// The commands the panel fires (`navigate`, `mergeBack`, `stopSession`,
/// `interruptTurn`, `stopWorkflow`). Unset handlers make the control inert.
#[derive(Clone, Default)]
pub struct LineageActions {
    pub open_thread: Option<ThreadHandler>,
    pub merge: Option<ActionHandler>,
    pub detach: Option<ActionHandler>,
    pub stop_subagent: Option<StopSubagentHandler>,
    pub stop_workflow: Option<StopWorkflowHandler>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineageBusyAction {
    Merge,
    Detach,
}

#[derive(Clone, Copy)]
struct GroupState {
    expanded: bool,
    visible_count: usize,
}

pub struct ThreadRelationshipsPanel {
    snapshot: LineagePanelSnapshot,
    actions: LineageActions,
    groups: [GroupState; 3],
    busy_action: Option<LineageBusyAction>,
    stopping_id: Option<String>,
}

const GROUP_ORDER: [ThreadLineageGroupId; 3] = [
    ThreadLineageGroupId::Related,
    ThreadLineageGroupId::Active,
    ThreadLineageGroupId::Previous,
];

fn group_index(id: ThreadLineageGroupId) -> usize {
    match id {
        ThreadLineageGroupId::Related => 0,
        ThreadLineageGroupId::Active => 1,
        ThreadLineageGroupId::Previous => 2,
    }
}

fn is_failed(status: Option<&str>) -> bool {
    matches!(status, Some("failed" | "error"))
}

fn is_live(status: RuntimeSubagentStatus) -> bool {
    matches!(
        status,
        RuntimeSubagentStatus::Pending
            | RuntimeSubagentStatus::Running
            | RuntimeSubagentStatus::Waiting
    )
}

fn runtime_status_wire(status: RuntimeSubagentStatus) -> &'static str {
    match status {
        RuntimeSubagentStatus::Pending => "pending",
        RuntimeSubagentStatus::Running => "running",
        RuntimeSubagentStatus::Waiting => "waiting",
        RuntimeSubagentStatus::Idle => "idle",
        RuntimeSubagentStatus::Completed => "completed",
        RuntimeSubagentStatus::Failed => "failed",
        RuntimeSubagentStatus::Cancelled => "cancelled",
        RuntimeSubagentStatus::Interrupted => "interrupted",
    }
}

impl ThreadRelationshipsPanel {
    pub fn new(mut snapshot: LineagePanelSnapshot) -> Self {
        // A workflow coordinator is already a subagent row here; the grouped
        // model is what lets that row unfold into the phases and members it ran.
        let current = snapshot.current_thread_id.clone();
        let statuses: Vec<Option<&'static str>> = snapshot
            .rows
            .iter()
            .map(|row| {
                let agent = (row.edge.kind == ThreadRelationshipKind::Subagent
                    && !is_parent_thread_relationship(&row.edge, &current))
                .then(|| {
                    snapshot
                        .subagents_by_child_thread
                        .get(row.thread_id.as_str())
                })
                .flatten()?;
                snapshot
                    .workflow_groups
                    .iter()
                    .find(|group| group.workflow.id == agent.id)
                    .map(|group| runtime_status_wire(group.workflow.status))
            })
            .collect();
        for (row, status) in snapshot.rows.iter_mut().zip(statuses) {
            if let Some(status) = status {
                row.edge.status = Some(status.to_owned());
            }
        }
        let group = |expanded| GroupState {
            expanded,
            visible_count: THREAD_LINEAGE_INITIAL_COUNT,
        };
        Self {
            snapshot,
            actions: LineageActions::default(),
            groups: [group(true), group(true), group(lineage_demo_expanded())],
            busy_action: None,
            stopping_id: None,
        }
    }

    #[allow(dead_code)]
    pub fn actions(mut self, actions: LineageActions) -> Self {
        self.actions = actions;
        self
    }

    /// Replace the projected snapshot while keeping wired actions.
    pub fn reload(&mut self, snapshot: LineagePanelSnapshot, cx: &mut Context<Self>) {
        let actions = self.actions.clone();
        *self = Self::new(snapshot).actions(actions);
        cx.notify();
    }

    /// The caller's `mergeBack` / `stopSession` command settled.
    #[allow(dead_code)]
    pub fn finish_busy_action(&mut self, cx: &mut Context<Self>) {
        self.busy_action = None;
        cx.notify();
    }

    /// The caller's `interruptTurn` / `stopWorkflow` command settled.
    #[allow(dead_code)]
    pub fn finish_stop(&mut self, cx: &mut Context<Self>) {
        self.stopping_id = None;
        cx.notify();
    }

    /// Subagents without a child thread yet have no row, so count them separately.
    fn running_count(&self, active: &[ThreadRelationshipWalkRow]) -> usize {
        self.snapshot.running_without_thread
            + active
                .iter()
                .filter(|row| row.edge.status.as_deref() == Some("running"))
                .count()
    }

    /// `projectedAgent`, then `currentSubagent` unless it is a workflow.
    /// The flag is whether the agent coordinates a workflow.
    fn agent_for_row(&self, row: &ThreadRelationshipWalkRow) -> (Option<RuntimeSubagent>, bool) {
        let is_subagent = row.edge.kind == ThreadRelationshipKind::Subagent;
        let is_parent = is_parent_thread_relationship(&row.edge, &self.snapshot.current_thread_id);
        let projected = (is_subagent && !is_parent)
            .then(|| {
                self.snapshot
                    .subagents_by_child_thread
                    .get(row.thread_id.as_str())
                    .cloned()
            })
            .flatten();
        let is_workflow = projected.as_ref().is_some_and(|agent| {
            self.snapshot
                .workflow_groups
                .iter()
                .any(|group| group.workflow.id == agent.id)
        });
        if is_workflow {
            return (projected, true);
        }
        let child = self
            .snapshot
            .graph
            .nodes
            .get(&row.thread_id)
            .and_then(|node| node.thread.as_deref());
        (current_subagent(projected, child), false)
    }
}

impl Render for ThreadRelationshipsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.snapshot.current_thread_id.clone();
        let (related, active, previous) = group_thread_lineage_rows(&self.snapshot.rows, &current);
        let running_count = self.running_count(&active);
        if self.snapshot.rows.is_empty() && running_count == 0 {
            return div().into_any_element();
        }
        let mut section = ThreadDetailsSection::new(lineage_title(running_count));
        if self.snapshot.can_detach {
            section = section.actions(self.more_actions_menu(cx));
        }
        let groups = GROUP_ORDER
            .into_iter()
            .zip([related, active, previous])
            .filter(|(_, rows)| !rows.is_empty())
            .map(|(id, rows)| self.render_group(window, cx, id, rows))
            .collect::<Vec<_>>();
        section
            .child(
                div()
                    .id("thread-relationships-panel")
                    .flex()
                    .flex_col()
                    .children(groups),
            )
            .into_any_element()
    }
}

impl ThreadRelationshipsPanel {
    /// `<Menu>` with the "More thread actions" trigger.
    fn more_actions_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let busy = self.busy_action.is_some();
        let panel = cx.entity().downgrade();
        let trigger = ThreadDetailsControl::new("lineage-more-actions")
            .part(ThreadDetailsPart::Icon)
            .disabled(busy)
            .child(
                Icon::new(IconName::Ellipsis)
                    .size(rems(0.875))
                    .color(theme.muted_foreground),
            );
        dropdown(
            "lineage-more-actions-menu",
            Anchor::TopRight,
            trigger,
            move |menu, _, _| {
                let panel = panel.clone();
                menu.item(
                    gpui_kit::component::menu::PopupMenuItem::new("Disconnect agent session")
                        .icon(IconName::Unplug)
                        .on_click(move |_, window, cx| {
                            panel
                                .update(cx, |panel, cx| {
                                    if panel.busy_action.is_some() {
                                        return;
                                    }
                                    panel.busy_action = Some(LineageBusyAction::Detach);
                                    cx.notify();
                                    if let Some(detach) = panel.actions.detach.clone() {
                                        detach(window, cx);
                                    }
                                })
                                .ok();
                        }),
                )
            },
        )
        .into_any_element()
    }

    /// `ThreadLineageGroup` around `ThreadLineageRowList`.
    fn render_group(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        id: ThreadLineageGroupId,
        rows: Vec<ThreadRelationshipWalkRow>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let index = group_index(id);
        let state = self.groups[index];
        let label = (id == ThreadLineageGroupId::Previous).then_some("Previous agents");
        let window_rows = resolve_thread_lineage_window(&rows, state.visible_count);
        let failed_count = rows
            .iter()
            .filter(|row| is_failed(row.edge.status.as_deref()))
            .count();
        let header = label.map(|label| {
            let mut header = CollapsibleSectionHeader::new(
                ("lineage-group", index),
                if state.expanded {
                    label.to_owned()
                } else {
                    format!("{label} ({})", rows.len())
                },
                state.expanded,
            )
            .variant(CollapsibleSectionVariant::Panel)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.groups[index].expanded = !this.groups[index].expanded;
                cx.notify();
            }));
            if failed_count > 0 {
                header =
                    header.accessory(SectionHeaderStatus(format!("{failed_count} failed").into()));
            }
            header
        });
        // An expanded workflow asks for a taller window, so it raises the
        // bound while its tree is open (`has-data-workflow-expanded`).
        let workflow_open = window_rows.visible_rows.iter().any(|row| {
            self.workflow_state_key(row)
                .is_some_and(|key| workflow_expanded(window, cx, &key))
        });
        let max_height = if workflow_open {
            rems(28.)
                .to_pixels(window.rem_size())
                .min(window.viewport_size().height * 0.55)
        } else {
            rems(13.5).to_pixels(window.rem_size())
        };
        let hidden_count = window_rows.hidden_count;
        let rows = if state.expanded {
            window_rows
                .visible_rows
                .into_iter()
                .map(|row| self.render_row(window, cx, row))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        div()
            .children(header)
            .when(state.expanded, |this| {
                this.child(
                    div()
                        .id(("lineage-list", index))
                        .flex()
                        .flex_col()
                        .max_h(max_height)
                        .overflow_y_scroll()
                        .children(rows),
                )
                .when(hidden_count > 0, |this| {
                    this.child(
                        div()
                            .id(("lineage-show-more", index))
                            .flex()
                            .h(rems(2.))
                            .w_full()
                            .cursor_pointer()
                            .items_center()
                            .gap(rems(0.625))
                            .px(rems(0.625))
                            .rounded(RADIUS_LG)
                            .text_size(TEXT_SM.size)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(with_alpha(theme.muted_foreground, 0.7))
                            .hover(|style| {
                                style
                                    .bg(show_more_hover_surface(&theme))
                                    .text_color(with_alpha(theme.foreground, 0.8))
                            })
                            .child(Icon::new(IconName::Plus).size(rems(1.)))
                            .child(thread_lineage_show_more_label(hidden_count))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.groups[index].visible_count += THREAD_LINEAGE_PAGE_COUNT;
                                cx.notify();
                            })),
                    )
                })
            })
            .into_any_element()
    }

    fn workflow_state_key(&self, row: &ThreadRelationshipWalkRow) -> Option<SharedString> {
        let (agent, is_workflow) = self.agent_for_row(row);
        agent
            .filter(|_| is_workflow)
            .map(|agent| workflow_state_key(&agent.id))
    }

    /// One lineage `<li>`.
    fn render_row(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        row: ThreadRelationshipWalkRow,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let current = self.snapshot.current_thread_id.clone();
        let thread_id = row.thread_id.clone();
        let node = self.snapshot.graph.nodes.get(&thread_id);
        let missing = node.is_some_and(|node| node.missing);
        let thread = node.and_then(|node| node.thread.clone());
        let edge = row.edge.clone();
        let is_subagent = edge.kind == ThreadRelationshipKind::Subagent;
        let is_merge_target = self.snapshot.merge_target_thread_id.as_ref() == Some(&thread_id);
        let is_parent = is_parent_thread_relationship(&edge, &current);
        let row_status = thread_relationship_row_status(&self.snapshot.graph, &row);
        let icon = if is_parent {
            IconName::CornerLeftUp
        } else if is_subagent {
            IconName::Bot
        } else {
            IconName::GitFork
        };
        let relationship = relationship_label(&edge, &current);
        let (agent, is_workflow) = self.agent_for_row(&row);
        let workflow_group = agent
            .as_ref()
            .filter(|_| is_workflow)
            .and_then(|agent| {
                self.snapshot
                    .workflow_groups
                    .iter()
                    .find(|group| group.workflow.id == agent.id)
            })
            .cloned();
        let failed = is_failed(row_status.as_deref());
        let can_stop = !is_workflow
            && agent.as_ref().is_some_and(|agent| {
                self.snapshot.stoppable_agent_ids.contains(&agent.id)
                    && agent.started_at.is_some()
                    && is_live(agent.status)
            });
        let thread_title = relationship_thread_title(
            // A workflow's name is shorter than its child thread's title.
            workflow_group
                .as_ref()
                .and_then(|group| group.workflow.workflow_name.as_deref())
                .or(thread.as_ref().map(|thread| thread.title.as_str()))
                .or(agent.as_ref().map(|agent| agent.title.as_str()))
                .unwrap_or(thread_id.as_str()),
            is_subagent,
        );
        let driver: Option<SharedString> = self.snapshot.driver.clone().map(Into::into);
        let can_stop_workflow = self.snapshot.can_operate_workflow
            && workflow_group
                .as_ref()
                .is_some_and(|group| group.workflow.status == RuntimeSubagentStatus::Running);
        let hide_trailing_on_hover = if workflow_group.is_some() {
            can_stop_workflow
        } else {
            can_stop
        };
        let group_name = SharedString::from(format!("lineage-li-{}", thread_id.as_str()));
        let status_label = thread_relationship_status_label(row_status.as_deref());

        // `relationshipContent`: the trailing elapsed time or status label.
        let trailing: Option<AnyElement> = match &agent {
            Some(agent)
                if !failed && (elapsed_for(agent, true).has_value() || can_stop_workflow) =>
            {
                Some(
                    div()
                        .flex_shrink_0()
                        .text_size(TEXT_2XS.size)
                        .font_weight(FontWeight::NORMAL)
                        .text_color(theme.muted_foreground)
                        .when(can_stop_workflow, |this| {
                            this.min_w(rems(1.5)).flex().justify_end()
                        })
                        .child(elapsed_for(agent, true))
                        .into_any_element(),
                )
            }
            _ if !is_merge_target => Some(
                div()
                    .flex_shrink_0()
                    .text_size(TEXT_2XS.size)
                    .text_color(if failed {
                        theme.destructive
                    } else {
                        theme.muted_foreground
                    })
                    // A workflow reads its state from the icon badge alone.
                    .when(workflow_group.is_some(), |this| this.invisible().w_0())
                    .child(status_label)
                    .into_any_element(),
            ),
            _ => None,
        };
        let trailing = trailing.map(|trailing| {
            if hide_trailing_on_hover {
                // `group-hover:opacity-0`: keep the space, hide the label.
                div()
                    .flex_shrink_0()
                    .group_hover(group_name.clone(), |style| style.invisible())
                    .child(trailing)
                    .into_any_element()
            } else {
                trailing
            }
        });
        let link_content = div()
            .flex()
            .min_w_0()
            .flex_1()
            .items_center()
            .gap(rems(0.625))
            .child(ThreadRelationshipIcon {
                driver: if is_subagent && !is_parent {
                    driver.clone()
                } else {
                    None
                },
                status: row_status.clone(),
                fallback: icon,
            })
            .child(
                div().min_w_0().flex_1().child(
                    div()
                        .truncate()
                        .text_size(TEXT_SM.size)
                        .line_height(px(16.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(with_alpha(theme.foreground, 0.85))
                        .child(thread_title.clone()),
                ),
            )
            .when(agent.is_none(), |this| {
                // `opacity-0 group-hover:opacity-100`.
                this.child(
                    div()
                        .invisible()
                        .group_hover(group_name.clone(), |style| style.visible())
                        .child(
                            Icon::new(IconName::ArrowRight)
                                .size(rems(0.75))
                                .color(theme.muted_foreground),
                        ),
                )
            })
            .children(trailing);

        // `relationshipTooltip`.
        let link_id = SharedString::from(format!("lineage-link-{}", thread_id.as_str()));
        let open_id = thread_id.clone();
        let link_part = if workflow_group.is_some() || is_merge_target {
            ThreadDetailsPart::Primary
        } else {
            ThreadDetailsPart::Row
        };
        let control = ThreadDetailsControl::new(link_id.clone())
            .part(link_part)
            .disabled(missing)
            .child(link_content)
            .on_click({
                let open = self.actions.open_thread.clone();
                move |_: &ClickEvent, window, cx| {
                    if let Some(open) = &open {
                        open(&open_id, window, cx);
                    }
                }
            });
        let link = match agent.as_ref() {
            Some(agent) => {
                let child_thread = thread.as_deref().map(|thread| SubagentChildThread {
                    branch: thread.branch.as_ref().map(|b| b.as_str()),
                    worktree_path: thread.worktree_path.as_ref().map(|p| p.as_str()),
                });
                let parent_thread = self
                    .snapshot
                    .graph
                    .nodes
                    .get(&current)
                    .and_then(|node| node.thread.as_deref())
                    .map(|thread| SubagentParentThread {
                        project_id: thread.project_id.as_str(),
                        worktree_path: thread.worktree_path.as_ref().map(|p| p.as_str()),
                    });
                let metadata = resolve_subagent_metadata(&ResolveSubagentMetadataInput {
                    model: agent.model.as_deref(),
                    provider: None,
                    parent_thread,
                    child_thread,
                    parent_project_workspace_root: None,
                    child_project: None,
                });
                let content = SubagentTooltipContent::new(
                    thread_title.clone(),
                    runtime_status_to_turn_item_status(agent.status),
                )
                .metadata(metadata)
                .driver(driver.clone())
                .effort(agent.effort.clone())
                .elapsed(Some(elapsed_for(agent, false)))
                .preview(subagent_detail_preview(
                    agent.status,
                    agent.result.as_deref(),
                    agent.progress.as_deref(),
                ));
                with_subagent_tooltip(
                    SharedString::from(format!("{link_id}-tip")),
                    content,
                    control,
                )
            }
            None => {
                let hint: SharedString = if missing {
                    "This related thread is unavailable".into()
                } else {
                    format!("Open {} in this chat", relationship.to_lowercase()).into()
                };
                div()
                    .id(SharedString::from(format!("{link_id}-tip")))
                    .flex()
                    .min_w_0()
                    .child(control)
                    .tooltip_show_delay(std::time::Duration::from_millis(200))
                    .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
            }
        }
        // The link fills a split row's leading half, or the whole row.
        .flex_1();

        if let (Some(group), Some(agent)) = (workflow_group, agent.as_ref()) {
            let state = window.use_keyed_state(workflow_state_key(&agent.id), cx, |_, _| {
                WorkflowRowState::initial()
            });
            let stopping = self.stopping_id.as_deref() == Some(agent.id.as_str());
            let stop_disabled = self.stopping_id.is_some();
            let on_stop = can_stop_workflow.then(|| {
                let agent_id = agent.id.clone();
                let panel = cx.entity().downgrade();
                Rc::new(move |window: &mut Window, cx: &mut App| {
                    panel
                        .update(cx, |panel, cx| {
                            if panel.stopping_id.is_some() {
                                return;
                            }
                            panel.stopping_id = Some(agent_id.clone());
                            cx.notify();
                            if let Some(stop) = panel.actions.stop_workflow.clone() {
                                stop(&agent_id, window, cx);
                            }
                        })
                        .ok();
                }) as ActionHandler
            });
            let graph = self.snapshot.graph.clone();
            let open = self.actions.open_thread.clone();
            return ThreadLineageWorkflowRow {
                id: SharedString::from(thread_id.as_str().to_owned()),
                group,
                header: link.into_any_element(),
                state,
                driver,
                on_open_thread: Rc::new(move |member_thread_id, window, cx| {
                    if let (Some(open), Ok(id)) = (&open, ThreadId::new(member_thread_id)) {
                        open(&id, window, cx);
                    }
                }),
                is_thread_available: Rc::new(move |member_thread_id| {
                    ThreadId::new(member_thread_id)
                        .ok()
                        .and_then(|id| graph.nodes.get(&id).map(|node| !node.missing))
                        .unwrap_or(false)
                }),
                on_stop,
                stopping,
                stop_disabled,
            }
            .into_any_element();
        }

        let body: AnyElement = if is_merge_target {
            let parent_title = thread.as_ref().map(|thread| thread.title.clone());
            let busy = self.busy_action.is_some();
            let merging = self.busy_action == Some(LineageBusyAction::Merge);
            let merge_tip: SharedString = if !self.snapshot.has_merge_back_run {
                "Complete a run in this fork before merging it back".into()
            } else if let Some(title) = &parent_title {
                format!("Merge this conversation back into {title}").into()
            } else {
                "Merge this conversation back into its source".into()
            };
            let merge = ThreadDetailsControl::new(SharedString::from(format!(
                "lineage-merge-{}",
                thread_id.as_str()
            )))
            .part(ThreadDetailsPart::Secondary)
            .disabled(!self.snapshot.has_merge_back_run || busy)
            .child(if merging {
                Spinner::new("lineage-merge")
                    .size(rems(0.75))
                    .color(theme.muted_foreground)
                    .into_any_element()
            } else {
                Icon::new(IconName::GitMerge)
                    .size(rems(0.75))
                    .color(theme.muted_foreground)
                    .into_any_element()
            })
            .on_click(cx.listener(|this, _, window, cx| {
                if this.busy_action.is_some() {
                    return;
                }
                this.busy_action = Some(LineageBusyAction::Merge);
                cx.notify();
                if let Some(merge) = this.actions.merge.clone() {
                    merge(window, cx);
                }
            }));
            panel_split_group(
                SharedString::from(format!("lineage-split-{}", thread_id.as_str())),
                false,
                &theme,
            )
            .child(link)
            .child(panel_split_separator(&theme))
            .child(
                div()
                    .id(SharedString::from(format!(
                        "lineage-merge-tip-{}",
                        thread_id.as_str()
                    )))
                    .child(merge)
                    .tooltip(move |window, cx| Tooltip::new(merge_tip.clone()).build(window, cx)),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .border_1()
                    .border_color(gpui_kit::transparent_black())
                    .pl_1()
                    .pr(rems(0.625))
                    .text_size(TEXT_2XS.size)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.muted_foreground)
                    .child(status_label),
            )
            .into_any_element()
        } else {
            link.into_any_element()
        };

        // `canStop && agent`: the per-row stop button, revealed on hover.
        let stop_button = can_stop.then(|| {
            let agent = agent.clone().expect("can_stop implies an agent");
            let stopping = self.stopping_id.as_deref() == Some(thread_id.as_str());
            let stop_disabled = self.stopping_id.is_some();
            let child_id = thread_id.clone();
            div()
                .absolute()
                .right_1()
                .top_0()
                .bottom_0()
                .flex()
                .items_center()
                .invisible()
                .group_hover(group_name.clone(), |style| style.visible())
                .child(with_text_tooltip(
                    SharedString::from(format!("lineage-stop-tip-{}", thread_id.as_str())),
                    "Stop subagent",
                    ThreadDetailsControl::new(SharedString::from(format!(
                        "lineage-stop-{}",
                        thread_id.as_str()
                    )))
                    .part(ThreadDetailsPart::Icon)
                    .tone(ThreadDetailsTone::Destructive)
                    .disabled(stop_disabled)
                    .child(if stopping {
                        Spinner::new(format!("lineage-stop-{}", thread_id.as_str()))
                            .size(rems(0.75))
                            .color(theme.destructive_foreground)
                            .into_any_element()
                    } else {
                        Icon::new(IconName::Square)
                            .size(rems(0.75))
                            .color(theme.destructive_foreground)
                            .into_any_element()
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.stopping_id.is_some() {
                            return;
                        }
                        this.stopping_id = Some(child_id.as_str().to_owned());
                        cx.notify();
                        if let Some(stop) = this.actions.stop_subagent.clone() {
                            stop(&child_id, &agent, window, cx);
                        }
                    })),
                ))
        });
        div()
            .group(group_name)
            .relative()
            .flex()
            .h(rems(2.))
            .items_center()
            .rounded(RADIUS_LG)
            .child(body)
            .children(stop_button)
            .into_any_element()
    }
}

fn lineage_title(running_count: usize) -> SharedString {
    if running_count > 0 {
        SharedString::from(format!("Lineage · {running_count} running"))
    } else {
        SharedString::from("Lineage")
    }
}

fn elapsed_for(agent: &RuntimeSubagent, compact: bool) -> AgentElapsed {
    AgentElapsed {
        status: agent.status,
        started_at: agent.started_at.clone(),
        completed_at: agent.completed_at.clone(),
        compact,
    }
}

fn workflow_state_key(agent_id: &str) -> SharedString {
    format!("workflow-row-state-{agent_id}").into()
}

fn workflow_expanded(window: &mut Window, cx: &mut App, key: &SharedString) -> bool {
    window
        .use_keyed_state(key.clone(), cx, |_, _| WorkflowRowState::initial())
        .read(cx)
        .expanded
}

/// `hover:bg-black/[0.055] dark:hover:bg-white/[0.075]`.
fn show_more_hover_surface(theme: &AppTheme) -> gpui_kit::Hsla {
    if theme.is_dark() {
        with_alpha(tw::WHITE, 0.075).into()
    } else {
        with_alpha(tw::BLACK, 0.055).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::chat::thread_details_demo::lineage_demo_snapshot;

    fn row_for<'a>(
        panel: &'a ThreadRelationshipsPanel,
        thread: &str,
    ) -> &'a ThreadRelationshipWalkRow {
        panel
            .snapshot
            .rows
            .iter()
            .find(|row| row.thread_id.as_str() == thread)
            .unwrap_or_else(|| panic!("no row for {thread}"))
    }

    fn grouped(
        panel: &ThreadRelationshipsPanel,
    ) -> (
        Vec<ThreadRelationshipWalkRow>,
        Vec<ThreadRelationshipWalkRow>,
        Vec<ThreadRelationshipWalkRow>,
    ) {
        group_thread_lineage_rows(&panel.snapshot.rows, &panel.snapshot.current_thread_id)
    }

    // ThreadRelationshipsControl.agents.test.tsx: "groups and counts a
    // workflow by its run status instead of its coordinator chat".
    #[test]
    fn counts_a_running_workflow_by_its_run_status_not_its_coordinator_chat() {
        let panel = ThreadRelationshipsPanel::new(lineage_demo_snapshot());
        // The coordinator's chat shell is not "running" for the panel's
        // purposes; the workflow's own status is what the row carries.
        assert_eq!(
            row_for(&panel, "workflow-chat").edge.status.as_deref(),
            Some("running")
        );
        let (_, active, _) = grouped(&panel);
        // The workflow and the plain subagent.
        assert_eq!(panel.running_count(&active), 2);
        assert_eq!(lineage_title(2), "Lineage · 2 running");
    }

    #[test]
    fn a_settled_workflow_leaves_the_running_count_and_moves_to_previous() {
        let mut snapshot = lineage_demo_snapshot();
        for group in &mut snapshot.workflow_groups {
            group.workflow.status = RuntimeSubagentStatus::Completed;
        }
        let panel = ThreadRelationshipsPanel::new(snapshot);
        assert_eq!(
            row_for(&panel, "workflow-chat").edge.status.as_deref(),
            Some("completed")
        );
        let (_, active, previous) = grouped(&panel);
        assert_eq!(panel.running_count(&active), 1);
        assert!(
            previous
                .iter()
                .any(|row| row.thread_id.as_str() == "workflow-chat")
        );
        assert_eq!(lineage_title(1), "Lineage · 1 running");
        assert_eq!(lineage_title(0), "Lineage");
    }

    #[test]
    fn subagents_without_a_child_thread_count_as_running_without_a_row() {
        let mut snapshot = lineage_demo_snapshot();
        snapshot.running_without_thread = 3;
        let panel = ThreadRelationshipsPanel::new(snapshot);
        let (_, active, _) = grouped(&panel);
        assert_eq!(panel.running_count(&active), 5);
    }

    #[test]
    fn only_workflow_coordinators_unfold_into_phases() {
        let panel = ThreadRelationshipsPanel::new(lineage_demo_snapshot());
        let (workflow, is_workflow) = panel.agent_for_row(row_for(&panel, "workflow-chat"));
        assert!(is_workflow);
        assert_eq!(workflow.unwrap().id, "workflow-coordinator");
        let (_, is_workflow) = panel.agent_for_row(row_for(&panel, "thread-subagent"));
        assert!(!is_workflow);
        // A fork has no agent at all.
        let (agent, _) = panel.agent_for_row(row_for(&panel, "thread-fork-a"));
        assert!(agent.is_none());
    }

    #[test]
    fn pins_the_merge_target_parent_first() {
        let panel = ThreadRelationshipsPanel::new(lineage_demo_snapshot());
        assert_eq!(panel.snapshot.rows[0].thread_id.as_str(), "thread-parent");
        assert_eq!(
            panel
                .snapshot
                .merge_target_thread_id
                .as_ref()
                .map(|id| id.as_str()),
            Some("thread-parent")
        );
    }
}
