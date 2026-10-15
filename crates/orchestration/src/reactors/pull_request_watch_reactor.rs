//! Port of upstream/apps/server/src/orchestration-v2/PullRequestWatchReactor.ts
//!
//! Wakes a thread's agent when a pull request it watches (`watch_pull_request`) needs a look:
//! checks finished on the head commit, someone else commented, or the branch started to
//! conflict. A pass every two minutes reads each watched pull request once for all the threads of
//! a project that watch it. Where the host has a fingerprint, one batched read tells the pass
//! which pull requests moved; elsewhere a pull request is read unless its sync snapshot has not
//! moved while nothing is in flight.
//!
//! The evaluation is in [`super::pull_request_watch`]. This module decides what to read, reads
//! through [`WatchHost`], and records the result through [`ServerCommandDispatch`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde::Serialize;
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::orchestration_v2::{
    OrchestrationV2InternalCommand, OrchestrationV2ServerCommand, OrchestrationV2WakeMessage,
};
use turin_contracts::pull_request::{
    PullRequestActivity, PullRequestComment, PullRequestCommentKind, PullRequestDetail,
    PullRequestMergeability, PullRequestRef, PullRequestState, PullRequestThreadComment,
    PullRequestThreadCommentsResult,
};
use turin_contracts::thread_pull_request::{ThreadPullRequestLink, ThreadPullRequestWatch};
use turin_contracts::{CommandId, MessageId, ProjectId, ThreadId, UtcDateTime};
use turin_shared::thread_pull_requests::{
    normalize_thread_pull_request_key, visible_thread_pull_requests,
};
use turin_source_control::source_control_core::pull_request_provider::ProviderChangeRequestWatchFingerprint;

use super::pull_request_sync::sweep::ServerCommandDispatch;
use super::pull_request_watch::{
    PullRequestWatchMessageInput, WatchDetail, evaluate_pull_request_watch,
    pull_request_watch_message,
};
use crate::pipeline::orchestrator_port::BoxError;
use crate::projection::shared::thread_pull_requests::thread_pull_request_key_of;

/// Minutes between passes. Checks take minutes, so a faster pass mostly spends the host's rate
/// limit, which every machine on the same account shares.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(2 * 60);
/// Reads in a row that failed for a reason other than a rate limit before the watch ends.
pub const READ_FAILURE_LIMIT: u32 = 8;
/// Without a host fingerprint, a quiet pull request is read again after this long, for news the
/// sync snapshot cannot show, such as a bot editing its review.
pub const QUIET_REREAD_MS: i64 = 10 * 60_000;
/// With a host fingerprint, how long until the activity is read anyway: the fingerprint does not
/// see edits to comments inside review threads.
pub const FINGERPRINT_REREAD_MS: i64 = 30 * 60_000;

/// Why a watch ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchEndReason {
    Merged,
    Closed,
    Unreadable,
    CommentLimit,
    Settled,
    Subagent,
    /// Anything outside this reactor: the agent unwatched, the user stopped it, or the thread
    /// settled, archived, or was deleted between passes.
    Stopped,
}

impl WatchEndReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Merged => "merged",
            Self::Closed => "closed",
            Self::Unreadable => "unreadable",
            Self::CommentLimit => "comment-limit",
            Self::Settled => "settled",
            Self::Subagent => "subagent",
            Self::Stopped => "stopped",
        }
    }
}

/// The host fingerprint (`watchFingerprint`): what the host reports changed, as two digests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub status: String,
    pub remarks: String,
}

impl From<ProviderChangeRequestWatchFingerprint> for Fingerprint {
    fn from(value: ProviderChangeRequestWatchFingerprint) -> Self {
        Self {
            status: value.status,
            remarks: value.remarks,
        }
    }
}

/// Which reads a pass makes for one pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadPlan {
    pub detail: bool,
    pub activity: bool,
}

/// The last successful read of a pull request, kept in memory: a restart reads each once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastRead {
    /// When the activity was last read.
    pub at: i64,
    /// The sync snapshot fingerprint this read saw.
    pub fingerprint: String,
    /// A check was still running or mergeability unknown, so the detail can move unannounced.
    pub in_flight: bool,
    /// The last activity read listed every remark; an incomplete one is read again.
    pub remarks_complete: bool,
    /// The watches this read evaluated; a watch started since takes its first look next pass.
    pub watches: HashSet<String>,
    /// The host fingerprint parts these reads answer, when the host gives one.
    pub status: Option<String>,
    pub remarks: Option<String>,
}

