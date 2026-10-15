//! The write side of `ProjectionStore.ts` (`apply`): fold one domain event
//! into the normalized projection tables.
//!
//! **Pipeline seam.** The event sink calls [`apply_event`] with the
//! transaction that appends the event, in the same `Db::write` closure, and
//! publishes the committed event only after that closure returns. This module
//! never publishes and never awaits.

use rusqlite::{Connection, params};
use turin_contracts::orchestration_v2::{
    OrchestrationV2AppThread, OrchestrationV2DomainEvent as E, OrchestrationV2ProviderThread,
    OrchestrationV2ProviderTurn,
};
use turin_contracts::{ThreadId, UtcDateTime};

use crate::projection::error::{Cause, ProjectionStoreError};
use crate::projection::event::{event_occurred_at, event_thread_id, event_type};
use crate::projection::shared::thread_pull_requests::apply_thread_pull_request_link_update;
use crate::projection::sql::{CauseResult, boolean_int, execute, query_payload, text};

/// A future queued provider has a reserved thread record but is not active until delivery.
pub fn is_queued_provider_thread_placeholder(
    provider_thread: &OrchestrationV2ProviderThread,
) -> bool {
    use turin_contracts::orchestration_v2::OrchestrationV2ProviderThreadStatus as S;
    provider_thread.status == S::NotLoaded
        && provider_thread.first_run_ordinal.is_none()
        && provider_thread.native_thread_ref.is_none()
        && provider_thread.provider_session_id.is_none()
}

/// `upsertProviderTurn` for one turn: a snapshot without `tokenUsage` keeps the
/// usage another event recorded.
pub fn merge_provider_turn(
    current: Option<&OrchestrationV2ProviderTurn>,
    next: &OrchestrationV2ProviderTurn,
) -> OrchestrationV2ProviderTurn {
    let mut merged = next.clone();
    if merged.token_usage.is_none() {
        merged.token_usage = current.and_then(|turn| turn.token_usage.clone());
    }
    merged
}

/// Fold `event` into the projection tables using the caller's transaction.
///
/// Errors carry the event type like upstream's `ProjectionStoreApplyEventError`.
pub fn apply_event(conn: &Connection, event: &E) -> Result<(), ProjectionStoreError> {
    apply_inner(conn, event).map_err(|cause| ProjectionStoreError::ApplyEvent {
        event_type: event_type(event).to_owned(),
        cause,
    })
}

/// SQL for run upserts: a snapshot without `path` keeps the value another
/// event recorded there.
fn keep_recorded_run_field(payload: &str, path: &str) -> String {
    format!(
        "CASE
            WHEN json_type(excluded.payload_json, '{path}') IS NULL
              AND json_type(orchestration_v2_projection_runs.payload_json, '{path}') IS NOT NULL
            THEN json_set(
              {payload},
              '{path}',
              json_extract(orchestration_v2_projection_runs.payload_json, '{path}')
            )
            ELSE {payload}
          END"
    )
}

fn thread_payload(
    conn: &Connection,
    thread_id: &ThreadId,
) -> CauseResult<Option<OrchestrationV2AppThread>> {
    query_payload(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_threads WHERE thread_id = ?1 LIMIT 1",
        [thread_id.as_str()],
    )
}

