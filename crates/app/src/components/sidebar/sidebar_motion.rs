//! Port of upstream/apps/web/src/components/Sidebar.motion.ts
//!
//! `createSidebarListMotion`: animate rows between their layout positions.
//! Upstream reads `offsetTop` from DOM children and plays Web Animations;
//! here the view passes each frame's measured rows to [`SidebarListMotion::update`]
//! and samples per-row translate/opacity (and exit ghosts) while painting.
//! Animations are timed against a caller clock (ms) with upstream's 150ms
//! `ease-out`, so the engine stays pure and testable.

use std::collections::HashMap;

/// `{ duration: 150, easing: "ease-out" }`.
pub const MOTION_DURATION_MS: f64 = 150.;

/// Rows normally ride their displaced neighbour's travel. Absent a moving
/// neighbour, a row still travels on its own, clamped so a tall card does
/// not slide its full height.
fn row_travel(height: f32) -> f32 {
    height.min(40.)
}

/// A filter change or bulk snooze swaps a large part of the list at once.
/// Fades are the expensive part, so only the fade count decides whether an
/// update animates.
const MAX_FADED_ROWS_PER_UPDATE: usize = 40;

/// A row's layout box relative to the list (`offsetTop` etc.).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RowPosition {
    pub top: f32,
    pub left: f32,
    pub width: f32,
    pub height: f32,
}

