//! Port of upstream/apps/web/src/components/chat/ThreadLineageWorkflowRow.tsx
//!
//! Workflow phase timing and status labels, and [`ThreadLineageWorkflowRow`]:
//! the lineage row that unfolds into the phases and members a workflow ran.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, rems,
};

use super::agent_elapsed::AgentElapsed;
use super::subagent_tooltip_content::{
    SubagentTooltipContent, runtime_status_to_turn_item_status, with_subagent_tooltip,
};
use super::thread_details_control::{
    ThreadDetailsControl, ThreadDetailsPart, ThreadDetailsTone, panel_split_group,
    panel_split_separator,
};
use super::thread_details_panel_styles::THREAD_DETAILS_WORKFLOW_INDENT;
use super::thread_relationship_icon::ThreadRelationshipIcon;
use crate::pull_request::presentation_view::Spinner;
use crate::state::subagent_display::{
    ResolveSubagentMetadataInput, resolve_subagent_metadata, subagent_detail_preview,
};
use crate::state::subagent_runtime::{
    PhaseState, RuntimeSubagent, RuntimeSubagentStatus, SubagentWorkflowGroup,
    SubagentWorkflowGroupPhase,
};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{TEXT_2XS, TEXT_SM};
use crate::ui::collapsible_section_header::{
    CollapsibleSectionHeader, CollapsibleSectionTone, CollapsibleSectionVariant,
    SectionHeaderStatus,
};
use crate::ui::{Icon, IconName};

/// Wall clock for the phase: members run in parallel, so the span from the
/// first start to the last finish is the time the phase actually took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhaseElapsed {
    pub status: RuntimeSubagentStatus,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

pub fn phase_elapsed(phase: &SubagentWorkflowGroupPhase) -> PhaseElapsed {
    let instants = |key: fn(&RuntimeSubagent) -> &Option<String>| {
        let mut values: Vec<&str> = phase
            .members
            .iter()
            .filter_map(|member| key(member).as_deref())
            .collect();
        values.sort_unstable();
        values
    };
    let running = phase.state == PhaseState::Running;
    let started_at = instants(|m| &m.started_at).first().map(|s| (*s).to_owned());
    let completed_at = if running {
        None
    } else {
        instants(|m| &m.completed_at)
            .last()
            .map(|s| (*s).to_owned())
    };
    PhaseElapsed {
        status: if running {
            RuntimeSubagentStatus::Running
        } else {
            RuntimeSubagentStatus::Completed
        },
        started_at: if running || completed_at.is_some() {
            started_at
        } else {
            None
        },
        completed_at,
    }
}

/// Phase state for the header's label; tone and member badges show it visually.
pub fn phase_status_label(phase: &SubagentWorkflowGroupPhase) -> &'static str {
    if phase.state == PhaseState::Running {
        return "running";
    }
    if phase
        .members
        .iter()
        .any(|member| member.status == RuntimeSubagentStatus::Failed)
    {
        return "failed";
    }
    if phase.members.iter().any(|member| {
        matches!(
            member.status,
            RuntimeSubagentStatus::Cancelled | RuntimeSubagentStatus::Interrupted
        )
    }) {
        return "stopped";
    }
    if phase.state == PhaseState::Done {
        "done"
    } else {
        "not started"
    }
}

/// The wire spelling of a runtime status, as `ThreadRelationshipIcon` keys it.
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

fn elapsed_for(member: &RuntimeSubagent, compact: bool) -> AgentElapsed {
    AgentElapsed {
        status: member.status,
        started_at: member.started_at.clone(),
        completed_at: member.completed_at.clone(),
        compact,
    }
}

/// `useState` of the row: whether it is open, and which phases the user
/// picked a side for. The lineage list reads `expanded` to trade its compact
/// height for the open tree (`data-workflow-expanded`).
#[derive(Clone, Debug, Default)]
pub struct WorkflowRowState {
    pub expanded: bool,
    /// The running phase opens itself until the user picks a side for it.
    pub phase_open: HashMap<u64, bool>,
}

