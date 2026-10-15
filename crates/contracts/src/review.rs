//! Port of upstream/packages/contracts/src/review.ts

use crate::base_schemas::TrimmedNonEmptyString;
use crate::git::GitCommandError;
use crate::vcs::VcsError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiffPreviewInput {
    pub cwd: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<TrimmedNonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignore_whitespace: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<ReviewDiffPreviewInputFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiffPreviewInputFile {
    pub path: String,
    pub previous_path: Option<String>,
    pub source_kind: ReviewDiffPreviewSourceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewDiffPreviewSourceKind {
    WorkingTree,
    BranchRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiffFileStat {
    pub path: String,
    pub previous_path: Option<String>,
    pub additions: i64,
    pub deletions: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiffPreviewSource {
    pub id: TrimmedNonEmptyString,
    pub kind: ReviewDiffPreviewSourceKind,
    pub title: TrimmedNonEmptyString,
    pub base_ref: Option<TrimmedNonEmptyString>,
    pub head_ref: Option<TrimmedNonEmptyString>,
    pub diff: String,
    pub diff_hash: TrimmedNonEmptyString,
    pub truncated: bool,
    /// Complete statistics, independent of patch limits. Absent on older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<ReviewDiffFileStat>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewDiffFileChangeType {
    Change,
    RenamePure,
    RenameChanged,
    New,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiffFileContentsInput {
    pub cwd: TrimmedNonEmptyString,
    pub source_kind: ReviewDiffPreviewSourceKind,
    pub change_type: ReviewDiffFileChangeType,
    pub base_ref: Option<TrimmedNonEmptyString>,
    pub head_ref: Option<TrimmedNonEmptyString>,
    pub old_path: TrimmedNonEmptyString,
    pub new_path: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiffFileContentsResult {
    pub old_contents: String,
    pub new_contents: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiffPreviewResult {
    pub cwd: TrimmedNonEmptyString,
    /// `Schema.DateTimeUtc`: an ISO-8601 string.
    pub generated_at: String,
    pub sources: Vec<ReviewDiffPreviewSource>,
}

/// `ReviewDiffPreviewError`: union of the VCS and git command errors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(untagged)]
pub enum ReviewDiffPreviewError {
    #[error(transparent)]
    Vcs(VcsError),
    #[error(transparent)]
    GitCommand(GitCommandError),
}
