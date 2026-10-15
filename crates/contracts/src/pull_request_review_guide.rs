//! Port of upstream/packages/contracts/src/pullRequestReviewGuide.ts
//!
//! A guided review is written for the maintainer deciding whether a change
//! should land, not for someone checking it line by line. So a guide is a
//! tour of stops in reading order (contract, then the service, then its
//! callers), with the mechanical rest grouped at the end. The model writes
//! the tour; the server holds it to the change.

use crate::base_schemas::{IsoDateTime, PositiveInt, TrimmedNonEmptyString};
use crate::model_selection::ModelSelection;
use serde::{Deserialize, Serialize};

/// The one call a maintainer should make at a stop. Not a bug or a review
/// nit; those belong to the review agents. The kind says what sort of
/// judgment it asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReviewGuideAttention {
    pub kind: PullRequestReviewGuideAttentionKind,
    pub note: TrimmedNonEmptyString,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestReviewGuideAttentionKind {
    Tradeoff,
    Contract,
    Product,
    Risk,
    Accessibility,
}

/// Lines worth reading, always inside the change. `old` shows removed code,
/// for a change that is mostly deletion; line numbers are on the side named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReviewGuideExcerpt {
    pub path: TrimmedNonEmptyString,
    pub side: PullRequestReviewGuideExcerptSide,
    pub start: PositiveInt,
    pub end: PositiveInt,
    pub caption: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PullRequestReviewGuideExcerptSide {
    New,
    Old,
}

/// What the code at one line does not say for itself: why it is shaped this
/// way, what it connects to. An explanation, not a finding. Anchored inside
/// one of its stop's excerpts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReviewGuideNote {
    pub path: TrimmedNonEmptyString,
    pub line: PositiveInt,
    pub title: TrimmedNonEmptyString,
    pub body: String,
}

/// A changed file listed by a stop's closing skim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReviewGuideFile {
    pub path: TrimmedNonEmptyString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReviewGuideStop {
    pub title: TrimmedNonEmptyString,
    pub summary: String,
    pub attention: Option<PullRequestReviewGuideAttention>,
    pub excerpts: Vec<PullRequestReviewGuideExcerpt>,
    /// Changed files the tour never excerpts, each listed by one stop. A
    /// file the tour does show may be excerpted by several stops, each
    /// showing the part that stop is about.
    pub files: Vec<PullRequestReviewGuideFile>,
    pub notes: Vec<PullRequestReviewGuideNote>,
    /// Questions about intent a reader might ask here, for a follow-up conversation.
    pub questions: Vec<TrimmedNonEmptyString>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReviewGuide {
    /// The head the guide was written against. A different head means the guide is stale.
    pub head_sha: TrimmedNonEmptyString,
    pub generated_at: IsoDateTime,
    pub model_selection: ModelSelection,
    pub stops: Vec<PullRequestReviewGuideStop>,
}

/// A guide run in progress or the last one that failed. A run reads the
/// change in a checkout and takes minutes on a large one, so it runs on the
/// server and the page watches this instead of holding a request open. A
/// server restart drops a run in progress; nothing claims it.
///
/// Upstream tags are the literal strings "Running" / "Failed" (capitalized).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag", rename_all_fields = "camelCase")]
pub enum PullRequestReviewGuideRun {
    #[serde(rename = "Running")]
    Running {
        head_sha: TrimmedNonEmptyString,
        started_at: IsoDateTime,
    },
    #[serde(rename = "Failed")]
    Failed {
        head_sha: TrimmedNonEmptyString,
        failed_at: IsoDateTime,
        detail: TrimmedNonEmptyString,
    },
}

/// The stored guide for a pull request and the head it is now at. `guide` is
/// null when none was generated; a guide whose `headSha` differs from
/// `currentHeadSha` was written before a push. `run` is the generation in
/// progress, or the last failed one; a finished run leaves only `guide`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReviewGuideResult {
    pub guide: Option<PullRequestReviewGuide>,
    pub run: Option<PullRequestReviewGuideRun>,
    /// Null where the host did not report a head with the detail.
    pub current_head_sha: Option<TrimmedNonEmptyString>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_keeps_upstream_capitalized_tags() {
        let running: PullRequestReviewGuideRun = serde_json::from_value(serde_json::json!({
            "_tag": "Running", "headSha": "abc", "startedAt": "2026-10-14T00:00:00.000Z"
        }))
        .unwrap();
        assert!(matches!(running, PullRequestReviewGuideRun::Running { .. }));
        let json = serde_json::to_value(&running).unwrap();
        assert_eq!(json["_tag"], "Running");

        let failed: PullRequestReviewGuideRun = serde_json::from_value(serde_json::json!({
            "_tag": "Failed", "headSha": "abc", "failedAt": "2026-10-14T00:00:00.000Z", "detail": "boom"
        }))
        .unwrap();
        assert!(matches!(failed, PullRequestReviewGuideRun::Failed { .. }));
    }

    #[test]
    fn result_decodes_with_empty_guide() {
        let result: PullRequestReviewGuideResult = serde_json::from_value(serde_json::json!({
            "guide": null,
            "run": null,
            "currentHeadSha": "def",
        }))
        .unwrap();
        assert!(result.guide.is_none());
        assert!(result.run.is_none());
        assert_eq!(result.current_head_sha.as_ref().unwrap().as_str(), "def");
    }
}
