//! Model and traits pickers for the settings rows (`ProviderModelPicker` /
//! `TraitsPicker` as used by ProjectDefaultsSettings and the text-generation
//! row).
//!
//! These are lean stand-ins for the composer's pickers (T17's
//! `composer::views::model_picker`, which owns the full favorites / search /
//! legacy-section UI); T22 swaps them for it. They list the enabled provider
//! instances' models and the selected model's option descriptors.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px, rems,
};
use serde_json::{Value, json};
use turin_contracts::model::ProviderOptionDescriptor;
use turin_provider_core::server_provider::{ServerProvider, ServerProviderState};

use super::controls::{PopupAlign, SelectItem, popup_list, with_popup};
use super::provider_meta::instance_label;
use super::view::{SettingsView, Which};
use crate::theme::tokens::{RADIUS_MD, TEXT_SM};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Icon, IconName};

/// A selectable model of one provider instance.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelOption {
    pub instance_id: String,
    pub instance_label: String,
    pub driver: String,
    pub slug: String,
    pub name: String,
    pub descriptors: Vec<ProviderOptionDescriptor>,
}

pub fn provider_is_selectable(provider: &ServerProvider) -> bool {
    provider.enabled
        && provider.installed
        && provider.is_available()
        && provider.status != ServerProviderState::Disabled
}

