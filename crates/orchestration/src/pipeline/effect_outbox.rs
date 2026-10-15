//! Port of upstream/apps/server/src/orchestration-v2/EffectOutbox.ts
//!
//! The outbox is durable. Notifications are only latency hints and are sent
//! after the enclosing writer transaction commits. Each thread has one
//! lifecycle lane and an independent title-generation lane.

use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::Arc;
use tokio::sync::Notify;
use turin_contracts::{CommandId, ThreadId};
use turin_persistence::{Db, PersistenceError, format_utc_millis, utc_millis_now};

pub const REPLAY_SAFE_EFFECT_TYPES_AFTER_PROCESS_LOSS: &[&str] = &[
    "provider-runtime.continue",
    "provider-session.detach",
    "provider-thread.rollback",
    "checkpoint.capture",
    "terminal.cleanup",
    "attachment.cleanup",
    "thread-title.generate",
    "delegated-tasks.stop",
];
pub const PROCESS_BOUND_EFFECT_TYPES: &[&str] = &[
    "provider-turn.start",
    "provider-turn.interrupt",
    "provider-turn.steer",
    "provider-turn.restart",
    "runtime-request.respond",
];
pub const SETTLED_EFFECT_RETENTION_DAYS: i64 = 7;
pub const PRUNE_BATCH_SIZE: i64 = 500;
pub const EFFECT_TYPES: &[&str] = &[
    "provider-runtime.continue",
    "provider-session.detach",
    "provider-turn.start",
    "provider-turn.interrupt",
    "provider-turn.steer",
    "provider-turn.restart",
    "runtime-request.respond",
    "provider-thread.rollback",
    "checkpoint.capture",
    "terminal.cleanup",
    "attachment.cleanup",
    "thread-title.generate",
    "delegated-tasks.stop",
];

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
            _ => Err(PersistenceError::Decode(
                turin_persistence::PersistenceDecodeError::new(
                    "EffectOutbox.decode",
                    format!("unknown status {s}"),
                ),
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PendingEffect {
    pub id: String,
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    /// Exact upstream request JSON, including its "type" discriminator.
    pub request: serde_json::Value,
    pub available_at: Option<String>,
}

impl PendingEffect {
    pub fn effect_type(&self) -> Result<&str, PersistenceError> {
        let kind = self
            .request
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                PersistenceError::Decode(turin_persistence::PersistenceDecodeError::new(
                    "EffectOutbox.enqueue",
                    "effect request lacks type",
                ))
            })?;
        if !EFFECT_TYPES.contains(&kind) {
            return Err(PersistenceError::Decode(
                turin_persistence::PersistenceDecodeError::new(
                    "EffectOutbox.enqueue",
                    format!("unknown effect type {kind}"),
                ),
            ));
        }
        Ok(kind)
    }
}

#[derive(Debug, Clone)]
pub struct Effect {
    pub id: String,
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    pub request: serde_json::Value,
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

#[derive(Clone)]
pub struct EffectOutbox {
    db: Db,
    available: Arc<Notify>,
    settled: Arc<Notify>,
    cancellations: Arc<std::sync::Mutex<std::collections::HashMap<String, Arc<Notify>>>>,
}

impl EffectOutbox {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            available: Arc::new(Notify::new()),
            settled: Arc::new(Notify::new()),
            cancellations: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }
    pub fn db(&self) -> &Db {
        &self.db
    }
    pub fn notify_available(&self, count: usize) {
        for _ in 0..count.min(64) {
            self.available.notify_one();
        }
    }
    pub async fn await_available(&self) {
        self.available.notified().await;
    }
    pub fn signal_cancellations(&self, ids: &[String]) {
        let signals = self.cancellations.lock().unwrap_or_else(|e| e.into_inner());
        for id in ids {
            if let Some(signal) = signals.get(id) {
                signal.notify_waiters();
            }
        }
        if !ids.is_empty() {
            self.settled.notify_waiters();
            self.notify_available(ids.len());
        }
    }
    pub fn cancellation_signal(&self, id: &str) -> Arc<Notify> {
        let mut signals = self.cancellations.lock().unwrap_or_else(|e| e.into_inner());
        signals
            .entry(id.to_owned())
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    }
    pub fn clear_cancellation(&self, id: &str) {
        self.cancellations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }
    pub fn signal_settled(&self) {
        self.settled.notify_waiters();
        self.notify_available(1);
    }

