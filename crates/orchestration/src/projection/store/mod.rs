//! Port of upstream/apps/server/src/orchestration-v2/ProjectionStore.ts
//!
//! The thread read model. Writes go through [`apply::apply_event`] inside the
//! event sink's transaction; reads run on the database's read connection and
//! never touch the UI thread. See `apply.rs` for the seam the pipeline uses.
//!
//! Not ported: `layerMemory` (the in-memory twin, used by upstream tests; Turin
//! tests run against an in-memory SQLite `Db`), `WireProjection` (wire
//! shrinking of turn items for socket frames) and the bounded snapshot
//! builders. See PORTING.md Deviations.

pub mod apply;
pub mod canonical;
#[cfg(test)]
mod context_tests;
pub mod contexts;
pub mod control;
#[cfg(test)]
mod control_tests;
pub mod history;
#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod read_tests;
pub mod recovery;
pub mod runtime_recovery;
pub mod shell;
#[cfg(test)]
mod shell_tests;
pub mod unreadable;
#[cfg(test)]
mod upstream_tests;
#[cfg(test)]
mod window_tests;

use std::sync::Arc;

use rusqlite::Connection;
use turin_contracts::orchestration_v2::{
    OrchestrationV2AppThread, OrchestrationV2DomainEvent, OrchestrationV2PlanArtifact,
    OrchestrationV2RuntimeRequest, OrchestrationV2ThreadHistoryPage,
    OrchestrationV2ThreadProjection, OrchestrationV2TurnItem,
};
use turin_contracts::{ProviderTurnId, RunId, ThreadId, TurnItemId};
use turin_persistence::Db;

use crate::projection::error::{ProjectionStoreError, ReadFailure, keep_or_read};
use crate::projection::sql::{query_payload, query_payloads};
use canonical::{
    HistoryAnchor, ProjectionRecordField, ProjectionRecordFilter, ReadWindow,
    read_canonical_projection, read_projection,
};
use recovery::{ProjectionRecoveryKind, read_recovery_thread_ids};
use shell::{
    ProjectionLimitRecoveryCandidate, ProjectionSettlementCandidate, ProjectionThreadPullRequests,
    ShellSnapshotOptions, ShellSnapshotRows, ThreadWorktreePath,
};
use turin_contracts::UtcDateTime;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ConversationMessage, OrchestrationV2ExecutionNode,
};
use turin_contracts::orchestration_v2::{
    OrchestrationV2ThreadShell, OrchestrationV2ThreadShellSnapshot,
};

/// `ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION`.
pub const ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION: u64 = 2;

/// A thread projection read together with the event sequence it reflects
/// (`getThreadSnapshot` / `getThreadSnapshotWindow`).
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadSnapshot {
    pub schema_version: u64,
    pub snapshot_sequence: u64,
    pub projection: OrchestrationV2ThreadProjection,
}

/// Options of [`ProjectionStore::get_thread_snapshot_window`].
#[derive(Debug, Clone, Default)]
pub struct ThreadSnapshotWindowOptions {
    /// Row fallback for histories without turn starts.
    pub row_limit: i64,
    /// User-turn window, with extra anchors for the inclusive cursor and has-more check.
    pub user_turn_limit: Option<i64>,
    pub anchor_item_id: Option<TurnItemId>,
    pub anchor_thread_id: Option<ThreadId>,
    pub required_run_id: Option<RunId>,
}

/// Handle on the projection tables. Cheap to clone.
#[derive(Clone)]
pub struct ProjectionStore {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
}

impl ProjectionStore {
    pub fn new(db: Db) -> Self {
        Self {
            inner: Arc::new(Inner { db }),
        }
    }

    pub fn db(&self) -> &Db {
        &self.inner.db
    }

    /// `apply` in its own write transaction. The event sink does not use this:
    /// it calls [`apply::apply_event`] inside the transaction that appends the
    /// event, so the event and its projection commit together.
    pub async fn apply(
        &self,
        event: OrchestrationV2DomainEvent,
    ) -> Result<(), ProjectionStoreError> {
        self.inner
            .db
            .write(move |tx| Ok(apply::apply_event(tx, &event)))
            .await
            .map_err(ProjectionStoreError::Database)?
    }

    /// Run `f` on the read connection, outside any transaction.
    pub(crate) async fn read<R, F>(&self, f: F) -> Result<R, ProjectionStoreError>
    where
        R: Send + 'static,
        F: FnOnce(&Connection) -> Result<R, ProjectionStoreError> + Send + 'static,
    {
        self.inner
            .db
            .read(move |conn| Ok(f(conn)))
            .await
            .map_err(ProjectionStoreError::Database)?
    }

