//! Fixtures shared by the service tests: a scripted provider (`fakeProvider`), project shells
//! (`project`), provider rows (`changeRequest`), a clock that moves with tokio's paused time
//! (`TestClock`), and the service wired to in-memory ports (`makeService`).

#![allow(dead_code)] // scripted helpers the service tests still to be ported will use

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use futures::FutureExt;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{Value, json};
use turin_contracts::base_schemas::{PositiveInt, ProjectId, TrimmedNonEmptyString};
use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::orchestration_project::OrchestrationProjectShell;
use turin_contracts::pull_request::{
    PullRequestActor, PullRequestCapabilities, PullRequestInvalidateInput,
    PullRequestLabelCandidateList, PullRequestMergeCapabilities, PullRequestMergeability,
    PullRequestRef, PullRequestReviewerCandidateList, PullRequestState,
    PullRequestThreadCommentsResult, PullRequestViewerPermissions,
};
use turin_contracts::source_control::{SourceControlProviderError, SourceControlProviderKind};
use turin_persistence::Db;

use super::errors::PullRequestError;
use super::ports::{PullRequestProjectSource, PullRequestSettings, RepositoryIdentityResolver};
use super::{PullRequestService, PullRequestServiceDependencies};
use crate::pull_request::provider_registry::PullRequestProviderRegistry;
use crate::pull_request::read_cache::PullRequestReadCache;
use crate::source_control_core::Cause;
use crate::source_control_core::clock::Clock;
use crate::source_control_core::pull_request_provider::*;
use crate::source_control_core::rate_limit::SourceControlRateLimit;
use crate::source_control_core::source_control_provider::{
    ResolveProviderContext, SourceControlProviderContext,
};

// ---------------------------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------------------------

/// `TestClock`: starts at the epoch and moves only when the test advances tokio's paused time, so
/// `tokio::time::sleep` and the service's own reads of the clock agree. Use with
/// `#[tokio::test(start_paused = true)]`.
pub struct TestClock {
    start: tokio::time::Instant,
}

impl TestClock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            start: tokio::time::Instant::now(),
        })
    }

    /// `TestClock.adjust`
    pub async fn adjust(&self, millis: i64) {
        tokio::time::advance(std::time::Duration::from_millis(millis as u64)).await;
    }

    /// `TestClock.setTime`
    pub async fn set_time(&self, millis: i64) {
        let now = self.now_millis();
        if millis > now {
            self.adjust(millis - now).await;
        }
    }
}

impl Clock for TestClock {
    fn now_millis(&self) -> i64 {
        self.start.elapsed().as_millis() as i64
    }
}

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

pub fn kind(value: &str) -> SourceControlProviderKind {
    SourceControlProviderKind::new(value).unwrap()
}

pub fn tne(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).unwrap()
}

/// `project({ id, title, workspaceRoot, repository, provider, host, remoteUrl })`
#[derive(Clone)]
pub struct ProjectSpec {
    pub id: &'static str,
    pub title: &'static str,
    pub workspace_root: &'static str,
    pub repository: Option<&'static str>,
    pub provider: Option<&'static str>,
    pub host: Option<&'static str>,
    pub remote_url: Option<String>,
}

pub fn project(
    id: &'static str,
    title: &'static str,
    workspace_root: &'static str,
    repository: &'static str,
) -> ProjectSpec {
    ProjectSpec {
        id,
        title,
        workspace_root,
        repository: Some(repository),
        provider: None,
        host: None,
        remote_url: None,
    }
}

impl ProjectSpec {
    pub fn provider(mut self, provider: &'static str) -> Self {
        self.provider = Some(provider);
        self
    }
    pub fn host(mut self, host: &'static str) -> Self {
        self.host = Some(host);
        self
    }
    pub fn remote_url(mut self, remote_url: &str) -> Self {
        self.remote_url = Some(remote_url.to_owned());
        self
    }
    /// A project with no repository identity at all.
    pub fn without_repository(mut self) -> Self {
        self.repository = None;
        self
    }

