//! The sidebar's data seam: everything upstream's `Sidebar` reads from atoms
//! and stores (`useProjects`, `useThreadShells`, `useSidebarProjectGroups`,
//! `environmentServerConfigsAtom`, provider instances, composer drafts,
//! `useThreadSearch`, client settings) gathered into one typed snapshot, and
//! every command it dispatches (`useThreadActions`, `threadEnvironment.updateMetadata`).
//!
//! T14 (client-runtime reducers) owns the real implementation: it keeps a
//! [`SidebarStore`] up to date from its read model and implements
//! [`SidebarBackend`] on the orchestrator. Until then
//! [`super::mock_source`] provides both in memory.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::{App, Context, EventEmitter, Task};
use turin_contracts::{EnvironmentId, ProjectId, ThreadId};

use super::sidebar_logic::{CommandOutcome, ProjectSortOrder};
use crate::components::thread_search_match::SearchMatchSource;
use crate::hooks::use_sidebar_project_groups::{
    SidebarProjectGroupsInput, use_sidebar_project_groups,
};
use crate::state::models::{EnvironmentProject, EnvironmentThreadShell};
use crate::state::project_grouping::ProjectGroupingSettings;
use crate::timestamp_format::TimestampFormat;

/// `environment.capabilities` the sidebar gates on. A capability that is
/// false hides its affordance rather than failing on click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadCapabilities {
    pub thread_settlement: bool,
    pub thread_snooze: bool,
    pub thread_pinning: bool,
    pub thread_pin_reorder: bool,
    pub thread_active_reorder: bool,
    pub thread_auto_settle_opt_out: bool,
    pub thread_title_regeneration: bool,
}

impl ThreadCapabilities {
    /// What Turin's in-process orchestrator supports.
    pub const ALL: Self = Self {
        thread_settlement: true,
        thread_snooze: true,
        thread_pinning: true,
        thread_pin_reorder: true,
        thread_active_reorder: true,
        thread_auto_settle_opt_out: true,
        thread_title_regeneration: true,
    };
}

/// One environment (Turin has one: this machine).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarEnvironment {
    pub id: EnvironmentId,
    pub label: String,
    pub capabilities: ThreadCapabilities,
}

/// The provider driver behind an instance (the four kept providers).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderDriverKind {
    Codex,
    Claude,
    Cursor,
    OpenCode,
}

/// `ProviderInstanceEntry`: what rows show for a provider instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderEntry {
    pub instance_id: String,
    pub display_name: String,
    pub driver_kind: ProviderDriverKind,
    /// `(slug, trigger display label)`.
    pub models: Vec<(String, String)>,
}

/// `SidebarProjectSnapshot`: a logical project (members across
/// environments share one row in the scope menu).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarProjectGroup {
    pub project_key: String,
    pub display_name: String,
    /// The group's project timestamps; the activity sort falls back to them
    /// for a project with no visible threads.
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    /// `${environmentId}:${projectId}` of every member.
    pub member_project_keys: Vec<String>,
}

/// An unsent "new thread" draft with content (`SidebarDraftBlock` rows).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarDraft {
    pub draft_id: String,
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
    /// First line of the prompt with context references replaced by labels.
    pub prompt_preview: String,
    pub attachment_count: usize,
    pub created_at: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
}

impl SidebarDraft {
    pub fn project_key(&self) -> String {
        format!("{}:{}", self.environment_id, self.project_id)
    }
}

/// `EnvironmentThreadSearchMatch`: a server-side message hit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadSearchMatch {
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
    pub source: SearchMatchSource,
    pub snippet: String,
}

/// Client settings the sidebar reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarSettings {
    /// `sidebarWorkingShelfEnabled` (beta, off by default).
    pub working_shelf_enabled: bool,
    pub timestamp_format: TimestampFormat,
    pub confirm_thread_delete: bool,
    pub confirm_thread_archive: bool,
    pub project_sort_order: ProjectSortOrder,
}

impl Default for SidebarSettings {
    fn default() -> Self {
        Self {
            working_shelf_enabled: false,
            timestamp_format: TimestampFormat::Locale,
            confirm_thread_delete: true,
            confirm_thread_archive: false,
            project_sort_order: ProjectSortOrder::UpdatedAt,
        }
    }
}

