use serde_json::{Value, json};
use turin_contracts::pull_request::{
    PullRequestChecksFilter, PullRequestDraftFilter, PullRequestInvolvement,
    PullRequestListFilters, PullRequestListState, PullRequestQualifierValue,
    PullRequestReviewFilter, PullRequestStackMembership,
};

use super::super::errors::GitHubPullRequestApiError;
use super::super::list::*;
use super::super::test_support::*;
use crate::github::api::{GitHubApiError, GitHubApiResponseError};
use crate::source_control_core::pull_request_provider::ProviderListCursor;

fn qv(value: &str) -> PullRequestQualifierValue {
    PullRequestQualifierValue::new(value).unwrap()
}

fn list_input(
    state: PullRequestListState,
    involvement: PullRequestInvolvement,
    limit: usize,
) -> ListPullRequestsInput {
    ListPullRequestsInput {
        cwd: "/w".into(),
        repository: "acme/web".into(),
        host: "github.com".into(),
        state,
        involvement,
        viewer: "bilal".into(),
        limit,
        query: None,
        cursor: None,
        filters: None,
    }
}

fn open_all(limit: usize) -> ListPullRequestsInput {
    list_input(
        PullRequestListState::Open,
        PullRequestInvolvement::All,
        limit,
    )
}

fn search_input(
    state: PullRequestListState,
    involvement: PullRequestInvolvement,
    repositories: &[&str],
    limit: usize,
) -> SearchPullRequestsInput {
    SearchPullRequestsInput {
        cwd: "/w".into(),
        host: "github.com".into(),
        repositories: repositories.iter().map(|r| (*r).to_owned()).collect(),
        state,
        involvement,
        viewer: "bilal".into(),
        limit,
        query: None,
        cursor: None,
        filters: None,
    }
}

fn filters() -> PullRequestListFilters {
    PullRequestListFilters {
        draft: None,
        review: None,
        checks: None,
        labels: None,
        excluded_labels: None,
        author: None,
    }
}

fn numbers(batch: &GitHubPullRequestListBatch) -> Vec<u64> {
    batch.items.iter().map(|item| item.number).collect()
}

fn membership() -> PullRequestStackMembership {
    serde_json::from_value(json!({ "number": 3, "size": 2, "base": "main", "position": 1 }))
        .unwrap()
}

#[tokio::test]
async fn asks_for_one_row_more_than_the_page_to_probe_for_a_next_page() {
    let harness = Harness::new();
    harness.once_body(pull_requests(3, 1, no_overrides));
    let batch = harness.cli.list_pull_requests(&open_all(10)).await.unwrap();
    assert_eq!(batch.items.len(), 3);
    assert!(!batch.truncated);
    let call = harness.call_at(0);
    assert!(call.is_graphql());
    assert_eq!(call.host(), "github.com");
    assert_eq!(
        harness.search_of_call(0).as_deref(),
        Some("is:pr is:open sort:updated-desc repo:acme/web")
    );
    assert!(harness.query_at(0).contains("first: 11"));
}

#[tokio::test]
async fn reports_truncation_from_the_extra_row_counted_before_decoding() {
    let harness = Harness::new();
    harness.once_body(pull_requests(11, 1, no_overrides));
    let batch = harness.cli.list_pull_requests(&open_all(10)).await.unwrap();
    assert_eq!(batch.items.len(), 10);
    assert!(batch.truncated);
}

#[tokio::test]
async fn reports_truncation_when_github_repeats_a_cursor_before_the_page_is_full() {
    // Two pages of 100 that hand back the same cursor, for a page of 250.
    let harness = Harness::new();
    harness.always(ok_json(json!({ "data": { "search": {
        "pageInfo": { "hasNextPage": true, "endCursor": "same" },
        "nodes": rows(100, 1, no_overrides),
    } } })));
    let batch = harness
        .cli
        .list_pull_requests(&open_all(250))
        .await
        .unwrap();
    assert_eq!(harness.call_count(), 2);
    assert!(batch.truncated);
}