    pub fn build(&self) -> OrchestrationProjectShell {
        // The host defaults from the provider, so a fixture only names one when the point of the
        // test is two hosts of the same kind.
        let host = self.host.unwrap_or(if self.provider == Some("gitlab") {
            "gitlab.com"
        } else {
            "github.com"
        });
        let mut shell = json!({
            "id": self.id,
            "title": self.title,
            "workspaceRoot": self.workspace_root,
            "defaultModelSelection": null,
            "scripts": [],
            "createdAt": "2026-07-01T00:00:00Z",
            "updatedAt": "2026-07-01T00:00:00Z",
        });
        if let Some(repository) = self.repository {
            shell["repositoryIdentity"] = json!({
                "canonicalKey": format!("{host}/{repository}"),
                "locator": {
                    "source": "git-remote",
                    "remoteName": "origin",
                    "remoteUrl": self
                        .remote_url
                        .clone()
                        .unwrap_or_else(|| format!("https://{host}/{repository}.git")),
                },
                "provider": self.provider.unwrap_or("github"),
                "displayName": repository,
            });
        }
        serde_json::from_value(shell).unwrap()
    }
}

pub fn pr_ref(project_id: &str, repository: &str, number: u64) -> PullRequestRef {
    PullRequestRef {
        project_id: ProjectId::new(project_id).unwrap(),
        host: None,
        expected_account_id: None,
        allow_stale: None,
        repository: tne(repository),
        number: PositiveInt::new(number).unwrap(),
    }
}

pub fn octocat() -> PullRequestActor {
    PullRequestActor {
        is_bot: None,
        login: tne("octocat"),
        name: None,
        avatar_url: None,
    }
}

/// `changeRequest(number, updatedAt)`
pub fn change_request(number: u64, updated_at: &str) -> ProviderChangeRequest {
    ProviderChangeRequest {
        stack: None,
        number,
        title: format!("Change request {number}"),
        url: format!("https://host/pull/{number}"),
        author: Some(octocat()),
        head_branch: format!("feat/{number}"),
        head_repository_name_with_owner: None,
        base_branch: "main".to_owned(),
        state: PullRequestState::Open,
        is_draft: false,
        mergeability: PullRequestMergeability::Mergeable,
        additions: 1,
        deletions: 0,
        created_at: "2026-07-01T00:00:00Z".to_owned(),
        closed_at: None,
        merged_at: None,
        updated_at: updated_at.to_owned(),
        review_request_logins: Vec::new(),
        labels: Vec::new(),
        review_decision: None,
        checks_state: None,
    }
}

/// `hostedChangeRequest(body, additions)`
pub fn hosted_change_request(body: &str, additions: u64) -> ProviderChangeRequestDetail {
    ProviderChangeRequestDetail {
        base: ProviderChangeRequest {
            additions,
            ..change_request(1, "2026-07-02T00:00:00Z")
        },
        head_sha: None,
        body: body.to_owned(),
        changed_files: 2,
        merged_at: None,
        closed_at: None,
        reviewers: Vec::new(),
        checks: Vec::new(),
        merge_capabilities: PullRequestMergeCapabilities {
            merge: true,
            squash: true,
            rebase: true,
        },
        viewer_permissions: serde_json::from_value(json!({
            "actions": ["merge"],
            "comment": true,
            "resolve": true,
            "verdicts": ["comment", "approve", "request-changes"],
            "requestReviewers": true,
        }))
        .unwrap(),
        base_comparison: None,
        behind_by: None,
        auto_merge_enabled: None,
        auto_merge_method: None,
        workflow_approvals_required: None,
    }
}

/// `batchedChangeRequest(number, repository, updatedAt)`
pub fn batched_change_request(
    number: u64,
    repository: &str,
    updated_at: &str,
) -> ProviderBatchedChangeRequest {
    ProviderBatchedChangeRequest {
        base: change_request(number, updated_at),
        repository: repository.to_owned(),
    }
}

