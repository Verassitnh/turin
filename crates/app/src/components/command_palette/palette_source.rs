//! The palette's data seam: everything upstream's `CommandPalette` reads from
//! atoms and stores (`useProjects`, `useThreadShells`, `useHandleNewThread`,
//! `useThreadSearch`, `filesystemEnvironment.browse`, `projectEnvironment.create`,
//! `useTheme`, `useAvailableSettingsSearchItems`, …) gathered into one typed
//! snapshot, and every effect it runs behind [`PaletteBackend`].
//!
//! T14 (view models) and T22 (live services) implement this on the real
//! state; [`super::mock_source`] provides an in-memory one for now. Turin has
//! one environment (this machine), so the multi-environment machinery
//! (`useEnvironments`, WSL, connection phases) collapses into the snapshot's
//! single `environment_label`.

use std::rc::Rc;

use gpui_kit::{App, Task};
use turin_contracts::filesystem::FilesystemBrowseResult;
use turin_shared::thread_reference::ThreadReferenceKind;

use super::command_palette_logic::{AppearanceModeChoice, ThemePreviewSpec};
use crate::components::sidebar::runtime::thread_sort::ThreadSortOrder;
use crate::components::sidebar::view_model::{SidebarSnapshot, ThreadRef, ThreadSearchMatch};

/// A built-in or custom theme as the "Change theme" list shows it
/// (`ThemeCardDefinition`): `id: None` is the standard T3 artwork.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeCard {
    pub id: Option<String>,
    pub label: String,
    pub previews: Vec<ThemePreviewSpec>,
}

/// `SettingsSearchItem` (settings/settingsSearch.ts), narrowed to what the
/// palette reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsSearchItem {
    pub id: String,
    pub title: String,
    /// `SETTINGS_SECTION_LABELS[item.to]`.
    pub section_label: String,
    pub search_terms: Vec<String>,
    /// Sorts after every other match.
    pub secondary: bool,
}

/// `ThreadReferenceCopyTarget`, narrowed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferenceCopyTarget {
    pub kind: ThreadReferenceKind,
    pub value: String,
    pub success_title: String,
    pub failure_title: String,
}

/// Where the user is, as the palette's contextual items read it
/// (`useParams`, `useHandleNewThread`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaletteContext {
    /// Scoped key of the routed server thread.
    pub active_thread_key: Option<String>,
    /// `environmentId:projectId` of the routed draft thread.
    pub active_draft_project_key: Option<String>,
}

/// Everything the palette renders from, besides the sidebar's snapshot.
#[derive(Clone, Debug)]
pub struct PaletteSnapshot {
    /// Projects, groups, threads, providers and the client settings the
    /// sidebar shares (`sidebarThreadSortOrder`, …).
    pub sidebar: SidebarSnapshot,
    /// `clientSettings.sidebarThreadSortOrder`.
    pub thread_sort_order: ThreadSortOrder,
    /// Label of the local environment in project descriptions ("Local").
    pub environment_label: String,
    /// `serverConfig.newProjectsRoot`: the folder name-only projects go in.
    pub new_projects_root: Option<String>,
    /// `useScratchProject`: threads without a project are available.
    pub scratch_thread_available: bool,
    /// `serverConfig.threadFind`.
    pub supports_thread_find: bool,
    /// `capabilities.pullRequests`.
    pub pull_requests_available: bool,
    /// `threadPullRequestLinkMode !== "unsupported"`.
    pub link_pull_request_available: bool,
    /// `capabilities.threadPullRequests`.
    pub thread_pull_requests_available: bool,
    /// `resolveThreadReferenceCopyTarget` for the routed thread.
    pub reference_copy_target: Option<ReferenceCopyTarget>,
    /// `serverConfig.settings.addProjectBaseDirectory`.
    pub add_project_base_directory: Option<String>,
    /// The workspace root of the routed thread's project (relative-path base).
    pub current_project_cwd: Option<String>,
    /// `useTheme`.
    pub appearance_mode: AppearanceModeChoice,
    /// The theme id applied to the resolved light/dark half (`None` standard).
    pub active_theme_id: Option<String>,
    pub theme_cards: Vec<ThemeCard>,
}

/// The outcome of adding a project from a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddProjectOutcome {
    /// The path was already a project: open its latest thread (or a new one).
    OpenedExisting {
        project_key: String,
    },
    Created {
        project_key: String,
    },
}

/// Effects the palette runs. Each returns a task the palette awaits; state
/// changes arrive through the sidebar store the snapshot is built from.
pub trait PaletteBackend {
    fn snapshot(&self, cx: &App) -> PaletteSnapshot;

    /// `useThreadSearch`: server message hits (two-character floor, debounced
    /// by the caller).
    fn search_messages(&self, query: String, cx: &mut App) -> Task<Vec<ThreadSearchMatch>>;

    /// `searchSettings(query, availableSettingsSearchItems)`.
    fn search_settings(&self, query: &str, cx: &App) -> Vec<SettingsSearchItem>;

    /// `filesystemEnvironment.browse` (`partialPath`, optional `cwd` for
    /// relative paths).
    fn browse(
        &self,
        partial_path: String,
        cwd: Option<String>,
        cx: &mut App,
    ) -> Task<Result<FilesystemBrowseResult, String>>;

    /// `handleAddProjectForEnvironment`: find or create the project at `cwd`.
    fn add_project(&self, cwd: String, cx: &mut App) -> Task<Result<AddProjectOutcome, String>>;

    /// `useNewProject`: create `<newProjectsRoot>/<name>` as a Git repository.
    fn create_new_project(&self, name: String, cx: &mut App) -> Task<Result<String, String>>;

    /// "Restart agent session": stop the provider process and refresh the
    /// provider's workspace scan.
    fn restart_agent_session(&self, thread: ThreadRef, cx: &mut App) -> Task<Result<(), String>>;
}

pub type SharedPaletteBackend = Rc<dyn PaletteBackend>;
