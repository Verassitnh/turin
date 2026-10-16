//! Port of upstream/apps/web/src/components/pullRequest/PullRequestCodeTab.tsx: the pull
//! request's patch with the review written against it. Conversations already on the host sit
//! under the line they were written on, and a new comment joins the review being drafted rather
//! than being posted as it is typed.
//!
//! `@pierre/diffs` has no Rust equivalent, so the patch the host returned is parsed by
//! [`patch`] and laid out as one virtualized GPUI list ([`items`]): file headers, hunk
//! separators, lines (unified or split) and annotations. Lines are highlighted with the chat
//! markdown's tree-sitter highlighter.
//!
//! Not ported (see PORTING.md Deviations): sticky file headers, horizontal scrolling of long
//! lines (lines wrap by default), expanding the unmodified lines between hunks
//! (`loadDiffFiles`), range selection by dragging line numbers (a comment is on one line, which
//! is what upstream collapses a range to), the "Add to agent" action, the "Fix in a thread"
//! hand-off, the find panel, persisting the toolbar choices in client settings, and the file
//! tree's pierre icons / virtualization.

mod files_viewed;
pub mod items;
pub mod patch;
mod toolbar;
mod view;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, Context, Entity, HighlightStyle, Hsla,
    InteractiveElement as _, IntoElement, ListAlignment, ListOffset, ListState, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, StyledText, Subscription,
    Window, div, list, px, relative, rems,
};
use turin_contracts::pull_request::{
    PullRequestCommit, PullRequestDiffResult, PullRequestDiffSide, PullRequestReviewCommentDraft,
    PullRequestReviewPosition, PullRequestReviewThread,
};

