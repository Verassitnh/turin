//! The context reads of `ProjectionStore.ts`: the exact rows a provider,
//! runtime-response, checkpoint, or run-completion handler needs before it acts.
//! Each read names its upstream method in its doc comment. SQL is copied
//! verbatim; a read that also needed a thread check calls `read_thread`.

use rusqlite::{Connection, OptionalExtension, params_from_iter};
use turin_contracts::orchestration_v2::{
    OrchestrationV2AppThread, OrchestrationV2CheckpointScope, OrchestrationV2ContextHandoff,
    OrchestrationV2ContextTransfer, OrchestrationV2ConversationMessage,
    OrchestrationV2ExecutionNode, OrchestrationV2ProviderSession, OrchestrationV2ProviderThread,
    OrchestrationV2ProviderTurn, OrchestrationV2ResponseCapability, OrchestrationV2Run,
    OrchestrationV2RunAttempt, OrchestrationV2RuntimeRequest, OrchestrationV2Subagent,
    OrchestrationV2TurnItem,
};
use turin_contracts::{MessageId, ProviderThreadId, ProviderTurnId, RunAttemptId, ThreadId};

use crate::projection::error::ReadFailure;
use crate::projection::sql::{query_payload, query_payloads};
use crate::projection::store::control::runtime_request;
use crate::projection::store::read_thread;

/// What `getProviderControlContext` targets: the provider thread and turn, and
/// optionally one attempt and one message.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderControlTarget {
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: ProviderTurnId,
    pub attempt_id: Option<RunAttemptId>,
    pub message_id: Option<MessageId>,
}

/// `getProviderControlContext` result.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProviderControlContext {
    pub provider_thread: Option<OrchestrationV2ProviderThread>,
    pub provider_turn: Option<OrchestrationV2ProviderTurn>,
    pub attempt: Option<OrchestrationV2RunAttempt>,
    pub message: Option<OrchestrationV2ConversationMessage>,
    pub run: Option<OrchestrationV2Run>,
}

pub(crate) fn provider_control_context(
    conn: &Connection,
    thread_id: &ThreadId,
    target: &ProviderControlTarget,
) -> Result<ProviderControlContext, ReadFailure> {
    read_thread(conn, thread_id)?;
    // Match the full projection's ownership scope, but fetch only the target
    // records. Transcript items and fork ancestors are never read here.
    let provider_thread = query_payload(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_provider_threads
         WHERE provider_thread_id = ?1 AND (
           thread_id = ?2
           OR owner_node_id IN (SELECT node_id FROM orchestration_v2_projection_nodes WHERE thread_id = ?2)
           OR provider_thread_id IN (SELECT provider_thread_id FROM orchestration_v2_projection_subagents WHERE thread_id = ?2)
         )",
        rusqlite::params![target.provider_thread_id.as_str(), thread_id.as_str()],
    )?;
    let provider_turn: Option<OrchestrationV2ProviderTurn> = query_payload(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_provider_turns
         WHERE thread_id = ?1 AND provider_turn_id = ?2",
        rusqlite::params![thread_id.as_str(), target.provider_turn_id.as_str()],
    )?;
    let attempt = match &target.attempt_id {
        None => None,
        Some(attempt_id) => query_payload(
            conn,
            "SELECT payload_json FROM orchestration_v2_projection_run_attempts
             WHERE thread_id = ?1 AND attempt_id = ?2",
            rusqlite::params![thread_id.as_str(), attempt_id.as_str()],
        )?,
    };
    let message = match &target.message_id {
        None => None,
        Some(message_id) => query_payload(
            conn,
            "SELECT payload_json FROM orchestration_v2_projection_messages
             WHERE thread_id = ?1 AND message_id = ?2",
            rusqlite::params![thread_id.as_str(), message_id.as_str()],
        )?,
    };
    // The run that owns the turn's attempt, when the target names a message.
    let run = match (&target.message_id, &provider_turn) {
        (Some(_), Some(turn)) => query_payload(
            conn,
            "SELECT payload_json FROM orchestration_v2_projection_runs
             WHERE thread_id = ?1 AND json_extract(payload_json, '$.activeAttemptId') IS ?2
             ORDER BY ordinal ASC LIMIT 1",
            rusqlite::params![
                thread_id.as_str(),
                turn.run_attempt_id.as_ref().map(|id| id.as_str())
            ],
        )?,
        _ => None,
    };
    Ok(ProviderControlContext {
        provider_thread,
        provider_turn,
        attempt,
        message,
        run,
    })
}

