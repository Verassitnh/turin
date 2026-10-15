//! Port of upstream/packages/client-runtime/src/operations/commands.ts
//!
//! Turns the UI's intent into [`OrchestrationV2Command`]s and hands them to a
//! [`CommandPort`], which the orchestrator implements in-process. Upstream's
//! RPC branches that exist for servers *without* command-context support
//! (`serverResolvedCommandContext`) are dropped: Turin's orchestrator is that
//! server, so the context is always resolved on its side and the client never
//! fetches a projection to shape a command.

use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;

use crate::state::contracts::{
    ChatAttachment, CheckpointId, CheckpointScopeId, CommandId, MessageId, ModelSelection, NodeId,
    OrchestrationMessageContext, OrchestrationV2Actor, OrchestrationV2Command,
    OrchestrationV2CreationSource, OrchestrationV2DeliveryIntent, OrchestrationV2DispatchMode,
    OrchestrationV2LimitRecoveryUpdate, OrchestrationV2ThreadForkSourcePoint,
    OrchestrationV2ThreadProjection, PlanId, ProjectIconOverride, ProjectId, ProjectMutation,
    ProjectScript, ProviderApprovalDecision, ProviderInteractionMode, ProviderUserInputAnswers,
    RunId, RuntimeMode, RuntimeRequestId, ThreadEnvMode, ThreadId, ThreadLinkedPullRequest,
    ThreadPullRequestLinkSource, TrimmedNonEmptyString, UploadChatAttachment, UserInputAttachments,
    UserUnsettleReason,
};
use crate::state::contracts::{
    ComposerContextRecord, ContextAttachmentId, KnownComposerContextRecord,
    OrchestrationV2SourcePlanRef,
};
use crate::state::support::background_work::{
    DerivePendingBackgroundWorkInput, derive_pending_background_work,
};
use crate::state::support::thread_pull_requests::visible_thread_pull_requests;
use crate::state::thread_projection::ThreadProjection;
use crate::state::thread_workflows::resolve_active_thread_run;

/// `{ sequence }`: the event-log position the command landed at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DispatchResult {
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum CommandError {
    /// The orchestrator rejected or failed the command.
    #[error("{0}")]
    Rejected(String),
    /// `OrchestrationV2CheckpointUnavailableError`.
    #[error("Rollback target {target} is unavailable for thread {thread_id}.")]
    CheckpointUnavailable { thread_id: ThreadId, target: String },
    /// The input cannot form a valid command (an empty required string).
    #[error("{0}")]
    Invalid(String),
}

/// What the orchestrator exposes to the UI (upstream's RPC client surface that
/// `commands.ts` calls).
#[async_trait]
pub trait CommandPort: Send + Sync {
    /// `ORCHESTRATION_V2_WS_METHODS.dispatchCommand`.
    async fn dispatch(
        &self,
        command: OrchestrationV2Command,
    ) -> Result<DispatchResult, CommandError>;
    /// `WS_METHODS.projectsMutate`.
    async fn project_mutate(&self, mutation: ProjectMutation) -> Result<(), CommandError>;
    /// `ORCHESTRATION_V2_WS_METHODS.getThreadProjection`.
    async fn get_thread_projection(
        &self,
        thread_id: &ThreadId,
    ) -> Result<OrchestrationV2ThreadProjection, CommandError>;
    /// `WS_METHODS.assetsPersistChatAttachments`: stores uploads against a message.
    async fn persist_chat_attachments(
        &self,
        thread_id: &ThreadId,
        message_id: &MessageId,
        attachments: Vec<UploadChatAttachment>,
    ) -> Result<Vec<ChatAttachment>, CommandError>;
}

#[derive(Clone, Debug, Default)]
pub struct CommandMetadata {
    pub command_id: Option<CommandId>,
    pub created_at: Option<String>,
    pub creation_source: Option<OrchestrationV2CreationSource>,
}

impl CommandMetadata {
    fn command_id(&self) -> CommandId {
        self.command_id.clone().unwrap_or_else(CommandId::random)
    }

    fn creation_source(&self) -> OrchestrationV2CreationSource {
        self.creation_source
            .unwrap_or(OrchestrationV2CreationSource::Web)
    }
}

fn non_empty(value: &str, what: &str) -> Result<TrimmedNonEmptyString, CommandError> {
    TrimmedNonEmptyString::new(value)
        .map_err(|_| CommandError::Invalid(format!("{what} must not be empty.")))
}

#[derive(Clone, Debug)]
pub struct CreateProjectInput {
    pub meta: CommandMetadata,
    pub project_id: ProjectId,
    pub title: String,
    pub workspace_root: String,
    pub create_workspace_root_if_missing: Option<bool>,
    /// `Some(None)` clears.
    pub default_model_selection: Option<Option<ModelSelection>>,
    pub scripts: Option<Vec<ProjectScript>>,
}

#[derive(Clone, Debug, Default)]
pub struct UpdateProjectInput {
    pub meta: CommandMetadata,
    pub project_id: Option<ProjectId>,
    pub title: Option<String>,
    pub workspace_root: Option<String>,
    pub default_model_selection: Option<Option<ModelSelection>>,
    pub auto_pull: Option<bool>,
    pub project_icon: Option<Option<ProjectIconOverride>>,
    pub favicon_path: Option<Option<String>>,
    pub default_thread_env_mode: Option<Option<ThreadEnvMode>>,
    pub scripts: Option<Vec<ProjectScript>>,
}

#[derive(Clone, Debug)]
pub struct CreateThreadInput {
    pub meta: CommandMetadata,
    pub thread_id: ThreadId,
    pub project_id: ProjectId,
    pub title: String,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct UpdateThreadMetadataInput {
    pub meta: CommandMetadata,
    pub limit_recovery: Option<Option<OrchestrationV2LimitRecoveryUpdate>>,
    pub title: Option<String>,
    pub model_selection: Option<ModelSelection>,
    pub branch: Option<Option<String>>,
    pub worktree_path: Option<Option<String>>,
    /// Kick off an async title regeneration for the thread.
    pub regenerate_title: Option<bool>,
    /// Link (`Some(Some)`) or unlink (`Some(None)`) a pull request (#8160).
    pub linked_pull_request: Option<Option<ThreadLinkedPullRequest>>,
    /// Apply only while no message or run has landed on the thread.
    pub expected_empty: bool,
}

#[derive(Clone, Debug)]
pub struct StartThreadBootstrapCreate {
    pub project_id: ProjectId,
    pub title: String,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Debug)]
pub struct StartThreadMessage {
    pub message_id: MessageId,
    pub text: String,
    /// Already stored attachments, in order.
    pub attachments: Vec<ChatAttachment>,
    /// Image uploads still carrying data URLs; persisted before dispatch. They
    /// take the place of `None` slots… upstream interleaves them with stored
    /// ones, so `attachments` here is the full ordered list.
    pub uploads: Vec<StartThreadAttachment>,
    pub context: Option<OrchestrationMessageContext>,
}

