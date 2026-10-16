//! Port of upstream/apps/server/src/orchestration-v2/pullRequestWatch.test.ts.

use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::orchestration_v2::{
    OrchestrationV2Notification, OrchestrationV2NotificationOutcome,
    OrchestrationV2NotificationSource,
};
use turin_contracts::pull_request::{
    PullRequestActor, PullRequestCheck, PullRequestCheckStatus, PullRequestComment,
    PullRequestCommentKind, PullRequestMergeability,
};
use turin_contracts::thread_pull_request::ThreadPullRequestWatch;

use super::*;

const STARTED: &str = "2026-10-02T12:00:00.000Z";

fn tne(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).expect("non-empty fixture")
}

fn watch() -> ThreadPullRequestWatch {
    ThreadPullRequestWatch {
        started_at: STARTED.to_owned(),
        head_sha: None,
        failed_checks: Vec::new(),
        passed: false,
        passed_checks: Vec::new(),
        remarks_through: STARTED.to_owned(),
        remark_ids: Vec::new(),
        conflicting: false,
        wakes: 0,
    }
}

fn check(name: &str, status: PullRequestCheckStatus) -> PullRequestCheck {
    PullRequestCheck {
        name: tne(name),
        status,
        description: None,
        url: Some(format!("https://ci.example/{name}")),
        required: None,
    }
}

fn required(name: &str, status: PullRequestCheckStatus) -> PullRequestCheck {
    PullRequestCheck {
        required: Some(true),
        ..check(name, status)
    }
}

fn detail() -> WatchDetail {
    WatchDetail {
        head_sha: Some("aaaaaaaaaa".to_owned()),
        checks: vec![
            check("lint", PullRequestCheckStatus::Success),
            check("test", PullRequestCheckStatus::Pending),
        ],
        mergeability: PullRequestMergeability::Mergeable,
        viewer: Some("agent-user".to_owned()),
        author: Some("agent-user".to_owned()),
    }
}

fn remark(login: &str, created_at: &str, body: &str) -> PullRequestComment {
    PullRequestComment {
        id: tne(&format!("{login}-{created_at}")),
        kind: PullRequestCommentKind::ReviewComment,
        author: Some(PullRequestActor {
            is_bot: None,
            login: tne(login),
            name: None,
            avatar_url: None,
        }),
        body: body.to_owned(),
        created_at: created_at.to_owned(),
        edited_at: None,
        url: Some(format!("https://github.com/o/r/pull/1#{login}")),
        path: Some("src/index.ts".to_owned()),
        review_state: None,
        reactions: None,
        can_edit: None,
    }
}

fn remark_with_id(id: &str, login: &str, created_at: &str) -> PullRequestComment {
    PullRequestComment {
        id: tne(id),
        ..remark(login, created_at, "Please rename this.")
    }
}

fn evaluate(
    watch: &ThreadPullRequestWatch,
    detail: &WatchDetail,
    remarks: Option<&[PullRequestComment]>,
) -> PullRequestWatchReport {
    evaluate_pull_request_watch(watch, detail, remarks)
}

#[test]
fn reports_each_failure_at_once_even_while_another_check_never_finishes() {
    let bot = check("CodeRabbit", PullRequestCheckStatus::Pending);
    let first = WatchDetail {
        checks: vec![
            check("lint", PullRequestCheckStatus::Failure),
            check("test", PullRequestCheckStatus::Pending),
            bot.clone(),
        ],
        ..detail()
    };
    let lint = evaluate(&watch(), &first, Some(&[]));
    assert_eq!(
        lint.changes,
        vec![PullRequestWatchChange::ChecksFailed {
            failed: vec![check("lint", PullRequestCheckStatus::Failure)]
        }]
    );
    assert!(evaluate(&lint.next, &first, Some(&[])).changes.is_empty());

    // A different job failing later is news of its own.
    let second = WatchDetail {
        checks: vec![
            check("lint", PullRequestCheckStatus::Failure),
            check("test", PullRequestCheckStatus::Failure),
            bot.clone(),
        ],
        ..detail()
    };
    let test = evaluate(&lint.next, &second, Some(&[]));
    assert_eq!(
        test.changes,
        vec![PullRequestWatchChange::ChecksFailed {
            failed: vec![check("test", PullRequestCheckStatus::Failure)]
        }]
    );

    // A rerun leaves the list while it runs, so failing again is reported again.
    let rerun = evaluate(&test.next, &first, Some(&[]));
    assert_eq!(evaluate(&rerun.next, &second, Some(&[])).changes.len(), 1);

    // A push reports its failures, even ones that failed between two passes.
    let pushed = WatchDetail {
        head_sha: Some("bbbbbbbbbb".to_owned()),
        ..second.clone()
    };
    assert_eq!(evaluate(&test.next, &pushed, Some(&[])).changes.len(), 1);
}

