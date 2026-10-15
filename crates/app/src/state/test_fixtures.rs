//! Port of upstream/packages/client-runtime/src/state/orchestrationV2TestFixtures.ts
//! (test support only).

use serde_json::json;

use crate::state::contracts::{
    OrchestrationProjectShell, OrchestrationV2ThreadProjection, OrchestrationV2ThreadShell,
};

pub const V2_NOW: &str = "2026-06-20T00:00:00.000Z";

pub fn v2_project() -> OrchestrationProjectShell {
    serde_json::from_value(json!({
        "id": "project-v2",
        "title": "Project",
        "workspaceRoot": "/workspace/project",
        "repositoryIdentity": null,
        "defaultModelSelection": null,
        "scripts": [],
        "createdAt": V2_NOW,
        "updatedAt": V2_NOW,
    }))
    .expect("fixture decodes")
}

fn thread_fields() -> serde_json::Value {
    json!({
        "id": "thread-v2",
        "projectId": "project-v2",
        "title": "Thread",
        "providerInstanceId": "codex",
        "modelSelection": { "instanceId": "codex", "model": "gpt-5.4" },
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "branch": null,
        "worktreePath": null,
        "activeProviderThreadId": null,
        "lineage": { "rootThreadId": "thread-v2", "parentThreadId": null, "relationshipToParent": null },
        "forkedFrom": null,
        "createdBy": "user",
        "creationSource": "web",
        "createdAt": V2_NOW,
        "updatedAt": V2_NOW,
        "archivedAt": null,
        "settledOverride": null,
        "settledAt": null,
        "lastVisitedAt": null,
        "deletedAt": null,
    })
}

pub fn v2_thread_shell() -> OrchestrationV2ThreadShell {
    let mut value = thread_fields();
    let object = value.as_object_mut().expect("object");
    for (key, field) in [
        ("latestRunId", json!(null)),
        ("activeRunId", json!(null)),
        ("status", json!("idle")),
        ("pendingRuntimeRequest", json!(null)),
        ("latestVisibleMessage", json!(null)),
        ("latestUserMessageAt", json!(null)),
        ("hasActionableProposedPlan", json!(false)),
        ("itemCount", json!(0)),
        ("visibleItemCount", json!(0)),
    ] {
        object.insert(key.into(), field);
    }
    serde_json::from_value(value).expect("fixture decodes")
}

pub fn v2_projection() -> OrchestrationV2ThreadProjection {
    serde_json::from_value(json!({
        "thread": thread_fields(),
        "runs": [],
        "attempts": [],
        "nodes": [],
        "subagents": [],
        "providerSessions": [],
        "providerThreads": [],
        "providerTurns": [],
        "runtimeRequests": [],
        "messages": [],
        "plans": [],
        "turnItems": [],
        "checkpointScopes": [],
        "checkpoints": [],
        "contextHandoffs": [],
        "contextTransfers": [],
        "visibleTurnItems": [],
        "updatedAt": V2_NOW,
    }))
    .expect("fixture decodes")
}

/// Decode a fixture literal into a contract type.
pub fn from_json<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
    serde_json::from_value(value).expect("fixture decodes")
}

/// A run of `thread-v2`.
pub fn v2_run(id: &str, ordinal: u64, status: &str) -> crate::state::contracts::OrchestrationV2Run {
    from_json(json!({
        "id": id,
        "threadId": "thread-v2",
        "ordinal": ordinal,
        "providerInstanceId": "codex",
        "modelSelection": { "instanceId": "codex", "model": "gpt-5.4" },
        "providerThreadId": null,
        "userMessageId": "message-reducer",
        "rootNodeId": null,
        "activeAttemptId": null,
        "status": status,
        "requestedAt": V2_NOW,
        "startedAt": V2_NOW,
        "completedAt": V2_NOW,
        "checkpointId": null,
        "contextHandoffId": null,
    }))
}

/// A completed `command_execution` item of `thread-v2`.
pub fn v2_command_item(
    id: &str,
    run_id: &str,
    output: &str,
    ordinal: u64,
) -> crate::state::contracts::OrchestrationV2TurnItem {
    from_json(json!({
        "id": id,
        "threadId": "thread-v2",
        "runId": run_id,
        "nodeId": null,
        "providerThreadId": null,
        "providerTurnId": null,
        "nativeItemRef": null,
        "parentItemId": null,
        "ordinal": ordinal,
        "status": "completed",
        "title": null,
        "startedAt": V2_NOW,
        "completedAt": V2_NOW,
        "updatedAt": V2_NOW,
        "type": "command_execution",
        "input": "pwd",
        "output": output,
        "exitCode": 0,
    }))
}

