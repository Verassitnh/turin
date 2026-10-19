//! Port of upstream/apps/web/src/components/chat/MessagesTimeline.tsx
//! (`ConversationTimeline`, `TimelineRowContent`).
//!
//! Upstream virtualizes with LegendList; Turin uses GPUI's [`ListState`],
//! which measures rows lazily, keeps measured heights for offscreen rows and
//! follows the tail (`FollowMode::Tail`) the way upstream's
//! `maintainScrollAtEnd` does: it sticks to the bottom while content grows and
//! lets go as soon as the reader scrolls up, re-arming when they return.
//!
//! Rows come from [`derive_messages_timeline_rows_with_state`] and pass
//! through [`compute_stable_messages_timeline_rows`], so a streamed chunk
//! changes only the rows it touches. The list is spliced over exactly the
//! changed index range, keeping every other row's measured height.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, FollowMode, InteractiveElement as _, IntoElement,
    ListAlignment, ListState, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, list, px, rems,
};

use super::assistant_citation_source::citation_highlight_opacity;
use super::assistant_citation_target::{
    AssistantCitationController, AssistantCitationRequest, CitationEntryView, CitationRowView,
    CitationTargetEffect, CitationTargetInput,
};
use super::markdown::chat_markdown::MarkdownFind;
use super::markdown::inline_flow::FindMatchProbe;
use super::thread_find_navigation::{
    find_match_key, is_visible_delta, reveal_margin, vertical_reveal_delta,
};
use super::timeline::assistant_citations::AssistantCitation;
use super::timeline::messages_timeline_logic::{
    MessagesTimelineRow, MessagesTimelineRowContent, MessagesTimelineRowsInput,
    MessagesTimelineRowsProjection, StableMessagesTimelineRowsState,
    compute_stable_messages_timeline_rows, derive_messages_timeline_rows_with_state,
};
use super::timeline::source::{ThreadTimelineStore, TimelineSource};
use super::timeline::types::{ThreadTimelineSnapshot, TimelineHistoryControls};
use super::timeline_message_rows::{
    assistant_meta_row, assistant_row, proposed_plan_row, user_row,
};
use super::timeline_work_rows::{
    attempt_fold_row, context_compaction_row, event_row, thinking_row, turn_fold_row,
    work_group_row, work_live_row, work_toggle_row, working_row,
};
use super::timestamp_format::TimestampFormat;
use super::work_log::{WorkLogLayout, work_log_block};
use crate::theme::color::with_alpha;
use crate::theme::tokens::{CHAT_CONTENT_MAX_WIDTH, TEXT_SM, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};

/// `estimatedItemSize`-like overdraw: rows within this distance of the
/// viewport stay laid out, so scrolling never shows unmeasured gaps.
const LIST_OVERDRAW: f32 = 1200.;

/// Expansion and disclosure state upstream keeps in component state and the
/// UI store. Rows read it through [`RowCx`].
#[derive(Clone, Default)]
pub struct TimelineUiState {
    pub expanded_run_ids: HashSet<String>,
    pub expanded_attempt_ids: HashSet<String>,
    pub expanded_work_group_ids: HashSet<String>,
    /// Open work-entry disclosures (`workGroupViewState.expandedEntries`).
    pub expanded_entries: HashSet<String>,
    /// User messages showing their full text.
    pub expanded_user_messages: HashSet<String>,
    /// Expanded proposed plans.
    pub expanded_plans: HashSet<String>,
    /// Open error-event disclosures (`<details>`).
    pub open_events: HashSet<String>,
}

/// The active match of the open find (`activeFindMatch`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineFindMatch {
    pub entry_id: String,
    pub run_id: Option<String>,
    pub occurrence: u64,
}

/// What the open find bar tells the timeline (`timelineProps`: `findQuery`,
/// `activeFindMatch`, `findNavigationId`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TimelineFind {
    /// The query to paint. Empty while there is no result to paint.
    pub query: String,
    pub active: Option<TimelineFindMatch>,
    pub navigation_id: u64,
}

/// The find state the rows paint from.
#[derive(Clone)]
pub struct RowFind {
    pub query: SharedString,
    pub active: Option<TimelineFindMatch>,
    pub probe: FindMatchProbe,
}

