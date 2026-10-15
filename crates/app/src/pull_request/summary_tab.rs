//! Port of upstream/apps/web/src/components/pullRequest/PullRequestSummaryTab.tsx: the reviewers
//! and labels rows, then the Description, Checks and Comments sections.
//!
//! The tab reads the store through [`DetailContext`] and sends edits to its source. It reports
//! how far it is scrolled ([`SummaryTabEvent::Scrolled`]) so the panel can fold its header, as
//! upstream's `onScrollCapture` does.
//!
//! Not ported: the sticky section headings (GPUI has no `position: sticky`, so a heading scrolls
//! with its section and `sectionCollapseAnchorScrollTop` is not needed), the "Fix in a thread" /
//! per-check "Fix" buttons (they hand a finding to a new thread), the persisted comment filter
//! (`useLocalStorage`; it lives for the life of the tab), the `content-visibility` skips (the
//! tab renders the comments it shows, ten at a time), the check-details link opening in the
//! thread's in-app browser (always the system browser), and the stale-checks banner (the panel
//! never marks the detail's checks stale).

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::LazyLock;

use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, Context, CursorStyle, Entity, EventEmitter,
    FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
    div, px, rems,
};
use regex::Regex;
use turin_contracts::pull_request::{
    PullRequestActor, PullRequestComment, PullRequestCommentKind, PullRequestCommentUpdateKind,
    PullRequestDetailView, PullRequestReviewThread,
};

use super::comment_body::{
    CommentBody, EditButton, PULL_REQUEST_SUBJECT, ReactionBar, pull_request_markdown,
};
use super::comment_form::{MarkdownEditor, MarkdownEditorEvent};
use super::detail_context::DetailContext;
use super::detail_layout::{alpha, conversation_ghost, meta_row};
use super::detail_panel::editing_subject;
use super::dropdown::dropdown;
use super::pickers::{PullRequestLabelPicker, PullRequestReviewerPicker};
use super::presentation_view::{
    PullRequestActorAvatar, PullRequestActorLabel, PullRequestCheckStatusIcon,
    PullRequestLabelChip, PullRequestReviewOutcomeBadge, actor_tooltip, tone_color,
};
use super::pull_request_detail_logic::{
    PullRequestCommentOrder, PullRequestReviewOutcome, latest_pull_request_review_outcomes,
    order_pull_request_comments, pull_request_review_outcome, visible_body,
};
use super::pull_request_editing_logic::can_edit_pull_request_comment;
use super::pull_request_presentation::{
    pull_request_check_status_label, pull_request_review_outcome_label,
    pull_request_review_outcome_stale_label,
};
use super::relative_time::format_relative_time_label;
use super::view_model::{Loadable, PullRequestDetailSnapshot};
use crate::theme::color::{MixSpace, color_mix};
use crate::theme::tokens::{RADIUS_LG, RADIUS_MD, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

/// What the first render of the conversation carries. A pull request with two hundred comments is
/// two hundred markdown documents, and the ones worth arriving for are the recent ones.
const COMMENT_PAGE: usize = 10;

/// Raised as the reader scrolls, with the distance from the top in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SummaryTabEvent {
    Scrolled(f32),
}

/// Which kinds of comment the conversation shows (`COMMENT_FILTER_STORAGE_KEY`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommentFilter {
    pub bots: bool,
    pub resolved: bool,
}

/// The three collapsible sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    Description,
    Checks,
    Comments,
}

pub struct SummaryTab {
    pub ctx: DetailContext,
    scroll: ScrollHandle,
    last_top: f32,
    open: [bool; 3],
    shown_comments: usize,
    filter: CommentFilter,
    order: PullRequestCommentOrder,
    /// Finished comments the reader has opened (`CollapsedComment`'s `open`).
    expanded: HashSet<String>,
    body_editor: Option<Entity<MarkdownEditor>>,
    comment_editor: Option<(String, Entity<MarkdownEditor>)>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SummaryTabEvent> for SummaryTab {}

impl SummaryTab {
    pub fn new(ctx: DetailContext, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&ctx.store, |_, _, cx| cx.notify());
        let scroll = ScrollHandle::new();
        // Dev hook for screenshots: start scrolled down, which folds the panel's header.
        if std::env::var("TURIN_PR_OPEN").is_ok_and(|wanted| wanted == "detail-scrolled") {
            scroll.set_offset(gpui_kit::point(px(0.), px(-420.)));
        }
        Self {
            ctx,
            scroll,
            last_top: 0.,
            open: [true, false, true],
            shown_comments: COMMENT_PAGE,
            filter: CommentFilter::default(),
            order: PullRequestCommentOrder::Newest,
            expanded: HashSet::new(),
            body_editor: None,
            comment_editor: None,
            _subscriptions: vec![subscription],
        }
    }

    /// The tab's scroll position, for the panel's header fold.
    pub fn scroll_handle(&self) -> &ScrollHandle {
        &self.scroll
    }

    fn section_open(&self, section: Section) -> bool {
        self.open[section as usize]
    }

    fn toggle_section(&mut self, section: Section, cx: &mut Context<Self>) {
        self.open[section as usize] = !self.open[section as usize];
        cx.notify();
    }

