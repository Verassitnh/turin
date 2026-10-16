//! Port of upstream/apps/server/src/orchestration-v2/ProviderTurnControlService.ts.
//!
//! Interrupts, steers, and restart-interrupts a provider turn. Every command
//! first loads the recorded provider execution target and checks that the
//! session it names is the one the projection binds, or the exact committed
//! restart replacement. A turn that already ended is not interrupted again.

use std::{fmt, sync::Arc, time::Duration};

use tokio::time::Instant;
use turin_contracts::{
    MessageId, NodeId, ProviderSessionId, ProviderThreadId, ProviderTurnId, RunAttemptId, ThreadId,
    UtcDateTime,
    orchestration_v2::{
        OrchestrationV2ProviderThread, OrchestrationV2ProviderTurn,
        OrchestrationV2ProviderTurnStatus, OrchestrationV2RunAttemptStatus,
    },
};
use turin_provider_core::provider_adapter::{
    ProviderAdapterV2InterruptInput, ProviderAdapterV2SteerInput,
    ProviderAdapterV2StopSubagentInput, ProviderAdapterV2SubagentOutcome,
    ProviderAdapterV2TurnMessage, ProviderSessionRuntime,
};
use turin_shared::composer_context_references::project_composer_context_for_provider;

use super::projection_reads::{
    ProviderControlContext, ProviderControlReads, ProviderControlTarget,
};
use super::runtime_request_service::LiveSessions;

const SUBAGENT_STOP_TIMEOUT: Duration = Duration::from_secs(2);
const SUBAGENT_STOP_POLL: Duration = Duration::from_millis(10);
/// Yields to the runtime this many times while waiting for a restart's interrupt.
const TERMINAL_WAIT_YIELDS: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderTurnControlOperation {
    Interrupt,
    Restart,
    Steer,
}

impl ProviderTurnControlOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Interrupt => "interrupt",
            Self::Restart => "restart",
            Self::Steer => "steer",
        }
    }
}

/// `ProviderTurnControlError`. `turn_completed` is set only when the turn's
/// end is what made the command fail.
#[derive(Debug)]
pub struct ProviderTurnControlError {
    pub thread_id: ThreadId,
    pub operation: ProviderTurnControlOperation,
    pub provider_turn_id: ProviderTurnId,
    pub turn_completed: Option<bool>,
    pub cause: String,
}

impl fmt::Display for ProviderTurnControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Provider turn {} {} failed on thread {}: {}",
            self.provider_turn_id.as_str(),
            self.operation.as_str(),
            self.thread_id.as_str(),
            self.cause
        )
    }
}

impl std::error::Error for ProviderTurnControlError {}

/// A subagent stop: the subagent node and the native task to stop.
#[derive(Debug, Clone)]
pub struct SubagentStop {
    pub id: NodeId,
    pub native_task_id: String,
    pub requested_at: Option<UtcDateTime>,
}

#[derive(Debug, Clone)]
pub struct InterruptInput {
    pub thread_id: ThreadId,
    pub provider_session_id: ProviderSessionId,
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: ProviderTurnId,
    pub subagent: Option<SubagentStop>,
}

#[derive(Debug, Clone)]
pub struct SteerInput {
    pub thread_id: ThreadId,
    pub provider_session_id: ProviderSessionId,
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: ProviderTurnId,
    pub message_id: MessageId,
}

#[derive(Debug, Clone)]
pub struct AwaitTerminalInput {
    pub thread_id: ThreadId,
    pub provider_session_id: ProviderSessionId,
    /// The session a committed restart just bound. The old one may still be live.
    pub replacement_provider_session_id: Option<ProviderSessionId>,
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: ProviderTurnId,
    pub interrupted_attempt_id: RunAttemptId,
}

/// One command's target, shared by `load` and the wait loops.
struct Execution {
    thread_id: ThreadId,
    provider_session_id: ProviderSessionId,
    replacement_provider_session_id: Option<ProviderSessionId>,
    provider_thread_id: ProviderThreadId,
    provider_turn_id: ProviderTurnId,
    message_id: Option<MessageId>,
}

impl Execution {
    fn control_target(&self, attempt_id: Option<RunAttemptId>) -> ProviderControlTarget {
        ProviderControlTarget {
            provider_thread_id: self.provider_thread_id.clone(),
            provider_turn_id: self.provider_turn_id.clone(),
            attempt_id,
            message_id: self.message_id.clone(),
        }
    }

