//! Port of the effectful half of upstream/apps/server/src/orchestration-v2/RestartContinuation.ts
//! (`continueRestartedRun`), and of the restart members of `Orchestrator.ts`
//! that read it: `delegatedTaskResultPending`, `restartContinuationPending`,
//! `awaitRestartContinuation`.
//!
//! Recovery cancels a run a restart cut and queues a `provider-runtime.continue`
//! effect for it. The effect executor calls [`RestartContinuationService::continue_restarted_run`],
//! which dispatches a "continue where you left off" prompt as a new run, unless
//! the user got there first, opted out, or the run is not one to continue.

use std::sync::Arc;
use turin_shared::orchestration_v2_thread_error::run_ran_after;

use async_trait::async_trait;
use turin_contracts::{
    MessageId, NodeId, RunId, ThreadId,
    base_schemas::TrimmedNonEmptyString,
    orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2Command, OrchestrationV2ConversationMessageRole,
        OrchestrationV2CreationSource, OrchestrationV2DispatchMode, OrchestrationV2Notification,
        OrchestrationV2NotificationOutcome, OrchestrationV2NotificationSource, OrchestrationV2Run,
        OrchestrationV2RunStatus, OrchestrationV2ServerCommand, OrchestrationV2TurnItem,
    },
};
use turin_provider_core::subagent_projection::{
    DelegatedTaskProgressInput, DelegatedTaskProgressState, delegated_task_progress,
};

use super::{
    orchestrator::services::RestartRecovery,
    restart_background_note::{
        ProviderTurnState, RunAttemptRef, is_restart_continuation_source, is_restart_note_source,
        restart_cancelled_background_work_note, restart_continuation_note,
    },
    restart_continuation::restart_continuation_blocked,
};
use crate::{
    pipeline::{
        effect_outbox::{EffectOutbox, EffectStatus},
        orchestrator_port::{BoxError, DispatchResult, RecordField, RecordFilter, ThreadRecords},
        seams::ServerSettingsSource,
        thread_management_service::{ThreadManagementError, ThreadManagementService},
    },
    projection::store::{
        ProjectionStore,
        canonical::{ProjectionRecordField, ProjectionRecordFilter},
    },
};

const CONTINUE_PROMPT: &str = "Continue where you left off.";

/// Consecutive automatic continuations allowed before a person has to step in.
const MAX_CONTINUATION_CHAIN: usize = 3;

// ---------------------------------------------------------------------------
// Seams
// ---------------------------------------------------------------------------

/// The `ThreadManagementService` members `continueRestartedRun` uses.
#[async_trait]
pub trait RestartThreads: Send + Sync {
    async fn get_thread_records(
        &self,
        thread_id: &ThreadId,
        fields: &[RecordField],
        filter: &RecordFilter,
    ) -> Result<ThreadRecords, BoxError>;

    /// `ThreadManagementProviderInstanceDisabledError` is a
    /// [`ThreadManagementError::ProviderInstanceDisabled`].
    async fn dispatch(
        &self,
        command: OrchestrationV2ServerCommand,
    ) -> Result<DispatchResult, ThreadManagementError>;

    async fn recover_delegated_task(&self, thread_id: &ThreadId, source_run_id: &RunId);
}

#[async_trait]
impl RestartThreads for ThreadManagementService {
    async fn get_thread_records(
        &self,
        thread_id: &ThreadId,
        fields: &[RecordField],
        filter: &RecordFilter,
    ) -> Result<ThreadRecords, BoxError> {
        self.orchestrator()
            .get_thread_records(thread_id, fields, filter)
            .await
            .map_err(|error| Box::new(error) as BoxError)
    }

    async fn dispatch(
        &self,
        command: OrchestrationV2ServerCommand,
    ) -> Result<DispatchResult, ThreadManagementError> {
        ThreadManagementService::dispatch(self, command).await
    }

    async fn recover_delegated_task(&self, thread_id: &ThreadId, source_run_id: &RunId) {
        self.orchestrator()
            .recover_delegated_task(thread_id, source_run_id)
            .await;
    }
}