pub fn model_options(providers: &[ServerProvider]) -> Vec<ModelOption> {
    providers
        .iter()
        .filter(|provider| provider_is_selectable(provider))
        .flat_map(|provider| {
            provider.models.iter().map(move |model| ModelOption {
                instance_id: provider.instance_id.as_str().to_owned(),
                instance_label: instance_label(provider),
                driver: provider.driver.as_str().to_owned(),
                slug: model.slug.as_str().to_owned(),
                name: model.name.as_str().to_owned(),
                descriptors: model
                    .capabilities
                    .as_ref()
                    .and_then(|capabilities| capabilities.option_descriptors.clone())
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// The selection a null `defaultModelSelection` resolves to
/// (`resolveDefaultProviderModelSelection`): the first available provider's
/// default (else first) model.
pub fn resolve_default_selection(providers: &[ServerProvider]) -> Option<(String, String)> {
    providers
        .iter()
        .filter(|provider| provider_is_selectable(provider))
        .find_map(|provider| {
            let model = provider
                .models
                .iter()
                .find(|model| model.is_default == Some(true))
                .or_else(|| provider.models.first())?;
            Some((
                provider.instance_id.as_str().to_owned(),
                model.slug.as_str().to_owned(),
            ))
        })
}

fn selection_fields(selection: &Value) -> Option<(String, String)> {
    Some((
        selection.get("instanceId")?.as_str()?.to_owned(),
        selection.get("model")?.as_str()?.to_owned(),
    ))
}

/// `ComposerControl size="sm"` trigger look, in settings rows.
fn picker_trigger(
    id: impl Into<ElementId>,
    label: SharedString,
    icon: Option<IconName>,
    open: bool,
    theme: &AppTheme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .relative()
        .flex()
        .h(rems(1.75))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .gap(rems(0.375))
        .px(rems(0.625))
        .rounded(RADIUS_MD)
        .border_1()
        .border_color(gpui_kit::transparent_black())
        .text_size(TEXT_SM.size)
        .font_weight(FontWeight::MEDIUM)
        .cursor(CursorStyle::PointingHand)
        .when(open, |this| {
            this.bg(theme.accent).text_color(theme.accent_foreground)
        })
        .when(!open, |this| {
            this.text_color(theme.secondary_label)
                .hover(|style| style.bg(theme.accent).text_color(theme.foreground))
        })
        .when_some(icon, |this, icon| {
            this.child(Icon::new(icon).size(rems(1.)).color(theme.icon_muted))
        })
        .child(div().min_w_0().truncate().child(label))
        .child(
            div().mx(px(-2.)).child(
                Icon::new(IconName::ChevronDown)
                    .size(rems(0.875))
                    .color(theme.icon_muted),
            ),
        )
}

impl SettingsView {
    /// `ProviderModelPicker` + `TraitsPicker` for a model selection stored as
    /// `{instanceId, model, options?}` under `key`.
    pub(super) fn model_selection_control(
        &mut self,
        cx: &mut Context<Self>,
        menu_id: &'static str,
        providers: &[ServerProvider],
        selection: &Value,
        mixed: bool,
        on_change: impl Fn(&mut Self, Value, &mut Context<Self>) + Clone + 'static,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let options = model_options(providers);
        let current = selection_fields(selection).or_else(|| resolve_default_selection(providers));
        let Some((instance_id, model)) = current else {
            return div()
                .text_size(TEXT_SM.size)
                .text_color(theme.muted_foreground)
                .child("No providers available")
                .into_any_element();
        };
        let active = options
            .iter()
            .find(|option| option.instance_id == instance_id && option.slug == model);
        // `triggerLabel: "Mixed"` when the targets disagree.
        let label: SharedString = if mixed {
            "Mixed".into()
        } else {
            active
                .map(|option| option.name.clone())
                .unwrap_or_else(|| model.clone())
                .into()
        };
        let open = self.menus.is_open(menu_id);

        let trigger = picker_trigger(
            SharedString::from(format!("model-picker-{menu_id}")),
            label,
            Some(IconName::Bot),
            open,
            &theme,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.menus.toggle(menu_id);
            cx.notify();
        }));

        let popup = open.then(|| {
            let mut items: Vec<SelectItem> = Vec::new();
            let mut picks: Vec<(String, String)> = Vec::new();
            let mut last_instance = String::new();
            for option in &options {
                if option.instance_id != last_instance {
                    last_instance = option.instance_id.clone();
                    items.push(
                        SelectItem::new(
                            format!("group-{}", option.instance_id),
                            div()
                                .text_size(crate::theme::tokens::TEXT_XS.size)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.muted_foreground)
                                .child(option.instance_label.clone()),
                        )
                        .disabled(true),
                    );
                    picks.push((String::new(), String::new()));
                }
                items.push(
                    SelectItem::new(
                        format!("{}:{}", option.instance_id, option.slug),
                        div().pl_2().child(option.name.clone()),
                    )
                    .selected(option.instance_id == instance_id && option.slug == model),
                );
                picks.push((option.instance_id.clone(), option.slug.clone()));
            }
            let entity = cx.entity();
            let on_change = on_change.clone();
            let pick: std::rc::Rc<dyn Fn(SharedString, &mut gpui_kit::Window, &mut gpui_kit::App)> =
                std::rc::Rc::new(move |item_id, _, cx| {
                    let Some((instance, slug)) = item_id.split_once(':') else {
                        return;
                    };
                    let value = json!({ "instanceId": instance, "model": slug });
                    entity.update(cx, |this, cx| {
                        this.menus.close();
                        on_change(this, value, cx);
                        cx.notify();
                    });
                });
            popup_list(
                SharedString::from(format!("popup-{menu_id}")),
                items,
                Some(rems(14.).to_pixels(px(16.))),
                cx,
                pick,
            )
            .into_any_element()
        });
        let picker = with_popup(
            trigger,
            popup,
            PopupAlign::End,
            rems(1.75).to_pixels(px(16.)),
        );

        // TraitsPicker: the selected model's option descriptors (hidden when mixed).
        let traits = active.filter(|_| !mixed).and_then(|option| {
            if option.descriptors.is_empty() {
                return None;
            }
            let selected_options: Vec<Value> = selection
                .get("options")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let summary = traits_summary(&option.descriptors, &selected_options);
            let traits_menu: &'static str = Box::leak(format!("{menu_id}-traits").into_boxed_str());
            let traits_open = self.menus.is_open(traits_menu);
            let trigger = picker_trigger(
                SharedString::from(format!("traits-picker-{menu_id}")),
                summary.into(),
                None,
                traits_open,
                &theme,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.menus.toggle(traits_menu);
                cx.notify();
            }));
            let popup = traits_open.then(|| {
                let mut items: Vec<SelectItem> = Vec::new();
                for descriptor in &option.descriptors {
                    match descriptor {
                        ProviderOptionDescriptor::Select(select) => {
                            items.push(
                                SelectItem::new(
                                    format!("group-{}", select.id.as_str()),
                                    div()
                                        .text_size(crate::theme::tokens::TEXT_XS.size)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.muted_foreground)
                                        .child(select.label.as_str().to_owned()),
                                )
                                .disabled(true),
                            );
                            let current = selected_value(&selected_options, select.id.as_str())
                                .and_then(|value| value.as_str().map(str::to_owned))
                                .or_else(|| {
                                    select.current_value.as_ref().map(|v| v.as_str().to_owned())
                                })
                                .or_else(|| {
                                    select
                                        .options
                                        .iter()
                                        .find(|choice| choice.is_default == Some(true))
                                        .map(|choice| choice.id.as_str().to_owned())
                                });
                            for choice in &select.options {
                                items.push(
                                    SelectItem::new(
                                        format!(
                                            "{}\u{1}{}",
                                            select.id.as_str(),
                                            choice.id.as_str()
                                        ),
                                        div().pl_2().child(choice.label.as_str().to_owned()),
                                    )
                                    .selected(current.as_deref() == Some(choice.id.as_str())),
                                );
                            }
                        }
                        ProviderOptionDescriptor::Boolean(boolean) => {
                            let on = selected_value(&selected_options, boolean.id.as_str())
                                .and_then(Value::as_bool)
                                .or(boolean.current_value)
                                .unwrap_or(false);
                            items.push(
                                SelectItem::new(
                                    format!("{}\u{1}{}", boolean.id.as_str(), !on),
                                    div().child(format!(
                                        "{}: {}",
                                        boolean.label.as_str(),
                                        if on { "On" } else { "Off" }
                                    )),
                                )
                                .selected(on),
                            );
                        }
                    }
                }
                let entity = cx.entity();
                let on_change = on_change.clone();
                let base = json!({ "instanceId": instance_id, "model": model });
                let previous = selected_options.clone();
                let pick: std::rc::Rc<
                    dyn Fn(SharedString, &mut gpui_kit::Window, &mut gpui_kit::App),
                > = std::rc::Rc::new(move |item_id, _, cx| {
                    let Some((id, value)) = item_id.split_once('\u{1}') else {
                        return;
                    };
                    let value = match value {
                        "true" => json!(true),
                        "false" => json!(false),
                        other => json!(other),
                    };
                    let mut next: Vec<Value> = previous
                        .iter()
                        .filter(|selection| selection.get("id").and_then(Value::as_str) != Some(id))
                        .cloned()
                        .collect();
                    next.push(json!({ "id": id, "value": value }));
                    let mut selection = base.clone();
                    selection["options"] = Value::Array(next);
                    entity.update(cx, |this, cx| {
                        on_change(this, selection, cx);
                        cx.notify();
                    });
                });
                popup_list(
                    SharedString::from(format!("popup-{traits_menu}")),
                    items,
                    Some(rems(11.).to_pixels(px(16.))),
                    cx,
                    pick,
                )
                .into_any_element()
            });
            Some(with_popup(
                trigger,
                popup,
                PopupAlign::End,
                rems(1.75).to_pixels(px(16.)),
            ))
        });

        div()
            .flex()
            .min_w_0()
            .flex_wrap()
            .items_center()
            .justify_end()
            .gap(rems(0.375))
            .child(picker)
            .children(traits)
            .into_any_element()
    }

    /// `modelRow` of ProjectDefaultsSettings.
    pub(super) fn default_model_row(
        &mut self,
        json: &super::rows::Json,
        project_scope: bool,
        unavailable: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::settings_logic::scoped_settings::{ScopedSettingSource, scoped_settings_source};
        let vm = self.vm(cx);
        let context = self.scope_context(cx);
        let selection = json.get(Which::Server, "defaultModelSelection").clone();
        let is_automatic = selection.is_null();
        let source = scoped_settings_source(&context.targets, &["defaultModelSelection"]);
        let row = super::layout::SettingsRow::new("Model")
            .id("default-model")
            .description(if project_scope {
                "Model for new threads in this project."
            } else {
                "Default model for new threads. Projects can override it."
            });
        let (mut row, mixed) = self.scoped_row(
            cx,
            json,
            row,
            "default-model",
            "defaultModelSelection",
            "default model",
            "Reset to default",
        );
        let control = self.model_selection_control(
            cx,
            "default-model",
            &vm.providers,
            &selection,
            mixed,
            |this, value, cx| this.set_default_model(value, cx),
        );
        // "Automatic" names the null default while no project override hides it.
        if !unavailable && !mixed && source != ScopedSettingSource::Project && is_automatic {
            row = row.status("Automatic");
        }
        row.control(control).into_any_element()
    }

    /// `setModel`: refuses a model a selected target cannot run, else saves it.
    pub(super) fn set_default_model(&mut self, value: Value, cx: &mut Context<Self>) {
        if let Some((instance_id, model)) = selection_fields(&value)
            && let Some(reason) = self.model_disabled_reason(&instance_id, &model, cx)
        {
            self.toast(
                super::view::SettingsToastKind::Error,
                "Default model not saved",
                reason,
                cx,
            );
            return;
        }
        self.update(Which::Server, json!({ "defaultModelSelection": value }), cx);
    }

    /// `modelDisabledReason`: why a selected target cannot run `model` of
    /// `instance_id`, if one cannot.
    pub(super) fn model_disabled_reason(
        &self,
        instance_id: &str,
        model: &str,
        cx: &gpui_kit::App,
    ) -> Option<String> {
        use turin_contracts::provider_instance::ProviderInstanceId;
        let vm = self.vm(cx);
        let context = self.scope_context(cx);
        let provider = vm
            .providers
            .iter()
            .find(|provider| provider.instance_id.as_str() == instance_id);
        for target in &context.targets {
            let enabled_for_target =
                match (&target.project_id, ProviderInstanceId::new(instance_id)) {
                    (Some(_), Ok(id)) => {
                        turin_contracts::settings::resolve_project_provider_instance_enabled(
                            &target.settings.provider_instances,
                            &target.settings.project_settings_overrides,
                            target.project_id.as_ref(),
                            &id,
                        )
                    }
                    _ => provider.is_some_and(|provider| provider.enabled),
                };
            let runnable = provider.is_some_and(|provider| {
                provider_is_selectable(provider)
                    && provider
                        .models
                        .iter()
                        .any(|candidate| candidate.slug.as_str() == model)
            });
            if !enabled_for_target || !runnable {
                return Some(format!(
                    "This model is unavailable on {}. Select that environment to choose its model separately.",
                    target.label
                ));
            }
        }
        None
    }

    pub(super) fn text_generation_model_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let vm = self.vm(cx);
        let json = self.json(cx);
        let selection = json
            .get(Which::Server, "textGenerationModelSelection")
            .clone();
        let text_providers: Vec<ServerProvider> = vm
            .providers
            .iter()
            .filter(|provider| provider.is_text_generation_capable())
            .cloned()
            .collect();
        let control = if !text_providers.iter().any(provider_is_selectable) {
            div()
                .text_size(TEXT_SM.size)
                .text_color(cx.app_theme().muted_foreground)
                .child("No text generation providers available.")
                .into_any_element()
        } else {
            self.model_selection_control(
                cx,
                "text-generation-model",
                &text_providers,
                &selection,
                false,
                |this, value, cx| {
                    this.update_server(json!({ "textGenerationModelSelection": value }), cx)
                },
            )
        };
        Self::base_row("text-generation-model")
            .description("Used for thread titles and other generated text on connected devices with this provider. Source control can override it.")
            .reset_action(self.reset_button(cx, &json, Which::Server, "textGenerationModelSelection", "text generation model"))
            .control(control)
            .into_any_element()
    }
}