/// What a host with a narrow preview read hands back for `changeRequest(number, updatedAt)`.
pub fn preview_of(change_request: &ProviderChangeRequest) -> ProviderChangeRequestPreview {
    ProviderChangeRequestPreview {
        number: change_request.number,
        title: change_request.title.clone(),
        url: change_request.url.clone(),
        author: change_request.author.clone(),
        state: change_request.state,
        is_draft: change_request.is_draft,
        created_at: change_request.created_at.clone(),
    }
}

/// Decodes a service input from the object literal a test would write upstream.
pub fn input<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}

/// A reference as the object literal upstream spreads into other inputs (`{ ...ref, ... }`).
pub fn ref_json(reference: &PullRequestRef) -> Value {
    serde_json::to_value(reference).unwrap()
}

/// `{ ...ref, ...extra }`
pub fn with_ref(reference: &PullRequestRef, extra: Value) -> Value {
    let mut value = ref_json(reference);
    if let (Some(target), Value::Object(extra)) = (value.as_object_mut(), extra) {
        target.extend(extra);
    }
    value
}

/// `service.invalidate({ reference })`
pub fn invalidate_ref(reference: &PullRequestRef) -> PullRequestInvalidateInput {
    PullRequestInvalidateInput {
        reference: Some(reference.clone()),
        files_viewed_only: None,
    }
}

pub fn empty_activity() -> ProviderChangeRequestActivity {
    ProviderChangeRequestActivity {
        author: None,
        reviewers: None,
        comments: Vec::new(),
        comment_count: 0,
        comments_truncated: false,
        review_threads_truncated: None,
        review_threads: Vec::new(),
        commits: Vec::new(),
        reactions: None,
    }
}

pub fn page(items: Vec<ProviderChangeRequest>, truncated: bool) -> ProviderChangeRequestPage {
    ProviderChangeRequestPage {
        items,
        truncated,
        cursor_advance: None,
        continues: true,
    }
}

pub fn provider_error(
    provider: &str,
    operation: &str,
    reason: PullRequestProviderErrorReason,
    detail: &str,
) -> PullRequestProviderError {
    PullRequestProviderError::new(kind(provider), operation, reason, detail)
}

/// `unusable(provider, reason)`
pub fn unusable(
    provider: &str,
    reason: PullRequestProviderErrorReason,
) -> PullRequestProviderError {
    provider_error(
        provider,
        "getViewer",
        reason,
        &format!("{provider} is not usable."),
    )
}

/// `requestFailed`
pub fn request_failed() -> PullRequestProviderError {
    provider_error(
        "github",
        "listChangeRequests",
        PullRequestProviderErrorReason::Failed,
        "HTTP 404",
    )
}

// ---------------------------------------------------------------------------------------------
// Scripted provider
// ---------------------------------------------------------------------------------------------

type Handler<A, R> = Arc<dyn Fn(A) -> BoxFuture<'static, ProviderResult<R>> + Send + Sync>;

type Fallback<R> = fn(&SourceControlProviderKind) -> ProviderResult<R>;