    fn fail(
        &self,
        operation: ProviderTurnControlOperation,
        cause: impl ToString,
        turn_completed: Option<bool>,
    ) -> ProviderTurnControlError {
        ProviderTurnControlError {
            thread_id: self.thread_id.clone(),
            operation,
            provider_turn_id: self.provider_turn_id.clone(),
            turn_completed,
            cause: cause.to_string(),
        }
    }
}

/// What `load` found. `session` is `None` when the turn ended, or when the
/// session is gone and the command may proceed without it.
struct Loaded {
    context: ProviderControlContext,
    provider_thread: OrchestrationV2ProviderThread,
    provider_turn: OrchestrationV2ProviderTurn,
    session: Option<Arc<dyn ProviderSessionRuntime>>,
}

pub struct ProviderTurnControlService {
    reads: Arc<dyn ProviderControlReads>,
    sessions: Arc<dyn LiveSessions>,
}

fn is_running(turn: Option<&OrchestrationV2ProviderTurn>) -> bool {
    turn.is_some_and(|turn| turn.status == OrchestrationV2ProviderTurnStatus::Running)
}

impl ProviderTurnControlService {
    pub fn new(reads: Arc<dyn ProviderControlReads>, sessions: Arc<dyn LiveSessions>) -> Self {
        Self { reads, sessions }
    }

    /// `interrupt`. A subagent stop returns the provider's outcome. A turn
    /// interrupt waits up to 2 s for the turn to leave the running state.
    pub async fn interrupt(
        &self,
        input: InterruptInput,
    ) -> Result<Option<ProviderAdapterV2SubagentOutcome>, ProviderTurnControlError> {
        use ProviderTurnControlOperation::Interrupt;
        let execution = Execution {
            thread_id: input.thread_id.clone(),
            provider_session_id: input.provider_session_id.clone(),
            replacement_provider_session_id: None,
            provider_thread_id: input.provider_thread_id.clone(),
            provider_turn_id: input.provider_turn_id.clone(),
            message_id: None,
        };
        let loaded = self.load(&execution, Interrupt).await?;
        let session = match loaded.session.clone() {
            Some(session) => session,
            None => match self
                .sessions
                .get_session(&input.provider_session_id)
                .await
                .map_err(|error| execution.fail(Interrupt, error, None))?
            {
                Some(session) => session,
                None => {
                    if input.subagent.is_some() {
                        return Err(execution.fail(
                            Interrupt,
                            "No live provider session can confirm this subagent's outcome.",
                            None,
                        ));
                    }
                    return Ok(None);
                }
            },
        };
        if let Some(subagent) = input.subagent {
            let stop = session
                .stop_subagent(ProviderAdapterV2StopSubagentInput {
                    provider_thread: loaded.provider_thread,
                    native_task_id: subagent.native_task_id,
                    requested_at: subagent.requested_at,
                })
                .ok_or_else(|| {
                    execution.fail(
                        Interrupt,
                        "The provider does not support stopping a native subagent.",
                        None,
                    )
                })?;
            return stop
                .await
                .map_err(|error| execution.fail(Interrupt, error, None));
        }
        // A settled turn reaches its adapter too: only the adapter knows whether
        // it still runs work for the thread.
        session
            .interrupt_turn(ProviderAdapterV2InterruptInput {
                provider_thread: loaded.provider_thread,
                provider_turn_id: loaded.provider_turn.id.clone(),
                request_runtime_restart: Some(true),
            })
            .await
            .map_err(|error| execution.fail(Interrupt, error, None))?;
        // Native terminal ingestion runs on other tasks. Give it time to finish
        // before the stop follow-up repairs a run the provider no longer reports on.
        let deadline = Instant::now() + SUBAGENT_STOP_TIMEOUT;
        while loaded.provider_turn.status == OrchestrationV2ProviderTurnStatus::Running
            && Instant::now() < deadline
        {
            let current = self
                .reads
                .provider_control_context(&input.thread_id, &execution.control_target(None))
                .await
                .map_err(|error| execution.fail(Interrupt, error, None))?;
            // Upstream treats a missing attempt as not running.
            let attempt_running = current
                .attempt
                .is_some_and(|attempt| attempt.status == OrchestrationV2RunAttemptStatus::Running);
            if !is_running(current.provider_turn.as_ref()) && !attempt_running {
                return Ok(None);
            }
            tokio::time::sleep(SUBAGENT_STOP_POLL).await;
        }
        Ok(None)
    }