#[tokio::test]
async fn excludes_merged_pull_requests_from_the_closed_tab() {
    let harness = Harness::new();
    harness.on(empty_listing);
    harness
        .cli
        .list_pull_requests(&list_input(
            PullRequestListState::Closed,
            PullRequestInvolvement::All,
            10,
        ))
        .await
        .unwrap();
    // A closed pull request may also be a merged one, so the tab narrows through search.
    assert_eq!(
        harness.search_of_call(0).as_deref(),
        Some("is:pr is:closed is:unmerged sort:updated-desc repo:acme/web")
    );
}

#[tokio::test]
async fn narrows_to_the_author_on_the_authored_tab() {
    let harness = Harness::new();
    harness.on(empty_listing);
    harness
        .cli
        .list_pull_requests(&list_input(
            PullRequestListState::Open,
            PullRequestInvolvement::Authored,
            10,
        ))
        .await
        .unwrap();
    assert_eq!(
        harness.search_of_call(0).as_deref(),
        Some("is:pr is:open author:bilal sort:updated-desc repo:acme/web")
    );
}

#[tokio::test]
async fn narrows_through_search_on_the_reviewing_tab() {
    let harness = Harness::new();
    harness.on(empty_listing);
    harness
        .cli
        .list_pull_requests(&list_input(
            PullRequestListState::Open,
            PullRequestInvolvement::Reviewing,
            10,
        ))
        .await
        .unwrap();
    assert_eq!(
        harness.search_of_call(0).as_deref(),
        Some("is:pr is:open review-requested:bilal sort:updated-desc repo:acme/web")
    );
}

#[tokio::test]
async fn carries_every_repository_and_every_qualifier_into_one_search() {
    let harness = Harness::new();
    harness.always(search_page(vec![], false));
    let mut input = search_input(
        PullRequestListState::Closed,
        PullRequestInvolvement::Reviewing,
        &["acme/web", "pingdotgg/t3code"],
        10,
    );
    input.query = Some("pull requests page".into());
    input.cursor = Some(ProviderListCursor {
        updated_before: "2026-07-02T00:00:00Z".into(),
        delivered: 10,
    });
    harness.cli.search_pull_requests(&input).await.unwrap();
    // One request for both repositories, carrying everything the per-repository read expresses
    // as a flag: the tab, the involvement, the reader's words, where to carry on from, and the
    // order the page reads in.
    assert_eq!(harness.call_count(), 1);
    assert_eq!(
        harness.search_of_call(0).as_deref(),
        Some(
            "is:pr is:closed is:unmerged review-requested:bilal \"pull requests page\" \
             updated:<=2026-07-02T00:00:00Z sort:updated-desc repo:acme/web repo:pingdotgg/t3code"
        )
    );
}

#[tokio::test]
async fn narrows_a_search_to_the_author_and_to_merged_on_the_merged_tab() {
    let harness = Harness::new();
    harness.always(search_page(vec![], false));
    harness
        .cli
        .search_pull_requests(&search_input(
            PullRequestListState::Merged,
            PullRequestInvolvement::Authored,
            &["acme/web"],
            10,
        ))
        .await
        .unwrap();
    assert_eq!(
        harness.search_of_call(0).as_deref(),
        Some("is:pr is:merged author:bilal sort:updated-desc repo:acme/web")
    );
}

#[tokio::test]
async fn keeps_a_searched_for_qualifier_inside_the_phrase() {
    let harness = Harness::new();
    harness.always(search_page(vec![], false));
    let mut input = search_input(
        PullRequestListState::Open,
        PullRequestInvolvement::All,
        &["acme/web"],
        10,
    );
    input.query = Some("x\" is:merged repo:evil/repo".into());
    harness.cli.search_pull_requests(&input).await.unwrap();
    // Quoted and escaped, so the words a reader typed narrow the listing rather than widening it.
    assert_eq!(
        harness.search_of_call(0).as_deref(),
        Some("is:pr is:open \"x\\\" is:merged repo:evil/repo\" sort:updated-desc repo:acme/web")
    );
}

#[tokio::test]
async fn refuses_to_search_for_a_repository_github_cannot_address() {
    let harness = Harness::new();
    let failure = harness
        .cli
        .search_pull_requests(&search_input(
            PullRequestListState::Open,
            PullRequestInvolvement::All,
            &["acme/web", "acme/web is:merged"],
            10,
        ))
        .await
        .unwrap_err();
    // Nothing is sent: a name that could end its own qualifier is refused rather than escaped.
    assert!(matches!(
        failure,
        GitHubPullRequestApiError::RepositorySelector(_)
    ));
    assert_eq!(harness.call_count(), 0);
}

