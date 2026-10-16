//! Port of upstream/apps/web/src/components/pullRequest/PullRequestChecksPopover.tsx (the detail
//! header's variant, which already holds every check) and of the stale-branch warning popover
//! `PullRequestBaseFreshnessWarning` from PullRequestDetailPanel.tsx.
//!
//! Not ported: the listing-row variant that reads the detail itself (`LazyChecksBody`; the list
//! row shows the rollup glyph with its own tooltip), the `count` trigger, and the hover-open of
//! the freshness warning (`openOnHover`; GPUI popovers open on press, so the mark is pressed to
//! see the sentence and the way out of it).

use std::rc::Rc;

use gpui_kit::component::popover::Popover;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, rems,
};
use turin_contracts::pull_request::{
    PullRequestCheck, PullRequestChecksState, PullRequestUpdateMethod,
};

use super::detail_layout::{TriggerBox, alpha, scroll_box};
use super::presentation_view::{PullRequestCheckStatusIcon, tone_color};
use super::pull_request_detail_logic::group_pull_request_checks;
use super::pull_request_presentation::{
    pull_request_check_status_label, pull_request_checks_state_presentation,
    summarize_pull_request_checks,
};
use crate::theme::tokens::{TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{
    Button, ButtonSize, ButtonVariant, Icon, IconName, PopoverOptions, PopoverPadding,
    PopoverWidth, Tooltip, popover,
};

type OpenUrl = Rc<dyn Fn(&str, &mut App)>;
type ToggleFn = Rc<dyn Fn(&mut Window, &mut App)>;
/// Called with the way the reader chose to catch the branch up.
pub type UpdateBranch = Rc<dyn Fn(PullRequestUpdateMethod, &mut Window, &mut App)>;

/// `max-h-64`.
const LIST_MAX_HEIGHT: f32 = 16.;

fn glyph_icon(glyph: &str) -> IconName {
    match glyph {
        "circle-check" => IconName::CircleCheck,
        "circle-x" => IconName::CircleX,
        _ => IconName::CircleDot,
    }
}

/// `ChecksBody`: the failing and awaiting checks first, then the running ones, and the finished
/// ones behind "Show all" when there is anything above them.
fn checks_body(
    id: &str,
    checks: &[PullRequestCheck],
    show_all: bool,
    on_toggle: ToggleFn,
    on_open: OpenUrl,
    theme: &AppTheme,
) -> AnyElement {
    if checks.is_empty() {
        return div()
            .text_size(TEXT_XS.size)
            .text_color(theme.muted_foreground)
            .child("No checks reported")
            .into_any_element();
    }
    let groups = group_pull_request_checks(checks);
    let can_collapse =
        groups.attention.len() + groups.running.len() > 0 && !groups.completed.is_empty();
    let visible: Vec<PullRequestCheck> = groups
        .attention
        .iter()
        .chain(groups.running.iter())
        .chain(if show_all || !can_collapse {
            groups.completed.iter()
        } else {
            [].iter()
        })
        .cloned()
        .collect();
    let rows = visible.into_iter().enumerate().map(|(index, check)| {
        let label = pull_request_check_status_label(&check);
        let tooltip = check
            .description
            .clone()
            .unwrap_or_else(|| check.name.as_str().to_owned());
        let url = check.url.clone();
        let on_open = on_open.clone();
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_size(TEXT_XS.size)
            .child(PullRequestCheckStatusIcon::new(
                SharedString::from(format!("{id}-check-{index}")),
                check.status,
            ))
            .child(
                div()
                    .id(SharedString::from(format!("{id}-check-name-{index}")))
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .child(SharedString::from(check.name.as_str().to_owned()))
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx)),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(theme.muted_foreground)
                    .child(label),
            )
            .when_some(url, |this, url| {
                let primary = theme.primary;
                this.child(
                    div()
                        .id(SharedString::from(format!("{id}-check-details-{index}")))
                        .flex_shrink_0()
                        .cursor_pointer()
                        .text_color(primary)
                        .hover(|style| style.underline())
                        .child("Details")
                        .on_click(move |_, _, cx| on_open(&url, cx)),
                )
            })
    });
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            scroll_box(
                SharedString::from(format!("{id}-scroll")),
                rems(LIST_MAX_HEIGHT),
            )
            .child(div().flex().flex_col().gap_1().children(rows)),
        )
        .when(can_collapse, |this| {
            this.child(
                Button::new(SharedString::from(format!("{id}-show-all")))
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Xs)
                    .label(if show_all { "Show less" } else { "Show all" })
                    .on_click(move |_, window, cx| on_toggle(window, cx)),
            )
        })
        .into_any_element()
}

