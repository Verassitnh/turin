//! Port of upstream/apps/web/src/components/DiffPanel.tsx (state and data
//! derivation; rendering is in `panel_view.rs`).
//!
//! `DiffPanel` is one entity: it reads from a [`DiffSource`], keeps the
//! panel-local state upstream holds in hooks (`wordWrap`,
//! `diffIgnoreWhitespace`, `fileTreeOpen`, collapsed files, the selected
//! lines and comment draft) and derives the flat list of rows the GPUI list
//! virtualizes.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui_kit::{App, Context, FocusHandle, ListAlignment, ListState, Subscription, Window, px};

use super::DiffPanelPersistedSnapshot;
use super::base_ref_choices::{BaseRefChoice, build_base_ref_choices, filter_base_ref_choices};
use super::diff_collapse::{are_all_diff_files_collapsed, toggle_all_diff_files};
use super::diff_file_actions::{
    DiffFileTargets, OpenDiffFilePrimaryActionInput, open_diff_file_primary_action,
};
use super::diff_file_contents::load_diff_file_contents as load_diff_files;
use super::diff_file_tree_logic::{DiffFileTreeEntry, diff_file_tree_entries};
use super::diff_panel_persistence::{decode_diff_panel_store_json, encode_diff_panel_store};
use super::diff_panel_store::{DiffPanelSelection, DiffPanelStore, GitScope, ScopedThreadRef};
use super::diff_rendering::{
    DiffLineStat, RenderablePatch, RenderablePatchOptions, build_file_diff_content_version,
    build_file_diff_identity_key, build_patch_cache_key, get_diff_line_stat, get_renderable_patch,
    resolve_file_diff_path,
};
use super::highlight::{FileHighlights, file_highlights};
use super::natural::natural_compare;
use super::pierre::hydrate_partial_diff::hydrate_partial_diff;
use super::pierre::iterate_over_diff::{DiffStyle, ExpandDirection, ExpandedHunks};
use super::pierre::types::{
    ChangeType, DiffLineAnnotation, FileDiffMetadata, SelectedLineRange, SelectionSide,
};
use super::review_comment_context::{
    BuildDiffReviewCommentInput, ReviewCommentContext, build_diff_review_comment,
    restore_diff_review_comment_range,
};
use super::rows::{DiffRow, LineKind, build_rows};
use super::view_model::{
    DiffContext, DiffSource, GitDiffFileStat, GitDiffPreview, GitDiffSource, GitSourceKind,
    Loadable,
};
use crate::components::chat::timeline::types::TurnDiffSummary;
use crate::theme::ActiveAppTheme;

/// `AUTOMATIC_BASE_REF`.
pub const AUTOMATIC_BASE_REF: &str = "__automatic_base_ref__";
/// `DiffLayout` of the client settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLayout {
    Stacked,
    Split,
}

impl DiffLayout {
    pub fn style(self) -> DiffStyle {
        match self {
            Self::Stacked => DiffStyle::Unified,
            Self::Split => DiffStyle::Split,
        }
    }
}

/// The client settings and `useLocalStorage` flags the panel reads, at their
/// upstream defaults (`diffFilesCollapsed: true`, `diffIgnoreWhitespace: true`,
/// `diffLayout: "stacked"`, `wordWrap: true`, `t3code.diffFileTreeOpen: false`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffPanelSettings {
    pub layout: DiffLayout,
    pub word_wrap: bool,
    pub ignore_whitespace: bool,
    pub files_collapsed: bool,
    pub file_tree_open: bool,
}

impl Default for DiffPanelSettings {
    fn default() -> Self {
        Self {
            layout: DiffLayout::Stacked,
            word_wrap: true,
            ignore_whitespace: true,
            files_collapsed: true,
            file_tree_open: false,
        }
    }
}

/// What the body shows when there is no file list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BodyState {
    NoThread,
    NotGit,
    NoTurns,
    /// `!canReadFiles`.
    NoFileAccess(String),
    Loading(&'static str),
    Message(&'static str),
    Raw {
        text: String,
        reason: String,
    },
    Files,
}

/// One file in the viewer (`codeViewFiles` entry).
#[derive(Clone)]
pub struct FileEntry {
    pub file: Arc<FileDiffMetadata>,
    /// `buildFileDiffIdentityKey`.
    pub key: String,
    pub path: String,
    pub version: u32,
    /// A header-only placeholder awaiting its patch (`cacheKey` ends `:pending`).
    pub pending: bool,
    /// Complete statistics of a lazily loaded file.
    pub stat: Option<GitDiffFileStat>,
    pub error: bool,
    pub truncated: bool,
}

/// What a file's rows depend on: split or unified, loader presence, content
/// version and the expanded regions.
type RowsKey = (bool, bool, u32, Vec<(usize, usize, usize)>);

/// A row of the virtualized list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlatItem {
    Header(usize),
    Row(usize, usize),
    /// The 8px under an expanded file (`itemMetrics.paddingBottom`).
    Gap(usize),
    /// Skeleton headers for files still loading (`DiffFileLoadingBoundary`).
    LoadingBoundary,
    /// Comment annotation (`renderAnnotation`), attached after a row.
    Annotation(usize, usize),
}

/// The scope the panel shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    Branch { base_ref: Option<String> },
    Unstaged,
    Turn { run_id: String },
}

/// A comment being written.
#[derive(Clone, Debug)]
pub struct CommentDraft {
    pub file_key: String,
    pub id: String,
    pub range: SelectedLineRange,
    pub range_label: String,
    pub annotation: DiffLineAnnotation<()>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineSelection {
    pub file_key: String,
    pub range: SelectedLineRange,
}

/// The open dropdown, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenMenu {
    Scope,
    ScopeTurns,
    BaseRef,
}

/// Panel-wide derived state, rebuilt by [`DiffPanel::reload`].
pub struct Derived {
    pub context: DiffContext,
    pub turns: Vec<TurnDiffSummary>,
    pub scope: Scope,
    pub selected_scope_label: String,
    pub review_section_id: String,
    pub review_section_title: String,
    pub collapse_scope_key: Option<String>,
    pub body: BodyState,
    pub files: Vec<FileEntry>,
    pub stat: DiffLineStat,
    pub has_stat: bool,
    pub banner: Option<String>,
    pub error: Option<String>,
    pub git_source: Option<GitDiffSource>,
    pub lazy: bool,
    pub settled_count: usize,
    pub refreshing: bool,
    pub can_refresh: bool,
    pub tree_entries: Vec<DiffFileTreeEntry>,
    pub selected_file_path: Option<String>,
    pub reveal_request_id: u64,
}

