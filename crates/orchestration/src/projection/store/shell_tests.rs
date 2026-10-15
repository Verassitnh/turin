//! Ports of the shell tests in ProjectionStore.test.ts.

use serde_json::json;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ProviderFailureClass, OrchestrationV2ShellThreadStatus,
};

use crate::projection::store::ProjectionStore;
use crate::projection::store::shell::{ShellSnapshotLocation, ShellSnapshotOptions};
use crate::projection::test_support::*;

async fn shell(
    store: &ProjectionStore,
    id: &str,
) -> turin_contracts::orchestration_v2::OrchestrationV2ThreadShell {
    store
        .get_shell_snapshot(ShellSnapshotOptions::default())
        .await
        .unwrap()
        .threads
        .into_iter()
        .find(|thread| thread.id.as_str() == id)
        .unwrap_or_else(|| panic!("shell for {id}"))
}

fn provider_thread(
    suffix: &str,
    instance: &str,
    owner: Option<&str>,
    seconds: u32,
    goal: serde_json::Value,
) -> serde_json::Value {
    json!({
        "id": format!("provider-thread:{suffix}"), "driver": if instance == "claude" { "claudeAgent" } else { "codex" },
        "providerInstanceId": instance, "providerSessionId": null, "appThreadId": "thread:a",
        "ownerNodeId": owner, "nativeThreadRef": null, "nativeConversationHeadRef": null,
        "status": "idle", "firstRunOrdinal": null, "lastRunOrdinal": null, "handoffIds": [],
        "forkedFrom": null, "goal": goal, "createdAt": at(seconds), "updatedAt": at(seconds)
    })
}

#[tokio::test]
async fn projects_root_provider_owners_into_the_shell_in_first_use_order() {
    let store = memory_store().await;
    let mut events = vec![event("thread.created", "thread:a", T0, thread("thread:a"))];
    // Root Codex, then a Claude subagent it delegated to (owned by a node, so
    // not a handoff), then the handoff target, then Codex again: no duplicate.
    for (suffix, instance, owner, seconds) in [
        ("codex", "codex", None, 0),
        (
            "claude-subagent",
            "claude",
            Some("node:provider-history"),
            1,
        ),
        ("claude", "claude", None, 2),
        ("codex-again", "codex", None, 3),
    ] {
        events.push(event(
            "provider-thread.updated",
            "thread:a",
            &at(seconds),
            provider_thread(suffix, instance, owner, seconds, json!(null)),
        ));
    }
    apply_all(&store, events).await;
    let row = shell(&store, "thread:a").await;
    let history: Vec<_> = row
        .provider_instance_history
        .iter()
        .map(|id| id.as_str())
        .collect();
    assert_eq!(history, ["codex", "claude"]);
}

#[tokio::test]
async fn shows_the_native_goal_of_the_active_provider_thread_on_the_shell() {
    let store = memory_store().await;
    apply_all(
        &store,
        vec![event("thread.created", "thread:a", T0, thread("thread:a"))],
    )
    .await;
    let goal = json!({"objective": "Ship it", "status": "active", "tokensUsed": 10});
    apply_all(
        &store,
        vec![event("provider-thread.updated", "thread:a", &at(0), {
            let mut value = provider_thread("first", "codex", None, 0, goal.clone());
            value["firstRunOrdinal"] = json!(1);
            value["status"] = json!("active");
            value
        })],
    )
    .await;
    assert_eq!(
        serde_json::to_value(shell(&store, "thread:a").await.goal).unwrap(),
        json!(goal)
    );
    // A handoff moves the conversation; the previous provider's goal stays behind.
    apply_all(
        &store,
        vec![event("provider-thread.updated", "thread:a", &at(1), {
            let mut value = provider_thread("second", "codex", None, 1, json!(null));
            value["firstRunOrdinal"] = json!(2);
            value["status"] = json!("active");
            value
        })],
    )
    .await;
    assert_eq!(shell(&store, "thread:a").await.goal, Some(None));
}

#[tokio::test]
async fn dates_thread_activity_by_a_run_event_and_shows_it_on_the_shell() {
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
    let row = shell(&store, "thread:a").await;
    assert_eq!(row.updated_at.to_string(), at(5));
    assert_eq!(row.status, OrchestrationV2ShellThreadStatus::Running);
    assert_eq!(
        row.active_run_id.as_ref().map(|id| id.as_str()),
        Some("run:a")
    );
}

