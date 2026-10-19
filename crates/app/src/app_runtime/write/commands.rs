//! The app's command layer: what the UI seams (`ComposerSource::send`,
//! `ThreadCommands`, project create and rename, thread create) call.
//!
//! Every method builds the same command a web client sends over
//! `orchestrationV2.dispatchCommand` and runs it through
//! [`WriteRuntime::dispatch`]. Errors carry the text upstream's toast shows
//! (`error.message` plus its cause chain).

use std::sync::Arc;

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::OrchestrationV2Command;
use turin_contracts::{CommandId, EventId, MessageId, ProjectId, ThreadId, UtcDateTime};
use turin_orchestration::pipeline::effect_worker::error_chain;
use turin_orchestration::pipeline::event_sink::ProjectCommit;
use turin_orchestration::pipeline::orchestrator_port::{
    DispatchResult, OrchestratorPort as _, RecordField, RecordFilter,
};
use turin_orchestration::pipeline::project_commands::{
    ProjectCommand, ProjectCommandRow, ProjectCommandState, ProjectCreateCommand,
    ProjectMetaUpdateCommand, plan_project_command,
};
use turin_orchestration::pipeline::thread_management_service::latest_active_run;

use super::runtime::WriteRuntime;

/// A failed command, ready for a toast or an inline error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CommandError {
    pub message: String,
}

impl CommandError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn from_error(error: &(dyn std::error::Error + 'static)) -> Self {
        Self::new(error_chain(error))
    }
}

/// What the composer's send reports back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendReceipt {
    pub command_id: CommandId,
    pub message_id: MessageId,
    pub sequence: u64,
}

/// How the follow-up is delivered (`ComposerDispatchMode` after resolving the
/// target run).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// `auto` while idle: start a run now.
    StartImmediately,
    QueueAfterActive,
    SteerActive {
        target_run_id: String,
    },
    RestartActive {
        target_run_id: String,
    },
}

