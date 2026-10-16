//! Port of upstream/apps/web/src/components/ProviderUpdateLaunchNotification.environments.ts
#![allow(dead_code)]
//!
//! Builds local-environment update groups from catalog inputs. WSL and
//! desktop-local secondaries are cut in Turin; only the primary local
//! environment is enumerated (see PORTING.md Deviations).

use turin_contracts::EnvironmentId;
use turin_contracts::environment::ExecutionEnvironmentPlatformOs;

use super::provider_update_launch_notification_logic::{
    EnvironmentUpdateConnectionState, LocalEnvironmentProvidersInput, LocalEnvironmentUpdateGroup,
    build_local_environment_update_groups, derive_environment_display_label,
};

/// Upstream connection phase strings from the environment catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvironmentConnectionPhase {
    Connected,
    Connecting,
    Reconnecting,
    Unsupported,
    Error,
    Offline,
    Available,
    Other,
}

impl EnvironmentConnectionPhase {
    pub fn from_phase_str(phase: Option<&str>) -> Self {
        match phase {
            Some("connected") => Self::Connected,
            Some("connecting") | Some("reconnecting") => Self::Connecting,
            Some("unsupported") | Some("error") => Self::Error,
            Some("offline") => Self::Offline,
            Some("available") => Self::Available,
            Some(_) => Self::Other,
            None => Self::Available,
        }
    }
}

pub fn normalize_connection_state(
    phase: EnvironmentConnectionPhase,
) -> EnvironmentUpdateConnectionState {
    match phase {
        EnvironmentConnectionPhase::Connected => EnvironmentUpdateConnectionState::Ready,
        EnvironmentConnectionPhase::Connecting | EnvironmentConnectionPhase::Reconnecting => {
            EnvironmentUpdateConnectionState::Connecting
        }
        EnvironmentConnectionPhase::Unsupported | EnvironmentConnectionPhase::Error => {
            EnvironmentUpdateConnectionState::Error
        }
        EnvironmentConnectionPhase::Offline => EnvironmentUpdateConnectionState::Disconnected,
        EnvironmentConnectionPhase::Available | EnvironmentConnectionPhase::Other => {
            EnvironmentUpdateConnectionState::Connecting
        }
    }
}

/// One row from the environment catalog the host passes in.
#[derive(Clone, Debug)]
pub struct EnvironmentCatalogEntry {
    pub environment_id: EnvironmentId,
    pub label: String,
    pub is_primary: bool,
    pub connection_phase: EnvironmentConnectionPhase,
    pub platform_os: Option<ExecutionEnvironmentPlatformOs>,
    pub providers: Vec<turin_provider_core::server_provider::ServerProvider>,
}

/// Turin has a single local environment; upstream filters to primary + desktop-local.
pub fn local_environment_catalog_entries(
    entries: &[EnvironmentCatalogEntry],
) -> Vec<EnvironmentCatalogEntry> {
    entries
        .iter()
        .filter(|entry| entry.is_primary)
        .cloned()
        .collect()
}

pub struct LocalEnvironmentUpdateGroupsView {
    pub groups: Vec<LocalEnvironmentUpdateGroup>,
    pub is_any_settling: bool,
}

/// Upstream `useLocalEnvironmentUpdateGroups`.
pub fn build_local_environment_update_groups_view(
    entries: &[EnvironmentCatalogEntry],
    primary_environment_id: Option<&EnvironmentId>,
) -> LocalEnvironmentUpdateGroupsView {
    let mut inputs: Vec<LocalEnvironmentProvidersInput> = Vec::new();
    for environment in local_environment_catalog_entries(entries) {
        let is_primary = primary_environment_id.is_some_and(|id| id == &environment.environment_id)
            || environment.is_primary;
        let label = if is_primary {
            derive_environment_display_label(
                false,
                None,
                environment.platform_os,
                &environment.label,
            )
        } else {
            environment.label.clone()
        };
        let connection_state = if is_primary {
            EnvironmentUpdateConnectionState::Ready
        } else {
            normalize_connection_state(environment.connection_phase)
        };
        inputs.push(LocalEnvironmentProvidersInput {
            environment_id: environment.environment_id.clone(),
            label,
            is_primary,
            connection_state,
            providers: environment.providers.clone(),
        });
    }
    inputs.sort_by(|left, right| {
        right
            .is_primary
            .cmp(&left.is_primary)
            .then_with(|| left.label.cmp(&right.label))
    });
    let (groups, is_any_settling) = build_local_environment_update_groups(&inputs);
    LocalEnvironmentUpdateGroupsView {
        groups,
        is_any_settling,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(
        id: &str,
        primary: bool,
        phase: EnvironmentConnectionPhase,
    ) -> EnvironmentCatalogEntry {
        EnvironmentCatalogEntry {
            environment_id: EnvironmentId::new(id).unwrap(),
            label: id.to_owned(),
            is_primary: primary,
            connection_phase: phase,
            platform_os: Some(ExecutionEnvironmentPlatformOs::Darwin),
            providers: Vec::new(),
        }
    }

    #[test]
    fn primary_environment_is_ready_while_connecting_phases_map_for_secondaries() {
        assert_eq!(
            normalize_connection_state(EnvironmentConnectionPhase::Connected),
            EnvironmentUpdateConnectionState::Ready
        );
        assert_eq!(
            normalize_connection_state(EnvironmentConnectionPhase::Connecting),
            EnvironmentUpdateConnectionState::Connecting
        );
    }

    #[test]
    fn build_view_keeps_only_the_primary_local_environment() {
        let primary = EnvironmentId::new("primary").unwrap();
        let view = build_local_environment_update_groups_view(
            &[
                entry("primary", true, EnvironmentConnectionPhase::Connected),
                entry("wsl", false, EnvironmentConnectionPhase::Connected),
            ],
            Some(&primary),
        );
        assert_eq!(view.groups.len(), 1);
        assert!(view.groups[0].is_primary);
    }
}
