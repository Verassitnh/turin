//! Port of upstream/apps/web/src/components/pullRequest/guide/{GuideTour,GuideStop,GuideRail,
//! GuideNotes,GuideProse,GuideExcerpt,GuideFileDiff}.tsx and guideRunStore.ts (progress) plus
//! guide/useGuideDiffFiles.ts: the guide as a tour — a rail of stops beside the stop being read.
//!
//! Progress is kept per guide head for as long as the app is open ([`GuideProgressStore`], a GPUI
//! global), so the same guide in another panel opens where the reader is; a new head starts it
//! over. What the reader says about the change goes into the review: line comments queue with the
//! Code tab's (the shared review store), and the last stop calls [`GuideTour::on_finish`].
//!
//! Not ported (see PORTING.md Deviations): excerpts expanding into the unchanged lines around
//! them (`loadDiffFiles`), dragging across line numbers to select a range (a comment is on one
//! line), "Ask an agent" about a stop or lines (no agent seam in `DetailNav`), the `J`/`K`
//! shortcut when focus is outside the tour (keys work while the tour holds focus), sticky rail,
//! `DiffWorkerPoolProvider`.
//!
//! Port of upstream/apps/web/src/components/pullRequest/guide/GuideTour.tsx.
//! Port of upstream/apps/web/src/components/pullRequest/guide/GuideStop.tsx.
//! Port of upstream/apps/web/src/components/pullRequest/guide/GuideRail.tsx.
//! Port of upstream/apps/web/src/components/pullRequest/guide/GuideNotes.tsx.
//! Port of upstream/apps/web/src/components/pullRequest/guide/GuideProse.tsx.
//! Port of upstream/apps/web/src/components/pullRequest/guide/GuideFileDiff.tsx.
//! Port of upstream/apps/web/src/components/pullRequest/guide/useGuideDiffFiles.ts (file read in guide_excerpt_expansion.rs).

use crate::theme::color::solid_over;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, FocusHandle, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Pixels, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
    div, point, px, relative, rems,
};
use turin_contracts::pull_request::{
    PullRequestDiffFileChangeType, PullRequestDiffSide, PullRequestReviewCommentDraft,
    PullRequestReviewPosition,
};
use turin_contracts::pull_request_review_guide::{
    PullRequestReviewGuide, PullRequestReviewGuideAttentionKind, PullRequestReviewGuideExcerpt,
    PullRequestReviewGuideExcerptSide, PullRequestReviewGuideStop,
};

use super::code_tab::items::FileView;
use super::code_tab::patch::{
    FileChange, ParsedPatch, PatchFile, parse_patch, resolve_diff_review_position,
};
use super::code_tab::{LineTokens, Palette, code_text, row_tokens};
use super::comment_body::pull_request_markdown;
use super::detail_context::DetailContext;
use super::guide_excerpt_expansion::{Expansion, hidden_above, plan_excerpt_expansion};
use super::presentation_view::PullRequestDiffStat;
use super::pull_request_review_store::{
    PendingReviewComment, PullRequestReviewStore, pull_request_review_key,
};
use super::review_annotation::{
    DraftCommentCard, DraftCommentEvent, PendingReviewCommentCard, review_position_anchor,
};
use super::review_form::review_store;
use super::view_model::Loadable;
use crate::state::helpers::review_guide_tour::{
    DiffRow, DiffRowKind, ExcerptRange, LetteredNote, excerpt_rows, letter_stop_notes,
    step_tour_position, stop_file_paths, stop_subline,
};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, RADIUS_MD, TEXT_2XS, TEXT_3XS, TEXT_LG, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{
    Badge, BadgeSize, BadgeVariant, Button, ButtonSize, ButtonVariant, Icon, IconName, Kbd, Tooltip,
};

/// The last stop's "Finish review".
pub type FinishHandler = Rc<dyn Fn(&mut App)>;
type SelectStop = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// `@min-[56rem]/guide`: the rail sits beside the stop from this width.
const RAIL_BESIDE: f32 = 896.;
/// `@min-[90rem]/guide`: the stop's notes take a column of their own.
const NOTES_BESIDE: f32 = 1440.;
/// `@max-[30rem]/guide`: the J / K hint is dropped below this.
const HINT_WIDTH: f32 = 480.;
/// Whole-file diffs inside a stop are cut here; the rest is one press away in the Code tab.
const FILE_DIFF_ROW_LIMIT: usize = 500;

// ── Keys and progress ───────────────────────────────────────────────────────

use super::guide_run_store::GuideRunStore;
pub use super::guide_run_store::{GuideProgress, guide_progress_key, guide_run_key};

/// The progress half of `useGuideRunStore`.
pub type GuideProgressStore = GuideRunStore;

fn progress_store(cx: &mut App) -> Entity<GuideProgressStore> {
    GuideRunStore::global(cx)
}

/// `ATTENTION_LABELS`.
pub fn attention_label(kind: PullRequestReviewGuideAttentionKind) -> &'static str {
    match kind {
        PullRequestReviewGuideAttentionKind::Tradeoff => "A tradeoff to weigh",
        PullRequestReviewGuideAttentionKind::Contract => "A contract to check",
        PullRequestReviewGuideAttentionKind::Product => "A product call",
        PullRequestReviewGuideAttentionKind::Risk => "A risk to judge",
        PullRequestReviewGuideAttentionKind::Accessibility => "An accessibility call",
    }
}

// ── Alert ───────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AlertVariant {
    Default,
    Info,
    Warning,
}

/// `ui/alert.tsx`: `rounded-xl border px-3.5 py-3 text-sm`, tinted per variant.
pub fn alert(
    variant: AlertVariant,
    icon: AnyElement,
    content: AnyElement,
    action: Option<AnyElement>,
    cx: &App,
) -> gpui_kit::Div {
    let theme = cx.app_theme();
    let (border, bg): (Hsla, Hsla) = match variant {
        AlertVariant::Default => (
            theme.border,
            if theme.is_dark() {
                with_alpha(theme.input, 0.32).into()
            } else {
                with_alpha(theme.background, 0.).into()
            },
        ),
        AlertVariant::Info => (
            with_alpha(theme.info, 0.32).into(),
            with_alpha(theme.info, 0.04).into(),
        ),
        AlertVariant::Warning => (
            with_alpha(theme.warning, 0.32).into(),
            theme.warning_surface,
        ),
    };
    div()
        .flex()
        .items_start()
        .gap_2()
        .rounded(px(14.))
        .border_1()
        .border_color(border)
        .bg(bg)
        .px(px(14.))
        .py_3()
        .text_size(TEXT_SM.size)
        .line_height(TEXT_SM.line_height)
        .text_color(if variant == AlertVariant::Warning {
            theme.warning_foreground
        } else {
            theme.card_foreground
        })
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .size_4()
                .items_center()
                .justify_center()
                .child(icon),
        )
        .child(
            div()
                .flex()
                .min_w_0()
                .flex_1()
                .flex_col()
                .gap(px(2.))
                .child(content),
        )
        .children(action.map(|action| div().flex().flex_shrink_0().items_center().child(action)))
}

