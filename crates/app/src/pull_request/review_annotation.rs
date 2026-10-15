//! Port of upstream/apps/web/src/components/pullRequest/PullRequestReviewAnnotation.tsx and the
//! `draft` half of upstream/apps/web/src/components/diffs/DiffCommentAnnotation.tsx: what is
//! pinned under a line of the diff — conversations already on the host, comments queued for the
//! review being written, and the box a new comment is typed in.
//!
//! Not ported: the "Fix in a thread" hand-off button (the panel's `startFixFinding` has no seam in
//! [`super::detail_context::DetailNav`]), the "Add to agent" secondary action of the draft box
//! (the agent composer is another task), `contentEditable`/`onPointerDown` guards (no DOM).

use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, EventEmitter, FontWeight,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px, rems,
};
use turin_contracts::pull_request::{
    PullRequestCommentKind, PullRequestCommentUpdateKind, PullRequestDiffSide,
    PullRequestReviewPosition, PullRequestReviewThread, PullRequestThreadComment,
};

use super::comment_body::{EditButton, ReactionBar, pull_request_markdown};
use super::comment_form::{MarkdownEditor, MarkdownEditorEvent};
use super::detail_context::DetailContext;
use super::presentation_view::PullRequestActorLabel;
use super::pull_request_detail_logic::merge_pull_request_thread_comments;
use super::pull_request_editing_logic::{EditingSubject, can_edit_pull_request_comment};
use super::pull_request_review_store::PendingReviewComment;
use super::relative_time::format_relative_time_label;
use super::view_model::Loadable;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_XL, TEXT_3XS, TEXT_SM, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

/// `getReviewPositionAnchor`: the line and side a position is drawn under.
pub fn review_position_anchor(position: &PullRequestReviewPosition) -> (i64, PullRequestDiffSide) {
    match position {
        PullRequestReviewPosition::Added { new_line } => (new_line.get() as i64, PullRequestDiffSide::Right),
        PullRequestReviewPosition::Deleted { old_line } => (old_line.get() as i64, PullRequestDiffSide::Left),
        PullRequestReviewPosition::Context {
            old_line,
            new_line,
            side,
        } => match side {
            PullRequestDiffSide::Left => (old_line.get() as i64, *side),
            PullRequestDiffSide::Right => (new_line.get() as i64, *side),
        },
    }
}

/// `CARD_CLASS`: `mx-3 my-2 rounded-xl border border-border/70 bg-background p-3 text-sm
/// shadow-sm`.
fn card(cx: &App) -> gpui_kit::Div {
    let theme = cx.app_theme();
    div()
        .mx_3()
        .my_2()
        .rounded(RADIUS_XL)
        .border_1()
        .border_color(with_alpha(theme.border, 0.7))
        .bg(theme.background)
        .p_3()
        .text_size(TEXT_SM.size)
        .line_height(TEXT_SM.line_height)
        .text_color(theme.foreground)
        .shadow(tokens::shadow_sm(0.05))
}

/// A comment waiting to be sent with the rest of the review.
#[derive(IntoElement)]
pub struct PendingReviewCommentCard {
    comment: PendingReviewComment,
    on_remove: std::rc::Rc<dyn Fn(&str, &mut Window, &mut App)>,
}

impl PendingReviewCommentCard {
    pub fn new(
        comment: PendingReviewComment,
        on_remove: impl Fn(&str, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            comment,
            on_remove: std::rc::Rc::new(on_remove),
        }
    }
}

impl gpui_kit::RenderOnce for PendingReviewCommentCard {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let id = self.comment.id.clone();
        let on_remove = self.on_remove;
        card(cx)
            // `border-dashed`
            .border_dashed()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(TEXT_XS.size)
                    .line_height(TEXT_XS.line_height)
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::MessageSquare).size(rems(0.875)).color(theme.muted_foreground))
                    .child("Pending \u{2014} sent when you submit the review")
                    .child(
                        div().ml_auto().child(
                            Button::new(SharedString::from(format!("discard-{}", self.comment.id)))
                                .size(ButtonSize::IconXs)
                                .variant(ButtonVariant::Ghost)
                                .icon(IconName::Trash)
                                .icon_size(rems(0.875))
                                .tooltip(Tooltip::new("Discard this comment"))
                                .on_click(move |_, window, cx| on_remove(&id, window, cx)),
                        ),
                    ),
            )
            .child(
                div()
                    .mt_2()
                    .whitespace_normal()
                    .line_height(relative_line(1.625))
                    .child(SharedString::from(self.comment.draft.body.clone())),
            )
    }
}

fn relative_line(value: f32) -> gpui_kit::DefiniteLength {
    gpui_kit::relative(value).into()
}

// ── Draft ───────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DraftCommentEvent {
    Cancel,
    Submit(String),
}

