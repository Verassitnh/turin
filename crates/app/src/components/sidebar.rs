//! Port of upstream/apps/web/src/components/Sidebar.tsx (the v2 sidebar).
//!
//! One flat, sectioned thread list across projects: pinned, the active
//! inbox, the Working shelf (beta), Snoozed and the Settled tail, with a
//! project scope instead of a project tree (LegacySidebar.tsx, the opt-in
//! project tree, is not ported; see Deviations). The component is split by
//! concern:
//!
//! - this module: state, store sync, navigation, keyboard commands, layout;
//! - `sidebar_list`: the sortable list, shelf headers, drafts, motion;
//! - `sidebar_thread_row`: card and slim rows, status slot, hover actions;
//! - `sidebar_actions`: lifecycle commands, context menus, rename;
//! - `sidebar_gestures`: row drag (reorder/move) and action sweeps;
//! - `sidebar_search`: search results.
//!
//! Data comes from a [`view_model::SidebarStore`] and commands go to a
//! [`view_model::SidebarBackend`] (the T14 seam; [`mock_source`] for now).

pub mod custom_snooze_dialog;
pub mod mock_source;
pub mod runtime;
pub mod sidebar_actions;
pub mod sidebar_chrome;
pub mod sidebar_drag;
pub mod sidebar_gestures;
pub mod sidebar_list;
pub mod sidebar_logic;
pub mod sidebar_motion;
pub mod sidebar_pointer;
pub mod sidebar_search;
pub mod sidebar_sections;
pub mod sidebar_snooze;
pub mod sidebar_thread_header;
pub mod sidebar_thread_row;
pub mod thread_hover_card;
pub mod thread_selection;
pub mod thread_status_indicators;
pub mod view_model;

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    KeyBinding, Modifiers, ParentElement as _, Render, Styled as _, Subscription, Task, Window,
    actions, div,
};

use self::runtime::models::{EnvironmentThreadShell, SettledOverride};
use self::runtime::thread_inbox::InboxReturnTracker;
use self::sidebar_chrome::{SidebarChromeFooter, SidebarChromeHeader};
use self::sidebar_gestures::{DragSession, SweepSession};
use self::sidebar_logic::{
    JumpHintAction, SidebarProjectScopeMenuState, THREAD_JUMP_HINT_SHOW_DELAY_MS,
    ThreadJumpHintVisibilityController, ThreadTraversalDirection, resolve_adjacent_thread_id,
    should_create_new_thread_in_current_project,
};
use self::sidebar_motion::SidebarListMotion;
use self::sidebar_sections::{
    OptimisticDrop, SETTLED_TAIL_INITIAL_COUNT, SidebarListLayout, SidebarPartition,
    layout_sidebar_list, partition_sidebar_threads, should_release_optimistic_drop,
};
use self::thread_selection::ThreadSelection;
use self::view_model::{
    SharedSidebarBackend, SidebarSnapshot, SidebarStore, SidebarStoreChanged, ThreadRef,
    ThreadSearchMatch,
};
use crate::theme::ActiveAppTheme;
use crate::timestamp_format::now_iso;
use crate::ui::sidebar::{SidebarContent, SidebarGroup};

actions!(
    sidebar,
    [
        /// `thread.previous` (mod+shift+[).
        ThreadPrevious,
        /// `thread.next` (mod+shift+]).
        ThreadNext,
        /// `thread.settle` (mod+shift+s): park the open thread and move on.
        SettleThread,
        /// `thread.pin` (mod+shift+p).
        PinThread,
        /// `chat.new` (mod+n, mod+shift+o).
        NewThread,
        /// `chat.newLocal` (mod+shift+n): new thread in the current project.
        NewThreadInProject,
        ThreadJump1,
        ThreadJump2,
        ThreadJump3,
        ThreadJump4,
        ThreadJump5,
        ThreadJump6,
        ThreadJump7,
        ThreadJump8,
        ThreadJump9,
    ]
);