// ── Tour ────────────────────────────────────────────────────────────────────

/// The notice above the stop: a run in progress, a failure, or a guide older than the branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuideBanner {
    pub running: bool,
    pub message: String,
    /// Whether "Rewrite" is offered.
    pub can_write: bool,
}

struct Draft {
    excerpt: usize,
    path: String,
    old_path: Option<String>,
    position: PullRequestReviewPosition,
    card: Entity<DraftCommentCard>,
    _subscription: Subscription,
}

/// What the diff holds for the open stop's files.
struct StopFiles {
    files: HashMap<String, Rc<FileView>>,
    loading: bool,
    error: Option<String>,
}

pub struct GuideTour {
    ctx: DetailContext,
    guide: Arc<PullRequestReviewGuide>,
    banner: Option<GuideBanner>,
    can_comment: bool,
    active: bool,
    progress: Entity<GuideProgressStore>,
    review: Entity<PullRequestReviewStore>,
    rail_open: bool,
    width: f32,
    scroll: ScrollHandle,
    focus: FocusHandle,
    draft: Option<Draft>,
    open_files: HashSet<String>,
    /// How far each excerpt's bars have opened, by stop and excerpt.
    expansions: HashMap<(usize, usize), Expansion>,
    /// Paths whose whole file has been asked for.
    requested_contents: HashSet<String>,
    on_finish: Option<FinishHandler>,
    /// Parsed diff by the patch it came from.
    parsed: Option<(usize, usize, Vec<Rc<FileView>>)>,
    requested_diff: bool,
    requested_cursors: HashSet<String>,
    tokens: std::cell::RefCell<HashMap<String, Rc<Vec<LineTokens>>>>,
    _subscriptions: Vec<Subscription>,
}

impl GuideTour {
    pub fn new(
        ctx: DetailContext,
        guide: PullRequestReviewGuide,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let progress = progress_store(cx);
        let review = review_store(cx);
        let subscriptions = vec![
            cx.observe(&ctx.store, |_, _, cx| cx.notify()),
            cx.observe(&progress, |_, _, cx| cx.notify()),
            cx.observe(&review, |_, _, cx| cx.notify()),
        ];
        Self {
            ctx,
            guide: Arc::new(guide),
            banner: None,
            can_comment: false,
            active: false,
            progress,
            review,
            rail_open: false,
            width: 0.,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            draft: None,
            open_files: HashSet::new(),
            expansions: HashMap::new(),
            requested_contents: HashSet::new(),
            on_finish: None,
            parsed: None,
            requested_diff: false,
            requested_cursors: HashSet::new(),
            tokens: Default::default(),
            _subscriptions: subscriptions,
        }
    }

    /// The last stop's "Finish review": the panel opens the review composer.
    pub fn on_finish(mut self, handler: impl Fn(&mut App) + 'static) -> Self {
        self.on_finish = Some(Rc::new(handler));
        self
    }

    pub fn set_finish(&mut self, handler: Option<FinishHandler>) {
        self.on_finish = handler;
    }

