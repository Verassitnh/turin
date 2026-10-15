//! Port of upstream/apps/server/src/orchestration-v2/ThreadManagementService.ts
//!
//! The single chokepoint in front of the orchestrator for commands that can
//! start or wake provider work, plus the thread-level operations built on it
//! (send, wait, interrupt, settle, stop).
//!
//! Not ported:
//! * the legacy V1 transcript importer hooks (`ensureLegacyTranscript`,
//!   `existingThreadIdsForCommand` stays as a pure helper): the importer is cut;
//! * the pure read pass-throughs (`getThreadHistoryPage`, `getTimelinePage`,
//!   `getThreadSnapshot`, `search*`, `stream*`): callers use the
//!   [`OrchestratorPort`] directly through [`ThreadManagementService::orchestrator`];
//! * `layerWithLegacyImporter`.
//!
//! Turin additions: `thread.create` runs the `thread.created` hooks before it
//! reaches the orchestrator (see `hooks_bridge`), replacing launch-into-worktree.

use super::command_receipt_store::CommandReceiptStore;
use super::dispatch_mode_limit::{
    DispatchModeLimit, DispatchModeRefusal, DispatchModes, exceeded_dispatch_mode_limit,
};
use super::event_sink::StreamOptions;
use super::hooks_bridge::OrchestrationHooks;
use super::orchestrator_port::{
    BoxError, DispatchResult, OrchestratorError, OrchestratorPort, RecordField, RecordFilter,
    ShellLocation, ShellSnapshotOptions, ThreadRecords,
};
use super::seams::{
    ProjectDirectory, ProviderSessionLookup, ServerSettingsSource, instance_enabled_for_project,
};
use super::thread_command_executor::ThreadCommandExecutor;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::{self, BoxStream};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;
use turin_contracts::chat_attachment::ChatAttachment;
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::orchestration_v2::{
    OrchestrationV2Actor, OrchestrationV2Command, OrchestrationV2ConversationMessage,
    OrchestrationV2CreationSource, OrchestrationV2DispatchMode, OrchestrationV2DomainEvent,
    OrchestrationV2InternalCommand, OrchestrationV2LineageRelationship, OrchestrationV2Run,
    OrchestrationV2RunStatus, OrchestrationV2ServerCommand, OrchestrationV2StopWorkflowError,
    OrchestrationV2StopWorkflowInput, OrchestrationV2StopWorkflowReason,
    OrchestrationV2SubagentOrigin, OrchestrationV2SubagentStatus, OrchestrationV2ThreadProjection,
    OrchestrationV2ThreadShell, OrchestrationV2TurnItem, OrchestrationV2UserMessageInputIntent,
};
use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::{
    CommandId, MessageId, ProjectId, RunId, ScheduledTaskId, ThreadId, TrimmedNonEmptyString,
};
use turin_hooks::{HookDispatchContext, HookEvent};
use turin_provider_core::provider_adapter::ProviderAdapterV2StopTaskInput;

/// `settleAfterRun` waits this long for the run to end; a turn that outlives
/// the wait leaves its thread active.
const SETTLE_AFTER_RUN_WAIT_MS: u64 = 24 * 60 * 60 * 1_000;
/// Bounded so a provider that never answers cannot hold the thread lock.
const STOP_TASK_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendMode {
    Auto,
    Queue,
    Steer,
    Restart,
}

/// `ThreadManagementProvenance`.
#[derive(Debug, Clone, Copy)]
pub struct Provenance {
    pub created_by: OrchestrationV2Actor,
    pub creation_source: OrchestrationV2CreationSource,
}

/// Stamps authoritative provenance on commands that create threads or
/// messages; every other command is returned unchanged.
pub fn with_creation_provenance(
    mut command: OrchestrationV2Command,
    provenance: Provenance,
) -> OrchestrationV2Command {
    match &mut command {
        OrchestrationV2Command::ThreadCreate {
            created_by,
            creation_source,
            ..
        }
        | OrchestrationV2Command::MessageDispatch {
            created_by,
            creation_source,
            ..
        }
        | OrchestrationV2Command::ThreadFork {
            created_by,
            creation_source,
            ..
        }
        | OrchestrationV2Command::ThreadMergeBack {
            created_by,
            creation_source,
            ..
        }
        | OrchestrationV2Command::DelegatedTaskRequest {
            created_by,
            creation_source,
            ..
        } => {
            *created_by = provenance.created_by;
            *creation_source = provenance.creation_source;
        }
        _ => {}
    }
    command
}

fn command_json(command: &OrchestrationV2ServerCommand) -> Value {
    serde_json::to_value(command).unwrap_or(Value::Null)
}

fn command_thread_field(value: &Value, name: &str) -> Option<ThreadId> {
    value
        .get(name)
        .and_then(Value::as_str)
        .and_then(|id| ThreadId::new(id).ok())
}

/// The threads that exist before a command runs and whose state it reads
/// (upstream used this to hydrate imported V1 transcripts first).
pub fn existing_thread_ids_for_command(command: &OrchestrationV2ServerCommand) -> Vec<ThreadId> {
    let value = command_json(command);
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    let field = |name: &str| command_thread_field(&value, name);
    let distinct = |first: Option<ThreadId>, second: Option<ThreadId>| {
        let mut ids: Vec<ThreadId> = first.into_iter().collect();
        if let Some(second) = second
            && !ids.contains(&second)
        {
            ids.push(second);
        }
        ids
    };
    match kind {
        "thread.create" => vec![],
        // Read-state commands only rewrite the thread payload's visited/unread
        // watermark; they never touch messages, so they do not need the
        // imported v1 transcript hydrated first. Visits fire on every
        // thread-activity bump while a thread is open.
        "thread.visit" | "thread.mark-unread" => vec![],
        "thread.fork" => field("sourceThreadId").into_iter().collect(),
        "thread.merge_back" => distinct(field("sourceThreadId"), field("targetThreadId")),
        "delegated_task.request"
        | "delegated_task.wake-policy"
        | "delegated_task.completion-delivery.acknowledge"
        | "delegated_task.completion-delivery.dispose" => {
            field("parentThreadId").into_iter().collect()
        }
        "thread.created.record" => distinct(field("parentThreadId"), field("targetThreadId")),
        _ => field("threadId").into_iter().collect(),
    }
}