#[tokio::test]
async fn files_each_searched_row_under_the_repository_it_came_from() {
    let harness = Harness::new();
    harness.always(search_page(
        vec![
            search_item(7, "acme/web", "2026-07-03T00:00:00Z"),
            search_item(9, "pingdotgg/t3code", "2026-07-02T00:00:00Z"),
            // Not a pull request, which `is:pr` excludes and a decode skips rather than fails on.
            json!({}),
        ],
        false,
    ));
    let batch = harness
        .cli
        .search_pull_requests(&search_input(
            PullRequestListState::Open,
            PullRequestInvolvement::All,
            &["acme/web", "pingdotgg/t3code"],
            10,
        ))
        .await
        .unwrap();
    let rows: Vec<_> = batch
        .items
        .iter()
        .map(|item| {
            (
                item.repository.clone(),
                item.number,
                item.author
                    .as_ref()
                    .and_then(|author| author.avatar_url.clone()),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "acme/web".to_owned(),
                7,
                Some("https://avatars/octocat".to_owned())
            ),
            (
                "pingdotgg/t3code".to_owned(),
                9,
                Some("https://avatars/octocat".to_owned())
            ),
        ]
    );
    // The listing leaves the line counts to a read of their own.
    assert_eq!(
        batch
            .items
            .iter()
            .map(|item| (item.additions, item.deletions))
            .collect::<Vec<_>>(),
        vec![(0, 0), (0, 0)]
    );
    assert!(!batch.truncated);
}

#[tokio::test]
async fn reports_search_truncation_from_the_extra_row_and_from_a_page_github_says_has_more() {
    let harness = Harness::new();
    harness.once(search_page(
        vec![
            search_item(1, "acme/web", "2026-07-03T00:00:00Z"),
            search_item(2, "acme/web", "2026-07-02T00:00:00Z"),
            search_item(3, "acme/web", "2026-07-01T00:00:00Z"),
        ],
        false,
    ));
    harness.once(search_page(
        vec![search_item(1, "acme/web", "2026-07-03T00:00:00Z")],
        true,
    ));
    let input = search_input(
        PullRequestListState::Open,
        PullRequestInvolvement::All,
        &["acme/web"],
        2,
    );
    let overflowing = harness.cli.search_pull_requests(&input).await.unwrap();
    let capped = harness.cli.search_pull_requests(&input).await.unwrap();
    // The extra row is the probe, and it is not handed on.
    assert_eq!(overflowing.items.len(), 2);
    assert!(overflowing.truncated);
    // A slice at GitHub's own ceiling has no extra row to probe with, so `hasNextPage` answers.
    assert!(capped.truncated);
}

#[tokio::test]
async fn enriches_only_the_visible_fallback_rows_after_filtering_and_widening() {
    let harness = Harness::new();
    harness.once(empty_search());
    harness.once_body(listed_pull_requests(3, 1, |_| json!({ "isDraft": true })));
    harness.once_body(listed_pull_requests(
        6,
        1,
        |number| json!({ "isDraft": number < 4 }),
    ));
    harness.stack_once(ok_json(json!({ "data": {
        "s0": { "pullRequest": {
            "stack": { "number": 3, "size": 2, "baseRefName": "main" },
            "stackEntry": { "position": 1 },
        } },
        "s1": { "pullRequest": { "stack": null, "stackEntry": null } },
    } })));
    let mut input = open_all(2);
    input.filters = Some(PullRequestListFilters {
        draft: Some(PullRequestDraftFilter::Hide),
        ..filters()
    });
    let batch = harness.cli.list_pull_requests(&input).await.unwrap();
    assert_eq!(numbers(&batch), vec![4, 5]);
    assert_eq!(batch.items[0].stack, Some(membership()));
    assert_eq!(batch.items[1].stack, None);
    assert!(batch.truncated);
    assert!(!batch.continues);
    let stack_calls = harness.stack_calls();
    assert_eq!(stack_calls.len(), 1);
    let numbers: Vec<Value> = stack_calls[0]
        .variables()
        .into_iter()
        .filter(|(name, _)| name.ends_with("_number"))
        .map(|(_, number)| number)
        .collect();
    assert_eq!(numbers, vec![json!(4), json!(5)]);
}