/// The `draft` kind of `DiffCommentAnnotation`: a small textarea under the line with
/// "Cancel" and "Add to review" (⌘/Ctrl+Enter sends, Escape abandons).
pub struct DraftCommentCard {
    id: ElementId,
    range_label: SharedString,
    input: Entity<TextareaState>,
    _subscription: Subscription,
}

impl EventEmitter<DraftCommentEvent> for DraftCommentCard {}

impl DraftCommentCard {
    pub fn new(
        id: impl Into<ElementId>,
        range_label: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .placeholder("Add a comment\u{2026}")
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if let InputEvent::PressEnter { secondary: true, .. } = event {
                this.submit(cx);
            }
        });
        Self {
            id: id.into(),
            range_label: range_label.into(),
            input,
            _subscription: subscription,
        }
    }

    fn text(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_owned()
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let text = self.text(cx);
        if !text.is_empty() {
            cx.emit(DraftCommentEvent::Submit(text));
        }
    }
}

impl Render for DraftCommentCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let empty = self.text(cx).is_empty();
        div()
            .id(SharedString::from(format!("{:?}-draft", self.id)))
            .px_3()
            .py_2()
            .text_color(theme.foreground)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(DraftCommentEvent::Cancel);
                    cx.stop_propagation();
                }
            }))
            .child(Textarea::new(&self.input).aria_label(format!("Comment on lines {}", self.range_label)))
            .child(
                div()
                    .mt(px(6.))
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .mr_auto()
                            .text_size(TEXT_3XS.size)
                            .line_height(TEXT_3XS.line_height)
                            .text_color(with_alpha(theme.muted_foreground, 0.7))
                            .child("\u{2318}/Ctrl Enter to send"),
                    )
                    .child(
                        Button::new(SharedString::from(format!("{:?}-draft-cancel", self.id)))
                            .size(ButtonSize::Xs)
                            .variant(ButtonVariant::GhostMuted)
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DraftCommentEvent::Cancel))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("{:?}-draft-submit", self.id)))
                            .size(ButtonSize::Xs)
                            .variant(ButtonVariant::Default)
                            .disabled(empty)
                            .label("Add to review")
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    ),
            )
    }
}

// ── Thread ──────────────────────────────────────────────────────────────────

/// What a thread card is told by the tab that owns it, refreshed whenever the snapshot moves.
#[derive(Clone, Debug)]
pub struct ThreadCardProps {
    pub thread: PullRequestReviewThread,
    pub can_reply: bool,
    pub can_resolve: bool,
    pub can_react: bool,
}

/// A conversation already on the host, with whatever this host lets the reader do to it.
pub struct ReviewThreadCard {
    ctx: DetailContext,
    props: ThreadCardProps,
    /// A resolved thread is finished work, so it opens collapsed and stays one line until asked.
    expanded: bool,
    replying: bool,
    reply: Entity<TextareaState>,
    editing: Option<EditSession>,
    /// The cursor a "Load more comments" press is waiting on.
    loading_cursor: Option<String>,
}

struct EditSession {
    comment_id: String,
    editor: Entity<MarkdownEditor>,
    _subscription: Subscription,
}

impl ReviewThreadCard {
    pub fn new(
        ctx: DetailContext,
        props: ThreadCardProps,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let reply = cx.new(|cx| TextareaState::new(window, cx).rows(2).placeholder("Reply"));
        cx.subscribe(&reply, |this, _, event: &InputEvent, cx| {
            if let InputEvent::PressEnter { secondary: true, .. } = event {
                this.send(cx);
            }
        })
        .detach();
        Self {
            expanded: !props.thread.is_resolved,
            ctx,
            props,
            replying: false,
            reply,
            editing: None,
            loading_cursor: None,
        }
    }

    /// Takes the snapshot's newest answer; only a change repaints.
    pub fn set_props(&mut self, props: ThreadCardProps, cx: &mut Context<Self>) {
        let cursor_moved = self.props.thread.next_comments_cursor != props.thread.next_comments_cursor;
        if cursor_moved {
            self.loading_cursor = None;
        }
        let changed = format!("{:?}", self.props) != format!("{props:?}");
        if changed {
            self.props = props;
            cx.notify();
        }
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        let body = self.reply.read(cx).value().trim().to_owned();
        if body.is_empty() {
            return;
        }
        if let Some(source) = self.ctx.source(cx) {
            source.reply_to_thread(&self.ctx.reference, self.props.thread.id.as_str(), body, cx);
        }
        self.replying = false;
        cx.notify();
    }

    fn toggle_resolved(&mut self, cx: &mut Context<Self>) {
        if let Some(source) = self.ctx.source(cx) {
            source.set_thread_resolved(
                &self.ctx.reference,
                self.props.thread.id.as_str(),
                !self.props.thread.is_resolved,
                cx,
            );
        }
    }

