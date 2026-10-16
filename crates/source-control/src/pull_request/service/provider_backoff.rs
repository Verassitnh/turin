//! `withRateLimitBackoff` of `PullRequestService.ts`: wraps a provider so every call is refused
//! while its host is paused for a rate limit, reports the outcome back to the limiter, and tells
//! the call whether it is interactive.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use turin_contracts::pull_request::{
    PullRequestCapabilities, PullRequestChecks, PullRequestLabelCandidateList,
    PullRequestReviewerCandidateList, PullRequestThreadCommentsResult,
    PullRequestViewerPermissions,
};
use turin_contracts::source_control::SourceControlProviderKind;

use crate::source_control_core::ambient::{self, PinnedCredential};
use crate::source_control_core::cause_from_message;
use crate::source_control_core::pull_request_provider::*;
use crate::source_control_core::rate_limit::{
    RateLimitKey, RateLimitLease, SourceControlRateLimit,
};

/// `withRateLimitBackoff(api, host, limits, options)`.
pub(crate) struct RateLimitedProvider {
    api: Arc<dyn PullRequestProviderApi>,
    key: RateLimitKey,
    limits: Arc<SourceControlRateLimit>,
    viewer_allows_pause: bool,
}

/// Wraps `api`; the wrapper is what the service hands out as a project's provider.
pub(crate) fn with_rate_limit_backoff(
    api: Arc<dyn PullRequestProviderApi>,
    host: &str,
    limits: &Arc<SourceControlRateLimit>,
    viewer_allows_pause: bool,
) -> Arc<dyn PullRequestProviderApi> {
    Arc::new(RateLimitedProvider {
        key: RateLimitKey {
            provider: api.kind(),
            host: host.to_owned(),
        },
        api,
        limits: limits.clone(),
        viewer_allows_pause,
    })
}

impl RateLimitedProvider {
    async fn protect<T>(
        &self,
        operation: &str,
        allow_paused: bool,
        call: impl Future<Output = ProviderResult<T>>,
    ) -> ProviderResult<T> {
        let lease = self
            .limits
            .check(&self.key, allow_paused)
            .map_err(|error| PullRequestProviderError {
                provider: self.key.provider.clone(),
                operation: operation.to_owned(),
                reason: PullRequestProviderErrorReason::RateLimited,
                detail: error.detail(),
                retry_at: Some(error.retry_at),
                cause: Some(cause_from_message(error.detail())),
            })?;
        let result = ambient::with_interactive(allow_paused, call).await;
        match &result {
            Ok(_) => self
                .limits
                .record_success(&RateLimitLease::new(&self.key, lease)),
            Err(error) if error.reason == PullRequestProviderErrorReason::RateLimited => self
                .limits
                .record_rate_limit(&RateLimitLease::new(&self.key, lease), error.retry_at),
            Err(_) => {}
        }
        result
    }
}

#[async_trait]
impl PullRequestProviderApi for RateLimitedProvider {
    fn kind(&self) -> SourceControlProviderKind {
        self.api.kind()
    }

    fn capabilities(&self) -> &PullRequestCapabilities {
        self.api.capabilities()
    }

    fn supports(&self, operation: PullRequestOperation) -> bool {
        self.api.supports(operation)
    }

    fn merge_message_rewrite(&self, message: &str) -> Option<String> {
        self.api.merge_message_rewrite(message)
    }

    fn repository_key(&self, canonical_key: &str) -> Option<String> {
        self.api.repository_key(canonical_key)
    }

    async fn get_routing_identity(
        &self,
        input: VerifiedCredentialInput,
    ) -> ProviderResult<RoutingIdentity> {
        self.api.get_routing_identity(input).await
    }

    async fn verify_credential(
        &self,
        input: VerifiedCredentialInput,
    ) -> ProviderResult<(VerifiedIdentity, PinnedCredential)> {
        self.api.verify_credential(input).await
    }

    // Refused during a pause like any other read, except for the caller that asks for the
    // bypass: a lookup that failed is not held, so letting every background read through would
    // spawn this host's CLI on each of them and re-extend the pause it was already in.
    async fn get_viewer(&self, cwd: &str, host: Option<&str>) -> ProviderResult<String> {
        self.protect(
            "getViewer",
            self.viewer_allows_pause,
            self.api.get_viewer(cwd, host),
        )
        .await
    }

    async fn list_change_requests(
        &self,
        input: ListChangeRequestsInput,
    ) -> ProviderResult<ProviderChangeRequestPage> {
        self.protect(
            "listChangeRequests",
            false,
            self.api.list_change_requests(input),
        )
        .await
    }

    async fn list_change_requests_across(
        &self,
        input: ListChangeRequestsAcrossInput,
    ) -> ProviderResult<ProviderBatchedChangeRequestPage> {
        self.protect(
            "listChangeRequestsAcross",
            false,
            self.api.list_change_requests_across(input),
        )
        .await
    }

    async fn list_change_request_stats(
        &self,
        input: ListChangeRequestStatsInput,
    ) -> ProviderResult<Vec<ProviderChangeRequestStat>> {
        self.protect(
            "listChangeRequestStats",
            false,
            self.api.list_change_request_stats(input),
        )
        .await
    }

