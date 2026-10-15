//! The seam between the orchestration read model and the thread view.
//!
//! [`ThreadTimelineStore`] is a GPUI model holding the current
//! [`ThreadTimelineSnapshot`]; the view observes it and re-derives rows when
//! it changes. Whatever feeds the store implements [`TimelineSource`]:
//! it publishes snapshots and receives the user's responses (approvals,
//! answers, rollbacks, …) — upstream's `MessagesTimelineProps` callbacks and
//! the composer's request handlers.
//!
//! T14 replaces [`MockTimelineSource`] with the real reducers: build the
//! snapshot from the orchestration projection (visible turn items, runs,
//! attempts, plans, checkpoints, pending requests) and call
//! [`ThreadTimelineStore::publish`]; route the `respond_*` calls to the
//! orchestrator.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::{App, AppContext as _, Context, Entity};

use super::fixtures::{FixtureThread, startup_thread, with_pending_question};
use super::session_logic::{TimelineEntriesInput, derive_timeline_entries_from_visible_turn_items};
use super::turn_item::{ProviderApprovalDecision, TurnItemPayload};
use super::types::{RunStatus, ThreadTimelineSnapshot};

/// User responses the view sends back to whatever owns the thread.
pub trait TimelineSource: 'static {
    /// `onRespondToApproval`.
    fn respond_to_approval(&self, request_id: &str, decision: ProviderApprovalDecision, cx: &mut App);
    /// The composer's pending-user-input submit: answers keyed by question id.
    fn respond_to_user_input(&self, request_id: &str, answers: Vec<(String, String)>, cx: &mut App);
    /// `onDismissActivePendingUserInput`.
    fn dismiss_user_input(&self, request_id: &str, cx: &mut App);
    /// `onRevertToTurnCount` ("Edit from here").
    fn revert_to_turn_count(&self, turn_count: u64, message_id: &str, cx: &mut App);
    /// `onOpenTurnDiff`: the diff panel for a run (and file).
    fn open_turn_diff(&self, run_id: &str, file_path: Option<&str>, cx: &mut App);
    /// `onOpenThread`: navigate to another thread (subagents, created threads).
    fn open_thread(&self, thread_id: &str, cx: &mut App);
}

/// The observable snapshot holder.
pub struct ThreadTimelineStore {
    snapshot: Arc<ThreadTimelineSnapshot>,
    source: Option<Arc<dyn TimelineSource>>,
}

impl ThreadTimelineStore {
    pub fn new(snapshot: ThreadTimelineSnapshot) -> Self {
        Self {
            snapshot: Arc::new(snapshot),
            source: None,
        }
    }

    pub fn snapshot(&self) -> &Arc<ThreadTimelineSnapshot> {
        &self.snapshot
    }

    pub fn source(&self) -> Option<Arc<dyn TimelineSource>> {
        self.source.clone()
    }

    pub fn set_source(&mut self, source: Arc<dyn TimelineSource>) {
        self.source = Some(source);
    }

    /// Replace the snapshot and notify observers.
    pub fn publish(&mut self, snapshot: ThreadTimelineSnapshot, cx: &mut Context<Self>) {
        self.snapshot = Arc::new(snapshot);
        cx.notify();
    }
}

/// Builds a snapshot from fixture turn items the way the client runtime
/// would from a projection.
pub fn snapshot_from_fixture(thread: &FixtureThread) -> ThreadTimelineSnapshot {
    let timeline_entries = derive_timeline_entries_from_visible_turn_items(&TimelineEntriesInput {
        visible_turn_items: thread.items.clone(),
        ..Default::default()
    });
    let latest_run = thread.latest_run();
    let running = latest_run
        .as_ref()
        .filter(|run| matches!(run.status, RunStatus::Running | RunStatus::Starting | RunStatus::Waiting));
    ThreadTimelineSnapshot {
        thread_id: thread.thread_id.clone(),
        timeline_entries,
        running_run_id: running.map(|run| run.run_id.clone()),
        is_working: running.is_some(),
        active_turn_started_at: running.and_then(|run| run.started_at.clone()),
        latest_run,
        turn_diff_summaries: thread.checkpoints.clone(),
        supports_conversation_rollback: true,
        pending_approvals: thread.pending_approvals.clone(),
        pending_user_inputs: thread.pending_user_inputs.clone(),
        workspace_root: thread.workspace_root.clone(),
        ..Default::default()
    }
}

