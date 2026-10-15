//! Port of upstream/apps/server/src/orchestration-v2/ProjectionStore.ts
//!
//! Pipeline seam: call `ProjectionStore::apply_in_tx(tx, event)` after appending
//! the event and before committing that same `Db::write` transaction. This
//! method never publishes. The pipeline publishes the committed event only
//! after `Db::write` succeeds. UI reads use `get_thread_projection` on the
//! read connection and therefore cannot block the writer or GPUI thread.

use rusqlite::types::Value as SqlValue;
use rusqlite::{OptionalExtension, Transaction, params, params_from_iter};
use serde::de::DeserializeOwned;
use serde_json::Value;
use turin_contracts::orchestration_v2::*;
use turin_contracts::{ThreadId, TurnItemId};
use turin_persistence::{Db, PersistenceDecodeError, PersistenceError};

pub const ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION: i64 = 2;

#[derive(Clone)]
pub struct ProjectionStore {
    db: Db,
}

impl ProjectionStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn apply(&self, event: OrchestrationV2DomainEvent) -> Result<(), PersistenceError> {
        self.db.write(move |tx| Self::apply_in_tx(tx, &event)).await
    }

    /// Mutates projection rows in the caller's event transaction. No async work here.
    pub fn apply_in_tx(
        tx: &Transaction<'_>,
        event: &OrchestrationV2DomainEvent,
    ) -> Result<(), PersistenceError> {
        let value =
            serde_json::to_value(event).map_err(|e| decode_error("ProjectionStore.apply", e))?;
        let kind = value["type"]
            .as_str()
            .ok_or_else(|| invalid("ProjectionStore.apply", "event type missing"))?;
        let thread_id = value["threadId"]
            .as_str()
            .ok_or_else(|| invalid("ProjectionStore.apply", "thread id missing"))?;
        let occurred_at = value["occurredAt"]
            .as_str()
            .ok_or_else(|| invalid("ProjectionStore.apply", "occurredAt missing"))?;
        let mut payload = value["payload"].clone();

        if kind == "thread.pull-request-link-updated" {
            // This patch is deliberately handled separately from whole-thread
            // lifecycle events; a stale snapshot may not erase a newer link.
            return apply_pull_request_link_update(tx, thread_id, &payload, occurred_at);
        }
        if kind == "run.background-work-cancelled" {
            let work = serde_json::to_string(&payload["restartCancelledBackgroundWork"])
                .map_err(|e| decode_error("ProjectionStore.apply", e))?;
            tx.execute("UPDATE orchestration_v2_projection_runs SET payload_json=json_set(payload_json, '$.restartCancelledBackgroundWork', json(?1)) WHERE run_id=?2 AND thread_id=?3", params![work, as_text(&payload["runId"]), thread_id]).map_err(|e| PersistenceError::sql("ProjectionStore.apply", &e))?;
        } else if kind == "provider-session.detached" {
            tx.execute("DELETE FROM orchestration_v2_projection_provider_session_bindings WHERE provider_session_id=?1 AND thread_id=?2", params![as_text(&payload["providerSessionId"]), thread_id]).map_err(|e| PersistenceError::sql("ProjectionStore.apply", &e))?;
        } else if kind != "checkpoint.rollback-requested" {
            let descriptor = descriptor(kind).ok_or_else(|| {
                invalid(
                    "ProjectionStore.apply",
                    format!("unhandled event type: {kind}"),
                )
            })?;
            if kind == "run.created" || kind == "run.updated" {
                preserve_absent(
                    tx,
                    descriptor.table,
                    descriptor.key,
                    &mut payload,
                    &["delegatedCompletion", "restartCancelledBackgroundWork"],
                )?;
            } else if kind == "subagent.updated" {
                preserve_absent(
                    tx,
                    descriptor.table,
                    descriptor.key,
                    &mut payload,
                    &["completionDelivery"],
                )?;
            } else if kind == "provider-turn.updated" {
                preserve_absent(
                    tx,
                    descriptor.table,
                    descriptor.key,
                    &mut payload,
                    &["tokenUsage"],
                )?;
            }
            upsert(tx, descriptor, &payload, thread_id)?;
            if kind == "provider-session.attached" {
                tx.execute("INSERT OR IGNORE INTO orchestration_v2_projection_provider_session_bindings (provider_session_id, thread_id) VALUES (?1, ?2)", params![as_text(&payload["id"]), thread_id]).map_err(|e| PersistenceError::sql("ProjectionStore.apply", &e))?;
            }
            if kind == "provider-thread.updated" {
                let placeholder = payload["status"] == "not_loaded"
                    && payload["firstRunOrdinal"].is_null()
                    && payload["nativeThreadRef"].is_null()
                    && payload["providerSessionId"].is_null();
                if !placeholder && let Some(app_thread_id) = payload["appThreadId"].as_str() {
                    tx.execute("UPDATE orchestration_v2_projection_threads SET active_provider_thread_id=?1, updated_at=?2, payload_json=json_set(payload_json, '$.activeProviderThreadId', ?1, '$.updatedAt', ?2) WHERE thread_id=?3", params![as_text(&payload["id"]), as_text(&payload["updatedAt"]), app_thread_id]).map_err(|e| PersistenceError::sql("ProjectionStore.apply", &e))?;
                }
            }
        }
        // A visit/read marker does not count as thread activity. Whole-thread
        // events already carry their own timestamp; all other events patch it.
        if !kind.starts_with("thread.") {
            tx.execute("UPDATE orchestration_v2_projection_threads SET updated_at=?1, payload_json=json_set(payload_json, '$.updatedAt', ?1) WHERE thread_id=?2", params![occurred_at, thread_id]).map_err(|e| PersistenceError::sql("ProjectionStore.apply", &e))?;
        }
        Ok(())
    }

    pub async fn get_thread_projection(
        &self,
        thread_id: ThreadId,
    ) -> Result<Option<OrchestrationV2ThreadProjection>, PersistenceError> {
        self.db.read(move |conn| {
            let thread_json: Option<String> = conn.query_row("SELECT payload_json FROM orchestration_v2_projection_threads WHERE thread_id=?1", [thread_id.as_str()], |row| row.get(0)).optional().map_err(|e| PersistenceError::sql("ProjectionStore.getThread", &e))?;
            let Some(thread_json) = thread_json else { return Ok(None); };
            let thread: OrchestrationV2AppThread = parse_json(&thread_json, "ProjectionStore.getThread")?;
            let tid = thread_id.as_str();
            let runs = read_rows(conn, "orchestration_v2_projection_runs", "thread_id=?1", "ordinal, run_id", tid)?;
            let attempts = read_rows(conn, "orchestration_v2_projection_run_attempts", "thread_id=?1", "run_id, attempt_ordinal", tid)?;
            let nodes = read_rows(conn, "orchestration_v2_projection_nodes", "thread_id=?1", "COALESCE(started_at,''), node_id", tid)?;
            let subagents = read_rows(conn, "orchestration_v2_projection_subagents", "thread_id=?1", "COALESCE(started_at,''), subagent_id", tid)?;
            let provider_sessions = read_rows(conn, "orchestration_v2_projection_provider_sessions AS sessions INNER JOIN orchestration_v2_projection_provider_session_bindings AS bindings ON bindings.provider_session_id=sessions.provider_session_id", "bindings.thread_id=?1", "sessions.updated_at, sessions.provider_session_id", tid)?;
            let provider_threads = read_rows(conn, "orchestration_v2_projection_provider_threads", "thread_id=?1 OR owner_node_id IN (SELECT node_id FROM orchestration_v2_projection_nodes WHERE thread_id=?1) OR provider_thread_id IN (SELECT provider_thread_id FROM orchestration_v2_projection_subagents WHERE thread_id=?1 AND provider_thread_id IS NOT NULL)", "COALESCE(first_run_ordinal,0), provider_thread_id", tid)?;
            let provider_turns = read_rows(conn, "orchestration_v2_projection_provider_turns", "thread_id=?1", "provider_thread_id, ordinal", tid)?;
            let runtime_requests = read_rows(conn, "orchestration_v2_projection_runtime_requests", "thread_id=?1", "created_at, runtime_request_id", tid)?;
            let messages = read_rows(conn, "orchestration_v2_projection_messages", "thread_id=?1", "created_at, message_id", tid)?;
            let plans = read_rows(conn, "orchestration_v2_projection_plans", "thread_id=?1", "plan_id", tid)?;
            let turn_items = read_rows(conn, "orchestration_v2_projection_turn_items", "thread_id=?1", "ordinal, turn_item_id", tid)?;
            let checkpoint_scopes = read_rows(conn, "orchestration_v2_projection_checkpoint_scopes", "thread_id=?1", "ordinal_within_parent, scope_id", tid)?;
            let checkpoints = read_rows(conn, "orchestration_v2_projection_checkpoints", "thread_id=?1", "ordinal_within_scope, checkpoint_id", tid)?;
            let context_handoffs = read_rows(conn, "orchestration_v2_projection_context_handoffs", "thread_id=?1", "updated_at, context_handoff_id", tid)?;
            let context_transfers = read_rows(conn, "orchestration_v2_projection_context_transfers", "source_thread_id=?1 OR target_thread_id=?1", "updated_at, context_transfer_id", tid)?;
            let updated_at = thread.updated_at;
            let mut projection = OrchestrationV2ThreadProjection { thread, runs, attempts, nodes, subagents, provider_sessions, provider_threads, provider_turns, runtime_requests, messages, plans, turn_items, checkpoint_scopes, checkpoints, context_handoffs, context_transfers, visible_turn_items: Vec::new(), updated_at };
            projection.visible_turn_items = local_visible_turn_items(&projection);
            Ok(Some(projection))
        }).await
    }
}