#[derive(Debug, Clone)]
pub struct SendMessage {
    pub thread_id: ThreadId,
    pub text: String,
    /// `ChatAttachment` JSON values, as the composer produces them.
    pub attachments: Vec<Value>,
    /// `{"instanceId", "model", "options"?}`; `None` keeps the thread's selection.
    pub model_selection: Option<Value>,
    pub delivery: Delivery,
    /// Seeds a title from the first message.
    pub title_seed: Option<String>,
    /// The proposed plan this message implements (`sourcePlanRef`).
    pub source_plan_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CreateThread {
    pub project_id: ProjectId,
    pub title: String,
    pub model_selection: Value,
    pub runtime_mode: String,
    pub interaction_mode: String,
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn command_id() -> CommandId {
    CommandId::new(new_id()).expect("a uuid is a valid command id")
}

fn client_command(value: Value) -> Result<OrchestrationV2Command, CommandError> {
    serde_json::from_value::<OrchestrationV2Command>(value)
        .map_err(|error| CommandError::new(format!("Invalid command: {error}")))
}

#[derive(Clone)]
pub struct Commands {
    runtime: Arc<WriteRuntime>,
}

impl Commands {
    pub fn new(runtime: Arc<WriteRuntime>) -> Self {
        Self { runtime }
    }

    pub fn runtime(&self) -> Arc<WriteRuntime> {
        Arc::clone(&self.runtime)
    }

    async fn dispatch(&self, command: Value) -> Result<DispatchResult, CommandError> {
        let command = client_command(command)?;
        self.runtime
            .dispatch(command)
            .await
            .map_err(|error| CommandError::from_error(&error))
    }

    /// `project.create`. The caller picked the folder; the workspace root must
    /// not already belong to an active project.
    pub async fn create_project(
        &self,
        title: &str,
        workspace_root: &str,
    ) -> Result<ProjectId, CommandError> {
        let project_id = ProjectId::new(new_id()).expect("uuid project id");
        self.create_project_with_id(project_id, title, workspace_root)
            .await
    }

    /// `project.create` with `createWorkspaceRootIfMissing: true` (the command
    /// palette's Add project): the folder is created when it does not exist, so
    /// a mistyped path never yields a project whose sessions cannot start.
    pub async fn create_project_creating_folder(
        &self,
        title: &str,
        workspace_root: &str,
    ) -> Result<ProjectId, CommandError> {
        tokio::fs::create_dir_all(workspace_root)
            .await
            .map_err(|error| CommandError {
                message: format!("Could not create the folder {workspace_root}: {error}"),
            })?;
        self.create_project(title, workspace_root).await
    }

    /// Same as [`Self::create_project`], but keeps the caller's project id
    /// (onboarding retries must not mint a new id each attempt).
    pub async fn create_project_with_id(
        &self,
        project_id: ProjectId,
        title: &str,
        workspace_root: &str,
    ) -> Result<ProjectId, CommandError> {
        let command = ProjectCommand::Create(ProjectCreateCommand {
            command_id: command_id(),
            project_id: project_id.clone(),
            title: title.to_owned(),
            workspace_root: workspace_root.to_owned(),
            scripts: None,
        });
        self.commit_project(command).await?;
        Ok(project_id)
    }

    /// `project.meta.update` with a new title.
    pub async fn rename_project(
        &self,
        project_id: &ProjectId,
        title: &str,
    ) -> Result<(), CommandError> {
        let command = ProjectCommand::MetaUpdate(ProjectMetaUpdateCommand {
            command_id: command_id(),
            project_id: project_id.clone(),
            title: Some(title.to_owned()),
            workspace_root: None,
            default_model_selection: None,
            default_thread_env_mode: None,
            auto_pull: None,
            favicon_path: None,
            project_icon: None,
            scripts: None,
        });
        self.commit_project(command).await
    }

    async fn commit_project(&self, command: ProjectCommand) -> Result<(), CommandError> {
        let stores = &self.runtime.stores;
        let project_id = command.project_id().clone();
        let row =
            |row: turin_orchestration::projection::project_store::ProjectRow| ProjectCommandRow {
                project_id: row.project_id,
                scripts: row.scripts,
                deleted_at: row.deleted_at,
            };
        let project = stores
            .projects
            .get(project_id.clone(), true)
            .await
            .map_err(|error| CommandError::from_error(&error))?
            .map(row);
        let workspace_root = match &command {
            ProjectCommand::Create(create) => Some(create.workspace_root.clone()),
            ProjectCommand::MetaUpdate(update) => update.workspace_root.clone(),
            ProjectCommand::Delete(_) => None,
        };
        let workspace_owner = match workspace_root {
            Some(root) => stores
                .projects
                .find_active_by_workspace_root(root)
                .await
                .map_err(|error| CommandError::from_error(&error))?
                .map(row),
            None => None,
        };
        let now = UtcDateTime::now();
        let event = plan_project_command(
            &command,
            &ProjectCommandState {
                project,
                workspace_owner,
            },
            EventId::new(new_id()).expect("uuid event id"),
            now,
        )
        .map_err(|rejection| CommandError::new(rejection.to_string()))?;
        stores
            .sink
            .commit_project_command(ProjectCommit {
                command_id: command.command_id().clone(),
                project_id,
                command_type: command.command_type().to_owned(),
                accepted_at: now,
                event,
            })
            .await
            .map_err(|error| CommandError::from_error(&error))?;
        Ok(())
    }

    /// `thread.create`, created by the user from the app.
    pub async fn create_thread(&self, input: CreateThread) -> Result<ThreadId, CommandError> {
        let thread_id = ThreadId::new(new_id()).expect("uuid thread id");
        self.dispatch(json!({
            "type": "thread.create",
            "createdBy": "user",
            "creationSource": "web",
            "commandId": command_id(),
            "threadId": thread_id,
            "projectId": input.project_id,
            "title": input.title,
            "modelSelection": input.model_selection,
            "runtimeMode": input.runtime_mode,
            "interactionMode": input.interaction_mode,
            "branch": null,
            "worktreePath": null,
        }))
        .await?;
        Ok(thread_id)
    }

    /// `message.dispatch` from the composer.
    pub async fn send_message(&self, input: SendMessage) -> Result<SendReceipt, CommandError> {
        let command_id = command_id();
        let message_id = MessageId::new(new_id()).expect("uuid message id");
        let dispatch_mode = match &input.delivery {
            Delivery::StartImmediately => json!({"type": "start_immediately"}),
            Delivery::QueueAfterActive => json!({"type": "queue_after_active"}),
            Delivery::SteerActive { target_run_id } => {
                json!({"type": "steer_active", "targetRunId": target_run_id})
            }
            Delivery::RestartActive { target_run_id } => {
                json!({"type": "restart_active", "targetRunId": target_run_id})
            }
        };
        let mut command = json!({
            "type": "message.dispatch",
            "createdBy": "user",
            "creationSource": "web",
            "commandId": command_id,
            "threadId": input.thread_id,
            "messageId": message_id,
            "text": input.text,
            "attachments": input.attachments,
            "dispatchMode": dispatch_mode,
        });
        let object = command.as_object_mut().expect("object literal");
        if let Some(selection) = input.model_selection {
            object.insert("modelSelection".into(), selection);
        }
        if let Some(seed) = input.title_seed {
            object.insert("titleSeed".into(), json!(seed));
        }
        if let Some(plan_id) = input.source_plan_id {
            object.insert(
                "sourcePlanRef".into(),
                json!({"threadId": input.thread_id, "planId": plan_id}),
            );
        }
        let result = self.dispatch(command).await?;
        Ok(SendReceipt {
            command_id,
            message_id,
            sequence: result.sequence,
        })
    }

    /// Stop button: `run.interrupt` on the thread's active run with
    /// `holdQueue`, which also holds the queue, ends pull request watches and
    /// stops delegated tasks. Nothing running is an accepted no-op.
    pub async fn stop_thread(&self, thread_id: &ThreadId) -> Result<(), CommandError> {
        let records = self
            .runtime
            .orchestrator
            .get_thread_records(thread_id, &[RecordField::Runs], &RecordFilter::default())
            .await
            .map_err(|error| CommandError::from_error(&error))?;
        let Some(run) = latest_active_run(&records.runs) else {
            return Ok(());
        };
        self.dispatch(json!({
            "type": "run.interrupt",
            "commandId": command_id(),
            "threadId": thread_id,
            "runId": run.id,
            "holdQueue": true,
        }))
        .await
        .map(drop)
    }

    /// `mergeBack` in the thread relationships panel: merges `run_id` of the
    /// fork `source_thread_id` back into `target_thread_id`.
    pub async fn merge_thread_back(
        &self,
        source_thread_id: &ThreadId,
        target_thread_id: &ThreadId,
        run_id: &str,
    ) -> Result<(), CommandError> {
        self.dispatch(json!({
            "type": "thread.merge_back",
            "createdBy": "user",
            "creationSource": "web",
            "commandId": command_id(),
            "sourceThreadId": source_thread_id,
            "targetThreadId": target_thread_id,
            "sourcePoint": {"type": "run", "runId": run_id},
        }))
        .await
        .map(drop)
    }

    /// `stopSession` ("Disconnect agent session"): detaches every provider
    /// session of the thread.
    pub async fn detach_thread_sessions(&self, thread_id: &ThreadId) -> Result<(), CommandError> {
        let records = self
            .runtime
            .orchestrator
            .get_thread_records(
                thread_id,
                &[RecordField::ProviderSessions],
                &RecordFilter::default(),
            )
            .await
            .map_err(|error| CommandError::from_error(&error))?;
        let base = command_id();
        for session in records.provider_sessions.iter() {
            self.dispatch(json!({
                "type": "provider-session.detach",
                "commandId": format!("{base}:detach:{}", session.id),
                "threadId": thread_id,
                "providerSessionId": session.id,
                "reason": "client-requested",
            }))
            .await?;
        }
        Ok(())
    }

    /// The lineage panel's per-row stop (`interruptTurn`): a provider-native
    /// child stops through `subagent.stop` on the parent; an app-owned child
    /// is its own thread, so its active run is interrupted.
    pub async fn stop_subagent(
        &self,
        parent_thread_id: &ThreadId,
        subagent_id: &str,
        child_thread_id: &ThreadId,
    ) -> Result<(), CommandError> {
        let records = self
            .runtime
            .orchestrator
            .get_thread_records(
                parent_thread_id,
                &[RecordField::Subagents],
                &RecordFilter::default(),
            )
            .await
            .map_err(|error| CommandError::from_error(&error))?;
        let native = records.subagents.iter().find(|agent| {
            agent.id.as_str() == subagent_id
                && agent.origin
                    == turin_contracts::orchestration_v2::OrchestrationV2SubagentOrigin::ProviderNative
        });
        match native {
            Some(agent) => {
                let Some(run_id) = agent.run_id.as_ref() else {
                    return Ok(());
                };
                self.dispatch(json!({
                    "type": "subagent.stop",
                    "commandId": command_id(),
                    "threadId": parent_thread_id,
                    "runId": run_id,
                    "subagentId": agent.id,
                }))
                .await
                .map(drop)
            }
            None => self.stop_thread(child_thread_id).await,
        }
    }

    /// `stopWorkflow`: asks the provider to stop a running workflow coordinator.
    pub async fn stop_workflow(
        &self,
        thread_id: &ThreadId,
        subagent_id: &str,
    ) -> Result<(), CommandError> {
        let subagent_id = turin_contracts::NodeId::new(subagent_id)
            .map_err(|error| CommandError::new(error.to_string()))?;
        self.runtime
            .threads
            .stop_workflow(
                turin_contracts::orchestration_v2::OrchestrationV2StopWorkflowInput {
                    thread_id: thread_id.clone(),
                    subagent_id,
                },
                None,
            )
            .await
            .map_err(|error| CommandError::from_error(&error))
    }

    /// `runtime-request.respond`: an approval decision (`accept`,
    /// `acceptForSession`, `acceptAlways`, `decline`, `cancel`) or the answers
    /// to a user-input request (question id -> answer).
    pub async fn respond_to_request(
        &self,
        thread_id: &ThreadId,
        request_id: &str,
        decision: Option<&str>,
        answers: Option<Value>,
    ) -> Result<(), CommandError> {
        let mut command = json!({
            "type": "runtime-request.respond",
            "commandId": command_id(),
            "threadId": thread_id,
            "requestId": request_id,
        });
        if let Some(object) = command.as_object_mut() {
            if let Some(decision) = decision {
                object.insert("decision".into(), json!(decision));
            }
            if let Some(answers) = answers {
                object.insert("answers".into(), answers);
            }
        }
        self.dispatch(command).await.map(drop)
    }

    /// `runtime-request.respond` for user-input answers, optionally with
    /// staged question attachments (`attachmentsByQuestionId`).
    pub async fn respond_to_user_input_request(
        &self,
        thread_id: &ThreadId,
        request_id: &str,
        answers: Value,
        attachments_by_question_id: Option<Value>,
    ) -> Result<(), CommandError> {
        let mut command = json!({
            "type": "runtime-request.respond",
            "commandId": command_id(),
            "threadId": thread_id,
            "requestId": request_id,
            "answers": answers,
        });
        if let Some(object) = command.as_object_mut()
            && let Some(attachments) = attachments_by_question_id
        {
            object.insert("attachmentsByQuestionId".into(), attachments);
        }
        self.dispatch(command).await.map(drop)
    }

    /// `thread.user-input.dismiss`: gives up on a pending question.
    pub async fn dismiss_user_input(
        &self,
        thread_id: &ThreadId,
        request_id: &str,
    ) -> Result<(), CommandError> {
        self.thread_command(
            "thread.user-input.dismiss",
            thread_id,
            json!({"requestId": request_id}),
        )
        .await
    }

    /// Writes an upload to the attachment store as a pending attachment and
    /// returns its `ChatAttachment` (JSON), ready for `send_message`. The
    /// message intake claims it into the thread on send.
    pub async fn stage_attachment(
        &self,
        image: bool,
        name: &str,
        mime_type: &str,
        bytes: Vec<u8>,
    ) -> Result<Value, CommandError> {
        use turin_orchestration::pipeline::attachment_store::{
            attachment_file_extension, attachment_relative_path, create_pending_attachment_id,
            infer_image_extension, resolve_attachment_relative_path,
        };
        let extension = if image {
            infer_image_extension(mime_type, Some(name))
        } else {
            attachment_file_extension(name)
        };
        let attachment = json!({
            "type": if image { "image" } else { "file" },
            "id": create_pending_attachment_id(Some(&extension)),
            "name": name,
            "mimeType": mime_type,
            "sizeBytes": bytes.len(),
        });
        let parsed: turin_contracts::chat_attachment::ChatAttachment =
            serde_json::from_value(attachment.clone())
                .map_err(|error| CommandError::new(format!("Invalid attachment: {error}")))?;
        let dir = self.runtime.attachments_dir.clone();
        let path = attachment_relative_path(&parsed)
            .and_then(|relative| resolve_attachment_relative_path(&dir, &relative))
            .ok_or_else(|| CommandError::new("Cannot place the attachment in the store."))?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                CommandError::new(format!("Cannot create {}: {error}", parent.display()))
            })?;
        }
        tokio::fs::write(&path, bytes)
            .await
            .map_err(|error| CommandError::new(format!("Cannot store {name}: {error}")))?;
        Ok(attachment)
    }

    /// `@` completion: workspace files and folders matching `query`, as
    /// `(relative path, is directory)`, best first.
    pub async fn search_paths(
        &self,
        cwd: &str,
        query: &str,
        limit: u32,
    ) -> Result<Vec<(String, bool)>, CommandError> {
        use turin_contracts::project::{ProjectEntryKind, ProjectSearchEntriesInput};
        let input: ProjectSearchEntriesInput =
            serde_json::from_value(json!({"cwd": cwd, "query": query, "limit": limit}))
                .map_err(|error| CommandError::new(format!("Invalid path search: {error}")))?;
        let result = self
            .runtime
            .workspace_entries
            .search(&input)
            .await
            .map_err(|error| CommandError::from_error(&error))?;
        Ok(result
            .entries
            .into_iter()
            .map(|entry| {
                (
                    entry.path.to_string(),
                    entry.kind == ProjectEntryKind::Directory,
                )
            })
            .collect())
    }

    /// Cross-thread message search for the sidebar and palette
    /// (`ThreadSearch.ts`): the newest best message per thread.
    pub async fn search_messages(
        &self,
        query: &str,
        limit: i64,
    ) -> Result<Vec<turin_contracts::thread_search::OrchestrationThreadSearchMatch>, CommandError>
    {
        let input = turin_contracts::thread_search::OrchestrationSearchThreadsInput {
            query: turin_contracts::TrimmedString::new(query),
            limit: Some(limit),
        };
        turin_orchestration::projection::thread_search::search(&self.runtime.stores.db, input)
            .await
            .map(|result| result.matches)
            .map_err(|error| CommandError::from_error(&error))
    }

    /// Any thread command that is just `{type, commandId, threadId, ..extra}`
    /// (archive, delete, pin, ...).
    pub async fn thread_command(
        &self,
        kind: &str,
        thread_id: &ThreadId,
        extra: Value,
    ) -> Result<(), CommandError> {
        let mut command = json!({
            "type": kind,
            "commandId": command_id(),
            "threadId": thread_id,
        });
        if let (Some(target), Value::Object(extra)) = (command.as_object_mut(), extra) {
            target.extend(extra);
        }
        self.dispatch(command).await.map(drop)
    }

    /// The model and runtime mode a new thread in `project_id` starts with
    /// (project default, then settings default, then the first ready provider).
    pub async fn new_thread_defaults(
        &self,
        project_id: &ProjectId,
    ) -> Result<super::new_thread_defaults::NewThreadDefaults, CommandError> {
        let settings = self.server_settings().await?;
        let legacy = self
            .runtime
            .stores
            .projects
            .get(project_id.clone(), true)
            .await
            .map_err(|error| CommandError::from_error(&error))?
            .map(
                |row| turin_shared::project_settings::LegacyProjectSettingsFields {
                    default_model_selection: row.default_model_selection,
                    default_thread_env_mode: row.default_thread_env_mode,
                },
            );
        Ok(super::new_thread_defaults::resolve_new_thread_defaults(
            &settings,
            project_id,
            legacy.as_ref(),
            &self.provider_snapshots(),
        ))
    }

    pub async fn server_settings(
        &self,
    ) -> Result<turin_contracts::settings::ServerSettings, CommandError> {
        self.runtime
            .server_settings
            .get_settings()
            .await
            .map_err(|error| CommandError::from_error(&error))
    }

    pub async fn update_server_settings(
        &self,
        patch: turin_contracts::settings::ServerSettingsPatch,
    ) -> Result<turin_contracts::settings::ServerSettings, CommandError> {
        self.runtime
            .server_settings
            .update_settings(&patch)
            .await
            .map_err(|error| CommandError::from_error(&error))?;
        self.runtime
            .sync_provider_settings()
            .await
            .map_err(|error| CommandError::from_error(&error))
    }

    pub async fn update_provider_instance(
        &self,
        mutation: turin_contracts::provider_instance::ProviderInstanceMutation,
    ) -> Result<turin_contracts::settings::ServerSettings, CommandError> {
        self.runtime
            .server_settings
            .update_provider_instance(&mutation, None)
            .await
            .map_err(|error| CommandError::from_error(&error))?;
        self.runtime
            .sync_provider_settings()
            .await
            .map_err(|error| CommandError::from_error(&error))
    }

    /// Full live git status including the real source-control change request.
    pub async fn git_status(
        &self,
        cwd: &str,
    ) -> Result<turin_contracts::git::VcsStatusResult, CommandError> {
        let input = turin_contracts::git::VcsStatusInput {
            cwd: turin_contracts::TrimmedNonEmptyString::new(cwd)
                .map_err(|error| CommandError::new(error.to_string()))?,
        };
        self.runtime
            .git_manager
            .status(&input)
            .await
            .map_err(|error| CommandError::from_error(&error))
    }

    /// Local branches; use `git_list_refs` for paged local + remote refs.
    pub async fn git_branches(&self, cwd: &str) -> Result<Vec<String>, CommandError> {
        self.runtime
            .git
            .list_local_branch_names(cwd)
            .await
            .map_err(|error| CommandError::from_error(&error))
    }

    pub async fn git_list_refs(
        &self,
        input: &turin_contracts::git::VcsListRefsInput,
    ) -> Result<turin_contracts::git::VcsListRefsResult, CommandError> {
        self.runtime
            .git
            .list_refs(input)
            .await
            .map_err(|error| CommandError::from_error(&error))
    }

    pub async fn git_switch_branch(
        &self,
        input: turin_contracts::git::VcsSwitchRefInput,
    ) -> Result<turin_contracts::git::VcsSwitchRefResult, CommandError> {
        let result = self
            .runtime
            .git
            .switch_ref(&input)
            .await
            .map_err(|error| CommandError::from_error(&error))?;
        self.runtime
            .git_manager
            .invalidate_status(input.cwd.as_str());
        Ok(result)
    }

    /// `vcsEnvironment.createRef`: creates `ref_name` and, when asked, switches to it.
    pub async fn git_create_ref(
        &self,
        input: turin_contracts::git::VcsCreateRefInput,
    ) -> Result<turin_contracts::git::VcsCreateRefResult, CommandError> {
        let result = self
            .runtime
            .git
            .create_ref(&input)
            .await
            .map_err(|error| CommandError::from_error(&error))?;
        self.runtime
            .git_manager
            .invalidate_status(input.cwd.as_str());
        Ok(result)
    }

    /// `vcsEnvironment.pull`: fast-forwards the current branch from its upstream.
    pub async fn git_pull(
        &self,
        cwd: &str,
    ) -> Result<turin_contracts::git::VcsPullResult, CommandError> {
        self.runtime
            .git_manager
            .pull_current_branch(cwd)
            .await
            .map_err(|error| CommandError::from_error(&error))
    }

    /// `vcsEnvironment.init`: `git init` in `cwd`.
    pub async fn git_init(&self, cwd: &str) -> Result<(), CommandError> {
        let input = turin_contracts::git::VcsInitInput {
            cwd: turin_contracts::TrimmedNonEmptyString::new(cwd)
                .map_err(|error| CommandError::new(error.to_string()))?,
            kind: None,
        };
        let result = self
            .runtime
            .git
            .init_repo(&input)
            .await
            .map_err(|error| CommandError::from_error(&error));
        self.runtime.git_manager.invalidate_status(cwd);
        result
    }

    /// `vcsEnvironment.runStackedAction`: commit, push and pull request steps.
    /// `progress` hears every phase and hook event as it happens.
    pub async fn git_run_stacked_action(
        &self,
        input: turin_contracts::git::GitRunStackedActionInput,
        progress: Option<Arc<dyn Fn(turin_contracts::git::GitActionProgressEvent) + Send + Sync>>,
    ) -> Result<turin_contracts::git::GitRunStackedActionResult, CommandError> {
        let options = turin_git::git_manager::GitRunStackedActionOptions {
            progress,
            project: None,
        };
        self.runtime
            .git_manager
            .run_stacked_action(&input, options)
            .await
            .map_err(|error| CommandError::from_error(&error))
    }

    /// `sourceControlEnvironment.publishRepository`: creates the remote, wires it
    /// up and pushes the current branch.
    pub async fn git_publish_repository(
        &self,
        input: turin_contracts::source_control::SourceControlPublishRepositoryInput,
    ) -> Result<turin_contracts::source_control::SourceControlPublishRepositoryResult, CommandError>
    {
        let cwd = input.cwd.as_str().to_owned();
        let service =
            turin_source_control::source_control::repository_service::SourceControlRepositoryService::new(
                turin_source_control::shared::host_process::working_directory(),
                std::env::var("HOME").unwrap_or_default(),
                Arc::clone(&self.runtime.source_control) as Arc<_>,
                Arc::new(turin_git::source_control_bridge::GitRepositoryGit::new(
                    Arc::clone(&self.runtime.git),
                )),
            );
        let result = service
            .publish_repository(input)
            .await
            .map_err(|error| CommandError::from_error(&error));
        self.runtime.git_manager.invalidate_status(&cwd);
        result
    }

    /// Which source control providers are installed and signed in (Publish needs one).
    pub async fn source_control_providers(
        &self,
    ) -> Vec<turin_contracts::source_control::SourceControlProviderDiscoveryItem> {
        self.runtime.source_control.discover().await
    }

    /// `threadEnvironment.updateMetadata` with a branch (and worktree) patch. `None`
    /// clears the field; `expected_empty` makes the update lose to any launch that
    /// lands first.
    pub async fn update_thread_branch(
        &self,
        thread_id: &ThreadId,
        branch: Option<String>,
        worktree_path: Option<Option<String>>,
        expected_empty: bool,
    ) -> Result<(), CommandError> {
        let mut command = json!({
            "type": "thread.metadata.update",
            "commandId": command_id(),
            "threadId": thread_id,
            "branch": branch,
        });
        if let Some(object) = command.as_object_mut() {
            if let Some(worktree_path) = worktree_path {
                object.insert("worktreePath".into(), json!(worktree_path));
            }
            if expected_empty {
                object.insert("expectedEmpty".into(), json!(true));
            }
        }
        self.dispatch(command).await.map(drop)
    }

    /// A missing production reader is unavailable, never a successful zero total.
    pub async fn usage_summary(
        &self,
        input: &turin_contracts::usage::UsageSummaryInput,
    ) -> Result<turin_contracts::usage::UsageSummary, CommandError> {
        if !self.runtime.usage_readers_available {
            return Err(CommandError::new(
                "Usage history is unavailable: no production usage readers are connected.",
            ));
        }
        let settings = self.runtime.providers.settings.settings().await;
        self.runtime
            .usage
            .read_summary(input, &settings)
            .await
            .map_err(|error| CommandError::from_error(&error))
    }

    /// Pending approval cards across active, non-archived threads. Questions and
    /// auth prompts have separate UI affordances and do not inflate the badge.
    pub async fn pending_approval_count(&self) -> Result<usize, CommandError> {
        self.runtime.stores.db.read(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM orchestration_v2_projection_runtime_requests request
                 INNER JOIN orchestration_v2_projection_threads thread ON thread.thread_id = request.thread_id
                 WHERE request.status = 'pending' AND thread.archived_at IS NULL AND thread.deleted_at IS NULL
                 AND request.kind IN ('command', 'file-read', 'file-change', 'mcp-elicitation', 'permission', 'dynamic_tool_call')",
                [], |row| row.get::<_, i64>(0),
            ).map(|count| count as usize).map_err(|error| turin_persistence::PersistenceError::sql("runtime.pendingApprovalCount", &error))
        }).await.map_err(|error| CommandError::from_error(&error))
    }
}

