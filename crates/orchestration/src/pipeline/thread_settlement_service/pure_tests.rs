use super::*;
use serde_json::json;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ActivityRunStatus, OrchestrationV2PendingBackgroundTask,
    OrchestrationV2PendingRuntimeRequestSummary, OrchestrationV2SettleOverride,
    OrchestrationV2ShellThreadStatus,
};
use turin_contracts::pull_request::PullRequestState;
use turin_contracts::settings::{
    ProjectSettingsOverrides, ServerSettings, SidebarAutoSettleAfterDays,
};
use turin_contracts::thread_pull_request::{
    ThreadLinkedPullRequest, ThreadPullRequestLink, ThreadPullRequestLinkSource,
};
use turin_contracts::{ProjectId, RunId, ThreadId, TrimmedNonEmptyString};

const DAY: i64 = DAY_MS;
// `Date.parse("2026-06-10T12:00:00.000Z")`
const NOW_MS: i64 = 1_781_092_800_000;

fn defaults() -> ServerSettings {
    serde_json::from_value(json!({})).unwrap()
}

fn at(offset_ms: i64) -> UtcDateTime {
    utc_from_millis(NOW_MS + offset_ms)
}

fn shell(
    overrides: impl FnOnce(&mut ProjectionSettlementCandidate),
) -> ProjectionSettlementCandidate {
    let mut thread = ProjectionSettlementCandidate {
        id: ThreadId::new("thread-1").unwrap(),
        project_id: ProjectId::new("project-1").unwrap(),
        branch: None,
        worktree_path: None,
        pull_requests: None,
        linked_pull_request: None,
        branch_pull_request: None,
        created_at: at(-30 * DAY),
        updated_at: at(-10 * DAY),
        archived_at: None,
        settled_override: None,
        pinned_at: None,
        auto_settle_disabled_at: None,
        snoozed_until: None,
        snoozed_at: None,
        last_snooze_wake_at: None,
        latest_run_id: None,
        latest_run_requested_at: None,
        latest_run_started_at: None,
        latest_run_completed_at: None,
        latest_user_message_at: None,
        latest_user_authored_message_at: None,
        status: OrchestrationV2ShellThreadStatus::Idle,
        activity_run_started_at: None,
        activity_run_status: None,
        pending_runtime_request: None,
        pending_background_tasks: vec![],
    };
    overrides(&mut thread);
    // A fixture's user message is one the user wrote unless it sets the authored time.
    if thread.latest_user_authored_message_at.is_none() {
        thread.latest_user_authored_message_at = thread.latest_user_message_at;
    }
    thread
}

fn pending_request() -> OrchestrationV2PendingRuntimeRequestSummary {
    serde_json::from_value(json!({
        "id": "runtime-request:pending",
        "kind": "command",
        "createdAt": "2026-06-10T11:00:00.000Z",
    }))
    .unwrap()
}

fn merged_pull_request(merged_at_offset: i64) -> SettlementPullRequest {
    SettlementPullRequest {
        state: PullRequestState::Merged,
        closed_at: None,
        merged_at: Some(at(merged_at_offset).to_string()),
    }
}

fn linked(number: u64) -> ThreadLinkedPullRequest {
    serde_json::from_value(json!({
        "projectId": "project-1",
        "repository": "owner/repo",
        "number": number,
        "url": format!("https://example.test/owner/repo/pull/{number}"),
    }))
    .unwrap()
}

fn input(
    thread: &ProjectionSettlementCandidate,
    pull_request: Option<SettlementPullRequest>,
    after_days: Option<f64>,
    on_merge: bool,
) -> AutoSettlementInput<'_> {
    AutoSettlementInput {
        thread,
        pull_request,
        now_ms: NOW_MS,
        auto_settle_after_days: after_days,
        auto_settle_on_merge: on_merge,
        keep_pull_requests: false,
    }
}

