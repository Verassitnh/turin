#![allow(unused_imports)]
use super::super::*;
use serde_json::{Value, json};

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
    detail.behind_by = NonNegativeInt::new(12);
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
    detail.capabilities.merge_methods = vec![PullRequestMergeMethod::Merge, PullRequestMergeMethod::Squash];
    detail.viewer_permissions.actions = actions;
    detail.merge_capabilities = PullRequestMergeCapabilities {
        merge: true,
        squash: true,
        rebase: false,
    };
    detail.checks = vec![check(PullRequestCheckStatus::Success)];
    detail
}

pub(super) fn check(status: PullRequestCheckStatus) -> PullRequestCheck {
    PullRequestCheck {
        name: turin_contracts::TrimmedNonEmptyString::new("ci").unwrap(),
        status,
        description: None,
        url: None,
        required: None,
    }
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
pub(super) fn legacy_link(project_id: &str, repository: &str, number: u64) -> ThreadLinkedPullRequest {
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
        host: host.map(|host| turin_contracts::TrimmedNonEmptyString::new(host).unwrap()),
        expected_account_id: None,
        allow_stale: None,
        repository: turin_contracts::TrimmedNonEmptyString::new(repository).unwrap(),
        number: turin_contracts::PositiveInt::new(number).unwrap(),
    }
}

/// A repository identity read from a git remote, as the detail page's reference resolution sees it.
pub(super) fn repository_identity(canonical_key: &str, remote_url: &str, provider: Option<&str>) -> RepositoryIdentity {
    wire(json!({
        "canonicalKey": canonical_key,
        "locator": { "source": "git-remote", "remoteName": "origin", "remoteUrl": remote_url },
        "provider": provider,
    }))
}