#[tokio::test]
async fn batches_membership_reads_and_keeps_successful_rows_when_one_chunk_fails() {
    let harness = Harness::new();
    harness.once_body(pull_requests(27, 1, no_overrides));
    harness.stack_on(|call| {
        if call.variables().values().any(|value| value == &json!(26)) {
            Err(GitHubApiError::Response(GitHubApiResponseError {
                host: "github.com".into(),
                operation: "listPullRequestStackMemberships".into(),
                status: 502,
                github_errors: None,
            }))
        } else {
            ok_json(json!({ "data": { "s0": { "pullRequest": {
                "stack": { "number": 3, "size": 2, "baseRefName": "main" },
                "stackEntry": { "position": 1 },
            } } } }))
        }
    });
    let batch = harness.cli.list_pull_requests(&open_all(26)).await.unwrap();
    assert_eq!(numbers(&batch), (1..=26).collect::<Vec<u64>>());
    assert_eq!(batch.items[0].stack, Some(membership()));
    assert_eq!(batch.items[25].stack, None);
    assert!(batch.truncated);
    assert!(batch.continues);
    assert_eq!(harness.stack_calls().len(), 2);
}

#[tokio::test]
async fn skips_membership_enrichment_for_empty_pages_and_enterprise_hosts() {
    let harness = Harness::new();
    harness.once(empty_search());
    harness.once(empty_list());
    harness.once_body(pull_requests(1, 7, no_overrides));
    let empty = harness.cli.list_pull_requests(&open_all(2)).await.unwrap();
    let mut enterprise_input = open_all(2);
    enterprise_input.host = "github.acme.test".into();
    let enterprise = harness
        .cli
        .list_pull_requests(&enterprise_input)
        .await
        .unwrap();
    assert!(empty.items.is_empty());
    assert_eq!(numbers(&enterprise), vec![7]);
    assert!(harness.stack_calls().is_empty());
}

#[tokio::test]
async fn reads_the_line_counts_in_chunks_and_files_them_back_by_position() {
    let harness = Harness::new();
    let change_requests: Vec<_> = (1..=26)
        .map(
            |number| crate::github::pull_request_json::ChangeRequestSelector {
                repository: "acme/web".into(),
                number,
            },
        )
        .collect();
    // Every chunk answers for its first alias only, so a row GitHub said nothing about is
    // dropped rather than shown as a change of no size.
    harness.always(ok_json(
        json!({ "data": { "s0": { "pullRequest": { "additions": 4, "deletions": 1 } } } }),
    ));
    let stats = harness
        .cli
        .list_pull_request_stats(&ListPullRequestStatsInput {
            cwd: "/w".into(),
            host: "github.com".into(),
            change_requests,
        })
        .await
        .unwrap();
    // Twenty-five aliases a request, so twenty-six rows are two requests.
    assert_eq!(harness.call_count(), 2);
    assert_eq!(
        stats,
        vec![
            GitHubPullRequestStat {
                repository: "acme/web".into(),
                number: 1,
                additions: 4,
                deletions: 1
            },
            GitHubPullRequestStat {
                repository: "acme/web".into(),
                number: 26,
                additions: 4,
                deletions: 1
            },
        ]
    );
    assert!(
        harness
            .query_at(0)
            .contains("s0: repository(owner: $s0_owner, name: $s0_name)")
    );
    let vars = harness.vars_at(0);
    assert_eq!(vars["s0_owner"], "acme");
    assert_eq!(vars["s0_name"], "web");
    assert_eq!(vars["s24_number"], 25);
}

#[tokio::test]
async fn refuses_to_look_up_counts_for_a_repository_github_cannot_address() {
    let harness = Harness::new();
    let failure = harness
        .cli
        .list_pull_request_stats(&ListPullRequestStatsInput {
            cwd: "/w".into(),
            host: "github.com".into(),
            change_requests: vec![crate::github::pull_request_json::ChangeRequestSelector {
                repository: "acme/web\") { x } #".into(),
                number: 1,
            }],
        })
        .await
        .unwrap_err();
    assert!(matches!(
        failure,
        GitHubPullRequestApiError::RepositorySelector(_)
    ));
    assert_eq!(harness.call_count(), 0);
}