macro_rules! fake_provider_methods {
    ($(
        $field:ident / $method:ident / $sync:ident / $asynch:ident :
        $arg:ty => $ret:ty, op $op:expr, name $name:literal, default $default:expr
    );* $(;)?) => {
        /// A provider whose every call is supplied by the test; anything unset succeeds emptily.
        pub struct FakeProvider {
            kind: SourceControlProviderKind,
            capabilities: PullRequestCapabilities,
            rewrite: bool,
            repository_key_is_canonical: bool,
            get_viewer: Option<Handler<(String, Option<String>), String>>,
            $( $field: Option<Handler<$arg, $ret>>, )*
        }

        impl FakeProvider {
            $(
                #[allow(dead_code)]
                pub fn $sync(
                    mut self,
                    handler: impl Fn($arg) -> ProviderResult<$ret> + Send + Sync + 'static,
                ) -> Self {
                    self.$field = Some(Arc::new(move |arg| {
                        let result = handler(arg);
                        async move { result }.boxed()
                    }));
                    self
                }

                #[allow(dead_code)]
                pub fn $asynch<Fut>(
                    mut self,
                    handler: impl Fn($arg) -> Fut + Send + Sync + 'static,
                ) -> Self
                where
                    Fut: Future<Output = ProviderResult<$ret>> + Send + 'static,
                {
                    self.$field = Some(Arc::new(move |arg| handler(arg).boxed()));
                    self
                }
            )*
        }

        #[async_trait]
        impl PullRequestProviderApi for FakeProvider {
            fn kind(&self) -> SourceControlProviderKind {
                self.kind.clone()
            }

            fn capabilities(&self) -> &PullRequestCapabilities {
                &self.capabilities
            }

            fn supports(&self, operation: PullRequestOperation) -> bool {
                match operation {
                    PullRequestOperation::MergeMessageRewrite => return self.rewrite,
                    PullRequestOperation::RepositoryKey => return self.repository_key_is_canonical,
                    _ => {}
                }
                $(
                    let op: Option<PullRequestOperation> = $op;
                    if op == Some(operation) {
                        return self.$field.is_some();
                    }
                )*
                false
            }

            fn merge_message_rewrite(&self, message: &str) -> Option<String> {
                self.rewrite.then(|| message.to_owned())
            }

            fn repository_key(&self, canonical_key: &str) -> Option<String> {
                self.repository_key_is_canonical
                    .then(|| canonical_key.to_owned())
            }

            async fn get_viewer(&self, cwd: &str, host: Option<&str>) -> ProviderResult<String> {
                match &self.get_viewer {
                    Some(handler) => handler((cwd.to_owned(), host.map(str::to_owned))).await,
                    None => Ok("bilal".to_owned()),
                }
            }

            $(
                async fn $method(&self, input: $arg) -> ProviderResult<$ret> {
                    match &self.$field {
                        Some(handler) => handler(input).await,
                        None => {
                            let fallback: Fallback<$ret> = $default;
                            fallback(&self.kind)
                        }
                    }
                }
            )*
        }
    };
}

fn unused<R>(_: &SourceControlProviderKind) -> ProviderResult<R> {
    panic!("unused")
}

fn full_permissions() -> PullRequestViewerPermissions {
    serde_json::from_value(json!({
        "actions": ["merge", "ready", "draft", "close", "reopen"],
        "comment": true,
        "resolve": true,
        "verdicts": ["comment", "approve", "request-changes"],
        "requestReviewers": true,
    }))
    .unwrap()
}

