//! Port of upstream/apps/web/src/hooks/usePullRequestLinking.ts: `canLinkChangeRequest`, and the
//! hook's `canLink` / `isLinked` / `changeLink` over one environment's capabilities and projects.
//!
//! `changeLink` here stops at [`PullRequestLinking::plan_change_link`], the mutation to send; the
//! pull request source dispatches it (`PullRequestSource::change_thread_link`), the way upstream's
//! `useAtomCommand` hands it to the environment.

#![allow(dead_code)]

use crate::pull_request::link_pull_request_dialog_logic::find_project_for_change_request;
use crate::state::helpers::thread_pull_request_compatibility::{
    PlanThreadPullRequestMutationInput, ThreadPullRequestLinkMode, ThreadPullRequestMutation,
    ThreadPullRequestReference, plan_thread_pull_request_mutation, thread_pull_request_link_mode,
};
use crate::state::support::change_request_url::{
    ChangeRequestLink, matches_linked_pull_request_url, parse_change_request_url,
};
use crate::state::support::thread_pull_requests::{
    ThreadPullRequestKeySource, source_control_repository_selector, thread_pull_request_keys_equal,
    visible_thread_pull_requests,
};
use turin_contracts::environment::{ExecutionEnvironmentCapabilities, RepositoryIdentity};
use turin_contracts::thread_pull_request::{ThreadLinkedPullRequest, ThreadPullRequestLink};
use turin_contracts::{ProjectId, ThreadId};

pub fn can_link_change_request<P>(
    capabilities: Option<&ExecutionEnvironmentCapabilities>,
    projects: &[P],
    parsed: &ChangeRequestLink,
    identity_of: impl Fn(&P) -> Option<&turin_contracts::environment::RepositoryIdentity>,
) -> bool {
    match thread_pull_request_link_mode(capabilities) {
        ThreadPullRequestLinkMode::Unsupported => false,
        ThreadPullRequestLinkMode::Multiple => true,
        ThreadPullRequestLinkMode::Single => {
            find_project_for_change_request(projects, identity_of, parsed).is_some()
        }
    }
}

/// A project of the environment, as far as linking reads it (`EnvironmentProject`).
#[derive(Clone, Debug)]
pub struct LinkingProject {
    pub id: ProjectId,
    pub repository_identity: Option<RepositoryIdentity>,
}

/// What `isLinked` reads of a thread.
#[derive(Clone, Copy, Debug, Default)]
pub struct LinkedThreadView<'a> {
    pub pull_requests: &'a [ThreadPullRequestLink],
    pub linked_pull_request: Option<&'a ThreadLinkedPullRequest>,
}

pub const NOT_AVAILABLE_IN_ENVIRONMENT: &str =
    "The pull request is not available in this environment.";
pub const LINKING_UNSUPPORTED: &str =
    "This environment does not support linking this pull request.";

/// `usePullRequestLinking(environmentId)`: link actions routed through what this environment
/// advertises.
#[derive(Clone, Debug)]
pub struct PullRequestLinking {
    capabilities: Option<ExecutionEnvironmentCapabilities>,
    /// The environment's own projects.
    projects: Vec<LinkingProject>,
}

impl PullRequestLinking {
    pub fn new(
        capabilities: Option<ExecutionEnvironmentCapabilities>,
        projects: Vec<LinkingProject>,
    ) -> Self {
        Self {
            capabilities,
            projects,
        }
    }

    pub fn mode(&self) -> ThreadPullRequestLinkMode {
        thread_pull_request_link_mode(self.capabilities.as_ref())
    }

    pub fn can_link(&self, url: &str) -> bool {
        parse_change_request_url(url).is_some_and(|parsed| {
            can_link_change_request(
                self.capabilities.as_ref(),
                &self.projects,
                &parsed,
                |project| project.repository_identity.as_ref(),
            )
        })
    }

    pub fn is_linked(&self, thread: Option<LinkedThreadView<'_>>, url: &str) -> bool {
        let mode = self.mode();
        let Some(thread) = thread else {
            return false;
        };
        if mode == ThreadPullRequestLinkMode::Unsupported {
            return false;
        }
        if mode != ThreadPullRequestLinkMode::Multiple {
            return thread
                .linked_pull_request
                .is_some_and(|linked| matches_linked_pull_request_url(linked.url.as_str(), url));
        }
        let Some(parsed) = parse_change_request_url(url) else {
            return false;
        };
        let target = ThreadPullRequestKeySource {
            host: &parsed.host,
            repository: &parsed.repository,
            number: parsed.number,
            authority: parsed.authority.as_deref(),
            url: None,
        };
        visible_thread_pull_requests(thread.pull_requests)
            .into_iter()
            .any(|entry| thread_pull_request_keys_equal(&entry.into(), &target))
    }

