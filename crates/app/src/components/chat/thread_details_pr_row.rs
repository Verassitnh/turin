//! Port of upstream/apps/web/src/components/chat/ThreadDetailsPrRow.tsx
//!
//! The thread details panel's pull request row: what the thread's pull request is, and the one
//! thing worth doing to it right now.
//!
//! The row itself opens the pull request in the right panel. Once the host answers, the title
//! gets the full first line and a second line carries the number, checks, watch, and a single
//! action slot, ranked by what unblocks the merge next: "Resolve" on conflicts, "Ready" on a
//! draft, "Fix" under failing checks, and "Merge" only once the branch is clean and its checks
//! pass. While checks run the slot reports that instead. Everything else the host knows lives in
//! the row's tooltip. Until the detail arrives the row renders from the linked snapshot, or just
//! the link.
//!
//! Upstream passes the whole `ThreadPr` (the checkout's change request) but the row only reads its
//! `state`, so [`ThreadDetailsPrRow::pr_state`] carries just that.
//!
//! The row reads a resolved `PullRequestDetail` and reports through [`PrRowActions`]; the owner
//! runs the host queries, the action runner and the handoffs (`usePullRequestActionRunner`,
//! `usePullRequestHandoffs`).

use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, px, rems,
};
use turin_contracts::git::GitPullRequestState;
use turin_contracts::pull_request::{
    PullRequestCheckStatus, PullRequestChecksState as RollupState, PullRequestDetail,
    PullRequestMergeMethod, PullRequestState,
};

use super::thread_details_control::{
    ThreadDetailsControl, ThreadDetailsPart, ThreadDetailsTone, panel_split_group,
    panel_split_separator,
};
use crate::components::sidebar::thread_hover_card::thread_hover_card_popup;
use crate::components::sidebar::thread_status_indicators::{
    PrStatusIndicator, pull_request_state_tone,
};
use crate::pull_request::checks_popover::PullRequestChecksPopover;
use crate::pull_request::presentation_view::{
    PullRequestCheckStatusIcon, PullRequestDiffStat, glyph_icon, tone_color,
};
use crate::pull_request::pull_request_detail_logic::{
    PullRequestChecksState, ThreadPanelPullRequestAction, allowed_pull_request_merge_methods,
    classify_pull_request_checks, describe_pull_request_checks, is_pull_request_conflicting,
    resolve_selected_merge_method, resolve_thread_panel_pull_request_action,
};
use crate::pull_request::pull_request_presentation::{
    pull_request_checks_state, resolve_pull_request_state,
};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{TEXT_XS, tw};
use crate::ui::tooltip::Tooltip;
use crate::ui::{Icon, IconName};

/// Which handoff thread is being prepared (`handoff` of `usePullRequestHandoffs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrHandoff {
    Conflicts,
    Findings,
}

type RowHandler = Rc<dyn Fn(&mut Window, &mut App)>;
/// Opens the pull request (`onOpen`).
pub type OpenHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type MergeHandler = Rc<dyn Fn(PullRequestMergeMethod, &mut Window, &mut App)>;

/// The commands behind the row's action slot. Unset handlers make the action inert.
#[derive(Clone, Default)]
pub struct PrRowActions {
    pub ready: Option<RowHandler>,
    pub merge: Option<MergeHandler>,
    pub resolve_conflicts: Option<RowHandler>,
    pub fix_checks: Option<RowHandler>,
}

/// How the row lays out: the title keeps the full width on a first line, with the number, checks,
/// watch and action beneath it, or it is a single (optionally split) line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrRowLayout {
    TwoLine,
    Split,
    Single,
}

/// What the row derives from the host's detail.
#[derive(Clone, Debug, PartialEq)]
pub struct PrRowModel {
    pub row_action: Option<ThreadPanelPullRequestAction>,
    pub conflicting: bool,
    pub checks_state: PullRequestChecksState,
    pub checks_rollup: Option<RollupState>,
    pub show_checks: bool,
    pub watching: bool,
    pub layout: PrRowLayout,
    pub selected_merge_method: PullRequestMergeMethod,
}

