//! Port of upstream/packages/client-runtime/src/promptLinks.ts
//!
//! How web and mobile resolve a prompt link against what this client knows. Both read the same
//! shells, so a link lands in the same project and thread on every surface.

use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::environment::{RepositoryIdentity, repository_grouping_key_of};
use turin_contracts::model_selection::ModelSelection;
use turin_contracts::thread_pull_request::{ThreadLinkedPullRequest, ThreadPullRequestLink};

use super::provider_types::{ServerProvider, ServerProviderAvailability, ServerProviderState};
use super::shared_support::{PromptLinkPullRequest, PromptLinkRepository};
use crate::state::support::model::resolve_selectable_model;
use crate::state::support::thread_pull_requests::{
    ThreadPullRequestKeySource, thread_pull_request_key_of, thread_pull_requests_of,
    visible_thread_pull_requests,
};

fn repository_key(repository: &PromptLinkRepository) -> String {
    format!("github.com/{}/{}", repository.owner, repository.name).to_lowercase()
}

/// `P extends { readonly repositoryIdentity?: RepositoryIdentity | null }`
pub trait PromptLinkProject {
    fn repository_identity(&self) -> Option<&RepositoryIdentity>;
}

/// The project a link's repository names. Callers pass projects most recently active first, so
/// the user lands where they last worked. A fork only matches its own owner, not its upstream.
pub fn find_prompt_link_project<'a, P: PromptLinkProject>(
    projects: &'a [P],
    repository: &PromptLinkRepository,
) -> Option<&'a P> {
    let key = repository_key(repository);
    projects.iter().find(|project| {
        project
            .repository_identity()
            .is_some_and(|identity| repository_grouping_key_of(identity).to_lowercase() == key)
    })
}

/// `T extends { updatedAt, pullRequests?, linkedPullRequest? }`
pub trait PromptLinkThread {
    fn updated_at(&self) -> &str;
    fn pull_requests(&self) -> Option<&[ThreadPullRequestLink]>;
    fn linked_pull_request(&self) -> Option<&ThreadLinkedPullRequest>;
}

/// The most recently updated thread linked to the pull request, across every environment given.
pub fn find_prompt_link_pull_request_thread<'a, T: PromptLinkThread>(
    threads: &'a [T],
    pull_request: &PromptLinkPullRequest,
) -> Option<&'a T> {
    let repository = format!("{}/{}", pull_request.owner, pull_request.name);
    let key = thread_pull_request_key_of(&ThreadPullRequestKeySource {
        host: "github.com",
        repository: &repository,
        number: pull_request.number,
        authority: None,
        url: None,
    });
    let mut latest: Option<&T> = None;
    for thread in threads {
        if latest.is_some_and(|latest| thread.updated_at() <= latest.updated_at()) {
            continue;
        }
        let links = thread_pull_requests_of(thread.pull_requests(), thread.linked_pull_request());
        let linked = visible_thread_pull_requests(&links)
            .into_iter()
            .any(|link| thread_pull_request_key_of(&ThreadPullRequestKeySource::from(link)) == key);
        if linked {
            latest = Some(thread);
        }
    }
    latest
}

// People write the name they know. Driver kinds are what the server reports.
fn provider_alias(provider: &str) -> &str {
    match provider {
        "claude" => "claudeagent",
        other => other,
    }
}

/// What a prompt link asks for: a provider driver name and, optionally, a model.
#[derive(Debug, Clone, Copy)]
pub struct PromptLinkModelRequest<'a> {
    pub provider: &'a str,
    pub model: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptLinkModelSelection {
    pub selection: ModelSelection,
    pub exact: bool,
}

