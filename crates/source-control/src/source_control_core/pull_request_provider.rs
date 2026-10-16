//! Port of upstream/packages/source-control-core/src/server/PullRequestProvider.ts
//!
//! Conventions for the Rust port:
//! - TS interfaces that `extends` another (`ProviderChangeRequestDetail`, `ProviderBatchedChangeRequest`)
//!   embed the parent as `base` and `Deref` to it. A field the child redeclares (detail's
//!   `merged_at`/`closed_at`) is the child's own and wins over the parent's.
//! - `x?: T | null | undefined` is `Option<Option<T>>` (outer `None` = absent, `Some(None)` = null).
//! - Optional interface members (`getChangeRequestChecks?:` …) are trait methods with a default
//!   body that fails with [`unsupported`]; the matching [`PullRequestOperation`] is reported by
//!   [`PullRequestProviderApi::supports`] so callers can branch exactly where upstream tests for
//!   presence (`api.getX === undefined`).
//! - `withVerifiedCredential(input, use)` is split into [`PullRequestProviderApi::verify_credential`]
//!   (returns the identity plus a pin) and the caller running its work under
//!   [`crate::source_control_core::ambient::with_pinned_credential`]. See PORTING.md Deviations.

use std::ops::{Deref, DerefMut};

use async_trait::async_trait;
use indexmap::IndexMap;
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestActor, PullRequestBaseComparison, PullRequestCapabilities,
    PullRequestCheck, PullRequestChecks, PullRequestChecksState, PullRequestComment,
    PullRequestCommit, PullRequestDiffFileChangeType, PullRequestFileViewed,
    PullRequestInvolvement, PullRequestLabel, PullRequestLabelCandidateList,
    PullRequestListFilters, PullRequestListState, PullRequestMergeCapabilities,
    PullRequestMergeMethod, PullRequestMergeability, PullRequestOmittedFileStat,
    PullRequestReaction, PullRequestReactionContent, PullRequestReviewCommentDraft,
    PullRequestReviewDecision, PullRequestReviewThread, PullRequestReviewVerdict,
    PullRequestReviewerCandidateList, PullRequestReviewerKind, PullRequestStackHead,
    PullRequestStackMembership, PullRequestState, PullRequestThreadCommentsResult,
    PullRequestUpdateMethod, PullRequestViewerPermissions,
};
use turin_contracts::source_control::SourceControlProviderKind;

use super::Cause;

/// `PullRequestProviderError.reason`: a missing or unauthenticated tool disables the provider for
/// the whole workspace, a rate limit pauses its host, and anything else is specific to the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PullRequestProviderErrorReason {
    MissingTool,
    Unauthenticated,
    RateLimited,
    NotFound,
    Failed,
}

impl PullRequestProviderErrorReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingTool => "missing-tool",
            Self::Unauthenticated => "unauthenticated",
            Self::RateLimited => "rate-limited",
            Self::NotFound => "not-found",
            Self::Failed => "failed",
        }
    }
}

/// The one failure shape every provider reports, so the service can decide what a failure means
/// without knowing which CLI or API produced it.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{provider} failed in {operation}: {detail}")]
pub struct PullRequestProviderError {
    pub provider: SourceControlProviderKind,
    pub operation: String,
    pub reason: PullRequestProviderErrorReason,
    pub detail: String,
    /// Epoch milliseconds.
    pub retry_at: Option<i64>,
    pub cause: Option<Cause>,
}

impl PullRequestProviderError {
    pub fn new(
        provider: SourceControlProviderKind,
        operation: impl Into<String>,
        reason: PullRequestProviderErrorReason,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            operation: operation.into(),
            reason,
            detail: detail.into(),
            retry_at: None,
            cause: None,
        }
    }
    pub fn with_cause(mut self, cause: Cause) -> Self {
        self.cause = Some(cause);
        self
    }
    pub fn with_retry_at(mut self, retry_at: Option<i64>) -> Self {
        self.retry_at = retry_at;
        self
    }
}

