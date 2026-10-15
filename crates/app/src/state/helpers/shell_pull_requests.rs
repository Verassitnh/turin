//! Port of upstream/packages/client-runtime/src/state/shellPullRequests.ts
//!
//! A shell snapshot whose threads may arrive before their pull request links are decoded. Those
//! links are most of a large shell's decode cost and only feed PR badges, so the thread list
//! can paint first.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;
use turin_contracts::ThreadId;
use turin_contracts::orchestration_v2::OrchestrationV2ShellSnapshot;
use turin_contracts::thread_pull_request::ThreadPullRequestLink;

/// Bounds each uninterrupted stretch of decoding, so frames run between batches.
const LINKS_PER_BATCH: usize = 400;

/// `DeferredShellSnapshot`: until `load_pull_requests` runs, the snapshot's threads carry no
/// `pull_requests`. Upstream stores the loader as an `Effect` on the snapshot; here it is a
/// method over the detached JSON.
#[derive(Debug, Clone)]
pub struct DeferredShellSnapshot {
    pub snapshot: OrchestrationV2ShellSnapshot,
    thread_ids: Vec<ThreadId>,
    raw_links: Vec<Option<Value>>,
}

/// Removes each parsed thread row's pull request links from `snapshot` in place and returns
/// them in row order (`None` where a row had no `pullRequests` key), for `defer_pull_requests`
/// once the rows are decoded.
pub fn detach_pull_requests(snapshot: &mut Value) -> Vec<Option<Value>> {
    let Some(rows) = snapshot.get_mut("threads").and_then(Value::as_array_mut) else {
        return Vec::new();
    };
    rows.iter_mut()
        .map(|row| row.as_object_mut()?.remove("pullRequests"))
        .collect()
}

/// Attaches the detached links to a decoded snapshot.
pub fn defer_pull_requests(
    snapshot: OrchestrationV2ShellSnapshot,
    raw_links: Vec<Option<Value>>,
) -> DeferredShellSnapshot {
    let thread_ids = snapshot
        .threads
        .iter()
        .map(|thread| thread.id.clone())
        .collect();
    DeferredShellSnapshot {
        snapshot,
        thread_ids,
        raw_links,
    }
}

impl DeferredShellSnapshot {
    /// Decodes the detached links after yielding to the host. A thread whose links cannot be
    /// read gets none; its row stays usable.
    pub async fn load_pull_requests(&self) -> HashMap<ThreadId, Vec<ThreadPullRequestLink>> {
        let mut links_by_thread_id = HashMap::new();
        let mut batch_size = LINKS_PER_BATCH;
        for (index, thread_id) in self.thread_ids.iter().enumerate() {
            let Some(Some(raw)) = self.raw_links.get(index) else {
                continue;
            };
            let size = raw.as_array().map_or(1, Vec::len);
            if batch_size + size > LINKS_PER_BATCH {
                yield_to_host().await;
                batch_size = 0;
            }
            batch_size += size;
            // An unreadable thread keeps no links; the others still fill in.
            match serde_json::from_value::<Vec<ThreadPullRequestLink>>(raw.clone()) {
                Ok(links) => {
                    links_by_thread_id.insert(thread_id.clone(), links);
                }
                Err(error) => tracing::warn!(
                    thread_id = %thread_id,
                    cause = %error,
                    "Discarding unreadable shell pull request links."
                ),
            }
        }
        links_by_thread_id
    }
}

/// A zero sleep only yields to the executor. A 1ms sleep takes a timer, so the rows can render
/// before the deferred decode.
async fn yield_to_host() {
    tokio::time::sleep(Duration::from_millis(1)).await;
}

