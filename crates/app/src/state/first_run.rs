//! Port of upstream/apps/web/src/onboarding/firstRun.logic.ts
//!
//! Decides whether the app shell opens, the onboarding wizard opens, or the
//! gate stays pending while workspace evidence settles. Turin is one process
//! with one local environment, so the hosted (multi-environment) decision and
//! the remote fields are kept only where the local decision reads them.
// Seams for consumers the shell wires (T22); nothing calls these yet.
#![allow(dead_code)]

use crate::state::projects::normalize_project_path_for_comparison;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstRunDecision {
    Pending,
    App,
    Wizard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstRunGateState {
    pub decision: FirstRunDecision,
    pub stalled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstRunGateEvent {
    Evidence(FirstRunDecision),
    Timeout,
}

/// `bootstrapStatus` of the welcome message. `None` on the caller's side means
/// an older server omitted it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapStatus {
    Pending,
    Complete,
}

/// A project as the first-run check reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstRunProject {
    pub id: String,
    pub environment_id: String,
    pub workspace_root: String,
}

/// A thread as the first-run check reads it. `latestRun`, `runtime` and
/// `latestUserMessageAt` are only checked for `null`, so they are flags here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstRunThread {
    pub id: String,
    pub project_id: String,
    pub environment_id: String,
    pub latest_run_present: bool,
    pub latest_user_message_present: bool,
    pub runtime_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstRunWorkspaceInput<'a> {
    pub primary_environment_id: Option<&'a str>,
    pub server_cwd: Option<&'a str>,
    pub bootstrap_project_id: Option<&'a str>,
    pub bootstrap_thread_id: Option<&'a str>,
    pub bootstrap_project_created: Option<bool>,
    pub bootstrap_thread_created: Option<bool>,
    pub projects: &'a [FirstRunProject],
    pub threads: &'a [FirstRunThread],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstRunResolution {
    pub decision: FirstRunDecision,
    pub persist_completion: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostedFirstRunDecisionInput {
    pub local_environment_disabled: bool,
    pub hydrated: bool,
    pub completed: bool,
    pub catalog_ready: bool,
    pub environment_count: usize,
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
    let next = match event {
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
        FirstRunGateEvent::Evidence(decision) => decision,
    };

    if state.decision == FirstRunDecision::Wizard
        || next == FirstRunDecision::Pending
        || (state.decision == FirstRunDecision::App && next != FirstRunDecision::Wizard)
    {
        return state;
    }

    FirstRunGateState {
        decision: next,
        stalled: false,
    }
}

/// Only a project and thread created by this startup count as a fresh nonempty workspace.
pub fn is_fresh_first_run_workspace(input: &FirstRunWorkspaceInput<'_>) -> bool {
    if input.projects.len() > 1 || input.threads.len() > 1 {
        return false;
    }

    let bootstrap_project = input.projects.first();
    if let Some(project) = bootstrap_project {
        let server_cwd_matches = input.server_cwd.is_some_and(|cwd| {
            normalize_project_path_for_comparison(&project.workspace_root)
                == normalize_project_path_for_comparison(cwd)
        });
        if input.bootstrap_project_created != Some(true)
            || input.bootstrap_project_id != Some(project.id.as_str())
            || input.server_cwd.is_none()
            || input.primary_environment_id != Some(project.environment_id.as_str())
            || !server_cwd_matches
        {
            return false;
        }
    }

    let Some(thread) = input.threads.first() else {
        return true;
    };

    bootstrap_project.is_some_and(|project| {
        input.bootstrap_thread_created == Some(true)
            && input.bootstrap_thread_id == Some(thread.id.as_str())
            && input.primary_environment_id == Some(thread.environment_id.as_str())
            && thread.project_id == project.id
            && !thread.latest_run_present
            && !thread.latest_user_message_present
            && !thread.runtime_present
    })
}