/// `state` is the linked pull request's state while the detail has not arrived (`pr?.state`).
pub fn resolve_pr_row_model(
    detail: Option<&PullRequestDetail>,
    linked_state: Option<GitPullRequestState>,
    can_stop_watching: bool,
) -> PrRowModel {
    let row_action = resolve_thread_panel_pull_request_action(detail);
    let conflicting = is_pull_request_conflicting(detail);
    let checks_state = detail.map_or(PullRequestChecksState::None, |detail| {
        classify_pull_request_checks(&detail.checks)
    });
    let checks_rollup = detail.and_then(|detail| pull_request_checks_state(&detail.checks));
    let selected_merge_method = resolve_selected_merge_method(
        &allowed_pull_request_merge_methods(detail),
        PullRequestMergeMethod::Merge,
    );
    // The server ends a watch when the pull request closes, so only an open one shows the eye.
    let open = match (detail, linked_state) {
        (Some(detail), _) => detail.state == PullRequestState::Open,
        (None, Some(state)) => state == GitPullRequestState::Open,
        (None, None) => true,
    };
    let watching = can_stop_watching && open;
    let show_checks =
        detail.is_some_and(|detail| checks_rollup.is_some() && !conflicting && !detail.is_draft);
    let layout = if detail.is_some() && (show_checks || row_action.is_some() || watching) {
        PrRowLayout::TwoLine
    } else if watching {
        PrRowLayout::Split
    } else {
        PrRowLayout::Single
    };
    PrRowModel {
        row_action,
        conflicting,
        checks_state,
        checks_rollup,
        show_checks,
        watching,
        layout,
        selected_merge_method,
    }
}

/// The merge confirmation only stays up while Merge is still the row's action; once checks go
/// pending (or anything else changes the offer) it closes and needs a new click.
pub fn keep_confirming_merge(
    confirming: bool,
    row_action: Option<ThreadPanelPullRequestAction>,
) -> bool {
    confirming && row_action == Some(ThreadPanelPullRequestAction::Merge)
}

/// The label of a merge method as the confirmation names it.
fn merge_method_wire(method: PullRequestMergeMethod) -> &'static str {
    match method {
        PullRequestMergeMethod::Merge => "merge",
        PullRequestMergeMethod::Squash => "squash",
        PullRequestMergeMethod::Rebase => "rebase",
    }
}

struct TrailingAction {
    label: &'static str,
    pending_label: &'static str,
    pending: bool,
    destructive: bool,
    suffix: bool,
    tooltip: String,
}

#[derive(IntoElement)]
pub struct ThreadDetailsPrRow {
    pub id: SharedString,
    /// `pr?.state`: the linked change request's state until the host's detail arrives.
    pub pr_state: Option<GitPullRequestState>,
    pub number: u64,
    pub status: Option<PrStatusIndicator>,
    pub detail: Option<PullRequestDetail>,
    pub label: SharedString,
    pub on_open: OpenHandler,
    /// Set while the server watches this pull request for the thread; stops the watch.
    pub on_stop_watching: Option<RowHandler>,
    pub action_pending: bool,
    pub handoff: Option<PrHandoff>,
    pub actions: PrRowActions,
}

impl ThreadDetailsPrRow {
    pub fn new(
        id: impl Into<SharedString>,
        number: u64,
        label: impl Into<SharedString>,
        on_open: OpenHandler,
    ) -> Self {
        Self {
            id: id.into(),
            pr_state: None,
            number,
            status: None,
            detail: None,
            label: label.into(),
            on_open,
            on_stop_watching: None,
            action_pending: false,
            handoff: None,
            actions: PrRowActions::default(),
        }
    }

    pub fn pr_state(mut self, state: Option<GitPullRequestState>) -> Self {
        self.pr_state = state;
        self
    }

    pub fn status(mut self, status: Option<PrStatusIndicator>) -> Self {
        self.status = status;
        self
    }

