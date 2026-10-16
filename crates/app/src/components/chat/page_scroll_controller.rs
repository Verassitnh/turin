//! Port of upstream/apps/web/src/components/chat/pageScrollController.ts
//!
//! PageUp/PageDown in the timeline: a tap scrolls one page with an ease-in-out
//! animation, and holding the key past a short delay switches to a hold scroll
//! whose speed ramps up over time.
//!
//! Upstream schedules its frames and timers with closures on `requestAnimationFrame`
//! and `setTimeout`. This port is pull-based: the view calls
//! [`PageScrollController::frame`] once per frame while
//! [`PageScrollController::needs_frame`] holds, passing the current time and the
//! container's metrics. A returned `Some(scroll_top)` is what upstream would have
//! assigned to `container.scrollTop` that frame. The hold-delay timeout is checked
//! at the top of each frame, before any animation step, as upstream's timers run
//! before its animation frames.

/// Length of the tap animation. The hold delay is the same length.
pub const PAGE_SCROLL_ANIMATION_MS: f64 = 150.0;
/// Hold velocity reaches its maximum this long after the hold starts.
pub const PAGE_SCROLL_ACCELERATION_MS: f64 = 400.0;
pub const PAGE_SCROLL_MAX_MULTIPLIER: f64 = 2.0;

const PAGE_SCROLL_ALIGNMENT_OFFSET_PX: f64 = 36.0;
const PAGE_SCROLL_BOUNDARY_EPSILON_PX: f64 = 1.0;
const PAGE_SCROLL_HOLD_DELAY_MS: f64 = PAGE_SCROLL_ANIMATION_MS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageScrollKey {
    PageUp,
    PageDown,
}

impl PageScrollKey {
    /// The DOM `KeyboardEvent.key` value for this key.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PageUp => "PageUp",
            Self::PageDown => "PageDown",
        }
    }

    fn direction(self) -> f64 {
        match self {
            Self::PageUp => -1.0,
            Self::PageDown => 1.0,
        }
    }
}

/// The scroll container's geometry at the moment of a read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageScrollMetrics {
    pub client_height: f64,
    pub scroll_height: f64,
    pub scroll_top: f64,
    /// `getBoundingClientRect().height`, which page distance is measured from.
    pub bounding_height: f64,
}

impl PageScrollMetrics {
    fn max_scroll_top(&self) -> f64 {
        (self.scroll_height - self.client_height).max(0.0)
    }

    /// `scrollTop` as the browser stores it: clamped to the scrollable range.
    fn clamped_scroll_top(&self, value: f64) -> f64 {
        value.min(self.max_scroll_top()).max(0.0)
    }
}

/// A key event as the timeline sees it. `key_code` is the legacy code (229 during IME).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageScrollKeyEvent<'a> {
    pub alt_key: bool,
    pub ctrl_key: bool,
    pub meta_key: bool,
    pub shift_key: bool,
    pub default_prevented: bool,
    pub is_composing: bool,
    pub key: &'a str,
    pub key_code: u32,
    pub client_height: f64,
    pub scroll_height: f64,
    pub scroll_top: f64,
}

fn can_scroll_in_direction(
    client_height: f64,
    scroll_height: f64,
    scroll_top: f64,
    key: PageScrollKey,
) -> bool {
    let max_scroll_top = (scroll_height - client_height).max(0.0);
    let clamped_scroll_top = scroll_top.min(max_scroll_top).max(0.0);
    match key {
        PageScrollKey::PageUp => clamped_scroll_top > PAGE_SCROLL_BOUNDARY_EPSILON_PX,
        PageScrollKey::PageDown => {
            clamped_scroll_top < max_scroll_top - PAGE_SCROLL_BOUNDARY_EPSILON_PX
        }
    }
}

/// The page key the timeline should take over, or `None` when the key belongs to the
/// composer or to the platform. A composer that can still scroll in the key's direction keeps it.
pub fn get_timeline_page_scroll_key(event: &PageScrollKeyEvent<'_>) -> Option<PageScrollKey> {
    let key = match event.key {
        "PageUp" => PageScrollKey::PageUp,
        "PageDown" => PageScrollKey::PageDown,
        _ => return None,
    };
    if event.default_prevented
        || event.is_composing
        || event.key_code == 229
        || event.alt_key
        || event.ctrl_key
        || event.meta_key
        || event.shift_key
    {
        return None;
    }

    let editor_can_scroll = can_scroll_in_direction(
        event.client_height,
        event.scroll_height,
        event.scroll_top,
        key,
    );
    if editor_can_scroll { None } else { Some(key) }
}

