//! Integration tests for the second half of `orchestration_v2.rs`
//! (upstream orchestrationV2.ts lines 1950-3858): thread shells, shell and
//! thread streams, commands, subscription payloads, errors, thread search and
//! provider replay transcripts. They pin wire shape through the public API.

use pretty_assertions::assert_eq;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use turin_contracts::base_schemas::{PositiveInt, assert_known_tags, has_unknown_union_tag};
use turin_contracts::orchestration_v2::*;

const NOW: &str = "2026-04-20T00:00:00.000Z";

/// Decodes `value`, re-encodes it, and asserts the wire form is unchanged.
fn round_trip<T: DeserializeOwned + Serialize>(value: Value) -> T {
    let decoded: T = serde_json::from_value(value.clone())
        .unwrap_or_else(|error| panic!("decode failed: {error}\n{value}"));
    assert_eq!(serde_json::to_value(&decoded).unwrap(), value);
    decoded
}

fn rejects<T: DeserializeOwned>(value: Value) {
    let result = serde_json::from_value::<T>(value.clone());
    assert!(result.is_err(), "expected decode failure for {value}");
}

fn merge(mut base: Value, extra: Value) -> Value {
    base.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    base
}

fn without(mut value: Value, key: &str) -> Value {
    value.as_object_mut().unwrap().remove(key);
    value
}

// ---------------------------------------------------------------------------
// Thread shell
// ---------------------------------------------------------------------------

fn shell_json() -> Value {
    json!({
        "createdBy": "user",
        "creationSource": "web",
        "id": "thread-1",
        "projectId": "project-1",
        "title": "Thread",
        "providerInstanceId": "claudeAgent",
        "modelSelection": { "instanceId": "claudeAgent", "model": "claude-sonnet" },
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "branch": null,
        "worktreePath": null,
        "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": "thread-1" },
        "forkedFrom": null,
        "activeProviderThreadId": "provider-thread-1",
        "latestRunId": "run-1",
        "activeRunId": null,
        "status": "completed",
        "pendingRuntimeRequest": null,
        "latestVisibleMessage": null,
        "latestUserMessageAt": null,
        "hasActionableProposedPlan": false,
        "itemCount": 0,
        "visibleItemCount": 0,
        "createdAt": NOW,
        "updatedAt": NOW,
        "archivedAt": null,
        "settledOverride": null,
        "settledAt": null,
        "deletedAt": null,
        "pendingBackgroundTasks": [],
        "providerInstanceHistory": [],
    })
}

#[test]
fn thread_shell_defaults_are_filled_and_always_re_encoded() {
    // Historical rows omit both defaulted keys.
    let historical = without(
        without(shell_json(), "pendingBackgroundTasks"),
        "providerInstanceHistory",
    );
    let shell: OrchestrationV2ThreadShell = serde_json::from_value(historical).unwrap();
    assert!(shell.pending_background_tasks.is_empty());
    assert!(shell.provider_instance_history.is_empty());

    // `withDecodingDefault` keys are always written (no skip_serializing_if).
    let encoded = serde_json::to_value(&shell).unwrap();
    assert_eq!(encoded["pendingBackgroundTasks"], json!([]));
    assert_eq!(encoded["providerInstanceHistory"], json!([]));
    assert_eq!(encoded, shell_json());
    assert_eq!(
        serde_json::from_value::<OrchestrationV2ThreadShell>(encoded).unwrap(),
        shell
    );
}

#[test]
fn thread_shell_optional_nullable_fields_keep_absent_and_null_distinct() {
    // Every `optional(NullOr(X))` key of the shell.
    let keys = [
        "latestRunRequestedAt",
        "latestRunStartedAt",
        "latestRunCompletedAt",
        "activityRunStartedAt",
        "activityRunStatus",
        "lastError",
        "lastErrorClass",
        "usageLimitResetAt",
        "latestUserAuthoredMessageAt",
        "goal",
        "unsettledAt",
        "snoozedUntil",
        "snoozedAt",
        "limitRecovery",
        "pinnedAt",
        "autoSettleDisabledAt",
        "pinOrderKey",
        "activeOrderKey",
        "lastVisitedAt",
        "titleRegeneration",
        "linkedPullRequest",
        "branchPullRequest",
    ];
    // Absent stays absent in the encoding (except the defaulted keys).
    let absent: OrchestrationV2ThreadShell = serde_json::from_value(shell_json()).unwrap();
    let encoded = serde_json::to_value(&absent).unwrap();
    for key in keys {
        assert!(encoded.get(key).is_none(), "{key} must stay absent");
    }
    // Null stays null.
    let nulls = keys
        .iter()
        .fold(shell_json(), |acc, key| merge(acc, json!({ *key: null })));
    let with_nulls: OrchestrationV2ThreadShell = serde_json::from_value(nulls.clone()).unwrap();
    let encoded = serde_json::to_value(&with_nulls).unwrap();
    for key in keys {
        assert_eq!(encoded[key], Value::Null, "{key} must stay null");
    }
    assert_ne!(with_nulls, absent);
}

#[test]
fn thread_shell_decodes_every_optional_field_with_a_value() {
    let full = merge(
        merge(
            shell_json(),
            json!({
                "linkedPullRequest": { "projectId": "project-1", "repository": "o/r", "number": 3, "url": "https://x/3" },
                "pullRequests": [],
                "branchPullRequest": null,
                "historyOrigin": "v1_import",
                "latestRunRequestedAt": NOW,
                "latestRunStartedAt": NOW,
                "latestRunCompletedAt": NOW,
                "activityRunStartedAt": NOW,
                "activityRunStatus": "waiting",
                "lastError": "boom",
                "lastErrorClass": null,
                "usageLimitResetAt": "not-a-timestamp-is-just-a-string",
                "latestUserAuthoredMessageAt": NOW,
                "pendingBackgroundTasks": [],
                "providerInstanceHistory": ["codex", "claudeAgent"],
                "goal": null,
            }),
        ),
        json!({
            "unsettledAt": NOW,
            "snoozedUntil": NOW,
            "snoozedAt": NOW,
            "pinnedAt": NOW,
            "autoSettleDisabledAt": NOW,
            "pinOrderKey": "a0",
            "activeOrderKey": "a1",
            "lastVisitedAt": NOW,
            "titleRegeneration": { "requestId": "command-1", "startedAt": NOW },
        }),
    );
    let shell: OrchestrationV2ThreadShell = round_trip(full);
    assert_eq!(shell.provider_instance_history.len(), 2);
    assert_eq!(
        shell.activity_run_status,
        Some(Some(OrchestrationV2ActivityRunStatus::Waiting))
    );
}

#[test]
fn thread_shell_literals_are_closed() {
    // `status` is "idle" or any run status.
    for status in [
        "idle",
        "preparing",
        "queued",
        "starting",
        "running",
        "waiting",
        "completed",
        "interrupted",
        "failed",
        "cancelled",
        "rolled_back",
    ] {
        let shell: OrchestrationV2ThreadShell =
            serde_json::from_value(merge(shell_json(), json!({ "status": status }))).unwrap();
        assert_eq!(serde_json::to_value(&shell).unwrap()["status"], status);
    }
    rejects::<OrchestrationV2ThreadShell>(merge(shell_json(), json!({ "status": "paused" })));
    // `activityRunStatus` is the four live statuses only.
    for status in ["preparing", "starting", "running", "waiting"] {
        let shell: OrchestrationV2ThreadShell =
            serde_json::from_value(merge(shell_json(), json!({ "activityRunStatus": status })))
                .unwrap();
        assert_eq!(
            serde_json::to_value(&shell).unwrap()["activityRunStatus"],
            status
        );
    }
    for status in ["idle", "queued", "completed"] {
        rejects::<OrchestrationV2ThreadShell>(merge(
            shell_json(),
            json!({ "activityRunStatus": status }),
        ));
    }
    rejects::<OrchestrationV2ThreadShell>(merge(
        shell_json(),
        json!({ "settledOverride": "maybe" }),
    ));
    for value in ["settled", "active"] {
        let shell: OrchestrationV2ThreadShell =
            serde_json::from_value(merge(shell_json(), json!({ "settledOverride": value })))
                .unwrap();
        assert_eq!(
            serde_json::to_value(&shell).unwrap()["settledOverride"],
            value
        );
    }
    // Counts are non-negative integers; keys are trimmed non-empty strings.
    rejects::<OrchestrationV2ThreadShell>(merge(shell_json(), json!({ "itemCount": -1 })));
    rejects::<OrchestrationV2ThreadShell>(merge(shell_json(), json!({ "itemCount": 1.5 })));
    rejects::<OrchestrationV2ThreadShell>(merge(shell_json(), json!({ "pinOrderKey": "  " })));
    rejects::<OrchestrationV2ThreadShell>(merge(shell_json(), json!({ "branch": " " })));
}

