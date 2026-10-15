//! Port of AddProviderInstanceDialog.tsx and AddProviderInstanceWizardSteps.tsx
//! (the three-step wizard) with `ui/wizard.tsx`.
//!
//! Not ported (PORTING.md Deviations): the ACP registry branch, the managed
//! ChatGPT account flow (`ChatGptConnectionButton`) and the post-create
//! sign-in step; the new instance is added enabled and the Account row of
//! its card starts the sign-in.

use std::collections::{HashMap, HashSet};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px, rems,
};
use serde_json::{Value, json};
use turin_contracts::settings::ProviderSettingsFormControl;

use super::super::add_provider_instance_logic::{
    ADD_PROVIDER_WIZARD_STEPS, ProviderIdentityDraft, ProviderIdentityDraftUpdate, WizardNavigation,
    WizardValidation, derive_available_instance_id, get_provider_identity_draft,
    resolve_wizard_navigation, update_provider_identity_draft,
};
use super::super::controls::{FieldSize, SelectWidth, switch, text_field};
use super::super::dialog::{Dialog, dialog_frame, dialog_header, dialog_panel};
use super::super::layout::SettingsRow;
use super::super::provider_form::{
    FieldValue, derive_provider_settings_fields, next_provider_config_with_field_value,
    read_provider_config_boolean, read_provider_config_string,
};
use super::super::provider_meta::{PROVIDER_CLIENTS, driver_label};
use super::super::view::SettingsView;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, RADIUS_XL, TEXT_SM, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName};

pub struct AddProviderState {
    pub step: usize,
    pub driver: String,
    pub identity: HashMap<String, ProviderIdentityDraft>,
    pub config: HashMap<String, Value>,
    pub attempted: bool,
}

impl Default for AddProviderState {
    fn default() -> Self {
        Self {
            step: 0,
            driver: "codex".into(),
            identity: HashMap::new(),
            config: HashMap::new(),
            attempted: false,
        }
    }
}

/// `slugifyLabel` + `deriveInstanceId`.
fn derive_instance_id(driver: &str, label: &str) -> String {
    let mut slug = String::new();
    let mut last_underscore = false;
    for ch in label.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_underscore = false;
        } else if !last_underscore {
            slug.push('_');
            last_underscore = true;
        }
    }
    let slug: String = slug.trim_matches('_').chars().take(48).collect();
    if slug.is_empty() { String::new() } else { format!("{driver}_{slug}") }
}

/// `validateInstanceId`.
fn validate_instance_id(id: &str, existing: &HashSet<String>) -> Option<String> {
    if id.is_empty() {
        return Some("Instance ID is required.".into());
    }
    if id.chars().count() > 64 {
        return Some("Instance ID must be 64 characters or fewer.".into());
    }
    let mut chars = id.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !valid {
        return Some("Instance ID must start with a letter and use only letters, digits, '-', or '_'.".into());
    }
    if existing.contains(id) {
        return Some(format!("An instance named '{id}' already exists."));
    }
    None
}

impl SettingsView {
    pub(in crate::settings) fn open_add_provider(&mut self, cx: &mut Context<Self>) {
        self.panels.providers.wizard = AddProviderState::default();
        self.dialog = Some(Dialog::AddProvider);
        cx.notify();
    }

    fn wizard_existing_ids(&self, cx: &gpui_kit::App) -> HashSet<String> {
        let vm = self.vm(cx);
        let mut ids: HashSet<String> = ["codex".to_owned(), "claudeAgent".to_owned()].into();
        ids.extend(vm.server.provider_instances.keys().map(|id| id.as_str().to_owned()));
        ids
    }