impl WorkflowRowState {
    /// Closed, unless the lineage demo asks for the open tree.
    pub fn initial() -> Self {
        Self {
            expanded: super::thread_details_demo::lineage_demo_expanded(),
            phase_open: HashMap::new(),
        }
    }
}

pub type OpenThreadHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;
pub type ThreadAvailability = Rc<dyn Fn(&str) -> bool>;
pub type StopHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// What the member rows and their tooltips share from the coordinator.
#[derive(Clone)]
struct MemberContext {
    driver: Option<SharedString>,
    on_open: OpenThreadHandler,
    is_thread_available: ThreadAvailability,
}

/// `WorkflowMemberRow`. Members run under the coordinator's provider, so they
/// share its glyph.
fn workflow_member_row(member: &RuntimeSubagent, context: &MemberContext, cx: &App) -> AnyElement {
    let theme = cx.app_theme().clone();
    let thread_id = member.child_thread_id.clone();
    let can_open = thread_id
        .as_deref()
        .is_some_and(|id| (context.is_thread_available)(id));
    let metadata = resolve_subagent_metadata(&ResolveSubagentMetadataInput {
        model: member.model.as_deref(),
        ..Default::default()
    });
    let tooltip = SubagentTooltipContent::new(
        member.title.clone(),
        runtime_status_to_turn_item_status(member.status),
    )
    .metadata(metadata)
    .driver(context.driver.clone())
    .elapsed(Some(elapsed_for(member, false)))
    .preview(subagent_detail_preview(
        member.status,
        member.result.as_deref().or(member.error.as_deref()),
        member.progress.as_deref(),
    ));
    let on_open = context.on_open.clone();
    let control = ThreadDetailsControl::new(SharedString::from(format!("member-{}", member.id)))
        .part(ThreadDetailsPart::Row)
        .disabled(!can_open)
        .child(ThreadRelationshipIcon {
            driver: context.driver.clone(),
            status: Some(runtime_status_wire(member.status).to_owned()),
            fallback: IconName::Bot,
        })
        .child(
            div()
                .min_w_0()
                .flex_1()
                .truncate()
                .child(member.title.clone()),
        )
        .when(member.started_at.is_some(), |this| {
            this.child(
                div()
                    .flex_shrink_0()
                    .text_size(TEXT_2XS.size)
                    .font_weight(FontWeight::NORMAL)
                    .text_color(theme.muted_foreground)
                    .child(elapsed_for(member, false)),
            )
        })
        .on_click(move |_, window, cx| {
            if let Some(id) = &thread_id {
                on_open(id, window, cx);
            }
        });
    with_subagent_tooltip(
        SharedString::from(format!("member-tip-{}", member.id)),
        tooltip,
        control,
    )
    .into_any_element()
}

#[derive(IntoElement)]
pub struct ThreadLineageWorkflowRow {
    pub id: SharedString,
    pub group: SubagentWorkflowGroup,
    /// The coordinator's own lineage link: the leading half of the split row.
    pub header: AnyElement,
    pub state: Entity<WorkflowRowState>,
    /// The provider driver kind behind the members' glyph.
    pub driver: Option<SharedString>,
    pub on_open_thread: OpenThreadHandler,
    pub is_thread_available: ThreadAvailability,
    pub on_stop: Option<StopHandler>,
    pub stopping: bool,
    pub stop_disabled: bool,
}

