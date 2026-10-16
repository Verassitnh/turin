//! Listing rows, details, summaries, watch fingerprints and the aliased batch documents.

use indexmap::IndexMap;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use turin_contracts::pull_request::PullRequestStackMembership;

use super::convert::{
    ListInput, ReviewRequestRef, count, rollup_checks_state, to_actor, to_can_triage, to_can_write,
    to_detail, to_list_item, to_mergeability, to_pull_request_viewer_fields, to_requested_team,
    to_review_decision_with_reviews, to_stack_membership, to_state, trimmed, trimmed_ref,
};
use super::de::alias_index;
use super::queries::{
    PULL_REQUEST_SUMMARY_SELECTION, STACK_MEMBERSHIP_SELECTION,
    pull_request_watch_fingerprint_selection,
};
use super::raw::{
    RawAliased, RawCheck, RawCheckContextNode, RawCore, RawDetail, RawLabel, RawLatestReview,
    RawRepositoryPullRequestsEnvelope, RawSearch, RawSearchItem, RawStackMemberships, RawStats,
    RawSummary, RawWatchFingerprintEntry,
};
use super::types::{
    DecodeFailure, DecodeResult, GitHubBaseComparison, GitHubDiffCounts, GitHubPullRequestCore,
    GitHubPullRequestDetail, GitHubPullRequestListBatch, GitHubPullRequestListItem,
    GitHubPullRequestSearchBatch, GitHubPullRequestSearchItem, GitHubPullRequestSummary,
    GitHubPullRequestWatchFingerprint, GitHubViewerAccess, GitHubViewerRepositoryAccess,
};
use crate::github::graphql::{
    AliasedGraphQlDocumentInput, GraphQlDocument, GraphQlOperation, GraphQlPlaceholders,
    GraphQlVariable, GraphQlVariables, aliased_graphql_document,
};
use crate::shared::js::js_trim;
use turin_contracts::pull_request::PullRequestMergeCapabilities;

/// `decodeJsonResult(schema)(raw)`: `JSON.parse`, then the schema.
pub(super) fn decode_json<T: DeserializeOwned>(raw: &str) -> DecodeResult<T> {
    serde_json::from_str(raw).map_err(|error| DecodeFailure::new(error.to_string()))
}

// ---------------------------------------------------------------------------------------------
// Rows (repository list and search)
// ---------------------------------------------------------------------------------------------

struct Row {
    item: GitHubPullRequestListItem,
    repository: Option<String>,
}

/// A row as GraphQL answers it: reviewers and labels arrive as connections, and the checks as
/// GitHub's one-word rollup rather than the whole check list. `None` for a node that is not a
/// pull request, or one whose fields no longer decode.
fn to_row(entry: &Value) -> Option<Row> {
    let node = RawSearchItem::deserialize(entry).ok()?;
    let stack = to_stack_membership(&node.stack);
    let connections = &node.connections;
    let review_requests: Vec<ReviewRequestRef<'_>> = connections
        .review_requests
        .as_ref()
        .and_then(|requests| requests.nodes.as_deref())
        .unwrap_or_default()
        .iter()
        .filter_map(|request| {
            let reviewer = request.as_ref()?.requested_reviewer.as_ref();
            if let Some(login) = trimmed_ref(reviewer.and_then(|r| r.actor.login.as_deref())) {
                return Some(ReviewRequestRef {
                    login: Some(login),
                    ..Default::default()
                });
            }
            let slug = trimmed_ref(reviewer.and_then(|r| r.slug.as_deref()))?;
            Some(ReviewRequestRef {
                slug: Some(slug),
                ..Default::default()
            })
        })
        .collect();
    let latest_review_states: Vec<Option<&str>> = connections
        .latest_reviews
        .as_ref()
        .map(|reviews| reviews.nodes.as_slice())
        .unwrap_or_default()
        .iter()
        .flatten()
        .map(|review| review.state.as_deref())
        .collect();
    let labels: Vec<&RawLabel> = connections
        .labels
        .as_ref()
        .and_then(|labels| labels.nodes.as_deref())
        .unwrap_or_default()
        .iter()
        .flatten()
        .collect();
    let rollup = rollup_as_checks(
        connections
            .commits
            .as_ref()
            .and_then(|commits| commits.nodes.as_deref())
            .unwrap_or_default()
            .iter()
            .map(|commit_node| {
                commit_node
                    .as_ref()
                    .and_then(|node| node.commit.as_ref())
                    .and_then(|commit| commit.status_check_rollup.as_ref())
                    .map(|rollup| rollup.state.as_str())
            }),
    );
    let mut item = to_list_item(&ListInput {
        common: &node.common,
        latest_review_states,
        review_requests,
        labels,
        // The rollup arrives as one enum. Dressed as a single check here so it is read the same
        // way the detail's checks are.
        status_check_rollup: Some(&rollup),
    });
    item.stack = stack;
    Some(Row {
        item,
        repository: trimmed(
            connections
                .repository
                .as_ref()
                .map(|repository| repository.name_with_owner.as_str()),
        ),
    })
}

