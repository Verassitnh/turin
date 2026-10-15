#![allow(unused_imports)]
//! Part B of upstream pullRequestList.logic.test.ts: every `it` from the blocked-on-me ranking
//! describe at line 890 onward. The `it.each` over list states is one test with a loop, since the
//! four rows share one fixture and one body.
use super::super::*;
use super::fixtures::*;
use crate::pull_request::pull_request_detail_logic::MemorySnapshotStorage;
use crate::pull_request::pull_request_list_preferences::{
    PreferenceStorage, PullRequestListPreferences, pull_request_list_preferences,
    read_pull_request_list_preferences, write_pull_request_list_preferences,
};
use indexmap::IndexMap;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::io;
use turin_contracts::ProjectId;

/// A row the override tests key as `#<number>`, the way the page keys a row it has no repository
/// for.
fn hash_key(row: &EnvironmentPullRequestEntry) -> String {
    format!("#{}", row.number.get())
}

/// A row in the given state, for the override tests.
fn row(number: u64, state: &str) -> EnvironmentPullRequestEntry {
    listed(json!({ "number": number, "state": state, "isDraft": false, "labels": [] }))
}

fn override_at(
    state: PullRequestState,
    updated_at: &str,
    token: u64,
    at: i64,
) -> PullRequestListOverride {
    PullRequestListOverride {
        state,
        is_draft: None,
        updated_at: updated_at.to_owned(),
        token,
        at,
    }
}

fn authored_row(number: u64, updated_at: &str) -> EnvironmentPullRequestEntry {
    listed(json!({
        "number": number,
        "updatedAt": updated_at,
        "author": { "login": "Bilal", "name": null, "avatarUrl": null },
    }))
}

/// Upstream `data`: what the list held when it was last read, carrying the failure and cursor
/// that a cold start must not inherit.
fn held_list(entries: Vec<EnvironmentPullRequestEntry>) -> MergedPullRequestList {
    wire(json!({
        "viewers": { "github.com": "Bilal" },
        "providers": [],
        "entries": entries,
        "errors": [{
            "environmentId": "env-1",
            "projectId": "project-1",
            "projectTitle": "t3code",
            "message": "boom",
        }],
        "truncated": true,
        "nextCursors": { "env-1": { "pingdotgg/t3code": "cursor-1" } },
        "truncatedEnvironments": [],
    }))
}

fn save_snapshot(storage: &mut MemorySnapshotStorage, scope: &str, data: MergedPullRequestList) {
    write_pull_request_list_snapshot(
        Some(storage as &mut dyn SnapshotStorage),
        ENV_1,
        &PullRequestListSnapshot {
            scope: scope.to_owned(),
            data,
            partitions: None,
        },
    );
}

fn read_snapshot(
    storage: Option<&MemorySnapshotStorage>,
    env: &str,
) -> Option<PullRequestListSnapshot> {
    read_pull_request_list_snapshot(storage.map(|storage| storage as &dyn SnapshotStorage), env)
}

/// Browser storage that holds whatever it was given.
#[derive(Default)]
struct HeldPreferences(RefCell<BTreeMap<String, String>>);

impl PreferenceStorage for HeldPreferences {
    fn get_item(&self, key: &str) -> io::Result<Option<String>> {
        Ok(self.0.borrow().get(key).cloned())
    }
    fn set_item(&self, key: &str, value: &str) -> io::Result<()> {
        self.0.borrow_mut().insert(key.to_owned(), value.to_owned());
        Ok(())
    }
}

/// Browser policy that refuses storage: every read and write throws.
struct DeniedPreferences;

impl PreferenceStorage for DeniedPreferences {
    fn get_item(&self, _: &str) -> io::Result<Option<String>> {
        Err(io::Error::other("storage denied"))
    }
    fn set_item(&self, _: &str, _: &str) -> io::Result<()> {
        Err(io::Error::other("storage denied"))
    }
}

fn preferences(
    involvement: PullRequestInvolvement,
    state: PullRequestListState,
) -> PullRequestListPreferences {
    PullRequestListPreferences {
        involvement,
        state,
        ..Default::default()
    }
}

/// A project the way a server lists it: its id, and the environment that holds it.
struct ScopedProject {
    id: &'static str,
    environment_id: &'static str,
}

// blocked-on-me ranking

#[test]
fn ranks_authored_work_by_how_surely_it_is_the_authors_to_unblock_newest_first_within_a_tier() {
    let conflict = listed(json!({
        "number": 1,
        "mergeability": "conflicting",
        "checksState": "failing",
        "updatedAt": "2026-08-01T00:00:00Z",
    }));
    let older_failing = listed(json!({
        "number": 2,
        "checksState": "failing",
        "updatedAt": "2026-08-01T00:00:00Z",
    }));
    let changes_requested = listed(json!({
        "number": 3,
        "reviewDecision": "changes-requested",
        "checksState": "failing",
        "updatedAt": "2026-08-02T00:00:00Z",
    }));
    let approved =
        listed(json!({ "number": 4, "checksState": "passing", "reviewDecision": "approved" }));
    let draft = listed(json!({ "number": 5, "isDraft": true, "checksState": "passing" }));
    let waiting = listed(json!({ "number": 6 }));
    let merged =
        listed(json!({ "number": 7, "state": "merged", "updatedAt": "2026-08-01T00:00:00Z" }));
    let newer_closed =
        listed(json!({ "number": 8, "state": "closed", "updatedAt": "2026-08-03T00:00:00Z" }));
    let newer_failing = listed(json!({
        "number": 9,
        "checksState": "failing",
        "updatedAt": "2026-08-03T00:00:00Z",
    }));

    assert_eq!(
        numbers_of(&rank_pull_requests_blocked_on_author(&[
            waiting,
            merged,
            older_failing,
            draft,
            approved,
            changes_requested,
            newer_closed,
            conflict,
            newer_failing,
        ])),
        vec![1, 3, 9, 2, 5, 6, 4, 8, 7]
    );
}