/// A [`RestartThreads`] bound after construction. The orchestrator takes its
/// `RestartRecovery` at construction, and this service's `RestartThreads` is the
/// `ThreadManagementService`, which holds the orchestrator. Build the service
/// over a `LateRestartThreads`, build the orchestrator and the thread
/// management service, then [`bind`](Self::bind) the latter. Until then reads
/// and dispatches fail, which fails the effect (the worker retries it).
#[derive(Default)]
pub struct LateRestartThreads {
    inner: std::sync::OnceLock<Arc<dyn RestartThreads>>,
}

impl LateRestartThreads {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Binds the real threads. Returns false if already bound.
    pub fn bind(&self, threads: Arc<dyn RestartThreads>) -> bool {
        self.inner.set(threads).is_ok()
    }

    fn bound(&self) -> Option<&Arc<dyn RestartThreads>> {
        self.inner.get()
    }
}

#[async_trait]
impl RestartThreads for LateRestartThreads {
    async fn get_thread_records(
        &self,
        thread_id: &ThreadId,
        fields: &[RecordField],
        filter: &RecordFilter,
    ) -> Result<ThreadRecords, BoxError> {
        match self.bound() {
            Some(threads) => threads.get_thread_records(thread_id, fields, filter).await,
            None => Err("restart threads are not bound yet".into()),
        }
    }

    async fn dispatch(
        &self,
        command: OrchestrationV2ServerCommand,
    ) -> Result<DispatchResult, ThreadManagementError> {
        match self.bound() {
            Some(threads) => threads.dispatch(command).await,
            None => Err(ThreadManagementError::ProjectionLoad {
                project_id: turin_contracts::ProjectId::new("restart-threads")
                    .expect("literal id is non-empty"),
                thread_id: ThreadId::new("restart-threads").expect("literal id is non-empty"),
                cause: "restart threads are not bound yet".into(),
            }),
        }
    }

    async fn recover_delegated_task(&self, thread_id: &ThreadId, source_run_id: &RunId) {
        if let Some(threads) = self.bound() {
            threads
                .recover_delegated_task(thread_id, source_run_id)
                .await;
        }
    }
}

/// The delegated-task settling the orchestrator owns (`finalizeAppOwnedSubagent`
/// and the completion deliveries, T13c): the startup sweep, and the settle of a
/// child whose continuation declined.
#[async_trait]
pub trait DelegatedTaskRecovery: Send + Sync {
    async fn recover_delegated_tasks(&self);
    async fn recover_delegated_task(&self, thread_id: &ThreadId, source_run_id: &RunId);
}

/// Settles nothing, until the orchestrator's delegated-task finalizing is wired.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoDelegatedTaskRecovery;

#[async_trait]
impl DelegatedTaskRecovery for NoDelegatedTaskRecovery {
    async fn recover_delegated_tasks(&self) {}
    async fn recover_delegated_task(&self, _thread_id: &ThreadId, _source_run_id: &RunId) {}
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// `continueRestartedRun` plus the restart reads the orchestrator answers.
pub struct RestartContinuationService {
    settings: Arc<dyn ServerSettingsSource>,
    threads: Arc<dyn RestartThreads>,
    projections: Arc<ProjectionStore>,
    outbox: EffectOutbox,
    delegated: Arc<dyn DelegatedTaskRecovery>,
}

fn turn_states(records: &ThreadRecords) -> Vec<ProviderTurnState> {
    records.provider_turns.iter().map(Into::into).collect()
}

/// `delegatedTaskTerminalStatus(status) !== null`.
fn run_is_terminal(status: OrchestrationV2RunStatus) -> bool {
    matches!(
        status,
        OrchestrationV2RunStatus::Completed
            | OrchestrationV2RunStatus::Failed
            | OrchestrationV2RunStatus::Cancelled
            | OrchestrationV2RunStatus::Interrupted
            | OrchestrationV2RunStatus::RolledBack
    )
}

fn restart_effect_id(run: &OrchestrationV2Run) -> String {
    format!("effect:restart-continuation:{}", run.id)
}

impl RestartContinuationService {
    pub fn new(
        settings: Arc<dyn ServerSettingsSource>,
        threads: Arc<dyn RestartThreads>,
        projections: Arc<ProjectionStore>,
        outbox: EffectOutbox,
        delegated: Arc<dyn DelegatedTaskRecovery>,
    ) -> Self {
        Self {
            settings,
            threads,
            projections,
            outbox,
            delegated,
        }
    }

