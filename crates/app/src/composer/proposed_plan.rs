//! Port of upstream/apps/web/src/proposedPlan.ts (+ test).
//!
//! The plan follow-up helpers: the implementation prompt, which interaction
//! mode a follow-up submits in, the implementation thread's title, and the
//! exported markdown's filename and body.
//!
//! `buildCollapsedProposedPlanPreviewMarkdown` lives with the card that shows
//! it, as `components::chat::timeline_message_rows::build_collapsed_plan_preview`.
//! `downloadPlanAsTextFile` (a `Blob` and an anchor click) has no native
//! counterpart; saving a plan waits on the file dialogs (PORTING.md).

use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::base_schemas::{js_trim, js_trim_end};
use turin_shared::proposed_plan_text::proposed_plan_title;

use crate::composer::view_model::InteractionMode;

/// Characters `sanitizePlanFileSegment` drops before collapsing the rest.
static PUNCTUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[`'".,!?()\[\]{}]+"#).expect("static regex"));
static NON_ALPHANUMERIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-z0-9]+").expect("static regex"));

fn sanitize_plan_file_segment(input: &str) -> String {
    let lowered = input.to_lowercase();
    let without_punctuation = PUNCTUATION.replace_all(&lowered, "");
    let dashed = NON_ALPHANUMERIC.replace_all(&without_punctuation, "-");
    let sanitized = dashed.trim_matches('-');
    if sanitized.is_empty() {
        "plan".to_owned()
    } else {
        sanitized.to_owned()
    }
}

/// Prefix of the message the app sends when the user approves a plan.
pub const PLAN_IMPLEMENTATION_PROMPT_PREFIX: &str = "PLEASE IMPLEMENT THIS PLAN:\n";

pub fn build_plan_implementation_prompt(plan_markdown: &str) -> String {
    format!(
        "{PLAN_IMPLEMENTATION_PROMPT_PREFIX}{}",
        js_trim(plan_markdown)
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFollowUpSubmission {
    pub text: String,
    pub interaction_mode: InteractionMode,
}

pub fn resolve_plan_follow_up_submission(
    draft_text: &str,
    plan_markdown: &str,
) -> PlanFollowUpSubmission {
    let trimmed_draft_text = js_trim(draft_text);
    if !trimmed_draft_text.is_empty() {
        return PlanFollowUpSubmission {
            text: trimmed_draft_text.to_owned(),
            interaction_mode: InteractionMode::Plan,
        };
    }

    PlanFollowUpSubmission {
        text: build_plan_implementation_prompt(plan_markdown),
        interaction_mode: InteractionMode::Default,
    }
}

pub fn build_plan_implementation_thread_title(plan_markdown: &str) -> String {
    match proposed_plan_title(plan_markdown) {
        Some(title) => format!("Implement {title}"),
        None => "Implement plan".to_owned(),
    }
}

pub fn build_proposed_plan_markdown_filename(plan_markdown: &str) -> String {
    let title = proposed_plan_title(plan_markdown);
    format!(
        "{}.md",
        sanitize_plan_file_segment(title.as_deref().unwrap_or("plan"))
    )
}

pub fn normalize_plan_markdown_for_export(plan_markdown: &str) -> String {
    format!("{}\n", js_trim_end(plan_markdown))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::chat::timeline_message_rows::build_collapsed_plan_preview;

    #[test]
    fn formats_the_plan_exactly_like_the_codex_follow_up_handoff_prompt() {
        assert_eq!(
            build_plan_implementation_prompt("## Ship it\n\n- step 1\n"),
            "PLEASE IMPLEMENT THIS PLAN:\n## Ship it\n\n- step 1"
        );
    }

    #[test]
    fn collapsed_preview_drops_the_redundant_title_heading_and_preserves_following_lines() {
        assert_eq!(
            build_collapsed_plan_preview("# Integrate RPC\n\n## Summary\n\n- step 1\n- step 2", 4),
            "- step 1\n- step 2"
        );
    }

    #[test]
    fn collapsed_preview_appends_an_overflow_marker_when_it_truncates() {
        assert_eq!(
            build_collapsed_plan_preview("# Integrate RPC\n\n- step 1\n- step 2\n- step 3", 2),
            "- step 1\n- step 2\n\n..."
        );
    }

    #[test]
    fn keeps_context_bearing_feedback_in_plan_mode() {
        for prose in ["", "Consider this "] {
            let draft_text = format!("{prose}[PR #42](t3-context://v1/review-comment/pr-42)");
            assert_eq!(
                resolve_plan_follow_up_submission(&draft_text, "# Plan"),
                PlanFollowUpSubmission {
                    text: draft_text.clone(),
                    interaction_mode: InteractionMode::Plan,
                },
                "prose {prose:?}"
            );
        }
    }

    #[test]
    fn switches_to_default_mode_when_implementing_the_ready_plan_without_extra_text() {
        assert_eq!(
            resolve_plan_follow_up_submission("   ", "## Ship it\n\n- step 1\n"),
            PlanFollowUpSubmission {
                text: "PLEASE IMPLEMENT THIS PLAN:\n## Ship it\n\n- step 1".to_owned(),
                interaction_mode: InteractionMode::Default,
            }
        );
    }

    #[test]
    fn stays_in_plan_mode_when_the_user_adds_a_follow_up_prompt() {
        assert_eq!(
            resolve_plan_follow_up_submission("Refine step 2 first", "## Ship it\n\n- step 1\n"),
            PlanFollowUpSubmission {
                text: "Refine step 2 first".to_owned(),
                interaction_mode: InteractionMode::Plan,
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
    fn sanitizing_drops_punctuation_and_collapses_separators() {
        assert_eq!(
            sanitize_plan_file_segment("`Fix` the (bug), it's -- done!"),
            "fix-the-bug-its-done"
        );
        assert_eq!(sanitize_plan_file_segment("???"), "plan");
    }

    #[test]
    fn export_ends_with_exactly_one_newline() {
        assert_eq!(
            normalize_plan_markdown_for_export("# Plan  \n\n"),
            "# Plan\n"
        );
        assert_eq!(normalize_plan_markdown_for_export(""), "\n");
    }
}
