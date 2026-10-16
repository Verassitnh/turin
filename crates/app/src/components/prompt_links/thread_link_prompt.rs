//! Port of upstream/apps/web/src/components/promptLinks/ThreadLinkPrompt.tsx
#![allow(dead_code)]

use turin_contracts::environment::ScopedThreadRef;
use turin_shared::prompt_links::{LinkedPrompt, PromptLinkResult, parse_thread_link_prompt};

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
    handled_keys: &mut std::collections::HashSet<String>,
) -> ThreadLinkPromptEffect {
    let _ = thread_ref;
    if !search.contains("prompt=") {
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
                    place_linked_prompt_decision("", &prompt),
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