    pub fn detail(mut self, detail: Option<PullRequestDetail>) -> Self {
        self.detail = detail;
        self
    }

    pub fn on_stop_watching(mut self, handler: Option<RowHandler>) -> Self {
        self.on_stop_watching = handler;
        self
    }

    #[allow(dead_code)]
    pub fn action_pending(mut self, pending: bool) -> Self {
        self.action_pending = pending;
        self
    }

    #[allow(dead_code)]
    pub fn handoff(mut self, handoff: Option<PrHandoff>) -> Self {
        self.handoff = handoff;
        self
    }

    pub fn actions(mut self, actions: PrRowActions) -> Self {
        self.actions = actions;
        self
    }
}

impl RenderOnce for ThreadDetailsPrRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let model = resolve_pr_row_model(
            self.detail.as_ref(),
            self.pr_state,
            self.on_stop_watching.is_some(),
        );
        let confirming = window.use_keyed_state(
            SharedString::from(format!("pr-row-confirm-{}", self.id)),
            cx,
            |_, _| false,
        );
        if *confirming.read(cx) && !keep_confirming_merge(true, model.row_action) {
            confirming.update(cx, |confirming, _| *confirming = false);
            window.close_dialog(cx);
        }

        let state_presentation = self
            .detail
            .as_ref()
            .map(|detail| resolve_pull_request_state(detail.state, detail.is_draft));
        // Host details distinguish drafts; all panels share the same PR-state glyph.
        let icon: AnyElement = match (&state_presentation, self.pr_state, &self.status) {
            (Some(presentation), _, _) => Icon::new(glyph_icon(presentation.glyph))
                .size(rems(1.))
                .color(tone_color(presentation.tone_class_name, &theme))
                .into_any_element(),
            (None, Some(_), Some(status)) => Icon::new(status.icon)
                .size(rems(1.))
                .color(pull_request_state_tone(status.state, &theme))
                .into_any_element(),
            _ => Icon::new(IconName::GitPullRequestArrow)
                .size(rems(1.))
                .color(theme.muted_foreground)
                .into_any_element(),
        };

        let row_tooltip = self.row_tooltip(&model, &theme);
        let open_row = |text: SharedString, part: ThreadDetailsPart, icon: AnyElement| {
            let on_open = self.on_open.clone();
            let control =
                ThreadDetailsControl::new(SharedString::from(format!("{}-open", self.id)))
                    .part(part)
                    .child(icon)
                    .child(div().min_w_0().flex_1().truncate().child(text))
                    .on_click(move |event, window, cx| on_open(event, window, cx));
            let tooltip = row_tooltip.clone();
            div()
                .id(SharedString::from(format!("{}-open-tip", self.id)))
                .flex()
                .min_w_0()
                .when(part == ThreadDetailsPart::Primary, |this| this.flex_1())
                .when(part == ThreadDetailsPart::Row, |this| this.w_full())
                .child(control)
                .tooltip(move |window, cx| tooltip.build(window, cx))
        };

        let watch_control = |part: ThreadDetailsPart, theme: &crate::theme::AppTheme| {
            let on_stop = self.on_stop_watching.clone()?;
            let group = SharedString::from(format!("{}-watch", self.id));
            let size = if part == ThreadDetailsPart::Icon {
                rems(0.875)
            } else {
                rems(1.)
            };
            let control =
                ThreadDetailsControl::new(SharedString::from(format!("{}-watch-btn", self.id)))
                    .part(part)
                    .child(
                        div()
                            .group(group.clone())
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .group_hover(group.clone(), |style| style.hidden())
                                    .child(
                                        Icon::new(IconName::Eye)
                                            .size(size)
                                            .color(theme.muted_foreground),
                                    ),
                            )
                            .child(
                                div()
                                    .hidden()
                                    .group_hover(group.clone(), |style| style.block())
                                    .child(
                                        Icon::new(IconName::EyeOff)
                                            .size(size)
                                            .color(theme.muted_foreground),
                                    ),
                            ),
                    )
                    .on_click(move |_, window, cx| on_stop(window, cx));
            Some(
                div()
                    .id(SharedString::from(format!("{}-watch-tip", self.id)))
                    .child(control)
                    .tooltip(|window, cx| {
                        Tooltip::new(
                            "Watching: the agent wakes when checks finish, someone comments, or the branch conflicts. Click to stop.",
                        )
                        .build(window, cx)
                    })
                    .into_any_element(),
            )
        };

        let trailing_action = self.trailing_action(&model);
        let actions = self.actions.clone();
        let content: AnyElement = match (model.layout, self.detail.as_ref()) {
            (PrRowLayout::TwoLine, Some(detail)) => {
                // Two lines so the title keeps the full row width: the number, checks, watch,
                // and the next action sit underneath, aligned with the title text. The second
                // line hugs its title and the block keeps space below, so stacked pull requests
                // read as separate pairs.
                let checks = (model.show_checks)
                    .then(|| {
                        model.checks_rollup.map(|rollup| {
                            PullRequestChecksPopover::new(
                                SharedString::from(format!("{}-checks", self.id)),
                                detail.checks.clone(),
                                rollup,
                            )
                            .count_variant(true)
                            .into_any_element()
                        })
                    })
                    .flatten();
                let action_button = trailing_action.map(|action| {
                    let on_click: Option<RowHandler> = match model.row_action {
                        Some(ThreadPanelPullRequestAction::Resolve) => {
                            actions.resolve_conflicts.clone()
                        }
                        Some(ThreadPanelPullRequestAction::Ready) => actions.ready.clone(),
                        Some(ThreadPanelPullRequestAction::Fix) => actions.fix_checks.clone(),
                        Some(ThreadPanelPullRequestAction::Merge) | None => None,
                    };
                    let is_merge = model.row_action == Some(ThreadPanelPullRequestAction::Merge);
                    let confirming = confirming.clone();
                    let merge = actions.merge.clone();
                    let method = model.selected_merge_method;
                    let number = self.number;
                    let tooltip = action.tooltip.clone();
                    let control = ThreadDetailsControl::new(SharedString::from(format!(
                        "{}-action",
                        self.id
                    )))
                    .part(ThreadDetailsPart::Meta)
                    .tone(if action.destructive {
                        ThreadDetailsTone::Destructive
                    } else {
                        ThreadDetailsTone::Primary
                    })
                    .disabled(self.action_pending || self.handoff.is_some())
                    .child(if action.pending {
                        action.pending_label
                    } else {
                        action.label
                    })
                    .when(action.suffix, |this| {
                        this.child(
                            Icon::new(IconName::ArrowUpRight)
                                .size(rems(0.75))
                                .color(theme.destructive_foreground),
                        )
                    })
                    .on_click(move |_, window, cx| {
                        if is_merge {
                            confirming.update(cx, |confirming, _| *confirming = true);
                            open_merge_dialog(
                                number,
                                method,
                                merge.clone(),
                                confirming.clone(),
                                window,
                                cx,
                            );
                        } else if let Some(on_click) = &on_click {
                            on_click(window, cx);
                        }
                    });
                    div()
                        .id(SharedString::from(format!("{}-action-tip", self.id)))
                        .child(control)
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                        .into_any_element()
                });
                div()
                    .pb_2()
                    .child(open_row(
                        detail.title.as_str().to_owned().into(),
                        ThreadDetailsPart::Row,
                        icon,
                    ))
                    .child(
                        div()
                            .flex()
                            .h(rems(1.5))
                            .min_w_0()
                            .items_center()
                            .gap(px(2.))
                            // `-mt-1`, `ps-9 pe-1`.
                            .mt(px(-4.))
                            .pl(rems(2.25))
                            .pr_1()
                            .text_size(TEXT_XS.size)
                            .text_color(theme.muted_foreground)
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .mr_1()
                                    .child(format!("#{}", self.number)),
                            )
                            .children(checks)
                            .children(watch_control(ThreadDetailsPart::Icon, &theme))
                            .child(div().flex_1())
                            .children(action_button),
                    )
                    .into_any_element()
            }
            (PrRowLayout::Split, _) => panel_split_group(
                SharedString::from(format!("{}-split", self.id)),
                false,
                &theme,
            )
            .child(open_row(
                self.label.clone(),
                ThreadDetailsPart::Primary,
                icon,
            ))
            .child(panel_split_separator(&theme))
            .children(watch_control(ThreadDetailsPart::Secondary, &theme))
            .into_any_element(),
            _ => open_row(self.label.clone(), ThreadDetailsPart::Row, icon).into_any_element(),
        };
        content
    }
}

