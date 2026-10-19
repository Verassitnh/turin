//! The live [`SidebarStore`] and [`SidebarBackend`]: the sidebar renders the
//! client store's shell (projects and threads from Turin's own database) and
//! its thread commands dispatch orchestrator commands through the command bus.
//! Replaces `mock_source` in the running app; the mock stays for screenshots
//! (`TURIN_MOCK_UI=1`).
//!
//! UI-local state the server does not keep (shelf toggles, the project scope,
//! local visited watermarks) lives in [`LocalUi`] and is merged into every
//! snapshot.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::{App, AppContext as _, Entity, Task};
use serde_json::{Value, json};

use super::sidebar_logic::CommandOutcome;
use super::view_model::{
    ProviderDriverKind, ProviderEntry, Shelf, SidebarBackend, SidebarEnvironment, SidebarSettings,
    SidebarSnapshot, SidebarStore, ThreadCapabilities, ThreadRef, ThreadSearchMatch,
};
use crate::app_runtime::write::bus::CommandBus;
use crate::app_runtime::write::commands::{CommandError, Commands};
use crate::components::thread_search_match::SearchMatchSource;
use crate::hooks::use_local_storage::FileLocalStorage;
use crate::state::client_state::ClientState;
use crate::state::local_environment_id;
use crate::state::project_grouping::{
    ProjectGroupingSettings, get_project_order_key, select_project_grouping_settings,
};
use crate::state::store::ClientStore;
use crate::state::ui_state_file::UiStateFile;
use turin_contracts::settings::ClientSettings;
use turin_contracts::thread_search::OrchestrationThreadSearchSource;

/// State the sidebar keeps for itself.
#[derive(Default)]
struct LocalUi {
    working_shelf_expanded: bool,
    snoozed_shelf_expanded: bool,
    settled_shelf_expanded: bool,
    project_scope_key: Option<String>,
    local_last_visited_at: HashMap<String, String>,
    /// The saved client settings the sidebar reads (`useSettings()`).
    settings: SidebarSettings,
    grouping: ProjectGroupingSettings,
    /// The manual project order, loaded from and saved to the data directory
    /// (`useUiStateStore().projectOrder`).
    ui_state: Option<UiStateFile>,
}

impl LocalUi {
    fn project_order(&self) -> &[String] {
        self.ui_state
            .as_ref()
            .map_or(&[], UiStateFile::project_order)
    }
}

/// The Claude models the sidebar's hover cards name. The provider registry's
/// snapshots replace this when the catalog is bound to the UI.
fn provider_entries() -> Vec<ProviderEntry> {
    vec![ProviderEntry {
        instance_id: "claudeAgent".into(),
        display_name: "Claude".into(),
        driver_kind: ProviderDriverKind::Claude,
        models: vec![
            ("claude-opus-5-5".into(), "Opus 5.5".into()),
            ("claude-sonnet-5-5".into(), "Sonnet 5.5".into()),
            ("claude-sonnet-4-6".into(), "Sonnet 4.6".into()),
            ("claude-haiku-5-5".into(), "Haiku 5.5".into()),
        ],
    }]
}

fn snapshot_from(state: &ClientState, ui: &LocalUi) -> SidebarSnapshot {
    let environment_id = local_environment_id();
    let mut snapshot = SidebarSnapshot {
        environments: vec![SidebarEnvironment {
            id: environment_id.clone(),
            label: "This device".into(),
            capabilities: ThreadCapabilities::ALL,
        }],
        primary_environment_id: Some(environment_id),
        provider_entries: provider_entries(),
        settings: ui.settings.clone(),
        local_last_visited_at: ui.local_last_visited_at.clone(),
        project_scope_key: ui.project_scope_key.clone(),
        settled_shelf_expanded: ui.settled_shelf_expanded,
        snoozed_shelf_expanded: ui.snoozed_shelf_expanded,
        working_shelf_expanded: ui.working_shelf_expanded,
        ..Default::default()
    };
    if let Some(view) = &state.shell_view {
        snapshot.projects = view
            .projects
            .iter()
            .map(|project| (**project).clone())
            .collect();
        snapshot.threads = view
            .navigation_threads
            .iter()
            .map(|thread| (**thread).clone())
            .collect();
        // `useSidebarProjectGroups`: grouped per the saved grouping settings
        // and ordered per the saved project sort order.
        snapshot.refresh_project_groups(ui.project_order(), &ui.grouping);
    }
    snapshot
}