/// The sync snapshot fingerprint of a link: what sync last saw, joined as upstream joins it.
/// An absent value is an empty field.
pub fn snapshot_fingerprint(link: &ThreadPullRequestLink) -> String {
    let Some(snapshot) = &link.snapshot else {
        return String::new();
    };
    let wire = |value: Option<String>| value.unwrap_or_default();
    [
        wire(wire_name(&snapshot.state)),
        snapshot.updated_at.clone().unwrap_or_default(),
        wire(snapshot.checks_state.flatten().as_ref().and_then(wire_name)),
        wire(snapshot.mergeability.as_ref().and_then(wire_name)),
        wire(
            snapshot
                .review_decision
                .flatten()
                .as_ref()
                .and_then(wire_name),
        ),
        snapshot.is_draft.to_string(),
    ]
    .join(" ")
}

fn wire_name<T: Serialize>(value: &T) -> Option<String> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
}

/// `planRead`: with a host fingerprint, the detail is read when anything moved or a check is still
/// running, and the activity only when the remarks moved. Without one, both are read whenever the
/// sync snapshot moved or something is in flight. A watch that is new to this read takes its
/// first look with everything.
pub fn plan_read(
    group_fingerprint: &str,
    watch_keys: &[String],
    last: Option<&LastRead>,
    fingerprint: Option<&Fingerprint>,
    now: i64,
) -> ReadPlan {
    let Some(last) = last else {
        return ReadPlan {
            detail: true,
            activity: true,
        };
    };
    if watch_keys.iter().any(|key| !last.watches.contains(key)) {
        return ReadPlan {
            detail: true,
            activity: true,
        };
    }
    match fingerprint {
        None => {
            let read = last.in_flight
                || !last.remarks_complete
                || last.fingerprint != group_fingerprint
                || now - last.at >= QUIET_REREAD_MS;
            ReadPlan {
                detail: read,
                activity: read,
            }
        }
        Some(fingerprint) => {
            let activity = last.remarks.as_deref() != Some(fingerprint.remarks.as_str())
                || !last.remarks_complete
                || now - last.at >= FINGERPRINT_REREAD_MS;
            ReadPlan {
                detail: activity
                    || last.status.as_deref() != Some(fingerprint.status.as_str())
                    || last.in_flight,
                activity,
            }
        }
    }
}

/// `endsWithoutRead`: why a watch ends before any host read. A merged pull request cannot reopen.
/// Settling and archiving end watches, and a subagent cannot start one; a watch left from before
/// those rules ends here.
pub fn ends_without_read(
    snapshot_state: Option<PullRequestState>,
    settled: bool,
    subagent: bool,
) -> Option<WatchEndReason> {
    if snapshot_state == Some(PullRequestState::Merged) {
        Some(WatchEndReason::Merged)
    } else if settled {
        Some(WatchEndReason::Settled)
    } else if subagent {
        Some(WatchEndReason::Subagent)
    } else {
        None
    }
}

/// Field-by-field equality of two watches, with the list fields compared by their joined text.
pub fn watches_equal(left: &ThreadPullRequestWatch, right: &ThreadPullRequestWatch) -> bool {
    let joined = |values: &[TrimmedNonEmptyString]| {
        values
            .iter()
            .map(TrimmedNonEmptyString::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    };
    left.started_at == right.started_at
        && left.head_sha == right.head_sha
        && joined(&left.failed_checks) == joined(&right.failed_checks)
        && left.passed == right.passed
        && joined(&left.passed_checks) == joined(&right.passed_checks)
        && left.remarks_through == right.remarks_through
        && joined(&left.remark_ids) == joined(&right.remark_ids)
        && left.conflicting == right.conflicting
        && left.wakes == right.wakes
}

/// A thread as the watch pass reads it: its identity, whether it is still watched, and its links.
pub struct WatchThread {
    pub thread_id: ThreadId,
    pub project_id: ProjectId,
    pub settled: bool,
    pub subagent: bool,
    pub pull_requests: Vec<ThreadPullRequestLink>,
}

/// Every thread with pull request links, as the watch pass needs them.
#[async_trait]
pub trait WatchProjections: Send + Sync {
    async fn threads(&self) -> Result<Vec<WatchThread>, BoxError>;
}

/// A host read that failed. `rate_limited` pauses the pass, since the host refuses every read until
/// its window ends; anything else counts toward giving up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchHostError {
    pub message: String,
    pub rate_limited: bool,
}

impl std::fmt::Display for WatchHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WatchHostError {}

