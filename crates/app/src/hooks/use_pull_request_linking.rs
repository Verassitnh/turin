//! Port of upstream/apps/web/src/hooks/usePullRequestLinking.ts (`canLinkChangeRequest`).

#![allow(dead_code)]

use crate::pull_request::link_pull_request_dialog_logic::find_project_for_change_request;
use crate::state::helpers::thread_pull_request_compatibility::{
    ThreadPullRequestLinkMode, thread_pull_request_link_mode,
};
use crate::state::support::change_request_url::ChangeRequestLink;
use turin_contracts::environment::ExecutionEnvironmentCapabilities;

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
}
