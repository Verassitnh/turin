//! Port of upstream/apps/web/src/components/ui/skeleton.tsx and the `skeleton` keyframes in
//! index.css: `bg-muted-foreground/15` bars and one stepped opacity breath per container
//! (2.4s: full opacity to 42%, 0.55 from 50% to 92%, each ramp in four steps).

use std::time::Duration;

use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, Div, ElementId, Hsla, IntoElement, ParentElement,
    Pixels, Styled, div, px,
};

use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SkeletonShape {
    /// `rounded-sm`
    #[default]
    Block,
    /// `rounded-lg`
    Card,
    /// `rounded-full`
    Pill,
}

/// One bar; size it with `w`/`h` on the returned div.
pub fn skeleton(shape: SkeletonShape, cx: &App) -> Div {
    let color: Hsla = with_alpha(cx.app_theme().muted_foreground, 0.15).into();
    let bar = div().bg(color);
    match shape {
        SkeletonShape::Block => bar.rounded(px(6.)),
        SkeletonShape::Card => bar.rounded(px(10.)),
        SkeletonShape::Pill => bar.rounded_full(),
    }
}

/// `animate-skeleton` opacity at `delta` (0..1) through the cycle, quantized like `steps(4)`.
pub fn skeleton_opacity(delta: f32) -> f32 {
    let step = |from: f32, to: f32, start: f32, end: f32| {
        let t = ((delta - start) / (end - start)).clamp(0., 1.);
        let stepped = (t * 4.).floor() / 4.;
        from + (to - from) * stepped
    };
    if delta < 0.42 {
        1.
    } else if delta < 0.5 {
        step(1., 0.55, 0.42, 0.5)
    } else if delta < 0.92 {
        0.55
    } else {
        step(0.55, 1., 0.92, 1.)
    }
}

/// Wraps a group of bars in the single shared pulse.
pub fn skeleton_group(id: impl Into<ElementId>, content: Div) -> AnyElement {
    content
        .with_animation(
            id,
            Animation::new(Duration::from_millis(2400)).repeat(),
            |el, delta| el.opacity(skeleton_opacity(delta)),
        )
        .into_any_element()
}

#[allow(dead_code)]
fn _px(_: Pixels) {}

#[cfg(test)]
mod tests {
    use super::skeleton_opacity;

    #[test]
    fn breathes_like_the_keyframes() {
        assert_eq!(skeleton_opacity(0.0), 1.);
        assert_eq!(skeleton_opacity(0.41), 1.);
        assert_eq!(skeleton_opacity(0.6), 0.55);
        assert_eq!(skeleton_opacity(0.9), 0.55);
        assert_eq!(skeleton_opacity(0.999), 0.55 + 0.45 * 0.75);
    }
}
