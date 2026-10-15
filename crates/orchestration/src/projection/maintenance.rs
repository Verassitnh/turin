//! Port of upstream/apps/server/src/orchestration-v2/ProjectionMaintenance.ts
//!
//! `verify` checks the transaction boundary (metadata sequence and thread set)
//! and that every stored projection decodes. It does not replay events: the
//! sink commits events and projections together, and a second projector would
//! have to track [`super::store::apply`] exactly.
//!
//! Upstream reads events through `EventStoreV2`. The read here is raw SQL, run
//! inside the same write transaction as the projection rewrite, because the
//! persistence crate's row reader is private and a rebuild must not interleave
//! with live appends. `RESTORED_PAYLOAD` is copied from
//! `turin_persistence::orchestration_event_store` and must stay identical.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use turin_contracts::ThreadId;
use turin_contracts::orchestration_v2::OrchestrationV2DomainEvent;
use turin_persistence::{Db, PersistenceDecodeError, PersistenceError, utc_millis_now};

use crate::projection::error::Cause;
use crate::projection::event::event_thread_id;
use crate::projection::store::ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION;
use crate::projection::store::ProjectionStore;
use crate::projection::store::apply::apply_event;
use crate::projection::store::unreadable::read_unreadable_thread_ids;
use crate::projection::turn_item;

/// Metadata row name, `projection_name` in `orchestration_v2_projection_metadata`.
const PROJECTION_NAME: &str = "thread-projections";

/// Events replayed per page during rebuild (upstream `pageSize`).
const REBUILD_PAGE_SIZE: i64 = 500;

/// Events and receipts scanned per compaction page (upstream `COMPACTION_PAGE_SIZE`).
const COMPACTION_PAGE_SIZE: i64 = 500;

/// Projection tables cleared by rebuild, in upstream order.
const REBUILD_TABLES: &[&str] = &[
    "orchestration_v2_projection_context_transfers",
    "orchestration_v2_projection_context_handoffs",
    "orchestration_v2_projection_checkpoints",
    "orchestration_v2_projection_checkpoint_scopes",
    "orchestration_v2_projection_turn_items",
    "orchestration_v2_projection_plans",
    "orchestration_v2_projection_messages",
    "orchestration_v2_projection_runtime_requests",
    "orchestration_v2_projection_provider_turns",
    "orchestration_v2_projection_provider_threads",
    "orchestration_v2_projection_provider_session_bindings",
    "orchestration_v2_projection_provider_sessions",
    "orchestration_v2_projection_subagents",
    "orchestration_v2_projection_nodes",
    "orchestration_v2_projection_run_attempts",
    "orchestration_v2_projection_runs",
    "orchestration_v2_projection_threads",
    "orchestration_v2_turn_item_positions",
];

/// Thread-state events whose payload is the complete thread: only the newest
/// per thread can influence a replay or an afterSequence catch-up.
/// `thread.created` stays out: verify derives the expected thread set from it,
/// and it anchors replay ordering.
const SUPERSEDABLE_THREAD_EVENT_TYPES: &[&str] = &[
    "thread.archived",
    "thread.unarchived",
    "thread.deleted",
    "thread.settled",
    "thread.unsettled",
    "thread.snoozed",
    "thread.unsnoozed",
    "thread.pinned",
    "thread.auto-settle-set",
    "thread.unpinned",
    "thread.pin-reordered",
    "thread.active-reordered",
    "thread.metadata-updated",
    "thread.pull-request-synced",
    "thread.runtime-mode-updated",
    "thread.interaction-mode-updated",
    "thread.model-selection-updated",
    "thread.provider-switched",
    "thread.visited",
    "thread.marked-unread",
];

