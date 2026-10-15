//! Port of upstream/apps/server/src/orchestration-v2/EffectWorker.ts
//!
//! T13c supplies the executor for provider/session/checkpoint effects. Its
//! future must stop the underlying process or operation when dropped by the
//! cancellation race. The daemon owns every worker in a JoinSet.

use super::effect_outbox::{
    Effect, EffectOutbox, EffectStatus, REPLAY_SAFE_EFFECT_TYPES_AFTER_PROCESS_LOSS,
};
use async_trait::async_trait;
use chrono::Utc;
use std::sync::Arc;
use std::time::Duration as StdDuration;
use tokio::task::JoinSet;
use turin_persistence::PersistenceError;

#[async_trait]
pub trait EffectExecutor: Send + Sync {
    async fn execute(&self, effect: &Effect, will_retry: bool) -> Result<(), String>;
}

#[derive(Debug, thiserror::Error)]
pub enum EffectWorkerError {
    #[error("Orchestration effect worker {operation} failed")]
    Operation {
        operation: &'static str,
        effect_id: Option<String>,
        #[source]
        cause: PersistenceError,
    },
    #[error("Worker no longer owns effect lease {0}")]
    LostLease(String),
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
    pub async fn run_once(&self) -> Result<bool, EffectWorkerError> {
        self.run_once_inner(false).await
    }
    pub async fn run_recovery_once(&self) -> Result<bool, EffectWorkerError> {
        self.run_once_inner(true).await
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
                operation: "claim",
                effect_id: None,
                cause,
            })?;
        let Some(effect) = claimed else {
            return Ok(false);
        };
        // Arm the local cancellation before reading the durable state. This
        // closes the gap between a claimed row and external execution.
        let cancellation = self.outbox.cancellation_signal(&effect.id);
        let notified = cancellation.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let current = self.outbox.get(effect.id.clone()).await.map_err(|cause| {
            EffectWorkerError::Operation {
                operation: "check-cancellation",
                effect_id: Some(effect.id.clone()),
                cause,
            }
        })?;
        if current.is_some_and(|row| row.status == EffectStatus::Cancelled) {
            self.outbox.clear_cancellation(&effect.id);
            return Ok(true);
        }
        let will_retry = effect.attempt_count < self.options.max_attempts;
        let execution = self.executor.execute(&effect, will_retry);
        tokio::pin!(execution);
        let outcome = tokio::select! {
            result = &mut execution => Some(result),
            () = &mut notified => None,
        };
        self.outbox.clear_cancellation(&effect.id);
        let Some(outcome) = outcome else {
            return Ok(true);
        };
        match outcome {
            Ok(()) => {
                let settled = self
                    .outbox
                    .succeed(effect.id.clone(), self.options.worker_id.clone())
                    .await
                    .map_err(|cause| EffectWorkerError::Operation {
                        operation: "complete",
                        effect_id: Some(effect.id.clone()),
                        cause,
                    })?;
                if !settled && !self.was_cancelled(&effect.id).await? {
                    return Err(EffectWorkerError::LostLease(effect.id.clone()));
                }
            }
            Err(error) => {
                let kind = effect
                    .request
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let non_retryable = is_non_retryable_provider_turn_control_failure(kind, &error);
                tracing::warn!(effect_id = %effect.id, effect_type = kind, attempt_count = effect.attempt_count, non_retryable, %error, "Orchestration effect execution failed");
                let settled = if non_retryable {
                    self.outbox
                        .succeed(effect.id.clone(), self.options.worker_id.clone())
                        .await
                } else if !will_retry {
                    self.outbox
                        .fail(effect.id.clone(), self.options.worker_id.clone(), error)
                        .await
                } else {
                    let shift = u32::try_from((effect.attempt_count - 1).max(0))
                        .unwrap_or(31)
                        .min(31);
                    let delay = (100_i64.saturating_mul(1_i64 << shift)).min(30_000);
                    self.outbox
                        .retry(
                            effect.id.clone(),
                            self.options.worker_id.clone(),
                            error,
                            delay,
                        )
                        .await
                }
                .map_err(|cause| EffectWorkerError::Operation {
                    operation: "reschedule",
                    effect_id: Some(effect.id.clone()),
                    cause,
                })?;
                if !settled && !self.was_cancelled(&effect.id).await? {
                    return Err(EffectWorkerError::LostLease(effect.id.clone()));
                }
            }
        }
        Ok(true)
    }

    async fn was_cancelled(&self, id: &str) -> Result<bool, EffectWorkerError> {
        self.outbox
            .get(id.to_owned())
            .await
            .map(|row| row.is_some_and(|effect| effect.status == EffectStatus::Cancelled))
            .map_err(|cause| EffectWorkerError::Operation {
                operation: "check-cancellation",
                effect_id: Some(id.to_owned()),
                cause,
            })
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
    pub async fn drain(&self, max_effects: usize) -> Result<usize, EffectWorkerError> {
        let mut completed = 0;
        while completed < max_effects && self.run_once().await? {
            completed += 1;
        }
        Ok(completed)
    }

    /// Runs until its owner drops the future or its cancellation token fires.
    /// Dropping the JoinSet aborts all worker futures; executor drop must stop
    /// provider processes according to the provider-core contract.
    pub async fn run_daemon(
        &self,
        concurrency: usize,
        liveness_poll_interval: StdDuration,
        shutdown: tokio_util::sync::CancellationToken,
    ) {
        let mut workers = JoinSet::new();
        for _ in 0..concurrency.max(1) {
            let worker = self.clone();
            let shutdown = shutdown.clone();
            workers.spawn(async move {
                loop {
                    if shutdown.is_cancelled() { break; }
                    match worker.run_once().await {
                        Ok(true) => { tokio::task::yield_now().await; continue; }
                        Err(err) => {
                            tracing::warn!(%err, "Orchestration effect worker failed");
                            tokio::select! { () = shutdown.cancelled() => break, () = tokio::time::sleep(liveness_poll_interval.min(StdDuration::from_secs(1))) => {} }
                            continue;
                        }
                        Ok(false) => {}
                    }
                    let until = worker.next_claimable_at().await.ok().flatten()
                        .map(|at| (at - Utc::now()).to_std().unwrap_or(StdDuration::from_millis(25)))
                        .unwrap_or(liveness_poll_interval);
                    let sleep = until.min(liveness_poll_interval).max(StdDuration::from_millis(1));
                    tokio::select! {
                        () = shutdown.cancelled() => break,
                        () = worker.outbox.await_available() => {},
                        () = tokio::time::sleep(sleep) => {},
                    }
                }
            });
        }
        tokio::select! { () = shutdown.cancelled() => {}, _ = workers.join_next() => {} }
        workers.abort_all();
        while workers.join_next().await.is_some() {}
    }
}

pub fn is_non_retryable_provider_turn_control_failure(effect_type: &str, error_text: &str) -> bool {
    if effect_type != "provider-turn.interrupt" {
        return false;
    }
    let lower = error_text.to_ascii_lowercase();
    [
        "is not active",
        "hard teardown is already in progress",
        "treating as already interrupted",
        "treating as already stopped",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase))
}

pub fn replay_safe_after_process_loss(effect: &Effect) -> bool {
    effect
        .request
        .get("type")
        .and_then(|v| v.as_str())
        .is_some_and(|kind| REPLAY_SAFE_EFFECT_TYPES_AFTER_PROCESS_LOSS.contains(&kind))
}

#[cfg(test)]
mod tests {
    use super::*;
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
    }
}
