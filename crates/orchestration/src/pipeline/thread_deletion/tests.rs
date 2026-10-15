//! Ports of upstream `ThreadDeletion.test.ts`. Upstream reduces the events
//! over the projection (`applyToProjection`, T13a); these tests inspect the
//! emitted events directly, which pins the same facts.

use super::*;
use crate::pipeline::test_support::{projection, run_json};
use serde_json::json;
use turin_contracts::orchestration_v2::OrchestrationV2ThreadProjection;

const THREAD: &str = "thread:delete-plan";
const STATUSES: [&str; 6] = ["preparing", "queued", "starting", "running", "waiting", "completed"];

fn make_projection(extra: Value) -> OrchestrationV2ThreadProjection {
    let runs: Vec<Value> = STATUSES
        .iter()
        .enumerate()
        .map(|(index, status)| {
            let mut run = run_json(&format!("run:delete-plan:{status}"), THREAD, index as u64 + 1, status);
            run["providerThreadId"] = json!("provider-thread:delete-plan");
            run["rootNodeId"] = json!(format!("node:delete-plan:{status}"));
            run["activeAttemptId"] = json!(format!("attempt:delete-plan:{status}"));
            run["queuePosition"] = if *status == "queued" { json!(1) } else { Value::Null };
            run["startedAt"] = json!("2026-09-01T00:00:00.000Z");
            run["completedAt"] = if *status == "completed" { json!("2026-09-01T00:00:00.000Z") } else { Value::Null };
            run
        })
        .collect();
    let attempts: Vec<Value> = STATUSES
        .iter()
        .map(|status| {
            json!({
                "id": format!("attempt:delete-plan:{status}"), "runId": format!("run:delete-plan:{status}"),
                "attemptOrdinal": 1, "rootNodeId": format!("node:delete-plan:{status}"),
                "providerInstanceId": "codex", "providerThreadId": "provider-thread:delete-plan",
                "providerTurnId": null, "reason": "initial",
                "status": if *status == "completed" { "completed" } else { "running" },
                "startedAt": "2026-09-01T00:00:00.000Z", "completedAt": null,
            })
        })
        .collect();
    let nodes: Vec<Value> = STATUSES
        .iter()
        .map(|status| {
            json!({
                "id": format!("node:delete-plan:{status}"), "threadId": THREAD,
                "runId": format!("run:delete-plan:{status}"), "parentNodeId": null,
                "rootNodeId": format!("node:delete-plan:{status}"), "kind": "root_turn",
                "status": if *status == "completed" { "completed" } else { "waiting" },
                "countsForRun": true, "providerThreadId": "provider-thread:delete-plan",
                "providerTurnId": null, "nativeItemRef": null, "runtimeRequestId": null,
                "checkpointScopeId": null, "startedAt": "2026-09-01T00:00:00.000Z", "completedAt": null,
            })
        })
        .collect();
    let requests: Vec<Value> = ["pending", "resolved"]
        .iter()
        .map(|status| {
            json!({
                "id": format!("request:delete-plan:{status}"), "nodeId": "node:delete-plan:running",
                "providerTurnId": null, "nativeRequestRef": null, "kind": "user_input",
                "status": status, "responseCapability": {"type": "message"},
                "createdAt": "2026-09-01T00:00:00.000Z",
                "resolvedAt": if *status == "resolved" { json!("2026-09-01T00:00:00.000Z") } else { Value::Null },
            })
        })
        .collect();
    let mut fields = json!({
        "runs": runs, "attempts": attempts, "nodes": nodes, "runtimeRequests": requests,
    });
    for (key, value) in extra.as_object().cloned().unwrap_or_default() {
        fields[key] = value;
    }
    let mut records = projection(THREAD, "project:delete-plan", fields);
    records.thread.branch = Some(turin_contracts::TrimmedNonEmptyString::new("feature").unwrap());
    records.thread.worktree_path = Some(turin_contracts::TrimmedNonEmptyString::new("/workspace/feature").unwrap());
    records
}

fn plan(projection: &OrchestrationV2ThreadProjection, attachments: &[&str]) -> ThreadDeletionPlan {
    let ids: Vec<String> = attachments.iter().map(|id| (*id).to_owned()).collect();
    plan_thread_deletion(
        &CommandId::new("command:delete-plan").unwrap(),
        &ThreadId::new(THREAD).unwrap(),
        projection,
        &ids,
        "2026-09-04T00:00:00.000Z".parse().unwrap(),
        &IdAllocator,
    )
    .unwrap()
}

fn events_of(plan: &ThreadDeletionPlan, kind: &str) -> Vec<Value> {
    plan.events
        .iter()
        .map(|event| serde_json::to_value(event).unwrap())
        .filter(|event| event["type"] == kind)
        .collect()
}