/// Upstream default bindings (packages/shared/src/keybindings.ts) for the
/// sidebar's commands, in the app shell's key context.
pub fn bind_keys(cx: &mut App, context: &'static str) {
    cx.bind_keys([
        KeyBinding::new("cmd-shift-[", ThreadPrevious, Some(context)),
        KeyBinding::new("cmd-shift-]", ThreadNext, Some(context)),
        KeyBinding::new("cmd-shift-s", SettleThread, Some(context)),
        KeyBinding::new("cmd-shift-p", PinThread, Some(context)),
        KeyBinding::new("cmd-n", NewThread, Some(context)),
        KeyBinding::new("cmd-shift-o", NewThread, Some(context)),
        KeyBinding::new("cmd-shift-n", NewThreadInProject, Some(context)),
        KeyBinding::new("cmd-1", ThreadJump1, Some(context)),
        KeyBinding::new("cmd-2", ThreadJump2, Some(context)),
        KeyBinding::new("cmd-3", ThreadJump3, Some(context)),
        KeyBinding::new("cmd-4", ThreadJump4, Some(context)),
        KeyBinding::new("cmd-5", ThreadJump5, Some(context)),
        KeyBinding::new("cmd-6", ThreadJump6, Some(context)),
        KeyBinding::new("cmd-7", ThreadJump7, Some(context)),
        KeyBinding::new("cmd-8", ThreadJump8, Some(context)),
        KeyBinding::new("cmd-9", ThreadJump9, Some(context)),
    ]);
}

/// The jump shortcut labels (`shortcutLabelForCommand(keybindings, thread.jump.N)`).
pub fn jump_shortcut_label(index: usize) -> Option<String> {
    (index < 9).then(|| format!("⌘{}", index + 1))
}

/// What the main view shows (upstream: the router).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarRoute {
    Thread(String),
    Draft(String),
}

/// Requests the sidebar makes of the app shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarEvent {
    /// The route changed (selection → main view).
    Navigate(Option<SidebarRoute>),
    /// Start a new thread (`useHandleNewThread`); `None` project opens the
    /// "New thread in…" picker.
    NewThread {
        project_key: Option<String>,
        branch: Option<String>,
        worktree_path: Option<String>,
    },
    AddProject,
    OpenProjectSettings(String),
    /// Open a thread's pull requests surface.
    OpenPullRequests(String),
    OpenSettings,
    OpenPullRequestsPage,
    OpenUsage,
}

/// Inline rename state.
pub(crate) struct RenameState {
    pub key: String,
    pub input: Entity<InputState>,
    pub original: String,
    pub committed: bool,
}

