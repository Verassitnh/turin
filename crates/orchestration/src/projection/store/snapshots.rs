//! The snapshots a client loads before it follows a feed: the active shell
//! (`loadShellSnapshotParts` + `buildActiveShellSnapshot`) and a bounded
//! thread projection (`buildBoundedThreadProjection`, the window the web
//! client opens with).
//!
//! Upstream reads the shell in one reader transaction. Here the threads,
//! projects and the sequence come from the same read transaction, so the
//! sequence never runs ahead of the rows.

use std::collections::HashSet;

use turin_contracts::orchestration_v2::{
    OrchestrationV2ProjectedTurnItem, OrchestrationV2ProjectedTurnItemVisibility,
    OrchestrationV2ShellSnapshot, OrchestrationV2ThreadProjection, OrchestrationV2TurnItem,
};
use turin_contracts::{ThreadId, TurnItemId};

use crate::projection::error::ProjectionStoreError;
use crate::projection::project_store::list_shells_in;
use crate::projection::shell_stream::build_active_shell_snapshot;
use crate::projection::store::ProjectionStore;
use crate::projection::store::shell::{
    ShellSnapshotLocation, ShellSnapshotOptions, read_shell_snapshot_rows,
};
use crate::projection::thread_history_paging::{
    projected_row_encoded_bytes, select_recent_timeline_window,
};
use crate::projection::turn_item;

/// A thread projection narrowed to the newest user turns, with the cursor that
/// loads the page before it (`OrchestrationV2ThreadStreamItem::Snapshot`'s
/// `historyCursor` / `hasMoreHistory`).
#[derive(Debug, Clone, PartialEq)]
pub struct BoundedThreadSnapshot {
    pub snapshot_sequence: u64,
    pub projection: OrchestrationV2ThreadProjection,
    pub history_cursor: Option<String>,
    pub has_more_history: bool,
}

impl ProjectionStore {
    /// The active shell the sidebar starts from: decoded thread shells, every
    /// project, and the latest application sequence, read in one transaction.
    pub async fn get_active_shell_snapshot(
        &self,
    ) -> Result<OrchestrationV2ShellSnapshot, ProjectionStoreError> {
        self.read_tx(|conn| {
            let rows = read_shell_snapshot_rows(
                conn,
                &ShellSnapshotOptions {
                    location: Some(ShellSnapshotLocation::Active),
                    ..ShellSnapshotOptions::default()
                },
            )?;
            let threads = rows.decode()?;
            let projects = list_shells_in(conn, None).map_err(ProjectionStoreError::Database)?;
            let sequence = turin_persistence::latest_application_sequence(conn)
                .map_err(ProjectionStoreError::Database)?;
            Ok(build_active_shell_snapshot(
                projects,
                threads,
                u64::try_from(sequence).unwrap_or(0),
            ))
        })
        .await
    }

    /// The thread snapshot a client opens with: the full projection, then the
    /// newest user turns kept as `visibleTurnItems` (`buildBoundedThreadProjection`).
    /// Older turns stay behind `history_cursor` for `get_thread_history_page`.
    pub async fn get_bounded_thread_snapshot(
        &self,
        thread_id: &ThreadId,
    ) -> Result<BoundedThreadSnapshot, ProjectionStoreError> {
        let snapshot = self.get_thread_snapshot(thread_id).await?;
        let mut projection = snapshot.projection;
        // `None` is `THREAD_HISTORY_PAGE_POLICY`: the same bounds the web client asks for.
        let window = select_recent_timeline_window(
            &projection.visible_turn_items,
            snapshot.snapshot_sequence,
            None,
            &projected_row_encoded_bytes,
        );
        // Before the window is replaced: the turn items it keeps depend on its rows.
        projection.turn_items = bounded_turn_items(&projection, &window.items);
        projection.visible_turn_items = window.items;
        Ok(BoundedThreadSnapshot {
            snapshot_sequence: snapshot.snapshot_sequence,
            projection,
            history_cursor: window.next_cursor,
            has_more_history: window.has_more_history,
        })
    }
}

/// The turn items a bounded snapshot keeps (`buildBoundedThreadProjection`'s
/// `localTurnItemsForVisibleWindow` + `retainedInterruptRequestTurnItems`).
/// Older items are left out so that a history page can bring them back: the
/// store's history merge skips page rows whose item it already holds.
fn bounded_turn_items(
    projection: &OrchestrationV2ThreadProjection,
    visible: &[OrchestrationV2ProjectedTurnItem],
) -> Vec<OrchestrationV2TurnItem> {
    let thread_id = &projection.thread.id;
    let is_local = |row: &OrchestrationV2ProjectedTurnItem| {
        row.visibility == OrchestrationV2ProjectedTurnItemVisibility::Local
            || &row.source_thread_id == thread_id
    };
    let visible_local: HashSet<&TurnItemId> = visible
        .iter()
        .filter(|row| is_local(row))
        .map(|row| &row.source_item_id)
        .collect();

    let mut kept: Vec<OrchestrationV2TurnItem> = projection
        .turn_items
        .iter()
        .filter(|item| visible_local.contains(turn_item::common(item).id))
        .cloned()
        .collect();
    // Interrupt requests outside the window stay: a visible result may need them.
    kept.extend(
        projection
            .turn_items
            .iter()
            .filter(|item| {
                turn_item::type_tag(item) == "run_interrupt_request"
                    && !visible_local.contains(turn_item::common(item).id)
            })
            .cloned(),
    );
    kept
}
