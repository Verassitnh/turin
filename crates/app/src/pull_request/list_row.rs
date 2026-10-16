//! Port of upstream/apps/web/src/components/pullRequest/PullRequestRow.tsx and the shared row
//! shape in PullRequestListRow.tsx.
//!
//! One row shape for the page list and a thread's linked panel: a status column, then two lines —
//! number and title with the diff counts on the right; author and whatever else the caller shows
//! under them with the time on the right. The page row keeps `px-3 py-2.5` around those lines.
//!
//! Speed mode's buttons ride at the row's end ([`RowSpeed`]); the page owns what they do.
//!
//! Not ported: the `content-visibility` skip (GPUI lists virtualize instead), and the
//! container-query label slots, which are decided from the width the page hands in
//! (`meta_width`).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Bounds, CursorStyle, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement, Pixels, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled,
    Window, div, px, rems,
};

use super::presentation_view::{
    PullRequestActorAvatar, PullRequestChecksStateGlyph, PullRequestConflictGlyph,
    PullRequestDiffStat, PullRequestLabelChip, PullRequestReviewDecisionGlyph,
    PullRequestStateGlyph, actor_tooltip, meta_text_size, with_tooltip,
};
use super::pull_request_list_logic::{EnvironmentPullRequestEntry, pull_request_entry_key};
use super::relative_time::format_relative_time_label;
use super::speed_actions::{PerformAction, PullRequestSpeedActions, SweepStart};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_MD, TEXT_SM, TEXT_XS};
use crate::ui::{BadgeSize, Icon, IconName};
use turin_contracts::pull_request::PullRequestAction;

/// `@xl/pr-row-meta` (36rem) and `@3xl/pr-row-meta` (48rem): the meta line widths at which the
/// second and third label pills appear.
const LABEL_SLOT_WIDTHS: [f32; 3] = [0., 576., 768.];
/// `@xs/pr-row-meta` (20rem): below it the author shows only the avatar.
const AUTHOR_LABEL_MIN_WIDTH: f32 = 320.;

/// Where each row last laid out, by row key: what the close sweep finds the row under the
/// pointer from (`[data-pull-request-key]` in upstream).
pub type RowBounds = Rc<RefCell<HashMap<String, Bounds<Pixels>>>>;

/// What a row needs to offer speed mode (`hostActions`, `speedMode`, `closing`, `sweeping`).
#[derive(Clone)]
pub struct RowSpeed {
    pub host_actions: HashSet<PullRequestAction>,
    pub visible: bool,
    pub can_write: bool,
    /// `actionPending || closing || sweeping`.
    pub busy: bool,
    pub on_perform: PerformAction,
    pub on_close_sweep_start: Option<SweepStart>,
}

pub type SelectRow = Rc<dyn Fn(&EnvironmentPullRequestEntry, &mut Window, &mut App)>;

/// `PullRequestRowLabels`: up to three label pills, each past the first only once the line is
/// wide enough, with the "+N" overflow riding on the last visible one.
fn row_labels(entry: &EnvironmentPullRequestEntry, meta_width: Pixels) -> Option<AnyElement> {
    if entry.labels.is_empty() {
        return None;
    }
    let width = f32::from(meta_width);
    let visible = LABEL_SLOT_WIDTHS
        .iter()
        .take_while(|min| width >= **min)
        .count()
        .min(entry.labels.len());
    let remaining = entry.labels.len() - visible;
    Some(
        div()
            .flex()
            .min_w_0()
            .items_center()
            .gap_1()
            .children(
                entry
                    .labels
                    .iter()
                    .take(visible)
                    .enumerate()
                    .map(|(index, label)| {
                        let chip = PullRequestLabelChip::new(label.clone()).size(BadgeSize::Sm);
                        if index + 1 == visible && remaining > 0 {
                            chip.trailing(format!("+{remaining}")).into_any_element()
                        } else {
                            chip.into_any_element()
                        }
                    }),
            )
            .into_any_element(),
    )
}

#[derive(IntoElement)]
pub struct PullRequestRow {
    entry: EnvironmentPullRequestEntry,
    selected: bool,
    show_project_title: bool,
    show_provider: bool,
    matched_elsewhere: bool,
    meta_width: Pixels,
    on_select: SelectRow,
    speed: Option<RowSpeed>,
    bounds: Option<RowBounds>,
    /// The store and layer callback the stack trigger opens its menu with.
    stack: Option<(
        gpui_kit::Entity<super::view_model::PullRequestStore>,
        super::stack_popover::SelectLayer,
    )>,
}

