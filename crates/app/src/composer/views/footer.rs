//! The composer footer: model trigger, mode controls, attach button and the
//! primary action (ChatComposer.tsx footer, ComposerFooterModeControls,
//! ComposerPrimaryActions.tsx).

use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, FontWeight, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, StatefulInteractiveElement as _, Styled as _, Window, div, px,
    rems,
};

use crate::assets::{SEND_ARROW, provider_icon_path};
use crate::composer::footer_layout::{
    should_use_compact_composer_footer, should_use_compact_composer_primary_actions,
};
use crate::composer::logic::ComposerSubmissionIntent;
use crate::composer::primary_actions::{
    PrimaryActions, SendButton, SendIcon, resolve_primary_actions,
};
use crate::composer::provider_model_picker::{
    ProviderModelTriggerInput, resolve_provider_model_trigger,
};
use crate::composer::view_model::{ComposerViewModel, InteractionMode, RuntimeMode, SessionPhase};
use crate::composer::views::chat_composer::{ChatComposer, Layer};
use crate::composer::views::chrome::{
    ControlSize, composer_control, control_chevron, control_separator, floating, glass_surface,
    menu_row, track_bounds,
};
use crate::composer::views::provider_model_picker::provider_model_trigger_children;
use crate::theme::ActiveAppTheme;
use crate::theme::color::{MixSpace, color_mix, with_alpha};
use crate::theme::tokens::{self, TEXT_SM, TEXT_XS, tw};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

/// `runtimeModeConfig`.
pub struct RuntimeModeOption {
    pub mode: RuntimeMode,
    pub label: &'static str,
    pub description: &'static str,
    pub icon: IconName,
}

pub const RUNTIME_MODES: [RuntimeModeOption; 4] = [
    RuntimeModeOption {
        mode: RuntimeMode::ApprovalRequired,
        label: "Supervised",
        description: "Ask before commands and file changes.",
        icon: IconName::Lock,
    },
    RuntimeModeOption {
        mode: RuntimeMode::AutoAcceptEdits,
        label: "Auto-accept edits",
        description: "Auto-approve edits, ask before other actions.",
        icon: IconName::PenLine,
    },
    RuntimeModeOption {
        mode: RuntimeMode::Auto,
        label: "Auto",
        description: "Supported providers approve routine actions; others still ask.",
        icon: IconName::Sparkles,
    },
    RuntimeModeOption {
        mode: RuntimeMode::FullAccess,
        label: "Full access",
        description: "Allow commands and edits without prompts.",
        icon: IconName::LockOpen,
    },
];

pub fn runtime_mode_option(mode: RuntimeMode) -> &'static RuntimeModeOption {
    RUNTIME_MODES
        .iter()
        .find(|option| option.mode == mode)
        .unwrap_or(&RUNTIME_MODES[0])
}

impl ChatComposer {
    pub(super) fn render_footer(
        &mut self,
        vm: &Arc<ComposerViewModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let width = f32::from(self.trigger_bounds.body.get().size.width);
        let measured = (width > 0.).then_some(width);
        let _ = should_use_compact_composer_footer(measured, false);
        let compact_actions = should_use_compact_composer_primary_actions(measured, false);

        let left = self.render_left_controls(vm, window, cx);
        let right = self.render_right_controls(vm, compact_actions, cx);
        let _ = &theme;
        div()
            .flex()
            .min_w_0()
            .items_center()
            .justify_between()
            .gap_2()
            .px_4()
            .pb_4()
            .child(left)
            .child(right)
            .into_any_element()
    }

    fn render_left_controls(
        &mut self,
        vm: &Arc<ComposerViewModel>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if vm.no_provider_available() {
            return self.render_provider_unavailable(vm, cx);
        }
        let model = self.render_model_trigger(vm, cx);
        let traits = self
            .traits_section_for(vm, cx)
            .map(|section| self.render_traits_trigger(vm, &section, cx));
        let mode = self.render_mode_controls(vm, cx);
        div()
            .flex()
            .min_w_0()
            .flex_1()
            .items_center()
            .gap_1()
            .child(model)
            .children(traits)
            .child(mode)
            .into_any_element()
    }