    /// Run `f` inside one read transaction (`sql.withTransaction`), so several
    /// statements agree with each other even while the writer commits.
    pub(crate) async fn read_tx<R, F>(&self, f: F) -> Result<R, ProjectionStoreError>
    where
        R: Send + 'static,
        F: FnOnce(&Connection) -> Result<R, ProjectionStoreError> + Send + 'static,
    {
        self.read(move |conn| {
            let tx = conn
                .unchecked_transaction()
                .map_err(ProjectionStoreError::setup)?;
            let result = f(&tx);
            // A read transaction has nothing to commit; dropping rolls it back.
            drop(tx);
            result
        })
        .await
    }

    /// Like [`Self::read`], for a body that fails with [`ReadFailure`]: raw
    /// causes are wrapped in `ProjectionStoreReadError` for `thread_id`, typed
    /// store errors (thread not found) pass through.
    pub(crate) async fn read_for_thread<R, F>(
        &self,
        thread_id: &ThreadId,
        transaction: bool,
        f: F,
    ) -> Result<R, ProjectionStoreError>
    where
        R: Send + 'static,
        F: FnOnce(&Connection) -> Result<R, ReadFailure> + Send + 'static,
    {
        let thread_id = thread_id.clone();
        let body =
            move |conn: &Connection| f(conn).map_err(|error| keep_or_read(&thread_id, error));
        if transaction {
            self.read_tx(body).await
        } else {
            self.read(body).await
        }
    }

    /// `getThread`: the thread row, without its records.
    pub async fn get_thread(
        &self,
        thread_id: &ThreadId,
    ) -> Result<OrchestrationV2AppThread, ProjectionStoreError> {
        let id = thread_id.clone();
        self.read_for_thread(thread_id, false, move |conn| read_thread(conn, &id))
            .await
    }

    /// `getThreadProjection`: every record of the thread plus its visible
    /// timeline, with inherited fork history.
    pub async fn get_thread_projection(
        &self,
        thread_id: &ThreadId,
    ) -> Result<OrchestrationV2ThreadProjection, ProjectionStoreError> {
        let id = thread_id.clone();
        self.read_for_thread(thread_id, false, move |conn| {
            read_projection(conn, &id, &[], None)
        })
        .await
    }

    /// `getThreadRecords`: the thread plus only the requested record kinds.
    /// Unrequested kinds come back empty (upstream returns a `Pick`).
    pub async fn get_thread_records(
        &self,
        thread_id: &ThreadId,
        fields: Vec<ProjectionRecordField>,
        filter: Option<ProjectionRecordFilter>,
    ) -> Result<OrchestrationV2ThreadProjection, ProjectionStoreError> {
        let id = thread_id.clone();
        self.read_for_thread(thread_id, true, move |conn| {
            read_canonical_projection(conn, &id, None, Some(&fields), filter.as_ref())
        })
        .await
    }

    /// `getThreadSnapshot`: the full projection and the event sequence it reflects.
    pub async fn get_thread_snapshot(
        &self,
        thread_id: &ThreadId,
    ) -> Result<ThreadSnapshot, ProjectionStoreError> {
        let id = thread_id.clone();
        self.read_for_thread(thread_id, true, move |conn| {
            let projection = read_projection(conn, &id, &[], None)?;
            Ok(ThreadSnapshot {
                schema_version: ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION,
                snapshot_sequence: thread_snapshot_sequence(conn, &id)?,
                projection,
            })
        })
        .await
    }

    /// `getThreadSnapshotWindow`: the projection narrowed to recent user turns
    /// (or an anchored page) so opening a very long thread stays cheap.
    pub async fn get_thread_snapshot_window(
        &self,
        thread_id: &ThreadId,
        options: ThreadSnapshotWindowOptions,
    ) -> Result<ThreadSnapshot, ProjectionStoreError> {
        let id = thread_id.clone();
        self.read_for_thread(thread_id, true, move |conn| {
            let history_anchor = match &options.anchor_item_id {
                None => None,
                Some(item_id) => Some(HistoryAnchor {
                    item_id: item_id.clone(),
                    thread_id: match &options.anchor_thread_id {
                        Some(anchor_thread_id) => anchor_thread_id.clone(),
                        None => anchor_thread_of(conn, item_id)?.unwrap_or_else(|| id.clone()),
                    },
                }),
            };
            let window = ReadWindow {
                row_limit: options.row_limit,
                user_turn_limit: options.user_turn_limit,
                anchor_item_id: history_anchor
                    .as_ref()
                    .filter(|anchor| anchor.thread_id == id)
                    .map(|anchor| anchor.item_id.clone()),
                history_anchor,
                ..ReadWindow::default()
            };
            let projection = read_projection(conn, &id, &[], Some(&window))?;
            Ok(ThreadSnapshot {
                schema_version: ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION,
                snapshot_sequence: thread_snapshot_sequence(conn, &id)?,
                projection,
            })
        })
        .await
    }
}

