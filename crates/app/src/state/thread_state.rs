//! Port of upstream/packages/client-runtime/src/state/threadState.ts and the
//! state machine of `threads.ts` (`makeEnvironmentThreadState`): what the
//! store does with a thread's stream items.
//!
//! Dropped with the transport: the HTTP snapshot loaders, the persistent
//! thread cache, resume cursors / `afterSequence` negotiation, the RPC
//! subscription lifecycle and its retry. Kept: sequence filtering, snapshot
//! replacement, the `synchronizing`/`live`/`cached`/`deleted` statuses,
//! deletion, and the partial-timeline (progressive history) event handling.

use std::sync::Arc;

use crate::state::contracts::{
    OrchestrationV2DomainEvent, OrchestrationV2ThreadProjection, OrchestrationV2ThreadStreamItem,
};
use crate::state::orchestration_v2_projection::{
    ApplyOrchestrationV2ProjectionEventOptions, apply_orchestration_v2_projection_event,
};
use crate::state::thread_history_merge::{EMPTY_THREAD_HISTORY_META, ThreadHistoryMeta};
use crate::state::thread_projection::ThreadProjection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvironmentThreadStatus {
    Empty,
    Cached,
    Synchronizing,
    Live,
    Deleted,
}

/// One frame of a thread's stream.
#[derive(Clone, Debug, PartialEq)]
pub enum ThreadStreamItem {
    Synchronized,
    Snapshot {
        snapshot_sequence: u64,
        projection: Arc<ThreadProjection>,
        /// Bounded (windowed) snapshots carry their history cursor; full ones
        /// omit it and replace progressive state.
        history: Option<ThreadHistoryMeta>,
    },
    Event {
        sequence: u64,
        event: Box<OrchestrationV2DomainEvent>,
    },
}

impl From<OrchestrationV2ThreadStreamItem> for ThreadStreamItem {
    /// `UnknownEvent` (an event type a newer build emitted) has no in-process
    /// producer; it maps to an empty `Synchronized`-free no-op by being dropped
    /// by the caller. Use [`ThreadStreamItem::from_contract`] to keep the cursor.
    fn from(item: OrchestrationV2ThreadStreamItem) -> Self {
        match item {
            OrchestrationV2ThreadStreamItem::Synchronized => Self::Synchronized,
            OrchestrationV2ThreadStreamItem::Snapshot {
                snapshot_sequence,
                projection,
                history_cursor,
                has_more_history,
                latest_local_turn_ordinal,
                ..
            } => {
                let has_progressive_history = history_cursor.is_some()
                    || has_more_history.is_some()
                    || latest_local_turn_ordinal.is_some();
                Self::Snapshot {
                    snapshot_sequence,
                    projection: Arc::new(projection.into()),
                    history: has_progressive_history.then(|| ThreadHistoryMeta {
                        history_cursor: history_cursor
                            .flatten()
                            .map(|cursor| cursor.as_str().to_owned()),
                        has_more_history: has_more_history.unwrap_or(false),
                        loading: false,
                        error: None,
                        expanded: false,
                        latest_local_turn_ordinal: latest_local_turn_ordinal.flatten(),
                    }),
                }
            }
            OrchestrationV2ThreadStreamItem::Event { sequence, event } => Self::Event {
                sequence,
                event: Box::new(event),
            },
            // Same build on both ends: cannot happen. Treat as an already-seen cursor bump.
            OrchestrationV2ThreadStreamItem::UnknownEvent { .. } => Self::Synchronized,
        }
    }
}

/// `EnvironmentThreadState`.
#[derive(Clone, Debug, PartialEq)]
pub struct EnvironmentThreadState {
    pub data: Option<Arc<ThreadProjection>>,
    pub status: EnvironmentThreadStatus,
    pub error: Option<String>,
    /// Progressive history cursor for bounded hydration. Cleared on full snapshots.
    pub history: ThreadHistoryMeta,
    /// Highest event sequence applied (`lastSequence`).
    pub applied_sequence: u64,
    /// The server marks the end of a replay with `synchronized`; until then a
    /// subscription that asked for the marker reports `synchronizing`.
    awaiting_completion: bool,
}

pub fn empty_environment_thread_state() -> EnvironmentThreadState {
    EnvironmentThreadState {
        data: None,
        status: EnvironmentThreadStatus::Empty,
        error: None,
        history: EMPTY_THREAD_HISTORY_META,
        applied_sequence: 0,
        awaiting_completion: false,
    }
}

impl Default for EnvironmentThreadState {
    fn default() -> Self {
        empty_environment_thread_state()
    }
}