#[test]
fn excludes_overridden_pinned_blocked_and_working_threads() {
    assert!(is_auto_settlement_candidate(&shell(|_| {}), NOW_MS));
    let excluded = [
        shell(|t| t.archived_at = Some(at(-1))),
        shell(|t| t.settled_override = Some(OrchestrationV2SettleOverride::Settled)),
        shell(|t| t.settled_override = Some(OrchestrationV2SettleOverride::Active)),
        shell(|t| t.pinned_at = Some(at(-1))),
        shell(|t| t.auto_settle_disabled_at = Some(at(-1))),
        shell(|t| t.activity_run_status = Some(OrchestrationV2ActivityRunStatus::Running)),
        shell(|t| t.pending_runtime_request = Some(pending_request())),
        shell(|t| {
            t.pending_background_tasks = vec![OrchestrationV2PendingBackgroundTask::Subagent {
                task_id: TrimmedNonEmptyString::new("review").unwrap(),
                description: None,
                child_thread_id: None,
            }]
        }),
    ];
    for thread in excluded {
        assert!(!is_auto_settlement_candidate(&thread, NOW_MS));
    }
}

#[test]
fn settles_a_thread_whose_only_background_work_is_a_command_left_running() {
    let thread = shell(|t| {
        t.pending_background_tasks = vec![OrchestrationV2PendingBackgroundTask::Command {
            task_id: TrimmedNonEmptyString::new("dev").unwrap(),
            description: TrimmedNonEmptyString::new("vp run dev --share").ok(),
        }]
    });
    assert!(is_auto_settlement_candidate(&thread, NOW_MS));
}

#[test]
fn keeps_snoozed_threads_parked_until_they_wake_early_on_error_or_completion() {
    let snoozed = |t: &mut ProjectionSettlementCandidate| {
        t.snoozed_until = Some(at(60 * 60 * 1_000));
        t.snoozed_at = Some(at(-60 * 60 * 1_000));
    };
    assert!(!is_auto_settlement_candidate(&shell(snoozed), NOW_MS));
    let woke = |status, completed: Option<i64>| {
        shell(|t| {
            snoozed(t);
            t.status = status;
            t.latest_run_completed_at = completed.map(at);
        })
    };
    assert!(is_auto_settlement_candidate(
        &woke(
            OrchestrationV2ShellThreadStatus::Failed,
            Some(-30 * 60 * 1_000)
        ),
        NOW_MS
    ));
    assert!(is_auto_settlement_candidate(
        &woke(
            OrchestrationV2ShellThreadStatus::Idle,
            Some(-30 * 60 * 1_000)
        ),
        NOW_MS
    ));
    assert!(!is_auto_settlement_candidate(
        &woke(
            OrchestrationV2ShellThreadStatus::Failed,
            Some(-2 * 60 * 60 * 1_000)
        ),
        NOW_MS
    ));
    assert!(!is_auto_settlement_candidate(
        &woke(
            OrchestrationV2ShellThreadStatus::Failed,
            Some(-60 * 60 * 1_000)
        ),
        NOW_MS
    ));
    assert!(!is_auto_settlement_candidate(
        &woke(OrchestrationV2ShellThreadStatus::Failed, None),
        NOW_MS
    ));
    // An expired snooze is no longer a park.
    assert!(is_auto_settlement_candidate(
        &shell(|t| {
            snoozed(t);
            t.snoozed_until = Some(at(-1));
        }),
        NOW_MS
    ));
}