impl Derived {
    fn empty() -> Self {
        Self {
            context: DiffContext::default(),
            turns: Vec::new(),
            scope: Scope::Branch { base_ref: None },
            selected_scope_label: "Changes".into(),
            review_section_id: "branch".into(),
            review_section_title: "Changes".into(),
            collapse_scope_key: None,
            body: BodyState::NoThread,
            files: Vec::new(),
            stat: DiffLineStat::default(),
            has_stat: false,
            banner: None,
            error: None,
            git_source: None,
            lazy: false,
            settled_count: 0,
            refreshing: false,
            can_refresh: false,
            tree_entries: Vec::new(),
            selected_file_path: None,
            reveal_request_id: 0,
        }
    }

    /// `checkpointTurnCount ?? inferred`.
    pub fn turn_count(turn: &TurnDiffSummary) -> u64 {
        turn.checkpoint_turn_count
    }
}

pub struct DiffPanel {
    pub(super) source: Arc<dyn DiffSource>,
    pub store: DiffPanelStore,
    pub settings: DiffPanelSettings,
    pub(super) derived: Derived,
    /// `collapsedDiffFiles`: the scope it belongs to and its keys.
    pub(super) collapsed_scope: Option<String>,
    pub(super) collapsed_keys: HashSet<String>,
    pub(super) expanded: HashMap<String, ExpandedHunks>,
    pub(super) hydrated: HashMap<String, Arc<FileDiffMetadata>>,
    pub(super) file_rows: Vec<Arc<Vec<DiffRow>>>,
    /// Rows per file, reused while its inputs are unchanged.
    rows_cache: HashMap<String, (RowsKey, Arc<Vec<DiffRow>>)>,
    pub(super) items: Vec<FlatItem>,
    pub(super) list: ListState,
    pub(super) menu: Option<OpenMenu>,
    pub(super) base_ref_query: String,
    pub(super) selection: Option<LineSelection>,
    pub(super) draft: Option<CommentDraft>,
    pub(super) comments: Vec<ReviewCommentContext>,
    pub(super) next_comment_id: u64,
    /// Lazy sources: how many files' patches have been requested.
    pub(super) requested_files: usize,
    pub(super) last_patch: Option<(String, Arc<Vec<FileDiffMetadata>>)>,
    pub(super) tree: super::file_tree::TreeState,
    pub(super) focus_handle: FocusHandle,
    pub(super) tree_focus: FocusHandle,
    pub(super) copied_path: Option<String>,
    pub(super) comment_input: Option<super::annotation::CommentInput>,
    pub(super) handled_reveal: Option<(String, u64)>,
    pub(super) pending_reveal: Option<String>,
    pub(super) keyboard_file: Option<usize>,
    pub(super) dark: bool,
    /// Dev switch: expand only files whose path contains this.
    pub(super) open_only: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl DiffPanel {
    pub fn new(source: Arc<dyn DiffSource>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let list = ListState::new(0, ListAlignment::Top, px(1200.));
        let entity = cx.entity().downgrade();
        source.set_notifier(Arc::new(move |cx: &mut App| {
            entity.update(cx, |panel, cx| panel.reload(cx)).ok();
        }));
        let mut settings = DiffPanelSettings::default();
        // Dev switches for screenshots.
        let env = |name: &str| std::env::var(name).ok();
        if env("TURIN_DIFF_EXPAND").as_deref() == Some("1") {
            settings.files_collapsed = false;
        }
        if env("TURIN_DIFF_LAYOUT").as_deref() == Some("split") {
            settings.layout = DiffLayout::Split;
        }
        // `TURIN_DIFF_OPEN=<substring>`: only matching files start expanded.
        let open_only = env("TURIN_DIFF_OPEN");
        if env("TURIN_DIFF_TREE").as_deref() == Some("1") {
            settings.file_tree_open = true;
        }
        if env("TURIN_DIFF_WRAP").as_deref() == Some("0") {
            settings.word_wrap = false;
        }
        let store = std::env::var("TURIN_DIFF_PANEL_STATE_JSON")
            .ok()
            .and_then(|json| decode_diff_panel_store_json(&json))
            .unwrap_or_default();
        let mut panel = Self {
            source,
            store,
            settings,
            derived: Derived::empty(),
            collapsed_scope: None,
            collapsed_keys: HashSet::new(),
            expanded: HashMap::new(),
            hydrated: HashMap::new(),
            file_rows: Vec::new(),
            rows_cache: HashMap::new(),
            items: Vec::new(),
            list,
            menu: None,
            base_ref_query: String::new(),
            selection: None,
            draft: None,
            comments: Vec::new(),
            next_comment_id: 1,
            requested_files: 4,
            last_patch: None,
            tree: Default::default(),
            focus_handle: cx.focus_handle(),
            tree_focus: cx.focus_handle(),
            copied_path: None,
            comment_input: None,
            handled_reveal: None,
            pending_reveal: None,
            keyboard_file: None,
            open_only,
            dark: cx.app_theme().is_dark(),
            _subscriptions: Vec::new(),
        };
        let _ = window;
        match env("TURIN_DIFF_SCOPE").as_deref() {
            Some("unstaged") => {
                if let Some(thread) = panel.source.context().thread {
                    panel.store.select_git_scope(&thread, GitScope::Unstaged);
                }
            }
            Some("branch") => {
                if let Some(thread) = panel.source.context().thread {
                    panel.store.select_git_scope(&thread, GitScope::Branch);
                }
            }
            Some("turn") => {
                if let Some(thread) = panel.source.context().thread
                    && let Some(turn) = panel.source.turns().last()
                {
                    panel.store.select_turn(&thread, &turn.run_id, None);
                }
            }
            _ => {}
        }
        panel.sync_persisted_store(cx);
        panel.reload(cx);
        // Dev switches: open a menu, or a comment draft / saved comment on the
        // first changed line of the first expanded file.
        panel.menu = match env("TURIN_DIFF_MENU").as_deref() {
            Some("scope") => Some(OpenMenu::Scope),
            Some("turns") => Some(OpenMenu::ScopeTurns),
            Some("baseref") => Some(OpenMenu::BaseRef),
            _ => None,
        };
        if let Some(item) = env("TURIN_DIFF_SCROLL_ITEM").and_then(|v| v.parse::<usize>().ok()) {
            panel.list.scroll_to(gpui_kit::ListOffset {
                item_ix: item,
                offset_in_item: px(12.),
            });
        }
        if let Some(spec) = env("TURIN_DIFF_EXPAND_CONTEXT") {
            // `<hunk>[:down|up]` on the first expanded file.
            let mut parts = spec.split(':');
            let hunk = parts.next().and_then(|v| v.parse().ok()).unwrap_or(1);
            let direction = match parts.next() {
                Some("up") => ExpandDirection::Up,
                Some("down") => ExpandDirection::Down,
                _ => ExpandDirection::Both,
            };
            if let Some(index) = panel
                .derived
                .files
                .iter()
                .position(|entry| !panel.collapsed_keys.contains(&entry.key))
            {
                panel.expand_hunk(index, hunk, direction, false, cx);
            }
        }
        if let Some(mode) = env("TURIN_DIFF_COMMENT") {
            panel.dev_comment(&mode, window, cx);
        }
        panel
    }

    /// `TURIN_DIFF_COMMENT=draft|saved|select`.
    fn dev_comment(&mut self, mode: &str, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.derived.files.iter().position(|entry| {
            !self.collapsed_keys.contains(&entry.key) && !entry.file.hunks.is_empty()
        }) else {
            return;
        };
        let entry = self.derived.files[index].clone();
        let Some(cell) = self.file_rows[index].iter().find_map(|row| match row {
            DiffRow::Line(cell) if cell.kind == LineKind::Addition => Some(*cell),
            DiffRow::Split {
                right: Some(cell), ..
            } if cell.kind == LineKind::Addition => Some(*cell),
            _ => None,
        }) else {
            return;
        };
        let range = SelectedLineRange {
            start: cell.number,
            side: Some(cell.side),
            end: cell.number,
            end_side: Some(cell.side),
        };
        self.selection = Some(LineSelection {
            file_key: entry.key.clone(),
            range,
        });
        match mode {
            "draft" => self.begin_comment(index, range, cx),
            "saved" => {
                if let Some(mut comment) = build_diff_review_comment(BuildDiffReviewCommentInput {
                    id: "review-comment-0",
                    section_id: &self.derived.review_section_id,
                    section_title: &self.derived.review_section_title,
                    file_path: &entry.path,
                    file_diff: &entry.file,
                    range,
                    text: "Keep this in sync with the checkpoint summary.\nThe stacked layout reads the same list.",
                }) {
                    comment.id = "review-comment-0".into();
                    self.comments.push(comment);
                    self.selection = None;
                    self.rebuild_items();
                }
            }
            _ => cx.notify(),
        }
    }

    pub fn source(&self) -> &Arc<dyn DiffSource> {
        &self.source
    }

    /// `useDiffPanelStore.selectTurn` for "Open diff" on a turn's changed
    /// files card.
    pub fn open_turn_diff(
        &mut self,
        run_id: &str,
        file_path: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.derived.context.thread.clone() else {
            return;
        };
        self.store.select_turn(&thread, run_id, file_path);
        self.sync_persisted_store(cx);
        self.reload(cx);
        cx.notify();
    }

    /// Keyboard scrolling of the code view (the focusable custom element
    /// scrolls with the arrow, page, home and end keys).
    pub(super) fn scroll_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let page = self.list.viewport_bounds().size.height * 0.9;
        match key {
            "down" => self.list.scroll_by(px(40.)),
            "up" => self.list.scroll_by(px(-40.)),
            "pagedown" | "space" => self.list.scroll_by(page),
            "pageup" => self.list.scroll_by(-page),
            "home" => self.list.scroll_to(gpui_kit::ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            }),
            "end" => self.list.scroll_to_end(),
            _ => return false,
        }
        cx.notify();
        true
    }

    pub fn thread(&self) -> Option<&ScopedThreadRef> {
        self.derived.context.thread.as_ref()
    }

    // ── derivation ──────────────────────────────────────────────────────

    /// Recomputes everything derived from the source and panel state, and
    fn sync_persisted_store(&self, cx: &mut Context<Self>) {
        cx.default_global::<DiffPanelPersistedSnapshot>().value =
            Some(encode_diff_panel_store(&self.store));
    }

    /// splices the list.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let started = std::time::Instant::now();
        let context = self.source.context();
        let mut derived = Derived::empty();
        derived.context = context.clone();
        let thread = context.thread.clone();

        // `useTurnDiffSummaries` + ordering.
        let mut turns = self.source.turns();
        turns.sort_by(|left, right| {
            Derived::turn_count(right)
                .cmp(&Derived::turn_count(left))
                .then_with(|| right.completed_at.cmp(&left.completed_at))
        });
        if let Some(thread) = &thread {
            let ids: Vec<String> = turns.iter().map(|t| t.run_id.clone()).collect();
            self.store.reconcile_turn_selection(thread, &ids);
            self.sync_persisted_store(cx);
        }
        let selection = self.store.selection(thread.as_ref());
        let selected_run_id = match &selection {
            DiffPanelSelection::Turn { turn_id, .. } => Some(turn_id.clone()),
            _ => None,
        };
        let (selected_git_scope, selected_base_ref) = match &selection {
            DiffPanelSelection::Unstaged => (GitScope::Unstaged, None),
            DiffPanelSelection::Branch { base_ref } => (GitScope::Branch, base_ref.clone()),
            _ => (GitScope::Branch, None),
        };
        let selected_file_path = match &selection {
            DiffPanelSelection::Turn { file_path, .. } => file_path.clone(),
            _ => None,
        };
        derived.reveal_request_id = match &selection {
            DiffPanelSelection::Turn {
                reveal_request_id, ..
            } => *reveal_request_id,
            _ => 0,
        };
        derived.selected_file_path = selected_file_path.clone();
        let latest_turn = turns.first().cloned();
        let selected_turn = selected_run_id.as_ref().and_then(|run_id| {
            turns
                .iter()
                .find(|turn| &turn.run_id == run_id)
                .or(turns.first())
                .cloned()
        });
        let selected_turn_count = selected_turn.as_ref().map(Derived::turn_count);
        derived.selected_scope_label = match &selected_run_id {
            None => match selected_git_scope {
                GitScope::Unstaged => "Uncommitted".into(),
                GitScope::Branch => "Changes".into(),
            },
            Some(_) => match (&selected_turn, &latest_turn) {
                (Some(selected), Some(latest)) if selected.run_id == latest.run_id => {
                    "Latest turn".into()
                }
                _ => format!(
                    "Turn {}",
                    selected_turn_count.map_or("?".to_string(), |c| c.to_string())
                ),
            },
        };
        derived.review_section_id = match &selected_turn {
            Some(turn) if selected_run_id.is_some() => format!("turn:{}", turn.run_id),
            _ => match selected_git_scope {
                GitScope::Unstaged => "unstaged".into(),
                GitScope::Branch => "branch".into(),
            },
        };
        derived.review_section_title = match (&selected_turn, selected_run_id.is_some()) {
            (Some(_), true) => format!(
                "Turn {}",
                selected_turn_count.map_or("?".to_string(), |c| c.to_string())
            ),
            _ => match selected_git_scope {
                GitScope::Unstaged => "Uncommitted".into(),
                GitScope::Branch => "Changes".into(),
            },
        };
        derived.collapse_scope_key = thread
            .as_ref()
            .map(|thread| format!("{}:{}", thread.key(), derived.review_section_id));
        derived.scope = match &selected_run_id {
            Some(_) => selected_turn
                .as_ref()
                .map(|t| Scope::Turn {
                    run_id: t.run_id.clone(),
                })
                .unwrap_or(Scope::Branch { base_ref: None }),
            None => match selected_git_scope {
                GitScope::Unstaged => Scope::Unstaged,
                GitScope::Branch => Scope::Branch {
                    base_ref: selected_base_ref.clone(),
                },
            },
        };
        derived.turns = turns;
        let ignore_whitespace = self.settings.ignore_whitespace;
        let turn_selected = selected_turn.is_some() && selected_run_id.is_some();
        derived.can_refresh = context.is_git_repo
            && selected_run_id.is_none()
            && context.thread.is_some()
            && context.active_cwd.is_some();

        // ── body state and data ─────────────────────────────────────────
        if thread.is_none() {
            derived.body = BodyState::NoThread;
        } else if !context.is_git_repo {
            derived.body = BodyState::NotGit;
        } else if selected_run_id.is_some() && derived.turns.is_empty() {
            derived.body = BodyState::NoTurns;
        } else if selected_run_id.is_none() && !context.can_read_files {
            derived.body =
                BodyState::NoFileAccess("This connection cannot read local diffs.".into());
        } else {
            let mut patch: Option<String> = None;
            let mut loading = false;
            let mut error: Option<String> = None;
            let mut lazy_files: Option<Vec<GitDiffFileStat>> = None;
            let mut git_source: Option<GitDiffSource> = None;
            if turn_selected {
                let to = selected_turn_count.unwrap_or(0);
                match self
                    .source
                    .checkpoint_diff(to.saturating_sub(1), to, ignore_whitespace)
                {
                    Loadable::Ready(text) => patch = Some(text),
                    Loadable::Pending => loading = true,
                    Loadable::Failed(message) => error = Some(message),
                }
            } else {
                match self
                    .source
                    .git_diff_preview(selected_base_ref.as_deref(), ignore_whitespace)
                {
                    Loadable::Ready(preview) => {
                        let kind = match selected_git_scope {
                            GitScope::Unstaged => GitSourceKind::WorkingTree,
                            GitScope::Branch => GitSourceKind::BranchRange,
                        };
                        git_source = preview.sources.iter().find(|s| s.kind == kind).cloned();
                        if let Some(source) = &git_source {
                            if source.truncated && source.files.is_some() {
                                lazy_files = source.files.clone();
                            } else {
                                patch = Some(source.diff.clone());
                            }
                        }
                        let _: GitDiffPreview = preview;
                    }
                    Loadable::Pending => loading = true,
                    Loadable::Failed(message) => error = Some(message),
                }
            }
            derived.git_source = git_source.clone();
            derived.refreshing = loading;
            derived.error = error.clone();
            let truncated_without_lazy = git_source
                .as_ref()
                .is_some_and(|s| s.truncated && lazy_files.is_none());
            if truncated_without_lazy {
                derived.banner = Some(format!(
                    "This preview exceeds the size limit. Changes shown are incomplete.{}",
                    if git_source.as_ref().is_some_and(|s| s.files.is_some()) {
                        " Totals include all changes."
                    } else {
                        ""
                    }
                ));
            }
            if let Some(stats) = lazy_files {
                derived.lazy = true;
                self.derive_lazy_files(
                    &mut derived,
                    &stats,
                    git_source.as_ref(),
                    selected_base_ref.as_deref(),
                );
            } else {
                let has_patch = patch.is_some();
                let has_no_net_changes =
                    patch.as_deref().is_some_and(|text| text.trim().is_empty());
                let renderable = self.renderable_files(
                    patch.as_deref(),
                    &derived.collapse_scope_key,
                    selected_run_id.is_none(),
                );
                match renderable {
                    Some(Ok(files)) => {
                        derived.files = self.entries_for(files);
                        derived.body = BodyState::Files;
                    }
                    Some(Err((text, reason))) => derived.body = BodyState::Raw { text, reason },
                    None => {
                        derived.body = if loading {
                            BodyState::Loading(if turn_selected {
                                "Loading checkpoint diff..."
                            } else if selected_git_scope == GitScope::Unstaged {
                                "Loading uncommitted changes..."
                            } else {
                                "Loading changes..."
                            })
                        } else if has_no_net_changes && has_patch {
                            BodyState::Message("No net changes in this selection.")
                        } else {
                            BodyState::Message("No patch available for this selection.")
                        };
                    }
                }
            }
            // `diffLineStat`
            derived.stat = match (&git_source, turn_selected) {
                (Some(source), false) if source.files.is_some() => source
                    .files
                    .iter()
                    .flatten()
                    .fold(DiffLineStat::default(), |total, file| DiffLineStat {
                        additions: total.additions + file.additions,
                        deletions: total.deletions + file.deletions,
                    }),
                _ => get_diff_line_stat(derived.files.iter().map(|entry| entry.file.as_ref())),
            };
            derived.has_stat = !derived.files.is_empty()
                || (!turn_selected
                    && git_source
                        .as_ref()
                        .is_some_and(|s| s.files.as_ref().is_some_and(|f| !f.is_empty())));
            if derived.lazy {
                derived.settled_count = derived
                    .files
                    .iter()
                    .position(|entry| entry.pending)
                    .unwrap_or(derived.files.len());
            } else {
                derived.settled_count = derived.files.len();
            }
        }

        derived.tree_entries = diff_file_tree_entries(
            &derived
                .files
                .iter()
                .map(|entry| entry.file.as_ref().clone())
                .collect::<Vec<_>>(),
        );

        // Collapsed set belongs to a scope; a new scope starts from the default.
        if self.collapsed_scope != derived.collapse_scope_key {
            self.collapsed_scope = derived.collapse_scope_key.clone();
            self.collapsed_keys = if let Some(only) = &self.open_only {
                derived
                    .files
                    .iter()
                    .filter(|entry| !entry.path.contains(only.as_str()))
                    .map(|entry| entry.key.clone())
                    .collect()
            } else if self.settings.files_collapsed {
                derived
                    .files
                    .iter()
                    .map(|entry| entry.key.clone())
                    .collect()
            } else {
                HashSet::new()
            };
            self.expanded.clear();
            self.selection = None;
            self.draft = None;
            self.comment_input = None;
            self.requested_files = 4;
            self.handled_reveal = None;
        } else if self.settings.files_collapsed {
            // Files that newly arrive (lazy loading, refreshes) start collapsed
            // like the rest of the scope (`defaultCollapsedDiffFileKeys`).
            for entry in &derived.files {
                if !self.known_file_keys().contains(&entry.key) {
                    self.collapsed_keys.insert(entry.key.clone());
                }
            }
        }
        self.derived = derived;
        self.remember_file_keys();
        self.rebuild_items();
        self.apply_reveal(cx);
        tracing::debug!(
            files = self.derived.files.len(),
            items = self.items.len(),
            elapsed_us = started.elapsed().as_micros() as u64,
            "diff derived"
        );
        cx.notify();
    }

    fn known_file_keys(&self) -> HashSet<String> {
        self.tree.known_file_keys.clone()
    }

    fn remember_file_keys(&mut self) {
        self.tree.known_file_keys = self.derived.files.iter().map(|e| e.key.clone()).collect();
    }

    /// `getRenderablePatch` with the panel's options, cached on the patch.
    #[allow(clippy::type_complexity)]
    fn renderable_files(
        &mut self,
        patch: Option<&str>,
        scope_key: &Option<String>,
        compact: bool,
    ) -> Option<Result<Vec<FileDiffMetadata>, (String, String)>> {
        let patch = patch?;
        let normalized = patch.trim();
        if normalized.is_empty() {
            return None;
        }
        let options = RenderablePatchOptions {
            ignore_whitespace: self.settings.ignore_whitespace,
            compact_partial_hunk_offsets: compact,
        };
        let dark = self.is_dark_cache();
        let cache_scope = format!("diff-panel:{}", if dark { "dark" } else { "light" });
        let key = format!(
            "{}:{}:{}:{}",
            build_patch_cache_key(normalized, &cache_scope),
            options.ignore_whitespace,
            options.compact_partial_hunk_offsets,
            scope_key.as_deref().unwrap_or("")
        );
        if let Some((cached_key, files)) = &self.last_patch
            && *cached_key == key
        {
            return Some(Ok(files.as_ref().clone()));
        }
        match get_renderable_patch(Some(normalized), &cache_scope, options) {
            Some(RenderablePatch::Files { mut files, .. }) => {
                files.sort_by(|a, b| {
                    natural_compare(&resolve_file_diff_path(a), &resolve_file_diff_path(b))
                });
                self.last_patch = Some((key, Arc::new(files.clone())));
                Some(Ok(files))
            }
            Some(RenderablePatch::Raw { text, reason }) => Some(Err((text, reason))),
            None => None,
        }
    }

    fn is_dark_cache(&self) -> bool {
        self.dark
    }

    /// `useReviewFilePatches` for a truncated source: stats-ordered files,
    /// patches for the requested prefix, header-only placeholders after.
    fn derive_lazy_files(
        &mut self,
        derived: &mut Derived,
        stats: &[GitDiffFileStat],
        source: Option<&GitDiffSource>,
        base_ref: Option<&str>,
    ) {
        let Some(source) = source else {
            return;
        };
        let mut stats = stats.to_vec();
        stats.sort_by(|a, b| natural_compare(&a.path, &b.path));
        let mut files = Vec::new();
        for (index, stat) in stats.iter().enumerate() {
            let mut entry_file: Option<FileDiffMetadata> = None;
            let mut error = false;
            let mut truncated = false;
            if index < self.requested_files {
                match self.source.git_file_patch(
                    source.kind,
                    base_ref,
                    stat,
                    self.settings.ignore_whitespace,
                ) {
                    Loadable::Ready(file_source) => {
                        truncated = file_source.truncated;
                        let options = RenderablePatchOptions {
                            ignore_whitespace: self.settings.ignore_whitespace,
                            compact_partial_hunk_offsets: true,
                        };
                        match get_renderable_patch(Some(&file_source.diff), "diff-panel", options) {
                            Some(RenderablePatch::Files { files, .. }) if files.len() == 1 => {
                                let mut file =
                                    files.into_iter().next().unwrap_or_else(|| unreachable!());
                                file.name = stat.path.clone();
                                file.prev_name = stat.previous_path.clone();
                                entry_file = Some(file);
                            }
                            Some(RenderablePatch::Files { files, .. }) => {
                                entry_file = files
                                    .into_iter()
                                    .find(|f| resolve_file_diff_path(f) == stat.path);
                                error = entry_file.is_none();
                            }
                            _ => error = true,
                        }
                    }
                    Loadable::Failed(_) => error = true,
                    Loadable::Pending => {}
                }
            }
            let pending = entry_file.is_none();
            let file = entry_file.unwrap_or_else(|| {
                let mut file = FileDiffMetadata::empty(true);
                file.name = stat.path.clone();
                file.prev_name = stat.previous_path.clone();
                file.r#type = if stat.previous_path.is_some() {
                    ChangeType::RenameChanged
                } else {
                    ChangeType::Change
                };
                file.cache_key = Some(format!("{}:{}:pending", source.diff_hash, stat.path));
                file
            });
            let key = build_file_diff_identity_key(&file);
            let file = self
                .hydrated
                .get(&key)
                .cloned()
                .unwrap_or_else(|| Arc::new(file));
            files.push(FileEntry {
                key,
                path: stat.path.clone(),
                version: build_file_diff_content_version(&file),
                pending: pending && !error,
                stat: Some(stat.clone()),
                error,
                truncated,
                file,
            });
        }
        derived.body = BodyState::Files;
        derived.files = files;
    }

    fn entries_for(&self, files: Vec<FileDiffMetadata>) -> Vec<FileEntry> {
        files
            .into_iter()
            .map(|file| {
                let key = build_file_diff_identity_key(&file);
                let file = self
                    .hydrated
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| Arc::new(file));
                FileEntry {
                    path: resolve_file_diff_path(&file),
                    version: build_file_diff_content_version(&file),
                    pending: false,
                    stat: None,
                    error: false,
                    truncated: false,
                    key,
                    file,
                }
            })
            .collect()
    }

    // ── list ────────────────────────────────────────────────────────────

    /// Builds rows per expanded file and splices the list.
    pub fn rebuild_items(&mut self) {
        let style = self.settings.layout.style();
        let loader = self.has_file_loader();
        let mut file_rows = Vec::with_capacity(self.derived.files.len());
        let mut items = Vec::new();
        for (file_index, entry) in self.derived.files.iter().enumerate() {
            items.push(FlatItem::Header(file_index));
            let collapsed = self.collapsed_keys.contains(&entry.key) || entry.pending;
            if collapsed {
                file_rows.push(Arc::new(Vec::new()));
                continue;
            }
            let expanded = self.expanded.get(&entry.key).cloned().unwrap_or_default();
            let mut regions: Vec<(usize, usize, usize)> = expanded
                .regions
                .iter()
                .map(|(index, region)| (*index, region.from_start, region.from_end))
                .collect();
            regions.sort_unstable();
            let rows_key = (style == DiffStyle::Split, loader, entry.version, regions);
            let rows = match self.rows_cache.get(&entry.key) {
                Some((key, rows)) if *key == rows_key => rows.clone(),
                _ => {
                    let rows = Arc::new(build_rows(&entry.file, style, &expanded, loader));
                    self.rows_cache
                        .insert(entry.key.clone(), (rows_key, rows.clone()));
                    rows
                }
            };
            let annotations = self.annotation_rows(entry, &rows);
            for row_index in 0..rows.len() {
                items.push(FlatItem::Row(file_index, row_index));
                if annotations.contains(&row_index) {
                    items.push(FlatItem::Annotation(file_index, row_index));
                }
            }
            items.push(FlatItem::Gap(file_index));
            file_rows.push(rows);
        }
        if self.derived.lazy && self.derived.settled_count < self.derived.files.len() {
            items.push(FlatItem::LoadingBoundary);
        }
        let old = self.items.len();
        self.file_rows = file_rows;
        self.items = items;
        self.list.splice(0..old, self.items.len());
    }

    /// Row indexes after which an annotation (a saved comment or the draft)
    /// is rendered.
    pub(super) fn annotation_rows(&self, entry: &FileEntry, rows: &[DiffRow]) -> HashSet<usize> {
        let mut targets: Vec<(SelectionSide, usize)> = Vec::new();
        for comment in &self.comments {
            if comment.section_id == self.derived.review_section_id
                && comment.file_path == entry.path
                && comment.fence_language.as_deref().unwrap_or("diff") == "diff"
                && let Some(range) = restore_diff_review_comment_range(&entry.file, comment)
            {
                targets.push((annotation_side(&range), range.end));
            }
        }
        if let Some(draft) = &self.draft
            && draft.file_key == entry.key
        {
            targets.push((draft.annotation.side, draft.annotation.line_number));
        }
        let mut result = HashSet::new();
        for (side, number) in targets {
            if let Some(index) = row_index_for_line(rows, side, number) {
                result.insert(index);
            }
        }
        result
    }

    pub(super) fn has_file_loader(&self) -> bool {
        self.derived.git_source.is_some()
            && !matches!(self.derived.scope, Scope::Turn { .. })
            && self.derived.context.can_read_files
    }

    pub fn is_collapsed(&self, key: &str) -> bool {
        self.collapsed_keys.contains(key)
    }

    // ── actions ─────────────────────────────────────────────────────────

    pub fn toggle_file_collapsed(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.collapsed_keys.remove(key) {
            self.collapsed_keys.insert(key.to_string());
        }
        self.rebuild_items();
        cx.notify();
    }

    /// `unfoldDiffFile`: never folds.
    pub fn unfold_file(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.collapsed_keys.remove(key) {
            self.rebuild_items();
            cx.notify();
        }
    }

    pub fn all_files_collapsed(&self) -> bool {
        let keys: Vec<String> = self.derived.files.iter().map(|e| e.key.clone()).collect();
        are_all_diff_files_collapsed(&keys, &self.collapsed_keys)
    }

    /// `toggleDiffFileCollapse`.
    pub fn toggle_all_files_collapsed(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<String> = self.derived.files.iter().map(|e| e.key.clone()).collect();
        self.collapsed_keys = toggle_all_diff_files(&keys, &self.collapsed_keys);
        self.rebuild_items();
        cx.notify();
    }

    pub fn set_layout(&mut self, layout: DiffLayout, cx: &mut Context<Self>) {
        if self.settings.layout != layout {
            self.settings.layout = layout;
            self.rebuild_items();
            cx.notify();
        }
    }

    pub fn set_word_wrap(&mut self, wrap: bool, cx: &mut Context<Self>) {
        self.settings.word_wrap = wrap;
        self.list.remeasure();
        cx.notify();
    }

    pub fn set_ignore_whitespace(&mut self, ignore: bool, cx: &mut Context<Self>) {
        self.settings.ignore_whitespace = ignore;
        self.last_patch = None;
        self.reload(cx);
    }

    pub fn set_file_tree_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.settings.file_tree_open = open;
        cx.notify();
    }

    pub fn select_scope(&mut self, value: &str, cx: &mut Context<Self>) {
        let Some(thread) = self.derived.context.thread.clone() else {
            return;
        };
        match value {
            "branch" => self.store.select_git_scope(&thread, GitScope::Branch),
            "unstaged" => self.store.select_git_scope(&thread, GitScope::Unstaged),
            "latest" => {
                if let Some(latest) = self.derived.turns.first() {
                    let run_id = latest.run_id.clone();
                    self.store.select_turn(&thread, &run_id, None);
                }
            }
            other => {
                if let Some(run_id) = other.strip_prefix("turn:") {
                    self.store.select_turn(&thread, run_id, None);
                }
            }
        }
        self.menu = None;
        self.sync_persisted_store(cx);
        self.reload(cx);
    }

    pub fn select_base_ref(&mut self, base_ref: Option<&str>, cx: &mut Context<Self>) {
        let Some(thread) = self.derived.context.thread.clone() else {
            return;
        };
        self.store.select_branch_base_ref(&thread, base_ref);
        self.sync_persisted_store(cx);
        self.menu = None;
        self.base_ref_query.clear();
        self.reload(cx);
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let source = self.source.clone();
        source.refresh(cx);
    }

    pub fn base_ref_choices(&self, query: &str) -> (Vec<BaseRefChoice>, Vec<BaseRefChoice>) {
        let refs = self.source.list_refs(query);
        let head = self
            .derived
            .git_source
            .as_ref()
            .and_then(|source| source.head_ref.clone());
        let local: Vec<_> = refs
            .local
            .into_iter()
            .filter(|reference| Some(&reference.name) != head.as_ref())
            .collect();
        let choices = build_base_ref_choices(&local, &refs.remote);
        let matching = filter_base_ref_choices(&choices, query);
        (choices, matching)
    }

    /// `expandHunk` + `loadFilesIfNecessary`.
    pub fn expand_hunk(
        &mut self,
        file_index: usize,
        hunk_index: usize,
        direction: ExpandDirection,
        all: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.derived.files.get(file_index).cloned() else {
            return;
        };
        if entry.file.is_partial {
            let Some(source) = self.derived.git_source.clone() else {
                return;
            };
            let cache_key = source.diff_hash.clone();
            let base_ref = match &self.derived.scope {
                Scope::Branch { .. } => source.base_ref.clone(),
                _ => None,
            };
            match load_diff_files(
                self.source.as_ref(),
                source.kind,
                base_ref.as_deref(),
                source.head_ref.as_deref(),
                &cache_key,
                &entry.file,
            ) {
                Ok((old, new)) => {
                    match hydrate_partial_diff(&entry.file, old.as_ref(), Some(&new)) {
                        Ok(hydrated) => {
                            self.hydrated.insert(entry.key.clone(), Arc::new(hydrated));
                        }
                        Err(error) => {
                            tracing::warn!(%error, path = %entry.path, "failed to hydrate diff");
                            return;
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, path = %entry.path, "failed to load diff file contents");
                    return;
                }
            }
            if let Some(updated) = self.hydrated.get(&entry.key).cloned()
                && let Some(slot) = self.derived.files.get_mut(file_index)
            {
                slot.version = build_file_diff_content_version(&updated);
                slot.file = updated;
            }
        }
        let count = if all {
            usize::MAX / 4
        } else {
            super::pierre::iterate_over_diff::DEFAULT_EXPANSION_LINE_COUNT
        };
        let direction = if all {
            ExpandDirection::Both
        } else {
            direction
        };
        self.expanded
            .entry(entry.key.clone())
            .or_default()
            .expand(hunk_index, direction, count);
        self.rebuild_items();
        cx.notify();
    }

    /// Lazy sources: asks for the next batch of files
    /// (`loadNextFiles`, four at a time).
    pub fn load_next_files(&mut self, cx: &mut Context<Self>) {
        if !self.derived.lazy {
            return;
        }
        let target = self.derived.settled_count + 4;
        if self.requested_files < target {
            self.requested_files = target;
            self.reload(cx);
        }
    }

    /// `openDiffFile` → `openDiffFilePrimaryAction`.
    pub fn open_diff_file(&mut self, file_path: &str, cx: &mut Context<Self>) {
        struct Targets<'a> {
            source: &'a dyn DiffSource,
            cx: &'a mut App,
        }
        impl DiffFileTargets for Targets<'_> {
            fn open_in_thread_file_viewer(&mut self, thread: &ScopedThreadRef, path: &str) {
                self.source
                    .open_in_thread_file_viewer(thread, path, self.cx);
            }
            fn open_in_editor(&mut self, target_path: &str) {
                self.source.open_in_editor(target_path, self.cx);
            }
        }
        let context = self.derived.context.clone();
        let source = self.source.clone();
        let mut targets = Targets {
            source: source.as_ref(),
            cx,
        };
        open_diff_file_primary_action(
            OpenDiffFilePrimaryActionInput {
                thread: context.thread.as_ref(),
                file_path,
                active_cwd: context.active_cwd.as_deref(),
                repository_root: context.repository_root.as_deref(),
            },
            &mut targets,
        );
    }

    /// Reveals a file from the tree or an external selection
    /// (`revealDiffFile`): unfolds it and scrolls it to the top.
    pub fn reveal_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(index) = self.derived.files.iter().position(|e| e.path == path) else {
            return;
        };
        let key = self.derived.files[index].key.clone();
        self.collapsed_keys.remove(&key);
        if self.derived.lazy && index >= self.derived.settled_count {
            self.requested_files = self.requested_files.max(index + 1);
            self.pending_reveal = Some(key.clone());
            self.reload(cx);
            return;
        }
        self.rebuild_items();
        self.scroll_to_file(index);
        cx.notify();
    }

    fn scroll_to_file(&self, file_index: usize) {
        if let Some(ix) = self
            .items
            .iter()
            .position(|item| *item == FlatItem::Header(file_index))
        {
            self.list.scroll_to(gpui_kit::ListOffset {
                item_ix: ix,
                offset_in_item: px(0.),
            });
        }
    }

    /// Applies the external selection's file reveal once per request.
    fn apply_reveal(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.pending_reveal.take()
            && let Some(index) = self
                .derived
                .files
                .iter()
                .position(|e| e.key == key && !e.pending)
        {
            self.scroll_to_file(index);
        }
        let Some(path) = self.derived.selected_file_path.clone() else {
            return;
        };
        let token = (path.clone(), self.derived.reveal_request_id);
        if self.handled_reveal.as_ref() == Some(&token) {
            return;
        }
        if self.derived.files.iter().any(|e| e.path == path) {
            self.handled_reveal = Some(token);
            self.reveal_file(&path, cx);
        }
    }

    // ── helpers for rendering ───────────────────────────────────────────

    pub(super) fn highlights(&self, entry: &FileEntry, dark: bool) -> Arc<FileHighlights> {
        file_highlights(&entry.file, &entry.key, entry.version, dark)
    }

    // ── comments ────────────────────────────────────────────────────────

    pub fn begin_comment(
        &mut self,
        file_index: usize,
        range: SelectedLineRange,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.derived.files.get(file_index) else {
            return;
        };
        let id = format!("review-comment-{}", self.next_comment_id);
        let Some(comment) = build_diff_review_comment(BuildDiffReviewCommentInput {
            id: &id,
            section_id: &self.derived.review_section_id,
            section_title: &self.derived.review_section_title,
            file_path: &entry.path,
            file_diff: &entry.file,
            range,
            text: "",
        }) else {
            return;
        };
        self.next_comment_id += 1;
        self.draft = Some(CommentDraft {
            file_key: entry.key.clone(),
            id,
            range,
            range_label: comment.range_label,
            annotation: DiffLineAnnotation {
                side: annotation_side(&range),
                line_number: range.end,
                metadata: (),
            },
        });
        self.comment_input = None;
        self.rebuild_items();
        cx.notify();
    }

    /// The "+" button of a line: comments on the current selection when it
    /// holds the line, else on the line alone (`onGutterUtilityClick`).
    pub fn begin_comment_at(
        &mut self,
        file_key: &str,
        file_index: usize,
        cell: super::rows::Cell,
        cx: &mut Context<Self>,
    ) {
        if self.draft.is_some() {
            return;
        }
        let selected = self
            .selection
            .as_ref()
            .filter(|selection| selection.file_key == file_key)
            .map(|selection| selection.range)
            .filter(|range| {
                let (low, high) = (range.start.min(range.end), range.start.max(range.end));
                cell.number >= low && cell.number <= high
            });
        let range = selected.unwrap_or(SelectedLineRange {
            start: cell.number,
            side: Some(cell.side),
            end: cell.number,
            end_side: Some(cell.side),
        });
        self.selection = Some(LineSelection {
            file_key: file_key.to_string(),
            range,
        });
        self.begin_comment(file_index, range, cx);
    }

    /// Clicking a line number selects it; shift extends within the file
    /// (`enableLineSelection`; off while a comment is open).
    pub fn select_line(
        &mut self,
        file_key: &str,
        cell: super::rows::Cell,
        shift: bool,
        cx: &mut Context<Self>,
    ) {
        if self.draft.is_some() {
            return;
        }
        let range = match &self.selection {
            Some(selection) if shift && selection.file_key == file_key => SelectedLineRange {
                end: cell.number,
                end_side: Some(cell.side),
                ..selection.range
            },
            _ => SelectedLineRange {
                start: cell.number,
                side: Some(cell.side),
                end: cell.number,
                end_side: Some(cell.side),
            },
        };
        self.selection = Some(LineSelection {
            file_key: file_key.to_string(),
            range,
        });
        cx.notify();
    }

    /// `isCommentSubmitShortcut` / the Comment button: send when the trimmed
    /// text is not empty.
    pub fn try_submit_draft(&mut self, text: &str, cx: &mut Context<Self>) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        let trimmed = trimmed.to_string();
        self.submit_draft(&trimmed, cx);
    }

    pub fn cancel_draft(&mut self, cx: &mut Context<Self>) {
        self.selection = None;
        self.draft = None;
        self.comment_input = None;
        self.rebuild_items();
        cx.notify();
    }

    /// `submitEntry`: stores the comment on the composer draft.
    pub fn submit_draft(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(draft) = self.draft.clone() else {
            return;
        };
        let Some(entry) = self
            .derived
            .files
            .iter()
            .find(|e| e.key == draft.file_key)
            .cloned()
        else {
            return;
        };
        if let Some(comment) = build_diff_review_comment(BuildDiffReviewCommentInput {
            id: &draft.id,
            section_id: &self.derived.review_section_id,
            section_title: &self.derived.review_section_title,
            file_path: &entry.path,
            file_diff: &entry.file,
            range: draft.range,
            text,
        }) {
            tracing::info!(id = %comment.id, path = %comment.file_path, "review comment added");
            self.comments.push(comment);
        }
        self.cancel_draft(cx);
    }

    pub fn delete_comment(&mut self, id: &str, cx: &mut Context<Self>) {
        self.selection = None;
        self.comments.retain(|comment| comment.id != id);
        self.rebuild_items();
        cx.notify();
    }

    /// Review comments are the composer draft's; this is what the composer
    /// task reads.
    pub fn review_comments(&self) -> &[ReviewCommentContext] {
        &self.comments
    }
}