#[tokio::test]
async fn hands_a_search_to_github_rather_than_to_the_rows_already_read() {
    let harness = Harness::new();
    harness.on(empty_listing);
    let mut input = open_all(10);
    input.query = Some("pull requests page".into());
    harness.cli.list_pull_requests(&input).await.unwrap();
    // The recency qualifier rides along, because free text would otherwise reorder the page
    // by relevance and truncation would drop the newest matches.
    assert_eq!(
        harness.search_of_call(0).as_deref(),
        Some("is:pr is:open \"pull requests page\" sort:updated-desc repo:acme/web")
    );
}

#[tokio::test]
async fn joins_a_search_onto_the_tabs_own_qualifiers_instead_of_replacing_them() {
    let harness = Harness::new();
    harness.on(empty_listing);
    let mut input = list_input(
        PullRequestListState::Closed,
        PullRequestInvolvement::Reviewing,
        10,
    );
    input.query = Some("page".into());
    harness.cli.list_pull_requests(&input).await.unwrap();
    assert_eq!(
        harness.search_of_call(0).as_deref(),
        Some(
            "is:pr is:closed is:unmerged review-requested:bilal \"page\" sort:updated-desc repo:acme/web"
        )
    );
}

#[tokio::test]
async fn carries_the_further_narrowings_into_the_search_as_qualifiers() {
    let harness = Harness::new();
    harness.on(empty_listing);
    let mut input = open_all(10);
    input.filters = Some(PullRequestListFilters {
        draft: Some(PullRequestDraftFilter::Hide),
        review: Some(PullRequestReviewFilter::ChangesRequested),
        checks: Some(PullRequestChecksFilter::Failing),
        labels: Some(vec![vec![qv("needs design")], vec![qv("quo\"te")]]),
        excluded_labels: Some(vec![qv("wip")]),
        author: Some(qv("octocat")),
    });
    harness.cli.list_pull_requests(&input).await.unwrap();
    // Quotes around anything a reader typed, and the one character that could end a quoted
    // value early dropped rather than escaped.
    assert_eq!(
        harness.search_of_call(0).as_deref(),
        Some(
            "is:pr is:open label:\"needs design\" label:\"quote\" -label:\"wip\" author:\"octocat\" \
             draft:false review:changes_requested status:failure sort:updated-desc repo:acme/web"
        )
    );
}

#[tokio::test]
async fn resolves_an_author_filter_of_me_to_the_viewer_not_the_literal_word() {
    let harness = Harness::new();
    harness.on(empty_listing);
    let mut input = open_all(10);
    input.filters = Some(PullRequestListFilters {
        author: Some(qv("me")),
        ..filters()
    });
    harness.cli.list_pull_requests(&input).await.unwrap();
    assert_eq!(
        harness.search_of_call(0).as_deref(),
        Some("is:pr is:open author:\"bilal\" sort:updated-desc repo:acme/web")
    );
}

#[tokio::test]
async fn sends_one_label_qualifier_per_group_its_names_joined_the_way_github_ors_them() {
    let harness = Harness::new();
    harness.on(empty_listing);
    let mut input = open_all(10);
    input.filters = Some(PullRequestListFilters {
        labels: Some(vec![vec![qv("size:S"), qv("size:XS")], vec![qv("bug")]]),
        ..filters()
    });
    harness.cli.list_pull_requests(&input).await.unwrap();
    // One qualifier satisfied by either size, and a second one that must hold as well.
    assert_eq!(
        harness.search_of_call(0).as_deref(),
        Some(
            "is:pr is:open label:\"size:S\",\"size:XS\" label:\"bug\" sort:updated-desc repo:acme/web"
        )
    );
}

