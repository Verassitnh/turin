//! In-memory [`PaletteBackend`] until T22 wires the live services. Not an
//! upstream module. Projects and threads come from the sidebar's mock store
//! (the same fixture the sidebar renders), so the palette lists what the
//! sidebar shows; the filesystem is a small fixed tree.

use gpui_kit::{App, Entity, Task};
use turin_contracts::filesystem::{FilesystemBrowseEntry, FilesystemBrowseResult};
use turin_contracts::{EnvironmentId, ProjectId, TrimmedNonEmptyString};

use super::palette_source::{
    AddProjectOutcome, PaletteBackend, PaletteSnapshot, SettingsSearchItem,
};
use super::theme_actions;
use crate::components::sidebar::mock_source::MockSidebarBackend;
use crate::components::sidebar::runtime::thread_sort::ThreadSortOrder;
use crate::components::sidebar::view_model::{
    SharedSidebarBackend, SidebarProjectGroup, SidebarStore, ThreadRef, ThreadSearchMatch,
};
use crate::state::models::EnvironmentProject;
use crate::utils::normalize_search_text;

pub struct MockPaletteBackend {
    store: Entity<SidebarStore>,
    sidebar_backend: SharedSidebarBackend,
}

impl MockPaletteBackend {
    pub fn new(
        store: Entity<SidebarStore>,
        sidebar_backend: std::rc::Rc<MockSidebarBackend>,
    ) -> Self {
        Self::with_backend(store, sidebar_backend)
    }

    /// Over any sidebar backend: the live app passes its own, so the
    /// palette's thread actions reach the orchestrator.
    pub fn with_backend(
        store: Entity<SidebarStore>,
        sidebar_backend: SharedSidebarBackend,
    ) -> Self {
        Self {
            store,
            sidebar_backend,
        }
    }
}

const HOME: &str = "/Users/dev";

/// `(directory, entries)` of the fixed tree, directories only.
const TREE: &[(&str, &[&str])] = &[
    (
        "/Users/dev",
        &[
            ".config",
            "Code",
            "Desktop",
            "Documents",
            "Downloads",
            "Library",
            "Projects",
        ],
    ),
    ("/Users/dev/Code", &["notes", "t3code", "turin"]),
    ("/Users/dev/Projects", &["scratch", "t3"]),
    ("/Users/dev/Documents", &["Taxes", "Travel"]),
];

fn expand_home(path: &str) -> String {
    match path.strip_prefix('~') {
        Some(rest) => format!("{HOME}{rest}"),
        None => path.to_owned(),
    }
}

fn trimmed(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).expect("non-blank")
}

const SETTINGS: &[(&str, &str, &str, bool)] = &[
    ("default-model", "Default model", "General", false),
    ("timestamp-format", "Time format", "General", false),
    (
        "confirm-thread-delete",
        "Confirm thread deletion",
        "General",
        false,
    ),
    ("theme", "Themes", "Appearance", false),
    ("font-size", "Interface font size", "Appearance", false),
    (
        "keybinding-modelPicker.toggle",
        "Model Picker: Toggle",
        "Keybindings",
        true,
    ),
    ("providers", "Providers", "Providers", false),
    ("source-control", "GitHub", "Source Control", false),
];

impl PaletteBackend for MockPaletteBackend {
    fn snapshot(&self, cx: &App) -> PaletteSnapshot {
        let sidebar = self.store.read(cx).snapshot().clone();
        PaletteSnapshot {
            sidebar,
            thread_sort_order: ThreadSortOrder::UpdatedAt,
            environment_label: "Local".into(),
            new_projects_root: Some("~/Projects/t3".into()),
            scratch_thread_available: true,
            supports_thread_find: true,
            pull_requests_available: true,
            link_pull_request_available: true,
            thread_pull_requests_available: true,
            reference_copy_target: None,
            add_project_base_directory: None,
            current_project_cwd: None,
            appearance_mode: theme_actions::current_appearance_mode(cx),
            active_theme_id: theme_actions::active_theme_id(cx),
            theme_cards: theme_actions::theme_cards(),
        }
    }

