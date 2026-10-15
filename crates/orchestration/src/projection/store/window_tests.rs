//! Ports of the SQL window tests in ProjectionStore.test.ts: histories are
//! seeded straight into the projection tables, then read through
//! `get_thread_snapshot_window` and paged with the cursor helpers.

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::OrchestrationV2ProjectedTurnItem;
use turin_contracts::{ThreadId, TurnItemId};

use crate::projection::store::canonical::{
    RETAINED_NODE_IDS_SQL, WINDOW_NODES_SQL, WINDOW_TURN_ITEMS_SQL,
};
use crate::projection::store::{ProjectionStore, ThreadSnapshotWindowOptions};
use crate::projection::test_support::*;
use crate::projection::thread_history_paging::{
    THREAD_HISTORY_PAGE_POLICY, ThreadHistoryPagePolicy, decode_thread_history_cursor,
    projected_row_encoded_bytes, select_history_page_from_cursor, select_recent_timeline_window,
};

fn window(row_limit: i64, user_turn_limit: Option<i64>) -> ThreadSnapshotWindowOptions {
    ThreadSnapshotWindowOptions {
        row_limit,
        user_turn_limit,
        ..Default::default()
    }
}

fn item_ids(rows: &[OrchestrationV2ProjectedTurnItem]) -> Vec<String> {
    rows.iter()
        .map(|row| row.source_item_id.to_string())
        .collect()
}

/// Upstream's `buildBoundedThreadProjection` window selection, without the
/// socket byte budget.
fn recent_visible(
    rows: &[OrchestrationV2ProjectedTurnItem],
    policy: ThreadHistoryPagePolicy,
) -> crate::projection::thread_history_paging::SelectTimelinePageResult<
    OrchestrationV2ProjectedTurnItem,
> {
    select_recent_timeline_window(rows, 0, Some(policy), &projected_row_encoded_bytes)
}

async fn create_thread(store: &ProjectionStore, id: &str) {
    apply_all(store, vec![event("thread.created", id, T0, thread(id))]).await;
}