#[test]
fn ranks_reviewer_work_with_open_rows_first_newest_first() {
    let older_open = listed(json!({ "number": 1, "updatedAt": "2026-08-01T00:00:00Z" }));
    let newer_open = listed(json!({ "number": 2, "updatedAt": "2026-08-02T00:00:00Z" }));
    let older_finished =
        listed(json!({ "number": 3, "state": "merged", "updatedAt": "2026-08-01T00:00:00Z" }));
    let newer_finished =
        listed(json!({ "number": 4, "state": "closed", "updatedAt": "2026-08-03T00:00:00Z" }));

    assert_eq!(
        numbers_of(&rank_pull_requests_blocked_on_reviewer(&[
            newer_finished,
            newer_open,
            older_finished,
            older_open,
        ])),
        vec![2, 1, 4, 3]
    );
}

#[test]
fn ranks_blocked_groups_according_to_their_involvement() {
    let authored = vec![
        listed(json!({ "number": 1, "isDraft": true })),
        listed(json!({ "number": 2, "checksState": "failing" })),
    ];
    let reviewing = vec![
        listed(json!({ "number": 3, "updatedAt": "2026-08-02T00:00:00Z" })),
        listed(json!({ "number": 4, "updatedAt": "2026-08-01T00:00:00Z" })),
    ];
    let others = vec![
        listed(json!({ "number": 5, "updatedAt": "2026-08-02T00:00:00Z" })),
        listed(json!({ "number": 6, "isDraft": true })),
    ];
    let groups = vec![
        PullRequestGroup::new(PullRequestGroupKey::Authored, authored),
        PullRequestGroup::new(PullRequestGroupKey::ReviewRequested, reviewing),
        PullRequestGroup::new(PullRequestGroupKey::Others, others),
    ];

    let sorted = sort_pull_request_groups(
        &groups,
        PullRequestListSort::Blocked,
        "",
        unmeasured,
        PullRequestInvolvement::All,
    );
    assert_eq!(
        sorted
            .iter()
            .map(|group| numbers_of(&group.entries))
            .collect::<Vec<_>>(),
        vec![vec![2, 1], vec![3, 4], vec![5, 6]]
    );
    let others_as_authored = sort_pull_request_groups(
        &groups[2..],
        PullRequestListSort::Blocked,
        "",
        unmeasured,
        PullRequestInvolvement::Authored,
    );
    assert_eq!(numbers_of(&others_as_authored[0].entries), vec![6, 5]);
    let others_as_reviewing = sort_pull_request_groups(
        &groups[2..],
        PullRequestListSort::Blocked,
        "",
        unmeasured,
        PullRequestInvolvement::Reviewing,
    );
    assert_eq!(numbers_of(&others_as_reviewing[0].entries), vec![5, 6]);
    assert_eq!(
        sort_pull_request_groups(
            &groups,
            PullRequestListSort::Blocked,
            "needle",
            unmeasured,
            PullRequestInvolvement::Authored,
        ),
        groups
    );
}

// line counts that arrive after the rows

#[test]
fn fills_in_a_row_whose_host_left_the_counts_for_later() {
    let stats = PullRequestDiffStats::from([(
        "env-1 project-1 7".to_owned(),
        PullRequestLineCounts {
            additions: 42,
            deletions: 3,
        },
    )]);
    let row = listed(json!({ "number": 7, "additions": 0, "deletions": 0 }));
    assert_eq!(
        with_diff_stat(&row, &stats),
        listed(json!({ "number": 7, "additions": 42, "deletions": 3 }))
    );
}

#[test]
fn leaves_a_row_the_host_already_counted_alone() {
    let stats = PullRequestDiffStats::from([(
        "env-1 project-1 7".to_owned(),
        PullRequestLineCounts {
            additions: 42,
            deletions: 3,
        },
    )]);
    let row = listed(json!({ "number": 7, "additions": 1, "deletions": 1 }));
    assert_eq!(with_diff_stat(&row, &stats), row);
}

#[test]
fn leaves_a_row_whose_counts_have_not_arrived_as_it_is() {
    let stats = PullRequestDiffStats::from([(
        "env-1 project-1 7".to_owned(),
        PullRequestLineCounts {
            additions: 42,
            deletions: 3,
        },
    )]);
    let row = listed(json!({ "number": 9, "additions": 0, "deletions": 0 }));
    assert_eq!(with_diff_stat(&row, &stats), row);
}

// merging line counts across keyed stats queries

#[test]
fn keeps_counts_already_held_while_a_fresh_batch_says_nothing_about_them() {
    let held = merge_pull_request_diff_stats(
        &PullRequestDiffStats::new(),
        &[
            line_stat("env-1", "project-1", 1, 10, 2),
            line_stat("env-1", "project-1", 2, 5, 1),
        ],
    );
    // A third row appeared; its batch is still pending and contributes nothing yet.
    let merged = merge_pull_request_diff_stats(&held, &[]);
    assert_eq!(
        merged.get("env-1 project-1 1"),
        Some(&PullRequestLineCounts {
            additions: 10,
            deletions: 2,
        })
    );
    assert_eq!(
        merged.get("env-1 project-1 2"),
        Some(&PullRequestLineCounts {
            additions: 5,
            deletions: 1,
        })
    );
}

