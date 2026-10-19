//! Port of upstream/apps/web/src/components/pullRequest/PullRequestCommentBody.tsx,
//! PullRequestMarkdown.tsx, PullRequestReactions.tsx and PullRequestEditButton.tsx: the shared
//! pieces every remark on the Timeline, Code and Summary tabs is built from.
//!
//! `PullRequestMarkdown` runs `remarkPullRequestAutolinks` through ChatMarkdown's extra remark
//! plugin seam (`pull_request_markdown_logic::apply_pull_request_autolinks`), with the
//! repository the open pull request belongs to read from [`PullRequestMarkdownContext`].
//!
//! Not ported: the video/attachment segments of `PullRequestMarkdown` (`splitPullRequestBody`,
//! GitHub media signing: no asset service in the view layer, so a media link renders as a link),
//! `onFocusCapture` expanding a clamped body (no focus inside a read-only body), and the
//! reaction's optimistic rollback toast (commands return immediately).
//!
//! Port of upstream/apps/web/src/components/pullRequest/PullRequestReactions.tsx.
//! Port of upstream/apps/web/src/components/pullRequest/PullRequestMarkdown.tsx.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, App, ElementId, Global, InteractiveElement as _, IntoElement, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    linear_color_stop, linear_gradient, px, rems,
};
use turin_contracts::pull_request::{PullRequestReaction, PullRequestReactionContent};

use super::detail_context::DetailContext;
use super::pull_request_markdown_logic::apply_pull_request_autolinks;
use super::pull_request_reactions_logic::{
    PULL_REQUEST_REACTION_ORDER, apply_pending_pull_request_reactions, pull_request_reaction_emoji,
    pull_request_reaction_name, pull_request_reaction_tooltip,
};
use crate::assets::SMILE_PLUS;
use crate::components::chat::markdown::{ChatMarkdown, ExtraRemarkPlugin};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_MD, TEXT_BASE, TEXT_XS};
use crate::ui::{
    Button, ButtonSize, ButtonVariant, Icon, IconName, PopoverOptions, PopoverPadding, Tooltip,
    popover,
};

type ClickHandler = Rc<dyn Fn(&mut Window, &mut App)>;
type ToggleReaction = Rc<dyn Fn(PullRequestReactionContent, bool, &mut App)>;

/// The id a reaction on the change request's own description is sent with (upstream omits
/// `subjectId`; the source resolves the host's id for the description).
pub const PULL_REQUEST_SUBJECT: &str = "";

/// `PullRequestMarkdownContext`: the repository url the detail panel provides to the markdown
/// beneath it, so `#123` and commit hashes link into that repository.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PullRequestMarkdownContext {
    pub repository_url: Option<String>,
}

impl Global for PullRequestMarkdownContext {}

/// The detail panel's `PullRequestMarkdownContext.Provider`. Sets the global only when it
/// changes, so rendering does not notify observers every frame.
pub fn provide_pull_request_markdown_context(cx: &mut App, repository_url: Option<String>) {
    let next = PullRequestMarkdownContext { repository_url };
    if cx.try_global::<PullRequestMarkdownContext>() != Some(&next) {
        cx.set_global(next);
    }
}

/// `PullRequestMarkdown`: the body through ChatMarkdown, with the same-repository autolinks when
/// the context names a repository.
#[derive(IntoElement)]
pub struct PullRequestMarkdown {
    id: ElementId,
    text: SharedString,
}

impl RenderOnce for PullRequestMarkdown {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let repository_url = cx
            .try_global::<PullRequestMarkdownContext>()
            .and_then(|context| context.repository_url.clone());
        let markdown = ChatMarkdown::new(self.id, self.text);
        match repository_url {
            Some(repository_url) => markdown.extra_remark_plugin(ExtraRemarkPlugin {
                key: SharedString::from(format!("pull-request-autolinks:{repository_url}")),
                apply: Arc::new(move |document| {
                    apply_pull_request_autolinks(document, &repository_url)
                }),
            }),
            None => markdown,
        }
    }
}

pub fn pull_request_markdown(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
) -> PullRequestMarkdown {
    PullRequestMarkdown {
        id: id.into(),
        text: text.into(),
    }
}

