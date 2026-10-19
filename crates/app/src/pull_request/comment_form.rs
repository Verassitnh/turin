//! Port of upstream/apps/web/src/components/pullRequest/PullRequestMarkdownEditor.tsx,
//! PullRequestCommentForm.tsx and PullRequestComposer.tsx: the box a body is rewritten in, the
//! comment half of the floating composer, and the composer itself.
//!
//! Not ported: the TipTap rich editor of the full upstream editor is out of scope (the upstream
//! file is a plain `Textarea` with a Write / Preview switch, which is what [`MarkdownEditor`]
//! is); the auth write scope check (one process: the snapshot's `can_write` stands in for
//! `AuthSourceControlWriteScope`); toasts for failed posts (commands return immediately and the
//! source reports failures through the snapshot's `action_error`).
//!
//! Port of upstream/apps/web/src/components/pullRequest/PullRequestComposer.tsx.
//! Port of upstream/apps/web/src/components/pullRequest/PullRequestCommentForm.tsx.

use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, App, AppContext as _, Context, Div, ElementId, Entity, EventEmitter,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement, Render, SharedString,
    Styled, Subscription, Window, div, px, rems,
};
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestReviewVerdict, PullRequestState,
};

use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, TEXT_3XS, TEXT_XS};
use crate::ui::{
    Button, ButtonSize, ButtonVariant, IconName, PopoverOptions, PopoverWidth, Toggle, ToggleSize,
    ToggleVariant, Tooltip, popover, popover_title,
};

use super::comment_body::pull_request_markdown;
use super::detail_context::DetailContext;
use super::pull_request_review_store::{PullRequestReviewStore, pull_request_review_key};
use super::review_form::{ReviewForm, ReviewFormEvent, review_store};
use super::view_model::Loadable;

/// `ToggleGroup variant="segmented"`: `flex w-fit gap-0.5 rounded-lg bg-input/40 p-0.5`.
pub fn segmented_group(cx: &App) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .gap(px(2.))
        .p(px(2.))
        .rounded(RADIUS_LG)
        .bg(with_alpha(cx.app_theme().input, 0.4))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkdownEditorEvent {
    Save(String),
    Cancel,
}

/// `PullRequestMarkdownEditor`: the box a body is rewritten in. It owns the draft and nothing
/// else: the caller sends the request and says whether it is still in flight ([`set_saving`]),
/// so the same box serves every mutation without knowing which one it is.
///
/// Preview renders through the same component the saved body will be read through, which is the
/// only way to see what a host's markdown will actually become before it is sent.
///
/// [`set_saving`]: MarkdownEditor::set_saving
pub struct MarkdownEditor {
    id: ElementId,
    input: Entity<TextareaState>,
    preview: bool,
    saving: bool,
    /// A description may be cleared, which is how one is removed; a remark may not be emptied.
    allow_empty: bool,
    can_write: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<MarkdownEditorEvent> for MarkdownEditor {}

impl MarkdownEditor {
    pub fn new(
        id: impl Into<ElementId>,
        value: &str,
        placeholder: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let value = value.to_owned();
        let placeholder = placeholder.map(str::to_owned);
        let input = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx).rows(6).default_value(value);
            if let Some(placeholder) = placeholder {
                state = state.placeholder(placeholder);
            }
            state
        });
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if let InputEvent::PressEnter {
                secondary: true, ..
            } = event
            {
                this.save(cx);
            }
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        Self {
            id: id.into(),
            input,
            preview: false,
            saving: false,
            allow_empty: false,
            can_write: true,
            _subscriptions: vec![subscription],
        }
    }

    pub fn allow_empty(mut self, allow: bool) -> Self {
        self.allow_empty = allow;
        self
    }

    pub fn set_saving(&mut self, saving: bool, cx: &mut Context<Self>) {
        self.saving = saving;
        cx.notify();
    }

    pub fn set_can_write(&mut self, can_write: bool, cx: &mut Context<Self>) {
        self.can_write = can_write;
        cx.notify();
    }

    pub fn draft(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn save_disabled(&self, cx: &App) -> bool {
        let empty = self.draft(cx).trim().is_empty();
        !self.can_write || self.saving || (empty && !self.allow_empty)
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if !self.save_disabled(cx) {
            cx.emit(MarkdownEditorEvent::Save(self.draft(cx)));
        }
    }
}