#[tokio::test]
async fn only_exposes_interruptible_runs_through_the_shell_active_run_id() {
    let store = memory_store().await;
    apply_all(
        &store,
        vec![
            event("thread.created", "thread:a", T0, thread("thread:a")),
            event(
                "run.created",
                "thread:a",
                T0,
                run("thread:a", "run:a", 1, "waiting"),
            ),
        ],
    )
    .await;
    let row = shell(&store, "thread:a").await;
    assert_eq!(
        row.active_run_id, None,
        "a waiting run cannot be interrupted"
    );
    assert_eq!(row.status, OrchestrationV2ShellThreadStatus::Waiting);
    assert_eq!(
        serde_json::to_value(row.activity_run_status).unwrap(),
        json!("waiting")
    );
}

#[tokio::test]
async fn reads_only_the_requested_projects_threads_into_a_scoped_shell_snapshot() {
    let store = memory_store().await;
    let mut other = thread("thread:other");
    other["projectId"] = json!("project:other");
    let mut archived = thread("thread:archived");
    archived["archivedAt"] = json!(T0);
    apply_all(
        &store,
        vec![
            event("thread.created", "thread:a", T0, thread("thread:a")),
            event("thread.created", "thread:other", T0, other),
            event("thread.created", "thread:archived", T0, archived),
        ],
    )
    .await;
    // Corrupt the other project's row: a scoped read that succeeds never read it.
    seed(&store, |conn| {
        conn.execute(
            "UPDATE orchestration_v2_projection_threads SET payload_json = '{}' WHERE thread_id = 'thread:other'",
            [],
        )
        .unwrap();
    })
    .await;
    assert!(
        store
            .get_shell_snapshot(ShellSnapshotOptions::default())
            .await
            .is_err()
    );
    let scoped = store
        .get_shell_snapshot(ShellSnapshotOptions {
            project_id: Some(turin_contracts::ProjectId::new("project:test").unwrap()),
            location: Some(ShellSnapshotLocation::Active),
            ..Default::default()
        })
        .await
        .unwrap();
    let ids: Vec<_> = scoped
        .threads
        .iter()
        .map(|thread| thread.id.as_str())
        .collect();
    assert_eq!(ids, ["thread:a"]);
    assert!(scoped.archived_threads.is_empty());
}

#[tokio::test]
async fn reads_the_shell_rows_first_and_decodes_them_in_a_separate_step() {
    let store = memory_store().await;
    apply_all(
        &store,
        vec![event(
            "thread.created",
            "thread:before",
            T0,
            thread("thread:before"),
        )],
    )
    .await;
    let rows = store
        .read_shell_snapshot(ShellSnapshotOptions::default())
        .await
        .unwrap();
    // Commits between the read and the decode, as another request can.
    apply_all(
        &store,
        vec![event(
            "thread.created",
            "thread:after",
            T0,
            thread("thread:after"),
        )],
    )
    .await;
    let ids = |snapshot: &turin_contracts::orchestration_v2::OrchestrationV2ThreadShellSnapshot| {
        snapshot
            .threads
            .iter()
            .map(|thread| thread.id.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&rows.decode().unwrap()), ["thread:before"]);
    let fresh = store
        .get_shell_snapshot(ShellSnapshotOptions::default())
        .await
        .unwrap();
    assert_eq!(ids(&fresh).len(), 2);
}

#[tokio::test]
async fn shell_projects_only_the_latest_failed_root_turns_usage_limit() {
    let store = memory_store().await;
    let mut failed = run("thread:a", "run:a", 1, "failed");
    failed["completedAt"] = json!(at(3));
    let error = turn_item(
        "thread:a",
        "item:error",
        Some("run:a"),
        1,
        "error",
        "failed",
        json!({"failure": {"class": "usage_limit", "message": "limit reached", "code": null,
            "retryable": null, "resetAt": "2026-01-02T00:00:00.000Z"}}),
    );
    apply_all(
        &store,
        vec![
            event("thread.created", "thread:a", T0, thread("thread:a")),
            event("run.created", "thread:a", T0, failed),
            event("turn-item.updated", "thread:a", T0, error),
        ],
    )
    .await;
    let row = shell(&store, "thread:a").await;
    assert_eq!(row.status, OrchestrationV2ShellThreadStatus::Failed);
    assert_eq!(row.last_error, Some(Some("limit reached".to_owned())));
    assert_eq!(
        row.last_error_class,
        Some(Some(OrchestrationV2ProviderFailureClass::UsageLimit))
    );
    assert_eq!(
        row.usage_limit_reset_at,
        Some(Some("2026-01-02T00:00:00.000Z".to_owned()))
    );
    // The in-memory twin agrees.
    let projection = store.get_thread_projection(&tid("thread:a")).await.unwrap();
    let memory = crate::projection::store::shell::thread_shell_from_projection(&projection);
    assert_eq!(memory.last_error, row.last_error);
    assert_eq!(memory.last_error_class, row.last_error_class);
    assert_eq!(memory.usage_limit_reset_at, row.usage_limit_reset_at);
    assert_eq!(memory.status, row.status);
}

