//! Port of upstream/apps/web/src/components/settings/ChatGptWelcomeCoordinator.tsx.
//!
//! Reads the verified provider snapshot, never the browser callback
//! acknowledgment: once a provider reports ChatGPT plan sharing, the user sees
//! a one-time welcome per profile.
//!
//! Deviation: upstream mounts this at the app root (and the Welcome wizard).
//! Until the app shell hosts it (S5), the settings view renders the dialog;
//! [`next_welcome_profile`] is the shared logic. Acknowledged keys live in the
//! source (`t3:chatgpt-sharing-welcome:v1`, a localStorage list upstream).

use gpui_kit::{
    AnyElement, Context, IntoElement as _, ParentElement as _, Styled as _, Window, div, rems,
};
use turin_provider_core::server_provider::{ServerProvider, ServerProviderAuthStatus};

use super::dialog::{dialog_footer, dialog_frame};
use super::view::SettingsView;
use super::view_model::SettingsViewModel;
use crate::assets::PROVIDER_OPENAI;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{TEXT_SM, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon};

/// The environment id the single local environment carries in keys.
pub const LOCAL_ENVIRONMENT_ID: &str = "local";

/// `usesChatGptSharing`: verified sharing metadata, not the driver or the login type.
pub fn uses_chat_gpt_sharing(provider: &ServerProvider) -> bool {
    provider.auth.status == ServerProviderAuthStatus::Authenticated
        && provider.auth.subscription_sharing == Some(true)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WelcomeProfile {
    /// `JSON.stringify([environmentId, instanceId, profileId ?? email ?? "default"])`.
    pub key: String,
    pub environment_label: String,
    pub provider_name: String,
}

/// The profiles to welcome: every provider sharing a ChatGPT plan.
pub fn welcome_profiles(
    providers: &[ServerProvider],
    environment_id: &str,
    environment_label: &str,
) -> Vec<WelcomeProfile> {
    providers
        .iter()
        .filter(|provider| uses_chat_gpt_sharing(provider))
        .map(|provider| {
            let profile = provider
                .auth
                .profile_id
                .as_ref()
                .or(provider.auth.email.as_ref())
                .map(|value| value.as_str())
                .unwrap_or("default");
            WelcomeProfile {
                key: serde_json::to_string(&[
                    environment_id,
                    provider.instance_id.as_str(),
                    profile,
                ])
                .expect("welcome key encodes"),
                environment_label: environment_label.to_owned(),
                provider_name: provider
                    .display_name
                    .as_ref()
                    .map(|name| name.as_str().to_owned())
                    .unwrap_or_else(|| "Codex".to_owned()),
            }
        })
        .collect()
}

/// `profiles.find(profile => !acknowledged.includes(profile.key))`.
pub fn next_welcome_profile(
    profiles: Vec<WelcomeProfile>,
    acknowledged: &[String],
) -> Option<WelcomeProfile> {
    profiles
        .into_iter()
        .find(|profile| !acknowledged.contains(&profile.key))
}

impl SettingsView {
    pub(super) fn next_chat_gpt_welcome(&self, vm: &SettingsViewModel) -> Option<WelcomeProfile> {
        next_welcome_profile(
            welcome_profiles(&vm.providers, LOCAL_ENVIRONMENT_ID, &vm.environment.label),
            &vm.chat_gpt_welcome_acknowledged,
        )
    }

    /// `<ChatGptWelcomeCoordinator />`'s dialog.
    pub(super) fn render_chat_gpt_welcome_dialog(
        &mut self,
        profile: WelcomeProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let _ = window;
        let theme = cx.app_theme().clone();
        let key = profile.key.clone();
        let dismiss = {
            let key = key.clone();
            self.cb(cx, move |this, _, cx| {
                this.store
                    .read(cx)
                    .source()
                    .acknowledge_chat_gpt_welcome(&key, cx);
            })
        };
        let continue_key = key;
        dialog_frame(
            &theme,
            rems(32.),
            vec![
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_6()
                    .child(
                        div().mb_2().child(
                            Icon::path(PROVIDER_OPENAI)
                                .size(rems(2.))
                                .color(theme.foreground),
                        ),
                    )
                    .child(
                        div()
                            .text_size(rems(1.25))
                            .line_height(gpui_kit::relative(1.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child("Your ChatGPT plan is connected"),
                    )
                    .child(
                        div()
                            .text_size(TEXT_SM.size)
                            .line_height(TEXT_SM.line_height)
                            .text_color(theme.muted_foreground)
                            .child("Eligible usage in T3 Code uses your ChatGPT plan. Manage your shared usage and any credit settings in ChatGPT."),
                    )
                    .child(
                        div()
                            .text_size(TEXT_XS.size)
                            .line_height(TEXT_XS.line_height)
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "{} on {}",
                                profile.provider_name, profile.environment_label
                            )),
                    )
                    .into_any_element(),
                dialog_footer(
                    &theme,
                    vec![
                        self.chat_gpt_usage_button("chatgpt-welcome-usage", cx),
                        Button::new("chatgpt-welcome-continue")
                            .variant(ButtonVariant::Default)
                            .size(ButtonSize::Default)
                            .label("Continue")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.store
                                    .read(cx)
                                    .source()
                                    .acknowledge_chat_gpt_welcome(&continue_key, cx);
                            }))
                            .into_any_element(),
                    ],
                ),
            ],
            dismiss,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider(
        instance: &str,
        sharing: Option<bool>,
        status: &str,
        extra: serde_json::Value,
    ) -> ServerProvider {
        let mut auth = json!({ "status": status });
        if let Some(sharing) = sharing {
            auth["subscriptionSharing"] = json!(sharing);
        }
        if let (Some(auth), Some(extra)) = (auth.as_object_mut(), extra.as_object()) {
            auth.extend(extra.clone());
        }
        serde_json::from_value(json!({
            "instanceId": instance, "driver": "codex", "enabled": true, "installed": true,
            "status": "ready", "auth": auth,
            "checkedAt": "2026-10-15T10:00:00.000Z",
            "models": [], "slashCommands": [], "skills": []
        }))
        .expect("provider")
    }

    #[test]
    fn welcomes_only_verified_chat_gpt_sharing() {
        let providers = vec![
            provider(
                "codex",
                Some(true),
                "authenticated",
                json!({ "email": "me@example.com" }),
            ),
            provider("codex_work", Some(false), "authenticated", json!({})),
            provider("codex_old", Some(true), "unauthenticated", json!({})),
            provider("claudeAgent", None, "authenticated", json!({})),
        ];
        let profiles = welcome_profiles(&providers, "local", "This device");
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].key, r#"["local","codex","me@example.com"]"#);
        assert_eq!(profiles[0].provider_name, "Codex");
        assert_eq!(profiles[0].environment_label, "This device");
    }

    #[test]
    fn keys_a_profile_by_profile_id_then_email_then_default() {
        let with_profile = provider(
            "codex",
            Some(true),
            "authenticated",
            json!({ "profileId": "p1", "email": "me@example.com" }),
        );
        let bare = provider("codex_b", Some(true), "authenticated", json!({}));
        let keys: Vec<String> = welcome_profiles(&[with_profile, bare], "local", "x")
            .into_iter()
            .map(|profile| profile.key)
            .collect();
        assert_eq!(
            keys,
            vec![
                r#"["local","codex","p1"]"#.to_owned(),
                r#"["local","codex_b","default"]"#.to_owned()
            ]
        );
    }

    #[test]
    fn shows_the_first_profile_that_was_not_acknowledged() {
        let providers = vec![
            provider("codex", Some(true), "authenticated", json!({})),
            provider("codex_work", Some(true), "authenticated", json!({})),
        ];
        let profiles = welcome_profiles(&providers, "local", "This device");
        let first = profiles[0].key.clone();
        assert_eq!(
            next_welcome_profile(profiles.clone(), &[]).map(|p| p.key),
            Some(first.clone())
        );
        assert_eq!(
            next_welcome_profile(profiles.clone(), std::slice::from_ref(&first)).map(|p| p.key),
            Some(profiles[1].key.clone())
        );
        assert_eq!(
            next_welcome_profile(profiles.clone(), &[first, profiles[1].key.clone()]),
            None
        );
    }

    #[test]
    fn names_the_provider_after_its_display_name() {
        let mut named = provider("codex_work", Some(true), "authenticated", json!({}));
        named.display_name = Some(turin_contracts::TrimmedNonEmptyString::new("Work").unwrap());
        let profiles = welcome_profiles(&[named], "local", "This device");
        assert_eq!(profiles[0].provider_name, "Work");
    }
}