fn apply_inner(conn: &Connection, event: &E) -> CauseResult<()> {
    match event {
        E::ThreadPullRequestLinkUpdated {
            thread_id, payload, ..
        } => {
            // Upstream re-applies this as `thread.pull-request-synced` with the
            // patched thread; the event then bumps `updatedAt` below.
            let thread = thread_payload(conn, thread_id)?.ok_or_else(|| -> Cause {
                ProjectionStoreError::ThreadNotFound {
                    thread_id: thread_id.clone(),
                }
                .into()
            })?;
            upsert_thread(
                conn,
                &apply_thread_pull_request_link_update(&thread, payload),
            )?;
        }
        E::ThreadCreated { payload, .. }
        | E::ThreadArchived { payload, .. }
        | E::ThreadUnarchived { payload, .. }
        | E::ThreadDeleted { payload, .. }
        | E::ThreadSettled { payload, .. }
        | E::ThreadUnsettled { payload, .. }
        | E::ThreadSnoozed { payload, .. }
        | E::ThreadUnsnoozed { payload, .. }
        | E::ThreadAutoSettleSet { payload, .. }
        | E::ThreadPinned { payload, .. }
        | E::ThreadUnpinned { payload, .. }
        | E::ThreadPinReordered { payload, .. }
        | E::ThreadActiveReordered { payload, .. }
        | E::ThreadVisited { payload, .. }
        | E::ThreadMarkedUnread { payload, .. }
        | E::ThreadMetadataUpdated { payload, .. }
        | E::ThreadPullRequestSynced { payload, .. }
        | E::ThreadRuntimeModeUpdated { payload, .. }
        | E::ThreadInteractionModeUpdated { payload, .. }
        | E::ThreadModelSelectionUpdated { payload, .. }
        | E::ThreadProviderSwitched { payload, .. } => upsert_thread(conn, payload)?,
        E::RunCreated { payload, .. } | E::RunUpdated { payload, .. } => {
            let payload_json = serde_json::to_string(payload)?;
            let kept = keep_recorded_run_field(
                &keep_recorded_run_field("excluded.payload_json", "$.delegatedCompletion"),
                "$.restartCancelledBackgroundWork",
            );
            execute(
                conn,
                &format!(
                    "INSERT INTO orchestration_v2_projection_runs (
                        run_id, thread_id, ordinal, provider, provider_instance_id,
                        provider_thread_id, status, requested_at, completed_at, payload_json
                    )
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                    ON CONFLICT(run_id)
                    DO UPDATE SET
                        thread_id = excluded.thread_id,
                        ordinal = excluded.ordinal,
                        provider = excluded.provider,
                        provider_instance_id = excluded.provider_instance_id,
                        provider_thread_id = excluded.provider_thread_id,
                        status = excluded.status,
                        requested_at = excluded.requested_at,
                        completed_at = excluded.completed_at,
                        payload_json = {kept}"
                ),
                params![
                    payload.id.as_str(),
                    payload.thread_id.as_str(),
                    payload.ordinal.get() as i64,
                    payload.provider_instance_id.as_str(),
                    payload.provider_instance_id.as_str(),
                    payload.provider_thread_id.as_ref().map(|id| id.as_str()),
                    text(&payload.status),
                    payload.requested_at.to_string(),
                    payload.completed_at.map(|at| at.to_string()),
                    payload_json,
                ],
            )?;
        }
        E::RunBackgroundWorkCancelled {
            thread_id, payload, ..
        } => {
            // Only this field changes, so a concurrent lifecycle write is never regressed.
            let work_json = serde_json::to_string(&payload.restart_cancelled_background_work)?;
            execute(
                conn,
                "UPDATE orchestration_v2_projection_runs
                 SET payload_json = json_set(
                   payload_json,
                   '$.restartCancelledBackgroundWork',
                   json(?1)
                 )
                 WHERE run_id = ?2 AND thread_id = ?3",
                params![work_json, payload.run_id.as_str(), thread_id.as_str()],
            )?;
        }
        E::RunAttemptCreated {
            thread_id, payload, ..
        }
        | E::RunAttemptUpdated {
            thread_id, payload, ..
        } => {
            execute(
                conn,
                "INSERT INTO orchestration_v2_projection_run_attempts (
                    attempt_id, thread_id, run_id, attempt_ordinal, root_node_id, provider,
                    provider_instance_id, provider_thread_id, provider_turn_id, status, payload_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ON CONFLICT(attempt_id)
                DO UPDATE SET
                    thread_id = excluded.thread_id,
                    run_id = excluded.run_id,
                    attempt_ordinal = excluded.attempt_ordinal,
                    root_node_id = excluded.root_node_id,
                    provider = excluded.provider,
                    provider_instance_id = excluded.provider_instance_id,
                    provider_thread_id = excluded.provider_thread_id,
                    provider_turn_id = excluded.provider_turn_id,
                    status = excluded.status,
                    payload_json = excluded.payload_json",
                params![
                    payload.id.as_str(),
                    thread_id.as_str(),
                    payload.run_id.as_str(),
                    payload.attempt_ordinal.get() as i64,
                    payload.root_node_id.as_str(),
                    payload.provider_instance_id.as_str(),
                    payload.provider_instance_id.as_str(),
                    payload.provider_thread_id.as_str(),
                    payload.provider_turn_id.as_ref().map(|id| id.as_str()),
                    text(&payload.status),
                    serde_json::to_string(payload)?,
                ],
            )?;
        }
        E::NodeUpdated { payload, .. } => {
            execute(
                conn,
                "INSERT INTO orchestration_v2_projection_nodes (
                    node_id, thread_id, run_id, parent_node_id, root_node_id, kind, status,
                    provider_thread_id, provider_turn_id, runtime_request_id, checkpoint_scope_id,
                    started_at, completed_at, payload_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                ON CONFLICT(node_id)
                DO UPDATE SET
                    thread_id = excluded.thread_id,
                    run_id = excluded.run_id,
                    parent_node_id = excluded.parent_node_id,
                    root_node_id = excluded.root_node_id,
                    kind = excluded.kind,
                    status = excluded.status,
                    provider_thread_id = excluded.provider_thread_id,
                    provider_turn_id = excluded.provider_turn_id,
                    runtime_request_id = excluded.runtime_request_id,
                    checkpoint_scope_id = excluded.checkpoint_scope_id,
                    started_at = excluded.started_at,
                    completed_at = excluded.completed_at,
                    payload_json = excluded.payload_json",
                params![
                    payload.id.as_str(),
                    payload.thread_id.as_str(),
                    payload.run_id.as_ref().map(|id| id.as_str()),
                    payload.parent_node_id.as_ref().map(|id| id.as_str()),
                    payload.root_node_id.as_str(),
                    text(&payload.kind),
                    text(&payload.status),
                    payload.provider_thread_id.as_ref().map(|id| id.as_str()),
                    payload.provider_turn_id.as_ref().map(|id| id.as_str()),
                    payload.runtime_request_id.as_ref().map(|id| id.as_str()),
                    payload.checkpoint_scope_id.as_ref().map(|id| id.as_str()),
                    payload.started_at.map(|at| at.to_string()),
                    payload.completed_at.map(|at| at.to_string()),
                    serde_json::to_string(payload)?,
                ],
            )?;
        }
        E::SubagentUpdated { payload, .. } => {
            execute(
                conn,
                "INSERT INTO orchestration_v2_projection_subagents (
                    subagent_id, thread_id, run_id, parent_node_id, provider, driver,
                    provider_instance_id, provider_thread_id, child_thread_id, origin, status,
                    started_at, completed_at, updated_at, payload_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                ON CONFLICT(subagent_id)
                DO UPDATE SET
                    thread_id = excluded.thread_id,
                    run_id = excluded.run_id,
                    parent_node_id = excluded.parent_node_id,
                    provider = excluded.provider,
                    driver = excluded.driver,
                    provider_instance_id = excluded.provider_instance_id,
                    provider_thread_id = excluded.provider_thread_id,
                    child_thread_id = excluded.child_thread_id,
                    origin = excluded.origin,
                    status = excluded.status,
                    started_at = excluded.started_at,
                    completed_at = excluded.completed_at,
                    updated_at = excluded.updated_at,
                    payload_json = CASE
                      WHEN json_type(excluded.payload_json, '$.completionDelivery') IS NULL
                        AND json_type(orchestration_v2_projection_subagents.payload_json, '$.completionDelivery') IS NOT NULL
                      THEN json_set(
                        excluded.payload_json,
                        '$.completionDelivery',
                        json_extract(
                          orchestration_v2_projection_subagents.payload_json,
                          '$.completionDelivery'
                        )
                      )
                      ELSE excluded.payload_json
                    END",
                params![
                    payload.id.as_str(),
                    payload.thread_id.as_str(),
                    payload.run_id.as_ref().map(|id| id.as_str()),
                    payload.parent_node_id.as_str(),
                    payload.provider_instance_id.as_str(),
                    payload.driver.as_str(),
                    payload.provider_instance_id.as_str(),
                    payload.provider_thread_id.as_ref().map(|id| id.as_str()),
                    payload.child_thread_id.as_ref().map(|id| id.as_str()),
                    text(&payload.origin),
                    text(&payload.status),
                    payload.started_at.map(|at| at.to_string()),
                    payload.completed_at.map(|at| at.to_string()),
                    payload.updated_at.to_string(),
                    serde_json::to_string(payload)?,
                ],
            )?;
        }
        E::ProviderSessionAttached {
            thread_id, payload, ..
        }
        | E::ProviderSessionUpdated {
            thread_id, payload, ..
        } => {
            execute(
                conn,
                "INSERT INTO orchestration_v2_projection_provider_sessions (
                    provider_session_id, thread_id, provider, driver, provider_instance_id,
                    status, model, updated_at, payload_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ON CONFLICT(provider_session_id)
                DO UPDATE SET
                    thread_id = excluded.thread_id,
                    provider = excluded.provider,
                    driver = excluded.driver,
                    provider_instance_id = excluded.provider_instance_id,
                    status = excluded.status,
                    model = excluded.model,
                    updated_at = excluded.updated_at,
                    payload_json = excluded.payload_json",
                params![
                    payload.id.as_str(),
                    thread_id.as_str(),
                    payload.provider_instance_id.as_str(),
                    payload.driver.as_str(),
                    payload.provider_instance_id.as_str(),
                    text(&payload.status),
                    payload.model.as_ref().map(|model| model.as_str()),
                    payload.updated_at.to_string(),
                    serde_json::to_string(payload)?,
                ],
            )?;
            if matches!(event, E::ProviderSessionAttached { .. }) {
                execute(
                    conn,
                    "INSERT OR IGNORE INTO orchestration_v2_projection_provider_session_bindings (
                        provider_session_id, thread_id
                    )
                    VALUES (?1, ?2)",
                    params![payload.id.as_str(), thread_id.as_str()],
                )?;
            }
        }
        E::ProviderSessionDetached {
            thread_id, payload, ..
        } => {
            execute(
                conn,
                "DELETE FROM orchestration_v2_projection_provider_session_bindings
                 WHERE provider_session_id = ?1 AND thread_id = ?2",
                params![payload.provider_session_id.as_str(), thread_id.as_str()],
            )?;
        }
        E::ProviderThreadUpdated { payload, .. } => apply_provider_thread(conn, payload)?,
        E::ProviderTurnUpdated {
            thread_id, payload, ..
        } => {
            let existing = if payload.token_usage.is_none() {
                query_payload::<OrchestrationV2ProviderTurn>(
                    conn,
                    "SELECT payload_json FROM orchestration_v2_projection_provider_turns
                     WHERE provider_turn_id = ?1 LIMIT 1",
                    [payload.id.as_str()],
                )?
            } else {
                None
            };
            let provider_turn = merge_provider_turn(existing.as_ref(), payload);
            execute(
                conn,
                "INSERT INTO orchestration_v2_projection_provider_turns (
                    provider_turn_id, thread_id, provider_thread_id, node_id, run_attempt_id,
                    ordinal, status, started_at, completed_at, payload_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                ON CONFLICT(provider_turn_id)
                DO UPDATE SET
                    thread_id = excluded.thread_id,
                    provider_thread_id = excluded.provider_thread_id,
                    node_id = excluded.node_id,
                    run_attempt_id = excluded.run_attempt_id,
                    ordinal = excluded.ordinal,
                    status = excluded.status,
                    started_at = excluded.started_at,
                    completed_at = excluded.completed_at,
                    payload_json = excluded.payload_json",
                params![
                    payload.id.as_str(),
                    thread_id.as_str(),
                    provider_turn.provider_thread_id.as_str(),
                    provider_turn.node_id.as_str(),
                    provider_turn.run_attempt_id.as_ref().map(|id| id.as_str()),
                    provider_turn.ordinal.get() as i64,
                    text(&provider_turn.status),
                    provider_turn.started_at.map(|at| at.to_string()),
                    provider_turn.completed_at.map(|at| at.to_string()),
                    serde_json::to_string(&provider_turn)?,
                ],
            )?;
        }
        E::RuntimeRequestUpdated {
            thread_id, payload, ..
        } => {
            execute(
                conn,
                "INSERT INTO orchestration_v2_projection_runtime_requests (
                    runtime_request_id, thread_id, node_id, provider_turn_id, kind, status,
                    created_at, resolved_at, payload_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ON CONFLICT(runtime_request_id)
                DO UPDATE SET
                    thread_id = excluded.thread_id,
                    node_id = excluded.node_id,
                    provider_turn_id = excluded.provider_turn_id,
                    kind = excluded.kind,
                    status = excluded.status,
                    created_at = excluded.created_at,
                    resolved_at = excluded.resolved_at,
                    payload_json = excluded.payload_json",
                params![
                    payload.id.as_str(),
                    thread_id.as_str(),
                    payload.node_id.as_str(),
                    payload.provider_turn_id.as_ref().map(|id| id.as_str()),
                    text(&payload.kind),
                    text(&payload.status),
                    payload.created_at.to_string(),
                    payload.resolved_at.map(|at| at.to_string()),
                    serde_json::to_string(payload)?,
                ],
            )?;
        }
        E::MessageUpdated { payload, .. } => {
            execute(
                conn,
                "INSERT INTO orchestration_v2_projection_messages (
                    message_id, thread_id, run_id, node_id, role, streaming, created_at,
                    updated_at, payload_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ON CONFLICT(message_id)
                DO UPDATE SET
                    thread_id = excluded.thread_id,
                    run_id = excluded.run_id,
                    node_id = excluded.node_id,
                    role = excluded.role,
                    streaming = excluded.streaming,
                    created_at = excluded.created_at,
                    updated_at = excluded.updated_at,
                    payload_json = excluded.payload_json",
                params![
                    payload.id.as_str(),
                    payload.thread_id.as_str(),
                    payload.run_id.as_ref().map(|id| id.as_str()),
                    payload.node_id.as_ref().map(|id| id.as_str()),
                    text(&payload.role),
                    boolean_int(payload.streaming),
                    payload.created_at.to_string(),
                    payload.updated_at.to_string(),
                    serde_json::to_string(payload)?,
                ],
            )?;
        }
        E::PlanUpdated { payload, .. } => {
            use turin_contracts::orchestration_v2::OrchestrationV2PlanArtifact as Plan;
            let (kind, id, thread_id, run_id, node_id, status) = match payload {
                Plan::ProposedPlan {
                    id,
                    thread_id,
                    run_id,
                    node_id,
                    status,
                    ..
                } => ("proposed_plan", id, thread_id, run_id, node_id, status),
                Plan::TodoList {
                    id,
                    thread_id,
                    run_id,
                    node_id,
                    status,
                    ..
                } => ("todo_list", id, thread_id, run_id, node_id, status),
            };
            execute(
                conn,
                "INSERT INTO orchestration_v2_projection_plans (
                    plan_id, thread_id, run_id, node_id, kind, status, payload_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(plan_id)
                DO UPDATE SET
                    thread_id = excluded.thread_id,
                    run_id = excluded.run_id,
                    node_id = excluded.node_id,
                    kind = excluded.kind,
                    status = excluded.status,
                    payload_json = excluded.payload_json",
                params![
                    id.as_str(),
                    thread_id.as_str(),
                    run_id.as_ref().map(|id| id.as_str()),
                    node_id.as_str(),
                    kind,
                    text(status),
                    serde_json::to_string(payload)?,
                ],
            )?;
        }
        E::TurnItemUpdated { payload, .. } => {
            let common = crate::projection::turn_item::common(payload);
            execute(
                conn,
                "INSERT INTO orchestration_v2_projection_turn_items (
                    turn_item_id, thread_id, run_id, node_id, provider_thread_id,
                    provider_turn_id, parent_item_id, ordinal, type, status, updated_at,
                    payload_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                ON CONFLICT(turn_item_id)
                DO UPDATE SET
                    thread_id = excluded.thread_id,
                    run_id = excluded.run_id,
                    node_id = excluded.node_id,
                    provider_thread_id = excluded.provider_thread_id,
                    provider_turn_id = excluded.provider_turn_id,
                    parent_item_id = excluded.parent_item_id,
                    ordinal = excluded.ordinal,
                    type = excluded.type,
                    status = excluded.status,
                    updated_at = excluded.updated_at,
                    payload_json = excluded.payload_json",
                params![
                    common.id.as_str(),
                    common.thread_id.as_str(),
                    common.run_id.map(|id| id.as_str()),
                    common.node_id.map(|id| id.as_str()),
                    common.provider_thread_id.map(|id| id.as_str()),
                    common.provider_turn_id.map(|id| id.as_str()),
                    common.parent_item_id.map(|id| id.as_str()),
                    common.ordinal as i64,
                    crate::projection::turn_item::type_tag(payload),
                    text(&common.status),
                    common.updated_at.to_string(),
                    serde_json::to_string(payload)?,
                ],
            )?;
        }
        E::CheckpointScopeCreated { payload, .. } => {
            execute(
                conn,
                "INSERT INTO orchestration_v2_projection_checkpoint_scopes (
                    scope_id, thread_id, run_id, node_id, parent_scope_id, provider_thread_id,
                    kind, ordinal_within_parent, advances_app_run_count, created_at, payload_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ON CONFLICT(scope_id)
                DO UPDATE SET
                    thread_id = excluded.thread_id,
                    run_id = excluded.run_id,
                    node_id = excluded.node_id,
                    parent_scope_id = excluded.parent_scope_id,
                    provider_thread_id = excluded.provider_thread_id,
                    kind = excluded.kind,
                    ordinal_within_parent = excluded.ordinal_within_parent,
                    advances_app_run_count = excluded.advances_app_run_count,
                    created_at = excluded.created_at,
                    payload_json = excluded.payload_json",
                params![
                    payload.id.as_str(),
                    payload.thread_id.as_str(),
                    payload.run_id.as_ref().map(|id| id.as_str()),
                    payload.node_id.as_str(),
                    payload.parent_scope_id.as_ref().map(|id| id.as_str()),
                    payload.provider_thread_id.as_ref().map(|id| id.as_str()),
                    text(&payload.kind),
                    payload.ordinal_within_parent as i64,
                    boolean_int(payload.advances_app_run_count),
                    payload.created_at.to_string(),
                    serde_json::to_string(payload)?,
                ],
            )?;
        }
        E::CheckpointCaptured { payload, .. } => {
            execute(
                conn,
                "INSERT INTO orchestration_v2_projection_checkpoints (
                    checkpoint_id, thread_id, scope_id, run_id, node_id, parent_checkpoint_id,
                    ordinal_within_scope, app_run_ordinal, status, captured_at, payload_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ON CONFLICT(checkpoint_id)
                DO UPDATE SET
                    thread_id = excluded.thread_id,
                    scope_id = excluded.scope_id,
                    run_id = excluded.run_id,
                    node_id = excluded.node_id,
                    parent_checkpoint_id = excluded.parent_checkpoint_id,
                    ordinal_within_scope = excluded.ordinal_within_scope,
                    app_run_ordinal = excluded.app_run_ordinal,
                    status = excluded.status,
                    captured_at = excluded.captured_at,
                    payload_json = excluded.payload_json",
                params![
                    payload.id.as_str(),
                    payload.thread_id.as_str(),
                    payload.scope_id.as_str(),
                    payload.run_id.as_ref().map(|id| id.as_str()),
                    payload.node_id.as_str(),
                    payload.parent_checkpoint_id.as_ref().map(|id| id.as_str()),
                    payload.ordinal_within_scope as i64,
                    payload.app_run_ordinal.map(|ordinal| ordinal.get() as i64),
                    text(&payload.status),
                    payload.captured_at.to_string(),
                    serde_json::to_string(payload)?,
                ],
            )?;
        }
        E::CheckpointRollbackRequested { .. } => {}
        E::ContextHandoffUpdated { payload, .. } => {
            execute(
                conn,
                "INSERT INTO orchestration_v2_projection_context_handoffs (
                    context_handoff_id, thread_id, target_run_id, to_provider_thread_id,
                    strategy, status, updated_at, payload_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                ON CONFLICT(context_handoff_id)
                DO UPDATE SET
                    thread_id = excluded.thread_id,
                    target_run_id = excluded.target_run_id,
                    to_provider_thread_id = excluded.to_provider_thread_id,
                    strategy = excluded.strategy,
                    status = excluded.status,
                    updated_at = excluded.updated_at,
                    payload_json = excluded.payload_json",
                params![
                    payload.id.as_str(),
                    payload.thread_id.as_str(),
                    payload.target_run_id.as_str(),
                    payload.to_provider_thread_id.as_str(),
                    text(&payload.strategy),
                    text(&payload.status),
                    payload.updated_at.to_string(),
                    serde_json::to_string(payload)?,
                ],
            )?;
        }
        E::ContextTransferCreated { payload, .. } | E::ContextTransferUpdated { payload, .. } => {
            execute(
                conn,
                "INSERT INTO orchestration_v2_projection_context_transfers (
                    context_transfer_id, source_thread_id, target_thread_id, target_run_id, type,
                    status, source_provider, target_provider, source_provider_instance_id,
                    target_provider_instance_id, updated_at, payload_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                ON CONFLICT(context_transfer_id)
                DO UPDATE SET
                    source_thread_id = excluded.source_thread_id,
                    target_thread_id = excluded.target_thread_id,
                    target_run_id = excluded.target_run_id,
                    type = excluded.type,
                    status = excluded.status,
                    source_provider = excluded.source_provider,
                    target_provider = excluded.target_provider,
                    source_provider_instance_id = excluded.source_provider_instance_id,
                    target_provider_instance_id = excluded.target_provider_instance_id,
                    updated_at = excluded.updated_at,
                    payload_json = excluded.payload_json",
                params![
                    payload.id.as_str(),
                    payload.source_thread_id.as_str(),
                    payload.target_thread_id.as_str(),
                    payload.target_run_id.as_ref().map(|id| id.as_str()),
                    text(&payload.r#type),
                    text(&payload.status),
                    payload
                        .source_provider_instance_id
                        .as_ref()
                        .map(|id| id.as_str()),
                    payload
                        .target_provider_instance_id
                        .as_ref()
                        .map(|id| id.as_str()),
                    payload
                        .source_provider_instance_id
                        .as_ref()
                        .map(|id| id.as_str()),
                    payload
                        .target_provider_instance_id
                        .as_ref()
                        .map(|id| id.as_str()),
                    payload.updated_at.to_string(),
                    serde_json::to_string(payload)?,
                ],
            )?;
        }
    }

    if bumps_thread_activity(event) {
        // Only the activity timestamp changes. This runs for every streamed
        // run, node and turn-item event, so the thread row is patched in place
        // instead of being read, decoded and re-encoded each time.
        bump_thread_updated_at(conn, event_thread_id(event), event_occurred_at(event))?;
    }
    Ok(())
}