/// An optional member of [`PullRequestProviderApi`]; `supports` is upstream's
/// `api.<member> !== undefined`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PullRequestOperation {
    MergeMessageRewrite,
    RepositoryKey,
    RoutingIdentity,
    VerifyCredential,
    ListChangeRequestsAcross,
    ListChangeRequestStats,
    GetChangeRequestChecks,
    GetChangeRequestPreview,
    GetChangeRequestSummary,
    GetChangeRequestWatchFingerprint,
    GetChangeRequestStack,
    GetReviewThreadComments,
    GetDiffFileContents,
    GetFilesViewed,
    SetFilesViewed,
    GetFileRevisions,
    UpdateChangeRequest,
    UpdateComment,
    ListLabelCandidates,
    SetLabels,
}

/// The failure an unimplemented optional member reports.
pub fn unsupported(
    provider: &SourceControlProviderKind,
    operation: &str,
) -> PullRequestProviderError {
    PullRequestProviderError::new(
        provider.clone(),
        operation,
        PullRequestProviderErrorReason::Failed,
        format!("{operation} is not supported by {provider}."),
    )
}

pub type ProviderResult<T> = Result<T, PullRequestProviderError>;

/// `status` moves with the state, mergeability, head, or checks, which the detail read reports;
/// `remarks` moves with comments and reviews, which only the activity read reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderChangeRequestWatchFingerprint {
    pub status: String,
    pub remarks: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PullRequestProviderFailure {
    pub reason: PullRequestProviderErrorReason,
    pub retry_at: Option<i64>,
}

/// A change request as the provider sees it, before the service attaches project context.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderChangeRequest {
    pub stack: Option<PullRequestStackMembership>,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: Option<PullRequestActor>,
    pub head_branch: String,
    pub head_repository_name_with_owner: Option<Option<String>>,
    pub base_branch: String,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub mergeability: PullRequestMergeability,
    pub additions: u64,
    pub deletions: u64,
    pub created_at: String,
    pub closed_at: Option<Option<String>>,
    pub merged_at: Option<Option<String>>,
    pub updated_at: String,
    /// Accounts with a review requested. Team-level requests are excluded by each provider.
    pub review_request_logins: Vec<String>,
    pub labels: Vec<PullRequestLabel>,
    /// Absent from a host that does not summarise its reviews, which is every host but GitHub.
    pub review_decision: Option<Option<PullRequestReviewDecision>>,
    /// Absent from a host that reports no check rollup on its listings.
    pub checks_state: Option<Option<PullRequestChecksState>>,
}

/// The fields needed to keep a linked thread's pull request status live.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderChangeRequestSummary {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub head_branch: String,
    pub base_branch: String,
    pub state: PullRequestState,
    /// Present when the host says an open pull request is still a draft.
    pub is_draft: Option<bool>,
    pub closed_at: Option<Option<String>>,
    pub merged_at: Option<Option<String>>,
    pub updated_at: String,
    /// Overview fields, present where the host's single read returns them at no extra cost.
    pub author: Option<Option<PullRequestActor>>,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub changed_files: Option<u64>,
    pub review_decision: Option<Option<PullRequestReviewDecision>>,
    pub checks_state: Option<Option<PullRequestChecksState>>,
    pub mergeability: Option<PullRequestMergeability>,
    /// The host-native stack the pull request sits in, from the same read. `Some(None)` when the
    /// host says it is in none; `None` when the read did not ask.
    pub stack: Option<Option<PullRequestStackMembership>>,
}

/// One layer of a host-native stack, bottom to top order is the array's.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderChangeRequestStackLayer {
    pub title: Option<String>,
    pub is_draft: Option<bool>,
    pub head_sha: Option<String>,
    pub number: u64,
    pub head_branch: String,
    pub state: PullRequestState,
}

