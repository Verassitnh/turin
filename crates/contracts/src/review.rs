//! Port of upstream/packages/contracts/src/review.ts

use crate::base_schemas::TrimmedNonEmptyString;
use crate::git::GitCommandError;
use crate::vcs::VcsError;
use serde::{Deserialize, Deserializer, Serialize};

/// `Schema.NonEmptyString`: non-empty, *not* trimmed.
mod non_empty {
    use super::*;

    fn check<E: serde::de::Error>(value: String) -> Result<String, E> {
        if value.is_empty() {
            Err(E::custom("expected a non-empty string"))
        } else {
            Ok(value)
        }
    }

    pub fn required<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
        check(String::deserialize(d)?)
    }

    pub fn optional<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
        Option::<String>::deserialize(d)?.map(check).transpose()
    }
}

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
    #[serde(deserialize_with = "non_empty::required")]
    pub path: String,
    #[serde(deserialize_with = "non_empty::optional")]
    pub previous_path: Option<String>,
    pub source_kind: ReviewDiffPreviewSourceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewDiffPreviewSourceKind {
    WorkingTree,
    BranchRange,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiffFileStat {
    pub path: String,
    pub previous_path: Option<String>,
    #[serde(with = "crate::base_schemas::js_number")]
    pub additions: f64,
    #[serde(with = "crate::base_schemas::js_number")]
    pub deletions: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiffPreviewResult {
    pub cwd: TrimmedNonEmptyString,
    pub generated_at: crate::base_schemas::UtcDateTime,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn round_trip<T: Serialize + serde::de::DeserializeOwned>(value: Value) -> T {
        let parsed: T = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), value);
        parsed
    }

    #[test]
    fn preview_input_optional_keys_and_non_empty_paths() {
        let minimal: ReviewDiffPreviewInput = round_trip(json!({ "cwd": "/repo" }));
        assert!(minimal.base_ref.is_none() && minimal.file.is_none());

        let full: ReviewDiffPreviewInput = round_trip(json!({
            "cwd": "/repo", "baseRef": "main", "ignoreWhitespace": true,
            "file": { "path": " a.rs", "previousPath": null, "sourceKind": "branch-range" },
        }));
        let file = full.file.unwrap();
        // NonEmptyString is not trimmed.
        assert_eq!(file.path, " a.rs");
        assert_eq!(file.previous_path, None);
        assert_eq!(file.source_kind, ReviewDiffPreviewSourceKind::BranchRange);

        let with_old: ReviewDiffPreviewInput = round_trip(json!({
            "cwd": "/repo",
            "file": { "path": "b", "previousPath": "a", "sourceKind": "working-tree" },
        }));
        assert_eq!(with_old.file.unwrap().previous_path.as_deref(), Some("a"));

        for bad in [
            json!({ "cwd": "/repo", "file": { "path": "", "previousPath": null, "sourceKind": "working-tree" } }),
            json!({ "cwd": "/repo", "file": { "path": "a", "previousPath": "", "sourceKind": "working-tree" } }),
            json!({ "cwd": "/repo", "file": { "path": "a", "previousPath": null, "sourceKind": "staged" } }),
            json!({ "cwd": "  " }),
            json!({ "cwd": "/repo", "baseRef": " " }),
        ] {
            assert!(
                serde_json::from_value::<ReviewDiffPreviewInput>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn preview_result_round_trips_with_whole_number_stats() {
        let value = json!({
            "cwd": "/repo",
            "generatedAt": "2026-10-14T00:00:00.000Z",
            "sources": [
                {
                    "id": "wt", "kind": "working-tree", "title": "Working tree",
                    "baseRef": null, "headRef": null, "diff": "", "diffHash": "abc",
                    "truncated": false,
                },
                {
                    "id": "br", "kind": "branch-range", "title": "Branch",
                    "baseRef": "main", "headRef": "feat", "diff": "@@", "diffHash": "def",
                    "truncated": true,
                    "files": [
                        { "path": "a", "previousPath": null, "additions": 3, "deletions": 0 },
                        { "path": "b", "previousPath": "c", "additions": 1.5, "deletions": 2 },
                    ],
                },
            ],
        });
        let result: ReviewDiffPreviewResult = round_trip(value);
        assert!(result.sources[0].files.is_none());
        assert_eq!(result.sources[1].files.as_ref().unwrap().len(), 2);

        // generatedAt normalises to the canonical UTC form.
        let normalised: ReviewDiffPreviewResult = serde_json::from_value(json!({
            "cwd": "/repo", "generatedAt": "2026-10-14T02:00:00+02:00", "sources": [],
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&normalised).unwrap()["generatedAt"],
            json!("2026-10-14T00:00:00.000Z")
        );
    }

    #[test]
    fn file_contents_input_and_result() {
        for change in ["change", "rename-pure", "rename-changed", "new", "deleted"] {
            let input: ReviewDiffFileContentsInput = round_trip(json!({
                "cwd": "/repo", "sourceKind": "working-tree", "changeType": change,
                "baseRef": null, "headRef": "HEAD", "oldPath": "a", "newPath": "b",
            }));
            assert_eq!(input.head_ref.unwrap().as_str(), "HEAD");
        }
        assert!(
            serde_json::from_value::<ReviewDiffFileContentsInput>(json!({
                "cwd": "/repo", "sourceKind": "working-tree", "changeType": "renamed",
                "baseRef": null, "headRef": null, "oldPath": "a", "newPath": "b",
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ReviewDiffFileContentsInput>(json!({
                "cwd": "/repo", "sourceKind": "working-tree", "changeType": "new",
                "baseRef": null, "headRef": null, "oldPath": "", "newPath": "b",
            }))
            .is_err()
        );
        let _: ReviewDiffFileContentsResult =
            round_trip(json!({ "oldContents": "", "newContents": " x " }));
    }

    #[test]
    fn preview_error_union_tries_vcs_then_git() {
        let vcs: ReviewDiffPreviewError = round_trip(json!({
            "_tag": "VcsUnsupportedOperationError", "operation": "o", "kind": "jj", "detail": "d",
        }));
        assert!(matches!(vcs, ReviewDiffPreviewError::Vcs(_)));
        let git: ReviewDiffPreviewError = round_trip(json!({
            "_tag": "GitCommandError", "operation": "o", "command": "git", "cwd": "/r",
            "detail": "d", "reason": "not_a_repository",
        }));
        assert!(matches!(git, ReviewDiffPreviewError::GitCommand(_)));
        assert!(
            serde_json::from_value::<ReviewDiffPreviewError>(json!({ "_tag": "Nope" })).is_err()
        );
    }
}
