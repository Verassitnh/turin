//! Port of upstream/apps/web/src/components/pullRequest/PullRequestReviewForm.tsx: the review half
//! of the floating composer — the summary and the verdict that sends it, together with whatever
//! line comments the review is holding. The popover around it belongs to the composer
//! (`comment_form`).
//!
//! The Zustand `pullRequestReviewStore` is one [`PullRequestReviewStore`] entity held as a GPUI
//! global ([`review_store`]) so the Code tab (which queues line comments) and this form share it.
//!
//! Not ported: toasts ("Review submitted", "The review could not be submitted") — commands
//! return immediately and failures arrive through the snapshot's `action_error`;
//! `reviewSummaryRequired` is `false` for GitHub, the only host kept, so a summary is never
//! required to request changes.

use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::{
    Anchor, App, AppContext as _, Context, Entity, EventEmitter, Global, IntoElement,
    ParentElement as _, Render, Styled as _, Subscription, Window, div, rems,
};
use turin_contracts::pull_request::PullRequestReviewVerdict;

use super::detail_context::DetailContext;
use super::dropdown::dropdown;
use super::pull_request_review_store::{PullRequestReviewStore, pull_request_review_key};
use super::view_model::ReviewSubmission;
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

struct ReviewStoreGlobal(Entity<PullRequestReviewStore>);

impl Global for ReviewStoreGlobal {}

/// The review being written, for every pull request, shared by the Code tab and the composer.
pub fn review_store(cx: &mut App) -> Entity<PullRequestReviewStore> {
    if let Some(global) = cx.try_global::<ReviewStoreGlobal>() {
        return global.0.clone();
    }
    let store = cx.new(|_| PullRequestReviewStore::new());
    cx.set_global(ReviewStoreGlobal(store.clone()));
    store
}

/// `VERDICTS`.
pub struct VerdictOption {
    pub value: PullRequestReviewVerdict,
    pub label: &'static str,
    pub sent: &'static str,
    pub icon: IconName,
}

pub const VERDICTS: [VerdictOption; 3] = [
    VerdictOption {
        value: PullRequestReviewVerdict::Comment,
        label: "Comment",
        sent: "Review submitted",
        icon: IconName::MessageSquare,
    },
    VerdictOption {
        value: PullRequestReviewVerdict::Approve,
        label: "Approve",
        sent: "Pull request approved",
        icon: IconName::Check,
    },
    VerdictOption {
        value: PullRequestReviewVerdict::RequestChanges,
        label: "Request changes",
        sent: "Changes requested",
        icon: IconName::CircleX,
    },
];

/// `canSubmit`: an approval needs no words; the others need a summary or a line comment.
pub fn can_submit(
    verdict: PullRequestReviewVerdict,
    summary: &str,
    pending_comments: usize,
    request_changes_summary_required: bool,
) -> bool {
    if verdict == PullRequestReviewVerdict::RequestChanges && request_changes_summary_required {
        !summary.trim().is_empty()
    } else {
        verdict == PullRequestReviewVerdict::Approve
            || !summary.trim().is_empty()
            || pending_comments > 0
    }
}

/// The placeholder asks for exactly what the submit button is waiting on.
pub fn summary_placeholder(
    verdict: Option<PullRequestReviewVerdict>,
    request_changes_summary_required: bool,
) -> &'static str {
    match verdict {
        None => "Summarize your review (optional)",
        Some(verdict) if can_submit(verdict, "", 0, request_changes_summary_required) => {
            "Summarize your review (optional)"
        }
        Some(PullRequestReviewVerdict::RequestChanges) if request_changes_summary_required => {
            "Summarize your review (required to request changes)"
        }
        Some(_) => "Summarize your review or add line comments",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewFormEvent {
    Submitted,
}

pub struct ReviewForm {
    ctx: DetailContext,
    verdicts: Vec<PullRequestReviewVerdict>,
    requested: PullRequestReviewVerdict,
    summary: Entity<TextareaState>,
    store: Entity<PullRequestReviewStore>,
    pending: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ReviewFormEvent> for ReviewForm {}

impl ReviewForm {
    pub fn new(
        ctx: DetailContext,
        verdicts: Vec<PullRequestReviewVerdict>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = review_store(cx);
        let key = pull_request_review_key(&ctx.reference);
        let initial = store
            .read(cx)
            .summaries
            .get(&key)
            .cloned()
            .unwrap_or_default();
        let summary = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .default_value(initial)
                .placeholder("Summarize your review (optional)")
        });
        let change_key = key.clone();
        let subscriptions = vec![
            cx.subscribe(&summary, move |this, state, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = state.read(cx).value().to_string();
                    this.store
                        .update(cx, |store, _| store.set_summary(&change_key, &text));
                    cx.notify();
                }
            }),
            cx.observe(&store, |_, _, cx| cx.notify()),
        ];
        Self {
            ctx,
            verdicts,
            requested: PullRequestReviewVerdict::Comment,
            summary,
            store,
            pending: false,
            _subscriptions: subscriptions,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.summary
            .update(cx, |summary, cx| summary.focus(window, cx));
    }

    pub fn set_pending(&mut self, pending: bool, cx: &mut Context<Self>) {
        self.pending = pending;
        cx.notify();
    }

    fn offered(&self) -> Vec<&'static VerdictOption> {
        VERDICTS
            .iter()
            .filter(|option| self.verdicts.contains(&option.value))
            .collect()
    }

    fn selected(&self) -> Option<&'static VerdictOption> {
        let offered = self.offered();
        offered
            .iter()
            .copied()
            .find(|option| option.value == self.requested)
            .or_else(|| offered.first().copied())
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        let Some(verdict) = self.selected() else {
            return;
        };
        let key = pull_request_review_key(&self.ctx.reference);
        let body = self.summary.read(cx).value().to_string();
        let comments: Vec<_> = self
            .store
            .read(cx)
            .pending_review_comments(&self.ctx.reference)
            .to_vec();
        if !can_submit(verdict.value, &body, comments.len(), false) {
            return;
        }
        if let Some(source) = self.ctx.source(cx) {
            source.submit_review_with_comments(
                &self.ctx.reference,
                ReviewSubmission {
                    verdict: verdict.value,
                    body: body.clone(),
                },
                comments
                    .iter()
                    .map(|comment| comment.draft.clone())
                    .collect(),
                cx,
            );
        }
        // More remarks may have been added while the host was accepting this snapshot. Leave
        // those, and any summary revised in the meantime, ready for the next review.
        let ids: Vec<String> = comments.iter().map(|comment| comment.id.clone()).collect();
        self.store.update(cx, |store, _| {
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            store.remove_comments(&key, &ids);
            store.clear_summary(&key, &body);
        });
        self.summary
            .update(cx, |summary, cx| summary.set_value("", window, cx));
        cx.emit(ReviewFormEvent::Submitted);
        cx.notify();
    }
}