/// `handleProjectDragEnd`: the groups' physical project keys, in the order
/// shown, are the current order; the dragged group moves in front of the target.
#[allow(dead_code)] // see `SidebarBackend::reorder_projects`
fn reorder_project_groups(
    file: &mut UiStateFile,
    snapshot: &SidebarSnapshot,
    dragged_group_key: &str,
    target_group_key: &str,
) -> Result<bool, crate::hooks::use_local_storage::LocalStorageError> {
    let order_keys_of = |group_key: &str| -> Vec<String> {
        snapshot
            .project_groups
            .iter()
            .find(|group| group.project_key == group_key)
            .map(|group| {
                group
                    .member_project_keys
                    .iter()
                    .filter_map(|member| snapshot.project(member))
                    .map(get_project_order_key)
                    .collect()
            })
            .unwrap_or_default()
    };
    let dragged = order_keys_of(dragged_group_key);
    let target = order_keys_of(target_group_key);
    if dragged.is_empty() || target.is_empty() || dragged_group_key == target_group_key {
        return Ok(false);
    }
    let current: Vec<String> = snapshot
        .project_groups
        .iter()
        .flat_map(|group| order_keys_of(&group.project_key))
        .collect();
    file.reorder_projects(&current, &dragged, &target)
}

pub struct LiveSidebar;

impl LiveSidebar {
    /// Creates the store, keeps it current from `client`, and returns the
    /// backend the sidebar drives.
    pub fn start(
        client: Entity<ClientStore>,
        bus: CommandBus,
        cx: &mut App,
    ) -> (Entity<SidebarStore>, Rc<LiveSidebarBackend>) {
        let ui = Rc::new(RefCell::new(LocalUi {
            ui_state: Some(UiStateFile::load(FileLocalStorage::shared_in_data_dir(
                "local-storage.json",
            ))),
            ..LocalUi::default()
        }));
        let initial = snapshot_from(client.read(cx).state(), &ui.borrow());
        let store = cx.new(|_| SidebarStore::new(initial));
        let refresh = {
            let store = store.downgrade();
            let client = client.clone();
            let ui = ui.clone();
            Rc::new(move |cx: &mut App| {
                let snapshot = snapshot_from(client.read(cx).state(), &ui.borrow());
                store
                    .update(cx, |store, cx| {
                        if *store.snapshot() != snapshot {
                            store.update_snapshot(cx, |current| *current = snapshot);
                        }
                    })
                    .ok();
            })
        };
        let observed = refresh.clone();
        cx.observe(&client, move |_, cx| observed(cx)).detach();
        let backend = Rc::new(LiveSidebarBackend {
            bus,
            ui,
            refresh,
            store: store.downgrade(),
        });
        (store, backend)
    }
}

pub struct LiveSidebarBackend {
    bus: CommandBus,
    ui: Rc<RefCell<LocalUi>>,
    refresh: Rc<dyn Fn(&mut App)>,
    #[allow(dead_code)] // read by `reorder_projects`, which has no caller yet
    store: gpui_kit::WeakEntity<SidebarStore>,
}

impl LiveSidebarBackend {
    /// Runs one thread command on the runtime and reports its outcome. State
    /// changes arrive through the client store, not here.
    fn thread_command(
        &self,
        thread: &ThreadRef,
        kind: &'static str,
        extra: Value,
        cx: &mut App,
    ) -> Task<CommandOutcome> {
        let thread_id = thread.thread_id.clone();
        self.run(cx, move |commands: Commands| async move {
            commands.thread_command(kind, &thread_id, extra).await
        })
    }

    fn run<Fut>(
        &self,
        cx: &mut App,
        command: impl FnOnce(Commands) -> Fut + Send + 'static,
    ) -> Task<CommandOutcome>
    where
        Fut: std::future::Future<Output = Result<(), CommandError>> + Send + 'static,
    {
        let task = self.bus.task(cx, command);
        cx.spawn(async move |_| match task.await {
            Ok(()) => CommandOutcome::Success,
            Err(error) => CommandOutcome::Failure(error.message),
        })
    }

    fn local(&self, cx: &mut App, edit: impl FnOnce(&mut LocalUi)) {
        edit(&mut self.ui.borrow_mut());
        (self.refresh)(cx);
    }
}

impl SidebarBackend for LiveSidebarBackend {
    fn apply_client_settings(&self, settings: &ClientSettings, cx: &mut App) {
        let sidebar = SidebarSettings::from_client(settings);
        let grouping = select_project_grouping_settings(settings);
        self.local(cx, |ui| {
            ui.settings = sidebar;
            ui.grouping = grouping;
        });
    }

