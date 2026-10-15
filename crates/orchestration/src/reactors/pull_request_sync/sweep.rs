//! Port of the sweep in upstream/apps/server/src/orchestration-v2/PullRequestSyncReactor.ts.
//!
//! One sweep reads each due pull request once, however many threads link it, and
//! writes back only what changed. The host reads go through [`SyncHost`] (T07's
//! `PullRequestService` behind an adapter), the thread state through
//! [`SyncProjections`], and the commands through [`ServerCommandDispatch`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicI64, Ordering};

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use parking_lot::Mutex;
use tokio::sync::Semaphore;
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::orchestration_v2::{OrchestrationV2Command, OrchestrationV2ServerCommand};
use turin_contracts::pull_request::{PullRequestRef, PullRequestSummary};
use turin_contracts::thread_pull_request::{
    ThreadPullRequestLink, ThreadPullRequestLinkSource, ThreadPullRequestSnapshot,
    ThreadPullRequestStack,
};
use turin_contracts::{CommandId, ProjectId, ThreadId, UtcDateTime};
use turin_shared::thread_pull_requests::{
    PullRequestRef as SharedRef, normalize_thread_pull_request_key, thread_pull_request_keys_equal,
    visible_thread_pull_requests,
};

use super::{
    LinkEntry, SnapshotFields, SyncCadence, is_unsettled, snapshot_fields_equal,
    snapshot_fields_of, stacks_equal, stored_snapshot_fields,
};
use crate::pipeline::orchestrator_port::BoxError;
use crate::projection::shared::thread_pull_requests::thread_pull_request_key_of;
use crate::projection::store::ProjectionStore;

/// Each group read runs at most this many at once, as wide as one batched summary read.
const SWEEP_CONCURRENCY: usize = 25;

/// A host read failed. `retry_at` is set when the host is rate limited and the failure
/// pauses that host's reads until then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncHostError {
    pub message: String,
    pub retry_at: Option<i64>,
}

impl fmt::Display for SyncHostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SyncHostError {}

/// The host reads a sweep makes. The T07 adapter converts a stack into the thread's shape
/// (`kind: native`, layers with number, branch and state).
#[async_trait]
pub trait SyncHost: Send + Sync {
    async fn invalidate(&self, reference: &PullRequestRef);
    async fn summary(
        &self,
        reference: &PullRequestRef,
    ) -> Result<PullRequestSummary, SyncHostError>;
    async fn stack(
        &self,
        reference: &PullRequestRef,
    ) -> Result<Option<ThreadPullRequestStack>, SyncHostError>;
}

/// The thread fields a sweep reads: its links, and whether it is still watched (`isUnsettled`).
pub struct SyncThread {
    pub thread_id: ThreadId,
    pub project_id: ProjectId,
    pub unsettled: bool,
    pub pull_requests: Vec<ThreadPullRequestLink>,
}

/// Every thread that has pull request links.
#[async_trait]
pub trait SyncProjections: Send + Sync {
    async fn threads_with_pull_requests(&self) -> Result<Vec<SyncThread>, BoxError>;
}

#[async_trait]
impl SyncProjections for ProjectionStore {
    async fn threads_with_pull_requests(&self) -> Result<Vec<SyncThread>, BoxError> {
        let threads = ProjectionStore::get_threads_with_pull_requests(self, None).await?;
        Ok(threads
            .into_iter()
            .map(|thread| SyncThread {
                unsettled: is_unsettled(&thread),
                thread_id: thread.id,
                project_id: thread.project_id,
                pull_requests: thread.pull_requests,
            })
            .collect())
    }
}

/// `engine.dispatch`: the sweep's link and link-sync commands.
#[async_trait]
pub trait ServerCommandDispatch: Send + Sync {
    async fn dispatch(&self, command: OrchestrationV2ServerCommand) -> Result<(), BoxError>;
}

/// `"all"` is the periodic pass. `"requested"` reads only keys asked for through `requestSync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepScope {
    All,
    Requested,
}

/// One thread's link, with what the sweep needs to act on it.
struct Entry {
    thread_id: ThreadId,
    project_id: ProjectId,
    unsettled: bool,
    /// Every link the thread holds, tombstones included, so a dismissed stack layer is never re-added.
    pull_requests: std::sync::Arc<Vec<ThreadPullRequestLink>>,
    link: ThreadPullRequestLink,
}

