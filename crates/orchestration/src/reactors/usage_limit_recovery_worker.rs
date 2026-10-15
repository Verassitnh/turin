//! Port of upstream/apps/server/src/orchestration-v2/UsageLimitRecoveryWorker.ts
//!
//! The upstream worker registers one sweep on the shared scheduler. Here the
//! sweep is an async method and [`UsageLimitRecoveryWorker::spawn`] owns the
//! 5 second tick (`scheduling/Scheduler.ts`): a tick sleeps, then runs the
//! sweep inline, so a slow sweep can never overlap itself. The caller spawns
//! the loop after startup recovery, the same point the upstream scheduler
//! releases parked sources.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio_util::task::AbortOnDropHandle;
use turin_contracts::orchestration_v2::{
    OrchestrationV2Actor, OrchestrationV2Command, OrchestrationV2CreationSource,
    OrchestrationV2DispatchMode, OrchestrationV2LimitRecoveryUpdate,
    OrchestrationV2ProviderFailureClass, OrchestrationV2ServerCommand,
    OrchestrationV2SettleOverride, OrchestrationV2ShellThreadStatus,
};
use turin_contracts::{CommandId, MessageId, UtcDateTime};

use crate::pipeline::orchestrator_port::BoxError;
use crate::pipeline::seams::ServerSettingsSource;
use crate::projection::store::ProjectionStore;
use crate::projection::store::shell::ProjectionLimitRecoveryCandidate;

/// Scheduler tick shared by every due-work source (`Scheduler.ts`).
pub const SCHEDULER_TICK: Duration = Duration::from_secs(5);

/// `ProjectionStoreV2.getLimitRecoveryCandidates`.
#[async_trait]
pub trait LimitRecoveryCandidateSource: Send + Sync {
    async fn get_limit_recovery_candidates(
        &self,
        now: UtcDateTime,
        auto_resume: bool,
        snooze: bool,
    ) -> Result<Vec<ProjectionLimitRecoveryCandidate>, BoxError>;
}

#[async_trait]
impl LimitRecoveryCandidateSource for ProjectionStore {
    async fn get_limit_recovery_candidates(
        &self,
        now: UtcDateTime,
        auto_resume: bool,
        snooze: bool,
    ) -> Result<Vec<ProjectionLimitRecoveryCandidate>, BoxError> {
        ProjectionStore::get_limit_recovery_candidates(self, now, auto_resume, snooze)
            .await
            .map_err(|error| Box::new(error) as BoxError)
    }
}

/// `ThreadManagementService.dispatch`, reduced to what the sweep needs.
#[async_trait]
pub trait LimitRecoveryDispatch: Send + Sync {
    async fn dispatch(&self, command: OrchestrationV2ServerCommand) -> Result<(), BoxError>;
}

#[async_trait]
impl LimitRecoveryDispatch for crate::pipeline::thread_management_service::ThreadManagementService {
    async fn dispatch(&self, command: OrchestrationV2ServerCommand) -> Result<(), BoxError> {
        crate::pipeline::thread_management_service::ThreadManagementService::dispatch(self, command)
            .await
            .map(|_| ())
            .map_err(|error| Box::new(error) as BoxError)
    }
}

