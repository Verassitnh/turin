//! Port of upstream/packages/source-control-github/src/server/GitHubPullRequestProvider.test.ts
//!
//! Upstream scripts the service with `Layer.mock(GitHubPullRequestApi)`: the methods a test names
//! answer, every other one dies. [`Mock`] is the same thing over [`GitHubPullRequestService`].

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::future::BoxFuture;
use indexmap::{IndexMap, IndexSet};
use parking_lot::Mutex;
use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestActor, PullRequestBaseComparison, PullRequestCheck,
    PullRequestCheckStatus, PullRequestChecks, PullRequestChecksState, PullRequestComment,
    PullRequestCommentKind, PullRequestCommit, PullRequestMergeCapabilities,
    PullRequestMergeMethod, PullRequestMergeability, PullRequestReviewVerdict, PullRequestState,
    PullRequestUpdateMethod, PullRequestViewerPermissions,
};

use super::*;
use crate::github::api::GitHubApiError;
use crate::github::api::{
    GitHubApiAuthenticationError, GitHubApiNotFoundError, GitHubApiRateLimitError,
};
use crate::github::credentials::{
    GitHubCliFailedError, GitHubCliMissingError, GitHubCredentialUnavailableError,
    GitHubHostDisabledError, GitHubNotSignedInError,
};
use crate::github::pull_request_api::{
    GitHubPullRequestReadError, GitHubViewerLoginUnavailableError,
    GitHubWorkflowApprovalRefusedError, GitHubWorkflowApprovalRefusedReason,
};
use crate::github::pull_request_json::{
    GitHubBaseComparison, GitHubPullRequestDetail, decode_pull_request_detail_json,
};
use crate::source_control_core::cause_from_message;
use crate::source_control_core::pull_request_provider::{ProviderRepositoryRef, RoutingIdentity};
use crate::source_control_core::rate_limit::SourceControlRateLimitPausedError;
use crate::source_control_core::redacted::Redacted;

type Scripted<A, R> = Option<Box<dyn Fn(&A) -> Result<R, GitHubPullRequestApiError> + Send + Sync>>;

macro_rules! mock_service {
    ($($name:ident($arg:ty) -> $ret:ty;)*) => {
        /// The scripted service: unset methods panic, like a `Layer.mock` method left out.
        #[derive(Default)]
        struct Mock {
            $($name: Scripted<$arg, $ret>,)*
        }

        #[async_trait]
        impl GitHubPullRequestService for Mock {
            /// Every upstream test mocks `revalidateChecks: (_input, read) => read`.
            async fn revalidate<'a>(
                &'a self,
                _input: &'a ProviderChangeRequestRef,
                read: BoxFuture<'a, Result<ChecksRead, GitHubPullRequestApiError>>,
            ) -> Result<PullRequestChecks, GitHubPullRequestApiError> {
                read.await
                    .map(|read| PullRequestChecks { state: read.state, checks: read.checks })
            }
            $(
                async fn $name(&self, input: &$arg) -> Result<$ret, GitHubPullRequestApiError> {
                    let script = self
                        .$name
                        .as_ref()
                        .unwrap_or_else(|| panic!(concat!("unmocked ", stringify!($name))));
                    script(input)
                }
            )*
        }
    };
}

mock_service! {
    verify_credential(VerifiedCredentialInput) -> (VerifiedIdentity, PinnedCredential);
    get_routing_identity(VerifiedCredentialInput) -> RoutingIdentity;
    get_viewer_login(VerifiedCredentialInput) -> String;
    list_pull_requests(ListPullRequestsInput) -> GitHubPullRequestListBatch;
    search_pull_requests(SearchPullRequestsInput) -> GitHubPullRequestSearchBatch;
    list_pull_request_stats(ListPullRequestStatsInput) -> Vec<GitHubPullRequestStat>;
    list_actor_avatars(ListActorAvatarsInput) -> IndexMap<String, String>;
    get_pull_request_summary(GitHubPullRequestTarget) -> ProviderChangeRequestSummary;
    get_pull_request_watch_fingerprint(GitHubPullRequestTarget) -> Option<GitHubPullRequestWatchFingerprint>;
    get_pull_request_detail(GitHubPullRequestTarget) -> GitHubPullRequestCore;
    get_pull_request_preview(GitHubPullRequestTarget) -> ProviderChangeRequestPreview;
    list_workflow_runs_requiring_approval(ListWorkflowRunsRequiringApprovalInput) -> Vec<GitHubWorkflowRunApproval>;
    get_pull_request_stack(GetPullRequestStackInput) -> Option<GitHubPullRequestStack>;
    get_pull_request_activity(GitHubPullRequestTarget) -> GitHubPullRequestActivity;
    list_review_thread_comments(GitHubPullRequestTarget) -> GitHubReviewThreadComments;
    get_review_thread_comments(api::GetReviewThreadCommentsInput) -> PullRequestThreadCommentsResult;
    get_viewer_access(GetViewerAccessInput) -> GitHubViewerRepositoryAccess;
    get_pull_request_diff(GetPullRequestDiffInput) -> GitHubPullRequestDiffSlice;
    get_pull_request_diff_file_contents(GetPullRequestDiffFileContentsInput) -> GitHubPullRequestDiffFileContents;
    get_pull_request_files_viewed(GitHubPullRequestTarget) -> GitHubPullRequestFilesViewed;
    set_pull_request_files_viewed(SetPullRequestFilesViewedInput) -> ();
    list_reviewer_candidates(GitHubPullRequestTarget) -> PullRequestReviewerCandidateList;
    list_label_candidates(GitHubPullRequestTarget) -> PullRequestLabelCandidateList;
    run_pull_request_action(api::RunPullRequestActionInput) -> ();
    comment_on_pull_request(api::CommentOnPullRequestInput) -> ();
    submit_review(api::SubmitReviewInput) -> ();
    reply_to_review_thread(api::ReplyToReviewThreadInput) -> ();
    set_review_thread_resolution(api::SetReviewThreadResolutionInput) -> ();
    set_reviewer_request(api::SetReviewerRequestInput) -> ();
    set_labels(api::SetLabelsInput) -> ();
    set_reaction(api::SetReactionInput) -> ();
    update_pull_request(api::UpdatePullRequestInput) -> ();
    update_comment(api::UpdateCommentInput) -> ();
}

fn script<A, R>(
    f: impl Fn(&A) -> Result<R, GitHubPullRequestApiError> + Send + Sync + 'static,
) -> Scripted<A, R> {
    Some(Box::new(f))
}

fn api_error<E: Into<GitHubApiError>>(error: E) -> GitHubPullRequestApiError {
    GitHubPullRequestApiError::Api(error.into())
}

fn credential_error<E: Into<GitHubCredentialUnavailableError>>(
    error: E,
) -> GitHubPullRequestApiError {
    api_error(GitHubApiError::CredentialUnavailable(error.into()))
}

fn provider(mock: Mock) -> GitHubPullRequestProvider {
    make(Arc::new(mock))
}

fn request(number: u64) -> ProviderChangeRequestRef {
    ProviderChangeRequestRef {
        repo: ProviderRepositoryRef {
            cwd: "/w".into(),
            repository: "acme/web".into(),
            host: "github.com".into(),
        },
        number,
    }
}

fn tne(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).unwrap()
}

