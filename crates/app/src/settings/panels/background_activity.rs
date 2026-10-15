//! Port of `BackgroundActivityAdvancedDialog` (SettingsPanels.tsx).

use gpui_kit::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _,
    Window, div, px, rems,
};
use serde_json::{Value, json};
use turin_contracts::settings::BackgroundActivityProfile;

use super::super::controls::SelectWidth;
use super::super::dialog::{dialog_footer, dialog_frame, dialog_header, dialog_panel};
use super::super::panels_logic::{
    BackgroundActivityOverridePatch, PROVIDER_HEALTH_INTERVAL_STEP_SECONDS,
    background_activity_override_settings, background_activity_shared_policy_settings,
    duration_to_seconds, normalize_interval_seconds,
};
use super::super::view::{SettingsView, Which};
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_XL, TEXT_SM, TEXT_XS};
use crate::ui::{Button, ButtonVariant};

type Patch = fn(u64) -> BackgroundActivityOverridePatch;

impl SettingsView {
    fn write_background_override(&mut self, patch: BackgroundActivityOverridePatch, cx: &mut Context<Self>) {
        let vm = self.vm(cx);
        let resolved = turin_shared::server_settings::resolve_server_background_activity_settings(&vm.server);
        let next = background_activity_override_settings(&vm.server.background_activity, &resolved, patch);
        self.update_server(json!({ "backgroundActivity": serde_json::to_value(next).unwrap_or(Value::Null) }), cx);
    }

    pub(in crate::settings) fn render_background_activity_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let vm = self.vm(cx);
        let resolved = turin_shared::server_settings::resolve_server_background_activity_settings(&vm.server);
        let profile_label = |profile: BackgroundActivityProfile| match profile {
            BackgroundActivityProfile::Balanced => "Balanced",
            BackgroundActivityProfile::Performance => "Performance",
            BackgroundActivityProfile::BatterySaver => "Battery saver",
        };

        let row = |title: &'static str, description: &'static str, control: AnyElement, bordered: bool| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .px_4()
                .py_3()
                .when_bordered(bordered, with_alpha(theme.border, 1.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .min_w_0()
                        .child(div().text_size(TEXT_SM.size).font_weight(FontWeight::MEDIUM).child(title))
                        .child(
                            div()
                                .text_size(TEXT_XS.size)
                                .line_height(rems(1.25))
                                .text_color(theme.muted_foreground)
                                .child(description),
                        ),
                )
                .child(control)
        };

        // Shared policy.
        let policy = self.select_control(
            cx,
            "background-shared-policy",
            profile_label(resolved.profile).into(),
            [
                BackgroundActivityProfile::Balanced,
                BackgroundActivityProfile::Performance,
                BackgroundActivityProfile::BatterySaver,
            ]
            .into_iter()
            .map(|profile| {
                (
                    serde_json::to_value(profile).unwrap_or(Value::Null),
                    SharedString::from(profile_label(profile)),
                    profile == resolved.profile,
                )
            })
            .collect(),
            SelectWidth::Fixed(10.),
            false,
            |this, value, _, cx| {
                let Ok(profile) = serde_json::from_value::<BackgroundActivityProfile>(value) else { return };
                let vm = this.vm(cx);
                let next = background_activity_shared_policy_settings(&vm.server, profile);
                this.update_server(json!({ "backgroundActivity": serde_json::to_value(next).unwrap_or(Value::Null) }), cx);
            },
        );

        let interval = |this: &mut Self,
                        id: &str,
                        value_ms: u64,
                        min: u64,
                        step: u64,
                        patch: Patch,
                        window: &mut Window,
                        cx: &mut Context<Self>|
         -> AnyElement {
            let field = this.number_field(id, duration_to_seconds(value_ms), min, step, window, cx, move |this, value, cx| {
                this.write_background_override(patch(normalize_interval_seconds(Some(value as f64), min) * 1_000), cx);
            });
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap_2()
                .child(field)
                .child(div().text_size(TEXT_XS.size).text_color(theme.muted_foreground).child("seconds"))
                .into_any_element()
        };