/// CSS `ease-out` = `cubic-bezier(0, 0, 0.58, 1)`.
pub fn ease_out(t: f64) -> f64 {
    let t = t.clamp(0., 1.);
    let (x1, y1, x2, y2) = (0., 0., 0.58, 1.);
    let bezier = |a: f64, b: f64, s: f64| {
        3. * (1. - s).powi(2) * s * a + 3. * (1. - s) * s.powi(2) * b + s.powi(3)
    };
    // Solve x(s) = t by bisection; x is monotonic for these control points.
    let (mut low, mut high) = (0., 1.);
    for _ in 0..40 {
        let mid = (low + high) / 2.;
        if bezier(x1, x2, mid) < t {
            low = mid;
        } else {
            high = mid;
        }
    }
    bezier(y1, y2, (low + high) / 2.)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Clip {
    start: f64,
}

impl Clip {
    /// Computed progress, easing included (WAAPI `getComputedTiming`).
    fn progress(&self, now: f64) -> f32 {
        ease_out((now - self.start) / MOTION_DURATION_MS) as f32
    }

    fn finished(&self, now: f64) -> bool {
        now - self.start >= MOTION_DURATION_MS
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Move {
    clip: Clip,
    offset: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Enter {
    clip: Clip,
    travel: f32,
}

/// A removed row still fading out (upstream's inert DOM clone).
#[derive(Clone, Debug, PartialEq)]
pub struct ExitGhost {
    pub key: String,
    /// The clone's box: last layout box shifted by its remaining travel.
    pub position: RowPosition,
    pub from_opacity: f32,
    pub travel: f32,
    clip: Clip,
}

impl ExitGhost {
    /// `(translate_y, opacity)` at `now`.
    pub fn sample(&self, now: f64) -> (f32, f32) {
        let p = self.clip.progress(now);
        (self.travel * p, self.from_opacity * (1. - p))
    }
}

/// A row's visual state at a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowMotion {
    pub translate_y: f32,
    pub opacity: f32,
}

impl Default for RowMotion {
    fn default() -> Self {
        Self {
            translate_y: 0.,
            opacity: 1.,
        }
    }
}

/// `createSidebarListMotion(parent)`.
#[derive(Debug, Default)]
pub struct SidebarListMotion {
    positions: Option<Vec<(String, RowPosition)>>,
    disposed: bool,
    running: HashMap<String, Move>,
    entering: HashMap<String, Enter>,
    exiting: Vec<ExitGhost>,
    /// Visual tops at drag release, so the release commit glides every row
    /// from where the drag left it into its slot.
    released: Option<HashMap<String, f32>>,
    /// `prefers-reduced-motion: reduce`.
    pub reduced_motion: bool,
}

impl SidebarListMotion {
    pub fn new() -> Self {
        Self::default()
    }

    fn remaining_offset(&self, key: &str, now: f64) -> f32 {
        let run = self
            .running
            .get(key)
            .map_or(0., |m| m.offset * (1. - m.clip.progress(now)));
        let enter = self
            .entering
            .get(key)
            .map_or(0., |e| e.travel * (1. - e.clip.progress(now)));
        run + enter
    }

    fn clear_fades(&mut self) {
        self.entering.clear();
        self.exiting.clear();
    }

    fn fade_out(&mut self, key: &str, position: RowPosition, travel: f32, now: f64) {
        if position.height == 0. {
            return;
        }
        let entry_progress = self
            .entering
            .get(key)
            .map_or(1., |entry| entry.clip.progress(now));
        let top = position.top + self.remaining_offset(key, now);
        self.exiting.push(ExitGhost {
            key: key.to_string(),
            position: RowPosition { top, ..position },
            from_opacity: entry_progress,
            travel,
            clip: Clip { start: now },
        });
    }

    fn move_row(&mut self, key: &str, offset: f32, now: f64) {
        self.running.remove(key);
        let has_entry = match self.entering.get_mut(key) {
            // The newer transform supersedes entry travel; its original
            // opacity keeps fading.
            Some(entry) => {
                entry.travel = 0.;
                true
            }
            None => false,
        };
        if offset == 0. && !has_entry {
            return;
        }
        self.running.insert(
            key.to_string(),
            Move {
                clip: Clip { start: now },
                offset,
            },
        );
    }

    /// Drop finished animations (WAAPI `finish` events).
    pub fn tick(&mut self, now: f64) {
        self.running.retain(|_, m| !m.clip.finished(now));
        self.entering.retain(|_, e| !e.clip.finished(now));
        self.exiting.retain(|ghost| !ghost.clip.finished(now));
    }

    /// Whether any animation is still playing at `now`.
    pub fn is_animating(&self, now: f64) -> bool {
        self.running.values().any(|m| !m.clip.finished(now))
            || self.entering.values().any(|e| !e.clip.finished(now))
            || self.exiting.iter().any(|g| !g.clip.finished(now))
    }

    /// `update(animate)` with this frame's rows (list order, layout boxes).
    pub fn update(&mut self, animate: bool, next: Vec<(String, RowPosition)>, now: f64) {
        if self.disposed {
            return;
        }
        let next_index: HashMap<&str, usize> = next
            .iter()
            .enumerate()
            .map(|(index, (key, _))| (key.as_str(), index))
            .collect();
        let previous = self.positions.take();
        let previous_index: HashMap<String, usize> = previous
            .iter()
            .flatten()
            .enumerate()
            .map(|(index, (key, _))| (key.clone(), index))
            .collect();
        let mut fade_count = 0;
        if let Some(previous) = &previous {
            fade_count += previous
                .iter()
                .filter(|(key, position)| {
                    !next_index.contains_key(key.as_str()) && position.height > 0.
                })
                .count();
            fade_count += next
                .iter()
                .filter(|(key, position)| !previous_index.contains_key(key) && position.height > 0.)
                .count();
        }
        let should_animate = animate
            && previous.is_some()
            && !self.reduced_motion
            && fade_count <= MAX_FADED_ROWS_PER_UPDATE;
        let mut moved_delta: HashMap<String, f32> = HashMap::new();
        if !should_animate {
            self.clear_fades();
        } else if let Some(previous) = &previous {
            // A shelf that opens above its collapsed anchor shifts every
            // retained row by the same amount. Entering rows take that same
            // displacement so the shelf arrives as one block; exiting rows
            // leave by it.
            for (key, position) in &next {
                let Some(previous_top) = previous_index.get(key).map(|i| previous[*i].1.top) else {
                    continue;
                };
                if previous_top == position.top {
                    continue;
                }
                let delta = previous_top + self.remaining_offset(key, now) - position.top;
                moved_delta.insert(key.clone(), delta);
            }
            for (index, (key, position)) in previous.iter().enumerate() {
                if next_index.contains_key(key.as_str()) {
                    continue;
                }
                let delta = self.riding_delta(previous, index, &moved_delta, now, |key| {
                    next_index.contains_key(key)
                });
                let travel = match delta {
                    None => row_travel(position.height),
                    Some(delta) => -delta,
                };
                self.fade_out(key, *position, travel, now);
            }
        }
        self.entering
            .retain(|key, _| next_index.contains_key(key.as_str()));
        self.running
            .retain(|key, _| should_animate && next_index.contains_key(key.as_str()));
        if should_animate && let Some(previous) = &previous {
            for (index, (key, position)) in next.iter().enumerate() {
                if !previous_index.contains_key(key) {
                    if position.height > 0. {
                        let delta = self.riding_delta(&next, index, &moved_delta, now, |key| {
                            previous_index.contains_key(key)
                        });
                        let travel = match delta {
                            None => -row_travel(position.height),
                            Some(delta) => delta,
                        };
                        self.entering.insert(
                            key.clone(),
                            Enter {
                                clip: Clip { start: now },
                                travel,
                            },
                        );
                    }
                    continue;
                }
                // Only our own translate is carried forward.
                if let Some(delta) = moved_delta.get(key).copied() {
                    self.move_row(key, delta, now);
                }
            }
        }
        if let Some(released) = self.released.take()
            && !self.reduced_motion
        {
            for (key, position) in &next {
                if let Some(top) = released.get(key) {
                    self.move_row(key, top - position.top, now);
                }
            }
        }
        self.positions = Some(next);
    }

    fn riding_delta(
        &self,
        order: &[(String, RowPosition)],
        index: usize,
        moved_delta: &HashMap<String, f32>,
        now: f64,
        retained: impl Fn(&str) -> bool,
    ) -> Option<f32> {
        for (key, _) in order[..index].iter().rev() {
            if let Some(delta) = moved_delta.get(key) {
                return Some(*delta);
            }
            if retained(key) {
                return Some(self.remaining_offset(key, now));
            }
        }
        None
    }

    /// Called on drag release, before the commit that clears the drag
    /// transforms: every row's visual top relative to the list.
    pub fn release(&mut self, visual_tops: Vec<(String, f32)>) {
        self.suspend();
        self.released = Some(visual_tops.into_iter().collect());
    }

    /// Stop all motion (pickup, filter flips) and forget the baseline.
    pub fn suspend(&mut self) {
        self.running.clear();
        self.clear_fades();
        self.positions = None;
        self.released = None;
    }

    pub fn dispose(&mut self) {
        self.suspend();
        self.disposed = true;
    }

    /// The row's translate and opacity at `now`.
    pub fn sample(&self, key: &str, now: f64) -> RowMotion {
        let mut motion = RowMotion::default();
        if let Some(m) = self.running.get(key) {
            motion.translate_y += m.offset * (1. - m.clip.progress(now));
        }
        if let Some(e) = self.entering.get(key) {
            let p = e.clip.progress(now);
            motion.translate_y += e.travel * (1. - p);
            motion.opacity = p;
        }
        motion
    }

    /// Exit ghosts still fading.
    pub fn exiting(&self) -> &[ExitGhost] {
        &self.exiting
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clock time at which an animation started at 0 reports `progress`.
    fn at_progress(progress: f32) -> f64 {
        let (mut low, mut high) = (0., MOTION_DURATION_MS);
        for _ in 0..60 {
            let mid = (low + high) / 2.;
            if (ease_out(mid / MOTION_DURATION_MS) as f32) < progress {
                low = mid;
            } else {
                high = mid;
            }
        }
        (low + high) / 2.
    }

    struct Fixture {
        motion: SidebarListMotion,
        heights: HashMap<String, f32>,
        order: Vec<String>,
        /// Extra visual offsets (dnd-kit transforms), for release().
        drag: HashMap<String, f32>,
        /// Manual offsetTop overrides applied after layout.
        shift: HashMap<String, f32>,
    }

    impl Fixture {
        fn new(rows: &[(&str, f32)]) -> Self {
            let mut fixture = Self {
                motion: SidebarListMotion::new(),
                heights: rows.iter().map(|(k, h)| (k.to_string(), *h)).collect(),
                order: Vec::new(),
                drag: HashMap::new(),
                shift: HashMap::new(),
            };
            fixture.layout(&rows.iter().map(|(k, _)| *k).collect::<Vec<_>>());
            fixture
        }

        fn layout(&mut self, order: &[&str]) {
            self.order = order.iter().map(|k| k.to_string()).collect();
            self.shift.clear();
        }

        fn positions(&self) -> Vec<(String, RowPosition)> {
            let mut top = 8.;
            self.order
                .iter()
                .map(|key| {
                    let height = self.heights.get(key).copied().unwrap_or(82.);
                    let position = RowPosition {
                        top: top + self.shift.get(key).copied().unwrap_or(0.),
                        left: 4.,
                        width: 260.,
                        height,
                    };
                    top += height + 1.;
                    (key.clone(), position)
                })
                .collect()
        }

        fn update(&mut self, animate: bool, now: f64) {
            let positions = self.positions();
            self.motion.update(animate, positions, now);
        }

        fn release(&mut self) {
            let tops = self
                .positions()
                .into_iter()
                .map(|(key, p)| {
                    let drag = self.drag.get(&key).copied().unwrap_or(0.);
                    (key, p.top + drag)
                })
                .collect();
            self.motion.release(tops);
        }

        fn move_offset(&self, key: &str) -> Option<f32> {
            self.motion.running.get(key).map(|m| m.offset)
        }

        fn entry_travel(&self, key: &str) -> Option<f32> {
            self.motion.entering.get(key).map(|e| e.travel)
        }

        fn ghost(&self, key: &str) -> Option<&ExitGhost> {
            self.motion.exiting.iter().find(|g| g.key == key)
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn moves_a_retained_active_row_into_settled_with_its_displaced_peers() {
        let mut f = Fixture::new(&[
            ("Pinned", 0.),
            ("pin", 82.),
            ("Active", 0.),
            ("a", 82.),
            ("b", 82.),
            ("Settled", 32.),
            ("settled", 36.),
        ]);
        f.update(true, 0.);
        assert!(f.motion.running.is_empty() && f.motion.entering.is_empty());
        f.heights.insert("a".into(), 36.);
        f.layout(&["Pinned", "pin", "Active", "b", "Settled", "settled", "a"]);
        f.update(true, 1000.);
        assert_eq!(f.move_offset("a"), Some(-153.));
        assert_eq!(f.move_offset("b"), Some(83.));
        assert_eq!(f.move_offset("Settled"), Some(83.));
        assert_eq!(f.move_offset("settled"), Some(83.));
        assert_eq!(f.move_offset("pin"), None);
        assert_eq!(f.move_offset("Active"), None);
    }

    #[test]
    fn glides_every_row_from_its_released_position_into_its_committed_slot() {
        let mut f = Fixture::new(&[("a", 82.), ("b", 82.), ("c", 82.)]);
        f.update(true, 0.);
        f.motion.suspend();
        f.drag.insert("a".into(), 130.);
        f.drag.insert("b".into(), -83.);
        f.drag.insert("c".into(), 16.);
        f.update(false, 10.);
        f.release();
        f.layout(&["b", "a", "c"]);
        f.drag.clear();
        f.update(true, 20.);
        assert_eq!(f.move_offset("a"), Some(47.));
        assert_eq!(f.move_offset("b"), None);
        assert_eq!(f.move_offset("c"), Some(16.));
    }

    #[test]
    fn does_not_glide_on_release_when_motion_is_reduced() {
        let mut f = Fixture::new(&[("a", 82.), ("b", 82.)]);
        f.update(true, 0.);
        f.motion.reduced_motion = true;
        f.drag.insert("a".into(), 100.);
        f.release();
        f.layout(&["b", "a"]);
        f.drag.clear();
        f.update(true, 10.);
        assert!(f.motion.running.is_empty());
    }

    #[test]
    fn does_not_carry_a_canceled_drags_transformed_position_into_the_next_move() {
        let mut f = Fixture::new(&[("a", 82.), ("b", 82.)]);
        f.update(true, 0.);
        f.motion.suspend();
        f.update(false, 10.);
        f.motion.suspend();
        f.update(true, 20.);
        assert!(f.motion.running.is_empty());
        f.layout(&["b", "a"]);
        f.update(true, 30.);
        assert_eq!(f.move_offset("a"), Some(-83.));
        assert_eq!(f.move_offset("b"), Some(83.));
    }

    #[test]
    fn retargets_rapid_changes_from_the_current_visual_position() {
        let mut f = Fixture::new(&[("a", 99.), ("b", 99.), ("c", 99.)]);
        f.update(true, 0.);
        f.layout(&["b", "c", "a"]);
        f.update(true, 0.);
        assert_eq!(f.move_offset("a"), Some(-200.));
        f.layout(&["b", "a", "c"]);
        f.update(true, at_progress(0.25));
        assert!(close(f.move_offset("a").unwrap(), -50.));
        f.motion.suspend();
        assert!(f.motion.running.is_empty());
    }

    #[test]
    fn keeps_an_uninterrupted_movement_when_the_layout_position_does_not_change() {
        let mut f = Fixture::new(&[("a", 82.), ("b", 82.)]);
        f.update(true, 0.);
        f.layout(&["b", "a"]);
        f.update(true, 0.);
        let first = f.motion.running["a"];
        f.update(true, at_progress(0.5));
        assert_eq!(f.motion.running["a"], first);
    }

    #[test]
    fn cancels_owned_motion_on_suspension_and_never_animates_a_disposed_list() {
        let mut f = Fixture::new(&[("a", 82.), ("b", 82.)]);
        f.update(true, 0.);
        f.layout(&["b", "a"]);
        f.update(true, 0.);
        f.motion.suspend();
        assert!(f.motion.running.is_empty());
        f.update(false, 10.);
        f.layout(&["a", "b"]);
        f.update(true, 20.);
        assert!(f.move_offset("a").is_some());
        f.motion.dispose();
        assert!(f.motion.running.is_empty());
        f.layout(&["b", "a"]);
        f.update(true, 30.);
        assert!(f.motion.running.is_empty());
    }

    #[test]
    fn fades_a_collapsed_shelf_exit_at_its_current_visual_box_and_a_new_wake_in() {
        let mut f = Fixture::new(&[("a", 82.), ("b", 82.), ("new", 82.)]);
        f.layout(&["a", "b"]);
        f.update(true, 0.);
        f.layout(&["b", "a"]);
        f.update(true, 0.);
        // Only a's animation has progressed (b's stays at 0).
        f.motion.running.get_mut("b").unwrap().clip.start = at_progress(0.5);
        f.layout(&["b", "new"]);
        f.update(true, at_progress(0.5));
        assert!(!f.motion.running.contains_key("a"));
        assert_eq!(f.entry_travel("new"), Some(83.));
        let ghost = f.ghost("a").unwrap();
        assert!(close(ghost.position.top, 49.5));
        assert_eq!(ghost.position.left, 4.);
        assert_eq!(ghost.position.width, 260.);
        assert_eq!(ghost.position.height, 82.);
        assert_eq!(ghost.from_opacity, 1.);
        assert_eq!(ghost.travel, -83.);
        let before = f.motion.exiting.len();
        f.update(true, at_progress(0.5) + 1.);
        assert_eq!(f.motion.exiting.len(), before);
        f.motion.tick(at_progress(0.5) + MOTION_DURATION_MS);
        assert!(f.ghost("a").is_none());
    }

    #[test]
    fn rides_entering_rows_on_the_shelf_displacement() {
        let mut f = Fixture::new(&[
            ("a", 40.),
            ("header", 32.),
            ("x", 36.),
            ("y", 36.),
            ("z", 36.),
        ]);
        f.layout(&["a", "header", "x"]);
        f.update(true, 0.);
        f.layout(&["a", "header", "x", "y", "z"]);
        for key in ["header", "x", "y", "z"] {
            f.shift.insert(key.into(), -72.);
        }
        f.update(true, 0.);
        assert_eq!(f.move_offset("header"), Some(72.));
        assert_eq!(f.move_offset("x"), Some(72.));
        assert_eq!(f.move_offset("a"), None);
        assert_eq!(f.entry_travel("y"), Some(72.));
        assert_eq!(f.entry_travel("z"), Some(72.));
    }

    #[test]
    fn rides_new_rows_on_a_retained_headers_unfinished_travel() {
        let mut f = Fixture::new(&[("header", 32.), ("above", 39.), ("incoming", 36.)]);
        f.layout(&["header"]);
        f.update(true, 0.);
        f.layout(&["above", "header"]);
        f.update(true, 0.);
        f.layout(&["above", "header", "incoming"]);
        f.update(true, at_progress(0.25));
        assert!(close(f.entry_travel("incoming").unwrap(), -30.));
    }

    #[test]
    fn cancels_in_flight_row_travel_when_a_bulk_update_skips_animation() {
        let mut heights = vec![("a", 82.), ("b", 82.)];
        let names: Vec<String> = (0..41).map(|i| format!("new-{i}")).collect();
        heights.extend(names.iter().map(|n| (n.as_str(), 82.)));
        let mut f = Fixture::new(&heights);
        f.layout(&["a", "b"]);
        f.update(true, 0.);
        f.layout(&["b", "a"]);
        f.update(true, 0.);
        f.layout(&names.iter().map(String::as_str).collect::<Vec<_>>());
        f.update(true, at_progress(0.4));
        assert!(f.motion.running.is_empty());
        assert!(f.motion.entering.is_empty());
    }

    #[test]
    fn keeps_in_flight_shelf_entry_travel_when_the_shelf_closes_mid_animation() {
        let mut f = Fixture::new(&[("a", 40.), ("header", 32.), ("x", 36.), ("y", 36.)]);
        f.layout(&["a", "header", "x"]);
        f.update(true, 0.);
        f.layout(&["a", "header", "x", "y"]);
        for key in ["header", "x", "y"] {
            f.shift.insert(key.into(), -36.);
        }
        f.update(true, 0.);
        f.layout(&["a", "header", "x"]);
        f.update(true, at_progress(0.25));
        assert!(!f.motion.entering.contains_key("y"));
        let ghost = f.ghost("y").unwrap();
        assert!(close(ghost.position.top, 110.));
        assert!(close(ghost.from_opacity, 0.25));
        assert!(close(ghost.travel, 9.));
        assert!(close(f.move_offset("header").unwrap(), -9.));
        assert!(close(f.move_offset("x").unwrap(), -9.));
    }

    #[test]
    fn clears_exit_clones_on_pickup_and_does_not_fade_the_release_commit() {
        let mut f = Fixture::new(&[("a", 82.), ("b", 82.), ("c", 82.)]);
        f.layout(&["a", "b"]);
        f.update(false, 0.);
        f.layout(&["b"]);
        f.update(true, 0.);
        assert!(f.ghost("a").is_some());
        f.motion.suspend();
        assert!(f.motion.exiting.is_empty());
        f.update(false, 10.);
        f.motion.suspend();
        f.layout(&["c"]);
        f.update(true, 20.);
        assert!(f.ghost("b").is_none());
        assert!(f.motion.entering.is_empty());
        f.layout(&["c", "a"]);
        f.update(true, 30.);
        assert_eq!(f.entry_travel("a"), Some(0.));
        f.motion.dispose();
        assert!(f.motion.entering.is_empty());
    }

    #[test]
    fn carries_entry_opacity_into_a_quick_exit_and_removes_artifacts_on_a_silent_update() {
        let mut f = Fixture::new(&[("boundary", 0.), ("a", 82.)]);
        f.layout(&["boundary"]);
        f.update(false, 0.);
        f.layout(&["boundary", "a"]);
        f.update(true, 0.);
        f.layout(&[]);
        f.update(true, at_progress(0.4));
        assert!(f.ghost("boundary").is_none());
        let ghost = f.ghost("a").unwrap();
        assert!(close(ghost.from_opacity, 0.4));
        assert_eq!(ghost.travel, 40.);
        f.update(false, at_progress(0.4) + 1.);
        assert!(f.motion.exiting.is_empty());
    }

    #[test]
    fn retargets_an_in_flight_entry_when_a_later_layout_shift_moves_the_row() {
        let mut f = Fixture::new(&[("a", 40.), ("header", 32.), ("x", 36.), ("y", 36.)]);
        f.layout(&["a", "header", "x"]);
        f.update(true, 0.);
        f.layout(&["a", "header", "x", "y"]);
        for key in ["header", "x", "y"] {
            f.shift.insert(key.into(), -36.);
        }
        f.update(true, 0.);
        for key in ["header", "x", "y"] {
            f.shift.insert(key.into(), 4.);
        }
        f.update(true, at_progress(0.25));
        assert!(f.motion.entering.contains_key("y"));
        assert!(close(f.move_offset("y").unwrap(), -13.));
    }

    #[test]
    fn skips_fades_when_a_large_list_change_would_clone_too_many_rows() {
        let rows: Vec<String> = (0..41).map(|i| format!("row-{i}")).collect();
        let fresh: Vec<String> = (0..41).map(|i| format!("new-{i}")).collect();
        let mut all: Vec<(&str, f32)> = rows.iter().map(|r| (r.as_str(), 82.)).collect();
        all.extend(fresh.iter().map(|r| (r.as_str(), 82.)));
        let mut f = Fixture::new(&all);
        f.layout(&rows.iter().map(String::as_str).collect::<Vec<_>>());
        f.update(true, 0.);
        f.layout(&[]);
        f.update(true, 10.);
        assert!(f.motion.exiting.is_empty());
        f.layout(&fresh.iter().map(String::as_str).collect::<Vec<_>>());
        f.update(true, 20.);
        assert!(f.motion.entering.is_empty());
    }

    #[test]
    fn still_slides_rows_when_one_removal_displaces_a_large_list() {
        let rows: Vec<String> = (0..60).map(|i| format!("row-{i}")).collect();
        let mut f = Fixture::new(&rows.iter().map(|r| (r.as_str(), 82.)).collect::<Vec<_>>());
        f.update(true, 0.);
        f.layout(&rows[1..].iter().map(String::as_str).collect::<Vec<_>>());
        f.update(true, 10.);
        assert!(f.ghost("row-0").is_some());
        assert!(rows[1..].iter().all(|r| f.motion.running.contains_key(r)));
    }

    #[test]
    fn respects_reduced_motion_while_keeping_the_next_baseline_fresh() {
        let mut f = Fixture::new(&[("a", 82.), ("b", 82.)]);
        f.update(true, 0.);
        f.motion.reduced_motion = true;
        f.layout(&["b", "a"]);
        f.update(true, 10.);
        assert!(f.motion.running.is_empty());
        f.motion.reduced_motion = false;
        f.layout(&["a", "b"]);
        f.update(true, 20.);
        assert_eq!(f.move_offset("a"), Some(83.));
        assert_eq!(f.move_offset("b"), Some(-83.));
    }

    #[test]
    fn samples_ease_out_toward_rest() {
        let mut f = Fixture::new(&[("a", 82.), ("b", 82.)]);
        f.update(true, 0.);
        f.layout(&["b", "a"]);
        f.update(true, 0.);
        assert_eq!(f.motion.sample("a", 0.).translate_y, -83.);
        assert!(f.motion.sample("a", 75.).translate_y > -83. * 0.5);
        assert_eq!(f.motion.sample("a", 150.).translate_y, 0.);
        assert!(f.motion.is_animating(100.));
        assert!(!f.motion.is_animating(150.));
    }
}
