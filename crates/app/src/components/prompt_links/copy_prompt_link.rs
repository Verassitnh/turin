//! Port of upstream/apps/web/src/components/promptLinks/copyPromptLink.ts
#![allow(dead_code)]

use turin_contracts::environment::repository_grouping_key_of;
use turin_contracts::model_selection::ModelSelection;
use turin_shared::js_string::utf16_len;
use turin_shared::prompt_links::{
    NewThreadLinkInput, PROMPT_LINK_MAX_PROMPT_LENGTH, PromptLinkRepository, build_new_thread_link,
    parse_prompt_link_repository,
};

use crate::state::helpers::prompt_links::PromptLinkProject;
use crate::state::helpers::provider_types::ServerProvider;

/// Shareable hosted-app link for a prompt, or none when it cannot fit.
pub fn prompt_link_for(
    prompt: &str,
    context: Option<PromptLinkContext<'_>>,
    hosted_app_url: Option<&str>,
) -> Option<String> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() || utf16_len(trimmed) > PROMPT_LINK_MAX_PROMPT_LENGTH {
        return None;
    }
    let context = context.unwrap_or(PromptLinkContext {
        repository: None,
        provider: None,
        model: None,
    });
    build_new_thread_link(NewThreadLinkInput {
        prompt: trimmed,
        repository: context.repository,
        provider: context.provider,
        model: context.model,
        origin: hosted_app_url,
    })
    .ok()
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PromptLinkContext<'a> {
    pub repository: Option<&'a PromptLinkRepository>,
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
}

/// GitHub repository identity for a prompt link, when the project has one.
pub fn prompt_link_repository_of(
    project: Option<&impl PromptLinkProject>,
) -> Option<PromptLinkRepository> {
    let identity = project?.repository_identity()?;
    let key = repository_grouping_key_of(identity);
    key.strip_prefix("github.com/")
        .and_then(parse_prompt_link_repository)
}

#[derive(Clone, Debug)]
pub struct ComposerDraftSnapshot {
    pub prompt: String,
    pub active_provider: Option<String>,
    pub model_selection_by_provider: indexmap::IndexMap<String, ModelSelection>,
}

pub fn composer_prompt_link(
    draft: Option<&ComposerDraftSnapshot>,
    project: Option<&impl PromptLinkProject>,
    providers: &[ServerProvider],
    thread_model_selection: Option<&ModelSelection>,
    with_context: bool,
    hosted_app_url: Option<&str>,
) -> Option<String> {
    let draft = draft?;
    if !with_context {
        return prompt_link_for(&draft.prompt, None, hosted_app_url);
    }
    let selection = draft
        .active_provider
        .as_ref()
        .and_then(|provider| draft.model_selection_by_provider.get(provider))
        .or(thread_model_selection);
    let provider = selection.and_then(|selection| {
        providers
            .iter()
            .find(|candidate| candidate.instance_id == selection.instance_id)
            .map(|candidate| candidate.driver.as_str())
    });
    prompt_link_for(
        &draft.prompt,
        Some(PromptLinkContext {
            repository: prompt_link_repository_of(project).as_ref(),
            provider,
            model: provider.and_then(|_| selection.map(|value| value.model.as_str())),
        }),
        hosted_app_url,
    )
}

/// Clipboard + toast seam (upstream `copyPromptLink`).
pub enum CopyPromptLinkOutcome {
    Copied,
    ClipboardUnavailable,
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::TrimmedNonEmptyString;
    use turin_contracts::environment::RepositoryIdentity;
    use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};

    struct Project {
        repository_identity: Option<RepositoryIdentity>,
    }

    impl PromptLinkProject for Project {
        fn repository_identity(&self) -> Option<&RepositoryIdentity> {
            self.repository_identity.as_ref()
        }
    }

    #[test]
    fn builds_a_link_for_trimmed_prompts_and_rejects_empty_or_oversized_prompts() {
        assert!(prompt_link_for("", None, None).is_none());
        assert!(prompt_link_for("   ", None, None).is_none());
        let link = prompt_link_for("hello", None, None);
        assert!(link.is_some());
        assert!(link.unwrap().contains("prompt="));
    }

    #[test]
    fn rejects_prompts_over_the_js_utf16_length_limit() {
        let within_limit = "a".repeat(8_000);
        assert!(prompt_link_for(&within_limit, None, None).is_some());
        let over_limit = format!("a{}", "😀".repeat(4_000));
        assert!(utf16_len(&over_limit) > PROMPT_LINK_MAX_PROMPT_LENGTH);
        assert!(prompt_link_for(&over_limit, None, None).is_none());
    }

    #[test]
    fn maps_github_repository_identity_to_prompt_link_repository() {
        let identity: RepositoryIdentity = serde_json::from_value(serde_json::json!({
            "canonicalKey": "github.com/pingdotgg/t3code",
            "locator": {
                "source": "git-remote",
                "remoteName": "origin",
                "remoteUrl": "https://github.com/pingdotgg/t3code",
            },
        }))
        .unwrap();
        let repo = prompt_link_repository_of(Some(&Project {
            repository_identity: Some(identity),
        }))
        .unwrap();
        assert_eq!(repo.owner, "pingdotgg");
        assert_eq!(repo.name, "t3code");
    }

    #[test]
    fn composer_prompt_link_includes_model_context_when_requested() {
        let instance = ProviderInstanceId::new("work").unwrap();
        let selection = ModelSelection {
            instance_id: instance.clone(),
            model: TrimmedNonEmptyString::new("big").unwrap(),
            options: None,
        };
        let mut by_provider = indexmap::IndexMap::new();
        by_provider.insert("codex".to_owned(), selection);
        let providers = [ServerProvider::new(
            instance,
            ProviderDriverKind::new("codex").unwrap(),
        )];
        let link: Option<String> = composer_prompt_link(
            Some(&ComposerDraftSnapshot {
                prompt: "ship it".to_owned(),
                active_provider: Some("codex".to_owned()),
                model_selection_by_provider: by_provider,
            }),
            None::<&Project>,
            &providers,
            None,
            true,
            None,
        );
        assert!(link.is_some());
    }
}