/// The host reads a watch pass makes.
#[async_trait]
pub trait WatchHost: Send + Sync {
    async fn invalidate(&self, reference: &PullRequestRef);
    async fn detail(&self, reference: &PullRequestRef)
    -> Result<PullRequestDetail, WatchHostError>;
    async fn activity(
        &self,
        reference: &PullRequestRef,
    ) -> Result<PullRequestActivity, WatchHostError>;
    async fn thread_comments(
        &self,
        reference: &PullRequestRef,
        thread_id: &TrimmedNonEmptyString,
        cursor: &TrimmedNonEmptyString,
    ) -> Result<PullRequestThreadCommentsResult, WatchHostError>;
    async fn watch_fingerprint(
        &self,
        reference: &PullRequestRef,
    ) -> Result<Option<Fingerprint>, WatchHostError>;
}

/// One watched link, with the thread it belongs to.
#[derive(Clone)]
struct WatchTarget {
    thread_id: ThreadId,
    project_id: ProjectId,
    link: ThreadPullRequestLink,
    watch: ThreadPullRequestWatch,
}

impl WatchTarget {
    fn key(&self) -> String {
        format!("{} {}", self.thread_id, self.watch.started_at)
    }

    fn life_key(&self) -> String {
        format!(
            "{} {} {}",
            self.thread_id,
            thread_pull_request_key_of(&self.link),
            self.watch.started_at
        )
    }
}

/// One project's watches on one pull request, read once per pass.
struct WatchGroup {
    key: String,
    targets: Vec<WatchTarget>,
    fingerprint: String,
}

/// What one watch did while this process ran, logged once when it ends.
#[derive(Clone)]
struct WatchLife {
    thread_id: ThreadId,
    pull_request: String,
    started_ms: i64,
    head_sha: Option<String>,
    pushed_ms: i64,
    longest_quiet_ms: i64,
    wakes: u64,
    reads: u64,
}

fn minutes(ms: i64) -> i64 {
    (ms.max(0) as f64 / 60_000.0).round() as i64
}

/// Which pass a host fingerprint result asks for. `Paused` skips the pass while the host is
/// rate limited, since every read would be refused.
enum FingerprintRead {
    Read(Option<Fingerprint>),
    Paused,
}

pub struct PullRequestWatchReactor {
    host: Arc<dyn WatchHost>,
    projections: Arc<dyn WatchProjections>,
    dispatch: Arc<dyn ServerCommandDispatch>,
    booted_ms: i64,
    lives: Mutex<HashMap<String, WatchLife>>,
    read_failures: Mutex<HashMap<String, u32>>,
    last_reads: Mutex<HashMap<String, LastRead>>,
    /// Replies past each long thread's first page, per pull request, so a pass pages a thread again
    /// only when the host's count of it moves.
    thread_tails: Mutex<ThreadTails>,
}

/// Per pull request, per review thread: the reply count the tail was read at, and its replies.
type ThreadTails = HashMap<String, HashMap<String, (u64, Vec<PullRequestComment>)>>;

fn host_of(link: &ThreadPullRequestLink) -> String {
    normalize_thread_pull_request_key(
        link.host.as_str(),
        link.repository.as_str(),
        link.number.get(),
    )
    .host
}

fn reference_of(project_id: &ProjectId, link: &ThreadPullRequestLink) -> PullRequestRef {
    PullRequestRef {
        project_id: project_id.clone(),
        host: Some(TrimmedNonEmptyString::new(host_of(link)).expect("host is a non-empty key")),
        expected_account_id: None,
        allow_stale: None,
        repository: link.repository.clone(),
        number: link.number,
    }
}

impl PullRequestWatchReactor {
    pub fn new(
        host: Arc<dyn WatchHost>,
        projections: Arc<dyn WatchProjections>,
        dispatch: Arc<dyn ServerCommandDispatch>,
        booted_at: UtcDateTime,
    ) -> Self {
        Self {
            host,
            projections,
            dispatch,
            booted_ms: booted_at.as_datetime().timestamp_millis(),
            lives: Mutex::new(HashMap::new()),
            read_failures: Mutex::new(HashMap::new()),
            last_reads: Mutex::new(HashMap::new()),
            thread_tails: Mutex::new(HashMap::new()),
        }
    }

