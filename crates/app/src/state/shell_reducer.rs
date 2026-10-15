//! Port of upstream/packages/client-runtime/src/state/shellReducer.ts
//! (`shellPullRequests.ts` and the deferred-links machinery are dropped: a
//! shell snapshot never crosses a wire here, so there is nothing to defer).
//!
//! The snapshot the UI reads is [`ShellSnapshot`]: upstream's
//! `OrchestrationV2ShellSnapshot` with every row and list behind an `Arc`, so
//! applying a delta shares everything it did not touch and "keeps the same
//! object" (upstream's `toBe` assertions) is `Arc::ptr_eq`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::state::contracts::{
    NonNegativeInt, OrchestrationProjectShell, OrchestrationV2ShellLocation,
    OrchestrationV2ShellSnapshot, OrchestrationV2ShellStreamItem, OrchestrationV2ThreadShell,
    PositiveInt, ProjectId, ThreadId, ThreadPullRequestLink,
};

pub type ThreadShellRow = Arc<OrchestrationV2ThreadShell>;
pub type ProjectShellRow = Arc<OrchestrationProjectShell>;

/// `OrchestrationV2ShellSnapshot` with structurally shared rows.
#[derive(Clone, Debug, PartialEq)]
pub struct ShellSnapshot {
    pub schema_version: PositiveInt,
    pub snapshot_sequence: NonNegativeInt,
    pub threads: Arc<Vec<ThreadShellRow>>,
    pub archived_threads: Arc<Vec<ThreadShellRow>>,
    pub projects: Arc<Vec<ProjectShellRow>>,
}

impl From<OrchestrationV2ShellSnapshot> for ShellSnapshot {
    fn from(snapshot: OrchestrationV2ShellSnapshot) -> Self {
        Self {
            schema_version: snapshot.schema_version,
            snapshot_sequence: snapshot.snapshot_sequence,
            threads: Arc::new(snapshot.threads.into_iter().map(Arc::new).collect()),
            archived_threads: Arc::new(
                snapshot
                    .archived_threads
                    .into_iter()
                    .map(Arc::new)
                    .collect(),
            ),
            projects: Arc::new(snapshot.projects.into_iter().map(Arc::new).collect()),
        }
    }
}

/// One shell subscription frame, with `Arc` payloads
/// (`OrchestrationV2ShellStreamItem` minus the frames the reducer does not
/// apply as deltas).
#[derive(Clone, Debug, PartialEq)]
pub enum ShellStreamEvent {
    ProjectUpdated {
        sequence: NonNegativeInt,
        project: ProjectShellRow,
    },
    ProjectRemoved {
        sequence: NonNegativeInt,
        project_id: ProjectId,
    },
    ThreadUpdated {
        sequence: NonNegativeInt,
        location: OrchestrationV2ShellLocation,
        thread: ThreadShellRow,
    },
    ThreadRemoved {
        sequence: NonNegativeInt,
        location: OrchestrationV2ShellLocation,
        thread_id: ThreadId,
    },
}

/// `OrchestrationV2ShellStreamItem`.
#[derive(Clone, Debug, PartialEq)]
pub enum ShellStreamItem {
    Synchronized,
    Snapshot {
        snapshot: ShellSnapshot,
        resolved_repository_identity_roots: Option<Vec<String>>,
    },
    Event(ShellStreamEvent),
}

impl From<OrchestrationV2ShellStreamItem> for ShellStreamItem {
    fn from(item: OrchestrationV2ShellStreamItem) -> Self {
        match item {
            OrchestrationV2ShellStreamItem::Synchronized => Self::Synchronized,
            OrchestrationV2ShellStreamItem::Snapshot {
                snapshot,
                resolved_repository_identity_roots,
            } => Self::Snapshot {
                snapshot: snapshot.into(),
                resolved_repository_identity_roots,
            },
            OrchestrationV2ShellStreamItem::ProjectUpdated { sequence, project } => {
                Self::Event(ShellStreamEvent::ProjectUpdated {
                    sequence,
                    project: Arc::new(project),
                })
            }
            OrchestrationV2ShellStreamItem::ProjectRemoved {
                sequence,
                project_id,
            } => Self::Event(ShellStreamEvent::ProjectRemoved {
                sequence,
                project_id,
            }),
            OrchestrationV2ShellStreamItem::ThreadUpdated {
                sequence,
                location,
                thread,
            } => Self::Event(ShellStreamEvent::ThreadUpdated {
                sequence,
                location,
                thread: Arc::new(thread),
            }),
            OrchestrationV2ShellStreamItem::ThreadRemoved {
                sequence,
                location,
                thread_id,
            } => Self::Event(ShellStreamEvent::ThreadRemoved {
                sequence,
                location,
                thread_id,
            }),
        }
    }
}

