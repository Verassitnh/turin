//! `PullRequestService.test.ts` lines 577-1988: listings over several hosts and repositories,
//! continuation cursors, routed reads, and the merge and action gates. Cases whose subject is a
//! cut host (GitLab, Azure DevOps, Bitbucket, Forgejo) are left out; see the Deviations table in
//! PORTING.md.

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use futures::FutureExt;
use futures::StreamExt;
use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tokio::sync::Notify;
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestState, PullRequestUnavailableError, PullRequestUnavailableReason,
    PullRequestViewerPermissions,
};
use turin_contracts::source_control::SourceControlProviderInfo;

use super::{counter, drain_sorted, log, rows, summary_of};
use crate::pull_request::read_cache::PullRequestReadError as PullRequestError;
use crate::pull_request::service::PullRequestService;
use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::*;
use crate::source_control_core::source_control_provider::SourceControlProviderContext;

/// `service.list(input)` as JSON, the shape every assertion below reads.
async fn list_json(service: &PullRequestService, value: Value) -> Value {
    serde_json::to_value(service.list(&input(value)).await.unwrap()).unwrap()
}

#[tokio::test(start_paused = true)]
async fn lists_github_enterprise_prs_for_a_stored_unknown_repository_after_host_discovery() {
    let h = Harness::new(ServiceOptions {
        projects: vec![
            project("p1", "enterprise", "/repo", "team/project")
                .provider("unknown")
                .host("code.example.test")
                .build(),
        ],
        providers: vec![
            FakeProvider::new("github").on_list_change_requests(|input| {
                assert_eq!(input.repo.host, "code.example.test");
                assert_eq!(input.repo.repository, "team/project");
                Ok(page(
                    vec![change_request(42, "2026-07-05T00:00:00Z")],
                    false,
                ))
            }),
        ],
        // Stands in for discovery claiming the custom host as GitHub.
        resolve_handle: Some(Arc::new(|_, context: &SourceControlProviderContext| {
            assert_eq!(
                context.remote_url,
                "https://code.example.test/team/project.git"
            );
            Ok(Some(SourceControlProviderContext {
                provider: SourceControlProviderInfo {
                    kind: kind("github"),
                    name: tne("GitHub Self-Hosted"),
                    ..context.provider.clone()
                },
                ..context.clone()
            }))
        })),
        ..ServiceOptions::default()
    })
    .await;

    let result = list_json(&h.service, json!({ "state": "open" })).await;

    assert_eq!(
        rows(&result, "entries", &["host", "number"]),
        json!([["code.example.test", 42]])
    );
}

#[tokio::test(start_paused = true)]
async fn reads_nothing_from_a_host_with_no_implementation_but_reports_it() {
    let listed = log();
    let pushed = listed.clone();
    let h = make_service(
        vec![
            project("p1", "t3code", "/a", "pingdotgg/t3code"),
            project("p2", "notes", "/b", "unused").without_repository(),
            project("p3", "on gitlab", "/c", "group/project").provider("gitlab"),
        ],
        vec![
            FakeProvider::new("github").on_list_change_requests(move |input| {
                pushed.lock().push(input.repo.repository.clone());
                Ok(page(vec![change_request(1, "2026-07-02T00:00:00Z")], false))
            }),
        ],
    )
    .await;

    let result = list_json(&h.service, json!({ "state": "open" })).await;

    assert_eq!(*listed.lock(), vec!["pingdotgg/t3code"]);
    assert_eq!(result["entries"][0]["provider"], "github");
    // The GitLab project is explained rather than quietly missing from the page.
    assert_eq!(
        rows(
            &result,
            "providers",
            &["kind", "configured", "projectCount"]
        ),
        json!([["github", true, 1], ["gitlab", false, 1]])
    );
}

