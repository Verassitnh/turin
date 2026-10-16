//! Port of upstream/apps/web/src/components/promptLinks/ThreadLinkPrompt.tsx
#![allow(dead_code)]

use turin_contracts::environment::ScopedThreadRef;
use turin_shared::prompt_links::{
    LinkedPrompt, PromptLinkResult, parse_thread_link_prompt, search_has_query_key,
};

use super::linked_prompt::{PlaceLinkedPromptDecision, place_linked_prompt_decision};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadLinkPromptEffect {
    None,
    ParseError(String),
    Place(PlaceLinkedPromptDecision, LinkedPrompt),
    StripPromptFromUrl { pathname: String, search: String },
}

pub fn thread_link_prompt_effect(
    location_key: &str,
    pathname: &str,
    search: &str,
    thread_ref: &ScopedThreadRef,
    existing_composer_draft: &str,
    handled_keys: &mut std::collections::HashSet<String>,
) -> ThreadLinkPromptEffect {
    let _ = thread_ref;
    if !search_has_query_key(search, "prompt") {
        return ThreadLinkPromptEffect::None;
    }
    if !handled_keys.contains(location_key) {
        handled_keys.insert(location_key.to_owned());
        match parse_thread_link_prompt(search) {
            PromptLinkResult::Failure(error) => {
                return ThreadLinkPromptEffect::ParseError(error);
            }
            PromptLinkResult::Success(Some(prompt)) => {
                return ThreadLinkPromptEffect::Place(
                    place_linked_prompt_decision(existing_composer_draft, &prompt),
                    prompt,
                );
            }
            PromptLinkResult::Success(None) => {}
        }
    }
    ThreadLinkPromptEffect::StripPromptFromUrl {
        pathname: pathname.to_owned(),
        search: turin_shared::prompt_links::without_prompt_link_params(search),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::environment::ScopedThreadRef;

    fn thread_ref() -> ScopedThreadRef {
        serde_json::from_value(serde_json::json!({
            "environmentId": "env",
            "threadId": "thread",
        }))
        .unwrap()
    }

    #[test]
    fn ignores_queries_without_a_prompt_key() {
        let mut handled = std::collections::HashSet::new();
        assert_eq!(
            thread_link_prompt_effect(
                "loc",
                "/thread",
                "?panel=diff",
                &thread_ref(),
                "",
                &mut handled,
            ),
            ThreadLinkPromptEffect::None
        );
    }

    #[test]
    fn confirms_before_replacing_a_nonempty_different_draft() {
        let mut handled = std::collections::HashSet::new();
        let effect = thread_link_prompt_effect(
            "loc",
            "/thread",
            "?prompt=linked",
            &thread_ref(),
            "draft",
            &mut handled,
        );
        assert!(matches!(
            effect,
            ThreadLinkPromptEffect::Place(
                PlaceLinkedPromptDecision::ConfirmReplaceDraft,
                _
            )
        ));
    }
}
