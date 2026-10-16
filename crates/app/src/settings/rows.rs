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
    PopupAlign, SelectItem, SelectSize, SelectWidth, popup_list, select_trigger, switch_state,
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
    /// `server_settings` are the values rows display: the representative
    /// target's effective settings (project overrides applied) when a scope is
    /// selected, else the environment's.
    pub fn new(
        vm: &super::view_model::SettingsViewModel,
        server_settings: Option<&turin_contracts::settings::ServerSettings>,
    ) -> Self {
        Self {
            client: serde_json::to_value(&vm.client).unwrap_or(Value::Null),
            server: serde_json::to_value(server_settings.unwrap_or(&vm.server))
                .unwrap_or(Value::Null),
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

/// What [`SettingsView::row_scope_parts`] computes for one row.
pub struct RowScopeParts {
    pub inheritance: Option<AnyElement>,
    pub inert: Option<SharedString>,
    /// Present at a project scope when the row has an override to clear.
    pub reset: Option<AnyElement>,
    /// At a project scope a scoped row resets by clearing its override, not by
    /// restoring the default: `reset` replaces the usual reset action.
    pub reset_overridden: bool,
    /// The targets disagree on the row's keys.
    pub mixed: bool,
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
        let context = self.scope_context(cx);
        Json::new(&self.vm(cx), context.settings())
    }

    /// The scope-dependent parts of a server row (`SettingsRow`'s
    /// `settingKeys`): the inheritance indicator, whether the control is inert
    /// at a project scope, and the reset action that clears the override.
    pub(super) fn row_scope_parts(
        &mut self,
        cx: &mut Context<Self>,
        row_id: &str,
        keys: &[&'static str],
        label: &'static str,
    ) -> RowScopeParts {
        use super::setting_inheritance::SettingInheritanceState as State;
        use crate::settings_logic::scoped_settings::{
            ScopedSettingSource, is_project_scoped_setting_key, scoped_settings_are_mixed,
            scoped_settings_source,
        };
        let context = self.scope_context(cx);
        let project_scope = context.is_project_scope();
        let scoped_keys: Vec<&'static str> = keys
            .iter()
            .copied()
            .filter(|key| is_project_scoped_setting_key(key))
            .collect();
        let environment_wide = project_scope && scoped_keys.is_empty();
        let mixed = scoped_settings_are_mixed(&context.targets, keys);
        let source = if project_scope {
            scoped_settings_source(&context.targets, &scoped_keys)
        } else {
            ScopedSettingSource::Environment
        };
        let reset = if project_scope && !scoped_keys.is_empty() {
            matches!(
                source,
                ScopedSettingSource::Project | ScopedSettingSource::Mixed
            )
            .then(|| {
                let keys = scoped_keys.clone();
                let action = self.cb(cx, move |this, _, cx| {
                    this.clear_scoped_settings(&keys, cx);
                });
                setting_reset_button(
                    SharedString::from(format!("reset-override-{row_id}")),
                    "Reset to inherited value",
                    move |window, cx| action(window, cx),
                )
                .into_any_element()
            })
        } else {
            None
        };
        let environment_settings = context
            .environments
            .first()
            .and_then(|environment| environment.server_config.as_ref())
            .map(|config| config.settings.clone());
        let customized = environment_settings.as_ref().is_some_and(|settings| {
            keys.iter().any(|key| {
                let current = serde_json::to_value(settings)
                    .ok()
                    .and_then(|value| value.get(*key).cloned());
                let default =
                    serde_json::to_value(turin_contracts::settings::default_server_settings())
                        .ok()
                        .and_then(|value| value.get(*key).cloned());
                current != default
            })
        });
        let inherited_from = if matches!(source, ScopedSettingSource::Environment)
            && context.scope.environment_ids.len() == 1
        {
            context
                .environments
                .iter()
                .find(|environment| {
                    Some(&environment.environment_id) == context.scope.environment_ids.first()
                })
                .map(|environment| environment.label.clone())
                .unwrap_or_else(|| "environment".to_owned())
        } else {
            "environment".to_owned()
        };
        let (state, summary) = if mixed {
            (
                State::Mixed,
                "Mixed across selected environments".to_owned(),
            )
        } else if matches!(source, ScopedSettingSource::Project) {
            (State::Overridden, "Overridden for this project".to_owned())
        } else if matches!(source, ScopedSettingSource::Environment) && !scoped_keys.is_empty() {
            (State::Inherited, format!("Inherited from {inherited_from}"))
        } else if customized {
            (State::Environment, "Set on the environment".to_owned())
        } else {
            (State::Default, "Built-in default".to_owned())
        };
        let overriding = self.overriding_projects(&scoped_keys, cx);
        let inheritance = environment_settings.as_ref().and_then(|settings| {
            self.setting_inheritance(
                row_id,
                state,
                &summary,
                &context.targets,
                settings,
                keys,
                overriding,
                cx,
            )
        });
        let _ = label;
        RowScopeParts {
            inheritance,
            inert: environment_wide.then(|| {
                SharedString::from("Environment-wide setting. Select an environment to change it.")
            }),
            reset,
            reset_overridden: project_scope && !scoped_keys.is_empty(),
            mixed,
        }
    }

    /// Adds [`Self::row_scope_parts`] to a row of server keys.
    pub(super) fn decorate_server_row(
        &mut self,
        cx: &mut Context<Self>,
        row: SettingsRow,
        row_id: &str,
        keys: &[&'static str],
        label: &'static str,
    ) -> (SettingsRow, Option<Option<AnyElement>>, bool) {
        let parts = self.row_scope_parts(cx, row_id, keys, label);
        let reset = parts.reset_overridden.then_some(parts.reset);
        (
            row.inheritance(parts.inheritance).inert(parts.inert),
            reset,
            parts.mixed,
        )
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
        self.reset_button_tip(cx, json, which, key, label, "Reset to default")
    }

    /// [`Self::reset_button`] with its own tooltip (`SettingResetButton tooltip`).
    pub(super) fn reset_button_tip(
        &self,
        cx: &mut Context<Self>,
        json: &Json,
        which: Which,
        key: &'static str,
        label: &'static str,
        tooltip: &'static str,
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
                tooltip,
                move |window, cx| action(window, cx),
            )
            .into_any_element(),
        )
    }

    /// Decorates a server row with the scope parts and its reset: clearing the
    /// override at a project scope, else restoring the default (`tooltip`).
    /// Returns whether the targets disagree on the row's key.
    pub(super) fn scoped_row(
        &mut self,
        cx: &mut Context<Self>,
        json: &Json,
        row: SettingsRow,
        row_id: &str,
        key: &'static str,
        reset_label: &'static str,
        tooltip: &'static str,
    ) -> (SettingsRow, bool) {
        let (row, scoped_reset, mixed) =
            self.decorate_server_row(cx, row, row_id, &[key], reset_label);
        let reset = match scoped_reset {
            Some(reset) => reset,
            None => self.reset_button_tip(cx, json, Which::Server, key, reset_label, tooltip),
        };
        (row.reset_action(reset), mixed)
    }

    pub(super) fn bool_row(
        &mut self,
        cx: &mut Context<Self>,
        json: &Json,
        spec: BoolRow,
    ) -> AnyElement {
        self.bool_row_tip(cx, json, spec, "Reset to default")
    }

    /// [`Self::bool_row`] with the reset tooltip of the row.
    pub(super) fn bool_row_tip(
        &mut self,
        cx: &mut Context<Self>,
        json: &Json,
        spec: BoolRow,
        tooltip: &'static str,
    ) -> AnyElement {
        let checked = json.bool(spec.which, spec.key);
        let key = spec.key;
        let which = spec.which;
        let entity = cx.entity();
        let row = match spec.title {
            Some(title) => SettingsRow::new(title).id(spec.search_id),
            None => Self::base_row(spec.search_id),
        };
        let (row, scoped_reset, mixed) = if which == Which::Server {
            self.decorate_server_row(cx, row, spec.search_id, &[key], spec.reset_label)
        } else {
            (row, None, false)
        };
        let control = switch_state(
            SharedString::from(format!("switch-{}", spec.search_id)),
            checked && !mixed,
            mixed,
            false,
            cx,
            move |next, _, cx| {
                entity.update(cx, |this, cx| this.update(which, json!({ key: next }), cx));
            },
        );
        let reset = match scoped_reset {
            Some(reset) => reset,
            None => self.reset_button_tip(cx, json, which, key, spec.reset_label, tooltip),
        };
        row.description(spec.description)
            .reset_action(reset)
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
        let (row, scoped_reset, _) = if which == Which::Server {
            self.decorate_server_row(
                cx,
                Self::base_row(spec.search_id),
                spec.search_id,
                &[key],
                spec.reset_label,
            )
        } else {
            (Self::base_row(spec.search_id), None, false)
        };
        let reset = match scoped_reset {
            Some(reset) => reset,
            None => self.reset_button(cx, json, which, key, spec.reset_label),
        };
        row.description(spec.description)
            .reset_action(reset)
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
        self.select_control_sized(
            cx,
            menu_id,
            label.into_any_element(),
            options,
            width,
            SelectSize::Sm,
            check,
            on_pick,
        )
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
        self.select_control_sized(
            cx,
            menu_id,
            label,
            options,
            width,
            SelectSize::Sm,
            false,
            on_pick,
        )
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
            .child(with_popup(
                trigger,
                popup,
                PopupAlign::End,
                trigger_height(size),
            ))
            .into_any_element()
    }
}