impl ThreadDetailsPrRow {
    fn trailing_action(&self, model: &PrRowModel) -> Option<TrailingAction> {
        let action = model.row_action?;
        Some(match action {
            ThreadPanelPullRequestAction::Resolve => TrailingAction {
                label: "Resolve",
                pending_label: "Preparing...",
                pending: self.handoff == Some(PrHandoff::Conflicts),
                destructive: true,
                suffix: true,
                tooltip: "Check the branch out and resolve the conflicts in a new thread".into(),
            },
            ThreadPanelPullRequestAction::Ready => TrailingAction {
                label: "Ready",
                pending_label: "Marking...",
                pending: self.action_pending,
                destructive: false,
                suffix: false,
                tooltip: "Mark this pull request as ready for review".into(),
            },
            ThreadPanelPullRequestAction::Fix => TrailingAction {
                label: "Fix",
                pending_label: "Preparing...",
                pending: self.handoff == Some(PrHandoff::Findings),
                destructive: true,
                suffix: true,
                tooltip: "Fix the failing checks in a new thread".into(),
            },
            ThreadPanelPullRequestAction::Merge => TrailingAction {
                label: "Merge",
                pending_label: "Merging...",
                pending: self.action_pending,
                destructive: false,
                suffix: false,
                tooltip: format!(
                    "Merge this pull request ({})",
                    merge_method_wire(model.selected_merge_method)
                ),
            },
        })
    }