    /// `continueRestartedRun`. A continuation this declines to start still owes a
    /// delegated child's parent a result. Once a continuation run exists the
    /// settle is a no-op, since that run settles it. A failure skips the settle:
    /// the effect worker retries, and settles after its last attempt.
    pub async fn continue_restarted_run(
        &self,
        thread_id: &ThreadId,
        source_run_id: &RunId,
    ) -> Result<(), BoxError> {
        self.continue_run(thread_id, source_run_id).await?;
        self.threads
            .recover_delegated_task(thread_id, source_run_id)
            .await;
        Ok(())
    }

    /// The `provider-runtime.continue` arm of the effect executor
    /// (`EffectWorker.ts` `layerExecutor`): continues the run, and when that
    /// fails for the last time (`will_retry` false) settles the delegated task
    /// the continuation would have settled. The error is returned either way,
    /// for the worker to retry or fail the effect.
    pub async fn execute_continue_effect(
        &self,
        thread_id: &ThreadId,
        source_run_id: &RunId,
        will_retry: bool,
    ) -> Result<(), BoxError> {
        let result = self.continue_restarted_run(thread_id, source_run_id).await;
        if result.is_err() && !will_retry {
            self.threads
                .recover_delegated_task(thread_id, source_run_id)
                .await;
        }
        result
    }

