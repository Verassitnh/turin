//! Port of upstream/apps/web/src/proposedPlan.ts (the client helpers)
//!
//! `buildCollapsedProposedPlanPreviewMarkdown` lives with the plan card
//! (`timeline_message_rows::build_collapsed_plan_preview`); the rest is here: the implementation
//! prompt, the follow-up submission, the export filename and the download.

use std::path::{Path, PathBuf};

use super::timeline_message_rows::proposed_plan_title;
use crate::composer::prompt_history::PLAN_IMPLEMENTATION_PROMPT_PREFIX;

/// `buildPlanImplementationPrompt`.
pub fn build_plan_implementation_prompt(plan_markdown: &str) -> String {
    format!(
        "{PLAN_IMPLEMENTATION_PROMPT_PREFIX}{}",
        turin_contracts::base_schemas::js_trim(plan_markdown)
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanInteractionMode {
    Default,
    Plan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanFollowUpSubmission {
    pub text: String,
    pub interaction_mode: PlanInteractionMode,
}

/// `resolvePlanFollowUpSubmission`: feedback keeps the thread in plan mode; an empty draft
/// implements the plan.
pub fn resolve_plan_follow_up_submission(
    draft_text: &str,
    plan_markdown: &str,
) -> PlanFollowUpSubmission {
    let trimmed = turin_contracts::base_schemas::js_trim(draft_text);
    if !trimmed.is_empty() {
        return PlanFollowUpSubmission {
            text: trimmed.to_owned(),
            interaction_mode: PlanInteractionMode::Plan,
        };
    }
    PlanFollowUpSubmission {
        text: build_plan_implementation_prompt(plan_markdown),
        interaction_mode: PlanInteractionMode::Default,
    }
}

/// `buildPlanImplementationThreadTitle`.
pub fn build_plan_implementation_thread_title(plan_markdown: &str) -> String {
    match proposed_plan_title(plan_markdown) {
        Some(title) => format!("Implement {title}"),
        None => "Implement plan".to_owned(),
    }
}

/// `sanitizePlanFileSegment`.
fn sanitize_plan_file_segment(input: &str) -> String {
    let lowered = input.to_lowercase();
    let without_punctuation: String = lowered
        .chars()
        .filter(|ch| !"`'\".,!?()[]{}".contains(*ch))
        .collect();
    let mut sanitized = String::new();
    let mut pending_dash = false;
    for ch in without_punctuation.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            if pending_dash && !sanitized.is_empty() {
                sanitized.push('-');
            }
            pending_dash = false;
            sanitized.push(ch);
        } else {
            pending_dash = true;
        }
    }
    if sanitized.is_empty() {
        "plan".to_owned()
    } else {
        sanitized
    }
}

/// `buildProposedPlanMarkdownFilename`.
pub fn build_proposed_plan_markdown_filename(plan_markdown: &str) -> String {
    let title = proposed_plan_title(plan_markdown);
    format!(
        "{}.md",
        sanitize_plan_file_segment(title.as_deref().unwrap_or("plan"))
    )
}

/// `normalizePlanMarkdownForExport`.
pub fn normalize_plan_markdown_for_export(plan_markdown: &str) -> String {
    format!(
        "{}\n",
        turin_contracts::base_schemas::js_trim_end(plan_markdown)
    )
}

/// `downloadPlanAsTextFile`: the browser saves into Downloads; the app does the same, picking
/// a name that does not overwrite an earlier download.
pub fn download_plan_as_text_file(
    downloads_dir: &Path,
    filename: &str,
    contents: &str,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(downloads_dir)?;
    let (stem, extension) = match filename.rsplit_once('.') {
        Some((stem, extension)) => (stem, format!(".{extension}")),
        None => (filename, String::new()),
    };
    let mut path = downloads_dir.join(filename);
    let mut copy = 1;
    while path.exists() {
        path = downloads_dir.join(format!("{stem} ({copy}){extension}"));
        copy += 1;
    }
    std::fs::write(&path, contents)?;
    Ok(path)
}

/// The user's Downloads folder.
pub fn downloads_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Downloads"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_the_plan_exactly_like_the_codex_follow_up_handoff_prompt() {
        assert_eq!(
            build_plan_implementation_prompt("## Ship it\n\n- step 1\n"),
            "PLEASE IMPLEMENT THIS PLAN:\n## Ship it\n\n- step 1"
        );
    }

    #[test]
    fn keeps_context_bearing_feedback_in_plan_mode() {
        for prose in ["", "Consider this "] {
            let draft = format!("{prose}[PR #42](t3-context://v1/review-comment/pr-42)");
            assert_eq!(
                resolve_plan_follow_up_submission(&draft, "# Plan"),
                PlanFollowUpSubmission {
                    text: draft.trim().to_owned(),
                    interaction_mode: PlanInteractionMode::Plan,
                }
            );
        }
    }

    #[test]
    fn switches_to_default_mode_when_implementing_the_ready_plan_without_extra_text() {
        assert_eq!(
            resolve_plan_follow_up_submission("   ", "## Ship it\n\n- step 1\n"),
            PlanFollowUpSubmission {
                text: "PLEASE IMPLEMENT THIS PLAN:\n## Ship it\n\n- step 1".into(),
                interaction_mode: PlanInteractionMode::Default,
            }
        );
    }

    #[test]
    fn stays_in_plan_mode_when_the_user_adds_a_follow_up_prompt() {
        assert_eq!(
            resolve_plan_follow_up_submission("Refine step 2 first", "## Ship it\n\n- step 1\n"),
            PlanFollowUpSubmission {
                text: "Refine step 2 first".into(),
                interaction_mode: PlanInteractionMode::Plan,
            }
        );
    }

    #[test]
    fn uses_the_plan_heading_when_building_the_implementation_thread_title() {
        assert_eq!(
            build_plan_implementation_thread_title("# Integrate RPC\n\nBody"),
            "Implement Integrate RPC"
        );
    }

    #[test]
    fn falls_back_when_the_plan_has_no_markdown_heading() {
        assert_eq!(
            build_plan_implementation_thread_title("- step 1"),
            "Implement plan"
        );
    }

    #[test]
    fn derives_a_stable_markdown_filename_from_the_plan_heading() {
        assert_eq!(
            build_proposed_plan_markdown_filename("# Integrate Effect RPC Into Server App"),
            "integrate-effect-rpc-into-server-app.md"
        );
    }

    #[test]
    fn falls_back_to_a_generic_filename_when_the_plan_has_no_heading() {
        assert_eq!(build_proposed_plan_markdown_filename("- step 1"), "plan.md");
    }

    #[test]
    fn strips_quotes_and_punctuation_from_the_filename() {
        assert_eq!(
            build_proposed_plan_markdown_filename("# Fix `foo()`: it's \"broken\"!"),
            "fix-foo-its-broken.md"
        );
    }

    #[test]
    fn the_export_ends_with_exactly_one_newline() {
        assert_eq!(
            normalize_plan_markdown_for_export("# Plan\n\n\n"),
            "# Plan\n"
        );
        assert_eq!(normalize_plan_markdown_for_export("# Plan"), "# Plan\n");
    }

    #[test]
    fn a_download_never_overwrites_an_earlier_one() {
        let dir = std::env::temp_dir().join(format!("turin-plan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = download_plan_as_text_file(&dir, "plan.md", "one\n").unwrap();
        let second = download_plan_as_text_file(&dir, "plan.md", "two\n").unwrap();
        assert_eq!(first.file_name().unwrap(), "plan.md");
        assert_eq!(second.file_name().unwrap(), "plan (1).md");
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "one\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
