//! Mock pull request data for the view-model seam (`MockPullRequestSource`) and
//! the demo hook. Built from JSON in the contract's wire shape so a fixture that
//! drifts from the contracts fails loudly in `fixtures_decode` instead of rendering
//! something the live service could never send.
//!
//! Timestamps are relative to "now" so relative labels ("2h ago") read the same in
//! every screenshot run.

use chrono::{Duration, SecondsFormat, Utc};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use turin_contracts::pull_request::{
    PullRequestChecks, PullRequestDetailView, PullRequestDiffResult, PullRequestFilesViewedResult,
    PullRequestLinkedThreadsResult, PullRequestListEntry, PullRequestListResult, PullRequestStack,
};
use turin_contracts::pull_request_review_guide::PullRequestReviewGuideResult;
use turin_contracts::thread_pull_request::ThreadPullRequestLink;

pub const DEMO_PROJECT_ID: &str = "project-turin";
pub const DEMO_PROJECT_TITLE: &str = "turin";
pub const DEMO_HOST: &str = "github.com";
pub const DEMO_REPOSITORY: &str = "acme/turin";
pub const DEMO_VIEWER: &str = "jackson";
/// The pull request the detail fixtures describe.
pub const DEMO_DETAIL_NUMBER: u64 = 482;

fn decode<T: DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("pull request fixture matches the contract")
}

/// ISO timestamp `secs` seconds before now.
pub fn ago(secs: i64) -> String {
    (Utc::now() - Duration::seconds(secs)).to_rfc3339_opts(SecondsFormat::Millis, true)
}

const MINUTE: i64 = 60;
const HOUR: i64 = 3600;
const DAY: i64 = 86_400;

fn actor(login: &str, name: &str) -> Value {
    json!({ "login": login, "name": name, "avatarUrl": null })
}

fn bot(login: &str) -> Value {
    json!({ "login": login, "name": null, "avatarUrl": null, "isBot": true })
}

fn label(name: &str, color: &str) -> Value {
    json!({ "name": name, "color": color })
}

#[allow(clippy::too_many_arguments)]
fn entry(
    number: u64,
    title: &str,
    author: Value,
    head: &str,
    base: &str,
    state: &str,
    draft: bool,
    mergeability: &str,
    (additions, deletions): (u64, u64),
    updated_secs: i64,
    review_requested: bool,
    labels: Vec<Value>,
    review_decision: Option<&str>,
    checks_state: Option<&str>,
    stack: Option<Value>,
) -> Value {
    let mut value = json!({
        "provider": "github",
        "host": DEMO_HOST,
        "projectId": DEMO_PROJECT_ID,
        "projectTitle": DEMO_PROJECT_TITLE,
        "repository": DEMO_REPOSITORY,
        "number": number,
        "title": title,
        "url": format!("https://{DEMO_HOST}/{DEMO_REPOSITORY}/pull/{number}"),
        "author": author,
        "headBranch": head,
        "baseBranch": base,
        "state": state,
        "isDraft": draft,
        "mergeability": mergeability,
        "additions": additions,
        "deletions": deletions,
        "createdAt": ago(updated_secs + 2 * DAY),
        "updatedAt": ago(updated_secs),
        "viewerReviewRequested": review_requested,
        "labels": labels,
    });
    if let Some(decision) = review_decision {
        value["reviewDecision"] = json!(decision);
    }
    if let Some(checks) = checks_state {
        value["checksState"] = json!(checks);
    }
    if let Some(stack) = stack {
        value["stack"] = stack;
    }
    value
}

fn stack_membership(number: u64, position: u64, size: u64, base: &str) -> Value {
    json!({ "number": number, "position": position, "size": size, "base": base })
}