    /// Everything the host reported, at a glance. The rows stay short; the tooltip is where the
    /// rest of the answer lives, styled like the sidebar's thread tooltip.
    fn row_tooltip(&self, model: &PrRowModel, _: &crate::theme::AppTheme) -> RowTooltip {
        match &self.detail {
            None => RowTooltip::Text(
                self.status
                    .as_ref()
                    .map(|status| status.tooltip.clone())
                    .unwrap_or_else(|| format!("Pull request #{}", self.number))
                    .into(),
            ),
            Some(detail) => RowTooltip::Card(PrTooltipCard {
                title: detail.title.as_str().to_owned().into(),
                number: detail.number.get(),
                state: resolve_pull_request_state(detail.state, detail.is_draft),
                branches: format!(
                    "{} ← {}",
                    detail.base_branch.as_str(),
                    detail.head_branch.as_str()
                ),
                checks: (detail.state == PullRequestState::Open
                    && model.checks_state != PullRequestChecksState::None)
                    .then(|| {
                        (
                            match model.checks_state {
                                PullRequestChecksState::Failing => PullRequestCheckStatus::Failure,
                                PullRequestChecksState::Pending => PullRequestCheckStatus::Pending,
                                _ => PullRequestCheckStatus::Success,
                            },
                            describe_pull_request_checks(&detail.checks),
                        )
                    }),
                conflict: (detail.is_draft && model.conflicting)
                    .then(|| format!("Merge conflicts with {}", detail.base_branch.as_str())),
                changed_files: detail.changed_files,
                additions: detail.additions,
                deletions: detail.deletions,
            }),
        }
    }
}