/// One attachment of a message being sent: already stored, or an upload.
#[derive(Clone, Debug)]
pub enum StartThreadAttachment {
    Stored(ChatAttachment),
    Upload(UploadChatAttachment),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DispatchModeRequest {
    #[default]
    Auto,
    Queue,
    Steer,
    Restart,
    Start,
}

#[derive(Clone, Debug)]
pub struct StartThreadTurnInput {
    pub meta: CommandMetadata,
    pub thread_id: ThreadId,
    pub manual_continuation_of_run_id: Option<RunId>,
    pub message_id: MessageId,
    pub text: String,
    pub attachments: Vec<StartThreadAttachment>,
    pub context: Option<OrchestrationMessageContext>,
    pub model_selection: Option<ModelSelection>,
    pub title_seed: Option<String>,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: ProviderInteractionMode,
    /// A thread that does not exist yet is created first.
    pub create_thread: Option<StartThreadBootstrapCreate>,
    pub source_proposed_plan: Option<(ThreadId, PlanId)>,
    pub dispatch_mode: Option<DispatchModeRequest>,
}

#[derive(Clone, Debug, Default)]
pub struct InterruptThreadTurnInput {
    pub meta: CommandMetadata,
    pub run_id: Option<RunId>,
    pub subagent_id: Option<NodeId>,
}

#[derive(Clone, Debug, Default)]
pub struct RevertThreadCheckpointInput {
    pub meta: CommandMetadata,
    pub restore_files: Option<bool>,
    pub checkpoint_id: Option<String>,
    pub scope_id: Option<String>,
    pub turn_count: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct EditQueuedRunEdit {
    pub message_id: MessageId,
    pub attachments: Vec<StartThreadAttachment>,
    pub context: Option<OrchestrationMessageContext>,
}

/// `remapComposerContextAttachments`: preserve context bindings when uploads
/// become thread-owned attachments.
pub fn remap_composer_context_attachments(
    context: Option<&OrchestrationMessageContext>,
    before: &[StartThreadAttachment],
    after: &[ChatAttachment],
) -> Option<OrchestrationMessageContext> {
    let context = context?;
    let ids: std::collections::HashMap<String, String> = before
        .iter()
        .enumerate()
        .filter_map(|(index, attachment)| {
            let before_id = match attachment {
                StartThreadAttachment::Stored(stored) => Some(chat_attachment_id(stored)),
                StartThreadAttachment::Upload(upload) => {
                    upload.id.as_ref().map(|id| id.as_str().to_owned())
                }
            }?;
            Some((before_id, chat_attachment_id(after.get(index)?)))
        })
        .collect();
    let mut remapped = context.clone();
    for record in &mut remapped.records {
        let ComposerContextRecord::Known(known) = record else {
            continue;
        };
        let attachment_id = match known {
            KnownComposerContextRecord::Image(record) => &mut record.attachment_id,
            KnownComposerContextRecord::File(record) => &mut record.attachment_id,
            _ => continue,
        };
        if let Some(target) = ids.get(attachment_id.as_str())
            && let Ok(target) = ContextAttachmentId::new(target)
        {
            *attachment_id = target;
        }
    }
    Some(remapped)
}

fn chat_attachment_id(attachment: &ChatAttachment) -> String {
    match attachment {
        ChatAttachment::Image(image) => image.id.as_str().to_owned(),
        ChatAttachment::File(file) => file.id.as_str().to_owned(),
        ChatAttachment::Unknown(unknown) => unknown.id.as_str().to_owned(),
    }
}

/// The ordered attachment list after persisting uploads: stored ones keep their
/// place, each upload is replaced by what the orchestrator stored for it.
async fn persist_attachments(
    port: &dyn CommandPort,
    thread_id: &ThreadId,
    message_id: &MessageId,
    attachments: &[StartThreadAttachment],
) -> Result<Vec<ChatAttachment>, CommandError> {
    let uploads: Vec<UploadChatAttachment> = attachments
        .iter()
        .filter_map(|attachment| match attachment {
            StartThreadAttachment::Upload(upload) => Some(upload.clone()),
            StartThreadAttachment::Stored(_) => None,
        })
        .collect();
    if uploads.is_empty() {
        return Ok(attachments
            .iter()
            .filter_map(|attachment| match attachment {
                StartThreadAttachment::Stored(stored) => Some(stored.clone()),
                StartThreadAttachment::Upload(_) => None,
            })
            .collect());
    }
    let persisted = port
        .persist_chat_attachments(thread_id, message_id, uploads)
        .await?;
    let mut persisted = persisted.into_iter();
    Ok(attachments
        .iter()
        .filter_map(|attachment| match attachment {
            StartThreadAttachment::Stored(stored) => Some(stored.clone()),
            StartThreadAttachment::Upload(_) => persisted.next(),
        })
        .collect())
}

/// The commands the UI sends, built from its intent.
#[derive(Clone)]
pub struct EnvironmentCommands {
    port: Arc<dyn CommandPort>,
}

impl EnvironmentCommands {
    pub fn new(port: Arc<dyn CommandPort>) -> Self {
        Self { port }
    }

    async fn dispatch(
        &self,
        command: OrchestrationV2Command,
    ) -> Result<DispatchResult, CommandError> {
        self.port.dispatch(command).await
    }

    // ---- projects ------------------------------------------------------

    pub async fn create_project(&self, input: CreateProjectInput) -> Result<(), CommandError> {
        self.port
            .project_mutate(ProjectMutation::ProjectCreate {
                command_id: input.meta.command_id(),
                project_id: input.project_id,
                title: non_empty(&input.title, "Project title")?,
                workspace_root: non_empty(&input.workspace_root, "Workspace root")?,
                create_workspace_root_if_missing: input.create_workspace_root_if_missing,
                default_model_selection: input.default_model_selection,
                scripts: input.scripts,
            })
            .await
    }

    pub async fn update_project(&self, input: UpdateProjectInput) -> Result<(), CommandError> {
        let project_id = input
            .project_id
            .clone()
            .ok_or_else(|| CommandError::Invalid("A project id is required.".to_owned()))?;
        let mutation = build_project_update(&input, project_id)?;
        self.port.project_mutate(mutation).await
    }

    pub async fn delete_project(
        &self,
        meta: CommandMetadata,
        project_id: ProjectId,
        force: Option<bool>,
    ) -> Result<(), CommandError> {
        self.port
            .project_mutate(ProjectMutation::ProjectDelete {
                command_id: meta.command_id(),
                project_id,
                force,
            })
            .await
    }

    // ---- threads -------------------------------------------------------

    pub async fn create_thread(
        &self,
        input: CreateThreadInput,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadCreate {
            created_by: OrchestrationV2Actor::User,
            creation_source: input.meta.creation_source(),
            command_id: input.meta.command_id(),
            thread_id: input.thread_id,
            project_id: input.project_id,
            title: non_empty(&input.title, "Thread title")?,
            model_selection: input.model_selection,
            runtime_mode: input.runtime_mode,
            interaction_mode: input.interaction_mode,
            branch: input.branch.map(|b| non_empty(&b, "Branch")).transpose()?,
            worktree_path: input
                .worktree_path
                .map(|p| non_empty(&p, "Worktree path"))
                .transpose()?,
            imported_native_thread: None,
        })
        .await
    }

    pub async fn delete_thread(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadDelete {
            command_id: meta.command_id(),
            thread_id,
        })
        .await
    }