impl ShellStreamEvent {
    pub fn sequence(&self) -> NonNegativeInt {
        match self {
            Self::ProjectUpdated { sequence, .. }
            | Self::ProjectRemoved { sequence, .. }
            | Self::ThreadUpdated { sequence, .. }
            | Self::ThreadRemoved { sequence, .. } => *sequence,
        }
    }
}

/// Whether two rows' pull request links are equal, including both absent.
pub fn same_thread_pull_requests(
    left: Option<&[ThreadPullRequestLink]>,
    right: Option<&[ThreadPullRequestLink]>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => std::ptr::eq(left, right) || left == right,
        _ => false,
    }
}

/// Keep the previous object for each row a full snapshot left unchanged, so the list only
/// re-renders rows that changed. Links are compared apart from the rest of the row because a
/// deferred snapshot arrives without them; such a row keeps its links until they fill in.
pub fn reuse_unchanged_thread_shells(
    previous: Option<&ShellSnapshot>,
    next: ShellSnapshot,
) -> ShellSnapshot {
    let Some(previous) = previous else {
        return next;
    };
    if previous.threads.is_empty() {
        return next;
    }
    let previous_by_id: HashMap<&ThreadId, &ThreadShellRow> = previous
        .threads
        .iter()
        .map(|thread| (&thread.id, thread))
        .collect();
    let mut reused = 0usize;
    let threads: Vec<ThreadShellRow> = next
        .threads
        .iter()
        .map(|thread| {
            let Some(prior) = previous_by_id.get(&thread.id) else {
                return Arc::clone(thread);
            };
            let same = if thread.pull_requests.is_none() && prior.pull_requests.is_some() {
                let candidate = OrchestrationV2ThreadShell {
                    pull_requests: prior.pull_requests.clone(),
                    ..(**thread).clone()
                };
                ***prior == candidate
            } else {
                ***prior == **thread
            };
            if same {
                reused += 1;
                Arc::clone(prior)
            } else {
                Arc::clone(thread)
            }
        })
        .collect();
    if reused == 0 {
        return next;
    }
    ShellSnapshot {
        threads: Arc::new(threads),
        ..next
    }
}

fn upsert_by<T, K: PartialEq>(
    items: &[Arc<T>],
    item: Arc<T>,
    key: impl Fn(&T) -> &K,
) -> Vec<Arc<T>> {
    match items
        .iter()
        .position(|candidate| key(candidate) == key(&item))
    {
        None => {
            let mut next = items.to_vec();
            next.push(item);
            next
        }
        Some(index) => {
            let mut next = items.to_vec();
            next[index] = item;
            next
        }
    }
}

fn identity_of(
    project: &OrchestrationProjectShell,
) -> Option<&crate::state::contracts::RepositoryIdentity> {
    project
        .repository_identity
        .as_ref()
        .and_then(|identity| identity.as_ref())
}

