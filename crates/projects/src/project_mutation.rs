//! Port of upstream/apps/server/src/project/ProjectMutation.ts: maps a wire
//! `ProjectMutation` onto the matching `ProjectService` call.

use turin_contracts::project::{
    Project, ProjectCreatePayload, ProjectMutation, ProjectUpdatePayload,
};

use crate::project_service::{
    ProjectCreateInput, ProjectDeleteInput, ProjectServiceApi, ProjectServiceError,
    ProjectUpdateInput,
};

/// Runs one project mutation through the service. Every field the client
/// sent reaches the service unchanged; unset fields stay unset.
pub async fn project_mutation_operation(
    projects: &dyn ProjectServiceApi,
    mutation: ProjectMutation,
) -> Result<Project, ProjectServiceError> {
    match mutation {
        ProjectMutation::ProjectCreate {
            command_id,
            project_id,
            title,
            workspace_root,
            create_workspace_root_if_missing,
            default_model_selection,
            scripts,
        } => {
            projects
                .create(ProjectCreateInput {
                    command_id,
                    project_id,
                    payload: ProjectCreatePayload {
                        title,
                        workspace_root,
                        create_workspace_root_if_missing,
                        default_model_selection,
                        scripts,
                    },
                })
                .await
        }
        ProjectMutation::ProjectUpdate {
            command_id,
            project_id,
            title,
            workspace_root,
            default_model_selection,
            auto_pull,
            project_icon,
            favicon_path,
            default_thread_env_mode,
            scripts,
        } => {
            projects
                .update(ProjectUpdateInput {
                    command_id,
                    project_id,
                    payload: ProjectUpdatePayload {
                        title,
                        workspace_root,
                        default_model_selection,
                        auto_pull,
                        project_icon,
                        favicon_path,
                        default_thread_env_mode,
                        scripts,
                    },
                })
                .await
        }
        ProjectMutation::ProjectDelete {
            command_id,
            project_id,
            force,
        } => {
            projects
                .delete(ProjectDeleteInput {
                    command_id,
                    project_id,
                    force,
                })
                .await
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use parking_lot::Mutex;
    use pretty_assertions::assert_eq;
    use turin_contracts::base_schemas::TrimmedNonEmptyString;
    use turin_contracts::project::{Project, ProjectMutation};
    use turin_contracts::{CommandId, ProjectId};

    use super::project_mutation_operation;
    use crate::project_service::{
        ProjectBootstrap, ProjectCreateInput, ProjectDeleteInput, ProjectOperationError,
        ProjectServiceApi, ProjectServiceError, ProjectUpdateInput,
    };
    use turin_contracts::orchestration_project::OrchestrationProjectShell;
    use turin_contracts::project::ProjectSnapshot;

    #[derive(Debug, PartialEq)]
    enum Call {
        Create(ProjectCreateInput),
        Update(ProjectUpdateInput),
        Delete(ProjectDeleteInput),
    }

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<Call>>,
    }

    fn fixture_project(project_id: &ProjectId) -> Project {
        Project {
            id: project_id.clone(),
            title: TrimmedNonEmptyString::new("Mapping").expect("title"),
            workspace_root: TrimmedNonEmptyString::new("/work/mapping").expect("root"),
            repository_identity: None,
            favicon_path: None,
            project_icon: None,
            default_model_selection: None,
            default_thread_env_mode: None,
            auto_pull: Some(false),
            scripts: Vec::new(),
            created_at: "2026-09-05T00:00:00.000Z".to_owned(),
            updated_at: "2026-09-05T00:00:00.000Z".to_owned(),
            deleted_at: None,
        }
    }

    // Only create, update and delete are reachable through a mutation.
    #[async_trait]
    impl ProjectServiceApi for Recorder {
        async fn create(&self, input: ProjectCreateInput) -> Result<Project, ProjectServiceError> {
            let project = fixture_project(&input.project_id);
            self.calls.lock().push(Call::Create(input));
            Ok(project)
        }

        async fn bootstrap(
            &self,
            _input: ProjectCreateInput,
        ) -> Result<ProjectBootstrap, ProjectServiceError> {
            unreachable!("not reached by a mutation")
        }

        async fn update(&self, input: ProjectUpdateInput) -> Result<Project, ProjectServiceError> {
            let project = fixture_project(&input.project_id);
            self.calls.lock().push(Call::Update(input));
            Ok(project)
        }

        async fn delete(&self, input: ProjectDeleteInput) -> Result<Project, ProjectServiceError> {
            let project = fixture_project(&input.project_id);
            self.calls.lock().push(Call::Delete(input));
            Ok(project)
        }

        async fn get_by_id(
            &self,
            _: &ProjectId,
            _: bool,
        ) -> Result<Option<Project>, ProjectOperationError> {
            unreachable!("not reached by a mutation")
        }

        async fn get_by_workspace_root(
            &self,
            _: &str,
            _: bool,
        ) -> Result<Option<Project>, ProjectOperationError> {
            unreachable!("not reached by a mutation")
        }

        async fn snapshot(&self) -> Result<ProjectSnapshot, ProjectOperationError> {
            unreachable!("not reached by a mutation")
        }

        async fn get_shell(
            &self,
            _: &ProjectId,
        ) -> Result<Option<OrchestrationProjectShell>, ProjectOperationError> {
            unreachable!("not reached by a mutation")
        }

        async fn list_shells(
            &self,
            _: Option<&[ProjectId]>,
        ) -> Result<Vec<OrchestrationProjectShell>, ProjectOperationError> {
            unreachable!("not reached by a mutation")
        }
    }

    #[tokio::test]
    async fn preserves_every_project_mutation_field() {
        let recorder = Arc::new(Recorder::default());
        let project_id = ProjectId::new("project:mutation-mapping").expect("id");
        let command = |value: &str| CommandId::new(value).expect("id");

        project_mutation_operation(
            recorder.as_ref(),
            ProjectMutation::ProjectCreate {
                command_id: command("command:create"),
                project_id: project_id.clone(),
                title: TrimmedNonEmptyString::new("Created").expect("title"),
                workspace_root: TrimmedNonEmptyString::new("/work/created").expect("root"),
                create_workspace_root_if_missing: Some(true),
                default_model_selection: Some(None),
                scripts: Some(Vec::new()),
            },
        )
        .await
        .expect("create");
        project_mutation_operation(
            recorder.as_ref(),
            ProjectMutation::ProjectUpdate {
                command_id: command("command:update"),
                project_id: project_id.clone(),
                title: Some(TrimmedNonEmptyString::new("Updated").expect("title")),
                workspace_root: Some(TrimmedNonEmptyString::new("/work/updated").expect("root")),
                default_model_selection: Some(None),
                auto_pull: Some(false),
                project_icon: Some(None),
                favicon_path: Some(None),
                default_thread_env_mode: Some(None),
                scripts: Some(Vec::new()),
            },
        )
        .await
        .expect("update");
        project_mutation_operation(
            recorder.as_ref(),
            ProjectMutation::ProjectDelete {
                command_id: command("command:delete"),
                project_id: project_id.clone(),
                force: Some(true),
            },
        )
        .await
        .expect("delete");

        let calls = recorder.calls.lock();
        assert_eq!(calls.len(), 3);
        assert!(matches!(
            &calls[0],
            Call::Create(input)
                if input.payload.create_workspace_root_if_missing == Some(true)
                    && input.payload.default_model_selection == Some(None)
                    && input.payload.scripts == Some(Vec::new())
                    && input.payload.workspace_root.as_str() == "/work/created"
        ));
        assert!(matches!(
            &calls[1],
            Call::Update(input)
                if input.payload.title.as_ref().map(|title| title.as_str()) == Some("Updated")
                    && input.payload.auto_pull == Some(false)
                    && input.payload.project_icon == Some(None)
                    && input.payload.favicon_path == Some(None)
                    && input.payload.default_thread_env_mode == Some(None)
        ));
        assert_eq!(
            calls[2],
            Call::Delete(ProjectDeleteInput {
                command_id: command("command:delete"),
                project_id,
                force: Some(true),
            })
        );
    }
}
