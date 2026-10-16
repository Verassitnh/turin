//! Port of upstream/apps/web/src/components/ui/skeleton.tsx
//!
//! `bg-muted-foreground/15` bars that breathe with the stepped
//! `animate-skeleton` opacity pulse (2.4s: hold, a four-step dip to 0.55,
//! hold, a four-step return). Shapes: `block` rounded-sm, `card` rounded-lg,
//! `pill` rounded-full.

use std::time::Duration;

use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, Div, ElementId, IntoElement, RenderOnce,
    StyleRefinement, Styled as _, Window, div,
};

use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, RADIUS_SM};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SkeletonShape {
    #[default]
    Block,
    Card,
    Pill,
}

/// Opacity of `@keyframes skeleton` at `progress` (0..1 of the 2.4s cycle).
pub fn skeleton_opacity(progress: f32) -> f32 {
    const LOW: f32 = 0.55;
    let steps = |from: f32, to: f32, within: f32| {
        // `steps(4)`: four discrete levels, jumping at the end of each quarter.
        let level = (within * 4.).floor().min(3.) + 1.;
        from + (to - from) * (level / 4.)
    };
    match progress {
        p if p < 0.42 => 1.,
        p if p < 0.5 => steps(1., LOW, (p - 0.42) / 0.08),
        p if p < 0.92 => LOW,
        p => steps(LOW, 1., (p - 0.92) / 0.08),
    }
}

/// One bar; size it with `w`/`h` on the returned div.
pub fn skeleton(shape: SkeletonShape, cx: &App) -> Div {
    let color = with_alpha(cx.app_theme().muted_foreground, 0.15);
    let bar = div().bg(color);
    match shape {
        SkeletonShape::Block => bar.rounded(RADIUS_SM),
        SkeletonShape::Card => bar.rounded(RADIUS_LG),
        SkeletonShape::Pill => bar.rounded_full(),
    }
}

/// Wraps a group of bars in the single shared pulse.
pub fn skeleton_group(id: impl Into<ElementId>, content: Div) -> AnyElement {
    content
        .with_animation(
            id,
            Animation::new(Duration::from_millis(2400)).repeat(),
            |el, progress| el.opacity(skeleton_opacity(progress)),
        )
        .into_any_element()
}

/// Size it with the styled refinements (`.w(..)`, `.h(..)`), like the
/// `className` layout classes upstream allows.
#[derive(IntoElement)]
pub struct Skeleton {
    id: ElementId,
    shape: SkeletonShape,
    style: StyleRefinement,
}

impl Skeleton {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            shape: SkeletonShape::Block,
            style: StyleRefinement::default(),
        }
    }

    pub fn shape(mut self, shape: SkeletonShape) -> Self {
        self.shape = shape;
        self
    }
}

impl gpui_kit::Styled for Skeleton {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Skeleton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        use gpui_kit::Refineable as _;
        let color = with_alpha(cx.app_theme().muted_foreground, 0.15);
        let mut bar = div().bg(color);
        bar = match self.shape {
            SkeletonShape::Block => bar.rounded(RADIUS_SM),
            SkeletonShape::Card => bar.rounded(RADIUS_LG),
            SkeletonShape::Pill => bar.rounded_full(),
        };
        bar.style().refine(&self.style);
        bar.with_animation(
            self.id,
            Animation::new(Duration::from_millis(2400)).repeat(),
            |bar, progress| bar.opacity(skeleton_opacity(progress)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_holds_dips_in_four_steps_and_returns() {
        assert_eq!(skeleton_opacity(0.0), 1.);
        assert_eq!(skeleton_opacity(0.41), 1.);
        assert!((skeleton_opacity(0.43) - (1. - 0.45 / 4.)).abs() < 1e-5);
        assert!((skeleton_opacity(0.49) - 0.55).abs() < 1e-5);
        assert!((skeleton_opacity(0.7) - 0.55).abs() < 1e-5);
        assert!((skeleton_opacity(0.99) - 1.).abs() < 1e-5);
    }
}
