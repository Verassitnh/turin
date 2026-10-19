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

/// Where "new project" folders go until `newProjectsRoot` is a setting.
const NEW_PROJECTS_ROOT: &str = "~/Projects/t3";

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

/// The mock snapshot advertises every capability. Two palette commands have no live
/// implementation (see PORTING.md, "Palette: New thread without a project / Find in thread"), so
/// they are left out rather than listed as dead rows.
pub(crate) fn hide_unported_commands(mut snapshot: PaletteSnapshot) -> PaletteSnapshot {
    snapshot.scratch_thread_available = false;
    snapshot.supports_thread_find = false;
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
        hide_unported_commands(self.inner.snapshot(cx))
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

    /// `useNewProject`: a Git repository at `<root>/<name>`, then the project.
    fn create_new_project(&self, name: String, cx: &mut App) -> Task<Result<String, String>> {
        let folder = format!("{}/{name}", expand_home(NEW_PROJECTS_ROOT));
        let make_folder = {
            let folder = folder.clone();
            cx.background_spawn(async move {
                std::fs::create_dir_all(&folder)
                    .map_err(|error| format!("Cannot create {folder}: {error}"))?;
                let init = std::process::Command::new("git")
                    .args(["init", "-q"])
                    .current_dir(&folder)
                    .status()
                    .map_err(|error| format!("Cannot run git init: {error}"))?;
                init.success()
                    .then_some(())
                    .ok_or_else(|| format!("git init failed in {folder}"))
            })
        };
        let add = {
            let title = name.clone();
            let bus = self.bus.clone();
            move |cx: &mut App| {
                bus.task(cx, move |commands| async move {
                    commands.create_project(&title, &folder).await
                })
            }
        };
        cx.spawn(async move |cx| {
            make_folder.await?;
            let task = cx.update(add);
            task.await
                .map(|project_id| project_key(&project_id))
                .map_err(|error| error.message)
        })
    }

    /// Needs `ProviderSessionManager::close` behind a command; not wired.
    fn restart_agent_session(&self, _thread: ThreadRef, _cx: &mut App) -> Task<Result<(), String>> {
        Task::ready(Err(
            "Restarting the agent session is not wired yet.".to_owned()
        ))
    }
}
