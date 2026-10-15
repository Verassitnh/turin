//! Round-trip tests for the first half of `orchestrationV2.ts` (upstream lines
//! 1-1950): null vs absent, decoding defaults, forward-compatible unions and
//! the check bounds that matter on decode. Uses the public API only.

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use turin_contracts::base_schemas::{ForwardCompatible, TaggedUnion, assert_known_tags};
use turin_contracts::orchestration_v2::*;

const NOW: &str = "2026-04-20T00:00:00.000Z";

fn decode<T: DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("value should decode")
}

fn rejects<T: DeserializeOwned>(value: Value) {
    assert!(
        serde_json::from_value::<T>(value.clone()).is_err(),
        "expected decode failure for {value}"
    );
}

fn encode<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("value should encode")
}

/// Decodes `value`, re-encodes it, and returns the encoding.
fn round_trip<T: DeserializeOwned + Serialize>(value: Value) -> Value {
    encode(&decode::<T>(value))
}

fn merge(mut base: Value, extra: Value) -> Value {
    base.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    base
}

fn without(mut value: Value, keys: &[&str]) -> Value {
    for key in keys {
        value.as_object_mut().unwrap().remove(*key);
    }
    value
}

fn app_thread_json() -> Value {
    json!({
        "createdBy": "user",
        "creationSource": "web",
        "id": "thread-1",
        "projectId": "project-1",
        "title": "Thread",
        "providerInstanceId": "codex",
        "modelSelection": { "instanceId": "codex", "model": "gpt-5-codex" },
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "branch": null,
        "worktreePath": null,
        "activeProviderThreadId": null,
        "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": "thread-1" },
        "forkedFrom": null,
        "createdAt": NOW,
        "updatedAt": NOW,
        "archivedAt": null,
        "deletedAt": null,
    })
}

fn run_json() -> Value {
    json!({
        "id": "run-1",
        "threadId": "thread-1",
        "ordinal": 1,
        "providerInstanceId": "codex",
        "modelSelection": { "instanceId": "codex", "model": "gpt-5.4" },
        "providerThreadId": null,
        "userMessageId": "message-1",
        "rootNodeId": null,
        "activeAttemptId": null,
        "status": "queued",
        "requestedAt": NOW,
        "startedAt": null,
        "completedAt": null,
        "checkpointId": null,
        "contextHandoffId": null,
    })
}

fn provider_thread_json() -> Value {
    json!({
        "id": "provider-thread-1",
        "driver": "claude",
        "providerInstanceId": "claudeAgent",
        "providerSessionId": null,
        "appThreadId": "thread-1",
        "ownerNodeId": null,
        "nativeThreadRef": null,
        "nativeConversationHeadRef": null,
        "status": "idle",
        "firstRunOrdinal": null,
        "lastRunOrdinal": null,
        "handoffIds": [],
        "forkedFrom": null,
        "createdAt": NOW,
        "updatedAt": NOW,
    })
}

fn turn_item_json(kind: &str, extra: Value) -> Value {
    merge(
        json!({
            "id": format!("turn-item-{kind}"),
            "type": kind,
            "threadId": "thread-1",
            "runId": null,
            "nodeId": null,
            "providerThreadId": null,
            "providerTurnId": null,
            "nativeItemRef": null,
            "parentItemId": null,
            "ordinal": 1,
            "status": "completed",
            "title": null,
            "startedAt": null,
            "completedAt": null,
            "updatedAt": NOW,
        }),
        extra,
    )
}

// ---------------------------------------------------------------------------
// Optional vs NullOr: absent and null stay distinct
// ---------------------------------------------------------------------------

const APP_THREAD_TRISTATE_KEYS: [&str; 7] = [
    "unsettledAt",
    "snoozedUntil",
    "snoozedAt",
    "lastSnoozeWakeAt",
    "pinnedAt",
    "autoSettleDisabledAt",
    "pinOrderKey",
];

#[test]
fn app_thread_optional_null_or_fields_distinguish_absent_from_null() {
    // Absent stays absent.
    let encoded = round_trip::<OrchestrationV2AppThread>(app_thread_json());
    for key in APP_THREAD_TRISTATE_KEYS
        .into_iter()
        .chain([
            "activeOrderKey",
            "limitRecovery",
            "titleRegeneration",
            "rollbackFailure",
        ])
        .chain(["linkedPullRequest", "branchPullRequest"])
    {
        assert!(
            encoded.get(key).is_none(),
            "{key} must stay absent, got {encoded}"
        );
    }
    for key in ["pullRequests", "historyOrigin", "rollbackRequestId"] {
        assert!(encoded.get(key).is_none(), "{key} must stay absent");
    }

    // Explicit null stays null (not collapsed to absent).
    let mut nulls = app_thread_json();
    for key in APP_THREAD_TRISTATE_KEYS
        .into_iter()
        .chain([
            "activeOrderKey",
            "limitRecovery",
            "titleRegeneration",
            "rollbackFailure",
        ])
        .chain(["linkedPullRequest", "branchPullRequest"])
    {
        nulls[key] = Value::Null;
    }
    let thread: OrchestrationV2AppThread = decode(nulls.clone());
    assert_eq!(thread.unsettled_at, Some(None));
    assert_eq!(thread.limit_recovery, Some(None));
    assert_eq!(thread.linked_pull_request, Some(None));
    let encoded = encode(&thread);
    for key in [
        "unsettledAt",
        "pinOrderKey",
        "limitRecovery",
        "linkedPullRequest",
    ] {
        assert_eq!(
            encoded.get(key),
            Some(&Value::Null),
            "{key} must encode null"
        );
    }

    // A value survives.
    let valued = merge(
        app_thread_json(),
        json!({
            "unsettledAt": "2026-04-21T00:00:00.000Z",
            "pinOrderKey": "a0",
            "limitRecovery": {
                "runId": "run-1",
                "resetAt": "2026-09-20T21:00:00.000Z",
                "autoResume": true,
            },
            "titleRegeneration": { "requestId": "command-1", "startedAt": NOW },
            "rollbackFailure": { "requestId": "command-2", "message": "boom" },
        }),
    );
    let encoded = round_trip::<OrchestrationV2AppThread>(valued.clone());
    assert_eq!(encoded["unsettledAt"], valued["unsettledAt"]);
    assert_eq!(encoded["limitRecovery"], valued["limitRecovery"]);
    assert_eq!(encoded["rollbackFailure"], valued["rollbackFailure"]);
}

#[test]
fn app_thread_decoding_defaults_encode_their_keys() {
    // settledOverride / settledAt / lastVisitedAt default to null and are
    // always encoded (no skip_serializing_if).
    let thread: OrchestrationV2AppThread = decode(app_thread_json());
    assert_eq!(thread.settled_override, None);
    assert_eq!(thread.settled_at, None);
    assert_eq!(thread.last_visited_at, None);
    let encoded = encode(&thread);
    for key in ["settledOverride", "settledAt", "lastVisitedAt"] {
        assert_eq!(encoded.get(key), Some(&Value::Null), "{key}");
    }
    let settled = round_trip::<OrchestrationV2AppThread>(merge(
        app_thread_json(),
        json!({ "settledOverride": "settled", "settledAt": NOW }),
    ));
    assert_eq!(settled["settledOverride"], json!("settled"));
    rejects::<OrchestrationV2AppThread>(merge(
        app_thread_json(),
        json!({ "settledOverride": "snoozed" }),
    ));
}