    fn load_more(&mut self, cursor: String, cx: &mut Context<Self>) {
        if self.loading_cursor.is_some() {
            return;
        }
        if let Some(source) = self.ctx.source(cx) {
            source.load_thread_comments(
                &self.ctx.reference,
                self.props.thread.id.as_str(),
                &cursor,
                cx,
            );
            self.loading_cursor = Some(cursor);
            cx.notify();
        }
    }

    fn begin_edit(&mut self, comment: &PullRequestThreadComment, window: &mut Window, cx: &mut Context<Self>) {
        let id = comment.id.as_str().to_owned();
        let body = comment.body.clone();
        let editor = cx.new(|cx| {
            MarkdownEditor::new(
                SharedString::from(format!("thread-edit-{id}")),
                &body,
                None,
                window,
                cx,
            )
        });
        let subscription = cx.subscribe(&editor, |this, _, event: &MarkdownEditorEvent, cx| {
            match event {
                MarkdownEditorEvent::Save(body) => {
                    if let Some(edit) = this.editing.take() {
                        if let Some(source) = this.ctx.source(cx) {
                            source.update_comment(
                                &this.ctx.reference,
                                &edit.comment_id,
                                PullRequestCommentUpdateKind::ReviewComment,
                                body.clone(),
                                cx,
                            );
                        }
                    }
                }
                MarkdownEditorEvent::Cancel => this.editing = None,
            }
            cx.notify();
        });
        self.editing = Some(EditSession {
            comment_id: id,
            editor,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Whether this reader wrote this remark, which is what rewriting one takes. A conversation
    /// on a line is made of review comments, whatever the host filed them as.
    fn can_edit(&self, comment: &PullRequestThreadComment, cx: &App) -> bool {
        let Some(snapshot) = self.ctx.snapshot(cx) else {
            return false;
        };
        let Loadable::Ready(detail) = &snapshot.detail else {
            return false;
        };
        let subject = EditingSubject {
            author: detail.author.as_ref(),
            capabilities: &detail.capabilities,
            viewer: detail.viewer.as_ref().map(|viewer| viewer.as_str()),
            viewer_permissions: &detail.viewer_permissions,
        };
        let probe = turin_contracts::pull_request::PullRequestComment {
            id: comment.id.clone(),
            kind: PullRequestCommentKind::ReviewComment,
            author: comment.author.clone(),
            body: String::new(),
            created_at: comment.created_at.clone(),
            edited_at: None,
            url: None,
            path: None,
            review_state: None,
            reactions: None,
            can_edit: comment.can_edit,
        };
        can_edit_pull_request_comment(&subject, &probe)
    }
}

impl Render for ReviewThreadCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let thread = self.props.thread.clone();
        let comments = merge_pull_request_thread_comments(&thread.comments, &[]);
        let comment_count = thread
            .comment_count
            .map_or(comments.len() as u64, |count| count.max(comments.len() as u64));
        let thread_id = thread.id.as_str().to_owned();
        let expanded = self.expanded;
        let resolved = thread.is_resolved;

        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .text_size(TEXT_XS.size)
            .line_height(TEXT_XS.line_height)
            .text_color(theme.muted_foreground)
            .child(
                Icon::new(if resolved { IconName::CircleCheck } else { IconName::Circle })
                    .size(rems(0.875))
                    .color(if resolved { theme.success } else { theme.muted_foreground }),
            )
            .child(
                div()
                    .id(SharedString::from(format!("thread-toggle-{thread_id}")))
                    .cursor_pointer()
                    .hover(|style| style.text_color(theme.foreground))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.expanded = !this.expanded;
                        cx.notify();
                    }))
                    .child(SharedString::from(format!(
                        "{} \u{b7} {} {}",
                        if resolved { "Resolved" } else { "Open" },
                        comment_count,
                        if comment_count == 1 { "comment" } else { "comments" }
                    ))),
            )
            .when(thread.is_outdated, |this| this.child("outdated"))
            .when(self.props.can_resolve, |this| {
                this.child(
                    div().ml_auto().child(
                        Button::new(SharedString::from(format!("thread-resolve-{thread_id}")))
                            .size(ButtonSize::Xs)
                            .variant(ButtonVariant::Ghost)
                            .label(if resolved { "Unresolve" } else { "Resolve" })
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_resolved(cx))),
                    ),
                )
            });

        let mut body = div();
        if expanded {
            let mut list = div().mt_2().flex().flex_col().gap_3();
            for comment in &comments {
                let group: SharedString = format!("thread-comment-{}", comment.id).into();
                let editing = self
                    .editing
                    .as_ref()
                    .filter(|edit| edit.comment_id == comment.id.as_str())
                    .map(|edit| edit.editor.clone());
                let can_edit = editing.is_none() && self.can_edit(comment, cx);
                let comment_for_edit = comment.clone();
                let entity = cx.entity().downgrade();
                list = list.child(
                    div()
                        .id(SharedString::from(group.to_string()))
                        .group(group.clone())
                        .min_w_0()
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .text_size(TEXT_XS.size)
                                .line_height(TEXT_XS.line_height)
                                .text_color(theme.muted_foreground)
                                .child(PullRequestActorLabel::new(comment.author.clone()))
                                .child(SharedString::from(format_relative_time_label(&comment.created_at)))
                                .child(
                                    ReactionBar::new(
                                        SharedString::from(format!("thread-reactions-{}", comment.id)),
                                        self.ctx.clone(),
                                        comment.reactions.clone().unwrap_or_default(),
                                        self.props.can_react,
                                        comment.id.as_str(),
                                    )
                                    .justify_end(),
                                ),
                        )
                        .child(match editing {
                            Some(editor) => div().mt_1().child(editor).into_any_element(),
                            None => div()
                                .mt_1()
                                .flex()
                                .items_start()
                                .gap_1()
                                .child(
                                    div().min_w_0().flex_1().child(pull_request_markdown(
                                        SharedString::from(format!("thread-md-{}", comment.id)),
                                        SharedString::from(comment.body.clone()),
                                    )),
                                )
                                .when(can_edit, |this| {
                                    this.child(
                                        EditButton::new(
                                            SharedString::from(format!("edit-{}", comment.id)),
                                            group.clone(),
                                            "Edit comment",
                                        )
                                        .on_click(move |window, cx| {
                                            let comment = comment_for_edit.clone();
                                            entity
                                                .update(cx, |this, cx| {
                                                    this.begin_edit(&comment, window, cx)
                                                })
                                                .ok();
                                        }),
                                    )
                                })
                                .into_any_element(),
                        }),
                );
            }
            body = body.child(list);
            if let Some(cursor) = thread.next_comments_cursor.as_ref().map(|c| c.as_str().to_owned()) {
                let loading = self.loading_cursor.is_some();
                body = body.child(
                    div().mt_2().flex().child(
                        Button::new(SharedString::from(format!("thread-more-{thread_id}")))
                            .size(ButtonSize::Xs)
                            .variant(ButtonVariant::Ghost)
                            .disabled(loading)
                            .label(if loading { "Loading..." } else { "Load more comments" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.load_more(cursor.clone(), cx)
                            })),
                    ),
                );
            }
            if self.props.can_reply {
                body = body.child(if self.replying {
                    div()
                        .mt_2()
                        .child(Textarea::new(&self.reply).aria_label("Reply to this conversation"))
                        .child(
                            div()
                                .mt_2()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new(SharedString::from(format!("reply-cancel-{thread_id}")))
                                        .size(ButtonSize::Xs)
                                        .variant(ButtonVariant::Ghost)
                                        .label("Cancel")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.replying = false;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("reply-send-{thread_id}")))
                                        .size(ButtonSize::Xs)
                                        .variant(ButtonVariant::Default)
                                        .label("Reply")
                                        .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
                                ),
                        )
                        .into_any_element()
                } else {
                    div()
                        .mt_2()
                        .flex()
                        .child(
                            Button::new(SharedString::from(format!("reply-{thread_id}")))
                                .size(ButtonSize::Xs)
                                .variant(ButtonVariant::Ghost)
                                .label("Reply")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.replying = true;
                                    this.reply.update(cx, |input, cx| input.focus(window, cx));
                                    cx.notify();
                                })),
                        )
                        .into_any_element()
                });
            }
        }
        card(cx)
            .font_weight(FontWeight::NORMAL)
            .child(header)
            .child(body)
    }
}

/// Used by tabs that keep cards alive between frames.
pub fn any(element: impl IntoElement) -> AnyElement {
    element.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use turin_contracts::PositiveInt;

    fn line(value: u64) -> PositiveInt {
        PositiveInt::new(value).unwrap()
    }

    #[test]
    fn positions_name_the_line_and_side_they_hang_under() {
        assert_eq!(
            review_position_anchor(&PullRequestReviewPosition::Added { new_line: line(7) }),
            (7, PullRequestDiffSide::Right)
        );
        assert_eq!(
            review_position_anchor(&PullRequestReviewPosition::Deleted { old_line: line(3) }),
            (3, PullRequestDiffSide::Left)
        );
        let context = |side| PullRequestReviewPosition::Context {
            old_line: line(4),
            new_line: line(9),
            side,
        };
        assert_eq!(
            review_position_anchor(&context(PullRequestDiffSide::Left)),
            (4, PullRequestDiffSide::Left)
        );
        assert_eq!(
            review_position_anchor(&context(PullRequestDiffSide::Right)),
            (9, PullRequestDiffSide::Right)
        );
    }
}
