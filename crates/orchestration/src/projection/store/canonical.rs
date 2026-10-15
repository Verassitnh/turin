//! `readCanonicalProjection` / `readProjection` of ProjectionStore.ts: one
//! thread's rows, optionally narrowed to a window, to named fields, or by a
//! record filter, plus inherited fork history for the full projection.

use std::collections::HashMap;

use rusqlite::{Connection, params};
use turin_contracts::orchestration_v2::{
    OrchestrationV2AppThread, OrchestrationV2ConversationMessage,
    OrchestrationV2ConversationMessageRole, OrchestrationV2ForkSource,
    OrchestrationV2ProjectedTurnItem, OrchestrationV2ProjectedTurnItemVisibility,
    OrchestrationV2ThreadProjection, OrchestrationV2TurnItem, OrchestrationV2TurnItemStatus,
};
use turin_contracts::{RunId, ThreadId, TurnItemId};

use crate::projection::error::{ProjectionStoreError, ReadFailure};
use crate::projection::shared::timeline::{
    create_orchestration_v2_turn_item_visibility, is_orchestration_v2_superseded_interrupt,
    timeline_attempts, timeline_runs,
};
use crate::projection::sql::{
    SqlBuilder, id_list, opt_text, query_map, query_payload, query_payloads,
};
use crate::projection::thread_history_paging::{
    THREAD_HISTORY_MAX_RAW_TURNS, is_thread_history_user_turn,
};
use crate::projection::turn_item;

/// A non-thread record kind of [`OrchestrationV2ThreadProjection`]
/// (`ProjectionRecordField`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProjectionRecordField {
    Runs,
    Attempts,
    Nodes,
    Subagents,
    ProviderSessions,
    ProviderThreads,
    ProviderTurns,
    RuntimeRequests,
    Messages,
    Plans,
    TurnItems,
    CheckpointScopes,
    Checkpoints,
    ContextHandoffs,
    ContextTransfers,
}

/// `ProjectionRecordFilter`.
#[derive(Debug, Clone, Default)]
pub struct ProjectionRecordFilter {
    pub message_roles: Option<Vec<OrchestrationV2ConversationMessageRole>>,
    pub turn_item_run_id: Option<RunId>,
    pub message_ids: Option<Vec<String>>,
    pub message_run_ids: Option<Vec<String>>,
    /// `None` entries match items with no run (`run_id IS NULL`).
    pub turn_item_run_ids: Option<Vec<Option<RunId>>>,
    pub run_ids: Option<Vec<RunId>>,
    pub turn_item_types: Option<Vec<String>>,
    pub turn_item_statuses: Option<Vec<OrchestrationV2TurnItemStatus>>,
    /// Skips finished tool call, assistant message and reasoning nodes of finished
    /// runs. Command decisions only read root, subagent and active-run nodes.
    pub omit_settled_leaf_nodes: bool,
}

/// Where a history window ends, for forks: the item the client anchors on and
/// the thread that owns it.
#[derive(Debug, Clone)]
pub struct HistoryAnchor {
    pub thread_id: ThreadId,
    pub item_id: TurnItemId,
}

/// The `window` argument of `readCanonicalProjection` / `readProjection`.
#[derive(Debug, Clone, Default)]
pub struct ReadWindow {
    pub row_limit: i64,
    pub user_turn_limit: Option<i64>,
    pub anchor_item_id: Option<TurnItemId>,
    pub required_run_id: Option<RunId>,
    pub suppress_local: bool,
    pub history_anchor: Option<HistoryAnchor>,
}

fn wants(fields: Option<&[ProjectionRecordField]>, field: ProjectionRecordField) -> bool {
    fields.is_none_or(|fields| fields.contains(&field))
}

/// A pointer `MAX_SAFE_INTEGER` for message ordering without turn items.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