/// `PullRequestChecksPopover` with the checks in hand: the rollup glyph, and what it opens.
#[derive(IntoElement)]
pub struct PullRequestChecksPopover {
    id: ElementId,
    checks: Vec<PullRequestCheck>,
    checks_state: PullRequestChecksState,
    stale: bool,
    on_open: Option<OpenUrl>,
}

impl PullRequestChecksPopover {
    pub fn new(
        id: impl Into<ElementId>,
        checks: Vec<PullRequestCheck>,
        checks_state: PullRequestChecksState,
    ) -> Self {
        Self {
            id: id.into(),
            checks,
            checks_state,
            stale: false,
            on_open: None,
        }
    }

    /// Check details are out of date: the popover says so instead of listing them.
    pub fn stale(mut self, stale: bool) -> Self {
        self.stale = stale;
        self
    }

    /// How a check's details link opens (the thread's browser, else the system's).
    pub fn on_open(mut self, on_open: OpenUrl) -> Self {
        self.on_open = Some(on_open);
        self
    }
}

impl RenderOnce for PullRequestChecksPopover {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let presentation = pull_request_checks_state_presentation(self.checks_state);
        let tone = tone_color(presentation.tone_class_name, &theme);
        let summary = (!self.stale).then(|| summarize_pull_request_checks(&self.checks));
        let id = SharedString::from(format!("{:?}", self.id));
        let trigger = TriggerBox::new(
            SharedString::from(format!("{id}-trigger")),
            Icon::new(glyph_icon(presentation.glyph))
                .size(rems(0.875))
                .color(tone)
                .into_any_element(),
        )
        .tooltip(Tooltip::new(format!("Checks: {}", presentation.label)));
        let checks = self.checks;
        let stale = self.stale;
        let on_open: OpenUrl = self
            .on_open
            .unwrap_or_else(|| Rc::new(|url, cx| cx.open_url(url)));
        let label = presentation.label;
        let popup_theme = theme.clone();
        let popover_id = SharedString::from(format!("{id}-popover"));
        popover(
            SharedString::from(format!("{id}-popover")),
            PopoverOptions {
                width: PopoverWidth::Md,
                ..PopoverOptions::default()
            },
            window,
            cx,
        )
        .anchor(Anchor::TopLeft)
        .bg(theme.popover)
        .font_family(theme.fonts.sans.clone())
        .default_open(super::dropdown::demo_open(&popover_id))
        .trigger(trigger)
        .content(move |_, window, cx| {
            let theme = &popup_theme;
            let key = SharedString::from(format!("{id}-show-all-state"));
            let show_all = window.use_keyed_state(key, cx, |_, _| false);
            let shown = *show_all.read(cx);
            let toggle = {
                let show_all = show_all.clone();
                Rc::new(move |_: &mut Window, cx: &mut App| {
                    show_all.update(cx, |state, cx| {
                        *state = !*state;
                        cx.notify();
                    });
                }) as ToggleFn
            };
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .mb_2()
                        .text_size(TEXT_SM.size)
                        .font_weight(FontWeight::MEDIUM)
                        .child(label),
                )
                .children(summary.clone().map(|summary| {
                    div()
                        .mb_2()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.muted_foreground)
                        .child(summary)
                }))
                .child(if stale {
                    div()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.muted_foreground)
                        .child("Check details are out of date. Refresh the pull request to update them.")
                        .into_any_element()
                } else {
                    checks_body(&id, &checks, shown, toggle, on_open.clone(), theme)
                })
        })
    }
}