    async fn continue_run(
        &self,
        thread_id: &ThreadId,
        source_run_id: &RunId,
    ) -> Result<(), BoxError> {
        let Ok(settings) = self.settings.get_settings().await else {
            return Ok(());
        };
        let message_id = MessageId::new(format!("message:restart-continuation:{source_run_id}"))
            .expect("continuation message id is non-blank");
        let projection = self
            .threads
            .get_thread_records(
                thread_id,
                &[
                    RecordField::Messages,
                    RecordField::Runs,
                    RecordField::ProviderTurns,
                    RecordField::Attempts,
                    RecordField::RuntimeRequests,
                    RecordField::TurnItems,
                ],
                &RecordFilter {
                    message_ids: Some(vec![message_id.clone()]),
                    turn_item_types: Some(vec!["secret_request".to_owned()]),
                    ..RecordFilter::default()
                },
            )
            .await?;
        if !turin_shared::project_settings::resolve_project_settings(
            &settings,
            Some(&projection.thread.project_id),
            None,
            &|_, _, _| None,
        )
        .settings
        .continue_threads_after_server_update
        {
            return Ok(());
        }
        if projection
            .messages
            .iter()
            .any(|message| message.id == message_id)
        {
            return Ok(());
        }
        let turns = turn_states(&projection);
        // Reconciliation cancels a waiting run it cannot checkpoint, so a settled
        // source can be cancelled too. It continues only for awaited work.
        let Some(source) = projection.runs.iter().find(|run| &run.id == source_run_id) else {
            return Ok(());
        };
        if !is_restart_continuation_source(source, &turns)
            || restart_continuation_blocked(
                &projection,
                source,
                chrono::Utc::now(),
                is_restart_note_source(source, &turns),
            )
        {
            return Ok(());
        }
        // A user submission after reconciliation takes precedence over an automatic
        // prompt. Queued runs never started and stay held behind this one.
        if projection.runs.iter().any(|run| {
            run.id != source.id
                && run.status != OrchestrationV2RunStatus::Queued
                && run_ran_after(run, source)
        }) {
            return Ok(());
        }
        if projection.thread.provider_instance_id != source.provider_instance_id {
            return Ok(());
        }
        // A turn that keeps crashing the server would otherwise restart it forever.
        let mut chain_length = 0;
        let mut run = Some(source);
        while let Some(current) = run
            && let Some(previous_id) = current.restart_continuation_of_run_id.as_ref()
            && chain_length < MAX_CONTINUATION_CHAIN
        {
            chain_length += 1;
            run = projection
                .runs
                .iter()
                .find(|candidate| &candidate.id == previous_id);
        }
        if chain_length >= MAX_CONTINUATION_CHAIN {
            tracing::warn!(
                thread_id = %thread_id,
                source_run_id = %source_run_id,
                "Not continuing a run restarted too many times in a row"
            );
            return Ok(());
        }
        let source_records = self
            .threads
            .get_thread_records(
                thread_id,
                &[RecordField::Messages, RecordField::TurnItems],
                &RecordFilter {
                    message_ids: Some(vec![source.user_message_id.clone()]),
                    turn_item_run_ids: Some(vec![Some(source.id.clone())]),
                    turn_item_types: Some(vec!["run_interrupt_request".to_owned()]),
                    ..RecordFilter::default()
                },
            )
            .await?;
        // The user asked this run to stop before the restart cut it.
        if source_records.turn_items.iter().any(|item| {
            matches!(item, OrchestrationV2TurnItem::RunInterruptRequest { .. })
                && crate::projection::turn_item::common(item).run_id == Some(&source.id)
        }) {
            return Ok(());
        }
        if source_records
            .messages
            .iter()
            .find(|message| message.id == source.user_message_id)
            .is_some_and(|message| {
                super::orchestrator::is_native_maintenance_command(
                    &message.text,
                    &message.attachments,
                )
            })
        {
            return Ok(());
        }
        let attempts: Vec<RunAttemptRef> = projection
            .attempts
            .iter()
            .map(|attempt| RunAttemptRef {
                id: attempt.id.clone(),
                run_id: attempt.run_id.clone(),
            })
            .collect();
        let note = restart_continuation_note(source, &projection.runs, &turns, &attempts);
        let note_text =
            (!note.work.is_empty()).then(|| restart_cancelled_background_work_note(&note.work));
        let text = match note_text {
            None => CONTINUE_PROMPT.to_owned(),
            Some(note_text) if note.settled => note_text,
            Some(note_text) => format!("{note_text}\n\n{CONTINUE_PROMPT}"),
        };
        let command =
            OrchestrationV2ServerCommand::Client(OrchestrationV2Command::MessageDispatch {
                // Shown as a work log row; the prompt the agent got is its detail.
                notification: Some(OrchestrationV2Notification {
                    source: OrchestrationV2NotificationSource::System,
                    outcome: OrchestrationV2NotificationOutcome::Updated,
                    summary: TrimmedNonEmptyString::new(if note.settled {
                        "T3 Code restarted and stopped background work"
                    } else {
                        "T3 Code restarted and resumed this turn"
                    })
                    .expect("summary is non-empty"),
                    detail: Some(text.clone()),
                }),
                created_by: OrchestrationV2Actor::Agent,
                creation_source: OrchestrationV2CreationSource::Server,
                scheduled_task_id: None,
                sender_thread_id: None,
                command_id: turin_contracts::CommandId::new(format!(
                    "command:restart-continuation:{source_run_id}"
                ))
                .expect("command id is non-blank"),
                thread_id: thread_id.clone(),
                message_id,
                text,
                context: None,
                attachments: Vec::new(),
                title_seed: None,
                model_selection: Some(source.model_selection.clone()),
                source_plan_ref: None,
                restart_continuation_of_run_id: Some(source_run_id.clone()),
                usage_limit_continuation_of_run_id: None,
                manual_continuation_of_run_id: None,
                usage_limit_recovery_request_id: None,
                delivery_intent: None,
                delegated_completion: None,
                dispatch_mode: OrchestrationV2DispatchMode::StartImmediately,
            });
        match self.threads.dispatch(command).await {
            Ok(_) => Ok(()),
            // The project turned this instance off since the restart. No turn
            // resumes, and the settle still runs so a delegated child waiting on
            // this run is recovered immediately instead of after the effect
            // worker exhausts its retries on a refusal that will not change.
            Err(ThreadManagementError::ProviderInstanceDisabled {
                project_id,
                instance_id,
            }) => {
                tracing::info!(
                    thread_id = %thread_id,
                    source_run_id = %source_run_id,
                    project_id = %project_id,
                    instance_id = %instance_id,
                    "orchestration-v2.restart-continuation.instance-disabled"
                );
                Ok(())
            }
            Err(error) => Err(Box::new(error)),
        }
    }

