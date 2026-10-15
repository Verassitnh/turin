//! Port of upstream/packages/client-runtime/src/state/pullRequests.ts (pure parts).
//!
//! Kept: the detail-to-status projection, the command ordering keys, the quick-merge
//! preparation, the cache patches that follow confirmed label and reviewer edits, and the query
//! timing constants. Cut: the Effect atom families and RPC commands
//! (`createPullRequestEnvironmentAtoms`, `createLinkedPullRequestSummaryAtomFamily`,
//! `createPullRequestStackAtomFamily`), the writable-atom wrapper (`writableQueryFamily`,
//! `updateCached`), query persistence and the HTTP diff loader. A cache layer applies the
//! patches below to whatever it holds for the reference.

use std::future::Future;

use serde_json::json;
use turin_contracts::git::{GitPullRequestState, VcsStatusChangeRequest};
use turin_contracts::pull_request::{
    PullRequestAction, PullRequestActionInput, PullRequestActivity, PullRequestActor,
    PullRequestCommentKind, PullRequestDetail, PullRequestDiffFileContentsInput, PullRequestLabel,
    PullRequestLabelCandidateList, PullRequestMergeMethod, PullRequestOperationError,
    PullRequestRef, PullRequestReviewerCandidate, PullRequestReviewerCandidateList,
    PullRequestReviewerRequestReviewer, PullRequestSetFilesViewedInput, PullRequestStack,
    PullRequestState, PullRequestSummary, PullRequestThreadCommentsInput,
};
use turin_contracts::{EnvironmentId, TrimmedNonEmptyString};

/// How long a linked pull request's summary stays subscribed after its last reader leaves.
pub const LINKED_PULL_REQUEST_IDLE_TTL_MS: u64 = 5_000;
/// Reopening a PR within a minute reuses detail, activity, preview and the like.
pub const PULL_REQUEST_STALE_TIME_MS: u64 = 60_000;
/// Listing rows (`pullRequests.list`).
pub const PULL_REQUEST_LIST_STALE_TIME_MS: u64 = 30_000;
/// Checks (`pullRequests.checks`).
pub const PULL_REQUEST_CHECKS_STALE_TIME_MS: u64 = 45_000;
/// Viewed-file marks (`pullRequests.filesViewed`).
pub const PULL_REQUEST_FILES_VIEWED_STALE_TIME_MS: u64 = 15_000;
/// Linked threads are polled this often (`pullRequests.linkedThreads`, stale time 0).
pub const PULL_REQUEST_LINKED_THREADS_REFRESH_INTERVAL_MS: u64 = 10_000;
/// A linked pull request's summary is re-read this often.
pub const LINKED_PULL_REQUEST_REFRESH_INTERVAL_MS: u64 = 60_000;
/// Confirmed edits stay on the cached reference this long once nobody reads it.
pub const WRITABLE_QUERY_IDLE_TTL_MS: u64 = 5 * 60_000;

fn change_request_state(state: PullRequestState) -> GitPullRequestState {
    match state {
        PullRequestState::Open => GitPullRequestState::Open,
        PullRequestState::Closed => GitPullRequestState::Closed,
        PullRequestState::Merged => GitPullRequestState::Merged,
    }
}

/// `pullRequestDetailToVcsStatus` for a detail.
pub fn pull_request_detail_to_vcs_status(detail: &PullRequestDetail) -> VcsStatusChangeRequest {
    VcsStatusChangeRequest {
        number: detail.number,
        title: detail.title.clone(),
        url: detail.url.to_string(),
        base_ref: detail.base_branch.clone(),
        head_ref: detail.head_branch.clone(),
        state: change_request_state(detail.state),
        is_draft: detail.is_draft.then_some(true),
        updated_at: Some(Some(detail.updated_at.clone())),
    }
}

/// `pullRequestDetailToVcsStatus` for a summary.
pub fn pull_request_summary_to_vcs_status(summary: &PullRequestSummary) -> VcsStatusChangeRequest {
    VcsStatusChangeRequest {
        number: summary.number,
        title: summary.title.clone(),
        url: summary.url.to_string(),
        base_ref: summary.base_branch.clone(),
        head_ref: summary.head_branch.clone(),
        state: change_request_state(summary.state),
        is_draft: (summary.is_draft == Some(true)).then_some(true),
        updated_at: Some(Some(summary.updated_at.clone())),
    }
}