fn messages_by_turn_item_order(
    mut messages: Vec<OrchestrationV2ConversationMessage>,
    turn_items: &[OrchestrationV2TurnItem],
    mut message_ordinals: HashMap<String, u64>,
) -> Vec<OrchestrationV2ConversationMessage> {
    for item in turn_items {
        let Some(message_id) = turn_item::message_id(item) else {
            continue;
        };
        let ordinal = turn_item::common(item).ordinal;
        let entry = message_ordinals
            .entry(message_id.to_string())
            .or_insert(ordinal);
        if ordinal < *entry {
            *entry = ordinal;
        }
    }
    messages.sort_by(|left, right| {
        let ordinal = |message: &OrchestrationV2ConversationMessage| {
            message_ordinals
                .get(message.id.as_str())
                .copied()
                .unwrap_or(MAX_SAFE_INTEGER)
        };
        ordinal(left)
            .cmp(&ordinal(right))
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    messages
}

/// One thread's persisted rows (`readCanonicalProjection`). `visible_turn_items`
/// is left empty; [`read_projection`] fills it.
pub(crate) fn read_canonical_projection(
    conn: &Connection,
    thread_id: &ThreadId,
    window: Option<&ReadWindow>,
    fields: Option<&[ProjectionRecordField]>,
    filter: Option<&ProjectionRecordFilter>,
) -> Result<OrchestrationV2ThreadProjection, ReadFailure> {
    use ProjectionRecordField as F;
    let thread: OrchestrationV2AppThread = query_payload(
        conn,
        "SELECT payload_json
         FROM orchestration_v2_projection_threads
         WHERE thread_id = ?1
         LIMIT 1",
        [thread_id.as_str()],
    )?
    .ok_or_else(|| {
        ReadFailure::Store(ProjectionStoreError::ThreadNotFound {
            thread_id: thread_id.clone(),
        })
    })?;

    let turn_items: Vec<OrchestrationV2TurnItem> = if !wants(fields, F::TurnItems) {
        Vec::new()
    } else {
        match window {
            None => read_turn_items(conn, thread_id, filter)?,
            Some(window) => read_windowed_turn_items(conn, thread_id, window)?,
        }
    };

    // Reuse the decoded items for cohort IDs and the resulting projection.
    let cohort = |field: fn(&OrchestrationV2TurnItem) -> Option<String>| {
        id_list(turn_items.iter().filter_map(field))
    };
    let cohort_run_ids = cohort(|item| turn_item::common(item).run_id.map(|id| id.to_string()));
    let node_ids_of_items = cohort(|item| turn_item::common(item).node_id.map(|id| id.to_string()));
    let required_run_id = window
        .and_then(|window| window.required_run_id.as_ref())
        .map(|id| id.as_str());

    // A run can contain thousands of completed nodes. Load the visible
    // nodes and live control dependencies, then walk only their ancestors.
    let cohort_node_ids = match window {
        Some(_)
            if !fields.is_some_and(|fields| {
                fields
                    .iter()
                    .all(|field| matches!(field, F::Runs | F::Attempts | F::TurnItems))
            }) =>
        {
            id_list(retained_node_ids(
                conn,
                thread_id,
                &node_ids_of_items,
                &cohort_run_ids,
                required_run_id,
            )?)
        }
        _ => node_ids_of_items,
    };
    let cohort_provider_thread_ids = cohort(|item| {
        turn_item::common(item)
            .provider_thread_id
            .map(|id| id.to_string())
    });
    let cohort_provider_turn_ids = cohort(|item| {
        turn_item::common(item)
            .provider_turn_id
            .map(|id| id.to_string())
    });
    let cohort_message_ids = cohort(|item| turn_item::message_id(item).map(|id| id.to_string()));
    let cohort_plan_ids = cohort(|item| turn_item::plan_id(item).map(|id| id.to_string()));
    let cohort_checkpoint_ids =
        cohort(|item| turn_item::checkpoint_id(item).map(|id| id.to_string()));
    let cohort_handoff_ids =
        cohort(|item| turn_item::context_handoff_id(item).map(|id| id.to_string()));
    let tid = thread_id.as_str();
    let windowed = window.is_some();

    let runs = if !wants(fields, F::Runs) {
        Vec::new()
    } else if !windowed {
        let mut sql = SqlBuilder::new(
            "SELECT payload_json
             FROM orchestration_v2_projection_runs
             WHERE thread_id = ",
        );
        sql.bind(tid.to_owned());
        if let Some(ids) = filter.and_then(|filter| filter.run_ids.as_ref()) {
            sql.push(" AND run_id IN (SELECT value FROM json_each(")
                .bind(id_list(ids.iter().map(|id| id.as_str())))
                .push("))");
        }
        sql.push(" ORDER BY ordinal ASC");
        sql.query_payloads(conn)?
    } else {
        query_payloads(
            conn,
            "SELECT payload_json FROM orchestration_v2_projection_runs
             WHERE thread_id = ?1
               AND (status IN ('queued','preparing','starting','running','waiting')
                 OR run_id IN (SELECT value FROM json_each(?2))
                 OR run_id = ?3)
             ORDER BY ordinal ASC",
            params![tid, cohort_run_ids, required_run_id],
        )?
    };

    let attempts = if !wants(fields, F::Attempts) {
        Vec::new()
    } else if !windowed {
        query_payloads(
            conn,
            "SELECT payload_json
             FROM orchestration_v2_projection_run_attempts
             WHERE thread_id = ?1
             ORDER BY run_id ASC, attempt_ordinal ASC",
            [tid],
        )?
    } else {
        query_payloads(
            conn,
            "SELECT payload_json FROM orchestration_v2_projection_run_attempts
             WHERE thread_id = ?1
               AND (status IN ('pending','starting','running','waiting')
                 OR run_id IN (SELECT value FROM json_each(?2)))
             ORDER BY run_id ASC, attempt_ordinal ASC",
            params![tid, cohort_run_ids],
        )?
    };

    let nodes = if !wants(fields, F::Nodes) {
        Vec::new()
    } else if !windowed {
        let omit = filter.is_some_and(|filter| filter.omit_settled_leaf_nodes);
        let sql = format!(
            "SELECT payload_json
             FROM orchestration_v2_projection_nodes AS node
             WHERE node.thread_id = ?1
               {}
             ORDER BY COALESCE(node.started_at, ''), node.node_id ASC",
            if omit {
                "AND NOT (
                  node.kind IN ('tool_call', 'assistant_message', 'reasoning')
                  AND node.status IN ('completed', 'interrupted', 'failed', 'cancelled', 'rolled_back')
                  AND node.run_id IS NOT NULL
                  AND NOT EXISTS (
                    SELECT 1 FROM orchestration_v2_projection_runs AS run
                    WHERE run.run_id = node.run_id
                      AND run.status IN ('preparing', 'queued', 'starting', 'running', 'waiting')
                  )
                )"
            } else {
                ""
            }
        );
        query_payloads(conn, &sql, [tid])?
    } else {
        // Keep retained IDs outermost so SQLite looks up each node by its
        // primary key instead of scanning every historical node in the thread.
        let mut rows = query_map(
            conn,
            WINDOW_NODES_SQL,
            params![tid, cohort_node_ids],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )?;
        rows.sort_by_key(|(_, position)| *position);
        rows.into_iter()
            .map(|(payload, _)| crate::projection::sql::decode(&payload))
            .collect::<Result<Vec<_>, _>>()?
    };

    let subagents = if !wants(fields, F::Subagents) {
        Vec::new()
    } else if !windowed {
        query_payloads(
            conn,
            "SELECT payload_json
             FROM orchestration_v2_projection_subagents
             WHERE thread_id = ?1
             ORDER BY COALESCE(started_at, ''), subagent_id ASC",
            [tid],
        )?
    } else {
        query_payloads(
            conn,
            "SELECT payload_json FROM orchestration_v2_projection_subagents
             WHERE thread_id = ?1
               AND (status IN ('pending','starting','running','waiting')
                 OR run_id IN (SELECT value FROM json_each(?2))
                 OR parent_node_id IN (SELECT value FROM json_each(?3)))
             ORDER BY COALESCE(started_at, ''), subagent_id ASC",
            params![tid, cohort_run_ids, cohort_node_ids],
        )?
    };

    let provider_sessions = if !wants(fields, F::ProviderSessions) {
        Vec::new()
    } else if !windowed {
        query_payloads(
            conn,
            "SELECT sessions.payload_json
             FROM orchestration_v2_projection_provider_sessions AS sessions
             INNER JOIN orchestration_v2_projection_provider_session_bindings AS bindings
               ON bindings.provider_session_id = sessions.provider_session_id
             WHERE bindings.thread_id = ?1
             ORDER BY sessions.updated_at ASC, sessions.provider_session_id ASC",
            [tid],
        )?
    } else {
        query_payloads(
            conn,
            "SELECT DISTINCT sessions.payload_json
             FROM orchestration_v2_projection_provider_sessions AS sessions
             INNER JOIN orchestration_v2_projection_provider_session_bindings AS bindings
               ON bindings.provider_session_id = sessions.provider_session_id
             LEFT JOIN orchestration_v2_projection_provider_threads AS threads
               ON threads.provider_session_id = sessions.provider_session_id
             WHERE bindings.thread_id = ?1
               AND (sessions.status IN ('starting','running','waiting')
                 OR threads.provider_thread_id IN (SELECT value FROM json_each(?2)))
             ORDER BY sessions.updated_at ASC, sessions.provider_session_id ASC",
            params![tid, cohort_provider_thread_ids],
        )?
    };

    let provider_threads = if !wants(fields, F::ProviderThreads) {
        Vec::new()
    } else if !windowed {
        query_payloads(
            conn,
            "SELECT payload_json
             FROM orchestration_v2_projection_provider_threads
             WHERE thread_id = ?1
                OR owner_node_id IN (
                  SELECT node_id
                  FROM orchestration_v2_projection_nodes
                  WHERE thread_id = ?1
                )
                OR provider_thread_id IN (
                  SELECT provider_thread_id
                  FROM orchestration_v2_projection_subagents
                  WHERE thread_id = ?1
                    AND provider_thread_id IS NOT NULL
                )
             ORDER BY COALESCE(first_run_ordinal, 0), provider_thread_id ASC",
            [tid],
        )?
    } else {
        query_payloads(
            conn,
            "SELECT payload_json FROM orchestration_v2_projection_provider_threads
             WHERE (thread_id = ?1 AND status = 'active')
               OR provider_thread_id IN (SELECT value FROM json_each(?2))
               OR owner_node_id IN (SELECT value FROM json_each(?3))
             ORDER BY COALESCE(first_run_ordinal, 0), provider_thread_id ASC",
            params![tid, cohort_provider_thread_ids, cohort_node_ids],
        )?
    };

    let provider_turns = if !wants(fields, F::ProviderTurns) {
        Vec::new()
    } else if !windowed {
        query_payloads(
            conn,
            "SELECT payload_json
             FROM orchestration_v2_projection_provider_turns
             WHERE thread_id = ?1
             ORDER BY provider_thread_id ASC, ordinal ASC",
            [tid],
        )?
    } else {
        query_payloads(
            conn,
            "SELECT payload_json FROM orchestration_v2_projection_provider_turns
             WHERE thread_id = ?1
               AND (status IN ('starting','running','waiting')
                 OR provider_turn_id IN (SELECT value FROM json_each(?2))
                 OR node_id IN (SELECT value FROM json_each(?3)))
             ORDER BY provider_thread_id ASC, ordinal ASC",
            params![tid, cohort_provider_turn_ids, cohort_node_ids],
        )?
    };

    let runtime_requests = if !wants(fields, F::RuntimeRequests) {
        Vec::new()
    } else if !windowed {
        query_payloads(
            conn,
            "SELECT payload_json
             FROM orchestration_v2_projection_runtime_requests
             WHERE thread_id = ?1
             ORDER BY created_at ASC, runtime_request_id ASC",
            [tid],
        )?
    } else {
        query_payloads(
            conn,
            "SELECT payload_json FROM orchestration_v2_projection_runtime_requests
             WHERE thread_id = ?1
               AND (status IN ('pending','waiting')
                 OR node_id IN (SELECT value FROM json_each(?2))
                 OR provider_turn_id IN (SELECT value FROM json_each(?3)))
             ORDER BY created_at ASC, runtime_request_id ASC",
            params![tid, cohort_node_ids, cohort_provider_turn_ids],
        )?
    };

    let messages: Vec<OrchestrationV2ConversationMessage> = if !wants(fields, F::Messages) {
        Vec::new()
    } else if !windowed {
        let mut sql = SqlBuilder::new(
            "SELECT payload_json FROM orchestration_v2_projection_messages
             WHERE thread_id = ",
        );
        sql.bind(tid.to_owned());
        if let Some(ids) = filter.and_then(|filter| filter.message_ids.as_ref()) {
            sql.push(" AND message_id IN (SELECT value FROM json_each(")
                .bind(id_list(ids))
                .push("))");
        }
        if let Some(roles) = filter.and_then(|filter| filter.message_roles.as_ref()) {
            sql.push(" AND role IN (SELECT value FROM json_each(")
                .bind(id_list(roles.iter().map(crate::projection::sql::text)))
                .push("))");
        }
        if let Some(ids) = filter.and_then(|filter| filter.message_run_ids.as_ref()) {
            sql.push(" AND run_id IN (SELECT value FROM json_each(")
                .bind(id_list(ids))
                .push("))");
        }
        sql.push(" ORDER BY created_at ASC, message_id ASC");
        sql.query_payloads(conn)?
    } else {
        query_payloads(
            conn,
            "SELECT payload_json FROM orchestration_v2_projection_messages AS message
             WHERE message.thread_id = ?1
               AND (
                 message.message_id IN (
                   SELECT value FROM json_each(?2)
                 )
                 OR message.run_id IN (
                   SELECT run_id FROM orchestration_v2_projection_runs
                   WHERE thread_id = ?1
                     AND status IN ('queued', 'preparing', 'starting', 'running', 'waiting')
                 )
               )
             ORDER BY created_at ASC, message_id ASC",
            params![tid, cohort_message_ids],
        )?
    };

    let plans = if !wants(fields, F::Plans) {
        Vec::new()
    } else if !windowed {
        query_payloads(
            conn,
            "SELECT payload_json FROM orchestration_v2_projection_plans
             WHERE thread_id = ?1 ORDER BY plan_id ASC",
            [tid],
        )?
    } else {
        query_payloads(
            conn,
            "SELECT payload_json FROM orchestration_v2_projection_plans AS plan
             WHERE plan.thread_id = ?1
               AND (
                 plan.status = 'active'
                 OR plan.plan_id IN (
                   SELECT value FROM json_each(?2)
                 )
               )
             ORDER BY plan_id ASC",
            params![tid, cohort_plan_ids],
        )?
    };

    let checkpoint_scopes = if !wants(fields, F::CheckpointScopes) {
        Vec::new()
    } else if !windowed {
        query_payloads(
            conn,
            "SELECT payload_json
             FROM orchestration_v2_projection_checkpoint_scopes
             WHERE thread_id = ?1
             ORDER BY ordinal_within_parent ASC, scope_id ASC",
            [tid],
        )?
    } else {
        query_payloads(
            conn,
            "SELECT payload_json FROM orchestration_v2_projection_checkpoint_scopes
             WHERE thread_id = ?1
               AND (run_id IN (SELECT value FROM json_each(?2))
                 OR node_id IN (SELECT value FROM json_each(?3)))
             ORDER BY ordinal_within_parent ASC, scope_id ASC",
            params![tid, cohort_run_ids, cohort_node_ids],
        )?
    };

    let checkpoints = if !wants(fields, F::Checkpoints) {
        Vec::new()
    } else if !windowed {
        query_payloads(
            conn,
            "SELECT payload_json
             FROM orchestration_v2_projection_checkpoints
             WHERE thread_id = ?1
             ORDER BY scope_id ASC, ordinal_within_scope ASC",
            [tid],
        )?
    } else {
        query_payloads(
            conn,
            "SELECT payload_json FROM orchestration_v2_projection_checkpoints
             WHERE thread_id = ?1
               AND (status IN ('pending','capturing')
                 OR checkpoint_id IN (SELECT value FROM json_each(?2))
                 OR run_id IN (SELECT value FROM json_each(?3))
                 OR node_id IN (SELECT value FROM json_each(?4)))
             ORDER BY scope_id ASC, ordinal_within_scope ASC",
            params![tid, cohort_checkpoint_ids, cohort_run_ids, cohort_node_ids],
        )?
    };

    let context_handoffs = if !wants(fields, F::ContextHandoffs) {
        Vec::new()
    } else if !windowed {
        query_payloads(
            conn,
            "SELECT payload_json FROM orchestration_v2_projection_context_handoffs
             WHERE thread_id = ?1 ORDER BY rowid ASC",
            [tid],
        )?
    } else {
        query_payloads(
            conn,
            "SELECT payload_json FROM orchestration_v2_projection_context_handoffs AS handoff
             WHERE handoff.thread_id = ?1
               AND (
                 handoff.status IN ('pending', 'ready')
                 OR handoff.context_handoff_id IN (
                   SELECT value FROM json_each(?2)
                 )
               )
             ORDER BY rowid ASC",
            params![tid, cohort_handoff_ids],
        )?
    };

    let context_transfers = if !wants(fields, F::ContextTransfers) {
        Vec::new()
    } else if !windowed {
        query_payloads(
            conn,
            "SELECT payload_json
             FROM orchestration_v2_projection_context_transfers
             WHERE source_thread_id = ?1 OR target_thread_id = ?1
             ORDER BY rowid ASC",
            [tid],
        )?
    } else {
        query_payloads(
            conn,
            "SELECT payload_json FROM orchestration_v2_projection_context_transfers
             WHERE (source_thread_id = ?1 OR target_thread_id = ?1)
               AND (status IN ('pending','running','waiting')
                 OR target_run_id IN (SELECT value FROM json_each(?2))
                 OR json_extract(payload_json, '$.contextHandoffId') IN
                   (SELECT value FROM json_each(?3)))
             ORDER BY rowid ASC",
            params![tid, cohort_run_ids, cohort_handoff_ids],
        )?
    };

    // A metadata read must preserve message ordering without decoding tool output.
    let mut message_ordinals = HashMap::new();
    if fields.is_some_and(|fields| fields.contains(&F::Messages)) && messages.len() > 1 {
        for (message_id, ordinal) in query_map(
            conn,
            "SELECT json_extract(payload_json, '$.messageId') AS message_id, MIN(ordinal) AS ordinal
             FROM orchestration_v2_projection_turn_items
             WHERE thread_id = ?1 AND type IN ('user_message', 'assistant_message')
             GROUP BY json_extract(payload_json, '$.messageId')",
            [tid],
            |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?)),
        )? {
            if let Some(message_id) = message_id {
                message_ordinals.insert(message_id, ordinal.max(0) as u64);
            }
        }
    }
    let messages = messages_by_turn_item_order(messages, &turn_items, message_ordinals);

    let updated_at = thread.updated_at;
    Ok(OrchestrationV2ThreadProjection {
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
        plans,
        turn_items,
        checkpoint_scopes,
        checkpoints,
        context_handoffs,
        context_transfers,
        visible_turn_items: Vec::new(),
        updated_at,
    })
}