impl SidebarSettings {
    /// The slice of the saved client settings the sidebar reads
    /// (`useSettings()` in Sidebar.tsx).
    pub fn from_client(settings: &turin_contracts::settings::ClientSettings) -> Self {
        use turin_contracts::settings::SidebarProjectSortOrder;
        Self {
            working_shelf_enabled: settings.sidebar_working_shelf_enabled,
            timestamp_format: match settings.timestamp_format {
                turin_contracts::settings::TimestampFormat::Locale => TimestampFormat::Locale,
                turin_contracts::settings::TimestampFormat::TwelveHour => {
                    TimestampFormat::TwelveHour
                }
                turin_contracts::settings::TimestampFormat::TwentyFourHour => {
                    TimestampFormat::TwentyFourHour
                }
            },
            confirm_thread_delete: settings.confirm_thread_delete,
            confirm_thread_archive: settings.confirm_thread_archive,
            project_sort_order: match settings.sidebar_project_sort_order {
                SidebarProjectSortOrder::UpdatedAt => ProjectSortOrder::UpdatedAt,
                SidebarProjectSortOrder::CreatedAt => ProjectSortOrder::CreatedAt,
                SidebarProjectSortOrder::Manual => ProjectSortOrder::Manual,
            },
        }
    }
}

/// Everything the sidebar renders from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SidebarSnapshot {
    pub projects: Vec<EnvironmentProject>,
    pub project_groups: Vec<SidebarProjectGroup>,
    pub threads: Vec<EnvironmentThreadShell>,
    pub environments: Vec<SidebarEnvironment>,
    pub primary_environment_id: Option<EnvironmentId>,
    pub provider_entries: Vec<ProviderEntry>,
    pub drafts: Vec<SidebarDraft>,
    pub settings: SidebarSettings,
    /// Local `threadLastVisitedAt` watermarks (uiStateStore), keyed by scoped key.
    pub local_last_visited_at: HashMap<String, String>,
    /// The persisted project scope (`sidebarProjectScopeKey`).
    pub project_scope_key: Option<String>,
    /// Persisted shelf toggles (`t3code:sidebar:*-expanded`).
    pub settled_shelf_expanded: bool,
    pub snoozed_shelf_expanded: bool,
    pub working_shelf_expanded: bool,
}

impl SidebarSnapshot {
    pub fn capabilities(&self, environment_id: &EnvironmentId) -> Option<ThreadCapabilities> {
        self.environments
            .iter()
            .find(|environment| &environment.id == environment_id)
            .map(|environment| environment.capabilities)
    }

    pub fn project(&self, project_key: &str) -> Option<&EnvironmentProject> {
        self.projects
            .iter()
            .find(|project| project.key() == project_key)
    }

    pub fn project_group_for(&self, project_key: &str) -> Option<&SidebarProjectGroup> {
        self.project_groups.iter().find(|group| {
            group
                .member_project_keys
                .iter()
                .any(|key| key == project_key)
        })
    }

    pub fn provider_entry(&self, instance_id: &str) -> Option<&ProviderEntry> {
        self.provider_entries
            .iter()
            .find(|entry| entry.instance_id == instance_id)
    }

    /// `useSidebarProjectGroups()` over this snapshot: groups the projects (by repository, per the
    /// grouping settings), orders them (the saved order when manual, else thread activity) and
    /// stores the result as [`Self::project_groups`].
    pub fn refresh_project_groups(
        &mut self,
        project_order: &[String],
        grouping_settings: &ProjectGroupingSettings,
    ) {
        self.project_groups = use_sidebar_project_groups(&SidebarProjectGroupsInput {
            projects: &self.projects,
            threads: &self.threads,
            project_order,
            sort_order: self.settings.project_sort_order,
            grouping_settings,
            environments: &self.environments,
            primary_environment_id: self.primary_environment_id.as_ref(),
        });
    }
}

/// The reactive holder T14 keeps current. The sidebar observes it.
pub struct SidebarStore {
    snapshot: SidebarSnapshot,
}

/// Emitted when the snapshot changes.
pub struct SidebarStoreChanged;

impl EventEmitter<SidebarStoreChanged> for SidebarStore {}

impl SidebarStore {
    pub fn new(snapshot: SidebarSnapshot) -> Self {
        Self { snapshot }
    }

    pub fn snapshot(&self) -> &SidebarSnapshot {
        &self.snapshot
    }

    pub fn update_snapshot(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut SidebarSnapshot),
    ) {
        f(&mut self.snapshot);
        cx.emit(SidebarStoreChanged);
        cx.notify();
    }
}

/// A thread command's target (`ScopedThreadRef`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ThreadRef {
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
}

