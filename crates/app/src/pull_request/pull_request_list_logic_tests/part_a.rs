#![allow(unused_imports)]
use super::super::*;

use super::fixtures::*;
use pretty_assertions::assert_eq;
use serde_json::json;

/// The row numbers `matchesPullRequestFilters` keeps, in order.
fn kept_numbers<E: ListEntry>(
    rows: &[E],
    filters: &PullRequestListFilters,
    viewer: Option<&str>,
) -> Vec<u64> {
    rows.iter()
        .filter(|&row| matches_pull_request_filters(row, filters, viewer))
        .map(|row| row.listing().number.get())
        .collect()
}

fn involvement_rows() -> Vec<EnvironmentPullRequestEntry> {
    vec![
        listed(json!({"number": 1, "author": {"login": "Bilal", "name": null, "avatarUrl": null}})),
        listed(json!({"number": 2, "viewerReviewRequested": true})),
        listed(json!({"number": 3})),
    ]
}

fn carried_rows() -> Vec<EnvironmentPullRequestEntry> {
    vec![
        listed(json!({"number": 1})),
        listed(json!({"number": 2, "state": "merged"})),
        listed(json!({"number": 3, "state": "closed"})),
        listed(json!({"number": 4, "host": "github.acme.dev"})),
        listed(json!({"number": 5, "projectId": "project-2"})),
    ]
}

fn narrowing_rows() -> Vec<EnvironmentPullRequestEntry> {
    vec![
        listed(json!({"number": 1})),
        listed(json!({"number": 2, "isDraft": true})),
        listed(json!({"number": 3, "reviewDecision": "approved"})),
        listed(json!({
            "number": 4,
            "labels": [{"name": "Needs design", "color": null}],
            "author": {"login": "Hubot", "name": null, "avatarUrl": null},
            "reviewDecision": "changes-requested",
        })),
    ]
}

#[test]
fn reuses_counts_supplied_by_the_listing_and_only_requests_missing_counts() {
    let entries = vec![
        listed(json!({"number": 1, "additions": 12, "deletions": 0})),
        listed(json!({"number": 2, "additions": 0, "deletions": 7})),
        listed(json!({"number": 3, "additions": 0, "deletions": 0})),
    ];
    let entries_by_key = by_key(&entries);
    let keys = pull_request_stats_keys_to_request(
        &entries_by_key,
        &keys_of(&entries),
        &[],
        &PullRequestDiffStats::new(),
    );
    let batches = pull_request_stats_batches(&entries_by_key, &keys);
    assert_eq!(batches[0].input.refs, vec![stats_ref(3)]);
}

#[test]
fn does_not_request_a_row_again_after_its_received_batch_is_pruned() {
    let entries = vec![
        listed(json!({"additions": 0, "deletions": 0, "number": 1})),
        listed(json!({"additions": 0, "deletions": 0, "number": 2})),
    ];
    let entries_by_key = by_key(&entries);
    let first_key = pull_request_entry_key_of(&entries[0]);
    let second_key = pull_request_entry_key_of(&entries[1]);
    let completed_stats = merge_pull_request_diff_stats(
        &PullRequestDiffStats::new(),
        &[line_stat(ENV_1, "project-1", 1, 1, 1)],
    );

    let keys = pull_request_stats_keys_to_request(
        &entries_by_key,
        &IndexSet::from([first_key, second_key.clone()]),
        &[],
        &completed_stats,
    );
    assert_eq!(keys.iter().cloned().collect::<Vec<_>>(), vec![second_key]);
    assert_eq!(
        batch_numbers(&pull_request_stats_batches(&entries_by_key, &keys)),
        vec![2]
    );
}

#[test]
fn drops_historical_rows_after_a_long_scroll_so_refresh_stays_bounded_to_the_viewport() {
    let entries: Vec<_> = (1..=500)
        .map(|number| listed(json!({"additions": 0, "deletions": 0, "number": number})))
        .collect();
    let entries_by_key = by_key(&entries);
    let batches: Vec<PullRequestStatsBatch> = entries
        .iter()
        .map(|entry| {
            pull_request_stats_batches(
                &entries_by_key,
                &IndexSet::from([pull_request_entry_key_of(entry)]),
            )
            .remove(0)
        })
        .collect();
    let visible_keys = keys_of(&entries[488..]);

    let retained = retain_visible_pull_request_stats_batches(&batches, &visible_keys);
    assert_eq!(retained.len(), 12);
    assert_eq!(batch_numbers(&retained), (489..=500).collect::<Vec<u64>>());
}

#[test]
fn keeps_every_per_environment_batch_within_the_stats_contract_limit() {
    let entries: Vec<_> = (1..=501)
        .map(|number| listed(json!({"additions": 0, "deletions": 0, "number": number})))
        .collect();
    let batches = pull_request_stats_batches(&by_key(&entries), &keys_of(&entries));

    assert_eq!(
        batches
            .iter()
            .map(|batch| batch.input.refs.len())
            .collect::<Vec<_>>(),
        vec![500, 1]
    );
    assert_eq!(
        batches
            .iter()
            .flat_map(|batch| batch.keys.iter().cloned())
            .collect::<Vec<_>>(),
        entry_keys(&entries)
    );
}