/// `subagent.updated` and `turn-item.updated` may be stored slimmed: the prompt
/// lives on the anchor event named by `promptSequence`. Copied from the
/// persistence crate's `RESTORED_PAYLOAD`.
const RESTORED_PAYLOAD: &str = "
CASE
  WHEN orchestration_events.event_type IN ('subagent.updated', 'turn-item.updated')
    AND json_type(orchestration_events.payload_json, '$.promptSequence') = 'integer'
  THEN json_set(
    json_remove(orchestration_events.payload_json, '$.promptSequence'),
    '$.prompt',
    (SELECT json_extract(anchor.payload_json, '$.prompt')
     FROM orchestration_events AS anchor
     WHERE anchor.sequence = json_extract(orchestration_events.payload_json, '$.promptSequence'))
  )
  ELSE orchestration_events.payload_json
END
";

/// `ProjectionVerificationV2`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionVerification {
    pub valid: bool,
    pub schema_version: i64,
    pub expected_sequence: i64,
    pub projection_sequence: i64,
    pub unreadable_thread_ids: Vec<ThreadId>,
    pub missing_thread_ids: Vec<ThreadId>,
    pub unexpected_thread_ids: Vec<ThreadId>,
}

/// Result of [`ProjectionMaintenance::compact_event_store`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CompactionReport {
    pub deleted_event_count: u64,
    pub deleted_receipt_count: u64,
    pub reclaimable_bytes: i64,
}

/// `ProjectionMaintenanceError`: the operation name and the underlying cause.
#[derive(Debug, thiserror::Error)]
#[error("Projection maintenance failed during {operation}.")]
pub struct ProjectionMaintenanceError {
    pub operation: &'static str,
    #[source]
    pub cause: Cause,
}

impl ProjectionMaintenanceError {
    fn wrap(operation: &'static str) -> impl FnOnce(PersistenceError) -> Self {
        move |cause| Self {
            operation,
            cause: Box::new(cause),
        }
    }
}

/// Startup verification, rebuild, and event-log compaction for the thread projections.
#[derive(Clone)]
pub struct ProjectionMaintenance {
    store: ProjectionStore,
}

impl ProjectionMaintenance {
    pub fn new(store: ProjectionStore) -> Self {
        Self { store }
    }

    fn db(&self) -> &Db {
        self.store.db()
    }

    /// `verify`, in one read transaction so the metadata, thread set, and
    /// unreadable scan agree with each other.
    pub async fn verify(&self) -> Result<ProjectionVerification, ProjectionMaintenanceError> {
        self.db()
            .read(verify_in)
            .await
            .map_err(ProjectionMaintenanceError::wrap("verify"))
    }

    /// `rebuild`: clear the projection tables and replay every event through
    /// `thread.created`'s high-water mark, all in one write transaction, then
    /// verify.
    pub async fn rebuild(&self) -> Result<ProjectionVerification, ProjectionMaintenanceError> {
        let wrap = ProjectionMaintenanceError::wrap("rebuild");
        self.db().write(rebuild_in).await.map_err(wrap)?;
        self.db()
            .read(verify_in)
            .await
            .map_err(ProjectionMaintenanceError::wrap("rebuild"))
    }