    pub async fn archive_thread(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadArchive {
            command_id: meta.command_id(),
            thread_id,
        })
        .await
    }

    pub async fn unarchive_thread(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadUnarchive {
            command_id: meta.command_id(),
            thread_id,
        })
        .await
    }

    pub async fn settle_thread(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadSettle {
            command_id: meta.command_id(),
            thread_id,
            settled_at: None,
        })
        .await
    }

    pub async fn pin_thread(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        order_key: Option<String>,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadPin {
            command_id: meta.command_id(),
            thread_id,
            order_key: order_key
                .map(|key| non_empty(&key, "Order key"))
                .transpose()?,
        })
        .await
    }

    pub async fn unpin_thread(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadUnpin {
            command_id: meta.command_id(),
            thread_id,
        })
        .await
    }

    pub async fn set_thread_auto_settle(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        enabled: bool,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadAutoSettleSet {
            command_id: meta.command_id(),
            thread_id,
            enabled,
        })
        .await
    }

    /// Fractional-index key that sorts between the drop position's neighbors.
    pub async fn reorder_pinned_thread(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        order_key: &str,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadPinReorder {
            command_id: meta.command_id(),
            thread_id,
            order_key: non_empty(order_key, "Order key")?,
        })
        .await
    }

    /// Fractional-index key that sorts between the drop position's neighbors.
    pub async fn reorder_active_thread(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        order_key: &str,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadActiveReorder {
            command_id: meta.command_id(),
            thread_id,
            order_key: non_empty(order_key, "Order key")?,
        })
        .await
    }

    pub async fn unsettle_thread(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadUnsettle {
            command_id: meta.command_id(),
            thread_id,
            reason: UserUnsettleReason,
        })
        .await
    }

    pub async fn snooze_thread(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        snoozed_until: String,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadSnooze {
            command_id: meta.command_id(),
            thread_id,
            snoozed_until,
        })
        .await
    }

    pub async fn unsnooze_thread(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadUnsnooze {
            command_id: meta.command_id(),
            thread_id,
            reason: UserUnsettleReason,
        })
        .await
    }

    /// `visited_at`: watermark of the thread state the viewer has seen (ISO timestamp).
    pub async fn visit_thread(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        visited_at: String,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadVisit {
            command_id: meta.command_id(),
            thread_id,
            visited_at,
        })
        .await
    }

    pub async fn mark_thread_unread(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadMarkUnread {
            command_id: meta.command_id(),
            thread_id,
        })
        .await
    }

    pub async fn update_thread_metadata(
        &self,
        thread_id: ThreadId,
        input: UpdateThreadMetadataInput,
    ) -> Result<DispatchResult, CommandError> {
        let command_id = input.meta.command_id();
        let mut result = None;
        if input.title.is_some()
            || input.branch.is_some()
            || input.worktree_path.is_some()
            || input.regenerate_title.is_some()
            || input.linked_pull_request.is_some()
            || input.limit_recovery.is_some()
        {
            result = Some(
                self.dispatch(OrchestrationV2Command::ThreadMetadataUpdate {
                    command_id: command_id.clone(),
                    thread_id: thread_id.clone(),
                    title: input
                        .title
                        .as_deref()
                        .map(|t| non_empty(t, "Title"))
                        .transpose()?,
                    regenerate_title: input.regenerate_title,
                    branch: input
                        .branch
                        .map(|branch| branch.map(|b| non_empty(&b, "Branch")).transpose())
                        .transpose()?,
                    worktree_path: input
                        .worktree_path
                        .map(|path| path.map(|p| non_empty(&p, "Worktree path")).transpose())
                        .transpose()?,
                    expected_worktree_path: None,
                    expected_empty: input.expected_empty.then_some(true),
                    limit_recovery: input.limit_recovery,
                    linked_pull_request: input.linked_pull_request,
                })
                .await?,
            );
        }
        if let Some(model_selection) = input.model_selection {
            // The orchestrator resolves provider switching itself.
            result = Some(
                self.dispatch(OrchestrationV2Command::ThreadModelSelectionSet {
                    command_id: if result.is_none() {
                        command_id
                    } else {
                        CommandId::new(format!("{command_id}:model-selection"))
                            .map_err(|e| CommandError::Invalid(e.to_string()))?
                    },
                    thread_id,
                    model_selection,
                })
                .await?,
            );
        }
        Ok(result.unwrap_or(DispatchResult { sequence: 0 }))
    }

    pub async fn set_thread_runtime_mode(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        runtime_mode: RuntimeMode,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadRuntimeModeSet {
            command_id: meta.command_id(),
            thread_id,
            runtime_mode,
        })
        .await
    }

    pub async fn set_thread_interaction_mode(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        interaction_mode: ProviderInteractionMode,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadInteractionModeSet {
            command_id: meta.command_id(),
            thread_id,
            interaction_mode,
        })
        .await
    }

    /// Sends a user message: persists uploads, creates the thread first when it
    /// does not exist yet, then dispatches `message.dispatch`.
    pub async fn start_thread_turn(
        &self,
        input: StartThreadTurnInput,
    ) -> Result<DispatchResult, CommandError> {
        let command_id = input.meta.command_id();
        let attachments = persist_attachments(
            &*self.port,
            &input.thread_id,
            &input.message_id,
            &input.attachments,
        )
        .await?;
        let context = remap_composer_context_attachments(
            input.context.as_ref(),
            &input.attachments,
            &attachments,
        );

        // First-message launch: the thread is created, then the message lands on it.
        // (Worktree preparation is the hooks' job; see PORTING.md.)
        if let Some(create) = &input.create_thread {
            self.dispatch(OrchestrationV2Command::ThreadCreate {
                created_by: OrchestrationV2Actor::User,
                creation_source: input.meta.creation_source(),
                command_id: CommandId::new(format!("{command_id}:thread"))
                    .map_err(|e| CommandError::Invalid(e.to_string()))?,
                thread_id: input.thread_id.clone(),
                project_id: create.project_id.clone(),
                title: non_empty(
                    &input
                        .title_seed
                        .clone()
                        .unwrap_or_else(|| create.title.clone()),
                    "Thread title",
                )?,
                model_selection: input
                    .model_selection
                    .clone()
                    .unwrap_or_else(|| create.model_selection.clone()),
                runtime_mode: create.runtime_mode,
                interaction_mode: create.interaction_mode,
                branch: create
                    .branch
                    .as_deref()
                    .map(|b| non_empty(b, "Branch"))
                    .transpose()?,
                worktree_path: create
                    .worktree_path
                    .as_deref()
                    .map(|p| non_empty(p, "Worktree path"))
                    .transpose()?,
                imported_native_thread: None,
            })
            .await?;
        }

        let requested_mode = input.dispatch_mode.unwrap_or_default();
        let source_plan_ref = input
            .source_proposed_plan
            .as_ref()
            .map(|(thread_id, plan_id)| OrchestrationV2SourcePlanRef {
                thread_id: thread_id.clone(),
                plan_id: plan_id.clone(),
            });
        let (dispatch_mode, delivery_intent) = if requested_mode == DispatchModeRequest::Start {
            (OrchestrationV2DispatchMode::StartImmediately, None)
        } else {
            (
                if requested_mode == DispatchModeRequest::Queue {
                    OrchestrationV2DispatchMode::QueueAfterActive
                } else {
                    OrchestrationV2DispatchMode::StartImmediately
                },
                match requested_mode {
                    DispatchModeRequest::Auto => Some(OrchestrationV2DeliveryIntent::Auto),
                    DispatchModeRequest::Steer => Some(OrchestrationV2DeliveryIntent::Steer),
                    DispatchModeRequest::Restart => Some(OrchestrationV2DeliveryIntent::Restart),
                    DispatchModeRequest::Queue | DispatchModeRequest::Start => None,
                },
            )
        };
        let title_seed = if requested_mode == DispatchModeRequest::Start {
            None
        } else {
            input
                .title_seed
                .as_deref()
                .map(|seed| non_empty(seed, "Title seed"))
                .transpose()?
        };
        self.dispatch(OrchestrationV2Command::MessageDispatch {
            notification: None,
            created_by: OrchestrationV2Actor::User,
            creation_source: input.meta.creation_source(),
            scheduled_task_id: None,
            sender_thread_id: None,
            command_id,
            thread_id: input.thread_id,
            message_id: input.message_id,
            text: input.text,
            context,
            attachments,
            title_seed,
            model_selection: input.model_selection,
            source_plan_ref,
            restart_continuation_of_run_id: None,
            usage_limit_continuation_of_run_id: None,
            manual_continuation_of_run_id: input.manual_continuation_of_run_id,
            usage_limit_recovery_request_id: None,
            delivery_intent,
            delegated_completion: None,
            dispatch_mode,
        })
        .await
    }

    /// Stop for the thread's latest work: interrupts the active run, or the settled run whose
    /// background work still runs. With no run to stop, it ends the thread's pull request
    /// watches, the only background work that has no run.
    pub async fn interrupt_thread_turn(
        &self,
        thread_id: ThreadId,
        input: InterruptThreadTurnInput,
    ) -> Result<DispatchResult, CommandError> {
        if let Some(subagent_id) = &input.subagent_id {
            let projection: ThreadProjection =
                self.port.get_thread_projection(&thread_id).await?.into();
            let Some(subagent) = projection
                .subagents
                .iter()
                .find(|agent| agent.id == *subagent_id)
            else {
                return Ok(DispatchResult { sequence: 0 });
            };
            let Some(run_id) = subagent.run_id.clone() else {
                return Ok(DispatchResult { sequence: 0 });
            };
            return self
                .dispatch(OrchestrationV2Command::SubagentStop {
                    command_id: input.meta.command_id(),
                    thread_id,
                    run_id,
                    subagent_id: subagent.id.clone(),
                })
                .await;
        }
        let mut run_id = input.run_id.clone();
        if run_id.is_none() {
            let projection: ThreadProjection =
                self.port.get_thread_projection(&thread_id).await?.into();
            run_id = resolve_active_thread_run(&projection.runs).map(|run| run.id.clone());
            if run_id.is_none() {
                let latest_run = projection.runs.last();
                let pending = derive_pending_background_work(&DerivePendingBackgroundWorkInput {
                    latest_run: latest_run.map(|run| &**run),
                    provider_threads: &projection.provider_threads,
                    turn_items: &projection.turn_items,
                    active_provider_thread_id: projection.thread.active_provider_thread_id.as_ref(),
                    has_active_run: None,
                    runs: Some(&projection.runs),
                    pull_requests: None,
                });
                if !pending.is_empty() {
                    run_id = latest_run.map(|run| run.id.clone());
                }
            }
            if run_id.is_none() {
                let mut result = DispatchResult { sequence: 0 };
                let first_command_id = input.meta.command_id();
                let links = projection.thread.pull_requests.clone().unwrap_or_default();
                for link in visible_thread_pull_requests(&links) {
                    if link.watch.is_none() {
                        continue;
                    }
                    result = self
                        .dispatch(OrchestrationV2Command::ThreadPullRequestWatch {
                            command_id: if result.sequence == 0 {
                                first_command_id.clone()
                            } else {
                                CommandId::random()
                            },
                            thread_id: thread_id.clone(),
                            host: link.host.clone(),
                            repository: link.repository.clone(),
                            number: link.number,
                            watching: false,
                            link: None,
                        })
                        .await?;
                }
                return Ok(result);
            }
        }
        let Some(run_id) = run_id else {
            return Ok(DispatchResult { sequence: 0 });
        };
        self.dispatch(OrchestrationV2Command::RunInterrupt {
            command_id: input.meta.command_id(),
            thread_id,
            run_id,
            reason: None,
            hold_queue: Some(true),
        })
        .await
    }

    pub async fn respond_to_thread_approval(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        request_id: RuntimeRequestId,
        decision: ProviderApprovalDecision,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::RuntimeRequestRespond {
            command_id: meta.command_id(),
            thread_id,
            request_id,
            decision: Some(decision),
            answers: None,
            attachments_by_question_id: None,
        })
        .await
    }

    pub async fn respond_to_thread_user_input(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        request_id: RuntimeRequestId,
        answers: ProviderUserInputAnswers,
        attachments_by_question_id: Option<UserInputAttachments>,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::RuntimeRequestRespond {
            command_id: meta.command_id(),
            thread_id,
            request_id,
            decision: None,
            answers: Some(answers),
            attachments_by_question_id,
        })
        .await
    }

    pub async fn dismiss_thread_user_input(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        request_id: RuntimeRequestId,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadUserInputDismiss {
            command_id: meta.command_id(),
            thread_id,
            request_id,
        })
        .await
    }

    pub async fn revert_thread_checkpoint(
        &self,
        thread_id: ThreadId,
        input: RevertThreadCheckpointInput,
    ) -> Result<DispatchResult, CommandError> {
        if let (Some(checkpoint_id), Some(scope_id)) = (&input.checkpoint_id, &input.scope_id) {
            return self
                .dispatch(OrchestrationV2Command::CheckpointRollback {
                    restore_files: input.restore_files,
                    command_id: input.meta.command_id(),
                    thread_id,
                    scope_id: CheckpointScopeId::new(scope_id)
                        .map_err(|e| CommandError::Invalid(e.to_string()))?,
                    checkpoint_id: CheckpointId::new(checkpoint_id)
                        .map_err(|e| CommandError::Invalid(e.to_string()))?,
                })
                .await;
        }
        // Older callers name the target by run ordinal; look it up locally.
        let projection: ThreadProjection =
            self.port.get_thread_projection(&thread_id).await?.into();
        let by_id = projection.checkpoints.iter().find(|candidate| {
            input.checkpoint_id.as_deref() == Some(candidate.id.as_str())
                && input.scope_id.as_deref() == Some(candidate.scope_id.as_str())
        });
        let checkpoint = by_id.or_else(|| {
            projection.checkpoints.iter().rfind(|candidate| {
                if input.turn_count == Some(0) {
                    candidate.ordinal_within_scope == 0 && candidate.app_run_ordinal.is_none()
                } else {
                    candidate.app_run_ordinal.map(|ordinal| ordinal.get()) == input.turn_count
                        && input.turn_count.is_some()
                }
            })
        });
        let Some(checkpoint) = checkpoint.filter(|checkpoint| {
            checkpoint.status == crate::state::contracts::OrchestrationV2CheckpointStatus::Ready
        }) else {
            let target = match &input.checkpoint_id {
                None => format!(
                    "run ordinal {}",
                    input
                        .turn_count
                        .map_or("unknown".to_owned(), |count| count.to_string())
                ),
                Some(id) => format!("checkpoint {id}"),
            };
            return Err(CommandError::CheckpointUnavailable { thread_id, target });
        };
        self.dispatch(OrchestrationV2Command::CheckpointRollback {
            restore_files: input.restore_files,
            command_id: input.meta.command_id(),
            thread_id,
            scope_id: checkpoint.scope_id.clone(),
            checkpoint_id: checkpoint.id.clone(),
        })
        .await
    }

    pub async fn stop_thread_session(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
    ) -> Result<DispatchResult, CommandError> {
        let projection: ThreadProjection =
            self.port.get_thread_projection(&thread_id).await?.into();
        let command_id = meta.command_id();
        let mut result = DispatchResult { sequence: 0 };
        for session in projection.provider_sessions.iter() {
            result = self
                .dispatch(OrchestrationV2Command::ProviderSessionDetach {
                    command_id: CommandId::new(format!("{command_id}:detach:{}", session.id))
                        .map_err(|e| CommandError::Invalid(e.to_string()))?,
                    thread_id: thread_id.clone(),
                    provider_session_id: session.id.clone(),
                    reason: Some("client-requested".to_owned()),
                })
                .await?;
        }
        Ok(result)
    }

    pub async fn fork_thread_from_run(
        &self,
        meta: CommandMetadata,
        source_thread_id: ThreadId,
        target_thread_id: ThreadId,
        run_id: RunId,
        title: Option<String>,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadFork {
            created_by: OrchestrationV2Actor::User,
            creation_source: meta.creation_source(),
            command_id: meta.command_id(),
            source_thread_id,
            target_thread_id,
            source_point: OrchestrationV2ThreadForkSourcePoint::Run { run_id },
            title: title
                .as_deref()
                .map(|t| non_empty(t, "Title"))
                .transpose()?,
            created_at: None,
        })
        .await
    }

    pub async fn merge_thread_back(
        &self,
        meta: CommandMetadata,
        source_thread_id: ThreadId,
        target_thread_id: ThreadId,
        run_id: RunId,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadMergeBack {
            created_by: OrchestrationV2Actor::User,
            creation_source: meta.creation_source(),
            command_id: meta.command_id(),
            source_thread_id,
            target_thread_id,
            source_point: OrchestrationV2ThreadForkSourcePoint::Run { run_id },
            created_at: None,
        })
        .await
    }

    pub async fn resume_thread_queue(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::QueueResume {
            command_id: meta.command_id(),
            thread_id,
        })
        .await
    }

    pub async fn reorder_queued_run(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        run_id: RunId,
        before_run_id: Option<RunId>,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::QueuedRunReorder {
            command_id: meta.command_id(),
            thread_id,
            run_id,
            before_run_id,
        })
        .await
    }

    pub async fn promote_queued_run(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        queued_run_id: RunId,
        target_run_id: RunId,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::QueuedMessagePromoteToSteer {
            command_id: meta.command_id(),
            thread_id,
            queued_run_id,
            target_run_id,
        })
        .await
    }

    pub async fn cancel_queued_run(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        run_id: RunId,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::QueuedRunCancel {
            command_id: meta.command_id(),
            thread_id,
            run_id,
        })
        .await
    }

    pub async fn retry_workspace_preparation(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        run_id: RunId,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::PreparedRunRetry {
            command_id: meta.command_id(),
            thread_id,
            run_id,
        })
        .await
    }

    /// `edit`: a full replacement attachment list for the queued message.
    /// Omitted = text-only edit that leaves attachments untouched.
    pub async fn edit_queued_run(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        run_id: RunId,
        text: String,
        edit: Option<EditQueuedRunEdit>,
    ) -> Result<DispatchResult, CommandError> {
        let attachments = match &edit {
            None => None,
            Some(edit) => Some(
                persist_attachments(&*self.port, &thread_id, &edit.message_id, &edit.attachments)
                    .await?,
            ),
        };
        let context = match (&edit, &attachments) {
            (Some(edit), Some(attachments)) if edit.context.is_some() => {
                remap_composer_context_attachments(
                    edit.context.as_ref(),
                    &edit.attachments,
                    attachments,
                )
            }
            _ => None,
        };
        self.dispatch(OrchestrationV2Command::QueuedRunEdit {
            context,
            command_id: meta.command_id(),
            thread_id,
            run_id,
            text,
            attachments,
        })
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn link_thread_pull_request(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        host: &str,
        repository: &str,
        number: u64,
        url: &str,
        source: ThreadPullRequestLinkSource,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadPullRequestLink {
            command_id: meta.command_id(),
            thread_id,
            host: non_empty(host, "Host")?,
            repository: non_empty(repository, "Repository")?,
            number: crate::state::contracts::PositiveInt::new(number).ok_or_else(|| {
                CommandError::Invalid("A pull request number is required.".to_owned())
            })?,
            url: non_empty(url, "URL")?,
            source,
        })
        .await
    }

    pub async fn unlink_thread_pull_request(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        host: &str,
        repository: &str,
        number: u64,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadPullRequestUnlink {
            command_id: meta.command_id(),
            thread_id,
            host: non_empty(host, "Host")?,
            repository: non_empty(repository, "Repository")?,
            number: crate::state::contracts::PositiveInt::new(number).ok_or_else(|| {
                CommandError::Invalid("A pull request number is required.".to_owned())
            })?,
        })
        .await
    }

    pub async fn watch_thread_pull_request(
        &self,
        meta: CommandMetadata,
        thread_id: ThreadId,
        host: &str,
        repository: &str,
        number: u64,
        watching: bool,
    ) -> Result<DispatchResult, CommandError> {
        self.dispatch(OrchestrationV2Command::ThreadPullRequestWatch {
            command_id: meta.command_id(),
            thread_id,
            host: non_empty(host, "Host")?,
            repository: non_empty(repository, "Repository")?,
            number: crate::state::contracts::PositiveInt::new(number).ok_or_else(|| {
                CommandError::Invalid("A pull request number is required.".to_owned())
            })?,
            watching,
            link: None,
        })
        .await
    }
}

