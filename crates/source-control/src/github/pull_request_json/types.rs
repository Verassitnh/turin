//! The normalized shapes the decoders hand to the rest of the GitHub driver.
//!
//! TS interfaces that `extends` another embed the parent as `base` and `Deref` to it, the same
//! convention `source_control_core::pull_request_provider` uses. `x?: T` is `Option<T>`;
//! `x: T | null` is `Option<T>`.

use std::ops::{Deref, DerefMut};

use indexmap::{IndexMap, IndexSet};
use turin_contracts::pull_request::{
    PullRequestActor, PullRequestCheck, PullRequestChecksState, PullRequestComment,
    PullRequestCommit, PullRequestLabel, PullRequestMergeCapabilities, PullRequestMergeMethod,
    PullRequestMergeability, PullRequestOmittedFileStat, PullRequestReaction,
    PullRequestReviewDecision, PullRequestReviewThread, PullRequestStackMembership,
    PullRequestState,
};

/// `Cause<SchemaError>`: the payload was not valid JSON, or did not have the expected shape. The
/// message names what failed, never a value from the payload.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct DecodeFailure {
    pub message: String,
}

impl DecodeFailure {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub type DecodeResult<T> = Result<T, DecodeFailure>;

#[derive(Debug, Clone, PartialEq)]
pub struct GitHubPullRequestListItem {
    pub stack: Option<PullRequestStackMembership>,
    /// The author's node id, kept so a batch can resolve the avatar the listing does not carry.
    pub author_id: Option<String>,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: Option<PullRequestActor>,
    pub head_branch: String,
    pub base_branch: String,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub mergeability: PullRequestMergeability,
    /// Null where GitHub has no verdict to summarise, which includes a draft nobody has reviewed.
    pub review_decision: Option<PullRequestReviewDecision>,
    pub additions: u64,
    pub deletions: u64,
    pub created_at: String,
    pub updated_at: String,
    pub review_request_logins: Vec<String>,
    /// At least one outstanding request targets a team rather than an individual login.
    pub has_team_review_request: bool,
    pub labels: Vec<PullRequestLabel>,
    /// Null where the head commit reported no checks, which is not the same as passing none.
    pub checks_state: Option<PullRequestChecksState>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitHubPullRequestDetail {
    pub base: GitHubPullRequestListItem,
    /// True only when GitHub says the head belongs to another repository.
    pub is_cross_repository: Option<bool>,
    /// The owner of the head branch's repository; `None` where `gh` did not say.
    pub head_repository_owner: Option<String>,
    pub head_sha: Option<String>,
    pub body: String,
    pub changed_files: u64,
    pub merged_at: Option<String>,
    pub closed_at: Option<String>,
    pub checks: Vec<PullRequestCheck>,
    /// Absent where `gh` did not answer for auto-merge at all, which is not the same as off.
    pub auto_merge_enabled: Option<bool>,
    /// Absent where auto-merge is off or GitHub did not report the stored strategy.
    pub auto_merge_method: Option<PullRequestMergeMethod>,
}

impl Deref for GitHubPullRequestDetail {
    type Target = GitHubPullRequestListItem;
    fn deref(&self) -> &GitHubPullRequestListItem {
        &self.base
    }
}
impl DerefMut for GitHubPullRequestDetail {
    fn deref_mut(&mut self) -> &mut GitHubPullRequestListItem {
        &mut self.base
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubWorkflowRunApproval {
    pub id: i64,
    pub name: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubPullRequestHead {
    pub number: u64,
    pub head_sha: String,
    pub is_cross_repository: Option<bool>,
    pub head_repository_owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitHubPullRequestActivity {
    pub author: Option<PullRequestActor>,
    pub comments: Vec<PullRequestComment>,
    pub commits: Vec<PullRequestCommit>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitHubPullRequestListBatch {
    pub items: Vec<GitHubPullRequestListItem>,
    /// Rows GitHub returned, counted before decoding, so a skipped row cannot hide a next page.
    pub raw_count: usize,
    /// Where the next page starts, or `None` once GitHub has handed over every row.
    pub end_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitHubPullRequestSearchItem {
    pub base: GitHubPullRequestListItem,
    /// `owner/name` as GitHub spells it, which is how a row from a search finds its repository.
    pub repository: String,
}

impl Deref for GitHubPullRequestSearchItem {
    type Target = GitHubPullRequestListItem;
    fn deref(&self) -> &GitHubPullRequestListItem {
        &self.base
    }
}
impl DerefMut for GitHubPullRequestSearchItem {
    fn deref_mut(&mut self) -> &mut GitHubPullRequestListItem {
        &mut self.base
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitHubPullRequestSearchBatch {
    pub items: Vec<GitHubPullRequestSearchItem>,
    /// Rows the search returned, counted before decoding, so a skipped row cannot hide a next page.
    pub raw_count: usize,
    /// More rows than this slice asked for, which is truncation for every repository in it.
    pub has_next_page: bool,
    /// Where the next page of the same search starts.
    pub end_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitHubPullRequestSummary {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub head_branch: String,
    pub base_branch: String,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub closed_at: Option<String>,
    pub merged_at: Option<String>,
    pub updated_at: String,
    pub author: Option<PullRequestActor>,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub review_decision: Option<PullRequestReviewDecision>,
    pub checks_state: Option<PullRequestChecksState>,
    pub mergeability: PullRequestMergeability,
    /// `Some(None)` when GitHub says the pull request is in no stack; `None` when the read did
    /// not ask.
    pub stack: Option<Option<PullRequestStackMembership>>,
}

/// Two parts, so a watch reads only what moved: `status` (state, mergeability, head, and checks)
/// needs the detail read, and `remarks` also needs the far costlier activity read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubPullRequestWatchFingerprint {
    pub status: String,
    pub remarks: String,
}

/// Line counts for one row of a stats read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitHubDiffCounts {
    pub additions: u64,
    pub deletions: u64,
}

/// Repository settings returned alongside the pull request viewer permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitHubRepositoryAccess {
    pub merge_capabilities: PullRequestMergeCapabilities,
    pub can_write: bool,
}

/// Everything GitHub says about what the signed-in account may do here. `can_write` is about the
/// repository, the other two about this pull request in particular.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitHubViewerAccess {
    pub can_write: bool,
    /// The viewer's role reaches triage, which is the least that may label.
    pub can_triage: bool,
    /// GitHub's own `viewerCanUpdate`, true for the author as well as for anyone with write.
    pub can_update: bool,
    pub did_author: bool,
    /// GitHub's own `viewerCanUpdateBranch`, read with the base comparison rather than here.
    /// Absent where the comparison was not read.
    pub can_update_branch: Option<bool>,
}

/// `GitHubViewerAccess & GitHubRepositoryAccess`: both halves share `can_write`, so the viewer
/// half is embedded (and dereferenced to) and only the merge settings are added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitHubViewerRepositoryAccess {
    pub access: GitHubViewerAccess,
    pub merge_capabilities: PullRequestMergeCapabilities,
}

impl Deref for GitHubViewerRepositoryAccess {
    type Target = GitHubViewerAccess;
    fn deref(&self) -> &GitHubViewerAccess {
        &self.access
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitHubBaseComparison {
    /// `None` where the host could not compare, which the page reads as "unknown".
    pub behind_by: Option<u64>,
    pub viewer_can_update: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitHubPullRequestCore {
    pub base: GitHubPullRequestDetail,
    /// Teams asked for a review. `review_request_logins` holds the people.
    pub review_request_teams: Vec<PullRequestActor>,
    pub viewer_access: GitHubViewerRepositoryAccess,
    pub comparison: Option<GitHubBaseComparison>,
    pub checks_truncated: bool,
}

impl Deref for GitHubPullRequestCore {
    type Target = GitHubPullRequestDetail;
    fn deref(&self) -> &GitHubPullRequestDetail {
        &self.base
    }
}
impl DerefMut for GitHubPullRequestCore {
    fn deref_mut(&mut self) -> &mut GitHubPullRequestDetail {
        &mut self.base
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubWorkflowRunPage {
    pub runs: Vec<GitHubWorkflowRunApproval>,
    /// Runs on the page, counted before decoding, which is what decides whether to page on.
    pub raw_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubPullRequestHeadsPage {
    pub heads: Vec<GitHubPullRequestHead>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitHubPullRequestActivityPage {
    /// Only on the first page, which is the one that asks for them. `Some(None)` is a null author.
    pub author: Option<Option<PullRequestActor>>,
    pub commits: Option<Vec<PullRequestCommit>>,
    /// Issue comments and reviews, sorted by creation time.
    pub remarks: Vec<PullRequestComment>,
    pub next_comments_cursor: Option<String>,
    pub next_reviews_cursor: Option<String>,
}

/// A check context as the rollup reports it; opaque outside this module.
pub type GitHubCheckContext = super::raw::RawCheck;

#[derive(Debug, Clone, PartialEq)]
pub struct GitHubCheckContextsPage {
    pub head_sha: String,
    pub contexts: Vec<GitHubCheckContext>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitHubPullRequestChecksFromContexts {
    pub checks: Vec<PullRequestCheck>,
    pub checks_state: Option<PullRequestChecksState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitHubViewerFlags {
    pub can_update: bool,
    pub did_author: bool,
}

/// One thread as a page found it, with what it takes to finish reading it.
#[derive(Debug, Clone, PartialEq)]
pub struct GitHubReviewThreadEntry {
    pub thread: PullRequestReviewThread,
    /// How many comments GitHub says the thread holds, read or not.
    pub comment_count: u64,
    /// Where the rest of this thread's comments carry on from, or `None` once it is whole.
    pub next_comment_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitHubReviewThreadPage {
    pub threads: Vec<GitHubReviewThreadEntry>,
    /// Where the next page of threads starts, or `None` once the host has handed them all over.
    pub next_cursor: Option<String>,
    /// The pull request's own reactions, which sit on its description.
    pub reactions: Vec<PullRequestReaction>,
    /// Reactions by node id, for the conversation comments and reviews `gh pr view --json`
    /// answers for without any. Only ids with a reaction are here.
    pub reactions_by_id: IndexMap<String, Vec<PullRequestReaction>>,
    pub edited_at_by_id: IndexMap<String, String>,
    pub reviewers: Vec<PullRequestActor>,
    pub avatars_by_login: IndexMap<String, String>,
    pub bot_logins: IndexSet<String>,
    pub commit_stats: IndexMap<String, GitHubDiffCounts>,
    pub commits: Vec<PullRequestCommit>,
    pub viewer: GitHubViewerFlags,
    /// Dismissal reasons by the dismissed review's node id, which the review itself never carries.
    pub dismissals_by_review_id: IndexMap<String, String>,
    /// Where the rest of the dismissal events start, or `None` once this page carried them all.
    pub next_dismissal_cursor: Option<String>,
}

/// The merged form of every page of a thread read.
#[derive(Debug, Clone, PartialEq)]
pub struct GitHubReviewThreadComments {
    pub comments: Vec<PullRequestComment>,
    pub dismissals_by_review_id: IndexMap<String, String>,
    pub review_threads: Vec<PullRequestReviewThread>,
    pub comment_count: u64,
    pub truncated: bool,
    pub review_threads_truncated: bool,
    pub reactions: Vec<PullRequestReaction>,
    pub reactions_by_id: IndexMap<String, Vec<PullRequestReaction>>,
    pub edited_at_by_id: IndexMap<String, String>,
    pub reviewers: Vec<PullRequestActor>,
    pub avatars_by_login: IndexMap<String, String>,
    pub bot_logins: IndexSet<String>,
    pub commit_stats: IndexMap<String, GitHubDiffCounts>,
    pub commits: Vec<PullRequestCommit>,
    pub viewer: GitHubViewerFlags,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitHubReviewDismissalsPage {
    pub dismissals_by_review_id: IndexMap<String, String>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitHubReviewThreadCommentsPage {
    pub belongs_to_pull_request: bool,
    pub comments: Vec<turin_contracts::pull_request::PullRequestThreadComment>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitHubPullRequestFilesPatch {
    pub patch: String,
    /// At least one file's hunks were withheld by GitHub, so they are missing from the patch.
    pub truncated: bool,
    /// Files GitHub returned, counted before decoding, so the caller can page.
    pub raw_count: usize,
    /// GitHub's own counts for the files whose hunks it withheld.
    pub omitted_file_stats: Vec<PullRequestOmittedFileStat>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubViewedFile {
    pub path: String,
    pub state: turin_contracts::pull_request::PullRequestFileViewedState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubPullRequestFilesViewedPage {
    pub files: Vec<GitHubViewedFile>,
    /// Where the next page carries on, or `None` once the host has no more to give.
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubPullRequestStackLayer {
    pub title: Option<String>,
    pub is_draft: Option<bool>,
    pub head_sha: Option<String>,
    pub number: u64,
    pub head_branch: String,
    pub state: PullRequestState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubPullRequestStack {
    pub id: String,
    pub number: u64,
    pub url: String,
    pub base: String,
    /// Bottom to top, which is the order GitHub lists them in.
    pub layers: Vec<GitHubPullRequestStackLayer>,
}
