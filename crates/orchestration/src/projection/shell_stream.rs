//! Port of upstream/apps/server/src/orchestration-v2/ShellStream.ts
//!
//! `loadShellSnapshotParts` is `store::shell::read_shell_snapshot_rows` +
//! `ShellSnapshotRows::decode`; `composeShellStreamWithEnrichment` is a
//! `futures` chain/merge at the subscription site. The two stream filters
//! (`dedupeShellEnrichment`, `skipUnchangedThreadShells`) are stateful structs
//! whose `accept` is the per-item predicate.

use std::collections::{HashMap, HashSet};

use indexmap::{IndexMap, IndexSet};
use turin_contracts::orchestration_project::OrchestrationProjectShell;
use turin_contracts::orchestration_v2::{
    OrchestrationV2ArchivedShellStreamItem, OrchestrationV2DomainEvent,
    OrchestrationV2ShellLocation, OrchestrationV2ShellSnapshot, OrchestrationV2ShellStreamItem,
    OrchestrationV2StoredEvent, OrchestrationV2ThreadShell, OrchestrationV2ThreadShellSnapshot,
};
use turin_contracts::{NonNegativeInt, ThreadId};
use turin_persistence::ApplicationStoredEvent;

use crate::projection::event::event_thread_id;

/// Build the regular navigation shell without duplicating the archive dataset.
pub fn build_active_shell_snapshot(
    projects: Vec<OrchestrationProjectShell>,
    threads: OrchestrationV2ThreadShellSnapshot,
    snapshot_sequence: NonNegativeInt,
) -> OrchestrationV2ShellSnapshot {
    OrchestrationV2ShellSnapshot {
        schema_version: threads.schema_version,
        snapshot_sequence,
        projects,
        threads: threads.threads,
        archived_threads: Vec::new(),
    }
}

/// A committed event reduced to what a shell update needs to refetch its aggregate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellApplicationEvent {
    Project {
        aggregate_id: String,
        event_type: String,
        sequence: NonNegativeInt,
    },
    Thread {
        sequence: NonNegativeInt,
        thread_id: ThreadId,
    },
}

/// Shell updates refetch an aggregate; drop transcript bodies before retaining an event.
pub fn to_shell_application_event(stored: &ApplicationStoredEvent) -> ShellApplicationEvent {
    match stored {
        ApplicationStoredEvent::Project(event) => ShellApplicationEvent::Project {
            aggregate_id: event.aggregate_id.clone(),
            event_type: event.event_type.clone(),
            sequence: stored_sequence(event.sequence),
        },
        ApplicationStoredEvent::Agent(event) => ShellApplicationEvent::Thread {
            sequence: stored_sequence(event.sequence),
            thread_id: event.event.thread_id.clone(),
        },
    }
}

fn stored_sequence(sequence: i64) -> NonNegativeInt {
    u64::try_from(sequence).unwrap_or(0)
}

/// An event that belongs to one project or thread aggregate
/// (upstream's `A extends ShellApplicationEvent`).
pub trait ShellAggregateEvent {
    fn shell_sequence(&self) -> NonNegativeInt;
    fn shell_aggregate_key(&self) -> String;
}

impl ShellAggregateEvent for ShellApplicationEvent {
    fn shell_sequence(&self) -> NonNegativeInt {
        match self {
            Self::Project { sequence, .. } | Self::Thread { sequence, .. } => *sequence,
        }
    }
    fn shell_aggregate_key(&self) -> String {
        match self {
            Self::Project { aggregate_id, .. } => format!("project:{aggregate_id}"),
            Self::Thread { thread_id, .. } => format!("thread:{}", thread_id.as_str()),
        }
    }
}

impl ShellAggregateEvent for ApplicationStoredEvent {
    fn shell_sequence(&self) -> NonNegativeInt {
        stored_sequence(self.sequence())
    }
    fn shell_aggregate_key(&self) -> String {
        match self {
            Self::Project(event) => format!("project:{}", event.aggregate_id),
            Self::Agent(event) => format!("thread:{}", event.event.thread_id.as_str()),
        }
    }
}