/// Frames the reveal keeps adjusting for while the list measures rows.
const FIND_REVEAL_MAX_FRAMES: u32 = 90;

/// Shared per-frame state for row renderers (upstream's `TimelineRowCtx` and
/// `TimelineRowActivityCtx`).
#[derive(Clone)]
pub struct RowCx {
    pub theme: AppTheme,
    pub view: Entity<MessagesTimeline>,
    pub source: Option<Arc<dyn TimelineSource>>,
    pub snapshot: Arc<ThreadTimelineSnapshot>,
    pub ui: Arc<TimelineUiState>,
    pub timestamp_format: TimestampFormat,
    /// Selection document order base for this row.
    pub order: u64,
    /// Assistant message currently pulsing as a citation source, and its opacity.
    pub citation_message: Option<String>,
    pub citation_opacity: f32,
    /// The open find's matches, painted into message text.
    pub find: Option<RowFind>,
}

impl RowCx {
    /// What `ChatMarkdown` paints for the row of timeline entry `entry_id`.
    pub fn markdown_find(&self, entry_id: &str) -> Option<MarkdownFind> {
        let find = self.find.as_ref()?;
        Some(MarkdownFind {
            query: find.query.clone(),
            active: find
                .active
                .as_ref()
                .filter(|active| active.entry_id == entry_id)
                .map(|active| active.occurrence),
            probe: find.probe.clone(),
        })
    }

    pub fn workspace_root(&self) -> Option<&str> {
        self.snapshot.workspace_root.as_deref()
    }

    pub fn latest_run_id(&self) -> Option<&str> {
        self.snapshot
            .latest_run
            .as_ref()
            .map(|run| run.run_id.as_str())
    }

    /// Mutates the timeline's UI state and re-derives rows.
    pub fn update_ui(&self, cx: &mut App, update: impl FnOnce(&mut TimelineUiState)) {
        self.view.update(cx, |view, cx| {
            let mut ui = (*view.ui).clone();
            update(&mut ui);
            view.ui = Arc::new(ui);
            view.recompute(cx);
            cx.notify();
        });
    }

    pub fn toggle(set: &mut HashSet<String>, key: &str) {
        if !set.remove(key) {
            set.insert(key.to_string());
        }
    }
}

pub struct MessagesTimeline {
    store: Entity<ThreadTimelineStore>,
    pub(super) list: ListState,
    projection: Option<MessagesTimelineRowsProjection>,
    stable: StableMessagesTimelineRowsState,
    rows: Arc<Vec<Arc<MessagesTimelineRow>>>,
    ui: Arc<TimelineUiState>,
    timestamp_format: TimestampFormat,
    ticker: Option<Task<()>>,
    /// The "Load earlier turns" row shown in the list header.
    history: Option<TimelineHistoryControls>,
    citation: AssistantCitationController,
    pulse: Option<(String, Instant)>,
    pulse_task: Option<Task<()>>,
    find: TimelineFind,
    /// Where the active match painted last frame (window coordinates).
    find_probe: FindMatchProbe,
    /// The navigation step whose row was last scrolled to.
    find_revealed_key: Option<String>,
    /// Frames spent settling the active match into the readable band; `None` once settled.
    find_reveal_frames: Option<u32>,
    _observe: Subscription,
}

