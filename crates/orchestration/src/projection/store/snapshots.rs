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
    ThreadHistoryPagePolicy, compute_latest_local_turn_ordinal, projected_row_encoded_bytes,
    select_recent_timeline_window,
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
    /// Max ordinal over the full projection's turn items before the window trim.
    pub latest_local_turn_ordinal: Option<u64>,
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
        Ok(build_bounded_thread_projection(
            snapshot.projection,
            snapshot.snapshot_sequence,
            None,
        ))
    }
}

/// `buildBoundedThreadProjection` over an already-loaded projection.
pub fn build_bounded_thread_projection(
    mut projection: OrchestrationV2ThreadProjection,
    snapshot_sequence: u64,
    policy: Option<ThreadHistoryPagePolicy>,
) -> BoundedThreadSnapshot {
    let latest_local_turn_ordinal = compute_latest_local_turn_ordinal(&projection.turn_items);
    let window = select_recent_timeline_window(
        &projection.visible_turn_items,
        snapshot_sequence,
        policy,
        &projected_row_encoded_bytes,
    );
    projection.turn_items = bounded_turn_items(&projection, &window.items);
    projection.visible_turn_items = window.items;
    BoundedThreadSnapshot {
        snapshot_sequence,
        projection,
        history_cursor: window.next_cursor,
        has_more_history: window.has_more_history,
        latest_local_turn_ordinal,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::thread_history_paging::{
        ThreadHistoryPagePolicy, compute_latest_local_turn_ordinal, projected_row_encoded_bytes,
        select_recent_timeline_window,
    };
    use serde_json::json;
    use turin_contracts::orchestration_v2::{
        OrchestrationV2ProjectedTurnItemVisibility, OrchestrationV2ThreadProjection,
    };

    const NOW: &str = "2026-06-20T00:00:00.000Z";
    const THREAD: &str = "thread-1";

    fn make_row(index: u64, output_bytes: Option<usize>) -> OrchestrationV2ProjectedTurnItem {
        let output = output_bytes
            .map(|bytes| "x".repeat(bytes))
            .unwrap_or_else(|| format!("output-{index}"));
        serde_json::from_value(json!({
            "position": index,
            "visibility": "local",
            "sourceThreadId": THREAD,
            "sourceItemId": format!("item-{index}"),
            "item": {
                "id": format!("item-{index}"), "type": "command_execution", "threadId": THREAD,
                "runId": null, "nodeId": null, "providerThreadId": null, "providerTurnId": null,
                "nativeItemRef": null, "parentItemId": null, "ordinal": index + 1,
                "status": "completed", "title": format!("Command {index}"),
                "input": format!("cmd-{index}"), "output": output, "exitCode": 0,
                "startedAt": NOW, "completedAt": NOW, "updatedAt": NOW
            }
        }))
        .unwrap()
    }

    fn interrupt_item(id: &str, type_tag: &str, ordinal: u64) -> OrchestrationV2TurnItem {
        serde_json::from_value(json!({
            "id": id, "type": type_tag, "threadId": THREAD, "runId": "run-interrupt-1",
            "nodeId": "node-interrupt-1", "providerThreadId": null, "providerTurnId": null,
            "nativeItemRef": null, "parentItemId": null, "ordinal": ordinal,
            "status": "completed", "title": type_tag,
            "message": if type_tag == "run_interrupt_request" { "Stopping" } else { "Stopped" },
            "startedAt": NOW, "completedAt": NOW, "updatedAt": NOW
        }))
        .unwrap()
    }

    fn interrupt_row(index: u64, type_tag: &str) -> OrchestrationV2ProjectedTurnItem {
        let id = if type_tag == "run_interrupt_request" {
            "interrupt-req"
        } else {
            "interrupt-res"
        };
        let item = interrupt_item(id, type_tag, index + 1);
        serde_json::from_value(json!({
            "position": index,
            "visibility": "local",
            "sourceThreadId": THREAD,
            "sourceItemId": id,
            "item": item
        }))
        .unwrap()
    }

    fn make_projection(
        visible_turn_items: Vec<OrchestrationV2ProjectedTurnItem>,
    ) -> OrchestrationV2ThreadProjection {
        let turn_items: Vec<_> = visible_turn_items
            .iter()
            .map(|row| row.item.clone())
            .collect();
        crate::pipeline::test_support::projection(
            THREAD,
            "project-1",
            json!({
                "turnItems": turn_items,
                "visibleTurnItems": visible_turn_items,
            }),
        )
    }

    fn items_budget(max_items: usize) -> ThreadHistoryPagePolicy {
        ThreadHistoryPagePolicy {
            max_user_turns: None,
            max_items: max_items,
            max_encoded_bytes: 10_000_000,
        }
    }

    #[test]
    fn carries_full_projection_watermark_when_bounded_window_is_inherited_only() {
        let local_older = make_row(0, None);
        let local_mid = make_row(1, None);
        let mut inherited = make_row(2, None);
        inherited.visibility = OrchestrationV2ProjectedTurnItemVisibility::Inherited;
        inherited.source_thread_id = ThreadId::new("parent-thread").expect("parent thread id");
        let full = make_projection(vec![
            local_older.clone(),
            local_mid.clone(),
            inherited.clone(),
        ]);
        let inherited_only = OrchestrationV2ThreadProjection {
            visible_turn_items: vec![inherited],
            turn_items: vec![local_older.item, local_mid.item],
            ..full
        };
        let bounded = build_bounded_thread_projection(inherited_only, 2, Some(items_budget(1)));
        assert_eq!(
            bounded
                .projection
                .visible_turn_items
                .iter()
                .map(|row| row.source_item_id.to_string())
                .collect::<Vec<_>>(),
            vec!["item-2"]
        );
        assert_eq!(bounded.latest_local_turn_ordinal, Some(2));
        assert_eq!(compute_latest_local_turn_ordinal(&[]), None);
    }

    #[test]
    fn retains_out_of_window_run_interrupt_request_needed_by_visible_result() {
        let request = interrupt_row(0, "run_interrupt_request");
        let filler = make_row(1, None);
        let result = interrupt_row(2, "run_interrupt_result");
        let full = make_projection(vec![request, filler, result]);
        let bounded = build_bounded_thread_projection(full, 3, Some(items_budget(1)));
        assert_eq!(
            bounded
                .projection
                .visible_turn_items
                .iter()
                .map(|row| row.source_item_id.to_string())
                .collect::<Vec<_>>(),
            vec!["interrupt-res"]
        );
        let mut turn_ids: Vec<_> = bounded
            .projection
            .turn_items
            .iter()
            .map(|item| turn_item::common(item).id.to_string())
            .collect();
        turn_ids.sort();
        assert_eq!(turn_ids, vec!["interrupt-req", "interrupt-res"]);
        assert!(bounded.has_more_history);
    }

    #[test]
    fn retains_run_interrupt_requests_when_no_result_is_in_the_initial_window() {
        let request = interrupt_row(0, "run_interrupt_request");
        let result = interrupt_row(1, "run_interrupt_result");
        let recent = make_row(2, None);
        let full = make_projection(vec![request, result, recent]);
        let bounded = build_bounded_thread_projection(full, 4, Some(items_budget(1)));
        assert_eq!(
            bounded
                .projection
                .visible_turn_items
                .iter()
                .map(|row| row.source_item_id.to_string())
                .collect::<Vec<_>>(),
            vec!["item-2"]
        );
        let mut turn_ids: Vec<_> = bounded
            .projection
            .turn_items
            .iter()
            .map(|item| turn_item::common(item).id.to_string())
            .collect();
        turn_ids.sort();
        assert_eq!(turn_ids, vec!["interrupt-req", "item-2"]);
        assert!(
            bounded
                .projection
                .turn_items
                .iter()
                .any(|item| turn_item::type_tag(item) == "run_interrupt_request")
        );
        assert!(
            !bounded
                .projection
                .turn_items
                .iter()
                .any(|item| turn_item::type_tag(item) == "run_interrupt_result")
        );
    }

    #[test]
    fn history_page_allows_more_rows_than_bounded_snapshot_without_double_charge() {
        let rows: Vec<_> = (0..8).map(|index| make_row(index, Some(2_000))).collect();
        let full = make_projection(rows.clone());
        let max_encoded_bytes = 12_000;
        let policy = ThreadHistoryPagePolicy {
            max_user_turns: None,
            max_items: 50,
            max_encoded_bytes,
        };
        let bounded = build_bounded_thread_projection(full, 1, Some(policy));
        let history_page =
            select_recent_timeline_window(&rows, 1, Some(policy), &projected_row_encoded_bytes);
        let history_bytes: usize = history_page
            .items
            .iter()
            .map(projected_row_encoded_bytes)
            .sum();
        assert!(history_bytes <= max_encoded_bytes);
        assert!(!bounded.projection.visible_turn_items.is_empty());
        assert!(bounded.projection.visible_turn_items.len() < rows.len());
        // Upstream's bounded snapshot double-charges local row bytes, so history
        // pages include fewer rows at the same cap. Turin single-counts (T22.md).
        assert!(history_page.items.len() >= bounded.projection.visible_turn_items.len());
    }
}
