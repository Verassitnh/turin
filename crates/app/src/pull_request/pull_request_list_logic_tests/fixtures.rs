#![allow(unused_imports)]
use super::super::*;

use indexmap::{IndexMap, IndexSet};
use serde_json::{Value, json};
use turin_contracts::{PositiveInt, ProjectId, TrimmedNonEmptyString};

pub(super) const ENV_1: &str = "env-1";

/// Upstream `entry()`: a listed change request in env-1 on github.com, with `overrides` merged on
/// top. Upstream requires `number`; here it defaults to 1 so `listed(json!({}))` is a valid row.
/// The url and head branch follow the number unless overridden.
pub(super) fn listed(overrides: Value) -> EnvironmentPullRequestEntry {
    let number = overrides.get("number").and_then(Value::as_u64).unwrap_or(1);
    let mut row = json!({
        "environmentId": ENV_1,
        "provider": "github",
        "host": "github.com",
        "projectId": "project-1",
        "projectTitle": "t3code",
        "repository": "pingdotgg/t3code",
        "number": number,
        "title": "Add the pull requests page",
        "url": format!("https://github.com/pingdotgg/t3code/pull/{number}"),
        "author": { "login": "octocat", "name": null, "avatarUrl": null },
        "headBranch": format!("feat/branch-{number}"),
        "baseBranch": "main",
        "state": "open",
        "isDraft": false,
        "mergeability": "mergeable",
        "additions": 1,
        "deletions": 0,
        "createdAt": "2026-07-01T00:00:00Z",
        "updatedAt": "2026-07-02T00:00:00Z",
        "viewerReviewRequested": false,
        "labels": [],
    });
    let row_fields = row.as_object_mut().expect("fixture row is an object");
    for (key, value) in overrides.as_object().expect("overrides are an object") {
        row_fields.insert(key.clone(), value.clone());
    }
    serde_json::from_value(row).expect("fixture entry is valid")
}

/// Upstream `VIEWERS`: the signed-in login per host.
pub(super) fn bilal_viewers() -> PullRequestViewers {
    viewers_for(&[("github.com", "Bilal")])
}

/// Upstream `NO_VIEWERS`.
pub(super) fn no_viewers() -> PullRequestViewers {
    PullRequestViewers::new()
}

pub(super) fn viewers_for(pairs: &[(&str, &str)]) -> PullRequestViewers {
    pairs
        .iter()
        .map(|(host, login)| (host.to_string(), login.to_string()))
        .collect()
}

/// A typed list filter, written the way the query string and the page send it.
pub(super) fn list_filters(value: Value) -> PullRequestListFilters {
    serde_json::from_value(value).expect("fixture filters are valid")
}

/// Upstream `new Map(entries.map((item) => [pullRequestEntryKey(item), item]))`.
pub(super) fn by_key(
    entries: &[EnvironmentPullRequestEntry],
) -> IndexMap<String, EnvironmentPullRequestEntry> {
    entries
        .iter()
        .map(|entry| (pull_request_entry_key_of(entry), entry.clone()))
        .collect()
}

pub(super) fn entry_keys<E: ListEntry>(entries: &[E]) -> Vec<String> {
    entries.iter().map(pull_request_entry_key_of).collect()
}

pub(super) fn keys_of<E: ListEntry>(entries: &[E]) -> IndexSet<String> {
    entry_keys(entries).into_iter().collect()
}

pub(super) fn numbers_of<E: ListEntry>(entries: &[E]) -> Vec<u64> {
    entries
        .iter()
        .map(|entry| entry.listing().number.get())
        .collect()
}

/// The refs a stats batch asks the host for, in the order they are asked.
pub(super) fn batch_numbers(batches: &[PullRequestStatsBatch]) -> Vec<u64> {
    batches
        .iter()
        .flat_map(|batch| batch.input.refs.iter().map(|reference| reference.number.get()))
        .collect()
}

/// The ref a stats read sends for a row of the fixture repository.
pub(super) fn stats_ref(number: u64) -> PullRequestRef {
    PullRequestRef {
        project_id: ProjectId::new("project-1").expect("fixture project id is valid"),
        host: None,
        expected_account_id: None,
        allow_stale: None,
        repository: TrimmedNonEmptyString::new("pingdotgg/t3code")
            .expect("fixture repository is valid"),
        number: PositiveInt::new(number).expect("fixture numbers are positive"),
    }
}

/// A line-count read for one row, tagged with the environment it came from.
pub(super) fn line_stat(
    environment_id: &str,
    project_id: &str,
    number: u64,
    additions: u64,
    deletions: u64,
) -> EnvironmentPullRequestStat {
    serde_json::from_value(json!({
        "environmentId": environment_id,
        "projectId": project_id,
        "repository": "pingdotgg/t3code",
        "number": number,
        "additions": additions,
        "deletions": deletions,
    }))
    .expect("fixture line-count read is valid")
}

pub(super) fn stats_scope(key: &str, policy: PullRequestStatsPolicy) -> PullRequestStatsScope {
    PullRequestStatsScope {
        key: key.to_owned(),
        policy,
    }
}