/// Scope of actual history reads. Missing readers must be shown as unavailable,
/// rather than implying that an absent provider's usage was zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageReadiness {
    pub history_driver_kinds: Vec<String>,
    pub unavailable_history_driver_kinds: Vec<String>,
}

impl Commands {
    pub fn usage_readiness(&self) -> UsageReadiness {
        let history_driver_kinds = self.runtime.usage_history_drivers.clone();
        let unavailable_history_driver_kinds =
            turin_providers::built_in_drivers::BUILT_IN_USAGE_DRIVER_ORDER
                .iter()
                .filter(|kind| {
                    !history_driver_kinds
                        .iter()
                        .any(|available| available == **kind)
                })
                .map(|kind| (*kind).to_owned())
                .collect();
        UsageReadiness {
            history_driver_kinds,
            unavailable_history_driver_kinds,
        }
    }

    /// Latest real provider readiness + quota snapshots, safe to read in a UI
    /// render. Subscribe to CommandBus::providers for subsequent changes.
    pub fn provider_snapshots(&self) -> Vec<turin_provider_core::server_provider::ServerProvider> {
        self.runtime.providers.registry.get_providers()
    }

    /// On-demand probes refresh actual install/auth/readiness and quota state.
    /// Quota failures live on each provider snapshot; they are not zero windows.
    pub async fn refresh_usage_limits(
        &self,
    ) -> Vec<turin_provider_core::server_provider::ServerProvider> {
        self.runtime.providers.registry.refresh(None).await
    }

