//! Port of upstream/packages/contracts/src/threadSearch.ts

use crate::base_schemas::{IsoDateTime, ProjectId, ThreadId, TrimmedNonEmptyString, TrimmedString};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OrchestrationThreadSearchSource {
    User,
    Assistant,
}

/// The server's SQLite client is synchronous and single-connection. Bound both
/// scan input and response size so a search cannot monopolize that connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationSearchThreadsInput {
    pub query: TrimmedString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationThreadSearchMatch {
    pub thread_id: ThreadId,
    pub project_id: ProjectId,
    pub source: OrchestrationThreadSearchSource,
    pub snippet: String,
    pub message_created_at: Option<IsoDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationSearchThreadsResult {
    pub matches: Vec<OrchestrationThreadSearchMatch>,
}

/// `OrchestrationSearchThreadsError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "_tag", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum OrchestrationSearchThreadsError {
    #[serde(rename = "OrchestrationSearchThreadsError")]
    #[error("{message}")]
    OrchestrationSearchThreadsError {
        message: TrimmedNonEmptyString,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<serde_json::Value>,
    },
}
