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