impl Render for MarkdownEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let draft = self.draft(cx);
        let empty = draft.trim().is_empty();
        let save_disabled = self.save_disabled(cx);
        let saving = self.saving;
        let preview = self.preview;
        let id = self.id.clone();
        let toggle = |label: &'static str, pressed: bool, to_preview: bool| {
            Toggle::new(SharedString::from(format!("{id:?}-mode-{label}")))
                .variant(ToggleVariant::Segmented)
                .size(ToggleSize::Segmented)
                .pressed(pressed)
                .disabled(saving)
                .label(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.preview = to_preview;
                    cx.notify();
                }))
        };
        div()
            .id(SharedString::from(format!("{:?}-editor", self.id)))
            .flex()
            .flex_col()
            .gap_2()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && !this.saving {
                    cx.emit(MarkdownEditorEvent::Cancel);
                    cx.stop_propagation();
                }
            }))
            .child(
                segmented_group(cx)
                    .child(toggle("Write", !preview, false))
                    .child(toggle("Preview", preview, true)),
            )
            .child(if preview {
                div()
                    .rounded(RADIUS_LG)
                    .border_1()
                    .border_color(with_alpha(theme.border, 0.6))
                    .px_3()
                    .py_2()
                    .child(if empty {
                        div()
                            .text_size(TEXT_XS.size)
                            .line_height(TEXT_XS.line_height)
                            .text_color(theme.muted_foreground)
                            .child("Nothing to preview.")
                            .into_any_element()
                    } else {
                        pull_request_markdown(
                            SharedString::from(format!("{:?}-preview", self.id)),
                            draft,
                        )
                        .into_any_element()
                    })
                    .into_any_element()
            } else {
                Textarea::new(&self.input)
                    .disabled(saving)
                    .into_any_element()
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!("{:?}-cancel", self.id)))
                            .size(ButtonSize::Xs)
                            .variant(ButtonVariant::Ghost)
                            .disabled(saving)
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(MarkdownEditorEvent::Cancel);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("{:?}-save", self.id)))
                            .size(ButtonSize::Xs)
                            .variant(ButtonVariant::Outline)
                            .disabled(save_disabled)
                            .label(if saving { "Saving..." } else { "Save" })
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
    }
}

// ── Comment form ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommentFormEvent {
    /// A comment went out: the composer closes.
    Posted,
}

/// What pressing a button of the form does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommentSubmit {
    Comment,
    Close,
    Reopen,
}

/// The follow-up that rides with a comment: close an open pull request, reopen a closed one,
/// where the host offers it and this viewer may.
pub fn follow_up_action(
    state: PullRequestState,
    host_actions: &[PullRequestAction],
    viewer_actions: &[PullRequestAction],
) -> Option<CommentSubmit> {
    let allowed = |action| host_actions.contains(&action) && viewer_actions.contains(&action);
    if state == PullRequestState::Open && allowed(PullRequestAction::Close) {
        Some(CommentSubmit::Close)
    } else if state == PullRequestState::Closed && allowed(PullRequestAction::Reopen) {
        Some(CommentSubmit::Reopen)
    } else {
        None
    }
}

