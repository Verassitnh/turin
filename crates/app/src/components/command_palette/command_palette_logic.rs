//! Port of upstream/apps/web/src/components/CommandPalette.logic.ts
//!
//! Pure state and list-building for the command palette: the overlay-mode
//! reducer, the item/group/row model, thread and project item builders, the
//! search filter and its ranking, and the browse (add project) groups.
//!
//! Deviations (see PORTING.md): upstream items carry `ReactNode` icons and
//! `run: () => Promise<void>` closures. Here an item carries data only: a
//! [`PaletteIcon`], [`PaletteTitle`]/[`PaletteDescription`]/[`PaletteAddon`]
//! descriptions the view renders, and a [`PaletteAction`] the palette executes
//! (`run` / `runThread` / `runProject` / `browseTo` / `browseUp` become
//! actions), so items are `Clone + PartialEq` and the builders are testable
//! without a window. `formatRelativeTimeLabel` reads the clock, so the builders
//! take `now_ms`.
// A faithful port keeps upstream's whole API (rule 3); the view wires the
// rest as the palette and toolbar land.
#![allow(dead_code)]

use std::collections::HashMap;

use turin_contracts::filesystem::FilesystemBrowseEntry;
use turin_contracts::pull_request::PullRequestLinkedThreadsEntry;
use turin_contracts::{EnvironmentId, ThreadId};

use crate::components::sidebar::runtime::thread_sort::{
    SortableThread, ThreadSortOrder, get_thread_sort_timestamp, sort_threads,
};
use crate::components::thread_search_match::SearchMatchSource;
use crate::state::helpers::filesystem::{FilteredBrowseEntries, filter_filesystem_browse_entries};
use crate::state::models::EnvironmentThreadShell;
use crate::timestamp_format::format_relative_time_label;
use crate::ui::icon::IconName;
use crate::utils::normalize_search_text;

pub const RECENT_THREAD_LIMIT: usize = 12;

/// Which palette item a keyboard shortcut label comes from
/// (`THREAD_JUMP_KEYBINDING_COMMANDS`).
pub const THREAD_JUMP_COMMANDS: &[&str] =
    turin_contracts::keybindings::THREAD_JUMP_KEYBINDING_COMMANDS;

/// What an item draws in its icon slot (`ITEM_ICON_CLASS`: `size-4
/// text-icon-muted`, or the project favicon).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteIcon {
    /// Upstream tests pass `icon: null`.
    None,
    Named(IconName),
    /// A project's favicon, by `environmentId:projectId`.
    ProjectFavicon,
}

/// `title: ReactNode`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteTitle {
    Text(String),
    /// `New thread in <span className="font-semibold">{project}</span>`.
    NewThreadIn {
        project: String,
    },
}

impl PaletteTitle {
    pub fn plain(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::NewThreadIn { project } => format!("New thread in {project}"),
        }
    }
}

impl From<&str> for PaletteTitle {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl From<String> for PaletteTitle {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

/// The inputs of `ThreadCommandSubtitle`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadSubtitle {
    /// The project whose favicon leads the row.
    pub project_key: Option<String>,
    pub project_title: Option<String>,
    pub environment_label: Option<String>,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub is_current: bool,
    pub driver_kind: Option<crate::components::sidebar::view_model::ProviderDriverKind>,
    pub provider_display_name: Option<String>,
}

/// `description: ReactNode`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteDescription {
    Text(String),
    Thread(ThreadSubtitle),
    /// `ProjectSearchDescription` for an ungrouped project: location · path.
    ProjectLocation {
        location_label: String,
        workspace_root: String,
    },
    /// `ProjectSearchDescription` for a grouped project: the environments.
    ProjectEnvironments(String),
}

impl PaletteDescription {
    pub fn text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Thread(subtitle) => [subtitle.project_title.clone(), subtitle.branch.clone()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · "),
            Self::ProjectLocation {
                location_label,
                workspace_root,
            } => format!("{location_label} · {workspace_root}"),
            Self::ProjectEnvironments(labels) => labels.clone(),
        }
    }
}