impl RenderOnce for ThreadLineageWorkflowRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let WorkflowRowState {
            expanded,
            phase_open,
        } = self.state.read(cx).clone();
        let group_name = SharedString::from(format!("workflow-row-{}", self.id));
        let phases: Vec<&SubagentWorkflowGroupPhase> = self
            .group
            .phases
            .iter()
            .filter(|phase| !phase.members.is_empty())
            .collect();
        let context = MemberContext {
            driver: self.driver.clone(),
            on_open: self.on_open_thread.clone(),
            is_thread_available: self.is_thread_available.clone(),
        };
        let stop_overlay = self.on_stop.clone().map(|on_stop| {
            // Revealed on row hover (`group-hover:opacity-100`); hidden
            // elements take no clicks, which `pointer-events-none` achieves
            // upstream.
            div()
                .absolute()
                .right(rems(2.25))
                .top_0()
                .bottom_0()
                .flex()
                .items_center()
                .invisible()
                .group_hover(group_name.clone(), |style| style.visible())
                .child(with_text_tooltip(
                    SharedString::from(format!("workflow-stop-{}", self.id)),
                    "Stop entire workflow",
                    ThreadDetailsControl::new(SharedString::from(format!(
                        "workflow-stop-btn-{}",
                        self.id
                    )))
                    .part(ThreadDetailsPart::Icon)
                    .tone(ThreadDetailsTone::Destructive)
                    .disabled(self.stop_disabled)
                    .child(if self.stopping {
                        Spinner::new(format!("workflow-stop-{}", self.id))
                            .size(rems(0.75))
                            .color(theme.destructive_foreground)
                            .into_any_element()
                    } else {
                        Icon::new(IconName::Square)
                            .size(rems(0.75))
                            .color(theme.destructive_foreground)
                            .into_any_element()
                    })
                    .on_click(move |_, window, cx| on_stop(window, cx)),
                ))
        });
        let state = self.state.clone();
        let toggle =
            ThreadDetailsControl::new(SharedString::from(format!("workflow-toggle-{}", self.id)))
                .part(ThreadDetailsPart::Secondary)
                .child(
                    Icon::new(IconName::ChevronDown)
                        .size(rems(0.875))
                        .color(theme.muted_foreground)
                        .rotate_180(expanded),
                )
                .on_click(move |_, _, cx| {
                    state.update(cx, |state, cx| {
                        state.expanded = !state.expanded;
                        cx.notify();
                    });
                });
        let split = panel_split_group(
            SharedString::from(format!("workflow-split-{}", self.id)),
            false,
            &theme,
        )
        .relative()
        .child(self.header)
        .children(stop_overlay)
        .child(panel_split_separator(&theme))
        .child(toggle);
        let tree = expanded.then(|| {
            let mut list = div().flex().flex_col().pl(THREAD_DETAILS_WORKFLOW_INDENT);
            for phase in &phases {
                let open = phase_open
                    .get(&phase.index)
                    .copied()
                    .unwrap_or(phase.state == PhaseState::Running);
                let status = phase_status_label(phase);
                let count_label = format!(
                    "{}/{} {}",
                    phase.settled_count,
                    phase.members.len(),
                    if phase.members.len() == 1 {
                        "agent"
                    } else {
                        "agents"
                    }
                );
                let elapsed = phase_elapsed(phase);
                let elapsed = AgentElapsed {
                    status: elapsed.status,
                    started_at: elapsed.started_at,
                    completed_at: elapsed.completed_at,
                    compact: false,
                };
                let accessory = div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(status == "failed", |this| {
                        this.child(SectionHeaderStatus("Failed".into()))
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(TEXT_2XS.size)
                            .font_weight(FontWeight::NORMAL)
                            .child(count_label),
                    )
                    .when(elapsed.has_value(), |this| {
                        this.child(
                            div()
                                .flex_shrink_0()
                                .text_size(TEXT_2XS.size)
                                .font_weight(FontWeight::NORMAL)
                                .child(elapsed),
                        )
                    });
                let phase_index = phase.index;
                let state = self.state.clone();
                list = list.child(
                    div()
                        .child(
                            CollapsibleSectionHeader::new(
                                SharedString::from(format!("{}-phase-{}", self.id, phase.index)),
                                phase.title.clone(),
                                open,
                            )
                            .variant(CollapsibleSectionVariant::Panel)
                            .tone(if phase.state == PhaseState::Running {
                                CollapsibleSectionTone::Emphasized
                            } else {
                                CollapsibleSectionTone::Muted
                            })
                            .accessory(accessory)
                            .on_click(move |_, _, cx| {
                                state.update(cx, |state, cx| {
                                    state.phase_open.insert(phase_index, !open);
                                    cx.notify();
                                });
                            }),
                        )
                        .when(open, |this| {
                            this.child(
                                div().flex().flex_col().children(
                                    phase
                                        .members
                                        .iter()
                                        .map(|member| workflow_member_row(member, &context, cx)),
                                ),
                            )
                        }),
                );
            }
            for member in &self.group.unphased_members {
                list = list.child(workflow_member_row(member, &context, cx));
            }
            if phases.is_empty() && self.group.unphased_members.is_empty() {
                list = list.child(
                    div()
                        .px(rems(0.625))
                        .py(rems(0.375))
                        .text_size(TEXT_2XS.size)
                        .text_color(with_alpha(theme.muted_foreground, 0.7))
                        .child("No agents yet"),
                );
            }
            list
        });
        div()
            .group(group_name)
            .flex()
            .flex_col()
            .text_size(TEXT_SM.size)
            .child(split)
            .children(tree)
    }
}