    /// `compactEventStore`: drop superseded thread-state events and entity
    /// rows, and legacy receipts whose thread import finished. Each page is
    /// discovered on the reader and deleted in its own write transaction.
    pub async fn compact_event_store(
        &self,
    ) -> Result<CompactionReport, ProjectionMaintenanceError> {
        let wrap = || ProjectionMaintenanceError::wrap("compact event store");
        let (mut through_sequence, mut through_receipt_row_id) = self
            .db()
            .read(|conn| {
                conn.query_row(
                    "SELECT
                       COALESCE((SELECT MAX(sequence) FROM orchestration_events), 0),
                       COALESCE((SELECT MAX(rowid) FROM orchestration_command_receipts), 0)",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .map_err(sql_error("compact event store"))
            })
            .await
            .map_err(wrap())?;

        let mut report = CompactionReport::default();
        // Pages run newest first, so the first thread-state event or entity key
        // seen is the one to keep.
        let mut retained_thread_ids: HashSet<String> = HashSet::new();
        let mut retained_entity_keys: HashSet<String> = HashSet::new();

        while through_sequence > 0 {
            let through = through_sequence;
            let rows = self
                .db()
                .read(move |conn| discover_event_page(conn, through))
                .await
                .map_err(wrap())?;

            let mut obsolete: Vec<i64> = Vec::new();
            for row in &rows {
                if row.imported_legacy_thread == 1 {
                    obsolete.push(row.sequence);
                } else if row.application_event_version == 2 {
                    if row.aggregate_kind == "thread"
                        && SUPERSEDABLE_THREAD_EVENT_TYPES.contains(&row.event_type.as_str())
                    {
                        if !retained_thread_ids.insert(row.stream_id.clone()) {
                            obsolete.push(row.sequence);
                        }
                    } else if row.event_type == "message.updated"
                        || row.event_type == "node.updated"
                    {
                        // Same bytes as the upstream `encodeEntityKey` JSON tuple.
                        let key = serde_json::to_string(&(
                            row.event_type.as_str(),
                            row.stream_id.as_str(),
                            row.entity_id.as_deref(),
                        ))
                        .map_err(|error| decode_error("compact event store", error))
                        .map_err(wrap())?;
                        if !retained_entity_keys.insert(key) {
                            obsolete.push(row.sequence);
                        }
                    }
                }
            }
            if !obsolete.is_empty() {
                let deleted = obsolete.len() as u64;
                self.db()
                    .write(move |tx| {
                        let sql = format!(
                            "DELETE FROM orchestration_events WHERE sequence IN ({})",
                            placeholders(obsolete.len())
                        );
                        tx.execute(&sql, rusqlite::params_from_iter(obsolete.iter()))
                            .map_err(sql_error("compact event store"))?;
                        Ok(())
                    })
                    .await
                    .map_err(wrap())?;
                report.deleted_event_count += deleted;
            }

            through_sequence = rows.last().map_or(1, |row| row.sequence) - 1;
            if (rows.len() as i64) < COMPACTION_PAGE_SIZE {
                break;
            }
        }

        // Legacy receipts are removable only after their thread's v1 import
        // finishes. Page by rowid before checking eligibility, as above.
        while through_receipt_row_id > 0 {
            let through = through_receipt_row_id;
            let rows = self
                .db()
                .read(move |conn| discover_receipt_page(conn, through))
                .await
                .map_err(wrap())?;

            let obsolete: Vec<String> = rows
                .iter()
                .filter(|row| row.imported_legacy_thread == 1)
                .map(|row| row.command_id.clone())
                .collect();
            if !obsolete.is_empty() {
                let deleted = obsolete.len() as u64;
                self.db()
                    .write(move |tx| {
                        let sql = format!(
                            "DELETE FROM orchestration_command_receipts WHERE command_id IN ({})",
                            placeholders(obsolete.len())
                        );
                        tx.execute(&sql, rusqlite::params_from_iter(obsolete.iter()))
                            .map_err(sql_error("compact event store"))?;
                        Ok(())
                    })
                    .await
                    .map_err(wrap())?;
                report.deleted_receipt_count += deleted;
            }

            through_receipt_row_id = rows.last().map_or(1, |row| row.row_id) - 1;
            if (rows.len() as i64) < COMPACTION_PAGE_SIZE {
                break;
            }
        }

        report.reclaimable_bytes = self
            .db()
            .read(|conn| {
                let freelist: i64 = conn
                    .query_row("PRAGMA freelist_count", [], |row| row.get(0))
                    .map_err(sql_error("compact event store"))?;
                let page_size: i64 = conn
                    .query_row("PRAGMA page_size", [], |row| row.get(0))
                    .map_err(sql_error("compact event store"))?;
                Ok(freelist * page_size)
            })
            .await
            .map_err(wrap())?;

        Ok(report)
    }
}

/// The `verify` body. Takes a read transaction so every statement sees one snapshot.
fn verify_in(conn: &Connection) -> Result<ProjectionVerification, PersistenceError> {
    let operation = "ProjectionMaintenance.verify";
    let tx = conn.unchecked_transaction().map_err(sql_error(operation))?;

    let expected_ids = query_ids(
        &tx,
        operation,
        "SELECT DISTINCT stream_id AS thread_id
         FROM orchestration_events INDEXED BY orchestration_events_v2_created_threads_idx
         WHERE application_event_version = 2
           AND aggregate_kind = 'thread'
           AND event_type = 'thread.created'
         ORDER BY stream_id ASC",
    )?;
    let actual_ids = query_ids(
        &tx,
        operation,
        "SELECT thread_id FROM orchestration_v2_projection_threads ORDER BY thread_id ASC",
    )?;
    let expected_set: HashSet<&String> = expected_ids.iter().collect();
    let actual_set: HashSet<&String> = actual_ids.iter().collect();
    let missing_thread_ids = expected_ids
        .iter()
        .filter(|id| !actual_set.contains(id))
        .map(|id| thread_id(id))
        .collect::<Result<Vec<_>, _>>()?;
    let unexpected_thread_ids = actual_ids
        .iter()
        .filter(|id| !expected_set.contains(id))
        .map(|id| thread_id(id))
        .collect::<Result<Vec<_>, _>>()?;

    let unreadable_thread_ids = read_unreadable_thread_ids(&tx).map_err(PersistenceError::from)?;

    let (schema_version, projection_sequence) = tx
        .query_row(
            "SELECT schema_version, last_sequence
             FROM orchestration_v2_projection_metadata
             WHERE projection_name = ?1
             LIMIT 1",
            params![PROJECTION_NAME],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(sql_error(operation))?
        .unwrap_or((0, 0));
    let expected_sequence: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM orchestration_events
             WHERE application_event_version = 2 AND aggregate_kind = 'thread'",
            [],
            |row| row.get(0),
        )
        .map_err(sql_error(operation))?;

    let valid = schema_version == ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION as i64
        && projection_sequence == expected_sequence
        && missing_thread_ids.is_empty()
        && unexpected_thread_ids.is_empty()
        && unreadable_thread_ids.is_empty();

    Ok(ProjectionVerification {
        valid,
        schema_version,
        expected_sequence,
        projection_sequence,
        unreadable_thread_ids,
        missing_thread_ids,
        unexpected_thread_ids,
    })
}

/// The `rebuild` write body: clear projections, replay events up to the
/// current high-water mark, then stamp the metadata row. One transaction, so a
/// live append cannot interleave with the replay.
fn rebuild_in(tx: &Transaction<'_>) -> Result<(), PersistenceError> {
    let operation = "ProjectionMaintenance.rebuild";
    let through_sequence: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM orchestration_events
             WHERE application_event_version = 2 AND aggregate_kind = 'thread'",
            [],
            |row| row.get(0),
        )
        .map_err(sql_error(operation))?;