impl MessagesTimeline {
    pub fn new(store: Entity<ThreadTimelineStore>, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let list = ListState::new(2, ListAlignment::Top, px(LIST_OVERDRAW));
        list.set_follow_mode(FollowMode::Tail);
        let observe = cx.observe(&store, |this, _, cx| {
            this.recompute(cx);
            cx.notify();
        });
        let mut this = Self {
            store,
            list,
            projection: None,
            stable: StableMessagesTimelineRowsState::default(),
            rows: Arc::new(Vec::new()),
            ui: Arc::new(TimelineUiState::default()),
            timestamp_format: TimestampFormat::default(),
            ticker: None,
            history: None,
            citation: AssistantCitationController::default(),
            pulse: None,
            pulse_task: None,
            find: TimelineFind::default(),
            find_probe: FindMatchProbe::default(),
            find_revealed_key: None,
            find_reveal_frames: None,
            _observe: observe,
        };
        this.recompute(cx);
        // `TURIN_EXPAND=folds|all` (dev/screenshots) opens every turn fold
        // (and every tool group).
        if let Ok(mode) = std::env::var("TURIN_EXPAND") {
            for _ in 0..3 {
                let mut ui = (*this.ui).clone();
                for row in this.rows.iter() {
                    match &row.content {
                        MessagesTimelineRowContent::TurnFold { run_id, .. } => {
                            ui.expanded_run_ids.insert(run_id.clone());
                        }
                        MessagesTimelineRowContent::WorkToggle { group_id, .. }
                            if mode == "all" =>
                        {
                            ui.expanded_work_group_ids.insert(group_id.clone());
                        }
                        _ => {}
                    }
                }
                this.ui = Arc::new(ui);
                this.recompute(cx);
            }
        }
        // `initialScrollAtEnd`. `TURIN_SCROLL_ROW=n` (dev/screenshots) opens
        // at row n instead.
        match std::env::var("TURIN_SCROLL_ROW")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
        {
            Some(row) => {
                this.list.set_follow_mode(FollowMode::Normal);
                this.list.scroll_to(gpui_kit::ListOffset {
                    item_ix: row + 1,
                    offset_in_item: px(0.),
                })
            }
            None => this.list.scroll_to_end(),
        }
        this
    }

    pub fn store(&self) -> &Entity<ThreadTimelineStore> {
        &self.store
    }