/// Time for one sweep, read once at its start.
struct SweepClock {
    now_ms: i64,
    now_iso: String,
}

pub struct PullRequestSyncReactor {
    host: std::sync::Arc<dyn SyncHost>,
    projections: std::sync::Arc<dyn SyncProjections>,
    dispatch: std::sync::Arc<dyn ServerCommandDispatch>,
    cadence: Mutex<SyncCadence>,
    /// Rate limit pauses, keyed by project and host, since each project reads with its own credential.
    paused_until: Mutex<HashMap<String, i64>>,
    request_generation: AtomicI64,
}

fn host_of(link: &ThreadPullRequestLink) -> String {
    normalize_thread_pull_request_key(
        link.host.as_str(),
        link.repository.as_str(),
        link.number.get(),
    )
    .host
}

fn reference_of(entry: &Entry) -> PullRequestRef {
    PullRequestRef {
        project_id: entry.project_id.clone(),
        host: Some(
            TrimmedNonEmptyString::new(host_of(&entry.link)).expect("host is a non-empty key"),
        ),
        expected_account_id: None,
        allow_stale: None,
        repository: entry.link.repository.clone(),
        number: entry.link.number,
    }
}

fn snapshot_of(fields: &SnapshotFields, synced_at: &str) -> ThreadPullRequestSnapshot {
    ThreadPullRequestSnapshot {
        state: fields.state,
        title: fields.title.clone(),
        head_branch: fields.head_branch.clone(),
        base_branch: fields.base_branch.clone(),
        is_draft: fields.is_draft,
        updated_at: fields.updated_at.clone(),
        synced_at: synced_at.to_owned(),
        closed_at: Some(fields.closed_at.clone()),
        merged_at: Some(fields.merged_at.clone()),
        author: fields.author.clone(),
        additions: fields.additions,
        deletions: fields.deletions,
        changed_files: fields.changed_files,
        review_decision: fields.review_decision,
        checks_state: fields.checks_state,
        mergeability: fields.mergeability,
    }
}

impl PullRequestSyncReactor {
    pub fn new(
        host: std::sync::Arc<dyn SyncHost>,
        projections: std::sync::Arc<dyn SyncProjections>,
        dispatch: std::sync::Arc<dyn ServerCommandDispatch>,
    ) -> Self {
        Self {
            host,
            projections,
            dispatch,
            cadence: Mutex::new(SyncCadence::default()),
            paused_until: Mutex::new(HashMap::new()),
            request_generation: AtomicI64::new(0),
        }
    }

    /// `requestSync`: the next requested sweep re-reads this pull request, even when its
    /// snapshot is terminal.
    pub fn request_sync(&self, key: &str) {
        let generation = self.request_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.cadence.lock().request(key, generation);
    }

    /// One sweep. Returns after every due group has been read and written.
    pub async fn sweep(&self, scope: SweepScope) -> Result<(), BoxError> {
        let threads = self.projections.threads_with_pull_requests().await?;
        let now = UtcDateTime::now();
        let clock = SweepClock {
            now_ms: now.as_datetime().timestamp_millis(),
            now_iso: now.to_string(),
        };

        let mut groups: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
        for thread in threads {
            let unsettled = thread.unsettled;
            let pull_requests = std::sync::Arc::new(thread.pull_requests);
            for link in visible_thread_pull_requests(&pull_requests) {
                groups
                    .entry(thread_pull_request_key_of(link))
                    .or_default()
                    .push(Entry {
                        thread_id: thread.thread_id.clone(),
                        project_id: thread.project_id.clone(),
                        unsettled,
                        pull_requests: pull_requests.clone(),
                        link: link.clone(),
                    });
            }
        }

        self.cadence.lock().prune(|key| groups.contains_key(key));

        let mut due: Vec<(&String, &Vec<Entry>)> = Vec::new();
        for (key, entries) in &groups {
            let views: Vec<LinkEntry<'_>> = entries
                .iter()
                .map(|entry| LinkEntry {
                    unsettled: entry.unsettled,
                    link: &entry.link,
                })
                .collect();
            let cadence = self.cadence.lock();
            let include = (scope == SweepScope::All || cadence.request_is_set(key))
                && cadence.is_due(key, &views, clock.now_ms);
            drop(cadence);
            if include {
                due.push((key, entries));
            }
        }

        // Layers auto-linked this sweep, so two links of one thread that share a stack do not
        // both try to add the same sibling. Sync writes are one at a time, as upstream's semaphore.
        let linked: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
        let persistence = Semaphore::new(1);
        let skips: Mutex<BTreeMap<String, (usize, String)>> = Mutex::new(BTreeMap::new());
        let clock = &clock;
        stream::iter(due)
            .map(|(key, entries)| {
                self.read_group_if_unpaused(key, entries, clock, &persistence, &linked, &skips)
            })
            .buffer_unordered(SWEEP_CONCURRENCY)
            .collect::<Vec<()>>()
            .await;

        // A host failure such as a signed-out CLI fails every due pull request the same way, so
        // a sweep reports one line per reason rather than one per pull request.
        for (reason, (count, key)) in skips.into_inner() {
            tracing::warn!(count, key = %key, reason = %reason, "pull request sync skipped");
        }
        Ok(())
    }