#[test]
fn thread_shell_timestamps_normalise_to_millisecond_utc() {
    let shell: OrchestrationV2ThreadShell = serde_json::from_value(merge(
        shell_json(),
        json!({ "updatedAt": "2026-04-20T02:00:00+02:00", "archivedAt": "2026-04-20T00:00:00Z" }),
    ))
    .unwrap();
    let encoded = serde_json::to_value(&shell).unwrap();
    assert_eq!(encoded["updatedAt"], "2026-04-20T00:00:00.000Z");
    assert_eq!(encoded["archivedAt"], "2026-04-20T00:00:00.000Z");
}

// ---------------------------------------------------------------------------
// Shell snapshots and stream items
// ---------------------------------------------------------------------------

fn thread_shell_snapshot_json() -> Value {
    json!({
        "schemaVersion": 1,
        "snapshotSequence": 7,
        "threads": [shell_json()],
        "archivedThreads": [],
    })
}

fn project_shell_json() -> Value {
    json!({
        "id": "project-1",
        "title": "Project",
        "workspaceRoot": "/tmp/project",
        "defaultModelSelection": null,
        "scripts": [],
        "createdAt": NOW,
        "updatedAt": NOW,
    })
}

#[test]
fn shell_snapshots_require_a_positive_schema_version() {
    let snapshot: OrchestrationV2ThreadShellSnapshot =
        serde_json::from_value(thread_shell_snapshot_json()).unwrap();
    assert_eq!(snapshot.schema_version, PositiveInt::new(1).unwrap());
    assert_eq!(snapshot.threads.len(), 1);
    rejects::<OrchestrationV2ThreadShellSnapshot>(merge(
        thread_shell_snapshot_json(),
        json!({ "schemaVersion": 0 }),
    ));
    rejects::<OrchestrationV2ThreadShellSnapshot>(merge(
        thread_shell_snapshot_json(),
        json!({ "snapshotSequence": -1 }),
    ));
}

#[test]
fn shell_stream_items_cover_every_kind_and_reject_others() {
    let project = project_shell_json();
    let full = merge(
        thread_shell_snapshot_json(),
        json!({ "projects": [project.clone()] }),
    );
    let items = [
        json!({ "kind": "synchronized" }),
        json!({ "kind": "snapshot", "snapshot": full.clone() }),
        json!({
            "kind": "snapshot",
            "snapshot": full,
            "resolvedRepositoryIdentityRoots": ["/tmp/a", "/tmp/b"],
        }),
        json!({ "kind": "project.updated", "sequence": 3, "project": project }),
        json!({ "kind": "project.removed", "sequence": 4, "projectId": "project-1" }),
        json!({ "kind": "thread.updated", "sequence": 5, "location": "archive", "thread": shell_json() }),
        json!({ "kind": "thread.removed", "sequence": 6, "location": "active", "threadId": "thread-1" }),
    ];
    for item in items {
        let _: OrchestrationV2ShellStreamItem = round_trip(item);
    }
    // Shell frames are not forward compatible: an unknown kind is an error.
    rejects::<OrchestrationV2ShellStreamItem>(json!({ "kind": "thread.exploded" }));
    rejects::<OrchestrationV2ShellStreamItem>(json!({
        "kind": "thread.updated", "sequence": 5, "location": "elsewhere", "thread": shell_json(),
    }));
}

#[test]
fn archived_shell_stream_has_no_location() {
    let items = [
        json!({
            "kind": "snapshot",
            "snapshot": { "schemaVersion": 1, "snapshotSequence": 0, "projects": [], "threads": [] },
        }),
        json!({ "kind": "thread.updated", "sequence": 1, "thread": shell_json() }),
        json!({ "kind": "thread.removed", "sequence": 2, "threadId": "thread-1" }),
    ];
    for item in items {
        let _: OrchestrationV2ArchivedShellStreamItem = round_trip(item);
    }
    rejects::<OrchestrationV2ArchivedShellStreamItem>(json!({ "kind": "synchronized" }));
}

#[test]
fn stored_event_command_id_is_nullable() {
    let event = json!({
        "id": "event-1",
        "type": "provider-session.detached",
        "threadId": "thread-1",
        "occurredAt": NOW,
        "payload": { "providerSessionId": "provider-session-1", "detachedAt": NOW },
    });
    let stored: OrchestrationV2StoredEvent = round_trip(json!({
        "sequence": 9, "commandId": null, "event": event.clone(),
    }));
    assert!(stored.command_id.is_none());
    let _: OrchestrationV2StoredEvent = round_trip(json!({
        "sequence": 10, "commandId": "command-1", "event": event.clone(),
    }));
    rejects::<OrchestrationV2StoredEvent>(
        json!({ "sequence": -1, "commandId": null, "event": event }),
    );
}

// ---------------------------------------------------------------------------
// Event type registry and unknown-tag helpers
// ---------------------------------------------------------------------------

#[test]
fn known_event_types_match_the_domain_event_union() {
    // upstream DomainEvent literals: 1 created + 20 lifecycle + 1 link + 22 others.
    assert_eq!(KNOWN_ORCHESTRATION_V2_EVENT_TYPES.len(), 44);
    let mut sorted: Vec<_> = KNOWN_ORCHESTRATION_V2_EVENT_TYPES.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 44, "no duplicates");

    let base = |kind: &str| json!({ "id": "event-1", "type": kind, "threadId": "thread-1", "occurredAt": NOW });
    for kind in KNOWN_ORCHESTRATION_V2_EVENT_TYPES {
        assert!(is_known_orchestration_v2_event_type(kind));
        // A listed type reaches its own member (and fails there for the
        // missing payload); it is never reported as an unknown variant.
        let error = serde_json::from_value::<OrchestrationV2DomainEvent>(base(kind))
            .unwrap_err()
            .to_string();
        assert!(!error.contains("unknown variant"), "{kind}: {error}");
    }
    let error = serde_json::from_value::<OrchestrationV2DomainEvent>(base("run.from-the-future"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("unknown variant"), "{error}");
    assert!(!is_known_orchestration_v2_event_type("run.from-the-future"));
    assert!(!is_known_orchestration_v2_event_type(""));
}

#[test]
fn unknown_turn_item_detection_uses_has_unknown_union_tag() {
    assert!(is_unknown_turn_item(&json!({ "type": "hologram" })));
    // A present non-string tag is unknown too (hasUnknownUnionTag).
    assert!(is_unknown_turn_item(&json!({ "type": 5 })));
    assert!(is_unknown_turn_item(&json!({ "type": null })));
    assert!(!is_unknown_turn_item(&json!({ "type": "error" })));
    assert!(!is_unknown_turn_item(&json!({})));
    assert!(!is_unknown_turn_item(&json!(null)));
    assert!(!is_unknown_turn_item(&json!("error")));
    assert!(has_unknown_union_tag(
        &json!({ "type": 5 }),
        "type",
        &["error"]
    ));
}

