//! Port of upstream/apps/web/src/pendingUserInput.ts
//!
//! Answer-draft logic for the pending user-input card: how a draft becomes an
//! answer, how option and custom selections replace each other, and the
//! per-question progress the card renders. Upstream keeps the drafts in the
//! composer draft store; here the caller owns the map and passes it in.

use std::collections::HashMap;

use turin_contracts::base_schemas::{js_trim, js_trim_end};
use turin_contracts::composer_context::ThreadContextRecord;
use turin_contracts::provider_runtime::{UserInputQuestion, UserInputQuestionOption};
use turin_shared::composer_context_references::replace_composer_context_references;
use turin_shared::thread_links::format_thread_link;

/// The resolved answer for one question: `string | string[]` upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingUserInputAnswer {
    Text(String),
    Choices(Vec<String>),
}

/// `PendingUserInputDraftAnswer`. Upstream's optional keys are `Option`s; an
/// absent key and an explicit `undefined` are the same here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingUserInputDraftAnswer {
    pub selected_option_values: Option<Vec<String>>,
    pub custom_answer: Option<String>,
    pub attachment_count: Option<usize>,
    pub attachments_blocked: Option<bool>,
}

/// `PendingUserInputProgress`. Owns copies of the active question and draft, as upstream's
/// snapshot does.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingUserInputProgress {
    pub question_index: usize,
    pub active_question: Option<UserInputQuestion>,
    pub active_draft: Option<PendingUserInputDraftAnswer>,
    pub selected_option_values: Vec<String>,
    pub custom_answer: String,
    pub resolved_answer: Option<PendingUserInputAnswer>,
    pub using_custom_answer: bool,
    pub answered_question_count: usize,
    pub is_last_question: bool,
    pub is_complete: bool,
    pub can_advance: bool,
}

/// Question responses carry text, so send thread chips as links with the actual thread IDs.
pub fn project_pending_user_input_answer(
    answer: PendingUserInputAnswer,
    thread_contexts: &[ThreadContextRecord],
) -> PendingUserInputAnswer {
    let text = match &answer {
        PendingUserInputAnswer::Text(text) if !thread_contexts.is_empty() => text,
        _ => return answer,
    };
    let records_by_id: HashMap<&str, &ThreadContextRecord> = thread_contexts
        .iter()
        .map(|record| (record.context_id.as_str(), record))
        .collect();
    PendingUserInputAnswer::Text(replace_composer_context_references(text, |reference| {
        let record = if reference.kind.as_str() == "thread" {
            records_by_id.get(reference.context_id.as_str()).copied()
        } else {
            None
        };
        match record {
            Some(record) => format_thread_link(record.thread_id.as_str(), &reference.label),
            None => reference.source.clone(),
        }
    }))
}

