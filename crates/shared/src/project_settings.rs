//! Port of upstream/packages/shared/src/projectSettings.ts
//!
//! Applies one project's overrides on top of environment settings, tracks where each scoped value
//! came from, and resolves the new-thread workspace mode and worktree cleanup rules.
//!
//! Deviations:
//! - The `t3.json` tier (`applyProjectFile`, `resolveProjectFileBackedSetting`) is cut with
//!   `t3ProjectFile` (PORTING.md cut list), so sources are `environment` or `project`.
//! - Text-generation fallback is `fallbackTextGenerationModelSelection` in serverSettings.ts, owned
//!   by N1. Callers pass it in as [`TextGenerationFallback`] until that module lands here.
//! - `Object.keys(overrides).length` counts keys holding `undefined`. Rust has no undefined, so an
//!   override with every field absent is the empty override.

use std::collections::HashMap;

use turin_contracts::environment::ThreadEnvMode;
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::provider_instance::{ProviderInstanceConfigMap, ProviderInstanceId};
use turin_contracts::settings::{
    ProjectSettingsOverrides, ProjectSettingsOverridesMap, ServerSettings, WorktreeCleanup,
    WorktreeCleanupRules, resolve_project_provider_instance_enabled,
};

use turin_contracts::base_schemas::ProjectId;

use crate::path::normalize_project_path_for_comparison;

/// Where a project-scoped value came from. The order is the priority order: a project override,
/// then the environment value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectSettingSource {
    Environment,
    Project,
}

/// Source of each scoped key. Keys without an entry came from the environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectSettingSources {
    from_project: HashMap<&'static str, ProjectSettingSource>,
}

