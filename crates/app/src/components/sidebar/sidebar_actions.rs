//! Lifecycle commands, context menus and inline rename from upstream
//! Sidebar.tsx (`attemptSettle`, `planForwardNavigation`, `performSnooze`,
//! `handleThreadContextMenu`, `handleMultiSelectContextMenu`,
//! `handleDraftContextMenu`, `commitThreadRename`, …).
//!
//! Upstream shows the desktop's native context menu (`api.contextMenu.show`)
//! from the same `ContextMenuItem` lists; here they become gpui-component
//! `PopupMenu`s. Toasts are gpui-component notifications; confirms are
//! alert dialogs.

use std::collections::HashSet;
use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::input::InputState;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, IntoElement as _, Task, WeakEntity, Window,
};

use super::runtime::models::{EnvironmentThreadShell, SettledOverride, thread_runtime_can_archive};
use super::runtime::thread_settled::can_snooze;
use super::sidebar_logic::{
    CommandOutcome, ContextMenuItem, ParkAction, build_bulk_title_regeneration_context_menu_item,
    build_bulk_unpin_context_menu_item, delete_selected_thread_entries,
    should_navigate_after_thread_park,
};
use super::sidebar_snooze::resolve_snooze_presets_now;
use super::thread_selection::get_thread_keys_to_deselect_after_delete;
use super::view_model::{Shelf, ThreadRef};
use super::{RenameState, Sidebar, SidebarEvent, SidebarRoute};
use crate::components::thread_action_menu_logic::{
    ProjectFilterState, ThreadActionMenuState, ThreadActionSupports, build_draft_action_menu_items,
    build_thread_action_menu_items, thread_action_requires_operate,
};
use crate::timestamp_format::now_iso;

/// Where parking the open thread moves the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ForwardNavigation {
    Thread(String),
    NewThread(String),
    Home,
}

type MenuSelect = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// Build a gpui-component popup menu from upstream's `ContextMenuItem`s.
pub(crate) fn build_popup_menu(
    mut menu: PopupMenu,
    items: &[ContextMenuItem],
    on_select: MenuSelect,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    for item in items {
        if item.separator_before {
            menu = menu.separator();
        }
        if !item.children.is_empty() {
            let children = item.children.clone();
            let select = on_select.clone();
            let disabled = item.disabled;
            menu = menu.submenu(item.label.clone(), window, cx, move |menu, window, cx| {
                let children: Vec<ContextMenuItem> = children
                    .iter()
                    .cloned()
                    .map(|child| {
                        if disabled {
                            child.disabled(true)
                        } else {
                            child
                        }
                    })
                    .collect();
                build_popup_menu(menu, &children, select.clone(), window, cx)
            });
            continue;
        }
        let id = item.id.clone();
        let select = on_select.clone();
        let mut entry = PopupMenuItem::new(item.label.clone())
            .disabled(item.disabled)
            .on_click(move |_, window, cx| select(&id, window, cx));
        if let Some(checked) = item.checked {
            entry = entry.checked(checked);
        }
        menu = menu.item(entry);
    }
    menu
}

impl Sidebar {
    pub(crate) fn toast_error(&self, title: &str, error: &str, window: &mut Window, cx: &mut App) {
        window.push_notification(
            Notification::error(error.to_string()).title(title.to_string()),
            cx,
        );
    }

    fn toast_success(&self, title: &str, description: &str, window: &mut Window, cx: &mut App) {
        window.push_notification(
            Notification::success(description.to_string()).title(title.to_string()),
            cx,
        );
    }

