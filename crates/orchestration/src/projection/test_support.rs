//! JSON fixture builders for projection tests. They mirror the object
//! literals the upstream tests pass (`ProjectionStore.test.ts`), so a fixture
//! reads the same here as there.

use serde_json::{Value, json};
use turin_contracts::orchestration_v2::OrchestrationV2DomainEvent;
use turin_persistence::Db;

use crate::projection::store::ProjectionStore;

pub const T0: &str = "2026-01-01T00:00:00.000Z";

pub fn at(second: u32) -> String {
    format!("2026-01-01T00:00:{second:02}.000Z")
}

pub fn model_selection() -> Value {
    json!({"instanceId": "codex", "model": "gpt-5.4"})
}

pub fn thread(id: &str) -> Value {
    json!({
        "createdBy": "user",
        "creationSource": "web",
        "id": id,
        "projectId": "project:test",
        "title": id,
        "providerInstanceId": "codex",
        "modelSelection": model_selection(),
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "branch": null,
        "worktreePath": null,
        "activeProviderThreadId": null,
        "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": id},
        "forkedFrom": null,
        "createdAt": T0,
        "updatedAt": T0,
        "archivedAt": null,
        "settledOverride": null,
        "settledAt": null,
        "lastVisitedAt": null,
        "deletedAt": null
    })
}

pub fn run(thread_id: &str, id: &str, ordinal: u64, status: &str) -> Value {
    json!({
        "id": id,
        "threadId": thread_id,
        "ordinal": ordinal,
        "providerInstanceId": "codex",
        "modelSelection": model_selection(),
        "providerThreadId": null,
        "userMessageId": format!("message:{id}"),
        "rootNodeId": format!("node:{id}:root"),
        "activeAttemptId": null,
        "status": status,
        "requestedAt": T0,
        "startedAt": T0,
        "completedAt": null,
        "checkpointId": null,
        "contextHandoffId": null
    })
}

/// A turn item with the base fields; `extra` carries the type-specific ones.
pub fn turn_item(
    thread_id: &str,
    id: &str,
    run_id: Option<&str>,
    ordinal: u64,
    kind: &str,
    status: &str,
    extra: Value,
) -> Value {
    let mut item = json!({
        "id": id,
        "threadId": thread_id,
        "runId": run_id,
        "nodeId": run_id.map(|run| format!("node:{run}:root")),
        "providerThreadId": null,
        "providerTurnId": null,
        "nativeItemRef": null,
        "parentItemId": null,
        "ordinal": ordinal,
        "status": status,
        "title": null,
        "startedAt": T0,
        "completedAt": null,
        "updatedAt": T0,
        "type": kind
    });
    item.as_object_mut()
        .unwrap()
        .extend(extra.as_object().cloned().unwrap_or_default());
    item
}

pub fn user_message_item(
    thread_id: &str,
    id: &str,
    run_id: &str,
    ordinal: u64,
    text: &str,
    intent: &str,
) -> Value {
    turn_item(
        thread_id,
        id,
        Some(run_id),
        ordinal,
        "user_message",
        "completed",
        json!({
            "createdBy": "user", "creationSource": "web", "messageId": format!("message:{id}"),
            "inputIntent": intent, "text": text, "attachments": []
        }),
    )
}

pub fn assistant_item(thread_id: &str, id: &str, run_id: &str, ordinal: u64, text: &str) -> Value {
    turn_item(
        thread_id,
        id,
        Some(run_id),
        ordinal,
        "assistant_message",
        "completed",
        json!({"messageId": format!("message:{id}"), "text": text, "streaming": false}),
    )
}

pub fn message(
    thread_id: &str,
    id: &str,
    run_id: Option<&str>,
    role: &str,
    text: &str,
    created_at: &str,
) -> Value {
    json!({
        "createdBy": if role == "user" { "user" } else { "agent" },
        "creationSource": "web",
        "id": id,
        "threadId": thread_id,
        "runId": run_id,
        "nodeId": null,
        "role": role,
        "text": text,
        "attachments": [],
        "streaming": false,
        "createdAt": created_at,
        "updatedAt": created_at
    })
}

/// Build a domain event from its parts (`{ id, type, threadId, occurredAt, payload }`).
pub fn event(
    kind: &str,
    thread_id: &str,
    occurred_at: &str,
    payload: Value,
) -> OrchestrationV2DomainEvent {
    serde_json::from_value(json!({
        "id": format!("event:{kind}:{}:{occurred_at}", payload["id"].as_str().unwrap_or("-")),
        "type": kind,
        "threadId": thread_id,
        "occurredAt": occurred_at,
        "payload": payload
    }))
    .unwrap_or_else(|error| panic!("event fixture {kind}: {error}"))
}

