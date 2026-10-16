//! Port of upstream/apps/web/src/hooks/useLiveRefresh.ts (+ test), the
//! decision half.
//!
//! Keeps what a reader is looking at true: a read on arrival, on returning to
//! the window, and while the view stays open and the reader stays present. The
//! DOM listeners and timers are the UI's seam; [`LiveRefreshCadence`] holds the
//! per-view read times and the shared last-interaction time, with the clock
//! passed in as epoch milliseconds (upstream reads `Date.now()`).
// Seams for consumers the shell wires (T22); nothing calls these yet.
#![allow(dead_code)]

use std::collections::HashMap;

/// Long enough that alt-tabbing through windows does not become a request per tab stop.
pub const LIVE_REFRESH_MIN_INTERVAL_MS: i64 = 10_000;

/// Slow enough to preserve host quota while still updating a view left open.
pub const LIVE_REFRESH_INTERVAL_MS: i64 = 5 * 60_000;

/// How long a showing window goes untouched before it stops reading. One
/// minute past the five-minute interval, so the first timer tick still runs.
pub const LIVE_REFRESH_IDLE_AFTER_MS: i64 = 6 * 60_000;

/// Whether a view should read again now: only while the window is showing, and
/// not again straight away.
pub fn should_live_refresh(visible: bool, now: i64, last_refreshed_at: i64) -> bool {
    visible && now - last_refreshed_at >= LIVE_REFRESH_MIN_INTERVAL_MS
}

/// Whether arriving at a view should read it. A view nobody has read this
/// session is left alone: its own first read is already on its way.
pub fn should_refresh_on_arrival(visible: bool, now: i64, last_refreshed_at: Option<i64>) -> bool {
    last_refreshed_at
        .is_some_and(|last_refreshed_at| should_live_refresh(visible, now, last_refreshed_at))
}

/// Whether the interval should read again: the same rule as any other read,
/// plus the reader having been here recently enough for the answer to matter.
pub fn should_refresh_on_interval(
    visible: bool,
    now: i64,
    last_refreshed_at: i64,
    last_interacted_at: i64,
) -> bool {
    now - last_interacted_at < LIVE_REFRESH_IDLE_AFTER_MS
        && should_live_refresh(visible, now, last_refreshed_at)
}

/// Read times per view (the hook's `lastRefreshedAtByView`) and the shared
/// last interaction. Entries are one number each and are never pruned.
#[derive(Debug, Default)]
pub struct LiveRefreshCadence {
    last_refreshed_at_by_view: HashMap<String, i64>,
    last_interacted_at: i64,
}

impl LiveRefreshCadence {
    pub fn new() -> Self {
        Self::default()
    }

    /// `noteInteraction`. Returns true when the reader was idle and the
    /// watchers should resume (the listeners re-run their arrival and timer
    /// sync).
    pub fn note_interaction(&mut self, now: i64) -> bool {
        let was_idle = now - self.last_interacted_at >= LIVE_REFRESH_IDLE_AFTER_MS;
        self.last_interacted_at = now;
        was_idle
    }

    /// `watchInteraction` on the first watcher: arriving is itself the reader
    /// doing something, so the first interval tick after a mount counts.
    pub fn start_watching_interaction(&mut self, now: i64) {
        self.last_interacted_at = now;
    }

    pub fn last_interacted_at(&self) -> i64 {
        self.last_interacted_at
    }

    pub fn last_refreshed_at(&self, view: &str) -> Option<i64> {
        self.last_refreshed_at_by_view.get(view).copied()
    }

    /// `onArrival`. Returns true when the caller should read now; the read
    /// time is recorded here, as the hook's `read` does before it calls the
    /// refresh.
    pub fn on_arrival(&mut self, view: &str, now: i64, visible: bool) -> bool {
        if now - self.last_interacted_at >= LIVE_REFRESH_IDLE_AFTER_MS {
            return false;
        }
        let Some(last_refreshed_at) = self.last_refreshed_at(view) else {
            // Nothing read yet: the mount's own read fills this in.
            self.last_refreshed_at_by_view.insert(view.to_owned(), now);
            return false;
        };
        if should_refresh_on_arrival(visible, now, Some(last_refreshed_at)) {
            self.last_refreshed_at_by_view.insert(view.to_owned(), now);
            return true;
        }
        false
    }

