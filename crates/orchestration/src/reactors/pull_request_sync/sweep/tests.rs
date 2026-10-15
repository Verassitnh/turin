//! Behaviour tests for the sweep, against fake host, projections and dispatch. Covers the
//! cases that PullRequestSyncReactor.test.ts exercises through the Effect runtime.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{Value, json};
use turin_contracts::orchestration_v2::{OrchestrationV2Command, OrchestrationV2ServerCommand};
use turin_contracts::pull_request::{PullRequestRef, PullRequestSummary};
use turin_contracts::thread_pull_request::{ThreadPullRequestLink, ThreadPullRequestStack};
use turin_contracts::{ProjectId, ThreadId};

use super::*;

const PROJECT: &str = "project:test";
const REPO: &str = "pingdotgg/t3code";
const UPDATED: &str = "2026-10-02T12:00:00.000Z";

fn link_json(number: u64, snapshot: Value) -> Value {
    json!({
        "host": "github.com",
        "repository": REPO,
        "number": number,
        "url": format!("https://github.com/{REPO}/pull/{number}"),
        "source": "agent",
        "linkedAt": UPDATED,
        "snapshot": snapshot,
        "stack": null,
    })
}

fn link(number: u64, snapshot: Option<Value>) -> ThreadPullRequestLink {
    serde_json::from_value(link_json(number, snapshot.unwrap_or(Value::Null)))
        .expect("link fixture decodes")
}

fn snapshot_json(state: &str, title: &str) -> Value {
    json!({
        "state": state,
        "title": title,
        "headBranch": "feature",
        "baseBranch": "main",
        "isDraft": false,
        "updatedAt": UPDATED,
        "syncedAt": UPDATED,
    })
}

fn summary(number: u64, state: &str, title: &str) -> PullRequestSummary {
    serde_json::from_value(json!({
        "provider": "github",
        "projectId": PROJECT,
        "repository": REPO,
        "number": number,
        "title": title,
        "url": format!("https://github.com/{REPO}/pull/{number}"),
        "state": state,
        "isDraft": false,
        "headBranch": "feature",
        "baseBranch": "main",
        "closedAt": null,
        "mergedAt": null,
        "updatedAt": UPDATED,
    }))
    .expect("summary fixture decodes")
}

fn stack(numbers: &[u64]) -> ThreadPullRequestStack {
    let layers: Vec<Value> = numbers
        .iter()
        .map(|number| json!({ "number": number, "headBranch": format!("branch-{number}"), "state": "open" }))
        .collect();
    serde_json::from_value(json!({
        "kind": "native",
        "id": "stack-1",
        "number": numbers[0],
        "url": format!("https://github.com/{REPO}/pull/{}", numbers[0]),
        "base": "main",
        "layers": layers,
    }))
    .expect("stack fixture decodes")
}

#[derive(Clone)]
struct ThreadSpec {
    thread_id: &'static str,
    unsettled: bool,
    links: Vec<ThreadPullRequestLink>,
}

#[derive(Default)]
struct FakeHost {
    summaries: Mutex<HashMap<u64, PullRequestSummary>>,
    failures: Mutex<HashMap<u64, SyncHostError>>,
    stacks: Mutex<HashMap<u64, ThreadPullRequestStack>>,
    summary_reads: Mutex<Vec<u64>>,
    invalidated: Mutex<Vec<u64>>,
}

#[async_trait]
impl SyncHost for FakeHost {
    async fn invalidate(&self, reference: &PullRequestRef) {
        self.invalidated.lock().push(reference.number.get());
    }

    async fn summary(
        &self,
        reference: &PullRequestRef,
    ) -> Result<PullRequestSummary, SyncHostError> {
        let number = reference.number.get();
        self.summary_reads.lock().push(number);
        if let Some(error) = self.failures.lock().get(&number) {
            return Err(error.clone());
        }
        Ok(self
            .summaries
            .lock()
            .get(&number)
            .cloned()
            .expect("fixture has a summary for this pull request"))
    }

    async fn stack(
        &self,
        reference: &PullRequestRef,
    ) -> Result<Option<ThreadPullRequestStack>, SyncHostError> {
        Ok(self.stacks.lock().get(&reference.number.get()).cloned())
    }
}

#[derive(Default)]
struct FakeProjections {
    threads: Mutex<Vec<ThreadSpec>>,
}

