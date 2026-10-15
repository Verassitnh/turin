//! Ports of the upstream `ProjectionStore.test.ts` cases that the shell,
//! recovery, fork and background-work reads depend on. Each test names its
//! upstream line. Events are applied in-process, like upstream
//! `projectionStore.apply(event)`; no `orchestration_events` rows are written.

use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::{Value, json};
use turin_contracts::orchestration_v2::OrchestrationV2ProjectedTurnItem;

use crate::projection::store::ProjectionStore;
use crate::projection::store::ThreadSnapshotWindowOptions;
use crate::projection::store::canonical::ProjectionRecordField;
use crate::projection::store::history::{TimelinePageOptions, TimelineView};
use crate::projection::store::recovery::ProjectionRecoveryKind;
use crate::projection::store::shell::ShellSnapshotOptions;
use crate::projection::test_support::*;

/// The wire (camelCase JSON) form of a contract value, for deep equality.
fn wire<T: Serialize + ?Sized>(value: &T) -> Value {
    serde_json::to_value(value).expect("serialize")
}

/// Shallow object merge, like the `{ ...base, key: value }` spreads upstream.
fn patch(mut base: Value, changes: Value) -> Value {
    base.as_object_mut()
        .expect("object base")
        .extend(changes.as_object().expect("object patch").clone());
    base
}

/// `[visibility, item.type, text-or-title]` per visible row, as upstream maps them.
fn visible_labels(rows: &[OrchestrationV2ProjectedTurnItem]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            let item = wire(&row.item);
            let kind = item["type"].as_str().expect("item type");
            let label = if matches!(kind, "user_message" | "assistant_message") {
                item["text"].as_str().expect("text").to_owned()
            } else {
                item["title"].as_str().unwrap_or("null").to_owned()
            };
            let visibility = wire(&row.visibility);
            format!(
                "{}|{kind}|{label}",
                visibility.as_str().expect("visibility")
            )
        })
        .collect()
}

/// Visible rows as `text` for messages and `type` otherwise (upstream's `row.item.type === ... ? text : type`).
fn visible_text_or_type(rows: &[OrchestrationV2ProjectedTurnItem]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            let item = wire(&row.item);
            let kind = item["type"].as_str().expect("item type");
            if matches!(kind, "user_message" | "assistant_message") {
                item["text"].as_str().expect("text").to_owned()
            } else {
                kind.to_owned()
            }
        })
        .collect()
}

fn visible_types(rows: &[OrchestrationV2ProjectedTurnItem]) -> Vec<String> {
    rows.iter()
        .map(|row| wire(&row.item)["type"].as_str().expect("type").to_owned())
        .collect()
}

/// A thread-created payload with the fields upstream spells out for each test.
fn thread_with(id: &str, changes: Value) -> Value {
    patch(thread(id), changes)
}

/// `addRolledBackRecoveryCandidate`: one run, rolled back while a command was still running.
async fn add_rolled_back_recovery_candidate(store: &ProjectionStore, suffix: &str) -> String {
    let thread_id = format!("thread:{suffix}:rolled-back");
    let run_id = format!("run:{suffix}:rolled-back");
    let item_id = format!("item:{suffix}:rolled-back");
    let rolled_back = patch(
        run(&thread_id, &run_id, 1, "rolled_back"),
        json!({ "completedAt": T0 }),
    );
    apply_all(
        store,
        vec![
            event("thread.created", &thread_id, T0, thread(&thread_id)),
            event(
                "run.created",
                &thread_id,
                T0,
                run(&thread_id, &run_id, 1, "running"),
            ),
            event(
                "turn-item.updated",
                &thread_id,
                T0,
                turn_item(
                    &thread_id,
                    &item_id,
                    Some(&run_id),
                    1,
                    "command_execution",
                    "running",
                    json!({ "title": "abandoned command", "input": "sleep 60" }),
                ),
            ),
            event("run.updated", &thread_id, T0, rolled_back),
        ],
    )
    .await;
    thread_id
}

/// `addOrphanedRecoveryCandidate`: a running command whose run row never arrived.
async fn add_orphaned_recovery_candidate(store: &ProjectionStore, suffix: &str) -> String {
    let thread_id = format!("thread:{suffix}:orphaned");
    let missing_run = format!("run:{suffix}:missing");
    apply_all(
        store,
        vec![
            event("thread.created", &thread_id, T0, thread(&thread_id)),
            event(
                "turn-item.updated",
                &thread_id,
                T0,
                turn_item(
                    &thread_id,
                    &format!("item:{suffix}:orphaned"),
                    Some(&missing_run),
                    1,
                    "command_execution",
                    "running",
                    json!({ "title": "orphaned command", "input": "sleep 60" }),
                ),
            ),
        ],
    )
    .await;
    thread_id
}

/// Insert a turn-item row directly, the way the upstream test writes raw
/// `orchestration_v2_projection_turn_items` rows. The node, provider-thread and
/// status columns come from the payload, as they do in the upstream fixtures.
fn insert_item_row(
    conn: &Connection,
    id: &str,
    thread: &str,
    run: Option<&str>,
    ordinal: i64,
    kind: &str,
    payload: &Value,
) {
    conn.execute(
        "INSERT INTO orchestration_v2_projection_turn_items (
            turn_item_id, thread_id, run_id, node_id, provider_thread_id, provider_turn_id,
            parent_item_id, ordinal, type, status, updated_at, payload_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, ?6, ?7, ?8, ?9, ?10)",
        params![
            id,
            thread,
            run,
            payload["nodeId"].as_str(),
            payload["providerThreadId"].as_str(),
            ordinal,
            kind,
            payload["status"].as_str(),
            T0,
            payload.to_string(),
        ],
    )
    .expect("insert turn item row");
}

/// A completed command execution as the raw-row fixtures spell it.
#[allow(clippy::too_many_arguments)] // mirrors the upstream fixture's option bag
fn command_payload(
    id: &str,
    thread: &str,
    run: &str,
    node: Option<&str>,
    provider: Option<&str>,
    ordinal: i64,
    title: &str,
    input: &str,
    output: &str,
) -> Value {
    json!({
        "id": id, "threadId": thread, "runId": run, "nodeId": node,
        "providerThreadId": provider, "providerTurnId": null, "nativeItemRef": null,
        "parentItemId": null, "ordinal": ordinal, "status": "completed", "title": title,
        "input": input, "output": output, "exitCode": 0, "startedAt": T0,
        "completedAt": T0, "updatedAt": T0, "type": "command_execution"
    })
}

/// Set a run's status and its `payload_json.status`, as the upstream SQL does.
fn set_run_status(conn: &Connection, run: &str, status: &str) {
    conn.execute(
        "UPDATE orchestration_v2_projection_runs
         SET status = ?1, payload_json = json_set(payload_json, '$.status', ?1)
         WHERE run_id = ?2",
        params![status, run],
    )
    .expect("update run status");
}