    /// Takes the tab's newest answer; only a change repaints.
    pub fn set_guide(
        &mut self,
        guide: &PullRequestReviewGuide,
        banner: Option<GuideBanner>,
        can_comment: bool,
        active: bool,
        cx: &mut Context<Self>,
    ) {
        let mut changed = false;
        if *self.guide != *guide {
            self.guide = Arc::new(guide.clone());
            self.draft = None;
            // A new head reads a new diff: what was opened belonged to the old one.
            self.expansions.clear();
            self.requested_contents.clear();
            changed = true;
        }
        if self.banner != banner {
            self.banner = banner;
            changed = true;
        }
        if self.can_comment != can_comment || self.active != active {
            self.can_comment = can_comment;
            self.active = active;
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    fn key(&self) -> String {
        guide_progress_key(
            &guide_run_key(&self.ctx.environment_id, &self.ctx.reference),
            self.guide.head_sha.as_str(),
        )
    }

    fn position(&self, cx: &App) -> usize {
        let count = self.guide.stops.len();
        self.progress
            .read(cx)
            .get(&self.key())
            .position
            .min(count.saturating_sub(1))
    }

    fn go_to(&mut self, next: usize, cx: &mut Context<Self>) {
        let position = self.position(cx);
        let key = self.key();
        // Leaving a stop for a later one is reading it; jumping back is not.
        self.progress.update(cx, |store, _| {
            store.set(&key, |current| {
                let mut seen = current.seen.clone();
                if next > position {
                    seen.insert(position);
                }
                GuideProgress {
                    position: next,
                    seen,
                }
            })
        });
        self.rail_open = false;
        self.draft = None;
        // A new stop starts at its top rather than wherever the last one was scrolled to.
        self.scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    fn step(&mut self, delta: i64, cx: &mut Context<Self>) {
        let position = self.position(cx) as i64;
        let next = step_tour_position(position, delta, self.guide.stops.len() as i64);
        self.go_to(next as usize, cx);
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        let position = self.position(cx);
        let key = self.key();
        self.progress.update(cx, |store, _| {
            store.set(&key, |current| {
                let mut seen = current.seen.clone();
                seen.insert(position);
                GuideProgress {
                    position: current.position,
                    seen,
                }
            })
        });
        if let Some(finish) = self.on_finish.clone() {
            finish(cx);
        }
    }

    // ── The diff the stop reads ──

    /// The pull request's parsed file diffs, read through the same diff the Code tab uses. Paging
    /// stops once every wanted path has arrived: a stop's excerpts are usually in the first page,
    /// and a large change should not be read whole to show one of them.
    fn stop_files(&mut self, wanted: &[&str], cx: &mut Context<Self>) -> StopFiles {
        if !self.requested_diff {
            self.requested_diff = true;
            if let Some(source) = self.ctx.source(cx) {
                source.load_diff(&self.ctx.reference, None, cx);
            }
        }
        let mut out = StopFiles {
            files: HashMap::new(),
            loading: false,
            error: None,
        };
        // The answer holds a whole patch, so it is read in place rather than copied per frame.
        let mut next_cursor: Option<String> = None;
        let mut missing = false;
        {
            let store = self.ctx.store.clone();
            let result = store
                .read(cx)
                .detail(&self.ctx.reference)
                .and_then(|snapshot| snapshot.diffs.get(&None));
            match result {
                None | Some(Loadable::Pending) => out.loading = true,
                Some(Loadable::Failed(error)) => out.error = Some(error.clone()),
                Some(Loadable::Ready(result)) => {
                    let identity = (result.patch.as_ptr() as usize, result.patch.len());
                    if self.parsed.as_ref().map(|(ptr, len, _)| (*ptr, *len)) != Some(identity) {
                        let files = match parse_patch(&result.patch) {
                            Some(ParsedPatch::Files(files)) => files
                                .into_iter()
                                .map(|file| Rc::new(FileView::new(file)))
                                .collect(),
                            _ => Vec::new(),
                        };
                        self.parsed = Some((identity.0, identity.1, files));
                        self.tokens.borrow_mut().clear();
                    }
                    if let Some((_, _, files)) = &self.parsed {
                        for view in files {
                            out.files.insert(view.path.clone(), view.clone());
                        }
                    }
                    missing = wanted.iter().any(|path| !out.files.contains_key(*path));
                    next_cursor = result.next_cursor.as_ref().map(|c| c.as_str().to_owned());
                }
            }
        }
        if let (true, Some(cursor)) = (missing, next_cursor) {
            out.loading = true;
            if self.requested_cursors.insert(cursor.clone())
                && let Some(source) = self.ctx.source(cx)
            {
                source.load_diff_page(&self.ctx.reference, None, &cursor, cx);
            }
        }
        out
    }

    /// A press on a bar around an excerpt: opens one more page of unchanged lines on that side,
    /// and reads the whole file the first time (`loadDiffFiles`).
    fn open_excerpt(
        &mut self,
        key: (usize, usize),
        up: bool,
        hidden: usize,
        file: &PatchFile,
        cx: &mut Context<Self>,
    ) {
        let current = self.expansions.get(&key).copied().unwrap_or_default();
        self.expansions.insert(
            key,
            if up {
                current.open_up(hidden)
            } else {
                current.open_down(hidden)
            },
        );
        if self.requested_contents.insert(file.name.clone())
            && let Some(source) = self.ctx.source(cx)
        {
            source.load_diff_file_contents(
                &self.ctx.reference,
                None,
                diff_file_change_type(file.change),
                file.prev_name.as_deref().unwrap_or(&file.name),
                &file.name,
                cx,
            );
        }
        cx.notify();
    }

    fn tokens_for(&self, view: &FileView, dark: bool) -> Rc<Vec<LineTokens>> {
        if let Some(tokens) = self.tokens.borrow().get(&view.key) {
            return tokens.clone();
        }
        let tokens = Rc::new(row_tokens(view, dark));
        self.tokens
            .borrow_mut()
            .insert(view.key.clone(), tokens.clone());
        tokens
    }

    // ── Commenting ──

    fn begin_comment(
        &mut self,
        excerpt: usize,
        view: &FileView,
        row: &DiffRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let side = if row.kind == DiffRowKind::Deletion {
            PullRequestDiffSide::Left
        } else {
            PullRequestDiffSide::Right
        };
        let Some(line) = (match side {
            PullRequestDiffSide::Left => row.old_line,
            PullRequestDiffSide::Right => row.new_line,
        }) else {
            return;
        };
        // Positions come from the whole file's diff, so a comment lands where the Code tab
        // would put it rather than on the excerpt's cut-down rows.
        let Some(position) = resolve_diff_review_position(&view.file, line, side) else {
            return;
        };
        let previous = view.file.previous_path().to_owned();
        let label = format!("{}:{}", view.path, review_position_anchor(&position).0);
        let id = SharedString::from(format!("guide-draft-{excerpt}-{line}"));
        let card = cx.new(|cx| DraftCommentCard::new(id, label, window, cx));
        let subscription = cx.subscribe(&card, |this, _, event: &DraftCommentEvent, cx| {
            match event {
                DraftCommentEvent::Cancel => this.draft = None,
                DraftCommentEvent::Submit(body) => this.add_comment(body.clone(), cx),
            }
            cx.notify();
        });
        self.draft = Some(Draft {
            excerpt,
            path: view.path.clone(),
            old_path: (previous != view.path).then_some(previous),
            position,
            card,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn add_comment(&mut self, body: String, cx: &mut Context<Self>) {
        let Some(draft) = self.draft.take() else {
            return;
        };
        let Ok(path) = turin_contracts::TrimmedNonEmptyString::new(&draft.path) else {
            return;
        };
        let old_path = draft
            .old_path
            .and_then(|old| turin_contracts::TrimmedNonEmptyString::new(old).ok());
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
}

// ── Pieces ──────────────────────────────────────────────────────────────────

/// `GuideNoteMarker`: the letter that ties a note to its line in an excerpt.
/// The contract's name for how a file changed.
fn diff_file_change_type(change: FileChange) -> PullRequestDiffFileChangeType {
    match change {
        FileChange::Change => PullRequestDiffFileChangeType::Change,
        FileChange::RenamePure => PullRequestDiffFileChangeType::RenamePure,
        FileChange::RenameChanged => PullRequestDiffFileChangeType::RenameChanged,
        FileChange::New => PullRequestDiffFileChangeType::New,
        FileChange::Deleted => PullRequestDiffFileChangeType::Deleted,
    }
}

fn note_marker(marker: &str, theme: &AppTheme) -> AnyElement {
    div()
        .flex()
        .flex_shrink_0()
        .size(px(18.))
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(solid_over(theme.info, 0.16, theme.background))
        .text_size(TEXT_3XS.size)
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.info_foreground)
        .child(SharedString::from(marker.to_owned()))
        .into_any_element()
}

fn prose(id: impl Into<ElementId>, text: &str) -> AnyElement {
    div()
        .child(pull_request_markdown(
            id,
            SharedString::from(text.to_owned()),
        ))
        .into_any_element()
}

/// `GuideNotes`: a stop's notes, lettered in the order the excerpts mark them.
fn notes_section(
    stop_index: usize,
    notes: &[LetteredNote<
        &turin_contracts::pull_request_review_guide::PullRequestReviewGuideNote,
    >],
    theme: &AppTheme,
) -> Option<AnyElement> {
    if notes.is_empty() {
        return None;
    }
    let mut list = div().flex().flex_col().gap_3();
    for LetteredNote { marker, note } in notes {
        let file = note
            .path
            .as_str()
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_owned();
        list = list.child(
            div()
                .flex()
                .min_w_0()
                .gap_2()
                .child(note_marker(marker, theme))
                .child(
                    div()
                        .flex()
                        .min_w_0()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_baseline()
                                .gap_x_2()
                                .text_size(TEXT_XS.size)
                                .child(
                                    div()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme.foreground)
                                        .child(SharedString::from(note.title.as_str().to_owned())),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .font_family(SharedString::from(theme.fonts.mono.clone()))
                                        .text_size(TEXT_2XS.size)
                                        .text_color(theme.muted_foreground)
                                        .child(SharedString::from(format!("{file}:{}", note.line))),
                                ),
                        )
                        .when(!note.body.trim().is_empty(), |this| {
                            this.child(
                                div()
                                    .text_size(TEXT_XS.size)
                                    .text_color(theme.muted_foreground)
                                    .child(prose(
                                        SharedString::from(format!(
                                            "guide-note-{stop_index}-{marker}"
                                        )),
                                        &note.body,
                                    )),
                            )
                        }),
                ),
        );
    }
    Some(
        div()
            .flex()
            .min_w_0()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .justify_between()
                    .gap_2()
                    .text_size(TEXT_XS.size)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child("Notes on this stop")
                    .child(
                        div()
                            .font_weight(FontWeight::NORMAL)
                            .text_color(theme.muted_foreground)
                            .child(if notes.len() == 1 {
                                "1 note".to_owned()
                            } else {
                                format!("{} notes", notes.len())
                            }),
                    ),
            )
            .child(list)
            .into_any_element(),
    )
}

/// `GuideRail`: where the reader is and which stops they have read past.
fn rail(
    stops: &[PullRequestReviewGuideStop],
    position: usize,
    seen: &BTreeSet<usize>,
    show_count: bool,
    on_select: SelectStop,
    theme: &AppTheme,
) -> AnyElement {
    let mut list = div().flex().flex_col().gap(px(2.));
    for (index, stop) in stops.iter().enumerate() {
        let active = position == index;
        let read = seen.contains(&index);
        let select = on_select.clone();
        let accent = theme.accent;
        let bullet = div()
            .flex()
            .flex_shrink_0()
            .size_5()
            .items_center()
            .justify_center()
            .rounded_full()
            .border_1()
            .border_color(if read {
                with_alpha(theme.success, 0.4).into()
            } else {
                theme.border
            })
            .text_size(TEXT_2XS.size)
            .text_color(if read {
                theme.success_foreground
            } else {
                theme.muted_foreground
            })
            .child(if read {
                Icon::new(IconName::Check)
                    .size(rems(0.75))
                    .color(theme.success_foreground)
                    .into_any_element()
            } else {
                SharedString::from((index + 1).to_string()).into_any_element()
            });
        list = list.child(
            div()
                .id(SharedString::from(format!("rail-{index}")))
                .flex()
                .w_full()
                .items_start()
                .gap_2()
                .rounded(RADIUS_MD)
                .px_2()
                .py(px(6.))
                .cursor_pointer()
                .when(active, |this| {
                    this.bg(theme.accent).text_color(theme.accent_foreground)
                })
                .when(!active, |this| {
                    this.hover(move |style| style.bg(with_alpha(accent, 0.5)))
                })
                .on_click(move |_, window, cx| select(index, window, cx))
                .child(bullet)
                .child(
                    div()
                        .flex()
                        .min_w_0()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .text_size(TEXT_SM.size)
                                .line_height(TEXT_SM.line_height)
                                .text_color(if active {
                                    theme.accent_foreground
                                } else {
                                    theme.foreground
                                })
                                .child(SharedString::from(stop.title.as_str().to_owned())),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(rems(0.375))
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .when(stop.attention.is_some(), |this| {
                                    this.child(
                                        Badge::new()
                                            .size(BadgeSize::Sm)
                                            .variant(BadgeVariant::Warning)
                                            .child("Look closely"),
                                    )
                                })
                                .child(SharedString::from(stop_subline(stop))),
                        ),
                ),
        );
    }
    div()
        .flex()
        .min_h_0()
        .flex_col()
        .gap_4()
        .when(show_count, |this| {
            this.child(
                div()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(format!(
                        "{} of {} read",
                        seen.len(),
                        stops.len()
                    ))),
            )
        })
        .child(list)
        .into_any_element()
}

/// The code face and line metrics the excerpt rows share.
struct Metrics {
    font: SharedString,
    size: Pixels,
    line: Pixels,
}

impl GuideTour {
    fn metrics(theme: &AppTheme) -> Metrics {
        let size = theme.fonts.size_code;
        Metrics {
            font: SharedString::from(theme.fonts.mono.clone()),
            size: px(size),
            line: px((size * 1.5).round().max(20.)),
        }
    }

