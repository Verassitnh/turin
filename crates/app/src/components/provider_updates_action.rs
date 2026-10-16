//! Port of upstream/apps/web/src/components/ProviderUpdatesAction.tsx
#![allow(dead_code)]

use turin_contracts::EnvironmentId;
use turin_contracts::model::provider_display_name;

use super::provider_update_launch_notification_logic::{
    ProviderUpdateRun, ProviderUpdateRunToastView, can_one_click_update_provider_candidate,
    collect_provider_update_candidates, get_provider_update_run_toast_view,
};
use turin_provider_core::server_provider::ServerProvider;

#[derive(Clone, Debug)]
pub struct ProviderUpdateEnvironmentInput {
    pub environment_id: EnvironmentId,
    pub label: String,
    pub connected: bool,
    pub providers: Vec<ServerProvider>,
}

#[derive(Clone, Debug)]
pub struct ProviderUpdateMachine {
    pub environment_id: EnvironmentId,
    pub label: String,
    pub candidates: Vec<ServerProvider>,
}

pub fn provider_update_machines(
    environments: &[ProviderUpdateEnvironmentInput],
) -> Vec<ProviderUpdateMachine> {
    environments
        .iter()
        .filter(|environment| environment.connected)
        .filter_map(|environment| {
            let candidates: Vec<ServerProvider> =
                collect_provider_update_candidates(&environment.providers)
                    .into_iter()
                    .filter(|candidate| {
                        can_one_click_update_provider_candidate(candidate, &environment.providers)
                    })
                    .cloned()
                    .collect();
            (!candidates.is_empty()).then_some(ProviderUpdateMachine {
                environment_id: environment.environment_id.clone(),
                label: environment.label.clone(),
                candidates,
            })
        })
        .collect()
}

pub fn provider_update_run_toast_view(
    runs: &[ProviderUpdateRun],
    quiet: bool,
) -> Option<ProviderUpdateRunToastView> {
    if quiet {
        return None;
    }
    get_provider_update_run_toast_view(runs)
}

pub fn provider_update_action_tooltip(machines: &[ProviderUpdateMachine]) -> String {
    machines
        .iter()
        .map(|machine| {
            let names = machine
                .candidates
                .iter()
                .map(|candidate| {
                    provider_display_name(candidate.driver.as_str())
                        .unwrap_or(candidate.driver.as_str())
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}: {names}", machine.label)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether the settings action should stay visible while an update is in flight.
pub fn show_provider_updates_action(machines: &[ProviderUpdateMachine], is_pending: bool) -> bool {
    !machines.is_empty() || is_pending
}

#[cfg(test)]
mod tests {
    use super::*;
    fn environment(connected: bool) -> ProviderUpdateEnvironmentInput {
        ProviderUpdateEnvironmentInput {
            environment_id: EnvironmentId::new("env").unwrap(),
            label: "Local".to_owned(),
            connected,
            providers: Vec::new(),
        }
    }

    #[test]
    fn skips_disconnected_environments() {
        assert!(provider_update_machines(&[environment(false)]).is_empty());
    }

    #[test]
    fn keeps_the_button_visible_while_pending() {
        assert!(show_provider_updates_action(&[], true));
    }
}