fn open_merge_dialog(
    number: u64,
    method: PullRequestMergeMethod,
    merge: Option<MergeHandler>,
    confirming: gpui_kit::Entity<bool>,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_alert_dialog(cx, move |alert, _, _| {
        let merge = merge.clone();
        let confirming_ok = confirming.clone();
        let confirming_close = confirming.clone();
        alert
            .title(SharedString::from("Merge pull request?").into_any_element())
            .description(
                SharedString::from(format!(
                    "This merges #{number} using {}.",
                    merge_method_wire(method)
                ))
                .into_any_element(),
            )
            .show_cancel(true)
            .ok_text("Merge")
            .on_ok(move |_, window, cx| {
                confirming_ok.update(cx, |confirming, _| *confirming = false);
                if let Some(merge) = &merge {
                    merge(method, window, cx);
                }
                true
            })
            .on_cancel(move |_, _, cx| {
                confirming_close.update(cx, |confirming, _| *confirming = false);
                true
            })
    });
}

#[derive(Clone)]
enum RowTooltip {
    Text(SharedString),
    Card(PrTooltipCard),
}

impl RowTooltip {
    fn build(&self, window: &mut Window, cx: &mut App) -> gpui_kit::AnyView {
        match self {
            Self::Text(text) => Tooltip::new(text.clone()).build(window, cx),
            Self::Card(card) => {
                let card = card.clone();
                gpui_kit::AppContext::new(cx, |_| PrTooltipView(card)).into()
            }
        }
    }
}

#[derive(Clone)]
struct PrTooltipCard {
    title: SharedString,
    number: u64,
    state: crate::pull_request::pull_request_presentation::PullRequestStatePresentation,
    branches: String,
    checks: Option<(PullRequestCheckStatus, String)>,
    conflict: Option<String>,
    changed_files: u64,
    additions: u64,
    deletions: u64,
}

struct PrTooltipView(PrTooltipCard);

impl gpui_kit::Render for PrTooltipView {
    fn render(&mut self, _: &mut Window, cx: &mut gpui_kit::Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let card = self.0.clone();
        let small = rems(0.75);
        let muted = theme.muted_foreground;
        let body = with_alpha(theme.foreground, 0.75);
        let row = |icon: AnyElement, text: AnyElement| {
            div()
                .flex()
                .min_w_0()
                .items_center()
                .gap_2()
                .child(icon)
                .child(text)
                .into_any_element()
        };
        let mut rows = vec![
            row(
                Icon::new(glyph_icon(card.state.glyph))
                    .size(small)
                    .color(tone_color(card.state.tone_class_name, &theme))
                    .into_any_element(),
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(body)
                    .child(card.state.label)
                    .into_any_element(),
            ),
            row(
                Icon::new(IconName::GitBranch)
                    .size(small)
                    .color(muted)
                    .into_any_element(),
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(body)
                    .child(card.branches.clone())
                    .into_any_element(),
            ),
        ];
        if let Some((status, description)) = card.checks.clone() {
            rows.push(row(
                PullRequestCheckStatusIcon::new("pr-tip-check", status).into_any_element(),
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(body)
                    .child(description)
                    .into_any_element(),
            ));
        }
        if let Some(conflict) = card.conflict.clone() {
            rows.push(row(
                Icon::new(IconName::TriangleAlert)
                    .size(small)
                    .color(theme.destructive)
                    .into_any_element(),
                div()
                    .min_w_0()
                    .flex_1()
                    .text_color(theme.destructive)
                    .child(conflict)
                    .into_any_element(),
            ));
        }
        rows.push(row(
            Icon::new(IconName::FileDiff)
                .size(small)
                .color(muted)
                .into_any_element(),
            div()
                .flex()
                .min_w_0()
                .items_baseline()
                .gap_1()
                .truncate()
                .text_color(body)
                .child(format!(
                    "{} {}",
                    card.changed_files,
                    if card.changed_files == 1 {
                        "file"
                    } else {
                        "files"
                    }
                ))
                .child(PullRequestDiffStat::new(card.additions, card.deletions))
                .into_any_element(),
        ));
        thread_hover_card_popup(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .max_w(rems(20.))
                .gap_2()
                .px_1()
                .py_2()
                .text_size(TEXT_XS.size)
                .child(
                    div()
                        .flex()
                        .min_w_0()
                        .items_baseline()
                        .gap(rems(0.375))
                        .line_height(px(12.))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .child(card.title.clone()),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(muted)
                                .child(format!("#{}", card.number)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rems(0.375))
                        .pl(px(2.))
                        .text_color(muted)
                        .children(rows),
                ),
        )
    }
}

