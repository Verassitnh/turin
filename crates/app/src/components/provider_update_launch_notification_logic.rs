//! Port of upstream/apps/web/src/components/ProviderUpdateLaunchNotification.logic.ts
//! (+ test).
//!
//! Decides which providers are outdated and one-click updatable, the toast and
//! sidebar-pill copy for an update run, and the per-environment roll-up. The
//! dispatch itself (`serverEnvironment.updateProvider`) and the popover are the
//! caller's. Providers are `turin_provider_core::ServerProvider`, the same
//! snapshot the server publishes.
//!
//! Upstream's `Promise` and `AsyncResult` rejections become `Err(String)`
//! holding the message. Upstream's "not an `Error`" fallback therefore has no
//! case; the message always comes through.
// Seams for consumers the shell wires (T22); nothing calls these yet.
#![allow(dead_code)]

use indexmap::IndexMap;
use turin_contracts::base_schemas::{EnvironmentId, IsoDateTime};
use turin_contracts::environment::ExecutionEnvironmentPlatformOs;
use turin_contracts::model::provider_display_name;
use turin_contracts::provider_instance::{
    ProviderDriverKind, ProviderInstanceId, default_instance_id_for_driver,
};
use turin_provider_core::server_provider::{
    ServerProvider, ServerProviderCompatibilityStatus, ServerProviderUpdateStatus,
    ServerProviderVersionAdvisoryStatus,
};
use turin_shared::js_string::locale_compare;

use crate::state::command_result::{CommandFailure, CommandResult, is_command_interrupted};

const PROVIDER_UPDATE_SUCCESS_VISIBLE_MS: u64 = 3_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderUpdateToastType {
    Warning,
    Loading,
    Error,
    Success,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderUpdateToastPhase {
    Initial,
    Running,
    Failed,
    Unchanged,
    Succeeded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderUpdateToastView {
    pub phase: ProviderUpdateToastPhase,
    pub toast_type: ProviderUpdateToastType,
    pub title: String,
    pub description: String,
    pub dismiss_after_visible_ms: Option<u64>,
}

/// Terminal update phases are outcomes that are safe to persist as a one-shot
/// row result. A non-terminal snapshot never re-polls itself, so storing one
/// would pin an update row's spinner after its pending flag expires.
pub fn is_terminal_provider_update_phase(phase: ProviderUpdateToastPhase) -> bool {
    matches!(
        phase,
        ProviderUpdateToastPhase::Succeeded
            | ProviderUpdateToastPhase::Failed
            | ProviderUpdateToastPhase::Unchanged
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderUpdateSidebarPillTone {
    Loading,
    Warning,
    Error,
    Success,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderUpdateSidebarPillView {
    pub key: String,
    pub tone: ProviderUpdateSidebarPillTone,
    pub title: String,
    pub description: String,
    pub dismissible: bool,
    pub dismiss_after_visible_ms: Option<u64>,
}

/// `ProviderUpdateSidebarPillOptions`. `visible_after_iso` hides terminal states
/// from before the current app session; `dismissed_keys` hides keys the user closed.
#[derive(Clone, Debug, Default)]
pub struct ProviderUpdateSidebarPillOptions<'a> {
    pub visible_after_iso: Option<&'a str>,
    pub dismissed_keys: &'a [String],
}

fn format_version(value: &str) -> String {
    if value.starts_with('v') {
        value.to_owned()
    } else {
        format!("v{value}")
    }
}

fn driver_name(driver: &ProviderDriverKind) -> String {
    provider_display_name(driver.as_str()).map_or_else(|| driver.as_str().to_owned(), str::to_owned)
}

fn choose_representative_provider<'a>(
    current: Option<&'a ServerProvider>,
    candidate: &'a ServerProvider,
) -> &'a ServerProvider {
    let Some(current) = current else {
        return candidate;
    };
    let default_instance_id = default_instance_id_for_driver(&candidate.driver);
    if candidate.instance_id == default_instance_id {
        return candidate;
    }
    if current.instance_id == default_instance_id {
        return current;
    }
    if locale_compare(&candidate.checked_at, &current.checked_at).is_ge() {
        candidate
    } else {
        current
    }
}

/// One provider per driver, preferring the default instance. Output keeps
/// first-seen driver order, as upstream's `Map` does.
fn dedupe_providers_by_driver<'a>(
    providers: impl IntoIterator<Item = &'a ServerProvider>,
) -> Vec<&'a ServerProvider> {
    let mut latest: IndexMap<String, &'a ServerProvider> = IndexMap::new();
    for provider in providers {
        let key = provider.driver.as_str().to_owned();
        let current = latest.get(&key).copied();
        latest.insert(key, choose_representative_provider(current, provider));
    }
    latest.into_values().collect()
}