#[test]
fn replaces_a_count_once_its_replacement_arrives_keeping_its_neighbours() {
    let held = merge_pull_request_diff_stats(
        &PullRequestDiffStats::new(),
        &[line_stat("env-1", "project-1", 1, 10, 2)],
    );
    let merged = merge_pull_request_diff_stats(
        &held,
        &[
            line_stat("env-1", "project-1", 1, 11, 2),
            line_stat("env-1", "project-1", 3, 7, 0),
        ],
    );
    assert_eq!(
        merged.get("env-1 project-1 1"),
        Some(&PullRequestLineCounts {
            additions: 11,
            deletions: 2,
        })
    );
    assert_eq!(
        merged.get("env-1 project-1 3"),
        Some(&PullRequestLineCounts {
            additions: 7,
            deletions: 0,
        })
    );
}

#[test]
fn does_not_mutate_the_map_it_was_handed() {
    let held = PullRequestDiffStats::from([(
        "env-1 project-1 1".to_owned(),
        PullRequestLineCounts {
            additions: 1,
            deletions: 1,
        },
    )]);
    merge_pull_request_diff_stats(&held, &[line_stat("env-1", "project-1", 1, 2, 2)]);
    assert_eq!(
        held.get("env-1 project-1 1"),
        Some(&PullRequestLineCounts {
            additions: 1,
            deletions: 1,
        })
    );
}

// partitioning with the hosts' own priority reads

#[test]
fn keeps_others_in_feed_order_when_a_continuation_lands_an_authored_row_already_partitioned() {
    let older = listed(json!({ "number": 1, "updatedAt": "2026-07-05T00:00:00Z" }));
    let newer = listed(json!({ "number": 2, "updatedAt": "2026-07-06T00:00:00Z" }));
    let mine = authored_row(3, "2026-07-04T00:00:00Z");
    // The authored partition already holds the row the continuation carries.
    let groups = partition_pull_requests_with_priority(
        &[newer, older, mine.clone()],
        std::slice::from_ref(&mine),
        &[],
    );
    assert_eq!(
        groups.iter().map(|group| group.key).collect::<Vec<_>>(),
        vec![PullRequestGroupKey::Authored, PullRequestGroupKey::Others]
    );
    assert_eq!(numbers_of(&groups[1].entries), vec![2, 1]);
    assert_eq!(numbers_of(&groups[0].entries), vec![3]);
}

#[test]
fn appends_an_authored_row_the_partition_page_missed_to_others_rather_than_moving_it_up() {
    let shown = listed(json!({ "number": 1, "updatedAt": "2026-07-06T00:00:00Z" }));
    let older_mine = authored_row(9, "2026-01-01T00:00:00Z");
    let groups = partition_pull_requests_with_priority(&[shown, older_mine], &[], &[]);
    assert_eq!(groups.len(), 1);
    assert_eq!(numbers_of(&groups[0].entries), vec![1, 9]);
}

#[test]
fn gives_authored_precedence_over_review_requested_and_orders_partitions_by_recency() {
    let both = authored_row(1, "2026-07-01T00:00:00Z");
    let requested = listed(json!({
        "number": 2,
        "viewerReviewRequested": true,
        "updatedAt": "2026-07-02T00:00:00Z",
    }));
    let requested_older = listed(json!({
        "number": 3,
        "viewerReviewRequested": true,
        "updatedAt": "2026-06-02T00:00:00Z",
    }));
    let groups = partition_pull_requests_with_priority(
        &[],
        std::slice::from_ref(&both),
        &[both.clone(), requested_older, requested],
    );
    assert_eq!(
        groups.iter().map(|group| group.key).collect::<Vec<_>>(),
        vec![
            PullRequestGroupKey::Authored,
            PullRequestGroupKey::ReviewRequested
        ]
    );
    assert_eq!(numbers_of(&groups[0].entries), vec![1]);
    assert_eq!(numbers_of(&groups[1].entries), vec![2, 3]);
}

#[test]
fn lets_the_feeds_copy_of_a_partitioned_row_replace_the_partitions() {
    let stale = authored_row(1, "2026-07-01T00:00:00Z");
    let mut fresh = stale.clone();
    fresh.title = wire(json!("Retitled"));
    let groups = partition_pull_requests_with_priority(
        std::slice::from_ref(&fresh),
        std::slice::from_ref(&stale),
        &[],
    );
    assert_eq!(groups[0].entries[0].title.as_str(), "Retitled");
}

// the list snapshot across a reload

#[test]
fn hydrates_the_retained_rows_so_ghosts_never_replace_them() {
    let mut storage = MemorySnapshotStorage::default();
    save_snapshot(
        &mut storage,
        "env-1:open:all::",
        held_list(vec![listed(json!({ "number": 1 }))]),
    );
    let snapshot = read_snapshot(Some(&storage), ENV_1);
    assert_eq!(
        snapshot.as_ref().map(|snapshot| snapshot.scope.as_str()),
        Some("env-1:open:all::")
    );
    assert_eq!(
        numbers_of(&snapshot.expect("snapshot is held").data.entries),
        vec![1]
    );
}

#[test]
fn carries_neither_stale_failures_nor_stale_cursors() {
    let mut storage = MemorySnapshotStorage::default();
    save_snapshot(
        &mut storage,
        "s",
        held_list(vec![listed(json!({ "number": 1 }))]),
    );
    let snapshot = read_snapshot(Some(&storage), ENV_1).expect("snapshot is held");
    assert!(snapshot.data.errors.is_empty());
    assert!(snapshot.data.next_cursors.is_empty());
}

#[test]
fn answers_nothing_for_another_environment() {
    let mut storage = MemorySnapshotStorage::default();
    save_snapshot(
        &mut storage,
        "s",
        held_list(vec![listed(json!({ "number": 1 }))]),
    );
    assert!(read_snapshot(Some(&storage), ENV_2).is_none());
}

