//! Tests for `provider_update_launch_notification_logic.rs`, ported from
//! `ProviderUpdateLaunchNotification.logic.test.ts`. Providers are built from
//! the wire JSON shape, so each fixture decodes like a server snapshot.

use serde_json::{Value, json};

use super::*;

const CHECKED_AT: &str = "2026-04-23T10:00:00.000Z";
const SESSION_STARTED_AT: &str = "2026-04-23T09:59:00.000Z";
const LATER_CHECKED_AT: &str = "2026-04-23T10:01:00.000Z";

/// A `ServerProvider` fixture. Defaults match the upstream `provider()` helper:
/// an enabled codex 1.0.0 behind 1.1.0, one-click updatable.
#[derive(Clone)]
struct Spec {
    driver: &'static str,
    instance: Option<&'static str>,
    enabled: bool,
    version: Option<&'static str>,
    latest: Option<&'static str>,
    advisory: &'static str,
    can_update: bool,
    command: Option<&'static str>,
    update: Option<(&'static str, Option<&'static str>, Option<&'static str>)>,
    latest_status: Option<&'static str>,
    checked_at: &'static str,
}

fn spec(driver: &'static str) -> Spec {
    Spec {
        driver,
        instance: None,
        enabled: true,
        version: Some("1.0.0"),
        latest: Some("1.1.0"),
        advisory: "behind_latest",
        can_update: true,
        command: Some("npm install -g provider"),
        update: None,
        latest_status: None,
        checked_at: CHECKED_AT,
    }
}

impl Spec {
    fn instance(mut self, instance: &'static str) -> Self {
        self.instance = Some(instance);
        self
    }
    fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    fn version(mut self, version: &'static str) -> Self {
        self.version = Some(version);
        self
    }
    fn latest(mut self, latest: Option<&'static str>) -> Self {
        self.latest = latest;
        self
    }
    fn advisory(mut self, advisory: &'static str) -> Self {
        self.advisory = advisory;
        self
    }
    fn can_update(mut self, can_update: bool) -> Self {
        self.can_update = can_update;
        self
    }
    fn command(mut self, command: Option<&'static str>) -> Self {
        self.command = command;
        self
    }
    fn updated(
        mut self,
        status: &'static str,
        message: &'static str,
        finished_at: Option<&'static str>,
    ) -> Self {
        self.update = Some((status, Some(message), finished_at));
        self
    }
    fn latest_status(mut self, status: &'static str) -> Self {
        self.latest_status = Some(status);
        self
    }
    fn checked_at(mut self, checked_at: &'static str) -> Self {
        self.checked_at = checked_at;
        self
    }

    fn build(self) -> ServerProvider {
        let mut value = json!({
            "instanceId": self.instance.unwrap_or(self.driver),
            "driver": self.driver,
            "enabled": self.enabled,
            "installed": true,
            "version": self.version,
            "status": "ready",
            "auth": { "status": "authenticated" },
            "checkedAt": self.checked_at,
            "models": [],
            "slashCommands": [],
            "skills": [],
            "versionAdvisory": {
                "status": self.advisory,
                "currentVersion": self.version,
                "latestVersion": self.latest,
                "updateCommand": self.command,
                "canUpdate": self.can_update,
                "checkedAt": CHECKED_AT,
                "message": "Update available.",
            },
        });
        if let Some(status) = self.latest_status {
            value["compatibilityAdvisory"] = json!({
                "status": "supported",
                "latestVersionStatus": status,
                "message": null,
                "recommendedVersion": null,
                "recommendedRange": null,
            });
        }
        if let Some((status, message, finished_at)) = self.update {
            value["updateState"] = json!({
                "status": status,
                "startedAt": if finished_at.is_some() { json!(CHECKED_AT) } else { Value::Null },
                "finishedAt": finished_at,
                "message": message,
                "output": null,
            });
        }
        serde_json::from_value(value).expect("fixture decodes as a server provider")
    }
}

fn provider(driver: &'static str) -> ServerProvider {
    spec(driver).build()
}

fn candidate(driver: &'static str) -> ServerProvider {
    spec(driver).build()
}

fn ids<const N: usize>(values: [&str; N]) -> Vec<ProviderInstanceId> {
    values
        .iter()
        .map(|value| ProviderInstanceId::new(value).expect("instance id"))
        .collect()
}

fn refs(providers: &[ServerProvider]) -> Vec<&ServerProvider> {
    providers.iter().collect()
}

fn pill_options(visible_after: Option<&str>) -> ProviderUpdateSidebarPillOptions<'_> {
    ProviderUpdateSidebarPillOptions {
        visible_after_iso: visible_after,
        dismissed_keys: &[],
    }
}

