//! Activity, review threads, thread comments, avatars, viewer access and candidates.

use indexmap::{IndexMap, IndexSet};
use serde_json::json;
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::pull_request::{
    PullRequestActor, PullRequestComment, PullRequestCommit, PullRequestLabelCandidateList,
    PullRequestReviewerCandidateList, PullRequestThreadCommentsResult,
};

use super::errors::{GitHubPullRequestApiError, GitHubSubjectScopeError};
use super::{
    GitHubPullRequestApi, GitHubPullRequestTarget, GraphqlRead, REVIEW_THREAD_PAGES,
    RepositorySelector, parse_repository_selector, variables,
};
use crate::github::graphql::{ReadGraphQlPagesOptions, read_graphql_pages};
use crate::github::pull_request_json::{
    ACTOR_AVATARS_GRAPHQL_QUERY, GitHubPullRequestActivity, GitHubReviewDismissalsPage,
    GitHubReviewThreadComments, GitHubReviewThreadEntry, GitHubReviewThreadPage, GitHubViewerFlags,
    GitHubViewerRepositoryAccess, LABEL_CANDIDATES_GRAPHQL_QUERY,
    PULL_REQUEST_ACTIVITY_GRAPHQL_QUERY, REVIEW_DISMISSALS_GRAPHQL_QUERY,
    REVIEW_THREAD_COMMENTS_GRAPHQL_QUERY, REVIEW_THREADS_GRAPHQL_QUERY,
    REVIEWER_CANDIDATES_GRAPHQL_QUERY, VIEWER_PERMISSIONS_GRAPHQL_QUERY, decode_actor_avatars_json,
    decode_label_candidates_json, decode_pull_request_activity_json, decode_review_dismissals_json,
    decode_review_thread_comments_json, decode_review_threads_json,
    decode_reviewer_candidates_json, decode_viewer_permissions_json, review_thread_conversation,
};

