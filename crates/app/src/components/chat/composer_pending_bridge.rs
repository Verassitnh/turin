//! Composer footer pending-question mode for `UserInputResponseCapability::Message`.

use std::collections::HashMap;

use gpui_kit::App;
use turin_contracts::provider_runtime::UserInputQuestion;

use crate::composer::primary_actions::PendingActionState;
use crate::composer::view_model::PendingAnswer;
use crate::state::thread_requests::{
    PendingThreadRequests, ThreadPendingUserInput, ThreadUserInputQuestion,
    UserInputResponseCapability,
};

use super::pending_user_input::{
    PendingUserInputAnswer, PendingUserInputDraftAnswer, build_pending_user_input_answers,
    derive_pending_user_input_progress, set_pending_user_input_custom_answer,
};
#[derive(Debug, Clone, Default)]
pub struct ComposerPendingInputSession {
    pub request_id: Option<String>,
    pub question_index: usize,
    pub answers: HashMap<String, PendingUserInputDraftAnswer>,
    pub responding: bool,
}

pub fn message_mode_pending_input<'a>(
    requests: &'a PendingThreadRequests,
) -> Option<&'a ThreadPendingUserInput> {
    requests
        .user_inputs
        .iter()
        .find(|input| input.response_capability == UserInputResponseCapability::Message)
}

pub fn sync_pending_input_session(
    session: &mut ComposerPendingInputSession,
    input: Option<&ThreadPendingUserInput>,
) {
    let Some(input) = input else {
        *session = ComposerPendingInputSession::default();
        return;
    };
    let request_id = input.request_id.to_string();
    if session.request_id.as_deref() != Some(request_id.as_str()) {
        *session = ComposerPendingInputSession {
            request_id: Some(request_id),
            question_index: 0,
            answers: HashMap::new(),
            responding: false,
        };
    }
}

fn contract_questions(questions: &[ThreadUserInputQuestion]) -> Vec<UserInputQuestion> {
    questions
        .iter()
        .filter_map(|question| {
            serde_json::from_value(serde_json::json!({
                "id": question.id.as_str(),
                "header": question.header.as_str(),
                "question": question.question.as_str(),
                "options": question.options.iter().map(|option| serde_json::json!({
                    "label": option.label,
                    "description": option.description,
                    "value": option.value,
                })).collect::<Vec<_>>(),
                "allowCustomAnswer": question.allow_custom_answer,
                "initialAnswer": question.initial_answer,
                "multiSelect": question.multi_select,
            }))
            .ok()
        })
        .collect()
}

fn merge_attachment_counts(
    cx: &App,
    environment_id: &str,
    thread_id: &str,
    input: &ThreadPendingUserInput,
    mut answers: HashMap<String, PendingUserInputDraftAnswer>,
) -> HashMap<String, PendingUserInputDraftAnswer> {
    for question in &input.questions {
        let count = super::question_attachment_drafts::question_draft_attachment_count(
            cx,
            environment_id,
            thread_id,
            input.request_id.as_str(),
            question.id.as_str(),
        );
        if count == 0 {
            continue;
        }
        answers
            .entry(question.id.to_string())
            .and_modify(|draft| draft.attachment_count = Some(count))
            .or_insert_with(|| PendingUserInputDraftAnswer {
                attachment_count: Some(count),
                ..Default::default()
            });
    }
    answers
}

pub fn pending_answer_for_message_input(
    input: &ThreadPendingUserInput,
    session: &ComposerPendingInputSession,
    environment_id: &str,
    thread_id: &str,
    composer_prompt: &str,
    cx: &App,
) -> PendingAnswer {
    let questions = contract_questions(&input.questions);
    let question_index = session
        .question_index
        .min(questions.len().saturating_sub(1));
    let active = input.questions.get(question_index);
    let active_contract = questions.get(question_index);

    let mut answers = session.answers.clone();
    if let (Some(active), Some(contract)) = (active, active_contract) {
        let qid = active.id.to_string();
        let draft = set_pending_user_input_custom_answer(
            answers.get(&qid),
            composer_prompt,
            Some(contract),
        );
        answers.insert(qid, draft);
    }
    let answers = merge_attachment_counts(cx, environment_id, thread_id, input, answers);
    let progress =
        derive_pending_user_input_progress(&questions, &answers, question_index);
    let active = active.expect("message pending input always has a question");
    let choice_only = active.allow_custom_answer == Some(false) && !active.options.is_empty();
    PendingAnswer {
        request_id: input.request_id.to_string(),
        question_id: active.id.to_string(),
        custom_answer: progress.custom_answer,
        initial_answer: active.initial_answer.clone(),
        is_choice_only: choice_only,
        is_responding: session.responding,
        question_index: progress.question_index,
        is_last_question: progress.is_last_question,
        can_advance: progress.can_advance,
        is_complete: progress.is_complete,
    }
}

pub fn pending_action_state_for_prompt(
    answer: &PendingAnswer,
    prompt: &str,
) -> PendingActionState {
    let prompt_ready = !turin_contracts::base_schemas::js_trim(prompt).is_empty();
    PendingActionState {
        question_index: answer.question_index,
        is_last_question: answer.is_last_question,
        can_advance: answer.can_advance || prompt_ready,
        is_responding: answer.is_responding,
        is_complete: answer.is_complete
            || (answer.is_last_question && (answer.can_advance || prompt_ready)),
    }
}

/// Applies the composer prompt to the active question and returns whether the batch is ready to send.
pub fn apply_composer_prompt_to_session(
    input: &ThreadPendingUserInput,
    session: &mut ComposerPendingInputSession,
    environment_id: &str,
    thread_id: &str,
    composer_prompt: &str,
    cx: &App,
) -> PendingSubmitPlan {
    let questions = contract_questions(&input.questions);
    let question_index = session
        .question_index
        .min(questions.len().saturating_sub(1));
    let Some(active) = input.questions.get(question_index) else {
        return PendingSubmitPlan::Rejected;
    };
    let Some(contract) = questions.get(question_index) else {
        return PendingSubmitPlan::Rejected;
    };
    let qid = active.id.to_string();
    let draft = set_pending_user_input_custom_answer(
        session.answers.get(&qid),
        composer_prompt,
        Some(contract),
    );
    session.answers.insert(qid, draft);

    let answers = merge_attachment_counts(
        cx,
        environment_id,
        thread_id,
        input,
        session.answers.clone(),
    );
    let progress = derive_pending_user_input_progress(&questions, &answers, question_index);
    if !progress.can_advance {
        return PendingSubmitPlan::Rejected;
    }
    if !progress.is_last_question {
        session.question_index = progress.question_index + 1;
        return PendingSubmitPlan::Advanced;
    }
    let Some(built) = build_pending_user_input_answers(&questions, &answers) else {
        return PendingSubmitPlan::Rejected;
    };
    PendingSubmitPlan::Submit {
        request_id: input.request_id.to_string(),
        answers: questions
            .iter()
            .filter_map(|question| {
                built.get(question.id.as_str()).map(|answer| {
                    (
                        question.id.as_str().to_owned(),
                        pending_user_input_answer_text(answer),
                    )
                })
            })
            .collect(),
    }
}

pub fn pending_user_input_answer_text(answer: &PendingUserInputAnswer) -> String {
    match answer {
        PendingUserInputAnswer::Text(text) => text.clone(),
        PendingUserInputAnswer::Choices(choices) => choices.join(", "),
    }
}

#[derive(Debug)]
pub enum PendingSubmitPlan {
    Rejected,
    Advanced,
    Submit {
        request_id: String,
        answers: Vec<(String, String)>,
    },
}