/// Events that carry the whole thread keep its own `updatedAt`; everything else
/// (plus `thread.active-reordered`, `thread.pull-request-synced` and
/// `thread.pull-request-link-updated`, which upstream's exclusion list omits)
/// moves the thread's activity timestamp to the event time.
fn bumps_thread_activity(event: &E) -> bool {
    !matches!(
        event,
        E::ThreadCreated { .. }
            | E::ThreadArchived { .. }
            | E::ThreadUnarchived { .. }
            | E::ThreadDeleted { .. }
            | E::ThreadSettled { .. }
            | E::ThreadUnsettled { .. }
            | E::ThreadSnoozed { .. }
            | E::ThreadUnsnoozed { .. }
            | E::ThreadAutoSettleSet { .. }
            | E::ThreadPinned { .. }
            | E::ThreadUnpinned { .. }
            | E::ThreadPinReordered { .. }
            | E::ThreadVisited { .. }
            | E::ThreadMarkedUnread { .. }
            | E::ThreadMetadataUpdated { .. }
            | E::ThreadRuntimeModeUpdated { .. }
            | E::ThreadInteractionModeUpdated { .. }
            | E::ThreadModelSelectionUpdated { .. }
            | E::ThreadProviderSwitched { .. }
    )
}

fn bump_thread_updated_at(
    conn: &Connection,
    thread_id: &ThreadId,
    occurred_at: &UtcDateTime,
) -> CauseResult<()> {
    let updated_at = occurred_at.to_string();
    execute(
        conn,
        "UPDATE orchestration_v2_projection_threads
         SET updated_at = ?1, payload_json = json_set(payload_json, '$.updatedAt', ?1)
         WHERE thread_id = ?2",
        params![updated_at, thread_id.as_str()],
    )?;
    Ok(())
}