#[test]
fn reports_passed_once_the_required_checks_pass_whatever_the_others_do() {
    let green = WatchDetail {
        checks: vec![
            required("test", PullRequestCheckStatus::Success),
            required("lint", PullRequestCheckStatus::Success),
            check("bot", PullRequestCheckStatus::Pending),
        ],
        ..detail()
    };
    let passed = evaluate(&watch(), &green, Some(&[]));
    assert_eq!(
        passed.changes,
        vec![PullRequestWatchChange::ChecksPassed {
            count: 2,
            required: true
        }]
    );
    assert!(evaluate(&passed.next, &green, Some(&[])).changes.is_empty());

    // Where nothing is marked required, every check has to pass.
    let plain = WatchDetail {
        checks: vec![
            check("test", PullRequestCheckStatus::Success),
            check("bot", PullRequestCheckStatus::Pending),
        ],
        ..detail()
    };
    assert!(evaluate(&watch(), &plain, Some(&[])).changes.is_empty());
}

#[test]
fn reports_passed_again_when_a_required_check_shows_up_already_passed() {
    let tests = WatchDetail {
        checks: vec![
            required("Tests", PullRequestCheckStatus::Success),
            check("Smoke Tests", PullRequestCheckStatus::Pending),
        ],
        ..detail()
    };
    let first = evaluate(&watch(), &tests, Some(&[]));
    assert_eq!(
        first.changes,
        vec![PullRequestWatchChange::ChecksPassed {
            count: 1,
            required: true
        }]
    );

    // The gate job was created and finished between two passes, so it was never seen pending.
    let gated = WatchDetail {
        checks: vec![
            required("Tests", PullRequestCheckStatus::Success),
            check("Smoke Tests", PullRequestCheckStatus::Success),
            required("Smoke Tests Gate", PullRequestCheckStatus::Success),
        ],
        ..detail()
    };
    let second = evaluate(&first.next, &gated, Some(&[]));
    assert_eq!(
        second.changes,
        vec![PullRequestWatchChange::ChecksPassed {
            count: 2,
            required: true
        }]
    );
    assert!(evaluate(&second.next, &gated, Some(&[])).changes.is_empty());

    // Seen pending first, the gate is reported once it passes, as before.
    let pending = WatchDetail {
        checks: vec![
            required("Tests", PullRequestCheckStatus::Success),
            required("Smoke Tests Gate", PullRequestCheckStatus::Pending),
        ],
        ..detail()
    };
    let waiting = evaluate(&first.next, &pending, Some(&[]));
    assert!(waiting.changes.is_empty());
    assert_eq!(
        evaluate(&waiting.next, &gated, Some(&[])).changes,
        vec![PullRequestWatchChange::ChecksPassed {
            count: 2,
            required: true
        }]
    );
}

#[test]
fn does_not_report_passed_again_for_a_new_passed_check_where_none_is_required() {
    let first = evaluate(
        &watch(),
        &WatchDetail {
            checks: vec![check("test", PullRequestCheckStatus::Success)],
            ..detail()
        },
        Some(&[]),
    );
    assert_eq!(
        first.changes,
        vec![PullRequestWatchChange::ChecksPassed {
            count: 1,
            required: false
        }]
    );
    let both = WatchDetail {
        checks: vec![
            check("test", PullRequestCheckStatus::Success),
            check("lint", PullRequestCheckStatus::Success),
        ],
        ..detail()
    };
    assert!(evaluate(&first.next, &both, Some(&[])).changes.is_empty());
}

#[test]
fn does_not_wake_a_watch_saved_before_passed_checks_were_recorded() {
    let green = WatchDetail {
        checks: vec![PullRequestCheck {
            required: Some(true),
            ..check("test", PullRequestCheckStatus::Success)
        }],
        ..detail()
    };
    let told = ThreadPullRequestWatch {
        head_sha: Some(tne("aaaaaaaaaa")),
        passed: true,
        ..watch()
    };
    let saved = evaluate(&told, &green, Some(&[]));
    assert!(saved.changes.is_empty());
    assert_eq!(saved.next.passed_checks, vec![tne("test")]);
}