/// `getRunningTurnContext` result.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RunningTurnContext {
    pub run: Option<OrchestrationV2Run>,
    pub provider_thread: Option<OrchestrationV2ProviderThread>,
    pub provider_turn: Option<OrchestrationV2ProviderTurn>,
}

pub(crate) fn running_turn_context(
    conn: &Connection,
    thread_id: &ThreadId,
) -> Result<RunningTurnContext, ReadFailure> {
    read_thread(conn, thread_id)?;
    let run: Option<OrchestrationV2Run> = query_payload(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_runs
         WHERE thread_id = ?1 AND status = 'running' ORDER BY ordinal ASC LIMIT 1",
        [thread_id.as_str()],
    )?;
    let Some(run) = run else {
        return Ok(RunningTurnContext::default());
    };
    let provider_thread = query_payload(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_provider_threads
         WHERE provider_thread_id = ?1",
        rusqlite::params![run.provider_thread_id.as_ref().map(|id| id.as_str())],
    )?;
    let provider_turn = query_payload(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_provider_turns
         WHERE provider_thread_id = ?1
           AND run_attempt_id = ?2
           AND node_id = ?3
           AND status = 'running'",
        rusqlite::params![
            run.provider_thread_id.as_ref().map(|id| id.as_str()),
            run.active_attempt_id.as_ref().map(|id| id.as_str()),
            run.root_node_id.as_ref().map(|id| id.as_str()),
        ],
    )?;
    Ok(RunningTurnContext {
        run: Some(run),
        provider_thread,
        provider_turn,
    })
}

/// `getThreadProviderContext` result.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadProviderContext {
    pub thread: OrchestrationV2AppThread,
    pub provider_sessions: Vec<OrchestrationV2ProviderSession>,
    pub provider_threads: Vec<OrchestrationV2ProviderThread>,
}

/// `getThreadProviderContext`: the thread's sessions, and, when a provider
/// instance is named, its active provider thread plus the newest unowned one for
/// that instance. Errors if the thread does not exist.
pub(crate) fn thread_provider_context(
    conn: &Connection,
    thread_id: &ThreadId,
    target_instance_id: Option<&str>,
) -> Result<ThreadProviderContext, ReadFailure> {
    let thread = read_thread(conn, thread_id)?;
    let provider_sessions = query_payloads(
        conn,
        "SELECT session.payload_json FROM orchestration_v2_projection_provider_sessions AS session
         JOIN orchestration_v2_projection_provider_session_bindings AS binding
           ON binding.provider_session_id = session.provider_session_id
         WHERE binding.thread_id = ?1
         ORDER BY session.updated_at ASC, session.provider_session_id ASC",
        [thread_id.as_str()],
    )?;
    let provider_threads = match target_instance_id {
        None => Vec::new(),
        Some(instance_id) => query_payloads(
            conn,
            "SELECT payload_json FROM orchestration_v2_projection_provider_threads
             WHERE provider_thread_id = ?1
                OR provider_thread_id = (
                  SELECT provider_thread_id FROM orchestration_v2_projection_provider_threads
                  WHERE thread_id = ?2 AND owner_node_id IS NULL
                    AND provider_instance_id = ?3
                  ORDER BY updated_at DESC, provider_thread_id ASC LIMIT 1
                )
             ORDER BY updated_at ASC, provider_thread_id ASC",
            rusqlite::params![
                thread
                    .active_provider_thread_id
                    .as_ref()
                    .map(|id| id.as_str()),
                thread_id.as_str(),
                instance_id,
            ],
        )?,
    };
    Ok(ThreadProviderContext {
        thread,
        provider_sessions,
        provider_threads,
    })
}

/// `getRuntimeResponseContext` result.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RuntimeResponseContext {
    pub request: Option<OrchestrationV2RuntimeRequest>,
    pub node: Option<OrchestrationV2ExecutionNode>,
    pub item: Option<OrchestrationV2TurnItem>,
    pub session: Option<OrchestrationV2ProviderSession>,
}

