//! The branch toolbar's data seam: `usePaginatedBranches`,
//! `vcsEnvironment.switchRef` / `createRef`, the thread/draft stores the
//! toolbar writes through (`threadEnvironment.updateMetadata`,
//! `setDraftThreadContext`) and `useThreadShellsForProjectRefs`. Status comes
//! from the git control's backend ([`crate::components::git_actions_control::backend`]).
//!
//! T22 implements it on `turin-git` and the orchestrator;
//! [`super::mock_source`] is the in-memory stand-in. The environment write
//! scopes went with the auth cut.

// A faithful port keeps upstream's whole API (PORTING rule 3); the hosts wire
// the rest as the composer, thread details and settings tasks land.
#![allow(dead_code)]

use std::rc::Rc;

use gpui_kit::{App, Task};
use turin_contracts::git::VcsRef;
use turin_contracts::{EnvironmentId, ThreadId};

use super::branch_toolbar_logic::{EnvMode, EnvironmentOption, WorktreeThread};

/// `usePaginatedBranches` state for the current query.
#[derive(Clone, Debug, Default)]
pub struct RefPage {
    pub refs: Vec<VcsRef>,
    pub total_count: usize,
    pub has_next_page: bool,
    pub is_pending: bool,
    pub is_fetching_next_page: bool,
}

/// A project thread, for the "Previous worktree" lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectThreadInfo {
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub updated_at: String,
    pub archived_at: Option<String>,
}

impl ProjectThreadInfo {
    pub fn as_worktree_thread(&self) -> WorktreeThread<'_> {
        WorktreeThread {
            branch: self.branch.as_deref(),
            worktree_path: self.worktree_path.as_deref(),
            updated_at: &self.updated_at,
            archived_at: self.archived_at.as_deref(),
        }
    }
}

/// What the toolbar knows about the thread (or draft) it belongs to
/// (`useThreadShell`, `getDraftSession`, `useProject`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchThreadContext {
    /// A server thread exists; otherwise this is a draft.
    pub is_server_thread: bool,
    /// The thread has a provider session (`serverSession`).
    pub has_session: bool,
    pub has_active_project: bool,
    pub project_cwd: Option<String>,
    pub project_workspace_root: Option<String>,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub draft_env_mode: Option<EnvMode>,
    pub project_threads: Vec<ProjectThreadInfo>,
    /// `linkedPullRequest` / `pullRequests` of the thread, for the PR badge.
    pub pull_requests: Vec<crate::state::models::ThreadPullRequestLink>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitchRefResult {
    pub ref_name: Option<String>,
}

pub trait BranchBackend {
    /// The ref list for `cwd` filtered by `query`.
    fn list_refs(&self, cwd: &str, query: &str, cx: &App) -> RefPage;
    fn load_next_refs(&self, cwd: &str, query: &str, cx: &mut App);
    /// Starts the first page for `cwd` and `query` when nothing has asked for it
    /// yet. Cheap and idempotent: the view calls it every render.
    fn ensure_refs(&self, _cwd: &str, _query: &str, _cx: &mut App) {}
    /// Reloads the first page (the picker opening: refs change behind the
    /// view's back when a git action or another tool switches branches).
    fn reload_refs(&self, _cwd: &str, _query: &str, _cx: &mut App) {}
    /// `vcsEnvironment.listRefs` for one name (is the active ref remote?).
    fn find_ref(&self, cwd: &str, name: &str, cx: &App) -> Option<VcsRef>;
    fn switch_ref(
        &self,
        cwd: &str,
        ref_name: &str,
        cx: &mut App,
    ) -> Task<Result<SwitchRefResult, String>>;
    /// Creates `name` and switches to it; resolves to the created ref's name.
    fn create_ref(&self, cwd: &str, name: &str, cx: &mut App) -> Task<Result<String, String>>;
    /// `threadEnvironment.updateMetadata` (`expectedEmpty` for automatic defaults).
    fn update_thread_metadata(
        &self,
        environment_id: &EnvironmentId,
        thread_id: &ThreadId,
        branch: Option<String>,
        worktree_path: Option<String>,
        expected_empty: bool,
        cx: &mut App,
    ) -> Task<Result<(), String>>;
    fn stop_thread_session(
        &self,
        environment_id: &EnvironmentId,
        thread_id: &ThreadId,
        cx: &mut App,
    );
    /// The machines a thread can run on.
    fn environments(&self, cx: &App) -> Vec<EnvironmentOption>;
    /// Copies text (`writeTextToClipboard`).
    fn copy_text(&self, text: String, cx: &mut App);
}

pub type SharedBranchBackend = Rc<dyn BranchBackend>;