#[test]
fn rejects_a_snapshot_whose_rows_do_not_decode_as_entries() {
    let mut storage = MemorySnapshotStorage::default();
    storage
        .set_item(
            "t3.pullRequests.list:env-1",
            &json!({ "scope": "s", "data": { "entries": [null] } }).to_string(),
        )
        .expect("memory storage accepts writes");
    assert!(read_snapshot(Some(&storage), ENV_1).is_none());
    storage
        .set_item(
            "t3.pullRequests.list:env-1",
            &json!({ "scope": "s", "data": { "entries": [{ "host": "github.com" }] } }).to_string(),
        )
        .expect("memory storage accepts writes");
    assert!(read_snapshot(Some(&storage), ENV_1).is_none());
}

#[test]
fn shrugs_off_corrupt_storage_and_no_storage_at_all() {
    let mut storage = MemorySnapshotStorage::default();
    storage
        .set_item("t3.pullRequests.list:env-1", "{not json")
        .expect("memory storage accepts writes");
    assert!(read_snapshot(Some(&storage), ENV_1).is_none());
    assert!(read_snapshot(None, ENV_1).is_none());
}

#[test]
fn caps_the_accumulated_rows_but_keeps_the_whole_host_set() {
    // The page keeps growing as the reader scrolls, so what gets written is the accumulated
    // list rather than one round's answer — capped at the one page a cold start needs, while
    // the hosts it was read from (which do not grow with the page) are kept in full.
    let mut storage = MemorySnapshotStorage::default();
    let many_entries: Vec<EnvironmentPullRequestEntry> = (1..=120)
        .map(|number| listed(json!({ "number": number })))
        .collect();
    let viewers: PullRequestViewers =
        viewers_for(&[("github.com", "Bilal"), ("gitlab.com", "Bilal")]);
    let providers = vec![
        provider(json!({ "host": "github.com", "kind": "github", "detail": null })),
        provider(json!({ "host": "gitlab.com", "kind": "gitlab", "detail": null })),
    ];
    let accumulated = MergedPullRequestList {
        viewers: viewers.clone(),
        providers: providers.clone(),
        ..held_list(many_entries)
    };
    save_snapshot(&mut storage, "s", accumulated);
    let snapshot = read_snapshot(Some(&storage), ENV_1).expect("snapshot is held");
    assert_eq!(snapshot.data.entries.len(), 99);
    assert_eq!(snapshot.data.viewers, viewers);
    assert_eq!(snapshot.data.providers, providers);
}

// remembered pull request list controls

#[test]
fn restores_every_filter_and_the_selected_sort() {
    let storage = HeldPreferences::default();
    let saved = PullRequestListPreferences {
        involvement: PullRequestInvolvement::Reviewing,
        state: PullRequestListState::Merged,
        environment_id: Some(env(ENV_1)),
        project_id: Some(ProjectId::new("project-1").expect("fixture project id is valid")),
        host: Some("github.com".to_owned()),
        q: Some("workflow".to_owned()),
        draft: Some(PullRequestDraftFilter::Hide),
        review: Some(PullRequestReviewFilter::Approved),
        checks: Some(PullRequestChecksFilter::Passing),
        author: Some("octocat".to_owned()),
        labels: Some(vec!["bug".to_owned(), "priority".to_owned()]),
        sort: Some(PullRequestListSort::Largest),
    };

    write_pull_request_list_preferences(&saved, &storage);
    assert_eq!(read_pull_request_list_preferences(&storage), saved);
}

#[test]
fn omits_the_default_sort_from_storage() {
    assert_eq!(
        pull_request_list_preferences(&PullRequestListPreferences {
            sort: Some(PullRequestListSort::Ready),
            ..preferences(PullRequestInvolvement::All, PullRequestListState::Open)
        }),
        preferences(PullRequestInvolvement::All, PullRequestListState::Open)
    );
}

#[test]
fn keeps_an_explicit_recency_sort() {
    assert_eq!(
        pull_request_list_preferences(&PullRequestListPreferences {
            sort: Some(PullRequestListSort::Updated),
            ..preferences(PullRequestInvolvement::All, PullRequestListState::Open)
        }),
        PullRequestListPreferences {
            sort: Some(PullRequestListSort::Updated),
            ..preferences(PullRequestInvolvement::All, PullRequestListState::Open)
        }
    );
}

#[test]
fn falls_back_to_the_default_controls_when_storage_is_corrupt() {
    let storage = HeldPreferences::default();
    storage
        .set_item("t3.pullRequests.preferences", "{not json")
        .expect("held storage accepts writes");
    assert_eq!(
        read_pull_request_list_preferences(&storage),
        preferences(PullRequestInvolvement::All, PullRequestListState::Open)
    );
}

#[test]
fn falls_back_when_browser_policy_denies_storage() {
    assert_eq!(
        read_pull_request_list_preferences(&DeniedPreferences),
        preferences(PullRequestInvolvement::All, PullRequestListState::Open)
    );
    write_pull_request_list_preferences(
        &preferences(PullRequestInvolvement::All, PullRequestListState::Open),
        &DeniedPreferences,
    );
}

// merging the environments' own listings

#[test]
fn answers_nothing_until_an_environment_has() {
    assert!(merge_pull_request_lists(&[]).is_none());
}

#[test]
fn tags_every_row_with_the_environment_that_read_it_newest_first() {
    let merged = merge_pull_request_lists(&[
        (
            env(ENV_1),
            PullRequestListResult {
                entries: vec![listing(
                    json!({ "number": 1, "updatedAt": "2026-07-01T00:00:00Z" }),
                )],
                ..answer()
            },
        ),
        (
            env(ENV_2),
            PullRequestListResult {
                entries: vec![listing(
                    json!({ "number": 2, "updatedAt": "2026-08-01T00:00:00Z" }),
                )],
                ..answer()
            },
        ),
    ])
    .expect("two answers merge");
    assert_eq!(
        merged
            .entries
            .iter()
            .map(|row| (row.environment_id.as_str(), row.number.get()))
            .collect::<Vec<_>>(),
        vec![(ENV_2, 2), (ENV_1, 1)]
    );
}