    /// One pass over every watched pull request.
    pub async fn sweep(&self) -> Result<(), BoxError> {
        let threads = self.projections.threads().await?;
        let mut targets: Vec<WatchTarget> = Vec::new();
        let mut ending: Vec<(WatchTarget, WatchEndReason)> = Vec::new();
        for thread in threads {
            for link in visible_thread_pull_requests(&thread.pull_requests) {
                let Some(watch) = link.watch.clone() else {
                    continue;
                };
                let target = WatchTarget {
                    thread_id: thread.thread_id.clone(),
                    project_id: thread.project_id.clone(),
                    link: link.clone(),
                    watch,
                };
                match ends_without_read(
                    link.snapshot.as_ref().map(|snapshot| snapshot.state),
                    thread.settled,
                    thread.subagent,
                ) {
                    Some(reason) => ending.push((target.clone(), reason)),
                    None => targets.push(target),
                }
            }
        }

        // A watch seen last pass and gone now was ended outside this reactor.
        let present: HashSet<String> = targets
            .iter()
            .chain(ending.iter().map(|(target, _)| target))
            .map(WatchTarget::life_key)
            .collect();
        let gone: Vec<(String, WatchLife)> = self
            .lives
            .lock()
            .iter()
            .filter(|(key, _)| !present.contains(*key))
            .map(|(key, life)| (key.clone(), life.clone()))
            .collect();
        for (key, life) in gone {
            self.report_end(&key, &life, WatchEndReason::Stopped);
        }
        for target in targets
            .iter()
            .chain(ending.iter().map(|(target, _)| target))
        {
            self.life_of(target);
        }

        // Grouped per project too: each project reads through its own checkout, so one that cannot
        // read the pull request must not end another project's watches.
        let mut by_pull_request: IndexMap<String, Vec<WatchTarget>> = IndexMap::new();
        for target in targets {
            let key = format!(
                "{} {}",
                target.project_id,
                thread_pull_request_key_of(&target.link)
            );
            by_pull_request.entry(key).or_default().push(target);
        }
        let groups: Vec<WatchGroup> = by_pull_request
            .into_iter()
            .map(|(key, members)| {
                let fingerprint = members
                    .iter()
                    .map(snapshot_fingerprint_of)
                    .collect::<std::collections::BTreeSet<String>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join("\n");
                WatchGroup {
                    key,
                    targets: members,
                    fingerprint,
                }
            })
            .collect();
        let live_pull_requests: HashSet<String> =
            groups.iter().map(|group| group.key.clone()).collect();
        self.read_failures
            .lock()
            .retain(|key, _| live_pull_requests.contains(key));
        self.last_reads
            .lock()
            .retain(|key, _| live_pull_requests.contains(key));
        self.thread_tails
            .lock()
            .retain(|key, _| live_pull_requests.contains(key));

        for (target, reason) in ending {
            self.stop(&target, reason).await;
        }

        let fingerprints: Vec<FingerprintRead> = futures::stream::iter(groups.iter())
            .map(|group| self.fingerprint_of(group))
            .buffered(groups.len().max(1))
            .collect()
            .await;
        futures::stream::iter(groups.iter().zip(fingerprints))
            .filter_map(|(group, fingerprint)| async move {
                match fingerprint {
                    FingerprintRead::Paused => None,
                    FingerprintRead::Read(fingerprint) => Some((group, fingerprint)),
                }
            })
            .map(|(group, fingerprint)| async move {
                if let Err(error) = self.read_group(group, fingerprint.as_ref()).await {
                    tracing::warn!(pull_request = %group.key, error = %error, "pull request watch check failed");
                }
            })
            .buffer_unordered(4)
            .collect::<Vec<()>>()
            .await;
        Ok(())
    }

    async fn fingerprint_of(&self, group: &WatchGroup) -> FingerprintRead {
        let first = &group.targets[0];
        let reference = reference_of(&first.project_id, &first.link);
        match self.host.watch_fingerprint(&reference).await {
            Ok(fingerprint) => FingerprintRead::Read(fingerprint),
            Err(error) if error.rate_limited => FingerprintRead::Paused,
            // The snapshot-gated reads report a host that cannot be read, and count toward giving up.
            Err(error) => {
                tracing::debug!(pull_request = %group.key, error = %error, "pull request watch fingerprint failed");
                FingerprintRead::Read(None)
            }
        }
    }

    fn life_of(&self, target: &WatchTarget) -> WatchLife {
        let mut lives = self.lives.lock();
        lives
            .entry(target.life_key())
            .or_insert_with(|| {
                let started_ms = chrono::DateTime::parse_from_rfc3339(&target.watch.started_at)
                    .map(|parsed| parsed.timestamp_millis())
                    .unwrap_or(0);
                WatchLife {
                    thread_id: target.thread_id.clone(),
                    pull_request: thread_pull_request_key_of(&target.link),
                    started_ms,
                    head_sha: target
                        .watch
                        .head_sha
                        .as_ref()
                        .map(|sha| sha.as_str().to_owned()),
                    pushed_ms: started_ms,
                    longest_quiet_ms: 0,
                    wakes: 0,
                    reads: 0,
                }
            })
            .clone()
    }