#[test]
fn selects_visible_rows_for_date_modes_and_every_uncached_row_for_size_modes() {
    let entries = vec![
        listed(json!({"additions": 0, "deletions": 0, "number": 1})),
        listed(json!({"additions": 0, "deletions": 0, "number": 2})),
        listed(json!({"additions": 0, "deletions": 0, "number": 3, "environmentId": "env-2"})),
    ];
    let entries_by_key = by_key(&entries);
    let cached_stats = merge_pull_request_diff_stats(
        &PullRequestDiffStats::new(),
        &[line_stat(ENV_1, "project-1", 1, 1, 1)],
    );

    let visible = pull_request_stats_request_batches(PullRequestStatsRequest {
        entries_by_key: &entries_by_key,
        candidate_keys: &keys_of(&entries[..2]),
        policy: PullRequestStatsPolicy::Visible,
        active_batches: &[],
        stats_by_row: &cached_stats,
        refresh: false,
    });
    assert_eq!(batch_numbers(&visible), vec![2]);

    let eager = pull_request_stats_request_batches(PullRequestStatsRequest {
        entries_by_key: &entries_by_key,
        candidate_keys: &keys_of(&entries[..1]),
        policy: PullRequestStatsPolicy::Eager,
        active_batches: &[],
        stats_by_row: &cached_stats,
        refresh: false,
    });
    assert_eq!(
        eager
            .iter()
            .map(|batch| batch.environment_id.as_str())
            .collect::<Vec<_>>(),
        vec![ENV_1, "env-2"]
    );
    assert_eq!(batch_numbers(&eager), vec![2, 3]);
}

#[test]
fn refreshes_only_visible_rows_unless_size_sorting_needs_every_loaded_row() {
    let entries = vec![
        listed(json!({"additions": 0, "deletions": 0, "number": 1})),
        listed(json!({"additions": 0, "deletions": 0, "number": 2})),
        listed(json!({"additions": 0, "deletions": 0, "number": 3})),
    ];
    let entries_by_key = by_key(&entries);
    let visible_keys = keys_of(&entries[1..2]);
    let stats: Vec<EnvironmentPullRequestStat> = entries
        .iter()
        .map(|item| {
            line_stat(
                ENV_1,
                "project-1",
                item.number.get(),
                item.additions,
                item.deletions,
            )
        })
        .collect();
    let cached_stats = merge_pull_request_diff_stats(&PullRequestDiffStats::new(), &stats);

    let visible = pull_request_stats_request_batches(PullRequestStatsRequest {
        entries_by_key: &entries_by_key,
        candidate_keys: &visible_keys,
        policy: PullRequestStatsPolicy::Visible,
        active_batches: &[],
        stats_by_row: &cached_stats,
        refresh: true,
    });
    assert_eq!(batch_numbers(&visible), vec![2]);

    let eager = pull_request_stats_request_batches(PullRequestStatsRequest {
        entries_by_key: &entries_by_key,
        candidate_keys: &visible_keys,
        policy: PullRequestStatsPolicy::Eager,
        active_batches: &[],
        stats_by_row: &cached_stats,
        refresh: true,
    });
    assert_eq!(batch_numbers(&eager), vec![1, 2, 3]);
}

#[test]
fn ignores_a_late_refresh_after_the_filter_or_stats_policy_changes() {
    let entries = vec![
        listed(json!({"additions": 0, "deletions": 0, "number": 1})),
        listed(json!({"additions": 0, "deletions": 0, "number": 2})),
    ];
    let entries_by_key = by_key(&entries);
    let visible_keys = keys_of(&entries[1..]);
    let requested_scope = stats_scope("open", PullRequestStatsPolicy::Visible);
    let refresh = |current: &PullRequestStatsScope| {
        pull_request_stats_refresh_batches(
            &requested_scope,
            current,
            &entries_by_key,
            &visible_keys,
            &PullRequestDiffStats::new(),
        )
    };

    let refreshed = refresh(&requested_scope).expect("the same scope still refreshes");
    assert_eq!(batch_numbers(&refreshed), vec![2]);
    assert!(refresh(&stats_scope("closed", PullRequestStatsPolicy::Visible)).is_none());
    assert!(refresh(&stats_scope("open", PullRequestStatsPolicy::Eager)).is_none());
}

