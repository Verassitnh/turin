//! Port of upstream/apps/web/src/components/AnimatedHeight.tsx and
//! AnimatedWidth.tsx.
//!
//! Height: measure, then set an explicit height. The first measurement does
//! not clip; later changes clip for the 200ms `ease-out` travel (fallback
//! 250ms). `holdHeight` keeps the previous box and stretches the replacement.
//! Width: `Math.ceil` of the content rect, ignored until it is positive.
//! The CSS transition is [`css_ease_out`] (cubic-bezier(0, 0, 0.2, 1)) over
//! 200ms (width: 150ms).
//!
//! Port of upstream/apps/web/src/components/AnimatedWidth.tsx.

use std::time::{Duration, Instant};

use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, ElementId, Entity, IntoElement, ParentElement,
    RenderOnce, Styled, Window, div, px,
};

pub const HEIGHT_TRANSITION_MS: u64 = 200;
pub const HEIGHT_TRANSITION_FALLBACK_MS: u64 = 250;
pub const WIDTH_TRANSITION_MS: u64 = 150;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HeightState {
    pub height: Option<f64>,
    pub is_clipping: bool,
}

/// The `setHeightState` updater. Unchanged measurements return the same state.
pub fn next_height(current: HeightState, next: f64) -> HeightState {
    if current.height == Some(next) {
        return current;
    }
    HeightState {
        height: Some(next),
        is_clipping: current.height.is_some(),
    }
}

pub fn clear_clipping(current: HeightState) -> HeightState {
    if current.is_clipping {
        HeightState {
            is_clipping: false,
            ..current
        }
    } else {
        current
    }
}

/// `Math.ceil(contentRect.width)`, kept only when positive.
pub fn measured_width(content_rect_width: f64) -> Option<f64> {
    let next = content_rect_width.ceil();
    if next > 0. { Some(next) } else { None }
}

/// CSS `ease-out`: cubic-bezier(0, 0, 0.2, 1).
pub fn css_ease_out(t: f32) -> f32 {
    cubic_bezier(0., 0., 0.2, 1., t)
}

/// CSS `cubic-bezier(x1, y1, x2, y2)` timing function.
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    let u = solve_bezier_x(t, x1, x2);
    bezier_1d(u, y1, y2)
}

fn bezier_1d(u: f32, c1: f32, c2: f32) -> f32 {
    let o = 1. - u;
    3. * o * o * u * c1 + 3. * o * u * u * c2 + u * u * u
}

fn bezier_1d_slope(u: f32, c1: f32, c2: f32) -> f32 {
    let o = 1. - u;
    3. * o * o * c1 + 6. * o * u * (c2 - c1) + 3. * u * u * (1. - c2)
}

/// Newton steps, then bisection when the slope is too flat to trust.
fn solve_bezier_x(target: f32, x1: f32, x2: f32) -> f32 {
    let mut u = target;
    for _ in 0..8 {
        let x = bezier_1d(u, x1, x2);
        let slope = bezier_1d_slope(u, x1, x2);
        if (x - target).abs() < 1e-6 {
            return u;
        }
        if slope.abs() < 1e-6 {
            break;
        }
        u = (u - (x - target) / slope).clamp(0., 1.);
    }
    let (mut low, mut high) = (0f32, 1f32);
    u = target;
    for _ in 0..24 {
        let x = bezier_1d(u, x1, x2);
        if (x - target).abs() < 1e-6 {
            break;
        }
        if x < target {
            low = u;
        } else {
            high = u;
        }
        u = (low + high) / 2.;
    }
    u
}

pub fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

/// The height a running travel `from -> to` has reached after `elapsed`.
/// CSS `transition-[height]` retargets from this computed value, not from the
/// previous end value.
pub fn in_flight_height(from: f64, to: f64, elapsed: Duration) -> f64 {
    let progress = (elapsed.as_secs_f32() / (HEIGHT_TRANSITION_MS as f32 / 1000.)).min(1.);
    f64::from(lerp(from as f32, to as f32, css_ease_out(progress)))
}

/// Measured state behind one [`AnimatedHeight`]: upstream's `heightState`
/// plus the height the current travel starts from.
#[derive(Default)]
struct AnimatedHeightState {
    height: HeightState,
    from: Option<f64>,
    /// When the current travel began (for retargeting mid-flight).
    started: Option<Instant>,
    epoch: u64,
}

/// Upstream's `HEIGHT_TRANSITION_FALLBACK_MS` timer: clip no longer than this.
fn schedule_clip_release(state: &Entity<AnimatedHeightState>, epoch: u64, cx: &mut App) {
    let state = state.downgrade();
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(HEIGHT_TRANSITION_FALLBACK_MS))
            .await;
        state
            .update(cx, |state, cx| {
                if state.epoch == epoch && state.height.is_clipping {
                    state.height = clear_clipping(state.height);
                    cx.notify();
                }
            })
            .ok();
    })
    .detach();
}

/// Applies one `updateHeight` measurement (`Math.ceil` of the content).
fn apply_measurement(state: &Entity<AnimatedHeightState>, content_height: f32, cx: &mut App) {
    let next = f64::from(content_height).ceil();
    let current = state.read(cx).height;
    let updated = next_height(current, next);
    if updated == current {
        return;
    }
    let epoch = state.update(cx, |state, cx| {
        let now = Instant::now();
        state.from = match (state.from, current.height, state.started) {
            (Some(from), Some(to), Some(started)) if current.is_clipping => {
                Some(in_flight_height(from, to, now.duration_since(started)))
            }
            _ => current.height,
        };
        state.started = Some(now);
        state.height = updated;
        state.epoch += 1;
        cx.notify();
        state.epoch
    });
    if updated.is_clipping {
        schedule_clip_release(state, epoch, cx);
    }
}

