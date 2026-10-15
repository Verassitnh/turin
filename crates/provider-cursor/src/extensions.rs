//! Cursor extension requests as T3 cards.
//!
//! `cursor/ask_question` and `cursor/create_plan` replies nest `outcome`.
//! A flat `{"outcome":"answered"}` is what the CLI reads as cancel. The three
//! side-channel requests are acknowledged with `{}` by the session; this
//! module only projects their payloads.

use serde_json::Value;
use turin_acp::cursor::{
    AskAnswer, AskQuestionParams, AskQuestionResult, CreatePlanResult, CursorTodo, TodoStatus,
};
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::orchestration_v2::{
    OrchestrationV2PlanStep, OrchestrationV2PlanStepStatus, OrchestrationV2UserInputQuestion,
    OrchestrationV2UserInputQuestionOption,
};
use turin_contracts::provider_policy::ProviderUserInputAnswers;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskDisposition {
    Answered(Vec<AskAnswer>),
    Skipped { reason: Option<String> },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanDisposition {
    Accepted { plan_uri: Option<String> },
    Rejected { reason: Option<String> },
    Cancelled,
}

pub fn user_input_questions(params: &AskQuestionParams) -> Vec<OrchestrationV2UserInputQuestion> {
    params
        .questions
        .iter()
        .filter_map(|question| {
            let id = trimmed(&question.id)?;
            let prompt = trimmed(&question.prompt).or_else(|| trimmed(&question.id))?;
            let options = question
                .options
                .iter()
                .filter_map(|option| {
                    Some(OrchestrationV2UserInputQuestionOption {
                        label: trimmed(&option.label).or_else(|| trimmed(&option.id))?,
                        description: trimmed(&option.label)
                            .unwrap_or_else(|| trimmed(&option.id).unwrap()),
                        value: Some(option.id.clone()),
                    })
                })
                .collect();
            Some(OrchestrationV2UserInputQuestion {
                id,
                header: params
                    .title
                    .as_deref()
                    .and_then(trimmed)
                    .unwrap_or_else(|| prompt.clone()),
                question: prompt,
                options,
                multi_select: Some(question.allow_multiple),
                allow_custom_answer: None,
                initial_answer: None,
                required: None,
            })
        })
        .collect()
}

/// Upstream `cursorTodoSteps`: cancelled and blank todos are dropped, and each
/// kept row's id is its position among the kept rows.
pub fn plan_steps(todos: &[CursorTodo]) -> Vec<OrchestrationV2PlanStep> {
    todos
        .iter()
        .filter(|todo| todo.status != TodoStatus::Cancelled)
        .filter_map(|todo| {
            TrimmedNonEmptyString::new(todo.content.trim())
                .ok()
                .map(|text| (text, plan_step_status(&todo.status)))
        })
        .enumerate()
        .map(|(index, (text, status))| plan_step(index, text, status))
        .collect()
}

pub(crate) fn plan_step(
    index: usize,
    text: TrimmedNonEmptyString,
    status: OrchestrationV2PlanStepStatus,
) -> OrchestrationV2PlanStep {
    OrchestrationV2PlanStep {
        id: TrimmedNonEmptyString::new(format!("todo-{}", index + 1)).expect("step id"),
        text,
        status,
        duration_anchor_at: None,
        duration_ms: None,
    }
}

fn plan_step_status(status: &TodoStatus) -> OrchestrationV2PlanStepStatus {
    match status {
        TodoStatus::Completed => OrchestrationV2PlanStepStatus::Completed,
        TodoStatus::InProgress => OrchestrationV2PlanStepStatus::Running,
        TodoStatus::Pending | TodoStatus::Cancelled => OrchestrationV2PlanStepStatus::Pending,
    }
}

pub fn ask_result(disposition: AskDisposition) -> AskQuestionResult {
    match disposition {
        AskDisposition::Answered(answers) => AskQuestionResult::answered(answers),
        AskDisposition::Skipped { reason } => AskQuestionResult::skipped(reason),
        AskDisposition::Cancelled => AskQuestionResult::cancelled(),
    }
}

pub fn plan_result(disposition: PlanDisposition) -> CreatePlanResult {
    match disposition {
        PlanDisposition::Accepted { plan_uri } => CreatePlanResult::accepted(plan_uri),
        PlanDisposition::Rejected { reason } => CreatePlanResult::rejected(reason),
        PlanDisposition::Cancelled => CreatePlanResult::cancelled(),
    }
}

/// Builds the nested ask result from a user-input answer map.
///
/// Each question id maps to a string option id or an array of option ids.
/// Missing answers are skipped. An empty map is a skip, not a flat cancel.
pub fn ask_disposition_from_answers(
    params: &AskQuestionParams,
    answers: &ProviderUserInputAnswers,
    cancelled: bool,
) -> AskDisposition {
    if cancelled {
        return AskDisposition::Cancelled;
    }
    if answers.is_empty() {
        return AskDisposition::Skipped { reason: None };
    }
    let mut selected = Vec::new();
    for question in &params.questions {
        let Some(value) = answers.get(&question.id) else {
            continue;
        };
        let ids = match value {
            Value::String(id) => vec![id.clone()],
            Value::Array(items) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            _ => Vec::new(),
        };
        if ids.is_empty() {
            continue;
        }
        selected.push(AskAnswer {
            question_id: question.id.clone(),
            selected_option_ids: ids,
        });
    }
    if selected.is_empty() {
        AskDisposition::Skipped { reason: None }
    } else {
        AskDisposition::Answered(selected)
    }
}

pub fn nested_outcome(result: &impl serde::Serialize) -> Value {
    serde_json::to_value(result).unwrap_or_else(|_| serde_json::json!({}))
}

fn trimmed(value: &str) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turin_acp::cursor::AskQuestionParams;

    #[test]
    fn ask_and_plan_results_nest_the_outcome() {
        let answered = nested_outcome(&ask_result(AskDisposition::Answered(vec![AskAnswer {
            question_id: "q1".to_owned(),
            selected_option_ids: vec!["a".to_owned()],
        }])));
        assert_eq!(answered["outcome"]["outcome"], "answered");
        assert_eq!(answered["outcome"]["answers"][0]["questionId"], "q1");
        assert!(
            serde_json::from_value::<AskQuestionResult>(json!({"outcome": "answered"})).is_err()
        );

        let accepted = nested_outcome(&plan_result(PlanDisposition::Accepted {
            plan_uri: Some("file:///plan.md".to_owned()),
        }));
        assert_eq!(accepted["outcome"]["outcome"], "accepted");
        assert_eq!(accepted["outcome"]["planUri"], "file:///plan.md");
        let cancelled = nested_outcome(&plan_result(PlanDisposition::Cancelled));
        assert_eq!(cancelled["outcome"]["outcome"], "cancelled");
    }

    #[test]
    fn answers_map_onto_question_ids() {
        let params: AskQuestionParams = serde_json::from_value(json!({
            "toolCallId": "tool-1",
            "title": "Pick",
            "questions": [{
                "id": "q1",
                "prompt": "Which?",
                "allowMultiple": false,
                "options": [{"id": "a", "label": "A"}]
            }]
        }))
        .unwrap();
        let questions = user_input_questions(&params);
        assert_eq!(questions[0].question.as_str(), "Which?");
        assert_eq!(questions[0].options[0].value.as_deref(), Some("a"));
        let mut answers = ProviderUserInputAnswers::new();
        answers.insert("q1".to_owned(), json!("a"));
        match ask_disposition_from_answers(&params, &answers, false) {
            AskDisposition::Answered(answers) => {
                assert_eq!(answers[0].selected_option_ids, vec!["a".to_owned()]);
            }
            other => panic!("{other:?}"),
        }
    }
}