/// The reference a confirmed edit is cached under: the same reference whatever the property
/// order, and without the routing guard (`writableQueryFamily`).
pub fn writable_query_reference(reference: &PullRequestRef) -> PullRequestRef {
    PullRequestRef {
        project_id: reference.project_id.clone(),
        host: reference.host.clone(),
        expected_account_id: None,
        allow_stale: reference.allow_stale,
        repository: reference.repository.clone(),
        number: reference.number,
    }
}

// Command ordering. Writes run serially per key, reads of the same key share one flight.

/// Mutations run serially per environment: actions on the same pull request are order-sensitive.
pub fn serial_per_environment_key(environment_id: &EnvironmentId) -> String {
    environment_id.to_string()
}

/// `threadComments` single-flight key.
pub fn thread_comments_command_key(
    environment_id: &EnvironmentId,
    input: &PullRequestThreadCommentsInput,
) -> String {
    json!([
        environment_id.as_str(),
        input.thread_id.as_str(),
        input.cursor.as_str()
    ])
    .to_string()
}

/// `diffFileContents` single-flight key. Hosts are compared case-insensitively.
pub fn diff_file_contents_command_key(
    environment_id: &EnvironmentId,
    input: &PullRequestDiffFileContentsInput,
) -> String {
    json!([
        environment_id.as_str(),
        input.project_id.as_str(),
        input.host.as_ref().map(|host| host.as_str().to_lowercase()),
        input.repository.as_str(),
        input.number.get(),
        input.commit.as_ref().map(|commit| commit.as_str()),
        input.change_type,
        input.old_path.as_str(),
        input.new_path.as_str(),
    ])
    .to_string()
}

/// One write in flight per change request: the host applies these in order, and a reader
/// ticking down a file list faster than the round trip would otherwise race their own presses.
pub fn set_files_viewed_command_key(
    environment_id: &EnvironmentId,
    input: &PullRequestSetFilesViewedInput,
) -> String {
    json!([
        environment_id.as_str(),
        input.project_id.as_str(),
        input.repository.as_str(),
        input.number.get(),
    ])
    .to_string()
}

fn operation_error(detail: &str) -> PullRequestOperationError {
    PullRequestOperationError::Operation {
        operation: "runAction".to_owned(),
        detail: TrimmedNonEmptyString::new(detail).expect("fixed text is not blank"),
        reason: None,
        cause: None,
    }
}

/// Chooses the merge method from the freshly read detail. An `Err` carries its message.
pub type ResolveMergeMethod<'a> =
    &'a dyn Fn(&PullRequestDetail) -> Result<PullRequestMergeMethod, String>;

/// `runAction` input: a pull request action plus the optional `resolveMergeMethod` callback.
pub struct RunPullRequestActionInput<'a> {
    pub action: PullRequestActionInput,
    pub resolve_merge_method: Option<ResolveMergeMethod<'a>>,
}

/// Prepares a quick merge inside the write's own lane: re-reads the pull request without
/// stale data and refuses unless it can still be merged on its own, then picks the method.
/// Any other action passes through untouched.
///
/// Preparation belongs to the write's lane. Refreshable queries would restart it after
/// every preceding action, and preparing outside the lane could reorder the clicks.
pub async fn prepare_pull_request_action<E, Detail, Stack, DetailFut, StackFut>(
    input: RunPullRequestActionInput<'_>,
    read_detail: Detail,
    read_stack: Stack,
) -> Result<PullRequestActionInput, E>
where
    E: From<PullRequestOperationError>,
    Detail: FnOnce(PullRequestRef) -> DetailFut,
    DetailFut: Future<Output = Result<PullRequestDetail, E>>,
    Stack: FnOnce(PullRequestRef) -> StackFut,
    StackFut: Future<Output = Result<Option<PullRequestStack>, E>>,
{
    let RunPullRequestActionInput {
        mut action,
        resolve_merge_method,
    } = input;
    let Some(resolve_merge_method) = resolve_merge_method else {
        return Ok(action);
    };
    if action.action != PullRequestAction::Merge {
        return Ok(action);
    }
    let reference = PullRequestRef {
        project_id: action.project_id.clone(),
        host: action.host.clone(),
        expected_account_id: None,
        allow_stale: Some(false),
        repository: action.repository.clone(),
        number: action.number,
    };
    let detail = read_detail(reference.clone()).await?;
    if detail.state != PullRequestState::Open
        || detail.is_draft
        || !detail
            .capabilities
            .actions
            .contains(&PullRequestAction::Merge)
        || !detail
            .viewer_permissions
            .actions
            .contains(&PullRequestAction::Merge)
    {
        return Err(operation_error("This pull request cannot be merged.").into());
    }
    if detail.capabilities.stack_actions == Some(true) && read_stack(reference).await?.is_some() {
        return Err(operation_error("Open this pull request to merge its stack.").into());
    }
    let merge_method = resolve_merge_method(&detail).map_err(|message| {
        operation_error(if message.trim().is_empty() {
            "Could not choose a merge method."
        } else {
            &message
        })
    })?;
    action.merge_method = Some(merge_method);
    Ok(action)
}

