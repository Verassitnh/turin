//! Row builders shared by the panels: settings are read and written by their
//! JSON key (the wire names), the way upstream's rows do with
//! `settings.<key>` / `updateSettings({ <key>: … })`.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, ElementId, IntoElement, ParentElement as _, SharedString, Styled as _,
    Window, div, px, rems,
};
use serde_json::{Value, json};

use super::controls::{
    PopupAlign, SelectItem, SelectSize, SelectWidth, popup_list, select_trigger, switch,
    trigger_height, with_popup,
};
use super::layout::{SettingsRow, setting_reset_button};
use super::search::searchable_setting;
use super::view::{SettingsView, Which};
use crate::theme::ActiveAppTheme;
use turin_contracts::settings::{default_client_settings, default_server_settings};

/// Settings as JSON next to their defaults.
pub struct Json {
    client: Value,
    server: Value,
    client_default: Value,
    server_default: Value,
}

impl Json {
    pub fn new(vm: &super::view_model::SettingsViewModel) -> Self {
        Self {
            client: serde_json::to_value(&vm.client).unwrap_or(Value::Null),
            server: serde_json::to_value(&vm.server).unwrap_or(Value::Null),
            client_default: serde_json::to_value(default_client_settings()).unwrap_or(Value::Null),
            server_default: serde_json::to_value(default_server_settings()).unwrap_or(Value::Null),
        }
    }

    pub fn get(&self, which: Which, key: &str) -> &Value {
        let source = match which {
            Which::Client => &self.client,
            Which::Server => &self.server,
        };
        source.get(key).unwrap_or(&Value::Null)
    }

    pub fn default_of(&self, which: Which, key: &str) -> &Value {
        let source = match which {
            Which::Client => &self.client_default,
            Which::Server => &self.server_default,
        };
        source.get(key).unwrap_or(&Value::Null)
    }

    pub fn bool(&self, which: Which, key: &str) -> bool {
        self.get(which, key).as_bool().unwrap_or(false)
    }

    pub fn str(&self, which: Which, key: &str) -> &str {
        self.get(which, key).as_str().unwrap_or("")
    }

    pub fn changed(&self, which: Which, key: &str) -> bool {
        self.get(which, key) != self.default_of(which, key)
    }
}

/// A switch row on one boolean key.
pub struct BoolRow {
    pub search_id: &'static str,
    /// For rows without a catalog entry.
    pub title: Option<&'static str>,
    pub description: SharedString,
    pub which: Which,
    pub key: &'static str,
    /// `aria-label` (used for the element id) and the reset button's label.
    pub label: &'static str,
    pub reset_label: &'static str,
}

/// A select row on one key.
pub struct SelectRowSpec {
    pub search_id: &'static str,
    pub description: SharedString,
    pub which: Which,
    pub key: &'static str,
    pub reset_label: &'static str,
    pub options: Vec<(Value, SharedString)>,
    pub width: SelectWidth,
    /// Show a check next to the selected item (`CheckIcon` in the item).
    pub check: bool,
}

impl SettingsView {
    pub(super) fn json(&self, cx: &gpui_kit::App) -> Json {
        Json::new(&self.vm(cx))
    }

    /// `<SettingsRow {...searchableSetting(id)} />`.
    pub(super) fn base_row(search_id: &str) -> SettingsRow {
        let (id, title) = searchable_setting(search_id);
        SettingsRow::new(title).id(id)
    }

    pub(super) fn reset_button(
        &self,
        cx: &mut Context<Self>,
        json: &Json,
        which: Which,
        key: &'static str,
        label: &'static str,
    ) -> Option<AnyElement> {
        if label.is_empty() || !json.changed(which, key) {
            return None;
        }
        let default = json.default_of(which, key).clone();
        let action = self.cb(cx, move |this, _, cx| {
            this.update(which, json!({ key: default.clone() }), cx);
        });
        Some(
            setting_reset_button(
                SharedString::from(format!("reset-{key}")),
                "Reset to default",
                move |window, cx| action(window, cx),
            )
            .into_any_element(),
        )
        .map(|element| {
            let _ = label;
            element
        })
    }

    pub(super) fn bool_row(&mut self, cx: &mut Context<Self>, json: &Json, spec: BoolRow) -> AnyElement {
        let checked = json.bool(spec.which, spec.key);
        let key = spec.key;
        let which = spec.which;
        let toggle = self.cb(cx, |_, _, _| {});
        let _ = toggle;
        let entity = cx.entity();
        let control = switch(
            SharedString::from(format!("switch-{}", spec.search_id)),
            checked,
            false,
            cx,
            move |next, _, cx| {
                entity.update(cx, |this, cx| this.update(which, json!({ key: next }), cx));
            },
        );
        let row = match spec.title {
            Some(title) => SettingsRow::new(title).id(spec.search_id),
            None => Self::base_row(spec.search_id),
        };
        row.description(spec.description)
            .reset_action(self.reset_button(cx, json, which, key, spec.reset_label))
            .control(control)
            .into_any_element()
    }