/// The listing, newest activity first like a host answers it.
pub fn list_entries() -> Vec<PullRequestListEntry> {
    let rows = vec![
        entry(
            482,
            "Port the pull request views to GPUI",
            actor("jackson", "Jackson Wells"),
            "jackson/pull-request-views",
            "main",
            "open",
            false,
            "mergeable",
            (1842, 217),
            12 * MINUTE,
            false,
            vec![label("ui", "1d76db"), label("port", "5319e7")],
            Some("review-required"),
            Some("passing"),
            None,
        ),
        entry(
            488,
            "Fix sidebar status for stacked pull requests",
            actor("mara", "Mara Ellison"),
            "mara/stack-status",
            "main",
            "open",
            false,
            "mergeable",
            (64, 12),
            3 * HOUR,
            true,
            vec![label("bug", "d73a4a")],
            Some("approved"),
            Some("passing"),
            None,
        ),
        entry(
            479,
            "Composer: restore draft when a send fails",
            actor("noah", "Noah Okafor"),
            "noah/composer-draft-restore",
            "main",
            "open",
            false,
            "conflicting",
            (211, 38),
            7 * HOUR,
            true,
            vec![label("composer", "0e8a16"), label("size:M", "ededed")],
            Some("changes-requested"),
            Some("failing"),
            None,
        ),
        entry(
            491,
            "WIP: checkpoint diff virtualization",
            actor("jackson", "Jackson Wells"),
            "jackson/diff-virtualization",
            "jackson/pull-request-views",
            "open",
            true,
            "unknown",
            (903, 144),
            DAY,
            false,
            vec![label("diff", "fbca04")],
            None,
            Some("pending"),
            Some(stack_membership(491, 2, 3, "jackson/pull-request-views")),
        ),
        entry(
            474,
            "Settings: keybindings editor conflict hints",
            actor("priya", "Priya Raman"),
            "priya/keybinding-conflicts",
            "main",
            "open",
            false,
            "mergeable",
            (318, 91),
            2 * DAY,
            false,
            vec![label("settings", "c5def5")],
            Some("review-required"),
            Some("passing"),
            None,
        ),
        entry(
            468,
            "Refactor provider registry to resolve instances lazily",
            actor("sam", "Sam Duarte"),
            "sam/lazy-registry",
            "main",
            "merged",
            false,
            "unknown",
            (402, 377),
            3 * DAY,
            false,
            vec![label("refactor", "bfd4f2")],
            Some("approved"),
            Some("passing"),
            None,
        ),
        entry(
            465,
            "Docs: explain the hooks trust prompt",
            actor("sam", "Sam Duarte"),
            "docs/hooks-trust",
            "main",
            "closed",
            false,
            "unknown",
            (24, 3),
            5 * DAY,
            false,
            vec![label("documentation", "0075ca")],
            None,
            None,
            None,
        ),
        entry(
            460,
            "Bump gpui-component to 0.5.1",
            bot("dependabot[bot]"),
            "dependabot/cargo/gpui-component-0.5.1",
            "main",
            "open",
            false,
            "mergeable",
            (6, 6),
            6 * DAY,
            false,
            vec![label("dependencies", "0366d6")],
            None,
            Some("passing"),
            None,
        ),
    ];
    rows.into_iter().map(decode).collect()
}

/// What `pullRequests.list` answers: the rows plus the host summary and the signed-in account.
pub fn list_result() -> PullRequestListResult {
    let entries: Vec<Value> = list_entries()
        .iter()
        .map(|entry| serde_json::to_value(entry).expect("entries encode"))
        .collect();
    decode(json!({
        "viewers": { DEMO_HOST: DEMO_VIEWER },
        "providers": [{
            "host": DEMO_HOST,
            "kind": "github",
            "searchesOnHost": true,
            "actions": ["merge", "ready", "draft", "close", "reopen"],
            "projectCount": 1,
            "configured": true,
            "detail": null,
        }],
        "entries": entries,
        "errors": [],
        "truncated": false,
        "nextCursors": {},
    }))
}

