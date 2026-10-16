//! Builds a [`SettingsViewModel`] from live services and the client store.

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use turin_contracts::settings::{ClientSettings, ServerSettings, StorageCleanupReport};
use turin_provider_core::server_provider::ServerProvider;
use turin_runtime::keybindings::KeybindingsConfigState;
use turin_runtime::shared_keybindings::ResolvedKeybinding;
use turin_shared::keybindings::ResolvedKeybindingRule;

use crate::state::client_state::ClientState;
use crate::state::models::EnvironmentProject;
use super::view_model::{
    AppearanceState, ArchivedThread, CleanupEntry, CleanupReport, EnvironmentInfo,
    KeybindingIssue, KeybindingsViewModel, SettingsViewModel,
};

pub struct LiveSnapshotInput<'a> {
    pub appearance: AppearanceState,
    pub client: ClientSettings,
    pub server: ServerSettings,
    pub platform: &'a str,
    pub environment: EnvironmentInfo,
    pub keybindings: KeybindingsConfigState,
    pub keybindings_config_path: String,
    pub providers: Arc<Vec<ServerProvider>>,
    pub providers_refreshing: bool,
    pub provider_auth: IndexMap<String, turin_contracts::provider_setup::ProviderAuthState>,
    pub provider_install: IndexMap<String, turin_contracts::provider_setup::ProviderInstallState>,
    pub chat_gpt_welcome_acknowledged: Vec<String>,
    pub pending_provider_auth_delivery: Option<super::view_model::ProviderAuthDelivery>,
    pub source_control: turin_contracts::source_control::SourceControlDiscoveryResult,
    pub storage_cleanup: Option<CleanupReport>,
    pub client_store: &'a ClientState,
}

pub fn build_snapshot(input: LiveSnapshotInput<'_>) -> SettingsViewModel {
    let (environment_projects, project_thread_counts, archived_threads) =
        shell_projects_and_threads(input.client_store);
    SettingsViewModel {
        appearance: input.appearance,
        client: input.client,
        server: input.server,
        platform: input.platform.to_owned(),
        environment: input.environment,
        keybindings: keybindings_vm(&input.keybindings, input.keybindings_config_path),
        providers: input.providers.iter().cloned().collect(),
        providers_refreshing: input.providers_refreshing,
        provider_auth: input.provider_auth,
        provider_install: input.provider_install,
        chat_gpt_welcome_acknowledged: input.chat_gpt_welcome_acknowledged,
        pending_provider_auth_delivery: input.pending_provider_auth_delivery,
        source_control: input.source_control,
        project_thread_counts,
        environment_projects,
        archived_threads,
        archived_threads_loading: false,
        archived_threads_error: None,
        storage_cleanup: input.storage_cleanup,
        licenses: Vec::new(),
    }
}

fn keybindings_vm(state: &KeybindingsConfigState, config_path: String) -> KeybindingsViewModel {
    KeybindingsViewModel {
        rules: state
            .keybindings
            .iter()
            .map(resolved_keybinding_rule)
            .collect(),
        issues: state
            .issues
            .iter()
            .map(|issue| KeybindingIssue {
                kind: format!("{:?}", issue),
                message: match issue {
                    turin_runtime::keybindings::ServerConfigIssue::MalformedConfig { message } => {
                        message.clone()
                    }
                    turin_runtime::keybindings::ServerConfigIssue::InvalidEntry { message, .. } => {
                        message.clone()
                    }
                },
                index: match issue {
                    turin_runtime::keybindings::ServerConfigIssue::InvalidEntry { index, .. } => {
                        Some(*index)
                    }
                    _ => None,
                },
            })
            .collect(),
        config_path,
    }
}