    /// `changeLink`, up to the command to send. An error is the sentence the dialog shows.
    pub fn plan_change_link(
        &self,
        thread_id: &ThreadId,
        url: &str,
        linked: bool,
    ) -> Result<ThreadPullRequestMutation, String> {
        let parsed = match parse_change_request_url(url) {
            Some(parsed) if !(linked && !self.can_link(url)) => parsed,
            _ => return Err(NOT_AVAILABLE_IN_ENVIRONMENT.to_owned()),
        };
        let legacy_project = find_project_for_change_request(
            &self.projects,
            |project| project.repository_identity.as_ref(),
            &parsed,
        );
        let legacy_repository = legacy_project
            .and_then(|project| project.repository_identity.as_ref())
            .and_then(source_control_repository_selector);
        let reference = ThreadPullRequestReference {
            host: parsed.host,
            repository: parsed.repository,
            number: parsed.number,
            url: url.to_owned(),
        };
        plan_thread_pull_request_mutation(&PlanThreadPullRequestMutationInput {
            capabilities: self.capabilities.as_ref(),
            thread_id,
            reference: &reference,
            legacy_project_id: legacy_project.map(|project| &project.id),
            legacy_repository: legacy_repository.as_deref(),
            linked,
        })
        .ok_or_else(|| LINKING_UNSUPPORTED.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::environment::ExecutionEnvironmentCapabilities;

    fn link() -> ChangeRequestLink {
        ChangeRequestLink {
            host: "github.com".into(),
            repository: "o/r".into(),
            number: 1,
            authority: None,
        }
    }

    #[test]
    fn multiple_mode_allows_without_project() {
        let caps = ExecutionEnvironmentCapabilities {
            thread_pull_requests: Some(true),
            ..Default::default()
        };
        assert!(can_link_change_request(
            Some(&caps),
            &[] as &[()],
            &link(),
            |_| unreachable!(),
        ));
    }

    fn multiple() -> ExecutionEnvironmentCapabilities {
        ExecutionEnvironmentCapabilities {
            thread_pull_requests: Some(true),
            ..Default::default()
        }
    }

    fn thread() -> ThreadId {
        ThreadId::new("thread-1").unwrap()
    }

    #[test]
    fn single_mode_needs_a_matching_project_to_link() {
        let capabilities = ExecutionEnvironmentCapabilities {
            thread_pull_request_linking: Some(true),
            ..Default::default()
        };
        let linking = PullRequestLinking::new(Some(capabilities), Vec::new());
        assert!(!linking.can_link("https://github.com/o/r/pull/1"));
        assert_eq!(
            linking.plan_change_link(&thread(), "https://github.com/o/r/pull/1", true),
            Err(NOT_AVAILABLE_IN_ENVIRONMENT.to_owned())
        );
    }

    #[test]
    fn an_environment_without_linking_refuses_every_link() {
        let linking = PullRequestLinking::new(None, Vec::new());
        assert_eq!(linking.mode(), ThreadPullRequestLinkMode::Unsupported);
        assert!(!linking.can_link("https://github.com/o/r/pull/1"));
        assert!(!linking.is_linked(None, "https://github.com/o/r/pull/1"));
    }

    #[test]
    fn multiple_mode_plans_a_link_and_an_unlink_by_host_repository_and_number() {
        let linking = PullRequestLinking::new(Some(multiple()), Vec::new());
        let url = "https://github.com/Acme/Turin/pull/7";
        let link = linking.plan_change_link(&thread(), url, true).unwrap();
        match link {
            ThreadPullRequestMutation::Link(input) => {
                assert_eq!(input.host, "github.com");
                assert_eq!(input.repository, "acme/turin");
                assert_eq!(input.number, 7);
                assert_eq!(input.url, url);
            }
            other => panic!("expected a link, got {other:?}"),
        }
        assert!(matches!(
            linking.plan_change_link(&thread(), url, false),
            Ok(ThreadPullRequestMutation::Unlink(_))
        ));
    }

    #[test]
    fn something_that_is_not_a_change_request_is_not_available() {
        let linking = PullRequestLinking::new(Some(multiple()), Vec::new());
        assert_eq!(
            linking.plan_change_link(&thread(), "https://example.com/", true),
            Err(NOT_AVAILABLE_IN_ENVIRONMENT.to_owned())
        );
    }
}
