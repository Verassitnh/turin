//! Port of ProviderSettingsPanel.tsx (`EnvironmentProviderSettings`),
//! ProviderInstanceCard.tsx, ProviderSettingsForm.tsx, ProviderModelsSection.tsx
//! and UsageProviderSettings.tsx for the single local environment.
//!
//! Not ported (PORTING.md Deviations): the environment picker / read-only
//! access states (remote access and auth are cut), the ACP registry and
//! Antigravity / Grok / Pi / Muse drivers, the Codex managed-runtime setup
//! and the provider sign-in flows that run in a terminal (the Account row
//! only starts / ends sign-in through the source), the project-scope enable
//! override UI, and the HSV accent color picker (preset swatches + hex field).
//!
//! Port of upstream/apps/web/src/components/settings/ProviderSettingsPanel.tsx.
//! Port of upstream/apps/web/src/components/settings/ProviderInstanceCard.tsx.
//! Port of upstream/apps/web/src/components/settings/UsageProviderSettings.tsx.

use std::rc::Rc;

use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, ClipboardItem, Context, CursorStyle, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, px, rems,
};
use serde_json::{Value, json};
use turin_contracts::base_schemas::ProjectId;
use turin_contracts::provider_instance::{ProviderInstanceConfig, ProviderInstanceId};
use turin_contracts::settings::{
    ProviderSettingsFormControl, resolve_project_provider_instance_enabled,
    resolve_provider_instance_enabled,
};
use turin_provider_core::server_provider::{
    ServerProvider, ServerProviderAuthStatus, ServerProviderState,
};

use super::super::add_provider_instance_logic::{CodexSetupMode, read_codex_setup_mode};
use super::super::controls::{
    FieldSize, PopupAlign, SelectWidth, password_field, redacted_sensitive_text, switch,
    text_field, toggle_menu, with_popup,
};
use super::super::folded_settings_section::{FoldedHeaderPlacement, FoldedSection};
use super::super::layout::{SettingsRow, SettingsSection, setting_reset_button, settings_group};
use super::super::panels_logic::{
    BackgroundActivityOverridePatch, PROVIDER_HEALTH_INTERVAL_STEP_SECONDS,
    background_activity_override_settings, duration_to_seconds, normalize_interval_seconds,
};
use super::super::provider_form::{
    EnvironmentVariable, FieldValue, ProviderEnvironmentField, ProviderSettingsFieldModel,
    derive_provider_models_for_display, derive_provider_settings_fields,
    describe_model_capabilities, environment_fields_for_driver, environment_from_value,
    environment_to_value, group_models_for_display, is_valid_environment_variable_name,
    model_list_changes, next_config_blob_with_value, next_hidden_models_for_bulk_toggle,
    next_provider_config_with_field_value, next_provider_environment_with_field_value,
    provider_environment_without_names, read_config_custom_models, read_provider_config_boolean,
    read_provider_config_string, read_provider_environment_variable, redact_saved_environment,
};
use super::super::provider_meta::driver_label;
use super::super::provider_settings_logic::build_provider_instance_override_patch;
use super::super::provider_status::{
    AdvisoryEmphasis, get_provider_summary, get_provider_version_advisory_presentation,
    get_provider_version_label,
};
use super::super::scope::LOCAL_ENVIRONMENT_ID;
use super::super::search::searchable_setting;
use super::super::time_format::{RelativeTimeState, get_relative_time_state};
use super::super::view::{SettingsToastKind, SettingsView, Which};
use super::super::view_model::ProviderInstanceMutation;
use crate::components::provider_update_launch_notification_logic::is_provider_update_active;
use crate::components::provider_updates_action::{
    ProviderUpdateEnvironmentInput, provider_update_action_tooltip, provider_update_machines,
    show_provider_updates_action,
};
use crate::pull_request::presentation_view::Spinner;
use crate::state::contracts::EnvironmentId;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{self, RADIUS_LG, RADIUS_MD, TEXT_2XS, TEXT_3XS, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip, scroll_area};
use turin_shared::model::CustomModelDefinition;

/// Per-page UI state of the Providers page.
#[derive(Default)]
pub struct ProvidersState {
    pub selected: Option<String>,
    /// `targetInstanceId`: the instance a link or search asked to open.
    pub target: Option<String>,
    /// Instance rows whose account email is revealed (`RedactedSensitiveText`).
    pub revealed_emails: std::collections::HashSet<String>,
    pub model_filter: String,
    pub editor_scroll: gpui_kit::ScrollHandle,
    pub demo_editor_scroll: Option<(f32, u8)>,
    pub adding_model: bool,
    pub model_input: String,
    pub model_error: Option<String>,
    pub editing_model: Option<String>,
    pub custom_model_draft: Option<super::super::custom_model_editor_logic::CustomModelDraft>,
    pub custom_model_editor_error: Option<String>,
    /// `ProviderAuthenticationSection` state per instance.
    pub auth_ui: std::collections::HashMap<
        String,
        super::super::provider_authentication::ProviderAuthUiState,
    >,
    /// `ManagedCodexSetup` state per instance.
    pub codex:
        std::collections::HashMap<String, super::super::codex_setup_logic::ManagedCodexState>,
    /// `ProviderAccentColorPicker` state per picker.
    pub accent: super::super::provider_accent_color_picker::AccentPickerStates,
    /// `AddCodexAccountDialog` state.
    pub add_codex: super::super::add_codex_account_dialog::AddCodexAccountState,
    /// `ProviderAuthCallbackCoordinator`'s `started` ref.
    pub auth_delivery_started: bool,
    pub wizard: super::add_provider::AddProviderState,
    pub env_drafts:
        std::collections::HashMap<String, Vec<super::super::provider_form::EnvironmentVariable>>,
}

/// `updateProgress` of `ProviderInstanceCard`: the running update's message,
/// "Starting update" before the server's first report, `None` when idle.
fn update_progress_message(provider: &ServerProvider) -> Option<String> {
    use turin_provider_core::server_provider::ServerProviderUpdateStatus as UpdateStatus;
    if !is_provider_update_active(provider) {
        return None;
    }
    let state = provider.update_state.as_ref()?;
    Some(
        matches!(state.status, UpdateStatus::Queued | UpdateStatus::Running)
            .then(|| {
                state
                    .message
                    .as_ref()
                    .map(|message| message.as_str().to_owned())
            })
            .flatten()
            .unwrap_or_else(|| "Starting update".to_owned()),
    )
}

/// `ProjectOverrideControl`: what the card needs when a project is in scope.
struct ProviderProjectOverride {
    /// Every checkout of the project shares the same choice.
    project_ids: Vec<ProjectId>,
    /// The project's explicit choice (`None` inherits the device).
    value: Option<bool>,
    /// What the switch shows.
    effective: bool,
    /// The device's own setting.
    device: bool,
}

pub(crate) struct InstanceRow {
    pub(crate) id: String,
    pub(crate) driver: String,
    is_default: bool,
    is_dirty: bool,
    pub(crate) instance: ProviderInstanceConfig,
}

pub(crate) fn instance_enabled(row: &InstanceRow) -> bool {
    resolve_provider_instance_enabled(&row.instance)
}

/// `PROVIDER_STATUS_STYLES[..].dot`.
fn status_dot_color(status: ProviderStatus, theme: &AppTheme) -> Hsla {
    match status {
        ProviderStatus::Disabled => with_alpha(theme.muted_foreground, 0.5).into(),
        ProviderStatus::Error => theme.destructive,
        ProviderStatus::Ready => theme.success,
        ProviderStatus::Warning => theme.warning,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProviderStatus {
    Disabled,
    Error,
    Ready,
    Warning,
}

pub(super) fn provider_brand_icon(driver: &str, size: gpui_kit::Rems, tint: Hsla) -> AnyElement {
    match super::super::icons::provider_icon_path(driver) {
        Some(path) => Icon::path(path).size(size).color(tint).into_any_element(),
        None => Icon::new(IconName::Bot)
            .size(size)
            .color(tint)
            .into_any_element(),
    }
}

/// The instance rows of `ProviderSettingsPanel`: the default slots worth
/// showing, each followed by its custom instances, then instances of drivers
/// without a slot. Codex and Claude always show; another default slot shows
/// once configured, enabled, or asked for by `target_instance_id`.
pub(crate) fn build_instance_rows(
    instances: &turin_contracts::provider_instance::ProviderInstanceConfigMap,
    live: &[ServerProvider],
    target_instance_id: Option<&str>,
) -> Vec<InstanceRow> {
    let mut rows: Vec<InstanceRow> = Vec::new();
    let visible_drivers = ["codex", "claudeAgent", "cursor", "opencode"];
    for driver in visible_drivers {
        let default_id = driver;
        let explicit = instances
            .iter()
            .find(|(id, _)| id.as_str() == default_id)
            .map(|(_, instance)| instance.clone());
        let is_dirty = explicit.is_some();
        // An unconfigured default slot runs with the driver's default config.
        let effective = explicit.unwrap_or_else(|| {
            serde_json::from_value(json!({ "driver": driver })).expect("driver envelope")
        });
        // Cursor appears once the server reports its default instance.
        let has_default_slot = driver != "cursor"
            || live.iter().any(|p| p.instance_id.as_str() == default_id)
            || is_dirty;
        if has_default_slot
            && (driver == "codex"
                || driver == "claudeAgent"
                || is_dirty
                || resolve_provider_instance_enabled(&effective)
                || target_instance_id == Some(default_id))
        {
            rows.push(InstanceRow {
                id: default_id.to_owned(),
                driver: driver.to_owned(),
                is_default: true,
                is_dirty,
                instance: effective,
            });
        }
        for (id, instance) in instances {
            if id.as_str() != default_id && instance.driver.as_str() == driver {
                rows.push(InstanceRow {
                    id: id.as_str().to_owned(),
                    driver: driver.to_owned(),
                    is_default: false,
                    is_dirty: false,
                    instance: instance.clone(),
                });
            }
        }
    }
    for (id, instance) in instances {
        if !visible_drivers.contains(&instance.driver.as_str()) {
            rows.push(InstanceRow {
                id: id.as_str().to_owned(),
                driver: instance.driver.as_str().to_owned(),
                is_default: visible_drivers.contains(&id.as_str()),
                is_dirty: false,
                instance: instance.clone(),
            });
        }
    }
    rows
}

/// The row the editor shows: the selected one, else the first, except that a
/// requested instance that is gone leaves the editor empty
/// (`targetInstanceMissing`) rather than showing another account.
pub(crate) fn selected_instance_row<'a>(
    rows: &'a [InstanceRow],
    selected: Option<&str>,
    target_instance_id: Option<&str>,
) -> Option<&'a InstanceRow> {
    let target_missing = target_instance_id.is_some()
        && selected == target_instance_id
        && !rows
            .iter()
            .any(|row| Some(row.id.as_str()) == target_instance_id);
    selected
        .and_then(|selected| rows.iter().find(|row| row.id == selected))
        .or_else(|| if target_missing { None } else { rows.first() })
}

impl SettingsView {
    pub(crate) fn instance_rows(&self, cx: &gpui_kit::App) -> Vec<InstanceRow> {
        let vm = self.vm(cx);
        build_instance_rows(
            &vm.server.provider_instances,
            &vm.providers,
            self.panels.providers.target.as_deref(),
        )
    }

    pub(super) fn render_providers(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        vec![
            self.providers_section(window, cx),
            self.usage_providers_section(cx),
            self.provider_advanced_section(window, cx),
        ]
    }

    // ── patching ────────────────────────────────────────────────────────

