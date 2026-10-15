//! Port of upstream/apps/web/src/components/pullRequest/PullRequestTimelineTab.tsx, together with
//! the Timeline loading ghost (`PullRequestGhosts.tsx`) and `PullRequestActivityUnavailableState`.
//!
//! One rail with a marker per entry: commits, lifecycle updates and review verdicts are rows of
//! their own; consecutive remarks fold into a collapsed conversation. The reading order
//! (`newest` / `oldest`) is the panel header's toggle, set with [`TimelineTab::set_order`].
//!
//! Not ported: `content-visibility: auto` row skipping (rows are plain divs), the focus ring of
//! the commit row, the `scrollerRef` handoff to the summary scroll logic and the
//! `reportFailure: false` toast when a comment edit fails (commands return immediately).

use std::collections::HashSet;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Div, ElementId, Entity, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px, relative, rems,
};
use turin_contracts::pull_request::{
    PullRequestActor, PullRequestComment, PullRequestCommentKind, PullRequestCommentUpdateKind,
};

use super::comment_body::{EditButton, ReactionBar};
use super::comment_form::{MarkdownEditor, MarkdownEditorEvent};
use super::detail_context::DetailContext;
use super::presentation_view::{
    PullRequestActorAvatar, PullRequestDiffStat, pull_request_meta_line, with_tooltip,
};
use super::pull_request_detail_logic::{
    PullRequestCommentOrder, PullRequestReviewOutcome, PullRequestTimelineEvent,
    PullRequestTimelineEventKind, PullRequestTimelineRow, PullRequestTimelineSource,
    build_pull_request_timeline, group_pull_request_timeline_conversations,
    is_pull_request_verdict_stale, newest_pull_request_commit_at,
    pull_request_review_outcome,
};
use super::pull_request_editing_logic::{EditingSubject, can_edit_pull_request_comment};
use super::pull_request_presentation::{
    pull_request_review_outcome_label, pull_request_review_outcome_stale_label,
    pull_request_review_outcome_tone_class_name,
};
use super::presentation_view::review_outcome_icon;
use super::relative_time::format_relative_time_label;
use super::view_model::Loadable;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{TEXT_2XS, TEXT_3XS, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::skeleton::{SkeletonShape, skeleton, skeleton_group};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, scroll_area};

/// `size-8` marker box on the rail.
const MARKER: f32 = 32.;

pub struct TimelineTab {
    pub ctx: DetailContext,
    order: PullRequestCommentOrder,
    /// Conversation groups the reader opened, by the id of their first entry.
    open_groups: HashSet<String>,
    edit: Option<EditSession>,
    _subscriptions: Vec<Subscription>,
}

/// The remark being rewritten: one editor at a time, keyed by the host's comment id.
struct EditSession {
    comment_id: String,
    kind: PullRequestCommentUpdateKind,
    editor: Entity<MarkdownEditor>,
    _subscription: Subscription,
}