    fn report_end(&self, key: &str, life: &WatchLife, reason: WatchEndReason) {
        self.lives.lock().remove(key);
        let now = UtcDateTime::now().as_datetime().timestamp_millis();
        let quiet_ms = now - life.pushed_ms;
        tracing::info!(
            thread_id = %life.thread_id,
            pull_request = %life.pull_request,
            reason = reason.as_str(),
            minutes = minutes(now - life.started_ms),
            quiet_minutes = minutes(quiet_ms),
            longest_quiet_minutes = minutes(life.longest_quiet_ms.max(quiet_ms)),
            wakes = life.wakes,
            reads = life.reads,
            partial = life.started_ms < self.booted_ms,
            "pull request watch ended"
        );
    }

    fn woke(&self, target: &WatchTarget) {
        self.life_of(target);
        if let Some(life) = self.lives.lock().get_mut(&target.life_key()) {
            life.wakes += 1;
        }
    }

    async fn record(
        &self,
        target: &WatchTarget,
        next: Option<ThreadPullRequestWatch>,
        wake: Option<OrchestrationV2WakeMessage>,
    ) -> Result<(), BoxError> {
        let uuid = uuid::Uuid::new_v4();
        let host = host_of(&target.link);
        self.dispatch
            .dispatch(OrchestrationV2ServerCommand::Internal(
                OrchestrationV2InternalCommand::ThreadPullRequestWatchSync {
                    command_id: CommandId::new(format!(
                        "server:pr-watch:{}:{uuid}",
                        target.thread_id
                    ))?,
                    thread_id: target.thread_id.clone(),
                    host: TrimmedNonEmptyString::new(host)?,
                    repository: target.link.repository.clone(),
                    number: target.link.number,
                    started_at: target.watch.started_at.clone(),
                    watch: next,
                    wake: wake.map(|mut wake| {
                        wake.message_id = MessageId::new(format!("message:pr-watch:{uuid}"))
                            .expect("message id is non-empty");
                        wake
                    }),
                },
            ))
            .await
    }

    /// A stop that is not a wake: the watch is removed and logged.
    async fn stop(&self, target: &WatchTarget, reason: WatchEndReason) {
        if let Err(error) = self.record(target, None, None).await {
            tracing::warn!(thread_id = %target.thread_id, error = %error, "pull request watch stop failed");
            return;
        }
        let life = self.life_of(target);
        self.report_end(&target.life_key(), &life, reason);
    }