fn dedupe_providers_by_instance_id(providers: &[ServerProvider]) -> Vec<ServerProvider> {
    let mut latest: IndexMap<ProviderInstanceId, &ServerProvider> = IndexMap::new();
    for provider in providers {
        let replace = latest.get(&provider.instance_id).is_none_or(|current| {
            locale_compare(&provider.checked_at, &current.checked_at).is_ge()
        });
        if replace {
            latest.insert(provider.instance_id.clone(), provider);
        }
    }
    latest.into_values().cloned().collect()
}

fn provider_updated_title(provider: &ServerProvider) -> String {
    let provider_name = driver_name(&provider.driver);
    match provider
        .version
        .as_ref()
        .filter(|version| !version.as_str().is_empty())
    {
        Some(version) => format!(
            "{provider_name} updated: {}",
            format_version(version.as_str())
        ),
        None => format!("{provider_name} updated"),
    }
}

fn provider_updated_description(provider_count: usize) -> String {
    if provider_count == 1 {
        "New sessions will use the updated provider.".to_owned()
    } else {
        "New sessions will use the updated providers.".to_owned()
    }
}

fn provider_failed_update_title(provider: &ServerProvider) -> String {
    let provider_name = driver_name(&provider.driver);
    match latest_version_of(provider).filter(|version| !version.is_empty()) {
        Some(attempted) => format!(
            "{provider_name} {} update failed",
            format_version(attempted)
        ),
        None => format!("{provider_name} update failed"),
    }
}

/// `versionAdvisory?.latestVersion`.
fn latest_version_of(provider: &ServerProvider) -> Option<&str> {
    provider
        .version_advisory
        .as_ref()
        .and_then(|advisory| advisory.latest_version.as_ref())
        .map(|version| version.as_str())
}

/// Latest-version status is `broken` or `unsupported`, which hides updates.
fn latest_version_status_blocks_update(provider: &ServerProvider) -> bool {
    matches!(
        provider
            .compatibility_advisory
            .as_ref()
            .and_then(|advisory| advisory.latest_version_status),
        Some(
            ServerProviderCompatibilityStatus::Broken
                | ServerProviderCompatibilityStatus::Unsupported
        )
    )
}

/// Provider update candidates are enabled, behind the latest version, and have a
/// latest version to update to.
pub fn is_provider_update_candidate(provider: &ServerProvider) -> bool {
    provider.enabled
        && !latest_version_status_blocks_update(provider)
        && provider.version_advisory.as_ref().is_some_and(|advisory| {
            advisory.status == ServerProviderVersionAdvisoryStatus::BehindLatest
                && advisory.latest_version.is_some()
        })
}

pub fn is_provider_update_active(provider: &ServerProvider) -> bool {
    matches!(
        provider.update_state.as_ref().map(|state| state.status),
        Some(ServerProviderUpdateStatus::Queued | ServerProviderUpdateStatus::Running)
    )
}

pub fn collect_provider_update_candidates(providers: &[ServerProvider]) -> Vec<&ServerProvider> {
    dedupe_providers_by_driver(providers.iter().filter(|p| is_provider_update_candidate(p)))
}

pub fn is_provider_settings_update_candidate(provider: &ServerProvider) -> bool {
    provider.enabled
        && !latest_version_status_blocks_update(provider)
        && provider.version_advisory.as_ref().is_some_and(|advisory| {
            advisory.status == ServerProviderVersionAdvisoryStatus::BehindLatest
                && advisory.can_update
                && advisory.update_command.is_some()
        })
}

/// Whether the candidate's driver can be updated in one click. Instances of
/// that driver must agree on the update command.
pub fn has_one_click_update_provider_candidate(
    candidate: &ServerProvider,
    providers: &[ServerProvider],
) -> bool {
    let candidate_can_update = candidate
        .version_advisory
        .as_ref()
        .is_some_and(|advisory| advisory.can_update && advisory.update_command.is_some());
    if !candidate_can_update {
        return false;
    }

    let driver_providers: Vec<&ServerProvider> = providers
        .iter()
        .filter(|provider| provider.driver == candidate.driver)
        .collect();
    if driver_providers.is_empty() {
        return false;
    }

    let mut update_commands: Vec<&str> = Vec::new();
    for provider in driver_providers {
        if !is_provider_update_candidate(provider) {
            continue;
        }
        let Some(advisory) = provider.version_advisory.as_ref() else {
            return false;
        };
        let Some(command) = advisory
            .update_command
            .as_ref()
            .filter(|_| advisory.can_update)
        else {
            return false;
        };
        if !update_commands.contains(&command.as_str()) {
            update_commands.push(command.as_str());
        }
    }

    update_commands.len() == 1
}