#[test]
fn does_not_add_request_batches_again_while_rows_are_active_or_cached() {
    let entries = vec![
        listed(json!({"additions": 0, "deletions": 0, "number": 1})),
        listed(json!({"additions": 0, "deletions": 0, "number": 2})),
    ];
    let entries_by_key = by_key(&entries);
    let first = pull_request_stats_request_batches(PullRequestStatsRequest {
        entries_by_key: &entries_by_key,
        candidate_keys: &IndexSet::new(),
        policy: PullRequestStatsPolicy::Eager,
        active_batches: &[],
        stats_by_row: &PullRequestDiffStats::new(),
        refresh: false,
    });

    assert!(
        pull_request_stats_request_batches(PullRequestStatsRequest {
            entries_by_key: &entries_by_key,
            candidate_keys: &IndexSet::new(),
            policy: PullRequestStatsPolicy::Eager,
            active_batches: &first,
            stats_by_row: &PullRequestDiffStats::new(),
            refresh: false,
        })
        .is_empty()
    );

    let stats: Vec<EnvironmentPullRequestStat> = entries
        .iter()
        .map(|item| {
            line_stat(
                ENV_1,
                "project-1",
                item.number.get(),
                item.additions,
                item.deletions,
            )
        })
        .collect();
    let cached_stats = merge_pull_request_diff_stats(&PullRequestDiffStats::new(), &stats);
    assert!(
        pull_request_stats_request_batches(PullRequestStatsRequest {
            entries_by_key: &entries_by_key,
            candidate_keys: &keys_of(&entries),
            policy: PullRequestStatsPolicy::Visible,
            active_batches: &[],
            stats_by_row: &cached_stats,
            refresh: false,
        })
        .is_empty()
    );
}

#[test]
fn matches_the_viewers_own_pull_requests_case_insensitively() {
    let entries = involvement_rows();
    assert_eq!(
        numbers_of(&filter_pull_requests_by_involvement(
            &entries,
            &bilal_viewers(),
            PullRequestInvolvement::Authored
        )),
        vec![1]
    );
}

#[test]
fn returns_nothing_for_authored_when_the_viewer_is_unknown() {
    let entries = involvement_rows();
    assert!(
        filter_pull_requests_by_involvement(
            &entries,
            &no_viewers(),
            PullRequestInvolvement::Authored
        )
        .is_empty()
    );
}

#[test]
fn uses_the_server_computed_review_request_flag_for_reviewing() {
    let entries = involvement_rows();
    assert_eq!(
        numbers_of(&filter_pull_requests_by_involvement(
            &entries,
            &bilal_viewers(),
            PullRequestInvolvement::Reviewing
        )),
        vec![2]
    );
}

#[test]
fn does_not_treat_a_matching_login_on_another_host_as_the_viewer() {
    // The same name on GitLab is a different account, and its viewer is unknown here.
    let mixed = vec![
        listed(json!({"number": 1, "author": {"login": "Bilal", "name": null, "avatarUrl": null}})),
        listed(json!({
            "number": 2,
            "provider": "gitlab",
            "host": "gitlab.com",
            "author": {"login": "Bilal", "name": null, "avatarUrl": null},
        })),
    ];
    assert_eq!(
        numbers_of(&filter_pull_requests_by_involvement(
            &mixed,
            &bilal_viewers(),
            PullRequestInvolvement::Authored
        )),
        vec![1]
    );
}

#[test]
fn leaves_the_superset_untouched_for_all() {
    let entries = involvement_rows();
    assert_eq!(
        filter_pull_requests_by_involvement(
            &entries,
            &bilal_viewers(),
            PullRequestInvolvement::All
        )
        .len(),
        3
    );
}

#[test]
fn keeps_two_hosts_of_one_provider_kind_as_two_accounts() {
    // A GitHub Enterprise install is a different account from github.com, so the viewer for
    // one must not claim authorship of the other's change requests.
    let mixed = vec![
        listed(json!({"number": 1, "author": {"login": "Bilal", "name": null, "avatarUrl": null}})),
        listed(json!({
            "number": 2,
            "host": "github.acme.dev",
            "author": {"login": "Bilal", "name": null, "avatarUrl": null},
        })),
    ];
    assert_eq!(
        numbers_of(&filter_pull_requests_by_involvement(
            &mixed,
            &bilal_viewers(),
            PullRequestInvolvement::Authored
        )),
        vec![1]
    );
}

#[test]
fn buckets_authored_above_review_requested_and_drops_empty_groups() {
    let groups = group_pull_requests_by_involvement(
        &[
            listed(
                json!({"number": 1, "author": {"login": "bilal", "name": null, "avatarUrl": null}}),
            ),
            listed(json!({"number": 2, "viewerReviewRequested": true})),
        ],
        &bilal_viewers(),
    );
    assert_eq!(
        groups
            .iter()
            .map(|group| (group.key, group.entries.len()))
            .collect::<Vec<_>>(),
        vec![
            (PullRequestGroupKey::Authored, 1),
            (PullRequestGroupKey::ReviewRequested, 1),
        ]
    );
}

