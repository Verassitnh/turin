//! Port of upstream/apps/web/src/components/Sidebar.pointer.ts
//!
//! `SidebarPointerSensor`: a sidebar gesture (row drag or action sweep)
//! that starts after a distance threshold and ends on release,
//! cancellation, or loss of its window. Upstream owns DOM listeners; here
//! the sidebar view forwards the same events (window-level mouse moves and
//! ups, Escape, blur/resize) and the sensor reports through a
//! [`PointerSensorHandler`]. `SidebarDragLifecycle` (cancel on unmount)
//! becomes the view dropping/cancelling its sensor when the list unmounts.

/// A pointer position in window coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// The sensor's dnd-kit callbacks plus the sidebar options.
pub trait PointerSensorHandler {
    fn on_pending(&mut self, _origin: Point, _offset: Option<Point>) {}
    fn on_start(&mut self, _origin: Point) {}
    fn on_move(&mut self, _point: Point) {}
    fn on_end(&mut self) {}
    fn on_cancel(&mut self) {}
    fn on_abort(&mut self) {}
    /// Runs before end/cancel/abort; `started` is false for a click.
    fn on_finish(&mut self, _started: bool) {}
    /// Return true to claim the move: the sort ignores this position.
    fn claim_move(&mut self, _point: Point) -> bool {
        false
    }
    /// Return true when the release was consumed elsewhere (sort cancelled).
    fn claim_drop(&mut self, _point: Point) -> bool {
        false
    }
    /// Checked right after `on_start`: upstream handlers may cancel the
    /// sensor from inside activation.
    fn cancel_requested(&mut self) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Pending,
    Dragging,
    Finished,
}

/// `SidebarPointerSensor`.
#[derive(Debug)]
pub struct SidebarPointerSensor {
    phase: Phase,
    origin: Point,
    pointer_id: u64,
    distance: f32,
    /// A click listener that swallows the next click (cancelled release).
    suppress_click: bool,
}

