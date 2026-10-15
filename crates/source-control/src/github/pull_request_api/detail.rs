//! Detail (core), preview and workflow runs awaiting approval.

use futures::future::try_join;
use serde_json::json;

use super::errors::{
    GitHubPullRequestApiError, GitHubWorkflowApprovalRefusedError,
    GitHubWorkflowApprovalRefusedReason,
};
use super::{
    GitHubPullRequestApi, GitHubPullRequestTarget, GraphqlRead, RepositorySelector,
    parse_repository_selector, variables,
};
use crate::github::conditional_checks::known_workflow_runs;
use crate::github::graphql::{ReadGraphQlPagesOptions, read_graphql_pages};
use crate::github::pull_request_json::{
    GitHubCheckContext, GitHubCheckContextsPage, GitHubPullRequestCore, GitHubPullRequestHead,
    GitHubPullRequestHeadsPage, GitHubWorkflowRunApproval, PULL_REQUEST_HEADS_GRAPHQL_QUERY,
    PULL_REQUEST_PREVIEW_GRAPHQL_QUERY, decode_pull_request_check_contexts_json,
    decode_pull_request_core_json, decode_pull_request_heads_json,
    decode_pull_request_preview_json, decode_workflow_runs_json,
    pull_request_check_contexts_graphql_query, pull_request_checks_from_contexts,
    pull_request_core_graphql_query,
};
use crate::shared::js::{encode_uri_component, js_trim};
use crate::source_control_core::ambient;
use crate::source_control_core::pull_request_provider::ProviderChangeRequestPreview;

/// Pages of a hundred check contexts to walk for one head before the rollup says it was cut.
const CHECK_CONTEXT_PAGES: usize = 10;

pub(crate) const WORKFLOW_APPROVAL_LIMIT: usize = 1_000;

/// `Omit<PullRequestPreview, "projectId" | "repository">` is the provider's neutral preview.
pub type GitHubPullRequestPreview = ProviderChangeRequestPreview;

#[derive(Debug, Clone)]
pub struct ListWorkflowRunsRequiringApprovalInput {
    pub target: GitHubPullRequestTarget,
    pub head_sha: String,
    pub head_branch: String,
    pub head_repository_owner: String,
}

/// Every check context of the head commit, a page at a time.
struct AllCheckContexts {
    head_sha: Option<String>,
    contexts: Vec<GitHubCheckContext>,
    truncated: bool,
}

impl GitHubPullRequestApi {
    /// Every check context of the head commit, a page at a time, for a rollup the detail read cut
    /// at its first hundred. Each page names the head it read, so a push mid-walk fails rather
    /// than mixing two revisions' checks.
    async fn read_all_check_contexts(
        &self,
        target: &GitHubPullRequestTarget,
        allow_reserve: bool,
    ) -> Result<AllCheckContexts, GitHubPullRequestApiError> {
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        let (owner, name) = (&owner, &name);
        let pages = read_graphql_pages(
            |after: Option<String>, previous: &[GitHubCheckContextsPage]| {
                // Read before the request starts: the future must not borrow the pages so far.
                let first_head = previous.first().map(|page| page.head_sha.clone());
                async move {
                let page = self
                    .graphql_read(
                        GraphqlRead {
                            cwd: &target.cwd,
                            host: &target.host,
                            operation: "getPullRequestDetail",
                            allow_reserve,
                            variables: Some(variables(json!({
                                "owner": owner, "name": name, "number": target.number, "after": after
                            }))),
                            query: pull_request_check_contexts_graphql_query(&target.host),
                        },
                        decode_pull_request_check_contexts_json,
                    )
                    .await?;
                // Each page names the head it read, so a push mid-walk fails rather than mixing
                // two revisions' checks.
                if first_head.is_some_and(|first_head| page.head_sha != first_head) {
                    return Err(Self::read_error_message(
                        &target.cwd,
                        "getPullRequestDetail",
                        "Pull request head changed while reading checks.",
                    ));
                }
                Ok(page)
                }
            },
            ReadGraphQlPagesOptions {
                next_cursor: &|page: &GitHubCheckContextsPage| page.next_cursor.clone(),
                from: None,
                max_pages: Some(CHECK_CONTEXT_PAGES),
                until: None,
            },
        )
        .await?;
        Ok(AllCheckContexts {
            head_sha: pages.pages.first().map(|page| page.head_sha.clone()),
            contexts: pages
                .pages
                .iter()
                .flat_map(|page| page.contexts.iter().cloned())
                .collect(),
            truncated: pages.truncated,
        })
    }