#[test]
fn puts_everything_in_others_when_the_viewer_is_unknown() {
    let groups = group_pull_requests_by_involvement(&[listed(json!({"number": 1}))], &no_viewers());
    assert_eq!(
        groups.iter().map(|group| group.key).collect::<Vec<_>>(),
        vec![PullRequestGroupKey::Others]
    );
}

#[test]
fn counts_an_authored_pull_request_once_even_with_a_review_request_on_it() {
    let groups = group_pull_requests_by_involvement(
        &[listed(json!({
            "number": 1,
            "author": {"login": "bilal", "name": null, "avatarUrl": null},
            "viewerReviewRequested": true,
        }))],
        &bilal_viewers(),
    );
    assert_eq!(
        groups.iter().map(|group| group.key).collect::<Vec<_>>(),
        vec![PullRequestGroupKey::Authored]
    );
}

#[test]
fn matches_the_number_with_or_without_the_leading_hash() {
    let target = listed(json!({
        "number": 4711,
        "title": "Restore sidebar actions",
        "headBranch": "fix/sidebar",
    }));
    assert!(matches_pull_request_query(&target, "#4711"));
    assert!(matches_pull_request_query(&target, "4711"));
}

#[test]
fn matches_title_branch_and_author_case_insensitively() {
    let target = listed(json!({
        "number": 4711,
        "title": "Restore sidebar actions",
        "headBranch": "fix/sidebar",
    }));
    assert!(matches_pull_request_query(&target, "SIDEBAR"));
    assert!(matches_pull_request_query(&target, "fix/side"));
    assert!(matches_pull_request_query(&target, "octocat"));
}

#[test]
fn ignores_surrounding_whitespace_and_rejects_non_matches() {
    let target = listed(json!({
        "number": 4711,
        "title": "Restore sidebar actions",
        "headBranch": "fix/sidebar",
    }));
    assert!(matches_pull_request_query(&target, "   "));
    assert!(!matches_pull_request_query(&target, "kanban"));
}

#[test]
fn never_lets_a_merged_pull_request_sit_under_open() {
    let rows = carried_rows();
    assert_eq!(
        numbers_of(&narrow_pull_requests_to_filters(
            &rows,
            PullRequestListState::Open,
            None,
            None
        )),
        vec![1, 4, 5]
    );
}

#[test]
fn keeps_only_the_state_that_was_asked_for() {
    let rows = carried_rows();
    assert_eq!(
        numbers_of(&narrow_pull_requests_to_filters(
            &rows,
            PullRequestListState::Merged,
            None,
            None
        )),
        vec![2]
    );
}

#[test]
fn narrows_to_one_host_which_two_installs_of_one_kind_cannot_share() {
    let rows = carried_rows();
    assert_eq!(
        numbers_of(&narrow_pull_requests_to_filters(
            &rows,
            PullRequestListState::All,
            None,
            Some("github.acme.dev")
        )),
        vec![4]
    );
}

#[test]
fn narrows_to_one_project() {
    let rows = carried_rows();
    assert_eq!(
        numbers_of(&narrow_pull_requests_to_filters(
            &rows,
            PullRequestListState::All,
            Some("project-2"),
            None
        )),
        vec![5]
    );
}

#[test]
fn holds_on_to_everything_when_nothing_narrows_it() {
    let rows = carried_rows();
    assert_eq!(
        narrow_pull_requests_to_filters(&rows, PullRequestListState::All, None, None),
        rows
    );
}

#[test]
fn keeps_or_drops_drafts_as_asked() {
    let rows = narrowing_rows();
    assert_eq!(
        kept_numbers(&rows, &list_filters(json!({"draft": "only"})), None),
        vec![2]
    );
    assert_eq!(
        kept_numbers(&rows, &list_filters(json!({"draft": "hide"})), None),
        vec![1, 3, 4]
    );
}

#[test]
fn keeps_the_review_state_that_was_asked_for_and_no_reviews_means_none_at_all() {
    let rows = narrowing_rows();
    assert_eq!(
        kept_numbers(&rows, &list_filters(json!({"review": "approved"})), None),
        vec![3]
    );
    assert_eq!(
        kept_numbers(
            &rows,
            &list_filters(json!({"review": "changes-requested"})),
            None
        ),
        vec![4]
    );
    assert_eq!(
        kept_numbers(&rows, &list_filters(json!({"review": "none"})), None),
        vec![1, 2]
    );
}

#[test]
fn matches_labels_and_authors_however_either_was_capitalized() {
    let rows = narrowing_rows();
    assert_eq!(
        kept_numbers(
            &rows,
            &list_filters(json!({"labels": [["needs DESIGN"]]})),
            None
        ),
        vec![4]
    );
    assert_eq!(
        kept_numbers(
            &rows,
            &list_filters(json!({"excludedLabels": ["needs design"]})),
            None
        ),
        vec![1, 2, 3]
    );
    assert_eq!(
        kept_numbers(&rows, &list_filters(json!({"author": "hubot"})), None),
        vec![4]
    );
}