fake_provider_methods! {
    list_change_requests / list_change_requests / on_list_change_requests / on_list_change_requests_async:
        ListChangeRequestsInput => ProviderChangeRequestPage, op None, name "listChangeRequests",
        default |_| Ok(ProviderChangeRequestPage { items: Vec::new(), truncated: false, cursor_advance: None, continues: true });
    list_change_requests_across / list_change_requests_across / on_list_change_requests_across / on_list_change_requests_across_async:
        ListChangeRequestsAcrossInput => ProviderBatchedChangeRequestPage, op Some(PullRequestOperation::ListChangeRequestsAcross), name "listChangeRequestsAcross",
        default |k| Err(unsupported(k, "listChangeRequestsAcross"));
    list_change_request_stats / list_change_request_stats / on_list_change_request_stats / on_list_change_request_stats_async:
        ListChangeRequestStatsInput => Vec<ProviderChangeRequestStat>, op Some(PullRequestOperation::ListChangeRequestStats), name "listChangeRequestStats",
        default |k| Err(unsupported(k, "listChangeRequestStats"));
    get_change_request_checks / get_change_request_checks / on_get_change_request_checks / on_get_change_request_checks_async:
        ProviderChangeRequestRef => turin_contracts::pull_request::PullRequestChecks, op Some(PullRequestOperation::GetChangeRequestChecks), name "getChangeRequestChecks",
        default |k| Err(unsupported(k, "getChangeRequestChecks"));
    get_change_request / get_change_request / on_get_change_request / on_get_change_request_async:
        ProviderChangeRequestRef => ProviderChangeRequestDetail, op None, name "getChangeRequest",
        default unused;
    get_change_request_preview / get_change_request_preview / on_get_change_request_preview / on_get_change_request_preview_async:
        ProviderChangeRequestRef => ProviderChangeRequestPreview, op Some(PullRequestOperation::GetChangeRequestPreview), name "getChangeRequestPreview",
        default |k| Err(unsupported(k, "getChangeRequestPreview"));
    get_change_request_summary / get_change_request_summary / on_get_change_request_summary / on_get_change_request_summary_async:
        ProviderChangeRequestRef => ProviderChangeRequestSummary, op Some(PullRequestOperation::GetChangeRequestSummary), name "getChangeRequestSummary",
        default |k| Err(unsupported(k, "getChangeRequestSummary"));
    get_change_request_watch_fingerprint / get_change_request_watch_fingerprint / on_get_change_request_watch_fingerprint / on_get_change_request_watch_fingerprint_async:
        ProviderChangeRequestRef => Option<ProviderChangeRequestWatchFingerprint>, op Some(PullRequestOperation::GetChangeRequestWatchFingerprint), name "getChangeRequestWatchFingerprint",
        default |k| Err(unsupported(k, "getChangeRequestWatchFingerprint"));
    get_change_request_stack / get_change_request_stack / on_get_change_request_stack / on_get_change_request_stack_async:
        GetChangeRequestStackInput => Option<ProviderChangeRequestStack>, op Some(PullRequestOperation::GetChangeRequestStack), name "getChangeRequestStack",
        default |k| Err(unsupported(k, "getChangeRequestStack"));
    get_change_request_activity / get_change_request_activity / on_get_change_request_activity / on_get_change_request_activity_async:
        ProviderChangeRequestRef => ProviderChangeRequestActivity, op None, name "getChangeRequestActivity",
        default unused;
    get_review_thread_comments / get_review_thread_comments / on_get_review_thread_comments / on_get_review_thread_comments_async:
        GetReviewThreadCommentsInput => PullRequestThreadCommentsResult, op Some(PullRequestOperation::GetReviewThreadComments), name "getReviewThreadComments",
        default |k| Err(unsupported(k, "getReviewThreadComments"));
    get_viewer_permissions / get_viewer_permissions / on_get_viewer_permissions / on_get_viewer_permissions_async:
        GetViewerPermissionsInput => PullRequestViewerPermissions, op None, name "getViewerPermissions",
        default |_| Ok(full_permissions());
    get_diff / get_diff / on_get_diff / on_get_diff_async:
        GetDiffInput => ProviderDiffSlice, op None, name "getDiff",
        default unused;
    get_diff_file_contents / get_diff_file_contents / on_get_diff_file_contents / on_get_diff_file_contents_async:
        GetDiffFileContentsInput => ProviderDiffFileContents, op Some(PullRequestOperation::GetDiffFileContents), name "getDiffFileContents",
        default |k| Err(unsupported(k, "getDiffFileContents"));
    get_files_viewed / get_files_viewed / on_get_files_viewed / on_get_files_viewed_async:
        ProviderChangeRequestRef => ProviderFilesViewed, op Some(PullRequestOperation::GetFilesViewed), name "getFilesViewed",
        default |k| Err(unsupported(k, "getFilesViewed"));
    set_files_viewed / set_files_viewed / on_set_files_viewed / on_set_files_viewed_async:
        SetFilesViewedInput => (), op Some(PullRequestOperation::SetFilesViewed), name "setFilesViewed",
        default |k| Err(unsupported(k, "setFilesViewed"));
    get_file_revisions / get_file_revisions / on_get_file_revisions / on_get_file_revisions_async:
        GetFileRevisionsInput => ProviderFileRevisions, op Some(PullRequestOperation::GetFileRevisions), name "getFileRevisions",
        default |k| Err(unsupported(k, "getFileRevisions"));
    run_action / run_action / on_run_action / on_run_action_async:
        RunActionInput => (), op None, name "runAction", default |_| Ok(());
    update_change_request / update_change_request / on_update_change_request / on_update_change_request_async:
        UpdateChangeRequestInput => (), op Some(PullRequestOperation::UpdateChangeRequest), name "updateChangeRequest",
        default |k| Err(unsupported(k, "updateChangeRequest"));
    comment / comment / on_comment / on_comment_async:
        CommentInput => (), op None, name "comment", default |_| Ok(());
    update_comment / update_comment / on_update_comment / on_update_comment_async:
        UpdateCommentInput => (), op Some(PullRequestOperation::UpdateComment), name "updateComment",
        default |k| Err(unsupported(k, "updateComment"));
    submit_review / submit_review / on_submit_review / on_submit_review_async:
        SubmitReviewInput => (), op None, name "submitReview", default |_| Ok(());
    list_reviewer_candidates / list_reviewer_candidates / on_list_reviewer_candidates / on_list_reviewer_candidates_async:
        ProviderChangeRequestRef => PullRequestReviewerCandidateList, op None, name "listReviewerCandidates",
        default |_| Ok(serde_json::from_value(json!({ "candidates": [], "truncated": false })).unwrap());
    set_reviewer_request / set_reviewer_request / on_set_reviewer_request / on_set_reviewer_request_async:
        SetReviewerRequestInput => (), op None, name "setReviewerRequest", default |_| Ok(());
    list_label_candidates / list_label_candidates / on_list_label_candidates / on_list_label_candidates_async:
        ProviderChangeRequestRef => PullRequestLabelCandidateList, op Some(PullRequestOperation::ListLabelCandidates), name "listLabelCandidates",
        default |k| Err(unsupported(k, "listLabelCandidates"));
    set_labels / set_labels / on_set_labels / on_set_labels_async:
        SetLabelsInput => (), op Some(PullRequestOperation::SetLabels), name "setLabels",
        default |k| Err(unsupported(k, "setLabels"));
    reply_to_thread / reply_to_thread / on_reply_to_thread / on_reply_to_thread_async:
        ReplyToThreadInput => (), op None, name "replyToThread", default |_| Ok(());
    set_reaction / set_reaction / on_set_reaction / on_set_reaction_async:
        SetReactionInput => (), op None, name "setReaction", default |_| Ok(());
    set_thread_resolution / set_thread_resolution / on_set_thread_resolution / on_set_thread_resolution_async:
        SetThreadResolutionInput => (), op None, name "setThreadResolution", default |_| Ok(());
    get_routing_identity / get_routing_identity / on_get_routing_identity / on_get_routing_identity_async:
        VerifiedCredentialInput => RoutingIdentity, op Some(PullRequestOperation::RoutingIdentity), name "getRoutingIdentity",
        default |k| Err(unsupported(k, "getRoutingIdentity"));
}