fn read_turn_items(
    conn: &Connection,
    thread_id: &ThreadId,
    filter: Option<&ProjectionRecordFilter>,
) -> Result<Vec<OrchestrationV2TurnItem>, ReadFailure> {
    let mut sql = SqlBuilder::new(
        "SELECT payload_json
         FROM orchestration_v2_projection_turn_items
         WHERE thread_id = ",
    );
    sql.bind(thread_id.to_string());
    if let Some(types) = filter.and_then(|filter| filter.turn_item_types.as_ref()) {
        sql.push(" AND type IN (SELECT value FROM json_each(")
            .bind(id_list(types))
            .push("))");
    }
    if let Some(statuses) = filter.and_then(|filter| filter.turn_item_statuses.as_ref()) {
        sql.push(" AND status IN (SELECT value FROM json_each(")
            .bind(id_list(statuses.iter().map(crate::projection::sql::text)))
            .push("))");
    }
    if let Some(run_id) = filter.and_then(|filter| filter.turn_item_run_id.as_ref()) {
        sql.push(" AND run_id = ").bind(run_id.to_string());
    }
    if let Some(run_ids) = filter.and_then(|filter| filter.turn_item_run_ids.as_ref()) {
        let named = run_ids
            .iter()
            .filter_map(|id| id.as_ref().map(|id| id.as_str()))
            .collect::<Vec<_>>();
        let includes_null = run_ids.iter().any(Option::is_none);
        sql.push(" AND (run_id IN (SELECT value FROM json_each(")
            .bind(id_list(named))
            .push(")) OR (")
            .bind(i64::from(includes_null))
            .push(" = 1 AND run_id IS NULL))");
    }
    sql.push(" ORDER BY ordinal ASC, turn_item_id ASC");
    Ok(sql.query_payloads(conn)?)
}

