//! The host-native stack a pull request is in (`getPullRequestStack`).

use super::errors::GitHubPullRequestApiError;
use super::{
    GitHubPullRequestApi, GitHubPullRequestTarget, RepositorySelector, parse_repository_selector,
};
use crate::github::pull_request_json::{GitHubPullRequestStack, decode_pull_request_stacks_json};

#[derive(Debug, Clone)]
pub struct GetPullRequestStackInput {
    pub target: GitHubPullRequestTarget,
    pub include_details: bool,
}

impl GitHubPullRequestApi {
    /// The host-native stack this pull request is in, or `None` when it is in none — which is
    /// also the answer for a host that refuses the stacks preview altogether.
    pub async fn get_pull_request_stack(
        &self,
        input: &GetPullRequestStackInput,
    ) -> Result<Option<GitHubPullRequestStack>, GitHubPullRequestApiError> {
        let target = &input.target;
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        let result = async {
            let stack = self
                .rest_read(
                    &target.cwd,
                    &target.host,
                    "getPullRequestStack",
                    format!("repos/{owner}/{name}/stacks?pull_request={}", target.number),
                    decode_pull_request_stacks_json,
                )
                .await?;
            let Some(summary) = stack.as_ref().filter(|_| input.include_details) else {
                return Ok(stack);
            };
            self.rest_read(
                &target.cwd,
                &target.host,
                "getPullRequestStack",
                format!("repos/{owner}/{name}/stacks/{}", summary.number),
                |raw| decode_pull_request_stacks_json(&format!("[{raw}]")),
            )
            .await
        }
        .await;
        match result {
            // Hosts without the stacks preview return 404. Other failures must preserve the
            // previously synced stack and let the caller retry.
            Err(error) if error.is_not_found() => Ok(None),
            other => other,
        }
    }
}