#[tokio::test(start_paused = true)]
async fn asks_for_a_whole_page_of_a_host_and_for_the_readers_own_size_when_given_one() {
    let limits: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let asked = limits.clone();
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![
            FakeProvider::new("github").on_list_change_requests(move |input| {
                asked.lock().push(input.limit);
                Ok(page(Vec::new(), false))
            }),
        ],
    )
    .await;

    list_json(&h.service, json!({ "state": "open" })).await;
    list_json(&h.service, json!({ "state": "open", "limit": 10 })).await;

    // Providers probe with one row over this, so 99 asks a host for 100 — the most GitHub and
    // GitLab serve in one request. 100 here would cost a second round trip for a single row.
    assert_eq!(*limits.lock(), vec![99, 10]);
}

#[tokio::test(start_paused = true)]
async fn says_where_each_repository_carries_on_and_from_nothing_it_has_run_out_of() {
    let h = make_service(
        vec![
            project("p1", "t3code", "/a", "pingdotgg/t3code"),
            project("p2", "web", "/b", "acme/web"),
        ],
        vec![
            FakeProvider::new("github").on_list_change_requests(|input| {
                Ok(ProviderChangeRequestPage {
                    items: vec![change_request(1, "2026-07-02T00:00:00Z")],
                    truncated: input.repo.repository == "pingdotgg/t3code",
                    cursor_advance: None,
                    continues: true,
                })
            }),
        ],
    )
    .await;

    let result = list_json(&h.service, json!({ "state": "open" })).await;

    // The instant of the oldest row, how many rows have gone, and the row already sent at that
    // instant. The repository that had nothing more is simply not in it.
    assert_eq!(
        result["nextCursors"],
        json!({ "github.com pingdotgg/t3code": "2026-07-02T00:00:00Z|1|1" })
    );
}

#[tokio::test(start_paused = true)]
async fn offers_no_continuation_for_a_host_that_cannot_be_carried_on_from() {
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![FakeProvider::new("github").on_list_change_requests(|_| {
            Ok(ProviderChangeRequestPage {
                items: vec![change_request(1, "2026-07-02T00:00:00Z")],
                truncated: true,
                cursor_advance: None,
                continues: false,
            })
        })],
    )
    .await;

    let result = list_json(&h.service, json!({ "state": "open" })).await;

    // More rows exist and no cursor reaches them, which is what asking for a larger page is for.
    assert_eq!(result["truncated"], true);
    assert_eq!(result["nextCursors"], json!({}));
}