/// GitHub's one-word rollup enum, dressed as a nameless check each.
fn rollup_as_checks<'a>(states: impl Iterator<Item = Option<&'a str>>) -> Vec<RawCheck> {
    states
        .filter_map(|state| trimmed(state))
        .map(|state| RawCheck {
            state: Some(state),
            ..Default::default()
        })
        .collect()
}

/// One page of a repository's own pull request list. Malformed rows are skipped rather than
/// failing the page: one unexpected pull request must not blank the whole list. A repository the
/// viewer cannot see answers as an empty page, which is what `gh pr list` printed for it too.
pub fn decode_pull_request_list_json(raw: &str) -> DecodeResult<GitHubPullRequestListBatch> {
    let decoded: RawRepositoryPullRequestsEnvelope = decode_json(raw)?;
    let connection = decoded
        .data
        .repository
        .map(|repository| repository.pull_requests);
    let nodes = connection
        .as_ref()
        .and_then(|connection| connection.nodes.as_deref())
        .unwrap_or_default();
    let items = nodes
        .iter()
        .filter_map(to_row)
        .map(|row| row.item)
        .collect();
    let end_cursor = connection
        .as_ref()
        .and_then(|connection| connection.page_info.as_ref())
        .filter(|page_info| page_info.has_next_page)
        .and_then(|page_info| trimmed(page_info.end_cursor.as_deref()));
    Ok(GitHubPullRequestListBatch {
        items,
        raw_count: nodes.len(),
        end_cursor,
    })
}

/// A search answers with the same row a repository's list does, each naming its repository.
///
/// Rows that are not pull requests decode as empty and are skipped, the way a malformed listing
/// row is: `is:pr` already excludes them, and one surprise must not blank a whole host.
pub fn decode_pull_request_search_json(raw: &str) -> DecodeResult<GitHubPullRequestSearchBatch> {
    let decoded: RawSearch = decode_json(raw)?;
    let search = decoded.data.search;
    let nodes = search.nodes.as_deref().unwrap_or_default();
    let items = nodes
        .iter()
        .filter_map(to_row)
        .filter_map(|row| {
            Some(GitHubPullRequestSearchItem {
                repository: row.repository?,
                base: row.item,
            })
        })
        .collect();
    let has_next_page = search
        .page_info
        .as_ref()
        .is_some_and(|page_info| page_info.has_next_page);
    Ok(GitHubPullRequestSearchBatch {
        items,
        raw_count: nodes.len(),
        has_next_page,
        end_cursor: search
            .page_info
            .as_ref()
            .filter(|page_info| page_info.has_next_page)
            .and_then(|page_info| trimmed(page_info.end_cursor.as_deref())),
    })
}

// ---------------------------------------------------------------------------------------------
// Aliased batch documents
// ---------------------------------------------------------------------------------------------

/// A change request addressed by repository and number, as the batch builders take them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRequestSelector {
    pub repository: String,
    pub number: u64,
}

impl From<(String, u64)> for ChangeRequestSelector {
    fn from((repository, number): (String, u64)) -> Self {
        Self { repository, number }
    }
}