    async fn read_group(
        &self,
        group: &WatchGroup,
        fingerprint: Option<&Fingerprint>,
    ) -> Result<(), BoxError> {
        let now = UtcDateTime::now().as_datetime().timestamp_millis();
        let last = self.last_reads.lock().get(&group.key).cloned();
        let watch_keys: Vec<String> = group.targets.iter().map(WatchTarget::key).collect();
        let plan = plan_read(
            &group.fingerprint,
            &watch_keys,
            last.as_ref(),
            fingerprint,
            now,
        );
        if !plan.detail {
            return Ok(());
        }
        let first = &group.targets[0];
        let reference = reference_of(&first.project_id, &first.link);
        // The reads are recorded against this fingerprint, so a cached answer older than it must
        // not stand in for them.
        if fingerprint.is_some() {
            self.host.invalidate(&reference).await;
        }
        let detail_read = self.host.detail(&reference);
        let activity_read = async {
            if plan.activity {
                self.host.activity(&reference).await.map(Some)
            } else {
                Ok(None)
            }
        };
        let (detail, activity) = futures::join!(detail_read, activity_read);
        let (detail, activity) = match (detail, activity) {
            (Ok(detail), Ok(activity)) => (detail, activity),
            (Err(error), _) | (_, Err(error)) => {
                self.last_reads.lock().remove(&group.key);
                // The host's pause refuses later reads without a request, so waiting it out is free.
                if error.rate_limited {
                    return Ok(());
                }
                let failures = {
                    let mut failures = self.read_failures.lock();
                    let count = failures.entry(group.key.clone()).or_insert(0);
                    *count += 1;
                    *count
                };
                // The count stays until every stop lands, so a failed stop is tried again on the next
                // failed read, and a watch started after the stops begins from zero.
                if failures >= READ_FAILURE_LIMIT && self.give_up_all(group).await {
                    self.read_failures.lock().remove(&group.key);
                }
                return Err(Box::new(error) as BoxError);
            }
        };
        self.read_failures.lock().remove(&group.key);

        let head_sha = detail.head_sha.as_ref().map(|sha| sha.as_str().to_owned());
        for target in &group.targets {
            let mut life = self.life_of(target);
            // The first read only learns the head, so it is not a push.
            let pushed = life.head_sha.is_some() && head_sha != life.head_sha;
            life.head_sha = head_sha.clone();
            life.reads += 1;
            if pushed {
                life.longest_quiet_ms = life.longest_quiet_ms.max(now - life.pushed_ms);
                life.pushed_ms = now;
            }
            self.lives.lock().insert(target.life_key(), life);
        }

        if detail.state != PullRequestState::Open {
            self.last_reads.lock().remove(&group.key);
            for target in &group.targets {
                if detail.state == PullRequestState::Closed {
                    self.closed(target).await;
                } else {
                    self.stop(target, WatchEndReason::Merged).await;
                }
            }
            return Ok(());
        }

        // Never advance the remark watermark past comments an incomplete read could have missed.
        // A pass that did not read the activity has no remarks to report.
        let remarks = match &activity {
            Some(activity) => self.read_remarks(&group.key, &reference, activity).await,
            None => None,
        };
        let in_flight = detail.mergeability == PullRequestMergeability::Unknown
            || detail.checks.iter().any(|check| {
                check.status == turin_contracts::pull_request::PullRequestCheckStatus::Pending
            });
        self.last_reads.lock().insert(
            group.key.clone(),
            LastRead {
                at: if plan.activity || last.is_none() {
                    now
                } else {
                    last.as_ref().map_or(now, |last| last.at)
                },
                fingerprint: group.fingerprint.clone(),
                in_flight,
                // Comments a partial read could not see are read again next pass.
                remarks_complete: if plan.activity {
                    remarks.is_some()
                } else {
                    last.as_ref().is_some_and(|last| last.remarks_complete)
                },
                watches: watch_keys.iter().cloned().collect(),
                status: fingerprint.map(|fingerprint| fingerprint.status.clone()),
                remarks: fingerprint.map(|fingerprint| fingerprint.remarks.clone()),
            },
        );

        let watch_detail = watch_detail_of(&detail);
        // Each target runs on its own, as upstream's `eachTarget` does: one refusal must not skip
        // the rest of the group.
        for target in &group.targets {
            let step = async {
                let report =
                    evaluate_pull_request_watch(&target.watch, &watch_detail, remarks.as_deref());
                if !report.changes.is_empty() {
                    let (text, notification) =
                        pull_request_watch_message(PullRequestWatchMessageInput {
                            number: target.link.number.get(),
                            url: target.link.url.as_str(),
                            base_branch: detail.base_branch.as_str(),
                            head_sha: report.next.head_sha.as_ref().map(|sha| sha.as_str()),
                            report: &report,
                        });
                    let uuid_message =
                        MessageId::new(format!("message:pr-watch:{}", uuid::Uuid::new_v4()))
                            .expect("message id is non-empty");
                    let wake = OrchestrationV2WakeMessage {
                        message_id: uuid_message,
                        text,
                        notification,
                    };
                    let next = if report.exhausted {
                        None
                    } else {
                        Some(report.next.clone())
                    };
                    self.record(target, next, Some(wake)).await?;
                    self.woke(target);
                    if report.exhausted {
                        let life = self.life_of(target);
                        self.report_end(&target.life_key(), &life, WatchEndReason::CommentLimit);
                    }
                } else if !watches_equal(&report.next, &target.watch) {
                    self.record(target, Some(report.next.clone()), None).await?;
                }
                Ok::<(), BoxError>(())
            };
            if let Err(error) = step.await {
                // A thread that did not get its update must not wait for the quiet reread.
                self.last_reads.lock().remove(&group.key);
                tracing::warn!(
                    thread_id = %target.thread_id,
                    pull_request = %group.key,
                    error = %error,
                    "pull request watch update failed"
                );
            }
        }
        Ok(())
    }