#[test]
fn app_thread_required_fields_and_literals_are_checked() {
    rejects::<OrchestrationV2AppThread>(merge(app_thread_json(), json!({ "title": "   " })));
    rejects::<OrchestrationV2AppThread>(merge(
        app_thread_json(),
        json!({ "creationSource": "cli" }),
    ));
    rejects::<OrchestrationV2AppThread>(merge(app_thread_json(), json!({ "historyOrigin": "v2" })));
    let encoded = round_trip::<OrchestrationV2AppThread>(merge(
        app_thread_json(),
        json!({
            "historyOrigin": "v1_import",
            "forkedFrom": { "type": "provider_thread", "providerThreadId": "pt-1" },
        }),
    ));
    assert_eq!(encoded["historyOrigin"], json!("v1_import"));
    // Optional member key stays absent.
    assert!(encoded["forkedFrom"].get("providerTurnId").is_none());
    let node = round_trip::<OrchestrationV2AppThread>(merge(
        app_thread_json(),
        json!({ "forkedFrom": { "type": "node", "nodeId": "node-1" } }),
    ));
    assert_eq!(
        node["forkedFrom"],
        json!({ "type": "node", "nodeId": "node-1" })
    );
    rejects::<OrchestrationV2AppThread>(merge(
        app_thread_json(),
        json!({ "forkedFrom": { "type": "run", "threadId": "t" } }),
    ));
}

#[test]
fn dates_normalize_to_millisecond_utc_on_encode() {
    let encoded = round_trip::<OrchestrationV2AppThread>(merge(
        app_thread_json(),
        json!({ "createdAt": "2026-04-20T02:00:00+02:00", "updatedAt": "2026-04-20T00:00:00Z" }),
    ));
    assert_eq!(encoded["createdAt"], json!("2026-04-20T00:00:00.000Z"));
    assert_eq!(encoded["updatedAt"], json!("2026-04-20T00:00:00.000Z"));
    rejects::<OrchestrationV2AppThread>(merge(
        app_thread_json(),
        json!({ "createdAt": "yesterday" }),
    ));
}

// ---------------------------------------------------------------------------
// Provider capabilities
// ---------------------------------------------------------------------------

fn legacy_capabilities_json() -> Value {
    json!({
        "sessions": {
            "supportsMultipleProviderThreadsPerSession": false,
            "supportsModelSwitchInSession": false,
            "supportsProviderSwitchingViaHandoff": true,
            "supportsRuntimeModeSwitchInSession": false,
            "pendingRequestsSurviveRestart": false,
        },
        "threads": {
            "canCreateEmptyThread": true,
            "canReadThreadSnapshot": false,
            "canRollbackThread": true,
            "canForkThread": false,
            "canForkFromTurn": false,
            "canForkFromSubagentThread": false,
            "exposesNativeThreadId": true,
        },
        "turns": {
            "exposesNativeTurnId": false,
            "emitsTurnStarted": true,
            "emitsTurnCompleted": true,
            "supportsInterrupt": true,
            "supportsActiveSteering": false,
            "supportsSteeringByInterruptRestart": true,
            "supportsQueuedMessages": true,
            "terminalStatusQuality": "strong",
        },
        "streaming": {
            "streamsAssistantText": true,
            "streamsReasoning": true,
            "streamsToolOutput": true,
            "streamsPlanText": false,
            "emitsMessageCompleted": true,
        },
        "tools": {
            "exposesToolItemIds": true,
            "emitsToolStarted": true,
            "emitsToolCompleted": true,
            "emitsToolOutput": true,
            "supportsMcpTools": false,
            "supportsDynamicToolCallbacks": false,
        },
        "approvals": {
            "supportsCommandApproval": true,
            "supportsFileReadApproval": true,
            "supportsFileChangeApproval": true,
            "supportsApplyPatchApproval": false,
            "approvalsHaveNativeRequestIds": false,
            "approvalCallbacksAreLiveOnly": true,
            "approvalsCanOriginateFromSubagents": false,
        },
        "planning": {
            "emitsPlanUpdated": true,
            "emitsTodoList": true,
            "emitsProposedPlan": false,
            "supportsStructuredQuestions": true,
            "planDeltasHaveItemIds": false,
        },
        "subagents": {
            "supportsSubagents": false,
            "exposesSubagentThreadIds": false,
            "emitsSubagentLifecycle": false,
            "canWaitForSubagents": false,
            "canCloseSubagents": false,
            "canForkSubagentThread": false,
        },
        "context": {
            "acceptsSystemContext": false,
            "acceptsDeveloperContext": false,
            "acceptsSyntheticUserContext": true,
            "canGenerateSummaries": true,
            "canConsumeHandoffSummaries": true,
            "supportsDeltaHandoff": true,
            "supportsFullThreadHandoff": true,
            "maxRecommendedHandoffChars": null,
        },
        "checkpointing": {
            "appCanCheckpointFilesystem": true,
            "supportsNestedCheckpointScopes": true,
            "providerCanRollbackConversation": true,
            "providerRollbackReturnsSnapshot": true,
            "providerCanReadConversationSnapshot": false,
        },
        "identity": {
            "nativeThreadIds": "strong",
            "nativeTurnIds": "weak",
            "nativeItemIds": "weak",
            "nativeRequestIds": "weak",
        },
    })
}

#[test]
fn capabilities_default_runtime_policy_and_encode_it() {
    let decoded: OrchestrationV2ProviderCapabilities = decode(legacy_capabilities_json());
    assert_eq!(
        decoded.runtime_policy.enforcement,
        OrchestrationV2RuntimePolicyEnforcement::ClientBoundary
    );
    let encoded = encode(&decoded);
    assert_eq!(
        encoded["runtimePolicy"],
        json!({ "enforcement": "client-boundary" })
    );
    // The optional steering flag stays absent; max chars encodes null.
    assert!(
        encoded["turns"]
            .get("activeSteeringInterruptsTools")
            .is_none()
    );
    assert_eq!(
        encoded["context"]["maxRecommendedHandoffChars"],
        Value::Null
    );

    let native = round_trip::<OrchestrationV2ProviderCapabilities>(merge(
        legacy_capabilities_json(),
        json!({ "runtimePolicy": { "enforcement": "native" } }),
    ));
    assert_eq!(native["runtimePolicy"]["enforcement"], json!("native"));

    rejects::<OrchestrationV2ProviderCapabilities>(merge(
        legacy_capabilities_json(),
        json!({ "runtimePolicy": { "enforcement": "sandbox" } }),
    ));
    let mut zero_chars = legacy_capabilities_json();
    zero_chars["context"]["maxRecommendedHandoffChars"] = json!(0);
    rejects::<OrchestrationV2ProviderCapabilities>(zero_chars);
}

// ---------------------------------------------------------------------------
// Runs, provider threads, turns, handoffs
// ---------------------------------------------------------------------------