impl TimelineTab {
    pub fn new(ctx: DetailContext, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&ctx.store, |_, _, cx| cx.notify());
        Self {
            ctx,
            order: PullRequestCommentOrder::Newest,
            // `TURIN_TIMELINE_OPEN=<first entry id>` (dev/screenshots) opens a conversation.
            open_groups: std::env::var("TURIN_TIMELINE_OPEN")
                .map(|ids| ids.split(',').map(str::to_owned).collect())
                .unwrap_or_default(),
            edit: None,
            _subscriptions: vec![subscription],
        }
    }

    pub fn order(&self) -> PullRequestCommentOrder {
        self.order
    }

    /// The header's newest / oldest toggle.
    pub fn set_order(&mut self, order: PullRequestCommentOrder, cx: &mut Context<Self>) {
        if self.order != order {
            self.order = order;
            cx.notify();
        }
    }

    fn toggle_group(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.open_groups.remove(key) {
            self.open_groups.insert(key.to_owned());
        }
        cx.notify();
    }

    fn begin_edit(
        &mut self,
        comment: &PullRequestComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let kind = match comment.kind {
            PullRequestCommentKind::IssueComment => PullRequestCommentUpdateKind::IssueComment,
            PullRequestCommentKind::ReviewComment => PullRequestCommentUpdateKind::ReviewComment,
            // A review's own summary is not a kind any host rewrites.
            PullRequestCommentKind::Review => return,
        };
        let id = comment.id.as_str().to_owned();
        let body = comment.body.clone();
        let can_write = self.ctx.snapshot(cx).is_some_and(|snapshot| snapshot.can_write);
        let editor = cx.new(|cx| {
            let mut editor = MarkdownEditor::new(
                SharedString::from(format!("timeline-edit-{id}")),
                &body,
                None,
                window,
                cx,
            );
            editor.set_can_write(can_write, cx);
            editor
        });
        let comment_id = id.clone();
        let subscription = cx.subscribe(&editor, move |this, _, event: &MarkdownEditorEvent, cx| {
            match event {
                MarkdownEditorEvent::Save(body) => this.save_edit(body.clone(), cx),
                MarkdownEditorEvent::Cancel => {
                    if this.edit.as_ref().is_some_and(|edit| edit.comment_id == comment_id) {
                        this.edit = None;
                        cx.notify();
                    }
                }
            }
        });
        self.edit = Some(EditSession {
            comment_id: id,
            kind,
            editor,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn save_edit(&mut self, body: String, cx: &mut Context<Self>) {
        let Some(edit) = self.edit.take() else {
            return;
        };
        if let Some(source) = self.ctx.source(cx) {
            source.update_comment(&self.ctx.reference, &edit.comment_id, edit.kind, body, cx);
        }
        cx.notify();
    }

    fn open_on_host(&self, url: &str, cx: &mut App) {
        if let Some(source) = self.ctx.source(cx) {
            source.open_external(url, cx);
        }
    }
}

// ── Pieces ──────────────────────────────────────────────────────────────────

fn text_size<T: gpui_kit::Styled>(element: T, size: crate::theme::tokens::TextSize) -> T {
    element.text_size(size.size).line_height(size.line_height)
}

/// `TimelineMarker`: `absolute left-0 top-1/2 z-10 flex size-8 -translate-y-1/2 items-center
/// justify-center bg-background`; `top` is the `top-6` override (24px, centred on the marker).
fn timeline_marker(top: Option<Pixels>, theme: &AppTheme, child: impl IntoElement) -> Div {
    let boxed = div()
        .flex()
        .flex_shrink_0()
        .size(px(MARKER))
        .items_center()
        .justify_center()
        .bg(theme.background)
        .child(child);
    match top {
        Some(top) => div().absolute().left_0().top(top - px(MARKER / 2.)).child(boxed),
        None => div()
            .absolute()
            .left_0()
            .top_0()
            .bottom_0()
            .flex()
            .items_center()
            .child(boxed),
    }
}

/// `IconMarker`: the glyph in a `size-7` muted box.
fn icon_marker(top: Option<Pixels>, icon: IconName, theme: &AppTheme) -> Div {
    timeline_marker(
        top,
        theme,
        div()
            .flex()
            .size(px(28.))
            .items_center()
            .justify_center()
            .bg(theme.background)
            .child(Icon::new(icon).size(rems(0.875)).color(theme.muted_foreground)),
    )
}

/// `ActorTimelineMarker`: the first actor's face, or the fallback glyph when nobody is named.
fn actor_marker(
    top: Option<Pixels>,
    actors: &[PullRequestActor],
    fallback: IconName,
    muted: bool,
    theme: &AppTheme,
) -> Div {
    match actors.first() {
        None => icon_marker(top, fallback, theme),
        Some(actor) => timeline_marker(
            top,
            theme,
            div()
                .flex()
                // `opacity-45 grayscale` for a folded conversation.
                .when(muted, |this| this.opacity(0.45))
                .child(PullRequestActorAvatar::new(Some(actor)).size(px(28.))),
        ),
    }
}

fn actor_name(actor: Option<&PullRequestActor>, theme: &AppTheme) -> Div {
    div()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.foreground)
        .child(SharedString::from(
            actor.map_or("ghost", |actor| actor.login.as_str()).to_owned(),
        ))
}

/// `friendlyReviewState`: `CHANGES_REQUESTED` → `Changes requested`.
fn friendly_review_state(value: &str) -> String {
    let words = value.to_lowercase().replace(['_', '-'], " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) if first.is_alphanumeric() || first == '_' => {
            first.to_uppercase().chain(chars).collect()
        }
        _ => words,
    }
}