/// A detail for any listed row: the demo detail re-labelled with the row's own fields and no
/// activity, so every row opens to something that agrees with the list.
pub fn detail_view_for(entry: &PullRequestListEntry) -> PullRequestDetailView {
    if entry.number.get() == DEMO_DETAIL_NUMBER {
        return detail_view();
    }
    let mut value = serde_json::to_value(detail_view()).expect("detail encodes");
    let row = serde_json::to_value(entry).expect("entry encodes");
    for key in [
        "title", "url", "author", "state", "isDraft", "mergeability", "additions", "deletions",
        "headBranch", "baseBranch", "createdAt", "updatedAt", "labels", "number",
    ] {
        value[key] = row[key].clone();
    }
    value["body"] = json!(format!("Automated description for {}.", entry.title.as_str()));
    value["changedFiles"] = json!(3);
    value["comments"] = json!([]);
    value["commentCount"] = json!(0);
    value["reviewThreads"] = json!([]);
    value["commits"] = json!([]);
    value["reactions"] = json!([]);
    value["checks"] = json!([]);
    value["reviewers"] = json!([]);
    if let Some(object) = value.as_object_mut() {
        object.remove("headSha");
    }
    if entry.state != turin_contracts::pull_request::PullRequestState::Open {
        value["mergedAt"] = if entry.state == turin_contracts::pull_request::PullRequestState::Merged {
            json!(ago(2 * DAY))
        } else {
            Value::Null
        };
        value["closedAt"] = json!(ago(2 * DAY));
    }
    decode(value)
}

/// A freshly posted review-thread reply by the viewer.
pub fn new_thread_comment(id: &str, body: &str) -> turin_contracts::pull_request::PullRequestThreadComment {
    decode(json!({
        "id": id, "author": actor(DEMO_VIEWER, "Jackson Wells"),
        "body": body, "createdAt": ago(0), "url": null, "canEdit": true,
    }))
}

/// A freshly posted comment by the viewer.
pub fn new_comment(id: &str, body: &str) -> turin_contracts::pull_request::PullRequestComment {
    decode(json!({
        "id": id, "kind": "issue-comment",
        "author": actor(DEMO_VIEWER, "Jackson Wells"),
        "body": body, "createdAt": ago(0), "url": null, "path": null, "reviewState": null,
        "canEdit": true,
    }))
}

fn demo_reviewers() -> Value {
    json!([actor("mara", "Mara Ellison"), actor("noah", "Noah Okafor")])
}

fn checks_json() -> Value {
    json!([
        { "name": "check", "status": "success", "description": "Completed in 2m 14s", "url": "https://github.com/acme/turin/actions/runs/1", "required": true },
        { "name": "clippy", "status": "success", "description": "Completed in 3m 02s", "url": "https://github.com/acme/turin/actions/runs/2", "required": true },
        { "name": "test (macos-14)", "status": "success", "description": "Completed in 6m 41s", "url": "https://github.com/acme/turin/actions/runs/3", "required": true },
        { "name": "test (ubuntu-24.04)", "status": "failure", "description": "tests::pull_request::list_logic failed", "url": "https://github.com/acme/turin/actions/runs/4", "required": false },
        { "name": "screenshots", "status": "pending", "description": "Queued", "url": null },
        { "name": "deploy-preview", "status": "skipped", "description": "Skipped for forks", "url": null },
        { "name": "codeql", "status": "neutral", "description": null, "url": null },
        { "name": "release-notes", "status": "action-required", "description": "Approve to run", "url": null },
    ])
}

/// What `pullRequests.checks` answers for the demo pull request.
pub fn checks() -> PullRequestChecks {
    decode(json!({ "state": "open", "checks": checks_json() }))
}

fn reactions() -> Value {
    json!([
        { "content": "thumbs-up", "count": 3, "actors": ["mara", "noah", "priya"], "viewerHasReacted": false },
        { "content": "rocket", "count": 1, "actors": ["jackson"], "viewerHasReacted": true },
    ])
}

const BODY: &str = "## Summary\n\nPorts `components/pullRequest/*` to GPUI: the list with filters, the detail \
panel (summary, timeline, code, checks, reviews, guide), thread links and stacks.\n\n\
### What changed\n\n- Pure logic modules ported 1:1 with their tests\n- A view-model seam so the views never call the \
service directly\n- Mock source for screenshots (`TURIN_PULL_REQUESTS_DEMO`)\n\n### Notes for review\n\n\
1. The code tab renders the unified patch itself until the shared diff viewer lands.\n\
2. Checks refresh every 45s, matching upstream.\n\n> The live service is wired by T22.\n\n\
```rust\nlet snapshot = store.read(cx).snapshot();\n```\n\nCloses #471, refs #455.";