fn actor(login: &str) -> PullRequestActor {
    PullRequestActor {
        is_bot: None,
        login: tne(login),
        name: None,
        avatar_url: None,
    }
}

fn access(
    can_write: bool,
    can_triage: bool,
    can_update: bool,
    did_author: bool,
) -> GitHubViewerAccess {
    GitHubViewerAccess {
        can_write,
        can_triage,
        can_update,
        did_author,
        can_update_branch: None,
    }
}

fn all_merge_methods() -> PullRequestMergeCapabilities {
    PullRequestMergeCapabilities {
        merge: true,
        squash: true,
        rebase: true,
    }
}

/// `coreFields`' viewer access: write, triage and update, not the author.
fn viewer_access() -> GitHubViewerRepositoryAccess {
    GitHubViewerRepositoryAccess {
        access: access(true, true, true, false),
        merge_capabilities: all_merge_methods(),
    }
}

fn list_item() -> GitHubPullRequestListItem {
    GitHubPullRequestListItem {
        stack: None,
        author_id: None,
        number: 7,
        title: "Pull request 7".into(),
        url: "https://github.com/acme/web/pull/7".into(),
        author: None,
        head_branch: "feat/page".into(),
        base_branch: "main".into(),
        state: PullRequestState::Open,
        is_draft: false,
        mergeability: PullRequestMergeability::Mergeable,
        review_decision: None,
        additions: 1,
        deletions: 1,
        created_at: "2026-07-01T00:00:00Z".into(),
        updated_at: "2026-07-02T00:00:00Z".into(),
        review_request_logins: Vec::new(),
        has_team_review_request: false,
        labels: Vec::new(),
        checks_state: None,
    }
}

/// `openDetail`: an open pull request from a fork, with `coreFields` around it.
fn open_detail() -> GitHubPullRequestCore {
    GitHubPullRequestCore {
        base: GitHubPullRequestDetail {
            base: list_item(),
            is_cross_repository: Some(true),
            head_repository_owner: Some("acme".into()),
            head_sha: Some("abc123".into()),
            body: String::new(),
            changed_files: 1,
            merged_at: None,
            closed_at: None,
            checks: Vec::new(),
            auto_merge_enabled: None,
            auto_merge_method: None,
        },
        review_request_teams: Vec::new(),
        viewer_access: viewer_access(),
        comparison: Some(GitHubBaseComparison {
            behind_by: Some(0),
            viewer_can_update: true,
        }),
        checks_truncated: false,
    }
}

fn with_comparison(
    mut core: GitHubPullRequestCore,
    comparison: Option<GitHubBaseComparison>,
) -> GitHubPullRequestCore {
    core.comparison = comparison;
    core
}

fn check(name: &str, status: PullRequestCheckStatus, url: Option<&str>) -> PullRequestCheck {
    PullRequestCheck {
        name: tne(name),
        status,
        description: None,
        url: url.map(str::to_owned),
        required: None,
    }
}

fn workflow_approval_unknown() -> PullRequestCheck {
    PullRequestCheck {
        name: tne("Workflow approval status"),
        status: PullRequestCheckStatus::ActionRequired,
        description: Some(
            "GitHub could not determine whether workflows are awaiting approval.".into(),
        ),
        url: None,
        required: None,
    }
}

fn expected_permissions(
    actions: &[PullRequestAction],
    resolve: bool,
    verdicts: &[PullRequestReviewVerdict],
    request_reviewers: bool,
    labels: bool,
    stack_rebase: bool,
) -> PullRequestViewerPermissions {
    PullRequestViewerPermissions {
        stack_rebase: stack_rebase.then_some(true),
        actions: actions.to_vec(),
        comment: true,
        resolve,
        verdicts: verdicts.to_vec(),
        request_reviewers,
        update_methods: None,
        labels: Some(labels),
        edit_change_request: None,
    }
}

const ALL_VERDICTS: [PullRequestReviewVerdict; 3] = [
    PullRequestReviewVerdict::Comment,
    PullRequestReviewVerdict::Approve,
    PullRequestReviewVerdict::RequestChanges,
];

// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn maps_credential_verification_failures_without_relabeling_operation_failures() {
    // Upstream's `withVerifiedCredential(input, use)` ran `use` inside the verification. Here the
    // caller runs its work under the returned pin, so a failure of that work never passes through
    // this module at all; what remains to prove is that a failed verification is reported as
    // `routeIdentity` and a good one hands the identity back.
    let verification_fails = Arc::new(AtomicUsize::new(1));
    let flag = verification_fails.clone();
    let provider = provider(Mock {
        verify_credential: script(move |_| {
            if flag.load(Ordering::SeqCst) == 1 {
                Err(GitHubViewerLoginUnavailableError { cwd: "/w".into() }.into())
            } else {
                Ok((
                    VerifiedIdentity {
                        account_id: "123".into(),
                        viewer: "viewer".into(),
                        credential_fingerprint: "fingerprint".into(),
                    },
                    PinnedCredential {
                        host: "github.com".into(),
                        token: Redacted::new("credential".into()),
                        credential_fingerprint: "fingerprint".into(),
                    },
                ))
            }
        }),
        ..Default::default()
    });
    assert!(provider.supports(PullRequestOperation::VerifyCredential));
    let input = VerifiedCredentialInput {
        cwd: "/w".into(),
        host: "github.com".into(),
    };
    let error = provider.verify_credential(input.clone()).await.unwrap_err();
    assert_eq!(error.operation, "routeIdentity");
    assert_eq!(error.reason, PullRequestProviderErrorReason::Failed);
    assert_eq!(
        error.detail,
        "GitHub returned no login for the authenticated account."
    );
    assert_eq!(error.provider.as_ref(), "github");

    verification_fails.store(0, Ordering::SeqCst);
    let (identity, pinned) = provider.verify_credential(input).await.unwrap();
    assert_eq!(identity.account_id, "123");
    assert_eq!(identity.viewer, "viewer");
    assert_eq!(identity.credential_fingerprint, "fingerprint");
    assert_eq!(pinned.credential_fingerprint, "fingerprint");
}