    /// Render redemption only when the actual instance supplies its hook.
    pub fn can_consume_reset_credit(
        &self,
        instance_id: &turin_contracts::provider_instance::ProviderInstanceId,
    ) -> bool {
        self.runtime
            .providers
            .instances()
            .get_instance(instance_id)
            .is_some_and(|instance| instance.enabled && instance.consume_reset_credit.is_some())
    }

    pub async fn consume_reset_credit(
        &self,
        input: turin_contracts::provider_usage_limits::ProviderConsumeResetCreditInput,
    ) -> Result<
        turin_contracts::provider_usage_limits::ProviderConsumeResetCreditResult,
        CommandError,
    > {
        use turin_contracts::provider_usage_limits::{
            ProviderConsumeResetCreditInput, ProviderConsumeResetCreditResult,
        };
        let ProviderConsumeResetCreditInput::Instance { instance_id } = input else {
            return Err(CommandError::new(
                "External usage-limit sources are not connected in this desktop runtime.",
            ));
        };
        let instance = self
            .runtime
            .providers
            .instances()
            .get_instance(&instance_id)
            .ok_or_else(|| CommandError::new("This provider instance is no longer available."))?;
        if !instance.enabled {
            return Err(CommandError::new(
                "Enable this provider instance before redeeming reset credits.",
            ));
        }
        let consume = instance.consume_reset_credit.as_ref().ok_or_else(|| {
            CommandError::new("This provider instance does not support reset credits.")
        })?;
        let outcome = consume()
            .await
            .map_err(|error| CommandError::from_error(&error))?;
        self.runtime
            .providers
            .registry
            .refresh_instance(&instance_id)
            .await;
        Ok(ProviderConsumeResetCreditResult {
            outcome,
            warning: None,
        })
    }
}
