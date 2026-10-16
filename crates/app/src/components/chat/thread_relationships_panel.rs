//! Port of upstream/apps/web/src/components/chat/ThreadRelationshipsControl.tsx
//! (`ThreadRelationshipsPanel` and helpers).

use std::collections::{HashMap, HashSet};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px, rems,
};

use super::agent_elapsed::AgentElapsed;
use super::subagent_tooltip_content::SubagentTooltipContent;
use super::thread_details_control::{
    ThreadDetailsControl, ThreadDetailsControlPart, ThreadDetailsControlTone,
    thread_details_split_group,
};
use super::thread_details_demo::LineagePanelSnapshot;
use super::thread_details_panel_styles::{
    THREAD_DETAILS_LINEAGE_MAX_HEIGHT, THREAD_DETAILS_META_SIZE,
};
use super::thread_details_section::ThreadDetailsSection;
use super::thread_relationship_icon::{ThreadRelationshipIcon, thread_relationship_status_label};
use super::thread_relationships_control::{
    THREAD_LINEAGE_INITIAL_COUNT, THREAD_LINEAGE_PAGE_COUNT, ThreadLineageGroupId,
    group_thread_lineage_rows, relationship_label, relationship_thread_title,
    resolve_thread_lineage_window, thread_lineage_show_more_label,
};
use crate::state::contracts::OrchestrationV2TurnItemStatus;
use crate::state::subagent_runtime::{RuntimeSubagentStatus, SubagentWorkflowGroup};
use crate::state::thread_relationships::{
    ThreadRelationshipKind, is_parent_thread_relationship, thread_relationship_row_status,
};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::TEXT_SM;
use crate::ui::{Icon, IconName};

pub struct ThreadRelationshipsPanel {
    snapshot: LineagePanelSnapshot,
    visible_count: usize,
    previous_expanded: bool,
    workflow_expanded: HashSet<String>,
    #[allow(dead_code)]
    phase_open: HashMap<u64, bool>,
}

impl ThreadRelationshipsPanel {
    pub fn new(snapshot: LineagePanelSnapshot) -> Self {
        Self {
            snapshot,
            visible_count: THREAD_LINEAGE_INITIAL_COUNT,
            previous_expanded: false,
            workflow_expanded: HashSet::new(),
            phase_open: HashMap::new(),
        }
    }

    fn workflow_group_for_agent(&self, agent_id: &str) -> Option<&SubagentWorkflowGroup> {
        self.snapshot
            .workflow_groups
            .iter()
            .find(|group| group.workflow.id == agent_id)
    }

    fn running_count(&self) -> usize {
        self.snapshot
            .rows
            .iter()
            .filter(|row| {
                row.edge.kind == ThreadRelationshipKind::Subagent
                    && !is_parent_thread_relationship(&row.edge, &self.snapshot.current_thread_id)
                    && matches!(
                        row.edge.status.as_deref(),
                        Some("running" | "in_progress" | "pending" | "waiting")
                    )
            })
            .count()
    }
}