pub struct Sidebar {
    pub(crate) store: Entity<SidebarStore>,
    pub(crate) backend: SharedSidebarBackend,
    pub(crate) route: Option<SidebarRoute>,
    pub(crate) search_input: Entity<InputState>,
    pub(crate) search_query: String,
    pub(crate) active_search_index: usize,
    pub(crate) search_matches: Vec<ThreadSearchMatch>,
    pub(crate) search_pending: bool,
    pub(crate) search_task: Option<Task<()>>,
    pub(crate) scope_menu: SidebarProjectScopeMenuState,
    pub(crate) scope_input: Entity<InputState>,
    pub(crate) hovered_row: Option<String>,
    pub(crate) selection: ThreadSelection,
    pub(crate) rename: Option<RenameState>,
    pub(crate) settled_visible_count: usize,
    settled_reset_key: Option<String>,
    pub(crate) inbox_returns: InboxReturnTracker,
    pub(crate) optimistic_drop: Option<OptimisticDrop>,
    pub(crate) settling: HashSet<String>,
    pub(crate) snoozing: HashSet<String>,
    pub(crate) drag: Option<DragSession>,
    pub(crate) sweep: Option<SweepSession>,
    pub(crate) motion: SidebarListMotion,
    /// Last measured list box (window coords): `(top, height, left, width)`,
    /// written during prepaint.
    pub(crate) list_box_cell: Rc<Cell<Option<(f32, f32, f32, f32)>>>,
    pub(crate) epoch: Instant,
    jump_hints: ThreadJumpHintVisibilityController,
    jump_hint_task: Option<Task<()>>,
    pub(crate) snooze_menu_for: Option<String>,
    /// Section membership last frame, for motion keys and exit ghosts.
    pub(crate) last_rows: Vec<(
        String,
        EnvironmentThreadShell,
        sidebar_logic::SidebarSection,
    )>,
    _subscriptions: Vec<Subscription>,
    _ticker: Task<()>,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

impl Sidebar {
    pub fn new(
        store: Entity<SidebarStore>,
        backend: SharedSidebarBackend,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let scope_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search projects..."));
        let subscriptions = vec![
            cx.subscribe(&store, |this, _, _: &SidebarStoreChanged, cx| {
                this.on_store_changed(cx)
            }),
            cx.subscribe_in(
                &search_input,
                window,
                |this, input, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        let value = input.read(cx).value().to_string();
                        this.set_search_query(value, cx);
                    }
                    InputEvent::PressEnter { .. } => this.select_active_search_result(window, cx),
                    _ => {}
                },
            ),
            cx.subscribe(&scope_input, |this, input, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    let query = input.read(cx).value().to_string();
                    this.scope_menu = sidebar_logic::reduce_sidebar_project_scope_menu_state(
                        &this.scope_menu,
                        sidebar_logic::SidebarProjectScopeMenuAction::QueryChanged(query),
                    );
                    cx.notify();
                }
            }),
        ];
        // Relative labels tick by the minute, Working durations and snooze
        // wake boundaries by the second; one coarse timer drives both, and
        // only repaints while something time-dependent is on screen.
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this
                    .update(cx, |this, cx| {
                        if this.needs_tick(cx) {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut sidebar = Self {
            store,
            backend,
            route: None,
            search_input,
            search_query: String::new(),
            active_search_index: 0,
            search_matches: Vec::new(),
            search_pending: false,
            search_task: None,
            scope_menu: SidebarProjectScopeMenuState::default(),
            scope_input,
            hovered_row: None,
            selection: ThreadSelection::default(),
            rename: None,
            settled_visible_count: SETTLED_TAIL_INITIAL_COUNT,
            settled_reset_key: None,
            inbox_returns: InboxReturnTracker::default(),
            optimistic_drop: None,
            settling: HashSet::new(),
            snoozing: HashSet::new(),
            drag: None,
            sweep: None,
            motion: SidebarListMotion::new(),
            list_box_cell: Rc::new(Cell::new(None)),
            epoch: Instant::now(),
            jump_hints: ThreadJumpHintVisibilityController::default(),
            jump_hint_task: None,
            snooze_menu_for: None,
            last_rows: Vec::new(),
            _subscriptions: subscriptions,
            _ticker: ticker,
        };
        sidebar.settled_reset_key = sidebar.snapshot(cx).project_scope_key.clone();
        sidebar
    }

    pub(crate) fn list_box(&self) -> Option<(f32, f32, f32, f32)> {
        self.list_box_cell.get()
    }

    pub(crate) fn snapshot<'a>(&self, cx: &'a App) -> &'a SidebarSnapshot {
        self.store.read(cx).snapshot()
    }

    /// Milliseconds on the motion clock.
    pub(crate) fn motion_now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64() * 1000.
    }

    pub fn route(&self) -> Option<&SidebarRoute> {
        self.route.as_ref()
    }

    pub(crate) fn route_thread_key(&self) -> Option<&str> {
        match &self.route {
            Some(SidebarRoute::Thread(key)) => Some(key),
            _ => None,
        }
    }

    fn needs_tick(&self, cx: &App) -> bool {
        // Working rows show elapsed seconds; snoozed rows count down; the
        // relative labels roll over each minute.
        let snapshot = self.snapshot(cx);
        let second_boundary = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() % 60 == 0)
            .unwrap_or(false);
        second_boundary
            || snapshot.threads.iter().any(|thread| {
                thread.runtime.as_ref().is_some_and(|runtime| {
                    matches!(
                        runtime.status,
                        runtime::models::RunStatus::Running
                            | runtime::models::RunStatus::Preparing
                            | runtime::models::RunStatus::Starting
                            | runtime::models::RunStatus::Queued
                            | runtime::models::RunStatus::Waiting
                    )
                }) || thread.snoozed_until.is_some()
            })
    }

    fn on_store_changed(&mut self, cx: &mut Context<Self>) {
        // Scope flips drop the selection and reset the settled paging.
        let scope = self.snapshot(cx).project_scope_key.clone();
        if scope != self.settled_reset_key {
            self.settled_reset_key = scope;
            self.settled_visible_count = SETTLED_TAIL_INITIAL_COUNT;
            self.selection.clear();
        }
        // Release an optimistic drop once its writes land.
        if let Some(drop) = self.optimistic_drop.clone() {
            let (partition, _) = self.compute_sections(cx);
            let pinned: Vec<String> = partition
                .pinned
                .iter()
                .map(EnvironmentThreadShell::key)
                .collect();
            let active: Vec<String> = partition
                .active
                .iter()
                .map(EnvironmentThreadShell::key)
                .collect();
            if should_release_optimistic_drop(
                &drop,
                &self.snapshot(cx).threads,
                &pinned,
                &active,
                &now_iso(),
            ) {
                self.optimistic_drop = None;
            }
        }
        // A deleted or archived renaming row cancels the rename.
        if let Some(rename) = &self.rename
            && !self
                .snapshot(cx)
                .threads
                .iter()
                .any(|thread| thread.key() == rename.key && thread.archived_at.is_none())
        {
            self.rename = None;
        }
        cx.notify();
    }

    /// `scopedProjectKeys` for the persisted scope.
    pub(crate) fn scoped_project_keys(&self, cx: &App) -> Option<HashSet<String>> {
        let snapshot = self.snapshot(cx);
        let key = snapshot.project_scope_key.as_ref()?;
        let group = snapshot
            .project_groups
            .iter()
            .find(|group| &group.project_key == key)?;
        Some(group.member_project_keys.iter().cloned().collect())
    }

    /// The partition and list layout for this frame.
    pub(crate) fn compute_sections(&mut self, cx: &App) -> (SidebarPartition, SidebarListLayout) {
        let scoped = self.scoped_project_keys(cx);
        let snapshot = self.store.read(cx).snapshot();
        let partition = partition_sidebar_threads(
            snapshot,
            scoped.as_ref(),
            self.optimistic_drop.as_ref(),
            &mut self.inbox_returns,
            &now_iso(),
        );
        let layout = layout_sidebar_list(
            &partition,
            self.route_thread_key(),
            self.settled_visible_count,
            snapshot.settled_shelf_expanded,
            snapshot.snoozed_shelf_expanded,
            snapshot.working_shelf_expanded,
        );
        (partition, layout)
    }

    pub(crate) fn thread(&self, key: &str, cx: &App) -> Option<EnvironmentThreadShell> {
        self.snapshot(cx)
            .threads
            .iter()
            .find(|thread| thread.key() == key)
            .cloned()
    }

    /// `navigateToThread`: plain navigation; also clears a selection, sets
    /// the range anchor and advances the visited watermark.
    pub(crate) fn navigate_to_thread(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.selection.has_selection() {
            self.selection.clear();
        }
        self.selection.set_anchor(key);
        self.route = Some(SidebarRoute::Thread(key.to_string()));
        if let Some(thread) = self.thread(key, cx) {
            self.backend
                .mark_visited(ThreadRef::of(&thread), now_iso(), cx)
                .detach();
        }
        cx.emit(SidebarEvent::Navigate(self.route.clone()));
        cx.notify();
    }

    pub(crate) fn navigate_to_draft(&mut self, draft_id: &str, cx: &mut Context<Self>) {
        self.selection.clear();
        self.route = Some(SidebarRoute::Draft(draft_id.to_string()));
        cx.emit(SidebarEvent::Navigate(self.route.clone()));
        cx.notify();
    }

    /// The route was cleared elsewhere (home).
    pub fn navigate_home(&mut self, cx: &mut Context<Self>) {
        self.route = None;
        cx.emit(SidebarEvent::Navigate(None));
        cx.notify();
    }

    /// Open a thread by key (the app shell's own navigation).
    pub fn open_thread(&mut self, key: &str, cx: &mut Context<Self>) {
        self.navigate_to_thread(key, cx);
    }

    fn traverse(&mut self, direction: ThreadTraversalDirection, cx: &mut Context<Self>) {
        let (_, layout) = self.compute_sections(cx);
        let current = self.route_thread_key().map(str::to_string);
        if let Some(target) =
            resolve_adjacent_thread_id(&layout.ordered_keys, current.as_ref(), direction)
        {
            self.navigate_to_thread(&target, cx);
        }
    }

    fn jump(&mut self, index: usize, cx: &mut Context<Self>) {
        let (_, layout) = self.compute_sections(cx);
        if let Some(key) = layout.ordered_keys.get(index).cloned() {
            self.navigate_to_thread(&key, cx);
        }
    }

    /// Dispatch a sidebar keyboard action (the shell forwards them).
    pub fn handle_action(
        &mut self,
        action: &dyn gpui_kit::Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let any = action.as_any();
        let jumps = [
            any.is::<ThreadJump1>(),
            any.is::<ThreadJump2>(),
            any.is::<ThreadJump3>(),
            any.is::<ThreadJump4>(),
            any.is::<ThreadJump5>(),
            any.is::<ThreadJump6>(),
            any.is::<ThreadJump7>(),
            any.is::<ThreadJump8>(),
            any.is::<ThreadJump9>(),
        ];
        if let Some(index) = jumps.iter().position(|is| *is) {
            self.jump(index, cx);
        } else if any.is::<ThreadPrevious>() {
            self.traverse(ThreadTraversalDirection::Previous, cx);
        } else if any.is::<ThreadNext>() {
            self.traverse(ThreadTraversalDirection::Next, cx);
        } else if any.is::<SettleThread>() {
            if let Some(key) = self.route_thread_key().map(str::to_string)
                && let Some(thread) = self.thread(&key, cx)
                && thread.settled_override != Some(SettledOverride::Settled)
            {
                self.attempt_settle(ThreadRef::of(&thread), None, window, cx);
            }
        } else if any.is::<PinThread>() {
            if let Some(key) = self.route_thread_key().map(str::to_string)
                && let Some(thread) = self.thread(&key, cx)
            {
                if thread.pinned_at.is_some() {
                    self.attempt_unpin(ThreadRef::of(&thread), window, cx);
                } else {
                    self.attempt_pin(ThreadRef::of(&thread), window, cx);
                }
            }
        } else if any.is::<NewThread>() {
            self.handle_new_thread(false, cx);
        } else if any.is::<NewThreadInProject>() {
            self.handle_new_thread(true, cx);
        }
    }

    /// `handleNewThreadClick`: one project (or Shift) creates directly in
    /// the current project; several open the "New thread in…" picker.
    pub(crate) fn handle_new_thread(&mut self, shift: bool, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(cx);
        let group_count = snapshot.project_groups.len();
        let project_key = if should_create_new_thread_in_current_project(shift, group_count) {
            self.route_thread_key()
                .and_then(|key| snapshot.threads.iter().find(|thread| thread.key() == key))
                .map(EnvironmentThreadShell::project_key)
                .or_else(|| snapshot.projects.first().map(|project| project.key()))
        } else {
            None
        };
        cx.emit(SidebarEvent::NewThread {
            project_key,
            branch: None,
            worktree_path: None,
        });
    }

    /// `useShortcutModifierState` → jump hints after a 200ms hold.
    pub fn modifiers_changed(&mut self, modifiers: &Modifiers, cx: &mut Context<Self>) {
        // Only the exact thread-jump modifier set (⌘ alone) shows hints.
        let should_show = modifiers.platform
            && !modifiers.shift
            && !modifiers.alt
            && !modifiers.control
            && !modifiers.function;
        match self.jump_hints.sync(should_show) {
            JumpHintAction::Schedule => {
                self.jump_hint_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(THREAD_JUMP_HINT_SHOW_DELAY_MS))
                        .await;
                    this.update(cx, |this, cx| {
                        if this.jump_hints.fire() != JumpHintAction::None {
                            cx.notify();
                        }
                    })
                    .ok();
                }));
            }
            JumpHintAction::Changed(_) => {
                self.jump_hint_task = None;
                cx.notify();
            }
            JumpHintAction::None => {
                if !should_show {
                    self.jump_hint_task = None;
                }
            }
        }
    }

    pub(crate) fn show_jump_hints(&self) -> bool {
        self.jump_hints.is_visible()
    }

    pub(crate) fn emit(&mut self, event: SidebarEvent, cx: &mut Context<Self>) {
        cx.emit(event);
    }
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let scope = *theme.sidebar_scope();
        let header = self.render_thread_header(window, cx);
        let body = if self.is_searching() {
            self.render_search_results(window, cx).into_any_element()
        } else {
            self.render_thread_list(window, cx).into_any_element()
        };
        div()
            .id("sidebar")
            .flex()
            .flex_col()
            .size_full()
            .text_color(scope.sidebar_foreground)
            .child(SidebarChromeHeader)
            .child(
                SidebarContent::new("sidebar-content")
                    .fixed_header(SidebarGroup::new().child(header))
                    .child(SidebarGroup::new().with(|this| this.flex_1()).child(body)),
            )
            .child(SidebarChromeFooter::new(cx.entity().downgrade()))
            .children(self.render_gesture_listeners(window, cx))
    }
}