impl FakeProvider {
    /// `fakeProvider(kind)`: everything a host could offer, so a fixture only narrows what its
    /// own test is about.
    pub fn new(kind_name: &str) -> Self {
        let mut provider = Self {
            kind: kind(kind_name),
            capabilities: default_capabilities(),
            rewrite: kind_name == "github",
            repository_key_is_canonical: kind_name == "azure-devops",
            get_viewer: None,
            list_change_requests: None,
            list_change_requests_across: None,
            list_change_request_stats: None,
            get_change_request_checks: None,
            get_change_request: None,
            get_change_request_preview: None,
            get_change_request_summary: None,
            get_change_request_watch_fingerprint: None,
            get_change_request_stack: None,
            get_change_request_activity: None,
            get_review_thread_comments: None,
            get_viewer_permissions: None,
            get_diff: None,
            get_diff_file_contents: None,
            get_files_viewed: None,
            set_files_viewed: None,
            get_file_revisions: None,
            run_action: None,
            update_change_request: None,
            comment: None,
            update_comment: None,
            submit_review: None,
            list_reviewer_candidates: None,
            set_reviewer_request: None,
            list_label_candidates: None,
            set_labels: None,
            reply_to_thread: None,
            set_reaction: None,
            set_thread_resolution: None,
            get_routing_identity: None,
        };
        // Present by default, as `fakeProvider` supplies them.
        provider = provider
            .on_update_change_request(|_| Ok(()))
            .on_update_comment(|_| Ok(()));
        provider
    }

