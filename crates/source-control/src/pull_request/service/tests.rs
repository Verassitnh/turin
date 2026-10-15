//! Port of upstream/apps/server/src/pullRequest/PullRequestService.test.ts and
//! upstream/apps/server/src/pullRequest/PullRequestProviderRateLimit.test.ts, split by upstream
//! line range so each file stays readable.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use parking_lot::Mutex;
use serde_json::Value;
use turin_contracts::pull_request::PullRequestState;

use crate::pull_request::service::PullRequestService;
use crate::pull_request::service::test_support::input;
use crate::source_control_core::pull_request_provider::{
    ProviderChangeRequest, ProviderChangeRequestSummary,
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