#[tokio::test]
async fn refreshes_checks_without_permissions_or_comparison_reads() {
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = reads.clone();
    let snapshot = decode_pull_request_detail_json(
        r#"{
      "number": 7, "title": "Checks", "url": "https://github.com/acme/web/pull/7",
      "headRefName": "feature", "baseRefName": "main", "state": "OPEN",
      "createdAt": "2026-07-01T00:00:00Z", "updatedAt": "2026-07-01T00:00:00Z"
    }"#,
    )
    .unwrap();
    let provider = provider(Mock {
        get_pull_request_detail: script(move |_| {
            let read = counter.fetch_add(1, Ordering::SeqCst) + 1;
            let mut base = snapshot.clone();
            base.base.state = if read == 3 {
                PullRequestState::Merged
            } else {
                PullRequestState::Open
            };
            base.checks = vec![check(
                "build",
                if read == 1 {
                    PullRequestCheckStatus::Pending
                } else {
                    PullRequestCheckStatus::Success
                },
                None,
            )];
            Ok(GitHubPullRequestCore {
                base,
                review_request_teams: Vec::new(),
                viewer_access: viewer_access(),
                comparison: Some(GitHubBaseComparison {
                    behind_by: Some(0),
                    viewer_can_update: true,
                }),
                checks_truncated: false,
            })
        }),
        ..Default::default()
    });
    assert!(provider.supports(PullRequestOperation::GetChangeRequestChecks));
    for tick in 1..=3 {
        let result = provider
            .get_change_request_checks(request(7))
            .await
            .unwrap();
        assert_eq!(
            result.checks[0].status,
            if tick == 1 {
                PullRequestCheckStatus::Pending
            } else {
                PullRequestCheckStatus::Success
            }
        );
        assert_eq!(
            result.state,
            if tick == 3 {
                PullRequestState::Merged
            } else {
                PullRequestState::Open
            }
        );
    }
    assert_eq!(reads.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn uses_one_narrow_read_for_a_linked_pull_request_summary() {
    let summary_reads = Arc::new(AtomicUsize::new(0));
    let counter = summary_reads.clone();
    let provider = provider(Mock {
        get_pull_request_summary: script(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(ProviderChangeRequestSummary {
                number: 7,
                title: "Summary".into(),
                url: "https://github.com/acme/web/pull/7".into(),
                head_branch: "feat/summary".into(),
                base_branch: "main".into(),
                state: PullRequestState::Open,
                is_draft: None,
                closed_at: None,
                merged_at: None,
                updated_at: "2026-08-24T12:34:56.000Z".into(),
                author: Some(Some(actor("octocat"))),
                additions: None,
                deletions: None,
                changed_files: None,
                review_decision: None,
                checks_state: None,
                mergeability: None,
                stack: None,
            })
        }),
        ..Default::default()
    });
    assert!(provider.supports(PullRequestOperation::GetChangeRequestSummary));
    let summary = provider
        .get_change_request_summary(request(7))
        .await
        .unwrap();
    assert_eq!(summary.state, PullRequestState::Open);
    assert_eq!(summary_reads.load(Ordering::SeqCst), 1);
    // The author's avatar comes from the login-shaped URL, not a second request.
    assert_eq!(
        summary.author.unwrap().unwrap().avatar_url.as_deref(),
        Some("https://github.com/octocat.png?size=80")
    );
}

