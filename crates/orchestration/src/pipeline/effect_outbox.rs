//! Port of upstream/apps/server/src/orchestration-v2/EffectOutbox.ts
//!
//! The outbox is durable. Notifications are only latency hints and are sent
//! after the enclosing writer transaction commits. Each thread has one
//! lifecycle lane and an independent title-generation lane.

use super::task::OwnedTask;
use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use turin_contracts::{
    CheckpointId, CheckpointScopeId, CommandId, MessageId, NodeId, ProviderSessionId,
    ProviderThreadId, ProviderTurnId, RunAttemptId, RunId, RuntimeRequestId, ThreadId,
};
use turin_persistence::{
    Db, PersistenceDecodeError, PersistenceError, format_utc_millis, utc_millis_now,
};

/// Upstream: `Queue.dropping<void>(64)`. A small burst lets several worker
/// slots wake for distinct threads without letting notifications pile up.
const AVAILABLE_BURST: usize = 64;

/// Subagent a `provider-turn.interrupt` targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterruptSubagent {
    pub id: NodeId,
    pub native_task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SessionTransition {
    #[serde(rename = "replace", rename_all = "camelCase")]
    Replace {
        replacement_provider_session_id: ProviderSessionId,
    },
    #[serde(rename = "detach")]
    Detach,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ThreadTitleKind {
    #[serde(rename = "initial", rename_all = "camelCase")]
    Initial { message_id: MessageId },
    #[serde(rename = "regenerate")]
    Regenerate,
}

/// `OrchestrationEffectRequestV2`: exact upstream JSON, including the `type`
/// discriminator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum EffectRequest {
    #[serde(rename = "provider-runtime.continue")]
    ProviderRuntimeContinue { source_run_id: RunId },
    #[serde(rename = "provider-session.detach")]
    ProviderSessionDetach {
        provider_session_id: ProviderSessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// Set on terminal detaches (thread archive/delete): revoke the thread's MCP credentials.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revoke_mcp_credential: Option<bool>,
    },
    #[serde(rename = "provider-turn.start")]
    ProviderTurnStart { run_id: RunId },
    #[serde(rename = "provider-turn.interrupt")]
    ProviderTurnInterrupt {
        provider_session_id: ProviderSessionId,
        provider_thread_id: ProviderThreadId,
        provider_turn_id: ProviderTurnId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent: Option<InterruptSubagent>,
    },
    #[serde(rename = "provider-turn.steer")]
    ProviderTurnSteer {
        provider_session_id: ProviderSessionId,
        provider_thread_id: ProviderThreadId,
        provider_turn_id: ProviderTurnId,
        message_id: MessageId,
    },
    #[serde(rename = "provider-turn.restart")]
    ProviderTurnRestart {
        provider_session_id: ProviderSessionId,
        provider_thread_id: ProviderThreadId,
        provider_turn_id: ProviderTurnId,
        interrupted_attempt_id: RunAttemptId,
        run_id: RunId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_transition: Option<SessionTransition>,
    },
    #[serde(rename = "runtime-request.respond")]
    RuntimeRequestRespond {
        provider_session_id: ProviderSessionId,
        request_id: RuntimeRequestId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision: Option<turin_contracts::provider_policy::ProviderApprovalDecision>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answers: Option<turin_contracts::provider_policy::ProviderUserInputAnswers>,
    },
    #[serde(rename = "provider-thread.rollback")]
    ProviderThreadRollback {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restore_files: Option<bool>,
        provider_thread_id: ProviderThreadId,
        checkpoint_id: CheckpointId,
        scope_id: CheckpointScopeId,
    },
    #[serde(rename = "checkpoint.capture")]
    CheckpointCapture {
        run_id: RunId,
        scope_id: CheckpointScopeId,
    },
    #[serde(rename = "terminal.cleanup")]
    TerminalCleanup {},
    /// Closes a deleted thread's preview sessions. Turin has no preview
    /// feature, so nothing emits this; the variant exists so a row imported
    /// from a T3 database still decodes (the executor treats it as done).
    #[serde(rename = "preview.cleanup")]
    PreviewCleanup {},
    #[serde(rename = "attachment.cleanup")]
    AttachmentCleanup { attachment_ids: Vec<String> },
    #[serde(rename = "thread-title.generate")]
    ThreadTitleGenerate { kind: ThreadTitleKind },
    /// Follows a Stop: sends `thread.stop` to every delegated task under the stopped thread.
    #[serde(rename = "delegated-tasks.stop")]
    DelegatedTasksStop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// `OrchestrationEffectRequestV2["type"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectType {
    ProviderRuntimeContinue,
    ProviderSessionDetach,
    ProviderTurnStart,
    ProviderTurnInterrupt,
    ProviderTurnSteer,
    ProviderTurnRestart,
    RuntimeRequestRespond,
    ProviderThreadRollback,
    CheckpointCapture,
    TerminalCleanup,
    PreviewCleanup,
    AttachmentCleanup,
    ThreadTitleGenerate,
    DelegatedTasksStop,
}

