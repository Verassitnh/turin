//! Every write: actions (merge, auto-merge, update-branch, revert, approve-workflows, ready,
//! draft, close, reopen), comments, reviews, reactions, labels, reviewers and rewrites.

use serde::Deserialize;
use serde_json::{Value, json};
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestMergeMethod, PullRequestReactionContent,
    PullRequestReviewCommentDraft, PullRequestReviewVerdict, PullRequestReviewerKind,
    PullRequestStackHead, PullRequestUpdateMethod,
};

use super::detail::ListWorkflowRunsRequiringApprovalInput;
use super::errors::{
    GitHubPullRequestApiError, GitHubSubjectScopeError, GitHubWorkflowApprovalHeadChangedError,
    GitHubWorkflowApprovalHeadUnavailableError,
};
use super::{
    GitHubPullRequestApi, GitHubPullRequestTarget, GraphqlRead, RepositorySelector, decode_json,
    parse_repository_selector, variables,
};
use crate::github::api::{GitHubRestInput, RestMethod};
use crate::github::pull_request_json::de::{self, Int};
use crate::github::pull_request_json::{
    ADD_REACTION_GRAPHQL_MUTATION, REMOVE_REACTION_GRAPHQL_MUTATION,
    RESOLVE_REVIEW_THREAD_GRAPHQL_MUTATION, REVERT_PULL_REQUEST_GRAPHQL_MUTATION,
    REVIEW_THREAD_REPLY_GRAPHQL_MUTATION, UNRESOLVE_REVIEW_THREAD_GRAPHQL_MUTATION,
    UPDATE_ISSUE_COMMENT_GRAPHQL_MUTATION, UPDATE_PULL_REQUEST_GRAPHQL_MUTATION,
    UPDATE_REVIEW_COMMENT_GRAPHQL_MUTATION, build_label_request, build_review_submission,
    build_reviewer_request, github_reaction_content,
};
use crate::github::stack_actions::{GitHubStackActionInput, run_github_stack_action};
use crate::shared::js::encode_uri_component;
use crate::source_control_core::merge_message::remove_agent_credits;

const MERGE_MESSAGE_GRAPHQL_QUERY: &str = "
query PullRequestMergeMessage($owner: String!, $name: String!, $number: Int!, $method: PullRequestMergeMethod!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      isMergeQueueEnabled
      headRefOid
      viewerMergeBodyText(mergeType: $method)
    }
  }
}";

/// What `gh pr merge` and `gh pr update-branch` read before they act.
const ACTION_STATE_GRAPHQL_QUERY: &str = "
query PullRequestActionState($owner: String!, $name: String!, $number: Int!, $headRef: String!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      id
      headRefOid
      isMergeQueueEnabled
      mergeStateStatus
      baseRef { compare(headRef: $headRef) { behindBy } }
    }
  }
}";

const MERGE_PULL_REQUEST_GRAPHQL_MUTATION: &str = "mutation($input: MergePullRequestInput!) {
  mergePullRequest(input: $input) { clientMutationId }
}";
const ENABLE_AUTO_MERGE_GRAPHQL_MUTATION: &str =
    "mutation($input: EnablePullRequestAutoMergeInput!) {
  enablePullRequestAutoMerge(input: $input) { clientMutationId }
}";
const DISABLE_AUTO_MERGE_GRAPHQL_MUTATION: &str = "mutation($pullRequestId: ID!) {
  disablePullRequestAutoMerge(input: { pullRequestId: $pullRequestId }) { clientMutationId }
}";
const UPDATE_BRANCH_GRAPHQL_MUTATION: &str = "mutation($pullRequestId: ID!, $expectedHeadOid: GitObjectID!, $updateMethod: PullRequestBranchUpdateMethod!) {
  updatePullRequestBranch(input: { pullRequestId: $pullRequestId, expectedHeadOid: $expectedHeadOid, updateMethod: $updateMethod }) { clientMutationId }
}";
const READY_FOR_REVIEW_GRAPHQL_MUTATION: &str = "mutation($pullRequestId: ID!) {
  markPullRequestReadyForReview(input: { pullRequestId: $pullRequestId }) { clientMutationId }
}";
const CONVERT_TO_DRAFT_GRAPHQL_MUTATION: &str = "mutation($pullRequestId: ID!) {
  convertPullRequestToDraft(input: { pullRequestId: $pullRequestId }) { clientMutationId }
}";
const CLOSE_PULL_REQUEST_GRAPHQL_MUTATION: &str = "mutation($pullRequestId: ID!) {
  closePullRequest(input: { pullRequestId: $pullRequestId }) { clientMutationId }
}";
const REOPEN_PULL_REQUEST_GRAPHQL_MUTATION: &str = "mutation($pullRequestId: ID!) {
  reopenPullRequest(input: { pullRequestId: $pullRequestId }) { clientMutationId }
}";
const ADD_COMMENT_GRAPHQL_MUTATION: &str = "mutation($subjectId: ID!, $body: String!) {
  addComment(input: { subjectId: $subjectId, body: $body }) { clientMutationId }
}";