#[tokio::test(start_paused = true)]
async fn reads_only_the_repositories_it_was_asked_to_carry_on_with() {
    let listed = log();
    let asked = Arc::new(Mutex::new(Vec::<Option<ProviderListCursor>>::new()));
    let (pushed, cursors) = (listed.clone(), asked.clone());
    let h = make_service(
        vec![
            project("p1", "t3code", "/a", "pingdotgg/t3code"),
            project("p2", "web", "/b", "acme/web"),
        ],
        vec![
            FakeProvider::new("github").on_list_change_requests(move |input| {
                pushed.lock().push(input.repo.repository.clone());
                cursors.lock().push(input.cursor.clone());
                Ok(page(Vec::new(), false))
            }),
        ],
    )
    .await;

    let result = list_json(
        &h.service,
        json!({
            "state": "open",
            "cursors": { "github.com acme/web": "2026-07-02T00:00:00Z|99|7" },
        }),
    )
    .await;

    // The other repository is already on the page, and reading it again is the whole cost this
    // is here to avoid. The host summaries stay over the workspace, because the switcher they
    // fill is about the workspace rather than about this slice.
    assert_eq!(drain_sorted(&listed), vec!["acme/web"]);
    assert_eq!(
        *asked.lock(),
        vec![Some(ProviderListCursor {
            updated_before: "2026-07-02T00:00:00Z".to_owned(),
            delivered: 99,
        })]
    );
    assert_eq!(result["providers"].as_array().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn keeps_a_row_already_sent_at_the_boundary_instant_from_arriving_twice() {
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![
            // The boundary instant is asked for inclusively, so the host hands back the rows
            // already sent at it alongside the ones beside them, which a strictly-older read
            // would have lost instead.
            FakeProvider::new("github").on_list_change_requests(|_| {
                Ok(ProviderChangeRequestPage {
                    items: vec![
                        change_request(7, "2026-07-02T00:00:00Z"),
                        change_request(8, "2026-07-02T00:00:00Z"),
                        change_request(9, "2026-07-01T00:00:00Z"),
                    ],
                    truncated: true,
                    cursor_advance: None,
                    continues: true,
                })
            }),
        ],
    )
    .await;

    let result = list_json(
        &h.service,
        json!({
            "state": "open",
            "cursors": { "github.com pingdotgg/t3code": "2026-07-02T00:00:00Z|1|7" },
        }),
    )
    .await;

    assert_eq!(rows(&result, "entries", &["number"]), json!([[8], [9]]));
    assert_eq!(
        result["nextCursors"],
        json!({ "github.com pingdotgg/t3code": "2026-07-01T00:00:00Z|3|9" })
    );
}

/// `error.reason` of a `PullRequestUnavailableError`.
fn unavailable_reason_of(error: &PullRequestError) -> PullRequestUnavailableReason {
    match error {
        PullRequestError::Unavailable(PullRequestUnavailableError::Unavailable {
            reason, ..
        }) => *reason,
        other => panic!("expected an unavailable error, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn keeps_the_earlier_exclusions_when_a_slice_ends_on_the_instant_it_began_on() {
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![FakeProvider::new("github").on_list_change_requests(|_| {
            Ok(ProviderChangeRequestPage {
                items: vec![
                    change_request(7, "2026-07-02T00:00:00Z"),
                    change_request(8, "2026-07-02T00:00:00Z"),
                ],
                truncated: true,
                cursor_advance: None,
                continues: true,
            })
        })],
    )
    .await;

    let result = list_json(
        &h.service,
        json!({
            "state": "open",
            "cursors": { "github.com pingdotgg/t3code": "2026-07-02T00:00:00Z|1|6" },
        }),
    )
    .await;

    // Eight rows can share one second, so a whole slice inside one is ordinary. The next read
    // has to keep excluding 6 as well as the two just sent, or it hands 6 over again.
    assert_eq!(rows(&result, "entries", &["number"]), json!([[7], [8]]));
    assert_eq!(
        result["nextCursors"],
        json!({ "github.com pingdotgg/t3code": "2026-07-02T00:00:00Z|3|6,7,8" })
    );
}

#[tokio::test(start_paused = true)]
async fn refuses_a_continuation_it_did_not_issue_before_asking_any_host_anything() {
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![FakeProvider::new("github").on_list_change_requests(|_| panic!("should not be read"))],
    )
    .await;

    let error = h
        .service
        .list(&input(json!({
            "state": "open",
            "cursors": { "github.com pingdotgg/t3code": "yesterday" },
        })))
        .await
        .unwrap_err();

    assert_eq!(
        operation_of(&error),
        (
            "list".to_owned(),
            "The list could not be carried on from where it left off.".to_owned()
        )
    );
}

#[tokio::test(start_paused = true)]
async fn calls_a_transient_viewer_failure_a_failed_operation_not_a_signed_out_cli() {
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![FakeProvider::new("github").on_get_viewer(|_, _| {
            Err(provider_error(
                "github",
                "getViewer",
                PullRequestProviderErrorReason::Failed,
                "HTTP 500",
            ))
        })],
    )
    .await;

    let error = h
        .service
        .list(&input(json!({ "state": "open" })))
        .await
        .unwrap_err();

    // `cli-unauthenticated` would send the reader to `gh auth login` over a transient error.
    assert!(is_operation_error(&error));
}

#[tokio::test(start_paused = true)]
async fn tells_two_hosts_of_one_kind_apart_in_the_switcher_and_the_filter() {
    let h = make_service(
        vec![
            project("p1", "on github.com", "/a", "ping/one"),
            project("p2", "on the enterprise install", "/b", "ping/two").host("ghe.example.com"),
        ],
        vec![
            FakeProvider::new("github").on_list_change_requests(|input| {
                Ok(page(
                    if input.repo.host == "ghe.example.com" {
                        vec![change_request(2, "2026-07-05T00:00:00Z")]
                    } else {
                        Vec::new()
                    },
                    false,
                ))
            }),
        ],
    )
    .await;

    // Both hosts are GitHub, so a switcher keyed by provider kind would offer one pill for the
    // two of them and no way to ask for either.
    let all = list_json(&h.service, json!({ "state": "open" })).await;
    assert_eq!(
        rows(&all, "providers", &["host", "kind", "projectCount"]),
        json!([
            ["github.com", "github", 1],
            ["ghe.example.com", "github", 1],
        ])
    );

    let scoped = list_json(
        &h.service,
        json!({ "state": "open", "host": "ghe.example.com" }),
    )
    .await;
    assert_eq!(
        rows(&scoped, "entries", &["host", "number"]),
        json!([["ghe.example.com", 2]])
    );
}