    pub async fn get_pull_request_detail(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<GitHubPullRequestCore, GitHubPullRequestApiError> {
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        let allow_reserve = ambient::interactive();
        let mut core = self
            .graphql_read(
                GraphqlRead {
                    cwd: &target.cwd,
                    host: &target.host,
                    operation: "getPullRequestDetail",
                    allow_reserve,
                    variables: Some(variables(json!({
                        "owner": owner,
                        "name": name,
                        "number": target.number,
                        "headRef": format!("refs/pull/{}/head", target.number),
                    }))),
                    query: pull_request_core_graphql_query(&target.host),
                },
                decode_pull_request_core_json,
            )
            .await?;
        // Past a hundred checks the first page would let the rest imply success, so the whole
        // rollup is walked instead.
        if !core.checks_truncated {
            return Ok(core);
        }
        let all = self.read_all_check_contexts(target, allow_reserve).await?;
        if all.head_sha != core.head_sha {
            return Err(Self::read_error_message(
                &target.cwd,
                "getPullRequestDetail",
                "Pull request head changed while reading checks.",
            ));
        }
        let rollup = pull_request_checks_from_contexts(&all.contexts);
        core.checks = rollup.checks;
        core.checks_state = rollup.checks_state;
        core.checks_truncated = all.truncated;
        Ok(core)
    }

    pub async fn get_pull_request_preview(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<GitHubPullRequestPreview, GitHubPullRequestApiError> {
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        self.graphql_read(
            GraphqlRead {
                cwd: &target.cwd,
                host: &target.host,
                operation: "getPullRequestPreview",
                allow_reserve: false,
                variables: Some(variables(
                    json!({ "owner": owner, "name": name, "number": target.number }),
                )),
                query: PULL_REQUEST_PREVIEW_GRAPHQL_QUERY.into(),
            },
            decode_pull_request_preview_json,
        )
        .await
    }

    /// Every open pull request whose head branch carries this name, up to one past the limit.
    /// `truncated` says some were never read, which is never enough to call a head unique.
    async fn list_heads_by_branch(
        &self,
        target: &GitHubPullRequestTarget,
        head_branch: &str,
    ) -> Result<(Vec<GitHubPullRequestHead>, bool), GitHubPullRequestApiError> {
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        let count_heads = |pages: &[GitHubPullRequestHeadsPage]| {
            pages.iter().map(|page| page.heads.len()).sum::<usize>()
        };
        let (owner, name) = (&owner, &name);
        let pages = read_graphql_pages(
            |after: Option<String>, _: &[GitHubPullRequestHeadsPage]| async move {
                self.graphql_read(
                    GraphqlRead {
                        cwd: &target.cwd,
                        host: &target.host,
                        operation: "listWorkflowRunsRequiringApproval",
                        allow_reserve: true,
                        variables: Some(variables(json!({
                            "owner": owner, "name": name, "head": head_branch, "after": after
                        }))),
                        query: PULL_REQUEST_HEADS_GRAPHQL_QUERY.into(),
                    },
                    decode_pull_request_heads_json,
                )
                .await
            },
            ReadGraphQlPagesOptions {
                next_cursor: &|page: &GitHubPullRequestHeadsPage| page.next_cursor.clone(),
                from: None,
                max_pages: None,
                until: Some(&|pages: &[GitHubPullRequestHeadsPage]| {
                    count_heads(pages) > WORKFLOW_APPROVAL_LIMIT
                }),
            },
        )
        .await?;
        Ok((
            pages
                .pages
                .into_iter()
                .flat_map(|page| page.heads)
                .collect(),
            pages.truncated,
        ))
    }

    /// The runs waiting on a maintainer for this exact head, up to one past the limit. `head_sha`
    /// is what scopes them; `head_branch` is checked as well, the way `gh run list --branch` did.
    async fn list_action_required_runs(
        &self,
        input: &ListWorkflowRunsRequiringApprovalInput,
    ) -> Result<Vec<GitHubWorkflowRunApproval>, GitHubPullRequestApiError> {
        let target = &input.target;
        // The checks revalidator may have just confirmed every run of this head as current.
        if let Some(known) = known_workflow_runs()
            && known.head_sha == input.head_sha
        {
            return Ok(known
                .runs
                .iter()
                .filter(|run| {
                    // A run waiting on a maintainer reports `completed` with an `action_required`
                    // conclusion; the `status=action_required` query matches either.
                    (run.conclusion.as_deref() == Some("action_required")
                        || run.status.as_deref() == Some("action_required"))
                        && run.head_branch.as_deref() == Some(input.head_branch.as_str())
                })
                .map(|run| GitHubWorkflowRunApproval {
                    id: run.id,
                    name: run
                        .name
                        .as_deref()
                        .map(js_trim)
                        .filter(|name| !name.is_empty())
                        .map_or_else(|| format!("Workflow run {}", run.id), str::to_owned),
                    url: run
                        .html_url
                        .as_deref()
                        .map(js_trim)
                        .filter(|url| !url.is_empty())
                        .map(str::to_owned),
                })
                .collect());
        }
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        let mut runs: Vec<GitHubWorkflowRunApproval> = Vec::new();
        let mut page = 1;
        while runs.len() <= WORKFLOW_APPROVAL_LIMIT {
            let read = self
                .rest_read(
                    &target.cwd,
                    &target.host,
                    "listWorkflowRunsRequiringApproval",
                    format!(
                        "repos/{owner}/{name}/actions/runs?head_sha={}&branch={}&event=pull_request&status=action_required&per_page=100&page={page}",
                        encode_uri_component(&input.head_sha),
                        encode_uri_component(&input.head_branch),
                    ),
                    decode_workflow_runs_json,
                )
                .await
                .map_err(|error| {
                    Self::read_error(
                        &target.cwd,
                        "listWorkflowRunsRequiringApproval",
                        std::sync::Arc::new(error),
                    )
                })?;
            runs.extend(read.runs);
            if read.raw_count < 100 {
                break;
            }
            page += 1;
        }
        Ok(runs)
    }

    pub async fn list_workflow_runs_requiring_approval(
        &self,
        input: &ListWorkflowRunsRequiringApprovalInput,
    ) -> Result<Vec<GitHubWorkflowRunApproval>, GitHubPullRequestApiError> {
        let target = &input.target;
        let heads = async {
            let (heads, truncated) = self
                .list_heads_by_branch(target, &input.head_branch)
                .await?;
            let exact_heads: Vec<_> = heads
                .iter()
                .filter(|pull_request| {
                    pull_request.head_sha == input.head_sha
                        && pull_request.is_cross_repository == Some(true)
                        && pull_request
                            .head_repository_owner
                            .as_deref()
                            .map(str::to_lowercase)
                            == Some(input.head_repository_owner.to_lowercase())
                })
                .collect();
            if truncated || heads.len() > WORKFLOW_APPROVAL_LIMIT {
                return Err(GitHubPullRequestApiError::from(
                    GitHubWorkflowApprovalRefusedError {
                        cwd: target.cwd.clone(),
                        number: target.number,
                        reason: GitHubWorkflowApprovalRefusedReason::HeadListTruncated,
                        observed_count: heads.len() as u64,
                        limit: WORKFLOW_APPROVAL_LIMIT as u64,
                    },
                ));
            }
            if exact_heads.len() != 1 || exact_heads[0].number != target.number {
                return Err(GitHubWorkflowApprovalRefusedError {
                    cwd: target.cwd.clone(),
                    number: target.number,
                    reason: GitHubWorkflowApprovalRefusedReason::HeadNotUnique,
                    observed_count: exact_heads.len() as u64,
                    limit: WORKFLOW_APPROVAL_LIMIT as u64,
                }
                .into());
            }
            Ok(())
        };
        let runs = async {
            let runs = self.list_action_required_runs(input).await?;
            if runs.len() > WORKFLOW_APPROVAL_LIMIT {
                return Err(GitHubPullRequestApiError::from(
                    GitHubWorkflowApprovalRefusedError {
                        cwd: target.cwd.clone(),
                        number: target.number,
                        reason: GitHubWorkflowApprovalRefusedReason::RunListTruncated,
                        observed_count: runs.len() as u64,
                        limit: WORKFLOW_APPROVAL_LIMIT as u64,
                    },
                ));
            }
            Ok(runs)
        };
        let ((), runs) = try_join(heads, runs).await?;
        Ok(runs)
    }
}