#[test]
fn thread_stream_treats_a_non_string_turn_item_type_as_unknown() {
    let frame = |item_type: Value| {
        json!({
            "kind": "event",
            "sequence": 11,
            "event": {
                "id": "event-1",
                "type": "turn-item.updated",
                "threadId": "thread-1",
                "occurredAt": NOW,
                "payload": { "type": item_type },
            },
        })
    };
    for item_type in [json!("hologram"), json!(7)] {
        let item: OrchestrationV2ThreadStreamItem =
            serde_json::from_value(frame(item_type)).unwrap();
        assert_eq!(
            item,
            OrchestrationV2ThreadStreamItem::UnknownEvent {
                sequence: 11,
                event_type: "turn-item.updated".to_owned()
            }
        );
        assert!(serde_json::to_value(&item).is_err());
    }
    // A known item type with a broken payload is a defect, not an unknown arm.
    rejects::<OrchestrationV2ThreadStreamItem>(frame(json!("error")));
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn creation() -> Value {
    json!({ "createdBy": "user", "creationSource": "web" })
}

fn model() -> Value {
    json!({ "instanceId": "codex", "model": "gpt-5.4" })
}

fn pr_key() -> Value {
    json!({ "host": "github.com", "repository": "pingdotgg/t3code", "number": 42 })
}

fn snapshot_json() -> Value {
    json!({
        "state": "open",
        "title": "Title",
        "headBranch": "feature",
        "baseBranch": "main",
        "isDraft": false,
        "updatedAt": null,
        "syncedAt": NOW,
    })
}

fn watch_json() -> Value {
    json!({
        "startedAt": NOW,
        "headSha": null,
        "failedChecks": [],
        "passed": false,
        "passedChecks": [],
        "remarksThrough": NOW,
        "remarkIds": [],
        "conflicting": false,
        "wakes": 0,
    })
}

fn linked_pr() -> Value {
    json!({ "projectId": "project-1", "repository": "o/r", "number": 3, "url": "https://x/3" })
}

/// A wire-canonical sample of every client command, keyed by `type`, with its
/// required fields only (optional keys omitted).
fn client_command_samples() -> Vec<Value> {
    let c = || json!({ "commandId": "command-1", "threadId": "thread-1" });
    let with = |kind: &str, extra: Value| merge(merge(json!({ "type": kind }), c()), extra);
    let parent = |kind: &str, extra: Value| {
        merge(
            json!({ "type": kind, "commandId": "command-1", "parentThreadId": "thread-parent" }),
            extra,
        )
    };
    vec![
        merge(
            merge(json!({ "type": "thread.create" }), creation()),
            json!({
                "commandId": "command-1", "threadId": "thread-1", "projectId": "project-1",
                "title": "Title", "modelSelection": model(), "runtimeMode": "full-access",
                "interactionMode": "default", "branch": null, "worktreePath": null,
            }),
        ),
        with("thread.archive", json!({})),
        with("thread.unarchive", json!({})),
        with("thread.delete", json!({})),
        with("thread.settle", json!({})),
        with("thread.auto-settle", json!({ "snapshotAt": NOW })),
        with("thread.unsettle", json!({ "reason": "user" })),
        with(
            "thread.snooze",
            json!({ "snoozedUntil": "2026-05-01T00:00:00.000Z" }),
        ),
        with("thread.unsnooze", json!({ "reason": "user" })),
        with("thread.auto-settle.set", json!({ "enabled": true })),
        with("thread.pin", json!({})),
        with("thread.unpin", json!({})),
        with("thread.pin.reorder", json!({ "orderKey": "a0" })),
        with("thread.active.reorder", json!({ "orderKey": "a0" })),
        with("thread.visit", json!({ "visitedAt": NOW })),
        with("thread.mark-unread", json!({})),
        with("thread.metadata.update", json!({})),
        with(
            "thread.pull-request.link",
            merge(
                pr_key(),
                json!({ "url": "https://x/42", "source": "agent" }),
            ),
        ),
        with("thread.pull-request.unlink", pr_key()),
        with(
            "thread.pull-request-link.sync",
            merge(
                pr_key(),
                json!({ "snapshot": snapshot_json(), "stack": null }),
            ),
        ),
        with(
            "thread.pull-request.watch",
            merge(pr_key(), json!({ "watching": true })),
        ),
        with(
            "thread.pull-request.sync",
            json!({
                "projectId": "project-1",
                "snapshotSequence": 4,
                "expected": {
                    "workspaceRoot": "/tmp/p", "branch": null, "worktreePath": null,
                    "linkedPullRequest": null, "branchPullRequest": null,
                },
                "branchPullRequest": null,
            }),
        ),
        with(
            "thread.title.regeneration.complete",
            json!({ "requestId": "command-2" }),
        ),
        with(
            "thread.runtime-mode.set",
            json!({ "runtimeMode": "approval-required" }),
        ),
        with(
            "thread.interaction-mode.set",
            json!({ "interactionMode": "plan" }),
        ),
        with(
            "thread.model-selection.set",
            json!({ "modelSelection": model() }),
        ),
        with(
            "provider-session.detach",
            json!({ "providerSessionId": "provider-session-1" }),
        ),
        merge(
            merge(
                with(
                    "message.dispatch",
                    json!({
                        "messageId": "message-1", "text": "hi", "attachments": [],
                        "dispatchMode": { "type": "queue_after_active" },
                    }),
                ),
                creation(),
            ),
            json!({}),
        ),
        with("prepared-run.release", json!({ "runId": "run-1" })),
        with(
            "notification.delivery.accept",
            json!({ "messageId": "message-1" }),
        ),
        with(
            "prepared-run.progress",
            json!({ "runId": "run-1", "phase": "setup" }),
        ),
        with(
            "prepared-run.fail",
            json!({ "runId": "run-1", "failure": { "class": "unknown", "message": "boom", "code": null, "retryable": null } }),
        ),
        with("prepared-run.retry", json!({ "runId": "run-1" })),
        with(
            "subagent.stop",
            json!({ "runId": "run-1", "subagentId": "node-1" }),
        ),
        with("run.interrupt", json!({ "runId": "run-1" })),
        with(
            "queued-message.promote-to-steer",
            json!({ "queuedRunId": "run-2", "targetRunId": "run-1" }),
        ),
        with("queue.resume", json!({})),
        with(
            "queued-run.reorder",
            json!({ "runId": "run-1", "beforeRunId": null }),
        ),
        with("queued-run.cancel", json!({ "runId": "run-1" })),
        with(
            "queued-run.edit",
            json!({ "runId": "run-1", "text": "edited" }),
        ),
        with(
            "runtime-request.respond",
            json!({ "requestId": "request-1" }),
        ),
        with(
            "thread.user-input.dismiss",
            json!({ "requestId": "request-1" }),
        ),
        with(
            "checkpoint.rollback",
            json!({ "scopeId": "scope-1", "checkpointId": "checkpoint-1" }),
        ),
        merge(
            merge(json!({ "type": "thread.fork" }), creation()),
            json!({
                "commandId": "command-1", "sourceThreadId": "thread-1", "targetThreadId": "thread-2",
                "sourcePoint": { "type": "latest_stable" },
            }),
        ),
        merge(
            merge(json!({ "type": "thread.merge_back" }), creation()),
            json!({
                "commandId": "command-1", "sourceThreadId": "thread-1", "targetThreadId": "thread-2",
                "sourcePoint": { "type": "latest_stable" },
            }),
        ),
        merge(
            merge(json!({ "type": "delegated_task.request" }), creation()),
            json!({
                "commandId": "command-1", "parentThreadId": "thread-parent", "parentRunId": "run-1",
                "parentNodeId": "node-1", "task": "Do it", "modelSelection": model(),
                "runtimeMode": "full-access", "interactionMode": "default",
            }),
        ),
        parent(
            "delegated_task.wake-policy",
            json!({ "taskId": "node-1", "completionWake": "settled_only" }),
        ),
        parent(
            "delegated_task.completion-delivery.acknowledge",
            json!({ "taskId": "node-1", "observedByRunId": null }),
        ),
        parent(
            "delegated_task.completion-delivery.dispose",
            json!({ "taskId": "node-1" }),
        ),
        parent(
            "thread.created.record",
            json!({
                "parentRunId": "run-1", "parentNodeId": "node-1",
                "targetThreadId": "thread-2", "targetRunId": null,
            }),
        ),
        with("provider.switch", json!({ "modelSelection": model() })),
    ]
}

#[test]
fn every_client_command_round_trips_byte_for_byte() {
    let samples = client_command_samples();
    // Upstream declares 51 command members.
    assert_eq!(samples.len(), 51);
    let mut seen = std::collections::BTreeSet::new();
    for sample in samples {
        assert!(
            seen.insert(sample["type"].as_str().unwrap().to_owned()),
            "duplicate sample"
        );
        let _: OrchestrationV2Command = round_trip(sample);
    }
}

#[test]
fn client_commands_reject_missing_required_fields_and_wrong_literals() {
    for sample in client_command_samples() {
        // The tag is mandatory and closed.
        rejects::<OrchestrationV2Command>(without(sample.clone(), "type"));
        rejects::<OrchestrationV2Command>(merge(sample.clone(), json!({ "type": "thread.nope" })));
        // Every command carries a command id.
        rejects::<OrchestrationV2Command>(without(sample.clone(), "commandId"));
    }
    let samples = client_command_samples();
    let find = |kind: &str| {
        samples
            .iter()
            .find(|sample| sample["type"] == kind)
            .unwrap()
            .clone()
    };
    // Literal("user") reasons.
    rejects::<OrchestrationV2Command>(merge(find("thread.unsettle"), json!({ "reason": "agent" })));
    rejects::<OrchestrationV2Command>(merge(find("thread.unsnooze"), json!({ "reason": "agent" })));
    // Closed literal sets.
    rejects::<OrchestrationV2Command>(merge(
        find("prepared-run.progress"),
        json!({ "phase": "teardown" }),
    ));
    rejects::<OrchestrationV2Command>(merge(
        find("delegated_task.wake-policy"),
        json!({ "completionWake": "never" }),
    ));
    // PositiveInt pull request numbers.
    rejects::<OrchestrationV2Command>(merge(
        find("thread.pull-request.unlink"),
        json!({ "number": 0 }),
    ));
    rejects::<OrchestrationV2Command>(merge(
        find("thread.pull-request.unlink"),
        json!({ "number": -3 }),
    ));
    // Trimmed non-empty strings.
    rejects::<OrchestrationV2Command>(merge(
        find("thread.pin.reorder"),
        json!({ "orderKey": " " }),
    ));
    rejects::<OrchestrationV2Command>(merge(find("thread.create"), json!({ "title": "" })));
    // NonNegativeInt.
    rejects::<OrchestrationV2Command>(merge(
        find("thread.pull-request.sync"),
        json!({ "snapshotSequence": -1 }),
    ));
}

#[test]
fn thread_metadata_update_keeps_absent_null_and_value_distinct() {
    let base = json!({
        "type": "thread.metadata.update", "commandId": "command-1", "threadId": "thread-1",
    });
    let nullable = [
        "branch",
        "worktreePath",
        "expectedWorktreePath",
        "limitRecovery",
        "linkedPullRequest",
    ];
    // Absent fields stay absent.
    let command: OrchestrationV2Command = round_trip(base.clone());
    let OrchestrationV2Command::ThreadMetadataUpdate {
        branch,
        linked_pull_request,
        ..
    } = &command
    else {
        panic!("expected thread.metadata.update");
    };
    assert_eq!((branch, linked_pull_request), (&None, &None));

    // Explicit null means "clear", and survives re-encoding.
    let cleared = nullable
        .iter()
        .fold(base.clone(), |acc, key| merge(acc, json!({ *key: null })));
    let OrchestrationV2Command::ThreadMetadataUpdate {
        branch,
        linked_pull_request,
        limit_recovery,
        ..
    } = round_trip::<OrchestrationV2Command>(cleared)
    else {
        panic!("expected thread.metadata.update");
    };
    assert_eq!(branch, Some(None));
    assert_eq!(linked_pull_request, Some(None));
    assert_eq!(limit_recovery, Some(None));

    // Values; plus the other optional keys.
    let full = merge(
        base,
        json!({
            "title": "New", "regenerateTitle": false, "branch": "main", "worktreePath": "/tmp/w",
            "expectedWorktreePath": null, "expectedEmpty": true,
            "limitRecovery": { "runId": "run-1", "resetAt": "2026-09-20T21:00:00.000Z", "snooze": true },
            "linkedPullRequest": linked_pr(),
        }),
    );
    let _: OrchestrationV2Command = round_trip(full);
}

#[test]
fn thread_metadata_update_limit_recovery_needs_a_choice() {
    rejects::<OrchestrationV2Command>(json!({
        "type": "thread.metadata.update", "commandId": "command-1", "threadId": "thread-1",
        "limitRecovery": { "runId": "run-1", "resetAt": "2026-09-20T21:00:00.000Z" },
    }));
}

#[test]
fn message_dispatch_modes_and_optional_fields() {
    let base = merge(
        json!({
            "type": "message.dispatch", "commandId": "command-1", "threadId": "thread-1",
            "messageId": "message-1", "text": "hi", "attachments": [],
        }),
        creation(),
    );
    let modes = [
        json!({ "type": "defer_start" }),
        json!({ "type": "defer_start", "workspaceStrategy": { "type": "root", "branch": "main" } }),
        json!({ "type": "defer_start", "workspaceStrategy": { "type": "existing_worktree", "worktreePath": "/w" } }),
        json!({
            "type": "defer_start",
            "workspaceStrategy": { "type": "worktree", "baseRef": "main", "startFromOrigin": true },
        }),
        json!({ "type": "steer_active", "targetRunId": "run-1" }),
        json!({ "type": "restart_active", "targetRunId": "run-1" }),
        json!({ "type": "queue_after_active" }),
        json!({ "type": "start_immediately" }),
    ];
    for mode in modes {
        let _: OrchestrationV2Command =
            round_trip(merge(base.clone(), json!({ "dispatchMode": mode })));
    }
    // The mode is required, closed, and carries its own required fields.
    rejects::<OrchestrationV2Command>(base.clone());
    for bad in [
        json!({ "type": "run_later" }),
        json!({ "type": "steer_active" }),
        json!({ "type": "restart_active" }),
    ] {
        rejects::<OrchestrationV2Command>(merge(base.clone(), json!({ "dispatchMode": bad })));
    }
    let full = merge(
        base.clone(),
        json!({
            "senderThreadId": "thread-2", "scheduledTaskId": "task-1", "titleSeed": "Seed",
            "modelSelection": model(),
            "sourcePlanRef": { "threadId": "thread-1", "planId": "plan-1" },
            "restartContinuationOfRunId": "run-1", "usageLimitContinuationOfRunId": "run-2",
            "manualContinuationOfRunId": "run-3", "usageLimitRecoveryRequestId": "command-9",
            "deliveryIntent": "steer",
            "delegatedCompletion": { "parentRunId": "run-1", "generation": 2, "taskIds": ["node-1"] },
            "dispatchMode": { "type": "start_immediately" },
        }),
    );
    let _: OrchestrationV2Command = round_trip(full.clone());
    for intent in ["auto", "steer", "restart"] {
        let _: OrchestrationV2Command =
            round_trip(merge(full.clone(), json!({ "deliveryIntent": intent })));
    }
    rejects::<OrchestrationV2Command>(merge(full.clone(), json!({ "deliveryIntent": "later" })));
    // generation is a PositiveInt.
    rejects::<OrchestrationV2Command>(merge(
        full,
        json!({ "delegatedCompletion": { "parentRunId": "run-1", "generation": 0, "taskIds": [] } }),
    ));
    // attachments are required on dispatch but optional on queued-run.edit.
    rejects::<OrchestrationV2Command>(without(base, "attachments"));
}

#[test]
fn remaining_command_optional_fields_round_trip() {
    let c = json!({ "commandId": "command-1", "threadId": "thread-1" });
    let full = |kind: &str, extra: Value| merge(merge(json!({ "type": kind }), c.clone()), extra);
    let commands = vec![
        full("thread.settle", json!({ "settledAt": NOW })),
        full(
            "thread.auto-settle",
            json!({ "snapshotAt": NOW, "settledAt": NOW }),
        ),
        full("thread.pin", json!({ "orderKey": "a0" })),
        full(
            "thread.pull-request.link",
            merge(
                pr_key(),
                json!({ "url": "https://x/42", "source": "stack-dismissed" }),
            ),
        ),
        full(
            "thread.pull-request-link.sync",
            merge(
                pr_key(),
                json!({
                    "snapshot": snapshot_json(),
                    "stack": {
                        "kind": "native", "id": "s1", "number": 1, "url": "https://x/s",
                        "base": "main", "layers": [],
                    },
                }),
            ),
        ),
        full(
            "thread.pull-request.watch",
            merge(
                pr_key(),
                json!({ "watching": true, "link": { "url": "https://x/42", "source": "manual" } }),
            ),
        ),
        full(
            "thread.pull-request.sync",
            json!({
                "projectId": "project-1", "snapshotSequence": 4,
                "expected": {
                    "workspaceRoot": "/tmp/p", "branch": "b", "worktreePath": "/w",
                    "linkedPullRequest": linked_pr(), "branchPullRequest": null,
                },
                "branchPullRequest": linked_pr(),
                "linkedPullRequest": linked_pr(),
            }),
        ),
        full(
            "thread.title.regeneration.complete",
            json!({ "requestId": "command-2", "title": "Done" }),
        ),
        full(
            "provider-session.detach",
            json!({ "providerSessionId": "provider-session-1", "reason": "user closed" }),
        ),
        full(
            "run.interrupt",
            json!({ "runId": "run-1", "reason": "stop", "holdQueue": true }),
        ),
        full(
            "queued-run.reorder",
            json!({ "runId": "run-1", "beforeRunId": "run-2" }),
        ),
        full(
            "queued-run.edit",
            json!({ "runId": "run-1", "text": "edited", "attachments": [] }),
        ),
        full(
            "runtime-request.respond",
            json!({
                "requestId": "request-1", "decision": "acceptForSession",
                "answers": { "q1": "yes", "q2": ["a", "b"] },
                "attachmentsByQuestionId": {},
            }),
        ),
        full(
            "checkpoint.rollback",
            json!({ "scopeId": "scope-1", "checkpointId": "checkpoint-1", "restoreFiles": false }),
        ),
        merge(
            merge(json!({ "type": "thread.fork" }), creation()),
            json!({
                "commandId": "command-1", "sourceThreadId": "thread-1", "targetThreadId": "thread-2",
                "sourcePoint": { "type": "latest_stable" }, "title": "Fork", "createdAt": NOW,
            }),
        ),
        merge(
            merge(json!({ "type": "thread.merge_back" }), creation()),
            json!({
                "commandId": "command-1", "sourceThreadId": "thread-1", "targetThreadId": "thread-2",
                "sourcePoint": { "type": "latest_stable" }, "createdAt": NOW,
            }),
        ),
        merge(
            merge(json!({ "type": "delegated_task.request" }), creation()),
            json!({
                "commandId": "command-1", "parentThreadId": "thread-parent", "parentRunId": "run-1",
                "parentNodeId": "node-1", "task": "Do it", "title": "T", "modelSelection": model(),
                "runtimeMode": "full-access", "interactionMode": "default",
                "workspaceStrategy": { "type": "root" }, "completionWake": "always", "createdAt": NOW,
            }),
        ),
        json!({
            "type": "delegated_task.completion-delivery.acknowledge", "commandId": "command-1",
            "parentThreadId": "thread-parent", "taskId": "node-1", "observedByRunId": "run-5",
        }),
        merge(
            merge(json!({ "type": "thread.create" }), creation()),
            json!({
                "commandId": "command-1", "threadId": "thread-1", "projectId": "project-1",
                "title": "Title", "modelSelection": model(), "runtimeMode": "full-access",
                "interactionMode": "default", "branch": "main", "worktreePath": "/w",
                "importedNativeThread": {
                    "ref": { "driver": "codex", "nativeId": "native-1", "strength": "strong" },
                },
            }),
        ),
    ];
    for command in commands {
        let _: OrchestrationV2Command = round_trip(command);
    }
    // Only strong native refs may be imported.
    for strength in ["weak", "none", "other"] {
        rejects::<OrchestrationV2Command>(merge(
            merge(json!({ "type": "thread.create" }), creation()),
            json!({
                "commandId": "command-1", "threadId": "thread-1", "projectId": "project-1",
                "title": "Title", "modelSelection": model(), "runtimeMode": "full-access",
                "interactionMode": "default", "branch": null, "worktreePath": null,
                "importedNativeThread": { "ref": { "driver": "codex", "nativeId": "n", "strength": strength } },
            }),
        ));
    }
}

fn internal_command_samples() -> Vec<Value> {
    let c = json!({ "commandId": "command-1", "threadId": "thread-1" });
    let with = |kind: &str, extra: Value| merge(merge(json!({ "type": kind }), c.clone()), extra);
    vec![
        with(
            "thread.pull-request-watch.sync",
            merge(pr_key(), json!({ "startedAt": NOW, "watch": null })),
        ),
        with(
            "thread.pull-request-watch.sync",
            merge(pr_key(), json!({ "startedAt": NOW, "watch": watch_json() })),
        ),
        with(
            "checkpoint.rollback.fail",
            json!({ "requestId": "command-2", "message": "gone" }),
        ),
        with(
            "thread.background-work.settle",
            json!({ "providerThreadId": "provider-thread-1", "providerTurnId": "provider-turn-1" }),
        ),
        with(
            "thread.background-work.settle",
            json!({
                "providerThreadId": "provider-thread-1", "providerTurnId": "provider-turn-1",
                "subagent": {
                    "id": "node-1", "stoppedAt": NOW,
                    "outcome": { "status": "cancelled", "completedAt": NOW, "result": null },
                },
            }),
        ),
        with("thread.stop", json!({})),
        with("thread.stop", json!({ "reason": "user" })),
        with(
            "secret_request.record",
            json!({
                "runId": "run-1", "nodeId": "node-1", "turnItemId": "turn-item-1",
                "label": "API key", "reason": "needed", "secretStatus": "pending",
            }),
        ),
        with(
            "secret_request.record",
            json!({
                "runId": "run-1", "nodeId": "node-1", "turnItemId": "turn-item-1",
                "label": "API key", "reason": "needed", "placeholder": "sk-", "secretStatus": "saved",
            }),
        ),
    ]
}

#[test]
fn internal_commands_round_trip_and_stay_out_of_the_client_union() {
    for sample in internal_command_samples() {
        let _: OrchestrationV2InternalCommand = round_trip(sample.clone());
        // No client can send them.
        rejects::<OrchestrationV2Command>(sample.clone());
        // The server accepts both families.
        let server: OrchestrationV2ServerCommand = round_trip(sample);
        assert!(matches!(server, OrchestrationV2ServerCommand::Internal(_)));
    }
    for sample in client_command_samples() {
        rejects::<OrchestrationV2InternalCommand>(sample.clone());
        let server: OrchestrationV2ServerCommand = round_trip(sample);
        assert!(matches!(server, OrchestrationV2ServerCommand::Client(_)));
    }
    rejects::<OrchestrationV2ServerCommand>(json!({ "type": "thread.nope", "commandId": "c" }));
    rejects::<OrchestrationV2InternalCommand>(merge(
        internal_command_samples().remove(6),
        json!({ "type": "thread.stop", "commandId": "" }),
    ));
    // Closed outcome status and secret status.
    rejects::<OrchestrationV2InternalCommand>(json!({
        "type": "thread.background-work.settle", "commandId": "command-1", "threadId": "thread-1",
        "providerThreadId": "provider-thread-1", "providerTurnId": "provider-turn-1",
        "subagent": {
            "id": "node-1", "stoppedAt": NOW,
            "outcome": { "status": "running", "completedAt": NOW, "result": null },
        },
    }));
    let secret = internal_command_samples().remove(7);
    rejects::<OrchestrationV2InternalCommand>(merge(secret, json!({ "secretStatus": "leaked" })));
}

// ---------------------------------------------------------------------------
// Subscription payloads and snapshots
// ---------------------------------------------------------------------------

fn projection_json() -> Value {
    json!({
        "thread": {
            "createdBy": "user", "creationSource": "web", "id": "thread-1", "projectId": "project-1",
            "title": "Thread", "providerInstanceId": "codex",
            "modelSelection": { "instanceId": "codex", "model": "gpt-5-codex" },
            "runtimeMode": "full-access", "interactionMode": "default",
            "branch": null, "worktreePath": null, "activeProviderThreadId": null,
            "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": "thread-1" },
            "forkedFrom": null, "createdAt": NOW, "updatedAt": NOW, "archivedAt": null, "deletedAt": null,
            "settledOverride": null, "settledAt": null, "lastVisitedAt": null,
        },
        "runs": [], "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
        "providerThreads": [], "providerTurns": [], "runtimeRequests": [], "messages": [],
        "plans": [], "turnItems": [], "checkpointScopes": [], "checkpoints": [],
        "contextHandoffs": [], "contextTransfers": [], "visibleTurnItems": [], "updatedAt": NOW,
    })
}

