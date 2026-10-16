//! Port of upstream/packages/client-runtime/src/worktreeSetup.ts and
//! upstream/apps/web/src/components/ChatView.logic.ts (`resolveWorktreeSetupProgress`).

use crate::state::contracts::ThreadId;

use crate::state::models::RunStatus;

pub const WORKTREE_SETUP_ACTIVITY_KIND: &str = "worktree-setup";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeSetupStageId {
    Fetch,
    Checkout,
    Submodules,
    SetupScript,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeSetupStageStatus {
    Pending,
    Running,
    Done,
    Skipped,
    Warning,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeSetupStage {
    pub id: WorktreeSetupStageId,
    pub status: WorktreeSetupStageStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeSetupPhase {
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeSetupSnapshot {
    pub thread_id: ThreadId,
    pub phase: WorktreeSetupPhase,
    pub stages: Vec<WorktreeSetupStage>,
    pub sequence: u64,
    pub error: Option<String>,
}

/// The live stream wins while its sequence is newer or equal; otherwise the other side.
fn newer_worktree_snapshot<'a>(
    primary: Option<&'a WorktreeSetupSnapshot>,
    secondary: Option<&'a WorktreeSetupSnapshot>,
) -> Option<&'a WorktreeSetupSnapshot> {
    match (primary, secondary) {
        (Some(primary), Some(secondary)) if primary.sequence >= secondary.sequence => Some(primary),
        (Some(_), Some(secondary)) => Some(secondary),
        (Some(primary), None) => Some(primary),
        (None, Some(secondary)) => Some(secondary),
        (None, None) => None,
    }
}

pub fn worktree_setup_agent_started(snapshot: &WorktreeSetupSnapshot) -> bool {
    snapshot.stages.iter().any(|stage| {
        stage.id == WorktreeSetupStageId::Agent && stage.status == WorktreeSetupStageStatus::Done
    })
}

pub fn find_recorded_worktree_setup(
    activities: &[(impl AsRef<str>, WorktreeSetupSnapshot)],
    thread_id: &ThreadId,
) -> Option<WorktreeSetupSnapshot> {
    for (kind, snapshot) in activities.iter().rev() {
        if kind.as_ref() != WORKTREE_SETUP_ACTIVITY_KIND {
            continue;
        }
        if snapshot.thread_id == *thread_id {
            return Some(snapshot.clone());
        }
    }
    None
}

pub fn resolve_visible_worktree_setup(
    input: ResolveVisibleWorktreeSetupInput<'_>,
) -> Option<WorktreeSetupSnapshot> {
    let snapshot = newer_worktree_snapshot(input.live, input.recorded).cloned()?;
    if snapshot.phase == WorktreeSetupPhase::Running {
        return Some(snapshot);
    }
    if input.follow_up_sent {
        return None;
    }
    if snapshot.phase != WorktreeSetupPhase::Done {
        return Some(snapshot);
    }
    if !input.turn_started {
        return Some(snapshot);
    }
    if snapshot
        .stages
        .iter()
        .any(|stage| stage.status == WorktreeSetupStageStatus::Failed)
    {
        Some(snapshot)
    } else {
        None
    }
}

#[derive(Debug, Clone)]
pub struct ResolveVisibleWorktreeSetupInput<'a> {
    pub live: Option<&'a WorktreeSetupSnapshot>,
    pub recorded: Option<&'a WorktreeSetupSnapshot>,
    pub turn_started: bool,
    pub follow_up_sent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeSetupProgress {
    pub snapshot: Option<WorktreeSetupSnapshot>,
    pub is_preparing_worktree: bool,
}

pub fn resolve_worktree_setup_progress(
    input: ResolveWorktreeSetupProgressInput<'_>,
) -> WorktreeSetupProgress {
    let latest = input
        .latest
        .filter(|snapshot| snapshot.thread_id == *input.thread_id)
        .cloned();
    let held = input
        .held
        .filter(|snapshot| snapshot.thread_id == *input.thread_id)
        .cloned();
    let snapshot = newer_worktree_snapshot(latest.as_ref(), held.as_ref()).cloned();
    let is_preparing_worktree = input.local_preparing
        || input.run_status == Some(RunStatus::Preparing)
        || snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.phase == WorktreeSetupPhase::Running && !worktree_setup_agent_started(snapshot)
        });
    WorktreeSetupProgress {
        snapshot,
        is_preparing_worktree,
    }
}