use self::files_viewed::FilesViewed;
use self::items::{AnnotationKey, BuildInput, FileView, Item, build_items};
use self::patch::{
    FileChange, ParsedPatch, PatchFile, hide_whitespace_changes, parse_patch,
    resolve_diff_review_position,
};
use super::comment_form::segmented_group;
use super::detail_context::DetailContext;
use super::dropdown::dropdown;
use super::presentation_view::{
    PullRequestDiffStat, Spinner, pull_request_meta_line, with_tooltip,
};
use super::pull_request_detail_logic::infer_review_comment_fence_language;
use super::pull_request_diff_logic::{
    DiffFoldOverride, is_file_diff_collapsed, is_line_in_file_diff,
    toggle_file_diff_fold_for_viewed,
};
use super::pull_request_editing_logic::can_resolve_pull_request_thread;
use super::pull_request_file_order_logic::order_diff_files;
use super::pull_request_review_store::{
    PendingReviewComment, PullRequestReviewStore, pull_request_review_key,
};
use super::review_annotation::{
    DraftCommentCard, DraftCommentEvent, PendingReviewCommentCard, ReviewThreadCard,
    ThreadCardProps, review_position_anchor,
};
use super::review_form::review_store;
use super::view_model::{Loadable, PullRequestDetailSnapshot};
use crate::components::chat::markdown::code_highlight::highlight_code;
use crate::state::helpers::review_guide_tour::DiffRowKind;
use crate::theme::color::{MixSpace, color_mix, with_alpha};
use crate::theme::tokens::{TEXT_2XS, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{
    Button, ButtonSize, ButtonVariant, Icon, IconName, Toggle, ToggleSize, ToggleVariant, Tooltip,
};

/// Commits per press of "Show more" in the scope menu.
const COMMIT_PAGE_SIZE: usize = 10;
/// `@lg` container width: below it the toolbar folds its controls into one menu.
const WIDE_TOOLBAR: f32 = 512.;
/// Highlighting is skipped past this much source per file side.
const HIGHLIGHT_LIMIT: usize = 400 * 1024;
const LIST_OVERDRAW: f32 = 600.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLayout {
    Stacked,
    Split,
}

// ── Colors ──────────────────────────────────────────────────────────────────

/// The diff surface: `--code-background` with pierre's tinted lines.
#[derive(Clone)]
pub(crate) struct Palette {
    pub(crate) bg: Hsla,
    pub(crate) fg: Hsla,
    pub(crate) gutter_fg: Hsla,
    pub(crate) addition_line: Hsla,
    pub(crate) addition_gutter: Hsla,
    pub(crate) deletion_line: Hsla,
    pub(crate) deletion_gutter: Hsla,
    pub(crate) addition_bar: Hsla,
    pub(crate) deletion_bar: Hsla,
    pub(crate) separator_bg: Hsla,
    pub(crate) separator_fg: Hsla,
}

impl Palette {
    pub(crate) fn new(theme: &AppTheme) -> Self {
        let dark = theme.is_dark();
        let bg = theme.code_background;
        let fg = theme.code_foreground;
        let (line, gutter) = if dark { (0.84, 0.74) } else { (0.88, 0.78) };
        let mix = |base: Hsla, weight: f32| -> Hsla {
            color_mix(MixSpace::Oklab, bg, weight, base).into()
        };
        Self {
            bg,
            fg,
            gutter_fg: color_mix(MixSpace::Srgb, fg, 0.5, bg).into(),
            addition_line: mix(theme.success, line),
            addition_gutter: mix(theme.success, gutter),
            deletion_line: mix(theme.destructive, line),
            deletion_gutter: mix(theme.destructive, gutter),
            addition_bar: theme.success,
            deletion_bar: theme.destructive,
            separator_bg: color_mix(MixSpace::Srgb, bg, 0.97, fg).into(),
            separator_fg: color_mix(MixSpace::Srgb, fg, 0.5, bg).into(),
        }
    }

    pub(crate) fn line(&self, kind: DiffRowKind) -> Option<Hsla> {
        match kind {
            DiffRowKind::Addition => Some(self.addition_line),
            DiffRowKind::Deletion => Some(self.deletion_line),
            DiffRowKind::Context => None,
        }
    }

    pub(crate) fn gutter(&self, kind: DiffRowKind) -> Option<Hsla> {
        match kind {
            DiffRowKind::Addition => Some(self.addition_gutter),
            DiffRowKind::Deletion => Some(self.deletion_gutter),
            DiffRowKind::Context => None,
        }
    }

    pub(crate) fn bar(&self, kind: DiffRowKind) -> Option<Hsla> {
        match kind {
            DiffRowKind::Addition => Some(self.addition_bar),
            DiffRowKind::Deletion => Some(self.deletion_bar),
            DiffRowKind::Context => None,
        }
    }
}

// ── Highlighting ────────────────────────────────────────────────────────────

pub(crate) type LineTokens = Vec<(Range<usize>, Hsla)>;

/// Token colors per row of a file, aligned with `FileView::rows`.
pub(crate) fn row_tokens(view: &FileView, dark: bool) -> Vec<LineTokens> {
    let language = infer_review_comment_fence_language(&view.path);
    let meta = &view.file.meta;
    let side = |lines: &[String]| -> (Vec<usize>, Vec<(Range<usize>, Hsla)>) {
        let mut offsets = Vec::with_capacity(lines.len() + 1);
        let mut total = 0;
        for line in lines {
            offsets.push(total);
            total += line.len();
        }
        offsets.push(total);
        if total == 0 || total > HIGHLIGHT_LIMIT || language == "text" {
            return (offsets, Vec::new());
        }
        let text = lines.concat();
        let mut tokens = highlight_code(&text, &language, dark).tokens.clone();
        tokens.sort_by_key(|(range, _)| range.start);
        (offsets, tokens)
    };
    let (new_offsets, new_tokens) = side(&meta.addition_lines);
    let (old_offsets, old_tokens) = side(&meta.deletion_lines);
    let slice =
        |offsets: &[usize], tokens: &[(Range<usize>, Hsla)], line: usize, lines: &[String]| {
            let start = offsets[line];
            let text_len = lines[line].trim_end_matches(['\n', '\r']).len();
            let end = start + text_len;
            let first = tokens.partition_point(|(range, _)| range.end <= start);
            let mut out: LineTokens = Vec::new();
            for (range, color) in &tokens[first..] {
                if range.start >= end {
                    break;
                }
                let from = range.start.max(start) - start;
                let to = range.end.min(end) - start;
                if to > from && out.last().is_none_or(|(last, _)| last.end <= from) {
                    out.push((from..to, *color));
                }
            }
            out
        };
    let (mut added, mut removed) = (0, 0);
    view.rows
        .iter()
        .map(|row| match row.kind {
            DiffRowKind::Context => {
                let tokens = slice(&new_offsets, &new_tokens, added, &meta.addition_lines);
                added += 1;
                removed += 1;
                tokens
            }
            DiffRowKind::Addition => {
                let tokens = slice(&new_offsets, &new_tokens, added, &meta.addition_lines);
                added += 1;
                tokens
            }
            DiffRowKind::Deletion => {
                let tokens = slice(&old_offsets, &old_tokens, removed, &meta.deletion_lines);
                removed += 1;
                tokens
            }
        })
        .collect()
}

/// Tabs become four spaces; token ranges move with the text.
fn expand_tabs(text: &str, tokens: &[(Range<usize>, Hsla)]) -> (String, LineTokens) {
    if !text.contains('\t') {
        return (text.to_owned(), tokens.to_vec());
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut map = vec![0usize; text.len() + 1];
    for (index, ch) in text.char_indices() {
        map[index] = out.len();
        if ch == '\t' {
            out.push_str("    ");
        } else {
            out.push(ch);
        }
    }
    map[text.len()] = out.len();
    // Interior bytes of a multi-byte char map to the char's start.
    let mut last = 0;
    for slot in map.iter_mut() {
        if *slot == 0 && last != 0 {
            *slot = last;
        }
        last = *slot;
    }
    let tokens = tokens
        .iter()
        .map(|(range, color)| (map[range.start]..map[range.end.min(text.len())], *color))
        .collect();
    (out, tokens)
}

pub(crate) fn code_text(text: &str, tokens: &[(Range<usize>, Hsla)], fg: Hsla) -> AnyElement {
    let (text, tokens) = expand_tabs(text, tokens);
    let highlights: Vec<(Range<usize>, HighlightStyle)> = tokens
        .into_iter()
        .map(|(range, color)| {
            (
                range,
                HighlightStyle {
                    color: Some(color),
                    ..Default::default()
                },
            )
        })
        .collect();
    div()
        .text_color(fg)
        .child(StyledText::new(text).with_highlights(highlights))
        .into_any_element()
}

// ── Parsing ─────────────────────────────────────────────────────────────────

/// What a slice of the diff turned into.
#[derive(Clone)]
enum Parsed {
    Empty,
    Files(Arc<Vec<FileView>>),
    Raw(Vec<(String, String)>),
}

#[derive(Clone, PartialEq, Eq)]
struct ParseKey {
    commit: Option<String>,
    ptr: usize,
    len: usize,
    edge: u64,
    ignore_whitespace: bool,
}

fn parse_key(commit: &Option<String>, patch: &str, ignore_whitespace: bool) -> ParseKey {
    // The patch is held in one allocation per answer, so its address and ends say whether it is
    // the one parsed already without hashing megabytes every frame.
    let edge = patch
        .bytes()
        .take(256)
        .chain(patch.bytes().rev().take(256))
        .fold(0u64, |acc, byte| {
            acc.wrapping_mul(31).wrapping_add(u64::from(byte))
        });
    ParseKey {
        commit: commit.clone(),
        ptr: patch.as_ptr() as usize,
        len: patch.len(),
        edge,
        ignore_whitespace,
    }
}

fn parse_files(patch: &str, ignore_whitespace: bool) -> Parsed {
    match parse_patch(patch) {
        None => Parsed::Empty,
        Some(ParsedPatch::Raw { reason, text }) => Parsed::Raw(vec![(reason, text)]),
        Some(ParsedPatch::Files(files)) => {
            // Ordered within a slice rather than across them: ordering the accumulated set would
            // let a late slice push a file the reader is part way through further down.
            let orderable: Vec<_> = files.iter().map(PatchFile::orderable).collect();
            let order = order_diff_files(&orderable);
            let mut remaining: Vec<Option<PatchFile>> = files.into_iter().map(Some).collect();
            let mut ordered = Vec::with_capacity(remaining.len());
            for wanted in &order {
                if let Some(slot) = remaining
                    .iter_mut()
                    .find(|slot| slot.as_ref().is_some_and(|file| file.path() == wanted.path))
                {
                    ordered.extend(slot.take());
                }
            }
            ordered.extend(remaining.into_iter().flatten());
            let views = ordered
                .into_iter()
                .map(|file| {
                    let file = if ignore_whitespace {
                        hide_whitespace_changes(&file)
                    } else {
                        file
                    };
                    FileView::new(file)
                })
                .collect();
            Parsed::Files(Arc::new(views))
        }
    }
}

// ── Tab state ───────────────────────────────────────────────────────────────

/// Everything pinned to one line of one file: what is already there, and what is being added.
#[derive(Default, Clone)]
struct AnnotationGroup {
    threads: Vec<PullRequestReviewThread>,
    pending: Vec<PendingReviewComment>,
    draft: bool,
}

struct Draft {
    file_key: String,
    path: String,
    old_path: Option<String>,
    position: PullRequestReviewPosition,
    card: Entity<DraftCommentCard>,
    _subscription: Subscription,
}

pub struct CodeTab {
    pub ctx: DetailContext,
    commit: Option<String>,
    visible_commits: usize,
    fold_override: Option<DiffFoldOverride>,
    toggled: HashSet<String>,
    layout: DiffLayout,
    wrap: bool,
    ignore_whitespace: bool,
    tree_open: bool,
    orphans_open: bool,
    draft: Option<Draft>,
    toolbar_width: f32,
    /// A file another tab asked to show, answered once it has arrived.
    reveal: Option<String>,

    parsed: Parsed,
    parse_key: Option<ParseKey>,
    raw_files: Arc<Vec<FileView>>,
    groups: HashMap<AnnotationKey, AnnotationGroup>,
    placed: HashSet<String>,
    items: Arc<Vec<Item>>,
    list: ListState,
    dirty: bool,
    viewed: FilesViewed,
    flush_generation: u64,
    review: Entity<PullRequestReviewStore>,
    thread_cards: HashMap<String, Entity<ReviewThreadCard>>,
    tokens: RefCell<HashMap<String, Rc<Vec<LineTokens>>>>,
    requested: HashSet<Option<String>>,
    _subscriptions: Vec<Subscription>,
}

impl CodeTab {
    pub fn new(ctx: DetailContext, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let review = review_store(cx);
        let subscriptions = vec![
            cx.observe(&ctx.store, |this, _, cx| {
                this.dirty = true;
                cx.notify();
            }),
            cx.observe(&review, |this, _, cx| {
                this.dirty = true;
                cx.notify();
            }),
        ];
        let mut this = Self {
            ctx,
            commit: None,
            visible_commits: COMMIT_PAGE_SIZE,
            fold_override: None,
            toggled: HashSet::new(),
            layout: DiffLayout::Stacked,
            wrap: true,
            ignore_whitespace: false,
            tree_open: false,
            orphans_open: false,
            draft: None,
            toolbar_width: 0.,
            reveal: None,
            parsed: Parsed::Empty,
            parse_key: None,
            raw_files: Arc::new(Vec::new()),
            groups: HashMap::new(),
            placed: HashSet::new(),
            items: Arc::new(Vec::new()),
            list: ListState::new(0, ListAlignment::Top, px(LIST_OVERDRAW)),
            dirty: true,
            viewed: FilesViewed::default(),
            flush_generation: 0,
            review,
            thread_cards: HashMap::new(),
            tokens: RefCell::new(HashMap::new()),
            requested: HashSet::new(),
            _subscriptions: subscriptions,
        };
        // `TURIN_CODE_LAYOUT=split` (dev/screenshots).
        if std::env::var("TURIN_CODE_LAYOUT").is_ok_and(|value| value == "split") {
            this.layout = DiffLayout::Split;
        }
        this.request_diff(cx);
        this
    }

    // ── Commands the panel and the toolbar use ──

    /// The summary's commit row: scope the diff to one commit (`None` is the whole change).
    pub fn set_commit(&mut self, commit: Option<String>, cx: &mut Context<Self>) {
        if self.commit == commit {
            return;
        }
        self.commit = commit;
        self.reset_scope();
        self.request_diff(cx);
        cx.notify();
    }

    pub fn commit(&self) -> Option<&str> {
        self.commit.as_deref()
    }

    /// The panel's refresh button: drop what was accumulated and read the diff again.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.parse_key = None;
        self.requested.remove(&self.commit);
        self.request_diff(cx);
        if let Some(source) = self.ctx.source(cx) {
            // The ticks start over with it: a push since the last read can have made some stale.
            source.refresh_detail(&self.ctx.reference, cx);
        }
        self.dirty = true;
        cx.notify();
    }

    /// A file another tab asked to show (the guide's file links, the file tree).
    pub fn reveal_file(&mut self, path: &str, cx: &mut Context<Self>) {
        self.reveal = Some(path.to_owned());
        self.dirty = true;
        cx.notify();
    }

    fn reset_scope(&mut self) {
        self.draft = None;
        self.toggled.clear();
        self.fold_override = None;
        self.visible_commits = COMMIT_PAGE_SIZE;
        self.orphans_open = false;
        self.parse_key = None;
        self.dirty = true;
    }

    fn request_diff(&mut self, cx: &mut Context<Self>) {
        if !self.requested.insert(self.commit.clone()) {
            return;
        }
        if let Some(source) = self.ctx.source(cx) {
            source.load_diff(&self.ctx.reference, self.commit.clone(), cx);
        }
    }

    fn toggle_file(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.toggled.remove(key) {
            self.toggled.insert(key.to_owned());
        }
        self.dirty = true;
        cx.notify();
    }

    /// The tick and the fold are one gesture: clearing a file puts it away, un-clearing brings
    /// it back. Folding is still held as the reader's difference from the toolbar's default
    /// rather than derived from what has been ticked, so folding everything ticks nothing off.
    fn set_file_viewed(&mut self, key: &str, path: &str, viewed: bool, cx: &mut Context<Self>) {
        self.viewed.press(path, viewed);
        self.toggled =
            toggle_file_diff_fold_for_viewed(key, viewed, self.fold_override, &self.toggled);
        self.dirty = true;
        cx.notify();
        // Presses gather for FLUSH_DELAY_MS so ticking down a list costs one request.
        self.flush_generation += 1;
        let generation = self.flush_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(
                    super::pull_request_files_viewed_logic::FLUSH_DELAY_MS,
                ))
                .await;
            this.update(cx, |this, cx| {
                if this.flush_generation == generation {
                    this.flush_viewed(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn flush_viewed(&mut self, cx: &mut Context<Self>) {
        let batch = self.viewed.flush();
        if batch.is_empty() {
            return;
        }
        if let Some(source) = self.ctx.source(cx) {
            source.set_files_viewed(&self.ctx.reference, batch.clone(), cx);
        }
        self.viewed.acknowledge(&batch);
    }

    fn toggle_all_files(&mut self, all_collapsed: bool, cx: &mut Context<Self>) {
        // Held as an override of the default rather than as the file keys on screen: a diff
        // that is still paging would otherwise bring its next slice in folded, moments after
        // the reader asked for everything to be open.
        self.fold_override = Some(if all_collapsed {
            DiffFoldOverride::Expanded
        } else {
            DiffFoldOverride::Folded
        });
        self.toggled.clear();
        self.dirty = true;
        cx.notify();
    }

    fn begin_comment(
        &mut self,
        file: usize,
        row: usize,
        side: PullRequestDiffSide,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.files().and_then(|files| files.get(file).cloned()) else {
            return;
        };
        let Some(line) = view.rows.get(row).and_then(|row| match side {
            PullRequestDiffSide::Left => row.old_line,
            PullRequestDiffSide::Right => row.new_line,
        }) else {
            return;
        };
        let Some(position) = resolve_diff_review_position(&view.file, line, side) else {
            return;
        };
        let previous = view.file.previous_path().to_owned();
        let label = format!("{}:{}", view.path, review_position_anchor(&position).0);
        let id = SharedString::from(format!("draft-{}-{line}", view.key));
        let card = cx.new(|cx| DraftCommentCard::new(id, label, window, cx));
        let subscription = cx.subscribe(&card, |this, _, event: &DraftCommentEvent, cx| {
            match event {
                DraftCommentEvent::Cancel => this.draft = None,
                DraftCommentEvent::Submit(body) => this.add_draft_comment(body.clone(), cx),
            }
            this.dirty = true;
            cx.notify();
        });
        self.draft = Some(Draft {
            file_key: view.key.clone(),
            path: view.path.clone(),
            old_path: (previous != view.path).then_some(previous),
            position,
            card,
            _subscription: subscription,
        });
        self.dirty = true;
        cx.notify();
    }

    fn add_draft_comment(&mut self, body: String, cx: &mut Context<Self>) {
        let Some(draft) = self.draft.take() else {
            return;
        };
        let (Ok(path), old_path) = (
            turin_contracts::TrimmedNonEmptyString::new(&draft.path),
            draft
                .old_path
                .and_then(|old| turin_contracts::TrimmedNonEmptyString::new(old).ok()),
        ) else {
            return;
        };
        let key = pull_request_review_key(&self.ctx.reference);
        self.review.update(cx, |store, _| {
            let id = store.next_pending_review_comment_id();
            store.add_comment(
                &key,
                PendingReviewComment {
                    id,
                    draft: PullRequestReviewCommentDraft {
                        path,
                        old_path,
                        position: draft.position,
                        body,
                    },
                },
            );
        });
    }

    fn remove_pending(&mut self, id: &str, cx: &mut Context<Self>) {
        let key = pull_request_review_key(&self.ctx.reference);
        self.review
            .update(cx, |store, _| store.remove_comment(&key, id));
    }

    // ── Derived state ──

    fn files(&self) -> Option<Arc<Vec<FileView>>> {
        match &self.parsed {
            Parsed::Files(files) => Some(files.clone()),
            _ => None,
        }
    }

    fn collapsed(&self, key: &str) -> bool {
        is_file_diff_collapsed(key, self.fold_override, &self.toggled)
    }

    /// Reads the snapshot, re-parses when the patch moved, and rebuilds the list when anything
    /// it shows has changed.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let store = self.ctx.store.clone();
        let reference = self.ctx.reference.clone();
        let mut leave_commit = false;
        {
            let Some(snapshot) = store.read(cx).detail(&reference) else {
                return;
            };
            self.viewed.sync(&snapshot.files_viewed);
            let result = match snapshot.diffs.get(&self.commit) {
                Some(Loadable::Ready(result)) => Some(result),
                _ => None,
            };
            match result {
                Some(result) => {
                    let key = parse_key(&self.commit, &result.patch, self.ignore_whitespace);
                    if self.parse_key.as_ref() != Some(&key) {
                        self.parse_key = Some(key);
                        self.parsed = parse_files(&result.patch, self.ignore_whitespace);
                        self.tokens.borrow_mut().clear();
                        self.dirty = true;
                    }
                }
                None => {
                    if self.parse_key.take().is_some() {
                        self.parsed = Parsed::Empty;
                        self.dirty = true;
                    }
                }
            }
            // A rebase or a force-push can take the scoped commit out of the change; the scope
            // then goes back to the whole change rather than sitting under a name nothing matches.
            if let (Some(commit), Loadable::Ready(detail)) = (&self.commit, &snapshot.detail)
                && !detail
                    .commits
                    .iter()
                    .any(|entry| entry.oid.as_str() == commit)
            {
                leave_commit = true;
            } else if self.dirty {
                self.rebuild(snapshot, result, cx);
                self.dirty = false;
            }
        }
        if leave_commit {
            self.commit = None;
            self.reset_scope();
            self.request_diff(cx);
        }
    }

    fn rebuild(
        &mut self,
        snapshot: &PullRequestDetailSnapshot,
        result: Option<&PullRequestDiffResult>,
        cx: &App,
    ) {
        let files = self.files();
        let files_ref: &[FileView] = files.as_deref().map_or(&[], Vec::as_slice);
        // Omitted files report the host's counts rather than a zero the patch never had.
        let omitted: HashMap<&str, (f64, f64)> = result
            .and_then(|result| result.omitted_file_stats.as_ref())
            .map(|stats| {
                stats
                    .iter()
                    .map(|stat| (stat.path.as_str(), (stat.additions, stat.deletions)))
                    .collect()
            })
            .unwrap_or_default();
        let _ = omitted;

        // Placing a conversation takes more than its file being in the diff: its line has to
        // fall inside a hunk that was rendered. A commit's diff counts lines within that commit
        // while a review comment is anchored to the head diff, so under a commit scope every
        // conversation is listed rather than placed.
        self.placed.clear();
        self.groups.clear();
        let mut annotated: HashSet<AnnotationKey> = HashSet::new();
        if self.commit.is_none()
            && let Loadable::Ready(detail) = &snapshot.detail
        {
            for (index, view) in files_ref.iter().enumerate() {
                let ranges = view.file.hunk_ranges();
                for thread in &detail.review_threads {
                    if thread.path.as_str() == view.path
                        && let Some(line) = thread.line
                        && is_line_in_file_diff(&ranges, thread.side, line.get() as i64)
                    {
                        self.placed.insert(thread.id.as_str().to_owned());
                        let key = (index, thread.side, line.get() as i64);
                        self.groups
                            .entry(key)
                            .or_default()
                            .threads
                            .push(thread.clone());
                        annotated.insert(key);
                    }
                }
            }
            // Pending comments anchor to the head diff exactly like host threads do.
            let pending = self
                .review
                .read(cx)
                .pending_review_comments(&self.ctx.reference)
                .to_vec();
            for comment in pending {
                let Some(index) = files_ref
                    .iter()
                    .position(|view| view.path == comment.draft.path.as_str())
                else {
                    continue;
                };
                let (line, side) = review_position_anchor(&comment.draft.position);
                let key = (index, side, line);
                self.groups.entry(key).or_default().pending.push(comment);
                annotated.insert(key);
            }
        }
        if let Some(draft) = &self.draft
            && let Some(index) = files_ref.iter().position(|view| view.key == draft.file_key)
        {
            let (line, side) = review_position_anchor(&draft.position);
            let key = (index, side, line);
            self.groups.entry(key).or_default().draft = true;
            annotated.insert(key);
        }
        let footer = result.is_some_and(|result| result.next_cursor.is_some());
        let items = build_items(&BuildInput {
            files: files_ref,
            collapsed: &|key| is_file_diff_collapsed(key, self.fold_override, &self.toggled),
            split: self.layout == DiffLayout::Split,
            annotated: &annotated,
            footer,
        });
        let old = self.items.len();
        self.items = Arc::new(items);
        self.list.splice(0..old, self.items.len());
        // Answered once the requested file has arrived.
        if let Some(path) = self.reveal.clone()
            && let Some(index) = files_ref.iter().position(|view| view.path == path)
        {
            let key = files_ref[index].key.clone();
            if self.collapsed(&key) {
                self.toggled = toggle_file_diff_fold_for_viewed(
                    &key,
                    false,
                    self.fold_override,
                    &self.toggled,
                );
                self.reveal = Some(path);
                self.dirty = true;
                return;
            }
            self.reveal = None;
            if let Some(item) = self
                .items
                .iter()
                .position(|item| matches!(item, Item::FileHeader { file } if *file == index))
            {
                self.list.scroll_to(ListOffset {
                    item_ix: item,
                    offset_in_item: px(0.),
                });
            }
        }
    }
}