    pub fn on_get_viewer(
        mut self,
        handler: impl Fn(String, Option<String>) -> ProviderResult<String> + Send + Sync + 'static,
    ) -> Self {
        self.get_viewer = Some(Arc::new(move |(cwd, host)| {
            let result = handler(cwd, host);
            async move { result }.boxed()
        }));
        self
    }

    pub fn on_get_viewer_async<Fut>(
        mut self,
        handler: impl Fn(String, Option<String>) -> Fut + Send + Sync + 'static,
    ) -> Self
    where
        Fut: Future<Output = ProviderResult<String>> + Send + 'static,
    {
        self.get_viewer = Some(Arc::new(move |(cwd, host)| handler(cwd, host).boxed()));
        self
    }

    /// Edits the capabilities `fakeProvider` starts from.
    pub fn capabilities_with(mut self, edit: impl FnOnce(&mut Value)) -> Self {
        let mut value = serde_json::to_value(&self.capabilities).unwrap();
        edit(&mut value);
        self.capabilities = serde_json::from_value(value).unwrap();
        self
    }

    /// A host that never claimed the member (`updateChangeRequest: undefined`).
    pub fn without_update_change_request(mut self) -> Self {
        self.update_change_request = None;
        self
    }

    pub fn without_update_comment(mut self) -> Self {
        self.update_comment = None;
        self
    }

    /// A host with no merge message rewrite (`mergeMessageRewrite: undefined`).
    pub fn without_merge_message_rewrite(mut self) -> Self {
        self.rewrite = false;
        self
    }

    pub fn into_arc(self) -> Arc<dyn PullRequestProviderApi> {
        Arc::new(self)
    }
}

pub fn default_capabilities() -> PullRequestCapabilities {
    serde_json::from_value(json!({
        "diff": true,
        "comment": true,
        "actions": ["merge", "ready", "draft", "close", "reopen"],
        "mergeMethods": ["merge"],
        "search": true,
        "reactions": true,
        "review": {
            "inlineComment": true,
            "reply": true,
            "resolve": true,
            "verdicts": ["comment", "approve", "request-changes"],
        },
        "reviewers": { "request": true, "listCandidates": true },
        "edit": { "changeRequest": true, "comment": true },
    }))
    .unwrap()
}

// ---------------------------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------------------------

struct Projects(Vec<OrchestrationProjectShell>);

#[async_trait]
impl PullRequestProjectSource for Projects {
    async fn list_shells(
        &self,
        project_ids: Option<&[ProjectId]>,
    ) -> Result<Vec<OrchestrationProjectShell>, Cause> {
        Ok(self
            .0
            .iter()
            .filter(|project| project_ids.is_none_or(|ids| ids.contains(&project.id)))
            .cloned()
            .collect())
    }

