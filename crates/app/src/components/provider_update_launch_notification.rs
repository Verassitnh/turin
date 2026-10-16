//! Port of upstream/apps/web/src/components/ProviderUpdateLaunchNotification.tsx
#![allow(dead_code)]

use super::provider_update_launch_notification_environments::LocalEnvironmentUpdateGroupsView;
use super::provider_update_launch_notification_logic::{
    ProviderUpdateToastView, collect_provider_update_candidates, environment_groups_with_updates,
    get_provider_update_initial_toast_view, local_environment_update_notification_key,
};
use super::provider_update_primary_notification::SETTLING_GRACE_MS;
use turin_provider_core::server_provider::ServerProvider;

pub fn has_local_secondary_environment(_entries: &[()]) -> bool {
    false
}

pub fn launch_notification_mode(has_local_secondary: bool) -> LaunchNotificationMode {
    if has_local_secondary {
        LaunchNotificationMode::Environments
    } else {
        LaunchNotificationMode::Primary
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchNotificationMode {
    Primary,
    Environments,
}

pub fn environment_launch_toast_view(
    groups: &LocalEnvironmentUpdateGroupsView,
    is_gated: bool,
) -> Option<ProviderUpdateToastView> {
    if is_gated {
        return None;
    }
    let update_groups: Vec<_> = environment_groups_with_updates(&groups.groups)
        .into_iter()
        .cloned()
        .collect();
    if update_groups.is_empty() {
        return None;
    }
    let providers: Vec<ServerProvider> = update_groups
        .iter()
        .flat_map(|group| group.candidates.iter().cloned())
        .collect();
    let candidates = collect_provider_update_candidates(&providers);
    Some(get_provider_update_initial_toast_view(
        &candidates,
        &candidates,
    ))
}

pub fn environment_launch_notification_key(
    groups: &LocalEnvironmentUpdateGroupsView,
) -> Option<String> {
    local_environment_update_notification_key(&groups.groups)
}

pub fn environment_launch_is_gated(is_any_settling: bool, settle_grace_elapsed: bool) -> bool {
    is_any_settling && !settle_grace_elapsed
}

pub fn environment_launch_settling_grace_ms() -> u64 {
    SETTLING_GRACE_MS
}