    fn render_provider_unavailable(
        &mut self,
        vm: &Arc<ComposerViewModel>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let setup = vm
            .selected_entry()
            .filter(|entry| entry.has_setup())
            .map(|entry| entry.instance_id.clone());
        let label = if setup.is_some() {
            "Open provider settings"
        } else {
            "No provider available"
        };
        let theme = cx.app_theme().clone();
        composer_control("provider-unavailable", ControlSize::Sm, false, cx)
            .when(setup.is_none(), |this| this.opacity(0.64))
            .child(Icon::new(IconName::CircleAlert).color(theme.muted_foreground))
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                if let (Some(instance), Some(source)) = (&setup, this.source(cx)) {
                    source.open_provider_setup(instance, cx);
                }
            }))
            .into_any_element()
    }

    fn render_model_trigger(
        &mut self,
        vm: &Arc<ComposerViewModel>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let options_for = |instance_id: &str| -> &[crate::composer::provider_state::ProviderModel] {
            vm.providers
                .iter()
                .find(|entry| entry.instance_id == instance_id)
                .map_or(&[][..], |entry| entry.models.as_slice())
        };
        let trigger = resolve_provider_model_trigger(&ProviderModelTriggerInput {
            active_instance_id: &vm.selected_instance_id,
            model: &vm.selected_model,
            instance_entries: &vm.providers,
            options_for: &options_for,
            selected_models: None,
            trigger_label: None,
            shortcut_label: vm
                .shortcut_labels
                .get("modelPicker.toggle")
                .map(String::as_str),
        });
        let open = self.layer == Some(Layer::ModelPicker);
        let disabled = vm.is_send_busy || !vm.provider_catalog_known;
        let bounds = self.trigger_bounds.model.clone();
        composer_control("composer-model-picker", ControlSize::Sm, open, cx)
            // `-ms-2.5 min-w-13 shrink justify-between`: the control's padding
            // lines the icon up with the text above.
            .ml(rems(-0.625))
            .min_w(rems(3.25))
            .justify_between()
            .when(disabled, |this| this.opacity(0.64))
            .child(track_bounds(&bounds))
            .children(provider_model_trigger_children(
                &trigger,
                ControlSize::Sm,
                true,
                cx,
            ))
            .on_click(cx.listener(move |this, _, window, cx| {
                if disabled {
                    return;
                }
                this.open_model_picker(window, cx);
            }))
            .into_any_element()
    }

    fn render_mode_controls(
        &mut self,
        vm: &Arc<ComposerViewModel>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let option = runtime_mode_option(vm.runtime_mode);
        let plan = vm.interaction_mode == InteractionMode::Plan;
        let open = self.layer == Some(Layer::RuntimeMode);
        let runtime_bounds = self.trigger_bounds.runtime.clone();
        let plan_tooltip = if plan {
            "Plan mode — click to return to normal build mode"
        } else {
            "Default mode — click to enter plan mode"
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .flex_shrink_0()
            .child(control_separator(ControlSize::Sm, cx))
            .child(
                composer_control("composer-runtime-mode", ControlSize::Sm, open, cx)
                    .child(track_bounds(&runtime_bounds))
                    .child(Icon::new(option.icon).color(theme.muted_foreground))
                    .child(option.label)
                    .child(control_chevron(ControlSize::Sm, cx))
                    .tooltip(move |window, cx| Tooltip::new(option.description).build(window, cx))
                    .on_click(
                        cx.listener(|this, _, _, cx| this.set_layer(Some(Layer::RuntimeMode), cx)),
                    ),
            )
            .when(vm.settings.plan_mode_enabled, |this| {
                this.child(control_separator(ControlSize::Sm, cx)).child(
                    composer_control("composer-interaction-mode", ControlSize::Sm, plan, cx)
                        .child(
                            Icon::new(if plan {
                                IconName::PencilRuler
                            } else {
                                IconName::Bot
                            })
                            .size(if plan { rems(1.) } else { rems(1.125) })
                            .color(if plan {
                                theme.accent_foreground
                            } else {
                                theme.muted_foreground
                            }),
                        )
                        .child(if plan { "Plan" } else { "Build" })
                        .tooltip(move |window, cx| Tooltip::new(plan_tooltip).build(window, cx))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_interaction_mode(cx))),
                )
            })
            .into_any_element()
    }

    fn render_right_controls(
        &mut self,
        vm: &Arc<ComposerViewModel>,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let attach = Button::new("composer-attach")
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::IconSm)
            .icon(IconName::Paperclip)
            .tooltip(Tooltip::new("Attach files"))
            .on_click(cx.listener(|this, _, window, cx| this.pick_attachments(window, cx)));
        let actions = self.render_primary_actions(vm, compact, cx);
        let meter = self.render_context_meter(vm, cx);
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_end()
            .gap(if compact { rems(0.375) } else { rems(0.5) })
            .children(meter)
            .child(attach)
            .child(actions)
            .into_any_element()
    }

    fn render_primary_actions(
        &mut self,
        vm: &Arc<ComposerViewModel>,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let input = self.primary_actions_input(vm, compact, cx);
        match resolve_primary_actions(&input) {
            PrimaryActions::Pending {
                show_stop,
                show_previous,
                previous_disabled,
                submit_label,
                submit_disabled,
            } => {
                let stop = show_stop.then(|| self.stop_button(true, !vm.can_operate_thread, cx));
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(if compact { rems(0.375) } else { rems(0.5) })
                    .children(stop)
                    .when(show_previous, |this| {
                        this.child(
                            Button::new("composer-previous-question")
                                .variant(ButtonVariant::Outline)
                                .size(if compact {
                                    ButtonSize::IconSm
                                } else {
                                    ButtonSize::Sm
                                })
                                .disabled(previous_disabled)
                                .when(compact, |b| b.icon(IconName::ChevronLeft))
                                .when(!compact, |b| b.label("Previous"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(source) = this.source(cx) {
                                        source.previous_pending_question(cx);
                                    }
                                })),
                        )
                    })
                    .child(self.pill_button(
                        "composer-submit-answer",
                        submit_label,
                        submit_disabled,
                        compact,
                        28.,
                        cx,
                    ))
                    .into_any_element()
            }
            PrimaryActions::Refine { label, disabled } => self
                .pill_button("composer-refine", label, disabled, compact, 32., cx)
                .into_any_element(),
            PrimaryActions::Implement { label, disabled } => self
                .pill_button("composer-implement", label, disabled, compact, 32., cx)
                .into_any_element(),
            PrimaryActions::Stop => self
                .stop_button(false, !vm.can_operate_thread, cx)
                .into_any_element(),
            PrimaryActions::Send(button) => self.send_button(&button, vm, cx),
        }
    }

    fn stop_button(
        &mut self,
        inside_pending: bool,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let size = if inside_pending { px(28.) } else { px(32.) };
        div()
            .id("composer-stop")
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(size)
            .rounded_full()
            .bg(with_alpha(theme.destructive, 0.9))
            .text_color(tw::WHITE)
            .shadow(tokens::shadow_xs(0.05))
            .cursor(CursorStyle::PointingHand)
            .hover(|style| style.bg(theme.destructive))
            .when(disabled, |this| this.opacity(0.64))
            .child(
                // `<rect x=2 y=2 width=8 height=8 rx=1.5>` in a 12px box.
                div().size(px(8.)).rounded(px(1.5)).bg(tw::WHITE),
            )
            .tooltip(|window, cx| Tooltip::new("Interrupt").build(window, cx))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !disabled {
                    this.interrupt(cx);
                }
            }))
            .into_any_element()
    }

    /// `messageActionPillClassName` at desktop width.
    fn pill_button(
        &mut self,
        id: &'static str,
        label: &'static str,
        disabled: bool,
        compact: bool,
        height: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        div()
            .id(id)
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .gap(rems(0.375))
            .h(px(height))
            .px(if compact { rems(0.75) } else { rems(1.) })
            .rounded_full()
            .whitespace_nowrap()
            .bg(theme.message_action)
            .text_color(theme.message_action_foreground)
            .text_size(TEXT_SM.size)
            .font_weight(FontWeight::MEDIUM)
            .shadow(tokens::shadow_xs(0.05))
            .when(!disabled, |this| {
                this.cursor(CursorStyle::PointingHand)
                    .hover(|style| style.bg(theme.message_action_hover))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.submit(ComposerSubmissionIntent::Foreground, window, cx)
                    }))
            })
            .when(disabled, |this| this.opacity(0.64))
            .child(label)
    }

    fn send_button(
        &mut self,
        button: &SendButton,
        vm: &Arc<ComposerViewModel>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let disabled = button.disabled;
        let resume = button.is_resume;
        let tooltip = button.tooltip.clone();
        let _ = vm;
        let dark = theme.is_dark();
        let glow = color_mix(
            MixSpace::Srgb,
            theme.message_action,
            0.24,
            with_alpha(theme.message_action, 0.),
        );
        let icon: AnyElement = match button.icon {
            SendIcon::Spinner => Icon::new(IconName::LoaderCircle)
                .color(theme.message_action_foreground)
                .into_any_element(),
            SendIcon::Resume => Icon::new(IconName::Play)
                .color(theme.message_action_foreground)
                .into_any_element(),
            SendIcon::Check => Icon::new(IconName::Check)
                .color(theme.message_action_foreground)
                .into_any_element(),
            SendIcon::Queue => Icon::new(IconName::ListPlus)
                .color(theme.message_action_foreground)
                .into_any_element(),
            SendIcon::Steer => Icon::new(IconName::CornerUpRight)
                .color(theme.message_action_foreground)
                .into_any_element(),
            SendIcon::ArrowUp => gpui_kit::svg()
                .path(SEND_ARROW)
                .size(px(14.))
                .text_color(theme.message_action_foreground)
                .into_any_element(),
        };
        let _ = dark;
        div()
            .id("composer-send")
            .flex()
            .flex_shrink_0()
            .size_8()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(theme.message_action)
            .when(!disabled, |this| {
                this.cursor(CursorStyle::PointingHand)
                    .shadow(vec![gpui_kit::BoxShadow {
                        color: glow.into(),
                        offset: gpui_kit::point(px(0.), px(1.)),
                        blur_radius: px(2.),
                        spread_radius: px(0.),
                        inset: false,
                    }])
                    .hover(|style| style.bg(theme.message_action_hover))
                    .on_click(
                        cx.listener(move |this, event: &gpui_kit::ClickEvent, window, cx| {
                            if resume {
                                if let Some(source) = this.source(cx) {
                                    source.resume(cx);
                                }
                                return;
                            }
                            let intent =
                                if event.modifiers().secondary() || event.modifiers().control {
                                    ComposerSubmissionIntent::Alternate
                                } else {
                                    ComposerSubmissionIntent::Foreground
                                };
                            this.submit(intent, window, cx);
                        }),
                    )
            })
            .when(disabled, |this| this.opacity(0.64))
            .child(icon)
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .into_any_element()
    }

    pub(super) fn render_layers(
        &mut self,
        vm: &Arc<ComposerViewModel>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut layers = Vec::new();
        match self.layer {
            Some(Layer::ModelPicker) => {
                if let Some(picker) = self.model_picker.clone() {
                    layers.push(floating(
                        self.trigger_bounds.model.get(),
                        true,
                        false,
                        glass_surface(cx).child(picker),
                    ));
                }
            }
            Some(Layer::RuntimeMode) => layers.push(self.runtime_mode_menu(vm, cx)),
            Some(Layer::CodeLanguage) => {
                if let Some((picker, anchor)) = self.language_picker.clone() {
                    let bounds = gpui_kit::Bounds::new(anchor, gpui_kit::size(px(0.), px(0.)));
                    layers.push(floating(
                        bounds,
                        false,
                        false,
                        glass_surface(cx).child(picker),
                    ));
                }
            }
            Some(Layer::ContextMeter) => layers.extend(self.context_meter_popover(vm, cx)),
            Some(Layer::Traits) => layers.extend(self.traits_menu(vm, _window, cx)),
            _ => {}
        }
        if self.layer.is_some() {
            // Outside clicks dismiss the open layer.
            layers.insert(
                0,
                div()
                    .id("composer-layer-backdrop")
                    .absolute()
                    .top(px(-4000.))
                    .left(px(-4000.))
                    .w(px(8000.))
                    .h(px(8000.))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| this.close_layer(window, cx)),
                    )
                    .into_any_element(),
            );
        }
        layers
    }

    fn runtime_mode_menu(
        &mut self,
        vm: &Arc<ComposerViewModel>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let current = vm.runtime_mode;
        let rows = RUNTIME_MODES.iter().enumerate().map(|(index, option)| {
            let mode = option.mode;
            menu_row(("runtime-mode", index), mode == current, cx)
                .min_w(rems(16.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .flex_1()
                        .gap(px(2.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(rems(0.375))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.foreground)
                                .child(
                                    Icon::new(option.icon)
                                        .size(rems(0.875))
                                        .color(theme.muted_foreground),
                                )
                                .child(option.label),
                        )
                        .child(
                            div()
                                .text_size(TEXT_XS.size)
                                .line_height(rems(1.))
                                .text_color(theme.muted_foreground)
                                .child(option.description),
                        ),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(source) = this.source(cx) {
                        source.set_runtime_mode(mode, cx);
                    }
                    this.close_layer(window, cx);
                }))
        });
        floating(
            self.trigger_bounds.runtime.get(),
            true,
            false,
            glass_surface(cx).p_1().flex().flex_col().children(rows),
        )
    }
}

/// A provider's mark at `size`: the bundled brand glyph, else a generic bot.
pub fn provider_mark(
    driver_kind: &str,
    size: gpui_kit::Rems,
    color: gpui_kit::Hsla,
) -> impl IntoElement {
    match provider_icon_path(driver_kind) {
        Some(path) => Icon::path(path).size(size).color(color),
        None => Icon::new(IconName::Bot).size(size).color(color),
    }
}

#[allow(dead_code)]
fn _phase(_: SessionPhase) {}
