//! Narrow seams between the orchestrator decider and the turn, recovery and
//! continuation services. Upstream: `ProviderTurnStartService.ts`,
//! `ProviderTurnControlService.ts`, `RuntimeRequestService.ts`,
//! `ProviderRuntimeRecoveryService.ts`, `RestartContinuation.ts`,
//! `ProviderContinuationService.ts`, `ProviderSwitchService.ts`.
//!
//! The decider does not call the turn services directly. Upstream starts,
//! interrupts and answers turns from effects that `EffectWorker` runs after
//! the command commits. These traits are the contract the turn port (T13e)
//! implements: an [`EffectExecutor`](crate::pipeline::effect_worker::EffectExecutor)
//! calls the turn traits, and the composition root hands the decider a
//! [`RestartRecovery`]. `ProviderSwitchPlanner` lands with the `provider.switch`
//! handler, because its plan carries `ProviderSessionTransition`, which is not
//! ported yet.
//!
//! Errors are `BoxError` on purpose. The decider wraps them as
//! `OrchestratorError::Dispatch` or `Projection`, so each service keeps its own
//! error type.

use async_trait::async_trait;
use turin_contracts::{
    NodeId, ProviderSessionId, ProviderThreadId, ProviderTurnId, RunId, RuntimeRequestId, ThreadId,
    UtcDateTime,
    model_selection::ModelSelection,
    orchestration_v2::{
        OrchestrationV2CheckpointScope, OrchestrationV2Run, OrchestrationV2ThreadProjection,
    },
    provider_policy::ProviderApprovalDecision,
};
use turin_provider_core::provider_adapter::ProviderAdapterV2SubagentOutcome;

use crate::pipeline::orchestrator_port::BoxError;

/// `ProviderTurnStartServiceV2.start` input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnStartInput {
    pub thread_id: ThreadId,
    pub run_id: RunId,
    /// When true, a session-open failure is returned so the caller can retry.
    /// Otherwise the run settles as failed.
    pub will_retry: bool,
}

/// The provider turn a control command addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRef {
    pub thread_id: ThreadId,
    pub provider_session_id: ProviderSessionId,
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: ProviderTurnId,
}

/// A native subagent task inside a turn, for `interrupt` on a child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentTaskRef {
    pub id: NodeId,
    pub native_task_id: String,
    pub requested_at: Option<UtcDateTime>,
}

/// `ProviderTurnControlServiceV2.interruptAndAwaitTerminal` input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptAndAwaitInput {
    pub thread_id: ThreadId,
    pub provider_session_id: ProviderSessionId,
    pub replacement_provider_session_id: Option<ProviderSessionId>,
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: ProviderTurnId,
    pub interrupted_attempt_id: turin_contracts::RunAttemptId,
}

/// `ProviderTurnStartServiceV2`.
#[async_trait]
pub trait ProviderTurnStart: Send + Sync {
    async fn start(&self, input: TurnStartInput) -> Result<(), BoxError>;
}

/// `ProviderTurnControlServiceV2`.
#[async_trait]
pub trait ProviderTurnControl: Send + Sync {
    /// Interrupts one turn. A child subagent returns its adapter outcome.
    async fn interrupt(
        &self,
        turn: TurnRef,
        subagent: Option<SubagentTaskRef>,
    ) -> Result<Option<ProviderAdapterV2SubagentOutcome>, BoxError>;

    async fn steer(
        &self,
        turn: TurnRef,
        message_id: turin_contracts::MessageId,
    ) -> Result<(), BoxError>;

    async fn interrupt_and_await_terminal(
        &self,
        input: InterruptAndAwaitInput,
    ) -> Result<(), BoxError>;
}

/// `RuntimeRequestServiceV2.respond`: answers an approval or a user-input
/// request that a live session is waiting on.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeRequestResponse {
    pub thread_id: ThreadId,
    pub provider_session_id: ProviderSessionId,
    pub request_id: RuntimeRequestId,
    pub decision: Option<ProviderApprovalDecision>,
    pub answers: Option<turin_contracts::provider_policy::ProviderUserInputAnswers>,
}

#[async_trait]
pub trait RuntimeRequestResponder: Send + Sync {
    async fn respond(&self, response: RuntimeRequestResponse) -> Result<(), BoxError>;
}

/// Why `ProviderRuntimeRecoveryService.reconcile` runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileTrigger {
    Startup,
    Shutdown,
}

/// `ProviderRuntimeRecoveryService` (the reconcile, prepare-for-shutdown and
/// recover entry points). Summaries are counts, so they are returned as-is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoverySummary {
    pub terminalized_runs: usize,
    pub stopped_sessions: usize,
    pub closed_requests: usize,
    pub retired_effects: usize,
    pub requeued_effects: usize,
}

#[async_trait]
pub trait ProviderRuntimeRecovery: Send + Sync {
    async fn reconcile(&self, trigger: ReconcileTrigger) -> Result<RecoverySummary, BoxError>;
    async fn prepare_for_shutdown(&self) -> Result<(), BoxError>;
    async fn recover(&self) -> Result<RecoverySummary, BoxError>;
}