pub fn is_active_run(run: &OrchestrationV2Run) -> bool {
    matches!(
        run.status,
        OrchestrationV2RunStatus::Preparing
            | OrchestrationV2RunStatus::Starting
            | OrchestrationV2RunStatus::Running
            | OrchestrationV2RunStatus::Waiting
    )
}

pub fn is_terminal_run_status(status: OrchestrationV2RunStatus) -> bool {
    matches!(
        status,
        OrchestrationV2RunStatus::Completed
            | OrchestrationV2RunStatus::Failed
            | OrchestrationV2RunStatus::Cancelled
            | OrchestrationV2RunStatus::Interrupted
            | OrchestrationV2RunStatus::RolledBack
    )
}

pub fn latest_run(runs: &[OrchestrationV2Run]) -> Option<&OrchestrationV2Run> {
    runs.iter().max_by_key(|run| run.ordinal)
}

/// The newest run in `run`'s restart continuation chain, or `run` itself.
fn latest_restart_continuation<'a>(
    runs: &'a [OrchestrationV2Run],
    run: &'a OrchestrationV2Run,
) -> &'a OrchestrationV2Run {
    let mut current = run;
    for _ in 0..runs.len() {
        let next = runs.iter().find(|candidate| {
            candidate.restart_continuation_of_run_id.as_ref() == Some(&current.id)
        });
        match next {
            Some(next) => current = next,
            None => break,
        }
    }
    current
}

pub fn latest_active_run(runs: &[OrchestrationV2Run]) -> Option<&OrchestrationV2Run> {
    runs.iter()
        .filter(|run| is_active_run(run))
        .max_by_key(|run| run.ordinal)
}

fn latest_steerable_run(
    projection: &OrchestrationV2ThreadProjection,
) -> Option<&OrchestrationV2Run> {
    projection
        .runs
        .iter()
        .filter(|run| {
            run.status == OrchestrationV2RunStatus::Running
                && run.active_attempt_id.as_ref().is_some_and(|attempt_id| {
                    projection.provider_turns.iter().any(|turn| {
                        turn.run_attempt_id.as_ref() == Some(attempt_id)
                            && turn.status
                                == turin_contracts::orchestration_v2::OrchestrationV2ProviderTurnStatus::Running
                    })
                })
        })
        .max_by_key(|run| run.ordinal)
}

/// `ThreadManagementError`.
#[derive(Debug, thiserror::Error)]
pub enum ThreadManagementError {
    #[error("Thread {thread_id} was not found in project {project_id}.")]
    ThreadNotFound {
        project_id: ProjectId,
        thread_id: ThreadId,
    },
    #[error("Run {run_id} does not belong to thread {thread_id}.")]
    RunNotFound { thread_id: ThreadId, run_id: RunId },
    #[error("Thread {thread_id} is archived and cannot receive messages.")]
    ThreadArchived { thread_id: ThreadId },
    #[error("Thread {thread_id} has no running turn that can be {}.", if *.mode == SendMode::Steer { "steered" } else { "restarted" })]
    NoSteerableRun { thread_id: ThreadId, mode: SendMode },
    #[error("Run {run_id} is not currently interruptible.")]
    ThreadNotInterruptible { thread_id: ThreadId, run_id: RunId },
    #[error("Unable to load thread {thread_id} in project {project_id}.")]
    ProjectionLoad {
        project_id: ProjectId,
        thread_id: ThreadId,
        #[source]
        cause: BoxError,
    },
    #[error("Unable to list threads in project {project_id}.")]
    ProjectThreadsList {
        project_id: ProjectId,
        #[source]
        cause: BoxError,
    },
    #[error(
        "Message {message_id} was accepted on thread {thread_id} without a durable run projection."
    )]
    DurableRunProjection {
        thread_id: ThreadId,
        message_id: MessageId,
    },
    #[error("Provider instance \"{instance_id}\" is disabled for project {project_id}.")]
    ProviderInstanceDisabled {
        project_id: ProjectId,
        instance_id: ProviderInstanceId,
    },
    #[error(transparent)]
    Orchestrator(#[from] OrchestratorError),
}