    fn begin_body_edit(&mut self, body: &str, window: &mut Window, cx: &mut Context<Self>) {
        let editor = cx.new(|cx| {
            MarkdownEditor::new(
                "pr-description-editor",
                body,
                Some("Describe this pull request"),
                window,
                cx,
            )
            .allow_empty(true)
        });
        let subscription =
            cx.subscribe(
                &editor,
                |this, _, event: &MarkdownEditorEvent, cx| match event {
                    MarkdownEditorEvent::Save(body) => {
                        if let Some(source) = this.ctx.source(cx) {
                            source.update_body(&this.ctx.reference, body.clone(), cx);
                        }
                        this.body_editor = None;
                        cx.notify();
                    }
                    MarkdownEditorEvent::Cancel => {
                        this.body_editor = None;
                        cx.notify();
                    }
                },
            );
        self._subscriptions.push(subscription);
        self.body_editor = Some(editor);
        cx.notify();
    }

    fn begin_comment_edit(
        &mut self,
        comment: &PullRequestComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = comment.id.as_str().to_owned();
        let kind = match comment.kind {
            PullRequestCommentKind::ReviewComment => PullRequestCommentUpdateKind::ReviewComment,
            // A review's own summary is never rewritten; the pencil is not offered on one.
            _ => PullRequestCommentUpdateKind::IssueComment,
        };
        let editor_id = SharedString::from(format!("pr-comment-editor-{id}"));
        let editor = cx.new(|cx| MarkdownEditor::new(editor_id, &comment.body, None, window, cx));
        let target = id.clone();
        let subscription = cx.subscribe(
            &editor,
            move |this, _, event: &MarkdownEditorEvent, cx| match event {
                MarkdownEditorEvent::Save(body) => {
                    if let Some(source) = this.ctx.source(cx) {
                        source.update_comment(&this.ctx.reference, &target, kind, body.clone(), cx);
                    }
                    this.comment_editor = None;
                    cx.notify();
                }
                MarkdownEditorEvent::Cancel => {
                    this.comment_editor = None;
                    cx.notify();
                }
            },
        );
        self._subscriptions.push(subscription);
        self.comment_editor = Some((id, editor));
        cx.notify();
    }
}

/// One reviewer, however a host happens to have cased their login this time.
fn reviewer_key(login: &str) -> String {
    login.to_lowercase()
}