impl EffectType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderRuntimeContinue => "provider-runtime.continue",
            Self::ProviderSessionDetach => "provider-session.detach",
            Self::ProviderTurnStart => "provider-turn.start",
            Self::ProviderTurnInterrupt => "provider-turn.interrupt",
            Self::ProviderTurnSteer => "provider-turn.steer",
            Self::ProviderTurnRestart => "provider-turn.restart",
            Self::RuntimeRequestRespond => "runtime-request.respond",
            Self::ProviderThreadRollback => "provider-thread.rollback",
            Self::CheckpointCapture => "checkpoint.capture",
            Self::TerminalCleanup => "terminal.cleanup",
            Self::PreviewCleanup => "preview.cleanup",
            Self::AttachmentCleanup => "attachment.cleanup",
            Self::ThreadTitleGenerate => "thread-title.generate",
            Self::DelegatedTasksStop => "delegated-tasks.stop",
        }
    }
}

impl EffectRequest {
    pub fn effect_type(&self) -> EffectType {
        match self {
            Self::ProviderRuntimeContinue { .. } => EffectType::ProviderRuntimeContinue,
            Self::ProviderSessionDetach { .. } => EffectType::ProviderSessionDetach,
            Self::ProviderTurnStart { .. } => EffectType::ProviderTurnStart,
            Self::ProviderTurnInterrupt { .. } => EffectType::ProviderTurnInterrupt,
            Self::ProviderTurnSteer { .. } => EffectType::ProviderTurnSteer,
            Self::ProviderTurnRestart { .. } => EffectType::ProviderTurnRestart,
            Self::RuntimeRequestRespond { .. } => EffectType::RuntimeRequestRespond,
            Self::ProviderThreadRollback { .. } => EffectType::ProviderThreadRollback,
            Self::CheckpointCapture { .. } => EffectType::CheckpointCapture,
            Self::TerminalCleanup {} => EffectType::TerminalCleanup,
            Self::PreviewCleanup {} => EffectType::PreviewCleanup,
            Self::AttachmentCleanup { .. } => EffectType::AttachmentCleanup,
            Self::ThreadTitleGenerate { .. } => EffectType::ThreadTitleGenerate,
            Self::DelegatedTasksStop { .. } => EffectType::DelegatedTasksStop,
        }
    }
}

pub const REPLAY_SAFE_EFFECT_TYPES_AFTER_PROCESS_LOSS: &[EffectType] = &[
    EffectType::ProviderRuntimeContinue,
    EffectType::ProviderSessionDetach,
    EffectType::ProviderThreadRollback,
    EffectType::CheckpointCapture,
    EffectType::TerminalCleanup,
    EffectType::PreviewCleanup,
    EffectType::AttachmentCleanup,
    EffectType::ThreadTitleGenerate,
    EffectType::DelegatedTasksStop,
];

pub const PROCESS_BOUND_EFFECT_TYPES: &[EffectType] = &[
    EffectType::ProviderTurnStart,
    EffectType::ProviderTurnInterrupt,
    EffectType::ProviderTurnSteer,
    EffectType::ProviderTurnRestart,
    EffectType::RuntimeRequestRespond,
];