    /// `awaitsRestartContinuation`. A run with a pending restart continuation has
    /// no outcome yet: restart reconciliation cancelled it mid-turn, or cancelled
    /// the background work a settled turn was waiting on. The continuation's own
    /// run settles it, or the continuation recovers the thread when it declines.
    pub async fn awaits_restart_continuation(
        &self,
        run: &OrchestrationV2Run,
    ) -> Result<bool, BoxError> {
        let effect = self.outbox.get(restart_effect_id(run)).await?;
        Ok(effect.is_some_and(|effect| {
            matches!(effect.status, EffectStatus::Pending | EffectStatus::Running)
        }))
    }

    /// `childAwaitsRestartContinuation`: a delegated child's result is held while
    /// its result run, or a run after it that a second restart cut before it
    /// started, still has a continuation pending.
    pub async fn child_awaits_restart_continuation(
        &self,
        runs: &[OrchestrationV2Run],
        result_run: &OrchestrationV2Run,
        settled_continuation_of: Option<&RunId>,
    ) -> Result<bool, BoxError> {
        let mut pending = false;
        for run in runs.iter().filter(|run| {
            Some(&run.id) != settled_continuation_of
                && (run.id == result_run.id || run_ran_after(*run, result_run))
        }) {
            pending |= self.awaits_restart_continuation(run).await?;
        }
        Ok(pending)
    }
}

#[async_trait]
impl RestartRecovery for RestartContinuationService {
    async fn recover_delegated_tasks(&self) {
        self.delegated.recover_delegated_tasks().await;
    }

    async fn recover_delegated_task(&self, thread_id: &ThreadId, source_run_id: &RunId) {
        self.delegated
            .recover_delegated_task(thread_id, source_run_id)
            .await;
    }

    async fn delegated_task_result_pending(
        &self,
        child_thread_id: &ThreadId,
        task_id: Option<&NodeId>,
    ) -> Result<bool, BoxError> {
        let child = self
            .projections
            .get_thread_records(
                child_thread_id,
                vec![
                    ProjectionRecordField::Runs,
                    ProjectionRecordField::Messages,
                    ProjectionRecordField::Subagents,
                    ProjectionRecordField::ProviderThreads,
                    ProjectionRecordField::ProviderTurns,
                    ProjectionRecordField::Attempts,
                ],
                Some(ProjectionRecordFilter {
                    message_roles: Some(vec![OrchestrationV2ConversationMessageRole::User]),
                    ..ProjectionRecordFilter::default()
                }),
            )
            .await?;
        if let Some(task_id) = task_id {
            let runs: Vec<OrchestrationV2Run> = child
                .runs
                .iter()
                .filter(|run| run.delegated_task_id.as_ref() == Some(task_id))
                .cloned()
                .collect();
            let Some(run) = runs.iter().max_by_key(|run| run.ordinal.get()) else {
                return Ok(true);
            };
            if !run_is_terminal(run.status) {
                return Ok(true);
            }
            return self
                .child_awaits_restart_continuation(&runs, run, None)
                .await;
        }
        let progress = delegated_task_progress(DelegatedTaskProgressInput::from(&child));
        // A caller's older read saw a result; newer work since then means it is not final.
        if progress.state != DelegatedTaskProgressState::ResultAvailable {
            return Ok(true);
        }
        let Some(result_run) = progress.result_run else {
            return Ok(false);
        };
        self.child_awaits_restart_continuation(&child.runs, result_run, None)
            .await
    }

    async fn awaits_restart_continuation(
        &self,
        run: &OrchestrationV2Run,
    ) -> Result<bool, BoxError> {
        RestartContinuationService::awaits_restart_continuation(self, run).await
    }

    async fn restart_continuation_pending(&self, run: &OrchestrationV2Run) -> bool {
        self.awaits_restart_continuation(run).await.unwrap_or(false)
    }

    async fn await_restart_continuation(&self, run: &OrchestrationV2Run) {
        let _ = self.outbox.await_settled(restart_effect_id(run)).await;
    }
}

#[cfg(test)]
mod tests;
