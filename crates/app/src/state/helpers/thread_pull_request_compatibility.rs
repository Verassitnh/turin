//! Port of upstream/packages/client-runtime/src/threadPullRequestCompatibility.ts

use turin_contracts::environment::ExecutionEnvironmentCapabilities;
use turin_contracts::{ProjectId, ThreadId};

use super::shared_support::{ThreadPullRequestKeySource, normalize_thread_pull_request_key};

/// How an environment lets a client link pull requests to a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadPullRequestLinkMode {
    Multiple,
    Single,
    Unsupported,
}

/// Capability negotiation keeps both single-link and multi-link environments usable.
/// (`Pick<ExecutionEnvironmentCapabilities, "threadPullRequests" | "threadPullRequestLinking">`)
pub fn thread_pull_request_link_mode(
    capabilities: Option<&ExecutionEnvironmentCapabilities>,
) -> ThreadPullRequestLinkMode {
    if capabilities.is_some_and(|c| c.thread_pull_requests == Some(true)) {
        ThreadPullRequestLinkMode::Multiple
    } else if capabilities.is_some_and(|c| c.thread_pull_request_linking == Some(true)) {
        ThreadPullRequestLinkMode::Single
    } else {
        ThreadPullRequestLinkMode::Unsupported
    }
}

/// `ThreadPullRequestKey & { readonly url: string }`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadPullRequestReference {
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
}

/// Input of `thread.pull-request.link` (`source` is always `"manual"` from here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadPullRequestLinkInput {
    pub thread_id: ThreadId,
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
}

/// Input of `thread.pull-request.unlink`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadPullRequestUnlinkInput {
    pub thread_id: ThreadId,
    pub host: String,
    pub repository: String,
    pub number: u64,
}

/// The legacy `linkedPullRequest` of `thread.meta.update`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyLinkedPullRequestInput {
    pub project_id: ProjectId,
    pub repository: String,
    pub number: u64,
    pub url: String,
}

/// Input of `thread.meta.update` as far as pull request linking goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadMetaUpdateInput {
    pub thread_id: ThreadId,
    pub linked_pull_request: Option<LegacyLinkedPullRequestInput>,
}

/// The command to dispatch (`type`) with its input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadPullRequestMutation {
    /// `thread.pull-request.link`
    Link(ThreadPullRequestLinkInput),
    /// `thread.pull-request.unlink`
    Unlink(ThreadPullRequestUnlinkInput),
    /// `thread.meta.update`
    MetaUpdate(ThreadMetaUpdateInput),
}

#[derive(Debug, Clone, Copy)]
pub struct PlanThreadPullRequestMutationInput<'a> {
    pub capabilities: Option<&'a ExecutionEnvironmentCapabilities>,
    pub thread_id: &'a ThreadId,
    pub reference: &'a ThreadPullRequestReference,
    /// Older servers need the exact repository checkout; same-host routing is not available.
    pub legacy_project_id: Option<&'a ProjectId>,
    /// Provider selector for legacy APIs, which omit Azure organization and project paths.
    pub legacy_repository: Option<&'a str>,
    pub linked: bool,
}