pub fn get_page_scroll_multiplier(hold_elapsed_ms: f64) -> f64 {
    let progress = hold_elapsed_ms.max(0.0) / PAGE_SCROLL_ACCELERATION_MS;
    1.0 + progress.min(1.0) * (PAGE_SCROLL_MAX_MULTIPLIER - 1.0)
}

pub fn get_page_scroll_velocity_px_per_ms(
    hold_elapsed_ms: f64,
    page_scroll_distance_px: f64,
) -> f64 {
    (page_scroll_distance_px * get_page_scroll_multiplier(hold_elapsed_ms))
        / PAGE_SCROLL_ANIMATION_MS
}

pub fn get_page_scroll_distance_px(container_height_px: f64, scroll_padding_bottom_px: f64) -> f64 {
    (container_height_px - PAGE_SCROLL_ALIGNMENT_OFFSET_PX - scroll_padding_bottom_px).max(0.0)
}

/// Cubic-bezier `(0.42, 0, 0.58, 1)`, solved by Newton iteration on x (five steps at most).
fn ease_in_out(progress: f64) -> f64 {
    let ax = 3.0 * 0.42 - 3.0 * 0.58 + 1.0;
    let bx = 3.0 * (0.58 - 2.0 * 0.42);
    let cx = 3.0 * 0.42;
    let ay = -2.0;
    let by = 3.0;
    let x = |value: f64| ((ax * value + bx) * value + cx) * value;
    let y = |value: f64| (ay * value + by) * value * value;

    let mut value = progress;
    for _ in 0..5 {
        let delta = x(value) - progress;
        let derivative = (3.0 * ax * value + 2.0 * bx) * value + cx;
        if delta.abs() < 1e-4 || derivative == 0.0 {
            break;
        }
        value -= delta / derivative;
        value = value.clamp(0.0, 1.0);
    }

    y(value)
}

#[derive(Debug, Clone, Copy)]
struct DiscreteScroll {
    start_time: f64,
    start_scroll_top: f64,
    delta_y: f64,
}

#[derive(Debug, Clone, Copy)]
struct HoldScroll {
    start_time: f64,
    last_frame_time: f64,
}

#[derive(Debug, Default)]
pub struct PageScrollController {
    active_key: Option<PageScrollKey>,
    discrete: Option<DiscreteScroll>,
    hold_delay_deadline: Option<f64>,
    hold: Option<HoldScroll>,
}

impl PageScrollController {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the view must keep requesting frames. A tap animation keeps running after
    /// its key is released, as upstream's does.
    pub fn needs_frame(&self) -> bool {
        self.discrete.is_some() || self.hold.is_some() || self.hold_delay_deadline.is_some()
    }

    /// Handles a page-key press. Returns `true` when a page scroll starts (upstream's
    /// `onScrollStart`). Auto-repeat presses of the same key, and presses at the boundary
    /// in their direction, return `false`.
    pub fn handle_key_down(
        &mut self,
        key: PageScrollKey,
        now: f64,
        metrics: PageScrollMetrics,
        scroll_padding_bottom_px: f64,
    ) -> bool {
        if !can_scroll_in_direction(
            metrics.client_height,
            metrics.scroll_height,
            metrics.scroll_top,
            key,
        ) {
            return false;
        }
        if self.active_key == Some(key) {
            return false;
        }

        self.stop(true);
        self.active_key = Some(key);

        let page_distance =
            get_page_scroll_distance_px(metrics.bounding_height, scroll_padding_bottom_px);
        self.discrete = Some(DiscreteScroll {
            start_time: now,
            start_scroll_top: metrics.scroll_top,
            delta_y: page_distance * key.direction(),
        });
        self.hold_delay_deadline = Some(now + PAGE_SCROLL_HOLD_DELAY_MS);
        true
    }

    pub fn handle_key_up(&mut self, key: &str) {
        if self.active_key.map(PageScrollKey::as_str) != Some(key) {
            return;
        }

        self.stop(self.hold.is_some());
    }

    pub fn release_active_key(&mut self) {
        self.stop(self.hold.is_some());
    }

    pub fn dispose(&mut self) {
        self.stop(true);
    }