fn terminal(status: &'static str, message: &'static str) -> Spec {
    spec("codex").updated(status, message, Some(CHECKED_AT))
}

#[test]
fn detects_enabled_providers_with_a_latest_version_advisory() {
    assert!(is_provider_update_candidate(&provider("codex")));
    assert!(!is_provider_update_candidate(
        &spec("codex").enabled(false).build()
    ));
    assert!(!is_provider_update_candidate(
        &spec("codex").advisory("current").latest(None).build()
    ));
    assert!(!is_provider_update_candidate(
        &spec("codex").latest(None).build()
    ));
}

#[test]
fn deduplicates_multi_instance_provider_candidates_by_driver() {
    let providers = [
        spec("codex")
            .instance("codex_personal")
            .latest(Some("1.1.0"))
            .build(),
        spec("codex")
            .instance("codex")
            .latest(Some("1.1.0"))
            .build(),
        spec("cursor").latest(Some("0.3.0")).build(),
    ];
    assert_eq!(collect_provider_update_candidates(&providers).len(), 2);
}

#[test]
fn disables_one_click_updates_when_instances_disagree_on_the_update_command() {
    let target = candidate("claudeAgent");
    let providers = [
        target.clone(),
        spec("claudeAgent")
            .instance("claude_work")
            .command(Some("bun add -g @anthropic-ai/claude-code@latest"))
            .build(),
    ];
    assert!(!can_one_click_update_provider_candidate(
        &target, &providers
    ));
}

#[test]
fn keeps_one_click_updates_enabled_when_sibling_instances_are_already_current() {
    let target = spec("claudeAgent")
        .instance("claude_personal")
        .command(Some("npm install -g @anthropic-ai/claude-code@latest"))
        .build();
    let sibling = spec("claudeAgent")
        .instance("claude_work")
        .version("2.1.123")
        .advisory("current")
        .can_update(false)
        .command(None)
        .build();
    let providers = [target.clone(), sibling];

    assert!(has_one_click_update_provider_candidate(&target, &providers));
    assert!(can_one_click_update_provider_candidate(&target, &providers));
}

#[test]
fn keeps_the_inline_update_action_available_while_a_provider_update_is_running() {
    let running = spec("codex")
        .updated("running", "Updating provider.", None)
        .build();
    assert!(has_one_click_update_provider_candidate(
        &running,
        std::slice::from_ref(&running)
    ));
    assert!(!can_one_click_update_provider_candidate(
        &running,
        std::slice::from_ref(&running)
    ));
}

#[test]
fn builds_a_notification_key_from_provider_latest_versions() {
    let codex = spec("codex").version("1.0.0").latest(Some("1.1.0")).build();
    let cursor = spec("cursor")
        .version("0.2.0")
        .latest(Some("0.3.0"))
        .build();

    assert_eq!(
        provider_update_notification_key(&[&codex, &cursor]).as_deref(),
        Some("codex:1.1.0|cursor:0.3.0")
    );
    assert_eq!(provider_update_notification_key(&[]), None);
}

#[test]
fn keeps_the_same_notification_key_while_the_published_update_version_is_unchanged() {
    let first = spec("codex").version("1.0.0").latest(Some("1.2.0")).build();
    let second = spec("codex").version("1.1.0").latest(Some("1.2.0")).build();
    let next_published = spec("codex").version("1.1.0").latest(Some("1.3.0")).build();

    assert_eq!(
        provider_update_notification_key(&[&first]),
        provider_update_notification_key(&[&second])
    );
    assert_ne!(
        provider_update_notification_key(&[&next_published]),
        provider_update_notification_key(&[&first])
    );
}

#[test]
fn tracks_updated_provider_snapshots_by_instance_instead_of_collapsing_to_a_sibling() {
    let target_id = ids(["codex_personal"]);
    let updated_personal = spec("codex")
        .instance("codex_personal")
        .version("1.1.0")
        .latest(Some("1.1.0"))
        .advisory("current")
        .updated("succeeded", "Provider updated.", Some(CHECKED_AT))
        .build();
    let current_default_sibling = spec("codex")
        .instance("codex")
        .version("1.1.0")
        .latest(Some("1.1.0"))
        .advisory("current")
        .build();

    assert_eq!(
        collect_updated_provider_snapshots(
            &[Ok(vec![updated_personal.clone(), current_default_sibling])],
            &target_id,
        ),
        vec![updated_personal]
    );
}