#[test]
fn cancels_active_work_without_reviving_a_run_while_disposing_delegated_completion() {
    let base = make_projection(json!({}));
    let mut run_values = serde_json::to_value(&base.runs).unwrap();
    for run in run_values.as_array_mut().unwrap() {
        if run["id"] == "run:delete-plan:running" {
            run["delegatedCompletion"] = json!({
                "disposition": "open", "nextGeneration": 3,
                "delivery": {"generation": 2, "messageId": "message:delete-plan:queued", "taskIds": ["task:delete-plan"]},
            });
        }
    }
    let task = json!({
        "id": "task:delete-plan", "threadId": THREAD, "runId": "run:delete-plan:running",
        "parentNodeId": "node:delete-plan:running", "origin": "app_owned", "createdBy": "agent",
        "driver": "codex", "providerInstanceId": "codex", "providerThreadId": null,
        "childThreadId": null, "nativeTaskRef": null, "prompt": "Inspect the project",
        "title": null, "model": null,
        "completionDelivery": {"state": "claimed", "observedByRunId": null},
        "status": "completed", "result": "done", "startedAt": "2026-09-01T00:00:00.000Z",
        "completedAt": "2026-09-01T00:00:00.000Z", "updatedAt": "2026-09-01T00:00:00.000Z",
    });
    let projection = make_projection(json!({ "runs": run_values, "subagents": [task] }));
    let plan = plan(&projection, &[]);

    let deleted = &events_of(&plan, "thread.deleted")[0]["payload"];
    assert_eq!(deleted["deletedAt"], "2026-09-04T00:00:00.000Z");
    assert_eq!(deleted["worktreePath"], "/workspace/feature");
    assert!(projection.thread.deleted_at.is_none());

    // Every active run is cancelled once; the completed one is left alone.
    let run_updates = events_of(&plan, "run.updated");
    let cancelled: Vec<_> = run_updates
        .iter()
        .filter(|event| event["payload"]["status"] == "cancelled")
        .collect();
    assert_eq!(cancelled.len(), 5);
    for event in &cancelled {
        assert_eq!(event["payload"]["completedAt"], "2026-09-04T00:00:00.000Z");
        assert!(event["payload"]["queuePosition"].is_null());
    }
    assert!(
        !run_updates
            .iter()
            .any(|event| event["payload"]["id"] == "run:delete-plan:completed")
    );
    assert_eq!(
        run_updates
            .iter()
            .filter(|e| e["payload"]["id"] == "run:delete-plan:queued")
            .count(),
        1
    );
    for attempt in events_of(&plan, "run-attempt.updated") {
        assert_eq!(attempt["payload"]["status"], "cancelled");
        assert!(!attempt["payload"]["runId"].as_str().unwrap().ends_with(":completed"));
    }
    assert_eq!(events_of(&plan, "run-attempt.updated").len(), 5);
    assert_eq!(events_of(&plan, "node.updated").len(), 5);

    let requests = events_of(&plan, "runtime-request.updated");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["payload"]["id"], "request:delete-plan:pending");
    assert_eq!(requests[0]["payload"]["status"], "cancelled");
    assert_eq!(
        requests[0]["payload"]["responseCapability"],
        json!({"type": "not_resumable", "reason": "The thread was deleted."})
    );

    // The cohort update is built on the cancelled run, so the run stays
    // cancelled while its delegated completion is disposed.
    let cohort = run_updates
        .iter()
        .rev()
        .find(|event| event["payload"]["id"] == "run:delete-plan:running")
        .unwrap();
    assert_eq!(cohort["payload"]["status"], "cancelled");
    assert_eq!(
        cohort["payload"]["delegatedCompletion"],
        json!({"disposition": "disposed", "nextGeneration": 3, "delivery": null})
    );
    let tasks = events_of(&plan, "subagent.updated");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["payload"]["completionDelivery"]["state"], "disposed");
    assert_eq!(tasks[0]["nodeId"], "task:delete-plan");
}

#[test]
fn queues_provider_and_resource_cleanup_and_preserves_an_earlier_deletion_timestamp() {
    let sessions: Vec<Value> = ["running", "stopped", "error"]
        .iter()
        .map(|status| {
            json!({
                "id": format!("session:delete-plan:{status}"), "driver": "codex",
                "providerInstanceId": "codex", "status": status, "cwd": "/workspace/feature",
                "model": null, "capabilities": codex_capabilities(),
                "createdAt": "2026-09-01T00:00:00.000Z", "updatedAt": "2026-09-01T00:00:00.000Z",
                "lastError": null,
            })
        })
        .collect();
    let mut projection = make_projection(json!({ "providerSessions": sessions }));
    projection.thread.deleted_at = Some("2026-09-01T00:00:00.000Z".parse().unwrap());
    let plan = plan(&projection, &["shared_file", "shared_file"]);
    assert_eq!(
        events_of(&plan, "thread.deleted")[0]["payload"]["deletedAt"],
        "2026-09-01T00:00:00.000Z"
    );
    let detached = events_of(&plan, "provider-session.detached");
    assert_eq!(detached.len(), 1);
    assert_eq!(detached[0]["payload"]["providerSessionId"], "session:delete-plan:running");
    let requests: Vec<Value> = plan
        .effects
        .iter()
        .map(|effect| serde_json::to_value(&effect.request).unwrap())
        .collect();
    assert_eq!(
        requests,
        [
            json!({"type": "provider-session.detach", "providerSessionId": "session:delete-plan:running",
                   "detail": "Thread deleted.", "revokeMcpCredential": true}),
            json!({"type": "terminal.cleanup"}),
            json!({"type": "preview.cleanup"}),
            json!({"type": "attachment.cleanup", "attachmentIds": ["shared_file"]}),
        ]
    );
    assert_eq!(
        plan.effects[0].id,
        "effect:command:delete-plan:provider-session.detach:session:delete-plan:running"
    );
}

#[test]
fn no_attachment_cleanup_is_queued_without_attachments() {
    let plan = plan(&make_projection(json!({})), &[]);
    assert_eq!(plan.effects.len(), 2);
}

/// The Codex capability set, minimal but decodable.
fn codex_capabilities() -> Value {
    serde_json::to_value(crate::pipeline::test_support::default_capabilities()).unwrap()
}
