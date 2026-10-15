//! Port of upstream/apps/web/src/components/pullRequest/ThreadPullRequestsPanel.tsx: the pull
//! requests linked to a thread, as the list rows the page uses (PullRequestListRow.tsx shape) with a
//! stack indent, a hover menu (copy link, open, watch, unlink) and a footer with the counts and
//! the "Link" button.
//!
//! Not ported: speed mode (shift-held quick actions), the right-click context menu position
//! (the row's `…` menu opens from its button), the link dialog (`LinkPullRequestDialog`, opened
//! through [`ThreadPullRequestsPanelEvent::OpenLinkDialog`]) and the project lookup for actions.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, ClipboardItem, Context, CursorStyle, Entity, EventEmitter,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, rems,
};
use turin_contracts::ThreadId;
use turin_contracts::thread_pull_request::{
    ThreadPullRequestLink, ThreadPullRequestLinkSource,
};
use turin_contracts::pull_request::PullRequestState;

use super::dropdown::dropdown;
use super::presentation_view::{
    PullRequestActorAvatar, PullRequestChecksStateGlyph, PullRequestConflictGlyph,
    PullRequestDiffStat, PullRequestReviewDecisionGlyph, PullRequestStateGlyph, actor_tooltip,
    meta_text_size, with_tooltip,
};
use super::pull_request_list_lines::{PullRequestListLine, pull_request_list_lines};
use super::relative_time::format_relative_time_label;
use super::view_model::PullRequestStore;
use crate::state::support::thread_pull_requests::{
    ChainKind, resolve_thread_pull_request_chains, visible_thread_pull_requests,
};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_MD, TEXT_SM, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, scroll_area};

/// `SOURCE_LABELS`.
fn source_label(source: ThreadPullRequestLinkSource) -> &'static str {
    match source {
        ThreadPullRequestLinkSource::Manual => "Linked by you",
        ThreadPullRequestLinkSource::Created => "Created from this thread",
        ThreadPullRequestLinkSource::Agent => "Linked by the agent",
        ThreadPullRequestLinkSource::Stack => "Found in the stack",
        ThreadPullRequestLinkSource::StackDismissed => "Dismissed",
    }
}

/// Raised for the controls whose targets live outside this panel.
#[derive(Clone, Debug)]
pub enum ThreadPullRequestsPanelEvent {
    OpenLinkDialog,
    Open { url: String },
}

pub struct ThreadPullRequestsPanel {
    store: Entity<PullRequestStore>,
    thread_id: ThreadId,
}

impl EventEmitter<ThreadPullRequestsPanelEvent> for ThreadPullRequestsPanel {}

impl ThreadPullRequestsPanel {
    pub fn new(
        store: Entity<PullRequestStore>,
        thread_id: ThreadId,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        Self { store, thread_id }
    }
}

/// `PullRequestRowBranches`: `head → base` in mono, each cut when short of room.
fn branches(head: &str, base: &str) -> impl IntoElement {
    div()
        .flex()
        .min_w_0()
        .items_center()
        .gap_1()
        .font_family("monospace")
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(SharedString::from(head.to_owned())),
        )
        .child(div().flex_shrink_0().child("→"))
        .child(
            div()
                .max_w(gpui_kit::relative(0.45))
                .flex_shrink_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(SharedString::from(base.to_owned())),
        )
}