#[test]
fn subscribe_inputs_have_only_optional_keys() {
    let _: OrchestrationV2SubscribeShellInput = round_trip(json!({}));
    let _: OrchestrationV2SubscribeShellInput =
        round_trip(json!({ "afterSequence": 0, "requestCompletionMarker": false }));
    rejects::<OrchestrationV2SubscribeShellInput>(json!({ "afterSequence": -1 }));

    let _: OrchestrationV2SubscribeThreadInput = round_trip(json!({ "threadId": "thread-1" }));
    let _: OrchestrationV2SubscribeThreadInput = round_trip(json!({
        "threadId": "thread-1", "afterSequence": 12, "requestCompletionMarker": true,
        "acceptBoundedSnapshot": true, "acceptCompactTurnItems": false,
    }));
    rejects::<OrchestrationV2SubscribeThreadInput>(json!({}));
    rejects::<OrchestrationV2SubscribeThreadInput>(
        json!({ "threadId": "thread-1", "acceptBoundedSnapshot": "yes" }),
    );
}

#[test]
fn thread_detail_snapshot_keeps_cursor_null_distinct_from_absent() {
    let base = json!({ "snapshotSequence": 3, "projection": projection_json() });
    let absent: OrchestrationV2ThreadDetailSnapshot = round_trip(base.clone());
    assert_eq!(absent.history_cursor, None);
    assert_eq!(absent.latest_local_turn_ordinal, None);

    let nulls = merge(
        base.clone(),
        json!({ "historyCursor": null, "latestLocalTurnOrdinal": null }),
    );
    let decoded: OrchestrationV2ThreadDetailSnapshot = round_trip(nulls);
    assert_eq!(decoded.history_cursor, Some(None));
    assert_eq!(decoded.latest_local_turn_ordinal, Some(None));

    let full = merge(
        base.clone(),
        json!({ "historyCursor": "cursor-1", "hasMoreHistory": true, "latestLocalTurnOrdinal": 9 }),
    );
    let _: OrchestrationV2ThreadDetailSnapshot = round_trip(full);
    rejects::<OrchestrationV2ThreadDetailSnapshot>(merge(
        base.clone(),
        json!({ "historyCursor": " " }),
    ));
    rejects::<OrchestrationV2ThreadDetailSnapshot>(merge(
        base,
        json!({ "latestLocalTurnOrdinal": -1 }),
    ));
}

