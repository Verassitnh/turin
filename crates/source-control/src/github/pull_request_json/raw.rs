//! The raw GitHub payload shapes, one struct per upstream `Raw*Schema`.
//!
//! Field attributes follow [`super::de`]: `#[serde(default)] Option<X>` is
//! `optional(NullOr(X))`, `#[serde(default, deserialize_with = "de::opt")]` is `optional(X)`,
//! and `deserialize_with = "de::nullable"` with no default is a required `NullOr(X)`. A spread
//! (`...Other.fields`) is `#[serde(flatten)]`.

#![allow(dead_code)]

use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

use super::de::{self, Int};

// ---------------------------------------------------------------------------------------------
// Actors, labels, reviews, checks
// ---------------------------------------------------------------------------------------------

/// Enum-ish GitHub CLI fields are decoded as plain strings and normalized later: a `gh` release
/// that adds a conclusion or a review state must not fail the whole payload.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawActor {
    #[serde(rename = "__typename", default, deserialize_with = "de::opt")]
    pub typename: Option<String>,
    #[serde(rename = "is_bot", default, deserialize_with = "de::opt")]
    pub is_bot: Option<bool>,
    /// Optional because a requested team goes by its slug instead, and a mannequin, which no
    /// query has a fragment for, arrives as an empty object.
    #[serde(default, deserialize_with = "de::opt")]
    pub login: Option<String>,
    /// The node id, which is how a listing's authors are resolved to avatars in one request.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