/// `PullRequestCommentForm`: the comment half of the floating composer — a remark on the pull
/// request itself, optionally the one that closes or reopens it.
pub struct CommentForm {
    ctx: DetailContext,
    input: Entity<TextareaState>,
    /// The textarea is cleared on the next render, which has the window `set_value` needs.
    reset: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CommentFormEvent> for CommentForm {}

impl CommentForm {
    pub fn new(ctx: DetailContext, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .placeholder("Leave a comment")
        });
        let subscriptions = vec![
            cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
                InputEvent::PressEnter {
                    secondary: true, ..
                } => this.submit(CommentSubmit::Comment, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            }),
            cx.observe(&ctx.store, |_, _, cx| cx.notify()),
        ];
        Self {
            ctx,
            input,
            reset: false,
            _subscriptions: subscriptions,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn action_pending(&self, cx: &App) -> bool {
        self.ctx
            .snapshot(cx)
            .is_some_and(|snapshot| snapshot.pending_action.is_some())
    }

    fn submit(&mut self, action: CommentSubmit, cx: &mut Context<Self>) {
        let body = self.input.read(cx).value().trim().to_owned();
        if body.is_empty() || self.action_pending(cx) {
            return;
        }
        let Some(source) = self.ctx.source(cx) else {
            return;
        };
        // The comment goes first, then the action it rides with.
        source.post_comment(&self.ctx.reference, body, cx);
        match action {
            CommentSubmit::Comment => {}
            CommentSubmit::Close => source.run_action(
                &self.ctx.reference,
                PullRequestAction::Close,
                None,
                None,
                cx,
            ),
            CommentSubmit::Reopen => source.run_action(
                &self.ctx.reference,
                PullRequestAction::Reopen,
                None,
                None,
                cx,
            ),
        }
        self.reset = true;
        cx.emit(CommentFormEvent::Posted);
        cx.notify();
    }
}

impl Render for CommentForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.reset {
            self.reset = false;
            self.input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        let empty = self.input.read(cx).value().trim().is_empty();
        let pending = self.action_pending(cx);
        let follow_up = self
            .ctx
            .snapshot(cx)
            .and_then(|snapshot| match &snapshot.detail {
                Loadable::Ready(detail) => follow_up_action(
                    detail.state,
                    &detail.capabilities.actions,
                    &detail.viewer_permissions.actions,
                ),
                _ => None,
            });
        let disabled = empty || pending;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                Textarea::new(&self.input)
                    .disabled(pending)
                    .aria_label("Comment on this pull request"),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .children(follow_up.map(|action| {
                        let closing = action == CommentSubmit::Close;
                        Button::new("comment-follow-up")
                            .size(ButtonSize::Xs)
                            .variant(if closing {
                                ButtonVariant::DestructiveOutline
                            } else {
                                ButtonVariant::Outline
                            })
                            .disabled(disabled)
                            .icon(if closing {
                                IconName::GitPullRequestClosed
                            } else {
                                IconName::RotateCcw
                            })
                            .icon_size(rems(0.875))
                            .label(if closing {
                                "Close with comment"
                            } else {
                                "Reopen with comment"
                            })
                            .on_click(cx.listener(move |this, _, _, cx| this.submit(action, cx)))
                    }))
                    .child(
                        Button::new("comment-submit")
                            .size(ButtonSize::Xs)
                            .variant(ButtonVariant::Outline)
                            .disabled(disabled)
                            .icon(IconName::Send)
                            .icon_size(rems(0.875))
                            .label("Comment")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.submit(CommentSubmit::Comment, cx)
                            })),
                    ),
            )
    }
}

// ── Composer ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposerMode {
    Comment,
    Review,
}

/// `PullRequestComposer`: the single floating control over a pull request. Commenting on the
/// change and submitting the review that carries the Code tab's line comments are two modes of
/// one composer.
///
/// Opening picks the mode with work waiting in it, so a reader who has collected line comments
/// lands on the review and everyone else lands on the comment box. Either mode's draft survives
/// the toggle: they are separate texts going to separate places.
pub struct PullRequestComposer {
    ctx: DetailContext,
    open: bool,
    requested: ComposerMode,
    review_form: Option<Entity<ReviewForm>>,
    comment_form: Entity<CommentForm>,
    store: Entity<PullRequestReviewStore>,
    _subscriptions: Vec<Subscription>,
}

impl PullRequestComposer {
    pub fn new(ctx: DetailContext, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = review_store(cx);
        let comment_form = cx.new(|cx| CommentForm::new(ctx.clone(), window, cx));
        let subscriptions = vec![
            cx.observe(&ctx.store, |_, _, cx| cx.notify()),
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe(&comment_form, |this, _, event: &CommentFormEvent, cx| {
                let CommentFormEvent::Posted = event;
                this.open = false;
                cx.notify();
            }),
        ];
        Self {
            ctx,
            open: false,
            requested: ComposerMode::Comment,
            review_form: None,
            comment_form,
            store,
            _subscriptions: subscriptions,
        }
    }