    for table in REBUILD_TABLES {
        tx.execute(&format!("DELETE FROM {table}"), [])
            .map_err(sql_error(operation))?;
    }

    let mut last_sequence = 0_i64;
    loop {
        let page = read_replay_page(tx, last_sequence, through_sequence)?;
        let page_len = page.len() as i64;
        for (sequence, event) in page {
            apply_event(tx, &event)?;
            if let OrchestrationV2DomainEvent::TurnItemUpdated { payload, .. } = &event {
                let common = turn_item::common(payload);
                tx.execute(
                    "INSERT INTO orchestration_v2_turn_item_positions (
                       thread_id,
                       turn_item_id,
                       ordinal
                     )
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(thread_id, turn_item_id) DO UPDATE SET
                       ordinal = excluded.ordinal",
                    params![
                        event_thread_id(&event).as_str(),
                        common.id.as_str(),
                        common.ordinal as i64,
                    ],
                )
                .map_err(sql_error(operation))?;
            }
            last_sequence = sequence;
        }
        if page_len < REBUILD_PAGE_SIZE {
            break;
        }
    }

    tx.execute(
        "INSERT INTO orchestration_v2_projection_metadata (
           projection_name,
           schema_version,
           last_sequence,
           updated_at
         )
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(projection_name) DO UPDATE SET
           schema_version = excluded.schema_version,
           last_sequence = excluded.last_sequence,
           updated_at = excluded.updated_at",
        params![
            PROJECTION_NAME,
            ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION as i64,
            last_sequence,
            utc_millis_now(),
        ],
    )
    .map_err(sql_error(operation))?;
    Ok(())
}

