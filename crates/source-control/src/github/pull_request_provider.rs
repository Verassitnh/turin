//! Port of upstream/packages/source-control-github/src/server/GitHubPullRequestProvider.ts
//!
//! The GitHub implementation of the neutral [`PullRequestProviderApi`]: it translates the
//! `GitHub*` shapes the [`GitHubPullRequestApi`] service reads into the `Provider*` ones the
//! pull request service consumes, maps every failure to a [`PullRequestProviderError`], and fills
//! in what GitHub's reads leave out (avatars, fork workflow approvals, viewer permissions).
//!
//! Deviations, all mechanical:
//! - Upstream reaches the service through its Effect context, which its tests replace with
//!   `Layer.mock`. Here the provider talks to the service through the narrow
//!   [`GitHubPullRequestService`] trait (implemented by [`GitHubPullRequestApi`] by forwarding),
//!   so the tests can script it the same way.
//! - `withVerifiedCredential(input, use)` is [`PullRequestProviderApi::verify_credential`]; the
//!   caller runs its work under the returned pin, so an operation's own failure can never be
//!   relabelled by this module.

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use futures::future::{BoxFuture, try_join};
use indexmap::{IndexMap, IndexSet};
use regex::Regex;
use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestActor, PullRequestBaseComparison, PullRequestCapabilities,
    PullRequestCheck, PullRequestCheckStatus, PullRequestChecks, PullRequestComment,
    PullRequestCommentKind, PullRequestEditCapabilities, PullRequestLabelCandidateList,
    PullRequestMergeMethod, PullRequestReviewCapabilities, PullRequestReviewThread,
    PullRequestReviewVerdict, PullRequestReviewerCandidateList, PullRequestReviewerCapabilities,
    PullRequestState, PullRequestThreadCommentsResult, PullRequestUpdateMethod,
    PullRequestViewedFilesStore, PullRequestViewerPermissions,
};
use turin_contracts::source_control::SourceControlProviderKind;

use super::api::GitHubApiError;
use super::conditional_checks::ChecksRead;
use super::pull_request_api::{
    self as api, GetPullRequestDiffFileContentsInput, GetPullRequestDiffInput,
    GetPullRequestStackInput, GetViewerAccessInput, GitHubPullRequestApi,
    GitHubPullRequestApiError, GitHubPullRequestDiffFileContents, GitHubPullRequestDiffSlice,
    GitHubPullRequestFilesViewed, GitHubPullRequestListBatch, GitHubPullRequestSearchBatch,
    GitHubPullRequestStat, GitHubPullRequestTarget, ListActorAvatarsInput,
    ListPullRequestStatsInput, ListPullRequestsInput, ListWorkflowRunsRequiringApprovalInput,
    SearchPullRequestsInput, SetPullRequestFilesViewedInput, ViewedChange,
};
use super::pull_request_json::{
    ChangeRequestSelector, GitHubPullRequestActivity, GitHubPullRequestCore,
    GitHubPullRequestDetail, GitHubPullRequestListItem, GitHubPullRequestSearchItem,
    GitHubPullRequestStack, GitHubPullRequestWatchFingerprint, GitHubReviewThreadComments,
    GitHubViewerAccess, GitHubViewerFlags, GitHubViewerRepositoryAccess, GitHubWorkflowRunApproval,
};
use crate::source_control_core::Cause;
use crate::source_control_core::ambient::{self, PinnedCredential};
use crate::source_control_core::merge_message::remove_agent_credits;
use crate::source_control_core::pull_request_provider::{
    CommentInput, CommentUpdateKind, GetChangeRequestStackInput, GetDiffFileContentsInput,
    GetDiffInput, GetReviewThreadCommentsInput, GetViewerPermissionsInput,
    ListChangeRequestStatsInput, ListChangeRequestsAcrossInput, ListChangeRequestsInput,
    ProviderBatchedChangeRequest, ProviderBatchedChangeRequestPage, ProviderChangeRequest,
    ProviderChangeRequestActivity, ProviderChangeRequestDetail, ProviderChangeRequestPage,
    ProviderChangeRequestPreview, ProviderChangeRequestRef, ProviderChangeRequestStack,
    ProviderChangeRequestStackLayer, ProviderChangeRequestStat, ProviderChangeRequestSummary,
    ProviderChangeRequestWatchFingerprint, ProviderDiffFileContents, ProviderDiffSlice,
    ProviderFilesViewed, ProviderResult, PullRequestOperation, PullRequestProviderApi,
    PullRequestProviderError, PullRequestProviderErrorReason, PullRequestProviderFailure,
    ReplyToThreadInput, RoutingIdentity, RunActionInput, SetFilesViewedInput, SetLabelsInput,
    SetReactionInput, SetReviewerRequestInput, SetThreadResolutionInput, SubmitReviewInput,
    UpdateChangeRequestInput, UpdateCommentInput, VerifiedCredentialInput, VerifiedIdentity,
};

#[cfg(test)]
mod tests;

const KIND: &str = "github";

fn kind() -> SourceControlProviderKind {
    SourceControlProviderKind::new(KIND).expect("github is a valid kind")
}

/// `definition.changeRequestActions` of the client definition, in its insertion order.
const CHANGE_REQUEST_ACTIONS: [PullRequestAction; 10] = [
    PullRequestAction::Merge,
    PullRequestAction::Ready,
    PullRequestAction::Draft,
    PullRequestAction::Close,
    PullRequestAction::Reopen,
    PullRequestAction::UpdateBranch,
    PullRequestAction::EnableAutoMerge,
    PullRequestAction::DisableAutoMerge,
    PullRequestAction::Revert,
    PullRequestAction::ApproveWorkflows,
];

const REVIEW_VERDICTS: [PullRequestReviewVerdict; 3] = [
    PullRequestReviewVerdict::Comment,
    PullRequestReviewVerdict::Approve,
    PullRequestReviewVerdict::RequestChanges,
];

const UPDATE_METHODS: [PullRequestUpdateMethod; 2] = [
    PullRequestUpdateMethod::Merge,
    PullRequestUpdateMethod::Rebase,
];

static CAPABILITIES: LazyLock<PullRequestCapabilities> =
    LazyLock::new(|| PullRequestCapabilities {
        diff: true,
        comment: true,
        actions: CHANGE_REQUEST_ACTIONS.to_vec(),
        merge_methods: vec![
            PullRequestMergeMethod::Merge,
            PullRequestMergeMethod::Squash,
            PullRequestMergeMethod::Rebase,
        ],
        update_methods: Some(UPDATE_METHODS.to_vec()),
        search: true,
        reactions: Some(true),
        viewed_files: Some(PullRequestViewedFilesStore::Host),
        review: PullRequestReviewCapabilities {
            inline_comment: true,
            reply: true,
            resolve: true,
            verdicts: REVIEW_VERDICTS.to_vec(),
        },
        reviewers: PullRequestReviewerCapabilities {
            request: true,
            list_candidates: true,
        },
        edit: Some(PullRequestEditCapabilities {
            change_request: true,
            comment: true,
        }),
        stacks: Some(true),
        stack_actions: Some(true),
        labels: Some(true),
    });