/// Fixture-backed source. Streams the open assistant message in small
/// chunks so the timeline's live behaviour (stick-to-bottom, streaming
/// markdown, working timer) is exercised, and resolves requests locally.
pub struct MockTimelineSource {
    store: Entity<ThreadTimelineStore>,
    thread: Arc<parking_lot_free::Mutex<FixtureThread>>,
}

/// A tiny std-mutex wrapper so the mock does not need another dependency.
mod parking_lot_free {
    pub struct Mutex<T>(std::sync::Mutex<T>);
    impl<T> Mutex<T> {
        pub fn new(value: T) -> Self {
            Self(std::sync::Mutex::new(value))
        }
        pub fn lock(&self) -> std::sync::MutexGuard<'_, T> {
            self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
        }
    }
}

const STREAM_TAIL: &str = " confirm both screenshots look right, then commit them under `docs/screenshots/T16/`.\n\n| Theme | File |\n| --- | --- |\n| Light | `light.png` |\n| Dark | `dark.png` |\n\nThe timeline keeps following the bottom while this streams, unless you scroll up.";

impl MockTimelineSource {
    /// Creates the store for the startup fixture and starts streaming.
    pub fn start(cx: &mut App) -> Entity<ThreadTimelineStore> {
        let mut thread = startup_thread();
        if std::env::var("TURIN_PENDING").is_ok_and(|v| v == "question") {
            thread.pending_approvals.clear();
            thread = with_pending_question(thread);
        }
        let store = cx.new(|_| ThreadTimelineStore::new(snapshot_from_fixture(&thread)));
        let source = Arc::new(Self {
            store: store.clone(),
            thread: Arc::new(parking_lot_free::Mutex::new(thread)),
        });
        store.update(cx, |store, _| store.set_source(source.clone()));
        if std::env::var("TURIN_STREAM").is_ok_and(|v| v == "0") {
            return store;
        }
        let stream_source = source.clone();
        cx.spawn(async move |cx| {
            let words: Vec<String> = STREAM_TAIL
                .split_inclusive(' ')
                .map(str::to_string)
                .collect();
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            for word in words {
                cx.background_executor().timer(Duration::from_millis(60)).await;
                let snapshot = {
                    let mut thread = stream_source.thread.lock();
                    let Some(item) = thread.items.iter_mut().rev().find(|item| {
                        matches!(item.item.payload, TurnItemPayload::AssistantMessage { streaming: true, .. })
                    }) else {
                        return;
                    };
                    if let TurnItemPayload::AssistantMessage { text, .. } = &mut item.item.payload {
                        text.push_str(&word);
                    }
                    snapshot_from_fixture(&thread)
                };
                if cx
                    .update(|cx| {
                        stream_source
                            .store
                            .update(cx, |store, cx| store.publish(snapshot, cx))
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
        store
    }

    fn republish(&self, cx: &mut App) {
        let snapshot = snapshot_from_fixture(&self.thread.lock());
        self.store.update(cx, |store, cx| store.publish(snapshot, cx));
    }
}

impl TimelineSource for MockTimelineSource {
    fn respond_to_approval(&self, request_id: &str, decision: ProviderApprovalDecision, cx: &mut App) {
        tracing::info!(request_id, ?decision, "mock: approval response");
        self.thread
            .lock()
            .pending_approvals
            .retain(|approval| approval.request_id != request_id);
        self.republish(cx);
    }

    fn respond_to_user_input(&self, request_id: &str, answers: Vec<(String, String)>, cx: &mut App) {
        tracing::info!(request_id, ?answers, "mock: user input response");
        self.dismiss_user_input(request_id, cx);
    }

    fn dismiss_user_input(&self, request_id: &str, cx: &mut App) {
        self.thread
            .lock()
            .pending_user_inputs
            .retain(|input| input.request_id != request_id);
        self.republish(cx);
    }

    fn revert_to_turn_count(&self, turn_count: u64, message_id: &str, _: &mut App) {
        tracing::info!(turn_count, message_id, "mock: revert to turn count");
    }

    fn open_turn_diff(&self, run_id: &str, file_path: Option<&str>, _: &mut App) {
        tracing::info!(run_id, ?file_path, "mock: open turn diff");
    }

    fn open_thread(&self, thread_id: &str, _: &mut App) {
        tracing::info!(thread_id, "mock: open thread");
    }
}