#[test]
fn run_optional_null_or_queue_position_and_positive_ints() {
    let absent = round_trip::<OrchestrationV2Run>(run_json());
    assert!(absent.get("queuePosition").is_none());
    for key in [
        "queueHeld",
        "restartContinuationOfRunId",
        "delegatedTaskId",
        "workStartedAt",
        "restartCancelledBackgroundWork",
        "sourcePlanRef",
        "delegatedCompletion",
        "workspacePreparation",
    ] {
        assert!(absent.get(key).is_none(), "{key}");
    }
    let null =
        round_trip::<OrchestrationV2Run>(merge(run_json(), json!({ "queuePosition": null })));
    assert_eq!(null["queuePosition"], Value::Null);
    let some = round_trip::<OrchestrationV2Run>(merge(run_json(), json!({ "queuePosition": 3 })));
    assert_eq!(some["queuePosition"], json!(3));
    rejects::<OrchestrationV2Run>(merge(run_json(), json!({ "queuePosition": 0 })));
    rejects::<OrchestrationV2Run>(merge(run_json(), json!({ "ordinal": 0 })));
    rejects::<OrchestrationV2Run>(merge(run_json(), json!({ "ordinal": 1.5 })));
    rejects::<OrchestrationV2Run>(merge(run_json(), json!({ "status": "paused" })));
}

#[test]
fn run_workspace_preparation_union_branches() {
    for (strategy, expect_absent) in [
        (json!({ "type": "root" }), vec!["branch"]),
        (
            json!({ "type": "existing_worktree", "worktreePath": "/tmp/wt" }),
            vec!["branch"],
        ),
        (
            json!({ "type": "worktree", "baseRef": "main", "branch": "feature", "startFromOrigin": false }),
            vec![],
        ),
    ] {
        let encoded = round_trip::<OrchestrationV2Run>(merge(
            run_json(),
            json!({ "workspacePreparation": strategy.clone() }),
        ));
        assert_eq!(encoded["workspacePreparation"], strategy);
        for key in expect_absent {
            assert!(encoded["workspacePreparation"].get(key).is_none());
        }
    }
    rejects::<OrchestrationV2Run>(merge(
        run_json(),
        json!({ "workspacePreparation": { "type": "worktree" } }),
    ));
}

#[test]
fn provider_thread_defaults_decode_and_always_encode() {
    let thread: OrchestrationV2ProviderThread = decode(provider_thread_json());
    assert!(thread.pending_background_tasks.is_empty());
    assert!(thread.context_usage.is_none());
    assert!(thread.native_metadata.is_none());
    assert!(thread.goal.is_none());
    let encoded = encode(&thread);
    assert_eq!(encoded["pendingBackgroundTasks"], json!([]));
    assert_eq!(encoded["contextUsage"], Value::Null);
    assert_eq!(encoded["nativeMetadata"], Value::Null);
    assert_eq!(encoded["goal"], Value::Null);

    // Explicit nulls decode the same way.
    let nulls: OrchestrationV2ProviderThread = decode(merge(
        provider_thread_json(),
        json!({ "contextUsage": null, "nativeMetadata": null, "goal": null }),
    ));
    assert_eq!(nulls, thread);

    let full = round_trip::<OrchestrationV2ProviderThread>(merge(
        provider_thread_json(),
        json!({
            "contextUsage": { "usedTokens": 10, "maxTokens": 100 },
            "nativeMetadata": { "title": null, "itemIdentityVersion": 2 },
            "goal": { "objective": "Ship", "status": "usage_limited", "tokenBudget": null },
        }),
    ));
    assert_eq!(full["contextUsage"]["usedTokens"], json!(10));
    // Inside native metadata and the goal, optional(NullOr) still keeps null.
    assert_eq!(full["nativeMetadata"]["title"], Value::Null);
    assert!(full["nativeMetadata"].get("updatedAt").is_none());
    assert_eq!(full["nativeMetadata"]["itemIdentityVersion"], json!(2));
    assert_eq!(full["goal"]["tokenBudget"], Value::Null);
    assert!(full["goal"].get("tokensUsed").is_none());

    rejects::<OrchestrationV2ProviderThread>(merge(
        provider_thread_json(),
        json!({ "nativeMetadata": { "itemIdentityVersion": 3 } }),
    ));
    rejects::<OrchestrationV2ProviderThread>(merge(
        provider_thread_json(),
        json!({ "goal": { "objective": "Ship", "status": "done" } }),
    ));
    rejects::<OrchestrationV2ProviderThread>(merge(
        provider_thread_json(),
        json!({ "firstRunOrdinal": 0 }),
    ));
}

#[test]
fn pending_background_task_kinds_and_decode_only_fallback() {
    let thread: OrchestrationV2ProviderThread = decode(merge(
        provider_thread_json(),
        json!({
            "pendingBackgroundTasks": [
                { "taskId": "bg-1", "description": "Background sleep", "taskType": "local_bash" },
                { "taskId": "bg-2" },
                { "taskId": "bg-3", "kind": "workflow", "schedule": "0 3 * * *" },
                { "taskId": "bg-4", "kind": "command" },
                { "taskId": "bg-5", "kind": "subagent", "childThreadId": "thread-child" },
                { "taskId": "bg-6", "kind": "monitor" },
                { "taskId": "bg-7", "kind": "background_task" },
            ],
        }),
    ));
    assert_eq!(
        encode(&thread)["pendingBackgroundTasks"],
        json!([
            { "taskId": "bg-1", "kind": "background_task", "description": "Background sleep" },
            { "taskId": "bg-2", "kind": "background_task" },
            { "taskId": "bg-3", "kind": "background_task" },
            { "taskId": "bg-4", "kind": "command" },
            { "taskId": "bg-5", "kind": "subagent", "childThreadId": "thread-child" },
            { "taskId": "bg-6", "kind": "monitor" },
            { "taskId": "bg-7", "kind": "background_task" },
        ])
    );
    // A known kind must decode in full; a non-string kind never falls back.
    for bad in [
        json!({ "taskId": "", "kind": "command" }),
        json!({ "taskId": "bg-1", "kind": "subagent", "childThreadId": 7 }),
        json!({ "taskId": "bg-1", "kind": 7 }),
        json!({ "taskId": "bg-1", "kind": null }),
        json!({ "kind": "monitor" }),
        json!({ "taskId": "bg-1", "description": "  " }),
    ] {
        rejects::<OrchestrationV2ProviderThread>(merge(
            provider_thread_json(),
            json!({ "pendingBackgroundTasks": [bad] }),
        ));
    }
}

#[test]
fn provider_turn_token_usage_fields() {
    let turn = json!({
        "id": "provider-turn-1",
        "providerThreadId": "provider-thread-1",
        "nodeId": "node-1",
        "runAttemptId": null,
        "nativeTurnRef": null,
        "ordinal": 1,
        "status": "running",
        "startedAt": null,
        "completedAt": null,
    });
    let encoded = round_trip::<OrchestrationV2ProviderTurn>(turn.clone());
    assert!(encoded.get("tokenUsage").is_none());
    assert!(encoded.get("turnTokenUsage").is_none());

    let with_usage = merge(
        turn.clone(),
        json!({
            "tokenUsage": { "usedTokens": 5, "maxTokens": null, "promptCacheTtlMs": 300000, "updatedAt": NOW },
            "turnTokenUsage": {
                "usageScope": "main_agent",
                "hasSubagents": false,
                "usageStatus": "complete",
                "inputTokens": 3,
                "outputTokens": 2,
            },
        }),
    );
    let encoded = round_trip::<OrchestrationV2ProviderTurn>(with_usage.clone());
    assert_eq!(encoded["tokenUsage"]["maxTokens"], Value::Null);
    assert_eq!(encoded["turnTokenUsage"]["usageStatus"], json!("complete"));
    rejects::<OrchestrationV2ProviderTurn>(merge(
        turn.clone(),
        json!({ "tokenUsage": { "usedTokens": 5, "promptCacheTtlMs": 0, "updatedAt": NOW } }),
    ));
    rejects::<OrchestrationV2ProviderTurn>(merge(turn, json!({ "ordinal": 0 })));
}

