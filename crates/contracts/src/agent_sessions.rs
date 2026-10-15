//! Port of upstream/packages/contracts/src/agentSessions.ts

use crate::base_schemas::{IsoDateTime, NonNegativeInt, ProjectId, TrimmedNonEmptyString};
use crate::provider_instance::ProviderInstanceId;
use serde::{Deserialize, Serialize};

/// Coding agent home directories the scanner knows how to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentSessionSource {
    ClaudeAgent,
    Codex,
}

/// File identity saved with an imported session so bounded retries can skip
/// unchanged history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionImportSource {
    pub provider: AgentSessionSource,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_session_id: TrimmedNonEmptyString,
    pub file_path: TrimmedNonEmptyString,
    pub size: NonNegativeInt,
    pub mtime_ms: Option<f64>,
    pub device: f64,
    pub inode: Option<f64>,
    pub birthtime_ms: Option<f64>,
}

/// Empty for now. Kept as a struct so future scan options (source filters,
/// explicit roots) can be added without a new method.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionScanInput {}

/// Git identity of a candidate directory, read from `.git/config` without
/// spawning git. `remoteKey` is the normalized origin URL, shared by every
/// clone of the same repository so the client can group them. `repository` is
/// the GitHub `owner/name` when the origin is on GitHub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionProjectGit {
    pub remote_key: Option<String>,
    pub repository: Option<String>,
}

/// A directory that at least one agent CLI has run in, suitable for import
/// as a T3 Code project. `already_imported` marks candidates that already
/// have an active project rooted at the same path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionProjectCandidate {
    pub path: TrimmedNonEmptyString,
    pub title: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    pub sources: Vec<AgentSessionSource>,
    pub thread_count: NonNegativeInt,
    pub last_active_at: Option<IsoDateTime>,
    pub already_imported: bool,
    /// `null` when the directory is not the root of a git repository.
    /// Missing on servers that predate the git scan, where the client cannot
    /// tell repositories from plain folders and should treat every candidate
    /// as a standalone project.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub git: Option<Option<AgentSessionProjectGit>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionScanResult {
    pub candidates: Vec<AgentSessionProjectCandidate>,
    pub scanned_at: IsoDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionImportInput {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_workspace_root: Option<TrimmedNonEmptyString>,
}

/// `AgentSessionImportProjectNotFoundError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "_tag", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AgentSessionImportProjectNotFoundError {
    #[serde(rename = "AgentSessionImportProjectNotFoundError")]
    #[error("Project '{project_id}' does not exist.")]
    NotFound { project_id: ProjectId },
}

/// `AgentSessionImportProjectChangedError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "_tag", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AgentSessionImportProjectChangedError {
    #[serde(rename = "AgentSessionImportProjectChangedError")]
    #[error("Project '{project_id}' changed directories. Scan for projects again before importing history.")]
    Changed { project_id: ProjectId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionImportResult {
    pub imported_count: NonNegativeInt,
    pub skipped_count: NonNegativeInt,
}

/// `AgentSessionScanError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "_tag", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AgentSessionScanError {
    #[serde(rename = "AgentSessionScanError")]
    #[error("Failed to scan agent sessions during {operation}.")]
    ScanError {
        operation: AgentSessionScanOperation,
        cause: serde_json::Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentSessionScanOperation {
    ReadSettings,
    ReadProjects,
}

impl std::fmt::Display for AgentSessionScanOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ReadSettings => "read-settings",
            Self::ReadProjects => "read-projects",
        })
    }
}
