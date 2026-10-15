//! Port of upstream/packages/client-runtime/src/work-log/userInput.ts

use serde_json::Value;

use super::turn_item::UserInputAnswerPayload;

/// Collapses every run of whitespace to one space (JS `.replace(/\s+/g, " ")`).
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() || ch == '\u{feff}' {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    out
}

pub fn get_question_answer_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| get_question_answer_text(Some(value)))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        Some(Value::Object(nested)) => get_question_answer_text(nested.get("answers")),
        _ => String::new(),
    }
}

pub fn get_question_text_preview(answer: &UserInputAnswerPayload) -> String {
    answer
        .question_text_by_id
        .iter()
        .map(|(_, text)| collapse_whitespace(text).trim().to_string())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

pub fn get_question_answer_preview(answer: &UserInputAnswerPayload) -> String {
    let answers = answer
        .answers
        .iter()
        .map(|(_, value)| get_question_answer_text(Some(value)))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    let attachments = answer
        .attachments_by_question_id
        .iter()
        .flat_map(|(_, names)| names.iter().cloned())
        .collect::<Vec<_>>();
    let preview = if !answers.is_empty() {
        answers.join(" · ")
    } else if !attachments.is_empty() {
        attachments.join(", ")
    } else {
        answer
            .question_text_by_id
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join(" · ")
    };
    collapse_whitespace(&preview).trim().to_string()
}

pub fn has_question_answer(answer: &UserInputAnswerPayload) -> bool {
    answer
        .answers
        .iter()
        .any(|(_, value)| !get_question_answer_text(Some(value)).is_empty())
        || answer
            .attachments_by_question_id
            .iter()
            .any(|(_, attachments)| !attachments.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(question_text_by_id: &[(&str, &str)]) -> UserInputAnswerPayload {
        UserInputAnswerPayload {
            request_id: "request-1".into(),
            question_text_by_id: question_text_by_id
                .iter()
                .map(|(id, text)| (id.to_string(), text.to_string()))
                .collect(),
            answers: vec![(
                "scope".into(),
                Value::String("Use the private repository".into()),
            )],
            attachments_by_question_id: Vec::new(),
        }
    }

    #[test]
    fn joins_the_question_texts() {
        assert_eq!(
            get_question_text_preview(&answer(&[
                ("scope", "Which repository?"),
                ("name", "What name?")
            ])),
            "Which repository? · What name?"
        );
    }

    #[test]
    fn normalizes_whitespace_and_skips_blank_texts() {
        assert_eq!(
            get_question_text_preview(&answer(&[("scope", "Which\nrepository?"), ("x", "  ")])),
            "Which repository?"
        );
    }

    #[test]
    fn returns_an_empty_string_without_question_texts() {
        assert_eq!(get_question_text_preview(&answer(&[])), "");
    }
}
