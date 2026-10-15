#![allow(unused_imports)]
use super::super::*;

use turin_contracts::environment::RepositoryIdentity;
use turin_contracts::pull_request::{
    PullRequestActor, PullRequestCheck, PullRequestCheckStatus, PullRequestComment,
    PullRequestCommentKind, PullRequestCommit, PullRequestDiffSide, PullRequestRef,
    PullRequestReviewThread, PullRequestThreadComment,
};
use turin_contracts::source_control::SourceControlProviderKind;
use turin_contracts::{PositiveInt, ProjectId, TrimmedNonEmptyString};

pub(super) fn tne(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).expect("fixture ids are non-empty")
}

pub(super) fn kind(value: &str) -> SourceControlProviderKind {
    SourceControlProviderKind::new(value).expect("fixture provider kinds are valid")
}

pub(super) fn actor(login: &str) -> PullRequestActor {
    PullRequestActor {
        is_bot: None,
        login: tne(login),
        name: None,
        avatar_url: None,
    }
}

pub(super) fn check(name: &str, status: PullRequestCheckStatus) -> PullRequestCheck {
    PullRequestCheck {
        name: tne(name),
        status,
        description: None,
        url: None,
        required: None,
    }
}

/// The failing typecheck run the handoff tests quote.
pub(super) fn failing_check() -> PullRequestCheck {
    PullRequestCheck {
        description: Some("2 errors".into()),
        ..check("typecheck", PullRequestCheckStatus::Failure)
    }
}

pub(super) fn reference(host: Option<&str>) -> PullRequestRef {
    PullRequestRef {
        project_id: ProjectId::new("project-1").expect("fixture project id is valid"),
        host: host.map(tne),
        expected_account_id: None,
        allow_stale: None,
        repository: tne("acme/web"),
        number: PositiveInt::new(42).expect("42 is positive"),
    }
}

/// A repository identity built from its wire shape, the way the server sends it.
pub(super) fn identity(provider: &str, canonical_key: &str) -> RepositoryIdentity {
    serde_json::from_value(serde_json::json!({
        "canonicalKey": canonical_key,
        "locator": {
            "source": "git-remote",
            "remoteName": "origin",
            "remoteUrl": "git@github.com:acme/web.git",
        },
        "provider": provider,
    }))
    .expect("fixture identity is valid")
}

pub(super) fn commit(oid: &str, headline: &str, committed_date: &str) -> PullRequestCommit {
    PullRequestCommit {
        oid: tne(oid),
        message_headline: headline.into(),
        committed_date: committed_date.into(),
        additions: None,
        deletions: None,
        authors: None,
    }
}

/// The one conversation remark the timeline fixtures start from. Tests override the fields they
/// vary with struct update syntax.
pub(super) fn base_comment() -> PullRequestComment {
    PullRequestComment {
        id: tne("c1"),
        kind: PullRequestCommentKind::IssueComment,
        author: Some(actor("bilal")),
        body: "looks good".into(),
        created_at: "2026-07-03T00:00:00Z".into(),
        edited_at: None,
        url: None,
        path: None,
        review_state: None,
        reactions: None,
        can_edit: None,
    }
}

/// A review submission with no line, which is how a review with only words arrives.
pub(super) fn review_comment(
    id: &str,
    login: Option<&str>,
    review_state: &str,
    created_at: &str,
) -> PullRequestComment {
    PullRequestComment {
        id: tne(id),
        kind: PullRequestCommentKind::Review,
        author: login.map(actor),
        body: String::new(),
        created_at: created_at.into(),
        review_state: Some(review_state.into()),
        ..base_comment()
    }
}

/// The pull request the timeline fixtures describe: opened, one commit, one remark.
pub(super) fn timeline_source() -> PullRequestTimelineSource {
    PullRequestTimelineSource {
        created_at: "2026-07-01T00:00:00Z".into(),
        author: Some(actor("octocat")),
        commits: vec![commit(
            "1baf7bdcafe",
            "add the page",
            "2026-07-02T00:00:00Z",
        )],
        comments: vec![base_comment()],
        merged_at: None,
        closed_at: None,
    }
}

/// A plain object with an id and a body, the shape the activity merge and edit read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TestComment {
    pub id: String,
    pub body: String,
}

impl ThreadComment for TestComment {
    fn id(&self) -> &str {
        &self.id
    }
    fn set_body(&mut self, body: &str) {
        self.body = body.to_owned();
    }
}

pub(super) fn test_comment(id: &str, body: &str) -> TestComment {
    TestComment {
        id: id.into(),
        body: body.into(),
    }
}