#[test]
fn keeps_remarks_for_a_later_pass_when_the_conversation_was_not_read_whole() {
    let comments = [remark(
        "reviewer",
        "2026-10-02T12:06:00Z",
        "Please rename this.",
    )];
    let partial = evaluate(&watch(), &detail(), None);
    assert!(partial.changes.is_empty());
    let later = evaluate(&partial.next, &detail(), Some(&comments));
    assert!(matches!(
        later.changes.first(),
        Some(PullRequestWatchChange::Remarks { .. })
    ));
}

#[test]
fn reports_a_remark_that_shows_up_late_with_the_same_time_as_a_reported_one() {
    let first = remark("reviewer", "2026-10-02T12:06:00Z", "Please rename this.");
    let late = remark_with_id("late", "bot", "2026-10-02T12:06:00Z");
    let reported = evaluate(&watch(), &detail(), Some(std::slice::from_ref(&first)));
    let again = evaluate(
        &reported.next,
        &detail(),
        Some(&[first.clone(), late.clone()]),
    );
    assert_eq!(
        again.changes,
        vec![PullRequestWatchChange::Remarks {
            remarks: vec![late]
        }]
    );
    assert_eq!(
        again.next.remark_ids,
        vec![tne(first.id.as_str()), tne("late")]
    );
}

#[test]
fn reports_edits_after_the_watermark_once_and_counts_them_toward_the_wake_limit() {
    let old = remark(
        "greptile[bot]",
        "2026-10-02T11:00:00Z",
        "Please rename this.",
    );
    let watching = ThreadPullRequestWatch {
        head_sha: Some(tne("aaaaaaaaaa")),
        remark_ids: vec![old.id.clone()],
        wakes: PULL_REQUEST_WATCH_WAKE_LIMIT - 1,
        ..watch()
    };
    assert!(
        evaluate(&watching, &detail(), Some(std::slice::from_ref(&old)))
            .changes
            .is_empty()
    );
    let edited = PullRequestComment {
        edited_at: Some(Some("2026-10-02T12:06:00Z".to_owned())),
        ..old.clone()
    };
    let report = evaluate(&watching, &detail(), Some(std::slice::from_ref(&edited)));
    assert_eq!(
        report.changes,
        vec![PullRequestWatchChange::Remarks {
            remarks: vec![edited.clone()]
        }]
    );
    assert_eq!(report.next.remarks_through, "2026-10-02T12:06:00Z");
    assert_eq!(report.next.remark_ids, vec![old.id.clone()]);
    assert!(report.exhausted);
    assert!(
        evaluate(&report.next, &detail(), Some(std::slice::from_ref(&edited)))
            .changes
            .is_empty()
    );
    let late = PullRequestComment {
        id: tne("late"),
        ..edited.clone()
    };
    assert_eq!(
        evaluate(
            &report.next,
            &detail(),
            Some(&[edited.clone(), late.clone()])
        )
        .changes,
        vec![PullRequestWatchChange::Remarks {
            remarks: vec![late]
        }]
    );
}

#[test]
fn does_not_treat_a_failed_check_read_as_a_rerun() {
    let failed = WatchDetail {
        checks: vec![check("lint", PullRequestCheckStatus::Failure)],
        ..detail()
    };
    let reported = evaluate(&watch(), &failed, Some(&[]));
    assert_eq!(reported.changes.len(), 1);
    let unreadable = evaluate(
        &reported.next,
        &WatchDetail {
            checks: Vec::new(),
            ..detail()
        },
        Some(&[]),
    );
    assert!(
        evaluate(&unreadable.next, &failed, Some(&[]))
            .changes
            .is_empty()
    );
}

#[test]
fn wakes_for_the_pull_request_author_when_the_agent_is_someone_else() {
    let contributor = WatchDetail {
        viewer: None,
        author: Some("contributor".to_owned()),
        ..detail()
    };
    let contributor_with_viewer = WatchDetail {
        viewer: Some("agent-user".to_owned()),
        ..contributor.clone()
    };
    let reply = remark("contributor", "2026-10-02T12:06:00Z", "Please rename this.");
    assert_eq!(
        evaluate(
            &watch(),
            &contributor_with_viewer,
            Some(std::slice::from_ref(&reply))
        )
        .changes,
        vec![PullRequestWatchChange::Remarks {
            remarks: vec![reply.clone()]
        }]
    );
    // Without a viewer, the author is taken to be the agent.
    assert!(
        evaluate(&watch(), &contributor, Some(std::slice::from_ref(&reply)))
            .changes
            .is_empty()
    );
}