#[derive(IntoElement)]
pub struct AnimatedHeight {
    id: ElementId,
    hold_height: bool,
    children: Vec<AnyElement>,
}

impl AnimatedHeight {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            hold_height: false,
            children: Vec::new(),
        }
    }

    /// Retain the previous content height while a replacement is loading.
    pub fn hold_height(mut self, hold: bool) -> Self {
        self.hold_height = hold;
        self
    }
}

impl ParentElement for AnimatedHeight {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for AnimatedHeight {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state =
            window.use_keyed_state(self.id.clone(), cx, |_, _| AnimatedHeightState::default());
        let (height, from, epoch) = {
            let state = state.read(cx);
            (state.height, state.from, state.epoch)
        };
        let hold = self.hold_height;
        let content = if hold {
            div().h_full().children(self.children)
        } else {
            div().children(self.children)
        };
        // The wrapper keeps the measured content as its only child; its natural
        // height (what `scrollHeight` reports) is the first child's bounds.
        let mut root = div().on_children_prepainted(move |bounds, _, cx| {
            if hold {
                return;
            }
            if let Some(content) = bounds.first() {
                apply_measurement(&state, f32::from(content.size.height), cx);
            }
        });
        let Some(target) = height.height else {
            return root.child(content).into_any_element();
        };
        if height.is_clipping
            && let Some(from) = from
        {
            root = root.child(content);
            return root
                .with_animation(
                    ElementId::NamedInteger(format!("{:?}-height", self.id).into(), epoch),
                    Animation::new(Duration::from_millis(HEIGHT_TRANSITION_MS))
                        .with_easing(css_ease_out),
                    move |el, delta| {
                        el.h(px(lerp(from as f32, target as f32, delta)))
                            .overflow_hidden()
                    },
                )
                .into_any_element();
        }
        root.h(px(target as f32)).child(content).into_any_element()
    }
}

#[derive(IntoElement)]
pub struct AnimatedWidth {
    id: ElementId,
    width: Option<f64>,
    from: Option<f64>,
    children: Vec<AnyElement>,
}

impl AnimatedWidth {
    pub fn new(id: impl Into<ElementId>, width: Option<f64>) -> Self {
        Self {
            id: id.into(),
            width,
            from: None,
            children: Vec::new(),
        }
    }

    pub fn previous_width(mut self, from: f64) -> Self {
        self.from = Some(from);
        self
    }
}

impl ParentElement for AnimatedWidth {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for AnimatedWidth {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let mut root = div().overflow_hidden();
        if let Some(width) = self.width {
            if let Some(from) = self.from {
                let id = self.id;
                return root
                    .child(div().whitespace_nowrap().children(self.children))
                    .with_animation(
                        id,
                        Animation::new(Duration::from_millis(WIDTH_TRANSITION_MS))
                            .with_easing(css_ease_out),
                        move |el, delta| el.w(px(lerp(from as f32, width as f32, delta))),
                    )
                    .into_any_element();
            }
            root = root.w(px(width as f32));
        }
        root.child(div().whitespace_nowrap().children(self.children))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_measurement_does_not_clip() {
        let next = next_height(HeightState::default(), 40.);
        assert_eq!(next.height, Some(40.));
        assert!(!next.is_clipping);
    }

    #[test]
    fn a_later_change_clips_until_the_transition_ends() {
        let current = HeightState {
            height: Some(40.),
            is_clipping: false,
        };
        let next = next_height(current, 64.);
        assert_eq!(next.height, Some(64.));
        assert!(next.is_clipping);
        assert_eq!(next_height(next, 64.), next);
        let settled = clear_clipping(next);
        assert!(!settled.is_clipping);
        assert_eq!(settled.height, Some(64.));
        assert_eq!(clear_clipping(settled), settled);
    }

    #[test]
    fn retargeting_starts_from_the_in_flight_height() {
        let half = Duration::from_millis(HEIGHT_TRANSITION_MS / 2);
        let mid = in_flight_height(40., 100., half);
        assert!(mid > 40. && mid < 100., "mid-flight height {mid}");
        // ease-out is past the linear midpoint at half time.
        assert!(mid > 70., "ease-out midpoint {mid}");
        assert_eq!(in_flight_height(40., 100., Duration::ZERO), 40.);
        assert_eq!(
            in_flight_height(40., 100., Duration::from_millis(HEIGHT_TRANSITION_MS * 2)),
            100.
        );
    }

    #[test]
    fn width_ceils_and_ignores_non_positive() {
        assert_eq!(measured_width(10.2), Some(11.));
        assert_eq!(measured_width(0.), None);
        assert_eq!(measured_width(-0.2), None);
    }

    #[test]
    fn ease_out_starts_fast() {
        assert_eq!(css_ease_out(0.), 0.);
        assert_eq!(css_ease_out(1.), 1.);
        let mid = css_ease_out(0.5);
        assert!(mid > 0.75 && mid < 0.9, "ease-out midpoint {mid}");
    }

    #[test]
    fn toast_curve_overshoots_then_settles() {
        assert_eq!(cubic_bezier(0.22, 1., 0.36, 1., 0.), 0.);
        assert_eq!(cubic_bezier(0.22, 1., 0.36, 1., 1.), 1.);
        let early = cubic_bezier(0.22, 1., 0.36, 1., 0.2);
        assert!(early > 0.5, "fast start: {early}");
    }
}