/// A user message of `thread-v2`.
pub fn v2_message(
    id: &str,
    text: &str,
) -> crate::state::contracts::OrchestrationV2ConversationMessage {
    from_json(json!({
        "createdBy": "user",
        "creationSource": "web",
        "id": id,
        "threadId": "thread-v2",
        "runId": null,
        "nodeId": null,
        "role": "user",
        "text": text,
        "attachments": [],
        "streaming": false,
        "createdAt": V2_NOW,
        "updatedAt": V2_NOW,
    }))
}

/// Provider capabilities with every flag off, `overrides` merged per section
/// (`{ "turns": { "supportsQueuedMessages": true } }`).
pub fn v2_capabilities(
    overrides: serde_json::Value,
) -> crate::state::contracts::OrchestrationV2ProviderCapabilities {
    let mut base = json!({
        "sessions": {
            "supportsMultipleProviderThreadsPerSession": false,
            "supportsModelSwitchInSession": false,
            "supportsProviderSwitchingViaHandoff": false,
            "supportsRuntimeModeSwitchInSession": false,
            "pendingRequestsSurviveRestart": false,
        },
        "threads": {
            "canCreateEmptyThread": false,
            "canReadThreadSnapshot": false,
            "canRollbackThread": false,
            "canForkThread": false,
            "canForkFromTurn": false,
            "canForkFromSubagentThread": false,
            "exposesNativeThreadId": false,
        },
        "turns": {
            "exposesNativeTurnId": false,
            "emitsTurnStarted": false,
            "emitsTurnCompleted": false,
            "supportsInterrupt": false,
            "supportsActiveSteering": false,
            "supportsSteeringByInterruptRestart": false,
            "supportsQueuedMessages": false,
            "terminalStatusQuality": "none",
        },
        "streaming": {
            "streamsAssistantText": false,
            "streamsReasoning": false,
            "streamsToolOutput": false,
            "streamsPlanText": false,
            "emitsMessageCompleted": false,
        },
        "tools": {
            "exposesToolItemIds": false,
            "emitsToolStarted": false,
            "emitsToolCompleted": false,
            "emitsToolOutput": false,
            "supportsMcpTools": false,
            "supportsDynamicToolCallbacks": false,
        },
        "approvals": {
            "supportsCommandApproval": false,
            "supportsFileReadApproval": false,
            "supportsFileChangeApproval": false,
            "supportsApplyPatchApproval": false,
            "approvalsHaveNativeRequestIds": false,
            "approvalCallbacksAreLiveOnly": false,
            "approvalsCanOriginateFromSubagents": false,
        },
        "planning": {
            "emitsPlanUpdated": false,
            "emitsTodoList": false,
            "emitsProposedPlan": false,
            "supportsStructuredQuestions": false,
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
            "acceptsSyntheticUserContext": false,
            "canGenerateSummaries": false,
            "canConsumeHandoffSummaries": false,
            "supportsDeltaHandoff": false,
            "supportsFullThreadHandoff": false,
            "maxRecommendedHandoffChars": null,
        },
        "checkpointing": {
            "appCanCheckpointFilesystem": false,
            "supportsNestedCheckpointScopes": false,
            "providerCanRollbackConversation": false,
            "providerRollbackReturnsSnapshot": false,
            "providerCanReadConversationSnapshot": false,
        },
        "identity": {
            "nativeThreadIds": "none",
            "nativeTurnIds": "none",
            "nativeItemIds": "none",
            "nativeRequestIds": "none",
        },
    });
    if let Some(sections) = overrides.as_object() {
        for (section, fields) in sections {
            if let (Some(target), Some(fields)) =
                (base[section].as_object_mut(), fields.as_object())
            {
                for (key, value) in fields {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
    }
    from_json(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_decode() {
        assert_eq!(v2_thread_shell().id.as_str(), "thread-v2");
        assert_eq!(v2_projection().thread.id.as_str(), "thread-v2");
        assert_eq!(v2_project().id.as_str(), "project-v2");
    }
}