#[test]
fn describes_a_single_one_click_update() {
    let view = get_provider_update_initial_toast_view(
        &[&spec("codex").latest(Some("1.1.0")).build()],
        &[&spec("codex").latest(Some("1.1.0")).build()],
    );

    assert_eq!(view.phase, ProviderUpdateToastPhase::Initial);
    assert_eq!(view.toast_type, ProviderUpdateToastType::Warning);
    assert_eq!(view.title, "Update Available: Codex v1.1.0");
    assert_eq!(
        view.description,
        "Install the update now or review provider settings."
    );
}

#[test]
fn describes_settings_only_updates_without_one_click_support() {
    let codex = spec("codex").can_update(false).build();
    let cursor = spec("cursor").can_update(false).build();
    let view = get_provider_update_initial_toast_view(&[&codex, &cursor], &[]);

    assert_eq!(
        view.description,
        "Codex and Cursor can be updated from provider settings."
    );
}

#[test]
fn uses_server_update_state_for_running_progress() {
    let view = get_provider_update_progress_toast_view(
        &[spec("codex")
            .updated("running", "Updating provider.", None)
            .build()],
        1,
    );

    assert_eq!(view.phase, ProviderUpdateToastPhase::Running);
    assert_eq!(view.toast_type, ProviderUpdateToastType::Loading);
    assert_eq!(view.title, "Updating provider");
    assert!(!should_show_primary_provider_update_toast(&view));
}

#[test]
fn keeps_the_initial_prompt_and_terminal_outcomes_visible_as_toasts() {
    let codex = provider("codex");
    assert!(should_show_primary_provider_update_toast(
        &get_provider_update_initial_toast_view(&[&codex], &[&codex])
    ));
    assert!(should_show_primary_provider_update_toast(
        &get_provider_update_rejected_toast_view(1, "boom")
    ));
}

#[test]
fn uses_server_failure_state_for_failed_progress() {
    let view = get_provider_update_progress_toast_view(
        &[spec("codex")
            .updated("failed", "command failed", Some(CHECKED_AT))
            .build()],
        1,
    );

    assert_eq!(view.phase, ProviderUpdateToastPhase::Failed);
    assert_eq!(view.toast_type, ProviderUpdateToastType::Error);
    assert_eq!(view.title, "Provider update failed");
    assert_eq!(view.description, "command failed");
}

#[test]
fn keeps_unchanged_providers_actionable_from_settings() {
    let view = get_provider_update_progress_toast_view(
        &[spec("cursor")
            .updated("unchanged", "still old", Some(CHECKED_AT))
            .build()],
        1,
    );

    assert_eq!(view.phase, ProviderUpdateToastPhase::Unchanged);
    assert_eq!(view.toast_type, ProviderUpdateToastType::Warning);
    assert_eq!(view.title, "Provider still needs an update");
    assert_eq!(
        view.description,
        "Cursor still appears outdated. Check provider settings for details."
    );
}

#[test]
fn marks_progress_succeeded_once_every_attempted_provider_is_no_longer_outdated() {
    let view = get_provider_update_progress_toast_view(
        &[spec("codex")
            .version("1.1.0")
            .latest(Some("1.1.0"))
            .advisory("current")
            .updated("succeeded", "Provider updated.", Some(CHECKED_AT))
            .build()],
        1,
    );

    assert_eq!(view.phase, ProviderUpdateToastPhase::Succeeded);
    assert_eq!(view.toast_type, ProviderUpdateToastType::Success);
    assert_eq!(view.title, "Provider updated");
    assert_eq!(
        view.description,
        "New sessions will use the updated provider."
    );
    assert_eq!(view.dismiss_after_visible_ms, Some(3_000));
}

#[test]
fn falls_back_to_a_rejected_rpc_message_for_transport_level_failures() {
    let results: Vec<CommandResult<Vec<ServerProvider>, String>> =
        vec![Err(CommandFailure::Failed("WebSocket closed".to_owned()))];

    assert_eq!(
        first_failed_provider_update_message(&results).as_deref(),
        Some("WebSocket closed")
    );
    let view = get_provider_update_rejected_toast_view(2, "WebSocket closed");
    assert_eq!(view.phase, ProviderUpdateToastPhase::Failed);
    assert_eq!(view.title, "Provider updates failed");
    assert_eq!(view.description, "WebSocket closed");
}

