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
use std::time::Duration;

use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, FollowMode, IntoElement, ListAlignment, ListState,
    ParentElement as _, Render, Styled as _, Subscription, Task, Window, div, list, px, rems,
};

use super::timeline::messages_timeline_logic::{
    MessagesTimelineRow, MessagesTimelineRowContent, MessagesTimelineRowsInput,
    MessagesTimelineRowsProjection, StableMessagesTimelineRowsState,
    compute_stable_messages_timeline_rows, derive_messages_timeline_rows_with_state,
};
use super::timeline::source::{ThreadTimelineStore, TimelineSource};
use super::timeline::types::ThreadTimelineSnapshot;
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
use crate::theme::tokens::{CHAT_CONTENT_MAX_WIDTH, TEXT_SM};
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
}

impl RowCx {
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
    list: ListState,
    projection: Option<MessagesTimelineRowsProjection>,
    stable: StableMessagesTimelineRowsState,
    rows: Arc<Vec<Arc<MessagesTimelineRow>>>,
    ui: Arc<TimelineUiState>,
    timestamp_format: TimestampFormat,
    ticker: Option<Task<()>>,
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
        let snapshot = self.store.read(cx).snapshot().clone();
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

    fn render_item(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let row_count = self.rows.len();
        // `TIMELINE_LIST_HEADER` / footer: `h-3 sm:h-4`.
        if ix == 0 || ix > row_count {
            return div().h(rems(1.)).into_any_element();
        }
        let row = self.rows[ix - 1].clone();
        let row_cx = RowCx {
            theme: cx.app_theme().clone(),
            view: cx.entity(),
            source: self.store.read(cx).source(),
            snapshot: self.store.read(cx).snapshot().clone(),
            ui: self.ui.clone(),
            timestamp_format: self.timestamp_format,
            order: (ix as u64) << 20,
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.app_theme().clone();
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