impl ProjectSettingSources {
    pub fn get(&self, key: &str) -> ProjectSettingSource {
        self.from_project
            .get(key)
            .copied()
            .unwrap_or(ProjectSettingSource::Environment)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProjectSettings {
    /// Environment settings with the project's overrides applied.
    pub settings: ServerSettings,
    /// Where each scopable key's effective value came from.
    pub sources: ProjectSettingSources,
    /// The project's override entry after the legacy fields are merged in.
    pub overrides: ProjectSettingsOverrides,
}

/// Model fields the aggregate of a project carried before the server folded them into the override
/// record. Each is `None` when unset.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LegacyProjectSettingsFields {
    pub default_model_selection: Option<ModelSelection>,
    pub default_thread_env_mode: Option<ThreadEnvMode>,
}

/// `fallbackTextGenerationModelSelection` for the project-scoped caller: the provider instances,
/// whether an instance is enabled for this project, and the project's default model selection.
pub type TextGenerationFallback<'a> = &'a dyn Fn(
    &ProviderInstanceConfigMap,
    &dyn Fn(&ProviderInstanceId) -> bool,
    Option<&ModelSelection>,
) -> Option<ModelSelection>;

/// Cheap check so hot paths skip the projectId lookup when nothing is overridden.
pub fn has_project_settings_overrides(overrides: &ProjectSettingsOverridesMap) -> bool {
    overrides
        .values()
        .any(|entry| *entry != ProjectSettingsOverrides::default())
}

fn provider_enabled(
    settings: &ServerSettings,
    project_id: Option<&ProjectId>,
    instance_id: &ProviderInstanceId,
) -> bool {
    resolve_project_provider_instance_enabled(
        &settings.provider_instances,
        &settings.project_settings_overrides,
        project_id,
        instance_id,
    )
}

/// Overwrite a scoped setting from the override and record it as project-sourced. `plain` takes a
/// set override; `some` wraps the value for an optional setting; `nullable` takes the override's
/// `null` as a value too.
macro_rules! apply_scoped {
    ($effective:ident, $sources:ident, $overrides:ident, $key:literal, $field:ident) => {
        if let Some(value) = $overrides.$field.clone() {
            $effective.$field = value;
            $sources
                .from_project
                .insert($key, ProjectSettingSource::Project);
        }
    };
    ($effective:ident, $sources:ident, $overrides:ident, $key:literal, $field:ident, some) => {
        if let Some(value) = $overrides.$field.clone() {
            $effective.$field = Some(value);
            $sources
                .from_project
                .insert($key, ProjectSettingSource::Project);
        }
    };
    ($effective:ident, $sources:ident, $overrides:ident, $key:literal, $field:ident, nullable) => {
        if let Some(value) = $overrides.$field.clone() {
            $effective.$field = value;
            $sources
                .from_project
                .insert($key, ProjectSettingSource::Project);
        }
    };
}

/// Apply one project's overrides on top of environment settings. A model override whose provider
/// is disabled on this environment falls back to the environment value. Text generation moves off
/// an instance this project turned off.
pub fn resolve_project_settings(
    settings: &ServerSettings,
    project_id: Option<&ProjectId>,
    project: Option<&LegacyProjectSettingsFields>,
    text_generation_fallback: TextGenerationFallback<'_>,
) -> ResolvedProjectSettings {
    let stored = project_id.and_then(|id| settings.project_settings_overrides.get(id).cloned());
    let overrides = match project {
        Some(project) if !settings.project_settings_folded => {
            let legacy = ProjectSettingsOverrides {
                default_model_selection: project.default_model_selection.clone().map(Some),
                default_thread_env_mode: project.default_thread_env_mode,
                ..ProjectSettingsOverrides::default()
            };
            merge_overrides(legacy, stored.unwrap_or_default())
        }
        _ => stored.unwrap_or_default(),
    };
    if overrides == ProjectSettingsOverrides::default() {
        return ResolvedProjectSettings {
            settings: settings.clone(),
            sources: ProjectSettingSources::default(),
            overrides,
        };
    }

    let mut effective = settings.clone();
    let mut sources = ProjectSettingSources::default();
    apply_scoped!(
        effective,
        sources,
        overrides,
        "worktreeCleanup",
        worktree_cleanup,
        nullable
    );
    // A model on a provider this environment disabled falls back to the environment value.
    if let Some(value) = overrides.default_model_selection.clone() {
        let enabled = value
            .as_ref()
            .is_none_or(|model| provider_enabled(settings, project_id, &model.instance_id));
        if enabled {
            effective.default_model_selection = value;
            sources
                .from_project
                .insert("defaultModelSelection", ProjectSettingSource::Project);
        }
    }
    apply_scoped!(
        effective,
        sources,
        overrides,
        "defaultRuntimeMode",
        default_runtime_mode
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "defaultThreadEnvMode",
        default_thread_env_mode,
        some
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "newWorktreesStartFromOrigin",
        new_worktrees_start_from_origin
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "worktreeSubmodules",
        worktree_submodules,
        some
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "defaultAutoPull",
        default_auto_pull
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "defaultProjectScripts",
        default_project_scripts
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "enableAgentBrowserAccess",
        enable_agent_browser_access
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "enableAgentDeviceAccess",
        enable_agent_device_access
    );
    if let Some(value) = overrides.text_generation_model_selection.clone()
        && provider_enabled(settings, project_id, &value.instance_id)
    {
        effective.text_generation_model_selection = value;
        sources.from_project.insert(
            "textGenerationModelSelection",
            ProjectSettingSource::Project,
        );
    }
    apply_scoped!(
        effective,
        sources,
        overrides,
        "sourceControlWriterModelSelection",
        source_control_writer_model_selection,
        nullable
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "reviewGuideModelSelection",
        review_guide_model_selection,
        nullable
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "reviewGuideInstructions",
        review_guide_instructions
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "sourceControlWritingStyle",
        source_control_writing_style
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "removeAgentCreditsOnMerge",
        remove_agent_credits_on_merge
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "branchNamingMode",
        branch_naming_mode
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "branchNamePrefix",
        branch_name_prefix
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "branchNameInstructions",
        branch_name_instructions
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "pullRequestMergeMethod",
        pull_request_merge_method,
        nullable
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "sidebarAutoSettleOnMerge",
        sidebar_auto_settle_on_merge
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "sidebarAutoSettleAfterDays",
        sidebar_auto_settle_after_days,
        nullable
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "sidebarAutoSettleKeepPullRequests",
        sidebar_auto_settle_keep_pull_requests
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "continueThreadsAfterServerUpdate",
        continue_threads_after_server_update
    );
    apply_scoped!(
        effective,
        sources,
        overrides,
        "responseStreamingMode",
        response_streaming_mode
    );

    // Like the environment guard, text generation never runs on an instance this project turned off.
    if !provider_enabled(
        settings,
        project_id,
        &effective.text_generation_model_selection.instance_id,
    ) {
        let is_enabled =
            |instance_id: &ProviderInstanceId| provider_enabled(settings, project_id, instance_id);
        if let Some(fallback) = text_generation_fallback(
            &effective.provider_instances,
            &is_enabled,
            effective.default_model_selection.as_ref(),
        ) {
            effective.text_generation_model_selection = fallback;
        }
    }
    ResolvedProjectSettings {
        settings: effective,
        sources,
        overrides,
    }
}

/// `{...legacy, ...stored}`: a stored field wins; the legacy field fills what the record leaves out.
fn merge_overrides(
    legacy: ProjectSettingsOverrides,
    stored: ProjectSettingsOverrides,
) -> ProjectSettingsOverrides {
    ProjectSettingsOverrides {
        default_model_selection: stored
            .default_model_selection
            .or(legacy.default_model_selection),
        default_thread_env_mode: stored
            .default_thread_env_mode
            .or(legacy.default_thread_env_mode),
        ..stored
    }
}

/// The workspace mode a new thread starts in. A project the environment made from just a name (a
/// folder directly inside `newProjectsRoot`) has only its first commit, so until it has a thread, a
/// new thread works in the project checkout even when the environment default is a new worktree.
/// Only that environment default is replaced: a project override still wins, and so does a t3.json
/// when the environment sets no default.
pub fn resolve_new_thread_env_mode(
    project_settings: &ResolvedProjectSettings,
    workspace_root: Option<&str>,
    new_projects_root: Option<&str>,
    project_has_threads: bool,
) -> Option<ThreadEnvMode> {
    let configured = project_settings.settings.default_thread_env_mode;
    let (Some(workspace_root), Some(new_projects_root)) = (workspace_root, new_projects_root)
    else {
        return configured;
    };
    if configured == Some(ThreadEnvMode::Local)
        || project_has_threads
        || project_settings.sources.get("defaultThreadEnvMode") != ProjectSettingSource::Environment
    {
        return configured;
    }
    let root = normalize_project_path_for_comparison(new_projects_root);
    let folder = normalize_project_path_for_comparison(workspace_root);
    let separator = if root.contains('\\') { '\\' } else { '/' };
    let prefix = if root.ends_with(separator) {
        root
    } else {
        format!("{root}{separator}")
    };
    let is_new_project_folder = folder.starts_with(&prefix)
        && folder.len() > prefix.len()
        && !folder[prefix.len()..].contains(separator);
    if is_new_project_folder {
        Some(ThreadEnvMode::Local)
    } else {
        configured
    }
}

/// Replace the project's entry, dropping it entirely when nothing is overridden.
pub fn with_project_settings_overrides(
    overrides: &ProjectSettingsOverridesMap,
    project_id: &ProjectId,
    next: Option<ProjectSettingsOverrides>,
) -> ProjectSettingsOverridesMap {
    let mut rest = overrides.clone();
    rest.shift_remove(project_id);
    match next {
        Some(next) if next != ProjectSettingsOverrides::default() => {
            rest.insert(project_id.clone(), next);
            rest
        }
        _ => rest,
    }
}

/// Which keys to clear from the project's entry. `None` when that leaves the entry empty.
pub fn clear_project_settings_overrides(
    overrides: &ProjectSettingsOverridesMap,
    project_id: &ProjectId,
    keys: &[&str],
) -> Option<ProjectSettingsOverrides> {
    let mut next = overrides.get(project_id)?.clone();
    for key in keys {
        clear_override_key(&mut next, key);
    }
    (next != ProjectSettingsOverrides::default()).then_some(next)
}

/// Clears one scoped key. Keys with no field in the override record are ignored.
fn clear_override_key(overrides: &mut ProjectSettingsOverrides, key: &str) {
    match key {
        "worktreeCleanup" => overrides.worktree_cleanup = None,
        "defaultModelSelection" => overrides.default_model_selection = None,
        "defaultRuntimeMode" => overrides.default_runtime_mode = None,
        "defaultThreadEnvMode" => overrides.default_thread_env_mode = None,
        "newWorktreesStartFromOrigin" => overrides.new_worktrees_start_from_origin = None,
        "worktreeSubmodules" => overrides.worktree_submodules = None,
        "defaultAutoPull" => overrides.default_auto_pull = None,
        "defaultProjectScripts" => overrides.default_project_scripts = None,
        "enableAgentBrowserAccess" => overrides.enable_agent_browser_access = None,
        "enableAgentDeviceAccess" => overrides.enable_agent_device_access = None,
        "textGenerationModelSelection" => overrides.text_generation_model_selection = None,
        "sourceControlWriterModelSelection" => {
            overrides.source_control_writer_model_selection = None
        }
        "reviewGuideModelSelection" => overrides.review_guide_model_selection = None,
        "reviewGuideInstructions" => overrides.review_guide_instructions = None,
        "sourceControlWritingStyle" => overrides.source_control_writing_style = None,
        "removeAgentCreditsOnMerge" => overrides.remove_agent_credits_on_merge = None,
        "branchNamingMode" => overrides.branch_naming_mode = None,
        "branchNamePrefix" => overrides.branch_name_prefix = None,
        "branchNameInstructions" => overrides.branch_name_instructions = None,
        "pullRequestMergeMethod" => overrides.pull_request_merge_method = None,
        "sidebarAutoSettleOnMerge" => overrides.sidebar_auto_settle_on_merge = None,
        "sidebarAutoSettleAfterDays" => overrides.sidebar_auto_settle_after_days = None,
        "sidebarAutoSettleKeepPullRequests" => {
            overrides.sidebar_auto_settle_keep_pull_requests = None
        }
        "continueThreadsAfterServerUpdate" => overrides.continue_threads_after_server_update = None,
        "responseStreamingMode" => overrides.response_streaming_mode = None,
        _ => {}
    }
}

/// Worktree rules are project-scoped; artifact and log retention stays environment-wide.
pub fn resolve_worktree_cleanup(
    settings: &ServerSettings,
    project_id: Option<&ProjectId>,
    text_generation_fallback: TextGenerationFallback<'_>,
) -> WorktreeCleanupRules {
    let policy = resolve_project_settings(settings, project_id, None, text_generation_fallback)
        .settings
        .worktree_cleanup;
    let storage = &settings.storage_cleanup;
    match policy {
        Some(WorktreeCleanup::Custom { rules }) => rules,
        Some(WorktreeCleanup::Off) => WorktreeCleanupRules {
            worktree_keep_when: storage.worktree_keep_when,
            worktree_after_days: None,
            worktree_on_merge: false,
            worktree_on_delete: false,
            worktree_unchanged: false,
        },
        None => WorktreeCleanupRules {
            worktree_keep_when: storage.worktree_keep_when,
            worktree_after_days: storage.worktree_after_days,
            worktree_on_merge: storage.worktree_on_merge,
            worktree_on_delete: storage.worktree_on_delete,
            worktree_unchanged: storage.worktree_unchanged,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turin_contracts::provider_policy::RuntimeMode;
    use turin_contracts::settings::default_server_settings;

    fn no_fallback(
        _: &ProviderInstanceConfigMap,
        _: &dyn Fn(&ProviderInstanceId) -> bool,
        _: Option<&ModelSelection>,
    ) -> Option<ModelSelection> {
        None
    }

    fn fallback_to_claude(
        _: &ProviderInstanceConfigMap,
        _: &dyn Fn(&ProviderInstanceId) -> bool,
        _: Option<&ModelSelection>,
    ) -> Option<ModelSelection> {
        serde_json::from_value(json!({ "instanceId": "claudeAgent", "model": "claude-haiku-5-5" }))
            .ok()
    }

    fn project(id: &str) -> ProjectId {
        ProjectId::new(id).expect("valid project id")
    }

    fn with_overrides(
        settings: &mut ServerSettings,
        project_id: &ProjectId,
        overrides: serde_json::Value,
    ) {
        let parsed: ProjectSettingsOverrides =
            serde_json::from_value(overrides).expect("valid overrides");
        settings
            .project_settings_overrides
            .insert(project_id.clone(), parsed);
    }

    #[test]
    fn inherits_every_scoped_key_when_the_project_has_no_overrides() {
        let settings = default_server_settings();
        let resolved =
            resolve_project_settings(&settings, Some(&project("project-a")), None, &no_fallback);
        assert_eq!(resolved.settings, settings);
        for key in turin_contracts::settings::PROJECT_SCOPED_SERVER_SETTING_KEYS {
            assert_eq!(
                resolved.sources.get(key),
                ProjectSettingSource::Environment,
                "{key}"
            );
        }
        assert_eq!(
            resolve_project_settings(&settings, None, None, &no_fallback).settings,
            settings
        );
    }

    #[test]
    fn treats_a_null_project_like_an_absent_one() {
        let settings = default_server_settings();
        assert_eq!(
            resolve_project_settings(
                &settings,
                None,
                Some(&LegacyProjectSettingsFields::default()),
                &no_fallback
            )
            .settings,
            settings
        );
    }

    #[test]
    fn applies_overrides_per_key_and_reports_their_source() {
        let mut settings = default_server_settings();
        let id = project("project-a");
        with_overrides(
            &mut settings,
            &id,
            json!({ "defaultRuntimeMode": "full-access", "defaultAutoPull": true }),
        );
        let resolved = resolve_project_settings(&settings, Some(&id), None, &no_fallback);
        assert_eq!(
            resolved.settings.default_runtime_mode,
            RuntimeMode::FullAccess
        );
        assert!(resolved.settings.default_auto_pull);
        assert_eq!(
            resolved.sources.get("defaultRuntimeMode"),
            ProjectSettingSource::Project
        );
        assert_eq!(
            resolved.sources.get("defaultAutoPull"),
            ProjectSettingSource::Project
        );
        assert_eq!(
            resolved.sources.get("responseStreamingMode"),
            ProjectSettingSource::Environment
        );
    }

    #[test]
    fn a_null_override_clears_a_nullable_setting() {
        let mut settings = default_server_settings();
        settings.default_thread_env_mode = Some(ThreadEnvMode::Worktree);
        let id = project("project-a");
        with_overrides(&mut settings, &id, json!({ "worktreeCleanup": null }));
        let resolved = resolve_project_settings(&settings, Some(&id), None, &no_fallback);
        assert_eq!(resolved.settings.worktree_cleanup, None);
        assert_eq!(
            resolved.sources.get("worktreeCleanup"),
            ProjectSettingSource::Project
        );
    }

    #[test]
    fn keeps_the_environment_text_generation_model_when_the_overrides_provider_is_disabled() {
        let mut settings = default_server_settings();
        let id = project("project-a");
        with_overrides(
            &mut settings,
            &id,
            json!({
                "providerInstances": { "claudeAgent": { "enabled": false } },
                "textGenerationModelSelection": { "instanceId": "claudeAgent", "model": "claude-opus-4-6" },
            }),
        );
        let environment = settings.text_generation_model_selection.clone();
        let resolved = resolve_project_settings(&settings, Some(&id), None, &no_fallback);
        assert_eq!(
            resolved.settings.text_generation_model_selection,
            environment
        );
        assert_eq!(
            resolved.sources.get("textGenerationModelSelection"),
            ProjectSettingSource::Environment
        );
    }

    #[test]
    fn moves_text_generation_off_an_instance_the_project_turned_off() {
        let mut settings = default_server_settings();
        let id = project("project-a");
        // The environment's text generation instance is disabled for this project.
        let environment_instance = settings.text_generation_model_selection.instance_id.clone();
        with_overrides(
            &mut settings,
            &id,
            json!({ "providerInstances": { environment_instance.as_str(): { "enabled": false } } }),
        );
        let resolved = resolve_project_settings(&settings, Some(&id), None, &fallback_to_claude);
        assert_eq!(
            resolved
                .settings
                .text_generation_model_selection
                .instance_id
                .as_str(),
            "claudeAgent"
        );
        assert_eq!(
            resolved
                .settings
                .text_generation_model_selection
                .model
                .as_str(),
            "claude-haiku-5-5"
        );
        // Keys whose value comes from the fallback keep their source.
        assert_eq!(
            resolved.sources.get("textGenerationModelSelection"),
            ProjectSettingSource::Environment
        );
    }

    #[test]
    fn legacy_project_fields_fill_the_overrides_until_the_server_folds_them() {
        let settings = default_server_settings();
        let legacy = LegacyProjectSettingsFields {
            default_model_selection: None,
            default_thread_env_mode: Some(ThreadEnvMode::Local),
        };
        let resolved = resolve_project_settings(
            &settings,
            Some(&project("project-a")),
            Some(&legacy),
            &no_fallback,
        );
        assert_eq!(
            resolved.settings.default_thread_env_mode,
            Some(ThreadEnvMode::Local)
        );
        assert_eq!(
            resolved.sources.get("defaultThreadEnvMode"),
            ProjectSettingSource::Project
        );

        let mut folded = settings.clone();
        folded.project_settings_folded = true;
        let resolved = resolve_project_settings(
            &folded,
            Some(&project("project-a")),
            Some(&legacy),
            &no_fallback,
        );
        assert_eq!(resolved.settings.default_thread_env_mode, None);
    }

    #[test]
    fn new_projects_start_their_first_thread_in_the_checkout() {
        let mut settings = default_server_settings();
        settings.default_thread_env_mode = Some(ThreadEnvMode::Worktree);
        let resolved =
            resolve_project_settings(&settings, Some(&project("project-a")), None, &no_fallback);
        assert_eq!(
            resolve_new_thread_env_mode(
                &resolved,
                Some("/Users/me/Projects/new-app"),
                Some("/Users/me/Projects/"),
                false
            ),
            Some(ThreadEnvMode::Local)
        );
    }

    #[test]
    fn keeps_the_configured_mode_once_the_project_has_a_thread_or_is_not_a_new_project() {
        let mut settings = default_server_settings();
        settings.default_thread_env_mode = Some(ThreadEnvMode::Worktree);
        let resolved =
            resolve_project_settings(&settings, Some(&project("project-a")), None, &no_fallback);
        for (workspace, has_threads) in [
            ("/Users/me/Projects/new-app", true),
            ("/Users/me/Projects/new-app/nested", false),
            ("/Users/me/code/app", false),
        ] {
            assert_eq!(
                resolve_new_thread_env_mode(
                    &resolved,
                    Some(workspace),
                    Some("/Users/me/Projects"),
                    has_threads
                ),
                Some(ThreadEnvMode::Worktree),
                "{workspace}"
            );
        }
        assert_eq!(
            resolve_new_thread_env_mode(&resolved, None, Some("/Users/me/Projects"), false),
            Some(ThreadEnvMode::Worktree)
        );
    }

    #[test]
    fn a_project_override_keeps_worktree_mode() {
        let mut settings = default_server_settings();
        settings.default_thread_env_mode = Some(ThreadEnvMode::Worktree);
        let id = project("project-a");
        with_overrides(
            &mut settings,
            &id,
            json!({ "defaultThreadEnvMode": "worktree" }),
        );
        let resolved = resolve_project_settings(&settings, Some(&id), None, &no_fallback);
        assert_eq!(
            resolve_new_thread_env_mode(
                &resolved,
                Some("/Users/me/Projects/new-app"),
                Some("/Users/me/Projects"),
                false
            ),
            Some(ThreadEnvMode::Worktree)
        );
    }

    #[test]
    fn replaces_and_removes_a_projects_entry() {
        let settings = default_server_settings();
        let id = project("project-a");
        let overrides: ProjectSettingsOverrides =
            serde_json::from_value(json!({ "defaultAutoPull": true })).unwrap();
        let with = with_project_settings_overrides(
            &settings.project_settings_overrides,
            &id,
            Some(overrides.clone()),
        );
        assert_eq!(with.get(&id), Some(&overrides));
        let without =
            with_project_settings_overrides(&with, &id, Some(ProjectSettingsOverrides::default()));
        assert!(without.get(&id).is_none());
        assert!(!has_project_settings_overrides(&without));
        assert!(has_project_settings_overrides(&with));
    }

    #[test]
    fn clears_keys_and_returns_none_when_the_entry_empties() {
        let mut settings = default_server_settings();
        let id = project("project-a");
        with_overrides(
            &mut settings,
            &id,
            json!({ "defaultAutoPull": true, "defaultRuntimeMode": "full-access" }),
        );
        let cleared = clear_project_settings_overrides(
            &settings.project_settings_overrides,
            &id,
            &["defaultAutoPull"],
        )
        .unwrap();
        assert_eq!(cleared.default_auto_pull, None);
        assert_eq!(cleared.default_runtime_mode, Some(RuntimeMode::FullAccess));
        assert_eq!(
            clear_project_settings_overrides(
                &settings.project_settings_overrides,
                &id,
                &["defaultAutoPull", "defaultRuntimeMode"]
            ),
            None
        );
        assert_eq!(
            clear_project_settings_overrides(
                &settings.project_settings_overrides,
                &project("other"),
                &["defaultAutoPull"]
            ),
            None
        );
    }

    #[test]
    fn worktree_cleanup_inherits_machine_rules_unless_the_project_overrides_them() {
        let mut settings = default_server_settings();
        let machine = resolve_worktree_cleanup(&settings, None, &no_fallback);
        assert_eq!(
            machine.worktree_on_merge,
            settings.storage_cleanup.worktree_on_merge
        );

        let id = project("project-a");
        with_overrides(
            &mut settings,
            &id,
            json!({ "worktreeCleanup": { "mode": "off" } }),
        );
        let off = resolve_worktree_cleanup(&settings, Some(&id), &no_fallback);
        assert_eq!(off.worktree_after_days, None);
        assert!(!off.worktree_on_merge && !off.worktree_on_delete && !off.worktree_unchanged);
        // The machine's rules stay for every other project.
        assert_eq!(
            resolve_worktree_cleanup(&settings, Some(&project("project-b")), &no_fallback),
            machine
        );
    }
}
