//! Listing, searching and stats (`listPullRequests`, `searchPullRequests`, `listPullRequestStats`).

use std::sync::LazyLock;

use futures::stream::{self, StreamExt, TryStreamExt};
use regex::Regex;
use turin_contracts::pull_request::{
    PullRequestChecksFilter, PullRequestChecksState, PullRequestDraftFilter,
    PullRequestInvolvement, PullRequestListFilters, PullRequestListState,
    PullRequestReviewDecision, PullRequestReviewFilter, PullRequestState,
    resolve_pull_request_author_filter,
};

use super::errors::{GitHubPullRequestApiError, GitHubRepositorySelectorError};
use super::{
    GitHubPullRequestApi, GraphqlRead, RepositorySelector, STAT_ALIASES_PER_REQUEST,
    STAT_REQUEST_CONCURRENCY, parse_repository_selector, variables,
};
use crate::github::graphql::{ReadGraphQlPagesOptions, read_graphql_pages};
use crate::github::pull_request_json::{
    ChangeRequestSelector, GitHubPullRequestListBatch as ListPage, GitHubPullRequestListItem,
    GitHubPullRequestSearchItem, PULL_REQUEST_SEARCH_MAX_ROWS,
    build_pull_request_stack_memberships_graphql_query, build_pull_request_stats_graphql_query,
    decode_pull_request_list_json, decode_pull_request_search_json,
    decode_pull_request_stack_memberships_json, decode_pull_request_stats_json,
    pull_request_list_graphql_query, pull_request_search_graphql_query,
};
use crate::shared::js::js_trim;
use crate::source_control_core::pull_request_provider::ProviderListCursor;

/// A search-free fallback may scan older rows for local filters, but never the whole repository.
const PULL_REQUEST_FALLBACK_MAX_ROWS: usize = 1_000;

