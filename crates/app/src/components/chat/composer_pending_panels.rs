//! Port of upstream/apps/web/src/components/chat/ComposerPendingApprovalPanel.tsx,
//! ComposerPendingApprovalActions.tsx, ComposerPendingUserInputPanel.tsx and
//! the parts of ComposerBanner.tsx they sit in (`Attachment`, `Root`, `Row`,
//! `Icon`, `Content`, `Actions`, `Body`): the drawer attached above the
//! composer while the agent waits on the user.
//!
//! Selection drafts live in element state (upstream keeps them in the
//! composer draft store). The approval "more options" menu is an inline
//! disclosure, and the 1–9 option shortcuts are not bound yet.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ElementId, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, px, rems,
};

use super::markdown::ChatMarkdown;
use super::timeline::source::TimelineSource;
use super::timeline::turn_item::{
    ProviderApprovalDecision, ProviderApprovalOption, ProviderRequestKind,
};
use super::timeline::types::{PendingApproval, PendingUserInput, ResponseCapability};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{
    RADIUS_2XL, RADIUS_MD, TEXT_2XS, TEXT_3XS, TEXT_SM, TEXT_XS, shadow_composer,
};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

/// `--chat-composer-drawer-inset: 1.375rem`.
const DRAWER_INSET_REM: f32 = 1.375;
/// `--chat-composer-attachment-overlap: calc(1rem + 1px)`.
fn attachment_overlap(window: &Window) -> gpui_kit::Pixels {
    window.rem_size() + px(1.)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BannerVariant {
    Default,
    Warning,
}

/// `ComposerBanner.Attachment` + `Root`: a surface that tucks under the
/// composer's top edge by the overlap.
fn banner_surface(
    variant: BannerVariant,
    spacious: bool,
    child: AnyElement,
    window: &Window,
    theme: &AppTheme,
) -> AnyElement {
    let dark = theme.is_dark();
    let surface = if dark {
        theme.surface_raised
    } else {
        theme.card
    };
    let (outline, tint): (Hsla, Option<Hsla>) = match variant {
        BannerVariant::Warning => (
            with_alpha(theme.warning, 0.28).into(),
            Some(with_alpha(theme.warning, 0.08).into()),
        ),
        BannerVariant::Default => (
            if dark {
                with_alpha(crate::theme::tokens::tw::WHITE, 0.05).into()
            } else {
                with_alpha(theme.foreground, 0.08).into()
            },
            None,
        ),
    };
    let overlap = attachment_overlap(window);
    div()
        .mx(rems(DRAWER_INSET_REM))
        .mb(-overlap)
        .child(
            div()
                .relative()
                .rounded_t(RADIUS_2XL)
                .border_1()
                .border_color(outline)
                .bg(surface)
                .shadow(shadow_composer(dark))
                .child(
                    div()
                        .rounded_t(RADIUS_2XL)
                        .when_some(tint, |this, tint| this.bg(tint))
                        .px(if spacious { rems(0.75) } else { rems(0.25) })
                        .pt(if spacious { rems(0.75) } else { rems(0.25) })
                        .pb((if spacious {
                            window.rem_size() * 0.75
                        } else {
                            window.rem_size() * 0.25
                        }) + overlap)
                        .text_size(TEXT_XS.size)
                        .line_height(rems(1.))
                        .child(child),
                ),
        )
        .into_any_element()
}

/// Options shown when the provider advertises none.
fn default_approval_options() -> Vec<ProviderApprovalOption> {
    let option = |decision, label: &str| ProviderApprovalOption {
        decision,
        label: label.into(),
        warning: None,
    };
    vec![
        option(ProviderApprovalDecision::Cancel, "Cancel"),
        option(ProviderApprovalDecision::Decline, "Decline"),
        option(
            ProviderApprovalDecision::AcceptForSession,
            "Always allow this session",
        ),
        option(ProviderApprovalDecision::Accept, "Approve"),
    ]
}

fn approval_labels(kind: ProviderRequestKind) -> (&'static str, &'static str) {
    match kind {
        ProviderRequestKind::McpElicitation => ("App access approval", "App access request"),
        ProviderRequestKind::Command => ("Command approval", "Command"),
        ProviderRequestKind::FileRead => ("File read approval", "File to read"),
        ProviderRequestKind::Permission => ("App permission approval", "Permission request"),
        ProviderRequestKind::FileChange => ("File change approval", "File change"),
    }
}

#[derive(Default)]
struct ApprovalState {
    responding: Option<String>,
    more_open: bool,
}

/// The approval drawer: `ComposerBanner.Row layout="approval"` with the panel
/// and its actions.
#[derive(IntoElement)]
pub struct ComposerPendingApprovalBanner {
    pub approval: PendingApproval,
    pub pending_count: usize,
    pub source: Option<Arc<dyn TimelineSource>>,
}

impl RenderOnce for ComposerPendingApprovalBanner {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let state =
            window.use_keyed_state("composer-approval", cx, |_, _| ApprovalState::default());
        let approval = self.approval;
        let (fallback_label, _aria) = approval_labels(approval.request_kind);
        let monospace = approval.request_kind != ProviderRequestKind::McpElicitation;
        let detail = if approval.response_capability == ResponseCapability::NotResumable {
            "Provider process is gone — interrupt or restart the run to respond.".to_string()
        } else {
            approval
                .detail
                .clone()
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| fallback_label.to_string())
        };
        let panel = div()
            .flex()
            .min_w_0()
            .flex_1()
            .flex_col()
            .items_start()
            .gap(rems(0.25))
            .child(
                div()
                    .flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .gap(rems(0.5))
                    .text_size(TEXT_2XS.size)
                    .line_height(TEXT_2XS.line_height)
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.warning)
                            .child(fallback_label),
                    )
                    .when_some(approval.app_name.clone(), |this, app| {
                        this.child(div().min_w_0().truncate().child(app))
                    })
                    .when(self.pending_count > 1, |this| {
                        this.child(
                            div()
                                .ml_auto()
                                .flex_shrink_0()
                                .child(format!("1/{}", self.pending_count)),
                        )
                    }),
            )
            .child(
                div()
                    .id("approval-detail")
                    .w_full()
                    .min_w_0()
                    .max_h(rems(5.))
                    .overflow_scroll()
                    .text_size(TEXT_XS.size)
                    .line_height(TEXT_XS.line_height)
                    .text_color(theme.foreground)
                    .when(monospace, |this| {
                        this.font_family(theme.fonts.mono.clone())
                            .whitespace_nowrap()
                    })
                    .child(detail),
            );

        let options = approval
            .options
            .clone()
            .unwrap_or_else(default_approval_options);
        let can_respond = approval.response_capability == ResponseCapability::Live;
        let responding = state.read(cx).responding.as_deref() == Some(approval.request_id.as_str());
        let more_open = state.read(cx).more_open;
        let respond = {
            let source = self.source.clone();
            let request_id = approval.request_id.clone();
            let state = state.clone();
            move |decision: ProviderApprovalDecision, cx: &mut App| {
                state.update(cx, |state, cx| {
                    state.responding = Some(request_id.clone());
                    state.more_open = false;
                    cx.notify();
                });
                if let Some(source) = source.as_ref() {
                    source.respond_to_approval(&request_id, decision, cx);
                }
            }
        };
        let mut actions = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_end()
            .gap(rems(0.375))
            .self_center();
        let respond = Arc::new(respond);
        for (ix, option) in options
            .iter()
            .filter(|o| {
                matches!(
                    o.decision,
                    ProviderApprovalDecision::Decline | ProviderApprovalDecision::Accept
                )
            })
            .enumerate()
        {
            let decision = option.decision;
            let respond = respond.clone();
            let mut button = Button::new(("approval-option", ix))
                .size(ButtonSize::Xs)
                .variant(if decision == ProviderApprovalDecision::Accept {
                    ButtonVariant::Default
                } else {
                    ButtonVariant::Outline
                })
                .disabled(responding || !can_respond)
                .when(option.warning.is_some(), |this| {
                    this.child(Icon::new(IconName::TriangleAlert).size(rems(0.75)))
                })
                .child(
                    div()
                        .max_w(rems(10.))
                        .truncate()
                        .child(option.label.clone()),
                )
                .on_click(move |_, _, cx| respond(decision, cx));
            if let Some(warning) = option.warning.clone() {
                button = button.tooltip(Tooltip::new(warning));
            }
            actions = actions.child(button);
        }
        let more: Vec<ProviderApprovalOption> = options
            .iter()
            .filter(|o| {
                !matches!(
                    o.decision,
                    ProviderApprovalDecision::Decline | ProviderApprovalDecision::Accept
                )
            })
            .cloned()
            .collect();
        if !more.is_empty() {
            let toggle = state.clone();
            actions = actions.child(
                Button::new("approval-more")
                    .size(ButtonSize::IconXs)
                    .variant(ButtonVariant::Outline)
                    .pressed(more_open)
                    .disabled(responding)
                    .tooltip(Tooltip::new("More approval options"))
                    .child(Icon::new(IconName::Ellipsis).size(rems(1.)))
                    .on_click(move |_, _, cx| {
                        toggle.update(cx, |state, cx| {
                            state.more_open = !state.more_open;
                            cx.notify();
                        })
                    }),
            );
        }
        let mut body = div().flex().flex_col().gap(rems(0.75)).child(
            div()
                .flex()
                .w_full()
                .min_w_0()
                .items_start()
                .gap_x(rems(0.5))
                .child(
                    div()
                        .flex()
                        .w(rems(1.5))
                        .flex_none()
                        .justify_center()
                        .pt(rems(0.125))
                        .child(
                            Icon::new(IconName::Shield)
                                .size(rems(1.))
                                .color(theme.warning),
                        ),
                )
                .child(panel)
                .child(actions),
        );
        if more_open {
            let mut list = div().ml(rems(2.)).flex().flex_col().gap(rems(0.25));
            for (ix, option) in more.iter().enumerate() {
                let decision = option.decision;
                let respond = respond.clone();
                list = list.child(
                    div()
                        .id(("approval-more-option", ix))
                        .flex()
                        .items_center()
                        .gap(rems(0.5))
                        .rounded(RADIUS_MD)
                        .px(rems(0.5))
                        .py(rems(0.375))
                        .cursor_pointer()
                        .text_size(TEXT_SM.size)
                        .hover(|s| s.bg(theme.accent))
                        .when(option.warning.is_some(), |this| {
                            this.child(
                                Icon::new(IconName::TriangleAlert)
                                    .size(rems(0.75))
                                    .color(theme.warning),
                            )
                        })
                        .child(option.label.clone())
                        .on_click(move |_, _, cx| respond(decision, cx)),
                );
            }
            body = body.child(list);
        }
        banner_surface(
            BannerVariant::Warning,
            true,
            body.into_any_element(),
            window,
            &theme,
        )
    }
}

