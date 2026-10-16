//! Port of upstream/apps/web/src/components/settings/AddCodexAccountDialog.tsx
//!
//! "Add ChatGPT account": names a new managed Codex instance, saves it, then
//! runs the managed sign-in in place. Closes itself once the destination
//! snapshot reports ChatGPT plan sharing.
//!
//! Deviation: `AuthProvidersManageScope` is always held (one local
//! environment, no sessions), so `canManageProviders` is true.

use gpui_kit::{
    AnyElement, Context, IntoElement as _, ParentElement as _, Styled as _, Window, div, rems,
};
use serde_json::json;

use super::chat_gpt_welcome_coordinator::uses_chat_gpt_sharing;
use super::codex_setup_section::CodexSetupHost;
use super::controls::{FieldSize, text_field};
use super::dialog::Dialog;
use super::layout::SettingsRow;
use super::view::SettingsView;
use super::wizard::{WizardSize, wizard_footer, wizard_header, wizard_panel, wizard_popup};
use crate::theme::ActiveAppTheme;
use crate::ui::{Button, ButtonSize, ButtonVariant};

/// `useState` of `AddCodexAccountDialog`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddCodexAccountState {
    pub name: String,
    pub instance_id: Option<String>,
    pub pending: bool,
}

impl Default for AddCodexAccountState {
    fn default() -> Self {
        Self {
            name: "Personal".to_owned(),
            instance_id: None,
            pending: false,
        }
    }
}

/// The display name a new account gets (`ChatGPT - {name}`).
pub fn account_display_name(name: &str) -> String {
    format!("ChatGPT - {}", name.trim())
}

/// The new managed Codex instance (`providerInstances[id]`).
pub fn new_account_instance(display_name: &str) -> serde_json::Value {
    json!({
        "driver": "codex",
        "displayName": display_name,
        "enabled": true,
        "config": { "enabled": true, "setupMode": "managed" },
    })
}

/// A fresh routing id; the name is editable and need not be unique.
pub fn new_account_instance_id() -> String {
    format!("codex_{}", uuid::Uuid::new_v4())
}

impl SettingsView {
    pub(in crate::settings) fn open_add_codex_account(&mut self, cx: &mut Context<Self>) {
        self.panels.providers.add_codex = AddCodexAccountState::default();
        self.dialog = Some(Dialog::AddCodexAccount);
        cx.notify();
    }

    fn close_add_codex_account(&mut self, cx: &mut Context<Self>) {
        self.dialog = None;
        cx.notify();
    }

    /// `createAccount()`.
    fn create_codex_account(&mut self, cx: &mut Context<Self>) {
        let state = self.panels.providers.add_codex.clone();
        if state.pending || state.name.trim().is_empty() {
            return;
        }
        self.panels.providers.add_codex.pending = true;
        let id = new_account_instance_id();
        let display_name = account_display_name(&state.name);
        self.write_provider_instance(&id, Some(new_account_instance(&display_name)), false, cx);
        let add = &mut self.panels.providers.add_codex;
        add.instance_id = Some(id);
        add.pending = false;
        cx.notify();
    }

    /// `AddCodexAccountDialog` with `AddManagedCodexAccountDialog`'s `renderSetup`.
    pub(super) fn render_add_codex_account_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = cx.app_theme().clone();
        let vm = self.vm(cx);
        let state = self.panels.providers.add_codex.clone();
        let provider = state.instance_id.as_ref().and_then(|id| {
            vm.providers
                .iter()
                .find(|candidate| candidate.instance_id.as_str() == id)
        });
        // The destination snapshot confirms the sign-in; close once it does.
        if provider.is_some_and(uses_chat_gpt_sharing) {
            self.dialog = None;
            return None;
        }
        let display_name = account_display_name(&state.name);
        let title = if state.instance_id.is_some() {
            display_name.clone()
        } else {
            "Add ChatGPT account".to_owned()
        };