#[test]
fn resolves_author_me_to_the_given_viewer_case_insensitively() {
    let mine = listed(json!({
        "number": 30,
        "author": {"login": "Bilal", "name": null, "avatarUrl": null},
    }));
    let theirs = listed(json!({
        "number": 31,
        "author": {"login": "Hubot", "name": null, "avatarUrl": null},
    }));
    let filters = list_filters(json!({"author": "me"}));
    assert!(matches_pull_request_filters(&mine, &filters, Some("bilal")));
    assert!(!matches_pull_request_filters(
        &theirs,
        &filters,
        Some("bilal")
    ));
}

#[test]
fn resolves_author_at_me_the_same_way_as_author_me() {
    let mine = listed(json!({
        "number": 32,
        "author": {"login": "Bilal", "name": null, "avatarUrl": null},
    }));
    let theirs = listed(json!({
        "number": 33,
        "author": {"login": "Hubot", "name": null, "avatarUrl": null},
    }));
    let filters = list_filters(json!({"author": "@me"}));
    assert!(matches_pull_request_filters(&mine, &filters, Some("Bilal")));
    assert!(!matches_pull_request_filters(
        &theirs,
        &filters,
        Some("Bilal")
    ));
}

#[test]
fn compares_author_me_against_the_literal_name_when_there_is_no_viewer() {
    let literal_me = listed(json!({
        "number": 34,
        "author": {"login": "me", "name": null, "avatarUrl": null},
    }));
    let someone_else = listed(json!({
        "number": 35,
        "author": {"login": "Bilal", "name": null, "avatarUrl": null},
    }));
    let filters = list_filters(json!({"author": "me"}));
    assert!(matches_pull_request_filters(&literal_me, &filters, None));
    assert!(!matches_pull_request_filters(&someone_else, &filters, None));
}

#[test]
fn takes_a_group_of_labels_as_an_either_or() {
    let sized = vec![
        listed(json!({"number": 10, "labels": [{"name": "size:S", "color": null}]})),
        listed(json!({"number": 11, "labels": [{"name": "size:XS", "color": null}]})),
        listed(json!({"number": 12, "labels": [{"name": "size:L", "color": null}]})),
    ];
    assert_eq!(
        kept_numbers(
            &sized,
            &list_filters(json!({"labels": [["size:S", "size:XS"]]})),
            None
        ),
        vec![10, 11]
    );
    assert_eq!(
        kept_numbers(
            &sized,
            &list_filters(json!({"labels": [["size:XXL"]]})),
            None
        ),
        Vec::<u64>::new()
    );
}

#[test]
fn takes_two_groups_as_an_and_each_satisfied_on_its_own() {
    let row = listed(json!({
        "number": 20,
        "labels": [{"name": "size:S", "color": null}, {"name": "bug", "color": null}],
    }));
    assert!(matches_pull_request_filters(
        &row,
        &list_filters(json!({"labels": [["size:S", "size:XS"], ["bug"]]})),
        None
    ));
    // The second group holds nothing this row carries, so the first one satisfied is not enough.
    assert!(!matches_pull_request_filters(
        &row,
        &list_filters(json!({"labels": [["size:S"], ["wip"]]})),
        None
    ));
}

#[test]
fn holds_on_to_every_row_for_a_filter_no_row_carries_the_answer_to() {
    let rows = narrowing_rows();
    assert_eq!(
        kept_numbers(&rows, &list_filters(json!({"checks": "passing"})), None),
        vec![1, 2, 3, 4]
    );
}