    /// Advances the scroll by one frame. Returns the `scrollTop` to apply, if any.
    pub fn frame(
        &mut self,
        now: f64,
        metrics: PageScrollMetrics,
        scroll_padding_bottom_px: f64,
    ) -> Option<f64> {
        if let Some(deadline) = self.hold_delay_deadline
            && now >= deadline
        {
            self.hold_delay_deadline = None;
            if self.active_key.is_some() {
                self.start_hold(now);
            }
            return None;
        }

        if let Some(discrete) = self.discrete {
            let progress = ((now - discrete.start_time) / PAGE_SCROLL_ANIMATION_MS).min(1.0);
            let value = discrete.start_scroll_top + discrete.delta_y * ease_in_out(progress);
            if progress >= 1.0 {
                self.discrete = None;
            }
            return Some(value);
        }

        let hold = self.hold?;
        let key = self.active_key?;
        let delta_ms = now - hold.last_frame_time;
        self.hold = Some(HoldScroll {
            last_frame_time: now,
            ..hold
        });

        let page_distance =
            get_page_scroll_distance_px(metrics.bounding_height, scroll_padding_bottom_px);
        let velocity_px_per_ms =
            get_page_scroll_velocity_px_per_ms(now - hold.start_time, page_distance);
        let previous_scroll_top = metrics.scroll_top;
        let next_scroll_top = previous_scroll_top + velocity_px_per_ms * delta_ms * key.direction();

        // Upstream reads `scrollTop` back after assigning it, so a browser clamp at the
        // boundary shows up as no movement. Compare the clamped value to get the same result.
        if delta_ms > 0.0 && metrics.clamped_scroll_top(next_scroll_top) == previous_scroll_top {
            self.stop(true);
            return None;
        }

        Some(next_scroll_top)
    }

    /// Upstream `stop`: clears the hold timer and any hold, and optionally the tap animation.
    fn stop(&mut self, cancel_discrete: bool) {
        self.hold_delay_deadline = None;
        self.hold = None;
        if cancel_discrete {
            self.discrete = None;
        }
        self.active_key = None;
    }