    async fn get_change_request_checks(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<PullRequestChecks> {
        self.protect(
            "getChangeRequestChecks",
            false,
            self.api.get_change_request_checks(input),
        )
        .await
    }

    async fn get_change_request(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<ProviderChangeRequestDetail> {
        self.protect(
            "getChangeRequest",
            false,
            self.api.get_change_request(input),
        )
        .await
    }

    async fn get_change_request_preview(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<ProviderChangeRequestPreview> {
        self.protect(
            "getChangeRequestPreview",
            false,
            self.api.get_change_request_preview(input),
        )
        .await
    }

    async fn get_change_request_summary(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<ProviderChangeRequestSummary> {
        self.protect(
            "getChangeRequestSummary",
            false,
            self.api.get_change_request_summary(input),
        )
        .await
    }

    async fn get_change_request_watch_fingerprint(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<Option<ProviderChangeRequestWatchFingerprint>> {
        self.protect(
            "getChangeRequestWatchFingerprint",
            false,
            self.api.get_change_request_watch_fingerprint(input),
        )
        .await
    }

    async fn get_change_request_stack(
        &self,
        input: GetChangeRequestStackInput,
    ) -> ProviderResult<Option<ProviderChangeRequestStack>> {
        self.protect(
            "getChangeRequestStack",
            false,
            self.api.get_change_request_stack(input),
        )
        .await
    }

    async fn get_change_request_activity(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<ProviderChangeRequestActivity> {
        self.protect(
            "getChangeRequestActivity",
            false,
            self.api.get_change_request_activity(input),
        )
        .await
    }

    async fn get_review_thread_comments(
        &self,
        input: GetReviewThreadCommentsInput,
    ) -> ProviderResult<PullRequestThreadCommentsResult> {
        self.protect(
            "getReviewThreadComments",
            false,
            self.api.get_review_thread_comments(input),
        )
        .await
    }

    async fn get_viewer_permissions(
        &self,
        input: GetViewerPermissionsInput,
    ) -> ProviderResult<PullRequestViewerPermissions> {
        self.protect(
            "getViewerPermissions",
            true,
            self.api.get_viewer_permissions(input),
        )
        .await
    }

    async fn get_diff(&self, input: GetDiffInput) -> ProviderResult<ProviderDiffSlice> {
        self.protect("getDiff", false, self.api.get_diff(input))
            .await
    }

    async fn get_diff_file_contents(
        &self,
        input: GetDiffFileContentsInput,
    ) -> ProviderResult<ProviderDiffFileContents> {
        self.protect(
            "getDiffFileContents",
            false,
            self.api.get_diff_file_contents(input),
        )
        .await
    }

    async fn get_files_viewed(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<ProviderFilesViewed> {
        self.protect("getFilesViewed", false, self.api.get_files_viewed(input))
            .await
    }

    async fn set_files_viewed(&self, input: SetFilesViewedInput) -> ProviderResult<()> {
        self.protect("setFilesViewed", true, self.api.set_files_viewed(input))
            .await
    }

    async fn get_file_revisions(
        &self,
        input: GetFileRevisionsInput,
    ) -> ProviderResult<ProviderFileRevisions> {
        self.protect(
            "getFileRevisions",
            false,
            self.api.get_file_revisions(input),
        )
        .await
    }

    async fn run_action(&self, input: RunActionInput) -> ProviderResult<()> {
        self.protect("runAction", true, self.api.run_action(input))
            .await
    }

    async fn update_change_request(&self, input: UpdateChangeRequestInput) -> ProviderResult<()> {
        self.protect(
            "updateChangeRequest",
            true,
            self.api.update_change_request(input),
        )
        .await
    }

    async fn comment(&self, input: CommentInput) -> ProviderResult<()> {
        self.protect("comment", true, self.api.comment(input)).await
    }

    async fn update_comment(&self, input: UpdateCommentInput) -> ProviderResult<()> {
        self.protect("updateComment", true, self.api.update_comment(input))
            .await
    }

    async fn submit_review(&self, input: SubmitReviewInput) -> ProviderResult<()> {
        self.protect("submitReview", true, self.api.submit_review(input))
            .await
    }

    async fn list_reviewer_candidates(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<PullRequestReviewerCandidateList> {
        self.protect(
            "listReviewerCandidates",
            true,
            self.api.list_reviewer_candidates(input),
        )
        .await
    }

    async fn set_reviewer_request(&self, input: SetReviewerRequestInput) -> ProviderResult<()> {
        self.protect(
            "setReviewerRequest",
            true,
            self.api.set_reviewer_request(input),
        )
        .await
    }

    async fn list_label_candidates(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<PullRequestLabelCandidateList> {
        self.protect(
            "listLabelCandidates",
            true,
            self.api.list_label_candidates(input),
        )
        .await
    }

    async fn set_labels(&self, input: SetLabelsInput) -> ProviderResult<()> {
        self.protect("setLabels", true, self.api.set_labels(input))
            .await
    }

    async fn reply_to_thread(&self, input: ReplyToThreadInput) -> ProviderResult<()> {
        self.protect("replyToThread", true, self.api.reply_to_thread(input))
            .await
    }

    async fn set_reaction(&self, input: SetReactionInput) -> ProviderResult<()> {
        self.protect("setReaction", true, self.api.set_reaction(input))
            .await
    }

    async fn set_thread_resolution(&self, input: SetThreadResolutionInput) -> ProviderResult<()> {
        self.protect(
            "setThreadResolution",
            true,
            self.api.set_thread_resolution(input),
        )
        .await
    }
}