#[derive(Debug, Clone)]
pub struct ResolveWorktreeSetupProgressInput<'a> {
    pub thread_id: &'a ThreadId,
    pub local_preparing: bool,
    pub run_status: Option<RunStatus>,
    pub latest: Option<&'a WorktreeSetupSnapshot>,
    pub held: Option<&'a WorktreeSetupSnapshot>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::models::fixtures;

    fn stage(id: WorktreeSetupStageId, status: WorktreeSetupStageStatus) -> WorktreeSetupStage {
        WorktreeSetupStage { id, status }
    }

    fn base_snapshot(thread_id: ThreadId) -> WorktreeSetupSnapshot {
        WorktreeSetupSnapshot {
            thread_id,
            phase: WorktreeSetupPhase::Running,
            stages: vec![
                stage(
                    WorktreeSetupStageId::Checkout,
                    WorktreeSetupStageStatus::Running,
                ),
                stage(
                    WorktreeSetupStageId::Agent,
                    WorktreeSetupStageStatus::Pending,
                ),
            ],
            sequence: 1,
            error: None,
        }
    }

    #[test]
    fn keeps_setup_visible_until_provider_handoff() {
        let thread_id = fixtures::thread_id("thread-1");
        let base = base_snapshot(thread_id.clone());
        let progress = |local_preparing: bool,
                        run_status: Option<RunStatus>,
                        latest: Option<&WorktreeSetupSnapshot>,
                        held: Option<&WorktreeSetupSnapshot>| {
            resolve_worktree_setup_progress(ResolveWorktreeSetupProgressInput {
                thread_id: &thread_id,
                local_preparing,
                run_status,
                latest,
                held,
            })
        };
        assert!(progress(true, None, None, None).is_preparing_worktree);
        assert!(progress(false, Some(RunStatus::Preparing), None, None).is_preparing_worktree);
        assert_eq!(
            progress(false, Some(RunStatus::Preparing), Some(&base), None).snapshot,
            Some(base.clone())
        );
        let handed_off = WorktreeSetupSnapshot {
            sequence: 2,
            stages: vec![
                stage(
                    WorktreeSetupStageId::SetupScript,
                    WorktreeSetupStageStatus::Running,
                ),
                stage(WorktreeSetupStageId::Agent, WorktreeSetupStageStatus::Done),
            ],
            ..base.clone()
        };
        let result = progress(
            false,
            Some(RunStatus::Starting),
            Some(&handed_off),
            Some(&base),
        );
        assert_eq!(result.snapshot, Some(handed_off));
        assert!(!result.is_preparing_worktree);
    }

    #[test]
    fn resolve_visible_worktree_setup_prefers_newer_sequences() {
        let thread_id = fixtures::thread_id("thread-1");
        let base = base_snapshot(thread_id.clone());
        let settled = WorktreeSetupSnapshot {
            phase: WorktreeSetupPhase::Done,
            stages: vec![stage(
                WorktreeSetupStageId::Agent,
                WorktreeSetupStageStatus::Done,
            )],
            ..base.clone()
        };
        let newest = WorktreeSetupSnapshot {
            sequence: 9,
            ..settled.clone()
        };
        assert_eq!(
            resolve_visible_worktree_setup(ResolveVisibleWorktreeSetupInput {
                live: Some(&newest),
                recorded: Some(&base),
                turn_started: false,
                follow_up_sent: false,
            }),
            Some(newest)
        );
        assert_eq!(
            find_recorded_worktree_setup(
                &[
                    ("setup-script.started".to_owned(), base.clone()),
                    (WORKTREE_SETUP_ACTIVITY_KIND.to_owned(), settled.clone()),
                ],
                &thread_id,
            ),
            Some(settled.clone())
        );
        let recorded_newer = WorktreeSetupSnapshot {
            sequence: 7,
            ..settled
        };
        assert_eq!(
            resolve_visible_worktree_setup(ResolveVisibleWorktreeSetupInput {
                live: Some(&WorktreeSetupSnapshot {
                    sequence: 3,
                    ..base
                }),
                recorded: Some(&recorded_newer),
                turn_started: false,
                follow_up_sent: false,
            }),
            Some(recorded_newer)
        );
    }

    #[test]
    fn uses_streamed_setup_progress_without_reverting_to_an_older_held_snapshot() {
        let thread_id = fixtures::thread_id("thread-1");
        let base = base_snapshot(thread_id.clone());
        let newest = WorktreeSetupSnapshot {
            phase: WorktreeSetupPhase::Done,
            sequence: 9,
            stages: vec![stage(
                WorktreeSetupStageId::Agent,
                WorktreeSetupStageStatus::Done,
            )],
            ..base.clone()
        };
        let resolve = |latest: Option<&WorktreeSetupSnapshot>,
                       held: Option<&WorktreeSetupSnapshot>| {
            resolve_worktree_setup_progress(ResolveWorktreeSetupProgressInput {
                thread_id: &thread_id,
                local_preparing: false,
                run_status: Some(RunStatus::Running),
                latest,
                held,
            })
        };
        assert_eq!(
            resolve(Some(&newest), Some(&base)),
            WorktreeSetupProgress {
                snapshot: Some(newest.clone()),
                is_preparing_worktree: false,
            }
        );
        assert_eq!(
            resolve(Some(&base), Some(&newest)),
            WorktreeSetupProgress {
                snapshot: Some(newest),
                is_preparing_worktree: false,
            }
        );
        let other = WorktreeSetupSnapshot {
            thread_id: fixtures::thread_id("another-thread"),
            ..base
        };
        assert_eq!(
            resolve(Some(&other), Some(&other)),
            WorktreeSetupProgress {
                snapshot: None,
                is_preparing_worktree: false,
            }
        );
    }
}
