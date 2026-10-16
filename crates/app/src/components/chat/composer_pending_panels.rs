//! Port of upstream/apps/web/src/components/chat/ComposerPendingApprovalPanel.tsx,
//! ComposerPendingApprovalActions.tsx, ComposerPendingUserInputPanel.tsx and
//! the parts of ComposerBanner.tsx they sit in (`Attachment`, `Root`, `Row`,
//! `Icon`, `Content`, `Actions`, `Body`): the drawer attached above the
//! composer while the agent waits on the user.
//!
//! The answer drafts are `pending_user_input.rs`'s (U3a) `PendingUserInputDraftAnswer`s, held in
//! element state (upstream keeps them in the composer draft store); the card reads its selection
//! and progress through `derive_pending_user_input_progress`. The approval "more options" menu
//! is an inline disclosure, and the 1–9 option shortcuts are not bound yet.

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
use super::pending_user_input::{
    PendingUserInputAnswer, PendingUserInputDraftAnswer, build_pending_user_input_answers,
    derive_pending_user_input_progress, toggle_pending_user_input_option_selection,
};
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
pub(crate) const DRAWER_INSET_REM: f32 = 1.375;
/// `--chat-composer-attachment-overlap: calc(1rem + 1px)`.
pub(crate) fn attachment_overlap(window: &Window) -> gpui_kit::Pixels {
    window.rem_size() + px(1.)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BannerVariant {
    Default,
    Error,
    Info,
    Success,
    Warning,
}

/// `ComposerBanner.Root` `density`: the block padding of the surface
/// (`--composer-banner-padding-block`).
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum BannerDensity {
    /// `px-1 py-1`.
    Default,
    /// `py-1.25`.
    Comfortable,
    /// `px-3 py-3`.
    Spacious,
}

/// `ComposerBanner.Attachment` + `Root`: a surface that tucks under the
/// composer's top edge by the overlap.
pub(crate) fn banner_surface(
    variant: BannerVariant,
    spacious: bool,
    child: AnyElement,
    window: &Window,
    theme: &AppTheme,
) -> AnyElement {
    let density = if spacious {
        BannerDensity::Spacious
    } else {
        BannerDensity::Default
    };
    banner_surface_with_density(variant, density, child, window, theme)
}

pub(crate) fn banner_surface_with_density(
    variant: BannerVariant,
    density: BannerDensity,
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
        BannerVariant::Error => (
            with_alpha(theme.error, 0.32).into(),
            Some(with_alpha(theme.error, 0.08).into()),
        ),
        BannerVariant::Default | BannerVariant::Info | BannerVariant::Success => (
            if dark {
                with_alpha(crate::theme::tokens::tw::WHITE, 0.05).into()
            } else {
                with_alpha(theme.foreground, 0.08).into()
            },
            None,
        ),
    };
    let overlap = attachment_overlap(window);
    let padding_block = match density {
        BannerDensity::Default => rems(0.25),
        BannerDensity::Comfortable => rems(0.3125),
        BannerDensity::Spacious => rems(0.75),
    };
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
                        .px(if density == BannerDensity::Spacious {
                            rems(0.75)
                        } else {
                            rems(0.25)
                        })
                        .pt(padding_block)
                        .pb(padding_block.to_pixels(window.rem_size()) + overlap)
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
        let respond = {
            let source = self.source.clone();
            let request_id = approval.request_id.clone();
            let state = state.clone();
            move |decision: ProviderApprovalDecision, cx: &mut App| {
                state.update(cx, |state, cx| {
                    state.responding = Some(request_id.clone());
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
        let respond = std::rc::Rc::new(respond);
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
            // `<Menu>`: the rest of the decisions, opening above the trigger and aligned to its end.
            let respond = respond.clone();
            let menu_options = more.clone();
            actions = actions.child(crate::pull_request::dropdown::dropdown(
                "approval-more-menu",
                gpui_kit::Anchor::BottomRight,
                Button::new("approval-more")
                    .size(ButtonSize::IconXs)
                    .variant(ButtonVariant::Outline)
                    .disabled(responding)
                    .tooltip(Tooltip::new("More approval options"))
                    .child(Icon::new(IconName::Ellipsis).size(rems(1.))),
                move |mut menu, _, _| {
                    for option in &menu_options {
                        let decision = option.decision;
                        let respond = respond.clone();
                        let mut item =
                            gpui_kit::component::menu::PopupMenuItem::new(option.label.clone())
                                .disabled(responding)
                                .on_click(move |_, _, cx| respond(decision, cx));
                        if option.warning.is_some() {
                            item = item.icon(IconName::TriangleAlert);
                        }
                        menu = menu.item(item);
                    }
                    menu
                },
            ));
        }
        let body = div().flex().flex_col().gap(rems(0.75)).child(
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
        banner_surface(
            BannerVariant::Warning,
            true,
            body.into_any_element(),
            window,
            &theme,
        )
    }
}

/// The timeline's question as the contract type U3a's draft logic reads.
fn contract_questions(
    questions: &[super::timeline::turn_item::UserInputQuestion],
) -> Vec<turin_contracts::provider_runtime::UserInputQuestion> {
    questions
        .iter()
        .filter_map(|question| {
            serde_json::from_value(serde_json::json!({
                "id": question.id,
                "header": question.header,
                "question": question.question,
                "options": question.options.iter().map(|option| serde_json::json!({
                    "label": option.label,
                    "description": option.description,
                    "value": option.value,
                })).collect::<Vec<_>>(),
                "allowCustomAnswer": question.allow_custom_answer,
                "initialAnswer": question.initial_answer,
                "multiSelect": question.multi_select,
            }))
            .ok()
        })
        .collect()
}

/// What the source takes for one answer: text as is, a choice list joined.
fn answer_text(answer: &PendingUserInputAnswer) -> String {
    match answer {
        PendingUserInputAnswer::Text(text) => text.clone(),
        PendingUserInputAnswer::Choices(choices) => choices.join(", "),
    }
}

struct UserInputState {
    request_id: String,
    question_index: usize,
    answers: HashMap<String, PendingUserInputDraftAnswer>,
    collapsed_question: Option<String>,
    responding: bool,
}

fn merge_question_attachment_counts(
    cx: &App,
    environment_id: Option<&str>,
    thread_id: Option<&str>,
    prompt: &PendingUserInput,
    mut answers: HashMap<String, PendingUserInputDraftAnswer>,
) -> HashMap<String, PendingUserInputDraftAnswer> {
    let (Some(environment_id), Some(thread_id)) = (environment_id, thread_id) else {
        return answers;
    };
    for question in &prompt.questions {
        let count = super::question_attachment_drafts::question_draft_attachment_count(
            cx,
            environment_id,
            thread_id,
            &prompt.request_id,
            &question.id,
        );
        if count == 0 {
            continue;
        }
        answers
            .entry(question.id.clone())
            .and_modify(|draft| draft.attachment_count = Some(count))
            .or_insert_with(|| PendingUserInputDraftAnswer {
                attachment_count: Some(count),
                ..Default::default()
            });
    }
    answers
}

/// `ComposerPendingUserInputPanel` (the active prompt's card).
#[derive(IntoElement)]
pub struct ComposerPendingUserInputBanner {
    pub prompt: PendingUserInput,
    pub source: Option<Arc<dyn TimelineSource>>,
    pub environment_id: Option<String>,
    pub thread_id: Option<String>,
}

/// `onAdvanceActivePendingUserInput`: the next question, or the answers on the last one.
fn advance(
    state: &Entity<UserInputState>,
    prompt: &PendingUserInput,
    source: Option<Arc<dyn TimelineSource>>,
    environment_id: Option<&str>,
    thread_id: Option<&str>,
    cx: &mut App,
) {
    if prompt.response_capability == ResponseCapability::NotResumable {
        return;
    }
    let questions = contract_questions(&prompt.questions);
    let (progress, answers) = {
        let state = state.read(cx);
        let merged = merge_question_attachment_counts(
            cx,
            environment_id,
            thread_id,
            prompt,
            state.answers.clone(),
        );
        (
            derive_pending_user_input_progress(&questions, &merged, state.question_index),
            build_pending_user_input_answers(&questions, &merged),
        )
    };
    if !progress.is_last_question {
        state.update(cx, |state, cx| {
            state.question_index = progress.question_index + 1;
            cx.notify();
        });
        return;
    }
    // Every question must resolve before the answers go out.
    let Some(answers) = answers else { return };
    state.update(cx, |state, cx| {
        state.responding = true;
        cx.notify();
    });
    if let Some(source) = source {
        let answers = questions
            .iter()
            .filter_map(|question| {
                answers
                    .get(question.id.as_str())
                    .map(|answer| (question.id.as_str().to_owned(), answer_text(answer)))
            })
            .collect();
        let ok = source.respond_to_user_input(&prompt.request_id, answers, cx);
        if !ok {
            state.update(cx, |state, cx| {
                state.responding = false;
                cx.notify();
            });
        }
    }
}

impl RenderOnce for ComposerPendingUserInputBanner {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let prompt = self.prompt.clone();

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
        let contract = contract_questions(&prompt.questions);
        let answers = {
            let base = state.read(cx).answers.clone();
            merge_question_attachment_counts(
                cx,
                self.environment_id.as_deref(),
                self.thread_id.as_deref(),
                &self.prompt,
                base,
            )
        };
        let progress = {
            let state = state.read(cx);
            derive_pending_user_input_progress(&contract, &answers, state.question_index)
        };
        let index = progress.question_index;
        let Some(question) = prompt.questions.get(index).cloned() else {
            return div().into_any_element();
        };
        let collapsed = state.read(cx).collapsed_question.as_deref() == Some(question.id.as_str());
        let responding = state.read(cx).responding;
        let disabled = responding || prompt.response_capability == ResponseCapability::NotResumable;
        // A typed custom answer outranks the options (`customAnswerActive`).
        let custom_answer_active = !progress.custom_answer.trim().is_empty();
        let selected: Vec<String> = if custom_answer_active {
            Vec::new()
        } else {
            progress.selected_option_values.clone()
        };

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
            let question_for_click = question.clone();
            let banner_environment_id = self.environment_id.clone();
            let banner_thread_id = self.thread_id.clone();
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
                        let Some(contract_question) =
                            contract_questions(std::slice::from_ref(&question_for_click))
                                .into_iter()
                                .next()
                        else {
                            return;
                        };
                        state.update(cx, |state, cx| {
                            let draft = toggle_pending_user_input_option_selection(
                                &contract_question,
                                state.answers.get(&qid),
                                &value,
                            );
                            state.answers.insert(qid.clone(), draft);
                            cx.notify();
                        });
                        if !multi {
                            // Single-select advances after a short beat (200ms).
                            let state = state.clone();
                            let prompt = prompt_for_click.clone();
                            let source = source.clone();
                            let env = banner_environment_id.clone();
                            let thread = banner_thread_id.clone();
                            cx.spawn(async move |cx| {
                                cx.background_executor()
                                    .timer(Duration::from_millis(200))
                                    .await;
                                cx.update(|cx| {
                                    advance(
                                        &state,
                                        &prompt,
                                        source,
                                        env.as_deref(),
                                        thread.as_deref(),
                                        cx,
                                    )
                                });
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
                    .when(self.environment_id.is_some() && self.thread_id.is_some(), |this| {
                        let attachments_enabled =
                            super::question_attachment_submit::pending_user_input_attachments_enabled(
                                &self.prompt,
                            );
                        let disabled_reason =
                            super::question_attachment_submit::pending_user_input_attachments_disabled_reason(
                                &self.prompt,
                            );
                        let env = self.environment_id.clone().unwrap_or_default();
                        let thread = self.thread_id.clone().unwrap_or_default();
                        let request = self.prompt.request_id.clone();
                        let qid = question.id.clone();
                        let count = super::question_attachment_drafts::question_draft_attachment_count(
                            cx,
                            &env,
                            &thread,
                            &request,
                            &qid,
                        );
                        this.child(
                            div()
                                .mt(rems(0.5))
                                .flex()
                                .items_center()
                                .gap(rems(0.5))
                                .child(
                                    Button::new(("pending-attach", index))
                                        .variant(ButtonVariant::GhostMuted)
                                        .size(ButtonSize::Xs)
                                        .disabled(!attachments_enabled || responding)
                                        .when_some(disabled_reason.map(str::to_owned), |button, reason| {
                                            button.tooltip(Tooltip::new(reason))
                                        })
                                        .child(Icon::new(IconName::Plus).size(rems(0.75)))
                                        .child("Attach files")
                                        .on_click(move |_, window, cx| {
                                            let key =
                                                crate::state::question_attachments::question_attachment_draft_id(
                                                    &env, &thread, &request, &qid,
                                                );
                                            super::question_attachment_drafts::pick_question_attachments(
                                                window, cx, key,
                                            );
                                        }),
                                )
                                .when(count > 0, |row| {
                                    row.child(
                                        div()
                                            .text_size(TEXT_XS.size)
                                            .text_color(theme.secondary_label)
                                            .child(format!("{count} attached")),
                                    )
                                }),
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

    fn question(multi_select: bool) -> super::super::timeline::turn_item::UserInputQuestion {
        use super::super::timeline::turn_item::{UserInputQuestion, UserInputQuestionOption};
        UserInputQuestion {
            id: "scope".into(),
            header: "Scope".into(),
            question: "Which parts?".into(),
            options: ["api", "ui"]
                .into_iter()
                .map(|label| UserInputQuestionOption {
                    label: label.into(),
                    description: String::new(),
                    value: None,
                })
                .collect(),
            multi_select,
            allow_custom_answer: None,
            initial_answer: None,
            required: None,
        }
    }

    #[test]
    fn the_card_reads_selection_through_the_u3a_drafts() {
        let questions = contract_questions(&[question(true)]);
        assert_eq!(questions.len(), 1);
        let mut answers = HashMap::new();
        for value in ["api", "ui", "api"] {
            let draft = toggle_pending_user_input_option_selection(
                &questions[0],
                answers.get("scope"),
                value,
            );
            answers.insert("scope".to_owned(), draft);
        }
        // Toggling "api" twice leaves "ui".
        let progress = derive_pending_user_input_progress(&questions, &answers, 0);
        assert_eq!(progress.selected_option_values, vec!["ui".to_owned()]);
        assert!(progress.is_last_question);
        assert!(progress.can_advance);
        assert_eq!(
            build_pending_user_input_answers(&questions, &answers)
                .unwrap()
                .get("scope")
                .map(answer_text)
                .as_deref(),
            Some("ui")
        );
    }

    #[test]
    fn nothing_is_sent_while_a_question_is_unanswered() {
        let questions = contract_questions(&[question(false)]);
        assert!(build_pending_user_input_answers(&questions, &HashMap::new()).is_none());
        let progress = derive_pending_user_input_progress(&questions, &HashMap::new(), 0);
        assert!(!progress.can_advance);
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
