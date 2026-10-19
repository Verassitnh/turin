//! Port of upstream/apps/web/src/components/settings/CustomModelEditor.tsx

use crate::theme::color::solid_over;
use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, rems,
};
use turin_contracts::model::ModelCapabilities;
use turin_shared::model::CustomModelDefinition;

use super::controls::{
    FieldSize, PopupAlign, SelectItem, SelectSize, SelectWidth, popup_list, select_trigger, switch,
    text_field, toggle_menu, trigger_height, with_popup,
};
use super::custom_model_editor_logic::{
    CustomModelDraft, DESCRIPTOR_PRESETS_BY_KIND, EditorChoice, EditorDescriptor,
    EditorDescriptorType, choice_from_preset, descriptor_from_preset,
    descriptors_from_capabilities, draft_from_definition, empty_editor_choice,
    empty_editor_descriptor,
};
use super::view::SettingsView;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_MD, TEXT_2XS, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant, IconName, Tooltip};

const CUSTOM_ID_VALUE: &str = "__custom__";

impl SettingsView {
    pub(super) fn begin_custom_model_edit(
        &mut self,
        entry: &CustomModelDefinition,
        cx: &mut Context<Self>,
    ) {
        self.panels.providers.editing_model = Some(entry.slug.clone());
        self.panels.providers.custom_model_draft = Some(draft_from_definition(entry));
        self.panels.providers.custom_model_editor_error = None;
        cx.notify();
    }

    pub(super) fn cancel_custom_model_edit(&mut self, cx: &mut Context<Self>) {
        self.panels.providers.editing_model = None;
        self.panels.providers.custom_model_draft = None;
        self.panels.providers.custom_model_editor_error = None;
        cx.notify();
    }