// ---------------------------------------------------------------------------

/// Upstream line 3010.
#[tokio::test]
async fn selects_only_threads_with_runtime_state_that_needs_recovery() {
    let (store, _dir) = file_store().await;
    let settled = "thread:recovery-candidates:settled";
    let running = "thread:recovery-candidates:running";
    let member = "thread:recovery-candidates:member";
    let child = "thread:recovery-candidates:child";
    let rolled_back = add_rolled_back_recovery_candidate(&store, "recovery-candidates").await;
    let orphaned = add_orphaned_recovery_candidate(&store, "recovery-candidates").await;

    let mut events = Vec::new();
    for id in [settled, running, member, child] {
        events.push(event("thread.created", id, T0, thread(id)));
    }
    let run_id = "run:recovery-candidates:running";
    events.push(event(
        "run.created",
        running,
        T0,
        run(running, run_id, 1, "running"),
    ));

    let member_node = "node:recovery-candidates:member";
    let coordinator_root = "node:recovery-candidates:coordinator-root";
    let child_root = "node:recovery-candidates:child-root";
    let native_task_ref =
        json!({"driver": "codex", "nativeId": "workflow:agent:1", "strength": "strong"});
    let member_node_json = json!({
        "id": member_node, "threadId": member, "runId": null,
        "parentNodeId": coordinator_root, "rootNodeId": coordinator_root,
        "kind": "subagent", "status": "running", "countsForRun": false,
        "providerThreadId": null, "providerTurnId": null,
        "nativeItemRef": native_task_ref, "runtimeRequestId": null,
        "checkpointScopeId": null, "startedAt": T0, "completedAt": null
    });
    let child_node_json = patch(
        member_node_json.clone(),
        json!({
            "id": child_root, "threadId": child, "parentNodeId": null,
            "rootNodeId": child_root, "kind": "root_turn"
        }),
    );
    events.push(event(
        "subagent.updated",
        member,
        T0,
        json!({
            "id": member_node, "threadId": member, "runId": null,
            "parentNodeId": coordinator_root, "origin": "provider_native",
            "createdBy": "agent", "driver": "codex", "providerInstanceId": "codex",
            "providerThreadId": null, "childThreadId": child,
            "nativeTaskRef": native_task_ref, "prompt": "Do the work",
            "title": "Member", "model": null, "status": "running",
            "result": null, "startedAt": T0, "completedAt": null, "updatedAt": T0
        }),
    ));
    events.push(event("node.updated", member, T0, member_node_json));
    events.push(event("node.updated", child, T0, child_node_json));
    apply_all(&store, events).await;

    let recovery_thread_ids: Vec<String> = store
        .get_recovery_thread_ids(ProjectionRecoveryKind::Runtime)
        .await
        .expect("recovery ids")
        .iter()
        .map(|id| id.as_str().to_owned())
        .collect();
    for expected in [running, orphaned.as_str(), member, child] {
        assert!(
            recovery_thread_ids.iter().any(|id| id == expected),
            "{expected} in {recovery_thread_ids:?}"
        );
    }
    assert!(!recovery_thread_ids.iter().any(|id| id == settled));
    assert!(!recovery_thread_ids.iter().any(|id| id == &rolled_back));

    let member_recovery = store
        .get_runtime_recovery_projection(&tid(member))
        .await
        .expect("member recovery");
    let child_recovery = store
        .get_runtime_recovery_projection(&tid(child))
        .await
        .expect("child recovery");
    assert_eq!(
        member_recovery
            .subagents
            .iter()
            .map(|subagent| wire(&subagent.id))
            .collect::<Vec<_>>(),
        vec![json!(member_node)]
    );
    assert_eq!(
        member_recovery
            .nodes
            .iter()
            .map(|node| wire(&node.id))
            .collect::<Vec<_>>(),
        vec![json!(member_node)]
    );
    assert_eq!(
        child_recovery
            .nodes
            .iter()
            .map(|node| wire(&node.id))
            .collect::<Vec<_>>(),
        vec![json!(child_root)]
    );

    let cancelled = patch(
        wire(&member_recovery.subagents[0]),
        json!({ "status": "cancelled", "completedAt": T0, "updatedAt": T0 }),
    );
    apply_all(
        &store,
        vec![event("subagent.updated", member, T0, cancelled)],
    )
    .await;
    let after_cancel = store
        .get_recovery_thread_ids(ProjectionRecoveryKind::Runtime)
        .await
        .expect("recovery ids after cancel");
    assert!(after_cancel.iter().any(|id| id.as_str() == child));
}

/// Upstream line 4018.
#[tokio::test]
async fn selects_the_latest_waiting_secret_only_from_active_runs() {
    let (store, _dir) = file_store().await;
    let suffix = "shell-pending-secret";
    let thread_id = add_rolled_back_recovery_candidate(&store, suffix).await;
    let run_id = format!("run:{suffix}:rolled-back");
    let node_id = format!("node:{suffix}:rolled-back");
    let secret = |id: &str, ordinal: u64, status: &str| {
        event(
            "turn-item.updated",
            &thread_id,
            T0,
            turn_item(
                &thread_id,
                id,
                Some(&run_id),
                ordinal,
                "secret_request",
                status,
                json!({
                    "nodeId": node_id,
                    "label": "Test credential",
                    "reason": "Test pending input",
                    "secretStatus": if status == "waiting" { "pending" } else { "saved" },
                }),
            ),
        )
    };
    apply_all(
        &store,
        vec![
            secret("secret:a", 2, "waiting"),
            secret("secret:b", 3, "waiting"),
            secret("secret:c", 4, "completed"),
        ],
    )
    .await;

    for status in [
        "preparing",
        "starting",
        "running",
        "waiting",
        "completed",
        "rolled_back",
    ] {
        let (run_for_sql, status_for_sql) = (run_id.clone(), status.to_owned());
        seed(&store, move |conn| {
            set_run_status(conn, &run_for_sql, &status_for_sql)
        })
        .await;
        let shell = store
            .get_shell_snapshot(ShellSnapshotOptions::default())
            .await
            .expect("shell");
        let thread = shell
            .threads
            .iter()
            .find(|candidate| candidate.id.as_str() == thread_id)
            .expect("thread shell");
        let expected = if matches!(status, "completed" | "rolled_back") {
            None
        } else {
            Some("secret:b")
        };
        assert_eq!(
            thread
                .pending_runtime_request
                .as_ref()
                .map(|request| request.id.as_str()),
            expected,
            "status {status}"
        );
    }
}

