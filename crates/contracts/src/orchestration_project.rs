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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_identity: Option<Option<RepositoryIdentity>>,
    pub default_model_selection: Option<ModelSelection>,
    // Per-project override for where new threads start. Null/absent means
    // "no override": clients fall back to t3.json, then the global setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_thread_env_mode: Option<Option<ThreadEnvMode>>,
    // Opt-in because background sync performs network I/O and may move the checkout.
    // Optional on the wire so cached snapshots from older servers still decode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_pull: Option<bool>,
    // Optional on the wire so cached snapshots from older servers still decode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favicon_path: Option<Option<TrimmedNonEmptyString>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_icon: Option<Option<ReceivedProjectIcon>>,
    pub scripts: Vec<ProjectScript>,
    pub created_at: IsoDateTime,
    pub updated_at: IsoDateTime,
}