fn local_visible_turn_items(
    projection: &OrchestrationV2ThreadProjection,
) -> Vec<OrchestrationV2ProjectedTurnItem> {
    let superseded: std::collections::HashSet<_> = projection
        .attempts
        .iter()
        .filter_map(|attempt| {
            let value = serde_json::to_value(attempt).ok()?;
            if value["status"] == "superseded" {
                Some((
                    value["runId"].as_str()?.to_owned(),
                    value["rootNodeId"].as_str()?.to_owned(),
                ))
            } else {
                None
            }
        })
        .collect();
    let interrupt_runs: std::collections::HashSet<_> = projection
        .turn_items
        .iter()
        .filter_map(|item| {
            let value = serde_json::to_value(item).ok()?;
            (value["type"] == "run_interrupt_request")
                .then(|| value["runId"].as_str().map(str::to_owned))
                .flatten()
        })
        .collect();
    projection
        .turn_items
        .iter()
        .filter_map(|item| {
            let value = serde_json::to_value(item).ok()?;
            let run_id = value["runId"].as_str();
            if run_id.is_some_and(|id| {
                projection.runs.iter().any(|run| {
                    run.id.as_str() == id
                        && matches!(run.status, OrchestrationV2RunStatus::RolledBack)
                })
            }) {
                return None;
            }
            if value["type"] == "user_message"
                && value["inputIntent"] == "queued_turn"
                && run_id.is_some_and(|id| {
                    projection.runs.iter().any(|run| {
                        run.id.as_str() == id
                            && matches!(run.status, OrchestrationV2RunStatus::Cancelled)
                    })
                })
            {
                return None;
            }
            if value["type"] == "run_interrupt_result"
                && run_id
                    .zip(value["nodeId"].as_str())
                    .is_some_and(|(run, node)| {
                        superseded.contains(&(run.to_owned(), node.to_owned()))
                            && !interrupt_runs.contains(run)
                    })
            {
                return None;
            }
            let source_thread_id = ThreadId::new(value["threadId"].as_str()?).ok()?;
            let source_item_id = TurnItemId::new(value["id"].as_str()?).ok()?;
            Some(OrchestrationV2ProjectedTurnItem {
                position: 0,
                visibility: OrchestrationV2ProjectedTurnItemVisibility::Local,
                source_thread_id,
                source_item_id,
                item: item.clone(),
            })
        })
        .enumerate()
        .map(|(position, mut row)| {
            row.position = position as u64;
            row
        })
        .collect()
}