impl Render for ReviewForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let offered = self.offered();
        let selected = self.selected();
        let body = self.summary.read(cx).value().to_string();
        let pending_comments = self
            .store
            .read(cx)
            .pending_review_comments(&self.ctx.reference)
            .len();
        let submit_disabled = self.pending
            || selected
                .is_none_or(|verdict| !can_submit(verdict.value, &body, pending_comments, false));
        let placeholder = summary_placeholder(selected.map(|verdict| verdict.value), false);
        self.summary.update(cx, |summary, cx| {
            summary.set_placeholder(placeholder, window, cx)
        });
        let entity = cx.entity().downgrade();
        let verdict_menu = (offered.len() > 1).then(|| {
            let selected = selected.expect("a verdict is offered");
            let trigger = Button::new("review-verdict")
                .size(ButtonSize::Xs)
                .variant(ButtonVariant::Outline)
                .disabled(self.pending)
                .child(Icon::new(selected.icon).size(rems(0.75)))
                .child(selected.label)
                .child(Icon::new(IconName::ChevronDown).size(rems(0.75)));
            let options: Vec<_> = offered.clone();
            let current = selected.value;
            div().flex_none().child(
                dropdown(
                    "review-verdict-menu",
                    Anchor::BottomLeft,
                    trigger,
                    move |mut menu, _, _| {
                        for option in &options {
                            let entity = entity.clone();
                            let value = option.value;
                            menu = menu.item(
                                PopupMenuItem::new(option.label)
                                    .checked(option.value == current)
                                    .on_click(move |_, _, cx| {
                                        entity
                                            .update(cx, |this, cx| {
                                                this.requested = value;
                                                cx.notify();
                                            })
                                            .ok();
                                    }),
                            );
                        }
                        menu
                    },
                )
                .trigger_style(gpui_kit::StyleRefinement::default().flex_none().w_auto()),
            )
        });
        div()
            .child(Textarea::new(&self.summary).aria_label("Review summary"))
            .child(
                div()
                    .mt_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(verdict_menu)
                    .child(
                        div().ml_auto().child(
                            Button::new("review-submit")
                                .size(ButtonSize::Xs)
                                .variant(ButtonVariant::Default)
                                .disabled(submit_disabled)
                                .label(if self.pending {
                                    "Submitting..."
                                } else {
                                    "Submit review"
                                })
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.submit(window, cx)),
                                ),
                        ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PullRequestReviewVerdict::*;

    #[test]
    fn approval_needs_no_words_the_others_do() {
        assert!(can_submit(Approve, "", 0, false));
        assert!(!can_submit(Comment, "  ", 0, false));
        assert!(can_submit(Comment, "looks fine", 0, false));
        assert!(can_submit(RequestChanges, "", 2, false));
    }

    #[test]
    fn a_host_that_requires_a_summary_wants_one_to_request_changes() {
        assert!(!can_submit(RequestChanges, "", 3, true));
        assert!(can_submit(RequestChanges, "because", 0, true));
        assert!(can_submit(Approve, "", 0, true));
    }

    #[test]
    fn placeholder_asks_for_what_submit_waits_on() {
        assert_eq!(
            summary_placeholder(None, false),
            "Summarize your review (optional)"
        );
        assert_eq!(
            summary_placeholder(Some(Approve), false),
            "Summarize your review (optional)"
        );
        assert_eq!(
            summary_placeholder(Some(Comment), false),
            "Summarize your review or add line comments"
        );
        assert_eq!(
            summary_placeholder(Some(RequestChanges), true),
            "Summarize your review (required to request changes)"
        );
    }
}