/// Upstream line 4085. The upstream body asserts shell counts over a stale
/// turn-item row and that the full projection fails to decode it. It has no
/// timing assertion; it is ignored here to match the requested `timing` marker.
#[ignore = "stale-row shell perf case; not a timing assertion upstream, run manually"]
#[tokio::test]
async fn builds_shell_snapshots_without_decoding_full_turn_item_payloads() {
    let store = memory_store().await;
    let thread_id = "thread:projection-shell-stale-item";
    apply_all(
        &store,
        vec![event("thread.created", thread_id, T0, thread(thread_id))],
    )
    .await;
    let stale = json!({
        "id": "turn-item:stale-user-message", "threadId": thread_id, "runId": null,
        "nodeId": null, "providerThreadId": null, "providerTurnId": null,
        "nativeItemRef": null, "parentItemId": null, "ordinal": 0, "status": "completed",
        "title": null, "startedAt": T0, "completedAt": T0, "updatedAt": T0,
        "type": "user_message", "messageId": "message:stale-user-message",
        "text": "stale user message", "attachments": []
    });
    seed(&store, move |conn| {
        insert_item_row(
            conn,
            "turn-item:stale-user-message",
            thread_id,
            None,
            0,
            "user_message",
            &stale,
        )
    })
    .await;

    let shell = store
        .get_shell_snapshot(ShellSnapshotOptions::default())
        .await
        .expect("shell");
    let rows: Vec<_> = shell
        .threads
        .iter()
        .filter(|thread| thread.id.as_str() == thread_id)
        .map(|thread| {
            (
                thread.id.as_str().to_owned(),
                thread.item_count,
                thread.visible_item_count,
                thread.status,
            )
        })
        .collect();
    assert_eq!(rows, vec![(thread_id.to_owned(), 1, 1, turn_status_idle())]);
    assert!(store.get_thread_projection(&tid(thread_id)).await.is_err());
}

fn turn_status_idle() -> turin_contracts::orchestration_v2::OrchestrationV2ShellThreadStatus {
    turin_contracts::orchestration_v2::OrchestrationV2ShellThreadStatus::Idle
}

/// Upstream line 4203.
#[tokio::test]
async fn counts_imported_runless_history_inherited_by_fork_shells() {
    let (store, _dir) = file_store().await;
    let source = "thread:projection-imported-fork-shell:source";
    let target = "thread:projection-imported-fork-shell:target";
    let source_run = "run:projection-imported-fork-shell:source";
    let source_thread = thread_with(
        source,
        json!({
            "createdBy": "system", "creationSource": "server",
            "historyOrigin": "v1_import"
        }),
    );
    let target_thread = thread_with(
        target,
        json!({
            "lineage": {"parentThreadId": source, "relationshipToParent": "fork", "rootThreadId": source},
            "forkedFrom": {"type": "run", "threadId": source, "runId": source_run}
        }),
    );
    apply_all(
        &store,
        vec![
            event("thread.created", source, T0, source_thread),
            event("thread.created", target, T0, target_thread),
            event(
                "run.created",
                source,
                T0,
                run(source, source_run, 1, "completed"),
            ),
        ],
    )
    .await;

    let assistant = |suffix: &str, run_id: Option<&str>, ordinal: u64| {
        event(
            "turn-item.updated",
            source,
            T0,
            turn_item(
                source,
                &format!("turn-item:projection-imported-fork-shell:{suffix}"),
                run_id,
                ordinal,
                "assistant_message",
                "completed",
                json!({
                    "nodeId": null,
                    "messageId": format!("message:projection-imported-fork-shell:{suffix}"),
                    "text": suffix,
                    "streaming": false
                }),
            ),
        )
    };
    apply_all(
        &store,
        vec![
            assistant("imported-one", None, 1),
            assistant("imported-two", None, 2),
            assistant("native-run", Some(source_run), 3),
        ],
    )
    .await;

    let shell = store
        .get_shell_snapshot(ShellSnapshotOptions::default())
        .await
        .expect("shell");
    let target_shell = shell
        .threads
        .iter()
        .find(|thread| thread.id.as_str() == target);
    let target_projection = store
        .get_thread_projection(&tid(target))
        .await
        .expect("target projection");

    let target_shell = target_shell.expect("target shell");
    assert_eq!(target_shell.item_count, 0);
    assert_eq!(target_shell.visible_item_count, 4);
    assert_eq!(target_projection.visible_turn_items.len(), 4);
}