fn read_rows<T: DeserializeOwned>(
    conn: &rusqlite::Connection,
    table: &str,
    predicate: &str,
    order: &str,
    thread_id: &str,
) -> Result<Vec<T>, PersistenceError> {
    // Call sites pass only static SQL fragments.
    let sql = format!("SELECT payload_json FROM {table} WHERE {predicate} ORDER BY {order}");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| PersistenceError::sql("ProjectionStore.read", &e))?;
    let rows = stmt
        .query_map([thread_id], |row| row.get::<_, String>(0))
        .map_err(|e| PersistenceError::sql("ProjectionStore.read", &e))?;
    rows.map(|row| {
        parse_json(
            &row.map_err(|e| PersistenceError::sql("ProjectionStore.read", &e))?,
            "ProjectionStore.read",
        )
    })
    .collect()
}

fn parse_json<T: DeserializeOwned>(json: &str, operation: &str) -> Result<T, PersistenceError> {
    serde_json::from_str(json).map_err(|e| decode_error(operation, e))
}
fn decode_error(operation: &str, error: serde_json::Error) -> PersistenceError {
    PersistenceError::Decode(PersistenceDecodeError::new(operation, error.to_string()))
}
fn invalid(operation: &str, message: impl Into<String>) -> PersistenceError {
    PersistenceError::Decode(PersistenceDecodeError::new(operation, message))
}
fn as_text(value: &Value) -> Option<&str> {
    value.as_str()
}