#[test]
fn bounded_snapshot_requires_cursor_and_ordinal_but_allows_null() {
    let base = json!({
        "snapshotSequence": 3, "projection": projection_json(),
        "historyCursor": null, "hasMoreHistory": false, "latestLocalTurnOrdinal": null,
    });
    let _: OrchestrationV2ThreadBoundedSnapshot = round_trip(base.clone());
    let full = merge(
        base.clone(),
        json!({
            "historyCursor": "c", "hasMoreHistory": true, "latestLocalTurnOrdinal": 4,
            "payloadBudgetExceeded": true, "turnItemsOmitLocalVisible": true,
        }),
    );
    let decoded: OrchestrationV2ThreadBoundedSnapshot = round_trip(full);
    assert_eq!(decoded.turn_items_omit_local_visible, Some(TrueLiteral));
    rejects::<OrchestrationV2ThreadBoundedSnapshot>(without(base.clone(), "hasMoreHistory"));
    rejects::<OrchestrationV2ThreadBoundedSnapshot>(merge(
        base,
        json!({ "turnItemsOmitLocalVisible": false }),
    ));
}

#[test]
fn thread_history_page_drops_unknown_items_but_not_broken_known_ones() {
    let known = |id: &str| {
        json!({
            "position": 0, "visibility": "local", "sourceThreadId": "thread-1",
            "sourceItemId": id,
            "item": {
                "id": id, "type": "system_notice", "threadId": "thread-1", "runId": null,
                "nodeId": null, "providerThreadId": null, "providerTurnId": null,
                "nativeItemRef": null, "parentItemId": null, "ordinal": 1, "status": "completed",
                "title": null, "startedAt": null, "completedAt": null, "updatedAt": NOW,
                "message": "hello",
            },
        })
    };
    let future = json!({
        "position": 1, "visibility": "inherited", "sourceThreadId": "thread-1",
        "sourceItemId": "future", "item": { "id": "future", "type": "hologram", "anything": 1 },
    });
    let page: OrchestrationV2ThreadHistoryPage = serde_json::from_value(json!({
        "snapshotSequence": 1,
        "items": [known("a"), future.clone(), known("b")],
        "nextCursor": null,
        "hasMoreHistory": false,
    }))
    .unwrap();
    assert_eq!(page.items.len(), 2);
    // Re-encoding is the plain array encoding of what was kept.
    let encoded = serde_json::to_value(&page).unwrap();
    assert_eq!(encoded["items"].as_array().unwrap().len(), 2);
    assert_eq!(encoded["nextCursor"], Value::Null);

    let mut broken = known("c");
    broken["item"]["message"] = json!(5);
    rejects::<OrchestrationV2ThreadHistoryPage>(json!({
        "snapshotSequence": 1, "items": [broken], "nextCursor": null, "hasMoreHistory": false,
    }));
    rejects::<OrchestrationV2ThreadHistoryPage>(json!({
        "snapshotSequence": 1, "items": [], "nextCursor": "  ", "hasMoreHistory": false,
    }));
}