    fn start_hold(&mut self, now: f64) {
        self.discrete = None;
        self.hold = Some(HoldScroll {
            start_time: now,
            last_frame_time: now,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake clock and a scroll container. Each 16 ms step runs the due hold timer, then
    /// the animation frame, and the returned `scrollTop` is written back as the browser would.
    struct Harness {
        now: f64,
        container: PageScrollMetrics,
        padding: f64,
    }

    impl Harness {
        fn new(scroll_height: f64, scroll_top: f64) -> Self {
            Self {
                now: 0.0,
                container: PageScrollMetrics {
                    client_height: 600.0,
                    scroll_height,
                    scroll_top,
                    bounding_height: 600.0,
                },
                padding: 24.0,
            }
        }

        fn advance_by(&mut self, controller: &mut PageScrollController, ms: f64) {
            let target = self.now + ms;
            while self.now < target {
                self.now = target.min(self.now + 16.0);
                if let Some(value) = controller.frame(self.now, self.container, self.padding) {
                    self.container.scroll_top = value;
                }
            }
        }
    }

    fn key_event<'a>(
        key: &'a str,
        key_code: u32,
        overrides: impl FnOnce(&mut PageScrollKeyEvent<'a>),
    ) -> PageScrollKeyEvent<'a> {
        let mut event = PageScrollKeyEvent {
            alt_key: false,
            ctrl_key: false,
            meta_key: false,
            shift_key: false,
            default_prevented: false,
            is_composing: false,
            key,
            key_code,
            client_height: 200.0,
            scroll_height: 200.0,
            scroll_top: 0.0,
        };
        overrides(&mut event);
        event
    }

    fn page_down(
        overrides: impl FnOnce(&mut PageScrollKeyEvent<'static>),
    ) -> PageScrollKeyEvent<'static> {
        key_event("PageDown", 34, overrides)
    }

    #[test]
    fn leaves_page_keys_to_ime_composition() {
        assert_eq!(
            get_timeline_page_scroll_key(&page_down(|e| e.is_composing = true)),
            None
        );
        assert_eq!(
            get_timeline_page_scroll_key(&page_down(|e| e.key_code = 229)),
            None
        );
    }

    #[test]
    fn leaves_page_keys_to_an_overflowing_composer_until_it_reaches_the_boundary() {
        assert_eq!(
            get_timeline_page_scroll_key(&page_down(|e| {
                e.scroll_height = 600.0;
                e.scroll_top = 0.0;
            })),
            None
        );
        assert_eq!(
            get_timeline_page_scroll_key(&key_event("PageUp", 33, |e| {
                e.scroll_height = 600.0;
                e.scroll_top = 400.0;
            })),
            None
        );
        assert_eq!(
            get_timeline_page_scroll_key(&key_event("PageUp", 33, |e| {
                e.scroll_height = 600.0;
                e.scroll_top = 0.0;
            })),
            Some(PageScrollKey::PageUp)
        );
        assert_eq!(
            get_timeline_page_scroll_key(&page_down(|e| {
                e.scroll_height = 600.0;
                e.scroll_top = 400.0;
            })),
            Some(PageScrollKey::PageDown)
        );
    }

    #[test]
    fn hands_off_page_keys_within_a_fractional_pixel_of_the_composer_boundary() {
        assert_eq!(
            get_timeline_page_scroll_key(&key_event("PageUp", 33, |e| {
                e.scroll_height = 600.0;
                e.scroll_top = 0.5;
            })),
            Some(PageScrollKey::PageUp)
        );
        assert_eq!(
            get_timeline_page_scroll_key(&page_down(|e| {
                e.scroll_height = 600.0;
                e.scroll_top = 399.5;
            })),
            Some(PageScrollKey::PageDown)
        );
    }

    #[test]
    fn ramps_multiplier_over_time_and_caps_at_the_max_velocity() {
        assert_eq!(get_page_scroll_multiplier(0.0), 1.0);
        assert!((get_page_scroll_multiplier(PAGE_SCROLL_ACCELERATION_MS / 2.0) - 1.5).abs() < 1e-9);
        assert_eq!(
            get_page_scroll_multiplier(PAGE_SCROLL_ACCELERATION_MS * 5.0),
            PAGE_SCROLL_MAX_MULTIPLIER
        );
    }

    #[test]
    fn derives_the_hold_velocity_from_page_size_and_acceleration() {
        assert!((get_page_scroll_velocity_px_per_ms(0.0, 600.0) - 4.0).abs() < 1e-9);
        assert!(
            (get_page_scroll_velocity_px_per_ms(PAGE_SCROLL_ACCELERATION_MS * 5.0, 600.0) - 8.0)
                .abs()
                < 1e-9
        );
    }

    #[test]
    fn keeps_a_single_page_scroll_when_the_key_is_tapped() {
        let mut harness = Harness::new(1_800.0, 0.0);
        let mut controller = PageScrollController::new();
        let metrics = harness.container;

        controller.handle_key_down(
            PageScrollKey::PageDown,
            harness.now,
            metrics,
            harness.padding,
        );
        controller.handle_key_up("PageDown");
        harness.advance_by(&mut controller, PAGE_SCROLL_ANIMATION_MS);

        assert!(
            (harness.container.scroll_top - get_page_scroll_distance_px(600.0, 24.0)).abs() < 1e-5
        );
    }

    #[test]
    fn continues_scrolling_on_hold_without_repeated_keydown_events_and_stops_on_keyup() {
        let mut harness = Harness::new(4_000.0, 0.0);
        let mut controller = PageScrollController::new();

        controller.handle_key_down(
            PageScrollKey::PageDown,
            harness.now,
            harness.container,
            harness.padding,
        );
        harness.advance_by(&mut controller, PAGE_SCROLL_ANIMATION_MS + 50.0);

        let after_hold_starts = harness.container.scroll_top;
        harness.advance_by(&mut controller, 200.0);
        assert!(harness.container.scroll_top > after_hold_starts);

        let stopped_at = harness.container.scroll_top;
        controller.handle_key_up("PageDown");
        harness.advance_by(&mut controller, 250.0);
        assert_eq!(harness.container.scroll_top, stopped_at);
        assert!(!controller.needs_frame());
    }

    #[test]
    fn notifies_once_when_a_page_scroll_starts() {
        let mut controller = PageScrollController::new();
        let harness = Harness::new(1_800.0, 600.0);
        let mut started = 0;

        if controller.handle_key_down(
            PageScrollKey::PageUp,
            harness.now,
            harness.container,
            harness.padding,
        ) {
            started += 1;
        }
        if controller.handle_key_down(
            PageScrollKey::PageUp,
            harness.now,
            harness.container,
            harness.padding,
        ) {
            started += 1;
        }

        assert_eq!(started, 1);
    }

    #[test]
    fn does_not_start_a_page_scroll_at_the_timeline_boundary() {
        let mut harness = Harness::new(1_800.0, 0.5);
        let mut controller = PageScrollController::new();
        let mut started = 0;

        if controller.handle_key_down(
            PageScrollKey::PageUp,
            harness.now,
            harness.container,
            harness.padding,
        ) {
            started += 1;
        }
        harness.advance_by(&mut controller, PAGE_SCROLL_ANIMATION_MS * 2.0);

        harness.container.scroll_top =
            harness.container.scroll_height - harness.container.client_height - 0.5;
        if controller.handle_key_down(
            PageScrollKey::PageDown,
            harness.now,
            harness.container,
            harness.padding,
        ) {
            started += 1;
        }
        harness.advance_by(&mut controller, PAGE_SCROLL_ANIMATION_MS * 2.0);

        assert_eq!(started, 0);
        assert_eq!(harness.container.scroll_top, 1_199.5);
    }
}