/// What `gh pr list --state` asked a repository's own list for, without search.
fn list_states(state: PullRequestListState) -> &'static [&'static str] {
    match state {
        PullRequestListState::All => &["OPEN", "CLOSED", "MERGED"],
        PullRequestListState::Open => &["OPEN"],
        // Closed includes merged here, the way GitHub's own list reads it; merged rows are
        // filtered out locally by `matches_unsorted_listing`.
        PullRequestListState::Closed => &["CLOSED", "MERGED"],
        PullRequestListState::Merged => &["MERGED"],
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitHubPullRequestListBatch {
    pub items: Vec<GitHubPullRequestListItem>,
    pub truncated: bool,
    /// False for a page GitHub would not search, which came back in `gh`'s own order instead.
    pub continues: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitHubPullRequestSearchBatch {
    /// Rows across every repository asked for, newest update first, each naming its own.
    pub items: Vec<GitHubPullRequestSearchItem>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubPullRequestStat {
    pub repository: String,
    pub number: u64,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Debug, Clone)]
pub struct ListPullRequestsInput {
    pub cwd: String,
    pub repository: String,
    pub host: String,
    pub state: PullRequestListState,
    pub involvement: PullRequestInvolvement,
    pub viewer: String,
    pub limit: usize,
    /// Free text for `--search`, matched as one literal phrase.
    pub query: Option<String>,
    /// Where to carry on from, as a `updated:` qualifier on the same search.
    pub cursor: Option<ProviderListCursor>,
    /// Further narrowings, as qualifiers on the search and as a local pass on the fallback.
    pub filters: Option<PullRequestListFilters>,
}

/// The same listing for a whole host in one search. `limit` is the size of the slice across all
/// of the repositories rather than per repository, because that is what a search answers: the
/// newest rows of the lot, which is exactly the page.
#[derive(Debug, Clone)]
pub struct SearchPullRequestsInput {
    /// Any checkout on the host; the search names its repositories itself.
    pub cwd: String,
    pub host: String,
    pub repositories: Vec<String>,
    pub state: PullRequestListState,
    pub involvement: PullRequestInvolvement,
    pub viewer: String,
    pub limit: usize,
    pub query: Option<String>,
    pub cursor: Option<ProviderListCursor>,
    pub filters: Option<PullRequestListFilters>,
}

#[derive(Debug, Clone)]
pub struct ListPullRequestStatsInput {
    pub cwd: String,
    pub host: String,
    pub change_requests: Vec<ChangeRequestSelector>,
}

/// The parameters of [`search_query`].
pub(crate) struct SearchQueryInput<'a> {
    pub repositories: &'a [String],
    pub state: PullRequestListState,
    pub involvement: PullRequestInvolvement,
    pub viewer: &'a str,
    pub query: Option<&'a str>,
    pub cursor: Option<&'a ProviderListCursor>,
    pub filters: Option<&'a PullRequestListFilters>,
}

/// The reader's own words as one literal phrase of a GitHub search query. Quoting is the whole
/// defence: outside quotes GitHub reads `is:merged` as a qualifier and `label:x` as another, so
/// text typed into a search box could widen the very listing it is meant to narrow — inside them
/// it is only text. The two characters that could end the phrase early are therefore escaped
/// first, which GitHub reads back as themselves; an unbalanced quote is dropped instead, which
/// would let everything after it out of the phrase.
///
/// The phrase is one argv element, so nothing in it can become a flag of its own either.
fn search_phrase(query: &str) -> String {
    format!("\"{}\"", query.replace('\\', "\\\\").replace('"', "\\\""))
}

/// GitHub's own spelling of a review state, which is not the contract's.
fn review_qualifier(review: PullRequestReviewFilter) -> &'static str {
    match review {
        PullRequestReviewFilter::Approved => "approved",
        PullRequestReviewFilter::ChangesRequested => "changes_requested",
        PullRequestReviewFilter::ReviewRequired => "required",
        PullRequestReviewFilter::None => "none",
    }
}

/// The extra narrowings as GitHub search qualifiers. Values a reader typed are quoted, and the
/// one character that could end the quoted value early is dropped rather than escaped: no GitHub
/// label or login holds a double quote, so there is nothing to preserve and everything to lose.
fn qualifier_value(value: &str) -> String {
    format!("\"{}\"", js_trim(&value.replace('"', "")))
}

fn filter_qualifiers(filters: Option<&PullRequestListFilters>, viewer: &str) -> Vec<String> {
    let Some(filters) = filters else {
        return Vec::new();
    };
    let mut qualifiers = Vec::new();
    // One qualifier per group, its names joined by commas — GitHub's own OR.
    for group in filters.labels.iter().flatten() {
        if !group.is_empty() {
            let names: Vec<String> = group
                .iter()
                .map(|name| qualifier_value(name.as_str()))
                .collect();
            qualifiers.push(format!("label:{}", names.join(",")));
        }
    }
    for label in filters.excluded_labels.iter().flatten() {
        qualifiers.push(format!("-label:{}", qualifier_value(label.as_str())));
    }
    if let Some(author) = &filters.author {
        qualifiers.push(format!(
            "author:{}",
            qualifier_value(&resolve_pull_request_author_filter(
                author.as_str(),
                Some(viewer)
            ))
        ));
    }
    if let Some(draft) = filters.draft {
        qualifiers.push(format!("draft:{}", draft == PullRequestDraftFilter::Only));
    }
    if let Some(review) = filters.review {
        qualifiers.push(format!("review:{}", review_qualifier(review)));
    }
    if let Some(checks) = filters.checks {
        qualifiers.push(format!(
            "status:{}",
            if checks == PullRequestChecksFilter::Passing {
                "success"
            } else {
                "failure"
            }
        ));
    }
    qualifiers
}

/// The same narrowings over a row that has already arrived, for the search-free fallback. Every
/// listed row now carries its own `checksState`, so `checks` is judged the way `review` is: by
/// equality against the row's field. Unlike `review`, `checks` has no `"none"` value to catch an
/// absent state on purpose — a row with no checks configured, or whose checks are still `pending`,
/// equals neither `"passing"` nor `"failing"` and so fails both, the same as a row search would
/// not have surfaced for `status:success` or `status:failure`.
fn matches_filters(
    item: &GitHubPullRequestListItem,
    filters: Option<&PullRequestListFilters>,
    viewer: &str,
) -> bool {
    let Some(filters) = filters else {
        return true;
    };
    let labels: std::collections::HashSet<String> = item
        .labels
        .iter()
        .map(|label| js_trim(label.name.as_str()).to_lowercase())
        .collect();
    let holds = |label: &str| labels.contains(&js_trim(label).to_lowercase());
    filters
        .draft
        .is_none_or(|draft| item.is_draft == (draft == PullRequestDraftFilter::Only))
        && filters.review.is_none_or(|review| match review {
            PullRequestReviewFilter::None => item.review_decision.is_none(),
            PullRequestReviewFilter::Approved => {
                item.review_decision == Some(PullRequestReviewDecision::Approved)
            }
            PullRequestReviewFilter::ChangesRequested => {
                item.review_decision == Some(PullRequestReviewDecision::ChangesRequested)
            }
            PullRequestReviewFilter::ReviewRequired => {
                item.review_decision == Some(PullRequestReviewDecision::ReviewRequired)
            }
        })
        && filters.checks.is_none_or(|checks| match checks {
            PullRequestChecksFilter::Passing => {
                item.checks_state == Some(PullRequestChecksState::Passing)
            }
            PullRequestChecksFilter::Failing => {
                item.checks_state == Some(PullRequestChecksState::Failing)
            }
        })
        && filters.labels.as_ref().is_none_or(|groups| {
            groups
                .iter()
                .all(|group| group.iter().any(|name| holds(name.as_str())))
        })
        && filters
            .excluded_labels
            .as_ref()
            .is_none_or(|excluded| !excluded.iter().any(|name| holds(name.as_str())))
        && filters.author.as_ref().is_none_or(|author| {
            item.author.as_ref().is_some_and(|actor| {
                actor.login.as_str().to_lowercase()
                    == resolve_pull_request_author_filter(author.as_str(), Some(viewer))
                        .to_lowercase()
            })
        })
}

/// The search-free fallback is wider than the request, so narrow its decoded rows locally.
fn matches_unsorted_listing(
    item: &GitHubPullRequestListItem,
    input: &ListPullRequestsInput,
) -> bool {
    let matches_state = input.state == PullRequestListState::All
        || matches!(
            (input.state, item.state),
            (PullRequestListState::Open, PullRequestState::Open)
                | (PullRequestListState::Closed, PullRequestState::Closed)
                | (PullRequestListState::Merged, PullRequestState::Merged)
        );
    let viewer = input.viewer.to_lowercase();
    let matches_involvement = match input.involvement {
        PullRequestInvolvement::All => true,
        PullRequestInvolvement::Authored => item
            .author
            .as_ref()
            .is_some_and(|actor| actor.login.as_str().to_lowercase() == viewer),
        PullRequestInvolvement::Reviewing => {
            item.has_team_review_request
                || item
                    .review_request_logins
                    .iter()
                    .any(|login| login.to_lowercase() == viewer)
        }
    };
    matches_state
        && matches_involvement
        && matches_filters(item, input.filters.as_ref(), &input.viewer)
}

/// What a repository selector may hold before it goes into a search as itself.
static SEARCH_REPOSITORY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$").expect("static regex"));

/// A listing as one GitHub search, across one repository or several: the only way to read a
/// whole host in one request, and the only order (`updated`) a continuation can carry on from.
///
/// `--state closed` in GitHub's own list includes merged pull requests, so the Closed tab is
/// `is:closed is:unmerged` here.
///
/// None where a repository is not `owner/name`. A name is written into the query as itself, and a
/// name holding a space could otherwise end the `repo:` qualifier and start a qualifier of its
/// own — so an unaddressable one refuses the whole read rather than being escaped into something
/// GitHub might still read.
pub(crate) fn search_query(input: &SearchQueryInput<'_>) -> Option<String> {
    if input.repositories.is_empty() {
        return None;
    }
    let repositories: Vec<&str> = input.repositories.iter().map(|r| js_trim(r)).collect();
    if !repositories
        .iter()
        .all(|repository| SEARCH_REPOSITORY.is_match(repository))
    {
        return None;
    }
    let query = js_trim(input.query.unwrap_or(""));
    let mut parts: Vec<String> = vec!["is:pr".into()];
    // "all" is every state, which `is:pr` already is.
    match input.state {
        PullRequestListState::Open => parts.push("is:open".into()),
        PullRequestListState::Closed => {
            parts.push("is:closed".into());
            parts.push("is:unmerged".into());
        }
        PullRequestListState::Merged => parts.push("is:merged".into()),
        PullRequestListState::All => {}
    }
    match input.involvement {
        PullRequestInvolvement::Authored => parts.push(format!("author:{}", input.viewer)),
        PullRequestInvolvement::Reviewing => {
            parts.push(format!("review-requested:{}", input.viewer))
        }
        PullRequestInvolvement::All => {}
    }
    if !query.is_empty() {
        parts.push(search_phrase(query));
    }
    // Inclusive, and de-duplicated by the caller, for the reason the per-repository read gives.
    if let Some(cursor) = input.cursor {
        parts.push(format!("updated:<={}", cursor.updated_before));
    }
    parts.extend(filter_qualifiers(input.filters, input.viewer));
    // The order the page reads its rows in, and the only order a continuation can carry on from.
    parts.push("sort:updated-desc".into());
    parts.extend(
        repositories
            .iter()
            .map(|repository| format!("repo:{repository}")),
    );
    Some(parts.join(" "))
}

struct Collected {
    items: Vec<GitHubPullRequestListItem>,
    raw_count: usize,
    truncated: bool,
}

impl GitHubPullRequestApi {
    /// One page of a listing, from the search or from the repository's own list.
    #[allow(clippy::too_many_arguments)]
    async fn list_page(
        &self,
        input: &ListPullRequestsInput,
        owner: &str,
        name: &str,
        query: &str,
        continues: bool,
        after: Option<String>,
        rows: usize,
    ) -> Result<ListPage, GitHubPullRequestApiError> {
        let rows = rows.min(u32::MAX as usize) as u32;
        if continues {
            let batch = self
                .graphql_read(
                    GraphqlRead {
                        cwd: &input.cwd,
                        host: &input.host,
                        operation: "listPullRequests",
                        allow_reserve: false,
                        // The reader's own words are in the query.
                        variables: Some(variables(
                            serde_json::json!({ "q": query, "after": after }),
                        )),
                        query: pull_request_search_graphql_query(rows, false, true),
                    },
                    decode_pull_request_search_json,
                )
                .await?;
            Ok(ListPage {
                items: batch.items.into_iter().map(|item| item.base).collect(),
                raw_count: batch.raw_count,
                end_cursor: batch.end_cursor,
            })
        } else {
            self.graphql_read(
                GraphqlRead {
                    cwd: &input.cwd,
                    host: &input.host,
                    operation: "listPullRequests",
                    allow_reserve: false,
                    variables: Some(variables(serde_json::json!({
                        "owner": owner, "name": name, "states": list_states(input.state), "after": after
                    }))),
                    query: pull_request_list_graphql_query(rows, false),
                },
                decode_pull_request_list_json,
            )
            .await
        }
    }

    /// Pages of up to a hundred until `rows` have arrived or GitHub has no more.
    #[allow(clippy::too_many_arguments)]
    async fn collect_pages(
        &self,
        input: &ListPullRequestsInput,
        owner: &str,
        name: &str,
        query: &str,
        continues: bool,
        rows: usize,
    ) -> Result<Collected, GitHubPullRequestApiError> {
        let count_rows =
            |pages: &[ListPage]| pages.iter().map(|page| page.raw_count).sum::<usize>();
        let pages = read_graphql_pages(
            |after: Option<String>, pages: &[ListPage]| {
                // Counted before the request starts: the future must not borrow the pages so far.
                let remaining = rows.saturating_sub(count_rows(pages));
                async move {
                    self.list_page(input, owner, name, query, continues, after, remaining)
                        .await
                }
            },
            ReadGraphQlPagesOptions {
                next_cursor: &|page: &ListPage| page.end_cursor.clone(),
                from: None,
                max_pages: None,
                until: Some(&|pages: &[ListPage]| count_rows(pages) >= rows),
            },
        )
        .await?;
        // `truncated` also covers a cursor GitHub repeated before `rows` arrived.
        Ok(Collected {
            raw_count: count_rows(&pages.pages),
            items: pages
                .pages
                .into_iter()
                .flat_map(|page| page.items)
                .collect(),
            truncated: pages.truncated,
        })
    }

    async fn read_listing(
        &self,
        input: &ListPullRequestsInput,
        owner: &str,
        name: &str,
        query: &str,
        continues: bool,
    ) -> Result<GitHubPullRequestListBatch, GitHubPullRequestApiError> {
        let fallback_max_rows = (input.limit + 1).max(PULL_REQUEST_FALLBACK_MAX_ROWS);
        let mut requested_rows = input.limit + 1;
        loop {
            let Collected {
                items: raw_items,
                raw_count,
                truncated: cut_short,
            } = self
                .collect_pages(input, owner, name, query, continues, requested_rows)
                .await?;
            let mut items: Vec<GitHubPullRequestListItem> = if continues {
                raw_items
            } else {
                raw_items
                    .into_iter()
                    .filter(|item| matches_unsorted_listing(item, input))
                    .collect()
            };
            if !continues
                && items.len() < input.limit
                && raw_count >= requested_rows
                && requested_rows < fallback_max_rows
            {
                let next_rows = (requested_rows * 2).min(fallback_max_rows);
                if next_rows > requested_rows {
                    requested_rows = next_rows;
                    continue;
                }
            }
            // One row over the page size is the probe for a next page, and it is counted before
            // decoding: a skipped malformed row must not end paging.
            let truncated = cut_short
                || if continues {
                    raw_count > input.limit
                } else {
                    items.len() > input.limit || raw_count >= requested_rows
                };
            items.truncate(input.limit);
            return Ok(GitHubPullRequestListBatch {
                items,
                truncated,
                continues,
            });
        }
    }

    pub async fn list_pull_requests(
        &self,
        input: &ListPullRequestsInput,
    ) -> Result<GitHubPullRequestListBatch, GitHubPullRequestApiError> {
        let RepositorySelector { owner, name } = parse_repository_selector(&input.repository);
        let repositories = [input.repository.clone()];
        let Some(query) = search_query(&SearchQueryInput {
            repositories: &repositories,
            state: input.state,
            involvement: input.involvement,
            viewer: &input.viewer,
            query: input.query.as_deref(),
            cursor: input.cursor.as_ref(),
            filters: input.filters.as_ref(),
        }) else {
            return Err(GitHubRepositorySelectorError {
                cwd: input.cwd.clone(),
                operation: "listPullRequests".into(),
            }
            .into());
        };
        // GitHub does not index every repository for search, and one it will not search answers
        // with no rows rather than with an error — so an empty listing is read again from the
        // repository's own list. Those rows come back newest-created first, an order no `updated:`
        // qualifier can carry on from, so that page says it cannot be continued and the reader
        // reaches the rest of it by asking for a larger page, as every listing used to.
        //
        // Only ever the first slice: a repository that answered the search once will answer it
        // again, so an empty slice under a cursor is a repository that has run out.
        // A text search that finds nothing has found nothing: falling back would answer it with the
        // repository's whole list, which is every row the reader did not search for. The fallback
        // is for a repository the index does not cover, and a listing with no text to match is the
        // only place an empty answer can mean that.
        // Every filter is a qualifier `matches_filters` can judge over the fallback's own rows just
        // as well as search judges them over its own, so carrying them into the fallback answers
        // the same read rather than a wider one. Free text is the one thing the fallback cannot
        // judge locally — it lists rows, it does not search their text — so a query still rules
        // the fallback out: an empty answer under one is already the answer.
        let has_query = input
            .query
            .as_deref()
            .is_some_and(|q| !js_trim(q).is_empty());
        let mut batch = self
            .read_listing(input, &owner, &name, &query, true)
            .await?;
        if batch.items.is_empty() && input.cursor.is_none() && !has_query {
            batch = self
                .read_listing(input, &owner, &name, &query, false)
                .await?;
        }
        // Match the search query's host support, and enrich only rows that survived paging.
        if input.host != "github.com" || batch.items.is_empty() {
            return Ok(batch);
        }
        let chunks: Vec<Vec<GitHubPullRequestListItem>> = batch
            .items
            .chunks(STAT_ALIASES_PER_REQUEST)
            .map(<[_]>::to_vec)
            .collect();
        let chunks: Vec<Vec<GitHubPullRequestListItem>> = stream::iter(chunks)
            .map(|chunk| self.enrich_with_stacks(input, chunk))
            .buffered(STAT_REQUEST_CONCURRENCY)
            .collect()
            .await;
        batch.items = chunks.into_iter().flatten().collect();
        Ok(batch)
    }

    async fn enrich_with_stacks(
        &self,
        input: &ListPullRequestsInput,
        mut chunk: Vec<GitHubPullRequestListItem>,
    ) -> Vec<GitHubPullRequestListItem> {
        let numbers: Vec<u64> = chunk.iter().map(|item| item.number).collect();
        let Some(document) =
            build_pull_request_stack_memberships_graphql_query(&input.repository, &numbers)
        else {
            return chunk;
        };
        match self
            .graphql_read(
                GraphqlRead {
                    cwd: &input.cwd,
                    host: &input.host,
                    operation: "listPullRequestStackMemberships",
                    allow_reserve: false,
                    variables: Some(document.variables),
                    query: document.query,
                },
                decode_pull_request_stack_memberships_json,
            )
            .await
        {
            Ok(memberships) => {
                for (index, item) in chunk.iter_mut().enumerate() {
                    if let Some(stack) = memberships.get(&(index as u64)) {
                        item.stack = Some(stack.clone());
                    }
                }
                chunk
            }
            // Optional badges must not take down a listing that already read successfully.
            Err(_) => {
                tracing::warn!(
                    operation = "listPullRequestStackMemberships",
                    host = %input.host,
                    rows = chunk.len(),
                    "Pull request stack membership enrichment failed"
                );
                chunk
            }
        }
    }

    pub async fn search_pull_requests(
        &self,
        input: &SearchPullRequestsInput,
    ) -> Result<GitHubPullRequestSearchBatch, GitHubPullRequestApiError> {
        let Some(query) = search_query(&SearchQueryInput {
            repositories: &input.repositories,
            state: input.state,
            involvement: input.involvement,
            viewer: &input.viewer,
            query: input.query.as_deref(),
            cursor: input.cursor.as_ref(),
            filters: input.filters.as_ref(),
        }) else {
            return Err(GitHubRepositorySelectorError {
                cwd: input.cwd.clone(),
                operation: "searchPullRequests".into(),
            }
            .into());
        };
        // One extra row reveals that the host has more than the slice shows, the way the
        // per-repository read does — up to GitHub's own ceiling on a search page, past which
        // `hasNextPage` is what says there is more.
        let rows = (input.limit + 1).min(PULL_REQUEST_SEARCH_MAX_ROWS as usize);
        let batch = self
            .graphql_read(
                GraphqlRead {
                    cwd: &input.cwd,
                    host: &input.host,
                    operation: "searchPullRequests",
                    allow_reserve: false,
                    variables: Some(variables(serde_json::json!({ "q": query, "after": null }))),
                    query: pull_request_search_graphql_query(
                        rows as u32,
                        input.host == "github.com",
                        false,
                    ),
                },
                decode_pull_request_search_json,
            )
            .await?;
        let truncated = batch.raw_count > input.limit || batch.has_next_page;
        let mut items = batch.items;
        items.truncate(input.limit);
        Ok(GitHubPullRequestSearchBatch { items, truncated })
    }

    /// The line counts the search leaves out, for rows already on the page.
    pub async fn list_pull_request_stats(
        &self,
        input: &ListPullRequestStatsInput,
    ) -> Result<Vec<GitHubPullRequestStat>, GitHubPullRequestApiError> {
        // Owned chunks: a stream of borrowed slices makes the future's `Send` unprovable.
        let chunks: Vec<Vec<ChangeRequestSelector>> = input
            .change_requests
            .chunks(STAT_ALIASES_PER_REQUEST)
            .map(<[ChangeRequestSelector]>::to_vec)
            .collect();
        let results: Vec<Vec<GitHubPullRequestStat>> = stream::iter(chunks)
            .map(|chunk: Vec<ChangeRequestSelector>| async move {
                let Some(document) = build_pull_request_stats_graphql_query(&chunk) else {
                    return Err(GitHubPullRequestApiError::from(
                        GitHubRepositorySelectorError {
                            cwd: input.cwd.clone(),
                            operation: "listPullRequestStats".into(),
                        },
                    ));
                };
                let stats = self
                    .graphql_read(
                        GraphqlRead {
                            cwd: &input.cwd,
                            host: &input.host,
                            operation: "listPullRequestStats",
                            allow_reserve: false,
                            variables: Some(document.variables),
                            query: document.query,
                        },
                        decode_pull_request_stats_json,
                    )
                    .await?;
                Ok(chunk
                    .iter()
                    .enumerate()
                    .filter_map(|(index, change_request)| {
                        stats
                            .get(&(index as u64))
                            .map(|stat| GitHubPullRequestStat {
                                repository: change_request.repository.clone(),
                                number: change_request.number,
                                additions: stat.additions,
                                deletions: stat.deletions,
                            })
                    })
                    .collect())
            })
            .buffered(STAT_REQUEST_CONCURRENCY)
            .try_collect()
            .await?;
        Ok(results.into_iter().flatten().collect())
    }
}