fn link_row(
    line: &PullRequestListLine<'_>,
    index: usize,
    store: &Entity<PullRequestStore>,
    thread_id: &ThreadId,
    panel: gpui_kit::WeakEntity<ThreadPullRequestsPanel>,
    cx: &App,
) -> AnyElement {
    let theme = cx.app_theme().clone();
    let link = line.link;
    let snapshot = link.snapshot.as_ref();
    let open = snapshot.is_none_or(|snapshot| snapshot.state == PullRequestState::Open);
    let watching = link.watch.is_some();
    let author = snapshot.and_then(|snapshot| snapshot.author.clone().flatten());
    let id = SharedString::from(format!("thread-pr-link-{index}"));
    let group = SharedString::from(format!("thread-pr-row-{index}"));

    let glyph = div()
        .flex()
        .flex_col()
        .items_center()
        .flex_shrink_0()
        .w(rems(1.))
        .child(match snapshot {
            None => with_tooltip(
                "pr-link-waiting",
                "Waiting for host state",
                Icon::new(IconName::GitPullRequestArrow)
                    .size(rems(1.))
                    .color(theme.muted_foreground),
            )
            .into_any_element(),
            Some(snapshot) => div()
                .relative()
                .flex()
                .child(PullRequestStateGlyph::new(snapshot.state, snapshot.is_draft))
                .child(
                    div().absolute().right(px(-4.)).bottom(px(-4.)).child(
                        PullRequestConflictGlyph::new(
                            snapshot.state,
                            snapshot.is_draft,
                            snapshot.mergeability,
                            Some(snapshot.base_branch.as_str().to_owned().into()),
                        )
                        .size(rems(0.75)),
                    ),
                )
                .into_any_element(),
        });

    let number = with_tooltip(
        "pr-link-number",
        format!(
            "{} · {}",
            source_label(link.source),
            format_relative_time_label(&link.linked_at)
        ),
        div()
            .flex_shrink_0()
            .font_family(theme.fonts.mono.clone())
            .text_size(TEXT_XS.size)
            .text_color(theme.muted_foreground)
            .child(format!("#{}", link.number.get())),
    );
    let title: SharedString = snapshot
        .map(|snapshot| snapshot.title.as_str().to_owned())
        .unwrap_or_else(|| link.repository.as_str().to_owned())
        .into();

    let signals = div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap_1()
        .text_size(meta_text_size())
        .when(open, |this| {
            this.when(watching, |this| {
                this.child(with_tooltip(
                    "pr-link-watching",
                    "Watching: the agent wakes when checks finish, someone comments, or the branch conflicts",
                    Icon::new(IconName::Eye).size(rems(0.875)),
                ))
            })
            .when_some(
                snapshot.and_then(|snapshot| snapshot.checks_state.flatten()),
                |this, state| this.child(PullRequestChecksStateGlyph::new(state)),
            )
            .when_some(
                snapshot.and_then(|snapshot| snapshot.review_decision.flatten()),
                |this, decision| this.child(PullRequestReviewDecisionGlyph::new(decision)),
            )
        });

    let status = PullRequestDiffStat::new(
        snapshot.and_then(|s| s.additions).unwrap_or(0),
        snapshot.and_then(|s| s.deletions).unwrap_or(0),
    )
    .mono();

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
                .child(title),
        )
        .child(signals)
        .child(div().flex_1())
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(rems(0.375))
                .text_size(meta_text_size())
                .child(status),
        );

    let stack_cell = line.stack.map(|stack| {
        with_tooltip(
            "pr-link-stack",
            match stack.kind {
                ChainKind::Native => format!(
                    "GitHub stack of {}: merging a layer lands the ones below it.",
                    stack.size
                ),
                ChainKind::Derived => format!("{} pull requests chained by base branch.", stack.size),
            },
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(2.))
                .text_color(with_alpha(theme.foreground, 0.7))
                .child(Icon::new(IconName::Layers).size(rems(0.75)))
                .child(stack.size.to_string()),
        )
    });
    let author_cell = author.as_ref().map(|actor| {
        with_tooltip(
            "pr-link-author",
            actor_tooltip(Some(actor)),
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap_1()
                .child(PullRequestActorAvatar::new(Some(actor)).size(px(14.)))
                .child(
                    div()
                        .max_w(rems(7.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(SharedString::from(actor.login.as_str().to_owned())),
                ),
        )
    });
    let line_two = div()
        .flex()
        .min_w_0()
        .items_center()
        .gap(rems(0.375))
        .overflow_hidden()
        .text_size(meta_text_size())
        .line_height(rems(1.))
        .text_color(theme.muted_foreground)
        .children(stack_cell)
        .children(author_cell)
        .map(|this| match snapshot {
            Some(snapshot) => this
                .child(
                    div()
                        .max_w(rems(8.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_family(theme.fonts.mono.clone())
                        .child(SharedString::from(link.repository.as_str().to_owned())),
                )
                .child(branches(snapshot.head_branch.as_str(), snapshot.base_branch.as_str())),
            None => this.child(
                div()
                    .truncate()
                    .font_family(theme.fonts.mono.clone())
                    .child(format!("{}/{}", link.host.as_str(), link.repository.as_str())),
            ),
        })
        .child(div().flex_1())
        .when_some(snapshot.and_then(|s| s.updated_at.as_deref()), |this, updated| {
            this.child(
                div()
                    .flex_shrink_0()
                    .whitespace_nowrap()
                    .child(format_relative_time_label(updated)),
            )
        });

    let url = link.url.as_str().to_owned();
    let url_for_copy = url.clone();
    let url_for_open = url.clone();
    let ref_for_menu = (link.host.as_str().to_owned(), link.repository.as_str().to_owned(), link.number);
    let dismiss = link.source == ThreadPullRequestLinkSource::Stack;
    let store_for_menu = store.clone();
    let thread_for_menu = thread_id.clone();
    let panel_for_open = panel.clone();
    let menu_button = Button::new(SharedString::from(format!("thread-pr-menu-{index}")))
        .variant(ButtonVariant::Ghost)
        .size(ButtonSize::IconMicro)
        .icon(IconName::Ellipsis)
        .icon_size(rems(0.875));
    let menu = dropdown(
        SharedString::from(format!("thread-pr-menu-{index}")),
        Anchor::TopRight,
        menu_button,
        move |menu, _, _| {
            let copy_url = url_for_copy.clone();
            let open_url = url_for_open.clone();
            let panel = panel_for_open.clone();
            let store = store_for_menu.clone();
            let thread = thread_for_menu.clone();
            let (host, repository, number) = ref_for_menu.clone();
            menu.item(
                gpui_kit::component::menu::PopupMenuItem::new("Copy link")
                    .icon(gpui_kit::component::Icon::new(IconName::Link))
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_url.clone()))
                    }),
            )
            .item(
                gpui_kit::component::menu::PopupMenuItem::new("Open")
                    .icon(gpui_kit::component::Icon::new(IconName::ExternalLink))
                    .on_click(move |_, _, cx| {
                        let url = open_url.clone();
                        panel
                            .update(cx, |_, cx| cx.emit(ThreadPullRequestsPanelEvent::Open { url }))
                            .ok();
                    }),
            )
            .item(
                gpui_kit::component::menu::PopupMenuItem::new(if dismiss {
                    "Dismiss from thread"
                } else {
                    "Unlink from thread"
                })
                .icon(gpui_kit::component::Icon::new(IconName::Unlink2))
                .on_click(move |_, _, cx| {
                    if let Some(source) = store.read(cx).source()
                        && let Some(reference) =
                            super::mock_source::reference_for(&host, &repository, number)
                    {
                        source.unlink_thread(&thread, &reference, cx);
                    }
                }),
            )
        },
    );

    // `paddingLeft: 0.5rem + min(depth, 3) * 1.25rem`; deeper layers get the connector line.
    let indent = rems(0.5 + line.depth.min(3) as f32 * 1.25);
    div()
        .id(id)
        .group(group.clone())
        .relative()
        .flex()
        .w_full()
        .items_center()
        .gap_2()
        .rounded(RADIUS_MD)
        .py_1()
        .pr_1()
        .pl(indent)
        .cursor(CursorStyle::PointingHand)
        .hover(|style| style.bg(with_alpha(theme.accent, 0.6)))
        .when(line.depth > 0, |this| {
            this.child(
                div()
                    .flex_shrink_0()
                    .ml(rems(-0.5))
                    .h_6()
                    .w(px(1.))
                    .bg(with_alpha(theme.border, 0.7)),
            )
        })
        .child(glyph)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(line_one)
                .child(line_two),
        )
        // The hover menu sits over the right end of the second line on the row's own hover
        // color; only the hovered row shows it.
        .child(
            div()
                .absolute()
                .right_0()
                .bottom(px(2.))
                .flex()
                .items_center()
                .rounded_r(RADIUS_MD)
                .bg(theme.background)
                .pr_1()
                .pl(rems(1.25))
                .opacity(0.)
                .group_hover(group, |style| style.opacity(1.))
                .child(menu),
        )
        .into_any_element()
}