/// How long a succeeded or cancelled effect row is kept after it completes.
/// Claiming, recovery, the orchestrator and storage cleanup only ask whether a
/// row is pending, running or failed, so a missing succeeded or cancelled row
/// reads the same as a present one. Its one other use is deduplication:
/// `enqueue` ignores an id that already exists, and ids are deterministic per
/// command or run. Failed rows are kept: storage cleanup keeps a deleted
/// thread's worktree while any of its effects failed.
pub const SETTLED_EFFECT_RETENTION_DAYS: i64 = 7;
pub const PRUNE_BATCH_SIZE: i64 = 500;
const PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl EffectStatus {
    fn parse(s: &str) -> Result<Self, PersistenceError> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(decode_err(format!("unknown status {s}"))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PendingEffect {
    pub id: String,
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub request: EffectRequest,
    pub available_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct Effect {
    pub id: String,
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub request: EffectRequest,
    pub status: EffectStatus,
    pub attempt_count: i64,
    pub available_at: String,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileResult {
    pub requeued: usize,
    pub cancelled: usize,
}

fn decode_err(issue: impl Into<String>) -> PersistenceError {
    PersistenceError::Decode(PersistenceDecodeError::new("EffectOutbox.decode", issue))
}

fn in_list(count: usize, first: usize) -> String {
    (0..count)
        .map(|i| format!("?{}", first + i))
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Clone)]
pub struct EffectOutbox {
    db: Db,
    /// Counting semaphore capped at [`AVAILABLE_BURST`]: `notify_available`
    /// adds a permit per effect, `await_available` consumes one.
    available: Arc<Semaphore>,
    /// Process-local, sticky per-effect signals (upstream `Deferred`s).
    cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
    settled: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl EffectOutbox {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            available: Arc::new(Semaphore::new(0)),
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            settled: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub fn notify_available(&self, count: usize) {
        for _ in 0..count.min(AVAILABLE_BURST) {
            if self.available.available_permits() >= AVAILABLE_BURST {
                break;
            }
            self.available.add_permits(1);
        }
    }

    pub async fn await_available(&self) {
        if let Ok(permit) = self.available.acquire().await {
            permit.forget();
        }
    }

    fn signal_settled(&self, ids: &[String]) {
        let mut settled = self.settled.lock();
        for id in ids {
            if let Some(signal) = settled.remove(id) {
                signal.cancel();
            }
        }
    }

    /// Post-commit companion of `cancel_unsettled_in`: interrupt running
    /// executions, release `await_settled` waiters, and wake claimers because
    /// a cancellation can unblock other pending work on the same thread.
    pub fn signal_cancellations(&self, ids: &[String]) {
        {
            let cancellations = self.cancellations.lock();
            for id in ids {
                if let Some(signal) = cancellations.get(id) {
                    signal.cancel();
                }
            }
        }
        self.signal_settled(ids);
        if !ids.is_empty() {
            self.notify_available(ids.len());
        }
    }

    /// The cancellation signal for `id`, created on first use. It stays
    /// cancelled once signalled (a signal that arrives before the worker
    /// starts waiting is not lost).
    pub fn cancellation_signal(&self, id: &str) -> CancellationToken {
        self.cancellations
            .lock()
            .entry(id.to_owned())
            .or_default()
            .clone()
    }

    pub fn clear_cancellation(&self, id: &str) {
        self.cancellations.lock().remove(id);
    }

    /// Insert rows in the caller's transaction; callers notify after commit.
    pub fn enqueue_in(
        conn: &Connection,
        effects: &[PendingEffect],
    ) -> Result<(), PersistenceError> {
        let now = utc_millis_now();
        for effect in effects {
            let payload = serde_json::to_string(&effect.request)
                .map_err(|err| decode_err(err.to_string()))?;
            let available_at = effect
                .available_at
                .map_or_else(|| now.clone(), format_utc_millis);
            conn.execute(
                "INSERT INTO orchestration_v2_effect_outbox (effect_id, command_id, thread_id, effect_type, payload_json, status, attempt_count, available_at, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', 0, ?6, ?7, ?7) ON CONFLICT(effect_id) DO NOTHING",
                params![
                    effect.id,
                    effect.command_id.as_str(),
                    effect.thread_id.as_str(),
                    effect.request.effect_type().as_str(),
                    payload,
                    available_at,
                    now
                ],
            )
            .map_err(|err| PersistenceError::sql("EffectOutbox.enqueue", &err))?;
        }
        Ok(())
    }

    /// Standalone enqueue (its own transaction, then notify). The sink's
    /// `write_with_effects` is the path that composes enqueue with events.
    pub async fn enqueue(&self, effects: Vec<PendingEffect>) -> Result<(), PersistenceError> {
        let count = effects.len();
        self.db
            .write(move |tx| Self::enqueue_in(tx, &effects))
            .await?;
        self.notify_available(count);
        Ok(())
    }

    pub async fn get(&self, id: String) -> Result<Option<Effect>, PersistenceError> {
        self.db.read(move |conn| get_in(conn, &id)).await
    }

    pub async fn list_by_command_id(
        &self,
        command_id: CommandId,
    ) -> Result<Vec<Effect>, PersistenceError> {
        self.db.read(move |conn| {
            let mut stmt = conn.prepare("SELECT * FROM orchestration_v2_effect_outbox WHERE command_id = ?1 ORDER BY created_at ASC, effect_id ASC").map_err(|err| PersistenceError::sql("EffectOutbox.list", &err))?;
            let rows = stmt.query_map([command_id.as_str()], decode_row).map_err(|err| PersistenceError::sql("EffectOutbox.list", &err))?;
            rows.map(|row| decode_effect(row.map_err(|err| PersistenceError::sql("EffectOutbox.list", &err))?)).collect()
        }).await
    }

    /// Cancels the thread's pending/running effects of `effect_types` and
    /// returns their ids. Post-commit, pass them to `signal_cancellations`.
    pub fn cancel_unsettled_in(
        conn: &Connection,
        thread_id: &ThreadId,
        effect_types: &[EffectType],
        reason: &str,
    ) -> Result<Vec<String>, PersistenceError> {
        if effect_types.is_empty() {
            return Ok(vec![]);
        }
        let now = utc_millis_now();
        let sql = format!(
            "UPDATE orchestration_v2_effect_outbox SET status = 'cancelled', lease_owner = NULL, lease_expires_at = NULL, completed_at = ?1, updated_at = ?1, last_error = ?2 WHERE thread_id = ?3 AND status IN ('pending', 'running') AND effect_type IN ({}) RETURNING effect_id",
            in_list(effect_types.len(), 4)
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|err| PersistenceError::sql("EffectOutbox.cancel-unsettled", &err))?;
        let mut values: Vec<rusqlite::types::Value> = vec![
            now.into(),
            reason.to_owned().into(),
            thread_id.as_str().to_owned().into(),
        ];
        values.extend(effect_types.iter().map(|t| t.as_str().to_owned().into()));
        let rows = stmt
            .query_map(params_from_iter(values), |row| row.get::<_, String>(0))
            .map_err(|err| PersistenceError::sql("EffectOutbox.cancel-unsettled", &err))?;
        rows.map(|row| {
            row.map_err(|err| PersistenceError::sql("EffectOutbox.cancel-unsettled", &err))
        })
        .collect()
    }

    /// Per-thread lane rules (upstream comment): each thread runs its effects
    /// one at a time, in enqueue (rowid) order. An earlier effect waiting out
    /// a retry backoff still blocks later ones, so a turn cannot start while a
    /// failed rollback is about to restore files. A claim that skips restart
    /// continuations is not blocked by them either. Title generation is
    /// correlated metadata work with its own per-thread lane, so it cannot
    /// delay provider lifecycle effects.
    pub async fn claim_next(
        &self,
        worker_id: String,
        lease_duration_ms: i64,
        exclude_restart_continuations: bool,
    ) -> Result<Option<Effect>, PersistenceError> {
        let claimed = self.db.write(move |conn| {
            let now = Utc::now();
            let now_iso = format_utc_millis(now);
            let expires = format_utc_millis(now + Duration::milliseconds(lease_duration_ms.max(1)));
            let mut stmt = conn.prepare(
                "UPDATE orchestration_v2_effect_outbox SET status = 'running', attempt_count = attempt_count + 1, lease_owner = ?1, lease_expires_at = ?2, updated_at = ?3, last_error = NULL WHERE effect_id = (SELECT candidate.effect_id FROM orchestration_v2_effect_outbox AS candidate WHERE candidate.available_at <= ?3 AND candidate.status = 'pending' AND NOT EXISTS (SELECT 1 FROM orchestration_v2_effect_outbox AS active WHERE active.thread_id = candidate.thread_id AND (active.status = 'running' OR (active.status = 'pending' AND active.rowid < candidate.rowid AND (?4 = 0 OR active.effect_type != 'provider-runtime.continue'))) AND ((candidate.effect_type = 'thread-title.generate' AND active.effect_type = 'thread-title.generate') OR (candidate.effect_type != 'thread-title.generate' AND active.effect_type != 'thread-title.generate'))) AND (?4 = 0 OR candidate.effect_type != 'provider-runtime.continue') ORDER BY candidate.available_at ASC, candidate.created_at ASC, candidate.effect_id ASC LIMIT 1) RETURNING *"
            ).map_err(|err| PersistenceError::sql("EffectOutbox.claim", &err))?;
            let raw = stmt.query_row(params![worker_id, expires, now_iso, exclude_restart_continuations as i64], decode_row).optional().map_err(|err| PersistenceError::sql("EffectOutbox.claim", &err))?;
            raw.map(decode_effect).transpose()
        }).await?;
        if let Some(effect) = &claimed {
            // A fresh signal per claim: a stale cancellation from a previous
            // attempt of the same effect must not cancel this one.
            self.cancellations
                .lock()
                .insert(effect.id.clone(), CancellationToken::new());
        }
        Ok(claimed)
    }

    pub async fn next_claimable_at(&self) -> Result<Option<DateTime<Utc>>, PersistenceError> {
        let value: Option<String> = self.db.read(|conn| conn.query_row(
            "SELECT MIN(candidate.available_at) FROM orchestration_v2_effect_outbox AS candidate WHERE candidate.status = 'pending' AND NOT EXISTS (SELECT 1 FROM orchestration_v2_effect_outbox AS active WHERE active.thread_id = candidate.thread_id AND (active.status = 'running' OR (active.status = 'pending' AND active.rowid < candidate.rowid)) AND ((candidate.effect_type = 'thread-title.generate' AND active.effect_type = 'thread-title.generate') OR (candidate.effect_type != 'thread-title.generate' AND active.effect_type != 'thread-title.generate')))", [], |row| row.get(0)
        ).map_err(|err| PersistenceError::sql("EffectOutbox.next-claimable", &err))).await?;
        value
            .map(|text| {
                DateTime::parse_from_rfc3339(&text)
                    .map(|dt| dt.with_timezone(&Utc))
                    .map_err(|_| {
                        PersistenceError::Decode(PersistenceDecodeError::new(
                            "EffectOutbox.next-claimable",
                            format!("Invalid available_at timestamp: {text}"),
                        ))
                    })
            })
            .transpose()
    }

    /// Settles a claimed row. `succeeded`/`failed` set `completed_at`;
    /// `retry` (back to pending) leaves it null and moves `available_at`.
    async fn settle(
        &self,
        id: String,
        worker_id: String,
        outcome: Settle,
    ) -> Result<bool, PersistenceError> {
        let id_for_sql = id.clone();
        let changed = self.db.write(move |conn| {
            let now = Utc::now();
            let now_iso = format_utc_millis(now);
            let changed = match outcome {
                Settle::Succeed => conn.execute(
                    "UPDATE orchestration_v2_effect_outbox SET status = 'succeeded', lease_owner = NULL, lease_expires_at = NULL, completed_at = ?1, updated_at = ?1, last_error = NULL WHERE effect_id = ?2 AND status = 'running' AND lease_owner = ?3",
                    params![now_iso, id_for_sql, worker_id],
                ),
                Settle::Fail(ref error) => conn.execute(
                    "UPDATE orchestration_v2_effect_outbox SET status = 'failed', lease_owner = NULL, lease_expires_at = NULL, completed_at = ?1, updated_at = ?1, last_error = ?2 WHERE effect_id = ?3 AND status = 'running' AND lease_owner = ?4",
                    params![now_iso, error, id_for_sql, worker_id],
                ),
                Settle::Retry { ref error, delay_ms } => {
                    let available = format_utc_millis(now + Duration::milliseconds(delay_ms.max(0)));
                    conn.execute(
                        "UPDATE orchestration_v2_effect_outbox SET status = 'pending', available_at = ?1, lease_owner = NULL, lease_expires_at = NULL, updated_at = ?2, last_error = ?3 WHERE effect_id = ?4 AND status = 'running' AND lease_owner = ?5",
                        params![available, now_iso, error, id_for_sql, worker_id],
                    )
                }
            }
            .map_err(|err| PersistenceError::sql("EffectOutbox.settle", &err))?;
            Ok((changed, matches!(outcome, Settle::Retry { .. })))
        }).await?;
        let (changed, retried) = changed;
        if changed == 1 {
            self.cancellations.lock().remove(&id);
            if !retried {
                self.signal_settled(std::slice::from_ref(&id));
            }
            self.notify_available(1);
        }
        Ok(changed == 1)
    }

    pub async fn succeed(&self, id: String, worker_id: String) -> Result<bool, PersistenceError> {
        self.settle(id, worker_id, Settle::Succeed).await
    }

    pub async fn retry(
        &self,
        id: String,
        worker_id: String,
        error: String,
        delay_ms: i64,
    ) -> Result<bool, PersistenceError> {
        self.settle(id, worker_id, Settle::Retry { error, delay_ms })
            .await
    }

    pub async fn fail(
        &self,
        id: String,
        worker_id: String,
        error: String,
    ) -> Result<bool, PersistenceError> {
        self.settle(id, worker_id, Settle::Fail(error)).await
    }

    /// Resolves once the effect is neither pending nor running, or right away
    /// when it does not exist.
    pub async fn await_settled(&self, id: String) -> Result<(), PersistenceError> {
        // Registered before the read, so a settle right after it still resolves.
        let signal = self.settled.lock().entry(id.clone()).or_default().clone();
        let effect = self.get(id.clone()).await?;
        if effect.is_some_and(|e| matches!(e.status, EffectStatus::Pending | EffectStatus::Running))
        {
            signal.cancelled().await;
            return Ok(());
        }
        // Settled already: release every waiter that shares this signal.
        self.signal_settled(std::slice::from_ref(&id));
        Ok(())
    }

    /// After a process restart: process-bound effects (a turn start that never
    /// finished) are cancelled; replay-safe running effects go back to pending.
    pub async fn reconcile_after_process_loss(&self) -> Result<ReconcileResult, PersistenceError> {
        let result = self.db.write(|conn| {
            let now = utc_millis_now();
            let bound = PROCESS_BOUND_EFFECT_TYPES.iter().map(|t| t.as_str().to_owned().into()).collect::<Vec<rusqlite::types::Value>>();
            let mut values: Vec<rusqlite::types::Value> = vec![now.clone().into()];
            values.extend(bound);
            let cancelled = conn.execute(
                &format!("UPDATE orchestration_v2_effect_outbox SET status = 'cancelled', lease_owner = NULL, lease_expires_at = NULL, completed_at = ?1, updated_at = ?1, last_error = 'Cancelled because the server process ended before the effect completed.' WHERE status IN ('pending', 'running') AND effect_type IN ({})", in_list(PROCESS_BOUND_EFFECT_TYPES.len(), 2)),
                params_from_iter(values),
            ).map_err(|err| PersistenceError::sql("EffectOutbox.reconcile-process-loss", &err))?;
            let safe = REPLAY_SAFE_EFFECT_TYPES_AFTER_PROCESS_LOSS.iter().map(|t| t.as_str().to_owned().into()).collect::<Vec<rusqlite::types::Value>>();
            let mut values: Vec<rusqlite::types::Value> = vec![now.into()];
            values.extend(safe);
            let requeued = conn.execute(
                &format!("UPDATE orchestration_v2_effect_outbox SET status = 'pending', lease_owner = NULL, lease_expires_at = NULL, available_at = ?1, updated_at = ?1, last_error = 'Requeued after the previous server process ended.' WHERE status = 'running' AND effect_type IN ({})", in_list(REPLAY_SAFE_EFFECT_TYPES_AFTER_PROCESS_LOSS.len(), 2)),
                params_from_iter(values),
            ).map_err(|err| PersistenceError::sql("EffectOutbox.reconcile-process-loss", &err))?;
            Ok(ReconcileResult { requeued, cancelled })
        }).await?;
        if result.requeued > 0 {
            self.notify_available(result.requeued);
        }
        Ok(result)
    }

    /// Deletes succeeded and cancelled rows older than the retention window.
    /// Each batch is its own transaction, so a large backlog never holds the
    /// write lock for long and writers can commit between batches.
    pub async fn prune_settled(&self) -> Result<usize, PersistenceError> {
        let cutoff = format_utc_millis(Utc::now() - Duration::days(SETTLED_EFFECT_RETENTION_DAYS));
        let mut total = 0;
        loop {
            let cutoff = cutoff.clone();
            let deleted = self.db.write(move |conn| conn.execute(
                "DELETE FROM orchestration_v2_effect_outbox WHERE rowid IN (SELECT rowid FROM orchestration_v2_effect_outbox WHERE status IN ('succeeded','cancelled') AND completed_at < ?1 LIMIT ?2)",
                params![cutoff, PRUNE_BATCH_SIZE],
            ).map_err(|err| PersistenceError::sql("EffectOutbox.prune-settled", &err))).await?;
            total += deleted;
            if deleted < PRUNE_BATCH_SIZE as usize {
                return Ok(total);
            }
            tokio::task::yield_now().await;
        }
    }

    /// Prunes settled effect rows right away (the caller starts this once the
    /// app is active), then every hour. Stops when the returned task drops.
    pub fn spawn_prune_worker(&self) -> OwnedTask {
        let outbox = self.clone();
        OwnedTask::spawn(async move {
            loop {
                if let Err(error) = outbox.prune_settled().await {
                    tracing::warn!(%error, "Failed to prune settled effects");
                }
                tokio::time::sleep(PRUNE_INTERVAL).await;
            }
        })
    }
}

enum Settle {
    Succeed,
    Fail(String),
    Retry { error: String, delay_ms: i64 },
}

type RawRow = (
    String,
    String,
    String,
    String,
    String,
    i64,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
);

fn decode_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
    Ok((
        row.get("effect_id")?,
        row.get("command_id")?,
        row.get("thread_id")?,
        row.get("payload_json")?,
        row.get("status")?,
        row.get("attempt_count")?,
        row.get("available_at")?,
        row.get("lease_owner")?,
        row.get("lease_expires_at")?,
        row.get("created_at")?,
        row.get("updated_at")?,
        row.get("completed_at")?,
        row.get("last_error")?,
    ))
}

fn decode_effect(row: RawRow) -> Result<Effect, PersistenceError> {
    let (
        id,
        command_id,
        thread_id,
        request,
        status,
        attempt_count,
        available_at,
        lease_owner,
        lease_expires_at,
        created_at,
        updated_at,
        completed_at,
        last_error,
    ) = row;
    Ok(Effect {
        id,
        command_id: CommandId::new(&command_id).map_err(|err| decode_err(err.to_string()))?,
        thread_id: ThreadId::new(&thread_id).map_err(|err| decode_err(err.to_string()))?,
        request: serde_json::from_str(&request).map_err(|err| decode_err(err.to_string()))?,
        status: EffectStatus::parse(&status)?,
        attempt_count,
        available_at,
        lease_owner,
        lease_expires_at,
        created_at,
        updated_at,
        completed_at,
        last_error,
    })
}

fn get_in(conn: &Connection, id: &str) -> Result<Option<Effect>, PersistenceError> {
    conn.query_row(
        "SELECT * FROM orchestration_v2_effect_outbox WHERE effect_id = ?1 LIMIT 1",
        [id],
        decode_row,
    )
    .optional()
    .map_err(|err| PersistenceError::sql("EffectOutbox.get", &err))?
    .map(decode_effect)
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn effect(
        id: &str,
        command: &CommandId,
        thread: &ThreadId,
        request: EffectRequest,
    ) -> PendingEffect {
        PendingEffect {
            id: id.into(),
            command_id: command.clone(),
            thread_id: thread.clone(),
            request,
            available_at: None,
        }
    }

    fn start(run: &str) -> EffectRequest {
        EffectRequest::ProviderTurnStart {
            run_id: RunId::new(run).unwrap(),
        }
    }

    fn detach() -> EffectRequest {
        EffectRequest::ProviderSessionDetach {
            provider_session_id: ProviderSessionId::new("session").unwrap(),
            detail: None,
            revoke_mcp_credential: None,
        }
    }

    fn title() -> EffectRequest {
        EffectRequest::ThreadTitleGenerate {
            kind: ThreadTitleKind::Regenerate,
        }
    }

    #[test]
    fn request_json_matches_upstream_shape() {
        let request = EffectRequest::ProviderSessionDetach {
            provider_session_id: ProviderSessionId::new("session:1").unwrap(),
            detail: Some("Thread deleted.".into()),
            revoke_mcp_credential: Some(true),
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({
                "type": "provider-session.detach",
                "providerSessionId": "session:1",
                "detail": "Thread deleted.",
                "revokeMcpCredential": true,
            })
        );
        assert_eq!(
            serde_json::to_value(EffectRequest::TerminalCleanup {}).unwrap(),
            serde_json::json!({"type": "terminal.cleanup"})
        );
        let restart: EffectRequest = serde_json::from_value(serde_json::json!({
            "type": "provider-turn.restart",
            "providerSessionId": "s", "providerThreadId": "t", "providerTurnId": "u",
            "interruptedAttemptId": "a", "runId": "r",
            "sessionTransition": {"type": "replace", "replacementProviderSessionId": "s2"},
        }))
        .unwrap();
        assert_eq!(restart.effect_type(), EffectType::ProviderTurnRestart);
        assert!(
            serde_json::from_value::<EffectRequest>(serde_json::json!({"type": "unknown.type"}))
                .is_err()
        );
    }

    #[tokio::test]
    async fn retry_blocks_later_lifecycle_effect_but_not_title_lane() {
        let db = Db::open_in_memory().await.unwrap();
        let outbox = EffectOutbox::new(db);
        let command = CommandId::new("cmd:outbox").unwrap();
        let thread = ThreadId::new("thread:outbox").unwrap();
        outbox
            .enqueue(vec![
                effect("a", &command, &thread, start("run")),
                effect("b", &command, &thread, detach()),
                effect("title", &command, &thread, title()),
            ])
            .await
            .unwrap();
        let first = outbox
            .claim_next("worker".into(), 30_000, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.id, "a");
        let title = outbox
            .claim_next("worker".into(), 30_000, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(title.id, "title");
        assert!(
            outbox
                .claim_next("worker".into(), 30_000, false)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            outbox
                .retry("a".into(), "worker".into(), "retry".into(), 5_000)
                .await
                .unwrap()
        );
        assert!(
            outbox
                .claim_next("worker".into(), 30_000, false)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            outbox
                .succeed("title".into(), "worker".into())
                .await
                .unwrap()
        );
        assert!(
            outbox
                .claim_next("worker".into(), 30_000, false)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn restart_reconciles_process_bound_and_replay_safe_effects() {
        let db = Db::open_in_memory().await.unwrap();
        let outbox = EffectOutbox::new(db);
        let command = CommandId::new("cmd:restart").unwrap();
        let thread = ThreadId::new("thread:restart").unwrap();
        outbox
            .enqueue(vec![
                effect(
                    "interrupt",
                    &command,
                    &thread,
                    EffectRequest::ProviderTurnInterrupt {
                        provider_session_id: ProviderSessionId::new("s").unwrap(),
                        provider_thread_id: ProviderThreadId::new("t").unwrap(),
                        provider_turn_id: ProviderTurnId::new("u").unwrap(),
                        subagent: None,
                    },
                ),
                effect(
                    "cleanup",
                    &command,
                    &thread,
                    EffectRequest::AttachmentCleanup {
                        attachment_ids: vec![],
                    },
                ),
            ])
            .await
            .unwrap();
        let claimed = outbox
            .claim_next("worker".into(), 30_000, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, "interrupt");
        let result = outbox.reconcile_after_process_loss().await.unwrap();
        assert_eq!(result.cancelled, 1);
        assert_eq!(
            outbox
                .get("interrupt".into())
                .await
                .unwrap()
                .unwrap()
                .status,
            EffectStatus::Cancelled
        );
        assert_eq!(
            outbox.get("cleanup".into()).await.unwrap().unwrap().status,
            EffectStatus::Pending
        );
    }

    #[tokio::test]
    async fn requeues_a_running_replay_safe_effect_after_process_loss() {
        let outbox = EffectOutbox::new(Db::open_in_memory().await.unwrap());
        let command = CommandId::new("cmd:requeue").unwrap();
        let thread = ThreadId::new("thread:requeue").unwrap();
        outbox
            .enqueue(vec![effect("detach", &command, &thread, detach())])
            .await
            .unwrap();
        outbox
            .claim_next("worker".into(), 30_000, false)
            .await
            .unwrap()
            .unwrap();
        let result = outbox.reconcile_after_process_loss().await.unwrap();
        assert_eq!(
            result,
            ReconcileResult {
                requeued: 1,
                cancelled: 0
            }
        );
        let row = outbox.get("detach".into()).await.unwrap().unwrap();
        assert_eq!(row.status, EffectStatus::Pending);
        assert_eq!(
            row.last_error.as_deref(),
            Some("Requeued after the previous server process ended.")
        );
    }

    #[tokio::test]
    async fn enqueue_ignores_an_existing_effect_id() {
        let outbox = EffectOutbox::new(Db::open_in_memory().await.unwrap());
        let command = CommandId::new("cmd:dup").unwrap();
        let thread = ThreadId::new("thread:dup").unwrap();
        outbox
            .enqueue(vec![effect("same", &command, &thread, start("run:1"))])
            .await
            .unwrap();
        outbox
            .enqueue(vec![effect("same", &command, &thread, start("run:2"))])
            .await
            .unwrap();
        let rows = outbox.list_by_command_id(command).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].request, start("run:1"));
    }

    #[tokio::test]
    async fn cancel_unsettled_cancels_matching_types_and_wakes_waiters() {
        let outbox = EffectOutbox::new(Db::open_in_memory().await.unwrap());
        let command = CommandId::new("cmd:cancel").unwrap();
        let thread = ThreadId::new("thread:cancel").unwrap();
        outbox
            .enqueue(vec![
                effect("start", &command, &thread, start("run")),
                effect("detach", &command, &thread, detach()),
            ])
            .await
            .unwrap();
        let claimed = outbox
            .claim_next("worker".into(), 30_000, false)
            .await
            .unwrap()
            .unwrap();
        let signal = outbox.cancellation_signal(&claimed.id);
        let waiter = {
            let outbox = outbox.clone();
            tokio::spawn(async move { outbox.await_settled("start".into()).await })
        };
        tokio::task::yield_now().await;
        let tid = thread.clone();
        let ids = outbox
            .db()
            .write(move |tx| {
                EffectOutbox::cancel_unsettled_in(
                    tx,
                    &tid,
                    &[EffectType::ProviderTurnStart],
                    "stopped",
                )
            })
            .await
            .unwrap();
        assert_eq!(ids, vec!["start".to_string()]);
        outbox.signal_cancellations(&ids);
        signal.cancelled().await;
        waiter.await.unwrap().unwrap();
        let row = outbox.get("start".into()).await.unwrap().unwrap();
        assert_eq!(row.status, EffectStatus::Cancelled);
        assert_eq!(row.last_error.as_deref(), Some("stopped"));
        assert_eq!(
            outbox.get("detach".into()).await.unwrap().unwrap().status,
            EffectStatus::Pending
        );
        // A worker that lost the race cannot settle the cancelled row.
        assert!(
            !outbox
                .succeed("start".into(), "worker".into())
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn await_settled_returns_for_a_missing_or_finished_effect() {
        let outbox = EffectOutbox::new(Db::open_in_memory().await.unwrap());
        outbox.await_settled("missing".into()).await.unwrap();
        let command = CommandId::new("cmd:settled").unwrap();
        let thread = ThreadId::new("thread:settled").unwrap();
        outbox
            .enqueue(vec![effect("e", &command, &thread, detach())])
            .await
            .unwrap();
        let claimed = outbox
            .claim_next("w".into(), 30_000, false)
            .await
            .unwrap()
            .unwrap();
        let waiter = {
            let outbox = outbox.clone();
            tokio::spawn(async move { outbox.await_settled("e".into()).await })
        };
        tokio::task::yield_now().await;
        assert!(outbox.succeed(claimed.id, "w".into()).await.unwrap());
        waiter.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn excluding_restart_continuations_skips_them_and_is_not_blocked_by_them() {
        let outbox = EffectOutbox::new(Db::open_in_memory().await.unwrap());
        let command = CommandId::new("cmd:continue").unwrap();
        let thread = ThreadId::new("thread:continue").unwrap();
        outbox
            .enqueue(vec![
                effect(
                    "continue",
                    &command,
                    &thread,
                    EffectRequest::ProviderRuntimeContinue {
                        source_run_id: RunId::new("run").unwrap(),
                    },
                ),
                effect("detach", &command, &thread, detach()),
            ])
            .await
            .unwrap();
        let claimed = outbox
            .claim_next("w".into(), 30_000, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, "detach");
    }

    #[tokio::test]
    async fn prune_removes_only_old_succeeded_and_cancelled_rows() {
        let db = Db::open_in_memory().await.unwrap();
        let outbox = EffectOutbox::new(db.clone());
        let command = CommandId::new("cmd:prune").unwrap();
        let thread = ThreadId::new("thread:prune").unwrap();
        outbox
            .enqueue(vec![
                effect("old-ok", &command, &thread, start("1")),
                effect("old-failed", &command, &thread, start("2")),
                effect("fresh-ok", &command, &thread, start("3")),
            ])
            .await
            .unwrap();
        let old = format_utc_millis(Utc::now() - Duration::days(8));
        let fresh = format_utc_millis(Utc::now());
        db.write(move |conn| {
            conn.execute("UPDATE orchestration_v2_effect_outbox SET status = 'succeeded', completed_at = ?1 WHERE effect_id = 'old-ok'", [&old]).unwrap();
            conn.execute("UPDATE orchestration_v2_effect_outbox SET status = 'failed', completed_at = ?1 WHERE effect_id = 'old-failed'", [&old]).unwrap();
            conn.execute("UPDATE orchestration_v2_effect_outbox SET status = 'succeeded', completed_at = ?1 WHERE effect_id = 'fresh-ok'", [&fresh]).unwrap();
            Ok(())
        }).await.unwrap();
        assert_eq!(outbox.prune_settled().await.unwrap(), 1);
        assert!(outbox.get("old-ok".into()).await.unwrap().is_none());
        assert!(outbox.get("old-failed".into()).await.unwrap().is_some());
        assert!(outbox.get("fresh-ok".into()).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn availability_notifications_are_counted_and_capped() {
        let outbox = EffectOutbox::new(Db::open_in_memory().await.unwrap());
        outbox.notify_available(3);
        for _ in 0..3 {
            outbox.await_available().await;
        }
        outbox.notify_available(1_000);
        assert_eq!(outbox.available.available_permits(), AVAILABLE_BURST);
    }
}