/// The persisted run and reset form the identity of one recovery opportunity.
///
/// Returns `None` when there is nothing to do: the thread is not a usage-limit
/// failure of its latest run, the reset is missing or already expired, or the
/// user's choice does not allow the next step.
pub fn limit_recovery_command(
    thread: &ProjectionLimitRecoveryCandidate,
    auto_resume: bool,
    now_ms: i64,
    snooze: bool,
) -> Option<OrchestrationV2Command> {
    let latest_run_id = thread.latest_run_id.as_ref()?;
    let reset_at = thread.usage_limit_reset_at.as_ref()?;
    if thread.status != OrchestrationV2ShellThreadStatus::Failed
        || thread.last_error_class != Some(OrchestrationV2ProviderFailureClass::UsageLimit)
        || thread.archived_at.is_some()
        || thread.settled_override == Some(OrchestrationV2SettleOverride::Settled)
        || thread.pending_runtime_request.is_some()
    {
        return None;
    }
    let reset_ms = parse_iso_millis(reset_at)?;
    // An already-expired window reported with a fresh failure cannot start a retry loop.
    let completed_ms = thread
        .latest_run_completed_at
        .as_ref()
        .unwrap_or(&thread.updated_at)
        .0
        .timestamp_millis();
    if reset_ms <= completed_ms {
        return None;
    }
    let identity = format!("{}:{}:{}", thread.id, latest_run_id, reset_ms);
    let recovery = thread.limit_recovery.as_ref();
    let recovery_matches = recovery.is_some_and(|recovery| {
        recovery.run_id == *latest_run_id && recovery.reset_at == *reset_at
    });
    if !recovery_matches {
        if !auto_resume && (!snooze || reset_ms <= now_ms) {
            return None;
        }
        return Some(OrchestrationV2Command::ThreadMetadataUpdate {
            command_id: command_id(format!("limit-arm:{identity}")),
            thread_id: thread.id.clone(),
            title: None,
            regenerate_title: None,
            branch: None,
            worktree_path: None,
            expected_worktree_path: None,
            expected_empty: None,
            limit_recovery: Some(Some(OrchestrationV2LimitRecoveryUpdate {
                run_id: latest_run_id.clone(),
                reset_at: reset_at.clone(),
                auto_resume: Some(auto_resume),
                snooze: Some(snooze && reset_ms > now_ms),
            })),
            linked_pull_request: None,
        });
    }
    let recovery = recovery?;
    let snoozed_past_now = thread
        .snoozed_until
        .as_ref()
        .is_some_and(|until| until.0.timestamp_millis() > now_ms);
    if !recovery.auto_resume || reset_ms > now_ms || snoozed_past_now {
        return None;
    }
    let delivery_identity = format!(
        "{identity}:{}",
        recovery
            .request_id
            .as_ref()
            .map_or("legacy", |request_id| request_id.as_str())
    );
    Some(OrchestrationV2Command::MessageDispatch {
        notification: None,
        created_by: OrchestrationV2Actor::User,
        creation_source: OrchestrationV2CreationSource::Server,
        scheduled_task_id: None,
        sender_thread_id: None,
        command_id: command_id(format!("limit-resume:{delivery_identity}")),
        thread_id: thread.id.clone(),
        message_id: MessageId::new(format!("limit-resume:{delivery_identity}"))
            .expect("limit resume message id is non-empty"),
        text: "Continue where you left off.".to_owned(),
        context: None,
        attachments: Vec::new(),
        title_seed: None,
        model_selection: None,
        source_plan_ref: None,
        restart_continuation_of_run_id: None,
        usage_limit_continuation_of_run_id: Some(latest_run_id.clone()),
        manual_continuation_of_run_id: None,
        usage_limit_recovery_request_id: recovery.request_id.clone(),
        delivery_intent: None,
        delegated_completion: None,
        dispatch_mode: OrchestrationV2DispatchMode::StartImmediately,
    })
}

fn command_id(value: String) -> CommandId {
    CommandId::new(value).expect("limit recovery command id is non-empty")
}

/// `Date.parse` for the ISO timestamps this crate writes. Anything that does
/// not parse is `NaN` upstream, and `NaN` fails every comparison, so `None`
/// here means "no command".
fn parse_iso_millis(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc).timestamp_millis())
}

pub struct UsageLimitRecoveryWorker {
    candidates: Arc<dyn LimitRecoveryCandidateSource>,
    threads: Arc<dyn LimitRecoveryDispatch>,
    settings: Arc<dyn ServerSettingsSource>,
}

impl UsageLimitRecoveryWorker {
    pub fn new(
        candidates: Arc<dyn LimitRecoveryCandidateSource>,
        threads: Arc<dyn LimitRecoveryDispatch>,
        settings: Arc<dyn ServerSettingsSource>,
    ) -> Self {
        Self {
            candidates,
            threads,
            settings,
        }
    }

    /// One sweep. A failed settings or candidate read fails the sweep; a failed
    /// dispatch is logged and the remaining threads still run.
    pub async fn sweep(&self) -> Result<(), BoxError> {
        self.sweep_at(UtcDateTime::now()).await
    }

    async fn sweep_at(&self, now: UtcDateTime) -> Result<(), BoxError> {
        let preferences = self.settings.get_settings().await?;
        let auto_resume = preferences.auto_resume_limited_threads;
        let snooze = preferences.snooze_limited_threads;
        let candidates = self
            .candidates
            .get_limit_recovery_candidates(now, auto_resume, snooze)
            .await?;
        let now_ms = now.0.timestamp_millis();
        for thread in &candidates {
            let Some(command) = limit_recovery_command(thread, auto_resume, now_ms, snooze) else {
                continue;
            };
            let thread_id = thread.id.clone();
            if let Err(error) = self
                .threads
                .dispatch(OrchestrationV2ServerCommand::Client(command))
                .await
            {
                tracing::warn!(
                    thread_id = %thread_id,
                    error = %error,
                    "orchestration-v2.limit-recovery.dispatch-failed"
                );
            }
        }
        Ok(())
    }

    /// Runs [`Self::sweep`] every [`SCHEDULER_TICK`]. The sleep comes first and
    /// the sweep is awaited inline, so sweeps never overlap and missed ticks
    /// are not queued. Dropping the handle stops the loop.
    pub fn spawn(self: Arc<Self>) -> AbortOnDropHandle<()> {
        AbortOnDropHandle::new(tokio::spawn(async move {
            loop {
                tokio::time::sleep(SCHEDULER_TICK).await;
                if let Err(error) = self.sweep().await {
                    tracing::warn!(
                        source = "usage-limit-recovery",
                        error = %error,
                        "Scheduler source failed"
                    );
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests;
