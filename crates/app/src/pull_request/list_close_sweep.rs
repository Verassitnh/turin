//! Port of the close sweep in upstream/apps/web/src/routes/_chat.pull-requests.tsx
//! (`startCloseSweep`): pressing Close in speed mode and dragging down (or up) the list closes
//! every closable row between the pressed one and the pointer.
//!
//! `SidebarPointerSensor` is the gesture (a 6px threshold, release, Escape); the page forwards
//! window-level pointer events to a [`CloseSweep`] and hands it the row under the pointer.

use std::collections::HashSet;

use crate::components::sidebar::sidebar_gestures::SIDEBAR_DRAG_DISTANCE;
use crate::components::sidebar::sidebar_logic::resolve_sidebar_sweep_keys;
use crate::components::sidebar::sidebar_pointer::{
    Point, PointerSensorHandler, SidebarPointerSensor,
};

#[derive(Default)]
struct Events {
    start: bool,
    moves: Vec<Point>,
    end: bool,
    cancel: bool,
    finished: bool,
}

impl PointerSensorHandler for Events {
    fn on_start(&mut self, _: Point) {
        self.start = true;
    }
    fn on_move(&mut self, point: Point) {
        self.moves.push(point);
    }
    fn on_end(&mut self) {
        self.end = true;
    }
    fn on_cancel(&mut self) {
        self.cancel = true;
    }
    fn on_finish(&mut self, _started: bool) {
        self.finished = true;
    }
}

/// What a forwarded event changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepProgress {
    /// The swept set changed (redraw the rows).
    pub changed: bool,
    /// The gesture ended on a release: these rows are to be closed.
    pub close: Option<Vec<String>>,
    /// The gesture is over, released or cancelled.
    pub done: bool,
}

pub struct CloseSweep {
    origin_key: String,
    sensor: SidebarPointerSensor,
    /// Every row in list order, across groups (`orderedKeys`).
    ordered_keys: Vec<String>,
    /// The rows the sweep may take by what they are: same group as the pressed one, open and
    /// closable by the host. Whether one is answering an action is asked live (`canClose`).
    closable: HashSet<String>,
    target: Option<String>,
    keys: Vec<String>,
}

impl CloseSweep {
    pub fn new(
        origin_key: String,
        origin: Point,
        ordered_keys: Vec<String>,
        closable: HashSet<String>,
    ) -> Self {
        let mut events = Events::default();
        let sensor = SidebarPointerSensor::new(origin, 0, SIDEBAR_DRAG_DISTANCE, &mut events);
        Self {
            origin_key,
            sensor,
            ordered_keys,
            closable,
            target: None,
            keys: Vec::new(),
        }
    }

    /// `sweeping`: the rows the release would close.
    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    pub fn contains(&self, key: &str) -> bool {
        self.keys.iter().any(|swept| swept == key)
    }

    pub fn is_finished(&self) -> bool {
        self.sensor.is_finished()
    }

    /// The click that ends a drag from the button must not also close the one row.
    pub fn consume_click(&mut self) -> bool {
        self.sensor.consume_click()
    }

    fn sweep_to(&mut self, key: &str, can_close: &impl Fn(&str) -> bool) -> bool {
        if self.target.as_deref() == Some(key) {
            return false;
        }
        self.target = Some(key.to_owned());
        let closable = &self.closable;
        let next = resolve_sidebar_sweep_keys(&self.ordered_keys, &self.origin_key, key, |key| {
            closable.contains(key) && can_close(key)
        });
        let changed = next != self.keys;
        self.keys = next;
        changed
    }

    fn apply(
        &mut self,
        events: Events,
        row_at: &impl Fn(f32) -> Option<String>,
        can_close: &impl Fn(&str) -> bool,
    ) -> SweepProgress {
        let mut progress = SweepProgress::default();
        if events.start {
            let origin = self.origin_key.clone();
            progress.changed |= self.sweep_to(&origin, can_close);
        }
        for point in events.moves {
            if let Some(key) = row_at(point.y) {
                progress.changed |= self.sweep_to(&key, can_close);
            }
        }
        if events.end {
            let batch: Vec<String> = self
                .keys
                .iter()
                .filter(|key| self.closable.contains(key.as_str()) && can_close(key))
                .cloned()
                .collect();
            progress.close = Some(batch);
        }
        if events.finished {
            progress.done = true;
            progress.changed |= !self.keys.is_empty();
            self.keys.clear();
        }
        progress
    }

    pub fn pointer_moved(
        &mut self,
        position: Point,
        primary_down: bool,
        row_at: impl Fn(f32) -> Option<String>,
        can_close: impl Fn(&str) -> bool,
    ) -> SweepProgress {
        let mut events = Events::default();
        self.sensor
            .pointer_move(0, position, primary_down, &mut events);
        self.apply(events, &row_at, &can_close)
    }