    /// `goToCitation`: unfold the turn, scroll the source row, and pulse it.
    pub fn reveal_citation(
        &mut self,
        citation: AssistantCitation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.store.read(cx).snapshot().clone();
        let request = AssistantCitationRequest {
            key: format!(
                "{}:{}:{}",
                citation.message_id, citation.start, citation.end
            ),
            citation,
        };
        for _ in 0..3 {
            let entries = snapshot
                .timeline_entries
                .iter()
                .filter_map(|entry| match &entry.kind {
                    super::timeline::types::TimelineEntryKind::Message { message, .. } => {
                        Some(CitationEntryView {
                            id: entry.id.clone(),
                            message_id: Some(message.id.clone()),
                            assistant: message.role
                                == super::timeline::types::MessageRole::Assistant,
                            run_id: message.run_id.clone(),
                        })
                    }
                    _ => None,
                })
                .collect();
            let rows = self
                .rows
                .iter()
                .filter_map(|row| match &row.content {
                    MessagesTimelineRowContent::Message { id, message, .. } => {
                        Some(CitationRowView {
                            id: id.clone(),
                            message_id: Some(message.id.clone()),
                        })
                    }
                    _ => None,
                })
                .collect();
            let effects = self.citation.sync(&CitationTargetInput {
                request: Some(request.clone()),
                entries,
                rows,
                viewport: true,
                history_loading: false,
                load_earlier: None,
                list_loaded: true,
                list_present: true,
            });
            let mut expanded = false;
            for effect in effects {
                match effect {
                    CitationTargetEffect::ExpandTurn(run_id) => {
                        let mut ui = (*self.ui).clone();
                        ui.expanded_run_ids.insert(run_id);
                        self.ui = Arc::new(ui);
                        self.recompute(cx);
                        expanded = true;
                    }
                    CitationTargetEffect::Toast { title, description } => {
                        window
                            .push_notification(Notification::warning(description).title(title), cx);
                    }
                    CitationTargetEffect::LoadEarlier => {
                        window.push_notification(
                            Notification::warning(
                                "Load earlier turns, then click the citation to try again. Your saved quote is unchanged.",
                            )
                            .title("Could not load the cited response"),
                            cx,
                        );
                    }
                    CitationTargetEffect::ManualNavigation | CitationTargetEffect::CancelScroll => {
                    }
                }
            }
            if !expanded {
                break;
            }
        }
        let message_id = request.citation.message_id.clone();
        if let Some(index) = self.rows.iter().position(|row| {
            matches!(
                &row.content,
                MessagesTimelineRowContent::Message { message, .. }
                    if message.id == message_id
            )
        }) {
            self.list.set_follow_mode(FollowMode::Normal);
            self.list.scroll_to(gpui_kit::ListOffset {
                item_ix: index + 1,
                offset_in_item: px(0.),
            });
            self.citation.mark_scrolled();
            self.pulse = Some((message_id, Instant::now()));
            self.pulse_task = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(50))
                        .await;
                    let done = this
                        .update(cx, |this, cx| {
                            let finished = this
                                .pulse
                                .as_ref()
                                .is_none_or(|(_, started)| started.elapsed().as_millis() > 3000);
                            if finished {
                                this.pulse = None;
                            }
                            cx.notify();
                            finished
                        })
                        .unwrap_or(true);
                    if done {
                        break;
                    }
                }
            }));
        }
        cx.notify();
    }

    /// `timelineProps` from the find bar. A new active match unfolds what hides it and
    /// scrolls its row into view; the frames after that settle the match itself into the
    /// readable band once it has painted (`useThreadFindNavigation`).
    pub fn set_find(&mut self, find: TimelineFind, cx: &mut Context<Self>) {
        if self.find == find {
            return;
        }
        self.find = find;
        match self.find.active.clone() {
            Some(active) => {
                let key = find_match_key(
                    self.find.navigation_id,
                    &self.find.query,
                    &active.entry_id,
                    active.occurrence,
                );
                if self.find_revealed_key.as_deref() != Some(key.as_str()) {
                    self.find_revealed_key = Some(key);
                    self.reveal_find_row(&active, cx);
                }
            }
            None => {
                self.find_revealed_key = None;
                self.find_reveal_frames = None;
            }
        }
        cx.notify();
    }

    /// `findPositionReaderRef`: where the next search starts, from the rows on screen. The
    /// first searchable row in view anchors it; occurrences within a row are not measured
    /// (see PORTING.md Deviations), so the anchor is that row's first occurrence.
    pub fn read_find_position(&self) -> Option<super::thread_find::ThreadFindStart> {
        use super::thread_find_position::{
            ThreadFindBand, ThreadFindRowGeometry, read_thread_find_position,
        };
        let viewport = self.list.viewport_bounds();
        let band = ThreadFindBand {
            top: f32::from(viewport.top()) as f64,
            bottom: f32::from(viewport.bottom()) as f64,
        };
        let first = self.list.logical_scroll_top().item_ix.max(1);
        let mut rows = Vec::new();
        for item_ix in first..=self.rows.len() {
            let Some(bounds) = self.list.bounds_for_item(item_ix) else {
                continue;
            };
            let top = f32::from(bounds.top()) as f64;
            let row_id = match &self.rows[item_ix - 1].content {
                MessagesTimelineRowContent::Message { message, .. } => Some(message.id.clone()),
                MessagesTimelineRowContent::ProposedPlan { proposed_plan, .. } => {
                    Some(proposed_plan.id.clone())
                }
                _ => None,
            };
            rows.push(ThreadFindRowGeometry {
                row_id,
                top,
                bottom: f32::from(bounds.bottom()) as f64,
                match_bottoms: Vec::new(),
            });
            if top >= band.bottom {
                break;
            }
        }
        read_thread_find_position(
            band,
            &rows,
            |id| turin_contracts::base_schemas::TrimmedNonEmptyString::new(id).ok(),
            0.0,
        )
    }

    /// The index in `rows` of the message or plan row for timeline entry `entry_id`.
    fn find_row_index(&self, entry_id: &str) -> Option<usize> {
        self.rows.iter().position(|row| match &row.content {
            MessagesTimelineRowContent::Message { message, .. } => message.id == entry_id,
            MessagesTimelineRowContent::ProposedPlan { proposed_plan, .. } => {
                proposed_plan.id == entry_id
            }
            _ => false,
        })
    }

    /// Opens the turn fold, the collapsed message or the collapsed plan that hides the
    /// match, then scrolls its row to the top of the list.
    fn reveal_find_row(&mut self, active: &TimelineFindMatch, cx: &mut Context<Self>) {
        // A match inside a folded turn has no row until the turn opens; nested folds
        // need a pass each.
        for _ in 0..3 {
            if self.find_row_index(&active.entry_id).is_some() {
                break;
            }
            let Some(run_id) = active.run_id.clone() else {
                break;
            };
            if !self.ui.expanded_run_ids.contains(&run_id) {
                let mut ui = (*self.ui).clone();
                ui.expanded_run_ids.insert(run_id);
                self.ui = Arc::new(ui);
                self.recompute(cx);
            } else {
                break;
            }
        }
        let Some(index) = self.find_row_index(&active.entry_id) else {
            self.find_reveal_frames = None;
            return;
        };
        // A collapsed body clips the match; open it so the whole text is there.
        let is_plan = matches!(
            self.rows[index].content,
            MessagesTimelineRowContent::ProposedPlan { .. }
        );
        let mut ui = (*self.ui).clone();
        let opened = if is_plan {
            ui.expanded_plans.insert(active.entry_id.clone())
        } else {
            ui.expanded_user_messages.insert(active.entry_id.clone())
        };
        if opened {
            self.ui = Arc::new(ui);
            self.recompute(cx);
        }
        self.list.set_follow_mode(FollowMode::Normal);
        self.list.scroll_to(gpui_kit::ListOffset {
            item_ix: index + 1,
            offset_in_item: px(0.),
        });
        self.find_probe.set(None);
        self.find_reveal_frames = Some(0);
    }

    /// One frame of settling the active match into view, run after the frame painted it.
    fn settle_find_reveal(&mut self, cx: &mut Context<Self>) {
        let Some(frames) = self.find_reveal_frames else {
            return;
        };
        let frames = frames + 1;
        let viewport = self.list.viewport_bounds();
        match self.find_probe.get() {
            // Not painted yet: the list is still measuring around the row.
            None => {}
            Some(rect) => {
                let (rect_top, rect_bottom) = (
                    f32::from(rect.top()) as f64,
                    f32::from(rect.bottom()) as f64,
                );
                let (view_top, view_bottom) = (
                    f32::from(viewport.top()) as f64,
                    f32::from(viewport.bottom()) as f64,
                );
                // The composer sits below the list, not over it: no end inset.
                let margin = reveal_margin(view_bottom - view_top, 0.0, rect_bottom - rect_top);
                let delta = vertical_reveal_delta(
                    rect_top,
                    rect_bottom,
                    view_top,
                    view_bottom,
                    margin,
                    0.0,
                );
                if is_visible_delta(delta) {
                    self.list.scroll_by(px(delta as f32));
                } else {
                    self.find_reveal_frames = None;
                    return;
                }
            }
        }
        self.find_reveal_frames = (frames < FIND_REVEAL_MAX_FRAMES).then_some(frames);
        cx.notify();
    }

    fn rows_input(&self, snapshot: &ThreadTimelineSnapshot) -> MessagesTimelineRowsInput {
        MessagesTimelineRowsInput {
            timeline_entries: snapshot.timeline_entries.clone(),
            latest_run: snapshot.latest_run.clone(),
            running_run_id: snapshot.running_run_id.clone(),
            expanded_run_ids: self.ui.expanded_run_ids.clone(),
            expanded_attempt_ids: self.ui.expanded_attempt_ids.clone(),
            expanded_work_group_ids: self.ui.expanded_work_group_ids.clone(),
            is_working: snapshot.is_working,
            runless_work_active: snapshot.runless_work_active,
            active_turn_started_at: snapshot.active_turn_started_at.clone(),
            turn_diff_summaries: snapshot.turn_diff_summaries.clone(),
            supports_conversation_rollback: snapshot.supports_conversation_rollback,
            live_agent_task_ids: (!snapshot.live_agent_task_ids.is_empty())
                .then(|| snapshot.live_agent_task_ids.iter().cloned().collect()),
        }
    }

    /// Re-derives rows and splices the list over the changed range only.
    fn recompute(&mut self, cx: &mut Context<Self>) {
        let started = std::time::Instant::now();
        let snapshot = self.store.read(cx).snapshot().clone();
        if self.history != snapshot.history {
            self.history = snapshot.history.clone();
            // Re-measure the header: its height follows the history row.
            self.list.splice(0..1, 1);
        }
        let input = self.rows_input(&snapshot);
        let projection = derive_messages_timeline_rows_with_state(input, self.projection.as_ref());
        self.stable = compute_stable_messages_timeline_rows(projection.rows.clone(), &self.stable);
        self.projection = Some(projection);
        let next = self.stable.result.clone();
        let previous = std::mem::replace(&mut self.rows, next.clone());
        if !Arc::ptr_eq(&previous, &next) {
            let prefix = previous
                .iter()
                .zip(next.iter())
                .take_while(|(a, b)| Arc::ptr_eq(a, b))
                .count();
            let max_suffix = previous.len().min(next.len()) - prefix;
            let suffix = previous
                .iter()
                .rev()
                .zip(next.iter().rev())
                .take(max_suffix)
                .take_while(|(a, b)| Arc::ptr_eq(a, b))
                .count();
            // Index 0 is the list header.
            let old_range = 1 + prefix..1 + previous.len() - suffix;
            let count = next.len() - prefix - suffix;
            if old_range.end - old_range.start + count > 0 {
                self.list.splice(old_range, count);
            }
        }
        tracing::debug!(
            entries = snapshot.timeline_entries.len(),
            rows = self.rows.len(),
            elapsed_us = started.elapsed().as_micros() as u64,
            "timeline rows derived"
        );
        self.update_ticker(snapshot.is_working, cx);
    }

    /// `WorkingTimer` ticks once a second; only while the thread works.
    fn update_ticker(&mut self, working: bool, cx: &mut Context<Self>) {
        if !working {
            self.ticker = None;
            return;
        }
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
    }

    /// The list header: the spacer, plus `TimelineHistoryControl` while earlier
    /// turns remain (or a load failed).
    fn render_header(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(history) = self.history.clone() else {
            return div().h(rems(1.)).into_any_element();
        };
        let theme = cx.app_theme().clone();
        let source = self.store.read(cx).source();
        let mut lane = div()
            .flex()
            .flex_col()
            .gap(rems(0.375))
            .pb(rems(0.5))
            .w_full()
            .min_w_0();
        if history.has_more_history {
            let label = if history.loading {
                "Loading earlier turns\u{2026}"
            } else {
                "Load earlier turns"
            };
            let loading = history.loading;
            let muted = with_alpha(theme.muted_foreground, 0.6);
            let hover = theme.foreground;
            lane = lane.child(
                div()
                    .id("load-earlier-turns")
                    .w_full()
                    .py(rems(0.375))
                    .flex()
                    .justify_center()
                    .text_size(TEXT_XS.size)
                    .text_color(muted)
                    .when(!loading, |button| {
                        button
                            .cursor_pointer()
                            .hover(move |style| style.text_color(hover))
                            .on_click(move |_, _, cx| {
                                if let Some(source) = source.as_ref() {
                                    source.load_earlier_history(cx);
                                }
                            })
                    })
                    .child(label),
            );
        }
        if let Some(error) = history.error.clone() {
            lane = lane.child(
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(error),
            );
        }
        div()
            .w_full()
            .px(rems(3.))
            .child(
                div()
                    .mx_auto()
                    .w_full()
                    .min_w_0()
                    .max_w(CHAT_CONTENT_MAX_WIDTH)
                    .pt(rems(1.))
                    .child(lane),
            )
            .into_any_element()
    }

    fn render_item(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let row_count = self.rows.len();
        // `TIMELINE_LIST_HEADER` / footer: `h-3 sm:h-4`.
        if ix == 0 {
            return self.render_header(cx);
        }
        if ix > row_count {
            return div().h(rems(1.)).into_any_element();
        }
        let row = self.rows[ix - 1].clone();
        tracing::trace!(ix, kind = row.kind().as_str(), "render timeline row");
        let (citation_message, citation_opacity) = match &self.pulse {
            Some((id, started)) => {
                let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                (
                    Some(id.clone()),
                    citation_highlight_opacity(elapsed, false) as f32,
                )
            }
            None => (None, 0.0),
        };
        let row_cx = RowCx {
            theme: cx.app_theme().clone(),
            view: cx.entity(),
            source: self.store.read(cx).source(),
            snapshot: self.store.read(cx).snapshot().clone(),
            ui: self.ui.clone(),
            timestamp_format: self.timestamp_format,
            order: (ix as u64) << 20,
            citation_message,
            citation_opacity,
            find: (!self.find.query.is_empty()).then(|| RowFind {
                query: self.find.query.clone().into(),
                active: self.find.active.clone(),
                probe: self.find_probe.clone(),
            }),
        };
        // `.messages-timeline-scroll` padding-inline (3rem at ≥640px) and the
        // `.chat-content-lane` max width.
        div()
            .w_full()
            .px(rems(3.))
            .child(
                div()
                    .mx_auto()
                    .w_full()
                    .min_w_0()
                    .max_w(CHAT_CONTENT_MAX_WIDTH)
                    .child(timeline_row_content(&row, &row_cx)),
            )
            .into_any_element()
    }
}

