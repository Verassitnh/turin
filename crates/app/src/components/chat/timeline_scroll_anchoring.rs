//! Port of upstream/apps/web/src/components/chat/timelineScrollAnchoring.ts
//!
//! Scroll bookkeeping for the messages timeline: which run is new (so its first
//! message anchors to the top), where the viewport sits relative to the rows, and
//! the per-thread remembered position for switching back to a thread.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use turin_contracts::base_schemas::{MessageId, RunAttemptId, RunId};

/// Matches the titlebar fade inset so draft promotion preserves the first row's position.
pub const CHAT_TIMELINE_ANCHOR_OFFSET: f64 = 24.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineScrollMode {
    FollowingEnd,
    AnchoringNewTurn,
    FreeScrolling,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineRunObservation {
    pub thread_key: Option<String>,
    pub hydrated: bool,
    pub run_id: Option<RunId>,
}

/// The inputs to [`observe_timeline_run`]: the observation plus the queue flag and the
/// message the new turn starts with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineRunInput {
    pub thread_key: Option<String>,
    pub hydrated: bool,
    pub run_id: Option<RunId>,
    pub queued: bool,
    pub message_id: Option<MessageId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedTimelineRun {
    pub observation: TimelineRunObservation,
    pub anchor_message_id: Option<MessageId>,
}

/// Opening a thread establishes a baseline; only later runs get new-turn framing.
pub fn observe_timeline_run(
    previous: Option<&TimelineRunObservation>,
    input: TimelineRunInput,
) -> ObservedTimelineRun {
    let observation = TimelineRunObservation {
        thread_key: input.thread_key.clone(),
        hydrated: input.hydrated,
        run_id: if input.queued {
            None
        } else {
            input.run_id.clone()
        },
    };
    let Some(previous) =
        previous.filter(|previous| previous.thread_key == input.thread_key && previous.hydrated)
    else {
        return ObservedTimelineRun {
            observation,
            anchor_message_id: None,
        };
    };
    if !input.hydrated
        || input.run_id.is_none()
        || input.queued
        || previous.run_id == input.run_id
        || input.message_id.is_none()
    {
        return ObservedTimelineRun {
            observation: previous.clone(),
            anchor_message_id: None,
        };
    }
    ObservedTimelineRun {
        observation,
        anchor_message_id: input.message_id,
    }
}

/// Row geometry as the list reports it. `None` means the row is not measured yet.
pub struct TimelineListMeasurementState<'a> {
    pub data_len: usize,
    pub scroll: f64,
    pub scroll_length: f64,
    pub position_at_index: &'a dyn Fn(usize) -> Option<f64>,
    pub size_at_index: &'a dyn Fn(usize) -> Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnchoredTurnMetrics {
    pub anchor_top: f64,
    pub last_bottom: f64,
    pub turn_height: f64,
    pub usable_viewport_height: f64,
    pub visible_usable_bottom: f64,
    pub overflows_usable_viewport: bool,
    pub target_scroll_to_reveal_end: f64,
    pub scroll_delta_to_reveal_end: f64,
}

pub fn get_row_bottom(state: &TimelineListMeasurementState<'_>, index: usize) -> Option<f64> {
    let top = (state.position_at_index)(index)?;
    let height = (state.size_at_index)(index)?;
    if !top.is_finite() || !height.is_finite() {
        return None;
    }

    Some(top + height.max(1.0))
}

/// Whether the timeline's real rows extend past the viewport left above the composer. The
/// list's own content length includes the composer inset spacer, so this measures from the
/// last row instead. Unknown row geometry or an unmeasured viewport counts as fitting.
pub fn timeline_content_overflows_viewport(
    state: Option<&TimelineListMeasurementState<'_>>,
    composer_inset: f64,
    anchor_offset: f64,
) -> bool {
    let Some(state) = state.filter(|state| state.data_len > 0) else {
        return false;
    };
    let scroll_length = state.scroll_length;
    if !scroll_length.is_finite() || scroll_length <= 0.0 {
        return false;
    }
    let Some(last_bottom) = get_row_bottom(state, state.data_len - 1) else {
        return false;
    };
    let visible_scroll_length = (scroll_length - composer_inset - anchor_offset).max(0.0);
    last_bottom > visible_scroll_length
}