/// An unresolved, right-side review thread on line 12 with one remark from `reviewer`.
pub(super) fn review_thread(body: &str) -> PullRequestReviewThread {
    PullRequestReviewThread {
        id: tne("t1"),
        path: tne("apps/web/src/page.tsx"),
        line: PositiveInt::new(12),
        side: PullRequestDiffSide::Right,
        is_resolved: false,
        is_outdated: false,
        comments: vec![PullRequestThreadComment {
            id: tne("tc1"),
            author: Some(actor("reviewer")),
            body: body.into(),
            created_at: "2026-07-03T00:00:00Z".into(),
            edited_at: None,
            url: None,
            reactions: None,
            can_edit: None,
        }],
        comment_count: None,
        next_comments_cursor: None,
        can_resolve: None,
    }
}

/// The findings handoff input every fix-findings test starts from.
pub(super) fn base_findings_input() -> FixFindingsInput {
    FixFindingsInput {
        number: 42,
        title: "Add the pull requests page".into(),
        url: "https://github.com/pingdotgg/t3code/pull/42".into(),
        head_branch: "feat/page".into(),
        base_branch: "main".into(),
        review_threads: vec![],
        comments: vec![],
        checks: vec![],
        comments_truncated: false,
    }
}

/// The single-finding handoff input every one-finding test starts from.
pub(super) fn base_finding_input(finding: PullRequestFinding) -> FixFindingInput {
    FixFindingInput {
        number: 42,
        title: "Add the pull requests page".into(),
        url: "https://github.com/pingdotgg/t3code/pull/42".into(),
        head_branch: "feat/page".into(),
        base_branch: "main".into(),
        finding,
    }
}

/// The remark a review left with words only, which has nowhere to hang.
pub(super) fn review_remark() -> PullRequestComment {
    PullRequestComment {
        id: tne("r1"),
        kind: PullRequestCommentKind::Review,
        author: Some(actor("julius")),
        body: "This breaks SSO auth, revert the middleware change.".into(),
        created_at: "2026-07-01T00:00:00Z".into(),
        url: None,
        path: None,
        review_state: Some("CHANGES_REQUESTED".into()),
        ..base_comment()
    }
}

pub(super) fn strs(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

/// Each row as its kind followed by the ids it holds, for asserting the grouping.
pub(super) fn timeline_rows(events: &[PullRequestTimelineEvent]) -> Vec<Vec<String>> {
    group_pull_request_timeline_conversations(events)
        .iter()
        .map(|row| match row {
            PullRequestTimelineRow::Comments(events) => std::iter::once("comments".to_owned())
                .chain(events.iter().map(|event| event.id.clone()))
                .collect(),
            PullRequestTimelineRow::Event(event) => {
                vec!["event".to_owned(), event.id.clone()]
            }
        })
        .collect()
}

pub(super) fn names(checks: &[PullRequestCheck]) -> Vec<String> {
    checks
        .iter()
        .map(|check| check.name.as_str().to_owned())
        .collect()
}

pub(super) fn event_ids(events: &[PullRequestTimelineEvent]) -> Vec<String> {
    events.iter().map(|event| event.id.clone()).collect()
}

use serde_json::{Value, json};
use turin_contracts::pull_request::PullRequestMergeCapabilities;
use turin_contracts::thread_pull_request::{ThreadLinkedPullRequest, ThreadPullRequestLink};

/// Builds a contract value from its wire JSON. Upstream tests cast partial objects to the full
/// type; the Rust fixtures are whole values, so every field the contract requires is present.
pub(super) fn wire<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("fixture matches the contract")
}

/// A detail as the host reports it, open and mergeable with both update methods on offer.
/// Overrides merge at the top level only, the same as a JS object spread: a key given replaces
/// the whole value under it.
pub(super) fn pull_request_detail(overrides: Value) -> PullRequestDetail {
    let mut base = json!({
        "provider": "github",
        "capabilities": {
            "diff": true,
            "comment": true,
            "actions": ["merge"],
            "mergeMethods": ["merge"],
            "search": true,
            "review": {
                "inlineComment": true,
                "reply": true,
                "resolve": true,
                "verdicts": ["comment", "approve", "request-changes"]
            },
            "reviewers": { "request": true, "listCandidates": true }
        },
        "viewerPermissions": {
            "actions": ["merge"],
            "comment": true,
            "resolve": true,
            "verdicts": ["comment", "approve", "request-changes"],
            "requestReviewers": true
        },
        "projectId": "project-1",
        "projectTitle": "web",
        "workspaceRoot": "/repo",
        "repository": "acme/web",
        "number": 7,
        "title": "Cache the title",
        "body": "who made it",
        "url": "https://github.com/acme/web/pull/7",
        "author": { "login": "octocat", "name": null, "avatarUrl": "https://avatars.example/octocat" },
        "state": "open",
        "isDraft": false,
        "mergeability": "mergeable",
        "additions": 12,
        "deletions": 3,
        "changedFiles": 2,
        "headBranch": "feat/cache",
        "baseBranch": "main",
        "createdAt": "2026-07-01T00:00:00.000Z",
        "updatedAt": "2026-07-02T00:00:00.000Z",
        "mergedAt": null,
        "closedAt": null,
        "reviewers": [],
        "labels": [],
        "checks": [],
        "mergeCapabilities": { "merge": true, "squash": true, "rebase": true },
    });
    if let (Value::Object(base), Value::Object(overrides)) = (&mut base, overrides) {
        base.extend(overrides);
    }
    wire(base)
}