fn upsert_thread(conn: &Connection, thread: &OrchestrationV2AppThread) -> CauseResult<()> {
    execute(
        conn,
        "INSERT INTO orchestration_v2_projection_threads (
            thread_id, project_id, title, default_provider, provider_instance_id, runtime_mode,
            interaction_mode, active_provider_thread_id, created_at, updated_at, archived_at,
            deleted_at, payload_json
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        ON CONFLICT(thread_id)
        DO UPDATE SET
            project_id = excluded.project_id,
            title = excluded.title,
            default_provider = excluded.default_provider,
            provider_instance_id = excluded.provider_instance_id,
            runtime_mode = excluded.runtime_mode,
            interaction_mode = excluded.interaction_mode,
            active_provider_thread_id = excluded.active_provider_thread_id,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at,
            archived_at = excluded.archived_at,
            deleted_at = excluded.deleted_at,
            payload_json = excluded.payload_json",
        params![
            thread.id.as_str(),
            thread.project_id.as_str(),
            thread.title.as_str(),
            thread.provider_instance_id.as_str(),
            thread.provider_instance_id.as_str(),
            text(&thread.runtime_mode),
            text(&thread.interaction_mode),
            thread
                .active_provider_thread_id
                .as_ref()
                .map(|id| id.as_str()),
            thread.created_at.to_string(),
            thread.updated_at.to_string(),
            thread.archived_at.map(|at| at.to_string()),
            thread.deleted_at.map(|at| at.to_string()),
            serde_json::to_string(thread)?,
        ],
    )?;
    Ok(())
}