/// `PullRequestMetaLine` segments for a remark: when, and which file.
fn remark_meta(event: &PullRequestTimelineEvent, theme: &AppTheme) -> Div {
    text_size(pull_request_meta_line(
        [
            Some(
                div()
                    .child(SharedString::from(format_relative_time_label(&event.at)))
                    .into_any_element(),
            ),
            event.path.as_ref().map(|path| {
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap_1()
                    .child(
                        Icon::new(IconName::FileCode)
                            .size(rems(0.75))
                            .color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(path.clone())),
                    )
                    .into_any_element()
            }),
        ],
        theme,
    ), TEXT_2XS)
    .flex_wrap()
    .text_color(theme.muted_foreground)
}

fn body_block(event_id: &str, event: &PullRequestTimelineEvent, theme: &AppTheme) -> Option<AnyElement> {
    let body = event.body.as_ref()?;
    // `TimelineBody`: `mt-3`.
    Some(
        div()
            .mt_3()
            .child(if event.markdown {
                super::comment_body::pull_request_markdown(
                    SharedString::from(format!("timeline-md-{event_id}")),
                    SharedString::from(body.clone()),
                )
                .into_any_element()
            } else {
                div()
                    .text_size(TEXT_XS.size)
                    .line_height(TEXT_XS.line_height)
                    .text_color(theme.muted_foreground)
                    .whitespace_normal()
                    .child(SharedString::from(body.clone()))
                    .into_any_element()
            })
            .into_any_element(),
    )
}

// ── Rendering ───────────────────────────────────────────────────────────────

impl TimelineTab {
    fn open_on_host_button(&self, id: String, url: Option<String>, cx: &mut Context<Self>) -> Option<AnyElement> {
        let url = url?;
        Some(
            div()
                .mr(px(-4.))
                .mt(px(-4.))
                .flex_shrink_0()
                .child(
                    Button::new(SharedString::from(format!("open-host-{id}")))
                        .size(ButtonSize::IconXs)
                        .variant(ButtonVariant::GhostMuted)
                        .icon(IconName::ExternalLink)
                        .icon_size(rems(0.75))
                        .tooltip(crate::ui::Tooltip::new("Open activity on host"))
                        .on_click(cx.listener(move |this, _, _, cx| this.open_on_host(&url, cx))),
                )
                .into_any_element(),
        )
    }

    fn reactions_for(
        &self,
        event: &PullRequestTimelineEvent,
        can_react: bool,
    ) -> Option<AnyElement> {
        if !can_react && event.reactions.is_empty() {
            return None;
        }
        Some(
            ReactionBar::new(
                SharedString::from(format!("timeline-reactions-{}", event.id)),
                self.ctx.clone(),
                event.reactions.clone(),
                can_react,
                event.id.clone(),
            )
            .justify_end()
            .into_any_element(),
        )
    }