/// A host detail for demos and tests: an open pull request with every action offered and
/// permitted, passing checks, and merge available by merge and squash.
pub(crate) fn demo_pull_request_detail(overrides: serde_json::Value) -> PullRequestDetail {
    use serde_json::json;
    let mut base = json!({
        "provider": "github",
        "capabilities": {
            "diff": true,
            "comment": true,
            "actions": ["merge", "ready"],
            "mergeMethods": ["merge", "squash"],
            "search": true,
            "review": {
                "inlineComment": true,
                "reply": true,
                "resolve": true,
                "verdicts": ["comment", "approve", "request-changes"]
            },
            "reviewers": { "request": true, "listCandidates": true }
        },
        "viewerPermissions": {
            "actions": ["merge", "ready"],
            "comment": true,
            "resolve": true,
            "verdicts": ["comment", "approve", "request-changes"],
            "requestReviewers": true
        },
        "projectId": "project-1",
        "projectTitle": "web",
        "workspaceRoot": "/repo",
        "repository": "acme/web",
        "number": 7,
        "title": "Cache the title",
        "body": "",
        "url": "https://github.com/acme/web/pull/7",
        "author": { "login": "octocat", "name": null, "avatarUrl": "https://avatars.example/octocat" },
        "state": "open",
        "isDraft": false,
        "mergeability": "mergeable",
        "additions": 12,
        "deletions": 3,
        "changedFiles": 2,
        "headBranch": "feat/cache",
        "baseBranch": "main",
        "createdAt": "2026-07-01T00:00:00.000Z",
        "updatedAt": "2026-07-02T00:00:00.000Z",
        "mergedAt": null,
        "closedAt": null,
        "reviewers": [],
        "labels": [],
        "checks": [],
        "mergeCapabilities": { "merge": true, "squash": true, "rebase": false },
    });
    if let (serde_json::Value::Object(base), serde_json::Value::Object(overrides)) =
        (&mut base, overrides)
    {
        base.extend(overrides);
    }
    serde_json::from_value(base).expect("demo pull request detail decodes")
}