pub fn plan_thread_pull_request_mutation(
    input: &PlanThreadPullRequestMutationInput<'_>,
) -> Option<ThreadPullRequestMutation> {
    let PlanThreadPullRequestMutationInput {
        capabilities,
        thread_id,
        reference,
        legacy_project_id,
        legacy_repository,
        linked,
    } = *input;
    let key = normalize_thread_pull_request_key(&ThreadPullRequestKeySource {
        host: &reference.host,
        repository: &reference.repository,
        number: reference.number,
        authority: None,
        url: Some(&reference.url),
    });
    match thread_pull_request_link_mode(capabilities) {
        ThreadPullRequestLinkMode::Multiple => Some(if linked {
            ThreadPullRequestMutation::Link(ThreadPullRequestLinkInput {
                thread_id: thread_id.clone(),
                host: key.host,
                repository: key.repository,
                number: key.number,
                url: reference.url.clone(),
            })
        } else {
            ThreadPullRequestMutation::Unlink(ThreadPullRequestUnlinkInput {
                thread_id: thread_id.clone(),
                host: key.host,
                repository: key.repository,
                number: key.number,
            })
        }),
        ThreadPullRequestLinkMode::Single => {
            if linked && legacy_project_id.is_none() {
                return None;
            }
            Some(ThreadPullRequestMutation::MetaUpdate(
                ThreadMetaUpdateInput {
                    thread_id: thread_id.clone(),
                    linked_pull_request: match legacy_project_id {
                        Some(project_id) if linked => Some(LegacyLinkedPullRequestInput {
                            project_id: project_id.clone(),
                            repository: legacy_repository
                                .unwrap_or(&reference.repository)
                                .to_owned(),
                            number: reference.number,
                            url: reference.url.clone(),
                        }),
                        _ => None,
                    },
                },
            ))
        }
        ThreadPullRequestLinkMode::Unsupported => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference() -> ThreadPullRequestReference {
        ThreadPullRequestReference {
            host: "github.example".to_owned(),
            repository: "team/repo".to_owned(),
            number: 7,
            url: "https://github.example/team/repo/pull/7".to_owned(),
        }
    }

    fn thread_id() -> ThreadId {
        ThreadId::new("thread").unwrap()
    }

    fn project_id() -> ProjectId {
        ProjectId::new("exact-checkout").unwrap()
    }

    fn capabilities(
        thread_pull_requests: Option<bool>,
        thread_pull_request_linking: Option<bool>,
    ) -> ExecutionEnvironmentCapabilities {
        ExecutionEnvironmentCapabilities {
            thread_pull_requests,
            thread_pull_request_linking,
            ..Default::default()
        }
    }

    /// The upstream test's shared `input`, with the fields a case overrides passed in.
    fn plan(
        capabilities: Option<&ExecutionEnvironmentCapabilities>,
        reference: &ThreadPullRequestReference,
        legacy_project_id: Option<&ProjectId>,
        legacy_repository: Option<&str>,
        linked: bool,
    ) -> Option<ThreadPullRequestMutation> {
        plan_thread_pull_request_mutation(&PlanThreadPullRequestMutationInput {
            capabilities,
            thread_id: &thread_id(),
            reference,
            legacy_project_id,
            legacy_repository,
            linked,
        })
    }

    #[test]
    fn preserves_forgejo_ports_in_a_multi_link_mutation() {
        for linked in [true, false] {
            let mutation = plan(
                Some(&capabilities(Some(true), None)),
                &ThreadPullRequestReference {
                    host: "forge.example".to_owned(),
                    repository: "team/repo".to_owned(),
                    number: 7,
                    url: "http://forge.example:3000/team/repo/pulls/7".to_owned(),
                },
                Some(&project_id()),
                None,
                linked,
            )
            .unwrap();
            let (host, number) = match mutation {
                ThreadPullRequestMutation::Link(input) => (input.host, input.number),
                ThreadPullRequestMutation::Unlink(input) => (input.host, input.number),
                other => panic!("expected a multi-link mutation, got {other:?}"),
            };
            assert_eq!(
                (host.as_str(), number),
                ("forge.example:3000", 7),
                "linked={linked}"
            );
        }
    }

    #[test]
    fn does_not_dispatch_when_linking_is_unadvertised() {
        let unadvertised = [
            None,
            Some(capabilities(None, None)),
            Some(capabilities(Some(false), Some(false))),
        ];
        for capabilities in unadvertised {
            assert_eq!(
                thread_pull_request_link_mode(capabilities.as_ref()),
                ThreadPullRequestLinkMode::Unsupported
            );
            assert_eq!(
                plan(
                    capabilities.as_ref(),
                    &reference(),
                    Some(&project_id()),
                    None,
                    true
                ),
                None
            );
        }
    }

    #[test]
    fn uses_metadata_updates_for_old_single_link_servers_including_unlink() {
        let single = capabilities(None, Some(true));
        let reference = reference();
        assert_eq!(
            plan(Some(&single), &reference, Some(&project_id()), None, true),
            Some(ThreadPullRequestMutation::MetaUpdate(
                ThreadMetaUpdateInput {
                    thread_id: thread_id(),
                    linked_pull_request: Some(LegacyLinkedPullRequestInput {
                        project_id: project_id(),
                        repository: reference.repository.clone(),
                        number: 7,
                        url: reference.url.clone(),
                    }),
                }
            ))
        );
        assert_eq!(
            plan(Some(&single), &reference, None, None, false),
            Some(ThreadPullRequestMutation::MetaUpdate(
                ThreadMetaUpdateInput {
                    thread_id: thread_id(),
                    linked_pull_request: None,
                }
            ))
        );
    }

    #[test]
    fn uses_the_checkouts_azure_selector_only_for_legacy_metadata_commands() {
        let azure = ThreadPullRequestReference {
            host: "dev.azure.com".to_owned(),
            repository: "org/project/_git/web".to_owned(),
            number: 42,
            url: "https://dev.azure.com/org/project/_git/web/pullrequest/42".to_owned(),
        };
        let single = plan(
            Some(&capabilities(None, Some(true))),
            &azure,
            Some(&project_id()),
            Some("web"),
            true,
        );
        let Some(ThreadPullRequestMutation::MetaUpdate(update)) = single else {
            panic!("expected a metadata update, got {single:?}");
        };
        let linked = update.linked_pull_request.unwrap();
        assert_eq!((linked.repository.as_str(), linked.number), ("web", 42));

        let multiple = plan(
            Some(&capabilities(Some(true), None)),
            &azure,
            Some(&project_id()),
            Some("web"),
            true,
        );
        let Some(ThreadPullRequestMutation::Link(link)) = multiple else {
            panic!("expected a link, got {multiple:?}");
        };
        assert_eq!(
            (link.repository.as_str(), link.number),
            ("org/project/_git/web", 42)
        );
    }

    #[test]
    fn never_sends_a_same_host_route_as_an_exact_repository_to_an_old_server() {
        assert_eq!(
            plan(
                Some(&capabilities(None, Some(true))),
                &reference(),
                None,
                None,
                true
            ),
            None
        );
    }

    #[test]
    fn prefers_multi_link_commands_when_available() {
        let preferring = [
            capabilities(Some(true), None),
            capabilities(Some(true), Some(true)),
        ];
        let reference = reference();
        for capabilities in &preferring {
            assert_eq!(
                plan(Some(capabilities), &reference, None, None, true),
                Some(ThreadPullRequestMutation::Link(
                    ThreadPullRequestLinkInput {
                        thread_id: thread_id(),
                        host: reference.host.clone(),
                        repository: reference.repository.clone(),
                        number: reference.number,
                        url: reference.url.clone(),
                    }
                ))
            );
            assert_eq!(
                plan(
                    Some(capabilities),
                    &reference,
                    Some(&project_id()),
                    None,
                    false
                ),
                Some(ThreadPullRequestMutation::Unlink(
                    ThreadPullRequestUnlinkInput {
                        thread_id: thread_id(),
                        host: reference.host.clone(),
                        repository: reference.repository.clone(),
                        number: reference.number,
                    }
                ))
            );
        }
    }
}