struct Descriptor {
    table: &'static str,
    key: &'static str,
    fields: &'static [(&'static str, &'static str)],
}
// Column names and JSON paths are static, mirroring each INSERT in upstream.
fn descriptor(kind: &str) -> Option<Descriptor> {
    let (table, key, fields): (_, _, &[(&str, &str)]) = match kind {
        "thread.created"
        | "thread.archived"
        | "thread.unarchived"
        | "thread.deleted"
        | "thread.settled"
        | "thread.unsettled"
        | "thread.snoozed"
        | "thread.unsnoozed"
        | "thread.auto-settle-set"
        | "thread.pinned"
        | "thread.unpinned"
        | "thread.pin-reordered"
        | "thread.active-reordered"
        | "thread.visited"
        | "thread.marked-unread"
        | "thread.metadata-updated"
        | "thread.pull-request-synced"
        | "thread.runtime-mode-updated"
        | "thread.interaction-mode-updated"
        | "thread.model-selection-updated"
        | "thread.provider-switched" => (
            "orchestration_v2_projection_threads",
            "thread_id",
            &[
                ("thread_id", "id"),
                ("project_id", "projectId"),
                ("title", "title"),
                ("default_provider", "providerInstanceId"),
                ("provider_instance_id", "providerInstanceId"),
                ("runtime_mode", "runtimeMode"),
                ("interaction_mode", "interactionMode"),
                ("active_provider_thread_id", "activeProviderThreadId"),
                ("created_at", "createdAt"),
                ("updated_at", "updatedAt"),
                ("archived_at", "archivedAt"),
                ("deleted_at", "deletedAt"),
            ],
        ),
        "run.created" | "run.updated" => (
            "orchestration_v2_projection_runs",
            "run_id",
            &[
                ("run_id", "id"),
                ("thread_id", "threadId"),
                ("ordinal", "ordinal"),
                ("provider", "providerInstanceId"),
                ("provider_instance_id", "providerInstanceId"),
                ("provider_thread_id", "providerThreadId"),
                ("status", "status"),
                ("requested_at", "requestedAt"),
                ("completed_at", "completedAt"),
            ],
        ),
        "run-attempt.created" | "run-attempt.updated" => (
            "orchestration_v2_projection_run_attempts",
            "attempt_id",
            &[
                ("attempt_id", "id"),
                ("thread_id", "@thread"),
                ("run_id", "runId"),
                ("attempt_ordinal", "attemptOrdinal"),
                ("root_node_id", "rootNodeId"),
                ("provider", "providerInstanceId"),
                ("provider_instance_id", "providerInstanceId"),
                ("provider_thread_id", "providerThreadId"),
                ("provider_turn_id", "providerTurnId"),
                ("status", "status"),
            ],
        ),
        "node.updated" => (
            "orchestration_v2_projection_nodes",
            "node_id",
            &[
                ("node_id", "id"),
                ("thread_id", "threadId"),
                ("run_id", "runId"),
                ("parent_node_id", "parentNodeId"),
                ("root_node_id", "rootNodeId"),
                ("kind", "kind"),
                ("status", "status"),
                ("provider_thread_id", "providerThreadId"),
                ("provider_turn_id", "providerTurnId"),
                ("runtime_request_id", "runtimeRequestId"),
                ("checkpoint_scope_id", "checkpointScopeId"),
                ("started_at", "startedAt"),
                ("completed_at", "completedAt"),
            ],
        ),
        "subagent.updated" => (
            "orchestration_v2_projection_subagents",
            "subagent_id",
            &[
                ("subagent_id", "id"),
                ("thread_id", "threadId"),
                ("run_id", "runId"),
                ("parent_node_id", "parentNodeId"),
                ("provider", "providerInstanceId"),
                ("driver", "driver"),
                ("provider_instance_id", "providerInstanceId"),
                ("provider_thread_id", "providerThreadId"),
                ("child_thread_id", "childThreadId"),
                ("origin", "origin"),
                ("status", "status"),
                ("started_at", "startedAt"),
                ("completed_at", "completedAt"),
                ("updated_at", "updatedAt"),
            ],
        ),
        "provider-session.attached" | "provider-session.updated" => (
            "orchestration_v2_projection_provider_sessions",
            "provider_session_id",
            &[
                ("provider_session_id", "id"),
                ("thread_id", "@thread"),
                ("provider", "providerInstanceId"),
                ("driver", "driver"),
                ("provider_instance_id", "providerInstanceId"),
                ("status", "status"),
                ("model", "model"),
                ("updated_at", "updatedAt"),
            ],
        ),
        "provider-thread.updated" => (
            "orchestration_v2_projection_provider_threads",
            "provider_thread_id",
            &[
                ("provider_thread_id", "id"),
                ("thread_id", "appThreadId"),
                ("owner_node_id", "ownerNodeId"),
                ("provider", "providerInstanceId"),
                ("driver", "driver"),
                ("provider_instance_id", "providerInstanceId"),
                ("provider_session_id", "providerSessionId"),
                ("status", "status"),
                ("first_run_ordinal", "firstRunOrdinal"),
                ("last_run_ordinal", "lastRunOrdinal"),
                ("updated_at", "updatedAt"),
            ],
        ),
        "provider-turn.updated" => (
            "orchestration_v2_projection_provider_turns",
            "provider_turn_id",
            &[
                ("provider_turn_id", "id"),
                ("thread_id", "@thread"),
                ("provider_thread_id", "providerThreadId"),
                ("node_id", "nodeId"),
                ("run_attempt_id", "runAttemptId"),
                ("ordinal", "ordinal"),
                ("status", "status"),
                ("started_at", "startedAt"),
                ("completed_at", "completedAt"),
            ],
        ),
        "runtime-request.updated" => (
            "orchestration_v2_projection_runtime_requests",
            "runtime_request_id",
            &[
                ("runtime_request_id", "id"),
                ("thread_id", "@thread"),
                ("node_id", "nodeId"),
                ("provider_turn_id", "providerTurnId"),
                ("kind", "kind"),
                ("status", "status"),
                ("created_at", "createdAt"),
                ("resolved_at", "resolvedAt"),
            ],
        ),
        "message.updated" => (
            "orchestration_v2_projection_messages",
            "message_id",
            &[
                ("message_id", "id"),
                ("thread_id", "threadId"),
                ("run_id", "runId"),
                ("node_id", "nodeId"),
                ("role", "role"),
                ("streaming", "streaming"),
                ("created_at", "createdAt"),
                ("updated_at", "updatedAt"),
            ],
        ),
        "plan.updated" => (
            "orchestration_v2_projection_plans",
            "plan_id",
            &[
                ("plan_id", "id"),
                ("thread_id", "threadId"),
                ("run_id", "runId"),
                ("node_id", "nodeId"),
                ("kind", "kind"),
                ("status", "status"),
            ],
        ),
        "turn-item.updated" => (
            "orchestration_v2_projection_turn_items",
            "turn_item_id",
            &[
                ("turn_item_id", "id"),
                ("thread_id", "threadId"),
                ("run_id", "runId"),
                ("node_id", "nodeId"),
                ("provider_thread_id", "providerThreadId"),
                ("provider_turn_id", "providerTurnId"),
                ("parent_item_id", "parentItemId"),
                ("ordinal", "ordinal"),
                ("type", "type"),
                ("status", "status"),
                ("updated_at", "updatedAt"),
            ],
        ),
        "checkpoint-scope.created" => (
            "orchestration_v2_projection_checkpoint_scopes",
            "scope_id",
            &[
                ("scope_id", "id"),
                ("thread_id", "threadId"),
                ("run_id", "runId"),
                ("node_id", "nodeId"),
                ("parent_scope_id", "parentScopeId"),
                ("provider_thread_id", "providerThreadId"),
                ("kind", "kind"),
                ("ordinal_within_parent", "ordinalWithinParent"),
                ("advances_app_run_count", "advancesAppRunCount"),
                ("created_at", "createdAt"),
            ],
        ),
        "checkpoint.captured" => (
            "orchestration_v2_projection_checkpoints",
            "checkpoint_id",
            &[
                ("checkpoint_id", "id"),
                ("thread_id", "threadId"),
                ("scope_id", "scopeId"),
                ("run_id", "runId"),
                ("node_id", "nodeId"),
                ("parent_checkpoint_id", "parentCheckpointId"),
                ("ordinal_within_scope", "ordinalWithinScope"),
                ("app_run_ordinal", "appRunOrdinal"),
                ("status", "status"),
                ("captured_at", "capturedAt"),
            ],
        ),
        "context-handoff.updated" => (
            "orchestration_v2_projection_context_handoffs",
            "context_handoff_id",
            &[
                ("context_handoff_id", "id"),
                ("thread_id", "threadId"),
                ("target_run_id", "targetRunId"),
                ("to_provider_thread_id", "toProviderThreadId"),
                ("strategy", "strategy"),
                ("status", "status"),
                ("updated_at", "updatedAt"),
            ],
        ),
        "context-transfer.created" | "context-transfer.updated" => (
            "orchestration_v2_projection_context_transfers",
            "context_transfer_id",
            &[
                ("context_transfer_id", "id"),
                ("source_thread_id", "sourceThreadId"),
                ("target_thread_id", "targetThreadId"),
                ("target_run_id", "targetRunId"),
                ("type", "type"),
                ("status", "status"),
                ("source_provider", "sourceProviderInstanceId"),
                ("target_provider", "targetProviderInstanceId"),
                ("source_provider_instance_id", "sourceProviderInstanceId"),
                ("target_provider_instance_id", "targetProviderInstanceId"),
                ("updated_at", "updatedAt"),
            ],
        ),
        _ => return None,
    };
    Some(Descriptor { table, key, fields })
}