#[test]
fn tells_two_environments_copies_of_one_pull_request_apart() {
    let row = listed(json!({ "number": 4 }));
    let other = EnvironmentPullRequestEntry {
        environment_id: env(ENV_2),
        ..row.clone()
    };
    assert_ne!(
        pull_request_entry_key_of(&row),
        pull_request_entry_key_of(&other)
    );
}

#[test]
fn keeps_project_errors_scoped_to_the_environment_that_reported_them() {
    let error: PullRequestListProjectError = wire(json!({
        "projectId": "project-1",
        "projectTitle": "Web",
        "message": "Not signed in",
    }));
    let merged = merge_pull_request_lists(&[
        (
            env(ENV_1),
            PullRequestListResult {
                errors: vec![error.clone()],
                ..answer()
            },
        ),
        (env(ENV_2), answer()),
    ])
    .expect("two answers merge");

    assert_eq!(
        merged.errors,
        vec![EnvironmentPullRequestError {
            error,
            environment_id: env(ENV_1),
        }]
    );
}

#[test]
fn folds_a_host_reached_from_two_environments_into_one_switcher_row() {
    let merged = merge_pull_request_lists(&[
        (env(ENV_1), answer()),
        (
            env(ENV_2),
            PullRequestListResult {
                providers: vec![provider(json!({
                    "host": "github.com",
                    "kind": "github",
                    "searchesOnHost": false,
                    "projectCount": 2,
                    "configured": false,
                    "detail": "Not signed in.",
                }))],
                ..answer()
            },
        ),
    ])
    .expect("two answers merge");
    // Readable because one environment could read it, but narrowed locally because the other
    // answers unsearched.
    assert_eq!(
        merged.providers,
        vec![provider(json!({
            "host": "github.com",
            "kind": "github",
            "searchesOnHost": false,
            "projectCount": 3,
            "configured": true,
            "detail": null,
        }))]
    );
}

#[test]
fn keeps_each_environments_continuation_to_itself() {
    let merged = merge_pull_request_lists(&[
        (
            env(ENV_1),
            PullRequestListResult {
                next_cursors: cursors(json!({ "github.com pingdotgg/t3code": "cursor-1" })),
                ..answer()
            },
        ),
        (env(ENV_2), answer()),
    ])
    .expect("two answers merge");
    assert_eq!(
        merged.next_cursors,
        IndexMap::from([(
            ENV_1.to_owned(),
            cursors(json!({ "github.com pingdotgg/t3code": "cursor-1" })),
        )])
    );
}

#[test]
fn reads_the_same_key_whichever_order_the_environments_connected_in() {
    assert_eq!(
        pull_request_environment_set_key(&[ENV_2, ENV_1]),
        pull_request_environment_set_key(&[ENV_1, ENV_2])
    );
}

// who "I" am, per server

#[test]
fn does_not_let_one_servers_account_decide_who_authored_another_servers_rows() {
    // Both servers reach github.com, signed in as different people. Folded into one host-keyed
    // record, whichever answered last spoke for both — and every row of the reader's own work
    // was filed under Others.
    let by_bilal = json!({ "login": "Bilal", "name": null, "avatarUrl": null });
    let merged = merge_pull_request_lists(&[
        (
            env(ENV_1),
            PullRequestListResult {
                viewers: wire(json!({ "github.com": "Bilal" })),
                entries: vec![listing(json!({ "number": 1, "author": by_bilal }))],
                ..answer()
            },
        ),
        (
            env(ENV_2),
            PullRequestListResult {
                viewers: wire(json!({ "github.com": "Octocat" })),
                entries: vec![listing(json!({ "number": 2, "author": by_bilal }))],
                ..answer()
            },
        ),
    ])
    .expect("two answers merge");

    let groups = group_pull_requests_by_involvement(&merged.entries, &merged.viewers);
    let authored = groups
        .iter()
        .find(|group| group.key == PullRequestGroupKey::Authored)
        .map(|group| numbers_of(&group.entries));
    assert_eq!(authored, Some(vec![1]));
    assert_eq!(
        numbers_of(&filter_pull_requests_by_involvement(
            &merged.entries,
            &merged.viewers,
            PullRequestInvolvement::Authored,
        )),
        vec![1]
    );
}

#[test]
fn still_reads_a_single_servers_host_keyed_viewers_which_is_what_a_snapshot_carries() {
    let by_bilal = json!({ "login": "Bilal", "name": null, "avatarUrl": null });
    assert_eq!(
        numbers_of(&filter_pull_requests_by_involvement(
            &[listed(json!({ "number": 1, "author": by_bilal }))],
            &viewers_for(&[("github.com", "Bilal")]),
            PullRequestInvolvement::Authored,
        )),
        vec![1]
    );
}

#[test]
fn names_the_servers_with_more_rows_and_no_cursor_to_reach_them_by() {
    let merged = merge_pull_request_lists(&[
        (
            env(ENV_1),
            PullRequestListResult {
                truncated: true,
                next_cursors: cursors(json!({ "github.com acme/web": "cursor-1" })),
                ..answer()
            },
        ),
        (
            env(ENV_2),
            PullRequestListResult {
                truncated: true,
                ..answer()
            },
        ),
    ])
    .expect("two answers merge");

    assert_eq!(
        merged.truncated_environments,
        vec![ENV_1.to_owned(), ENV_2.to_owned()]
    );
    assert_eq!(
        merged.next_cursors.keys().cloned().collect::<Vec<_>>(),
        vec![ENV_1.to_owned()]
    );
}