fn status_without_live_data(data: &Option<Arc<ThreadProjection>>) -> EnvironmentThreadStatus {
    if data.is_some() {
        EnvironmentThreadStatus::Cached
    } else {
        EnvironmentThreadStatus::Empty
    }
}

impl EnvironmentThreadState {
    /// Mark the stream as (re)connecting: `synchronizing` unless deleted or failed.
    /// `awaiting_completion` is true when the producer will send `Synchronized`.
    pub fn set_connecting(&mut self, awaiting_completion: bool) {
        self.awaiting_completion = awaiting_completion;
        if self.status == EnvironmentThreadStatus::Deleted || self.error.is_some() {
            return;
        }
        self.status = EnvironmentThreadStatus::Synchronizing;
        self.error = None;
    }

    /// The stream dropped.
    pub fn set_disconnected(&mut self) {
        self.awaiting_completion = false;
        if self.status != EnvironmentThreadStatus::Deleted {
            self.status = status_without_live_data(&self.data);
        }
    }

    pub fn set_stream_error(&mut self, message: impl Into<String>) {
        self.awaiting_completion = false;
        if self.status != EnvironmentThreadStatus::Deleted {
            self.status = status_without_live_data(&self.data);
        }
        self.error = Some(message.into());
    }

    fn set_thread(&mut self, thread: Arc<ThreadProjection>, history: Option<ThreadHistoryMeta>) {
        self.history = history.unwrap_or(EMPTY_THREAD_HISTORY_META);
        self.data = Some(thread);
        // Buffered values from a failed attempt can arrive after its error.
        self.status = if self.error.is_some() {
            EnvironmentThreadStatus::Cached
        } else if self.awaiting_completion {
            EnvironmentThreadStatus::Synchronizing
        } else {
            EnvironmentThreadStatus::Live
        };
    }

    fn set_deleted(&mut self) {
        self.awaiting_completion = false;
        self.data = None;
        self.status = EnvironmentThreadStatus::Deleted;
        self.error = None;
        self.history = EMPTY_THREAD_HISTORY_META;
    }

    fn apply_events(&mut self, items: Vec<(u64, OrchestrationV2DomainEvent)>) {
        let applied_sequence = self.applied_sequence;
        let mut sequence = applied_sequence;
        let mut fresh: Vec<OrchestrationV2DomainEvent> = Vec::new();
        for (item_sequence, event) in items {
            if item_sequence <= sequence {
                continue;
            }
            sequence = item_sequence;
            fresh.push(event);
        }
        if sequence == applied_sequence {
            return;
        }
        self.applied_sequence = sequence;
        if fresh.is_empty() {
            return;
        }

        let waiting = self.awaiting_completion;
        let mut deleted = false;
        for event in fresh {
            if self.status == EnvironmentThreadStatus::Deleted {
                continue;
            }
            let is_deleted = matches!(&event, OrchestrationV2DomainEvent::ThreadDeleted { .. });
            let Some(current) = self.data.clone() else {
                if is_deleted {
                    deleted = true;
                    break;
                }
                continue;
            };
            if is_deleted {
                deleted = true;
                break;
            }
            // Incomplete progressive windows only (hasMore or open cursor). Do not
            // use expanded: it remains true after the last page as a cache marker.
            // Full/web and fully-loaded timelines keep default append-on-miss.
            let partial = (self.history.has_more_history || self.history.history_cursor.is_some())
                .then_some(ApplyOrchestrationV2ProjectionEventOptions {
                    partial_timeline: true,
                    latest_local_turn_ordinal: self.history.latest_local_turn_ordinal,
                });
            let ordinal = match &event {
                OrchestrationV2DomainEvent::TurnItemUpdated { payload, .. } => {
                    use crate::state::support::turn_item::TurnItemExt;
                    Some(payload.ordinal())
                }
                _ => None,
            };
            let Some(next) = apply_orchestration_v2_projection_event(&current, event, partial)
            else {
                // True no-op: do not clear stream error/status.
                continue;
            };
            if partial.is_some()
                && let Some(ordinal) = ordinal
                && self
                    .history
                    .latest_local_turn_ordinal
                    .is_none_or(|watermark| ordinal > watermark)
            {
                self.history.latest_local_turn_ordinal = Some(ordinal);
            }
            self.data = Some(Arc::new(next));
            self.status = if waiting {
                EnvironmentThreadStatus::Synchronizing
            } else {
                EnvironmentThreadStatus::Live
            };
            self.error = None;
        }
        if deleted {
            self.set_deleted();
        }
    }

