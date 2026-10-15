//! Port of `ProjectionStore.getUnreadableThreadIds`: scans every projection
//! table and reports the threads whose rows no longer decode, plus the fork
//! children whose source is unreadable or missing. Startup verification uses
//! it; it never loads history.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;
use serde::de::DeserializeOwned;
use turin_contracts::ThreadId;
use turin_contracts::orchestration_v2::{
    OrchestrationV2AppThread, OrchestrationV2Checkpoint, OrchestrationV2CheckpointScope,
    OrchestrationV2ContextHandoff, OrchestrationV2ContextTransfer,
    OrchestrationV2ConversationMessage, OrchestrationV2ExecutionNode, OrchestrationV2ForkSource,
    OrchestrationV2PlanArtifact, OrchestrationV2ProviderSession, OrchestrationV2ProviderThread,
    OrchestrationV2ProviderTurn, OrchestrationV2Run, OrchestrationV2RunAttempt,
    OrchestrationV2RuntimeRequest, OrchestrationV2Subagent, OrchestrationV2TurnItem,
};

use crate::projection::error::ProjectionStoreError;
use crate::projection::sql::query_map;

/// One projection table: how to find its rows' thread ids and whether a payload decodes.
struct ProjectionTable {
    name: &'static str,
    id_column: &'static str,
    /// `source_thread_id` for context transfers, `thread_id` elsewhere.
    thread_column: &'static str,
    /// Only context transfers have a second thread (the target).
    has_target_thread: bool,
    decodes: fn(&str) -> bool,
}

fn decodes<T: DeserializeOwned>(payload_json: &str) -> bool {
    serde_json::from_str::<T>(payload_json).is_ok()
}

const TABLES: &[ProjectionTable] = &[
    ProjectionTable {
        name: "threads",
        id_column: "thread_id",
        thread_column: "thread_id",
        has_target_thread: false,
        decodes: decodes::<OrchestrationV2AppThread>,
    },
    ProjectionTable {
        name: "runs",
        id_column: "run_id",
        thread_column: "thread_id",
        has_target_thread: false,
        decodes: decodes::<OrchestrationV2Run>,
    },
    ProjectionTable {
        name: "run_attempts",
        id_column: "attempt_id",
        thread_column: "thread_id",
        has_target_thread: false,
        decodes: decodes::<OrchestrationV2RunAttempt>,
    },
    ProjectionTable {
        name: "nodes",
        id_column: "node_id",
        thread_column: "thread_id",
        has_target_thread: false,
        decodes: decodes::<OrchestrationV2ExecutionNode>,
    },
    ProjectionTable {
        name: "subagents",
        id_column: "subagent_id",
        thread_column: "thread_id",
        has_target_thread: false,
        decodes: decodes::<OrchestrationV2Subagent>,
    },
    ProjectionTable {
        name: "provider_sessions",
        id_column: "provider_session_id",
        thread_column: "thread_id",
        has_target_thread: false,
        decodes: decodes::<OrchestrationV2ProviderSession>,
    },
    ProjectionTable {
        name: "provider_threads",
        id_column: "provider_thread_id",
        thread_column: "thread_id",
        has_target_thread: false,
        decodes: decodes::<OrchestrationV2ProviderThread>,
    },
    ProjectionTable {
        name: "provider_turns",
        id_column: "provider_turn_id",
        thread_column: "thread_id",
        has_target_thread: false,
        decodes: decodes::<OrchestrationV2ProviderTurn>,
    },
    ProjectionTable {
        name: "runtime_requests",
        id_column: "runtime_request_id",
        thread_column: "thread_id",
        has_target_thread: false,
        decodes: decodes::<OrchestrationV2RuntimeRequest>,
    },
    ProjectionTable {
        name: "messages",
        id_column: "message_id",
        thread_column: "thread_id",
        has_target_thread: false,
        decodes: decodes::<OrchestrationV2ConversationMessage>,
    },
    ProjectionTable {
        name: "plans",
        id_column: "plan_id",
        thread_column: "thread_id",
        has_target_thread: false,
        decodes: decodes::<OrchestrationV2PlanArtifact>,
    },
    ProjectionTable {
        name: "turn_items",
        id_column: "turn_item_id",
        thread_column: "thread_id",
        has_target_thread: false,
        decodes: decodes::<OrchestrationV2TurnItem>,
    },
    ProjectionTable {
        name: "checkpoint_scopes",
        id_column: "scope_id",
        thread_column: "thread_id",
        has_target_thread: false,
        decodes: decodes::<OrchestrationV2CheckpointScope>,
    },
    ProjectionTable {
        name: "checkpoints",
        id_column: "checkpoint_id",
        thread_column: "thread_id",
        has_target_thread: false,
        decodes: decodes::<OrchestrationV2Checkpoint>,
    },
    ProjectionTable {
        name: "context_handoffs",
        id_column: "context_handoff_id",
        thread_column: "thread_id",
        has_target_thread: false,
        decodes: decodes::<OrchestrationV2ContextHandoff>,
    },
    ProjectionTable {
        name: "context_transfers",
        id_column: "context_transfer_id",
        thread_column: "source_thread_id",
        has_target_thread: true,
        decodes: decodes::<OrchestrationV2ContextTransfer>,
    },
];

