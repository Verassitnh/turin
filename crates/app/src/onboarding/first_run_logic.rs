//! Port of upstream/apps/web/src/onboarding/firstRun.logic.ts

use turin_shared::path::normalize_project_path_for_comparison;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstRunDecision {
    Pending,
    App,
    Wizard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirstRunGateState {
    pub decision: FirstRunDecision,
    pub stalled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstRunGateEvent {
    Evidence { decision: FirstRunDecision },
    Timeout,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirstRunProject {
    pub id: String,
    pub environment_id: String,
    pub workspace_root: String,
}

/// The thread fields the fresh-workspace check reads. `latestRun` and
/// `runtime` are compared against `null` upstream, so only their presence matters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirstRunThread {
    pub id: String,
    pub project_id: String,
    pub environment_id: String,
    pub has_latest_run: bool,
    pub latest_user_message_at: Option<String>,
    pub has_runtime: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FirstRunWorkspaceInput {
    pub primary_environment_id: Option<String>,
    pub server_cwd: Option<String>,
    pub bootstrap_project_id: Option<String>,
    pub bootstrap_thread_id: Option<String>,
    pub bootstrap_project_created: Option<bool>,
    pub bootstrap_thread_created: Option<bool>,
    pub projects: Vec<FirstRunProject>,
    pub threads: Vec<FirstRunThread>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirstRunDecisionInput {
    pub enabled: bool,
    pub hydrated: bool,
    pub completed: bool,
    pub bootstrapped: bool,
    pub authoritative: bool,
    pub workspace_authoritative: bool,
    pub workspace_provenance_authoritative: bool,
    pub catalog_ready: bool,
    pub server_config_available: bool,
    pub workspace_fresh: bool,
    pub project_count: usize,
    pub thread_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostedFirstRunDecisionInput {
    pub local_environment_disabled: bool,
    pub hydrated: bool,
    pub completed: bool,
    pub catalog_ready: bool,
    pub environment_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirstRunResolution {
    pub decision: FirstRunDecision,
    pub persist_completion: bool,
}

const fn resolution(decision: FirstRunDecision, persist_completion: bool) -> FirstRunResolution {
    FirstRunResolution {
        decision,
        persist_completion,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootstrapStatus {
    Pending,
    Complete,
}

pub fn is_first_run_workspace_provenance_authoritative(
    welcome_received: bool,
    bootstrap_status: Option<BootstrapStatus>,
) -> bool {
    // An empty catalog is not final while cwd auto-bootstrap is pending. Older
    // servers omit bootstrapStatus, so a received welcome with null stays valid.
    welcome_received && bootstrap_status != Some(BootstrapStatus::Pending)
}

/// Keeps the authenticated app unmounted until workspace evidence settles.
pub fn transition_first_run_gate_state(
    state: FirstRunGateState,
    event: FirstRunGateEvent,
) -> FirstRunGateState {
    let decision = match event {
        FirstRunGateEvent::Timeout => {
            return if state.decision == FirstRunDecision::Pending && !state.stalled {
                FirstRunGateState {
                    stalled: true,
                    ..state
                }
            } else {
                state
            };
        }
        FirstRunGateEvent::Evidence { decision } => decision,
    };

    if state.decision == FirstRunDecision::Wizard
        || decision == FirstRunDecision::Pending
        || (state.decision == FirstRunDecision::App && decision != FirstRunDecision::Wizard)
    {
        return state;
    }

    FirstRunGateState {
        decision,
        stalled: false,
    }
}

/// Only a project and thread created by this startup count as a fresh nonempty workspace.
pub fn is_fresh_first_run_workspace(input: &FirstRunWorkspaceInput) -> bool {
    if input.projects.len() > 1 || input.threads.len() > 1 {
        return false;
    }

    let bootstrap_project = input.projects.first();
    if let Some(project) = bootstrap_project {
        let Some(server_cwd) = input.server_cwd.as_deref() else {
            return false;
        };
        if input.bootstrap_project_created != Some(true)
            || input.bootstrap_project_id.as_deref() != Some(project.id.as_str())
            || Some(project.environment_id.as_str()) != input.primary_environment_id.as_deref()
            || normalize_project_path_for_comparison(&project.workspace_root)
                != normalize_project_path_for_comparison(server_cwd)
        {
            return false;
        }
    }

    let Some(thread) = input.threads.first() else {
        return true;
    };

    bootstrap_project.is_some_and(|project| {
        input.bootstrap_thread_created == Some(true)
            && input.bootstrap_thread_id.as_deref() == Some(thread.id.as_str())
            && Some(thread.environment_id.as_str()) == input.primary_environment_id.as_deref()
            && thread.project_id == project.id
            && !thread.has_latest_run
            && thread.latest_user_message_at.is_none()
            && !thread.has_runtime
    })
}

/// Cached projects may open the app, but only live workspace data may complete onboarding.
pub fn resolve_first_run_decision(input: &FirstRunDecisionInput) -> FirstRunResolution {
    if !input.enabled || (input.hydrated && input.completed) {
        return resolution(FirstRunDecision::App, false);
    }

    if !input.hydrated {
        return resolution(FirstRunDecision::Pending, false);
    }

    if input.project_count > 1 || input.thread_count > 1 {
        return resolution(
            FirstRunDecision::App,
            input.bootstrapped
                && input.authoritative
                && input.workspace_authoritative
                && input.catalog_ready
                && input.server_config_available,
        );
    }

    if !input.bootstrapped
        || !input.authoritative
        || !input.workspace_provenance_authoritative
        || !input.catalog_ready
        || !input.server_config_available
    {
        return resolution(FirstRunDecision::Pending, false);
    }

    if input.workspace_fresh {
        resolution(FirstRunDecision::Wizard, false)
    } else {
        resolution(FirstRunDecision::App, input.workspace_authoritative)
    }
}

/// Hosted onboarding depends on saved environments because there is no primary server.
pub fn resolve_hosted_first_run_decision(
    input: &HostedFirstRunDecisionInput,
) -> FirstRunResolution {
    if !input.hydrated {
        return resolution(FirstRunDecision::Pending, false);
    }

    if input.completed {
        return resolution(FirstRunDecision::App, false);
    }

    if !input.catalog_ready {
        return resolution(FirstRunDecision::Pending, false);
    }

    // An existing desktop may have disabled its server before onboarding existed.
    // Keep Connections accessible so it can turn local execution back on.
    if input.environment_count == 0 && !input.local_environment_disabled {
        resolution(FirstRunDecision::Wizard, false)
    } else {
        resolution(FirstRunDecision::App, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use FirstRunDecision::{App, Pending, Wizard};

    const FRESH: FirstRunDecisionInput = FirstRunDecisionInput {
        enabled: true,
        hydrated: true,
        completed: false,
        bootstrapped: true,
        authoritative: true,
        workspace_authoritative: true,
        workspace_provenance_authoritative: true,
        catalog_ready: true,
        server_config_available: true,
        workspace_fresh: true,
        project_count: 1,
        thread_count: 1,
    };

    fn decide(input: FirstRunDecisionInput) -> (FirstRunDecision, bool) {
        let result = resolve_first_run_decision(&input);
        (result.decision, result.persist_completion)
    }

    #[test]
    fn opens_the_wizard_for_an_authoritative_fresh_workspace() {
        assert_eq!(decide(FRESH), (Wizard, false));
    }

    #[test]
    fn does_not_permanently_complete_onboarding_from_cached_project_counts() {
        assert_eq!(
            decide(FirstRunDecisionInput {
                authoritative: false,
                project_count: 3,
                workspace_fresh: false,
                ..FRESH
            }),
            (App, false)
        );
    }

    #[test]
    fn backfills_completion_once_existing_projects_are_confirmed_by_the_server() {
        assert_eq!(
            decide(FirstRunDecisionInput {
                project_count: 3,
                workspace_fresh: false,
                ..FRESH
            }),
            (App, true)
        );
    }

    #[test]
    fn does_not_complete_onboarding_while_another_environment_is_still_bootstrapping() {
        assert_eq!(
            decide(FirstRunDecisionInput {
                bootstrapped: false,
                project_count: 3,
                workspace_fresh: false,
                ..FRESH
            }),
            (App, false)
        );
    }

    #[test]
    fn waits_for_managed_environments_before_treating_a_workspace_as_new() {
        assert_eq!(
            decide(FirstRunDecisionInput {
                catalog_ready: false,
                ..FRESH
            }),
            (Pending, false)
        );
    }

    #[test]
    fn does_not_complete_onboarding_before_the_environment_catalog_is_ready() {
        assert_eq!(
            decide(FirstRunDecisionInput {
                catalog_ready: false,
                project_count: 3,
                workspace_fresh: false,
                ..FRESH
            }),
            (App, false)
        );
    }

    #[test]
    fn does_not_complete_onboarding_before_the_server_configuration_is_available() {
        assert_eq!(
            decide(FirstRunDecisionInput {
                project_count: 3,
                server_config_available: false,
                workspace_fresh: false,
                ..FRESH
            }),
            (App, false)
        );
    }

    #[test]
    fn does_not_complete_onboarding_from_cached_remote_projects() {
        assert_eq!(
            decide(FirstRunDecisionInput {
                project_count: 3,
                workspace_authoritative: false,
                workspace_fresh: false,
                ..FRESH
            }),
            (App, false)
        );
    }

    #[test]
    fn does_not_complete_onboarding_from_a_single_cached_remote_project() {
        assert_eq!(
            decide(FirstRunDecisionInput {
                workspace_authoritative: false,
                workspace_fresh: false,
                ..FRESH
            }),
            (App, false)
        );
    }

    #[test]
    fn waits_for_live_data_before_judging_a_single_cached_project() {
        assert_eq!(
            decide(FirstRunDecisionInput {
                authoritative: false,
                workspace_fresh: false,
                ..FRESH
            }),
            (Pending, false)
        );
    }

    #[test]
    fn waits_for_the_completed_bootstrap_welcome_before_judging_a_nonempty_workspace() {
        assert_eq!(
            decide(FirstRunDecisionInput {
                workspace_provenance_authoritative: false,
                ..FRESH
            }),
            (Pending, false)
        );
    }

    #[test]
    fn waits_when_the_initial_welcome_is_pending_and_opens_the_wizard_after_completion() {
        let pending =
            is_first_run_workspace_provenance_authoritative(true, Some(BootstrapStatus::Pending));
        assert_eq!(
            decide(FirstRunDecisionInput {
                workspace_provenance_authoritative: pending,
                ..FRESH
            }),
            (Pending, false)
        );
        let completed =
            is_first_run_workspace_provenance_authoritative(true, Some(BootstrapStatus::Complete));
        assert_eq!(
            decide(FirstRunDecisionInput {
                workspace_provenance_authoritative: completed,
                ..FRESH
            }),
            (Wizard, false)
        );
    }

    #[test]
    fn does_not_wait_for_server_data_after_onboarding_is_already_complete() {
        assert_eq!(
            decide(FirstRunDecisionInput {
                authoritative: false,
                bootstrapped: false,
                completed: true,
                server_config_available: false,
                ..FRESH
            }),
            (App, false)
        );
    }

    #[test]
    fn disabled_gate_opens_the_app() {
        assert_eq!(
            decide(FirstRunDecisionInput {
                enabled: false,
                hydrated: false,
                ..FRESH
            }),
            (App, false)
        );
    }

    #[test]
    fn provenance_waits_for_cwd_bootstrap_and_a_welcome() {
        let check = is_first_run_workspace_provenance_authoritative;
        assert!(!check(true, Some(BootstrapStatus::Pending)));
        assert!(check(true, Some(BootstrapStatus::Complete)));
        assert!(!check(false, None));
        assert!(check(true, None));
    }

    fn state(decision: FirstRunDecision, stalled: bool) -> FirstRunGateState {
        FirstRunGateState { decision, stalled }
    }

    #[test]
    fn shows_recovery_without_mounting_the_app_when_evidence_stalls() {
        assert_eq!(
            transition_first_run_gate_state(state(Pending, false), FirstRunGateEvent::Timeout),
            state(Pending, true)
        );
        // A second timeout, or one after a decision, changes nothing.
        assert_eq!(
            transition_first_run_gate_state(state(Pending, true), FirstRunGateEvent::Timeout),
            state(Pending, true)
        );
        assert_eq!(
            transition_first_run_gate_state(state(App, false), FirstRunGateEvent::Timeout),
            state(App, false)
        );
    }

    #[test]
    fn resolves_stalled_recovery_only_after_authoritative_evidence() {
        for decision in [App, Wizard] {
            assert_eq!(
                transition_first_run_gate_state(
                    state(Pending, true),
                    FirstRunGateEvent::Evidence { decision }
                ),
                state(decision, false)
            );
        }
    }

    #[test]
    fn keeps_recovery_visible_while_evidence_remains_pending() {
        let stalled = state(Pending, true);
        assert_eq!(
            transition_first_run_gate_state(
                stalled,
                FirstRunGateEvent::Evidence { decision: Pending }
            ),
            stalled
        );
    }

    #[test]
    fn allows_authoritative_wizard_evidence_to_replace_an_app_decision() {
        assert_eq!(
            transition_first_run_gate_state(
                state(App, false),
                FirstRunGateEvent::Evidence { decision: Wizard }
            ),
            state(Wizard, false)
        );
        // A wizard decision sticks, and an app decision is not replaced by app.
        assert_eq!(
            transition_first_run_gate_state(
                state(Wizard, false),
                FirstRunGateEvent::Evidence { decision: App }
            ),
            state(Wizard, false)
        );
    }

    fn hosted(
        hydrated: bool,
        completed: bool,
        catalog_ready: bool,
        environment_count: usize,
        local_environment_disabled: bool,
    ) -> (FirstRunDecision, bool) {
        let result = resolve_hosted_first_run_decision(&HostedFirstRunDecisionInput {
            local_environment_disabled,
            hydrated,
            completed,
            catalog_ready,
            environment_count,
        });
        (result.decision, result.persist_completion)
    }

    #[test]
    fn hosted_keeps_the_shell_hidden_until_client_settings_are_hydrated() {
        assert_eq!(hosted(false, false, true, 0, false), (Pending, false));
    }

    #[test]
    fn hosted_waits_for_the_saved_environment_catalog() {
        assert_eq!(hosted(true, false, false, 0, false), (Pending, false));
    }

    #[test]
    fn hosted_opens_onboarding_when_no_environments_are_saved() {
        assert_eq!(hosted(true, false, true, 0, false), (Wizard, false));
    }

    #[test]
    fn hosted_keeps_connections_reachable_for_a_remote_only_desktop() {
        assert_eq!(hosted(true, false, true, 0, true), (App, true));
    }

    #[test]
    fn hosted_backfills_onboarding_for_saved_environments() {
        assert_eq!(hosted(true, false, true, 1, false), (App, true));
    }

    #[test]
    fn hosted_opens_the_app_immediately_after_onboarding_is_complete() {
        assert_eq!(hosted(true, true, false, 0, false), (App, false));
    }

    const PRIMARY: &str = "primary-environment";

    fn project() -> FirstRunProject {
        FirstRunProject {
            id: "bootstrap-project".to_owned(),
            environment_id: PRIMARY.to_owned(),
            workspace_root: "/projects/current".to_owned(),
        }
    }

    fn thread() -> FirstRunThread {
        FirstRunThread {
            id: "bootstrap-thread".to_owned(),
            project_id: "bootstrap-project".to_owned(),
            environment_id: PRIMARY.to_owned(),
            has_latest_run: false,
            latest_user_message_at: None,
            has_runtime: false,
        }
    }

    fn workspace(
        server_cwd: &str,
        projects: Vec<FirstRunProject>,
        threads: Vec<FirstRunThread>,
    ) -> FirstRunWorkspaceInput {
        FirstRunWorkspaceInput {
            primary_environment_id: Some(PRIMARY.to_owned()),
            server_cwd: Some(server_cwd.to_owned()),
            projects,
            threads,
            ..Default::default()
        }
    }

    fn with_bootstrap(mut input: FirstRunWorkspaceInput, created: bool) -> FirstRunWorkspaceInput {
        input.bootstrap_project_id = Some("bootstrap-project".to_owned());
        input.bootstrap_thread_id = Some("bootstrap-thread".to_owned());
        input.bootstrap_project_created = Some(created);
        input.bootstrap_thread_created = Some(created);
        input
    }

    #[test]
    fn accepts_an_empty_workspace() {
        assert!(is_fresh_first_run_workspace(&workspace(
            "/projects/current",
            vec![],
            vec![]
        )));
    }

    #[test]
    fn accepts_only_the_unused_project_and_thread_created_from_the_server_cwd() {
        let input = with_bootstrap(
            workspace("/projects/current/", vec![project()], vec![thread()]),
            true,
        );
        assert!(is_fresh_first_run_workspace(&input));
    }

    #[test]
    fn rejects_an_existing_unused_cwd_project_and_thread_reused_by_startup() {
        let input = with_bootstrap(
            workspace("/projects/current", vec![project()], vec![thread()]),
            false,
        );
        assert!(!is_fresh_first_run_workspace(&input));
    }

    #[test]
    fn rejects_a_nonempty_workspace_when_an_older_server_omits_creation_provenance() {
        let mut input = with_bootstrap(
            workspace("/projects/current", vec![project()], vec![thread()]),
            true,
        );
        input.bootstrap_project_created = None;
        input.bootstrap_thread_created = None;
        assert!(!is_fresh_first_run_workspace(&input));
    }

    #[test]
    fn normalizes_windows_project_paths_before_checking_the_bootstrap_workspace() {
        let mut windows_project = project();
        windows_project.workspace_root = "c:/projects/current".to_owned();
        let input = with_bootstrap(
            workspace(
                "C:\\Projects\\Current\\",
                vec![windows_project],
                vec![thread()],
            ),
            true,
        );
        assert!(is_fresh_first_run_workspace(&input));
    }

    #[test]
    fn rejects_projects_and_threads_from_another_environment() {
        let mut remote_project = project();
        remote_project.environment_id = "remote-environment".to_owned();
        assert!(!is_fresh_first_run_workspace(&workspace(
            "/projects/current",
            vec![remote_project],
            vec![]
        )));
        let mut remote_thread = thread();
        remote_thread.environment_id = "remote-environment".to_owned();
        assert!(!is_fresh_first_run_workspace(&workspace(
            "/projects/current",
            vec![project()],
            vec![remote_thread]
        )));
    }

    #[test]
    fn rejects_a_thread_that_does_not_belong_to_the_bootstrap_project() {
        let mut other = thread();
        other.project_id = "another-project".to_owned();
        assert!(!is_fresh_first_run_workspace(&workspace(
            "/projects/current",
            vec![project()],
            vec![other]
        )));
    }

    #[test]
    fn rejects_a_thread_when_there_is_no_bootstrap_project() {
        assert!(!is_fresh_first_run_workspace(&workspace(
            "/projects/current",
            vec![],
            vec![thread()]
        )));
    }

    #[test]
    fn rejects_a_bootstrap_thread_that_has_been_used() {
        let used = [
            FirstRunThread {
                latest_user_message_at: Some("2026-08-23T12:00:00.000Z".to_owned()),
                ..thread()
            },
            FirstRunThread {
                has_latest_run: true,
                ..thread()
            },
            FirstRunThread {
                has_runtime: true,
                ..thread()
            },
        ];
        for used_thread in used {
            assert!(!is_fresh_first_run_workspace(&workspace(
                "/projects/current",
                vec![project()],
                vec![used_thread]
            )));
        }
    }
}