#[test]
fn context_handoff_tri_state_and_true_literal() {
    let handoff = json!({
        "id": "handoff-1",
        "threadId": "thread-1",
        "targetRunId": "run-2",
        "fromProviderThreadIds": ["pt-1"],
        "toProviderThreadId": "pt-2",
        "coveredRunOrdinals": { "from": 1, "to": 2 },
        "strategy": "fork_delta_summary",
        "status": "ready",
        "summaryMessageId": null,
        "summaryText": "summary",
        "createdByProviderInstanceId": null,
        "createdAt": NOW,
        "updatedAt": NOW,
    });
    let absent = round_trip::<OrchestrationV2ContextHandoff>(handoff.clone());
    for key in ["transferId", "history", "delivery", "detailInTurnItem"] {
        assert!(absent.get(key).is_none(), "{key}");
    }
    let null = round_trip::<OrchestrationV2ContextHandoff>(merge(
        handoff.clone(),
        json!({ "transferId": null }),
    ));
    assert_eq!(null["transferId"], Value::Null);
    let full = round_trip::<OrchestrationV2ContextHandoff>(merge(
        handoff.clone(),
        json!({
            "transferId": "transfer-1",
            "detailInTurnItem": true,
            "history": {
                "messages": [{
                    "role": "user", "text": "hi", "threadId": "thread-1", "runId": null,
                    "itemId": "item-1", "providerThreadId": null, "status": "completed", "kind": "message",
                }],
                "coverage": "full",
                "omittedItems": 0,
                "omittedItemIds": ["item-2"],
            },
            "delivery": { "nativeThreadId": "native", "status": "inline", "itemIds": [] },
        }),
    ));
    assert_eq!(full["detailInTurnItem"], json!(true));
    assert!(full["delivery"].get("omittedItemIds").is_none());
    rejects::<OrchestrationV2ContextHandoff>(merge(
        handoff.clone(),
        json!({ "detailInTurnItem": false }),
    ));
    rejects::<OrchestrationV2ContextHandoff>(merge(
        handoff.clone(),
        json!({ "coveredRunOrdinals": { "from": 0, "to": 1 } }),
    ));
    rejects::<OrchestrationV2ContextHandoff>(merge(handoff, json!({ "strategy": "everything" })));
}

#[test]
fn context_transfer_resolution_branches() {
    let transfer = |resolution: Value| {
        json!({
            "id": "transfer-1",
            "type": "provider_handoff",
            "sourceThreadId": "thread-1",
            "targetThreadId": "thread-2",
            "sourcePoint": { "threadId": "thread-1" },
            "basePoint": null,
            "sourceProviderInstanceId": null,
            "targetProviderInstanceId": null,
            "targetRunId": null,
            "status": "resolved_portable",
            "resolution": resolution,
            "createdBy": "system",
            "error": null,
            "createdAt": NOW,
            "updatedAt": NOW,
            "consumedAt": null,
        })
    };
    for resolution in [
        json!({ "strategy": "native_fork", "providerThreadRef": { "driver": "codex", "nativeId": null, "strength": "none" } }),
        json!({ "strategy": "portable_context", "contextHandoffId": "h" }),
        json!({ "strategy": "delta_context", "contextHandoffId": "h" }),
        json!({ "strategy": "fork_delta_context", "contextHandoffId": "h" }),
        json!({ "strategy": "checkpoint_context", "contextHandoffId": "h" }),
        Value::Null,
    ] {
        let encoded = round_trip::<OrchestrationV2ContextTransfer>(transfer(resolution.clone()));
        assert_eq!(encoded["resolution"], resolution);
    }
    rejects::<OrchestrationV2ContextTransfer>(transfer(json!({ "strategy": "telepathy" })));
    rejects::<OrchestrationV2ContextTransfer>(transfer(json!({ "strategy": "native_fork" })));
}

#[test]
fn provider_ref_optional_fields() {
    let encoded = round_trip::<OrchestrationV2ProviderRef>(
        json!({ "driver": "codex", "nativeId": null, "strength": "weak" }),
    );
    assert_eq!(encoded["nativeId"], Value::Null);
    assert!(encoded.get("fingerprint").is_none());
    assert!(encoded.get("ordinal").is_none());
    let encoded = round_trip::<OrchestrationV2ProviderRef>(
        json!({ "driver": "codex", "nativeId": "n", "strength": "strong", "fingerprint": "f", "ordinal": 0 }),
    );
    assert_eq!(encoded["ordinal"], json!(0));
    rejects::<OrchestrationV2ProviderRef>(
        json!({ "driver": "codex", "nativeId": " ", "strength": "strong" }),
    );
    rejects::<OrchestrationV2ProviderRef>(
        json!({ "driver": "codex", "nativeId": "n", "strength": "strong", "ordinal": -1 }),
    );
}

// ---------------------------------------------------------------------------
// Workflow subagents
// ---------------------------------------------------------------------------

#[test]
fn workflow_prompt_preserves_whitespace_but_rejects_empty() {
    let agent = |prompt: &str| {
        json!({
            "phases": [],
            "agents": [{ "index": 1, "label": "Member", "state": "completed", "prompt": prompt, "result": prompt }],
        })
    };
    for preview in ["  indented prompt\n", "\n \t\n"] {
        let encoded = round_trip::<OrchestrationV2SubagentWorkflow>(agent(preview));
        assert_eq!(encoded["agents"][0]["prompt"], json!(preview));
        assert_eq!(encoded["agents"][0]["result"], json!(preview));
    }
    rejects::<OrchestrationV2SubagentWorkflow>(agent(""));
    let encoded = round_trip::<OrchestrationV2SubagentWorkflow>(agent("x"));
    for key in [
        "name",
        "runHandles",
        "totalTokens",
        "toolCalls",
        "durationMs",
    ] {
        assert!(encoded.get(key).is_none(), "{key}");
    }
    rejects::<OrchestrationV2SubagentWorkflow>(json!({
        "phases": [{ "index": -1, "title": "x" }],
        "agents": [],
    }));
}

// ---------------------------------------------------------------------------
// Failures: UTF-16 length bounds
// ---------------------------------------------------------------------------

fn error_item(message: &str, code: Value) -> Value {
    turn_item_json(
        "error",
        json!({
            "failure": { "class": "validation_error", "message": message, "code": code, "retryable": null },
        }),
    )
}

#[test]
fn provider_failure_bounds_count_utf16_code_units() {
    decode::<OrchestrationV2TurnItem>(error_item(&"x".repeat(4_096), Value::Null));
    rejects::<OrchestrationV2TurnItem>(error_item(&"x".repeat(4_097), Value::Null));
    // 2,048 astral chars are 4,096 UTF-16 units: allowed. 2,049 are 4,098: not.
    // (Both are far below 4,096 bytes' worth of `chars()`, so a char-count
    // check would wrongly accept the second.)
    decode::<OrchestrationV2TurnItem>(error_item(&"😀".repeat(2_048), Value::Null));
    rejects::<OrchestrationV2TurnItem>(error_item(&"😀".repeat(2_049), Value::Null));
    // A multi-byte BMP char is one unit but three bytes: 4,096 of them fit.
    decode::<OrchestrationV2TurnItem>(error_item(&"€".repeat(4_096), Value::Null));
    rejects::<OrchestrationV2TurnItem>(error_item(&"€".repeat(4_097), Value::Null));

    decode::<OrchestrationV2TurnItem>(error_item("m", json!("c".repeat(128))));
    rejects::<OrchestrationV2TurnItem>(error_item("m", json!("c".repeat(129))));
    decode::<OrchestrationV2TurnItem>(error_item("m", json!("€".repeat(128))));
    rejects::<OrchestrationV2TurnItem>(error_item("m", json!("😀".repeat(65))));
    rejects::<OrchestrationV2TurnItem>(error_item("   ", Value::Null));
    rejects::<OrchestrationV2TurnItem>(error_item("m", json!("  ")));
}