/// A remark's keyed state: whether it is showing in full and whether it is taller than the clamp.
#[derive(Default)]
struct BodyState {
    expanded: bool,
    overflowing: bool,
}

/// `PullRequestCommentBody`: keeps the complete markdown intact while limiting long reports to a
/// readable preview (240px) with a fade and a "Show full comment" button.
#[derive(IntoElement)]
pub struct CommentBody {
    id: ElementId,
    text: SharedString,
}

impl CommentBody {
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
        }
    }
}

/// `max-h-[240px]`.
const CLAMP_HEIGHT: f32 = 240.;

impl RenderOnce for CommentBody {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let state = window.use_keyed_state(
            ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "body-state".into()),
            cx,
            |_, _| BodyState::default(),
        );
        let (expanded, overflowing) = {
            let state = state.read(cx);
            (state.expanded, state.overflowing)
        };
        let measure = state.clone();
        let toggle = state.clone();
        div()
            .min_w_0()
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .when(!expanded, |this| {
                        this.max_h(px(CLAMP_HEIGHT)).overflow_hidden()
                    })
                    .on_children_prepainted(move |bounds, _, cx| {
                        let Some(content) = bounds.first() else {
                            return;
                        };
                        let tall = content.size.height > px(CLAMP_HEIGHT);
                        if measure.read(cx).overflowing != tall {
                            measure.update(cx, |state, cx| {
                                state.overflowing = tall;
                                cx.notify();
                            });
                        }
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .child(pull_request_markdown(self.id.clone(), self.text.clone())),
                    )
                    .when(overflowing && !expanded, |this| {
                        // `h-10 bg-gradient-to-t from-background to-transparent`
                        this.child(div().absolute().left_0().right_0().bottom_0().h_10().bg(
                            linear_gradient(
                                180.,
                                linear_color_stop(with_alpha(theme.background, 0.), 0.),
                                linear_color_stop(theme.background, 1.),
                            ),
                        ))
                    }),
            )
            .when(overflowing, |this| {
                this.child(
                    div().mt_2().flex().child(
                        Button::new(ElementId::NamedChild(
                            std::sync::Arc::new(self.id.clone()),
                            "body-toggle".into(),
                        ))
                        .size(ButtonSize::Xs)
                        .variant(ButtonVariant::GhostMuted)
                        .label(if expanded {
                            "Show less"
                        } else {
                            "Show full comment"
                        })
                        .on_click(move |_, _, cx| {
                            toggle.update(cx, |state, cx| {
                                state.expanded = !state.expanded;
                                cx.notify();
                            });
                        }),
                    ),
                )
            })
    }
}

// ── Edit button ─────────────────────────────────────────────────────────────

/// `PullRequestEditButton`: a pencil that reveals on hover of the surrounding `group`
/// (`opacity-0 group-hover:opacity-100`; there is no coarse pointer here).
#[derive(IntoElement)]
pub struct EditButton {
    id: ElementId,
    group: SharedString,
    label: SharedString,
    on_click: Option<ClickHandler>,
    margin_top: bool,
}

impl EditButton {
    /// `group` names the ancestor whose hover reveals the pencil.
    pub fn new(
        id: impl Into<ElementId>,
        group: impl Into<SharedString>,
        label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            group: group.into(),
            label: label.into(),
            on_click: None,
            margin_top: false,
        }
    }

    /// `className="-mt-1"`.
    pub fn nudge_up(mut self) -> Self {
        self.margin_top = true;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for EditButton {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let handler = self.on_click;
        div()
            .flex()
            .flex_shrink_0()
            .opacity(0.)
            .group_hover(self.group, |style| style.opacity(1.))
            .when(self.margin_top, |this| this.mt(px(-4.)))
            .child(
                Button::new(self.id)
                    .size(ButtonSize::IconXs)
                    .variant(ButtonVariant::GhostMuted)
                    .icon(IconName::Pencil)
                    .icon_size(rems(0.75))
                    .tooltip(Tooltip::new(self.label))
                    .on_click(move |_, window, cx| {
                        if let Some(handler) = &handler {
                            handler(window, cx);
                        }
                    }),
            )
    }
}

// ── Reactions ───────────────────────────────────────────────────────────────