fn capabilities_json() -> Value {
    json!({
        "diff": true,
        "comment": true,
        "actions": ["merge", "ready", "draft", "close", "reopen", "update-branch", "enable-auto-merge", "disable-auto-merge", "revert"],
        "mergeMethods": ["merge", "squash", "rebase"],
        "updateMethods": ["merge", "rebase"],
        "search": true,
        "reactions": true,
        "viewedFiles": "host",
        "review": { "inlineComment": true, "reply": true, "resolve": true, "verdicts": ["comment", "approve", "request-changes"] },
        "reviewers": { "request": true, "listCandidates": true },
        "edit": { "changeRequest": true, "comment": true },
        "stacks": true,
        "stackActions": false,
        "labels": true,
    })
}

fn permissions_json() -> Value {
    json!({
        "actions": ["merge", "ready", "draft", "close", "reopen", "update-branch", "enable-auto-merge", "revert"],
        "comment": true,
        "resolve": true,
        "verdicts": ["comment", "approve", "request-changes"],
        "requestReviewers": true,
        "updateMethods": ["merge", "rebase"],
        "labels": true,
        "editChangeRequest": true,
    })
}

fn comments_json() -> Value {
    json!([
        {
            "id": "IC_1", "kind": "issue-comment",
            "author": actor("mara", "Mara Ellison"),
            "body": "Looks great so far. The stack header matches upstream. One question about the checks refresh cadence.",
            "createdAt": ago(20 * HOUR), "url": "https://github.com/acme/turin/pull/482#issuecomment-1",
            "path": null, "reviewState": null, "reactions": reactions(), "canEdit": false,
        },
        {
            "id": "R_1", "kind": "review",
            "author": actor("noah", "Noah Okafor"),
            "body": "A few nits inline, otherwise good to go once CI is green.",
            "createdAt": ago(9 * HOUR), "url": "https://github.com/acme/turin/pull/482#pullrequestreview-1",
            "path": null, "reviewState": "COMMENTED",
        },
        {
            "id": "IC_2", "kind": "issue-comment",
            "author": bot("github-actions[bot]"),
            "body": "Screenshots uploaded to the workflow run.",
            "createdAt": ago(3 * HOUR), "url": null, "path": null, "reviewState": null,
        },
        {
            "id": "IC_3", "kind": "issue-comment",
            "author": actor("jackson", "Jackson Wells"),
            "body": "Thanks! Pushed fixes for the nits, rerunning CI.",
            "createdAt": ago(40 * MINUTE), "editedAt": ago(30 * MINUTE),
            "url": null, "path": null, "reviewState": null, "canEdit": true,
        },
    ])
}

fn review_threads_json() -> Value {
    json!([
        {
            "id": "RT_1", "path": "crates/app/src/pull_request/pull_request_list_logic.rs",
            "line": 61, "side": "right", "isResolved": false, "isOutdated": false,
            "canResolve": true,
            "comments": [
                {
                    "id": "RC_1", "author": actor("noah", "Noah Okafor"),
                    "body": "Should this take `&[T]` instead of an owned `Vec`?",
                    "createdAt": ago(9 * HOUR), "url": null,
                },
                {
                    "id": "RC_2", "author": actor("jackson", "Jackson Wells"),
                    "body": "Good call. Switched to a slice.",
                    "createdAt": ago(8 * HOUR), "url": null, "canEdit": true,
                },
            ],
        },
        {
            "id": "RT_2", "path": "crates/app/src/pull_request/view_model.rs",
            "line": 14, "side": "right", "isResolved": true, "isOutdated": true,
            "canResolve": true,
            "comments": [
                {
                    "id": "RC_3", "author": actor("mara", "Mara Ellison"),
                    "body": "Nit: doc comment says `store` twice.",
                    "createdAt": ago(DAY), "url": null,
                },
            ],
        },
    ])
}