    fn settle(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome> {
        self.thread_command(&thread, "thread.settle", json!({}), cx)
    }

    fn unsettle(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome> {
        self.thread_command(&thread, "thread.unsettle", json!({"reason": "user"}), cx)
    }

    fn snooze(
        &self,
        thread: ThreadRef,
        snoozed_until: String,
        cx: &mut App,
    ) -> Task<CommandOutcome> {
        self.thread_command(
            &thread,
            "thread.snooze",
            json!({"snoozedUntil": snoozed_until}),
            cx,
        )
    }

    fn unsnooze(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome> {
        self.thread_command(&thread, "thread.unsnooze", json!({"reason": "user"}), cx)
    }

    fn pin(
        &self,
        thread: ThreadRef,
        order_key: Option<String>,
        cx: &mut App,
    ) -> Task<CommandOutcome> {
        let extra = match order_key {
            Some(key) => json!({"orderKey": key}),
            None => json!({}),
        };
        self.thread_command(&thread, "thread.pin", extra, cx)
    }

    fn unpin(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome> {
        self.thread_command(&thread, "thread.unpin", json!({}), cx)
    }

    fn reorder_pinned(
        &self,
        thread: ThreadRef,
        order_key: String,
        cx: &mut App,
    ) -> Task<CommandOutcome> {
        self.thread_command(
            &thread,
            "thread.pin.reorder",
            json!({"orderKey": order_key}),
            cx,
        )
    }

    fn reorder_active(
        &self,
        thread: ThreadRef,
        order_key: String,
        cx: &mut App,
    ) -> Task<CommandOutcome> {
        self.thread_command(
            &thread,
            "thread.active.reorder",
            json!({"orderKey": order_key}),
            cx,
        )
    }

    fn set_auto_settle(
        &self,
        thread: ThreadRef,
        enabled: bool,
        cx: &mut App,
    ) -> Task<CommandOutcome> {
        self.thread_command(
            &thread,
            "thread.auto-settle.set",
            json!({"enabled": enabled}),
            cx,
        )
    }

    fn rename(&self, thread: ThreadRef, title: String, cx: &mut App) -> Task<CommandOutcome> {
        self.thread_command(
            &thread,
            "thread.metadata.update",
            json!({"title": title}),
            cx,
        )
    }

    /// `thread.metadata.update` with `regenerateTitle`: the title effect runs
    /// N3's text generation and clears the marker when it finishes.
    fn regenerate_title(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome> {
        self.thread_command(
            &thread,
            "thread.metadata.update",
            json!({"regenerateTitle": true}),
            cx,
        )
    }

    fn mark_unread(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome> {
        self.thread_command(&thread, "thread.mark-unread", json!({}), cx)
    }

    fn mark_visited(
        &self,
        thread: ThreadRef,
        visited_at: String,
        cx: &mut App,
    ) -> Task<CommandOutcome> {
        self.thread_command(
            &thread,
            "thread.visit",
            json!({"visitedAt": visited_at}),
            cx,
        )
    }

    fn archive(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome> {
        self.thread_command(&thread, "thread.archive", json!({}), cx)
    }

    fn unarchive(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome> {
        self.thread_command(&thread, "thread.unarchive", json!({}), cx)
    }

    fn delete(&self, thread: ThreadRef, cx: &mut App) -> Task<CommandOutcome> {
        self.thread_command(&thread, "thread.delete", json!({}), cx)
    }

    fn discard_draft(&self, draft_id: String, cx: &mut App) {
        let Some(global) =
            cx.try_global::<crate::shell::composer_draft_host::ComposerDraftHostGlobal>()
        else {
            return;
        };
        let host = global.0.clone();
        host.update(cx, |host, cx| {
            host.discard_draft_id(&draft_id);
            cx.notify();
        });
    }

    fn reorder_projects(&self, dragged_group_key: &str, target_group_key: &str, cx: &mut App) {
        let Some(snapshot) = self
            .store
            .upgrade()
            .map(|store| store.read(cx).snapshot().clone())
        else {
            return;
        };
        let outcome = {
            let mut ui = self.ui.borrow_mut();
            let Some(file) = ui.ui_state.as_mut() else {
                return;
            };
            reorder_project_groups(file, &snapshot, dragged_group_key, target_group_key)
        };
        match outcome {
            Ok(true) => (self.refresh)(cx),
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, "could not save the project order"),
        }
    }

    fn set_project_scope(&self, project_key: Option<String>, cx: &mut App) {
        self.local(cx, |ui| ui.project_scope_key = project_key);
    }

    fn set_shelf_expanded(&self, shelf: Shelf, expanded: bool, cx: &mut App) {
        self.local(cx, |ui| match shelf {
            Shelf::Working => ui.working_shelf_expanded = expanded,
            Shelf::Snoozed => ui.snoozed_shelf_expanded = expanded,
            Shelf::Settled => ui.settled_shelf_expanded = expanded,
        });
    }

    /// Cross-thread message search (`useThreadSearch`): two-character floor,
    /// the caller debounces.
    fn search_messages(&self, query: String, cx: &mut App) -> Task<Vec<ThreadSearchMatch>> {
        let query = query.trim().to_owned();
        if query.chars().count() < 2 {
            return Task::ready(Vec::new());
        }
        let task = self.bus.task(cx, move |commands| async move {
            commands.search_messages(&query, 50).await
        });
        cx.spawn(async move |_| {
            task.await
                .unwrap_or_default()
                .into_iter()
                .map(|found| ThreadSearchMatch {
                    environment_id: local_environment_id(),
                    thread_id: found.thread_id,
                    source: match found.source {
                        OrchestrationThreadSearchSource::User => SearchMatchSource::User,
                        OrchestrationThreadSearchSource::Assistant => SearchMatchSource::Assistant,
                    },
                    snippet: found.snippet,
                })
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::sidebar::sidebar_logic::ProjectSortOrder;
    use crate::state::client_state::ClientStateCore;

    #[test]
    fn the_snapshot_carries_the_saved_client_settings() {
        let state = ClientStateCore::new(local_environment_id()).snapshot();
        let saved = ClientSettings {
            confirm_thread_delete: false,
            confirm_thread_archive: true,
            sidebar_working_shelf_enabled: true,
            timestamp_format: turin_contracts::settings::TimestampFormat::TwentyFourHour,
            sidebar_project_sort_order: turin_contracts::settings::SidebarProjectSortOrder::Manual,
            ..ClientSettings::default()
        };
        let ui = LocalUi {
            settings: SidebarSettings::from_client(&saved),
            grouping: select_project_grouping_settings(&saved),
            ..LocalUi::default()
        };
        let snapshot = snapshot_from(&state, &ui);
        assert!(!snapshot.settings.confirm_thread_delete);
        assert!(snapshot.settings.confirm_thread_archive);
        assert!(snapshot.settings.working_shelf_enabled);
        assert_eq!(
            snapshot.settings.timestamp_format,
            crate::timestamp_format::TimestampFormat::TwentyFourHour
        );
        assert_eq!(
            snapshot.settings.project_sort_order,
            ProjectSortOrder::Manual
        );
    }

    fn project(id: &str, root: &str, title: &str) -> crate::state::models::EnvironmentProject {
        crate::state::models::EnvironmentProject {
            environment_id: local_environment_id(),
            id: turin_contracts::ProjectId::new(id).unwrap(),
            title: title.to_owned(),
            workspace_root: root.to_owned(),
            favicon_path: None,
            project_icon: None,
            created_at: None,
            updated_at: None,
            is_scratch: false,
            source: None,
        }
    }

    fn manual_snapshot(order: &[String]) -> SidebarSnapshot {
        let mut snapshot = SidebarSnapshot {
            projects: vec![
                project("project-a", "/code/a", "A"),
                project("project-b", "/code/b", "B"),
                project("project-c", "/code/c", "C"),
            ],
            ..SidebarSnapshot::default()
        };
        snapshot.settings.project_sort_order = ProjectSortOrder::Manual;
        snapshot.refresh_project_groups(order, &ProjectGroupingSettings::default());
        snapshot
    }

    fn titles(snapshot: &SidebarSnapshot) -> Vec<&str> {
        snapshot
            .project_groups
            .iter()
            .map(|group| group.display_name.as_str())
            .collect()
    }

    #[test]
    fn the_saved_manual_order_reaches_the_snapshot_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local-storage.json");
        let open = || {
            UiStateFile::load(std::sync::Arc::new(
                crate::hooks::use_local_storage::FileLocalStorage::open(&path),
            ))
        };

        // Drag C in front of A.
        let mut first = open();
        let before = manual_snapshot(first.project_order());
        let key_of = |title: &str| {
            before
                .project_groups
                .iter()
                .find(|group| group.display_name == title)
                .unwrap()
                .project_key
                .clone()
        };
        assert!(reorder_project_groups(&mut first, &before, &key_of("C"), &key_of("A")).unwrap());

        // A new process reads the file and orders the sidebar by it.
        let second = open();
        let state = ClientStateCore::new(local_environment_id()).snapshot();
        let ui = LocalUi {
            ui_state: Some(second),
            ..LocalUi::default()
        };
        assert!(snapshot_from(&state, &ui).project_groups.is_empty());
        let reordered = manual_snapshot(ui.project_order());
        assert_eq!(titles(&reordered), ["C", "A", "B"]);
    }

    #[test]
    fn dropping_a_group_on_itself_or_an_unknown_group_changes_nothing() {
        let mut file =
            UiStateFile::load(crate::hooks::use_local_storage::MemoryLocalStorage::shared());
        let snapshot = manual_snapshot(&[]);
        let a = snapshot.project_groups[0].project_key.clone();
        assert!(!reorder_project_groups(&mut file, &snapshot, &a, &a).unwrap());
        assert!(!reorder_project_groups(&mut file, &snapshot, &a, "nope").unwrap());
        assert!(file.project_order().is_empty());
    }
}