/// Keep retained IDs outermost so SQLite looks up each node by its primary key
/// instead of scanning every historical node in the thread.
pub(crate) const WINDOW_NODES_SQL: &str = "SELECT node.payload_json, retained.key AS position
     FROM json_each(?2) AS retained
     CROSS JOIN orchestration_v2_projection_nodes AS node
       ON node.node_id = retained.value
     WHERE node.thread_id = ?1";

/// The turn-item window: eligible rows up to the anchor, grouped into turns
/// from the newest backwards, plus interrupt requests of retained results.
/// Choose and sort rows by ID, then fetch payloads in that order. A turn
/// window can hold megabytes of tool output, and carrying it through the
/// union and sort cost about a second.
fn read_windowed_turn_items(
    conn: &Connection,
    thread_id: &ThreadId,
    window: &ReadWindow,
) -> Result<Vec<OrchestrationV2TurnItem>, ReadFailure> {
    let user_turn_limit = window.user_turn_limit;
    Ok(query_payloads(
        conn,
        WINDOW_TURN_ITEMS_SQL,
        params![
            thread_id.as_str(),
            opt_text(window.anchor_item_id.as_ref().map(|id| id.as_str())),
            opt_text(window.required_run_id.as_ref().map(|id| id.as_str())),
            user_turn_limit,
            window.row_limit,
            THREAD_HISTORY_MAX_RAW_TURNS as i64 + 2,
            user_turn_limit.unwrap_or(0) + 2,
        ],
    )?)
}

