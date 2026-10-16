//! Port of upstream/apps/web/src/components/settings/ProviderAuthCallbackCoordinator.tsx
//! and upstream/apps/web/src/providerAuthDelivery.ts.
//!
//! Upstream's hosted web build receives only the one-time code in the page
//! address and the selected environment verifies and stores the tokens. Turin
//! has no hosted page: the source hands the code over as a
//! [`ProviderAuthDelivery`] (an OS URL-scheme or loopback receipt, wired by
//! T22) and this coordinator completes the flow once, then clears the delivery.
//!
//! Deviation: upstream keeps its `started` ref set for the component's whole
//! life, so a second delivery in one session would never be completed. Turin
//! resets it once the delivery is cleared, since deliveries can repeat.

use gpui_kit::Context;

use super::view::{SettingsToastKind, SettingsView};
use super::view_model::ProviderAuthDelivery;

/// Whether the coordinator should complete `delivery` now: there is one, it has
/// not been started, and its environment is connected (always, in-process).
pub fn should_complete(
    delivery: Option<&ProviderAuthDelivery>,
    connected: bool,
    started: bool,
) -> bool {
    delivery.is_some() && connected && !started
}

/// The toast for a completion that the environment rejected.
pub fn failure_toast() -> (&'static str, &'static str) {
    (
        "ChatGPT sign-in couldn't finish",
        "Return to the provider and try again.",
    )
}

/// The toast for a completion that could not be sent at all.
pub fn unreachable_toast() -> (&'static str, &'static str) {
    (
        "ChatGPT sign-in couldn't finish",
        "Reconnect to the environment and try again.",
    )
}

impl SettingsView {
    /// `<ProviderAuthCallbackCoordinator />`, evaluated each render.
    pub(super) fn provider_auth_callback_coordinator(&mut self, cx: &mut Context<Self>) {
        let vm = self.vm(cx);
        let delivery = vm.pending_provider_auth_delivery.as_ref();
        if !should_complete(delivery, true, self.panels.providers.auth_delivery_started) {
            return;
        }
        let Some(delivery) = delivery.cloned() else {
            return;
        };
        self.panels.providers.auth_delivery_started = true;
        let source = self.store.read(cx).source();
        let result = source.complete_provider_auth(
            &delivery.instance_id,
            &delivery.flow_id,
            &delivery.callback_url,
            cx,
        );
        if result.is_err() {
            let (title, description) = failure_toast();
            self.toast(SettingsToastKind::Error, title, description, cx);
        }
        source.clear_provider_auth_delivery(cx);
        self.panels.providers.auth_delivery_started = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delivery() -> ProviderAuthDelivery {
        ProviderAuthDelivery {
            instance_id: "codex".into(),
            flow_id: "flow-1".into(),
            callback_url: "http://localhost:1455/auth/callback?code=abc".into(),
        }
    }

    #[test]
    fn completes_a_pending_delivery_once_connected() {
        let delivery = delivery();
        assert!(should_complete(Some(&delivery), true, false));
        assert!(!should_complete(Some(&delivery), false, false));
        assert!(!should_complete(Some(&delivery), true, true));
        assert!(!should_complete(None, true, false));
    }

    #[test]
    fn explains_a_failed_completion() {
        assert_eq!(
            failure_toast(),
            (
                "ChatGPT sign-in couldn't finish",
                "Return to the provider and try again."
            )
        );
        assert_eq!(
            unreachable_toast(),
            (
                "ChatGPT sign-in couldn't finish",
                "Reconnect to the environment and try again."
            )
        );
    }
}