// the project an id names

fn projects() -> Vec<ScopedProject> {
    vec![
        ScopedProject {
            id: "project-1",
            environment_id: ENV_1,
        },
        ScopedProject {
            id: "project-1",
            environment_id: ENV_2,
        },
        ScopedProject {
            id: "project-2",
            environment_id: ENV_2,
        },
    ]
}

#[test]
fn takes_the_server_it_was_given() {
    assert_eq!(
        find_scoped_project(
            &projects(),
            Some(ENV_2),
            Some("project-1"),
            |project| project.id,
            |project| { project.environment_id }
        )
        .map(|project| project.environment_id),
        Some(ENV_2)
    );
}

#[test]
fn answers_a_bare_id_only_where_one_server_has_it() {
    assert_eq!(
        find_scoped_project(
            &projects(),
            None,
            Some("project-2"),
            |project| project.id,
            |project| { project.environment_id }
        )
        .map(|project| project.environment_id),
        Some(ENV_2)
    );
    // Two servers hold this id: narrowing to either would be a coin toss the reader cannot see.
    assert!(
        find_scoped_project(
            &projects(),
            None,
            Some("project-1"),
            |project| project.id,
            |project| { project.environment_id }
        )
        .is_none()
    );
}

#[test]
fn answers_nothing_for_a_server_that_does_not_have_it() {
    assert!(
        find_scoped_project(
            &projects(),
            Some(ENV_1),
            Some("project-2"),
            |project| project.id,
            |project| { project.environment_id }
        )
        .is_none()
    );
}

// which environments a listing should ask

#[test]
fn asks_only_the_owning_server_once_the_project_is_unambiguous() {
    let projects = vec![("project-1".to_owned(), ENV_2)];
    assert_eq!(
        resolve_query_environment_ids(
            &[ENV_1, ENV_2, ENV_3],
            &projects,
            Some(&ENV_2),
            Some("project-1"),
            true,
        ),
        vec![ENV_2]
    );
}

#[test]
fn asks_every_environment_when_there_is_no_project_narrowing_at_all() {
    assert_eq!(
        resolve_query_environment_ids(&[ENV_1, ENV_2, ENV_3], &[], None, None, true),
        vec![ENV_1, ENV_2, ENV_3]
    );
}

#[test]
fn asks_only_the_environments_that_actually_hold_an_ambiguous_bare_id_never_a_third_that_doesnt() {
    let projects = vec![
        ("project-1".to_owned(), ENV_1),
        ("project-1".to_owned(), ENV_2),
    ];
    // Two servers can genuinely hold the same project id string; the third holds no such project
    // and asking it would return an unrelated project's rows rather than an honest empty answer.
    assert_eq!(
        resolve_query_environment_ids(
            &[ENV_1, ENV_2, ENV_3],
            &projects,
            None,
            Some("project-1"),
            true
        ),
        vec![ENV_1, ENV_2]
    );
}

#[test]
fn asks_nothing_for_a_bare_id_no_environment_holds() {
    let projects = vec![("project-1".to_owned(), ENV_1)];
    assert_eq!(
        resolve_query_environment_ids(
            &[ENV_1, ENV_2, ENV_3],
            &projects,
            None,
            Some("project-9"),
            true
        ),
        Vec::<&str>::new()
    );
}

#[test]
fn asks_every_environment_while_the_servers_have_not_said_what_they_hold_yet() {
    // Nothing matches the id because nothing has been read yet, which is not the same answer as
    // no server holding it: reading none of them would show an empty page for a project that is
    // there.
    assert_eq!(
        resolve_query_environment_ids(&[ENV_1, ENV_2, ENV_3], &[], None, Some("project-1"), false),
        vec![ENV_1, ENV_2, ENV_3]
    );
}

#[test]
fn keeps_the_single_environment_case_unchanged() {
    let projects = vec![("project-1".to_owned(), ENV_1)];
    assert_eq!(
        resolve_query_environment_ids(&[ENV_1], &projects, None, Some("project-1"), true),
        vec![ENV_1]
    );
}

// the server a saved selection names

#[test]
fn resolves_to_nothing_yet_for_a_server_that_is_known_but_not_ready_rather_than_falling_back() {
    // ENV_2 is in the catalog (still connecting, say) but not the fallback; a caller that then
    // looks a project up under ENV_2 finds none rather than one belonging to the fallback server.
    let known = HashSet::from([ENV_1, ENV_2]);
    assert_eq!(
        resolve_selected_environment_id(Some(&ENV_2), &known, Some(&ENV_1)),
        Some(ENV_2)
    );
}

#[test]
fn falls_back_for_a_server_the_workspace_genuinely_no_longer_has() {
    let known = HashSet::from([ENV_1, ENV_2]);
    assert_eq!(
        resolve_selected_environment_id(Some(&"env-gone"), &known, Some(&ENV_1)),
        Some(ENV_1)
    );
}

#[test]
fn falls_back_to_nothing_when_no_server_names_a_fallback_either() {
    let known = HashSet::from([ENV_1, ENV_2]);
    assert_eq!(
        resolve_selected_environment_id(Some(&"env-gone"), &known, None),
        None
    );
}

#[test]
fn uses_the_fallback_outright_when_nothing_was_named() {
    let known = HashSet::from([ENV_1, ENV_2]);
    assert_eq!(
        resolve_selected_environment_id(None, &known, Some(&ENV_1)),
        Some(ENV_1)
    );
    assert_eq!(resolve_selected_environment_id(None, &known, None), None);
}

// colon-namespaced labels typed as a search