impl Render for ThreadPullRequestsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let links: Vec<ThreadPullRequestLink> = self.store.read(cx).thread_links(&self.thread_id).to_vec();
        let visible = visible_thread_pull_requests(&links);
        if visible.is_empty() {
            let panel = cx.entity().downgrade();
            return div()
                .flex()
                .flex_col()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .p(rems(1.5))
                .text_center()
                .child(
                    Icon::new(IconName::Link2)
                        .size(rems(1.5))
                        .color(with_alpha(theme.muted_foreground, 0.6).into()),
                )
                .child(
                    div()
                        .text_size(TEXT_SM.size)
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child("No linked pull requests"),
                )
                .child(
                    div()
                        .max_w(rems(15.))
                        .text_size(TEXT_XS.size)
                        .text_color(theme.muted_foreground)
                        .child("Pull requests the agent opens from this thread land here. Link one yourself from a URL or a number."),
                )
                .child(
                    Button::new("thread-pr-empty-link")
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Sm)
                        .icon(IconName::Plus)
                        .icon_size(rems(0.875))
                        .label("Link pull request")
                        .on_click(move |_, _, cx| {
                            panel
                                .update(cx, |_, cx| cx.emit(ThreadPullRequestsPanelEvent::OpenLinkDialog))
                                .ok();
                        }),
                )
                .into_any_element();
        }
        let chains = resolve_thread_pull_request_chains(&links);
        let lines = pull_request_list_lines(&chains);
        let open_count = visible
            .iter()
            .filter(|link| {
                link.snapshot
                    .as_ref()
                    .is_none_or(|snapshot| snapshot.state == PullRequestState::Open)
            })
            .count();
        let last_synced = visible
            .iter()
            .filter_map(|link| link.snapshot.as_ref().map(|snapshot| snapshot.synced_at.as_str()))
            .max();
        let panel = cx.entity().downgrade();
        let rows: Vec<AnyElement> = lines
            .iter()
            .enumerate()
            .map(|(index, line)| link_row(line, index, &self.store, &self.thread_id, panel.clone(), cx))
            .collect();
        let footer_text = format!(
            "{} open · {} linked{}",
            open_count,
            visible.len(),
            last_synced
                .map(|at| format!(" · synced {}", format_relative_time_label(at)))
                .unwrap_or_default()
        );
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .child(
                div().flex_1().min_h_0().child(
                    scroll_area("thread-pr-scroll")
                        .child(div().flex().flex_col().p(rems(0.375)).children(rows)),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .border_t_1()
                    .border_color(with_alpha(theme.border, 0.6))
                    .px_2()
                    .py(rems(0.375))
                    .text_size(meta_text_size())
                    .text_color(theme.muted_foreground)
                    .child(footer_text)
                    .child(
                        Button::new("thread-pr-footer-link")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Xs)
                            .icon(IconName::Plus)
                            .icon_size(rems(0.875))
                            .label("Link")
                            .on_click(move |_, _, cx| {
                                panel
                                    .update(cx, |_, cx| {
                                        cx.emit(ThreadPullRequestsPanelEvent::OpenLinkDialog)
                                    })
                                    .ok();
                            }),
                    ),
            )
            .into_any_element()
    }
}

#[allow(dead_code)]
fn _rc(_: Rc<()>) {}
