//! Port of upstream/apps/server/src/orchestration-v2/EffectWorker.ts
//! (`OrchestrationEffectWorkerV2`, `runDaemonWithOptions`, and the executor
//! contract; the executor's per-effect dispatch is `effect_executor.rs`).
//!
//! The executor's future must stop the underlying process or operation when
//! dropped: a cancelled effect drops it (the cancellation race), and so does
//! the daemon's shutdown. The daemon owns every worker loop in a `JoinSet`.
//!
//! Metrics (`orchestrationEffectClaimsTotal`, `orchestrationEffectQueueWait`)
//! are not ported: telemetry is cut.

use super::effect_outbox::{
    Effect, EffectOutbox, EffectStatus, EffectType, REPLAY_SAFE_EFFECT_TYPES_AFTER_PROCESS_LOSS,
};
use super::task::OwnedTask;
use async_trait::async_trait;
use chrono::Utc;
use futures::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration as StdDuration;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use turin_persistence::PersistenceError;

pub const DEFAULT_EFFECT_WORKER_CONCURRENCY: usize = 4;
pub const DEFAULT_EFFECT_WORKER_LIVENESS_POLL_INTERVAL_MS: u64 = 30_000;

/// `OrchestrationEffectExecutionError`.
#[derive(Debug, thiserror::Error)]
#[error("Orchestration effect {effect_type} ({effect_id}) failed: {cause}")]
pub struct EffectExecutionError {
    pub effect_id: String,
    pub effect_type: &'static str,
    #[source]
    pub cause: Box<dyn std::error::Error + Send + Sync>,
}

impl EffectExecutionError {
    pub fn new(
        effect: &Effect,
        cause: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self {
            effect_id: effect.id.clone(),
            effect_type: effect.request.effect_type().as_str(),
            cause: cause.into(),
        }
    }
}

/// The error and its whole source chain, the way `Cause.pretty` shows nested
/// causes. Non-retryable-failure matching and the stored `last_error` use it.
pub fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(next) = source {
        let next_text = next.to_string();
        if !text.ends_with(&next_text) {
            text.push_str(": ");
            text.push_str(&next_text);
        }
        source = next.source();
    }
    text
}

/// `OrchestrationEffectExecutorV2`.
#[async_trait]
pub trait EffectExecutor: Send + Sync {
    /// Runs one claimed effect. `will_retry` is true when the worker will
    /// retry a failure, so a step can fail and try again instead of settling
    /// the run.
    async fn execute(&self, effect: &Effect, will_retry: bool) -> Result<(), EffectExecutionError>;
}

/// `OrchestrationEffectWorkerError`.
#[derive(Debug, thiserror::Error)]
pub enum EffectWorkerError {
    #[error("Orchestration effect worker {operation} failed{}", effect_suffix(.effect_id))]
    Operation {
        operation: &'static str,
        effect_id: Option<String>,
        #[source]
        cause: PersistenceError,
    },
    #[error("Orchestration effect worker {operation} failed for {effect_id}: The worker no longer owns the effect lease.")]
    LostLease {
        operation: &'static str,
        effect_id: String,
    },
}

fn effect_suffix(effect_id: &Option<String>) -> String {
    effect_id
        .as_ref()
        .map_or_else(String::new, |id| format!(" for {id}"))
}

#[derive(Debug, Clone)]
pub struct EffectWorkerOptions {
    pub worker_id: String,
    pub lease_duration_ms: i64,
    pub max_attempts: i64,
}