// ?1 thread, ?2 anchor item, ?3 required run, ?4 user-turn limit, ?5 row limit,
// ?6 raw-turn ceiling + 2, ?7 user-turn limit + 2.
pub(crate) const WINDOW_TURN_ITEMS_SQL: &str = "
WITH eligible AS NOT MATERIALIZED (
  SELECT item.rowid AS item_rowid, item.payload_json, item.ordinal, item.turn_item_id,
    item.run_id, item.node_id, item.type
  FROM orchestration_v2_projection_turn_items AS item
  LEFT JOIN orchestration_v2_projection_runs AS run
    ON run.run_id = item.run_id
  WHERE item.thread_id = ?1
    AND item.ordinal <= COALESCE(
      (
        SELECT ordinal
        FROM orchestration_v2_projection_turn_items
        WHERE thread_id = ?1
          AND turn_item_id = ?2
        LIMIT 1
      ),
      9223372036854775807
    )
    AND (
      ?3 IS NOT NULL
      OR (
        (run.status IS NULL OR run.status <> 'rolled_back')
        AND NOT (
          item.type = 'user_message'
          AND json_extract(item.payload_json, '$.inputIntent') = 'queued_turn'
          AND run.status IS 'cancelled'
        )
      )
    )
    AND (
      ?3 IS NULL
      OR (
        item.run_id IS NULL
        AND EXISTS (
          SELECT 1
          FROM orchestration_v2_projection_threads AS source_thread
          WHERE source_thread.thread_id = item.thread_id
            AND json_extract(source_thread.payload_json, '$.historyOrigin') = 'v1_import'
        )
      )
      OR run.ordinal <= (
        SELECT required_run.ordinal
        FROM orchestration_v2_projection_runs AS required_run
        WHERE required_run.thread_id = item.thread_id
          AND required_run.run_id = ?3
        LIMIT 1
      )
    )
    AND NOT (
      item.type = 'run_interrupt_result'
      AND EXISTS (
        SELECT 1
        FROM orchestration_v2_projection_run_attempts AS attempt
        WHERE attempt.run_id = item.run_id
          AND attempt.root_node_id = item.node_id
          AND attempt.status = 'superseded'
      )
      AND NOT EXISTS (
        SELECT 1
        FROM orchestration_v2_projection_turn_items AS request
        WHERE request.thread_id = item.thread_id
          AND request.run_id = item.run_id
          AND request.type = 'run_interrupt_request'
      )
    )
), run_starts AS (
  -- A wake (PR watch, task completion) starts its run with a notification
  -- instead of a user message. Read the first input of each run, newest run
  -- first, and add the notifications to the user-message turn starts. A run
  -- without an eligible first input (queued, rolled back) is skipped, so it
  -- cannot hide older turns. CROSS JOIN keeps the runs as the outer loop, and
  -- each probe reads only the first rows of a run.
  SELECT start.ordinal, start.type
  FROM orchestration_v2_projection_runs AS recent_run
  CROSS JOIN eligible AS start ON start.item_rowid = (
    SELECT first_input.rowid
    FROM orchestration_v2_projection_turn_items AS first_input
    WHERE first_input.run_id = recent_run.run_id
      AND first_input.type IN ('user_message', 'notification')
    ORDER BY first_input.ordinal
    LIMIT 1
  )
  WHERE recent_run.thread_id = ?1
    AND recent_run.ordinal <= COALESCE(
      (
        SELECT anchor_run.ordinal
        FROM orchestration_v2_projection_turn_items AS anchor_item
        JOIN orchestration_v2_projection_runs AS anchor_run
          ON anchor_run.run_id = anchor_item.run_id
        WHERE anchor_item.thread_id = ?1
          AND anchor_item.turn_item_id = ?2
        LIMIT 1
      ),
      9223372036854775807
    )
    AND ?4 IS NOT NULL
  ORDER BY recent_run.ordinal DESC
  LIMIT ?6
), turn_anchors AS (
  SELECT ordinal, payload_json
  FROM (
    SELECT ordinal, payload_json
    FROM eligible
    WHERE type = 'user_message'
      AND json_extract(payload_json, '$.inputIntent') IN ('turn_start', 'queued_turn')
      AND ?4 IS NOT NULL
    ORDER BY ordinal DESC
    LIMIT ?6
  )
  UNION ALL
  SELECT ordinal, NULL FROM run_starts WHERE type = 'notification'
  ORDER BY ordinal DESC
  LIMIT ?6
), user_anchors AS (
  SELECT ordinal
  FROM turn_anchors
  WHERE json_extract(payload_json, '$.createdBy') = 'user'
  ORDER BY ordinal DESC
  LIMIT ?7
), boundary AS (
  SELECT CASE
    WHEN COUNT(*) >= ?7 THEN MIN(ordinal)
    WHEN (SELECT COUNT(*) FROM turn_anchors) >= ?6
      THEN (SELECT MIN(ordinal) FROM turn_anchors)
    ELSE 0
  END AS ordinal
  FROM user_anchors
), selected AS (
  SELECT item_rowid, ordinal, turn_item_id, run_id, type
  FROM eligible
  WHERE ordinal >= (SELECT ordinal FROM boundary)
  ORDER BY ordinal DESC, turn_item_id DESC
  LIMIT CASE
    WHEN ?5 = 0 THEN 0
    WHEN (SELECT COUNT(*) FROM user_anchors) > 0 THEN -1
    ELSE ?5
  END
), retained AS MATERIALIZED (
  SELECT item_rowid, ordinal, turn_item_id FROM selected
  UNION
  SELECT request.rowid AS item_rowid, request.ordinal, request.turn_item_id
  FROM orchestration_v2_projection_turn_items AS request
  WHERE request.run_id IN (
      SELECT run_id FROM selected
      WHERE type = 'run_interrupt_result' AND run_id IS NOT NULL
    )
    AND request.type = 'run_interrupt_request'
  UNION
  SELECT latest.item_rowid, latest.ordinal, latest.turn_item_id
  FROM (
    SELECT rowid AS item_rowid, ordinal, turn_item_id
    FROM orchestration_v2_projection_turn_items
    WHERE thread_id = ?1
      AND ?2 IS NULL
      AND ?3 IS NULL
    ORDER BY ordinal DESC, turn_item_id DESC
    LIMIT 1
  ) AS latest
  ORDER BY ordinal ASC, turn_item_id ASC
)
-- CROSS JOIN keeps the sorted IDs as the outer loop, so SQLite skips sorting
-- again once the payloads are attached. Carry the rowid through selection to
-- avoid a second string-key index lookup.
SELECT item.payload_json
FROM retained
CROSS JOIN orchestration_v2_projection_turn_items AS item
  ON item.rowid = retained.item_rowid