impl PullRequestRow {
    pub fn new(entry: EnvironmentPullRequestEntry, on_select: SelectRow) -> Self {
        Self {
            entry,
            selected: false,
            show_project_title: true,
            show_provider: false,
            matched_elsewhere: false,
            meta_width: px(640.),
            on_select,
            speed: None,
            bounds: None,
            stack: None,
        }
    }

    /// Speed mode's actions at the end of the row.
    pub fn speed(mut self, speed: RowSpeed) -> Self {
        self.speed = Some(speed);
        self
    }

    /// Records the row's laid-out bounds under its key on every frame.
    pub fn record_bounds(mut self, bounds: RowBounds) -> Self {
        self.bounds = Some(bounds);
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn show_project_title(mut self, show: bool) -> Self {
        self.show_project_title = show;
        self
    }

    /// Only when the list spans more than one host, where the repository alone is ambiguous.
    pub fn show_provider(mut self, show: bool) -> Self {
        self.show_provider = show;
        self
    }

    /// A search found this, but in something the row does not show.
    pub fn matched_elsewhere(mut self, matched: bool) -> Self {
        self.matched_elsewhere = matched;
        self
    }

    /// `PullRequestStackPopover`: the stack trigger opens the layers menu (reads the stack from
    /// `store`, and `on_select` opens the chosen layer).
    pub fn stack_popover(
        mut self,
        store: gpui_kit::Entity<super::view_model::PullRequestStore>,
        on_select: super::stack_popover::SelectLayer,
    ) -> Self {
        self.stack = Some((store, on_select));
        self
    }

    /// The width of the row's meta line, which decides how many label pills fit.
    pub fn meta_width(mut self, width: Pixels) -> Self {
        self.meta_width = width;
        self
    }
}

impl RenderOnce for PullRequestRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let entry = self.entry;
        let key = pull_request_entry_key(&entry.entry, Some(entry.environment_id.as_str()));
        let id = SharedString::from(format!("pr-row:{key}"));
        let width = f32::from(self.meta_width);
        let author = entry.author.clone();
        let login: SharedString = author
            .as_ref()
            .map_or("ghost", |actor| actor.login.as_str())
            .to_owned()
            .into();
        let author_tooltip: SharedString = actor_tooltip(author.as_ref()).into();

        // Status column: the lifecycle glyph with the conflict warning on its corner.
        let glyph = div()
            .flex()
            .flex_col()
            .items_center()
            .flex_shrink_0()
            .w(rems(1.))
            .gap(px(2.))
            // `mt-0.75 self-start`: on the title line rather than between the lines.
            .mt(px(3.))
            .self_start()
            .child(
                div()
                    .relative()
                    .flex()
                    .child(PullRequestStateGlyph::new(entry.state, entry.is_draft))
                    .child(
                        div().absolute().right(px(-4.)).bottom(px(-4.)).child(
                            PullRequestConflictGlyph::new(
                                entry.state,
                                entry.is_draft,
                                Some(entry.mergeability),
                                Some(entry.base_branch.as_str().to_owned().into()),
                            )
                            .size(rems(0.75)),
                        ),
                    ),
            );

        let number = div()
            .flex_shrink_0()
            .font_family(theme.fonts.mono.clone())
            .text_size(TEXT_XS.size)
            .text_color(theme.muted_foreground)
            .child(format!("#{}", entry.number.get()));

        let signals = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .when_some(entry.checks_state, |this, state| {
                this.child(PullRequestChecksStateGlyph::new(state))
            })
            .when_some(entry.review_decision, |this, decision| {
                this.child(PullRequestReviewDecisionGlyph::new(decision))
            });

        let status = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(rems(0.375))
            .text_size(meta_text_size())
            .when_some(entry.stack.as_ref(), |this, stack| {
                match self.stack.clone() {
                    Some((store, on_select)) => {
                        let reference = turin_contracts::pull_request::PullRequestRef {
                            project_id: entry.project_id.clone(),
                            host: Some(entry.host.clone()),
                            expected_account_id: None,
                            allow_stale: None,
                            repository: entry.repository.clone(),
                            number: entry.number,
                        };
                        this.child(super::stack_popover::PullRequestStackPopover::new(
                            store,
                            reference,
                            stack.clone(),
                            on_select,
                        ))
                    }
                    None => {
                        let label = format!(
                            "View stack #{}, layer {} of {}",
                            stack.number.get(),
                            stack.position.get(),
                            stack.size.get()
                        );
                        this.child(with_tooltip(
                            "pr-stack-trigger",
                            label,
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .child(Icon::new(IconName::Layers).size(rems(0.75)))
                                .child(format!("{}/{}", stack.position.get(), stack.size.get())),
                        ))
                    }
                }
            })
            .child(PullRequestDiffStat::new(entry.additions, entry.deletions).mono());

