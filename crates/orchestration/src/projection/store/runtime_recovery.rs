//! Port of `ProjectionStore.getRuntimeRecoveryProjection`: the runtime state a
//! boot-time recovery pass needs for one thread. Each query is copied verbatim
//! from upstream. The rules inside them (what counts as active work, which
//! provider sessions outlive a detach, which runs keep background work) are the
//! recovery contract, so keep them as written.

use rusqlite::Connection;
use turin_contracts::ThreadId;
use turin_contracts::orchestration_v2::{
    OrchestrationV2AppThread, OrchestrationV2ConversationMessage, OrchestrationV2ExecutionNode,
    OrchestrationV2ProviderSession, OrchestrationV2ProviderThread, OrchestrationV2ProviderTurn,
    OrchestrationV2Run, OrchestrationV2RunAttempt, OrchestrationV2RuntimeRequest,
    OrchestrationV2Subagent, OrchestrationV2TurnItem,
};

use crate::projection::error::ReadFailure;
use crate::projection::sql::query_payloads;
use crate::projection::store::read_thread;

/// `ProjectionRuntimeRecoveryState`.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeRecoveryState {
    pub thread: OrchestrationV2AppThread,
    pub runs: Vec<OrchestrationV2Run>,
    pub attempts: Vec<OrchestrationV2RunAttempt>,
    pub nodes: Vec<OrchestrationV2ExecutionNode>,
    pub subagents: Vec<OrchestrationV2Subagent>,
    pub provider_sessions: Vec<OrchestrationV2ProviderSession>,
    pub provider_threads: Vec<OrchestrationV2ProviderThread>,
    pub provider_turns: Vec<OrchestrationV2ProviderTurn>,
    pub runtime_requests: Vec<OrchestrationV2RuntimeRequest>,
    pub messages: Vec<OrchestrationV2ConversationMessage>,
    pub turn_items: Vec<OrchestrationV2TurnItem>,
}

/// Runs the ten recovery queries in upstream order. A missing thread is
/// `ThreadNotFound`; a row that fails to decode is a read error for the thread.
pub(crate) fn runtime_recovery_state(
    conn: &Connection,
    thread_id: &ThreadId,
) -> Result<RuntimeRecoveryState, ReadFailure> {
    let thread = read_thread(conn, thread_id)?;
    let params = rusqlite::params![thread_id.as_str()];

    let runs = query_payloads(conn, RUNS_SQL, params)?;
    let attempts = query_payloads(conn, ATTEMPTS_SQL, params)?;
    let nodes = query_payloads(conn, NODES_SQL, params)?;
    let subagents = query_payloads(conn, SUBAGENTS_SQL, params)?;
    let provider_sessions = query_payloads(conn, PROVIDER_SESSIONS_SQL, params)?;
    let provider_threads = query_payloads(conn, PROVIDER_THREADS_SQL, params)?;
    let provider_turns = query_payloads(conn, PROVIDER_TURNS_SQL, params)?;
    let runtime_requests = query_payloads(conn, RUNTIME_REQUESTS_SQL, params)?;
    let messages = query_payloads(conn, MESSAGES_SQL, params)?;
    let turn_items = query_payloads(conn, TURN_ITEMS_SQL, params)?;

    Ok(RuntimeRecoveryState {
        thread,
        runs,
        attempts,
        nodes,
        subagents,
        provider_sessions,
        provider_threads,
        provider_turns,
        runtime_requests,
        messages,
        turn_items,
    })
}

// Every query binds the thread id as `?1`; upstream interpolates `${threadId}`
// in each. Runs first, because the other queries reference its status set.
const RUNS_SQL: &str = "
SELECT payload_json FROM orchestration_v2_projection_runs AS run
WHERE run.thread_id = ?1
  AND (
    run.status IN ('queued', 'preparing', 'starting', 'running', 'waiting')
    OR run.run_id = (
      SELECT latest.run_id FROM orchestration_v2_projection_runs AS latest
      WHERE latest.thread_id = ?1
      ORDER BY latest.ordinal DESC LIMIT 1
    )
    -- Retain the latest run for each provider thread with lost
    -- background work, including owners used before a handoff.
    OR run.run_id IN (
      SELECT (
        SELECT ended.run_id FROM orchestration_v2_projection_runs AS ended
        WHERE ended.thread_id = ?1
          AND ended.provider_thread_id = roster.provider_thread_id
          AND ended.status NOT IN ('queued', 'rolled_back')
        ORDER BY ended.completed_at IS NULL DESC, ended.completed_at DESC,
          ended.ordinal DESC
        LIMIT 1
      )
      FROM orchestration_v2_projection_provider_threads AS roster
      WHERE roster.thread_id = ?1
        AND CASE WHEN json_valid(roster.payload_json)
          THEN json_array_length(roster.payload_json, '$.pendingBackgroundTasks') > 0
          ELSE 0 END
    )
    OR run.run_id IN (
      SELECT item.run_id FROM orchestration_v2_projection_turn_items AS item
      WHERE item.thread_id = ?1
        AND item.type IN ('command_execution', 'dynamic_tool', 'subagent')
        AND item.status IN ('pending', 'running', 'waiting')
        AND item.run_id IS NOT NULL
    )
  )