#[tokio::test]
async fn declares_host_native_stacks_and_passes_the_one_the_cli_reads_through() {
    let stack = GitHubPullRequestStack {
        id: "42".into(),
        number: 3,
        url: "https://github.com/acme/web/stacks/3".into(),
        base: "main".into(),
        layers: vec![
            crate::github::pull_request_json::GitHubPullRequestStackLayer {
                title: None,
                is_draft: None,
                head_sha: None,
                number: 6,
                head_branch: "feat/one".into(),
                state: PullRequestState::Merged,
            },
            crate::github::pull_request_json::GitHubPullRequestStackLayer {
                title: None,
                is_draft: None,
                head_sha: None,
                number: 7,
                head_branch: "feat/two".into(),
                state: PullRequestState::Open,
            },
        ],
    };
    let scripted = stack.clone();
    let provider = provider(Mock {
        get_pull_request_stack: script(move |input: &GetPullRequestStackInput| {
            Ok((input.target.number == 7).then(|| scripted.clone()))
        }),
        ..Default::default()
    });
    assert_eq!(provider.capabilities().stacks, Some(true));
    assert!(provider.supports(PullRequestOperation::GetChangeRequestStack));
    let expected = provider_stack(stack);
    assert_eq!(
        provider
            .get_change_request_stack(GetChangeRequestStackInput {
                request: request(7),
                include_details: false,
            })
            .await
            .unwrap(),
        Some(expected)
    );
    assert_eq!(
        provider
            .get_change_request_stack(GetChangeRequestStackInput {
                request: request(8),
                include_details: false,
            })
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn reports_a_failed_stack_read_against_its_own_operation() {
    let provider = provider(Mock {
        get_pull_request_stack: script(|_| {
            Err(GitHubPullRequestReadError {
                cwd: "/w".into(),
                operation: "getPullRequestStack".into(),
                cause: cause_from_message("unreadable"),
            }
            .into())
        }),
        ..Default::default()
    });
    let error = provider
        .get_change_request_stack(GetChangeRequestStackInput {
            request: request(7),
            include_details: false,
        })
        .await
        .unwrap_err();
    assert_eq!(error.operation, "getChangeRequestStack");
    assert_eq!(error.reason, PullRequestProviderErrorReason::Failed);
    assert_eq!(
        error.detail,
        "GitHub returned an unreadable getPullRequestStack response."
    );
}

mod viewer_permissions {
    use super::*;
    use PullRequestAction::*;

    #[test]
    fn offers_everything_to_a_viewer_who_can_write_to_the_repository() {
        assert_eq!(
            github_viewer_permissions(&access(true, true, true, false)),
            expected_permissions(
                // Arming a merge for later is the merge, so it travels with it.
                &[
                    Merge,
                    EnableAutoMerge,
                    DisableAutoMerge,
                    Revert,
                    ApproveWorkflows,
                    Ready,
                    Draft,
                    Close,
                    Reopen
                ],
                true,
                &ALL_VERDICTS,
                true,
                true,
                true
            )
        );
    }

    #[test]
    fn leaves_a_passer_by_on_a_repository_they_can_only_read_nothing_but_the_review() {
        // Every open-source pull request somebody else opened: GitHub says no to all five actions
        // and to resolving, and yes to commenting and to every verdict.
        assert_eq!(
            github_viewer_permissions(&access(false, false, false, false)),
            // Asking somebody else to review is the one thing read access never stretches to.
            expected_permissions(&[], false, &ALL_VERDICTS, false, false, false)
        );
    }

    #[test]
    fn lets_a_triager_label_without_letting_them_merge_or_ask_for_a_review() {
        let permissions = github_viewer_permissions(&access(false, true, false, false));
        assert_eq!(permissions.labels, Some(true));
        assert!(!permissions.request_reviewers);
        assert_eq!(permissions.actions, Vec::<PullRequestAction>::new());
    }

    #[test]
    fn keeps_an_authors_own_pull_request_theirs_to_close_with_read_access_and_no_more() {
        assert_eq!(
            github_viewer_permissions(&access(false, false, true, true)),
            // Merging is the one thing writing is needed for, now or later; the rest an author may
            // do. GitHub refuses an author's approval of their own change, so the page does not
            // offer one.
            expected_permissions(
                &[Ready, Draft, Close, Reopen],
                true,
                &[PullRequestReviewVerdict::Comment],
                false,
                false,
                false
            )
        );
    }

    #[test]
    fn offers_update_branch_only_where_github_said_the_viewer_may() {
        let mut viewer = access(true, true, true, false);
        viewer.can_update_branch = Some(true);
        let permissions = github_viewer_permissions(&viewer);
        assert_eq!(permissions.actions.last(), Some(&UpdateBranch));
        assert_eq!(
            permissions.update_methods,
            Some(vec![
                PullRequestUpdateMethod::Merge,
                PullRequestUpdateMethod::Rebase
            ])
        );
        viewer.can_update_branch = Some(false);
        let permissions = github_viewer_permissions(&viewer);
        assert!(!permissions.actions.contains(&UpdateBranch));
        assert_eq!(permissions.update_methods, None);
    }
}

mod core_read {
    use super::*;

    #[tokio::test]
    async fn uses_the_permissions_carried_by_the_core_read_without_a_second_request() {
        let provider = provider(Mock {
            get_pull_request_detail: script(|_| {
                // `GitHubApi.PinnedGitHubCredential` is read from the caller's context.
                let credential = ambient::pinned_credential();
                let mut viewer_access = viewer_access();
                viewer_access.access.can_write = false;
                viewer_access.access.can_triage = false;
                viewer_access.merge_capabilities.squash = credential
                    .map(|credential| credential.credential_fingerprint != "restricted")
                    .unwrap_or(true);
                let mut core = open_detail();
                core.comparison = None;
                core.viewer_access = viewer_access;
                core.base.head_repository_owner = None;
                core.base.head_sha = None;
                Ok(core)
            }),
            ..Default::default()
        });
        let detail = provider.get_change_request(request(7)).await.unwrap();
        assert_eq!(
            detail.viewer_permissions,
            expected_permissions(
                &[
                    PullRequestAction::Ready,
                    PullRequestAction::Draft,
                    PullRequestAction::Close,
                    PullRequestAction::Reopen
                ],
                false,
                &ALL_VERDICTS,
                false,
                false,
                false
            )
        );
        assert_eq!(detail.workflow_approvals_required, None);
        assert!(detail.checks.contains(&workflow_approval_unknown()));
        for fingerprint in ["broad", "restricted", "broad"] {
            let scoped = ambient::with_pinned_credential(
                Some(PinnedCredential {
                    host: "github.com".into(),
                    token: Redacted::new("credential".into()),
                    credential_fingerprint: fingerprint.into(),
                }),
                provider.get_change_request(request(7)),
            )
            .await
            .unwrap();
            assert_eq!(
                scoped.merge_capabilities.squash,
                fingerprint != "restricted"
            );
        }
    }

    #[tokio::test]
    async fn keeps_fork_workflows_awaiting_approval_out_of_the_passing_state() {
        let mut core = open_detail();
        core.base.head_repository_owner = Some("octocat".into());
        core.base.base.checks_state = Some(PullRequestChecksState::Passing);
        core.base.checks = vec![
            check(
                "manual gate",
                PullRequestCheckStatus::ActionRequired,
                Some("https://example.com/manual-gate"),
            ),
            check("build", PullRequestCheckStatus::Success, None),
        ];
        let provider = provider(Mock {
            get_pull_request_detail: script(move |_| Ok(core.clone())),
            list_workflow_runs_requiring_approval: script(|_| {
                Ok(vec![GitHubWorkflowRunApproval {
                    id: 123,
                    name: "contributor tests".into(),
                    url: Some("https://github.com/acme/web/actions/runs/123".into()),
                }])
            }),
            get_viewer_access: script(|_| Ok(viewer_access())),
            ..Default::default()
        });
        let detail = provider.get_change_request(request(7)).await.unwrap();
        let checks = provider
            .get_change_request_checks(request(7))
            .await
            .unwrap();
        assert_eq!(
            checks,
            PullRequestChecks {
                state: detail.state,
                checks: detail.checks.clone()
            }
        );
        assert_eq!(detail.workflow_approvals_required, Some(1));
        assert_eq!(
            detail.checks,
            vec![
                check(
                    "manual gate",
                    PullRequestCheckStatus::ActionRequired,
                    Some("https://example.com/manual-gate")
                ),
                check("build", PullRequestCheckStatus::Success, None),
                PullRequestCheck {
                    description: Some(
                        "A maintainer must approve this workflow before it can run.".into()
                    ),
                    ..check(
                        "contributor tests",
                        PullRequestCheckStatus::ActionRequired,
                        Some("https://github.com/acme/web/actions/runs/123")
                    )
                },
            ]
        );
    }

    #[tokio::test]
    async fn does_not_double_count_a_run_a_check_already_represents() {
        let mut core = open_detail();
        core.base.checks = vec![check(
            "tests",
            PullRequestCheckStatus::ActionRequired,
            Some("https://github.com/acme/web/actions/runs/123/job/9"),
        )];
        let provider = provider(Mock {
            get_pull_request_detail: script(move |_| Ok(core.clone())),
            list_workflow_runs_requiring_approval: script(|_| {
                Ok(vec![GitHubWorkflowRunApproval {
                    id: 123,
                    name: "tests".into(),
                    url: Some("https://github.com/acme/web/actions/runs/123".into()),
                }])
            }),
            ..Default::default()
        });
        let detail = provider.get_change_request(request(7)).await.unwrap();
        assert_eq!(detail.checks.len(), 1);
        assert_eq!(detail.workflow_approvals_required, Some(1));
    }

    #[tokio::test]
    async fn uses_the_core_comparison_and_permissions_while_preserving_workflow_approval_checks() {
        let provider = provider(Mock {
            get_pull_request_detail: script(|_| {
                Ok(with_comparison(
                    open_detail(),
                    Some(GitHubBaseComparison {
                        behind_by: Some(2),
                        viewer_can_update: true,
                    }),
                ))
            }),
            list_workflow_runs_requiring_approval: script(|_| {
                Ok(vec![GitHubWorkflowRunApproval {
                    id: 123,
                    name: "tests".into(),
                    url: Some("https://example.com/runs/123".into()),
                }])
            }),
            ..Default::default()
        });
        let detail = provider.get_change_request(request(7)).await.unwrap();
        assert_eq!(detail.behind_by, Some(2));
        assert_eq!(
            detail.base_comparison,
            Some(PullRequestBaseComparison::Behind)
        );
        assert_eq!(detail.workflow_approvals_required, Some(1));
        assert!(
            detail
                .viewer_permissions
                .actions
                .contains(&PullRequestAction::UpdateBranch)
        );
        assert!(detail.checks.contains(&PullRequestCheck {
            description: Some("A maintainer must approve this workflow before it can run.".into()),
            ..check(
                "tests",
                PullRequestCheckStatus::ActionRequired,
                Some("https://example.com/runs/123")
            )
        }));
    }

    #[tokio::test]
    async fn does_not_classify_same_repository_gates_as_fork_workflow_approvals() {
        let provider = provider(Mock {
            get_pull_request_detail: script(|_| {
                let mut core = open_detail();
                core.base.is_cross_repository = Some(false);
                Ok(core)
            }),
            list_workflow_runs_requiring_approval: script(|_| {
                panic!("same-repository pull requests must not probe fork workflow approvals")
            }),
            get_viewer_access: script(|_| Ok(viewer_access())),
            ..Default::default()
        });
        let detail = provider.get_change_request(request(7)).await.unwrap();
        assert_eq!(detail.workflow_approvals_required, Some(0));
        assert_eq!(detail.checks, Vec::<PullRequestCheck>::new());
    }

    #[tokio::test]
    async fn keeps_an_unsafe_workflow_approval_scope_visible_as_unknown() {
        let provider = provider(Mock {
            get_pull_request_detail: script(|_| Ok(open_detail())),
            list_workflow_runs_requiring_approval: script(|_| {
                Err(GitHubWorkflowApprovalRefusedError {
                    cwd: "/w".into(),
                    number: 7,
                    reason: GitHubWorkflowApprovalRefusedReason::HeadNotUnique,
                    observed_count: 2,
                    limit: 1_000,
                }
                .into())
            }),
            get_viewer_access: script(|_| Ok(viewer_access())),
            ..Default::default()
        });
        let detail = provider.get_change_request(request(7)).await.unwrap();
        assert_eq!(detail.workflow_approvals_required, None);
        assert_eq!(detail.checks, vec![workflow_approval_unknown()]);
    }

    #[tokio::test]
    async fn propagates_workflow_discovery_rate_limits() {
        let provider = provider(Mock {
            get_pull_request_detail: script(|_| Ok(open_detail())),
            list_workflow_runs_requiring_approval: script(|_| {
                Err(api_error(GitHubApiRateLimitError {
                    host: "github.com".into(),
                    operation: "listWorkflowRunsRequiringApproval".into(),
                    retry_at: None,
                }))
            }),
            get_viewer_access: script(|_| Ok(viewer_access())),
            ..Default::default()
        });
        let error = provider.get_change_request(request(7)).await.unwrap_err();
        assert_eq!(error.operation, "getChangeRequest");
        assert_eq!(error.reason, PullRequestProviderErrorReason::RateLimited);
    }

    #[tokio::test]
    async fn maps_the_detail_the_way_the_service_consumes_it() {
        let mut core = open_detail();
        core.base.base.review_request_logins = vec!["hubot".into()];
        core.review_request_teams = vec![actor("core-team")];
        core.base.merged_at = Some("2026-07-03T00:00:00Z".into());
        core.base.closed_at = Some("2026-07-03T00:00:00Z".into());
        core.base.auto_merge_enabled = Some(true);
        core.base.auto_merge_method = Some(PullRequestMergeMethod::Squash);
        core.base.base.author = Some(actor("octocat"));
        core.base.is_cross_repository = Some(false);
        core.comparison = Some(GitHubBaseComparison {
            behind_by: None,
            viewer_can_update: false,
        });
        let provider = provider(Mock {
            get_pull_request_detail: script(move |_| Ok(core.clone())),
            ..Default::default()
        });
        let detail = provider.get_change_request(request(7)).await.unwrap();
        assert_eq!(detail.reviewers, vec![actor("hubot"), actor("core-team")]);
        assert_eq!(
            detail.base_comparison,
            Some(PullRequestBaseComparison::Unknown)
        );
        assert_eq!(detail.behind_by, None);
        assert_eq!(detail.head_sha, Some(Some("abc123".into())));
        assert_eq!(detail.merged_at.as_deref(), Some("2026-07-03T00:00:00Z"));
        assert_eq!(
            detail.base.merged_at,
            Some(Some("2026-07-03T00:00:00Z".into()))
        );
        assert_eq!(
            detail.auto_merge_method,
            Some(PullRequestMergeMethod::Squash)
        );
        assert_eq!(detail.merge_capabilities, all_merge_methods());
        assert_eq!(
            detail.author.as_ref().unwrap().avatar_url.as_deref(),
            Some("https://github.com/octocat.png?size=80")
        );
        assert_eq!(detail.base.review_decision, Some(None));
        assert_eq!(detail.base.head_repository_name_with_owner, None);
    }
}

mod get_viewer_permissions {
    use super::*;

    fn permissions_input(include_update_branch: bool) -> GetViewerPermissionsInput {
        GetViewerPermissionsInput {
            request: request(7),
            include_update_branch,
        }
    }

    fn full_access() -> GitHubViewerRepositoryAccess {
        viewer_access()
    }

    #[tokio::test]
    async fn checks_fresh_access_without_reading_branch_details_for_unrelated_operations() {
        let access_reads = Arc::new(AtomicUsize::new(0));
        let counter = access_reads.clone();
        let provider = provider(Mock {
            get_pull_request_detail: script(|_| panic!("Unexpected detail read")),
            get_viewer_access: script(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(full_access())
            }),
            ..Default::default()
        });
        let permissions = provider
            .get_viewer_permissions(permissions_input(false))
            .await
            .unwrap();
        assert_eq!(access_reads.load(Ordering::SeqCst), 1);
        assert!(permissions.actions.contains(&PullRequestAction::Merge));
        assert!(
            !permissions
                .actions
                .contains(&PullRequestAction::UpdateBranch)
        );
    }

    fn detail_mock(
        detail: Result<GitHubPullRequestCore, GitHubPullRequestApiError>,
        on_detail: impl Fn(bool) + Send + Sync + 'static,
    ) -> GitHubPullRequestProvider {
        provider(Mock {
            get_pull_request_detail: script(move |_| {
                on_detail(ambient::interactive());
                detail.clone()
            }),
            get_viewer_access: script(|_| panic!("Unexpected access read")),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn answers_access_and_update_branch_from_the_one_detail_read() {
        let provider = detail_mock(
            Ok(with_comparison(
                open_detail(),
                Some(GitHubBaseComparison {
                    behind_by: Some(3),
                    viewer_can_update: true,
                }),
            )),
            |_| {},
        );
        let permissions = provider
            .get_viewer_permissions(permissions_input(true))
            .await
            .unwrap();
        assert!(permissions.actions.contains(&PullRequestAction::Merge));
        assert!(
            permissions
                .actions
                .contains(&PullRequestAction::UpdateBranch)
        );
        assert_eq!(
            permissions.update_methods,
            Some(vec![
                PullRequestUpdateMethod::Merge,
                PullRequestUpdateMethod::Rebase
            ])
        );
    }

    #[tokio::test]
    async fn uses_the_graphql_reserve_for_manual_permission_checks() {
        let allowed = Arc::new(Mutex::new(None));
        let slot = allowed.clone();
        let provider = detail_mock(Ok(open_detail()), move |allow_reserve| {
            *slot.lock() = Some(allow_reserve);
        });
        provider
            .get_viewer_permissions(permissions_input(true))
            .await
            .unwrap();
        assert_eq!(*allowed.lock(), Some(true));
    }

    #[tokio::test]
    async fn still_answers_from_the_light_access_read_when_the_detail_read_fails() {
        let provider = provider(Mock {
            get_pull_request_detail: script(|_| {
                Err(GitHubPullRequestReadError {
                    cwd: "/w".into(),
                    operation: "getPullRequestDetail".into(),
                    cause: cause_from_message("head changed"),
                }
                .into())
            }),
            get_viewer_access: script(|_| Ok(full_access())),
            ..Default::default()
        });
        let permissions = provider
            .get_viewer_permissions(permissions_input(true))
            .await
            .unwrap();
        assert!(permissions.actions.contains(&PullRequestAction::Merge));
        assert!(
            !permissions
                .actions
                .contains(&PullRequestAction::UpdateBranch)
        );
    }

    #[tokio::test]
    async fn does_not_fall_back_when_the_detail_read_was_rate_limited() {
        let provider = provider(Mock {
            get_pull_request_detail: script(|_| {
                Err(SourceControlRateLimitPausedError {
                    provider: kind(),
                    host: "github.com".into(),
                    retry_at: 5_000,
                }
                .into())
            }),
            get_viewer_access: script(|_| panic!("a rate limit is not worked around")),
            ..Default::default()
        });
        let error = provider
            .get_viewer_permissions(permissions_input(true))
            .await
            .unwrap_err();
        assert_eq!(error.operation, "getViewerPermissions");
        assert_eq!(error.reason, PullRequestProviderErrorReason::RateLimited);
        assert_eq!(error.retry_at, Some(5_000));
    }

    #[tokio::test]
    async fn withholds_update_branch_where_github_could_not_compare_the_branch() {
        let provider = detail_mock(Ok(with_comparison(open_detail(), None)), |_| {});
        let permissions = provider
            .get_viewer_permissions(permissions_input(true))
            .await
            .unwrap();
        assert!(
            !permissions
                .actions
                .contains(&PullRequestAction::UpdateBranch)
        );
        assert_eq!(permissions.update_methods, None);
        assert!(permissions.actions.contains(&PullRequestAction::Merge));
    }

    #[tokio::test]
    async fn offers_update_branch_for_nobody_once_the_pull_request_is_no_longer_open() {
        let mut core = open_detail();
        core.base.base.state = PullRequestState::Merged;
        let provider = detail_mock(Ok(core), |_| {});
        let permissions = provider
            .get_viewer_permissions(permissions_input(true))
            .await
            .unwrap();
        assert!(
            !permissions
                .actions
                .contains(&PullRequestAction::UpdateBranch)
        );
    }
}

mod commits {
    use super::*;

    fn commit(oid: &str, headline: &str, date: &str) -> PullRequestCommit {
        PullRequestCommit {
            oid: tne(oid),
            message_headline: headline.into(),
            committed_date: date.into(),
            additions: None,
            deletions: None,
            authors: Some(Vec::new()),
        }
    }

    pub(super) fn base_thread_comments() -> GitHubReviewThreadComments {
        GitHubReviewThreadComments {
            comments: Vec::new(),
            dismissals_by_review_id: IndexMap::new(),
            review_threads: Vec::new(),
            comment_count: 0,
            truncated: false,
            review_threads_truncated: false,
            reactions: Vec::new(),
            reactions_by_id: IndexMap::new(),
            edited_at_by_id: IndexMap::new(),
            reviewers: Vec::new(),
            avatars_by_login: IndexMap::new(),
            bot_logins: IndexSet::new(),
            commit_stats: IndexMap::new(),
            commits: Vec::new(),
            viewer: crate::github::pull_request_json::GitHubViewerFlags {
                can_update: true,
                did_author: false,
            },
        }
    }

    fn provider_with(commits: Vec<PullRequestCommit>) -> GitHubPullRequestProvider {
        provider(Mock {
            get_pull_request_activity: script(|_| {
                Ok(GitHubPullRequestActivity {
                    author: None,
                    comments: Vec::new(),
                    commits: vec![commit(
                        "view-oldest",
                        "gh pr view's oldest commit",
                        "2026-01-01T00:00:00Z",
                    )],
                })
            }),
            list_review_thread_comments: script(move |_| {
                Ok(GitHubReviewThreadComments {
                    commits: commits.clone(),
                    ..base_thread_comments()
                })
            }),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn prefers_the_graphql_commits_which_are_the_newest_over_the_gh_view_list() {
        let provider = provider_with(vec![commit(
            "graphql-newest",
            "the newest commit gh pr view drops",
            "2026-07-06T00:00:00Z",
        )]);
        let detail = provider
            .get_change_request_activity(request(7))
            .await
            .unwrap();
        assert_eq!(
            detail
                .commits
                .iter()
                .map(|c| c.oid.as_str())
                .collect::<Vec<_>>(),
            vec!["graphql-newest"]
        );
    }

    #[tokio::test]
    async fn falls_back_to_the_gh_view_list_when_the_graphql_read_has_no_commits() {
        let provider = provider_with(Vec::new());
        let detail = provider
            .get_change_request_activity(request(7))
            .await
            .unwrap();
        assert_eq!(
            detail
                .commits
                .iter()
                .map(|c| c.oid.as_str())
                .collect::<Vec<_>>(),
            vec!["view-oldest"]
        );
    }

    #[tokio::test]
    async fn degrades_to_a_truncated_conversation_when_the_thread_read_fails() {
        let provider = provider(Mock {
            get_pull_request_activity: script(|_| {
                Ok(GitHubPullRequestActivity {
                    author: None,
                    comments: Vec::new(),
                    commits: Vec::new(),
                })
            }),
            list_review_thread_comments: script(|_| {
                Err(GitHubPullRequestReadError {
                    cwd: "/w".into(),
                    operation: "listReviewThreadComments".into(),
                    cause: cause_from_message("nope"),
                }
                .into())
            }),
            ..Default::default()
        });
        let activity = provider
            .get_change_request_activity(request(7))
            .await
            .unwrap();
        assert!(activity.comments_truncated);
        assert_eq!(activity.review_threads_truncated, Some(true));
        assert_eq!(activity.comment_count, 0);
    }

    #[tokio::test]
    async fn fails_when_the_gh_view_read_fails() {
        let provider = provider(Mock {
            get_pull_request_activity: script(|_| {
                Err(api_error(GitHubApiNotFoundError {
                    host: "github.com".into(),
                    operation: "getPullRequestActivity".into(),
                }))
            }),
            list_review_thread_comments: script(|_| Ok(base_thread_comments())),
            ..Default::default()
        });
        let error = provider
            .get_change_request_activity(request(7))
            .await
            .unwrap_err();
        assert_eq!(error.operation, "getChangeRequestActivity");
        assert_eq!(error.reason, PullRequestProviderErrorReason::NotFound);
    }
}

mod dismissed_reviews {
    use super::*;

    fn dismissed_review(body: &str) -> PullRequestComment {
        PullRequestComment {
            id: tne("PRR_1"),
            kind: PullRequestCommentKind::Review,
            author: Some(actor("macroscopeapp")),
            body: body.into(),
            created_at: "2026-07-03T00:00:00Z".into(),
            edited_at: None,
            url: None,
            path: None,
            review_state: Some("DISMISSED".into()),
            reactions: None,
            can_edit: None,
        }
    }

    async fn read_activity(body: &str) -> ProviderChangeRequestActivity {
        let review = dismissed_review(body);
        let provider = provider(Mock {
            get_pull_request_activity: script(move |_| {
                Ok(GitHubPullRequestActivity {
                    author: None,
                    comments: vec![review.clone()],
                    commits: Vec::new(),
                })
            }),
            list_review_thread_comments: script(|_| {
                Ok(GitHubReviewThreadComments {
                    dismissals_by_review_id: IndexMap::from([(
                        "PRR_1".to_owned(),
                        "Dismissing prior approval to re-evaluate 9b66581".to_owned(),
                    )]),
                    edited_at_by_id: IndexMap::from([(
                        "PRR_1".to_owned(),
                        "2026-07-04T00:00:00Z".to_owned(),
                    )]),
                    bot_logins: IndexSet::from(["macroscopeapp".to_owned()]),
                    ..commits::base_thread_comments()
                })
            }),
            ..Default::default()
        });
        provider
            .get_change_request_activity(request(7))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn fills_a_marker_only_dismissed_review_with_the_timelines_reason() {
        // Macroscope's approvals carry only an HTML comment, which markdown renders as nothing —
        // an empty-string check misses them and the card opens onto nothing.
        let activity =
            read_activity("<!-- Macroscope (Approvability) review body marker -->").await;
        assert_eq!(
            activity.comments[0].body,
            "Dismissing prior approval to re-evaluate 9b66581"
        );
        assert_eq!(
            activity.comments[0].author.as_ref().unwrap().is_bot,
            Some(true)
        );
        assert_eq!(
            activity.comments[0].edited_at,
            Some(Some("2026-07-04T00:00:00Z".to_owned()))
        );
        assert_eq!(activity.comments[0].reactions, Some(Vec::new()));
    }

    #[tokio::test]
    async fn keeps_the_words_of_a_dismissed_review_that_has_its_own() {
        let activity = read_activity("These findings still stand.").await;
        assert_eq!(activity.comments[0].body, "These findings still stand.");
    }

    #[tokio::test]
    async fn sorts_the_conversation_oldest_first_and_counts_both_halves() {
        let mut early = dismissed_review("first");
        early.id = tne("IC_1");
        early.kind = PullRequestCommentKind::IssueComment;
        early.created_at = "2026-07-01T00:00:00Z".into();
        let late = dismissed_review("last");
        let (activity_comments, thread_comments) = (vec![late], vec![early]);
        let provider = provider(Mock {
            get_pull_request_activity: script(move |_| {
                Ok(GitHubPullRequestActivity {
                    author: None,
                    comments: activity_comments.clone(),
                    commits: Vec::new(),
                })
            }),
            list_review_thread_comments: script(move |_| {
                Ok(GitHubReviewThreadComments {
                    comments: thread_comments.clone(),
                    comment_count: 4,
                    ..commits::base_thread_comments()
                })
            }),
            ..Default::default()
        });
        let activity = provider
            .get_change_request_activity(request(7))
            .await
            .unwrap();
        assert_eq!(
            activity
                .comments
                .iter()
                .map(|c| c.body.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "last"]
        );
        assert_eq!(activity.comment_count, 5);
    }
}

mod editing {
    use super::*;

    #[tokio::test]
    async fn hands_a_rewrite_to_the_cli_as_the_request_named_it() {
        let rewrites: Arc<Mutex<Vec<String>>> = Arc::default();
        let (pull_request_log, comment_log) = (rewrites.clone(), rewrites.clone());
        let provider = provider(Mock {
            update_pull_request: script(move |input: &api::UpdatePullRequestInput| {
                pull_request_log.lock().push(format!(
                    "{}|{}|{}|{}|title={:?}|body={:?}",
                    input.target.cwd,
                    input.target.repository,
                    input.target.host,
                    input.target.number,
                    input.title,
                    input.body
                ));
                Ok(())
            }),
            update_comment: script(move |input: &api::UpdateCommentInput| {
                comment_log.lock().push(format!(
                    "{}|{}|{}|{}|{}|{:?}|{}",
                    input.target.cwd,
                    input.target.repository,
                    input.target.host,
                    input.target.number,
                    input.comment_id,
                    input.kind,
                    input.body
                ));
                Ok(())
            }),
            ..Default::default()
        });
        let capabilities = provider.capabilities();
        assert_eq!(
            capabilities.edit,
            Some(turin_contracts::pull_request::PullRequestEditCapabilities {
                change_request: true,
                comment: true
            })
        );
        provider
            .update_change_request(UpdateChangeRequestInput {
                request: request(7),
                title: Some("A better title".into()),
                body: None,
            })
            .await
            .unwrap();
        provider
            .update_comment(UpdateCommentInput {
                request: request(7),
                comment_id: "IC_1".into(),
                kind: CommentUpdateKind::ReviewComment,
                body: "Reworded.".into(),
            })
            .await
            .unwrap();
        assert_eq!(
            *rewrites.lock(),
            vec![
                "/w|acme/web|github.com|7|title=Some(\"A better title\")|body=None".to_owned(),
                "/w|acme/web|github.com|7|IC_1|ReviewComment|Reworded.".to_owned(),
            ]
        );
    }
}

mod login_avatar {
    use super::*;

    #[test]
    fn serves_a_users_picture_from_the_host_they_belong_to() {
        assert_eq!(
            login_avatar_url("octocat", "github.com").as_deref(),
            Some("https://github.com/octocat.png?size=80")
        );
        assert_eq!(
            login_avatar_url("octocat", "ghe.example.com").as_deref(),
            Some("https://ghe.example.com/octocat.png?size=80")
        );
    }

    #[test]
    fn has_nothing_for_an_app_which_names_no_page() {
        // `dependabot[bot]` has a picture, but not at `/dependabot[bot].png` — a guess that 404s is
        // worse than the initials it would replace.
        assert_eq!(login_avatar_url("dependabot[bot]", "github.com"), None);
    }

    #[test]
    fn refuses_anything_that_is_not_a_login_rather_than_building_a_url_out_of_it() {
        for login in [
            "../../etc",
            "a b",
            "-leading",
            &"x".repeat(40),
            "",
            "trailing\n",
        ] {
            assert_eq!(login_avatar_url(login, "github.com"), None, "{login:?}");
        }
        // The longest login GitHub allows.
        assert!(login_avatar_url(&"x".repeat(39), "github.com").is_some());
    }
}

// -------------------------------------------------------------------------------------------
// Beyond upstream's file: the mapping itself.
// -------------------------------------------------------------------------------------------

mod mapping {
    use super::*;

    #[test]
    fn maps_every_failure_that_means_github_is_out_of_reach() {
        use PullRequestProviderErrorReason::*;
        let cases: Vec<(
            GitHubPullRequestApiError,
            PullRequestProviderErrorReason,
            Option<i64>,
        )> = vec![
            (
                credential_error(GitHubCliMissingError {
                    host: "github.com".into(),
                }),
                MissingTool,
                None,
            ),
            (
                credential_error(GitHubNotSignedInError {
                    host: "github.com".into(),
                    account: None,
                }),
                Unauthenticated,
                None,
            ),
            (
                credential_error(GitHubHostDisabledError {
                    host: "github.com".into(),
                }),
                Unauthenticated,
                None,
            ),
            (
                api_error(GitHubApiAuthenticationError {
                    host: "github.com".into(),
                    operation: "x".into(),
                }),
                Unauthenticated,
                None,
            ),
            (
                api_error(GitHubApiRateLimitError {
                    host: "github.com".into(),
                    operation: "x".into(),
                    retry_at: Some(42),
                }),
                RateLimited,
                Some(42),
            ),
            (
                api_error(GitHubApiRateLimitError {
                    host: "github.com".into(),
                    operation: "x".into(),
                    retry_at: None,
                }),
                RateLimited,
                None,
            ),
            (
                SourceControlRateLimitPausedError {
                    provider: kind(),
                    host: "github.com".into(),
                    retry_at: 99,
                }
                .into(),
                RateLimited,
                Some(99),
            ),
            (
                api_error(GitHubApiNotFoundError {
                    host: "github.com".into(),
                    operation: "x".into(),
                }),
                NotFound,
                None,
            ),
            (
                credential_error(GitHubCliFailedError {
                    host: "github.com".into(),
                    cause: cause_from_message("timed out"),
                }),
                Failed,
                None,
            ),
            (
                GitHubViewerLoginUnavailableError { cwd: "/w".into() }.into(),
                Failed,
                None,
            ),
        ];
        for (error, reason, retry_at) in cases {
            let failure = github_provider_failure(&error);
            assert_eq!(failure.reason, reason, "{error}");
            assert_eq!(failure.retry_at, retry_at, "{error}");
        }
    }

    #[test]
    fn the_provider_error_carries_the_service_errors_own_message_and_cause() {
        let error = fail("listChangeRequests")(api_error(GitHubApiRateLimitError {
            host: "github.com".into(),
            operation: "x".into(),
            retry_at: Some(7),
        }));
        assert_eq!(error.operation, "listChangeRequests");
        assert_eq!(error.reason, PullRequestProviderErrorReason::RateLimited);
        assert_eq!(error.detail, "GitHub API rate limit exceeded.");
        assert_eq!(error.retry_at, Some(7));
        assert_eq!(error.provider.as_ref(), "github");
        assert!(error.cause.is_some());
    }

    #[test]
    fn declares_what_github_can_do() {
        let provider = provider(Mock::default());
        let capabilities = provider.capabilities();
        assert!(capabilities.diff && capabilities.comment && capabilities.search);
        assert_eq!(capabilities.actions.len(), 10);
        assert_eq!(capabilities.actions[5], PullRequestAction::UpdateBranch);
        assert_eq!(capabilities.reactions, Some(true));
        assert_eq!(
            capabilities.viewed_files,
            Some(turin_contracts::pull_request::PullRequestViewedFilesStore::Host)
        );
        assert_eq!(capabilities.stack_actions, Some(true));
        assert_eq!(capabilities.labels, Some(true));
        assert_eq!(provider.kind().as_ref(), "github");
        for supported in [
            PullRequestOperation::MergeMessageRewrite,
            PullRequestOperation::RoutingIdentity,
            PullRequestOperation::ListChangeRequestsAcross,
            PullRequestOperation::SetLabels,
            PullRequestOperation::UpdateComment,
        ] {
            assert!(provider.supports(supported), "{supported:?}");
        }
        // GitHub keeps no environment-side viewed marks and addresses repositories as owner/name.
        for unsupported in [
            PullRequestOperation::GetFileRevisions,
            PullRequestOperation::RepositoryKey,
        ] {
            assert!(!provider.supports(unsupported), "{unsupported:?}");
        }
    }

    #[test]
    fn rewrites_a_merge_message_by_stripping_agent_credits() {
        let provider = provider(Mock::default());
        let message = "Fix the thing\n\nCo-authored-by: Claude <noreply@anthropic.com>";
        let rewritten = provider.merge_message_rewrite(message).unwrap();
        assert_eq!(rewritten, remove_agent_credits(message));
        assert!(!rewritten.contains("Co-authored-by"));
    }

    #[tokio::test]
    async fn lists_with_the_avatars_the_second_request_found_and_survives_its_failure() {
        let mut with_id = list_item();
        with_id.author = Some(actor("octocat"));
        with_id.author_id = Some("U_1".into());
        let mut bot = list_item();
        bot.number = 8;
        bot.author = Some(actor("dependabot[bot]"));
        bot.author_id = Some("U_1".into());
        let items = vec![with_id, bot];
        let asked: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        let log = asked.clone();
        let fail_lookup = Arc::new(AtomicUsize::new(0));
        let flag = fail_lookup.clone();
        let provider = provider(Mock {
            list_pull_requests: script(move |input: &ListPullRequestsInput| {
                assert_eq!(input.limit, 25);
                Ok(GitHubPullRequestListBatch {
                    items: items.clone(),
                    truncated: true,
                    continues: true,
                })
            }),
            list_actor_avatars: script(move |input: &ListActorAvatarsInput| {
                log.lock().push(input.ids.clone());
                if flag.load(Ordering::SeqCst) == 1 {
                    return Err(api_error(GitHubApiNotFoundError {
                        host: "github.com".into(),
                        operation: "listActorAvatars".into(),
                    }));
                }
                Ok(IndexMap::from([(
                    "octocat".to_owned(),
                    "https://avatars.example/octocat".to_owned(),
                )]))
            }),
            ..Default::default()
        });
        let list_input = || ListChangeRequestsInput {
            repo: request(0).repo,
            state: turin_contracts::pull_request::PullRequestListState::Open,
            involvement: turin_contracts::pull_request::PullRequestInvolvement::All,
            viewer: "viewer".into(),
            limit: 25,
            query: None,
            cursor: None,
            filters: None,
        };
        let page = provider.list_change_requests(list_input()).await.unwrap();
        // One id, however many rows share it.
        assert_eq!(*asked.lock(), vec![vec!["U_1".to_owned()]]);
        assert!(page.truncated && page.continues);
        assert_eq!(page.cursor_advance, None);
        assert_eq!(
            page.items[0].author.as_ref().unwrap().avatar_url.as_deref(),
            Some("https://avatars.example/octocat")
        );
        // An app has no login-shaped page, so it keeps its initials.
        assert_eq!(page.items[1].author.as_ref().unwrap().avatar_url, None);
        assert_eq!(page.items[1].number, 8);

        fail_lookup.store(1, Ordering::SeqCst);
        let page = provider.list_change_requests(list_input()).await.unwrap();
        assert_eq!(
            page.items[0].author.as_ref().unwrap().avatar_url.as_deref(),
            Some("https://github.com/octocat.png?size=80")
        );
    }

    #[tokio::test]
    async fn passes_an_action_through_with_everything_the_request_named() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = seen.clone();
        let provider = provider(Mock {
            run_pull_request_action: script(move |input: &api::RunPullRequestActionInput| {
                log.lock().push(format!(
                    "{:?}|{:?}|{}|{:?}|{:?}",
                    input.action,
                    input.stack_number,
                    input.remove_agent_credits_on_merge,
                    input.merge_method,
                    input.update_method
                ));
                Ok(())
            }),
            ..Default::default()
        });
        provider
            .run_action(RunActionInput {
                request: request(7),
                action: PullRequestAction::Merge,
                stack_number: Some(3),
                expected_stack_heads: None,
                remove_agent_credits_on_merge: true,
                merge_method: Some(PullRequestMergeMethod::Squash),
                update_method: None,
            })
            .await
            .unwrap();
        assert_eq!(
            *seen.lock(),
            vec!["Merge|Some(3)|true|Some(Squash)|None".to_owned()]
        );
    }

    #[tokio::test]
    async fn defaults_the_viewer_lookup_to_github_dot_com() {
        let hosts: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = hosts.clone();
        let provider = provider(Mock {
            get_viewer_login: script(move |input: &VerifiedCredentialInput| {
                log.lock().push(input.host.clone());
                Ok("octocat".into())
            }),
            ..Default::default()
        });
        assert_eq!(provider.get_viewer("/w", None).await.unwrap(), "octocat");
        assert_eq!(
            provider
                .get_viewer("/w", Some("ghe.example.com"))
                .await
                .unwrap(),
            "octocat"
        );
        assert_eq!(*hosts.lock(), vec!["github.com", "ghe.example.com"]);
    }
}

/// Port of upstream/apps/server/src/pullRequest/PullRequestProviderRateLimit.test.ts, its GitHub
/// cases. GitLab, Azure DevOps and Bitbucket are cut with their hosts.
#[cfg(test)]
mod upstream_rate_limit {
    use super::*;

    #[test]
    fn classifies_a_github_rate_limit_as_rate_limited() {
        let failure = github_provider_failure(&api_error(GitHubApiRateLimitError {
            host: "github.com".into(),
            operation: "execute".into(),
            retry_at: None,
        }));
        assert_eq!(failure.reason, PullRequestProviderErrorReason::RateLimited);
        assert_eq!(failure.retry_at, None);
    }

    #[test]
    fn keeps_githubs_exact_retry_time() {
        let paused = SourceControlRateLimitPausedError {
            provider: SourceControlProviderKind::new("github").unwrap(),
            host: "github.com".into(),
            retry_at: 1_786_802_400_000,
        };
        let failure = github_provider_failure(&api_error(GitHubApiError::Paused(paused)));
        assert_eq!(failure.reason, PullRequestProviderErrorReason::RateLimited);
        assert_eq!(failure.retry_at, Some(1_786_802_400_000));
    }
}