/// `TimelineRowContent`: per-kind trailing space plus the row body.
fn timeline_row_content(row: &MessagesTimelineRow, cx: &RowCx) -> AnyElement {
    use MessagesTimelineRowContent as C;
    let content = &row.content;
    let is_subagent_group = matches!(content, C::Event { projected_item, .. }
        if projected_item.item.item_type() == super::timeline::turn_item::TurnItemType::Subagent);
    let is_work_log_row = matches!(
        content,
        C::Work { .. } | C::WorkLive { .. } | C::WorkToggle { .. } | C::Thinking { .. }
    );
    let padding = if is_work_log_row || is_subagent_group {
        rems(0.)
    } else {
        match content {
            C::TurnFold { .. } | C::Working { .. } => rems(0.375),
            C::Message {
                message,
                show_assistant_meta,
                ..
            } if message.role == super::timeline::types::MessageRole::Assistant
                && !show_assistant_meta =>
            {
                rems(0.5)
            }
            C::Event { .. } | C::AttemptFold { .. } => rems(0.5),
            _ => rems(1.),
        }
    };
    let body: AnyElement = if is_work_log_row {
        let expanded_header = match content {
            C::WorkToggle { expanded, .. } | C::WorkLive { expanded, .. } => *expanded,
            C::Thinking { expanded, .. } => expanded.unwrap_or(false),
            _ => false,
        };
        let layout = match content {
            C::Work {
                is_expanded_tool_group: true,
                ..
            } => WorkLogLayout::GroupContent,
            _ if expanded_header => WorkLogLayout::GroupHeader,
            _ => WorkLogLayout::Standalone,
        };
        let inner = match content {
            C::Work { .. } => work_group_row(row, cx),
            C::WorkLive { .. } => work_live_row(row, cx),
            C::WorkToggle { .. } => work_toggle_row(row, cx),
            C::Thinking { .. } => thinking_row(row, cx),
            _ => div().into_any_element(),
        };
        work_log_block(layout, row.continues_work_log, inner).into_any_element()
    } else {
        match content {
            C::TurnFold { .. } => turn_fold_row(row, cx),
            C::AttemptFold { .. } => attempt_fold_row(row, cx),
            C::ContextCompaction { .. } => context_compaction_row(row, cx),
            C::Message { message, .. } => {
                if message.role == super::timeline::types::MessageRole::User {
                    user_row(row, cx)
                } else {
                    assistant_row(row, cx)
                }
            }
            C::AssistantMeta { .. } => assistant_meta_row(row, cx),
            C::ProposedPlan { .. } => proposed_plan_row(row, cx),
            C::Working { .. } => working_row(row, cx),
            C::Event { .. } => event_row(row, cx),
            _ => div().into_any_element(),
        }
    };
    div()
        .w_full()
        .min_w_0()
        .pb(padding)
        .child(body)
        .into_any_element()
}

impl Render for MessagesTimeline {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        if self.find_reveal_frames.is_some() {
            let timeline = cx.entity();
            window.on_next_frame(move |_, cx| {
                timeline.update(cx, |this, cx| this.settle_find_reveal(cx));
            });
        }
        let snapshot = self.store.read(cx).snapshot().clone();
        if self.rows.is_empty() && !snapshot.is_working {
            return empty_state(&theme).into_any_element();
        }
        div()
            .relative()
            .flex_1()
            .size_full()
            .min_h_0()
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, ix, window, cx| this.render_item(ix, window, cx)),
                )
                .size_full(),
            )
            .vertical_scrollbar(&self.list)
            .into_any_element()
    }
}

/// Empty server thread.
fn empty_state(theme: &AppTheme) -> impl IntoElement {
    div()
        .flex()
        .flex_1()
        .size_full()
        .items_center()
        .justify_center()
        .px(rems(3.))
        .text_size(TEXT_SM.size)
        .text_color(with_alpha(theme.muted_foreground, 0.3))
        .child("Send a message to start the conversation.")
}