impl Render for ThreadRelationshipsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.snapshot.current_thread_id.clone();
        let running = self.running_count();
        let title = if running > 0 {
            SharedString::from(format!("Lineage · {running} running"))
        } else {
            SharedString::from("Lineage")
        };
        let (related, active, previous) = group_thread_lineage_rows(&self.snapshot.rows, &current);
        let groups = [
            (ThreadLineageGroupId::Related, None, related, true),
            (ThreadLineageGroupId::Active, None, active, true),
            (
                ThreadLineageGroupId::Previous,
                Some("Previous agents"),
                previous,
                self.previous_expanded,
            ),
        ];
        ThreadDetailsSection::new(title)
            .child(
                div()
                    .id("thread-relationships-panel")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(
                        groups
                            .into_iter()
                            .filter_map(|(id, label, rows, expanded)| {
                                if rows.is_empty() {
                                    return None;
                                }
                                let label = label.map(SharedString::from);
                                let row_count = rows.len();
                                Some(
                                    self.render_group(
                                        window, cx, id, label, rows, expanded, row_count,
                                    ),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }
}

impl ThreadRelationshipsPanel {
    #[allow(clippy::too_many_arguments)]
    fn render_group(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        id: ThreadLineageGroupId,
        label: Option<SharedString>,
        rows: Vec<crate::state::thread_relationships::ThreadRelationshipWalkRow>,
        expanded: bool,
        row_count: usize,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let visible_count = if id == ThreadLineageGroupId::Previous {
            self.visible_count
        } else {
            rows.len()
        };
        let windowed = resolve_thread_lineage_window(&rows, visible_count);
        let header = label.map(|label| {
            div()
                .flex()
                .min_h(rems(2.))
                .items_center()
                .justify_between()
                .px(px(6.))
                .child(
                    div()
                        .text_size(rems(0.6875))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted_foreground)
                        .child(if expanded {
                            label
                        } else {
                            format!("{label} ({row_count})").into()
                        }),
                )
                .into_any_element()
        });
        div()
            .flex()
            .flex_col()
            .when(!expanded, |this| this)
            .when(expanded, |this| {
                this.child(
                    div()
                        .id(("lineage-list", lineage_group_index(id) as usize))
                        .flex()
                        .flex_col()
                        .max_h(THREAD_DETAILS_LINEAGE_MAX_HEIGHT)
                        .overflow_y_scroll()
                        .children(
                            windowed
                                .visible_rows
                                .into_iter()
                                .map(|row| self.render_row(window, cx, row)),
                        )
                        .when(windowed.hidden_count > 0, |list| {
                            list.child(
                                ThreadDetailsControl::new((
                                    "show-more",
                                    lineage_group_index(id) as usize,
                                ))
                                .part(ThreadDetailsControlPart::Row)
                                .tone(ThreadDetailsControlTone::Muted)
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            Icon::new(IconName::Plus)
                                                .size(rems(1.))
                                                .color(theme.muted_foreground),
                                        )
                                        .child(thread_lineage_show_more_label(
                                            windowed.hidden_count,
                                        )),
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.visible_count += THREAD_LINEAGE_PAGE_COUNT;
                                        cx.notify();
                                    },
                                )),
                            )
                        }),
                )
            })
            .when(id == ThreadLineageGroupId::Previous, |this| {
                this.on_mouse_down(
                    gpui_kit::MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.previous_expanded = !this.previous_expanded;
                        cx.notify();
                    }),
                )
            })
            .children(header)
            .into_any_element()
    }

    fn render_row(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        row: crate::state::thread_relationships::ThreadRelationshipWalkRow,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let current = &self.snapshot.current_thread_id;
        let thread_id = row.thread_id.clone();
        let node = self.snapshot.graph.nodes.get(&thread_id);
        let edge = row.edge.clone();
        let is_subagent = edge.kind == ThreadRelationshipKind::Subagent;
        let is_parent = is_parent_thread_relationship(&edge, current);
        let row_status = thread_relationship_row_status(&self.snapshot.graph, &row);
        let status_wire = row_status.as_deref().or(edge.status.as_deref());
        let agent = (!is_parent)
            .then(|| {
                self.snapshot
                    .subagents_by_child_thread
                    .get(thread_id.as_str())
            })
            .flatten();
        let workflow_group = agent.and_then(|a| self.workflow_group_for_agent(&a.id));
        let title = relationship_thread_title(
            workflow_group
                .and_then(|g| {
                    g.workflow
                        .workflow_name
                        .as_deref()
                        .or(Some(g.workflow.title.as_str()))
                })
                .or(node.and_then(|n| n.thread.as_ref().map(|t| t.title.as_str())))
                .or(agent.map(|a| a.title.as_str()))
                .unwrap_or(thread_id.as_str()),
            is_subagent,
        );
        let relationship = relationship_label(&edge, current);
        let icon = if is_parent {
            IconName::ArrowUpRight
        } else if is_subagent {
            IconName::Bot
        } else {
            IconName::GitFork
        };
        let status_for_tooltip = agent
            .map(|a| runtime_to_turn_status(a.status))
            .unwrap_or(OrchestrationV2TurnItemStatus::Running);
        let header = self.relationship_link(
            window,
            cx,
            &title,
            &relationship,
            status_wire,
            icon,
            if workflow_group.is_some() {
                ThreadDetailsControlPart::Primary
            } else {
                ThreadDetailsControlPart::Row
            },
            SubagentTooltipContent::new(title.clone(), status_for_tooltip)
                .model_label(SharedString::from("Composer"))
                .preview(agent.and_then(|a| a.progress.clone()))
                .elapsed(agent.map(|a| {
                    AgentElapsed {
                        status: a.status,
                        started_at: a.started_at.clone(),
                        completed_at: a.completed_at.clone(),
                        compact: true,
                    }
                    .into_any_element()
                })),
        );
        if let Some(group) = workflow_group {
            let workflow_id = group.workflow.id.clone();
            let expanded = self.workflow_expanded.contains(&workflow_id);
            return div()
                .flex()
                .flex_col()
                .child(thread_details_split_group(vec![
                    header,
                    ThreadDetailsControl::new(("workflow-toggle", workflow_id.len()))
                        .part(ThreadDetailsControlPart::Secondary)
                        .child(
                            Icon::new(if expanded {
                                IconName::ChevronUp
                            } else {
                                IconName::ChevronDown
                            })
                            .size(rems(0.875))
                            .color(theme.muted_foreground),
                        )
                        .on_click({
                            let id = workflow_id.clone();
                            cx.listener(move |this, _, _, cx| {
                                if this.workflow_expanded.contains(&id) {
                                    this.workflow_expanded.remove(&id);
                                } else {
                                    this.workflow_expanded.insert(id.clone());
                                }
                                cx.notify();
                            })
                        })
                        .into_any_element(),
                ]))
                .when(expanded, |this| {
                    this.child(
                        div()
                            .pl(super::thread_details_panel_styles::THREAD_DETAILS_WORKFLOW_INDENT)
                            .children(group.phases.iter().map(|phase| {
                                div().py_1().child(phase.title.clone()).into_any_element()
                            })),
                    )
                })
                .into_any_element();
        }
        div()
            .group("lineage-row")
            .relative()
            .flex()
            .h(rems(2.))
            .items_center()
            .rounded(rems(0.5))
            .child(header)
            .when(
                status_wire.is_some_and(|s| s == "failed" || s == "error"),
                |this| {
                    this.child(
                        div()
                            .ml_auto()
                            .text_size(THREAD_DETAILS_META_SIZE)
                            .text_color(theme.destructive)
                            .child(thread_relationship_status_label(status_wire)),
                    )
                },
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn relationship_link(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
        title: &str,
        relationship: &str,
        status: Option<&str>,
        icon: IconName,
        part: ThreadDetailsControlPart,
        _tooltip: SubagentTooltipContent,
    ) -> AnyElement {
        let _ = relationship;
        ThreadDetailsControl::new(("lineage-row", title.len()))
            .part(part)
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .flex_1()
                    .items_center()
                    .gap_2()
                    .child(ThreadRelationshipIcon {
                        status: status.map(str::to_owned),
                        fallback: icon,
                    })
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(TEXT_SM.size)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(with_alpha(_cx.app_theme().foreground, 0.85))
                            .child(title.to_owned()),
                    ),
            )
            .into_any_element()
    }
}

fn lineage_group_index(id: ThreadLineageGroupId) -> u8 {
    match id {
        ThreadLineageGroupId::Related => 0,
        ThreadLineageGroupId::Active => 1,
        ThreadLineageGroupId::Previous => 2,
    }
}

fn runtime_to_turn_status(status: RuntimeSubagentStatus) -> OrchestrationV2TurnItemStatus {
    match status {
        RuntimeSubagentStatus::Pending => OrchestrationV2TurnItemStatus::Pending,
        RuntimeSubagentStatus::Running => OrchestrationV2TurnItemStatus::Running,
        RuntimeSubagentStatus::Waiting => OrchestrationV2TurnItemStatus::Waiting,
        RuntimeSubagentStatus::Completed => OrchestrationV2TurnItemStatus::Completed,
        RuntimeSubagentStatus::Failed => OrchestrationV2TurnItemStatus::Failed,
        RuntimeSubagentStatus::Cancelled => OrchestrationV2TurnItemStatus::Cancelled,
        RuntimeSubagentStatus::Interrupted => OrchestrationV2TurnItemStatus::Interrupted,
        RuntimeSubagentStatus::Idle => OrchestrationV2TurnItemStatus::Idle,
    }
}