    /// `steer`. Delivers a steering message to the running turn. A message
    /// that belongs to a delegated completion is delivered only while that
    /// cohort still owns it. Otherwise steering is a silent no-op.
    pub async fn steer(&self, input: SteerInput) -> Result<(), ProviderTurnControlError> {
        use ProviderTurnControlOperation::Steer;
        let execution = Execution {
            thread_id: input.thread_id.clone(),
            provider_session_id: input.provider_session_id.clone(),
            replacement_provider_session_id: None,
            provider_thread_id: input.provider_thread_id.clone(),
            provider_turn_id: input.provider_turn_id.clone(),
            message_id: Some(input.message_id.clone()),
        };
        let context = self
            .reads
            .provider_control_context(&input.thread_id, &execution.control_target(None))
            .await
            .map_err(|error| execution.fail(Steer, error, None))?;
        if let Some(ownership) = context
            .message
            .as_ref()
            .and_then(|message| message.delegated_completion.as_ref())
        {
            let cohort = self
                .reads
                .run_delegated_completion(&input.thread_id, &ownership.parent_run_id)
                .await
                .map_err(|error| execution.fail(Steer, error, None))?;
            let owns_message = cohort.is_some_and(|cohort| {
                cohort.disposition
                    == turin_contracts::orchestration_v2::OrchestrationV2DelegatedCompletionCohortDisposition::Open
                    && cohort.delivery.is_some_and(|delivery| {
                        delivery.message_id == input.message_id
                            && delivery.generation == ownership.generation
                            && !delivery.task_ids.is_empty()
                    })
            });
            if !owns_message {
                return Ok(());
            }
        }
        let loaded = self.load(&execution, Steer).await?;
        let Some(session) = loaded.session else {
            return Ok(());
        };
        let (Some(message), Some(run)) = (loaded.context.message, loaded.context.run) else {
            return Err(execution.fail(
                Steer,
                "The persisted steering message or target run is missing.",
                None,
            ));
        };
        let records = message
            .context
            .as_ref()
            .map(|context| context.records.as_slice())
            .unwrap_or_default();
        let text = project_composer_context_for_provider(&message.text, records);
        let delivered = session
            .steer_turn(ProviderAdapterV2SteerInput {
                thread_id: input.thread_id.clone(),
                run_id: run.id.clone(),
                provider_thread: loaded.provider_thread,
                provider_turn_id: loaded.provider_turn.id.clone(),
                message: ProviderAdapterV2TurnMessage {
                    message_id: message.id.clone(),
                    text,
                    attachments: message.attachments.clone(),
                    created_by: message.created_by,
                    creation_source: message.creation_source,
                    scheduled_task_id: message.scheduled_task_id.clone(),
                    sender_thread_id: message.sender_thread_id.clone(),
                },
            })
            .await;
        match delivered {
            Ok(()) => Ok(()),
            Err(error) => {
                // A failed steer may only mean the turn ended first. Read it back
                // so the caller can tell the two apart.
                let turn_completed = self
                    .reads
                    .provider_control_context(&input.thread_id, &execution.control_target(None))
                    .await
                    .ok()
                    .and_then(|current| current.provider_turn)
                    .map(|turn| turn.status == OrchestrationV2ProviderTurnStatus::Completed);
                Err(execution.fail(Steer, error, turn_completed))
            }
        }
    }