    /// `onInterval`. A view with no record counts as read just now, so the
    /// tick does nothing for it. Returns true when the caller should read.
    pub fn on_interval(&mut self, view: &str, now: i64, visible: bool) -> bool {
        let last_refreshed_at = self.last_refreshed_at(view).unwrap_or(now);
        if should_refresh_on_interval(visible, now, last_refreshed_at, self.last_interacted_at) {
            self.last_refreshed_at_by_view.insert(view.to_owned(), now);
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = LIVE_REFRESH_MIN_INTERVAL_MS;

    fn at(now: i64, last_refreshed_at: i64, visible: bool) -> bool {
        should_live_refresh(visible, now, last_refreshed_at)
    }

    #[test]
    fn cadence_constants_match_upstream() {
        assert_eq!(LIVE_REFRESH_INTERVAL_MS, 5 * 60_000);
        assert_eq!(LIVE_REFRESH_MIN_INTERVAL_MS, 10_000);
        assert_eq!(LIVE_REFRESH_IDLE_AFTER_MS, 6 * 60_000);
    }

    #[test]
    fn reads_a_view_again_when_it_is_navigated_to() {
        assert!(at(MIN, 0, true));
    }

    #[test]
    fn does_not_read_a_view_again_that_was_left_and_returned_to_seconds_later() {
        assert!(!at(3_000, 0, true));
    }

    #[test]
    fn reads_again_when_the_interval_comes_round_on_a_view_left_open() {
        assert!(at(LIVE_REFRESH_INTERVAL_MS, 0, true));
    }

    #[test]
    fn does_not_read_again_for_every_window_tabbed_through() {
        assert!(!at(1_000, 0, true));
    }

    #[test]
    fn stays_quiet_while_the_window_is_not_showing() {
        // A focus event can arrive for a window that is still hidden behind another one.
        assert!(!at(MIN * 5, 0, false));
    }

    #[test]
    fn reads_once_for_a_window_hidden_an_hour_not_once_per_interval_it_missed() {
        let hour = 60 * 60_000;
        let mut last_refreshed_at = 0;
        let mut reads = 0;
        let mut tick = |now: i64, visible: bool| {
            if !at(now, last_refreshed_at, visible) {
                return;
            }
            last_refreshed_at = now;
            reads += 1;
        };

        let mut now = LIVE_REFRESH_INTERVAL_MS;
        while now < hour {
            tick(now, false);
            now += LIVE_REFRESH_INTERVAL_MS;
        }
        // Coming back raises a visibility change and a focus event, one straight after the other.
        tick(hour, true);
        tick(hour, true);

        assert_eq!(reads, 1);
    }

    #[test]
    fn arrival_leaves_a_view_alone_the_first_time_it_is_opened() {
        assert!(!should_refresh_on_arrival(true, 5_000, None));
    }

    #[test]
    fn arrival_reads_a_view_that_was_read_earlier_in_the_session_and_returned_to() {
        assert!(should_refresh_on_arrival(true, 90_000, Some(0)));
    }

    #[test]
    fn arrival_keeps_the_minimum_interval_on_a_view_returned_to_straight_away() {
        assert!(!should_refresh_on_arrival(true, 2_000, Some(0)));
    }

    fn tick(now: i64, last_interacted_at: i64) -> bool {
        should_refresh_on_interval(true, now, 0, last_interacted_at)
    }

    #[test]
    fn interval_reads_for_a_reader_who_is_here() {
        assert!(tick(
            LIVE_REFRESH_INTERVAL_MS,
            LIVE_REFRESH_INTERVAL_MS - 1_000
        ));
    }

    #[test]
    fn interval_reads_on_the_first_interval_after_an_untouched_mount() {
        assert!(tick(LIVE_REFRESH_INTERVAL_MS + 1_000, 0));
    }

    #[test]
    fn interval_stops_reading_for_a_window_left_showing_on_a_desk_nobody_is_at() {
        assert!(!tick(LIVE_REFRESH_IDLE_AFTER_MS + 60_000, 0));
    }

    #[test]
    fn interval_starts_reading_again_once_the_reader_touches_the_window() {
        let away = LIVE_REFRESH_IDLE_AFTER_MS + 60_000;
        assert!(tick(away + MIN, away));
    }

    #[test]
    fn mount_records_the_arrival_without_a_read_and_the_interval_then_reads() {
        let mut cadence = LiveRefreshCadence::new();
        cadence.start_watching_interaction(1_000);
        // First arrival on a view: recorded, not read.
        assert!(!cadence.on_arrival("pr-1", 1_000, true));
        assert_eq!(cadence.last_refreshed_at("pr-1"), Some(1_000));
        // Interval tick after five minutes for a reader who has been here.
        assert!(cadence.on_interval("pr-1", 1_000 + LIVE_REFRESH_INTERVAL_MS, true));
        assert_eq!(
            cadence.last_refreshed_at("pr-1"),
            Some(1_000 + LIVE_REFRESH_INTERVAL_MS)
        );
    }

    #[test]
    fn interval_does_nothing_for_a_view_with_no_record() {
        let mut cadence = LiveRefreshCadence::new();
        cadence.start_watching_interaction(0);
        assert!(!cadence.on_interval("unseen", LIVE_REFRESH_INTERVAL_MS, true));
        assert_eq!(cadence.last_refreshed_at("unseen"), None);
    }

    #[test]
    fn returning_to_a_read_view_refreshes_once_and_the_key_follows_the_view() {
        let mut cadence = LiveRefreshCadence::new();
        cadence.start_watching_interaction(0);
        assert!(!cadence.on_arrival("pr-1", 0, true));
        assert!(cadence.on_arrival("pr-1", MIN, true));
        // A second arrival straight after is inside the minimum interval.
        assert!(!cadence.on_arrival("pr-1", MIN + 1, true));
        // A different view has its own record.
        assert!(!cadence.on_arrival("pr-2", MIN + 2, true));
        assert_eq!(cadence.last_refreshed_at("pr-2"), Some(MIN + 2));
    }

    #[test]
    fn idle_reader_gets_no_arrival_read_until_input_resumes_the_watchers() {
        let mut cadence = LiveRefreshCadence::new();
        cadence.start_watching_interaction(0);
        assert!(!cadence.on_arrival("pr-1", 0, true));

        let away = LIVE_REFRESH_IDLE_AFTER_MS;
        assert!(!cadence.on_arrival("pr-1", away, true));
        assert!(!cadence.on_interval("pr-1", away + 90_000, true));

        // Input after the idle window reports that the watchers should resume.
        assert!(cadence.note_interaction(away + 90_000));
        assert!(cadence.on_arrival("pr-1", away + 90_000 + MIN, true));
        assert!(!cadence.note_interaction(away + 90_000 + MIN + 1));
    }
}