#[tokio::test(start_paused = true)]
async fn fails_as_unavailable_only_when_no_host_can_be_read() {
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![FakeProvider::new("github").on_get_viewer(|_, _| {
            Err(unusable(
                "github",
                PullRequestProviderErrorReason::MissingTool,
            ))
        })],
    )
    .await;

    let error = h
        .service
        .list(&input(json!({ "state": "open" })))
        .await
        .unwrap_err();

    assert!(is_unavailable(&error));
    assert_eq!(
        unavailable_reason_of(&error),
        PullRequestUnavailableReason::CliMissing
    );
}

#[tokio::test(start_paused = true)]
async fn reads_a_repository_once_when_several_worktrees_share_it() {
    let calls = counter();
    let counted = calls.clone();
    let h = make_service(
        vec![
            project("p1", "t3code", "/a", "pingdotgg/t3code"),
            project("p2", "t3code worktree", "/b", "PingDotGG/T3Code"),
        ],
        vec![
            FakeProvider::new("github").on_list_change_requests(move |_| {
                counted.fetch_add(1, SeqCst);
                Ok(page(vec![change_request(1, "2026-07-02T00:00:00Z")], false))
            }),
        ],
    )
    .await;

    let result = list_json(&h.service, json!({ "state": "open" })).await;

    assert_eq!(calls.load(SeqCst), 1);
    assert_eq!(result["entries"].as_array().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn keeps_healthy_repositories_when_one_of_them_cannot_be_read() {
    let h = make_service(
        vec![
            project("p1", "t3code", "/a", "pingdotgg/t3code"),
            project("p2", "broken", "/b", "pingdotgg/broken"),
        ],
        vec![
            FakeProvider::new("github").on_list_change_requests(|input| {
                if input.repo.repository == "pingdotgg/broken" {
                    return Err(request_failed());
                }
                Ok(page(vec![change_request(1, "2026-07-02T00:00:00Z")], false))
            }),
        ],
    )
    .await;

    let result = list_json(&h.service, json!({ "state": "open" })).await;

    assert_eq!(result["entries"].as_array().unwrap().len(), 1);
    assert_eq!(
        rows(&result, "errors", &["projectTitle"]),
        json!([["broken"]])
    );
}

#[tokio::test(start_paused = true)]
async fn tries_another_workspace_on_the_same_host_for_the_viewer() {
    let h = make_service(
        vec![
            project("p1", "broken", "/broken", "acme/one"),
            project("p2", "healthy", "/healthy", "acme/two"),
        ],
        vec![
            FakeProvider::new("github")
                .on_get_viewer(|cwd, _| {
                    if cwd == "/healthy" {
                        Ok("bilal".to_owned())
                    } else {
                        Err(unusable(
                            "github",
                            PullRequestProviderErrorReason::MissingTool,
                        ))
                    }
                })
                .on_list_change_requests(|_| {
                    Ok(page(vec![change_request(1, "2026-07-02T00:00:00Z")], false))
                }),
        ],
    )
    .await;

    let result = list_json(&h.service, json!({ "state": "open" })).await;

    assert_eq!(result["entries"].as_array().unwrap().len(), 2);
    assert_eq!(result["viewers"]["github.com"], "bilal");
}

#[tokio::test(start_paused = true)]
async fn routing_verifies_the_current_account_on_the_requested_host_without_caching_it() {
    let viewer = Arc::new(Mutex::new("first-account".to_owned()));
    let current = viewer.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web").host("github.example.test")],
        vec![
            FakeProvider::new("github").on_get_routing_identity(move |input| {
                assert_eq!(input.cwd, "/a");
                assert_eq!(input.host, "github.example.test");
                let viewer = current.lock().clone();
                Ok(RoutingIdentity {
                    account_id: if viewer == "first-account" {
                        "123"
                    } else {
                        "456"
                    }
                    .to_owned(),
                    viewer,
                })
            }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    let first = h.service.routing(&reference).await.unwrap();
    assert_eq!(
        serde_json::to_value(first).unwrap(),
        json!({
            "accountId": "123",
            "host": "github.example.test",
            "provider": "github",
            "viewer": "first-account",
            "projectTitle": "web",
            "workspaceRoot": "/a",
        })
    );
    *viewer.lock() = "second-account".to_owned();
    assert_eq!(
        h.service.routing(&reference).await.unwrap().viewer.as_str(),
        "second-account"
    );
    *viewer.lock() = " ".to_owned();
    let failure = h.service.routing(&reference).await.unwrap_err();
    assert_eq!(operation_of(&failure).0, "routeIdentity");
}

#[tokio::test(start_paused = true)]
async fn refuses_an_action_the_host_never_claimed_it_could_run() {
    let ran = counter();
    let counted = ran.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                // Bitbucket's shape: it can merge and close, but cannot reopen.
                .capabilities_with(|capabilities| {
                    capabilities["actions"] = json!(["merge", "close"]);
                })
                .on_run_action(move |_| {
                    counted.fetch_add(1, SeqCst);
                    Ok(())
                }),
        ],
    )
    .await;

    let error = h
        .service
        .run_action(&input(with_ref(
            &pr_ref("p1", "acme/web", 1),
            json!({ "action": "reopen" }),
        )))
        .await
        .unwrap_err();

    assert!(is_operation_error(&error));
    assert_eq!(ran.load(SeqCst), 0);
}

/// `PullRequestViewerPermissions` from the object literal a test writes upstream.
fn permissions(value: Value) -> PullRequestViewerPermissions {
    serde_json::from_value(value).unwrap()
}

/// What a provider's `runAction` is handed, as `{ action, mergeMethod? }` in the shape upstream
/// tests compare against.
fn sent(input: &RunActionInput) -> Value {
    let mut value = json!({ "action": input.action });
    if let Some(method) = input.merge_method {
        value["mergeMethod"] = json!(method);
    }
    value
}

#[tokio::test(start_paused = true)]
async fn publishes_a_merge_for_immediate_settlement_only_after_host_confirmation() {
    let merged_at = "2026-09-03T02:00:00.000Z";
    let state = Arc::new(Mutex::new(PullRequestState::Open));
    let confirmation_fails = Arc::new(Mutex::new(false));
    let (current, failing) = (state.clone(), confirmation_fails.clone());
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_get_change_request_summary(move |_| {
                if *failing.lock() {
                    return Err(provider_error(
                        "github",
                        "getChangeRequestSummary",
                        PullRequestProviderErrorReason::Failed,
                        "HTTP 504",
                    ));
                }
                Ok(summary_of(&change_request(1, merged_at), *current.lock()))
            }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);
    let mut merges = h.service.subscribe_merges();
    let mut refreshes = h.service.subscribe_refreshes();

    // Queueing succeeds while the host still reports an open PR.
    h.service
        .run_action(&input(with_ref(&reference, json!({ "action": "merge" }))))
        .await
        .unwrap();
    let queued_refresh = refreshes.next().await.unwrap();
    *confirmation_fails.lock() = true;
    h.service
        .run_action(&input(with_ref(&reference, json!({ "action": "merge" }))))
        .await
        .unwrap();
    assert!(refreshes.next().await.unwrap() > queued_refresh);
    *confirmation_fails.lock() = false;
    *state.lock() = PullRequestState::Merged;
    h.clock
        .set_time(
            chrono::DateTime::parse_from_rfc3339(merged_at)
                .unwrap()
                .timestamp_millis(),
        )
        .await;
    h.service
        .run_action(&input(with_ref(
            &reference,
            json!({ "repository": " ACME/WEB ", "action": "merge", "mergeMethod": "merge" }),
        )))
        .await
        .unwrap();

    let event = merges.next().await.unwrap();
    assert_eq!(
        (
            event.project_id.as_str(),
            event.repository.as_str(),
            event.number.get(),
            event.merged_at.as_str(),
        ),
        ("p1", "acme/web", 1, merged_at)
    );
}

#[tokio::test(start_paused = true)]
async fn refreshes_every_reader_before_a_queued_merge_confirmation_finishes() {
    let confirmation_started = Arc::new(Notify::new());
    let confirm = Arc::new(Notify::new());
    let (started, release) = (confirmation_started.clone(), confirm.clone());
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_get_change_request_summary_async(move |_| {
                let (started, release) = (started.clone(), release.clone());
                async move {
                    started.notify_one();
                    release.notified().await;
                    Ok(summary_of(
                        &change_request(1, "2026-09-16T00:00:00.000Z"),
                        PullRequestState::Open,
                    ))
                }
            }),
        ],
    )
    .await;
    let mut merges = h.service.subscribe_merges();
    let mut first_reader = h.service.subscribe_refreshes();
    let mut second_reader = h.service.subscribe_refreshes();
    let service = h.service.clone();
    let action = tokio::spawn(async move {
        service
            .run_action(&input(with_ref(
                &pr_ref("p1", "acme/web", 1),
                json!({ "action": "merge" }),
            )))
            .await
    });
    confirmation_started.notified().await;

    let revision = first_reader.next().await.unwrap();
    assert!(revision > 0);
    assert_eq!(second_reader.next().await.unwrap(), revision);
    assert!(!action.is_finished());
    confirm.notify_one();
    action.await.unwrap().unwrap();
    // The confirmed row is still open, so nothing merged.
    assert!(merges.next().now_or_never().is_none());
}