#[test]
fn provider_failure_reset_at_distinguishes_absent_from_null() {
    let failure = |extra: Value| {
        merge(
            json!({ "class": "usage_limit", "message": "limit", "code": null, "retryable": true }),
            extra,
        )
    };
    let absent = round_trip::<OrchestrationV2ProviderFailure>(failure(json!({})));
    assert!(absent.get("resetAt").is_none());
    let null = round_trip::<OrchestrationV2ProviderFailure>(failure(json!({ "resetAt": null })));
    assert_eq!(null["resetAt"], Value::Null);
    let value = round_trip::<OrchestrationV2ProviderFailure>(failure(
        json!({ "resetAt": "2026-09-20T21:00:00.000Z" }),
    ));
    // IsoDateTime is a plain string and passes through untouched.
    assert_eq!(value["resetAt"], json!("2026-09-20T21:00:00.000Z"));
    assert_eq!(absent["retryable"], json!(true));
}

// ---------------------------------------------------------------------------
// Limit recovery
// ---------------------------------------------------------------------------

#[test]
fn limit_recovery_update_needs_a_choice_and_keeps_false() {
    let identity = json!({ "runId": "run:limited", "resetAt": "2026-09-20T21:00:00.000Z" });
    rejects::<OrchestrationV2LimitRecoveryUpdate>(identity.clone());
    for choice in [
        json!({ "autoResume": true }),
        json!({ "autoResume": false }),
        json!({ "snooze": true }),
        json!({ "snooze": false }),
        json!({ "autoResume": true, "snooze": false }),
    ] {
        let wanted = merge(identity.clone(), choice);
        assert_eq!(
            round_trip::<OrchestrationV2LimitRecoveryUpdate>(wanted.clone()),
            wanted
        );
    }
    let recovery = round_trip::<OrchestrationV2LimitRecovery>(
        json!({ "runId": "r", "resetAt": "x", "autoResume": false }),
    );
    assert!(recovery.get("requestId").is_none());
    assert!(recovery.get("snooze").is_none());
}

#[test]
fn latest_provider_turn_picks_highest_ordinal_of_the_attempt() {
    let turn = |id: &str, attempt: Value, ordinal: u64| -> OrchestrationV2ProviderTurn {
        decode(json!({
            "id": id,
            "providerThreadId": "pt",
            "nodeId": "node",
            "runAttemptId": attempt,
            "nativeTurnRef": null,
            "ordinal": ordinal,
            "status": "completed",
            "startedAt": null,
            "completedAt": null,
        }))
    };
    let turns = vec![
        turn("first", json!("goal-attempt"), 3),
        turn("other", json!("other-attempt"), 9),
        turn("last", json!("goal-attempt"), 5),
        turn("subagent", Value::Null, 7),
    ];
    let attempt = decode(json!("goal-attempt"));
    let latest = latest_provider_turn_for_attempt(&turns, Some(&attempt)).unwrap();
    assert_eq!(latest.id.as_str(), "last");
    assert!(latest_provider_turn_for_attempt(&turns, None).is_none());
}

// ---------------------------------------------------------------------------
// Runtime requests, notification sources
// ---------------------------------------------------------------------------

#[test]
fn runtime_request_kinds_capabilities_and_optionals() {
    let request = |kind: &str, capability: Value| {
        json!({
            "id": "request-1",
            "nodeId": "node-1",
            "providerTurnId": null,
            "nativeRequestRef": null,
            "kind": kind,
            "status": "pending",
            "responseCapability": capability,
            "createdAt": NOW,
            "resolvedAt": null,
        })
    };
    for kind in [
        "command",
        "file-read",
        "file-change",
        "mcp-elicitation",
        "permission",
        "dynamic_tool_call",
        "user_input",
        "auth_refresh",
    ] {
        let encoded = round_trip::<OrchestrationV2RuntimeRequest>(request(
            kind,
            json!({ "type": "message" }),
        ));
        assert_eq!(encoded["kind"], json!(kind));
        assert!(encoded.get("decision").is_none());
        assert!(encoded.get("answers").is_none());
    }
    rejects::<OrchestrationV2RuntimeRequest>(request("filesystem", json!({ "type": "message" })));
    for capability in [
        json!({ "type": "live", "providerSessionId": "ps" }),
        json!({ "type": "message" }),
        json!({ "type": "not_resumable", "reason": "restarted" }),
    ] {
        assert_eq!(
            round_trip::<OrchestrationV2RuntimeRequest>(request("command", capability.clone()))["responseCapability"],
            capability
        );
    }
    rejects::<OrchestrationV2RuntimeRequest>(request("command", json!({ "type": "live" })));
}

fn notification_item(source: Value) -> Value {
    turn_item_json(
        "notification",
        json!({ "source": source, "outcome": "completed", "summary": "Background activity updated" }),
    )
}

fn notification_source(item: &OrchestrationV2TurnItem) -> &OrchestrationV2NotificationSource {
    match item {
        OrchestrationV2TurnItem::Notification { source, .. } => source,
        other => panic!("expected notification, got {other:?}"),
    }
}

#[test]
fn notification_system_source_round_trips() {
    // Regression: `system` used to fall into the unknown-kind arm.
    let item: OrchestrationV2TurnItem = decode(notification_item(json!({ "kind": "system" })));
    assert_eq!(
        *notification_source(&item),
        OrchestrationV2NotificationSource::System
    );
    assert_eq!(encode(&item)["source"], json!({ "kind": "system" }));
}

#[test]
fn notification_source_fallbacks_follow_member_order() {
    // A subagent source whose childThreadId is broken fails its own member,
    // but the plain `background_task` member after it still matches.
    let item: OrchestrationV2TurnItem = decode(notification_item(
        json!({ "kind": "background_task", "work": "subagent", "childThreadId": 7 }),
    ));
    assert_eq!(
        *notification_source(&item),
        OrchestrationV2NotificationSource::BackgroundTask
    );
    // The same defect under the plain `subagent` member is a real failure.
    rejects::<OrchestrationV2TurnItem>(notification_item(
        json!({ "kind": "subagent", "childThreadId": 7 }),
    ));
    rejects::<OrchestrationV2TurnItem>(notification_item(json!({ "kind": "delegated_task" })));
    // No kind or an unknown string kind is generic work; a non-string kind is not.
    for source in [
        json!({}),
        json!({ "kind": "workflow", "workflowId": "wf-1" }),
    ] {
        let item: OrchestrationV2TurnItem = decode(notification_item(source));
        assert_eq!(
            *notification_source(&item),
            OrchestrationV2NotificationSource::BackgroundTask
        );
    }
    rejects::<OrchestrationV2TurnItem>(notification_item(json!({ "kind": 3 })));
    rejects::<OrchestrationV2TurnItem>(notification_item(json!({ "kind": null })));
    rejects::<OrchestrationV2TurnItem>(notification_item(json!("command")));
}

