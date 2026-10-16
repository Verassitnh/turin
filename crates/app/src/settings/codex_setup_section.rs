//! Port of upstream/apps/web/src/components/settings/CodexSetupSection.tsx
//! (settings presentation). Handoff URLs and ChatGPT profile import remain on
//! task/R3-settings-logic (`codexAuthHandoff`, `providerAuthReturnUrl`).

use gpui_kit::{AnyElement, Context, IntoElement, ParentElement as _, Styled as _, Window, div};
use turin_provider_core::server_provider::{ServerProvider, ServerProviderAuthStatus};

use super::add_provider_instance_logic::{CodexSetupMode, read_codex_setup_mode};
use super::layout::{SettingsRow, SettingsSection};
use super::provider_form::next_config_blob_with_value;
use super::provider_status::get_provider_summary;
use super::view::SettingsView;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::TEXT_XS;
use crate::ui::{Button, ButtonSize, ButtonVariant};

impl SettingsView {
    pub(super) fn codex_setup_section(
        &mut self,
        row: &super::panels::providers::InstanceRow,
        live: Option<&ServerProvider>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let config = serde_json::to_value(&row.instance)
            .ok()
            .and_then(|value| value.get("config").cloned());
        let mode = read_codex_setup_mode(config.as_ref());
        let authenticated =
            live.is_some_and(|p| p.auth.status == ServerProviderAuthStatus::Authenticated);
        let summary = get_provider_summary(live);
        let mut section = SettingsSection::new("ChatGPT account");
        if mode == CodexSetupMode::Existing {
            section = section.child(
                SettingsRow::new("Codex CLI")
                    .description(if authenticated {
                        "Connected with your Codex CLI.".to_owned()
                    } else {
                        summary.headline.clone()
                    })
                    .control(
                        Button::new("codex-use-managed")
                            .variant(ButtonVariant::GhostMuted)
                            .size(ButtonSize::Sm)
                            .label("Use managed account")
                            .on_click({
                                let row_id = row.id.clone();
                                cx.listener(move |this, _, _, cx| {
                                    this.set_codex_setup_mode(&row_id, CodexSetupMode::Managed, cx);
                                })
                            }),
                    )
                    .into_any_element(),
            );
            return section.into_any_element();
        }
        let description = if authenticated {
            if let Some(email) = live.and_then(|p| p.auth.email.as_ref()) {
                format!("Signed in as {}.", email.as_str())
            } else {
                "Signed in.".to_owned()
            }
        } else {
            "Connect with your ChatGPT subscription.".to_owned()
        };
        let connect = if authenticated {
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.success)
                        .child("Ready"),
                )
                .child(self.chat_gpt_usage_button("codex-usage", cx))
                .into_any_element()
        } else {
            self.chat_gpt_connection_button(
                "codex-connect-chatgpt",
                "Continue with ChatGPT",
                false,
                cx,
                {
                    let id = row.id.clone();
                    move |this, _, cx| {
                        this.store.read(cx).source().start_provider_auth(&id, cx);
                    }
                },
            )
        };
        section = section
            .child(
                SettingsRow::new("Managed account")
                    .description(description)
                    .control(connect)
                    .into_any_element(),
            )
            .child(
                SettingsRow::new("Setup mode")
                    .description("Managed installs and signs in through T3 Code.")
                    .control(
                        Button::new("codex-use-existing")
                            .variant(ButtonVariant::GhostMuted)
                            .size(ButtonSize::Sm)
                            .label("Use existing CLI")
                            .on_click({
                                let row_id = row.id.clone();
                                cx.listener(move |this, _, _, cx| {
                                    this.set_codex_setup_mode(
                                        &row_id,
                                        CodexSetupMode::Existing,
                                        cx,
                                    );
                                })
                            }),
                    )
                    .into_any_element(),
            );
        section.into_any_element()
    }

    fn set_codex_setup_mode(&mut self, row_id: &str, mode: CodexSetupMode, cx: &mut Context<Self>) {
        let rows = self.instance_rows(cx);
        let Some(row) = rows.iter().find(|row| row.id == row_id) else {
            return;
        };
        let setup_mode = match mode {
            CodexSetupMode::Managed => "managed",
            CodexSetupMode::Existing => "existing",
        };
        let config = next_config_blob_with_value(
            row.instance.config.as_ref(),
            "setupMode",
            serde_json::json!(setup_mode),
        );
        self.update_instance(row, vec![("config", Some(config))], cx);
    }
}