    /// The guide's last stop asks for the review: opens the composer on it.
    pub fn open_review(&mut self, cx: &mut Context<Self>) {
        self.requested = ComposerMode::Review;
        self.open = true;
        cx.notify();
    }

    /// What is offered: the intersection of what this host can do at all, and what this account
    /// may do on this repository.
    fn offers(&self, cx: &App) -> Option<(bool, Vec<PullRequestReviewVerdict>)> {
        let snapshot = self.ctx.snapshot(cx)?;
        let Loadable::Ready(detail) = &snapshot.detail else {
            return None;
        };
        let can_comment = detail.capabilities.comment && detail.viewer_permissions.comment;
        let verdicts: Vec<_> = detail
            .capabilities
            .review
            .verdicts
            .iter()
            .copied()
            .filter(|verdict| detail.viewer_permissions.verdicts.contains(verdict))
            .collect();
        (can_comment || !verdicts.is_empty()).then_some((can_comment, verdicts))
    }

    fn pending_comments(&self, cx: &App) -> usize {
        self.store
            .read(cx)
            .pending_review_comments(&self.ctx.reference)
            .len()
    }

    fn summary_started(&self, cx: &App) -> bool {
        self.store
            .read(cx)
            .summaries
            .get(&pull_request_review_key(&self.ctx.reference))
            .is_some_and(|summary| !summary.trim().is_empty())
    }
}