fn build_project_update(
    input: &UpdateProjectInput,
    project_id: ProjectId,
) -> Result<ProjectMutation, CommandError> {
    Ok(ProjectMutation::ProjectUpdate {
        command_id: input.meta.command_id(),
        project_id,
        title: input
            .title
            .as_deref()
            .map(|t| non_empty(t, "Project title"))
            .transpose()?,
        workspace_root: input
            .workspace_root
            .as_deref()
            .map(|r| non_empty(r, "Workspace root"))
            .transpose()?,
        default_model_selection: input.default_model_selection.clone(),
        auto_pull: input.auto_pull,
        project_icon: input.project_icon.clone(),
        favicon_path: input
            .favicon_path
            .clone()
            .map(|path| path.map(|p| non_empty(&p, "Favicon path")).transpose())
            .transpose()?,
        default_thread_env_mode: input.default_thread_env_mode,
        scripts: input.scripts.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fixtures::{from_json, v2_projection};
    use parking_lot::Mutex;
    use serde_json::{Value, json};

    const NOW: &str = "2026-06-20T00:00:00.000Z";

    #[derive(Default)]
    struct Recorder {
        commands: Mutex<Vec<Value>>,
        projects: Mutex<Vec<Value>>,
        projection: Mutex<Option<OrchestrationV2ThreadProjection>>,
        projection_requests: Mutex<Vec<ThreadId>>,
        persisted: Mutex<Vec<(ThreadId, MessageId, usize)>>,
    }

    #[async_trait]
    impl CommandPort for Recorder {
        async fn dispatch(
            &self,
            command: OrchestrationV2Command,
        ) -> Result<DispatchResult, CommandError> {
            let mut commands = self.commands.lock();
            commands.push(serde_json::to_value(&command).unwrap());
            Ok(DispatchResult {
                sequence: commands.len() as u64,
            })
        }

        async fn project_mutate(&self, mutation: ProjectMutation) -> Result<(), CommandError> {
            self.projects
                .lock()
                .push(serde_json::to_value(&mutation).unwrap());
            Ok(())
        }

        async fn get_thread_projection(
            &self,
            thread_id: &ThreadId,
        ) -> Result<OrchestrationV2ThreadProjection, CommandError> {
            self.projection_requests.lock().push(thread_id.clone());
            Ok(self.projection.lock().clone().unwrap_or_else(v2_projection))
        }

        async fn persist_chat_attachments(
            &self,
            thread_id: &ThreadId,
            message_id: &MessageId,
            attachments: Vec<UploadChatAttachment>,
        ) -> Result<Vec<ChatAttachment>, CommandError> {
            self.persisted
                .lock()
                .push((thread_id.clone(), message_id.clone(), attachments.len()));
            Ok(attachments
                .iter()
                .enumerate()
                .map(|(index, upload)| {
                    from_json(json!({
                        "type": "image", "id": format!("stored-{index}"),
                        "name": upload.name.as_str(), "mimeType": upload.mime_type.as_str(),
                        "sizeBytes": upload.size_bytes,
                    }))
                })
                .collect())
        }
    }

    fn setup() -> (Arc<Recorder>, EnvironmentCommands) {
        let recorder = Arc::new(Recorder::default());
        (Arc::clone(&recorder), EnvironmentCommands::new(recorder))
    }

    fn meta(command_id: &str) -> CommandMetadata {
        CommandMetadata {
            command_id: Some(CommandId::new(command_id).unwrap()),
            ..CommandMetadata::default()
        }
    }

    fn thread() -> ThreadId {
        ThreadId::new("thread-v2").unwrap()
    }

    fn model() -> ModelSelection {
        from_json(json!({ "instanceId": "codex", "model": "gpt-5.4" }))
    }

    fn turn_input(command_id: &str) -> StartThreadTurnInput {
        StartThreadTurnInput {
            meta: meta(command_id),
            thread_id: thread(),
            manual_continuation_of_run_id: None,
            message_id: MessageId::new("message-1").unwrap(),
            text: "Implement the plan".into(),
            attachments: vec![],
            context: None,
            model_selection: Some(model()),
            title_seed: Some("Implement the plan".into()),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: ProviderInteractionMode::Default,
            create_thread: None,
            source_proposed_plan: None,
            dispatch_mode: None,
        }
    }

    #[tokio::test]
    async fn routes_projects_through_the_event_sourced_project_transport() {
        let (recorder, commands) = setup();
        commands
            .create_project(CreateProjectInput {
                meta: meta("command-1"),
                project_id: ProjectId::new("project-1").unwrap(),
                title: "Project".into(),
                workspace_root: "/workspace/project".into(),
                create_workspace_root_if_missing: Some(true),
                default_model_selection: None,
                scripts: None,
            })
            .await
            .unwrap();
        assert_eq!(
            *recorder.projects.lock(),
            vec![json!({
                "type": "project.create", "commandId": "command-1", "projectId": "project-1",
                "title": "Project", "workspaceRoot": "/workspace/project",
                "createWorkspaceRootIfMissing": true,
            })]
        );
    }

    #[tokio::test]
    async fn persists_and_clears_project_presentation_and_environment_settings() {
        let (recorder, commands) = setup();
        let project_id = ProjectId::new("project-1").unwrap();
        commands
            .update_project(UpdateProjectInput {
                meta: meta("c1"),
                project_id: Some(project_id.clone()),
                auto_pull: Some(true),
                project_icon: Some(Some(from_json(json!({ "kind": "emoji", "emoji": "🌲" })))),
                favicon_path: Some(Some("/workspace/project/icon.png".into())),
                default_thread_env_mode: Some(Some(from_json(json!("worktree")))),
                ..UpdateProjectInput::default()
            })
            .await
            .unwrap();
        commands
            .update_project(UpdateProjectInput {
                meta: meta("c2"),
                project_id: Some(project_id),
                auto_pull: Some(false),
                project_icon: Some(None),
                favicon_path: Some(None),
                default_thread_env_mode: Some(None),
                ..UpdateProjectInput::default()
            })
            .await
            .unwrap();
        assert_eq!(
            *recorder.projects.lock(),
            vec![
                json!({
                    "type": "project.update", "commandId": "c1", "projectId": "project-1",
                    "autoPull": true, "projectIcon": { "kind": "emoji", "emoji": "🌲" },
                    "faviconPath": "/workspace/project/icon.png", "defaultThreadEnvMode": "worktree",
                }),
                json!({
                    "type": "project.update", "commandId": "c2", "projectId": "project-1",
                    "autoPull": false, "projectIcon": null, "faviconPath": null,
                    "defaultThreadEnvMode": null,
                }),
            ]
        );
    }

    #[tokio::test]
    async fn preserves_caller_command_ids_for_idempotent_commands() {
        let (recorder, commands) = setup();
        commands
            .archive_thread(meta("queued-command"), ThreadId::new("thread-1").unwrap())
            .await
            .unwrap();
        assert_eq!(
            *recorder.commands.lock(),
            vec![
                json!({ "type": "thread.archive", "commandId": "queued-command", "threadId": "thread-1" })
            ]
        );
    }

    #[tokio::test]
    async fn resolves_run_ordinal_zero_to_the_persisted_thread_start_checkpoint() {
        let (recorder, commands) = setup();
        let mut projection = v2_projection();
        projection.checkpoints = vec![from_json(json!({
            "id": "checkpoint-thread-start", "threadId": "thread-v2", "scopeId": "checkpoint-scope-root",
            "runId": null, "nodeId": "node-run-1", "parentCheckpointId": null, "ordinalWithinScope": 0,
            "appRunOrdinal": null, "ref": "refs/t3/thread-start", "status": "ready", "files": [],
            "capturedAt": NOW,
        }))];
        *recorder.projection.lock() = Some(projection);
        commands
            .revert_thread_checkpoint(
                thread(),
                RevertThreadCheckpointInput {
                    meta: meta("rollback-thread-start"),
                    turn_count: Some(0),
                    ..RevertThreadCheckpointInput::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            *recorder.commands.lock(),
            vec![json!({
                "type": "checkpoint.rollback", "commandId": "rollback-thread-start",
                "threadId": "thread-v2", "scopeId": "checkpoint-scope-root",
                "checkpointId": "checkpoint-thread-start",
            })]
        );
    }

    #[tokio::test]
    async fn an_unavailable_rollback_target_is_reported() {
        let (_recorder, commands) = setup();
        let error = commands
            .revert_thread_checkpoint(
                thread(),
                RevertThreadCheckpointInput {
                    turn_count: Some(3),
                    ..RevertThreadCheckpointInput::default()
                },
            )
            .await
            .unwrap_err();
        assert_eq!(
            error,
            CommandError::CheckpointUnavailable {
                thread_id: thread(),
                target: "run ordinal 3".into()
            }
        );
    }

    #[tokio::test]
    async fn identified_checkpoints_roll_back_without_fetching_the_projection() {
        let (recorder, commands) = setup();
        commands
            .revert_thread_checkpoint(
                thread(),
                RevertThreadCheckpointInput {
                    meta: meta("rollback"),
                    restore_files: Some(true),
                    checkpoint_id: Some("checkpoint-1".into()),
                    scope_id: Some("scope-1".into()),
                    ..RevertThreadCheckpointInput::default()
                },
            )
            .await
            .unwrap();
        assert!(recorder.projection_requests.lock().is_empty());
        assert_eq!(
            *recorder.commands.lock(),
            vec![json!({
                "type": "checkpoint.rollback", "restoreFiles": true, "commandId": "rollback",
                "threadId": "thread-v2", "scopeId": "scope-1", "checkpointId": "checkpoint-1",
            })]
        );
    }

    #[tokio::test]
    async fn preserves_plan_implementation_provenance_on_runs() {
        let (recorder, commands) = setup();
        commands
            .start_thread_turn(StartThreadTurnInput {
                source_proposed_plan: Some((
                    ThreadId::new("thread-plan").unwrap(),
                    PlanId::new("plan-1").unwrap(),
                )),
                ..turn_input("implement-plan")
            })
            .await
            .unwrap();
        let sent = recorder.commands.lock();
        assert_eq!(sent.len(), 1);
        let command = &sent[0];
        assert_eq!(command["type"], "message.dispatch");
        assert_eq!(command["commandId"], "implement-plan");
        assert_eq!(command["threadId"], "thread-v2");
        assert_eq!(command["titleSeed"], "Implement the plan");
        assert_eq!(
            command["sourcePlanRef"],
            json!({ "threadId": "thread-plan", "planId": "plan-1" })
        );
        assert_eq!(command["deliveryIntent"], "auto");
        assert_eq!(
            command["dispatchMode"],
            json!({ "type": "start_immediately" })
        );
    }

    #[tokio::test]
    async fn uses_server_resolved_delivery_intent_without_fetching_the_projection() {
        let (recorder, commands) = setup();
        for (mode, intent, dispatch_mode) in [
            (
                DispatchModeRequest::Steer,
                Some("steer"),
                "start_immediately",
            ),
            (
                DispatchModeRequest::Restart,
                Some("restart"),
                "start_immediately",
            ),
            (DispatchModeRequest::Queue, None, "queue_after_active"),
            (DispatchModeRequest::Start, None, "start_immediately"),
        ] {
            commands
                .start_thread_turn(StartThreadTurnInput {
                    dispatch_mode: Some(mode),
                    ..turn_input("send")
                })
                .await
                .unwrap();
            let sent = recorder.commands.lock();
            let command = sent.last().unwrap();
            assert_eq!(
                command.get("deliveryIntent").and_then(Value::as_str),
                intent,
                "{mode:?}"
            );
            assert_eq!(
                command["dispatchMode"],
                json!({ "type": dispatch_mode }),
                "{mode:?}"
            );
            // An explicit start never sends a title seed.
            assert_eq!(
                command.get("titleSeed").is_some(),
                mode != DispatchModeRequest::Start,
                "{mode:?}"
            );
        }
        assert!(recorder.projection_requests.lock().is_empty());
    }

    #[tokio::test]
    async fn creates_the_thread_first_for_a_first_message() {
        let (recorder, commands) = setup();
        commands
            .start_thread_turn(StartThreadTurnInput {
                create_thread: Some(StartThreadBootstrapCreate {
                    project_id: ProjectId::new("project-1").unwrap(),
                    title: "Draft".into(),
                    model_selection: model(),
                    runtime_mode: RuntimeMode::FullAccess,
                    interaction_mode: ProviderInteractionMode::Default,
                    branch: Some("feature".into()),
                    worktree_path: Some("/worktrees/feature".into()),
                    created_at: NOW.into(),
                }),
                ..turn_input("first")
            })
            .await
            .unwrap();
        let sent = recorder.commands.lock();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0]["type"], "thread.create");
        assert_eq!(sent[0]["commandId"], "first:thread");
        assert_eq!(sent[0]["projectId"], "project-1");
        assert_eq!(sent[0]["branch"], "feature");
        assert_eq!(sent[0]["worktreePath"], "/worktrees/feature");
        assert_eq!(sent[1]["type"], "message.dispatch");
        assert_eq!(sent[1]["commandId"], "first");
    }

    #[tokio::test]
    async fn persists_uploads_and_remaps_context_attachment_ids() {
        let (recorder, commands) = setup();
        let upload: UploadChatAttachment = from_json(json!({
            "type": "image", "id": "draft-image", "name": "shot.png", "mimeType": "image/png",
            "sizeBytes": 4, "dataUrl": "data:image/png;base64,AAAA",
        }));
        let context: OrchestrationMessageContext = from_json(json!({
            "version": 1,
            "records": [{
                "version": 1, "contextId": "ctx-1", "label": "shot", "kind": "image",
                "attachmentId": "draft-image", "name": "shot.png", "mimeType": "image/png", "sizeBytes": 4,
            }],
        }));
        commands
            .start_thread_turn(StartThreadTurnInput {
                attachments: vec![StartThreadAttachment::Upload(upload)],
                context: Some(context),
                ..turn_input("with-image")
            })
            .await
            .unwrap();
        assert_eq!(recorder.persisted.lock().len(), 1);
        let sent = recorder.commands.lock();
        assert_eq!(sent[0]["attachments"][0]["id"], "stored-0");
        assert_eq!(sent[0]["context"]["records"][0]["attachmentId"], "stored-0");
    }

    #[tokio::test]
    async fn interrupts_a_known_run_without_fetching_the_projection() {
        let (recorder, commands) = setup();
        commands
            .interrupt_thread_turn(
                thread(),
                InterruptThreadTurnInput {
                    meta: meta("stop"),
                    run_id: Some(RunId::new("run-1").unwrap()),
                    subagent_id: None,
                },
            )
            .await
            .unwrap();
        assert!(recorder.projection_requests.lock().is_empty());
        assert_eq!(
            *recorder.commands.lock(),
            vec![json!({
                "type": "run.interrupt", "commandId": "stop", "threadId": "thread-v2",
                "runId": "run-1", "holdQueue": true,
            })]
        );
    }

    #[tokio::test]
    async fn stop_finds_the_active_run_in_the_projection() {
        let (recorder, commands) = setup();
        let mut projection = v2_projection();
        projection.runs = vec![crate::state::test_fixtures::v2_run(
            "run-active",
            1,
            "running",
        )];
        *recorder.projection.lock() = Some(projection);
        commands
            .interrupt_thread_turn(thread(), InterruptThreadTurnInput::default())
            .await
            .unwrap();
        assert_eq!(recorder.commands.lock()[0]["runId"], "run-active");
    }

    #[tokio::test]
    async fn stop_with_no_run_left_ends_the_threads_pull_request_watches() {
        let (recorder, commands) = setup();
        let link = |number: u64, source: &str, watched: bool| {
            let mut value = json!({
                "host": "github.com", "repository": "acme/app", "number": number,
                "url": format!("https://github.com/acme/app/pull/{number}"),
                "source": source, "linkedAt": NOW, "snapshot": null, "stack": null,
            });
            if watched {
                value["watch"] = json!({
                    "startedAt": NOW, "headSha": null, "failedChecks": [], "passed": false,
                    "passedChecks": [], "remarksThrough": NOW, "remarkIds": [], "conflicting": false, "wakes": 0,
                });
            }
            value
        };
        let mut projection = v2_projection();
        projection.thread.pull_requests = Some(vec![
            from_json(link(1, "agent", true)),
            from_json(link(2, "agent", false)),
            from_json(link(3, "stack-dismissed", true)),
        ]);
        *recorder.projection.lock() = Some(projection);
        let result = commands
            .interrupt_thread_turn(
                thread(),
                InterruptThreadTurnInput {
                    meta: meta("stop"),
                    ..InterruptThreadTurnInput::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(result.sequence, 1);
        let sent = recorder.commands.lock();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["type"], "thread.pull-request.watch");
        assert_eq!(sent[0]["commandId"], "stop");
        assert_eq!(sent[0]["number"], 1);
        assert_eq!(sent[0]["watching"], false);
    }

    #[tokio::test]
    async fn delegates_model_selection_to_the_orchestrator_without_fetching_the_projection() {
        let (recorder, commands) = setup();
        commands
            .update_thread_metadata(
                thread(),
                UpdateThreadMetadataInput {
                    meta: meta("select"),
                    model_selection: Some(model()),
                    ..UpdateThreadMetadataInput::default()
                },
            )
            .await
            .unwrap();
        assert!(recorder.projection_requests.lock().is_empty());
        assert_eq!(
            *recorder.commands.lock(),
            vec![json!({
                "type": "thread.model-selection.set", "commandId": "select", "threadId": "thread-v2",
                "modelSelection": { "instanceId": "codex", "model": "gpt-5.4" },
            })]
        );
    }

    #[tokio::test]
    async fn a_metadata_edit_and_a_model_change_get_distinct_command_ids() {
        let (recorder, commands) = setup();
        commands
            .update_thread_metadata(
                thread(),
                UpdateThreadMetadataInput {
                    meta: meta("both"),
                    title: Some("Renamed".into()),
                    model_selection: Some(model()),
                    ..UpdateThreadMetadataInput::default()
                },
            )
            .await
            .unwrap();
        {
            let sent = recorder.commands.lock();
            assert_eq!(sent.len(), 2);
            assert_eq!(
                (sent[0]["type"].as_str(), sent[0]["commandId"].as_str()),
                (Some("thread.metadata.update"), Some("both"))
            );
            assert_eq!(sent[1]["commandId"], "both:model-selection");
        }
        // Nothing to send is a zero-sequence no-op.
        let none = commands
            .update_thread_metadata(thread(), UpdateThreadMetadataInput::default())
            .await
            .unwrap();
        assert_eq!(none.sequence, 0);
    }

    #[tokio::test]
    async fn dispatches_settle_and_unsettle_commands_without_timestamps() {
        let (recorder, commands) = setup();
        commands
            .settle_thread(meta("settle"), thread())
            .await
            .unwrap();
        commands
            .unsettle_thread(meta("unsettle"), thread())
            .await
            .unwrap();
        assert_eq!(
            *recorder.commands.lock(),
            vec![
                json!({ "type": "thread.settle", "commandId": "settle", "threadId": "thread-v2" }),
                json!({ "type": "thread.unsettle", "commandId": "unsettle", "threadId": "thread-v2", "reason": "user" }),
            ]
        );
    }

    #[tokio::test]
    async fn sends_an_active_order_key_without_changing_activity_timestamps() {
        let (recorder, commands) = setup();
        commands
            .reorder_active_thread(meta("reorder"), thread(), "a0")
            .await
            .unwrap();
        assert_eq!(
            *recorder.commands.lock(),
            vec![
                json!({ "type": "thread.active.reorder", "commandId": "reorder", "threadId": "thread-v2", "orderKey": "a0" })
            ]
        );
    }

    #[tokio::test]
    async fn dismisses_a_pending_user_input_request() {
        let (recorder, commands) = setup();
        commands
            .dismiss_thread_user_input(
                meta("dismiss"),
                thread(),
                RuntimeRequestId::new("request-1").unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            *recorder.commands.lock(),
            vec![
                json!({ "type": "thread.user-input.dismiss", "commandId": "dismiss", "threadId": "thread-v2", "requestId": "request-1" })
            ]
        );
    }

    #[tokio::test]
    async fn queue_commands_carry_their_targets() {
        let (recorder, commands) = setup();
        let run = |id: &str| RunId::new(id).unwrap();
        commands
            .reorder_queued_run(meta("a"), thread(), run("r1"), Some(run("r0")))
            .await
            .unwrap();
        commands
            .promote_queued_run(meta("b"), thread(), run("q"), run("t"))
            .await
            .unwrap();
        commands
            .cancel_queued_run(meta("c"), thread(), run("r1"))
            .await
            .unwrap();
        commands
            .resume_thread_queue(meta("d"), thread())
            .await
            .unwrap();
        let sent = recorder.commands.lock();
        assert_eq!(
            sent[0],
            json!({ "type": "queued-run.reorder", "commandId": "a", "threadId": "thread-v2", "runId": "r1", "beforeRunId": "r0" })
        );
        assert_eq!(
            sent[1],
            json!({ "type": "queued-message.promote-to-steer", "commandId": "b", "threadId": "thread-v2", "queuedRunId": "q", "targetRunId": "t" })
        );
        assert_eq!(sent[2]["type"], "queued-run.cancel");
        assert_eq!(sent[3]["type"], "queue.resume");
    }

    #[tokio::test]
    async fn stopping_a_session_detaches_each_provider_session() {
        let (recorder, commands) = setup();
        let mut projection = v2_projection();
        projection.provider_sessions = ["session-a", "session-b"]
            .into_iter()
            .map(|id| {
                from_json(json!({
                    "id": id, "driver": "codex", "providerInstanceId": "codex", "status": "ready",
                    "cwd": "/workspace/project", "model": null,
                    "capabilities": serde_json::to_value(crate::state::test_fixtures::v2_capabilities(json!({}))).unwrap(),
                    "createdAt": NOW, "updatedAt": NOW, "lastError": null,
                }))
            })
            .collect();
        *recorder.projection.lock() = Some(projection);
        commands
            .stop_thread_session(meta("stop"), thread())
            .await
            .unwrap();
        let sent = recorder.commands.lock();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0]["commandId"], "stop:detach:session-a");
        assert_eq!(sent[1]["providerSessionId"], "session-b");
        assert_eq!(sent[1]["reason"], "client-requested");
    }
}