    /// One row of an excerpt: gutter number, bar, code.
    #[allow(clippy::too_many_arguments)]
    fn diff_line(
        &self,
        id: SharedString,
        row: &DiffRow,
        tokens: &[(std::ops::Range<usize>, gpui_kit::Hsla)],
        gutter: Pixels,
        comment: Option<(usize, Rc<FileView>)>,
        metrics: &Metrics,
        palette: &Palette,
        theme: &AppTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let number = if row.kind == DiffRowKind::Deletion {
            row.old_line
        } else {
            row.new_line
        };
        let group: SharedString = format!("{id}-row").into();
        let row_for_click = row.clone();
        div()
            .id(SharedString::from(group.to_string()))
            .group(group.clone())
            .flex()
            .w_full()
            .min_h(metrics.line)
            .bg(palette.line(row.kind).unwrap_or(palette.bg))
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_shrink_0()
                    .w(gutter)
                    .justify_end()
                    .pr(px(12.))
                    .border_l(px(4.))
                    .border_color(
                        palette
                            .bar(row.kind)
                            .unwrap_or(gpui_kit::transparent_black()),
                    )
                    .bg(palette.gutter(row.kind).unwrap_or(palette.bg))
                    .font_family(metrics.font.clone())
                    .text_size(metrics.size)
                    .line_height(metrics.line)
                    .text_color(palette.gutter_fg)
                    .children(number.map(|number| SharedString::from(number.to_string())))
                    .when_some(comment, |this, (excerpt, view)| {
                        this.child(
                            div()
                                .id(SharedString::from(format!("{id}-plus")))
                                .absolute()
                                .left(px(6.))
                                .top(px(2.))
                                .flex()
                                .size(px(16.))
                                .items_center()
                                .justify_center()
                                .rounded(px(4.))
                                .bg(theme.primary)
                                .cursor_pointer()
                                .opacity(0.)
                                .group_hover(group.clone(), |style| style.opacity(1.))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.begin_comment(excerpt, &view, &row_for_click, window, cx);
                                }))
                                .child(
                                    Icon::new(IconName::Plus)
                                        .size(rems(0.75))
                                        .color(theme.primary_foreground),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(8.))
                    .font_family(metrics.font.clone())
                    .text_size(metrics.size)
                    .line_height(metrics.line)
                    .child(code_text(
                        row.text.trim_end_matches(['\n', '\r']),
                        tokens,
                        palette.fg,
                    )),
            )
            .into_any_element()
    }

    fn gutter_width(rows: &[DiffRow], metrics: &Metrics) -> Pixels {
        let widest = rows
            .iter()
            .flat_map(|row| [row.old_line, row.new_line])
            .flatten()
            .max()
            .unwrap_or(1)
            .max(1)
            .to_string()
            .len()
            .max(3);
        px(widest as f32 * f32::from(metrics.size) * 0.62 + 26.)
    }