/// Upstream line 4355.
#[tokio::test]
async fn removes_rolled_back_runs_from_the_active_visible_projection() {
    let (store, _dir) = file_store().await;
    let thread_id = "thread:projection-rollback-prune";
    let run_id = "run:projection-rollback-prune";
    let attempt_id = "attempt:projection-rollback-prune";
    let root = "node:projection-rollback-prune:root";
    let assistant_node = "node:projection-rollback-prune:assistant";
    let provider_thread = "provider-thread:projection-rollback-prune";
    let provider_turn = "provider-turn:projection-rollback-prune";
    let user_message = "message:projection-rollback-prune:user";
    let assistant_message = "message:projection-rollback-prune:assistant";

    let provider_thread_json = json!({
        "id": provider_thread, "driver": "codex", "providerInstanceId": "codex",
        "providerSessionId": null, "appThreadId": thread_id, "ownerNodeId": null,
        "nativeThreadRef": null, "nativeConversationHeadRef": null, "status": "active",
        "firstRunOrdinal": 1, "lastRunOrdinal": 1, "handoffIds": [], "forkedFrom": null,
        "createdAt": T0, "updatedAt": T0
    });
    let run_completed = patch(
        run(thread_id, run_id, 1, "completed"),
        json!({
            "rootNodeId": root, "providerThreadId": provider_thread,
            "userMessageId": user_message, "activeAttemptId": attempt_id
        }),
    );
    let root_node = json!({
        "id": root, "threadId": thread_id, "runId": run_id, "parentNodeId": null,
        "rootNodeId": root, "kind": "root_turn", "status": "completed", "countsForRun": true,
        "providerThreadId": provider_thread, "providerTurnId": null, "nativeItemRef": null,
        "runtimeRequestId": null, "checkpointScopeId": null, "startedAt": T0, "completedAt": T0
    });
    let assistant_node_json = json!({
        "id": assistant_node, "threadId": thread_id, "runId": run_id, "parentNodeId": root,
        "rootNodeId": root, "kind": "assistant_message", "status": "completed",
        "countsForRun": false, "providerThreadId": provider_thread,
        "providerTurnId": provider_turn, "nativeItemRef": null, "runtimeRequestId": null,
        "checkpointScopeId": null, "startedAt": T0, "completedAt": T0
    });

    let mut thread_json = thread(thread_id);
    thread_json["activeProviderThreadId"] = json!(provider_thread);
    let events = vec![
        event("thread.created", thread_id, T0, thread_json),
        event(
            "provider-thread.updated",
            thread_id,
            T0,
            provider_thread_json,
        ),
        event("run.created", thread_id, T0, run_completed),
        event(
            "run-attempt.created",
            thread_id,
            T0,
            json!({
                "id": attempt_id, "runId": run_id, "attemptOrdinal": 1, "rootNodeId": root,
                "providerInstanceId": "codex", "providerThreadId": provider_thread,
                "providerTurnId": provider_turn, "reason": "initial", "status": "completed",
                "startedAt": T0, "completedAt": T0
            }),
        ),
        event("node.updated", thread_id, T0, root_node.clone()),
        event("node.updated", thread_id, T0, assistant_node_json),
        event(
            "provider-turn.updated",
            thread_id,
            T0,
            json!({
                "id": provider_turn, "providerThreadId": provider_thread, "nodeId": root,
                "runAttemptId": attempt_id, "nativeTurnRef": null, "ordinal": 1,
                "status": "completed", "startedAt": T0, "completedAt": T0
            }),
        ),
        event(
            "message.updated",
            thread_id,
            T0,
            json!({
                "createdBy": "user", "creationSource": "web", "id": user_message,
                "threadId": thread_id, "runId": run_id, "nodeId": root, "role": "user",
                "text": "rolled back user", "attachments": [], "streaming": false,
                "createdAt": T0, "updatedAt": T0
            }),
        ),
        event(
            "message.updated",
            thread_id,
            T0,
            json!({
                "createdBy": "agent", "creationSource": "provider", "id": assistant_message,
                "threadId": thread_id, "runId": run_id, "nodeId": assistant_node,
                "role": "assistant", "text": "rolled back assistant", "attachments": [],
                "streaming": false, "createdAt": T0, "updatedAt": T0
            }),
        ),
        event(
            "turn-item.updated",
            thread_id,
            T0,
            turn_item(
                thread_id,
                "turn-item:projection-rollback-prune:user",
                Some(run_id),
                100,
                "user_message",
                "completed",
                json!({
                    "createdBy": "user", "creationSource": "web", "nodeId": root,
                    "providerThreadId": provider_thread, "messageId": user_message,
                    "inputIntent": "turn_start", "text": "rolled back user", "attachments": []
                }),
            ),
        ),
        event(
            "turn-item.updated",
            thread_id,
            T0,
            turn_item(
                thread_id,
                "turn-item:projection-rollback-prune:assistant",
                Some(run_id),
                101,
                "assistant_message",
                "completed",
                json!({
                    "nodeId": assistant_node, "providerThreadId": provider_thread,
                    "providerTurnId": provider_turn, "messageId": assistant_message,
                    "text": "rolled back assistant", "streaming": false
                }),
            ),
        ),
        event(
            "turn-item.updated",
            thread_id,
            T0,
            turn_item(
                thread_id,
                "turn-item:projection-rollback-prune:background",
                Some(run_id),
                300,
                "command_execution",
                "running",
                json!({
                    "nodeId": root, "providerThreadId": provider_thread,
                    "title": "rolled back background command", "input": "sleep 60"
                }),
            ),
        ),
    ];
    apply_all(&store, events).await;
    apply_all(
        &store,
        vec![
            event(
                "run.updated",
                thread_id,
                T0,
                patch(
                    run(thread_id, run_id, 1, "rolled_back"),
                    json!({
                        "rootNodeId": root, "providerThreadId": provider_thread,
                        "userMessageId": user_message, "activeAttemptId": attempt_id,
                        "completedAt": T0
                    }),
                ),
            ),
            event(
                "node.updated",
                thread_id,
                T0,
                patch(root_node, json!({ "status": "rolled_back" })),
            ),
        ],
    )
    .await;

    let projection = store
        .get_thread_projection(&tid(thread_id))
        .await
        .expect("projection");
    assert_eq!(
        projection
            .runs
            .iter()
            .map(|run| wire(&run.status))
            .collect::<Vec<_>>(),
        vec![json!("rolled_back")]
    );
    assert_eq!(
        projection
            .nodes
            .iter()
            .map(|node| (wire(&node.id), wire(&node.status)))
            .collect::<Vec<_>>(),
        vec![
            (json!(assistant_node), json!("completed")),
            (json!(root), json!("rolled_back")),
        ]
    );
    assert_eq!(projection.provider_turns.len(), 1);
    assert_eq!(projection.messages.len(), 2);
    assert_eq!(projection.turn_items.len(), 3);
    assert_eq!(projection.visible_turn_items.len(), 0);

    // A rolled-back run's background item is abandoned, not pending. The
    // shell must not report it as Waiting, or the sidebar shows Waiting for
    // work nothing will ever finish.
    let shell = store
        .get_shell_snapshot(ShellSnapshotOptions::default())
        .await
        .expect("shell");
    let rolled_back_shell = shell
        .threads
        .iter()
        .find(|entry| entry.id.as_str() == thread_id)
        .expect("rolled back shell");
    assert!(rolled_back_shell.latest_visible_message.is_none());
    assert!(rolled_back_shell.pending_background_tasks.is_empty());
}

/// A run with the ids and timestamps upstream spells out (`run()` leaves them null).
fn run_with(
    thread: &str,
    id: &str,
    ordinal: u64,
    status: &str,
    root: &str,
    user_message: &str,
    provider_thread: Option<&str>,
) -> Value {
    patch(
        run(thread, id, ordinal, status),
        json!({
            "rootNodeId": root, "userMessageId": user_message,
            "providerThreadId": provider_thread, "completedAt": T0
        }),
    )
}

/// `visibility|type` per visible row.
fn visible_kinds(rows: &[OrchestrationV2ProjectedTurnItem]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            format!(
                "{}|{}",
                wire(&row.visibility).as_str().expect("visibility"),
                wire(&row.item)["type"].as_str().expect("type")
            )
        })
        .collect()
}

