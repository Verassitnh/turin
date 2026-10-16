//! Port of upstream/apps/web/src/components/chat/composerSubmission.ts
//!
//! Prompt length validation and the send gate.

use turin_contracts::chat_attachment::PROVIDER_SEND_TURN_MAX_INPUT_CHARS;

use crate::components::chat::timeline::assistant_citations::expand_assistant_citations_for_provider;
use crate::composer::js_string::utf16_len;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionTarget {
    ProviderTurn,
    PendingUserInput,
}

pub struct ComposerSubmissionInput<'a> {
    pub prompt: &'a str,
    pub provider_input: Option<&'a str>,
    pub submission_target: SubmissionTarget,
}

/// `Number.prototype.toLocaleString("en-US")` for whole numbers.
pub fn format_en_us(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// `getComposerPromptLengthValidationMessage`.
pub fn get_composer_prompt_length_validation_message(prompt: &str) -> Option<String> {
    let normalized = turin_contracts::base_schemas::js_trim(prompt);
    // Upstream measures the longer of the prompt and its citation expansion:
    // each citation adds its quote block, which can outgrow the marker.
    let expanded = expand_assistant_citations_for_provider(normalized);
    let input_length = utf16_len(normalized).max(utf16_len(&expanded));
    let excess = input_length.checked_sub(PROVIDER_SEND_TURN_MAX_INPUT_CHARS)?;
    if excess == 0 {
        return None;
    }
    let label = if excess == 1 {
        "character"
    } else {
        "characters"
    };
    Some(format!(
        "Prompt is {} {label} over the {}-character limit. Shorten or split it before sending.",
        format_en_us(excess),
        format_en_us(PROVIDER_SEND_TURN_MAX_INPUT_CHARS)
    ))
}

/// `getComposerSubmissionValidationMessage`.
pub fn get_composer_submission_validation_message(
    input: &ComposerSubmissionInput<'_>,
) -> Option<String> {
    match input.submission_target {
        SubmissionTarget::ProviderTurn => get_composer_prompt_length_validation_message(
            input.provider_input.unwrap_or(input.prompt),
        ),
        SubmissionTarget::PendingUserInput => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitResult {
    pub validation_message: Option<String>,
    pub did_dispatch: bool,
}

/// `submitComposerDraft`. `on_send` returns `false` when the send boundary
/// rejected the draft; `prevent_default` is called when the event should not
/// proceed.
pub fn submit_composer_draft(
    input: &ComposerSubmissionInput<'_>,
    mut prevent_default: impl FnMut(),
    on_send: impl FnOnce() -> bool,
) -> SubmitResult {
    if let Some(message) = get_composer_submission_validation_message(input) {
        prevent_default();
        return SubmitResult {
            validation_message: Some(message),
            did_dispatch: false,
        };
    }
    if !on_send() {
        prevent_default();
        return SubmitResult {
            validation_message: None,
            did_dispatch: false,
        };
    }
    SubmitResult {
        validation_message: None,
        did_dispatch: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    const LIMIT: usize = PROVIDER_SEND_TURN_MAX_INPUT_CHARS;

    fn input<'a>(
        prompt: &'a str,
        provider_input: Option<&'a str>,
        target: SubmissionTarget,
    ) -> ComposerSubmissionInput<'a> {
        ComposerSubmissionInput {
            prompt,
            provider_input,
            submission_target: target,
        }
    }

    #[test]
    fn keeps_an_oversized_draft_editable_and_sends_a_corrected_follow_up() {
        let draft = "x".repeat(LIMIT + 1);
        let dispatched: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let prevented = Cell::new(0);
        let result = submit_composer_draft(
            &input(&draft, None, SubmissionTarget::ProviderTurn),
            || prevented.set(prevented.get() + 1),
            || {
                dispatched.borrow_mut().push(draft.clone());
                true
            },
        );
        assert_eq!(dispatched.borrow().len(), 0);
        assert_eq!(
            result.validation_message.as_deref(),
            Some(
                "Prompt is 1 character over the 120,000-character limit. Shorten or split it before sending."
            )
        );
        assert_eq!(prevented.get(), 1);
        let result = submit_composer_draft(
            &input("Corrected prompt", None, SubmissionTarget::ProviderTurn),
            || {},
            || true,
        );
        assert_eq!(
            result,
            SubmitResult {
                validation_message: None,
                did_dispatch: true
            }
        );
    }

    #[test]
    fn allows_a_draft_at_the_shared_limit_and_surrounding_whitespace() {
        let draft = "x".repeat(LIMIT);
        let prevented = Cell::new(false);
        let result = submit_composer_draft(
            &input(&draft, None, SubmissionTarget::ProviderTurn),
            || prevented.set(true),
            || true,
        );
        assert!(result.did_dispatch);
        assert!(!prevented.get());
        let padded = format!("  {}  ", "x".repeat(LIMIT));
        assert!(get_composer_prompt_length_validation_message(&padded).is_none());
    }

    #[test]
    fn measures_the_citation_expansion_when_it_is_longer() {
        use crate::components::chat::timeline::assistant_citations::{
            AssistantCitation, expand_assistant_citations_for_provider,
            serialize_assistant_citation,
        };
        // Each distinct short marker expands to a quote block several times its size.
        let prompt = (0..500)
            .map(|index| {
                serialize_assistant_citation(&AssistantCitation {
                    version: 1,
                    environment_id: "e".into(),
                    thread_id: "t".into(),
                    message_id: format!("m{index}"),
                    text: "a".into(),
                    comment: None,
                    start: 0,
                    end: 1,
                    prefix: String::new(),
                    suffix: String::new(),
                })
            })
            .collect::<Vec<_>>()
            .join("\n");
        let raw = utf16_len(&prompt);
        let expanded = utf16_len(&expand_assistant_citations_for_provider(&prompt));
        assert!(
            raw <= LIMIT && expanded > LIMIT,
            "raw {raw}, expanded {expanded}"
        );
        assert!(get_composer_prompt_length_validation_message(&prompt).is_some());
    }

    #[test]
    fn blocks_when_appended_provider_input_pushes_over_the_limit() {
        let draft = "x".repeat(LIMIT);
        let provider_input = format!("{draft}more");
        let message = get_composer_submission_validation_message(&input(
            &draft,
            Some(&provider_input),
            SubmissionTarget::ProviderTurn,
        ));
        assert_eq!(
            message.as_deref(),
            Some(
                "Prompt is 4 characters over the 120,000-character limit. Shorten or split it before sending."
            )
        );
        assert!(
            get_composer_submission_validation_message(&input(
                &draft,
                None,
                SubmissionTarget::ProviderTurn
            ))
            .is_none()
        );
    }

    #[test]
    fn does_not_finish_submission_when_the_send_boundary_rejects() {
        let prevented = Cell::new(0);
        let result = submit_composer_draft(
            &input("hi", None, SubmissionTarget::ProviderTurn),
            || prevented.set(prevented.get() + 1),
            || false,
        );
        assert_eq!(
            result,
            SubmitResult {
                validation_message: None,
                did_dispatch: false
            }
        );
        assert_eq!(prevented.get(), 1);
    }

    #[test]
    fn pending_user_input_answers_skip_the_provider_limit() {
        let answer = "x".repeat(LIMIT + 10);
        let result = submit_composer_draft(
            &input(&answer, None, SubmissionTarget::PendingUserInput),
            || {},
            || true,
        );
        assert_eq!(
            result,
            SubmitResult {
                validation_message: None,
                did_dispatch: true
            }
        );
    }

    #[test]
    fn formats_en_us_thousands() {
        assert_eq!(format_en_us(0), "0");
        assert_eq!(format_en_us(999), "999");
        assert_eq!(format_en_us(1000), "1,000");
        assert_eq!(format_en_us(1_234_567), "1,234,567");
    }
}
