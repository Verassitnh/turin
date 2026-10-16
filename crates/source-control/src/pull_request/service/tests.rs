//! Port of upstream/apps/server/src/pullRequest/PullRequestService.test.ts and
//! upstream/apps/server/src/pullRequest/PullRequestProviderRateLimit.test.ts, split by upstream
//! line range so each file stays readable.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{Value, json};
use turin_contracts::pull_request::{PullRequestRef, PullRequestState};

use crate::pull_request::service::PullRequestService;
use crate::pull_request::service::test_support::*;
use crate::source_control_core::pull_request_provider::{
    ProviderChangeRequest, ProviderChangeRequestSummary, ProviderFileRevisions,
};

mod part_a;
mod part_b;
mod part_c;
mod part_d;
mod part_e;
mod part_f;
mod part_g;
mod part_h;
mod part_i;
mod part_j;
mod part_k;
mod part_l;
mod part_m;
mod part_n;
mod part_o;
mod part_p;
mod part_q;
mod part_r;
mod part_s;
mod part_t;
mod part_u;
mod part_v;
mod part_w;

pub(super) fn counter() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

pub(super) fn log() -> Arc<Mutex<Vec<String>>> {
    Arc::new(Mutex::new(Vec::new()))
}

/// `reads.splice(0).toSorted()`
pub(super) fn drain_sorted(reads: &Mutex<Vec<String>>) -> Vec<String> {
    let mut drained = std::mem::take(&mut *reads.lock());
    drained.sort();
    drained
}

/// The named fields of each row under `key`, as arrays, in row order: the shape a test reads
/// `result.entries.map(({ host, number }) => [host, number])` in.
pub(super) fn rows(value: &Value, key: &str, fields: &[&str]) -> Value {
    Value::Array(
        value[key]
            .as_array()
            .expect("rows array")
            .iter()
            .map(|row| Value::Array(fields.iter().map(|field| row[*field].clone()).collect()))
            .collect(),
    )
}

/// What `getChangeRequestSummary` hands back for `changeRequest(number, updatedAt)` in `state`.
pub(super) fn summary_of(
    change_request: &ProviderChangeRequest,
    state: PullRequestState,
) -> ProviderChangeRequestSummary {
    ProviderChangeRequestSummary {
        number: change_request.number,
        title: change_request.title.clone(),
        url: change_request.url.clone(),
        head_branch: change_request.head_branch.clone(),
        base_branch: change_request.base_branch.clone(),
        state,
        is_draft: Some(change_request.is_draft),
        closed_at: None,
        merged_at: None,
        updated_at: change_request.updated_at.clone(),
        author: None,
        additions: None,
        deletions: None,
        changed_files: None,
        review_decision: None,
        checks_state: None,
        mergeability: None,
        stack: None,
    }
}

/// `service.list(input)` as JSON, the shape every listing assertion reads.
pub(super) async fn list_json(service: &PullRequestService, value: Value) -> Value {
    serde_json::to_value(service.list(&input(value)).await.unwrap()).unwrap()
}

/// `{ ...ref }` as the JSON a test puts in an input object.
pub(super) fn ref_json_value(reference: &turin_contracts::pull_request::PullRequestRef) -> Value {
    serde_json::to_value(reference).unwrap()
}

/// Paths each `getFileRevisions` read was asked about, one entry per read.
pub(super) type AskedPaths = Arc<Mutex<Vec<Vec<String>>>>;

/// Each path's head version. A path left out is at the empty version, which is what a file the
/// change request deletes is at.
pub(super) type Revisions = Arc<Mutex<IndexMap<String, String>>>;

/// Paths the host could not read, so it leaves them out of its answer.
pub(super) type Unreadable = Arc<Mutex<HashSet<String>>>;

pub(super) fn asked_paths() -> AskedPaths {
    Arc::new(Mutex::new(Vec::new()))
}

pub(super) fn revisions_of(pairs: &[(&str, &str)]) -> Revisions {
    Arc::new(Mutex::new(
        pairs
            .iter()
            .map(|(path, version)| ((*path).to_owned(), (*version).to_owned()))
            .collect(),
    ))
}

pub(super) fn no_unreadable() -> Unreadable {
    Arc::new(Mutex::new(HashSet::new()))
}

/// Each asked read with its paths sorted, as upstream compares them.
pub(super) fn asked_sorted(asked: &AskedPaths) -> Vec<Vec<String>> {
    asked
        .lock()
        .iter()
        .map(|read| {
            let mut read = read.clone();
            read.sort();
            read
        })
        .collect()
}

/// Lets spawned refreshes run, as the upstream `Effect.yieldNow`s do.
pub(super) async fn settle() {
    for _ in 0..3 {
        tokio::task::yield_now().await;
    }
}

/// The GitLab project the environment-kept viewed-files tests share.
pub(super) fn environment_project() -> ProjectSpec {
    project("p1", "on gitlab", "/a", "group/project").provider("gitlab")
}

/// `{ projectId: "p1", repository: "group/project", number }`
pub(super) fn environment_ref(number: u64) -> PullRequestRef {
    pr_ref("p1", "group/project", number)
}

/// Upstream `environmentViewedProvider`: a host that keeps no marks of its own, so the marks are
/// this environment's. It answers each asked path with its version from `revisions`, and leaves out
/// any path in `unreadable`.
pub(super) fn environment_viewed_provider(
    revisions: &Revisions,
    asked: &AskedPaths,
    unreadable: &Unreadable,
) -> FakeProvider {
    let (revisions, asked, unreadable) = (revisions.clone(), asked.clone(), unreadable.clone());
    FakeProvider::new("gitlab")
        .capabilities_with(|capabilities| {
            capabilities["viewedFiles"] = json!("environment");
        })
        // A merge confirms itself against the host before it says the change request landed.
        .on_get_change_request_summary(|_| {
            Ok(summary_of(
                &change_request(1, "2026-07-02T00:00:00Z"),
                PullRequestState::Open,
            ))
        })
        .on_get_file_revisions(move |input| {
            asked.lock().push(input.paths.clone());
            let (revisions, unreadable) = (revisions.lock(), unreadable.lock());
            Ok(ProviderFileRevisions {
                revisions: input
                    .paths
                    .iter()
                    .filter(|path| !unreadable.contains(*path))
                    .map(|path| {
                        (
                            path.clone(),
                            revisions.get(path).cloned().unwrap_or_default(),
                        )
                    })
                    .collect(),
                complete: None,
            })
        })
}

/// `service.files_viewed(reference)` as JSON, with its files sorted by path.
pub(super) async fn viewed_sorted(
    service: &PullRequestService,
    reference: &PullRequestRef,
) -> Value {
    let mut value = serde_json::to_value(service.files_viewed(reference).await.unwrap()).unwrap();
    if let Some(files) = value["files"].as_array_mut() {
        files.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    }
    value
}

/// `setFilesViewed` for one reference: each `(path, viewed)` pair.
pub(super) async fn press(
    service: &PullRequestService,
    reference: &PullRequestRef,
    files: &[(&str, bool)],
) {
    let files: Vec<Value> = files
        .iter()
        .map(|(path, viewed)| json!({ "path": path, "viewed": viewed }))
        .collect();
    service
        .set_files_viewed(&input(with_ref(reference, json!({ "files": files }))))
        .await
        .unwrap();
}
