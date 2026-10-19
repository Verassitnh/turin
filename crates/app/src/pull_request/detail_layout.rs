//! Port of upstream/apps/web/src/components/pullRequest/PullRequestDetailLayout.tsx: the header
//! body (title, author line, branch line), the title row, the tab bar and the checks status line
//! that the panel and its loading ghost both assemble.
//!
//! The `@container/pr-header` queries (`@max-[30rem]/pr-header:hidden` and friends) are decided by
//! the caller from the width of the panel; here they are plain flags.

use crate::theme::color::solid_over;
use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, ClickEvent, CursorStyle, Div, ElementId, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, rems,
};
use turin_contracts::pull_request::PullRequestChecksState;

use super::detail_context::DetailTab;
use super::presentation_view::{
    PullRequestActorLabel, PullRequestDiffStat, PullRequestLabelChip, pull_request_meta_line,
    tone_color,
};
use super::pull_request_list_logic::EnvironmentPullRequestEntry;
use super::pull_request_presentation::{
    pull_request_checks_state_presentation, resolve_pull_request_state,
};
use super::relative_time::format_relative_time_label;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, RADIUS_MD, TEXT_BASE, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::skeleton::skeleton_group;
use crate::ui::{
    BadgeSize, Button, ButtonSize, ButtonVariant, Icon, IconName, Toggle, ToggleSize,
    ToggleVariant, Tooltip,
};

/// `TABS`: the tab strip's values and labels.
pub const TABS: [(DetailTab, &str); 4] = [
    (DetailTab::Summary, "Summary"),
    (DetailTab::Guide, "Guide"),
    (DetailTab::Timeline, "Timeline"),
    (DetailTab::Code, "Code"),
];

/// The width of the panel below which the labelled header buttons shrink to their glyph
/// (`@max-[30rem]/pr-header:hidden`).
pub const HEADER_LABELS_MIN_WIDTH: f32 = 480.;
/// `@max-[35rem]/pr-header:hidden`, the checkout button's label.
pub const HEADER_CHECKOUT_LABEL_MIN_WIDTH: f32 = 560.;

/// `color-mix(…/alpha)` shorthand as an `Hsla`.
pub fn alpha(color: Hsla, alpha: f32) -> Hsla {
    with_alpha(color, alpha).into()
}

/// `PullRequestDetailTitleRow`: `flex min-h-7 min-w-0 items-center sm:min-h-6`.
pub fn title_row() -> Div {
    div().flex().min_h(rems(1.5)).min_w_0().items_center()
}

/// The slots of `PullRequestDetailHeaderBody`.
pub struct HeaderBody {
    pub title: AnyElement,
    pub author: AnyElement,
    pub updated: AnyElement,
    pub checkout: Option<AnyElement>,
    pub base: AnyElement,
    pub head: AnyElement,
    pub files: AnyElement,
    pub diff_stat: AnyElement,
}

/// `PullRequestDetailHeaderBody`: `mt-1 min-w-0 px-4 pb-4` around the title, the author line and
/// the branch line.
pub fn header_body(body: HeaderBody, theme: &AppTheme) -> Div {
    let muted = theme.muted_foreground;
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .mt_1()
        .px_4()
        .pb_4()
        .child(body.title)
        .child(
            div()
                .flex()
                .mt_2()
                .min_h(rems(1.25))
                .min_w_0()
                .items_center()
                .gap_2()
                .text_size(TEXT_XS.size)
                .text_color(muted)
                .child(pull_request_meta_line(
                    [Some(body.author), Some(body.updated)],
                    theme,
                ))
                .children(body.checkout),
        )
        .child(
            div()
                .flex()
                .mt_4()
                .min_h(rems(1.25))
                .min_w_0()
                .items_center()
                .gap_2()
                .text_size(TEXT_XS.size)
                .text_color(muted)
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .items_center()
                        .gap(rems(0.375))
                        .font_family(theme.fonts.mono.clone())
                        .text_size(TEXT_XS.size)
                        .text_color(alpha(muted, 0.7))
                        .child(body.base)
                        .child(
                            Icon::new(IconName::ArrowLeft)
                                .size(rems(0.875))
                                .color(alpha(muted, 0.6)),
                        )
                        .child(body.head),
                )
                .child(
                    div()
                        .flex()
                        .flex_shrink_0()
                        .ml_auto()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .min_w(rems(4.))
                                .items_center()
                                .justify_end()
                                .gap(rems(0.375))
                                .child(Icon::new(IconName::FileDiff).size(rems(0.875)))
                                .child(body.files),
                        )
                        .child(body.diff_stat),
                ),
        )
}