pub async fn memory_store() -> ProjectionStore {
    ProjectionStore::new(Db::open_in_memory().await.expect("in-memory db"))
}

/// A file-backed store, so reads run on the separate read-only connection.
pub async fn file_store() -> (ProjectionStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::open(dir.path().join("state.sqlite"))
        .await
        .expect("file db");
    (ProjectionStore::new(db), dir)
}

/// Apply events in order, each in its own transaction.
pub async fn apply_all(store: &ProjectionStore, events: Vec<OrchestrationV2DomainEvent>) {
    for event in events {
        store.apply(event).await.expect("apply");
    }
}

/// First column of the first row as text (`None` for no row or NULL).
pub async fn scalar(store: &ProjectionStore, sql: &'static str, arg: &str) -> Option<String> {
    let arg = arg.to_owned();
    store
        .db()
        .read(move |conn| {
            Ok(conn
                .query_row(sql, [arg], |row| {
                    Ok(match row.get::<_, rusqlite::types::Value>(0)? {
                        rusqlite::types::Value::Null => None,
                        rusqlite::types::Value::Integer(value) => Some(value.to_string()),
                        rusqlite::types::Value::Real(value) => Some(value.to_string()),
                        rusqlite::types::Value::Text(value) => Some(value),
                        rusqlite::types::Value::Blob(_) => None,
                    })
                })
                .ok()
                .flatten())
        })
        .await
        .expect("scalar read")
}

pub async fn count(store: &ProjectionStore, sql: &'static str, arg: &str) -> i64 {
    scalar(store, sql, arg)
        .await
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

/// Provider capabilities with every flag off, enough for a session fixture.
pub fn capabilities() -> Value {
    json!({
        "sessions": {"supportsMultipleProviderThreadsPerSession": false, "supportsModelSwitchInSession": false,
            "supportsProviderSwitchingViaHandoff": false, "supportsRuntimeModeSwitchInSession": false,
            "pendingRequestsSurviveRestart": false},
        "threads": {"canCreateEmptyThread": false, "canReadThreadSnapshot": false, "canRollbackThread": false,
            "canForkThread": false, "canForkFromTurn": false, "canForkFromSubagentThread": false,
            "exposesNativeThreadId": false},
        "turns": {"exposesNativeTurnId": false, "emitsTurnStarted": false, "emitsTurnCompleted": false,
            "supportsInterrupt": false, "supportsActiveSteering": false,
            "supportsSteeringByInterruptRestart": false, "supportsQueuedMessages": false,
            "terminalStatusQuality": "weak"},
        "streaming": {"streamsAssistantText": false, "streamsReasoning": false, "streamsToolOutput": false,
            "streamsPlanText": false, "emitsMessageCompleted": false},
        "tools": {"exposesToolItemIds": false, "emitsToolStarted": false, "emitsToolCompleted": false,
            "emitsToolOutput": false, "supportsMcpTools": false, "supportsDynamicToolCallbacks": false},
        "approvals": {"supportsCommandApproval": false, "supportsFileReadApproval": false,
            "supportsFileChangeApproval": false, "supportsApplyPatchApproval": false,
            "approvalsHaveNativeRequestIds": false, "approvalCallbacksAreLiveOnly": false,
            "approvalsCanOriginateFromSubagents": false},
        "planning": {"emitsPlanUpdated": false, "emitsTodoList": false, "emitsProposedPlan": false,
            "supportsStructuredQuestions": false, "planDeltasHaveItemIds": false},
        "subagents": {"supportsSubagents": false, "exposesSubagentThreadIds": false,
            "emitsSubagentLifecycle": false, "canWaitForSubagents": false, "canCloseSubagents": false,
            "canForkSubagentThread": false},
        "context": {"acceptsSystemContext": false, "acceptsDeveloperContext": false,
            "acceptsSyntheticUserContext": false, "canGenerateSummaries": false,
            "canConsumeHandoffSummaries": false, "supportsDeltaHandoff": false,
            "supportsFullThreadHandoff": false, "maxRecommendedHandoffChars": null},
        "checkpointing": {"appCanCheckpointFilesystem": false, "supportsNestedCheckpointScopes": false,
            "providerCanRollbackConversation": false, "providerRollbackReturnsSnapshot": false,
            "providerCanReadConversationSnapshot": false},
        "identity": {"nativeThreadIds": "weak", "nativeTurnIds": "weak", "nativeItemIds": "weak",
            "nativeRequestIds": "weak"}
    })
}