#[test]
fn notification_sources_encode_in_the_pre_specific_kind_shapes() {
    for (source, wire) in [
        (
            json!({ "kind": "subagent", "childThreadId": "child" }),
            json!({ "kind": "background_task", "work": "subagent", "childThreadId": "child" }),
        ),
        (
            json!({ "kind": "subagent" }),
            json!({ "kind": "background_task", "work": "subagent" }),
        ),
        (
            json!({ "kind": "command" }),
            json!({ "kind": "background_command" }),
        ),
        (json!({ "kind": "monitor" }), json!({ "kind": "monitor" })),
        (
            json!({ "kind": "delegated_task", "taskIds": ["t"], "childThreadId": "c" }),
            json!({ "kind": "delegated_task", "taskIds": ["t"], "childThreadId": "c" }),
        ),
    ] {
        let item: OrchestrationV2TurnItem = decode(notification_item(source));
        let encoded = encode(&item);
        assert_eq!(encoded["source"], wire);
        // Current clients read the specific kind back.
        assert_eq!(decode::<OrchestrationV2TurnItem>(encoded), item);
    }
}

// ---------------------------------------------------------------------------
// Turn items
// ---------------------------------------------------------------------------

#[test]
fn turn_item_tool_activity_base_fields_round_trip() {
    let extra = json!({
        "text": "Thinking",
        "streaming": false,
        "toolNonExecutionKind": "denied",
        "toolSurface": "browser",
        "toolSource": { "key": "figma", "name": "Figma", "kind": "integration" },
        "toolIcon": { "_tag": "website", "pageUrl": "https://example.com" },
    });
    let encoded = round_trip::<OrchestrationV2TurnItem>(turn_item_json("reasoning", extra.clone()));
    for key in [
        "toolNonExecutionKind",
        "toolSurface",
        "toolSource",
        "toolIcon",
    ] {
        assert_eq!(encoded[key], extra[key], "{key}");
    }
    let bare = round_trip::<OrchestrationV2TurnItem>(turn_item_json(
        "reasoning",
        json!({ "text": "Thinking", "streaming": false }),
    ));
    for key in [
        "toolNonExecutionKind",
        "toolSurface",
        "toolSource",
        "toolIcon",
    ] {
        assert!(bare.get(key).is_none(), "{key}");
    }
    rejects::<OrchestrationV2TurnItem>(turn_item_json(
        "reasoning",
        json!({ "text": "x", "streaming": false, "toolSurface": "phone" }),
    ));
}

#[test]
fn turn_item_unknown_input_and_exit_code_types() {
    // `Schema.Unknown`: JSON.stringify drops an undefined `input`, so an absent
    // key must decode.
    let item = decode::<OrchestrationV2TurnItem>(turn_item_json(
        "dynamic_tool",
        json!({ "toolName": null }),
    ));
    match &item {
        OrchestrationV2TurnItem::DynamicTool { input, output, .. } => {
            assert_eq!(*input, Value::Null);
            assert!(output.is_none());
        }
        other => panic!("{other:?}"),
    }
    let encoded = round_trip::<OrchestrationV2TurnItem>(turn_item_json(
        "dynamic_tool",
        json!({ "toolName": "t", "input": { "a": [1, 2.5] }, "output": "ok", "outputOmitted": true }),
    ));
    assert_eq!(encoded["input"], json!({ "a": [1, 2.5] }));

    // `Schema.Int`: negative exit codes are valid, fractions are not.
    let encoded = round_trip::<OrchestrationV2TurnItem>(turn_item_json(
        "command_execution",
        json!({ "input": "ls", "exitCode": -1 }),
    ));
    assert_eq!(encoded["exitCode"], json!(-1));
    rejects::<OrchestrationV2TurnItem>(turn_item_json(
        "command_execution",
        json!({ "input": "ls", "exitCode": 1.5 }),
    ));
    rejects::<OrchestrationV2TurnItem>(turn_item_json(
        "file_change",
        json!({ "fileName": "a", "additions": -1 }),
    ));
}

#[test]
fn fork_item_provider_thread_id_is_optional_not_nullable() {
    let item = turn_item_json(
        "fork",
        json!({ "source": { "type": "node", "nodeId": "node-1" }, "targetThreadId": "thread-2" }),
    );
    // Fork overrides the base NullOr `providerThreadId` with `optional`.
    let encoded =
        round_trip::<OrchestrationV2TurnItem>(without(item.clone(), &["providerThreadId"]));
    assert!(encoded.get("providerThreadId").is_none());
    let encoded =
        round_trip::<OrchestrationV2TurnItem>(merge(item, json!({ "providerThreadId": "pt-1" })));
    assert_eq!(encoded["providerThreadId"], json!("pt-1"));
}

#[test]
fn thread_created_target_model_is_trimmed_non_empty() {
    let item = |model: &str| {
        turn_item_json(
            "thread_created",
            json!({
                "targetThreadId": "thread-2",
                "targetRunId": null,
                "targetProviderInstanceId": "codex",
                "targetModel": model,
            }),
        )
    };
    let encoded = round_trip::<OrchestrationV2TurnItem>(item("  gpt-5  "));
    assert_eq!(encoded["targetModel"], json!("gpt-5"));
    rejects::<OrchestrationV2TurnItem>(item("   "));
}

#[test]
fn turn_item_union_is_tagged_by_type_and_rejects_unknown_known_defects() {
    rejects::<OrchestrationV2TurnItem>(turn_item_json("hologram", json!({ "beam": "ref-1" })));
    rejects::<OrchestrationV2TurnItem>(without(
        turn_item_json("system_notice", json!({ "message": "x" })),
        &["updatedAt"],
    ));
    rejects::<OrchestrationV2TurnItem>(turn_item_json("system_notice", json!({})));
    rejects::<OrchestrationV2TurnItem>(turn_item_json(
        "system_notice",
        json!({ "message": "x", "ordinal": -1 }),
    ));
}