#[tokio::test]
async fn background_work_keeps_a_finished_thread_waiting_and_rolled_back_work_does_not() {
    let store = memory_store().await;
    let mut done = run("thread:a", "run:a", 1, "completed");
    done["completedAt"] = json!(at(2));
    let mut rolled = run("thread:a", "run:b", 2, "rolled_back");
    rolled["completedAt"] = json!(at(3));
    let command = |id: &str, run_id: &str, ordinal: u64| {
        turn_item(
            "thread:a",
            id,
            Some(run_id),
            ordinal,
            "command_execution",
            "running",
            json!({"input": "sleep 60", "nativeItemRef": null}),
        )
    };
    apply_all(
        &store,
        vec![
            event("thread.created", "thread:a", T0, thread("thread:a")),
            event("run.created", "thread:a", T0, done),
            event(
                "turn-item.updated",
                "thread:a",
                T0,
                command("item:live", "run:a", 1),
            ),
            event(
                "turn-item.updated",
                "thread:a",
                T0,
                command("item:abandoned", "run:b", 2),
            ),
        ],
    )
    .await;
    // Run b is the newest, so it presents as latest and is rolled back after the fact.
    apply_all(
        &store,
        vec![event("run.created", "thread:a", &at(3), rolled)],
    )
    .await;
    let row = shell(&store, "thread:a").await;
    let tasks: Vec<_> = row
        .pending_background_tasks
        .iter()
        .map(|task| task.task_id().to_string())
        .collect();
    // Latest run is rolled back: the settled-run gate closes entirely.
    assert!(tasks.is_empty(), "{tasks:?}");
    let memory = crate::projection::store::shell::thread_shell_from_projection(
        &store.get_thread_projection(&tid("thread:a")).await.unwrap(),
    );
    assert_eq!(
        memory.pending_background_tasks,
        row.pending_background_tasks
    );
}