ORDER BY run.ordinal ASC
";

const ATTEMPTS_SQL: &str = "
SELECT attempt.payload_json
FROM orchestration_v2_projection_run_attempts AS attempt
WHERE attempt.thread_id = ?1
  AND attempt.run_id IN (
    SELECT run_id FROM orchestration_v2_projection_runs
    WHERE thread_id = ?1
      AND status IN ('queued', 'preparing', 'starting', 'running', 'waiting')
  )
ORDER BY attempt.run_id ASC, attempt.attempt_ordinal ASC
";

const NODES_SQL: &str = "
SELECT node.payload_json FROM orchestration_v2_projection_nodes AS node
WHERE node.thread_id = ?1
  AND node.status IN ('pending', 'starting', 'running', 'waiting')
  AND (
    (node.run_id IS NULL AND node.kind = 'root_turn')
    OR node.run_id IN (
      SELECT run_id FROM orchestration_v2_projection_runs
      WHERE thread_id = ?1
        AND status IN ('queued', 'preparing', 'starting', 'running', 'waiting')
    )
    OR node.node_id IN (
      SELECT item.node_id FROM orchestration_v2_projection_turn_items AS item
      WHERE item.thread_id = ?1
        AND item.type IN ('command_execution', 'dynamic_tool', 'subagent')
        AND item.status IN ('pending', 'running', 'waiting')
    )
    OR node.node_id IN (
      SELECT json_extract(item.payload_json, '$.subagentId')
      FROM orchestration_v2_projection_turn_items AS item
      WHERE item.thread_id = ?1 AND item.type = 'subagent'
        AND item.status IN ('pending', 'running', 'waiting')
    )
    OR node.node_id IN (
      SELECT subagent_id FROM orchestration_v2_projection_subagents
      WHERE thread_id = ?1 AND origin = 'provider_native'
        AND status IN ('pending', 'running', 'waiting')
    )
  )
ORDER BY COALESCE(node.started_at, ''), node.node_id ASC
";

const SUBAGENTS_SQL: &str = "
SELECT subagent.payload_json FROM orchestration_v2_projection_subagents AS subagent
WHERE subagent.thread_id = ?1
  AND subagent.status IN ('pending', 'starting', 'running', 'waiting')
  AND (
    subagent.origin = 'provider_native'
    OR subagent.run_id IN (
      SELECT run_id FROM orchestration_v2_projection_runs
      WHERE thread_id = ?1
        AND status IN ('queued', 'preparing', 'starting', 'running', 'waiting')
    )
    OR subagent.subagent_id IN (
      SELECT json_extract(item.payload_json, '$.subagentId')
      FROM orchestration_v2_projection_turn_items AS item
      WHERE item.thread_id = ?1 AND item.type = 'subagent'
        AND item.status IN ('pending', 'running', 'waiting')
    )
  )
ORDER BY COALESCE(subagent.started_at, ''), subagent.subagent_id ASC
";

const PROVIDER_SESSIONS_SQL: &str = "
SELECT session.payload_json
FROM orchestration_v2_projection_provider_sessions AS session
WHERE (
    session.provider_session_id IN (
      SELECT binding.provider_session_id
      FROM orchestration_v2_projection_provider_session_bindings AS binding
      WHERE binding.thread_id = ?1
    )
    AND (
      session.status NOT IN ('stopped', 'error')
      OR session.provider_session_id IN (
        SELECT provider_thread.provider_session_id
        FROM orchestration_v2_projection_provider_threads AS provider_thread
        WHERE provider_thread.provider_thread_id IN (
          SELECT run.provider_thread_id
          FROM orchestration_v2_projection_runs AS run
          WHERE run.thread_id = ?1
            AND run.status IN ('queued', 'preparing', 'starting', 'running', 'waiting')
        )
      )
    )
  )
  -- This thread detached it, which keeps its status. Its process
  -- did not outlive the server either, so recovery stops it.
  OR (
    session.thread_id = ?1
    AND session.status NOT IN ('stopped', 'error')
    AND NOT EXISTS (
      SELECT 1 FROM orchestration_v2_projection_provider_session_bindings AS binding
      WHERE binding.provider_session_id = session.provider_session_id
    )
  )