/// One row of a projection scan: the rowid cursor plus the columns the
/// unreadable check attributes a failure to.
struct ScanRow {
    row_id: i64,
    entity_id: String,
    thread_id: Option<String>,
    target_thread_id: Option<String>,
    payload_json: String,
}

/// Rows per page, matching the upstream scan.
const PAGE_SIZE: i64 = 500;

/// Threads whose projection is unreadable, sorted. Fork children of an
/// unreadable source are included, so a broken ancestor cannot be read
/// through its descendant. Only threads that have a `threads` row are reported.
pub(crate) fn read_unreadable_thread_ids(
    conn: &Connection,
) -> Result<Vec<ThreadId>, ProjectionStoreError> {
    let setup = ProjectionStoreError::setup;
    let mut actual_thread_ids: HashSet<String> = HashSet::new();
    let mut unreadable: HashSet<String> = HashSet::new();
    // Source thread id -> fork children, recorded only for threads that decode.
    let mut fork_children: HashMap<String, Vec<String>> = HashMap::new();

    for table in TABLES {
        let target_select = if table.has_target_thread {
            "target_thread_id"
        } else {
            "NULL"
        };
        let sql = format!(
            "SELECT rowid, {id} AS entity_id, {thread} AS thread_id, {target} AS target_thread_id, payload_json \
             FROM orchestration_v2_projection_{name} WHERE rowid > ?1 ORDER BY rowid ASC LIMIT ?2",
            id = table.id_column,
            thread = table.thread_column,
            target = target_select,
            name = table.name,
        );
        let mut after_row_id = 0_i64;
        loop {
            let rows: Vec<ScanRow> = query_map(
                conn,
                &sql,
                rusqlite::params![after_row_id, PAGE_SIZE],
                |row| {
                    Ok(ScanRow {
                        row_id: row.get(0)?,
                        entity_id: row.get(1)?,
                        thread_id: row.get(2)?,
                        target_thread_id: row.get(3)?,
                        payload_json: row.get(4)?,
                    })
                },
            )
            .map_err(setup)?;

            for row in &rows {
                if table.name == "threads" {
                    actual_thread_ids.insert(row.entity_id.clone());
                    if let Ok(thread) =
                        serde_json::from_str::<OrchestrationV2AppThread>(&row.payload_json)
                    {
                        if let Some(OrchestrationV2ForkSource::Run { thread_id, .. }) =
                            thread.forked_from
                        {
                            fork_children
                                .entry(thread_id.as_str().to_owned())
                                .or_default()
                                .push(row.entity_id.clone());
                        }
                        continue;
                    }
                } else if (table.decodes)(&row.payload_json) {
                    continue;
                }

                if table.name == "provider_sessions" {
                    let bindings = query_map(
                        conn,
                        "SELECT thread_id FROM orchestration_v2_projection_provider_session_bindings \
                         WHERE provider_session_id = ?1",
                        rusqlite::params![row.entity_id],
                        |row| row.get::<_, String>(0),
                    )
                    .map_err(setup)?;
                    unreadable.extend(bindings);
                } else {
                    unreadable.extend(row.thread_id.iter().cloned());
                    unreadable.extend(row.target_thread_id.iter().cloned());
                }

                if table.name == "provider_threads" {
                    let owners = query_map(
                        conn,
                        "SELECT thread_id FROM orchestration_v2_projection_nodes \
                         WHERE node_id = ( \
                             SELECT owner_node_id FROM orchestration_v2_projection_provider_threads \
                             WHERE provider_thread_id = ?1 \
                         ) \
                         UNION \
                         SELECT thread_id FROM orchestration_v2_projection_subagents \
                         WHERE provider_thread_id = ?1",
                        rusqlite::params![row.entity_id],
                        |row| row.get::<_, String>(0),
                    )
                    .map_err(setup)?;
                    unreadable.extend(owners);
                }
            }

            if (rows.len() as i64) < PAGE_SIZE {
                break;
            }
            after_row_id = rows.last().map_or(after_row_id, |row| row.row_id);
        }
    }

    // A full fork projection also reads its source. Propagate decode failures
    // and missing sources through the same ancestry without loading history.
    let mut pending: Vec<String> = unreadable.iter().cloned().collect();
    pending.extend(
        fork_children
            .keys()
            .filter(|source| !actual_thread_ids.contains(*source))
            .cloned(),
    );
    let mut index = 0;
    while index < pending.len() {
        let current = pending[index].clone();
        index += 1;
        for child in fork_children.get(&current).into_iter().flatten() {
            if unreadable.insert(child.clone()) {
                pending.push(child.clone());
            }
        }
    }

    let mut ids: Vec<String> = unreadable
        .into_iter()
        .filter(|thread_id| actual_thread_ids.contains(thread_id))
        .collect();
    ids.sort();
    ids.iter()
        .map(|id| ThreadId::new(id).map_err(|error| setup(Box::new(error))))
        .collect()
}