    /// Writes one instance (or removes it) with the `providerInstances` map
    /// replacement the settings service expects.
    pub(in crate::settings) fn write_provider_instance(
        &mut self,
        id: &str,
        next: Option<Value>,
        clear_text_generation: bool,
        cx: &mut Context<Self>,
    ) {
        let mutation = match next {
            Some(instance) => ProviderInstanceMutation::Upsert {
                instance_id: id.to_owned(),
                instance,
            },
            None => ProviderInstanceMutation::Remove {
                instance_id: id.to_owned(),
            },
        };
        let patch = clear_text_generation.then(|| {
            let default = self
                .json(cx)
                .default_of(Which::Server, "textGenerationModelSelection")
                .clone();
            json!({ "textGenerationModelSelection": default })
        });
        let source = self.store.read(cx).source();
        source.mutate_provider_instance(mutation, patch, cx);
    }

    fn instance_value(row: &InstanceRow) -> Value {
        serde_json::to_value(&row.instance).unwrap_or(Value::Null)
    }

    /// Replaces keys of an instance's envelope: `Some` sets, `None` removes.
    pub(crate) fn update_instance(
        &mut self,
        row: &InstanceRow,
        patch: Vec<(&str, Option<Value>)>,
        cx: &mut Context<Self>,
    ) {
        let mut value = Self::instance_value(row);
        let was_enabled = instance_enabled(row);
        let mut disabling = false;
        if let Some(object) = value.as_object_mut() {
            for (key, entry) in patch {
                if key == "enabled" && entry == Some(Value::Bool(false)) && was_enabled {
                    disabling = true;
                }
                match entry {
                    Some(entry) => {
                        object.insert(key.to_owned(), entry);
                    }
                    None => {
                        object.remove(key);
                    }
                }
            }
        }
        let vm = self.vm(cx);
        let text_gen_uses_row = vm
            .server
            .text_generation_model_selection
            .instance_id
            .as_str()
            == row.id;
        self.write_provider_instance(&row.id, Some(value), disabling && text_gen_uses_row, cx);
    }

    // ── section ─────────────────────────────────────────────────────────

    fn providers_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let vm = self.vm(cx);
        let rows = self.instance_rows(cx);
        let selected_id = selected_instance_row(
            &rows,
            self.panels.providers.selected.as_deref(),
            self.panels.providers.target.as_deref(),
        )
        .map(|row| row.id.clone());
        let target_missing = selected_id.is_none() && self.panels.providers.target.is_some();

