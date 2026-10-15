#![allow(unused_imports)]
use super::super::*;

use super::fixtures::*;
use pretty_assertions::assert_eq;
use turin_contracts::pull_request::{
    PullRequestActor, PullRequestCheckStatus, PullRequestCommentKind, PullRequestDiffSide,
    PullRequestReviewThread, PullRequestThreadComment,
};

fn stack_ref<'a>(
    name: &'a str,
    is_default: bool,
    is_remote: Option<bool>,
    remote_name: Option<&'a str>,
) -> StackBaseRef<'a> {
    StackBaseRef {
        name,
        is_default,
        is_remote,
        remote_name,
    }
}

fn open_primary_control_input() -> PullRequestPrimaryControlInput {
    PullRequestPrimaryControlInput {
        state: PullRequestState::Open,
        is_draft: false,
        mergeability: PullRequestMergeability::Mergeable,
        checks_state: Some(ContractChecksState::Passing),
        auto_merge_enabled: Some(false),
        has_merge_method: true,
        can_merge: true,
        can_mark_ready: true,
        can_enable_auto_merge: true,
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Dated {
    created_at: &'static str,
}

#[test]
fn groups_checks_needing_attention_before_running_and_completed_checks_without_losing_any() {
    let statuses = [
        PullRequestCheckStatus::Success,
        PullRequestCheckStatus::Pending,
        PullRequestCheckStatus::Failure,
        PullRequestCheckStatus::Skipped,
        PullRequestCheckStatus::ActionRequired,
        PullRequestCheckStatus::Cancelled,
        PullRequestCheckStatus::Neutral,
        PullRequestCheckStatus::Pending,
    ];
    let checks: Vec<PullRequestCheck> = statuses
        .iter()
        .enumerate()
        .map(|(index, status)| check(&format!("check-{index}"), *status))
        .collect();
    let grouped = group_pull_request_checks(&checks);
    assert_eq!(
        names(&grouped.attention),
        strs(&["check-2", "check-4", "check-5"])
    );
    assert_eq!(names(&grouped.running), strs(&["check-1", "check-7"]));
    assert_eq!(
        names(&grouped.completed),
        strs(&["check-0", "check-3", "check-6"])
    );
    assert_eq!(checks[0].status, PullRequestCheckStatus::Success);
    assert_eq!(
        group_pull_request_checks(&[]),
        GroupedPullRequestChecks {
            attention: vec![],
            running: vec![],
            completed: vec![],
        }
    );
}

// The `it.each` rows for gitlab, azure-devops and bitbucket, and the Forgejo fetch and quoting
// tests, are not ported: those hosts are cut (PORTING cut list), so their commands do not exist.
#[test]
fn builds_the_github_checkout_command() {
    assert_eq!(
        pull_request_checkout_command(&kind("github"), 42, "feature", None, None),
        Some("gh pr checkout 42".to_owned())
    );
}

#[test]
fn builds_no_checkout_command_for_forgejo_or_an_unknown_host() {
    assert_eq!(
        pull_request_checkout_command(&kind("forgejo"), 42, "feature", None, None),
        None
    );
    assert_eq!(
        pull_request_checkout_command(&kind("unknown"), 42, "feature", None, None),
        None
    );
}

// The gitlab half of this test is not ported: GitLab is cut, so only the GitHub command is checked.
#[test]
fn uses_a_public_host_when_no_repository_identity_is_available() {
    assert_eq!(
        loading_pull_request_checkout_command(&reference(Some("github.com")), None),
        Some("gh pr checkout 42".to_owned())
    );
}

#[test]
fn uses_a_matching_enterprise_identity_and_rejects_an_explicit_host_mismatch() {
    let enterprise = identity("github", "github.example.test/acme/web");
    assert_eq!(
        loading_pull_request_checkout_command(
            &reference(Some("github.example.test")),
            Some(&enterprise)
        ),
        Some("gh pr checkout 42".to_owned())
    );
    assert_eq!(
        loading_pull_request_checkout_command(&reference(Some("github.com")), Some(&enterprise)),
        None
    );
}

#[test]
fn does_not_infer_a_number_only_command_without_a_trusted_provider() {
    assert_eq!(
        loading_pull_request_checkout_command(&reference(None), None),
        None
    );
    assert_eq!(
        loading_pull_request_checkout_command(
            &reference(Some("github.com")),
            Some(&identity("gitlab", "gitlab.com/acme/web")),
        ),
        None
    );
}

#[test]
fn uses_the_current_choice_then_the_project_default_then_the_last_choice() {
    assert_eq!(
        resolve_pull_request_merge_method(
            &[
                PullRequestMergeMethod::Merge,
                PullRequestMergeMethod::Squash,
                PullRequestMergeMethod::Rebase
            ],
            None,
            Some(PullRequestMergeMethod::Squash),
            PullRequestMergeMethod::Rebase,
        ),
        PullRequestMergeMethod::Squash
    );
    assert_eq!(
        resolve_pull_request_merge_method(
            &[
                PullRequestMergeMethod::Merge,
                PullRequestMergeMethod::Squash,
                PullRequestMergeMethod::Rebase
            ],
            Some(PullRequestMergeMethod::Rebase),
            Some(PullRequestMergeMethod::Squash),
            PullRequestMergeMethod::Merge,
        ),
        PullRequestMergeMethod::Rebase
    );
    assert_eq!(
        resolve_pull_request_merge_method(
            &[
                PullRequestMergeMethod::Merge,
                PullRequestMergeMethod::Rebase
            ],
            None,
            Some(PullRequestMergeMethod::Squash),
            PullRequestMergeMethod::Rebase,
        ),
        PullRequestMergeMethod::Rebase
    );
    assert_eq!(
        resolve_pull_request_merge_method(
            &[PullRequestMergeMethod::Squash],
            None,
            Some(PullRequestMergeMethod::Merge),
            PullRequestMergeMethod::Rebase,
        ),
        PullRequestMergeMethod::Squash
    );
}

fn activity_revision(key: &str, updated_at: &str) -> PullRequestActivityRevision {
    PullRequestActivityRevision {
        key: key.into(),
        updated_at: updated_at.into(),
    }
}

#[test]
fn refreshes_activity_only_after_the_same_pull_request_changes() {
    let first = activity_revision("project:acme/web#7", "2026-08-13T13:00:00Z");
    assert!(should_refresh_pull_request_activity(
        Some(&first),
        &activity_revision("project:acme/web#7", "2026-08-13T13:01:00Z"),
    ));
}

#[test]
fn does_not_duplicate_the_first_activity_read_or_carry_a_revision_across_pull_requests() {
    let first = activity_revision("project:acme/web#7", "2026-08-13T13:00:00Z");
    assert!(!should_refresh_pull_request_activity(None, &first));
    assert!(!should_refresh_pull_request_activity(Some(&first), &first));
    assert!(!should_refresh_pull_request_activity(
        Some(&first),
        &activity_revision("project:acme/web#8", "2026-08-13T13:01:00Z"),
    ));
}

#[test]
fn appends_new_comments_once_and_keeps_refreshed_base_comments() {
    assert_eq!(
        merge_pull_request_thread_comments(
            &[
                test_comment("c1", "refreshed"),
                test_comment("c2", "already in base"),
            ],
            &[
                test_comment("c2", "stale page copy"),
                test_comment("c3", "next page"),
            ],
        ),
        vec![
            test_comment("c1", "refreshed"),
            test_comment("c2", "already in base"),
            test_comment("c3", "next page"),
        ]
    );
}

#[test]
fn keeps_a_loaded_comment_after_its_body_is_edited() {
    let loaded = [
        test_comment("c2", "old body"),
        test_comment("c3", "another loaded comment"),
    ];
    assert_eq!(
        edit_pull_request_thread_comment(&loaded, "c2", "saved body"),
        vec![
            test_comment("c2", "saved body"),
            test_comment("c3", "another loaded comment"),
        ]
    );
}

#[test]
fn keeps_the_group_divider_when_auto_merge_is_the_only_action() {
    assert!(pull_request_action_menu_has_group(false, true, false));
}

#[test]
fn moves_pending_and_failing_checks_to_auto_merge() {
    assert_eq!(
        resolve_pull_request_primary_control(PullRequestPrimaryControlInput {
            checks_state: Some(ContractChecksState::Pending),
            ..open_primary_control_input()
        }),
        Some(PullRequestPrimaryControl::EnableAutoMerge)
    );
    assert_eq!(
        resolve_pull_request_primary_control(PullRequestPrimaryControlInput {
            checks_state: Some(ContractChecksState::Failing),
            ..open_primary_control_input()
        }),
        Some(PullRequestPrimaryControl::EnableAutoMerge)
    );
}

#[test]
fn does_not_offer_auto_merge_while_the_host_state_is_unknown() {
    assert_eq!(
        resolve_pull_request_primary_control(PullRequestPrimaryControlInput {
            checks_state: Some(ContractChecksState::Pending),
            auto_merge_enabled: None,
            ..open_primary_control_input()
        }),
        Some(PullRequestPrimaryControl::Merge)
    );
}

#[test]
fn keeps_armed_and_terminal_states_in_the_merge_button_slot() {
    assert_eq!(
        resolve_pull_request_primary_control(PullRequestPrimaryControlInput {
            auto_merge_enabled: Some(true),
            ..open_primary_control_input()
        }),
        Some(PullRequestPrimaryControl::AutoMergeArmed)
    );
    assert_eq!(
        resolve_pull_request_primary_control(PullRequestPrimaryControlInput {
            state: PullRequestState::Merged,
            ..open_primary_control_input()
        }),
        Some(PullRequestPrimaryControl::Merged)
    );
    assert_eq!(
        resolve_pull_request_primary_control(PullRequestPrimaryControlInput {
            state: PullRequestState::Closed,
            ..open_primary_control_input()
        }),
        Some(PullRequestPrimaryControl::Closed)
    );
}

#[test]
fn keeps_conflicts_and_drafts_actionable_before_merge() {
    assert_eq!(
        resolve_pull_request_primary_control(PullRequestPrimaryControlInput {
            mergeability: PullRequestMergeability::Conflicting,
            ..open_primary_control_input()
        }),
        Some(PullRequestPrimaryControl::Resolve)
    );
    assert_eq!(
        resolve_pull_request_primary_control(PullRequestPrimaryControlInput {
            is_draft: true,
            ..open_primary_control_input()
        }),
        Some(PullRequestPrimaryControl::Ready)
    );
}

#[test]
fn requires_a_known_default_branch() {
    assert!(!is_stacked_pull_request_base(
        "main",
        &[stack_ref("main", false, None, None)]
    ));
}

#[test]
fn recognizes_local_and_remote_forms_of_the_default_branch() {
    assert!(!is_stacked_pull_request_base(
        "main",
        &[stack_ref("main", true, Some(false), None)]
    ));
    assert!(!is_stacked_pull_request_base(
        "main",
        &[stack_ref("origin/main", true, Some(true), Some("origin"))]
    ));
}

#[test]
fn classifies_a_non_default_base_as_stacked_once_the_default_is_known() {
    assert!(is_stacked_pull_request_base(
        "feature-base",
        &[stack_ref("origin/main", true, Some(true), Some("origin"))]
    ));
}

#[test]
fn does_not_mistake_a_nested_branch_suffix_for_the_default_branch() {
    assert!(is_stacked_pull_request_base(
        "main",
        &[stack_ref(
            "origin/feature/main",
            true,
            Some(true),
            Some("origin")
        )]
    ));
    assert!(is_stacked_pull_request_base(
        "1.0",
        &[stack_ref("release/1.0", true, Some(false), None)]
    ));
}

#[test]
fn reverses_the_chronological_list_for_newest_first_and_leaves_oldest_first_alone() {
    let comments = [
        Dated { created_at: "a" },
        Dated { created_at: "b" },
        Dated { created_at: "c" },
    ];
    assert_eq!(
        order_pull_request_comments(&comments, PullRequestCommentOrder::Newest),
        vec![
            Dated { created_at: "c" },
            Dated { created_at: "b" },
            Dated { created_at: "a" },
        ]
    );
    assert_eq!(
        order_pull_request_comments(&comments, PullRequestCommentOrder::Oldest),
        comments.to_vec()
    );
    // The source array is chronological input, not a mutation target.
    assert_eq!(
        comments.to_vec(),
        vec![
            Dated { created_at: "a" },
            Dated { created_at: "b" },
            Dated { created_at: "c" },
        ]
    );
}

#[test]
fn reads_the_same_three_verdicts_however_a_host_spells_them() {
    assert_eq!(
        pull_request_review_outcome(Some("APPROVED")),
        Some(PullRequestReviewOutcome::Approved)
    );
    assert_eq!(
        pull_request_review_outcome(Some("approved")),
        Some(PullRequestReviewOutcome::Approved)
    );
    assert_eq!(
        pull_request_review_outcome(Some("CHANGES_REQUESTED")),
        Some(PullRequestReviewOutcome::ChangesRequested)
    );
    assert_eq!(
        pull_request_review_outcome(Some("changes_requested")),
        Some(PullRequestReviewOutcome::ChangesRequested)
    );
    assert_eq!(
        pull_request_review_outcome(Some("DISMISSED")),
        Some(PullRequestReviewOutcome::Dismissed)
    );
}

#[test]
fn is_not_a_verdict_where_the_review_only_carried_remarks() {
    assert_eq!(pull_request_review_outcome(Some("COMMENTED")), None);
    assert_eq!(pull_request_review_outcome(Some("PENDING")), None);
    assert_eq!(pull_request_review_outcome(None), None);
}

#[test]
fn keeps_each_reviewers_last_word_whatever_order_the_host_returned_them_in() {
    let entries = latest_pull_request_review_outcomes(
        &[
            review_comment("r3", Some("bilal"), "APPROVED", "2026-07-03T00:00:00Z"),
            review_comment(
                "r1",
                Some("bilal"),
                "CHANGES_REQUESTED",
                "2026-07-01T00:00:00Z",
            ),
            review_comment(
                "r2",
                Some("octocat"),
                "CHANGES_REQUESTED",
                "2026-07-02T00:00:00Z",
            ),
            // Not a verdict, so it neither adds a reviewer nor overwrites one.
            review_comment("r4", Some("octocat"), "COMMENTED", "2026-07-04T00:00:00Z"),
        ],
        &[],
    );
    let summary: Vec<(Option<&str>, PullRequestReviewOutcome)> = entries
        .iter()
        .map(|entry| {
            (
                entry.actor.as_ref().map(|actor| actor.login.as_str()),
                entry.outcome,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (Some("bilal"), PullRequestReviewOutcome::Approved),
            (Some("octocat"), PullRequestReviewOutcome::ChangesRequested),
        ]
    );
}

#[test]
fn keeps_two_deleted_accounts_apart_rather_than_counting_them_as_one_reviewer() {
    assert_eq!(
        latest_pull_request_review_outcomes(
            &[
                review_comment("r1", None, "APPROVED", "2026-07-01T00:00:00Z"),
                review_comment("r2", None, "APPROVED", "2026-07-02T00:00:00Z"),
            ],
            &[],
        )
        .len(),
        2
    );
}

#[test]
fn gives_every_entry_a_key_that_separates_the_reviewers_it_kept_apart() {
    let entries = latest_pull_request_review_outcomes(
        &[
            review_comment("r1", None, "APPROVED", "2026-07-01T00:00:00Z"),
            review_comment("r2", None, "APPROVED", "2026-07-01T00:00:00Z"),
        ],
        &[],
    );
    // Same author (none) and the same instant, so only the review's own id tells them apart.
    let keys: std::collections::HashSet<&str> =
        entries.iter().map(|entry| entry.key.as_str()).collect();
    assert_eq!(keys.len(), 2);
}

#[test]
fn calls_a_verdict_stale_once_commits_land_after_it_and_current_before_that() {
    let commits = [commit("c0ffee", "later work", "2026-07-05T00:00:00Z")];
    let review = |created_at: &str| PullRequestComment {
        kind: PullRequestCommentKind::Review,
        review_state: Some("APPROVED".into()),
        created_at: created_at.into(),
        ..base_comment()
    };

    assert!(
        latest_pull_request_review_outcomes(&[review("2026-07-01T00:00:00Z")], &commits)[0].stale
    );
    assert!(
        !latest_pull_request_review_outcomes(&[review("2026-07-06T00:00:00Z")], &commits)[0].stale
    );
    // Nothing to be overtaken by, so nothing is stale.
    assert!(!latest_pull_request_review_outcomes(&[review("2026-07-01T00:00:00Z")], &[])[0].stale);
}

#[test]
fn measures_staleness_against_the_newest_commit_not_the_last_one_listed() {
    assert_eq!(
        newest_pull_request_commit_at(&[
            commit("a", "", "2026-07-09T00:00:00Z"),
            commit("b", "", "2026-07-02T00:00:00Z"),
        ]),
        Some("2026-07-09T00:00:00Z".to_owned())
    );
    assert_eq!(newest_pull_request_commit_at(&[]), None);
}

#[test]
fn orders_instants_rather_than_their_text_so_a_utc_offset_cannot_invert_them() {
    // 01:00+02:00 is 23:00 the previous day, so as text it sorts after the Z stamp and in time
    // it falls well before it.
    assert_eq!(
        newest_pull_request_commit_at(&[
            commit("a", "", "2026-07-05T00:30:00Z"),
            commit("b", "", "2026-07-05T01:00:00+02:00"),
        ]),
        Some("2026-07-05T00:30:00Z".to_owned())
    );
    assert!(!is_pull_request_verdict_stale(
        "2026-07-05T00:30:00Z",
        Some("2026-07-05T01:00:00+02:00")
    ));
    // A timestamp nothing can parse is not a position, so it settles nothing either way.
    assert!(!is_pull_request_verdict_stale(
        "2026-07-01T00:00:00Z",
        Some("not a date")
    ));
    assert_eq!(
        newest_pull_request_commit_at(&[commit("a", "", "not a date")]),
        None
    );
}

#[test]
fn shows_nothing_for_a_reviewer_whose_verdict_was_dismissed() {
    assert!(
        latest_pull_request_review_outcomes(
            &[
                review_comment("c1", Some("bilal"), "APPROVED", "2026-07-01T00:00:00Z"),
                review_comment("c2", Some("bilal"), "DISMISSED", "2026-07-02T00:00:00Z"),
            ],
            &[],
        )
        .is_empty()
    );
}

#[test]
fn orders_creation_commits_and_comments_newest_first() {
    // What happened last is what the reader opening the tab is asking about.
    assert_eq!(
        event_ids(&build_pull_request_timeline(&timeline_source())),
        strs(&["c1", "1baf7bdcafe", "created"])
    );
}

#[test]
fn carries_the_comment_url_and_leaves_the_events_the_host_cannot_address_without_one() {
    let events = build_pull_request_timeline(&PullRequestTimelineSource {
        comments: vec![PullRequestComment {
            url: Some("https://example.test/pull/1#c1".into()),
            ..base_comment()
        }],
        ..timeline_source()
    });
    assert_eq!(
        events
            .iter()
            .map(|event| event.url.clone())
            .collect::<Vec<_>>(),
        vec![
            Some("https://example.test/pull/1#c1".to_owned()),
            None,
            None
        ]
    );
}

#[test]
fn carries_actors_and_review_context_into_the_presentation_model() {
    let commit_authors = vec![
        PullRequestActor {
            is_bot: None,
            login: tne("octocat"),
            name: None,
            avatar_url: Some("https://example.test/octocat.png".into()),
        },
        PullRequestActor {
            is_bot: None,
            login: tne("pair"),
            name: Some("Pair Author".into()),
            avatar_url: None,
        },
    ];
    let events = build_pull_request_timeline(&PullRequestTimelineSource {
        commits: vec![PullRequestCommit {
            authors: Some(commit_authors.clone()),
            ..commit("1baf7bdcafe", "add the page", "2026-07-02T00:00:00Z")
        }],
        comments: vec![PullRequestComment {
            kind: PullRequestCommentKind::Review,
            path: Some("src/app.ts".into()),
            review_state: Some("APPROVED".into()),
            ..base_comment()
        }],
        ..timeline_source()
    });

    assert_eq!(
        events
            .iter()
            .find(|event| event.kind == PullRequestTimelineEventKind::Commit)
            .map(|event| event.commit_authors.clone()),
        Some(commit_authors)
    );
    let review = events
        .iter()
        .find(|event| event.kind == PullRequestTimelineEventKind::Review)
        .expect("the review is in the timeline");
    assert_eq!(review.actor, Some(actor("bilal")));
    assert_eq!(review.path, Some("src/app.ts".to_owned()));
    assert_eq!(review.review_state, Some("APPROVED".to_owned()));
}

#[test]
fn carries_each_commits_line_counts_into_its_timeline_event() {
    let events = build_pull_request_timeline(&PullRequestTimelineSource {
        commits: vec![PullRequestCommit {
            additions: Some(12),
            deletions: Some(4),
            ..commit("1baf7bdcafe", "add the page", "2026-07-02T00:00:00Z")
        }],
        ..timeline_source()
    });
    let commit_event = events
        .iter()
        .find(|event| event.kind == PullRequestTimelineEventKind::Commit)
        .expect("the commit is in the timeline");
    assert_eq!(commit_event.additions, Some(12));
    assert_eq!(commit_event.deletions, Some(4));
}

#[test]
fn drops_a_body_that_is_nothing_but_a_bots_html_comment_and_keeps_one_that_says_more() {
    let events = build_pull_request_timeline(&PullRequestTimelineSource {
        comments: vec![
            PullRequestComment {
                body: "<!-- MURMUR_IGNORE -->".into(),
                ..base_comment()
            },
            PullRequestComment {
                id: tne("c2"),
                body: "<!-- summarize by coderabbit.ai -->\nNeeds a test.".into(),
                created_at: "2026-07-04T00:00:00Z".into(),
                ..base_comment()
            },
        ],
        ..timeline_source()
    });
    assert_eq!(
        events
            .iter()
            .find(|event| event.id == "c1")
            .map(|event| event.body.clone()),
        Some(None)
    );
    // Kept whole: the renderer drops the marker itself, and stripping it here would also
    // strip an HTML comment a reviewer quoted inside a code fence.
    assert_eq!(
        events
            .iter()
            .find(|event| event.id == "c2")
            .and_then(|event| event.body.clone()),
        Some("<!-- summarize by coderabbit.ai -->\nNeeds a test.".to_owned())
    );
}

#[test]
fn calls_a_comment_markdown_and_a_commit_headline_plain_text() {
    let events = build_pull_request_timeline(&timeline_source());
    // A headline reading `fix: drop *legacy* path` is not asking for emphasis.
    assert!(
        events
            .iter()
            .any(|event| event.title.starts_with("Commit") && !event.markdown)
    );
    assert!(
        events
            .iter()
            .find(|event| event.id == "c1")
            .is_some_and(|event| event.markdown)
    );
}

#[test]
fn reports_a_merge_rather_than_the_close_github_records_alongside_it() {
    let events = build_pull_request_timeline(&PullRequestTimelineSource {
        merged_at: Some("2026-07-04T00:00:00Z".into()),
        closed_at: Some("2026-07-04T00:00:00Z".into()),
        ..timeline_source()
    });
    // Newest first, so the terminal event opens the list rather than ending it.
    assert_eq!(events[0].id, "merged");
    assert!(!events.iter().any(|event| event.id == "closed"));
}

#[test]
fn groups_conversation_sections_without_crossing_commits_or_pr_updates() {
    let events = build_pull_request_timeline(&PullRequestTimelineSource {
        comments: vec![
            PullRequestComment {
                id: tne("new-comment-1"),
                created_at: "2026-07-04T00:00:00Z".into(),
                ..base_comment()
            },
            PullRequestComment {
                id: tne("new-comment-2"),
                created_at: "2026-07-03T00:00:00Z".into(),
                ..base_comment()
            },
            PullRequestComment {
                id: tne("old-comment-1"),
                created_at: "2026-07-01T12:00:00Z".into(),
                ..base_comment()
            },
            PullRequestComment {
                id: tne("old-comment-2"),
                created_at: "2026-07-01T06:00:00Z".into(),
                ..base_comment()
            },
        ],
        merged_at: Some("2026-07-05T00:00:00Z".into()),
        ..timeline_source()
    });

    assert_eq!(
        timeline_rows(&events),
        vec![
            strs(&["event", "merged"]),
            strs(&["comments", "new-comment-1", "new-comment-2"]),
            strs(&["event", "1baf7bdcafe"]),
            strs(&["comments", "old-comment-1", "old-comment-2"]),
            strs(&["event", "created"]),
        ]
    );
}

#[test]
fn keeps_a_verdict_out_of_the_collapsed_conversation_it_was_submitted_in() {
    let events = build_pull_request_timeline(&PullRequestTimelineSource {
        comments: vec![
            PullRequestComment {
                id: tne("chatter-1"),
                created_at: "2026-07-05T00:00:00Z".into(),
                ..base_comment()
            },
            PullRequestComment {
                id: tne("approval"),
                kind: PullRequestCommentKind::Review,
                body: String::new(),
                review_state: Some("APPROVED".into()),
                created_at: "2026-07-04T00:00:00Z".into(),
                ..base_comment()
            },
            PullRequestComment {
                id: tne("chatter-2"),
                created_at: "2026-07-03T00:00:00Z".into(),
                ..base_comment()
            },
            // A review without a verdict is ordinary conversation and still groups.
            PullRequestComment {
                id: tne("remark"),
                kind: PullRequestCommentKind::Review,
                review_state: Some("COMMENTED".into()),
                created_at: "2026-07-02T12:00:00Z".into(),
                ..base_comment()
            },
        ],
        ..timeline_source()
    });

    assert_eq!(
        timeline_rows(&events),
        vec![
            strs(&["comments", "chatter-1"]),
            strs(&["event", "approval"]),
            strs(&["comments", "chatter-2", "remark"]),
            strs(&["event", "1baf7bdcafe"]),
            strs(&["event", "created"]),
        ]
    );
}

fn review_thread_with(id: &str, body: &str) -> PullRequestReviewThread {
    PullRequestReviewThread {
        id: tne(id),
        ..review_thread(body)
    }
}

#[test]
fn attaches_a_review_thread_as_an_annotation_instead_of_quoting_it_in_the_prompt() {
    let handoff = build_fix_findings_handoff(&FixFindingsInput {
        review_threads: vec![review_thread("rename the helper")],
        checks: vec![],
        ..base_findings_input()
    });
    assert_eq!(handoff.review_comments.len(), 1);
    let comment = &handoff.review_comments[0];
    assert_eq!(comment.file_path, "apps/web/src/page.tsx");
    assert_eq!(comment.range_label, "L12");
    assert_eq!(comment.start_index, 11);
    assert_eq!(comment.end_index, 11);
    assert_eq!(comment.text, "reviewer: rename the helper");
    assert!(!handoff.prompt.contains("rename the helper"));
    assert!(handoff.prompt.contains("untrusted data"));
}

#[test]
fn names_the_pre_change_side_and_a_thread_the_host_pinned_to_the_file_rather_than_a_line() {
    let handoff = build_fix_findings_handoff(&FixFindingsInput {
        review_threads: vec![
            PullRequestReviewThread {
                side: PullRequestDiffSide::Left,
                ..review_thread("was this deleted on purpose?")
            },
            PullRequestReviewThread {
                line: None,
                ..review_thread_with("t2", "wrong module")
            },
        ],
        checks: vec![],
        ..base_findings_input()
    });
    assert_eq!(
        handoff
            .review_comments
            .iter()
            .map(|comment| comment.range_label.clone())
            .collect::<Vec<_>>(),
        strs(&["L12 (before)", "file"])
    );
}

#[test]
fn keeps_failing_checks_in_the_prompt_having_no_line_to_attach_them_to() {
    let handoff = build_fix_findings_handoff(&FixFindingsInput {
        review_threads: vec![],
        checks: vec![failing_check()],
        ..base_findings_input()
    });
    assert!(handoff.prompt.contains("> typecheck — 2 errors"));
    assert!(handoff.review_comments.is_empty());
}

#[test]
fn leaves_out_a_resolved_conversation_and_one_nobody_wrote_in() {
    let handoff = build_fix_findings_handoff(&FixFindingsInput {
        review_threads: vec![
            PullRequestReviewThread {
                is_resolved: true,
                ..review_thread("already handled")
            },
            review_thread_with("t2", "   "),
            review_thread_with("t3", "still open"),
        ],
        checks: vec![],
        ..base_findings_input()
    });
    assert_eq!(
        handoff
            .review_comments
            .iter()
            .map(|comment| comment.text.clone())
            .collect::<Vec<_>>(),
        strs(&["reviewer: still open"])
    );
}

#[test]
fn says_so_plainly_when_there_is_nothing_actionable() {
    let handoff = build_fix_findings_handoff(&base_findings_input());
    assert!(
        handoff
            .prompt
            .contains("No unresolved review findings were returned")
    );
    assert!(handoff.review_comments.is_empty());
}

#[test]
fn bounds_a_hostile_review_body_instead_of_attaching_it_whole() {
    let handoff = build_fix_findings_handoff(&FixFindingsInput {
        review_threads: vec![review_thread(&"x".repeat(5_000))],
        checks: vec![],
        ..base_findings_input()
    });
    let text = &handoff.review_comments[0].text;
    assert_eq!(js_len(text), 1_000);
    assert!(text.ends_with("..."));
}

#[test]
fn keeps_the_newest_threads_and_the_failing_checks_when_it_has_to_cut() {
    let handoff = build_fix_findings_handoff(&FixFindingsInput {
        review_threads: (0..25)
            .map(|index| review_thread_with(&format!("t{index}"), &format!("finding {index}")))
            .collect(),
        checks: vec![failing_check()],
        ..base_findings_input()
    });
    // Oldest threads are dropped rather than the current failure and the recent feedback.
    let texts: Vec<String> = handoff
        .review_comments
        .iter()
        .map(|comment| comment.text.clone())
        .collect();
    assert_eq!(texts.len(), 19);
    assert_eq!(
        texts.last().map(String::as_str),
        Some("reviewer: finding 24")
    );
    assert!(!texts.contains(&"reviewer: finding 0".to_owned()));
    assert!(handoff.prompt.contains("typecheck"));
    assert!(handoff.prompt.contains("6 further findings were omitted"));
}

fn remark_with(id: &str, kind: PullRequestCommentKind) -> PullRequestComment {
    PullRequestComment {
        id: tne(id),
        kind,
        ..review_remark()
    }
}

#[test]
fn carries_a_review_submitted_with_words_but_no_line_which_has_nothing_to_attach_to() {
    let handoff = build_fix_findings_handoff(&FixFindingsInput {
        comments: vec![review_remark()],
        ..base_findings_input()
    });

    // It has no file and no line, so it travels the way a failing check does rather than
    // being dropped for lacking somewhere to point.
    assert!(handoff.review_comments.is_empty());
    assert!(handoff.prompt.contains("revert the middleware change"));
    assert!(!handoff.prompt.contains("No unresolved review findings"));
}

#[test]
fn carries_a_hosts_line_comments_when_it_reports_no_threads_at_all() {
    // Azure DevOps has no diff to pin a conversation to, so every remark arrives this way.
    let handoff = build_fix_findings_handoff(&FixFindingsInput {
        comments: vec![PullRequestComment {
            path: Some("src/app.ts".into()),
            ..remark_with("a1", PullRequestCommentKind::ReviewComment)
        }],
        ..base_findings_input()
    });

    assert!(handoff.prompt.contains("src/app.ts"));
    assert!(handoff.prompt.contains("revert the middleware change"));
}

#[test]
fn does_not_repeat_a_remark_that_was_already_attached_as_a_thread() {
    let attached_id = "t1c1";
    let handoff = build_fix_findings_handoff(&FixFindingsInput {
        review_threads: vec![PullRequestReviewThread {
            id: tne("t1"),
            path: tne("src/app.ts"),
            comments: vec![PullRequestThreadComment {
                id: tne(attached_id),
                author: Some(actor("julius")),
                body: "rename the helper".into(),
                created_at: "2026-07-01T00:00:00Z".into(),
                edited_at: None,
                url: None,
                reactions: None,
                can_edit: None,
            }],
            ..review_thread("unused")
        }],
        comments: vec![PullRequestComment {
            body: "rename the helper".into(),
            ..remark_with(attached_id, PullRequestCommentKind::ReviewComment)
        }],
        ..base_findings_input()
    });

    assert_eq!(handoff.review_comments.len(), 1);
    assert!(!handoff.prompt.contains("rename the helper"));
}

#[test]
fn attaches_a_thread_as_its_own_annotation_resolved_or_not() {
    // The bulk handoff skips resolved threads as finished work. Pressing the button on one is
    // an explicit request for that thread, so it is not second-guessed.
    let handoff = build_fix_finding_handoff(&base_finding_input(PullRequestFinding::Thread(
        PullRequestReviewThread {
            is_resolved: true,
            ..review_thread("rename the helper")
        },
    )));
    assert_eq!(handoff.review_comments.len(), 1);
    assert_eq!(
        handoff.review_comments[0].file_path,
        "apps/web/src/page.tsx"
    );
    assert_eq!(handoff.review_comments[0].range_label, "L12");
    assert!(handoff.prompt.contains("attached to this message"));
    assert!(!handoff.prompt.contains("rename the helper"));
}

#[test]
fn quotes_a_review_remark_which_has_no_line_to_attach_it_to() {
    let handoff = build_fix_finding_handoff(&base_finding_input(PullRequestFinding::Comment(
        PullRequestComment {
            body: "this breaks SSO auth".into(),
            path: Some("apps/server/src/auth.ts".into()),
            ..review_remark()
        },
    )));
    assert!(handoff.review_comments.is_empty());
    assert!(
        handoff
            .prompt
            .contains("> julius on `apps/server/src/auth.ts`: this breaks SSO auth")
    );
}
