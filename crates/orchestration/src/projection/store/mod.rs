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
pub mod shell;
#[cfg(test)]
mod shell_tests;
#[cfg(test)]
mod window_tests;

use std::sync::Arc;

use rusqlite::Connection;
use turin_contracts::orchestration_v2::{
    OrchestrationV2AppThread, OrchestrationV2DomainEvent, OrchestrationV2ThreadProjection,
};
use turin_contracts::{RunId, ThreadId, TurnItemId};
use turin_persistence::Db;

use crate::projection::error::{ProjectionStoreError, ReadFailure, keep_or_read};
use crate::projection::sql::query_payload;
use canonical::{
    HistoryAnchor, ProjectionRecordField, ProjectionRecordFilter, ReadWindow,
    read_canonical_projection, read_projection,
};
use shell::{
    ProjectionLimitRecoveryCandidate, ProjectionSettlementCandidate, ProjectionThreadPullRequests,
    ShellSnapshotOptions, ShellSnapshotRows, ThreadWorktreePath,
};
use turin_contracts::UtcDateTime;
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

fn read_thread(
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
