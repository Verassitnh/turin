//! Port of upstream/packages/client-runtime/src/operations/projects.ts
//!
//! The add-project flow's shared logic. Cut with project clone (PORTING cut
//! list): the clone-source pickers, clone URL normalisation, clone folder
//! names and transport choice, publish targets of source-control hosts. Cut
//! with the connection layer: `canCreateProjectInEnvironment` (the single
//! environment is always connected).

use crate::state::contracts::{
    CommandId, EnvironmentId, ModelSelection, ProjectId, ProjectMutation, TrimmedNonEmptyString,
};
use crate::state::models::EnvironmentProject;
use crate::state::projects::{
    ensure_browse_directory_path, find_project_by_path, infer_project_title_from_path,
    is_explicit_relative_project_path, is_unsupported_windows_project_path,
    resolve_project_path_for_dispatch,
};
use crate::state::support::path::new_project_folder_name;

/// The Scratch folder the environment offers threads without a project, if any.
pub fn available_scratch_workspace_root(scratch_workspace_root: Option<&str>) -> Option<String> {
    scratch_workspace_root.map(str::to_owned)
}

pub fn get_add_project_initial_query(base_directory: Option<&str>) -> String {
    let trimmed = crate::state::contracts::js_trim(base_directory.unwrap_or(""));
    if trimmed.is_empty() {
        "~/".to_owned()
    } else {
        ensure_browse_directory_path(trimmed)
    }
}

/// Clone destination proposed for a directory: the directory the user picked
/// plus the repository folder inside it. Without a name the directory is the
/// destination, which is what the raw clone URL flow keeps doing.
pub fn get_clone_destination_path(directory_path: &str, directory_name: Option<&str>) -> String {
    let name = crate::state::contracts::js_trim(directory_name.unwrap_or(""));
    if name.is_empty() {
        return directory_path.to_owned();
    }
    format!("{}{name}", ensure_browse_directory_path(directory_path))
}

/// Where `projects.createNew` will put a project named `name`. The server adds
/// `-2`, `-3`, ... when that folder is taken, so this is a preview.
pub fn get_new_project_path_preview(new_projects_root: &str, name: &str) -> String {
    get_clone_destination_path(new_projects_root, Some(&new_project_folder_name(name)))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveAddProjectPath {
    Ok(String),
    Err(String),
}

pub fn resolve_add_project_path(
    raw_path: &str,
    current_project_cwd: Option<&str>,
    platform: &str,
) -> ResolveAddProjectPath {
    let raw_path = crate::state::contracts::js_trim(raw_path);
    if raw_path.is_empty() {
        return ResolveAddProjectPath::Err("Enter a project path.".to_owned());
    }
    if is_unsupported_windows_project_path(raw_path, platform) {
        return ResolveAddProjectPath::Err(
            "Windows-style paths are only supported on Windows environments.".to_owned(),
        );
    }
    if is_explicit_relative_project_path(raw_path) && current_project_cwd.is_none_or(str::is_empty)
    {
        return ResolveAddProjectPath::Err(
            "Relative paths require an active project in this environment.".to_owned(),
        );
    }
    let path = resolve_project_path_for_dispatch(raw_path, current_project_cwd);
    if path.is_empty() {
        ResolveAddProjectPath::Err("Enter a project path.".to_owned())
    } else {
        ResolveAddProjectPath::Ok(path)
    }
}

pub fn find_existing_add_project<'a>(
    projects: &'a [EnvironmentProject],
    environment_id: &EnvironmentId,
    path: &str,
) -> Option<&'a EnvironmentProject> {
    let in_environment: Vec<&EnvironmentProject> = projects
        .iter()
        .filter(|project| project.environment_id == *environment_id)
        .collect();
    find_project_by_path(&in_environment, path, |project| {
        Some(project.workspace_root.clone())
    })
    .copied()
}