#[test]
fn keeps_quoted_values_whole_and_separates_negation() {
    assert_eq!(
        parse_pull_request_query(r#"label:"needs design" -label:wip Fix the parser"#),
        ParsedPullRequestQuery {
            text: "Fix the parser".to_owned(),
            filters: list_filters(json!({
                "labels": [["needs design"]],
                "excludedLabels": ["wip"],
            })),
        }
    );
}

#[test]
fn reads_a_comma_separated_list_as_one_group_either_name_satisfies() {
    assert_eq!(
        parse_pull_request_query("label:size:S,size:XS")
            .filters
            .labels,
        list_filters(json!({"labels": [["size:S", "size:XS"]]})).labels
    );
}

#[test]
fn keeps_the_spaces_in_a_quoted_name_of_a_list_and_drops_an_empty_part() {
    assert_eq!(
        parse_pull_request_query(r#"label:"needs design","wip""#)
            .filters
            .labels,
        list_filters(json!({"labels": [["needs design", "wip"]]})).labels
    );
    assert_eq!(
        parse_pull_request_query("label:a,,b").filters.labels,
        list_filters(json!({"labels": [["a", "b"]]})).labels
    );
}

#[test]
fn excludes_every_name_of_a_negated_list() {
    assert_eq!(
        parse_pull_request_query("-label:a,b").filters,
        list_filters(json!({"excludedLabels": ["a", "b"]}))
    );
}

#[test]
fn makes_each_label_qualifier_its_own_group_so_the_groups_are_an_and() {
    assert_eq!(
        parse_pull_request_query("label:bug label:a,b")
            .filters
            .labels,
        list_filters(json!({"labels": [["bug"], ["a", "b"]]})).labels
    );
}

#[test]
fn reads_the_scalar_qualifiers_in_githubs_own_spelling() {
    assert_eq!(
        parse_pull_request_query(
            "author:octocat DRAFT:false review:changes_requested status:failure"
        )
        .filters,
        list_filters(json!({
            "author": "octocat",
            "draft": "hide",
            "review": "changes-requested",
            "checks": "failing",
        }))
    );
}

#[test]
fn leaves_an_unknown_value_and_a_stray_colon_as_text_and_reads_an_unknown_key_as_a_label() {
    let parsed = parse_pull_request_query("milestone:v2 draft:maybe status: parser");
    assert_eq!(
        parsed.filters,
        list_filters(json!({"labels": [["milestone:v2"]]}))
    );
    // A known key whose value it does not take is text, so "status:" itself stays findable.
    assert_eq!(parsed.text, "draft:maybe status: parser");
}

#[test]
fn reads_a_quoted_name_holding_a_comma_as_the_one_label_it_is() {
    assert_eq!(
        parse_pull_request_query(r#"label:"needs,triage""#)
            .filters
            .labels,
        list_filters(json!({"labels": [["needs,triage"]]})).labels
    );
    assert_eq!(
        parse_pull_request_query(r#"-label:"needs,triage""#)
            .filters
            .excluded_labels,
        list_filters(json!({"excludedLabels": ["needs,triage"]})).excluded_labels
    );
}

#[test]
fn cuts_a_typed_query_back_to_what_the_contract_carries() {
    let many = (0..14)
        .map(|index| format!("l{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let long = "x".repeat(240);
    let parsed = parse_pull_request_query(&format!("label:{many} label:{long} author:{long}"));
    let labels = parsed.filters.labels.expect("labels are present");
    assert_eq!(labels[0].len(), 10);
    assert_eq!(
        labels[1]
            .iter()
            .map(|value| value.as_str().to_owned())
            .collect::<Vec<_>>(),
        vec!["x".repeat(200)]
    );
    assert_eq!(
        parsed
            .filters
            .author
            .map(|author| author.as_str().to_owned()),
        Some("x".repeat(200))
    );
}

#[test]
fn cuts_a_namespaced_label_after_its_key_is_prefixed_on() {
    let parsed = parse_pull_request_query(&format!("size:{}", "x".repeat(240)));
    let expected = format!("size:{}", "x".repeat(240))[..200].to_owned();
    assert_eq!(
        parsed
            .filters
            .labels
            .expect("labels are present")
            .iter()
            .map(|group| group
                .iter()
                .map(|value| value.as_str().to_owned())
                .collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        vec![vec![expected]]
    );
}

#[test]
fn answers_an_empty_query_with_an_empty_everything() {
    assert_eq!(
        parse_pull_request_query("   "),
        ParsedPullRequestQuery {
            text: String::new(),
            filters: list_filters(json!({})),
        }
    );
}

#[test]
fn keeps_an_id_the_environment_has() {
    assert_eq!(
        resolve_project_scope(Some("p2"), &["p1", "p2"], true),
        Some("p2")
    );
}

#[test]
fn drops_an_id_from_another_environment() {
    assert_eq!(
        resolve_project_scope(Some("p9"), &["p1", "p2"], false),
        Some("p9")
    );
    assert_eq!(resolve_project_scope(Some("p9"), &["p1", "p2"], true), None);
}

#[test]
fn drops_an_id_once_the_environment_is_known_to_have_no_projects() {
    assert_eq!(resolve_project_scope(Some("p9"), &[], true), None);
}

#[test]
fn keeps_an_id_while_the_projects_are_still_unknown() {
    // Dropping here would list every project for a moment before narrowing back down.
    assert_eq!(resolve_project_scope(Some("p9"), &[], false), Some("p9"));
}

#[test]
fn puts_the_pull_request_whose_title_says_it_above_one_that_merely_mentions_it() {
    let titled = listed(json!({
        "number": 1,
        "title": "Add the welcome wizard",
        "updatedAt": "2026-07-01T00:00:00Z",
    }));
    let mentioned = listed(json!({
        "number": 2,
        "title": "Unrelated work",
        "updatedAt": "2026-08-01T00:00:00Z",
    }));
    assert_eq!(
        numbers_of(&rank_pull_request_matches(
            &[mentioned, titled],
            "welcome wizard"
        )),
        vec![1, 2]
    );
}

#[test]
fn finds_the_words_in_any_order_which_is_how_people_type_them() {
    // Substrings count, so "welcomes" answers "welcome" — a search is not a spelling test.
    assert_eq!(
        score_pull_request_match(
            &listed(json!({"title": "The wizard that welcomes"})),
            "welcome wizard"
        ),
        70
    );
    assert_eq!(
        score_pull_request_match(
            &listed(json!({"title": "Wizard for the welcome flow"})),
            "welcome wizard"
        ),
        70
    );
    // One of the two words is a mention, not an answer, and ranks under both.
    assert_eq!(
        score_pull_request_match(
            &listed(json!({"title": "A wizard, of sorts"})),
            "welcome wizard"
        ),
        30
    );
}

#[test]
fn answers_a_number_with_the_pull_request_that_has_it_and_nothing_else() {
    assert_eq!(
        score_pull_request_match(&listed(json!({"number": 42})), "#42"),
        100
    );
    assert_eq!(
        score_pull_request_match(&listed(json!({"number": 42})), "43"),
        0
    );
}

#[test]
fn keeps_recency_where_nothing_tells_two_results_apart() {
    let older = listed(json!({
        "number": 1,
        "title": "no match here",
        "updatedAt": "2026-07-01T00:00:00Z",
    }));
    let newer = listed(json!({
        "number": 2,
        "title": "none here either",
        "updatedAt": "2026-08-01T00:00:00Z",
    }));
    assert_eq!(
        numbers_of(&rank_pull_request_matches(&[older, newer], "wizard")),
        vec![2, 1]
    );
}

#[test]
fn leaves_a_listing_with_no_search_in_the_order_it_arrived() {
    let first = listed(json!({"number": 1, "updatedAt": "2026-07-01T00:00:00Z"}));
    let second = listed(json!({"number": 2, "updatedAt": "2026-08-01T00:00:00Z"}));
    assert_eq!(
        rank_pull_request_matches(&[first.clone(), second.clone()], "  "),
        vec![first, second]
    );
}

#[test]
fn puts_ready_work_first_finished_work_after_open_work_and_every_conflict_last() {
    let conflict = listed(json!({
        "number": 1,
        "mergeability": "conflicting",
        "checksState": "passing",
        "reviewDecision": "approved",
        "updatedAt": "2026-09-01T00:00:00Z",
    }));
    let other = listed(json!({
        "number": 2,
        "checksState": "pending",
        "updatedAt": "2026-08-01T00:00:00Z",
    }));
    let green = listed(json!({
        "number": 3,
        "checksState": "passing",
        "reviewDecision": "review-required",
        "updatedAt": "2026-07-01T00:00:00Z",
    }));
    let approved = listed(json!({
        "number": 4,
        "checksState": "passing",
        "reviewDecision": "approved",
        "additions": 1_000,
        "updatedAt": "2026-06-01T00:00:00Z",
    }));
    let draft = listed(json!({
        "number": 5,
        "isDraft": true,
        "checksState": "passing",
        "reviewDecision": "approved",
        "updatedAt": "2026-09-02T00:00:00Z",
    }));
    let finished = listed(json!({
        "number": 6,
        "state": "merged",
        "checksState": "passing",
        "reviewDecision": "approved",
        "updatedAt": "2026-09-03T00:00:00Z",
    }));

    assert_eq!(
        numbers_of(&rank_pull_requests_by_merge_readiness(
            &[conflict, other, green, approved, draft, finished],
            has_measured_size
        )),
        vec![4, 3, 5, 2, 6, 1]
    );
}

#[test]
fn uses_recency_when_readiness_and_diff_size_tie() {
    let older = listed(json!({"number": 1, "checksState": "passing"}));
    let newer = listed(json!({
        "number": 2,
        "checksState": "passing",
        "updatedAt": "2026-08-01T00:00:00Z",
    }));
    assert_eq!(
        numbers_of(&rank_pull_requests_by_merge_readiness(
            &[older, newer],
            has_measured_size
        )),
        vec![2, 1]
    );
}

#[test]
fn ranks_smaller_measured_diffs_first_within_a_readiness_tier_as_counts_arrive() {
    let larger = listed(json!({
        "number": 1,
        "checksState": "passing",
        "reviewDecision": "approved",
        "additions": 1,
        "deletions": 49,
        "updatedAt": "2026-09-01T00:00:00Z",
    }));
    let smaller = listed(json!({
        "number": 2,
        "checksState": "passing",
        "reviewDecision": "approved",
        "additions": 3,
        "deletions": 2,
        "updatedAt": "2026-08-01T00:00:00Z",
    }));
    let unknown = listed(json!({
        "number": 3,
        "checksState": "passing",
        "reviewDecision": "approved",
        "additions": 0,
        "deletions": 0,
        "updatedAt": "2026-09-02T00:00:00Z",
    }));

    assert_eq!(
        numbers_of(&rank_pull_requests_by_merge_readiness(
            &[larger.clone(), unknown.clone(), smaller.clone()],
            has_measured_size
        )),
        vec![2, 1, 3]
    );
    let mut counted = unknown;
    counted.additions = 1;
    counted.deletions = 0;
    assert_eq!(
        numbers_of(&rank_pull_requests_by_merge_readiness(
            &[larger, counted, smaller],
            has_measured_size
        )),
        vec![3, 2, 1]
    );
}

#[test]
fn distinguishes_measured_empty_diffs_from_missing_counts_when_sorting_groups() {
    let unknown = listed(json!({"number": 1, "additions": 0, "deletions": 0}));
    let measured = listed(json!({"number": 2}));
    let empty = listed(json!({"number": 3, "additions": 0, "deletions": 0}));
    let groups = vec![PullRequestGroup::new(
        PullRequestGroupKey::Others,
        vec![unknown, measured, empty],
    )];
    let measured_by_count = |row: &EnvironmentPullRequestEntry| row.number.get() != 1;

    let sorted = sort_pull_request_groups(
        &groups,
        PullRequestListSort::Ready,
        "",
        measured_by_count,
        PullRequestInvolvement::All,
    );
    assert_eq!(numbers_of(&sorted[0].entries), vec![3, 2, 1]);
    assert_eq!(
        sort_pull_request_groups(
            &groups,
            PullRequestListSort::Ready,
            "sidebar",
            measured_by_count,
            PullRequestInvolvement::All,
        ),
        groups
    );
}

#[test]
fn keeps_authored_work_first_and_ranks_each_group_by_readiness() {
    let authored_waiting = listed(json!({"number": 1, "checksState": "pending"}));
    let authored_ready = listed(json!({
        "number": 2,
        "checksState": "passing",
        "reviewDecision": "approved",
    }));
    let other_ready = listed(json!({
        "number": 3,
        "checksState": "passing",
        "reviewDecision": "approved",
    }));
    let sorted = sort_pull_request_groups(
        &[
            PullRequestGroup::new(
                PullRequestGroupKey::Authored,
                vec![authored_waiting, authored_ready],
            ),
            PullRequestGroup::new(PullRequestGroupKey::Others, vec![other_ready]),
        ],
        PullRequestListSort::Ready,
        "",
        has_measured_size,
        PullRequestInvolvement::All,
    );

    assert_eq!(
        sorted.iter().map(|group| group.key).collect::<Vec<_>>(),
        vec![PullRequestGroupKey::Authored, PullRequestGroupKey::Others]
    );
    assert_eq!(
        numbers_of(
            &sorted
                .iter()
                .flat_map(|group| group.entries.clone())
                .collect::<Vec<_>>()
        ),
        vec![2, 1, 3]
    );
}

/// Shared body of the `it.each` rows: authored work stays first, and `sort` orders each group.
fn authored_first_under_sort(sort: PullRequestListSort) -> Vec<u64> {
    let older_larger = listed(json!({
        "number": 1,
        "additions": 20,
        "createdAt": "2026-07-01T00:00:00Z",
        "updatedAt": "2026-08-01T00:00:00Z",
    }));
    let newer_smaller = listed(json!({
        "number": 2,
        "additions": 2,
        "createdAt": "2026-08-01T00:00:00Z",
        "updatedAt": "2026-07-01T00:00:00Z",
    }));
    let sorted = sort_pull_request_groups(
        &[
            PullRequestGroup::new(
                PullRequestGroupKey::Authored,
                vec![older_larger, newer_smaller],
            ),
            PullRequestGroup::new(
                PullRequestGroupKey::Others,
                vec![listed(json!({"number": 3}))],
            ),
        ],
        sort,
        "",
        has_measured_size,
        PullRequestInvolvement::All,
    );

    assert_eq!(
        sorted.iter().map(|group| group.key).collect::<Vec<_>>(),
        vec![PullRequestGroupKey::Authored, PullRequestGroupKey::Others]
    );
    numbers_of(&sorted[0].entries)
}

#[test]
fn keeps_authored_first_while_applying_the_updated_sort_inside_groups() {
    assert_eq!(
        authored_first_under_sort(PullRequestListSort::Updated),
        vec![1, 2]
    );
}

#[test]
fn keeps_authored_first_while_applying_the_newest_sort_inside_groups() {
    assert_eq!(
        authored_first_under_sort(PullRequestListSort::Newest),
        vec![2, 1]
    );
}

#[test]
fn keeps_authored_first_while_applying_the_oldest_sort_inside_groups() {
    assert_eq!(
        authored_first_under_sort(PullRequestListSort::Oldest),
        vec![1, 2]
    );
}

#[test]
fn keeps_authored_first_while_applying_the_largest_sort_inside_groups() {
    assert_eq!(
        authored_first_under_sort(PullRequestListSort::Largest),
        vec![1, 2]
    );
}

#[test]
fn keeps_authored_first_while_applying_the_smallest_sort_inside_groups() {
    assert_eq!(
        authored_first_under_sort(PullRequestListSort::Smallest),
        vec![2, 1]
    );
}