#[allow(dead_code)]
fn unused_colors() -> gpui_kit::Hsla {
    tw::BLACK.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pull_request::checks_popover::checks_count_label;
    use serde_json::json;
    use turin_contracts::pull_request::PullRequestCheck;

    fn checks_for(status: &str, extra: Option<&str>) -> serde_json::Value {
        let check = |status: &str| json!({ "name": format!("CI-{status}"), "status": status, "description": null, "url": null });
        let mut checks = vec![check(status)];
        checks.extend(extra.map(check));
        json!(checks)
    }

    fn detail_with(status: &str, extra: Option<&str>) -> PullRequestDetail {
        demo_pull_request_detail(json!({
            "title": "Test PR",
            "checks": checks_for(status, extra),
        }))
    }

    // ThreadDetailsPrRow.test.tsx: "requires a new merge click after passing checks become
    // pending and pass again".
    #[test]
    fn requires_a_new_merge_click_after_passing_checks_become_pending_and_pass_again() {
        let passing = detail_with("success", None);
        let model = resolve_pr_row_model(Some(&passing), None, false);
        assert_eq!(model.row_action, Some(ThreadPanelPullRequestAction::Merge));
        // Click Merge: the confirmation opens.
        let mut confirming = true;
        assert!(keep_confirming_merge(confirming, model.row_action));
        // A check starts running: the offer is gone, and the confirmation with it.
        let pending = detail_with("success", Some("pending"));
        let model = resolve_pr_row_model(Some(&pending), None, false);
        assert_eq!(model.row_action, None);
        confirming = keep_confirming_merge(confirming, model.row_action);
        assert!(!confirming);
        // Checks pass again: Merge is offered, but the dialog stays closed until clicked.
        let model = resolve_pr_row_model(Some(&passing), None, false);
        confirming = keep_confirming_merge(confirming, model.row_action);
        assert!(!confirming);
        assert_eq!(model.row_action, Some(ThreadPanelPullRequestAction::Merge));
        // The next click opens it again.
        confirming = true;
        assert!(keep_confirming_merge(confirming, model.row_action));
    }

    // ThreadDetailsPrRow.test.tsx: "shows a count only while checks run".
    #[test]
    fn shows_a_count_only_while_checks_run() {
        for (status, extra, count) in [
            ("success", "skipped", ""),
            ("failure", "cancelled", ""),
            ("success", "action-required", ""),
            ("success", "pending", "1/2"),
            ("failure", "pending", "1/2"),
        ] {
            let detail = detail_with(status, Some(extra));
            let label = checks_count_label(&detail.checks).filter(|label| {
                label.split_once('/').is_some_and(|(a, b)| {
                    !a.is_empty()
                        && !b.is_empty()
                        && a.bytes().all(|c| c.is_ascii_digit())
                        && b.bytes().all(|c| c.is_ascii_digit())
                })
            });
            assert_eq!(label.as_deref().unwrap_or(""), count, "({status}, {extra})");
        }
    }

    #[test]
    fn counts_failures_when_nothing_runs() {
        let detail = detail_with("failure", Some("cancelled"));
        let checks: &[PullRequestCheck] = &detail.checks;
        assert_eq!(checks_count_label(checks).as_deref(), Some("2 failed"));
        assert_eq!(checks_count_label(&[]), None);
    }

    #[test]
    fn lays_out_by_what_the_row_can_show() {
        // No detail yet and not watched: the link alone.
        assert_eq!(
            resolve_pr_row_model(None, None, false).layout,
            PrRowLayout::Single
        );
        // No detail but watched: the split row with the eye.
        let watched = resolve_pr_row_model(None, Some(GitPullRequestState::Open), true);
        assert_eq!(watched.layout, PrRowLayout::Split);
        // A closed pull request drops the eye.
        let closed = resolve_pr_row_model(None, Some(GitPullRequestState::Closed), true);
        assert!(!closed.watching);
        assert_eq!(closed.layout, PrRowLayout::Single);
        // Detail with checks: two lines.
        let passing = detail_with("success", None);
        assert_eq!(
            resolve_pr_row_model(Some(&passing), None, false).layout,
            PrRowLayout::TwoLine
        );
        // A draft shows no checks, and an empty slot, so it falls back to a single line.
        let draft = demo_pull_request_detail(json!({
            "isDraft": true,
            "viewerPermissions": { "actions": [], "comment": true, "resolve": true,
                "verdicts": ["comment"], "requestReviewers": true },
        }));
        let model = resolve_pr_row_model(Some(&draft), None, false);
        assert!(!model.show_checks);
        assert_eq!(model.layout, PrRowLayout::Single);
    }

    #[test]
    fn ranks_the_action_by_what_unblocks_the_merge_next() {
        let conflicting = demo_pull_request_detail(json!({ "mergeability": "conflicting" }));
        assert_eq!(
            resolve_pr_row_model(Some(&conflicting), None, false).row_action,
            Some(ThreadPanelPullRequestAction::Resolve)
        );
        let failing = detail_with("failure", None);
        assert_eq!(
            resolve_pr_row_model(Some(&failing), None, false).row_action,
            Some(ThreadPanelPullRequestAction::Fix)
        );
        let draft = demo_pull_request_detail(json!({ "isDraft": true }));
        assert_eq!(
            resolve_pr_row_model(Some(&draft), None, false).row_action,
            Some(ThreadPanelPullRequestAction::Ready)
        );
    }
}