impl Render for PullRequestComposer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some((can_comment, verdicts)) = self.offers(cx) else {
            return div().into_any_element();
        };
        let pending = self.pending_comments(cx);
        let review_started = pending > 0 || self.summary_started(cx);
        if !verdicts.is_empty() && self.review_form.is_none() {
            let ctx = self.ctx.clone();
            let verdicts = verdicts.clone();
            self.review_form = Some(cx.new(|cx| ReviewForm::new(ctx, verdicts, window, cx)));
            if let Some(form) = &self.review_form {
                cx.subscribe(form, |this, _, event: &ReviewFormEvent, cx| {
                    let ReviewFormEvent::Submitted = event;
                    this.open = false;
                    cx.notify();
                })
                .detach();
            }
        }
        let mode = if can_comment {
            if verdicts.is_empty() {
                ComposerMode::Comment
            } else {
                self.requested
            }
        } else {
            ComposerMode::Review
        };
        let label = if pending > 0 {
            format!(
                "Review pull request, {pending} {} pending",
                if pending == 1 { "comment" } else { "comments" }
            )
        } else if review_started || !can_comment {
            "Review pull request".to_owned()
        } else {
            "Comment on pull request".to_owned()
        };
        let entity = cx.entity().downgrade();
        let trigger = ComposerTrigger {
            label: SharedString::from(label),
            pending,
            open: false,
        };
        let review_form = self.review_form.clone();
        let comment_form = self.comment_form.clone();
        let content_entity = cx.entity().downgrade();
        let open_entity = entity.clone();
        let both = can_comment && !verdicts.is_empty();
        popover(
            "pull-request-composer",
            PopoverOptions {
                width: PopoverWidth::Lg,
                ..PopoverOptions::default()
            },
            window,
            cx,
        )
        .anchor(Anchor::BottomRight)
        .open(self.open)
        .on_open_change(move |next, _, cx| {
            let next = *next;
            open_entity
                .update(cx, |this, cx| {
                    if next {
                        this.requested = if review_started {
                            ComposerMode::Review
                        } else {
                            ComposerMode::Comment
                        };
                    }
                    this.open = next;
                    cx.notify();
                })
                .ok();
        })
        .trigger(trigger)
        .content(move |popover_state, window, cx| {
            let theme = cx.app_theme().clone();
            let weak_popover = cx.entity().downgrade();
            let set_mode = |mode: ComposerMode| {
                let entity = content_entity.clone();
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                    entity
                        .update(cx, |this, cx| {
                            this.requested = mode;
                            cx.notify();
                        })
                        .ok();
                }
            };
            let _ = (window, popover_state);
            let toggle = |id: &'static str, label: String, pressed: bool, mode| {
                Toggle::new(id)
                    .variant(ToggleVariant::Segmented)
                    .size(ToggleSize::Segmented)
                    .pressed(pressed)
                    .label(label)
                    .on_click(set_mode(mode))
            };
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .mb_3()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(if both {
                            div()
                                .flex()
                                .flex_shrink_0()
                                .gap(px(2.))
                                .p(px(2.))
                                .rounded(RADIUS_LG)
                                .bg(with_alpha(theme.input, 0.4))
                                .child(toggle(
                                    "composer-comment",
                                    "Comment".into(),
                                    mode == ComposerMode::Comment,
                                    ComposerMode::Comment,
                                ))
                                .child(toggle(
                                    "composer-review",
                                    if pending > 0 {
                                        format!("Review ({pending})")
                                    } else {
                                        "Review".into()
                                    },
                                    mode == ComposerMode::Review,
                                    ComposerMode::Review,
                                ))
                                .into_any_element()
                        } else {
                            popover_title(if mode == ComposerMode::Review {
                                "Review pull request"
                            } else {
                                "Comment on pull request"
                            })
                            .into_any_element()
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .when(mode == ComposerMode::Review && pending > 0, |this| {
                                    let entity = content_entity.clone();
                                    this.child(
                                        Button::new("composer-discard")
                                            .size(ButtonSize::IconXs)
                                            .variant(ButtonVariant::Ghost)
                                            .icon(IconName::Trash)
                                            .icon_size(rems(0.875))
                                            .tooltip(Tooltip::new("Discard pending line comments"))
                                            .on_click(move |_, _, cx| {
                                                entity
                                                    .update(cx, |this, cx| {
                                                        let key = pull_request_review_key(
                                                            &this.ctx.reference,
                                                        );
                                                        this.store.update(cx, |store, _| {
                                                            store.clear(&key)
                                                        });
                                                    })
                                                    .ok();
                                            }),
                                    )
                                })
                                .child(
                                    Button::new("composer-close")
                                        .size(ButtonSize::IconXs)
                                        .variant(ButtonVariant::Ghost)
                                        .icon(IconName::X)
                                        .icon_size(rems(0.875))
                                        .tooltip(Tooltip::new("Close composer"))
                                        .on_click({
                                            let entity = content_entity.clone();
                                            move |_, window, cx| {
                                                entity
                                                    .update(cx, |this, cx| {
                                                        this.open = false;
                                                        cx.notify();
                                                    })
                                                    .ok();
                                                weak_popover
                                                    .update(cx, |state, cx| {
                                                        state.dismiss(window, cx)
                                                    })
                                                    .ok();
                                            }
                                        }),
                                ),
                        ),
                )
                // Both forms stay mounted across toggles so drafts survive; one is shown.
                .children(review_form.clone().map(|form| {
                    div()
                        .when(mode != ComposerMode::Review, |this| this.hidden())
                        .child(form)
                }))
                .child(
                    div()
                        .when(mode != ComposerMode::Comment || !can_comment, |this| {
                            this.hidden()
                        })
                        .child(comment_form.clone()),
                )
        })
        .into_any_element()
    }
}

/// The composer's trigger: `Button size="icon" variant="glass"` with the pending count badge.
#[derive(IntoElement)]
struct ComposerTrigger {
    label: SharedString,
    pending: usize,
    open: bool,
}

impl gpui_kit::component::Selectable for ComposerTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl gpui_kit::RenderOnce for ComposerTrigger {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        div()
            .relative()
            .child(
                Button::new("composer-trigger")
                    .size(ButtonSize::Icon)
                    .variant(ButtonVariant::Glass)
                    .pressed(self.open)
                    .icon(IconName::MessageSquare)
                    .icon_size(rems(1.))
                    .tooltip(Tooltip::new(self.label)),
            )
            .when(self.pending > 0, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(-4.))
                        .right(px(-4.))
                        .flex()
                        .h(px(14.))
                        .min_w(px(14.))
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(theme.info)
                        .px(px(4.))
                        .text_size(TEXT_3XS.size)
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(gpui_kit::white())
                        .child(SharedString::from(self.pending.to_string())),
                )
            })
    }
}