/// Cached projects may open the app, but only live workspace data may complete onboarding.
pub fn resolve_first_run_decision(input: FirstRunDecisionInput) -> FirstRunResolution {
    let app = FirstRunResolution {
        decision: FirstRunDecision::App,
        persist_completion: false,
    };
    let pending = FirstRunResolution {
        decision: FirstRunDecision::Pending,
        persist_completion: false,
    };

    if !input.enabled || (input.hydrated && input.completed) {
        return app;
    }

    if !input.hydrated {
        return pending;
    }

    if input.project_count > 1 || input.thread_count > 1 {
        return FirstRunResolution {
            decision: FirstRunDecision::App,
            persist_completion: input.bootstrapped
                && input.authoritative
                && input.workspace_authoritative
                && input.catalog_ready
                && input.server_config_available,
        };
    }

    if !input.bootstrapped
        || !input.authoritative
        || !input.workspace_provenance_authoritative
        || !input.catalog_ready
        || !input.server_config_available
    {
        return pending;
    }

    if input.workspace_fresh {
        FirstRunResolution {
            decision: FirstRunDecision::Wizard,
            persist_completion: false,
        }
    } else {
        FirstRunResolution {
            decision: FirstRunDecision::App,
            persist_completion: input.workspace_authoritative,
        }
    }
}