    /// Stops every watch on a pull request whose host cannot be read. Returns whether every
    /// stop landed.
    async fn give_up_all(&self, group: &WatchGroup) -> bool {
        let mut landed = true;
        for target in &group.targets {
            let number = target.link.number.get();
            let url = target.link.url.as_str();
            let wake = OrchestrationV2WakeMessage {
                message_id: MessageId::new(format!("message:pr-watch:{}", uuid::Uuid::new_v4()))
                    .expect("message id is non-empty"),
                text: format!(
                    "T3 Code stopped watching pull request #{number} ({url}) because it failed to read it from the host {READ_FAILURE_LIMIT} times in a row. Check it yourself, and call watch_pull_request to watch it again."
                ),
                notification: turin_contracts::orchestration_v2::OrchestrationV2Notification {
                    source: turin_contracts::orchestration_v2::OrchestrationV2NotificationSource::Monitor,
                    outcome: turin_contracts::orchestration_v2::OrchestrationV2NotificationOutcome::Failed,
                    summary: TrimmedNonEmptyString::new(format!("#{number}: stopped watching, could not read it"))
                        .expect("summary is non-empty"),
                    detail: None,
                },
            };
            match self.record(target, None, Some(wake)).await {
                Ok(()) => {
                    self.woke(target);
                    let life = self.life_of(target);
                    self.report_end(&target.life_key(), &life, WatchEndReason::Unreadable);
                }
                Err(_) => {
                    // A failed wake still ends the watch, as upstream's catch does.
                    if self.record(target, None, None).await.is_ok() {
                        let life = self.life_of(target);
                        self.report_end(&target.life_key(), &life, WatchEndReason::Unreadable);
                    } else {
                        landed = false;
                    }
                }
            }
        }
        landed
    }

    async fn closed(&self, target: &WatchTarget) {
        let number = target.link.number.get();
        let url = target.link.url.as_str();
        let wake = OrchestrationV2WakeMessage {
            message_id: MessageId::new(format!("message:pr-watch:{}", uuid::Uuid::new_v4()))
                .expect("message id is non-empty"),
            text: format!(
                "Pull request #{number} ({url}) was closed, so T3 Code stopped watching it. Call watch_pull_request if it reopens."
            ),
            notification: turin_contracts::orchestration_v2::OrchestrationV2Notification {
                source:
                    turin_contracts::orchestration_v2::OrchestrationV2NotificationSource::Monitor,
                outcome:
                    turin_contracts::orchestration_v2::OrchestrationV2NotificationOutcome::Updated,
                summary: TrimmedNonEmptyString::new(format!("#{number}: closed, stopped watching"))
                    .expect("summary is non-empty"),
                detail: None,
            },
        };
        if self.record(target, None, Some(wake)).await.is_ok() {
            self.woke(target);
            let life = self.life_of(target);
            self.report_end(&target.life_key(), &life, WatchEndReason::Closed);
        }
    }

    /// `readRemarks`: the activity's comments plus every review thread's replies past its first page.
    /// Returns `None` when the read is incomplete, so the watermark does not move past what it missed.
    async fn read_remarks(
        &self,
        key: &str,
        reference: &PullRequestRef,
        activity: &PullRequestActivity,
    ) -> Option<Vec<PullRequestComment>> {
        // Comment cursors cannot account for missing threads. Only finish a truncated read when the
        // host confirms that every thread was listed.
        if activity.comments_truncated && activity.review_threads_truncated != Some(false) {
            return None;
        }
        let mut remarks: Vec<PullRequestComment> = activity.comments.clone();
        for thread in &activity.review_threads {
            let Some(mut cursor) = thread.next_comments_cursor.clone() else {
                continue;
            };
            let count = thread.comment_count.unwrap_or(0);
            let cached = self
                .thread_tails
                .lock()
                .get(key)
                .and_then(|tails| tails.get(thread.id.as_str()).cloned());
            let tail = match cached {
                Some((cached_count, comments)) if cached_count == count => comments,
                _ => {
                    let mut seen_cursors: HashSet<String> = HashSet::new();
                    let mut comments: BTreeMap<String, PullRequestComment> = BTreeMap::new();
                    loop {
                        if !seen_cursors.insert(cursor.as_str().to_owned()) {
                            return None;
                        }
                        let page = match self
                            .host
                            .thread_comments(reference, &thread.id, &cursor)
                            .await
                        {
                            Ok(page) => page,
                            Err(error) => {
                                tracing::warn!(error = %error, "pull request watch comment pagination failed");
                                return None;
                            }
                        };
                        for comment in page.comments {
                            let mapped = review_comment(comment, &thread.path);
                            comments.insert(mapped.id.as_str().to_owned(), mapped);
                        }
                        match page.next_cursor {
                            Some(next) => cursor = next,
                            None => break,
                        }
                    }
                    if ((thread.comments.len() + comments.len()) as u64) < count {
                        return None;
                    }
                    let tail: Vec<PullRequestComment> = comments.into_values().collect();
                    self.thread_tails
                        .lock()
                        .entry(key.to_owned())
                        .or_default()
                        .insert(thread.id.as_str().to_owned(), (count, tail.clone()));
                    tail
                }
            };
            remarks.extend(tail);
        }
        remarks.sort_by(|left, right| left.created_at.cmp(&right.created_at));
        Some(remarks)
    }
}