pub(crate) fn runtime_response_context(
    conn: &Connection,
    thread_id: &ThreadId,
    request_id: &str,
) -> Result<RuntimeResponseContext, ReadFailure> {
    read_thread(conn, thread_id)?;
    let Some(request) = runtime_request(conn, thread_id, request_id)? else {
        return Ok(RuntimeResponseContext::default());
    };
    let node = query_payload(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_nodes
         WHERE thread_id = ?1 AND node_id = ?2",
        rusqlite::params![thread_id.as_str(), request.node_id.as_str()],
    )?;
    let item = query_payload(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_turn_items
         WHERE thread_id = ?1 AND node_id = ?2 AND type IN ('approval_request', 'user_input_request')
           AND json_extract(payload_json, '$.requestId') = ?3
         ORDER BY ordinal ASC LIMIT 1",
        rusqlite::params![thread_id.as_str(), request.node_id.as_str(), request_id],
    )?;
    let session = match &request.response_capability {
        OrchestrationV2ResponseCapability::Live { provider_session_id } => query_payload(
            conn,
            "SELECT session.payload_json FROM orchestration_v2_projection_provider_sessions AS session
             JOIN orchestration_v2_projection_provider_session_bindings AS binding
               ON binding.provider_session_id = session.provider_session_id
             WHERE binding.thread_id = ?1 AND session.provider_session_id = ?2",
            rusqlite::params![thread_id.as_str(), provider_session_id.as_str()],
        )?,
        _ => None,
    };
    Ok(RuntimeResponseContext {
        request: Some(request),
        node,
        item,
        session,
    })
}

/// `getRunMessage`: the user message that opened a run, `None` when absent.
pub(crate) fn run_message(
    conn: &Connection,
    thread_id: &ThreadId,
    run_id: &str,
) -> Result<Option<OrchestrationV2ConversationMessage>, ReadFailure> {
    read_thread(conn, thread_id)?;
    Ok(query_payload(
        conn,
        "SELECT message.payload_json FROM orchestration_v2_projection_messages AS message
         JOIN orchestration_v2_projection_runs AS run
           ON message.message_id = json_extract(run.payload_json, '$.userMessageId')
         WHERE run.thread_id = ?1 AND run.run_id = ?2
           AND message.thread_id = ?1",
        rusqlite::params![thread_id.as_str(), run_id],
    )?)
}

/// `canStartQueuedRun`: an unarchived, undeleted thread with a queued run and no
/// active run and no held queue. Errors if the thread does not exist.
pub(crate) fn can_start_queued_run(
    conn: &Connection,
    thread_id: &ThreadId,
) -> Result<bool, ReadFailure> {
    read_thread(conn, thread_id)?;
    let ready: Option<i64> = conn
        .query_row(
            "SELECT 1 AS ready FROM orchestration_v2_projection_threads AS thread
             WHERE thread.thread_id = ?1 AND thread.archived_at IS NULL AND thread.deleted_at IS NULL
               AND EXISTS (SELECT 1 FROM orchestration_v2_projection_runs
                 WHERE thread_id = ?1 AND status = 'queued')
               AND NOT EXISTS (SELECT 1 FROM orchestration_v2_projection_runs
                 WHERE thread_id = ?1
                   AND (status IN ('preparing', 'starting', 'running', 'waiting')
                     OR (status = 'queued' AND json_extract(payload_json, '$.queueHeld') = 1)))",
            [thread_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(ReadFailure::from)?;
    Ok(ready.is_some())
}

/// `hasUnpairedRunInterruptRequest`: the request item exists and its result does not.
pub(crate) fn has_unpaired_run_interrupt_request(
    conn: &Connection,
    thread_id: &ThreadId,
    request_id: &str,
    result_id: &str,
) -> Result<bool, ReadFailure> {
    let pending: i64 = conn.query_row(
        "SELECT EXISTS (
           SELECT 1 FROM orchestration_v2_projection_turn_items
           WHERE thread_id = ?1 AND turn_item_id = ?2
         ) AND NOT EXISTS (
           SELECT 1 FROM orchestration_v2_projection_turn_items
           WHERE thread_id = ?1 AND turn_item_id = ?3
         ) AS pending",
        rusqlite::params![thread_id.as_str(), request_id, result_id],
        |row| row.get(0),
    )?;
    Ok(pending == 1)
}

/// `isNodeStored`: the stored node payload is byte-identical to the given node's
/// serialization. Upstream compares with its schema encoding; serde writes the
/// same field order on both sides because both come from this crate.
pub(crate) fn is_node_stored(
    conn: &Connection,
    node: &OrchestrationV2ExecutionNode,
) -> Result<bool, ReadFailure> {
    let payload_json = serde_json::to_string(node)?;
    let stored: Option<String> = conn
        .query_row(
            "SELECT payload_json FROM orchestration_v2_projection_nodes WHERE node_id = ?1",
            [node.id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(stored.as_deref() == Some(payload_json.as_str()))
}

/// `getCheckpointContext` run row: the metadata columns only, so large run JSON
/// stays in SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointContextRun {
    pub id: String,
    pub ordinal: i64,
    pub status: String,
}

/// `getCheckpointContext` scope row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointContextScope {
    pub id: String,
    pub run_id: Option<String>,
    pub kind: String,
    pub cwd: Option<String>,
}

/// `getCheckpointContext` checkpoint row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointContextCheckpoint {
    pub scope_id: String,
    pub run_id: Option<String>,
    pub app_run_ordinal: Option<i64>,
    pub status: String,
    /// `$.ref` as JSON text.
    pub reference: Option<String>,
}

