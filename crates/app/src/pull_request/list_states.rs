//! The non-row states of the pull request list: group headers (route `PullRequestGroupHeader`),
//! the loading ghost (PullRequestGhosts.tsx `PullRequestListGhost`), the empty states
//! (PullRequestListEmptyState.tsx) and the unavailable state (PullRequestsUnavailableState.tsx).

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Div, FontWeight, IntoElement, ParentElement, RenderOnce,
    SharedString, Styled, Window, div, px, rems, svg,
};

use super::pull_request_list_logic::PullRequestGroupKey;
use super::presentation_view::Spinner;
use crate::assets::BRANCH_MARK;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::TEXT_XS;
use crate::ui::empty::{Empty, EmptySize, empty_description, empty_header, empty_media_icon, empty_title};
use crate::ui::skeleton::{SkeletonShape, skeleton, skeleton_group};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

type Callback = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// `GROUP_ICONS`: the glyph the involvement filter uses for the same idea.
fn group_icon(key: PullRequestGroupKey) -> IconName {
    match key {
        PullRequestGroupKey::Authored => IconName::PenLine,
        PullRequestGroupKey::ReviewRequested => IconName::Eye,
        PullRequestGroupKey::Others => IconName::Users,
    }
}

/// `PullRequestGroupHeader`: `px-3 pb-1 text-xs font-medium text-muted-foreground/70`, the
/// count, then a hairline filling the rest.
pub fn group_header(key: PullRequestGroupKey, label: &str, count: usize, cx: &App) -> Div {
    let theme = cx.app_theme();
    let muted = with_alpha(theme.muted_foreground, 0.7);
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .pb_1()
        .text_size(TEXT_XS.size)
        .line_height(TEXT_XS.line_height)
        .font_weight(FontWeight::MEDIUM)
        .text_color(muted)
        .child(Icon::new(group_icon(key)).size(rems(0.875)).color(muted.into()))
        .child(div().flex_shrink_0().child(SharedString::from(label.to_owned())))
        .child(
            div()
                .flex_shrink_0()
                .text_color(with_alpha(theme.muted_foreground, 0.5))
                .child(count.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w(rems(0.5))
                .h(px(1.))
                .bg(theme.border),
        )
}

/// Widths cycle rather than randomize, so the ghost renders the same on every pass.
const TITLE_WIDTHS: [f32; 7] = [0.6, 0.4, 0.5, 0.66, 0.4, 0.6, 0.5];
const META_WIDTHS: [f32; 7] = [0.4, 0.33, 0.4, 0.25, 0.33, 0.4, 0.33];

/// `PullRequestListGhost`: bars in the geometry of the rows they stand for, one shared pulse.
pub fn list_ghost(rows: usize, caption: Option<String>, cx: &App) -> AnyElement {
    let theme = cx.app_theme();
    let mut group = div().flex().flex_col().gap(px(2.));
    if let Some(caption) = caption {
        group = group.child(
            div()
                .px_3()
                .pb_1()
                .text_size(TEXT_XS.size)
                .line_height(TEXT_XS.line_height)
                .font_weight(FontWeight::MEDIUM)
                .text_color(with_alpha(theme.muted_foreground, 0.7))
                .child(caption),
        );
    }
    for index in 0..rows {
        let title = TITLE_WIDTHS[index % TITLE_WIDTHS.len()];
        let meta = META_WIDTHS[index % META_WIDTHS.len()];
        group = group.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .rounded(crate::theme::tokens::RADIUS_MD)
                .px_3()
                .py(rems(0.625))
                .child(skeleton(SkeletonShape::Pill, cx).size(rems(1.)).flex_shrink_0())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(rems(0.375))
                        .child(skeleton(SkeletonShape::Block, cx).h(rems(1.)).w(gpui_kit::relative(title)))
                        .child(skeleton(SkeletonShape::Block, cx).h(rems(0.875)).w(gpui_kit::relative(meta))),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_end()
                        .gap(rems(0.375))
                        .child(skeleton(SkeletonShape::Block, cx).h(rems(0.75)).w(rems(3.)))
                        .child(skeleton(SkeletonShape::Block, cx).h(rems(0.75)).w(rems(4.))),
                ),
        );
    }
    skeleton_group("pr-list-ghost", group)
}

/// `BranchMark`: two branch lines and the node where a change would land, unjoined.
fn branch_mark(cx: &App) -> impl IntoElement {
    svg()
        .path(BRANCH_MARK)
        .w(rems(8.))
        .h(rems(5.))
        .flex_shrink_0()
        .text_color(with_alpha(cx.app_theme().muted_foreground, 0.6))
}

fn refresh_button(
    id: &'static str,
    label: &'static str,
    refreshing: bool,
    on_refresh: Callback,
) -> Button {
    Button::new(id)
        .variant(ButtonVariant::Outline)
        .size(ButtonSize::Sm)
        .disabled(refreshing)
        .child(if refreshing {
            Spinner::new(id).size(rems(0.875)).into_any_element()
        } else {
            Icon::new(IconName::RefreshCw).size(rems(0.875)).into_any_element()
        })
        .child(if refreshing { "Checking..." } else { label })
        .on_click(move |event, window, cx| on_refresh(event, window, cx))
}

