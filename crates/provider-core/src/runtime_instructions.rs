//! Port of upstream/packages/provider-core/src/server/runtimeInstructions.ts.
//!
//! Prompt text is verbatim.

const PULL_REQUEST_LINKING_INSTRUCTIONS: &str = r#"<pull_request_linking>
When the t3-code MCP server exposes link_pull_request, you must use it to register every pull request you create or work on for this thread. Call link_pull_request with the full PR URL immediately after creating a PR or starting work on an existing PR. For a stack, call it for every layer, not just the current branch or the top PR. This applies when creating or updating PRs through gh, gh stack, another CLI, or the host API: those operations do not register the PRs with this thread. Linking an already-linked PR is safe. Before finishing PR work, call list_thread_pull_requests and link any PR from your work that is missing. Do not link unrelated PRs mentioned only as background. If a linking call fails, report that failure instead of claiming the PR is linked. When asked to monitor, watch, or babysit a PR and watch_pull_request is available, call it and end your turn: T3 Code wakes you when checks finish, someone else comments, or the branch conflicts, so do not poll or run your own watcher. When you hand the work back to the user, call unwatch_pull_request first so the thread returns to their inbox.
</pull_request_linking>"#;

/// Inputs to [`build_runtime_instructions`]; omit model and effort when the
/// harness manages them dynamically. `model_name` is the display name users
/// see in the model picker; `model` is the slug.
#[derive(Debug, Clone, Default)]
pub struct RuntimeInfo<'a> {
    pub harness: &'a str,
    pub model: Option<&'a str>,
    pub model_name: Option<&'a str>,
    pub reasoning_effort: Option<&'a str>,
}

pub fn build_runtime_instructions(runtime: &RuntimeInfo<'_>) -> String {
    let harness = to_single_line(runtime.harness);
    let model = to_single_line(runtime.model.unwrap_or(""));
    let model_name = to_single_line(runtime.model_name.unwrap_or(""));
    let effort = to_single_line(runtime.reasoning_effort.unwrap_or(""));
    let model_label = if !model_name.is_empty() && model_name != model {
        format!("{model_name} (model slug: {model})")
    } else {
        model.clone()
    };
    let model_info = if !model.is_empty() && model != "auto" && model != "default" {
        format!(", as {model_label}")
    } else {
        String::new()
    };
    let effort_info = if effort.is_empty() {
        String::new()
    } else {
        format!(" with {effort} reasoning effort")
    };
    format!(
        "<runtime_info>In case you're asked: you are running in T3 Code through the {harness} harness{model_info}{effort_info}. No need to mention this otherwise. You can embed images and videos in your response using Markdown with absolute file paths.</runtime_info>\n\n{PULL_REQUEST_LINKING_INSTRUCTIONS}"
    )
}

/// `value.replaceAll(/\s+/g, " ").trim()`.
fn to_single_line(value: &str) -> String {
    turin_contracts::js_trim(&crate::js::collapse_whitespace(value)).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info<'a>(harness: &'a str) -> RuntimeInfo<'a> {
        RuntimeInfo {
            harness,
            ..RuntimeInfo::default()
        }
    }

    #[test]
    fn requires_explicit_registration_of_every_pr_and_stack_layer() {
        let instructions = build_runtime_instructions(&info("Codex"));
        for needle in [
            "When the t3-code MCP server exposes link_pull_request",
            "with the full PR URL immediately after creating a PR",
            "For a stack, call it for every layer",
            "call list_thread_pull_requests and link any PR",
        ] {
            assert!(instructions.contains(needle), "{needle}");
        }
    }

    #[test]
    fn keeps_known_model_and_effort_metadata_on_one_line() {
        let instructions = build_runtime_instructions(&RuntimeInfo {
            harness: "Codex",
            model: Some("  custom\nmodel  "),
            reasoning_effort: Some(" high\n"),
            ..RuntimeInfo::default()
        });
        assert!(
            instructions
                .contains("through the Codex harness, as custom model with high reasoning effort.")
        );
    }

    #[test]
    fn names_the_model_by_display_name_and_slug_when_they_differ() {
        assert!(
            build_runtime_instructions(&RuntimeInfo {
                harness: "Codex",
                model: Some("gpt-5.4"),
                model_name: Some("GPT-5.4"),
                ..RuntimeInfo::default()
            })
            .contains("through the Codex harness, as GPT-5.4 (model slug: gpt-5.4).")
        );
        assert!(
            build_runtime_instructions(&RuntimeInfo {
                harness: "Codex",
                model: Some("my-model"),
                model_name: Some("my-model"),
                ..RuntimeInfo::default()
            })
            .contains("through the Codex harness, as my-model.")
        );
    }

    #[test]
    fn omits_unresolved_model() {
        for model in [None, Some(""), Some("auto"), Some("default")] {
            let instructions = build_runtime_instructions(&RuntimeInfo {
                harness: "Cursor",
                model,
                ..RuntimeInfo::default()
            });
            assert!(instructions.contains("through the Cursor harness."));
            assert!(!instructions.contains("reasoning effort"));
        }
    }
}