pub fn build_project_create_command(
    command_id: CommandId,
    project_id: ProjectId,
    workspace_root: &str,
) -> Option<ProjectMutation> {
    Some(ProjectMutation::ProjectCreate {
        command_id,
        project_id,
        title: TrimmedNonEmptyString::new(infer_project_title_from_path(workspace_root)).ok()?,
        workspace_root: TrimmedNonEmptyString::new(workspace_root).ok()?,
        create_workspace_root_if_missing: Some(true),
        default_model_selection: Some(None::<ModelSelection>),
        scripts: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(environment: &str, id: &str, root: &str) -> EnvironmentProject {
        EnvironmentProject {
            environment_id: EnvironmentId::new(environment).unwrap(),
            id: ProjectId::new(id).unwrap(),
            title: id.to_owned(),
            workspace_root: root.to_owned(),
            favicon_path: None,
            project_icon: None,
            created_at: None,
            updated_at: None,
            is_scratch: false,
            source: None,
        }
    }

    #[test]
    fn resolves_initial_browse_paths_from_settings() {
        assert_eq!(get_add_project_initial_query(Some("")), "~/");
        assert_eq!(get_add_project_initial_query(None), "~/");
        assert_eq!(get_add_project_initial_query(Some("/work")), "/work/");
        assert_eq!(
            get_add_project_initial_query(Some("C:\\work")),
            "C:\\work\\"
        );
    }

    #[test]
    fn rejects_unsupported_windows_paths_on_non_windows_environments() {
        assert_eq!(
            resolve_add_project_path("C:\\repo", None, "MacIntel"),
            ResolveAddProjectPath::Err(
                "Windows-style paths are only supported on Windows environments.".into()
            )
        );
    }

    #[test]
    fn resolves_relative_paths_from_the_active_project_cwd() {
        assert_eq!(
            resolve_add_project_path("../next", Some("/work/current"), "Linux"),
            ResolveAddProjectPath::Ok("/work/next".into())
        );
        assert_eq!(
            resolve_add_project_path("../next", None, "Linux"),
            ResolveAddProjectPath::Err(
                "Relative paths require an active project in this environment.".into()
            )
        );
        assert_eq!(
            resolve_add_project_path("  ", None, "Linux"),
            ResolveAddProjectPath::Err("Enter a project path.".into())
        );
    }

    #[test]
    fn finds_existing_projects_by_normalized_path_in_the_target_environment() {
        let projects = vec![
            project("other", "same-path-other-env", "/repo"),
            project("env", "project", "/repo/"),
        ];
        let found =
            find_existing_add_project(&projects, &EnvironmentId::new("env").unwrap(), "/repo");
        assert_eq!(found.map(|project| project.id.as_str()), Some("project"));
        assert!(
            find_existing_add_project(&projects, &EnvironmentId::new("env").unwrap(), "/elsewhere")
                .is_none()
        );
    }

    #[test]
    fn builds_the_v2_project_create_mutation() {
        let mutation = build_project_create_command(
            CommandId::new("command").unwrap(),
            ProjectId::new("project").unwrap(),
            "/work/repo",
        )
        .unwrap();
        let ProjectMutation::ProjectCreate {
            command_id,
            project_id,
            title,
            workspace_root,
            create_workspace_root_if_missing,
            default_model_selection,
            ..
        } = mutation
        else {
            panic!("a create mutation");
        };
        assert_eq!(command_id.as_str(), "command");
        assert_eq!(project_id.as_str(), "project");
        assert_eq!(title.as_str(), "repo");
        assert_eq!(workspace_root.as_str(), "/work/repo");
        assert_eq!(create_workspace_root_if_missing, Some(true));
        assert_eq!(default_model_selection, Some(None));
    }

    #[test]
    fn previews_where_a_new_project_will_live() {
        assert_eq!(
            get_new_project_path_preview("/work", "Pinball Stats"),
            "/work/pinball-stats"
        );
        assert_eq!(
            get_new_project_path_preview("/work/", "Pinball Stats"),
            "/work/pinball-stats"
        );
        assert_eq!(get_clone_destination_path("/work", None), "/work");
    }
}