#[async_trait]
impl SyncProjections for FakeProjections {
    async fn threads_with_pull_requests(&self) -> Result<Vec<SyncThread>, BoxError> {
        Ok(self
            .threads
            .lock()
            .iter()
            .map(|spec| SyncThread {
                thread_id: ThreadId::new(spec.thread_id).expect("thread id"),
                project_id: ProjectId::new(PROJECT).expect("project id"),
                unsettled: spec.unsettled,
                pull_requests: spec.links.clone(),
            })
            .collect())
    }
}

#[derive(Default)]
struct FakeDispatch {
    commands: Mutex<Vec<OrchestrationV2ServerCommand>>,
}

#[async_trait]
impl ServerCommandDispatch for FakeDispatch {
    async fn dispatch(&self, command: OrchestrationV2ServerCommand) -> Result<(), BoxError> {
        self.commands.lock().push(command);
        Ok(())
    }
}

struct Harness {
    reactor: PullRequestSyncReactor,
    host: Arc<FakeHost>,
    projections: Arc<FakeProjections>,
    dispatch: Arc<FakeDispatch>,
}

fn harness(threads: Vec<ThreadSpec>) -> Harness {
    let host = Arc::new(FakeHost::default());
    let projections = Arc::new(FakeProjections {
        threads: Mutex::new(threads),
    });
    let dispatch = Arc::new(FakeDispatch::default());
    let reactor = PullRequestSyncReactor::new(host.clone(), projections.clone(), dispatch.clone());
    Harness {
        reactor,
        host,
        projections,
        dispatch,
    }
}

fn thread(
    thread_id: &'static str,
    unsettled: bool,
    links: Vec<ThreadPullRequestLink>,
) -> ThreadSpec {
    ThreadSpec {
        thread_id,
        unsettled,
        links,
    }
}

fn link_commands(dispatch: &FakeDispatch) -> Vec<OrchestrationV2Command> {
    dispatch
        .commands
        .lock()
        .iter()
        .filter_map(|command| match command {
            OrchestrationV2ServerCommand::Client(
                command @ OrchestrationV2Command::ThreadPullRequestLink { .. },
            ) => Some(command.clone()),
            _ => None,
        })
        .collect()
}