        let git = interval(self, "bg-git", resolved.automatic_git_fetch_interval, 0, 5,
            |ms| BackgroundActivityOverridePatch { automatic_git_fetch_interval: Some(ms), ..Default::default() }, window, cx);
        let health = interval(self, "bg-health", resolved.provider_health_refresh_interval, 0, PROVIDER_HEALTH_INTERVAL_STEP_SECONDS,
            |ms| BackgroundActivityOverridePatch { provider_health_refresh_interval: Some(ms), ..Default::default() }, window, cx);
        let active = interval(self, "bg-host-active", resolved.host_power_monitor_active_interval, 5, 5,
            |ms| BackgroundActivityOverridePatch { host_power_monitor_active_interval: Some(ms), ..Default::default() }, window, cx);
        let idle = interval(self, "bg-host-idle", resolved.host_power_monitor_idle_interval, 5, 30,
            |ms| BackgroundActivityOverridePatch { host_power_monitor_idle_interval: Some(ms), ..Default::default() }, window, cx);

        let toggles: [(&'static str, bool, fn(bool) -> BackgroundActivityOverridePatch); 4] = [
            ("Pause when host is locked", resolved.pause_when_host_locked, |v| BackgroundActivityOverridePatch { pause_when_host_locked: Some(v), ..Default::default() }),
            ("Pause on host low power", resolved.pause_when_host_low_power, |v| BackgroundActivityOverridePatch { pause_when_host_low_power: Some(v), ..Default::default() }),
            ("Pause on client low power", resolved.pause_when_client_low_power, |v| BackgroundActivityOverridePatch { pause_when_client_low_power: Some(v), ..Default::default() }),
            ("Pause on battery", resolved.pause_when_on_battery, |v| BackgroundActivityOverridePatch { pause_when_on_battery: Some(v), ..Default::default() }),
        ];
        let mut grid = div().flex().flex_wrap().border_t_1().border_color(theme.border);
        for (index, (label, checked, patch)) in toggles.into_iter().enumerate() {
            let entity = cx.entity();
            grid = grid.child(
                div()
                    .flex()
                    .w(gpui_kit::relative(0.5))
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .when_bordered_right(index % 2 == 0, theme.border)
                    .child(div().text_size(TEXT_SM.size).font_weight(FontWeight::MEDIUM).child(label))
                    .child(super::super::controls::switch(
                        SharedString::from(format!("bg-toggle-{index}")),
                        checked,
                        false,
                        cx,
                        move |next, _, cx| {
                            entity.update(cx, |this, cx| this.write_background_override(patch(next), cx));
                        },
                    )),
            );
        }

        let card = div()
            .flex()
            .flex_col()
            .min_w_0()
            .overflow_hidden()
            .rounded(RADIUS_XL)
            .border_1()
            .border_color(theme.border)
            .bg(theme.card)
            .text_color(theme.card_foreground)
            .child(row("Shared policy", "Controls whether background work may run after a subscribed interval fires.", policy, false))
            .child(row("Git fetch interval", "Refresh remote branch status in the background.", git, true))
            .child(row("Provider health interval", "Refresh provider availability, versions, and auth state. Model catalog updates follow Provider update checks.", health, true))
            .child(row("Host power monitor", "Poll host power state while clients are active.", active, true))
            .child(row("Idle host monitor", "Poll host power state when no foreground client is active.", idle, true))
            .child(grid);

        let dismiss = self.cb(cx, |this, _, cx| {
            this.dialog = None;
            cx.notify();
        });
        dialog_frame(
            &theme,
            rems(36.),
            vec![
                dialog_header(
                    "Background Activity",
                    Some("Tune the shared power policy and the background intervals that feed it.".into()),
                    &theme,
                ),
                dialog_panel("bg-body", card),
                dialog_footer(
                    &theme,
                    vec![
                        Button::new("bg-reset-all")
                            .variant(ButtonVariant::Outline)
                            .label("Reset all")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let default = this.json(cx).default_of(Which::Server, "backgroundActivity").clone();
                                this.update_server(json!({ "backgroundActivity": default }), cx);
                            }))
                            .into_any_element(),
                        Button::new("bg-done")
                            .label("Done")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.dialog = None;
                                cx.notify();
                            }))
                            .into_any_element(),
                    ],
                ),
            ],
            dismiss,
        )
    }
}

/// Border helpers for the dialog's card rows.
trait CardBorders: gpui_kit::Styled + Sized {
    fn when_bordered(self, bordered: bool, color: impl Into<gpui_kit::Hsla>) -> Self {
        if bordered { self.border_t_1().border_color(color) } else { self }
    }
    fn when_bordered_right(self, bordered: bool, color: impl Into<gpui_kit::Hsla>) -> Self {
        if bordered { self.border_r_1().border_color(color) } else { self }
    }
}

impl CardBorders for gpui_kit::Div {}

#[allow(dead_code)]
fn _unused() -> gpui_kit::Pixels {
    px(0.)
}