        let last_checked = vm.providers.iter().map(|p| p.checked_at.clone()).max();
        let checked_label =
            match get_relative_time_state(last_checked.as_deref(), chrono::Utc::now()) {
                RelativeTimeState::Missing => String::new(),
                RelativeTimeState::Invalid => "Checked unavailable".to_owned(),
                RelativeTimeState::Relative(parts) => match parts.suffix {
                    Some(suffix) => format!("Checked {} {suffix}", parts.value),
                    None => format!("Checked {}", parts.value),
                },
            };
        // `disabled={isRefreshingProviders}`; the label turns into "Refreshing providers".
        let refreshing = vm.providers_refreshing;
        let refresh = Button::new("refresh-providers")
            .variant(ButtonVariant::GhostMuted)
            .size(ButtonSize::Xs)
            .icon(IconName::RefreshCw)
            .disabled(refreshing)
            .label(if refreshing {
                "Refreshing providers".to_owned()
            } else {
                checked_label
            })
            .tooltip(Tooltip::new("Refresh provider status"))
            .on_click(cx.listener(|this, _, _, cx| {
                let source = this.store.read(cx).source();
                source.refresh_providers(cx);
            }));
        // `ProviderUpdatesAction`: every outdated provider with a one-click
        // update; it stays while the updates run, since candidates leave the
        // list once their servers report them queued.
        let update_machines = provider_update_machines(&[ProviderUpdateEnvironmentInput {
            environment_id: EnvironmentId::new(LOCAL_ENVIRONMENT_ID).expect("environment id"),
            label: vm.environment.label.clone(),
            connected: true,
            providers: vm.providers.clone(),
        }]);
        let updates_pending = vm.providers.iter().any(is_provider_update_active);
        let update_all =
            show_provider_updates_action(&update_machines, updates_pending).then(|| {
                let instance_ids: Vec<String> = update_machines
                    .iter()
                    .flat_map(|machine| machine.candidates.iter())
                    .map(|candidate| candidate.instance_id.as_str().to_owned())
                    .collect();
                Button::new("update-all-providers")
                    .variant(ButtonVariant::GhostMuted)
                    .size(ButtonSize::Xs)
                    .disabled(updates_pending)
                    .label(if updates_pending {
                        "Updating…"
                    } else {
                        "Update all"
                    })
                    .tooltip(Tooltip::new(provider_update_action_tooltip(
                        &update_machines,
                    )))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let source = this.store.read(cx).source();
                        for instance_id in &instance_ids {
                            source.update_provider(instance_id, None, cx);
                        }
                    }))
            });
        let add_button = Button::new("add-provider-title")
            .variant(ButtonVariant::GhostMuted)
            .size(ButtonSize::IconXs)
            .icon(IconName::Plus)
            .tooltip(Tooltip::new("Add provider"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.open_add_provider(cx);
            }));

        // List column.
        let mut list = div().flex().flex_col().children(
            rows.iter()
                .enumerate()
                .map(|(index, row)| {
                    let selected = selected_id.as_deref() == Some(row.id.as_str());
                    self.provider_list_row(row, selected, index > 0, &vm.providers, cx)
                })
                .collect::<Vec<_>>(),
        );
        list = list.child(
            div()
                .id("add-provider-row")
                .flex()
                .w_full()
                .items_center()
                .gap_3()
                .px_4()
                .py_3()
                .border_t_1()
                .border_color(with_alpha(theme.border, 0.5))
                .text_size(TEXT_SM.size)
                .text_color(theme.muted_foreground)
                .cursor(CursorStyle::PointingHand)
                .hover(|style| {
                    style
                        .bg(with_alpha(theme.muted, 0.25))
                        .text_color(theme.foreground)
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.open_add_provider(cx);
                }))
                .child(
                    Icon::new(IconName::Plus)
                        .size(rems(1.))
                        .color(theme.muted_foreground),
                )
                .child("Add provider"),
        );

        let editor: AnyElement = match rows
            .iter()
            .find(|row| Some(&row.id) == selected_id.as_ref())
        {
            Some(row) => self.provider_editor(row, &vm.providers, window, cx),
            None => div()
                .p_6()
                .text_size(TEXT_SM.size)
                .text_color(theme.muted_foreground)
                .child(if target_missing {
                    "This provider instance is no longer available on this device."
                } else {
                    "No providers configured."
                })
                .into_any_element(),
        };

        let editor_scroll = self.panels.providers.editor_scroll.clone();
        if let Some((offset, frames)) = self.panels.providers.demo_editor_scroll {
            editor_scroll.set_offset(gpui_kit::point(px(0.), px(-offset)));
            self.panels.providers.demo_editor_scroll = (frames > 1).then_some((offset, frames - 1));
            cx.notify();
        }
        // `@min-[48rem]/providers:grid grid-cols-[17rem_minmax(0,1fr)]`, height
        // `min(44rem, 100dvh - 11rem)` with a 32rem floor.
        let height = (f32::from(window.viewport_size().height) - 176.).clamp(512., 704.);
        let card = settings_group(
            vec![
                div()
                    .flex()
                    .flex_row()
                    .h(px(height))
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_shrink_0()
                            .w(rems(17.))
                            .h_full()
                            .min_h_0()
                            .border_r_1()
                            .border_color(with_alpha(theme.border, 0.6))
                            .bg(with_alpha(theme.muted, 0.1))
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .child(scroll_area("providers-list").child(list)),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .min_h_0()
                            .relative()
                            .child(
                                div()
                                    .id("providers-editor")
                                    .size_full()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .track_scroll(&editor_scroll)
                                    .child(div().flex().flex_col().gap_6().p_4().child(editor))
                                    .vertical_scrollbar(&editor_scroll),
                            ),
                    )
                    .into_any_element(),
            ],
            cx,
        );
        let (id, title) = searchable_setting("providers");
        SettingsSection::new(title)
            .id(id)
            .plain()
            .title_action(add_button)
            .header_action(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .children(update_all)
                    .child(refresh),
            )
            .child(card)
            .into_any_element()
    }

    fn provider_status_of(
        &self,
        row: &InstanceRow,
        live: Option<&ServerProvider>,
        project: Option<&ProviderProjectOverride>,
    ) -> (bool, ProviderStatus) {
        let enabled = project
            .map(|project| project.effective)
            .unwrap_or_else(|| instance_enabled(row));
        let status = if enabled {
            match live.map(|p| p.status) {
                Some(ServerProviderState::Ready) => ProviderStatus::Ready,
                Some(ServerProviderState::Error) => ProviderStatus::Error,
                Some(ServerProviderState::Disabled) => ProviderStatus::Disabled,
                _ => ProviderStatus::Warning,
            }
        } else {
            ProviderStatus::Disabled
        };
        (enabled, status)
    }

    fn display_name(row: &InstanceRow) -> String {
        row.instance
            .display_name
            .as_ref()
            .map(|name| name.as_str().trim().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| driver_label(&row.driver))
    }

    /// ProviderInstanceCard `mode="list"`.
    fn provider_list_row(
        &mut self,
        row: &InstanceRow,
        selected: bool,
        divided: bool,
        providers: &[ServerProvider],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let live = providers.iter().find(|p| p.instance_id.as_str() == row.id);
        let project = self.provider_project_override(row, cx);
        let (enabled, status) = self.provider_status_of(row, live, project.as_ref());
        let summary = {
            let mut provider = live.cloned();
            if let (true, Some(provider)) = (enabled, provider.as_mut()) {
                provider.enabled = true;
            }
            if enabled {
                get_provider_summary(provider.as_ref())
            } else {
                super::super::provider_status::ProviderSummary {
                    headline: "Disabled".into(),
                    detail: None,
                }
            }
        };
        let version =
            get_provider_version_label(live.and_then(|p| p.version.as_ref()).map(|v| v.as_str()));
        let needs_attention = matches!(status, ProviderStatus::Warning | ProviderStatus::Error);
        let display_name = Self::display_name(row);
        let id = row.id.clone();
        let select_id = id.clone();
        let advisory = live.and_then(|provider| {
            get_provider_version_advisory_presentation(
                provider.version_advisory.as_ref(),
                provider.compatibility_advisory.as_ref(),
                enabled,
            )
        });
        let advisory_icon = advisory.map(|advisory| {
            self.version_advisory_popover(&id, &display_name, advisory, live, true, cx)
        });

        let inline_detail = if needs_attention {
            summary.detail.clone()
        } else {
            None
        };
        let update_progress = live.and_then(update_progress_message);
        let status_line = div()
            .mt(px(2.))
            .flex()
            .items_start()
            .gap(rems(0.375))
            .text_size(TEXT_XS.size)
            .line_height(rems(1.))
            .text_color(with_alpha(theme.muted_foreground, 0.8))
            .when(needs_attention, |this| {
                this.child(
                    div()
                        .flex()
                        .h(rems(1.0))
                        .flex_shrink_0()
                        .items_center()
                        .child(
                            div()
                                .size(px(6.))
                                .rounded_full()
                                .bg(status_dot_color(status, &theme)),
                        ),
                )
            })
            .child(
                div()
                    .min_w_0()
                    .child(match (&update_progress, inline_detail) {
                        (Some(progress), _) => format!("Updating · {progress}"),
                        (None, Some(detail)) => format!("{} · {detail}", summary.headline),
                        (None, None) => summary.headline.clone(),
                    }),
            );

        let toggle = {
            let entity = cx.entity();
            let row_id = row.id.clone();
            let project_ids = project.as_ref().map(|project| project.project_ids.clone());
            switch(
                SharedString::from(format!("provider-enable-{id}")),
                enabled,
                false,
                cx,
                move |next, _, cx| {
                    entity.update(cx, |this, cx| {
                        // `updateEnabled`: with a project in scope the switch
                        // writes that project's override.
                        if let Some(project_ids) = &project_ids {
                            this.write_provider_project_override(
                                project_ids,
                                &row_id,
                                Some(next),
                                cx,
                            );
                            return;
                        }
                        let rows = this.instance_rows(cx);
                        if let Some(row) = rows.iter().find(|row| row.id == row_id) {
                            this.update_instance(
                                row,
                                vec![("enabled", Some(Value::Bool(next)))],
                                cx,
                            );
                        }
                    });
                },
            )
        };
        // `projectOverride?.value !== undefined`: the layers icon and the reset.
        let override_controls = project
            .as_ref()
            .filter(|project| project.value.is_some())
            .map(|project| {
                let reset = {
                    let entity = cx.entity();
                    let row_id = row.id.clone();
                    let project_ids = project.project_ids.clone();
                    setting_reset_button(
                        SharedString::from(format!("reset-provider-override-{id}")),
                        "Reset to the device setting",
                        move |_, cx| {
                            entity.update(cx, |this, cx| {
                                this.write_provider_project_override(
                                    &project_ids,
                                    &row_id,
                                    None,
                                    cx,
                                );
                            });
                        },
                    )
                };
                div()
                    .flex()
                    .items_center()
                    .child(self.project_override_inheritance(
                        &id,
                        project.value.unwrap_or(project.effective),
                        project.device,
                        cx,
                    ))
                    .child(reset)
            });

        div()
            .id(SharedString::from(format!("provider-row-{id}")))
            .flex()
            .min_h(rems(4.5))
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .cursor(CursorStyle::PointingHand)
            .when(divided, |this| {
                this.border_t_1()
                    .border_color(with_alpha(theme.border, 0.5))
            })
            .when(selected, |this| this.bg(with_alpha(theme.muted, 0.45)))
            .when(!selected, |this| {
                this.hover(|style| style.bg(with_alpha(theme.muted, 0.25)))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.panels.providers.selected = Some(select_id.clone());
                this.menus.close();
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .flex_1()
                    .items_start()
                    .gap_3()
                    .when(!enabled && !selected, |this| this.opacity(0.6))
                    .child(
                        div()
                            .flex()
                            .size_5()
                            .flex_shrink_0()
                            .items_center()
                            .justify_center()
                            .child(provider_brand_icon(
                                &row.driver,
                                rems(1.),
                                with_alpha(theme.foreground, 0.8).into(),
                            )),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .flex()
                                    .min_w_0()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(TEXT_SM.size)
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.foreground)
                                            .child(display_name),
                                    )
                                    .children(version.map(|version| {
                                        div()
                                            .max_w(rems(6.))
                                            .flex_shrink_0()
                                            .truncate()
                                            .font_family(theme.fonts.mono.clone())
                                            .text_size(TEXT_XS.size)
                                            .text_color(theme.muted_foreground)
                                            .child(version)
                                    }))
                                    .children(advisory_icon),
                            )
                            .child(status_line),
                    ),
            )
            .child(
                div()
                    .flex()
                    .h_5()
                    .flex_shrink_0()
                    .items_center()
                    .gap_1()
                    .children(override_controls)
                    .child(toggle),
            )
            .into_any_element()
    }

    // ── project override ────────────────────────────────────────────────

    /// `projectOverride` of `ProviderSettingsPanel`: present when a project is
    /// in scope, `None` for environment-wide editing.
    fn provider_project_override(
        &self,
        row: &InstanceRow,
        cx: &gpui_kit::App,
    ) -> Option<ProviderProjectOverride> {
        let context = self.scope_context(cx);
        if !context.is_project_scope() {
            return None;
        }
        let project_ids: Vec<ProjectId> = context
            .targets
            .iter()
            .filter_map(|target| target.project_id.clone())
            .collect();
        let representative = project_ids.first()?;
        let instance_id = ProviderInstanceId::new(&row.id).ok()?;
        let server = &self.vm(cx).server;
        let value = server
            .project_settings_overrides
            .get(representative)
            .and_then(|overrides| overrides.provider_instances.as_ref())
            .and_then(|instances| instances.get(&instance_id))
            .and_then(|instance| instance.enabled);
        Some(ProviderProjectOverride {
            effective: resolve_project_provider_instance_enabled(
                &server.provider_instances,
                &server.project_settings_overrides,
                Some(representative),
                &instance_id,
            ),
            device: instance_enabled(row),
            value,
            project_ids,
        })
    }

    /// `writeProjectOverride`: `Some` sets the project's choice, `None` resets
    /// it back to the device's.
    fn write_provider_project_override(
        &mut self,
        project_ids: &[ProjectId],
        row_id: &str,
        next: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        let Ok(instance_id) = ProviderInstanceId::new(row_id) else {
            return;
        };
        let patch = build_provider_instance_override_patch(
            &self.vm(cx).server,
            project_ids,
            &instance_id,
            next,
        );
        if let Ok(overrides) = serde_json::to_value(&patch) {
            self.update_server(json!({ "projectSettingsOverrides": overrides }), cx);
        }
    }

    /// `ProjectOverrideInheritance`: the layers icon; open it for the project
    /// and device values side by side.
    fn project_override_inheritance(
        &mut self,
        row_id: &str,
        project_enabled: bool,
        device_enabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let menu_id = SharedString::from(format!("provider-override-{row_id}"));
        let open = self.menus.is_open(&menu_id);
        let trigger = Button::new(SharedString::from(format!(
            "provider-override-trigger-{row_id}"
        )))
        .variant(ButtonVariant::GhostMuted)
        .size(ButtonSize::IconMicro)
        .tooltip(Tooltip::new("Overridden for this project"))
        .child(
            Icon::new(IconName::Layers)
                .size(rems(0.75))
                .color(theme.primary),
        )
        .on_click(toggle_menu(cx, |this| &mut this.menus, menu_id.clone()));
        let popup = open.then(|| {
            let layers = [
                ("Project", project_enabled, true),
                ("Device", device_enabled, false),
            ];
            let mut list = div().flex().flex_col().px_3().py(rems(0.625));
            for (label, enabled, effective) in layers {
                list = list.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .rounded(RADIUS_MD)
                        .px_2()
                        .py_1()
                        .text_size(TEXT_SM.size)
                        .when(effective, |this| {
                            this.bg(with_alpha(theme.foreground, 0.06))
                        })
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .when(effective, |this| {
                                    this.font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.foreground)
                                })
                                .when(!effective, |this| this.text_color(theme.muted_foreground))
                                .child(label),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(rems(0.375))
                                .text_color(if effective {
                                    theme.foreground
                                } else {
                                    theme.muted_foreground
                                })
                                .child(if enabled { "On" } else { "Off" })
                                .child(if effective {
                                    Icon::new(IconName::Check)
                                        .size(rems(0.875))
                                        .color(theme.primary)
                                        .into_any_element()
                                } else {
                                    div().size(rems(0.875)).into_any_element()
                                }),
                        ),
                );
            }
            div()
                .id(SharedString::from(format!(
                    "provider-override-popup-{row_id}"
                )))
                .occlude()
                .w(rems(16.))
                .rounded(RADIUS_LG)
                .border_1()
                .border_color(crate::ui::tooltip::dropdown_glass_border(&theme))
                .bg(theme.popover)
                .shadow(tokens::shadow_popover(theme.is_dark()))
                .text_color(theme.popover_foreground)
                .child(list)
                .into_any_element()
        });
        with_popup(trigger, popup, PopupAlign::End, px(20.)).into_any_element()
    }

    // ── editor ──────────────────────────────────────────────────────────

    fn provider_editor(
        &mut self,
        row: &InstanceRow,
        providers: &[ServerProvider],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let live = providers
            .iter()
            .find(|p| p.instance_id.as_str() == row.id)
            .cloned();
        let project = self.provider_project_override(row, cx);
        let (enabled, status) = self.provider_status_of(row, live.as_ref(), project.as_ref());
        let display_name = Self::display_name(row);
        let version = get_provider_version_label(
            live.as_ref()
                .and_then(|p| p.version.as_ref())
                .map(|v| v.as_str()),
        );
        let config = row.instance.config.clone();

        // Header action: version, advisory popover, delete.
        let advisory = live.as_ref().and_then(|provider| {
            get_provider_version_advisory_presentation(
                provider.version_advisory.as_ref(),
                provider.compatibility_advisory.as_ref(),
                enabled,
            )
        });
        let advisory_node = advisory.map(|advisory| {
            self.version_advisory_popover(
                &row.id,
                &display_name,
                advisory,
                live.as_ref(),
                false,
                cx,
            )
        });
        let delete = (!row.is_default).then(|| {
            let id = row.id.clone();
            Button::new(SharedString::from(format!("delete-instance-{id}")))
                .variant(ButtonVariant::GhostDestructive)
                .size(ButtonSize::IconXs)
                .icon(IconName::Trash)
                .tooltip(Tooltip::new(format!("Delete instance {id}")))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let delete_id = id.clone();
                    this.dialog = Some(super::super::dialog::Dialog::Confirm(
                        super::super::dialog::ConfirmSpec {
                            title: format!("Delete {delete_id}?").into(),
                            description:
                                "This removes the provider instance and its configuration.".into(),
                            confirm_label: "Delete".into(),
                            destructive: true,
                            on_confirm: Rc::new(move |this, _, cx| {
                                this.write_provider_instance(&delete_id, None, false, cx)
                            }),
                        },
                    ));
                    cx.notify();
                }))
        });
        let reset = (row.is_default && row.is_dirty).then(|| {
            let id = row.id.clone();
            let action = self.cb(cx, move |this, _, cx| {
                this.write_provider_instance(&id, None, false, cx)
            });
            div()
                .flex()
                .size_5()
                .items_center()
                .justify_center()
                .child(setting_reset_button(
                    "reset-provider-instance",
                    "Reset to default",
                    move |w, c| action(w, c),
                ))
        });
        let header_action = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(rems(0.375))
            .children(version.clone().map(|version| {
                div()
                    .font_family(theme.fonts.mono.clone())
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(version)
            }))
            .children(advisory_node)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .children(reset)
                    .children(delete),
            );

        // Display name row.
        let status_text = {
            let mut provider = live.clone();
            if let (true, Some(provider)) = (enabled, provider.as_mut()) {
                provider.enabled = true;
            }
            let summary = get_provider_summary(provider.as_ref());
            let email = live
                .as_ref()
                .filter(|_| enabled)
                .filter(|p| p.auth.status == ServerProviderAuthStatus::Authenticated)
                .and_then(|p| p.auth.email.as_ref())
                .map(|email| email.as_str().to_owned());
            let label = live
                .as_ref()
                .filter(|_| enabled)
                .filter(|p| p.auth.status == ServerProviderAuthStatus::Authenticated)
                .and_then(|p| p.auth.label.as_ref().or(p.auth.type_.as_ref()))
                .map(|label| label.as_str().to_owned());
            let mut parts = div()
                .flex()
                .flex_wrap()
                .items_baseline()
                .gap_x(rems(0.375))
                .min_w_0();
            let needs_attention = matches!(status, ProviderStatus::Warning | ProviderStatus::Error);
            if needs_attention {
                parts = parts.child(
                    div()
                        .size(px(6.))
                        .rounded_full()
                        .bg(status_dot_color(status, &theme)),
                );
            }
            match (email,) {
                (Some(email),) => {
                    let revealed = self.panels.providers.revealed_emails.contains(&row.id);
                    let toggle_id = row.id.clone();
                    let on_toggle = self.cb(cx, move |this, _, cx| {
                        let revealed = &mut this.panels.providers.revealed_emails;
                        if !revealed.remove(&toggle_id) {
                            revealed.insert(toggle_id.clone());
                        }
                        cx.notify();
                    });
                    let email_node = redacted_sensitive_text(
                        SharedString::from(format!("provider-email-{}", row.id)),
                        &email,
                        revealed,
                        cx,
                        on_toggle,
                    );
                    parts = parts.child("Authenticated as").child(email_node);
                    if let Some(label) = label {
                        parts = parts.child(format!("· {label}"));
                    }
                }
                _ => {
                    parts = parts.child(summary.headline.clone());
                    if let Some(detail) = summary.detail.clone() {
                        parts = parts.child(format!("· {detail}"));
                    }
                }
            }
            parts
        };
        let accent = row
            .instance
            .accent_color
            .as_ref()
            .map(|color| color.as_str().to_owned());
        let name_input = {
            let row_value = row
                .instance
                .display_name
                .as_ref()
                .map(|n| n.as_str().to_owned())
                .unwrap_or_default();
            let id = row.id.clone();
            self.input(
                &format!("provider-name-{id}"),
                &row_value,
                &driver_label(&row.driver),
                window,
                cx,
                move |this, text, _, cx| {
                    let rows = this.instance_rows(cx);
                    if let Some(row) = rows.iter().find(|row| row.id == id) {
                        let trimmed = text.trim().to_owned();
                        this.update_instance(
                            row,
                            vec![(
                                "displayName",
                                (!trimmed.is_empty()).then(|| Value::String(trimmed)),
                            )],
                            cx,
                        );
                    }
                },
            )
        };
        let accent_picker =
            self.accent_picker(&row.id, accent.as_deref(), &display_name, window, cx);
        let name_control = div()
            .flex()
            .min_w_0()
            .items_center()
            .justify_end()
            .gap_2()
            .child(accent_picker)
            .child(text_field(
                &name_input,
                Some(rems(14.).to_pixels(px(16.))),
                FieldSize::Sm,
                false,
                cx,
            ));
        let display_row = SettingsRow::new("Display name")
            .status(status_text)
            .control(name_control);
        let title_icon = provider_brand_icon(
            &row.driver,
            rems(1.),
            with_alpha(theme.foreground, 0.8).into(),
        );
        let header = SettingsSection::new(display_name.clone())
            .icon(
                div()
                    .flex()
                    .size_5()
                    .items_center()
                    .justify_center()
                    .child(title_icon),
            )
            .header_action(header_action)
            .child(display_row);

        let mut sections: Vec<AnyElement> = vec![header.into_any_element()];

        // Setup: account + dedicated environment fields.
        let env_fields = environment_fields_for_driver(&row.driver);
        let environment = environment_from_value(
            serde_json::to_value(&row.instance)
                .ok()
                .and_then(|v| v.get("environment").cloned())
                .as_ref(),
        );
        let codex_managed = row.driver == "codex"
            && read_codex_setup_mode(config.as_ref()) == CodexSetupMode::Managed;
        let setup_node: Option<AnyElement> = if codex_managed {
            Some(self.codex_setup_section(row, live.as_ref(), false, window, cx))
        } else if let Some(provider) = live.as_ref().filter(|provider| {
            provider
                .setup
                .as_ref()
                .is_some_and(|setup| setup.can_authenticate)
        }) {
            Some(self.provider_authentication_section(&row.id, provider, false, window, cx))
        } else if row.driver == "cursor"
            && live
                .as_ref()
                .and_then(|provider| provider.setup.as_ref())
                .is_some_and(|setup| !setup.can_authenticate)
        {
            Some(
                SettingsRow::new("Cursor account")
                    .description("Using CURSOR_API_KEY. Remove it from this provider's environment to use browser sign-in.")
                    .into_any_element(),
            )
        } else {
            None
        };
        if setup_node.is_some() || !env_fields.is_empty() {
            let mut setup = SettingsSection::new("Setup");
            if let Some(node) = setup_node {
                setup = setup.child(node);
            }
            for field in env_fields {
                let variable =
                    read_provider_environment_variable(&environment, field.name).cloned();
                setup = setup.child(self.environment_field_row(
                    row,
                    field,
                    variable.as_ref(),
                    &environment,
                    window,
                    cx,
                ));
            }
            sections.push(setup.into_any_element());
        }

        // Runtime: the driver's settings form.
        if codex_managed {
            let fields = self.codex_managed_runtime_fields(&row.id, cx);
            sections.push(
                self.folded_settings_section(
                    FoldedSection::new(format!("provider-instance-{}-runtime", row.id), "Runtime")
                        .placement(FoldedHeaderPlacement::Outside),
                    fields,
                    cx,
                ),
            );
        } else {
            let fields = derive_provider_settings_fields(&row.driver);
            if !fields.is_empty() {
                let mut runtime = SettingsSection::new("Runtime");
                for field in &fields {
                    runtime = runtime.child(self.provider_field_row(
                        row,
                        field,
                        config.as_ref(),
                        window,
                        cx,
                    ));
                }
                sections.push(runtime.into_any_element());
            }
        }

        // Environment.
        let dedicated: std::collections::HashSet<&str> =
            env_fields.iter().map(|f| f.name).collect();
        let generic = provider_environment_without_names(&environment, &dedicated);
        sections.push(
            SettingsSection::new("Environment")
                .child(self.environment_variables_row(
                    row,
                    &generic,
                    &environment,
                    &dedicated,
                    window,
                    cx,
                ))
                .into_any_element(),
        );

        // Models.
        sections.push(self.models_section(row, live.as_ref(), config.as_ref(), window, cx));
        div()
            .flex()
            .flex_col()
            .gap_6()
            .children(sections)
            .into_any_element()
    }

    fn version_advisory_popover(
        &mut self,
        id: &str,
        display_name: &str,
        advisory: super::super::provider_status::VersionAdvisoryPresentation,
        live: Option<&ServerProvider>,
        list_mode: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use turin_provider_core::server_provider::ServerProviderUpdateStatus as UpdateStatus;
        let theme = cx.app_theme().clone();
        let menu_id = SharedString::from(format!("advisory-{id}"));
        let open = self.menus.is_open(&menu_id);
        let toggle_id = menu_id.clone();
        let has_warning = advisory.emphasis == AdvisoryEmphasis::Strong;
        // `isUpdating`, `updateProgress` and `updateProblem` of the card: the
        // server reports each update step; an active update with no message
        // yet reads "Starting update".
        let update_state = live.and_then(|provider| provider.update_state.as_ref());
        let updating = live.is_some_and(is_provider_update_active);
        let state_message = update_state
            .and_then(|state| state.message.as_ref())
            .map(|message| message.as_str().to_owned());
        let progress = updating.then(|| {
            update_state
                .filter(|state| {
                    matches!(state.status, UpdateStatus::Queued | UpdateStatus::Running)
                })
                .and_then(|_| state_message.clone())
                .unwrap_or_else(|| "Starting update".to_owned())
        });
        let problem = (!updating)
            .then(|| {
                update_state
                    .filter(|state| {
                        matches!(state.status, UpdateStatus::Failed | UpdateStatus::Unchanged)
                    })
                    .and(state_message.clone())
            })
            .flatten();
        let trigger_tooltip = if progress.is_some() {
            "Updating".to_owned()
        } else {
            advisory.title.clone()
        };
        let trigger = Button::new(SharedString::from(format!("advisory-trigger-{id}")))
            .variant(ButtonVariant::GhostMuted)
            .size(if list_mode {
                ButtonSize::IconMicro
            } else {
                ButtonSize::IconXs
            })
            .tooltip(Tooltip::new(trigger_tooltip))
            .child(if progress.is_some() {
                Spinner::new(SharedString::from(format!("advisory-spin-{id}")))
                    .size(rems(if list_mode { 0.875 } else { 1. }))
                    .color(theme.muted_foreground)
                    .into_any_element()
            } else {
                Icon::new(if has_warning {
                    IconName::TriangleAlert
                } else {
                    IconName::CircleArrowUp
                })
                .size(rems(if list_mode { 0.875 } else { 1. }))
                .color(if has_warning {
                    theme.warning
                } else {
                    theme.muted_foreground
                })
                .into_any_element()
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.menus.toggle(toggle_id.clone());
                cx.notify();
            }));
        let popup = open.then(|| {
            let update_label = if updating {
                "Updating".to_owned()
            } else {
                match &advisory.target_version {
                    Some(version) => format!(
                        "Install {}",
                        get_provider_version_label(Some(version)).unwrap_or_default()
                    ),
                    None => "Update now".to_owned(),
                }
            };
            let has_command = advisory.update_command.is_some();
            let instance_id = id.to_owned();
            let target = advisory.target_version.clone();
            let command = advisory.update_command.clone();
            let name = display_name.to_owned();
            let copy_name = display_name.to_owned();
            div()
                .id(SharedString::from(format!("advisory-popup-{id}")))
                .occlude()
                .flex()
                .flex_col()
                .gap_3()
                .w(rems(20.))
                .p_4()
                .rounded(RADIUS_LG)
                .border_1()
                .border_color(crate::ui::tooltip::dropdown_glass_border(&theme))
                .bg(theme.popover)
                .shadow(tokens::shadow_popover(theme.is_dark()))
                .text_color(theme.popover_foreground)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rems(0.125))
                        .child(
                            div()
                                .text_size(TEXT_SM.size)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(advisory.title.clone()),
                        )
                        .child(self.expandable_text(
                            &format!("advisory-detail-{id}"),
                            &advisory.detail,
                            "Show full error",
                            false,
                            cx,
                        )),
                )
                .child(
                    Button::new(SharedString::from(format!("advisory-update-{id}")))
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Xs)
                        .icon(if updating {
                            IconName::LoaderCircle
                        } else {
                            IconName::Download
                        })
                        .label(update_label)
                        .disabled(updating)
                        .w_full()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let source = this.store.read(cx).source();
                            source.update_provider(&instance_id, target.clone(), cx);
                        })),
                )
                .children((progress.is_some() || problem.is_some()).then(|| {
                    div()
                        .text_size(TEXT_XS.size)
                        .line_height(rems(1.))
                        .text_color(if problem.is_some() {
                            theme.warning
                        } else {
                            theme.muted_foreground
                        })
                        .child(progress.clone().or(problem.clone()).unwrap_or_default())
                }))
                .children(has_command.then(|| {
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(TEXT_3XS.size)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted_foreground)
                        .child(div().h(px(1.)).flex_1().bg(theme.border))
                        .child("OR, UPDATE MANUALLY USING")
                        .child(div().h(px(1.)).flex_1().bg(theme.border))
                }))
                .children(command.map(|command| {
                    div()
                        .flex()
                        .min_w_0()
                        .items_center()
                        .gap_1()
                        .rounded_md()
                        .border_1()
                        .border_color(with_alpha(theme.border, 0.7))
                        .bg(with_alpha(theme.muted, 0.4))
                        .py(px(2.))
                        .pr(px(2.))
                        .pl_2()
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .font_family(theme.fonts.mono.clone())
                                .text_size(TEXT_2XS.size)
                                .child(command.clone()),
                        )
                        .child(
                            Button::new(SharedString::from(format!("advisory-copy-{name}")))
                                .variant(ButtonVariant::GhostMuted)
                                .size(ButtonSize::IconXs)
                                .icon(IconName::Copy)
                                .icon_size(rems(0.75))
                                .tooltip(Tooltip::new("Copy command"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        command.clone(),
                                    ));
                                    this.toast(
                                        SettingsToastKind::Success,
                                        format!("{copy_name} update command copied"),
                                        "Run it in a terminal when you are ready to update.",
                                        cx,
                                    );
                                })),
                        )
                }))
                .into_any_element()
        });
        with_popup(
            trigger,
            popup,
            PopupAlign::End,
            rems(1.5).to_pixels(px(16.)),
        )
        .into_any_element()
    }

    /// `ProviderAccentColorPicker layout="inline" commitDelayMs={120}` of the
    /// instance card.
    fn accent_picker(
        &mut self,
        id: &str,
        value: Option<&str>,
        display_name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row_id = id.to_owned();
        self.provider_accent_color_picker(
            id,
            display_name,
            value,
            120,
            window,
            cx,
            move |this, hex, cx| {
                let rows = this.instance_rows(cx);
                if let Some(row) = rows.iter().find(|row| row.id == row_id) {
                    this.update_instance(
                        row,
                        vec![("accentColor", (!hex.is_empty()).then(|| Value::String(hex)))],
                        cx,
                    );
                }
            },
        )
    }

    fn environment_field_row(
        &mut self,
        row: &InstanceRow,
        field: &'static ProviderEnvironmentField,
        variable: Option<&EnvironmentVariable>,
        environment: &[EnvironmentVariable],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = variable
            .map(|v| {
                if v.value_redacted == Some(true) {
                    String::new()
                } else {
                    v.value.clone()
                }
            })
            .unwrap_or_default();
        let configured = variable.is_some_and(|v| v.value_redacted == Some(true));
        let id = row.id.clone();
        let environment_snapshot = environment.to_vec();
        let input_id = format!("provider-env-field-{id}-{}", field.name);
        let commit = move |this: &mut Self,
                           text: String,
                           _: &mut Window,
                           cx: &mut Context<Self>| {
            let rows = this.instance_rows(cx);
            if let Some(row) = rows.iter().find(|row| row.id == id) {
                let next =
                    next_provider_environment_with_field_value(&environment_snapshot, field, &text);
                this.write_environment(row, &next, cx);
            }
        };
        let input = if field.is_sensitive() {
            self.secret_commit_input(
                &input_id,
                &value,
                field.placeholder,
                configured,
                window,
                cx,
                commit,
            )
        } else {
            self.input(&input_id, &value, field.placeholder, window, cx, commit)
        };
        let clear_id = row.id.clone();
        let clear_env = environment.to_vec();
        let clear = variable.is_some().then(|| {
            Button::new(SharedString::from(format!(
                "env-clear-{}-{}",
                row.id, field.name
            )))
            .variant(ButtonVariant::GhostDestructive)
            .size(ButtonSize::IconSm)
            .icon(IconName::X)
            .icon_size(rems(0.875))
            .tooltip(Tooltip::new(format!("Clear {}", field.label)))
            .on_click(cx.listener(move |this, _, _, cx| {
                let rows = this.instance_rows(cx);
                if let Some(row) = rows.iter().find(|row| row.id == clear_id) {
                    let names: std::collections::HashSet<&str> = [field.name].into();
                    let next = provider_environment_without_names(&clear_env, &names);
                    this.write_environment(row, &next, cx);
                }
            }))
        });
        SettingsRow::new(field.label)
            .description(field.description)
            .control(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(rems(14.)).child(text_field(
                        &input,
                        None,
                        FieldSize::Sm,
                        false,
                        cx,
                    )))
                    .children(clear),
            )
            .into_any_element()
    }

    fn write_environment(
        &mut self,
        row: &InstanceRow,
        environment: &[EnvironmentVariable],
        cx: &mut Context<Self>,
    ) {
        let cleaned: Vec<EnvironmentVariable> = environment
            .iter()
            .filter(|variable| !variable.name.trim().is_empty())
            .cloned()
            .collect();
        let value = (!cleaned.is_empty()).then(|| environment_to_value(&cleaned));
        self.update_instance(row, vec![("environment", value)], cx);
    }

    fn provider_field_row(
        &mut self,
        row: &InstanceRow,
        field: &ProviderSettingsFieldModel,
        config: Option<&Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = row.id.clone();
        let field_for_commit = field.clone();
        let write = move |this: &mut Self, value: FieldValue<'_>, cx: &mut Context<Self>| {
            let rows = this.instance_rows(cx);
            if let Some(row) = rows.iter().find(|row| row.id == id) {
                let next = next_provider_config_with_field_value(
                    row.instance.config.as_ref(),
                    &field_for_commit,
                    value,
                );
                this.update_instance(row, vec![("config", next)], cx);
            }
        };
        let control: AnyElement = match field.control {
            ProviderSettingsFormControl::Switch => {
                let write = write.clone();
                let entity = cx.entity();
                switch(
                    SharedString::from(format!("provider-field-{}-{}", row.id, field.key)),
                    read_provider_config_boolean(
                        config,
                        &field.key,
                        field.default_boolean_value.unwrap_or(false),
                    ),
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
                    let value = read_provider_config_string(config, &field.key);
                    if value.is_empty() {
                        fallback.clone()
                    } else {
                        value
                    }
                };
                let label = options
                    .iter()
                    .find(|o| o.value == current)
                    .map(|o| o.label.clone())
                    .unwrap_or(current.clone());
                let write = write.clone();
                self.select_control_rich(
                    cx,
                    &format!("provider-select-{}-{}", row.id, field.key),
                    div().child(label).into_any_element(),
                    options
                        .iter()
                        .map(|o| {
                            (
                                json!(o.value),
                                div().child(o.label.clone()).into_any_element(),
                                o.value == current,
                            )
                        })
                        .collect(),
                    SelectWidth::Fixed(14.),
                    move |this, value, _, cx| {
                        if let Some(next) = value.as_str() {
                            // The default choice is stored as an omitted key.
                            let stored = if next == fallback { "" } else { next };
                            write(this, FieldValue::Text(stored), cx);
                        }
                    },
                )
            }
            ProviderSettingsFormControl::Password => {
                let current = read_provider_config_string(config, &field.key);
                let placeholder = field.placeholder.clone().unwrap_or_default();
                let write = write.clone();
                let input = self.password_input(
                    &format!("provider-field-{}-{}", row.id, field.key),
                    &current,
                    &placeholder,
                    window,
                    cx,
                    move |this, text, _, cx| write(this, FieldValue::Text(&text), cx),
                );
                div()
                    .w(rems(14.))
                    .child(password_field(&input, None, FieldSize::Sm, cx))
                    .into_any_element()
            }
            _ => {
                let current = read_provider_config_string(config, &field.key);
                let placeholder = field.placeholder.clone().unwrap_or_default();
                let write = write.clone();
                let input = self.input(
                    &format!("provider-field-{}-{}", row.id, field.key),
                    &current,
                    &placeholder,
                    window,
                    cx,
                    move |this, text, _, cx| write(this, FieldValue::Text(&text), cx),
                );
                div()
                    .w(rems(14.))
                    .child(text_field(&input, None, FieldSize::Sm, false, cx))
                    .into_any_element()
            }
        };
        let mut settings_row = SettingsRow::new(field.label.clone()).control(control);
        if let Some(description) = &field.description {
            settings_row = settings_row.description(description.clone());
        }
        settings_row.into_any_element()
    }

    /// `ProviderEnvironmentSection`: the generic KEY=value editor.
    fn environment_variables_row(
        &mut self,
        row: &InstanceRow,
        generic: &[EnvironmentVariable],
        all: &[EnvironmentVariable],
        dedicated: &std::collections::HashSet<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let dedicated_vars: Vec<EnvironmentVariable> = all
            .iter()
            .filter(|v| dedicated.contains(v.name.as_str()))
            .cloned()
            .collect();
        let add_id = row.id.clone();
        let add_dedicated = dedicated_vars.clone();
        let add_generic = generic.to_vec();
        let add = Button::new(SharedString::from(format!("env-add-{}", row.id)))
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Sm)
            .icon(IconName::Plus)
            .icon_size(rems(0.75))
            .label("Add variable")
            .on_click(cx.listener(move |this, _, _, cx| {
                // A blank sensitive row; it is published once it has a valid name.
                let mut next = add_dedicated.clone();
                next.extend(add_generic.clone());
                next.push(EnvironmentVariable {
                    name: String::new(),
                    value: String::new(),
                    sensitive: true,
                    value_redacted: None,
                });
                this.panels
                    .providers
                    .env_drafts
                    .insert(add_id.clone(), next);
                cx.notify();
            }));

        let draft = self.panels.providers.env_drafts.get(&row.id).cloned();
        let rows_source: Vec<EnvironmentVariable> = match &draft {
            Some(draft) => draft
                .iter()
                .filter(|v| !dedicated.contains(v.name.as_str()))
                .cloned()
                .collect(),
            None => generic.to_vec(),
        };
        // Dedicated (named-field) variables sit first in the published list.
        let offset = dedicated_vars.len();
        let mut children: Vec<AnyElement> = Vec::new();
        for (index, variable) in rows_source.iter().enumerate() {
            let id = row.id.clone();
            let all_vars: Vec<EnvironmentVariable> = {
                let mut v = dedicated_vars.clone();
                v.extend(rows_source.clone());
                v
            };
            let name_id = id.clone();
            let name_vars = all_vars.clone();
            let name_input = self.input(
                &format!("env-name-{id}-{index}"),
                &variable.name,
                "VARIABLE_NAME",
                window,
                cx,
                move |this, text, _, cx| {
                    let mut next = name_vars.clone();
                    if let Some(entry) = next.get_mut(offset + index) {
                        entry.name = text.trim().to_owned();
                    }
                    this.publish_environment_rows(&name_id, next, cx);
                },
            );
            let value_id = id.clone();
            let value_vars = all_vars.clone();
            let value_configured = variable.value_redacted == Some(true);
            let value_text = if value_configured {
                ""
            } else {
                &variable.value
            };
            let value_commit =
                move |this: &mut Self, text: String, _: &mut Window, cx: &mut Context<Self>| {
                    let mut next = value_vars.clone();
                    if let Some(entry) = next.get_mut(offset + index) {
                        entry.value = text;
                        entry.value_redacted = Some(false);
                    }
                    this.publish_environment_rows(&value_id, next, cx);
                };
            let value_input_id = format!("env-value-{id}-{index}");
            let value_input = if variable.sensitive {
                self.secret_commit_input(
                    &value_input_id,
                    value_text,
                    "value",
                    value_configured,
                    window,
                    cx,
                    value_commit,
                )
            } else {
                self.input(
                    &value_input_id,
                    value_text,
                    "value",
                    window,
                    cx,
                    value_commit,
                )
            };
            let sensitive = variable.sensitive;
            let sens_id = id.clone();
            let sens_vars = all_vars.clone();
            let sensitive_button =
                Button::new(SharedString::from(format!("env-sensitive-{id}-{index}")))
                    .variant(ButtonVariant::GhostMuted)
                    .size(ButtonSize::IconMicro)
                    .icon(if sensitive {
                        IconName::Lock
                    } else {
                        IconName::LockOpen
                    })
                    .pressed(sensitive)
                    .tooltip(Tooltip::new(if sensitive {
                        "Sensitive, stored separately"
                    } else {
                        "Plain text"
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let mut next = sens_vars.clone();
                        if let Some(entry) = next.get_mut(offset + index) {
                            entry.sensitive = !entry.sensitive;
                            // A plain value is never saved as redacted (`ProviderInstanceCard`).
                            if !entry.sensitive {
                                entry.value_redacted = Some(false);
                            }
                        }
                        this.publish_environment_rows(&sens_id, next, cx);
                    }));
            let rm_id = id.clone();
            let rm_vars = all_vars.clone();
            let remove = Button::new(SharedString::from(format!("env-remove-{id}-{index}")))
                .variant(ButtonVariant::GhostDestructive)
                .size(ButtonSize::IconMicro)
                .icon(IconName::X)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let mut next = rm_vars.clone();
                    if offset + index < next.len() {
                        next.remove(offset + index);
                    }
                    this.publish_environment_rows(&rm_id, next, cx);
                }));
            children.push(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(rems(0.375))
                    .child(div().w(rems(11.)).flex_shrink_0().child(text_field(
                        &name_input,
                        None,
                        FieldSize::Sm,
                        true,
                        cx,
                    )))
                    .child(
                        div()
                            .text_size(TEXT_XS.size)
                            .text_color(theme.muted_foreground)
                            .child("="),
                    )
                    .child(div().min_w_0().flex_1().child(text_field(
                        &value_input,
                        None,
                        FieldSize::Sm,
                        true,
                        cx,
                    )))
                    .child(sensitive_button)
                    .child(remove)
                    .into_any_element(),
            );
        }
        if !rows_source.is_empty() {
            children.push(
                div()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child("Sensitive values are stored separately and never returned to the app.")
                    .into_any_element(),
            );
        }
        let mut settings_row = SettingsRow::new("Variables")
            .description("API keys, base URLs, and other per-instance CLI settings.")
            .control(add);
        if !children.is_empty() {
            settings_row = settings_row.child(
                div()
                    .mt_3()
                    .flex()
                    .min_w_0()
                    .flex_col()
                    .gap_2()
                    .pb_2()
                    .children(children),
            );
        }
        settings_row.into_any_element()
    }

    /// Publishes the rows with valid names (`publishRows`); an incomplete row
    /// keeps the draft without writing.
    fn publish_environment_rows(
        &mut self,
        id: &str,
        rows: Vec<EnvironmentVariable>,
        cx: &mut Context<Self>,
    ) {
        let mut published = Vec::new();
        for row in &rows {
            let name = row.name.trim();
            if !is_valid_environment_variable_name(name) {
                if !name.is_empty()
                    || !row.value.is_empty()
                    || !row.sensitive
                    || row.value_redacted.is_some()
                {
                    // Unfinished rows stay as typed until they can be published.
                    self.panels
                        .providers
                        .env_drafts
                        .insert(id.to_owned(), rows.clone());
                    cx.notify();
                    return;
                }
                continue;
            }
            let mut row = row.clone();
            row.name = name.to_owned();
            published.push(row);
        }
        // The draft keeps no plaintext secret once it is published: the host holds
        // it, and the echoed row shows the "Configured" state.
        self.panels
            .providers
            .env_drafts
            .insert(id.to_owned(), redact_saved_environment(&rows));
        let rows_now = self.instance_rows(cx);
        if let Some(row) = rows_now.iter().find(|row| row.id == id) {
            self.write_environment(row, &published, cx);
        }
    }
}