/// One replay page: v2 thread events after `after`, through `through`, in
/// sequence order, decoded into domain events.
fn read_replay_page(
    tx: &Transaction<'_>,
    after: i64,
    through: i64,
) -> Result<Vec<(i64, OrchestrationV2DomainEvent)>, PersistenceError> {
    let operation = "ProjectionMaintenance.rebuild";
    let sql = format!(
        "SELECT orchestration_events.sequence,
                orchestration_events.event_id,
                orchestration_events.stream_id,
                orchestration_events.event_type,
                orchestration_events.occurred_at,
                {RESTORED_PAYLOAD} AS payload_json,
                orchestration_events.metadata_json
         FROM orchestration_events
         WHERE application_event_version = 2
           AND aggregate_kind = 'thread'
           AND sequence > ?1
           AND sequence <= ?2
         ORDER BY sequence ASC
         LIMIT ?3"
    );
    let mut statement = tx.prepare(&sql).map_err(sql_error(operation))?;
    let rows = statement
        .query_map(params![after, through, REBUILD_PAGE_SIZE], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(sql_error(operation))?;

    let mut page = Vec::new();
    for row in rows {
        let (sequence, event_id, stream_id, event_type, occurred_at, payload_json, metadata_json) =
            row.map_err(sql_error(operation))?;
        let payload: serde_json::Value =
            serde_json::from_str(&payload_json).map_err(|error| decode_error(operation, error))?;
        let metadata: serde_json::Value =
            serde_json::from_str(&metadata_json).map_err(|error| decode_error(operation, error))?;
        // The envelope the `OrchestrationV2DomainEvent` serde shape expects.
        // Run, node, driver, provider instance, and raw event ids live in the
        // metadata column, not the payload.
        let mut envelope = serde_json::Map::new();
        envelope.insert("type".into(), event_type.into());
        envelope.insert("id".into(), event_id.into());
        envelope.insert("threadId".into(), stream_id.into());
        envelope.insert("occurredAt".into(), occurred_at.into());
        envelope.insert("payload".into(), payload);
        for key in [
            "runId",
            "nodeId",
            "driver",
            "providerInstanceId",
            "rawEventId",
        ] {
            if let Some(value) = metadata.get(key) {
                envelope.insert(key.into(), value.clone());
            }
        }
        let event: OrchestrationV2DomainEvent =
            serde_json::from_value(serde_json::Value::Object(envelope))
                .map_err(|error| decode_error(operation, error))?;
        page.push((sequence, event));
    }
    Ok(page)
}

struct EventScanRow {
    sequence: i64,
    application_event_version: i64,
    aggregate_kind: String,
    stream_id: String,
    event_type: String,
    entity_id: Option<String>,
    imported_legacy_thread: i64,
}

/// One compaction discovery page, newest first, at or below `through`.
fn discover_event_page(
    conn: &Connection,
    through: i64,
) -> Result<Vec<EventScanRow>, PersistenceError> {
    let operation = "ProjectionMaintenance.compactEventStore";
    let mut statement = conn
        .prepare(
            "SELECT
               event.sequence,
               event.application_event_version,
               event.aggregate_kind,
               event.stream_id,
               event.event_type,
               CASE
                 WHEN event.application_event_version = 2
                   AND event.event_type IN ('message.updated', 'node.updated')
                 THEN json_extract(event.payload_json, '$.id')
                 ELSE NULL
               END AS entity_id,
               CASE
                 WHEN event.application_event_version = 1 AND event.aggregate_kind = 'thread'
                 THEN EXISTS (
                   SELECT 1 FROM orchestration_v2_legacy_imports AS legacy_import
                   WHERE legacy_import.thread_id = event.stream_id
                     AND legacy_import.transcript_imported_at IS NOT NULL
                 )
                 ELSE 0
               END AS imported_legacy_thread
             FROM orchestration_events AS event
             WHERE event.sequence <= ?1
             ORDER BY event.sequence DESC
             LIMIT ?2",
        )
        .map_err(sql_error(operation))?;
    let rows = statement
        .query_map(params![through, COMPACTION_PAGE_SIZE], |row| {
            Ok(EventScanRow {
                sequence: row.get(0)?,
                application_event_version: row.get(1)?,
                aggregate_kind: row.get(2)?,
                stream_id: row.get(3)?,
                event_type: row.get(4)?,
                entity_id: row.get(5)?,
                imported_legacy_thread: row.get(6)?,
            })
        })
        .map_err(sql_error(operation))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(sql_error(operation))
}

struct ReceiptScanRow {
    row_id: i64,
    command_id: String,
    imported_legacy_thread: i64,
}

/// One compaction discovery page of receipts, newest rowid first, at or below `through`.
fn discover_receipt_page(
    conn: &Connection,
    through: i64,
) -> Result<Vec<ReceiptScanRow>, PersistenceError> {
    let operation = "ProjectionMaintenance.compactEventStore";
    let mut statement = conn
        .prepare(
            "SELECT
               receipt.rowid AS row_id,
               receipt.command_id,
               CASE
                 WHEN receipt.command_type = 'legacy' AND receipt.aggregate_kind = 'thread'
                 THEN EXISTS (
                   SELECT 1 FROM orchestration_v2_legacy_imports AS legacy_import
                   WHERE legacy_import.thread_id = receipt.aggregate_id
                     AND legacy_import.transcript_imported_at IS NOT NULL
                 )
                 ELSE 0
               END AS imported_legacy_thread
             FROM orchestration_command_receipts AS receipt
             WHERE receipt.rowid <= ?1
             ORDER BY receipt.rowid DESC
             LIMIT ?2",
        )
        .map_err(sql_error(operation))?;
    let rows = statement
        .query_map(params![through, COMPACTION_PAGE_SIZE], |row| {
            Ok(ReceiptScanRow {
                row_id: row.get(0)?,
                command_id: row.get(1)?,
                imported_legacy_thread: row.get(2)?,
            })
        })
        .map_err(sql_error(operation))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(sql_error(operation))
}

fn query_ids(
    conn: &Connection,
    operation: &'static str,
    sql: &str,
) -> Result<Vec<String>, PersistenceError> {
    let mut statement = conn.prepare(sql).map_err(sql_error(operation))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sql_error(operation))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(sql_error(operation))
}

fn thread_id(value: &str) -> Result<ThreadId, PersistenceError> {
    ThreadId::new(value).map_err(|error| decode_error("ProjectionMaintenance.verify", error))
}

fn placeholders(count: usize) -> String {
    vec!["?"; count].join(", ")
}

fn sql_error(operation: &'static str) -> impl Fn(rusqlite::Error) -> PersistenceError {
    move |error| PersistenceError::sql(operation, &error)
}

fn decode_error(operation: &'static str, error: impl std::fmt::Display) -> PersistenceError {
    PersistenceError::Decode(PersistenceDecodeError::new(operation, error.to_string()))
}

#[cfg(test)]
#[path = "maintenance_tests.rs"]
mod maintenance_tests;
