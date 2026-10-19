//! The live [`PaletteBackend`]: project creation, folder browsing and message
//! search against the running app. The settings index, theme cards and the
//! palette snapshot come from [`MockPaletteBackend`], whose projects and
//! threads are the live sidebar store's, so the palette lists what the sidebar
//! shows.

use gpui_kit::{App, AppContext as _, Entity, Task};
use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::filesystem::{FilesystemBrowseEntry, FilesystemBrowseResult};

use super::mock_source::MockPaletteBackend;
use super::palette_source::{
    AddProjectOutcome, PaletteBackend, PaletteSnapshot, SettingsSearchItem,
};
use crate::app_runtime::write::bus::CommandBus;
use crate::components::sidebar::view_model::{
    SharedSidebarBackend, SidebarStore, ThreadRef, ThreadSearchMatch,
};
use crate::state::local_environment_id;

pub struct LivePaletteBackend {
    inner: MockPaletteBackend,
    store: Entity<SidebarStore>,
    bus: CommandBus,
}

impl LivePaletteBackend {
    pub fn new(
        store: Entity<SidebarStore>,
        sidebar_backend: SharedSidebarBackend,
        bus: CommandBus,
    ) -> Self {
        Self {
            inner: MockPaletteBackend::with_backend(store.clone(), sidebar_backend),
            store,
            bus,
        }
    }
}

/// The mock snapshot advertises every capability. "New thread without a project" needs the
/// environment's Scratch folder (`scratchWorkspaceRoot`), which is absent when the data dir is
/// inside a checkout, so it is listed only when the folder is known. "Find in current thread"
/// stays: the chat view mounts the find bar (`thread_find_provider`).
pub(crate) fn live_capabilities(
    mut snapshot: PaletteSnapshot,
    scratch_workspace_root: Option<&str>,
) -> PaletteSnapshot {
    snapshot.scratch_thread_available = scratch_workspace_root.is_some();
    snapshot
}

/// `serverConfig.newProjectsRoot`: shown in the New project step and gating its
/// row. Absent when there is no data folder to keep the projects in.
pub(crate) fn with_new_projects_root(
    mut snapshot: PaletteSnapshot,
    data_dir: Option<&std::path::Path>,
) -> PaletteSnapshot {
    snapshot.new_projects_root = data_dir.map(|dir| {
        super::named_project::new_projects_root(dir)
            .to_string_lossy()
            .into_owned()
    });
    snapshot
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".into())
}

fn expand_home(path: &str) -> String {
    match path.strip_prefix('~') {
        Some(rest) => format!("{}{rest}", home()),
        None => path.to_owned(),
    }
}

fn trimmed(value: &str) -> Result<TrimmedNonEmptyString, String> {
    TrimmedNonEmptyString::new(value).map_err(|error| error.to_string())
}

fn project_key(project_id: &impl std::fmt::Display) -> String {
    format!("{}:{project_id}", local_environment_id())
}

/// Subdirectories of `directory`, sorted by name; dot folders only when the
/// user is already typing one.
fn list_directories(directory: &str, show_hidden: bool) -> Result<FilesystemBrowseResult, String> {
    let root = if directory.is_empty() { "/" } else { directory };
    let mut names: Vec<String> = std::fs::read_dir(root)
        .map_err(|error| format!("Cannot read {root}: {error}"))?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| show_hidden || !name.starts_with('.'))
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    let base = root.trim_end_matches('/');
    Ok(FilesystemBrowseResult {
        parent_path: trimmed(root)?,
        entries: names
            .iter()
            .map(|name| {
                Ok(FilesystemBrowseEntry {
                    name: trimmed(name)?,
                    full_path: trimmed(&format!("{base}/{name}"))?,
                })
            })
            .collect::<Result<_, String>>()?,
    })
}

impl PaletteBackend for LivePaletteBackend {
    fn snapshot(&self, cx: &App) -> PaletteSnapshot {
        let scratch_root = cx
            .try_global::<crate::state::store::ClientStoreGlobal>()
            .and_then(|store| store.0.read(cx).state().scratch_workspace_root.clone());
        with_new_projects_root(
            live_capabilities(self.inner.snapshot(cx), scratch_root.as_deref()),
            crate::app_runtime::data_dir::resolve_data_dir().as_deref(),
        )
    }

    fn search_messages(&self, query: String, cx: &mut App) -> Task<Vec<ThreadSearchMatch>> {
        self.inner.search_messages(query, cx)
    }

    fn search_settings(&self, query: &str, cx: &App) -> Vec<SettingsSearchItem> {
        self.inner.search_settings(query, cx)
    }

    fn browse(
        &self,
        partial_path: String,
        cwd: Option<String>,
        cx: &mut App,
    ) -> Task<Result<FilesystemBrowseResult, String>> {
        let expanded = expand_home(partial_path.trim());
        let directory = match (expanded.starts_with('/'), cwd) {
            (false, Some(cwd)) => format!("{}/{expanded}", cwd.trim_end_matches('/')),
            _ => expanded,
        };
        let show_hidden = directory
            .rsplit('/')
            .next()
            .is_some_and(|last| last.starts_with('.'));
        let directory = directory.trim_end_matches('/').to_owned();
        cx.background_spawn(async move { list_directories(&directory, show_hidden) })
    }

    /// `handleAddProjectForEnvironment`: opens the project already rooted at
    /// `cwd`, else creates one titled from the folder name.
    fn add_project(&self, cwd: String, cx: &mut App) -> Task<Result<AddProjectOutcome, String>> {
        let cwd = expand_home(cwd.trim()).trim_end_matches('/').to_owned();
        let existing = self
            .store
            .read(cx)
            .snapshot()
            .projects
            .iter()
            .find(|project| project.workspace_root == cwd)
            .map(|project| project.key());
        if let Some(project_key) = existing {
            return Task::ready(Ok(AddProjectOutcome::OpenedExisting { project_key }));
        }
        let title = crate::state::projects::infer_project_title_from_path(&cwd);
        let task = self.bus.task(cx, move |commands| async move {
            commands.create_project_creating_folder(&title, &cwd).await
        });
        cx.spawn(async move |_| match task.await {
            Ok(project_id) => Ok(AddProjectOutcome::Created {
                project_key: project_key(&project_id),
            }),
            Err(error) => Err(error.message),
        })
    }

    /// `useNewProject` (`projects.createNew`): a new Git repository in
    /// `<baseDir>/projects/<slug>`, then the project. Returns its key.
    fn create_new_project(&self, name: String, cx: &mut App) -> Task<Result<String, String>> {
        let Some(data_dir) = crate::app_runtime::data_dir::resolve_data_dir() else {
            return Task::ready(Err(
                "Turin has no data folder to keep new projects in.".to_owned()
            ));
        };
        let base_dir = super::named_project::managed_base_dir(&data_dir);
        let task = self.bus.task(cx, move |commands| async move {
            super::named_project::create_named_project(
                commands,
                base_dir,
                super::named_project::system_git(),
                &name,
            )
            .await
            .map_err(crate::app_runtime::write::commands::CommandError::new)
        });
        cx.spawn(async move |_| match task.await {
            Ok(project) => Ok(project_key(&project.project_id)),
            Err(error) => Err(error.message),
        })
    }

    /// Needs `ProviderSessionManager::close` behind a command; not wired.
    fn restart_agent_session(&self, _thread: ThreadRef, _cx: &mut App) -> Task<Result<(), String>> {
        Task::ready(Err(
            "Restarting the agent session is not wired yet.".to_owned()
        ))
    }
}