impl SettingsView {
    // ── models ──────────────────────────────────────────────────────────

    fn model_prefs(
        &self,
        instance: &str,
        cx: &gpui_kit::App,
    ) -> (Vec<String>, Vec<String>, Vec<String>) {
        let vm = self.vm(cx);
        let server = serde_json::to_value(&vm.server).unwrap_or(Value::Null);
        let favorites = server
            .get("favorites")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| entry.get("provider").and_then(Value::as_str) == Some(instance))
                    .filter_map(|entry| {
                        entry
                            .get("model")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let preference = server
            .get("providerModelPreferences")
            .and_then(|prefs| prefs.get(instance));
        let list = |key: &str| -> Vec<String> {
            preference
                .and_then(|entry| entry.get(key))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        (favorites, list("hiddenModels"), list("modelOrder"))
    }

    fn models_section(
        &mut self,
        row: &InstanceRow,
        live: Option<&ServerProvider>,
        config: Option<&Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use std::collections::HashSet;
        let theme = cx.app_theme().clone();
        let id = row.id.clone();
        let custom_models = read_config_custom_models(config);
        let live_models = live.map(|provider| provider.models.as_slice());
        let models = derive_provider_models_for_display(live_models, &custom_models);
        let (favorites, hidden, order) = self.model_prefs(&id, cx);
        let favorite_set: HashSet<String> = favorites.iter().cloned().collect();
        let hidden_set: HashSet<String> = hidden.iter().cloned().collect();
        let display = group_models_for_display(&models, &favorite_set, &hidden_set, &order);
        let is_hidden = |slug: &str, custom: bool| !custom && hidden_set.contains(slug);
        let favorite_count = display
            .iter()
            .filter(|m| favorite_set.contains(m.slug.as_str()))
            .count();
        let hidden_count = display
            .iter()
            .filter(|m| is_hidden(m.slug.as_str(), m.is_custom))
            .count();
        let built_in: Vec<_> = models.iter().filter(|m| !m.is_custom).cloned().collect();
        let all_hidden = !built_in.is_empty()
            && built_in
                .iter()
                .all(|m| hidden_set.contains(m.slug.as_str()));
        let show_filter = models.len() > FILTER_THRESHOLD;
        let needle = self.panels.providers.model_filter.trim().to_lowercase();
        let filtering = show_filter && !needle.is_empty();
        let visible: Vec<_> = display
            .iter()
            .filter(|m| {
                !filtering
                    || m.name.as_str().to_lowercase().contains(&needle)
                    || m.slug.as_str().to_lowercase().contains(&needle)
            })
            .cloned()
            .collect();
        let group_of = |slug: &str, custom: bool| -> u8 {
            if favorite_set.contains(slug) {
                0
            } else if is_hidden(slug, custom) {
                2
            } else {
                1
            }
        };

        // Header row: filter, bulk toggle, counts, add custom model.
        let filter_input = show_filter.then(|| {
            let id = id.clone();
            let input = self.input_live(
                &format!("model-filter-{id}"),
                &self.panels.providers.model_filter.clone(),
                "Filter models",
                window,
                cx,
                move |this, text, _, cx| {
                    this.panels.providers.model_filter = text;
                    cx.notify();
                },
            );
            div().w(rems(14.)).max_w_full().child(text_field(
                &input,
                None,
                FieldSize::Sm,
                false,
                cx,
            ))
        });
        let bulk = (!built_in.is_empty()).then(|| {
            let id = id.clone();
            let models = models.clone();
            let hidden = hidden.clone();
            let slugs: Vec<String> = built_in.iter().map(|m| m.slug.as_str().to_owned()).collect();
            Button::new(SharedString::from(format!("models-bulk-{id}")))
                .variant(ButtonVariant::GhostMuted)
                .size(ButtonSize::Xs)
                .label(if all_hidden { "Enable all" } else { "Disable all" })
                .on_click(cx.listener(move |this, _, _, cx| {
                    let next = next_hidden_models_for_bulk_toggle(&models, &hidden);
                    let changes = model_list_changes(&hidden, &next);
                    let entries: Vec<Value> = changes
                        .iter()
                        .filter(|(slug, _)| slugs.contains(slug))
                        .map(|(slug, now_hidden)| json!({ "provider": id, "model": slug, "hidden": now_hidden }))
                        .collect();
                    if !entries.is_empty() {
                        this.update_server(json!({ "setModelsHidden": entries }), cx);
                    }
                }))
        });
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        let mut counts = format!("{} model{}", models.len(), plural(models.len()));
        if favorite_count > 0 {
            counts.push_str(&format!(
                " · {favorite_count} favorite{}",
                plural(favorite_count)
            ));
        }
        if hidden_count > 0 {
            counts.push_str(&format!(" · {hidden_count} hidden"));
        }
        let add_button = (!self.panels.providers.adding_model).then(|| {
            Button::new(SharedString::from(format!("add-custom-model-{id}")))
                .variant(ButtonVariant::GhostMuted)
                .size(ButtonSize::Xs)
                .icon(IconName::Plus)
                .icon_size(rems(0.75))
                .label("Add custom model")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.panels.providers.adding_model = true;
                    cx.notify();
                }))
        });
        let header = div()
            .flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .flex_1()
                    .items_center()
                    .gap_2()
                    .children(filter_input)
                    .child(
                        div().flex().items_center().gap_2().children(bulk).child(
                            div()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .child(counts),
                        ),
                    ),
            )
            .children(add_button.map(|button| div().flex_shrink_0().child(button)));

        // Rows.
        let group_label = |label: &'static str, first: bool| {
            div()
                .px_2()
                .pb(px(6.))
                .pt(if first { px(4.) } else { px(20.) })
                .text_size(TEXT_2XS.size)
                .text_color(theme.muted_foreground)
                .child(label)
        };
        let mut list = div().mt_2().mx(px(0.)).flex().flex_col();
        if visible.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .py_2()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(if filtering {
                        "No models match."
                    } else {
                        "No models reported for this provider yet."
                    }),
            );
        }
        for (index, model) in visible.iter().enumerate() {
            let slug = model.slug.as_str().to_owned();
            let group = group_of(&slug, model.is_custom);
            let starts = index == 0
                || group_of(
                    visible[index - 1].slug.as_str(),
                    visible[index - 1].is_custom,
                ) != group;
            if starts && favorite_count > 0 && group == 0 {
                list = list.child(group_label("Favorites", index == 0));
            }
            if starts && favorite_count > 0 && group == 1 {
                list = list.child(group_label("All", index == 0));
            }
            if starts && group == 2 {
                list = list.child(group_label("Hidden from picker", index == 0));
            }
            let hidden_now = is_hidden(&slug, model.is_custom);
            let position = display
                .iter()
                .position(|m| m.slug.as_str() == slug)
                .unwrap_or(0);
            let can_up = !filtering
                && position > 0
                && group_of(
                    display[position - 1].slug.as_str(),
                    display[position - 1].is_custom,
                ) == group;
            let can_down = !filtering
                && position + 1 < display.len()
                && group_of(
                    display[position + 1].slug.as_str(),
                    display[position + 1].is_custom,
                ) == group;
            let caps = describe_model_capabilities(model).join(" · ");
            let name = model.name.as_str().to_owned();
            let is_favorite = group == 0;

            let star = {
                let id = id.clone();
                let slug = slug.clone();
                let tooltip = Tooltip::new(if is_favorite {
                    "Remove from favorites"
                } else {
                    "Add to favorites"
                });
                div()
                    .id(SharedString::from(format!("model-star-{id}-{slug}")))
                    .flex()
                    .size_5()
                    .items_center()
                    .justify_center()
                    .rounded(RADIUS_MD)
                    .cursor(CursorStyle::PointingHand)
                    .hover(|style| style.bg(with_alpha(theme.accent, 0.5)))
                    .tooltip(move |window, cx| tooltip.clone().build(window, cx))
                    .child(
                        Icon::new(IconName::Star)
                            .size(rems(0.75))
                            .color(if is_favorite { theme.warning } else { theme.muted_foreground }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.update_server(
                            json!({ "setModelFavorites": [{ "provider": id, "model": slug, "favorite": !is_favorite }] }),
                            cx,
                        );
                    }))
            };
            let mut actions = div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_end()
                .gap(px(2.))
                .min_w(rems(5.5));
            if !hidden_now && !filtering {
                for (direction, icon, label, enabled) in [
                    (-1i32, IconName::ArrowUp, "Move up", can_up),
                    (1, IconName::ArrowDown, "Move down", can_down),
                ] {
                    let id = id.clone();
                    let slug = slug.clone();
                    let order_all: Vec<String> =
                        display.iter().map(|m| m.slug.as_str().to_owned()).collect();
                    actions = actions.child(
                        Button::new(SharedString::from(format!("model-move-{direction}-{id}-{slug}")))
                            .variant(ButtonVariant::GhostMuted)
                            .size(ButtonSize::IconMicro)
                            .icon(icon)
                            .icon_size(rems(0.75))
                            .disabled(!enabled)
                            .tooltip(Tooltip::new(label))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let mut next = order_all.clone();
                                let Some(index) = next.iter().position(|s| *s == slug) else { return };
                                let target = index as i32 + direction;
                                if target < 0 || target as usize >= next.len() {
                                    return;
                                }
                                next.swap(index, target as usize);
                                this.update_server(
                                    json!({ "setProviderModelOrder": { "provider": id, "modelOrder": next } }),
                                    cx,
                                );
                            })),
                    );
                }
            }
            if model.is_custom {
                let id = id.clone();
                let slug = slug.clone();
                if let Some(entry) = custom_models.iter().find(|entry| entry.slug == slug) {
                    let entry = entry.clone();
                    let slug_toggle = slug.clone();
                    actions = actions.child(
                        Button::new(SharedString::from(format!("model-edit-{id}-{slug}")))
                            .variant(ButtonVariant::GhostMuted)
                            .size(ButtonSize::IconMicro)
                            .icon(IconName::SquarePen)
                            .icon_size(rems(0.75))
                            .tooltip(Tooltip::new("Edit name and options"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.panels.providers.editing_model.as_deref()
                                    == Some(slug_toggle.as_str())
                                {
                                    this.cancel_custom_model_edit(cx);
                                } else {
                                    this.begin_custom_model_edit(&entry, cx);
                                }
                            })),
                    );
                }
                let remove_row = row.id.clone();
                let remaining: Vec<Value> = custom_models
                    .iter()
                    .filter(|entry| entry.slug != slug)
                    .map(|entry| json!({ "slug": entry.slug, "name": entry.name, "capabilities": entry.capabilities }))
                    .collect();
                let order_without: Vec<String> =
                    order.iter().filter(|s| **s != slug).cloned().collect();
                let had_favorite = favorite_set.contains(&slug);
                actions = actions.child(
                    Button::new(SharedString::from(format!("model-remove-{id}-{slug}")))
                        .variant(ButtonVariant::GhostMuted)
                        .size(ButtonSize::IconMicro)
                        .icon(IconName::X)
                        .icon_size(rems(0.75))
                        .tooltip(Tooltip::new("Remove custom model"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let rows = this.instance_rows(cx);
                            if let Some(row) = rows.iter().find(|row| row.id == remove_row) {
                                let next = next_config_blob_with_value(
                                    row.instance.config.as_ref(),
                                    "customModels",
                                    Value::Array(remaining.clone()),
                                );
                                this.update_instance(row, vec![("config", Some(next))], cx);
                            }
                            this.update_server(
                                json!({ "setProviderModelOrder": { "provider": id, "modelOrder": order_without } }),
                                cx,
                            );
                            if had_favorite {
                                this.update_server(
                                    json!({ "setModelFavorites": [{ "provider": id, "model": slug, "favorite": false }] }),
                                    cx,
                                );
                            }
                        })),
                );
            }
            // `pickerTooltip`: the switch sits in a wrapper span that owns the tooltip.
            let picker_tooltip = if model.is_custom {
                "Custom models are always shown in the picker"
            } else if hidden_now {
                "Hidden from picker"
            } else {
                "Shown in picker"
            };
            let picker_switch = {
                let id = id.clone();
                let slug = slug.clone();
                let entity = cx.entity();
                switch(
                    SharedString::from(format!("model-visible-{id}-{slug}")),
                    !hidden_now,
                    model.is_custom,
                    cx,
                    move |checked, _, cx| {
                        entity.update(cx, |this, cx| {
                            this.update_server(
                                json!({ "setModelsHidden": [{ "provider": id, "model": slug, "hidden": !checked }] }),
                                cx,
                            );
                        });
                    },
                )
            };
            list = list.child(
                div()
                    .id(SharedString::from(format!("model-row-{id}-{slug}")))
                    .flex()
                    .h_7()
                    .items_center()
                    .gap_2()
                    .rounded_md()
                    .px_2()
                    .hover(|style| style.bg(with_alpha(theme.muted, 0.3)))
                    .when(hidden_now, |this| this.opacity(0.5))
                    .child(div().w(rems(1.5)).flex_shrink_0().child(star))
                    .child(
                        div()
                            .flex()
                            .min_w_0()
                            .flex_1()
                            .items_baseline()
                            .gap_2()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(TEXT_XS.size)
                                    .text_color(if hidden_now {
                                        theme.muted_foreground
                                    } else {
                                        with_alpha(theme.foreground, 0.9).into()
                                    })
                                    .child(name.clone()),
                            )
                            .when(name != slug, |this| {
                                this.child(
                                    div()
                                        .truncate()
                                        .font_family(theme.fonts.mono.clone())
                                        .text_size(TEXT_2XS.size)
                                        .text_color(with_alpha(theme.muted_foreground, 0.7))
                                        .child(slug.clone()),
                                )
                            })
                            .when(model.is_custom, |this| {
                                this.child(
                                    div()
                                        .text_size(TEXT_2XS.size)
                                        .text_color(with_alpha(theme.muted_foreground, 0.7))
                                        .child("custom"),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(TEXT_2XS.size)
                            .text_color(with_alpha(theme.muted_foreground, 0.7))
                            .child(caps),
                    )
                    .child(actions)
                    .child(
                        div().flex().flex_shrink_0().items_center().child(
                            div()
                                .id(SharedString::from(format!("model-visible-tip-{id}-{slug}")))
                                .flex()
                                .flex_shrink_0()
                                .items_center()
                                .tooltip({
                                    let tooltip = Tooltip::new(picker_tooltip);
                                    move |window, cx| tooltip.clone().build(window, cx)
                                })
                                .child(picker_switch),
                        ),
                    ),
            );
            if model.is_custom
                && self.panels.providers.editing_model.as_deref() == Some(slug.as_str())
            {
                let built_in_live = live
                    .map(|provider| provider.models.as_slice())
                    .unwrap_or(&[]);
                if let Some(editor) = self.render_custom_model_editor(
                    &id,
                    &row.id,
                    &row.driver,
                    built_in_live,
                    window,
                    cx,
                ) {
                    list = list.child(editor);
                }
            }
        }

        // Add custom model input.
        let adding = self.panels.providers.adding_model.then(|| {
            let id = id.clone();
            let driver = row.driver.clone();
            let row_id = row.id.clone();
            let builtin_slugs: Vec<String> = built_in
                .iter()
                .map(|m| m.slug.as_str().to_owned())
                .collect();
            let existing = custom_models.clone();
            let input = self.input(
                &format!("custom-model-{id}"),
                &self.panels.providers.model_input.clone(),
                custom_model_placeholder(&driver),
                window,
                cx,
                move |this, text, _, cx| {
                    this.panels.providers.model_input = text;
                    this.commit_custom_model(&row_id, &builtin_slugs, &existing, cx);
                },
            );
            let add_row = row.id.clone();
            let builtin_slugs2: Vec<String> = built_in
                .iter()
                .map(|m| m.slug.as_str().to_owned())
                .collect();
            let existing2 = custom_models.clone();
            div()
                .mt_3()
                .flex()
                .gap_2()
                .child(div().min_w_0().flex_1().child(text_field(
                    &input,
                    None,
                    FieldSize::Sm,
                    false,
                    cx,
                )))
                .child(
                    Button::new("add-custom-model-confirm")
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Sm)
                        .label("Add")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let text = this
                                .inputs
                                .get(&SharedString::from(format!("custom-model-{}", add_row)))
                                .map(|state| state.read(cx).value().to_string())
                                .unwrap_or_default();
                            this.panels.providers.model_input = text;
                            this.commit_custom_model(&add_row, &builtin_slugs2, &existing2, cx);
                            let _ = window;
                        })),
                )
                .child(
                    Button::new("add-custom-model-cancel")
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Sm)
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.panels.providers.adding_model = false;
                            this.panels.providers.model_input.clear();
                            this.panels.providers.model_error = None;
                            cx.notify();
                        })),
                )
        });
        let error = self.panels.providers.model_error.clone().map(|error| {
            div()
                .mt_2()
                .text_size(TEXT_XS.size)
                .text_color(theme.destructive)
                .child(error)
        });

        SettingsSection::new("Models")
            .child(
                div()
                    .px_4()
                    .py_3()
                    .flex()
                    .flex_col()
                    .child(header)
                    .child(list)
                    .children(adding)
                    .children(error),
            )
            .into_any_element()
    }

    pub(crate) fn save_custom_model_draft(&mut self, row_id: &str, cx: &mut Context<Self>) {
        use super::super::custom_model_editor_logic::{definition_from_draft, validate_draft};
        let Some(draft) = self.panels.providers.custom_model_draft.clone() else {
            return;
        };
        if let Some(problem) = validate_draft(&draft) {
            self.panels.providers.custom_model_editor_error = Some(problem);
            cx.notify();
            return;
        }
        let definition = definition_from_draft(&draft);
        let rows = self.instance_rows(cx);
        let Some(row) = rows.iter().find(|row| row.id == row_id) else {
            return;
        };
        let custom_models = read_config_custom_models(row.instance.config.as_ref());
        let next: Vec<Value> = custom_models
            .iter()
            .map(|entry| {
                if entry.slug == definition.slug {
                    json!({
                        "slug": definition.slug,
                        "name": definition.name,
                        "capabilities": definition.capabilities,
                    })
                } else {
                    json!({
                        "slug": entry.slug,
                        "name": entry.name,
                        "capabilities": entry.capabilities,
                    })
                }
            })
            .collect();
        let config = next_config_blob_with_value(
            row.instance.config.as_ref(),
            "customModels",
            Value::Array(next),
        );
        self.update_instance(row, vec![("config", Some(config))], cx);
        self.cancel_custom_model_edit(cx);
    }

    /// `handleAdd`: validates and appends a custom model slug.
    fn commit_custom_model(
        &mut self,
        row_id: &str,
        built_in: &[String],
        existing: &[CustomModelDefinition],
        cx: &mut Context<Self>,
    ) {
        let input = std::mem::take(&mut self.panels.providers.model_input);
        let Some(normalized) = turin_shared::model::normalize_custom_model_slug(Some(&input))
        else {
            self.panels.providers.model_error = Some("Enter a model slug.".into());
            cx.notify();
            return;
        };
        let error = if built_in.contains(&normalized) {
            Some("That model is already built in.".to_owned())
        } else if normalized.chars().count() > MAX_CUSTOM_MODEL_LENGTH {
            Some(format!(
                "Model slugs must be {MAX_CUSTOM_MODEL_LENGTH} characters or less."
            ))
        } else if existing.iter().any(|entry| entry.slug == normalized) {
            Some("That custom model is already saved.".to_owned())
        } else {
            None
        };
        if error.is_some() {
            self.panels.providers.model_input = input;
            self.panels.providers.model_error = error;
            cx.notify();
            return;
        }
        let mut next: Vec<Value> = existing
            .iter()
            .map(|entry| json!({ "slug": entry.slug, "name": entry.name, "capabilities": entry.capabilities }))
            .collect();
        next.push(json!({ "slug": normalized, "name": normalized, "capabilities": Value::Null }));
        let rows = self.instance_rows(cx);
        if let Some(row) = rows.iter().find(|row| row.id == row_id) {
            let config = next_config_blob_with_value(
                row.instance.config.as_ref(),
                "customModels",
                Value::Array(next),
            );
            self.update_instance(row, vec![("config", Some(config))], cx);
        }
        self.panels.providers.model_filter.clear();
        self.panels.providers.model_error = None;
        self.panels.providers.adding_model = false;
        cx.notify();
    }

    // ── usage providers ─────────────────────────────────────────────────

    fn usage_providers_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let vm = self.vm(cx);
        let (id, title) = searchable_setting("usage-providers");
        let sources: Vec<(String, Value)> = serde_json::to_value(&vm.server.usage_limit_sources)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .map(|map| map.into_iter().collect())
            .unwrap_or_default();
        let add = Button::new("add-usage-hub")
            .variant(ButtonVariant::Outline)
            .size(ButtonSize::Xs)
            .icon(IconName::Plus)
            .icon_size(rems(0.75))
            .label("Add hub")
            .on_click(cx.listener(|this, _, _, cx| {
                this.dialog = Some(super::super::dialog::Dialog::AddHub);
                cx.notify();
            }));
        let mut section = SettingsSection::new(title).id(id).header_action(add);
        if vm.environment.platform_os == "darwin" {
            let enabled = vm.server.cursor_keychain_usage_enabled;
            let entity = cx.entity();
            section = section.child(
                SettingsRow::new("Cursor account usage")
                    .id("cursor-keychain-usage")
                    .description("Read your existing Cursor CLI login from macOS Keychain to show account history and monthly limits. macOS may ask you to allow access.")
                    .control(switch("cursor-keychain-usage-switch", enabled, false, cx, move |next, _, cx| {
                        entity.update(cx, |this, cx| {
                            this.update_server(json!({ "cursorKeychainUsageEnabled": next }), cx);
                            let source = this.store.read(cx).source();
                            source.refresh_providers(cx);
                        });
                    })),
            );
        }
        if sources.is_empty() {
            section = section.child(SettingsRow::new("No hubs configured."));
        }
        for (key, source) in sources {
            let url = source
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let label = source
                .get("label")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .unwrap_or(&url)
                .to_owned();
            let enabled = source
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let mut description = String::from("CLI Proxy");
            if !enabled {
                description.push_str(" · Disabled");
            }
            if label != url {
                description.push_str(&format!(" · {url}"));
            }
            let remove_key = key.clone();
            let remove_label = label.clone();
            section = section.child(
                SettingsRow::new(label.clone()).description(description).control(
                    Button::new(SharedString::from(format!("remove-hub-{key}")))
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Xs)
                        .label("Remove")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let key = remove_key.clone();
                            this.dialog = Some(super::super::dialog::Dialog::Confirm(super::super::dialog::ConfirmSpec {
                                title: format!("Remove {remove_label}?").into(),
                                description: "The hub's management key is deleted from this server. Its accounts leave the Limits view; the hub itself is untouched. Add it again with the URL and key to bring them back.".into(),
                                confirm_label: "Remove hub".into(),
                                destructive: true,
                                on_confirm: Rc::new(move |this, _, cx| {
                                    this.update_server(json!({ "usageLimitSources": { key.clone(): Value::Null } }), cx);
                                }),
                            }));
                            cx.notify();
                        })),
                ),
            );
        }
        section.into_any_element()
    }

    // ── advanced ────────────────────────────────────────────────────────

    fn provider_advanced_section(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let vm = self.vm(cx);
        let resolved =
            turin_shared::server_settings::resolve_server_background_activity_settings(&vm.server);
        let base = turin_shared::server_settings::get_background_activity_base_profile(
            &vm.server.background_activity,
        );
        let defaults = turin_shared::server_settings::get_background_activity_preset_settings(base);
        let seconds = duration_to_seconds(resolved.provider_health_refresh_interval);
        let default_seconds = duration_to_seconds(defaults.provider_health_refresh_interval);
        let (id, title) = searchable_setting("provider-health-check-interval");
        let reset = (seconds != default_seconds).then(|| {
            let current = vm.server.background_activity.clone();
            div()
                .flex()
                .size_5()
                .items_center()
                .justify_center()
                .child(setting_reset_button(
                    "reset-provider-health-interval",
                    "Reset to default",
                    self.cb(cx, move |this, _, cx| {
                        let next = background_activity_override_settings(
                            &current,
                            &resolved,
                            BackgroundActivityOverridePatch {
                                provider_health_refresh_interval: None,
                                ..Default::default()
                            },
                        );
                        // Clearing the override falls back to the preset.
                        let mut value = serde_json::to_value(next).unwrap_or(Value::Null);
                        if let Some(overrides) =
                            value.get_mut("overrides").and_then(Value::as_object_mut)
                        {
                            overrides.remove("providerHealthRefreshInterval");
                        }
                        this.update_server(json!({ "backgroundActivity": value }), cx);
                    }),
                ))
        });
        let current = vm.server.background_activity.clone();
        let resolved_for_write = resolved;
        let control = self.number_field("provider-health-interval", seconds, 0, PROVIDER_HEALTH_INTERVAL_STEP_SECONDS, window, cx, move |this, value, cx| {
            let next = background_activity_override_settings(
                &current,
                &resolved_for_write,
                BackgroundActivityOverridePatch {
                    provider_health_refresh_interval: Some(normalize_interval_seconds(Some(value as f64), 0) * 1_000),
                    ..Default::default()
                },
            );
            this.update_server(json!({ "backgroundActivity": serde_json::to_value(next).unwrap_or(Value::Null) }), cx);
        });
        SettingsSection::new("Advanced")
            .child(
                SettingsRow::new(title)
                    .id(id)
                    .description("Refresh provider status, versions, and models in the background. Set to 0 to disable.")
                    .reset_action(reset)
                    .control(control),
            )
            .into_any_element()
    }

    /// `NumberField` (`size="sm"`, `w-32`): bordered `[−][input][+]` group.
    pub(super) fn number_field(
        &mut self,
        id: &str,
        value: u64,
        min: u64,
        step: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_commit: impl Fn(&mut Self, u64, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let on_commit = Rc::new(on_commit);
        let commit_text = on_commit.clone();
        let input = self.input(
            &format!("{id}-input"),
            &value.to_string(),
            "",
            window,
            cx,
            move |this, text, _, cx| {
                if let Ok(parsed) = text.trim().parse::<f64>() {
                    commit_text(this, parsed.max(min as f64).round() as u64, cx);
                }
            },
        );
        let step_button = |name: &'static str,
                           icon: IconName,
                           delta: i64,
                           on_commit: Rc<dyn Fn(&mut Self, u64, &mut Context<Self>)>,
                           cx: &mut Context<Self>| {
            div()
                .id(SharedString::from(format!("{id}-{name}")))
                .flex()
                .w_8()
                .h_full()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .cursor(CursorStyle::PointingHand)
                .text_color(theme.muted_foreground)
                .hover(|style| style.bg(with_alpha(theme.accent, 0.5)))
                .child(Icon::new(icon).size(rems(1.)).color(theme.muted_foreground))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let next = (value as i64 + delta).max(min as i64) as u64;
                    on_commit(this, next, cx);
                }))
        };
        let dec = step_button(
            "dec",
            IconName::Minus,
            -(step as i64),
            on_commit.clone(),
            cx,
        );
        let inc = step_button("inc", IconName::Plus, step as i64, on_commit.clone(), cx);
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .h(px(28.))
            .w(rems(8.))
            .overflow_hidden()
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(theme.input)
            .bg(if theme.is_dark() {
                with_alpha(theme.input, 0.32).into()
            } else {
                theme.background
            })
            .child(dec)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .border_x_1()
                    .border_color(theme.input)
                    .text_size(TEXT_SM.size)
                    .child(
                        gpui_kit::component::input::Input::new(&input)
                            .appearance(false)
                            .bordered(false)
                            .focus_bordered(false),
                    ),
            )
            .child(inc)
            .into_any_element()
    }
}