    fn search_messages(&self, query: String, cx: &mut App) -> Task<Vec<ThreadSearchMatch>> {
        self.sidebar_backend.search_messages(query, cx)
    }

    fn search_settings(&self, query: &str, _: &App) -> Vec<SettingsSearchItem> {
        let tokens = normalize_search_text(query);
        if tokens.is_empty() {
            return Vec::new();
        }
        SETTINGS
            .iter()
            .filter(|(_, title, section, _)| {
                let haystack = normalize_search_text(&format!("{title} {section}"));
                tokens.split(' ').all(|token| haystack.contains(token))
            })
            .map(|(id, title, section, secondary)| SettingsSearchItem {
                id: (*id).into(),
                title: (*title).into(),
                section_label: (*section).into(),
                search_terms: Vec::new(),
                secondary: *secondary,
            })
            .collect()
    }

    fn browse(
        &self,
        partial_path: String,
        _cwd: Option<String>,
        _: &mut App,
    ) -> Task<Result<FilesystemBrowseResult, String>> {
        let directory = expand_home(partial_path.trim_end_matches('/'));
        let directory = if directory.is_empty() {
            "/".to_owned()
        } else {
            directory
        };
        let entries = TREE
            .iter()
            .find(|(path, _)| *path == directory)
            .map(|(_, names)| *names)
            .unwrap_or(&[]);
        Task::ready(Ok(FilesystemBrowseResult {
            parent_path: trimmed(&directory),
            entries: entries
                .iter()
                .map(|name| FilesystemBrowseEntry {
                    name: trimmed(name),
                    full_path: trimmed(&format!("{directory}/{name}")),
                })
                .collect(),
        }))
    }

    fn add_project(&self, cwd: String, cx: &mut App) -> Task<Result<AddProjectOutcome, String>> {
        let existing = self
            .store
            .read(cx)
            .snapshot()
            .projects
            .iter()
            .find(|project| project.workspace_root == cwd)
            .map(EnvironmentProject::key);
        if let Some(project_key) = existing {
            return Task::ready(Ok(AddProjectOutcome::OpenedExisting { project_key }));
        }
        let title = crate::state::projects::infer_project_title_from_path(&cwd);
        let id = format!("added-{}", title.to_lowercase().replace(' ', "-"));
        let environment_id = EnvironmentId::new("local").expect("non-blank");
        let project = EnvironmentProject {
            environment_id,
            id: ProjectId::new(&id).expect("non-blank"),
            title: title.clone(),
            workspace_root: cwd,
            favicon_path: None,
            project_icon: None,
            created_at: None,
            updated_at: None,
            is_scratch: false,
            source: None,
        };
        let key = project.key();
        self.store.update(cx, |store, cx| {
            store.update_snapshot(cx, |snapshot| {
                snapshot.project_groups.push(SidebarProjectGroup {
                    project_key: key.clone(),
                    display_name: title,
                    created_at: None,
                    updated_at: None,
                    member_project_keys: vec![key.clone()],
                });
                snapshot.projects.push(project);
            })
        });
        Task::ready(Ok(AddProjectOutcome::Created { project_key: key }))
    }

    fn create_new_project(&self, name: String, cx: &mut App) -> Task<Result<String, String>> {
        let cwd = format!("{HOME}/Projects/t3/{name}");
        let task = self.add_project(cwd, cx);
        cx.spawn(async move |_| match task.await? {
            AddProjectOutcome::OpenedExisting { project_key }
            | AddProjectOutcome::Created { project_key } => Ok(project_key),
        })
    }

    fn restart_agent_session(&self, _: ThreadRef, _: &mut App) -> Task<Result<(), String>> {
        Task::ready(Ok(()))
    }
}