pub fn can_one_click_update_provider_candidate(
    candidate: &ServerProvider,
    providers: &[ServerProvider],
) -> bool {
    !is_provider_update_active(candidate)
        && has_one_click_update_provider_candidate(candidate, providers)
}

/// Stable key over the (driver, latest version) pairs on offer, so the popover
/// is shown once per set.
pub fn provider_update_notification_key(providers: &[&ServerProvider]) -> Option<String> {
    let mut parts: Vec<String> = dedupe_providers_by_driver(providers.iter().copied())
        .into_iter()
        .map(|provider| {
            format!(
                "{}:{}",
                provider.driver.as_str(),
                latest_version_of(provider).unwrap_or("")
            )
        })
        .collect();
    parts.sort();
    (!parts.is_empty()).then(|| parts.join("|"))
}

/// `formatProviderList`: "A and B", or "A, B, and C".
fn format_provider_list(providers: &[&ServerProvider]) -> String {
    let names: Vec<String> = providers
        .iter()
        .map(|provider| driver_name(&provider.driver))
        .collect();
    if names.len() <= 2 {
        return names.join(" and ");
    }
    let (last, rest) = names.split_last().expect("more than two names");
    format!("{}, and {last}", rest.join(", "))
}

fn initial_toast_title(providers: &[&ServerProvider]) -> String {
    if let [provider] = providers {
        let provider_name = driver_name(&provider.driver);
        let latest = latest_version_of(provider).unwrap_or("");
        return format!(
            "Update Available: {provider_name} {}",
            format_version(latest)
        );
    }
    format!("Updates Available: {} providers", providers.len())
}

pub fn get_provider_update_initial_toast_view(
    update_providers: &[&ServerProvider],
    one_click_providers: &[&ServerProvider],
) -> ProviderUpdateToastView {
    ProviderUpdateToastView {
        phase: ProviderUpdateToastPhase::Initial,
        toast_type: ProviderUpdateToastType::Warning,
        title: initial_toast_title(update_providers),
        description: if one_click_providers.is_empty() {
            format!(
                "{} can be updated from provider settings.",
                format_provider_list(update_providers)
            )
        } else {
            "Install the update now or review provider settings.".to_owned()
        },
        dismiss_after_visible_ms: None,
    }
}

pub fn should_show_primary_provider_update_toast(view: &ProviderUpdateToastView) -> bool {
    view.phase != ProviderUpdateToastPhase::Running
}

fn running_toast_view(provider_count: usize) -> ProviderUpdateToastView {
    ProviderUpdateToastView {
        phase: ProviderUpdateToastPhase::Running,
        toast_type: ProviderUpdateToastType::Loading,
        title: if provider_count == 1 {
            "Updating provider".to_owned()
        } else {
            "Updating providers".to_owned()
        },
        description: "Running provider update command.".to_owned(),
        dismiss_after_visible_ms: None,
    }
}

pub fn get_provider_update_rejected_toast_view(
    provider_count: usize,
    message: &str,
) -> ProviderUpdateToastView {
    ProviderUpdateToastView {
        phase: ProviderUpdateToastPhase::Failed,
        toast_type: ProviderUpdateToastType::Error,
        title: if provider_count == 1 {
            "Provider update failed".to_owned()
        } else {
            "Provider updates failed".to_owned()
        },
        description: message.to_owned(),
        dismiss_after_visible_ms: None,
    }
}

fn failed_providers_title(count: usize) -> String {
    if count == 1 {
        "Provider update failed".to_owned()
    } else {
        "Provider updates failed".to_owned()
    }
}

fn failed_provider_update_description(providers: &[&ServerProvider]) -> String {
    if let [provider] = providers
        && let Some(message) = provider
            .update_state
            .as_ref()
            .and_then(|state| state.message.as_ref())
    {
        return message.as_str().to_owned();
    }
    format!(
        "{} failed to update. Check provider settings for details.",
        format_provider_list(providers)
    )
}