/// The selection a link asks for, on one of the user's own provider instances. `exact` is false
/// when the instance exists but the model does not, so its default model stands in. `None` when
/// no usable instance of that provider exists; the user's default applies then.
pub fn resolve_prompt_link_model_selection(
    providers: &[ServerProvider],
    request: &PromptLinkModelRequest<'_>,
) -> Option<PromptLinkModelSelection> {
    let driver = provider_alias(request.provider);
    let usable: Vec<&ServerProvider> = providers
        .iter()
        .filter(|provider| {
            provider.driver.as_str().to_lowercase() == driver
                && provider.enabled
                && provider.availability != Some(ServerProviderAvailability::Unavailable)
        })
        .collect();
    let instance = usable
        .iter()
        .find(|provider| provider.status == ServerProviderState::Ready)
        .or_else(|| {
            usable
                .iter()
                .find(|provider| provider.status == ServerProviderState::Warning)
        })?;
    let requested = request.model.and_then(|model| {
        resolve_selectable_model(instance.driver.as_str(), Some(model), &instance.models)
    });
    let model = match requested {
        Some(requested) => requested,
        None => instance
            .models
            .iter()
            .find(|candidate| candidate.is_default == Some(true))
            .or_else(|| instance.models.first())?
            .slug
            .as_str(),
    };
    Some(PromptLinkModelSelection {
        selection: ModelSelection {
            instance_id: instance.instance_id.clone(),
            model: TrimmedNonEmptyString::new(model).ok()?,
            options: None,
        },
        exact: request.model.is_none() || requested.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::helpers::provider_types::ServerProviderModel;
    use serde_json::json;
    use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};

    struct Project {
        id: &'static str,
        repository_identity: Option<RepositoryIdentity>,
    }

    impl PromptLinkProject for Project {
        fn repository_identity(&self) -> Option<&RepositoryIdentity> {
            self.repository_identity.as_ref()
        }
    }

    fn identity(canonical_key: &str, origin: Option<&str>) -> RepositoryIdentity {
        let mut value = json!({
            "canonicalKey": canonical_key,
            "locator": {
                "source": "git-remote",
                "remoteName": "origin",
                "remoteUrl": format!("https://{canonical_key}"),
            },
        });
        if let Some(origin) = origin {
            value["origin"] = json!({ "canonicalKey": origin });
        }
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn find_prompt_link_project_takes_the_first_exact_match_and_never_a_fork_of_the_repository() {
        let projects = [
            Project {
                id: "fork",
                repository_identity: Some(identity(
                    "github.com/pingdotgg/t3code",
                    Some("github.com/me/t3code"),
                )),
            },
            Project {
                id: "none",
                repository_identity: None,
            },
            Project {
                id: "recent",
                repository_identity: Some(identity("github.com/PingDotGG/T3Code", None)),
            },
            Project {
                id: "older",
                repository_identity: Some(identity("github.com/pingdotgg/t3code", None)),
            },
        ];
        let repository = |owner: &str, name: &str| PromptLinkRepository {
            owner: owner.to_owned(),
            name: name.to_owned(),
        };
        assert_eq!(
            find_prompt_link_project(&projects, &repository("pingdotgg", "t3code")).map(|p| p.id),
            Some("recent")
        );
        assert_eq!(
            find_prompt_link_project(&projects, &repository("me", "t3code")).map(|p| p.id),
            Some("fork")
        );
        assert!(find_prompt_link_project(&projects, &repository("gitlab", "x")).is_none());
    }

    struct Thread {
        id: &'static str,
        updated_at: &'static str,
        pull_requests: Vec<ThreadPullRequestLink>,
    }

    impl PromptLinkThread for Thread {
        fn updated_at(&self) -> &str {
            self.updated_at
        }
        fn pull_requests(&self) -> Option<&[ThreadPullRequestLink]> {
            Some(&self.pull_requests)
        }
        fn linked_pull_request(&self) -> Option<&ThreadLinkedPullRequest> {
            None
        }
    }

    fn link(number: u64, source: &str) -> ThreadPullRequestLink {
        serde_json::from_value(json!({
            "host": "github.com",
            "repository": "pingdotgg/t3code",
            "number": number,
            "url": format!("https://github.com/pingdotgg/t3code/pull/{number}"),
            "source": source,
            "linkedAt": "2026-01-01T00:00:00.000Z",
            "snapshot": null,
            "stack": null,
        }))
        .unwrap()
    }

    #[test]
    fn find_prompt_link_pull_request_thread_finds_the_most_recently_updated_linked_thread() {
        let threads = [
            Thread {
                id: "old",
                updated_at: "2026-01-01T00:00:00.000Z",
                pull_requests: vec![link(7, "created")],
            },
            Thread {
                id: "new",
                updated_at: "2026-02-01T00:00:00.000Z",
                pull_requests: vec![link(7, "created")],
            },
            Thread {
                id: "other",
                updated_at: "2026-03-01T00:00:00.000Z",
                pull_requests: vec![link(8, "created")],
            },
            Thread {
                id: "dismissed",
                updated_at: "2026-04-01T00:00:00.000Z",
                pull_requests: vec![link(7, "stack-dismissed")],
            },
        ];
        let pull_request = |number| PromptLinkPullRequest {
            owner: "PingDotGG".to_owned(),
            name: "t3code".to_owned(),
            number,
        };
        assert_eq!(
            find_prompt_link_pull_request_thread(&threads, &pull_request(7)).map(|t| t.id),
            Some("new")
        );
        assert!(find_prompt_link_pull_request_thread(&threads, &pull_request(9)).is_none());
    }

    fn provider(instance_id: &str, driver: &str, status: ServerProviderState) -> ServerProvider {
        let mut provider = ServerProvider::new(
            ProviderInstanceId::new(instance_id).unwrap(),
            ProviderDriverKind::new(driver).unwrap(),
        );
        provider.status = status;
        provider.models = vec![
            ServerProviderModel {
                slug: TrimmedNonEmptyString::new("small").unwrap(),
                name: TrimmedNonEmptyString::new("Small").unwrap(),
                aliases: None,
                is_default: None,
            },
            ServerProviderModel {
                slug: TrimmedNonEmptyString::new("big").unwrap(),
                name: TrimmedNonEmptyString::new("Big").unwrap(),
                aliases: Some(vec!["opus".to_owned()]),
                is_default: Some(true),
            },
        ];
        provider
    }

    fn selection(instance_id: &str, model: &str) -> ModelSelection {
        ModelSelection {
            instance_id: ProviderInstanceId::new(instance_id).unwrap(),
            model: TrimmedNonEmptyString::new(model).unwrap(),
            options: None,
        }
    }

    #[test]
    fn maps_the_requested_provider_and_model_onto_a_usable_instance() {
        let providers = [
            provider("broken", "claudeAgent", ServerProviderState::Error),
            provider("work", "claudeAgent", ServerProviderState::Ready),
        ];
        assert_eq!(
            resolve_prompt_link_model_selection(
                &providers,
                &PromptLinkModelRequest {
                    provider: "claude",
                    model: Some("small")
                }
            ),
            Some(PromptLinkModelSelection {
                selection: selection("work", "small"),
                exact: true
            })
        );
        assert_eq!(
            resolve_prompt_link_model_selection(
                &providers,
                &PromptLinkModelRequest {
                    provider: "claudeagent",
                    model: Some("opus")
                }
            ),
            Some(PromptLinkModelSelection {
                selection: selection("work", "big"),
                exact: true
            })
        );
    }

    #[test]
    fn uses_the_instance_default_for_an_unknown_model_and_nothing_for_a_missing_provider() {
        let providers = [provider("codex", "codex", ServerProviderState::Ready)];
        assert_eq!(
            resolve_prompt_link_model_selection(
                &providers,
                &PromptLinkModelRequest {
                    provider: "codex",
                    model: Some("nope")
                }
            ),
            Some(PromptLinkModelSelection {
                selection: selection("codex", "big"),
                exact: false
            })
        );
        assert_eq!(
            resolve_prompt_link_model_selection(
                &providers,
                &PromptLinkModelRequest {
                    provider: "cursor",
                    model: None
                }
            ),
            None
        );
    }
}