    /// `ConversationCard`.
    fn conversation_card(
        &self,
        event: &PullRequestTimelineEvent,
        editable: Option<&PullRequestComment>,
        can_react: bool,
        theme: &AppTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let group: SharedString = format!("card-{}", event.id).into();
        let editing = self
            .edit
            .as_ref()
            .filter(|edit| edit.comment_id == event.id && editable.is_some());
        let header = div()
            .flex()
            .flex_wrap()
            .min_w_0()
            .items_start()
            .gap_2()
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(
                        text_size(
                            div()
                                .flex()
                                .flex_wrap()
                                .min_w_0()
                                .items_center()
                                .gap_x(rems(0.375))
                                .gap_y_1(),
                            TEXT_XS,
                        )
                        .child(actor_name(event.actor.as_ref(), theme))
                        .child(
                            div()
                                .text_color(theme.muted_foreground)
                                .child(SharedString::from(event.title.clone())),
                        )
                        .when_some(event.review_state.as_ref(), |this, state| {
                            this.child(
                                text_size(div(), TEXT_3XS)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.muted_foreground)
                                    .child(SharedString::from(friendly_review_state(state))),
                            )
                        }),
                    )
                    .child(div().mt_1().child(remark_meta(event, theme))),
            )
            .when_some(editable.filter(|_| editing.is_none()), |this, comment| {
                let comment = comment.clone();
                let entity = cx.entity().downgrade();
                this.child(
                    EditButton::new(
                        SharedString::from(format!("edit-{}", event.id)),
                        group.clone(),
                        "Edit comment",
                    )
                    .nudge_up()
                    .on_click(move |window, cx| {
                        let comment = comment.clone();
                        entity
                            .update(cx, |this, cx| this.begin_edit(&comment, window, cx))
                            .ok();
                    }),
                )
            })
            .children(self.reactions_for(event, can_react))
            .children(self.open_on_host_button(event.id.clone(), event.url.clone(), cx));

        let body = match (editing, &event.body) {
            (Some(edit), _) => Some(
                div()
                    .px_2()
                    .pb_2()
                    .pt_3()
                    .child(edit.editor.clone())
                    .into_any_element(),
            ),
            (None, Some(_)) => body_block(&event.id, event, theme)
                .map(|body| div().px_2().pb_2().child(body).into_any_element()),
            (None, None) => None,
        };
        div()
            .id(SharedString::from(group.to_string()))
            .group(group)
            .py_2()
            .child(div().px_2().child(header))
            .children(body)
            .into_any_element()
    }

    /// `ConversationGroup`.
    #[allow(clippy::too_many_arguments)]
    fn conversation_group(
        &self,
        events: &[PullRequestTimelineEvent],
        editable: &std::collections::HashMap<&str, &PullRequestComment>,
        can_react: bool,
        theme: &AppTheme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let first = events.first()?;
        let key = first.id.clone();
        let open = self.open_groups.contains(&key);
        let mut actors: Vec<PullRequestActor> = Vec::new();
        for event in events {
            if let Some(actor) = &event.actor
                && !actors.iter().any(|known| known.login == actor.login)
            {
                actors.push(actor.clone());
            }
        }
        let comments = format!(
            "{} {}",
            events.len(),
            if events.len() == 1 { "comment" } else { "comments" }
        );
        let authors = format!(
            "{} {} \u{b7} {}",
            actors.len(),
            if actors.len() == 1 { "author" } else { "authors" },
            format_relative_time_label(&first.at)
        );
        let trigger_id = SharedString::from(format!("conversation-{key}"));
        let trigger = text_size(
            div()
                .id(trigger_id)
                .flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap_3()
                .py_2()
                .cursor_pointer()
                .text_color(if open {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .opacity(if open { 1. } else { 0.55 })
                .hover(|style| style.opacity(1.))
                .on_click({
                    let key = key.clone();
                    cx.listener(move |this, _, _, cx| this.toggle_group(&key, cx))
                }),
            TEXT_XS,
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SharedString::from(comments)),
                )
                .child(
                    text_size(div().truncate().text_color(theme.muted_foreground), TEXT_3XS)
                        .child(SharedString::from(authors)),
                ),
        )
        .child(
            Icon::new(if open { IconName::ChevronUp } else { IconName::ChevronDown })
                .size(rems(0.875))
                .color(theme.muted_foreground),
        );
        let cards = open.then(|| {
            let mut list = div().mt_1().flex().flex_col().gap_1();
            for event in events {
                list = list.child(self.conversation_card(
                    event,
                    editable.get(event.id.as_str()).copied(),
                    can_react,
                    theme,
                    cx,
                ));
            }
            list
        });
        Some(
            div()
                .relative()
                .mb_5()
                .pl(rems(3.))
                .child(actor_marker(
                    Some(px(24.)),
                    &actors,
                    IconName::MessageSquare,
                    !open,
                    theme,
                ))
                .child(div().child(trigger).children(cards))
                .into_any_element(),
        )
    }

    /// `CommitEvent`.
    fn commit_event(
        &self,
        event: &PullRequestTimelineEvent,
        theme: &AppTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let group: SharedString = format!("commit-{}", event.id).into();
        let oid = event.id.clone();
        let open_commit = self.ctx.nav.open_commit.clone();
        let short: String = event.id.chars().take(7).collect();
        let hover = theme.primary;
        let title = event.body.clone().unwrap_or_else(|| "Untitled commit".to_owned());
        let _ = cx;
        div()
            .id(SharedString::from(group.to_string()))
            .group(group.clone())
            .relative()
            .mb_5()
            .block()
            .w_full()
            .rounded(px(6.))
            .pl(rems(3.))
            .cursor_pointer()
            .on_click(move |_, _, cx| open_commit(&oid, cx))
            .child(actor_marker(None, &event.commit_authors, IconName::GitCommitHorizontal, false, theme))
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(rems(0.625))
                    .py(rems(0.375))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(
                                text_size(
                                    div()
                                        .truncate()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.foreground)
                                        .group_hover(group, move |style| style.text_color(hover)),
                                    TEXT_XS,
                                )
                                .child(SharedString::from(title)),
                            )
                            .child(
                                text_size(
                                    div()
                                        .mt_1()
                                        .flex()
                                        .flex_wrap()
                                        .items_center()
                                        .gap_x_2()
                                        .gap_y(rems(0.125))
                                        .text_color(theme.muted_foreground),
                                    TEXT_3XS,
                                )
                                .child(
                                    div()
                                        .font_family(theme.fonts.mono.clone())
                                        .child(SharedString::from(short)),
                                )
                                .child(SharedString::from(format_relative_time_label(&event.at))),
                            ),
                    )
                    .when_some(
                        event.additions.zip(event.deletions),
                        |this, (additions, deletions)| {
                            this.child(
                                text_size(
                                    div().ml_auto().flex_shrink_0(),
                                    TEXT_3XS,
                                )
                                .child(PullRequestDiffStat::new(additions, deletions).mono()),
                            )
                        },
                    ),
            )
            .into_any_element()
    }

    /// `LifecycleEvent`.
    fn lifecycle_event(&self, event: &PullRequestTimelineEvent, theme: &AppTheme) -> AnyElement {
        let (icon, label) = match event.kind {
            PullRequestTimelineEventKind::Opened => {
                (IconName::GitPullRequestArrow, "Pull request opened")
            }
            PullRequestTimelineEventKind::Merged => (IconName::GitMerge, "Pull request merged"),
            _ => (IconName::GitPullRequestClosed, "Pull request closed"),
        };
        div()
            .relative()
            .mb_5()
            .pl(rems(3.))
            .child(icon_marker(None, icon, theme))
            .child(
                text_size(div().py(rems(0.375)), TEXT_XS)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(rems(0.375))
                            .when(event.actor.is_some(), |this| {
                                this.child(actor_name(event.actor.as_ref(), theme))
                            })
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(label),
                            ),
                    )
                    .child(
                        text_size(
                            div().mt(rems(0.125)).text_color(theme.muted_foreground),
                            TEXT_2XS,
                        )
                        .child(SharedString::from(format_relative_time_label(&event.at))),
                    ),
            )
            .into_any_element()
    }

    /// `ReviewVerdictEvent`: a verdict as its own row, wearing the reviewer's face on the rail
    /// and the verdict's word beside their name.
    fn verdict_event(
        &self,
        event: &PullRequestTimelineEvent,
        outcome: PullRequestReviewOutcome,
        stale: bool,
        can_react: bool,
        theme: &AppTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let actors: Vec<PullRequestActor> = event.actor.iter().cloned().collect();
        let (_, tone) = review_outcome_icon(outcome, theme);
        let fallback = review_outcome_icon(outcome, theme).0;
        let word = pull_request_review_outcome_label(outcome).to_lowercase();
        let word_color = if stale {
            with_alpha(theme.muted_foreground, 0.7).into()
        } else {
            tone
        };
        let verdict = div()
            .font_weight(FontWeight::MEDIUM)
            .text_color(word_color)
            .child(SharedString::from(word));
        let verdict = if stale {
            with_tooltip(
                SharedString::from(format!("verdict-{}", event.id)),
                pull_request_review_outcome_stale_label(outcome),
                verdict,
            )
            .into_any_element()
        } else {
            verdict.into_any_element()
        };
        let _ = pull_request_review_outcome_tone_class_name;
        div()
            .relative()
            .mb_5()
            .pl(rems(3.))
            .child(actor_marker(Some(px(24.)), &actors, fallback, false, theme))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .min_w_0()
                    .items_start()
                    .gap_2()
                    .py(rems(0.375))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(
                                text_size(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .min_w_0()
                                        .items_center()
                                        .gap_x(rems(0.375))
                                        .gap_y_1(),
                                    TEXT_XS,
                                )
                                .child(actor_name(event.actor.as_ref(), theme))
                                .child(verdict),
                            )
                            .child(div().mt(rems(0.125)).child(remark_meta(event, theme))),
                    )
                    .children(self.reactions_for(event, can_react))
                    .children(self.open_on_host_button(event.id.clone(), event.url.clone(), cx)),
            )
            .children(body_block(&event.id, event, theme))
            .into_any_element()
    }

    fn ghost(&self, cx: &App) -> AnyElement {
        timeline_ghost(6, cx)
    }

    fn activity_unavailable(&self, error: &str, theme: &AppTheme, cx: &mut Context<Self>) -> AnyElement {
        let ctx = self.ctx.clone();
        div()
            .flex()
            .flex_col()
            .min_h(rems(12.))
            .items_center()
            .justify_center()
            .gap_2()
            .px_4()
            .py(rems(2.5))
            .text_center()
            .child(
                div()
                    .text_size(TEXT_SM.size)
                    .line_height(TEXT_SM.line_height)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child("Could not load pull request activity"),
            )
            .child(
                text_size(div().max_w(rems(28.)).text_color(theme.muted_foreground), TEXT_XS)
                    .child(SharedString::from(error.to_owned())),
            )
            .child(
                Button::new("timeline-retry")
                    .size(ButtonSize::Sm)
                    .variant(ButtonVariant::Outline)
                    .icon(IconName::RefreshCw)
                    .label("Retry")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        if let Some(source) = ctx.source(cx) {
                            source.refresh_detail(&ctx.reference, cx);
                        }
                    })),
            )
            .into_any_element()
    }
}