#[test]
fn reports_remarks_from_others_once_and_never_the_agents_own() {
    let comments = [
        remark(
            "agent-user",
            "2026-10-02T12:05:00Z",
            "Fixed in the latest push.",
        ),
        remark(
            "macroscope-app[bot]",
            "2026-10-02T12:06:00Z",
            "Please rename this.",
        ),
        remark("reviewer", "2026-10-02T11:00:00Z", "Older than the watch."),
    ];
    let report = evaluate(&watch(), &detail(), Some(&comments));
    assert_eq!(
        report.changes,
        vec![PullRequestWatchChange::Remarks {
            remarks: vec![comments[1].clone()]
        }]
    );
    assert_eq!(report.next.remarks_through, "2026-10-02T12:06:00Z");
    assert!(
        evaluate(&report.next, &detail(), Some(&comments))
            .changes
            .is_empty()
    );
}

#[test]
fn reports_a_conflict_once_until_the_branch_is_clean_again() {
    let conflicting = WatchDetail {
        mergeability: PullRequestMergeability::Conflicting,
        ..detail()
    };
    let first = evaluate(&watch(), &conflicting, Some(&[]));
    assert_eq!(first.changes, vec![PullRequestWatchChange::Conflicting]);
    // GitHub answers "unknown" while it recomputes after a push; that is not a resolution.
    let recomputing = evaluate(
        &first.next,
        &WatchDetail {
            mergeability: PullRequestMergeability::Unknown,
            ..detail()
        },
        Some(&[]),
    );
    assert!(
        evaluate(&recomputing.next, &conflicting, Some(&[]))
            .changes
            .is_empty()
    );
    let clean = evaluate(&first.next, &detail(), Some(&[]));
    assert_eq!(
        evaluate(&clean.next, &conflicting, Some(&[])).changes,
        vec![PullRequestWatchChange::Conflicting]
    );
}

#[test]
fn does_not_spend_the_comment_wake_limit_on_check_results() {
    let tired = ThreadPullRequestWatch {
        head_sha: Some(tne("aaaaaaaaaa")),
        wakes: PULL_REQUEST_WATCH_WAKE_LIMIT - 1,
        ..watch()
    };
    let result = evaluate(
        &tired,
        &WatchDetail {
            checks: vec![check("lint", PullRequestCheckStatus::Failure)],
            ..detail()
        },
        Some(&[]),
    );
    assert!(!result.exhausted);
    assert_eq!(result.next.wakes, 0);
}

#[test]
fn stops_after_the_wake_limit_unless_the_head_moves() {
    let comments = [remark(
        "reviewer",
        "2026-10-02T12:10:00Z",
        "Please rename this.",
    )];
    let tired = ThreadPullRequestWatch {
        head_sha: Some(tne("aaaaaaaaaa")),
        wakes: PULL_REQUEST_WATCH_WAKE_LIMIT - 1,
        ..watch()
    };
    assert!(evaluate(&tired, &detail(), Some(&comments)).exhausted);
    let pushed = evaluate(
        &tired,
        &WatchDetail {
            head_sha: Some("cccccccccc".to_owned()),
            ..detail()
        },
        Some(&comments),
    );
    assert!(!pushed.exhausted);
    assert_eq!(pushed.next.wakes, 1);
}

#[test]
fn watch_message_tells_the_agent_what_changed_and_marks_failures_for_the_timeline() {
    let report = evaluate(
        &watch(),
        &WatchDetail {
            checks: vec![check("lint", PullRequestCheckStatus::Failure)],
            ..detail()
        },
        Some(&[remark(
            "reviewer",
            "2026-10-02T12:10:00Z",
            "<!-- bot -->Needs a test.",
        )]),
    );
    let (text, notification) = pull_request_watch_message(PullRequestWatchMessageInput {
        number: 12,
        url: "https://github.com/o/r/pull/12",
        base_branch: "main",
        head_sha: report.next.head_sha.as_ref().map(|sha| sha.as_str()),
        report: &report,
    });
    assert!(text.contains("- Checks failed on aaaaaaa:\n  - lint https://ci.example/lint"));
    assert!(text.contains("  - reviewer on src/index.ts: \"Needs a test.\""));
    assert!(text.contains("unwatch_pull_request"));
    assert_eq!(
        notification,
        OrchestrationV2Notification {
            source: OrchestrationV2NotificationSource::Monitor,
            outcome: OrchestrationV2NotificationOutcome::Failed,
            summary: tne("#12: checks failed, new comments"),
            detail: None,
        }
    );
}