#[test]
fn collects_only_attempted_provider_snapshots_from_update_responses() {
    let codex = provider("codex");
    let cursor = provider("cursor");
    let results = [Ok(vec![codex.clone(), cursor.clone()])];

    assert_eq!(
        collect_updated_provider_snapshots(&results, std::slice::from_ref(&cursor.instance_id)),
        vec![cursor]
    );
}

#[test]
fn summarizes_active_provider_updates_for_the_sidebar_pill() {
    let view = get_provider_update_sidebar_pill_view(
        &[
            spec("codex")
                .updated("running", "Updating provider.", None)
                .build(),
            spec("cursor")
                .updated(
                    "queued",
                    "Waiting for another provider update to finish.",
                    None,
                )
                .build(),
        ],
        &pill_options(None),
    )
    .expect("active pill");

    assert_eq!(view.tone, ProviderUpdateSidebarPillTone::Loading);
    assert_eq!(view.title, "Updating 2 providers");
    assert_eq!(
        view.description,
        "Codex and Cursor updates are in progress."
    );
}

#[test]
fn uses_the_provider_name_for_a_single_active_sidebar_pill_update() {
    let view = get_provider_update_sidebar_pill_view(
        &[spec("codex")
            .updated("running", "Updating provider.", None)
            .build()],
        &pill_options(None),
    )
    .expect("active pill");

    assert_eq!(view.key, "loading:codex:running");
    assert_eq!(view.tone, ProviderUpdateSidebarPillTone::Loading);
    assert_eq!(view.title, "Updating Codex");
    assert_eq!(view.description, "Codex update in progress.");
}

#[test]
fn uses_the_provider_name_for_a_single_failed_sidebar_pill_update() {
    let view = get_provider_update_sidebar_pill_view(
        &[spec("claudeAgent")
            .updated(
                "failed",
                "Update command exited with code 1.",
                Some(CHECKED_AT),
            )
            .build()],
        &pill_options(Some(SESSION_STARTED_AT)),
    )
    .expect("failed pill");

    assert_eq!(
        view.key,
        "failed:claudeAgent:2026-04-23T10:00:00.000Z:Update command exited with code 1."
    );
    assert_eq!(view.tone, ProviderUpdateSidebarPillTone::Error);
    assert_eq!(view.title, "Claude v1.1.0 update failed");
    assert_eq!(view.description, "Update command exited with code 1.");
    assert!(view.dismissible);
}

#[test]
fn shows_a_short_lived_success_pill_after_a_single_provider_update_succeeds() {
    let view = get_provider_update_sidebar_pill_view(
        &[spec("codex")
            .version("1.1.0")
            .latest(Some("1.1.0"))
            .advisory("current")
            .updated("succeeded", "Provider updated.", Some(CHECKED_AT))
            .build()],
        &pill_options(Some(SESSION_STARTED_AT)),
    )
    .expect("success pill");

    assert_eq!(
        view.key,
        "succeeded:codex:2026-04-23T10:00:00.000Z:Provider updated."
    );
    assert_eq!(view.tone, ProviderUpdateSidebarPillTone::Success);
    assert_eq!(view.title, "Codex updated: v1.1.0");
    assert_eq!(
        view.description,
        "New sessions will use the updated provider."
    );
    assert_eq!(view.dismiss_after_visible_ms, Some(3_000));
}

#[test]
fn keeps_unchanged_sidebar_pill_states_dismissible() {
    let view = get_provider_update_sidebar_pill_view(
        &[spec("cursor")
            .updated("unchanged", "still old", Some(CHECKED_AT))
            .build()],
        &pill_options(Some(SESSION_STARTED_AT)),
    )
    .expect("unchanged pill");

    assert_eq!(
        view.key,
        "unchanged:cursor:2026-04-23T10:00:00.000Z:still old"
    );
    assert_eq!(view.tone, ProviderUpdateSidebarPillTone::Warning);
    assert_eq!(view.title, "Cursor still needs an update");
    assert!(view.dismissible);
}