#[test]
fn turn_item_tagged_union_known_tags_match_serialized_tags() {
    let samples: Vec<OrchestrationV2TurnItem> = [
        ("notification", json!({ "source": { "kind": "system" }, "outcome": "unknown", "summary": "s" })),
        (
            "user_message",
            json!({ "createdBy": "user", "creationSource": "web", "messageId": "m", "inputIntent": "steer", "text": "t", "attachments": [] }),
        ),
        ("assistant_message", json!({ "messageId": "m", "text": "t", "streaming": false })),
        ("reasoning", json!({ "text": "t", "streaming": true })),
        ("proposed_plan", json!({ "planId": "p", "markdown": "m", "streaming": false })),
        ("todo_list", json!({ "planId": "p", "steps": [] })),
        ("user_input_request", json!({ "requestId": "r", "questions": [], "responseMode": "message" })),
        ("file_change", json!({ "fileName": "f" })),
        ("command_execution", json!({ "input": "ls" })),
        ("file_search", json!({})),
        ("web_search", json!({})),
        ("approval_request", json!({ "requestId": "r", "requestKind": "file-read" })),
        ("checkpoint", json!({ "checkpointId": "c", "scopeId": "s", "files": [] })),
        ("run_interrupt_request", json!({ "message": "m" })),
        ("run_interrupt_result", json!({ "message": "m" })),
        ("system_notice", json!({ "message": "m" })),
        (
            "error",
            json!({ "failure": { "class": "unknown", "message": "m", "code": null, "retryable": null } }),
        ),
        ("compaction", json!({ "driver": null })),
        (
            "handoff",
            json!({
                "contextHandoffId": "h", "fromProviderThreadIds": [], "toProviderThreadId": "p",
                "fromProviderInstanceIds": [], "toProviderInstanceId": "codex", "strategy": "manual_context",
            }),
        ),
        ("fork", json!({ "source": { "type": "node", "nodeId": "n" }, "targetThreadId": "t" })),
        (
            "thread_created",
            json!({ "targetThreadId": "t", "targetRunId": null, "targetProviderInstanceId": "codex", "targetModel": "m" }),
        ),
        (
            "secret_request",
            json!({ "label": "Token", "reason": "needed", "secretStatus": "pending" }),
        ),
        (
            "subagent",
            json!({
                "subagentId": "n", "origin": "app_owned", "driver": "codex", "providerInstanceId": "codex",
                "childThreadId": null, "prompt": "p", "result": null,
            }),
        ),
        ("dynamic_tool", json!({ "toolName": null, "input": null })),
    ]
    .into_iter()
    .map(|(kind, extra)| decode(turn_item_json(kind, extra)))
    .collect();
    assert_eq!(samples.len(), OrchestrationV2TurnItem::known_tags().len());
    assert_known_tags(&samples);
}

#[test]
fn forward_compatible_turn_item_decodes_unknown_tags_and_never_encodes_them() {
    let future = turn_item_json("hologram", json!({ "beam": "ref-1" }));
    let decoded: ForwardCompatible<OrchestrationV2TurnItem> = decode(future.clone());
    match &decoded {
        ForwardCompatible::Unknown(member) => {
            assert_eq!(member.tag, "type");
            assert_eq!(member.value, "hologram");
            assert_eq!(member.raw, future);
        }
        other => panic!("expected unknown member, got {other:?}"),
    }
    assert!(serde_json::to_value(&decoded).is_err());

    let known: ForwardCompatible<OrchestrationV2TurnItem> = decode(turn_item_json(
        "system_notice",
        json!({ "message": "Hello" }),
    ));
    assert!(known.known().is_some());
    // A known tag with a broken payload still fails.
    assert!(
        serde_json::from_value::<ForwardCompatible<OrchestrationV2TurnItem>>(turn_item_json(
            "system_notice",
            json!({}),
        ))
        .is_err()
    );
}

// ---------------------------------------------------------------------------
// Projection arrays
// ---------------------------------------------------------------------------

fn projection_json(turn_items: Vec<Value>, visible: Vec<Value>) -> Value {
    json!({
        "thread": app_thread_json(),
        "runs": [], "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
        "providerThreads": [], "providerTurns": [], "runtimeRequests": [], "messages": [],
        "plans": [], "turnItems": turn_items, "checkpointScopes": [], "checkpoints": [],
        "contextHandoffs": [], "contextTransfers": [], "visibleTurnItems": visible,
        "updatedAt": NOW,
    })
}

fn projected(position: u64, item: &Value) -> Value {
    json!({
        "position": position,
        "visibility": "local",
        "sourceThreadId": "thread-1",
        "sourceItemId": item["id"],
        "item": item,
    })
}