/// `annotationSide`.
pub fn annotation_side(range: &SelectedLineRange) -> SelectionSide {
    if range.end_side.or(range.side) == Some(SelectionSide::Deletions) {
        SelectionSide::Deletions
    } else {
        SelectionSide::Additions
    }
}

/// The row a line annotation attaches to.
pub fn row_index_for_line(rows: &[DiffRow], side: SelectionSide, number: usize) -> Option<usize> {
    rows.iter().position(|row| match row {
        DiffRow::Line(cell) => cell_matches(cell.kind, cell.side, cell.number, side, number),
        DiffRow::Split { left, right } => {
            let cell = match side {
                SelectionSide::Deletions => left,
                SelectionSide::Additions => right,
            };
            cell.is_some_and(|cell| cell.number == number && matches_kind(cell.kind, side))
        }
        _ => false,
    })
}

fn matches_kind(kind: LineKind, side: SelectionSide) -> bool {
    matches!(
        (kind, side),
        (LineKind::Context, _)
            | (LineKind::Addition, SelectionSide::Additions)
            | (LineKind::Deletion, SelectionSide::Deletions)
    )
}

fn cell_matches(
    kind: LineKind,
    cell_side: SelectionSide,
    cell_number: usize,
    side: SelectionSide,
    number: usize,
) -> bool {
    if cell_number != number {
        return false;
    }
    match kind {
        // Unified context rows carry the addition number.
        LineKind::Context => side == SelectionSide::Additions || cell_side == side,
        _ => cell_side == side,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::pierre::parse_patch_files::parse_patch_files;

    fn rows() -> Vec<DiffRow> {
        let file = parse_patch_files(
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n",
            None,
        )
        .remove(0)
        .files
        .remove(0);
        build_rows(&file, DiffStyle::Unified, &ExpandedHunks::default(), false)
    }

    #[test]
    fn annotation_rows_attach_to_the_commented_line() {
        let rows = rows();
        assert_eq!(
            row_index_for_line(&rows, SelectionSide::Additions, 2),
            Some(2)
        );
        assert_eq!(
            row_index_for_line(&rows, SelectionSide::Deletions, 2),
            Some(1)
        );
        assert_eq!(
            row_index_for_line(&rows, SelectionSide::Additions, 3),
            Some(3)
        );
        assert_eq!(row_index_for_line(&rows, SelectionSide::Additions, 9), None);
    }

    #[test]
    fn annotation_side_prefers_the_end_side() {
        let range = SelectedLineRange {
            start: 2,
            side: Some(SelectionSide::Deletions),
            end: 2,
            end_side: Some(SelectionSide::Additions),
        };
        assert_eq!(annotation_side(&range), SelectionSide::Additions);
        let range = SelectedLineRange {
            end_side: None,
            ..range
        };
        assert_eq!(annotation_side(&range), SelectionSide::Deletions);
    }

    #[test]
    fn settings_default_to_upstream() {
        let settings = DiffPanelSettings::default();
        assert!(settings.files_collapsed && settings.ignore_whitespace && settings.word_wrap);
        assert_eq!(settings.layout.style(), DiffStyle::Unified);
        assert!(!settings.file_tree_open);
    }
}