#[tokio::test(start_paused = true)]
async fn refuses_an_action_this_viewer_may_not_take_and_says_what_access_it_takes() {
    let ran = Arc::new(Mutex::new(None::<PullRequestAction>));
    let recorded = ran.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                // The host merges; this account only reads it, and opened the change request,
                // which is every contributor to a repository they do not own.
                .on_get_viewer_permissions(|_| {
                    Ok(permissions(json!({
                        "actions": ["ready", "draft", "close", "reopen"],
                        "comment": true,
                        "resolve": true,
                        "verdicts": ["comment", "approve", "request-changes"],
                        "requestReviewers": false,
                    })))
                })
                .on_run_action(move |input| {
                    *recorded.lock() = Some(input.action);
                    Ok(())
                }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    let error = h
        .service
        .run_action(&input(with_ref(&reference, json!({ "action": "merge" }))))
        .await
        .unwrap_err();
    assert!(is_operation_error(&error));
    assert!(
        operation_of(&error)
            .1
            .contains("You need write access on this repository to merge.")
    );
    assert_eq!(*ran.lock(), None);

    // What the author keeps whatever their access is still theirs to take.
    h.service
        .run_action(&input(with_ref(&reference, json!({ "action": "close" }))))
        .await
        .unwrap();
    assert_eq!(*ran.lock(), Some(PullRequestAction::Close));
}

#[tokio::test(start_paused = true)]
async fn gates_arming_a_merge_for_later_exactly_as_it_gates_merging_now() {
    let ran = Arc::new(Mutex::new(None::<Value>));
    let recorded = ran.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .capabilities_with(|capabilities| {
                    capabilities["actions"] =
                        json!(["merge", "close", "enable-auto-merge", "disable-auto-merge"]);
                    capabilities["mergeMethods"] = json!(["merge", "squash"]);
                })
                // This account may close the change request it opened, and nothing else here.
                .on_get_viewer_permissions(|_| {
                    Ok(permissions(json!({
                        "actions": ["close"],
                        "comment": true,
                        "resolve": true,
                        "verdicts": ["comment", "approve", "request-changes"],
                        "requestReviewers": false,
                    })))
                })
                .on_run_action(move |input| {
                    *recorded.lock() = Some(sent(&input));
                    Ok(())
                }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    let refused = h
        .service
        .run_action(&input(with_ref(
            &reference,
            json!({ "action": "enable-auto-merge", "mergeMethod": "squash" }),
        )))
        .await
        .unwrap_err();
    assert!(is_operation_error(&refused));
    assert!(
        operation_of(&refused)
            .1
            .contains("merged for you once it is ready")
    );
    assert_eq!(*ran.lock(), None);

    // The strategy is checked against the host for an armed merge too: a merge it performs
    // later is still a merge, and one it cannot spell must not be passed on.
    let wrong_strategy = h
        .service
        .run_action(&input(with_ref(
            &reference,
            json!({ "action": "enable-auto-merge", "mergeMethod": "rebase" }),
        )))
        .await
        .unwrap_err();
    assert!(is_operation_error(&wrong_strategy));
    assert_eq!(*ran.lock(), None);
}

#[tokio::test(start_paused = true)]
async fn hands_the_host_the_strategy_an_armed_merge_was_asked_for() {
    let ran = Arc::new(Mutex::new(None::<Value>));
    let recorded = ran.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .capabilities_with(|capabilities| {
                    capabilities["actions"] =
                        json!(["merge", "enable-auto-merge", "disable-auto-merge"]);
                    capabilities["mergeMethods"] = json!(["merge", "squash"]);
                })
                .on_get_viewer_permissions(|_| {
                    Ok(permissions(json!({
                        "actions": ["merge", "enable-auto-merge", "disable-auto-merge"],
                        "comment": true,
                        "resolve": true,
                        "verdicts": ["comment", "approve", "request-changes"],
                        "requestReviewers": true,
                    })))
                })
                .on_run_action(move |input| {
                    *recorded.lock() = Some(sent(&input));
                    Ok(())
                }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 1);

    h.service
        .run_action(&input(with_ref(
            &reference,
            json!({ "action": "enable-auto-merge", "mergeMethod": "squash" }),
        )))
        .await
        .unwrap();
    assert_eq!(
        *ran.lock(),
        Some(json!({ "action": "enable-auto-merge", "mergeMethod": "squash" }))
    );

    h.service
        .run_action(&input(with_ref(
            &reference,
            json!({ "action": "disable-auto-merge" }),
        )))
        .await
        .unwrap();
    assert_eq!(*ran.lock(), Some(json!({ "action": "disable-auto-merge" })));
}

#[tokio::test(start_paused = true)]
async fn refuses_an_auto_merge_the_host_never_claimed_without_asking_it() {
    let ran = counter();
    let counted = ran.clone();
    // A host that merges, and has nothing that merges later on its own.
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![FakeProvider::new("github").on_run_action(move |_| {
            counted.fetch_add(1, SeqCst);
            Ok(())
        })],
    )
    .await;

    let error = h
        .service
        .run_action(&input(json!({
            "projectId": "p1",
            "repository": "acme/web",
            "number": 1,
            "action": "enable-auto-merge",
        })))
        .await
        .unwrap_err();

    assert!(is_operation_error(&error));
    assert_eq!(ran.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn refuses_to_resolve_a_conversation_this_viewer_may_not_without_asking_the_host() {
    let resolved = counter();
    let counted = resolved.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_get_viewer_permissions(|_| {
                    Ok(permissions(json!({
                        "actions": ["merge", "ready", "draft", "close", "reopen"],
                        "comment": true,
                        "resolve": false,
                        "verdicts": ["comment", "approve", "request-changes"],
                        "requestReviewers": true,
                    })))
                })
                .on_set_thread_resolution(move |_| {
                    counted.fetch_add(1, SeqCst);
                    Ok(())
                }),
        ],
    )
    .await;

    let error = h
        .service
        .set_thread_resolution(&input(json!({
            "projectId": "p1",
            "repository": "acme/web",
            "number": 1,
            "threadId": "t1",
            "resolved": true,
        })))
        .await
        .unwrap_err();

    assert!(is_operation_error(&error));
    assert!(
        operation_of(&error)
            .1
            .contains("to resolve a review conversation.")
    );
    assert_eq!(resolved.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn asks_nobody_what_the_viewer_may_do_when_the_host_cannot_do_it_at_all() {
    let asked = counter();
    let counted = asked.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .capabilities_with(|capabilities| {
                    capabilities["actions"] = json!(["merge", "close"]);
                    capabilities["mergeMethods"] = json!(["merge"]);
                })
                .on_get_viewer_permissions(move |_| {
                    counted.fetch_add(1, SeqCst);
                    Err(provider_error(
                        "github",
                        "getViewerPermissions",
                        PullRequestProviderErrorReason::Failed,
                        "must not be called",
                    ))
                }),
        ],
    )
    .await;

    h.service
        .run_action(&input(with_ref(
            &pr_ref("p1", "acme/web", 1),
            json!({ "action": "reopen" }),
        )))
        .await
        .unwrap_err();

    // The capability check costs nothing; the permission read is a request, so it comes second.
    assert_eq!(asked.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn refuses_a_comment_on_a_host_that_cannot_post_one() {
    let posted = counter();
    let counted = posted.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github")
                .capabilities_with(|capabilities| {
                    capabilities["diff"] = json!(false);
                    capabilities["comment"] = json!(false);
                    capabilities["actions"] = json!(["merge"]);
                    capabilities["mergeMethods"] = json!(["merge"]);
                })
                .on_comment(move |_| {
                    counted.fetch_add(1, SeqCst);
                    Ok(())
                }),
        ],
    )
    .await;

    let error = h
        .service
        .comment(&input(json!({
            "projectId": "p1",
            "repository": "acme/web",
            "number": 1,
            "body": "Looks good.",
        })))
        .await
        .unwrap_err();

    assert!(is_operation_error(&error));
    assert_eq!(posted.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn keeps_two_hosts_of_one_provider_kind_as_two_accounts() {
    let h = make_service(
        vec![
            project("p1", "cloud", "/cloud", "acme/web"),
            // The same path on a different host: neither the viewer nor the row may be shared.
            project("p2", "enterprise", "/enterprise", "acme/web").host("github.acme.dev"),
        ],
        vec![
            FakeProvider::new("github")
                .on_get_viewer(|cwd, _| {
                    Ok(match cwd.as_str() {
                        "/cloud" => "bilal",
                        "/enterprise" => "b.hassan",
                        _ => "unknown",
                    }
                    .to_owned())
                })
                .on_list_change_requests(|_| {
                    Ok(page(vec![change_request(1, "2026-07-02T00:00:00Z")], false))
                }),
        ],
    )
    .await;

    let result = list_json(&h.service, json!({ "state": "open" })).await;

    // Both repositories survive de-duplication, each with its own account.
    assert_eq!(result["entries"].as_array().unwrap().len(), 2);
    assert_eq!(
        result["viewers"],
        json!({ "github.com": "bilal", "github.acme.dev": "b.hassan" })
    );
    let mut hosts: Vec<&str> = result["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["host"].as_str().unwrap())
        .collect();
    hosts.sort();
    assert_eq!(hosts, vec!["github.acme.dev", "github.com"]);
}

#[tokio::test(start_paused = true)]
async fn reports_repositories_on_a_host_that_could_not_be_read() {
    let h = make_service(
        vec![
            project("p1", "cloud", "/cloud", "acme/web"),
            project("p2", "enterprise", "/enterprise", "acme/api").host("github.acme.dev"),
        ],
        vec![
            FakeProvider::new("github")
                .on_get_viewer(|cwd, _| {
                    if cwd == "/cloud" {
                        Ok("bilal".to_owned())
                    } else {
                        Err(unusable(
                            "github",
                            PullRequestProviderErrorReason::Unauthenticated,
                        ))
                    }
                })
                .on_list_change_requests(|_| {
                    Ok(page(vec![change_request(1, "2026-07-02T00:00:00Z")], false))
                }),
        ],
    )
    .await;

    let result = list_json(&h.service, json!({ "state": "open" })).await;

    // The healthy host still lists, and the unreadable one is named rather than dropped.
    assert_eq!(result["entries"].as_array().unwrap().len(), 1);
    assert_eq!(rows(&result, "errors", &["projectId"]), json!([["p2"]]));
}