#[tokio::test]
async fn falls_back_for_a_repository_the_index_does_not_cover_under_a_checks_filter() {
    let harness = Harness::new();
    harness.once(empty_search());
    harness.once_body(listed_pull_requests(
        2,
        1,
        |number| json!({ "checks": if number == 1 { "SUCCESS" } else { "FAILURE" } }),
    ));
    let mut input = open_all(10);
    input.filters = Some(PullRequestListFilters {
        checks: Some(PullRequestChecksFilter::Passing),
        ..filters()
    });
    let batch = harness.cli.list_pull_requests(&input).await.unwrap();
    // The fallback's rows carry `checksState` exactly as a search's rows do, so `checks` is now a
    // filter the fallback judges itself, the same as `draft`: an empty search answer under it is
    // still ambiguous, and the row picked out afterwards is the one whose own `checksState` reads
    // "passing".
    assert_eq!(harness.search_of_call(1), None);
    assert_eq!(numbers(&batch), vec![1]);
}

#[tokio::test]
async fn fails_a_checks_filter_for_a_row_whose_checks_are_still_pending() {
    let harness = Harness::new();
    harness.once(empty_search());
    harness.once_body(listed_pull_requests(
        1,
        1,
        |_| json!({ "checks": "PENDING" }),
    ));
    let mut input = open_all(10);
    input.filters = Some(PullRequestListFilters {
        checks: Some(PullRequestChecksFilter::Passing),
        ..filters()
    });
    let batch = harness.cli.list_pull_requests(&input).await.unwrap();
    // Pending equals neither "passing" nor "failing", so it satisfies neither filter value — the
    // same row would also be dropped by `checks: "failing"`.
    assert!(batch.items.is_empty());
}

#[tokio::test]
async fn falls_back_for_a_repository_the_index_does_not_cover_even_under_a_judgeable_filter() {
    let harness = Harness::new();
    harness.once(empty_search());
    harness.once_body(listed_pull_requests(
        2,
        1,
        |number| json!({ "isDraft": number == 1 }),
    ));
    let mut input = open_all(10);
    input.filters = Some(PullRequestListFilters {
        draft: Some(PullRequestDraftFilter::Hide),
        ..filters()
    });
    let batch = harness.cli.list_pull_requests(&input).await.unwrap();
    // `draft` is a filter the fallback can judge over its own rows just as search judges it, so
    // an empty search answer under it alone is still ambiguous between "nothing matches" and
    // "this repository is not indexed" — and the fallback applies the filter itself, keeping only
    // the non-draft row.
    assert_eq!(harness.search_of_call(1), None);
    assert_eq!(numbers(&batch), vec![2]);
}

#[tokio::test]
async fn carries_the_further_narrowings_into_a_batched_search() {
    let harness = Harness::new();
    harness.always(search_page(vec![], false));
    let mut input = search_input(
        PullRequestListState::Open,
        PullRequestInvolvement::All,
        &["acme/web"],
        10,
    );
    input.filters = Some(PullRequestListFilters {
        draft: Some(PullRequestDraftFilter::Only),
        review: Some(PullRequestReviewFilter::None),
        labels: Some(vec![vec![qv("bug")]]),
        ..filters()
    });
    harness.cli.search_pull_requests(&input).await.unwrap();
    assert_eq!(
        harness.search_of_call(0).as_deref(),
        Some("is:pr is:open label:\"bug\" draft:true review:none sort:updated-desc repo:acme/web")
    );
}

#[tokio::test]
async fn quotes_a_search_so_it_cannot_add_a_qualifier_or_a_flag_of_its_own() {
    let harness = Harness::new();
    harness.on(empty_listing);
    let mut input = open_all(10);
    input.query = Some("-- is:merged label:secret \"widen me\"".into());
    harness.cli.list_pull_requests(&input).await.unwrap();
    // Every word stays inside one phrase: nothing before it, nothing after it, and the leading
    // dashes are text rather than the start of another argument.
    assert_eq!(
        harness.search_of_call(0).as_deref(),
        Some(
            r#"is:pr is:open "-- is:merged label:secret \"widen me\"" sort:updated-desc repo:acme/web"#
        )
    );
}