    /// Insert rows in the caller's transaction; callers notify after commit.
    pub fn enqueue_in(
        conn: &Connection,
        effects: &[PendingEffect],
    ) -> Result<(), PersistenceError> {
        let now = utc_millis_now();
        for effect in effects {
            let kind = effect.effect_type()?;
            let payload = serde_json::to_string(&effect.request).map_err(|err| {
                PersistenceError::Decode(turin_persistence::PersistenceDecodeError::new(
                    "EffectOutbox.enqueue",
                    err.to_string(),
                ))
            })?;
            conn.execute(
                "INSERT INTO orchestration_v2_effect_outbox (effect_id, command_id, thread_id, effect_type, payload_json, status, attempt_count, available_at, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', 0, ?6, ?7, ?7) ON CONFLICT(effect_id) DO NOTHING",
                params![effect.id, effect.command_id.as_str(), effect.thread_id.as_str(), kind, payload, effect.available_at.as_deref().unwrap_or(&now), now],
            ).map_err(|err| PersistenceError::sql("EffectOutbox.enqueue", &err))?;
        }
        Ok(())
    }

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

    pub fn cancel_unsettled_in(
        conn: &Connection,
        thread_id: &ThreadId,
        effect_types: &[String],
        reason: &str,
    ) -> Result<Vec<String>, PersistenceError> {
        if effect_types.is_empty() {
            return Ok(vec![]);
        }
        let now = utc_millis_now();
        let mut ids = Vec::new();
        let mut stmt = conn.prepare(
            "UPDATE orchestration_v2_effect_outbox SET status = 'cancelled', lease_owner = NULL, lease_expires_at = NULL, completed_at = ?1, updated_at = ?1, last_error = ?2 WHERE thread_id = ?3 AND status IN ('pending', 'running') AND effect_type = ?4 RETURNING effect_id"
        ).map_err(|err| PersistenceError::sql("EffectOutbox.cancel-unsettled", &err))?;
        for kind in effect_types {
            let rows = stmt
                .query_map(params![now, reason, thread_id.as_str(), kind], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|err| PersistenceError::sql("EffectOutbox.cancel-unsettled", &err))?;
            for row in rows {
                ids.push(
                    row.map_err(|err| {
                        PersistenceError::sql("EffectOutbox.cancel-unsettled", &err)
                    })?,
                );
            }
        }
        Ok(ids)
    }

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
                "UPDATE orchestration_v2_effect_outbox SET status = 'running', attempt_count = attempt_count + 1, lease_owner = ?1, lease_expires_at = ?2, updated_at = ?3, last_error = NULL WHERE effect_id = (SELECT candidate.effect_id FROM orchestration_v2_effect_outbox AS candidate WHERE candidate.available_at <= ?3 AND candidate.status = 'pending' AND (?4 = 0 OR candidate.effect_type != 'provider-runtime.continue') AND NOT EXISTS (SELECT 1 FROM orchestration_v2_effect_outbox AS active WHERE active.thread_id = candidate.thread_id AND (active.status = 'running' OR (active.status = 'pending' AND active.rowid < candidate.rowid AND (?4 = 0 OR active.effect_type != 'provider-runtime.continue'))) AND ((candidate.effect_type = 'thread-title.generate' AND active.effect_type = 'thread-title.generate') OR (candidate.effect_type != 'thread-title.generate' AND active.effect_type != 'thread-title.generate'))) ORDER BY candidate.available_at ASC, candidate.created_at ASC, candidate.effect_id ASC LIMIT 1) RETURNING *"
            ).map_err(|err| PersistenceError::sql("EffectOutbox.claim", &err))?;
            let raw = stmt.query_row(params![worker_id, expires, now_iso, exclude_restart_continuations as i64], decode_row).optional().map_err(|err| PersistenceError::sql("EffectOutbox.claim", &err))?;
            raw.map(decode_effect).transpose()
        }).await?;
        if let Some(effect) = &claimed {
            self.cancellation_signal(&effect.id);
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
                        PersistenceError::Decode(turin_persistence::PersistenceDecodeError::new(
                            "EffectOutbox.next-claimable",
                            format!("invalid available_at {text}"),
                        ))
                    })
            })
            .transpose()
    }

    async fn settle(
        &self,
        id: String,
        worker_id: String,
        status: &'static str,
        error: Option<String>,
        delay_ms: i64,
    ) -> Result<bool, PersistenceError> {
        let settled = self.db.write(move |conn| {
            let now = Utc::now();
            let now_iso = format_utc_millis(now);
            let available = format_utc_millis(now + Duration::milliseconds(delay_ms.max(0)));
            let changed = conn.execute(
                "UPDATE orchestration_v2_effect_outbox SET status = ?1, available_at = ?2, lease_owner = NULL, lease_expires_at = NULL, completed_at = CASE WHEN ?1 = 'pending' THEN NULL ELSE ?3 END, updated_at = ?3, last_error = ?4 WHERE effect_id = ?5 AND status = 'running' AND lease_owner = ?6",
                params![status, available, now_iso, error, id, worker_id]
            ).map_err(|err| PersistenceError::sql("EffectOutbox.settle", &err))?;
            Ok(changed == 1)
        }).await?;
        if settled {
            self.signal_settled();
        }
        Ok(settled)
    }
    pub async fn succeed(&self, id: String, worker_id: String) -> Result<bool, PersistenceError> {
        self.settle(id, worker_id, "succeeded", None, 0).await
    }
    pub async fn retry(
        &self,
        id: String,
        worker_id: String,
        error: String,
        delay_ms: i64,
    ) -> Result<bool, PersistenceError> {
        self.settle(id, worker_id, "pending", Some(error), delay_ms)
            .await
    }
    pub async fn fail(
        &self,
        id: String,
        worker_id: String,
        error: String,
    ) -> Result<bool, PersistenceError> {
        self.settle(id, worker_id, "failed", Some(error), 0).await
    }

    pub async fn await_settled(&self, id: String) -> Result<(), PersistenceError> {
        loop {
            let notified = self.settled.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let active = self.get(id.clone()).await?.is_some_and(|effect| {
                matches!(effect.status, EffectStatus::Pending | EffectStatus::Running)
            });
            if !active {
                return Ok(());
            }
            notified.await;
        }
    }

    pub async fn reconcile_after_process_loss(&self) -> Result<ReconcileResult, PersistenceError> {
        let result = self.db.write(|conn| {
            let now = utc_millis_now();
            let mut cancelled = 0;
            for kind in PROCESS_BOUND_EFFECT_TYPES {
                cancelled += conn.execute("UPDATE orchestration_v2_effect_outbox SET status = 'cancelled', lease_owner = NULL, lease_expires_at = NULL, completed_at = ?1, updated_at = ?1, last_error = 'Cancelled because the server process ended before the effect completed.' WHERE status IN ('pending','running') AND effect_type = ?2", params![now, kind]).map_err(|err| PersistenceError::sql("EffectOutbox.reconcile", &err))?;
            }
            let mut requeued = 0;
            for kind in REPLAY_SAFE_EFFECT_TYPES_AFTER_PROCESS_LOSS {
                requeued += conn.execute("UPDATE orchestration_v2_effect_outbox SET status = 'pending', lease_owner = NULL, lease_expires_at = NULL, available_at = ?1, updated_at = ?1, last_error = 'Requeued after the previous server process ended.' WHERE status = 'running' AND effect_type = ?2", params![now, kind]).map_err(|err| PersistenceError::sql("EffectOutbox.reconcile", &err))?;
            }
            Ok(ReconcileResult { requeued, cancelled })
        }).await?;
        self.notify_available(result.requeued);
        if result.cancelled > 0 {
            self.settled.notify_waiters();
        }
        Ok(result)
    }
    pub async fn prune_settled(&self) -> Result<usize, PersistenceError> {
        let cutoff = format_utc_millis(Utc::now() - Duration::days(SETTLED_EFFECT_RETENTION_DAYS));
        let mut total = 0;
        loop {
            let cutoff = cutoff.clone();
            let deleted = self.db.write(move |conn| conn.execute(
                "DELETE FROM orchestration_v2_effect_outbox WHERE rowid IN (SELECT rowid FROM orchestration_v2_effect_outbox WHERE status IN ('succeeded','cancelled') AND completed_at < ?1 LIMIT ?2)",
                params![cutoff, PRUNE_BATCH_SIZE],
            ).map_err(|err| PersistenceError::sql("EffectOutbox.prune", &err))).await?;
            total += deleted;
            if deleted < PRUNE_BATCH_SIZE as usize {
                return Ok(total);
            }
            tokio::task::yield_now().await;
        }
    }
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
    let invalid = |message: String| {
        PersistenceError::Decode(turin_persistence::PersistenceDecodeError::new(
            "EffectOutbox.decode",
            message,
        ))
    };
    Ok(Effect {
        id,
        command_id: CommandId::new(&command_id).map_err(|err| invalid(err.to_string()))?,
        thread_id: ThreadId::new(&thread_id).map_err(|err| invalid(err.to_string()))?,
        request: serde_json::from_str(&request).map_err(|err| invalid(err.to_string()))?,
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