/// `reviewStateLabel`: "CHANGES_REQUESTED" reads as "Changes requested".
pub fn review_state_label(state: &str) -> String {
    let words = state.to_lowercase().replace('_', " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

static COMMENT_MARKERS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<!--.*?-->").expect("static pattern"));
static ALERT_LINES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t\r\f\v]*>?[ \t\r\f\v]*\[!\w+\][ \t\r\f\v]*$").expect("static pattern")
});
static LINKS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!?(\[([^\]]+)\])\([^)]*\)").expect("static pattern"));
static LEADING_MARKS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^[\s>#*-]+").expect("static pattern"));
static EMPHASIS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[*`]").expect("static pattern"));
static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("static pattern"));

/// The one-line preview a finished comment shows while collapsed: its body with the markdown
/// stripped away.
pub fn collapsed_preview(body: &str) -> String {
    let text = COMMENT_MARKERS.replace_all(body, "");
    let text = ALERT_LINES.replace_all(&text, "");
    let text = LINKS.replace_all(&text, "$2");
    let text = LEADING_MARKS.replace_all(&text, "");
    let text = EMPHASIS.replace_all(&text, "");
    let text = WHITESPACE.replace_all(&text, " ");
    text.trim().to_owned()
}

/// `new Date(iso).toLocaleString()` in the en-US shape.
pub fn locale_string(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|date| {
            date.with_timezone(&chrono::Local)
                .format("%-m/%-d/%Y, %-I:%M:%S %p")
                .to_string()
        })
        .unwrap_or_else(|_| iso.to_owned())
}

/// The ring a verdict puts around a face (`pullRequestReviewOutcomeRingClassName`): opaque, so a
/// stale verdict is mixed into the background instead of being made translucent.
pub fn outcome_ring_color(
    outcome: PullRequestReviewOutcome,
    stale: bool,
    theme: &AppTheme,
) -> Hsla {
    let dark = theme.is_dark();
    let base = match outcome {
        PullRequestReviewOutcome::Approved => tone_color(
            if dark {
                "text-emerald-400"
            } else {
                "text-emerald-500"
            },
            theme,
        ),
        PullRequestReviewOutcome::ChangesRequested => theme.destructive,
        PullRequestReviewOutcome::Dismissed => alpha(theme.muted_foreground, 0.6),
    };
    if stale {
        let weight = if outcome == PullRequestReviewOutcome::Dismissed {
            0.3
        } else {
            0.35
        };
        color_mix(MixSpace::Srgb, base, weight, theme.background).into()
    } else {
        base
    }
}

/// One person in the reviewers stack, with the verdict their face earned.
struct ReviewerEntry {
    key: String,
    actor: Option<PullRequestActor>,
    outcome: Option<PullRequestReviewOutcome>,
    stale: bool,
}

/// Everyone whose face belongs on the row: the people a review was asked of, then anyone who
/// ruled without being on that list. A host drops a reviewer from the requested set once they have
/// reviewed, and their verdict is the thing this row now exists to show.
fn reviewer_entries(detail: &PullRequestDetailView) -> Vec<ReviewerEntry> {
    let outcomes = latest_pull_request_review_outcomes(&detail.comments, &detail.commits);
    let by_login = |login: &str| {
        outcomes.iter().find(|entry| {
            entry
                .actor
                .as_ref()
                .is_some_and(|actor| reviewer_key(actor.login.as_str()) == reviewer_key(login))
        })
    };
    let mut entries: Vec<ReviewerEntry> = detail
        .reviewers
        .iter()
        .map(|actor| {
            let outcome = by_login(actor.login.as_str());
            ReviewerEntry {
                key: actor.login.as_str().to_owned(),
                actor: Some(actor.clone()),
                outcome: outcome.map(|entry| entry.outcome),
                stale: outcome.is_some_and(|entry| entry.stale),
            }
        })
        .collect();
    for entry in &outcomes {
        let listed = entry.actor.as_ref().is_some_and(|actor| {
            detail.reviewers.iter().any(|listed| {
                reviewer_key(listed.login.as_str()) == reviewer_key(actor.login.as_str())
            })
        });
        if !listed {
            entries.push(ReviewerEntry {
                key: entry.key.clone(),
                actor: entry.actor.clone(),
                outcome: Some(entry.outcome),
                stale: entry.stale,
            });
        }
    }
    entries
}

fn is_bot(comment: &PullRequestComment) -> bool {
    comment.author.as_ref().is_some_and(|author| {
        author.is_bot == Some(true) || author.login.as_str().ends_with("[bot]")
    })
}

impl Render for SummaryTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let top = -f32::from(self.scroll.offset().y);
        if (top - self.last_top).abs() >= 1. {
            self.last_top = top;
            cx.emit(SummaryTabEvent::Scrolled(top));
        }
        let scroll = div()
            .id("pr-summary-scroll")
            .size_full()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .vertical_scrollbar(&self.scroll);
        let Some(snapshot) = self.ctx.snapshot(cx).cloned_light() else {
            return scroll.into_any_element();
        };
        let Loadable::Ready(detail) = snapshot.detail else {
            return scroll.into_any_element();
        };
        let can_write = snapshot.can_write;
        let activity_pending = snapshot.activity_pending;

        let weak = cx.entity().downgrade();
        let entries = reviewer_entries(&detail);

        // ── Reviewers and labels ───────────────────────────────────────────────────────────────
        let reviewers_value = div()
            .flex()
            .flex_wrap()
            .min_w_0()
            .items_center()
            .gap(rems(0.375))
            .child(if entries.is_empty() {
                div()
                    .text_color(theme.muted_foreground)
                    .child("None")
                    .into_any_element()
            } else {
                div()
                    .relative()
                    .flex_shrink_0()
                    .h(px(16.))
                    .w(px(16. + 12. * (entries.len() as f32 - 1.)))
                    .children(
                        entries
                            .iter()
                            .enumerate()
                            .map(|(index, entry)| reviewer_face(entry, index, &theme)),
                    )
                    .into_any_element()
            })
            .when(
                detail.capabilities.reviewers.request
                    && detail.capabilities.reviewers.list_candidates,
                |this| {
                    this.child(PullRequestReviewerPicker::new(
                        self.ctx.clone(),
                        can_write && detail.viewer_permissions.request_reviewers,
                    ))
                },
            );
        let labels_value = div()
            .flex()
            .flex_wrap()
            .min_w_0()
            .items_center()
            .gap_1()
            .child(if detail.labels.is_empty() {
                div()
                    .text_color(theme.muted_foreground)
                    .child("None")
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .children(detail.labels.iter().map(|label| {
                        PullRequestLabelChip::new(label.clone())
                            .size(crate::ui::BadgeSize::Default)
                            .max_width(px(192.))
                            .into_any_element()
                    }))
                    .into_any_element()
            })
            .when(detail.capabilities.labels == Some(true), |this| {
                this.child(PullRequestLabelPicker::new(
                    self.ctx.clone(),
                    can_write && detail.viewer_permissions.labels != Some(false),
                ))
            });
        let facts = div()
            .flex()
            .flex_col()
            .gap_2()
            .px_4()
            .pt(rems(0.625))
            .pb_1()
            .child(meta_row(
                IconName::Users,
                "Reviewers",
                reviewers_value,
                &theme,
            ))
            .when(
                !detail.labels.is_empty() || detail.capabilities.labels == Some(true),
                |this| this.child(meta_row(IconName::Tag, "Labels", labels_value, &theme)),
            );

        // ── Description ────────────────────────────────────────────────────────────────────────
        let subject = editing_subject(&detail, can_write);
        let can_edit_description =
            super::pull_request_editing_logic::can_edit_pull_request_change_request(&subject);
        let description: AnyElement = match self.body_editor.clone() {
            Some(editor) => editor.into_any_element(),
            None => {
                let text = if detail.body.trim().is_empty() {
                    "_No description provided._".to_owned()
                } else {
                    detail.body.clone()
                };
                let body = detail.body.clone();
                div()
                    .id("pr-description")
                    .group("pr-description")
                    .flex()
                    .items_start()
                    .gap_1()
                    .child(div().min_w_0().flex_1().child(pull_request_markdown(
                        SharedString::from(format!("pr-description-{}", detail.url.as_str())),
                        text,
                    )))
                    .when(can_edit_description, |this| {
                        this.child(
                            EditButton::new(
                                "pr-edit-description",
                                "pr-description",
                                "Edit description",
                            )
                            .on_click(move |window, cx| {
                                let body = body.clone();
                                weak.update(cx, |tab, cx| tab.begin_body_edit(&body, window, cx))
                                    .ok();
                            }),
                        )
                    })
                    .into_any_element()
            }
        };
        let _ = PULL_REQUEST_SUBJECT;

        let checks = self.render_checks(&detail, cx);
        let comments = self.render_comments(
            &detail,
            &snapshot_flags(can_write, activity_pending),
            window,
            cx,
        );
        let comment_count = detail.comment_count;
        let comments_actions = self.render_comment_actions(&detail, cx);

        scroll
            .child(facts)
            .child(self.section(
                Section::Description,
                "Description",
                None,
                description,
                &theme,
                cx,
            ))
            .child(self.section(Section::Checks, "Checks", None, checks, &theme, cx))
            .child(self.section(
                Section::Comments,
                &format!("Comments ({comment_count})"),
                Some(comments_actions),
                comments,
                &theme,
                cx,
            ))
            .into_any_element()
    }
}

/// The few snapshot flags the comment rows read.
struct SnapshotFlags {
    can_write: bool,
    activity_pending: bool,
}

fn snapshot_flags(can_write: bool, activity_pending: bool) -> SnapshotFlags {
    SnapshotFlags {
        can_write,
        activity_pending,
    }
}

/// A snapshot without its heavy halves cloned twice: the render clones the ready detail once.
trait ClonedLight {
    fn cloned_light(self) -> Option<PullRequestDetailSnapshot>;
}

impl ClonedLight for Option<&PullRequestDetailSnapshot> {
    fn cloned_light(self) -> Option<PullRequestDetailSnapshot> {
        self.map(|snapshot| PullRequestDetailSnapshot {
            detail: snapshot.detail.clone(),
            activity_pending: snapshot.activity_pending,
            can_write: snapshot.can_write,
            ..PullRequestDetailSnapshot::default()
        })
    }
}

/// One face of the reviewers stack: 16px avatar, a 2px ring outside it (the verdict's colour, or
/// the background that separates overlapping faces), overlapping its neighbour by 4px.
fn reviewer_face(entry: &ReviewerEntry, index: usize, theme: &AppTheme) -> AnyElement {
    let login = entry
        .actor
        .as_ref()
        .map_or("ghost", |actor| actor.login.as_str())
        .to_owned();
    let named = match entry.actor.as_ref() {
        Some(actor) => actor_tooltip(Some(actor)),
        None => login.clone(),
    };
    let tooltip = match entry.outcome {
        Some(outcome) => format!(
            "{named} — {}",
            if entry.stale {
                pull_request_review_outcome_stale_label(outcome)
            } else {
                pull_request_review_outcome_label(outcome).to_owned()
            }
        ),
        None => named,
    };
    let ring = entry.outcome.map_or(theme.background, |outcome| {
        outcome_ring_color(outcome, entry.stale, theme)
    });
    div()
        .id(SharedString::from(format!(
            "pr-reviewer-{index}-{}",
            entry.key
        )))
        .absolute()
        .top_0()
        .left(px(12. * index as f32))
        .flex()
        .flex_shrink_0()
        .size(px(16.))
        .rounded_full()
        .hover(|style| style.opacity(1.))
        .child(
            div()
                .absolute()
                .top(px(-2.))
                .left(px(-2.))
                .size(px(20.))
                .rounded_full()
                .border_2()
                .border_color(ring),
        )
        .child(PullRequestActorAvatar::new(entry.actor.as_ref()))
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .into_any_element()
}

impl SummaryTab {
    /// `Section`: a heading that collapses its body, with the heading's own controls beside it.
    fn section(
        &self,
        section: Section,
        title: &str,
        actions: Option<AnyElement>,
        body: AnyElement,
        theme: &AppTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.section_open(section);
        let weak = cx.entity().downgrade();
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .w_full()
                    .items_center()
                    .bg(theme.background)
                    .pr_4()
                    .child(
                        div()
                            .id(SharedString::from(format!("pr-section-{section:?}")))
                            .group(SharedString::from(format!("pr-section-{section:?}")))
                            .flex()
                            .min_w_0()
                            .flex_1()
                            .items_center()
                            .gap(rems(0.375))
                            .px_4()
                            .py_3()
                            .cursor(CursorStyle::PointingHand)
                            .text_size(TEXT_XS.size)
                            .line_height(TEXT_XS.line_height)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .hover({
                                let foreground = theme.foreground;
                                move |style| style.text_color(foreground)
                            })
                            .child(SharedString::from(title.to_owned()))
                            .child(
                                Icon::new(if open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .size(rems(0.875))
                                .color(alpha(theme.muted_foreground, 0.6)),
                            )
                            .on_click(move |_, _, cx| {
                                weak.update(cx, |tab, cx| tab.toggle_section(section, cx))
                                    .ok();
                            }),
                    )
                    .children(actions),
            )
            .when(open, |this| this.child(div().px_4().pb_4().child(body)))
            .into_any_element()
    }

    /// The Checks section: every check with its status and a link to its details.
    fn render_checks(&self, detail: &PullRequestDetailView, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        if detail.checks.is_empty() {
            return div()
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .child("No checks reported.")
                .into_any_element();
        }
        let source = self.ctx.source(cx);
        div()
            .flex()
            .flex_col()
            .children(detail.checks.iter().enumerate().map(|(index, check)| {
                let url = check.url.clone();
                let source = source.clone();
                let id = SharedString::from(format!("pr-check-{index}"));
                let group = SharedString::from(format!("pr-check-row-{index}"));
                div()
                    .group(group.clone())
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(RADIUS_MD)
                    .pr_1()
                    .hover({
                        let hover = alpha(theme.accent, 0.6);
                        move |style| style.bg(hover)
                    })
                    .child(
                        div()
                            .id(id.clone())
                            .flex()
                            .min_w_0()
                            .flex_1()
                            .items_start()
                            .gap_2()
                            .rounded(RADIUS_MD)
                            .px_2()
                            .py_2()
                            .text_size(TEXT_XS.size)
                            .line_height(rems(1.25))
                            .when(url.is_some(), |this| this.cursor(CursorStyle::PointingHand))
                            .child(div().mt(px(2.)).child(PullRequestCheckStatusIcon::new(
                                SharedString::from(format!("{id}-icon")),
                                check.status,
                            )))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .child(SharedString::from(check.name.as_str().to_owned())),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(theme.muted_foreground)
                                    .child(pull_request_check_status_label(check)),
                            )
                            .on_click(move |_, _, cx| {
                                if let Some(url) = url.as_deref() {
                                    match source.as_ref() {
                                        Some(source) => source.open_external(url, cx),
                                        None => cx.open_url(url),
                                    }
                                }
                            }),
                    )
            }))
            .into_any_element()
    }

    /// The Comments heading's controls: the filter menu and the order toggle.
    fn render_comment_actions(
        &self,
        detail: &PullRequestDetailView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let weak = cx.entity().downgrade();
        let filtered_out = detail.comments.len() - self.filtered_comments(detail).len();
        let filter = self.filter;
        let menu_weak = weak.clone();
        let filter_button = Button::new("pr-comment-filter")
            .size(ButtonSize::Xs)
            .variant(ButtonVariant::GhostMuted)
            .icon(IconName::ListFilter)
            .icon_size(rems(0.75))
            .label(if filtered_out > 0 {
                format!("{filtered_out} hidden")
            } else {
                "Filter".to_owned()
            });
        let menu = dropdown(
            "pr-comment-filter",
            Anchor::TopRight,
            filter_button,
            move |menu, _, _| {
                let bots = menu_weak.clone();
                let resolved = menu_weak.clone();
                menu.label("Show")
                    .item(
                        PopupMenuItem::new("Bot comments")
                            .checked(filter.bots)
                            .on_click(move |_, _, cx| {
                                bots.update(cx, |tab, cx| {
                                    tab.filter.bots = !tab.filter.bots;
                                    cx.notify();
                                })
                                .ok();
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Resolved or dismissed")
                            .checked(filter.resolved)
                            .on_click(move |_, _, cx| {
                                resolved
                                    .update(cx, |tab, cx| {
                                        tab.filter.resolved = !tab.filter.resolved;
                                        cx.notify();
                                    })
                                    .ok();
                            }),
                    )
            },
        );
        let order = self.order;
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .child(menu)
            .child(
                Button::new("pr-comment-order")
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::GhostMuted)
                    .icon(IconName::ArrowDownUp)
                    .icon_size(rems(0.75))
                    .label(match order {
                        PullRequestCommentOrder::Newest => "Newest first",
                        PullRequestCommentOrder::Oldest => "Oldest first",
                    })
                    .on_click(move |_, _, cx| {
                        weak.update(cx, |tab, cx| {
                            tab.order = match tab.order {
                                PullRequestCommentOrder::Newest => PullRequestCommentOrder::Oldest,
                                PullRequestCommentOrder::Oldest => PullRequestCommentOrder::Newest,
                            };
                            cx.notify();
                        })
                        .ok();
                    }),
            )
            .into_any_element()
    }

    fn thread_of<'a>(
        detail: &'a PullRequestDetailView,
        comment_id: &str,
    ) -> Option<&'a PullRequestReviewThread> {
        detail.review_threads.iter().find(|thread| {
            thread
                .comments
                .iter()
                .any(|comment| comment.id.as_str() == comment_id)
        })
    }

    /// A comment that lives on a resolved thread, or a dismissed review, is finished work.
    fn is_finished(detail: &PullRequestDetailView, comment: &PullRequestComment) -> bool {
        Self::thread_of(detail, comment.id.as_str()).is_some_and(|thread| thread.is_resolved)
            || pull_request_review_outcome(comment.review_state.as_deref())
                == Some(PullRequestReviewOutcome::Dismissed)
    }

    fn filtered_comments(&self, detail: &PullRequestDetailView) -> Vec<PullRequestComment> {
        detail
            .comments
            .iter()
            .filter(|comment| {
                (self.filter.bots || !is_bot(comment))
                    && (self.filter.resolved || !Self::is_finished(detail, comment))
            })
            .cloned()
            .collect()
    }

    /// The conversation: the most recent page of the comments the filter lets through.
    fn render_comments(
        &mut self,
        detail: &PullRequestDetailView,
        flags: &SnapshotFlags,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        if flags.activity_pending {
            return conversation_ghost(3, cx);
        }
        let weak = cx.entity().downgrade();
        let filtered = self.filtered_comments(detail);
        let filtered_out = detail.comments.len() - filtered.len();
        // Windowed by recency regardless of display order: expanding always reaches further back
        // in time, whether the newest comment currently reads first or last.
        let recent: Vec<PullRequestComment> =
            filtered[filtered.len().saturating_sub(self.shown_comments)..].to_vec();
        let hidden = filtered.len() - recent.len();
        let visible = order_pull_request_comments(&recent, self.order);
        let step = self.shown_comments + COMMENT_PAGE;
        let older_button = || {
            if hidden == 0 {
                return None;
            }
            let weak = weak.clone();
            Some(
                Button::new("pr-older-comments")
                    .size(ButtonSize::Sm)
                    .variant(ButtonVariant::Outline)
                    .label(format!(
                        "Show {} older comment{} ({} hidden)",
                        hidden.min(COMMENT_PAGE),
                        if hidden == 1 { "" } else { "s" },
                        hidden
                    ))
                    .w_full()
                    .on_click(move |_, _, cx| {
                        weak.update(cx, |tab, cx| {
                            tab.shown_comments = step;
                            cx.notify();
                        })
                        .ok();
                    })
                    .into_any_element(),
            )
        };

        let mut column = div().flex().flex_col().gap_3();
        if detail.comments_truncated {
            column = column.child(
                div()
                    .mb_2()
                    .rounded(RADIUS_MD)
                    .border_1()
                    .border_color(alpha(theme.warning, 0.3))
                    .bg(theme.warning_surface)
                    .px_2()
                    .py(rems(0.375))
                    .text_size(TEXT_XS.size)
                    .child(format!(
                        "This conversation is longer than this page reads in one go. The most recent {} are here; open it on the host to read the rest.",
                        detail.comments.len()
                    )),
            );
        }
        if detail.comments.is_empty() {
            return div()
                .py_2()
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .child("No comments yet.")
                .into_any_element();
        }
        if filtered.is_empty() {
            return div()
                .py_2()
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .child(if filtered_out == 1 {
                    "The only comment is hidden by the filter.".to_owned()
                } else {
                    format!("All {filtered_out} comments are hidden by the filter.")
                })
                .into_any_element();
        }
        if self.order == PullRequestCommentOrder::Oldest {
            column = column.children(older_button());
        }
        let rows: Vec<AnyElement> = visible
            .iter()
            .map(|comment| self.render_comment(detail, comment, flags, &theme, window, cx))
            .collect();
        column = column.children(rows);
        if self.order == PullRequestCommentOrder::Newest {
            column = column.children(older_button());
        }
        if self.shown_comments > COMMENT_PAGE {
            column = column.child(
                Button::new("pr-recent-comments")
                    .size(ButtonSize::Sm)
                    .variant(ButtonVariant::Ghost)
                    .label(format!("Show only {COMMENT_PAGE} recent comments"))
                    .w_full()
                    .on_click(move |_, _, cx| {
                        weak.update(cx, |tab, cx| {
                            tab.shown_comments = COMMENT_PAGE;
                            cx.notify();
                        })
                        .ok();
                    }),
            );
        }
        column.into_any_element()
    }

    /// `CommentIdentity`: the author and when, the time linking to the comment on the host.
    fn comment_identity(
        &self,
        detail: &PullRequestDetailView,
        comment: &PullRequestComment,
        theme: &AppTheme,
        cx: &App,
    ) -> AnyElement {
        let _ = detail;
        let created = comment.created_at.clone();
        let url = comment.url.clone();
        let tooltip = format!(
            "{}{}",
            locale_string(&created),
            if url.is_some() {
                " · Open comment on host"
            } else {
                ""
            }
        );
        let source = self.ctx.source(cx);
        div()
            .flex()
            .flex_wrap()
            .min_w_0()
            .flex_1()
            .items_center()
            .gap_x_2()
            .gap_y_1()
            .text_size(TEXT_XS.size)
            .child(PullRequestActorLabel::new(comment.author.clone()))
            .child(
                div()
                    .id(SharedString::from(format!(
                        "pr-comment-time-{}",
                        comment.id.as_str()
                    )))
                    .text_color(theme.muted_foreground)
                    .when(url.is_some(), |this| {
                        let foreground = theme.foreground;
                        this.cursor(CursorStyle::PointingHand)
                            .hover(move |style| style.text_color(foreground).underline())
                    })
                    .child(format_relative_time_label(&created))
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .on_click(move |_, _, cx| {
                        if let Some(url) = url.as_deref() {
                            match source.as_ref() {
                                Some(source) => source.open_external(url, cx),
                                None => cx.open_url(url),
                            }
                        }
                    }),
            )
            .into_any_element()
    }

    /// `CommentLocation`: the file and line a review remark is about.
    fn comment_location(
        comment: &PullRequestComment,
        thread: Option<&PullRequestReviewThread>,
        theme: &AppTheme,
    ) -> Option<AnyElement> {
        let path = thread
            .map(|thread| thread.path.as_str().to_owned())
            .or_else(|| comment.path.clone())?;
        let label = match thread.and_then(|thread| thread.line) {
            Some(line) => format!("{path}:{}", line.get()),
            None => path,
        };
        let outdated = thread.is_some_and(|thread| thread.is_outdated);
        Some(
            div()
                .flex()
                .mt_2()
                .min_w_0()
                .items_center()
                .gap_2()
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "pr-comment-path-{}",
                            comment.id.as_str()
                        )))
                        .min_w_0()
                        .truncate()
                        .font_family(theme.fonts.mono.clone())
                        .child(SharedString::from(label.clone()))
                        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx)),
                )
                .when(outdated, |this| {
                    this.child(div().flex_shrink_0().child("Outdated"))
                })
                .into_any_element(),
        )
    }

    /// `CommentBody`: a remark's words, and the pencil that swaps them for the editor.
    fn comment_body(
        &mut self,
        detail: &PullRequestDetailView,
        comment: &PullRequestComment,
        can_edit: bool,
        group: SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let _ = detail;
        if let Some((editing, editor)) = &self.comment_editor
            && editing == comment.id.as_str()
        {
            return editor.clone().into_any_element();
        }
        let weak = cx.entity().downgrade();
        let target = comment.clone();
        div()
            .flex()
            .items_start()
            .gap_1()
            .child(div().min_w_0().flex_1().child(CommentBody::new(
                SharedString::from(format!("pr-comment-body-{}", comment.id.as_str())),
                comment.body.clone(),
            )))
            .when(can_edit, |this| {
                this.child(
                    EditButton::new(
                        SharedString::from(format!("pr-edit-comment-{}", comment.id.as_str())),
                        group,
                        "Edit comment",
                    )
                    .on_click(move |window, cx| {
                        let target = target.clone();
                        weak.update(cx, |tab, cx| tab.begin_comment_edit(&target, window, cx))
                            .ok();
                    }),
                )
            })
            .into_any_element()
    }

    fn render_comment(
        &mut self,
        detail: &PullRequestDetailView,
        comment: &PullRequestComment,
        flags: &SnapshotFlags,
        theme: &AppTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let _ = window;
        let thread = Self::thread_of(detail, comment.id.as_str());
        let body = visible_body(&comment.body);
        let outcome = pull_request_review_outcome(comment.review_state.as_deref());
        let subject = editing_subject(detail, flags.can_write);
        let can_edit = flags.can_write && can_edit_pull_request_comment(&subject, comment);
        let group = SharedString::from(format!("pr-comment-{}", comment.id.as_str()));
        let reaction_bar = ReactionBar::new(
            SharedString::from(format!("pr-comment-reactions-{}", comment.id.as_str())),
            self.ctx.clone(),
            comment.reactions.clone().unwrap_or_default(),
            detail.capabilities.reactions == Some(true) && flags.can_write,
            comment.id.as_str().to_owned(),
        )
        .justify_end();
        let identity = self.comment_identity(detail, comment, theme, cx);
        let location = Self::comment_location(comment, thread, theme);

        if Self::is_finished(detail, comment) {
            let open = self.expanded.contains(comment.id.as_str());
            let label = if thread.is_some_and(|thread| thread.is_resolved) {
                "Resolved"
            } else {
                "Review dismissed"
            };
            let weak = cx.entity().downgrade();
            let key = comment.id.as_str().to_owned();
            let preview = body
                .as_deref()
                .map(collapsed_preview)
                .filter(|preview| !preview.is_empty());
            let toggle = {
                let weak = weak.clone();
                let key = key.clone();
                Rc::new(
                    move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                        let key = key.clone();
                        weak.update(cx, |tab, cx| {
                            if !tab.expanded.remove(&key) {
                                tab.expanded.insert(key);
                            }
                            cx.notify();
                        })
                        .ok();
                    },
                )
            };
            let body_open = (open && !(body.is_none() && !can_edit)).then(|| {
                div()
                    .px_3()
                    .pb_3()
                    .child(div().mt_2().child(self.comment_body(
                        detail,
                        comment,
                        can_edit,
                        group.clone(),
                        cx,
                    )))
                    .into_any_element()
            });
            let header_toggle = toggle.clone();
            let preview_toggle = toggle.clone();
            return div()
                .id(group.clone())
                .group(group.clone())
                .rounded(RADIUS_LG)
                .border_1()
                .border_color(alpha(theme.border, 0.6))
                .child(
                    div()
                        .p_3()
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_start()
                                .gap_2()
                                .child(identity)
                                .child(
                                    div()
                                        .id(SharedString::from(format!("{group}-toggle")))
                                        .flex()
                                        .flex_shrink_0()
                                        .items_center()
                                        .gap(rems(0.375))
                                        .rounded(RADIUS_MD)
                                        .px_2()
                                        .py_1()
                                        .cursor(CursorStyle::PointingHand)
                                        .text_size(TEXT_XS.size)
                                        .text_color(theme.muted_foreground)
                                        .hover({
                                            let muted = theme.muted;
                                            let foreground = theme.foreground;
                                            move |style| style.bg(muted).text_color(foreground)
                                        })
                                        .child(label)
                                        .child(
                                            Icon::new(if open {
                                                IconName::ChevronUp
                                            } else {
                                                IconName::ChevronDown
                                            })
                                            .size(rems(0.875)),
                                        )
                                        .on_click(move |event, window, cx| {
                                            header_toggle(event, window, cx)
                                        }),
                                )
                                .child(reaction_bar),
                        )
                        .children(location)
                        .when(!open && preview.is_some(), |this| {
                            this.child(
                                div()
                                    .id(SharedString::from(format!("{group}-preview")))
                                    .mt_2()
                                    .w_full()
                                    .truncate()
                                    .cursor(CursorStyle::PointingHand)
                                    .text_size(TEXT_XS.size)
                                    .text_color(theme.muted_foreground)
                                    .hover({
                                        let foreground = theme.foreground;
                                        move |style| style.text_color(foreground)
                                    })
                                    .child(SharedString::from(preview.clone().unwrap_or_default()))
                                    .on_click(move |event, window, cx| {
                                        preview_toggle(event, window, cx)
                                    }),
                            )
                        }),
                )
                .children(body_open)
                .into_any_element();
        }

        let body_block = (!(body.is_none() && !can_edit)).then(|| {
            div()
                .px_3()
                .py_3()
                .child(self.comment_body(detail, comment, can_edit, group.clone(), cx))
                .into_any_element()
        });
        div()
            .id(group.clone())
            .group(group.clone())
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(alpha(theme.border, 0.6))
            .bg(theme.background)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .gap_2()
                    .rounded_t(RADIUS_LG)
                    .bg(alpha(theme.muted, 0.25))
                    .px_3()
                    .py(rems(0.625))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .min_w_0()
                            .flex_1()
                            .items_center()
                            .gap_2()
                            .text_size(TEXT_XS.size)
                            .text_color(theme.muted_foreground)
                            .child(identity)
                            .children(match (outcome, comment.review_state.as_deref()) {
                                (Some(outcome), _) => Some(
                                    PullRequestReviewOutcomeBadge::new(outcome).into_any_element(),
                                ),
                                (None, Some(state)) => {
                                    Some(div().child(review_state_label(state)).into_any_element())
                                }
                                _ => None,
                            }),
                    )
                    .child(reaction_bar),
            )
            .children(location.map(|location| div().px_3().child(location)))
            .children(body_block)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pull_request::fixtures;

    #[test]
    fn reads_a_review_state_as_a_sentence() {
        assert_eq!(review_state_label("CHANGES_REQUESTED"), "Changes requested");
        assert_eq!(review_state_label("COMMENTED"), "Commented");
        assert_eq!(review_state_label(""), "");
    }

    #[test]
    fn strips_markdown_from_a_collapsed_preview() {
        let body = "<!-- bot marker -->\n> [!NOTE]\n## Heading with **bold** and `code`\n- see [the docs](https://example.com)\n\n\nmore   text";
        assert_eq!(
            collapsed_preview(body),
            "Heading with bold and code see the docs more text"
        );
        assert_eq!(collapsed_preview("<!-- only a marker -->"), "");
    }

    #[test]
    fn formats_a_timestamp_like_to_locale_string() {
        let formatted = locale_string("2026-09-09T10:00:00.000Z");
        assert!(formatted.contains("2026"));
        assert!(formatted.ends_with("AM") || formatted.ends_with("PM"));
        assert_eq!(locale_string("not a date"), "not a date");
    }

    #[test]
    fn lists_requested_reviewers_then_those_who_ruled_unasked() {
        let detail = fixtures::detail_view();
        let entries = reviewer_entries(&detail);
        let logins: Vec<_> = entries
            .iter()
            .map(|entry| entry.key.to_lowercase())
            .collect();
        let mut unique = logins.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(logins.len(), unique.len(), "one face per person");
        for reviewer in &detail.reviewers {
            assert!(logins.contains(&reviewer.login.as_str().to_lowercase()));
        }
    }

    #[test]
    fn filters_bot_and_finished_comments_by_default() {
        let detail = fixtures::detail_view();
        let kept: Vec<_> = detail
            .comments
            .iter()
            .filter(|comment| !is_bot(comment) && !SummaryTab::is_finished(&detail, comment))
            .collect();
        assert!(kept.len() <= detail.comments.len());
    }
}
