//! Port of upstream/apps/server/src/orchestration-v2/TurnItemPositionStore.ts

use rusqlite::{OptionalExtension, Transaction, params};
use turin_contracts::orchestration_v2::OrchestrationV2TurnItem;
use turin_contracts::{RunId, ThreadId, TurnItemId};
use turin_persistence::{Db, PersistenceError};

/// Call inside the event transaction. Repeated events keep the first assigned position.
pub fn allocate_in_tx(
    tx: &Transaction<'_>,
    thread_id: &ThreadId,
    turn_item_id: &TurnItemId,
    run_id: Option<&RunId>,
    supplied_run_ordinal: Option<u64>,
) -> Result<u64, PersistenceError> {
    let existing: Option<i64> = tx.query_row("SELECT ordinal FROM orchestration_v2_turn_item_positions WHERE thread_id=?1 AND turn_item_id=?2 LIMIT 1", params![thread_id.as_str(), turn_item_id.as_str()], |row| row.get(0)).optional().map_err(|e| PersistenceError::sql("TurnItemPositionStore.allocate", &e))?;
    if let Some(existing) = existing {
        return Ok(existing as u64);
    }
    let run_ordinal: Option<i64> = if let Some(supplied) = supplied_run_ordinal {
        Some(supplied as i64)
    } else if let Some(run_id) = run_id {
        tx.query_row("SELECT ordinal FROM orchestration_v2_projection_runs WHERE thread_id=?1 AND run_id=?2 LIMIT 1", params![thread_id.as_str(), run_id.as_str()], |row| row.get(0)).optional().map_err(|e| PersistenceError::sql("TurnItemPositionStore.allocate", &e))?
    } else {
        None
    };
    let lower = run_ordinal.unwrap_or(0) * 1_000_000;
    let upper = lower + 999_999;
    tx.execute("INSERT INTO orchestration_v2_turn_item_positions (thread_id, turn_item_id, ordinal) SELECT ?1, ?2, COALESCE(MAX(ordinal), ?3) + 1 FROM orchestration_v2_turn_item_positions WHERE thread_id=?1 AND ordinal >= ?3 AND ordinal <= ?4 ON CONFLICT(thread_id, turn_item_id) DO NOTHING", params![thread_id.as_str(), turn_item_id.as_str(), lower, upper]).map_err(|e| PersistenceError::sql("TurnItemPositionStore.allocate", &e))?;
    let ordinal: i64 = tx.query_row("SELECT ordinal FROM orchestration_v2_turn_item_positions WHERE thread_id=?1 AND turn_item_id=?2 LIMIT 1", params![thread_id.as_str(), turn_item_id.as_str()], |row| row.get(0)).map_err(|e| PersistenceError::sql("TurnItemPositionStore.allocate", &e))?;
    Ok(ordinal as u64)
}

pub async fn allocate(
    db: &Db,
    thread_id: ThreadId,
    turn_item_id: TurnItemId,
    run_id: Option<RunId>,
    run_ordinal: Option<u64>,
) -> Result<u64, PersistenceError> {
    db.write(move |tx| allocate_in_tx(tx, &thread_id, &turn_item_id, run_id.as_ref(), run_ordinal))
        .await
}

/// The turn-item union has shared fields on every arm; JSON roundtrip keeps
/// the arm-specific fields untouched while changing only the position.
pub fn with_ordinal(
    item: OrchestrationV2TurnItem,
    ordinal: u64,
) -> Result<OrchestrationV2TurnItem, serde_json::Error> {
    let mut value = serde_json::to_value(item)?;
    value["ordinal"] = serde_json::json!(ordinal);
    serde_json::from_value(value)
}
