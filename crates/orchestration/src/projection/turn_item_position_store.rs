//! Port of upstream/apps/server/src/orchestration-v2/TurnItemPositionStore.ts

use rusqlite::{OptionalExtension, Transaction, params};
use turin_contracts::orchestration_v2::OrchestrationV2TurnItem;

use crate::projection::turn_item;
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

/// `normalize`: the item with its allocated position.
pub fn normalize_in_tx(
    tx: &Transaction<'_>,
    mut item: OrchestrationV2TurnItem,
    run_ordinal: Option<u64>,
) -> Result<OrchestrationV2TurnItem, PersistenceError> {
    let common = turn_item::common(&item);
    let ordinal = allocate_in_tx(tx, common.thread_id, common.id, common.run_id, run_ordinal)?;
    if common.ordinal != ordinal {
        turn_item::set_ordinal(&mut item, ordinal);
    }
    Ok(item)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::test_support::*;
    use serde_json::json;

    async fn alloc(
        store: &crate::projection::store::ProjectionStore,
        thread: &str,
        item: &str,
        run: Option<&str>,
    ) -> u64 {
        allocate(
            store.db(),
            tid(thread),
            TurnItemId::new(item).unwrap(),
            run.map(|run| RunId::new(run).unwrap()),
            None,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn positions_grow_inside_the_run_block_and_repeat_for_the_same_item() {
        let store = memory_store().await;
        apply_all(
            &store,
            vec![
                event("thread.created", "thread:a", T0, thread("thread:a")),
                event(
                    "run.created",
                    "thread:a",
                    T0,
                    run("thread:a", "run:1", 1, "running"),
                ),
                event(
                    "run.created",
                    "thread:a",
                    T0,
                    run("thread:a", "run:2", 2, "running"),
                ),
            ],
        )
        .await;
        assert_eq!(
            alloc(&store, "thread:a", "i1", Some("run:1")).await,
            1_000_001
        );
        assert_eq!(
            alloc(&store, "thread:a", "i2", Some("run:1")).await,
            1_000_002
        );
        assert_eq!(
            alloc(&store, "thread:a", "i3", Some("run:2")).await,
            2_000_001
        );
        assert_eq!(alloc(&store, "thread:a", "r0", None).await, 1);
        // The first assignment sticks.
        assert_eq!(
            alloc(&store, "thread:a", "i1", Some("run:2")).await,
            1_000_001
        );
        // Another thread numbers independently.
        assert_eq!(alloc(&store, "thread:b", "i1", None).await, 1);
    }

    #[tokio::test]
    async fn normalize_rewrites_only_the_ordinal() {
        let store = memory_store().await;
        apply_all(
            &store,
            vec![
                event("thread.created", "thread:a", T0, thread("thread:a")),
                event(
                    "run.created",
                    "thread:a",
                    T0,
                    run("thread:a", "run:1", 1, "running"),
                ),
            ],
        )
        .await;
        let item: OrchestrationV2TurnItem =
            serde_json::from_value(assistant_item("thread:a", "i1", "run:1", 7, "hi")).unwrap();
        let expected = item.clone();
        let normalized = store
            .db()
            .write(move |tx| normalize_in_tx(tx, item, None))
            .await
            .unwrap();
        assert_eq!(turn_item::common(&normalized).ordinal, 1_000_001);
        let mut restored = normalized.clone();
        turn_item::set_ordinal(&mut restored, 7);
        assert_eq!(restored, expected);
        let _ = json!(null);
    }
}