fn same_login(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

/// `setLabels` success: the label menu and the detail follow the confirmed edit without
/// rereading the host.
pub fn apply_set_labels(
    candidates: Option<&mut PullRequestLabelCandidateList>,
    detail: Option<&mut PullRequestDetail>,
    labels: &[TrimmedNonEmptyString],
    applied: bool,
) {
    // Colors come from the menu as it stood before the edit.
    let colors: Vec<(TrimmedNonEmptyString, Option<String>)> = candidates
        .as_deref()
        .map(|list| {
            list.candidates
                .iter()
                .map(|candidate| (candidate.name.clone(), candidate.color.clone()))
                .collect()
        })
        .unwrap_or_default();
    if let Some(list) = candidates {
        for candidate in &mut list.candidates {
            if labels.contains(&candidate.name) {
                candidate.is_applied = applied;
            }
        }
    }
    let Some(detail) = detail else {
        return;
    };
    if applied {
        let added: Vec<PullRequestLabel> = labels
            .iter()
            .filter(|name| !detail.labels.iter().any(|label| label.name == **name))
            .map(|name| PullRequestLabel {
                name: name.clone(),
                color: colors
                    .iter()
                    .find(|(candidate, _)| candidate == name)
                    .and_then(|(_, color)| color.clone()),
            })
            .collect();
        detail.labels.extend(added);
    } else {
        detail.labels.retain(|label| !labels.contains(&label.name));
    }
}

/// `requestReviewers` success, planned once from the reviewer menu as it stood and then applied
/// to the menu, the detail and the activity.
#[derive(Debug, Clone)]
pub struct RequestReviewersPatch {
    requested: bool,
    reviewers: Vec<PullRequestReviewerRequestReviewer>,
    selected: Vec<PullRequestReviewerCandidate>,
    /// The menu did not know every person named, so their identities are not all here. The
    /// cached detail and activity are authoritative only after a re-read; upstream refreshes them.
    pub missing_identities: bool,
}

fn names_candidate(
    reviewers: &[PullRequestReviewerRequestReviewer],
    candidate: &PullRequestReviewerCandidate,
) -> bool {
    reviewers
        .iter()
        .any(|reviewer| reviewer.id == candidate.id && reviewer.kind == candidate.kind)
}

pub fn plan_request_reviewers(
    candidates: Option<&PullRequestReviewerCandidateList>,
    reviewers: &[PullRequestReviewerRequestReviewer],
    requested: bool,
) -> RequestReviewersPatch {
    let selected: Vec<PullRequestReviewerCandidate> = candidates
        .map(|list| {
            list.candidates
                .iter()
                .filter(|candidate| names_candidate(reviewers, candidate))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    RequestReviewersPatch {
        requested,
        reviewers: reviewers.to_vec(),
        missing_identities: selected.len() < reviewers.len(),
        selected,
    }
}

impl RequestReviewersPatch {
    /// The reviewer menu marks the people just asked, or no longer asked.
    pub fn apply_to_candidates(&self, list: &mut PullRequestReviewerCandidateList) {
        for candidate in &mut list.candidates {
            if names_candidate(&self.reviewers, candidate) {
                candidate.is_requested = self.requested;
            }
        }
    }

    /// `updateReviewers`: adds the selected people (by login, ignoring case), or takes them
    /// off except for those `keep` says must stay.
    pub fn update_reviewers(
        &self,
        actors: &[PullRequestActor],
        keep: impl Fn(&PullRequestActor) -> bool,
    ) -> Vec<PullRequestActor> {
        if self.requested {
            let mut next = actors.to_vec();
            next.extend(
                self.selected
                    .iter()
                    .filter(|candidate| {
                        !actors
                            .iter()
                            .any(|actor| same_login(actor.login.as_str(), candidate.login.as_str()))
                    })
                    .map(|candidate| PullRequestActor {
                        is_bot: None,
                        login: candidate.login.clone(),
                        name: candidate.name.clone(),
                        avatar_url: candidate.avatar_url.clone(),
                    }),
            );
            next
        } else {
            actors
                .iter()
                .filter(|actor| {
                    !self
                        .selected
                        .iter()
                        .any(|candidate| same_login(candidate.login.as_str(), actor.login.as_str()))
                        || keep(actor)
                })
                .cloned()
                .collect()
        }
    }

    pub fn apply_to_detail(&self, detail: &mut PullRequestDetail) {
        detail.reviewers = self.update_reviewers(&detail.reviewers, |_| false);
    }

    /// Somebody who has already reviewed stays listed when the request is taken back.
    pub fn apply_to_activity(&self, activity: &mut PullRequestActivity) {
        let Some(reviewers) = &activity.reviewers else {
            return;
        };
        let reviewed = |actor: &PullRequestActor| {
            activity.comments.iter().any(|comment| {
                matches!(
                    comment.kind,
                    PullRequestCommentKind::Review | PullRequestCommentKind::ReviewComment
                ) && comment
                    .author
                    .as_ref()
                    .is_some_and(|author| same_login(author.login.as_str(), actor.login.as_str()))
            })
        };
        let next = self.update_reviewers(reviewers, reviewed);
        activity.reviewers = Some(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use turin_contracts::pull_request::{PullRequestDiffFileChangeType, PullRequestReviewerKind};

    fn text(value: &str) -> TrimmedNonEmptyString {
        TrimmedNonEmptyString::new(value).unwrap()
    }

    fn detail_json() -> Value {
        json!({
            "provider": "github",
            "capabilities": {
                "diff": true,
                "comment": true,
                "actions": ["merge", "close"],
                "mergeMethods": ["merge", "squash", "rebase"],
                "search": true,
                "review": { "inlineComment": true, "reply": true, "resolve": true, "verdicts": [] },
                "reviewers": { "request": true, "listCandidates": true },
                "stackActions": true,
            },
            "viewerPermissions": {
                "actions": ["merge", "close"],
                "comment": true,
                "resolve": true,
                "verdicts": [],
                "requestReviewers": true,
            },
            "projectId": "project-1",
            "projectTitle": "Project",
            "workspaceRoot": "/workspace",
            "repository": "acme/web",
            "number": 1,
            "title": "keep this title",
            "body": "",
            "url": "https://github.com/acme/web/pull/1",
            "author": null,
            "state": "open",
            "isDraft": false,
            "mergeability": "mergeable",
            "additions": 1,
            "deletions": 2,
            "changedFiles": 3,
            "headBranch": "feature",
            "baseBranch": "main",
            "createdAt": "2026-09-01T00:00:00.000Z",
            "updatedAt": "2026-09-02T00:00:00.000Z",
            "mergedAt": null,
            "closedAt": null,
            "reviewers": [],
            "labels": [],
            "checks": [],
            "mergeCapabilities": { "merge": true, "squash": true, "rebase": false },
        })
    }

    fn detail(patch: impl FnOnce(&mut Value)) -> PullRequestDetail {
        let mut value = detail_json();
        patch(&mut value);
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn projects_a_detail_onto_the_status_change_request() {
        let status = pull_request_detail_to_vcs_status(&detail(|_| {}));
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            json!({
                "number": 1,
                "title": "keep this title",
                "url": "https://github.com/acme/web/pull/1",
                "baseRef": "main",
                "headRef": "feature",
                "state": "open",
                "updatedAt": "2026-09-02T00:00:00.000Z",
            })
        );
        let draft = pull_request_detail_to_vcs_status(&detail(|value| {
            value["isDraft"] = json!(true);
            value["state"] = json!("merged");
        }));
        assert_eq!(draft.is_draft, Some(true));
        assert_eq!(draft.state, GitPullRequestState::Merged);
    }

    #[test]
    fn projects_a_summary_and_only_flags_drafts() {
        let summary: PullRequestSummary = serde_json::from_value(json!({
            "provider": "github",
            "projectId": "project-1",
            "repository": "acme/web",
            "number": 4,
            "title": "Summary",
            "url": "https://github.com/acme/web/pull/4",
            "state": "closed",
            "isDraft": false,
            "headBranch": "topic",
            "baseBranch": "main",
            "updatedAt": "2026-09-03T00:00:00.000Z",
        }))
        .unwrap();
        let status = pull_request_summary_to_vcs_status(&summary);
        assert_eq!(status.number.get(), 4);
        assert_eq!(status.state, GitPullRequestState::Closed);
        assert_eq!(status.is_draft, None);
        assert_eq!(status.head_ref.as_str(), "topic");
        assert_eq!(
            status.updated_at,
            Some(Some("2026-09-03T00:00:00.000Z".to_owned()))
        );
    }

    #[test]
    fn writable_references_drop_the_routing_guard() {
        let reference: PullRequestRef = serde_json::from_value(json!({
            "number": 7,
            "expectedAccountId": "123",
            "repository": "acme/web",
            "host": "github.com",
            "allowStale": false,
            "projectId": "project-1",
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(writable_query_reference(&reference)).unwrap(),
            json!({
                "projectId": "project-1",
                "host": "github.com",
                "allowStale": false,
                "repository": "acme/web",
                "number": 7,
            })
        );
    }

    #[test]
    fn keeps_concurrent_diff_file_reads_on_different_hosts_separate() {
        let input = |host: Option<&str>| -> PullRequestDiffFileContentsInput {
            let mut value = json!({
                "projectId": "project-1",
                "repository": "acme/web",
                "number": 1,
                "changeType": "change",
                "oldPath": "src/app.ts",
                "newPath": "src/app.ts",
            });
            if let Some(host) = host {
                value["host"] = json!(host);
            }
            serde_json::from_value(value).unwrap()
        };
        let environment = EnvironmentId::new("environment-1").unwrap();
        let key = |host| diff_file_contents_command_key(&environment, &input(host));
        assert_ne!(key(Some("github.com")), key(Some("github.example.com")));
        assert_ne!(key(Some("github.com")), key(None));
        assert_eq!(key(Some("GitHub.com")), key(Some("github.com")));
        assert_eq!(
            key(Some("GitHub.com")),
            r#"["environment-1","project-1","github.com","acme/web",1,null,"change","src/app.ts","src/app.ts"]"#
        );
        let mut renamed = input(None);
        renamed.change_type = PullRequestDiffFileChangeType::RenamePure;
        assert_ne!(
            diff_file_contents_command_key(&environment, &renamed),
            key(None)
        );
    }

    #[test]
    fn orders_writes_by_environment_and_by_change_request() {
        let environment = EnvironmentId::new("environment-1").unwrap();
        assert_eq!(serial_per_environment_key(&environment), "environment-1");
        let comments: PullRequestThreadCommentsInput = serde_json::from_value(json!({
            "projectId": "project-1",
            "repository": "acme/web",
            "number": 2,
            "threadId": "thread-1",
            "cursor": "c1",
        }))
        .unwrap();
        assert_eq!(
            thread_comments_command_key(&environment, &comments),
            r#"["environment-1","thread-1","c1"]"#
        );
        let viewed: PullRequestSetFilesViewedInput = serde_json::from_value(json!({
            "projectId": "project-1",
            "repository": "acme/web",
            "number": 2,
            "files": [],
        }))
        .unwrap();
        assert_eq!(
            set_files_viewed_command_key(&environment, &viewed),
            r#"["environment-1","project-1","acme/web",2]"#
        );
    }

    // Quick merge preparation.

    fn action_input(action: &str, number: u64) -> PullRequestActionInput {
        serde_json::from_value(json!({
            "projectId": "project-1",
            "repository": "acme/web",
            "number": number,
            "action": action,
        }))
        .unwrap()
    }

    fn stack() -> PullRequestStack {
        serde_json::from_value(json!({
            "kind": "native",
            "id": "stack-1",
            "number": 1,
            "url": "https://github.com/acme/web/stack/1",
            "base": "main",
            "layers": [],
        }))
        .unwrap()
    }

    #[derive(Debug, PartialEq)]
    enum Failure {
        Operation(String),
        Read,
    }

    impl From<PullRequestOperationError> for Failure {
        fn from(error: PullRequestOperationError) -> Self {
            let PullRequestOperationError::Operation { detail, .. } = error;
            Failure::Operation(detail.to_string())
        }
    }

    struct Calls(parking_lot::Mutex<Vec<String>>);

    async fn prepare(
        reason: &str,
        resolve: Option<ResolveMergeMethod<'_>>,
        calls: &Calls,
    ) -> Result<PullRequestActionInput, Failure> {
        let action = if resolve.is_some() { "merge" } else { "close" };
        prepare_pull_request_action(
            RunPullRequestActionInput {
                action: action_input(action, 1),
                resolve_merge_method: resolve,
            },
            |reference| {
                calls.0.lock().push(format!(
                    "detail:{}:stale={:?}",
                    reference.number.get(),
                    reference.allow_stale
                ));
                let found = detail(|value| {
                    if reason == "closed" {
                        value["state"] = json!("closed");
                    }
                    value["isDraft"] = json!(reason == "draft");
                    if reason == "permission" {
                        value["viewerPermissions"]["actions"] = json!([]);
                    }
                });
                async move { Ok(found) }
            },
            |reference| {
                calls
                    .0
                    .lock()
                    .push(format!("stack:{}", reference.number.get()));
                let found = (reason == "stack").then(stack);
                async move { Ok(found) }
            },
        )
        .await
    }

    #[tokio::test]
    async fn prepares_a_quick_merge_with_fresh_reads_and_the_resolved_method() {
        let calls = Calls(Default::default());
        let squash = |_: &PullRequestDetail| Ok(PullRequestMergeMethod::Squash);
        let prepared = prepare("ok", Some(&squash), &calls).await.unwrap();
        assert_eq!(prepared.merge_method, Some(PullRequestMergeMethod::Squash));
        assert_eq!(prepared.action, PullRequestAction::Merge);
        assert_eq!(
            *calls.0.lock(),
            vec!["detail:1:stale=Some(false)", "stack:1"]
        );
    }

    #[tokio::test]
    async fn rejects_an_unsafe_quick_merge() {
        for (reason, expected) in [
            ("closed", "This pull request cannot be merged."),
            ("draft", "This pull request cannot be merged."),
            ("permission", "This pull request cannot be merged."),
            ("stack", "Open this pull request to merge its stack."),
            ("method", "No merge method is available."),
        ] {
            let calls = Calls(Default::default());
            let resolve = |_: &PullRequestDetail| {
                if reason == "method" {
                    Err("No merge method is available.".to_owned())
                } else {
                    Ok(PullRequestMergeMethod::Squash)
                }
            };
            let result = prepare(reason, Some(&resolve), &calls).await;
            assert_eq!(
                result,
                Err(Failure::Operation(expected.to_owned())),
                "{reason}"
            );
        }
    }

    #[tokio::test]
    async fn an_unexplained_method_failure_gets_a_generic_message() {
        let calls = Calls(Default::default());
        let resolve = |_: &PullRequestDetail| Err("  ".to_owned());
        assert_eq!(
            prepare("ok", Some(&resolve), &calls).await,
            Err(Failure::Operation(
                "Could not choose a merge method.".into()
            ))
        );
    }

    #[tokio::test]
    async fn other_actions_pass_through_without_reading_the_pull_request() {
        let calls = Calls(Default::default());
        let prepared = prepare("ok", None, &calls).await.unwrap();
        assert_eq!(prepared.action, PullRequestAction::Close);
        assert!(calls.0.lock().is_empty());
        // A merge without a resolver is sent as it came.
        let merge = prepare_pull_request_action(
            RunPullRequestActionInput {
                action: action_input("merge", 3),
                resolve_merge_method: None,
            },
            |_| async { Err::<PullRequestDetail, _>(Failure::Read) },
            |_| async { Err::<Option<PullRequestStack>, _>(Failure::Read) },
        )
        .await
        .unwrap();
        assert_eq!(merge.merge_method, None);
    }

    #[tokio::test]
    async fn a_failed_read_stops_the_merge() {
        let result = prepare_pull_request_action(
            RunPullRequestActionInput {
                action: action_input("merge", 3),
                resolve_merge_method: Some(&|_| Ok(PullRequestMergeMethod::Merge)),
            },
            |_| async { Err::<PullRequestDetail, _>(Failure::Read) },
            |_| async { Ok::<_, Failure>(None) },
        )
        .await;
        assert_eq!(result, Err(Failure::Read));
    }

    // Cache patches.

    fn labels(names: &[&str]) -> Vec<TrimmedNonEmptyString> {
        names.iter().map(|name| text(name)).collect()
    }

    fn label_candidates() -> PullRequestLabelCandidateList {
        serde_json::from_value(json!({
            "candidates": [
                { "name": "existing", "color": "111111", "description": null, "isApplied": true },
                { "name": "new", "color": "abcdef", "description": "description", "isApplied": false },
            ],
            "truncated": false,
        }))
        .unwrap()
    }

    fn label_names(detail: &PullRequestDetail) -> Vec<(&str, Option<&str>)> {
        detail
            .labels
            .iter()
            .map(|label| (label.name.as_str(), label.color.as_deref()))
            .collect()
    }

    #[test]
    fn updates_cached_labels_after_successful_edits() {
        let mut current = detail(|value| {
            value["labels"] = json!([{ "name": "existing", "color": "111111" }]);
        });
        let mut candidates = label_candidates();

        apply_set_labels(
            Some(&mut candidates),
            Some(&mut current),
            &labels(&["new"]),
            true,
        );
        assert_eq!(
            label_names(&current),
            vec![("existing", Some("111111")), ("new", Some("abcdef"))]
        );
        assert_eq!(current.title.as_str(), "keep this title");
        assert!(candidates.candidates[1].is_applied);
        assert_eq!(
            candidates.candidates[1].description.as_deref(),
            Some("description")
        );

        apply_set_labels(
            Some(&mut candidates),
            Some(&mut current),
            &labels(&["existing"]),
            false,
        );
        assert_eq!(label_names(&current), vec![("new", Some("abcdef"))]);
        assert!(!candidates.candidates[0].is_applied);
        assert!(candidates.candidates[1].is_applied);

        // Applying a label that is already on the pull request does not repeat it.
        apply_set_labels(None, Some(&mut current), &labels(&["new"]), true);
        assert_eq!(label_names(&current), vec![("new", Some("abcdef"))]);
    }

    #[test]
    fn a_label_unknown_to_the_menu_has_no_color() {
        let mut current = detail(|_| {});
        apply_set_labels(None, Some(&mut current), &labels(&["fresh"]), true);
        assert_eq!(label_names(&current), vec![("fresh", None)]);
        // Without a cached detail only the menu changes.
        let mut candidates = label_candidates();
        apply_set_labels(Some(&mut candidates), None, &labels(&["new"]), true);
        assert!(candidates.candidates[1].is_applied);
    }

    fn actor() -> Value {
        json!({ "login": "reviewer", "name": "Reviewer", "avatarUrl": null })
    }

    fn host_actor() -> Value {
        json!({ "login": "Reviewer", "name": "Reviewer", "avatarUrl": null })
    }

    fn reviewer_candidates() -> PullRequestReviewerCandidateList {
        let mut candidate = actor();
        candidate["id"] = json!("12");
        candidate["kind"] = json!("user");
        candidate["isRequested"] = json!(false);
        serde_json::from_value(json!({ "candidates": [candidate], "truncated": false })).unwrap()
    }

    fn activity(reviewers: Option<Value>, comments: Value) -> PullRequestActivity {
        let mut value = json!({
            "comments": comments,
            "commentCount": 0,
            "commentsTruncated": false,
            "reviewThreads": [],
            "commits": [],
        });
        if let Some(reviewers) = reviewers {
            value["reviewers"] = reviewers;
        }
        serde_json::from_value(value).unwrap()
    }

    fn reviewer_request(id: &str) -> Vec<PullRequestReviewerRequestReviewer> {
        vec![PullRequestReviewerRequestReviewer {
            id: text(id),
            kind: PullRequestReviewerKind::User,
        }]
    }

    fn logins(actors: &[PullRequestActor]) -> Vec<&str> {
        actors.iter().map(|actor| actor.login.as_str()).collect()
    }

    #[test]
    fn updates_reviewer_requests_and_enriched_reviewers() {
        let mut candidates = reviewer_candidates();
        let mut current = detail(|_| {});
        let mut feed = activity(Some(json!([])), json!([]));

        for (requested, expected) in [(true, vec!["reviewer"]), (false, vec![])] {
            let patch =
                plan_request_reviewers(Some(&candidates), &reviewer_request("12"), requested);
            assert!(!patch.missing_identities);
            patch.apply_to_candidates(&mut candidates);
            patch.apply_to_detail(&mut current);
            patch.apply_to_activity(&mut feed);
            assert_eq!(logins(&current.reviewers), expected);
            assert_eq!(logins(feed.reviewers.as_ref().unwrap()), expected);
            assert_eq!(candidates.candidates[0].is_requested, requested);
        }
        // The added person carries the actor fields the menu showed.
        let patch = plan_request_reviewers(Some(&candidates), &reviewer_request("12"), true);
        patch.apply_to_detail(&mut current);
        assert_eq!(current.reviewers[0].name.as_deref(), Some("Reviewer"));
        assert_eq!(current.reviewers[0].is_bot, None);
    }

    #[test]
    fn host_spelling_of_a_login_still_matches_and_a_review_keeps_the_reviewer() {
        let candidates = reviewer_candidates();
        let patch = plan_request_reviewers(Some(&candidates), &reviewer_request("12"), false);

        // The host reports "Reviewer"; the menu says "reviewer".
        let mut current = detail(|value| value["reviewers"] = json!([host_actor()]));
        patch.apply_to_detail(&mut current);
        assert!(current.reviewers.is_empty());

        let review = json!({
            "id": "c1",
            "kind": "review-comment",
            "author": host_actor(),
            "body": "",
            "createdAt": "2026-09-02T00:00:00.000Z",
            "url": null,
            "path": null,
            "reviewState": null,
        });
        let mut reviewed = activity(Some(json!([host_actor()])), json!([review.clone()]));
        patch.apply_to_activity(&mut reviewed);
        assert_eq!(
            logins(reviewed.reviewers.as_ref().unwrap()),
            vec!["Reviewer"]
        );

        // An issue comment is not a review.
        let mut issue = review;
        issue["kind"] = json!("issue-comment");
        let mut chatted = activity(Some(json!([host_actor()])), json!([issue]));
        patch.apply_to_activity(&mut chatted);
        assert!(chatted.reviewers.unwrap().is_empty());

        // An activity feed without reviewers stays without them.
        let mut absent = activity(None, json!([]));
        patch.apply_to_activity(&mut absent);
        assert_eq!(absent.reviewers, None);

        // Adding somebody the host already lists, in another spelling, does not repeat them.
        let add = plan_request_reviewers(Some(&candidates), &reviewer_request("12"), true);
        let mut listed = detail(|value| value["reviewers"] = json!([host_actor()]));
        add.apply_to_detail(&mut listed);
        assert_eq!(logins(&listed.reviewers), vec!["Reviewer"]);
    }

    #[test]
    fn a_caller_without_an_open_picker_needs_authoritative_reviewer_identities() {
        let patch = plan_request_reviewers(None, &reviewer_request("12"), true);
        assert!(patch.missing_identities);
        let mut current = detail(|_| {});
        patch.apply_to_detail(&mut current);
        assert!(current.reviewers.is_empty());

        // A person the menu does not list is missing even when others are known.
        let candidates = reviewer_candidates();
        let partial = plan_request_reviewers(
            Some(&candidates),
            &[reviewer_request("12"), reviewer_request("99")].concat(),
            true,
        );
        assert!(partial.missing_identities);
        // A team is not a user with the same id.
        let team = vec![PullRequestReviewerRequestReviewer {
            id: text("12"),
            kind: PullRequestReviewerKind::Team,
        }];
        assert!(plan_request_reviewers(Some(&candidates), &team, true).missing_identities);
    }
}
