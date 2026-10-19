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
use crate::state::client_state::ClientState;
use crate::state::local_environment_id;
use crate::state::project_grouping::{ProjectGroupingSettings, select_project_grouping_settings};
use crate::state::store::ClientStore;
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
        snapshot.refresh_project_groups(&[], &ui.grouping);
    }
    snapshot
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
        let ui = Rc::new(RefCell::new(LocalUi::default()));
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
        let backend = Rc::new(LiveSidebarBackend { bus, ui, refresh });
        (store, backend)
    }
}

pub struct LiveSidebarBackend {
    bus: CommandBus,
    ui: Rc<RefCell<LocalUi>>,
    refresh: Rc<dyn Fn(&mut App)>,
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
        let mut saved = ClientSettings::default();
        saved.confirm_thread_delete = false;
        saved.confirm_thread_archive = true;
        saved.sidebar_working_shelf_enabled = true;
        saved.timestamp_format = turin_contracts::settings::TimestampFormat::TwentyFourHour;
        saved.sidebar_project_sort_order =
            turin_contracts::settings::SidebarProjectSortOrder::Manual;
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
}