fn graphql_merge_method(method: PullRequestMergeMethod) -> &'static str {
    match method {
        PullRequestMergeMethod::Merge => "MERGE",
        PullRequestMergeMethod::Squash => "SQUASH",
        PullRequestMergeMethod::Rebase => "REBASE",
    }
}

/// States in which `gh pr merge --auto` merges at once instead of arming auto-merge.
const IMMEDIATELY_MERGEABLE: [&str; 3] = ["CLEAN", "HAS_HOOKS", "UNSTABLE"];

/// The mutations that only need the pull request's node id.
fn simple_action_mutation(action: PullRequestAction) -> Option<&'static str> {
    match action {
        PullRequestAction::DisableAutoMerge => Some(DISABLE_AUTO_MERGE_GRAPHQL_MUTATION),
        PullRequestAction::Ready => Some(READY_FOR_REVIEW_GRAPHQL_MUTATION),
        PullRequestAction::Draft => Some(CONVERT_TO_DRAFT_GRAPHQL_MUTATION),
        PullRequestAction::Close => Some(CLOSE_PULL_REQUEST_GRAPHQL_MUTATION),
        PullRequestAction::Reopen => Some(REOPEN_PULL_REQUEST_GRAPHQL_MUTATION),
        _ => None,
    }
}

#[derive(Deserialize)]
struct MergeMessageResponse {
    data: MergeMessageData,
}
#[derive(Deserialize)]
struct MergeMessageData {
    repository: MergeMessageRepository,
}
#[derive(Deserialize)]
struct MergeMessageRepository {
    #[serde(rename = "pullRequest")]
    pull_request: MergeMessage,
}
#[derive(Deserialize)]
struct MergeMessage {
    #[serde(rename = "isMergeQueueEnabled")]
    is_merge_queue_enabled: bool,
    #[serde(rename = "headRefOid")]
    head_ref_oid: String,
    #[serde(rename = "viewerMergeBodyText")]
    viewer_merge_body_text: String,
}

#[derive(Deserialize)]
struct ActionStateResponse {
    data: ActionStateData,
}
#[derive(Deserialize)]
struct ActionStateData {
    repository: ActionStateRepository,
}
#[derive(Deserialize)]
struct ActionStateRepository {
    #[serde(rename = "pullRequest")]
    pull_request: ActionState,
}
#[derive(Deserialize)]
struct ActionState {
    id: String,
    #[serde(rename = "headRefOid")]
    head_ref_oid: String,
    #[serde(default, rename = "isMergeQueueEnabled", deserialize_with = "de::opt")]
    is_merge_queue_enabled: Option<bool>,
    #[serde(default, rename = "mergeStateStatus")]
    merge_state_status: Option<String>,
    #[serde(default, rename = "baseRef")]
    base_ref: Option<ActionBaseRef>,
}
#[derive(Deserialize)]
struct ActionBaseRef {
    #[serde(deserialize_with = "de::nullable")]
    compare: Option<ActionCompare>,
}
#[derive(Deserialize)]
struct ActionCompare {
    #[serde(rename = "behindBy")]
    behind_by: Int,
}

#[derive(Debug, Clone)]
pub struct RunPullRequestActionInput {
    pub target: GitHubPullRequestTarget,
    pub action: PullRequestAction,
    pub stack_number: Option<u64>,
    pub expected_stack_heads: Option<Vec<PullRequestStackHead>>,
    pub remove_agent_credits_on_merge: bool,
    pub merge_method: Option<PullRequestMergeMethod>,
    pub update_method: Option<PullRequestUpdateMethod>,
}