/// A plain-text `TooltipPopup` over `child`.
pub fn with_text_tooltip(
    id: impl Into<gpui_kit::ElementId>,
    text: &'static str,
    child: impl IntoElement,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .child(child)
        .tooltip(move |window, cx| crate::ui::tooltip::Tooltip::new(text).build(window, cx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::subagent_runtime::PhaseState;

    fn member(
        started: &str,
        completed: Option<&str>,
        status: RuntimeSubagentStatus,
    ) -> RuntimeSubagent {
        use crate::state::subagent_runtime::RuntimeSubagentKind;
        RuntimeSubagent {
            id: "m".into(),
            kind: RuntimeSubagentKind::WorkflowAgent,
            title: "Member".into(),
            role: None,
            model: None,
            effort: None,
            status,
            activation_count: 0,
            usage: None,
            progress: None,
            last_tool_name: None,
            result: None,
            error: None,
            output_file: None,
            parent_agent_id: None,
            agent_index: None,
            phase_index: None,
            phase_title: None,
            attempt: None,
            workflow_name: None,
            phases: Vec::new(),
            run_handles: None,
            child_thread_id: None,
            recent_activity: Vec::new(),
            first_seen_at: started.into(),
            started_at: Some(started.into()),
            completed_at: completed.map(str::to_owned),
            updated_at: started.into(),
        }
    }

    #[test]
    fn phase_elapsed_uses_first_start_and_last_finish() {
        let phase = SubagentWorkflowGroupPhase {
            index: 0,
            title: "Inspect".into(),
            members: vec![
                member(
                    "2026-01-01T12:00:00Z",
                    Some("2026-01-01T12:05:00Z"),
                    RuntimeSubagentStatus::Completed,
                ),
                member(
                    "2026-01-01T12:01:00Z",
                    Some("2026-01-01T12:10:00Z"),
                    RuntimeSubagentStatus::Completed,
                ),
            ],
            state: PhaseState::Done,
            settled_count: 2,
        };
        let elapsed = phase_elapsed(&phase);
        assert_eq!(elapsed.started_at.as_deref(), Some("2026-01-01T12:00:00Z"));
        assert_eq!(
            elapsed.completed_at.as_deref(),
            Some("2026-01-01T12:10:00Z")
        );
    }

    #[test]
    fn running_phase_has_no_completed_at() {
        let phase = SubagentWorkflowGroupPhase {
            index: 0,
            title: "Run".into(),
            members: vec![member(
                "2026-01-01T12:00:00Z",
                None,
                RuntimeSubagentStatus::Running,
            )],
            state: PhaseState::Running,
            settled_count: 0,
        };
        let elapsed = phase_elapsed(&phase);
        assert_eq!(elapsed.status, RuntimeSubagentStatus::Running);
        assert!(elapsed.completed_at.is_none());
    }

    #[test]
    fn phase_status_label_prefers_failed_members() {
        let phase = SubagentWorkflowGroupPhase {
            index: 0,
            title: "Run".into(),
            members: vec![member(
                "2026-01-01T12:00:00Z",
                Some("2026-01-01T12:01:00Z"),
                RuntimeSubagentStatus::Failed,
            )],
            state: PhaseState::Done,
            settled_count: 1,
        };
        assert_eq!(phase_status_label(&phase), "failed");
    }
}