#[test]
fn reads_an_unknown_key_as_the_label_it_almost_always_is() {
    assert_eq!(
        parse_pull_request_query("size:XXL").filters.labels,
        label_groups(json!([["size:XXL"]]))
    );
    assert_eq!(
        parse_pull_request_query("vouch:trusted").filters.labels,
        label_groups(json!([["vouch:trusted"]]))
    );
    assert_eq!(parse_pull_request_query("size:XXL").text, "");
}

#[test]
fn reads_the_key_as_the_namespace_of_every_bare_name_in_its_list() {
    assert_eq!(
        parse_pull_request_query("size:S,XS").filters.labels,
        label_groups(json!([["size:S", "size:XS"]]))
    );
}

#[test]
fn leaves_a_name_that_already_carries_its_own_namespace_alone() {
    // `size:S,size:XS` is the same pair written out; prefixing again would ask for `size:size:XS`.
    assert_eq!(
        parse_pull_request_query("size:S,size:XS").filters.labels,
        label_groups(json!([["size:S", "size:XS"]]))
    );
}

#[test]
fn excludes_one_the_same_way() {
    assert_eq!(
        parse_pull_request_query("-size:XXL")
            .filters
            .excluded_labels,
        list_filters(json!({ "excludedLabels": ["size:XXL"] })).excluded_labels
    );
    assert_eq!(
        parse_pull_request_query("-size:S,XS")
            .filters
            .excluded_labels,
        list_filters(json!({ "excludedLabels": ["size:S", "size:XS"] })).excluded_labels
    );
}

#[test]
fn keeps_a_quoted_token_as_text_which_is_the_way_back_to_a_literal_search() {
    let parsed = parse_pull_request_query("\"size:XXL\"");
    assert_eq!(parsed.filters.labels, None);
    assert_eq!(parsed.text, "\"size:XXL\"");
}

#[test]
fn reads_a_hyphenated_namespace_as_a_label_too() {
    assert_eq!(
        parse_pull_request_query("priority-1:high").filters.labels,
        label_groups(json!([["priority-1:high"]]))
    );
    assert_eq!(parse_pull_request_query("priority-1:high").text, "");
}

#[test]
fn leaves_a_pasted_link_alone_rather_than_naming_a_label_after_its_scheme() {
    let parsed = parse_pull_request_query("https://github.com/pingdotgg/t3code/pull/1");
    assert_eq!(parsed.filters.labels, None);
    assert_eq!(parsed.text, "https://github.com/pingdotgg/t3code/pull/1");
}

#[test]
fn mixes_with_the_keys_it_does_know_and_with_plain_words() {
    let parsed = parse_pull_request_query("area:web draft:true wizard label:bug");
    assert_eq!(
        parsed.filters,
        list_filters(json!({ "labels": [["area:web"], ["bug"]], "draft": "only" }))
    );
    assert_eq!(parsed.text, "wizard");
}