fn selected_value<'a>(selected: &'a [Value], id: &str) -> Option<&'a Value> {
    selected
        .iter()
        .find(|selection| selection.get("id").and_then(Value::as_str) == Some(id))
        .and_then(|selection| selection.get("value"))
}

/// The trigger text of `TraitsPicker`: each select's current choice label and
/// the booleans that are on.
fn traits_summary(descriptors: &[ProviderOptionDescriptor], selected: &[Value]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for descriptor in descriptors {
        match descriptor {
            ProviderOptionDescriptor::Select(select) => {
                let current = selected_value(selected, select.id.as_str())
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .or_else(|| select.current_value.as_ref().map(|v| v.as_str().to_owned()))
                    .or_else(|| {
                        select
                            .options
                            .iter()
                            .find(|choice| choice.is_default == Some(true))
                            .map(|choice| choice.id.as_str().to_owned())
                    });
                if let Some(choice) = select
                    .options
                    .iter()
                    .find(|choice| Some(choice.id.as_str()) == current.as_deref())
                {
                    parts.push(choice.label.as_str().to_owned());
                }
            }
            ProviderOptionDescriptor::Boolean(boolean) => {
                let on = selected_value(selected, boolean.id.as_str())
                    .and_then(Value::as_bool)
                    .or(boolean.current_value)
                    .unwrap_or(false);
                if on {
                    parts.push(boolean.label.as_str().to_owned());
                }
            }
        }
    }
    parts.join(" · ")
}