/// `PullRequestTimelineGhost`: bars in the geometry of the entries they stand for.
pub fn timeline_ghost(rows: usize, cx: &App) -> AnyElement {
    const TITLE_WIDTHS: [f32; 7] = [0.6, 0.4, 0.5, 2. / 3., 0.4, 0.6, 0.5];
    let theme = cx.app_theme();
    let mut rail = div()
        .relative()
        .ml_2()
        .border_l_1()
        .border_color(with_alpha(theme.border, 0.7))
        .pl(rems(1.25));
    for index in 0..rows {
        rail = rail.child(
            div()
                .relative()
                .pb_5()
                .child(
                    skeleton(SkeletonShape::Pill, cx)
                        .absolute()
                        .left(rems(-1.55))
                        .top_1()
                        .size_2(),
                )
                .child(
                    skeleton(SkeletonShape::Block, cx)
                        .h(rems(0.875))
                        .w(relative(TITLE_WIDTHS[index % TITLE_WIDTHS.len()])),
                )
                .child(skeleton(SkeletonShape::Block, cx).h_3().mt(rems(0.375)).w(rems(4.))),
        );
    }
    skeleton_group(
        ElementId::Name("timeline-ghost".into()),
        div().px_4().py(rems(1.25)).child(rail),
    )
}