    fn apply_item(&mut self, item: ThreadStreamItem) {
        match item {
            ThreadStreamItem::Synchronized => {
                self.awaiting_completion = false;
                if self.data.is_some()
                    && self.status != EnvironmentThreadStatus::Deleted
                    && self.error.is_none()
                {
                    self.status = EnvironmentThreadStatus::Live;
                    self.error = None;
                }
            }
            ThreadStreamItem::Snapshot {
                snapshot_sequence,
                projection,
                history,
            } => {
                self.applied_sequence = snapshot_sequence;
                // Bounded socket fallbacks carry their cursor. Legacy-compatible full
                // snapshots omit these fields and still replace progressive state.
                self.set_thread(projection, history);
            }
            ThreadStreamItem::Event { sequence, event } => {
                self.apply_events(vec![(sequence, *event)]);
            }
        }
    }

    /// Applies one batch of stream items in order. Returns whether the state changed.
    pub fn apply_items(&mut self, items: Vec<ThreadStreamItem>) -> bool {
        let before = self.clone();
        let mut events: Vec<(u64, OrchestrationV2DomainEvent)> = Vec::new();
        for item in items {
            match item {
                ThreadStreamItem::Event { sequence, event }
                    if !matches!(*event, OrchestrationV2DomainEvent::ThreadDeleted { .. }) =>
                {
                    events.push((sequence, *event));
                }
                item => {
                    self.apply_events(std::mem::take(&mut events));
                    self.apply_item(item);
                }
            }
        }
        self.apply_events(events);
        *self != before
    }