ORDER BY retained.ordinal ASC, retained.turn_item_id ASC
";

/// The nodes a window needs: the cohort's own, live control dependencies, and
/// every ancestor (?1 thread, ?2 cohort nodes, ?3 cohort runs, ?4 required run).
pub(crate) const RETAINED_NODE_IDS_SQL: &str = "WITH RECURSIVE retained(node_id) AS (
           SELECT value FROM json_each(?2)
           UNION
           SELECT node_id FROM orchestration_v2_projection_nodes
           WHERE thread_id = ?1 AND status IN ('pending','starting','running','waiting')
           UNION
           SELECT root_node_id FROM orchestration_v2_projection_run_attempts
           WHERE thread_id = ?1 AND (
             status IN ('pending','starting','running','waiting')
             OR run_id IN (SELECT value FROM json_each(?3))
             OR run_id = ?4)
           UNION
           SELECT json_extract(payload_json, '$.rootNodeId') FROM orchestration_v2_projection_runs
           WHERE thread_id = ?1 AND (
             status IN ('queued','preparing','starting','running','waiting')
             OR run_id IN (SELECT value FROM json_each(?3))
             OR run_id = ?4)
           UNION
           SELECT node_id FROM orchestration_v2_projection_runtime_requests
           WHERE thread_id = ?1 AND status IN ('pending','waiting')
           UNION
           SELECT parent_node_id FROM orchestration_v2_projection_subagents
           WHERE thread_id = ?1 AND status IN ('pending','starting','running','waiting')
           UNION
           SELECT node_id FROM orchestration_v2_projection_provider_turns
           WHERE thread_id = ?1 AND status IN ('starting','running','waiting')
           UNION
           SELECT parent.parent_node_id FROM orchestration_v2_projection_nodes AS parent
           INNER JOIN retained ON parent.node_id = retained.node_id
           WHERE parent.thread_id = ?1 AND parent.parent_node_id IS NOT NULL
         )
         SELECT retained.node_id
         FROM retained
         LEFT JOIN orchestration_v2_projection_nodes AS node
           ON node.node_id = retained.node_id AND node.thread_id = ?1
         WHERE retained.node_id IS NOT NULL
         ORDER BY COALESCE(node.started_at, ''), retained.node_id ASC";

/// The nodes a window needs: the cohort's own, live control dependencies, and
/// every ancestor. Sort IDs before fetching bodies, so large payloads never
/// enter SQLite's temporary sort. The node read restores this order by key.
fn retained_node_ids(
    conn: &Connection,
    thread_id: &ThreadId,
    cohort_node_ids: &str,
    cohort_run_ids: &str,
    required_run_id: Option<&str>,
) -> Result<Vec<String>, ReadFailure> {
    Ok(query_map(
        conn,
        RETAINED_NODE_IDS_SQL,
        params![
            thread_id.as_str(),
            cohort_node_ids,
            cohort_run_ids,
            required_run_id
        ],
        |row| row.get::<_, String>(0),
    )?)
}

// ---------------------------------------------------------------------------
// Visible timeline rows and fork inheritance
// ---------------------------------------------------------------------------

/// `activeLocalTurnItems`: this thread's visible items, numbered from zero.
pub(crate) fn active_local_turn_items(
    projection: &OrchestrationV2ThreadProjection,
) -> Vec<OrchestrationV2ProjectedTurnItem> {
    let runs = timeline_runs(&projection.runs);
    let attempts = timeline_attempts(&projection.attempts);
    let visibility = create_orchestration_v2_turn_item_visibility(
        &runs,
        &attempts,
        projection.turn_items.iter(),
    );
    projection
        .turn_items
        .iter()
        .filter(|item| visibility.is_visible(*item))
        .enumerate()
        .map(|(position, item)| {
            let common = turn_item::common(item);
            OrchestrationV2ProjectedTurnItem {
                position: position as u64,
                visibility: OrchestrationV2ProjectedTurnItemVisibility::Local,
                source_thread_id: common.thread_id.clone(),
                source_item_id: common.id.clone(),
                item: item.clone(),
            }
        })
        .collect()
}