#[test]
fn does_not_show_sidebar_terminal_states_from_before_the_current_app_session() {
    let providers = [spec("codex")
        .updated("failed", "command failed", Some(CHECKED_AT))
        .build()];
    assert_eq!(
        get_provider_update_sidebar_pill_view(
            &providers,
            &pill_options(Some("2026-04-23T10:00:01.000Z"))
        ),
        None
    );
}

#[test]
fn shows_a_newer_success_before_falling_back_to_an_older_failure() {
    let providers = [
        spec("claudeAgent")
            .updated(
                "failed",
                "Update command exited with code 1.",
                Some(CHECKED_AT),
            )
            .build(),
        spec("codex")
            .version("1.2.0")
            .latest(Some("1.2.0"))
            .advisory("current")
            .updated("succeeded", "Provider updated.", Some(LATER_CHECKED_AT))
            .build(),
    ];

    let success =
        get_provider_update_sidebar_pill_view(&providers, &pill_options(Some(SESSION_STARTED_AT)))
            .expect("success pill");
    assert_eq!(
        success.key,
        "succeeded:codex:2026-04-23T10:01:00.000Z:Provider updated."
    );
    assert_eq!(success.tone, ProviderUpdateSidebarPillTone::Success);
    assert_eq!(success.title, "Codex updated: v1.2.0");

    let dismissed = [success.key.clone()];
    let failure = get_provider_update_sidebar_pill_view(
        &providers,
        &ProviderUpdateSidebarPillOptions {
            visible_after_iso: Some(SESSION_STARTED_AT),
            dismissed_keys: &dismissed,
        },
    )
    .expect("failure pill");
    assert_eq!(
        failure.key,
        "failed:claudeAgent:2026-04-23T10:00:00.000Z:Update command exited with code 1."
    );
    assert_eq!(failure.tone, ProviderUpdateSidebarPillTone::Error);
    assert_eq!(failure.title, "Claude v1.1.0 update failed");
}

#[test]
fn does_not_show_a_sidebar_pill_for_passive_update_availability() {
    let providers = [
        spec("codex").can_update(true).build(),
        spec("cursor").can_update(false).build(),
    ];
    assert_eq!(
        get_provider_update_sidebar_pill_view(&providers, &pill_options(None)),
        None
    );
}

fn fulfilled(
    is_primary: bool,
    provider: Option<ServerProvider>,
) -> Result<LocalProviderUpdateOutcome, String> {
    Ok(LocalProviderUpdateOutcome {
        environment_id: EnvironmentId::new("env").expect("id"),
        is_primary,
        driver: ProviderDriverKind::new("codex").expect("driver"),
        instance_id: ProviderInstanceId::new("codex").expect("instance"),
        provider,
    })
}

#[test]
fn surfaces_a_secondary_backend_failed_update_over_the_primarys_success() {
    let snapshots = collect_provider_update_outcome_snapshots(&[
        fulfilled(
            true,
            Some(terminal("succeeded", "Provider updated.").build()),
        ),
        fulfilled(false, Some(terminal("failed", "npm: NotFound").build())),
    ]);

    assert_eq!(snapshots.len(), 1);
    assert_eq!(
        snapshots[0].update_state.as_ref().map(|s| s.status),
        Some(ServerProviderUpdateStatus::Failed)
    );
    assert_eq!(
        get_provider_update_progress_toast_view(&snapshots, 1).phase,
        ProviderUpdateToastPhase::Failed
    );
}

#[test]
fn surfaces_a_secondary_backend_that_stayed_outdated_over_the_primarys_success() {
    let snapshots = collect_provider_update_outcome_snapshots(&[
        fulfilled(
            true,
            Some(terminal("succeeded", "Provider updated.").build()),
        ),
        fulfilled(false, Some(terminal("unchanged", "still outdated").build())),
    ]);

    assert_eq!(
        snapshots[0].update_state.as_ref().map(|s| s.status),
        Some(ServerProviderUpdateStatus::Unchanged)
    );
    assert_eq!(
        get_provider_update_progress_toast_view(&snapshots, 1).phase,
        ProviderUpdateToastPhase::Unchanged
    );
}

#[test]
fn reports_success_only_when_every_backend_succeeded() {
    let snapshots = collect_provider_update_outcome_snapshots(&[
        fulfilled(
            true,
            Some(terminal("succeeded", "Provider updated.").build()),
        ),
        fulfilled(
            false,
            Some(terminal("succeeded", "Provider updated.").build()),
        ),
    ]);

    assert_eq!(
        get_provider_update_progress_toast_view(&snapshots, 1).phase,
        ProviderUpdateToastPhase::Succeeded
    );
}

