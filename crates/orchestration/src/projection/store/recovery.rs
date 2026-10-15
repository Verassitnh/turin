//! Port of `ProjectionStore.getRecoveryThreadIds`: which threads need a boot-time
//! recovery pass. The candidate queries are copied verbatim from upstream; they
//! encode rules (held queues, provider-native subagents, rolled-back runs) that
//! are easy to get subtly wrong when rewritten.

use rusqlite::Connection;
use turin_contracts::ThreadId;

use crate::projection::error::ProjectionStoreError;
use crate::projection::sql::query_map;

/// `ProjectionRecoveryKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionRecoveryKind {
    QueuedRuns,
    Runtime,
    SubagentResults,
    DelegatedCompletions,
}

impl ProjectionRecoveryKind {
    /// The candidate subquery for this kind (a single `thread_id` column).
    fn candidates(self) -> &'static str {
        match self {
            Self::QueuedRuns => {
                "SELECT thread_id FROM orchestration_v2_projection_runs
                WHERE status = 'queued'
                  AND NOT EXISTS (
                    SELECT 1 FROM orchestration_v2_projection_runs AS active
                    WHERE active.thread_id = orchestration_v2_projection_runs.thread_id
                      AND active.status IN ('preparing', 'starting', 'running', 'waiting')
                  )"
            }
            Self::DelegatedCompletions => {
                "SELECT thread_id FROM orchestration_v2_projection_runs
                WHERE CASE WHEN json_valid(payload_json)
                  THEN json_type(payload_json, '$.delegatedCompletion.delivery') = 'object'
                  ELSE 0 END"
            }
            Self::SubagentResults => {
                "SELECT child.thread_id FROM orchestration_v2_projection_threads AS child
                WHERE CASE WHEN json_valid(child.payload_json) THEN
                  json_extract(child.payload_json, '$.lineage.relationshipToParent') = 'subagent'
                  AND json_extract(child.payload_json, '$.lineage.parentThreadId') IS NOT NULL
                  AND json_extract(child.payload_json, '$.forkedFrom.type') = 'node'
                  AND (EXISTS (
                    SELECT 1 FROM orchestration_v2_projection_runs AS requested
                    JOIN orchestration_v2_projection_subagents AS task
                      ON task.subagent_id = json_extract(requested.payload_json, '$.delegatedTaskId')
                    WHERE requested.thread_id = child.thread_id
                      AND requested.status IN ('completed', 'interrupted', 'failed', 'cancelled', 'rolled_back')
                      AND task.child_thread_id = child.thread_id
                      AND json_extract(task.payload_json, '$.origin') = 'app_owned'
                      AND json_extract(task.payload_json, '$.result') IS NULL
                  ) OR (
                    -- The original task still waits for the latest unheld work.
                    (SELECT status FROM orchestration_v2_projection_runs
                     WHERE thread_id = child.thread_id
                       AND NOT (status = 'queued' AND json_extract(payload_json, '$.queueHeld') IS 1)
                     ORDER BY ordinal DESC LIMIT 1
                    ) IN ('completed', 'interrupted', 'failed', 'cancelled', 'rolled_back')
                    AND NOT EXISTS (
                    SELECT 1 FROM orchestration_v2_projection_context_transfers
                    WHERE source_thread_id = child.thread_id
                      AND target_thread_id = json_extract(child.payload_json, '$.lineage.parentThreadId')
                      AND type = 'subagent_result'
                  )))
                  ELSE 0 END"
            }
            Self::Runtime => {
                "WITH pending_provider_threads AS MATERIALIZED (
                  SELECT provider_thread_id, thread_id, owner_node_id
                  FROM orchestration_v2_projection_provider_threads
                  WHERE status = 'active'
                    OR CASE WHEN json_valid(payload_json)
                      THEN json_array_length(payload_json, '$.pendingBackgroundTasks') > 0
                      ELSE 0 END
                )
                SELECT thread_id FROM orchestration_v2_projection_runs
                WHERE status IN ('preparing', 'starting', 'running', 'waiting')
                UNION
                -- A held queue already went through recovery; rereading it on
                -- every boot costs a projection read per held thread.
                SELECT thread_id FROM orchestration_v2_projection_runs
                WHERE status = 'queued'
                  AND CASE WHEN json_valid(payload_json)
                    THEN json_extract(payload_json, '$.queueHeld') IS NOT 1
                    ELSE 1 END
                UNION
                SELECT thread_id FROM orchestration_v2_projection_runtime_requests
                WHERE status = 'pending'
                UNION
                SELECT bindings.thread_id
                FROM orchestration_v2_projection_provider_sessions AS sessions
                CROSS JOIN orchestration_v2_projection_provider_session_bindings AS bindings
                  ON bindings.provider_session_id = sessions.provider_session_id
                WHERE sessions.status NOT IN ('stopped', 'error')
                UNION
                -- A detach drops the binding but keeps the session's status.
                SELECT sessions.thread_id
                FROM orchestration_v2_projection_provider_sessions AS sessions
                WHERE sessions.status NOT IN ('stopped', 'error')
                  AND sessions.thread_id IS NOT NULL
                  AND NOT EXISTS (
                    SELECT 1 FROM orchestration_v2_projection_provider_session_bindings AS bindings
                    WHERE bindings.provider_session_id = sessions.provider_session_id
                  )
                UNION
                SELECT thread_id FROM pending_provider_threads
                UNION
                SELECT nodes.thread_id FROM pending_provider_threads
                CROSS JOIN orchestration_v2_projection_nodes AS nodes
                  ON nodes.node_id = pending_provider_threads.owner_node_id
                UNION
                SELECT subagents.thread_id FROM pending_provider_threads
                CROSS JOIN orchestration_v2_projection_subagents AS subagents
                  ON subagents.provider_thread_id = pending_provider_threads.provider_thread_id
                UNION
                SELECT subagents.child_thread_id FROM orchestration_v2_projection_subagents AS subagents
                WHERE subagents.child_thread_id IS NOT NULL
                  AND EXISTS (
                    SELECT 1 FROM orchestration_v2_projection_nodes AS node
                    WHERE node.thread_id = subagents.child_thread_id
                      AND node.run_id IS NULL
                      AND node.kind = 'root_turn'
                      AND node.status IN ('pending', 'running', 'waiting')
                  )
                UNION
                -- No native subagent outlives the provider process that ran it.
                SELECT thread_id FROM orchestration_v2_projection_subagents
                WHERE origin = 'provider_native'
                  AND status IN ('pending', 'running', 'waiting')
                UNION
                SELECT item.thread_id FROM orchestration_v2_projection_turn_items AS item
                WHERE NOT EXISTS (
                    SELECT 1 FROM orchestration_v2_projection_runs AS run
                    WHERE run.run_id = item.run_id AND run.status = 'rolled_back'
                  )
                  AND type IN ('command_execution', 'dynamic_tool', 'subagent')
                  AND status IN ('pending', 'running', 'waiting')
                UNION
                SELECT thread_id FROM orchestration_v2_effect_outbox
                WHERE status IN ('pending', 'running')"
            }
        }
    }
}

/// Threads of `kind` that are not deleted (and, for queued runs, not archived),
/// ordered by `updated_at` then id, the order the boot recovery pass runs them in.
pub(crate) fn read_recovery_thread_ids(
    conn: &Connection,
    kind: ProjectionRecoveryKind,
) -> Result<Vec<ThreadId>, ProjectionStoreError> {
    let archived_filter = if kind == ProjectionRecoveryKind::QueuedRuns {
        "AND archived_at IS NULL"
    } else {
        ""
    };
    let sql = format!(
        "SELECT thread_id FROM orchestration_v2_projection_threads
         WHERE deleted_at IS NULL
           AND thread_id IN ({candidates})
           {archived_filter}
         ORDER BY updated_at ASC, thread_id ASC",
        candidates = kind.candidates(),
    );
    let ids = query_map(conn, &sql, [], |row| row.get::<_, String>(0))
        .map_err(ProjectionStoreError::setup)?;
    ids.iter()
        .map(|id| ThreadId::new(id).map_err(|error| ProjectionStoreError::setup(Box::new(error))))
        .collect()
}