/// A host-native stack: an ordered set of change requests the host itself merges and retargets as
/// a unit. Only GitHub offers one today.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderChangeRequestStack {
    pub id: String,
    pub number: u64,
    pub url: String,
    pub base: String,
    pub layers: Vec<ProviderChangeRequestStackLayer>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderChangeRequestPage {
    pub items: Vec<ProviderChangeRequest>,
    /// True when the host has more rows than the page size asked for.
    pub truncated: bool,
    /// Optional count-based cursor advance. Most hosts advance by the rows delivered after local
    /// de-duplication; an offset-paged host may need to count malformed raw rows it consumed too.
    pub cursor_advance: Option<u64>,
    /// This page can be carried on from, so the service may hand the caller a cursor for it.
    pub continues: bool,
}

/// Where a repository's next slice starts, as the provider that has to ask for it needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderListCursor {
    /// The instant of the oldest row already handed over, checked against a timestamp's shape
    /// before it gets here because it goes into a host's own filter. Asked for inclusively.
    pub updated_before: String,
    /// How many provider rows this repository has consumed so far, for a host that carries on by
    /// counting rather than by date.
    pub delivered: u64,
}

/// One repository's row inside an answer that spans several of them.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderBatchedChangeRequest {
    pub base: ProviderChangeRequest,
    /// Provider-native identity, exactly as it was asked for, so the caller can file the row.
    pub repository: String,
}

impl Deref for ProviderBatchedChangeRequest {
    type Target = ProviderChangeRequest;
    fn deref(&self) -> &ProviderChangeRequest {
        &self.base
    }
}
impl DerefMut for ProviderBatchedChangeRequest {
    fn deref_mut(&mut self) -> &mut ProviderChangeRequest {
        &mut self.base
    }
}

/// One slice of a host read across several repositories at once, newest update first.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderBatchedChangeRequestPage {
    pub items: Vec<ProviderBatchedChangeRequest>,
    /// True when the host has more rows than the slice asked for, for any of the repositories.
    pub truncated: bool,
}

/// The line counts for one change request, which a listing may leave for a second read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderChangeRequestStat {
    pub repository: String,
    pub number: u64,
    pub additions: u64,
    pub deletions: u64,
}

/// `ProviderChangeRequestDetail extends ProviderChangeRequest`. `merged_at`/`closed_at` here are
/// the detail's own required `string | null` fields.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderChangeRequestDetail {
    pub base: ProviderChangeRequest,
    /// The head commit, where the host's detail read reports it.
    pub head_sha: Option<Option<String>>,
    pub body: String,
    pub changed_files: u64,
    pub merged_at: Option<String>,
    pub closed_at: Option<String>,
    pub reviewers: Vec<PullRequestActor>,
    pub checks: Vec<PullRequestCheck>,
    pub merge_capabilities: PullRequestMergeCapabilities,
    pub viewer_permissions: PullRequestViewerPermissions,
    /// Absent from a host that cannot compare the branch with its base, which is most of them.
    pub base_comparison: Option<PullRequestBaseComparison>,
    pub behind_by: Option<u64>,
    /// Absent from a host that does not report whether it is armed to merge this on its own.
    pub auto_merge_enabled: Option<bool>,
    /// The strategy stored with an armed auto-merge, where the host reports it.
    pub auto_merge_method: Option<PullRequestMergeMethod>,
    /// Workflow runs on this head commit that still need a maintainer's approval.
    pub workflow_approvals_required: Option<u64>,
}

impl Deref for ProviderChangeRequestDetail {
    type Target = ProviderChangeRequest;
    fn deref(&self) -> &ProviderChangeRequest {
        &self.base
    }
}
impl DerefMut for ProviderChangeRequestDetail {
    fn deref_mut(&mut self) -> &mut ProviderChangeRequest {
        &mut self.base
    }
}

