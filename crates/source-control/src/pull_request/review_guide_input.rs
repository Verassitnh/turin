//! Port of upstream/apps/server/src/pullRequest/pullRequestReviewGuideInput.ts
//!
//! Also home of the guide-generation seam types upstream imports from
//! `@t3tools/provider-core/server/textGeneration` (`ReviewGuideGenerationInput`,
//! `ReviewGuideStackLayer`); the result is provider-core's and is re-exported here.

use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::pull_request::{PullRequestActivity, PullRequestDetail};

use crate::shared::js::js_trim;

/// `ReviewGuideStackLayer`: a layer of the stack the change is in, bottom first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewGuideStackLayer {
    pub number: u64,
    pub title: Option<String>,
    /// The layer's head in the checkout, `None` where it could not be fetched.
    pub head_sha: Option<String>,
    pub state: ReviewGuideStackLayerState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewGuideStackLayerState {
    Open,
    Closed,
    Merged,
}

/// `Omit<ReviewGuideGenerationInput, "cwd" | "modelSelection">`: the prompt input for a guide,
/// minus the checkout and model the caller provides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewGuideGenerationPrompt {
    pub number: u64,
    pub title: String,
    /// The author's own description, which states intent the diff cannot.
    pub description: String,
    pub base_branch: String,
    /// The commit the change is measured from, present in the checkout.
    pub base_commit: String,
    /// What review already settled: resolved conversations, reduced to who raised what. The guide
    /// leaves these alone rather than raising them again.
    pub settled_review: String,
    /// The maintainer's preferences for guides (tone, emphasis, ordering). `None` for none.
    pub instructions: Option<String>,
    /// The stack the change is a layer of, bottom first, when it is in one. Another layer's head
    /// is fetched into the checkout where the host served it, so the model can read across a seam.
    pub stack: Option<Vec<ReviewGuideStackLayer>>,
}

/// `ReviewGuideGenerationInput`: [`ReviewGuideGenerationPrompt`] plus the checkout and model.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewGuideGenerationInput {
    /// A disposable checkout of the change's head. The model investigates it read-only.
    pub cwd: String,
    pub number: u64,
    pub title: String,
    /// The author's own description, which states intent the diff cannot.
    pub description: String,
    pub base_branch: String,
    /// The commit the change is measured from, present in `cwd`.
    pub base_commit: String,
    /// What review already settled; see [`ReviewGuideGenerationPrompt::settled_review`].
    pub settled_review: String,
    /// The maintainer's preferences for guides (tone, emphasis, ordering). `None` for none.
    pub instructions: Option<String>,
    /// The stack the change is a layer of, bottom first, when it is in one.
    pub stack: Option<Vec<ReviewGuideStackLayer>>,
    /// What model and provider to use for generation.
    pub model_selection: ModelSelection,
}

impl ReviewGuideGenerationInput {
    pub fn from_prompt(
        prompt: ReviewGuideGenerationPrompt,
        cwd: String,
        model_selection: ModelSelection,
    ) -> Self {
        Self {
            cwd,
            number: prompt.number,
            title: prompt.title,
            description: prompt.description,
            base_branch: prompt.base_branch,
            base_commit: prompt.base_commit,
            settled_review: prompt.settled_review,
            instructions: prompt.instructions,
            stack: prompt.stack,
            model_selection,
        }
    }
}

/// `ReviewGuideGenerationResult` is provider-core's: the stops as the model wrote them, which
/// [`super::review_guide_normalize::normalize_review_guide`] checks and clamps before anything is stored.
pub use turin_provider_core::text_generation::ReviewGuideGenerationResult;

/// Longest a settled remark is quoted. The guide needs what was raised, not the argument.
const SETTLED_REMARK_CHARS: usize = 200;

fn one_line(text: &str) -> String {
    static COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<!--.*?-->").unwrap());
    static SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s\u{feff}]+").unwrap());
    let without_comments = COMMENT.replace_all(text, "");
    let spaced = SPACE.replace_all(&without_comments, " ");
    let flat = js_trim(&spaced);
    // `length` and `slice` count UTF-16 code units.
    if flat.encode_utf16().count() <= SETTLED_REMARK_CHARS {
        return flat.to_owned();
    }
    let mut units = 0;
    let mut end = 0;
    for (index, character) in flat.char_indices() {
        units += character.len_utf16();
        if units > SETTLED_REMARK_CHARS {
            break;
        }
        end = index + character.len_utf8();
    }
    format!("{}…", &flat[..end])
}