/// What the signed-in account may do here, from the three things GitHub says about it.
///
/// Merging needs a role that can push, which is the one thing a stranger on an open-source
/// repository never has. The other four actions go by `viewerCanUpdate`, because the author of a
/// pull request may close it, reopen it and move it in and out of draft with no more than read
/// access on the repository it was opened against.
///
/// Commenting and reviewing are not gated at all: read access is enough to say something and
/// enough to approve or ask for changes, which is what open-source review consists of. Resolving a
/// conversation is the exception — GitHub allows it to whoever can write, and to the author of the
/// pull request the conversation is on.
///
/// Asking somebody else for a review needs write access, which is the one thing here an author
/// cannot do on their own pull request: GitHub shows an outside contributor the reviewer control
/// and refuses the request behind it.
pub fn github_viewer_permissions(access: &GitHubViewerAccess) -> PullRequestViewerPermissions {
    let mut actions = Vec::new();
    // Arming a merge and taking the arming back are the merge, deferred: whoever may not
    // merge here may not leave an instruction to merge later either.
    if access.can_write {
        actions.extend([
            PullRequestAction::Merge,
            PullRequestAction::EnableAutoMerge,
            PullRequestAction::DisableAutoMerge,
            PullRequestAction::Revert,
            PullRequestAction::ApproveWorkflows,
        ]);
    }
    if access.can_update {
        actions.extend([
            PullRequestAction::Ready,
            PullRequestAction::Draft,
            PullRequestAction::Close,
            PullRequestAction::Reopen,
        ]);
    }
    // Whether this viewer may update the branch is GitHub's own answer, read with the
    // comparison; without it the action is offered to nobody rather than to everybody.
    let can_update_branch = access.can_update_branch == Some(true);
    if can_update_branch {
        actions.push(PullRequestAction::UpdateBranch);
    }
    PullRequestViewerPermissions {
        stack_rebase: access.can_write.then_some(true),
        actions,
        comment: true,
        resolve: access.can_write || access.did_author,
        // Anyone may review a pull request they can see, except their own: GitHub refuses an
        // author's approval and their request for changes ("Can not approve your own pull
        // request"), and leaves them commenting, which is what an author has to say about their
        // own change anyway.
        verdicts: if access.did_author {
            vec![PullRequestReviewVerdict::Comment]
        } else {
            REVIEW_VERDICTS.to_vec()
        },
        request_reviewers: access.can_write,
        update_methods: can_update_branch.then(|| UPDATE_METHODS.to_vec()),
        // Triage is the one role that labels without writing, which is what triage is for.
        labels: Some(access.can_triage),
        edit_change_request: None,
    }
}

/// The tags that mean GitHub is out of reach for this account, rather than one request failing.
pub fn github_provider_failure(error: &GitHubPullRequestApiError) -> PullRequestProviderFailure {
    use super::credentials::GitHubCredentialUnavailableError as Unavailable;
    let reason = |reason| PullRequestProviderFailure {
        reason,
        retry_at: None,
    };
    let Some(api_error) = error.api() else {
        return reason(PullRequestProviderErrorReason::Failed);
    };
    match api_error {
        GitHubApiError::CredentialUnavailable(Unavailable::CliMissing(_)) => {
            reason(PullRequestProviderErrorReason::MissingTool)
        }
        GitHubApiError::CredentialUnavailable(
            Unavailable::NotSignedIn(_) | Unavailable::HostDisabled(_),
        )
        | GitHubApiError::Authentication(_) => {
            reason(PullRequestProviderErrorReason::Unauthenticated)
        }
        GitHubApiError::RateLimit(limit) => PullRequestProviderFailure {
            reason: PullRequestProviderErrorReason::RateLimited,
            retry_at: limit.retry_at,
        },
        GitHubApiError::Paused(paused) => PullRequestProviderFailure {
            reason: PullRequestProviderErrorReason::RateLimited,
            retry_at: Some(paused.retry_at),
        },
        GitHubApiError::NotFound(_) => reason(PullRequestProviderErrorReason::NotFound),
        _ => reason(PullRequestProviderErrorReason::Failed),
    }
}

/// Null for anything that is not a plain user login: an app posts as `dependabot[bot]`, which
/// names no page, and a guessed URL that 404s is worse than the initials it would replace.
pub fn login_avatar_url(login: &str, host: &str) -> Option<String> {
    static LOGIN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^[a-z0-9][a-z0-9-]{0,38}$").expect("valid regex"));
    LOGIN
        .is_match(login)
        .then(|| format!("https://{host}/{login}.png?size=80"))
}

/// `gh pr view --json` reports no avatar for anyone, so the ones the GraphQL read collected are
/// applied here by login. An actor already carrying one keeps it.
///
/// A login GitHub did not answer for falls back to the picture every GitHub install serves at
/// `/<login>.png`. The lookup is one more request per repository and can be refused — a rate
/// limit, a slow host — and a face that comes and goes between two loads of the same page reads
/// as a bug in the page rather than as a request that failed quietly.
fn with_avatar(
    actor: Option<PullRequestActor>,
    avatars_by_login: &IndexMap<String, String>,
    host: &str,
    bot_logins: Option<&IndexSet<String>>,
) -> Option<PullRequestActor> {
    let mut actor = actor?;
    if bot_logins.is_some_and(|bots| bots.contains(actor.login.as_str())) {
        actor.is_bot = Some(true);
    }
    if actor.avatar_url.is_some() {
        return Some(actor);
    }
    let avatar_url = avatars_by_login
        .get(actor.login.as_str())
        .cloned()
        .or_else(|| login_avatar_url(actor.login.as_str(), host));
    if avatar_url.is_some() {
        actor.avatar_url = avatar_url;
    }
    Some(actor)
}

fn no_avatars() -> &'static IndexMap<String, String> {
    static EMPTY: LazyLock<IndexMap<String, String>> = LazyLock::new(IndexMap::new);
    &EMPTY
}

/// `Number(id)` of a run id in a check's URL, compared the way JavaScript numbers compare.
fn represented_run_ids(checks: &[PullRequestCheck]) -> Vec<f64> {
    static RUN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"/actions/runs/(\d+)(?:/|$)").expect("valid regex"));
    checks
        .iter()
        .filter(|check| check.status == PullRequestCheckStatus::ActionRequired)
        .filter_map(|check| {
            let url = check.url.as_deref()?;
            RUN.captures(url)?.get(1)?.as_str().parse::<f64>().ok()
        })
        .collect()
}

fn with_workflow_approvals(
    checks: &[PullRequestCheck],
    runs: &[GitHubWorkflowRunApproval],
    unavailable: bool,
) -> Vec<PullRequestCheck> {
    let represented = represented_run_ids(checks);
    let mut all = checks.to_vec();
    all.extend(
        runs.iter()
            .filter(|run| !represented.contains(&(run.id as f64)))
            .map(|run| PullRequestCheck {
                // The decoder never leaves a run unnamed; its own fallback is repeated for the
                // case a hand-built run does.
                name: TrimmedNonEmptyString::new(&run.name).unwrap_or_else(|_| {
                    TrimmedNonEmptyString::new(format!("Workflow run {}", run.id))
                        .expect("a run id is not blank")
                }),
                status: PullRequestCheckStatus::ActionRequired,
                description: Some(
                    "A maintainer must approve this workflow before it can run.".into(),
                ),
                url: run.url.clone(),
                required: None,
            }),
    );
    if unavailable {
        all.push(PullRequestCheck {
            name: TrimmedNonEmptyString::new("Workflow approval status").expect("not blank"),
            status: PullRequestCheckStatus::ActionRequired,
            description: Some(
                "GitHub could not determine whether workflows are awaiting approval.".into(),
            ),
            url: None,
            required: None,
        });
    }
    all
}