    pub(super) fn select_row(
        &mut self,
        cx: &mut Context<Self>,
        json: &Json,
        spec: SelectRowSpec,
    ) -> AnyElement {
        let current = json.get(spec.which, spec.key).clone();
        let label = spec
            .options
            .iter()
            .find(|(value, _)| *value == current)
            .map(|(_, label)| label.clone())
            .unwrap_or_default();
        let which = spec.which;
        let key = spec.key;
        let control = self.select_control(
            cx,
            spec.search_id,
            label,
            spec.options
                .iter()
                .map(|(value, label)| (value.clone(), label.clone(), *value == current))
                .collect(),
            spec.width,
            spec.check,
            move |this, value, _, cx| {
                this.update(which, json!({ key: value }), cx);
            },
        );
        Self::base_row(spec.search_id)
            .description(spec.description)
            .reset_action(self.reset_button(cx, json, which, key, spec.reset_label))
            .control(control)
            .into_any_element()
    }

    /// A `Select size="sm"` trigger with its popup (`align="end"`).
    pub(super) fn select_control(
        &mut self,
        cx: &mut Context<Self>,
        menu_id: &str,
        label: SharedString,
        options: Vec<(Value, SharedString, bool)>,
        width: SelectWidth,
        check: bool,
        on_pick: impl Fn(&mut Self, Value, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let options = options
            .into_iter()
            .map(|(value, label, selected)| (value, label.into_any_element(), selected))
            .collect();
        self.select_control_sized(cx, menu_id, label.into_any_element(), options, width, SelectSize::Sm, check, on_pick)
    }

    /// [`Self::select_control`] with element labels (icons, descriptions).
    pub(super) fn select_control_rich(
        &mut self,
        cx: &mut Context<Self>,
        menu_id: &str,
        label: AnyElement,
        options: Vec<(Value, AnyElement, bool)>,
        width: SelectWidth,
        on_pick: impl Fn(&mut Self, Value, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        self.select_control_sized(cx, menu_id, label, options, width, SelectSize::Sm, false, on_pick)
    }

    pub(super) fn select_control_sized(
        &mut self,
        cx: &mut Context<Self>,
        menu_id: &str,
        label: AnyElement,
        options: Vec<(Value, AnyElement, bool)>,
        width: SelectWidth,
        size: SelectSize,
        check: bool,
        on_pick: impl Fn(&mut Self, Value, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let menu_id = SharedString::from(menu_id.to_owned());
        let open = self.menus.is_open(&menu_id);
        let trigger = select_trigger(
            ElementId::Name(SharedString::from(format!("select-{menu_id}"))),
            label,
            size,
            width,
            open,
            false,
            cx,
        );
        let toggle_id = menu_id.clone();
        let trigger = gpui_kit::StatefulInteractiveElement::on_click(
            trigger,
            cx.listener(move |this, _, _, cx| {
                this.menus.toggle(toggle_id.clone());
                cx.notify();
            }),
        );
        let popup = open.then(|| {
            let values: Vec<Value> = options.iter().map(|(value, _, _)| value.clone()).collect();
            let theme = cx.app_theme().clone();
            let items: Vec<SelectItem> = options
                .into_iter()
                .enumerate()
                .map(|(index, (_, label, selected))| {
                    let content = div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_4()
                        .child(label)
                        .when_some((check && selected).then_some(()), |this, _| {
                            this.child(
                                crate::ui::Icon::new(crate::ui::IconName::Check)
                                    .size(rems(1.))
                                    .color(theme.foreground),
                            )
                        });
                    SelectItem::new(index.to_string(), content).selected(selected)
                })
                .collect();
            let close_id = menu_id.clone();
            let entity = cx.entity();
            let pick: std::rc::Rc<dyn Fn(SharedString, &mut Window, &mut gpui_kit::App)> =
                std::rc::Rc::new(move |item_id, window, cx| {
                    let Ok(index) = item_id.parse::<usize>() else {
                        return;
                    };
                    let value = values[index].clone();
                    let _ = &close_id;
                    entity.update(cx, |this, cx| {
                        this.menus.close();
                        on_pick(this, value, window, cx);
                        cx.notify();
                    });
                });
            popup_list(
                SharedString::from(format!("popup-{menu_id}")),
                items,
                Some(match width {
                    SelectWidth::Fixed(w) => rems(w).to_pixels(px(16.)),
                    _ => px(0.),
                }),
                cx,
                pick,
            )
            .into_any_element()
        });
        // flex-wrap: the wrapped line sizes the trigger to its content (a plain
        // flex row collapses `w-auto` select triggers to their chrome in GPUI).
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_end()
            .child(with_popup(trigger, popup, PopupAlign::End, trigger_height(size)))
            .into_any_element()
    }
}