    /// The identity of the current draft: (label, accent, instance id).
    fn wizard_identity(&self, cx: &gpui_kit::App) -> (String, String, String) {
        let wizard = &self.panels.providers.wizard;
        let default_label = driver_label(&wizard.driver);
        let draft = get_provider_identity_draft(&wizard.identity, &wizard.driver);
        let label = if wizard.identity.contains_key(&wizard.driver) { draft.label.clone() } else { default_label.clone() };
        let existing = self.wizard_existing_ids(cx);
        let driver = wizard.driver.clone();
        let instance_id = draft.instance_id_override.clone().unwrap_or_else(|| {
            derive_available_instance_id(
                |candidate| {
                    if candidate.trim().is_empty() || candidate == default_label {
                        driver.clone()
                    } else {
                        derive_instance_id(&driver, candidate)
                    }
                },
                &label,
                &existing,
            )
        });
        (label, draft.accent_color, instance_id)
    }

    fn wizard_navigate(&mut self, requested: isize, cx: &mut Context<Self>) {
        let (_, _, instance_id) = self.wizard_identity(cx);
        let error = validate_instance_id(&instance_id, &self.wizard_existing_ids(cx));
        let validation = WizardValidation { instance_id_error: error, identity_step: None, prerequisite: None };
        let navigation = resolve_wizard_navigation(
            self.panels.providers.wizard.step,
            requested,
            ADD_PROVIDER_WIZARD_STEPS.len(),
            &validation,
        );
        match navigation {
            WizardNavigation::Navigate { step } => self.panels.providers.wizard.step = step,
            WizardNavigation::Blocked { step, .. } => {
                self.panels.providers.wizard.attempted = true;
                self.panels.providers.wizard.step = step;
            }
        }
        cx.notify();
    }

    fn wizard_save(&mut self, cx: &mut Context<Self>) {
        self.panels.providers.wizard.attempted = true;
        let (label, accent, instance_id) = self.wizard_identity(cx);
        if validate_instance_id(&instance_id, &self.wizard_existing_ids(cx)).is_some() {
            self.panels.providers.wizard.step = 1;
            cx.notify();
            return;
        }
        let driver = self.panels.providers.wizard.driver.clone();
        let mut config = self.panels.providers.wizard.config.get(&driver).cloned().unwrap_or(Value::Null);
        if driver == "codex" {
            let mut map = config.as_object().cloned().unwrap_or_default();
            map.insert("setupMode".into(), json!("existing"));
            config = Value::Object(map);
        }
        let mut instance = serde_json::Map::new();
        instance.insert("driver".into(), json!(driver));
        instance.insert("enabled".into(), json!(true));
        if !label.trim().is_empty() {
            instance.insert("displayName".into(), json!(label.trim()));
        }
        if !accent.trim().is_empty() {
            instance.insert("accentColor".into(), json!(accent.trim()));
        }
        if config.as_object().is_some_and(|m| !m.is_empty()) {
            instance.insert("config".into(), config);
        }
        self.write_provider_instance(&instance_id, Some(Value::Object(instance)), false, cx);
        self.panels.providers.selected = Some(instance_id);
        self.dialog = None;
        cx.notify();
    }

    pub(in crate::settings) fn render_add_provider_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let vm = self.vm(cx);
        let step = self.panels.providers.wizard.step;
        let driver = self.panels.providers.wizard.driver.clone();
        let (label, accent, instance_id) = self.wizard_identity(cx);
        let id_error = validate_instance_id(&instance_id, &self.wizard_existing_ids(cx));
        let show_error = self.panels.providers.wizard.attempted && id_error.is_some();
        let preview_label = if label.trim().is_empty() { format!("{} Workspace", driver_label(&driver)) } else { label.trim().to_owned() };
        let summaries = [Some(driver_label(&driver)), Some(preview_label), None];