fn commits_json() -> Value {
    json!([
        {
            "oid": "1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d",
            "messageHeadline": "feat(app): scaffold pull request module",
            "committedDate": ago(2 * DAY), "additions": 120, "deletions": 4,
            "authors": [actor("jackson", "Jackson Wells")],
        },
        {
            "oid": "5e6f708192a3b4c5d6e7f8091a2b3c4d1a2b3c4d",
            "messageHeadline": "feat(app): list view with filters",
            "committedDate": ago(DAY), "additions": 902, "deletions": 61,
            "authors": [actor("jackson", "Jackson Wells")],
        },
        {
            "oid": "9f2c1ab07e5d4c3b8a6f10e2d49b7c5a3e8f1d60",
            "messageHeadline": "fix(app): address review nits",
            "committedDate": ago(40 * MINUTE), "additions": 820, "deletions": 152,
            "authors": [actor("jackson", "Jackson Wells")],
        },
    ])
}

/// The detail with its activity merged in, as the panel shows it.
pub fn detail_view() -> PullRequestDetailView {
    let mut value = json!({
        "provider": "github",
        "projectId": DEMO_PROJECT_ID,
        "projectTitle": DEMO_PROJECT_TITLE,
        "workspaceRoot": "/Users/jackson/code/turin",
        "repository": DEMO_REPOSITORY,
        "number": DEMO_DETAIL_NUMBER,
        "title": "Port the pull request views to GPUI",
        "body": BODY,
        "url": format!("https://{DEMO_HOST}/{DEMO_REPOSITORY}/pull/{DEMO_DETAIL_NUMBER}"),
        "author": actor("jackson", "Jackson Wells"),
        "state": "open",
        "isDraft": false,
        "mergeability": "mergeable",
        "additions": 1842,
        "deletions": 217,
        "changedFiles": 4,
        "headBranch": "jackson/pull-request-views",
        "headRepositoryNameWithOwner": null,
        "headSha": "9f2c1ab07e5d4c3b8a6f10e2d49b7c5a3e8f1d60",
        "baseBranch": "main",
        "createdAt": ago(2 * DAY),
        "updatedAt": ago(12 * MINUTE),
        "mergedAt": null,
        "closedAt": null,
        "mergeCapabilities": { "merge": true, "squash": true, "rebase": true },
        "viewer": DEMO_VIEWER,
        "baseComparison": "behind",
        "behindBy": 3,
        "autoMergeEnabled": false,
        "commentCount": 4,
        "commentsTruncated": false,
        "reviewThreadsTruncated": false,
    });
    value["capabilities"] = capabilities_json();
    value["viewerPermissions"] = permissions_json();
    value["reviewers"] = demo_reviewers();
    value["labels"] = json!([label("ui", "1d76db"), label("port", "5319e7")]);
    value["checks"] = checks_json();
    value["comments"] = comments_json();
    value["reviewThreads"] = review_threads_json();
    value["commits"] = commits_json();
    value["reactions"] = reactions();
    decode(value)
}

const PATCH: &str = r#"diff --git a/crates/app/src/pull_request/mod.rs b/crates/app/src/pull_request/mod.rs
new file mode 100644
index 0000000..3b1a2c4
--- /dev/null
+++ b/crates/app/src/pull_request/mod.rs
@@ -0,0 +1,9 @@
+//! The GitHub pull request views.
+
+pub mod fixtures;
+pub mod view_model;
+
+pub use view_model::{PullRequestSource, PullRequestStore};
+
+#[cfg(test)]
+mod tests;
diff --git a/crates/app/src/pull_request/pull_request_list_logic.rs b/crates/app/src/pull_request/pull_request_list_logic.rs
index 41d7a2e..c9d03fa 100644
--- a/crates/app/src/pull_request/pull_request_list_logic.rs
+++ b/crates/app/src/pull_request/pull_request_list_logic.rs
@@ -58,12 +58,16 @@ fn normalize(value: Option<&str>) -> Option<String> {
     let trimmed = js_trim(value.unwrap_or("")).to_lowercase();
     (!trimmed.is_empty()).then_some(trimmed)
 }