    /// Run a backend command and toast its failure (`squashAtomCommandFailure`).
    pub(crate) fn run_command(
        &mut self,
        task: Task<CommandOutcome>,
        failure_title: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, bool, &mut Window, &mut Context<Self>) + 'static,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let outcome = task.await;
            this.update_in(cx, |this, window, cx| {
                if let CommandOutcome::Failure(error) = &outcome {
                    this.toast_error(failure_title, error, window, cx);
                }
                then(this, outcome == CommandOutcome::Success, window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// `planForwardNavigation`: parking the open thread moves to the next
    /// remaining card (never a settled/snoozed row, never one leaving in the
    /// same batch), else a fresh draft in its project, else home. `None`
    /// for background parks.
    pub(crate) fn plan_forward_navigation(
        &mut self,
        key: &str,
        co_parking: Option<&HashSet<String>>,
        cx: &App,
    ) -> Option<ForwardNavigation> {
        if self.route_thread_key() != Some(key) {
            return None;
        }
        let (partition, layout) = self.compute_sections(cx);
        let settled: HashSet<String> = partition
            .settled
            .iter()
            .map(EnvironmentThreadShell::key)
            .collect();
        let snoozed: HashSet<String> = partition
            .snoozed
            .iter()
            .map(EnvironmentThreadShell::key)
            .collect();
        let ordered = &layout.ordered_keys;
        let next = ordered
            .iter()
            .position(|candidate| candidate == key)
            .and_then(|index| {
                ordered[index + 1..]
                    .iter()
                    .chain(&ordered[..index])
                    .find(|candidate| {
                        !settled.contains(*candidate)
                            && !snoozed.contains(*candidate)
                            && co_parking.is_none_or(|keys| !keys.contains(*candidate))
                    })
                    .cloned()
            });
        Some(match next {
            Some(next) => ForwardNavigation::Thread(next),
            None => match self.thread(key, cx) {
                Some(thread) => ForwardNavigation::NewThread(thread.project_key()),
                None => ForwardNavigation::Home,
            },
        })
    }

    fn apply_forward_navigation(&mut self, plan: ForwardNavigation, cx: &mut Context<Self>) {
        match plan {
            ForwardNavigation::Thread(key) => self.navigate_to_thread(&key, cx),
            ForwardNavigation::NewThread(project_key) => {
                cx.emit(SidebarEvent::NewThread {
                    project_key: Some(project_key),
                    branch: None,
                    worktree_path: None,
                });
            }
            ForwardNavigation::Home => self.navigate_home(cx),
        }
    }

    fn navigate_after_park(
        &mut self,
        key: &str,
        action: ParkAction,
        plan: Option<ForwardNavigation>,
        cx: &mut Context<Self>,
    ) {
        let thread = self.thread(key, cx);
        if should_navigate_after_thread_park(
            key,
            self.route_thread_key(),
            action,
            &now_iso(),
            thread.as_ref(),
        ) && let Some(plan) = plan
        {
            self.apply_forward_navigation(plan, cx);
        }
    }

    /// `attemptSettle`: one settle per thread at a time; never navigate
    /// away from a thread that did not settle.
    pub(crate) fn attempt_settle(
        &mut self,
        thread: ThreadRef,
        co_settling: Option<HashSet<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = thread.key();
        if !self.settling.insert(key.clone()) {
            return;
        }
        let plan = self.plan_forward_navigation(&key, co_settling.as_ref(), cx);
        let task = self.backend.settle(thread, cx);
        self.run_command(
            task,
            "Failed to settle thread",
            window,
            cx,
            move |this, ok, _, cx| {
                this.settling.remove(&key);
                if ok {
                    this.navigate_after_park(&key, ParkAction::Settle, plan, cx);
                }
            },
        );
    }

    /// `settleThreads`: rows already settled are skipped; pinned rows park too.
    pub(crate) fn settle_threads(
        &mut self,
        keys: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let co: HashSet<String> = keys.iter().cloned().collect();
        for key in keys {
            let Some(thread) = self.thread(key, cx) else {
                continue;
            };
            if thread.settled_override == Some(SettledOverride::Settled) {
                continue;
            }
            self.attempt_settle(ThreadRef::of(&thread), Some(co.clone()), window, cx);
        }
    }

    pub(crate) fn attempt_unsettle(
        &mut self,
        thread: ThreadRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self.backend.unsettle(thread, cx);
        self.run_command(
            task,
            "Failed to un-settle thread",
            window,
            cx,
            |_, _, _, _| {},
        );
    }

    pub(crate) fn attempt_unsnooze(
        &mut self,
        thread: ThreadRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self.backend.unsnooze(thread, cx);
        self.run_command(task, "Failed to wake thread", window, cx, |_, _, _, _| {});
    }

    /// `performSnooze` / `attemptSnooze`: parking the open thread moves forward.
    pub(crate) fn attempt_snooze(
        &mut self,
        thread: ThreadRef,
        snoozed_until: String,
        co_snoozing: Option<HashSet<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = thread.key();
        if !self.snoozing.insert(key.clone()) {
            return;
        }
        let plan = self.plan_forward_navigation(&key, co_snoozing.as_ref(), cx);
        let task = self.backend.snooze(thread, snoozed_until, cx);
        self.run_command(
            task,
            "Failed to snooze thread",
            window,
            cx,
            move |this, ok, _, cx| {
                this.snoozing.remove(&key);
                if ok {
                    this.navigate_after_park(&key, ParkAction::Snooze, plan, cx);
                }
            },
        );
    }

    pub(crate) fn attempt_pin(
        &mut self,
        thread: ThreadRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Fresh pins take the top of the arranged run (the backend computes
        // a key before the smallest key across all pins, hidden ones too).
        let task = self.backend.pin(thread, None, cx);
        self.run_command(task, "Failed to pin thread", window, cx, |_, _, _, _| {});
    }

    pub(crate) fn attempt_unpin(
        &mut self,
        thread: ThreadRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self.backend.unpin(thread, cx);
        self.run_command(task, "Failed to unpin thread", window, cx, |_, _, _, _| {});
    }

    /// `useAcknowledgeThreadWoke`: a visit at the wake time clears the pill.
    pub(crate) fn acknowledge_woke(
        &mut self,
        thread: ThreadRef,
        woke_at: String,
        cx: &mut Context<Self>,
    ) {
        let at = now_iso().max(woke_at);
        self.backend.mark_visited(thread, at, cx).detach();
    }

    pub(crate) fn toggle_shelf(&mut self, shelf: Shelf, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(cx);
        let expanded = match shelf {
            Shelf::Working => snapshot.working_shelf_expanded,
            Shelf::Snoozed => snapshot.snoozed_shelf_expanded,
            Shelf::Settled => snapshot.settled_shelf_expanded,
        };
        self.backend.set_shelf_expanded(shelf, !expanded, cx);
    }

    pub(crate) fn set_project_scope(&mut self, key: Option<String>, cx: &mut Context<Self>) {
        self.backend.set_project_scope(key, cx);
    }

    /// `requestCustomSnooze` then snooze every thread to the chosen time.
    pub(crate) fn open_custom_snooze(
        &mut self,
        threads: Vec<ThreadRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let weak = cx.entity().downgrade();
        super::custom_snooze_dialog::request_custom_snooze(window, cx, move |until, window, cx| {
            let threads = threads.clone();
            weak.update(cx, |this, cx| {
                let co: HashSet<String> = threads.iter().map(ThreadRef::key).collect();
                for thread in threads {
                    this.attempt_snooze(thread, until.clone(), Some(co.clone()), window, cx);
                }
            })
            .ok();
        });
    }

    // ── Rename ─────────────────────────────────────────────────────────

    /// Double-click or the menu: an inline input over the title.
    pub(crate) fn start_rename(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(key, cx) else {
            return;
        };
        let title = thread.title.clone();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title.clone()));
        input.update(cx, |state, cx| {
            state.focus(window, cx);
            state.select_all(window, cx);
        });
        cx.subscribe_in(
            &input,
            window,
            |this, _, event: &gpui_kit::component::input::InputEvent, window, cx| {
                use gpui_kit::component::input::InputEvent;
                match event {
                    InputEvent::PressEnter { .. } => this.commit_rename(window, cx),
                    InputEvent::Blur => {
                        if this.rename.as_ref().is_some_and(|rename| !rename.committed) {
                            this.commit_rename(window, cx);
                        }
                    }
                    _ => {}
                }
            },
        )
        .detach();
        self.rename = Some(RenameState {
            key: key.to_string(),
            input,
            original: title,
            committed: false,
        });
        cx.notify();
    }

    pub(crate) fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if let Some(rename) = &mut self.rename {
            rename.committed = true;
        }
        self.rename = None;
        cx.notify();
    }

    /// `commitThreadRename`: trimmed; empty warns; unchanged is a no-op.
    pub(crate) fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut rename) = self.rename.take() else {
            return;
        };
        rename.committed = true;
        let title = rename.input.read(cx).value().trim().to_string();
        cx.notify();
        if title.is_empty() {
            window.push_notification(Notification::warning("Thread title cannot be empty"), cx);
            return;
        }
        if title == rename.original {
            return;
        }
        let Some(thread) = self.thread(&rename.key, cx) else {
            return;
        };
        let task = self.backend.rename(ThreadRef::of(&thread), title, cx);
        self.run_command(task, "Failed to rename thread", window, cx, |_, _, _, _| {});
    }

    // ── Context menus ──────────────────────────────────────────────────

    /// The right-click menu for a row: the bulk menu when the row is part
    /// of a multi-selection, else `buildThreadActionMenuItems`.
    pub(crate) fn thread_menu_items(&mut self, key: &str, cx: &App) -> Vec<ContextMenuItem> {
        if self.selection.has_selection() && self.selection.contains(key) {
            return self.multi_select_menu_items(cx);
        }
        let Some(thread) = self.thread(key, cx) else {
            return Vec::new();
        };
        let snapshot = self.snapshot(cx);
        let capabilities = snapshot.capabilities(&thread.environment_id);
        let supports = capabilities.map_or(
            ThreadActionSupports {
                settlement: false,
                auto_settle_opt_out: false,
                snooze: false,
                pinning: false,
                title_regeneration: false,
            },
            |c| ThreadActionSupports {
                settlement: c.thread_settlement,
                auto_settle_opt_out: c.thread_auto_settle_opt_out,
                snooze: c.thread_snooze,
                pinning: c.thread_pinning,
                title_regeneration: c.thread_title_regeneration,
            },
        );
        let group = snapshot.project_group_for(&thread.project_key()).cloned();
        let scope_key = snapshot.project_scope_key.clone();
        let format = snapshot.settings.timestamp_format;
        let (partition, _) = self.compute_sections(cx);
        build_thread_action_menu_items(&ThreadActionMenuState {
            can_operate: true,
            branch: thread.branch.clone(),
            project_filter: group.map(|group| ProjectFilterState {
                is_active: scope_key.as_deref() == Some(group.project_key.as_str()),
                label: group.display_name,
            }),
            is_pinned: thread.pinned_at.is_some(),
            is_settled: partition.settled.iter().any(|t| t.key() == key),
            auto_settle_enabled: thread.auto_settle_disabled_at.is_none(),
            is_snoozed: partition.snoozed.iter().any(|t| t.key() == key),
            can_snooze_now: can_snooze(&thread, &now_iso()),
            is_regenerating_title: thread.title_regeneration.is_some(),
            is_running: !thread_runtime_can_archive(thread.runtime.as_ref()),
            supports,
            snooze_presets: resolve_snooze_presets_now(format),
        })
    }

    /// `handleMultiSelectContextMenu` items over the rendered selection.
    fn multi_select_menu_items(&mut self, cx: &App) -> Vec<ContextMenuItem> {
        let selected = self.selected_threads(cx);
        let count = selected.len();
        let snapshot = self.snapshot(cx);
        let caps = |thread: &EnvironmentThreadShell| snapshot.capabilities(&thread.environment_id);
        let settling = selected
            .iter()
            .filter(|thread| thread.settled_override != Some(SettledOverride::Settled))
            .count();
        let now = now_iso();
        let can_snooze_selection = selected.iter().all(|thread| {
            caps(thread).is_some_and(|c| c.thread_snooze) && can_snooze(thread, &now)
        });
        let regen_supported: Vec<&EnvironmentThreadShell> = selected
            .iter()
            .filter(|thread| caps(thread).is_some_and(|c| c.thread_title_regeneration))
            .collect();
        let regen_actionable = regen_supported
            .iter()
            .filter(|thread| thread.title_regeneration.is_none())
            .count();
        let pinned = selected
            .iter()
            .filter(|thread| {
                caps(thread).is_some_and(|c| c.thread_pinning) && thread.pinned_at.is_some()
            })
            .count();
        let mut items = Vec::new();
        items.extend(build_bulk_unpin_context_menu_item(pinned));
        items.push(
            ContextMenuItem::new("settle", format!("Settle ({count})")).disabled(settling == 0),
        );
        if can_snooze_selection {
            let mut children: Vec<ContextMenuItem> =
                resolve_snooze_presets_now(snapshot.settings.timestamp_format)
                    .into_iter()
                    .map(|preset| {
                        ContextMenuItem::new(
                            format!("snooze:{}", preset.id.as_str()),
                            format!("{} ({})", preset.label, preset.when_label),
                        )
                    })
                    .collect();
            children.push(ContextMenuItem::new("snooze:custom", "Custom…").separator_before());
            items.push(
                ContextMenuItem::new("snooze", format!("Snooze ({count})")).children(children),
            );
        }
        items.extend(build_bulk_title_regeneration_context_menu_item(
            regen_supported.len(),
            regen_actionable,
        ));
        items.push(ContextMenuItem::new(
            "mark-unread",
            format!("Mark unread ({count})"),
        ));
        items.push(ContextMenuItem::new("delete", format!("Delete ({count})")).destructive());
        items
    }

    /// Selected keys whose rows are rendered right now.
    fn selected_threads(&mut self, cx: &App) -> Vec<EnvironmentThreadShell> {
        let (_, layout) = self.compute_sections(cx);
        self.selection
            .selected()
            .iter()
            .filter(|key| layout.ordered_keys.contains(key))
            .filter_map(|key| self.thread(key, cx))
            .collect()
    }

    /// Builder for a row's `context_menu`.
    pub(crate) fn thread_context_menu(
        this: WeakEntity<Self>,
        key: String,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        move |menu, window, cx| {
            let Some(sidebar) = this.upgrade() else {
                return menu;
            };
            let items = sidebar.update(cx, |sidebar, cx| sidebar.thread_menu_items(&key, cx));
            let weak = this.clone();
            let menu_key = key.clone();
            let on_select: MenuSelect = Rc::new(move |id, window, cx| {
                let id = id.to_string();
                let key = menu_key.clone();
                weak.update(cx, |sidebar, cx| {
                    sidebar.handle_thread_menu(&key, &id, window, cx)
                })
                .ok();
            });
            build_popup_menu(
                menu.min_w(gpui_kit::px(200.)),
                &items,
                on_select,
                window,
                cx,
            )
        }
    }

    /// Builder for a draft row's `context_menu`.
    pub(crate) fn draft_context_menu(
        this: WeakEntity<Self>,
        draft_id: String,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        move |menu, window, cx| {
            let Some(sidebar) = this.upgrade() else {
                return menu;
            };
            let draft = sidebar
                .read(cx)
                .snapshot(cx)
                .drafts
                .iter()
                .find(|draft| draft.draft_id == draft_id)
                .cloned();
            let Some(draft) = draft else {
                return menu;
            };
            let snapshot = sidebar.read(cx).snapshot(cx);
            let path = draft.worktree_path.clone().or_else(|| {
                snapshot
                    .project(&draft.project_key())
                    .map(|p| p.workspace_root.clone())
            });
            let has_project = snapshot.project_group_for(&draft.project_key()).is_some();
            let items =
                build_draft_action_menu_items(path.is_some(), draft.branch.is_some(), has_project);
            let weak = this.clone();
            let on_select: MenuSelect = Rc::new(move |id, window, cx| {
                let id = id.to_string();
                let draft = draft.clone();
                let path = path.clone();
                weak.update(cx, |sidebar, cx| match id.as_str() {
                    "project-settings" => {
                        if let Some(group) =
                            sidebar.snapshot(cx).project_group_for(&draft.project_key())
                        {
                            let key = group.project_key.clone();
                            cx.emit(SidebarEvent::OpenProjectSettings(key));
                        }
                    }
                    "copy-path" => {
                        if let Some(path) = &path {
                            cx.write_to_clipboard(ClipboardItem::new_string(path.clone()));
                            sidebar.toast_success("Path copied", path, window, cx);
                        }
                    }
                    "copy-branch" => {
                        if let Some(branch) = &draft.branch {
                            cx.write_to_clipboard(ClipboardItem::new_string(branch.clone()));
                            sidebar.toast_success("Branch copied", branch, window, cx);
                        }
                    }
                    "discard" => sidebar.discard_draft(&draft.draft_id, cx),
                    _ => {}
                })
                .ok();
            });
            build_popup_menu(
                menu.min_w(gpui_kit::px(180.)),
                &items,
                on_select,
                window,
                cx,
            )
        }
    }

    pub(crate) fn discard_draft(&mut self, draft_id: &str, cx: &mut Context<Self>) {
        self.backend.discard_draft(draft_id.to_string(), cx);
        if self.route == Some(SidebarRoute::Draft(draft_id.to_string())) {
            self.navigate_home(cx);
        }
    }

    /// The single-thread menu's actions.
    pub(crate) fn handle_thread_menu(
        &mut self,
        key: &str,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selection.has_selection() && self.selection.contains(key) {
            self.handle_multi_select_menu(id, window, cx);
            return;
        }
        let Some(thread) = self.thread(key, cx) else {
            return;
        };
        let thread_ref = ThreadRef::of(&thread);
        let _ = thread_action_requires_operate(id);
        if let Some(preset) = id.strip_prefix("snooze:") {
            if preset == "custom" {
                self.open_custom_snooze(vec![thread_ref], window, cx);
                return;
            }
            let format = self.snapshot(cx).settings.timestamp_format;
            if let Some(preset) = resolve_snooze_presets_now(format)
                .into_iter()
                .find(|candidate| candidate.id.as_str() == preset)
            {
                self.attempt_snooze(thread_ref, preset.snoozed_until, None, window, cx);
            }
            return;
        }
        let snapshot = self.snapshot(cx);
        let group = snapshot.project_group_for(&thread.project_key()).cloned();
        let workspace_path = thread.worktree_path.clone().or_else(|| {
            snapshot
                .project(&thread.project_key())
                .map(|p| p.workspace_root.clone())
        });
        match id {
            "filter-by-project" => {
                if let Some(group) = group {
                    // The only scope control here: picking the scoped
                    // project again goes back to all projects.
                    let active = self.snapshot(cx).project_scope_key.as_deref()
                        == Some(group.project_key.as_str());
                    self.set_project_scope((!active).then_some(group.project_key), cx);
                }
            }
            "project-settings" => {
                if let Some(group) = group {
                    cx.emit(SidebarEvent::OpenProjectSettings(group.project_key));
                }
            }
            "new-thread-on-branch" => cx.emit(SidebarEvent::NewThread {
                project_key: Some(thread.project_key()),
                branch: thread.branch.clone(),
                worktree_path: thread.worktree_path.clone(),
            }),
            "settle" => self.attempt_settle(thread_ref, None, window, cx),
            "unsettle" => self.attempt_unsettle(thread_ref, window, cx),
            "unsnooze" => self.attempt_unsnooze(thread_ref, window, cx),
            "pin" => self.attempt_pin(thread_ref, window, cx),
            "unpin" => self.attempt_unpin(thread_ref, window, cx),
            "auto-settle:enabled" | "auto-settle:disabled" => {
                let task =
                    self.backend
                        .set_auto_settle(thread_ref, id == "auto-settle:enabled", cx);
                self.run_command(
                    task,
                    "Failed to update auto-settle",
                    window,
                    cx,
                    |_, _, _, _| {},
                );
            }
            "rename" => self.start_rename(key, window, cx),
            "regenerate-title" => {
                if thread.title_regeneration.is_none() {
                    let task = self.backend.regenerate_title(thread_ref, cx);
                    self.run_command(
                        task,
                        "Failed to regenerate thread title",
                        window,
                        cx,
                        |_, _, _, _| {},
                    );
                }
            }
            "mark-unread" => self.backend.mark_unread(thread_ref, cx).detach(),
            "copy-path" => match workspace_path {
                Some(path) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(path.clone()));
                    self.toast_success("Path copied", &path, window, cx);
                }
                None => self.toast_error(
                    "Path unavailable",
                    "This thread does not have a workspace path to copy.",
                    window,
                    cx,
                ),
            },
            "copy-branch" => {
                if let Some(branch) = &thread.branch {
                    cx.write_to_clipboard(ClipboardItem::new_string(branch.clone()));
                    self.toast_success("Branch copied", branch, window, cx);
                }
            }
            "copy-thread-id" => {
                cx.write_to_clipboard(ClipboardItem::new_string(thread.id.to_string()));
                self.toast_success("Thread ID copied", thread.id.as_str(), window, cx);
            }
            "archive" => {
                let title = thread.title.clone();
                let confirm = self.snapshot(cx).settings.confirm_thread_archive;
                let run = move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    let task = this.backend.archive(thread_ref.clone(), cx);
                    this.run_command(
                        task,
                        "Failed to archive thread",
                        window,
                        cx,
                        |_, _, _, _| {},
                    );
                };
                if confirm {
                    self.confirm(
                        format!("Archive thread \"{title}\"?"),
                        None,
                        false,
                        window,
                        cx,
                        run,
                    );
                } else {
                    run(self, window, cx);
                }
            }
            "delete" => {
                let title = thread.title.clone();
                let confirm = self.snapshot(cx).settings.confirm_thread_delete;
                let deleted_key = key.to_string();
                let run = move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    let task = this.backend.delete(thread_ref.clone(), cx);
                    let key = deleted_key.clone();
                    this.run_command(
                        task,
                        "Failed to delete thread",
                        window,
                        cx,
                        move |this, ok, _, cx| {
                            if ok && this.route_thread_key() == Some(key.as_str()) {
                                this.navigate_home(cx);
                            }
                        },
                    );
                };
                if confirm {
                    self.confirm(
                        format!("Delete thread \"{title}\"?"),
                        Some(
                            "This permanently clears conversation history for this thread.".into(),
                        ),
                        true,
                        window,
                        cx,
                        run,
                    );
                } else {
                    run(self, window, cx);
                }
            }
            _ => {}
        }
    }

    /// The bulk menu's actions.
    fn handle_multi_select_menu(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.selected_threads(cx);
        if selected.is_empty() {
            return;
        }
        let keys: Vec<String> = selected.iter().map(EnvironmentThreadShell::key).collect();
        if let Some(preset) = id.strip_prefix("snooze:") {
            let refs: Vec<ThreadRef> = selected.iter().map(ThreadRef::of).collect();
            if preset == "custom" {
                self.open_custom_snooze(refs, window, cx);
                return;
            }
            let format = self.snapshot(cx).settings.timestamp_format;
            if let Some(preset) = resolve_snooze_presets_now(format)
                .into_iter()
                .find(|candidate| candidate.id.as_str() == preset)
            {
                // Post-snooze navigation skips threads snoozing in this batch.
                let co: HashSet<String> = keys.iter().cloned().collect();
                self.selection.clear();
                for thread in refs {
                    self.attempt_snooze(
                        thread,
                        preset.snoozed_until.clone(),
                        Some(co.clone()),
                        window,
                        cx,
                    );
                }
            }
            return;
        }
        match id {
            "unpin" => {
                for thread in selected.iter().filter(|thread| thread.pinned_at.is_some()) {
                    self.attempt_unpin(ThreadRef::of(thread), window, cx);
                }
                self.selection.clear();
            }
            "regenerate-title" => {
                for thread in selected
                    .iter()
                    .filter(|thread| thread.title_regeneration.is_none())
                {
                    let task = self.backend.regenerate_title(ThreadRef::of(thread), cx);
                    self.run_command(
                        task,
                        "Failed to regenerate thread titles",
                        window,
                        cx,
                        |_, _, _, _| {},
                    );
                }
                self.selection.clear();
            }
            "settle" => {
                self.settle_threads(&keys, window, cx);
                self.selection.clear();
            }
            "mark-unread" => {
                for thread in &selected {
                    self.backend.mark_unread(ThreadRef::of(thread), cx).detach();
                }
                self.selection.clear();
            }
            "delete" => {
                let count = keys.len();
                let run = move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    this.delete_selected(keys.clone(), window, cx);
                };
                if self.snapshot(cx).settings.confirm_thread_delete {
                    self.confirm(
                        format!(
                            "Delete {count} thread{}?",
                            if count == 1 { "" } else { "s" }
                        ),
                        Some(
                            "This permanently clears conversation history for these threads."
                                .into(),
                        ),
                        true,
                        window,
                        cx,
                        run,
                    );
                } else {
                    run(self, window, cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// `deleteSelectedThreadEntries` then deselect what is gone.
    fn delete_selected(&mut self, keys: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        let selected: Vec<String> = self.selection.selected().to_vec();
        cx.spawn_in(window, async move |this, cx| {
            let mut tasks = Vec::new();
            for key in &keys {
                let task = this
                    .update(cx, |this, cx| {
                        this.thread(key, cx)
                            .map(|thread| backend.delete(ThreadRef::of(&thread), cx))
                    })
                    .ok()
                    .flatten();
                tasks.push((key.clone(), task));
            }
            let mut tasks = tasks.into_iter();
            let outcome = delete_selected_thread_entries(&keys, |_, _| {
                let next = tasks.next().and_then(|(_, task)| task);
                async move {
                    match next {
                        Some(task) => Some(task.await),
                        None => None,
                    }
                }
            })
            .await;
            this.update_in(cx, |this, window, cx| {
                if let Some(error) = &outcome.first_failure {
                    this.toast_error("Failed to delete threads", error, window, cx);
                }
                let snapshot_keys: HashSet<String> = this
                    .snapshot(cx)
                    .threads
                    .iter()
                    .map(EnvironmentThreadShell::key)
                    .collect();
                let remove = get_thread_keys_to_deselect_after_delete(
                    &selected,
                    &outcome.deleted_thread_keys,
                    |key| snapshot_keys.contains(key),
                );
                this.selection.remove(&remove);
                if this
                    .route_thread_key()
                    .is_some_and(|route| outcome.deleted_thread_keys.contains(route))
                {
                    this.navigate_home(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `api.dialogs.confirm` → an alert dialog.
    fn confirm(
        &mut self,
        title: String,
        description: Option<String>,
        destructive: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_ok: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let this = cx.entity().downgrade();
        let on_ok = Rc::new(on_ok);
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            let on_ok = on_ok.clone();
            let alert = alert
                .title(title.clone().into_any_element())
                .show_cancel(true)
                .ok_text(if destructive { "Delete" } else { "OK" });
            let alert = match &description {
                Some(description) => alert.description(description.clone().into_any_element()),
                None => alert,
            };
            let alert = if destructive {
                alert.ok_variant(ButtonVariant::Danger)
            } else {
                alert
            };
            alert.on_ok(move |_, window, cx| {
                let on_ok = on_ok.clone();
                this.update(cx, |sidebar, cx| on_ok(sidebar, window, cx))
                    .ok();
                true
            })
        });
    }
}