/// `withLocalVisibleTurnItems`.
pub(crate) fn with_local_visible_turn_items(
    mut projection: OrchestrationV2ThreadProjection,
) -> OrchestrationV2ThreadProjection {
    projection.visible_turn_items = active_local_turn_items(&projection);
    projection
}

/// A projected row without its position (`Omit<ProjectedTurnItem, "position">`).
struct UnpositionedRow {
    visibility: OrchestrationV2ProjectedTurnItemVisibility,
    source_thread_id: ThreadId,
    source_item_id: TurnItemId,
    item: OrchestrationV2TurnItem,
}

fn renumber(rows: Vec<UnpositionedRow>) -> Vec<OrchestrationV2ProjectedTurnItem> {
    rows.into_iter()
        .enumerate()
        .map(|(position, row)| OrchestrationV2ProjectedTurnItem {
            position: position as u64,
            visibility: row.visibility,
            source_thread_id: row.source_thread_id,
            source_item_id: row.source_item_id,
            item: row.item,
        })
        .collect()
}

/// `makeForkMarkerTurnItem`: the synthetic row where a fork's own history starts.
pub(crate) fn make_fork_marker_turn_item(
    target_thread: &OrchestrationV2AppThread,
    source_thread_id: &ThreadId,
    source_run_id: &RunId,
) -> OrchestrationV2TurnItem {
    let created_at = target_thread.created_at.to_string();
    serde_json::from_value(serde_json::json!({
        "id": format!("turn-item:fork:{}", target_thread.id),
        "threadId": target_thread.id,
        "runId": null,
        "nodeId": null,
        "providerTurnId": null,
        "nativeItemRef": null,
        "parentItemId": null,
        "ordinal": 0,
        "status": "completed",
        "title": "Forked from conversation",
        "startedAt": null,
        "completedAt": created_at,
        "updatedAt": created_at,
        "type": "fork",
        "source": {"type": "run", "threadId": source_thread_id, "runId": source_run_id},
        "targetThreadId": target_thread.id
    }))
    .expect("fork marker is a valid turn item")
}

/// `isTurnItemAtOrBeforeRun`.
pub fn is_turn_item_at_or_before_run(
    history_origin: Option<turin_contracts::orchestration_v2::OrchestrationV2ThreadHistoryOrigin>,
    item_run_id: Option<&RunId>,
    run_ordinal_by_id: &HashMap<RunId, u64>,
    source_run_ordinal: u64,
) -> bool {
    let Some(item_run_id) = item_run_id else {
        return history_origin
            == Some(
                turin_contracts::orchestration_v2::OrchestrationV2ThreadHistoryOrigin::V1Import,
            );
    };
    run_ordinal_by_id
        .get(item_run_id)
        .is_some_and(|ordinal| *ordinal <= source_run_ordinal)
}

fn visible_turn_items_through_run(
    source: &OrchestrationV2ThreadProjection,
    source_run_id: &RunId,
) -> Vec<UnpositionedRow> {
    let Some(source_run) = source.runs.iter().find(|run| &run.id == source_run_id) else {
        return Vec::new();
    };
    let run_ordinal_by_id = source
        .runs
        .iter()
        .map(|run| (run.id.clone(), run.ordinal.get()))
        .collect::<HashMap<_, _>>();
    let inherited_prefix = source
        .visible_turn_items
        .iter()
        .filter(|row| {
            turn_item::common(&row.item).thread_id != &source.thread.id
                || turn_item::type_tag(&row.item) == "fork"
        })
        .map(|row| UnpositionedRow {
            visibility: OrchestrationV2ProjectedTurnItemVisibility::Inherited,
            source_thread_id: row.source_thread_id.clone(),
            source_item_id: row.source_item_id.clone(),
            item: row.item.clone(),
        });
    let attempts = timeline_attempts(&source.attempts);
    let local_prefix = source
        .turn_items
        .iter()
        .filter(|item| {
            if is_orchestration_v2_superseded_interrupt(*item, &attempts, source.turn_items.iter())
            {
                return false;
            }
            is_turn_item_at_or_before_run(
                source.thread.history_origin,
                turn_item::common(item).run_id,
                &run_ordinal_by_id,
                source_run.ordinal.get(),
            )
        })
        .map(|item| {
            let common = turn_item::common(item);
            UnpositionedRow {
                visibility: OrchestrationV2ProjectedTurnItemVisibility::Inherited,
                source_thread_id: common.thread_id.clone(),
                source_item_id: common.id.clone(),
                item: item.clone(),
            }
        });
    inherited_prefix.chain(local_prefix).collect()
}

fn build_visible_turn_items(
    projection: &OrchestrationV2ThreadProjection,
    source: Option<&OrchestrationV2ThreadProjection>,
) -> Vec<OrchestrationV2ProjectedTurnItem> {
    let (Some(OrchestrationV2ForkSource::Run { thread_id, run_id }), Some(source)) =
        (projection.thread.forked_from.as_ref(), source)
    else {
        return active_local_turn_items(projection);
    };
    let inherited = visible_turn_items_through_run(source, run_id);
    let marker = make_fork_marker_turn_item(&projection.thread, thread_id, run_id);
    let marker_id = turn_item::common(&marker).id.clone();
    let local = active_local_turn_items(projection)
        .into_iter()
        .map(|row| UnpositionedRow {
            visibility: OrchestrationV2ProjectedTurnItemVisibility::Local,
            source_thread_id: row.source_thread_id,
            source_item_id: row.source_item_id,
            item: row.item,
        });
    let mut rows = inherited;
    rows.push(UnpositionedRow {
        visibility: OrchestrationV2ProjectedTurnItemVisibility::Synthetic,
        source_thread_id: thread_id.clone(),
        source_item_id: marker_id,
        item: marker,
    });
    rows.extend(local);
    renumber(rows)
}