/// What the host last said, so a press in flight is forgotten the moment the real counts land.
fn reactions_signature(reactions: &[PullRequestReaction]) -> String {
    reactions
        .iter()
        .map(|reaction| {
            format!(
                "{:?}:{}:{}",
                reaction.content,
                reaction.count,
                u8::from(reaction.viewer_has_reacted)
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Default)]
struct ReactionState {
    signature: String,
    pending: HashMap<PullRequestReactionContent, bool>,
}

/// The picker button: a pill with the smile glyph; marked open while the popover is.
#[derive(IntoElement)]
struct PickerTrigger {
    id: ElementId,
    open: bool,
}

impl gpui_kit::component::Selectable for PickerTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl RenderOnce for PickerTrigger {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let border = with_alpha(theme.border, 0.7);
        let hover_border = with_alpha(theme.primary, 0.6);
        let open = self.open;
        let _ = window;
        pill(self.id)
            .px(rems(0.375))
            .border_color(if open { hover_border } else { border })
            .text_color(if open {
                theme.foreground
            } else {
                theme.muted_foreground
            })
            .hover(move |style| {
                style
                    .border_color(hover_border)
                    .text_color(theme.foreground)
            })
            .child(Icon::path(SMILE_PLUS).size(rems(0.875)))
    }
}

/// `PILL_CLASS`: `inline-flex h-6 shrink-0 items-center gap-1 rounded-full border px-2 text-xs`.
fn pill(id: impl Into<ElementId>) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap_1()
        .h_6()
        .px_2()
        .rounded_full()
        .border_1()
        .text_size(TEXT_XS.size)
        .line_height(TEXT_XS.line_height)
        .cursor_pointer()
}

/// `PullRequestReactionBar`: reaction counts and an always-visible picker, routed to the supplied
/// host subject.
#[derive(IntoElement)]
pub struct ReactionBar {
    id: ElementId,
    ctx: DetailContext,
    reactions: Vec<PullRequestReaction>,
    can_react: bool,
    subject_id: String,
    justify_end: bool,
}

impl ReactionBar {
    /// `subject_id` is the remark's id, or [`PULL_REQUEST_SUBJECT`] for the description.
    pub fn new(
        id: impl Into<ElementId>,
        ctx: DetailContext,
        reactions: Vec<PullRequestReaction>,
        can_react: bool,
        subject_id: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            ctx,
            reactions,
            can_react,
            subject_id: subject_id.into(),
            justify_end: false,
        }
    }

    /// `className="ml-auto justify-end"`.
    pub fn justify_end(mut self) -> Self {
        self.justify_end = true;
        self
    }
}