/// The detail the freshness rows read: behind its base by 12, with both update methods offered
/// and allowed.
pub(super) fn behind_detail() -> PullRequestDetail {
    let mut detail = pull_request_detail(json!({}));
    detail.base_comparison = Some(PullRequestBaseComparison::Behind);
    detail.behind_by = Some(12);
    detail.capabilities.update_methods = Some(vec![
        PullRequestUpdateMethod::Merge,
        PullRequestUpdateMethod::Rebase,
    ]);
    detail.viewer_permissions.update_methods = Some(vec![
        PullRequestUpdateMethod::Merge,
        PullRequestUpdateMethod::Rebase,
    ]);
    detail
}

/// The open pull request the compact row and thread panel read: every action offered and
/// permitted, passing checks, and merge available by merge and squash but not rebase.
pub(super) fn open_compact_detail() -> PullRequestDetail {
    let mut detail = pull_request_detail(json!({}));
    let actions = vec![
        PullRequestAction::Merge,
        PullRequestAction::Ready,
        PullRequestAction::Draft,
        PullRequestAction::Close,
        PullRequestAction::Reopen,
    ];
    detail.capabilities.actions = actions.clone();
    detail.capabilities.merge_methods = vec![
        PullRequestMergeMethod::Merge,
        PullRequestMergeMethod::Squash,
    ];
    detail.viewer_permissions.actions = actions;
    detail.merge_capabilities = PullRequestMergeCapabilities {
        merge: true,
        squash: true,
        rebase: false,
    };
    detail.checks = vec![check("ci", PullRequestCheckStatus::Success)];
    detail
}

/// The base a pull request handoff is built from: number 42, open, not a draft.
pub(super) fn pull_request_context_input() -> PullRequestContextInput {
    PullRequestContextInput {
        number: 42,
        title: "Add the pull requests page".into(),
        url: "https://github.com/pingdotgg/t3code/pull/42".into(),
        head_branch: "feat/page".into(),
        base_branch: "main".into(),
        state: PullRequestState::Open,
        is_draft: false,
    }
}

/// A chip a handoff left in the composer, as the composer keeps it.
pub(super) fn chip(id: &str) -> ReviewCommentContext {
    ReviewCommentContext {
        id: id.into(),
        section_id: "pull-request:42".into(),
        section_title: "PR #42".into(),
        file_path: "PR #42".into(),
        start_index: 0,
        end_index: 0,
        range_label: "Add the pull requests page".into(),
        text: String::new(),
        diff: String::new(),
        fence_language: None,
        pull_request: None,
    }
}

/// A thread link as the thread holds it. The host is github.com unless the test changes it.
pub(super) fn thread_link(number: u64, source: &str) -> ThreadPullRequestLink {
    wire(json!({
        "host": "github.com",
        "repository": "pingdotgg/t3code",
        "number": number,
        "url": format!("https://github.com/pingdotgg/t3code/pull/{number}"),
        "source": source,
        "linkedAt": "2026-09-09T00:00:00Z",
        "snapshot": null,
        "stack": null,
    }))
}

/// The legacy single-link fields a thread carries beside its link list.
pub(super) fn legacy_link(
    project_id: &str,
    repository: &str,
    number: u64,
) -> ThreadLinkedPullRequest {
    wire(json!({
        "projectId": project_id,
        "repository": repository,
        "number": number,
        "url": format!("https://github.com/{repository}/pull/{number}"),
    }))
}

pub(super) fn pull_request_ref(
    project_id: &str,
    host: Option<&str>,
    repository: &str,
    number: u64,
) -> PullRequestRef {
    PullRequestRef {
        project_id: wire(json!(project_id)),
        host: host.map(tne),
        expected_account_id: None,
        allow_stale: None,
        repository: tne(repository),
        number: PositiveInt::new(number).expect("fixture numbers are positive"),
    }
}