    fn header_bar(
        &self,
        path: &str,
        trailing: Option<AnyElement>,
        palette: &Palette,
        theme: &AppTheme,
    ) -> AnyElement {
        div()
            .flex()
            .w_full()
            .min_h(px(32.))
            .items_center()
            .gap_2()
            .py(px(6.))
            .pl(px(12.))
            .pr(px(8.))
            .bg(palette.bg)
            .text_color(palette.fg)
            .font_family(SharedString::from(theme.fonts.sans.clone()))
            .text_size(px(12.))
            .line_height(relative(1.))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .child(SharedString::from(path.to_owned())),
            )
            .children(trailing)
            .into_any_element()
    }

    /// A bar between an excerpt and the lines it can open (`hunkSeparators: "line-info"`): the
    /// count of unmodified lines behind it, and a press opens a page of them.
    fn expand_bar(
        &self,
        id: SharedString,
        label: String,
        icon: IconName,
        busy: bool,
        on_click: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static,
        theme: &AppTheme,
    ) -> AnyElement {
        div()
            .id(id)
            .flex()
            .w_full()
            .h(px(24.))
            .items_center()
            .gap_2()
            .px_3()
            .bg(solid_over(theme.muted_foreground, 0.08, theme.background))
            .font_family(SharedString::from(theme.fonts.sans.clone()))
            .text_size(px(11.))
            .text_color(theme.muted_foreground)
            .when(!busy, |this| {
                this.cursor(gpui_kit::CursorStyle::PointingHand)
                    .hover(|style| style.bg(solid_over(theme.muted_foreground, 0.16, theme.background)))
                    .on_click(on_click)
            })
            .child(Icon::new(icon).size(rems(0.75)))
            .child(SharedString::from(if busy {
                "Loading...".to_owned()
            } else {
                label
            }))
            .into_any_element()
    }

    /// `GuideExcerpt`.
    #[allow(clippy::too_many_arguments)]
    fn excerpt_view(
        &mut self,
        index: usize,
        excerpt: &PullRequestReviewGuideExcerpt,
        files: &StopFiles,
        markers: &[crate::state::helpers::review_guide_tour::ExcerptLineMarkers],
        palette: &Palette,
        theme: &AppTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let caption = excerpt.caption.trim();
        let path = excerpt.path.as_str().to_owned();
        let view = files.files.get(&path).cloned();
        let range = ExcerptRange::from(excerpt);
        let shown = view
            .as_ref()
            .map(|view| excerpt_rows(&view.rows, &range))
            .unwrap_or_default();
        let body: AnyElement = match (&view, shown.is_empty()) {
            (Some(view), false) => {
                let metrics = Self::metrics(theme);
                let tokens = self.tokens_for(view, theme.is_dark());
                // The excerpt is a contiguous slice of the file's rows.
                let first = view
                    .rows
                    .iter()
                    .position(|row| {
                        row.kind == shown[0].kind
                            && row.old_line == shown[0].old_line
                            && row.new_line == shown[0].new_line
                    })
                    .unwrap_or(0);
                // The unchanged lines the bars have opened around the slice, when it is one
                // hunk's: the file's own text, once the file has been read.
                let stop_position = self.position(cx);
                let key = (stop_position, index);
                let expansion = self.expansions.get(&key).copied().unwrap_or_default();
                let contents = self
                    .ctx
                    .snapshot(cx)
                    .and_then(|snapshot| snapshot.file_contents.get(&path))
                    .cloned();
                let plan = contents
                    .as_ref()
                    .and_then(Loadable::ready)
                    .and_then(|file| plan_excerpt_expansion(&shown, &file.new_contents, expansion));
                let hidden_up = hidden_above(&shown, expansion);
                let hidden_down = plan.as_ref().map(|plan| plan.hidden_below);
                let (opened_above, opened_below) = plan
                    .as_ref()
                    .map(|plan| (plan.above.clone(), plan.below.clone()))
                    .unwrap_or_default();
                let all_rows: Vec<DiffRow> = opened_above
                    .iter()
                    .chain(&shown)
                    .chain(&opened_below)
                    .cloned()
                    .collect();
                let gutter = Self::gutter_width(&all_rows, &metrics);
                let loading_contents = matches!(contents, Some(Loadable::Pending));
                let pending: Vec<PendingReviewComment> = self
                    .review
                    .read(cx)
                    .pending_review_comments(&self.ctx.reference)
                    .iter()
                    .filter(|comment| comment.draft.path.as_str() == path)
                    .cloned()
                    .collect();
                let draft_anchor = self
                    .draft
                    .as_ref()
                    .filter(|draft| draft.excerpt == index)
                    .map(|draft| review_position_anchor(&draft.position));
                let mut rows = div().flex().flex_col().w_full();
                if let Some(hidden) = hidden_up.filter(|hidden| *hidden > 0) {
                    let file = view.file.clone();
                    rows = rows.child(self.expand_bar(
                        SharedString::from(format!("guide-expand-up-{index}")),
                        format!("{hidden} unmodified lines"),
                        IconName::ChevronUp,
                        loading_contents,
                        cx.listener(move |this, _, _, cx| {
                            this.open_excerpt(key, true, hidden, &file, cx)
                        }),
                        theme,
                    ));
                }
                for (offset, row) in opened_above.iter().enumerate() {
                    rows = rows.child(self.diff_line(
                        SharedString::from(format!("guide-{index}-above-{offset}")),
                        row,
                        &[],
                        gutter,
                        None,
                        &metrics,
                        palette,
                        theme,
                        cx,
                    ));
                }
                for (offset, row) in shown.iter().enumerate() {
                    let tokens_for_row = tokens.get(first + offset).map_or(&[][..], Vec::as_slice);
                    rows = rows.child(
                        self.diff_line(
                            SharedString::from(format!("guide-{index}-{offset}")),
                            row,
                            tokens_for_row,
                            gutter,
                            self.can_comment
                                .then(|| (index, view.clone()))
                                .filter(|_| self.draft.is_none()),
                            &metrics,
                            palette,
                            theme,
                            cx,
                        ),
                    );
                    // Notes, queued comments and the draft share the line they belong to.
                    let on_side = |side: PullRequestDiffSide| match side {
                        PullRequestDiffSide::Left => row.old_line,
                        PullRequestDiffSide::Right => row.new_line,
                    };
                    let note_line = match excerpt.side {
                        PullRequestReviewGuideExcerptSide::New => row.new_line,
                        PullRequestReviewGuideExcerptSide::Old => row.old_line,
                    };
                    if let Some(line) = note_line
                        && let Some(entry) = markers.iter().find(|entry| entry.line as i64 == line)
                    {
                        let mut line_notes = div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_x_3()
                            .gap_y_1()
                            .px_3()
                            .py_1()
                            .bg(palette.bg)
                            .font_family(SharedString::from(theme.fonts.sans.clone()))
                            .text_size(TEXT_XS.size);
                        for marker in &entry.markers {
                            line_notes = line_notes.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(rems(0.375))
                                    .child(note_marker(&marker.marker, theme))
                                    .child(
                                        div()
                                            .text_color(theme.foreground)
                                            .child(SharedString::from(marker.title.clone())),
                                    ),
                            );
                        }
                        rows = rows.child(line_notes);
                    }
                    for comment in pending.iter().filter(|comment| {
                        let (line, side) = review_position_anchor(&comment.draft.position);
                        on_side(side) == Some(line)
                    }) {
                        let id = comment.id.clone();
                        let entity = cx.entity().downgrade();
                        rows = rows.child(
                            div()
                                .w_full()
                                .bg(palette.bg)
                                .font_family(SharedString::from(theme.fonts.sans.clone()))
                                .text_color(theme.foreground)
                                .child(PendingReviewCommentCard::new(
                                    comment.clone(),
                                    move |_, _, cx| {
                                        let id = id.clone();
                                        entity
                                            .update(cx, |this, cx| {
                                                let key =
                                                    pull_request_review_key(&this.ctx.reference);
                                                this.review.update(cx, |store, _| {
                                                    store.remove_comment(&key, &id)
                                                });
                                            })
                                            .ok();
                                    },
                                )),
                        );
                    }
                    if let (Some((line, side)), Some(draft)) = (draft_anchor, &self.draft)
                        && on_side(side) == Some(line)
                    {
                        rows = rows.child(
                            div()
                                .w_full()
                                .bg(palette.bg)
                                .font_family(SharedString::from(theme.fonts.sans.clone()))
                                .text_color(theme.foreground)
                                .child(draft.card.clone()),
                        );
                    }
                }
                for (offset, row) in opened_below.iter().enumerate() {
                    rows = rows.child(self.diff_line(
                        SharedString::from(format!("guide-{index}-below-{offset}")),
                        row,
                        &[],
                        gutter,
                        None,
                        &metrics,
                        palette,
                        theme,
                        cx,
                    ));
                }
                // One hunk's slice can open downward; how far is known once the file is read.
                if hidden_up.is_some() && hidden_down != Some(0) {
                    let file = view.file.clone();
                    let label = match hidden_down {
                        Some(hidden) => format!("{hidden} unmodified lines"),
                        None => "Show more lines".to_owned(),
                    };
                    rows = rows.child(self.expand_bar(
                        SharedString::from(format!("guide-expand-down-{index}")),
                        label,
                        IconName::ChevronDown,
                        loading_contents,
                        cx.listener(move |this, _, _, cx| {
                            this.open_excerpt(
                                key,
                                false,
                                hidden_down.unwrap_or(usize::MAX),
                                &file,
                                cx,
                            )
                        }),
                        theme,
                    ));
                }
                let stat = format!(
                    "{}showing {} of {} diff lines",
                    if excerpt.side == PullRequestReviewGuideExcerptSide::Old {
                        "removed code, "
                    } else {
                        ""
                    },
                    shown.len(),
                    view.rows.len()
                );
                let open = self.ctx.nav.open_file_in_code.clone();
                let open_path = path.clone();
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .rounded(RADIUS_LG)
                    .border_1()
                    .border_color(with_alpha(theme.border, 0.6))
                    .child(
                        self.header_bar(
                            &path,
                            Some(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_size(TEXT_XS.size)
                                    .text_color(theme.muted_foreground)
                                    .child(SharedString::from(stat))
                                    .child(
                                        Button::new(SharedString::from(format!(
                                            "guide-open-{index}"
                                        )))
                                        .size(ButtonSize::IconXs)
                                        .variant(ButtonVariant::GhostMuted)
                                        .icon(IconName::ArrowUpRight)
                                        .icon_size(rems(0.875))
                                        .tooltip(Tooltip::new(format!("Open {path} in Code")))
                                        .on_click(move |_, _, cx| open(&open_path, cx)),
                                    )
                                    .into_any_element(),
                            ),
                            palette,
                            theme,
                        ),
                    )
                    .child(rows)
                    .into_any_element()
            }
            _ if files.loading => div()
                .rounded(RADIUS_LG)
                .border_1()
                .border_color(with_alpha(theme.border, 0.6))
                .px_3()
                .py_2()
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .child(SharedString::from(format!("Loading {path}...")))
                .into_any_element(),
            _ => {
                let open = self.ctx.nav.open_file_in_code.clone();
                let open_path = path.clone();
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .rounded(RADIUS_LG)
                    .border_1()
                    .border_color(with_alpha(theme.border, 0.6))
                    .px_3()
                    .py_2()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(
                                div()
                                    .font_family(SharedString::from(theme.fonts.mono.clone()))
                                    .text_color(theme.foreground)
                                    .child(SharedString::from(path.clone())),
                            )
                            .child(SharedString::from(format!(
                                " lines {}-{} are not in the loaded diff.",
                                excerpt.start, excerpt.end
                            ))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("guide-open-missing-{index}")))
                            .size(ButtonSize::Xs)
                            .variant(ButtonVariant::Outline)
                            .label("Open in Code")
                            .on_click(move |_, _, cx| open(&open_path, cx)),
                    )
                    .into_any_element()
            }
        };
        div()
            .flex()
            .min_w_0()
            .flex_col()
            .gap(px(6.))
            .when(!caption.is_empty(), |this| {
                this.child(
                    div()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(caption.to_owned())),
                )
            })
            .child(body)
            .into_any_element()
    }

    /// The files a stop owns without excerpting: collapsed rows that open into the file's diff.
    fn other_files(
        &mut self,
        stop: &PullRequestReviewGuideStop,
        files: &StopFiles,
        palette: &Palette,
        theme: &AppTheme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let excerpted: HashSet<&str> = stop.excerpts.iter().map(|e| e.path.as_str()).collect();
        let others: Vec<_> = stop
            .files
            .iter()
            .filter(|file| !excerpted.contains(file.path.as_str()))
            .collect();
        if others.is_empty() {
            return None;
        }
        let mut list = div()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(RADIUS_LG)
            .border_1()
            .border_color(with_alpha(theme.border, 0.6));
        for (index, file) in others.iter().enumerate() {
            let path = file.path.as_str().to_owned();
            let open = self.open_files.contains(&path);
            let view = files.files.get(&path).cloned();
            let toggle_path = path.clone();
            let accent = theme.accent;
            let header = div()
                .id(SharedString::from(format!("guide-file-{index}")))
                .flex()
                .w_full()
                .min_w_0()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .text_size(TEXT_XS.size)
                .cursor_pointer()
                .hover(move |style| style.bg(with_alpha(accent, 0.5)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.open_files.remove(&toggle_path) {
                        this.open_files.insert(toggle_path.clone());
                    }
                    cx.notify();
                }))
                .child(
                    Icon::new(if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size(rems(0.875))
                    .color(theme.muted_foreground),
                )
                .child(
                    Icon::new(IconName::FileCode)
                        .size(rems(0.875))
                        .color(theme.muted_foreground),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(SharedString::from(theme.fonts.mono.clone()))
                        .text_color(theme.foreground)
                        .child(SharedString::from(path.clone())),
                )
                .when_some(
                    file.note.as_ref().filter(|note| !note.trim().is_empty()),
                    |this, note| {
                        this.child(
                            div()
                                .ml_auto()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.muted_foreground)
                                .child(SharedString::from(note.clone())),
                        )
                    },
                )
                .when_some(view.as_ref(), |this, view| {
                    this.child(
                        div()
                            .flex_shrink_0()
                            .child(PullRequestDiffStat::new(view.additions, view.deletions).mono()),
                    )
                });
            let mut entry = div().child(header);
            if open {
                entry = entry.child(match &view {
                    Some(view) => {
                        let metrics = Self::metrics(theme);
                        let tokens = self.tokens_for(view, theme.is_dark());
                        let gutter = Self::gutter_width(&view.rows, &metrics);
                        let mut rows = div()
                            .flex()
                            .flex_col()
                            .w_full()
                            .bg(palette.bg)
                            .border_t_1()
                            .border_color(with_alpha(theme.border, 0.6));
                        for (offset, row) in view.rows.iter().take(FILE_DIFF_ROW_LIMIT).enumerate()
                        {
                            rows = rows.child(self.diff_line(
                                SharedString::from(format!("guide-file-{index}-{offset}")),
                                row,
                                tokens.get(offset).map_or(&[][..], Vec::as_slice),
                                gutter,
                                None,
                                &metrics,
                                palette,
                                theme,
                                cx,
                            ));
                        }
                        if view.rows.len() > FILE_DIFF_ROW_LIMIT {
                            let open = self.ctx.nav.open_file_in_code.clone();
                            let path = path.clone();
                            rows = rows.child(
                                div().flex().items_center().gap_2().px_3().py_2().child(
                                    Button::new(SharedString::from(format!(
                                        "guide-file-more-{index}"
                                    )))
                                    .size(ButtonSize::Xs)
                                    .variant(ButtonVariant::Outline)
                                    .label("Open the rest in Code")
                                    .on_click(move |_, _, cx| open(&path, cx)),
                                ),
                            );
                        }
                        rows.into_any_element()
                    }
                    None => div()
                        .px_3()
                        .py_2()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.muted_foreground)
                        .child(if files.loading {
                            "Loading the diff..."
                        } else {
                            "This file is not in the loaded diff."
                        })
                        .into_any_element(),
                });
            }
            list = list.child(
                div()
                    .when(index > 0, |this| {
                        this.border_t_1()
                            .border_color(with_alpha(theme.border, 0.6))
                    })
                    .child(entry),
            );
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap(rems(0.375))
                .child(
                    div()
                        .text_size(TEXT_XS.size)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted_foreground)
                        .child(if stop.excerpts.is_empty() {
                            "Files in this stop"
                        } else {
                            "Also in this stop"
                        }),
                )
                .child(list)
                .into_any_element(),
        )
    }

    /// `GuideStop`.
    fn stop_view(
        &mut self,
        stop: &PullRequestReviewGuideStop,
        position: usize,
        progress: &GuideProgress,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let _ = window;
        let theme = cx.app_theme().clone();
        let palette = Palette::new(&theme);
        let count = self.guide.stops.len();
        let wanted = stop_file_paths(stop);
        let files = self.stop_files(&wanted, cx);
        let lettered = letter_stop_notes(stop);
        let beside_notes = self.width >= NOTES_BESIDE && !lettered.lettered.is_empty();
        let narrow = self.width < RAIL_BESIDE;
        let rail_open = self.rail_open;
        let next_title = self
            .guide
            .stops
            .get(position + 1)
            .map(|next| next.title.as_str().to_owned());
        let can_finish = self.on_finish.is_some();
        let select: SelectStop = {
            let entity = cx.entity().downgrade();
            Rc::new(move |index, _, cx| {
                entity.update(cx, |this, cx| this.go_to(index, cx)).ok();
            })
        };

        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(if narrow {
                div()
                    .child(
                        Button::new("guide-rail-toggle")
                            .size(ButtonSize::Xs)
                            .variant(ButtonVariant::GhostMuted)
                            .child(SharedString::from(format!(
                                "Stop {} of {count}",
                                position + 1
                            )))
                            .child(
                                Icon::new(if rail_open {
                                    IconName::ChevronUp
                                } else {
                                    IconName::ChevronDown
                                })
                                .size(rems(0.875)),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.rail_open = !this.rail_open;
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            } else {
                div()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(format!(
                        "Stop {} of {count}",
                        position + 1
                    )))
                    .into_any_element()
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new("guide-prev")
                            .size(ButtonSize::IconXs)
                            .variant(ButtonVariant::Outline)
                            .icon(IconName::ChevronUp)
                            .icon_size(rems(0.875))
                            .tooltip(Tooltip::new("Previous stop"))
                            .disabled(position == 0)
                            .on_click(cx.listener(|this, _, _, cx| this.step(-1, cx))),
                    )
                    .child(
                        Button::new("guide-next")
                            .size(ButtonSize::IconXs)
                            .variant(ButtonVariant::Outline)
                            .icon(IconName::ChevronDown)
                            .icon_size(rems(0.875))
                            .tooltip(Tooltip::new("Next stop"))
                            .disabled(position + 1 >= count)
                            .on_click(cx.listener(|this, _, _, cx| this.step(1, cx))),
                    )
                    .when(self.width >= HINT_WIDTH, |this| {
                        this.child(
                            div()
                                .ml_1()
                                .flex()
                                .items_center()
                                .gap(px(2.))
                                .child(Kbd::new("J"))
                                .child(Kbd::new("K")),
                        )
                    }),
            );

        let mut article = div()
            .flex()
            .min_w_0()
            .flex_col()
            .gap(rems(1.25))
            .child(header);
        if narrow && rail_open {
            article = article.child(div().pt_3().child(rail(
                &self.guide.stops,
                position,
                &progress.seen,
                false,
                select.clone(),
                &theme,
            )));
        }
        article = article.child(
            div()
                .flex()
                .max_w(rems(60.))
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_size(TEXT_LG.size)
                        .line_height(TEXT_LG.line_height)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child(SharedString::from(stop.title.as_str().to_owned())),
                )
                .when(!stop.summary.trim().is_empty(), |this| {
                    this.child(
                        div()
                            .text_size(TEXT_SM.size)
                            .text_color(theme.muted_foreground)
                            .child(prose(
                                SharedString::from(format!("guide-summary-{position}")),
                                &stop.summary,
                            )),
                    )
                }),
        );
        if let Some(attention) = &stop.attention {
            // A call for the reader to make, not a warning, so it reads as a plain notice.
            article = article.child(alert(
                AlertVariant::Default,
                Icon::new(IconName::Eye)
                    .size(rems(1.))
                    .color(theme.muted_foreground)
                    .into_any_element(),
                div()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(attention_label(attention.kind)),
                    )
                    .child(div().text_color(theme.muted_foreground).child(prose(
                        SharedString::from(format!("guide-attention-{position}")),
                        attention.note.as_str(),
                    )))
                    .into_any_element(),
                None,
                cx,
            ));
        }
        if let Some(error) = &files.error {
            article = article.child(
                div()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(format!(
                        "The diff could not be loaded, so excerpts are missing: {error}"
                    ))),
            );
        }
        if !stop.excerpts.is_empty() {
            let mut excerpts = div().flex().min_w_0().flex_col().gap_4();
            for (index, excerpt) in stop.excerpts.iter().enumerate() {
                let markers = lettered.by_excerpt.get(index).cloned().unwrap_or_default();
                excerpts = excerpts.child(
                    self.excerpt_view(index, excerpt, &files, &markers, &palette, &theme, cx),
                );
            }
            article = article.child(excerpts);
        }
        if !beside_notes {
            article = article.children(notes_section(position, &lettered.lettered, &theme));
        }
        article = article.children(self.other_files(stop, &files, &palette, &theme, cx));
        if !stop.questions.is_empty() {
            let mut questions = div().flex().flex_col().items_start().gap(rems(0.375));
            for (index, question) in stop.questions.iter().enumerate() {
                questions = questions.child(
                    div()
                        .text_size(TEXT_SM.size)
                        .text_color(theme.foreground)
                        .child(prose(
                            SharedString::from(format!("guide-question-{position}-{index}")),
                            question.as_str(),
                        )),
                );
            }
            article = article.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.375))
                    .child(
                        div()
                            .text_size(TEXT_XS.size)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.muted_foreground)
                            .child("Questions to consider"),
                    )
                    .child(questions),
            );
        }
        article = article.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_end()
                .gap_3()
                .border_t_1()
                .border_color(with_alpha(theme.border, 0.6))
                .pt_4()
                .children(match next_title {
                    Some(title) => Some(
                        Button::new("guide-footer-next")
                            .variant(ButtonVariant::Default)
                            .min_w_0()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .child(SharedString::from(format!("Next: {title}"))),
                            )
                            .child(Icon::new(IconName::ArrowRight).size(rems(1.)))
                            .on_click(cx.listener(|this, _, _, cx| this.step(1, cx)))
                            .into_any_element(),
                    ),
                    None if can_finish => Some(
                        Button::new("guide-finish")
                            .variant(ButtonVariant::Default)
                            .icon(IconName::MessageSquare)
                            .label("Finish review")
                            .on_click(cx.listener(|this, _, _, cx| this.finish(cx)))
                            .into_any_element(),
                    ),
                    None => None,
                }),
        );
        if beside_notes {
            // On a maximized panel the notes take a column of their own beside the stop.
            div()
                .flex()
                .gap_8()
                .child(div().min_w_0().flex_1().child(article))
                .child(div().w(rems(20.)).flex_shrink_0().children(notes_section(
                    position,
                    &lettered.lettered,
                    &theme,
                )))
                .into_any_element()
        } else {
            article.into_any_element()
        }
    }

    fn banner_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let banner = self.banner.clone()?;
        let theme = cx.app_theme().clone();
        let icon = if banner.running {
            super::presentation_view::Spinner::new("guide-banner-spinner")
                .size(rems(1.))
                .into_any_element()
        } else {
            Icon::new(IconName::TriangleAlert)
                .size(rems(1.))
                .color(theme.warning)
                .into_any_element()
        };
        let rewrite = (!banner.running && banner.can_write).then(|| {
            Button::new("guide-rewrite")
                .size(ButtonSize::Xs)
                .variant(ButtonVariant::Outline)
                .icon(IconName::Sparkles)
                .label("Rewrite")
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(source) = this.ctx.source(cx) {
                        source.generate_guide(&this.ctx.reference, cx);
                    }
                }))
                .into_any_element()
        });
        Some(
            alert(
                if banner.running {
                    AlertVariant::Info
                } else {
                    AlertVariant::Warning
                },
                icon,
                div()
                    .child(SharedString::from(banner.message))
                    .into_any_element(),
                rewrite,
                cx,
            )
            .into_any_element(),
        )
    }
}