impl Render for TimelineTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let snapshot = self.ctx.snapshot(cx).cloned();
        let content = match snapshot {
            None => self.ghost(cx),
            Some(snapshot) => match &snapshot.detail {
                Loadable::Pending => self.ghost(cx),
                Loadable::Failed(error) => self.activity_unavailable(error, &theme, cx),
                Loadable::Ready(_) if snapshot.activity_pending => self.ghost(cx),
                Loadable::Ready(detail) => {
                    let source = PullRequestTimelineSource {
                        created_at: detail.created_at.clone(),
                        author: detail.author.clone(),
                        commits: detail.commits.clone(),
                        comments: detail.comments.clone(),
                        merged_at: detail.merged_at.clone(),
                        closed_at: detail.closed_at.clone(),
                    };
                    let events = build_pull_request_timeline(&source);
                    let newest_commit_at = newest_pull_request_commit_at(&detail.commits);
                    let can_react = detail.capabilities.reactions == Some(true);
                    // A timeline entry keeps only what it draws, so the remarks this reader may
                    // rewrite are looked up here by the id the entry carries.
                    let subject = EditingSubject {
                        author: detail.author.as_ref(),
                        capabilities: &detail.capabilities,
                        viewer: detail.viewer.as_ref().map(|viewer| viewer.as_str()),
                        viewer_permissions: &detail.viewer_permissions,
                    };
                    let editable: std::collections::HashMap<&str, &PullRequestComment> = detail
                        .comments
                        .iter()
                        .filter(|comment| can_edit_pull_request_comment(&subject, comment))
                        .map(|comment| (comment.id.as_str(), comment))
                        .collect();
                    // `build_pull_request_timeline` answers newest first; "oldest" reads it backwards.
                    let ordered: Vec<PullRequestTimelineEvent> = match self.order {
                        PullRequestCommentOrder::Newest => events.clone(),
                        PullRequestCommentOrder::Oldest => events.iter().rev().cloned().collect(),
                    };
                    let rows = group_pull_request_timeline_conversations(&ordered);
                    let mut rail = div().relative().child(
                        div()
                            .absolute()
                            .bottom(px(20.))
                            .left(px(15.))
                            .top(px(4.))
                            .w(px(1.))
                            .bg(with_alpha(theme.border, 0.45)),
                    );
                    for row in &rows {
                        let element = match row {
                            PullRequestTimelineRow::Comments(events) => {
                                self.conversation_group(events, &editable, can_react, &theme, cx)
                            }
                            PullRequestTimelineRow::Event(event) => {
                                if event.kind == PullRequestTimelineEventKind::Commit {
                                    Some(self.commit_event(event, &theme, cx))
                                } else if let Some(outcome) =
                                    pull_request_review_outcome(event.review_state.as_deref())
                                {
                                    let stale = is_pull_request_verdict_stale(
                                        &event.at,
                                        newest_commit_at.as_deref(),
                                    );
                                    Some(self.verdict_event(
                                        event, outcome, stale, can_react, &theme, cx,
                                    ))
                                } else {
                                    Some(self.lifecycle_event(event, &theme))
                                }
                            }
                        };
                        rail = rail.children(element);
                    }
                    div()
                        .mx_auto()
                        .w_full()
                        .max_w(rems(48.))
                        .child(rail)
                        .when(events.is_empty(), |this| {
                            this.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .justify_center()
                                    .py(rems(4.))
                                    .text_center()
                                    .text_color(theme.muted_foreground)
                                    .child(
                                        div().mb_2().child(
                                            Icon::new(IconName::GitPullRequestArrow)
                                                .size(rems(1.25))
                                                .color(theme.muted_foreground),
                                        ),
                                    )
                                    .child(
                                        text_size(div(), TEXT_XS).child("No activity yet."),
                                    ),
                            )
                        })
                        .into_any_element()
                }
            },
        };
        div()
            .size_full()
            .min_h_0()
            .child(scroll_area("timeline-scroll").child(div().px_4().py(rems(1.25)).child(content)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn review_states_read_as_words() {
        assert_eq!(friendly_review_state("CHANGES_REQUESTED"), "Changes requested");
        assert_eq!(friendly_review_state("commented"), "Commented");
        assert_eq!(friendly_review_state("changes-requested"), "Changes requested");
        assert_eq!(friendly_review_state(""), "");
    }
}

/// S2 DEV HOOK: one tab bare, for screenshots while the detail panel shell is in flight.
pub fn mount_isolated(
    tab: super::detail_context::DetailTab,
    ctx: DetailContext,
    window: &mut Window,
    cx: &mut App,
) -> Option<gpui_kit::AnyView> {
    use super::detail_context::DetailTab;
    if let Some(source) = ctx.source(cx) {
        source.open_detail(&ctx.reference, cx);
    }
    let bg = cx.app_theme().background;
    let view: gpui_kit::AnyView = match tab {
        DetailTab::Timeline => cx.new(|cx| TimelineTab::new(ctx, window, cx)).into(),
        DetailTab::Code => cx.new(|cx| super::code_tab::CodeTab::new(ctx, window, cx)).into(),
        DetailTab::Guide => cx.new(|cx| super::guide_tab::GuideTab::new(ctx, window, cx)).into(),
        _ => return None,
    };
    Some(cx.new(|_| Frame { view, bg }).into())
}

struct Frame {
    view: gpui_kit::AnyView,
    bg: gpui_kit::Hsla,
}

impl Render for Frame {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(self.bg).child(self.view.clone())
    }
}