    async fn read_group_if_unpaused(
        &self,
        key: &str,
        entries: &[Entry],
        clock: &SweepClock,
        persistence: &Semaphore,
        linked: &Mutex<HashSet<String>>,
        skips: &Mutex<BTreeMap<String, (usize, String)>>,
    ) {
        let first = &entries[0];
        let pause_key = format!("{}\0{}", first.project_id, host_of(&first.link));
        // Checked against the clock as each read starts, so a pause found earlier in this sweep
        // holds the rest, and one that ends during the sweep lets the rest through.
        let started_ms = UtcDateTime::now().as_datetime().timestamp_millis();
        if self
            .paused_until
            .lock()
            .get(&pause_key)
            .copied()
            .unwrap_or(0)
            > started_ms
        {
            return;
        }
        let Err(error) = self
            .read_group(key, entries, clock, persistence, linked)
            .await
        else {
            return;
        };
        if let Some(retry_at) = error.retry_at {
            let mut paused = self.paused_until.lock();
            let current = paused.get(&pause_key).copied().unwrap_or(0);
            paused.insert(pause_key, retry_at.max(current));
        }
        let mut skips = skips.lock();
        let (count, _) = skips
            .entry(error.message.clone())
            .or_insert((0, key.to_owned()));
        *count += 1;
    }

    async fn read_group(
        &self,
        key: &str,
        entries: &[Entry],
        clock: &SweepClock,
        persistence: &Semaphore,
        linked: &Mutex<HashSet<String>>,
    ) -> Result<(), SyncHostError> {
        let reference = reference_of(&entries[0]);
        let generation = self.cadence.lock().requested_generation(key);
        if generation.is_some() {
            self.host.invalidate(&reference).await;
        }
        let summary = self.host.summary(&reference).await?;
        let fields = snapshot_fields_of(&summary);
        // `None`: the summary carries no stack field. `Some(None)`: it says the pull request is in
        // no stack. `Some(Some(n))`: it is in stack `n`.
        let summary_stack = summary
            .stack
            .as_ref()
            .map(|membership| membership.as_ref().map(|member| member.number.get()));
        let link_stack_number = |entry: &Entry| entry.link.stack.as_ref().map(|s| s.number.get());
        let needs_stack = generation.is_some()
            || self.cadence.lock().retry_set(key)
            || entries.iter().any(|entry| {
                entry.link.snapshot.as_ref().is_none_or(|snapshot| {
                    !snapshot_fields_equal(&stored_snapshot_fields(snapshot), &fields)
                }) || summary_stack.is_some_and(|number| link_stack_number(entry) != number)
            });
        // A summary that says the pull request is in no stack, for links that hold none, already
        // answers what the stack read would.
        let known_unstacked =
            summary_stack == Some(None) && entries.iter().all(|entry| entry.link.stack.is_none());
        let fetched: Option<Option<ThreadPullRequestStack>> = if !needs_stack {
            None
        } else if known_unstacked {
            Some(None)
        } else {
            match self.host.stack(&reference).await {
                Ok(stack) => Some(stack),
                // The host paused reads: keep this key due and let the pause end first.
                Err(error) if error.retry_at.is_some() => {
                    self.cadence.lock().set_retry(key, true);
                    return Err(error);
                }
                // A failed read is not "no stack": keep the key due and write nothing for it.
                Err(error) => {
                    tracing::warn!(key, error = %error, "pull request stack lookup failed");
                    self.cadence.lock().set_retry(key, true);
                    return Ok(());
                }
            }
        };
        if needs_stack {
            self.cadence.lock().set_retry(key, false);
        }

        // The host answered, so the cadence clock ticks even if a dispatch below is rejected.
        {
            let mut cadence = self.cadence.lock();
            cadence.record_sync(key, clock.now_ms);
            // A refresh requested while the host read was in flight belongs to the next sweep.
            cadence.clear_request_if(key, generation);
        }

        for entry in entries {
            let _permit = persistence
                .acquire()
                .await
                .expect("sync semaphore stays open");
            if let Err(error) = self
                .sync_entry(entry, &fields, &fetched, clock, linked)
                .await
            {
                self.cadence.lock().set_retry(key, true);
                tracing::warn!(
                    thread_id = %entry.thread_id,
                    key,
                    error = %error,
                    "pull request sync skipped"
                );
            }
        }
        Ok(())
    }

