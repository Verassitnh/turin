//! Port of upstream/apps/web/src/components/promptLinks/linkedPrompt.ts
#![allow(dead_code)]

use turin_shared::prompt_links::LinkedPrompt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HiddenCharacterWarning {
    None,
    Warn { count: u32 },
}

pub fn hidden_character_warning(prompt: &LinkedPrompt) -> HiddenCharacterWarning {
    if prompt.hidden_characters == 0 {
        HiddenCharacterWarning::None
    } else {
        HiddenCharacterWarning::Warn {
            count: prompt.hidden_characters,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaceLinkedPromptDecision {
    PlaceImmediately,
    ConfirmReplaceDraft,
}

/// Whether placing a linked prompt replaces the draft outright or asks first.
pub fn place_linked_prompt_decision(
    existing_draft: &str,
    linked: &LinkedPrompt,
) -> PlaceLinkedPromptDecision {
    if existing_draft.trim().is_empty() || existing_draft == linked.text {
        PlaceLinkedPromptDecision::PlaceImmediately
    } else {
        PlaceLinkedPromptDecision::ConfirmReplaceDraft
    }
}

pub fn linked_prompt_info_notes(notes: &[String]) -> String {
    if notes.is_empty() {
        "Read it before you send.".to_owned()
    } else {
        std::iter::once("Read it before you send.")
            .chain(notes.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(text: &str) -> LinkedPrompt {
        LinkedPrompt {
            text: text.to_owned(),
            hidden_characters: 0,
        }
    }

    #[test]
    fn warns_when_hidden_characters_are_present() {
        assert!(matches!(
            hidden_character_warning(&LinkedPrompt {
                text: "x".to_owned(),
                hidden_characters: 2
            }),
            HiddenCharacterWarning::Warn { count: 2 }
        ));
    }

    #[test]
    fn replaces_empty_drafts_and_confirms_nonempty_ones() {
        assert_eq!(
            place_linked_prompt_decision("", &prompt("linked")),
            PlaceLinkedPromptDecision::PlaceImmediately
        );
        assert_eq!(
            place_linked_prompt_decision("draft", &prompt("linked")),
            PlaceLinkedPromptDecision::ConfirmReplaceDraft
        );
    }
}