/// What review already settled, one line per conversation: resolved threads, and threads whose
/// line a later push removed. These are the review loop's finished work; the guide is told about
/// them so it does not raise them again.
pub fn settled_review_summary(activity: &PullRequestActivity) -> String {
    activity
        .review_threads
        .iter()
        .filter(|thread| thread.is_resolved || thread.is_outdated)
        .filter_map(|thread| {
            let opener = thread.comments.first()?;
            let who = opener
                .author
                .as_ref()
                .map_or("ghost", |author| author.login.as_str());
            let state = if thread.is_resolved {
                "resolved"
            } else {
                "outdated"
            };
            Some(format!(
                "- {} ({state}, {who}): {}",
                thread.path,
                one_line(&opener.body)
            ))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The prompt input for a guide, minus the checkout and model the caller provides.
pub fn review_guide_input_from_pull_request(
    detail: &PullRequestDetail,
    activity: &PullRequestActivity,
    base_commit: &str,
) -> ReviewGuideGenerationPrompt {
    ReviewGuideGenerationPrompt {
        number: detail.number.get(),
        title: detail.title.to_string(),
        description: detail.body.clone(),
        base_branch: detail.base_branch.to_string(),
        base_commit: base_commit.to_owned(),
        settled_review: settled_review_summary(activity),
        instructions: None,
        stack: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn thread(
        id: &str,
        path: &str,
        is_resolved: bool,
        is_outdated: bool,
        login: &str,
        body: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "path": path,
            "line": 1,
            "side": "right",
            "isResolved": is_resolved,
            "isOutdated": is_outdated,
            "comments": [{
                "id": "c1",
                "author": { "login": login, "name": null, "avatarUrl": null },
                "body": body,
                "createdAt": "2026-10-01T00:00:00.000Z",
                "url": null,
            }],
        })
    }

    fn activity(threads: Vec<serde_json::Value>) -> PullRequestActivity {
        serde_json::from_value(serde_json::json!({
            "comments": [],
            "commentCount": 0,
            "commentsTruncated": false,
            "reviewThreads": threads,
            "commits": [],
        }))
        .unwrap()
    }

    #[test]
    fn lists_resolved_and_outdated_threads_and_leaves_open_ones_for_the_guide() {
        let bot = "macroscope[bot]";
        let summary = settled_review_summary(&activity(vec![
            thread("open", "src/open.ts", false, false, bot, "Still open"),
            thread(
                "resolved",
                "src/a.ts",
                true,
                false,
                bot,
                "Possible null dereference",
            ),
            thread(
                "outdated",
                "src/b.ts",
                false,
                true,
                "julius",
                "Rename this <!-- bot:meta -->\n\nplease",
            ),
        ]));
        assert_eq!(
            summary,
            [
                "- src/a.ts (resolved, macroscope[bot]): Possible null dereference",
                "- src/b.ts (outdated, julius): Rename this please",
            ]
            .join("\n")
        );
    }

    #[test]
    fn quotes_a_long_remark_up_to_200_characters() {
        let long = "x".repeat(250);
        let summary = settled_review_summary(&activity(vec![thread(
            "t", "a.ts", true, false, "julius", &long,
        )]));
        assert_eq!(
            summary,
            format!("- a.ts (resolved, julius): {}…", "x".repeat(200))
        );
    }

    #[test]
    fn names_an_unknown_author_ghost_and_skips_threads_without_comments() {
        let mut ghost = thread("g", "a.ts", true, false, "x", "hello");
        ghost["comments"][0]["author"] = serde_json::Value::Null;
        let mut empty = thread("e", "b.ts", true, false, "x", "hello");
        empty["comments"] = serde_json::json!([]);
        assert_eq!(
            settled_review_summary(&activity(vec![ghost, empty])),
            "- a.ts (resolved, ghost): hello"
        );
    }
}