pub type SelectTab = Rc<dyn Fn(DetailTab, &ClickEvent, &mut Window, &mut App)>;

/// `PullRequestDetailTabBar`: `flex flex-wrap items-center gap-2 border-t border-border/60
/// px-4 py-2` around the segmented tabs and whatever the active tab puts beside them.
pub fn tab_bar(
    tabs: &[DetailTab],
    value: DetailTab,
    on_select: Option<SelectTab>,
    trailing: Option<AnyElement>,
    theme: &AppTheme,
) -> Div {
    let toggles = tabs.iter().map(|tab| {
        let label = TABS
            .iter()
            .find(|(candidate, _)| candidate == tab)
            .map_or("", |(_, label)| *label);
        let tab = *tab;
        let toggle = Toggle::new(SharedString::from(format!("pr-tab-{label}")))
            .variant(ToggleVariant::Segmented)
            .size(ToggleSize::Segmented)
            .pressed(tab == value)
            .label(label);
        match on_select.clone() {
            Some(on_select) => toggle
                .on_click(move |event, window, cx| on_select(tab, event, window, cx))
                .into_any_element(),
            None => toggle.into_any_element(),
        }
    });
    div()
        .flex()
        .flex_wrap()
        .min_w_0()
        .items_center()
        .gap_2()
        .px_4()
        .py_2()
        .border_t_1()
        .border_color(alpha(theme.border, 0.6))
        // `ToggleGroup`: `flex w-fit gap-0.5 rounded-lg bg-input/40 p-0.5`.
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .gap(px(2.))
                .p(px(2.))
                .rounded(RADIUS_LG)
                .bg(solid_over(theme.input, 0.4, theme.background))
                .children(toggles),
        )
        .children(trailing)
}

/// `PullRequestChecksStatusLine`: `ml-auto flex h-4 min-w-0 flex-1 items-center justify-end
/// gap-x-1.5 overflow-hidden text-xs`.
pub fn checks_status_line(icon: AnyElement, label: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex()
        .ml_auto()
        .h(rems(1.))
        .min_w_0()
        .flex_1()
        .items_center()
        .justify_end()
        .gap(rems(0.375))
        .overflow_hidden()
        .text_size(TEXT_XS.size)
        .text_color(color)
        .child(icon)
        .child(div().whitespace_nowrap().child(label.into()))
}

/// The tabs the strip offers: Code goes where the host has no patch to show, Guide where the
/// reader has not opted into the beta (`visibleTabs`).
pub fn visible_tabs(has_diff: bool, guides_enabled: bool) -> Vec<DetailTab> {
    TABS.iter()
        .map(|(tab, _)| *tab)
        .filter(|tab| *tab != DetailTab::Code || has_diff)
        .filter(|tab| *tab != DetailTab::Guide || guides_enabled)
        .collect()
}