    pub fn pointer_released(
        &mut self,
        position: Point,
        row_at: impl Fn(f32) -> Option<String>,
        can_close: impl Fn(&str) -> bool,
    ) -> SweepProgress {
        let mut events = Events::default();
        self.sensor.pointer_up(0, position, &mut events);
        self.apply(events, &row_at, &can_close)
    }

    /// Escape, or the page going away.
    pub fn cancel(&mut self) -> SweepProgress {
        let mut events = Events::default();
        self.sensor.cancel(&mut events);
        self.apply(events, &|_| None, &|_| true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn rows(y: f32) -> Option<String> {
        // Rows 40px tall starting at 0: a, b, c, d.
        Some(["a", "b", "c", "d"][((y / 40.) as usize).min(3)].to_owned())
    }

    fn sweep(closable: &[&str]) -> CloseSweep {
        CloseSweep::new(
            "a".to_owned(),
            Point::new(10., 10.),
            keys(&["a", "b", "c", "d"]),
            closable.iter().map(|key| (*key).to_owned()).collect(),
        )
    }

    #[test]
    fn a_short_press_stays_a_click() {
        let mut sweep = sweep(&["a", "b"]);
        let moved = sweep.pointer_moved(Point::new(12., 13.), true, rows, |_| true);
        assert_eq!(moved, SweepProgress::default());
        assert!(sweep.keys().is_empty());
        let released = sweep.pointer_released(Point::new(12., 13.), rows, |_| true);
        assert!(released.done);
        assert_eq!(released.close.as_deref(), Some(&[][..]));
        // The sensor never started, so the click goes through to close the one row.
        assert!(!sweep.consume_click());
    }

    #[test]
    fn dragging_takes_the_closable_rows_down_to_the_pointer() {
        let mut sweep = sweep(&["a", "b", "d"]);
        let moved = sweep.pointer_moved(Point::new(10., 100.), true, rows, |_| true);
        assert!(moved.changed);
        // `c` is not closable and is skipped; `d` is below the pointer.
        assert_eq!(sweep.keys(), keys(&["a", "b"]).as_slice());
        let farther = sweep.pointer_moved(Point::new(10., 150.), true, rows, |_| true);
        assert!(farther.changed);
        assert_eq!(sweep.keys(), keys(&["a", "b", "d"]).as_slice());
        let released = sweep.pointer_released(Point::new(10., 150.), rows, |_| true);
        assert_eq!(released.close, Some(keys(&["a", "b", "d"])));
        assert!(released.done);
        assert!(sweep.keys().is_empty());
        // The release click that follows the drag is swallowed.
        assert!(sweep.consume_click());
    }

    #[test]
    fn a_row_that_starts_an_action_mid_drag_is_left_out_live() {
        let mut sweep = sweep(&["a", "b", "c", "d"]);
        sweep.pointer_moved(Point::new(10., 100.), true, rows, |_| true);
        assert_eq!(sweep.keys(), keys(&["a", "b", "c"]).as_slice());
        // `b` began closing since the press: the next move drops it, and so does the release.
        sweep.pointer_moved(Point::new(10., 150.), true, rows, |key| key != "b");
        assert_eq!(sweep.keys(), keys(&["a", "c", "d"]).as_slice());
        let released =
            sweep.pointer_released(Point::new(10., 150.), rows, |key| key != "b" && key != "d");
        assert_eq!(released.close, Some(keys(&["a", "c"])));
    }

    #[test]
    fn dragging_back_up_shrinks_the_sweep() {
        let mut sweep = sweep(&["a", "b", "c", "d"]);
        sweep.pointer_moved(Point::new(10., 150.), true, rows, |_| true);
        assert_eq!(sweep.keys().len(), 4);
        sweep.pointer_moved(Point::new(10., 50.), true, rows, |_| true);
        assert_eq!(sweep.keys(), keys(&["a", "b"]).as_slice());
    }

    #[test]
    fn escape_cancels_without_closing() {
        let mut sweep = sweep(&["a", "b"]);
        sweep.pointer_moved(Point::new(10., 60.), true, rows, |_| true);
        assert_eq!(sweep.keys().len(), 2);
        let cancelled = sweep.cancel();
        assert!(cancelled.done);
        assert_eq!(cancelled.close, None);
        assert!(sweep.keys().is_empty());
        assert!(sweep.is_finished());
    }

    #[test]
    fn letting_go_of_the_button_off_window_cancels() {
        let mut sweep = sweep(&["a", "b"]);
        sweep.pointer_moved(Point::new(10., 60.), true, rows, |_| true);
        let lost = sweep.pointer_moved(Point::new(10., 70.), false, rows, |_| true);
        assert!(lost.done);
        assert_eq!(lost.close, None);
    }
}