fn normalize_draft_answer(value: Option<&str>) -> Option<String> {
    let trimmed = js_trim(value?);
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Provider option IDs stay unchanged, including whitespace. Order is kept, duplicates dropped
/// (upstream builds a `Set`).
fn normalize_selected_option_values(value: Option<&[String]>) -> Vec<String> {
    let mut normalized: Vec<String> = Vec::new();
    for entry in value.unwrap_or(&[]) {
        if !normalized.contains(entry) {
            normalized.push(entry.clone());
        }
    }
    normalized
}

/// `option.value ?? option.label`: an empty `value` is still a value.
fn option_value(option: &UserInputQuestionOption) -> &str {
    option.value.as_deref().unwrap_or(option.label.as_str())
}

pub fn resolve_pending_user_input_answer(
    question: &UserInputQuestion,
    draft: Option<&PendingUserInputDraftAnswer>,
) -> Option<PendingUserInputAnswer> {
    if draft.is_some_and(|draft| draft.attachments_blocked == Some(true)) {
        return None;
    }
    let draft_custom = draft.and_then(|draft| draft.custom_answer.as_deref());
    // A question with an editable initial answer keeps the text exactly as typed, even when empty.
    let custom_answer = if question.allow_custom_answer == Some(false) {
        None
    } else if question.initial_answer.is_some() {
        draft_custom.map(str::to_string)
    } else {
        normalize_draft_answer(draft_custom)
    };
    if let Some(custom) = custom_answer.as_deref().filter(|custom| !custom.is_empty()) {
        return Some(PendingUserInputAnswer::Text(custom.to_string()));
    }

    let selected_option_values: Vec<String> = normalize_selected_option_values(
        draft.and_then(|draft| draft.selected_option_values.as_deref()),
    )
    .into_iter()
    .filter(|value| {
        question
            .options
            .iter()
            .any(|option| option_value(option) == value)
    })
    .collect();
    // Attachment-only answers resolve to an empty string once every upload has finished.
    let attachment_fallback = (question.allow_custom_answer != Some(false)
        && draft.and_then(|draft| draft.attachment_count).unwrap_or(0) > 0)
        .then(String::new);

    if question.multi_select == Some(true) {
        if !selected_option_values.is_empty() {
            return Some(PendingUserInputAnswer::Choices(selected_option_values));
        }
        return custom_answer
            .or(attachment_fallback)
            .map(PendingUserInputAnswer::Text);
    }

    if let Some(first) = selected_option_values.into_iter().next() {
        return Some(PendingUserInputAnswer::Text(first));
    }
    custom_answer
        .or(attachment_fallback)
        .map(PendingUserInputAnswer::Text)
}

/// Typing a custom answer clears the preset selection, because a non-empty custom answer
/// outranks selected options in [`resolve_pending_user_input_answer`]. Whitespace-only text
/// keeps the selection.
pub fn set_pending_user_input_custom_answer(
    draft: Option<&PendingUserInputDraftAnswer>,
    custom_answer: &str,
    question: Option<&UserInputQuestion>,
) -> PendingUserInputDraftAnswer {
    let has_initial_answer = question.is_some_and(|question| question.initial_answer.is_some());
    let selected_option_values = if has_initial_answer || !js_trim(custom_answer).is_empty() {
        Vec::new()
    } else {
        normalize_selected_option_values(
            draft.and_then(|draft| draft.selected_option_values.as_deref()),
        )
    };

    PendingUserInputDraftAnswer {
        custom_answer: Some(custom_answer.to_string()),
        selected_option_values: (!selected_option_values.is_empty())
            .then_some(selected_option_values),
        ..Default::default()
    }
}

const DISPLACED_ANSWER_SEPARATOR: &str = "\n\n";

/// Selecting an option replaces the custom answer. Text the user typed into the answer field
/// must not vanish on that click: it moves back into the thread draft, after whatever was
/// already waiting there.
pub fn carry_displaced_custom_answer_into_prompt(
    prompt: &str,
    custom_answer: Option<&str>,
) -> String {
    let displaced = js_trim(custom_answer.unwrap_or(""));
    if displaced.is_empty() {
        return prompt.to_string();
    }
    if js_trim(prompt).is_empty() {
        return displaced.to_string();
    }
    format!(
        "{}{DISPLACED_ANSWER_SEPARATOR}{displaced}",
        js_trim_end(prompt)
    )
}

pub fn toggle_pending_user_input_option_selection(
    question: &UserInputQuestion,
    draft: Option<&PendingUserInputDraftAnswer>,
    option_value: &str,
) -> PendingUserInputDraftAnswer {
    if question.multi_select == Some(true) {
        let mut selected = normalize_selected_option_values(
            draft.and_then(|draft| draft.selected_option_values.as_deref()),
        );
        if let Some(index) = selected.iter().position(|value| value == option_value) {
            selected.remove(index);
        } else {
            selected.push(option_value.to_string());
        }

        return PendingUserInputDraftAnswer {
            custom_answer: Some(String::new()),
            selected_option_values: (!selected.is_empty()).then_some(selected),
            ..Default::default()
        };
    }

    PendingUserInputDraftAnswer {
        custom_answer: Some(String::new()),
        selected_option_values: Some(vec![option_value.to_string()]),
        ..Default::default()
    }
}

/// A canonical answer map for every question, or `None` while any question is unanswered.
pub fn build_pending_user_input_answers(
    questions: &[UserInputQuestion],
    draft_answers: &HashMap<String, PendingUserInputDraftAnswer>,
) -> Option<HashMap<String, PendingUserInputAnswer>> {
    let mut answers = HashMap::new();
    for question in questions {
        let answer =
            resolve_pending_user_input_answer(question, draft_answers.get(question.id.as_str()))?;
        answers.insert(question.id.as_str().to_string(), answer);
    }
    Some(answers)
}

pub fn count_answered_pending_user_input_questions(
    questions: &[UserInputQuestion],
    draft_answers: &HashMap<String, PendingUserInputDraftAnswer>,
) -> usize {
    questions
        .iter()
        .filter(|question| {
            resolve_pending_user_input_answer(question, draft_answers.get(question.id.as_str()))
                .is_some()
        })
        .count()
}

/// The first unanswered question, or the last one when every question is answered.
pub fn find_first_unanswered_pending_user_input_question_index(
    questions: &[UserInputQuestion],
    draft_answers: &HashMap<String, PendingUserInputDraftAnswer>,
) -> usize {
    let unanswered_index = questions.iter().position(|question| {
        resolve_pending_user_input_answer(question, draft_answers.get(question.id.as_str()))
            .is_none()
    });

    unanswered_index.unwrap_or(questions.len().saturating_sub(1))
}

pub fn derive_pending_user_input_progress(
    questions: &[UserInputQuestion],
    draft_answers: &HashMap<String, PendingUserInputDraftAnswer>,
    question_index: usize,
) -> PendingUserInputProgress {
    let normalized_question_index = if questions.is_empty() {
        0
    } else {
        question_index.min(questions.len() - 1)
    };
    let active_question = questions.get(normalized_question_index);
    let active_draft = active_question.and_then(|question| draft_answers.get(question.id.as_str()));
    let resolved_answer = active_question
        .and_then(|question| resolve_pending_user_input_answer(question, active_draft));
    let custom_answer =
        if active_question.is_some_and(|question| question.allow_custom_answer == Some(false)) {
            String::new()
        } else {
            active_draft
                .and_then(|draft| draft.custom_answer.clone())
                .unwrap_or_default()
        };
    let has_initial_answer =
        active_question.is_some_and(|question| question.initial_answer.is_some());
    let custom_has_text = if has_initial_answer {
        !custom_answer.is_empty()
    } else {
        !js_trim(&custom_answer).is_empty()
    };

    PendingUserInputProgress {
        question_index: normalized_question_index,
        active_question: active_question.cloned(),
        active_draft: active_draft.cloned(),
        selected_option_values: normalize_selected_option_values(
            active_draft.and_then(|draft| draft.selected_option_values.as_deref()),
        ),
        custom_answer,
        can_advance: resolved_answer.is_some(),
        resolved_answer,
        using_custom_answer: custom_has_text,
        answered_question_count: count_answered_pending_user_input_questions(
            questions,
            draft_answers,
        ),
        is_last_question: questions.is_empty() || normalized_question_index + 1 >= questions.len(),
        is_complete: build_pending_user_input_answers(questions, draft_answers).is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use turin_shared::composer_context_references::format_composer_context_reference;

    fn question(value: Value) -> UserInputQuestion {
        serde_json::from_value(value).expect("question fixture")
    }

    fn draft(value: Value) -> PendingUserInputDraftAnswer {
        let selected = value["selectedOptionValues"].as_array().map(|values| {
            values
                .iter()
                .map(|value| value.as_str().expect("option value").to_string())
                .collect()
        });
        PendingUserInputDraftAnswer {
            selected_option_values: selected,
            custom_answer: value["customAnswer"].as_str().map(str::to_string),
            attachment_count: value["attachmentCount"]
                .as_u64()
                .map(|count| count as usize),
            attachments_blocked: value["attachmentsBlocked"].as_bool(),
        }
    }

    fn text(value: &str) -> PendingUserInputAnswer {
        PendingUserInputAnswer::Text(value.to_string())
    }

    fn choices(values: &[&str]) -> PendingUserInputAnswer {
        PendingUserInputAnswer::Choices(values.iter().map(|value| value.to_string()).collect())
    }

    fn draft_map(entries: Vec<(&str, Value)>) -> HashMap<String, PendingUserInputDraftAnswer> {
        entries
            .into_iter()
            .map(|(id, value)| (id.to_string(), draft(value)))
            .collect()
    }

    fn single_select_question() -> UserInputQuestion {
        question(json!({
            "id": "scope",
            "header": "Scope",
            "question": "What should the plan target first?",
            "options": [
                { "label": "Orchestration-first", "description": "Focus on orchestration first" }
            ],
            "multiSelect": false,
        }))
    }

    fn multi_select_question() -> UserInputQuestion {
        question(json!({
            "id": "areas",
            "header": "Areas",
            "question": "Which areas should this change cover?",
            "options": [
                { "label": "Server", "description": "Server" },
                { "label": "Web", "description": "Web" },
            ],
            "multiSelect": true,
        }))
    }

    fn native_choice_question() -> UserInputQuestion {
        question(json!({
            "id": "result",
            "header": "Result",
            "question": "Which result should be used?",
            "options": [
                { "value": " first\t", "label": "Result", "description": "First result" },
                { "value": "second", "label": "Result", "description": "Second result" },
            ],
            "allowCustomAnswer": false,
            "multiSelect": false,
        }))
    }

    fn compat_question() -> UserInputQuestion {
        question(json!({
            "id": "compat",
            "header": "Compat",
            "question": "How strict should compatibility be?",
            "options": [
                { "label": "Keep current envelope", "description": "Preserve current wire format" }
            ],
            "multiSelect": false,
        }))
    }

    fn thread_context_record(
        context_id: &str,
        thread_id: &str,
        label: &str,
    ) -> ThreadContextRecord {
        serde_json::from_value(json!({
            "version": 1,
            "kind": "thread",
            "contextId": context_id,
            "label": label,
            "environmentId": "env-answer",
            "threadId": thread_id,
            "title": label,
        }))
        .expect("thread context fixture")
    }

    #[test]
    fn projects_thread_chips_as_readable_links_with_the_actual_thread_id() {
        let record = thread_context_record(
            "thread_reference-thread",
            "reference-thread",
            "Reference work",
        );
        let answer = format!(
            "Compare {}",
            format_composer_context_reference(
                "thread",
                "thread_reference-thread",
                "Reference work"
            )
        );
        assert_eq!(
            project_pending_user_input_answer(text(&answer), &[record]),
            text("Compare [Reference work](t3-thread://v1/reference-thread)")
        );
    }

    #[test]
    fn leaves_plain_answers_unchanged_when_the_draft_has_unreferenced_thread_records() {
        let record = thread_context_record(
            "thread_reference-thread",
            "reference-thread",
            "Reference work",
        );
        assert_eq!(
            project_pending_user_input_answer(text("Plain answer"), &[record]),
            text("Plain answer")
        );
    }

    #[test]
    fn preserves_option_values_and_exact_answer_text() {
        let record = thread_context_record(
            "thread_reference-thread",
            "reference-thread",
            "Reference work",
        );
        let options = choices(&[" first\t", "second"]);
        assert_eq!(
            project_pending_user_input_answer(options.clone(), std::slice::from_ref(&record)),
            options
        );
        assert_eq!(
            project_pending_user_input_answer(text("  Edited answer\n"), &[record]),
            text("  Edited answer\n")
        );
    }

    #[test]
    fn preserves_exact_editor_text_including_a_deliberately_cleared_answer() {
        let mut question = single_select_question();
        question.initial_answer = Some("  Proposed message\n".to_string());
        let initial = question.initial_answer.clone().unwrap();
        assert_eq!(
            resolve_pending_user_input_answer(
                &question,
                Some(&PendingUserInputDraftAnswer {
                    custom_answer: Some(initial.clone()),
                    ..Default::default()
                })
            ),
            Some(text(&initial))
        );
        assert_eq!(
            resolve_pending_user_input_answer(
                &question,
                Some(&PendingUserInputDraftAnswer {
                    custom_answer: Some("  Edited message\n\n".to_string()),
                    ..Default::default()
                })
            ),
            Some(text("  Edited message\n\n"))
        );
        let cleared = PendingUserInputDraftAnswer {
            custom_answer: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(
            resolve_pending_user_input_answer(&question, Some(&cleared)),
            Some(text(""))
        );
        assert_eq!(
            resolve_pending_user_input_answer(
                &question,
                Some(&PendingUserInputDraftAnswer {
                    custom_answer: Some(" \n".to_string()),
                    ..Default::default()
                })
            ),
            Some(text(" \n"))
        );
        assert_eq!(
            set_pending_user_input_custom_answer(
                Some(&PendingUserInputDraftAnswer {
                    selected_option_values: Some(vec!["Orchestration-first".to_string()]),
                    ..Default::default()
                }),
                "",
                Some(&question),
            ),
            PendingUserInputDraftAnswer {
                custom_answer: Some(String::new()),
                ..Default::default()
            }
        );
        let second = question_with_id("follow-up", &single_select_question());
        let questions = vec![question.clone(), second];
        let draft_answers = draft_map(vec![("scope", json!({ "customAnswer": "" }))]);
        assert_eq!(
            find_first_unanswered_pending_user_input_question_index(&questions, &draft_answers),
            1
        );
        let progress =
            derive_pending_user_input_progress(std::slice::from_ref(&question), &draft_answers, 0);
        assert_eq!(progress.custom_answer, "");
        assert_eq!(progress.resolved_answer, Some(text("")));
        assert!(progress.can_advance);
        assert!(progress.is_complete);
    }

    fn question_with_id(id: &str, base: &UserInputQuestion) -> UserInputQuestion {
        let mut value = serde_json::to_value(base).expect("question json");
        value["id"] = json!(id);
        serde_json::from_value(value).expect("question fixture")
    }

    #[test]
    fn prefers_a_custom_answer_over_selected_options() {
        assert_eq!(
            resolve_pending_user_input_answer(
                &single_select_question(),
                Some(&draft(json!({
                    "selectedOptionValues": ["Orchestration-first"],
                    "customAnswer": "Keep the existing envelope for one release",
                }))),
            ),
            Some(text("Keep the existing envelope for one release"))
        );
    }

    #[test]
    fn falls_back_to_the_selected_option_for_single_select_questions() {
        assert_eq!(
            resolve_pending_user_input_answer(
                &single_select_question(),
                Some(&draft(
                    json!({ "selectedOptionValues": ["Orchestration-first"] })
                )),
            ),
            Some(text("Orchestration-first"))
        );
    }

    #[test]
    fn returns_all_selected_labels_for_multi_select_questions() {
        assert_eq!(
            resolve_pending_user_input_answer(
                &multi_select_question(),
                Some(&draft(json!({ "selectedOptionValues": ["Server", "Web"] }))),
            ),
            Some(choices(&["Server", "Web"]))
        );
    }

    #[test]
    fn clears_the_preset_selection_when_a_custom_answer_is_entered() {
        assert_eq!(
            set_pending_user_input_custom_answer(
                Some(&draft(json!({ "selectedOptionValues": ["Server", "Web"] }))),
                "doesn't matter",
                None,
            ),
            PendingUserInputDraftAnswer {
                custom_answer: Some("doesn't matter".to_string()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn does_not_replace_a_required_choice_with_a_custom_answer() {
        assert_eq!(
            resolve_pending_user_input_answer(
                &native_choice_question(),
                Some(&draft(json!({
                    "selectedOptionValues": ["second"],
                    "customAnswer": "Use another result",
                }))),
            ),
            Some(text("second"))
        );
    }

    #[test]
    fn does_not_submit_labels_or_unknown_values_when_an_option_has_a_value() {
        assert_eq!(
            resolve_pending_user_input_answer(
                &native_choice_question(),
                Some(&draft(
                    json!({ "selectedOptionValues": ["Result", "unknown"] })
                )),
            ),
            None
        );
    }

    #[test]
    fn toggles_options_for_multi_select_questions() {
        let question = multi_select_question();
        assert_eq!(
            toggle_pending_user_input_option_selection(&question, None, "Server"),
            PendingUserInputDraftAnswer {
                custom_answer: Some(String::new()),
                selected_option_values: Some(vec!["Server".to_string()]),
                ..Default::default()
            }
        );
        assert_eq!(
            toggle_pending_user_input_option_selection(
                &question,
                Some(&draft(json!({ "selectedOptionValues": ["Server", "Web"] }))),
                "Server",
            ),
            PendingUserInputDraftAnswer {
                custom_answer: Some(String::new()),
                selected_option_values: Some(vec!["Web".to_string()]),
                ..Default::default()
            }
        );
    }

    #[test]
    fn selects_and_removes_options_with_the_same_label_independently() {
        let mut question = native_choice_question();
        question.multi_select = Some(true);
        let first_selected =
            toggle_pending_user_input_option_selection(&question, None, " first\t");
        let both_selected =
            toggle_pending_user_input_option_selection(&question, Some(&first_selected), "second");

        assert_eq!(
            build_pending_user_input_answers(
                std::slice::from_ref(&question),
                &drafts_from(&[("result", both_selected.clone())]),
            ),
            Some(HashMap::from([(
                "result".to_string(),
                choices(&[" first\t", "second"])
            )]))
        );

        let second_selected =
            toggle_pending_user_input_option_selection(&question, Some(&both_selected), " first\t");
        assert_eq!(
            build_pending_user_input_answers(
                std::slice::from_ref(&question),
                &drafts_from(&[("result", second_selected)]),
            ),
            Some(HashMap::from([(
                "result".to_string(),
                choices(&["second"])
            )]))
        );
    }

    fn drafts_from(
        entries: &[(&str, PendingUserInputDraftAnswer)],
    ) -> HashMap<String, PendingUserInputDraftAnswer> {
        entries
            .iter()
            .map(|(id, draft)| (id.to_string(), draft.clone()))
            .collect()
    }

    #[test]
    fn returns_a_canonical_answer_map_for_complete_prompts() {
        let questions = vec![single_select_question(), compat_question()];
        let draft_answers = draft_map(vec![
            (
                "scope",
                json!({ "selectedOptionValues": ["Orchestration-first"] }),
            ),
            (
                "compat",
                json!({ "customAnswer": "Keep the current envelope for one release window" }),
            ),
        ]);
        assert_eq!(
            build_pending_user_input_answers(&questions, &draft_answers),
            Some(HashMap::from([
                ("scope".to_string(), text("Orchestration-first")),
                (
                    "compat".to_string(),
                    text("Keep the current envelope for one release window")
                ),
            ]))
        );
    }

    #[test]
    fn returns_arrays_for_answered_multi_select_prompts() {
        assert_eq!(
            build_pending_user_input_answers(
                &[multi_select_question()],
                &draft_map(vec![(
                    "areas",
                    json!({ "selectedOptionValues": ["Server", "Web"] })
                )]),
            ),
            Some(HashMap::from([(
                "areas".to_string(),
                choices(&["Server", "Web"])
            )]))
        );
    }

    #[test]
    fn returns_none_when_any_question_is_unanswered() {
        assert_eq!(
            build_pending_user_input_answers(&[single_select_question()], &HashMap::new()),
            None
        );
    }

    #[test]
    fn preserves_the_exact_selected_option_value() {
        for value in [" first\t", ""] {
            let mut question = native_choice_question();
            question.options[0].value = Some(value.to_string());
            let selected = toggle_pending_user_input_option_selection(&question, None, value);

            assert_eq!(
                build_pending_user_input_answers(
                    std::slice::from_ref(&question),
                    &drafts_from(&[("result", selected.clone())]),
                ),
                Some(HashMap::from([("result".to_string(), text(value))]))
            );
            let progress = derive_pending_user_input_progress(
                std::slice::from_ref(&question),
                &drafts_from(&[("result", selected)]),
                0,
            );
            assert_eq!(progress.selected_option_values, vec![value.to_string()]);
            assert_eq!(progress.answered_question_count, 1);
            assert!(progress.can_advance);
            assert!(progress.is_complete);
        }
    }

    #[test]
    fn counts_only_answered_questions() {
        let questions = vec![single_select_question(), compat_question()];
        assert_eq!(
            count_answered_pending_user_input_questions(
                &questions,
                &draft_map(vec![(
                    "scope",
                    json!({ "selectedOptionValues": ["Orchestration-first"] })
                )]),
            ),
            1
        );
    }

    #[test]
    fn finds_the_first_unanswered_question() {
        let questions = vec![single_select_question(), compat_question()];
        assert_eq!(
            find_first_unanswered_pending_user_input_question_index(
                &questions,
                &draft_map(vec![(
                    "scope",
                    json!({ "selectedOptionValues": ["Orchestration-first"] })
                )]),
            ),
            1
        );
    }

    #[test]
    fn returns_the_last_question_index_when_all_answers_are_complete() {
        let questions = vec![single_select_question(), compat_question()];
        assert_eq!(
            find_first_unanswered_pending_user_input_question_index(
                &questions,
                &draft_map(vec![
                    (
                        "scope",
                        json!({ "selectedOptionValues": ["Orchestration-first"] })
                    ),
                    (
                        "compat",
                        json!({ "customAnswer": "Keep it for one release window" })
                    ),
                ]),
            ),
            1
        );
    }

    #[test]
    fn derives_the_active_question_and_advancement_state() {
        let questions = vec![single_select_question(), compat_question()];
        let progress = derive_pending_user_input_progress(
            &questions,
            &draft_map(vec![(
                "scope",
                json!({ "selectedOptionValues": ["Orchestration-first"] }),
            )]),
            0,
        );
        assert_eq!(progress.question_index, 0);
        assert_eq!(progress.active_question, Some(questions[0].clone()));
        assert_eq!(
            progress.selected_option_values,
            vec!["Orchestration-first".to_string()]
        );
        assert_eq!(progress.custom_answer, "");
        assert_eq!(progress.resolved_answer, Some(text("Orchestration-first")));
        assert_eq!(progress.answered_question_count, 1);
        assert!(!progress.is_last_question);
        assert!(!progress.is_complete);
        assert!(progress.can_advance);
    }

    #[test]
    fn treats_multi_select_questions_as_answered_when_they_have_selected_options() {
        let progress = derive_pending_user_input_progress(
            &[multi_select_question()],
            &draft_map(vec![(
                "areas",
                json!({ "selectedOptionValues": ["Server", "Web"] }),
            )]),
            0,
        );
        assert_eq!(
            progress.selected_option_values,
            vec!["Server".to_string(), "Web".to_string()]
        );
        assert_eq!(progress.resolved_answer, Some(choices(&["Server", "Web"])));
        assert!(progress.can_advance);
        assert!(progress.is_complete);
    }

    #[test]
    fn requires_an_option_when_custom_answers_are_disabled() {
        let questions = [native_choice_question()];
        let draft_answers = draft_map(vec![(
            "result",
            json!({ "customAnswer": "Use another result" }),
        )]);

        assert_eq!(
            build_pending_user_input_answers(&questions, &draft_answers),
            None
        );
        let progress = derive_pending_user_input_progress(&questions, &draft_answers, 0);
        assert_eq!(progress.custom_answer, "");
        assert!(!progress.using_custom_answer);
        assert_eq!(progress.resolved_answer, None);
        assert_eq!(progress.answered_question_count, 0);
        assert!(!progress.can_advance);
        assert!(!progress.is_complete);
    }

    #[test]
    fn accepts_attachment_only_answers_after_every_upload_finishes() {
        let questions = [question(json!({
            "id": "spec",
            "header": "Spec",
            "question": "Provide a spec",
            "options": [],
            "multiSelect": false,
        }))];
        assert_eq!(
            build_pending_user_input_answers(
                &questions,
                &draft_map(vec![("spec", json!({ "attachmentCount": 1 }))])
            ),
            Some(HashMap::from([("spec".to_string(), text(""))]))
        );
        assert_eq!(
            build_pending_user_input_answers(
                &questions,
                &draft_map(vec![(
                    "spec",
                    json!({ "attachmentCount": 1, "attachmentsBlocked": true })
                )]),
            ),
            None
        );
        let mut no_custom = questions[0].clone();
        no_custom.allow_custom_answer = Some(false);
        assert_eq!(
            build_pending_user_input_answers(
                std::slice::from_ref(&no_custom),
                &draft_map(vec![("spec", json!({ "attachmentCount": 1 }))]),
            ),
            None
        );
    }

    #[test]
    fn keeps_the_thread_draft_when_nothing_was_typed_into_the_answer() {
        assert_eq!(
            carry_displaced_custom_answer_into_prompt("draft", None),
            "draft"
        );
        assert_eq!(
            carry_displaced_custom_answer_into_prompt("draft", Some("   ")),
            "draft"
        );
    }

    #[test]
    fn moves_the_typed_answer_into_an_empty_thread_draft() {
        assert_eq!(
            carry_displaced_custom_answer_into_prompt("", Some("also rename the flag ")),
            "also rename the flag"
        );
    }

    #[test]
    fn appends_the_typed_answer_after_an_existing_thread_draft() {
        assert_eq!(
            carry_displaced_custom_answer_into_prompt("first half\n", Some("second half")),
            "first half\n\nsecond half"
        );
    }
}