#[tokio::test]
async fn forked_shells_count_the_source_history_through_the_fork_point() {
    let store = memory_store().await;
    let mut fork = thread("thread:fork");
    fork["forkedFrom"] = json!({"type": "run", "threadId": "thread:a", "runId": "run:a2"});
    let mut events = vec![
        event("thread.created", "thread:a", T0, thread("thread:a")),
        event(
            "run.created",
            "thread:a",
            T0,
            run("thread:a", "run:a1", 1, "completed"),
        ),
        event(
            "run.created",
            "thread:a",
            T0,
            run("thread:a", "run:a2", 2, "completed"),
        ),
        event(
            "run.created",
            "thread:a",
            T0,
            run("thread:a", "run:a3", 3, "completed"),
        ),
        event("thread.created", "thread:fork", T0, fork),
        event(
            "run.created",
            "thread:fork",
            T0,
            run("thread:fork", "run:f1", 1, "completed"),
        ),
    ];
    for (id, run_id, ordinal) in [
        ("a1", "run:a1", 1),
        ("a2", "run:a2", 2),
        ("a3", "run:a3", 3),
    ] {
        events.push(event(
            "turn-item.updated",
            "thread:a",
            T0,
            assistant_item("thread:a", &format!("item:{id}"), run_id, ordinal, "x"),
        ));
    }
    events.push(event(
        "turn-item.updated",
        "thread:fork",
        T0,
        assistant_item("thread:fork", "item:f1", "run:f1", 1, "y"),
    ));
    apply_all(&store, events).await;
    let row = shell(&store, "thread:fork").await;
    // Two inherited source items, the fork marker, and the fork's own item.
    assert_eq!(row.item_count, 1);
    assert_eq!(row.visible_item_count, 2 + 1 + 1);
    let full = store
        .get_thread_projection(&tid("thread:fork"))
        .await
        .unwrap();
    assert_eq!(full.visible_turn_items.len() as u64, row.visible_item_count);
    assert_eq!(
        shell(&store, "thread:a").await.visible_item_count,
        3,
        "an unforked thread shows only its own items"
    );
    assert_eq!(
        store
            .get_thread_shell(&tid("thread:fork"))
            .await
            .unwrap()
            .unwrap()
            .visible_item_count,
        row.visible_item_count
    );
    assert!(
        store
            .get_thread_shell(&tid("thread:missing"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn limit_recovery_candidates_select_failed_usage_limit_runs_with_a_future_reset() {
    let store = memory_store().await;
    let mut failed = run("thread:a", "run:a", 1, "failed");
    failed["completedAt"] = json!(at(3));
    let error = turn_item(
        "thread:a",
        "item:error",
        Some("run:a"),
        1,
        "error",
        "failed",
        json!({"failure": {"class": "usage_limit", "message": "limit reached", "code": null,
            "retryable": null, "resetAt": "2026-01-02T00:00:00.000Z"}}),
    );
    apply_all(
        &store,
        vec![
            event("thread.created", "thread:a", T0, thread("thread:a")),
            event("run.created", "thread:a", T0, failed),
            event("turn-item.updated", "thread:a", T0, error),
        ],
    )
    .await;
    let now: turin_contracts::UtcDateTime = "2026-01-01T12:00:00.000Z".parse().unwrap();
    let candidates = store
        .get_limit_recovery_candidates(now, true, true)
        .await
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id.as_str(), "thread:a");
    assert_eq!(
        candidates[0].usage_limit_reset_at.as_deref(),
        Some("2026-01-02T00:00:00.000Z")
    );
    assert!(
        store
            .get_limit_recovery_candidates(now, false, false)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn settlement_candidates_skip_pinned_archived_and_busy_threads() {
    let store = memory_store().await;
    let mut pinned = thread("thread:pinned");
    pinned["pinnedAt"] = json!(T0);
    let mut archived = thread("thread:archived");
    archived["archivedAt"] = json!(T0);
    apply_all(
        &store,
        vec![
            event("thread.created", "thread:idle", T0, thread("thread:idle")),
            event("thread.created", "thread:pinned", T0, pinned),
            event("thread.created", "thread:archived", T0, archived),
            event("thread.created", "thread:busy", T0, thread("thread:busy")),
            event(
                "run.created",
                "thread:busy",
                T0,
                run("thread:busy", "run:busy", 1, "running"),
            ),
        ],
    )
    .await;
    let ids: Vec<_> = store
        .get_settlement_candidates(None)
        .await
        .unwrap()
        .into_iter()
        .map(|candidate| candidate.id.to_string())
        .collect();
    assert_eq!(ids, ["thread:idle"]);
}

#[tokio::test]
async fn threads_with_pull_requests_lists_only_active_linked_threads() {
    let store = memory_store().await;
    let link = json!({"host": "github.com", "repository": "o/r", "number": 1,
        "url": "https://github.com/o/r/pull/1", "source": "manual", "linkedAt": T0,
        "snapshot": null, "stack": null});
    let mut linked = thread("thread:linked");
    linked["pullRequests"] = json!([link]);
    let mut empty = thread("thread:empty");
    empty["pullRequests"] = json!([]);
    apply_all(
        &store,
        vec![
            event("thread.created", "thread:linked", T0, linked),
            event("thread.created", "thread:empty", T0, empty),
            event("thread.created", "thread:none", T0, thread("thread:none")),
        ],
    )
    .await;
    let ids: Vec<_> = store
        .get_threads_with_pull_requests(None)
        .await
        .unwrap()
        .into_iter()
        .map(|thread| thread.id.to_string())
        .collect();
    assert_eq!(ids, ["thread:linked"]);
}

#[tokio::test]
async fn worktree_path_lookup_reads_only_active_threads() {
    let store = memory_store().await;
    let with_path = |id: &str, path: &str| {
        let mut value = thread(id);
        value["worktreePath"] = json!(path);
        value["branch"] = json!(id);
        value
    };
    let mut archived = with_path("thread:archived", "/worktrees/archived");
    archived["archivedAt"] = json!(T0);
    let deleted = with_path("thread:deleted", "/worktrees/deleted");
    let mut deleted_after = deleted.clone();
    deleted_after["deletedAt"] = json!(T0);
    apply_all(
        &store,
        vec![
            event(
                "thread.created",
                "thread:active",
                T0,
                with_path("thread:active", "/worktrees/active"),
            ),
            event("thread.created", "thread:archived", T0, archived),
            event("thread.created", "thread:local", T0, thread("thread:local")),
            event("thread.created", "thread:deleted", T0, deleted),
            event("thread.deleted", "thread:deleted", T0, deleted_after),
        ],
    )
    .await;
    let paths = store.get_thread_worktree_paths().await.unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].thread_id.as_str(), "thread:active");
    assert_eq!(paths[0].worktree_path, "/worktrees/active");
}