fn truncated_query(query: &str) -> String {
    let chars: Vec<char> = query.chars().collect();
    if chars.len() > 48 {
        format!("{}…", chars[..48].iter().collect::<String>())
    } else {
        query.to_owned()
    }
}

/// What the list shows when it has no rows to show.
#[derive(IntoElement)]
pub struct PullRequestListEmptyState {
    pub query: String,
    pub filtered: bool,
    pub searching: bool,
    pub has_projects: bool,
    pub can_load_more: bool,
    pub loading_more: bool,
    pub refreshing: bool,
    pub on_clear_query: Callback,
    pub on_load_more: Callback,
    pub on_refresh: Callback,
}

impl RenderOnce for PullRequestListEmptyState {
    fn render(self, _: &mut Window, cx: &mut App) -> AnyElement {
        // Ahead of the search and the filters, because neither can produce a row until a
        // project does.
        if !self.has_projects {
            return Empty::new()
                .child(branch_mark(cx))
                .child(
                    empty_header()
                        .child(empty_title("No projects in this workspace", false))
                        .child(empty_description(
                            "Add a project, and the pull requests from its repository appear here.",
                            cx,
                        )),
                )
                .child(
                    Button::new("pr-empty-add-project")
                        .size(ButtonSize::Sm)
                        .icon(IconName::Plus)
                        .icon_size(rems(0.875))
                        .label("Add project"),
                )
                .into_any_element();
        }
        if self.searching {
            return list_ghost(
                5,
                Some(format!("Searching every host for “{}”", truncated_query(&self.query))),
                cx,
            );
        }
        if !self.query.is_empty() {
            return Empty::new()
                .child(branch_mark(cx))
                .child(
                    empty_header()
                        .child(empty_title(
                            format!("Nothing matches “{}”", truncated_query(&self.query)),
                            false,
                        ))
                        .child(empty_description(
                            "The hosts were searched for it. Try fewer words, or search by number, author or branch.",
                            cx,
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_center()
                        .gap_2()
                        .child({
                            let on_clear = self.on_clear_query.clone();
                            Button::new("pr-empty-clear-search")
                                .variant(ButtonVariant::Outline)
                                .size(ButtonSize::Sm)
                                .icon(IconName::Search)
                                .icon_size(rems(0.875))
                                .label("Clear search")
                                .on_click(move |event, window, cx| on_clear(event, window, cx))
                        })
                        .child(refresh_button(
                            "pr-empty-check-again",
                            "Check again",
                            self.refreshing,
                            self.on_refresh.clone(),
                        )),
                )
                .into_any_element();
        }
        Empty::new()
            .child(branch_mark(cx))
            .child(
                empty_header()
                    .child(empty_title(
                        if self.filtered {
                            "Nothing under these filters"
                        } else {
                            "No pull requests"
                        },
                        false,
                    ))
                    .child(empty_description(
                        if self.filtered {
                            "Widen the state, involvement or project filter to see more."
                        } else {
                            "Pull requests from every project in this workspace appear here."
                        },
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap_2()
                    .when(self.can_load_more, |this| {
                        let on_more = self.on_load_more.clone();
                        this.child(
                            Button::new("pr-empty-load-more")
                                .variant(ButtonVariant::Outline)
                                .size(ButtonSize::Sm)
                                .disabled(self.loading_more)
                                .label(if self.loading_more {
                                    "Loading..."
                                } else {
                                    "Load more pull requests"
                                })
                                .on_click(move |event, window, cx| on_more(event, window, cx)),
                        )
                    })
                    .child(refresh_button(
                        "pr-empty-check-again",
                        "Check again",
                        self.refreshing,
                        self.on_refresh.clone(),
                    )),
            )
            .into_any_element()
    }
}

/// `PullRequestsUnavailableState`: an icon tile, a title and the caller's own explanation.
#[derive(IntoElement)]
pub struct PullRequestsUnavailableState {
    pub title: SharedString,
    pub error: SharedString,
    pub refreshing: bool,
    pub on_retry: Option<Callback>,
}

impl PullRequestsUnavailableState {
    pub fn new(error: impl Into<SharedString>) -> Self {
        Self {
            title: "Could not load pull requests".into(),
            error: error.into(),
            refreshing: false,
            on_retry: None,
        }
    }
}

impl RenderOnce for PullRequestsUnavailableState {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let icon = Icon::new(IconName::GitPullRequestArrow)
            .size(rems(1.125))
            .into_any_element();
        Empty::new()
            .size(EmptySize::Default)
            .child(empty_media_icon(icon, cx))
            .child(
                empty_header()
                    .child(empty_title(self.title, false))
                    .child(empty_description(self.error, cx)),
            )
            .when_some(self.on_retry, |this, on_retry| {
                this.child(
                    div().flex().flex_wrap().justify_center().gap_2().child(refresh_button(
                        "pr-unavailable-retry",
                        "Retry",
                        self.refreshing,
                        on_retry,
                    )),
                )
            })
    }
}