fn apply_provider_thread(
    conn: &Connection,
    payload: &OrchestrationV2ProviderThread,
) -> CauseResult<()> {
    execute(
        conn,
        "INSERT INTO orchestration_v2_projection_provider_threads (
            provider_thread_id, thread_id, owner_node_id, provider, driver, provider_instance_id,
            provider_session_id, status, first_run_ordinal, last_run_ordinal, updated_at,
            payload_json
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        ON CONFLICT(provider_thread_id)
        DO UPDATE SET
            thread_id = excluded.thread_id,
            owner_node_id = excluded.owner_node_id,
            provider = excluded.provider,
            driver = excluded.driver,
            provider_instance_id = excluded.provider_instance_id,
            provider_session_id = excluded.provider_session_id,
            status = excluded.status,
            first_run_ordinal = excluded.first_run_ordinal,
            last_run_ordinal = excluded.last_run_ordinal,
            updated_at = excluded.updated_at,
            payload_json = excluded.payload_json",
        params![
            payload.id.as_str(),
            payload.app_thread_id.as_ref().map(|id| id.as_str()),
            payload.owner_node_id.as_ref().map(|id| id.as_str()),
            payload.provider_instance_id.as_str(),
            payload.driver.as_str(),
            payload.provider_instance_id.as_str(),
            payload.provider_session_id.as_ref().map(|id| id.as_str()),
            text(&payload.status),
            payload
                .first_run_ordinal
                .map(|ordinal| ordinal.get() as i64),
            payload.last_run_ordinal.map(|ordinal| ordinal.get() as i64),
            payload.updated_at.to_string(),
            serde_json::to_string(payload)?,
        ],
    )?;
    if let Some(app_thread_id) = &payload.app_thread_id
        && !is_queued_provider_thread_placeholder(payload)
        && let Some(mut thread) = thread_payload(conn, app_thread_id)?
    {
        thread.active_provider_thread_id = Some(payload.id.clone());
        thread.updated_at = payload.updated_at;
        execute(
            conn,
            "UPDATE orchestration_v2_projection_threads
             SET active_provider_thread_id = ?1, updated_at = ?2, payload_json = ?3
             WHERE thread_id = ?4",
            params![
                payload.id.as_str(),
                thread.updated_at.to_string(),
                serde_json::to_string(&thread)?,
                app_thread_id.as_str(),
            ],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::projection::test_support::*;

    const THREAD_ROW: &str =
        "SELECT payload_json FROM orchestration_v2_projection_threads WHERE thread_id = ?1";

    async fn json_field(
        store: &crate::projection::store::ProjectionStore,
        sql: &'static str,
        id: &str,
        path: &[&str],
    ) -> serde_json::Value {
        let payload = scalar(store, sql, id).await.expect("row");
        let mut value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        for key in path {
            value = value[*key].clone();
        }
        value
    }

    #[tokio::test]
    async fn streamed_events_move_the_thread_activity_timestamp_but_visits_do_not() {
        let store = memory_store().await;
        apply_all(
            &store,
            vec![
                event("thread.created", "thread:a", T0, thread("thread:a")),
                event(
                    "run.created",
                    "thread:a",
                    &at(5),
                    run("thread:a", "run:a", 1, "running"),
                ),
            ],
        )
        .await;
        assert_eq!(
            json_field(&store, THREAD_ROW, "thread:a", &["updatedAt"]).await,
            json!(at(5))
        );
        assert_eq!(
            scalar(
                &store,
                "SELECT updated_at FROM orchestration_v2_projection_threads WHERE thread_id = ?1",
                "thread:a"
            )
            .await,
            Some(at(5))
        );
        let mut visited = thread("thread:a");
        visited["lastVisitedAt"] = json!(at(9));
        visited["updatedAt"] = json!(at(5));
        apply_all(
            &store,
            vec![event("thread.visited", "thread:a", &at(9), visited)],
        )
        .await;
        assert_eq!(
            json_field(&store, THREAD_ROW, "thread:a", &["updatedAt"]).await,
            json!(at(5)),
            "a visit is read state, not activity"
        );
        assert_eq!(
            json_field(&store, THREAD_ROW, "thread:a", &["lastVisitedAt"]).await,
            json!(at(9))
        );
    }

    #[tokio::test]
    async fn pull_request_sync_and_active_reorder_bump_the_activity_timestamp() {
        let store = memory_store().await;
        apply_all(
            &store,
            vec![
                event("thread.created", "thread:a", T0, thread("thread:a")),
                event(
                    "thread.active-reordered",
                    "thread:a",
                    &at(7),
                    thread("thread:a"),
                ),
            ],
        )
        .await;
        assert_eq!(
            json_field(&store, THREAD_ROW, "thread:a", &["updatedAt"]).await,
            json!(at(7))
        );
        apply_all(
            &store,
            vec![event(
                "thread.pull-request-synced",
                "thread:a",
                &at(8),
                thread("thread:a"),
            )],
        )
        .await;
        assert_eq!(
            json_field(&store, THREAD_ROW, "thread:a", &["updatedAt"]).await,
            json!(at(8))
        );
    }

    #[tokio::test]
    async fn a_stale_run_snapshot_keeps_fields_other_events_recorded() {
        let store = memory_store().await;
        let work = json!([{"kind": "subagent", "label": "Background subagent test"}]);
        apply_all(
            &store,
            vec![
                event("thread.created", "thread:a", T0, thread("thread:a")),
                event(
                    "run.created",
                    "thread:a",
                    T0,
                    run("thread:a", "run:a", 1, "running"),
                ),
                event(
                    "run.background-work-cancelled",
                    "thread:a",
                    &at(1),
                    json!({"runId": "run:a", "restartCancelledBackgroundWork": work}),
                ),
            ],
        )
        .await;
        let mut completed = run("thread:a", "run:a", 1, "completed");
        completed["completedAt"] = json!(at(2));
        apply_all(
            &store,
            vec![event("run.updated", "thread:a", &at(2), completed)],
        )
        .await;
        const RUN_ROW: &str =
            "SELECT payload_json FROM orchestration_v2_projection_runs WHERE run_id = ?1";
        assert_eq!(
            json_field(&store, RUN_ROW, "run:a", &["status"]).await,
            json!("completed")
        );
        assert_eq!(
            json_field(
                &store,
                RUN_ROW,
                "run:a",
                &["restartCancelledBackgroundWork"]
            )
            .await,
            work
        );
        assert_eq!(
            scalar(
                &store,
                "SELECT status FROM orchestration_v2_projection_runs WHERE run_id = ?1",
                "run:a"
            )
            .await,
            Some("completed".to_owned())
        );
    }

    #[tokio::test]
    async fn delegated_completion_survives_a_snapshot_without_it() {
        let store = memory_store().await;
        let mut with_cohort = run("thread:a", "run:a", 1, "running");
        with_cohort["delegatedCompletion"] = json!({
            "disposition": "open", "nextGeneration": 2, "delivery": null
        });
        apply_all(
            &store,
            vec![
                event("thread.created", "thread:a", T0, thread("thread:a")),
                event("run.created", "thread:a", T0, with_cohort),
                event(
                    "run.updated",
                    "thread:a",
                    &at(1),
                    run("thread:a", "run:a", 1, "completed"),
                ),
            ],
        )
        .await;
        const RUN_ROW: &str =
            "SELECT payload_json FROM orchestration_v2_projection_runs WHERE run_id = ?1";
        assert_eq!(
            json_field(
                &store,
                RUN_ROW,
                "run:a",
                &["delegatedCompletion", "nextGeneration"]
            )
            .await,
            json!(2)
        );
    }

    fn subagent(completion_delivery: Option<serde_json::Value>) -> serde_json::Value {
        let mut value = json!({
            "id": "node:sub", "threadId": "thread:a", "runId": null, "parentNodeId": "node:root",
            "origin": "app_owned", "createdBy": "agent", "driver": "codex",
            "providerInstanceId": "codex", "providerThreadId": null, "childThreadId": null,
            "nativeTaskRef": null, "prompt": "p", "title": null, "model": null,
            "status": "running", "result": null, "startedAt": T0, "completedAt": null,
            "updatedAt": T0
        });
        if let Some(delivery) = completion_delivery {
            value["completionDelivery"] = delivery;
        }
        value
    }

    #[tokio::test]
    async fn subagent_completion_delivery_survives_a_stale_snapshot() {
        let store = memory_store().await;
        apply_all(
            &store,
            vec![
                event("thread.created", "thread:a", T0, thread("thread:a")),
                event(
                    "subagent.updated",
                    "thread:a",
                    T0,
                    subagent(Some(
                        json!({"state": "claimed", "observedByRunId": "run:a"}),
                    )),
                ),
                event("subagent.updated", "thread:a", &at(1), subagent(None)),
            ],
        )
        .await;
        const ROW: &str =
            "SELECT payload_json FROM orchestration_v2_projection_subagents WHERE subagent_id = ?1";
        assert_eq!(
            json_field(&store, ROW, "node:sub", &["completionDelivery", "state"]).await,
            json!("claimed")
        );
    }

    fn provider_turn(token_usage: Option<serde_json::Value>) -> serde_json::Value {
        let mut value = json!({
            "id": "turn:a", "providerThreadId": "pthread:a", "nodeId": "node:a",
            "runAttemptId": null, "nativeTurnRef": null, "ordinal": 1, "status": "running",
            "startedAt": T0, "completedAt": null
        });
        if let Some(usage) = token_usage {
            value["tokenUsage"] = usage;
        }
        value
    }

    #[tokio::test]
    async fn provider_turn_keeps_recorded_token_usage() {
        let store = memory_store().await;
        let usage = json!({"usedTokens": 42, "updatedAt": T0});
        apply_all(
            &store,
            vec![
                event("thread.created", "thread:a", T0, thread("thread:a")),
                event(
                    "provider-turn.updated",
                    "thread:a",
                    T0,
                    provider_turn(Some(usage.clone())),
                ),
                event(
                    "provider-turn.updated",
                    "thread:a",
                    &at(1),
                    provider_turn(None),
                ),
            ],
        )
        .await;
        const ROW: &str = "SELECT payload_json FROM orchestration_v2_projection_provider_turns WHERE provider_turn_id = ?1";
        assert_eq!(
            json_field(&store, ROW, "turn:a", &["tokenUsage"]).await,
            usage
        );
    }

    fn provider_session(status: &str) -> serde_json::Value {
        json!({
            "id": "session:a", "driver": "codex", "providerInstanceId": "codex",
            "status": status, "cwd": "/tmp", "model": null,
            "capabilities": crate::projection::test_support::capabilities(),
            "createdAt": T0, "updatedAt": T0, "lastError": null
        })
    }

    #[tokio::test]
    async fn sessions_bind_on_attach_not_on_update_and_unbind_on_detach() {
        const BINDINGS: &str = "SELECT COUNT(*) FROM orchestration_v2_projection_provider_session_bindings WHERE provider_session_id = ?1";
        let store = memory_store().await;
        apply_all(
            &store,
            vec![
                event("thread.created", "thread:a", T0, thread("thread:a")),
                event("thread.created", "thread:b", T0, thread("thread:b")),
                event(
                    "provider-session.attached",
                    "thread:a",
                    T0,
                    provider_session("ready"),
                ),
            ],
        )
        .await;
        assert_eq!(count(&store, BINDINGS, "session:a").await, 1);
        // An update from another thread's stream updates the row but binds nothing.
        apply_all(
            &store,
            vec![event(
                "provider-session.updated",
                "thread:b",
                &at(1),
                provider_session("running"),
            )],
        )
        .await;
        assert_eq!(count(&store, BINDINGS, "session:a").await, 1);
        assert_eq!(
            scalar(
                &store,
                "SELECT status FROM orchestration_v2_projection_provider_sessions WHERE provider_session_id = ?1",
                "session:a"
            )
            .await,
            Some("running".to_owned())
        );
        apply_all(
            &store,
            vec![event(
                "provider-session.detached",
                "thread:a",
                &at(2),
                json!({"providerSessionId": "session:a", "detachedAt": at(2)}),
            )],
        )
        .await;
        assert_eq!(count(&store, BINDINGS, "session:a").await, 0);
    }

    fn provider_thread(status: &str, first_run_ordinal: Option<u64>) -> serde_json::Value {
        json!({
            "id": "pthread:a", "driver": "codex", "providerInstanceId": "codex",
            "providerSessionId": null, "appThreadId": "thread:a", "ownerNodeId": null,
            "nativeThreadRef": null, "nativeConversationHeadRef": null, "status": status,
            "firstRunOrdinal": first_run_ordinal, "lastRunOrdinal": null, "handoffIds": [],
            "forkedFrom": null, "createdAt": T0, "updatedAt": at(3)
        })
    }

    #[tokio::test]
    async fn a_real_provider_thread_becomes_the_active_one_but_a_placeholder_does_not() {
        const ACTIVE: &str = "SELECT active_provider_thread_id FROM orchestration_v2_projection_threads WHERE thread_id = ?1";
        let store = memory_store().await;
        apply_all(
            &store,
            vec![
                event("thread.created", "thread:a", T0, thread("thread:a")),
                event(
                    "provider-thread.updated",
                    "thread:a",
                    &at(4),
                    provider_thread("not_loaded", None),
                ),
            ],
        )
        .await;
        assert_eq!(scalar(&store, ACTIVE, "thread:a").await, None);
        apply_all(
            &store,
            vec![event(
                "provider-thread.updated",
                "thread:a",
                &at(5),
                provider_thread("active", Some(1)),
            )],
        )
        .await;
        assert_eq!(
            scalar(&store, ACTIVE, "thread:a").await,
            Some("pthread:a".to_owned())
        );
        assert_eq!(
            json_field(&store, THREAD_ROW, "thread:a", &["activeProviderThreadId"]).await,
            json!("pthread:a")
        );
    }

    #[tokio::test]
    async fn a_pull_request_link_update_patches_one_link_of_the_stored_thread() {
        let store = memory_store().await;
        let link = |number: u64, source: &str| {
            json!({
                "host": "github.com", "repository": "o/r", "number": number,
                "url": format!("https://github.com/o/r/pull/{number}"), "source": source,
                "linkedAt": T0, "snapshot": null, "stack": null
            })
        };
        let mut created = thread("thread:a");
        created["pullRequests"] = json!([link(1, "manual"), link(2, "manual")]);
        apply_all(
            &store,
            vec![
                event("thread.created", "thread:a", T0, created),
                event(
                    "thread.pull-request-link-updated",
                    "thread:a",
                    &at(6),
                    link(2, "agent"),
                ),
            ],
        )
        .await;
        let links = json_field(&store, THREAD_ROW, "thread:a", &["pullRequests"]).await;
        assert_eq!(links[0]["source"], json!("manual"));
        assert_eq!(links[1]["source"], json!("agent"));
        let missing = store
            .apply(event(
                "thread.pull-request-link-updated",
                "thread:missing",
                &at(6),
                link(2, "agent"),
            ))
            .await;
        assert!(missing.is_err(), "no stored thread to patch");
    }

    #[tokio::test]
    async fn turn_items_and_messages_land_in_their_tables() {
        let store = memory_store().await;
        apply_all(
            &store,
            vec![
                event("thread.created", "thread:a", T0, thread("thread:a")),
                event(
                    "run.created",
                    "thread:a",
                    T0,
                    run("thread:a", "run:a", 1, "completed"),
                ),
                event(
                    "turn-item.updated",
                    "thread:a",
                    T0,
                    user_message_item("thread:a", "item:1", "run:a", 1, "hello", "turn_start"),
                ),
                event(
                    "message.updated",
                    "thread:a",
                    T0,
                    message(
                        "thread:a",
                        "message:item:1",
                        Some("run:a"),
                        "user",
                        "hello",
                        T0,
                    ),
                ),
            ],
        )
        .await;
        assert_eq!(
            scalar(
                &store,
                "SELECT type FROM orchestration_v2_projection_turn_items WHERE turn_item_id = ?1",
                "item:1"
            )
            .await,
            Some("user_message".to_owned())
        );
        assert_eq!(
            scalar(
                &store,
                "SELECT streaming FROM orchestration_v2_projection_messages WHERE message_id = ?1",
                "message:item:1"
            )
            .await,
            Some("0".to_owned())
        );
    }
}