        let body: AnyElement = match &state.instance_id {
            Some(instance_id) => {
                if provider.is_some_and(|provider| provider.setup.is_some()) {
                    let instance_id = instance_id.clone();
                    self.managed_codex_setup(
                        &instance_id,
                        true,
                        false,
                        CodexSetupHost::AccountDialog,
                        window,
                        cx,
                    )
                } else {
                    SettingsRow::new("Codex runtime")
                        .description("Preparing managed setup.")
                        .into_any_element()
                }
            }
            None => {
                let input = self.input_live(
                    "add-codex-account-name",
                    &state.name,
                    "e.g. Personal or Work",
                    window,
                    cx,
                    |this, text, _, cx| {
                        this.panels.providers.add_codex.name = text;
                        cx.notify();
                    },
                );
                self.on_input_submit("add-codex-account-name", |this, _, cx| {
                    this.create_codex_account(cx)
                });
                SettingsRow::new("Account name")
                    .description("Shown in the provider list and model picker.")
                    .control(div().w(rems(14.)).child(text_field(
                        &input,
                        None,
                        FieldSize::Default,
                        false,
                        cx,
                    )))
                    .into_any_element()
            }
        };

        let footer = if state.instance_id.is_some() {
            wizard_footer(
                None,
                vec![
                    Button::new("add-codex-finish-later")
                        .variant(ButtonVariant::Outline)
                        .label("Finish later")
                        .on_click(cx.listener(|this, _, _, cx| this.close_add_codex_account(cx)))
                        .into_any_element(),
                ],
            )
        } else {
            wizard_footer(
                None,
                vec![
                    Button::new("add-codex-cancel")
                        .variant(ButtonVariant::Outline)
                        .label("Cancel")
                        .disabled(state.pending)
                        .on_click(cx.listener(|this, _, _, cx| this.close_add_codex_account(cx)))
                        .into_any_element(),
                    Button::new("add-codex-continue")
                        .size(ButtonSize::Default)
                        .label(if state.pending {
                            "Adding account…"
                        } else {
                            "Continue"
                        })
                        .disabled(state.pending || state.name.trim().is_empty())
                        .on_click(cx.listener(|this, _, _, cx| this.create_codex_account(cx)))
                        .into_any_element(),
                ],
            )
        };
        let dismiss = self.cb(cx, |this, _, cx| this.close_add_codex_account(cx));
        Some(wizard_popup(
            &theme,
            WizardSize::Wide,
            vec![
                wizard_header(
                    &theme,
                    title,
                    Some("Each account has its own Codex instance and sign-in. Choose the other account on the sign-in page.".into()),
                    Vec::new(),
                ),
                wizard_panel(&theme, None, body),
                footer,
            ],
            dismiss,
        ))
    }
}

#[cfg(test)]
mod tests {
    use turin_contracts::provider_instance::ProviderInstanceId;

    use super::*;

    #[test]
    fn names_the_account_after_the_trimmed_input() {
        assert_eq!(account_display_name("  Work "), "ChatGPT - Work");
        assert_eq!(account_display_name("Personal"), "ChatGPT - Personal");
    }

    #[test]
    fn creates_a_managed_enabled_codex_instance() {
        let instance = new_account_instance("ChatGPT - Work");
        assert_eq!(
            instance,
            json!({
                "driver": "codex",
                "displayName": "ChatGPT - Work",
                "enabled": true,
                "config": { "enabled": true, "setupMode": "managed" },
            })
        );
        let decoded: turin_contracts::provider_instance::ProviderInstanceConfig =
            serde_json::from_value(instance).expect("instance decodes");
        assert_eq!(decoded.driver.as_str(), "codex");
    }

    #[test]
    fn routes_by_a_unique_codex_id_that_is_a_valid_instance_id() {
        let first = new_account_instance_id();
        let second = new_account_instance_id();
        assert_ne!(first, second);
        assert!(first.starts_with("codex_"));
        assert!(ProviderInstanceId::new(first.as_str()).is_ok());
    }

    #[test]
    fn starts_on_the_personal_name() {
        let state = AddCodexAccountState::default();
        assert_eq!(state.name, "Personal");
        assert_eq!(state.instance_id, None);
        assert!(!state.pending);
    }
}
