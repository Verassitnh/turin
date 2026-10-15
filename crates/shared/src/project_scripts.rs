//! Port of upstream/packages/shared/src/projectScripts.ts

use std::collections::BTreeMap;
use turin_contracts::base_schemas::ProjectId;
use turin_contracts::project::ProjectScript;
use turin_contracts::settings::ServerSettings;

pub struct ProjectScriptsProject<'a> {
    pub id: &'a ProjectId,
    pub scripts: &'a [ProjectScript],
}

pub fn resolve_project_scripts<'a>(
    settings: &'a ServerSettings,
    project: ProjectScriptsProject<'a>,
) -> &'a [ProjectScript] {
    if let Some(override_scripts) = settings
        .project_settings_overrides
        .get(project.id)
        .and_then(|override_settings| override_settings.default_project_scripts.as_ref())
    {
        return override_scripts;
    }
    if settings.project_settings_folded {
        return &settings.default_project_scripts;
    }
    match settings.project_script_overrides.get(project.id) {
        Some(Some(scripts)) => scripts,
        Some(None) => &settings.default_project_scripts,
        None if !project.scripts.is_empty() => project.scripts,
        None => &settings.default_project_scripts,
    }
}

pub fn project_scripts_inherit_defaults(
    settings: &ServerSettings,
    project: ProjectScriptsProject<'_>,
) -> bool {
    if settings
        .project_settings_overrides
        .get(project.id)
        .and_then(|override_settings| override_settings.default_project_scripts.as_ref())
        .is_some()
    {
        return false;
    }
    if settings.project_settings_folded {
        return true;
    }
    match settings.project_script_overrides.get(project.id) {
        Some(None) => true,
        None => project.scripts.is_empty(),
        Some(Some(_)) => false,
    }
}

pub fn project_script_cwd<'a>(project_cwd: &'a str, worktree_path: Option<&'a str>) -> &'a str {
    worktree_path.unwrap_or(project_cwd)
}

pub fn project_script_runtime_env(
    project_cwd: &str,
    worktree_path: Option<&str>,
    extra_env: Option<&BTreeMap<String, String>>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([("T3CODE_PROJECT_ROOT".into(), project_cwd.into())]);
    if let Some(path) = worktree_path.filter(|path| !path.is_empty()) {
        env.insert("T3CODE_WORKTREE_PATH".into(), path.into());
    }
    if let Some(extra_env) = extra_env {
        env.extend(
            extra_env
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    env
}

pub fn setup_project_script(scripts: &[ProjectScript]) -> Option<&ProjectScript> {
    scripts.iter().find(|script| script.run_on_worktree_create)
}

pub fn settle_project_script(scripts: &[ProjectScript]) -> Option<&ProjectScript> {
    scripts
        .iter()
        .find(|script| script.run_on_settle == Some(true))
}

pub fn project_script_menu_label(script: &ProjectScript) -> String {
    let mut roles = Vec::new();
    if script.run_on_worktree_create {
        roles.push("setup");
    }
    if script.run_on_settle == Some(true) {
        roles.push("on settle");
    }
    if roles.is_empty() {
        script.name.as_str().to_owned()
    } else {
        format!("{} ({})", script.name, roles.join(", "))
    }
}