fn retain_repository_identity(
    previous: Option<&ProjectShellRow>,
    next: ProjectShellRow,
) -> ProjectShellRow {
    if identity_of(&next).is_none()
        && let Some(previous) = previous
        && identity_of(previous).is_some()
        && previous.workspace_root == next.workspace_root
    {
        return Arc::new(OrchestrationProjectShell {
            repository_identity: previous.repository_identity.clone(),
            ..(*next).clone()
        });
    }
    next
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MergeShellSnapshotOptions {
    /// Metadata-only enrichment refresh: structure and sequence never change;
    /// listed roots accept identity exactly (including null).
    /// Omit this options object for authoritative initial snapshots.
    pub resolved_repository_identity_roots: Vec<String>,
}

/// Merge an incoming full shell snapshot into prior client state.
///
/// Authoritative snapshots (no options) replace structure and sequence even when
/// lower than cache, while retaining a prior non-null identity when the candidate
/// is still unresolved/null for the same root.
///
/// Enrichment snapshots (options present) only patch repository identity for
/// matching current projects. They never replace projects, threads, archives,
/// or sequence, regardless of the incoming snapshot sequence.
pub fn merge_shell_snapshot_projects(
    previous: Option<&ShellSnapshot>,
    next: ShellSnapshot,
    options: Option<&MergeShellSnapshotOptions>,
) -> ShellSnapshot {
    let Some(previous) = previous else {
        return next;
    };

    if let Some(options) = options {
        let resolved_roots: std::collections::HashSet<&str> = options
            .resolved_repository_identity_roots
            .iter()
            .map(String::as_str)
            .collect();
        let next_by_id: HashMap<&ProjectId, &ProjectShellRow> = next
            .projects
            .iter()
            .map(|project| (&project.id, project))
            .collect();
        let projects = previous
            .projects
            .iter()
            .map(|project| {
                let Some(candidate) = next_by_id.get(&project.id) else {
                    return Arc::clone(project);
                };
                if candidate.workspace_root != project.workspace_root {
                    return Arc::clone(project);
                }
                if resolved_roots.contains(project.workspace_root.as_str()) {
                    return Arc::new(OrchestrationProjectShell {
                        repository_identity: candidate.repository_identity.clone(),
                        ..(**project).clone()
                    });
                }
                if identity_of(project).is_none() && identity_of(candidate).is_some() {
                    return Arc::new(OrchestrationProjectShell {
                        repository_identity: candidate.repository_identity.clone(),
                        ..(**project).clone()
                    });
                }
                Arc::clone(project)
            })
            .collect();
        return ShellSnapshot {
            projects: Arc::new(projects),
            ..previous.clone()
        };
    }

    let previous_by_id: HashMap<&ProjectId, &ProjectShellRow> = previous
        .projects
        .iter()
        .map(|project| (&project.id, project))
        .collect();
    let projects = next
        .projects
        .iter()
        .map(|project| {
            retain_repository_identity(
                previous_by_id.get(&project.id).copied(),
                Arc::clone(project),
            )
        })
        .collect();
    ShellSnapshot {
        projects: Arc::new(projects),
        ..next
    }
}

/// Applies one committed V2 shell delta while preserving active/archive exclusivity.
/// `None` means the delta changed nothing (upstream returns the same snapshot object).
pub fn apply_shell_stream_event(
    snapshot: &ShellSnapshot,
    event: &ShellStreamEvent,
) -> Option<ShellSnapshot> {
    if event.sequence() <= snapshot.snapshot_sequence {
        return None;
    }
    let sequence = event.sequence();

    match event {
        ShellStreamEvent::ProjectUpdated { project, .. } => {
            // Enrichment is async. A project mutation can land with null
            // repositoryIdentity while an earlier snapshot already resolved it.
            // Keep the prior identity for the same workspace root so multi-env
            // grouping does not split until a full snapshot refresh arrives.
            let previous = snapshot
                .projects
                .iter()
                .find(|candidate| candidate.id == project.id);
            let project = retain_repository_identity(previous, Arc::clone(project));
            Some(ShellSnapshot {
                projects: Arc::new(upsert_by(&snapshot.projects, project, |p| &p.id)),
                snapshot_sequence: sequence,
                ..snapshot.clone()
            })
        }
        ShellStreamEvent::ProjectRemoved { project_id, .. } => Some(ShellSnapshot {
            projects: Arc::new(
                snapshot
                    .projects
                    .iter()
                    .filter(|project| project.id != *project_id)
                    .cloned()
                    .collect(),
            ),
            snapshot_sequence: sequence,
            ..snapshot.clone()
        }),
        ShellStreamEvent::ThreadUpdated {
            location, thread, ..
        } => {
            // An unchanged shell keeps its object and the list, so subscribers that
            // compare by reference skip the update. Only the cursor moves.
            let existing = if *location == OrchestrationV2ShellLocation::Active {
                snapshot
                    .threads
                    .iter()
                    .find(|candidate| candidate.id == thread.id)
            } else {
                None
            };
            if let Some(existing) = existing
                && **existing == **thread
            {
                return Some(ShellSnapshot {
                    snapshot_sequence: sequence,
                    ..snapshot.clone()
                });
            }
            let without_thread = |threads: &Arc<Vec<ThreadShellRow>>| -> Arc<Vec<ThreadShellRow>> {
                if threads.iter().any(|candidate| candidate.id == thread.id) {
                    Arc::new(
                        threads
                            .iter()
                            .filter(|candidate| candidate.id != thread.id)
                            .cloned()
                            .collect(),
                    )
                } else {
                    Arc::clone(threads)
                }
            };
            Some(ShellSnapshot {
                threads: if *location == OrchestrationV2ShellLocation::Active {
                    Arc::new(upsert_by(&snapshot.threads, Arc::clone(thread), |t| &t.id))
                } else {
                    without_thread(&snapshot.threads)
                },
                // The archive has its own bounded query/subscription. Older servers may
                // still send archive-located deltas here; remove them from the normal
                // shell instead of growing its persisted cache again.
                archived_threads: without_thread(&snapshot.archived_threads),
                snapshot_sequence: sequence,
                ..snapshot.clone()
            })
        }
        ShellStreamEvent::ThreadRemoved { thread_id, .. } => {
            let without = |threads: &Arc<Vec<ThreadShellRow>>| -> Arc<Vec<ThreadShellRow>> {
                Arc::new(
                    threads
                        .iter()
                        .filter(|thread| thread.id != *thread_id)
                        .cloned()
                        .collect(),
                )
            };
            Some(ShellSnapshot {
                threads: without(&snapshot.threads),
                archived_threads: without(&snapshot.archived_threads),
                snapshot_sequence: sequence,
                ..snapshot.clone()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fixtures::*;
    use serde_json::json;

    fn repository_identity(name: &str) -> crate::state::contracts::RepositoryIdentity {
        serde_json::from_value(json!({
            "canonicalKey": format!("github.com/example/{name}"),
            "locator": {
                "source": "git-remote",
                "remoteName": "origin",
                "remoteUrl": format!("https://github.com/example/{name}.git"),
            },
        }))
        .unwrap()
    }

    fn with_identity(
        project: &OrchestrationProjectShell,
        identity: Option<crate::state::contracts::RepositoryIdentity>,
    ) -> OrchestrationProjectShell {
        OrchestrationProjectShell {
            repository_identity: Some(identity),
            ..project.clone()
        }
    }

    fn snapshot(
        sequence: u64,
        projects: Vec<OrchestrationProjectShell>,
        threads: Vec<OrchestrationV2ThreadShell>,
    ) -> ShellSnapshot {
        ShellSnapshot {
            schema_version: PositiveInt::new(1).unwrap(),
            snapshot_sequence: sequence,
            threads: Arc::new(threads.into_iter().map(Arc::new).collect()),
            archived_threads: Arc::new(Vec::new()),
            projects: Arc::new(projects.into_iter().map(Arc::new).collect()),
        }
    }

    fn thread_titled(title: &str) -> OrchestrationV2ThreadShell {
        OrchestrationV2ThreadShell {
            title: title.into(),
            ..v2_thread_shell()
        }
    }

    fn thread_with_id(id: &str) -> OrchestrationV2ThreadShell {
        OrchestrationV2ThreadShell {
            id: ThreadId::new(id).unwrap(),
            ..v2_thread_shell()
        }
    }

    fn thread_updated(
        sequence: u64,
        location: OrchestrationV2ShellLocation,
        thread: OrchestrationV2ThreadShell,
    ) -> ShellStreamEvent {
        ShellStreamEvent::ThreadUpdated {
            sequence,
            location,
            thread: Arc::new(thread),
        }
    }

    fn project_updated(sequence: u64, project: OrchestrationProjectShell) -> ShellStreamEvent {
        ShellStreamEvent::ProjectUpdated {
            sequence,
            project: Arc::new(project),
        }
    }

    fn apply(snapshot: &ShellSnapshot, event: &ShellStreamEvent) -> ShellSnapshot {
        apply_shell_stream_event(snapshot, event).unwrap_or_else(|| snapshot.clone())
    }

    fn project_identity(
        snapshot: &ShellSnapshot,
        index: usize,
    ) -> Option<Option<crate::state::contracts::RepositoryIdentity>> {
        snapshot.projects[index].repository_identity.clone()
    }

    fn title(project: &OrchestrationProjectShell, value: &str) -> OrchestrationProjectShell {
        OrchestrationProjectShell {
            title: crate::state::contracts::TrimmedNonEmptyString::new(value).unwrap(),
            ..project.clone()
        }
    }

    fn other_project() -> OrchestrationProjectShell {
        OrchestrationProjectShell {
            id: ProjectId::new("project-other").unwrap(),
            workspace_root: crate::state::contracts::TrimmedNonEmptyString::new("/workspace/other")
                .unwrap(),
            repository_identity: Some(Some(repository_identity("other"))),
            ..v2_project()
        }
    }

    #[test]
    fn updates_a_thread_in_place_without_moving_its_siblings() {
        let threads: Vec<_> = ["a", "b", "c"].into_iter().map(thread_with_id).collect();
        let base = snapshot(0, vec![], threads);
        let updated = OrchestrationV2ThreadShell {
            title: "Streaming".into(),
            ..(*base.threads[1]).clone()
        };
        let next = apply(
            &base,
            &thread_updated(1, OrchestrationV2ShellLocation::Active, updated.clone()),
        );
        assert_eq!(
            next.threads
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        assert!(Arc::ptr_eq(&next.threads[0], &base.threads[0]));
        assert_eq!(*next.threads[1], updated);
        assert!(Arc::ptr_eq(&next.threads[2], &base.threads[2]));
    }

    #[test]
    fn keeps_the_thread_list_and_object_when_an_update_changes_nothing() {
        let threads: Vec<_> = ["a", "b"].into_iter().map(thread_with_id).collect();
        let base = snapshot(0, vec![], threads);
        let same = (*base.threads[1]).clone();
        let next = apply(
            &base,
            &thread_updated(1, OrchestrationV2ShellLocation::Active, same),
        );
        assert!(Arc::ptr_eq(&next.threads, &base.threads));
        assert_eq!(next.snapshot_sequence, 1);
    }

    #[test]
    fn ignores_stale_project_updates_without_mutating_the_snapshot() {
        let base = snapshot(4, vec![v2_project()], vec![]);
        for sequence in [3, 4] {
            let event = project_updated(sequence, title(&v2_project(), "Stale Title"));
            assert_eq!(apply_shell_stream_event(&base, &event), None);
            assert_eq!(base.snapshot_sequence, 4);
            assert_eq!(base.projects[0].title, v2_project().title);
        }
    }

    #[test]
    fn applies_project_updates_and_removals() {
        let base = snapshot(0, vec![v2_project()], vec![v2_thread_shell()]);
        let updated = apply(&base, &project_updated(1, title(&v2_project(), "Updated")));
        assert_eq!(updated.projects[0].title.as_str(), "Updated");
        assert_eq!(updated.snapshot_sequence, 1);

        let removed = apply(
            &updated,
            &ShellStreamEvent::ProjectRemoved {
                sequence: 2,
                project_id: v2_project().id,
            },
        );
        assert!(removed.projects.is_empty());
    }

    #[test]
    fn keeps_prior_repository_identity_when_a_delta_arrives_with_null_identity() {
        let base = snapshot(0, vec![v2_project()], vec![]);
        let identity = repository_identity("repo");
        let with = apply(
            &base,
            &project_updated(1, with_identity(&v2_project(), Some(identity.clone()))),
        );
        assert_eq!(project_identity(&with, 0), Some(Some(identity.clone())));

        let without = apply(
            &with,
            &project_updated(
                2,
                with_identity(&title(&v2_project(), "Still same root"), None),
            ),
        );
        assert_eq!(without.projects[0].title.as_str(), "Still same root");
        assert_eq!(project_identity(&without, 0), Some(Some(identity)));
    }

    #[test]
    fn does_not_keep_prior_repository_identity_after_a_workspace_root_move() {
        let base = snapshot(0, vec![v2_project()], vec![]);
        let with = apply(
            &base,
            &project_updated(
                1,
                with_identity(&v2_project(), Some(repository_identity("repo"))),
            ),
        );
        let moved = OrchestrationProjectShell {
            workspace_root: crate::state::contracts::TrimmedNonEmptyString::new("/tmp/other-root")
                .unwrap(),
            ..with_identity(&v2_project(), None)
        };
        let moved = apply(&with, &project_updated(2, moved));
        assert_eq!(moved.projects[0].workspace_root.as_str(), "/tmp/other-root");
        assert_eq!(project_identity(&moved, 0), Some(None));
    }

    #[test]
    fn merges_full_snapshot_projects_without_dropping_known_repository_identity() {
        let identity = repository_identity("repo");
        let previous = snapshot(
            0,
            vec![with_identity(&v2_project(), Some(identity.clone()))],
            vec![v2_thread_shell()],
        );
        let next = merge_shell_snapshot_projects(
            Some(&previous),
            snapshot(
                2,
                vec![with_identity(&title(&v2_project(), "Refreshed"), None)],
                vec![v2_thread_shell()],
            ),
            None,
        );
        assert_eq!(next.projects[0].title.as_str(), "Refreshed");
        assert_eq!(project_identity(&next, 0), Some(Some(identity)));
    }

    #[test]
    fn authoritative_lower_sequence_reset_replaces_client_ahead_structural_state() {
        let identity = repository_identity("repo");
        let previous = snapshot(
            9,
            vec![with_identity(
                &title(&v2_project(), "Client ahead"),
                Some(identity.clone()),
            )],
            vec![thread_titled("Local-only thread")],
        );
        let next = merge_shell_snapshot_projects(
            Some(&previous),
            snapshot(
                3,
                vec![with_identity(&title(&v2_project(), "Server reset"), None)],
                vec![thread_titled("Server thread")],
            ),
            None,
        );
        assert_eq!(next.snapshot_sequence, 3);
        assert_eq!(next.projects[0].title.as_str(), "Server reset");
        assert_eq!(project_identity(&next, 0), Some(Some(identity)));
        assert_eq!(next.threads[0].title, "Server thread");
    }

    fn enrichment_for(roots: &[&str]) -> MergeShellSnapshotOptions {
        MergeShellSnapshotOptions {
            resolved_repository_identity_roots: roots
                .iter()
                .map(|root| (*root).to_owned())
                .collect(),
        }
    }

    #[test]
    fn client_ahead_survives_lower_marked_enrichment_then_still_resets_on_unmarked_authoritative() {
        let identity = repository_identity("repo");
        let previous = snapshot(
            20,
            vec![with_identity(
                &title(&v2_project(), "Client ahead"),
                Some(identity),
            )],
            vec![thread_titled("Local-only thread")],
        );
        let server = snapshot(
            3,
            vec![with_identity(&title(&v2_project(), "Server reset"), None)],
            vec![thread_titled("Server thread")],
        );

        // Lower-sequence marked enrichment is identity-only and must not replace structure.
        let after_enrichment = merge_shell_snapshot_projects(
            Some(&previous),
            server.clone(),
            Some(&enrichment_for(&["/workspace/project"])),
        );
        assert_eq!(after_enrichment.snapshot_sequence, 20);
        assert_eq!(after_enrichment.projects[0].title.as_str(), "Client ahead");
        assert_eq!(project_identity(&after_enrichment, 0), Some(None));
        assert_eq!(after_enrichment.threads[0].title, "Local-only thread");

        // Unmarked authoritative at the same lower sequence still performs the reset.
        let after_authoritative =
            merge_shell_snapshot_projects(Some(&after_enrichment), server, None);
        assert_eq!(after_authoritative.snapshot_sequence, 3);
        assert_eq!(
            after_authoritative.projects[0].title.as_str(),
            "Server reset"
        );
        assert_eq!(project_identity(&after_authoritative, 0), Some(None));
        assert_eq!(after_authoritative.threads[0].title, "Server thread");
    }

    #[test]
    fn authoritative_null_retains_identity_then_same_sequence_marked_enrichment_clears_it() {
        let identity = repository_identity("repo");
        let previous = snapshot(
            9,
            vec![with_identity(
                &title(&v2_project(), "Client ahead"),
                Some(identity.clone()),
            )],
            vec![thread_titled("Local-only thread")],
        );
        let server = snapshot(
            3,
            vec![with_identity(&title(&v2_project(), "Server reset"), None)],
            vec![thread_titled("Server thread")],
        );

        let after_authoritative =
            merge_shell_snapshot_projects(Some(&previous), server.clone(), None);
        assert_eq!(after_authoritative.snapshot_sequence, 3);
        assert_eq!(
            after_authoritative.projects[0].title.as_str(),
            "Server reset"
        );
        assert_eq!(
            project_identity(&after_authoritative, 0),
            Some(Some(identity))
        );
        assert_eq!(after_authoritative.threads[0].title, "Server thread");

        let after_enrichment = merge_shell_snapshot_projects(
            Some(&after_authoritative),
            server,
            Some(&enrichment_for(&["/workspace/project"])),
        );
        assert_eq!(after_enrichment.snapshot_sequence, 3);
        assert_eq!(after_enrichment.projects[0].title.as_str(), "Server reset");
        assert_eq!(project_identity(&after_enrichment, 0), Some(None));
        assert_eq!(after_enrichment.threads[0].title, "Server thread");
    }

    #[test]
    fn takes_identity_from_a_lower_sequence_enrichment_without_reverting_live_thread_state() {
        let identity = repository_identity("repo");
        let previous = snapshot(
            5,
            vec![with_identity(&v2_project(), None)],
            vec![thread_titled("Newest title")],
        );
        let next = merge_shell_snapshot_projects(
            Some(&previous),
            snapshot(
                4,
                vec![with_identity(&v2_project(), Some(identity.clone()))],
                vec![thread_titled("Stale title")],
            ),
            Some(&enrichment_for(&["/workspace/project"])),
        );
        assert_eq!(next.snapshot_sequence, 5);
        assert_eq!(next.threads[0].title, "Newest title");
        assert_eq!(project_identity(&next, 0), Some(Some(identity)));
    }

    #[test]
    fn treats_higher_sequence_compact_enrichment_as_metadata_only() {
        let identity = repository_identity("repo");
        let previous = snapshot(
            5,
            vec![with_identity(&v2_project(), None)],
            vec![thread_titled("Newest title")],
        );
        let next = merge_shell_snapshot_projects(
            Some(&previous),
            snapshot(
                8,
                vec![with_identity(&v2_project(), Some(identity.clone()))],
                vec![],
            ),
            Some(&enrichment_for(&["/workspace/project"])),
        );
        assert_eq!(next.snapshot_sequence, 5);
        assert_eq!(next.threads[0].title, "Newest title");
        assert_eq!(project_identity(&next, 0), Some(Some(identity)));
    }

    #[test]
    fn lower_sequence_enrichment_cannot_resurrect_empty_structural_state() {
        let previous = snapshot(7, vec![], vec![]);
        let next = merge_shell_snapshot_projects(
            Some(&previous),
            snapshot(
                4,
                vec![with_identity(
                    &v2_project(),
                    Some(repository_identity("repo")),
                )],
                vec![v2_thread_shell()],
            ),
            Some(&enrichment_for(&["/workspace/project"])),
        );
        assert_eq!(next.snapshot_sequence, 7);
        assert!(next.projects.is_empty());
        assert!(next.threads.is_empty());
    }

    #[test]
    fn resolved_null_clears_identity_only_for_the_marked_root() {
        let identity = repository_identity("repo");
        let other = other_project();
        let previous = snapshot(
            5,
            vec![with_identity(&v2_project(), Some(identity)), other.clone()],
            vec![],
        );
        let next = merge_shell_snapshot_projects(
            Some(&previous),
            snapshot(
                4,
                vec![
                    with_identity(&v2_project(), None),
                    with_identity(&other, None),
                ],
                vec![],
            ),
            Some(&enrichment_for(&["/workspace/project"])),
        );
        assert_eq!(next.snapshot_sequence, 5);
        assert_eq!(next.projects.len(), 2);
        assert_eq!(project_identity(&next, 0), Some(None));
        assert_eq!(
            project_identity(&next, 1),
            Some(Some(repository_identity("other")))
        );
    }

    #[test]
    fn unrelated_unresolved_roots_retain_identity_in_the_same_enrichment_snapshot() {
        let identity = repository_identity("repo");
        let other = other_project();
        let previous = snapshot(
            3,
            vec![
                with_identity(&v2_project(), Some(identity.clone())),
                other.clone(),
            ],
            vec![thread_titled("Current thread")],
        );
        let next = merge_shell_snapshot_projects(
            Some(&previous),
            snapshot(
                3,
                vec![
                    with_identity(&v2_project(), Some(identity.clone())),
                    with_identity(&other, None),
                ],
                vec![thread_titled("Stale thread")],
            ),
            Some(&enrichment_for(&["/workspace/project"])),
        );
        assert_eq!(next.threads[0].title, "Current thread");
        assert_eq!(project_identity(&next, 0), Some(Some(identity)));
        assert_eq!(
            project_identity(&next, 1),
            Some(Some(repository_identity("other")))
        );
    }

    #[test]
    fn clears_identity_for_multiple_roots_resolved_in_one_enrichment_batch() {
        let other = other_project();
        let previous = snapshot(
            3,
            vec![
                with_identity(&v2_project(), Some(repository_identity("repo"))),
                other.clone(),
            ],
            vec![],
        );
        let next = merge_shell_snapshot_projects(
            Some(&previous),
            snapshot(
                3,
                vec![
                    with_identity(&v2_project(), None),
                    with_identity(&other, None),
                ],
                vec![],
            ),
            Some(&enrichment_for(&["/workspace/project", "/workspace/other"])),
        );
        assert_eq!(
            next.projects
                .iter()
                .map(|p| p.repository_identity.clone())
                .collect::<Vec<_>>(),
            vec![Some(None), Some(None)]
        );
    }

    #[test]
    fn does_not_replace_a_live_identity_with_one_from_an_unmarked_lower_sequence_enrichment() {
        let live = repository_identity("live");
        let next = merge_shell_snapshot_projects(
            Some(&snapshot(
                5,
                vec![with_identity(&v2_project(), Some(live.clone()))],
                vec![],
            )),
            snapshot(
                4,
                vec![with_identity(
                    &v2_project(),
                    Some(repository_identity("stale")),
                )],
                vec![],
            ),
            Some(&enrichment_for(&[])),
        );
        assert_eq!(next.snapshot_sequence, 5);
        assert_eq!(project_identity(&next, 0), Some(Some(live)));
    }

    #[test]
    fn keeps_archive_deltas_out_of_the_normal_shell_cache() {
        let base = snapshot(0, vec![], vec![]);
        let archived = apply(
            &base,
            &thread_updated(
                3,
                OrchestrationV2ShellLocation::Archive,
                OrchestrationV2ThreadShell {
                    archived_at: Some(v2_thread_shell().updated_at),
                    ..v2_thread_shell()
                },
            ),
        );
        assert!(archived.threads.is_empty());
        assert!(archived.archived_threads.is_empty());

        let active = apply(
            &archived,
            &thread_updated(4, OrchestrationV2ShellLocation::Active, v2_thread_shell()),
        );
        assert_eq!(active.threads.len(), 1);
        assert!(active.archived_threads.is_empty());
    }

    #[test]
    fn removes_a_thread_from_either_collection() {
        let base = snapshot(0, vec![], vec![v2_thread_shell()]);
        let next = apply(
            &base,
            &ShellStreamEvent::ThreadRemoved {
                sequence: 5,
                location: OrchestrationV2ShellLocation::Active,
                thread_id: v2_thread_shell().id,
            },
        );
        assert!(next.threads.is_empty());
        assert_eq!(next.snapshot_sequence, 5);
    }

    fn link() -> ThreadPullRequestLink {
        serde_json::from_value(json!({
            "host": "github.com",
            "repository": "pingdotgg/t3code",
            "number": 3,
            "url": "https://github.com/pingdotgg/t3code/pull/3",
            "source": "agent",
            "linkedAt": "2026-06-20T00:00:00.000Z",
            "snapshot": null,
            "stack": null,
        }))
        .unwrap()
    }

    #[test]
    fn reuse_keeps_the_previous_object_for_an_unchanged_row_even_without_its_deferred_links() {
        let linked = OrchestrationV2ThreadShell {
            pull_requests: Some(vec![link()]),
            ..v2_thread_shell()
        };
        let other = OrchestrationV2ThreadShell {
            id: ThreadId::new("thread-other").unwrap(),
            title: "Other".into(),
            ..v2_thread_shell()
        };
        let previous = snapshot(0, vec![], vec![linked.clone(), other.clone()]);
        let without_links = OrchestrationV2ThreadShell {
            pull_requests: None,
            ..linked
        };
        let next = reuse_unchanged_thread_shells(
            Some(&previous),
            snapshot(
                5,
                vec![],
                vec![
                    without_links,
                    OrchestrationV2ThreadShell {
                        title: "Renamed".into(),
                        ..other
                    },
                ],
            ),
        );
        assert_eq!(next.snapshot_sequence, 5);
        assert!(Arc::ptr_eq(&next.threads[0], &previous.threads[0]));
        assert!(!Arc::ptr_eq(&next.threads[1], &previous.threads[1]));
        assert_eq!(next.threads[1].title, "Renamed");
    }

    #[test]
    fn reuse_returns_the_incoming_snapshot_when_no_row_is_unchanged() {
        let other = OrchestrationV2ThreadShell {
            id: ThreadId::new("thread-other").unwrap(),
            title: "Other".into(),
            ..v2_thread_shell()
        };
        let incoming = snapshot(
            0,
            vec![],
            vec![OrchestrationV2ThreadShell {
                title: "Renamed".into(),
                ..other.clone()
            }],
        );
        let next = reuse_unchanged_thread_shells(
            Some(&snapshot(0, vec![], vec![other])),
            incoming.clone(),
        );
        assert!(Arc::ptr_eq(&next.threads, &incoming.threads));
    }
}