/// `readProjection`: the canonical rows plus the visible timeline, inheriting a
/// forked source's history.
pub(crate) fn read_projection(
    conn: &Connection,
    thread_id: &ThreadId,
    seen_thread_ids: &[ThreadId],
    window: Option<&ReadWindow>,
) -> Result<OrchestrationV2ThreadProjection, ReadFailure> {
    let suppress = window.is_some_and(|window| {
        window.suppress_local
            || window
                .history_anchor
                .as_ref()
                .is_some_and(|anchor| &anchor.thread_id != thread_id)
    });
    let local_window = window.map(|window| {
        if suppress {
            ReadWindow {
                row_limit: 0,
                anchor_item_id: None,
                ..window.clone()
            }
        } else {
            window.clone()
        }
    });
    // Ancestors contribute timeline items, not their provider/control state.
    // Keep their runs and attempts for fork boundaries and visibility.
    let ancestor_fields = [
        ProjectionRecordField::Runs,
        ProjectionRecordField::Attempts,
        ProjectionRecordField::TurnItems,
    ];
    let projection = read_canonical_projection(
        conn,
        thread_id,
        local_window.as_ref(),
        (window.is_some() && !seen_thread_ids.is_empty()).then_some(&ancestor_fields[..]),
        None,
    )?;
    let Some(OrchestrationV2ForkSource::Run {
        thread_id: source_thread_id,
        run_id: source_run_id,
    }) = projection.thread.forked_from.clone()
    else {
        return Ok(with_local_visible_turn_items(projection));
    };
    if seen_thread_ids.contains(&source_thread_id) {
        return Ok(with_local_visible_turn_items(projection));
    }

    // A row-limited segment without turn anchors must finish paging locally
    // before inherited user turns can influence the page boundary.
    if let (Some(window), Some(local_window)) = (window, local_window.as_ref())
        && window.user_turn_limit.is_some()
        && local_window.row_limit > 0
        && projection.turn_items.len() as i64 >= local_window.row_limit
        && !projection
            .turn_items
            .iter()
            .any(is_thread_history_user_turn)
    {
        return Ok(with_local_visible_turn_items(projection));
    }

    let source_window = match window {
        None => None,
        Some(window) => Some(source_window(
            conn,
            thread_id,
            window,
            &source_thread_id,
            &source_run_id,
        )?),
    };

    let mut seen = seen_thread_ids.to_vec();
    seen.push(thread_id.clone());
    let source_projection =
        read_projection(conn, &source_thread_id, &seen, source_window.as_ref())?;
    let visible = build_visible_turn_items(&projection, Some(&source_projection));
    Ok(OrchestrationV2ThreadProjection {
        visible_turn_items: visible,
        ..projection
    })
}

/// The window to read the fork source with: its newest row at or before the
/// fork point (bounded by the anchor when the anchor lives in the source).
fn source_window(
    conn: &Connection,
    thread_id: &ThreadId,
    window: &ReadWindow,
    source_thread_id: &ThreadId,
    source_run_id: &RunId,
) -> Result<ReadWindow, ReadFailure> {
    let history_anchor = window.history_anchor.as_ref();
    let anchor_belongs_to_source =
        history_anchor.is_some_and(|anchor| &anchor.thread_id == source_thread_id);
    let anchor: Option<String> = query_map(
        conn,
        "WITH fork_run AS (
           SELECT ordinal
           FROM orchestration_v2_projection_runs
           WHERE thread_id = ?1
             AND run_id = ?2
           LIMIT 1
         ), fork_boundary AS (
           SELECT (
             SELECT item.ordinal
             FROM orchestration_v2_projection_turn_items AS item
             WHERE item.run_id = run.run_id
             ORDER BY item.ordinal DESC
             LIMIT 1
           ) AS ordinal
           FROM orchestration_v2_projection_runs AS run
           WHERE run.thread_id = ?1
             AND run.ordinal <= (SELECT ordinal FROM fork_run)
             AND EXISTS (
               SELECT 1
               FROM orchestration_v2_projection_turn_items AS item
               WHERE item.run_id = run.run_id
               LIMIT 1
             )
           ORDER BY run.ordinal DESC
           LIMIT 1
         ), effective_boundary AS (
           SELECT COALESCE(
             (SELECT ordinal FROM fork_boundary),
             (
               SELECT ordinal
               FROM orchestration_v2_projection_turn_items
               WHERE thread_id = ?1
                 AND run_id IS NULL
                 AND json_extract(payload_json, '$.historyOrigin') = 'v1_import'
               ORDER BY ordinal DESC, turn_item_id DESC
               LIMIT 1
             ),
             -1
           ) AS ordinal
         )
         SELECT turn_item_id
         FROM orchestration_v2_projection_turn_items
         WHERE thread_id = ?1
           AND ordinal <= COALESCE(
             (SELECT ordinal FROM effective_boundary),
             -1
           )
           AND (
             ?3 = 0
             OR ordinal <= COALESCE(
               (SELECT ordinal FROM orchestration_v2_projection_turn_items
                WHERE thread_id = ?1
                  AND turn_item_id = ?4
                LIMIT 1),
               (SELECT ordinal FROM effective_boundary),
               -1
             )
           )
         ORDER BY ordinal DESC, turn_item_id DESC
         LIMIT 1",
        params![
            source_thread_id.as_str(),
            source_run_id.as_str(),
            i64::from(anchor_belongs_to_source),
            opt_text(history_anchor.map(|anchor| anchor.item_id.as_str())),
        ],
        |row| row.get::<_, String>(0),
    )?
    .into_iter()
    .next();
    let anchor_is_in_descendant = history_anchor.is_some_and(|anchor| {
        &anchor.thread_id != thread_id && &anchor.thread_id != source_thread_id
    });
    let suppress_local = anchor.is_none() || anchor_is_in_descendant;
    Ok(ReadWindow {
        row_limit: window.row_limit,
        user_turn_limit: window.user_turn_limit,
        required_run_id: Some(source_run_id.clone()),
        suppress_local,
        anchor_item_id: if suppress_local {
            None
        } else {
            anchor.and_then(|id| TurnItemId::new(id).ok())
        },
        history_anchor: match history_anchor {
            Some(anchor) if &anchor.thread_id != thread_id => Some(anchor.clone()),
            _ => None,
        },
    })
}