/// Upstream line 4768.
///
/// Omitted, with no Turin counterpart: the `traceSqlStatements` assertions
/// (statement shapes from the SQL tracer) and the nested cursor walks, which
/// use `buildBoundedThreadProjection`, a cut socket-budget helper (see the
/// `thread_history_paging` module header).
#[ignore = "omits the SQL statement tracing assertions and the cut buildBoundedThreadProjection cursor walks (upstream lines 5102-5140 and 5385-5450)"]
#[tokio::test]
async fn keeps_fork_visible_items_stable_after_a_source_run_is_rolled_back() {
    let (store, _dir) = file_store().await;
    let source = "thread:projection-fork-source-rollback:source";
    let target = "thread:projection-fork-source-rollback:target";
    let source_pt = "provider-thread:projection-fork-source-rollback:source";
    let target_pt = "provider-thread:projection-fork-source-rollback:target";
    let run1 = "run:projection-fork-source-rollback:source:1";
    let run2 = "run:projection-fork-source-rollback:source:2";
    let run3 = "run:projection-fork-source-rollback:source:3";
    let node1 = "node:projection-fork-source-rollback:source:1";
    let node2 = "node:projection-fork-source-rollback:source:2";
    let node3 = "node:projection-fork-source-rollback:source:3";

    let mut events = vec![
        event(
            "thread.created",
            source,
            T0,
            thread_with(source, json!({ "activeProviderThreadId": source_pt })),
        ),
        event(
            "thread.created",
            target,
            T0,
            thread_with(
                target,
                json!({
                    "activeProviderThreadId": target_pt,
                    "lineage": {"parentThreadId": source, "relationshipToParent": "fork", "rootThreadId": source},
                    "forkedFrom": {"type": "run", "threadId": source, "runId": run2}
                }),
            ),
        ),
    ];
    for (ordinal, run_id, node_id, prompt, response) in [
        (1u64, run1, node1, "source one", "one"),
        (2u64, run2, node2, "source two", "two"),
    ] {
        let user_message = format!("message:projection-fork-source-rollback:user:{ordinal}");
        let assistant_message =
            format!("message:projection-fork-source-rollback:assistant:{ordinal}");
        events.push(event(
            "run.created",
            source,
            T0,
            run_with(
                source,
                run_id,
                ordinal,
                "completed",
                node_id,
                &user_message,
                Some(source_pt),
            ),
        ));
        events.push(event(
            "turn-item.updated",
            source,
            T0,
            turn_item(
                source,
                &format!("turn-item:projection-fork-source-rollback:user:{ordinal}"),
                Some(run_id),
                ordinal * 100,
                "user_message",
                "completed",
                json!({
                    "createdBy": "user", "creationSource": "web", "nodeId": node_id,
                    "providerThreadId": source_pt, "messageId": user_message,
                    "inputIntent": "turn_start", "text": prompt, "attachments": []
                }),
            ),
        ));
        events.push(event(
            "turn-item.updated",
            source,
            T0,
            turn_item(
                source,
                &format!("turn-item:projection-fork-source-rollback:assistant:{ordinal}"),
                Some(run_id),
                ordinal * 100 + 1,
                "assistant_message",
                "completed",
                json!({
                    "nodeId": node_id, "providerThreadId": source_pt,
                    "messageId": assistant_message, "text": response, "streaming": false
                }),
            ),
        ));
    }
    apply_all(&store, events).await;

    let target_before = store
        .get_thread_projection(&tid(target))
        .await
        .expect("target before");
    assert_eq!(
        visible_types(&target_before.visible_turn_items),
        [
            "user_message",
            "assistant_message",
            "user_message",
            "assistant_message",
            "fork"
        ]
    );

    apply_all(
        &store,
        vec![event(
            "run.updated",
            source,
            T0,
            run_with(
                source,
                run2,
                2,
                "rolled_back",
                node2,
                "message:projection-fork-source-rollback:user:2",
                Some(source_pt),
            ),
        )],
    )
    .await;

    let source_messages = store
        .get_thread_records(&tid(source), vec![ProjectionRecordField::Messages], None)
        .await
        .expect("source records")
        .messages;
    let source_projection = store
        .get_thread_projection(&tid(source))
        .await
        .expect("source projection");
    assert_eq!(wire(&source_messages), wire(&source_projection.messages));
    let target_after = store
        .get_thread_projection(&tid(target))
        .await
        .expect("target after");

    // Both shell reads drop the rolled-back run from the source count, while
    // the fork keeps the prefix it inherited.
    let shell_snapshot = store
        .get_shell_snapshot(ShellSnapshotOptions::default())
        .await
        .expect("shell snapshot");
    let source_listed = shell_snapshot
        .threads
        .iter()
        .find(|thread| thread.id.as_str() == source)
        .map(|thread| wire(&thread.item_count));
    let source_shell = store
        .get_thread_shell(&tid(source))
        .await
        .expect("source shell")
        .map(|shell| wire(&shell.item_count));
    assert_eq!(source_shell, Some(json!(2)));
    assert_eq!(source_listed, Some(json!(2)));

    let target_visible = target_after.visible_turn_items.len();
    let target_listed = shell_snapshot
        .threads
        .iter()
        .find(|thread| thread.id.as_str() == target)
        .map(|thread| wire(&thread.visible_item_count));
    let target_shell = store
        .get_thread_shell(&tid(target))
        .await
        .expect("target shell")
        .map(|shell| wire(&shell.visible_item_count));
    assert_eq!(target_shell, Some(json!(target_visible)));
    assert_eq!(target_listed, Some(json!(target_visible)));

    let forward = store
        .get_timeline_page(
            &tid(target),
            TimelinePageOptions {
                view: TimelineView::Activity,
                limit: 2,
                ..Default::default()
            },
        )
        .await
        .expect("forward page");
    assert_eq!(
        wire(&forward.items),
        wire(&target_after.visible_turn_items[..2])
    );
    assert_eq!(forward.total_items, target_visible);
    assert!(forward.has_more);
    let following = store
        .get_timeline_page(
            &tid(target),
            TimelinePageOptions {
                view: TimelineView::Activity,
                limit: 10,
                after_position: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("following page");
    assert_eq!(
        wire(&following.items),
        wire(&target_after.visible_turn_items[2..])
    );
    assert!(!following.has_more);

    assert_eq!(
        visible_labels(&target_after.visible_turn_items),
        [
            "inherited|user_message|source one",
            "inherited|assistant_message|one",
            "inherited|user_message|source two",
            "inherited|assistant_message|two",
            "synthetic|fork|Forked from conversation",
        ]
    );

    // Run three is a later source run, completed after the fork. The fork must not show it.
    apply_all(
        &store,
        vec![event(
            "run.updated",
            source,
            T0,
            run_with(
                source,
                run3,
                4,
                "completed",
                node3,
                "message:projection-fork-source-rollback:user:3",
                Some(source_pt),
            ),
        )],
    )
    .await;

    // 300 command rows on run three; ordinals 190..=199 and 310..=599.
    let post_fork_source = source;
    seed(&store, move |conn| {
        for index in 0..300i64 {
            let ordinal = if index < 10 { 190 + index } else { 300 + index };
            let id = format!("turn-item:projection-fork-source-rollback:post-fork:{index}");
            let payload = command_payload(
                &id,
                post_fork_source,
                run3,
                Some(node3),
                Some(source_pt),
                ordinal,
                "post fork",
                "echo later",
                "later",
            );
            insert_item_row(
                conn,
                &id,
                post_fork_source,
                Some(run3),
                ordinal,
                "command_execution",
                &payload,
            );
        }
    })
    .await;

    // Upstream 5102-5140, minus the statement-trace assertions: a bounded
    // fork hydrates no own turn items, only the inherited prefix and marker.
    let bounded_fork = store
        .get_thread_snapshot_window(
            &tid(target),
            ThreadSnapshotWindowOptions {
                row_limit: 2,
                ..Default::default()
            },
        )
        .await
        .expect("bounded fork");
    assert!(bounded_fork.projection.turn_items.is_empty());
    assert_eq!(
        visible_kinds(&bounded_fork.projection.visible_turn_items),
        [
            "inherited|user_message",
            "inherited|assistant_message",
            "synthetic|fork",
        ]
    );

    // Upstream 5140-5152: the anchored window over the fork.
    let parent_page = store
        .get_thread_snapshot_window(
            &tid(target),
            ThreadSnapshotWindowOptions {
                row_limit: 2,
                anchor_item_id: Some(
                    turin_contracts::TurnItemId::new(
                        "turn-item:projection-fork-source-rollback:assistant:1",
                    )
                    .unwrap(),
                ),
                ..Default::default()
            },
        )
        .await
        .expect("parent page");
    assert_eq!(
        visible_text_or_type(&parent_page.projection.visible_turn_items),
        ["source one", "one", "fork"]
    );

    // Upstream 5160-5200: an empty boundary run, then the fork points at it.
    let empty_boundary_run = "run:projection-fork-source-rollback:source:empty-boundary";
    let empty_boundary_node = "node:projection-fork-source-rollback:source:empty-boundary";
    apply_all(
        &store,
        vec![event(
            "run.updated",
            source,
            T0,
            run_with(
                source,
                empty_boundary_run,
                3,
                "completed",
                empty_boundary_node,
                "message:projection-fork-source-rollback:user:empty-boundary",
                Some(source_pt),
            ),
        )],
    )
    .await;
    seed(&store, move |conn| {
        conn.execute(
            "UPDATE orchestration_v2_projection_threads
             SET payload_json = json_set(payload_json, '$.forkedFrom.runId', ?1)
             WHERE thread_id = ?2",
            params![empty_boundary_run, target],
        )
        .expect("repoint fork");
    })
    .await;
    let empty_boundary_fork = store
        .get_thread_snapshot_window(
            &tid(target),
            ThreadSnapshotWindowOptions {
                row_limit: 2,
                ..Default::default()
            },
        )
        .await
        .expect("empty boundary fork");
    assert_eq!(
        visible_text_or_type(&empty_boundary_fork.projection.visible_turn_items),
        ["source two", "two", "fork"]
    );
    seed(&store, move |conn| {
        conn.execute(
            "UPDATE orchestration_v2_projection_threads
             SET payload_json = json_set(payload_json, '$.forkedFrom.runId', ?1)
             WHERE thread_id = ?2",
            params![run2, target],
        )
        .expect("restore fork");
    })
    .await;

    // Upstream 5205-5262: a target run with 102 command rows.
    let target_run = "run:projection-fork-source-rollback:target:1";
    let target_node = "node:projection-fork-source-rollback:target:1";
    apply_all(
        &store,
        vec![event(
            "run.updated",
            target,
            T0,
            run_with(
                target,
                target_run,
                1,
                "completed",
                target_node,
                "message:projection-fork-source-rollback:target:1",
                Some(target_pt),
            ),
        )],
    )
    .await;
    seed(&store, move |conn| {
        for ordinal in 1..=102i64 {
            let id = format!("turn-item:projection-fork-source-rollback:target:{ordinal}");
            let payload = command_payload(
                &id,
                target,
                target_run,
                Some(target_node),
                Some(target_pt),
                ordinal,
                &format!("target {ordinal}"),
                &format!("echo target {ordinal}"),
                "target",
            );
            insert_item_row(
                conn,
                &id,
                target,
                Some(target_run),
                ordinal,
                "command_execution",
                &payload,
            );
        }
    })
    .await;

    // Lineage rows on the source, split across runs one and two.
    seed(&store, move |conn| {
        for ordinal in (1i64..100).chain(101..200) {
            let run_id = if ordinal < 100 { run1 } else { run2 };
            let id = format!("turn-item:projection-fork-source-rollback:lineage:{ordinal}");
            let payload = command_payload(
                &id,
                source,
                run_id,
                None,
                None,
                ordinal,
                &format!("lineage {ordinal}"),
                "echo lineage",
                "lineage",
            );
            insert_item_row(
                conn,
                &id,
                source,
                Some(run_id),
                ordinal,
                "command_execution",
                &payload,
            );
        }
    })
    .await;

    // A superseded attempt on run two, with 100 hidden interrupt rows under it.
    let inherited_superseded_node = "node:projection-fork-source-rollback:inherited-superseded";
    seed(&store, move |conn| {
        let payload = json!({
            "id": "attempt:projection-fork-source-rollback:inherited-superseded",
            "runId": run2, "attemptOrdinal": 2, "rootNodeId": inherited_superseded_node,
            "providerInstanceId": "codex", "providerThreadId": source_pt, "providerTurnId": null,
            "reason": "retry", "status": "superseded", "startedAt": T0, "completedAt": T0
        });
        conn.execute(
            "INSERT INTO orchestration_v2_projection_run_attempts (
                attempt_id, thread_id, run_id, attempt_ordinal, root_node_id, provider,
                provider_instance_id, provider_thread_id, provider_turn_id, status, payload_json
            ) VALUES (?1, ?2, ?3, 2, ?4, 'codex', 'codex', ?5, NULL, 'superseded', ?6)",
            params![
                "attempt:projection-fork-source-rollback:inherited-superseded",
                source,
                run2,
                inherited_superseded_node,
                source_pt,
                payload.to_string(),
            ],
        )
        .expect("insert superseded attempt");
        for ordinal in 201..=300i64 {
            let id =
                format!("turn-item:projection-fork-source-rollback:hidden-interrupt:{ordinal}");
            let payload = json!({
                "id": id, "threadId": source, "runId": run2, "nodeId": inherited_superseded_node,
                "providerThreadId": source_pt, "providerTurnId": null, "nativeItemRef": null,
                "parentItemId": null, "ordinal": ordinal, "status": "completed", "title": "Stopped",
                "message": "Stopped", "startedAt": T0, "completedAt": T0, "updatedAt": T0,
                "type": "run_interrupt_result"
            });
            insert_item_row(
                conn,
                &id,
                source,
                Some(run2),
                ordinal,
                "run_interrupt_result",
                &payload,
            );
        }
    })
    .await;

    // Upstream 5345-5385: a nested fork of the target.
    let nested = "thread:projection-fork-source-rollback:nested";
    apply_all(
        &store,
        vec![event(
            "thread.created",
            nested,
            T0,
            thread_with(
                nested,
                json!({
                    "lineage": {"parentThreadId": target, "relationshipToParent": "fork", "rootThreadId": source},
                    "forkedFrom": {"type": "run", "threadId": target, "runId": target_run}
                }),
            ),
        )],
    )
    .await;
    let bounded_nested = store
        .get_thread_snapshot_window(
            &tid(nested),
            ThreadSnapshotWindowOptions {
                row_limit: 2,
                ..Default::default()
            },
        )
        .await
        .expect("bounded nested");
    assert_eq!(
        visible_types(&bounded_nested.projection.visible_turn_items),
        [
            "user_message",
            "assistant_message",
            "fork",
            "command_execution",
            "command_execution",
            "fork",
        ]
    );

    // Upstream 5387-5450 walks the nested history with cursors built by the
    // cut buildBoundedThreadProjection; it is omitted here.

    let full_nested = store
        .get_thread_projection(&tid(nested))
        .await
        .expect("full nested");
    let nested_forward = store
        .get_timeline_page(
            &tid(nested),
            TimelinePageOptions {
                view: TimelineView::Activity,
                limit: full_nested.visible_turn_items.len() + 1,
                ..Default::default()
            },
        )
        .await
        .expect("nested forward");
    assert_eq!(
        wire(&nested_forward.items),
        wire(&full_nested.visible_turn_items)
    );
    let message_page = store
        .get_timeline_page(
            &tid(nested),
            TimelinePageOptions {
                view: TimelineView::Messages,
                limit: 2,
                ..Default::default()
            },
        )
        .await
        .expect("nested messages");
    let expected_messages: Vec<Value> = full_nested
        .visible_turn_items
        .iter()
        .filter(|row| {
            matches!(
                wire(&row.item)["type"].as_str(),
                Some("user_message" | "assistant_message" | "proposed_plan")
            )
        })
        .take(2)
        .map(wire)
        .collect();
    assert_eq!(wire(&message_page.items), Value::Array(expected_messages));

    // Upstream 5455-5500: an empty middle fork, then a leaf forked from it.
    let empty_middle = "thread:projection-fork-source-rollback:empty-middle";
    let empty_middle_run = "run:projection-fork-source-rollback:empty-middle";
    let empty_leaf = "thread:projection-fork-source-rollback:empty-leaf";
    apply_all(
        &store,
        vec![
            event(
                "thread.created",
                empty_middle,
                T0,
                thread_with(
                    empty_middle,
                    json!({
                        "lineage": {"parentThreadId": source, "relationshipToParent": "fork", "rootThreadId": source},
                        "forkedFrom": {"type": "run", "threadId": source, "runId": run2}
                    }),
                ),
            ),
            event(
                "run.updated",
                empty_middle,
                T0,
                run_with(
                    empty_middle,
                    empty_middle_run,
                    1,
                    "completed",
                    "node:projection-fork-source-rollback:empty-middle",
                    "message:projection-fork-source-rollback:empty-middle",
                    None,
                ),
            ),
            event(
                "thread.created",
                empty_leaf,
                T0,
                thread_with(
                    empty_leaf,
                    json!({
                        "lineage": {"parentThreadId": empty_middle, "relationshipToParent": "fork", "rootThreadId": source},
                        "forkedFrom": {"type": "run", "threadId": empty_middle, "runId": empty_middle_run}
                    }),
                ),
            ),
        ],
    )
    .await;
    let empty_middle_snapshot = store
        .get_thread_snapshot_window(
            &tid(empty_leaf),
            ThreadSnapshotWindowOptions {
                row_limit: 2,
                ..Default::default()
            },
        )
        .await
        .expect("empty middle snapshot");
    assert_eq!(
        empty_middle_snapshot
            .projection
            .visible_turn_items
            .iter()
            .map(|row| {
                format!(
                    "{}|{}",
                    row.source_thread_id.as_str(),
                    wire(&row.item)["type"].as_str().expect("type")
                )
            })
            .collect::<Vec<_>>(),
        [
            format!("{source}|user_message"),
            format!("{source}|assistant_message"),
            format!("{source}|fork"),
            format!("{empty_middle}|fork"),
        ]
    );

    // Upstream 5503-5560: drop run-scoped rows, add a legacy import row that
    // has no run, and mark the source as imported.
    let imported_item = "turn-item:projection-fork-source-rollback:legacy-import";
    seed(&store, move |conn| {
        conn.execute(
            "DELETE FROM orchestration_v2_projection_turn_items WHERE run_id IN (?1, ?2)",
            params![run1, run2],
        )
        .expect("delete run rows");
        let payload = json!({
            "id": imported_item, "threadId": source, "runId": null, "nodeId": null,
            "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
            "parentItemId": null, "ordinal": 50, "status": "completed", "title": null,
            "startedAt": T0, "completedAt": T0, "updatedAt": T0, "type": "assistant_message",
            "messageId": "message:projection-fork-source-rollback:legacy-import",
            "text": "legacy import before empty fork", "streaming": false,
            "historyOrigin": "v1_import"
        });
        insert_item_row(
            conn,
            imported_item,
            source,
            None,
            50,
            "assistant_message",
            &payload,
        );
        conn.execute(
            "UPDATE orchestration_v2_projection_threads
             SET payload_json = json_set(payload_json, '$.historyOrigin', 'v1_import')
             WHERE thread_id = ?1",
            params![source],
        )
        .expect("mark imported");
        conn.execute(
            "UPDATE orchestration_v2_projection_threads
             SET payload_json = json_set(payload_json, '$.forkedFrom.runId', ?1)
             WHERE thread_id = ?2",
            params![empty_boundary_run, target],
        )
        .expect("repoint fork to empty boundary");
    })
    .await;
    let imported_empty_boundary = store
        .get_thread_snapshot_window(
            &tid(target),
            ThreadSnapshotWindowOptions {
                row_limit: 2,
                ..Default::default()
            },
        )
        .await
        .expect("imported empty boundary");
    assert_eq!(
        imported_empty_boundary
            .projection
            .visible_turn_items
            .iter()
            .map(|row| {
                let item = wire(&row.item);
                let label = if item["type"] == "assistant_message" {
                    item["text"].as_str().expect("text").to_owned()
                } else {
                    item["type"].as_str().expect("type").to_owned()
                };
                format!(
                    "{}|{}|{label}",
                    wire(&row.visibility).as_str().expect("visibility"),
                    row.source_thread_id.as_str()
                )
            })
            .collect::<Vec<_>>(),
        [
            format!("inherited|{source}|legacy import before empty fork"),
            format!("synthetic|{source}|fork"),
            format!("local|{target}|command_execution"),
            format!("local|{target}|command_execution"),
        ]
    );
}

/// Upstream line 5663. The `projectThreadAwarenessV2` phase assertions are
/// omitted: the agent-awareness projection has no Turin port.
#[ignore = "projectThreadAwarenessV2 (shared agentAwareness) has no Turin port; the phase assertions are omitted"]
#[tokio::test]
async fn a_pull_request_watch_keeps_a_finished_thread_working_until_it_ends() {
    let store = memory_store().await;
    let thread_id = "thread:watched-pull-request";
    let run_id = "run:watched-pull-request";
    let at = "2026-10-05T12:00:00.000Z";
    let thread_json = thread_with(
        thread_id,
        json!({
            "title": "Babysit the PR",
            "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": thread_id},
            "createdAt": at, "updatedAt": at
        }),
    );
    let run_json = patch(
        run(thread_id, run_id, 1, "completed"),
        json!({
            "rootNodeId": null, "userMessageId": "message:watched-pull-request",
            "requestedAt": at, "startedAt": at, "completedAt": at
        }),
    );
    apply_all(
        &store,
        vec![
            event("thread.created", thread_id, at, thread_json.clone()),
            event("run.created", thread_id, at, run_json),
        ],
    )
    .await;

    let link = json!({
        "host": "github.com", "repository": "pingdotgg/t3code", "number": 7,
        "url": "https://github.com/pingdotgg/t3code/pull/7", "source": "agent",
        "linkedAt": at, "snapshot": null, "stack": null
    });
    let watch = json!({
        "startedAt": at, "headSha": null, "failedChecks": [], "passed": false,
        "passedChecks": [], "remarksThrough": at, "remarkIds": [], "conflicting": false,
        "wakes": 0
    });
    let sync = |pull_requests: Value| {
        event(
            "thread.pull-request-synced",
            thread_id,
            at,
            patch(
                thread_json.clone(),
                json!({ "pullRequests": pull_requests }),
            ),
        )
    };

    apply_all(
        &store,
        vec![sync(json!([patch(
            link.clone(),
            json!({ "watch": watch })
        )]))],
    )
    .await;
    let shell = store
        .get_thread_shell(&tid(thread_id))
        .await
        .expect("shell")
        .expect("thread shell");
    let listed = store
        .get_shell_snapshot(ShellSnapshotOptions::default())
        .await
        .expect("shell snapshot")
        .threads
        .into_iter()
        .find(|candidate| candidate.id.as_str() == thread_id)
        .expect("listed shell");
    assert_eq!(
        wire(&listed.pending_background_tasks),
        wire(&shell.pending_background_tasks)
    );
    assert_eq!(
        wire(&shell.pending_background_tasks),
        json!([{
            "taskId": "pull-request-watch:github.com/pingdotgg/t3code#7",
            "description": "Watching pull request #7",
            "kind": "monitor"
        }])
    );

    apply_all(&store, vec![sync(json!([link]))]).await;
    let shell = store
        .get_thread_shell(&tid(thread_id))
        .await
        .expect("shell")
        .expect("thread shell");
    assert_eq!(wire(&shell.pending_background_tasks), json!([]));
}