impl Render for GuideTour {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let key = self.key();
        let progress = self.progress.read(cx).get(&key);
        let position = self.position(cx);
        let stop = self.guide.stops.get(position).cloned();
        let select: SelectStop = {
            let entity = cx.entity().downgrade();
            Rc::new(move |index, _, cx| {
                entity.update(cx, |this, cx| this.go_to(index, cx)).ok();
            })
        };
        let wide = self.width >= RAIL_BESIDE;
        let main = {
            let banner = self.banner_view(cx);
            let stop_element = match &stop {
                Some(stop) => self.stop_view(stop, position, &progress, window, cx),
                None => div()
                    .text_size(TEXT_SM.size)
                    .text_color(theme.muted_foreground)
                    .child("This change has no files to tour.")
                    .into_any_element(),
            };
            div()
                .flex()
                .min_w_0()
                .flex_col()
                .gap_4()
                .when(wide, |this| this.flex_1())
                .children(banner)
                .child(stop_element)
        };
        let entity = cx.entity().downgrade();
        let focus = self.focus.clone();
        let root = div()
            .id("guide-tour")
            .key_context("GuideTour")
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let modifiers = event.keystroke.modifiers;
                if !this.active || modifiers.modified() {
                    return;
                }
                match event.keystroke.key.as_str() {
                    "j" => this.step(1, cx),
                    "k" => this.step(-1, cx),
                    _ => {}
                }
            }))
            .on_mouse_down(gpui_kit::MouseButton::Left, move |_, window, cx| {
                focus.focus(window, cx);
            })
            .child(
                div()
                    .relative()
                    .size_full()
                    .child(
                        div()
                            .id("guide-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .child(
                                div()
                                    .mx_auto()
                                    .flex()
                                    .max_w(rems(100.))
                                    .gap_6()
                                    .px_4()
                                    .py_4()
                                    .when(wide, |this| {
                                        this.child(div().w(rems(16.)).flex_shrink_0().child(rail(
                                            &self.guide.stops,
                                            position,
                                            &progress.seen,
                                            true,
                                            select,
                                            &theme,
                                        )))
                                    })
                                    .child(main),
                            ),
                    )
                    .vertical_scrollbar(&self.scroll),
            );
        div()
            .size_full()
            .min_h_0()
            .on_children_prepainted(move |bounds, _, cx| {
                let Some(width) = bounds.first().map(|bounds| f32::from(bounds.size.width)) else {
                    return;
                };
                entity
                    .update(cx, |this, cx| {
                        if (this.width - width).abs() > 0.5 {
                            this.width = width;
                            cx.notify();
                        }
                    })
                    .ok();
            })
            .child(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn progress_keys_are_per_guide_head() {
        assert_eq!(guide_progress_key("env:pr", "abc"), "env:pr:abc");
        assert_ne!(
            guide_progress_key("env:pr", "abc"),
            guide_progress_key("env:pr", "def")
        );
    }

    #[test]
    fn progress_store_starts_at_the_first_stop_and_keeps_seen() {
        let mut store = GuideProgressStore::default();
        assert_eq!(store.get("k"), GuideProgress::default());
        store.set("k", |current| {
            let mut seen = current.seen.clone();
            seen.insert(0);
            GuideProgress { position: 1, seen }
        });
        assert_eq!(store.get("k").position, 1);
        assert!(store.get("k").seen.contains(&0));
        assert_eq!(store.get("other"), GuideProgress::default());
    }

    #[test]
    fn attention_kinds_have_their_own_sentence() {
        assert_eq!(
            attention_label(PullRequestReviewGuideAttentionKind::Risk),
            "A risk to judge"
        );
        assert_eq!(
            attention_label(PullRequestReviewGuideAttentionKind::Accessibility),
            "An accessibility call"
        );
    }
}