impl ProjectionStore {
    /// `readShellSnapshot`: the raw rows, read in one transaction. Decode them
    /// with [`ShellSnapshotRows::decode`] after the connection is free.
    pub async fn read_shell_snapshot(
        &self,
        options: ShellSnapshotOptions,
    ) -> Result<ShellSnapshotRows, ProjectionStoreError> {
        self.read_tx(move |conn| shell::read_shell_snapshot_rows(conn, &options))
            .await
    }

    /// `getShellSnapshot`.
    pub async fn get_shell_snapshot(
        &self,
        options: ShellSnapshotOptions,
    ) -> Result<OrchestrationV2ThreadShellSnapshot, ProjectionStoreError> {
        self.read_shell_snapshot(options).await?.decode()
    }

    /// `getThreadShell`: one thread's shell, `None` when it does not exist.
    pub async fn get_thread_shell(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Option<OrchestrationV2ThreadShell>, ProjectionStoreError> {
        let id = thread_id.clone();
        self.read_for_thread(thread_id, true, move |conn| {
            Ok(shell::read_thread_shell(conn, &id)?)
        })
        .await
    }

    /// `getSettlementCandidates`.
    pub async fn get_settlement_candidates(
        &self,
        thread_id: Option<ThreadId>,
    ) -> Result<Vec<ProjectionSettlementCandidate>, ProjectionStoreError> {
        let id = thread_id.clone().unwrap_or_else(shell_thread_id);
        self.read_for_thread(&id, true, move |conn| {
            Ok(shell::read_settlement_candidates(conn, thread_id.as_ref())?)
        })
        .await
    }

    /// `getThreadsWithPullRequests`.
    pub async fn get_threads_with_pull_requests(
        &self,
        thread_id: Option<ThreadId>,
    ) -> Result<Vec<ProjectionThreadPullRequests>, ProjectionStoreError> {
        let id = thread_id.clone().unwrap_or_else(shell_thread_id);
        self.read_for_thread(&id, true, move |conn| {
            Ok(shell::read_threads_with_pull_requests(
                conn,
                thread_id.as_ref(),
            )?)
        })
        .await
    }

    /// `getThreadWorktreePaths`.
    pub async fn get_thread_worktree_paths(
        &self,
    ) -> Result<Vec<ThreadWorktreePath>, ProjectionStoreError> {
        self.read_for_thread(&shell_thread_id(), false, |conn| {
            Ok(shell::read_thread_worktree_paths(conn)?)
        })
        .await
    }

    /// `getTurnItem`: one item of a thread, `None` when it is not there.
    pub async fn get_turn_item(
        &self,
        thread_id: &ThreadId,
        item_id: &TurnItemId,
    ) -> Result<Option<OrchestrationV2TurnItem>, ProjectionStoreError> {
        let item_id = item_id.clone();
        let thread = thread_id.clone();
        self.read_for_thread(thread_id, false, move |conn| {
            Ok(query_payload::<OrchestrationV2TurnItem>(
                conn,
                "SELECT payload_json FROM orchestration_v2_projection_turn_items
                 WHERE turn_item_id = ?1 AND thread_id = ?2",
                rusqlite::params![item_id.as_str(), thread.as_str()],
            )?)
        })
        .await
    }

    /// `getTurnStartHistory`: the items that make up the visible turn history,
    /// optionally limited to `run_ids`. An empty `run_ids` matches nothing.
    pub async fn get_turn_start_history(
        &self,
        thread_id: &ThreadId,
        run_ids: Option<&[RunId]>,
    ) -> Result<Vec<OrchestrationV2TurnItem>, ProjectionStoreError> {
        let thread = thread_id.clone();
        let run_ids = run_ids.map(<[RunId]>::to_vec);
        self.read_for_thread(thread_id, false, move |conn| {
            let mut params = vec![thread.as_str().to_owned()];
            let run_filter = match &run_ids {
                None => "1".to_owned(),
                Some(ids) => {
                    params.extend(ids.iter().map(|id| id.as_str().to_owned()));
                    format!("run_id IN ({})", vec!["?"; ids.len()].join(", "))
                }
            };
            let sql = format!(
                "SELECT payload_json FROM orchestration_v2_projection_turn_items
                 WHERE thread_id = ?
                   AND type IN ('user_message', 'assistant_message', 'command_execution', 'error',
                     'run_interrupt_result', 'file_change', 'proposed_plan')
                   AND {run_filter}
                 ORDER BY ordinal ASC, turn_item_id ASC"
            );
            Ok(query_payloads::<OrchestrationV2TurnItem>(
                conn,
                &sql,
                rusqlite::params_from_iter(params.iter()),
            )?)
        })
        .await
    }

    /// `getPendingNativeUserInputs`.
    pub async fn get_pending_native_user_inputs(
        &self,
        thread_id: &ThreadId,
        provider_turn_id: &ProviderTurnId,
    ) -> Result<control::PendingNativeUserInputs, ProjectionStoreError> {
        let thread = thread_id.clone();
        let provider_turn_id = provider_turn_id.clone();
        self.read_for_thread(thread_id, true, move |conn| {
            control::pending_native_user_inputs(conn, &thread, &provider_turn_id)
        })
        .await
    }

    /// `getPlan`: `None` when the plan does not exist; errors if the thread does not.
    pub async fn get_plan(
        &self,
        thread_id: &ThreadId,
        plan_id: &str,
    ) -> Result<Option<OrchestrationV2PlanArtifact>, ProjectionStoreError> {
        let thread = thread_id.clone();
        let plan_id = plan_id.to_owned();
        self.read_for_thread(thread_id, true, move |conn| {
            control::plan(conn, &thread, &plan_id)
        })
        .await
    }

    /// `getRuntimeRequest`: `None` when the request does not exist; errors if the thread does not.
    pub async fn get_runtime_request(
        &self,
        thread_id: &ThreadId,
        request_id: &str,
    ) -> Result<Option<OrchestrationV2RuntimeRequest>, ProjectionStoreError> {
        let thread = thread_id.clone();
        let request_id = request_id.to_owned();
        self.read_for_thread(thread_id, true, move |conn| {
            control::runtime_request(conn, &thread, &request_id)
        })
        .await
    }

    /// `getRuntimeRecoveryProjection`: the runtime state a recovery pass reads
    /// for one thread. Runs in one read transaction, so the ten queries agree.
    pub async fn get_runtime_recovery_projection(
        &self,
        thread_id: &ThreadId,
    ) -> Result<runtime_recovery::RuntimeRecoveryState, ProjectionStoreError> {
        let thread = thread_id.clone();
        self.read_for_thread(thread_id, true, move |conn| {
            runtime_recovery::runtime_recovery_state(conn, &thread)
        })
        .await
    }

    /// `getProviderControlContext`.
    pub async fn get_provider_control_context(
        &self,
        thread_id: &ThreadId,
        target: contexts::ProviderControlTarget,
    ) -> Result<contexts::ProviderControlContext, ProjectionStoreError> {
        let thread = thread_id.clone();
        self.read_for_thread(thread_id, true, move |conn| {
            contexts::provider_control_context(conn, &thread, &target)
        })
        .await
    }

    /// `getRunningTurnContext`.
    pub async fn get_running_turn_context(
        &self,
        thread_id: &ThreadId,
    ) -> Result<contexts::RunningTurnContext, ProjectionStoreError> {
        let thread = thread_id.clone();
        self.read_for_thread(thread_id, true, move |conn| {
            contexts::running_turn_context(conn, &thread)
        })
        .await
    }

    /// `getThreadProviderContext`. `target_instance_id` names a provider instance;
    /// `None` skips the provider thread lookup.
    pub async fn get_thread_provider_context(
        &self,
        thread_id: &ThreadId,
        target_instance_id: Option<&str>,
    ) -> Result<contexts::ThreadProviderContext, ProjectionStoreError> {
        let thread = thread_id.clone();
        let target_instance_id = target_instance_id.map(str::to_owned);
        self.read_for_thread(thread_id, true, move |conn| {
            contexts::thread_provider_context(conn, &thread, target_instance_id.as_deref())
        })
        .await
    }

    /// `getRuntimeResponseContext`.
    pub async fn get_runtime_response_context(
        &self,
        thread_id: &ThreadId,
        request_id: &str,
    ) -> Result<contexts::RuntimeResponseContext, ProjectionStoreError> {
        let thread = thread_id.clone();
        let request_id = request_id.to_owned();
        self.read_for_thread(thread_id, true, move |conn| {
            contexts::runtime_response_context(conn, &thread, &request_id)
        })
        .await
    }

    /// `getCheckpointContext`.
    pub async fn get_checkpoint_context(
        &self,
        thread_id: &ThreadId,
    ) -> Result<contexts::CheckpointContext, ProjectionStoreError> {
        let thread = thread_id.clone();
        self.read_for_thread(thread_id, true, move |conn| {
            contexts::checkpoint_context(conn, &thread)
        })
        .await
    }

    /// `getCheckpointCaptureContext`.
    pub async fn get_checkpoint_capture_context(
        &self,
        thread_id: &ThreadId,
        run_id: &str,
        scope_id: &str,
    ) -> Result<contexts::CheckpointCaptureContext, ProjectionStoreError> {
        let thread = thread_id.clone();
        let run_id = run_id.to_owned();
        let scope_id = scope_id.to_owned();
        self.read_for_thread(thread_id, true, move |conn| {
            contexts::checkpoint_capture_context(conn, &thread, &run_id, &scope_id)
        })
        .await
    }

    /// `getRunMessage`: `None` when the run has no opening message.
    pub async fn get_run_message(
        &self,
        thread_id: &ThreadId,
        run_id: &str,
    ) -> Result<Option<OrchestrationV2ConversationMessage>, ProjectionStoreError> {
        let thread = thread_id.clone();
        let run_id = run_id.to_owned();
        self.read_for_thread(thread_id, true, move |conn| {
            contexts::run_message(conn, &thread, &run_id)
        })
        .await
    }

    /// `canStartQueuedRun`.
    pub async fn can_start_queued_run(
        &self,
        thread_id: &ThreadId,
    ) -> Result<bool, ProjectionStoreError> {
        let thread = thread_id.clone();
        self.read_for_thread(thread_id, true, move |conn| {
            contexts::can_start_queued_run(conn, &thread)
        })
        .await
    }

    /// `hasUnpairedRunInterruptRequest`.
    pub async fn has_unpaired_run_interrupt_request(
        &self,
        thread_id: &ThreadId,
        request_id: &str,
        result_id: &str,
    ) -> Result<bool, ProjectionStoreError> {
        let thread = thread_id.clone();
        let request_id = request_id.to_owned();
        let result_id = result_id.to_owned();
        self.read_for_thread(thread_id, false, move |conn| {
            contexts::has_unpaired_run_interrupt_request(conn, &thread, &request_id, &result_id)
        })
        .await
    }

    /// `isNodeStored`: whether the stored node row equals `node`.
    pub async fn is_node_stored(
        &self,
        node: &OrchestrationV2ExecutionNode,
    ) -> Result<bool, ProjectionStoreError> {
        let node = node.clone();
        let thread = node.thread_id.clone();
        self.read_for_thread(&thread, false, move |conn| {
            contexts::is_node_stored(conn, &node)
        })
        .await
    }

    /// `getThreadHistoryPage`. Errors with `ThreadNotFound` when the thread is deleted.
    pub async fn get_thread_history_page(
        &self,
        thread_id: &ThreadId,
        cursor: &str,
        through_entry_id: Option<&str>,
        conversation_only: bool,
    ) -> Result<OrchestrationV2ThreadHistoryPage, ProjectionStoreError> {
        let thread = thread_id.clone();
        let cursor = cursor.to_owned();
        let through_entry_id = through_entry_id.map(str::to_owned);
        self.read_for_thread(thread_id, true, move |conn| {
            history::thread_history_page(
                conn,
                &thread,
                &cursor,
                through_entry_id.as_deref(),
                conversation_only,
            )
        })
        .await
    }

    /// `getTimelinePage`.
    pub async fn get_timeline_page(
        &self,
        thread_id: &ThreadId,
        options: history::TimelinePageOptions,
    ) -> Result<history::ProjectionTimelinePage, ProjectionStoreError> {
        let thread = thread_id.clone();
        self.read_for_thread(thread_id, true, move |conn| {
            history::timeline_page(conn, &thread, &options)
        })
        .await
    }

    /// `getMessageCount`.
    pub async fn get_message_count(
        &self,
        thread_id: &ThreadId,
    ) -> Result<u64, ProjectionStoreError> {
        let thread = thread_id.clone();
        self.read_for_thread(thread_id, false, move |conn| {
            history::message_count(conn, &thread)
        })
        .await
    }

    /// `getNextTurnItemOrdinal`.
    pub async fn get_next_turn_item_ordinal(
        &self,
        thread_id: &ThreadId,
    ) -> Result<u64, ProjectionStoreError> {
        let thread = thread_id.clone();
        self.read_for_thread(thread_id, false, move |conn| {
            history::next_turn_item_ordinal(conn, &thread)
        })
        .await
    }

    /// `getThreadAttachmentIds`, message half only (see `history::message_attachment_ids`).
    pub async fn get_thread_attachment_ids(
        &self,
        thread_id: &ThreadId,
    ) -> Result<Vec<String>, ProjectionStoreError> {
        let thread = thread_id.clone();
        self.read_for_thread(thread_id, false, move |conn| {
            history::message_attachment_ids(conn, &thread)
        })
        .await
    }

    /// `getTurnStartContext`: what a turn start reads for one run.
    pub async fn get_turn_start_context(
        &self,
        thread_id: &ThreadId,
        run_id: &RunId,
    ) -> Result<contexts::TurnStartContext, ProjectionStoreError> {
        let thread = thread_id.clone();
        let run_id = run_id.as_str().to_owned();
        self.read_for_thread(thread_id, true, move |conn| {
            contexts::turn_start_context(conn, &thread, &run_id)
        })
        .await
    }

    /// `getRecoveryThreadIds`: threads a boot-time recovery pass of `kind` must visit.
    pub async fn get_recovery_thread_ids(
        &self,
        kind: ProjectionRecoveryKind,
    ) -> Result<Vec<ThreadId>, ProjectionStoreError> {
        self.read(move |conn| read_recovery_thread_ids(conn, kind))
            .await
    }

    /// `getUnreadableThreadIds`: threads whose projection rows fail to decode.
    /// Runs without a transaction, like upstream.
    pub async fn get_unreadable_thread_ids(&self) -> Result<Vec<ThreadId>, ProjectionStoreError> {
        self.read(unreadable::read_unreadable_thread_ids).await
    }

    /// `getLimitRecoveryCandidates`.
    pub async fn get_limit_recovery_candidates(
        &self,
        now: UtcDateTime,
        auto_resume: bool,
        snooze: bool,
    ) -> Result<Vec<ProjectionLimitRecoveryCandidate>, ProjectionStoreError> {
        self.read_for_thread(&shell_thread_id(), true, move |conn| {
            Ok(shell::read_limit_recovery_candidates(
                conn,
                &now,
                auto_resume,
                snooze,
            )?)
        })
        .await
    }
}

/// Stand-in thread id for reads that span threads, used only to label errors.
fn shell_thread_id() -> ThreadId {
    ThreadId::new("thread:shell").expect("literal id")
}

pub(crate) fn read_thread(
    conn: &Connection,
    thread_id: &ThreadId,
) -> Result<OrchestrationV2AppThread, ReadFailure> {
    query_payload(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_threads
         WHERE thread_id = ?1",
        [thread_id.as_str()],
    )?
    .ok_or_else(|| {
        ReadFailure::Store(ProjectionStoreError::ThreadNotFound {
            thread_id: thread_id.clone(),
        })
    })
}

/// The event sequence a thread's projection reflects: the newest v2 event on its stream.
pub(crate) fn thread_snapshot_sequence(
    conn: &Connection,
    thread_id: &ThreadId,
) -> Result<u64, ReadFailure> {
    let sequence: Option<i64> = conn.query_row(
        "SELECT MAX(sequence) AS snapshot_sequence
         FROM orchestration_events
         WHERE application_event_version = 2
           AND aggregate_kind = 'thread'
           AND stream_id = ?1",
        [thread_id.as_str()],
        |row| row.get(0),
    )?;
    Ok(sequence.unwrap_or(0).max(0) as u64)
}

fn anchor_thread_of(
    conn: &Connection,
    item_id: &TurnItemId,
) -> Result<Option<ThreadId>, ReadFailure> {
    let thread_id: Option<String> = conn
        .query_row(
            "SELECT thread_id
             FROM orchestration_v2_projection_turn_items
             WHERE turn_item_id = ?1
             LIMIT 1",
            [item_id.as_str()],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    Ok(thread_id.and_then(|id| ThreadId::new(id).ok()))
}