/// `PendingUserInputDraftAnswer` for one question.
#[derive(Clone, Default)]
struct DraftAnswer {
    selected: Vec<String>,
}

struct UserInputState {
    request_id: String,
    question_index: usize,
    answers: HashMap<String, DraftAnswer>,
    collapsed_question: Option<String>,
    responding: bool,
}

/// `ComposerPendingUserInputPanel` (the active prompt's card).
#[derive(IntoElement)]
pub struct ComposerPendingUserInputBanner {
    pub prompt: PendingUserInput,
    pub source: Option<Arc<dyn TimelineSource>>,
}

fn advance(
    state: &Entity<UserInputState>,
    prompt: &PendingUserInput,
    source: Option<Arc<dyn TimelineSource>>,
    cx: &mut App,
) {
    let last = prompt.questions.len().saturating_sub(1);
    let finished = state.update(cx, |state, cx| {
        let done = state.question_index >= last;
        if !done {
            state.question_index += 1;
        } else {
            state.responding = true;
        }
        cx.notify();
        done
    });
    if finished && let Some(source) = source {
        let answers = state
            .read(cx)
            .answers
            .iter()
            .map(|(id, answer)| (id.clone(), answer.selected.join(", ")))
            .collect();
        source.respond_to_user_input(&prompt.request_id, answers, cx);
    }
}