    /// Replace the timeline with a freshly loaded full thread (initial open).
    pub fn install_snapshot(
        &mut self,
        snapshot_sequence: u64,
        projection: OrchestrationV2ThreadProjection,
    ) {
        self.apply_item(ThreadStreamItem::Snapshot {
            snapshot_sequence,
            projection: Arc::new(projection.into()),
            history: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_fixtures::{from_json, v2_projection};
    use serde_json::json;

    const NOW: &str = "2026-06-20T00:00:00.000Z";

    fn snapshot(sequence: u64) -> ThreadStreamItem {
        ThreadStreamItem::Snapshot {
            snapshot_sequence: sequence,
            projection: Arc::new(v2_projection().into()),
            history: None,
        }
    }

    fn archived_event(sequence: u64, title: &str) -> ThreadStreamItem {
        let mut thread = serde_json::to_value(v2_projection().thread).unwrap();
        thread["title"] = json!(title);
        ThreadStreamItem::Event {
            sequence,
            event: Box::new(from_json(json!({
                "id": format!("event-{sequence}"), "type": "thread.metadata-updated",
                "threadId": "thread-v2", "occurredAt": NOW, "payload": thread,
            }))),
        }
    }

    fn deleted_event(sequence: u64) -> ThreadStreamItem {
        let thread = serde_json::to_value(v2_projection().thread).unwrap();
        ThreadStreamItem::Event {
            sequence,
            event: Box::new(from_json(json!({
                "id": format!("event-{sequence}"), "type": "thread.deleted",
                "threadId": "thread-v2", "occurredAt": NOW, "payload": thread,
            }))),
        }
    }

    fn title(state: &EnvironmentThreadState) -> String {
        state.data.as_ref().unwrap().thread.title.to_string()
    }

    #[test]
    fn a_snapshot_goes_live_and_events_after_its_sequence_apply() {
        let mut state = EnvironmentThreadState::default();
        state.set_connecting(false);
        assert_eq!(state.status, EnvironmentThreadStatus::Synchronizing);
        assert!(state.apply_items(vec![snapshot(7), archived_event(8, "Renamed")]));
        assert_eq!(state.status, EnvironmentThreadStatus::Live);
        assert_eq!(title(&state), "Renamed");
        assert_eq!(state.applied_sequence, 8);
    }

    #[test]
    fn events_at_or_below_the_applied_sequence_are_ignored() {
        let mut state = EnvironmentThreadState::default();
        state.apply_items(vec![snapshot(7)]);
        let before = state.clone();
        assert!(!state.apply_items(vec![archived_event(7, "Stale"), archived_event(5, "Older")]));
        assert_eq!(state, before);
    }

    #[test]
    fn waits_for_the_completion_marker_when_the_producer_sends_one() {
        let mut state = EnvironmentThreadState::default();
        state.set_connecting(true);
        state.apply_items(vec![snapshot(7), archived_event(8, "Renamed")]);
        assert_eq!(state.status, EnvironmentThreadStatus::Synchronizing);
        state.apply_items(vec![ThreadStreamItem::Synchronized]);
        assert_eq!(state.status, EnvironmentThreadStatus::Live);
    }

    #[test]
    fn a_dropped_stream_keeps_the_data_as_cached() {
        let mut state = EnvironmentThreadState::default();
        state.apply_items(vec![snapshot(7)]);
        state.set_disconnected();
        assert_eq!(state.status, EnvironmentThreadStatus::Cached);
        assert!(state.data.is_some());
        let mut empty = EnvironmentThreadState::default();
        empty.set_disconnected();
        assert_eq!(empty.status, EnvironmentThreadStatus::Empty);
    }

    #[test]
    fn a_stream_error_keeps_the_data_and_reports_the_error() {
        let mut state = EnvironmentThreadState::default();
        state.apply_items(vec![snapshot(7)]);
        state.set_stream_error("Could not synchronize the thread.");
        assert_eq!(state.status, EnvironmentThreadStatus::Cached);
        assert_eq!(
            state.error.as_deref(),
            Some("Could not synchronize the thread.")
        );
        // A snapshot that lands after the error stays cached until a retry clears it.
        state.apply_items(vec![snapshot(9)]);
        assert_eq!(state.status, EnvironmentThreadStatus::Cached);
        state.set_connecting(false);
        // `setConnecting` leaves an errored state alone; a fresh attempt clears it first.
        assert!(state.error.is_some());
    }

    #[test]
    fn deletion_clears_the_data_and_sticks() {
        let mut state = EnvironmentThreadState::default();
        state.apply_items(vec![
            snapshot(7),
            archived_event(8, "x"),
            deleted_event(9),
            archived_event(10, "y"),
        ]);
        assert_eq!(state.status, EnvironmentThreadStatus::Deleted);
        assert!(state.data.is_none());
        assert_eq!(state.history, EMPTY_THREAD_HISTORY_META);
        // Disconnects do not resurrect a deleted thread.
        state.set_disconnected();
        assert_eq!(state.status, EnvironmentThreadStatus::Deleted);
    }

    #[test]
    fn deleting_an_unloaded_thread_marks_it_deleted() {
        let mut state = EnvironmentThreadState::default();
        state.apply_items(vec![deleted_event(3)]);
        assert_eq!(state.status, EnvironmentThreadStatus::Deleted);
    }

    #[test]
    fn a_full_snapshot_resets_progressive_history_but_a_bounded_one_keeps_its_cursor() {
        let mut state = EnvironmentThreadState::default();
        let bounded = ThreadHistoryMeta {
            history_cursor: Some("cursor".into()),
            has_more_history: true,
            latest_local_turn_ordinal: Some(40),
            ..ThreadHistoryMeta::default()
        };
        state.apply_items(vec![ThreadStreamItem::Snapshot {
            snapshot_sequence: 3,
            projection: Arc::new(v2_projection().into()),
            history: Some(bounded.clone()),
        }]);
        assert_eq!(state.history, bounded);
        state.apply_items(vec![snapshot(4)]);
        assert_eq!(state.history, EMPTY_THREAD_HISTORY_META);
    }

    #[test]
    fn partial_windows_track_the_local_turn_ordinal_watermark() {
        let mut state = EnvironmentThreadState::default();
        state.apply_items(vec![ThreadStreamItem::Snapshot {
            snapshot_sequence: 3,
            projection: Arc::new(v2_projection().into()),
            history: Some(ThreadHistoryMeta {
                history_cursor: Some("cursor".into()),
                has_more_history: true,
                latest_local_turn_ordinal: Some(10),
                ..ThreadHistoryMeta::default()
            }),
        }]);
        let item = |ordinal: u64| ThreadStreamItem::Event {
            sequence: 4 + ordinal,
            event: Box::new(from_json(json!({
                "id": format!("e{ordinal}"), "type": "turn-item.updated", "threadId": "thread-v2",
                "occurredAt": NOW,
                "payload": {
                    "id": format!("item-{ordinal}"), "threadId": "thread-v2", "runId": null, "nodeId": null,
                    "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
                    "parentItemId": null, "ordinal": ordinal, "status": "completed", "title": null,
                    "startedAt": null, "completedAt": null, "updatedAt": NOW,
                    "type": "command_execution", "input": "pwd", "output": "x", "exitCode": 0,
                },
            }))),
        };
        // 11 is above the watermark: shown, and raises it. 9 is below it: dropped.
        state.apply_items(vec![item(11), item(9)]);
        assert_eq!(state.history.latest_local_turn_ordinal, Some(11));
        let ids: Vec<String> = state
            .data
            .as_ref()
            .unwrap()
            .visible_turn_items
            .iter()
            .map(|row| row.source_item_id.to_string())
            .collect();
        assert_eq!(ids, vec!["item-11"]);
    }
}