    /// `interruptAndAwaitTerminal`. Interrupts a turn a restart is replacing,
    /// then waits until both the turn and its attempt are no longer running.
    pub async fn interrupt_and_await_terminal(
        &self,
        input: AwaitTerminalInput,
    ) -> Result<(), ProviderTurnControlError> {
        use ProviderTurnControlOperation::Restart;
        let execution = Execution {
            thread_id: input.thread_id.clone(),
            provider_session_id: input.provider_session_id.clone(),
            replacement_provider_session_id: input.replacement_provider_session_id.clone(),
            provider_thread_id: input.provider_thread_id.clone(),
            provider_turn_id: input.provider_turn_id.clone(),
            message_id: None,
        };
        let loaded = self.load(&execution, Restart).await?;
        let Some(session) = loaded.session else {
            // No live adapter can emit a terminal update. Do not wait for one, or
            // the restart stalls. Detach and start proceed.
            if loaded.provider_turn.status == OrchestrationV2ProviderTurnStatus::Running {
                tracing::warn!(
                    thread_id = %input.thread_id.as_str(),
                    provider_turn_id = %input.provider_turn_id.as_str(),
                    "provider restart interrupt skipped; no live session for a still-projected running turn"
                );
            }
            return Ok(());
        };
        session
            .interrupt_turn(ProviderAdapterV2InterruptInput {
                provider_thread: loaded.provider_thread,
                provider_turn_id: loaded.provider_turn.id.clone(),
                request_runtime_restart: None,
            })
            .await
            .map_err(|error| execution.fail(Restart, error, None))?;
        let target = ProviderControlTarget {
            provider_thread_id: input.provider_thread_id.clone(),
            provider_turn_id: input.provider_turn_id.clone(),
            attempt_id: Some(input.interrupted_attempt_id.clone()),
            message_id: None,
        };
        for _ in 0..TERMINAL_WAIT_YIELDS {
            let current = self
                .reads
                .provider_control_context(&input.thread_id, &target)
                .await
                .map_err(|error| execution.fail(Restart, error, None))?;
            let attempt_done = current
                .attempt
                .as_ref()
                .is_some_and(|attempt| attempt.status != OrchestrationV2RunAttemptStatus::Running);
            let turn_done = current
                .provider_turn
                .as_ref()
                .is_some_and(|turn| turn.status != OrchestrationV2ProviderTurnStatus::Running);
            if turn_done && attempt_done {
                return Ok(());
            }
            // Terminal events are projected on other tasks. Yield so they can run.
            tokio::task::yield_now().await;
        }
        Err(execution.fail(
            Restart,
            format!(
                "Provider turn {} did not terminalize before restart.",
                input.provider_turn_id.as_str()
            ),
            None,
        ))
    }

    /// `load`. Checks the recorded target, then resolves the session. An
    /// interrupt or restart tolerates a missing session. A steer does not.
    async fn load(
        &self,
        execution: &Execution,
        operation: ProviderTurnControlOperation,
    ) -> Result<Loaded, ProviderTurnControlError> {
        use ProviderTurnControlOperation::{Interrupt, Restart, Steer};
        let context = self
            .reads
            .provider_control_context(&execution.thread_id, &execution.control_target(None))
            .await
            .map_err(|error| execution.fail(operation, error, None))?;
        let (Some(provider_thread), Some(provider_turn)) = (
            context.provider_thread.clone(),
            context.provider_turn.clone(),
        ) else {
            return Err(invalid_target(execution, operation));
        };
        let targets_recorded_session =
            provider_thread.provider_session_id.as_ref() == Some(&execution.provider_session_id);
        // A restart-session command commits the replacement binding before its
        // process-bound effect runs. The old live runtime must still get the
        // interrupt, but only when the projection matches the exact replacement.
        let targets_committed_replacement = operation == Restart
            && execution.replacement_provider_session_id.is_some()
            && provider_thread.provider_session_id.as_ref()
                == execution.replacement_provider_session_id.as_ref();
        if (!targets_recorded_session && !targets_committed_replacement)
            || provider_turn.provider_thread_id != provider_thread.id
        {
            return Err(invalid_target(execution, operation));
        }
        let interrupt_provider_thread = if targets_recorded_session {
            provider_thread
        } else {
            OrchestrationV2ProviderThread {
                provider_session_id: Some(execution.provider_session_id.clone()),
                ..provider_thread
            }
        };
        if provider_turn.status != OrchestrationV2ProviderTurnStatus::Running {
            if operation == Steer {
                return Err(execution.fail(
                    Steer,
                    "The provider turn ended before the steering message was delivered.",
                    Some(provider_turn.status == OrchestrationV2ProviderTurnStatus::Completed),
                ));
            }
            return Ok(Loaded {
                context,
                provider_thread: interrupt_provider_thread,
                provider_turn,
                session: None,
            });
        }
        let session = self
            .sessions
            .get_session(&execution.provider_session_id)
            .await
            .map_err(|error| execution.fail(operation, error, None))?;
        match session {
            Some(session) => Ok(Loaded {
                context,
                provider_thread: interrupt_provider_thread,
                provider_turn,
                session: Some(session),
            }),
            None if operation == Interrupt || operation == Restart => {
                tracing::warn!(
                    thread_id = %execution.thread_id.as_str(),
                    operation = operation.as_str(),
                    provider_session_id = %execution.provider_session_id.as_str(),
                    provider_turn_id = %execution.provider_turn_id.as_str(),
                    "provider interrupt/restart found no live session; treating as already stopped"
                );
                Ok(Loaded {
                    context,
                    provider_thread: interrupt_provider_thread,
                    provider_turn,
                    session: None,
                })
            }
            None => Err(execution.fail(
                operation,
                format!(
                    "Provider session {} is not active.",
                    execution.provider_session_id.as_str()
                ),
                None,
            )),
        }
    }
}

