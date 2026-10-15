//! Port of TraitsPicker.tsx: the reasoning/speed trigger and its menu. The
//! decisions live in `composer::provider_state`; this paints them.

use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div, rems,
};

use crate::composer::provider_options::{
    OptionDescriptor, OptionSelection, OptionValue, SelectionContext,
};
use crate::composer::provider_state::{
    TraitsInput, TraitsSection, TraitsSelectOutcome, TraitsTriggerInput,
    build_traits_trigger_display, handle_traits_select_change, replace_descriptor_current_value,
    selections_after_pick, traits_section,
};
use crate::composer::view_model::ComposerViewModel;
use crate::composer::views::chat_composer::{ChatComposer, Layer};
use crate::composer::views::chrome::{
    ControlSize, composer_control, control_chevron, control_separator, floating, glass_surface,
    menu_row, track_bounds,
};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::TEXT_XS;
use crate::ui::{Icon, IconName, Tooltip};

impl ChatComposer {
    pub(super) fn traits_section_for(
        &self,
        vm: &ComposerViewModel,
        cx: &gpui_kit::App,
    ) -> Option<TraitsSection> {
        let entry = vm.selected_entry()?;
        let prompt = self.prompt(cx);
        let section = traits_section(&TraitsInput {
            provider: &entry.driver_kind,
            models: &entry.models,
            model: Some(&vm.selected_model),
            prompt: &prompt,
            model_options: &vm.selected_model_options,
            allow_prompt_injected_effort: entry.driver_kind == "claudeAgent",
            plan_mode_enabled: vm.settings.plan_mode_enabled,
        });
        section.has_any_controls.then_some(section)
    }

    pub(super) fn render_traits_trigger(
        &mut self,
        vm: &Arc<ComposerViewModel>,
        section: &TraitsSection,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let entry = vm.selected_entry();
        let driver = entry
            .map(|entry| entry.driver_kind.clone())
            .unwrap_or_default();
        let selection = SelectionContext {
            instance_id: &vm.selected_instance_id,
            model: &vm.selected_model,
            options: &vm.selected_model_options,
        };
        let reported = vm
            .reported_model_options
            .as_ref()
            .map(|options| SelectionContext {
                instance_id: &vm.selected_instance_id,
                model: &vm.selected_model,
                options,
            });
        let label = build_traits_trigger_display(&TraitsTriggerInput {
            provider: &driver,
            descriptors: &section.descriptors,
            primary_select_descriptor_id: section.primary_select_id.as_deref(),
            ultrathink_prompt_controlled: section.ultrathink_prompt_controlled,
            model_selection: Some(&selection),
            reported_model_selection: reported.as_ref(),
        });
        let open = self.layer == Some(Layer::Traits);
        let bounds = self.trigger_bounds.traits.clone();
        let tooltip = label.clone();
        let codex = driver == "codex";
        div()
            .flex()
            .items_center()
            .gap_1()
            .flex_shrink_0()
            .child(control_separator(ControlSize::Sm, cx))
            .child(
                composer_control("composer-traits", ControlSize::Sm, open, cx)
                    .when(codex, |this| {
                        this.min_w_0()
                            .max_w(rems(12.))
                            .flex_shrink_1()
                            .overflow_hidden()
                    })
                    .child(track_bounds(&bounds))
                    .child(Icon::new(IconName::Brain).color(theme.muted_foreground))
                    .child(div().min_w_0().truncate().child(label))
                    .child(control_chevron(ControlSize::Sm, cx))
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .on_click(
                        cx.listener(|this, _, _, cx| this.set_layer(Some(Layer::Traits), cx)),
                    ),
            )
            .into_any_element()
    }

    fn apply_traits_selections(
        &mut self,
        vm: &ComposerViewModel,
        selections: Option<Vec<OptionSelection>>,
        cx: &mut Context<Self>,
    ) {
        if let Some(source) = self.source(cx) {
            source.set_model_options(&vm.selected_instance_id, &vm.selected_model, selections, cx);
        }
    }