/// The conversation-shaped half of a detail, loaded after the core can already render.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderChangeRequestActivity {
    /// An optional richer actor, e.g. after GitHub's GraphQL read supplies an avatar.
    pub author: Option<Option<PullRequestActor>>,
    /// Optional because most hosts already report their reviewer list in the core detail.
    pub reviewers: Option<Vec<PullRequestActor>>,
    pub comments: Vec<PullRequestComment>,
    /// The host's own count of the conversation, which a bounded read can fall short of.
    pub comment_count: u64,
    pub comments_truncated: bool,
    pub review_threads_truncated: Option<bool>,
    pub review_threads: Vec<PullRequestReviewThread>,
    pub commits: Vec<PullRequestCommit>,
    /// The change request's own reactions, from a host that has them.
    pub reactions: Option<Vec<PullRequestReaction>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderDiffSlice {
    pub patch: String,
    /// Something in this slice could not be shown, as opposed to there being more slices.
    pub truncated: bool,
    pub next_cursor: Option<String>,
    /// The host's own counts for the files whose hunks it withheld from this slice.
    pub omitted_file_stats: Option<Vec<PullRequestOmittedFileStat>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDiffFileContents {
    pub old_contents: String,
    pub new_contents: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderFilesViewed {
    pub files: Vec<PullRequestFileViewed>,
    /// The host has more files than were read, so the ones missing here are not "unviewed".
    pub truncated: bool,
}

/// What version each of the asked-for files is at, on the change request's head. Opaque strings,
/// compared only against one another, where the empty string is the answer for a file the change
/// request deletes rather than a gap. A path is absent only where the read could not say.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderFileRevisions {
    pub revisions: IndexMap<String, String>,
    /// Whether these are every file the change request carries rather than only the paths asked
    /// about. False for a read cut short, which cannot speak past the cut.
    pub complete: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderRepositoryRef {
    pub cwd: String,
    /// Provider-native repository identity, e.g. `owner/repo` or `group/subgroup/project`.
    pub repository: String,
    /// The host it lives on, which `repository` deliberately leaves out.
    pub host: String,
}

/// `ProviderRepositoryRef & { number }`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderChangeRequestRef {
    pub repo: ProviderRepositoryRef,
    pub number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCredentialInput {
    pub cwd: String,
    pub host: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingIdentity {
    pub account_id: String,
    pub viewer: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedIdentity {
    pub account_id: String,
    pub viewer: String,
    pub credential_fingerprint: String,
}

#[derive(Debug, Clone)]
pub struct ListChangeRequestsInput {
    pub repo: ProviderRepositoryRef,
    pub state: PullRequestListState,
    pub involvement: PullRequestInvolvement,
    pub viewer: String,
    pub limit: u32,
    /// Free text to narrow the listing by, as the host understands it.
    pub query: Option<String>,
    /// Where to carry on from; absent asks for the first slice.
    pub cursor: Option<ProviderListCursor>,
    /// Further narrowings, which a host applies as far as it can and ignores the rest of.
    pub filters: Option<PullRequestListFilters>,
}

#[derive(Debug, Clone)]
pub struct ListChangeRequestsAcrossInput {
    /// Any checkout on the host, which is what the tool is run in.
    pub cwd: String,
    pub host: String,
    pub repositories: Vec<String>,
    pub state: PullRequestListState,
    pub involvement: PullRequestInvolvement,
    pub viewer: String,
    pub limit: u32,
    pub query: Option<String>,
    pub cursor: Option<ProviderListCursor>,
    pub filters: Option<PullRequestListFilters>,
}

#[derive(Debug, Clone)]
pub struct ListChangeRequestStatsInput {
    pub cwd: String,
    pub host: String,
    /// `(repository, number)` pairs.
    pub change_requests: Vec<(String, u64)>,
}

#[derive(Debug, Clone)]
pub struct GetChangeRequestStackInput {
    pub request: ProviderChangeRequestRef,
    pub include_details: bool,
}

#[derive(Debug, Clone)]
pub struct GetReviewThreadCommentsInput {
    pub request: ProviderChangeRequestRef,
    pub thread_id: String,
    pub cursor: String,
}

#[derive(Debug, Clone)]
pub struct GetViewerPermissionsInput {
    pub request: ProviderChangeRequestRef,
    /// Skip branch comparison when checking permission for an unrelated operation.
    pub include_update_branch: bool,
}

#[derive(Debug, Clone)]
pub struct GetDiffInput {
    pub request: ProviderChangeRequestRef,
    pub cursor: Option<String>,
    /// One commit's own changes, rather than everything the change request carries.
    pub commit: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GetDiffFileContentsInput {
    pub request: ProviderChangeRequestRef,
    pub commit: Option<String>,
    pub change_type: PullRequestDiffFileChangeType,
    pub old_path: String,
    pub new_path: String,
}

#[derive(Debug, Clone)]
pub struct SetFilesViewedInput {
    pub request: ProviderChangeRequestRef,
    pub files: Vec<(String, bool)>,
}

#[derive(Debug, Clone)]
pub struct GetFileRevisionsInput {
    pub request: ProviderChangeRequestRef,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RunActionInput {
    pub request: ProviderChangeRequestRef,
    pub action: PullRequestAction,
    pub stack_number: Option<u64>,
    pub expected_stack_heads: Option<Vec<PullRequestStackHead>>,
    /// Apply `merge_message_rewrite` to the merge message; never sent to a host without one.
    pub remove_agent_credits_on_merge: bool,
    /// Meaningful for `merge` and `enable-auto-merge`; absent takes the host's own default.
    pub merge_method: Option<PullRequestMergeMethod>,
    /// Only meaningful for `update-branch`; absent takes the host's own default.
    pub update_method: Option<PullRequestUpdateMethod>,
}

#[derive(Debug, Clone)]
pub struct UpdateChangeRequestInput {
    pub request: ProviderChangeRequestRef,
    pub title: Option<String>,
    pub body: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CommentInput {
    pub request: ProviderChangeRequestRef,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentUpdateKind {
    IssueComment,
    ReviewComment,
}

#[derive(Debug, Clone)]
pub struct UpdateCommentInput {
    pub request: ProviderChangeRequestRef,
    pub comment_id: String,
    pub kind: CommentUpdateKind,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct SubmitReviewInput {
    pub request: ProviderChangeRequestRef,
    pub verdict: PullRequestReviewVerdict,
    pub body: String,
    pub comments: Vec<PullRequestReviewCommentDraft>,
}

#[derive(Debug, Clone)]
pub struct SetReviewerRequestInput {
    pub request: ProviderChangeRequestRef,
    pub reviewers: Vec<(String, PullRequestReviewerKind)>,
    pub requested: bool,
}

#[derive(Debug, Clone)]
pub struct SetLabelsInput {
    pub request: ProviderChangeRequestRef,
    pub labels: Vec<String>,
    pub applied: bool,
}

#[derive(Debug, Clone)]
pub struct ReplyToThreadInput {
    pub request: ProviderChangeRequestRef,
    pub thread_id: String,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct SetReactionInput {
    pub request: ProviderChangeRequestRef,
    /// A remark's id as the conversation carried it; absent means the change request itself.
    pub subject_id: Option<String>,
    pub content: PullRequestReactionContent,
    pub reacted: bool,
}

#[derive(Debug, Clone)]
pub struct SetThreadResolutionInput {
    pub request: ProviderChangeRequestRef,
    pub thread_id: String,
    pub resolved: bool,
}

/// `Omit<PullRequestPreview, "projectId" | "repository">`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderChangeRequestPreview {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: Option<PullRequestActor>,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub created_at: String,
}

/// One host's change requests. Implementations own their own tool and JSON shapes and hand back
/// the neutral types above; anything a host cannot do is declared in `capabilities` rather than
/// failing at call time.
#[async_trait]
pub trait PullRequestProviderApi: Send + Sync {
    fn kind(&self) -> SourceControlProviderKind;
    fn capabilities(&self) -> &PullRequestCapabilities;

    /// Whether an optional member is implemented (upstream `api.member !== undefined`).
    fn supports(&self, _operation: PullRequestOperation) -> bool {
        false
    }

    /// Rewrites the message a merge will use, for a host that lets the merge carry custom text.
    /// `None` means the host always writes its own message. Today's only rewrite strips agent
    /// credits (`merge_message::remove_agent_credits`) when the project asks for it.
    fn merge_message_rewrite(&self, _message: &str) -> Option<String> {
        None
    }

    /// The key a checkout's repository is known by on this host, where `owner/name` is not enough.
    /// `None` means `owner/name` on the host identifies it.
    fn repository_key(&self, _canonical_key: &str) -> Option<String> {
        None
    }

    async fn get_routing_identity(
        &self,
        _input: VerifiedCredentialInput,
    ) -> ProviderResult<RoutingIdentity> {
        Err(unsupported(&self.kind(), "getRoutingIdentity"))
    }

    /// Verifies the credential a request to `input.host` would carry and returns the identity plus
    /// the pin to run follow-up work under (upstream `withVerifiedCredential`).
    async fn verify_credential(
        &self,
        _input: VerifiedCredentialInput,
    ) -> ProviderResult<(VerifiedIdentity, super::ambient::PinnedCredential)> {
        Err(unsupported(&self.kind(), "withVerifiedCredential"))
    }

    /// The signed-in account, which is what involvement filtering compares against.
    async fn get_viewer(&self, cwd: &str, host: Option<&str>) -> ProviderResult<String>;

    async fn list_change_requests(
        &self,
        input: ListChangeRequestsInput,
    ) -> ProviderResult<ProviderChangeRequestPage>;

    /// The same listing for a whole host in one request, for a host that has a search across
    /// repositories. The caller falls back to `list_change_requests` per repository where this is
    /// absent, and where it fails.
    async fn list_change_requests_across(
        &self,
        _input: ListChangeRequestsAcrossInput,
    ) -> ProviderResult<ProviderBatchedChangeRequestPage> {
        Err(unsupported(&self.kind(), "listChangeRequestsAcross"))
    }

    /// The line counts for rows a listing has already handed over.
    async fn list_change_request_stats(
        &self,
        _input: ListChangeRequestStatsInput,
    ) -> ProviderResult<Vec<ProviderChangeRequestStat>> {
        Err(unsupported(&self.kind(), "listChangeRequestStats"))
    }

    async fn get_change_request_checks(
        &self,
        _input: ProviderChangeRequestRef,
    ) -> ProviderResult<PullRequestChecks> {
        Err(unsupported(&self.kind(), "getChangeRequestChecks"))
    }

    async fn get_change_request(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<ProviderChangeRequestDetail>;

    /// Hosts without a narrow read use their existing detail response for hover cards.
    async fn get_change_request_preview(
        &self,
        _input: ProviderChangeRequestRef,
    ) -> ProviderResult<ProviderChangeRequestPreview> {
        Err(unsupported(&self.kind(), "getChangeRequestPreview"))
    }

    /// The cheap live fields used by linked threads.
    async fn get_change_request_summary(
        &self,
        _input: ProviderChangeRequestRef,
    ) -> ProviderResult<ProviderChangeRequestSummary> {
        Err(unsupported(&self.kind(), "getChangeRequestSummary"))
    }

    /// A cheap fingerprint of what a pull request watch reports, so it reads the change request in
    /// full only when this moves. `None` when the host gave no answer for it.
    async fn get_change_request_watch_fingerprint(
        &self,
        _input: ProviderChangeRequestRef,
    ) -> ProviderResult<Option<ProviderChangeRequestWatchFingerprint>> {
        Err(unsupported(
            &self.kind(),
            "getChangeRequestWatchFingerprint",
        ))
    }

    /// The host-native stack a change request belongs to, or `None` when it is not stacked.
    async fn get_change_request_stack(
        &self,
        _input: GetChangeRequestStackInput,
    ) -> ProviderResult<Option<ProviderChangeRequestStack>> {
        Err(unsupported(&self.kind(), "getChangeRequestStack"))
    }

    /// Comments, line threads, and commits, kept off the critical path for the core detail.
    async fn get_change_request_activity(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<ProviderChangeRequestActivity>;

    /// One explicit page after a reader asks to continue an unfinished review thread.
    async fn get_review_thread_comments(
        &self,
        _input: GetReviewThreadCommentsInput,
    ) -> ProviderResult<PullRequestThreadCommentsResult> {
        Err(unsupported(&self.kind(), "getReviewThreadComments"))
    }

    /// The same answer `get_change_request` carries, on its own, read freshly before a write.
    async fn get_viewer_permissions(
        &self,
        input: GetViewerPermissionsInput,
    ) -> ProviderResult<PullRequestViewerPermissions>;

    /// One slice of the patch. Only called when `capabilities.diff` is true.
    async fn get_diff(&self, input: GetDiffInput) -> ProviderResult<ProviderDiffSlice>;

    /// Full files at the exact revisions the host used for its patch.
    async fn get_diff_file_contents(
        &self,
        _input: GetDiffFileContentsInput,
    ) -> ProviderResult<ProviderDiffFileContents> {
        Err(unsupported(&self.kind(), "getDiffFileContents"))
    }

    /// Which files the reader has already cleared. Only called when `capabilities.viewedFiles` is
    /// `"host"`.
    async fn get_files_viewed(
        &self,
        _input: ProviderChangeRequestRef,
    ) -> ProviderResult<ProviderFilesViewed> {
        Err(unsupported(&self.kind(), "getFilesViewed"))
    }

    /// Clears files, or puts them back. Only called when `capabilities.viewedFiles` is `"host"`.
    async fn set_files_viewed(&self, _input: SetFilesViewedInput) -> ProviderResult<()> {
        Err(unsupported(&self.kind(), "setFilesViewed"))
    }

    /// What version the head has of each of these files. Required of a host whose
    /// `capabilities.viewedFiles` is `"environment"`.
    async fn get_file_revisions(
        &self,
        _input: GetFileRevisionsInput,
    ) -> ProviderResult<ProviderFileRevisions> {
        Err(unsupported(&self.kind(), "getFileRevisions"))
    }

    async fn run_action(&self, input: RunActionInput) -> ProviderResult<()>;

    /// Rewrites the change request's own words. Only called when
    /// `capabilities.edit.changeRequest` is true, and never with both fields absent.
    async fn update_change_request(&self, _input: UpdateChangeRequestInput) -> ProviderResult<()> {
        Err(unsupported(&self.kind(), "updateChangeRequest"))
    }

    async fn comment(&self, input: CommentInput) -> ProviderResult<()>;

    /// Rewrites a remark somebody already posted. Only called when `capabilities.edit.comment` is
    /// true. Whether the remark is the reader's to rewrite is the host's own answer.
    async fn update_comment(&self, _input: UpdateCommentInput) -> ProviderResult<()> {
        Err(unsupported(&self.kind(), "updateComment"))
    }

    /// Sends a whole review at once.
    async fn submit_review(&self, input: SubmitReviewInput) -> ProviderResult<()>;

    /// The people this viewer may ask for a review. The author is left out by each provider.
    async fn list_reviewer_candidates(
        &self,
        input: ProviderChangeRequestRef,
    ) -> ProviderResult<PullRequestReviewerCandidateList>;

    /// Asks for a review, or takes the request back.
    async fn set_reviewer_request(&self, input: SetReviewerRequestInput) -> ProviderResult<()>;

    /// The repository's labels, with the ones already on the change request marked.
    async fn list_label_candidates(
        &self,
        _input: ProviderChangeRequestRef,
    ) -> ProviderResult<PullRequestLabelCandidateList> {
        Err(unsupported(&self.kind(), "listLabelCandidates"))
    }

    /// Puts labels on the change request, or takes them off.
    async fn set_labels(&self, _input: SetLabelsInput) -> ProviderResult<()> {
        Err(unsupported(&self.kind(), "setLabels"))
    }

    /// Only called when `capabilities.review.reply` is true.
    async fn reply_to_thread(&self, input: ReplyToThreadInput) -> ProviderResult<()>;

    /// Adds a reaction, or takes it back. Only called when `capabilities.reactions` is true.
    async fn set_reaction(&self, input: SetReactionInput) -> ProviderResult<()>;

    /// Only called when `capabilities.review.resolve` is true.
    async fn set_thread_resolution(&self, input: SetThreadResolutionInput) -> ProviderResult<()>;
}
