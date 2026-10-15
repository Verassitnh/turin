//! Port of upstream/packages/contracts/src/threadPullRequest.ts

use crate::base_schemas::{
    IsoDateTime, NonNegativeInt, PositiveInt, ProjectId, TrimmedNonEmptyString,
};
use crate::pull_request::{
    PullRequestActor, PullRequestChecksState, PullRequestMergeability, PullRequestReviewDecision,
    PullRequestState,
};
use serde::{Deserialize, Serialize};

/// Legacy single-PR link. Still emitted as the thread's derived current pull
/// request so clients from before `pullRequests` keep working independently
/// of their release schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadLinkedPullRequest {
    pub project_id: ProjectId,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    pub url: TrimmedNonEmptyString,
}

/// Who created a thread ↔ pull request link. `stack-dismissed` is a tombstone
/// for a native-stack member the user unlinked, so the sync reactor does not
/// re-add it; clients hide it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThreadPullRequestLinkSource {
    Manual,
    Created,
    Agent,
    Stack,
    StackDismissed,
}

/// Host state persisted on a link by the sync reactor; null until first
/// sync. The overview fields are optional: a host whose cheap read lacks
/// them leaves them out, and snapshots written before they existed still
/// decode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPullRequestSnapshot {
    pub state: PullRequestState,
    pub title: TrimmedNonEmptyString,
    pub head_branch: TrimmedNonEmptyString,
    pub base_branch: TrimmedNonEmptyString,
    pub is_draft: bool,
    pub updated_at: Option<IsoDateTime>,
    pub synced_at: IsoDateTime,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub closed_at: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub merged_at: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub author: Option<Option<PullRequestActor>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additions: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletions: Option<NonNegativeInt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_files: Option<NonNegativeInt>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub review_decision: Option<Option<PullRequestReviewDecision>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::base_schemas::double_option"
    )]
    pub checks_state: Option<Option<PullRequestChecksState>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mergeability: Option<PullRequestMergeability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPullRequestStackLayer {
    pub number: PositiveInt,
    pub head_branch: TrimmedNonEmptyString,
    pub state: PullRequestState,
}

string_literal!(
    /// Constant `"native"` kind of a thread's stack link.
    ThreadNativeStackKind,
    "native"
);

/// A host-native stack the pull request belongs to. Layers run bottom to top.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPullRequestStack {
    pub kind: ThreadNativeStackKind,
    pub id: TrimmedNonEmptyString,
    pub number: PositiveInt,
    pub url: TrimmedNonEmptyString,
    pub base: TrimmedNonEmptyString,
    pub layers: Vec<ThreadPullRequestStackLayer>,
}

/// Identity of a pull request as a thread link sees it: host-level, so the
/// same PR linked from two projects (or two environments) compares equal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPullRequestKey {
    pub host: TrimmedNonEmptyString,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
}

fn default_empty_string_vec() -> Vec<TrimmedNonEmptyString> {
    Vec::new()
}

/// Present while the server watches the pull request for its thread. The
/// server wakes the thread's agent when checks finish on the head commit,
/// someone else comments, or the branch starts to conflict. The other fields
/// record what the agent was last told, so each change is reported once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPullRequestWatch {
    pub started_at: IsoDateTime,
    /// Head commit at the last pass; null where the host does not report one.
    pub head_sha: Option<TrimmedNonEmptyString>,
    /// Failed checks on that commit the agent was told about; a rerun that fails again is news.
    pub failed_checks: Vec<TrimmedNonEmptyString>,
    /// The agent was told the required checks on that commit passed.
    pub passed: bool,
    /// Names in the passing gate on that commit, so a required check that
    /// first shows up already passed is news. Empty on watches saved before
    /// this field existed (`withDecodingDefault([])`).
    #[serde(default = "default_empty_string_vec")]
    pub passed_checks: Vec<TrimmedNonEmptyString>,
    /// Remarks from others created up to this host time were reported.
    pub remarks_through: IsoDateTime,
    /// Remarks created exactly at `remarksThrough` that were reported, so a late one still counts.
    pub remark_ids: Vec<TrimmedNonEmptyString>,
    pub conflicting: bool,
    /// Comment-only wakes in a row. Watching stops at a limit, so bots cannot loop it.
    pub wakes: NonNegativeInt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPullRequestLink {
    pub host: TrimmedNonEmptyString,
    pub repository: TrimmedNonEmptyString,
    pub number: PositiveInt,
    pub url: TrimmedNonEmptyString,
    pub source: ThreadPullRequestLinkSource,
    pub linked_at: IsoDateTime,
    pub snapshot: Option<ThreadPullRequestSnapshot>,
    pub stack: Option<ThreadPullRequestStack>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<ThreadPullRequestWatch>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_decodes_with_snapshot_defaults() {
        let link: ThreadPullRequestLink = serde_json::from_value(serde_json::json!({
            "host": "github.com",
            "repository": "pingdotgg/t3code",
            "number": 42,
            "url": "https://github.com/pingdotgg/t3code/pull/42",
            "source": "agent",
            "linkedAt": "2026-10-14T00:00:00.000Z",
            "snapshot": null,
            "stack": null,
        }))
        .unwrap();
        assert_eq!(link.source, ThreadPullRequestLinkSource::Agent);
        assert!(link.snapshot.is_none());
        assert!(link.watch.is_none());
    }

    #[test]
    fn watch_defaults_passed_checks_to_empty() {
        let watch: ThreadPullRequestWatch = serde_json::from_value(serde_json::json!({
            "startedAt": "2026-10-14T00:00:00.000Z",
            "headSha": null,
            "failedChecks": ["ci"],
            "passed": true,
            "remarksThrough": "2026-10-14T00:00:00.000Z",
            "remarkIds": [],
            "conflicting": false,
            "wakes": 0,
        }))
        .unwrap();
        assert!(watch.passed_checks.is_empty());

        // And it is always written back once decoded.
        let json = serde_json::to_value(&watch).unwrap();
        assert_eq!(json["passedChecks"], serde_json::json!([]));
    }

    #[test]
    fn snapshot_double_option_round_trips() {
        let json = serde_json::json!({
            "state": "open",
            "title": "t",
            "headBranch": "feature",
            "baseBranch": "main",
            "isDraft": false,
            "updatedAt": null,
            "syncedAt": "2026-10-14T00:00:00.000Z",
            "closedAt": null,
        });
        let snapshot: ThreadPullRequestSnapshot = serde_json::from_value(json.clone()).unwrap();
        // `closedAt: null` is present-null, re-encoded as null.
        assert_eq!(snapshot.closed_at, Some(None));
        assert_eq!(serde_json::to_value(&snapshot).unwrap(), json);
    }
}