/// Decodes a JSON shell snapshot with its threads' pull request links deferred.
pub fn decode_shell_snapshot_deferring_pull_requests(
    mut json: Value,
) -> Result<DeferredShellSnapshot, serde_json::Error> {
    let raw_links = detach_pull_requests(&mut json);
    let snapshot = serde_json::from_value::<OrchestrationV2ShellSnapshot>(json)?;
    Ok(defer_pull_requests(snapshot, raw_links))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fixtures::{v2_project, v2_thread_shell};
    use serde_json::json;

    fn link(number: u64) -> Value {
        json!({
            "host": "github.com",
            "repository": "acme/web",
            "number": number,
            "url": format!("https://github.com/acme/web/pull/{number}"),
            "source": "agent",
            "linkedAt": "2026-10-14T00:00:00.000Z",
            "snapshot": null,
            "stack": null,
        })
    }

    fn thread_json(id: &str, pull_requests: Option<Value>) -> Value {
        let mut thread = serde_json::to_value(v2_thread_shell()).unwrap();
        let object = thread.as_object_mut().unwrap();
        object.insert("id".into(), json!(id));
        object.remove("pullRequests");
        if let Some(links) = pull_requests {
            object.insert("pullRequests".into(), links);
        }
        thread
    }

    fn shell_json(threads: Vec<Value>) -> Value {
        json!({
            "schemaVersion": 1,
            "snapshotSequence": 7,
            "threads": threads,
            "archivedThreads": [],
            "projects": [serde_json::to_value(v2_project()).unwrap()],
        })
    }

    #[test]
    fn detaches_each_rows_links_in_row_order() {
        let mut snapshot = json!({
            "threads": [
                { "id": "a", "pullRequests": [1] },
                { "id": "b" },
                "not a row",
                { "id": "c", "pullRequests": null },
            ],
        });
        let links = detach_pull_requests(&mut snapshot);
        assert_eq!(links, vec![Some(json!([1])), None, None, Some(Value::Null)]);
        assert_eq!(
            snapshot,
            json!({ "threads": [{ "id": "a" }, { "id": "b" }, "not a row", { "id": "c" }] })
        );
    }

    #[test]
    fn detaches_nothing_from_a_snapshot_without_thread_rows() {
        assert!(detach_pull_requests(&mut json!("nope")).is_empty());
        assert!(detach_pull_requests(&mut json!({ "threads": 3 })).is_empty());
        assert!(detach_pull_requests(&mut json!({})).is_empty());
    }

    #[tokio::test]
    async fn decodes_the_snapshot_first_and_the_links_after() {
        let json = shell_json(vec![
            thread_json("thread-a", Some(json!([link(1), link(2)]))),
            thread_json("thread-b", None),
            thread_json("thread-c", Some(json!([link(3)]))),
        ]);
        let deferred = decode_shell_snapshot_deferring_pull_requests(json).unwrap();
        assert_eq!(deferred.snapshot.threads.len(), 3);
        assert!(
            deferred
                .snapshot
                .threads
                .iter()
                .all(|thread| thread.pull_requests.is_none())
        );

        let links = deferred.load_pull_requests().await;
        assert_eq!(links.len(), 2);
        let numbers = |id: &str| -> Vec<u64> {
            links[&ThreadId::new(id).unwrap()]
                .iter()
                .map(|link| link.number.get())
                .collect()
        };
        assert_eq!(numbers("thread-a"), vec![1, 2]);
        assert_eq!(numbers("thread-c"), vec![3]);
        assert!(!links.contains_key(&ThreadId::new("thread-b").unwrap()));
    }

    #[tokio::test]
    async fn an_unreadable_thread_keeps_no_links_while_the_others_fill_in() {
        let json = shell_json(vec![
            thread_json("thread-a", Some(json!([{ "host": "github.com" }]))),
            thread_json("thread-b", Some(json!("not links"))),
            thread_json("thread-c", Some(json!([link(3)]))),
        ]);
        let deferred = decode_shell_snapshot_deferring_pull_requests(json).unwrap();
        let links = deferred.load_pull_requests().await;
        assert_eq!(
            links.keys().collect::<Vec<_>>(),
            vec![&ThreadId::new("thread-c").unwrap()]
        );
    }

    #[tokio::test]
    async fn decodes_large_link_sets_across_batches() {
        let many: Vec<Value> = (1..=1_000).map(link).collect();
        let json = shell_json(vec![
            thread_json("thread-a", Some(Value::Array(many.clone()))),
            thread_json("thread-b", Some(Value::Array(many))),
        ]);
        let deferred = decode_shell_snapshot_deferring_pull_requests(json).unwrap();
        let links = deferred.load_pull_requests().await;
        assert_eq!(links[&ThreadId::new("thread-a").unwrap()].len(), 1_000);
        assert_eq!(links[&ThreadId::new("thread-b").unwrap()].len(), 1_000);
    }

    #[test]
    fn a_snapshot_that_does_not_decode_is_an_error() {
        assert!(decode_shell_snapshot_deferring_pull_requests(json!({ "threads": [] })).is_err());
    }
}