#[test]
fn shares_the_ceiling_with_the_labels_typed_as_labels() {
    let many = (0..12)
        .map(|index| format!("area:{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        parse_pull_request_query(&many)
            .filters
            .labels
            .map(|labels| labels.len()),
        Some(10)
    );
}

// the priority groups against a paginated feed

#[test]
fn shows_every_authored_row_the_host_reported_not_only_the_ones_the_feed_page_holds() {
    // The feed is one page ordered by recency, so on a busy repository it can hold exactly one of
    // somebody's own pull requests while the rest sit further down the host's list. The Authored
    // group comes from its own server-filtered read for that reason: grouping the page instead
    // leaves the older ones under Others, or off the page altogether.
    let newest = authored_row(6039, "2026-08-11T08:00:00Z");
    let older = [
        authored_row(5499, "2026-08-10T17:00:00Z"),
        authored_row(5544, "2026-08-10T16:00:00Z"),
    ];
    let feed = vec![
        newest.clone(),
        listed(json!({ "number": 6123, "updatedAt": "2026-08-11T09:00:00Z" })),
    ];
    let mut partition = vec![newest];
    partition.extend(older);

    let groups = partition_pull_requests_with_priority(&feed, &partition, &[]);

    assert_eq!(
        groups
            .iter()
            .find(|group| group.key == PullRequestGroupKey::Authored)
            .map(|group| numbers_of(&group.entries)),
        Some(vec![6039, 5499, 5544])
    );
    assert_eq!(
        groups
            .iter()
            .find(|group| group.key == PullRequestGroupKey::Others)
            .map(|group| numbers_of(&group.entries)),
        Some(vec![6123])
    );
}

// pull request list overrides

#[test]
fn maps_the_actions_that_change_a_rows_state_and_nothing_else() {
    let now = parse_ms("2026-07-02T00:00:00Z").expect("fixture time parses");
    assert_eq!(
        pull_request_override_after_action(&row(1, "open").entry, PullRequestAction::Close, now, 7),
        Some(PullRequestListOverride {
            state: PullRequestState::Closed,
            is_draft: None,
            updated_at: "2026-07-02T00:00:00.000Z".to_owned(),
            token: 7,
            at: now,
        })
    );
    assert_eq!(
        pull_request_override_after_action(
            &row(1, "closed").entry,
            PullRequestAction::Reopen,
            now,
            1
        )
        .map(|applied| applied.state),
        Some(PullRequestState::Open)
    );
    assert_eq!(
        pull_request_override_after_action(&row(1, "open").entry, PullRequestAction::Merge, now, 1)
            .map(|applied| applied.state),
        Some(PullRequestState::Merged)
    );
    assert_eq!(
        pull_request_override_after_action(&row(1, "open").entry, PullRequestAction::Draft, now, 1)
            .map(|applied| applied.is_draft),
        Some(Some(true))
    );
    assert_eq!(
        pull_request_override_after_action(
            &row(1, "open").entry,
            PullRequestAction::UpdateBranch,
            now,
            1,
        ),
        None
    );
}

#[test]
fn writes_the_override_over_the_row_and_drops_it_from_a_list_whose_state_it_left() {
    let rows = vec![row(1, "open"), row(2, "open")];
    let overrides = IndexMap::from([(
        "#1".to_owned(),
        override_at(PullRequestState::Closed, "2026-07-03T00:00:00Z", 1, 0),
    )]);
    assert_eq!(
        numbers_of(&apply_pull_request_overrides(
            &rows,
            &overrides,
            hash_key,
            PullRequestListState::Open,
        )),
        vec![2]
    );
    let all = apply_pull_request_overrides(&rows, &overrides, hash_key, PullRequestListState::All);
    assert_eq!(
        all.iter()
            .map(|row| (row.number.get(), row.state))
            .collect::<Vec<_>>(),
        vec![(1, PullRequestState::Closed), (2, PullRequestState::Open)]
    );
    assert_eq!(
        apply_pull_request_overrides(
            &rows,
            &IndexMap::new(),
            hash_key,
            PullRequestListState::Open
        ),
        rows
    );
}

#[test]
fn filters_host_rows_by_state_after_pending_actions_settle() {
    // Upstream's `it.each` over open, closed, merged and all, as one loop over the same rows.
    let rows = vec![row(1, "open"), row(2, "closed"), row(3, "merged")];
    let cases: [(PullRequestListState, Vec<u64>); 4] = [
        (PullRequestListState::Open, vec![1]),
        (PullRequestListState::Closed, vec![2]),
        (PullRequestListState::Merged, vec![3]),
        (PullRequestListState::All, vec![1, 2, 3]),
    ];
    for (state, expected) in cases {
        assert_eq!(
            numbers_of(&apply_pull_request_overrides(
                &rows,
                &IndexMap::new(),
                hash_key,
                state,
            )),
            expected,
            "state {state:?}"
        );
    }
}

#[test]
fn keeps_a_confirmed_close_hidden_when_its_override_is_cleared() {
    let at = 1_000_000;
    let rows = vec![row(1, "closed"), row(2, "open"), row(3, "merged")];
    let overrides = IndexMap::from([(
        "#1".to_owned(),
        override_at(PullRequestState::Closed, "2026-07-03T00:00:00Z", 1, at),
    )]);
    let settled = settle_pull_request_overrides(&overrides, &rows, hash_key, at + 5_000);
    assert!(settled.is_empty());
    assert_eq!(
        numbers_of(&apply_pull_request_overrides(
            &rows,
            &settled,
            hash_key,
            PullRequestListState::Open,
        )),
        vec![2]
    );
}

#[test]
fn hands_back_the_held_object_for_a_row_a_refresh_did_not_change() {
    let previous = vec![row(1, "open"), row(2, "open")];
    let next = vec![row(1, "open"), row(2, "merged")];
    let reused = reuse_pull_request_entries(&previous, &next, hash_key);
    assert_eq!(reused[0], previous[0]);
    assert_eq!(reused[1], next[1]);
    assert_eq!(
        reuse_pull_request_entries(&previous, &[row(1, "open"), row(2, "open")], hash_key),
        previous
    );
}

// pull request list override settlement

#[test]
fn keeps_an_override_until_an_answer_agrees_with_it() {
    let at = 1_000_000;
    let closed = override_at(PullRequestState::Closed, "2026-07-03T00:00:00Z", 1, at);
    let overrides = IndexMap::from([("#1".to_owned(), closed)]);
    // A read from before the action still says open: the override stands.
    assert_eq!(
        settle_pull_request_overrides(&overrides, &[row(1, "open")], hash_key, at + 5_000),
        overrides
    );
    // Absent from the answer says nothing: the row may live in another group or page.
    assert_eq!(
        settle_pull_request_overrides(&overrides, &[row(2, "open")], hash_key, at + 5_000).len(),
        1
    );
    // Present as closed: confirmed.
    assert_eq!(
        settle_pull_request_overrides(&overrides, &[row(1, "closed")], hash_key, at + 5_000).len(),
        0
    );
    // Present as open a good while later: the host's news, which outranks the note.
    assert_eq!(
        settle_pull_request_overrides(&overrides, &[row(1, "open")], hash_key, at + 90_000).len(),
        0
    );
}

#[test]
fn does_not_hand_back_the_old_order_when_only_the_order_changed() {
    let previous = vec![row(1, "open"), row(2, "open")];
    let swapped =
        reuse_pull_request_entries(&previous, &[row(2, "open"), row(1, "open")], hash_key);
    assert_ne!(swapped, previous);
    assert_eq!(numbers_of(&swapped), vec![2, 1]);
}

// pull request quick actions

#[test]
fn offers_what_each_host_reports_and_githubs_legacy_set_when_a_server_reports_none() {
    let providers = [
        provider(json!({
            "host": "bitbucket.org",
            "kind": "bitbucket",
            "actions": ["merge", "close"],
        })),
        provider(json!({ "host": "github.com", "kind": "github" })),
    ];
    let actions_of = pull_request_host_actions(&providers);
    let bitbucket = listed(json!({
        "host": "bitbucket.org",
        "provider": "bitbucket",
        "state": "closed",
        "isDraft": false,
    }));
    assert_eq!(
        pull_request_quick_actions(&bitbucket.entry, &actions_of(&bitbucket.entry)),
        Vec::<PullRequestQuickAction>::new()
    );
    let github = listed(json!({
        "host": "github.com",
        "provider": "github",
        "state": "open",
        "isDraft": true,
    }));
    assert_eq!(
        pull_request_quick_actions(&github.entry, &actions_of(&github.entry)),
        vec![PullRequestQuickAction::Close, PullRequestQuickAction::Ready]
    );
}