/// `x.toLocaleString()` for counts (en-US grouping).
pub fn format_count(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// `1 file` / `3 files`.
pub fn files_label(changed_files: u64) -> String {
    format!(
        "{} {}",
        format_count(changed_files),
        if changed_files == 1 { "file" } else { "files" }
    )
}

// ── Popover trigger ─────────────────────────────────────────────────────────────────────────────

/// A bare element that opens a popover or menu when pressed (`<PopoverTrigger render={<span/>}>`):
/// it carries no chrome of its own, only the pointer cursor and an optional tooltip.
#[derive(IntoElement)]
pub struct TriggerBox {
    id: ElementId,
    child: AnyElement,
    open: bool,
    tooltip: Option<Tooltip>,
    max_w: Option<gpui_kit::DefiniteLength>,
}

impl TriggerBox {
    pub fn new(id: impl Into<ElementId>, child: AnyElement) -> Self {
        Self {
            id: id.into(),
            child,
            open: false,
            tooltip: None,
            max_w: None,
        }
    }

    pub fn tooltip(mut self, tooltip: Tooltip) -> Self {
        self.tooltip = Some(tooltip);
        self
    }

    pub fn max_w(mut self, width: impl Into<gpui_kit::DefiniteLength>) -> Self {
        self.max_w = Some(width.into());
        self
    }
}

impl gpui_kit::component::Selectable for TriggerBox {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl RenderOnce for TriggerBox {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let mut el = div()
            .id(self.id)
            .flex()
            .min_w_0()
            .flex_shrink_0()
            .items_center()
            .cursor(CursorStyle::PointingHand)
            .child(self.child);
        if let Some(width) = self.max_w {
            el = el.max_w(width);
        }
        match self.tooltip {
            Some(tooltip) => el
                .tooltip(move |window, cx| tooltip.clone().build(window, cx))
                .into_any_element(),
            None => el.into_any_element(),
        }
    }
}

// ── Ghosts (PullRequestGhosts.tsx) ──────────────────────────────────────────────────────────────

/// Widths cycle rather than randomize, so the ghost renders the same on every pass.
const TITLE_WIDTHS: [f32; 7] = [0.6, 0.4, 0.5, 0.66, 0.4, 0.6, 0.5];
const META_WIDTHS: [f32; 7] = [0.4, 0.33, 0.4, 0.25, 0.33, 0.4, 0.33];

/// `GhostBar`: `block h-3 rounded bg-muted-foreground/15`.
pub fn ghost_bar(cx: &App) -> Div {
    div()
        .flex_shrink_0()
        .h(rems(0.75))
        .rounded(px(4.))
        .bg(solid_over(
            cx.app_theme().muted_foreground,
            0.15,
            cx.app_theme().background,
        ))
}

/// A vertically scrolling box that is only as tall as its content, up to `max_height`
/// (`overflow-y-auto max-h-*`); `ui::scroll_area` fills its parent instead.
pub fn scroll_box(id: impl Into<ElementId>, max_height: gpui_kit::Rems) -> gpui_kit::Stateful<Div> {
    use gpui_kit::StatefulInteractiveElement as _;
    div().id(id).max_h(max_height).overflow_y_scroll()
}

/// Wraps a ghost in the single `animate-skeleton` pulse.
fn pulse(id: &'static str, content: Div) -> AnyElement {
    skeleton_group(id, content)
}

/// `PullRequestPeopleGhost`: an avatar and a name, in the reviewer picker's own row height.
pub fn people_ghost(rows: usize, cx: &App) -> AnyElement {
    pulse(
        "pr-people-ghost",
        div()
            .flex()
            .flex_col()
            .gap_1()
            .p_1()
            .children((0..rows).map(|index| {
                div()
                    .flex()
                    .h(rems(1.75))
                    .items_center()
                    .gap_2()
                    .rounded(RADIUS_MD)
                    .px_2()
                    .child(ghost_bar(cx).size(rems(1.)).rounded_full())
                    .child(
                        ghost_bar(cx).w(gpui_kit::relative(META_WIDTHS[index % META_WIDTHS.len()])),
                    )
            })),
    )
}

/// `PullRequestTimelineGhost`: dots on the rail, a line and a date to each.
pub fn timeline_ghost(rows: usize, cx: &App) -> AnyElement {
    let theme = cx.app_theme();
    pulse(
        "pr-timeline-ghost",
        div().px_4().py_5().child(
            div()
                .relative()
                .ml_2()
                .pl(rems(1.25))
                .border_l_1()
                .border_color(alpha(theme.border, 0.7))
                .children((0..rows).map(|index| {
                    div()
                        .relative()
                        .pb_5()
                        .child(
                            ghost_bar(cx)
                                .absolute()
                                .left(rems(-1.55))
                                .top_1()
                                .size(rems(0.5))
                                .rounded_full(),
                        )
                        .child(
                            ghost_bar(cx)
                                .h(rems(0.875))
                                .w(gpui_kit::relative(TITLE_WIDTHS[index % TITLE_WIDTHS.len()])),
                        )
                        .child(ghost_bar(cx).mt(rems(0.375)).w(rems(4.)))
                })),
        ),
    )
}

/// `PullRequestConversationGhost`: a compact placeholder for the conversation while the core
/// detail is already readable.
pub fn conversation_ghost(rows: usize, cx: &App) -> AnyElement {
    pulse(
        "pr-conversation-ghost",
        div()
            .flex()
            .flex_col()
            .gap_4()
            .py_2()
            .children((0..rows).map(|index| {
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(ghost_bar(cx).size(rems(1.25)).rounded_full())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .gap(rems(0.375))
                            .child(
                                ghost_bar(cx)
                                    .w(gpui_kit::relative(META_WIDTHS[index % META_WIDTHS.len()])),
                            )
                            .child(ghost_bar(cx).w_full())
                            .child(ghost_bar(cx).w(gpui_kit::relative(0.75))),
                    )
            })),
    )
}