#[test]
fn projection_drops_unknown_turn_item_types_but_not_broken_known_ones() {
    let known = turn_item_json(
        "system_notice",
        json!({ "id": "item-known", "message": "Hello" }),
    );
    let future = turn_item_json("hologram", json!({ "id": "item-future", "beam": "ref-1" }));
    let projection: OrchestrationV2ThreadProjection = decode(projection_json(
        vec![future.clone(), known.clone()],
        vec![projected(0, &future), projected(1, &known)],
    ));
    assert_eq!(projection.turn_items.len(), 1);
    assert_eq!(projection.visible_turn_items.len(), 1);
    assert_eq!(
        projection.visible_turn_items[0].source_item_id.as_str(),
        "item-known"
    );

    let broken = turn_item_json("system_notice", json!({ "id": "item-broken" }));
    rejects::<OrchestrationV2ThreadProjection>(projection_json(vec![broken.clone()], vec![]));
    rejects::<OrchestrationV2ThreadProjection>(projection_json(
        vec![],
        vec![projected(0, &broken)],
    ));

    // Re-encoding keeps only the known rows.
    let encoded = encode(&projection);
    assert_eq!(encoded["turnItems"].as_array().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// Domain events
// ---------------------------------------------------------------------------

fn event_json(kind: &str, payload: Value) -> Value {
    json!({ "id": "event-1", "type": kind, "threadId": "thread-1", "occurredAt": NOW, "payload": payload })
}

#[test]
fn domain_event_thread_group_shares_the_app_thread_payload() {
    let thread_tags: Vec<&str> = KNOWN_ORCHESTRATION_V2_EVENT_TYPES
        .iter()
        .copied()
        .filter(|tag| tag.starts_with("thread.") && *tag != "thread.pull-request-link-updated")
        .collect();
    assert_eq!(thread_tags.len(), 21);
    for tag in thread_tags {
        let event: OrchestrationV2DomainEvent = decode(event_json(tag, app_thread_json()));
        let encoded = encode(&event);
        assert_eq!(encoded["type"], json!(tag));
        // Optional base fields stay absent.
        for key in [
            "runId",
            "nodeId",
            "driver",
            "providerInstanceId",
            "rawEventId",
        ] {
            assert!(encoded.get(key).is_none(), "{tag} {key}");
        }
    }
    rejects::<OrchestrationV2DomainEvent>(event_json("thread.vanished", app_thread_json()));
    rejects::<OrchestrationV2DomainEvent>(event_json("run.created", app_thread_json()));
}

#[test]
fn domain_event_known_types_cover_every_variant_once() {
    use std::collections::BTreeSet;
    assert_eq!(KNOWN_ORCHESTRATION_V2_EVENT_TYPES.len(), 44);
    let unique: BTreeSet<_> = KNOWN_ORCHESTRATION_V2_EVENT_TYPES.iter().collect();
    assert_eq!(unique.len(), 44);
    assert!(is_known_orchestration_v2_event_type("turn-item.updated"));
    assert!(!is_known_orchestration_v2_event_type(
        "run.from-a-future-server"
    ));
}

#[test]
fn domain_event_run_created_round_trips_with_ids() {
    let event: OrchestrationV2DomainEvent = decode(merge(
        event_json("run.created", run_json()),
        json!({ "runId": "run-1", "driver": "codex", "providerInstanceId": "codex", "rawEventId": "raw-1" }),
    ));
    let encoded = encode(&event);
    assert_eq!(encoded["runId"], json!("run-1"));
    assert_eq!(encoded["rawEventId"], json!("raw-1"));
    assert_eq!(encoded["payload"]["status"], json!("queued"));
    rejects::<OrchestrationV2DomainEvent>(event_json("run.created", json!({})));
}

// ---------------------------------------------------------------------------
// Raw provider events
// ---------------------------------------------------------------------------

#[test]
fn raw_provider_event_json_rpc_id_keeps_integer_form() {
    let event = |id: Value| {
        json!({
            "id": "raw-1",
            "driver": "codex",
            "providerInstanceId": "codex",
            "providerSessionId": "ps-1",
            "sequence": 1,
            "direction": "incoming",
            "messageKind": "response",
            "method": null,
            "jsonRpcId": id,
            "payload": { "ok": true },
            "observedAt": NOW,
        })
    };
    for id in [json!(5), json!("req-5"), json!(2.5), Value::Null] {
        let encoded = round_trip::<OrchestrationV2RawProviderEvent>(event(id.clone()));
        assert_eq!(encoded["jsonRpcId"], id);
    }
    // Serialized text keeps `5`, never `5.0`.
    let text =
        serde_json::to_string(&decode::<OrchestrationV2RawProviderEvent>(event(json!(5)))).unwrap();
    assert!(text.contains("\"jsonRpcId\":5,"), "{text}");
    rejects::<OrchestrationV2RawProviderEvent>(event(json!(true)));
    rejects::<OrchestrationV2RawProviderEvent>(merge(event(json!(1)), json!({ "sequence": 0 })));
    // `payload: Schema.Unknown` tolerates an absent key.
    decode::<OrchestrationV2RawProviderEvent>(without(event(json!(1)), &["payload"]));
}

// ---------------------------------------------------------------------------
// Plans, checkpoints, small structs
// ---------------------------------------------------------------------------

#[test]
fn plan_artifact_kinds_and_detail_literal() {
    let base = |kind: &str, extra: Value| {
        merge(
            json!({ "id": "plan-1", "threadId": "thread-1", "runId": null, "nodeId": "node-1", "status": "draft", "kind": kind }),
            extra,
        )
    };
    let plan = round_trip::<OrchestrationV2PlanArtifact>(base(
        "proposed_plan",
        json!({ "markdown": "# P" }),
    ));
    assert!(plan.get("detailInTurnItem").is_none());
    let todo = round_trip::<OrchestrationV2PlanArtifact>(base(
        "todo_list",
        json!({
            "steps": [{ "id": "s", "text": "t", "status": "running", "durationAnchorAt": "x", "durationMs": 3 }],
            "explanation": "why",
            "detailInTurnItem": true,
        }),
    ));
    assert_eq!(todo["steps"][0]["durationAnchorAt"], json!("x"));
    assert_eq!(todo["detailInTurnItem"], json!(true));
    rejects::<OrchestrationV2PlanArtifact>(base(
        "todo_list",
        json!({ "steps": [], "detailInTurnItem": false }),
    ));
    rejects::<OrchestrationV2PlanArtifact>(base("proposed_plan", json!({})));
    rejects::<OrchestrationV2PlanArtifact>(base("diagram", json!({})));
}

#[test]
fn checkpoint_ref_key_and_ordinal_bounds() {
    let checkpoint = json!({
        "id": "checkpoint-1", "threadId": "thread-1", "scopeId": "scope-1", "runId": null,
        "nodeId": "node-1", "parentCheckpointId": null, "ordinalWithinScope": 0,
        "appRunOrdinal": null, "ref": "git-ref-1", "status": "ready",
        "files": [{ "path": "a", "kind": "modified", "additions": 1, "deletions": 0 }],
        "capturedAt": NOW,
    });
    let encoded = round_trip::<OrchestrationV2Checkpoint>(checkpoint.clone());
    assert_eq!(encoded["ref"], json!("git-ref-1"));
    assert_eq!(encoded["appRunOrdinal"], Value::Null);
    rejects::<OrchestrationV2Checkpoint>(merge(checkpoint.clone(), json!({ "appRunOrdinal": 0 })));
    rejects::<OrchestrationV2Checkpoint>(merge(checkpoint.clone(), json!({ "status": "gone" })));
    rejects::<OrchestrationV2Checkpoint>(without(checkpoint, &["ref"]));
}

#[test]
fn user_input_question_optional_fields_and_whitespace_answers() {
    let question = json!({
        "id": "q", "header": "h", "question": "?",
        "options": [{ "label": "a", "description": "d" }],
        "initialAnswer": "",
    });
    let encoded = round_trip::<OrchestrationV2UserInputQuestion>(question);
    // An empty initial answer is a present value, not absent.
    assert_eq!(encoded["initialAnswer"], json!(""));
    for key in ["multiSelect", "allowCustomAnswer", "required"] {
        assert!(encoded.get(key).is_none(), "{key}");
    }
    assert!(encoded["options"][0].get("value").is_none());
    rejects::<OrchestrationV2UserInputQuestion>(json!({
        "id": "q", "header": "h", "question": "?",
        "options": [{ "label": " ", "description": "d" }],
    }));
}

#[test]
fn restart_cancelled_background_work_kinds() {
    for kind in ["subagent", "shell", "monitor", "task"] {
        let encoded = round_trip::<OrchestrationV2RunBackgroundWorkCancelled>(json!({
            "runId": "run-1",
            "restartCancelledBackgroundWork": [{ "kind": kind, "label": "work" }],
        }));
        assert_eq!(
            encoded["restartCancelledBackgroundWork"][0]["kind"],
            json!(kind)
        );
        assert!(
            encoded["restartCancelledBackgroundWork"][0]
                .get("id")
                .is_none()
        );
    }
    rejects::<OrchestrationV2RunBackgroundWorkCancelled>(json!({
        "runId": "run-1",
        "restartCancelledBackgroundWork": [{ "kind": "cron", "label": "work" }],
    }));
}

#[test]
fn subagent_optional_fields_and_literals() {
    let subagent = json!({
        "id": "node-sub", "threadId": "thread-1", "runId": null, "parentNodeId": "node-root",
        "origin": "app_owned", "createdBy": "agent", "driver": "codex", "providerInstanceId": "codex",
        "providerThreadId": null, "childThreadId": null, "nativeTaskRef": null, "prompt": "p",
        "title": null, "model": null, "status": "idle", "result": null,
        "startedAt": null, "completedAt": null, "updatedAt": NOW,
    });
    let encoded = round_trip::<OrchestrationV2Subagent>(subagent.clone());
    for key in [
        "modelSelection",
        "completionWake",
        "completionDelivery",
        "progress",
        "workflow",
    ] {
        assert!(encoded.get(key).is_none(), "{key}");
    }
    let encoded = round_trip::<OrchestrationV2Subagent>(merge(
        subagent.clone(),
        json!({
            "completionWake": "settled_only",
            "completionDelivery": { "state": "claimed", "observedByRunId": null },
        }),
    ));
    assert_eq!(
        encoded["completionDelivery"]["observedByRunId"],
        Value::Null
    );
    rejects::<OrchestrationV2Subagent>(merge(
        subagent.clone(),
        json!({ "completionWake": "sometimes" }),
    ));
    rejects::<OrchestrationV2Subagent>(merge(subagent, json!({ "status": "stopped" })));
}

#[test]
fn delegated_completion_cohort_positive_generation() {
    let run = merge(
        run_json(),
        json!({
            "delegatedCompletion": {
                "disposition": "open",
                "nextGeneration": 2,
                "delivery": { "generation": 1, "messageId": "m", "taskIds": ["n"] },
            },
        }),
    );
    let encoded = round_trip::<OrchestrationV2Run>(run.clone());
    assert_eq!(encoded["delegatedCompletion"], run["delegatedCompletion"]);
    let mut zero = run;
    zero["delegatedCompletion"]["nextGeneration"] = json!(0);
    rejects::<OrchestrationV2Run>(zero);
}