#[test]
fn holds_a_fresh_unadopted_user_message_inside_the_grace_window_only() {
    let fresh = shell(|t| t.latest_user_message_at = Some(at(-1_000)));
    assert!(thread_has_queued_turn_start(&fresh, NOW_MS));
    // Adoption stamps the run with the message time, clearing the hold.
    assert!(!thread_has_queued_turn_start(
        &shell(|t| {
            t.latest_user_message_at = Some(at(-1_000));
            t.latest_run_id = Some(RunId::new("run-1").unwrap());
            t.latest_run_requested_at = Some(at(-500));
        }),
        NOW_MS
    ));
    // Outside the grace window the stale message no longer blocks.
    assert!(!thread_has_queued_turn_start(
        &shell(|t| t.latest_user_message_at = Some(at(-(QUEUED_TURN_START_GRACE_MS + 1)))),
        NOW_MS
    ));
    // Client clocks ahead of the server must not extend the hold.
    assert!(!thread_has_queued_turn_start(
        &shell(|t| t.latest_user_message_at = Some(at(QUEUED_TURN_START_GRACE_MS + 1))),
        NOW_MS
    ));
    // A failed start clears the hold immediately.
    assert!(!thread_has_queued_turn_start(
        &shell(|t| {
            t.latest_user_message_at = Some(at(-1_000));
            t.status = OrchestrationV2ShellThreadStatus::Failed;
        }),
        NOW_MS
    ));
}

#[test]
fn keeps_threads_with_a_pr_from_inactivity_settlement_only_when_asked() {
    let thread = shell(|t| {
        t.latest_user_message_at = Some(at(-4 * DAY));
        t.linked_pull_request = Some(Some(linked(1)));
    });
    let mut keep = input(&thread, None, Some(3.0), true);
    keep.keep_pull_requests = true;
    assert_eq!(resolve_auto_settlement_at(&keep), None);
    keep.keep_pull_requests = false;
    assert_eq!(resolve_auto_settlement_at(&keep), Some(at(-4 * DAY)));
    assert_eq!(
        resolve_auto_settlement_at(&input(
            &thread,
            Some(merged_pull_request(-1)),
            Some(3.0),
            true
        )),
        Some(at(-4 * DAY))
    );
}

#[test]
fn uses_the_latest_activity_time_when_the_inactivity_window_elapses() {
    let idle = shell(|t| {
        t.latest_user_message_at = Some(at(-4 * DAY));
        t.latest_run_requested_at = Some(at(-4 * DAY));
        t.latest_run_started_at = Some(at(-4 * DAY));
        t.latest_run_completed_at = Some(at(-3 * DAY));
    });
    assert_eq!(
        resolve_auto_settlement_at(&input(&idle, None, Some(2.0), true)),
        Some(at(-3 * DAY))
    );
    assert_eq!(
        resolve_auto_settlement_at(&input(&idle, None, Some(5.0), true)),
        None
    );
    assert_eq!(
        resolve_auto_settlement_at(&input(&idle, None, None, true)),
        None
    );
    assert_eq!(
        resolve_auto_settlement_at(&input(&shell(|_| {}), None, Some(2.0), true)),
        None
    );
}

#[test]
fn restarts_inactivity_on_a_manual_or_timer_snooze_wake() {
    for wake in ["manual", "timer"] {
        let thread = shell(|t| {
            t.latest_run_completed_at = Some(at(-4 * DAY));
            if wake == "manual" {
                t.last_snooze_wake_at = Some(Some(at(-1_000)));
            } else {
                t.snoozed_at = Some(at(-3 * DAY));
                t.snoozed_until = Some(at(-1_000));
            }
        });
        let mut later = input(&thread, None, Some(1.0), false);
        assert_eq!(resolve_auto_settlement_at(&later), None, "{wake}");
        later.now_ms = NOW_MS + DAY;
        assert_eq!(
            resolve_auto_settlement_at(&later),
            Some(at(-1_000)),
            "{wake}"
        );
    }
}

