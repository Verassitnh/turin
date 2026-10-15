//! Port of upstream/apps/web/src/components/CommandPalette.tsx.
//!
//! The ⌘K overlay: root actions and recent threads, project/thread/settings
//! search, submenus ("New thread in…", themes, appearance), and the Add
//! project / New project path flows. The component is split by concern:
//!
//! - this module: state, the open/close reducer wiring, query handling,
//!   thread/browse lookups, item execution and the keyboard;
//! - [`command_palette_logic`]: pure rules (reducer, filter, ranking, rows);
//! - [`palette_items`]: the item lists as functions of a snapshot;
//! - [`command_palette_content`]: the dialog chrome (`CommandPaletteContent`);
//! - [`command_palette_results`]: the virtualized rows
//!   (`CommandPaletteVirtualizedResults`).
//!
//! Data comes from a [`palette_source::PaletteBackend`] (the T14/T22 seam;
//! [`mock_source`] for now). Navigation is requested through
//! [`CommandPaletteEvent`], which the app shell handles like `SidebarEvent`.
//!
//! The ⌘P file picker and ⇧⌘F project content search are separate surfaces
//! (`ProjectFilePicker`, `ProjectContentSearchDialog`); the overlay mode
//! reducer is ported in full and those modes render a placeholder until their
//! tasks land.

pub mod command_palette_content;
pub mod command_palette_logic;
pub mod command_palette_results;
pub mod demo;
pub mod mock_source;
pub mod palette_items;
pub mod palette_source;
pub mod shortcut_labels;
pub mod theme_actions;

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, IntoElement,
    KeyBinding, KeyDownEvent, ListAlignment, ListState, Modifiers, PathPromptOptions, Render,
    Subscription, Task, Window, actions, div, px,
};
use turin_contracts::filesystem::{FilesystemBrowseEntry, FilesystemBrowseResult};
use turin_contracts::{EnvironmentId, ThreadId};

use self::command_palette_logic::{
    AppearanceModeChoice, BuildBrowseGroupsInput, CommandPaletteGroup, CommandPaletteItem,
    CommandPaletteLinkedThreads, CommandPaletteMode, CommandPaletteOpenIntent, CommandPaletteRows,
    CommandPaletteUiAction, CommandPaletteUiState, CommandPaletteView,
    FilterCommandPaletteGroupsInput, PaletteAction, PaletteIcon, PaletteItemKind,
    RECENT_THREAD_LIMIT, SearchOverlayMode, build_browse_groups, build_command_palette_rows,
    build_linked_thread_action_items, build_root_groups, filter_command_palette_groups,
    find_highlighted_command_palette_item, get_command_palette_input_placeholder,
    get_command_palette_mode, reduce_command_palette_ui_state,
};
use self::palette_items::{
    PaletteModel, ThreadItemInput, build_action_items, build_all_thread_items,
    build_appearance_groups, build_project_search_items, build_settings_search_items,
    build_theme_groups,
};
use self::palette_source::{
    AddProjectOutcome, PaletteContext, PaletteSnapshot, SharedPaletteBackend,
};
use crate::components::sidebar::view_model::{ThreadRef, ThreadSearchMatch};
use crate::state::contracts::js_trim;
use crate::state::helpers::filesystem::{
    BrowseNavigationCoordinator, FilesystemBrowsePath, filter_filesystem_browse_entries,
    get_filesystem_browse_path,
};
use crate::state::models::scoped_thread_key;
use crate::state::operations::projects::{ResolveAddProjectPath, resolve_add_project_path};
use crate::state::projects::{
    append_browse_path_segment, ensure_browse_directory_path, has_trailing_path_separator,
    is_explicit_relative_project_path,
};
use crate::timestamp_format::now_ms;
use crate::utils::normalize_search_text;

actions!(
    command_palette,
    [
        /// `commandPalette.toggle` (mod+k).
        ToggleCommandPalette,
        /// `filePicker.toggle` (mod+p).
        ToggleFilePicker,
        /// `projectSearch.toggle` (mod+shift+f).
        ToggleProjectSearch,
        /// `theme.select` (mod+alt+a).
        SelectTheme,
        /// `appearance.cycle` (mod+alt+shift+a).
        CycleAppearance,
    ]
);

/// Upstream default bindings (packages/shared/src/keybindings.ts) for the
/// palette's shortcuts, in the app shell's key context.
pub fn bind_keys(cx: &mut App, context: &'static str) {
    cx.bind_keys([
        KeyBinding::new("cmd-k", ToggleCommandPalette, Some(context)),
        KeyBinding::new("cmd-p", ToggleFilePicker, Some(context)),
        KeyBinding::new("cmd-shift-f", ToggleProjectSearch, Some(context)),
        KeyBinding::new("cmd-alt-a", SelectTheme, Some(context)),
        KeyBinding::new("cmd-alt-shift-a", CycleAppearance, Some(context)),
    ]);
}

/// Key context of the open overlay.
pub const KEY_CONTEXT: &str = "CommandPalette";