fn sql_value(value: &Value) -> SqlValue {
    match value {
        Value::Null => SqlValue::Null,
        Value::Bool(v) => SqlValue::Integer(i64::from(*v)),
        Value::Number(v) => v
            .as_i64()
            .map(SqlValue::Integer)
            .or_else(|| v.as_f64().map(SqlValue::Real))
            .unwrap_or(SqlValue::Null),
        Value::String(v) => SqlValue::Text(v.clone()),
        _ => SqlValue::Text(value.to_string()),
    }
}

fn upsert(
    tx: &Transaction<'_>,
    descriptor: Descriptor,
    payload: &Value,
    thread_id: &str,
) -> Result<(), PersistenceError> {
    let mut columns: Vec<&str> = descriptor
        .fields
        .iter()
        .map(|(column, _)| *column)
        .collect();
    columns.push("payload_json");
    let mut values: Vec<SqlValue> = descriptor
        .fields
        .iter()
        .map(|(_, path)| {
            if *path == "@thread" {
                SqlValue::Text(thread_id.to_owned())
            } else {
                sql_value(&payload[*path])
            }
        })
        .collect();
    values.push(SqlValue::Text(
        serde_json::to_string(payload).map_err(|e| decode_error("ProjectionStore.apply", e))?,
    ));
    let placeholders = vec!["?"; columns.len()].join(",");
    let updates = columns
        .iter()
        .filter(|column| **column != descriptor.key)
        .map(|column| format!("{column}=excluded.{column}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT({}) DO UPDATE SET {}",
        descriptor.table,
        columns.join(","),
        placeholders,
        descriptor.key,
        updates
    );
    tx.execute(&sql, params_from_iter(values))
        .map_err(|e| PersistenceError::sql("ProjectionStore.apply", &e))?;
    Ok(())
}

fn preserve_absent(
    tx: &Transaction<'_>,
    table: &str,
    key: &str,
    payload: &mut Value,
    fields: &[&str],
) -> Result<(), PersistenceError> {
    let Some(id) = payload["id"].as_str() else {
        return Ok(());
    };
    let sql = format!("SELECT payload_json FROM {table} WHERE {key}=?1");
    let old: Option<String> = tx
        .query_row(&sql, [id], |row| row.get(0))
        .optional()
        .map_err(|e| PersistenceError::sql("ProjectionStore.apply", &e))?;
    if let Some(old) = old {
        let old: Value = parse_json(&old, "ProjectionStore.apply")?;
        for field in fields {
            if payload.get(*field).is_none()
                && let Some(previous) = old.get(*field)
            {
                payload[*field] = previous.clone();
            }
        }
    }
    Ok(())
}

fn apply_pull_request_link_update(
    tx: &Transaction<'_>,
    thread_id: &str,
    payload: &Value,
    _occurred_at: &str,
) -> Result<(), PersistenceError> {
    let old: String = tx
        .query_row(
            "SELECT payload_json FROM orchestration_v2_projection_threads WHERE thread_id=?1",
            [thread_id],
            |row| row.get(0),
        )
        .map_err(|e| PersistenceError::sql("ProjectionStore.applyPullRequestLink", &e))?;
    let mut thread: Value = parse_json(&old, "ProjectionStore.applyPullRequestLink")?;
    let mut links = thread
        .get("pullRequests")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if links.is_empty()
        && let Some(legacy) = thread.get("linkedPullRequest").filter(|v| !v.is_null())
    {
        let host = legacy["url"]
            .as_str()
            .and_then(|url| url.split("//").nth(1))
            .and_then(|part| part.split('/').next())
            .unwrap_or("unknown");
        let mut link = legacy.clone();
        link["host"] = Value::String(host.to_ascii_lowercase());
        link["source"] = Value::String("manual".into());
        link["linkedAt"] = Value::String("1970-01-01T00:00:00.000Z".into());
        link["snapshot"] = Value::Null;
        link["stack"] = Value::Null;
        links.push(link);
    }
    let key = |link: &Value| {
        (
            link["host"].as_str().unwrap_or("").to_ascii_lowercase(),
            link["repository"]
                .as_str()
                .unwrap_or("")
                .to_ascii_lowercase(),
            link["number"].as_i64(),
        )
    };
    let updated_key = key(payload);
    for link in &mut links {
        if key(link) == updated_key {
            *link = payload.clone();
        }
    }
    thread["pullRequests"] = Value::Array(links);
    upsert(
        tx,
        descriptor("thread.pull-request-synced").expect("thread descriptor"),
        &thread,
        thread_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: &str = "2026-09-14T00:00:00.000Z";

    fn thread_created() -> OrchestrationV2DomainEvent {
        serde_json::from_value(json!({
            "type":"thread.created", "id":"event-1", "threadId":"thread-1", "occurredAt":NOW,
            "payload": {
                "createdBy":"user", "creationSource":"web", "id":"thread-1", "projectId":"project-1",
                "title":"Thread", "providerInstanceId":"codex",
                "modelSelection":{"instanceId":"codex", "model":"gpt-5-codex"},
                "runtimeMode":"full-access", "interactionMode":"default", "branch":null,
                "worktreePath":null, "activeProviderThreadId":null,
                "lineage":{"parentThreadId":null,"relationshipToParent":null,"rootThreadId":"thread-1"},
                "forkedFrom":null, "createdAt":NOW, "updatedAt":NOW,
                "archivedAt":null, "deletedAt":null
            }
        })).unwrap()
    }

    #[tokio::test]
    async fn applies_and_reads_thread_with_message() {
        let db = Db::open_in_memory().await.unwrap();
        let store = ProjectionStore::new(db);
        store.apply(thread_created()).await.unwrap();
        let event: OrchestrationV2DomainEvent = serde_json::from_value(json!({
            "type":"message.updated", "id":"event-2", "threadId":"thread-1", "occurredAt":NOW,
            "payload": {"id":"message-1","threadId":"thread-1","runId":null,"nodeId":null,
              "createdBy":"user","creationSource":"web","role":"user","text":"hello search",
              "attachments":[],"streaming":false,"createdAt":NOW,"updatedAt":NOW}
        }))
        .unwrap();
        store.apply(event).await.unwrap();
        let projection = store
            .get_thread_projection(ThreadId::new("thread-1").unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(projection.thread.title.as_str(), "Thread");
        assert_eq!(projection.messages.len(), 1);
        assert_eq!(projection.messages[0].text, "hello search");
    }

    #[tokio::test]
    async fn mapped_columns_exist_in_migrated_schema() {
        let db = Db::open_in_memory().await.unwrap();
        db.read(|conn| {
            for kind in [
                "thread.created",
                "run.created",
                "run-attempt.created",
                "node.updated",
                "subagent.updated",
                "provider-session.attached",
                "provider-thread.updated",
                "provider-turn.updated",
                "runtime-request.updated",
                "message.updated",
                "plan.updated",
                "turn-item.updated",
                "checkpoint-scope.created",
                "checkpoint.captured",
                "context-handoff.updated",
                "context-transfer.created",
            ] {
                let descriptor = descriptor(kind).unwrap();
                let sql = format!("PRAGMA table_info({})", descriptor.table);
                let mut statement = conn.prepare(&sql).unwrap();
                let columns: std::collections::HashSet<String> = statement
                    .query_map([], |row| row.get(1))
                    .unwrap()
                    .map(Result::unwrap)
                    .collect();
                for (column, _) in descriptor.fields {
                    assert!(columns.contains(*column), "{kind} missing {column}");
                }
                assert!(columns.contains("payload_json"));
            }
            Ok(())
        })
        .await
        .unwrap();
    }
}