pub fn get_provider_update_progress_toast_view(
    providers: &[ServerProvider],
    provider_count: usize,
) -> ProviderUpdateToastView {
    let providers = dedupe_providers_by_driver(providers);
    let status_of = |provider: &ServerProvider| provider.update_state.as_ref().map(|s| s.status);

    let failed: Vec<&ServerProvider> = providers
        .iter()
        .copied()
        .filter(|p| status_of(p) == Some(ServerProviderUpdateStatus::Failed))
        .collect();
    if !failed.is_empty() {
        return ProviderUpdateToastView {
            phase: ProviderUpdateToastPhase::Failed,
            toast_type: ProviderUpdateToastType::Error,
            title: failed_providers_title(failed.len()),
            description: failed_provider_update_description(&failed),
            dismiss_after_visible_ms: None,
        };
    }

    let unchanged: Vec<&ServerProvider> = providers
        .iter()
        .copied()
        .filter(|p| status_of(p) == Some(ServerProviderUpdateStatus::Unchanged))
        .collect();
    if !unchanged.is_empty() {
        let verb = if unchanged.len() == 1 {
            "still appears"
        } else {
            "still appear"
        };
        return ProviderUpdateToastView {
            phase: ProviderUpdateToastPhase::Unchanged,
            toast_type: ProviderUpdateToastType::Warning,
            title: if unchanged.len() == 1 {
                "Provider still needs an update".to_owned()
            } else {
                "Providers still need updates".to_owned()
            },
            description: format!(
                "{} {verb} outdated. Check provider settings for details.",
                format_provider_list(&unchanged)
            ),
            dismiss_after_visible_ms: None,
        };
    }

    if providers.iter().any(|p| is_provider_update_active(p)) {
        return running_toast_view(provider_count);
    }

    let has_complete_snapshots = providers.len() >= provider_count;
    let all_updated = has_complete_snapshots
        && providers.iter().all(|provider| {
            status_of(provider) == Some(ServerProviderUpdateStatus::Succeeded)
                || !is_provider_update_candidate(provider)
        });
    if all_updated {
        return ProviderUpdateToastView {
            phase: ProviderUpdateToastPhase::Succeeded,
            toast_type: ProviderUpdateToastType::Success,
            title: if provider_count == 1 {
                "Provider updated".to_owned()
            } else {
                "Provider updates finished".to_owned()
            },
            description: provider_updated_description(provider_count),
            dismiss_after_visible_ms: Some(PROVIDER_UPDATE_SUCCESS_VISIBLE_MS),
        };
    }

    running_toast_view(provider_count)
}