/// `getCheckpointContext` result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CheckpointContext {
    pub runs: Vec<CheckpointContextRun>,
    pub checkpoint_scopes: Vec<CheckpointContextScope>,
    pub checkpoints: Vec<CheckpointContextCheckpoint>,
}

/// `getCheckpointContext`. Errors if the thread does not exist.
pub(crate) fn checkpoint_context(
    conn: &Connection,
    thread_id: &ThreadId,
) -> Result<CheckpointContext, ReadFailure> {
    read_thread(conn, thread_id)?;
    let runs = {
        let mut statement = conn.prepare(
            "SELECT run_id AS id, ordinal, status
             FROM orchestration_v2_projection_runs
             WHERE thread_id = ?1
             ORDER BY ordinal ASC",
        )?;
        statement
            .query_map([thread_id.as_str()], |row| {
                Ok(CheckpointContextRun {
                    id: row.get(0)?,
                    ordinal: row.get(1)?,
                    status: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let checkpoint_scopes = {
        let mut statement = conn.prepare(
            "SELECT scope_id AS id, run_id AS run_id, kind,
               json_extract(payload_json, '$.cwd') AS cwd
             FROM orchestration_v2_projection_checkpoint_scopes
             WHERE thread_id = ?1
             ORDER BY ordinal_within_parent ASC, scope_id ASC",
        )?;
        statement
            .query_map([thread_id.as_str()], |row| {
                Ok(CheckpointContextScope {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    kind: row.get(2)?,
                    cwd: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let checkpoints = {
        let mut statement = conn.prepare(
            "SELECT scope_id AS scope_id, run_id AS run_id,
               app_run_ordinal AS app_run_ordinal, status,
               json_extract(payload_json, '$.ref') AS reference
             FROM orchestration_v2_projection_checkpoints
             WHERE thread_id = ?1
             ORDER BY scope_id ASC, ordinal_within_scope ASC",
        )?;
        statement
            .query_map([thread_id.as_str()], |row| {
                Ok(CheckpointContextCheckpoint {
                    scope_id: row.get(0)?,
                    run_id: row.get(1)?,
                    app_run_ordinal: row.get(2)?,
                    status: row.get(3)?,
                    reference: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(CheckpointContext {
        runs,
        checkpoint_scopes,
        checkpoints,
    })
}

/// `getCheckpointCaptureContext` result.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CheckpointCaptureContext {
    pub run: Option<OrchestrationV2Run>,
    pub root_node: Option<OrchestrationV2ExecutionNode>,
    pub scope: Option<OrchestrationV2CheckpointScope>,
    pub provider_thread: Option<OrchestrationV2ProviderThread>,
    pub ready_checkpoint_ordinals: Vec<i64>,
}

pub(crate) fn checkpoint_capture_context(
    conn: &Connection,
    thread_id: &ThreadId,
    run_id: &str,
    scope_id: &str,
) -> Result<CheckpointCaptureContext, ReadFailure> {
    read_thread(conn, thread_id)?;
    let run: Option<OrchestrationV2Run> = query_payload(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_runs
         WHERE thread_id = ?1 AND run_id = ?2",
        rusqlite::params![thread_id.as_str(), run_id],
    )?;
    let root_node = query_payload(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_nodes
         WHERE thread_id = ?1 AND node_id = ?2",
        rusqlite::params![
            thread_id.as_str(),
            run.as_ref()
                .and_then(|run| run.root_node_id.as_ref())
                .map(|id| id.as_str()),
        ],
    )?;
    let scope = query_payload(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_checkpoint_scopes
         WHERE thread_id = ?1 AND scope_id = ?2",
        rusqlite::params![thread_id.as_str(), scope_id],
    )?;
    let provider_thread = query_payload(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_provider_threads
         WHERE provider_thread_id = ?2
           AND (thread_id = ?1
             OR owner_node_id IN (SELECT node_id FROM orchestration_v2_projection_nodes WHERE thread_id = ?1)
             OR provider_thread_id IN (SELECT provider_thread_id FROM orchestration_v2_projection_subagents WHERE thread_id = ?1))",
        rusqlite::params![
            thread_id.as_str(),
            run.as_ref()
                .and_then(|run| run.provider_thread_id.as_ref())
                .map(|id| id.as_str()),
        ],
    )?;
    let ready_checkpoint_ordinals = {
        let mut statement = conn.prepare(
            "SELECT ordinal_within_scope AS ordinal
             FROM orchestration_v2_projection_checkpoints
             WHERE thread_id = ?1 AND scope_id = ?2 AND status = 'ready'",
        )?;
        statement
            .query_map(rusqlite::params![thread_id.as_str(), scope_id], |row| {
                row.get(0)
            })?
            .collect::<Result<Vec<i64>, _>>()?
    };
    Ok(CheckpointCaptureContext {
        run,
        root_node,
        scope,
        provider_thread,
        ready_checkpoint_ordinals,
    })
}

/// JavaScript `String.prototype.trim` whitespace, for SQLite `trim(x, chars)`.
/// Compact-command recognition must trim the way upstream does.
const JS_TRIM_WHITESPACE: &str = " \t\n\r\u{0b}\u{0c}\u{a0}\u{1680}\u{2000}\u{2001}\u{2002}\u{2003}\u{2004}\u{2005}\u{2006}\u{2007}\u{2008}\u{2009}\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}";

/// `getTurnStartContext` result: everything a turn start reads, scoped to one run.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnStartContext {
    pub has_conversation: bool,
    pub thread: OrchestrationV2AppThread,
    pub runs: Vec<OrchestrationV2Run>,
    pub attempts: Vec<OrchestrationV2RunAttempt>,
    pub nodes: Vec<OrchestrationV2ExecutionNode>,
    pub subagents: Vec<OrchestrationV2Subagent>,
    pub provider_sessions: Vec<OrchestrationV2ProviderSession>,
    pub provider_threads: Vec<OrchestrationV2ProviderThread>,
    pub provider_turns: Vec<OrchestrationV2ProviderTurn>,
    pub messages: Vec<OrchestrationV2ConversationMessage>,
    pub checkpoint_scopes: Vec<OrchestrationV2CheckpointScope>,
    pub context_handoffs: Vec<OrchestrationV2ContextHandoff>,
    pub context_transfers: Vec<OrchestrationV2ContextTransfer>,
    pub turn_items: Vec<OrchestrationV2TurnItem>,
}

/// `getTurnStartContext`. Errors with `ThreadNotFound` when the thread is missing.
pub(crate) fn turn_start_context(
    conn: &Connection,
    thread_id: &ThreadId,
    run_id: &str,
) -> Result<TurnStartContext, ReadFailure> {
    let thread = read_thread(conn, thread_id)?;
    let thread_param = thread_id.as_str();
    let runs = query_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_runs
         WHERE thread_id = ?1 ORDER BY ordinal ASC",
        [thread_param],
    )?;
    let attempts = query_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_run_attempts
         WHERE thread_id = ?1 ORDER BY run_id ASC, attempt_ordinal ASC",
        [thread_param],
    )?;
    let nodes: Vec<OrchestrationV2ExecutionNode> = query_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_nodes
         WHERE thread_id = ?1 AND node_id IN (
           SELECT json_extract(payload_json, '$.rootNodeId') FROM orchestration_v2_projection_runs
           WHERE thread_id = ?1 AND run_id = ?2
         ) ORDER BY node_id ASC",
        rusqlite::params![thread_param, run_id],
    )?;
    let subagents = query_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_subagents
         WHERE thread_id = ?1 AND status NOT IN ('interrupted','failed','cancelled')
         ORDER BY subagent_id ASC",
        [thread_param],
    )?;
    let provider_threads = query_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_provider_threads
         WHERE thread_id = ?1 ORDER BY COALESCE(first_run_ordinal, 0), provider_thread_id ASC",
        [thread_param],
    )?;
    let provider_turns = query_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_provider_turns
         WHERE thread_id = ?1 ORDER BY provider_thread_id ASC, ordinal ASC",
        [thread_param],
    )?;

    // Scopes are fetched for the nodes that reference one; an empty list matches nothing.
    let scope_ids: Vec<String> = nodes
        .iter()
        .filter_map(|node| {
            node.checkpoint_scope_id
                .as_ref()
                .map(|id| id.as_str().to_owned())
        })
        .collect();
    let scope_sql = format!(
        "SELECT payload_json FROM orchestration_v2_projection_checkpoint_scopes
         WHERE thread_id = ?1 AND scope_id IN ({})
         ORDER BY ordinal_within_parent ASC, scope_id ASC",
        numbered_placeholders(2, scope_ids.len())
    );
    let mut scope_params: Vec<String> = vec![thread_id.as_str().to_owned()];
    scope_params.extend(scope_ids);
    let checkpoint_scopes =
        query_payloads(conn, &scope_sql, params_from_iter(scope_params.iter()))?;

    let context_handoffs = query_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_context_handoffs
         WHERE thread_id = ?1 ORDER BY rowid ASC",
        [thread_param],
    )?;
    let context_transfers = query_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_context_transfers
         WHERE source_thread_id = ?1 OR target_thread_id = ?1 ORDER BY rowid ASC",
        [thread_param],
    )?;
    let turn_items = query_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_turn_items
         WHERE thread_id = ?1 AND run_id = ?2 ORDER BY ordinal ASC, turn_item_id ASC",
        rusqlite::params![thread_param, run_id],
    )?;
    let provider_sessions = query_payloads(
        conn,
        "SELECT sessions.payload_json FROM orchestration_v2_projection_provider_sessions AS sessions
         INNER JOIN orchestration_v2_projection_provider_session_bindings AS bindings
           ON bindings.provider_session_id = sessions.provider_session_id
         WHERE bindings.thread_id = ?1
         ORDER BY sessions.updated_at ASC, sessions.provider_session_id ASC",
        [thread_param],
    )?;
    // Compact retries need their original markers; emptiness is an existence check.
    let messages = query_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_messages
         WHERE thread_id = ?1 AND (
           message_id IN (SELECT json_extract(payload_json, '$.userMessageId') FROM orchestration_v2_projection_runs
             WHERE thread_id = ?1 AND run_id = ?2)
           OR (role = 'user' AND lower(trim(json_extract(payload_json, '$.text'), ?3)) = '/compact')
         ) ORDER BY created_at ASC, message_id ASC",
        rusqlite::params![thread_param, run_id, JS_TRIM_WHITESPACE],
    )?;
    let has_conversation: i64 = conn.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM orchestration_v2_projection_messages
           WHERE thread_id = ?1 AND role = 'user'
             AND (lower(trim(json_extract(payload_json, '$.text'), ?2)) <> '/compact'
               OR json_array_length(payload_json, '$.attachments') > 0)
         ) AS present",
        rusqlite::params![thread_param, JS_TRIM_WHITESPACE],
        |row| row.get(0),
    )?;

    Ok(TurnStartContext {
        has_conversation: has_conversation == 1,
        thread,
        runs,
        attempts,
        nodes,
        subagents,
        provider_sessions,
        provider_threads,
        provider_turns,
        messages,
        checkpoint_scopes,
        context_handoffs,
        context_transfers,
        turn_items,
    })
}

/// `?first` … `?first+count-1`, for an `IN (…)` list after `first - 1` bound values.
fn numbered_placeholders(first: usize, count: usize) -> String {
    (first..first + count)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ")
}
