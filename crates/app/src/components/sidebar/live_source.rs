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
    ProviderDriverKind, ProviderEntry, Shelf, SidebarBackend, SidebarEnvironment,
    SidebarProjectGroup, SidebarSettings, SidebarSnapshot, SidebarStore, ThreadCapabilities,
    ThreadRef, ThreadSearchMatch,
};
use crate::app_runtime::write::bus::CommandBus;
use crate::app_runtime::write::commands::{CommandError, Commands};
use crate::state::client_state::ClientState;
use crate::state::local_environment_id;
use crate::state::store::ClientStore;

/// State the sidebar keeps for itself.
#[derive(Default)]
struct LocalUi {
    working_shelf_expanded: bool,
    snoozed_shelf_expanded: bool,
    settled_shelf_expanded: bool,
    project_scope_key: Option<String>,
    local_last_visited_at: HashMap<String, String>,
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
        settings: SidebarSettings::default(),
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
        snapshot.project_groups = snapshot
            .projects
            .iter()
            .map(|project| SidebarProjectGroup {
                project_key: project.key(),
                display_name: project.title.clone(),
                created_at: project.created_at.clone(),
                updated_at: project.updated_at.clone(),
                member_project_keys: vec![project.key()],
            })
            .collect();
        snapshot.threads = view
            .navigation_threads
            .iter()
            .map(|thread| (**thread).clone())
            .collect();
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

    /// Title generation needs text generation wired into the app (N3); until
    /// then a regeneration request would stay pending forever.
    fn regenerate_title(&self, _thread: ThreadRef, _cx: &mut App) -> Task<CommandOutcome> {
        Task::ready(CommandOutcome::Failure(
            "Title regeneration needs text generation, which is not wired yet.".into(),
        ))
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

    /// Drafts are not persisted yet (new threads are created at once).
    fn discard_draft(&self, _draft_id: String, _cx: &mut App) {}

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

    /// Message search needs the orchestrator's `searchThread` (T13c open item).
    fn search_messages(&self, _query: String, _cx: &mut App) -> Task<Vec<ThreadSearchMatch>> {
        Task::ready(Vec::new())
    }
}