impl ThreadRef {
    pub fn key(&self) -> String {
        crate::state::models::scoped_thread_key(&self.environment_id, &self.thread_id)
    }

    pub fn of(thread: &EnvironmentThreadShell) -> Self {
        Self {
            environment_id: thread.environment_id.clone(),
            thread_id: thread.id.clone(),
        }
    }
}

/// Commands the sidebar dispatches (`useThreadActions` and friends). Each
/// resolves to a [`CommandOutcome`]; state changes arrive through the store.
pub trait SidebarBackend {
    fn settle(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome>;
    fn unsettle(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome>;
    fn snooze(
        &self,
        thread: ThreadRef,
        snoozed_until: String,
        cx: &mut App,
    ) -> Task<CommandOutcome>;
    fn unsnooze(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome>;
    /// Fresh pins take `order_key` when given, else the top of the run.
    fn pin(
        &self,
        thread: ThreadRef,
        order_key: Option<String>,
        cx: &mut App,
    ) -> Task<CommandOutcome>;
    fn unpin(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome>;
    fn reorder_pinned(
        &self,
        thread: ThreadRef,
        order_key: String,
        cx: &mut App,
    ) -> Task<CommandOutcome>;
    fn reorder_active(
        &self,
        thread: ThreadRef,
        order_key: String,
        cx: &mut App,
    ) -> Task<CommandOutcome>;
    fn set_auto_settle(
        &self,
        thread: ThreadRef,
        enabled: bool,
        cx: &mut App,
    ) -> Task<CommandOutcome>;
    fn rename(&self, thread: ThreadRef, title: String, cx: &mut App) -> Task<CommandOutcome>;
    fn regenerate_title(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome>;
    fn mark_unread(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome>;
    /// Visiting a thread (and dismissing Woke) advances its visited watermark.
    fn mark_visited(
        &self,
        thread: ThreadRef,
        visited_at: String,
        cx: &mut App,
    ) -> Task<CommandOutcome>;
    fn archive(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome>;
    /// Undo of an archive.
    fn unarchive(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome>;
    fn delete(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome>;
    fn discard_draft(&self, draft_id: String, cx: &mut App);
    /// `reorderProjects`: drops the dragged project group in front of the
    /// target group (both by group key) and keeps the manual order. Backends
    /// without a saved order ignore it.
    #[allow(dead_code)]
    fn reorder_projects(&self, _dragged_group_key: &str, _target_group_key: &str, _cx: &mut App) {}
    /// The saved client settings changed (`useSettings()`); backends that
    /// build their own snapshots fold them in. The mock ignores them.
    fn apply_client_settings(
        &self,
        _settings: &turin_contracts::settings::ClientSettings,
        _cx: &mut App,
    ) {
    }
    fn set_project_scope(&self, project_key: Option<String>, cx: &mut App);
    fn set_shelf_expanded(&self, shelf: Shelf, expanded: bool, cx: &mut App);
    /// `useThreadSearch`: server message hits (two-character floor, debounced
    /// by the caller).
    fn search_messages(&self, query: String, cx: &mut App) -> Task<Vec<ThreadSearchMatch>>;
}

/// The collapsible shelves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shelf {
    Working,
    Snoozed,
    Settled,
}

pub type SharedSidebarBackend = Rc<dyn SidebarBackend>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::models::fixtures::project_id;

    #[test]
    fn refreshing_the_groups_assembles_them_from_the_projects() {
        let project = |id: &str, title: &str| EnvironmentProject {
            environment_id: crate::state::models::fixtures::env("local"),
            id: project_id(id),
            title: title.to_owned(),
            workspace_root: format!("/code/{id}"),
            favicon_path: None,
            project_icon: None,
            created_at: None,
            updated_at: None,
            is_scratch: false,
            source: None,
        };
        let mut snapshot = SidebarSnapshot {
            projects: vec![project("project-b", "B"), project("project-a", "A")],
            ..SidebarSnapshot::default()
        };
        snapshot.settings.project_sort_order = ProjectSortOrder::Manual;
        let order = vec![crate::logical_project::get_project_order_key(
            &snapshot.projects[1],
        )];
        snapshot.refresh_project_groups(&order, &ProjectGroupingSettings::default());
        let titles: Vec<&str> = snapshot
            .project_groups
            .iter()
            .map(|group| group.display_name.as_str())
            .collect();
        assert_eq!(titles, ["A", "B"]);
    }
}