/// Keep only the newest shell-relevant event per project/thread aggregate.
pub fn coalesce_shell_application_events<A: ShellAggregateEvent>(events: Vec<A>) -> Vec<A> {
    let mut latest_by_aggregate: IndexMap<String, A> = IndexMap::new();
    for stored in events {
        latest_by_aggregate.insert(stored.shell_aggregate_key(), stored);
    }
    let mut result: Vec<A> = latest_by_aggregate.into_values().collect();
    result.sort_by_key(ShellAggregateEvent::shell_sequence);
    result
}

/// Drop repeated metadata refreshes within one subscription, preserving resolution progress.
#[derive(Debug, Default)]
pub struct DedupeShellEnrichment {
    previous: Option<(Vec<OrchestrationProjectShell>, HashSet<String>)>,
}

impl DedupeShellEnrichment {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` when `item` should be emitted.
    pub fn accept(&mut self, item: &OrchestrationV2ShellStreamItem) -> bool {
        let OrchestrationV2ShellStreamItem::Snapshot {
            snapshot,
            resolved_repository_identity_roots,
        } = item
        else {
            // A project delta may change which projects/identities a later refresh repairs.
            if matches!(
                item,
                OrchestrationV2ShellStreamItem::ProjectUpdated { .. }
                    | OrchestrationV2ShellStreamItem::ProjectRemoved { .. }
            ) {
                self.previous = None;
            }
            return true;
        };
        let Some(resolved) = resolved_repository_identity_roots else {
            self.previous = None;
            return true;
        };
        if !snapshot.threads.is_empty() || !snapshot.archived_threads.is_empty() {
            self.previous = None;
            return true;
        }
        let roots: HashSet<String> = resolved.iter().cloned().collect();
        if let Some((prior_projects, prior_roots)) = &self.previous
            && roots == *prior_roots
            && *prior_projects == snapshot.projects
        {
            return false;
        }
        self.previous = Some((snapshot.projects.clone(), roots));
        true
    }
}

fn enrichment_snapshot(
    snapshot: &OrchestrationV2ShellSnapshot,
    roots: &[String],
) -> OrchestrationV2ShellSnapshot {
    OrchestrationV2ShellSnapshot {
        schema_version: snapshot.schema_version,
        snapshot_sequence: snapshot.snapshot_sequence,
        projects: snapshot
            .projects
            .iter()
            .filter(|project| {
                roots
                    .iter()
                    .any(|root| root == project.workspace_root.as_ref())
            })
            .cloned()
            .collect(),
        threads: Vec::new(),
        archived_threads: Vec::new(),
    }
}

fn unique_roots<'a>(roots: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    roots
        .into_iter()
        .map(str::to_owned)
        .collect::<IndexSet<_>>()
        .into_iter()
        .collect()
}

/// Build a shell snapshot stream item for a batched enrichment completion.
pub fn shell_stream_item_from_enrichment_refresh<'a>(
    snapshot: &OrchestrationV2ShellSnapshot,
    changed_workspace_roots: impl IntoIterator<Item = &'a str>,
) -> OrchestrationV2ShellStreamItem {
    let resolved = unique_roots(changed_workspace_roots);
    OrchestrationV2ShellStreamItem::Snapshot {
        snapshot: enrichment_snapshot(snapshot, &resolved),
        resolved_repository_identity_roots: Some(resolved),
    }
}

/// Initial subscribe frames: always emit the unmarked authoritative snapshot,
/// then a same-sequence enrichment frame only when some roots already resolved.
pub fn shell_stream_items_from_initial_snapshot(
    snapshot: OrchestrationV2ShellSnapshot,
    resolved_repository_identity_roots: &[String],
) -> Vec<OrchestrationV2ShellStreamItem> {
    if resolved_repository_identity_roots.is_empty() {
        return vec![OrchestrationV2ShellStreamItem::Snapshot {
            snapshot,
            resolved_repository_identity_roots: None,
        }];
    }
    let marked = OrchestrationV2ShellStreamItem::Snapshot {
        snapshot: enrichment_snapshot(&snapshot, resolved_repository_identity_roots),
        resolved_repository_identity_roots: Some(unique_roots(
            resolved_repository_identity_roots
                .iter()
                .map(String::as_str),
        )),
    };
    vec![
        OrchestrationV2ShellStreamItem::Snapshot {
            snapshot,
            resolved_repository_identity_roots: None,
        },
        marked,
    ]
}

/// Resume after HTTP/cache hydration with metadata only, never another full shell body.
pub fn shell_stream_items_from_resume_snapshot(
    snapshot: OrchestrationV2ShellSnapshot,
    resolved_repository_identity_roots: &[String],
) -> Vec<OrchestrationV2ShellStreamItem> {
    shell_stream_items_from_initial_snapshot(snapshot, resolved_repository_identity_roots)
        .into_iter()
        .filter(|item| {
            matches!(
                item,
                OrchestrationV2ShellStreamItem::Snapshot {
                    resolved_repository_identity_roots: Some(_),
                    ..
                }
            )
        })
        .collect()
}

/// How long an unchanged thread shell may go unsent on a live subscription.
pub const UNCHANGED_THREAD_SHELL_RESEND_MS: i64 = 5_000;

/// Drop live `thread.updated` deltas whose shell matches the last one this
/// subscription sent, apart from `updatedAt`. Every thread event bumps
/// `updatedAt`, so tool output and streaming text alone used to send the full,
/// otherwise unchanged shell once per batch.
///
/// A skipped delta leaves the client cursor behind, and a resume replays the
/// events after that cursor. So an unchanged shell is still sent once its last
/// send is `UNCHANGED_THREAD_SHELL_RESEND_MS` old. That bounds both the replay
/// and how stale the client's `updatedAt` can get.
#[derive(Debug, Default)]
pub struct SkipUnchangedThreadShells {
    last_sent: HashMap<ThreadId, (OrchestrationV2ThreadShell, i64)>,
}

impl SkipUnchangedThreadShells {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` when `item` should be emitted. `now_ms` is the clock reading for this item.
    pub fn accept(&mut self, item: &OrchestrationV2ShellStreamItem, now_ms: i64) -> bool {
        let thread = match item {
            OrchestrationV2ShellStreamItem::ThreadRemoved { thread_id, .. } => {
                self.last_sent.remove(thread_id);
                return true;
            }
            OrchestrationV2ShellStreamItem::ThreadUpdated { thread, .. } => thread,
            _ => return true,
        };
        if let Some((previous, sent_at)) = self.last_sent.get(&thread.id)
            && now_ms - sent_at < UNCHANGED_THREAD_SHELL_RESEND_MS
            && same_apart_from_updated_at(thread, previous)
        {
            return false;
        }
        self.last_sent
            .insert(thread.id.clone(), (thread.clone(), now_ms));
        true
    }
}

