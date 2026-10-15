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

    use serde_json::{Value, json};

    fn guide() -> Value {
        json!({
            "headSha": "abc123",
            "generatedAt": "2026-10-14T00:00:00Z",
            "modelSelection": { "instanceId": "codex", "model": "gpt-5" },
            "stops": [{
                "title": "Contract",
                "summary": "",
                "attention": { "kind": "tradeoff", "note": "pick one" },
                "excerpts": [{
                    "path": "a.rs", "side": "old", "start": 1, "end": 3, "caption": "",
                }],
                "files": [{ "path": "b.rs" }, { "path": "c.rs", "note": "mechanical" }],
                "notes": [{ "path": "a.rs", "line": 2, "title": "why", "body": "because" }],
                "questions": ["is it needed?"],
            }],
        })
    }

    #[test]
    fn guide_round_trips_keeping_absent_file_notes_and_null_attention() {
        let decoded: PullRequestReviewGuide = serde_json::from_value(guide()).unwrap();
        // IsoDateTime passes through untouched.
        assert_eq!(decoded.generated_at, "2026-10-14T00:00:00Z");
        assert_eq!(serde_json::to_value(&decoded).unwrap(), guide());

        let mut no_attention = guide();
        no_attention["stops"][0]["attention"] = Value::Null;
        let decoded: PullRequestReviewGuide = serde_json::from_value(no_attention.clone()).unwrap();
        assert!(decoded.stops[0].attention.is_none());
        assert_eq!(serde_json::to_value(&decoded).unwrap(), no_attention);
    }

    #[test]
    fn guide_rejects_bad_literals_lines_and_blank_required_strings() {
        let bad = |edit: &dyn Fn(&mut Value)| {
            let mut value = guide();
            edit(&mut value);
            serde_json::from_value::<PullRequestReviewGuide>(value).is_err()
        };
        assert!(bad(&|v| v["stops"][0]["attention"]["kind"] = json!("bug")));
        assert!(bad(
            &|v| v["stops"][0]["excerpts"][0]["side"] = json!("both")
        ));
        assert!(bad(&|v| v["stops"][0]["excerpts"][0]["start"] = json!(0)));
        assert!(bad(&|v| v["stops"][0]["notes"][0]["line"] = json!(0)));
        assert!(bad(&|v| v["stops"][0]["title"] = json!("   ")));
        assert!(bad(&|v| v["headSha"] = json!("")));
    }

    #[test]
    fn run_rejects_unknown_tags_and_lowercase_tags() {
        for tag in ["running", "Cancelled"] {
            assert!(
                serde_json::from_value::<PullRequestReviewGuideRun>(json!({
                    "_tag": tag, "headSha": "abc", "startedAt": "t"
                }))
                .is_err()
            );
        }
        let failed = PullRequestReviewGuideRun::Failed {
            head_sha: TrimmedNonEmptyString::new("abc").unwrap(),
            failed_at: "2026-10-14T00:00:00Z".to_owned(),
            detail: TrimmedNonEmptyString::new("boom").unwrap(),
        };
        assert_eq!(
            serde_json::to_value(&failed).unwrap(),
            json!({"_tag": "Failed", "headSha": "abc", "failedAt": "2026-10-14T00:00:00Z", "detail": "boom"})
        );
    }

    #[test]
    fn result_round_trips_guide_run_and_null_head() {
        let value = json!({
            "guide": guide(),
            "run": { "_tag": "Running", "headSha": "abc", "startedAt": "t" },
            "currentHeadSha": null,
        });
        let decoded: PullRequestReviewGuideResult = serde_json::from_value(value.clone()).unwrap();
        assert!(decoded.current_head_sha.is_none());
        assert_eq!(serde_json::to_value(&decoded).unwrap(), value);
    }
}