#[test]
fn ignores_backends_that_did_not_return_the_targeted_instance() {
    let primary = terminal("succeeded", "Provider updated.").build();
    let snapshots = collect_provider_update_outcome_snapshots(&[
        fulfilled(true, Some(primary.clone())),
        fulfilled(false, None),
    ]);

    assert_eq!(snapshots, vec![primary]);
}

#[test]
fn treats_a_rejected_dispatch_as_not_contributing_a_snapshot() {
    let primary = terminal("succeeded", "Provider updated.").build();
    let results = [
        fulfilled(true, Some(primary.clone())),
        Err("WebSocket closed".to_owned()),
    ];

    assert_eq!(
        collect_provider_update_outcome_snapshots(&results),
        vec![primary]
    );
    assert_eq!(
        first_rejected_provider_update_message(&results).as_deref(),
        Some("WebSocket closed")
    );
}

fn environment(
    environment_id: &str,
    label: &str,
    is_primary: bool,
    connection_state: EnvironmentUpdateConnectionState,
    providers: Vec<ServerProvider>,
) -> LocalEnvironmentProvidersInput {
    LocalEnvironmentProvidersInput {
        environment_id: EnvironmentId::new(environment_id).expect("id"),
        label: label.to_owned(),
        is_primary,
        connection_state,
        providers,
    }
}

const READY: EnvironmentUpdateConnectionState = EnvironmentUpdateConnectionState::Ready;
const CONNECTING: EnvironmentUpdateConnectionState = EnvironmentUpdateConnectionState::Connecting;

#[test]
fn groups_each_environments_outdated_one_click_candidates() {
    let (groups, is_any_settling) = build_local_environment_update_groups(&[
        environment(
            "env-windows",
            "Windows",
            true,
            READY,
            vec![spec("codex").latest(Some("1.1.0")).build()],
        ),
        environment(
            "env-wsl",
            "WSL",
            false,
            READY,
            vec![spec("codex").latest(Some("1.1.0")).build()],
        ),
    ]);

    assert!(!is_any_settling);
    assert_eq!(
        groups
            .iter()
            .map(|group| group.label.as_str())
            .collect::<Vec<_>>(),
        vec!["Windows", "WSL"]
    );
    assert!(groups.iter().all(|group| group.candidates.len() == 1));
}

#[test]
fn flags_settling_while_a_secondary_backend_is_still_connecting() {
    let (groups, is_any_settling) = build_local_environment_update_groups(&[
        environment(
            "env-windows",
            "env-windows",
            true,
            READY,
            vec![provider("codex")],
        ),
        environment("env-wsl", "env-wsl", false, CONNECTING, vec![]),
    ]);

    assert!(is_any_settling);
    let wsl = groups
        .iter()
        .find(|group| group.environment_id.as_str() == "env-wsl")
        .expect("wsl group");
    assert!(wsl.is_settling);
}

#[test]
fn keeps_only_environments_that_have_a_one_click_update_on_offer() {
    let (groups, _) = build_local_environment_update_groups(&[
        environment(
            "env-windows",
            "env-windows",
            true,
            READY,
            vec![provider("codex")],
        ),
        environment(
            "env-wsl",
            "env-wsl",
            false,
            READY,
            vec![spec("codex").advisory("current").latest(None).build()],
        ),
    ]);

    let with_updates: Vec<&str> = environment_groups_with_updates(&groups)
        .iter()
        .map(|group| group.environment_id.as_str())
        .collect();
    assert_eq!(with_updates, vec!["env-windows"]);
}

#[test]
fn keys_the_notification_by_environment_driver_and_latest_version() {
    let (no_updates, _) = build_local_environment_update_groups(&[environment(
        "env-windows",
        "env-windows",
        true,
        READY,
        vec![spec("codex").advisory("current").latest(None).build()],
    )]);
    assert_eq!(local_environment_update_notification_key(&no_updates), None);

    let (both, _) = build_local_environment_update_groups(&[
        environment(
            "env-windows",
            "env-windows",
            true,
            READY,
            vec![spec("codex").latest(Some("1.1.0")).build()],
        ),
        environment(
            "env-wsl",
            "env-wsl",
            false,
            READY,
            vec![spec("codex").latest(Some("1.1.0")).build()],
        ),
    ]);
    let key = local_environment_update_notification_key(&both).expect("key");
    assert!(key.contains("env-windows=codex:1.1.0"), "{key}");
    assert!(key.contains("env-wsl=codex:1.1.0"), "{key}");
}