    async fn get_shell(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<OrchestrationProjectShell>, Cause> {
        Ok(self
            .0
            .iter()
            .find(|project| &project.id == project_id)
            .cloned())
    }
}

type ResolveHandle = Arc<
    dyn Fn(
            &str,
            &SourceControlProviderContext,
        ) -> Result<Option<SourceControlProviderContext>, SourceControlProviderError>
        + Send
        + Sync,
>;

struct Refinement(Option<ResolveHandle>);

#[async_trait]
impl ResolveProviderContext for Refinement {
    async fn resolve(
        &self,
        cwd: &str,
        context: &SourceControlProviderContext,
    ) -> Result<Option<SourceControlProviderContext>, SourceControlProviderError> {
        match &self.0 {
            Some(handler) => handler(cwd, context),
            None => panic!("Unexpected provider refinement"),
        }
    }
}

type ResolveIdentity = Arc<dyn Fn(&str) -> Option<RepositoryIdentity> + Send + Sync>;

struct Identities(Option<ResolveIdentity>);

#[async_trait]
impl RepositoryIdentityResolver for Identities {
    async fn resolve(&self, workspace_root: &str) -> Option<RepositoryIdentity> {
        self.0.as_ref().and_then(|resolve| resolve(workspace_root))
    }
}

struct Settings(HashMap<String, bool>);

#[async_trait]
impl PullRequestSettings for Settings {
    async fn remove_agent_credits_on_merge(&self, project_id: &ProjectId) -> Result<bool, Cause> {
        Ok(self.0.get(project_id.as_str()).copied().unwrap_or(false))
    }
}

// ---------------------------------------------------------------------------------------------
// makeService
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
pub struct ServiceOptions {
    pub projects: Vec<OrchestrationProjectShell>,
    pub providers: Vec<FakeProvider>,
    pub resolve_handle: Option<ResolveHandle>,
    pub resolve_repository_identity: Option<ResolveIdentity>,
    /// Projects whose `removeAgentCreditsOnMerge` setting is on.
    pub remove_agent_credits_on_merge: HashMap<String, bool>,
}

pub struct Harness {
    pub service: PullRequestService,
    pub clock: Arc<TestClock>,
    pub rate_limits: Arc<SourceControlRateLimit>,
}

impl Harness {
    /// `makeService`
    pub async fn new(options: ServiceOptions) -> Self {
        let clock = TestClock::new();
        let rate_limits = SourceControlRateLimit::new(clock.clone());
        let registry = PullRequestProviderRegistry::from_providers(
            options
                .providers
                .into_iter()
                .map(FakeProvider::into_arc)
                .collect(),
        );
        let service = PullRequestService::new(PullRequestServiceDependencies {
            registry,
            settings: Arc::new(Settings(options.remove_agent_credits_on_merge)),
            projects: Arc::new(Projects(options.projects)),
            repository_identities: Arc::new(Identities(options.resolve_repository_identity)),
            provider_context: Arc::new(Refinement(options.resolve_handle)),
            rate_limits: rate_limits.clone(),
            // The real store over a database of its own, so the environment-kept marks are
            // exercised through the SQL that holds them rather than through a stand-in that
            // agrees with itself.
            db: Db::open_in_memory().await.unwrap(),
            read_cache: Arc::new(PullRequestReadCache::memory(clock.clone())),
            clock: clock.clone(),
        });
        Self {
            service,
            clock,
            rate_limits,
        }
    }
}

/// `makeService({ projects, providers })`
pub async fn make_service(projects: Vec<ProjectSpec>, providers: Vec<FakeProvider>) -> Harness {
    Harness::new(ServiceOptions {
        projects: projects.iter().map(ProjectSpec::build).collect(),
        providers,
        ..ServiceOptions::default()
    })
    .await
}

// ---------------------------------------------------------------------------------------------
// Assertions
// ---------------------------------------------------------------------------------------------

/// `(operation, detail)` of a `PullRequestOperationError`.
pub fn operation_of(error: &PullRequestError) -> (String, String) {
    match error {
        PullRequestError::Operation(
            turin_contracts::pull_request::PullRequestOperationError::Operation {
                operation,
                detail,
                ..
            },
        ) => (operation.clone(), detail.as_str().to_owned()),
        other => panic!("expected an operation error, got {other:?}"),
    }
}

pub fn is_operation_error(error: &PullRequestError) -> bool {
    matches!(error, PullRequestError::Operation(_))
}

pub fn is_unavailable(error: &PullRequestError) -> bool {
    matches!(error, PullRequestError::Unavailable(_))
}

/// Keeps the unused re-exports honest.
#[allow(dead_code)]
pub(crate) fn _types(_: Mutex<()>) {}