    pub(super) fn render_custom_model_editor(
        &mut self,
        instance_id: &str,
        row_id: &str,
        driver_kind: &str,
        built_in: &[turin_provider_core::server_provider::ServerProviderModel],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let draft = self.panels.providers.custom_model_draft.as_ref()?.clone();
        let theme = cx.app_theme().clone();
        let presets = DESCRIPTOR_PRESETS_BY_KIND
            .iter()
            .find(|(kind, _)| *kind == driver_kind)
            .map(|(_, presets)| *presets)
            .unwrap_or(&[]);
        let start_from: Vec<_> = built_in
            .iter()
            .filter(|model| {
                model
                    .capabilities
                    .as_ref()
                    .and_then(|c| c.option_descriptors.as_ref())
                    .map(|d| !d.is_empty())
                    .unwrap_or(false)
            })
            .collect();
        let prefix = format!("custom-editor-{instance_id}-{}", draft.slug);
        let name_input = self.input(
            &format!("{prefix}-name"),
            &draft.name,
            &draft.slug,
            window,
            cx,
            move |this, text, _, cx| {
                if let Some(draft) = this.panels.providers.custom_model_draft.as_mut() {
                    draft.name = text;
                }
                cx.notify();
            },
        );
        let mut body = div()
            .mx_2()
            .mt_1()
            .mb_2()
            .flex()
            .flex_col()
            .gap_3()
            .rounded(RADIUS_MD)
            .border_1()
            .border_color(theme.border)
            .bg(solid_over(theme.muted, 0.2, theme.background))
            .p_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(TEXT_XS.size)
                            .text_color(theme.muted_foreground)
                            .child("Display name"),
                    )
                    .child(div().w(rems(18.)).child(text_field(
                        &name_input,
                        None,
                        FieldSize::Sm,
                        false,
                        cx,
                    ))),
            )
            .child(self.custom_model_options_header(&prefix, &start_from, driver_kind, window, cx));
        if draft.descriptors.is_empty() {
            body = body.child(
                div()
                    .text_size(TEXT_XS.size)
                    .text_color(with_alpha(theme.muted_foreground, 0.7))
                    .child("No custom options. The composer uses the provider's default options."),
            );
        }
        for (index, descriptor) in draft.descriptors.iter().enumerate() {
            body = body.child(self.render_custom_model_descriptor(
                &prefix,
                index,
                descriptor,
                presets,
                driver_kind,
                window,
                cx,
            ));
        }
        body = body.child(self.custom_model_descriptor_actions(&prefix, presets, window, cx));
        if let Some(error) = self.panels.providers.custom_model_editor_error.clone() {
            body = body.child(
                div()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.destructive)
                    .child(error),
            );
        }
        let row_id = row_id.to_owned();
        body = body.child(
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new(SharedString::from(format!("{prefix}-save")))
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Sm)
                        .label("Save")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.save_custom_model_draft(&row_id, cx);
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("{prefix}-cancel")))
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Sm)
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.cancel_custom_model_edit(cx);
                        })),
                ),
        );
        Some(body.into_any_element())
    }

    fn custom_model_options_header(
        &mut self,
        prefix: &str,
        start_from: &[&turin_provider_core::server_provider::ServerProviderModel],
        driver_kind: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let theme = cx.app_theme().clone();
        div().flex().flex_col().gap_2().child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.muted_foreground)
                        .child("Options shown in the composer"),
                )
                .when(!start_from.is_empty(), |this| {
                    this.child(self.custom_model_copy_from_select(
                        prefix,
                        start_from,
                        driver_kind,
                        cx,
                    ))
                }),
        )
    }

    fn custom_model_copy_from_select(
        &mut self,
        prefix: &str,
        start_from: &[&turin_provider_core::server_provider::ServerProviderModel],
        driver_kind: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let menu_id = SharedString::from(format!("{prefix}-copy-from"));
        let open = self.menus.is_open(&menu_id);
        let trigger = select_trigger(
            SharedString::from(format!("{prefix}-copy-trigger")),
            "Copy from…",
            SelectSize::Compact,
            SelectWidth::Fixed(11.),
            open,
            false,
            cx,
        )
        .on_click(toggle_menu(cx, |this| &mut this.menus, menu_id.clone()));
        let popup = open.then(|| {
            let items: Vec<_> = start_from
                .iter()
                .map(|model| {
                    SelectItem::new(
                        SharedString::from(model.slug.to_string()),
                        SharedString::from(model.name.to_string()),
                    )
                })
                .collect();
            let driver = driver_kind.to_owned();
            let candidates: Vec<(String, Option<ModelCapabilities>)> = start_from
                .iter()
                .map(|model| (model.slug.to_string(), model.capabilities.clone()))
                .collect();
            let entity = cx.entity().downgrade();
            let on_pick = Rc::new(
                move |slug: SharedString, _: &mut Window, cx: &mut gpui_kit::App| {
                    let driver = driver.clone();
                    let candidates = candidates.clone();
                    entity
                        .update(cx, |this, cx| {
                            if let Some((_, capabilities)) =
                                candidates.iter().find(|(s, _)| s == slug.as_ref())
                            {
                                let descriptors = descriptors_from_capabilities(
                                    capabilities.as_ref(),
                                    Some(driver.as_str()),
                                );
                                if let Some(draft) =
                                    this.panels.providers.custom_model_draft.as_mut()
                                {
                                    draft.descriptors = descriptors;
                                }
                                this.panels.providers.custom_model_editor_error = None;
                                this.menus.close();
                            }
                            cx.notify();
                        })
                        .ok();
                },
            );
            popup_list(
                SharedString::from(format!("{prefix}-copy-popup")),
                items,
                Some(rems(11.).to_pixels(gpui_kit::px(16.))),
                cx,
                on_pick,
            )
            .into_any_element()
        });
        with_popup(
            trigger,
            popup,
            PopupAlign::End,
            trigger_height(SelectSize::Compact),
        )
        .into_any_element()
    }

    fn custom_model_descriptor_actions(
        &mut self,
        prefix: &str,
        presets: &[super::custom_model_editor_logic::DescriptorPreset],
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let draft = self
            .panels
            .providers
            .custom_model_draft
            .clone()
            .unwrap_or(CustomModelDraft {
                slug: String::new(),
                name: String::new(),
                descriptors: Vec::new(),
            });
        let mut row = div().flex().flex_wrap().gap_1();
        for preset in presets {
            if draft.descriptors.iter().any(|d| d.id == preset.id) {
                continue;
            }
            let preset = *preset;
            row = row.child(
                Button::new(SharedString::from(format!(
                    "{prefix}-add-preset-{}",
                    preset.id
                )))
                .variant(ButtonVariant::GhostMuted)
                .size(ButtonSize::Xs)
                .icon(IconName::Plus)
                .icon_size(rems(0.75))
                .label(preset.label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(draft) = this.panels.providers.custom_model_draft.as_mut() {
                        draft.descriptors.push(descriptor_from_preset(&preset));
                        this.panels.providers.custom_model_editor_error = None;
                    }
                    cx.notify();
                })),
            );
        }
        row.child(
            Button::new(SharedString::from(format!("{prefix}-add-custom-desc")))
                .variant(ButtonVariant::GhostMuted)
                .size(ButtonSize::Xs)
                .icon(IconName::Plus)
                .icon_size(rems(0.75))
                .label("Custom option")
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(draft) = this.panels.providers.custom_model_draft.as_mut() {
                        draft.descriptors.push(empty_editor_descriptor());
                        this.panels.providers.custom_model_editor_error = None;
                    }
                    cx.notify();
                })),
        )
    }

    fn render_custom_model_descriptor(
        &mut self,
        prefix: &str,
        index: usize,
        descriptor: &EditorDescriptor,
        presets: &[super::custom_model_editor_logic::DescriptorPreset],
        driver_kind: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let theme = cx.app_theme().clone();
        let desc_key = descriptor.key.clone();
        let id_select = if presets.iter().any(|p| p.id == descriptor.id) {
            descriptor.id.clone()
        } else {
            CUSTOM_ID_VALUE.to_owned()
        };
        let id_menu = SharedString::from(format!("{prefix}-desc-{desc_key}-id"));
        let open_id = self.menus.is_open(&id_menu);
        let id_trigger_label = if id_select == CUSTOM_ID_VALUE {
            "Custom…".to_owned()
        } else {
            descriptor.id.clone()
        };
        let id_trigger = select_trigger(
            SharedString::from(format!("{prefix}-desc-{desc_key}-id-trigger")),
            id_trigger_label,
            SelectSize::Compact,
            SelectWidth::Fixed(10.),
            open_id,
            presets.is_empty(),
            cx,
        )
        .on_click(toggle_menu(cx, |this| &mut this.menus, id_menu.clone()));
        let id_popup = open_id.then(|| {
            let mut items: Vec<_> = presets
                .iter()
                .map(|preset| {
                    SelectItem::new(
                        SharedString::from(preset.id),
                        format!("{} {}", preset.id, preset.label),
                    )
                    .selected(preset.id == descriptor.id)
                })
                .collect();
            items.push(
                SelectItem::new(SharedString::from(CUSTOM_ID_VALUE), "Custom…")
                    .selected(id_select == CUSTOM_ID_VALUE),
            );
            let id_popup_id = SharedString::from(format!("{prefix}-desc-{desc_key}-id-popup"));
            let desc_key_pick = desc_key.clone();
            let driver_kind = driver_kind.to_owned();
            let entity = cx.entity().downgrade();
            let on_pick = Rc::new(
                move |value: SharedString, _: &mut Window, cx: &mut gpui_kit::App| {
                    let driver_kind = driver_kind.clone();
                    entity
                        .update(cx, |this, cx| {
                            this.apply_custom_descriptor_preset(
                                &desc_key_pick,
                                value.as_ref(),
                                driver_kind.as_str(),
                            );
                            this.menus.close();
                            cx.notify();
                        })
                        .ok();
                },
            );
            popup_list(
                id_popup_id,
                items,
                Some(rems(10.).to_pixels(gpui_kit::px(16.))),
                cx,
                on_pick,
            )
            .into_any_element()
        });
        let type_menu = SharedString::from(format!("{prefix}-desc-{desc_key}-type"));
        let open_type = self.menus.is_open(&type_menu);
        let type_label = if descriptor.type_ == EditorDescriptorType::Boolean {
            "Toggle"
        } else {
            "Choices"
        };
        let type_trigger = select_trigger(
            SharedString::from(format!("{prefix}-desc-{desc_key}-type-trigger")),
            type_label,
            SelectSize::Compact,
            SelectWidth::Fixed(6.),
            open_type,
            false,
            cx,
        )
        .on_click(toggle_menu(cx, |this| &mut this.menus, type_menu.clone()));
        let type_popup = open_type.then(|| {
            let desc_key_pick = desc_key.clone();
            let type_popup_id = SharedString::from(format!("{prefix}-desc-{desc_key}-type-popup"));
            let entity = cx.entity().downgrade();
            let items = [
                SelectItem::new("select", "Choices")
                    .selected(descriptor.type_ == EditorDescriptorType::Select),
                SelectItem::new("boolean", "Toggle")
                    .selected(descriptor.type_ == EditorDescriptorType::Boolean),
            ];
            let on_pick = Rc::new(
                move |value: SharedString, _: &mut Window, cx: &mut gpui_kit::App| {
                    entity
                        .update(cx, |this, cx| {
                            let type_ = if value.as_ref() == "boolean" {
                                EditorDescriptorType::Boolean
                            } else {
                                EditorDescriptorType::Select
                            };
                            this.patch_custom_descriptor(&desc_key_pick, |d| d.type_ = type_);
                            this.menus.close();
                            cx.notify();
                        })
                        .ok();
                },
            );
            popup_list(type_popup_id, items.into(), None, cx, on_pick).into_any_element()
        });
        let label_input = self.input(
            &format!("{prefix}-desc-{desc_key}-label"),
            &descriptor.label,
            "Label",
            window,
            cx,
            {
                let desc_key = desc_key.clone();
                move |this, text, _, cx| {
                    this.patch_custom_descriptor(&desc_key, |d| d.label = text);
                    cx.notify();
                }
            },
        );
        let custom_id_input = (id_select == CUSTOM_ID_VALUE).then(|| {
            self.input(
                &format!("{prefix}-desc-{desc_key}-custom-id"),
                &descriptor.id,
                "optionId",
                window,
                cx,
                {
                    let desc_key = desc_key.clone();
                    move |this, text, _, cx| {
                        this.patch_custom_descriptor(&desc_key, |d| d.id = text);
                        cx.notify();
                    }
                },
            )
        });
        let mut card = div()
            .flex()
            .flex_col()
            .gap_2()
            .rounded(RADIUS_MD)
            .border_1()
            .border_color(with_alpha(theme.border, 0.6))
            .bg(solid_over(theme.background, 0.4, theme.background))
            .p_2()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(rems(3.5))
                            .flex_shrink_0()
                            .text_size(TEXT_2XS.size)
                            .text_color(theme.muted_foreground)
                            .child(format!("Option {}", index + 1)),
                    )
                    .when(!presets.is_empty(), |this| {
                        this.child(with_popup(
                            id_trigger,
                            id_popup,
                            PopupAlign::Start,
                            trigger_height(SelectSize::Compact),
                        ))
                    })
                    .children(custom_id_input.map(|state| {
                        div()
                            .w(rems(9.))
                            .child(text_field(&state, None, FieldSize::Sm, true, cx))
                    }))
                    .child(div().min_w_0().flex_1().child(text_field(
                        &label_input,
                        None,
                        FieldSize::Sm,
                        false,
                        cx,
                    )))
                    .child(with_popup(
                        type_trigger,
                        type_popup,
                        PopupAlign::End,
                        trigger_height(SelectSize::Compact),
                    ))
                    .child(
                        Button::new(SharedString::from(format!(
                            "{prefix}-desc-{desc_key}-remove"
                        )))
                        .variant(ButtonVariant::GhostMuted)
                        .size(ButtonSize::IconMicro)
                        .icon(IconName::X)
                        .icon_size(rems(0.75))
                        .tooltip(Tooltip::new(format!("Remove option {}", index + 1)))
                        .on_click({
                            let desc_key = desc_key.clone();
                            cx.listener(move |this, _, _, cx| {
                                if let Some(draft) =
                                    this.panels.providers.custom_model_draft.as_mut()
                                {
                                    draft.descriptors.retain(|d| d.key != desc_key);
                                    this.panels.providers.custom_model_editor_error = None;
                                }
                                cx.notify();
                            })
                        }),
                    ),
            );
        if descriptor.type_ == EditorDescriptorType::Select {
            for choice in &descriptor.choices {
                card = card
                    .child(self.render_custom_model_choice(prefix, &desc_key, choice, window, cx));
            }
            card = card.child(
                Button::new(SharedString::from(format!(
                    "{prefix}-desc-{desc_key}-add-choice"
                )))
                .variant(ButtonVariant::GhostMuted)
                .size(ButtonSize::Xs)
                .icon(IconName::Plus)
                .icon_size(rems(0.75))
                .label("Add choice")
                .on_click({
                    let desc_key = desc_key.clone();
                    cx.listener(move |this, _, _, cx| {
                        this.patch_custom_descriptor(&desc_key, |d| {
                            d.choices.push(empty_editor_choice());
                        });
                        cx.notify();
                    })
                }),
            );
        }
        card
    }

    fn render_custom_model_choice(
        &mut self,
        prefix: &str,
        desc_key: &str,
        choice: &EditorChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let theme = cx.app_theme().clone();
        let choice_key = choice.key.clone();
        let id_input = self.input(
            &format!("{prefix}-choice-{desc_key}-{choice_key}-id"),
            &choice.id,
            "value",
            window,
            cx,
            {
                let desc_key = desc_key.to_owned();
                let choice_key = choice_key.clone();
                move |this, text, _, cx| {
                    this.patch_custom_choice(&desc_key, &choice_key, |c| c.id = text);
                    cx.notify();
                }
            },
        );
        let label_input = self.input(
            &format!("{prefix}-choice-{desc_key}-{choice_key}-label"),
            &choice.label,
            "Label",
            window,
            cx,
            {
                let desc_key = desc_key.to_owned();
                let choice_key = choice_key.clone();
                move |this, text, _, cx| {
                    this.patch_custom_choice(&desc_key, &choice_key, |c| c.label = text);
                    cx.notify();
                }
            },
        );
        let desc_key_sw = desc_key.to_owned();
        let choice_key_sw = choice_key.clone();
        let entity = cx.entity();
        div()
            .flex()
            .items_center()
            .gap_2()
            .pl(rems(4.))
            .child(
                div()
                    .w(rems(7.))
                    .child(text_field(&id_input, None, FieldSize::Sm, true, cx)),
            )
            .child(div().min_w_0().flex_1().child(text_field(
                &label_input,
                None,
                FieldSize::Sm,
                false,
                cx,
            )))
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .text_size(TEXT_2XS.size)
                    .text_color(theme.muted_foreground)
                    .child(switch(
                        SharedString::from(format!(
                            "{prefix}-choice-{desc_key}-{choice_key}-default"
                        )),
                        choice.is_default,
                        false,
                        cx,
                        move |checked, _, cx| {
                            entity.update(cx, |this, cx| {
                                this.set_custom_choice_default(
                                    &desc_key_sw,
                                    &choice_key_sw,
                                    checked,
                                );
                                cx.notify();
                            });
                        },
                    ))
                    .child("Default"),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "{prefix}-choice-{desc_key}-{choice_key}-remove"
                )))
                .variant(ButtonVariant::GhostMuted)
                .size(ButtonSize::IconMicro)
                .icon(IconName::X)
                .icon_size(rems(0.75))
                .tooltip(Tooltip::new("Remove choice"))
                .on_click({
                    let desc_key = desc_key.to_owned();
                    let choice_key = choice_key.clone();
                    cx.listener(move |this, _, _, cx| {
                        this.patch_custom_descriptor(&desc_key, |d| {
                            d.choices.retain(|c| c.key != choice_key);
                        });
                        cx.notify();
                    })
                }),
            )
    }

    fn patch_custom_descriptor(&mut self, key: &str, patch: impl FnOnce(&mut EditorDescriptor)) {
        if let Some(draft) = self.panels.providers.custom_model_draft.as_mut()
            && let Some(descriptor) = draft.descriptors.iter_mut().find(|d| d.key == key)
        {
            patch(descriptor);
            self.panels.providers.custom_model_editor_error = None;
        }
    }

    fn patch_custom_choice(
        &mut self,
        desc_key: &str,
        choice_key: &str,
        patch: impl FnOnce(&mut EditorChoice),
    ) {
        if let Some(draft) = self.panels.providers.custom_model_draft.as_mut()
            && let Some(descriptor) = draft.descriptors.iter_mut().find(|d| d.key == desc_key)
            && let Some(choice) = descriptor.choices.iter_mut().find(|c| c.key == choice_key)
        {
            patch(choice);
            self.panels.providers.custom_model_editor_error = None;
        }
    }

    fn set_custom_choice_default(&mut self, desc_key: &str, choice_key: &str, checked: bool) {
        if let Some(draft) = self.panels.providers.custom_model_draft.as_mut()
            && let Some(descriptor) = draft.descriptors.iter_mut().find(|d| d.key == desc_key)
        {
            for choice in &mut descriptor.choices {
                if choice.key == choice_key {
                    choice.is_default = checked;
                } else if checked {
                    choice.is_default = false;
                }
            }
            self.panels.providers.custom_model_editor_error = None;
        }
    }

    fn apply_custom_descriptor_preset(&mut self, desc_key: &str, value: &str, driver_kind: &str) {
        if value == CUSTOM_ID_VALUE {
            self.patch_custom_descriptor(desc_key, |d| d.id.clear());
            return;
        }
        let presets = DESCRIPTOR_PRESETS_BY_KIND
            .iter()
            .find(|(kind, _)| *kind == driver_kind)
            .map(|(_, presets)| *presets)
            .unwrap_or(&[]);
        let Some(preset) = presets.iter().find(|p| p.id == value) else {
            return;
        };
        self.patch_custom_descriptor(desc_key, |d| {
            d.id = preset.id.to_owned();
            d.label = preset.label.to_owned();
            d.type_ = preset.type_;
            d.choices = preset.choices.iter().map(choice_from_preset).collect();
            d.current_boolean_value = None;
        });
    }
}