/// What the palette asks of the app shell.
#[derive(Clone, Debug, PartialEq)]
pub enum CommandPaletteEvent {
    /// The overlay closed: the shell returns focus to the composer.
    Closed,
    OpenThread(ThreadRef),
    /// `openProjectFromSearch`: the project's latest thread, else a new one.
    OpenProject {
        project_key: String,
    },
    /// `handleNewThread(projectRef)`.
    NewThreadInProject {
        project_key: String,
    },
    /// `startNewThreadFromContext`.
    NewThreadFromContext,
    /// `startScratchThread`.
    NewThreadWithoutProject,
    FindInCurrentThread,
    LinkPullRequest,
    ShowLinkedPullRequests,
    OpenPullRequests,
    OpenUsage,
    OpenSettings,
    OpenSetting {
        id: String,
    },
    OpenProjectSettings {
        project_key: String,
    },
    ToggleThemeEditor,
}

/// `commandPaletteBus.CommandPaletteOpenDetail`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandPaletteOpenDetail {
    pub open: Option<OpenTarget>,
    pub query: Option<String>,
    pub linked_threads: Option<CommandPaletteLinkedThreads>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenTarget {
    AddProject,
    NewThreadIn,
}

/// Thread-message search state (`useThreadSearch`).
#[derive(Default)]
pub(crate) struct ThreadSearchState {
    pub matches: HashMap<String, ThreadSearchMatch>,
    /// The query the matches were found for.
    pub query: String,
    pub pending: bool,
    pub task: Option<Task<()>>,
}

/// Folder browsing state (`filesystemEnvironment.browse`).
#[derive(Default)]
pub(crate) struct BrowseState {
    pub result: Option<FilesystemBrowseResult>,
    /// The directory `result` (or the request in flight) is for.
    pub directory: Option<String>,
    pub pending: bool,
    pub error: Option<String>,
    pub task: Option<Task<()>>,
}

/// The name step of New project.
#[derive(Clone, Debug, Default)]
pub(crate) struct NewProjectFlow {
    pub creating: bool,
}

const THREAD_SEARCH_DEBOUNCE_MS: u64 = 150;
const BROWSE_PLATFORM: &str = "MacIntel";

pub struct CommandPalette {
    pub(crate) backend: SharedPaletteBackend,
    pub(crate) context: PaletteContext,
    pub(crate) ui: CommandPaletteUiState,
    pub(crate) input: Entity<InputState>,
    pub(crate) focus_handle: FocusHandle,
    pub(crate) query: String,
    pub(crate) linked_thread_search: Option<(String, CommandPaletteLinkedThreads)>,
    pub(crate) view_stack: Vec<CommandPaletteView>,
    pub(crate) highlighted: Option<String>,
    /// Typing or entering a submenu clears the highlight; the first ArrowDown
    /// lands on the first row (the palette shows none until then).
    pub(crate) highlight_cleared: bool,
    pub(crate) list: ListState,
    pub(crate) snapshot: Option<PaletteSnapshot>,
    pub(crate) displayed_groups: Vec<CommandPaletteGroup>,
    pub(crate) rows: CommandPaletteRows,
    pub(crate) thread_search: ThreadSearchState,
    pub(crate) browse: BrowseState,
    pub(crate) browse_navigation: BrowseNavigationCoordinator,
    pub(crate) new_project_flow: Option<NewProjectFlow>,
    pub(crate) picking_folder: bool,
    /// Height taken above the results by the preview block and status line.
    pub(crate) panel_extra_height: f32,
    pub(crate) placeholder_shown: &'static str,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CommandPaletteEvent> for CommandPalette {}

impl CommandPalette {
    pub fn new(backend: SharedPaletteBackend, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let subscriptions = vec![cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, window, cx| {
                if let InputEvent::Change = event {
                    let value = input.read(cx).value().to_string();
                    this.handle_query_change(value, window, cx);
                }
            },
        )];
        Self {
            backend,
            context: PaletteContext::default(),
            ui: CommandPaletteUiState::default(),
            input,
            focus_handle: cx.focus_handle(),
            query: String::new(),
            linked_thread_search: None,
            view_stack: Vec::new(),
            highlighted: None,
            highlight_cleared: false,
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            snapshot: None,
            displayed_groups: Vec::new(),
            rows: CommandPaletteRows::default(),
            thread_search: ThreadSearchState::default(),
            browse: BrowseState::default(),
            browse_navigation: BrowseNavigationCoordinator::new(),
            new_project_flow: None,
            picking_folder: false,
            panel_extra_height: 0.,
            placeholder_shown: "",
            _subscriptions: subscriptions,
        }
    }

    pub fn is_open(&self) -> bool {
        self.ui.open
    }

    pub fn mode(&self) -> SearchOverlayMode {
        self.ui.mode
    }

    /// The routed thread/draft, so contextual items (find, copy reference,
    /// restart, link PR) target it. Set by the shell when the route changes.
    pub fn set_context(&mut self, context: PaletteContext, cx: &mut Context<Self>) {
        if self.context != context {
            self.context = context;
            if self.ui.open {
                self.recompute(cx);
                cx.notify();
            }
        }
    }

    fn dispatch(
        &mut self,
        action: CommandPaletteUiAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let was_open = self.ui.open;
        let previous_mode = self.ui.mode;
        self.ui = reduce_command_palette_ui_state(&self.ui, action);
        if self.ui.open && (!was_open || previous_mode != self.ui.mode) {
            self.begin_session(window, cx);
        } else if self.ui.open {
            self.apply_open_intent(window, cx);
        }
        if was_open && !self.ui.open {
            self.end_session(cx);
        }
        cx.notify();
    }