impl SidebarPointerSensor {
    /// Attach on a primary-button press at `origin`.
    pub fn new(
        origin: Point,
        pointer_id: u64,
        distance: f32,
        handler: &mut impl PointerSensorHandler,
    ) -> Self {
        handler.on_pending(origin, None);
        Self {
            phase: Phase::Pending,
            origin,
            pointer_id,
            distance,
            suppress_click: false,
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.phase == Phase::Dragging
    }

    pub fn is_finished(&self) -> bool {
        self.phase == Phase::Finished
    }

    /// `pointermove`. `primary_down` is `buttons & 1`.
    pub fn pointer_move(
        &mut self,
        pointer_id: u64,
        point: Point,
        primary_down: bool,
        handler: &mut impl PointerSensorHandler,
    ) {
        if self.phase == Phase::Finished || pointer_id != self.pointer_id {
            return;
        }
        // Desktop can report buttons=0 on the final move before pointerup.
        // Let an active context drag drop there before cancelling the sort.
        if !primary_down {
            if self.phase == Phase::Dragging {
                handler.claim_drop(point);
            }
            self.cancel(handler);
            return;
        }
        if self.phase == Phase::Pending {
            let offset = Point::new(point.x - self.origin.x, point.y - self.origin.y);
            if offset.x.hypot(offset.y) <= self.distance {
                handler.on_pending(self.origin, Some(offset));
                return;
            }
            self.phase = Phase::Dragging;
            self.suppress_click = true;
            handler.on_start(self.origin);
            if handler.cancel_requested() {
                self.cancel(handler);
            }
        }
        // The move that starts a drag also moves it, so a release before
        // the next pointermove still lands where the pointer is.
        if self.phase == Phase::Dragging {
            if handler.claim_move(point) {
                return;
            }
            handler.on_move(point);
        }
    }

    /// `pointerup`.
    pub fn pointer_up(
        &mut self,
        pointer_id: u64,
        point: Point,
        handler: &mut impl PointerSensorHandler,
    ) {
        if pointer_id != self.pointer_id {
            return;
        }
        let dropped = self.phase == Phase::Dragging && handler.claim_drop(point);
        self.finish(dropped, handler);
    }

    /// `pointercancel`.
    pub fn pointer_cancel(&mut self, pointer_id: u64, handler: &mut impl PointerSensorHandler) {
        if pointer_id == self.pointer_id {
            self.cancel(handler);
        }
    }

    /// Escape, a hidden document, blur, pagehide, resize, unmount.
    pub fn cancel(&mut self, handler: &mut impl PointerSensorHandler) {
        self.finish(true, handler);
    }

    /// A fresh `pointerdown` ends click suppression left by a release
    /// outside the window.
    pub fn pointer_down(&mut self) {
        if self.phase == Phase::Finished {
            self.suppress_click = false;
        }
    }

    /// A `click` reached the document: returns true when the sensor
    /// swallows it (the release of a drag or a cancelled gesture).
    pub fn consume_click(&mut self) -> bool {
        std::mem::take(&mut self.suppress_click)
    }

    fn finish(&mut self, cancelled: bool, handler: &mut impl PointerSensorHandler) {
        if self.phase == Phase::Finished {
            return;
        }
        let aborted = self.phase == Phase::Pending;
        self.phase = Phase::Finished;
        // Cancellation can precede release by an arbitrary amount of time.
        // Consume that release click, or let a fresh pointerdown end
        // suppression. Ordinary clicks never install this guard.
        self.suppress_click = !aborted || cancelled;
        handler.on_finish(!aborted);
        if aborted {
            handler.on_abort();
        }
        if cancelled {
            handler.on_cancel();
        } else {
            handler.on_end();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        start: Vec<Point>,
        moves: Vec<Point>,
        end: usize,
        cancel: usize,
        abort: usize,
        finish: Vec<bool>,
        drops: Vec<Point>,
        claim_beyond: Option<f32>,
        cancel_on_start: bool,
    }

    impl PointerSensorHandler for Recorder {
        fn on_start(&mut self, origin: Point) {
            self.start.push(origin);
        }
        fn on_move(&mut self, point: Point) {
            self.moves.push(point);
        }
        fn on_end(&mut self) {
            self.end += 1;
        }
        fn on_cancel(&mut self) {
            self.cancel += 1;
        }
        fn on_abort(&mut self) {
            self.abort += 1;
        }
        fn on_finish(&mut self, started: bool) {
            self.finish.push(started);
        }
        fn claim_move(&mut self, point: Point) -> bool {
            self.claim_beyond.is_some_and(|x| point.x > x)
        }
        fn cancel_requested(&mut self) -> bool {
            self.cancel_on_start
        }
        fn claim_drop(&mut self, point: Point) -> bool {
            if self.claim_beyond.is_some() {
                self.drops.push(point);
            }
            self.claim_beyond.is_some_and(|x| point.x > x)
        }
    }

    fn gesture(recorder: &mut Recorder) -> SidebarPointerSensor {
        SidebarPointerSensor::new(Point::new(10., 10.), 1, 6., recorder)
    }

    fn mv(sensor: &mut SidebarPointerSensor, r: &mut Recorder, x: f32, y: f32) {
        sensor.pointer_move(1, Point::new(x, y), true, r);
    }

    fn up(sensor: &mut SidebarPointerSensor, r: &mut Recorder, x: f32, y: f32) {
        sensor.pointer_up(1, Point::new(x, y), r);
    }

    #[test]
    fn keeps_a_click_idle_and_starts_only_after_the_drag_threshold() {
        let mut click = Recorder::default();
        let mut sensor = gesture(&mut click);
        mv(&mut sensor, &mut click, 10., 16.);
        assert!(click.start.is_empty());
        up(&mut sensor, &mut click, 10., 16.);
        assert_eq!(click.abort, 1);
        assert_eq!(click.finish.len(), 1);

        let mut drag = Recorder::default();
        let mut sensor = gesture(&mut drag);
        mv(&mut sensor, &mut drag, 10., 17.);
        assert_eq!(drag.start, vec![Point::new(10., 10.)]);
        up(&mut sensor, &mut drag, 10., 17.);
        assert_eq!(drag.end, 1);
        assert_eq!(drag.abort, 0);
        assert_eq!(drag.finish.len(), 1);
    }

    #[test]
    fn moves_to_the_pointer_on_the_move_that_starts_the_drag() {
        let mut drag = Recorder::default();
        let mut sensor = gesture(&mut drag);
        mv(&mut sensor, &mut drag, 10., 90.);
        assert_eq!(drag.start, vec![Point::new(10., 10.)]);
        assert_eq!(drag.moves, vec![Point::new(10., 90.)]);
    }

    #[test]
    fn cancels_on_interruptions_and_ignores_late_events() {
        type Interrupt = fn(&mut SidebarPointerSensor, &mut Recorder);
        let interruptions: [Interrupt; 3] = [
            |s, r| s.cancel(r),
            |s, r| s.pointer_cancel(1, r),
            // missed release
            |s, r| s.pointer_move(1, Point::new(10., 100.), false, r),
        ];
        for interrupt in interruptions {
            for started in [false, true] {
                let mut drag = Recorder::default();
                let mut sensor = gesture(&mut drag);
                if started {
                    mv(&mut sensor, &mut drag, 10., 20.);
                }
                interrupt(&mut sensor, &mut drag);
                mv(&mut sensor, &mut drag, 10., 100.);
                up(&mut sensor, &mut drag, 10., 100.);
                sensor.cancel(&mut drag);
                assert_eq!(drag.cancel, 1);
                assert_eq!(drag.end, 0);
                assert_eq!(drag.finish.len(), 1);
                assert_eq!(drag.start.len(), usize::from(started));
                assert_eq!(drag.abort, usize::from(!started));

                let mut next = Recorder::default();
                let mut sensor = gesture(&mut next);
                mv(&mut sensor, &mut next, 10., 20.);
                up(&mut sensor, &mut next, 10., 20.);
                assert_eq!(next.start.len(), 1);
                assert_eq!(next.end, 1);
            }
        }
    }

    #[test]
    fn hands_claimed_moves_and_drops_to_the_context_handlers_and_cancels_the_sort() {
        let mut drag = Recorder {
            claim_beyond: Some(50.),
            ..Default::default()
        };
        let mut sensor = gesture(&mut drag);
        mv(&mut sensor, &mut drag, 10., 20.);
        mv(&mut sensor, &mut drag, 30., 30.);
        assert_eq!(drag.moves, vec![Point::new(10., 20.), Point::new(30., 30.)]);
        mv(&mut sensor, &mut drag, 90., 30.);
        assert_eq!(drag.moves.len(), 2);
        up(&mut sensor, &mut drag, 90., 30.);
        assert_eq!(drag.drops, vec![Point::new(90., 30.)]);
        assert_eq!(drag.cancel, 1);
        assert_eq!(drag.end, 0);
        assert_eq!(drag.finish, vec![true]);

        let mut inside = Recorder {
            claim_beyond: Some(50.),
            ..Default::default()
        };
        let mut sensor = gesture(&mut inside);
        mv(&mut sensor, &mut inside, 10., 20.);
        up(&mut sensor, &mut inside, 10., 20.);
        assert_eq!(inside.end, 1);
        assert_eq!(inside.cancel, 0);
    }

    #[test]
    fn drops_context_when_a_zero_button_move_precedes_pointerup() {
        let mut drag = Recorder {
            claim_beyond: Some(50.),
            ..Default::default()
        };
        let mut sensor = gesture(&mut drag);
        mv(&mut sensor, &mut drag, 10., 20.);
        mv(&mut sensor, &mut drag, 90., 30.);
        sensor.pointer_move(1, Point::new(94., 32.), false, &mut drag);
        up(&mut sensor, &mut drag, 94., 32.);
        sensor.pointer_move(1, Point::new(100., 40.), false, &mut drag);
        assert_eq!(drag.drops, vec![Point::new(94., 32.)]);
        assert_eq!(drag.cancel, 1);
        assert_eq!(drag.end, 0);
        assert_eq!(drag.finish, vec![true]);
    }

    #[test]
    fn cancels_a_missed_sidebar_release_without_committing_a_reorder() {
        let mut drag = Recorder {
            claim_beyond: Some(50.),
            ..Default::default()
        };
        let mut sensor = gesture(&mut drag);
        mv(&mut sensor, &mut drag, 90., 30.);
        sensor.pointer_move(1, Point::new(30., 40.), false, &mut drag);
        up(&mut sensor, &mut drag, 30., 40.);
        assert_eq!(drag.drops, vec![Point::new(30., 40.)]);
        assert_eq!(drag.cancel, 1);
        assert_eq!(drag.end, 0);
        assert_eq!(drag.finish, vec![true]);
    }

    #[test]
    fn does_not_drop_context_when_the_button_is_released_before_activation() {
        let mut drag = Recorder {
            claim_beyond: Some(0.),
            ..Default::default()
        };
        let mut sensor = gesture(&mut drag);
        sensor.pointer_move(1, Point::new(90., 30.), false, &mut drag);
        up(&mut sensor, &mut drag, 90., 30.);
        assert!(drag.drops.is_empty());
        assert!(drag.start.is_empty());
        assert_eq!(drag.abort, 1);
        assert_eq!(drag.finish, vec![false]);
    }

    #[test]
    fn resumes_reordering_when_a_context_drag_returns_to_the_sidebar() {
        let mut drag = Recorder {
            claim_beyond: Some(50.),
            ..Default::default()
        };
        let mut sensor = gesture(&mut drag);
        mv(&mut sensor, &mut drag, 10., 20.);
        mv(&mut sensor, &mut drag, 30., 30.);
        mv(&mut sensor, &mut drag, 90., 100.);
        mv(&mut sensor, &mut drag, 90., 200.);
        assert_eq!(drag.moves.len(), 2);
        assert_eq!(drag.moves.last(), Some(&Point::new(30., 30.)));
        mv(&mut sensor, &mut drag, 30., 60.);
        assert_eq!(drag.moves.last(), Some(&Point::new(30., 60.)));
        up(&mut sensor, &mut drag, 30., 60.);
        assert_eq!(drag.end, 1);
        assert_eq!(drag.cancel, 0);
    }

    #[test]
    fn suppresses_a_cancelled_release_click() {
        for started in [false, true] {
            let mut drag = Recorder::default();
            let mut sensor = gesture(&mut drag);
            if started {
                mv(&mut sensor, &mut drag, 10., 20.);
            }
            sensor.cancel(&mut drag);
            assert!(sensor.consume_click());
            sensor.pointer_down();
            assert!(!sensor.consume_click());
        }
    }

    #[test]
    fn allows_a_fresh_click_after_an_outside_release() {
        for started in [false, true] {
            let mut drag = Recorder::default();
            let mut sensor = gesture(&mut drag);
            if started {
                mv(&mut sensor, &mut drag, 10., 20.);
            }
            sensor.cancel(&mut drag);
            sensor.pointer_down();
            assert!(!sensor.consume_click());
        }
    }

    #[test]
    fn does_not_move_after_cancellation_during_activation() {
        let mut drag = Recorder {
            cancel_on_start: true,
            ..Default::default()
        };
        let mut sensor = gesture(&mut drag);
        mv(&mut sensor, &mut drag, 10., 20.);
        assert_eq!(drag.cancel, 1);
        assert!(drag.moves.is_empty());
        assert_eq!(drag.finish.len(), 1);
    }

    #[test]
    fn ignores_events_from_other_pointers() {
        let mut drag = Recorder::default();
        let mut sensor = gesture(&mut drag);
        sensor.pointer_move(2, Point::new(10., 100.), false, &mut drag);
        sensor.pointer_cancel(2, &mut drag);
        sensor.pointer_up(2, Point::new(10., 100.), &mut drag);
        assert!(drag.start.is_empty());
        assert!(drag.finish.is_empty());
        mv(&mut sensor, &mut drag, 10., 20.);
        assert_eq!(drag.start.len(), 1);
    }

    #[test]
    fn cancels_when_the_list_unmounts() {
        for started in [false, true] {
            let mut drag = Recorder::default();
            let mut sensor = gesture(&mut drag);
            if started {
                mv(&mut sensor, &mut drag, 10., 20.);
            }
            // SidebarDragLifecycle: unmount cancels the sensor.
            sensor.cancel(&mut drag);
            mv(&mut sensor, &mut drag, 10., 100.);
            up(&mut sensor, &mut drag, 10., 100.);
            assert_eq!(drag.start.len(), usize::from(started));
            assert_eq!(drag.cancel, 1);
            assert_eq!(drag.end, 0);
            assert_eq!(drag.finish.len(), 1);
        }
    }
}