ORDER BY session.updated_at ASC, session.provider_session_id ASC
";

const PROVIDER_THREADS_SQL: &str = "
SELECT provider_thread.payload_json
FROM orchestration_v2_projection_provider_threads AS provider_thread
WHERE (
    provider_thread.thread_id = ?1
    OR EXISTS (
      SELECT 1 FROM orchestration_v2_projection_nodes AS owner
      WHERE owner.node_id = provider_thread.owner_node_id
        AND owner.thread_id = ?1
    )
    OR EXISTS (
      SELECT 1 FROM orchestration_v2_projection_subagents AS subagent
      WHERE subagent.provider_thread_id = provider_thread.provider_thread_id
        AND subagent.thread_id = ?1
    )
  )
  AND (
    provider_thread.status = 'active'
    OR CASE WHEN json_valid(provider_thread.payload_json)
      THEN json_array_length(provider_thread.payload_json, '$.pendingBackgroundTasks') > 0
      ELSE 0 END
    OR provider_thread.provider_thread_id IN (
      SELECT run.provider_thread_id FROM orchestration_v2_projection_runs AS run
      WHERE run.thread_id = ?1
        AND run.status IN ('queued', 'preparing', 'starting', 'running', 'waiting')
    )
    OR provider_thread.provider_thread_id IN (
      SELECT item.provider_thread_id
      FROM orchestration_v2_projection_turn_items AS item
      WHERE item.thread_id = ?1
        AND item.status IN ('pending', 'running', 'waiting')
    )
  )
ORDER BY COALESCE(provider_thread.first_run_ordinal, 0), provider_thread.provider_thread_id ASC
";

const PROVIDER_TURNS_SQL: &str = "
SELECT provider_turn.payload_json
FROM orchestration_v2_projection_provider_turns AS provider_turn
WHERE provider_turn.thread_id = ?1
  AND provider_turn.status IN ('pending', 'starting', 'running', 'waiting')
  AND provider_turn.run_attempt_id IN (
    SELECT attempt_id FROM orchestration_v2_projection_run_attempts
    WHERE thread_id = ?1
      AND run_id IN (
        SELECT run_id FROM orchestration_v2_projection_runs
        WHERE thread_id = ?1
          AND status IN ('queued', 'preparing', 'starting', 'running', 'waiting')
      )
  )
ORDER BY provider_turn.provider_thread_id ASC, provider_turn.ordinal ASC
";

const RUNTIME_REQUESTS_SQL: &str = "
SELECT payload_json FROM orchestration_v2_projection_runtime_requests
WHERE thread_id = ?1 AND status = 'pending'
ORDER BY created_at ASC, runtime_request_id ASC
";

const MESSAGES_SQL: &str = "
SELECT message.payload_json FROM orchestration_v2_projection_messages AS message
WHERE message.thread_id = ?1 AND message.streaming = 1
  AND message.run_id IN (
    SELECT run_id FROM orchestration_v2_projection_runs
    WHERE thread_id = ?1
      AND status IN ('queued', 'preparing', 'starting', 'running', 'waiting')
  )
ORDER BY message.created_at ASC, message.message_id ASC
";

const TURN_ITEMS_SQL: &str = "
SELECT item.payload_json FROM orchestration_v2_projection_turn_items AS item
WHERE item.thread_id = ?1
  AND item.status IN ('pending', 'running', 'waiting')
  AND (
    item.run_id IN (
      SELECT run_id FROM orchestration_v2_projection_runs
      WHERE thread_id = ?1
        AND status IN ('queued', 'preparing', 'starting', 'running', 'waiting')
    )
    OR item.type IN ('command_execution', 'dynamic_tool', 'subagent')
    OR (
      item.run_id IS NULL
      AND item.node_id IN (
        SELECT node_id FROM orchestration_v2_projection_nodes
        WHERE thread_id = ?1
          AND run_id IS NULL
          AND kind = 'root_turn'
          AND status IN ('pending', 'running', 'waiting')
      )
    )
  )
ORDER BY item.ordinal ASC, item.turn_item_id ASC
";
