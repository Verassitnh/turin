//! Port of upstream/apps/server/src/pullRequest/PullRequestService.test.ts and
//! upstream/apps/server/src/pullRequest/PullRequestProviderRateLimit.test.ts, split by upstream
//! line range so each file stays readable.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use parking_lot::Mutex;
use serde_json::Value;

mod part_a;
mod part_b;

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