#[tokio::test]
async fn escapes_a_backslash_before_the_quote_it_would_otherwise_let_out() {
    let harness = Harness::new();
    harness.on(empty_listing);
    let mut input = open_all(10);
    input.query = Some(r#"a\" is:merged"#.into());
    harness.cli.list_pull_requests(&input).await.unwrap();
    // GitHub reads `\\` as one backslash and `\"` as one quote, so the phrase ends where this
    // says it does; escaping the quote alone would have closed it early.
    assert_eq!(
        harness.search_of_call(0).as_deref(),
        Some(r#"is:pr is:open "a\\\" is:merged" sort:updated-desc repo:acme/web"#)
    );
}

#[tokio::test]
async fn asks_for_nothing_but_the_order_when_the_reader_typed_only_spaces() {
    let harness = Harness::new();
    harness.on(empty_listing);
    let mut input = open_all(10);
    input.query = Some("   ".into());
    harness.cli.list_pull_requests(&input).await.unwrap();
    // An empty phrase would match nothing rather than everything, so it is left out; the order
    // the page reads rows in is asked for whether or not anything was typed.
    assert_eq!(
        harness.search_of_call(0).as_deref(),
        Some("is:pr is:open sort:updated-desc repo:acme/web")
    );
}

#[tokio::test]
async fn carries_on_from_the_instant_the_last_slice_ended_on() {
    let harness = Harness::new();
    harness.always(ok(pull_requests(3, 1, no_overrides)));
    let mut input = open_all(10);
    input.cursor = Some(ProviderListCursor {
        updated_before: "2026-07-02T00:00:00Z".into(),
        delivered: 10,
    });
    let batch = harness.cli.list_pull_requests(&input).await.unwrap();
    // Inclusive, so the rows already sent at that instant come back for the caller to drop —
    // which is what keeps the ones beside them from being skipped.
    assert_eq!(
        harness.search_of_call(0).as_deref(),
        Some("is:pr is:open updated:<=2026-07-02T00:00:00Z sort:updated-desc repo:acme/web")
    );
    assert!(batch.continues);
}

#[tokio::test]
async fn answers_a_search_that_found_nothing_with_nothing_not_with_the_whole_repository() {
    // The fallback is for a repository the index does not cover. Under a text search an empty
    // answer means the text matched nothing, and listing everything instead would fill the page
    // with rows the reader did not search for.
    let harness = Harness::new();
    harness.once(empty_search());
    let mut input = open_all(10);
    input.query = Some("fdsfklj".into());
    let batch = harness.cli.list_pull_requests(&input).await.unwrap();
    assert!(batch.items.is_empty());
    assert_eq!(harness.call_count(), 1);
}

#[tokio::test]
async fn reads_a_repository_github_will_not_search_from_its_own_list() {
    // GitHub answers for a repository outside its search index with no rows and no error.
    let harness = Harness::new();
    harness.once(empty_search());
    harness.once_body(listed_pull_requests(3, 1, |_| json!({ "state": "CLOSED" })));
    let batch = harness
        .cli
        .list_pull_requests(&list_input(
            PullRequestListState::Closed,
            PullRequestInvolvement::All,
            10,
        ))
        .await
        .unwrap();
    assert_eq!(batch.items.len(), 3);
    // The fallback itself uses no search, then narrows the decoded rows locally. They arrive
    // newest-created first, so nothing can carry on from them.
    assert_eq!(harness.search_of_call(1), None);
    let vars = harness.vars_at(1);
    assert_eq!(vars["owner"], "acme");
    assert_eq!(vars["name"], "web");
    assert_eq!(vars["states"], json!(["CLOSED", "MERGED"]));
    assert!(!batch.continues);
}

#[tokio::test]
async fn keeps_state_and_involvement_filters_on_the_search_free_fallback() {
    let harness = Harness::new();
    harness.once(empty_search());
    harness.once_body(listed_pull_requests(4, 1, |number| {
        let mut row = json!({
            "state": if number == 4 { "OPEN" } else { "CLOSED" },
            "reviewRequests": if number == 2 {
                json!([{ "slug": "platform", "name": "Platform" }])
            } else {
                json!([{ "login": "bilal" }])
            },
        });
        if number == 3 {
            row["mergedAt"] = json!("2026-07-03T00:00:00Z");
        }
        row
    }));
    let batch = harness
        .cli
        .list_pull_requests(&list_input(
            PullRequestListState::Closed,
            PullRequestInvolvement::Reviewing,
            10,
        ))
        .await
        .unwrap();
    // Individual requests for this viewer and team requests survive. The fallback cannot resolve
    // team membership, so dropping team-routed reviews would hide legitimate work.
    assert_eq!(numbers(&batch), vec![1, 2]);
    assert_eq!(harness.search_of_call(1), None);
    assert!(!batch.continues);
}

#[tokio::test]
async fn grows_the_search_free_fallback_until_it_fills_the_filtered_page() {
    let unrelated = || json!({ "reviewRequests": [{ "login": "somebody-else" }] });
    let harness = Harness::new();
    harness.once(empty_search());
    harness.once_body(listed_pull_requests(3, 1, |_| unrelated()));
    harness.once_body(listed_pull_requests(4, 1, |number| {
        if number == 4 {
            json!({ "reviewRequests": [{ "login": "bilal" }] })
        } else {
            unrelated()
        }
    }));
    let batch = harness
        .cli
        .list_pull_requests(&ListPullRequestsInput {
            limit: 2,
            ..list_input(
                PullRequestListState::Open,
                PullRequestInvolvement::Reviewing,
                2,
            )
        })
        .await
        .unwrap();
    assert_eq!(numbers(&batch), vec![4]);
    assert!(harness.query_at(1).contains("first: 3,"));
    assert!(harness.query_at(2).contains("first: 6,"));
    assert!(!batch.truncated);
}

#[tokio::test]
async fn bounds_a_sparse_search_free_fallback_and_reports_the_unread_tail() {
    // A repository with far more pull requests than the bound, none of them for this reader.
    let harness = Harness::new();
    let listed = std::sync::Arc::new(parking_lot::Mutex::new(0u64));
    {
        let listed = listed.clone();
        harness.on(move |call| {
            if !call.is_graphql() || !call.query().contains("pullRequests(") {
                return empty_search();
            }
            let first: u64 = regex::Regex::new(r"first: (\d+)")
                .unwrap()
                .captures(call.query())
                .unwrap()[1]
                .parse()
                .unwrap();
            let mut listed = listed.lock();
            let nodes = rows(
                first,
                *listed + 1,
                |_| json!({ "reviewRequests": [{ "login": "somebody-else" }] }),
            );
            *listed += first;
            ok_json(json!({ "data": { "repository": { "pullRequests": {
                "pageInfo": { "hasNextPage": true, "endCursor": format!("c{}", *listed) },
                "nodes": nodes,
            } } } }))
        });
    }
    let batch = harness
        .cli
        .list_pull_requests(&list_input(
            PullRequestListState::Open,
            PullRequestInvolvement::Reviewing,
            2,
        ))
        .await
        .unwrap();
    // The last widening reads a thousand rows, a page of a hundred at a time, and stops there.
    let last = harness.call_count() - 1;
    let listed = *listed.lock();
    assert_eq!(
        harness.vars_at(last)["after"],
        json!(format!("c{}", listed - 100))
    );
    assert_eq!(listed - (3 + 6 + 12 + 24 + 48 + 96 + 192 + 384 + 768), 1000);
    assert_eq!(batch.items.len(), 0);
    assert!(batch.truncated);
}

#[tokio::test]
async fn takes_an_empty_slice_for_a_repository_that_has_run_out_not_one_to_read_again() {
    let harness = Harness::new();
    harness.on(empty_listing);
    let mut input = open_all(10);
    input.cursor = Some(ProviderListCursor {
        updated_before: "2026-07-02T00:00:00Z".into(),
        delivered: 10,
    });
    harness.cli.list_pull_requests(&input).await.unwrap();
    // A repository that answered the search once answers it again, so an empty slice under a
    // cursor is the end of it rather than a repository search cannot reach.
    assert_eq!(harness.call_count(), 1);
}

#[tokio::test]
async fn names_the_host_on_every_repository_it_addresses() {
    let harness = Harness::new();
    harness.on(empty_listing);
    let mut input = open_all(10);
    input.host = "github.acme.dev".into();
    harness.cli.list_pull_requests(&input).await.unwrap();
    // A request sent to github.com would read a different repository of the same name.
    assert!(harness.call_count() > 0);
    let hosts: std::collections::HashSet<_> = harness
        .calls()
        .iter()
        .map(|call| call.host().to_owned())
        .collect();
    assert_eq!(hosts, ["github.acme.dev".to_owned()].into_iter().collect());
    assert!(harness.search_of_call(0).unwrap().contains("repo:acme/web"));
}
