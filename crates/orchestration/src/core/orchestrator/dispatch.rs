//! The command path of upstream `Orchestrator.ts`: `dispatchWithReceipt`
//! (receipt replay, the per-thread lock, the rejected-command record and the
//! commit) and `dispatchOnce` (routing to a handler).
//!
//! Handlers live in sibling files of `core/orchestrator/`. Each one takes the
//! [`Decision`] and appends the events and effects it decides. It returns
//! `Ok(true)` when the kind is handled; [`decide`] returns `Ok(false)` for a
//! kind no handler has been ported for yet, and the command is rejected
//! before any receipt is written, so it can be retried once it is ported.

use turin_contracts::{
    CommandId, EventId, ThreadId, UtcDateTime,
    orchestration_v2::{OrchestrationV2DomainEvent, OrchestrationV2ServerCommand},
};
use turin_persistence::ReceiptStatus;
use turin_provider_core::id_allocator::IdAllocator;

use super::{
    Orchestrator, checkpoint_rollback, delegated_commands, delegated_request, message_dispatch,
    prepared_run, queue_start, queued_run, run_stop, runtime_request, secret_request,
    thread_archive, thread_delete, thread_fork, thread_lifecycle, thread_merge_back,
    thread_metadata, thread_mutation, thread_pull_request,
};
use crate::pipeline::command_receipt_store::CommandReceipt;
use crate::pipeline::dispatch_mode_limit::{
    DispatchModeLimit, DispatchModeRefusal, DispatchModes, exceeded_dispatch_mode_limit,
};
use crate::pipeline::effect_outbox::PendingEffect;
use crate::pipeline::event_sink::{CancelUnsettled, CommandCommit};
use crate::pipeline::orchestrator_port::{
    BoxError, DispatchResult, OrchestratorError, OrchestratorPort,
};

/// What one command decides to write: its events, the effects they queue, and
/// any unsettled effects to cancel. Upstream's `dispatchOnce` result.
pub struct Decision {
    command_id: CommandId,
    /// One clock reading per command. Upstream reads `DateTime.now` per event,
    /// but the events are decided in one synchronous step, so the difference is
    /// milliseconds. `accepted_at` is the last event's time upstream and this
    /// clock here.
    now: UtcDateTime,
    ids: IdAllocator,
    /// The sender's mode limit (`DispatchModeLimit`, an ambient reference
    /// upstream), for deciders that touch a thread outside this command's lock.
    limit: Option<DispatchModeLimit>,
    pub events: Vec<OrchestrationV2DomainEvent>,
    pub effects: Vec<PendingEffect>,
    pub cancel_unsettled_effects: Option<CancelUnsettled>,
}

impl Decision {
    pub(super) fn new(
        command_id: CommandId,
        ids: IdAllocator,
        limit: Option<DispatchModeLimit>,
    ) -> Self {
        Self {
            command_id,
            now: UtcDateTime::now(),
            ids,
            limit,
            events: Vec::new(),
            effects: Vec::new(),
            cancel_unsettled_effects: None,
        }
    }

    pub fn command_id(&self) -> &CommandId {
        &self.command_id
    }

    /// An empty decision for the same command, so a step that may fail keeps
    /// its partial writes apart (upstream's separate `Ref`s). Merge a
    /// successful one back with [`Self::absorb`].
    pub fn fork(&self) -> Self {
        Self {
            command_id: self.command_id.clone(),
            now: self.now,
            ids: self.ids,
            limit: self.limit.clone(),
            events: Vec::new(),
            effects: Vec::new(),
            cancel_unsettled_effects: None,
        }
    }

    /// Appends what a [`Self::fork`] decided.
    pub fn absorb(&mut self, other: Self) {
        self.events.extend(other.events);
        self.effects.extend(other.effects);
        if other.cancel_unsettled_effects.is_some() {
            self.cancel_unsettled_effects = other.cancel_unsettled_effects;
        }
    }

    /// `DateTime.now` for this decision (see the field doc).
    pub fn now(&self) -> UtcDateTime {
        self.now
    }

    /// The limit a decider checks for a thread outside the command's lock.
    pub fn mode_limit(&self) -> Option<&DispatchModeLimit> {
        self.limit.as_ref()
    }

    /// `makeEvent`'s id: `idAllocator.allocate.event({threadId, commandId})`.
    pub fn event_id(&self, thread_id: &ThreadId) -> EventId {
        self.ids
            .event(Some(thread_id), Some(&self.command_id), None)
    }

    pub fn emit(&mut self, event: OrchestrationV2DomainEvent) {
        self.events.push(event);
    }
}