/// One provider update sent by the cross-machine "Update all", with its result.
#[derive(Clone, Debug)]
pub struct ProviderUpdateRun {
    pub environment_id: EnvironmentId,
    pub machine_label: String,
    pub driver: ProviderDriverKind,
    pub instance_id: ProviderInstanceId,
    /// `Ok` carries the providers the backend returned; `Err` carries the
    /// failure message, or `CommandFailure::Interrupted` when cancelled.
    pub result: CommandResult<Vec<ServerProvider>, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderUpdateRunOutcome {
    Interrupted,
    Updated,
    Failed { line: String },
}

/// What one provider update in a cross-machine run did. A failure carries a
/// line that names the machine and provider, such as "alvin · Codex: npm exited".
pub fn get_provider_update_run_outcome(run: &ProviderUpdateRun) -> ProviderUpdateRunOutcome {
    if is_command_interrupted(&run.result) {
        return ProviderUpdateRunOutcome::Interrupted;
    }
    let label = format!("{} · {}", run.machine_label, driver_name(&run.driver));
    match &run.result {
        Err(CommandFailure::Failed(message)) => ProviderUpdateRunOutcome::Failed {
            line: format!("{label}: {message}"),
        },
        Err(CommandFailure::Interrupted) => ProviderUpdateRunOutcome::Interrupted,
        Ok(providers) => {
            let update_state = providers
                .iter()
                .find(|provider| provider.instance_id == run.instance_id)
                .and_then(|provider| provider.update_state.as_ref());
            if update_state.map(|state| state.status) == Some(ServerProviderUpdateStatus::Succeeded)
            {
                return ProviderUpdateRunOutcome::Updated;
            }
            let message = update_state
                .and_then(|state| state.message.as_ref())
                .map_or("Provider update did not finish.", |message| {
                    message.as_str()
                });
            ProviderUpdateRunOutcome::Failed {
                line: format!("{label}: {message}"),
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderUpdateRunToastView {
    pub toast_type: ProviderUpdateToastType,
    pub title: String,
    pub description: String,
}

/// Summarize a cross-machine "Update all" as one toast, or `None` when every
/// request was interrupted. Each update that did not succeed gets its own line.
pub fn get_provider_update_run_toast_view(
    runs: &[ProviderUpdateRun],
) -> Option<ProviderUpdateRunToastView> {
    let settled: Vec<ProviderUpdateRunOutcome> = runs
        .iter()
        .map(get_provider_update_run_outcome)
        .filter(|outcome| *outcome != ProviderUpdateRunOutcome::Interrupted)
        .collect();
    if settled.is_empty() {
        return None;
    }
    let failure_lines: Vec<&str> = settled
        .iter()
        .filter_map(|outcome| match outcome {
            ProviderUpdateRunOutcome::Failed { line } => Some(line.as_str()),
            _ => None,
        })
        .collect();
    if failure_lines.is_empty() {
        return Some(ProviderUpdateRunToastView {
            toast_type: ProviderUpdateToastType::Success,
            title: if settled.len() == 1 {
                "Provider updated".to_owned()
            } else {
                format!("{} providers updated", settled.len())
            },
            description: provider_updated_description(settled.len()),
        });
    }
    let title = if failure_lines.len() < settled.len() {
        format!(
            "{} of {} provider updates failed",
            failure_lines.len(),
            settled.len()
        )
    } else {
        failed_providers_title(settled.len())
    };
    Some(ProviderUpdateRunToastView {
        toast_type: ProviderUpdateToastType::Error,
        title,
        description: failure_lines.join("\n"),
    })
}

/// Providers the update responses report for the instances that were attempted.
pub fn collect_updated_provider_snapshots(
    results: &[CommandResult<Vec<ServerProvider>, String>],
    provider_instance_ids: &[ProviderInstanceId],
) -> Vec<ServerProvider> {
    let mut matched: Vec<ServerProvider> = Vec::new();
    for providers in results.iter().filter_map(|result| result.as_ref().ok()) {
        for provider in providers {
            if provider_instance_ids.contains(&provider.instance_id) {
                matched.push(provider.clone());
            }
        }
    }
    dedupe_providers_by_instance_id(&matched)
}

/// The message of the first failed result (upstream `firstFailedProviderUpdateMessage`).
/// An interrupted result has no message, so it gets the generic text.
pub fn first_failed_provider_update_message<T>(
    results: &[CommandResult<T, String>],
) -> Option<String> {
    match results.iter().find_map(|result| result.as_ref().err())? {
        CommandFailure::Failed(message) => Some(message.clone()),
        CommandFailure::Interrupted => Some("Provider update failed.".to_owned()),
    }
}

fn sidebar_pill_failed_key(providers: &[&ServerProvider]) -> String {
    let parts: Vec<String> = providers
        .iter()
        .map(|provider| {
            let state = provider.update_state.as_ref();
            format!(
                "{}:{}:{}",
                provider.driver.as_str(),
                state
                    .and_then(|s| s.finished_at.as_deref())
                    .unwrap_or("pending"),
                state
                    .and_then(|s| s.message.as_ref())
                    .map_or("", |message| message.as_str()),
            )
        })
        .collect();
    sorted_join(parts)
}

fn sorted_join(mut parts: Vec<String>) -> String {
    parts.sort();
    parts.join("|")
}

fn update_finished_at(provider: &ServerProvider) -> Option<&IsoDateTime> {
    provider
        .update_state
        .as_ref()
        .and_then(|state| state.finished_at.as_ref())
}

fn is_recent_terminal_provider(provider: &ServerProvider, visible_after_iso: Option<&str>) -> bool {
    let Some(status) = provider.update_state.as_ref().map(|state| state.status) else {
        return false;
    };
    if !matches!(
        status,
        ServerProviderUpdateStatus::Failed
            | ServerProviderUpdateStatus::Unchanged
            | ServerProviderUpdateStatus::Succeeded
    ) {
        return false;
    }
    match visible_after_iso {
        None => true,
        Some(visible_after) => update_finished_at(provider)
            .is_some_and(|finished_at| finished_at.as_str() >= visible_after),
    }
}

fn latest_finished_at_for_providers(providers: &[&ServerProvider]) -> Option<String> {
    providers
        .iter()
        .filter_map(|provider| update_finished_at(provider))
        .fold(None, |latest: Option<&String>, finished_at| match latest {
            Some(latest) if finished_at.as_str() <= latest.as_str() => Some(latest),
            _ => Some(finished_at),
        })
        .cloned()
}

fn sidebar_pill_candidate_for_tone<'a>(
    tone: ProviderUpdateSidebarPillTone,
    failed: &[&'a ServerProvider],
    unchanged: &[&'a ServerProvider],
    succeeded: &[&'a ServerProvider],
) -> Vec<&'a ServerProvider> {
    match tone {
        ProviderUpdateSidebarPillTone::Error => failed.to_vec(),
        ProviderUpdateSidebarPillTone::Warning => unchanged.to_vec(),
        _ => succeeded.to_vec(),
    }
}

/// The sidebar pill for the most recent relevant update state, or `None`.
pub fn get_provider_update_sidebar_pill_view(
    providers: &[ServerProvider],
    options: &ProviderUpdateSidebarPillOptions<'_>,
) -> Option<ProviderUpdateSidebarPillView> {
    let deduped = dedupe_providers_by_driver(providers);
    let active: Vec<&ServerProvider> = deduped
        .iter()
        .copied()
        .filter(|provider| is_provider_update_active(provider))
        .collect();
    if let Some(first_active) = active.first() {
        let first_name = driver_name(&first_active.driver);
        let key = sorted_join(
            active
                .iter()
                .map(|provider| {
                    format!(
                        "{}:{}",
                        provider.driver.as_str(),
                        provider
                            .update_state
                            .as_ref()
                            .map_or("idle", |state| state_status_name(state.status))
                    )
                })
                .collect(),
        );
        return Some(ProviderUpdateSidebarPillView {
            key: format!("loading:{key}"),
            tone: ProviderUpdateSidebarPillTone::Loading,
            title: if active.len() == 1 {
                format!("Updating {first_name}")
            } else {
                format!("Updating {} providers", active.len())
            },
            description: if active.len() == 1 {
                format!("{} update in progress.", format_provider_list(&active))
            } else {
                format!("{} updates are in progress.", format_provider_list(&active))
            },
            dismissible: false,
            dismiss_after_visible_ms: None,
        });
    }

    let recent: Vec<&ServerProvider> = deduped
        .iter()
        .copied()
        .filter(|provider| is_recent_terminal_provider(provider, options.visible_after_iso))
        .collect();
    let status_is = |status: ServerProviderUpdateStatus| {
        move |provider: &&ServerProvider| {
            provider.update_state.as_ref().map(|state| state.status) == Some(status)
        }
    };
    let failed: Vec<&ServerProvider> = recent
        .iter()
        .copied()
        .filter(status_is(ServerProviderUpdateStatus::Failed))
        .collect();
    let unchanged: Vec<&ServerProvider> = recent
        .iter()
        .copied()
        .filter(status_is(ServerProviderUpdateStatus::Unchanged))
        .collect();
    let succeeded: Vec<&ServerProvider> = recent
        .iter()
        .copied()
        .filter(status_is(ServerProviderUpdateStatus::Succeeded))
        .collect();

    let mut candidates: Vec<ProviderUpdateSidebarPillView> = Vec::new();
    if let Some(first_failed) = failed.first() {
        candidates.push(ProviderUpdateSidebarPillView {
            key: format!("failed:{}", sidebar_pill_failed_key(&failed)),
            tone: ProviderUpdateSidebarPillTone::Error,
            title: if failed.len() == 1 {
                provider_failed_update_title(first_failed)
            } else {
                format!("{} provider updates failed", failed.len())
            },
            description: failed_provider_update_description(&failed),
            dismissible: true,
            dismiss_after_visible_ms: None,
        });
    }
    if let Some(first_unchanged) = unchanged.first() {
        let name = driver_name(&first_unchanged.driver);
        let verb = if unchanged.len() == 1 {
            "still appears"
        } else {
            "still appear"
        };
        candidates.push(ProviderUpdateSidebarPillView {
            key: format!("unchanged:{}", sidebar_pill_failed_key(&unchanged)),
            tone: ProviderUpdateSidebarPillTone::Warning,
            title: if unchanged.len() == 1 {
                format!("{name} still needs an update")
            } else {
                format!("{} providers still need updates", unchanged.len())
            },
            description: format!(
                "{} {verb} outdated. Review provider settings for details.",
                format_provider_list(&unchanged)
            ),
            dismissible: true,
            dismiss_after_visible_ms: None,
        });
    }
    if let Some(first_succeeded) = succeeded.first() {
        candidates.push(ProviderUpdateSidebarPillView {
            key: format!("succeeded:{}", sidebar_pill_failed_key(&succeeded)),
            tone: ProviderUpdateSidebarPillTone::Success,
            title: if succeeded.len() == 1 {
                provider_updated_title(first_succeeded)
            } else {
                format!("{} providers updated", succeeded.len())
            },
            description: provider_updated_description(succeeded.len()),
            dismissible: false,
            dismiss_after_visible_ms: Some(PROVIDER_UPDATE_SUCCESS_VISIBLE_MS),
        });
    }

    // Newest finish first; ties keep the candidate order above (the sort is stable).
    candidates.sort_by(|left, right| {
        let providers_for = |view: &ProviderUpdateSidebarPillView| {
            sidebar_pill_candidate_for_tone(view.tone, &failed, &unchanged, &succeeded)
        };
        let left_finished =
            latest_finished_at_for_providers(&providers_for(left)).unwrap_or_default();
        let right_finished =
            latest_finished_at_for_providers(&providers_for(right)).unwrap_or_default();
        locale_compare(&right_finished, &left_finished)
    });
    candidates
        .into_iter()
        .find(|candidate| !options.dismissed_keys.contains(&candidate.key))
}

fn state_status_name(status: ServerProviderUpdateStatus) -> &'static str {
    match status {
        ServerProviderUpdateStatus::Idle => "idle",
        ServerProviderUpdateStatus::Queued => "queued",
        ServerProviderUpdateStatus::Running => "running",
        ServerProviderUpdateStatus::Succeeded => "succeeded",
        ServerProviderUpdateStatus::Failed => "failed",
        ServerProviderUpdateStatus::Unchanged => "unchanged",
    }
}

/// The settled result of dispatching a provider update to one local backend.
/// `provider` is the post-update snapshot of the targeted instance, or `None`
/// when the backend did not report it.
#[derive(Clone, Debug)]
pub struct LocalProviderUpdateOutcome {
    pub environment_id: EnvironmentId,
    pub is_primary: bool,
    pub driver: ProviderDriverKind,
    pub instance_id: ProviderInstanceId,
    pub provider: Option<ServerProvider>,
}

/// Worst case across backends: failed outranks unchanged, which outranks a
/// still-running one, which outranks a succeeded one.
fn provider_update_outcome_severity(provider: &ServerProvider) -> u8 {
    match provider.update_state.as_ref().map(|state| state.status) {
        Some(ServerProviderUpdateStatus::Succeeded) => 1,
        Some(ServerProviderUpdateStatus::Queued | ServerProviderUpdateStatus::Running) => 2,
        Some(ServerProviderUpdateStatus::Unchanged) => 3,
        Some(ServerProviderUpdateStatus::Failed) => 4,
        _ => 0,
    }
}

/// One representative snapshot per driver, keeping the worst status across
/// every local backend. The same driver has a distinct instance id per
/// environment, so a secondary backend's failure must surface.
pub fn collect_provider_update_outcome_snapshots(
    results: &[Result<LocalProviderUpdateOutcome, String>],
) -> Vec<ServerProvider> {
    let mut worst: IndexMap<String, ServerProvider> = IndexMap::new();
    for outcome in results.iter().filter_map(|result| result.as_ref().ok()) {
        let Some(provider) = outcome.provider.as_ref() else {
            continue;
        };
        let key = provider.driver.as_str().to_owned();
        let replace = worst.get(&key).is_none_or(|current| {
            provider_update_outcome_severity(provider) > provider_update_outcome_severity(current)
        });
        if replace {
            worst.insert(key, provider.clone());
        }
    }
    worst.into_values().collect()
}

/// The message of the first rejected dispatch (upstream `firstRejectedProviderUpdateMessage`).
pub fn first_rejected_provider_update_message<T>(results: &[Result<T, String>]) -> Option<String> {
    results
        .iter()
        .find_map(|result| result.as_ref().err())
        .cloned()
}

/// A label that tells local environments apart by platform (so the popover shows
/// "Windows" / "WSL" rather than the account name twice).
pub fn derive_environment_display_label(
    is_wsl: bool,
    wsl_distro: Option<&str>,
    platform_os: Option<ExecutionEnvironmentPlatformOs>,
    fallback_label: &str,
) -> String {
    if is_wsl {
        return match wsl_distro.filter(|distro| !distro.is_empty()) {
            Some(distro) => format!("WSL · {distro}"),
            None => "WSL".to_owned(),
        };
    }
    match platform_os {
        Some(ExecutionEnvironmentPlatformOs::Windows) => "Windows".to_owned(),
        Some(ExecutionEnvironmentPlatformOs::Darwin) => "macOS".to_owned(),
        Some(ExecutionEnvironmentPlatformOs::Linux) => "Linux".to_owned(),
        _ => fallback_label.to_owned(),
    }
}

/// Connection state of a local environment, normalized across primary and secondary sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvironmentUpdateConnectionState {
    Connecting,
    Ready,
    Disconnected,
    Error,
}

#[derive(Clone, Debug)]
pub struct LocalEnvironmentProvidersInput {
    pub environment_id: EnvironmentId,
    pub label: String,
    pub is_primary: bool,
    pub connection_state: EnvironmentUpdateConnectionState,
    pub providers: Vec<ServerProvider>,
}

#[derive(Clone, Debug)]
pub struct LocalEnvironmentUpdateGroup {
    pub environment_id: EnvironmentId,
    pub label: String,
    pub is_primary: bool,
    /// True while this environment's backend is still connecting.
    pub is_settling: bool,
    /// Outdated, one-click-updatable providers in this environment.
    pub candidates: Vec<ServerProvider>,
    /// Full provider list for this environment, used to derive live progress.
    pub providers: Vec<ServerProvider>,
}

/// One update group per local environment, pairing each environment's outdated
/// one-click candidates with its own provider list. The flag reports whether any
/// environment is still settling.
pub fn build_local_environment_update_groups(
    environments: &[LocalEnvironmentProvidersInput],
) -> (Vec<LocalEnvironmentUpdateGroup>, bool) {
    let groups = environments
        .iter()
        .map(|environment| LocalEnvironmentUpdateGroup {
            environment_id: environment.environment_id.clone(),
            label: environment.label.clone(),
            is_primary: environment.is_primary,
            is_settling: environment.connection_state
                == EnvironmentUpdateConnectionState::Connecting,
            candidates: collect_provider_update_candidates(&environment.providers)
                .into_iter()
                .filter(|candidate| {
                    can_one_click_update_provider_candidate(candidate, &environment.providers)
                })
                .cloned()
                .collect(),
            providers: environment.providers.clone(),
        })
        .collect();
    let is_any_settling = environments.iter().any(|environment| {
        environment.connection_state == EnvironmentUpdateConnectionState::Connecting
    });
    (groups, is_any_settling)
}

/// Groups that have a one-click update available, in display order.
pub fn environment_groups_with_updates(
    groups: &[LocalEnvironmentUpdateGroup],
) -> Vec<&LocalEnvironmentUpdateGroup> {
    groups
        .iter()
        .filter(|group| !group.candidates.is_empty())
        .collect()
}

/// Stable key over the (environment, driver, latest version) updates on offer.
pub fn local_environment_update_notification_key(
    groups: &[LocalEnvironmentUpdateGroup],
) -> Option<String> {
    let parts: Vec<String> = environment_groups_with_updates(groups)
        .into_iter()
        .map(|group| {
            let provider_parts = sorted_join_with(
                group
                    .candidates
                    .iter()
                    .map(|candidate| {
                        format!(
                            "{}:{}",
                            candidate.driver.as_str(),
                            latest_version_of(candidate).unwrap_or("")
                        )
                    })
                    .collect(),
                ",",
            );
            format!("{}={provider_parts}", group.environment_id.as_str())
        })
        .collect();
    let parts = sorted_join_with(parts, "|");
    (!parts.is_empty()).then_some(parts)
}

fn sorted_join_with(mut parts: Vec<String>, separator: &str) -> String {
    parts.sort();
    parts.join(separator)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderUpdateRowStatusKind {
    Idle,
    Loading,
    Success,
    Failed,
    Unchanged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderUpdateRowStatus {
    pub kind: ProviderUpdateRowStatusKind,
    pub text: String,
}

fn environment_provider_names(group: &LocalEnvironmentUpdateGroup) -> String {
    group
        .candidates
        .iter()
        .map(|candidate| driver_name(&candidate.driver))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One environment row's display, from every signal in priority order: a
/// transport error, the dispatch's terminal result, live server state, the
/// optimistic pending spinner, then idle. A non-terminal result is not
/// authoritative, so live state can still drive the row to its end.
pub fn resolve_environment_update_row_status(
    group: &LocalEnvironmentUpdateGroup,
    error: Option<&str>,
    result: Option<&ProviderUpdateToastView>,
    pill: Option<&ProviderUpdateSidebarPillView>,
    is_pending: bool,
) -> ProviderUpdateRowStatus {
    let status = |kind, text: &str| ProviderUpdateRowStatus {
        kind,
        text: text.to_owned(),
    };
    if let Some(error) = error.filter(|error| !error.is_empty()) {
        return status(ProviderUpdateRowStatusKind::Failed, error);
    }
    if let Some(result) = result {
        match result.phase {
            ProviderUpdateToastPhase::Succeeded => {
                return status(ProviderUpdateRowStatusKind::Success, "Updated");
            }
            ProviderUpdateToastPhase::Failed => {
                return status(ProviderUpdateRowStatusKind::Failed, &result.description);
            }
            ProviderUpdateToastPhase::Unchanged => {
                return status(ProviderUpdateRowStatusKind::Unchanged, &result.description);
            }
            // Running and initial are non-terminal: fall through to live state.
            ProviderUpdateToastPhase::Running | ProviderUpdateToastPhase::Initial => {}
        }
    }
    if let Some(pill) = pill {
        return match pill.tone {
            ProviderUpdateSidebarPillTone::Success => {
                status(ProviderUpdateRowStatusKind::Success, "Updated")
            }
            ProviderUpdateSidebarPillTone::Error => {
                status(ProviderUpdateRowStatusKind::Failed, &pill.description)
            }
            ProviderUpdateSidebarPillTone::Warning => {
                status(ProviderUpdateRowStatusKind::Unchanged, &pill.description)
            }
            ProviderUpdateSidebarPillTone::Loading => {
                status(ProviderUpdateRowStatusKind::Loading, "Updating…")
            }
        };
    }
    // A non-terminal result or the pending flag means an update is still in
    // flight, so keep the spinner rather than the Update button.
    if result.is_some() || is_pending {
        return status(ProviderUpdateRowStatusKind::Loading, "Updating…");
    }
    ProviderUpdateRowStatus {
        kind: ProviderUpdateRowStatusKind::Idle,
        text: environment_provider_names(group),
    }
}

#[cfg(test)]
#[path = "provider_update_launch_notification_logic_tests.rs"]
mod tests;