fn invalid_target(
    execution: &Execution,
    operation: ProviderTurnControlOperation,
) -> ProviderTurnControlError {
    execution.fail(
        operation,
        "The recorded provider execution target is no longer valid.",
        None,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;
    use parking_lot::Mutex;
    use serde_json::json;
    use turin_contracts::{
        RunId,
        orchestration_v2::{
            OrchestrationV2ProviderSession, OrchestrationV2ProviderTurn, OrchestrationV2RunAttempt,
        },
        provider_instance::{ProviderDriverKind, ProviderInstanceId},
    };
    use turin_provider_core::provider_adapter::{
        ProviderAdapterV2EnsureThreadInput, ProviderAdapterV2Error,
        ProviderAdapterV2InterruptInput, ProviderAdapterV2ReadThreadSnapshotInput,
        ProviderAdapterV2ResumeThreadInput, ProviderAdapterV2RuntimeRequestResponseInput,
        ProviderAdapterV2SteerInput, ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2TurnInput,
        ProviderEventStream,
    };

    use super::*;
    use crate::core::projection_reads::{ProviderControlContext, ProviderControlReads};
    use crate::pipeline::orchestrator_port::BoxError;

    const THREAD: &str = "thread:restart-session";
    const OLD_SESSION: &str = "provider-session:restart-session:old";
    const REPLACEMENT_SESSION: &str = "provider-session:restart-session:replacement";
    const UNRELATED_SESSION: &str = "provider-session:restart-session:unrelated";
    const PROVIDER_THREAD: &str = "provider-thread:restart-session";
    const PROVIDER_TURN: &str = "provider-turn:restart-session";
    const ATTEMPT: &str = "run-attempt:restart-session";
    const T0: &str = "2026-06-20T00:00:00.000Z";

    /// The projected records the control reads serve. The fake runtime settles
    /// the turn and attempt when interrupted, as the real adapter's events would.
    struct State {
        provider_thread: OrchestrationV2ProviderThread,
        turn: OrchestrationV2ProviderTurn,
        attempt: OrchestrationV2RunAttempt,
    }

    fn state() -> State {
        // The restart command has already projected the replacement binding
        // before the process-bound restart effect executes.
        let provider_thread: OrchestrationV2ProviderThread = serde_json::from_value(json!({
            "id": PROVIDER_THREAD,
            "driver": "codex",
            "providerInstanceId": "codex",
            "providerSessionId": REPLACEMENT_SESSION,
            "appThreadId": THREAD,
            "ownerNodeId": null,
            "nativeThreadRef": {"driver": "codex", "nativeId": "native-thread:restart-session", "strength": "strong"},
            "nativeConversationHeadRef": null,
            "status": "not_loaded",
            "firstRunOrdinal": 1,
            "lastRunOrdinal": 1,
            "handoffIds": [],
            "forkedFrom": null,
            "createdAt": T0,
            "updatedAt": T0,
        }))
        .expect("provider thread fixture");
        let turn: OrchestrationV2ProviderTurn = serde_json::from_value(json!({
            "id": PROVIDER_TURN,
            "providerThreadId": PROVIDER_THREAD,
            "nodeId": "node:restart-session",
            "runAttemptId": ATTEMPT,
            "nativeTurnRef": {"driver": "codex", "nativeId": "native-turn:restart-session", "strength": "strong"},
            "ordinal": 1,
            "status": "running",
            "startedAt": T0,
            "completedAt": null,
            "tokenUsage": null,
            "turnTokenUsage": null,
        }))
        .expect("provider turn fixture");
        let attempt: OrchestrationV2RunAttempt = serde_json::from_value(json!({
            "id": ATTEMPT,
            "runId": "run:restart-session",
            "attemptOrdinal": 1,
            "rootNodeId": "node:restart-session",
            "providerInstanceId": "codex",
            "providerThreadId": PROVIDER_THREAD,
            "providerTurnId": PROVIDER_TURN,
            "reason": "initial",
            "status": "running",
            "startedAt": T0,
            "completedAt": null,
        }))
        .expect("attempt fixture");
        State {
            provider_thread,
            turn,
            attempt,
        }
    }

    struct FakeReads {
        state: Arc<Mutex<State>>,
    }

    #[async_trait]
    impl ProviderControlReads for FakeReads {
        async fn provider_control_context(
            &self,
            _thread_id: &ThreadId,
            target: &ProviderControlTarget,
        ) -> Result<ProviderControlContext, BoxError> {
            let state = self.state.lock();
            Ok(ProviderControlContext {
                provider_thread: (state.provider_thread.id == target.provider_thread_id)
                    .then(|| state.provider_thread.clone()),
                provider_turn: (state.turn.id == target.provider_turn_id)
                    .then(|| state.turn.clone()),
                attempt: target
                    .attempt_id
                    .as_ref()
                    .filter(|id| **id == state.attempt.id)
                    .map(|_| state.attempt.clone()),
                message: None,
                run: None,
            })
        }

        async fn run_delegated_completion(
            &self,
            _thread_id: &ThreadId,
            _run_id: &RunId,
        ) -> Result<
            Option<turin_contracts::orchestration_v2::OrchestrationV2DelegatedCompletionCohort>,
            BoxError,
        > {
            Ok(None)
        }
    }

    /// Serves only the one live session. Every other session is gone.
    struct FakeSessions {
        live: ProviderSessionId,
        runtime: Arc<dyn ProviderSessionRuntime>,
    }

    #[async_trait]
    impl LiveSessions for FakeSessions {
        async fn get_session(
            &self,
            provider_session_id: &ProviderSessionId,
        ) -> Result<Option<Arc<dyn ProviderSessionRuntime>>, BoxError> {
            Ok((provider_session_id == &self.live).then(|| self.runtime.clone()))
        }
    }

    /// Records the provider thread it was asked to interrupt. The interrupt
    /// settles the turn and attempt in the shared state.
    struct FakeRuntime {
        state: Arc<Mutex<State>>,
        session_id: ProviderSessionId,
        session: OrchestrationV2ProviderSession,
        interrupted: StdMutex<Option<OrchestrationV2ProviderThread>>,
    }

    fn unused() -> ! {
        unreachable!("ProviderTurnControlService only calls interrupt_turn on this fake")
    }

    #[async_trait]
    impl ProviderSessionRuntime for FakeRuntime {
        fn instance_id(&self) -> &ProviderInstanceId {
            &self.session.provider_instance_id
        }
        fn driver(&self) -> &ProviderDriverKind {
            &self.session.driver
        }
        fn provider_session_id(&self) -> &ProviderSessionId {
            &self.session_id
        }
        fn provider_session(&self) -> &OrchestrationV2ProviderSession {
            &self.session
        }
        fn events(&self) -> ProviderEventStream {
            Box::pin(futures::stream::empty())
        }
        async fn ensure_thread(
            &self,
            _input: ProviderAdapterV2EnsureThreadInput,
        ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
            unused()
        }
        async fn resume_thread(
            &self,
            _input: ProviderAdapterV2ResumeThreadInput,
        ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
            unused()
        }
        async fn start_turn(
            &self,
            _input: ProviderAdapterV2TurnInput,
        ) -> Result<(), ProviderAdapterV2Error> {
            unused()
        }
        async fn steer_turn(
            &self,
            _input: ProviderAdapterV2SteerInput,
        ) -> Result<(), ProviderAdapterV2Error> {
            unused()
        }
        async fn interrupt_turn(
            &self,
            input: ProviderAdapterV2InterruptInput,
        ) -> Result<(), ProviderAdapterV2Error> {
            *self.interrupted.lock().expect("lock") = Some(input.provider_thread);
            let mut state = self.state.lock();
            state.turn.status = OrchestrationV2ProviderTurnStatus::Interrupted;
            state.attempt.status = OrchestrationV2RunAttemptStatus::Superseded;
            Ok(())
        }
        async fn respond_to_runtime_request(
            &self,
            _input: ProviderAdapterV2RuntimeRequestResponseInput,
        ) -> Result<(), ProviderAdapterV2Error> {
            unused()
        }
        async fn read_thread_snapshot(
            &self,
            _input: ProviderAdapterV2ReadThreadSnapshotInput,
        ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
            unused()
        }
        async fn rollback_thread(
            &self,
            _input: turin_provider_core::provider_adapter::ProviderAdapterV2RollbackThreadInput,
        ) -> Result<ProviderAdapterV2ThreadSnapshot, ProviderAdapterV2Error> {
            unused()
        }
        async fn fork_thread(
            &self,
            _input: turin_provider_core::provider_adapter::ProviderAdapterV2ForkThreadInput,
        ) -> Result<OrchestrationV2ProviderThread, ProviderAdapterV2Error> {
            unused()
        }
    }

    fn session(id: &str) -> OrchestrationV2ProviderSession {
        serde_json::from_value(json!({
            "id": id,
            "driver": "codex",
            "providerInstanceId": "codex",
            "status": "running",
            "cwd": "/workspace",
            "model": "gpt-5.4",
            "capabilities": crate::projection::test_support::capabilities(),
            "createdAt": T0,
            "updatedAt": T0,
            "lastError": null,
        }))
        .expect("session fixture")
    }

    fn thread() -> ThreadId {
        ThreadId::new(THREAD).expect("thread id")
    }

    fn provider_thread() -> ProviderThreadId {
        ProviderThreadId::new(PROVIDER_THREAD).expect("provider thread id")
    }

    fn provider_turn() -> ProviderTurnId {
        ProviderTurnId::new(PROVIDER_TURN).expect("provider turn id")
    }

    fn session_id(value: &str) -> ProviderSessionId {
        ProviderSessionId::new(value).expect("session id")
    }

    /// The restart and the ordinary interrupt both run against the same state.
    fn service(state: Arc<Mutex<State>>, runtime: Arc<FakeRuntime>) -> ProviderTurnControlService {
        ProviderTurnControlService::new(
            Arc::new(FakeReads {
                state: state.clone(),
            }),
            Arc::new(FakeSessions {
                live: session_id(OLD_SESSION),
                runtime,
            }),
        )
    }

    #[tokio::test]
    async fn interrupts_the_historical_session_only_for_the_exact_committed_restart_replacement() {
        let shared = Arc::new(Mutex::new(state()));
        let runtime = Arc::new(FakeRuntime {
            state: shared.clone(),
            session_id: session_id(OLD_SESSION),
            session: session(OLD_SESSION),
            interrupted: StdMutex::new(None),
        });
        let control = service(shared.clone(), runtime.clone());

        // An ordinary interrupt from the historical session is refused: the
        // projection binds the replacement.
        let ordinary = control
            .interrupt(InterruptInput {
                thread_id: thread(),
                provider_session_id: session_id(OLD_SESSION),
                provider_thread_id: provider_thread(),
                provider_turn_id: provider_turn(),
                subagent: None,
            })
            .await;
        assert!(ordinary.is_err());

        // A restart naming an unrelated replacement is refused too.
        let unrelated = control
            .interrupt_and_await_terminal(AwaitTerminalInput {
                thread_id: thread(),
                provider_session_id: session_id(OLD_SESSION),
                replacement_provider_session_id: Some(session_id(UNRELATED_SESSION)),
                provider_thread_id: provider_thread(),
                provider_turn_id: provider_turn(),
                interrupted_attempt_id: RunAttemptId::new(ATTEMPT).expect("attempt id"),
            })
            .await;
        assert!(unrelated.is_err());
        assert!(runtime.interrupted.lock().expect("lock").is_none());

        // The exact committed replacement interrupts the old runtime, and the
        // thread is presented under the old session it runs in.
        control
            .interrupt_and_await_terminal(AwaitTerminalInput {
                thread_id: thread(),
                provider_session_id: session_id(OLD_SESSION),
                replacement_provider_session_id: Some(session_id(REPLACEMENT_SESSION)),
                provider_thread_id: provider_thread(),
                provider_turn_id: provider_turn(),
                interrupted_attempt_id: RunAttemptId::new(ATTEMPT).expect("attempt id"),
            })
            .await
            .expect("restart interrupt");
        let interrupted = runtime
            .interrupted
            .lock()
            .expect("lock")
            .clone()
            .expect("interrupted thread");
        assert_eq!(
            interrupted.provider_session_id,
            Some(session_id(OLD_SESSION))
        );
        assert_eq!(interrupted.id, provider_thread());
        assert_eq!(
            interrupted
                .native_thread_ref
                .and_then(|native| native.native_id)
                .map(|id| id.as_str().to_owned())
                .as_deref(),
            Some("native-thread:restart-session")
        );
    }
}