fn resolved_keybinding_rule(rule: &ResolvedKeybinding) -> ResolvedKeybindingRule {
    ResolvedKeybindingRule {
        command: rule.command.clone(),
        shortcut: turin_shared::keybindings::ParsedShortcut {
            key: rule.shortcut.key.clone(),
            code: rule.shortcut.code.clone(),
            meta_key: rule.shortcut.meta_key,
            ctrl_key: rule.shortcut.ctrl_key,
            shift_key: rule.shortcut.shift_key,
            alt_key: rule.shortcut.alt_key,
            mod_key: rule.shortcut.mod_key,
        },
        when_ast: rule.when_ast.clone(),
    }
}

fn shell_projects_and_threads(
    state: &ClientState,
) -> (
    Vec<EnvironmentProject>,
    IndexMap<String, usize>,
    Vec<ArchivedThread>,
) {
    let Some(view) = &state.shell_view else {
        return (Vec::new(), IndexMap::new(), Vec::new());
    };
    let projects: Vec<EnvironmentProject> = view
        .projects
        .iter()
        .map(|project| (**project).clone())
        .collect();
    let project_by_id: HashMap<_, _> = projects
        .iter()
        .map(|project| (project.id.as_str().to_owned(), project))
        .collect();
    let mut counts: IndexMap<String, usize> = IndexMap::new();
    for thread in view
        .navigation_threads
        .iter()
        .chain(view.archived_threads.iter())
    {
        if thread.archived_at.is_none() {
            *counts.entry(thread.project_id.as_str().to_owned()).or_default() += 1;
        }
    }
    let archived_threads = view
        .archived_threads
        .iter()
        .map(|thread| {
            let project = project_by_id.get(thread.project_id.as_str());
            let project_name = project.map(|p| p.title.clone()).unwrap_or_else(|| {
                thread.project_id.as_str().to_owned()
            });
            let (monogram, color) = project
                .and_then(|p| p.project_icon.as_ref())
                .map(|icon| match icon {
                    crate::state::models::ProjectIcon::Monogram { text, .. } => {
                        (text.clone(), 0x155dfc)
                    }
                    crate::state::models::ProjectIcon::Emoji(emoji) => {
                        (emoji.chars().next().unwrap_or('?').to_string(), 0x155dfc)
                    }
                    crate::state::models::ProjectIcon::Lucide { .. } => {
                        (project_name.chars().next().unwrap_or('?').to_string(), 0x155dfc)
                    }
                })
                .unwrap_or_else(|| {
                    (
                        project_name.chars().next().unwrap_or('?').to_string(),
                        0x155dfc,
                    )
                });
            ArchivedThread {
                id: thread.id.as_str().to_owned(),
                title: thread.title.clone(),
                project_name,
                project_monogram: monogram,
                project_color: color,
                archived_at: thread.archived_at.clone().unwrap_or_else(|| thread.updated_at.clone()),
                created_at: thread.created_at.clone(),
            }
        })
        .collect();
    (projects, counts, archived_threads)
}

pub fn cleanup_report(report: &StorageCleanupReport) -> CleanupReport {
    CleanupReport {
        finished_at: report.finished_at.clone(),
        bytes_freed: report.bytes_freed as u64,
        removed: report.counts.removed as usize,
        kept: report.counts.kept as usize,
        failed: report.counts.failed as usize,
        omitted: report.omitted_count as usize,
        entries: report
            .entries
            .iter()
            .map(|entry| CleanupEntry {
                name: entry
                    .path
                    .clone()
                    .or_else(|| entry.thread_id.as_ref().map(|id| id.as_str().to_owned()))
                    .unwrap_or_else(|| "entry".into()),
                thread_title: entry.thread_title.clone(),
                reason: entry.reason.clone(),
                failed: matches!(
                    entry.outcome,
                    turin_contracts::settings::StorageCleanupReportEntryOutcome::Failed
                ),
                removed: matches!(
                    entry.outcome,
                    turin_contracts::settings::StorageCleanupReportEntryOutcome::Removed
                ),
                files: entry.files.map(|value| value as u64),
                bytes: entry.bytes.map(|value| value as u64),
            })
            .collect(),
    }
}

pub fn read_client_settings() -> ClientSettings {
    super::settings_logic::use_settings::get_client_settings()
}