        // Step pills (`WizardSteps`).
        let mut pills = div()
            .flex()
            .gap_1()
            .p_1()
            .rounded(RADIUS_XL)
            .bg(if theme.is_dark() { with_alpha(gpui_kit::white(), 0.04).into() } else { theme.background })
            .border_1()
            .border_color(if theme.is_dark() { with_alpha(gpui_kit::white(), 0.05) } else { with_alpha(gpui_kit::black(), 0.05) });
        for (index, name) in ADD_PROVIDER_WIZARD_STEPS.iter().enumerate() {
            let current = index == step;
            let done = index < step;
            let badge = div()
                .flex()
                .size_5()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .rounded_full()
                .text_size(TEXT_SM.size)
                .font_weight(FontWeight::MEDIUM)
                .border_1()
                .when(done, |this| this.bg(theme.primary).text_color(theme.primary_foreground).border_color(theme.primary))
                .when(current, |this| this.bg(with_alpha(theme.primary, 0.1)).text_color(theme.primary).border_color(with_alpha(theme.primary, 0.3)))
                .when(!done && !current, |this| this.bg(theme.card).text_color(theme.muted_foreground).border_color(with_alpha(theme.foreground, 0.1)))
                .child(if done {
                    Icon::new(IconName::Check).size(rems(1.)).color(theme.primary_foreground).into_any_element()
                } else {
                    div().child((index + 1).to_string()).into_any_element()
                });
            let _ = &summaries;
            pills = pills.child(
                div()
                    .id(SharedString::from(format!("wizard-step-{index}")))
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .rounded(RADIUS_LG)
                    .px(px(10.))
                    .py_2()
                    .cursor(CursorStyle::PointingHand)
                    .when(current, |this| this.bg(theme.card).border_1().border_color(with_alpha(theme.foreground, 0.05)))
                    .when(!current, |this| this.hover(|style| style.bg(theme.card)))
                    .on_click(cx.listener(move |this, _, _, cx| this.wizard_navigate(index as isize, cx)))
                    .child(badge)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(TEXT_SM.size)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if current { theme.foreground } else { theme.muted_foreground })
                            .child(*name),
                    ),
            );
        }

        // Panel body by step.
        let body: AnyElement = match step {
            0 => {
                let mut grid = div().flex().flex_wrap().w_full().gap_2();
                for (kind, name) in PROVIDER_CLIENTS {
                    let selected = *kind == driver;
                    let kind = (*kind).to_owned();
                    grid = grid.child(
                        div()
                            .id(SharedString::from(format!("wizard-driver-{kind}")))
                            .flex()
                            .w(gpui_kit::relative(0.495))
                            .items_center()
                            .gap_3()
                            .rounded(RADIUS_LG)
                            .px_3()
                            .py_3()
                            .cursor(CursorStyle::PointingHand)
                            .bg(if selected { with_alpha(theme.primary, if theme.is_dark() { 0.15 } else { 0.08 }).into() } else if theme.is_dark() { with_alpha(gpui_kit::white(), 0.03).into() } else { theme.card })
                            .border_1()
                            .border_color(if selected { theme.primary } else { with_alpha(theme.foreground, 0.05).into() })
                            .when(!selected, |this| this.hover(|style| style.bg(theme.muted)))
                            .on_click({
                                let kind = kind.clone();
                                cx.listener(move |this, _, _, cx| {
                                    this.panels.providers.wizard.driver = kind.clone();
                                    this.panels.providers.wizard.attempted = false;
                                    cx.notify();
                                })
                            })
                            .child(super::providers::provider_brand_icon(&kind, rems(1.), if selected { theme.foreground } else { theme.muted_foreground }))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .text_size(TEXT_SM.size)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child(*name),
                            )
                            .when(selected, |this| {
                                this.child(
                                    div()
                                        .flex()
                                        .size_5()
                                        .flex_shrink_0()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .bg(theme.primary)
                                        .child(Icon::new(IconName::Check).size(rems(0.875)).color(theme.primary_foreground)),
                                )
                            }),
                    );
                }
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_size(TEXT_SM.size).font_weight(FontWeight::MEDIUM).text_color(theme.foreground).child("Provider"))
                    .child(grid)
                    .into_any_element()
            }
            1 => {
                let label_input = self.input_live("add-provider-label", &label, "e.g. Work", window, cx, {
                    let driver = driver.clone();
                    move |this, text, _, cx| {
                        let wizard = &mut this.panels.providers.wizard;
                        wizard.identity = update_provider_identity_draft(
                            &wizard.identity,
                            &driver,
                            ProviderIdentityDraftUpdate { label: Some(text), ..Default::default() },
                        );
                        cx.notify();
                    }
                });
                let id_input = self.input_live("add-provider-instance-id", &instance_id, &format!("{driver}_work"), window, cx, {
                    let driver = driver.clone();
                    move |this, text, _, cx| {
                        let wizard = &mut this.panels.providers.wizard;
                        wizard.identity = update_provider_identity_draft(
                            &wizard.identity,
                            &driver,
                            ProviderIdentityDraftUpdate { instance_id_override: Some(Some(text)), ..Default::default() },
                        );
                        cx.notify();
                    }
                });
                let accent_input = self.input_live("add-provider-accent", &accent, "#2563eb", window, cx, {
                    let driver = driver.clone();
                    move |this, text, _, cx| {
                        let wizard = &mut this.panels.providers.wizard;
                        wizard.identity = update_provider_identity_draft(
                            &wizard.identity,
                            &driver,
                            ProviderIdentityDraftUpdate { accent_color: Some(text), ..Default::default() },
                        );
                        cx.notify();
                    }
                });
                // The label draft starts from the driver's label; a typed label replaces it.
                let mut error_status = None;
                if show_error {
                    error_status = id_error.clone();
                }
                let mut id_row = SettingsRow::new("Instance ID")
                    .description("Letters, digits, '-', or '_'.")
                    .control(div().w(rems(14.)).child(text_field(&id_input, None, FieldSize::Sm, false, cx)));
                if let Some(error) = error_status {
                    id_row = id_row.status(div().text_color(theme.destructive).child(error));
                }
                div()
                    .flex()
                    .flex_col()
                    .child(
                        SettingsRow::new("Label")
                            .description("Shown in the provider list.")
                            .control(div().w(rems(14.)).child(text_field(&label_input, None, FieldSize::Sm, false, cx))),
                    )
                    .child(id_row)
                    .child(
                        SettingsRow::new("Accent color")
                            .description("Optional marker shown in the picker.")
                            .control(div().w(rems(14.)).child(text_field(&accent_input, None, FieldSize::Sm, true, cx))),
                    )
                    .into_any_element()
            }
            _ => {
                let fields = derive_provider_settings_fields(&driver);
                if fields.is_empty() {
                    div()
                        .text_size(TEXT_SM.size)
                        .text_color(theme.muted_foreground)
                        .child("This driver has no required configuration. You can add the instance now.")
                        .into_any_element()
                } else {
                    let config = self.panels.providers.wizard.config.get(&driver).cloned();
                    let mut column = div().flex().flex_col();
                    for field in fields {
                        let driver_for_write = driver.clone();
                        let field_for_write = field.clone();
                        let write = move |this: &mut Self, value: FieldValue<'_>, cx: &mut Context<Self>| {
                            let current = this.panels.providers.wizard.config.get(&driver_for_write).cloned();
                            match next_provider_config_with_field_value(current.as_ref(), &field_for_write, value) {
                                Some(next) => {
                                    this.panels.providers.wizard.config.insert(driver_for_write.clone(), next);
                                }
                                None => {
                                    this.panels.providers.wizard.config.remove(&driver_for_write);
                                }
                            }
                            cx.notify();
                        };
                        let control: AnyElement = match field.control {
                            ProviderSettingsFormControl::Switch => {
                                let entity = cx.entity();
                                switch(
                                    SharedString::from(format!("add-provider-field-{}", field.key)),
                                    read_provider_config_boolean(config.as_ref(), &field.key, field.default_boolean_value.unwrap_or(false)),
                                    false,
                                    cx,
                                    move |next, _, cx| {
                                        entity.update(cx, |this, cx| write(this, FieldValue::Bool(next), cx));
                                    },
                                )
                                .into_any_element()
                            }
                            ProviderSettingsFormControl::Select => {
                                let options = field.options.clone().unwrap_or_default();
                                let fallback = options.first().map(|o| o.value.clone()).unwrap_or_default();
                                let current = {
                                    let value = read_provider_config_string(config.as_ref(), &field.key);
                                    if value.is_empty() { fallback.clone() } else { value }
                                };
                                let current_label = options.iter().find(|o| o.value == current).map(|o| o.label.clone()).unwrap_or_else(|| current.clone());
                                self.select_control(
                                    cx,
                                    &format!("add-provider-select-{}", field.key),
                                    current_label.into(),
                                    options.iter().map(|o| (json!(o.value), SharedString::from(o.label.clone()), o.value == current)).collect(),
                                    SelectWidth::Fixed(14.),
                                    false,
                                    move |this, value, _, cx| {
                                        if let Some(next) = value.as_str() {
                                            let stored = if next == fallback { "" } else { next };
                                            write(this, FieldValue::Text(stored), cx);
                                        }
                                    },
                                )
                            }
                            _ => {
                                let current = read_provider_config_string(config.as_ref(), &field.key);
                                let input = self.input(
                                    &format!("add-provider-field-{driver}-{}", field.key),
                                    &current,
                                    &field.placeholder.clone().unwrap_or_default(),
                                    window,
                                    cx,
                                    move |this, text, _, cx| write(this, FieldValue::Text(&text), cx),
                                );
                                div().w(rems(14.)).child(text_field(&input, None, FieldSize::Sm, false, cx)).into_any_element()
                            }
                        };
                        let mut row = SettingsRow::new(field.label.clone()).control(control);
                        if let Some(description) = &field.description {
                            row = row.description(description.clone());
                        }
                        column = column.child(row);
                    }
                    column.into_any_element()
                }
            }
        };

        let dismiss = self.cb(cx, |this, _, cx| {
            this.dialog = None;
            cx.notify();
        });
        let last = step + 1 == ADD_PROVIDER_WIZARD_STEPS.len();
        let back = Button::new("wizard-back")
            .variant(if step == 0 { ButtonVariant::GhostMuted } else { ButtonVariant::Outline })
            .size(ButtonSize::Sm)
            .label(if step == 0 { "Cancel" } else { "Back" })
            .on_click(cx.listener(|this, _, _, cx| {
                if this.panels.providers.wizard.step == 0 {
                    this.dialog = None;
                } else {
                    this.panels.providers.wizard.step -= 1;
                }
                cx.notify();
            }));
        let next = if last {
            Button::new("wizard-add")
                .size(ButtonSize::Sm)
                .label("Add instance")
                .on_click(cx.listener(|this, _, _, cx| this.wizard_save(cx)))
        } else {
            Button::new("wizard-next")
                .size(ButtonSize::Sm)
                .label("Next")
                .on_click(cx.listener(move |this, _, _, cx| this.wizard_navigate(step as isize + 1, cx)))
        };
        let environment = vm.environment.label.clone();
        let panel_body = div()
            .flex()
            .flex_col()
            .gap_4()
            .min_w_0()
            .child(pills)
            .child(
                div()
                    .min_w_0()
                    .rounded(RADIUS_LG)
                    .bg(if theme.is_dark() { gpui_kit::Hsla::from(with_alpha(gpui_kit::white(), 0.02)) } else { gpui_kit::Hsla::from(with_alpha(theme.background, 0.8)) })
                    .border_1()
                    .border_color(with_alpha(theme.foreground, 0.05))
                    .px_6()
                    .py_5()
                    .child(body),
            );
        dialog_frame(
            &theme,
            rems(48.),
            vec![
                dialog_header("Add provider", Some(format!("Add an account or configure a provider on {environment}.").into()), &theme),
                dialog_panel("wizard-body", panel_body),
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .px_6()
                    .pb_6()
                    .child(back)
                    .child(next)
                    .into_any_element(),
            ],
            dismiss,
        )
    }
}
