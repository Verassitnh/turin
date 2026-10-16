//! Port of upstream/apps/web/src/components/AnimatedHeight.tsx and
//! AnimatedWidth.tsx.
//!
//! Height: measure, then set an explicit height. The first measurement does
//! not clip; later changes clip for the 200ms `ease-out` travel (fallback
//! 250ms). `holdHeight` keeps the previous box and stretches the replacement.
//! Width: `Math.ceil` of the content rect, ignored until it is positive.
//! The CSS transition is [`css_ease_out`] (cubic-bezier(0, 0, 0.2, 1)) over
//! 200ms (width: 150ms).

use std::time::Duration;

use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, ElementId, IntoElement, ParentElement,
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
    let t = t.clamp(0., 1.);
    let u = solve_bezier_x(t, 0., 0.2);
    bezier_1d(u, 0., 1.)
}

fn bezier_1d(u: f32, c1: f32, c2: f32) -> f32 {
    let o = 1. - u;
    3. * o * o * u * c1 + 3. * o * u * u * c2 + u * u * u
}

fn bezier_1d_slope(u: f32, c1: f32, c2: f32) -> f32 {
    let o = 1. - u;
    3. * o * o * c1 + 6. * o * u * (c2 - c1) + 3. * u * u * (1. - c2)
}

fn solve_bezier_x(target: f32, x1: f32, x2: f32) -> f32 {
    let mut u = target;
    for _ in 0..8 {
        let x = bezier_1d(u, x1, x2);
        let slope = bezier_1d_slope(u, x1, x2);
        if slope.abs() < 1e-6 {
            break;
        }
        u = (u - (x - target) / slope).clamp(0., 1.);
    }
    u
}

fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

#[derive(IntoElement)]
pub struct AnimatedHeight {
    id: ElementId,
    state: HeightState,
    /// Height before [`next_height`], so a clipping change can travel.
    from: Option<f64>,
    hold_height: bool,
    children: Vec<AnyElement>,
}

impl AnimatedHeight {
    pub fn new(id: impl Into<ElementId>, state: HeightState) -> Self {
        Self {
            id: id.into(),
            state,
            from: None,
            hold_height: false,
            children: Vec::new(),
        }
    }

    pub fn previous_height(mut self, from: f64) -> Self {
        self.from = Some(from);
        self
    }

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
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let children = self.children;
        let hold = self.hold_height;
        let inner = |child: gpui_kit::Div| {
            if hold {
                child.child(div().h_full().children(children))
            } else {
                child.children(children)
            }
        };
        if self.state.is_clipping
            && let (Some(from), Some(to)) = (self.from, self.state.height)
        {
            return inner(div())
                .with_animation(
                    self.id,
                    Animation::new(Duration::from_millis(HEIGHT_TRANSITION_MS))
                        .with_easing(css_ease_out),
                    move |el, delta| {
                        el.h(px(lerp(from as f32, to as f32, delta)))
                            .overflow_hidden()
                    },
                )
                .into_any_element();
        }
        let mut root = div();
        if let Some(height) = self.state.height {
            root = root.h(px(height as f32));
            if self.state.is_clipping {
                root = root.overflow_hidden();
            }
        }
        inner(root).into_any_element()
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
}
