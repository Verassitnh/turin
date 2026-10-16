//! Port of upstream/apps/web/src/components/settings/scopedSettings.ts
//!
//! Plans scoped settings reads and writes across environments and project overrides.
//! Patches use [`serde_json::Value`] objects with upstream merge semantics; server
//! persistence uses [`ServerSettingsPatch`] where upstream does.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::OnceLock;

use indexmap::IndexMap;
use serde_json::{Map, Value, json};
use turin_contracts::base_schemas::ProjectId;
use turin_contracts::settings::{
    ClientSettings, PROJECT_SCOPED_SERVER_SETTING_KEYS, ServerSettings, ServerSettingsPatch,
    default_server_settings, is_nullable_project_settings_override,
};
use turin_shared::project_settings::{
    ProjectSettingSource, TextGenerationFallback, clear_project_settings_overrides,
    resolve_project_settings, resolve_worktree_cleanup,
};

use super::settings_scope::{ResolvedSettingsScope, SettingsScopeKind};
use crate::state::contracts::EnvironmentId;
use crate::state::helpers::usage_access::EnvironmentConnectionPhase;
use crate::state::project_grouping::GroupableProject;

pub type ScopedSettingsPatch = Value;

#[derive(Clone, Debug)]
pub struct ScopedSettingsServerConfig {
    pub settings: ServerSettings,
    pub project_settings_overrides_capable: bool,
}