impl RenderOnce for ReactionBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let state = window.use_keyed_state(
            ElementId::NamedChild(
                std::sync::Arc::new(self.id.clone()),
                "reaction-state".into(),
            ),
            cx,
            |_, _| ReactionState::default(),
        );
        let signature = reactions_signature(&self.reactions);
        let pending = {
            let state = state.read(cx);
            if state.signature == signature {
                state.pending.clone()
            } else {
                HashMap::new()
            }
        };
        let shown = apply_pending_pull_request_reactions(&self.reactions, &pending);
        if shown.is_empty() && !self.can_react {
            return div().into_any_element();
        }

        let can_react = self.can_react;
        let ctx = self.ctx.clone();
        let subject = self.subject_id.clone();
        let toggle: ToggleReaction = {
            let state = state.clone();
            Rc::new(move |content, reacted, cx| {
                state.update(cx, |state, cx| {
                    if state.signature != signature {
                        state.signature = signature.clone();
                        state.pending.clear();
                    }
                    state.pending.insert(content, reacted);
                    cx.notify();
                });
                if let Some(source) = ctx.source(cx) {
                    source.toggle_reaction(&ctx.reference, &subject, content, cx);
                }
            })
        };

        let id = self.id.clone();
        let mut bar = div()
            .flex()
            .flex_wrap()
            .min_w_0()
            .max_w_full()
            .items_center()
            .gap_1()
            .when(self.justify_end, |this| this.ml_auto().justify_end());

        for reaction in &shown {
            let content = reaction.content;
            let reacted = reaction.viewer_has_reacted;
            let tooltip = SharedString::from(pull_request_reaction_tooltip(reaction));
            let toggle = toggle.clone();
            let border = if reacted {
                with_alpha(theme.primary, 0.6)
            } else {
                with_alpha(theme.border, 0.7)
            };
            let hover_border = with_alpha(theme.primary, 0.6);
            bar = bar.child(
                pill(ElementId::NamedChild(
                    std::sync::Arc::new(id.clone()),
                    SharedString::from(format!("pill-{content:?}")),
                ))
                .border_color(border)
                .bg(if reacted {
                    with_alpha(theme.primary, 0.1)
                } else {
                    with_alpha(theme.muted, 0.4)
                })
                .text_color(if reacted {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .when(can_react, |this| {
                    this.hover(move |style| style.border_color(hover_border))
                })
                .when(!can_react, |this| this.cursor_default())
                .child(pull_request_reaction_emoji(content))
                .child(reaction.count.get().to_string())
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .on_click(move |_, _, cx| {
                    if can_react {
                        toggle(content, !reacted, cx);
                    }
                }),
            );
        }

        if can_react {
            let shown_for_picker = shown.clone();
            let toggle = toggle.clone();
            let trigger = PickerTrigger {
                id: ElementId::NamedChild(std::sync::Arc::new(id.clone()), "add-reaction".into()),
                open: false,
            };
            bar = bar.child(
                popover(
                    ElementId::NamedChild(std::sync::Arc::new(id.clone()), "picker".into()),
                    PopoverOptions {
                        padding: PopoverPadding::Compact,
                        ..PopoverOptions::default()
                    },
                    window,
                    cx,
                )
                .anchor(Anchor::BottomLeft)
                .trigger(trigger)
                .content(move |popover_state, _window, cx| {
                    let theme = cx.app_theme().clone();
                    let mut row = div().flex().items_center().gap(rems(0.125));
                    for content in PULL_REQUEST_REACTION_ORDER {
                        let reacted = shown_for_picker
                            .iter()
                            .find(|reaction| reaction.content == content)
                            .is_some_and(|reaction| reaction.viewer_has_reacted);
                        let toggle = toggle.clone();
                        let popover = cx.entity().downgrade();
                        let hover = theme.accent;
                        row = row.child(
                            div()
                                .id(SharedString::from(format!("pick-{content:?}")))
                                .flex()
                                .size_7()
                                .items_center()
                                .justify_center()
                                .rounded(RADIUS_MD)
                                .text_size(TEXT_BASE.size)
                                .cursor_pointer()
                                .when(reacted, |this| this.bg(with_alpha(theme.primary, 0.1)))
                                .hover(move |style| style.bg(hover))
                                .tooltip(move |window, cx| {
                                    Tooltip::new(pull_request_reaction_name(content))
                                        .build(window, cx)
                                })
                                .child(pull_request_reaction_emoji(content))
                                .on_click(move |_, window, cx| {
                                    popover
                                        .update(cx, |state, cx| state.dismiss(window, cx))
                                        .ok();
                                    toggle(content, !reacted, cx);
                                }),
                        );
                    }
                    let _ = &popover_state;
                    row
                }),
            );
        }
        bar.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use turin_contracts::PositiveInt;

    fn reaction(
        content: PullRequestReactionContent,
        count: u64,
        viewer: bool,
    ) -> PullRequestReaction {
        PullRequestReaction {
            content,
            count: PositiveInt::new(count).unwrap(),
            actors: Vec::new(),
            viewer_has_reacted: viewer,
        }
    }

    #[test]
    fn signature_changes_with_counts_and_viewer_flag() {
        let one = [reaction(PullRequestReactionContent::Heart, 2, false)];
        let two = [reaction(PullRequestReactionContent::Heart, 2, true)];
        let three = [reaction(PullRequestReactionContent::Heart, 3, false)];
        assert_eq!(reactions_signature(&one), reactions_signature(&one));
        assert_ne!(reactions_signature(&one), reactions_signature(&two));
        assert_ne!(reactions_signature(&one), reactions_signature(&three));
        assert_eq!(reactions_signature(&[]), "");
    }
}
