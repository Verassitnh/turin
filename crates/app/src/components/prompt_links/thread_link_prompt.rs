//! Port of upstream/apps/web/src/components/promptLinks/ThreadLinkPrompt.tsx
#![allow(dead_code)]

use turin_contracts::environment::ScopedThreadRef;
use turin_shared::prompt_links::{
    LinkedPrompt, PromptLinkResult, parse_thread_link_prompt, search_has_query_key,
    without_prompt_link_params,
};

use super::linked_prompt::{PlaceLinkedPromptDecision, place_linked_prompt_decision};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadLinkPromptEffect {
    None,
    ParseError(String),
    Place(PlaceLinkedPromptDecision, LinkedPrompt),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadLinkPromptNavigation {
    pub pathname: String,
    pub search: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadLinkPromptResult {
    pub effect: ThreadLinkPromptEffect,
    /// Set whenever the location had a `prompt` query key (upstream strips it on every pass).
    pub navigation: Option<ThreadLinkPromptNavigation>,
}

fn stripped_navigation(pathname: &str, search: &str) -> ThreadLinkPromptNavigation {
    ThreadLinkPromptNavigation {
        pathname: pathname.to_owned(),
        search: without_prompt_link_params(search),
    }
}

pub fn thread_link_prompt_effect(
    location_key: &str,
    pathname: &str,
    search: &str,
    thread_ref: &ScopedThreadRef,
    existing_composer_draft: &str,
    handled_keys: &mut std::collections::HashSet<String>,
) -> ThreadLinkPromptResult {
    let _ = thread_ref;
    if !search_has_query_key(search, "prompt") {
        return ThreadLinkPromptResult {
            effect: ThreadLinkPromptEffect::None,
            navigation: None,
        };
    }
    let navigation = Some(stripped_navigation(pathname, search));
    if !handled_keys.contains(location_key) {
        handled_keys.insert(location_key.to_owned());
        match parse_thread_link_prompt(search) {
            PromptLinkResult::Failure(error) => {
                return ThreadLinkPromptResult {
                    effect: ThreadLinkPromptEffect::ParseError(error),
                    navigation,
                };
            }
            PromptLinkResult::Success(Some(prompt)) => {
                return ThreadLinkPromptResult {
                    effect: ThreadLinkPromptEffect::Place(
                        place_linked_prompt_decision(existing_composer_draft, &prompt),
                        prompt,
                    ),
                    navigation,
                };
            }
            PromptLinkResult::Success(None) => {}
        }
    }
    ThreadLinkPromptResult {
        effect: ThreadLinkPromptEffect::None,
        navigation,
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
        let result = thread_link_prompt_effect(
            "loc",
            "/thread",
            "?panel=diff",
            &thread_ref(),
            "",
            &mut handled,
        );
        assert_eq!(result.effect, ThreadLinkPromptEffect::None);
        assert!(result.navigation.is_none());
    }

    #[test]
    fn confirms_before_replacing_a_nonempty_different_draft() {
        let mut handled = std::collections::HashSet::new();
        let result = thread_link_prompt_effect(
            "loc",
            "/thread",
            "?prompt=linked",
            &thread_ref(),
            "draft",
            &mut handled,
        );
        assert!(matches!(
            result.effect,
            ThreadLinkPromptEffect::Place(PlaceLinkedPromptDecision::ConfirmReplaceDraft, _)
        ));
        assert_eq!(
            result.navigation,
            Some(ThreadLinkPromptNavigation {
                pathname: "/thread".into(),
                search: String::new(),
            })
        );
    }

    #[test]
    fn strips_prompt_from_the_url_on_parse_errors() {
        let mut handled = std::collections::HashSet::new();
        let result = thread_link_prompt_effect(
            "loc",
            "/thread",
            "?prompt=%E2%80%8B&panel=diff",
            &thread_ref(),
            "",
            &mut handled,
        );
        assert!(matches!(
            result.effect,
            ThreadLinkPromptEffect::ParseError(_)
        ));
        assert_eq!(
            result.navigation,
            Some(ThreadLinkPromptNavigation {
                pathname: "/thread".into(),
                search: "?panel=diff".into(),
            })
        );
    }
}