/// Upstream line 5785. The `projectThreadAwarenessV2` phase and `updatedAt`
/// assertions are omitted: the agent-awareness projection has no Turin port.
#[ignore = "projectThreadAwarenessV2 (shared agentAwareness) has no Turin port; the phase and updatedAt assertions are omitted"]
#[tokio::test]
async fn dates_a_completion_released_by_held_background_work_at_the_release() {
    let store = memory_store().await;
    let thread_id = "thread:held-completion";
    let run_id = "run:held-completion";
    let provider_thread = "provider-thread:held-completion";
    let completed_at = "2026-09-28T12:00:00.000Z";
    let released_at = "2026-09-28T12:05:00.000Z";
    let provider_thread_json = json!({
        "id": provider_thread, "driver": "codex", "providerInstanceId": "codex",
        "providerSessionId": null, "appThreadId": thread_id, "ownerNodeId": null,
        "nativeThreadRef": null, "nativeConversationHeadRef": null, "status": "idle",
        "firstRunOrdinal": 1, "lastRunOrdinal": 1, "handoffIds": [], "forkedFrom": null,
        "pendingBackgroundTasks": [{"taskId": "watch", "kind": "monitor"}],
        "createdAt": completed_at, "updatedAt": completed_at
    });
    apply_all(
        &store,
        vec![
            event(
                "thread.created",
                thread_id,
                completed_at,
                thread_with(
                    thread_id,
                    json!({
                        "title": "Watch the build",
                        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": thread_id},
                        "createdAt": completed_at, "updatedAt": completed_at
                    }),
                ),
            ),
            event(
                "run.created",
                thread_id,
                completed_at,
                patch(
                    run(thread_id, run_id, 1, "completed"),
                    json!({
                        "rootNodeId": null, "providerThreadId": provider_thread,
                        "userMessageId": "message:held-completion", "activeAttemptId": null,
                        "requestedAt": completed_at, "startedAt": completed_at,
                        "completedAt": completed_at
                    }),
                ),
            ),
            event(
                "provider-thread.updated",
                thread_id,
                completed_at,
                provider_thread_json.clone(),
            ),
        ],
    )
    .await;

    // The monitor ends five minutes after the run.
    apply_all(
        &store,
        vec![event(
            "provider-thread.updated",
            thread_id,
            released_at,
            patch(
                provider_thread_json.clone(),
                json!({ "pendingBackgroundTasks": [], "updatedAt": released_at }),
            ),
        )],
    )
    .await;

    // A later provider must not hide this owner's roster from restart recovery.
    apply_all(
        &store,
        vec![event(
            "provider-thread.updated",
            thread_id,
            released_at,
            patch(
                provider_thread_json.clone(),
                json!({ "updatedAt": released_at }),
            ),
        )],
    )
    .await;
    let newer_run = "run:held-completion:new-provider";
    let first_run = store
        .get_thread_projection(&tid(thread_id))
        .await
        .expect("projection")
        .runs
        .into_iter()
        .next()
        .expect("first run");
    apply_all(
        &store,
        vec![event(
            "run.created",
            thread_id,
            released_at,
            patch(
                wire(&first_run),
                json!({
                    "id": newer_run, "ordinal": 2,
                    "providerThreadId": "provider-thread:held-completion:new-provider",
                    "requestedAt": released_at, "startedAt": released_at,
                    "completedAt": released_at
                }),
            ),
        )],
    )
    .await;
    let recovery_run_ids: Vec<Value> = store
        .get_runtime_recovery_projection(&tid(thread_id))
        .await
        .expect("recovery")
        .runs
        .iter()
        .map(|run| wire(&run.id))
        .collect();
    assert_eq!(recovery_run_ids, vec![json!(run_id), json!(newer_run)]);
}