const FILTER_THRESHOLD: usize = 8;
const MAX_CUSTOM_MODEL_LENGTH: usize = 256;

/// `CUSTOM_MODEL_PLACEHOLDER_BY_KIND`.
fn custom_model_placeholder(driver: &str) -> &'static str {
    match driver {
        "codex" => "gpt-6.7-codex-ultra-preview",
        "claudeAgent" => "claude-sonnet-5",
        "cursor" => "claude-sonnet-4-6",
        "opencode" => "openai/gpt-5",
        _ => "model-slug",
    }
}

#[cfg(test)]
mod tests {
    //! Port of the row-visibility and target cases of
    //! upstream/apps/web/src/components/settings/ProviderSettingsPanel.environment.test.tsx
    //! (the cases that do not depend on React rendering or auth-session scopes).

    use pretty_assertions::assert_eq;
    use serde_json::json;
    use turin_contracts::provider_instance::ProviderInstanceConfigMap;

    use super::*;

    fn instances(value: Value) -> ProviderInstanceConfigMap {
        serde_json::from_value(value).expect("provider instances decode")
    }

    fn ids(rows: &[InstanceRow]) -> Vec<&str> {
        rows.iter().map(|row| row.id.as_str()).collect()
    }

    #[test]
    fn shows_codex_and_claude_while_hiding_untouched_disabled_provider_slots() {
        let rows = build_instance_rows(&instances(json!({})), &[], None);
        assert_eq!(ids(&rows), vec!["codex", "claudeAgent"]);
        for hidden in ["cursor", "grok", "pi", "opencode", "antigravity"] {
            assert!(!ids(&rows).contains(&hidden), "{hidden}");
        }
    }