/// Hosted onboarding depends on saved environments because there is no primary server.
pub fn resolve_hosted_first_run_decision(input: HostedFirstRunDecisionInput) -> FirstRunResolution {
    if !input.hydrated {
        return FirstRunResolution {
            decision: FirstRunDecision::Pending,
            persist_completion: false,
        };
    }

    if input.completed {
        return FirstRunResolution {
            decision: FirstRunDecision::App,
            persist_completion: false,
        };
    }

    if !input.catalog_ready {
        return FirstRunResolution {
            decision: FirstRunDecision::Pending,
            persist_completion: false,
        };
    }

    // An existing desktop may have disabled its server before onboarding existed.
    // Keep Connections accessible so it can turn local execution back on.
    if input.environment_count == 0 && !input.local_environment_disabled {
        FirstRunResolution {
            decision: FirstRunDecision::Wizard,
            persist_completion: false,
        }
    } else {
        FirstRunResolution {
            decision: FirstRunDecision::App,
            persist_completion: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn resolution(decision: FirstRunDecision, persist_completion: bool) -> FirstRunResolution {
        FirstRunResolution {
            decision,
            persist_completion,
        }
    }

    fn with(change: impl FnOnce(&mut FirstRunDecisionInput)) -> FirstRunDecisionInput {
        let mut input = FRESH;
        change(&mut input);
        input
    }

    #[test]
    fn opens_the_wizard_for_an_authoritative_fresh_workspace() {
        assert_eq!(
            resolve_first_run_decision(FRESH),
            resolution(FirstRunDecision::Wizard, false)
        );
    }

    #[test]
    fn does_not_permanently_complete_onboarding_from_cached_project_counts() {
        let input = with(|i| {
            i.authoritative = false;
            i.project_count = 3;
            i.workspace_fresh = false;
        });
        assert_eq!(
            resolve_first_run_decision(input),
            resolution(FirstRunDecision::App, false)
        );
    }

    #[test]
    fn backfills_completion_once_existing_projects_are_confirmed_by_the_server() {
        let input = with(|i| {
            i.project_count = 3;
            i.workspace_fresh = false;
        });
        assert_eq!(
            resolve_first_run_decision(input),
            resolution(FirstRunDecision::App, true)
        );
    }

    #[test]
    fn does_not_complete_onboarding_while_another_environment_is_still_bootstrapping() {
        let input = with(|i| {
            i.bootstrapped = false;
            i.project_count = 3;
            i.workspace_fresh = false;
        });
        assert_eq!(
            resolve_first_run_decision(input),
            resolution(FirstRunDecision::App, false)
        );
    }

    #[test]
    fn waits_for_managed_environments_before_treating_a_workspace_as_new() {
        let input = with(|i| i.catalog_ready = false);
        assert_eq!(
            resolve_first_run_decision(input),
            resolution(FirstRunDecision::Pending, false)
        );
    }

    #[test]
    fn does_not_complete_onboarding_before_the_environment_catalog_is_ready() {
        let input = with(|i| {
            i.catalog_ready = false;
            i.project_count = 3;
            i.workspace_fresh = false;
        });
        assert_eq!(
            resolve_first_run_decision(input),
            resolution(FirstRunDecision::App, false)
        );
    }

    #[test]
    fn does_not_complete_onboarding_before_the_server_configuration_is_available() {
        let input = with(|i| {
            i.project_count = 3;
            i.server_config_available = false;
            i.workspace_fresh = false;
        });
        assert_eq!(
            resolve_first_run_decision(input),
            resolution(FirstRunDecision::App, false)
        );
    }

    #[test]
    fn does_not_complete_onboarding_from_cached_remote_projects() {
        let input = with(|i| {
            i.project_count = 3;
            i.workspace_authoritative = false;
            i.workspace_fresh = false;
        });
        assert_eq!(
            resolve_first_run_decision(input),
            resolution(FirstRunDecision::App, false)
        );
    }

    #[test]
    fn does_not_complete_onboarding_from_a_single_cached_remote_project() {
        let input = with(|i| {
            i.workspace_authoritative = false;
            i.workspace_fresh = false;
        });
        assert_eq!(
            resolve_first_run_decision(input),
            resolution(FirstRunDecision::App, false)
        );
    }

    #[test]
    fn waits_for_live_data_before_judging_a_single_cached_project() {
        let input = with(|i| {
            i.authoritative = false;
            i.workspace_fresh = false;
        });
        assert_eq!(
            resolve_first_run_decision(input),
            resolution(FirstRunDecision::Pending, false)
        );
    }

    #[test]
    fn waits_for_the_completed_bootstrap_welcome_before_judging_a_nonempty_workspace() {
        let input = with(|i| i.workspace_provenance_authoritative = false);
        assert_eq!(
            resolve_first_run_decision(input),
            resolution(FirstRunDecision::Pending, false)
        );
    }

    #[test]
    fn waits_when_the_initial_welcome_is_pending_and_opens_the_wizard_after_completion() {
        let pending =
            is_first_run_workspace_provenance_authoritative(true, Some(BootstrapStatus::Pending));
        assert_eq!(
            resolve_first_run_decision(with(|i| i.workspace_provenance_authoritative = pending)),
            resolution(FirstRunDecision::Pending, false)
        );

        let completed =
            is_first_run_workspace_provenance_authoritative(true, Some(BootstrapStatus::Complete));
        assert_eq!(
            resolve_first_run_decision(with(|i| i.workspace_provenance_authoritative = completed)),
            resolution(FirstRunDecision::Wizard, false)
        );
    }

    #[test]
    fn does_not_wait_for_server_data_after_onboarding_is_already_complete() {
        let input = with(|i| {
            i.authoritative = false;
            i.bootstrapped = false;
            i.completed = true;
            i.server_config_available = false;
        });
        assert_eq!(
            resolve_first_run_decision(input),
            resolution(FirstRunDecision::App, false)
        );
    }

    #[test]
    fn provenance_waits_for_cwd_bootstrap_when_the_initial_catalog_is_empty() {
        assert!(!is_first_run_workspace_provenance_authoritative(
            true,
            Some(BootstrapStatus::Pending),
        ));
    }

    #[test]
    fn provenance_accepts_an_empty_catalog_after_cwd_bootstrap_completes() {
        assert!(is_first_run_workspace_provenance_authoritative(
            true,
            Some(BootstrapStatus::Complete),
        ));
    }

    #[test]
    fn provenance_waits_for_a_welcome_before_treating_an_empty_catalog_as_final() {
        assert!(!is_first_run_workspace_provenance_authoritative(
            false, None
        ));
    }

    #[test]
    fn provenance_accepts_a_legacy_welcome_without_bootstrap_status() {
        assert!(is_first_run_workspace_provenance_authoritative(true, None));
    }

    #[test]
    fn stalls_recovery_without_mounting_the_app_when_evidence_stalls() {
        let pending = FirstRunGateState {
            decision: FirstRunDecision::Pending,
            stalled: false,
        };
        assert_eq!(
            transition_first_run_gate_state(pending, FirstRunGateEvent::Timeout),
            FirstRunGateState {
                decision: FirstRunDecision::Pending,
                stalled: true,
            }
        );
    }

    #[test]
    fn resolves_stalled_recovery_only_after_authoritative_evidence() {
        for decision in [FirstRunDecision::App, FirstRunDecision::Wizard] {
            let stalled = FirstRunGateState {
                decision: FirstRunDecision::Pending,
                stalled: true,
            };
            assert_eq!(
                transition_first_run_gate_state(stalled, FirstRunGateEvent::Evidence(decision)),
                FirstRunGateState {
                    decision,
                    stalled: false,
                }
            );
        }
    }

    #[test]
    fn keeps_recovery_visible_while_evidence_remains_pending() {
        let state = FirstRunGateState {
            decision: FirstRunDecision::Pending,
            stalled: true,
        };
        assert_eq!(
            transition_first_run_gate_state(
                state,
                FirstRunGateEvent::Evidence(FirstRunDecision::Pending)
            ),
            state
        );
    }

    #[test]
    fn allows_authoritative_wizard_evidence_to_replace_an_app_decision() {
        assert_eq!(
            transition_first_run_gate_state(
                FirstRunGateState {
                    decision: FirstRunDecision::App,
                    stalled: false,
                },
                FirstRunGateEvent::Evidence(FirstRunDecision::Wizard),
            ),
            FirstRunGateState {
                decision: FirstRunDecision::Wizard,
                stalled: false,
            }
        );
    }

    fn hosted(
        hydrated: bool,
        completed: bool,
        catalog_ready: bool,
        environment_count: usize,
    ) -> HostedFirstRunDecisionInput {
        HostedFirstRunDecisionInput {
            local_environment_disabled: false,
            hydrated,
            completed,
            catalog_ready,
            environment_count,
        }
    }

    #[test]
    fn hosted_keeps_the_shell_hidden_until_client_settings_are_hydrated() {
        assert_eq!(
            resolve_hosted_first_run_decision(hosted(false, false, true, 0)),
            resolution(FirstRunDecision::Pending, false)
        );
    }

    #[test]
    fn hosted_waits_for_the_saved_environment_catalog_before_judging_an_install() {
        assert_eq!(
            resolve_hosted_first_run_decision(hosted(true, false, false, 0)),
            resolution(FirstRunDecision::Pending, false)
        );
    }

    #[test]
    fn hosted_opens_onboarding_when_an_install_has_no_saved_environments() {
        assert_eq!(
            resolve_hosted_first_run_decision(hosted(true, false, true, 0)),
            resolution(FirstRunDecision::Wizard, false)
        );
    }

    #[test]
    fn hosted_keeps_connections_reachable_for_a_remote_only_desktop_predating_onboarding() {
        let input = HostedFirstRunDecisionInput {
            local_environment_disabled: true,
            ..hosted(true, false, true, 0)
        };
        assert_eq!(
            resolve_hosted_first_run_decision(input),
            resolution(FirstRunDecision::App, true)
        );
    }

    #[test]
    fn hosted_backfills_onboarding_for_an_install_with_saved_environments() {
        assert_eq!(
            resolve_hosted_first_run_decision(hosted(true, false, true, 1)),
            resolution(FirstRunDecision::App, true)
        );
    }

    #[test]
    fn hosted_opens_the_app_immediately_after_onboarding_is_complete() {
        assert_eq!(
            resolve_hosted_first_run_decision(hosted(true, true, false, 0)),
            resolution(FirstRunDecision::App, false)
        );
    }

    const PRIMARY: &str = "primary-environment";

    fn bootstrap_project() -> FirstRunProject {
        FirstRunProject {
            id: "bootstrap-project".to_owned(),
            environment_id: PRIMARY.to_owned(),
            workspace_root: "/projects/current".to_owned(),
        }
    }

    fn bootstrap_thread() -> FirstRunThread {
        FirstRunThread {
            id: "bootstrap-thread".to_owned(),
            project_id: "bootstrap-project".to_owned(),
            environment_id: PRIMARY.to_owned(),
            latest_run_present: false,
            latest_user_message_present: false,
            runtime_present: false,
        }
    }

    fn workspace<'a>(
        server_cwd: Option<&'a str>,
        projects: &'a [FirstRunProject],
        threads: &'a [FirstRunThread],
    ) -> FirstRunWorkspaceInput<'a> {
        FirstRunWorkspaceInput {
            primary_environment_id: Some(PRIMARY),
            server_cwd,
            bootstrap_project_id: None,
            bootstrap_thread_id: None,
            bootstrap_project_created: None,
            bootstrap_thread_created: None,
            projects,
            threads,
        }
    }

    #[test]
    fn accepts_an_empty_workspace() {
        assert!(is_fresh_first_run_workspace(&workspace(
            Some("/projects/current"),
            &[],
            &[],
        )));
    }

    #[test]
    fn accepts_only_the_unused_project_and_thread_created_from_the_server_cwd() {
        let projects = [bootstrap_project()];
        let threads = [bootstrap_thread()];
        let input = FirstRunWorkspaceInput {
            server_cwd: Some("/projects/current/"),
            bootstrap_project_id: Some("bootstrap-project"),
            bootstrap_thread_id: Some("bootstrap-thread"),
            bootstrap_project_created: Some(true),
            bootstrap_thread_created: Some(true),
            ..workspace(None, &projects, &threads)
        };
        assert!(is_fresh_first_run_workspace(&input));
    }

    #[test]
    fn rejects_an_existing_unused_cwd_project_and_thread_reused_by_startup() {
        let projects = [bootstrap_project()];
        let threads = [bootstrap_thread()];
        let input = FirstRunWorkspaceInput {
            server_cwd: Some("/projects/current"),
            bootstrap_project_id: Some("bootstrap-project"),
            bootstrap_thread_id: Some("bootstrap-thread"),
            bootstrap_project_created: Some(false),
            bootstrap_thread_created: Some(false),
            ..workspace(None, &projects, &threads)
        };
        assert!(!is_fresh_first_run_workspace(&input));
    }

    #[test]
    fn rejects_a_nonempty_workspace_when_an_older_server_omits_creation_provenance() {
        let projects = [bootstrap_project()];
        let threads = [bootstrap_thread()];
        let input = FirstRunWorkspaceInput {
            server_cwd: Some("/projects/current"),
            bootstrap_project_id: Some("bootstrap-project"),
            bootstrap_thread_id: Some("bootstrap-thread"),
            ..workspace(None, &projects, &threads)
        };
        assert!(!is_fresh_first_run_workspace(&input));
    }

    #[test]
    fn normalizes_windows_project_paths_before_checking_the_bootstrap_workspace() {
        let projects = [FirstRunProject {
            workspace_root: "c:/projects/current".to_owned(),
            ..bootstrap_project()
        }];
        let threads = [bootstrap_thread()];
        let input = FirstRunWorkspaceInput {
            server_cwd: Some("C:\\Projects\\Current\\"),
            bootstrap_project_id: Some("bootstrap-project"),
            bootstrap_thread_id: Some("bootstrap-thread"),
            bootstrap_project_created: Some(true),
            bootstrap_thread_created: Some(true),
            ..workspace(None, &projects, &threads)
        };
        assert!(is_fresh_first_run_workspace(&input));
    }

    #[test]
    fn rejects_projects_from_another_environment_even_when_their_paths_match() {
        let projects = [FirstRunProject {
            environment_id: "remote-environment".to_owned(),
            ..bootstrap_project()
        }];
        assert!(!is_fresh_first_run_workspace(&workspace(
            Some("/projects/current"),
            &projects,
            &[],
        )));
    }

    #[test]
    fn rejects_threads_from_another_environment() {
        let projects = [bootstrap_project()];
        let threads = [FirstRunThread {
            environment_id: "remote-environment".to_owned(),
            ..bootstrap_thread()
        }];
        assert!(!is_fresh_first_run_workspace(&workspace(
            Some("/projects/current"),
            &projects,
            &threads,
        )));
    }

    #[test]
    fn rejects_a_thread_that_does_not_belong_to_the_bootstrap_project() {
        let projects = [bootstrap_project()];
        let threads = [FirstRunThread {
            project_id: "another-project".to_owned(),
            ..bootstrap_thread()
        }];
        assert!(!is_fresh_first_run_workspace(&workspace(
            Some("/projects/current"),
            &projects,
            &threads,
        )));
    }

    #[test]
    fn rejects_a_thread_when_there_is_no_bootstrap_project() {
        let threads = [bootstrap_thread()];
        assert!(!is_fresh_first_run_workspace(&workspace(
            Some("/projects/current"),
            &[],
            &threads,
        )));
    }

    #[test]
    fn rejects_a_bootstrap_thread_that_already_has_a_user_message() {
        let projects = [bootstrap_project()];
        let threads = [FirstRunThread {
            latest_user_message_present: true,
            ..bootstrap_thread()
        }];
        assert!(!is_fresh_first_run_workspace(&workspace(
            Some("/projects/current"),
            &projects,
            &threads,
        )));
    }

    #[test]
    fn rejects_a_bootstrap_thread_that_has_started_a_turn() {
        let projects = [bootstrap_project()];
        let threads = [FirstRunThread {
            latest_run_present: true,
            ..bootstrap_thread()
        }];
        assert!(!is_fresh_first_run_workspace(&workspace(
            Some("/projects/current"),
            &projects,
            &threads,
        )));
    }

    #[test]
    fn rejects_a_bootstrap_thread_that_has_a_provider_session() {
        let projects = [bootstrap_project()];
        let threads = [FirstRunThread {
            runtime_present: true,
            ..bootstrap_thread()
        }];
        assert!(!is_fresh_first_run_workspace(&workspace(
            Some("/projects/current"),
            &projects,
            &threads,
        )));
    }
}