#[test]
fn thread_snapshot_stream_frame_round_trips_history_metadata() {
    let base =
        json!({ "kind": "snapshot", "snapshotSequence": 2, "projection": projection_json() });
    let absent: OrchestrationV2ThreadStreamItem = round_trip(base.clone());
    let OrchestrationV2ThreadStreamItem::Snapshot { history_cursor, .. } = &absent else {
        panic!("expected snapshot");
    };
    assert_eq!(history_cursor, &None);
    let full = merge(
        base,
        json!({
            "historyCursor": null, "hasMoreHistory": false, "latestLocalTurnOrdinal": 0,
            "payloadBudgetExceeded": false, "turnItemsOmitLocalVisible": true,
        }),
    );
    let _: OrchestrationV2ThreadStreamItem = round_trip(full);
    let _: OrchestrationV2ThreadStreamItem = round_trip(json!({ "kind": "synchronized" }));
    rejects::<OrchestrationV2ThreadStreamItem>(json!({ "kind": "mystery" }));
    rejects::<OrchestrationV2ThreadStreamItem>(json!({}));
}

#[test]
fn launch_and_small_rpc_payloads() {
    let input = json!({
        "commandId": "command-1", "projectId": "project-1", "title": "Launch",
        "modelSelection": model(), "runtimeMode": "full-access", "interactionMode": "default",
        "workspaceStrategy": { "type": "root" },
    });
    let _: OrchestrationV2ThreadLaunchInput = round_trip(input.clone());
    let full = merge(
        input.clone(),
        json!({
            "creationSource": "mcp", "threadId": "thread-9", "reuseExistingThread": true,
            "generateTitle": false,
            "initialMessage": { "messageId": "message-1", "text": "go", "attachments": [] },
        }),
    );
    let launch: OrchestrationV2ThreadLaunchInput = round_trip(full);
    assert_eq!(launch.initial_message.unwrap().text, "go");
    rejects::<OrchestrationV2ThreadLaunchInput>(without(input.clone(), "workspaceStrategy"));
    rejects::<OrchestrationV2ThreadLaunchInput>(merge(input.clone(), json!({ "title": " " })));
    rejects::<OrchestrationV2ThreadLaunchInput>(merge(
        input,
        json!({ "initialMessage": { "text": "go" } }),
    ));

    let _: OrchestrationV2ThreadLaunchResult = round_trip(json!({
        "threadId": "thread-1", "projection": projection_json(), "resumed": false,
    }));
    let _: OrchestrationV2DispatchCommandResult = round_trip(json!({ "sequence": 0 }));
    rejects::<OrchestrationV2DispatchCommandResult>(json!({ "sequence": -1 }));
    let _: OrchestrationV2GetThreadProjectionInput = round_trip(json!({ "threadId": "thread-1" }));
    let _: OrchestrationV2StopWorkflowInput =
        round_trip(json!({ "threadId": "thread-1", "subagentId": "node-1" }));
    let _: OrchestrationV2GetWorkflowScriptInput =
        round_trip(json!({ "threadId": "thread-1", "scriptPath": "/tmp/s.js" }));
    rejects::<OrchestrationV2GetWorkflowScriptInput>(
        json!({ "threadId": "thread-1", "scriptPath": " " }),
    );
    let _: OrchestrationV2GetWorkflowScriptResult =
        round_trip(json!({ "scriptPath": "/tmp/s.js", "contents": "", "truncated": true }));
    let _: OrchestrationV2GetTurnItemInput = round_trip(json!({ "threadId": "t", "itemId": "i" }));
    let _: OrchestrationV2GetTurnItemInput =
        round_trip(json!({ "threadId": "t", "itemId": "i", "revision": NOW }));
    let result: OrchestrationV2GetTurnItemResult = round_trip(json!({ "item": null }));
    assert!(result.item.is_none());
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[test]
fn tagged_errors_encode_their_tag_and_derive_messages() {
    let dispatch: OrchestrationV2DispatchCommandError = round_trip(json!({
        "_tag": "OrchestrationV2DispatchCommandError",
        "commandId": "command-1", "commandType": "thread.create", "message": "nope",
    }));
    assert_eq!(dispatch.to_string(), "nope");
    let _: OrchestrationV2DispatchCommandError = round_trip(json!({
        "_tag": "OrchestrationV2DispatchCommandError",
        "commandId": "command-1", "commandType": "thread.create", "message": "nope",
        "detail": "why", "cause": { "code": 5 },
    }));
    rejects::<OrchestrationV2DispatchCommandError>(json!({
        "_tag": "OrchestrationV2GetThreadProjectionError",
        "commandId": "command-1", "commandType": "x", "message": "m",
    }));
    let _: OrchestrationV2GetThreadProjectionError = round_trip(json!({
        "_tag": "OrchestrationV2GetThreadProjectionError", "threadId": "thread-1", "message": "m",
    }));
    let _: OrchestrationV2GetShellSnapshotError =
        round_trip(json!({ "_tag": "OrchestrationV2GetShellSnapshotError", "message": "m" }));
    let launch: OrchestrationV2ThreadLaunchError = round_trip(json!({
        "_tag": "OrchestrationV2ThreadLaunchError",
        "commandId": "command-1", "projectId": "project-1", "message": "m",
    }));
    assert_eq!(launch.to_string(), "m");

    let messages = [
        ("not-running", "This workflow is no longer running."),
        ("unsupported", "This provider cannot stop a workflow."),
        ("unavailable", "This workflow has no live provider task."),
        ("stop-failed", "Failed to stop the workflow."),
    ];
    for (reason, message) in messages {
        let error: OrchestrationV2StopWorkflowError = round_trip(json!({
            "_tag": "OrchestrationV2StopWorkflowError",
            "threadId": "thread-1", "subagentId": "node-1", "reason": reason,
        }));
        assert_eq!(error.to_string(), message);
    }
    rejects::<OrchestrationV2StopWorkflowError>(json!({
        "_tag": "OrchestrationV2StopWorkflowError",
        "threadId": "thread-1", "subagentId": "node-1", "reason": "exploded",
    }));

    let file_reasons = [
        (
            "invalid-path",
            "Workflow files must be absolute paths with the expected extension.",
        ),
        ("root-unavailable", "Workflow file root unavailable."),
        ("not-found", "Workflow file not found."),
        ("outside-root", "Path is outside the workflow files root."),
        ("wrong-extension", "Resolved file has the wrong extension."),
        ("not-regular-file", "Workflow file is not a regular file."),
        (
            "changed-during-read",
            "Workflow file changed between resolution and open.",
        ),
        ("read-failed", "Workflow file read failed."),
    ];
    for (reason, message) in file_reasons {
        let error: OrchestrationWorkflowFileError = round_trip(json!({
            "_tag": "OrchestrationWorkflowFileError", "reason": reason, "path": "/x",
        }));
        assert_eq!(error.to_string(), message);
    }
    rejects::<OrchestrationWorkflowFileError>(json!({
        "_tag": "OrchestrationWorkflowFileError", "reason": "not-js", "path": "/x",
    }));

    let script_reasons = [
        (
            "invalid-path",
            "Workflow scripts must be absolute .js paths.",
        ),
        ("root-unavailable", "Script root unavailable."),
        ("not-found", "Script not found."),
        (
            "outside-root",
            "Script path is outside the workflow scripts root.",
        ),
        ("not-js", "Resolved script is not a .js file."),
        ("not-regular-file", "Script is not a regular file."),
        (
            "changed-during-read",
            "Script changed between resolution and open.",
        ),
        ("read-failed", "Script read failed."),
    ];
    for (reason, message) in script_reasons {
        let error: OrchestrationGetWorkflowScriptError = round_trip(json!({
            "_tag": "OrchestrationGetWorkflowScriptError", "reason": reason, "scriptPath": "/x",
        }));
        assert_eq!(error.to_string(), message);
    }
    rejects::<OrchestrationGetWorkflowScriptError>(json!({
        "_tag": "OrchestrationGetWorkflowScriptError", "reason": "wrong-extension", "scriptPath": "/x",
    }));

    // `cause` is required on the search error (Schema.Defect()), optional elsewhere.
    let search: OrchestrationV2SearchThreadError = round_trip(json!({
        "_tag": "OrchestrationV2SearchThreadError", "cause": "boom",
    }));
    assert_eq!(
        search.to_string(),
        "Could not search this thread. Please retry."
    );
    rejects::<OrchestrationV2SearchThreadError>(
        json!({ "_tag": "OrchestrationV2SearchThreadError" }),
    );
}

#[test]
fn rpc_error_is_exactly_the_five_upstream_members() {
    let members = [
        json!({
            "_tag": "OrchestrationV2DispatchCommandError",
            "commandId": "command-1", "commandType": "x", "message": "m",
        }),
        json!({ "_tag": "OrchestrationV2GetThreadProjectionError", "threadId": "t", "message": "m" }),
        json!({ "_tag": "OrchestrationV2GetShellSnapshotError", "message": "m" }),
        json!({
            "_tag": "OrchestrationV2ThreadLaunchError",
            "commandId": "command-1", "projectId": "p", "message": "m",
        }),
        json!({
            "_tag": "OrchestrationV2StopWorkflowError",
            "threadId": "t", "subagentId": "n", "reason": "not-running",
        }),
    ];
    for member in members {
        let _: OrchestrationV2RpcError = round_trip(member);
    }
    // Search and workflow-file errors are separate RPC failures, not union members.
    for other in [
        json!({ "_tag": "OrchestrationV2SearchThreadError", "cause": "x" }),
        json!({ "_tag": "OrchestrationWorkflowFileError", "reason": "not-found", "path": "/x" }),
        json!({ "_tag": "OrchestrationGetWorkflowScriptError", "reason": "not-found", "scriptPath": "/x" }),
    ] {
        rejects::<OrchestrationV2RpcError>(other);
    }
}

// ---------------------------------------------------------------------------
// Thread search (length checks count UTF-16 code units)
// ---------------------------------------------------------------------------

fn search_input(extra: Value) -> Value {
    merge(json!({ "threadId": "thread-1", "query": "needle" }), extra)
}

#[test]
fn search_query_is_trimmed_non_empty_and_at_most_200_utf16_units() {
    let input: OrchestrationV2SearchThreadInput = round_trip(search_input(json!({})));
    assert_eq!(input.query.as_str(), "needle");
    // Trimmed on decode, so the padded form re-encodes trimmed.
    let padded: OrchestrationV2SearchThreadInput =
        serde_json::from_value(search_input(json!({ "query": "  needle \n" }))).unwrap();
    assert_eq!(serde_json::to_value(&padded).unwrap()["query"], "needle");

    rejects::<OrchestrationV2SearchThreadInput>(search_input(json!({ "query": "   " })));
    rejects::<OrchestrationV2SearchThreadInput>(search_input(json!({ "query": "" })));

    let at_limit = "a".repeat(200);
    let _: OrchestrationV2SearchThreadInput =
        round_trip(search_input(json!({ "query": at_limit })));
    rejects::<OrchestrationV2SearchThreadInput>(search_input(json!({ "query": "a".repeat(201) })));
    // The limit is on the trimmed value.
    let padded_limit = format!("  {}  ", "a".repeat(200));
    let _: OrchestrationV2SearchThreadInput =
        serde_json::from_value(search_input(json!({ "query": padded_limit }))).unwrap();

    // 100 astral code points are 200 UTF-16 units; 101 are 202 (and 101 chars).
    let _: OrchestrationV2SearchThreadInput =
        round_trip(search_input(json!({ "query": "\u{1F600}".repeat(100) })));
    rejects::<OrchestrationV2SearchThreadInput>(search_input(
        json!({ "query": "\u{1F600}".repeat(101) }),
    ));
    // 200 BMP characters that take 3 UTF-8 bytes each pass (bytes are not units).
    let _: OrchestrationV2SearchThreadInput =
        round_trip(search_input(json!({ "query": "\u{20AC}".repeat(200) })));

    assert!(OrchestrationV2SearchQuery::new("x".repeat(200)).is_ok());
    assert!(OrchestrationV2SearchQuery::new("x".repeat(201)).is_err());
    assert!(OrchestrationV2SearchQuery::new("\u{1F600}".repeat(101)).is_err());
}

#[test]
fn search_skills_enforce_name_label_and_array_limits() {
    let skill = |name: &str| json!({ "name": name });
    let _: OrchestrationV2SearchThreadInput = round_trip(search_input(json!({
        "skills": [skill("a"), { "name": "b", "displayName": "B" }],
    })));
    // name and displayName: at most 200 units.
    let _: OrchestrationV2SearchThreadInput = round_trip(search_input(json!({
        "skills": [{ "name": "n".repeat(200), "displayName": "d".repeat(200) }],
    })));
    rejects::<OrchestrationV2SearchThreadInput>(search_input(
        json!({ "skills": [skill(&"n".repeat(201))] }),
    ));
    rejects::<OrchestrationV2SearchThreadInput>(search_input(json!({
        "skills": [{ "name": "n", "displayName": "d".repeat(201) }],
    })));
    rejects::<OrchestrationV2SearchThreadInput>(search_input(json!({
        "skills": [skill(&"\u{1F600}".repeat(101))],
    })));
    // displayName is optional but not nullable.
    rejects::<OrchestrationV2SearchThreadInput>(search_input(json!({
        "skills": [{ "name": "n", "displayName": null }],
    })));
    // At most 1,000 skills.
    let many = |count: usize| json!((0..count).map(|_| skill("s")).collect::<Vec<_>>());
    let _: OrchestrationV2SearchThreadInput =
        round_trip(search_input(json!({ "skills": many(1000) })));
    rejects::<OrchestrationV2SearchThreadInput>(search_input(json!({ "skills": many(1001) })));
    // optionalKey: null is not absent.
    rejects::<OrchestrationV2SearchThreadInput>(search_input(json!({ "skills": null })));
}

#[test]
fn search_input_index_offset_and_start() {
    let _: OrchestrationV2SearchThreadInput = round_trip(search_input(json!({
        "index": 0, "offset": -3,
        "start": { "entryId": "entry-1", "occurrence": 2 },
    })));
    rejects::<OrchestrationV2SearchThreadInput>(search_input(json!({ "index": -1 })));
    rejects::<OrchestrationV2SearchThreadInput>(search_input(json!({ "offset": 1.5 })));
    rejects::<OrchestrationV2SearchThreadInput>(search_input(
        json!({ "start": { "entryId": "entry-1" } }),
    ));
    rejects::<OrchestrationV2SearchThreadInput>(search_input(json!({
        "start": { "entryId": " ", "occurrence": 0 },
    })));
}

#[test]
fn search_result_navigation_is_capped_at_17_entries() {
    let result = |navigation: Value| {
        json!({
            "snapshotSequence": 4, "totalMatches": 3, "activeIndex": 1, "match": null,
            "navigation": navigation,
        })
    };
    let entry = |index: u64| json!({ "entryId": format!("entry-{index}"), "runId": null, "startIndex": index, "count": 1 });
    let entries = |count: u64| json!((0..count).map(entry).collect::<Vec<_>>());

    // Without the optional keys.
    let bare: OrchestrationV2SearchThreadResult = round_trip(json!({
        "snapshotSequence": 4, "totalMatches": 0, "activeIndex": 0, "match": null,
    }));
    assert!(bare.r#match.is_none() && bare.complete.is_none() && bare.navigation.is_none());

    let _: OrchestrationV2SearchThreadResult = round_trip(merge(
        result(entries(17)),
        json!({
            "complete": true,
            "match": { "entryId": "entry-1", "runId": "run-1", "occurrence": 0 },
        }),
    ));
    rejects::<OrchestrationV2SearchThreadResult>(result(entries(18)));
    let _: OrchestrationV2SearchThreadResult = round_trip(result(entries(0)));
    rejects::<OrchestrationV2SearchThreadResult>(result(json!(null)));
    rejects::<OrchestrationV2SearchThreadResult>(json!({
        "snapshotSequence": 4, "totalMatches": -1, "activeIndex": 0, "match": null,
    }));
}

// ---------------------------------------------------------------------------
// Provider replay transcripts
// ---------------------------------------------------------------------------

#[test]
fn replay_entries_cover_every_member() {
    let entries = [
        json!({ "type": "expect_outbound", "frame": { "method": "initialize" } }),
        json!({ "type": "expect_outbound", "label": "init", "frame": null }),
        json!({ "type": "emit_inbound", "frame": [1, 2] }),
        json!({ "type": "emit_inbound", "label": "reply", "frame": "text", "afterMs": 25 }),
        json!({ "type": "runtime_exit", "status": "success" }),
        json!({ "type": "runtime_exit", "status": "error", "error": { "message": "bad" } }),
        json!({ "type": "runtime_exit", "status": "cancelled" }),
    ];
    for entry in entries {
        let _: ProviderReplayEntry = round_trip(entry);
    }
    // `Schema.optional(Schema.Unknown)`: an explicit null is a value, not absence.
    let exit: ProviderReplayEntry =
        round_trip(json!({ "type": "runtime_exit", "status": "error", "error": null }));
    let ProviderReplayEntry::RuntimeExit { error, .. } = exit else {
        panic!("expected runtime_exit");
    };
    assert_eq!(error, Some(Value::Null));

    rejects::<ProviderReplayEntry>(json!({ "type": "runtime_exit", "status": "crashed" }));
    rejects::<ProviderReplayEntry>(json!({ "type": "other", "frame": 1 }));
    rejects::<ProviderReplayEntry>(json!({ "type": "emit_inbound", "frame": 1, "afterMs": -1 }));
    rejects::<ProviderReplayEntry>(json!({ "type": "emit_inbound", "label": " ", "frame": 1 }));
}

#[test]
fn replay_transcript_and_ndjson_records() {
    let transcript = json!({
        "provider": "codex", "protocol": "app-server", "version": "1", "scenario": "hello",
        "metadata": { "z": 1, "a": { "nested": true }, "m": null },
        "entries": [{ "type": "runtime_exit", "status": "success" }],
    });
    let decoded: ProviderReplayTranscript = round_trip(transcript.clone());
    // Record keys keep their order.
    let keys: Vec<_> = decoded.metadata.as_ref().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["z", "a", "m"]);
    assert_eq!(
        serde_json::to_string(&decoded.metadata).unwrap(),
        r#"{"z":1,"a":{"nested":true},"m":null}"#
    );
    let _: ProviderReplayTranscript = round_trip(without(transcript.clone(), "metadata"));
    rejects::<ProviderReplayTranscript>(without(transcript.clone(), "entries"));
    rejects::<ProviderReplayTranscript>(merge(transcript, json!({ "scenario": "  " })));

    let header = json!({
        "type": "transcript_start", "provider": "codex", "protocol": "app-server",
        "version": "1", "scenario": "hello",
    });
    let record: ProviderReplayNdjsonRecord = round_trip(header.clone());
    assert!(matches!(record, ProviderReplayNdjsonRecord::Header(_)));
    let _: ProviderReplayTranscriptHeader =
        round_trip(merge(header.clone(), json!({ "metadata": {} })));
    let record: ProviderReplayNdjsonRecord =
        round_trip(json!({ "type": "emit_inbound", "frame": { "x": 1 } }));
    assert!(matches!(record, ProviderReplayNdjsonRecord::Entry(_)));
    rejects::<ProviderReplayNdjsonRecord>(merge(
        header.clone(),
        json!({ "type": "transcript_end" }),
    ));
    rejects::<ProviderReplayTranscriptHeader>(merge(header, json!({ "type": "emit_inbound" })));
}

// ---------------------------------------------------------------------------
// Tagged unions
// ---------------------------------------------------------------------------

#[test]
fn turn_item_union_known_tags_match_serialized_tags() {
    // One sample per member of the forward-compatible turn item union, as the
    // thread stream and history page decode them.
    let base = |kind: &str, extra: Value| {
        merge(
            json!({
                "id": format!("item-{kind}"), "type": kind, "threadId": "thread-1", "runId": null,
                "nodeId": null, "providerThreadId": null, "providerTurnId": null,
                "nativeItemRef": null, "parentItemId": null, "ordinal": 1, "status": "completed",
                "title": null, "startedAt": null, "completedAt": null, "updatedAt": NOW,
            }),
            extra,
        )
    };
    let samples = [
        base(
            "notification",
            json!({
                "outcome": "failed", "summary": "s", "detail": "d",
                "source": { "type": "subagent", "subagentId": "node-1", "threadId": "thread-2" },
            }),
        ),
        merge(
            base(
                "user_message",
                json!({
                    "messageId": "message-1", "inputIntent": "turn_start", "text": "t", "attachments": [],
                }),
            ),
            creation(),
        ),
        base(
            "assistant_message",
            json!({ "messageId": "message-1", "text": "t", "streaming": false }),
        ),
        base("reasoning", json!({ "text": "t", "streaming": false })),
        base(
            "proposed_plan",
            json!({ "planId": "plan-1", "markdown": "m", "streaming": false }),
        ),
        base("todo_list", json!({ "planId": "plan-1", "steps": [] })),
        base(
            "user_input_request",
            json!({ "requestId": "request-1", "questions": [] }),
        ),
        base("file_change", json!({ "fileName": "a.rs" })),
        base("command_execution", json!({ "input": "ls" })),
        base("file_search", json!({})),
        base("web_search", json!({})),
        base(
            "approval_request",
            json!({ "requestId": "request-1", "requestKind": "command" }),
        ),
        base(
            "checkpoint",
            json!({ "checkpointId": "checkpoint-1", "scopeId": "scope-1", "files": [] }),
        ),
        base("run_interrupt_request", json!({ "message": "m" })),
        base("run_interrupt_result", json!({ "message": "m" })),
        base("system_notice", json!({ "message": "m" })),
        base(
            "error",
            json!({ "failure": { "class": "unknown", "message": "boom", "code": null, "retryable": null } }),
        ),
        base("compaction", json!({ "driver": null })),
        base(
            "handoff",
            json!({
                "contextHandoffId": "handoff-1", "fromProviderThreadIds": [], "toProviderThreadId": "pt-1",
                "fromProviderInstanceIds": [], "toProviderInstanceId": "codex",
                "strategy": "manual_context",
            }),
        ),
        base(
            "fork",
            json!({
                "source": { "type": "node", "nodeId": "node-1" }, "targetThreadId": "thread-2",
            }),
        ),
        base(
            "thread_created",
            json!({
                "targetThreadId": "thread-2", "targetRunId": null, "targetProviderInstanceId": "codex",
                "targetModel": "gpt-5.4",
            }),
        ),
        base(
            "secret_request",
            json!({ "label": "K", "reason": "r", "secretStatus": "pending" }),
        ),
        base(
            "subagent",
            json!({
                "subagentId": "node-1", "origin": "app_owned", "driver": "codex",
                "providerInstanceId": "codex", "childThreadId": null, "prompt": "p", "result": null,
            }),
        ),
        base("dynamic_tool", json!({ "toolName": null, "input": {} })),
    ];
    let items: Vec<OrchestrationV2TurnItem> = samples
        .iter()
        .map(|sample| {
            serde_json::from_value(sample.clone())
                .unwrap_or_else(|error| panic!("{}: {error}", sample["type"]))
        })
        .collect();
    assert_known_tags(&items);
}