#[test]
fn settles_on_merge_only_after_the_user_last_acted_and_preserves_the_activity_time() {
    let thread = shell(|t| {
        t.latest_user_message_at = Some(at(-2 * 60 * 60 * 1_000));
        t.latest_run_completed_at = Some(at(-90 * 60 * 1_000));
    });
    let merged = || Some(merged_pull_request(-60 * 60 * 1_000));
    assert_eq!(
        resolve_auto_settlement_at(&input(&thread, merged(), None, true)),
        Some(at(-90 * 60 * 1_000))
    );
    assert_eq!(
        resolve_auto_settlement_at(&input(&thread, merged(), None, false)),
        None
    );
    assert_eq!(
        resolve_auto_settlement_at(&input(
            &thread,
            Some(merged_pull_request(-3 * 60 * 60 * 1_000)),
            None,
            true
        )),
        None
    );
    // A thread without messages still has a stable timestamp when its PR closes.
    let bare = shell(|_| {});
    let closed = SettlementPullRequest {
        state: PullRequestState::Closed,
        closed_at: Some(at(-1).to_string()),
        merged_at: None,
    };
    assert_eq!(
        resolve_auto_settlement_at(&input(&bare, Some(closed), None, false)),
        Some(bare.created_at)
    );
}

#[test]
fn settles_on_merge_after_agent_started_runs_but_not_after_the_user_writes_again() {
    // A background command stopped after the merge and its notification
    // started a run. Only the user's own messages hold a merged thread open.
    let woken = shell(|t| {
        t.latest_user_message_at = Some(at(-30 * 60 * 1_000));
        t.latest_user_authored_message_at = Some(at(-2 * 60 * 60 * 1_000));
        t.latest_run_requested_at = Some(at(-30 * 60 * 1_000));
        t.latest_run_completed_at = Some(at(-29 * 60 * 1_000));
    });
    let merged = || Some(merged_pull_request(-60 * 60 * 1_000));
    assert_eq!(
        resolve_auto_settlement_at(&input(&woken, merged(), None, true)),
        Some(at(-29 * 60 * 1_000))
    );
    let user_wrote_again = shell(|t| {
        t.latest_user_message_at = Some(at(-30 * 60 * 1_000));
        t.latest_user_authored_message_at = Some(at(-30 * 60 * 1_000));
        t.latest_run_requested_at = Some(at(-30 * 60 * 1_000));
        t.latest_run_completed_at = Some(at(-29 * 60 * 1_000));
    });
    assert_eq!(
        resolve_auto_settlement_at(&input(&user_wrote_again, merged(), None, true)),
        None
    );
}

#[test]
fn settles_inactive_threads_even_when_their_pull_request_remains_open() {
    let thread = shell(|t| t.latest_user_message_at = Some(at(-30 * DAY)));
    let open = || {
        Some(SettlementPullRequest {
            state: PullRequestState::Open,
            closed_at: None,
            merged_at: None,
        })
    };
    assert_eq!(
        resolve_auto_settlement_at(&input(&thread, open(), Some(2.0), true)),
        Some(at(-30 * DAY))
    );
    assert_eq!(
        resolve_auto_settlement_at(&input(&thread, open(), None, true)),
        None
    );
    let recent = shell(|t| t.latest_user_message_at = Some(at(-DAY)));
    assert_eq!(
        resolve_auto_settlement_at(&input(&recent, open(), Some(2.0), true)),
        None
    );
}

#[test]
fn an_open_pull_request_link_holds_the_thread_open() {
    let thread = shell(|t| {
        t.latest_user_message_at = Some(at(-30 * DAY));
        t.pull_requests = Some(vec![ThreadPullRequestLink {
            host: TrimmedNonEmptyString::new("example.test").unwrap(),
            repository: TrimmedNonEmptyString::new("owner/repo").unwrap(),
            number: turin_contracts::PositiveInt::new(1).unwrap(),
            url: TrimmedNonEmptyString::new("https://example.test/owner/repo/pull/1").unwrap(),
            source: ThreadPullRequestLinkSource::Manual,
            linked_at: "2026-06-01T00:00:00.000Z".parse().unwrap(),
            snapshot: Some(
                serde_json::from_value(json!({
                    "state": "open",
                    "title": "Open",
                    "headBranch": "feature",
                    "baseBranch": "main",
                    "isDraft": false,
                    "updatedAt": null,
                    "syncedAt": "2026-06-01T00:00:00.000Z",
                    "closedAt": null,
                    "mergedAt": null,
                }))
                .unwrap(),
            ),
            stack: None,
            watch: None,
        }]);
    });
    assert_eq!(
        resolve_auto_settlement_at(&input(&thread, None, Some(2.0), true)),
        None
    );
}