impl RunPullRequestActionInput {
    pub fn new(target: GitHubPullRequestTarget, action: PullRequestAction) -> Self {
        Self {
            target,
            action,
            stack_number: None,
            expected_stack_heads: None,
            remove_agent_credits_on_merge: false,
            merge_method: None,
            update_method: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommentOnPullRequestInput {
    pub target: GitHubPullRequestTarget,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct SubmitReviewInput {
    pub target: GitHubPullRequestTarget,
    pub verdict: PullRequestReviewVerdict,
    pub body: String,
    pub comments: Vec<PullRequestReviewCommentDraft>,
}

#[derive(Debug, Clone)]
pub struct ReplyToReviewThreadInput {
    pub cwd: String,
    pub repository: String,
    pub host: String,
    pub thread_id: String,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct SetReviewThreadResolutionInput {
    pub cwd: String,
    pub repository: String,
    pub host: String,
    pub thread_id: String,
    pub resolved: bool,
}

#[derive(Debug, Clone)]
pub struct SetReviewerRequestInput {
    pub target: GitHubPullRequestTarget,
    pub reviewers: Vec<(String, PullRequestReviewerKind)>,
    /// False deletes the same collection a request posts to, which takes the request back.
    pub requested: bool,
}

#[derive(Debug, Clone)]
pub struct SetLabelsInput {
    pub target: GitHubPullRequestTarget,
    pub labels: Vec<String>,
    /// False takes each label off; true adds each to whatever is already there.
    pub applied: bool,
}

#[derive(Debug, Clone)]
pub struct SetReactionInput {
    pub target: GitHubPullRequestTarget,
    pub subject_id: Option<String>,
    pub content: PullRequestReactionContent,
    pub reacted: bool,
}

#[derive(Debug, Clone)]
pub struct UpdatePullRequestInput {
    pub target: GitHubPullRequestTarget,
    pub title: Option<String>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateCommentKind {
    IssueComment,
    ReviewComment,
}

#[derive(Debug, Clone)]
pub struct UpdateCommentInput {
    pub target: GitHubPullRequestTarget,
    pub comment_id: String,
    pub kind: UpdateCommentKind,
    pub body: String,
}

impl GitHubPullRequestApi {
    async fn node_id_mutation(
        &self,
        target: &GitHubPullRequestTarget,
        operation: &str,
        query: &str,
    ) -> Result<(), GitHubPullRequestApiError> {
        let pull_request_id = self.pull_request_node_id(target, operation).await?;
        self.graphql(
            &target.host,
            operation,
            query,
            variables(json!({ "pullRequestId": pull_request_id })),
        )
        .await
    }

    pub async fn run_pull_request_action(
        &self,
        input: &RunPullRequestActionInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        let target = &input.target;
        if let Some(stack_number) = input.stack_number {
            return run_github_stack_action(
                self.api(),
                self.inner.host.as_ref(),
                self.inner.clock.as_ref(),
                &GitHubStackActionInput {
                    cwd: target.cwd.clone(),
                    repository: target.repository.clone(),
                    host: target.host.clone(),
                    number: target.number,
                    stack_number,
                    expected_stack_heads: input.expected_stack_heads.clone(),
                    action: input.action,
                    merge_method: input.merge_method,
                },
            )
            .await;
        }
        if input.action == PullRequestAction::Revert {
            return self
                .node_id_mutation(
                    target,
                    "revertPullRequest",
                    REVERT_PULL_REQUEST_GRAPHQL_MUTATION,
                )
                .await;
        }
        if input.action == PullRequestAction::ApproveWorkflows {
            return self.approve_workflows(target).await;
        }
        if let Some(mutation) = simple_action_mutation(input.action) {
            return self
                .node_id_mutation(target, "runPullRequestAction", mutation)
                .await;
        }
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        // Read fresh rather than from the node id cache: merging and updating act on the head as
        // it stands now, and the merge queue decides which mutation a merge is.
        let state = self
            .graphql_read(
                GraphqlRead {
                    cwd: &target.cwd,
                    host: &target.host,
                    operation: "runPullRequestAction",
                    allow_reserve: true,
                    query: ACTION_STATE_GRAPHQL_QUERY.into(),
                    variables: Some(variables(json!({
                        "owner": owner,
                        "name": name,
                        "number": target.number,
                        "headRef": format!("refs/pull/{}/head", target.number),
                    }))),
                },
                |raw| {
                    decode_json::<ActionStateResponse>(raw).map(|r| r.data.repository.pull_request)
                },
            )
            .await?;
        if input.action == PullRequestAction::UpdateBranch {
            // Already current is done, the way `gh pr update-branch` reports it.
            if state
                .base_ref
                .as_ref()
                .and_then(|base_ref| base_ref.compare.as_ref())
                .is_some_and(|compare| compare.behind_by.0 == 0)
            {
                return Ok(());
            }
            // GitHub updates with a merge commit unless asked to rebase, which is its own default.
            return self
                .graphql(
                    &target.host,
                    "runPullRequestAction",
                    UPDATE_BRANCH_GRAPHQL_MUTATION,
                    variables(json!({
                        "pullRequestId": state.id,
                        "expectedHeadOid": state.head_ref_oid,
                        "updateMethod": if input.update_method == Some(PullRequestUpdateMethod::Rebase) {
                            "REBASE"
                        } else {
                            "MERGE"
                        },
                    })),
                )
                .await;
        }
        let mut body: Option<String> = None;
        let mut expected_head: Option<String> = None;
        if input.remove_agent_credits_on_merge
            && input.merge_method != Some(PullRequestMergeMethod::Rebase)
        {
            let message = self
                .graphql_read(
                    GraphqlRead {
                        cwd: &target.cwd,
                        host: &target.host,
                        operation: "runPullRequestAction",
                        allow_reserve: true,
                        query: MERGE_MESSAGE_GRAPHQL_QUERY.into(),
                        variables: Some(variables(json!({
                            "owner": owner,
                            "name": name,
                            "number": target.number,
                            "method": if input.merge_method == Some(PullRequestMergeMethod::Squash) {
                                "SQUASH"
                            } else {
                                "MERGE"
                            },
                        }))),
                    },
                    |raw| {
                        decode_json::<MergeMessageResponse>(raw)
                            .map(|response| response.data.repository.pull_request)
                    },
                )
                .await?;
            // GitHub's merge queue chooses its own message and ignores custom text.
            if !message.is_merge_queue_enabled {
                let cleaned = remove_agent_credits(&message.viewer_merge_body_text);
                if cleaned != message.viewer_merge_body_text {
                    body = Some(cleaned);
                    expected_head = Some(message.head_ref_oid);
                }
            }
        }
        // A merge queue takes a pull request through auto-merge rather than a direct merge, and
        // `--auto` on a pull request that is mergeable right now simply merges it, as `gh` does.
        // GitHub stores the strategy with a standing instruction rather than choosing one at
        // merge time, so arming still names it.
        let auto = state.is_merge_queue_enabled == Some(true)
            || (input.action == PullRequestAction::EnableAutoMerge
                && !IMMEDIATELY_MERGEABLE.contains(
                    &state
                        .merge_state_status
                        .as_deref()
                        .map(str::to_uppercase)
                        .unwrap_or_default()
                        .as_str(),
                ));
        let mut merge_input = serde_json::Map::new();
        merge_input.insert("pullRequestId".into(), Value::String(state.id));
        merge_input.insert(
            "mergeMethod".into(),
            Value::String(
                graphql_merge_method(input.merge_method.unwrap_or(PullRequestMergeMethod::Merge))
                    .into(),
            ),
        );
        if let Some(expected_head) = expected_head {
            merge_input.insert("expectedHeadOid".into(), Value::String(expected_head));
        }
        if let Some(body) = body {
            merge_input.insert("commitBody".into(), Value::String(body));
        }
        self.graphql(
            &target.host,
            "runPullRequestAction",
            if auto {
                ENABLE_AUTO_MERGE_GRAPHQL_MUTATION
            } else {
                MERGE_PULL_REQUEST_GRAPHQL_MUTATION
            },
            variables(json!({ "input": Value::Object(merge_input) })),
        )
        .await
    }

    async fn approve_workflows(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<(), GitHubPullRequestApiError> {
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        let detail = self.get_pull_request_detail(target).await?;
        if detail.is_cross_repository != Some(true) {
            return Ok(());
        }
        let (Some(expected_head_sha), Some(expected_owner)) = (
            detail.head_sha.clone(),
            detail.head_repository_owner.clone(),
        ) else {
            return Err(GitHubWorkflowApprovalHeadUnavailableError {
                cwd: target.cwd.clone(),
                number: target.number,
            }
            .into());
        };
        let expected_head_branch = detail.head_branch.clone();
        let runs = self
            .list_workflow_runs_requiring_approval(&ListWorkflowRunsRequiringApprovalInput {
                target: target.clone(),
                head_sha: expected_head_sha.clone(),
                head_branch: expected_head_branch.clone(),
                head_repository_owner: expected_owner.clone(),
            })
            .await?;
        for run in runs {
            let current = self.get_pull_request_detail(target).await?;
            let (Some(current_sha), Some(current_owner)) = (
                current.head_sha.clone(),
                current.head_repository_owner.clone(),
            ) else {
                return Err(GitHubWorkflowApprovalHeadUnavailableError {
                    cwd: target.cwd.clone(),
                    number: target.number,
                }
                .into());
            };
            if current.is_cross_repository != Some(true)
                || current_sha != expected_head_sha
                || current.head_branch != expected_head_branch
                || current_owner.to_lowercase() != expected_owner.to_lowercase()
            {
                return Err(GitHubWorkflowApprovalHeadChangedError {
                    cwd: target.cwd.clone(),
                    number: target.number,
                }
                .into());
            }
            let current_runs = self
                .list_workflow_runs_requiring_approval(&ListWorkflowRunsRequiringApprovalInput {
                    target: target.clone(),
                    head_sha: current_sha,
                    head_branch: current.head_branch.clone(),
                    head_repository_owner: current_owner,
                })
                .await?;
            if current_runs.iter().any(|current| current.id == run.id) {
                self.api()
                    .rest(GitHubRestInput {
                        host: target.host.clone(),
                        operation: "approveWorkflowRun".into(),
                        method: RestMethod::Post,
                        path: format!("repos/{owner}/{name}/actions/runs/{}/approve", run.id),
                        ..Default::default()
                    })
                    .await?;
            }
        }
        Ok(())
    }

    pub async fn comment_on_pull_request(
        &self,
        input: &CommentOnPullRequestInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        let subject_id = self
            .pull_request_node_id(&input.target, "commentOnPullRequest")
            .await?;
        self.graphql(
            &input.target.host,
            "commentOnPullRequest",
            ADD_COMMENT_GRAPHQL_MUTATION,
            variables(json!({ "subjectId": subject_id, "body": input.body })),
        )
        .await
    }

    pub async fn submit_review(
        &self,
        input: &SubmitReviewInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        let target = &input.target;
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        // The whole review is one request, so nothing is visible to anyone else until the verdict
        // is sent.
        self.api()
            .rest(GitHubRestInput {
                host: target.host.clone(),
                operation: "submitReview".into(),
                method: RestMethod::Post,
                path: format!("repos/{owner}/{name}/pulls/{}/reviews", target.number),
                body: Some(
                    build_review_submission(input.verdict, &input.body, &input.comments).to_value(),
                ),
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    pub async fn reply_to_review_thread(
        &self,
        input: &ReplyToReviewThreadInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        self.graphql(
            &input.host,
            "replyToReviewThread",
            REVIEW_THREAD_REPLY_GRAPHQL_MUTATION,
            variables(json!({ "threadId": input.thread_id, "body": input.body })),
        )
        .await
    }

    pub async fn set_review_thread_resolution(
        &self,
        input: &SetReviewThreadResolutionInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        self.graphql(
            &input.host,
            "setReviewThreadResolution",
            if input.resolved {
                RESOLVE_REVIEW_THREAD_GRAPHQL_MUTATION
            } else {
                UNRESOLVE_REVIEW_THREAD_GRAPHQL_MUTATION
            },
            variables(json!({ "threadId": input.thread_id })),
        )
        .await
    }

    pub async fn set_reviewer_request(
        &self,
        input: &SetReviewerRequestInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        let target = &input.target;
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        // Posting to a login GitHub has already been asked about is what a re-request is, so
        // there is nothing to say here about somebody who has reviewed once already.
        self.api()
            .rest(GitHubRestInput {
                host: target.host.clone(),
                operation: "setReviewerRequest".into(),
                method: if input.requested {
                    RestMethod::Post
                } else {
                    RestMethod::Delete
                },
                path: format!(
                    "repos/{owner}/{name}/pulls/{}/requested_reviewers",
                    target.number
                ),
                body: Some(build_reviewer_request(&input.reviewers).to_value()),
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    pub async fn set_labels(
        &self,
        input: &SetLabelsInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        let target = &input.target;
        let RepositorySelector { owner, name } = parse_repository_selector(&target.repository);
        // A pull request is an issue to the labels API. Adding posts a list and leaves what was
        // already there; taking off is one delete per label, since the endpoint names one in its
        // path. The name goes into the path encoded, because a label may carry a space or a slash.
        let issue = format!("repos/{owner}/{name}/issues/{}/labels", target.number);
        if input.applied {
            self.api()
                .rest(GitHubRestInput {
                    host: target.host.clone(),
                    operation: "setLabels".into(),
                    method: RestMethod::Post,
                    path: issue,
                    body: Some(build_label_request(&input.labels).to_value()),
                    ..Default::default()
                })
                .await?;
            return Ok(());
        }
        for label in &input.labels {
            self.api()
                .rest(GitHubRestInput {
                    host: target.host.clone(),
                    operation: "setLabels".into(),
                    method: RestMethod::Delete,
                    path: format!("{issue}/{}", encode_uri_component(label)),
                    ..Default::default()
                })
                .await?;
        }
        Ok(())
    }

    /// Adds a reaction to a remark, or takes it back. `subject_id` is any node GitHub calls
    /// reactable — a comment, a review, or the pull request itself, which is looked up here
    /// because nothing in the conversation names it. A given `subject_id` is confirmed to belong
    /// to this pull request before the mutation runs, since nothing else ties the two together.
    pub async fn set_reaction(
        &self,
        input: &SetReactionInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        let target = &input.target;
        let subject_id = match &input.subject_id {
            None => self.pull_request_node_id(target, "setReaction").await?,
            Some(given) => {
                if !self
                    .subject_belongs_to_pull_request(target, given, "setReaction")
                    .await?
                {
                    return Err(GitHubSubjectScopeError {
                        cwd: target.cwd.clone(),
                        operation: "setReaction".into(),
                    }
                    .into());
                }
                given.clone()
            }
        };
        self.graphql(
            &target.host,
            "setReaction",
            if input.reacted {
                ADD_REACTION_GRAPHQL_MUTATION
            } else {
                REMOVE_REACTION_GRAPHQL_MUTATION
            },
            variables(json!({
                "subjectId": subject_id,
                "content": github_reaction_content(input.content),
            })),
        )
        .await
    }

    /// Rewrites the pull request's own words, leaving whichever of the two was not given.
    pub async fn update_pull_request(
        &self,
        input: &UpdatePullRequestInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        let pull_request_id = self
            .pull_request_node_id(&input.target, "updatePullRequest")
            .await?;
        // A field the caller did not name is left out of the request entirely, so GitHub keeps the
        // words that are there rather than being asked for an empty one.
        let mut vars = serde_json::Map::new();
        vars.insert("pullRequestId".into(), Value::String(pull_request_id));
        if let Some(title) = &input.title {
            vars.insert("title".into(), Value::String(title.clone()));
        }
        if let Some(body) = &input.body {
            vars.insert("body".into(), Value::String(body.clone()));
        }
        self.graphql(
            &input.target.host,
            "updatePullRequest",
            UPDATE_PULL_REQUEST_GRAPHQL_MUTATION,
            vars,
        )
        .await
    }

    /// Rewrites a remark. `comment_id` is trusted to be whatever node it names, so it is
    /// confirmed to belong to this pull request before the mutation runs, the way a reaction
    /// subject is. Whether the remark is the reader's to rewrite is GitHub's own answer, not one
    /// asked here.
    pub async fn update_comment(
        &self,
        input: &UpdateCommentInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        let target = &input.target;
        if !self
            .subject_belongs_to_pull_request(target, &input.comment_id, "updateComment")
            .await?
        {
            return Err(GitHubSubjectScopeError {
                cwd: target.cwd.clone(),
                operation: "updateComment".into(),
            }
            .into());
        }
        self.graphql(
            &target.host,
            "updateComment",
            match input.kind {
                UpdateCommentKind::IssueComment => UPDATE_ISSUE_COMMENT_GRAPHQL_MUTATION,
                UpdateCommentKind::ReviewComment => UPDATE_REVIEW_COMMENT_GRAPHQL_MUTATION,
            },
            variables(json!({ "commentId": input.comment_id, "body": input.body })),
        )
        .await
    }
}