fn same_apart_from_updated_at(
    thread: &OrchestrationV2ThreadShell,
    previous: &OrchestrationV2ThreadShell,
) -> bool {
    if thread.updated_at == previous.updated_at {
        return thread == previous;
    }
    let mut candidate = thread.clone();
    candidate.updated_at = previous.updated_at;
    candidate == *previous
}

/// Keep only the newest stored event per thread within a coalescing window.
pub fn coalesce_stored_thread_events(
    events: Vec<OrchestrationV2StoredEvent>,
) -> Vec<OrchestrationV2StoredEvent> {
    let mut latest_by_thread_id: IndexMap<ThreadId, OrchestrationV2StoredEvent> = IndexMap::new();
    for stored in events {
        latest_by_thread_id.insert(event_thread_id(&stored.event).clone(), stored);
    }
    let mut result: Vec<_> = latest_by_thread_id.into_values().collect();
    result.sort_by_key(|stored| stored.sequence);
    result
}

/// Converts a committed event and the affected thread's current shell into one
/// delta. `shell` is `None` when the thread is deleted or unknown.
pub fn shell_stream_item_from_thread_shell(
    sequence: NonNegativeInt,
    thread_id: &ThreadId,
    shell: Option<OrchestrationV2ThreadShell>,
) -> OrchestrationV2ShellStreamItem {
    match shell {
        Some(shell) if shell.archived_at.is_some() => {
            OrchestrationV2ShellStreamItem::ThreadRemoved {
                sequence,
                location: OrchestrationV2ShellLocation::Active,
                thread_id: shell.id,
            }
        }
        Some(shell) => OrchestrationV2ShellStreamItem::ThreadUpdated {
            sequence,
            location: OrchestrationV2ShellLocation::Active,
            thread: shell,
        },
        None => OrchestrationV2ShellStreamItem::ThreadRemoved {
            sequence,
            location: OrchestrationV2ShellLocation::Active,
            thread_id: thread_id.clone(),
        },
    }
}