impl ThreadManagementError {
    /// `dispatchWasNotAccepted` for the failures `ThreadMessageIntake` sees:
    /// the dispatcher failed in receipt validation or planning (or the
    /// enablement chokepoint refused) before any commit.
    pub fn was_not_accepted(&self) -> bool {
        match self {
            Self::Orchestrator(error) => error.was_not_accepted(),
            Self::ProviderInstanceDisabled { .. } => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SendInput {
    pub project_id: ProjectId,
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub message_id: MessageId,
    pub scheduled_task_id: Option<ScheduledTaskId>,
    pub sender_thread_id: Option<ThreadId>,
    pub text: String,
    pub attachments: Vec<ChatAttachment>,
    pub model_selection: Option<ModelSelection>,
    pub mode: SendMode,
    pub created_by: OrchestrationV2Actor,
    pub creation_source: OrchestrationV2CreationSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Started,
    Queued,
    Steered,
    Restarted,
}

#[derive(Debug, Clone)]
pub struct SendResult {
    pub dispatch: DispatchResult,
    pub projection: ThreadRecords,
    pub message: OrchestrationV2ConversationMessage,
    pub run: OrchestrationV2Run,
    /// None for queued sends: the user turn item materializes when the queued
    /// turn starts.
    pub turn_item: Option<OrchestrationV2TurnItem>,
    pub delivery: Delivery,
}

#[derive(Debug, Clone)]
pub struct WaitInput {
    pub project_id: ProjectId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub struct WaitResult {
    pub thread_id: ThreadId,
    pub run: Option<OrchestrationV2Run>,
    pub timed_out: bool,
}

#[derive(Debug, Clone)]
pub struct InterruptInput {
    pub project_id: ProjectId,
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub enum InterruptResult {
    InterruptRequested {
        run: OrchestrationV2Run,
        dispatch: DispatchResult,
    },
    NoActiveRun,
    AlreadyTerminal {
        run: OrchestrationV2Run,
    },
}

#[derive(Debug, Clone)]
pub enum SettleOutcome {
    Settled { sequence: u64 },
    SettlesWhenTurnEnds,
}

#[derive(Debug, Clone)]
pub struct StopDelegatedTasksInput {
    pub thread_id: ThreadId,
    pub command_id: CommandId,
    pub reason: Option<String>,
}

pub struct ThreadManagementService {
    executor: ThreadCommandExecutor,
    orchestrator: Arc<dyn OrchestratorPort>,
    sessions: Arc<dyn ProviderSessionLookup>,
    settings: Arc<dyn ServerSettingsSource>,
    receipts: CommandReceiptStore,
    projects: Arc<dyn ProjectDirectory>,
    hooks: Arc<dyn OrchestrationHooks>,
    stop_task_timeout: Duration,
    /// Upstream forks `settleAfterRun` into the layer scope; here the service
    /// owns the tasks and aborts them when it drops.
    background: Mutex<JoinSet<()>>,
}

impl ThreadManagementService {
    pub fn new(
        executor: ThreadCommandExecutor,
        orchestrator: Arc<dyn OrchestratorPort>,
        sessions: Arc<dyn ProviderSessionLookup>,
        settings: Arc<dyn ServerSettingsSource>,
        receipts: CommandReceiptStore,
        projects: Arc<dyn ProjectDirectory>,
        hooks: Arc<dyn OrchestrationHooks>,
    ) -> Self {
        Self {
            executor,
            orchestrator,
            sessions,
            settings,
            receipts,
            projects,
            hooks,
            stop_task_timeout: STOP_TASK_TIMEOUT,
            background: Mutex::new(JoinSet::new()),
        }
    }

    /// Overrides the 15 s bound on a provider's `stopTask` (tests).
    pub fn with_stop_task_timeout(mut self, timeout: Duration) -> Self {
        self.stop_task_timeout = timeout;
        self
    }

    pub fn orchestrator(&self) -> &Arc<dyn OrchestratorPort> {
        &self.orchestrator
    }

    pub fn executor(&self) -> &ThreadCommandExecutor {
        &self.executor
    }

    /// Refuses a new turn on an instance the project has effectively disabled.
    /// A command that already has a receipt was let through before, so its
    /// retry replays instead of being refused under newer settings.
    pub async fn assert_provider_instance_enabled(
        &self,
        project_id: &ProjectId,
        command_id: &CommandId,
        instance_id: &ProviderInstanceId,
    ) -> Result<(), ThreadManagementError> {
        if self.has_receipt(command_id).await {
            return Ok(());
        }
        let settings = self.settings.get_settings().await.map_err(|cause| {
            // Upstream: `Effect.orDie`; settings reads are infallible in practice.
            ThreadManagementError::ProjectionLoad {
                project_id: project_id.clone(),
                thread_id: ThreadId::new("settings").expect("literal id"),
                cause,
            }
        })?;
        if instance_enabled_for_project(&settings, project_id, instance_id) {
            Ok(())
        } else {
            Err(ThreadManagementError::ProviderInstanceDisabled {
                project_id: project_id.clone(),
                instance_id: instance_id.clone(),
            })
        }
    }

    /// Effective enablement for candidate selection, without the
    /// receipt-replay exemption `assert_provider_instance_enabled` grants an
    /// accepted command.
    pub async fn is_provider_instance_enabled_for_project(
        &self,
        project_id: &ProjectId,
        instance_id: &ProviderInstanceId,
    ) -> bool {
        match self.settings.get_settings().await {
            Ok(settings) => instance_enabled_for_project(&settings, project_id, instance_id),
            Err(error) => {
                tracing::error!(%error, "could not read server settings");
                false
            }
        }
    }

    /// Whether `command_id` already has a receipt. A caller resolving a target
    /// ahead of a dispatch under this same command id can use it to skip
    /// project-enablement filtering: the dispatch will only replay the
    /// recorded result, never start anything new.
    pub async fn has_receipt(&self, command_id: &CommandId) -> bool {
        // Upstream: `orElseSucceed(() => Option.none())`.
        matches!(
            self.receipts.get_by_command_id(command_id.clone()).await,
            Ok(Some(_))
        )
    }

    // Every path that can start or wake a provider turn funnels through this
    // dispatch, so resolving and checking the target instance here (instead
    // of at each caller) is the one place "off" is enforced.
    async fn assert_dispatch_target_enabled(
        &self,
        command: &OrchestrationV2ServerCommand,
    ) -> Result<(), ThreadManagementError> {
        match command {
            OrchestrationV2ServerCommand::Client(OrchestrationV2Command::MessageDispatch {
                command_id,
                thread_id,
                model_selection,
                ..
            }) => {
                let projection = self
                    .orchestrator
                    .get_thread_records(thread_id, &[], &RecordFilter::default())
                    .await?;
                let selection = model_selection
                    .as_ref()
                    .unwrap_or(&projection.thread.model_selection);
                self.assert_provider_instance_enabled(
                    &projection.thread.project_id,
                    command_id,
                    &selection.instance_id,
                )
                .await
            }
            OrchestrationV2ServerCommand::Client(
                OrchestrationV2Command::DelegatedTaskRequest {
                    command_id,
                    parent_thread_id,
                    model_selection,
                    ..
                },
            ) => {
                let parent = self
                    .orchestrator
                    .get_thread_records(parent_thread_id, &[], &RecordFilter::default())
                    .await?;
                self.assert_provider_instance_enabled(
                    &parent.thread.project_id,
                    command_id,
                    &model_selection.instance_id,
                )
                .await
            }
            _ => Ok(()),
        }
    }

    /// Runs the `thread.created` hooks for a new thread and binds the
    /// workspace a blocking hook reported. A command id that already has a
    /// receipt is a retry and runs nothing.
    async fn bind_thread_workspace(
        &self,
        command: OrchestrationV2ServerCommand,
    ) -> OrchestrationV2ServerCommand {
        let OrchestrationV2ServerCommand::Client(OrchestrationV2Command::ThreadCreate {
            command_id,
            thread_id,
            project_id,
            title,
            model_selection,
            branch,
            worktree_path,
            ..
        }) = &command
        else {
            return command;
        };
        if self.has_receipt(command_id).await {
            return command;
        }
        let root = match self.projects.get(project_id).await {
            Ok(Some(project)) => PathBuf::from(project.workspace_root),
            Ok(None) => return command,
            Err(error) => {
                tracing::warn!(%error, "thread.created hooks skipped: project lookup failed");
                return command;
            }
        };
        let context = HookDispatchContext {
            project_id: project_id.clone(),
            project_root: root,
            thread_id: Some(thread_id.clone()),
            thread_cwd: worktree_path
                .as_ref()
                .map(|path| PathBuf::from(path.as_str())),
        };
        let event = HookEvent::thread_created(json!({
            "threadId": thread_id,
            "projectId": project_id,
            "title": title,
            "branch": branch,
            "worktreePath": worktree_path,
            "modelSelection": model_selection,
        }));
        let summary = self.hooks.dispatch(event, &context).await;
        let Some(workspace) = summary.thread_workspace else {
            return command;
        };
        let mut command = command;
        if let OrchestrationV2ServerCommand::Client(OrchestrationV2Command::ThreadCreate {
            branch,
            worktree_path,
            ..
        }) = &mut command
        {
            // A caller that already chose a workspace keeps it.
            if worktree_path.is_none() {
                *worktree_path = workspace
                    .worktree_path
                    .and_then(|path| TrimmedNonEmptyString::new(path.to_string_lossy()).ok());
            }
            if branch.is_none() {
                *branch = workspace
                    .branch
                    .and_then(|name| TrimmedNonEmptyString::new(name).ok());
            }
        }
        command
    }

    pub async fn dispatch(
        &self,
        command: OrchestrationV2ServerCommand,
    ) -> Result<DispatchResult, ThreadManagementError> {
        self.assert_dispatch_target_enabled(&command).await?;
        let command = self.bind_thread_workspace(command).await;
        Ok(self.orchestrator.dispatch(command).await?)
    }

    pub async fn get_project_thread(
        &self,
        project_id: &ProjectId,
        thread_id: &ThreadId,
    ) -> Result<OrchestrationV2ThreadProjection, ThreadManagementError> {
        let projection = self
            .orchestrator
            .get_thread_projection(thread_id)
            .await
            .map_err(|cause| ThreadManagementError::ProjectionLoad {
                project_id: project_id.clone(),
                thread_id: thread_id.clone(),
                cause: Box::new(cause),
            })?;
        Self::in_project(projection, project_id, thread_id)
    }

    pub async fn get_project_thread_records(
        &self,
        project_id: &ProjectId,
        thread_id: &ThreadId,
        fields: &[RecordField],
        filter: &RecordFilter,
    ) -> Result<ThreadRecords, ThreadManagementError> {
        let records = self
            .orchestrator
            .get_thread_records(thread_id, fields, filter)
            .await
            .map_err(|cause| ThreadManagementError::ProjectionLoad {
                project_id: project_id.clone(),
                thread_id: thread_id.clone(),
                cause: Box::new(cause),
            })?;
        Self::in_project(records, project_id, thread_id)
    }

    fn in_project(
        projection: OrchestrationV2ThreadProjection,
        project_id: &ProjectId,
        thread_id: &ThreadId,
    ) -> Result<OrchestrationV2ThreadProjection, ThreadManagementError> {
        if &projection.thread.project_id == project_id && projection.thread.deleted_at.is_none() {
            Ok(projection)
        } else {
            Err(ThreadManagementError::ThreadNotFound {
                project_id: project_id.clone(),
                thread_id: thread_id.clone(),
            })
        }
    }

    pub async fn list_project_threads(
        &self,
        project_id: &ProjectId,
        include_subagents: bool,
    ) -> Result<Vec<OrchestrationV2ThreadShell>, ThreadManagementError> {
        let snapshot = self
            .orchestrator
            .get_shell_snapshot(&ShellSnapshotOptions {
                project_id: Some(project_id.clone()),
                location: ShellLocation::Active,
                unsettled_only: false,
            })
            .await
            .map_err(|cause| ThreadManagementError::ProjectThreadsList {
                project_id: project_id.clone(),
                cause: Box::new(cause),
            })?;
        let mut threads: Vec<_> = snapshot
            .threads
            .into_iter()
            .filter(|thread| {
                include_subagents
                    || thread.lineage.relationship_to_parent
                        != Some(OrchestrationV2LineageRelationship::Subagent)
            })
            .collect();
        threads.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| right.id.as_str().cmp(left.id.as_str()))
        });
        Ok(threads)
    }

    pub async fn send_to_thread(
        &self,
        input: SendInput,
    ) -> Result<SendResult, ThreadManagementError> {
        let target = self
            .get_project_thread_records(
                &input.project_id,
                &input.thread_id,
                &[RecordField::Runs, RecordField::ProviderTurns],
                &RecordFilter::default(),
            )
            .await?;
        if target.thread.archived_at.is_some() {
            return Err(ThreadManagementError::ThreadArchived {
                thread_id: input.thread_id.clone(),
            });
        }
        let steerable = latest_steerable_run(&target);
        let dispatch_mode = match input.mode {
            SendMode::Steer | SendMode::Restart => {
                let Some(run) = steerable else {
                    return Err(ThreadManagementError::NoSteerableRun {
                        thread_id: input.thread_id.clone(),
                        mode: input.mode,
                    });
                };
                if input.mode == SendMode::Steer {
                    OrchestrationV2DispatchMode::SteerActive {
                        target_run_id: run.id.clone(),
                    }
                } else {
                    OrchestrationV2DispatchMode::RestartActive {
                        target_run_id: run.id.clone(),
                    }
                }
            }
            SendMode::Auto if steerable.is_some() => OrchestrationV2DispatchMode::SteerActive {
                target_run_id: steerable.map(|run| run.id.clone()).expect("checked"),
            },
            SendMode::Queue => OrchestrationV2DispatchMode::QueueAfterActive,
            SendMode::Auto => OrchestrationV2DispatchMode::StartImmediately,
        };

        let dispatched = self
            .dispatch(OrchestrationV2ServerCommand::Client(
                OrchestrationV2Command::MessageDispatch {
                    notification: None,
                    created_by: input.created_by,
                    creation_source: input.creation_source,
                    scheduled_task_id: input.scheduled_task_id.clone(),
                    sender_thread_id: input.sender_thread_id.clone(),
                    command_id: input.command_id.clone(),
                    thread_id: input.thread_id.clone(),
                    message_id: input.message_id.clone(),
                    text: input.text.clone(),
                    context: None,
                    attachments: input.attachments.clone(),
                    title_seed: None,
                    model_selection: input.model_selection.clone(),
                    source_plan_ref: None,
                    restart_continuation_of_run_id: None,
                    usage_limit_continuation_of_run_id: None,
                    manual_continuation_of_run_id: None,
                    usage_limit_recovery_request_id: None,
                    delivery_intent: None,
                    delegated_completion: None,
                    dispatch_mode,
                },
            ))
            .await?;
        let projection = self
            .get_project_thread_records(
                &input.project_id,
                &input.thread_id,
                &[
                    RecordField::Runs,
                    RecordField::Messages,
                    RecordField::TurnItems,
                ],
                &RecordFilter {
                    message_ids: Some(vec![input.message_id.clone()]),
                    turn_item_types: Some(vec!["user_message".to_owned()]),
                    ..RecordFilter::default()
                },
            )
            .await?;
        let message = projection
            .messages
            .iter()
            .find(|candidate| candidate.id == input.message_id)
            .cloned();
        let run = message
            .as_ref()
            .and_then(|message| message.run_id.as_ref())
            .and_then(|run_id| {
                projection
                    .runs
                    .iter()
                    .find(|candidate| &candidate.id == run_id)
            })
            .cloned();
        let turn_item = projection
            .turn_items
            .iter()
            .find(|candidate| {
                matches!(candidate, OrchestrationV2TurnItem::UserMessage { message_id, .. } if *message_id == input.message_id)
            })
            .cloned();
        // A queued message's user turn item is deliberately not emitted at
        // dispatch time: it materializes when the queued turn actually
        // starts, so it can map onto the provider turn. Every other dispatch
        // mode still produces its turn item transactionally with the run.
        let (Some(message), Some(run)) = (message, run) else {
            return Err(ThreadManagementError::DurableRunProjection {
                thread_id: input.thread_id,
                message_id: input.message_id,
            });
        };
        if turn_item.is_none() && run.status != OrchestrationV2RunStatus::Queued {
            return Err(ThreadManagementError::DurableRunProjection {
                thread_id: input.thread_id,
                message_id: input.message_id,
            });
        }
        let delivery = match &turn_item {
            None => Delivery::Queued,
            Some(OrchestrationV2TurnItem::UserMessage { input_intent, .. }) => match input_intent {
                OrchestrationV2UserMessageInputIntent::QueuedTurn => Delivery::Queued,
                OrchestrationV2UserMessageInputIntent::TurnStart => Delivery::Started,
                _ if input.mode == SendMode::Restart => Delivery::Restarted,
                _ => Delivery::Steered,
            },
            Some(_) => Delivery::Steered,
        };
        Ok(SendResult {
            dispatch: dispatched,
            projection,
            message,
            run,
            turn_item,
            delivery,
        })
    }

    /// Waits for the selected run (or the latest) to end, following a restart
    /// continuation chain, until `timeout_ms`.
    pub async fn wait_for_thread(
        &self,
        input: WaitInput,
    ) -> Result<WaitResult, ThreadManagementError> {
        let load_error = |cause: OrchestratorError| ThreadManagementError::ProjectionLoad {
            project_id: input.project_id.clone(),
            thread_id: input.thread_id.clone(),
            cause: Box::new(cause),
        };
        // Taken before the first read, so a run update between that read and
        // the subscription below still replays.
        let after_sequence = self
            .orchestrator
            .get_thread_event_sequence(&input.thread_id)
            .await
            .map_err(load_error)?;
        let target = self
            .get_project_thread_records(
                &input.project_id,
                &input.thread_id,
                &[RecordField::Runs],
                &RecordFilter::default(),
            )
            .await?;
        let selected = match &input.run_id {
            None => latest_run(&target.runs),
            Some(run_id) => target.runs.iter().find(|candidate| &candidate.id == run_id),
        };
        if let Some(run_id) = &input.run_id
            && selected.is_none()
        {
            return Err(ThreadManagementError::RunNotFound {
                thread_id: input.thread_id.clone(),
                run_id: run_id.clone(),
            });
        }
        let Some(selected) = selected else {
            return Ok(WaitResult {
                thread_id: input.thread_id.clone(),
                run: None,
                timed_out: false,
            });
        };
        let selected_id = selected.id.clone();
        let initial = self
            .run_outcome(&input.thread_id, &selected_id, &target.runs)
            .await?;
        if initial.settled {
            return Ok(WaitResult {
                thread_id: input.thread_id.clone(),
                run: Some(initial.run),
                timed_out: false,
            });
        }

        // Re-read the runs only when the thread records an update to the
        // waited run or a continuation, or its deletion, instead of polling
        // the projection for up to an hour. Each stream keeps one event type,
        // so transcript events never fill its buffer. A declined continuation
        // records no event, so its effect settling wakes the wait too.
        let run_updates = self
            .orchestrator
            .stream_stored_events_from(StreamOptions {
                thread_id: Some(input.thread_id.clone()),
                after_sequence: Some(after_sequence),
                event_type: Some("run.updated".to_owned()),
            })
            .await
            .map_err(load_error)?;
        let deletions = self
            .orchestrator
            .stream_stored_events_from(StreamOptions {
                thread_id: Some(input.thread_id.clone()),
                after_sequence: Some(after_sequence),
                event_type: Some("thread.deleted".to_owned()),
            })
            .await
            .map_err(load_error)?;
        let selected_for_filter = selected_id.clone();
        let events = stream::select(run_updates, deletions).filter_map(move |item| {
            let selected = selected_for_filter.clone();
            async move {
                match item {
                    Err(_) => Some(Err(())),
                    Ok(stored) => match &stored.event {
                        OrchestrationV2DomainEvent::RunUpdated { payload, .. }
                            if payload.id != selected
                                && payload.restart_continuation_of_run_id.is_none() =>
                        {
                            None
                        }
                        _ => Some(Ok(())),
                    },
                }
            }
        });
        let continuation: BoxStream<'_, Result<(), ()>> = if initial.pending {
            let orchestrator = Arc::clone(&self.orchestrator);
            let run = initial.run.clone();
            stream::once(async move {
                orchestrator.await_restart_continuation(&run).await;
                Ok(())
            })
            .boxed()
        } else {
            stream::empty().boxed()
        };
        let mut wakeups = stream::select(events.boxed(), continuation);

        let wait = async {
            while let Some(wake) = wakeups.next().await {
                if wake.is_err() {
                    // A failed event stream is a failed read.
                    return Err(ThreadManagementError::ProjectionLoad {
                        project_id: input.project_id.clone(),
                        thread_id: input.thread_id.clone(),
                        cause: "event stream failed".into(),
                    });
                }
                let current = self
                    .get_project_thread_records(
                        &input.project_id,
                        &input.thread_id,
                        &[RecordField::Runs],
                        &RecordFilter::default(),
                    )
                    .await?;
                let outcome = self
                    .run_outcome(&input.thread_id, &selected_id, &current.runs)
                    .await?;
                if outcome.settled {
                    return Ok(Some(outcome.run));
                }
            }
            Ok(None)
        };
        let timeout = Duration::from_millis(input.timeout_ms.max(1));
        if let Ok(waited) = tokio::time::timeout(timeout, wait).await {
            return match waited? {
                Some(run) => Ok(WaitResult {
                    thread_id: input.thread_id.clone(),
                    run: Some(run),
                    timed_out: false,
                }),
                None => {
                    // The streams ended without a settled run; fall through to
                    // the final read like a timeout.
                    self.final_wait_read(&input, &selected_id).await
                }
            };
        }
        // The run may have reached a terminal status while the timeout was
        // winning the race; the final projection read decides what actually
        // happened, so only a still-active run counts as timed out.
        self.final_wait_read(&input, &selected_id).await
    }

    async fn final_wait_read(
        &self,
        input: &WaitInput,
        selected_id: &RunId,
    ) -> Result<WaitResult, ThreadManagementError> {
        let records = self
            .get_project_thread_records(
                &input.project_id,
                &input.thread_id,
                &[RecordField::Runs],
                &RecordFilter::default(),
            )
            .await?;
        let outcome = self
            .run_outcome(&input.thread_id, selected_id, &records.runs)
            .await?;
        Ok(WaitResult {
            thread_id: input.thread_id.clone(),
            run: Some(outcome.run),
            timed_out: !outcome.settled,
        })
    }

    async fn run_outcome(
        &self,
        thread_id: &ThreadId,
        selected_id: &RunId,
        runs: &[OrchestrationV2Run],
    ) -> Result<RunOutcome, ThreadManagementError> {
        let Some(current) = runs.iter().find(|candidate| &candidate.id == selected_id) else {
            return Err(ThreadManagementError::RunNotFound {
                thread_id: thread_id.clone(),
                run_id: selected_id.clone(),
            });
        };
        let run = latest_restart_continuation(runs, current).clone();
        let pending = is_terminal_run_status(run.status)
            && self.orchestrator.restart_continuation_pending(&run).await;
        let settled = is_terminal_run_status(run.status) && !pending;
        Ok(RunOutcome {
            run,
            pending,
            settled,
        })
    }

    /// Waits for `run_id` to end, then settles the thread if the run
    /// completed. Settling stops the provider session, so an agent that
    /// settles its own thread uses this to wait for its turn to end. A run
    /// that ends any other way leaves the thread active, as does anything
    /// `thread.settle` refuses then, such as a queued message.
    pub async fn settle_after_run(
        &self,
        project_id: &ProjectId,
        thread_id: &ThreadId,
        run_id: &RunId,
    ) -> Result<(), ThreadManagementError> {
        let waited = self
            .wait_for_thread(WaitInput {
                project_id: project_id.clone(),
                thread_id: thread_id.clone(),
                run_id: Some(run_id.clone()),
                timeout_ms: SETTLE_AFTER_RUN_WAIT_MS,
            })
            .await?;
        if waited.run.map(|run| run.status) != Some(OrchestrationV2RunStatus::Completed) {
            return Ok(());
        }
        self.dispatch(OrchestrationV2ServerCommand::Client(
            OrchestrationV2Command::ThreadSettle {
                command_id: CommandId::new(format!("server:settle-after-run:{run_id}"))
                    .expect("non-empty"),
                thread_id: thread_id.clone(),
                settled_at: None,
            },
        ))
        .await?;
        Ok(())
    }

    /// Settles a thread. When the thread's own agent asks, it is mid-turn, so
    /// the thread settles through `settle_after_run` once that turn ends.
    pub async fn settle_thread(
        self: &Arc<Self>,
        thread_id: &ThreadId,
        command_id: &CommandId,
        by_own_agent: bool,
    ) -> Result<SettleOutcome, ThreadManagementError> {
        let shell = if by_own_agent {
            self.orchestrator.get_thread_shell(thread_id).await?
        } else {
            None
        };
        if let Some(shell) = shell
            && let Some(active_run_id) = shell.active_run_id.clone()
        {
            let service = Arc::clone(self);
            let mut background = self.background.lock();
            // Reap finished settles so the set stays small.
            while background.try_join_next().is_some() {}
            background.spawn(async move {
                if let Err(error) = service
                    .settle_after_run(&shell.project_id, &shell.id, &active_run_id)
                    .await
                {
                    tracing::warn!(%error, "settle after run failed");
                }
            });
            return Ok(SettleOutcome::SettlesWhenTurnEnds);
        }
        let result = self
            .dispatch(OrchestrationV2ServerCommand::Client(
                OrchestrationV2Command::ThreadSettle {
                    command_id: command_id.clone(),
                    thread_id: thread_id.clone(),
                    settled_at: None,
                },
            ))
            .await?;
        Ok(SettleOutcome::Settled {
            sequence: result.sequence,
        })
    }

    pub async fn interrupt_thread(
        &self,
        input: InterruptInput,
    ) -> Result<InterruptResult, ThreadManagementError> {
        let target = self
            .get_project_thread_records(
                &input.project_id,
                &input.thread_id,
                &[RecordField::Runs, RecordField::ProviderTurns],
                &RecordFilter::default(),
            )
            .await?;
        let explicit = input
            .run_id
            .as_ref()
            .and_then(|run_id| target.runs.iter().find(|candidate| &candidate.id == run_id));
        if let Some(run_id) = &input.run_id
            && explicit.is_none()
        {
            return Err(ThreadManagementError::RunNotFound {
                thread_id: input.thread_id.clone(),
                run_id: run_id.clone(),
            });
        }
        if let Some(run) = explicit
            && is_terminal_run_status(run.status)
        {
            return Ok(InterruptResult::AlreadyTerminal { run: run.clone() });
        }
        let Some(interruptible) = latest_active_run(&target.runs) else {
            return match input.run_id {
                None => Ok(InterruptResult::NoActiveRun),
                Some(run_id) => Err(ThreadManagementError::ThreadNotInterruptible {
                    thread_id: input.thread_id,
                    run_id,
                }),
            };
        };
        if let Some(run_id) = &input.run_id
            && &interruptible.id != run_id
        {
            return Err(ThreadManagementError::ThreadNotInterruptible {
                thread_id: input.thread_id.clone(),
                run_id: run_id.clone(),
            });
        }
        // Bypasses the enablement chokepoint, like upstream (which calls the
        // orchestrator directly): interrupting never starts provider work.
        let dispatch = self
            .orchestrator
            .dispatch(OrchestrationV2ServerCommand::Client(
                OrchestrationV2Command::RunInterrupt {
                    command_id: input.command_id.clone(),
                    thread_id: input.thread_id.clone(),
                    run_id: interruptible.id.clone(),
                    reason: input.reason.clone(),
                    hold_queue: None,
                },
            ))
            .await?;
        Ok(InterruptResult::InterruptRequested {
            run: interruptible.clone(),
            dispatch,
        })
    }

    /// Asks the provider to stop a running workflow coordinator. The
    /// provider's own terminal notification then settles the coordinator and
    /// its members.
    pub async fn stop_workflow(
        &self,
        input: OrchestrationV2StopWorkflowInput,
        limit: Option<&DispatchModeLimit>,
    ) -> Result<(), OrchestrationV2StopWorkflowError> {
        let thread_id = input.thread_id.clone();
        self.executor
            .with_lock(&thread_id, || self.stop_workflow_locked(&input, limit))
            .await
    }

    async fn stop_workflow_locked(
        &self,
        input: &OrchestrationV2StopWorkflowInput,
        limit: Option<&DispatchModeLimit>,
    ) -> Result<(), OrchestrationV2StopWorkflowError> {
        let failure = |reason: OrchestrationV2StopWorkflowReason, cause: Option<Value>| {
            OrchestrationV2StopWorkflowError::OrchestrationV2StopWorkflowError {
                thread_id: input.thread_id.clone(),
                subagent_id: input.subagent_id.clone(),
                reason,
                cause,
            }
        };
        let unavailable = |error: &dyn std::fmt::Display| {
            failure(
                OrchestrationV2StopWorkflowReason::Unavailable,
                Some(json!({ "message": error.to_string() })),
            )
        };
        let records = self
            .orchestrator
            .get_thread_records(
                &input.thread_id,
                &[
                    RecordField::Subagents,
                    RecordField::Runs,
                    RecordField::ProviderThreads,
                ],
                &RecordFilter::default(),
            )
            .await
            .map_err(|error| unavailable(&error))?;
        if records.thread.deleted_at.is_some() {
            return Err(failure(
                OrchestrationV2StopWorkflowReason::Unavailable,
                None,
            ));
        }
        // A limited caller (an agent over MCP) may not stop work on a thread
        // running above its own modes. Checked under the thread lock, where
        // the thread's user cannot raise the modes between this read and the
        // stop.
        if let Some(limit) = limit {
            let modes = DispatchModes {
                runtime_mode: records.thread.runtime_mode,
                interaction_mode: records.thread.interaction_mode,
            };
            if let Some(mode) = exceeded_dispatch_mode_limit(limit.modes, modes) {
                limit.record_refusal(DispatchModeRefusal {
                    thread_id: input.thread_id.clone(),
                    runtime_mode: modes.runtime_mode,
                    interaction_mode: modes.interaction_mode,
                    mode,
                });
                return Err(failure(
                    OrchestrationV2StopWorkflowReason::Unavailable,
                    None,
                ));
            }
        }
        let Some(task) = records
            .subagents
            .iter()
            .find(|candidate| candidate.id == input.subagent_id)
        else {
            return Err(failure(OrchestrationV2StopWorkflowReason::NotRunning, None));
        };
        if task.thread_id != input.thread_id
            || task.workflow.is_none()
            || task.status != OrchestrationV2SubagentStatus::Running
        {
            return Err(failure(OrchestrationV2StopWorkflowReason::NotRunning, None));
        }
        if task.origin != OrchestrationV2SubagentOrigin::ProviderNative
            || task.driver.as_str() != "claudeAgent"
        {
            return Err(failure(
                OrchestrationV2StopWorkflowReason::Unsupported,
                None,
            ));
        }
        let provider_thread_id = task.provider_thread_id.clone().or_else(|| {
            records
                .runs
                .iter()
                .find(|run| Some(&run.id) == task.run_id.as_ref())
                .and_then(|run| run.provider_thread_id.clone())
        });
        let provider_thread = records.provider_threads.iter().find(|candidate| {
            Some(&candidate.id) == provider_thread_id.as_ref()
                && candidate.app_thread_id.as_ref() == Some(&input.thread_id)
                && candidate.provider_instance_id == task.provider_instance_id
                && candidate.driver == task.driver
        });
        let task_id = task
            .native_task_ref
            .as_ref()
            .and_then(|native| native.native_id.as_ref())
            .map(|id| id.as_str())
            .filter(|id| !id.is_empty());
        let (Some(provider_thread), Some(task_id)) = (provider_thread, task_id) else {
            return Err(failure(
                OrchestrationV2StopWorkflowReason::Unavailable,
                None,
            ));
        };
        let Some(session_id) = provider_thread.provider_session_id.as_ref() else {
            return Err(failure(
                OrchestrationV2StopWorkflowReason::Unavailable,
                None,
            ));
        };
        let runtime = self
            .sessions
            .get(session_id)
            .await
            .map_err(|error| unavailable(&error))?;
        let Some(runtime) = runtime else {
            return Err(failure(
                OrchestrationV2StopWorkflowReason::Unavailable,
                None,
            ));
        };
        let stop = runtime.stop_task(ProviderAdapterV2StopTaskInput {
            provider_thread: provider_thread.clone(),
            task_id: task_id.to_owned(),
        });
        match tokio::time::timeout(self.stop_task_timeout, stop).await {
            Err(_) => Err(failure(
                OrchestrationV2StopWorkflowReason::StopFailed,
                Some(json!({ "_tag": "TimeoutError" })),
            )),
            Ok(Err(error)) => Err(failure(
                OrchestrationV2StopWorkflowReason::StopFailed,
                Some(json!({ "message": error.to_string() })),
            )),
            Ok(Ok(false)) => Err(failure(
                OrchestrationV2StopWorkflowReason::Unsupported,
                None,
            )),
            Ok(Ok(true)) => Ok(()),
        }
    }

    /// Sends `thread.stop` to every delegated task under a thread, depth
    /// first. Their command ids derive from `command_id`, so a retry repeats
    /// nothing that already stopped. A task that cannot be stopped does not
    /// keep the others running: all are tried, then the first failure is
    /// returned.
    pub async fn stop_delegated_tasks(
        &self,
        input: StopDelegatedTasksInput,
    ) -> Result<(), ThreadManagementError> {
        self.stop_delegated_tasks_under(&input, input.thread_id.clone())
            .await
    }

    fn stop_delegated_tasks_under<'a>(
        &'a self,
        input: &'a StopDelegatedTasksInput,
        thread_id: ThreadId,
    ) -> BoxFuture<'a, Result<(), ThreadManagementError>> {
        Box::pin(async move {
            let records = self
                .orchestrator
                .get_thread_records(
                    &thread_id,
                    &[RecordField::Subagents],
                    &RecordFilter::default(),
                )
                .await?;
            let mut failures = Vec::new();
            for task in &records.subagents {
                let (OrchestrationV2SubagentOrigin::AppOwned, Some(child)) =
                    (task.origin, task.child_thread_id.clone())
                else {
                    continue;
                };
                let stopped = async {
                    self.dispatch(OrchestrationV2ServerCommand::Internal(
                        OrchestrationV2InternalCommand::ThreadStop {
                            command_id: CommandId::new(format!(
                                "{}:stop:{child}",
                                input.command_id
                            ))
                            .expect("non-empty"),
                            thread_id: child.clone(),
                            reason: input.reason.clone(),
                        },
                    ))
                    .await?;
                    self.stop_delegated_tasks_under(input, child.clone()).await
                }
                .await;
                if let Err(error) = stopped {
                    tracing::warn!(parent_thread_id = %thread_id, thread_id = %child, %error, "Unable to stop a delegated task");
                    failures.push(error);
                }
            }
            match failures.into_iter().next() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        })
    }
}

struct RunOutcome {
    run: OrchestrationV2Run,
    pending: bool,
    settled: bool,
}

#[cfg(test)]
mod tests;