fn sync_commands(dispatch: &FakeDispatch) -> Vec<OrchestrationV2Command> {
    dispatch
        .commands
        .lock()
        .iter()
        .filter_map(|command| match command {
            OrchestrationV2ServerCommand::Client(
                command @ OrchestrationV2Command::ThreadPullRequestLinkSync { .. },
            ) => Some(command.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_unchanged_snapshot_writes_nothing() {
    let h = harness(vec![thread(
        "thread:one",
        true,
        vec![link(42, Some(snapshot_json("open", "Same title")))],
    )]);
    h.host
        .summaries
        .lock()
        .insert(42, summary(42, "open", "Same title"));

    h.periodic_pass().await;

    assert!(sync_commands(&h.dispatch).is_empty());
}

#[tokio::test]
async fn a_link_without_a_snapshot_is_synced_once() {
    let h = harness(vec![thread("thread:one", true, vec![link(42, None)])]);
    h.host
        .summaries
        .lock()
        .insert(42, summary(42, "open", "New title"));

    h.periodic_pass().await;

    let commands = sync_commands(&h.dispatch);
    assert_eq!(commands.len(), 1);
    let OrchestrationV2Command::ThreadPullRequestLinkSync {
        snapshot, number, ..
    } = &commands[0]
    else {
        panic!("expected a link sync");
    };
    assert_eq!(number.get(), 42);
    assert_eq!(snapshot.title.as_str(), "New title");
    // The sweep stamps syncedAt from its own clock, as an RFC 3339 UTC millisecond string.
    assert!(snapshot.synced_at.ends_with("Z") && snapshot.synced_at.contains('T'));
}

#[tokio::test]
async fn a_merged_only_set_is_not_read_on_the_periodic_pass() {
    let h = harness(vec![thread(
        "thread:one",
        true,
        vec![link(42, Some(snapshot_json("merged", "Merged")))],
    )]);
    h.host
        .summaries
        .lock()
        .insert(42, summary(42, "merged", "Merged"));

    h.reactor.sweep(SweepScope::All).await.expect("sweep");

    assert!(h.host.summary_reads.lock().is_empty());
}

#[tokio::test]
async fn an_explicit_request_reads_a_merged_pull_request_and_invalidates_it() {
    let h = harness(vec![thread(
        "thread:one",
        true,
        vec![link(42, Some(snapshot_json("merged", "Merged")))],
    )]);
    h.host
        .summaries
        .lock()
        .insert(42, summary(42, "merged", "Merged"));
    let key = thread_pull_request_key_of(&link(42, None));

    h.reactor.request_sync(&key);
    h.reactor.sweep(SweepScope::Requested).await.expect("sweep");

    assert_eq!(h.host.invalidated.lock().as_slice(), &[42]);
    assert_eq!(h.host.summary_reads.lock().as_slice(), &[42]);
}

#[tokio::test]
async fn a_closed_link_waits_for_the_slow_interval_after_its_last_read() {
    let h = harness(vec![thread(
        "thread:one",
        true,
        vec![link(42, Some(snapshot_json("closed", "Closed")))],
    )]);
    h.host
        .summaries
        .lock()
        .insert(42, summary(42, "closed", "Closed"));

    h.reactor.sweep(SweepScope::All).await.expect("first sweep");
    h.reactor
        .sweep(SweepScope::All)
        .await
        .expect("second sweep");

    assert_eq!(h.host.summary_reads.lock().as_slice(), &[42]);
}

#[tokio::test]
async fn a_newly_stacked_layer_on_an_unsettled_thread_is_auto_linked() {
    let h = harness(vec![thread("thread:one", true, vec![link(42, None)])]);
    h.host
        .summaries
        .lock()
        .insert(42, summary(42, "open", "Stacked"));
    h.host.stacks.lock().insert(42, stack(&[42, 43]));

    h.reactor.sweep(SweepScope::All).await.expect("sweep");

    let links = link_commands(&h.dispatch);
    assert_eq!(links.len(), 1, "only the sibling layer is linked");
    let OrchestrationV2Command::ThreadPullRequestLink { number, source, .. } = &links[0] else {
        panic!("expected a link");
    };
    assert_eq!(number.get(), 43);
    assert_eq!(
        *source,
        turin_contracts::thread_pull_request::ThreadPullRequestLinkSource::Stack
    );
    let syncs = sync_commands(&h.dispatch);
    assert_eq!(syncs.len(), 1);
}

#[tokio::test]
async fn a_settled_thread_gains_no_stack_links() {
    let h = harness(vec![thread("thread:one", false, vec![link(42, None)])]);
    h.host
        .summaries
        .lock()
        .insert(42, summary(42, "open", "Stacked"));
    h.host.stacks.lock().insert(42, stack(&[42, 43]));

    h.reactor.sweep(SweepScope::All).await.expect("sweep");

    assert!(link_commands(&h.dispatch).is_empty());
    assert_eq!(
        sync_commands(&h.dispatch).len(),
        1,
        "the settled thread still takes the fresh snapshot"
    );
}

#[tokio::test]
async fn a_rate_limited_host_pauses_its_other_reads_until_retry_at() {
    let h = harness(vec![thread("thread:one", true, vec![link(42, None)])]);
    let retry_at = UtcDateTime::now().as_datetime().timestamp_millis() + 3_600_000;
    h.host.failures.lock().insert(
        42,
        SyncHostError {
            message: "rate limited".to_owned(),
            retry_at: Some(retry_at),
        },
    );
    h.reactor.sweep(SweepScope::All).await.expect("sweep");

    // A second thread on the same project and host is not read while the pause holds.
    h.projections
        .threads
        .lock()
        .push(thread("thread:two", true, vec![link(43, None)]));
    h.host
        .summaries
        .lock()
        .insert(43, summary(43, "open", "Other"));
    h.reactor.sweep(SweepScope::All).await.expect("sweep");

    assert!(!h.host.summary_reads.lock().contains(&43));
}

#[tokio::test]
async fn one_failed_group_does_not_stop_the_others() {
    let h = harness(vec![
        thread("thread:one", true, vec![link(42, None)]),
        thread("thread:two", true, vec![link(43, None)]),
    ]);
    h.host.failures.lock().insert(
        42,
        SyncHostError {
            message: "not found".to_owned(),
            retry_at: None,
        },
    );
    h.host
        .summaries
        .lock()
        .insert(43, summary(43, "open", "Fine"));

    h.reactor
        .sweep(SweepScope::All)
        .await
        .expect("sweep succeeds");

    let syncs = sync_commands(&h.dispatch);
    assert_eq!(syncs.len(), 1);
    let OrchestrationV2Command::ThreadPullRequestLinkSync { number, .. } = &syncs[0] else {
        panic!("expected a link sync");
    };
    assert_eq!(number.get(), 43);
}

impl Harness {
    /// One periodic pass over freshly loaded threads.
    async fn periodic_pass(&self) {
        self.reactor.sweep(SweepScope::All).await.expect("sweep");
    }
}