    async fn sync_entry(
        &self,
        entry: &Entry,
        fields: &SnapshotFields,
        fetched: &Option<Option<ThreadPullRequestStack>>,
        clock: &SweepClock,
        linked: &Mutex<HashSet<String>>,
    ) -> Result<(), BoxError> {
        let next_stack: Option<ThreadPullRequestStack> = match fetched {
            Some(stack) => stack.clone(),
            None => entry.link.stack.clone(),
        };
        let changed = entry.link.snapshot.as_ref().is_none_or(|snapshot| {
            !snapshot_fields_equal(&stored_snapshot_fields(snapshot), fields)
        }) || !stacks_equal(entry.link.stack.as_ref(), next_stack.as_ref());

        // Persist discovered siblings before a terminal snapshot can trigger settlement. A settled
        // thread that shares this pull request with an active one takes the fresh snapshot, but
        // gains no links.
        if let Some(Some(stack)) = fetched.as_ref().filter(|_| entry.unsettled) {
            let host = host_of(&entry.link);
            for layer in &stack.layers {
                let layer_key = normalize_thread_pull_request_key(
                    &host,
                    entry.link.repository.as_str(),
                    layer.number.get(),
                );
                let dedupe_key = format!(
                    "{}:{}/{}#{}",
                    entry.thread_id, layer_key.host, layer_key.repository, layer_key.number
                );
                if linked.lock().contains(&dedupe_key) {
                    continue;
                }
                // Tombstones count as present: a dismissed layer is never re-added.
                let already_held = entry.pull_requests.iter().any(|existing| {
                    thread_pull_request_keys_equal(
                        SharedRef::from(existing),
                        SharedRef {
                            host: &layer_key.host,
                            repository: &layer_key.repository,
                            number: layer_key.number,
                        },
                    )
                });
                if already_held {
                    continue;
                }
                let Some(url) = turin_shared::change_request_url::sibling_pull_request_url(
                    entry.link.url.as_str(),
                    layer.number.get(),
                ) else {
                    continue;
                };
                let uuid = uuid::Uuid::new_v4();
                self.dispatch
                    .dispatch(OrchestrationV2ServerCommand::Client(
                        OrchestrationV2Command::ThreadPullRequestLink {
                            command_id: CommandId::new(format!(
                                "server:pr-stack-link:{}:{uuid}",
                                entry.thread_id
                            ))?,
                            thread_id: entry.thread_id.clone(),
                            host: TrimmedNonEmptyString::new(&layer_key.host)?,
                            repository: TrimmedNonEmptyString::new(&layer_key.repository)?,
                            number: layer.number,
                            url: TrimmedNonEmptyString::new(url)?,
                            source: ThreadPullRequestLinkSource::Stack,
                        },
                    ))
                    .await?;
                linked.lock().insert(dedupe_key);
            }
        }

        if changed {
            let uuid = uuid::Uuid::new_v4();
            self.dispatch
                .dispatch(OrchestrationV2ServerCommand::Client(
                    OrchestrationV2Command::ThreadPullRequestLinkSync {
                        command_id: CommandId::new(format!(
                            "server:pr-sync:{}:{uuid}",
                            entry.thread_id
                        ))?,
                        thread_id: entry.thread_id.clone(),
                        host: TrimmedNonEmptyString::new(host_of(&entry.link))?,
                        repository: entry.link.repository.clone(),
                        number: entry.link.number,
                        snapshot: snapshot_of(fields, &clock.now_iso),
                        stack: next_stack,
                    },
                ))
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