/// A team answers with a slug where a user answers with a login.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawRequestedReviewer {
    #[serde(flatten)]
    pub actor: RawActor,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub combined_slug: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawLabel {
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawReviewRequest {
    #[serde(default)]
    pub login: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub combined_slug: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

/// One reviewer's most recent review: the state is all the verdict needs, the author is for who.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawLatestReview {
    #[serde(default)]
    pub author: Option<RawActor>,
    #[serde(default)]
    pub state: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCheck {
    #[serde(rename = "__typename", default, deserialize_with = "de::opt")]
    pub typename: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub details_url: Option<String>,
    #[serde(default)]
    pub target_url: Option<String>,
    /// What tells two same-named checks apart, and which run of one is the newest.
    #[serde(default)]
    pub workflow_name: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    /// Branch protection requires this check; read by the detail query on github.com only.
    #[serde(default)]
    pub is_required: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCheckSuite {
    #[serde(deserialize_with = "de::nullable")]
    pub workflow_run: Option<RawWorkflowRunOfSuite>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawWorkflowRunOfSuite {
    #[serde(deserialize_with = "de::nullable")]
    pub workflow: Option<RawWorkflowName>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawWorkflowName {
    pub name: String,
}

/// A check context node of the rollup: a check plus the workflow that owns it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCheckContextNode {
    #[serde(flatten)]
    pub check: RawCheck,
    #[serde(default)]
    pub check_suite: Option<RawCheckSuite>,
}

// ---------------------------------------------------------------------------------------------
// Listing rows
// ---------------------------------------------------------------------------------------------

/// The scalar fields every listing row shape shares.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawListCommon {
    pub number: Int,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub author: Option<RawActor>,
    pub head_ref_name: String,
    pub base_ref_name: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default, deserialize_with = "de::opt")]
    pub is_draft: Option<bool>,
    #[serde(default)]
    pub mergeable: Option<String>,
    #[serde(default)]
    pub review_decision: Option<String>,
    #[serde(default, deserialize_with = "de::opt")]
    pub additions: Option<Int>,
    #[serde(default, deserialize_with = "de::opt")]
    pub deletions: Option<Int>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub merged_at: Option<String>,
}

/// A `gh pr list/view --json` shaped row: reviewers and labels flat, checks as an array.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawListItem {
    #[serde(flatten)]
    pub common: RawListCommon,
    #[serde(default, deserialize_with = "de::opt")]
    pub review_requests: Option<Vec<RawReviewRequest>>,
    #[serde(default)]
    pub latest_reviews: Option<Vec<RawLatestReview>>,
    #[serde(default, deserialize_with = "de::opt")]
    pub labels: Option<Vec<RawLabel>>,
    #[serde(default)]
    pub status_check_rollup: Option<Vec<RawCheck>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawStack {
    pub number: Int,
    pub size: Int,
    pub base_ref_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawStackEntry {
    pub position: Int,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawStackMembership {
    /// `None` absent, `Some(None)` null: the summary read distinguishes the two.
    #[serde(default, deserialize_with = "de::present")]
    pub stack: Option<Option<RawStack>>,
    #[serde(default)]
    pub stack_entry: Option<RawStackEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawLatestReviewsConnection {
    pub nodes: Vec<Option<RawLatestReview>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawRepositoryName {
    pub name_with_owner: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawSlugReviewer {
    #[serde(flatten)]
    pub actor: RawActor,
    #[serde(default)]
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawSearchReviewRequest {
    #[serde(default)]
    pub requested_reviewer: Option<RawSlugReviewer>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawSearchReviewRequests {
    #[serde(default)]
    pub nodes: Option<Vec<Option<RawSearchReviewRequest>>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawSearchLabels {
    #[serde(default)]
    pub nodes: Option<Vec<Option<RawLabel>>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawRollupState {
    pub state: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCommitRollup {
    #[serde(default)]
    pub status_check_rollup: Option<RawRollupState>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawCommitNode {
    #[serde(default)]
    pub commit: Option<RawCommitRollup>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawSearchCommits {
    #[serde(default)]
    pub nodes: Option<Vec<Option<RawCommitNode>>>,
}

/// The connection-shaped fields a GraphQL row carries where `gh` flattens them.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawSearchConnections {
    #[serde(default)]
    pub latest_reviews: Option<RawLatestReviewsConnection>,
    #[serde(default)]
    pub repository: Option<RawRepositoryName>,
    #[serde(default)]
    pub review_requests: Option<RawSearchReviewRequests>,
    #[serde(default)]
    pub labels: Option<RawSearchLabels>,
    /// One enum for the head commit, rather than the whole check array `gh pr list --json`.
    #[serde(default)]
    pub commits: Option<RawSearchCommits>,
}

/// A search's own answer, which is the listing's row one connection deeper.
#[derive(Debug, Clone, Deserialize)]
pub struct RawSearchItem {
    #[serde(flatten)]
    pub stack: RawStackMembership,
    #[serde(flatten)]
    pub common: RawListCommon,
    #[serde(flatten)]
    pub connections: RawSearchConnections,
}

/// A summary row: the search row with its counts nullable and `createdAt` optional.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawSummary {
    #[serde(flatten)]
    pub stack: RawStackMembership,
    pub number: Int,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub author: Option<RawActor>,
    pub head_ref_name: String,
    pub base_ref_name: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default, deserialize_with = "de::opt")]
    pub is_draft: Option<bool>,
    #[serde(default)]
    pub mergeable: Option<String>,
    #[serde(default)]
    pub review_decision: Option<String>,
    #[serde(flatten)]
    pub connections: RawSearchConnections,
    #[serde(default)]
    pub changed_files: Option<Int>,
    #[serde(default)]
    pub additions: Option<Int>,
    #[serde(default)]
    pub deletions: Option<Int>,
    #[serde(default)]
    pub closed_at: Option<String>,
    #[serde(default, deserialize_with = "de::opt")]
    pub created_at: Option<String>,
    pub updated_at: String,
    #[serde(default)]
    pub merged_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawPageInfoRequired {
    #[serde(rename = "hasNextPage")]
    pub has_next_page: bool,
    #[serde(rename = "endCursor", default)]
    pub end_cursor: Option<String>,
}

/// Where a connection carries on from, which is what every paged read follows.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawPageInfo {
    #[serde(default, deserialize_with = "de::opt")]
    pub has_next_page: Option<bool>,
    #[serde(default)]
    pub end_cursor: Option<String>,
}

/// A connection read row by row: a node that is not a pull request, or one field GitHub changes,
/// is skipped rather than blanking every repository at once.
#[derive(Debug, Clone, Deserialize)]
pub struct RawRowConnection {
    #[serde(rename = "pageInfo", default)]
    pub page_info: Option<RawPageInfoRequired>,
    #[serde(default)]
    pub nodes: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawSearchData {
    pub search: RawRowConnection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawSearch {
    pub data: RawSearchData,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawRepositoryPullRequests {
    pub pull_requests: RawRowConnection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawRepositoryPullRequestsData {
    #[serde(deserialize_with = "de::nullable")]
    pub repository: Option<RawRepositoryPullRequests>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawRepositoryPullRequestsEnvelope {
    pub data: RawRepositoryPullRequestsData,
}

// ---------------------------------------------------------------------------------------------
// Alias-keyed batches (stats, stack memberships, summaries, watch fingerprints)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawStatsCounts {
    #[serde(default)]
    pub additions: Option<Int>,
    #[serde(default)]
    pub deletions: Option<Int>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawStatsEntry {
    #[serde(default)]
    pub pull_request: Option<RawStatsCounts>,
}

/// One aliased lookup per row, so the response is keyed by the position it was asked in.
#[derive(Debug, Clone, Deserialize)]
pub struct RawStats {
    #[serde(default)]
    pub data: Option<IndexMap<String, Option<RawStatsEntry>>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawStackMembershipsEntry {
    #[serde(deserialize_with = "de::nullable")]
    pub pull_request: Option<RawStackMembership>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawStackMemberships {
    pub data: IndexMap<String, Option<RawStackMembershipsEntry>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawAliasedPullRequest {
    /// `Schema.Unknown`: decoded again, row by row, by the caller.
    #[serde(default)]
    pub pull_request: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawAliased {
    #[serde(default)]
    pub data: Option<IndexMap<String, Option<RawAliasedPullRequest>>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawEditedNode {
    #[serde(deserialize_with = "de::nullable")]
    pub last_edited_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawEdited {
    pub total_count: Int,
    pub nodes: Vec<Option<RawEditedNode>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawStateCount {
    pub state: String,
    pub count: Int,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawWatchContexts {
    #[serde(deserialize_with = "de::nullable")]
    pub check_run_counts_by_state: Option<Vec<RawStateCount>>,
    #[serde(deserialize_with = "de::nullable")]
    pub status_context_counts_by_state: Option<Vec<RawStateCount>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawWatchRollup {
    pub contexts: RawWatchContexts,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawWatchCommit {
    #[serde(deserialize_with = "de::nullable")]
    pub status_check_rollup: Option<RawWatchRollup>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawWatchCommitNode {
    pub commit: RawWatchCommit,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawWatchCommits {
    pub nodes: Vec<Option<RawWatchCommitNode>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawTotalCount {
    #[serde(rename = "totalCount")]
    pub total_count: Int,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawWatchFingerprintEntry {
    pub state: String,
    #[serde(deserialize_with = "de::nullable")]
    pub mergeable: Option<String>,
    pub head_ref_oid: String,
    pub comments: RawEdited,
    pub reviews: RawEdited,
    pub review_threads: RawTotalCount,
    pub commits: RawWatchCommits,
}

// ---------------------------------------------------------------------------------------------
// Detail / core
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct RawLogin {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawAutoMergeRequest {
    #[serde(default)]
    pub merge_method: Option<String>,
}

/// The fields a detail adds to a listing row.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawDetailExtras {
    /// GitHub's explicit distinction between a fork head and a branch in the base repository.
    #[serde(default, deserialize_with = "de::opt")]
    pub is_cross_repository: Option<bool>,
    /// Names the fork a pull request came from, which is what qualifies its head ref.
    #[serde(default)]
    pub head_repository_owner: Option<RawLogin>,
    /// The exact head revision, used to find workflow runs that GitHub has not started yet.
    #[serde(default)]
    pub head_ref_oid: Option<String>,
    #[serde(default, deserialize_with = "de::opt")]
    pub body: Option<String>,
    #[serde(default, deserialize_with = "de::opt")]
    pub changed_files: Option<Int>,
    #[serde(default)]
    pub closed_at: Option<String>,
    /// The standing instruction and strategy GitHub will use once its requirements are met.
    /// `None` absent, `Some(None)` null.
    #[serde(default, deserialize_with = "de::present")]
    pub auto_merge_request: Option<Option<RawAutoMergeRequest>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawDetail {
    #[serde(flatten)]
    pub item: RawListItem,
    #[serde(flatten)]
    pub extras: RawDetailExtras,
}

/// Requested together, so a response missing any of them fails rather than defaulting open:
/// guessing `true` would offer a merge method the repository forbids.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawRepositoryAccess {
    pub merge_commit_allowed: bool,
    pub squash_merge_allowed: bool,
    pub rebase_merge_allowed: bool,
    /// ADMIN, MAINTAIN, WRITE, TRIAGE, READ or NONE, or null for a GitHub App.
    /// `None` absent, `Some(None)` null: `to_can_write` tells the two apart.
    #[serde(default, deserialize_with = "de::present")]
    pub viewer_permission: Option<Option<String>>,
}

/// What GitHub says the viewer may do with a pull request.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawViewerFields {
    #[serde(default, deserialize_with = "de::opt")]
    pub viewer_can_update: Option<bool>,
    #[serde(default, deserialize_with = "de::opt")]
    pub viewer_did_author: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCompare {
    pub behind_by: Int,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawBaseRef {
    #[serde(deserialize_with = "de::nullable")]
    pub compare: Option<RawCompare>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCoreReviewRequest {
    #[serde(deserialize_with = "de::nullable")]
    pub requested_reviewer: Option<RawReviewRequest>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawCoreReviewRequests {
    pub nodes: Vec<RawCoreReviewRequest>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawCoreLabels {
    pub nodes: Vec<RawLabel>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawHasNextPage {
    pub has_next_page: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawCoreContexts {
    pub nodes: Vec<RawCheckContextNode>,
    #[serde(rename = "pageInfo")]
    pub page_info: RawHasNextPage,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawCoreRollup {
    pub contexts: RawCoreContexts,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCoreCommit {
    #[serde(deserialize_with = "de::nullable")]
    pub status_check_rollup: Option<RawCoreRollup>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawCoreCommitNode {
    pub commit: RawCoreCommit,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawCoreCommits {
    pub nodes: Vec<RawCoreCommitNode>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCorePullRequest {
    #[serde(flatten)]
    pub common: RawListCommon,
    // `RawListItem`'s own `latestReviews` / `statusCheckRollup` survive the spread; its
    // `reviewRequests` / `labels` are replaced by the required connections below.
    #[serde(default)]
    pub latest_reviews: Option<Vec<RawLatestReview>>,
    #[serde(default)]
    pub status_check_rollup: Option<Vec<RawCheck>>,
    #[serde(flatten)]
    pub extras: RawDetailExtras,
    #[serde(flatten)]
    pub viewer: RawViewerFields,
    pub viewer_can_update_branch: bool,
    #[serde(deserialize_with = "de::nullable")]
    pub base_ref: Option<RawBaseRef>,
    pub review_requests: RawCoreReviewRequests,
    pub labels: RawCoreLabels,
    pub commits: RawCoreCommits,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawCoreRepository {
    #[serde(flatten)]
    pub access: RawRepositoryAccess,
    #[serde(rename = "pullRequest")]
    pub pull_request: RawCorePullRequest,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawCoreData {
    pub repository: RawCoreRepository,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawCore {
    pub data: RawCoreData,
}

/// `GET /repos/{owner}/{repo}/actions/runs`, one page of it.
#[derive(Debug, Clone, Deserialize)]
pub struct RawWorkflowRunApproval {
    pub id: Int,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub html_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawWorkflowRuns {
    pub workflow_runs: Vec<RawWorkflowRunApproval>,
}

// ---------------------------------------------------------------------------------------------
// Pull request heads
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawHeadNode {
    pub number: Int,
    pub head_ref_oid: String,
    #[serde(default, deserialize_with = "de::opt")]
    pub is_cross_repository: Option<bool>,
    #[serde(default)]
    pub head_repository_owner: Option<RawLogin>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawHeadsConnection {
    #[serde(default, deserialize_with = "de::opt")]
    pub page_info: Option<RawPageInfo>,
    pub nodes: Vec<RawHeadNode>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawHeadsRepository {
    pub pull_requests: RawHeadsConnection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawHeadsData {
    #[serde(deserialize_with = "de::nullable")]
    pub repository: Option<RawHeadsRepository>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawPullRequestHeads {
    pub data: RawHeadsData,
}

// ---------------------------------------------------------------------------------------------
// Conversation: comments, reviews, reactions, threads
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawReactors {
    #[serde(default, deserialize_with = "de::opt")]
    pub total_count: Option<Int>,
    #[serde(default)]
    pub nodes: Option<Vec<Option<RawReactorNode>>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawReactorNode {
    #[serde(default)]
    pub login: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawReactionGroup {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default, deserialize_with = "de::opt")]
    pub viewer_has_reacted: Option<bool>,
    #[serde(default)]
    pub reactors: Option<RawReactors>,
}

/// `RawReactionGroupsSchema`: `optional(NullOr(Array(group)))`.
pub type RawReactionGroups = Option<Vec<RawReactionGroup>>;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawComment {
    pub id: String,
    #[serde(default)]
    pub last_edited_at: Option<String>,
    #[serde(default)]
    pub author: Option<RawActor>,
    #[serde(default, deserialize_with = "de::opt")]
    pub body: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub url: Option<String>,
    /// Only ever present on a GraphQL read; `gh pr view --json` reports no reaction at all.
    #[serde(default)]
    pub reaction_groups: RawReactionGroups,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawReview {
    pub id: String,
    #[serde(default)]
    pub last_edited_at: Option<String>,
    #[serde(default)]
    pub author: Option<RawActor>,
    #[serde(default, deserialize_with = "de::opt")]
    pub body: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub submitted_at: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawActivityConnection<T> {
    #[serde(rename = "pageInfo", default, deserialize_with = "de::opt")]
    pub page_info: Option<RawPageInfo>,
    pub nodes: Vec<T>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawUserLogin {
    #[serde(default)]
    pub login: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawActivityCommitAuthor {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub user: Option<RawUserLogin>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawActivityCommitAuthors {
    pub nodes: Vec<RawActivityCommitAuthor>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawActivityCommit {
    pub oid: String,
    #[serde(default, deserialize_with = "de::opt")]
    pub message_headline: Option<String>,
    pub committed_date: String,
    #[serde(default)]
    pub authors: Option<RawActivityCommitAuthors>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawActivityCommitNode {
    pub commit: RawActivityCommit,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawActivityCommits {
    pub nodes: Vec<RawActivityCommitNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawActivityPullRequest {
    /// `None` absent, `Some(None)` null: only the first page asks for the author.
    #[serde(default, deserialize_with = "de::present")]
    pub author: Option<Option<RawActor>>,
    #[serde(default)]
    pub comments: Option<RawActivityConnection<RawComment>>,
    #[serde(default)]
    pub reviews: Option<RawActivityConnection<RawReview>>,
    /// `None` absent, `Some(None)` null.
    #[serde(default, deserialize_with = "de::present")]
    pub commits: Option<Option<RawActivityCommits>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawActivityRepository {
    pub pull_request: RawActivityPullRequest,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawActivityData {
    pub repository: RawActivityRepository,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawActivity {
    pub data: RawActivityData,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawThreadComments {
    #[serde(default, deserialize_with = "de::opt")]
    pub total_count: Option<Int>,
    #[serde(default, deserialize_with = "de::opt")]
    pub page_info: Option<RawPageInfo>,
    pub nodes: Vec<RawComment>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawReviewThread {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default, deserialize_with = "de::opt")]
    pub is_resolved: Option<bool>,
    #[serde(default, deserialize_with = "de::opt")]
    pub is_outdated: Option<bool>,
    #[serde(default)]
    pub path: Option<String>,
    /// Null once the thread's line has left the diff, which `isOutdated` reports.
    #[serde(default)]
    pub line: Option<Int>,
    #[serde(default)]
    pub diff_side: Option<String>,
    pub comments: RawThreadComments,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawReviewThreads {
    #[serde(default, deserialize_with = "de::opt")]
    pub total_count: Option<Int>,
    #[serde(default, deserialize_with = "de::opt")]
    pub page_info: Option<RawPageInfo>,
    pub nodes: Vec<RawReviewThread>,
}

/// A comment or review for its reactions and actor identity alone.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawRemarkIdentity {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub author: Option<RawActor>,
    #[serde(default)]
    pub last_edited_at: Option<String>,
    #[serde(default)]
    pub reaction_groups: RawReactionGroups,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawRemarkIdentities {
    pub nodes: Vec<RawRemarkIdentity>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawThreadReviewRequest {
    /// Null where GitHub does not name who was asked.
    #[serde(default)]
    pub requested_reviewer: Option<RawRequestedReviewer>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawThreadReviewRequests {
    pub nodes: Vec<RawThreadReviewRequest>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawThreadLatestReviews {
    pub nodes: Vec<RawLatestReview>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawDismissalReview {
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawDismissal {
    #[serde(default)]
    pub dismissal_message: Option<String>,
    #[serde(default)]
    pub review: Option<RawDismissalReview>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawReviewDismissals {
    #[serde(default, deserialize_with = "de::opt")]
    pub page_info: Option<RawPageInfo>,
    pub nodes: Vec<RawDismissal>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawParents {
    #[serde(rename = "totalCount", default, deserialize_with = "de::opt")]
    pub total_count: Option<Int>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawThreadCommitAuthor {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub user: Option<RawUserLogin>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawThreadCommitAuthors {
    pub nodes: Vec<RawThreadCommitAuthor>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawThreadCommit {
    pub oid: String,
    #[serde(default)]
    pub message_headline: Option<String>,
    #[serde(default)]
    pub committed_date: Option<String>,
    #[serde(default, deserialize_with = "de::opt")]
    pub additions: Option<Int>,
    #[serde(default, deserialize_with = "de::opt")]
    pub deletions: Option<Int>,
    #[serde(default)]
    pub parents: Option<RawParents>,
    #[serde(default)]
    pub authors: Option<RawThreadCommitAuthors>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawThreadCommitNode {
    pub commit: RawThreadCommit,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawThreadCommits {
    pub nodes: Vec<RawThreadCommitNode>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawReviewThreadsPullRequest {
    pub review_threads: RawReviewThreads,
    #[serde(flatten)]
    pub viewer: RawViewerFields,
    #[serde(default)]
    pub author: Option<RawActor>,
    #[serde(default)]
    pub reaction_groups: RawReactionGroups,
    #[serde(default)]
    pub comments: Option<RawRemarkIdentities>,
    /// Reviews for their reactions and actor identity: the words and the verdict arrive with
    /// `gh pr view --json reviews`, which reports no reaction of any kind.
    #[serde(default)]
    pub reviews: Option<RawRemarkIdentities>,
    #[serde(default)]
    pub review_requests: Option<RawThreadReviewRequests>,
    #[serde(default)]
    pub latest_reviews: Option<RawThreadLatestReviews>,
    #[serde(default)]
    pub review_dismissals: Option<RawReviewDismissals>,
    #[serde(default)]
    pub commits: Option<RawThreadCommits>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawReviewThreadsRepository {
    pub pull_request: RawReviewThreadsPullRequest,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawViewerLogin {
    #[serde(default)]
    pub login: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawReviewThreadsData {
    /// Rides along in the same request: GitHub names who reacted but never says whether that is
    /// the reader, so the comparison is made here rather than paid for with a request of its own.
    #[serde(default)]
    pub viewer: Option<RawViewerLogin>,
    pub repository: RawReviewThreadsRepository,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawReviewThreadsEnvelope {
    pub data: RawReviewThreadsData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawIdOnly {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawIdRepositoryPullRequest {
    #[serde(deserialize_with = "de::nullable")]
    pub pull_request: Option<RawIdOnly>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawThreadCommentsNode {
    #[serde(default, deserialize_with = "de::opt")]
    pub pull_request: Option<RawIdOnly>,
    #[serde(default, deserialize_with = "de::opt")]
    pub comments: Option<RawThreadComments>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawReviewThreadCommentsData {
    #[serde(default)]
    pub viewer: Option<RawViewerLogin>,
    #[serde(deserialize_with = "de::nullable")]
    pub repository: Option<RawIdRepositoryPullRequest>,
    /// Null for an id that names nothing the viewer can read, which is not a thread to page.
    #[serde(deserialize_with = "de::nullable")]
    pub node: Option<RawThreadCommentsNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawReviewThreadComments {
    pub data: RawReviewThreadCommentsData,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawNodeIdRepository {
    pub pull_request: RawIdOnly,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawNodeIdData {
    pub repository: RawNodeIdRepository,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawPullRequestNodeId {
    pub data: RawNodeIdData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawReactionSubjectNode {
    pub id: String,
    #[serde(rename = "pullRequest", default, deserialize_with = "de::opt")]
    pub pull_request: Option<RawIdOnly>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawReactionSubjectScopeData {
    #[serde(deserialize_with = "de::nullable")]
    pub repository: Option<RawIdRepositoryPullRequest>,
    #[serde(deserialize_with = "de::nullable")]
    pub node: Option<RawReactionSubjectNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawReactionSubjectScope {
    pub data: RawReactionSubjectScopeData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawDismissalsTimeline {
    #[serde(rename = "pageInfo", default, deserialize_with = "de::opt")]
    pub page_info: Option<RawPageInfo>,
    pub nodes: Vec<RawDismissal>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawDismissalsPullRequest {
    pub timeline_items: RawDismissalsTimeline,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawDismissalsRepository {
    pub pull_request: RawDismissalsPullRequest,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawDismissalsData {
    pub repository: RawDismissalsRepository,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawReviewDismissalsEnvelope {
    pub data: RawDismissalsData,
}

// ---------------------------------------------------------------------------------------------
// Check contexts, avatars, preview
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct RawCheckContexts {
    pub nodes: Vec<RawCheckContextNode>,
    #[serde(rename = "pageInfo")]
    pub page_info: RawPageInfo,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCheckContextsRollup {
    pub contexts: RawCheckContexts,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCheckContextsCommit {
    #[serde(deserialize_with = "de::nullable")]
    pub status_check_rollup: Option<RawCheckContextsRollup>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawCheckContextsCommitNode {
    pub commit: RawCheckContextsCommit,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawCheckContextsCommits {
    pub nodes: Vec<RawCheckContextsCommitNode>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCheckContextsPullRequest {
    pub head_ref_oid: String,
    pub commits: RawCheckContextsCommits,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCheckContextsRepository {
    pub pull_request: RawCheckContextsPullRequest,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawCheckContextsData {
    pub repository: RawCheckContextsRepository,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawCheckContextsEnvelope {
    pub data: RawCheckContextsData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawActorAvatarsData {
    pub nodes: Vec<Option<RawActor>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawActorAvatars {
    pub data: RawActorAvatarsData,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawPreviewPullRequest {
    pub number: Int,
    pub title: String,
    pub url: String,
    pub state: String,
    pub is_draft: bool,
    pub created_at: String,
    #[serde(deserialize_with = "de::nullable")]
    pub author: Option<RawActor>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawPreviewRepository {
    pub pull_request: RawPreviewPullRequest,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawPreviewData {
    pub repository: RawPreviewRepository,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawPreview {
    pub data: RawPreviewData,
}

// ---------------------------------------------------------------------------------------------
// Reviewers, labels, permissions
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawAssignableUsers {
    #[serde(default, deserialize_with = "de::opt")]
    pub page_info: Option<RawPageInfo>,
    pub nodes: Vec<Option<RawActor>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCandidatesPullRequest {
    #[serde(default)]
    pub author: Option<RawActor>,
    #[serde(default)]
    pub review_requests: Option<RawThreadReviewRequests>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawReviewerCandidatesRepository {
    pub assignable_users: RawAssignableUsers,
    /// Null for a number that names no pull request the viewer can see.
    #[serde(deserialize_with = "de::nullable")]
    pub pull_request: Option<RawCandidatesPullRequest>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawReviewerCandidatesData {
    pub repository: RawReviewerCandidatesRepository,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawReviewerCandidates {
    pub data: RawReviewerCandidatesData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawLabelWithDescription {
    #[serde(flatten)]
    pub label: RawLabel,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawRepositoryLabels {
    #[serde(default, deserialize_with = "de::opt")]
    pub page_info: Option<RawPageInfo>,
    pub nodes: Vec<Option<RawLabelWithDescription>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawAppliedLabelNodes {
    pub nodes: Vec<Option<RawLabel>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawLabelsPullRequest {
    #[serde(default)]
    pub labels: Option<RawAppliedLabelNodes>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawLabelCandidatesRepository {
    #[serde(default)]
    pub labels: Option<RawRepositoryLabels>,
    /// Null for a number that names no pull request the viewer can see.
    #[serde(deserialize_with = "de::nullable")]
    pub pull_request: Option<RawLabelsPullRequest>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawLabelCandidatesData {
    pub repository: RawLabelCandidatesRepository,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawLabelCandidates {
    pub data: RawLabelCandidatesData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawViewerPermissionsRepository {
    #[serde(flatten)]
    pub access: RawRepositoryAccess,
    /// Null for a number that names no pull request the viewer can see.
    #[serde(rename = "pullRequest", deserialize_with = "de::nullable")]
    pub pull_request: Option<RawViewerFields>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawViewerPermissionsData {
    pub repository: RawViewerPermissionsRepository,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawViewerPermissions {
    pub data: RawViewerPermissionsData,
}

// ---------------------------------------------------------------------------------------------
// Files, viewed state, stacks
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct RawPullRequestFile {
    pub filename: String,
    #[serde(default)]
    pub status: Option<String>,
    /// Only on a rename, where it names the file the hunks are counted against.
    #[serde(default)]
    pub previous_filename: Option<String>,
    /// Absent for a binary file, and for one whose diff GitHub considers too large.
    #[serde(default)]
    pub patch: Option<String>,
    #[serde(default)]
    pub additions: Option<Int>,
    #[serde(default)]
    pub deletions: Option<Int>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawCommitFiles {
    #[serde(default)]
    pub files: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawViewedFile {
    pub path: String,
    /// Decoded as a plain string and narrowed later: a GitHub release that adds a fourth state
    /// must not fail the whole page.
    #[serde(rename = "viewerViewedState")]
    pub viewer_viewed_state: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawViewedFiles {
    #[serde(rename = "pageInfo")]
    pub page_info: RawViewedPageInfo,
    #[serde(deserialize_with = "de::nullable")]
    pub nodes: Option<Vec<Option<RawViewedFile>>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawViewedPageInfo {
    #[serde(rename = "hasNextPage")]
    pub has_next_page: bool,
    #[serde(rename = "endCursor", deserialize_with = "de::nullable")]
    pub end_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawViewedPullRequest {
    pub files: RawViewedFiles,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawViewedRepository {
    #[serde(rename = "pullRequest", deserialize_with = "de::nullable")]
    pub pull_request: Option<RawViewedPullRequest>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawViewedData {
    #[serde(deserialize_with = "de::nullable")]
    pub repository: Option<RawViewedRepository>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawPullRequestFilesViewed {
    pub data: RawViewedData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawStackHead {
    #[serde(rename = "ref")]
    pub ref_name: String,
    #[serde(default, deserialize_with = "de::opt")]
    pub sha: Option<String>,
}

/// One pull request as the stacks API lists it: a number, a head, and whether it is done.
#[derive(Debug, Clone, Deserialize)]
pub struct RawStackPullRequest {
    #[serde(default, deserialize_with = "de::opt")]
    pub title: Option<String>,
    #[serde(default, deserialize_with = "de::opt")]
    pub draft: Option<bool>,
    pub number: Int,
    pub head: RawStackHead,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub merged_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RawStackId {
    Number(Int),
    Text(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawStackBaseRef {
    #[serde(rename = "ref")]
    pub ref_name: String,
}

/// `base` is accepted both as the ref object the preview sends today and as the bare branch name
/// it started out as.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RawStackBase {
    Name(String),
    Ref(RawStackBaseRef),
}

/// A stack as `GET /repos/{owner}/{repo}/stacks` answers it, in a public preview whose shape may
/// still move.
#[derive(Debug, Clone, Deserialize)]
pub struct RawStackListing {
    #[serde(default)]
    pub id: Option<RawStackId>,
    pub number: Int,
    #[serde(default)]
    pub node_id: Option<String>,
    pub url: String,
    #[serde(default)]
    pub html_url: Option<String>,
    pub base: RawStackBase,
    pub pull_requests: Vec<RawStackPullRequest>,
}