/// Restart and delegated-task recovery that the decider's `OrchestratorPort`
/// reads answer for. Upstream: the `Orchestrator.ts` members
/// `recoverDelegatedTasks`, `recoverDelegatedTask`,
/// `delegatedTaskResultPending`, `restartContinuationPending` and
/// `awaitRestartContinuation`, which read `RestartContinuation.ts` and
/// `RestartBackgroundNote.ts`.
#[async_trait]
pub trait RestartRecovery: Send + Sync {
    /// Startup pass that settles delegated-task results and deliveries runs
    /// left behind.
    async fn recover_delegated_tasks(&self);

    /// Settles a delegated child whose restart continuation of `source_run_id`
    /// declined or failed.
    async fn recover_delegated_task(&self, thread_id: &ThreadId, source_run_id: &RunId);

    /// Whether a delegated child's apparent result is not final yet. A failed
    /// read is an error, because the caller treats it as "not pending".
    async fn delegated_task_result_pending(
        &self,
        child_thread_id: &ThreadId,
        task_id: Option<&NodeId>,
    ) -> Result<bool, BoxError>;

    /// Whether a restart continuation of this cut run has not started yet.
    async fn restart_continuation_pending(&self, run: &OrchestrationV2Run) -> bool;

    /// Resolves once that continuation has run or was declined.
    async fn await_restart_continuation(&self, run: &OrchestrationV2Run);
}

/// `RestartRecovery` before T13e lands. Nothing is pending, and every read
/// answers the way upstream's `layerUnavailable` does.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoRestartRecovery;

#[async_trait]
impl RestartRecovery for NoRestartRecovery {
    async fn recover_delegated_tasks(&self) {}

    async fn recover_delegated_task(&self, _thread_id: &ThreadId, _source_run_id: &RunId) {}

    async fn delegated_task_result_pending(
        &self,
        _child_thread_id: &ThreadId,
        _task_id: Option<&NodeId>,
    ) -> Result<bool, BoxError> {
        Ok(false)
    }

    async fn restart_continuation_pending(&self, _run: &OrchestrationV2Run) -> bool {
        false
    }

    async fn await_restart_continuation(&self, _run: &OrchestrationV2Run) {}
}

/// `checkpointService.prepareRootRunScope`: the scope a root run is
/// checkpointed under. Message dispatch and restart continuation call it
/// inside their decision. T06-git implements the trait.
#[derive(Debug, Clone, PartialEq)]
pub struct PrepareRootRunScope {
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub root_node_id: NodeId,
    pub provider_thread_id: ProviderThreadId,
    pub cwd: String,
    pub created_at: UtcDateTime,
}

#[async_trait]
pub trait CheckpointScopes: Send + Sync {
    async fn prepare_root_run_scope(
        &self,
        input: PrepareRootRunScope,
    ) -> Result<OrchestrationV2CheckpointScope, BoxError>;

    /// `checkpointService.ensureScope`: returns the scope as stored (its
    /// checkpoint id may be filled in).
    async fn ensure_scope(
        &self,
        scope: OrchestrationV2CheckpointScope,
    ) -> Result<OrchestrationV2CheckpointScope, BoxError>;
}

/// [`CheckpointScopes`] before T06-git is wired. Every call fails, so message
/// dispatch refuses rather than starting a run without a checkpoint scope.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnwiredCheckpointScopes;

#[async_trait]
impl CheckpointScopes for UnwiredCheckpointScopes {
    async fn prepare_root_run_scope(
        &self,
        _input: PrepareRootRunScope,
    ) -> Result<OrchestrationV2CheckpointScope, BoxError> {
        Err("checkpoint scopes are not wired (T06-git)".into())
    }

    async fn ensure_scope(
        &self,
        _scope: OrchestrationV2CheckpointScope,
    ) -> Result<OrchestrationV2CheckpointScope, BoxError> {
        Err("checkpoint scopes are not wired (T06-git)".into())
    }
}

/// The provider-session part of a model or provider change, the
/// `ProviderSwitchServiceV2.plan` output. Used by `thread.model-selection.set`
/// and `provider.switch`. The transition type (`ProviderSessionTransition`) is
/// not ported yet, so only the fields the decider reads are here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSwitchPlan {
    pub instance_changed: bool,
    pub model_changed: bool,
    pub target_provider_thread_id: Option<ProviderThreadId>,
    pub release_provider_session_ids: Vec<ProviderSessionId>,
}

#[async_trait]
pub trait ProviderSwitchPlanner: Send + Sync {
    async fn plan(
        &self,
        thread: &OrchestrationV2ThreadProjection,
        target: &ModelSelection,
    ) -> Result<ProviderSwitchPlan, BoxError>;
}

/// [`ProviderSwitchPlanner`] before the switch port lands. Refuses every plan.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnwiredProviderSwitchPlanner;

#[async_trait]
impl ProviderSwitchPlanner for UnwiredProviderSwitchPlanner {
    async fn plan(
        &self,
        _thread: &OrchestrationV2ThreadProjection,
        _target: &ModelSelection,
    ) -> Result<ProviderSwitchPlan, BoxError> {
        Err("provider switching is not wired".into())
    }
}