    pub(super) fn traits_menu(
        &mut self,
        vm: &Arc<ComposerViewModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let section = self.traits_section_for(vm, cx)?;
        let theme = cx.app_theme().clone();
        let _ = window;
        let mut groups: Vec<AnyElement> = Vec::new();
        let selects: Vec<&OptionDescriptor> = section
            .descriptors
            .iter()
            .filter(|d| d.is_select())
            .collect();
        let booleans: Vec<&OptionDescriptor> = section
            .descriptors
            .iter()
            .filter(|d| !d.is_select())
            .collect();
        let heading = |text: String| {
            div()
                .px_2()
                .pt(rems(0.375))
                .pb_1()
                .text_size(TEXT_XS.size)
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let divider = || div().my_1().h(gpui_kit::px(1.)).bg(theme.border);
        let mut counter = 0usize;
        for (index, descriptor) in selects.iter().enumerate() {
            let OptionDescriptor::Select {
                id,
                label,
                options,
                current_value,
                ..
            } = descriptor
            else {
                continue;
            };
            if index > 0 {
                groups.push(divider().into_any_element());
            }
            let is_primary = section.primary_select_id.as_deref() == Some(id.as_str());
            let selected = if section.ultrathink_prompt_controlled && is_primary {
                Some("ultrathink".to_owned())
            } else {
                current_value
                    .clone()
                    .or_else(|| options.iter().find(|o| o.is_default).map(|o| o.id.clone()))
            };
            let locked = section.ultrathink_in_body_text && is_primary;
            groups.push(heading(label.clone()).into_any_element());
            if locked {
                groups.push(
                    div()
                        .px_2()
                        .pb(rems(0.375))
                        .text_size(TEXT_XS.size)
                        .text_color(with_alpha(theme.muted_foreground, 0.8))
                        .child("Your prompt contains \"ultrathink\" in the text. Remove it to change this option.")
                        .into_any_element(),
                );
            }
            for option in options {
                counter += 1;
                let (descriptor_id, value) = (id.clone(), option.id.clone());
                let section_for_click = section.clone();
                let vm_for_click = vm.clone();
                groups.push(
                    menu_row(
                        ("traits-option", counter),
                        selected.as_deref() == Some(option.id.as_str()),
                        cx,
                    )
                    .when(locked, |this| this.opacity(0.64))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .w_full()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(rems(0.375))
                                    .child(option.label.clone())
                                    .when(option.is_default, |this| {
                                        this.child(
                                            div()
                                                .rounded(rems(0.25))
                                                .border_1()
                                                .border_color(theme.border)
                                                .px(rems(0.25))
                                                .text_size(rems(0.6875))
                                                .text_color(theme.muted_foreground)
                                                .child("Default"),
                                        )
                                    }),
                            )
                            .when_some(option.description.clone(), |this, description| {
                                this.child(
                                    div()
                                        .max_w(rems(14.))
                                        .text_size(TEXT_XS.size)
                                        .text_color(with_alpha(theme.muted_foreground, 0.8))
                                        .child(description),
                                )
                            }),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if locked {
                            return;
                        }
                        let prompt = this.prompt(cx);
                        match handle_traits_select_change(
                            &section_for_click,
                            &descriptor_id,
                            &value,
                            &prompt,
                        ) {
                            TraitsSelectOutcome::None => {}
                            TraitsSelectOutcome::PromptChange(next) => {
                                this.replace_prompt(&next, window, cx)
                            }
                            TraitsSelectOutcome::OptionsChange { selections, prompt } => {
                                if let Some(prompt) = prompt {
                                    this.replace_prompt(&prompt, window, cx);
                                }
                                this.apply_traits_selections(&vm_for_click, selections, cx);
                            }
                        }
                        this.close_layer(window, cx);
                    }))
                    .into_any_element(),
                );
            }
        }
        for (index, descriptor) in booleans.iter().enumerate() {
            let OptionDescriptor::Boolean {
                id,
                label,
                current_value,
                ..
            } = descriptor
            else {
                continue;
            };
            if index > 0 || !selects.is_empty() {
                groups.push(divider().into_any_element());
            }
            groups.push(heading(label.clone()).into_any_element());
            for on in [true, false] {
                counter += 1;
                let descriptor_id = id.clone();
                let descriptors = section.descriptors.clone();
                let vm_for_click = vm.clone();
                groups.push(
                    menu_row(("traits-option", counter), (*current_value == Some(true)) == on, cx)
                        .child(if on { "On" } else { "Off" })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let next = replace_descriptor_current_value(&descriptors, &descriptor_id, &OptionValue::Bool(on));
                            let selections = crate::composer::provider_options::build_provider_option_selections_from_descriptors(&next);
                            this.apply_traits_selections(&vm_for_click, selections, cx);
                            this.close_layer(window, cx);
                        }))
                        .into_any_element(),
                );
            }
        }
        let _ = selections_after_pick;
        let _ = CursorStyle::Arrow;
        Some(floating(
            self.trigger_bounds.traits.get(),
            true,
            false,
            glass_surface(cx)
                .min_w(rems(12.))
                .p_1()
                .flex()
                .flex_col()
                .children(groups),
        ))
    }
}