fn snapshot_fingerprint_of(target: &WatchTarget) -> String {
    snapshot_fingerprint(&target.link)
}

/// The fields of a detail the watch evaluation reads.
fn watch_detail_of(detail: &PullRequestDetail) -> WatchDetail {
    WatchDetail {
        head_sha: detail.head_sha.as_ref().map(|sha| sha.as_str().to_owned()),
        checks: detail.checks.clone(),
        mergeability: detail.mergeability,
        viewer: detail
            .viewer
            .as_ref()
            .map(|viewer| viewer.as_str().to_owned()),
        author: detail
            .author
            .as_ref()
            .map(|author| author.login.as_str().to_owned()),
    }
}

/// A reply from a review thread, as a remark (`kind: review-comment`, on the thread's path).
fn review_comment(
    comment: PullRequestThreadComment,
    path: &TrimmedNonEmptyString,
) -> PullRequestComment {
    PullRequestComment {
        id: comment.id,
        kind: PullRequestCommentKind::ReviewComment,
        author: comment.author,
        body: comment.body,
        created_at: comment.created_at,
        edited_at: comment.edited_at,
        url: comment.url,
        path: Some(path.as_str().to_owned()),
        review_state: None,
        reactions: comment.reactions,
        can_edit: comment.can_edit,
    }
}

/// The watch's host reads over T07's `PullRequestService`. Reads ask the service with stale data
/// off, as upstream's watch does.
pub struct ServiceWatchHost {
    service: Arc<turin_source_control::pull_request::service::PullRequestService>,
}

impl ServiceWatchHost {
    pub fn new(
        service: Arc<turin_source_control::pull_request::service::PullRequestService>,
    ) -> Self {
        Self { service }
    }
}

fn watch_host_error(
    error: turin_source_control::pull_request::service::errors::PullRequestError,
) -> WatchHostError {
    WatchHostError {
        message: error.to_string(),
        rate_limited: super::pull_request_sync::adapters::rate_limited_retry_at(&error).is_some(),
    }
}

#[async_trait]
impl WatchHost for ServiceWatchHost {
    async fn invalidate(&self, reference: &PullRequestRef) {
        self.service
            .invalidate(
                &turin_contracts::pull_request::PullRequestInvalidateInput {
                    reference: Some(reference.clone()),
                    files_viewed_only: None,
                },
                false,
            )
            .await;
    }

    async fn detail(
        &self,
        reference: &PullRequestRef,
    ) -> Result<PullRequestDetail, WatchHostError> {
        let reference = PullRequestRef {
            allow_stale: Some(false),
            ..reference.clone()
        };
        self.service
            .detail(&reference)
            .await
            .map_err(watch_host_error)
    }

    async fn activity(
        &self,
        reference: &PullRequestRef,
    ) -> Result<PullRequestActivity, WatchHostError> {
        self.service
            .activity(reference)
            .await
            .map_err(watch_host_error)
    }

    async fn thread_comments(
        &self,
        reference: &PullRequestRef,
        thread_id: &TrimmedNonEmptyString,
        cursor: &TrimmedNonEmptyString,
    ) -> Result<PullRequestThreadCommentsResult, WatchHostError> {
        let input = turin_contracts::pull_request::PullRequestThreadCommentsInput {
            project_id: reference.project_id.clone(),
            host: reference.host.clone(),
            expected_account_id: reference.expected_account_id.clone(),
            allow_stale: reference.allow_stale,
            repository: reference.repository.clone(),
            number: reference.number,
            thread_id: thread_id.clone(),
            cursor: cursor.clone(),
        };
        self.service
            .thread_comments(&input)
            .await
            .map_err(watch_host_error)
    }

    async fn watch_fingerprint(
        &self,
        reference: &PullRequestRef,
    ) -> Result<Option<Fingerprint>, WatchHostError> {
        self.service
            .watch_fingerprint(reference)
            .await
            .map(|fingerprint| fingerprint.map(Fingerprint::from))
            .map_err(watch_host_error)
    }
}

#[cfg(test)]
mod tests;
