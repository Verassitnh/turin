//! Port of upstream/packages/contracts/src/orchestrationProject.ts

use crate::base_schemas::{IsoDateTime, ProjectId, TrimmedNonEmptyString};
use crate::environment::{RepositoryIdentity, ThreadEnvMode};
use crate::model_selection::ModelSelection;
use crate::project::{ProjectScript, ReceivedProjectIcon};
use serde::{Deserialize, Serialize};

/// Project summary shared by the V2 shell and application project APIs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationProjectShell {
    pub id: ProjectId,
    pub title: TrimmedNonEmptyString,
    pub workspace_root: TrimmedNonEmptyString,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub repository_identity: Option<Option<RepositoryIdentity>>,
    pub default_model_selection: Option<ModelSelection>,
    // Per-project override for where new threads start. Null/absent means
    // "no override": clients fall back to t3.json, then the global setting.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub default_thread_env_mode: Option<Option<ThreadEnvMode>>,
    // Opt-in because background sync performs network I/O and may move the checkout.
    // Optional on the wire so cached snapshots from older servers still decode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_pull: Option<bool>,
    // Optional on the wire so cached snapshots from older servers still decode.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub favicon_path: Option<Option<TrimmedNonEmptyString>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub project_icon: Option<Option<ReceivedProjectIcon>>,
    pub scripts: Vec<ProjectScript>,
    pub created_at: IsoDateTime,
    pub updated_at: IsoDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn shell() -> serde_json::Value {
        json!({
            "id": "p1", "title": "T", "workspaceRoot": "/w",
            "defaultModelSelection": null, "scripts": [],
            "createdAt": "2026-01-01T00:00:00.000Z", "updatedAt": "2026-01-01T00:00:00.000Z",
        })
    }

    #[test]
    fn optional_nullable_fields_distinguish_absent_from_null() {
        let absent: OrchestrationProjectShell = serde_json::from_value(shell()).unwrap();
        assert_eq!(absent.default_thread_env_mode, None);
        assert_eq!(absent.favicon_path, None);
        assert_eq!(serde_json::to_value(&absent).unwrap(), shell());

        let mut with_nulls = shell();
        with_nulls["defaultThreadEnvMode"] = json!(null);
        with_nulls["faviconPath"] = json!(null);
        with_nulls["projectIcon"] = json!(null);
        with_nulls["repositoryIdentity"] = json!(null);
        let nulls: OrchestrationProjectShell = serde_json::from_value(with_nulls.clone()).unwrap();
        assert_eq!(nulls.default_thread_env_mode, Some(None));
        assert_eq!(nulls.favicon_path, Some(None));
        assert_eq!(nulls.repository_identity, Some(None));
        assert_eq!(serde_json::to_value(&nulls).unwrap(), with_nulls);
    }

    #[test]
    fn default_model_selection_encodes_null() {
        let decoded: OrchestrationProjectShell = serde_json::from_value(shell()).unwrap();
        assert_eq!(
            serde_json::to_value(&decoded).unwrap()["defaultModelSelection"],
            json!(null)
        );
    }
}
