//! Port of upstream/packages/client-runtime/src/state/threadHistoryMerge.ts
//! (and the `ThreadHistoryMeta` of `threadState.ts`).
//!
//! `threadHistoryController.ts` (the per-thread `loadEarlier` registry) is
//! dropped: the UI asks the store for an earlier page directly.

use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;

use crate::state::contracts::{
    OrchestrationV2ProjectedTurnItemVisibility, OrchestrationV2TurnItem, ThreadId, TurnItemId,
};
use crate::state::support::timeline_visibility::is_orchestration_v2_turn_item_visible;
use crate::state::support::turn_item::{TurnItemExt, TurnItemType};
use crate::state::thread_projection::{ThreadProjection, VisibleTurnItem, VisibleTurnItemRow};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadHistoryMeta {
    pub history_cursor: Option<String>,
    pub has_more_history: bool,
    pub loading: bool,
    pub error: Option<String>,
    /// True after at least one older page was merged; skip monolithic cache growth.
    pub expanded: bool,
    /// Max local turn ordinal known for partial progressive windows. `None` when
    /// unknown (legacy cache) or when history meta is cleared for a full snapshot.
    pub latest_local_turn_ordinal: Option<u64>,
}

pub const EMPTY_THREAD_HISTORY_META: ThreadHistoryMeta = ThreadHistoryMeta {
    history_cursor: None,
    has_more_history: false,
    loading: false,
    error: None,
    expanded: false,
    latest_local_turn_ordinal: None,
};

/// Whether a history-page response still matches the cursor that started it.
pub fn is_active_history_request_cursor(request_cursor: &str, history: &ThreadHistoryMeta) -> bool {
    history.history_cursor.as_deref() == Some(request_cursor)
}

/// Clear history loading after an interrupted load-earlier only when the
/// request cursor is still active. Does not touch stream/status/error.
pub fn clear_active_history_loading(
    request_cursor: &str,
    history: &ThreadHistoryMeta,
) -> ThreadHistoryMeta {
    if !is_active_history_request_cursor(request_cursor, history) || !history.loading {
        return history.clone();
    }
    ThreadHistoryMeta {
        loading: false,
        ..history.clone()
    }
}

fn projected_item_key(row: &VisibleTurnItem) -> (ThreadId, TurnItemId) {
    (row.source_thread_id.clone(), row.source_item_id.clone())
}

fn renumber_visible(rows: Vec<VisibleTurnItemRow>) -> Arc<Vec<VisibleTurnItemRow>> {
    Arc::new(
        rows.into_iter()
            .enumerate()
            .map(|(position, row)| {
                if row.position == position as u64 {
                    row
                } else {
                    Arc::new(VisibleTurnItem {
                        position: position as u64,
                        ..(*row).clone()
                    })
                }
            })
            .collect(),
    )
}