/// Converts a committed event into an archive-only delta when it changes archive membership.
pub fn archived_shell_stream_item_from_thread_shell(
    stored: &OrchestrationV2StoredEvent,
    shell: Option<OrchestrationV2ThreadShell>,
) -> Option<OrchestrationV2ArchivedShellStreamItem> {
    if let Some(shell) = shell
        && shell.archived_at.is_some()
    {
        return Some(OrchestrationV2ArchivedShellStreamItem::ThreadUpdated {
            sequence: stored.sequence,
            thread: shell,
        });
    }
    let leaves_archive = match &stored.event {
        OrchestrationV2DomainEvent::ThreadUnarchived { .. } => true,
        OrchestrationV2DomainEvent::ThreadDeleted { payload, .. } => payload.archived_at.is_some(),
        _ => false,
    };
    leaves_archive.then(|| OrchestrationV2ArchivedShellStreamItem::ThreadRemoved {
        sequence: stored.sequence,
        thread_id: event_thread_id(&stored.event).clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};
    use turin_contracts::PositiveInt;

    use crate::projection::test_support::{T0, thread};

    fn shell_json(id: &str, overrides: Value) -> Value {
        let mut value = json!({
            "id": id,
            "projectId": "project-a",
            "title": "Thread",
            "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "gpt-5.4"},
            "runtimeMode": "full-access",
            "interactionMode": "default",
            "worktreePath": null,
            "activeProviderThreadId": null,
            "lineage": {"rootThreadId": id, "parentThreadId": null, "relationshipToParent": null},
            "forkedFrom": null,
            "createdBy": "user",
            "creationSource": "web",
            "activeRunId": null,
            "latestVisibleMessage": null,
            "hasActionableProposedPlan": false,
            "itemCount": 1,
            "visibleItemCount": 1,
            "branch": null,
            "status": "running",
            "pendingRuntimeRequest": null,
            "pendingBackgroundTasks": [],
            "providerInstanceHistory": [],
            "latestRunId": null,
            "latestUserMessageAt": null,
            "createdAt": T0,
            "updatedAt": T0,
            "archivedAt": null,
            "settledOverride": null,
            "settledAt": null,
            "deletedAt": null,
        });
        for (key, field) in overrides.as_object().cloned().unwrap_or_default() {
            value[key] = field;
        }
        value
    }

    fn shell(id: &str, overrides: Value) -> OrchestrationV2ThreadShell {
        serde_json::from_value(shell_json(id, overrides)).expect("shell fixture")
    }

    fn at_ms(ms: i64) -> Value {
        let base = chrono::DateTime::parse_from_rfc3339(T0).expect("T0");
        let time = base + chrono::Duration::milliseconds(ms);
        json!(
            time.with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        )
    }

    fn project_shell(id: &str, workspace_root: &str) -> OrchestrationProjectShell {
        serde_json::from_value(json!({
            "id": id,
            "title": "A project",
            "workspaceRoot": workspace_root,
            "repositoryIdentity": null,
            "defaultModelSelection": null,
            "scripts": [],
            "createdAt": "2026-09-01T00:00:00Z",
            "updatedAt": "2026-09-01T00:00:00Z",
        }))
        .expect("project fixture")
    }

    fn empty_snapshot() -> OrchestrationV2ShellSnapshot {
        OrchestrationV2ShellSnapshot {
            schema_version: PositiveInt::new(1).expect("1"),
            snapshot_sequence: 0,
            projects: Vec::new(),
            threads: Vec::new(),
            archived_threads: Vec::new(),
        }
    }

    fn snapshot_item(
        snapshot: OrchestrationV2ShellSnapshot,
        roots: Option<Vec<&str>>,
    ) -> OrchestrationV2ShellStreamItem {
        OrchestrationV2ShellStreamItem::Snapshot {
            snapshot,
            resolved_repository_identity_roots: roots
                .map(|roots| roots.into_iter().map(str::to_owned).collect()),
        }
    }

    fn tid(id: &str) -> ThreadId {
        ThreadId::new(id).expect("thread id")
    }

    fn stored(
        sequence: u64,
        thread_id: &str,
        kind: &str,
        payload: Value,
    ) -> OrchestrationV2StoredEvent {
        serde_json::from_value(json!({
            "sequence": sequence,
            "commandId": null,
            "event": {
                "id": format!("event:{sequence}"),
                "type": kind,
                "threadId": thread_id,
                "occurredAt": T0,
                "payload": payload,
            }
        }))
        .unwrap_or_else(|error| panic!("stored fixture {kind}: {error}"))
    }

    fn deleted_payload(thread_id: &str, archived_at: Option<&str>) -> Value {
        let mut payload = thread(thread_id);
        payload["archivedAt"] = json!(archived_at);
        payload["deletedAt"] = json!(T0);
        payload
    }

    fn thread_stored(sequence: u64, thread_id: &str) -> OrchestrationV2StoredEvent {
        stored(sequence, thread_id, "thread.created", thread(thread_id))
    }

    #[test]
    fn build_active_shell_snapshot_never_duplicates_archived_rows() {
        let active = shell("thread-a", json!({}));
        let archived = shell(
            "thread-archived",
            json!({"archivedAt": "2026-07-30T00:00:00.000Z"}),
        );
        let snapshot = build_active_shell_snapshot(
            Vec::new(),
            OrchestrationV2ThreadShellSnapshot {
                schema_version: PositiveInt::new(1).expect("1"),
                snapshot_sequence: 4,
                threads: vec![active.clone()],
                archived_threads: vec![archived],
            },
            7,
        );
        assert_eq!(
            snapshot,
            OrchestrationV2ShellSnapshot {
                schema_version: PositiveInt::new(1).expect("1"),
                snapshot_sequence: 7,
                projects: Vec::new(),
                threads: vec![active],
                archived_threads: Vec::new(),
            }
        );
    }

    #[test]
    fn coalesce_shell_application_events_keeps_newest_per_aggregate_in_sequence_order() {
        let project = |sequence, id: &str| ShellApplicationEvent::Project {
            aggregate_id: id.to_owned(),
            event_type: "project.updated".to_owned(),
            sequence,
        };
        let thread = |sequence, id: &str| ShellApplicationEvent::Thread {
            sequence,
            thread_id: tid(id),
        };
        let coalesced = coalesce_shell_application_events(vec![
            thread(2, "thread-a"),
            project(3, "project-a"),
            thread(4, "thread-b"),
            thread(5, "thread-a"),
            project(6, "project-a"),
        ]);
        assert_eq!(
            coalesced
                .iter()
                .map(ShellAggregateEvent::shell_sequence)
                .collect::<Vec<_>>(),
            vec![4, 5, 6]
        );
    }

    #[test]
    fn coalesce_stored_thread_events_keeps_newest_per_thread_in_sequence_order() {
        let coalesced = coalesce_stored_thread_events(vec![
            thread_stored(2, "thread-a"),
            thread_stored(3, "thread-b"),
            thread_stored(5, "thread-a"),
        ]);
        assert_eq!(
            coalesced.iter().map(|s| s.sequence).collect::<Vec<_>>(),
            vec![3, 5]
        );
    }

    #[test]
    fn emits_an_active_thread_update_when_the_shell_is_not_archived() {
        let shell = shell("thread-a", json!({}));
        assert_eq!(
            shell_stream_item_from_thread_shell(4, &tid("thread-a"), Some(shell.clone())),
            OrchestrationV2ShellStreamItem::ThreadUpdated {
                sequence: 4,
                location: OrchestrationV2ShellLocation::Active,
                thread: shell,
            }
        );
    }

    #[test]
    fn removes_archived_threads_from_the_active_only_shell() {
        let shell = shell(
            "thread-a",
            json!({"archivedAt": "2026-07-30T00:00:00.000Z"}),
        );
        assert_eq!(
            shell_stream_item_from_thread_shell(4, &tid("thread-a"), Some(shell)),
            OrchestrationV2ShellStreamItem::ThreadRemoved {
                sequence: 4,
                location: OrchestrationV2ShellLocation::Active,
                thread_id: tid("thread-a"),
            }
        );
    }

    #[test]
    fn emits_an_active_shell_removal_for_missing_shells() {
        // Upstream covers a deleted archived thread and any other missing
        // shell; both reduce to the same removal.
        assert_eq!(
            shell_stream_item_from_thread_shell(6, &tid("thread-a"), None),
            OrchestrationV2ShellStreamItem::ThreadRemoved {
                sequence: 6,
                location: OrchestrationV2ShellLocation::Active,
                thread_id: tid("thread-a"),
            }
        );
    }

    #[test]
    fn archived_stream_emits_an_update_for_an_archived_shell() {
        let shell = shell(
            "thread-a",
            json!({"archivedAt": "2026-07-30T00:00:00.000Z"}),
        );
        assert_eq!(
            archived_shell_stream_item_from_thread_shell(
                &thread_stored(4, "thread-a"),
                Some(shell.clone())
            ),
            Some(OrchestrationV2ArchivedShellStreamItem::ThreadUpdated {
                sequence: 4,
                thread: shell
            })
        );
    }

    #[test]
    fn archived_stream_ignores_active_threads_that_never_touched_the_archive() {
        let settled = stored(4, "thread-a", "thread.settled", thread("thread-a"));
        assert_eq!(
            archived_shell_stream_item_from_thread_shell(
                &settled,
                Some(shell("thread-a", json!({})))
            ),
            None
        );
    }

    #[test]
    fn archived_stream_emits_a_removal_when_a_thread_leaves_the_archive() {
        let unarchived = stored(4, "thread-a", "thread.unarchived", thread("thread-a"));
        assert_eq!(
            archived_shell_stream_item_from_thread_shell(
                &unarchived,
                Some(shell("thread-a", json!({})))
            ),
            Some(OrchestrationV2ArchivedShellStreamItem::ThreadRemoved {
                sequence: 4,
                thread_id: tid("thread-a")
            })
        );
    }

    #[test]
    fn archived_stream_emits_a_removal_when_an_archived_thread_is_deleted() {
        let deleted = stored(
            4,
            "thread-a",
            "thread.deleted",
            deleted_payload("thread-a", Some("2026-07-30T00:00:00.000Z")),
        );
        assert_eq!(
            archived_shell_stream_item_from_thread_shell(&deleted, None),
            Some(OrchestrationV2ArchivedShellStreamItem::ThreadRemoved {
                sequence: 4,
                thread_id: tid("thread-a")
            })
        );
        let deleted_active = stored(
            5,
            "thread-a",
            "thread.deleted",
            deleted_payload("thread-a", None),
        );
        assert_eq!(
            archived_shell_stream_item_from_thread_shell(&deleted_active, None),
            None
        );
    }

    #[test]
    fn enrichment_refresh_batches_nearby_completion_roots_onto_one_snapshot_item() {
        assert_eq!(
            shell_stream_item_from_enrichment_refresh(
                &empty_snapshot(),
                ["/workspace/a", "/workspace/b", "/workspace/a"]
            ),
            snapshot_item(empty_snapshot(), Some(vec!["/workspace/a", "/workspace/b"]))
        );
    }

    #[test]
    fn initial_snapshot_keeps_thread_rows_out_of_the_metadata_only_frame() {
        let mut snapshot = empty_snapshot();
        snapshot.projects = vec![
            project_shell("project-a", "/workspace/a"),
            project_shell("project-b", "/workspace/b"),
        ];
        snapshot.threads = vec![shell("thread-a", json!({}))];
        snapshot.archived_threads = vec![shell("thread-archived", json!({}))];

        let items =
            shell_stream_items_from_initial_snapshot(snapshot.clone(), &["/workspace/a".into()]);
        assert_eq!(items[0], snapshot_item(snapshot.clone(), None));
        let mut metadata = empty_snapshot();
        metadata.projects = vec![project_shell("project-a", "/workspace/a")];
        assert_eq!(
            items[1],
            snapshot_item(metadata, Some(vec!["/workspace/a"]))
        );
        let len = |item: &OrchestrationV2ShellStreamItem| {
            serde_json::to_string(item).expect("encode").len()
        };
        assert!(len(&items[1]) < len(&items[0]));
    }

    #[test]
    fn initial_snapshot_emits_unmarked_then_same_sequence_marked_enrichment() {
        let mut snapshot = empty_snapshot();
        snapshot.snapshot_sequence = 7;
        assert_eq!(
            shell_stream_items_from_initial_snapshot(
                snapshot.clone(),
                &["/workspace/a".into(), "/workspace/a".into()]
            ),
            vec![
                snapshot_item(snapshot.clone(), None),
                snapshot_item(snapshot, Some(vec!["/workspace/a"])),
            ]
        );
    }

    #[test]
    fn initial_snapshot_emits_only_the_authoritative_snapshot_when_no_roots_resolved() {
        assert_eq!(
            shell_stream_items_from_initial_snapshot(empty_snapshot(), &[]),
            vec![snapshot_item(empty_snapshot(), None)]
        );
    }

    #[test]
    fn resume_snapshot_never_repeats_the_authoritative_shell_snapshot() {
        let mut snapshot = empty_snapshot();
        snapshot.threads = vec![shell("thread-a", json!({}))];
        assert_eq!(
            shell_stream_items_from_resume_snapshot(snapshot, &[]),
            Vec::new()
        );
    }

    fn dedupe(items: &[OrchestrationV2ShellStreamItem]) -> Vec<OrchestrationV2ShellStreamItem> {
        let mut filter = DedupeShellEnrichment::new();
        items
            .iter()
            .filter(|item| filter.accept(item))
            .cloned()
            .collect()
    }

    fn with_projects(
        item: &OrchestrationV2ShellStreamItem,
        sequence: Option<u64>,
        projects: Vec<OrchestrationProjectShell>,
        roots: Option<Vec<&str>>,
    ) -> OrchestrationV2ShellStreamItem {
        let OrchestrationV2ShellStreamItem::Snapshot { snapshot, .. } = item else {
            unreachable!()
        };
        let mut snapshot = snapshot.clone();
        if let Some(sequence) = sequence {
            snapshot.snapshot_sequence = sequence;
        }
        snapshot.projects = projects;
        snapshot_item(snapshot, roots)
    }

    #[test]
    fn dedupe_keeps_resolution_and_real_changes_but_drops_sequence_only_repeats() {
        let project = project_shell("project-a", "/workspace/a");
        let mut base = empty_snapshot();
        base.projects = vec![project.clone()];
        let initial = snapshot_item(base.clone(), None);
        let marked = snapshot_item(base, Some(vec!["/workspace/a"]));
        let repeated = with_projects(
            &marked,
            Some(10),
            vec![project.clone()],
            Some(vec!["/workspace/a"]),
        );
        let progressed = with_projects(
            &repeated,
            None,
            vec![project.clone()],
            Some(vec!["/workspace/a", "/workspace/b"]),
        );
        let mut renamed = project.clone();
        renamed.title = turin_contracts::TrimmedNonEmptyString::new("Renamed").expect("title");
        let changed = with_projects(
            &progressed,
            None,
            vec![renamed],
            Some(vec!["/workspace/a", "/workspace/b"]),
        );
        assert_eq!(
            dedupe(&[
                initial.clone(),
                marked.clone(),
                repeated.clone(),
                progressed.clone(),
                changed.clone(),
                changed.clone(),
            ]),
            vec![initial, marked.clone(), progressed, changed]
        );
        // Each subscription starts fresh.
        assert_eq!(
            dedupe(&[marked.clone(), repeated.clone()]),
            vec![marked.clone()]
        );
        assert_eq!(dedupe(&[marked.clone(), repeated]), vec![marked]);
    }

    #[test]
    fn dedupe_keeps_authoritative_snapshots_and_invalidates_after_project_deltas() {
        let project = project_shell("project-a", "/workspace/a");
        let mut base = empty_snapshot();
        base.projects = vec![project.clone()];
        let initial = snapshot_item(base.clone(), None);
        let marked = snapshot_item(base.clone(), Some(vec!["/workspace/a"]));
        let removed = OrchestrationV2ShellStreamItem::ProjectRemoved {
            sequence: 2,
            project_id: project.id.clone(),
        };
        let updated = OrchestrationV2ShellStreamItem::ProjectUpdated {
            sequence: 3,
            project,
        };
        let mut threaded = base;
        threaded.threads = vec![shell("thread-a", json!({}))];
        let with_threads = snapshot_item(threaded, Some(vec!["/workspace/a"]));
        let values = vec![
            initial.clone(),
            initial,
            marked.clone(),
            removed,
            marked.clone(),
            updated,
            marked,
            with_threads.clone(),
            with_threads,
        ];
        assert_eq!(dedupe(&values), values);
    }

    #[test]
    fn dedupe_spans_thread_deltas_without_dropping_them_or_identity_clears() {
        let project = project_shell("project-a", "/workspace/a");
        let mut base = empty_snapshot();
        base.projects = vec![project.clone()];
        let marked = snapshot_item(base, Some(vec!["/workspace/a"]));
        let delta = OrchestrationV2ShellStreamItem::ThreadRemoved {
            sequence: 2,
            location: OrchestrationV2ShellLocation::Active,
            thread_id: tid("thread-a"),
        };
        let mut identified = project;
        identified.repository_identity = Some(Some(
            serde_json::from_value(json!({
                "canonicalKey": "github.com/test/repo",
                "locator": {"source": "git-remote", "remoteName": "origin",
                    "remoteUrl": "https://github.com/test/repo.git"},
            }))
            .expect("identity"),
        ));
        let resolved = with_projects(&marked, None, vec![identified], Some(vec!["/workspace/a"]));
        assert_eq!(
            dedupe(&[
                marked.clone(),
                delta.clone(),
                marked.clone(),
                resolved.clone(),
                marked.clone()
            ]),
            vec![marked.clone(), delta, resolved, marked]
        );
    }

    enum Step {
        Advance(i64),
        Item(OrchestrationV2ShellStreamItem),
    }

    fn updated(sequence: u64, thread: OrchestrationV2ThreadShell) -> Step {
        Step::Item(OrchestrationV2ShellStreamItem::ThreadUpdated {
            sequence,
            location: OrchestrationV2ShellLocation::Active,
            thread,
        })
    }

    fn run_skip(steps: Vec<Step>) -> Vec<u64> {
        let mut filter = SkipUnchangedThreadShells::new();
        let mut now = 0;
        let mut sent = Vec::new();
        for step in steps {
            match step {
                Step::Advance(ms) => now += ms,
                Step::Item(item) => {
                    if filter.accept(&item, now) {
                        sent.push(match item {
                            OrchestrationV2ShellStreamItem::ThreadUpdated { sequence, .. }
                            | OrchestrationV2ShellStreamItem::ThreadRemoved { sequence, .. } => {
                                sequence
                            }
                            _ => u64::MAX,
                        });
                    }
                }
            }
        }
        sent
    }

    #[test]
    fn skip_drops_shells_that_only_moved_updated_at_and_resends_after_a_while() {
        let sent = run_skip(vec![
            updated(1, shell("a", json!({}))),
            updated(2, shell("a", json!({"updatedAt": at_ms(100)}))),
            updated(
                3,
                shell(
                    "a",
                    json!({"updatedAt": at_ms(200), "itemCount": 2, "visibleItemCount": 2}),
                ),
            ),
            updated(
                4,
                shell(
                    "a",
                    json!({"updatedAt": at_ms(300), "itemCount": 2, "visibleItemCount": 2}),
                ),
            ),
            updated(5, shell("b", json!({}))),
            // The resend window is 5 s.
            Step::Advance(5_000),
            updated(
                6,
                shell(
                    "a",
                    json!({"updatedAt": at_ms(5_300), "itemCount": 2, "visibleItemCount": 2}),
                ),
            ),
        ]);
        assert_eq!(sent, vec![1, 3, 5, 6]);
    }

    #[test]
    fn skip_sends_the_next_update_after_a_removal() {
        let sent = run_skip(vec![
            updated(1, shell("a", json!({}))),
            Step::Item(OrchestrationV2ShellStreamItem::ThreadRemoved {
                sequence: 2,
                location: OrchestrationV2ShellLocation::Active,
                thread_id: tid("a"),
            }),
            updated(3, shell("a", json!({}))),
        ]);
        assert_eq!(sent, vec![1, 2, 3]);
    }
}
