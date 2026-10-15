//! `PullRequestService.test.ts` lines 1989-2944: rate limits on reads and actions, review
//! requests, repository ownership and routing to a checkout, host-native stacks, and the input
//! refusals. Cases whose subject is a cut host (GitLab, Azure DevOps, Bitbucket, Forgejo) are
//! left out; see the Deviations table in PORTING.md.

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use serde_json::json;
use turin_contracts::orchestration_project::OrchestrationProjectShell;
use turin_contracts::pull_request::{
    PullRequestActor, PullRequestChecks, PullRequestRef, PullRequestState,
};

use super::{counter, log, rows, summary_of};
use crate::pull_request::service::PullRequestService;
use crate::pull_request::service::test_support::*;
use crate::source_control_core::clock::Clock;
use crate::source_control_core::pull_request_provider::*;

/// `service.list(input)` as JSON, the shape every assertion below reads.
async fn list_json(service: &PullRequestService, value: serde_json::Value) -> serde_json::Value {
    serde_json::to_value(service.list(&input(value)).await.unwrap()).unwrap()
}

#[tokio::test(start_paused = true)]
async fn stops_new_reads_after_a_rate_limit_while_leaving_manual_actions_available() {
    let list_calls = counter();
    let action_calls = counter();
    let (listed, acted) = (list_calls.clone(), action_calls.clone());
    let h = make_service(
        vec![project("p1", "cloud", "/cloud", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_list_change_requests(move |_| {
                    listed.fetch_add(1, SeqCst);
                    Err(provider_error(
                        "github",
                        "listChangeRequests",
                        PullRequestProviderErrorReason::RateLimited,
                        "GitHub API rate limit exceeded.",
                    ))
                })
                .on_run_action(move |_| {
                    acted.fetch_add(1, SeqCst);
                    Ok(())
                }),
        ],
    )
    .await;

    let first = list_json(&h.service, json!({ "state": "open", "involvement": "all" })).await;
    let paused = list_json(
        &h.service,
        json!({ "state": "open", "involvement": "authored" }),
    )
    .await;
    h.service
        .run_action(&input(with_ref(
            &pr_ref("p1", "acme/web", 1),
            json!({ "action": "close" }),
        )))
        .await
        .unwrap();

    assert_eq!(list_calls.load(SeqCst), 1);
    assert_eq!(action_calls.load(SeqCst), 1);
    assert_eq!(first["errors"].as_array().unwrap().len(), 1);
    assert_eq!(paused["errors"].as_array().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn shares_fresh_github_detail_reads_and_respects_rate_limit_resets() {
    let calls = counter();
    let limited = Arc::new(Mutex::new(false));
    let clock = TestClock::new();
    let (counted, is_limited, now) = (calls.clone(), limited.clone(), clock.clone());
    let h = Harness::new(ServiceOptions {
        clock: Some(clock.clone()),
        projects: vec![project("p1", "web", "/repo", "acme/web").build()],
        providers: vec![FakeProvider::new("github").on_get_change_request(move |_| {
            counted.fetch_add(1, SeqCst);
            if *is_limited.lock() {
                return Err(PullRequestProviderError {
                    retry_at: Some(now.now_millis() + 120_000),
                    ..provider_error(
                        "github",
                        "getChangeRequest",
                        PullRequestProviderErrorReason::RateLimited,
                        "Retry after the reset.",
                    )
                });
            }
            Ok(hosted_change_request("current details", 1))
        })],
        ..ServiceOptions::default()
    })
    .await;
    let reference = PullRequestRef {
        allow_stale: Some(false),
        ..pr_ref("p1", "acme/web", 1)
    };

    let (first, second) = tokio::join!(h.service.detail(&reference), h.service.detail(&reference));
    first.unwrap();
    second.unwrap();
    assert_eq!(calls.load(SeqCst), 1);
    h.clock.adjust(61_000).await;
    *limited.lock() = true;
    assert!(h.service.detail(&reference).await.is_err());
    assert_eq!(calls.load(SeqCst), 2);
    h.clock.adjust(45_000).await;
    assert!(h.service.detail(&reference).await.is_err());
    assert_eq!(calls.load(SeqCst), 2);
    h.clock.adjust(75_000).await;
    *limited.lock() = false;
    let recovered = h.service.detail(&reference).await.unwrap();
    assert_eq!(
        serde_json::to_value(&recovered).unwrap()["state"],
        json!("open")
    );
    assert_eq!(calls.load(SeqCst), 3);
    h.service
        .invalidate(&invalidate_ref(&reference), false)
        .await;
    h.service.detail(&reference).await.unwrap();
    assert_eq!(calls.load(SeqCst), 4);
}

#[tokio::test(start_paused = true)]
async fn shares_fresh_github_checks_reads_and_respects_rate_limit_resets() {
    let calls = counter();
    let limited = Arc::new(Mutex::new(false));
    let clock = TestClock::new();
    let (counted, is_limited, now) = (calls.clone(), limited.clone(), clock.clone());
    let h = Harness::new(ServiceOptions {
        clock: Some(clock.clone()),
        projects: vec![project("p1", "web", "/repo", "acme/web").build()],
        providers: vec![
            FakeProvider::new("github").on_get_change_request_checks(move |_| {
                counted.fetch_add(1, SeqCst);
                if *is_limited.lock() {
                    return Err(PullRequestProviderError {
                        retry_at: Some(now.now_millis() + 120_000),
                        ..provider_error(
                            "github",
                            "getChangeRequest",
                            PullRequestProviderErrorReason::RateLimited,
                            "Retry after the reset.",
                        )
                    });
                }
                Ok(PullRequestChecks {
                    state: PullRequestState::Open,
                    checks: Vec::new(),
                })
            }),
        ],
        ..ServiceOptions::default()
    })
    .await;
    let reference = PullRequestRef {
        allow_stale: Some(false),
        ..pr_ref("p1", "acme/web", 1)
    };

    let (first, second) = tokio::join!(h.service.checks(&reference), h.service.checks(&reference));
    first.unwrap();
    second.unwrap();
    assert_eq!(calls.load(SeqCst), 1);
    h.clock.adjust(61_000).await;
    *limited.lock() = true;
    assert!(h.service.checks(&reference).await.is_err());
    assert_eq!(calls.load(SeqCst), 2);
    h.clock.adjust(45_000).await;
    assert!(h.service.checks(&reference).await.is_err());
    assert_eq!(calls.load(SeqCst), 2);
    h.clock.adjust(75_000).await;
    *limited.lock() = false;
    let recovered = h.service.checks(&reference).await.unwrap().unwrap();
    assert_eq!(recovered.state, PullRequestState::Open);
    assert_eq!(calls.load(SeqCst), 3);
    h.service
        .invalidate(&invalidate_ref(&reference), false)
        .await;
    h.service.checks(&reference).await.unwrap();
    assert_eq!(calls.load(SeqCst), 4);
}

#[tokio::test(start_paused = true)]
async fn does_not_fall_back_to_full_detail_when_checks_are_unsupported() {
    let full_reads = counter();
    let counted = full_reads.clone();
    let h = make_service(
        vec![project("p1", "web", "/repo", "acme/web")],
        vec![FakeProvider::new("github").on_get_change_request(move |_| {
            counted.fetch_add(1, SeqCst);
            Err(request_failed())
        })],
    )
    .await;

    assert!(
        h.service
            .checks(&pr_ref("p1", "acme/web", 1))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(full_reads.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn uses_a_manual_rate_limit_to_pause_later_reads() {
    let list_calls = counter();
    let counted = list_calls.clone();
    let h = make_service(
        vec![project("p1", "cloud", "/cloud", "acme/web")],
        vec![
            FakeProvider::new("github")
                .on_list_change_requests(move |_| {
                    counted.fetch_add(1, SeqCst);
                    Ok(page(Vec::new(), false))
                })
                .on_run_action(|_| {
                    Err(provider_error(
                        "github",
                        "runAction",
                        PullRequestProviderErrorReason::RateLimited,
                        "GitHub API rate limit exceeded.",
                    ))
                }),
        ],
    )
    .await;

    h.service
        .run_action(&input(with_ref(
            &pr_ref("p1", "acme/web", 1),
            json!({ "action": "close" }),
        )))
        .await
        .unwrap_err();
    let error = h
        .service
        .list(&input(json!({ "state": "open", "involvement": "all" })))
        .await
        .unwrap_err();

    assert_eq!(list_calls.load(SeqCst), 0);
    assert!(is_operation_error(&error));
}

#[tokio::test(start_paused = true)]
async fn flags_a_review_request_for_the_viewer_but_not_on_their_own_change_request() {
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![FakeProvider::new("github").on_list_change_requests(|_| {
            Ok(page(
                vec![
                    ProviderChangeRequest {
                        review_request_logins: vec!["Bilal".to_owned()],
                        ..change_request(1, "2026-07-02T00:00:00Z")
                    },
                    ProviderChangeRequest {
                        author: Some(PullRequestActor {
                            is_bot: None,
                            login: tne("bilal"),
                            name: None,
                            avatar_url: None,
                        }),
                        review_request_logins: vec!["bilal".to_owned()],
                        ..change_request(2, "2026-07-02T00:00:00Z")
                    },
                ],
                false,
            ))
        })],
    )
    .await;

    let result = list_json(&h.service, json!({ "state": "open" })).await;

    assert_eq!(
        rows(&result, "entries", &["viewerReviewRequested"]),
        json!([[true], [false]])
    );
}

#[tokio::test(start_paused = true)]
async fn refuses_a_repository_that_does_not_belong_to_the_requested_project() {
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![FakeProvider::new("github")],
    )
    .await;

    let error = h
        .service
        .diff(&input(json!({
            "projectId": "p1",
            "repository": "attacker/repo",
            "number": 1,
        })))
        .await
        .unwrap_err();

    assert!(is_operation_error(&error));
}

#[tokio::test(start_paused = true)]
async fn caches_stack_membership_separately_from_action_details() {
    let reads = Arc::new(Mutex::new(Vec::<bool>::new()));
    let recorded = reads.clone();
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_get_change_request_stack(move |input| {
                let details = input.include_details;
                recorded.lock().push(details);
                Ok(Some(ProviderChangeRequestStack {
                    id: "9".to_owned(),
                    number: 3,
                    url: "https://github.com/acme/web/stacks/3".to_owned(),
                    base: "main".to_owned(),
                    layers: vec![ProviderChangeRequestStackLayer {
                        number: 7,
                        head_branch: "a".to_owned(),
                        state: PullRequestState::Open,
                        title: details.then(|| "First layer".to_owned()),
                        head_sha: details.then(|| "abc".to_owned()),
                        is_draft: None,
                    }],
                }))
            }),
        ],
    )
    .await;
    let reference = pr_ref("p1", "acme/web", 7);
    h.service.stack(&reference, Some(false)).await.unwrap();
    h.service.stack(&reference, Some(false)).await.unwrap();
    let detail = h.service.stack(&reference, None).await.unwrap();
    h.service.stack(&reference, None).await.unwrap();
    assert_eq!(*reads.lock(), vec![false, true]);
    assert_eq!(
        serde_json::to_value(&detail).unwrap()["layers"][0]["headSha"],
        json!("abc")
    );
    h.service
        .invalidate(&invalidate_ref(&reference), false)
        .await;
    h.service.stack(&reference, Some(false)).await.unwrap();
    h.service.stack(&reference, None).await.unwrap();
    assert_eq!(*reads.lock(), vec![false, true, false, true]);
}

#[tokio::test(start_paused = true)]
async fn reads_a_host_native_stack_through_the_provider_and_null_where_it_has_none() {
    let h = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![
            FakeProvider::new("github").on_get_change_request_stack(|_| {
                Ok(Some(ProviderChangeRequestStack {
                    id: "9".to_owned(),
                    number: 3,
                    url: "https://github.com/acme/web/stacks/3".to_owned(),
                    base: "main".to_owned(),
                    layers: vec![
                        ProviderChangeRequestStackLayer {
                            number: 7,
                            head_branch: "a".to_owned(),
                            state: PullRequestState::Open,
                            title: None,
                            head_sha: None,
                            is_draft: None,
                        },
                        ProviderChangeRequestStackLayer {
                            number: 8,
                            head_branch: "b".to_owned(),
                            state: PullRequestState::Open,
                            title: None,
                            head_sha: None,
                            is_draft: None,
                        },
                    ],
                }))
            }),
        ],
    )
    .await;

    let stack = h
        .service
        .stack(&pr_ref("p1", "acme/web", 7), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        rows(
            &serde_json::to_value(&stack).unwrap(),
            "layers",
            &["number"]
        ),
        json!([[7], [8]])
    );

    let without_stacks = make_service(
        vec![project("p1", "web", "/a", "acme/web")],
        vec![FakeProvider::new("github")],
    )
    .await;
    assert!(
        without_stacks
            .service
            .stack(&pr_ref("p1", "acme/web", 7), None)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(start_paused = true)]
async fn resolves_a_projects_repository_identity_when_its_shell_has_none_cached() {
    let resolved = project("web", "web", "/web", "acme/web").build();
    let identity = resolved.repository_identity.clone();
    let h = Harness::new(ServiceOptions {
        projects: vec![OrchestrationProjectShell {
            repository_identity: None,
            ..resolved
        }],
        providers: vec![
            FakeProvider::new("github").on_get_change_request_summary(|_| {
                Ok(summary_of(
                    &change_request(7, "2026-07-02T00:00:00Z"),
                    PullRequestState::Open,
                ))
            }),
        ],
        resolve_repository_identity: Some(Arc::new(move |_| identity.clone().flatten())),
        ..ServiceOptions::default()
    })
    .await;

    let summary = h
        .service
        .summary(
            &input(json!({
                "projectId": "web",
                "host": "github.com",
                "repository": "acme/web",
                "number": 7,
            })),
            Some(false),
        )
        .await
        .unwrap();

    assert_eq!(summary.number.get(), 7);
}

#[tokio::test(start_paused = true)]
async fn routes_a_hosted_reference_to_another_repository_through_a_project_on_that_host() {
    let seen = log();
    let pushed = seen.clone();
    let h = make_service(
        vec![project("frontend", "web", "/web", "acme/web")],
        vec![
            FakeProvider::new("github").on_get_change_request_summary(move |input| {
                pushed.lock().push(format!(
                    "{} {} {}",
                    input.repo.cwd, input.repo.repository, input.repo.host
                ));
                Ok(summary_of(
                    &change_request(7, "2026-07-02T00:00:00Z"),
                    PullRequestState::Open,
                ))
            }),
        ],
    )
    .await;

    let summary = h
        .service
        .summary(
            &input(json!({
                "projectId": "frontend",
                "host": "github.com",
                "repository": "acme/api",
                "number": 7,
            })),
            Some(false),
        )
        .await
        .unwrap();

    assert_eq!(summary.number.get(), 7);
    assert_eq!(*seen.lock(), vec!["/web acme/api github.com"]);
}

#[tokio::test(start_paused = true)]
async fn reads_linked_pr_summaries_and_stacks_on_github_without_a_checkout() {
    let seen = log();
    let pushed = seen.clone();
    let stacked = seen.clone();
    let h = make_service(
        vec![project("inbox", "Inbox", "/home/alex", "acme/api").without_repository()],
        vec![
            FakeProvider::new("github")
                .on_get_change_request_summary(move |input| {
                    pushed.lock().push(format!(
                        "{} {} {}",
                        input.repo.cwd, input.repo.repository, input.repo.host
                    ));
                    Ok(summary_of(
                        &change_request(7, "2026-07-02T00:00:00Z"),
                        PullRequestState::Open,
                    ))
                })
                .on_get_change_request_stack(move |input| {
                    stacked.lock().push(format!(
                        "{} {} {}",
                        input.request.repo.cwd,
                        input.request.repo.repository,
                        input.request.repo.host
                    ));
                    Ok(None)
                }),
        ],
    )
    .await;
    let reference = input::<PullRequestRef>(json!({
        "projectId": "inbox",
        "host": "github.com",
        "repository": "acme/api",
        "number": 7,
    }));

    let summary = h.service.summary(&reference, Some(false)).await.unwrap();
    let stack = h.service.stack(&reference, Some(false)).await.unwrap();
    assert_eq!(summary.title.as_str(), "Change request 7");
    assert_eq!(
        serde_json::to_value(&summary).unwrap()["state"],
        json!("open")
    );
    assert_eq!(stack, None);
    assert_eq!(
        *seen.lock(),
        vec![
            "/home/alex acme/api github.com",
            "/home/alex acme/api github.com",
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn prefers_a_matching_checkout_over_a_non_repository_projects_directory() {
    let seen = log();
    let pushed = seen.clone();
    let h = make_service(
        vec![
            project("inbox", "Inbox", "/home/alex", "unused").without_repository(),
            project("repo", "API", "/api", "acme/api"),
        ],
        vec![
            FakeProvider::new("github").on_get_change_request_summary(move |input| {
                pushed.lock().push(input.repo.cwd.clone());
                Ok(summary_of(
                    &change_request(7, "2026-07-02T00:00:00Z"),
                    PullRequestState::Open,
                ))
            }),
        ],
    )
    .await;

    h.service
        .summary(
            &input(json!({
                "projectId": "inbox",
                "host": "github.com",
                "repository": "acme/api",
                "number": 7,
            })),
            Some(false),
        )
        .await
        .unwrap();

    assert_eq!(*seen.lock(), vec!["/api"]);
}

#[tokio::test(start_paused = true)]
async fn requires_an_explicit_host_for_a_non_repository_projects_linked_pr() {
    let h = make_service(
        vec![project("inbox", "Inbox", "/home/alex", "unused").without_repository()],
        vec![FakeProvider::new("github")],
    )
    .await;

    let error = h
        .service
        .summary(
            &input(json!({ "projectId": "inbox", "repository": "acme/api", "number": 7 })),
            Some(false),
        )
        .await
        .unwrap_err();

    assert!(is_unavailable(&error));
}

#[tokio::test(start_paused = true)]
async fn refuses_a_public_host_reference_without_a_selected_project_directory() {
    let h = make_service(Vec::new(), vec![FakeProvider::new("github")]).await;

    let error = h
        .service
        .summary(
            &input(json!({
                "projectId": "missing",
                "host": "github.com",
                "repository": "acme/api",
                "number": 7,
            })),
            Some(false),
        )
        .await
        .unwrap_err();

    assert!(is_unavailable(&error));
}

#[tokio::test(start_paused = true)]
async fn refuses_a_hosted_reference_when_its_provider_is_unavailable() {
    let h = make_service(
        vec![project("frontend", "web", "/web", "acme/web")],
        vec![FakeProvider::new("github")],
    )
    .await;

    let error = h
        .service
        .summary(
            &input(json!({
                "projectId": "frontend",
                "host": "gitlab.com",
                "repository": "acme/api",
                "number": 7,
            })),
            Some(false),
        )
        .await
        .unwrap_err();

    assert!(is_unavailable(&error));
}

#[tokio::test(start_paused = true)]
async fn rejects_an_empty_comment_before_reaching_the_host() {
    let posted = counter();
    let counted = posted.clone();
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![FakeProvider::new("github").on_comment(move |_| {
            counted.fetch_add(1, SeqCst);
            Ok(())
        })],
    )
    .await;

    let error = h
        .service
        .comment(&input(json!({
            "projectId": "p1",
            "repository": "pingdotgg/t3code",
            "number": 1,
            "body": "   ",
        })))
        .await
        .unwrap_err();

    assert!(is_operation_error(&error));
    assert_eq!(posted.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn refuses_line_comments_on_a_host_that_takes_only_a_summary() {
    let submitted = counter();
    let counted = submitted.clone();
    let h = make_service(
        vec![project("p1", "t3code", "/a", "pingdotgg/t3code")],
        vec![
            FakeProvider::new("github")
                .capabilities_with(|capabilities| {
                    capabilities["review"] = json!({
                        "inlineComment": false,
                        "reply": false,
                        "resolve": false,
                        "verdicts": ["comment"],
                    });
                })
                .on_submit_review(move |_| {
                    counted.fetch_add(1, SeqCst);
                    Ok(())
                }),
        ],
    )
    .await;

    let error = h
        .service
        .submit_review(&input(json!({
            "projectId": "p1",
            "repository": "pingdotgg/t3code",
            "number": 1,
            "verdict": "comment",
            "body": "",
            "comments": [{
                "path": "src/a.ts",
                "position": { "kind": "added", "newLine": 1 },
                "body": "nit",
            }],
        })))
        .await
        .unwrap_err();

    assert!(is_operation_error(&error));
    assert_eq!(submitted.load(SeqCst), 0);
}