impl From<&str> for PaletteDescription {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl From<String> for PaletteDescription {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

/// `titleLeadingContent` / `titleTrailingContent`.
#[derive(Clone, Debug, PartialEq)]
pub enum PaletteAddon {
    /// `<span className="text-xs text-muted-foreground/70">Current</span>`.
    Current,
    /// `ThreadRowLeadingStatus`, by scoped thread key.
    ThreadLeadingStatus { thread_key: String },
    /// `ThreadSearchPullRequestNumber` + `ThreadRowTrailingStatus`.
    ThreadTrailing {
        thread_key: String,
        query: String,
        settled: bool,
    },
    /// `ChevronRightIcon` in the trailing slot.
    ChevronRight,
    /// `CheckIcon` in the trailing slot.
    Check,
    /// Theme preview circles after an optional "Current" label.
    ThemePreview {
        current: bool,
        previews: Vec<ThemePreviewSpec>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThemePreviewSpec {
    pub dark: bool,
    /// `ThemePreviewColors` as CSS color strings.
    pub canvas: String,
    pub accent: String,
    pub message_action: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandPaletteThreadContentMatch {
    pub source: SearchMatchSource,
    pub snippet: String,
    pub query: String,
}

/// A palette item's `run`: executed by the palette, which owns the services.
#[derive(Clone, Debug, PartialEq)]
pub enum PaletteAction {
    /// `run: async () => {}` of a disabled row.
    Noop,
    OpenThread {
        environment_id: EnvironmentId,
        thread_id: ThreadId,
    },
    /// `openProjectFromSearch`: the project's latest thread, else a new one.
    OpenProject {
        project_key: String,
    },
    /// `handleNewThread(projectRef)` for a project picked in "New thread in…".
    NewThreadInProject {
        project_key: String,
    },
    /// `startNewThreadFromContext` (`chat.newLocal`).
    NewThreadFromContext,
    /// `startScratchThread`.
    NewThreadWithoutProject,
    FindInCurrentThread,
    CopyThreadReference,
    LinkPullRequest,
    ShowLinkedPullRequests,
    RestartAgentSession,
    OpenFilePicker,
    SearchProjectContents,
    NewProject,
    AddProject,
    /// The Add project "Local folder" source.
    AddProjectLocalFolder,
    /// The New project name step's "Add existing project" row.
    NewProjectExisting,
    /// The "Create private repository" toggle.
    ToggleNewProjectPublish,
    SetTheme {
        id: Option<String>,
    },
    SetAppearance {
        mode: AppearanceModeChoice,
    },
    ToggleThemeEditor,
    OpenPullRequests,
    OpenUsage,
    OpenSettings,
    OpenProjectSettings {
        project_key: String,
    },
    /// A Settings search hit (`navigate({ to, hash })`).
    OpenSetting {
        id: String,
    },
    BrowseUp,
    BrowseTo {
        name: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppearanceModeChoice {
    System,
    Light,
    Dark,
}

/// `CommandPaletteActionItem | CommandPaletteSubmenuItem`.
#[derive(Clone, Debug, PartialEq)]
pub enum PaletteItemKind {
    Action {
        keep_open: bool,
        run: PaletteAction,
    },
    Submenu {
        addon_icon: PaletteIcon,
        groups: Vec<CommandPaletteGroup>,
        initial_query: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommandPaletteItem {
    pub kind: PaletteItemKind,
    pub value: String,
    pub search_terms: Vec<String>,
    pub title: PaletteTitle,
    pub description: Option<PaletteDescription>,
    pub thread_content_match: Option<CommandPaletteThreadContentMatch>,
    pub timestamp: Option<String>,
    pub search_recency: Option<f64>,
    pub icon: PaletteIcon,
    pub disabled: bool,
    /// Optional content rendered inline before the title text.
    pub title_leading_content: Option<PaletteAddon>,
    /// Optional content rendered inline after the title text (before the timestamp).
    pub title_trailing_content: Vec<PaletteAddon>,
    pub shortcut_command: Option<String>,
    /// Sorts after every other match in its group; see `SettingsSearchItem.secondary`.
    pub secondary: bool,
}

impl CommandPaletteItem {
    /// An action item with every optional field empty.
    pub fn action(
        value: impl Into<String>,
        title: impl Into<PaletteTitle>,
        icon: PaletteIcon,
        run: PaletteAction,
    ) -> Self {
        Self {
            kind: PaletteItemKind::Action {
                keep_open: false,
                run,
            },
            value: value.into(),
            search_terms: Vec::new(),
            title: title.into(),
            description: None,
            thread_content_match: None,
            timestamp: None,
            search_recency: None,
            icon,
            disabled: false,
            title_leading_content: None,
            title_trailing_content: Vec::new(),
            shortcut_command: None,
            secondary: false,
        }
    }

    pub fn submenu(
        value: impl Into<String>,
        title: impl Into<PaletteTitle>,
        icon: PaletteIcon,
        addon_icon: PaletteIcon,
        groups: Vec<CommandPaletteGroup>,
    ) -> Self {
        Self {
            kind: PaletteItemKind::Submenu {
                addon_icon,
                groups,
                initial_query: None,
            },
            ..Self::action(value, title, icon, PaletteAction::Noop)
        }
    }

    pub fn search_terms<I, S>(mut self, terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.search_terms = terms.into_iter().map(Into::into).collect();
        self
    }

    pub fn description(mut self, description: impl Into<PaletteDescription>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn keep_open(mut self) -> Self {
        if let PaletteItemKind::Action { keep_open, .. } = &mut self.kind {
            *keep_open = true;
        }
        self
    }

    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }

    pub fn shortcut(mut self, command: &str) -> Self {
        self.shortcut_command = Some(command.to_owned());
        self
    }

    pub fn trailing(mut self, addon: PaletteAddon) -> Self {
        self.title_trailing_content.push(addon);
        self
    }

    pub fn is_submenu(&self) -> bool {
        matches!(self.kind, PaletteItemKind::Submenu { .. })
    }

    /// The action this item runs, `None` for a submenu.
    pub fn run(&self) -> Option<&PaletteAction> {
        match &self.kind {
            PaletteItemKind::Action { run, .. } => Some(run),
            PaletteItemKind::Submenu { .. } => None,
        }
    }

    pub fn keeps_open(&self) -> bool {
        matches!(
            self.kind,
            PaletteItemKind::Action {
                keep_open: true,
                ..
            }
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommandPaletteGroup {
    pub value: String,
    pub label: String,
    pub items: Vec<CommandPaletteItem>,
}

impl CommandPaletteGroup {
    pub fn new(
        value: impl Into<String>,
        label: impl Into<String>,
        items: Vec<CommandPaletteItem>,
    ) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            items,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommandPaletteView {
    pub addon_icon: PaletteIcon,
    pub groups: Vec<CommandPaletteGroup>,
    pub initial_query: Option<String>,
}

/// `commandPaletteBus.CommandPaletteLinkedThreads`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandPaletteLinkedThreads {
    pub environment_id: EnvironmentId,
    pub threads: Vec<PullRequestLinkedThreadsEntry>,
}

/// A PR's relations include archived threads that normal palette search omits.
pub fn build_linked_thread_action_items(
    linked: &CommandPaletteLinkedThreads,
    query: &str,
    icon: PaletteIcon,
) -> Vec<CommandPaletteItem> {
    linked
        .threads
        .iter()
        .map(|thread| {
            let title = if thread.title.is_empty() {
                "Untitled thread".to_owned()
            } else {
                thread.title.clone()
            };
            let mut item = CommandPaletteItem::action(
                format!("thread:{}:{}", linked.environment_id, thread.id),
                title,
                icon,
                PaletteAction::OpenThread {
                    environment_id: linked.environment_id.clone(),
                    thread_id: thread.id.clone(),
                },
            )
            .description(if thread.archived_at.is_none() {
                "Linked thread"
            } else {
                "Archived thread"
            });
            item.search_terms = vec![query.to_owned(), thread.title.clone()];
            item
        })
        .collect()
}

/// `browseInputEndPaddingClass` as the padding (rems) the input reserves at
/// its end for the submit button: `pe-38` / `pe-30` / `pe-24`.
pub fn browse_input_end_padding(
    will_create_project_path: bool,
    has_highlighted_browse_item: bool,
) -> f32 {
    if will_create_project_path {
        9.5
    } else if has_highlighted_browse_item {
        7.5
    } else {
        6.
    }
}

/// The global search overlay hosts three mutually exclusive surfaces: the
/// command palette (⌘K), the project file picker (⌘P), and project content
/// search (⇧⌘F). One reducer owns open/mode state so the surfaces can never
/// stack and re-triggering a mode's shortcut toggles it closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchOverlayMode {
    Command,
    Files,
    Content,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandPaletteOpenIntent {
    AddProject,
    NewThreadIn,
    ChangeTheme,
    Search {
        query: String,
        linked_threads: Option<CommandPaletteLinkedThreads>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandPaletteUiState {
    pub open: bool,
    pub mode: SearchOverlayMode,
    pub open_intent: Option<CommandPaletteOpenIntent>,
}

impl Default for CommandPaletteUiState {
    fn default() -> Self {
        Self {
            open: false,
            mode: SearchOverlayMode::Command,
            open_intent: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandPaletteUiAction {
    SetOpen(bool),
    ToggleMode(SearchOverlayMode),
    OpenSearch {
        query: String,
        linked_threads: Option<CommandPaletteLinkedThreads>,
    },
    OpenAddProject,
    OpenNewThreadIn,
    OpenChangeTheme,
    ClearOpenIntent,
}

pub fn reduce_command_palette_ui_state(
    state: &CommandPaletteUiState,
    action: CommandPaletteUiAction,
) -> CommandPaletteUiState {
    let command_with = |intent| CommandPaletteUiState {
        open: true,
        mode: SearchOverlayMode::Command,
        open_intent: Some(intent),
    };
    match action {
        CommandPaletteUiAction::SetOpen(true) => CommandPaletteUiState {
            open: true,
            mode: SearchOverlayMode::Command,
            open_intent: state.open_intent.clone(),
        },
        CommandPaletteUiAction::SetOpen(false) => CommandPaletteUiState {
            open: false,
            open_intent: None,
            ..state.clone()
        },
        CommandPaletteUiAction::ToggleMode(mode) => {
            if state.open && state.mode == mode {
                CommandPaletteUiState {
                    open: false,
                    open_intent: None,
                    ..state.clone()
                }
            } else {
                CommandPaletteUiState {
                    open: true,
                    mode,
                    open_intent: None,
                }
            }
        }
        CommandPaletteUiAction::OpenSearch {
            query,
            linked_threads,
        } => command_with(CommandPaletteOpenIntent::Search {
            query,
            linked_threads,
        }),
        CommandPaletteUiAction::OpenAddProject => {
            command_with(CommandPaletteOpenIntent::AddProject)
        }
        CommandPaletteUiAction::OpenNewThreadIn => {
            command_with(CommandPaletteOpenIntent::NewThreadIn)
        }
        CommandPaletteUiAction::OpenChangeTheme => {
            command_with(CommandPaletteOpenIntent::ChangeTheme)
        }
        CommandPaletteUiAction::ClearOpenIntent => {
            if state.open_intent.is_some() {
                CommandPaletteUiState {
                    open_intent: None,
                    ..state.clone()
                }
            } else {
                state.clone()
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum CommandPaletteRow {
    Label {
        key: String,
        label: String,
        first: bool,
    },
    Item {
        key: String,
        item: CommandPaletteItem,
        /// Position among enabled items, or `None` for disabled rows the
        /// keyboard skips.
        item_index: Option<usize>,
    },
}

impl CommandPaletteRow {
    pub fn key(&self) -> &str {
        match self {
            Self::Label { key, .. } | Self::Item { key, .. } => key,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CommandPaletteRows {
    pub rows: Vec<CommandPaletteRow>,
    /// The highlightable item order the keyboard navigates.
    pub item_values: Vec<String>,
    /// Maps a highlight back to its row for scrolling.
    pub row_index_by_item_index: Vec<usize>,
}

/// Flattens groups into the rows a virtualized list renders.
pub fn build_command_palette_rows(groups: &[CommandPaletteGroup]) -> CommandPaletteRows {
    let mut out = CommandPaletteRows::default();
    for group in groups {
        if !group.label.is_empty() {
            out.rows.push(CommandPaletteRow::Label {
                key: format!("group:{}", group.value),
                label: group.label.clone(),
                first: out.rows.is_empty(),
            });
        }
        for item in &group.items {
            let item_index = if item.disabled {
                None
            } else {
                Some(out.item_values.len())
            };
            if item_index.is_some() {
                out.item_values.push(item.value.clone());
                out.row_index_by_item_index.push(out.rows.len());
            }
            out.rows.push(CommandPaletteRow::Item {
                key: format!("{}:{}", group.value, item.value),
                item: item.clone(),
                item_index,
            });
        }
    }
    out
}

/// The enabled item Enter should run for a highlight, whether or not its row is mounted.
pub fn find_highlighted_command_palette_item<'a>(
    groups: &'a [CommandPaletteGroup],
    highlighted_item_value: Option<&str>,
) -> Option<&'a CommandPaletteItem> {
    let value = highlighted_item_value?;
    groups
        .iter()
        .flat_map(|group| &group.items)
        .find(|item| item.value == value && !item.disabled)
}

/// Keyboard highlight position, including Base UI's hidden first-row highlight.
pub fn resolve_command_palette_highlight_index(
    item_count: usize,
    highlighted: Option<usize>,
    auto_highlights_first_row: bool,
    highlight_cleared: bool,
) -> Option<usize> {
    if item_count == 0 {
        return None;
    }
    if let Some(index) = highlighted {
        return Some(index);
    }
    if auto_highlights_first_row && !highlight_cleared {
        Some(0)
    } else {
        None
    }
}

/// `CommandPalette.tsx` ArrowUp/ArrowDown over the flat item list.
pub fn move_command_palette_highlight_index(
    item_count: usize,
    highlighted: Option<usize>,
    auto_highlights_first_row: bool,
    highlight_cleared: bool,
    delta: isize,
) -> usize {
    let current = resolve_command_palette_highlight_index(
        item_count,
        highlighted,
        auto_highlights_first_row,
        highlight_cleared,
    );
    match (current, highlight_cleared, delta) {
        (None, true, d) if d > 0 => 0,
        (None, _, d) if d > 0 => 0,
        (None, _, _) => item_count.saturating_sub(1),
        (Some(index), _, d) => (index as isize + d).rem_euclid(item_count as isize) as usize,
    }
}

/// Assigns positional jump shortcuts to the first nine displayed items.
pub fn enumerate_command_palette_items(items: Vec<CommandPaletteItem>) -> Vec<CommandPaletteItem> {
    items
        .into_iter()
        .enumerate()
        .map(|(index, mut item)| {
            item.shortcut_command = THREAD_JUMP_COMMANDS.get(index).map(|c| (*c).to_owned());
            item
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandPaletteMode {
    Root,
    RootBrowse,
    Submenu,
    SubmenuBrowse,
}

/// A project as the palette shows it. `display_name` is the grouped label (for
/// example "owner/repo" when projects are merged across machines). `title`
/// stays the real project title: the automatic project icon is derived from
/// it, and every other surface uses the real title.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandPaletteProject {
    pub environment_id: EnvironmentId,
    pub id: String,
    pub title: String,
    pub workspace_root: String,
    pub display_name: String,
}

impl CommandPaletteProject {
    pub fn key(&self) -> String {
        format!("{}:{}", self.environment_id, self.id)
    }
}

/// The project fields `buildCommandPaletteProjectMetadata` reads.
pub struct ProjectMetadataInput<'a> {
    pub environment_id: &'a EnvironmentId,
    pub title: &'a str,
    pub workspace_root: &'a str,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandPaletteProjectMetadata {
    pub search_terms: Vec<String>,
    pub environment_labels: Vec<String>,
}

pub fn build_command_palette_project_metadata(
    projects: &[ProjectMetadataInput<'_>],
    location_label_by_environment_id: &HashMap<EnvironmentId, String>,
) -> CommandPaletteProjectMetadata {
    let mut search_terms = Vec::new();
    let mut environment_labels: Vec<String> = Vec::new();
    for project in projects {
        let label = location_label_by_environment_id
            .get(project.environment_id)
            .map_or("Remote", String::as_str);
        search_terms.push(project.title.to_owned());
        search_terms.push(project.workspace_root.to_owned());
        search_terms.push(label.to_owned());
        if !environment_labels.iter().any(|existing| existing == label) {
            environment_labels.push(label.to_owned());
        }
    }
    CommandPaletteProjectMetadata {
        search_terms,
        environment_labels,
    }
}

#[allow(clippy::type_complexity)]
pub struct BuildProjectActionItemsInput<'a> {
    pub projects: &'a [CommandPaletteProject],
    pub value_prefix: &'a str,
    pub icon: &'a dyn Fn(&CommandPaletteProject) -> PaletteIcon,
    pub run_project: &'a dyn Fn(&CommandPaletteProject) -> PaletteAction,
    pub search_terms: Option<&'a dyn Fn(&CommandPaletteProject) -> Vec<String>>,
    pub render_description: Option<&'a dyn Fn(&CommandPaletteProject) -> PaletteDescription>,
    pub shortcut_command: Option<&'a str>,
}

pub fn build_project_action_items(
    input: &BuildProjectActionItemsInput<'_>,
) -> Vec<CommandPaletteItem> {
    input
        .projects
        .iter()
        .map(|project| {
            let mut item = CommandPaletteItem::action(
                format!(
                    "{}:{}:{}",
                    input.value_prefix, project.environment_id, project.id
                ),
                project.display_name.clone(),
                (input.icon)(project),
                (input.run_project)(project),
            );
            item.search_terms = vec![
                project.display_name.clone(),
                project.title.clone(),
                project.workspace_root.clone(),
            ];
            if let Some(extra) = input.search_terms {
                item.search_terms.extend(extra(project));
            }
            item.description = Some(
                input
                    .render_description
                    .map(|render| render(project))
                    .unwrap_or_else(|| PaletteDescription::Text(project.workspace_root.clone())),
            );
            item.shortcut_command = input.shortcut_command.map(str::to_owned);
            item
        })
        .collect()
}

/// `BuildThreadActionItemsThread`: the thread fields the builder reads.
pub trait PaletteThread: SortableThread + Clone {
    fn archived_at(&self) -> Option<&str>;
    fn project_id(&self) -> &str;
    fn thread_environment_id(&self) -> &EnvironmentId;
    fn thread_id(&self) -> &ThreadId;
    fn title(&self) -> &str;
    fn branch(&self) -> Option<&str>;
    /// `threadPullRequestSearchTerms(thread)`.
    fn pull_request_search_terms(&self) -> Vec<String>;
}

impl PaletteThread for EnvironmentThreadShell {
    fn archived_at(&self) -> Option<&str> {
        self.archived_at.as_deref()
    }
    fn project_id(&self) -> &str {
        self.project_id.as_str()
    }
    fn thread_environment_id(&self) -> &EnvironmentId {
        &self.environment_id
    }
    fn thread_id(&self) -> &ThreadId {
        &self.id
    }
    fn title(&self) -> &str {
        &self.title
    }
    fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }
    fn pull_request_search_terms(&self) -> Vec<String> {
        crate::components::sidebar::thread_status_indicators::link_search_terms(&self.pull_requests)
    }
}

#[allow(clippy::type_complexity)]
pub struct BuildThreadActionItemsInput<'a, T: PaletteThread> {
    pub threads: &'a [T],
    pub active_thread_id: Option<&'a ThreadId>,
    /// Keyed by project id.
    pub project_title_by_id: &'a HashMap<String, String>,
    pub sort_order: ThreadSortOrder,
    pub icon: PaletteIcon,
    /// Optional content rendered inline before the title text per thread.
    pub render_leading_content: Option<&'a dyn Fn(&T) -> Option<PaletteAddon>>,
    /// Optional content rendered inline after the title text per thread.
    pub render_trailing_content: Option<&'a dyn Fn(&T) -> Vec<PaletteAddon>>,
    /// Optional rich description (e.g. favicon + workspace icons). Falls back to text.
    pub render_description: Option<&'a dyn Fn(&T, Option<&str>) -> PaletteDescription>,
    pub get_content_match: Option<&'a dyn Fn(&T) -> Option<CommandPaletteThreadContentMatch>>,
    pub limit: Option<usize>,
    pub now_ms: i64,
}

pub fn build_thread_action_items<T: PaletteThread>(
    input: &BuildThreadActionItemsInput<'_, T>,
) -> Vec<CommandPaletteItem> {
    let live: Vec<T> = input
        .threads
        .iter()
        .filter(|thread| thread.archived_at().is_none())
        .cloned()
        .collect();
    let mut sorted = sort_threads(&live, input.sort_order);
    if let Some(limit) = input.limit {
        sorted.truncate(limit);
    }

    sorted
        .iter()
        .map(|thread| {
            let project_title = input
                .project_title_by_id
                .get(thread.project_id())
                .map(String::as_str);
            let is_active = input
                .active_thread_id
                .is_some_and(|active| active == thread.thread_id());
            let content_match = input.get_content_match.and_then(|get| get(thread));

            let description = match input.render_description {
                Some(render) => render(thread, project_title),
                None => {
                    let mut parts: Vec<String> = Vec::new();
                    if let Some(title) = project_title.filter(|title| !title.is_empty()) {
                        parts.push(title.to_owned());
                    }
                    if let Some(branch) = thread.branch().filter(|branch| !branch.is_empty()) {
                        parts.push(format!("#{branch}"));
                    }
                    if is_active {
                        parts.push("Current thread".to_owned());
                    }
                    PaletteDescription::Text(parts.join(" · "))
                }
            };

            let mut search_terms = vec![thread.title().to_owned()];
            search_terms.extend(thread.pull_request_search_terms());
            search_terms.push(project_title.unwrap_or("").to_owned());
            search_terms.push(thread.branch().unwrap_or("").to_owned());
            search_terms.push(
                content_match
                    .as_ref()
                    .map(|m| m.snippet.clone())
                    .unwrap_or_default(),
            );
            // Last so pasted IDs never outrank title matches for shared substrings.
            search_terms.push(thread.thread_id().as_str().to_owned());

            let timestamp_source = thread
                .latest_user_message_at()
                .unwrap_or_else(|| thread.updated_at());
            let mut item = CommandPaletteItem::action(
                format!("thread:{}", thread.thread_id()),
                thread.title().to_owned(),
                input.icon,
                PaletteAction::OpenThread {
                    environment_id: thread.thread_environment_id().clone(),
                    thread_id: thread.thread_id().clone(),
                },
            );
            item.search_terms = search_terms;
            item.description = Some(description);
            item.timestamp = Some(format_relative_time_label(timestamp_source, input.now_ms));
            item.search_recency = Some(get_thread_sort_timestamp(
                thread,
                ThreadSortOrder::UpdatedAt,
            ));
            item.title_leading_content = input
                .render_leading_content
                .and_then(|render| render(thread));
            item.title_trailing_content = input
                .render_trailing_content
                .map(|render| render(thread))
                .unwrap_or_default();
            item.thread_content_match = content_match;
            item
        })
        .collect()
}

/// `rankSearchFieldMatch`. `f64::NEG_INFINITY` is "no match".
fn rank_search_field_match(field: &str, normalized_query: &str, query_tokens: &[&str]) -> f64 {
    let normalized_field = normalize_search_text(field);
    if normalized_field.is_empty()
        || !query_tokens
            .iter()
            .all(|token| normalized_field.contains(token))
    {
        return f64::NEG_INFINITY;
    }
    if normalized_field == normalized_query {
        return 3.;
    }
    if normalized_field.starts_with(normalized_query) {
        return 2.;
    }
    if normalized_field.contains(normalized_query) {
        return 1.;
    }
    0.
}

fn rank_command_palette_item_match(
    item: &CommandPaletteItem,
    normalized_query: &str,
    query_tokens: &[&str],
) -> f64 {
    let terms: Vec<&String> = item
        .search_terms
        .iter()
        .filter(|term| !term.is_empty())
        .collect();
    if terms.is_empty() {
        return 0.;
    }
    for (index, field) in terms.iter().enumerate() {
        let field_rank = rank_search_field_match(field, normalized_query, query_tokens);
        if field_rank != f64::NEG_INFINITY {
            if index == 0 && item.search_recency.is_some() {
                // All non-exact thread title matches share a tier so recency breaks the tie.
                return 1_000. + if field_rank == 3. { 1. } else { 0. };
            }
            return 1_000. - index as f64 * 100. + field_rank;
        }
    }
    0.
}

pub struct FilterCommandPaletteGroupsInput<'a> {
    pub active_groups: &'a [CommandPaletteGroup],
    pub query: &'a str,
    pub is_in_submenu: bool,
    pub project_search_items: &'a [CommandPaletteItem],
    pub settings_search_items: &'a [CommandPaletteItem],
    pub thread_search_items: &'a [CommandPaletteItem],
}

/// `(right - left)` as a comparator that ignores `NaN`, as JS's `||` chain does.
fn descending(left: f64, right: f64) -> std::cmp::Ordering {
    let diff = right - left;
    if diff.is_nan() || diff == 0. {
        std::cmp::Ordering::Equal
    } else if diff > 0. {
        std::cmp::Ordering::Greater
    } else {
        std::cmp::Ordering::Less
    }
}

pub fn filter_command_palette_groups(
    input: &FilterCommandPaletteGroupsInput<'_>,
) -> Vec<CommandPaletteGroup> {
    let is_actions_filter = input.query.starts_with('>');
    let search_query = if is_actions_filter {
        &input.query[1..]
    } else {
        input.query
    };
    let normalized_query = normalize_search_text(search_query);

    if normalized_query.is_empty() {
        if is_actions_filter {
            return input
                .active_groups
                .iter()
                .filter(|group| group.value == "actions")
                .cloned()
                .collect();
        }
        return input.active_groups.to_vec();
    }
    let query_tokens: Vec<&str> = normalized_query.split(' ').collect();

    let mut base_groups: Vec<CommandPaletteGroup> = input.active_groups.to_vec();
    if is_actions_filter {
        base_groups.retain(|group| group.value == "actions");
    } else if !input.is_in_submenu {
        base_groups.retain(|group| group.value != "recent-threads");
    }

    let mut searchable_groups = base_groups;
    if !input.is_in_submenu && !is_actions_filter {
        if !input.project_search_items.is_empty() {
            searchable_groups.push(CommandPaletteGroup::new(
                "projects-search",
                "Projects",
                input.project_search_items.to_vec(),
            ));
        }
        if !input.settings_search_items.is_empty() {
            searchable_groups.push(CommandPaletteGroup::new(
                "settings-search",
                "Settings",
                input.settings_search_items.to_vec(),
            ));
        }
        if !input.thread_search_items.is_empty() {
            searchable_groups.push(CommandPaletteGroup::new(
                "threads-search",
                "Threads",
                input.thread_search_items.to_vec(),
            ));
        }
    }

    searchable_groups
        .into_iter()
        .filter_map(|group| {
            let mut entries: Vec<(usize, f64, &CommandPaletteItem)> = group
                .items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    let haystack = normalize_search_text(&item.search_terms.join(" "));
                    if !query_tokens.iter().all(|token| haystack.contains(token)) {
                        return None;
                    }
                    Some((
                        index,
                        rank_command_palette_item_match(item, &normalized_query, &query_tokens),
                        item,
                    ))
                })
                .collect();
            entries.sort_by(
                |(left_index, left_rank, left), (right_index, right_rank, right)| {
                    left.secondary
                        .cmp(&right.secondary)
                        .then_with(|| descending(*left_rank, *right_rank))
                        .then_with(|| {
                            descending(
                                left.search_recency.unwrap_or(0.),
                                right.search_recency.unwrap_or(0.),
                            )
                        })
                        .then_with(|| left_index.cmp(right_index))
                },
            );
            if entries.is_empty() {
                return None;
            }
            Some(CommandPaletteGroup {
                value: group.value.clone(),
                label: group.label.clone(),
                items: entries
                    .into_iter()
                    .map(|(_, _, item)| item.clone())
                    .collect(),
            })
        })
        .collect()
}

pub struct BuildBrowseGroupsInput<'a> {
    pub browse_entries: &'a [&'a FilesystemBrowseEntry],
    pub browse_query: &'a str,
    pub can_browse_up: bool,
    pub up_icon: PaletteIcon,
    pub directory_icon: PaletteIcon,
}

pub fn build_browse_groups(input: &BuildBrowseGroupsInput<'_>) -> Vec<CommandPaletteGroup> {
    let mut items = Vec::new();

    if input.can_browse_up {
        let mut item =
            CommandPaletteItem::action("browse:up", "..", input.up_icon, PaletteAction::BrowseUp)
                .keep_open();
        item.search_terms = vec![input.browse_query.to_owned(), "..".to_owned()];
        items.push(item);
    }

    for entry in input.browse_entries {
        let mut item = CommandPaletteItem::action(
            format!("browse:{}", entry.full_path),
            entry.name.as_str().to_owned(),
            input.directory_icon,
            PaletteAction::BrowseTo {
                name: entry.name.as_str().to_owned(),
            },
        )
        .keep_open();
        item.search_terms = vec![
            input.browse_query.to_owned(),
            entry.full_path.as_str().to_owned(),
            entry.name.as_str().to_owned(),
        ];
        items.push(item);
    }

    vec![CommandPaletteGroup::new(
        "directories",
        "Directories",
        items,
    )]
}

pub fn filter_pinned_browse_entries<'a>(
    browse_entries: &'a [FilesystemBrowseEntry],
    filter_query: &str,
    pinned_directory_name: &str,
    case_sensitive: bool,
) -> FilteredBrowseEntries<'a> {
    let names_match = |left: &str, right: &str| {
        if case_sensitive {
            left == right
        } else {
            left.to_lowercase() == right.to_lowercase()
        }
    };
    let visible_filter_query = if names_match(filter_query, pinned_directory_name) {
        ""
    } else {
        filter_query
    };
    let visible_entries =
        filter_filesystem_browse_entries(browse_entries, visible_filter_query).visible_entries;
    let exact_entry = if filter_query.is_empty() {
        None
    } else {
        browse_entries
            .iter()
            .find(|entry| names_match(entry.name.as_str(), filter_query))
    };
    FilteredBrowseEntries {
        visible_entries,
        exact_entry,
    }
}

pub fn get_command_palette_mode(
    current_view: Option<&CommandPaletteView>,
    is_browsing: bool,
) -> CommandPaletteMode {
    match (current_view.is_some(), is_browsing) {
        (true, true) => CommandPaletteMode::SubmenuBrowse,
        (true, false) => CommandPaletteMode::Submenu,
        (false, true) => CommandPaletteMode::RootBrowse,
        (false, false) => CommandPaletteMode::Root,
    }
}

pub fn build_root_groups(
    action_items: Vec<CommandPaletteItem>,
    recent_thread_items: Vec<CommandPaletteItem>,
) -> Vec<CommandPaletteGroup> {
    let mut groups = Vec::new();
    if !action_items.is_empty() {
        groups.push(CommandPaletteGroup::new("actions", "Actions", action_items));
    }
    if !recent_thread_items.is_empty() {
        groups.push(CommandPaletteGroup::new(
            "recent-threads",
            "Recent Threads",
            recent_thread_items,
        ));
    }
    groups
}

pub fn get_command_palette_input_placeholder(mode: CommandPaletteMode) -> &'static str {
    match mode {
        CommandPaletteMode::Root => "Search commands, projects, and threads...",
        CommandPaletteMode::RootBrowse => "Enter project path (e.g. ~/projects/my-app)",
        CommandPaletteMode::Submenu => "Search...",
        CommandPaletteMode::SubmenuBrowse => "Enter path (e.g. ~/projects/my-app)",
    }
}

#[cfg(test)]
mod tests;