    pub fn set_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(CommandPaletteUiAction::SetOpen(open), window, cx);
    }

    /// Re-triggering a mode's shortcut toggles it closed.
    pub fn toggle_mode(
        &mut self,
        mode: SearchOverlayMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dispatch(CommandPaletteUiAction::ToggleMode(mode), window, cx);
    }

    /// `openCommandPalette(detail)` (commandPaletteBus).
    pub fn open_with(
        &mut self,
        detail: CommandPaletteOpenDetail,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let action = match (detail.open, detail.query) {
            (Some(OpenTarget::NewThreadIn), _) => CommandPaletteUiAction::OpenNewThreadIn,
            (Some(OpenTarget::AddProject), _) => CommandPaletteUiAction::OpenAddProject,
            (None, Some(query)) => CommandPaletteUiAction::OpenSearch {
                query,
                linked_threads: detail.linked_threads,
            },
            (None, None) => CommandPaletteUiAction::SetOpen(true),
        };
        self.dispatch(action, window, cx);
    }

    /// `theme.select`: opens straight into "Change theme".
    pub fn open_change_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dispatch(CommandPaletteUiAction::OpenChangeTheme, window, cx);
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_open(false, window, cx);
    }

    /// A fresh `OpenCommandPaletteDialog`: the dialog remounts on every open
    /// (and on a mode switch), so all per-open state resets.
    fn begin_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.clear();
        self.linked_thread_search = None;
        self.view_stack.clear();
        self.highlighted = None;
        self.highlight_cleared = false;
        self.thread_search = ThreadSearchState::default();
        self.browse = BrowseState::default();
        self.browse_navigation.invalidate();
        self.new_project_flow = None;
        self.picking_folder = false;
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.recompute(cx);
        self.apply_open_intent(window, cx);
    }

    fn end_session(&mut self, cx: &mut Context<Self>) {
        self.browse_navigation.invalidate();
        self.thread_search.task = None;
        self.browse.task = None;
        cx.emit(CommandPaletteEvent::Closed);
    }

    /// The `useLayoutEffect`s keyed on `openIntent`.
    fn apply_open_intent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(intent) = self.ui.open_intent.clone() else {
            return;
        };
        self.ui =
            reduce_command_palette_ui_state(&self.ui, CommandPaletteUiAction::ClearOpenIntent);
        self.browse_navigation.invalidate();
        self.new_project_flow = None;
        self.view_stack.clear();
        match intent {
            CommandPaletteOpenIntent::Search {
                query,
                linked_threads,
            } => {
                self.linked_thread_search = linked_threads.map(|linked| (query.clone(), linked));
                self.set_input_value(&query, window, cx);
                self.query = query;
            }
            CommandPaletteOpenIntent::AddProject => self.open_add_project_flow(window, cx),
            CommandPaletteOpenIntent::NewThreadIn => {
                let snapshot = self.backend.snapshot(cx);
                let model = PaletteModel::new(&snapshot, &self.context, true);
                let items = palette_items::build_project_thread_items(&model);
                if !items.is_empty() {
                    self.set_input_value("", window, cx);
                    self.query.clear();
                    self.push_view(
                        CommandPaletteView {
                            addon_icon: PaletteIcon::Named(crate::ui::icon::IconName::SquarePen),
                            groups: vec![CommandPaletteGroup::new("projects", "Projects", items)],
                            initial_query: None,
                        },
                        window,
                        cx,
                    );
                }
            }
            CommandPaletteOpenIntent::ChangeTheme => {
                self.push_view(
                    CommandPaletteView {
                        addon_icon: PaletteIcon::Named(crate::ui::icon::IconName::Palette),
                        groups: vec![CommandPaletteGroup::new(
                            "themes",
                            "Change theme",
                            Vec::new(),
                        )],
                        initial_query: None,
                    },
                    window,
                    cx,
                );
            }
        }
        self.recompute(cx);
    }

    pub(crate) fn set_input_value(
        &mut self,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = value.to_owned();
        self.input
            .update(cx, |input, cx| input.set_value(value, window, cx));
    }

    // ── Views ───────────────────────────────────────────────────────────

    pub(crate) fn current_view(&self) -> Option<&CommandPaletteView> {
        self.view_stack.last()
    }

    /// `pushPaletteView`.
    pub(crate) fn push_view(
        &mut self,
        view: CommandPaletteView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.browse_navigation.invalidate();
        let initial_query = view.initial_query.clone();
        self.view_stack.push(view);
        self.highlight_cleared = true;
        self.highlighted = None;
        let query = initial_query.unwrap_or_default();
        self.set_input_value(&query, window, cx);
        self.query = query;
        self.browse = BrowseState::default();
        self.recompute(cx);
        self.sync_browse(window, cx);
    }

    /// `popView`.
    pub(crate) fn pop_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.browse_navigation.invalidate();
        self.new_project_flow = None;
        self.view_stack.pop();
        self.highlighted = None;
        self.set_input_value("", window, cx);
        self.query.clear();
        self.browse = BrowseState::default();
        self.recompute(cx);
        cx.notify();
    }

    // ── Query ───────────────────────────────────────────────────────────

    /// `handleQueryChange`.
    pub(crate) fn handle_query_change(
        &mut self,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if value == self.query {
            return;
        }
        self.browse_navigation.invalidate();
        self.highlight_cleared = true;
        self.highlighted = None;
        let emptied_initial_query = value.is_empty()
            && self
                .current_view()
                .is_some_and(|view| view.initial_query.as_deref().is_some_and(|q| !q.is_empty()));
        self.query = value;
        if emptied_initial_query {
            self.pop_view(window, cx);
            return;
        }
        self.schedule_thread_search(cx);
        self.recompute(cx);
        self.sync_browse(window, cx);
        cx.notify();
    }

    pub(crate) fn is_actions_only(&self) -> bool {
        self.query.starts_with('>')
    }

    /// `useThreadSearch` with a two-character floor, debounced.
    fn schedule_thread_search(&mut self, cx: &mut Context<Self>) {
        let query = if self.current_view().is_none() && !self.is_actions_only() {
            self.query.trim().to_owned()
        } else {
            String::new()
        };
        if query.chars().count() < 2 {
            self.thread_search = ThreadSearchState::default();
            return;
        }
        if query == self.thread_search.query && !self.thread_search.pending {
            return;
        }
        self.thread_search.pending = true;
        let backend = self.backend.clone();
        self.thread_search.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(THREAD_SEARCH_DEBOUNCE_MS))
                .await;
            let Ok(task) = this.update(cx, |_, cx| backend.search_messages(query.clone(), cx))
            else {
                return;
            };
            let found = task.await;
            this.update(cx, |this, cx| {
                this.thread_search.matches = found
                    .into_iter()
                    .map(|hit| (scoped_thread_key(&hit.environment_id, &hit.thread_id), hit))
                    .collect();
                this.thread_search.query = query;
                this.thread_search.pending = false;
                this.recompute(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    // ── Browse ──────────────────────────────────────────────────────────

    pub(crate) fn browse_path(&self) -> FilesystemBrowsePath {
        get_filesystem_browse_path(
            &self.query,
            BROWSE_PLATFORM,
            self.new_project_flow.is_none(),
        )
    }

    /// `relativePathNeedsActiveProject`.
    pub(crate) fn relative_path_needs_active_project(&self) -> bool {
        is_explicit_relative_project_path(self.query.trim())
            && self
                .snapshot
                .as_ref()
                .is_none_or(|snapshot| snapshot.current_project_cwd.is_none())
    }

    /// Loads the directory the query points at when it changed.
    pub(crate) fn sync_browse(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let path = self.browse_path();
        if !path.is_browsing
            || path.directory_path.is_empty()
            || self.relative_path_needs_active_project()
        {
            return;
        }
        if self.browse.directory.as_deref() == Some(path.directory_path.as_str()) {
            return;
        }
        let directory = path.directory_path.clone();
        self.browse.directory = Some(directory.clone());
        self.browse.pending = true;
        self.browse.error = None;
        let cwd = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.current_project_cwd.clone());
        let task = self.backend.browse(directory.clone(), cwd, cx);
        self.browse.task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                // A newer directory superseded this request.
                if this.browse.directory.as_deref() != Some(directory.as_str()) {
                    return;
                }
                this.browse.pending = false;
                match result {
                    Ok(result) => {
                        this.browse.result = Some(result);
                        this.browse.error = None;
                    }
                    Err(message) => {
                        this.browse.result = None;
                        this.browse.error = Some(message);
                    }
                }
                this.recompute(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    /// The directory entries under the query, filtered by its leaf segment
    /// (`filterFilesystemBrowseEntries`), and the exact match if any.
    pub(crate) fn visible_browse_entries(
        &self,
    ) -> (Vec<FilesystemBrowseEntry>, Option<FilesystemBrowseEntry>) {
        let path = self.browse_path();
        let entries = self
            .browse
            .result
            .as_ref()
            .map(|result| result.entries.as_slice())
            .unwrap_or(&[]);
        let filtered = filter_filesystem_browse_entries(entries, &path.filter_query);
        (
            filtered.visible_entries.into_iter().cloned().collect(),
            filtered.exact_entry.cloned(),
        )
    }

    // ── Groups and rows ─────────────────────────────────────────────────

    pub(crate) fn mode_kind(&self) -> CommandPaletteMode {
        get_command_palette_mode(self.current_view(), self.browse_path().is_browsing)
    }

    pub(crate) fn placeholder(&self) -> &'static str {
        if self.new_project_flow.is_some() {
            return "Project name";
        }
        get_command_palette_input_placeholder(self.mode_kind())
    }

    /// Rebuilds `displayed_groups` and the row list from the current state.
    pub(crate) fn recompute(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.backend.snapshot(cx);
        let mac = true;
        let groups = self.compute_displayed_groups(&snapshot, mac, cx);
        self.snapshot = Some(snapshot);
        self.rows = build_command_palette_rows(&groups);
        self.displayed_groups = groups;
        self.list.reset(self.rows.rows.len());
        // Keep the highlight only while its row still exists.
        if let Some(value) = &self.highlighted
            && !self.rows.item_values.iter().any(|item| item == value)
        {
            self.highlighted = None;
        }
    }

    fn compute_displayed_groups(
        &self,
        snapshot: &PaletteSnapshot,
        mac: bool,
        cx: &App,
    ) -> Vec<CommandPaletteGroup> {
        let model = PaletteModel::new(snapshot, &self.context, mac);
        let now = now_ms();
        let thread_search_query = if self.current_view().is_none() && !self.is_actions_only() {
            self.query.as_str()
        } else {
            ""
        };
        let thread_input = ThreadItemInput {
            model: &model,
            content_matches: &self.thread_search.matches,
            search_query: thread_search_query,
            matched_query: &self.thread_search.query,
            now_ms: now,
            limit: None,
        };
        let all_thread_items = build_all_thread_items(&thread_input);
        let recent_thread_items: Vec<CommandPaletteItem> = all_thread_items
            .iter()
            .take(RECENT_THREAD_LIMIT)
            .cloned()
            .collect();

        let browse_path = self.browse_path();
        if self.new_project_flow.is_some() {
            return vec![CommandPaletteGroup::new(
                "new-project-existing",
                "",
                vec![
                    CommandPaletteItem::action(
                        "new-project:existing",
                        "Add existing project",
                        PaletteIcon::Named(crate::ui::icon::IconName::FolderPlus),
                        PaletteAction::NewProjectExisting,
                    )
                    .description("Open a folder")
                    .keep_open()
                    .trailing(command_palette_logic::PaletteAddon::ChevronRight),
                ],
            )];
        }
        if browse_path.is_browsing {
            if self.relative_path_needs_active_project() {
                return Vec::new();
            }
            let (entries, _) = self.visible_browse_entries();
            let refs: Vec<&FilesystemBrowseEntry> = entries.iter().collect();
            return build_browse_groups(&BuildBrowseGroupsInput {
                browse_entries: &refs,
                browse_query: &self.query,
                can_browse_up: browse_path.can_browse_up,
                up_icon: PaletteIcon::Named(crate::ui::icon::IconName::CornerLeftUp),
                directory_icon: PaletteIcon::Named(crate::ui::icon::IconName::Folder),
            });
        }

        let active_groups: Vec<CommandPaletteGroup> = match self.current_view() {
            Some(view) => match view.groups.first().map(|group| group.value.as_str()) {
                Some("themes") => build_theme_groups(snapshot),
                Some("appearance") => build_appearance_groups(snapshot),
                Some(value) if value.starts_with("sources:") => self.build_source_groups(snapshot),
                _ => view.groups.clone(),
            },
            None => build_root_groups(build_action_items(&model), recent_thread_items),
        };

        let project_search_items = build_project_search_items(&model);
        let settings_hits = self.backend.search_settings(&self.query, cx);
        let settings_search_items = build_settings_search_items(&settings_hits);
        let thread_search_items = match &self.linked_thread_search {
            Some((query, linked)) if self.query == *query => build_linked_thread_action_items(
                linked,
                query,
                PaletteIcon::Named(crate::ui::icon::IconName::MessageSquare),
            ),
            _ => all_thread_items,
        };
        filter_command_palette_groups(&FilterCommandPaletteGroupsInput {
            active_groups: &active_groups,
            query: &self.query,
            is_in_submenu: self.current_view().is_some(),
            project_search_items: &project_search_items,
            settings_search_items: &settings_search_items,
            thread_search_items: &thread_search_items,
        })
    }

    // ── Add project / New project ───────────────────────────────────────

    /// `buildAddProjectSourceGroups`, minus the clone sources (project clone
    /// is cut): New project (when the machine has a projects root) and Local
    /// folder.
    fn build_source_groups(&self, snapshot: &PaletteSnapshot) -> Vec<CommandPaletteGroup> {
        let mut items = vec![
            CommandPaletteItem::action(
                "action:add-project:local:local",
                "Local folder",
                PaletteIcon::Named(crate::ui::icon::IconName::FolderPlus),
                PaletteAction::AddProjectLocalFolder,
            )
            .search_terms(["local", "folder", "directory", "browse"])
            .description("Browse a folder on disk")
            .keep_open(),
        ];
        if snapshot.new_projects_root.is_some() {
            items.insert(
                0,
                CommandPaletteItem::action(
                    "action:add-project:local:new",
                    "New project",
                    PaletteIcon::Named(crate::ui::icon::IconName::FolderGit2),
                    PaletteAction::NewProject,
                )
                .search_terms(["new project", "create", "empty", "repository", "git init"])
                .description("Start a new Git repository from a name")
                .keep_open(),
            );
        }
        vec![CommandPaletteGroup::new("sources:local", "Sources", items)]
    }

    /// `openAddProjectFlow` (one connected environment: straight to its sources).
    fn open_add_project_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = CommandPaletteView {
            addon_icon: PaletteIcon::Named(crate::ui::icon::IconName::FolderPlus),
            groups: vec![CommandPaletteGroup::new(
                "sources:local",
                "Sources",
                Vec::new(),
            )],
            initial_query: None,
        };
        self.push_view(view, window, cx);
    }

    /// `startAddProjectBrowse`.
    pub(crate) fn start_add_project_browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let base = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.add_project_base_directory.clone())
            .map(|directory| directory.trim().to_owned())
            .filter(|directory| !directory.is_empty());
        let initial_query = match base {
            Some(directory) => ensure_browse_directory_path(&directory),
            None => "~/".to_owned(),
        };
        self.push_view(
            CommandPaletteView {
                addon_icon: PaletteIcon::Named(crate::ui::icon::IconName::FolderPlus),
                groups: Vec::new(),
                initial_query: Some(initial_query),
            },
            window,
            cx,
        );
    }

    /// `startNewProject`.
    pub(crate) fn start_new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_project_flow = Some(NewProjectFlow::default());
        self.push_view(
            CommandPaletteView {
                addon_icon: PaletteIcon::Named(crate::ui::icon::IconName::FolderGit2),
                groups: Vec::new(),
                initial_query: None,
            },
            window,
            cx,
        );
    }

    /// `handleAddProject(rawCwd)`.
    pub(crate) fn handle_add_project(
        &mut self,
        raw_cwd: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current_cwd = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.current_project_cwd.clone());
        let cwd = match resolve_add_project_path(&raw_cwd, current_cwd.as_deref(), BROWSE_PLATFORM)
        {
            ResolveAddProjectPath::Ok(path) => path,
            ResolveAddProjectPath::Err(message) => {
                self.toast_error("Failed to add project", &message, window, cx);
                return;
            }
        };
        let task = self.backend.add_project(cwd, cx);
        cx.spawn_in(window, async move |this, cx| {
            let outcome = task.await;
            this.update_in(cx, |this, window, cx| match outcome {
                Ok(AddProjectOutcome::OpenedExisting { project_key }) => {
                    this.close(window, cx);
                    cx.emit(CommandPaletteEvent::OpenProject { project_key });
                }
                Ok(AddProjectOutcome::Created { project_key }) => {
                    this.close(window, cx);
                    cx.emit(CommandPaletteEvent::NewThreadInProject { project_key });
                }
                Err(message) => this.toast_error("Failed to add project", &message, window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// `submitNewProject`.
    pub(crate) fn submit_new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.query.trim().to_owned();
        let Some(flow) = self.new_project_flow.as_mut() else {
            return;
        };
        if name.is_empty() || flow.creating {
            return;
        }
        flow.creating = true;
        let task = self.backend.create_new_project(name, cx);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = task.await;
            this.update_in(cx, |this, window, cx| {
                if let Some(flow) = this.new_project_flow.as_mut() {
                    flow.creating = false;
                }
                match outcome {
                    Ok(project_key) => {
                        this.close(window, cx);
                        cx.emit(CommandPaletteEvent::NewThreadInProject { project_key });
                    }
                    Err(message) => {
                        this.toast_error("Failed to create project", &message, window, cx)
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// "Open in Finder": the native folder picker (`api.dialogs.pickFolder`).
    pub(crate) fn pick_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picking_folder {
            return;
        }
        self.picking_folder = true;
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let picked = paths.await;
            this.update_in(cx, |this, window, cx| {
                this.picking_folder = false;
                if let Ok(Ok(Some(paths))) = picked
                    && let Some(path) = paths.into_iter().next()
                {
                    this.handle_add_project(path.to_string_lossy().into_owned(), window, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toast_error(&self, title: &str, description: &str, window: &mut Window, cx: &mut App) {
        window.push_notification(
            Notification::error(description.to_owned()).title(title.to_owned()),
            cx,
        );
    }

    // ── Execution ───────────────────────────────────────────────────────

    /// `executeItem`.
    pub(crate) fn execute_item(
        &mut self,
        item: &CommandPaletteItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if item.disabled {
            return;
        }
        match &item.kind {
            PaletteItemKind::Submenu {
                addon_icon,
                groups,
                initial_query,
            } => {
                self.push_view(
                    CommandPaletteView {
                        addon_icon: *addon_icon,
                        groups: groups.clone(),
                        initial_query: initial_query.clone(),
                    },
                    window,
                    cx,
                );
            }
            PaletteItemKind::Action { keep_open, run } => {
                if !keep_open {
                    self.close(window, cx);
                }
                self.run_action(run.clone(), window, cx);
            }
        }
        cx.notify();
    }

    fn run_action(&mut self, action: PaletteAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            PaletteAction::Noop => {}
            PaletteAction::OpenThread {
                environment_id,
                thread_id,
            } => cx.emit(CommandPaletteEvent::OpenThread(ThreadRef {
                environment_id,
                thread_id,
            })),
            PaletteAction::OpenProject { project_key } => {
                cx.emit(CommandPaletteEvent::OpenProject { project_key })
            }
            PaletteAction::NewThreadInProject { project_key } => {
                cx.emit(CommandPaletteEvent::NewThreadInProject { project_key })
            }
            PaletteAction::NewThreadFromContext => {
                cx.emit(CommandPaletteEvent::NewThreadFromContext)
            }
            PaletteAction::NewThreadWithoutProject => {
                cx.emit(CommandPaletteEvent::NewThreadWithoutProject)
            }
            PaletteAction::FindInCurrentThread => cx.emit(CommandPaletteEvent::FindInCurrentThread),
            PaletteAction::CopyThreadReference => self.copy_thread_reference(window, cx),
            PaletteAction::LinkPullRequest => cx.emit(CommandPaletteEvent::LinkPullRequest),
            PaletteAction::ShowLinkedPullRequests => {
                cx.emit(CommandPaletteEvent::ShowLinkedPullRequests)
            }
            PaletteAction::RestartAgentSession => self.restart_agent_session(window, cx),
            PaletteAction::OpenFilePicker => self.toggle_mode(SearchOverlayMode::Files, window, cx),
            PaletteAction::SearchProjectContents => {
                self.toggle_mode(SearchOverlayMode::Content, window, cx)
            }
            PaletteAction::NewProject => self.start_new_project(window, cx),
            PaletteAction::AddProject => self.open_add_project_flow(window, cx),
            PaletteAction::AddProjectLocalFolder => self.start_add_project_browse(window, cx),
            PaletteAction::NewProjectExisting => {
                // `showExistingProjectSources`: back to the sources view.
                self.pop_view(window, cx);
                self.open_add_project_flow(window, cx);
            }
            PaletteAction::ToggleNewProjectPublish => {}
            PaletteAction::SetTheme { id } => theme_actions::apply_theme(id, cx),
            PaletteAction::SetAppearance { mode } => theme_actions::apply_appearance_mode(mode, cx),
            PaletteAction::ToggleThemeEditor => cx.emit(CommandPaletteEvent::ToggleThemeEditor),
            PaletteAction::OpenPullRequests => cx.emit(CommandPaletteEvent::OpenPullRequests),
            PaletteAction::OpenUsage => cx.emit(CommandPaletteEvent::OpenUsage),
            PaletteAction::OpenSettings => cx.emit(CommandPaletteEvent::OpenSettings),
            PaletteAction::OpenProjectSettings { project_key } => {
                cx.emit(CommandPaletteEvent::OpenProjectSettings { project_key })
            }
            PaletteAction::OpenSetting { id } => cx.emit(CommandPaletteEvent::OpenSetting { id }),
            PaletteAction::BrowseUp => self.browse_up(window, cx),
            PaletteAction::BrowseTo { name } => self.browse_to(&name, window, cx),
        }
        if matches!(
            self.view_stack.last(),
            Some(view) if view.groups.first().is_some_and(|g| g.value == "appearance" || g.value == "themes")
        ) {
            self.recompute(cx);
        }
    }

    fn browse_to(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let next = append_browse_path_segment(&self.query, name);
        self.highlighted = None;
        self.set_input_value(&next, window, cx);
        self.query = next;
        self.recompute(cx);
        self.sync_browse(window, cx);
    }

    fn browse_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(parent) = self.browse_path().parent_path else {
            return;
        };
        self.highlighted = None;
        self.set_input_value(&parent, window, cx);
        self.query = parent;
        self.recompute(cx);
        self.sync_browse(window, cx);
    }

    fn copy_thread_reference(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.reference_copy_target.clone());
        let Some(target) = target else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(target.value.clone()));
        window.push_notification(
            Notification::success(target.value).title(target.success_title),
            cx,
        );
    }

    fn restart_agent_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.context.active_thread_key.clone() else {
            return;
        };
        let Some((environment, thread)) = key.split_once(':') else {
            return;
        };
        let (Ok(environment_id), Ok(thread_id)) =
            (EnvironmentId::new(environment), ThreadId::new(thread))
        else {
            return;
        };
        let task = self.backend.restart_agent_session(
            ThreadRef {
                environment_id,
                thread_id,
            },
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let outcome = task.await;
            this.update_in(cx, |this, window, cx| match outcome {
                Ok(()) => window.push_notification(
                    Notification::success("Your next message starts a fresh session.")
                        .title("Agent session will restart"),
                    cx,
                ),
                Err(message) => this.toast_error("Unable to run command", &message, window, cx),
            })
            .ok();
        })
        .detach();
    }

    // ── Keyboard ────────────────────────────────────────────────────────

    pub(crate) fn highlighted_item(&self) -> Option<&CommandPaletteItem> {
        find_highlighted_command_palette_item(&self.displayed_groups, self.highlighted.as_deref())
    }

    /// `autoHighlight="always"` outside browse and name steps.
    pub(crate) fn auto_highlights_first_row(&self) -> bool {
        !self.browse_path().is_browsing && self.new_project_flow.is_none()
    }

    /// The highlighted value, or the hidden first-row highlight Base UI keeps
    /// until the user navigates.
    fn effective_highlight(&self) -> Option<String> {
        self.highlighted.clone().or_else(|| {
            self.auto_highlights_first_row()
                .then(|| self.rows.item_values.first().cloned())
                .flatten()
        })
    }

    pub(crate) fn move_highlight(&mut self, delta: isize) {
        let count = self.rows.item_values.len();
        if count == 0 {
            return;
        }
        let current = self
            .highlighted
            .as_ref()
            .and_then(|value| self.rows.item_values.iter().position(|item| item == value));
        let next = match (current, self.highlight_cleared, delta) {
            // The first ArrowDown after typing lands on the first row.
            (None, true, d) if d > 0 => 0,
            (None, _, d) if d > 0 => 0,
            (None, _, _) => count - 1,
            (Some(index), _, d) => (index as isize + d).rem_euclid(count as isize) as usize,
        };
        self.highlight_cleared = false;
        self.highlighted = Some(self.rows.item_values[next].clone());
        if let Some(row) = self.rows.row_index_by_item_index.get(next) {
            self.list.scroll_to_reveal_item(*row);
        }
    }

    /// `isPrimaryModifierPressed`.
    fn primary_modifier(modifiers: &Modifiers) -> bool {
        modifiers.platform && !modifiers.control
    }

    pub(crate) fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ui.open || self.ui.mode != SearchOverlayMode::Command {
            if self.ui.open && event.keystroke.key == "escape" {
                cx.stop_propagation();
                self.toggle_mode(SearchOverlayMode::Command, window, cx);
            }
            return;
        }
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;

        // thread.jump.N → the matching positional item.
        if modifiers.platform
            && !modifiers.shift
            && !modifiers.alt
            && let Some(index) = key.parse::<usize>().ok().filter(|n| (1..=9).contains(n))
        {
            cx.stop_propagation();
            let command = command_palette_logic::THREAD_JUMP_COMMANDS[index - 1];
            let item = self
                .displayed_groups
                .iter()
                .flat_map(|group| &group.items)
                .find(|item| item.shortcut_command.as_deref() == Some(command))
                .cloned();
            if let Some(item) = item {
                self.execute_item(&item, window, cx);
            }
            return;
        }
        // thread.copyReference (⇧⌘C).
        if modifiers.platform && modifiers.shift && key == "c" {
            let has_target = self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.reference_copy_target.is_some());
            if has_target {
                cx.stop_propagation();
                self.close(window, cx);
                self.copy_thread_reference(window, cx);
                return;
            }
        }
        // chat.find (⌘F): ChatView ignores shortcuts while the palette is open.
        if modifiers.platform
            && !modifiers.shift
            && key == "f"
            && self.context.active_thread_key.is_some()
        {
            cx.stop_propagation();
            self.close(window, cx);
            cx.emit(CommandPaletteEvent::FindInCurrentThread);
            return;
        }

        match key {
            "escape" => {
                cx.stop_propagation();
                self.close(window, cx);
            }
            "enter" => {
                let composing = false;
                if composing {
                    return;
                }
                cx.stop_propagation();
                self.on_enter(&modifiers, window, cx);
            }
            "backspace" if self.query.is_empty() && self.current_view().is_some() => {
                cx.stop_propagation();
                self.pop_view(window, cx);
            }
            "down"
                if !modifiers.control
                    && !modifiers.alt
                    && !modifiers.platform
                    && !modifiers.shift =>
            {
                cx.stop_propagation();
                self.move_highlight(1);
                cx.notify();
            }
            "up" if !modifiers.control
                && !modifiers.alt
                && !modifiers.platform
                && !modifiers.shift =>
            {
                cx.stop_propagation();
                self.move_highlight(-1);
                cx.notify();
            }
            _ => {}
        }
    }

    pub(crate) fn on_enter(
        &mut self,
        modifiers: &Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let has_highlighted_browse_item = self
            .highlighted
            .as_deref()
            .is_some_and(|value| value.starts_with("browse:"));
        let primary = Self::primary_modifier(modifiers);

        // Enter creates the project unless an item below the name is
        // highlighted, in which case it runs that item. Mod+Enter always creates.
        if self.new_project_flow.is_some() && (self.highlighted.is_none() || primary) {
            self.submit_new_project(window, cx);
            return;
        }

        let path = self.browse_path();
        let can_submit_browse_path = path.is_browsing && !self.relative_path_needs_active_project();
        if can_submit_browse_path && (!has_highlighted_browse_item || primary) {
            let resolved = self.resolved_add_project_path();
            self.handle_add_project(resolved, window, cx);
            return;
        }

        // Run the tracked highlight directly: the virtualized list may have
        // unmounted its row.
        let value = self.effective_highlight();
        if let Some(item) =
            find_highlighted_command_palette_item(&self.displayed_groups, value.as_deref()).cloned()
        {
            self.execute_item(&item, window, cx);
        }
    }

    /// `resolvedAddProjectPath`: with a trailing separator the browsed
    /// directory itself, otherwise the exact entry's full path or the query.
    pub(crate) fn resolved_add_project_path(&self) -> String {
        if has_trailing_path_separator(&self.query) {
            return self
                .browse
                .result
                .as_ref()
                .map(|result| result.parent_path.as_str().to_owned())
                .unwrap_or_else(|| js_trim(&self.query).to_owned());
        }
        let (_, exact) = self.visible_browse_entries();
        exact
            .map(|entry| entry.full_path.as_str().to_owned())
            .unwrap_or_else(|| js_trim(&self.query).to_owned())
    }

    /// Whether Enter would create a missing folder (`willCreateProjectPath`).
    pub(crate) fn will_create_project_path(&self) -> bool {
        let path = self.browse_path();
        let has_highlighted_browse_item = self
            .highlighted
            .as_deref()
            .is_some_and(|value| value.starts_with("browse:"));
        path.is_browsing
            && !self.relative_path_needs_active_project()
            && !self.browse.pending
            && self.browse.error.is_none()
            && !self.query.trim().is_empty()
            && !has_highlighted_browse_item
            && if has_trailing_path_separator(&self.query) {
                self.browse.result.is_none()
            } else {
                self.visible_browse_entries().1.is_none()
            }
    }

    pub(crate) fn appearance_choice_name(mode: AppearanceModeChoice) -> &'static str {
        match mode {
            AppearanceModeChoice::System => "System",
            AppearanceModeChoice::Light => "Light",
            AppearanceModeChoice::Dark => "Dark",
        }
    }

    /// `appearance.cycle`: system → light → dark → system, with a toast.
    pub fn cycle_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = theme_actions::current_appearance_mode(cx);
        let next = match current {
            AppearanceModeChoice::System => AppearanceModeChoice::Light,
            AppearanceModeChoice::Light => AppearanceModeChoice::Dark,
            AppearanceModeChoice::Dark => AppearanceModeChoice::System,
        };
        theme_actions::apply_appearance_mode(next, cx);
        window.push_notification(
            Notification::info(format!(
                "Appearance: {}",
                Self::appearance_choice_name(next)
            )),
            cx,
        );
    }

    /// Normalized form of the query, for callers that match against it.
    pub fn normalized_query(&self) -> String {
        normalize_search_text(&self.query)
    }
}

impl Render for CommandPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.ui.open {
            return div().into_any_element();
        }
        self.render_overlay(window, cx)
    }
}

/// An unused handle so `Rc` stays imported for backend construction sites.
pub fn share_backend<B: palette_source::PaletteBackend + 'static>(
    backend: B,
) -> SharedPaletteBackend {
    Rc::new(backend)
}