#[derive(Debug, Clone)]
pub struct ListActorAvatarsInput {
    pub cwd: String,
    pub repository: String,
    pub host: String,
    pub ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct GetReviewThreadCommentsInput {
    pub target: GitHubPullRequestTarget,
    pub thread_id: String,
    pub cursor: String,
}

#[derive(Debug, Clone)]
pub struct GetViewerAccessInput {
    pub target: GitHubPullRequestTarget,
    /// Manual action checks may use the quota held back from automatic reads.
    pub allow_reserve: bool,
}

impl GitHubPullRequestApi {
    pub async fn get_pull_request_activity(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<GitHubPullRequestActivity, GitHubPullRequestApiError> {
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        let mut author: Option<PullRequestActor> = None;
        let mut commits: Vec<PullRequestCommit> = Vec::new();
        let mut remarks: Vec<PullRequestComment> = Vec::new();
        let mut comments_after: Option<String> = None;
        let mut reviews_after: Option<String> = None;
        let mut with_comments = true;
        let mut with_reviews = true;
        // Both remark lists page on their own; a page asks only for the ones with more to give.
        let mut page = 0;
        while page < REVIEW_THREAD_PAGES && (with_comments || with_reviews) {
            let read = self
                .graphql_read(
                    GraphqlRead {
                        cwd: &target.cwd,
                        host: &target.host,
                        operation: "getPullRequestActivity",
                        allow_reserve: false,
                        variables: Some(variables(json!({
                            "owner": owner,
                            "name": name,
                            "number": target.number,
                            "head": page == 0,
                            "withComments": with_comments,
                            "commentsAfter": comments_after,
                            "withReviews": with_reviews,
                            "reviewsAfter": reviews_after,
                        }))),
                        query: PULL_REQUEST_ACTIVITY_GRAPHQL_QUERY.into(),
                    },
                    decode_pull_request_activity_json,
                )
                .await?;
            if page == 0 {
                author = read.author.flatten();
                commits = read.commits.unwrap_or_default();
            }
            remarks.extend(read.remarks);
            comments_after = read.next_comments_cursor;
            reviews_after = read.next_reviews_cursor;
            with_comments = with_comments && comments_after.is_some();
            with_reviews = with_reviews && reviews_after.is_some();
            page += 1;
        }
        remarks.sort_by(|left, right| left.created_at.cmp(&right.created_at));
        Ok(GitHubPullRequestActivity {
            author,
            comments: remarks,
            commits,
        })
    }

    async fn review_thread_page(
        &self,
        target: &GitHubPullRequestTarget,
        owner: &str,
        name: &str,
        cursor: Option<&str>,
    ) -> Result<GitHubReviewThreadPage, GitHubPullRequestApiError> {
        self.graphql_read(
            GraphqlRead {
                cwd: &target.cwd,
                host: &target.host,
                operation: "listReviewThreadComments",
                allow_reserve: false,
                variables: Some(variables(json!({
                    "owner": owner, "name": name, "number": target.number, "cursor": cursor
                }))),
                query: REVIEW_THREADS_GRAPHQL_QUERY.into(),
            },
            decode_review_threads_json,
        )
        .await
    }

    pub async fn list_review_thread_comments(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<GitHubReviewThreadComments, GitHubPullRequestApiError> {
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        let mut entries: Vec<GitHubReviewThreadEntry> = Vec::new();
        let mut avatars_by_login: IndexMap<String, String> = IndexMap::new();
        let mut bot_logins: IndexSet<String> = IndexSet::new();
        let mut commit_stats = IndexMap::new();
        let mut reviewers = Vec::new();
        let mut reactions = Vec::new();
        let mut reactions_by_id = IndexMap::new();
        let mut edited_at_by_id = IndexMap::new();
        let mut commits = Vec::new();
        let mut viewer = GitHubViewerFlags {
            can_update: true,
            did_author: false,
        };
        let mut dismissals_by_review_id: IndexMap<String, String> = IndexMap::new();
        let mut dismissal_cursor: Option<String> = None;
        let mut cursor: Option<String> = None;
        let mut page = 0;
        loop {
            let read = self
                .review_thread_page(target, &owner, &name, cursor.as_deref())
                .await?;
            entries.extend(read.threads);
            bot_logins.extend(read.bot_logins);
            avatars_by_login.extend(read.avatars_by_login);
            // The roster, the commits and the viewer's standing travel with every page, and the
            // first one already carries all of them.
            if page == 0 {
                reviewers = read.reviewers;
                reactions = read.reactions;
                reactions_by_id.extend(read.reactions_by_id);
                edited_at_by_id.extend(read.edited_at_by_id);
                commits = read.commits;
                viewer = read.viewer;
                dismissals_by_review_id.extend(read.dismissals_by_review_id);
                dismissal_cursor = read.next_dismissal_cursor;
                commit_stats.extend(read.commit_stats);
            }
            cursor = read.next_cursor;
            page += 1;
            if cursor.is_none() || page >= REVIEW_THREAD_PAGES {
                break;
            }
        }

        // Almost never entered: the embedded page already holds every dismissal a pull request
        // ordinarily accrues. Followed so a review whose event fell past that page still finds
        // its reason.
        if let Some(from) = dismissal_cursor {
            let pages = read_graphql_pages(
                async |cursor: Option<String>, _: &[GitHubReviewDismissalsPage]| {
                    self.graphql_read(
                        GraphqlRead {
                            cwd: &target.cwd,
                            host: &target.host,
                            operation: "listReviewThreadComments",
                            allow_reserve: false,
                            variables: Some(variables(json!({
                                "owner": owner, "name": name, "number": target.number, "cursor": cursor
                            }))),
                            query: REVIEW_DISMISSALS_GRAPHQL_QUERY.into(),
                        },
                        decode_review_dismissals_json,
                    )
                    .await
                },
                ReadGraphQlPagesOptions {
                    next_cursor: &|read: &GitHubReviewDismissalsPage| read.next_cursor.clone(),
                    from: Some(from),
                    max_pages: Some(REVIEW_THREAD_PAGES),
                    until: None,
                },
            )
            .await?;
            for read in pages.pages {
                dismissals_by_review_id.extend(read.dismissals_by_review_id);
            }
        }

        let review_threads: Vec<_> = entries
            .iter()
            .map(|entry| {
                let mut thread = entry.thread.clone();
                thread.comment_count = Some(entry.comment_count);
                if let Some(next) = &entry.next_comment_cursor {
                    thread.next_comments_cursor = TrimmedNonEmptyString::new(next).ok();
                }
                thread
            })
            .collect();
        Ok(GitHubReviewThreadComments {
            comments: review_thread_conversation(&review_threads),
            dismissals_by_review_id,
            review_threads,
            // GitHub's own count of each thread, so the number the page shows is the host's even
            // where a bound kept some of the words on GitHub.
            comment_count: entries.iter().map(|entry| entry.comment_count).sum(),
            truncated: cursor.is_some()
                || entries
                    .iter()
                    .any(|entry| entry.next_comment_cursor.is_some()),
            review_threads_truncated: cursor.is_some(),
            reactions,
            reactions_by_id,
            edited_at_by_id,
            reviewers,
            avatars_by_login,
            bot_logins,
            commit_stats,
            commits,
            viewer,
        })
    }

    pub async fn get_review_thread_comments(
        &self,
        input: &GetReviewThreadCommentsInput,
    ) -> Result<PullRequestThreadCommentsResult, GitHubPullRequestApiError> {
        let target = &input.target;
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        let page = self
            .graphql_read(
                GraphqlRead {
                    cwd: &target.cwd,
                    host: &target.host,
                    operation: "getReviewThreadComments",
                    allow_reserve: false,
                    variables: Some(variables(json!({
                        "owner": owner,
                        "name": name,
                        "number": target.number,
                        "threadId": input.thread_id,
                        "cursor": input.cursor,
                    }))),
                    query: REVIEW_THREAD_COMMENTS_GRAPHQL_QUERY.into(),
                },
                decode_review_thread_comments_json,
            )
            .await?;
        if !page.belongs_to_pull_request {
            return Err(GitHubSubjectScopeError {
                cwd: target.cwd.clone(),
                operation: "getReviewThreadComments".into(),
            }
            .into());
        }
        Ok(PullRequestThreadCommentsResult {
            comments: page.comments,
            next_cursor: page
                .next_cursor
                .and_then(|cursor| TrimmedNonEmptyString::new(cursor).ok()),
        })
    }

    /// One request for a listing's authors, since no `gh` JSON field reports an avatar.
    pub async fn list_actor_avatars(
        &self,
        input: &ListActorAvatarsInput,
    ) -> Result<IndexMap<String, String>, GitHubPullRequestApiError> {
        if input.ids.is_empty() {
            return Ok(IndexMap::new());
        }
        self.graphql_read(
            GraphqlRead {
                cwd: &input.cwd,
                host: &input.host,
                operation: "listActorAvatars",
                allow_reserve: false,
                variables: Some(variables(json!({ "ids": input.ids }))),
                query: ACTOR_AVATARS_GRAPHQL_QUERY.into(),
            },
            decode_actor_avatars_json,
        )
        .await
    }

    /// The viewer's standing on its own, for deciding a write without reading the whole detail.
    pub async fn get_viewer_access(
        &self,
        input: &GetViewerAccessInput,
    ) -> Result<GitHubViewerRepositoryAccess, GitHubPullRequestApiError> {
        let target = &input.target;
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        self.graphql_read(
            GraphqlRead {
                cwd: &target.cwd,
                host: &target.host,
                operation: "getViewerAccess",
                allow_reserve: input.allow_reserve,
                variables: Some(variables(
                    json!({ "owner": owner, "name": name, "number": target.number }),
                )),
                query: VIEWER_PERMISSIONS_GRAPHQL_QUERY.into(),
            },
            decode_viewer_permissions_json,
        )
        .await
    }

    /// Who this pull request may be sent to, and who it has already been sent to.
    pub async fn list_reviewer_candidates(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<PullRequestReviewerCandidateList, GitHubPullRequestApiError> {
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        self.graphql_read(
            GraphqlRead {
                cwd: &target.cwd,
                host: &target.host,
                operation: "listReviewerCandidates",
                allow_reserve: true,
                variables: Some(variables(
                    json!({ "owner": owner, "name": name, "number": target.number }),
                )),
                query: REVIEWER_CANDIDATES_GRAPHQL_QUERY.into(),
            },
            decode_reviewer_candidates_json,
        )
        .await
    }

    /// The repository's labels, and which of them this pull request already wears.
    pub async fn list_label_candidates(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<PullRequestLabelCandidateList, GitHubPullRequestApiError> {
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        self.graphql_read(
            GraphqlRead {
                cwd: &target.cwd,
                host: &target.host,
                operation: "listLabelCandidates",
                allow_reserve: true,
                variables: Some(variables(
                    json!({ "owner": owner, "name": name, "number": target.number }),
                )),
                query: LABEL_CANDIDATES_GRAPHQL_QUERY.into(),
            },
            decode_label_candidates_json,
        )
        .await
    }
}