    #[test]
    fn keeps_explicitly_configured_providers_visible_when_disabled() {
        let rows = build_instance_rows(
            &instances(json!({ "opencode": { "driver": "opencode", "enabled": false } })),
            &[],
            None,
        );
        assert!(ids(&rows).contains(&"opencode"));
        assert!(
            rows.iter()
                .find(|row| row.id == "opencode")
                .unwrap()
                .is_dirty
        );
    }

    #[test]
    fn keeps_legacy_provider_configuration_visible_when_disabled() {
        let rows = build_instance_rows(
            &instances(json!({
                "opencode": {
                    "driver": "opencode",
                    "enabled": false,
                    "config": { "binaryPath": "/custom/opencode" }
                }
            })),
            &[],
            None,
        );
        assert!(ids(&rows).contains(&"opencode"));
    }

    #[test]
    fn lists_an_untouched_slot_only_when_it_is_the_requested_instance() {
        let rows = build_instance_rows(&instances(json!({})), &[], Some("opencode"));
        assert!(ids(&rows).contains(&"opencode"));
    }

    #[test]
    fn follows_each_default_slot_with_its_custom_instances() {
        let rows = build_instance_rows(
            &instances(json!({ "codex_work": { "driver": "codex", "enabled": true } })),
            &[],
            None,
        );
        assert_eq!(ids(&rows), vec!["codex", "codex_work", "claudeAgent"]);
        assert!(rows[0].is_default);
        assert!(!rows[1].is_default);
    }

    #[test]
    fn opens_the_requested_provider_instance_instead_of_the_first_provider() {
        let rows = build_instance_rows(
            &instances(json!({ "codex_work": { "driver": "codex", "enabled": true } })),
            &[],
            Some("codex_work"),
        );
        let selected = selected_instance_row(&rows, Some("codex_work"), Some("codex_work"));
        assert_eq!(selected.map(|row| row.id.as_str()), Some("codex_work"));
    }

    #[test]
    fn does_not_substitute_another_account_when_the_requested_instance_was_removed() {
        let rows = build_instance_rows(&instances(json!({})), &[], Some("codex_work"));
        assert_eq!(
            selected_instance_row(&rows, Some("codex_work"), Some("codex_work")).map(|r| &r.id),
            None
        );
    }

    #[test]
    fn falls_back_to_the_first_row_when_nothing_was_requested() {
        let rows = build_instance_rows(&instances(json!({})), &[], None);
        assert_eq!(
            selected_instance_row(&rows, None, None).map(|row| row.id.as_str()),
            Some("codex")
        );
        // A selection that vanished after a delete falls back too.
        assert_eq!(
            selected_instance_row(&rows, Some("gone"), None).map(|row| row.id.as_str()),
            Some("codex")
        );
    }
}