/// Merge an older history page into the live projection. Dedupes by
/// sourceThreadId + sourceItemId, keeps chronological order (older first), and
/// preserves newer live rows already present in state. `None` when the page
/// changes nothing.
pub fn merge_older_history_into_projection(
    projection: &ThreadProjection,
    older_items: &[VisibleTurnItem],
) -> Option<ThreadProjection> {
    if older_items.is_empty() {
        return None;
    }

    let current_by_key: IndexMap<(ThreadId, TurnItemId), &VisibleTurnItemRow> = projection
        .visible_turn_items
        .iter()
        .map(|row| (projected_item_key(row), row))
        .collect();
    let mut turn_item_by_id: IndexMap<TurnItemId, Arc<OrchestrationV2TurnItem>> = projection
        .turn_items
        .iter()
        .map(|item| (item.id().clone(), Arc::clone(item)))
        .collect();
    let mut page_keys: HashSet<(ThreadId, TurnItemId)> = HashSet::new();
    let mut page: Vec<VisibleTurnItemRow> = Vec::new();
    for page_row in older_items {
        let key = projected_item_key(page_row);
        if page_keys.contains(&key) {
            continue;
        }
        let mut row: VisibleTurnItemRow = match current_by_key.get(&key) {
            Some(current) => Arc::clone(current),
            None => Arc::new(page_row.clone()),
        };
        if !current_by_key.contains_key(&key)
            && (row.visibility == OrchestrationV2ProjectedTurnItemVisibility::Local
                || row.source_thread_id == projection.thread.id)
            && let Some(current_item) = turn_item_by_id.get(&row.source_item_id)
        {
            // Hidden live items stay hidden. Cold snapshots also retain interrupt
            // requests outside the visible window; paging may reveal those requests.
            if current_item.item_type() != TurnItemType::RunInterruptRequest
                || !is_orchestration_v2_turn_item_visible(
                    current_item,
                    &projection.runs,
                    &projection.attempts,
                    &projection.turn_items,
                )
            {
                continue;
            }
            row = Arc::new(VisibleTurnItem {
                item: Arc::clone(current_item),
                ..(*row).clone()
            });
        }
        page_keys.insert(key);
        if (row.visibility == OrchestrationV2ProjectedTurnItemVisibility::Local
            || row.source_thread_id == projection.thread.id)
            && !turn_item_by_id.contains_key(&row.source_item_id)
        {
            turn_item_by_id.insert(row.source_item_id.clone(), Arc::clone(&row.item));
        }
        page.push(row);
    }
    if page.is_empty() {
        return None;
    }
    // Server page order fills gaps between rows from a partial conversation load.
    // Existing rows supply live values; rows outside this range keep their order.
    let mut visible = page;
    visible.extend(
        projection
            .visible_turn_items
            .iter()
            .filter(|row| !page_keys.contains(&projected_item_key(row)))
            .cloned(),
    );
    if visible.len() == projection.visible_turn_items.len()
        && visible
            .iter()
            .zip(projection.visible_turn_items.iter())
            .all(|(left, right)| Arc::ptr_eq(left, right))
    {
        return None;
    }
    Some(ThreadProjection {
        turn_items: Arc::new(turn_item_by_id.into_values().collect()),
        visible_turn_items: renumber_visible(visible),
        ..projection.clone()
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadHistoryPageMeta {
    pub next_cursor: Option<String>,
    pub has_more_history: bool,
}

pub fn apply_history_page_meta(
    current: &ThreadHistoryMeta,
    page: &ThreadHistoryPageMeta,
) -> ThreadHistoryMeta {
    ThreadHistoryMeta {
        history_cursor: page.next_cursor.clone(),
        has_more_history: page.has_more_history,
        loading: false,
        error: None,
        expanded: true,
        // Preserve the partial-timeline watermark across load-earlier pages.
        latest_local_turn_ordinal: current.latest_local_turn_ordinal,
    }
}

/// Whether a client timeline should render its load-earlier control.
pub fn should_show_load_earlier_control(history: &ThreadHistoryMeta) -> bool {
    history.has_more_history || history.error.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::contracts::OrchestrationV2DomainEvent;
    use crate::state::orchestration_v2_projection::apply_orchestration_v2_projection_event;
    use crate::state::test_fixtures::{from_json, v2_projection};
    use serde_json::{Value, json};

    const NOW: &str = "2026-06-20T00:00:00.000Z";

    fn thread() -> ThreadId {
        ThreadId::new("thread-v2").unwrap()
    }

    fn row(index: u64) -> VisibleTurnItem {
        let id = format!("item-{index}");
        let item: OrchestrationV2TurnItem = from_json(json!({
            "id": id, "type": "command_execution", "threadId": "thread-v2", "runId": null,
            "nodeId": null, "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
            "parentItemId": null, "ordinal": index + 1, "status": "completed",
            "title": format!("Command {index}"), "input": format!("cmd-{index}"),
            "output": format!("out-{index}"), "exitCode": 0,
            "startedAt": NOW, "completedAt": NOW, "updatedAt": NOW,
        }));
        VisibleTurnItem {
            position: index,
            visibility: OrchestrationV2ProjectedTurnItemVisibility::Local,
            source_thread_id: thread(),
            source_item_id: TurnItemId::new(id).unwrap(),
            item: Arc::new(item),
        }
    }

    fn with_title(row: &VisibleTurnItem, title: &str) -> VisibleTurnItem {
        let mut value = serde_json::to_value(&*row.item).unwrap();
        value["title"] = json!(title);
        VisibleTurnItem {
            item: Arc::new(from_json(value)),
            ..row.clone()
        }
    }

    fn projection(rows: &[VisibleTurnItem]) -> ThreadProjection {
        let mut projection = ThreadProjection::from(v2_projection());
        projection.turn_items = Arc::new(rows.iter().map(|row| Arc::clone(&row.item)).collect());
        projection.visible_turn_items = Arc::new(
            rows.iter()
                .enumerate()
                .map(|(position, row)| {
                    Arc::new(VisibleTurnItem {
                        position: position as u64,
                        ..row.clone()
                    })
                })
                .collect(),
        );
        projection
    }

    fn source_ids(projection: &ThreadProjection) -> Vec<String> {
        projection
            .visible_turn_items
            .iter()
            .map(|row| row.source_item_id.to_string())
            .collect()
    }

    fn merged(projection: &ThreadProjection, older: &[VisibleTurnItem]) -> ThreadProjection {
        merge_older_history_into_projection(projection, older).unwrap_or_else(|| projection.clone())
    }

    #[test]
    fn fills_activity_around_partial_messages_in_canonical_order_and_preserves_live_edits() {
        let initial = projection(&[row(4)]);
        let preview = [row(1), row(3)];
        let partial = merged(&initial, &preview);
        let updated = with_title(&row(1), "newer live title");
        let live = ThreadProjection {
            visible_turn_items: Arc::new(
                partial
                    .visible_turn_items
                    .iter()
                    .map(|entry| {
                        if entry.source_item_id == updated.source_item_id {
                            Arc::new(updated.clone())
                        } else {
                            Arc::clone(entry)
                        }
                    })
                    .collect(),
            ),
            turn_items: Arc::new(
                partial
                    .turn_items
                    .iter()
                    .map(|item| {
                        if item.id() == updated.item.id() {
                            Arc::clone(&updated.item)
                        } else {
                            Arc::clone(item)
                        }
                    })
                    .collect(),
            ),
            ..partial.clone()
        };
        let complete = merged(&live, &[row(0), row(1), row(2), row(3)]);
        assert_eq!(
            source_ids(&complete),
            vec!["item-0", "item-1", "item-2", "item-3", "item-4"]
        );
        assert!(Arc::ptr_eq(
            &complete.visible_turn_items[1].item,
            &updated.item
        ));
        assert_eq!(
            complete
                .visible_turn_items
                .iter()
                .map(|entry| entry.position)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
        assert_eq!(
            complete
                .turn_items
                .iter()
                .map(|item| item.id().to_string())
                .collect::<HashSet<_>>()
                .len(),
            5
        );
        let hidden = ThreadProjection {
            visible_turn_items: Arc::new(
                partial
                    .visible_turn_items
                    .iter()
                    .filter(|entry| entry.source_item_id != preview[0].source_item_id)
                    .cloned()
                    .collect(),
            ),
            ..partial
        };
        let hidden_complete = merged(&hidden, &[row(0), row(1), row(2), row(3)]);
        assert_eq!(
            source_ids(&hidden_complete),
            vec!["item-0", "item-2", "item-3", "item-4"]
        );
    }

    #[test]
    fn prepends_older_rows_and_dedupes_by_source_identity() {
        let projection = projection(&[row(2), row(3)]);
        let merged = merged(&projection, &[row(0), row(1), row(2)]);
        assert_eq!(
            source_ids(&merged),
            vec!["item-0", "item-1", "item-2", "item-3"]
        );
        assert_eq!(
            merged
                .visible_turn_items
                .iter()
                .map(|entry| entry.position)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert_eq!(
            merged
                .turn_items
                .iter()
                .map(|item| item.id().to_string())
                .collect::<Vec<_>>(),
            vec!["item-2", "item-3", "item-0", "item-1"]
        );
    }

    fn inherited(index: u64) -> VisibleTurnItem {
        VisibleTurnItem {
            visibility: OrchestrationV2ProjectedTurnItemVisibility::Inherited,
            source_thread_id: ThreadId::new("fork-source").unwrap(),
            ..row(index)
        }
    }

    #[test]
    fn fills_inherited_activity_without_confusing_identical_item_ids_from_different_source_threads()
    {
        let local = row(1);
        let preview = VisibleTurnItem {
            item: with_title(&row(1), "newer inherited title").item,
            ..inherited(1)
        };
        let mut base = projection(std::slice::from_ref(&local));
        base.visible_turn_items = Arc::new(vec![
            Arc::new(VisibleTurnItem {
                position: 0,
                ..preview.clone()
            }),
            Arc::new(VisibleTurnItem {
                position: 1,
                ..local.clone()
            }),
        ]);
        let merged_once = merged(&base, &[inherited(0), inherited(1)]);
        assert_eq!(
            merged_once
                .visible_turn_items
                .iter()
                .map(|entry| (
                    entry.source_thread_id.to_string(),
                    entry.source_item_id.to_string()
                ))
                .collect::<Vec<_>>(),
            vec![
                ("fork-source".to_owned(), "item-0".to_owned()),
                ("fork-source".to_owned(), "item-1".to_owned()),
                ("thread-v2".to_owned(), "item-1".to_owned()),
            ]
        );
        assert!(Arc::ptr_eq(
            &merged_once.visible_turn_items[1].item,
            &preview.item
        ));
        assert_eq!(merged_once.turn_items.len(), 1);
        assert_eq!(
            merge_older_history_into_projection(&merged_once, &[inherited(0), inherited(1)]),
            None
        );
    }

    #[test]
    fn retains_live_rows_that_arrived_after_the_older_page_was_fetched() {
        let merged = merged(&projection(&[row(5), row(6)]), &[row(3), row(4)]);
        assert_eq!(
            source_ids(&merged),
            vec!["item-3", "item-4", "item-5", "item-6"]
        );
    }

    #[test]
    fn does_not_resurrect_a_local_item_hidden_while_an_older_page_was_in_flight() {
        let stale = row(1);
        let mut base = ThreadProjection::from(v2_projection());
        base.turn_items = Arc::new(vec![with_title(&stale, "newer live").item]);
        assert_eq!(merge_older_history_into_projection(&base, &[stale]), None);
    }

    #[test]
    fn marks_history_expanded_after_a_successful_page() {
        let next = apply_history_page_meta(
            &ThreadHistoryMeta {
                history_cursor: Some("cursor-a".into()),
                has_more_history: true,
                loading: true,
                error: None,
                expanded: false,
                latest_local_turn_ordinal: Some(42),
            },
            &ThreadHistoryPageMeta {
                next_cursor: Some("cursor-b".into()),
                has_more_history: true,
            },
        );
        assert_eq!(
            next,
            ThreadHistoryMeta {
                history_cursor: Some("cursor-b".into()),
                has_more_history: true,
                loading: false,
                error: None,
                expanded: true,
                latest_local_turn_ordinal: Some(42),
            }
        );
    }

    #[test]
    fn only_applies_history_responses_for_the_active_request_cursor() {
        let at = |cursor: Option<&str>| ThreadHistoryMeta {
            history_cursor: cursor.map(str::to_owned),
            ..ThreadHistoryMeta::default()
        };
        assert!(is_active_history_request_cursor(
            "cursor-a",
            &at(Some("cursor-a"))
        ));
        assert!(!is_active_history_request_cursor(
            "cursor-a",
            &at(Some("cursor-b"))
        ));
        assert!(!is_active_history_request_cursor("cursor-a", &at(None)));
    }

    #[test]
    fn clears_loading_on_interrupt_only_for_the_active_request_cursor() {
        let loading = ThreadHistoryMeta {
            history_cursor: Some("cursor-a".into()),
            has_more_history: true,
            loading: true,
            error: None,
            expanded: false,
            latest_local_turn_ordinal: Some(10),
        };
        assert_eq!(
            clear_active_history_loading("cursor-a", &loading),
            ThreadHistoryMeta {
                loading: false,
                ..loading.clone()
            }
        );
        // Stale cursor must not mutate replacement progressive meta.
        let other = ThreadHistoryMeta {
            history_cursor: Some("cursor-b".into()),
            ..loading.clone()
        };
        assert_eq!(clear_active_history_loading("cursor-a", &other), other);
    }

    #[test]
    fn shows_the_load_earlier_control_when_history_remains_or_a_local_error_is_set() {
        assert!(should_show_load_earlier_control(&ThreadHistoryMeta {
            history_cursor: Some("c".into()),
            has_more_history: true,
            ..ThreadHistoryMeta::default()
        }));
        assert!(should_show_load_earlier_control(&ThreadHistoryMeta {
            error: Some("Could not load earlier activity.".into()),
            expanded: true,
            ..ThreadHistoryMeta::default()
        }));
        assert!(!should_show_load_earlier_control(
            &ThreadHistoryMeta::default()
        ));
    }

    #[test]
    fn retains_merged_older_rows_when_a_live_turn_item_update_arrives() {
        let projection = projection(&[row(2), row(3)]);
        let merged = merged(&projection, &[row(0), row(1)]);
        let mut live_value = serde_json::to_value(&*row(3).item).unwrap();
        live_value["output"] = json!("streamed");
        let event: OrchestrationV2DomainEvent = from_json(json!({
            "id": "event-live-item", "type": "turn-item.updated", "threadId": "thread-v2",
            "occurredAt": NOW, "payload": live_value.clone(),
        }));
        let next = apply_orchestration_v2_projection_event(&merged, event, None).unwrap();
        assert_eq!(
            source_ids(&next),
            vec!["item-0", "item-1", "item-2", "item-3"]
        );
        assert_eq!(
            serde_json::to_value(&*next.visible_turn_items[3].item).unwrap(),
            live_value
        );
    }

    #[test]
    fn keeps_a_history_page_interrupt_result_visible_when_its_request_was_retained_at_cold_open() {
        // Cold bounded window: only a recent filler. Request was retained in
        // turnItems (not visible). Result arrives later via load-earlier merge.
        let interrupt = |id: &str, kind: &str, ordinal: u64, message: &str| -> Value {
            json!({
                "id": id, "type": kind, "threadId": "thread-v2", "runId": "run-interrupt-cold",
                "nodeId": "node-interrupt-cold", "providerThreadId": null, "providerTurnId": null,
                "nativeItemRef": null, "parentItemId": null, "ordinal": ordinal, "status": "completed",
                "title": null, "message": message, "startedAt": NOW, "completedAt": NOW, "updatedAt": NOW,
            })
        };
        let request_item: OrchestrationV2TurnItem = from_json(interrupt(
            "item-interrupt-request",
            "run_interrupt_request",
            1,
            "Stopping",
        ));
        let result_item: OrchestrationV2TurnItem = from_json(interrupt(
            "item-interrupt-result",
            "run_interrupt_result",
            2,
            "Stopped",
        ));
        let result_row = VisibleTurnItem {
            position: 0,
            visibility: OrchestrationV2ProjectedTurnItemVisibility::Local,
            source_thread_id: thread(),
            source_item_id: result_item.id().clone(),
            item: Arc::new(result_item.clone()),
        };
        let mut cold = projection(&[row(5)]);
        cold.turn_items = Arc::new(vec![
            Arc::new(request_item.clone()),
            Arc::clone(&cold.turn_items[0]),
        ]);
        // The request is retained (hidden from the visible window); the page brings the result.
        let merged_page = merged(&cold, &[result_row]);
        assert!(source_ids(&merged_page).contains(&"item-interrupt-result".to_owned()));
        assert!(
            merged_page
                .turn_items
                .iter()
                .any(|item| item.id().as_str() == "item-interrupt-result")
        );
    }
}