impl RenderOnce for ComposerPendingUserInputBanner {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let prompt = self.prompt;
        let state = window.use_keyed_state(
            ElementId::Name(format!("pending-input-{}", prompt.request_id).into()),
            cx,
            |_, _| UserInputState {
                request_id: prompt.request_id.clone(),
                question_index: 0,
                answers: HashMap::new(),
                collapsed_question: None,
                responding: false,
            },
        );
        let _ = &state.read(cx).request_id;
        let index = state
            .read(cx)
            .question_index
            .min(prompt.questions.len().saturating_sub(1));
        let Some(question) = prompt.questions.get(index).cloned() else {
            return div().into_any_element();
        };
        let collapsed = state.read(cx).collapsed_question.as_deref() == Some(question.id.as_str());
        let responding = state.read(cx).responding;
        let disabled = responding || prompt.response_capability == ResponseCapability::NotResumable;
        let selected: Vec<String> = state
            .read(cx)
            .answers
            .get(&question.id)
            .map(|a| a.selected.clone())
            .unwrap_or_default();

        let toggle_state = state.clone();
        let question_id: SharedString = question.id.clone().into();
        let header = div()
            .id("pending-input-toggle")
            .flex()
            .w_full()
            .min_w_0()
            .min_h(rems(1.5))
            .items_center()
            .gap_x(rems(0.25))
            .rounded(RADIUS_MD)
            .cursor_pointer()
            .child(div().w(rems(1.5)).flex_none())
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .flex_1()
                    .items_center()
                    .gap(rems(0.25))
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .child(question.header.clone()),
                    )
                    .when(collapsed, |this| {
                        this.child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_color(theme.secondary_label)
                                .child(question.question.clone()),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(rems(0.25))
                    .when(prompt.questions.len() > 1, |this| {
                        this.child(
                            div()
                                .text_size(TEXT_3XS.size)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.muted_foreground)
                                .child(format!("{}/{}", index + 1, prompt.questions.len())),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .size(rems(1.5))
                            .items_center()
                            .justify_center()
                            .text_color(theme.muted_foreground)
                            .child(
                                Icon::new(if collapsed {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronUp
                                })
                                .size(rems(0.875)),
                            ),
                    )
                    .when(prompt.dismissible, |this| {
                        let source = self.source.clone();
                        let request_id = prompt.request_id.clone();
                        this.child(
                            Button::new("pending-input-dismiss")
                                .variant(ButtonVariant::GhostMuted)
                                .size(ButtonSize::IconXs)
                                .disabled(responding)
                                .tooltip(Tooltip::new("Dismiss question without answering"))
                                .child(Icon::new(IconName::X).size(rems(0.875)))
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    if let Some(source) = source.as_ref() {
                                        source.dismiss_user_input(&request_id, cx);
                                    }
                                }),
                        )
                    }),
            )
            .on_click(move |_, _, cx| {
                let id = question_id.to_string();
                toggle_state.update(cx, |state, cx| {
                    state.collapsed_question =
                        if state.collapsed_question.as_deref() == Some(id.as_str()) {
                            None
                        } else {
                            Some(id)
                        };
                    cx.notify();
                });
            });

        let mut options = div().mt(rems(0.5)).flex().flex_col().gap(rems(0.125));
        for (ix, option) in question.options.iter().enumerate() {
            let value = option.value.clone().unwrap_or_else(|| option.label.clone());
            let is_selected = selected.contains(&value);
            let state = state.clone();
            let prompt_for_click = prompt.clone();
            let source = self.source.clone();
            let multi = question.multi_select;
            let qid = question.id.clone();
            options = options.child(
                div()
                    .id(("pending-option", ix))
                    .flex()
                    .w_full()
                    .items_center()
                    .gap(rems(0.5))
                    .rounded(RADIUS_MD)
                    .px(rems(0.625))
                    .py(rems(0.5))
                    .when(is_selected, |this| {
                        this.bg(with_alpha(theme.muted, 0.55))
                            .text_color(theme.foreground)
                    })
                    .when(!is_selected, |this| {
                        this.text_color(with_alpha(theme.foreground, 0.85))
                            .hover(|s| s.bg(with_alpha(theme.muted, 0.3)))
                    })
                    .when(disabled, |this| this.opacity(0.5))
                    .when(!disabled, |this| this.cursor_pointer())
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap(rems(0.125))
                            .child(
                                div()
                                    .text_size(TEXT_SM.size)
                                    .line_height(TEXT_SM.line_height)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(option.label.clone()),
                            )
                            .when(
                                !option.description.is_empty()
                                    && option.description != option.label,
                                |this| {
                                    this.child(
                                        div()
                                            .text_size(TEXT_2XS.size)
                                            .line_height(TEXT_2XS.line_height)
                                            .text_color(theme.secondary_label)
                                            .child(option.description.clone()),
                                    )
                                },
                            ),
                    )
                    .child(if is_selected {
                        Icon::new(IconName::Check)
                            .size(rems(0.875))
                            .color(theme.primary)
                            .into_any_element()
                    } else if ix < 9 {
                        div()
                            .flex()
                            .size(rems(1.25))
                            .flex_shrink_0()
                            .items_center()
                            .justify_center()
                            .text_size(TEXT_3XS.size)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .child((ix + 1).to_string())
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    })
                    .on_click(move |_, _, cx| {
                        if disabled {
                            return;
                        }
                        let value = value.clone();
                        state.update(cx, |state, cx| {
                            let answer = state.answers.entry(qid.clone()).or_default();
                            if multi {
                                if let Some(pos) = answer.selected.iter().position(|v| *v == value)
                                {
                                    answer.selected.remove(pos);
                                } else {
                                    answer.selected.push(value);
                                }
                            } else {
                                answer.selected = vec![value];
                            }
                            cx.notify();
                        });
                        if !multi {
                            // Single-select advances after a short beat (200ms).
                            let state = state.clone();
                            let prompt = prompt_for_click.clone();
                            let source = source.clone();
                            cx.spawn(async move |cx| {
                                cx.background_executor()
                                    .timer(Duration::from_millis(200))
                                    .await;
                                cx.update(|cx| advance(&state, &prompt, source, cx));
                            })
                            .detach();
                        }
                    }),
            );
        }

        let body = div()
            .id("pending-input-body")
            .max_h(rems(24.))
            .overflow_y_scroll()
            .child(
                div()
                    .pl(rems(1.75))
                    .pr(rems(0.25))
                    .pb(rems(0.25))
                    .child(ChatMarkdown::new(
                        ("pending-question", index),
                        question.question.clone(),
                    ))
                    .when(question.multi_select, |this| {
                        this.child(
                            div()
                                .mt(rems(0.25))
                                .text_size(TEXT_XS.size)
                                .text_color(theme.secondary_label)
                                .child("Select one or more options."),
                        )
                    })
                    .child(options),
            );
        let content = div()
            .flex()
            .flex_col()
            .child(header)
            .when(!collapsed, |this| this.child(body));
        banner_surface(
            BannerVariant::Default,
            false,
            content.into_any_element(),
            window,
            &theme,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_split_primary_and_more() {
        let options = default_approval_options();
        let primary = options
            .iter()
            .filter(|o| {
                matches!(
                    o.decision,
                    ProviderApprovalDecision::Decline | ProviderApprovalDecision::Accept
                )
            })
            .count();
        assert_eq!(primary, 2);
        assert_eq!(options.len() - primary, 2);
    }

    #[test]
    fn approval_labels_follow_request_kind() {
        assert_eq!(
            approval_labels(ProviderRequestKind::Command).0,
            "Command approval"
        );
        assert_eq!(
            approval_labels(ProviderRequestKind::FileRead).0,
            "File read approval"
        );
    }
}