#[test]
fn labels_environments_by_platform_so_they_are_distinguishable() {
    assert_eq!(
        derive_environment_display_label(
            false,
            None,
            Some(ExecutionEnvironmentPlatformOs::Windows),
            "Jgratton24"
        ),
        "Windows"
    );
    assert_eq!(
        derive_environment_display_label(
            true,
            None,
            Some(ExecutionEnvironmentPlatformOs::Linux),
            "Jgratton24"
        ),
        "WSL"
    );
    assert_eq!(
        derive_environment_display_label(
            true,
            Some("ubuntu"),
            Some(ExecutionEnvironmentPlatformOs::Linux),
            "Jgratton24"
        ),
        "WSL · ubuntu"
    );
    assert_eq!(
        derive_environment_display_label(false, None, None, "My Device"),
        "My Device"
    );
}

#[test]
fn treats_succeeded_failed_and_unchanged_as_terminal() {
    assert!(is_terminal_provider_update_phase(
        ProviderUpdateToastPhase::Succeeded
    ));
    assert!(is_terminal_provider_update_phase(
        ProviderUpdateToastPhase::Failed
    ));
    assert!(is_terminal_provider_update_phase(
        ProviderUpdateToastPhase::Unchanged
    ));
}

#[test]
fn treats_running_and_initial_as_non_terminal_so_they_are_not_persisted() {
    // A running or initial snapshot never re-polls, so persisting it would pin
    // the row's spinner once pending clears.
    assert!(!is_terminal_provider_update_phase(
        ProviderUpdateToastPhase::Running
    ));
    assert!(!is_terminal_provider_update_phase(
        ProviderUpdateToastPhase::Initial
    ));
}

fn wsl_group() -> LocalEnvironmentUpdateGroup {
    LocalEnvironmentUpdateGroup {
        environment_id: EnvironmentId::new("env-wsl").expect("id"),
        label: "WSL".to_owned(),
        is_primary: false,
        is_settling: false,
        candidates: vec![candidate("codex")],
        providers: vec![],
    }
}

fn toast(phase: ProviderUpdateToastPhase, description: &str) -> ProviderUpdateToastView {
    ProviderUpdateToastView {
        phase,
        toast_type: ProviderUpdateToastType::Loading,
        title: "t".to_owned(),
        description: description.to_owned(),
        dismiss_after_visible_ms: None,
    }
}

fn pill(tone: ProviderUpdateSidebarPillTone, description: &str) -> ProviderUpdateSidebarPillView {
    ProviderUpdateSidebarPillView {
        key: "k".to_owned(),
        tone,
        title: "t".to_owned(),
        description: description.to_owned(),
        dismissible: false,
        dismiss_after_visible_ms: None,
    }
}

#[test]
fn row_status_prefers_a_transport_error() {
    let status = resolve_environment_update_row_status(
        &wsl_group(),
        Some("boom"),
        Some(&toast(ProviderUpdateToastPhase::Succeeded, "")),
        Some(&pill(ProviderUpdateSidebarPillTone::Success, "")),
        true,
    );
    assert_eq!(
        status,
        ProviderUpdateRowStatus {
            kind: ProviderUpdateRowStatusKind::Failed,
            text: "boom".to_owned()
        }
    );
}

#[test]
fn row_status_uses_a_terminal_result_snapshot() {
    let status = resolve_environment_update_row_status(
        &wsl_group(),
        None,
        Some(&toast(ProviderUpdateToastPhase::Succeeded, "")),
        None,
        false,
    );
    assert_eq!(status.kind, ProviderUpdateRowStatusKind::Success);
}

#[test]
fn row_status_falls_through_a_non_terminal_result_to_live_server_state() {
    // The dispatch snapshot is still running, but server state reports success.
    let status = resolve_environment_update_row_status(
        &wsl_group(),
        None,
        Some(&toast(
            ProviderUpdateToastPhase::Running,
            "Running provider update command.",
        )),
        Some(&pill(ProviderUpdateSidebarPillTone::Success, "")),
        true,
    );
    assert_eq!(status.kind, ProviderUpdateRowStatusKind::Success);
}