#[tokio::test]
async fn pages_complete_user_turns_through_sql_regardless_of_tool_count_or_payload_size() {
    let (store, _dir) = file_store().await;
    create_thread(&store, "thread:user-turn-pages").await;
    let mut all_ids = Vec::new();
    for turn in 1..=45u64 {
        for offset in 0..102u64 {
            let ordinal = (turn - 1) * 102 + offset + 1;
            all_ids.push(format!("item:user-turn-pages:{ordinal}"));
        }
    }
    let ids = all_ids.clone();
    seed(&store, move |conn| {
        for turn in 1..=45u64 {
            for offset in 0..102u64 {
                let ordinal = (turn - 1) * 102 + offset + 1;
                let id = format!("item:user-turn-pages:{ordinal}");
                let item = if offset < 2 {
                    turn_item(
                        "thread:user-turn-pages",
                        &id,
                        None,
                        ordinal,
                        "user_message",
                        "completed",
                        json!({
                            "createdBy": "user", "creationSource": "web", "messageId": format!("message:{id}"),
                            "inputIntent": if offset == 0 { "turn_start" } else { "steer" },
                            "text": format!("Turn {turn}"), "attachments": []
                        }),
                    )
                } else {
                    turn_item(
                        "thread:user-turn-pages",
                        &id,
                        None,
                        ordinal,
                        "command_execution",
                        "completed",
                        json!({"input": "command", "output": "x".repeat(2048), "exitCode": 0}),
                    )
                };
                insert_item_row(conn, &item);
            }
        }
    })
    .await;
    let thread_id = tid("thread:user-turn-pages");
    let initial = store
        .get_thread_snapshot_window(&thread_id, window(77, Some(10)))
        .await
        .unwrap();
    // The boundary comes from the user-message index rather than a scan of
    // every tool row, and payloads are not sorted after they are fetched.
    let plan = query_plan(&store, WINDOW_TURN_ITEMS_SQL, 7).await;
    let details: Vec<_> = plan.iter().map(|(_, detail)| detail.as_str()).collect();
    assert!(
        details.iter().any(|detail| detail.contains(
            "SEARCH item USING INDEX orchestration_v2_projection_turn_items_user_message_idx (thread_id=? AND ordinal<?)"
        )),
        "{details:#?}"
    );
    let top_level: Vec<_> = plan
        .iter()
        .filter(|(parent, _)| *parent == 0)
        .map(|(_, d)| d.as_str())
        .collect();
    assert!(
        top_level.contains(&"SEARCH item USING INTEGER PRIMARY KEY (rowid=?)"),
        "{top_level:#?}"
    );
    assert!(
        !top_level.contains(&"USE TEMP B-TREE FOR ORDER BY"),
        "{top_level:#?}"
    );
    let node_plan = query_plan(&store, RETAINED_NODE_IDS_SQL, 4).await;
    assert!(
        node_plan.iter().any(|(_, detail)| detail.contains(
            "SEARCH orchestration_v2_projection_nodes USING INDEX orchestration_v2_projection_nodes_live_idx (thread_id=?)"
        )),
        "{node_plan:#?}"
    );

    // Only the selected turn cohort and two lookahead anchors are decoded.
    assert_eq!(initial.projection.turn_items.len(), 12 * 102);
    let policy = ThreadHistoryPagePolicy {
        max_encoded_bytes: usize::MAX,
        ..THREAD_HISTORY_PAGE_POLICY
    };
    let first = recent_visible(&initial.projection.visible_turn_items, policy);
    assert_eq!(first.items.len(), 10 * 102);
    assert_eq!(first.items[0].source_item_id.as_str(), ids[35 * 102]);
    let mut loaded = item_ids(&first.items);
    let mut cursor = first.next_cursor;
    for turns in [20usize, 15] {
        let cursor_text = cursor.clone().expect("another page");
        let anchor = decode_thread_history_cursor(&cursor_text).unwrap();
        let snapshot = store
            .get_thread_snapshot_window(
                &thread_id,
                ThreadSnapshotWindowOptions {
                    row_limit: 77,
                    user_turn_limit: Some(20),
                    anchor_item_id: Some(TurnItemId::new(&anchor.si).unwrap()),
                    anchor_thread_id: Some(ThreadId::new(&anchor.st).unwrap()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let page = select_history_page_from_cursor(
            &snapshot.projection.visible_turn_items,
            &cursor_text,
            0,
            None,
            &projected_row_encoded_bytes,
            None,
        )
        .unwrap();
        assert_eq!(page.items.len(), turns * 102);
        let mut ids_so_far = item_ids(&page.items);
        ids_so_far.extend(loaded);
        loaded = ids_so_far;
        cursor = page.next_cursor;
    }
    assert!(cursor.is_none());
    assert_eq!(loaded, all_ids);
}

#[tokio::test]
async fn caps_a_sql_window_by_runs_that_a_notification_started() {
    // A thread woken by PR watches: 12 user turns, then 400 runs that a
    // notification started, with a user turn every 50 runs. The user-turn
    // window reached back to the 12th-newest user turn and read almost every row.
    let (store, _dir) = file_store().await;
    let thread_name = "thread:notification-turn-pages";
    create_thread(&store, thread_name).await;
    let started_runs = 412u64;
    let (all_ids, start_ids) = {
        let mut all_ids = Vec::new();
        let mut start_ids = Vec::new();
        let mut ordinal = 0u64;
        let mut next = |all: &mut Vec<String>| {
            ordinal += 1;
            let id = format!("turn-item:notification-turn-pages:{ordinal}");
            all.push(id.clone());
            (id, ordinal)
        };
        for run in 1..=started_runs {
            let ids: Vec<_> = (0..4).map(|_| next(&mut all_ids)).collect();
            let _ = run;
            start_ids.push(ids[1].0.clone());
        }
        (all_ids, start_ids)
    };
    let seeded_ids = all_ids.clone();
    seed(&store, move |conn| {
        let mut cursor = 0usize;
        for run in 1..=started_runs + 1 {
            let run_id = format!("run:notification-turn-pages:{run}");
            let status = if run > started_runs { "queued" } else { "completed" };
            let mut run_json = crate::projection::test_support::run(thread_name, &run_id, run, status);
            run_json["completedAt"] = json!(T0);
            insert_run_row(conn, &run_json);
            if run > started_runs {
                continue;
            }
            let start = if run <= 12 || run % 50 == 0 {
                json!({"type": "user_message", "createdBy": "user", "creationSource": "web",
                    "messageId": format!("message:notification-turn-pages:{run}"),
                    "inputIntent": "turn_start", "text": format!("Turn {run}"), "attachments": []})
            } else {
                json!({"type": "notification", "source": {"kind": "monitor"},
                    "outcome": "completed", "summary": format!("Wake {run}")})
            };
            let command = json!({"type": "command_execution", "input": "command", "output": "x", "exitCode": 0});
            let steer = json!({"type": "notification", "source": {"kind": "monitor"},
                "outcome": "completed", "summary": format!("Steer {run}")});
            for (index, extra) in [command.clone(), start, command, steer].into_iter().enumerate() {
                let id = &seeded_ids[cursor + index];
                let kind = extra["type"].as_str().unwrap().to_owned();
                let mut item = turn_item(
                    thread_name,
                    id,
                    Some(&run_id),
                    (cursor + index) as u64 + 1,
                    &kind,
                    "completed",
                    extra,
                );
                item["nodeId"] = Value::Null;
                insert_item_row(conn, &item);
            }
            cursor += 4;
        }
    })
    .await;

    let thread_id = tid(thread_name);
    let initial = store
        .get_thread_snapshot_window(&thread_id, window(77, Some(10)))
        .await
        .unwrap();
    // The 152 newest turns are the 152 newest started runs, 261 to 412. Only
    // three of them are user turns, short of the 12 the user-turn window needs.
    let from = all_ids.iter().position(|id| id == &start_ids[260]).unwrap();
    let got: Vec<_> = initial
        .projection
        .turn_items
        .iter()
        .map(|item| crate::projection::turn_item::common(item).id.to_string())
        .collect();
    assert_eq!(got, all_ids[from..].to_vec());
    // Each run's first input is read by run ID, newest run first, never by a
    // walk over every turn item in the thread.
    let plan = query_plan(&store, WINDOW_TURN_ITEMS_SQL, 7).await;
    let details: Vec<_> = plan.iter().map(|(_, detail)| detail.as_str()).collect();
    assert!(details.contains(
        &"SEARCH recent_run USING INDEX orchestration_v2_projection_runs_thread_ordinal_idx (thread_id=? AND ordinal<?)"
    ), "{details:#?}");
    assert!(details.contains(
        &"SEARCH first_input USING INDEX orchestration_v2_projection_turn_items_run_ordinal_idx (run_id=?)"
    ), "{details:#?}");

    let policy = ThreadHistoryPagePolicy {
        max_encoded_bytes: usize::MAX,
        ..THREAD_HISTORY_PAGE_POLICY
    };
    let first = recent_visible(&initial.projection.visible_turn_items, policy);
    assert_eq!(first.items[0].source_item_id.as_str(), start_ids[262]);
    let mut loaded = item_ids(&first.items);
    let mut cursor = first.next_cursor;
    let mut pages = 0;
    while let Some(cursor_text) = cursor.clone() {
        assert!(pages < 5);
        pages += 1;
        let anchor = decode_thread_history_cursor(&cursor_text).unwrap();
        let snapshot = store
            .get_thread_snapshot_window(
                &thread_id,
                ThreadSnapshotWindowOptions {
                    row_limit: 77,
                    user_turn_limit: Some(20),
                    anchor_item_id: Some(TurnItemId::new(&anchor.si).unwrap()),
                    anchor_thread_id: Some(ThreadId::new(&anchor.st).unwrap()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // Older windows stay bounded too.
        assert!(snapshot.projection.turn_items.len() <= 152 * 4 + 1);
        let page = select_history_page_from_cursor(
            &snapshot.projection.visible_turn_items,
            &cursor_text,
            0,
            None,
            &projected_row_encoded_bytes,
            None,
        )
        .unwrap();
        let mut merged = item_ids(&page.items);
        merged.extend(loaded);
        loaded = merged;
        cursor = page.next_cursor;
    }
    assert_eq!(loaded, all_ids);
}

#[tokio::test]
async fn bounds_completed_nodes_within_one_long_run_while_preserving_ancestry_and_live_work() {
    let store = memory_store().await;
    let name = "thread:bounded-node-history";
    create_thread(&store, name).await;
    let run_id = "run:bounded-node-history";
    let root = "node:bounded-node-history:root";
    let mut run_json = run(name, run_id, 1, "running");
    run_json["rootNodeId"] = json!(root);
    apply_all(&store, vec![event("run.created", name, T0, run_json)]).await;
    let parent = "node:bounded-node-history:parent";
    let live = "node:bounded-node-history:live";
    let mut events = Vec::new();
    for index in -3i64..1000 {
        let id = match index {
            -3 => root.to_owned(),
            -2 => parent.to_owned(),
            -1 => live.to_owned(),
            _ => format!("node:bounded-node-history:{index}"),
        };
        let started_at = match index {
            998 => Value::Null,
            999 => json!("2020-01-01T00:00:00.000Z"),
            _ => json!(T0),
        };
        events.push(event(
            "node.updated",
            name,
            T0,
            json!({
                "id": id, "threadId": name, "runId": run_id, "rootNodeId": root,
                "parentNodeId": match index { -3 => Value::Null, -2 => json!(root), _ => json!(parent) },
                "kind": if index == -3 { "root_turn" } else { "assistant_message" },
                "status": if index == -1 { "running" } else { "completed" },
                "countsForRun": index == -3, "providerThreadId": null, "providerTurnId": null,
                "nativeItemRef": null, "runtimeRequestId": null, "checkpointScopeId": null,
                "startedAt": started_at, "completedAt": if index == -1 { Value::Null } else { json!(T0) }
            }),
        ));
        if index >= 0 {
            events.push(event("turn-item.updated", name, T0, {
                let mut item = turn_item(
                    name,
                    &format!("item:bounded-node-history:{index}"),
                    Some(run_id),
                    index as u64,
                    "command_execution",
                    "completed",
                    json!({"input": format!("echo {index}"), "output": "ok", "exitCode": 0}),
                );
                item["nodeId"] = json!(id);
                item
            }));
        }
    }
    apply_all(&store, events).await;
    let request_node = "node:bounded-node-history:0";
    apply_all(
        &store,
        vec![event(
            "runtime-request.updated",
            name,
            T0,
            json!({
                "id": "request:bounded-node-history", "nodeId": request_node,
                "providerTurnId": "provider-turn:bounded-node-history", "nativeRequestRef": null,
                "kind": "command", "status": "pending",
                "responseCapability": {"type": "live", "providerSessionId": "session:bounded-node-history"},
                "createdAt": T0, "resolvedAt": null
            }),
        )],
    )
    .await;

    // Cost must follow the retained cohort, even when the thread has thousands
    // of completed nodes outside the requested window.
    let plan = query_plan(&store, WINDOW_NODES_SQL, 2).await;
    let lookups: Vec<_> = plan
        .iter()
        .map(|(_, detail)| detail.as_str())
        .filter(|detail| detail.starts_with("SEARCH "))
        .collect();
    assert_eq!(
        lookups,
        [
            "SEARCH node USING INDEX sqlite_autoindex_orchestration_v2_projection_nodes_1 (node_id=?)"
        ]
    );
    assert!(
        !plan
            .iter()
            .any(|(_, detail)| detail == "USE TEMP B-TREE FOR ORDER BY")
    );

    let thread_id = tid(name);
    let snapshot = store
        .get_thread_snapshot_window(&thread_id, window(75, None))
        .await
        .unwrap();
    assert_eq!(snapshot.projection.visible_turn_items.len(), 75);
    assert_eq!(snapshot.projection.nodes.len(), 79);
    let retained: std::collections::HashSet<_> = snapshot
        .projection
        .nodes
        .iter()
        .map(|node| node.id.to_string())
        .collect();
    for expected in [root, parent, live, request_node] {
        assert!(retained.contains(expected), "{expected}");
    }
    assert_eq!(snapshot.projection.runtime_requests.len(), 1);
    assert!(!retained.contains("node:bounded-node-history:1"));
    for row in &snapshot.projection.visible_turn_items {
        let node = crate::projection::turn_item::common(&row.item)
            .node_id
            .unwrap();
        assert!(retained.contains(node.as_str()));
    }
    let older = store
        .get_thread_snapshot_window(
            &thread_id,
            ThreadSnapshotWindowOptions {
                row_limit: 75,
                anchor_item_id: Some(TurnItemId::new("item:bounded-node-history:925").unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(older.projection.nodes.len(), 79);
    assert!(
        older
            .projection
            .nodes
            .iter()
            .any(|node| node.id.as_str() == "node:bounded-node-history:851")
    );
    let full = store.get_thread_snapshot(&thread_id).await.unwrap();
    assert_eq!(full.projection.nodes.len(), 1003);
    // Bounded reads keep the full read's payloads and ordering, including null
    // start times and equal timestamps.
    let expected: Vec<_> = full
        .projection
        .nodes
        .iter()
        .filter(|node| retained.contains(node.id.as_str()))
        .cloned()
        .collect();
    assert_eq!(snapshot.projection.nodes, expected);
    // Two items sharing a node keep the window correct.
    seed(&store, |conn| {
        conn.execute(
            "INSERT INTO orchestration_v2_projection_turn_items
             SELECT 'item:bounded-node-history:duplicate', thread_id, run_id, node_id, provider_thread_id,
               provider_turn_id, parent_item_id, ordinal + 1, type, status, updated_at,
               json_set(payload_json, '$.id', 'item:bounded-node-history:duplicate', '$.ordinal', ordinal + 1)
             FROM orchestration_v2_projection_turn_items
             WHERE turn_item_id = 'item:bounded-node-history:999'",
            [],
        )
        .unwrap();
    })
    .await;
    let shared = store
        .get_thread_snapshot_window(&thread_id, window(76, None))
        .await
        .unwrap();
    assert_eq!(shared.projection.visible_turn_items.len(), 76);
    assert_eq!(shared.projection.nodes, snapshot.projection.nodes);
}

fn insert_command(
    conn: &rusqlite::Connection,
    thread: &str,
    id: &str,
    run_id: Option<&str>,
    ordinal: u64,
    title: &str,
) {
    insert_item_row(
        conn,
        &turn_item(
            thread,
            id,
            run_id,
            ordinal,
            "command_execution",
            "completed",
            json!({"title": title, "input": format!("echo {ordinal}"), "output": "ok", "exitCode": 0}),
        ),
    );
}

#[tokio::test]
async fn reads_a_fixed_sql_turn_item_window_for_long_histories_and_repeated_clients() {
    let (store, _dir) = file_store().await;
    let name = "thread:bounded-sql-history";
    create_thread(&store, name).await;
    seed(&store, move |conn| {
        for ordinal in 1..=1_000u64 {
            let run_id = format!("run:bounded-sql-history:{ordinal}");
            let mut run_json = run(name, &run_id, ordinal, "completed");
            run_json["completedAt"] = json!(T0);
            insert_run_row(conn, &run_json);
            let mut item = turn_item(
                name,
                &format!("turn-item:bounded-sql-history:{ordinal}"),
                Some(&run_id),
                ordinal,
                "command_execution",
                "completed",
                json!({"title": format!("command {ordinal}"), "input": format!("echo {ordinal}"), "output": "ok", "exitCode": 0}),
            );
            item["nodeId"] = Value::Null;
            insert_item_row(conn, &item);
        }
    })
    .await;
    let thread_id = tid(name);

    let snapshots = futures::future::join_all(
        (0..4).map(|_| store.get_thread_snapshot_window(&thread_id, window(76, None))),
    )
    .await;
    for snapshot in snapshots {
        let snapshot = snapshot.unwrap();
        assert_eq!(snapshot.projection.turn_items.len(), 76);
        assert_eq!(snapshot.projection.runs.len(), 76);
        assert_eq!(snapshot.projection.visible_turn_items.len(), 76);
        assert_eq!(
            crate::projection::turn_item::common(&snapshot.projection.turn_items[0]).ordinal,
            925
        );
        assert_eq!(
            crate::projection::turn_item::common(snapshot.projection.turn_items.last().unwrap())
                .ordinal,
            1_000
        );
    }
    let older = store
        .get_thread_snapshot_window(
            &thread_id,
            ThreadSnapshotWindowOptions {
                row_limit: 76,
                anchor_item_id: Some(TurnItemId::new("turn-item:bounded-sql-history:925").unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(older.projection.turn_items.len(), 76);
    assert_eq!(older.projection.runs.len(), 76);
    assert_eq!(
        crate::projection::turn_item::common(&older.projection.turn_items[0]).ordinal,
        850
    );
    assert_eq!(
        crate::projection::turn_item::common(older.projection.turn_items.last().unwrap()).ordinal,
        925
    );

    // Page the whole history back to the start.
    let page_limit = THREAD_HISTORY_PAGE_POLICY.max_items + 2;
    let policy = ThreadHistoryPagePolicy {
        max_user_turns: None,
        max_encoded_bytes: usize::MAX,
        ..THREAD_HISTORY_PAGE_POLICY
    };
    let initial = store
        .get_thread_snapshot_window(&thread_id, window(page_limit as i64, None))
        .await
        .unwrap();
    let first = recent_visible(&initial.projection.visible_turn_items, policy);
    let mut loaded = item_ids(&first.items);
    let mut cursor = first.next_cursor;
    let mut page_count = 1;
    while let Some(cursor_text) = cursor.clone() {
        let anchor = decode_thread_history_cursor(&cursor_text).unwrap();
        let snapshot = store
            .get_thread_snapshot_window(
                &thread_id,
                ThreadSnapshotWindowOptions {
                    row_limit: page_limit as i64,
                    anchor_item_id: Some(TurnItemId::new(&anchor.si).unwrap()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let page = select_history_page_from_cursor(
            &snapshot.projection.visible_turn_items,
            &cursor_text,
            snapshot.snapshot_sequence,
            Some(policy),
            &projected_row_encoded_bytes,
            None,
        )
        .unwrap();
        loaded.extend(item_ids(&page.items));
        page_count += 1;
        cursor = page.next_cursor;
    }
    assert!(page_count >= 4);
    assert_eq!(loaded.len(), 1_000);
    let unique: std::collections::HashSet<_> = loaded.iter().collect();
    assert_eq!(unique.len(), 1_000);
    let mut sorted = loaded.clone();
    sorted.sort_by_key(|id| id.rsplit(':').next().unwrap().parse::<u64>().unwrap());
    let expected: Vec<_> = (1..=1_000)
        .map(|i| format!("turn-item:bounded-sql-history:{i}"))
        .collect();
    assert_eq!(sorted, expected);

    // Physical insertion order must not change the ordinal/ID timeline order.
    seed(&store, |conn| {
        for id in [
            "turn-item:bounded-sql-history:tie:b",
            "turn-item:bounded-sql-history:tie:a",
        ] {
            conn.execute(
                "INSERT INTO orchestration_v2_projection_turn_items (
                   turn_item_id, thread_id, run_id, node_id, provider_thread_id, provider_turn_id,
                   parent_item_id, ordinal, type, status, updated_at, payload_json)
                 SELECT ?1, thread_id, run_id, node_id, provider_thread_id, provider_turn_id,
                   parent_item_id, 2000, type, status, updated_at,
                   json_set(payload_json, '$.id', ?1, '$.ordinal', 2000)
                 FROM orchestration_v2_projection_turn_items
                 WHERE turn_item_id = 'turn-item:bounded-sql-history:925'",
                [id],
            )
            .unwrap();
        }
    })
    .await;
    let tied = store
        .get_thread_snapshot_window(&thread_id, window(3, None))
        .await
        .unwrap();
    let tied_ids: Vec<_> = tied
        .projection
        .turn_items
        .iter()
        .map(|item| crate::projection::turn_item::common(item).id.to_string())
        .collect();
    assert_eq!(
        tied_ids,
        [
            "turn-item:bounded-sql-history:1000",
            "turn-item:bounded-sql-history:tie:a",
            "turn-item:bounded-sql-history:tie:b"
        ]
    );
    seed(&store, |conn| {
        conn.execute(
            "DELETE FROM orchestration_v2_projection_turn_items WHERE turn_item_id LIKE 'turn-item:bounded-sql-history:tie:%'",
            [],
        )
        .unwrap();
    })
    .await;

    // Rolled-back suffix rows never count against the page.
    seed(&store, move |conn| {
        let hidden = "run:bounded-sql-history:hidden-suffix";
        let mut run_json = run(name, hidden, 1001, "rolled_back");
        run_json["completedAt"] = json!(T0);
        insert_run_row(conn, &run_json);
        for ordinal in 1_001..=1_100u64 {
            let mut item = turn_item(name, &format!("turn-item:bounded-sql-history:hidden:{ordinal}"), Some(hidden), ordinal, "command_execution", "completed", json!({"title": "rolled back", "input": "echo hidden", "output": "hidden", "exitCode": 0}));
            item["nodeId"] = Value::Null;
            insert_item_row(conn, &item);
        }
    })
    .await;
    let hidden_suffix = store
        .get_thread_snapshot_window(&thread_id, window(page_limit as i64, None))
        .await
        .unwrap();
    let page = recent_visible(&hidden_suffix.projection.visible_turn_items, policy);
    assert_eq!(page.items.len(), 75);
    assert!(page.has_more_history);
    assert!(
        !page
            .items
            .iter()
            .any(|row| row.source_item_id.as_str().contains(":hidden:"))
    );
    let latest_local = hidden_suffix
        .projection
        .turn_items
        .iter()
        .map(|item| crate::projection::turn_item::common(item).ordinal)
        .max();
    assert_eq!(latest_local, Some(1_100));
    let cursor = page.next_cursor.unwrap();
    let anchor_item = TurnItemId::new(decode_thread_history_cursor(&cursor).unwrap().si).unwrap();
    let older_snapshot = store
        .get_thread_snapshot_window(
            &thread_id,
            ThreadSnapshotWindowOptions {
                row_limit: page_limit as i64,
                anchor_item_id: Some(anchor_item),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let older_page = select_history_page_from_cursor(
        &older_snapshot.projection.visible_turn_items,
        &cursor,
        older_snapshot.snapshot_sequence,
        Some(policy),
        &projected_row_encoded_bytes,
        None,
    )
    .unwrap();
    assert_eq!(older_page.items.len(), 75);
    assert!(older_page.has_more_history);

    // A cancelled run's queued messages are hidden; a runless queued message stays.
    seed(&store, move |conn| {
        let cancelled = "run:bounded-sql-history:cancelled-suffix";
        let mut run_json = run(name, cancelled, 1002, "cancelled");
        run_json["completedAt"] = json!(T0);
        insert_run_row(conn, &run_json);
        let queued = |id: &str, run_id: Option<&str>, ordinal: u64, title: &str, text: &str| {
            let mut item = turn_item(name, id, run_id, ordinal, "user_message", "completed", json!({
                "createdBy": "user", "creationSource": "web", "title": title,
                "messageId": format!("message:{id}"), "inputIntent": "queued_turn", "text": text, "attachments": []
            }));
            item["nodeId"] = Value::Null;
            item
        };
        for ordinal in 1_101..=1_200u64 {
            insert_item_row(conn, &queued(&format!("turn-item:bounded-sql-history:cancelled:{ordinal}"), Some(cancelled), ordinal, "cancelled queued message", "cancelled"));
        }
        insert_item_row(conn, &queued("turn-item:bounded-sql-history:runless-queued", None, 1201, "runless queued message", "still visible"));
        insert_item_row(conn, &queued("turn-item:bounded-sql-history:cancelled:1202", Some(cancelled), 1202, "later cancelled queued message", "cancelled"));
    })
    .await;
    let cancelled_suffix = store
        .get_thread_snapshot_window(&thread_id, window(page_limit as i64, None))
        .await
        .unwrap();
    let page = recent_visible(&cancelled_suffix.projection.visible_turn_items, policy);
    let latest_local = cancelled_suffix
        .projection
        .turn_items
        .iter()
        .map(|item| crate::projection::turn_item::common(item).ordinal)
        .max();
    assert_eq!(latest_local, Some(1_202));
    assert_eq!(page.items.len(), 75);
    assert!(
        !page
            .items
            .iter()
            .any(|row| row.source_item_id.as_str().contains(":cancelled:"))
    );
    assert!(
        page.items.iter().any(
            |row| row.source_item_id.as_str() == "turn-item:bounded-sql-history:runless-queued"
        )
    );

    // The request of a retained result stays even when it is far outside the window.
    seed(&store, move |conn| {
        let run_id = "run:bounded-sql-history:interrupt";
        let node_id = "node:bounded-sql-history:interrupt";
        let mut run_json = run(name, run_id, 1003, "completed");
        run_json["completedAt"] = json!(T0);
        run_json["rootNodeId"] = json!(node_id);
        run_json["providerThreadId"] = json!("provider-thread:interrupt");
        run_json["activeAttemptId"] = json!("attempt:bounded-sql-history:interrupt");
        insert_run_row(conn, &run_json);
        let attempt = json!({
            "id": "attempt:bounded-sql-history:interrupt", "runId": run_id, "attemptOrdinal": 1,
            "rootNodeId": node_id, "providerInstanceId": "codex", "providerThreadId": "provider-thread:interrupt",
            "providerTurnId": null, "reason": "initial", "status": "superseded", "startedAt": T0, "completedAt": T0
        });
        conn.execute(
            "INSERT INTO orchestration_v2_projection_run_attempts (
               attempt_id, thread_id, run_id, attempt_ordinal, root_node_id, provider,
               provider_instance_id, provider_thread_id, provider_turn_id, status, payload_json
             ) VALUES (?1, ?2, ?3, 1, ?4, 'codex', 'codex', 'provider-thread:interrupt', NULL, 'superseded', ?5)",
            rusqlite::params!["attempt:bounded-sql-history:interrupt", name, run_id, node_id, attempt.to_string()],
        )
        .unwrap();
        let interrupt = |id: &str, ordinal: u64, kind: &str| {
            let mut item = turn_item(name, id, Some(run_id), ordinal, kind, "completed", json!({
                "title": kind, "message": if kind == "run_interrupt_request" { "Stopping" } else { "Stopped" }
            }));
            item["nodeId"] = json!(node_id);
            item
        };
        insert_item_row(conn, &interrupt("turn-item:bounded-sql-history:interrupt-request", 1_201, "run_interrupt_request"));
        for ordinal in 1_202..=1_281u64 {
            insert_command(conn, name, &format!("turn-item:bounded-sql-history:interrupt-filler:{ordinal}"), None, ordinal, "filler");
        }
        insert_item_row(conn, &interrupt("turn-item:bounded-sql-history:interrupt-result", 1_282, "run_interrupt_result"));
    })
    .await;
    let interrupt = store
        .get_thread_snapshot_window(&thread_id, window(page_limit as i64, None))
        .await
        .unwrap();
    assert!(interrupt.projection.turn_items.iter().any(|item| {
        crate::projection::turn_item::common(item).id.as_str()
            == "turn-item:bounded-sql-history:interrupt-request"
    }));
    assert!(
        interrupt
            .projection
            .visible_turn_items
            .iter()
            .any(|row| row.source_item_id.as_str()
                == "turn-item:bounded-sql-history:interrupt-result")
    );

    // Agent prompts also anchor user-turn windows, so a child transcript pages by turn.
    seed(&store, |conn| {
        conn.execute(
            "UPDATE orchestration_v2_projection_turn_items
             SET payload_json = json_set(payload_json, '$.createdBy', 'agent')
             WHERE thread_id = 'thread:bounded-sql-history' AND type = 'user_message'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE orchestration_v2_projection_turn_items
             SET type = 'user_message',
               payload_json = json_set(payload_json,
                 '$.type', 'user_message', '$.inputIntent', 'turn_start',
                 '$.createdBy', 'agent', '$.creationSource', 'provider',
                 '$.messageId', 'message:bounded-sql-history:agent-prompt',
                 '$.text', 'Continue the child task', '$.attachments', json('[]'))
             WHERE turn_item_id = 'turn-item:bounded-sql-history:interrupt-filler:1281'",
            [],
        )
        .unwrap();
    })
    .await;
    let agent_window = store
        .get_thread_snapshot_window(
            &thread_id,
            window(
                page_limit as i64,
                THREAD_HISTORY_PAGE_POLICY
                    .max_user_turns
                    .map(|turns| turns as i64),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        agent_window
            .projection
            .visible_turn_items
            .iter()
            .filter(
                |row| crate::projection::turn_item::type_tag(&row.item) != "run_interrupt_request"
            )
            .count(),
        page_limit
    );
    assert!(
        agent_window
            .projection
            .visible_turn_items
            .iter()
            .any(|row| row.source_item_id.as_str()
                == "turn-item:bounded-sql-history:interrupt-filler:1281")
    );
}
