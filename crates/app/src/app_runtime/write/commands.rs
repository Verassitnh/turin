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

    pub fn runtime(&self) -> &Arc<WriteRuntime> {
        &self.runtime
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
        if let Some(object) = command.as_object_mut() {
            if let Some(attachments) = attachments_by_question_id {
                object.insert("attachmentsByQuestionId".into(), attachments);
            }
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
}