/// The command id of any server command.
pub fn command_id(command: &OrchestrationV2ServerCommand) -> &CommandId {
    command_ids(command).0
}

/// The thread a command's lock and receipt belong to (`commandThreadId`).
pub fn command_thread_id(command: &OrchestrationV2ServerCommand) -> &ThreadId {
    command_ids(command).1
}

fn command_ids(command: &OrchestrationV2ServerCommand) -> (&CommandId, &ThreadId) {
    use OrchestrationV2ServerCommand::{Client, Internal};
    use turin_contracts::orchestration_v2::{
        OrchestrationV2Command as C, OrchestrationV2InternalCommand as I,
    };
    match command {
        Client(C::ThreadFork {
            command_id,
            target_thread_id,
            ..
        })
        | Client(C::ThreadMergeBack {
            command_id,
            target_thread_id,
            ..
        }) => (command_id, target_thread_id),
        Client(C::DelegatedTaskRequest {
            command_id,
            parent_thread_id,
            ..
        })
        | Client(C::DelegatedTaskWakePolicy {
            command_id,
            parent_thread_id,
            ..
        })
        | Client(C::DelegatedTaskCompletionDeliveryAcknowledge {
            command_id,
            parent_thread_id,
            ..
        })
        | Client(C::DelegatedTaskCompletionDeliveryDispose {
            command_id,
            parent_thread_id,
            ..
        })
        | Client(C::ThreadCreatedRecord {
            command_id,
            parent_thread_id,
            ..
        }) => (command_id, parent_thread_id),
        Client(
            C::ThreadCreate {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadArchive {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadUnarchive {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadDelete {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadSettle {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadAutoSettle {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadUnsettle {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadSnooze {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadUnsnooze {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadAutoSettleSet {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadPin {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadUnpin {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadPinReorder {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadActiveReorder {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadVisit {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadMarkUnread {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadMetadataUpdate {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadPullRequestLink {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadPullRequestUnlink {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadPullRequestLinkSync {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadPullRequestWatch {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadPullRequestSync {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadTitleRegenerationComplete {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadRuntimeModeSet {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadInteractionModeSet {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadModelSelectionSet {
                command_id,
                thread_id,
                ..
            }
            | C::ProviderSessionDetach {
                command_id,
                thread_id,
                ..
            }
            | C::MessageDispatch {
                command_id,
                thread_id,
                ..
            }
            | C::PreparedRunRelease {
                command_id,
                thread_id,
                ..
            }
            | C::NotificationDeliveryAccept {
                command_id,
                thread_id,
                ..
            }
            | C::PreparedRunProgress {
                command_id,
                thread_id,
                ..
            }
            | C::PreparedRunFail {
                command_id,
                thread_id,
                ..
            }
            | C::PreparedRunRetry {
                command_id,
                thread_id,
                ..
            }
            | C::SubagentStop {
                command_id,
                thread_id,
                ..
            }
            | C::RunInterrupt {
                command_id,
                thread_id,
                ..
            }
            | C::QueuedMessagePromoteToSteer {
                command_id,
                thread_id,
                ..
            }
            | C::QueueResume {
                command_id,
                thread_id,
                ..
            }
            | C::QueuedRunReorder {
                command_id,
                thread_id,
                ..
            }
            | C::QueuedRunCancel {
                command_id,
                thread_id,
                ..
            }
            | C::QueuedRunEdit {
                command_id,
                thread_id,
                ..
            }
            | C::RuntimeRequestRespond {
                command_id,
                thread_id,
                ..
            }
            | C::ThreadUserInputDismiss {
                command_id,
                thread_id,
                ..
            }
            | C::CheckpointRollback {
                command_id,
                thread_id,
                ..
            }
            | C::ProviderSwitch {
                command_id,
                thread_id,
                ..
            },
        ) => (command_id, thread_id),
        Internal(
            I::ThreadPullRequestWatchSync {
                command_id,
                thread_id,
                ..
            }
            | I::CheckpointRollbackFail {
                command_id,
                thread_id,
                ..
            }
            | I::ThreadBackgroundWorkSettle {
                command_id,
                thread_id,
                ..
            }
            | I::ThreadStop {
                command_id,
                thread_id,
                ..
            }
            | I::SecretRequestRecord {
                command_id,
                thread_id,
                ..
            },
        ) => (command_id, thread_id),
    }
}

/// The wire `type` of a command (`thread.create`, `message.dispatch`, ...).
pub fn command_type(command: &OrchestrationV2ServerCommand) -> String {
    serde_json::to_value(command)
        .ok()
        .and_then(|value| value.get("type")?.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The sender of `queued-run.edit` gets a `CommandRejected` instead of a
/// generic failure when the edit is refused (upstream's special case).
const QUEUED_RUN_EDIT: &str = "queued-run.edit";

/// Commands whose plan may legitimately record no events: a settle that finds
/// nothing running, or a stop with nothing to stop.
fn may_plan_no_events(command: &OrchestrationV2ServerCommand) -> bool {
    use OrchestrationV2ServerCommand::{Client, Internal};
    use turin_contracts::orchestration_v2::{
        OrchestrationV2Command as C, OrchestrationV2InternalCommand as I,
    };
    matches!(
        command,
        Internal(I::ThreadBackgroundWorkSettle { .. } | I::ThreadStop { .. })
            | Client(C::SubagentStop { .. })
    )
}

/// A refusal `Orchestrator.ts` raises as `OrchestratorDispatchError` for this
/// command. Recorded as a rejected receipt by [`record_rejection`].
pub(super) fn dispatch_failure(
    command: &OrchestrationV2ServerCommand,
    cause: impl Into<BoxError>,
) -> OrchestratorError {
    dispatch_error(command_id(command), &command_type(command), cause)
}

fn dispatch_error(
    command_id: &CommandId,
    command_type: &str,
    cause: impl Into<BoxError>,
) -> OrchestratorError {
    OrchestratorError::Dispatch {
        command_id: command_id.clone(),
        command_type: command_type.to_owned(),
        cause: cause.into(),
    }
}

/// `dispatchWithReceipt` for one command, under its thread's lock.
pub async fn dispatch_with_receipt(
    orchestrator: &Orchestrator,
    command: OrchestrationV2ServerCommand,
) -> Result<DispatchResult, OrchestratorError> {
    dispatch_with_limit(orchestrator, command, None).await
}

/// `dispatchWithReceipt` for a limited sender (upstream: an MCP caller whose
/// `DispatchModeLimit` is in context). The limit is re-checked under the
/// thread's lock, where the thread's user cannot raise the modes meanwhile.
pub async fn dispatch_with_limit(
    orchestrator: &Orchestrator,
    command: OrchestrationV2ServerCommand,
    limit: Option<&DispatchModeLimit>,
) -> Result<DispatchResult, OrchestratorError> {
    let thread_id = command_thread_id(&command).clone();
    // Follow-up intake into an app-owned child writes the parent's task rows,
    // so it serializes with Stop and finalize on the parent lock, taken before
    // the child's.
    let parent_thread_id = match &command {
        OrchestrationV2ServerCommand::Client(
            turin_contracts::orchestration_v2::OrchestrationV2Command::MessageDispatch {
                created_by,
                creation_source,
                ..
            },
        ) if *created_by == turin_contracts::orchestration_v2::OrchestrationV2Actor::User
            || *creation_source
                == turin_contracts::orchestration_v2::OrchestrationV2CreationSource::Mcp =>
        {
            orchestrator
                .app_owned_subagent_parent_thread_id(&thread_id)
                .await?
        }
        _ => None,
    };
    let dispatch = || {
        orchestrator
            .locks
            .with_lock(&thread_id, || dispatch_locked(orchestrator, command, limit))
    };
    match parent_thread_id {
        Some(parent) => orchestrator.locks.with_lock(&parent, dispatch).await,
        None => dispatch().await,
    }
}

/// `refuseAboveDispatchModeLimit`: fails when a limited sender's command would
/// touch a thread running above its limit. The refusal is also recorded in the
/// limit's slot, so the sender can report it whatever wraps the error.
pub(super) fn refuse_above_dispatch_mode_limit(
    limit: Option<&DispatchModeLimit>,
    command: &OrchestrationV2ServerCommand,
    thread_id: &ThreadId,
    modes: DispatchModes,
) -> Result<(), OrchestratorError> {
    let Some(limit) = limit else {
        return Ok(());
    };
    let Some(mode) = exceeded_dispatch_mode_limit(limit.modes, modes) else {
        return Ok(());
    };
    limit.record_refusal(DispatchModeRefusal {
        thread_id: thread_id.clone(),
        runtime_mode: modes.runtime_mode,
        interaction_mode: modes.interaction_mode,
        mode,
    });
    Err(OrchestratorError::ThreadAboveModeLimit {
        command_id: command_id(command).clone(),
        thread_id: thread_id.clone(),
        mode,
        runtime_mode: modes.runtime_mode,
        interaction_mode: modes.interaction_mode,
    })
}

async fn dispatch_locked(
    orchestrator: &Orchestrator,
    command: OrchestrationV2ServerCommand,
    limit: Option<&DispatchModeLimit>,
) -> Result<DispatchResult, OrchestratorError> {
    let command_id = command_id(&command).clone();
    let thread_id = command_thread_id(&command).clone();
    let command_type = command_type(&command);

    let existing = orchestrator
        .receipts
        .get_by_command_id(command_id.clone())
        .await
        .map_err(|cause| dispatch_error(&command_id, &command_type, cause))?;
    if let Some(receipt) = existing {
        return replay_receipt(orchestrator, command_id, command_type, thread_id, receipt).await;
    }

    // A limited sender checked these modes before dispatching; the thread's
    // user may have raised them since, and only here can they not change. A
    // fork or merge-back source is not under this lock: its decider checks the
    // copy it reads instead.
    if limit.is_some()
        && let Some(shell) = orchestrator.get_thread_shell(&thread_id).await?
    {
        refuse_above_dispatch_mode_limit(
            limit,
            &command,
            &thread_id,
            DispatchModes {
                runtime_mode: shell.runtime_mode,
                interaction_mode: shell.interaction_mode,
            },
        )?;
    }

    let mut decision = Decision::new(command_id.clone(), orchestrator.ids, limit.cloned());
    let planned = match decide(orchestrator, &command, &mut decision).await {
        Ok(false) => {
            return Err(OrchestratorError::CommandRejected {
                command_id,
                command_type: command_type.clone(),
                cause: format!("Command {command_type} is not ported to Turin yet.").into(),
            });
        }
        Ok(true) if decision.events.is_empty() && !may_plan_no_events(&command) => {
            Err(dispatch_error(
                &command_id,
                &command_type,
                "Command produced no domain events.",
            ))
        }
        Ok(true) => Ok(()),
        Err(cause) => Err(cause),
    };
    if let Err(cause) = planned {
        return Err(record_rejection(orchestrator, &command, cause).await);
    }

    if decision.events.is_empty() {
        // A settle that ended nothing still records its receipt: a replayed Stop
        // effect then finds it instead of settling work that appeared since.
        let sequence = orchestrator
            .sink
            .latest_sequence(Some(thread_id.clone()))
            .await
            .map_err(|cause| dispatch_error(&command_id, &command_type, cause))?;
        let result_sequence = u64::try_from(sequence).unwrap_or(0);
        orchestrator
            .receipts
            .insert_if_absent(CommandReceipt {
                command_id: command_id.clone(),
                thread_id: thread_id.clone(),
                command_type: command_type.clone(),
                accepted_at: decision.now(),
                result_sequence,
                status: ReceiptStatus::Accepted,
                error: None,
            })
            .await
            .map_err(|cause| dispatch_error(&command_id, &command_type, cause))?;
        return Ok(DispatchResult {
            sequence: result_sequence,
            stored_events: Vec::new(),
        });
    }

    let committed = orchestrator
        .sink
        .commit_command(CommandCommit {
            command_id: command_id.clone(),
            thread_id: thread_id.clone(),
            command_type: command_type.clone(),
            accepted_at: decision.now(),
            events: decision.events,
            effects: decision.effects,
            cancel_unsettled_effects: decision.cancel_unsettled_effects,
        })
        .await
        .map_err(|cause| dispatch_error(&command_id, &command_type, cause))?;
    if committed.receipt.status == ReceiptStatus::Rejected {
        return Err(OrchestratorError::CommandPreviouslyRejected {
            command_id,
            command_type,
            detail: committed
                .receipt
                .error
                .unwrap_or_else(|| "Previously rejected.".to_owned()),
        });
    }
    run_post_commit_hooks(orchestrator, &command_id, &command_type, &thread_id).await?;
    Ok(DispatchResult {
        sequence: committed.receipt.result_sequence,
        stored_events: committed.stored_events,
    })
}

/// What upstream runs after a command commits (or replays). `queue.resume`
/// starts the thread's next queued run, still under the command's lock.
async fn run_post_commit_hooks(
    orchestrator: &Orchestrator,
    command_id: &CommandId,
    command_type: &str,
    thread_id: &ThreadId,
) -> Result<(), OrchestratorError> {
    if command_type == "queue.resume" {
        orchestrator
            .start_next_queued_run_locked(thread_id, &Default::default())
            .await
            .map_err(|cause| dispatch_error(command_id, command_type, cause))?;
    }
    // A task wake policy names its parent thread, which is the command's thread.
    if matches!(
        command_type,
        "notification.delivery.accept" | "delegated_task.wake-policy"
    ) {
        orchestrator
            .offer_delegated_completion_deliveries(thread_id)
            .await
            .map_err(|cause| dispatch_error(command_id, command_type, cause))?;
    }
    Ok(())
}

/// A command whose receipt already exists answers with what it recorded. A
/// rejected receipt, or one for another thread, is an error.
async fn replay_receipt(
    orchestrator: &Orchestrator,
    command_id: CommandId,
    command_type: String,
    thread_id: ThreadId,
    receipt: CommandReceipt,
) -> Result<DispatchResult, OrchestratorError> {
    if receipt.status == ReceiptStatus::Rejected {
        return Err(OrchestratorError::CommandPreviouslyRejected {
            command_id,
            command_type,
            detail: receipt
                .error
                .unwrap_or_else(|| "Previously rejected.".to_owned()),
        });
    }
    // A receipt proves this exact command ran for its own thread. Replaying it
    // for another thread would report success for work that never happened.
    if receipt.thread_id != thread_id {
        return Err(OrchestratorError::CommandIdConflict {
            command_id,
            command_type,
            receipt_thread_id: receipt.thread_id,
            command_thread_id: thread_id,
        });
    }
    let stored_events = orchestrator
        .sink
        .read_by_command_id(&command_id)
        .await
        .map_err(|cause| dispatch_error(&command_id, &command_type, cause))?;
    run_post_commit_hooks(orchestrator, &command_id, &command_type, &thread_id).await?;
    Ok(DispatchResult {
        sequence: receipt.result_sequence,
        stored_events,
    })
}

/// Records a refused command so a replay reports the same refusal. Upstream's
/// `Effect.catch` around `dispatchOnce`. A mode-limit refusal records nothing,
/// so the same command can go through once the user lowers the mode.
async fn record_rejection(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    cause: OrchestratorError,
) -> OrchestratorError {
    if matches!(cause, OrchestratorError::ThreadAboveModeLimit { .. }) {
        return cause;
    }
    let command_id = command_id(command).clone();
    let thread_id = command_thread_id(command).clone();
    let command_type = command_type(command);
    let receipt = match orchestrator
        .sink
        .commit_rejected_command(
            command_id.clone(),
            thread_id,
            command_type.clone(),
            UtcDateTime::now(),
            cause.to_string(),
        )
        .await
    {
        Ok(receipt) => receipt,
        Err(receipt_cause) => return dispatch_error(&command_id, &command_type, receipt_cause),
    };
    if command_type == QUEUED_RUN_EDIT
        && receipt.status == ReceiptStatus::Rejected
        && let OrchestratorError::Dispatch { cause: inner, .. } = cause
    {
        return OrchestratorError::CommandRejected {
            command_id,
            command_type,
            cause: inner,
        };
    }
    cause
}

/// `dispatchOnce`: routes the command to its handler group. `Ok(false)` means
/// no handler is ported for this kind, so nothing was decided.
async fn decide(
    orchestrator: &Orchestrator,
    command: &OrchestrationV2ServerCommand,
    decision: &mut Decision,
) -> Result<bool, OrchestratorError> {
    if thread_lifecycle::decide(orchestrator, command, decision).await? {
        return Ok(true);
    }
    if thread_mutation::decide(orchestrator, command, decision).await? {
        return Ok(true);
    }
    if thread_archive::decide(orchestrator, command, decision).await? {
        return Ok(true);
    }
    if thread_metadata::decide(orchestrator, command, decision).await? {
        return Ok(true);
    }
    if message_dispatch::decide(orchestrator, command, decision).await? {
        return Ok(true);
    }
    if prepared_run::decide(orchestrator, command, decision).await? {
        return Ok(true);
    }
    if queue_start::decide(orchestrator, command, decision).await? {
        return Ok(true);
    }
    if queued_run::decide(orchestrator, command, decision).await? {
        return Ok(true);
    }
    if thread_pull_request::decide(orchestrator, command, decision).await? {
        return Ok(true);
    }
    if checkpoint_rollback::decide(orchestrator, command, decision).await? {
        return Ok(true);
    }
    if delegated_commands::decide(orchestrator, command, decision).await? {
        return Ok(true);
    }
    if delegated_request::decide(orchestrator, command, decision).await? {
        return Ok(true);
    }
    if secret_request::decide(orchestrator, command, decision).await? {
        return Ok(true);
    }
    if runtime_request::decide(orchestrator, command, decision).await? {
        return Ok(true);
    }
    if run_stop::decide(orchestrator, command, decision).await? {
        return Ok(true);
    }
    if thread_delete::decide(orchestrator, command, decision).await? {
        return Ok(true);
    }
    if thread_fork::decide(orchestrator, command, decision).await? {
        return Ok(true);
    }
    if thread_merge_back::decide(orchestrator, command, decision).await? {
        return Ok(true);
    }
    Ok(false)
}
