//! Port of upstream/apps/web/src/components/ProviderUpdatePrimaryNotification.tsx
#![allow(dead_code)]
//!
//! Toast orchestration stays in the host; this module exposes the pure
//! decisions the effect hooks use.

use super::provider_update_dismissal::ProviderUpdateDismissalsStore;
use super::provider_update_launch_notification_logic::{
    ProviderUpdateToastView, can_one_click_update_provider_candidate,
    collect_provider_update_candidates, get_provider_update_initial_toast_view,
    get_provider_update_progress_toast_view, get_provider_update_rejected_toast_view,
    provider_update_notification_key, should_show_primary_provider_update_toast,
};
use turin_provider_core::server_provider::ServerProvider;

pub const SETTLING_GRACE_MS: u64 = 30_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrimaryProviderUpdateToastAction {
    ShowInitial(ProviderUpdateToastView),
    ShowProgress(ProviderUpdateToastView),
    ShowRejected(ProviderUpdateToastView),
    Close,
}

pub fn primary_provider_update_toast_action(
    providers: &[ServerProvider],
    dismissed: &mut dyn ProviderUpdateDismissalsStore,
    notification_key: Option<&str>,
    already_seen: bool,
) -> Option<PrimaryProviderUpdateToastAction> {
    let candidates = collect_provider_update_candidates(providers);
    let key = provider_update_notification_key(&candidates)?;
    if Some(key.as_str()) != notification_key {
        return Some(PrimaryProviderUpdateToastAction::Close);
    }
    if super::provider_update_dismissal::is_provider_update_notification_dismissed(
        dismissed,
        notification_key,
    ) || already_seen
    {
        return None;
    }
    let one_click: Vec<&ServerProvider> = candidates
        .iter()
        .copied()
        .filter(|candidate| can_one_click_update_provider_candidate(candidate, providers))
        .collect();
    let view = get_provider_update_initial_toast_view(&candidates, &one_click);
    if should_show_primary_provider_update_toast(&view) {
        Some(PrimaryProviderUpdateToastAction::ShowInitial(view))
    } else {
        None
    }
}

pub fn primary_provider_update_progress_view(
    providers: &[ServerProvider],
    provider_count: usize,
) -> ProviderUpdateToastView {
    get_provider_update_progress_toast_view(providers, provider_count)
}

pub fn primary_provider_update_rejected_view(
    provider_count: usize,
    message: &str,
) -> ProviderUpdateToastView {
    get_provider_update_rejected_toast_view(provider_count, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::provider_update_dismissal::InMemoryProviderUpdateDismissalsStore;

    #[test]
    fn skips_toast_when_notification_key_is_missing() {
        assert!(
            primary_provider_update_toast_action(
                &[],
                &mut InMemoryProviderUpdateDismissalsStore::default(),
                None,
                false,
            )
            .is_none()
        );
    }
}
