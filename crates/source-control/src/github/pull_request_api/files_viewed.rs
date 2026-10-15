//! Files viewed (`getPullRequestFilesViewed`, `setPullRequestFilesViewed`).

use serde_json::json;
use turin_contracts::pull_request::PullRequestFileViewed;

use super::errors::GitHubPullRequestApiError;
use super::{
    GitHubPullRequestApi, GitHubPullRequestTarget, GraphqlRead, RepositorySelector,
    parse_repository_selector, variables,
};
use crate::github::graphql::{ReadGraphQlPagesOptions, read_graphql_pages};
use crate::github::pull_request_json::{
    GitHubPullRequestFilesViewedPage, PULL_REQUEST_FILES_VIEWED_GRAPHQL_QUERY,
    build_set_files_viewed_graphql_mutation, decode_pull_request_files_viewed_json,
};

/// How many hundred-file pages of viewed state one read will walk. A point of the hourly GraphQL
/// budget per page, against a change request nobody reviews in one sitting past the first few
/// hundred files: beyond this the read stops and says it was cut short.
const FILES_VIEWED_MAX_PAGES: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubPullRequestFilesViewed {
    pub files: Vec<PullRequestFileViewed>,
    /// GitHub had more files than the page budget would read.
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct SetPullRequestFilesViewedInput {
    pub target: GitHubPullRequestTarget,
    pub files: Vec<ViewedChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewedChange {
    pub path: String,
    pub viewed: bool,
}

impl GitHubPullRequestApi {
    /// Which files of the pull request the signed-in account has cleared, and which of those have
    /// been pushed to since. Read apart from the patch because GitHub only reports it over GraphQL.
    pub async fn get_pull_request_files_viewed(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<GitHubPullRequestFilesViewed, GitHubPullRequestApiError> {
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        let (owner, name) = (&owner, &name);
        let pages = read_graphql_pages(
            |after: Option<String>, _: &[GitHubPullRequestFilesViewedPage]| async move {
                self.graphql_read(
                    GraphqlRead {
                        cwd: &target.cwd,
                        host: &target.host,
                        operation: "getPullRequestFilesViewed",
                        allow_reserve: false,
                        variables: Some(variables(json!({
                            "owner": owner, "name": name, "number": target.number, "after": after
                        }))),
                        query: PULL_REQUEST_FILES_VIEWED_GRAPHQL_QUERY.into(),
                    },
                    decode_pull_request_files_viewed_json,
                )
                .await
            },
            ReadGraphQlPagesOptions {
                next_cursor: &|page: &GitHubPullRequestFilesViewedPage| page.next_cursor.clone(),
                from: None,
                max_pages: Some(FILES_VIEWED_MAX_PAGES),
                until: None,
            },
        )
        .await?;
        Ok(GitHubPullRequestFilesViewed {
            files: pages
                .pages
                .into_iter()
                .flat_map(|page| page.files)
                .map(|file| PullRequestFileViewed {
                    path: file.path,
                    state: file.state,
                })
                .collect(),
            truncated: pages.truncated,
        })
    }

    /// Clears files, or puts them back, as one request. GitHub takes a single path per mutation,
    /// so a burst is batched with aliases into one document rather than one subprocess per press.
    pub async fn set_pull_request_files_viewed(
        &self,
        input: &SetPullRequestFilesViewedInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        if input.files.is_empty() {
            return Ok(());
        }
        let pull_request_id = self
            .pull_request_node_id(&input.target, "setPullRequestFilesViewed")
            .await?;
        let files: Vec<(String, bool)> = input
            .files
            .iter()
            .map(|file| (file.path.clone(), file.viewed))
            .collect();
        match build_set_files_viewed_graphql_mutation(&pull_request_id, &files) {
            None => Ok(()),
            Some(mutation) => {
                self.graphql(
                    &input.target.host,
                    "setPullRequestFilesViewed",
                    &mutation.query,
                    mutation.variables,
                )
                .await
            }
        }
    }
}