impl Default for EffectWorkerOptions {
    fn default() -> Self {
        Self {
            worker_id: format!("orchestration-v2:{}", std::process::id()),
            lease_duration_ms: 30_000,
            max_attempts: 5,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EffectDaemonOptions {
    pub concurrency: usize,
    pub liveness_poll_interval: StdDuration,
}

impl Default for EffectDaemonOptions {
    fn default() -> Self {
        Self {
            concurrency: DEFAULT_EFFECT_WORKER_CONCURRENCY,
            liveness_poll_interval: StdDuration::from_millis(
                DEFAULT_EFFECT_WORKER_LIVENESS_POLL_INTERVAL_MS,
            ),
        }
    }
}

#[derive(Clone)]
pub struct EffectWorker {
    outbox: EffectOutbox,
    executor: Arc<dyn EffectExecutor>,
    options: EffectWorkerOptions,
}

impl EffectWorker {
    pub fn new(
        outbox: EffectOutbox,
        executor: Arc<dyn EffectExecutor>,
        mut options: EffectWorkerOptions,
    ) -> Self {
        options.lease_duration_ms = options.lease_duration_ms.max(1);
        options.max_attempts = options.max_attempts.max(1);
        Self {
            outbox,
            executor,
            options,
        }
    }

    pub async fn await_work(&self) {
        self.outbox.await_available().await;
    }

    pub async fn run_once(&self) -> Result<bool, EffectWorkerError> {
        self.run_once_inner(false).await
    }

    /// Like `run_once`, but claims skip restart continuations (startup
    /// recovery runs those after the rest of the queue has drained).
    pub async fn run_recovery_once(&self) -> Result<bool, EffectWorkerError> {
        self.run_once_inner(true).await
    }

    pub async fn next_claimable_at(
        &self,
    ) -> Result<Option<chrono::DateTime<Utc>>, EffectWorkerError> {
        self.outbox
            .next_claimable_at()
            .await
            .map_err(|cause| EffectWorkerError::Operation {
                operation: "next-claimable",
                effect_id: None,
                cause,
            })
    }

    /// Runs effects until none is claimable or `max_effects` ran.
    pub async fn drain(&self, max_effects: usize) -> Result<usize, EffectWorkerError> {
        let mut completed = 0;
        while completed < max_effects && self.run_once().await? {
            completed += 1;
        }
        Ok(completed)
    }

    async fn was_cancelled(&self, id: &str) -> Result<bool, PersistenceError> {
        Ok(self
            .outbox
            .get(id.to_owned())
            .await?
            .is_some_and(|effect| effect.status == EffectStatus::Cancelled))
    }

    /// The worker failed after claiming but before the effect ran: hand the
    /// claim back immediately so another attempt (or worker) picks it up.
    async fn requeue_claim(&self, effect: &Effect, error: &str) {
        let outcome = self
            .outbox
            .retry(
                effect.id.clone(),
                self.options.worker_id.clone(),
                format!("Worker failed before settling the claimed effect: {error}"),
                0,
            )
            .await;
        match outcome {
            Ok(true) => tracing::warn!(
                effect_id = %effect.id,
                effect_type = effect.request.effect_type().as_str(),
                "Requeued effect after unexpected worker failure"
            ),
            Ok(false) => tracing::warn!(
                effect_id = %effect.id,
                effect_type = effect.request.effect_type().as_str(),
                "Could not requeue effect after worker lost its lease"
            ),
            Err(requeue_error) => tracing::error!(
                effect_id = %effect.id,
                effect_type = effect.request.effect_type().as_str(),
                error = %requeue_error,
                "Failed to requeue effect after unexpected worker failure"
            ),
        }
    }

    /// A process-bound effect already started; running it again is unsafe, so
    /// a settlement failure marks it failed instead of requeueing.
    async fn terminalize_claim(&self, effect: &Effect, error: &str) {
        let outcome = self
            .outbox
            .fail(
                effect.id.clone(),
                self.options.worker_id.clone(),
                format!(
                    "Worker failed to settle a process-bound effect after execution started: {error}"
                ),
            )
            .await;
        match outcome {
            Ok(true) => tracing::error!(
                effect_id = %effect.id,
                effect_type = effect.request.effect_type().as_str(),
                "Terminalized process-bound effect after settlement failure"
            ),
            Ok(false) => tracing::warn!(
                effect_id = %effect.id,
                effect_type = effect.request.effect_type().as_str(),
                "Could not terminalize process-bound effect after worker lost its lease"
            ),
            Err(fail_error) => tracing::error!(
                effect_id = %effect.id,
                effect_type = effect.request.effect_type().as_str(),
                error = %fail_error,
                "Failed to terminalize process-bound effect after settlement failure"
            ),
        }
    }

    async fn recover_post_success_settlement(&self, effect: &Effect, error: &str) {
        if REPLAY_SAFE_EFFECT_TYPES_AFTER_PROCESS_LOSS.contains(&effect.request.effect_type()) {
            self.requeue_claim(effect, error).await;
        } else {
            self.terminalize_claim(effect, error).await;
        }
    }

    async fn run_once_inner(
        &self,
        exclude_restart_continuations: bool,
    ) -> Result<bool, EffectWorkerError> {
        let claimed = self
            .outbox
            .claim_next(
                self.options.worker_id.clone(),
                self.options.lease_duration_ms,
                exclude_restart_continuations,
            )
            .await
            .map_err(|cause| EffectWorkerError::Operation {
                operation: "run",
                effect_id: None,
                cause,
            })?;
        let Some(effect) = claimed else {
            return Ok(false);
        };

        // Arm the process-local cancellation signal before re-reading durable
        // state. A cancellation that commits after the row read has begun can
        // then still win the execution race instead of falling into the gap
        // between the read and signal registration.
        let cancellation: CancellationToken = self.outbox.cancellation_signal(&effect.id);
        // Cancellation can commit after the durable claim but before the
        // process-local signal is registered. Re-read the authoritative row
        // once before starting external work; later cancellations use the
        // signal raced below.
        match self.was_cancelled(&effect.id).await {
            Ok(true) => {
                self.outbox.clear_cancellation(&effect.id);
                return Ok(true);
            }
            Ok(false) => {}
            Err(cause) => {
                self.requeue_claim(&effect, &cause.to_string()).await;
                return Err(EffectWorkerError::Operation {
                    operation: "run",
                    effect_id: Some(effect.id.clone()),
                    cause,
                });
            }
        }

        let will_retry = effect.attempt_count < self.options.max_attempts;
        // A panic in an executor is a defect, which Effect.exit also folds
        // into a failed exit: it retries like any other failure.
        let execution = AssertUnwindSafe(self.executor.execute(&effect, will_retry)).catch_unwind();
        tokio::pin!(execution);
        let outcome = tokio::select! {
            result = &mut execution => Some(result),
            () = cancellation.cancelled() => None,
        };
        self.outbox.clear_cancellation(&effect.id);
        let Some(outcome) = outcome else {
            // Cancelled mid-flight: the execution future was dropped above.
            return Ok(true);
        };

        let failure = match outcome {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error_chain(&error)),
            Err(panic) => Some(format!("Defect: {}", panic_message(&panic))),
        };

        let Some(error) = failure else {
            let settled = async {
                let completed = self
                    .outbox
                    .succeed(effect.id.clone(), self.options.worker_id.clone())
                    .await
                    .map_err(|cause| EffectWorkerError::Operation {
                        operation: "run",
                        effect_id: Some(effect.id.clone()),
                        cause,
                    })?;
                if completed {
                    return Ok(true);
                }
                let cancelled = self.was_cancelled(&effect.id).await.map_err(|cause| {
                    EffectWorkerError::Operation {
                        operation: "run",
                        effect_id: Some(effect.id.clone()),
                        cause,
                    }
                })?;
                if cancelled {
                    Ok(true)
                } else {
                    Err(EffectWorkerError::LostLease {
                        operation: "complete",
                        effect_id: effect.id.clone(),
                    })
                }
            }
            .await;
            if let Err(error) = &settled {
                self.recover_post_success_settlement(&effect, &error.to_string())
                    .await;
            }
            return settled;
        };

        let effect_type = effect.request.effect_type().as_str();
        let non_retryable = is_non_retryable_provider_turn_control_failure(effect_type, &error);
        tracing::warn!(
            effect_id = %effect.id,
            effect_type,
            attempt_count = effect.attempt_count,
            non_retryable,
            %error,
            "Orchestration effect execution failed"
        );
        // Prefer succeed for terminal interrupt races so the outbox does not
        // keep a failed interrupt around; fail only when we must not retry.
        let updated = if non_retryable {
            self.outbox
                .succeed(effect.id.clone(), self.options.worker_id.clone())
                .await
        } else if effect.attempt_count >= self.options.max_attempts {
            self.outbox
                .fail(
                    effect.id.clone(),
                    self.options.worker_id.clone(),
                    error.clone(),
                )
                .await
        } else {
            self.outbox
                .retry(
                    effect.id.clone(),
                    self.options.worker_id.clone(),
                    error.clone(),
                    retry_delay_ms(effect.attempt_count),
                )
                .await
        };
        let updated = match updated {
            Ok(updated) => updated,
            Err(cause) => {
                if non_retryable || effect.attempt_count >= self.options.max_attempts {
                    self.terminalize_claim(&effect, &cause.to_string()).await;
                } else {
                    self.requeue_claim(&effect, &cause.to_string()).await;
                }
                return Err(EffectWorkerError::Operation {
                    operation: "run",
                    effect_id: Some(effect.id.clone()),
                    cause,
                });
            }
        };
        if !updated {
            let cancelled = self.was_cancelled(&effect.id).await.map_err(|cause| {
                EffectWorkerError::Operation {
                    operation: "run",
                    effect_id: Some(effect.id.clone()),
                    cause,
                }
            })?;
            if cancelled {
                return Ok(true);
            }
            return Err(EffectWorkerError::LostLease {
                operation: "reschedule",
                effect_id: effect.id.clone(),
            });
        }
        Ok(true)
    }

    /// Runs `concurrency` worker loops until `shutdown` fires. Post-commit
    /// notifications are the low-latency path; `available_at` is the durable
    /// retry schedule, and the long liveness poll only recovers from a missed
    /// in-process notification or work inserted by another process.
    pub async fn run_daemon(&self, options: EffectDaemonOptions, shutdown: CancellationToken) {
        let concurrency = options.concurrency.max(1);
        let liveness = options
            .liveness_poll_interval
            .max(StdDuration::from_millis(1));
        let mut workers = JoinSet::new();
        for _ in 0..concurrency {
            let worker = self.clone();
            let shutdown = shutdown.clone();
            workers.spawn(async move { worker.worker_loop(liveness, shutdown).await });
        }
        shutdown.cancelled().await;
        workers.abort_all();
        while workers.join_next().await.is_some() {}
    }

    /// `run_daemon` as an owned task: dropping the handle stops the workers.
    pub fn spawn_daemon(&self, options: EffectDaemonOptions) -> EffectDaemon {
        let shutdown = CancellationToken::new();
        let worker = self.clone();
        let token = shutdown.clone();
        EffectDaemon {
            _task: OwnedTask::spawn(async move { worker.run_daemon(options, token).await }),
            shutdown,
        }
    }

    async fn worker_loop(&self, liveness: StdDuration, shutdown: CancellationToken) {
        loop {
            if shutdown.is_cancelled() {
                return;
            }
            match self.run_once().await {
                Ok(true) => {
                    tokio::task::yield_now().await;
                    continue;
                }
                Err(error) => {
                    tracing::warn!(%error, "Orchestration effect worker failed");
                    // A due row can remain visible when a claim UPDATE fails.
                    // Do not feed that past deadline back into the scheduler
                    // and retry at the one-millisecond floor; let transient
                    // database failures cool off.
                    tokio::select! {
                        () = shutdown.cancelled() => return,
                        () = tokio::time::sleep(liveness.min(StdDuration::from_secs(1))) => {}
                    }
                    continue;
                }
                Ok(false) => {}
            }
            let next = match self.next_claimable_at().await {
                Ok(next) => next,
                Err(error) => {
                    tracing::warn!(%error, "Failed to read the next orchestration effect deadline");
                    None
                }
            };
            let sleep = match next {
                None => liveness,
                Some(at) => {
                    let until = (at - Utc::now()).num_milliseconds();
                    liveness.min(if until > 0 {
                        StdDuration::from_millis(until as u64)
                    } else {
                        StdDuration::from_millis(25)
                    })
                }
            };
            tokio::select! {
                () = shutdown.cancelled() => return,
                () = self.outbox.await_available() => {}
                () = tokio::time::sleep(sleep) => {}
            }
        }
    }
}

/// Handle of a running daemon; dropping it stops every worker loop.
pub struct EffectDaemon {
    _task: OwnedTask,
    shutdown: CancellationToken,
}

impl EffectDaemon {
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "effect executor panicked".to_owned())
}

/// `min(30_000, 100 * 2 ** max(0, attemptCount - 1))`.
fn retry_delay_ms(attempt_count: i64) -> i64 {
    let exponent = u32::try_from((attempt_count - 1).max(0))
        .unwrap_or(31)
        .min(31);
    100_i64.saturating_mul(1_i64 << exponent).min(30_000)
}

/// Pure interrupt races with hard process teardown or a dead session produce
/// "not active" protocol errors. Retrying those only delays recovery.
///
/// Do not apply this to `provider-turn.restart`: that compound effect also
/// runs detach and start. Swallowing a start failure that happens to mention
/// "is not active" would drop the outbox item without ever starting the
/// replacement turn.
pub fn is_non_retryable_provider_turn_control_failure(effect_type: &str, error_text: &str) -> bool {
    if effect_type != EffectType::ProviderTurnInterrupt.as_str() {
        return false;
    }
    let lower = error_text.to_lowercase();
    [
        "is not active",
        "hard teardown is already in progress",
        "treating as already interrupted",
        "treating as already stopped",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::effect_outbox::{EffectRequest, PendingEffect};
    use parking_lot::Mutex;
    use turin_contracts::{
        CommandId, ProviderSessionId, ProviderThreadId, ProviderTurnId, RunId, ThreadId,
    };
    use turin_persistence::Db;

    #[test]
    fn terminal_interrupt_races_do_not_retry() {
        assert!(is_non_retryable_provider_turn_control_failure(
            "provider-turn.interrupt",
            "Turn is not active"
        ));
        assert!(is_non_retryable_provider_turn_control_failure(
            "provider-turn.interrupt",
            "Treating as already stopped"
        ));
        assert!(!is_non_retryable_provider_turn_control_failure(
            "provider-turn.steer",
            "Turn is not active"
        ));
        // A restart runs detach + start too; a start failure must still retry.
        assert!(!is_non_retryable_provider_turn_control_failure(
            "provider-turn.restart",
            "Turn is not active"
        ));
    }

    #[test]
    fn retry_delay_doubles_from_100ms_and_caps_at_30s() {
        assert_eq!(retry_delay_ms(1), 100);
        assert_eq!(retry_delay_ms(2), 200);
        assert_eq!(retry_delay_ms(3), 400);
        assert_eq!(retry_delay_ms(9), 25_600);
        assert_eq!(retry_delay_ms(10), 30_000);
        assert_eq!(retry_delay_ms(1_000), 30_000);
    }

    type Script = Box<dyn Fn(&Effect, bool) -> Result<(), String> + Send + Sync>;

    struct ScriptedExecutor {
        calls: Mutex<Vec<(String, bool)>>,
        script: Script,
    }

    #[async_trait]
    impl EffectExecutor for ScriptedExecutor {
        async fn execute(
            &self,
            effect: &Effect,
            will_retry: bool,
        ) -> Result<(), EffectExecutionError> {
            self.calls.lock().push((effect.id.clone(), will_retry));
            (self.script)(effect, will_retry).map_err(|message| EffectExecutionError::new(effect, message))
        }
    }

    fn interrupt(id: &str, command: &CommandId, thread: &ThreadId) -> PendingEffect {
        PendingEffect {
            id: id.into(),
            command_id: command.clone(),
            thread_id: thread.clone(),
            request: EffectRequest::ProviderTurnInterrupt {
                provider_session_id: ProviderSessionId::new("s").unwrap(),
                provider_thread_id: ProviderThreadId::new("t").unwrap(),
                provider_turn_id: ProviderTurnId::new("u").unwrap(),
                subagent: None,
            },
            available_at: None,
        }
    }

    fn start(id: &str, command: &CommandId, thread: &ThreadId) -> PendingEffect {
        PendingEffect {
            id: id.into(),
            command_id: command.clone(),
            thread_id: thread.clone(),
            request: EffectRequest::ProviderTurnStart {
                run_id: RunId::new("run").unwrap(),
            },
            available_at: None,
        }
    }

    async fn harness(
        script: Script,
        max_attempts: i64,
    ) -> (EffectOutbox, EffectWorker, Arc<ScriptedExecutor>) {
        let outbox = EffectOutbox::new(Db::open_in_memory().await.unwrap());
        let executor = Arc::new(ScriptedExecutor {
            calls: Mutex::new(vec![]),
            script,
        });
        let worker = EffectWorker::new(
            outbox.clone(),
            executor.clone(),
            EffectWorkerOptions {
                worker_id: "test-worker".into(),
                lease_duration_ms: 30_000,
                max_attempts,
            },
        );
        (outbox, worker, executor)
    }

    #[tokio::test]
    async fn a_succeeding_effect_is_settled_and_drain_stops_when_idle() {
        let (outbox, worker, executor) = harness(Box::new(|_, _| Ok(())), 5).await;
        let command = CommandId::new("cmd").unwrap();
        let thread = ThreadId::new("thread").unwrap();
        outbox
            .enqueue(vec![start("one", &command, &thread)])
            .await
            .unwrap();
        assert_eq!(worker.drain(usize::MAX).await.unwrap(), 1);
        assert_eq!(
            outbox.get("one".into()).await.unwrap().unwrap().status,
            EffectStatus::Succeeded
        );
        assert_eq!(executor.calls.lock().as_slice(), [("one".into(), true)]);
        assert!(!worker.run_once().await.unwrap());
    }

    #[tokio::test]
    async fn a_failing_effect_backs_off_then_fails_on_the_last_attempt() {
        let (outbox, worker, executor) = harness(Box::new(|_, _| Err("boom".into())), 2).await;
        let command = CommandId::new("cmd").unwrap();
        let thread = ThreadId::new("thread").unwrap();
        outbox
            .enqueue(vec![start("retrying", &command, &thread)])
            .await
            .unwrap();
        assert!(worker.run_once().await.unwrap());
        let after_first = outbox.get("retrying".into()).await.unwrap().unwrap();
        assert_eq!(after_first.status, EffectStatus::Pending);
        assert_eq!(after_first.attempt_count, 1);
        assert!(after_first.last_error.unwrap().contains("boom"));
        // The retry waits out its backoff, so nothing is claimable now.
        assert!(!worker.run_once().await.unwrap());
        // Make it due and run the last attempt.
        outbox
            .db()
            .write(|tx| {
                tx.execute(
                    "UPDATE orchestration_v2_effect_outbox SET available_at = '2000-01-01T00:00:00.000Z'",
                    [],
                )
                .unwrap();
                Ok(())
            })
            .await
            .unwrap();
        assert!(worker.run_once().await.unwrap());
        let last = outbox.get("retrying".into()).await.unwrap().unwrap();
        assert_eq!(last.status, EffectStatus::Failed);
        assert_eq!(last.attempt_count, 2);
        // willRetry is true only while attempts remain.
        assert_eq!(
            executor.calls.lock().as_slice(),
            [("retrying".into(), true), ("retrying".into(), false)]
        );
    }

    #[tokio::test]
    async fn a_terminal_interrupt_race_settles_as_succeeded() {
        let (outbox, worker, _) = harness(
            Box::new(|_, _| Err("Turn is not active".into())),
            5,
        )
        .await;
        let command = CommandId::new("cmd").unwrap();
        let thread = ThreadId::new("thread").unwrap();
        outbox
            .enqueue(vec![interrupt("stop", &command, &thread)])
            .await
            .unwrap();
        assert!(worker.run_once().await.unwrap());
        assert_eq!(
            outbox.get("stop".into()).await.unwrap().unwrap().status,
            EffectStatus::Succeeded
        );
    }

    #[tokio::test]
    async fn a_panicking_executor_is_a_retryable_failure() {
        let (outbox, worker, _) = harness(Box::new(|_, _| panic!("kaboom")), 5).await;
        let command = CommandId::new("cmd").unwrap();
        let thread = ThreadId::new("thread").unwrap();
        outbox
            .enqueue(vec![start("panics", &command, &thread)])
            .await
            .unwrap();
        assert!(worker.run_once().await.unwrap());
        let row = outbox.get("panics".into()).await.unwrap().unwrap();
        assert_eq!(row.status, EffectStatus::Pending);
        assert!(row.last_error.unwrap().contains("kaboom"));
    }

    struct BlockingExecutor {
        started: tokio::sync::Notify,
        dropped: Arc<std::sync::atomic::AtomicBool>,
    }

    struct DropFlag(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl EffectExecutor for BlockingExecutor {
        async fn execute(&self, _: &Effect, _: bool) -> Result<(), EffectExecutionError> {
            let _flag = DropFlag(self.dropped.clone());
            self.started.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn cancelling_a_running_effect_drops_its_execution_without_settling_it() {
        let outbox = EffectOutbox::new(Db::open_in_memory().await.unwrap());
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let executor = Arc::new(BlockingExecutor {
            started: tokio::sync::Notify::new(),
            dropped: dropped.clone(),
        });
        let worker = EffectWorker::new(
            outbox.clone(),
            executor.clone(),
            EffectWorkerOptions::default(),
        );
        let command = CommandId::new("cmd").unwrap();
        let thread = ThreadId::new("thread").unwrap();
        outbox
            .enqueue(vec![start("blocked", &command, &thread)])
            .await
            .unwrap();
        let run = {
            let worker = worker.clone();
            tokio::spawn(async move { worker.run_once().await })
        };
        executor.started.notified().await;
        let tid = thread.clone();
        let ids = outbox
            .db()
            .write(move |tx| {
                EffectOutbox::cancel_unsettled_in(
                    tx,
                    &tid,
                    &[EffectType::ProviderTurnStart],
                    "Run stopped.",
                )
            })
            .await
            .unwrap();
        outbox.signal_cancellations(&ids);
        assert!(run.await.unwrap().unwrap());
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            outbox.get("blocked".into()).await.unwrap().unwrap().status,
            EffectStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn the_daemon_runs_work_that_arrives_after_it_starts() {
        let (outbox, worker, executor) = harness(Box::new(|_, _| Ok(())), 5).await;
        let daemon = worker.spawn_daemon(EffectDaemonOptions {
            concurrency: 2,
            liveness_poll_interval: StdDuration::from_secs(60),
        });
        let command = CommandId::new("cmd").unwrap();
        let thread = ThreadId::new("thread").unwrap();
        // The enqueue's post-commit notification wakes an idle worker.
        outbox
            .enqueue(vec![start("late", &command, &thread)])
            .await
            .unwrap();
        outbox.await_settled("late".into()).await.unwrap();
        assert_eq!(executor.calls.lock().len(), 1);
        assert_eq!(
            outbox.get("late".into()).await.unwrap().unwrap().status,
            EffectStatus::Succeeded
        );
        daemon.shutdown();
    }
}