#[test]
fn settings_key_distinguishes_inheriting_a_threshold_from_disabling_it() {
    let project = ProjectId::new("project-1").unwrap();
    let mut inherits = defaults();
    inherits.project_settings_overrides.insert(
        project.clone(),
        ProjectSettingsOverrides {
            sidebar_auto_settle_on_merge: Some(true),
            ..Default::default()
        },
    );
    let mut never = inherits.clone();
    never
        .project_settings_overrides
        .get_mut(&project)
        .unwrap()
        .sidebar_auto_settle_after_days = Some(None);
    assert_ne!(
        auto_settlement_settings_key(&inherits),
        auto_settlement_settings_key(&never)
    );
}

#[test]
fn settings_key_ignores_project_overrides_that_do_not_touch_settlement() {
    let project = ProjectId::new("project-1").unwrap();
    let other = ProjectId::new("linked-project").unwrap();
    let mut base = defaults();
    base.project_settings_overrides.insert(
        project.clone(),
        ProjectSettingsOverrides {
            sidebar_auto_settle_on_merge: Some(false),
            ..Default::default()
        },
    );
    let mut unrelated = base.clone();
    unrelated.project_settings_overrides.insert(
        other,
        ProjectSettingsOverrides {
            default_model_selection: Some(None),
            ..Default::default()
        },
    );
    unrelated
        .project_settings_overrides
        .get_mut(&project)
        .unwrap()
        .default_model_selection = Some(None);
    assert_eq!(
        auto_settlement_settings_key(&base),
        auto_settlement_settings_key(&unrelated)
    );
}

#[test]
fn snoozed_threads_wake_for_completed_work_but_not_an_interrupted_run() {
    let snoozed = |t: &mut ProjectionSettlementCandidate| {
        t.snoozed_at = Some(at(-2 * DAY));
        t.snoozed_until = Some(at(DAY));
    };
    assert!(is_snoozed(&shell(snoozed), NOW_MS));
    assert!(is_snoozed(
        &shell(|t| {
            snoozed(t);
            t.status = OrchestrationV2ShellThreadStatus::Interrupted;
            t.latest_run_completed_at = Some(at(-DAY));
        }),
        NOW_MS
    ));
    assert!(!is_snoozed(
        &shell(|t| {
            snoozed(t);
            t.status = OrchestrationV2ShellThreadStatus::Completed;
            t.latest_run_completed_at = Some(at(-DAY));
        }),
        NOW_MS
    ));
    assert!(!is_snoozed(
        &shell(|t| {
            snoozed(t);
            t.snoozed_until = Some(at(-1));
        }),
        NOW_MS
    ));
}

#[test]
fn environment_settings_resolve_per_project_with_explicit_null_as_never() {
    let project = ProjectId::new("project-1").unwrap();
    let mut settings = defaults();
    settings.sidebar_auto_settle_after_days = Some(SidebarAutoSettleAfterDays::new(7.0).unwrap());
    assert_eq!(
        resolve_settlement_settings(&settings, &project).after_days,
        Some(7.0)
    );
    settings.project_settings_overrides.insert(
        project.clone(),
        ProjectSettingsOverrides {
            sidebar_auto_settle_after_days: Some(None),
            ..Default::default()
        },
    );
    assert_eq!(
        resolve_settlement_settings(&settings, &project).after_days,
        None
    );
}