-pub fn group_pull_requests(entries: Vec<Entry>) -> Vec<Group> {
-    let mut buckets = Buckets::default();
-    for entry in entries {
-        buckets.push(entry);
+pub fn group_pull_requests(entries: &[Entry], viewers: &Viewers) -> Vec<Group> {
+    let mut buckets = Buckets::default();
+    for entry in entries {
+        if is_authored_by_viewer(entry, viewers) {
+            buckets.authored.push(entry.clone());
+        } else if entry.viewer_review_requested {
+            buckets.review_requested.push(entry.clone());
+        } else {
+            buckets.others.push(entry.clone());
+        }
     }
     buckets.into_groups()
 }
@@ -90,4 +94,5 @@ pub fn entry_key(entry: &Entry) -> String {
     let scope = entry.environment_id.as_ref().map(|id| format!("{id}:")).unwrap_or_default();
     format!("{scope}{}:{}#{}", entry.host, entry.repository, entry.number)
 }
+
 // end of file
diff --git a/docs/screenshots/README.md b/docs/screenshots/README.md
deleted file mode 100644
index e69de29..0000000
--- a/docs/screenshots/README.md
+++ /dev/null
@@ -1,3 +0,0 @@
-# Screenshots
-
-Captured with the demo hooks.
diff --git a/crates/app/src/pull_request/view_model.rs b/crates/app/src/pull_request/view_model.rs
index 7d1e0aa..2f33b18 100644
--- a/crates/app/src/pull_request/view_model.rs
+++ b/crates/app/src/pull_request/view_model.rs
@@ -10,7 +10,11 @@ use gpui_kit::{App, Context};

 /// Whatever feeds the store implements this.
 pub trait PullRequestSource: 'static {
-    fn refresh(&self, cx: &mut App);
+    fn refresh_list(&self, cx: &mut App);
+    /// Reads one pull request's detail again.
+    fn refresh_detail(&self, cx: &mut App);
+    fn run_action(&self, action: PullRequestAction, cx: &mut App);
 }

 pub struct PullRequestStore {
"#;

/// The unified diff `pullRequests.diff` answers for the demo pull request.
pub fn diff_result() -> PullRequestDiffResult {
    decode(json!({
        "patch": PATCH,
        "truncated": false,
        "nextCursor": null,
    }))
}

pub fn files_viewed() -> PullRequestFilesViewedResult {
    decode(json!({
        "files": [
            { "path": "crates/app/src/pull_request/mod.rs", "state": "viewed" },
            { "path": "docs/screenshots/README.md", "state": "dismissed" },
        ],
        "truncated": false,
    }))
}

pub fn stack() -> PullRequestStack {
    decode(json!({
        "id": "stack-1",
        "number": 1,
        "url": "https://github.com/acme/turin/stack/1",
        "base": "main",
        "layers": [
            { "number": 482, "title": "Port the pull request views to GPUI", "isDraft": false, "headSha": "9f2c1ab07e5d4c3b8a6f10e2d49b7c5a3e8f1d60", "headBranch": "jackson/pull-request-views", "state": "open" },
            { "number": 491, "title": "WIP: checkpoint diff virtualization", "isDraft": true, "headSha": "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00", "headBranch": "jackson/diff-virtualization", "state": "open" },
            { "number": 497, "title": "Wire the live pull request service", "isDraft": true, "headSha": "feedbeef00feedbeef00feedbeef00feedbeef00", "headBranch": "jackson/live-service", "state": "open" },
        ],
    }))
}

pub fn linked_threads() -> PullRequestLinkedThreadsResult {
    decode(json!({
        "threads": [
            { "id": "thread-pr-views", "projectId": DEMO_PROJECT_ID, "title": "Port the pull request views", "archivedAt": null },
            { "id": "thread-pr-logic", "projectId": DEMO_PROJECT_ID, "title": "Port pull request logic modules", "archivedAt": ago(DAY) },
        ],
    }))
}

/// The thread ↔ pull request links of the demo thread: the PR it is working on and a
/// stack layer, one of each source.
pub fn thread_links() -> Vec<ThreadPullRequestLink> {
    let links = json!([
        {
            "host": DEMO_HOST, "repository": DEMO_REPOSITORY, "number": 482,
            "url": "https://github.com/acme/turin/pull/482",
            "source": "created", "linkedAt": ago(2 * DAY),
            "snapshot": {
                "state": "open", "title": "Port the pull request views to GPUI",
                "headBranch": "jackson/pull-request-views", "baseBranch": "main",
                "isDraft": false, "updatedAt": ago(12 * MINUTE), "syncedAt": ago(MINUTE),
                "reviewDecision": "review-required", "checksState": "passing",
                "mergeability": "mergeable", "additions": 1842, "deletions": 217, "changedFiles": 4,
            },
            "stack": {
                "kind": "native", "id": "stack-1", "number": 1,
                "url": "https://github.com/acme/turin/stack/1", "base": "main",
                "layers": [
                    { "number": 482, "headBranch": "jackson/pull-request-views", "state": "open" },
                    { "number": 491, "headBranch": "jackson/diff-virtualization", "state": "open" },
                    { "number": 497, "headBranch": "jackson/live-service", "state": "open" },
                ],
            },
        },
        {
            "host": DEMO_HOST, "repository": DEMO_REPOSITORY, "number": 468,
            "url": "https://github.com/acme/turin/pull/468",
            "source": "manual", "linkedAt": ago(3 * DAY),
            "snapshot": {
                "state": "merged", "title": "Refactor provider registry to resolve instances lazily",
                "headBranch": "sam/lazy-registry", "baseBranch": "main",
                "isDraft": false, "updatedAt": ago(3 * DAY), "syncedAt": ago(HOUR),
            },
            "stack": null,
        },
    ]);
    decode(links)
}

/// A finished guide written against the demo head.
pub fn guide_result() -> PullRequestReviewGuideResult {
    decode(json!({
        "guide": {
            "headSha": "9f2c1ab07e5d4c3b8a6f10e2d49b7c5a3e8f1d60",
            "generatedAt": ago(30 * MINUTE),
            "modelSelection": { "instanceId": "claudeAgent", "model": "claude-sonnet-5-5" },
            "stops": [
                {
                    "title": "The module scaffold",
                    "summary": "Start here: the new `pull_request` module declares every logic and view file, so the rest of the review reads as filling in stubs.",
                    "attention": null,
                    "excerpts": [
                        { "path": "crates/app/src/pull_request/mod.rs", "side": "new", "start": 1, "end": 9, "caption": "The public surface other modules import." },
                    ],
                    "files": [],
                    "notes": [],
                    "questions": ["Should `fixtures` be compiled outside tests?"],
                },
                {
                    "title": "Grouping by involvement",
                    "summary": "Authorship is decided per host, then review requests, then everything else. The change moves the helper to borrow its input.",
                    "attention": { "kind": "contract", "note": "The signature of `group_pull_requests` changes: callers must pass viewers." },
                    "excerpts": [
                        { "path": "crates/app/src/pull_request/pull_request_list_logic.rs", "side": "new", "start": 61, "end": 72, "caption": "Authored wins over review-requested, as upstream." },
                    ],
                    "files": [{ "path": "crates/app/src/pull_request/view_model.rs", "note": "Only doc changes." }],
                    "notes": [
                        { "path": "crates/app/src/pull_request/pull_request_list_logic.rs", "line": 66, "title": "Clones per entry", "body": "Each entry is cloned into its bucket; fine at page size, worth revisiting if the list grows." },
                    ],
                    "questions": [],
                },
            ],
        },
        "run": null,
        "currentHeadSha": "9f2c1ab07e5d4c3b8a6f10e2d49b7c5a3e8f1d60",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_decode() {
        assert_eq!(list_entries().len(), 8);
        assert_eq!(detail_view().number.get(), DEMO_DETAIL_NUMBER);
        assert_eq!(checks().checks.len(), 8);
        assert!(diff_result().patch.contains("diff --git"));
        assert_eq!(files_viewed().files.len(), 2);
        assert_eq!(stack().layers.len(), 3);
        assert_eq!(linked_threads().threads.len(), 2);
        assert_eq!(thread_links().len(), 2);
        assert!(guide_result().guide.is_some());
    }
}