#[test]
fn row_status_shows_the_pending_spinner_before_any_signal_arrives() {
    let status = resolve_environment_update_row_status(
        &wsl_group(),
        None,
        Some(&toast(ProviderUpdateToastPhase::Running, "")),
        None,
        true,
    );
    assert_eq!(status.kind, ProviderUpdateRowStatusKind::Loading);
}

#[test]
fn row_status_keeps_a_non_terminal_result_on_loading_after_pending_clears() {
    // The dispatch returned an incomplete snapshot and pending cleared. The row
    // must not fall back to the idle Update button.
    let status = resolve_environment_update_row_status(
        &wsl_group(),
        None,
        Some(&toast(ProviderUpdateToastPhase::Running, "")),
        None,
        false,
    );
    assert_eq!(status.kind, ProviderUpdateRowStatusKind::Loading);
}

#[test]
fn row_status_lists_the_providers_when_idle() {
    let status = resolve_environment_update_row_status(&wsl_group(), None, None, None, false);
    assert_eq!(
        status,
        ProviderUpdateRowStatus {
            kind: ProviderUpdateRowStatusKind::Idle,
            text: "Codex".to_owned(),
        }
    );
}

#[test]
fn does_not_offer_incompatible_latest_versions_and_restores_suggestions_after_policy_relaxation() {
    for latest_status in ["broken", "unsupported", "supported", "unknown"] {
        let snapshot = spec("codex").latest_status(latest_status).build();
        let expected = matches!(latest_status, "supported" | "unknown");
        assert_eq!(
            is_provider_update_candidate(&snapshot),
            expected,
            "{latest_status}"
        );
        assert_eq!(
            is_provider_settings_update_candidate(&snapshot),
            expected,
            "{latest_status}"
        );
    }
}

fn run(
    machine: &str,
    driver: &str,
    result: CommandResult<Vec<ServerProvider>, String>,
) -> ProviderUpdateRun {
    ProviderUpdateRun {
        environment_id: EnvironmentId::new(machine).expect("id"),
        machine_label: machine.to_owned(),
        driver: ProviderDriverKind::new(driver).expect("driver"),
        instance_id: ProviderInstanceId::new(driver).expect("instance"),
        result,
    }
}

#[test]
fn run_toast_lists_every_failed_update_and_ignores_interrupted_ones() {
    let view = get_provider_update_run_toast_view(&[
        run(
            "Mac Studio",
            "codex",
            Ok(vec![terminal("succeeded", "Provider updated.").build()]),
        ),
        run(
            "Mac Studio",
            "claudeAgent",
            Ok(vec![
                spec("claudeAgent")
                    .updated("failed", "npm exited with code 1.", Some(LATER_CHECKED_AT))
                    .build(),
            ]),
        ),
        run(
            "Laptop",
            "codex",
            Err(CommandFailure::Failed("WebSocket closed".to_owned())),
        ),
        run("Server", "codex", Err(CommandFailure::Interrupted)),
    ]);

    assert_eq!(
        view,
        Some(ProviderUpdateRunToastView {
            toast_type: ProviderUpdateToastType::Error,
            title: "2 of 3 provider updates failed".to_owned(),
            description:
                "Mac Studio · Claude: npm exited with code 1.\nLaptop · Codex: WebSocket closed"
                    .to_owned(),
        })
    );
}

#[test]
fn run_toast_reports_success_when_every_update_succeeded() {
    let succeeded = || Ok(vec![terminal("succeeded", "Provider updated.").build()]);

    assert_eq!(
        get_provider_update_run_toast_view(&[
            run("Mac Studio", "codex", succeeded()),
            run("Laptop", "codex", succeeded()),
        ]),
        Some(ProviderUpdateRunToastView {
            toast_type: ProviderUpdateToastType::Success,
            title: "2 providers updated".to_owned(),
            description: "New sessions will use the updated providers.".to_owned(),
        })
    );
    assert_eq!(
        get_provider_update_run_toast_view(&[run(
            "Server",
            "codex",
            Err(CommandFailure::Interrupted),
        )]),
        None
    );
}

#[test]
fn provider_list_joins_names_with_and_or_an_oxford_comma() {
    let codex = provider("codex");
    let cursor = provider("cursor");
    let claude = provider("claudeAgent");
    assert_eq!(format_provider_list(&[&codex, &cursor]), "Codex and Cursor");
    assert_eq!(
        format_provider_list(&[&codex, &cursor, &claude]),
        "Codex, Cursor, and Claude"
    );
    assert_eq!(format_provider_list(&refs(&[])), "");
}
