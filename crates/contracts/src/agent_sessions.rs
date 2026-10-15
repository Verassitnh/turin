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
    #[serde(with = "crate::base_schemas::js_number_nullable")]
    pub mtime_ms: Option<f64>,
    #[serde(with = "crate::base_schemas::js_number")]
    pub device: f64,
    #[serde(with = "crate::base_schemas::js_number_nullable")]
    pub inode: Option<f64>,
    #[serde(with = "crate::base_schemas::js_number_nullable")]
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
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AgentSessionImportProjectNotFoundError {
    #[serde(rename = "AgentSessionImportProjectNotFoundError")]
    #[error("Project '{project_id}' does not exist.")]
    NotFound { project_id: ProjectId },
}

/// `AgentSessionImportProjectChangedError` (Schema.TaggedError).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AgentSessionImportProjectChangedError {
    #[serde(rename = "AgentSessionImportProjectChangedError")]
    #[error(
        "Project '{project_id}' changed directories. Scan for projects again before importing history."
    )]
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
#[serde(
    tag = "_tag",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn candidate() -> serde_json::Value {
        json!({
            "path": "/projects/repo",
            "title": "repo",
            "sources": ["codex"],
            "threadCount": 3,
            "lastActiveAt": "2026-08-20T12:00:00.000Z",
            "alreadyImported": false,
        })
    }

    #[test]
    fn decodes_candidates_from_servers_that_predate_the_git_scan() {
        let result: AgentSessionScanResult = serde_json::from_value(json!({
            "candidates": [candidate()],
            "scannedAt": "2026-08-22T12:00:00.000Z",
        }))
        .unwrap();
        // Absent git: distinct from `null` (not a repository).
        assert_eq!(result.candidates[0].git, None);
        let encoded = serde_json::to_value(&result).unwrap();
        assert!(encoded["candidates"][0].get("git").is_none());
    }

    #[test]
    fn preserves_reported_git_identity_and_null() {
        let git =
            json!({ "remoteKey": "github.com/pingdotgg/t3code", "repository": "pingdotgg/t3code" });
        let mut with_git = candidate();
        with_git["git"] = git.clone();
        let mut null_git = candidate();
        null_git["git"] = json!(null);
        let result: AgentSessionScanResult = serde_json::from_value(json!({
            "candidates": [with_git, null_git],
            "scannedAt": "2026-08-22T12:00:00.000Z",
        }))
        .unwrap();
        let reported = result.candidates[0].git.clone().unwrap().unwrap();
        assert_eq!(reported.repository.as_deref(), Some("pingdotgg/t3code"));
        assert_eq!(result.candidates[1].git, Some(None));
        let encoded = serde_json::to_value(&result).unwrap();
        assert_eq!(encoded["candidates"][0]["git"], git);
        assert_eq!(encoded["candidates"][1]["git"], json!(null));
    }

    #[test]
    fn import_source_keeps_nullable_numbers_required() {
        let source = json!({
            "provider": "claudeAgent",
            "providerInstanceId": "claudeAgent",
            "providerSessionId": "s1",
            "filePath": "/tmp/a.jsonl",
            "size": 10,
            "mtimeMs": 1.5,
            "device": 7,
            "inode": null,
            "birthtimeMs": null,
        });
        let decoded: AgentSessionImportSource = serde_json::from_value(source.clone()).unwrap();
        assert_eq!(decoded.mtime_ms, Some(1.5));
        assert_eq!(serde_json::to_value(&decoded).unwrap(), source);
    }

    #[test]
    fn error_messages() {
        let error = AgentSessionImportProjectChangedError::Changed {
            project_id: ProjectId::new("p1").unwrap(),
        };
        assert_eq!(
            error.to_string(),
            "Project 'p1' changed directories. Scan for projects again before importing history."
        );
        let error = AgentSessionScanError::ScanError {
            operation: AgentSessionScanOperation::ReadProjects,
            cause: json!("boom"),
        };
        assert_eq!(
            error.to_string(),
            "Failed to scan agent sessions during read-projects."
        );
        assert_eq!(
            serde_json::to_value(&error).unwrap()["_tag"],
            json!("AgentSessionScanError")
        );
    }
}
