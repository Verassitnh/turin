//! Builds [`FirstRunEvidence`] from the client store and client settings.

use turin_contracts::settings::ClientSettings;

use crate::components::onboarding::backend::{FirstRunEvidence, HydrationStatus};
use crate::onboarding::first_run_logic::{BootstrapStatus, FirstRunProject, FirstRunThread};
use crate::onboarding::project_import_logic::OnboardingKnownProject;
use crate::settings_logic::use_settings::{
    ClientSettingsHydrationStatus, client_settings_hydration_status,
};
use crate::state::client_state::ClientState;
use crate::state::local_environment_id;
use crate::state::shell_state::EnvironmentShellStatus;

pub fn first_run_evidence(
    state: &ClientState,
    settings: &ClientSettings,
    commands_ready: bool,
) -> FirstRunEvidence {
    let hydration = match client_settings_hydration_status() {
        ClientSettingsHydrationStatus::Ready => HydrationStatus::Ready,
        ClientSettingsHydrationStatus::Failed => HydrationStatus::Failed,
        ClientSettingsHydrationStatus::Pending => HydrationStatus::Loading,
    };
    let shell_live = matches!(
        state.shell.status,
        EnvironmentShellStatus::Live | EnvironmentShellStatus::Synchronizing
    );
    let has_shell = state.shell_view.is_some();
    let bootstrapped = shell_live || has_shell;
    FirstRunEvidence {
        hydration,
        completed: settings.onboarding_completed_at.is_some(),
        bootstrapped,
        primary_shell_live: shell_live,
        workspace_evidence_live: has_shell,
        catalog_ready: commands_ready,
        server_config_available: commands_ready,
        welcome_received: has_shell,
        bootstrap_status: Some(BootstrapStatus::Complete),
        server_cwd: std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned()),
        primary_environment_id: Some(local_environment_id()),
        bootstrap_project_id: None,
        bootstrap_thread_id: None,
        bootstrap_project_created: None,
        bootstrap_thread_created: None,
        projects: projects_from_shell(state),
        threads: threads_from_shell(state),
    }
}

fn projects_from_shell(state: &ClientState) -> Vec<FirstRunProject> {
    let Some(view) = &state.shell_view else {
        return Vec::new();
    };
    view.projects
        .iter()
        .map(|project| FirstRunProject {
            id: project.id.to_string(),
            environment_id: project.environment_id.to_string(),
            workspace_root: project.workspace_root.clone(),
        })
        .collect()
}

pub fn known_projects_from_shell(state: &ClientState) -> Vec<OnboardingKnownProject> {
    let Some(view) = &state.shell_view else {
        return Vec::new();
    };
    view.projects
        .iter()
        .map(|project| OnboardingKnownProject {
            id: project.id.clone(),
            environment_id: project.environment_id.clone(),
            workspace_root: project.workspace_root.clone(),
        })
        .collect()
}

fn threads_from_shell(state: &ClientState) -> Vec<FirstRunThread> {
    let Some(view) = &state.shell_view else {
        return Vec::new();
    };
    view.navigation_threads
        .iter()
        .map(|thread| FirstRunThread {
            id: thread.id.to_string(),
            project_id: thread.project_id.to_string(),
            environment_id: thread.environment_id.to_string(),
            has_latest_run: thread.latest_run.is_some(),
            latest_user_message_at: thread.latest_user_message_at.clone(),
            has_runtime: thread.runtime.is_some(),
        })
        .collect()
}