/// True where markdown would render nothing: whitespace, or only HTML comments.
fn renders_empty(body: &str) -> bool {
    static COMMENT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"<!--[\s\S]*?-->").expect("valid regex"));
    crate::shared::js::js_trim(&COMMENT.replace_all(body, "")).is_empty()
}

/// A rate limit or a pause is never worked around: it says the host is out of reach for now.
fn is_rate_limit(error: &GitHubPullRequestApiError) -> bool {
    matches!(
        error.api(),
        Some(GitHubApiError::RateLimit(_) | GitHubApiError::Paused(_))
    )
}

fn fail(operation: &'static str) -> impl Fn(GitHubPullRequestApiError) -> PullRequestProviderError {
    move |error| {
        let failure = github_provider_failure(&error);
        PullRequestProviderError::new(kind(), operation, failure.reason, error.to_string())
            .with_retry_at(failure.retry_at)
            .with_cause(Arc::new(error) as Cause)
    }
}

// -------------------------------------------------------------------------------------------
// The service the provider reads through
// -------------------------------------------------------------------------------------------

/// The part of [`GitHubPullRequestApi`] the provider uses. A trait only so the tests can script
/// it, like upstream's `Layer.mock(GitHubPullRequestApi)`; there is one implementation.
#[async_trait]
pub trait GitHubPullRequestService: Send + Sync {
    /// `revalidateChecks(input, read)`.
    async fn revalidate<'a>(
        &'a self,
        input: &'a ProviderChangeRequestRef,
        read: BoxFuture<'a, Result<ChecksRead, GitHubPullRequestApiError>>,
    ) -> Result<PullRequestChecks, GitHubPullRequestApiError>;
    async fn verify_credential(
        &self,
        input: &VerifiedCredentialInput,
    ) -> Result<(VerifiedIdentity, PinnedCredential), GitHubPullRequestApiError>;
    async fn get_routing_identity(
        &self,
        input: &VerifiedCredentialInput,
    ) -> Result<RoutingIdentity, GitHubPullRequestApiError>;
    async fn get_viewer_login(
        &self,
        input: &VerifiedCredentialInput,
    ) -> Result<String, GitHubPullRequestApiError>;
    async fn list_pull_requests(
        &self,
        input: &ListPullRequestsInput,
    ) -> Result<GitHubPullRequestListBatch, GitHubPullRequestApiError>;
    async fn search_pull_requests(
        &self,
        input: &SearchPullRequestsInput,
    ) -> Result<GitHubPullRequestSearchBatch, GitHubPullRequestApiError>;
    async fn list_pull_request_stats(
        &self,
        input: &ListPullRequestStatsInput,
    ) -> Result<Vec<GitHubPullRequestStat>, GitHubPullRequestApiError>;
    async fn list_actor_avatars(
        &self,
        input: &ListActorAvatarsInput,
    ) -> Result<IndexMap<String, String>, GitHubPullRequestApiError>;
    async fn get_pull_request_summary(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<ProviderChangeRequestSummary, GitHubPullRequestApiError>;
    async fn get_pull_request_watch_fingerprint(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<Option<GitHubPullRequestWatchFingerprint>, GitHubPullRequestApiError>;
    async fn get_pull_request_detail(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<GitHubPullRequestCore, GitHubPullRequestApiError>;
    async fn get_pull_request_preview(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<ProviderChangeRequestPreview, GitHubPullRequestApiError>;
    async fn list_workflow_runs_requiring_approval(
        &self,
        input: &ListWorkflowRunsRequiringApprovalInput,
    ) -> Result<Vec<GitHubWorkflowRunApproval>, GitHubPullRequestApiError>;
    async fn get_pull_request_stack(
        &self,
        input: &GetPullRequestStackInput,
    ) -> Result<Option<GitHubPullRequestStack>, GitHubPullRequestApiError>;
    async fn get_pull_request_activity(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<GitHubPullRequestActivity, GitHubPullRequestApiError>;
    async fn list_review_thread_comments(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<GitHubReviewThreadComments, GitHubPullRequestApiError>;
    async fn get_review_thread_comments(
        &self,
        input: &api::GetReviewThreadCommentsInput,
    ) -> Result<PullRequestThreadCommentsResult, GitHubPullRequestApiError>;
    async fn get_viewer_access(
        &self,
        input: &GetViewerAccessInput,
    ) -> Result<GitHubViewerRepositoryAccess, GitHubPullRequestApiError>;
    async fn get_pull_request_diff(
        &self,
        input: &GetPullRequestDiffInput,
    ) -> Result<GitHubPullRequestDiffSlice, GitHubPullRequestApiError>;
    async fn get_pull_request_diff_file_contents(
        &self,
        input: &GetPullRequestDiffFileContentsInput,
    ) -> Result<GitHubPullRequestDiffFileContents, GitHubPullRequestApiError>;
    async fn get_pull_request_files_viewed(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<GitHubPullRequestFilesViewed, GitHubPullRequestApiError>;
    async fn set_pull_request_files_viewed(
        &self,
        input: &SetPullRequestFilesViewedInput,
    ) -> Result<(), GitHubPullRequestApiError>;
    async fn list_reviewer_candidates(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<PullRequestReviewerCandidateList, GitHubPullRequestApiError>;
    async fn list_label_candidates(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<PullRequestLabelCandidateList, GitHubPullRequestApiError>;
    async fn run_pull_request_action(
        &self,
        input: &api::RunPullRequestActionInput,
    ) -> Result<(), GitHubPullRequestApiError>;
    async fn comment_on_pull_request(
        &self,
        input: &api::CommentOnPullRequestInput,
    ) -> Result<(), GitHubPullRequestApiError>;
    async fn submit_review(
        &self,
        input: &api::SubmitReviewInput,
    ) -> Result<(), GitHubPullRequestApiError>;
    async fn reply_to_review_thread(
        &self,
        input: &api::ReplyToReviewThreadInput,
    ) -> Result<(), GitHubPullRequestApiError>;
    async fn set_review_thread_resolution(
        &self,
        input: &api::SetReviewThreadResolutionInput,
    ) -> Result<(), GitHubPullRequestApiError>;
    async fn set_reviewer_request(
        &self,
        input: &api::SetReviewerRequestInput,
    ) -> Result<(), GitHubPullRequestApiError>;
    async fn set_labels(
        &self,
        input: &api::SetLabelsInput,
    ) -> Result<(), GitHubPullRequestApiError>;
    async fn set_reaction(
        &self,
        input: &api::SetReactionInput,
    ) -> Result<(), GitHubPullRequestApiError>;
    async fn update_pull_request(
        &self,
        input: &api::UpdatePullRequestInput,
    ) -> Result<(), GitHubPullRequestApiError>;
    async fn update_comment(
        &self,
        input: &api::UpdateCommentInput,
    ) -> Result<(), GitHubPullRequestApiError>;
}

#[async_trait]
impl GitHubPullRequestService for GitHubPullRequestApi {
    async fn revalidate<'a>(
        &'a self,
        input: &'a ProviderChangeRequestRef,
        read: BoxFuture<'a, Result<ChecksRead, GitHubPullRequestApiError>>,
    ) -> Result<PullRequestChecks, GitHubPullRequestApiError> {
        self.revalidate_checks().revalidate(input, || read).await
    }
    async fn verify_credential(
        &self,
        input: &VerifiedCredentialInput,
    ) -> Result<(VerifiedIdentity, PinnedCredential), GitHubPullRequestApiError> {
        GitHubPullRequestApi::verify_credential(self, input).await
    }
    async fn get_routing_identity(
        &self,
        input: &VerifiedCredentialInput,
    ) -> Result<RoutingIdentity, GitHubPullRequestApiError> {
        GitHubPullRequestApi::get_routing_identity(self, input).await
    }
    async fn get_viewer_login(
        &self,
        input: &VerifiedCredentialInput,
    ) -> Result<String, GitHubPullRequestApiError> {
        GitHubPullRequestApi::get_viewer_login(self, input).await
    }
    async fn list_pull_requests(
        &self,
        input: &ListPullRequestsInput,
    ) -> Result<GitHubPullRequestListBatch, GitHubPullRequestApiError> {
        GitHubPullRequestApi::list_pull_requests(self, input).await
    }
    async fn search_pull_requests(
        &self,
        input: &SearchPullRequestsInput,
    ) -> Result<GitHubPullRequestSearchBatch, GitHubPullRequestApiError> {
        GitHubPullRequestApi::search_pull_requests(self, input).await
    }
    async fn list_pull_request_stats(
        &self,
        input: &ListPullRequestStatsInput,
    ) -> Result<Vec<GitHubPullRequestStat>, GitHubPullRequestApiError> {
        GitHubPullRequestApi::list_pull_request_stats(self, input).await
    }
    async fn list_actor_avatars(
        &self,
        input: &ListActorAvatarsInput,
    ) -> Result<IndexMap<String, String>, GitHubPullRequestApiError> {
        GitHubPullRequestApi::list_actor_avatars(self, input).await
    }
    async fn get_pull_request_summary(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<ProviderChangeRequestSummary, GitHubPullRequestApiError> {
        GitHubPullRequestApi::get_pull_request_summary(self, target).await
    }
    async fn get_pull_request_watch_fingerprint(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<Option<GitHubPullRequestWatchFingerprint>, GitHubPullRequestApiError> {
        GitHubPullRequestApi::get_pull_request_watch_fingerprint(self, target).await
    }
    async fn get_pull_request_detail(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<GitHubPullRequestCore, GitHubPullRequestApiError> {
        GitHubPullRequestApi::get_pull_request_detail(self, target).await
    }
    async fn get_pull_request_preview(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<ProviderChangeRequestPreview, GitHubPullRequestApiError> {
        GitHubPullRequestApi::get_pull_request_preview(self, target).await
    }
    async fn list_workflow_runs_requiring_approval(
        &self,
        input: &ListWorkflowRunsRequiringApprovalInput,
    ) -> Result<Vec<GitHubWorkflowRunApproval>, GitHubPullRequestApiError> {
        GitHubPullRequestApi::list_workflow_runs_requiring_approval(self, input).await
    }
    async fn get_pull_request_stack(
        &self,
        input: &GetPullRequestStackInput,
    ) -> Result<Option<GitHubPullRequestStack>, GitHubPullRequestApiError> {
        GitHubPullRequestApi::get_pull_request_stack(self, input).await
    }
    async fn get_pull_request_activity(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<GitHubPullRequestActivity, GitHubPullRequestApiError> {
        GitHubPullRequestApi::get_pull_request_activity(self, target).await
    }
    async fn list_review_thread_comments(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<GitHubReviewThreadComments, GitHubPullRequestApiError> {
        GitHubPullRequestApi::list_review_thread_comments(self, target).await
    }
    async fn get_review_thread_comments(
        &self,
        input: &api::GetReviewThreadCommentsInput,
    ) -> Result<PullRequestThreadCommentsResult, GitHubPullRequestApiError> {
        GitHubPullRequestApi::get_review_thread_comments(self, input).await
    }
    async fn get_viewer_access(
        &self,
        input: &GetViewerAccessInput,
    ) -> Result<GitHubViewerRepositoryAccess, GitHubPullRequestApiError> {
        GitHubPullRequestApi::get_viewer_access(self, input).await
    }
    async fn get_pull_request_diff(
        &self,
        input: &GetPullRequestDiffInput,
    ) -> Result<GitHubPullRequestDiffSlice, GitHubPullRequestApiError> {
        GitHubPullRequestApi::get_pull_request_diff(self, input).await
    }
    async fn get_pull_request_diff_file_contents(
        &self,
        input: &GetPullRequestDiffFileContentsInput,
    ) -> Result<GitHubPullRequestDiffFileContents, GitHubPullRequestApiError> {
        GitHubPullRequestApi::get_pull_request_diff_file_contents(self, input).await
    }
    async fn get_pull_request_files_viewed(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<GitHubPullRequestFilesViewed, GitHubPullRequestApiError> {
        GitHubPullRequestApi::get_pull_request_files_viewed(self, target).await
    }
    async fn set_pull_request_files_viewed(
        &self,
        input: &SetPullRequestFilesViewedInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        GitHubPullRequestApi::set_pull_request_files_viewed(self, input).await
    }
    async fn list_reviewer_candidates(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<PullRequestReviewerCandidateList, GitHubPullRequestApiError> {
        GitHubPullRequestApi::list_reviewer_candidates(self, target).await
    }
    async fn list_label_candidates(
        &self,
        target: &GitHubPullRequestTarget,
    ) -> Result<PullRequestLabelCandidateList, GitHubPullRequestApiError> {
        GitHubPullRequestApi::list_label_candidates(self, target).await
    }
    async fn run_pull_request_action(
        &self,
        input: &api::RunPullRequestActionInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        GitHubPullRequestApi::run_pull_request_action(self, input).await
    }
    async fn comment_on_pull_request(
        &self,
        input: &api::CommentOnPullRequestInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        GitHubPullRequestApi::comment_on_pull_request(self, input).await
    }
    async fn submit_review(
        &self,
        input: &api::SubmitReviewInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        GitHubPullRequestApi::submit_review(self, input).await
    }
    async fn reply_to_review_thread(
        &self,
        input: &api::ReplyToReviewThreadInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        GitHubPullRequestApi::reply_to_review_thread(self, input).await
    }
    async fn set_review_thread_resolution(
        &self,
        input: &api::SetReviewThreadResolutionInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        GitHubPullRequestApi::set_review_thread_resolution(self, input).await
    }
    async fn set_reviewer_request(
        &self,
        input: &api::SetReviewerRequestInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        GitHubPullRequestApi::set_reviewer_request(self, input).await
    }
    async fn set_labels(
        &self,
        input: &api::SetLabelsInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        GitHubPullRequestApi::set_labels(self, input).await
    }
    async fn set_reaction(
        &self,
        input: &api::SetReactionInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        GitHubPullRequestApi::set_reaction(self, input).await
    }
    async fn update_pull_request(
        &self,
        input: &api::UpdatePullRequestInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        GitHubPullRequestApi::update_pull_request(self, input).await
    }
    async fn update_comment(
        &self,
        input: &api::UpdateCommentInput,
    ) -> Result<(), GitHubPullRequestApiError> {
        GitHubPullRequestApi::update_comment(self, input).await
    }
}

// -------------------------------------------------------------------------------------------
// Conversions
// -------------------------------------------------------------------------------------------

/// A listing row as the neutral type, with the author replaced by the one that has a face.
fn provider_change_request(
    item: GitHubPullRequestListItem,
    author: Option<PullRequestActor>,
) -> ProviderChangeRequest {
    ProviderChangeRequest {
        stack: item.stack,
        number: item.number,
        title: item.title,
        url: item.url,
        author,
        head_branch: item.head_branch,
        head_repository_name_with_owner: None,
        base_branch: item.base_branch,
        state: item.state,
        is_draft: item.is_draft,
        mergeability: item.mergeability,
        additions: item.additions,
        deletions: item.deletions,
        created_at: item.created_at,
        closed_at: None,
        merged_at: None,
        updated_at: item.updated_at,
        review_request_logins: item.review_request_logins,
        labels: item.labels,
        review_decision: Some(item.review_decision),
        checks_state: Some(item.checks_state),
    }
}

fn provider_stack(stack: GitHubPullRequestStack) -> ProviderChangeRequestStack {
    ProviderChangeRequestStack {
        id: stack.id,
        number: stack.number,
        url: stack.url,
        base: stack.base,
        layers: stack
            .layers
            .into_iter()
            .map(|layer| ProviderChangeRequestStackLayer {
                title: layer.title,
                is_draft: layer.is_draft,
                head_sha: layer.head_sha,
                number: layer.number,
                head_branch: layer.head_branch,
                state: layer.state,
            })
            .collect(),
    }
}

/// What the checks read hands the conditional-request cache.
fn checks_read(pull_request: &CheckedPullRequest) -> ChecksRead {
    ChecksRead {
        state: pull_request.core.state,
        checks: pull_request.checks.clone(),
        head_sha: pull_request.core.head_sha.clone(),
        workflow_approvals_required: pull_request.workflow_approvals_required,
    }
}

/// The detail read with the fork workflow approvals folded into its checks
/// (`readChecks`' result: `{ ...pullRequest, author, checks, workflowApprovalsRequired? }`).
struct CheckedPullRequest {
    core: GitHubPullRequestCore,
    author: Option<PullRequestActor>,
    checks: Vec<PullRequestCheck>,
    workflow_approvals_required: Option<u64>,
}

/// Which fork workflows wait on a maintainer, and whether that could be told at all.
struct WorkflowApprovals {
    runs: Vec<GitHubWorkflowRunApproval>,
    unavailable: bool,
}

/// `editedAtById.get(id) ?? comment.editedAt ?? null`.
fn comment_edited_at(
    comment: &PullRequestComment,
    edited_at_by_id: &IndexMap<String, String>,
) -> Option<String> {
    edited_at_by_id
        .get(comment.id.as_str())
        .cloned()
        .or_else(|| comment.edited_at.clone().flatten())
}

// -------------------------------------------------------------------------------------------
// The provider
// -------------------------------------------------------------------------------------------

/// `GitHubPullRequestProvider.make`.
pub fn make(cli: Arc<dyn GitHubPullRequestService>) -> GitHubPullRequestProvider {
    GitHubPullRequestProvider { cli }
}

pub struct GitHubPullRequestProvider {
    cli: Arc<dyn GitHubPullRequestService>,
}

impl GitHubPullRequestProvider {
    async fn read_checks(
        &self,
        input: &ProviderChangeRequestRef,
    ) -> Result<CheckedPullRequest, GitHubPullRequestApiError> {
        let target = GitHubPullRequestTarget::from(input);
        let pull_request = self.cli.get_pull_request_detail(&target).await?;
        let approvals = if pull_request.state != PullRequestState::Open
            || pull_request.is_cross_repository != Some(true)
        {
            WorkflowApprovals {
                runs: Vec::new(),
                unavailable: false,
            }
        } else if let (Some(head_sha), Some(head_repository_owner)) = (
            pull_request.head_sha.clone(),
            pull_request.head_repository_owner.clone(),
        ) {
            match self
                .cli
                .list_workflow_runs_requiring_approval(&ListWorkflowRunsRequiringApprovalInput {
                    target,
                    head_sha,
                    head_branch: pull_request.head_branch.clone(),
                    head_repository_owner,
                })
                .await
            {
                Ok(runs) => WorkflowApprovals {
                    runs,
                    unavailable: false,
                },
                Err(error) if is_rate_limit(&error) => return Err(error),
                Err(_) => WorkflowApprovals {
                    runs: Vec::new(),
                    unavailable: true,
                },
            }
        } else {
            WorkflowApprovals {
                runs: Vec::new(),
                unavailable: true,
            }
        };
        let author = with_avatar(
            pull_request.author.clone(),
            no_avatars(),
            &input.repo.host,
            None,
        );
        let checks =
            with_workflow_approvals(&pull_request.checks, &approvals.runs, approvals.unavailable);
        Ok(CheckedPullRequest {
            core: pull_request,
            author,
            checks,
            workflow_approvals_required: (!approvals.unavailable)
                .then_some(approvals.runs.len() as u64),
        })
    }

    async fn viewer_permissions(
        &self,
        input: GetViewerPermissionsInput,
    ) -> Result<PullRequestViewerPermissions, GitHubPullRequestApiError> {
        let target = GitHubPullRequestTarget::from(&input.request);
        let light_access = || async {
            self.cli
                .get_viewer_access(&GetViewerAccessInput {
                    target: target.clone(),
                    allow_reserve: true,
                })
                .await
                .map(|access| github_viewer_permissions(&access))
        };
        if !input.include_update_branch {
            return light_access().await;
        }
        // The core detail already carries the viewer's access, the merge settings, and the base
        // comparison, so one read usually answers what used to take three. When that heavier read
        // fails, the light access read still answers, withholding only update-branch.
        match ambient::with_interactive(true, self.cli.get_pull_request_detail(&target)).await {
            Ok(pull_request) => Ok(github_viewer_permissions(&GitHubViewerAccess {
                can_update_branch: Some(
                    pull_request.state == PullRequestState::Open
                        && pull_request
                            .comparison
                            .is_some_and(|comparison| comparison.viewer_can_update),
                ),
                ..*pull_request.viewer_access
            })),
            Err(error) if !is_rate_limit(&error) => light_access().await,
            Err(error) => Err(error),
        }
    }
}

#[async_trait]
impl PullRequestProviderApi for GitHubPullRequestProvider {
    fn kind(&self) -> SourceControlProviderKind {
        kind()
    }

    fn capabilities(&self) -> &PullRequestCapabilities {
        &CAPABILITIES
    }

    fn supports(&self, operation: PullRequestOperation) -> bool {
        use PullRequestOperation::*;
        matches!(
            operation,
            MergeMessageRewrite
                | RoutingIdentity
                | VerifyCredential
                | ListChangeRequestsAcross
                | ListChangeRequestStats
                | GetChangeRequestChecks
                | GetChangeRequestPreview
                | GetChangeRequestSummary
                | GetChangeRequestWatchFingerprint
                | GetChangeRequestStack
                | GetReviewThreadComments
                | GetDiffFileContents
                | GetFilesViewed
                | SetFilesViewed
                | UpdateChangeRequest
                | UpdateComment
                | ListLabelCandidates
                | SetLabels
        )
    }

    fn merge_message_rewrite(&self, message: &str) -> Option<String> {
        Some(remove_agent_credits(message))
    }

    async fn get_routing_identity(
        &self,
        input: VerifiedCredentialInput,
    ) -> ProviderResult<RoutingIdentity> {
        self.cli
            .get_routing_identity(&input)
            .await
            .map_err(fail("routeIdentity"))
    }

    async fn verify_credential(
        &self,
        input: VerifiedCredentialInput,
    ) -> ProviderResult<(VerifiedIdentity, PinnedCredential)> {
        self.cli
            .verify_credential(&input)
            .await
            .map_err(fail("routeIdentity"))
    }

    async fn get_viewer(&self, cwd: &str, host: Option<&str>) -> ProviderResult<String> {
        self.cli
            .get_viewer_login(&VerifiedCredentialInput {
                cwd: cwd.to_owned(),
                host: host.unwrap_or("github.com").to_owned(),
            })
            .await
            .map_err(fail("getViewer"))
    }

    async fn list_change_requests(
        &self,
        input: ListChangeRequestsInput,
    ) -> ProviderResult<ProviderChangeRequestPage> {
        let repo = input.repo;
        let page = self
            .cli
            .list_pull_requests(&ListPullRequestsInput {
                cwd: repo.cwd.clone(),
                repository: repo.repository.clone(),
                host: repo.host.clone(),
                state: input.state,
                involvement: input.involvement,
                viewer: input.viewer,
                limit: input.limit as usize,
                query: input.query,
                cursor: input.cursor,
                filters: input.filters,
            })
            .await
            .map_err(fail("listChangeRequests"))?;
        let ids: IndexSet<String> = page
            .items
            .iter()
            .filter_map(|item| item.author_id.clone())
            .collect();
        // A listing without faces is still a listing, so a failed lookup falls back to
        // the initials rather than taking the rows down with it.
        let avatars_by_login = self
            .cli
            .list_actor_avatars(&ListActorAvatarsInput {
                cwd: repo.cwd.clone(),
                repository: repo.repository.clone(),
                host: repo.host.clone(),
                ids: ids.into_iter().collect(),
            })
            .await
            .unwrap_or_default();
        Ok(ProviderChangeRequestPage {
            items: page
                .items
                .into_iter()
                .map(|item| {
                    let author =
                        with_avatar(item.author.clone(), &avatars_by_login, &repo.host, None);
                    provider_change_request(item, author)
                })
                .collect(),
            truncated: page.truncated,
            cursor_advance: None,
            continues: page.continues,
        })
    }

    /// The same listing for a whole host in one search. The avatar lookup the per-repository read
    /// needs is not here: a search reports an author's picture itself, so a face costs no request
    /// of its own — `with_avatar` still stands behind it for the login GitHub answered nothing for.
    async fn list_change_requests_across(
        &self,
        input: ListChangeRequestsAcrossInput,
    ) -> ProviderResult<ProviderBatchedChangeRequestPage> {
        let host = input.host.clone();
        let batch = self
            .cli
            .search_pull_requests(&SearchPullRequestsInput {
                cwd: input.cwd,
                host: input.host,
                repositories: input.repositories,
                state: input.state,
                involvement: input.involvement,
                viewer: input.viewer,
                limit: input.limit as usize,
                query: input.query,
                cursor: input.cursor,
                filters: input.filters,
            })
            .await
            .map_err(fail("listChangeRequestsAcross"))?;
        Ok(ProviderBatchedChangeRequestPage {
            truncated: batch.truncated,
            items: batch
                .items
                .into_iter()
                .map(|GitHubPullRequestSearchItem { base, repository }| {
                    let author = with_avatar(base.author.clone(), no_avatars(), &host, None);
                    ProviderBatchedChangeRequest {
                        base: provider_change_request(base, author),
                        repository,
                    }
                })
                .collect(),
        })
    }

    async fn list_change_request_stats(
        &self,
        input: ListChangeRequestStatsInput,
    ) -> ProviderResult<Vec<ProviderChangeRequestStat>> {
        let stats = self
            .cli
            .list_pull_request_stats(&ListPullRequestStatsInput {
                cwd: input.cwd,
                host: input.host,
                change_requests: input
                    .change_requests
                    .into_iter()
                    .map(ChangeRequestSelector::from)
                    .collect(),
            })
            .await
            .map_err(fail("listChangeRequestStats"))?;
        Ok(stats
            .into_iter()
            .map(|stat| ProviderChangeRequestStat {
                repository: stat.repository,
                number: stat.number,
                additions: stat.additions,
                deletions: stat.deletions,
            })
            .collect())
    }

    async fn get_change_request_summary(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<ProviderChangeRequestSummary> {
        let mut summary = self
            .cli
            .get_pull_request_summary(&GitHubPullRequestTarget::from(&input))
            .await
            .map_err(fail("getChangeRequestSummary"))?;
        // `gh pr view` names the author without an avatar; the login-shaped URL every user
        // has stands in, without the second request the listing spends on it.
        if let Some(author) = summary.author.take() {
            summary.author = Some(with_avatar(author, no_avatars(), &input.repo.host, None));
        }
        Ok(summary)
    }

    async fn get_change_request_stack(
        &self,
        input: GetChangeRequestStackInput,
    ) -> ProviderResult<Option<ProviderChangeRequestStack>> {
        self.cli
            .get_pull_request_stack(&GetPullRequestStackInput {
                target: GitHubPullRequestTarget::from(&input.request),
                include_details: input.include_details,
            })
            .await
            .map(|stack| stack.map(provider_stack))
            .map_err(fail("getChangeRequestStack"))
    }

    async fn get_change_request_watch_fingerprint(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<Option<ProviderChangeRequestWatchFingerprint>> {
        self.cli
            .get_pull_request_watch_fingerprint(&GitHubPullRequestTarget::from(&input))
            .await
            .map(|fingerprint| {
                fingerprint.map(|fingerprint| ProviderChangeRequestWatchFingerprint {
                    status: fingerprint.status,
                    remarks: fingerprint.remarks,
                })
            })
            .map_err(fail("getChangeRequestWatchFingerprint"))
    }

    async fn get_change_request_preview(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<ProviderChangeRequestPreview> {
        self.cli
            .get_pull_request_preview(&GitHubPullRequestTarget::from(&input))
            .await
            .map_err(fail("getChangeRequestPreview"))
    }

    async fn get_change_request_checks(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<PullRequestChecks> {
        let read = Box::pin(async {
            self.read_checks(&input)
                .await
                .map(|pull_request| checks_read(&pull_request))
        });
        self.cli
            .revalidate(&input, read)
            .await
            .map_err(fail("getChangeRequestChecks"))
    }

    async fn get_change_request(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<ProviderChangeRequestDetail> {
        let checked = self
            .read_checks(&input)
            .await
            .map_err(fail("getChangeRequest"))?;
        let CheckedPullRequest {
            core,
            author,
            checks,
            workflow_approvals_required,
        } = checked;
        let GitHubPullRequestCore {
            base: detail,
            review_request_teams,
            viewer_access,
            comparison,
            ..
        } = core;
        let GitHubPullRequestDetail {
            base: item,
            head_sha,
            body,
            changed_files,
            merged_at,
            closed_at,
            auto_merge_enabled,
            auto_merge_method,
            ..
        } = detail;
        let reviewers = item
            .review_request_logins
            .iter()
            .filter_map(|login| {
                Some(PullRequestActor {
                    is_bot: None,
                    login: TrimmedNonEmptyString::new(login).ok()?,
                    name: None,
                    avatar_url: None,
                })
            })
            .chain(review_request_teams)
            .collect();
        let mut base = provider_change_request(item, author);
        base.closed_at = Some(closed_at.clone());
        base.merged_at = Some(merged_at.clone());
        Ok(ProviderChangeRequestDetail {
            base,
            head_sha: head_sha.map(Some),
            body,
            changed_files,
            merged_at,
            closed_at,
            reviewers,
            checks,
            merge_capabilities: viewer_access.merge_capabilities,
            viewer_permissions: github_viewer_permissions(&GitHubViewerAccess {
                can_update_branch: Some(
                    comparison.is_some_and(|comparison| comparison.viewer_can_update),
                ),
                ..*viewer_access
            }),
            base_comparison: Some(match comparison.and_then(|c| c.behind_by) {
                None => PullRequestBaseComparison::Unknown,
                Some(behind_by) if behind_by > 0 => PullRequestBaseComparison::Behind,
                Some(_) => PullRequestBaseComparison::UpToDate,
            }),
            behind_by: comparison.and_then(|comparison| comparison.behind_by),
            auto_merge_enabled,
            auto_merge_method,
            workflow_approvals_required,
        })
    }

    async fn get_change_request_activity(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<ProviderChangeRequestActivity> {
        let target = GitHubPullRequestTarget::from(&input);
        let host = input.repo.host.as_str();
        let (pull_request, review_threads) = try_join(
            self.cli.get_pull_request_activity(&target),
            // Line comments live on review threads, which `gh pr view --json` cannot reach. A
            // GraphQL hiccup degrades to a truncated conversation rather than blanking activity.
            async {
                Ok(self
                    .cli
                    .list_review_thread_comments(&target)
                    .await
                    .unwrap_or_else(|_| degraded_review_threads()))
            },
        )
        .await
        .map_err(fail("getChangeRequestActivity"))?;
        let avatars = &review_threads.avatars_by_login;
        let bots = Some(&review_threads.bot_logins);

        let commits = if review_threads.commits.is_empty() {
            pull_request.commits.clone()
        } else {
            review_threads.commits.clone()
        }
        .into_iter()
        .map(|mut commit| {
            if let Some(stats) = review_threads.commit_stats.get(commit.oid.as_str()) {
                commit.additions = Some(stats.additions);
                commit.deletions = Some(stats.deletions);
            }
            commit.authors = commit.authors.map(|authors| {
                authors
                    .into_iter()
                    .map(|author| {
                        with_avatar(Some(author.clone()), avatars, host, bots).unwrap_or(author)
                    })
                    .collect()
            });
            commit
        })
        .collect();

        let mut comments: Vec<PullRequestComment> = pull_request
            .comments
            .iter()
            .chain(review_threads.comments.iter())
            .cloned()
            .map(|comment| PullRequestComment {
                // GitHub keeps the dismissal reason on the timeline event, not on the review,
                // so a dismissed review with nothing visible of its own reads its words from
                // there. "Visible" and not "empty": bot reviews often carry only an HTML
                // marker comment, which markdown renders as nothing.
                body: if comment.kind == PullRequestCommentKind::Review
                    && comment
                        .review_state
                        .as_deref()
                        .is_some_and(|state| state.to_uppercase() == "DISMISSED")
                    && renders_empty(&comment.body)
                {
                    review_threads
                        .dismissals_by_review_id
                        .get(comment.id.as_str())
                        .cloned()
                        .unwrap_or_else(|| comment.body.clone())
                } else {
                    comment.body.clone()
                },
                author: with_avatar(comment.author.clone(), avatars, host, bots),
                // A comment out of `gh pr view --json` carries none of its own: that read
                // reports no reaction at all, so they arrive from the GraphQL page by node id.
                reactions: Some(
                    comment
                        .reactions
                        .clone()
                        .or_else(|| {
                            review_threads
                                .reactions_by_id
                                .get(comment.id.as_str())
                                .cloned()
                        })
                        .unwrap_or_default(),
                ),
                edited_at: Some(comment_edited_at(&comment, &review_threads.edited_at_by_id)),
                ..comment
            })
            .collect();
        // `toSorted` is stable; ISO timestamps order the same bytewise as by locale.
        comments.sort_by(|left, right| left.created_at.cmp(&right.created_at));

        let review_thread_list: Vec<PullRequestReviewThread> = review_threads
            .review_threads
            .iter()
            .cloned()
            .map(|mut thread| {
                for comment in &mut thread.comments {
                    comment.author = with_avatar(comment.author.take(), avatars, host, bots);
                }
                thread
            })
            .collect();

        Ok(ProviderChangeRequestActivity {
            author: Some(with_avatar(
                pull_request.author.clone(),
                avatars,
                host,
                bots,
            )),
            reviewers: Some(review_threads.reviewers.clone()),
            reactions: Some(review_threads.reactions.clone()),
            commits,
            comments,
            // `gh pr view --json comments,reviews` follows GitHub's cursors itself, so those two
            // are always whole and only the thread walk can stop short of the host.
            comment_count: pull_request.comments.len() as u64 + review_threads.comment_count,
            comments_truncated: review_threads.truncated,
            review_threads_truncated: Some(review_threads.review_threads_truncated),
            review_threads: review_thread_list,
        })
    }

    async fn get_review_thread_comments(
        &self,
        input: GetReviewThreadCommentsInput,
    ) -> ProviderResult<PullRequestThreadCommentsResult> {
        self.cli
            .get_review_thread_comments(&api::GetReviewThreadCommentsInput {
                target: GitHubPullRequestTarget::from(&input.request),
                thread_id: input.thread_id,
                cursor: input.cursor,
            })
            .await
            .map_err(fail("getReviewThreadComments"))
    }

    async fn get_viewer_permissions(
        &self,
        input: GetViewerPermissionsInput,
    ) -> ProviderResult<PullRequestViewerPermissions> {
        self.viewer_permissions(input)
            .await
            .map_err(fail("getViewerPermissions"))
    }

    async fn get_diff(&self, input: GetDiffInput) -> ProviderResult<ProviderDiffSlice> {
        self.cli
            .get_pull_request_diff(&GetPullRequestDiffInput {
                target: GitHubPullRequestTarget::from(&input.request),
                cursor: input.cursor,
                commit: input.commit,
            })
            .await
            .map(|slice| ProviderDiffSlice {
                patch: slice.patch,
                truncated: slice.truncated,
                next_cursor: slice.next_cursor,
                omitted_file_stats: slice.omitted_file_stats,
            })
            .map_err(fail("getDiff"))
    }

    async fn get_diff_file_contents(
        &self,
        input: GetDiffFileContentsInput,
    ) -> ProviderResult<ProviderDiffFileContents> {
        self.cli
            .get_pull_request_diff_file_contents(&GetPullRequestDiffFileContentsInput {
                target: GitHubPullRequestTarget::from(&input.request),
                commit: input.commit,
                change_type: input.change_type,
                old_path: input.old_path,
                new_path: input.new_path,
            })
            .await
            .map(|contents| ProviderDiffFileContents {
                old_contents: contents.old_contents,
                new_contents: contents.new_contents,
            })
            .map_err(fail("getDiffFileContents"))
    }

    async fn get_files_viewed(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<ProviderFilesViewed> {
        self.cli
            .get_pull_request_files_viewed(&GitHubPullRequestTarget::from(&input))
            .await
            .map(|viewed| ProviderFilesViewed {
                files: viewed.files,
                truncated: viewed.truncated,
            })
            .map_err(fail("getFilesViewed"))
    }

    async fn set_files_viewed(&self, input: SetFilesViewedInput) -> ProviderResult<()> {
        self.cli
            .set_pull_request_files_viewed(&SetPullRequestFilesViewedInput {
                target: GitHubPullRequestTarget::from(&input.request),
                files: input
                    .files
                    .into_iter()
                    .map(|(path, viewed)| ViewedChange { path, viewed })
                    .collect(),
            })
            .await
            .map_err(fail("setFilesViewed"))
    }

    async fn list_reviewer_candidates(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<PullRequestReviewerCandidateList> {
        self.cli
            .list_reviewer_candidates(&GitHubPullRequestTarget::from(&input))
            .await
            .map_err(fail("listReviewerCandidates"))
    }

    async fn set_reviewer_request(&self, input: SetReviewerRequestInput) -> ProviderResult<()> {
        self.cli
            .set_reviewer_request(&api::SetReviewerRequestInput {
                target: GitHubPullRequestTarget::from(&input.request),
                reviewers: input.reviewers,
                requested: input.requested,
            })
            .await
            .map_err(fail("setReviewerRequest"))
    }

    async fn list_label_candidates(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<PullRequestLabelCandidateList> {
        self.cli
            .list_label_candidates(&GitHubPullRequestTarget::from(&input))
            .await
            .map_err(fail("listLabelCandidates"))
    }

    async fn set_labels(&self, input: SetLabelsInput) -> ProviderResult<()> {
        self.cli
            .set_labels(&api::SetLabelsInput {
                target: GitHubPullRequestTarget::from(&input.request),
                labels: input.labels,
                applied: input.applied,
            })
            .await
            .map_err(fail("setLabels"))
    }

    async fn run_action(&self, input: RunActionInput) -> ProviderResult<()> {
        self.cli
            .run_pull_request_action(&api::RunPullRequestActionInput {
                target: GitHubPullRequestTarget::from(&input.request),
                action: input.action,
                stack_number: input.stack_number,
                expected_stack_heads: input.expected_stack_heads,
                remove_agent_credits_on_merge: input.remove_agent_credits_on_merge,
                merge_method: input.merge_method,
                update_method: input.update_method,
            })
            .await
            .map_err(fail("runAction"))
    }

    async fn update_change_request(&self, input: UpdateChangeRequestInput) -> ProviderResult<()> {
        self.cli
            .update_pull_request(&api::UpdatePullRequestInput {
                target: GitHubPullRequestTarget::from(&input.request),
                title: input.title,
                body: input.body,
            })
            .await
            .map_err(fail("updateChangeRequest"))
    }

    async fn comment(&self, input: CommentInput) -> ProviderResult<()> {
        self.cli
            .comment_on_pull_request(&api::CommentOnPullRequestInput {
                target: GitHubPullRequestTarget::from(&input.request),
                body: input.body,
            })
            .await
            .map_err(fail("comment"))
    }

    async fn update_comment(&self, input: UpdateCommentInput) -> ProviderResult<()> {
        self.cli
            .update_comment(&api::UpdateCommentInput {
                target: GitHubPullRequestTarget::from(&input.request),
                comment_id: input.comment_id,
                kind: match input.kind {
                    CommentUpdateKind::IssueComment => api::UpdateCommentKind::IssueComment,
                    CommentUpdateKind::ReviewComment => api::UpdateCommentKind::ReviewComment,
                },
                body: input.body,
            })
            .await
            .map_err(fail("updateComment"))
    }

    async fn submit_review(&self, input: SubmitReviewInput) -> ProviderResult<()> {
        self.cli
            .submit_review(&api::SubmitReviewInput {
                target: GitHubPullRequestTarget::from(&input.request),
                verdict: input.verdict,
                body: input.body,
                comments: input.comments,
            })
            .await
            .map_err(fail("submitReview"))
    }

    async fn reply_to_thread(&self, input: ReplyToThreadInput) -> ProviderResult<()> {
        self.cli
            .reply_to_review_thread(&api::ReplyToReviewThreadInput {
                cwd: input.request.repo.cwd,
                repository: input.request.repo.repository,
                host: input.request.repo.host,
                thread_id: input.thread_id,
                body: input.body,
            })
            .await
            .map_err(fail("replyToThread"))
    }

    async fn set_reaction(&self, input: SetReactionInput) -> ProviderResult<()> {
        self.cli
            .set_reaction(&api::SetReactionInput {
                target: GitHubPullRequestTarget::from(&input.request),
                subject_id: input.subject_id,
                content: input.content,
                reacted: input.reacted,
            })
            .await
            .map_err(fail("setReaction"))
    }

    async fn set_thread_resolution(&self, input: SetThreadResolutionInput) -> ProviderResult<()> {
        self.cli
            .set_review_thread_resolution(&api::SetReviewThreadResolutionInput {
                cwd: input.request.repo.cwd,
                repository: input.request.repo.repository,
                host: input.request.repo.host,
                thread_id: input.thread_id,
                resolved: input.resolved,
            })
            .await
            .map_err(fail("setThreadResolution"))
    }
}

/// What a failed thread read degrades to: no thread comments, a conversation marked truncated.
fn degraded_review_threads() -> GitHubReviewThreadComments {
    GitHubReviewThreadComments {
        comments: Vec::new(),
        dismissals_by_review_id: IndexMap::new(),
        review_threads: Vec::new(),
        comment_count: 0,
        truncated: true,
        review_threads_truncated: true,
        reactions: Vec::new(),
        reactions_by_id: IndexMap::new(),
        edited_at_by_id: IndexMap::new(),
        reviewers: Vec::new(),
        avatars_by_login: IndexMap::new(),
        bot_logins: IndexSet::new(),
        commit_stats: IndexMap::new(),
        commits: Vec::new(),
        viewer: GitHubViewerFlags {
            can_update: true,
            did_author: false,
        },
    }
}