        let line_one = div()
            .flex()
            .min_w_0()
            .items_center()
            .gap(rems(0.375))
            .child(number)
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(TEXT_SM.size)
                    .line_height(TEXT_SM.line_height)
                    .child(SharedString::from(entry.title.as_str().to_owned())),
            )
            .child(signals.text_size(meta_text_size()))
            .child(div().flex_1())
            .child(status);

        let author_cell = with_tooltip(
            "pr-row-author",
            author_tooltip,
            div()
                .flex()
                .min_w(px(14.))
                .max_w(px(160.))
                .items_center()
                .gap_1()
                .child(PullRequestActorAvatar::new(author.as_ref()).size(px(14.)))
                .when(width >= AUTHOR_LABEL_MIN_WIDTH, |this| {
                    this.child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(login),
                    )
                }),
        );

        let line_two = div()
            .flex()
            .min_w_0()
            .items_center()
            .gap(rems(0.375))
            .overflow_hidden()
            .text_size(meta_text_size())
            .line_height(rems(1.))
            .text_color(theme.muted_foreground)
            .when(self.matched_elsewhere, |this| {
                this.child(with_tooltip(
                    "pr-row-matched",
                    "Matched in the description",
                    div()
                        .flex()
                        .min_w(rems(1.5))
                        .items_center()
                        .gap_1()
                        .overflow_hidden()
                        .rounded_full()
                        .border_1()
                        .border_color(with_alpha(theme.border, 0.6))
                        .px_1()
                        .text_size(px(10.))
                        .child(Icon::new(IconName::Search).size(rems(0.75)))
                        .when(width >= AUTHOR_LABEL_MIN_WIDTH, |this| {
                            this.child("matched in the description")
                        }),
                ))
            })
            .when(self.show_provider, |this| {
                this.child(with_tooltip(
                    "pr-row-provider",
                    "GitHub",
                    Icon::new(IconName::Github).size(rems(0.75)),
                ))
            })
            .child(author_cell)
            .when(self.show_project_title, |this| {
                this.child(
                    div()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(SharedString::from(entry.repository.as_str().to_owned())),
                )
            })
            .children(row_labels(&entry, self.meta_width))
            .child(div().flex_1())
            .child(
                div()
                    .flex_shrink_0()
                    .whitespace_nowrap()
                    .child(format_relative_time_label(&entry.updated_at)),
            );

        let selected = self.selected;
        let on_select = self.on_select;
        let target = entry.clone();
        let row_key = key.clone();
        let sink = self.bounds;
        let speed = self
            .speed
            .filter(|speed| !speed.host_actions.is_empty())
            .map(|speed| {
                let mut actions = PullRequestSpeedActions::new(
                    entry.clone(),
                    speed.host_actions,
                    speed.on_perform,
                )
                .visible(speed.visible)
                .can_write(speed.can_write)
                .busy(speed.busy);
                if let Some(start) = speed.on_close_sweep_start {
                    actions = actions.on_close_sweep_start(start);
                }
                actions
            });
        div()
            .relative()
            .flex()
            .items_center()
            .when_some(sink, |this, sink| {
                this.child(
                    gpui_kit::canvas(
                        move |bounds, _, _| {
                            sink.borrow_mut().insert(row_key.clone(), bounds);
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
            })
            .rounded(RADIUS_MD)
            .when(selected, |this| this.bg(theme.accent))
            .when(!selected, |this| {
                this.hover(|style| style.bg(with_alpha(theme.accent, 0.6)))
            })
            .child(
                div()
                    .id(id)
                    .flex()
                    .w_full()
                    .min_w_0()
                    .flex_1()
                    .items_center()
                    .gap_2()
                    .rounded(RADIUS_MD)
                    .px_3()
                    .py(rems(0.625))
                    .cursor(CursorStyle::PointingHand)
                    .on_click(move |_, window, cx| on_select(&target, window, cx))
                    .child(glyph)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(line_one)
                            .child(line_two),
                    ),
            )
            .children(speed)
            .font_weight(FontWeight::NORMAL)
    }
}