#[derive(Clone, Debug)]
pub struct ScopedSettingsEnvironment {
    pub environment_id: EnvironmentId,
    pub label: String,
    pub connection_phase: EnvironmentConnectionPhase,
    pub server_config: Option<ScopedSettingsServerConfig>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScopedSettingsTarget {
    pub environment_id: EnvironmentId,
    pub label: String,
    pub project_id: Option<ProjectId>,
    pub settings: ServerSettings,
    pub sources: HashMap<&'static str, ProjectSettingSource>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ScopedSettingSource {
    Environment,
    Project,
    Mixed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScopedServerWrite {
    pub environment_id: EnvironmentId,
    pub label: String,
    pub patch: ServerSettingsPatch,
    pub previous: ServerSettings,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScopedSettingsPlan {
    pub client_patch: Value,
    pub has_client_write: bool,
    pub server_writes: Vec<ScopedServerWrite>,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectOverrideEntry {
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PersistScopedSettingsResult {
    pub failed_environments: Vec<FailedScopedSettingsEnvironment>,
    pub saved_environments: Vec<ScopedServerWrite>,
    pub saved_environment_count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FailedScopedSettingsEnvironment {
    pub environment_id: EnvironmentId,
    pub label: String,
    pub previous: ServerSettings,
    pub patch: ServerSettingsPatch,
    pub message: String,
}

pub fn is_project_scoped_setting_key(key: &str) -> bool {
    PROJECT_SCOPED_SERVER_SETTING_KEYS.contains(&key)
}

fn server_field_keys() -> &'static HashSet<String> {
    static KEYS: OnceLock<HashSet<String>> = OnceLock::new();
    KEYS.get_or_init(|| {
        let mut keys: HashSet<String> = serde_json::to_value(default_server_settings())
            .expect("server settings")
            .as_object()
            .expect("object")
            .keys()
            .cloned()
            .collect();
        if let Some(patch_keys) = serde_json::to_value(ServerSettingsPatch::default())
            .ok()
            .and_then(|value| value.as_object().cloned())
        {
            keys.extend(patch_keys.keys().cloned());
        }
        keys.extend(
            PROJECT_SCOPED_SERVER_SETTING_KEYS
                .iter()
                .map(|key| (*key).to_owned()),
        );
        keys
    })
}

fn client_field_keys() -> &'static HashSet<String> {
    static KEYS: OnceLock<HashSet<String>> = OnceLock::new();
    KEYS.get_or_init(|| {
        serde_json::to_value(ClientSettings::default())
            .expect("client settings")
            .as_object()
            .expect("object")
            .keys()
            .cloned()
            .collect()
    })
}

fn patch_object(patch: &Value) -> Map<String, Value> {
    patch.as_object().cloned().unwrap_or_default()
}

fn split_scoped_settings_patch(patch: &Value) -> (Value, Value) {
    let server_keys = server_field_keys();
    let client_keys = client_field_keys();
    let mut client = Map::new();
    let mut server = Map::new();
    for (key, value) in patch_object(patch) {
        if server_keys.contains(&key) {
            server.insert(key, value);
        } else if client_keys.contains(&key) {
            client.insert(key, value);
        }
    }
    (Value::Object(client), Value::Object(server))
}

fn is_plain_object(value: &Value) -> bool {
    value.is_object()
}

fn merge_json_objects(base: &Value, patch: &Value) -> Value {
    match (base.as_object(), patch.as_object()) {
        (Some(base), Some(patch)) => {
            let mut merged = base.clone();
            for (key, value) in patch {
                merged.insert(key.clone(), value.clone());
            }
            Value::Object(merged)
        }
        _ => patch.clone(),
    }
}

fn server_settings_field_value(settings: &ServerSettings, key: &str) -> Value {
    serde_json::to_value(settings)
        .expect("settings")
        .as_object()
        .and_then(|object| object.get(key))
        .cloned()
        .unwrap_or(Value::Null)
}

/// The representative supplies display values, never the set of write targets.
pub fn select_scoped_settings_environments<'a, P, T: ScopedSettingsEnvironmentView>(
    scope: &ResolvedSettingsScope<P>,
    available: &'a [T],
    primary_environment_id: Option<&EnvironmentId>,
) -> SelectedScopedSettingsEnvironments<'a, T> {
    let selected_ids: HashSet<_> = scope.environment_ids.iter().collect();
    let environments: Vec<_> = available
        .iter()
        .filter(|environment| selected_ids.contains(&environment.environment_id()))
        .collect();
    let connected_environments: Vec<_> = environments
        .iter()
        .copied()
        .filter(|environment| {
            environment.connection_phase() == EnvironmentConnectionPhase::Connected
                && environment.server_config().is_some()
        })
        .collect();
    let environment = primary_environment_id
        .and_then(|id| {
            connected_environments
                .iter()
                .find(|candidate| candidate.environment_id() == id)
                .copied()
        })
        .or_else(|| connected_environments.first().copied());
    SelectedScopedSettingsEnvironments {
        environments,
        connected_environments,
        environment,
    }
}

pub struct SelectedScopedSettingsEnvironments<'a, T> {
    pub environments: Vec<&'a T>,
    pub connected_environments: Vec<&'a T>,
    pub environment: Option<&'a T>,
}

pub trait ScopedSettingsEnvironmentView {
    fn environment_id(&self) -> &EnvironmentId;
    fn connection_phase(&self) -> EnvironmentConnectionPhase;
    fn server_config(&self) -> Option<&ScopedSettingsServerConfig>;
}

impl ScopedSettingsEnvironmentView for ScopedSettingsEnvironment {
    fn environment_id(&self) -> &EnvironmentId {
        &self.environment_id
    }
    fn connection_phase(&self) -> EnvironmentConnectionPhase {
        self.connection_phase
    }
    fn server_config(&self) -> Option<&ScopedSettingsServerConfig> {
        self.server_config.as_ref()
    }
}

pub fn resolve_scoped_settings_targets<P: GroupableProject + Clone>(
    scope: &ResolvedSettingsScope<P>,
    connected_environments: &[ScopedSettingsEnvironment],
    text_generation_fallback: TextGenerationFallback<'_>,
) -> Vec<ScopedSettingsTarget> {
    let by_id: HashMap<_, _> = connected_environments
        .iter()
        .map(|environment| (environment.environment_id.clone(), environment))
        .collect();
    match &scope.kind {
        SettingsScopeKind::Project { .. } | SettingsScopeKind::Checkout { .. } => scope
            .members
            .iter()
            .flat_map(|member| {
                let environment = by_id.get(member.project.environment_id())?;
                let server_config = environment.server_config.as_ref()?;
                let resolved = resolve_project_settings(
                    &server_config.settings,
                    Some(member.project.id()),
                    None,
                    text_generation_fallback,
                );
                let sources = PROJECT_SCOPED_SERVER_SETTING_KEYS
                    .iter()
                    .map(|key| (*key, resolved.sources.get(key)))
                    .collect();
                Some(ScopedSettingsTarget {
                    environment_id: member.project.environment_id().clone(),
                    label: environment.label.clone(),
                    project_id: Some(member.project.id().clone()),
                    settings: resolved.settings,
                    sources,
                })
            })
            .collect(),
        _ => connected_environments
            .iter()
            .flat_map(|environment| {
                let server_config = environment.server_config.as_ref()?;
                let resolved = resolve_project_settings(
                    &server_config.settings,
                    None,
                    None,
                    text_generation_fallback,
                );
                let sources = PROJECT_SCOPED_SERVER_SETTING_KEYS
                    .iter()
                    .map(|key| (*key, resolved.sources.get(key)))
                    .collect();
                Some(ScopedSettingsTarget {
                    environment_id: environment.environment_id.clone(),
                    label: environment.label.clone(),
                    project_id: None,
                    settings: server_config.settings.clone(),
                    sources,
                })
            })
            .collect(),
    }
}

pub fn scoped_settings_are_mixed(targets: &[ScopedSettingsTarget], keys: &[&str]) -> bool {
    let Some(first) = targets.first() else {
        return false;
    };
    targets.iter().any(|candidate| {
        keys.iter().any(|key| {
            server_settings_field_value(&first.settings, key)
                != server_settings_field_value(&candidate.settings, key)
        })
    })
}

pub fn scoped_settings_source(
    targets: &[impl ScopedSettingsTargetSources],
    keys: &[&str],
) -> ScopedSettingSource {
    let scoped: Vec<_> = keys
        .iter()
        .copied()
        .filter(|key| is_project_scoped_setting_key(key))
        .collect();
    if scoped.is_empty() || targets.is_empty() {
        return ScopedSettingSource::Environment;
    }
    let mut sources = Vec::new();
    for target in targets {
        for key in &scoped {
            let source = target.source_for_key(key);
            if !sources.contains(&source) {
                sources.push(source);
            }
        }
    }
    if sources.len() > 1 {
        ScopedSettingSource::Mixed
    } else if sources.contains(&ProjectSettingSource::Project) {
        ScopedSettingSource::Project
    } else {
        ScopedSettingSource::Environment
    }
}

pub trait ScopedSettingsTargetSources {
    fn source_for_key(&self, key: &str) -> ProjectSettingSource;
}

impl ScopedSettingsTargetSources for ScopedSettingsTarget {
    fn source_for_key(&self, key: &str) -> ProjectSettingSource {
        self.sources
            .get(key)
            .copied()
            .unwrap_or(ProjectSettingSource::Environment)
    }
}

fn is_project_scope<P>(scope: &ResolvedSettingsScope<P>) -> bool {
    matches!(
        scope.kind,
        SettingsScopeKind::Project { .. } | SettingsScopeKind::Checkout { .. }
    )
}

fn project_override_writes<P, F>(
    scope: &ResolvedSettingsScope<P>,
    environments: &[ScopedSettingsEnvironment],
    mut update: F,
) -> Vec<ScopedServerWrite>
where
    P: GroupableProject + Clone,
    F: FnMut(
        ProjectSettingsOverridesEntry,
        &ServerSettings,
        &ProjectId,
    ) -> Option<ProjectSettingsOverridesEntry>,
{
    let by_id: HashMap<_, _> = environments
        .iter()
        .map(|environment| (environment.environment_id.clone(), environment))
        .collect();
    let mut writes: IndexMap<EnvironmentId, ScopedServerWrite> = IndexMap::new();
    for member in &scope.members {
        let Some(environment) = by_id.get(member.project.environment_id()) else {
            continue;
        };
        let Some(server_config) = environment.server_config.as_ref() else {
            continue;
        };
        if environment.connection_phase != EnvironmentConnectionPhase::Connected
            || !server_config.project_settings_overrides_capable
        {
            continue;
        }
        let settings = &server_config.settings;
        let current = settings
            .project_settings_overrides
            .get(member.project.id())
            .cloned()
            .unwrap_or_default();
        let entry = update(current, settings, member.project.id());
        let existing = writes.get(&member.project.environment_id().clone());
        let mut overrides = existing
            .and_then(|write| write.patch.project_settings_overrides.clone())
            .unwrap_or_default();
        overrides.insert(member.project.id().clone(), entry);
        writes.insert(
            member.project.environment_id().clone(),
            ScopedServerWrite {
                environment_id: member.project.environment_id().clone(),
                label: environment.label.clone(),
                previous: settings.clone(),
                patch: ServerSettingsPatch {
                    project_settings_overrides: Some(overrides),
                    ..Default::default()
                },
            },
        );
    }
    writes.into_values().collect()
}

type ProjectSettingsOverridesEntry = turin_contracts::settings::ProjectSettingsOverrides;

fn apply_server_value_to_project_override(
    settings: &ServerSettings,
    project_id: &ProjectId,
    current: ProjectSettingsOverridesEntry,
    key: &str,
    value: &Value,
    text_generation_fallback: TextGenerationFallback<'_>,
) -> ProjectSettingsOverridesEntry {
    if key == "worktreeCleanup" {
        let mode = value.get("mode").and_then(Value::as_str);
        if mode == Some("custom") {
            let rules =
                resolve_worktree_cleanup(settings, Some(project_id), text_generation_fallback);
            let rules_value = serde_json::to_value(rules).expect("rules");
            let merged_rules =
                merge_json_objects(&rules_value, value.get("rules").unwrap_or(&Value::Null));
            let mut next = serde_json::to_value(&current).expect("override");
            next.as_object_mut().expect("object").insert(
                key.to_owned(),
                json!({ "mode": "custom", "rules": merged_rules }),
            );
            return serde_json::from_value(next).unwrap_or(current);
        }
    }
    if value.is_null() && !is_nullable_project_settings_override(key) {
        let mut next = serde_json::to_value(&current).expect("override");
        if let Some(object) = next.as_object_mut() {
            object.remove(key);
        }
        return serde_json::from_value(next).unwrap_or(current);
    }
    let effective =
        resolve_project_settings(settings, Some(project_id), None, text_generation_fallback)
            .settings;
    let base = server_settings_field_value(&effective, key);
    let merged = if is_plain_object(value) && is_plain_object(&base) {
        merge_json_objects(&base, value)
    } else {
        value.clone()
    };
    let mut next = serde_json::to_value(&current).expect("override");
    next.as_object_mut()
        .expect("object")
        .insert(key.to_owned(), merged);
    serde_json::from_value(next).unwrap_or(current)
}

fn apply_server_patch_to_project_override(
    settings: &ServerSettings,
    project_id: &ProjectId,
    current: ProjectSettingsOverridesEntry,
    server_patch: &Value,
    text_generation_fallback: TextGenerationFallback<'_>,
) -> ProjectSettingsOverridesEntry {
    let mut next = current;
    for (key, value) in patch_object(server_patch) {
        next = apply_server_value_to_project_override(
            settings,
            project_id,
            next,
            &key,
            &value,
            text_generation_fallback,
        );
    }
    next
}

fn environment_server_patch(
    previous: &ServerSettings,
    server_patch: &Value,
    text_generation_fallback: TextGenerationFallback<'_>,
) -> ServerSettingsPatch {
    let worktree_cleanup_set = previous.worktree_cleanup.is_some();
    let storage = server_patch.get("storageCleanup");
    let maps_worktree = storage
        .and_then(Value::as_object)
        .is_some_and(|object| object.keys().any(|key| key.starts_with("worktree")));
    if worktree_cleanup_set && maps_worktree {
        let rules = resolve_worktree_cleanup(previous, None, text_generation_fallback);
        let mut rules_object = serde_json::to_value(rules)
            .expect("rules")
            .as_object()
            .expect("object")
            .clone();
        if let Some(storage_object) = storage.and_then(Value::as_object) {
            for (key, value) in storage_object {
                if key.starts_with("worktree") {
                    rules_object.insert(key.clone(), value.clone());
                }
            }
        }
        let mut patch = patch_object(server_patch);
        patch.remove("storageCleanup");
        patch.insert(
            "worktreeCleanup".to_owned(),
            json!({ "mode": "custom", "rules": Value::Object(rules_object) }),
        );
        serde_json::from_value(Value::Object(patch)).unwrap_or_default()
    } else {
        serde_json::from_value(server_patch.clone()).unwrap_or_default()
    }
}

pub fn plan_scoped_settings_patch<P: GroupableProject + Clone>(
    scope: &ResolvedSettingsScope<P>,
    environments: &[ScopedSettingsEnvironment],
    patch: &ScopedSettingsPatch,
    text_generation_fallback: TextGenerationFallback<'_>,
) -> ScopedSettingsPlan {
    let (client_patch, server_patch) = split_scoped_settings_patch(patch);
    let server_keys: Vec<_> = patch_object(&server_patch).keys().cloned().collect();
    let selected = select_scoped_settings_environments(scope, environments, None);
    let project_scope = is_project_scope(scope);
    let unscopable_keys: Vec<_> = if project_scope {
        server_keys
            .iter()
            .filter(|key| !is_project_scoped_setting_key(key))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let server_writes = if server_keys.is_empty() {
        Vec::new()
    } else if project_scope {
        if !unscopable_keys.is_empty() {
            Vec::new()
        } else {
            let server_patch = server_patch.clone();
            project_override_writes(scope, environments, |current, settings, project_id| {
                Some(apply_server_patch_to_project_override(
                    settings,
                    project_id,
                    current,
                    &server_patch,
                    text_generation_fallback,
                ))
            })
        }
    } else if matches!(
        scope.kind,
        SettingsScopeKind::All | SettingsScopeKind::Environment { .. }
    ) {
        selected
            .connected_environments
            .iter()
            .flat_map(|environment| {
                let server_config = environment.server_config.as_ref()?;
                let previous = &server_config.settings;
                Some(ScopedServerWrite {
                    environment_id: environment.environment_id.clone(),
                    label: environment.label.clone(),
                    previous: previous.clone(),
                    patch: environment_server_patch(
                        previous,
                        &server_patch,
                        text_generation_fallback,
                    ),
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    let has_client_write = !patch_object(&client_patch).is_empty();
    let has_write = has_client_write || !server_writes.is_empty();
    let unavailable_reason = if has_write || patch_object(patch).is_empty() {
        None
    } else if let SettingsScopeKind::Unavailable { message, .. } = &scope.kind {
        Some(message.clone())
    } else if !unscopable_keys.is_empty() {
        Some("This setting is environment-wide and cannot be overridden by a project.".to_owned())
    } else if project_scope {
        Some(
            "Connect the selected checkouts, or update their environments, to save a project override."
                .to_owned(),
        )
    } else {
        let label = match &scope.kind {
            SettingsScopeKind::Environment { .. } => scope.label.clone(),
            _ => "an environment".to_owned(),
        };
        Some(format!("Connect {label} to save this setting."))
    };
    ScopedSettingsPlan {
        client_patch,
        has_client_write,
        server_writes,
        unavailable_reason,
    }
}

pub fn plan_scoped_settings_clear<P: GroupableProject + Clone>(
    scope: &ResolvedSettingsScope<P>,
    environments: &[ScopedSettingsEnvironment],
    keys: &[&str],
) -> ScopedSettingsPlan {
    let server_writes = if is_project_scope(scope) {
        project_override_writes(scope, environments, |_current, settings, project_id| {
            clear_project_settings_overrides(&settings.project_settings_overrides, project_id, keys)
        })
    } else {
        Vec::new()
    };
    let unavailable_reason = if server_writes.is_empty() {
        Some(
            "Connect the selected checkouts, or update their environments, to reset this override."
                .to_owned(),
        )
    } else {
        None
    };
    ScopedSettingsPlan {
        client_patch: json!({}),
        has_client_write: false,
        server_writes,
        unavailable_reason,
    }
}

pub fn list_project_overrides(
    environments: &[ScopedSettingsEnvironment],
    keys: &[&str],
) -> Vec<ProjectOverrideEntry> {
    let scoped: Vec<_> = keys
        .iter()
        .copied()
        .filter(|key| is_project_scoped_setting_key(key))
        .collect();
    if scoped.is_empty() {
        return Vec::new();
    }
    environments
        .iter()
        .flat_map(|environment| {
            let Some(overrides) = environment
                .server_config
                .as_ref()
                .map(|config| &config.settings.project_settings_overrides)
            else {
                return Vec::new();
            };
            overrides
                .iter()
                .flat_map(|(project_id, entry)| {
                    let entry_value = serde_json::to_value(entry).expect("entry");
                    let has_key = scoped.iter().any(|key| {
                        entry_value
                            .as_object()
                            .and_then(|object| object.get(*key))
                            .is_some()
                    });
                    has_key.then_some(ProjectOverrideEntry {
                        environment_id: environment.environment_id.clone(),
                        project_id: project_id.clone(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

pub fn plan_project_overrides_clear(
    environments: &[ScopedSettingsEnvironment],
    entries: &[ProjectOverrideEntry],
    keys: &[&str],
) -> ScopedSettingsPlan {
    let by_id: HashMap<_, _> = environments
        .iter()
        .map(|environment| (environment.environment_id.clone(), environment))
        .collect();
    let mut writes: IndexMap<EnvironmentId, ScopedServerWrite> = IndexMap::new();
    for entry in entries {
        let Some(environment) = by_id.get(&entry.environment_id) else {
            continue;
        };
        let Some(server_config) = environment.server_config.as_ref() else {
            continue;
        };
        if environment.connection_phase != EnvironmentConnectionPhase::Connected {
            continue;
        }
        let settings = &server_config.settings;
        let cleared = clear_project_settings_overrides(
            &settings.project_settings_overrides,
            &entry.project_id,
            keys,
        );
        let existing = writes.get(&entry.environment_id);
        let mut overrides = existing
            .and_then(|write| write.patch.project_settings_overrides.clone())
            .unwrap_or_default();
        overrides.insert(entry.project_id.clone(), cleared);
        writes.insert(
            entry.environment_id.clone(),
            ScopedServerWrite {
                environment_id: entry.environment_id.clone(),
                label: environment.label.clone(),
                previous: settings.clone(),
                patch: ServerSettingsPatch {
                    project_settings_overrides: Some(overrides),
                    ..Default::default()
                },
            },
        );
    }
    let server_writes: Vec<_> = writes.into_values().collect();
    ScopedSettingsPlan {
        client_patch: json!({}),
        has_client_write: false,
        server_writes: server_writes.clone(),
        unavailable_reason: if server_writes.is_empty() {
            Some("Connect the environments to reset these overrides.".to_owned())
        } else {
            None
        },
    }
}

fn write_was_dropped(value: &Value, before: &Value, saved: &Value) -> bool {
    if value.is_null() {
        return !before.is_null() && saved == before;
    }
    if is_plain_object(value) && !before.is_array() && !saved.is_array() {
        return value
            .as_object()
            .expect("object")
            .iter()
            .any(|(key, next)| {
                write_was_dropped(
                    next,
                    before
                        .as_object()
                        .and_then(|object| object.get(key))
                        .unwrap_or(&Value::Null),
                    saved
                        .as_object()
                        .and_then(|object| object.get(key))
                        .unwrap_or(&Value::Null),
                )
            });
    }
    let unchanged = match (value.as_str(), before.as_str()) {
        (Some(value), Some(before)) => value.trim() == before,
        _ => value == before,
    };
    !unchanged && saved == before
}

fn dropped_setting_keys(write: &ScopedServerWrite, saved: &ServerSettings) -> Vec<String> {
    let saved_value = serde_json::to_value(saved).expect("saved");
    let previous_value = serde_json::to_value(&write.previous).expect("previous");
    let patch_value = serde_json::to_value(&write.patch).expect("patch");
    patch_value
        .as_object()
        .into_iter()
        .flatten()
        .flat_map(|(key, value)| {
            write_was_dropped(
                value,
                &previous_value
                    .as_object()
                    .and_then(|object| object.get(key))
                    .cloned()
                    .unwrap_or(Value::Null),
                &saved_value
                    .as_object()
                    .and_then(|object| object.get(key))
                    .cloned()
                    .unwrap_or(Value::Null),
            )
            .then(|| key.clone())
        })
        .collect()
}

pub type PersistServerResult =
    Result<Option<ServerSettings>, Box<dyn std::error::Error + Send + Sync>>;

pub async fn persist_scoped_settings_patch<F, C, Fut>(
    plan: &ScopedSettingsPlan,
    mut persist_server: F,
    mut persist_client: C,
) -> PersistScopedSettingsResult
where
    F: FnMut(EnvironmentId, ServerSettingsPatch) -> Fut,
    Fut: Future<Output = PersistServerResult>,
    C: FnMut(Value),
{
    if plan.has_client_write {
        persist_client(plan.client_patch.clone());
    }
    let mut results = Vec::with_capacity(plan.server_writes.len());
    for write in &plan.server_writes {
        let result = persist_server(write.environment_id.clone(), write.patch.clone()).await;
        results.push(result);
    }
    let saved = |index: usize| {
        results.get(index).is_some_and(|result| {
            result.as_ref().is_ok_and(|saved| match saved {
                None => true,
                Some(saved) => dropped_setting_keys(&plan.server_writes[index], saved).is_empty(),
            })
        })
    };
    let failed_environments = plan
        .server_writes
        .iter()
        .enumerate()
        .flat_map(|(index, environment)| {
            if saved(index) {
                return Vec::new();
            }
            let message = match results.get(index) {
                Some(Ok(Some(saved))) if !dropped_setting_keys(environment, saved).is_empty() => {
                    "This environment does not support this setting. Update T3 Code on it."
                        .to_owned()
                }
                Some(Err(error)) => error.to_string(),
                _ => "The save failed. Try reconnecting and saving again.".to_owned(),
            };
            vec![FailedScopedSettingsEnvironment {
                environment_id: environment.environment_id.clone(),
                label: environment.label.clone(),
                previous: environment.previous.clone(),
                patch: environment.patch.clone(),
                message,
            }]
        })
        .collect::<Vec<_>>();
    let saved_environments: Vec<_> = plan
        .server_writes
        .iter()
        .enumerate()
        .filter(|(index, _)| saved(*index))
        .map(|(_, write)| write.clone())
        .collect();
    PersistScopedSettingsResult {
        saved_environment_count: saved_environments.len(),
        saved_environments,
        failed_environments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::{ProjectId, RepositoryIdentity};
    use crate::state::project_grouping::GroupableProject;
    use crate::state::sidebar_project_grouping::{
        EnvironmentPresence, SidebarProjectGroupMember, SidebarProjectSnapshot,
    };
    use turin_contracts::model_selection::ModelSelection;
    use turin_contracts::provider_instance::ProviderInstanceId;
    use turin_contracts::settings::default_server_settings;
    use turin_shared::server_settings::apply_server_settings_patch;

    use super::super::settings_scope::{
        SettingsScopeEnvironment, resolve_settings_scope, validate_settings_scope_search,
    };

    fn no_fallback(
        _instances: &turin_contracts::provider_instance::ProviderInstanceConfigMap,
        _enabled: &dyn Fn(&ProviderInstanceId) -> bool,
        _default: Option<&ModelSelection>,
    ) -> Option<ModelSelection> {
        None
    }

    #[derive(Clone, Debug, PartialEq)]
    struct Project {
        environment_id: EnvironmentId,
        id: ProjectId,
        workspace_root: String,
    }

    impl GroupableProject for Project {
        fn environment_id(&self) -> &EnvironmentId {
            &self.environment_id
        }
        fn id(&self) -> &ProjectId {
            &self.id
        }
        fn title(&self) -> &str {
            "Project"
        }
        fn workspace_root(&self) -> &str {
            &self.workspace_root
        }
        fn repository_identity(&self) -> Option<&RepositoryIdentity> {
            None
        }
        fn is_scratch(&self) -> bool {
            false
        }
        fn updated_at(&self) -> Option<&str> {
            Some("2026-09-07T00:00:00.000Z")
        }
        fn created_at(&self) -> Option<&str> {
            Some("2026-09-07T00:00:00.000Z")
        }
    }

    type Member = SidebarProjectGroupMember<Project>;

    fn env_id(id: &str) -> EnvironmentId {
        EnvironmentId::new(id).expect("id")
    }

    fn environment(id: &str, options: EnvironmentOptions) -> ScopedSettingsEnvironment {
        ScopedSettingsEnvironment {
            environment_id: env_id(id),
            label: id.to_owned(),
            connection_phase: if options.connected {
                EnvironmentConnectionPhase::Connected
            } else {
                EnvironmentConnectionPhase::Offline
            },
            server_config: options.loaded.then(|| {
                let mut settings = default_server_settings();
                if let Some(partial) = options.settings {
                    settings = merge_settings(settings, Value::Object(partial));
                }
                if let Some(overrides) = options.settings_overrides {
                    settings = overrides;
                }
                ScopedSettingsServerConfig {
                    settings,
                    project_settings_overrides_capable: options.project_overrides,
                }
            }),
        }
    }

    struct EnvironmentOptions {
        connected: bool,
        loaded: bool,
        settings_overrides: Option<ServerSettings>,
        settings: Option<Map<String, Value>>,
        project_overrides: bool,
    }

    impl Default for EnvironmentOptions {
        fn default() -> Self {
            Self {
                connected: true,
                loaded: true,
                settings_overrides: None,
                settings: None,
                project_overrides: true,
            }
        }
    }

    fn env_with_settings(id: &str, settings: ServerSettings) -> ScopedSettingsEnvironment {
        environment(
            id,
            EnvironmentOptions {
                settings_overrides: Some(settings),
                ..Default::default()
            },
        )
    }

    fn merge_settings(base: ServerSettings, patch: Value) -> ServerSettings {
        let mut value = serde_json::to_value(base).unwrap();
        if let (Some(base_object), Some(patch_object)) = (value.as_object_mut(), patch.as_object())
        {
            for (key, value) in patch_object {
                base_object.insert(key.clone(), value.clone());
            }
        }
        serde_json::from_value(value).unwrap()
    }

    fn laptop() -> ScopedSettingsEnvironment {
        environment("Laptop", Default::default())
    }
    fn server() -> ScopedSettingsEnvironment {
        environment("Server", Default::default())
    }
    fn offline() -> ScopedSettingsEnvironment {
        environment(
            "Offline",
            EnvironmentOptions {
                connected: false,
                ..Default::default()
            },
        )
    }
    fn loading() -> ScopedSettingsEnvironment {
        environment(
            "Loading",
            EnvironmentOptions {
                loaded: false,
                ..Default::default()
            },
        )
    }

    fn environments() -> Vec<ScopedSettingsEnvironment> {
        vec![laptop(), server(), offline(), loading()]
    }

    fn all_scope() -> ResolvedSettingsScope<Project> {
        resolve_settings_scope(
            &validate_settings_scope_search(None, None, None),
            &[],
            &scope_environments(),
        )
    }

    fn named_scope() -> ResolvedSettingsScope<Project> {
        resolve_settings_scope(
            &validate_settings_scope_search(None, Some("Server"), None),
            &[],
            &scope_environments(),
        )
    }

    fn scope_environments() -> Vec<SettingsScopeEnvironment> {
        environments()
            .iter()
            .map(|environment| SettingsScopeEnvironment {
                environment_id: environment.environment_id.clone(),
                label: environment.label.clone(),
            })
            .collect()
    }

    fn project_id(value: &str) -> ProjectId {
        ProjectId::new(value).expect("id")
    }

    fn fixture_group() -> (
        ResolvedSettingsScope<Project>,
        ResolvedSettingsScope<Project>,
        Member,
        Member,
        SidebarProjectSnapshot<Project>,
    ) {
        let project = project_id("project");
        let laptop_project = project_id("laptop-project");
        let server_env = env_id("Server");
        let laptop_env = env_id("Laptop");
        let member = Member {
            project: Project {
                id: project.clone(),
                environment_id: server_env.clone(),
                workspace_root: "/repo".to_owned(),
            },
            physical_project_key: format!("{server_env}:/repo"),
            environment_label: Some("Server".to_owned()),
        };
        let laptop_member = Member {
            project: Project {
                id: laptop_project.clone(),
                environment_id: laptop_env.clone(),
                workspace_root: "/repo".to_owned(),
            },
            physical_project_key: format!("{laptop_env}:/repo"),
            environment_label: Some("Laptop".to_owned()),
        };
        let group = SidebarProjectSnapshot {
            representative: member.project.clone(),
            project_key: "project-group".to_owned(),
            display_name: "Project".to_owned(),
            grouped_project_count: 2,
            environment_presence: EnvironmentPresence::RemoteOnly,
            member_projects: vec![member.clone(), laptop_member.clone()],
            member_project_refs: vec![],
            remote_environment_labels: vec!["Server".to_owned(), "Laptop".to_owned()],
        };
        let project_scope = resolve_settings_scope(
            &validate_settings_scope_search(Some("project-group"), None, None),
            std::slice::from_ref(&group),
            &scope_environments(),
        );
        let checkout_scope = resolve_settings_scope(
            &validate_settings_scope_search(
                Some("project-group"),
                Some("Server"),
                Some(member.physical_project_key.as_str()),
            ),
            std::slice::from_ref(&group),
            &scope_environments(),
        );
        (project_scope, checkout_scope, member, laptop_member, group)
    }

    #[test]
    fn uses_the_named_environment_even_when_a_different_primary_is_available() {
        let scope = named_scope();
        let available = environments();
        let selected =
            select_scoped_settings_environments(&scope, &available, Some(&env_id("Laptop")));
        assert_eq!(selected.environments.len(), 1);
        assert_eq!(selected.environments[0].environment_id, env_id("Server"));
        assert_eq!(
            selected
                .environment
                .map(|environment| environment.environment_id.clone()),
            Some(env_id("Server"))
        );
    }

    #[test]
    fn keeps_an_offline_named_environment_selected_without_falling_back_to_primary() {
        let scope: ResolvedSettingsScope<Project> = resolve_settings_scope(
            &validate_settings_scope_search(None, Some("Offline"), None),
            &[],
            &scope_environments(),
        );
        let available = environments();
        let selected =
            select_scoped_settings_environments(&scope, &available, Some(&env_id("Laptop")));
        assert_eq!(selected.environments.len(), 1);
        assert_eq!(selected.environments[0].label, "Offline");
        assert!(selected.connected_environments.is_empty());
        assert!(selected.environment.is_none());
        let plan = plan_scoped_settings_patch(
            &scope,
            &available,
            &json!({ "enableProviderUpdateChecks": false }),
            &no_fallback,
        );
        assert!(plan.server_writes.is_empty());
        assert_eq!(
            plan.unavailable_reason.as_deref(),
            Some("Connect Offline to save this setting.")
        );
    }

    #[test]
    fn prefers_the_selected_primary_as_the_aggregate_representative_without_including_disconnected_targets()
     {
        let scope = all_scope();
        let available = environments();
        let selected =
            select_scoped_settings_environments(&scope, &available, Some(&env_id("Server")));
        assert_eq!(
            selected
                .environment
                .map(|environment| environment.label.clone()),
            Some("Server".to_owned())
        );
        assert_eq!(selected.environments.len(), 4);
        assert_eq!(selected.connected_environments.len(), 2);
    }

    #[test]
    fn resolves_each_members_effective_settings_and_source_at_project_scope() {
        let (project_scope, _, member, laptop_member, _) = fixture_group();
        let project = member.project.id().clone();
        let laptop_project = laptop_member.project.id().clone();
        let overridden = environment(
            "Server",
            EnvironmentOptions {
                settings_overrides: Some(merge_settings(
                    default_server_settings(),
                    json!({
                        "defaultAutoPull": false,
                        "projectSettingsOverrides": {
                            project.as_str(): { "defaultAutoPull": true }
                        }
                    }),
                )),
                ..Default::default()
            },
        );
        let targets =
            resolve_scoped_settings_targets(&project_scope, &[laptop(), overridden], &no_fallback);
        assert_eq!(
            targets
                .iter()
                .map(|target| (target.project_id.clone(), target.settings.default_auto_pull))
                .collect::<Vec<_>>(),
            vec![(Some(project), true), (Some(laptop_project), false)]
        );
        assert_eq!(
            scoped_settings_source(&targets, &["defaultAutoPull"]),
            ScopedSettingSource::Mixed
        );
        assert_eq!(
            scoped_settings_source(std::slice::from_ref(&targets[0]), &["defaultAutoPull"]),
            ScopedSettingSource::Project
        );
        assert_eq!(
            scoped_settings_source(&targets, &["enableProviderUpdateChecks"]),
            ScopedSettingSource::Environment
        );
        assert!(scoped_settings_are_mixed(&targets, &["defaultAutoPull"]));
    }

    #[test]
    fn edits_the_effective_machine_policy_without_changing_other_machines_rules() {
        let custom = environment(
            "Laptop",
            EnvironmentOptions {
                settings_overrides: Some(merge_settings(
                    default_server_settings(),
                    json!({
                        "worktreeCleanup": {
                            "mode": "custom",
                            "rules": {
                                "worktreeAfterDays": 12,
                                "worktreeOnDelete": true,
                                "worktreeOnMerge": true,
                                "worktreeUnchanged": false,
                                "worktreeKeepWhen": "uncommitted-changes"
                            }
                        }
                    }),
                )),
                ..Default::default()
            },
        );
        let disabled = environment(
            "Server",
            EnvironmentOptions {
                settings_overrides: Some(merge_settings(
                    default_server_settings(),
                    json!({ "worktreeCleanup": { "mode": "off" } }),
                )),
                ..Default::default()
            },
        );
        let targets = vec![custom, disabled];
        let all = all_scope();
        let plan = plan_scoped_settings_patch(
            &all,
            &targets,
            &json!({ "storageCleanup": { "worktreeOnDelete": false } }),
            &no_fallback,
        );
        let policies: Vec<_> = plan
            .server_writes
            .iter()
            .zip(targets.iter())
            .map(|(write, target)| {
                resolve_worktree_cleanup(
                    &apply_server_settings_patch(
                        &target.server_config.as_ref().unwrap().settings,
                        &write.patch,
                    )
                    .expect("patch"),
                    None,
                    &no_fallback,
                )
            })
            .collect();
        assert_eq!(
            serde_json::to_value(policies).unwrap(),
            json!([
                {
                    "worktreeAfterDays": 12,
                    "worktreeOnDelete": false,
                    "worktreeOnMerge": true,
                    "worktreeUnchanged": false,
                    "worktreeKeepWhen": "uncommitted-changes"
                },
                {
                    "worktreeAfterDays": null,
                    "worktreeOnDelete": false,
                    "worktreeOnMerge": false,
                    "worktreeUnchanged": false,
                    "worktreeKeepWhen": "uncommitted-changes"
                }
            ])
        );
    }

    #[tokio::test]
    async fn isolates_a_formerly_shared_server_preference_to_the_named_environment() {
        let mut server_calls = Vec::new();
        let named = named_scope();
        let plan = plan_scoped_settings_patch(
            &named,
            &environments(),
            &json!({ "sidebarAutoSettleOnMerge": false }),
            &no_fallback,
        );
        persist_scoped_settings_patch(
            &plan,
            |environment_id, patch| {
                server_calls.push((environment_id, patch));
                async { Ok(None) }
            },
            |_patch| {},
        )
        .await;
        assert_eq!(server_calls.len(), 1);
        assert_eq!(server_calls[0].0, env_id("Server"));
        assert_eq!(
            serde_json::to_value(&server_calls[0].1).unwrap(),
            json!({ "sidebarAutoSettleOnMerge": false })
        );
    }

    #[tokio::test]
    async fn writes_an_aggregate_preference_only_to_connected_environments_with_loaded_configuration()
     {
        let mut server_calls = Vec::new();
        let all = all_scope();
        let plan = plan_scoped_settings_patch(
            &all,
            &environments(),
            &json!({ "enableProviderUpdateChecks": false }),
            &no_fallback,
        );
        persist_scoped_settings_patch(
            &plan,
            |environment_id, _patch| {
                server_calls.push(environment_id);
                async { Ok(None) }
            },
            |_patch| {},
        )
        .await;
        assert_eq!(server_calls, vec![env_id("Laptop"), env_id("Server")]);
    }

    mod dropped_keys {
        use std::pin::Pin;

        use super::*;

        async fn outcome(
            plan: ScopedSettingsPlan,
            persist: impl Fn(
                EnvironmentId,
                ServerSettingsPatch,
            ) -> Pin<Box<dyn Future<Output = PersistServerResult> + Send>>
            + Send
            + Sync,
        ) -> Value {
            let result = persist_scoped_settings_patch(&plan, persist, |_| {}).await;
            json!({
                "saved": result.saved_environments.iter().map(|write| write.label.clone()).collect::<Vec<_>>(),
                "failed": result.failed_environments.iter().map(|write| json!({ "label": write.label, "message": write.message })).collect::<Vec<_>>(),
            })
        }

        fn upgraded_laptop(
            saved: Value,
        ) -> impl Fn(
            EnvironmentId,
            ServerSettingsPatch,
        ) -> Pin<Box<dyn Future<Output = PersistServerResult> + Send>>
        + Send
        + Sync {
            move |environment_id, _patch| {
                let saved = saved.clone();
                Box::pin(async move {
                    Ok(Some(merge_settings(
                        default_server_settings(),
                        if environment_id == env_id("Laptop") {
                            saved
                        } else {
                            json!({})
                        },
                    )))
                })
            }
        }

        #[tokio::test]
        async fn reports_a_dropped_toggle() {
            let all = all_scope();
            assert_eq!(
                outcome(
                    plan_scoped_settings_patch(
                        &all,
                        &environments(),
                        &json!({ "reviewGuidesEnabled": true }),
                        &no_fallback,
                    ),
                    upgraded_laptop(json!({ "reviewGuidesEnabled": true })),
                )
                .await,
                json!({
                    "saved": ["Laptop"],
                    "failed": [{
                        "label": "Server",
                        "message": "This environment does not support this setting. Update T3 Code on it."
                    }]
                })
            );
        }

        #[tokio::test]
        async fn reports_dropped_text_but_accepts_the_server_trimming_it() {
            let all = all_scope();
            assert_eq!(
                outcome(
                    plan_scoped_settings_patch(
                        &all,
                        &environments(),
                        &json!({ "reviewGuideInstructions": " Read tests " }),
                        &no_fallback,
                    ),
                    upgraded_laptop(json!({ "reviewGuideInstructions": "Read tests" })),
                )
                .await,
                json!({
                    "saved": ["Laptop"],
                    "failed": [{
                        "label": "Server",
                        "message": "This environment does not support this setting. Update T3 Code on it."
                    }]
                })
            );
        }

        #[tokio::test]
        async fn reports_a_dropped_key_inside_a_project_override() {
            let (project_scope, _, _, laptop_member, _) = fixture_group();
            let laptop_project_id = laptop_member.project.id().clone();
            assert_eq!(
                outcome(
                    plan_scoped_settings_patch(
                        &project_scope,
                        &environments(),
                        &json!({ "defaultAutoPull": true }),
                        &no_fallback,
                    ),
                    upgraded_laptop(json!({
                        "projectSettingsOverrides": {
                            laptop_project_id.as_str(): { "defaultAutoPull": true }
                        }
                    })),
                )
                .await,
                json!({
                    "saved": ["Laptop"],
                    "failed": [{
                        "label": "Server",
                        "message": "This environment does not support this setting. Update T3 Code on it."
                    }]
                })
            );
        }

        #[tokio::test]
        async fn reports_a_nested_key_dropped_beside_one_that_saved() {
            let (project_scope, _, member, laptop_member, _) = fixture_group();
            let patch = json!({ "defaultAutoPull": true, "enableAgentBrowserAccess": false });
            let laptop_project_id = laptop_member.project.id().clone();
            let server_project_id = member.project.id().clone();
            let saved_laptop_patch = patch.clone();
            assert_eq!(
                outcome(
                    plan_scoped_settings_patch(
                        &project_scope,
                        &environments(),
                        &patch,
                        &no_fallback
                    ),
                    move |environment_id, _patch| {
                        let laptop_project_id = laptop_project_id.clone();
                        let server_project_id = server_project_id.clone();
                        let saved_laptop_patch = saved_laptop_patch.clone();
                        Box::pin(async move {
                            Ok(Some(merge_settings(
                                default_server_settings(),
                                if environment_id == env_id("Laptop") {
                                    json!({
                                        "projectSettingsOverrides": {
                                            laptop_project_id.as_str(): saved_laptop_patch
                                        }
                                    })
                                } else {
                                    json!({
                                        "projectSettingsOverrides": {
                                            server_project_id.as_str(): { "defaultAutoPull": true }
                                        }
                                    })
                                },
                            )))
                        })
                    },
                )
                .await,
                json!({
                    "saved": ["Laptop"],
                    "failed": [{
                        "label": "Server",
                        "message": "This environment does not support this setting. Update T3 Code on it."
                    }]
                })
            );
        }

        #[tokio::test]
        async fn accepts_a_write_that_matches_what_the_environment_already_had() {
            let all = all_scope();
            assert_eq!(
                outcome(
                    plan_scoped_settings_patch(
                        &all,
                        &environments(),
                        &json!({ "reviewGuidesEnabled": false }),
                        &no_fallback,
                    ),
                    upgraded_laptop(json!({})),
                )
                .await,
                json!({ "saved": ["Laptop", "Server"], "failed": [] })
            );
        }
    }

    #[tokio::test]
    async fn persists_client_keys_locally_at_any_scope_alongside_server_keys() {
        let mut client_patch = None;
        let mut server_calls = Vec::new();
        let named = named_scope();
        let plan = plan_scoped_settings_patch(
            &named,
            &environments(),
            &json!({
                "diffIgnoreWhitespace": false,
                "enableProviderUpdateChecks": false
            }),
            &no_fallback,
        );
        persist_scoped_settings_patch(
            &plan,
            |environment_id, patch| {
                server_calls.push((environment_id, patch));
                async { Ok(None) }
            },
            |patch| client_patch = Some(patch),
        )
        .await;
        assert_eq!(client_patch, Some(json!({ "diffIgnoreWhitespace": false })));
        assert_eq!(server_calls.len(), 1);
        assert_eq!(server_calls[0].0, env_id("Server"));
        assert_eq!(
            serde_json::to_value(&server_calls[0].1).unwrap(),
            json!({ "enableProviderUpdateChecks": false })
        );
    }

    #[test]
    fn writes_project_overrides_into_each_members_entry_on_its_environment() {
        let (project_scope, checkout_scope, member, laptop_member, _) = fixture_group();
        let with_existing = environment(
            "Server",
            EnvironmentOptions {
                settings_overrides: Some(merge_settings(
                    default_server_settings(),
                    json!({
                        "projectSettingsOverrides": {
                            member.project.id().as_str(): { "enableAgentBrowserAccess": false }
                        }
                    }),
                )),
                ..Default::default()
            },
        );
        let plan = plan_scoped_settings_patch(
            &project_scope,
            &[laptop(), with_existing],
            &json!({ "defaultAutoPull": true }),
            &no_fallback,
        );
        assert!(plan.unavailable_reason.is_none());
        assert_eq!(plan.server_writes.len(), 2);
        assert_eq!(plan.server_writes[0].environment_id, env_id("Server"));
        assert_eq!(
            serde_json::to_value(&plan.server_writes[0].patch).unwrap(),
            json!({
                "projectSettingsOverrides": {
                    member.project.id().as_str(): {
                        "enableAgentBrowserAccess": false,
                        "defaultAutoPull": true
                    }
                }
            })
        );
        assert_eq!(plan.server_writes[1].environment_id, env_id("Laptop"));
        assert_eq!(
            serde_json::to_value(&plan.server_writes[1].patch).unwrap(),
            json!({
                "projectSettingsOverrides": {
                    laptop_member.project.id().as_str(): { "defaultAutoPull": true }
                }
            })
        );
        let checkout_plan = plan_scoped_settings_patch(
            &checkout_scope,
            &[laptop(), server()],
            &json!({ "defaultAutoPull": true }),
            &no_fallback,
        );
        assert_eq!(checkout_plan.server_writes.len(), 1);
        assert_eq!(
            checkout_plan.server_writes[0].environment_id,
            env_id("Server")
        );
    }

    #[test]
    fn keeps_each_projects_other_cleanup_rules_when_changing_one_rule_across_machines() {
        let (project_scope, checkout_scope, member, laptop_member, _) = fixture_group();
        let machine = environment(
            "Laptop",
            EnvironmentOptions {
                settings_overrides: Some(merge_settings(
                    default_server_settings(),
                    json!({
                        "storageCleanup": {
                            "worktreeAfterDays": 30
                        }
                    }),
                )),
                ..Default::default()
            },
        );
        let customized = environment(
            "Server",
            EnvironmentOptions {
                settings_overrides: Some(merge_settings(
                    default_server_settings(),
                    json!({
                        "projectSettingsOverrides": {
                            member.project.id().as_str(): {
                                "defaultAutoPull": true,
                                "worktreeCleanup": {
                                    "mode": "custom",
                                    "rules": {
                                        "worktreeAfterDays": 8,
                                        "worktreeOnDelete": false,
                                        "worktreeOnMerge": true,
                                        "worktreeUnchanged": false,
                                        "worktreeKeepWhen": "uncommitted-changes"
                                    }
                                }
                            }
                        }
                    }),
                )),
                ..Default::default()
            },
        );
        let plan = plan_scoped_settings_patch(
            &project_scope,
            &[machine.clone(), customized.clone()],
            &json!({
                "worktreeCleanup": { "mode": "custom", "rules": { "worktreeOnDelete": true } }
            }),
            &no_fallback,
        );
        assert_eq!(
            plan.server_writes
                .iter()
                .map(|write| write.patch.project_settings_overrides.clone().unwrap())
                .collect::<Vec<_>>(),
            vec![
                IndexMap::from([(
                    member.project.id().clone(),
                    serde_json::from_value(json!({
                        "defaultAutoPull": true,
                        "worktreeCleanup": {
                            "mode": "custom",
                            "rules": {
                                "worktreeAfterDays": 8,
                                "worktreeOnDelete": true,
                                "worktreeOnMerge": true,
                                "worktreeUnchanged": false,
                                "worktreeKeepWhen": "uncommitted-changes"
                            }
                        }
                    }))
                    .unwrap()
                )]),
                IndexMap::from([(
                    laptop_member.project.id().clone(),
                    serde_json::from_value(json!({
                        "worktreeCleanup": {
                            "mode": "custom",
                            "rules": {
                                "worktreeAfterDays": 30,
                                "worktreeOnDelete": true,
                                "worktreeOnMerge": false,
                                "worktreeUnchanged": false,
                                "worktreeKeepWhen": "uncommitted-changes"
                            }
                        }
                    }))
                    .unwrap()
                )]),
            ]
        );
        assert_eq!(
            serde_json::to_value(
                &plan_scoped_settings_clear(
                    &checkout_scope,
                    std::slice::from_ref(&customized),
                    &["worktreeCleanup"]
                )
                .server_writes[0]
                    .patch
            )
            .unwrap(),
            json!({
                "projectSettingsOverrides": {
                    member.project.id().as_str(): { "defaultAutoPull": true }
                }
            })
        );
        assert!(
            plan_scoped_settings_patch(
                &project_scope,
                &[machine, customized],
                &json!({ "storageCleanup": { "browserArtifactsAfterDays": 8 } }),
                &no_fallback,
            )
            .server_writes
            .is_empty()
        );
    }

    #[test]
    fn scopes_agent_device_access_to_projects_while_keeping_hub_and_hosts_environment_wide() {
        let (project_scope, _, member, laptop_member, _) = fixture_group();
        let plan = plan_scoped_settings_patch(
            &project_scope,
            &[laptop(), server()],
            &json!({ "enableAgentDeviceAccess": true }),
            &no_fallback,
        );
        assert_eq!(
            plan.server_writes
                .iter()
                .map(|write| serde_json::to_value(&write.patch).unwrap())
                .collect::<Vec<_>>(),
            vec![
                json!({
                    "projectSettingsOverrides": {
                        member.project.id().as_str(): { "enableAgentDeviceAccess": true }
                    }
                }),
                json!({
                    "projectSettingsOverrides": {
                        laptop_member.project.id().as_str(): { "enableAgentDeviceAccess": true }
                    }
                }),
            ]
        );
        for patch in [
            json!({ "enableDeviceSupport": true }),
            json!({ "deviceHosts": [] }),
        ] {
            assert!(
                plan_scoped_settings_patch(&project_scope, &environments(), &patch, &no_fallback,)
                    .server_writes
                    .is_empty()
            );
        }
    }

    #[test]
    fn refuses_environment_wide_keys_and_older_servers_at_project_scope() {
        let (project_scope, checkout_scope, _, _, _) = fixture_group();
        let plan = plan_scoped_settings_patch(
            &project_scope,
            &environments(),
            &json!({ "enableProviderUpdateChecks": false }),
            &no_fallback,
        );
        assert!(plan.server_writes.is_empty());
        assert_eq!(
            plan.unavailable_reason.as_deref(),
            Some("This setting is environment-wide and cannot be overridden by a project.")
        );
        let legacy = environment(
            "Server",
            EnvironmentOptions {
                project_overrides: false,
                ..Default::default()
            },
        );
        let checkout_plan = plan_scoped_settings_patch(
            &checkout_scope,
            &[laptop(), legacy],
            &json!({ "defaultAutoPull": true }),
            &no_fallback,
        );
        assert!(checkout_plan.server_writes.is_empty());
        assert!(
            checkout_plan
                .unavailable_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("update"))
        );
    }

    #[test]
    fn clears_overrides_per_member_and_removes_an_emptied_entry() {
        let (_project_scope, checkout_scope, member, _, _) = fixture_group();
        let with_overrides = environment(
            "Server",
            EnvironmentOptions {
                settings_overrides: Some(merge_settings(
                    default_server_settings(),
                    json!({
                        "projectSettingsOverrides": {
                            member.project.id().as_str(): {
                                "defaultAutoPull": true,
                                "enableAgentBrowserAccess": false
                            }
                        }
                    }),
                )),
                ..Default::default()
            },
        );
        let plan = plan_scoped_settings_clear(
            &checkout_scope,
            &[laptop(), with_overrides.clone()],
            &["defaultAutoPull"],
        );
        assert_eq!(plan.server_writes.len(), 1);
        assert_eq!(
            serde_json::to_value(&plan.server_writes[0].patch).unwrap(),
            json!({
                "projectSettingsOverrides": {
                    member.project.id().as_str(): { "enableAgentBrowserAccess": false }
                }
            })
        );
        assert_eq!(
            serde_json::to_value(
                &plan_scoped_settings_clear(
                    &checkout_scope,
                    &[laptop(), with_overrides],
                    &["defaultAutoPull", "enableAgentBrowserAccess"]
                )
                .server_writes[0]
                    .patch
            )
            .unwrap(),
            json!({ "projectSettingsOverrides": { member.project.id().as_str(): null } })
        );
    }

    #[test]
    fn never_substitutes_an_environment_default_write_for_an_invalid_scope() {
        let (_, _, _, _, group) = fixture_group();
        let scope: ResolvedSettingsScope<Project> = resolve_settings_scope(
            &validate_settings_scope_search(None, Some("removed"), None),
            std::slice::from_ref(&group),
            &scope_environments(),
        );
        let plan = plan_scoped_settings_patch(
            &scope,
            &environments(),
            &json!({ "enableAgentBrowserAccess": false }),
            &no_fallback,
        );
        assert!(plan.server_writes.is_empty());
        assert!(!plan.has_client_write);
        assert!(plan.unavailable_reason.is_some());
    }

    #[tokio::test]
    async fn waits_for_every_target_and_identifies_both_rpc_failures_and_rejected_writes() {
        let third = environment("Third", Default::default());
        let fourth = environment("Fourth", Default::default());
        let selected = [environments(), vec![third, fourth]].concat();
        let scope: ResolvedSettingsScope<Project> = resolve_settings_scope(
            &validate_settings_scope_search(None, None, None),
            &[],
            &selected
                .iter()
                .map(|environment| SettingsScopeEnvironment {
                    environment_id: environment.environment_id.clone(),
                    label: environment.label.clone(),
                })
                .collect::<Vec<_>>(),
        );
        let plan = plan_scoped_settings_patch(
            &scope,
            &selected,
            &json!({ "enableAgentBrowserAccess": false }),
            &no_fallback,
        );
        use std::cell::Cell;
        let call = Cell::new(0);
        let result = persist_scoped_settings_patch(
            &plan,
            |_environment_id, _patch| {
                call.set(call.get() + 1);
                let attempt = call.get();
                async move {
                    match attempt {
                        1 => Ok(None),
                        2 => Err("This connection lacks permission to change settings.".into()),
                        3 => Err("Disconnected during save".into()),
                        _ => Ok(None),
                    }
                }
            },
            |_| {},
        )
        .await;
        assert_eq!(result.saved_environment_count, 2);
        assert_eq!(
            result
                .saved_environments
                .iter()
                .map(|write| write.label.clone())
                .collect::<Vec<_>>(),
            vec!["Laptop".to_owned(), "Fourth".to_owned()]
        );
        assert_eq!(
            result
                .failed_environments
                .iter()
                .map(|write| write.message.clone())
                .collect::<Vec<_>>(),
            vec![
                "This connection lacks permission to change settings.".to_owned(),
                "Disconnected during save".to_owned(),
            ]
        );
        assert_eq!(
            result
                .failed_environments
                .iter()
                .map(|write| write.label.clone())
                .collect::<Vec<_>>(),
            vec!["Server".to_owned(), "Third".to_owned()]
        );
        assert_eq!(call.get(), 4);
    }

    #[test]
    fn compares_only_requested_settings_across_connected_targets() {
        let changed = environment(
            "Changed",
            EnvironmentOptions {
                settings_overrides: Some(merge_settings(
                    default_server_settings(),
                    json!({ "enableAgentBrowserAccess": false }),
                )),
                ..Default::default()
            },
        );
        let all = all_scope();
        let targets = resolve_scoped_settings_targets(&all, &[laptop(), changed], &no_fallback);
        assert!(scoped_settings_are_mixed(
            &targets,
            &["enableAgentBrowserAccess"]
        ));
        assert!(!scoped_settings_are_mixed(
            &targets,
            &["enableProviderUpdateChecks"]
        ));
        assert!(!scoped_settings_are_mixed(
            &[],
            &["enableAgentBrowserAccess"]
        ));
    }

    #[test]
    fn treats_independently_decoded_equal_nested_settings_as_the_same_value() {
        let style = merge_settings(
            default_server_settings(),
            json!({
                "sourceControlWritingStyle": {
                    "mode": "custom",
                    "customInstructions": "Use plain language"
                }
            }),
        )
        .source_control_writing_style;
        let first = environment(
            "First",
            EnvironmentOptions {
                settings_overrides: Some(merge_settings(
                    default_server_settings(),
                    json!({ "sourceControlWritingStyle": style.clone() }),
                )),
                ..Default::default()
            },
        );
        let second = environment(
            "Second",
            EnvironmentOptions {
                settings_overrides: Some(merge_settings(
                    default_server_settings(),
                    json!({ "sourceControlWritingStyle": style }),
                )),
                ..Default::default()
            },
        );
        let all = all_scope();
        let targets = resolve_scoped_settings_targets(&all, &[first, second], &no_fallback);
        assert!(!scoped_settings_are_mixed(
            &targets,
            &["sourceControlWritingStyle"]
        ));
    }

    #[test]
    fn lists_only_the_projects_that_override_the_keys() {
        let laptop = env_id("laptop");
        let desk = env_id("desk");
        let fleet = project_id("fleet");
        let t3 = project_id("t3");
        let environments = vec![
            env_with_settings(
                "laptop",
                merge_settings(
                    default_server_settings(),
                    json!({
                        "projectSettingsOverrides": {
                            fleet.as_str(): { "defaultAutoPull": true, "defaultThreadEnvMode": "local" },
                            t3.as_str(): { "defaultThreadEnvMode": "local" }
                        }
                    }),
                ),
            ),
            env_with_settings(
                "desk",
                merge_settings(
                    default_server_settings(),
                    json!({
                        "projectSettingsOverrides": {
                            fleet.as_str(): { "defaultAutoPull": false }
                        }
                    }),
                ),
            ),
        ];
        let entries = list_project_overrides(&environments, &["defaultAutoPull"]);
        assert_eq!(
            entries,
            vec![
                ProjectOverrideEntry {
                    environment_id: laptop,
                    project_id: fleet.clone(),
                },
                ProjectOverrideEntry {
                    environment_id: desk,
                    project_id: fleet,
                },
            ]
        );
    }

    #[test]
    fn clears_only_those_keys_and_drops_entries_that_become_empty() {
        let laptop = env_id("laptop");
        let fleet = project_id("fleet");
        let t3 = project_id("t3");
        let environment = env_with_settings(
            "laptop",
            merge_settings(
                default_server_settings(),
                json!({
                    "projectSettingsOverrides": {
                        fleet.as_str(): { "defaultAutoPull": true, "defaultThreadEnvMode": "local" },
                        t3.as_str(): { "defaultAutoPull": true }
                    }
                }),
            ),
        );
        let plan = plan_project_overrides_clear(
            std::slice::from_ref(&environment),
            &[
                ProjectOverrideEntry {
                    environment_id: laptop.clone(),
                    project_id: fleet.clone(),
                },
                ProjectOverrideEntry {
                    environment_id: laptop.clone(),
                    project_id: t3.clone(),
                },
            ],
            &["defaultAutoPull"],
        );
        assert_eq!(plan.server_writes.len(), 1);
        assert_eq!(
            serde_json::to_value(&plan.server_writes[0].patch).unwrap(),
            json!({
                "projectSettingsOverrides": {
                    fleet.as_str(): { "defaultThreadEnvMode": "local" },
                    t3.as_str(): null
                }
            })
        );
    }

    #[test]
    fn removes_the_override_for_keys_that_cannot_store_null_and_keeps_it_for_keys_that_can() {
        let environment_id = env_id("laptop");
        let project = project_id("fleet");
        let scope = ResolvedSettingsScope {
            label: "fleet".to_owned(),
            members: vec![Member {
                project: Project {
                    id: project.clone(),
                    environment_id: environment_id.clone(),
                    workspace_root: "/repo".to_owned(),
                },
                physical_project_key: "laptop:/repo".to_owned(),
                environment_label: Some("Laptop".to_owned()),
            }],
            environment_ids: vec![environment_id.clone()],
            kind: SettingsScopeKind::Project {
                group: SidebarProjectSnapshot {
                    representative: Project {
                        id: project.clone(),
                        environment_id: environment_id.clone(),
                        workspace_root: "/repo".to_owned(),
                    },
                    project_key: "fleet".to_owned(),
                    display_name: "fleet".to_owned(),
                    grouped_project_count: 1,
                    environment_presence: EnvironmentPresence::LocalOnly,
                    member_projects: vec![],
                    member_project_refs: vec![],
                    remote_environment_labels: vec![],
                },
                environment_id: None,
            },
        };
        let environments = vec![environment(
            "laptop",
            EnvironmentOptions {
                settings_overrides: Some(merge_settings(
                    default_server_settings(),
                    json!({
                        "projectSettingsOverrides": {
                            project.as_str(): {
                                "defaultThreadEnvMode": "worktree",
                                "defaultAutoPull": true
                            }
                        }
                    }),
                )),
                ..Default::default()
            },
        )];
        assert_eq!(
            serde_json::to_value(
                &plan_scoped_settings_patch(
                    &scope,
                    &environments,
                    &json!({ "defaultThreadEnvMode": null }),
                    &no_fallback,
                )
                .server_writes[0]
                    .patch
            )
            .unwrap(),
            json!({
                "projectSettingsOverrides": {
                    project.as_str(): { "defaultAutoPull": true }
                }
            })
        );
        assert_eq!(
            serde_json::to_value(
                &plan_scoped_settings_patch(
                    &scope,
                    &environments,
                    &json!({ "defaultModelSelection": null }),
                    &no_fallback,
                )
                .server_writes[0]
                    .patch
            )
            .unwrap(),
            json!({
                "projectSettingsOverrides": {
                    project.as_str(): {
                        "defaultThreadEnvMode": "worktree",
                        "defaultAutoPull": true,
                        "defaultModelSelection": null
                    }
                }
            })
        );
    }

    #[test]
    fn completes_a_writing_style_field_patch_from_the_targets_effective_value() {
        let environment_id = env_id("laptop");
        let project = project_id("fleet");
        let member = Member {
            project: Project {
                id: project.clone(),
                environment_id: environment_id.clone(),
                workspace_root: "/repo".to_owned(),
            },
            physical_project_key: "laptop:/repo".to_owned(),
            environment_label: Some("Laptop".to_owned()),
        };
        let settings = merge_settings(
            default_server_settings(),
            json!({
                "projectSettingsOverrides": {
                    project.as_str(): {
                        "sourceControlWritingStyle": {
                            "mode": "custom",
                            "customInstructions": "Keep it short.",
                            "followChangeRequestTemplates": false
                        }
                    }
                }
            }),
        );
        let plan = plan_scoped_settings_patch(
            &ResolvedSettingsScope {
                label: "fleet".to_owned(),
                members: vec![member],
                environment_ids: vec![environment_id.clone()],
                kind: SettingsScopeKind::Project {
                    group: SidebarProjectSnapshot {
                        representative: Project {
                            id: project.clone(),
                            environment_id: environment_id.clone(),
                            workspace_root: "/repo".to_owned(),
                        },
                        project_key: "fleet".to_owned(),
                        display_name: "fleet".to_owned(),
                        grouped_project_count: 1,
                        environment_presence: EnvironmentPresence::LocalOnly,
                        member_projects: vec![],
                        member_project_refs: vec![],
                        remote_environment_labels: vec![],
                    },
                    environment_id: None,
                },
            },
            &[environment(
                "laptop",
                EnvironmentOptions {
                    settings_overrides: Some(settings),
                    ..Default::default()
                },
            )],
            &json!({ "sourceControlWritingStyle": { "customInstructions": "Be terse." } }),
            &no_fallback,
        );
        assert_eq!(
            serde_json::to_value(&plan.server_writes[0].patch).unwrap(),
            json!({
                "projectSettingsOverrides": {
                    project.as_str(): {
                        "sourceControlWritingStyle": {
                            "mode": "custom",
                            "customInstructions": "Be terse.",
                            "followChangeRequestTemplates": false
                        }
                    }
                }
            })
        );
    }

    #[test]
    fn routes_legacy_model_preference_keys_exclusively_to_the_selected_environment() {
        let patch = json!({
            "favorites": [{ "provider": "codex", "model": "sol" }],
            "providerModelPreferences": {}
        });
        let named = named_scope();
        let plan = plan_scoped_settings_patch(&named, &environments(), &patch, &no_fallback);
        assert_eq!(plan.client_patch, json!({}));
        assert!(!plan.has_client_write);
        assert_eq!(plan.server_writes.len(), 1);
        assert_eq!(plan.server_writes[0].environment_id, env_id("Server"));
        assert_eq!(
            serde_json::to_value(&plan.server_writes[0].patch).unwrap(),
            patch
        );
    }
}