/// What [`freshness_warning`] draws.
pub struct FreshnessWarning {
    /// "This branch is out-of-date with main by 3 commits."
    pub summary: String,
    /// Each way the host offers and this reader may take.
    pub methods: Vec<PullRequestUpdateMethod>,
    /// An action is in flight: the buttons are disabled.
    pub pending: bool,
    pub stacked: bool,
    /// What the warning is about, drawn in the same amber before the mark: the base branch.
    pub name: AnyElement,
    pub max_width: gpui_kit::Pixels,
    pub on_update: UpdateBranch,
}

/// `PullRequestBaseFreshnessWarning`: the base branch wearing the stale-branch mark, and the
/// popover that says how far behind it is with a button per way the host offers to catch up.
pub fn freshness_warning(input: FreshnessWarning, theme: &AppTheme) -> AnyElement {
    let FreshnessWarning {
        summary,
        methods,
        pending,
        stacked,
        name,
        max_width,
        on_update,
    } = input;
    let warning = theme.warning_foreground;
    let trigger = TriggerBox::new("pr-base-freshness-trigger", {
        div()
            .flex()
            .min_w_0()
            .items_center()
            .gap_1()
            .text_color(warning)
            .when(stacked, |this| {
                this.child(Icon::new(IconName::Layers).size(rems(0.75)).color(warning))
            })
            .child(name)
            .child(
                Icon::new(IconName::TriangleAlert)
                    .size(rems(0.875))
                    .color(warning),
            )
            .into_any_element()
    })
    .max_w(max_width)
    .tooltip(Tooltip::new(summary.clone()));
    let foreground = theme.foreground;
    let muted = theme.muted_foreground;
    FreshnessPopover {
        trigger,
        summary,
        methods,
        pending,
        on_update,
        foreground,
        muted,
        popup_bg: theme.popover,
        popup_font: theme.fonts.sans.clone().into(),
    }
    .into_any_element()
}

#[derive(IntoElement)]
struct FreshnessPopover {
    trigger: TriggerBox,
    summary: String,
    methods: Vec<PullRequestUpdateMethod>,
    pending: bool,
    on_update: UpdateBranch,
    foreground: gpui_kit::Hsla,
    muted: gpui_kit::Hsla,
    popup_bg: gpui_kit::Hsla,
    popup_font: SharedString,
}

impl RenderOnce for FreshnessPopover {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let FreshnessPopover {
            trigger,
            summary,
            methods,
            pending,
            on_update,
            foreground,
            muted,
            popup_bg,
            popup_font,
        } = self;
        popover(
            "pr-base-freshness",
            PopoverOptions {
                padding: PopoverPadding::Compact,
                ..PopoverOptions::default()
            },
            window,
            cx,
        )
        .anchor(Anchor::TopLeft)
        .bg(popup_bg)
        .font_family(popup_font)
        .default_open(super::dropdown::demo_open("pr-base-freshness"))
        .max_w(rems(20.))
        .trigger(trigger)
        .content(move |_, _, _| {
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(TEXT_XS.size)
                        .text_color(foreground)
                        .child(summary.clone()),
                )
                .child(
                    div()
                        .mt(rems(0.125))
                        .text_size(TEXT_XS.size)
                        .text_color(muted)
                        .child("Changes can be cleanly merged."),
                )
                .when(!methods.is_empty(), |this| {
                    this.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .mt_2()
                            .gap(rems(0.375))
                            .children(methods.iter().map(|method| {
                                let method = *method;
                                let on_update = on_update.clone();
                                Button::new(SharedString::from(format!("pr-update-{method:?}")))
                                    .size(ButtonSize::Xs)
                                    .variant(ButtonVariant::Outline)
                                    .disabled(pending)
                                    .icon(IconName::GitMerge)
                                    .icon_size(rems(0.75))
                                    .label(if method == PullRequestUpdateMethod::Rebase {
                                        "Update with rebase"
                                    } else {
                                        "Update branch"
                                    })
                                    .on_click(move |_, window, cx| on_update(method, window, cx))
                            })),
                    )
                })
        })
    }
}

#[allow(dead_code)]
fn _unused() {
    let _ = alpha;
    let _: Option<Popover> = None;
}