/// The `MetaRow` of the summary: a 6rem label column, then the value.
pub fn meta_row(icon: IconName, label: &str, value: impl IntoElement, theme: &AppTheme) -> Div {
    div()
        .flex()
        .min_h(rems(1.5))
        .min_w_0()
        .items_center()
        .gap_2()
        .text_size(TEXT_XS.size)
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .w(rems(6.))
                .items_center()
                .gap(rems(0.375))
                .text_color(theme.muted_foreground)
                .child(Icon::new(icon).size(rems(0.875)))
                .child(SharedString::from(label.to_owned())),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .text_color(theme.foreground)
                .child(value),
        )
}

/// `PullRequestDetailGhost`: the panel's expanded shape with the list row's fields in place and
/// bars for the rest, so the loaded pull request does not replace one layout with another.
pub fn detail_ghost(
    seed: Option<EnvironmentPullRequestEntry>,
    active: DetailTab,
    width: f32,
    cx: &App,
) -> AnyElement {
    let theme = cx.app_theme().clone();
    let _ = width;
    let tabs = [DetailTab::Summary, DetailTab::Timeline, DetailTab::Code];
    let selected = if tabs.contains(&active) {
        active
    } else {
        tabs[0]
    };
    let state = seed
        .as_ref()
        .map(|seed| resolve_pull_request_state(seed.state, seed.is_draft));
    let tone = state
        .as_ref()
        .map(|state| tone_color(state.tone_class_name, &theme))
        .unwrap_or(theme.muted_foreground);

    let identity = match &seed {
        Some(seed) => div()
            .flex()
            .min_w_0()
            .items_center()
            .gap_1()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .child(SharedString::from(seed.repository.as_str().to_owned())),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(2.))
                    .text_color(tone)
                    .child(format!("#{}", seed.number.get()))
                    .child(Icon::new(IconName::ExternalLink).size(rems(0.625))),
            ),
        None => div()
            .flex()
            .min_w_0()
            .items_center()
            .gap_1()
            .child(ghost_bar(cx).w(rems(6.)))
            .child(div().flex_shrink_0().child(format!("#{}", "…"))),
    };
    let title = match &seed {
        Some(seed) => div()
            .min_w_0()
            .truncate()
            .text_size(TEXT_BASE.size)
            .font_weight(FontWeight::SEMIBOLD)
            .line_height(rems(1.375))
            .child(SharedString::from(seed.title.as_str().to_owned()))
            .into_any_element(),
        None => ghost_bar(cx)
            .h(rems(1.25))
            .w(gpui_kit::relative(0.8))
            .max_w(rems(28.))
            .into_any_element(),
    };
    let author = match &seed {
        Some(seed) => PullRequestActorLabel::new(seed.author.clone())
            .tooltip(false)
            .into_any_element(),
        None => div()
            .flex()
            .items_center()
            .gap(rems(0.375))
            .child(ghost_bar(cx).size(rems(1.)).rounded_full())
            .child(ghost_bar(cx).h(rems(0.75)).w(rems(3.5)))
            .into_any_element(),
    };
    let updated = match &seed {
        Some(seed) => div()
            .child(format!(
                "updated {}",
                format_relative_time_label(seed.updated_at.as_str())
            ))
            .into_any_element(),
        None => ghost_bar(cx).w(rems(4.)).into_any_element(),
    };
    let base = match &seed {
        Some(seed) => div()
            .min_w_0()
            .max_w(gpui_kit::relative(0.4))
            .truncate()
            .child(SharedString::from(seed.base_branch.as_str().to_owned()))
            .into_any_element(),
        None => ghost_bar(cx).w(rems(3.)).into_any_element(),
    };
    let head = match &seed {
        Some(seed) => div()
            .min_w_0()
            .truncate()
            .px_1()
            .child(SharedString::from(seed.head_branch.as_str().to_owned()))
            .into_any_element(),
        None => ghost_bar(cx).w(rems(8.)).flex_1().into_any_element(),
    };
    let diff_stat = match &seed {
        Some(seed) => PullRequestDiffStat::new(seed.additions, seed.deletions)
            .mono()
            .into_any_element(),
        None => ghost_bar(cx).w(rems(5.)).into_any_element(),
    };
    let body = header_body(
        HeaderBody {
            title: title_row().child(title).into_any_element(),
            author,
            updated,
            checkout: None,
            base,
            head,
            files: ghost_bar(cx).w(rems(2.5)).into_any_element(),
            diff_stat,
        },
        &theme,
    );

    // Passing list rollups can omit workflows awaiting approval; wait for detail to claim success.
    let checks = seed
        .as_ref()
        .and_then(|seed| seed.checks_state)
        .filter(|state| {
            matches!(
                state,
                PullRequestChecksState::Failing | PullRequestChecksState::Pending
            )
        })
        .map(pull_request_checks_state_presentation);
    let checks_line = match checks {
        Some(presentation) => {
            let color = tone_color(presentation.tone_class_name, &theme);
            checks_status_line(
                Icon::new(if presentation.glyph == "circle-x" {
                    IconName::CircleX
                } else {
                    IconName::CircleDot
                })
                .size(rems(0.875))
                .color(color)
                .into_any_element(),
                presentation.label,
                color,
            )
        }
        None => checks_status_line(
            ghost_bar(cx)
                .size(rems(0.875))
                .rounded_full()
                .into_any_element(),
            "",
            theme.muted_foreground,
        )
        .child(ghost_bar(cx).w(rems(7.))),
    };

    let top_row = div()
        .flex()
        .items_start()
        .gap_x_2()
        .min_w_0()
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .h(rems(1.75))
                .pl_4()
                .items_center()
                .overflow_hidden()
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .child(identity),
        )
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .h(rems(1.75))
                .mr_4()
                .items_center()
                .justify_end()
                .gap_1()
                .child(ghost_bar(cx).h(rems(1.5)).w(rems(4.)).rounded(RADIUS_MD))
                .child(
                    Button::new("pr-ghost-actions")
                        .size(ButtonSize::IconXs)
                        .variant(ButtonVariant::Ghost)
                        .disabled(true)
                        .icon(IconName::Ellipsis),
                ),
        );
    let header = div()
        .flex()
        .flex_col()
        .min_w_0()
        .flex_shrink_0()
        .border_b_1()
        .border_color(alpha(theme.border, 0.6))
        .child(top_row)
        .child(body)
        .child(tab_bar(
            &tabs,
            selected,
            None,
            Some(checks_line.into_any_element()),
            &theme,
        ));

    let reviewers = meta_row(
        IconName::Users,
        "Reviewers",
        div()
            .flex()
            .items_center()
            .gap(rems(0.375))
            .child(ghost_bar(cx).w(rems(2.5)))
            .child(
                Button::new("pr-ghost-reviewers")
                    .size(ButtonSize::IconXs)
                    .variant(ButtonVariant::Ghost)
                    .disabled(true)
                    .icon(IconName::UserPlus),
            ),
        &theme,
    );
    let labels_value = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_1()
        .children(match &seed {
            Some(seed) if !seed.labels.is_empty() => seed
                .labels
                .iter()
                .map(|label| {
                    PullRequestLabelChip::new(label.clone())
                        .size(BadgeSize::Default)
                        .into_any_element()
                })
                .collect::<Vec<_>>(),
            Some(_) => vec![
                div()
                    .text_color(theme.muted_foreground)
                    .child("None")
                    .into_any_element(),
            ],
            None => vec![
                ghost_bar(cx).h(rems(1.125)).w(rems(6.)).into_any_element(),
                ghost_bar(cx).h(rems(1.125)).w(rems(5.)).into_any_element(),
            ],
        })
        .child(
            Button::new("pr-ghost-labels")
                .size(ButtonSize::IconXs)
                .variant(ButtonVariant::Ghost)
                .disabled(true)
                .icon(IconName::Tag),
        );
    let labels = meta_row(IconName::Tag, "Labels", labels_value, &theme);
    let summary = div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .child(
            div()
                .px_4()
                .pt(rems(0.625))
                .pb_1()
                .flex()
                .flex_col()
                .gap_2()
                .child(reviewers)
                .child(labels),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(rems(0.375))
                .px_4()
                .py_3()
                .text_size(TEXT_XS.size)
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child("Description")
                .child(
                    Icon::new(IconName::ChevronDown)
                        .size(rems(0.875))
                        .color(alpha(theme.muted_foreground, 0.6)),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .px_4()
                .pb_4()
                .child(ghost_bar(cx).h(rems(1.25)).w(rems(10.)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(ghost_bar(cx).h(rems(1.)).w_full())
                        .child(ghost_bar(cx).h(rems(1.)).w(gpui_kit::relative(0.92)))
                        .child(ghost_bar(cx).h(rems(1.)).w(gpui_kit::relative(0.8)))
                        .child(ghost_bar(cx).h(rems(1.)).w(gpui_kit::relative(0.66))),
                ),
        );

    let container = div()
        .flex()
        .flex_col()
        .size_full()
        .min_h_0()
        .overflow_hidden()
        .bg(theme.background)
        .child(header)
        .child(summary);
    if seed.is_none() {
        pulse("pr-detail-ghost", container)
    } else {
        container.into_any_element()
    }
}

#[allow(dead_code)]
fn _unused(_: &App) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_counts_like_to_locale_string() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1,000");
        assert_eq!(format_count(1234567), "1,234,567");
    }

    #[test]
    fn labels_files_in_the_singular() {
        assert_eq!(files_label(1), "1 file");
        assert_eq!(files_label(12), "12 files");
    }

    #[test]
    fn drops_code_without_a_patch_and_guide_without_the_beta() {
        assert_eq!(
            visible_tabs(true, true),
            vec![
                DetailTab::Summary,
                DetailTab::Guide,
                DetailTab::Timeline,
                DetailTab::Code
            ]
        );
        assert_eq!(
            visible_tabs(false, false),
            vec![DetailTab::Summary, DetailTab::Timeline]
        );
    }
}