/// What GitHub allows in an owner or repository name.
fn is_repository_part(part: &str) -> bool {
    !part.is_empty()
        && part
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// The owner and name of an `owner/name` selector, or `None` for one GitHub cannot name. A batch
/// is one document, and a single unanswerable alias fails all of it, so these are refused up front.
fn repository_parts(repository: &str) -> Option<(String, String)> {
    let mut parts = js_trim(repository).split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    (is_repository_part(owner) && is_repository_part(name))
        .then(|| (owner.to_owned(), name.to_owned()))
}

/// `Number.isSafeInteger(number) && number > 0`.
fn is_pull_request_number(number: u64) -> bool {
    number > 0 && number <= 9_007_199_254_740_991
}

/// One aliased `repository { pullRequest { selection } }` per change request, on one host.
fn aliased_pull_requests_document(
    name: &str,
    alias: &str,
    change_requests: &[ChangeRequestSelector],
    selection: &str,
) -> Option<GraphQlDocument> {
    let addressed: Vec<((String, String), u64)> = change_requests
        .iter()
        .map(|change_request| {
            Some((
                repository_parts(&change_request.repository)?,
                change_request.number,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    if addressed
        .iter()
        .any(|(_, number)| !is_pull_request_number(*number))
    {
        return None;
    }
    let variables = |item: &((String, String), u64)| -> GraphQlVariables {
        let ((owner, name), number) = item;
        vec![
            (
                "owner".to_owned(),
                GraphQlVariable::new("String!", owner.as_str()),
            ),
            (
                "name".to_owned(),
                GraphQlVariable::new("String!", name.as_str()),
            ),
            ("number".to_owned(), GraphQlVariable::new("Int!", *number)),
        ]
    };
    let field = |placeholders: &GraphQlPlaceholders, _item: &((String, String), u64)| -> String {
        format!(
            "repository(owner: {}, name: {}) {{ pullRequest(number: {}) {{ {selection} }} }}",
            placeholders["owner"], placeholders["name"], placeholders["number"]
        )
    };
    aliased_graphql_document(AliasedGraphQlDocumentInput {
        operation: GraphQlOperation::Query,
        name: Some(name),
        alias,
        key: None,
        items: &addressed,
        variables: &variables,
        field: &field,
        shared: Vec::new(),
        within: None,
    })
}

/// The line counts for rows a listing already handed over, as one aliased lookup each.
///
/// Aliases rather than `nodes(ids:)` because the caller asks in the terms the page holds (a
/// repository and a number) and never sees a node id. `None` for a selector GitHub cannot name,
/// which the caller reports rather than sends, and for an empty request.
pub fn build_pull_request_stats_graphql_query(
    change_requests: &[ChangeRequestSelector],
) -> Option<GraphQlDocument> {
    aliased_pull_requests_document(
        "PullRequestStats",
        "s",
        change_requests,
        "additions deletions",
    )
}

/// Stack membership for the visible rows of a per-repository listing.
pub fn build_pull_request_stack_memberships_graphql_query(
    repository: &str,
    numbers: &[u64],
) -> Option<GraphQlDocument> {
    let change_requests: Vec<ChangeRequestSelector> = numbers
        .iter()
        .map(|number| ChangeRequestSelector {
            repository: repository.to_owned(),
            number: *number,
        })
        .collect();
    aliased_pull_requests_document(
        "PullRequestStackMemberships",
        "s",
        &change_requests,
        STACK_MEMBERSHIP_SELECTION,
    )
}

/// Summaries for pull requests anywhere on one host, one aliased lookup each; `None` when unsafe.
pub fn build_pull_request_summaries_graphql_query(
    change_requests: &[ChangeRequestSelector],
    include_stacks: bool,
) -> Option<GraphQlDocument> {
    let selection = if include_stacks {
        format!("{PULL_REQUEST_SUMMARY_SELECTION} {STACK_MEMBERSHIP_SELECTION}")
    } else {
        PULL_REQUEST_SUMMARY_SELECTION.to_owned()
    };
    aliased_pull_requests_document("PullRequestSummaries", "s", change_requests, &selection)
}

/// Watch fingerprints for pull requests on one host, one aliased lookup each; `None` when unsafe.
pub fn build_pull_request_watch_fingerprints_graphql_query(
    change_requests: &[ChangeRequestSelector],
) -> Option<GraphQlDocument> {
    aliased_pull_requests_document(
        "PullRequestWatchFingerprints",
        "w",
        change_requests,
        &pull_request_watch_fingerprint_selection(),
    )
}

// ---------------------------------------------------------------------------------------------
// Alias-keyed decoders
// ---------------------------------------------------------------------------------------------

pub fn decode_pull_request_stack_memberships_json(
    raw: &str,
) -> DecodeResult<IndexMap<u64, PullRequestStackMembership>> {
    let decoded: RawStackMemberships = decode_json(raw)?;
    let mut memberships = IndexMap::new();
    for (alias, value) in &decoded.data {
        let Some(index) = alias_index(alias, 's') else {
            continue;
        };
        let Some(pull_request) = value.as_ref().and_then(|value| value.pull_request.as_ref())
        else {
            continue;
        };
        if let Some(stack) = to_stack_membership(pull_request) {
            memberships.insert(index, stack);
        }
    }
    Ok(memberships)
}

/// The counts by the position they were asked in. A repository or a pull request GitHub answered
/// nothing for is simply absent, which leaves the row with whatever it already had.
pub fn decode_pull_request_stats_json(raw: &str) -> DecodeResult<IndexMap<u64, GitHubDiffCounts>> {
    let decoded: RawStats = decode_json(raw)?;
    let mut stats = IndexMap::new();
    for (alias, value) in decoded.data.iter().flatten() {
        let Some(index) = alias_index(alias, 's') else {
            continue;
        };
        let Some(pull_request) = value.as_ref().and_then(|value| value.pull_request.as_ref())
        else {
            continue;
        };
        stats.insert(
            index,
            GitHubDiffCounts {
                additions: pull_request
                    .additions
                    .map_or(0, |value| value.non_negative()),
                deletions: pull_request
                    .deletions
                    .map_or(0, |value| value.non_negative()),
            },
        );
    }
    Ok(stats)
}

/// Summaries by the position they were asked in. A pull request GitHub answered nothing for, or
/// one whose fields no longer decode, is absent rather than failing the rest of the batch.
pub fn decode_pull_request_summaries_json(
    raw: &str,
) -> DecodeResult<IndexMap<u64, GitHubPullRequestSummary>> {
    let decoded: RawAliased = decode_json(raw)?;
    let mut summaries = IndexMap::new();
    for (alias, value) in decoded.data.iter().flatten() {
        let Some(index) = alias_index(alias, 's') else {
            continue;
        };
        let Some(pull_request) = value
            .as_ref()
            .and_then(|value| value.pull_request.as_ref())
            .filter(|value| !value.is_null())
        else {
            continue;
        };
        let Ok(pr) = RawSummary::deserialize(pull_request) else {
            continue;
        };
        let latest_review_states: Vec<Option<&str>> = pr
            .connections
            .latest_reviews
            .as_ref()
            .map(|reviews| reviews.nodes.as_slice())
            .unwrap_or_default()
            .iter()
            .flatten()
            .map(|review| review.state.as_deref())
            .collect();
        // One enum for the head commit, dressed as a single check like the search row's.
        let rollup = rollup_as_checks(
            pr.connections
                .commits
                .as_ref()
                .and_then(|commits| commits.nodes.as_deref())
                .unwrap_or_default()
                .iter()
                .map(|commit_node| {
                    commit_node
                        .as_ref()
                        .and_then(|node| node.commit.as_ref())
                        .and_then(|commit| commit.status_check_rollup.as_ref())
                        .map(|rollup| rollup.state.as_str())
                }),
        );
        summaries.insert(
            index,
            GitHubPullRequestSummary {
                number: pr.number.non_negative(),
                title: pr.title.clone(),
                url: pr.url.clone(),
                head_branch: pr.head_ref_name.clone(),
                base_branch: pr.base_ref_name.clone(),
                state: to_state(pr.state.as_deref(), pr.merged_at.as_deref()),
                is_draft: pr.is_draft.unwrap_or(false),
                closed_at: trimmed(pr.closed_at.as_deref()),
                merged_at: trimmed(pr.merged_at.as_deref()),
                updated_at: pr.updated_at.clone(),
                author: to_actor(pr.author.as_ref()),
                additions: pr.additions.map_or(0, |value| value.non_negative()),
                deletions: pr.deletions.map_or(0, |value| value.non_negative()),
                changed_files: pr.changed_files.map_or(0, |value| value.non_negative()),
                review_decision: to_review_decision_with_reviews(
                    pr.review_decision.as_deref(),
                    &latest_review_states,
                ),
                checks_state: rollup_checks_state(Some(&rollup)),
                mergeability: to_mergeability(pr.mergeable.as_deref()),
                // `Some(None)` where GitHub says the pull request is in no stack; absent when the
                // read did not ask.
                stack: pr
                    .stack
                    .stack
                    .as_ref()
                    .map(|_| to_stack_membership(&pr.stack)),
            },
        );
    }
    Ok(summaries)
}

// An edit only ever moves `lastEditedAt` forward, so the newest one stands for them all.
fn newest_edit(connection: &super::raw::RawEdited) -> &str {
    connection
        .nodes
        .iter()
        .flatten()
        .filter_map(|node| node.last_edited_at.as_deref())
        .fold("", |newest, edited_at| {
            if edited_at > newest {
                edited_at
            } else {
                newest
            }
        })
}

fn state_counts(counts: Option<&[super::raw::RawStateCount]>) -> String {
    let mut entries: Vec<String> = counts
        .unwrap_or_default()
        .iter()
        .filter(|count| count.count.0 > 0)
        .map(|count| format!("{}:{}", count.state, count.count.0))
        .collect();
    entries.sort();
    entries.join(",")
}

/// Fingerprints by alias index; a pull request GitHub returned nothing for is left out.
pub fn decode_pull_request_watch_fingerprints_json(
    raw: &str,
) -> DecodeResult<IndexMap<u64, GitHubPullRequestWatchFingerprint>> {
    let decoded: RawAliased = decode_json(raw)?;
    let mut fingerprints = IndexMap::new();
    for (alias, value) in decoded.data.iter().flatten() {
        let Some(index) = alias_index(alias, 'w') else {
            continue;
        };
        let Some(pull_request) = value
            .as_ref()
            .and_then(|value| value.pull_request.as_ref())
            .filter(|value| !value.is_null())
        else {
            continue;
        };
        let Ok(pr) = RawWatchFingerprintEntry::deserialize(pull_request) else {
            continue;
        };
        let contexts = pr
            .commits
            .nodes
            .first()
            .and_then(|node| node.as_ref())
            .and_then(|node| node.commit.status_check_rollup.as_ref())
            .map(|rollup| &rollup.contexts);
        fingerprints.insert(
            index,
            GitHubPullRequestWatchFingerprint {
                status: [
                    pr.state.clone(),
                    pr.mergeable.clone().unwrap_or_default(),
                    pr.head_ref_oid.clone(),
                    state_counts(contexts.and_then(|c| c.check_run_counts_by_state.as_deref())),
                    state_counts(
                        contexts.and_then(|c| c.status_context_counts_by_state.as_deref()),
                    ),
                ]
                .join(" "),
                remarks: [
                    pr.comments.total_count.0.to_string(),
                    newest_edit(&pr.comments).to_owned(),
                    pr.reviews.total_count.0.to_string(),
                    newest_edit(&pr.reviews).to_owned(),
                    pr.review_threads.total_count.0.to_string(),
                ]
                .join(" "),
            },
        );
    }
    Ok(fingerprints)
}

// ---------------------------------------------------------------------------------------------
// Detail and core
// ---------------------------------------------------------------------------------------------

/// A node of the rollup with the workflow that owns it, as the plain check the normalizers read.
pub(super) fn to_check_contexts(nodes: &[RawCheckContextNode]) -> Vec<RawCheck> {
    nodes
        .iter()
        .map(|node| {
            let mut check = node.check.clone();
            check.workflow_name = node
                .check_suite
                .as_ref()
                .and_then(|suite| suite.workflow_run.as_ref())
                .and_then(|run| run.workflow.as_ref())
                .map(|workflow| workflow.name.clone());
            check
        })
        .collect()
}

pub fn decode_pull_request_core_json(raw: &str) -> DecodeResult<GitHubPullRequestCore> {
    let decoded: RawCore = decode_json(raw)?;
    let repository = &decoded.data.repository;
    let pr = &repository.pull_request;
    let contexts = pr
        .commits
        .nodes
        .first()
        .and_then(|node| node.commit.status_check_rollup.as_ref())
        .map(|rollup| &rollup.contexts);
    let requested: Vec<_> = pr
        .review_requests
        .nodes
        .iter()
        .filter_map(|node| node.requested_reviewer.as_ref())
        .collect();
    let review_request_teams = requested
        .iter()
        .filter_map(|reviewer| {
            to_requested_team(
                reviewer.combined_slug.as_deref(),
                reviewer.name.as_deref(),
                reviewer.avatar_url.as_deref(),
            )
        })
        .collect();
    let rollup = contexts
        .map(|contexts| to_check_contexts(&contexts.nodes))
        .unwrap_or_default();
    let latest_review_states: Vec<Option<&str>> = pr
        .latest_reviews
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|review| review.state.as_deref())
        .collect();
    let list_input = ListInput {
        common: &pr.common,
        latest_review_states,
        review_requests: requested
            .iter()
            .map(|reviewer| ReviewRequestRef {
                login: reviewer.login.as_deref(),
                slug: reviewer.slug.as_deref(),
                name: reviewer.name.as_deref(),
            })
            .collect(),
        labels: pr.labels.nodes.iter().collect(),
        status_check_rollup: Some(&rollup),
    };
    let detail = to_detail(&list_input, &pr.extras);
    let viewer = to_pull_request_viewer_fields(Some(&pr.viewer));
    let viewer_permission = repository
        .access
        .viewer_permission
        .as_ref()
        .map(|permission| permission.as_deref());
    let comparison = match pr
        .base_ref
        .as_ref()
        .and_then(|base_ref| base_ref.compare.as_ref())
    {
        // `pr.state` is GitHub's own word here, compared as written.
        Some(compare) if pr.common.state.as_deref() == Some("OPEN") => Some(GitHubBaseComparison {
            behind_by: Some(count(compare.behind_by.0)),
            viewer_can_update: pr.viewer_can_update_branch,
        }),
        _ => None,
    };
    Ok(GitHubPullRequestCore {
        base: detail,
        review_request_teams,
        viewer_access: GitHubViewerRepositoryAccess {
            access: GitHubViewerAccess {
                can_write: to_can_write(viewer_permission),
                can_triage: to_can_triage(viewer_permission),
                can_update: viewer.can_update,
                did_author: viewer.did_author,
                can_update_branch: None,
            },
            merge_capabilities: PullRequestMergeCapabilities {
                merge: repository.access.merge_commit_allowed,
                squash: repository.access.squash_merge_allowed,
                rebase: repository.access.rebase_merge_allowed,
            },
        },
        comparison,
        checks_truncated: contexts.is_some_and(|contexts| contexts.page_info.has_next_page),
    })
}

pub fn decode_pull_request_detail_json(raw: &str) -> DecodeResult<GitHubPullRequestDetail> {
    let decoded: RawDetail = decode_json(raw)?;
    let item = &decoded.item;
    let latest_reviews: &[RawLatestReview] = item.latest_reviews.as_deref().unwrap_or_default();
    let list_input = ListInput {
        common: &item.common,
        latest_review_states: latest_reviews
            .iter()
            .map(|review| review.state.as_deref())
            .collect(),
        review_requests: item
            .review_requests
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|request| ReviewRequestRef {
                login: request.login.as_deref(),
                slug: request.slug.as_deref(),
                name: request.name.as_deref(),
            })
            .collect(),
        labels: item.labels.as_deref().unwrap_or_default().iter().collect(),
        status_check_rollup: item.status_check_rollup.as_deref(),
    };
    Ok(to_detail(&list_input, &decoded.extras))
}
