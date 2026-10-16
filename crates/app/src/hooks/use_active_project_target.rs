//! Port of upstream/apps/web/src/hooks/useActiveProjectTarget.ts (pure resolution).

use turin_contracts::environment::ScopedThreadRef;
use turin_contracts::{EnvironmentId, ProjectId, ThreadId};

/// `ActiveProjectTarget`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveProjectTarget {
    pub environment_id: EnvironmentId,
    pub cwd: String,
    pub project_name: String,
    pub thread_ref: ScopedThreadRef,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveThreadFacts {
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
    pub thread_id: ThreadId,
    pub worktree_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectFacts {
    pub environment_id: EnvironmentId,
    pub title: String,
    pub workspace_root: Option<String>,
}

/// Resolves the workspace behind the active thread or draft.
pub fn resolve_active_project_target(
    thread: Option<&ActiveThreadFacts>,
    project: Option<&ProjectFacts>,
) -> Option<ActiveProjectTarget> {
    let thread = thread?;
    let project = project?;
    let cwd = thread
        .worktree_path
        .clone()
        .or_else(|| project.workspace_root.clone())?;
    Some(ActiveProjectTarget {
        environment_id: project.environment_id.clone(),
        cwd,
        project_name: project.title.clone(),
        thread_ref: ScopedThreadRef {
            environment_id: thread.environment_id.clone(),
            thread_id: thread.thread_id.clone(),
        },
    })
}