pub fn get_anchored_turn_metrics(
    state: &TimelineListMeasurementState<'_>,
    anchor_index: usize,
    composer_overlay_height: f64,
    anchor_offset: f64,
) -> Option<AnchoredTurnMetrics> {
    if state.data_len == 0 {
        return None;
    }

    let bounded_anchor_index = anchor_index.min(state.data_len - 1);
    let anchor_top = (state.position_at_index)(bounded_anchor_index)?;
    let last_bottom = get_row_bottom(state, state.data_len - 1)?;
    if !anchor_top.is_finite() {
        return None;
    }

    let usable_viewport_height =
        (state.scroll_length - composer_overlay_height - anchor_offset).max(0.0);
    let turn_height = (last_bottom - anchor_top).max(0.0);
    let visible_usable_bottom = state.scroll + usable_viewport_height;
    let target_scroll_to_reveal_end = (last_bottom - usable_viewport_height).max(0.0);
    let scroll_delta_to_reveal_end = (target_scroll_to_reveal_end - state.scroll).max(0.0);

    Some(AnchoredTurnMetrics {
        anchor_top,
        last_bottom,
        turn_height,
        usable_viewport_height,
        visible_usable_bottom,
        overflows_usable_viewport: turn_height > usable_viewport_height,
        target_scroll_to_reveal_end,
        scroll_delta_to_reveal_end,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkGroupScrollPosition {
    pub entry_id: String,
    pub offset: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkGroupDisclosureState {
    pub scroll_positions: HashMap<String, WorkGroupScrollPosition>,
    pub expanded_entries: HashSet<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TimelineDisclosures {
    pub runs: HashSet<RunId>,
    pub work_groups: HashSet<String>,
    pub attempts: HashSet<RunAttemptId>,
    pub work_group_state: WorkGroupDisclosureState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RememberedTimelinePosition {
    pub row_id: String,
    pub offset_within_row: f64,
    pub scroll_offset: f64,
    pub at_end: bool,
    pub disclosures: Option<TimelineDisclosures>,
}

/// Upper bound on remembered threads. Upstream keeps the same bound per session.
const MAX_REMEMBERED_TIMELINE_POSITIONS: usize = 100;

/// Upstream keeps these in a module-level `Map`. The owner holds one of these instead, so the
/// memory is per view and needs no global lock. Scoped thread keys keep separate environments
/// independent. Insertion order is recency order, and the oldest entry is evicted first.
#[derive(Debug, Default)]
pub struct RememberedTimelinePositions {
    positions: IndexMap<String, RememberedTimelinePosition>,
}

impl RememberedTimelinePositions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn read(&self, thread_key: &str) -> Option<&RememberedTimelinePosition> {
        self.positions.get(thread_key)
    }

    pub fn remember(
        &mut self,
        thread_key: &str,
        position: RememberedTimelinePosition,
        live_follow_enabled: bool,
    ) {
        // Delete and re-insert so the entry moves to the most-recent end.
        self.positions.shift_remove(thread_key);
        let at_end = live_follow_enabled || position.at_end;
        self.positions.insert(
            thread_key.to_string(),
            RememberedTimelinePosition { at_end, ..position },
        );
        if self.positions.len() > MAX_REMEMBERED_TIMELINE_POSITIONS {
            self.positions.shift_remove_index(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str) -> RunId {
        RunId::new(id).expect("run id")
    }

    fn message(id: &str) -> MessageId {
        MessageId::new(id).expect("message id")
    }

    fn observation(
        thread_key: &str,
        hydrated: bool,
        run_id: Option<&str>,
    ) -> TimelineRunObservation {
        TimelineRunObservation {
            thread_key: Some(thread_key.to_string()),
            hydrated,
            run_id: run_id.map(run),
        }
    }

    fn input(
        thread_key: &str,
        hydrated: bool,
        run_id: Option<&str>,
        queued: bool,
        message_id: Option<&str>,
    ) -> TimelineRunInput {
        TimelineRunInput {
            thread_key: Some(thread_key.to_string()),
            hydrated,
            run_id: run_id.map(run),
            queued,
            message_id: message_id.map(message),
        }
    }

    #[test]
    fn opening_a_thread_establishes_a_baseline_without_anchoring() {
        let observed = observe_timeline_run(
            None,
            input("thread-a", true, Some("run-1"), false, Some("m1")),
        );
        assert_eq!(observed.anchor_message_id, None);
        assert_eq!(
            observed.observation,
            observation("thread-a", true, Some("run-1"))
        );
    }

    #[test]
    fn a_later_run_anchors_its_first_message() {
        let previous = observation("thread-a", true, Some("run-1"));
        let observed = observe_timeline_run(
            Some(&previous),
            input("thread-a", true, Some("run-2"), false, Some("m2")),
        );
        assert_eq!(observed.anchor_message_id, Some(message("m2")));
        assert_eq!(
            observed.observation,
            observation("thread-a", true, Some("run-2"))
        );
    }

    #[test]
    fn a_queued_or_unhydrated_run_keeps_the_previous_observation() {
        let previous = observation("thread-a", true, Some("run-1"));
        let queued = observe_timeline_run(
            Some(&previous),
            input("thread-a", true, Some("run-2"), true, Some("m2")),
        );
        assert_eq!(queued.anchor_message_id, None);
        assert_eq!(queued.observation, previous);

        let unhydrated = observe_timeline_run(
            Some(&previous),
            input("thread-a", false, Some("run-2"), false, Some("m2")),
        );
        assert_eq!(unhydrated.anchor_message_id, None);
        assert_eq!(unhydrated.observation, previous);
    }

    #[test]
    fn a_run_without_a_message_or_a_thread_change_does_not_anchor() {
        let previous = observation("thread-a", true, Some("run-1"));
        let no_message = observe_timeline_run(
            Some(&previous),
            input("thread-a", true, Some("run-2"), false, None),
        );
        assert_eq!(no_message.anchor_message_id, None);
        assert_eq!(no_message.observation, previous);

        let other_thread = observe_timeline_run(
            Some(&previous),
            input("thread-b", true, Some("run-2"), false, Some("m2")),
        );
        assert_eq!(other_thread.anchor_message_id, None);
        assert_eq!(
            other_thread.observation,
            observation("thread-b", true, Some("run-2"))
        );
    }

    fn list_state<'a>(
        data_len: usize,
        scroll: f64,
        scroll_length: f64,
        position: &'a dyn Fn(usize) -> Option<f64>,
        size: &'a dyn Fn(usize) -> Option<f64>,
    ) -> TimelineListMeasurementState<'a> {
        TimelineListMeasurementState {
            data_len,
            scroll,
            scroll_length,
            position_at_index: position,
            size_at_index: size,
        }
    }

    #[test]
    fn row_bottom_floors_height_at_one_pixel_and_rejects_unmeasured_rows() {
        let position = |index: usize| (index < 2).then_some(index as f64 * 100.0);
        let size = |index: usize| match index {
            0 => Some(0.0),
            1 => Some(40.0),
            _ => None,
        };
        let state = list_state(3, 0.0, 500.0, &position, &size);
        assert_eq!(get_row_bottom(&state, 0), Some(1.0));
        assert_eq!(get_row_bottom(&state, 1), Some(140.0));
        assert_eq!(get_row_bottom(&state, 2), None);
    }

    #[test]
    fn overflow_is_measured_from_the_last_row_and_unknown_geometry_fits() {
        let position = |index: usize| Some(index as f64 * 100.0);
        let size = |_: usize| Some(100.0);
        let state = list_state(3, 0.0, 400.0, &position, &size);
        // last bottom 300, visible length 400 - 40 composer inset - 24 anchor offset = 336
        assert!(!timeline_content_overflows_viewport(
            Some(&state),
            40.0,
            CHAT_TIMELINE_ANCHOR_OFFSET
        ));
        // a shorter viewport no longer holds the last row
        let short = list_state(3, 0.0, 300.0, &position, &size);
        assert!(timeline_content_overflows_viewport(
            Some(&short),
            40.0,
            CHAT_TIMELINE_ANCHOR_OFFSET
        ));
        assert!(!timeline_content_overflows_viewport(
            None,
            40.0,
            CHAT_TIMELINE_ANCHOR_OFFSET
        ));
        let unmeasured = list_state(3, 0.0, 0.0, &position, &size);
        assert!(!timeline_content_overflows_viewport(
            Some(&unmeasured),
            40.0,
            CHAT_TIMELINE_ANCHOR_OFFSET
        ));
    }

    #[test]
    fn anchored_turn_metrics_report_the_scroll_needed_to_reveal_the_end() {
        let position = |index: usize| Some(index as f64 * 100.0);
        let size = |_: usize| Some(100.0);
        // rows 0..3, last bottom 300; usable viewport 400 - 50 overlay - 24 offset = 326
        let state = list_state(3, 0.0, 400.0, &position, &size);
        let metrics = get_anchored_turn_metrics(&state, 1, 50.0, CHAT_TIMELINE_ANCHOR_OFFSET)
            .expect("metrics");
        assert_eq!(metrics.anchor_top, 100.0);
        assert_eq!(metrics.last_bottom, 300.0);
        assert_eq!(metrics.turn_height, 200.0);
        assert_eq!(metrics.usable_viewport_height, 326.0);
        assert!(!metrics.overflows_usable_viewport);
        assert_eq!(metrics.target_scroll_to_reveal_end, 0.0);
        assert_eq!(metrics.scroll_delta_to_reveal_end, 0.0);

        let tall = list_state(3, 10.0, 200.0, &position, &size);
        let metrics = get_anchored_turn_metrics(&tall, 0, 50.0, CHAT_TIMELINE_ANCHOR_OFFSET)
            .expect("metrics");
        assert_eq!(metrics.usable_viewport_height, 126.0);
        assert!(metrics.overflows_usable_viewport);
        assert_eq!(metrics.target_scroll_to_reveal_end, 174.0);
        assert_eq!(metrics.scroll_delta_to_reveal_end, 164.0);
        assert_eq!(metrics.visible_usable_bottom, 136.0);
    }

    #[test]
    fn anchored_turn_metrics_clamp_the_anchor_index_and_reject_empty_timelines() {
        let position = |index: usize| Some(index as f64 * 100.0);
        let size = |_: usize| Some(100.0);
        let state = list_state(2, 0.0, 400.0, &position, &size);
        let metrics = get_anchored_turn_metrics(&state, 99, 0.0, 0.0).expect("metrics");
        assert_eq!(metrics.anchor_top, 100.0);

        let empty = list_state(0, 0.0, 400.0, &position, &size);
        assert_eq!(get_anchored_turn_metrics(&empty, 0, 0.0, 0.0), None);
    }

    #[test]
    fn remembered_positions_bump_recency_and_evict_the_oldest_past_the_bound() {
        let position = |row: &str| RememberedTimelinePosition {
            row_id: row.to_string(),
            offset_within_row: 0.0,
            scroll_offset: 0.0,
            at_end: false,
            disclosures: None,
        };
        let mut remembered = RememberedTimelinePositions::new();
        for index in 0..MAX_REMEMBERED_TIMELINE_POSITIONS {
            remembered.remember(&format!("thread-{index}"), position("row"), false);
        }
        // Touch the oldest entry so the next insert evicts the second oldest.
        remembered.remember("thread-0", position("row-0"), false);
        remembered.remember("thread-new", position("row-new"), false);

        assert!(remembered.read("thread-0").is_some());
        assert!(remembered.read("thread-1").is_none());
        assert_eq!(
            remembered.read("thread-new").map(|p| p.row_id.as_str()),
            Some("row-new")
        );
    }

    #[test]
    fn remembering_while_following_the_live_end_marks_the_position_at_end() {
        let mut remembered = RememberedTimelinePositions::new();
        remembered.remember(
            "thread-a",
            RememberedTimelinePosition {
                row_id: "row".to_string(),
                offset_within_row: 4.0,
                scroll_offset: 120.0,
                at_end: false,
                disclosures: Some(TimelineDisclosures::default()),
            },
            true,
        );
        let stored = remembered.read("thread-a").expect("stored");
        assert!(stored.at_end);
        assert_eq!(stored.offset_within_row, 4.0);
        assert_eq!(stored.disclosures, Some(TimelineDisclosures::default()));
    }
}
